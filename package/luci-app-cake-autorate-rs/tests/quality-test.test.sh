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
	[ "${CAKE_TEST_KEEP_WORK:-0}" = 1 ] || rm -rf "$work"
}
trap cleanup EXIT INT TERM

mkdir -p "$work/runtime/automatic" "$work/runtime/client" "$work/runtime/concurrent" \
	"$work/runtime/standbyauto" "$work/runtime/standbyclient" \
	"$work/runtime/contaminated" "$work/runtime/busy" "$work/runtime/unhealthy" \
	"$work/runtime/delayed" "$work/runtime/publication" "$work/runtime/cancelrace" \
	"$work/runtime/foreign" "$work/runtime/transient" "$work/runtime/postlock" \
	"$work/runtime/postlockrelease" \
	"$work/runtime/runtimeloss" "$work/jobs"
baseline='{"uplink_state":"ACTIVE","route_active":true,"route_test_ready":true,"route_device":"lo","transport_probe_trusted":true,"quality_grade_baseline_ready":true,"quality_grade_baseline_samples":20,"quality_grade_baseline_required_samples":20,"quality_grade_dl_samples":0,"quality_grade_ul_samples":0,"quality_grade_required_samples":20,"quality_grade_state":"baseline_ready","rating_load_phase":"IDLE","rating_load_candidate":"IDLE","rating_load_smoothed_dl_percent":0,"rating_load_smoothed_ul_percent":0,"dl_achieved_rate_kbps":0,"ul_achieved_rate_kbps":0,"cake_dl_rate_kbps":100000,"cake_ul_rate_kbps":50000,"rating_capture_contaminated":false}'
printf '%s\n' "$baseline" > "$work/runtime/automatic/status.json"
printf '%s\n' "$baseline" > "$work/runtime/client/status.json"
printf '%s\n' "$baseline" > "$work/runtime/concurrent/status.json"
printf '%s\n' "$baseline" > "$work/runtime/contaminated/status.json"
printf '%s\n' "$baseline" > "$work/runtime/delayed/status.json"
printf '%s\n' "$baseline" > "$work/runtime/publication/status.json"
printf '%s\n' "$baseline" > "$work/runtime/cancelrace/status.json"
printf '%s\n' "$baseline" > "$work/runtime/foreign/status.json"
printf '%s\n' "$baseline" > "$work/runtime/postlock/status.json"
printf '%s\n' "$baseline" > "$work/runtime/postlockrelease/status.json"
printf '%s\n' "$baseline" > "$work/runtime/runtimeloss/status.json"
standby='{"uplink_state":"STANDBY","route_active":false,"route_test_ready":true,"route_device":"lo","transport_probe_trusted":true,"quality_grade_baseline_ready":true,"quality_grade_baseline_samples":20,"quality_grade_baseline_required_samples":20,"quality_grade_dl_samples":0,"quality_grade_ul_samples":0,"quality_grade_required_samples":20,"quality_grade_state":"baseline_ready","rating_load_phase":"IDLE","rating_load_candidate":"IDLE","rating_load_smoothed_dl_percent":0,"rating_load_smoothed_ul_percent":0,"dl_achieved_rate_kbps":0,"ul_achieved_rate_kbps":0,"cake_dl_rate_kbps":100000,"cake_ul_rate_kbps":50000,"rating_capture_contaminated":false}'
printf '%s\n' "$standby" > "$work/runtime/standbyauto/status.json"
printf '%s\n' "$standby" > "$work/runtime/standbyclient/status.json"
busy='{"uplink_state":"ACTIVE","route_active":true,"route_test_ready":true,"route_device":"lo","transport_probe_trusted":true,"quality_grade_baseline_ready":true,"quality_grade_baseline_samples":20,"quality_grade_baseline_required_samples":20,"quality_grade_dl_samples":0,"quality_grade_ul_samples":0,"quality_grade_required_samples":20,"quality_grade_state":"baseline_ready","rating_load_phase":"IDLE","rating_load_candidate":"IDLE","rating_load_smoothed_dl_percent":9,"rating_load_smoothed_ul_percent":0,"dl_achieved_rate_kbps":9000,"ul_achieved_rate_kbps":0,"cake_dl_rate_kbps":100000,"cake_ul_rate_kbps":50000,"rating_capture_contaminated":false}'
printf '%s\n' "$busy" > "$work/runtime/busy/status.json"
unhealthy='{"route_active":true,"route_device":"lo","sqm_runtime_managed":true,"sqm_runtime_state":"ERROR","sqm_runtime_healthy":false,"sqm_runtime_reason":"download counter is missing","transport_probe_trusted":true,"quality_grade_baseline_ready":true}'
printf '%s\n' "$unhealthy" > "$work/runtime/unhealthy/status.json"
transient='{"route_active":true,"route_device":"lo","sqm_runtime_managed":true,"sqm_runtime_state":"RECOVERING","sqm_runtime_healthy":false,"sqm_runtime_reason":"native SQM hotplug is recreating the IFB","transport_probe_trusted":true,"quality_grade_baseline_ready":false}'
printf '%s\n' "$transient" > "$work/runtime/transient/status.json"

