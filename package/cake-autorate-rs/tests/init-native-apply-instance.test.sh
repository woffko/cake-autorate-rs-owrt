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
	"$1" "$2"
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

for unsafe in 'wan;reboot' 'wan-sqm' '' 'abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijklm'; do
	if native_apply_register_instance "$unsafe"; then
		echo "unsafe native Apply bridge instance was accepted: $unsafe" >&2
		exit 1
	fi
done

CAKE_AUTORATE_NATIVE_APPLY_RECOVERY=0
if native_apply_register_instance wan_sqm; then
	echo "native Apply bridge accepted a missing recovery authority" >&2
	exit 1
fi
CAKE_AUTORATE_NATIVE_APPLY_RECOVERY=1
CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_BORROW=0
if native_apply_register_instance wan_sqm; then
	echo "native Apply bridge accepted a missing borrowed lock" >&2
	exit 1
fi

if grep -Eq 'native_apply_(restart|stop)_instance|native_apply_stop_selected' "$init_script"; then
	echo "retired selected-instance shell lifecycle remains in the rendered init" >&2
	exit 1
fi

echo "native Apply selected-instance procd bridge tests passed"
