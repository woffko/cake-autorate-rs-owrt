#!/bin/sh
set -eu

test_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
source_seed="$test_dir/../files/etc/uci-defaults/99-cake-autorate-rs-scheduler-engine"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM
mkdir -p "$tmp/bin" "$tmp/state"

cat > "$tmp/bin/uci" <<'EOF'
#!/bin/sh
set -eu
[ "${1:-}" != -q ] || shift
command="${1:-}"
[ "$#" -eq 0 ] || shift
case "$command" in
	get)
		case "${1:-}" in
			cake-autorate.globals)
				[ -e "$TEST_STATE/globals" ] || exit 1
				printf '%s\n' globals
				;;
			cake-autorate.globals.autotune_scheduler_engine)
				[ -e "$TEST_STATE/engine-present" ] || exit 1
				cat "$TEST_STATE/engine"
				;;
			*) exit 1 ;;
		esac
		;;
	show)
		[ "${1:-}" = cake-autorate ] || exit 1
		[ ! -e "$TEST_STATE/instance" ] ||
			printf '%s\n' 'cake-autorate.prior=cake_autorate'
		;;
	set)
		case "${1:-}" in
			cake-autorate.globals=globals)
				: > "$TEST_STATE/globals"
				;;
			cake-autorate.globals.autotune_scheduler_engine=*)
				value="${1#*=}"
				printf '%s\n' "$value" > "$TEST_STATE/engine"
				: > "$TEST_STATE/engine-present"
				;;
			*) exit 1 ;;
		esac
		;;
	commit)
		[ "${1:-}" = cake-autorate ] || exit 1
		[ "${TEST_FAIL_COMMIT:-0}" != 1 ] || exit 1
		printf '%s\n' commit >> "$TEST_STATE/actions"
		;;
	revert)
		case "${1:-}" in
			cake-autorate.globals.autotune_scheduler_engine)
				rm -f "$TEST_STATE/engine" "$TEST_STATE/engine-present"
				;;
			cake-autorate.globals)
				rm -f "$TEST_STATE/globals"
				;;
			*) exit 1 ;;
		esac
		printf '%s\n' "revert:${1:-}" >> "$TEST_STATE/actions"
		;;
	*) exit 1 ;;
esac
EOF

cat > "$tmp/bin/daemon" <<'EOF'
#!/bin/sh
printf '%s\n' probe >> "$TEST_STATE/actions"
[ "$#" -eq 1 ] && [ "$1" = --calibration-capabilities ] || exit 2
[ "${TEST_DAEMON_OK:-1}" = 1 ] || exit 1
printf '%s\n' "${TEST_CAPABILITIES:-}"
EOF

cat > "$tmp/bin/logger" <<'EOF'
#!/bin/sh
printf '%s\n' "logger:$*" >> "$TEST_STATE/actions"
EOF

chmod +x "$tmp/bin/uci" "$tmp/bin/daemon" "$tmp/bin/logger"
sed \
	-e "s#/usr/sbin/cake-autorated#$tmp/bin/daemon#" \
	-e "s#/etc/cake-autorate-rs-scheduler#$tmp/native-scheduler-store#" \
	"$source_seed" > "$tmp/seed"
chmod +x "$tmp/seed"

expected='cake-autorate-calibration-capabilities 3 native-autotune native-rating native-scheduler native-speedtest'

reset_state() {
	rm -rf "$tmp/state"
	mkdir -p "$tmp/state"
}

set_existing() {
	printf '%s\n' "$1" > "$tmp/state/engine"
	: > "$tmp/state/engine-present"
	: > "$tmp/state/globals"
}

run_seed() {
	upgrade="$1"
	capabilities="$2"
	daemon_ok="${3:-1}"
	TEST_STATE="$tmp/state" \
	TEST_CAPABILITIES="$capabilities" \
	TEST_DAEMON_OK="$daemon_ok" \
	TEST_FAIL_COMMIT="${TEST_FAIL_COMMIT:-0}" \
	PKG_UPGRADE="$upgrade" \
	PATH="$tmp/bin:/usr/bin:/bin" \
		sh "$tmp/seed"
}

