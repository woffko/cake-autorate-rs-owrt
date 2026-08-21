#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/cake-init-traffic-profile-test.XXXXXX")"
full_script="$work/cake-autorate.full"
lite_script="$work/cake-autorate.lite"
trap 'rm -rf "$work"' EXIT INT TERM

sh "$base/scripts/render-init-variant.sh" full \
	"$base/files/etc/init.d/cake-autorate" "$full_script"
sh "$base/scripts/render-init-variant.sh" lite \
	"$base/files/etc/init.d/cake-autorate" "$lite_script"

for rendered in "$full_script" "$lite_script"; do
	grep -Fq '"$DAEMON" --service-lifecycle prepare-start' "$rendered"
	! grep -Fq '"$DAEMON" --sync-presets' "$rendered"
	! grep -Fq 'sync_interface_presets' "$rendered"
	! grep -Fq 'migrate_legacy_route_instance' "$rendered"
	! grep -Fq 'sync_rate_preset_instance' "$rendered"
	! grep -Fq 'set_cake_option_if_changed' "$rendered"
done

! grep -Fq 'migrate_traffic_profile_instance' "$full_script"
! grep -Fq 'canonical_traffic_autotune_profile' "$full_script"

printf '%s\n' 'init native lifecycle preset-migration boundary tests passed'
