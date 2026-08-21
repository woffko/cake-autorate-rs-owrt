#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/cake-init-multiwan-test.XXXXXX")"
trap 'rm -rf "$work"' EXIT INT TERM

for variant in full lite; do
	rendered="$work/cake-autorate.$variant"
	sh "$base/scripts/render-init-variant.sh" "$variant" \
		"$base/files/etc/init.d/cake-autorate" "$rendered"
	sh -n "$rendered"
	grep -Fq 'procd_add_reload_trigger "$CONFIG"' "$rendered"
	grep -Fq 'procd_add_reload_trigger "$SQM_CONFIG"' "$rendered"
	if grep -Eq 'recover_interface|procd_add_interface_trigger|interface\.\*\.up' "$rendered"; then
		echo "$variant init still exposes the retired diagnostic-only member trigger" >&2
		exit 1
	fi
done

# Member-up recovery is continuously owned by each Rust controller's route
# state machine and procd respawn.  The removed init verb had no mutation or
# recovery effect; retaining its trigger would turn every link-up into an
# unknown rc.common command.
echo "init multi-WAN trigger boundary tests passed"
