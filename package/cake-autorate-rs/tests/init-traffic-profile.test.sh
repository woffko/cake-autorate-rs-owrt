#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
. "$base/files/etc/init.d/cake-autorate"

traffic_profile=""
traffic_profile_migrated=""
autotune_profile=gaming
defaults_value=""
rule_active_profile=gaming
rule_inactive_profile=fair
changes=""

config_get() {
	local variable="$1" section="$2" option="$3" fallback="${4:-}" value
	value="$fallback"
	case "$section.$option" in
		wan.traffic_profile) value="$traffic_profile" ;;
		wan.traffic_profile_migrated) value="$traffic_profile_migrated" ;;
	esac
	eval "$variable=\$value"
}

uci() {
	case "$*" in
		"-q get cake-autorate.wan.autotune_profile") printf '%s\n' "$autotune_profile" ;;
		"-q get cake-autorate.wan.traffic_defaults_gaming")
			[ -n "$defaults_value" ] && printf '%s\n' "$defaults_value" || true
			;;
		"-q show cake-autorate")
			printf '%s\n' \
				'cake-autorate.active_rule=traffic_rule' \
				'cake-autorate.inactive_rule=traffic_rule'
			;;
		"-q get cake-autorate.active_rule.instance"|"-q get cake-autorate.inactive_rule.instance") printf 'wan\n' ;;
		"-q get cake-autorate.active_rule.profile") printf '%s\n' "$rule_active_profile" ;;
		"-q get cake-autorate.inactive_rule.profile") printf '%s\n' "$rule_inactive_profile" ;;
		"set cake-autorate.wan.traffic_profile=auto") traffic_profile=auto; changes="${changes}profile=auto\n" ;;
		"set cake-autorate.wan.traffic_profile=custom") traffic_profile=custom; changes="${changes}profile=custom\n" ;;
		"set cake-autorate.wan.traffic_profile_migrated=1") traffic_profile_migrated=1; changes="${changes}migrated=1\n" ;;
		"set cake-autorate.active_rule.profile=custom") rule_active_profile=custom; changes="${changes}active=custom\n" ;;
		"set cake-autorate.inactive_rule.profile=custom") rule_inactive_profile=custom; changes="${changes}inactive=custom\n" ;;
		*) return 1 ;;
	esac
}

logger() { :; }

CAKE_CONFIG_CHANGED=0
migrate_traffic_profile_instance wan
[ "$traffic_profile" = auto ]
[ "$traffic_profile_migrated" = 1 ]
[ "$CAKE_CONFIG_CHANGED" -eq 1 ]
[ "$rule_active_profile" = gaming ]
[ "$rule_inactive_profile" = fair ]

traffic_profile=""
traffic_profile_migrated=""
defaults_value=0
rule_active_profile=""
rule_inactive_profile=fair
changes=""
CAKE_CONFIG_CHANGED=0
migrate_traffic_profile_instance wan
[ "$traffic_profile" = custom ]
[ "$traffic_profile_migrated" = 1 ]
[ "$rule_active_profile" = custom ]
[ "$rule_inactive_profile" = fair ]

changes=""
CAKE_CONFIG_CHANGED=0
migrate_traffic_profile_instance wan
[ -z "$changes" ]
[ "$CAKE_CONFIG_CHANGED" -eq 0 ]

# A LuCI-written Custom selection must not bypass the old rule migration when
# the one-time marker is still absent.
traffic_profile=custom
traffic_profile_migrated=""
defaults_value=0
rule_active_profile=gaming
rule_inactive_profile=fair
changes=""
CAKE_CONFIG_CHANGED=0
migrate_traffic_profile_instance wan
[ "$rule_active_profile" = custom ]
[ "$rule_inactive_profile" = fair ]
[ "$traffic_profile_migrated" = 1 ]

printf '%s\n' 'init traffic-profile migration tests passed'
