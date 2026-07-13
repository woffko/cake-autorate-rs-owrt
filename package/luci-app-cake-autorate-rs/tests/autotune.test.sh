#!/bin/sh
set -eu

test_dir="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
package_dir="$(CDPATH= cd -- "$test_dir/../../cake-autorate-rs" && pwd)"
autotune="$test_dir/../root/usr/libexec/cake-autorate-rs/autotune"
fixtures="$test_dir/fixtures/autotune"
work="${TMPDIR:-/tmp}/cake-autorate-autotune-test.$$"

cleanup() {
	[ -n "${blocking_pid:-}" ] && kill "$blocking_pid" 2>/dev/null || true
	rm -rf "$work"
}
trap cleanup EXIT INT TERM

mkdir -p "$work/jobs"
: > "$work/counter"

export CAKE_AUTORATE_AUTOTUNE_DIR="$work/jobs"
export CAKE_AUTORATE_SPEEDTEST="$fixtures/speedtest"
export CAKE_AUTORATE_PINGER_PLAN="$fixtures/pinger-plan"
export CAKE_AUTORATE_JSONFILTER="$fixtures/jsonfilter"
export CAKE_AUTORATE_FPING="$fixtures/fping"
export CAKE_AUTORATE_HTTP_FETCH="$fixtures/http-fetch"
export CAKE_AUTORATE_DAEMON="$package_dir/src/target/debug/cake-autorated"
export CAKE_AUTORATE_AUTOTUNE_TEST_PREFLIGHT=1
export CAKE_AUTORATE_AUTOTUNE_TEST_LINK_KIND=cellular
export CAKE_AUTORATE_AUTOTUNE_TEST_SHAPER=1
export AUTOTUNE_MOCK_SHAPER_RESTORE_MARKER="$work/shaper-restored"
export AUTOTUNE_MOCK_COUNTER="$work/counter"
export AUTOTUNE_MOCK_PIN_LOG="$work/server-pins"

"$autotune" fullauto lo start speedtest-go > "$work/start.json"

attempt=0
while [ "$attempt" -lt 100 ]; do
	"$autotune" fullauto lo status speedtest-go > "$work/status.json"
	if grep -q '"state":"complete"' "$work/status.json"; then
		break
	fi
	attempt=$((attempt + 1))
	sleep 0.05
done

grep -q '"state":"complete"' "$work/status.json"
grep -q '"configuration_written":false' "$work/status.json"
grep -q '"download_kbps":50000' "$work/status.json"
grep -q '"download_kbps":110000' "$work/status.json"
grep -q '"enabled":true' "$work/status.json"
grep -q '"kind":"cellular"' "$work/status.json"
grep -q '"median_ms":13.000' "$work/status.json"
grep -q '"http_median_ms":0.000' "$work/status.json"
grep -q '"http_latency":{' "$work/status.json"
grep -q '"validation":{"pass":true' "$work/status.json"
grep -q '"comparison":"observed-low"' "$work/status.json"
test -f "$work/shaper-restored"
test "$(sed -n '1p' "$work/server-pins")" = automatic
test "$(sed -n '2p' "$work/server-pins")" = 17372
test "$(sed -n '3p' "$work/server-pins")" = 17372

: > "$work/counter"
export AUTOTUNE_MOCK_CORRECT=1
"$autotune" corrected lo start speedtest-go > "$work/correct-start.json"
attempt=0
while [ "$attempt" -lt 100 ]; do
	"$autotune" corrected lo status speedtest-go > "$work/correct-status.json"
	grep -q '"state":"complete"' "$work/correct-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"validation_attempts":\[{"pass":false' "$work/correct-status.json"
grep -q '},{"pass":true' "$work/correct-status.json"
grep -q '"validation":{"pass":true' "$work/correct-status.json"
unset AUTOTUNE_MOCK_CORRECT

: > "$work/counter"
export AUTOTUNE_MOCK_ROUTE_MISMATCH=1
"$autotune" routebad lo start speedtest-go > "$work/routebad-start.json"
attempt=0
while [ "$attempt" -lt 100 ]; do
	"$autotune" routebad lo status speedtest-go > "$work/routebad-status.json"
	grep -q '"state":"failed"' "$work/routebad-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"failed"' "$work/routebad-status.json"
grep -q 'route identity changed' "$work/routebad-status.json"
unset AUTOTUNE_MOCK_ROUTE_MISMATCH

export AUTOTUNE_MOCK_BLOCK=1
export AUTOTUNE_MOCK_RESTORE_MARKER="$work/restored"
"$autotune" cancelled lo start speedtest-go > "$work/cancel-start.json"

attempt=0
while [ "$attempt" -lt 100 ]; do
	"$autotune" cancelled lo status speedtest-go > "$work/cancel-status.json"
	grep -q '"phase":"throughput"' "$work/cancel-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done

"$autotune" cancelled lo cancel speedtest-go > "$work/cancel.json"
attempt=0
while [ "$attempt" -lt 100 ]; do
	"$autotune" cancelled lo status speedtest-go > "$work/cancel-status.json"
	grep -q '"state":"cancelled"' "$work/cancel-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done

grep -q '"state":"cancelled"' "$work/cancel-status.json"
test -f "$work/restored"
test ! -e "$work/jobs/cancelled/http.running"

printf '%s\n' 'autotune lifecycle tests passed'
