#!/bin/sh
set -eu

test_dir="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
helper="$test_dir/../files/usr/libexec/cake-autorate-rs/sqm-recover"
fixtures="$test_dir/fixtures/sqm-recover"
work="${TMPDIR:-/tmp}/cake-sqm-recover.$$"

cleanup() {
	rm -rf "$work"
}
trap cleanup EXIT INT TERM

mkdir -p "$work/sys/eth0/statistics" "$work/state" "$work/recovery-lock" "$work/speedtest-lock"
: > "$work/sys/eth0/statistics/rx_bytes"
: > "$work/sys/eth0/statistics/tx_bytes"
: > "$work/state/eth0.state"

export CAKE_TEST_ROOT="$work"
export CAKE_AUTORATE_UCI="$fixtures/uci"
export CAKE_AUTORATE_TC="$fixtures/tc"
export CAKE_AUTORATE_SQM_RUN="$fixtures/sqm-run"
export CAKE_AUTORATE_SYS_CLASS_NET="$work/sys"
export CAKE_AUTORATE_SQM_STATE_DIR="$work/state"
export CAKE_AUTORATE_SQM_RECOVERY_LOCK_DIR="$work/recovery-lock"
export CAKE_AUTORATE_SPEEDTEST_LOCK_DIR="$work/speedtest-lock"

"$helper" wanb_sqm
[ -f "$work/healthy" ]
[ ! -e "$work/state/eth0.state" ]
[ "$(sed -n '1p' "$work/actions")" = "stop eth0" ]
[ "$(sed -n '2p' "$work/actions")" = "start eth0" ]

"$helper" wanb_sqm
[ "$(wc -l < "$work/actions")" -eq 2 ]

rm -f "$work/healthy" "$work/sys/ifb4eth0/statistics/tx_bytes"
mkdir -p "$work/speedtest-lock/interface-eth0.lock"
printf '%s\n' "$$" > "$work/speedtest-lock/interface-eth0.lock/pid"
if "$helper" wanb_sqm > "$work/deferred.out" 2> "$work/deferred.err"; then
	echo "recovery unexpectedly ran through an active speed-test lock" >&2
	exit 1
else
	status="$?"
fi
[ "$status" -eq 75 ]
grep -q 'recovery deferred' "$work/deferred.err"
[ "$(wc -l < "$work/actions")" -eq 2 ]

if "$helper" '../bad' > /dev/null 2>&1; then
	echo "invalid instance name unexpectedly passed" >&2
	exit 1
fi

echo "sqm-recover helper tests passed"
