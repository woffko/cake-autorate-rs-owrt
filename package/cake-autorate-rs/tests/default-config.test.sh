#!/bin/sh
set -eu

test_dir="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
config="$test_dir/../files/etc/config/cake-autorate"
makefile="$test_dir/../Makefile"

grep -q "^config globals 'globals'$" "$config"
if grep -q '^config cake_autorate ' "$config"; then
	echo "fresh-install config must not create an autorate instance" >&2
	exit 1
fi
grep -q "graph_history_ram_budget_kib 'auto'" "$config"
if grep -q 'autotune_scheduler_engine' "$config"; then
	echo 'fresh-install scheduler ownership must be decided by UCI defaults, not the static conffile' >&2
	exit 1
fi
grep -q '^define Package/cake-autorate-rs/postinst' "$makefile"
grep -q 'PKG_UPGRADE' "$makefile"
grep -q 'files/etc/uci-defaults/99-cake-autorate-rs-scheduler-engine' "$makefile"
if grep -q 'uci -q set cake-autorate\.globals\.autotune_scheduler_engine=' "$makefile"; then
	echo 'package postinst must not duplicate the UCI-defaults ownership decision' >&2
	exit 1
fi
grep -q 'PKG_UPGRADE=0 /etc/init.d/cake-autorate restart' "$makefile"
grep -q '\[ -x /etc/init.d/cake-autorate-autotune \]' "$makefile"
grep -q '/etc/init.d/cake-autorate-autotune restart >/dev/null 2>&1 || exit 1' "$makefile"
if grep -q 'ubus call service signal.*cake-autorate-autotune' "$makefile"; then
	echo 'package upgrade must restart the complete Auto-Tune procd service' >&2
	exit 1
fi
grep -q 'restart >/dev/null 2>&1 || exit 1' "$makefile"
grep -q '\[ "${PKG_UPGRADE:-0}" = 1 \] && return 0' \
	"$test_dir/../files/etc/init.d/cake-autorate"

printf '%s\n' 'default config clean-install tests passed'
