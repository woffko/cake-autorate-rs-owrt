#!/bin/sh
set -eu

test_dir="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
helper="$test_dir/../root/usr/libexec/cake-autorate-rs/quality-test"
fixtures="$test_dir/fixtures/quality-test"
work="${TMPDIR:-/tmp}/cake-quality-test.$$"

cleanup() {
	if [ -s "$work/jobs/client/pid" ]; then
		pid="$(sed -n '1p' "$work/jobs/client/pid")"
		kill -TERM "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
	fi
	rm -rf "$work"
}
trap cleanup EXIT INT TERM

mkdir -p "$work/runtime/automatic" "$work/runtime/client" "$work/jobs"
baseline='{"route_active":true,"route_device":"lo","transport_probe_trusted":true,"quality_grade_baseline_ready":true,"quality_grade_baseline_samples":20,"quality_grade_baseline_required_samples":20,"quality_grade_dl_samples":0,"quality_grade_ul_samples":0,"quality_grade_required_samples":20,"quality_grade_state":"baseline_ready","rating_load_phase":"IDLE","rating_load_candidate":"IDLE","rating_load_smoothed_dl_percent":0,"rating_load_smoothed_ul_percent":0}'
printf '%s\n' "$baseline" > "$work/runtime/automatic/status.json"
printf '%s\n' "$baseline" > "$work/runtime/client/status.json"

export CAKE_AUTORATE_QUALITY_DIR="$work/jobs"
export CAKE_AUTORATE_RUNTIME_DIR="$work/runtime"
export CAKE_AUTORATE_SPEEDTEST="$fixtures/speedtest"
export CAKE_AUTORATE_JSONFILTER="$fixtures/jsonfilter"
export CAKE_AUTORATE_UCI="$fixtures/uci"
export CAKE_AUTORATE_QUALITY_TIMEOUT_S=15

"$helper" automatic start automatic speedtest-go > "$work/start.json"
grep -q '"state":"running"' "$work/start.json"

attempt=0
while [ "$attempt" -lt 40 ]; do
	"$helper" automatic status > "$work/status.json"
	grep -q '"state":"complete"' "$work/status.json" && break
	attempt=$((attempt + 1))
	sleep 0.25
done
grep -q '"grade":"A"' "$work/status.json"
[ "$(sed -n '1p' "$work/jobs/speedtest-runs")" = 2 ]
[ ! -e "$work/runtime/automatic/rating-capture" ]

"$helper" client start client > "$work/client-start.json"
grep -q '"state":"running"' "$work/client-start.json"
"$helper" client status > "$work/client-status.json"
grep -q '"mode":"client"' "$work/client-status.json"
"$helper" client cancel > "$work/client-cancel.json"
grep -q '"state":"cancelled"' "$work/client-cancel.json"

if CAKE_TEST_DISABLED=1 "$helper" disabled start client > "$work/disabled.json" 2>/dev/null; then
	echo "disabled instance unexpectedly passed preflight" >&2
	exit 1
fi
grep -q 'Autorate instance is disabled' "$work/disabled.json"

echo "quality-test helper tests passed"
