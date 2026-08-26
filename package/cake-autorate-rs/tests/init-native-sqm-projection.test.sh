#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/cake-native-service-start-test.XXXXXX")"
rendered="$work/cake-autorate.full"
daemon="$work/cake-autorated"
calls="$work/calls"
events="$work/events"
trap 'rm -rf "$work"' EXIT INT TERM

sh "$base/scripts/render-init-variant.sh" full \
	"$base/files/etc/init.d/cake-autorate" "$rendered"

cat >"$daemon" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "$CAKE_TEST_CALLS"
case "$*" in
	'--service-lifecycle prepare-start')
		[ "${CAKE_TEST_EXIT:-0}" -eq 0 ] || exit "$CAKE_TEST_EXIT"
		printf '%s\n' "${CAKE_TEST_RESULT:-service-start-v2 - -}"
		;;
	'--service-lifecycle execute-stop')
		[ "${CAKE_TEST_STOP_EXIT:-0}" -eq 0 ] || exit "$CAKE_TEST_STOP_EXIT"
		printf '%s\n' "${CAKE_TEST_STOP_RESULT-service-stop-v1 ok}"
		;;
	*) exit 64 ;;
esac
EOF
chmod +x "$daemon"

. "$rendered"
DAEMON="$daemon"
CAKE_TEST_CALLS="$calls"
export CAKE_TEST_CALLS

logger() { :; }
procd_open_instance() { printf 'open:%s\n' "$1" >> "$events"; }
procd_set_param() { printf 'set:%s:%s:%s:%s\n' "$1" "${2:-}" "${3:-}" "${4:-}" >> "$events"; }
procd_close_instance() { printf '%s\n' close >> "$events"; }

CAKE_TEST_RESULT='service-start-v2 wan,backup_2 wan'
export CAKE_TEST_RESULT
start_service_locked
[ "$(cat "$calls")" = '--service-lifecycle prepare-start' ]
expected='open:wan
set:command:'"$daemon"':--instance:wan
set:respawn:3600:5:5
set:stdout:1::
set:stderr:1::
close
open:backup_2
set:command:'"$daemon"':--instance:backup_2
set:respawn:3600:5:5
set:stdout:1::
set:stderr:1::
close
open:mqtt_wan
set:command:'"$daemon"':--mqtt-publisher:wan
set:respawn:3600:5:5
set:term_timeout:5::
set:stdout:1::
set:stderr:1::
close'
[ "$(cat "$events")" = "$expected" ] || {
	printf 'unexpected procd registration:\n%s\n' "$(cat "$events")" >&2
	exit 1
}

: >"$events"
CAKE_TEST_RESULT='service-start-v2 - -'
export CAKE_TEST_RESULT
start_service_locked
[ ! -s "$events" ] || {
	echo 'empty native start plan registered an instance' >&2
	exit 1
}
[ "$SERVICE_START_CONFIRM_DEFERRED" -eq 0 ] || {
	echo 'genuine empty native start plan was confused with package deferral' >&2
	exit 1
}

CAKE_TEST_RESULT='service-start-deferred-v1'
export CAKE_TEST_RESULT
start_service_locked
[ "$SERVICE_START_CONFIRM_DEFERRED" -eq 1 ] || {
	echo 'typed package start deferral was not preserved' >&2
	exit 1
}
[ ! -s "$events" ] || {
	echo 'typed package start deferral registered an instance' >&2
	exit 1
}

for malformed in \
	'service-start-v2 wan' \
	'service-start-v2 bad-name -' \
	'service-start-v2 ,wan -' \
	'service-start-v2 wan, -' \
	'service-start-v2 wan,,backup -' \
	'service-start-v2 wan,wan -' \
	'service-start-v2 wan bad-name' \
	'service-start-v2 wan mqtt,mqtt' \
	'service-start-v2 - - extra' \
	'service-start-deferred-v1 extra' \
	'service-start-v1 wan -' \
	'service-start-v2'; do
	: >"$events"
	CAKE_TEST_RESULT="$malformed"
	export CAKE_TEST_RESULT
	if start_service_locked >/dev/null 2>&1; then
		echo "malformed native service response was accepted: $malformed" >&2
		exit 1
	fi
	[ ! -s "$events" ] || {
		echo "malformed response partially registered instances: $malformed" >&2
		exit 1
	}
done

CAKE_TEST_EXIT=7
export CAKE_TEST_EXIT
if start_service_locked >/dev/null 2>&1; then
	echo 'failed native service preparation was accepted' >&2
	exit 1
fi

# rc.common serializes its accumulated JSON even when start_service merely
# returns non-zero.  A construction failure must therefore terminate the
# child before procd_close_service can publish a prefix of the instance set.
CAKE_TEST_EXIT=0
CAKE_TEST_RESULT='service-start-v2 first,second,third -'
export CAKE_TEST_EXIT CAKE_TEST_RESULT
attempts="$work/attempts"
if (
	service_add_instance() {
		printf '%s\n' "$1" >> "$attempts"
		[ "$1" != second ]
	}
	start_service_locked
	printf '%s\n' reached-procd-close >> "$attempts"
); then
	echo 'procd construction failure did not terminate the init child' >&2
	exit 1
fi
[ "$(cat "$attempts")" = 'first
second' ] || {
	echo 'procd failure did not stop before the outer JSON commit' >&2
	cat "$attempts" >&2
	exit 1
}

# Stop accepts exactly one native transaction response.  A malformed or failed
# response must exit the child before rc.common can reach procd_kill.
service_runtime_lock_acquire_or_exit() {
	SERVICE_RUNTIME_LOCK_HELD=1
	exec 8>>"$work/runtime.guard"
}
service_runtime_lock_release_or_exit() {
	SERVICE_RUNTIME_LOCK_HELD=0
	exec 8>&-
}
: > "$calls"
stop_service
[ "$(cat "$calls")" = '--service-lifecycle execute-stop' ]

for bad in 'service-stop-v2 ok' 'service-stop-v1 failed' 'service-stop-v1 ok extra' ''; do
	if (
		CAKE_TEST_STOP_RESULT="$bad"
		export CAKE_TEST_STOP_RESULT
		stop_service
		printf '%s\n' forbidden-procd-kill
	) >/dev/null 2>&1; then
		echo "malformed native stop response was accepted: $bad" >&2
		exit 1
	fi
done

if (
	CAKE_TEST_STOP_EXIT=9
	export CAKE_TEST_STOP_EXIT
	stop_service
	printf '%s\n' forbidden-procd-kill
) >/dev/null 2>&1; then
	echo 'failed native stop was accepted' >&2
	exit 1
fi

printf '%s\n' 'init native service-start protocol tests passed'
