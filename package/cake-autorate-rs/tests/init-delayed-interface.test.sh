#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
. "$base/files/etc/init.d/cake-autorate"

events=""
managed=1
sqm_enabled=1
backing=1
conflict=0

config_get_bool() {
	variable="$1"
	option="$3"
	fallback="${4:-0}"
	value="$fallback"
	case "$option" in
		enabled) value=1 ;;
		manage_sqm) value="$managed" ;;
		sqm_enabled) value="$sqm_enabled" ;;
	esac
	eval "$variable=\$value"
}

config_get() {
	variable="$1"
	option="$3"
	fallback="${4:-}"
	value="$fallback"
	case "$option" in
		sqm_interface|ul_if|wan_if) value=missing-wan ;;
		sqm_section) value=cake_delayed ;;
	esac
	eval "$variable=\$value"
}

section_has_sqm_conflict() {
	[ "$conflict" -eq 1 ]
}

instance_has_enabled_sqm_backing() {
	[ "$backing" -eq 1 ]
}

procd_open_instance() { events="${events}${events:+ }open:$1"; }
procd_set_param() { events="${events}${events:+ }set:$1"; }
procd_close_instance() { events="${events}${events:+ }close"; }
logger() { :; }

# Runtime devices, IFB and redirect are deliberately absent. A valid enabled
# instance must still be represented in procd so the daemon can wait and
# recover after a delayed WWAN/PPPoE link appears.
start_instance delayed
case " $events " in
	*" open:delayed "*) ;;
	*)
		echo "delayed interface prevented procd instance registration: $events" >&2
		exit 1
		;;
esac
case " $events " in *" set:command "*) ;; *)
	echo "delayed instance omitted its daemon command: $events" >&2
	exit 1
esac
case " $events " in *" close "*) ;; *)
	echo "delayed instance did not close its procd registration: $events" >&2
	exit 1
esac

# A managed instance with its SQM half disabled remains a configuration error,
# not a transient runtime wait.
events=""
sqm_enabled=0
start_instance delayed
[ -z "$events" ] || {
	echo "managed instance started while its SQM queue was disabled" >&2
	exit 1
}

# Unmanaged mode may wait for externally owned topology but must never require
# a cake-autorate-owned SQM section.
events=""
managed=0
backing=0
start_instance delayed
case " $events " in
	*" open:delayed "*) ;;
	*)
		echo "unmanaged delayed instance was not registered: $events" >&2
		exit 1
		;;
esac
case " $events " in *" close "*) ;; *)
	echo "unmanaged delayed instance registration was incomplete: $events" >&2
	exit 1
esac

# Static ownership conflicts remain fail-closed.
events=""
managed=1
sqm_enabled=1
backing=1
conflict=1
start_instance delayed
[ -z "$events" ] || {
	echo "conflicting managed instance was registered" >&2
	exit 1
}

echo "init delayed-interface tests passed"