export CAKE_AUTORATE_QUALITY_DIR="$work/jobs"
export CAKE_AUTORATE_RUNTIME_DIR="$work/runtime"
export CAKE_AUTORATE_SPEEDTEST="$fixtures/speedtest"
export CAKE_AUTORATE_JSONFILTER="$fixtures/jsonfilter"
export CAKE_AUTORATE_UCI="$fixtures/uci"
export CAKE_AUTORATE_QUALITY_SELF="$helper"
export CAKE_AUTORATE_QUALITY_TIMEOUT_S=30
export CAKE_AUTORATE_RUNTIME_LOCK_LIB="$test_dir/../../cake-autorate-rs/files/usr/libexec/cake-autorate-rs/runtime-lock"
export CAKE_AUTORATE_RUNTIME_LOCK_ROOT="$work/runtime-locks"
export CAKE_QUALITY_TEST_WORK="$work"

cat > "$work/procd-job" <<'EOF'
#!/bin/sh
[ "$1" = launch ] || exit 1
shift 4
command="$1"
shift
exec 7>>"$CAKE_QUALITY_TEST_WORK/procd-count.lock"
flock -x 7
count="$(sed -n '1p' "$CAKE_QUALITY_TEST_WORK/procd-count" 2>/dev/null || true)"
case "$count" in ''|*[!0-9]*) count=0 ;; esac
printf '%s\n' "$((count + 1))" > "$CAKE_QUALITY_TEST_WORK/procd-count"
flock -u 7
exec 7>&-
if [ -n "${CAKE_TEST_PROCD_FOREIGN_PID:-}" ]; then
	printf '{"pid":%s,"starttime":%s}\n' \
		"$CAKE_TEST_PROCD_FOREIGN_PID" "$CAKE_TEST_PROCD_FOREIGN_START"
	exit 0
fi
if [ -n "${CAKE_TEST_PROCD_EXEC_DELAY:-}" ]; then
	setsid sh -c 'delay="$1"; shift; sleep "$delay"; exec "$@"' sh \
		"$CAKE_TEST_PROCD_EXEC_DELAY" "$command" "$@" </dev/null >/dev/null 2>&1 6>&- &
else
	setsid "$command" "$@" </dev/null >/dev/null 2>&1 6>&- &
fi
pid="$!"
start="$(sed 's/^.*) //' "/proc/$pid/stat" | awk '{ print $20; exit }')"
[ -z "${CAKE_TEST_PROCD_RETURN_DELAY:-}" ] || sleep "$CAKE_TEST_PROCD_RETURN_DELAY"
printf '{"pid":%s,"starttime":%s}\n' "$pid" "$start"
EOF
chmod +x "$work/procd-job"
export CAKE_AUTORATE_PROCD_JOB="$work/procd-job"
printf '0\n' > "$work/procd-count"

