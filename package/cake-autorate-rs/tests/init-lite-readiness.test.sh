#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/cake-lite-readiness-test.XXXXXX")"
trap 'rm -rf "$work"' EXIT INT TERM
sh "$base/scripts/render-init-variant.sh" lite \
	"$base/files/etc/init.d/cake-autorate" "$work/lite"
. "$work/lite"
DAEMON=fake_daemon
# `logger -t TAG` reads its lines from stdin; `logger -t TAG MESSAGE` logs one.
logger() {
	if [ "$#" -eq 2 ]; then
		while IFS= read -r line; do
			printf '%s %s\n' "$2" "$line" >> "$work/log"
		done
	else
		printf '%s %s\n' "$2" "$3" >> "$work/log"
	fi
}
fake_daemon() {
	[ "$*" = '--service-lifecycle confirm-started' ] || return 64
	printf '%s\n' confirm >> "$work/events"
	case "$scenario" in
		valid) printf '%s\n' 'service-started-v1 ready' ;;
		warned)
			printf '%s\n' 'WARNING: controller input cleanup deferred: busy' >&2
			printf '%s\n' 'service-started-v1 ready'
			;;
		failed)
			printf '%s\n' 'ERROR: controller wan did not become ready' >&2
			return 72
			;;
		failed-after-answer)
			printf '%s\n' 'service-started-v1 ready'
			return 1
			;;
		malformed) printf '%s\n' 'service-started-v1 ready extra' ;;
		empty) : ;;
	esac
}
failed='cake-autorate native controller lifecycle readiness confirmation failed'
invalid='cake-autorate native controller lifecycle returned an invalid readiness confirmation'
for scenario in valid warned failed failed-after-answer malformed empty; do
	: > "$work/events"
	: > "$work/log"
	SERVICE_START_CONFIRM_DEFERRED=0
	code=0
	service_started || code=$?
	[ "$(cat "$work/events")" = confirm ] || exit 1
	case "$scenario" in
		valid|warned) [ "$code" = 0 ] || exit 1 ;;
		*) [ "$code" != 0 ] || { echo "readiness accepted: $scenario" >&2; exit 1; } ;;
	esac
	case "$scenario" in
		valid) expected= ;;
		warned) expected='cake-autorate WARNING: controller input cleanup deferred: busy' ;;
		failed) expected="cake-autorate ERROR: controller wan did not become ready
$failed" ;;
		failed-after-answer) expected="$failed" ;;
		*) expected="$invalid" ;;
	esac
	[ "$(cat "$work/log")" = "$expected" ] || {
		echo "readiness confirmation logged the wrong lines for $scenario:" >&2
		cat "$work/log" >&2
		exit 1
	}
done
: > "$work/events"
SERVICE_START_CONFIRM_DEFERRED=1
service_started
[ ! -s "$work/events" ] || { echo 'deferred package upgrade performed readiness' >&2; exit 1; }
echo 'Lite native readiness and upgrade deferral tests passed'
