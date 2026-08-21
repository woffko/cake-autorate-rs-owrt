#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/cake-init-sqm-change-test.XXXXXX")"
full_script="$work/cake-autorate.full"
lite_script="$work/cake-autorate.lite"
trap 'rm -rf "$work"' EXIT INT TERM

sh "$base/scripts/render-init-variant.sh" full \
	"$base/files/etc/init.d/cake-autorate" "$full_script"
sh "$base/scripts/render-init-variant.sh" lite \
	"$base/files/etc/init.d/cake-autorate" "$lite_script"

for rendered in "$full_script" "$lite_script"; do
	grep -Fq '"$DAEMON" --service-lifecycle prepare-start' "$rendered"
	! grep -Fq -- '--sqm-project apply' "$rendered"
	! grep -Fq 'sqm-project-v1' "$rendered"
	! grep -Fq 'set_sqm_option()' "$rendered"
	! grep -Fq 'set_sqm_optional_option()' "$rendered"
done

printf '%s\n' 'init native lifecycle-owned SQM projection boundary tests passed'