before_count="$(sed -n '1p' "$work/procd-count")"
"$helper" concurrent start client > "$work/concurrent-one.json" &
concurrent_one="$!"
"$helper" concurrent start client > "$work/concurrent-two.json" &
concurrent_two="$!"
wait "$concurrent_one"
wait "$concurrent_two"
grep -q '"state":"running"' "$work/concurrent-one.json"
grep -q '"state":"running"' "$work/concurrent-two.json"
after_count="$(sed -n '1p' "$work/procd-count")"
[ "$((after_count - before_count))" -eq 1 ] || {
	echo "concurrent rating starts launched duplicate workers" >&2
	exit 1
}
"$helper" concurrent cancel client >/dev/null

# A busy cold boot may leave the isolated procd launcher visible for longer
# than the old 250 ms identity window.  The first request must tolerate a
# bounded one-second exec transition instead of killing a valid worker.
CAKE_TEST_PROCD_EXEC_DELAY=1 "$helper" delayed start client > "$work/delayed-start.json"
grep -q '"state":"running"' "$work/delayed-start.json"
"$helper" delayed cancel client >/dev/null

# The launched worker may exec before the parent is scheduled again to publish
# its authenticated PID record.  The worker-side registration budget must be
# as long as the parent-side handoff budget.
CAKE_TEST_PROCD_RETURN_DELAY=1 "$helper" publication start client > "$work/publication-start.json"
grep -q '"state":"running"' "$work/publication-start.json"
"$helper" publication cancel client >/dev/null

# Cancel serializes with startup.  It must not report success before a delayed
# start publishes its PID and then lose the cancellation marker.
CAKE_TEST_PROCD_RETURN_DELAY=1 "$helper" cancelrace start client > "$work/cancelrace-start.json" &
cancelrace_start="$!"
sleep 0.2
"$helper" cancelrace cancel client > "$work/cancelrace-cancel.json"
wait "$cancelrace_start"
grep -q '"state":"running"' "$work/cancelrace-start.json"
grep -q '"state":"cancelled"' "$work/cancelrace-cancel.json"
attempt=0
while [ "$attempt" -lt 40 ] && [ -s "$work/jobs/cancelrace/pid" ]; do
	attempt=$((attempt + 1))
	sleep 0.05
done
[ ! -s "$work/jobs/cancelrace/pid" ]

# A recycled PID with a different start time must never be killed when a
# malformed/stale launcher reply fails identity verification.
setsid sleep 30 &
foreign_pid="$!"
foreign_start="$(sed 's/^.*) //' "/proc/$foreign_pid/stat" | awk '{ print $20; exit }')"
if CAKE_TEST_PROCD_FOREIGN_PID="$foreign_pid" \
	CAKE_TEST_PROCD_FOREIGN_START="$((foreign_start + 1))" \
	"$helper" foreign start client > "$work/foreign-start.json" 2>/dev/null; then
	echo "stale launcher identity unexpectedly passed verification" >&2
	exit 1
fi
grep -q 'did not publish a verified isolated identity' "$work/foreign-start.json"
kill -0 "$foreign_pid"
kill "$foreign_pid"
wait "$foreign_pid" 2>/dev/null || true

# The per-instance lock must live only below a private, owned, non-symlink job
# root; otherwise concurrent starts could be redirected to different inodes.
mkdir "$work/unsafe-job-target"
ln -s "$work/unsafe-job-target" "$work/unsafe-jobs"
if CAKE_AUTORATE_QUALITY_DIR="$work/unsafe-jobs" \
	"$helper" automatic start client > "$work/unsafe-start.json" 2>/dev/null; then
	echo "symlink rating job root unexpectedly passed verification" >&2
	exit 1
fi
grep -q 'private rating job directory' "$work/unsafe-start.json"

