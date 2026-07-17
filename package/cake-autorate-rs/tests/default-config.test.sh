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
grep -q '/etc/init.d/cake-autorate restart' "$makefile"
grep -q '/etc/init.d/cake-autorate start' "$makefile"

printf '%s\n' 'default config clean-install tests passed'
