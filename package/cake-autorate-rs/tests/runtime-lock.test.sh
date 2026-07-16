#!/bin/sh
set -eu

test_dir="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
lib="$test_dir/../files/usr/libexec/cake-autorate-rs/runtime-lock"
work="${TMPDIR:-/tmp}/cake-runtime-lock-test.$$"
holder_pid=""

fail() {
	printf '%s\n' "$*" >&2
	exit 1
}

short_sleep() {
	sleep 0.02 2>/dev/null || sleep 1
}

wait_file() {
	file="$1"
	pid="$2"
	attempt=0
	while [ ! -e "$file" ]; do
		kill -0 "$pid" 2>/dev/null || fail "holder exited before publishing $file"
		attempt=$((attempt + 1))
		[ "$attempt" -lt 100 ] || fail "timed out waiting for $file"
		short_sleep
	done
}

cleanup() {
	status="$?"
	if [ -n "$holder_pid" ] && kill -0 "$holder_pid" 2>/dev/null; then
		kill -KILL "$holder_pid" 2>/dev/null || true
		wait "$holder_pid" 2>/dev/null || true
	fi
	rm -rf "$work"
	trap - EXIT INT TERM
	exit "$status"
}
trap cleanup EXIT INT TERM

mkdir -p "$work/locks"
export CAKE_AUTORATE_RUNTIME_LOCK_ROOT="$work/locks"
export CAKE_AUTORATE_RUNTIME_LOCK_LIB="$lib"

# shellcheck source=../files/usr/libexec/cake-autorate-rs/runtime-lock
. "$lib"

# A descriptor for the right pathname is not proof of a lock.  Both advertised
# modes must reject an unlocked, independently opened runtime.guard rather than
# silently acquiring authority from spoofable environment metadata.
for spoof_mode in shared exclusive; do
	set +e
	SPOOF_MODE="$spoof_mode" sh -c '
		set -eu
		. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
		runtime_lock_prepare_root
		exec 8>>"$CAKE_AUTORATE_RUNTIME_LOCK_ROOT/runtime.guard"
		export CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_FD=8
		export CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_MODE="$SPOOF_MODE"
		if runtime_lock_ensure_global_shared; then exit 90; fi
		[ -z "${runtime_global_lock_mode:-}" ]
		exit 1
	'
	rc="$?"
	set -e
	[ "$rc" -eq 1 ] || fail "unlocked correct-path $spoof_mode global descriptor returned rc $rc"
done

set +e
sh -c '
	set -eu
	. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
	runtime_lock_prepare_root
	runtime_lock_interface_paths spoof0
	exec 9>>"$runtime_interface_lock_guard"
	export CAKE_AUTORATE_INTERFACE_LOCK_FD=9
	runtime_lock_borrow_interface spoof0 "$$" 1 spooftoken ""
'
rc="$?"
set -e
[ "$rc" -eq 1 ] || fail "unlocked correct-path interface descriptor returned rc $rc"

# The runtime root and guard leaves are never followed through symlinks.  On a
# real router the same ownership check below resolves to uid 0; tests use their
# current effective uid without a production bypass.
mkdir -p "$work/real-root" "$work/symlink-guard-root" "$work/symlink-interface-root"
ln -s "$work/real-root" "$work/root-link"
if CAKE_AUTORATE_RUNTIME_LOCK_ROOT="$work/root-link" sh -c '
	. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
	runtime_lock_prepare_root
'; then
	fail "symlink runtime root was accepted"
fi
: > "$work/global-guard-target"
ln -s "$work/global-guard-target" "$work/symlink-guard-root/runtime.guard"
if CAKE_AUTORATE_RUNTIME_LOCK_ROOT="$work/symlink-guard-root" sh -c '
	. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
	runtime_lock_acquire_global_shared
'; then
	fail "symlink global guard was accepted"
fi
: > "$work/interface-guard-target"
ln -s "$work/interface-guard-target" \
	"$work/symlink-interface-root/interface-spoof1.lock.guard"
if CAKE_AUTORATE_RUNTIME_LOCK_ROOT="$work/symlink-interface-root" sh -c '
	. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
	runtime_lock_acquire_global_shared
	runtime_lock_acquire_interface spoof1 fixture "" spooftoken
'; then
	fail "symlink interface guard was accepted"
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