# A shared /tmp peer may pre-create the job root.  The helper must verify its
# effective-UID ownership before chmod so that a foreign path (or a path swapped
# to a symlink) is rejected without changing permissions on the target.
mkdir "$work/foreign-owner-jobs" "$work/foreign-owner-bin"
cat > "$work/foreign-owner-bin/ls" <<'EOF'
#!/bin/sh
"$CAKE_TEST_SYSTEM_LS" "$@" | awk '{ $3 = 424242; print }'
EOF
cat > "$work/foreign-owner-bin/chmod" <<'EOF'
#!/bin/sh
touch "$CAKE_TEST_FOREIGN_CHMOD_MARKER"
exit 99
EOF
chmod +x "$work/foreign-owner-bin/ls" "$work/foreign-owner-bin/chmod"
if PATH="$work/foreign-owner-bin:$PATH" \
	CAKE_TEST_SYSTEM_LS="$(command -v ls)" \
	CAKE_TEST_FOREIGN_CHMOD_MARKER="$work/foreign-owner-chmod-called" \
	CAKE_AUTORATE_QUALITY_DIR="$work/foreign-owner-jobs" \
	"$helper" automatic start client > "$work/foreign-owner-start.json" 2>/dev/null; then
	echo "foreign-owned rating job root unexpectedly passed verification" >&2
	exit 1
fi
grep -q 'private rating job directory' "$work/foreign-owner-start.json"
[ ! -e "$work/foreign-owner-chmod-called" ] || {
	echo "foreign-owned rating job root was chmodded before ownership rejection" >&2
	exit 1
}

# Native WWAN/SQM hotplug is a retryable readiness transition.  Start returns
# a managed worker immediately; the worker waits without holding the interface
# lock and proceeds when the runtime status becomes healthy.
(
	sleep 1
	printf '%s\n' "$baseline" > "$work/runtime/transient/status.json"
) &
transient_writer="$!"
"$helper" transient start automatic speedtest-go > "$work/transient-start.json"
grep -q '"state":"running"' "$work/transient-start.json"
wait "$transient_writer"
attempt=0
while [ "$attempt" -lt 80 ]; do
	"$helper" transient status > "$work/transient-status.json"
	grep -q '"state":"complete"' "$work/transient-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.25
done
grep -q '"grade":"A"' "$work/transient-status.json"

# If SQM becomes unhealthy during a generated direction, reject the result and
# preserve the exact runtime diagnostic instead of reporting generic samples.
CAKE_TEST_RUNTIME_LOSS=1 "$helper" runtimeloss start automatic speedtest-go \
	> "$work/runtimeloss-start.json"
grep -q '"state":"running"' "$work/runtimeloss-start.json"
attempt=0
while [ "$attempt" -lt 60 ]; do
	"$helper" runtimeloss status > "$work/runtimeloss-status.json"
	grep -q '"state":"error"' "$work/runtimeloss-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.25
done
grep -q 'Runtime became unready during download load: Managed SQM runtime is unhealthy: CAKE disappeared during load' \
	"$work/runtimeloss-status.json"
[ ! -e "$work/runtime/runtimeloss/rating-capture" ]

printf '0\n' > "$work/jobs/speedtest-runs"
rm -f "$work/jobs/speedtest-directions"

"$helper" automatic start automatic speedtest-go > "$work/start.json"
grep -q '"state":"running"' "$work/start.json"

attempt=0
while [ "$attempt" -lt 80 ]; do
	"$helper" automatic status > "$work/status.json"
	grep -q '"state":"complete"' "$work/status.json" && break
	attempt=$((attempt + 1))
	sleep 0.25
done
grep -q '"grade":"A"' "$work/status.json"
[ "$(sed -n '1p' "$work/jobs/speedtest-runs")" = 2 ]
[ "$(sed -n '1p' "$work/jobs/speedtest-directions")" = "download upload" ]
[ ! -e "$work/runtime/automatic/rating-capture" ]

# A non-default mwan3 member remains eligible for router-originated automatic
# load because the daemon has proved its isolated forced route. Guided LAN
# capture stays blocked until client traffic is explicitly routed there.
printf '0\n' > "$work/jobs/speedtest-runs"
rm -f "$work/jobs/speedtest-directions"
"$helper" standbyauto start automatic speedtest-go > "$work/standbyauto-start.json"
grep -q '"state":"running"' "$work/standbyauto-start.json"
attempt=0
while [ "$attempt" -lt 80 ]; do
	"$helper" standbyauto status > "$work/standbyauto-status.json"
	grep -q '"state":"complete"' "$work/standbyauto-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.25
