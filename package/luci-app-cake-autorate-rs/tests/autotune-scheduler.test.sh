#!/bin/sh
set -eu

root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM
mkdir -p "$tmp/bin" "$tmp/state"
log="$tmp/commands.log"
count="$tmp/service-count"

cat > "$tmp/bin/jsonfilter" <<'EOF'
#!/bin/sh
while [ "$#" -gt 0 ]; do
	case "$1" in
		-e) expression="$2"; shift 2 ;;
		*) shift ;;
	esac
done
case "$expression" in
	*.minimum_kbps) echo 5000 ;;
	*.base_kbps) echo 20000 ;;
	*.maximum_kbps) echo 80000 ;;
	*.absolute_cap_kbps) echo 90000 ;;
	*.observed_low_kbps) echo 75000 ;;
	*.observed_median_kbps) echo 80000 ;;
	*.active_threshold_kbps) echo 2000 ;;
	*.thresholds_ms.adjust_up) echo 5 ;;
	*.thresholds_ms.delay) echo 15 ;;
	*.thresholds_ms.adjust_down) echo 30 ;;
	*.adaptive_ceiling.hold_s) echo 20 ;;
	*.adaptive_ceiling.growth_percent) echo 3 ;;
	*.adaptive_ceiling.probe_s) echo 8 ;;
	*.adaptive_ceiling.cooldown_s) echo 30 ;;
	*.adaptive_ceiling.failed_bound_ttl_s) echo 900 ;;
	*.link.overhead) echo 44 ;;
	*.link.mpu) echo 84 ;;
	*.adaptive_ceiling.enabled) echo true ;;
	*.link.layer) echo ethernet ;;
	*) exit 1 ;;
esac
EOF

cat > "$tmp/bin/uci" <<'EOF'
#!/bin/sh
[ "${1:-}" = -q ] && shift
command="${1:-}"
shift || true
case "$command" in
	batch) cat >/dev/null; echo uci:batch >> "$TEST_LOG" ;;
	set) echo "uci:set:$*" >> "$TEST_LOG" ;;
	commit) echo uci:commit >> "$TEST_LOG" ;;
	revert) echo uci:revert >> "$TEST_LOG" ;;
	changes) [ "${PENDING_CHANGES:-0}" = 1 ] && echo "cake-autorate.test.enabled='1'" || true ;;
	*) exit 1 ;;
esac
EOF

cat > "$tmp/service" <<'EOF'
#!/bin/sh
value=0
[ ! -s "$TEST_COUNT" ] || value="$(cat "$TEST_COUNT")"
value=$((value + 1))
echo "$value" > "$TEST_COUNT"
echo "service:$value:$*" >> "$TEST_LOG"
[ "${FAIL_FIRST_RESTART:-0}" = 1 ] && [ "$value" -eq 1 ] && exit 1
exit 0
EOF
chmod +x "$tmp/bin/jsonfilter" "$tmp/bin/uci" "$tmp/service"

export PATH="$tmp/bin:$PATH"
export TEST_LOG="$log"
export TEST_COUNT="$count"
export CAKE_AUTOTUNE_SCHEDULER_STATE_ROOT="$tmp/state"
export CAKE_AUTOTUNE_SERVICE="$tmp/service"
export CAKE_AUTOTUNE_SCHEDULER_SOURCE_ONLY=1
. "$root/root/usr/libexec/cake-autorate-rs/autotune-scheduler"

: > "$log"
apply_result test '{}'
restart_line="$(sed -n '/^service:1:restart$/=' "$log")"
commit_line="$(sed -n '/^uci:commit$/=' "$log")"
[ -n "$restart_line" ] && [ -n "$commit_line" ] && [ "$restart_line" -lt "$commit_line" ]
! grep -q '^uci:revert$' "$log"

: > "$log"
: > "$count"
export FAIL_FIRST_RESTART=1
if apply_result test '{}'; then
	echo 'apply_result unexpectedly succeeded after a failed staged restart' >&2
	exit 1
fi
! grep -q '^uci:commit$' "$log"
grep -q '^uci:revert$' "$log"
grep -q '^service:1:restart$' "$log"
grep -q '^service:2:restart$' "$log"

: > "$log"
: > "$count"
unset FAIL_FIRST_RESTART
export PENDING_CHANGES=1
if apply_result test '{}'; then
	echo 'apply_result unexpectedly accepted pending administrator changes' >&2
	exit 1
fi
[ ! -s "$log" ]

echo 'autotune scheduler transaction tests passed'