# dash keeps $$ at the outer shell PID in a (...) subshell.  Opening FD 8/9
# there must inspect /proc/self, and a newly published owner record must name
# the real subshell rather than the unrelated outer process.
if command -v dash >/dev/null 2>&1; then
	dash -c '
		set -eu
		. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
		(
			runtime_lock_acquire_global_exclusive
			runtime_lock_acquire_interface dash0 dash-subshell "" dashtoken
			runtime_lock_set_current_pid
			[ "$CAKE_AUTORATE_INTERFACE_LOCK_OWNER" = "$runtime_lock_current_pid_value" ]
			[ "$CAKE_AUTORATE_INTERFACE_LOCK_OWNER" != "$$" ]
			runtime_lock_release_interface
			runtime_lock_release_global
		)
	'
	[ ! -e "$work/locks/interface-dash0.lock" ] || fail "dash subshell record stranded"
fi

# Basic ownership is atomically published and removed with both kernel locks.
runtime_lock_acquire_global_shared
runtime_lock_acquire_interface eth0 fixture "" basictoken
[ -f "$work/locks/interface-eth0.lock" ] || fail "basic ownership record missing"
grep -qx 'role=fixture' "$work/locks/interface-eth0.lock"
runtime_lock_release_interface
runtime_lock_release_global
[ ! -e "$work/locks/interface-eth0.lock" ] || fail "basic ownership record stranded"

# A forged mode cannot convert a genuine inherited lock.  In particular,
# claiming exclusive over a shared OFD must fail without upgrading it, and
# claiming shared over an exclusive OFD must fail before any LOCK_SH downgrade.
runtime_lock_acquire_global_shared
set +e
CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_MODE=exclusive sh -c '
	. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
	runtime_lock_ensure_global_shared
'
rc="$?"
set -e
[ "$rc" -eq 1 ] || fail "shared descriptor accepted forged exclusive mode (rc $rc)"
"$runtime_lock_flock_bin" -sn "$work/locks/runtime.guard" true ||
	fail "forged exclusive mode changed the parent shared lock"
if "$runtime_lock_flock_bin" -xn "$work/locks/runtime.guard" true; then
	fail "parent shared lock disappeared after forged exclusive mode"
fi
runtime_lock_release_global

runtime_lock_acquire_global_exclusive
set +e
CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_MODE=shared sh -c '
	. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
	runtime_lock_ensure_global_shared
'
rc="$?"
set -e
[ "$rc" -eq 1 ] || fail "exclusive descriptor accepted forged shared mode (rc $rc)"
if "$runtime_lock_flock_bin" -sn "$work/locks/runtime.guard" true; then
	fail "forged shared mode downgraded the parent exclusive lock"
fi
runtime_lock_release_global

# A child borrowing an inherited exclusive descriptor through the shared
# compatibility entry point must not downgrade the owner's open-file lock.
runtime_lock_acquire_global_exclusive
sh -c '
	set -eu
	. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
	runtime_lock_ensure_global_shared
	[ "$runtime_global_lock_mode" = exclusive ]
'
set +e
env -u CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_FD \
	-u CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_MODE \
	sh -c '. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"; runtime_lock_acquire_global_shared'
rc="$?"
set -e
[ "$rc" -eq 75 ] || fail "borrowed child downgraded exclusive global lock (rc $rc)"
runtime_lock_release_global

# Sanitised-name collisions share the same kernel guard and are conservatively
# retryable (rc 75); the contender must not replace the live record.
CAKE_READY="$work/collision.ready" CAKE_RELEASE="$work/collision.release" \
sh -c '
	set -eu
	. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
	runtime_lock_acquire_global_shared
	runtime_lock_acquire_interface wan.1 collision "" collisiontoken
	trap '\''runtime_lock_release_interface >/dev/null 2>&1 || true; runtime_lock_release_global >/dev/null 2>&1 || true'\'' EXIT
	: > "$CAKE_READY"
	while [ ! -e "$CAKE_RELEASE" ]; do sleep 1; done
' &
holder_pid="$!"
wait_file "$work/collision.ready" "$holder_pid"
set +e
sh -c '
	. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
	runtime_lock_acquire_global_shared || exit $?
	runtime_lock_acquire_interface wan_1 contender "" contendertoken
	rc=$?
	runtime_lock_release_global >/dev/null 2>&1 || true
	exit "$rc"
'
rc="$?"
set -e
[ "$rc" -eq 75 ] || fail "sanitised interface collision returned rc $rc"
grep -qx 'token=collisiontoken' "$work/locks/interface-wan_1.lock"
: > "$work/collision.release"
wait "$holder_pid"
holder_pid=""
[ ! -e "$work/locks/interface-wan_1.lock" ] || fail "collision record stranded"