done
grep -q '"grade":"A"' "$work/standbyauto-status.json"
if "$helper" standbyclient start client > "$work/standbyclient-start.json" 2>/dev/null; then
	echo "guided client capture unexpectedly accepted a non-default standby route" >&2
	exit 1
fi
grep -q 'Guided client mode requires client traffic' "$work/standbyclient-start.json"

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

# Revalidate after taking the rating lock.  This fixture flips SQM to a hard
# error at acquisition time, modelling a hotplug reset in the preflight/lock
# gap.  Capture must never arm and the lock must be released.
cat > "$work/postlock-runtime-lock" <<'EOF'
runtime_lock_interface_paths() {
	runtime_interface_lock_record="$CAKE_QUALITY_TEST_WORK/postlock.interface"
	return 0
}
runtime_lock_acquire_global_shared() { return 0; }
runtime_lock_release_global() { printf 'global\n' >> "$CAKE_QUALITY_TEST_WORK/postlock-released"; }
runtime_lock_acquire_interface() {
	cat > "$CAKE_AUTORATE_RUNTIME_DIR/postlock/status.json" <<'STATUS'
{"route_active":true,"route_device":"lo","sqm_runtime_managed":true,"sqm_runtime_state":"ERROR","sqm_runtime_healthy":false,"sqm_runtime_reason":"CAKE disappeared after preflight","transport_probe_trusted":true,"quality_grade_baseline_ready":true}
STATUS
	return 0
}
runtime_lock_release_interface() { printf 'interface\n' >> "$CAKE_QUALITY_TEST_WORK/postlock-released"; }
EOF
CAKE_AUTORATE_RUNTIME_LOCK_LIB="$work/postlock-runtime-lock" \
	"$helper" postlock start client > "$work/postlock-start.json"
grep -q '"state":"running"' "$work/postlock-start.json"
attempt=0
while [ "$attempt" -lt 40 ]; do
	"$helper" postlock status > "$work/postlock-status.json"
	grep -q '"state":"error"' "$work/postlock-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.25
done
grep -q 'CAKE disappeared after preflight' "$work/postlock-status.json"
[ ! -e "$work/runtime/postlock/rating-capture" ]
grep -q '^interface$' "$work/postlock-released"
grep -q '^global$' "$work/postlock-released"

# If interface unlock verification fails, still release the global shared lock
# and fail closed instead of waiting while blocking SQM recovery.
cat > "$work/postlockrelease-runtime-lock" <<'EOF'
runtime_lock_interface_paths() {
	runtime_interface_lock_record="$CAKE_QUALITY_TEST_WORK/postlockrelease.interface"
	return 0
}
runtime_lock_acquire_global_shared() { return 0; }
runtime_lock_release_global() { printf 'global\n' >> "$CAKE_QUALITY_TEST_WORK/postlockrelease-released"; }
runtime_lock_acquire_interface() {
	cat > "$CAKE_AUTORATE_RUNTIME_DIR/postlockrelease/status.json" <<'STATUS'
{"route_active":true,"route_device":"lo","sqm_runtime_managed":true,"sqm_runtime_state":"RECOVERING","sqm_runtime_healthy":false,"sqm_runtime_reason":"CAKE changed while locking","transport_probe_trusted":true,"quality_grade_baseline_ready":true}
STATUS
	return 0
}
runtime_lock_release_interface() { return 1; }
EOF
CAKE_AUTORATE_RUNTIME_LOCK_LIB="$work/postlockrelease-runtime-lock" \
	"$helper" postlockrelease start client > "$work/postlockrelease-start.json"
grep -q '"state":"running"' "$work/postlockrelease-start.json"
attempt=0
while [ "$attempt" -lt 40 ]; do
	"$helper" postlockrelease status > "$work/postlockrelease-status.json"
	grep -q '"state":"error"' "$work/postlockrelease-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.25
done
grep -q 'Unable to release the rating interface lock.*CAKE changed while locking' \
	"$work/postlockrelease-status.json"
grep -q '^global$' "$work/postlockrelease-released"

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

if grep -q "stat -c" "$helper"; then
	echo "quality-test depends on the optional GNU/BusyBox stat applet" >&2
	exit 1
fi

echo "quality-test helper tests passed"
