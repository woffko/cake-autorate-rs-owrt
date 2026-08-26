#!/bin/sh
set -eu

test_dir="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
config="$test_dir/../files/etc/config/cake-autorate"
makefile="$test_dir/../Makefile"
service_lifecycle="$test_dir/../src/src/operations/service_lifecycle.rs"

grep -q "^config globals 'globals'$" "$config"
if grep -q '^config cake_autorate ' "$config"; then
	echo "fresh-install config must not create an autorate instance" >&2
	exit 1
fi
grep -q "graph_history_ram_budget_kib 'auto'" "$config"
if grep -q 'autotune_scheduler_engine' "$config"; then
	echo 'fresh-install config must not retain the retired scheduler owner selector' >&2
	exit 1
fi
grep -q '^define Package/cake-autorate-rs/postinst' "$makefile"
grep -q 'PKG_UPGRADE' "$makefile"
if grep -q 'scheduler-engine\|scheduler-owner-seed\|autotune_scheduler_engine' "$makefile"; then
	echo 'package payload must not retain scheduler owner migration policy' >&2
	exit 1
fi
grep -q 'PKG_UPGRADE=0 /etc/init.d/cake-autorate restart' "$makefile"
grep -q 'files/etc/init.d/cake-autorate-autotune' "$makefile"
grep -q '/etc/init.d/cake-autorate-autotune enable >/dev/null 2>&1 || exit 1' "$makefile"
grep -q '/etc/init.d/cake-autorate-autotune stop >/dev/null 2>&1 || exit 1' "$makefile"
grep -q '/etc/init.d/cake-autorate-autotune start >/dev/null 2>&1 || exit 1' "$makefile"
if grep -q 'ubus call service signal.*cake-autorate-autotune' "$makefile"; then
	echo 'package upgrade must restart the complete Auto-Tune procd service' >&2
	exit 1
fi
grep -q 'restart >/dev/null 2>&1 || exit 1' "$makefile"
if grep -q 'PKG_UPGRADE' "$test_dir/../files/etc/init.d/cake-autorate"; then
	echo 'rc.common must not retain package-upgrade start policy' >&2
	exit 1
fi
grep -q 'package_upgrade_mode()' "$service_lifecycle"
grep -q 'return Ok(format!("{SERVICE_START_DEFERRED_V1}\\n"))' "$service_lifecycle"

printf '%s\n' 'default config clean-install tests passed'
