#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/cake-init-runtime-cleanup-test.XXXXXX")"
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
	if grep -Eq '^cleanup_runtime_files\(\)|^stop_managed_sqm_backend\(\)|sleep 1|pgrep -f' "$rendered"; then
		echo "rendered init still owns delayed runtime cleanup" >&2
		exit 1
	fi
	grep -Fq -- '"$DAEMON" --service-lifecycle execute-stop' "$rendered"
done

cat >"$daemon" <<'EOF'
#!/bin/sh
printf '%s:%s:%s\n' "$*" "${CAKE_AUTORATE_SERVICE_LOCK_BORROW:-0}" "${CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_FD:-}" >> "$CAKE_TEST_EVENTS"
[ "${CAKE_TEST_STOP_EXIT:-0}" -eq 0 ] || exit "$CAKE_TEST_STOP_EXIT"
printf '%s\n' "${CAKE_TEST_STOP_RESULT:-service-stop-v1 ok}"
EOF
chmod +x "$daemon"

(
	. "$full"
	DAEMON="$daemon"
	CAKE_TEST_EVENTS="$events"
	export CAKE_TEST_EVENTS
	logger() { :; }
	service_runtime_lock_acquire_or_exit() {
		SERVICE_RUNTIME_LOCK_HELD=1
		exec 8>>"$work/runtime.guard"
	}
	service_runtime_lock_release_or_exit() {
		printf '%s\n' release >> "$events"
		SERVICE_RUNTIME_LOCK_HELD=0
		exec 8>&-
	}
	stop_service
	[ "$SERVICE_RUNTIME_STOP_RELEASE_PENDING" -eq 1 ]
	printf '%s\n' procd-kill >> "$events"
	service_stopped
	[ "$SERVICE_RUNTIME_STOP_RELEASE_PENDING" -eq 0 ]
	[ "$SERVICE_RUNTIME_LOCK_HELD" -eq 0 ]
)

expected='--service-lifecycle execute-stop:1:8
procd-kill
release'
[ "$(cat "$events")" = "$expected" ] || {
	echo "native stop/cleanup lock lifetime is incorrect" >&2
	cat "$events" >&2
	exit 1
}

: > "$events"
(
	. "$lite"
	DAEMON="$daemon"
	CAKE_TEST_EVENTS="$events"
	export CAKE_TEST_EVENTS
	logger() { :; }
	stop_service
	printf '%s\n' procd-kill >> "$events"
	service_stopped
)
expected_lite='--service-lifecycle execute-stop:0:
procd-kill'
[ "$(cat "$events")" = "$expected_lite" ] || {
	echo "Lite did not use the self-locking native stop protocol" >&2
	cat "$events" >&2
	exit 1
}

: > "$events"
if (
	. "$lite"
	DAEMON="$daemon"
	CAKE_TEST_EVENTS="$events"
	CAKE_TEST_STOP_RESULT='service-stop-v2 ok'
	export CAKE_TEST_EVENTS CAKE_TEST_STOP_RESULT
	logger() { :; }
	stop_service
	printf '%s\n' forbidden-procd-kill >> "$events"
); then
	echo "Lite accepted a malformed native stop response" >&2
	exit 1
fi
! grep -q forbidden-procd-kill "$events"

# A Rust refusal must terminate the rc.common child.  Returning non-zero is not
# enough because rc.common would still proceed to procd_kill.
: > "$events"
if (
	. "$full"
	DAEMON="$daemon"
	CAKE_TEST_EVENTS="$events"
	CAKE_TEST_STOP_EXIT=7
	export CAKE_TEST_EVENTS CAKE_TEST_STOP_EXIT
	logger() { :; }
	service_runtime_lock_acquire_or_exit() {
		SERVICE_RUNTIME_LOCK_HELD=1
		exec 8>>"$work/runtime.guard"
	}
	stop_service
	printf '%s\n' forbidden-procd-kill >> "$events"
); then
	echo "failed native stop did not terminate the rc.common child" >&2
	exit 1
fi
! grep -q forbidden-procd-kill "$events"

echo "init native runtime cleanup tests passed"