# Borrowers authenticate PID/start/token/journal and the actual inherited FD
# paths. A bad token or descriptor must not disturb the parent's ownership.
journal="$work/borrow.journal"
: > "$journal"
runtime_lock_acquire_global_shared
runtime_lock_acquire_interface eth1 autotune "$journal" borrowtoken
owner="$CAKE_AUTORATE_INTERFACE_LOCK_OWNER"
start="$CAKE_AUTORATE_INTERFACE_LOCK_STARTTIME"
OWNER="$owner" START="$start" JOURNAL="$journal" sh -c '
	set -eu
	. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
	runtime_lock_ensure_global_shared
	if runtime_lock_borrow_interface eth1 "$OWNER" "$START" wrongtoken "$JOURNAL"; then
		exit 1
	fi
'
OWNER="$owner" START="$start" JOURNAL="$journal" sh -c '
	set -eu
	. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
	runtime_lock_ensure_global_shared
	runtime_lock_borrow_interface eth1 "$OWNER" "$START" borrowtoken "$JOURNAL"
'
OWNER="$owner" START="$start" JOURNAL="$journal" sh -c '
	set -eu
	. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
	exec 8>>"$CAKE_AUTORATE_RUNTIME_LOCK_ROOT/not-the-global-guard"
	if runtime_lock_ensure_global_shared; then exit 1; fi
'
OWNER="$owner" START="$start" JOURNAL="$journal" sh -c '
	set -eu
	. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
	exec 9>>"$CAKE_AUTORATE_RUNTIME_LOCK_ROOT/not-the-interface-guard"
	if runtime_lock_borrow_interface eth1 "$OWNER" "$START" borrowtoken "$JOURNAL"; then exit 1; fi
'
grep -qx "pid=$owner" "$work/locks/interface-eth1.lock"
grep -qx 'token=borrowtoken' "$work/locks/interface-eth1.lock"
runtime_lock_release_interface
runtime_lock_release_global
[ ! -e "$work/locks/interface-eth1.lock" ] || fail "borrowed ownership record stranded"

# A killed owner without a recovery journal leaves only advisory metadata. A
# new kernel-lock owner safely replaces it and later removes its own record.
CAKE_READY="$work/stale.ready" sh -c '
	set -eu
	. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
	runtime_lock_acquire_global_shared
	runtime_lock_acquire_interface eth2 killed "" killedtoken
	: > "$CAKE_READY"
	while :; do sleep 1; done
' &
holder_pid="$!"
wait_file "$work/stale.ready" "$holder_pid"
kill -KILL "$holder_pid"
wait "$holder_pid" 2>/dev/null || true
holder_pid=""
[ -f "$work/locks/interface-eth2.lock" ] || fail "killed-owner fixture record missing"
runtime_lock_acquire_global_shared
attempt=0
while :; do
	set +e
	runtime_lock_acquire_interface eth2 replacement "" replacementtoken
	rc="$?"
	set -e
	[ "$rc" -ne 0 ] || break
	[ "$rc" -eq 75 ] || fail "stale owner replacement returned rc $rc"
	attempt=$((attempt + 1))
	[ "$attempt" -lt 100 ] || fail "stale owner descendants kept the lock indefinitely"
	short_sleep
done
grep -qx 'token=replacementtoken' "$work/locks/interface-eth2.lock"
runtime_lock_release_interface
runtime_lock_release_global
[ ! -e "$work/locks/interface-eth2.lock" ] || fail "replacement record stranded"

# A killed owner with a live journal cannot be stolen by a normal operation.
# A recovery claimant with the exact immutable identity can take over and clean
# the record without signalling or trusting a recycled PID.
recovery_journal="$work/recovery.journal"
: > "$recovery_journal"
CAKE_READY="$work/recovery.ready" CAKE_ID="$work/recovery.id" \
CAKE_JOURNAL="$recovery_journal" sh -c '
	set -eu
	. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
	runtime_lock_acquire_global_shared
	runtime_lock_acquire_interface eth3 autotune "$CAKE_JOURNAL" recoverytoken
	printf "%s %s\n" "$CAKE_AUTORATE_INTERFACE_LOCK_OWNER" "$CAKE_AUTORATE_INTERFACE_LOCK_STARTTIME" > "$CAKE_ID"
	: > "$CAKE_READY"
	while :; do sleep 1; done
' &
holder_pid="$!"
wait_file "$work/recovery.ready" "$holder_pid"
set -- $(cat "$work/recovery.id")
dead_pid="$1"
dead_start="$2"
kill -KILL "$holder_pid"
wait "$holder_pid" 2>/dev/null || true
holder_pid=""
runtime_lock_acquire_global_shared
attempt=0
while :; do
	set +e
	runtime_lock_acquire_interface eth3 normal "" normaltoken
	rc="$?"
	set -e
	[ "$rc" -ne 75 ] && break
	attempt=$((attempt + 1))
	[ "$attempt" -lt 100 ] || fail "killed owner descendants kept journal lock indefinitely"
	short_sleep
