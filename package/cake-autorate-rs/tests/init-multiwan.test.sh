#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
work="${TMPDIR:-/tmp}/cake-init-multiwan-test.$$"
mkdir -p "$work"
trap 'rm -rf "$work"' EXIT INT TERM

CAKE_AUTORATE_SQM_CONFLICT_FILE="$work/conflicts"
export CAKE_AUTORATE_SQM_CONFLICT_FILE
. "$base/files/etc/init.d/cake-autorate"

second_enabled=1
reload_count=0
log_events=""

uci() {
	if [ "$1" = "-q" ] && [ "$2" = "show" ] && [ "$3" = "cake-autorate" ]; then
		printf '%s\n' \
			"cake-autorate.wan=cake_autorate" \
			"cake-autorate.wanb=cake_autorate"
		return 0
	fi
	return 1
}

config_get_bool() {
	local variable="$1" section="$2" option="$3" fallback="${4:-0}" value
	value="$fallback"
	case "$section.$option" in
		wan.enabled|wan.manage_sqm|wan.sqm_enabled) value=1 ;;
		wanb.enabled) value="$second_enabled" ;;
		wanb.manage_sqm|wanb.sqm_enabled) value=1 ;;
	esac
	eval "$variable=\$value"
}

config_get() {
	local variable="$1" section="$2" option="$3" fallback="${4:-}" value
	value="$fallback"
	case "$section.$option" in
		wan.sqm_interface|wanb.sqm_interface) value=lo ;;
		wan.mwan3_member) value=wan ;;
		wanb.mwan3_member) value=wanb ;;
	esac
	eval "$variable=\$value"
}

config_load() { :; }
pgrep() { return 1; }
reload_service() { reload_count=$((reload_count + 1)); }
logger() { log_events="${log_events}${log_events:+|}$*"; }

detect_managed_sqm_conflicts
[ "$(sort "$SQM_CONFLICT_FILE")" = "wan
wanb" ] || {
	echo "duplicate managed SQM target was not rejected" >&2
	exit 1
}

second_enabled=0
detect_managed_sqm_conflicts
[ ! -s "$SQM_CONFLICT_FILE" ] || {
	echo "disabled instance incorrectly remained in conflict set" >&2
	exit 1
}

# Member-up hints never reload all SQM/controllers. The continuously
# registered per-instance daemon owns delayed-link recovery, so an absent
# process is diagnostic only and cannot disrupt the other WAN.
second_enabled=1
reload_count=0
log_events=""
recover_interface_locked wan
[ "$reload_count" -eq 0 ] || {
	echo "wan recovery hint triggered a global reload" >&2
	exit 1
}
case "$log_events" in
	*"controller 'wan' is absent"*) ;;
	*) echo "wan recovery hint omitted its targeted diagnostic" >&2; exit 1 ;;
esac
case "$log_events" in
	*"controller 'wanb' is absent"*)
		echo "wan recovery hint inspected the unrelated wanb controller" >&2
		exit 1
		;;
esac

reload_count=0
log_events=""
recover_interface_locked unknown
[ "$reload_count" -eq 0 ] && [ -z "$log_events" ] || {
	echo "unknown recovery hint was not an idempotent no-op" >&2
	exit 1
}

echo "init multi-WAN conflict tests passed"
