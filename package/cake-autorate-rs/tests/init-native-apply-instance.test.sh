#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
CAKE_AUTORATE_SQM_RUNNER=/bin/false
CAKE_AUTORATE_TRAFFIC_CLASSIFIER=/nonexistent
CAKE_AUTORATE_TC_BIN=tc
export CAKE_AUTORATE_SQM_RUNNER CAKE_AUTORATE_TRAFFIC_CLASSIFIER
export CAKE_AUTORATE_TC_BIN
. "$base/files/etc/init.d/cake-autorate"

events=""
conflict=0
configured_sqm_section=cake_wan_sqm

event() { events="${events}${events:+ }$1"; }
logger() { event "log:$2"; }
service_runtime_lock_acquire_or_exit() { event lock; }
service_runtime_lock_release_or_exit() { event unlock; }
service_apply_guard_preflight() { event guard; }
config_load() { event config-load; }
detect_managed_sqm_conflicts() { event detect-conflicts; }
section_has_sqm_conflict() { [ "$conflict" -eq 1 ]; }
native_apply_stop_selected_controller() { event "stop-controller:$1"; }
sync_sqm_instance() {
	event "sync-sqm:$1"
	SQM_MANAGED=1
	SQM_CONFIG_CHANGED="${sync_sqm_changed:-1}"
	SQM_SYNC_FAILED="${sync_sqm_failed:-0}"
	SQM_INGRESS_INTERFACES=pppoe-wan
}
remove_empty_clsact() { event "prepare-ingress:$1"; }
resolve_interface_device() { printf '%s\n' "$1"; }
rc_procd() { event "procd-add:$2"; }

config_get() {
	variable="$1"
	option="$3"
	fallback="${4:-}"
	value="$fallback"
	case "$option" in
		sqm_section) value="$configured_sqm_section" ;;
		dl_if)
			case "$sqm_probe_state" in
				mismatched-ifb-cake) value=ifb-custom ;;
				*) value=ifb4pppoe-wan ;;
			esac
			;;
	esac
	eval "$variable=\$value"
}

uci() {
	if [ "$1" = -q ] && [ "$2" = get ]; then
		case "$3" in
			cake-autorate.wan_sqm) printf '%s\n' cake_autorate ;;
			sqm.cake_wan_sqm._cake_autorate_managed) printf '%s\n' wan_sqm ;;
			sqm.cake_wan_sqm.enabled) printf '%s\n' 1 ;;
			sqm.cake_wan_sqm.interface)
				case "$sqm_probe_state" in
					logical-unresolved) printf '%s\n' wan ;;
					*) printf '%s\n' pppoe-wan ;;
				esac
				;;
			*) return 1 ;;
		esac
		return 0
	fi
	if [ "$1" = commit ] && [ "$2" = sqm ]; then
		event commit-sqm
		return 0
	fi
	return 1
}

sqm_probe_state=absent
native_apply_netdev_exists() {
	case "$1:$sqm_probe_state" in
		pppoe-wan:target-missing*) return 1 ;;
		pppoe-wan:*) return 0 ;;
		ifb4pppoe-wan:ifb-cake) return 0 ;;
		ifb4pppoe-wan:mismatched-ifb-cake) return 0 ;;
		ifb4pppoe-wan:target-missing-ifb-cake) return 0 ;;
		*) return 1 ;;
	esac
}
tc() {
	[ "$1" = qdisc ] && [ "$2" = show ] && [ "$3" = dev ] || return 1
	case "$4:$sqm_probe_state" in
		pppoe-wan:tc-fail) return 1 ;;
		pppoe-wan:cake) printf '%s\n' 'qdisc cake 8001: root' ;;
		pppoe-wan:ingress) printf '%s\n' 'qdisc ingress ffff: parent ffff:fff1' ;;
		pppoe-wan:clsact) printf '%s\n' 'qdisc clsact ffff: parent ffff:fff1' ;;
		pppoe-wan:*) printf '%s\n' 'qdisc fq_codel 0: root' ;;
		ifb4pppoe-wan:ifb-cake) printf '%s\n' 'qdisc cake 8002: root' ;;
		ifb4pppoe-wan:mismatched-ifb-cake) printf '%s\n' 'qdisc cake 8003: root' ;;
		ifb4pppoe-wan:target-missing-ifb-cake) printf '%s\n' 'qdisc cake 8004: root' ;;
		*) return 1 ;;
	esac
}

# A failed sqm-scripts stop is idempotently acceptable only after exact
# runtime probes prove that neither direction nor the ingress hook remains.
native_apply_stop_selected_sqm cake_wan_sqm wan_sqm pppoe-wan
[ "$NATIVE_APPLY_SELECTED_IFACE" = pppoe-wan ]
sqm_probe_state=target-missing
native_apply_stop_selected_sqm cake_wan_sqm wan_sqm pppoe-wan
for sqm_probe_state in cake ingress clsact ifb-cake mismatched-ifb-cake \
	target-missing-ifb-cake logical-unresolved tc-fail; do
	if native_apply_stop_selected_sqm cake_wan_sqm wan_sqm pppoe-wan; then
		echo "unsafe SQM runtime '$sqm_probe_state' was accepted as already stopped" >&2
		exit 1
	fi
