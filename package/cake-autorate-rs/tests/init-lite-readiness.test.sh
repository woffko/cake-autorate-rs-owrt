#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/cake-lite-readiness-test.XXXXXX")"
trap 'rm -rf "$work"' EXIT INT TERM
sh "$base/scripts/render-init-variant.sh" lite \
	"$base/files/etc/init.d/cake-autorate" "$work/lite"
. "$work/lite"
DAEMON=fake_daemon
logger() { :; }
fake_daemon() {
	[ "$*" = '--service-lifecycle confirm-started' ] || return 64
	printf '%s\n' confirm >> "$work/events"
	case "$result" in
		valid) printf '%s\n' 'service-started-v1 ready' ;;
		failed) return 72 ;;
		malformed) printf '%s\n' 'service-started-v1 ready extra' ;;
		empty) : ;;
	esac
}
for result in valid failed malformed empty; do
	: > "$work/events"
	SERVICE_START_CONFIRM_DEFERRED=0
	code=0
	service_started || code=$?
	[ "$(cat "$work/events")" = confirm ] || exit 1
	if [ "$result" = valid ]; then
		[ "$code" = 0 ] || exit 1
	else
		[ "$code" != 0 ] || exit 1
	fi
done
: > "$work/events"
SERVICE_START_CONFIRM_DEFERRED=1
service_started
[ ! -s "$work/events" ] || { echo 'deferred package upgrade performed readiness' >&2; exit 1; }
echo 'Lite native readiness and upgrade deferral tests passed'
