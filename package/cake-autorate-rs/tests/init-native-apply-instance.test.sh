#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/cake-init-native-apply-test.XXXXXX")"
init_script="$work/cake-autorate.full"
trap 'rm -rf "$work"' EXIT INT TERM
sh "$base/scripts/render-init-variant.sh" full \
	"$base/files/etc/init.d/cake-autorate" "$init_script"

# Selected-instance business authority is Rust-owned.  rc.common retains only
# the mechanical procd JSON builder that OpenWrt itself provides.
. "$init_script"
events=""
event() { events="${events}${events:+ }$1"; }
procd_open_instance() { event "open:$1"; }
procd_set_param() { event "set:$*"; }
procd_close_instance() { event close; }
rc_procd() {
	event "rc:$1:$2"
	callback="$1"
	shift
	"$callback" "$@"
}

CAKE_AUTORATE_NATIVE_APPLY_RECOVERY=1
CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_BORROW=1
CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_FD=8
export CAKE_AUTORATE_NATIVE_APPLY_RECOVERY
export CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_BORROW
export CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_FD

native_apply_register_instance wan_sqm
[ "$events" = "rc:native_apply_add_selected_instance:wan_sqm open:wan_sqm set:command /usr/sbin/cake-autorated --instance wan_sqm set:respawn 3600 5 5 set:stdout 1 set:stderr 1 close" ] || {
	echo "native Apply procd bridge changed: $events" >&2
	exit 1
}

generation=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
events=""
native_apply_register_instance wan_sqm "$generation"
[ "$events" = "rc:native_apply_add_selected_instance:wan_sqm open:wan_sqm set:command /usr/sbin/cake-autorated --instance wan_sqm set:env CAKE_AUTORATE_SERVICE_CONFIG_ID=$generation set:respawn 3600 5 5 set:stdout 1 set:stderr 1 close" ] || {
	echo "native Apply generation bridge changed: $events" >&2
	exit 1
}
for invalid in '' short "${generation%?}" "F${generation#?}" "$generation:extra"; do
	events=""
	if native_apply_register_instance wan_sqm "$invalid"; then
		echo 'native Apply accepted an invalid generation' >&2
		exit 1
	fi
	[ -z "$events" ] || { echo 'invalid generation reached procd' >&2; exit 1; }
done
if native_apply_register_instance wan_sqm "$generation" extra; then
	echo 'native Apply accepted extra registration arguments' >&2
	exit 1
fi

for unsafe in 'wan;reboot' 'wan-sqm' '' 'abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijklm'; do
	if native_apply_register_instance "$unsafe"; then
		echo "unsafe native Apply bridge instance was accepted: $unsafe" >&2
		exit 1
	fi
done

events=""
native_apply_register_mqtt wan_sqm
[ "$events" = "rc:native_apply_add_selected_mqtt:wan_sqm open:mqtt_wan_sqm set:command /usr/sbin/cake-autorated --mqtt-publisher wan_sqm set:respawn 3600 5 5 set:term_timeout 5 set:stdout 1 set:stderr 1 close" ] || {
	echo 'native selected MQTT bridge changed' >&2
	exit 1
}
for unsafe in 'wan;reboot' 'wan-sqm' '' 'abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijklm'; do
	events=""
	if native_apply_register_mqtt "$unsafe"; then
		echo 'unsafe selected MQTT bridge instance accepted' >&2
		exit 1
	fi
	[ -z "$events" ] || exit 1
done
events=""
if native_apply_register_mqtt wan_sqm extra; then exit 1; fi
[ -z "$events" ] || exit 1
endpoint=cmq1_0123456789abcdef0123456789abcdef
events=""
native_apply_register_mqtt wan_sqm "$endpoint"
[ "$events" = "rc:native_apply_add_selected_mqtt:wan_sqm open:mqtt_wan_sqm set:command /usr/sbin/cake-autorated --mqtt-publisher wan_sqm set:env CAKE_AUTORATE_MQTT_READY_ENDPOINT=$endpoint set:respawn 3600 5 5 set:term_timeout 5 set:stdout 1 set:stderr 1 close" ] || exit 1
for endpoint in cmq1_bad cmq1_0123456789abcdef0123456789abcdeF cmq1_0123456789abcdef0123456789abcdef0 '../socket'; do
	events=""
	if native_apply_register_mqtt wan_sqm "$endpoint"; then exit 1; fi
	[ -z "$events" ] || exit 1
done
if native_apply_register_mqtt wan_sqm cmq1_0123456789abcdef0123456789abcdef extra; then exit 1; fi

CAKE_AUTORATE_NATIVE_APPLY_RECOVERY=0
if native_apply_register_mqtt wan_sqm; then exit 1; fi
if native_apply_register_instance wan_sqm; then
	echo "native Apply bridge accepted a missing recovery authority" >&2
	exit 1
fi
CAKE_AUTORATE_NATIVE_APPLY_RECOVERY=1
CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_BORROW=0
if native_apply_register_mqtt wan_sqm; then exit 1; fi
if native_apply_register_instance wan_sqm; then
	echo "native Apply bridge accepted a missing borrowed lock" >&2
	exit 1
fi

if grep -Eq 'native_apply_(restart|stop)_instance|native_apply_stop_selected' "$init_script"; then
	echo "retired selected-instance shell lifecycle remains in the rendered init" >&2
	exit 1
fi

echo "native Apply selected-instance procd bridge tests passed"
