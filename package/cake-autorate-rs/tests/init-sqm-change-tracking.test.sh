#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
. "$base/files/etc/init.d/cake-autorate"

events=""
download=100000
optional_present=0
sync_value=100000
fail_set=0

event() { events="${events}${events:+ }$1"; }

uci() {
	if [ "$1" = -q ] && [ "$2" = get ]; then
		case "$3" in
			sqm.queue.download) printf '%s\n' "$download" ;;
			sqm.queue.iqdisc_opts)
				[ "$optional_present" -eq 1 ] || return 1
				printf '%s\n' diffserv4
				;;
			*) return 1 ;;
		esac
		return 0
	fi
	if [ "$1" = set ]; then
		event "set:$2"
		[ "$fail_set" -eq 0 ] || return 1
		case "$2" in
			sqm.queue.download=*) download="${2#*=}" ;;
			*) return 1 ;;
		esac
		return 0
	fi
	if [ "$1" = -q ] && [ "$2" = delete ] &&
	   [ "$3" = sqm.queue.iqdisc_opts ]; then
		optional_present=0
		event "delete:$3"
		return 0
	fi
	if [ "$1" = commit ] && [ "$2" = sqm ]; then
		event commit:sqm
		return 0
	fi
	return 1
}

SQM_CONFIG_CHANGED=0
set_sqm_option queue download 100000
[ -z "$events" ] && [ "$SQM_CONFIG_CHANGED" -eq 0 ] || {
	echo "equal SQM scalar was rewritten: $events" >&2
	exit 1
}

set_sqm_option queue download 120000
[ "$events" = "set:sqm.queue.download=120000" ] &&
	[ "$SQM_CONFIG_CHANGED" -eq 1 ] && [ "$download" = 120000 ] || {
	echo "changed SQM scalar was not tracked exactly: $events" >&2
	exit 1
}

events=""
SQM_CONFIG_CHANGED=0
set_sqm_optional_option queue iqdisc_opts ""
[ -z "$events" ] && [ "$SQM_CONFIG_CHANGED" -eq 0 ] || {
	echo "missing optional SQM value was deleted: $events" >&2
	exit 1
}

optional_present=1
set_sqm_optional_option queue iqdisc_opts ""
[ "$events" = "delete:sqm.queue.iqdisc_opts" ] &&
	[ "$SQM_CONFIG_CHANGED" -eq 1 ] || {
	echo "present optional SQM value deletion was not tracked: $events" >&2
	exit 1
}

sync_sqm_instance() {
	SQM_MANAGED=1
	set_sqm_option queue download "$sync_value"
}
config_foreach() { sync_sqm_instance wan; }

events=""
sync_value="$download"
sync_sqm_config
[ -z "$events" ] || {
	echo "unchanged SQM synchronization committed configuration: $events" >&2
	exit 1
}

events=""
sync_value=130000
sync_sqm_config
[ "$events" = "set:sqm.queue.download=130000 commit:sqm" ] || {
	echo "changed SQM synchronization did not commit exactly once: $events" >&2
	exit 1
}

events=""
sync_value=140000
fail_set=1
if sync_sqm_config; then
	echo "failed SQM write was accepted" >&2
	exit 1
fi
[ "$events" = "set:sqm.queue.download=140000" ] || {
	echo "failed SQM write was committed or retried unexpectedly: $events" >&2
	exit 1
}

echo "init SQM change-tracking tests passed"
