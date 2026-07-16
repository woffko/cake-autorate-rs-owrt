#!/bin/sh
set -eu

test_dir="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
helper="$test_dir/../root/usr/libexec/cake-autorate-rs/quality-test"
fixtures="$test_dir/fixtures/quality-test"
work="${TMPDIR:-/tmp}/cake-quality-test.$$"

cleanup() {
	for pid_file in "$work"/jobs/*/pid; do
		[ -s "$pid_file" ] || continue
		pid="$(sed -n '1p' "$pid_file")"
		kill -TERM "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
	done
	rm -rf "$work"
}
trap cleanup EXIT INT TERM

mkdir -p "$work/runtime/automatic" "$work/runtime/client" "$work/runtime/contaminated" "$work/runtime/busy" "$work/runtime/unhealthy" "$work/jobs"
baseline='{"route_active":true,"route_device":"lo","transport_probe_trusted":true,"quality_grade_baseline_ready":true,"quality_grade_baseline_samples":20,"quality_grade_baseline_required_samples":20,"quality_grade_dl_samples":0,"quality_grade_ul_samples":0,"quality_grade_required_samples":20,"quality_grade_state":"baseline_ready","rating_load_phase":"IDLE","rating_load_candidate":"IDLE","rating_load_smoothed_dl_percent":0,"rating_load_smoothed_ul_percent":0,"dl_achieved_rate_kbps":0,"ul_achieved_rate_kbps":0,"cake_dl_rate_kbps":100000,"cake_ul_rate_kbps":50000,"rating_capture_contaminated":false}'
printf '%s\n' "$baseline" > "$work/runtime/automatic/status.json"
printf '%s\n' "$baseline" > "$work/runtime/client/status.json"
printf '%s\n' "$baseline" > "$work/runtime/contaminated/status.json"
busy='{"route_active":true,"route_device":"lo","transport_probe_trusted":true,"quality_grade_baseline_ready":true,"quality_grade_baseline_samples":20,"quality_grade_baseline_required_samples":20,"quality_grade_dl_samples":0,"quality_grade_ul_samples":0,"quality_grade_required_samples":20,"quality_grade_state":"baseline_ready","rating_load_phase":"IDLE","rating_load_candidate":"IDLE","rating_load_smoothed_dl_percent":9,"rating_load_smoothed_ul_percent":0,"dl_achieved_rate_kbps":9000,"ul_achieved_rate_kbps":0,"cake_dl_rate_kbps":100000,"cake_ul_rate_kbps":50000,"rating_capture_contaminated":false}'
printf '%s\n' "$busy" > "$work/runtime/busy/status.json"
unhealthy='{"route_active":true,"route_device":"lo","sqm_runtime_managed":true,"sqm_runtime_healthy":false,"sqm_runtime_reason":"download counter is missing","transport_probe_trusted":true,"quality_grade_baseline_ready":true}'
printf '%s\n' "$unhealthy" > "$work/runtime/unhealthy/status.json"

export CAKE_AUTORATE_QUALITY_DIR="$work/jobs"
export CAKE_AUTORATE_RUNTIME_DIR="$work/runtime"
export CAKE_AUTORATE_SPEEDTEST="$fixtures/speedtest"
export CAKE_AUTORATE_JSONFILTER="$fixtures/jsonfilter"
export CAKE_AUTORATE_UCI="$fixtures/uci"
export CAKE_AUTORATE_QUALITY_TIMEOUT_S=15
export CAKE_AUTORATE_RUNTIME_LOCK_LIB="$test_dir/../../cake-autorate-rs/files/usr/libexec/cake-autorate-rs/runtime-lock"
export CAKE_AUTORATE_RUNTIME_LOCK_ROOT="$work/runtime-locks"

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
[ "$(sed -n '1p' "$work/jobs/speedtest-directions")" = "download upload" ]
[ ! -e "$work/runtime/automatic/rating-capture" ]

"$helper" client start client > "$work/client-start.json"
grep -q '"state":"running"' "$work/client-start.json"
"$helper" client status > "$work/client-status.json"
grep -q '"mode":"client"' "$work/client-status.json"
"$helper" client cancel > "$work/client-cancel.json"
grep -q '"state":"cancelled"' "$work/client-cancel.json"

CAKE_TEST_CONTAMINATED=1
export CAKE_TEST_CONTAMINATED
"$helper" contaminated start automatic speedtest-go > "$work/contaminated-start.json"
unset CAKE_TEST_CONTAMINATED
attempt=0
while [ "$attempt" -lt 40 ]; do
	"$helper" contaminated status > "$work/contaminated-status.json"
	grep -q '"state":"error"' "$work/contaminated-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.25
done
grep -q 'unexpected_upload_during_download' "$work/contaminated-status.json"
[ ! -e "$work/runtime/contaminated/rating-capture" ]

"$helper" busy start client > "$work/busy-start.json"
attempt=0
while [ "$attempt" -lt 40 ]; do
	"$helper" busy status > "$work/busy-status.json"
	grep -q '"state":"error"' "$work/busy-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.25
done
grep -q 'Background traffic stayed above the quiet limits' "$work/busy-status.json"
[ ! -e "$work/runtime/busy/rating-capture" ]

if CAKE_TEST_DISABLED=1 "$helper" disabled start client > "$work/disabled.json" 2>/dev/null; then
	echo "disabled instance unexpectedly passed preflight" >&2
	exit 1
fi
grep -q 'Autorate instance is disabled' "$work/disabled.json"

if "$helper" unhealthy start client > "$work/unhealthy.json" 2>/dev/null; then
	echo "unhealthy SQM runtime unexpectedly passed preflight" >&2
	exit 1
fi
grep -q 'Managed SQM runtime is unhealthy: download counter is missing' "$work/unhealthy.json"

echo "quality-test helper tests passed"