assert_engine() {
	actual="$(cat "$tmp/state/engine")"
	[ "$actual" = "$1" ] || {
		echo "expected scheduler engine '$1', got '$actual'" >&2
		exit 1
	}
}

for value in native legacy invalid ''; do
	reset_state
	set_existing "$value"
	run_seed 0 "$expected"
	assert_engine "$value"
	[ ! -e "$tmp/state/actions" ] || {
		echo "existing scheduler owner '$value' must not be probed or committed" >&2
		exit 1
	}
done

reset_state
run_seed 0 "$expected"
assert_engine native
[ "$(grep -c '^probe$' "$tmp/state/actions")" -eq 1 ]
[ "$(grep -c '^commit$' "$tmp/state/actions")" -eq 1 ]

reset_state
run_seed 1 "$expected"
assert_engine legacy
if grep -q '^probe$' "$tmp/state/actions"; then
	echo 'upgrade migration must not probe into a new owner' >&2
	exit 1
fi

reset_state
: > "$tmp/state/instance"
run_seed 0 "$expected"
assert_engine legacy
if grep -q '^probe$' "$tmp/state/actions"; then
	echo 'a retained autorate instance must prevent silent native promotion after sysupgrade' >&2
	exit 1
fi

reset_state
mkdir -p "$tmp/native-scheduler-store"
run_seed 0 "$expected"
assert_engine legacy
if grep -q '^probe$' "$tmp/state/actions"; then
	echo 'a retained scheduler store must prevent silent owner replacement after sysupgrade' >&2
	exit 1
fi
rmdir "$tmp/native-scheduler-store"

for capabilities in \
	'cake-autorate-calibration-capabilities 2 native-autotune native-rating native-scheduler' \
	'cake-autorate-calibration-capabilities 3 native-autotune native-rating native-scheduler native-speedtest ' \
	'cake-autorate-calibration-capabilities 3 native-autotune native-rating'; do
	reset_state
	run_seed 0 "$capabilities"
	assert_engine legacy
done

reset_state
run_seed 0 "$expected" 0
assert_engine legacy

sed "s#$tmp/bin/daemon#$tmp/bin/missing-daemon#" "$tmp/seed" > "$tmp/missing-seed"
reset_state
TEST_STATE="$tmp/state" PKG_UPGRADE=0 PATH="$tmp/bin:/usr/bin:/bin" sh "$tmp/missing-seed"
assert_engine legacy

reset_state
run_seed 0 "$expected"
run_seed 1 'wrong-after-first-run'
assert_engine native
[ "$(grep -c '^probe$' "$tmp/state/actions")" -eq 1 ]
[ "$(grep -c '^commit$' "$tmp/state/actions")" -eq 1 ]

reset_state
TEST_STATE="$tmp/state"
TEST_CAPABILITIES="$expected"
TEST_DAEMON_OK=1
PKG_UPGRADE=0
PATH="$tmp/bin:/usr/bin:/bin"
export TEST_STATE TEST_CAPABILITIES TEST_DAEMON_OK PKG_UPGRADE PATH
( . "$tmp/seed" )
assert_engine native

reset_state
: > "$tmp/state/globals"
if TEST_FAIL_COMMIT=1 run_seed 0 "$expected"; then
	echo 'a failed UCI commit must fail scheduler ownership seeding' >&2
	exit 1
fi
[ ! -e "$tmp/state/engine-present" ] || {
	echo 'a failed UCI commit must revert the staged scheduler owner' >&2
	exit 1
}

if grep -Eq '(^|[^A-Za-z])(sleep|usleep)([^A-Za-z]|$)' "$source_seed"; then
	echo 'scheduler ownership seeding must be event-driven, not timer-driven' >&2
	exit 1
fi
if grep -q 'CAKE_AUTORATE_DAEMON' "$source_seed"; then
	echo 'shipped scheduler seeder must not expose an environment-controlled daemon target' >&2
	exit 1
fi

printf '%s\n' 'scheduler engine seed tests passed'
