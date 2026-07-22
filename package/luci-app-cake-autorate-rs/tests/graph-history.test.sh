#!/bin/sh
set -eu

ROOT="$(mktemp -d)"
trap 'rm -rf "$ROOT"' EXIT
mkdir -p "$ROOT/run/test" "$ROOT/bin"

i=1
while [ "$i" -le 250 ]; do
	printf '%s,1,1\n' "$i" >> "$ROOT/run/test/history.csv"
	i=$((i + 1))
done

cat > "$ROOT/bin/uci" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "$CAKE_AUTORATE_TEST_UCI_LOG"
case "$*" in
	'-q get cake-autorate.globals') exit 1 ;;
esac
EOF
chmod +x "$ROOT/bin/uci"

HELPER="$(dirname "$0")/../root/usr/libexec/cake-autorate-rs/graph-history"
export CAKE_AUTORATE_RUN_ROOT="$ROOT/run"
export CAKE_AUTORATE_UCI_BIN="$ROOT/bin/uci"
export CAKE_AUTORATE_TEST_UCI_LOG="$ROOT/uci.log"

"$HELPER" test read 0 100 > "$ROOT/latest"
[ "$(wc -l < "$ROOT/latest")" -eq 100 ]
[ "$(head -n 1 "$ROOT/latest" | cut -d, -f1)" = 151 ]
[ "$(tail -n 1 "$ROOT/latest" | cut -d, -f1)" = 250 ]

"$HELPER" test read 100 100 > "$ROOT/older"
[ "$(head -n 1 "$ROOT/older" | cut -d, -f1)" = 51 ]
[ "$(tail -n 1 "$ROOT/older" | cut -d, -f1)" = 150 ]

"$HELPER" test set-budget 102400
grep -q 'graph_history_ram_budget_kib=102400' "$ROOT/uci.log"
grep -q 'commit cake-autorate' "$ROOT/uci.log"
if "$HELPER" test set-budget 999999 >/dev/null 2>&1; then
	echo "invalid budget was accepted" >&2
	exit 1
fi

echo "graph-history helper tests passed"
