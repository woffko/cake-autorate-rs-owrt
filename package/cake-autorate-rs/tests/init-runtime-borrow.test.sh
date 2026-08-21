#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/cake-init-runtime-borrow-test.XXXXXX")"
full="$work/cake-autorate.full"
lite="$work/cake-autorate.lite"
daemon="$work/cake-autorated"
events="$work/events"
trap 'rm -rf "$work"' EXIT INT TERM

sh "$base/scripts/render-init-variant.sh" full \
	"$base/files/etc/init.d/cake-autorate" "$full"
sh "$base/scripts/render-init-variant.sh" lite \
	"$base/files/etc/init.d/cake-autorate" "$lite"

for rendered in "$full" "$lite"; do
	sh -n "$rendered"
	if grep -Eq 'service_legacy_apply_markers_present|service_legacy_apply_recovery_gate|\._autotune_apply_|uci -q show' "$rendered"; then
		echo "rendered init still owns retired Apply marker policy" >&2
		exit 1
	fi
done

cat >"$daemon" <<'EOF'
#!/bin/sh
printf '%s:service-borrow=%s:runtime-borrow=%s:fd=%s:mode=%s\n' \
	"$*" \
	"${CAKE_AUTORATE_SERVICE_LOCK_BORROW:-0}" \
	"${CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_BORROW:-0}" \
	"${CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_FD:-}" \
	"${CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_MODE:-}" >> "$CAKE_TEST_EVENTS"
case "$*" in
	'--service-lifecycle prepare-start')
		if [ "${CAKE_TEST_LITE:-0}" = 1 ]; then
			printf '%s\n' 'service-start-v1 -'
		else
			printf '%s\n' 'service-start-v2 - -'
		fi
		;;
	'--service-lifecycle execute-stop') printf '%s\n' 'service-stop-v1 ok' ;;
	*) exit 64 ;;
esac
EOF
chmod +x "$daemon"

(
	. "$full"
	DAEMON="$daemon"
	CAKE_TEST_EVENTS="$events"
	CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_BORROW=1
	CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_FD=8
	CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_MODE=exclusive
	export CAKE_TEST_EVENTS CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_BORROW
	export CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_FD CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_MODE
	logger() { :; }
	procd_open_instance() { :; }
	procd_set_param() { :; }
	procd_close_instance() { :; }
	start_service_locked
)

expected='--service-lifecycle prepare-start:service-borrow=1:runtime-borrow=1:fd=8:mode=exclusive'
[ "$(cat "$events")" = "$expected" ] || {
	echo "Full init did not lend the exact fd 8 authority to Rust" >&2
	cat "$events" >&2
	exit 1
}

: >"$events"
(
	. "$lite"
	DAEMON="$daemon"
	CAKE_TEST_EVENTS="$events"
	CAKE_TEST_LITE=1
	export CAKE_TEST_EVENTS CAKE_TEST_LITE
	logger() { :; }
	procd_open_instance() { :; }
	procd_set_param() { :; }
	procd_close_instance() { :; }
	start_service_locked
)
expected_lite='--service-lifecycle prepare-start:service-borrow=0:runtime-borrow=0:fd=:mode='
[ "$(cat "$events")" = "$expected_lite" ] || {
	echo "Lite init did not delegate a self-locking start to Rust" >&2
	cat "$events" >&2
	exit 1
}

echo "init native runtime-lock delegation tests passed"