done
sqm_probe_state=absent
SQM_RUNNER=/bin/true
events=""

native_apply_stop_selected_sqm() {
	event "stop-sqm:$1"
	NATIVE_APPLY_SELECTED_IFACE=pppoe-wan
}

CAKE_AUTORATE_NATIVE_APPLY_RECOVERY=1
CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_BORROW=1
CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_FD=8
export CAKE_AUTORATE_NATIVE_APPLY_RECOVERY
export CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_BORROW
export CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_FD

native_apply_restart_instance wan_sqm cake_wan_sqm pppoe-wan
[ "$events" = "lock config-load guard detect-conflicts stop-controller:wan_sqm stop-sqm:cake_wan_sqm sync-sqm:wan_sqm commit-sqm prepare-ingress:pppoe-wan procd-add:wan_sqm unlock" ] || {
	echo "selected native Apply restart order/scope is wrong: $events" >&2
	exit 1
}

# An exact already-materialized SQM projection must not be rewritten or
# committed. This preserves the byte-exact candidate used by native recovery.
events=""
sync_sqm_changed=0
native_apply_restart_instance wan_sqm cake_wan_sqm pppoe-wan
[ "$events" = "lock config-load guard detect-conflicts stop-controller:wan_sqm stop-sqm:cake_wan_sqm sync-sqm:wan_sqm prepare-ingress:pppoe-wan procd-add:wan_sqm unlock" ] || {
	echo "unchanged selected native Apply restart committed SQM: $events" >&2
	exit 1
}
sync_sqm_changed=1

events=""
sync_sqm_failed=1
if native_apply_restart_instance wan_sqm cake_wan_sqm pppoe-wan; then
	echo "failed selected SQM synchronization was accepted" >&2
	exit 1
fi
case " $events " in
	*" commit-sqm "*|*" prepare-ingress:"*|*" procd-add:"*)
		echo "failed selected SQM synchronization progressed to runtime mutation: $events" >&2
		exit 1
		;;
esac
sync_sqm_failed=0

events=""
native_apply_stop_instance wan_sqm cake_wan_sqm pppoe-wan
[ "$events" = "lock config-load stop-controller:wan_sqm stop-sqm:cake_wan_sqm unlock" ] || {
	echo "selected native Apply containment order/scope is wrong: $events" >&2
	exit 1
}

events=""
conflict=1
if native_apply_restart_instance wan_sqm cake_wan_sqm pppoe-wan; then
	echo "conflicting selected native Apply instance was restarted" >&2
	exit 1
fi
case " $events " in
	*" stop-controller:"*|*" stop-sqm:"*|*" sync-sqm:"*)
		echo "conflict rejection mutated selected runtime: $events" >&2
		exit 1
		;;
esac
conflict=0

events=""
configured_sqm_section=foreign_sqm
if native_apply_restart_instance wan_sqm cake_wan_sqm pppoe-wan; then
	echo "changed instance-to-SQM binding was accepted" >&2
	exit 1
fi
case " $events " in
	*" stop-controller:"*|*" stop-sqm:"*|*" sync-sqm:"*)
		echo "binding rejection mutated selected runtime: $events" >&2
		exit 1
		;;
esac
configured_sqm_section=cake_wan_sqm

events=""
if native_apply_restart_instance wan_sqm cake_wan_sqm eth9; then
	echo "changed immutable target interface was accepted" >&2
	exit 1
fi
case " $events " in
	*" stop-controller:"*|*" stop-sqm:"*|*" sync-sqm:"*)
		echo "target rejection mutated selected runtime: $events" >&2
		exit 1
		;;
esac

events=""
CAKE_AUTORATE_NATIVE_APPLY_RECOVERY=0
if native_apply_restart_instance wan_sqm cake_wan_sqm pppoe-wan; then
	echo "native Apply private marker was not required" >&2
	exit 1
fi
case " $events " in
	*" stop-controller:"*|*" stop-sqm:"*|*" sync-sqm:"*)
		echo "missing-marker rejection mutated selected runtime: $events" >&2
		exit 1
		;;
esac

if native_apply_restart_instance 'wan;bad' cake_wan_sqm pppoe-wan; then
	echo "unsafe native Apply instance identifier was accepted" >&2
	exit 1
fi
if native_apply_restart_instance wan_sqm cake_wan_sqm 'pppoe-wan;bad'; then
	echo "unsafe native Apply target interface was accepted" >&2
	exit 1
fi
if native_apply_restart_instance wan_sqm cake_wan_sqm pppoe-wan unexpected; then
	echo "extra native Apply restart argument was accepted" >&2
	exit 1
fi

echo "init native Apply selected-instance tests passed"
