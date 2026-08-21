#!/bin/sh
set -eu

test_dir="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
lib="$test_dir/../files/usr/libexec/cake-autorate-rs/runtime-lock"
work="$(mktemp -d "${TMPDIR:-/tmp}/cake-runtime-lock-test.XXXXXX")"

fail() {
	printf '%s\n' "$*" >&2
	exit 1
}

cleanup() {
	status="$?"
	rm -rf "$work"
	trap - EXIT INT TERM
	exit "$status"
}
trap cleanup EXIT INT TERM

export CAKE_AUTORATE_RUNTIME_LOCK_ROOT="$work/locks"
export CAKE_AUTORATE_RUNTIME_LOCK_LIB="$lib"

# shellcheck source=../files/usr/libexec/cake-autorate-rs/runtime-lock
. "$lib"

# The root and guard leaf must be private regular paths, never symlinks.
mkdir -p "$work/real-root" "$work/symlink-guard-root"
ln -s "$work/real-root" "$work/root-link"
if CAKE_AUTORATE_RUNTIME_LOCK_ROOT="$work/root-link" sh -c '
	. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
	runtime_lock_prepare_root
'; then
	fail "symlink runtime root was accepted"
fi
: > "$work/guard-target"
ln -s "$work/guard-target" "$work/symlink-guard-root/runtime.guard"
if CAKE_AUTORATE_RUNTIME_LOCK_ROOT="$work/symlink-guard-root" sh -c '
	. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
	runtime_lock_acquire_global_exclusive
'; then
	fail "symlink global guard was accepted"
fi

if [ "$(id -u)" -eq 0 ]; then
	mkdir -p "$work/foreign-root"
	chown 1 "$work/foreign-root"
	if CAKE_AUTORATE_RUNTIME_LOCK_ROOT="$work/foreign-root" sh -c '
		. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
		runtime_lock_prepare_root
	'; then
		fail "foreign-owned runtime root was accepted as root"
	fi
	chown 0 "$work/foreign-root"
fi

# An unlocked descriptor for the right path is not inherited authority.
set +e
sh -c '
	set -eu
	. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
	runtime_lock_prepare_root
	exec 8>>"$CAKE_AUTORATE_RUNTIME_LOCK_ROOT/runtime.guard"
	export CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_FD=8
	export CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_MODE=exclusive
	runtime_lock_borrow_global_exclusive
'
rc="$?"
set -e
[ "$rc" -eq 1 ] || fail "unlocked correct-path descriptor returned rc $rc"

runtime_lock_acquire_global_exclusive
[ "$runtime_global_lock_mode" = exclusive ]
[ "${CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_FD:-}" = 8 ]
[ "${CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_MODE:-}" = exclusive ]

# The borrowed child must prove the inherited exclusive OFD and must not
# downgrade or unlock its parent's transaction when it releases local state.
sh -c '
	set -eu
	. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
	runtime_lock_borrow_global_exclusive
	[ "$runtime_global_lock_mode" = exclusive ]
	[ "$runtime_global_lock_inherited" = 1 ]
	runtime_lock_release_global
'
if "$runtime_lock_flock_bin" -sn "$work/locks/runtime.guard" true; then
	fail "borrowed child downgraded or released the parent exclusive lock"
fi

# Wrong metadata cannot reinterpret the inherited descriptor.
set +e
CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_MODE=shared sh -c '
	. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
	runtime_lock_borrow_global_exclusive
'
rc="$?"
set -e
[ "$rc" -eq 1 ] || fail "forged inherited mode returned rc $rc"
if "$runtime_lock_flock_bin" -sn "$work/locks/runtime.guard" true; then
	fail "forged mode changed the parent exclusive lock"
fi

runtime_lock_release_global
"$runtime_lock_flock_bin" -xn "$work/locks/runtime.guard" true ||
	fail "released global lock remained held"

printf '%s\n' "runtime-lock minimal global bridge tests passed"
