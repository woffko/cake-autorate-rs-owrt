#!/bin/sh
set -eu

ROOT="$(mktemp -d)"
trap 'rm -rf "$ROOT"' EXIT
mkdir -p "$ROOT/bin"

cat > "$ROOT/bin/uci" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >> "$CAKE_AUTORATE_TEST_UCI_LOG"
case "$*" in
	'-q get cake-autorate.globals') exit "${CAKE_AUTORATE_TEST_GLOBALS_EXISTS:-1}" ;;
esac
EOF
chmod +x "$ROOT/bin/uci"

HELPER="$(dirname "$0")/../root/usr/libexec/cake-autorate-rs/status-columns"
export CAKE_AUTORATE_UCI_BIN="$ROOT/bin/uci"
export CAKE_AUTORATE_TEST_UCI_LOG="$ROOT/uci.log"

"$HELPER" set route cpu route
grep -q '^set cake-autorate.globals=globals$' "$ROOT/uci.log"
[ "$(grep -c '^add_list cake-autorate.globals.status_columns=route$' "$ROOT/uci.log")" -eq 1 ]
[ "$(grep -c '^add_list cake-autorate.globals.status_columns=cpu$' "$ROOT/uci.log")" -eq 1 ]
grep -q '^commit cake-autorate$' "$ROOT/uci.log"

if "$HELPER" set route 'bad;column' >/dev/null 2>&1; then
	echo "invalid column was accepted" >&2
	exit 1
fi

: > "$ROOT/uci.log"
export CAKE_AUTORATE_TEST_GLOBALS_EXISTS=0
"$HELPER" reset
grep -q '^-q delete cake-autorate.globals.status_columns$' "$ROOT/uci.log"
grep -q '^commit cake-autorate$' "$ROOT/uci.log"

echo "status-columns helper tests passed"