done
[ "$rc" -eq 76 ] || fail "live recovery journal returned rc $rc instead of 76"
attempt=0
while :; do
	set +e
	runtime_lock_claim_interface_recovery eth3 "$dead_pid" "$dead_start" recoverytoken "$recovery_journal"
	rc="$?"
	set -e
	[ "$rc" -ne 0 ] || break
	[ "$rc" -eq 75 ] || fail "recovery claim returned rc $rc"
	attempt=$((attempt + 1))
	[ "$attempt" -lt 100 ] || fail "killed owner descendants kept recovery locked indefinitely"
	short_sleep
done
grep -qx 'role=recovery' "$work/locks/interface-eth3.lock"
runtime_lock_release_interface
runtime_lock_release_global
[ ! -e "$work/locks/interface-eth3.lock" ] || fail "recovery record stranded"

# allow_missing is narrowly for a genuinely absent record after both kernel
# locks have been acquired.  A malformed file, a foreign valid owner, or even a
# dangling symlink is metadata corruption and must never be overwritten.
allow_journal="$work/allow-missing.journal"
: > "$allow_journal"
allow_start="$(runtime_lock_process_starttime "$$")"
runtime_lock_acquire_global_shared
runtime_lock_interface_paths eth_allow

printf 'not-a-record\n' > "$runtime_interface_lock_record"
set +e
runtime_lock_claim_interface_recovery eth_allow "$$" "$allow_start" allowtoken \
	"$allow_journal" 1
rc="$?"
set -e
[ "$rc" -eq 1 ] || fail "allow_missing accepted malformed metadata (rc $rc)"
grep -qx 'not-a-record' "$runtime_interface_lock_record" ||
	fail "malformed recovery metadata was overwritten"

rm -f "$runtime_interface_lock_record"
: > "$work/allow-symlink-target"
ln -s "$work/allow-symlink-target" "$runtime_interface_lock_record"
set +e
runtime_lock_claim_interface_recovery eth_allow "$$" "$allow_start" allowtoken \
	"$allow_journal" 1
rc="$?"
set -e
[ "$rc" -eq 1 ] || fail "allow_missing accepted symlink metadata (rc $rc)"
[ -L "$runtime_interface_lock_record" ] || fail "recovery metadata symlink was replaced"

rm -f "$runtime_interface_lock_record"
runtime_lock_write_record "$runtime_interface_lock_record" "$$" "$allow_start" \
	recovery foreigntoken "$allow_journal"
set +e
runtime_lock_claim_interface_recovery eth_allow "$$" "$allow_start" allowtoken \
	"$allow_journal" 1
rc="$?"
set -e
[ "$rc" -eq 1 ] || fail "allow_missing accepted foreign valid metadata (rc $rc)"
grep -qx 'token=foreigntoken' "$runtime_interface_lock_record" ||
	fail "foreign recovery metadata was overwritten"

rm -f "$runtime_interface_lock_record"
runtime_lock_claim_interface_recovery eth_allow "$$" "$allow_start" allowtoken \
	"$allow_journal" 1
grep -qx 'role=recovery' "$runtime_interface_lock_record"
grep -qx 'token=allowtoken' "$runtime_interface_lock_record"
runtime_lock_release_interface
runtime_lock_release_global
[ ! -e "$work/locks/interface-eth_allow.lock" ] || fail "allow_missing recovery record stranded"

# Malformed advisory metadata fails closed but must release the kernel FD. Once
# an administrator/recovery coordinator removes that corrupt record, acquisition
# succeeds immediately rather than remaining wedged by a leaked descriptor.
printf 'not-a-record\n' > "$work/locks/interface-eth4.lock"
runtime_lock_acquire_global_shared
set +e
runtime_lock_acquire_interface eth4 fixture "" invalidrecordtoken
rc="$?"
set -e
[ "$rc" -eq 1 ] || fail "malformed record returned rc $rc"
runtime_lock_release_global
rm -f "$work/locks/interface-eth4.lock"
runtime_lock_acquire_global_shared
runtime_lock_acquire_interface eth4 fixture "" repairedtoken
runtime_lock_release_interface
runtime_lock_release_global
[ ! -e "$work/locks/interface-eth4.lock" ] || fail "repaired record stranded"

if find "$work/locks" -name '*.tmp.*' -print | grep -q .; then
	fail "temporary ownership records were stranded"
fi

echo "runtime-lock tests passed"
