#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
script="$base/root/usr/libexec/cake-autorate-rs/speedtest"

if [ "${1:-}" = job-start-harness ]; then
	harness_work="$2"
	label="$3"
	export CAKE_AUTORATE_RUNTIME_LOCK_LIB="$base/../cake-autorate-rs/files/usr/libexec/cake-autorate-rs/runtime-lock"
	export CAKE_AUTORATE_RUNTIME_LOCK_ROOT="$harness_work/runtime-locks"
	export CAKE_AUTORATE_SPEEDTEST_JOB_DIR="$harness_work/jobs"
	export CAKE_AUTORATE_SPEEDTEST_SELF="$harness_work/bin/job-worker"
	: > "$harness_work/caller-$label.ready"
	while [ ! -e "$harness_work/callers.go" ]; do sleep 0.01; done
	set -- wan eth0 '' '' '' '' '' ''
	CAKE_AUTORATE_SPEEDTEST_SOURCE_ONLY=1 . "$script"
	section=wan
	target_if_override=eth0
	preferred_backend=speedtest-go
	route_mode_override=""
	mwan3_member_override=""
	speedtest_job_start
	exit $?
fi

if [ "${1:-}" = recovery-worker ]; then
	harness_work="$2"
	autorate_pid="$3"
	recovery_behavior="${4:-crash}"
	export CAKE_AUTORATE_RUNTIME_LOCK_LIB="$base/../cake-autorate-rs/files/usr/libexec/cake-autorate-rs/runtime-lock"
	export CAKE_AUTORATE_RUNTIME_LOCK_ROOT="$harness_work/runtime-locks"
	export CAKE_AUTORATE_SPEEDTEST_JOB_DIR="$harness_work/jobs"
	export CAKE_AUTORATE_SPEEDTEST_SELF="$script"
	export CAKE_AUTORATE_SQM_RUN="$harness_work/bin/sqm-run"
	export CAKE_AUTORATE_SQM_RECOVER="$harness_work/bin/sqm-recover"
	export CAKE_AUTORATE_SQM_CONFIG_FILE="$harness_work/config/sqm"
	export SQM_STATE_DIR="$harness_work/sqm-state"
	export CAKE_TEST_AUTORATE_PID="$autorate_pid"
	set -- wan eth0 '' '' '' '' '' ''
	CAKE_AUTORATE_SPEEDTEST_SOURCE_ONLY=1 . "$script"
	section=wan
	target_if=eth0
	target_if_override=eth0
	mode=run
	job_key_override=""
	warning=""
	calibration_autorate_was_running=0
	calibration_autorate_pid=""
	calibration_autorate_start=""
	calibration_sqm_was_running=0
	calibration_shaper_bypassed=false
	calibration_autorate_paused=false
	calibration_sqm_paused=false
	interface_lock_dir=""
	interface_lock_shared=0
	standalone_recovery_armed=0
	standalone_recovery_journal=""
	standalone_recovery_phase=""
	standalone_recovery_supervisor_pid=""
	standalone_recovery_supervisor_start=""
	autorate_instance_pid() { printf '%s\n' "$CAKE_TEST_AUTORATE_PID"; }
	speedtest_unshaped_enabled() { return 0; }
	acquire_interface_lock
	if [ "$recovery_behavior" = unmanaged ]; then
		if prepare_speedtest_calibration; then
			echo "unmanaged SQM unexpectedly entered unshaped calibration" >&2
			exit 1
		fi
		[ -f "$SQM_STATE_DIR/eth0.state" ] || exit 1
		if grep -qx stop "$harness_work/sqm-actions" 2>/dev/null; then exit 1; fi
		process_is_stopped "$autorate_pid" && exit 1
		restore_speedtest_calibration
		release_interface_lock
		exit 0
	fi
	prepare_speedtest_calibration
	: > "$harness_work/calibration.prepared"
		if [ "$recovery_behavior" = restore-fail ]; then
		if restore_speedtest_calibration; then
			echo "failed immutable SQM recovery was accepted" >&2
			exit 1
		fi
		process_is_stopped "$autorate_pid" || {
			echo "autorate resumed before SQM recovery succeeded" >&2
			exit 1
		}
		if grep -qx start "$harness_work/sqm-actions" 2>/dev/null; then
			echo "direct SQM start bypassed immutable recovery validation" >&2
			exit 1
		fi
		export CAKE_TEST_SQM_RECOVER_FAIL=0
		restore_speedtest_calibration
		release_interface_lock
			exit 0
		fi
		if [ "$recovery_behavior" = snapshot-drift ]; then
			printf '%s\n' "sqm.cake_wan.qdisc='fq_codel'" > "$harness_work/sqm-config-drift"
			if ! restore_speedtest_calibration; then
				echo "immutable SQM snapshot did not survive live UCI drift" >&2
				exit 1
			fi
			if process_is_stopped "$autorate_pid"; then
				echo "autorate remained stopped after exact snapshot restore" >&2
				exit 1
			fi
			rm -f "$harness_work/sqm-config-drift"
			release_interface_lock
			exit 0
		fi
	if [ "$recovery_behavior" = normal ]; then
		while [ ! -e "$harness_work/calibration.release" ]; do sleep 0.05; done
		restore_speedtest_calibration
		release_interface_lock
		exit 0
	fi
	while :; do sleep 0.05; done
fi

if [ "${1:-}" = stop-failure-worker ]; then
	harness_work="$2"
	autorate_pid="$3"
	export CAKE_AUTORATE_RUNTIME_LOCK_LIB="$base/../cake-autorate-rs/files/usr/libexec/cake-autorate-rs/runtime-lock"
	export CAKE_AUTORATE_RUNTIME_LOCK_ROOT="$harness_work/runtime-locks-failure"
	export CAKE_AUTORATE_SPEEDTEST_JOB_DIR="$harness_work/jobs-failure"
	export CAKE_AUTORATE_SPEEDTEST_SELF="$script"
	export CAKE_AUTORATE_SQM_RUN="$harness_work/bin/sqm-run"
	export CAKE_AUTORATE_SQM_RECOVER="$harness_work/bin/sqm-recover"
	export CAKE_AUTORATE_SQM_CONFIG_FILE="$harness_work/config/sqm"
	export SQM_STATE_DIR="$harness_work/sqm-state"
	export CAKE_TEST_AUTORATE_PID="$autorate_pid"
	set -- wan eth0 '' '' '' '' '' ''
	CAKE_AUTORATE_SPEEDTEST_SOURCE_ONLY=1 . "$script"
	section=wan
	target_if=eth0
	target_if_override=eth0
	mode=run
	job_key_override=""
	warning=""
	calibration_autorate_was_running=0
	calibration_autorate_pid=""
	calibration_autorate_start=""
	calibration_sqm_was_running=0
	calibration_shaper_bypassed=false
	calibration_autorate_paused=false
	calibration_sqm_paused=false
	interface_lock_dir=""
	interface_lock_shared=0
	standalone_recovery_armed=0
	standalone_recovery_journal=""
	standalone_recovery_phase=""
	standalone_recovery_supervisor_pid=""
	standalone_recovery_supervisor_start=""
	autorate_instance_pid() { printf '%s\n' "$CAKE_TEST_AUTORATE_PID"; }
	speedtest_unshaped_enabled() { return 0; }
	kill() {
		if [ "${1:-}" = -STOP ]; then return 1; fi
		command kill "$@"
	}
	acquire_interface_lock
	if prepare_speedtest_calibration; then
		echo "prepare unexpectedly accepted an unverifiable autorate STOP" >&2
		exit 1
	fi
	[ -f "$SQM_STATE_DIR/eth0.state" ] || {
		echo "SQM was stopped after autorate STOP failed" >&2
		exit 1
	}
	if grep -qx stop "$harness_work/sqm-actions" 2>/dev/null; then
		echo "SQM stop command ran after autorate STOP failed" >&2
		exit 1
	fi
	restore_speedtest_calibration
	release_interface_lock
	exit 0
fi

work="${TMPDIR:-/tmp}/cake-speedtest-routing-test.$$"
mkdir -p "$work/bin"
job_worker_pid=""
recovery_worker_pid=""
autorate_pid=""
caller_one_pid=""
caller_two_pid=""
supervisor_pid=""
foreign_holder_pid=""
cleanup_test() {
	[ -z "$caller_one_pid" ] || kill -KILL "$caller_one_pid" 2>/dev/null || true
	[ -z "$caller_two_pid" ] || kill -KILL "$caller_two_pid" 2>/dev/null || true
	[ -z "$job_worker_pid" ] || kill -KILL "$job_worker_pid" 2>/dev/null || true
	[ -z "$recovery_worker_pid" ] || kill -KILL "$recovery_worker_pid" 2>/dev/null || true
	[ -z "$supervisor_pid" ] || kill -CONT "$supervisor_pid" 2>/dev/null || true
	[ -z "$supervisor_pid" ] || kill -KILL "$supervisor_pid" 2>/dev/null || true
	[ -z "$foreign_holder_pid" ] || kill -KILL "$foreign_holder_pid" 2>/dev/null || true
	[ -z "$autorate_pid" ] || kill -CONT "$autorate_pid" 2>/dev/null || true
	[ -z "$autorate_pid" ] || kill "$autorate_pid" 2>/dev/null || true
	[ -z "$autorate_pid" ] || wait "$autorate_pid" 2>/dev/null || true
	rm -rf "$work"
}
trap cleanup_test EXIT INT TERM

fail_test() {
	printf '%s\n' "$*" >&2
	exit 1
}

wait_for_file() {
	file="$1"
	pid="$2"
	attempt=0
	while [ ! -e "$file" ]; do
		kill -0 "$pid" 2>/dev/null || fail_test "process $pid exited before creating $file"
		attempt=$((attempt + 1))
		[ "$attempt" -lt 200 ] || fail_test "timed out waiting for $file"
		sleep 0.02
	done
}

wait_for_process_state_test() {
	pid="$1"
	wanted="$2"
	attempt=0
	while [ "$attempt" -lt 200 ]; do
		state="$(sed -n 's/^State:[[:space:]]*\([A-Za-z]\).*$/\1/p' "/proc/$pid/status" 2>/dev/null || true)"
		case "$wanted:$state" in
			stopped:T|stopped:t) return 0 ;;
			running:T|running:t) ;;
			running:?) return 0 ;;
		esac
		attempt=$((attempt + 1))
		sleep 0.02
	done
	return 1
}

wait_for_process_gone() {
	pid="$1"
	attempt=0
	while kill -0 "$pid" 2>/dev/null; do
		attempt=$((attempt + 1))
		[ "$attempt" -lt 200 ] || return 1
		sleep 0.02
	done
}

first_recovery_journal() {
	for candidate in "$work/jobs"/recovery-*.journal; do
		[ -e "$candidate" ] || continue
		printf '%s\n' "$candidate"
		return 0
	done
	return 1
}

wait_for_recovery_cleanup() {
	attempt=0
	while :; do
		if ! first_recovery_journal >/dev/null 2>&1 &&
		   [ ! -e "$work/runtime-locks/interface-eth0.lock" ] &&
		   ! find "$work/runtime-locks" -maxdepth 1 -name 'sqm-snapshot.*' -print 2>/dev/null | grep -q .; then
			return 0
		fi
		attempt=$((attempt + 1))
		[ "$attempt" -lt 300 ] || return 1
		sleep 0.02
	done
}

export CAKE_AUTORATE_RUNTIME_LOCK_LIB="$base/../cake-autorate-rs/files/usr/libexec/cake-autorate-rs/runtime-lock"
export CAKE_AUTORATE_RUNTIME_LOCK_ROOT="$work/runtime-locks"

set -- test eth0 '' '' '' '' ''
CAKE_AUTORATE_SPEEDTEST_SOURCE_ONLY=1 . "$script"

grep -qx 'trap cleanup EXIT' "$script" || fail_test "speedtest EXIT cleanup trap is missing"
grep -qx "trap 'exit 129' HUP" "$script" || fail_test "speedtest HUP handler does not preserve cleanup"
grep -qx "trap 'exit 130' INT" "$script" || fail_test "speedtest INT handler does not preserve cleanup"
grep -qx "trap 'exit 143' TERM" "$script" || fail_test "speedtest TERM handler does not preserve cleanup"
if grep -q '^trap cleanup EXIT INT TERM' "$script"; then
	fail_test "speedtest signal trap can continue execution after cleanup"
fi
(
	route_mode=mwan3
	speedtest_route_pin_active=0
	speedtest_route_proof_error=""
	if run_speedtest_go_command /bin/true; then exit 1; fi
	[ "$speedtest_route_proof_error" = "the isolated speedtest-go route pin was not initialized by its parent" ]
) || fail_test "an uninitialized child speedtest-go route was allowed to run"

cat > "$work/bin/usleep-fixture" <<'EOF'
#!/bin/sh
printf '%s\n' "$1" >> "$CAKE_TEST_BRIEF_SLEEP_LOG"
EOF
chmod +x "$work/bin/usleep-fixture"
export CAKE_TEST_BRIEF_SLEEP_LOG="$work/brief-sleep.log"
(
	speedtest_brief_sleep_mode=""
	speedtest_usleep_bin=""
	sleep() {
		case "$1" in 0.*) return 1 ;; esac
		return 0
	}
	command_path() { printf '%s\n' "$work/bin/usleep-fixture"; }
	brief_sleep 0.05
	brief_sleep 0.10
	[ "$speedtest_brief_sleep_mode" = usleep ]
) || fail_test "brief sleep did not fall back to BusyBox usleep"
[ "$(sed -n '1p' "$work/brief-sleep.log")" = 50000 ] &&
	[ "$(sed -n '2p' "$work/brief-sleep.log")" = 100000 ] ||
	fail_test "brief sleep passed incorrect microsecond delays"
(
	speedtest_brief_sleep_mode=""
	speedtest_usleep_bin=""
	sleep() {
		case "$1" in
			0.*) return 1 ;;
			1) : > "$work/whole-sleep-used"; return 0 ;;
		esac
		return 1
	}
	command_path() { return 0; }
	brief_sleep 0.05
	[ "$speedtest_brief_sleep_mode" = whole ]
) || fail_test "brief sleep did not fall back to a non-spinning whole second"
[ -f "$work/whole-sleep-used" ] || fail_test "whole-second brief-sleep fallback was not invoked"

valid_ipv4 192.0.2.2
for invalid_ipv4 in '' 1.2.3.999 1.2.3.4.5 1.2.3,4 '2001:db8::1' ' 192.0.2.2' '192.0.2.2
198.51.100.2'; do
	if valid_ipv4 "$invalid_ipv4"; then
		fail_test "malformed IPv4 was accepted: $invalid_ipv4"
	fi
done

cat > "$work/bin/ip-fetch" <<'EOF'
#!/bin/sh
printf '%s\n' "$CAKE_TEST_EXTERNAL_RESPONSE"
EOF
chmod +x "$work/bin/ip-fetch"
(
	fallback_fetch="$work/bin/ip-fetch"
	curl_fetch=""
	route_mode=main
	timeout_s=5
	external_ip_url=https://example.invalid/ip
	route_exec() { "$@"; }
	export CAKE_TEST_EXTERNAL_RESPONSE=198.51.100.2
	[ "$(fetch_external_ip)" = 198.51.100.2 ]
	for malformed_response in '' 1.2.3.999 1.2.3.4.5 1.2.3,4 '2001:db8::1' '192.0.2.2
198.51.100.2'; do
		export CAKE_TEST_EXTERNAL_RESPONSE="$malformed_response"
		if fetch_external_ip >/dev/null 2>&1; then exit 1; fi
	done
) || fail_test "external-IP fetch accepted malformed output"

# Route attestation is deliberately read-only.  This unit harness turns every
# calibration/mutation entry point into a hard failure and verifies that the
# bounded status payload is built solely from route inspection + one external
# IP lookup.
(
	target_if_override=eth0
	target_if=eth0
	route_mode=main
	mwan3_member=""
	acquire_interface_lock() { exit 91; }
	prepare_speedtest_calibration() { exit 92; }
	restore_speedtest_calibration() { exit 93; }
	run_preferred_backend() { exit 94; }
	inspect_selected_route() {
		route_device=eth0
		route_source_ip=192.0.2.2
		route_fwmark=""
		route_table=main
		route_identity='main||eth0|192.0.2.2||main'
		route_active=true
		route_member_status=online
		return 0
	}
	fetch_external_ip() { printf '%s\n' 198.51.100.2; }
	route_status_json
) > "$work/route-status.json" || fail_test "read-only route status entered a calibration or mutation path"
node -e '
const fs = require("fs");
const value = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
if (value.state !== "ready" || value.schema_version !== 1)
  throw new Error("route status schema/state mismatch");
if (value.route_identity !== "main||eth0|192.0.2.2||main" || value.external_ip !== "198.51.100.2")
  throw new Error("route status identity mismatch");
if (value.route_interface !== "eth0" || value.source_ip !== "192.0.2.2")
  throw new Error("route status device/source mismatch");
' "$work/route-status.json"

if (
	target_if_override=eth0
	target_if=eth0
	route_mode=main
	mwan3_member=""
	inspect_selected_route() {
		route_device=eth0
		route_source_ip=1.2.3.999
		route_fwmark=""
		route_table=main
		route_identity='main||eth0|1.2.3.999||main'
		route_active=true
		route_member_status=online
	}
	fetch_external_ip() { printf '%s\n' 198.51.100.2; }
	route_status_json >/dev/null
); then
	fail_test "route status accepted malformed source IPv4"
fi

safe_route_name wanb
if safe_route_name 'wanb;reboot'; then
	echo "unsafe mwan3 member was accepted" >&2
	exit 1
fi

cat > "$work/bin/mwan3" <<'EOF'
#!/bin/sh
printf '%s\n' "$@" > "$CAKE_TEST_ARGV"
EOF
cat > "$work/bin/ip" <<'EOF'
#!/bin/sh
cat <<'RULES'
0: from all lookup local
1001: from all iif pppoe-wan lookup 1
1002: from all iif eth0 lookup 2
2001: from all fwmark 0x100/0x3f00 lookup 1
2002: from all fwmark 0x200/0x3f00 lookup 2
RULES
EOF
chmod +x "$work/bin/mwan3" "$work/bin/ip"

CAKE_TEST_ARGV="$work/argv"
export CAKE_TEST_ARGV
PATH="$work/bin:$PATH"
export PATH
route_mode=mwan3
mwan3_member=wanb
route_exec probe-command 'argument with spaces'

expected="use
wanb
exec
probe-command
argument with spaces"
actual="$(cat "$work/argv")"
[ "$actual" = "$expected" ] || {
	echo "routed command argv changed" >&2
	printf 'expected:\n%s\nactual:\n%s\n' "$expected" "$actual" >&2
	exit 1
}

[ "$(route_rule_for_device eth0)" = '0x200|2' ] || {
	echo "failed to resolve mwan3 fwmark/table for eth0" >&2
	exit 1
}

speedtest_job_dir="$work/jobs"
mkdir -p "$speedtest_job_dir"
runtime_lock_acquire_global_shared
runtime_lock_acquire_interface eth0 autotune "$work/recovery.journal" testtoken
CAKE_TEST_SPEEDTEST_SCRIPT="$script" sh -c '
	set -eu
	set -- test eth0 "" "" "" "" "" ""
	CAKE_AUTORATE_SPEEDTEST_SOURCE_ONLY=1 . "$CAKE_TEST_SPEEDTEST_SCRIPT"
	target_if=eth0
	interface_lock_dir=""
	interface_lock_shared=0
	acquire_interface_lock
	[ "$interface_lock_shared" = 1 ] || exit 1
	release_interface_lock
' || fail_test "Full Auto-Tune child did not borrow its parent interface lock"
[ -f "$CAKE_AUTORATE_RUNTIME_LOCK_ROOT/interface-eth0.lock" ] || {
	echo "borrowed interface lock was incorrectly released by child speed test" >&2
	exit 1
}
runtime_lock_release_interface
runtime_lock_release_global

cat > "$work/bin/speedtest-go" <<'EOF'
#!/bin/sh
printf '%s\n' "$@" > "$CAKE_TEST_SPEEDTEST_ARGV"
printf '%s\n' '{"dl_speed":100000000,"ul_speed":80000000,"server":{"id":"1","name":"test","sponsor":"test"}}'
EOF
chmod +x "$work/bin/speedtest-go"
route_exec() {
	"$@"
}
speedtest_go_bin="$work/bin/speedtest-go"
jsonfilter_bin="$base/tests/fixtures/quality-test/jsonfilter"
speedtest_go_server_id=1
upload_bytes=4000000
bind_interface_enabled=0
target_if=""
route_if=""
route_device=eth0
route_source_ip=""
tmp_response="$work/speedtest.json"
warning=""
CAKE_TEST_SPEEDTEST_ARGV="$work/speedtest-argv"
export CAKE_TEST_SPEEDTEST_ARGV
run_speedtest_go_command() { "$@"; }
interface_byte_counter() { printf '%s\n' 1000; }
verify_speedtest_go_route_bytes() {
	speedtest_route_traffic_proved=true
	speedtest_route_rx_delta=200000000
	speedtest_route_tx_delta=160000000
	speedtest_route_rx_required=10000000
	speedtest_route_tx_required=8000000
	return 0
}

direction_override=download
run_speedtest_go_once 1
grep -qx -- '--no-upload' "$work/speedtest-argv"
if grep -qx -- '--no-download' "$work/speedtest-argv"; then
	echo "download-only speedtest incorrectly disabled download" >&2
	exit 1
fi

direction_override=upload
run_speedtest_go_once 1
grep -qx -- '--no-download' "$work/speedtest-argv"
if grep -qx -- '--no-upload' "$work/speedtest-argv"; then
	echo "upload-only speedtest incorrectly disabled upload" >&2
	exit 1
fi

download_kbps=900000
upload_kbps=700000
download_size=100
upload_size=50
download_elapsed=10.0
upload_elapsed=10.0
source=speedtest-go
backend=speedtest-go
test_server_id=1
test_server_name=test
test_server_sponsor=test
bind_mode=source-ip
target_if=eth0
route_device=eth0
route_source_ip=10.0.100.101
route_fwmark=0x200
route_table=2
external_ip_before=46.1.1.1
external_ip_after=46.1.1.1
calibration_shaper_bypassed=false
calibration_autorate_paused=false
calibration_sqm_paused=false
warning=
direction_override=both
speedtest_route_traffic_proved=true
speedtest_route_rx_delta=200000000
speedtest_route_tx_delta=160000000
speedtest_route_rx_required=10000000
speedtest_route_tx_required=8000000
emit_result > "$work/result.json"

node -e '
const fs = require("fs");
const value = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
if (value.download_kbps !== 900000 || value.upload_kbps !== 700000)
  throw new Error("DL/UL fields were crossed");
if (value.test_direction !== "both")
  throw new Error("test direction missing from speedtest result");
if (value.route_mode !== "mwan3" || value.mwan3_member !== "wanb" || value.route_fwmark !== "0x200" || value.route_table !== "2")
  throw new Error("route metadata missing from speedtest result");
if (!value.route_traffic_proof || value.route_traffic_proof.passed !== true ||
    value.route_traffic_proof.rx_bytes !== 200000000 || value.route_traffic_proof.tx_bytes !== 160000000)
  throw new Error("selected-interface byte proof missing from speedtest result");
' "$work/result.json"

[ "$(minimum_proof_bytes 100000 10)" -eq 12500000 ] ||
	fail_test "speedtest route byte proof calculated the wrong proportional floor"
[ "$(minimum_proof_bytes 1 1)" -eq 1048576 ] ||
	fail_test "speedtest route byte proof did not retain its one-MiB minimum"
grep -q 'meta skuid.*meta mark set' "$script" ||
	fail_test "static speedtest-go route pin is not uid-scoped in nft output"
if grep -q '^[[:space:]]*local .*route_mark_mask' "$script"; then
	fail_test "mwan3 FWMARK mask is shadowed inside route-pin acquisition"
fi
grep -q 'speedtest_route_proof_error="the isolated speedtest-go route pin was not initialized by its parent"' "$script" ||
	fail_test "speedtest-go commands can still acquire mwan3 routing from a subshell"
grep -q 'USERID:=cake-speedtest:cake-speedtest' "$base/Makefile" ||
	fail_test "package-owned speedtest route user is missing"

# Two simultaneous LuCI job-start RPCs must publish and resume exactly one
# verified worker. The second caller observes the same immutable PID/start
# record instead of launching a duplicate heavy test.
cat > "$work/bin/job-worker" <<'EOF'
#!/bin/sh
if [ "${CAKE_AUTORATE_SPEEDTEST_JOB_START_STOPPED:-0}" = 1 ]; then
	kill -STOP "$$" || exit 1
	unset CAKE_AUTORATE_SPEEDTEST_JOB_START_STOPPED
fi
exec 7>>"$CAKE_TEST_JOB_WORK/worker-count.guard" || exit 1
flock -x 7 || exit 1
count="$(sed -n '1p' "$CAKE_TEST_JOB_WORK/worker-count" 2>/dev/null || true)"
case "$count" in ''|*[!0-9]*) count=0 ;; esac
count=$((count + 1))
printf '%s\n' "$count" > "$CAKE_TEST_JOB_WORK/worker-count"
flock -u 7
exec 7>&-
: > "$CAKE_TEST_JOB_WORK/worker-started"
while [ ! -e "$CAKE_TEST_JOB_WORK/worker-release" ]; do sleep 0.02; done
EOF
chmod +x "$work/bin/job-worker"
export CAKE_TEST_JOB_WORK="$work"
: > "$work/worker-count"
"$0" job-start-harness "$work" one > "$work/caller-one.out" 2> "$work/caller-one.err" &
caller_one_pid="$!"
"$0" job-start-harness "$work" two > "$work/caller-two.out" 2> "$work/caller-two.err" &
caller_two_pid="$!"
wait_for_file "$work/caller-one.ready" "$caller_one_pid"
wait_for_file "$work/caller-two.ready" "$caller_two_pid"
: > "$work/callers.go"
wait "$caller_one_pid" || fail_test "first concurrent job-start caller failed"
caller_one_pid=""
wait "$caller_two_pid" || fail_test "second concurrent job-start caller failed"
caller_two_pid=""
grep -q '"state":"running"' "$work/caller-one.out"
grep -q '"state":"running"' "$work/caller-two.out"
wait_for_file "$work/worker-started" "$(sed -n 's/^pid=//p' "$work/jobs/wan.pid")"
[ "$(sed -n '1p' "$work/worker-count")" = 1 ] ||
	fail_test "concurrent job-start launched more than one worker"
job_worker_pid="$(sed -n 's/^pid=//p' "$work/jobs/wan.pid")"
job_worker_start="$(sed -n 's/^start=//p' "$work/jobs/wan.pid")"
[ -n "$job_worker_pid" ] && [ -n "$job_worker_start" ] ||
	fail_test "verified worker identity was not published"
[ "$(speedtest_process_starttime "$job_worker_pid")" = "$job_worker_start" ] ||
	fail_test "published job PID/start does not identify the worker"
: > "$work/worker-release"
wait_for_process_gone "$job_worker_pid" || fail_test "job worker did not exit"
job_worker_pid=""
rm -f "$work/jobs/wan.pid"

# Recovery fixtures model the SQM state marker and a long-lived autorate
# process. The action log proves whether an unsafe STOP/start happened.
mkdir -p "$work/sqm-state" "$work/config"
cat > "$work/config/sqm" <<'EOF'
config queue 'cake_wan'
	option _cake_autorate_managed 'wan'
	option enabled '1'
	option interface 'eth0'
	option download '100000'
	option upload '80000'
	option qdisc 'cake'
EOF
chmod 600 "$work/config/sqm"
cat > "$work/bin/sqm-run" <<'EOF'
#!/bin/sh
printf '%s\n' "$1" >> "$CAKE_TEST_JOB_WORK/sqm-actions"
case "$1" in
	stop) rm -f "$SQM_STATE_DIR/$2.state" ;;
	start) mkdir -p "$SQM_STATE_DIR"; : > "$SQM_STATE_DIR/$2.state" ;;
	*) exit 1 ;;
esac
EOF
chmod +x "$work/bin/sqm-run"
cat > "$work/bin/uci" <<'EOF'
#!/bin/sh
config_dir=""
if [ "${2:-}" = -c ]; then
	config_dir="$3"
	key="$5"
else
	key="$3"
fi
case "$key" in
	sqm.cake_wan)
		if [ -n "$config_dir" ]; then
			[ -f "$config_dir/sqm" ] || exit 1
			grep -q "config queue 'cake_wan'" "$config_dir/sqm" || exit 1
			grep -q "option qdisc 'cake'" "$config_dir/sqm" || exit 1
		fi
		printf '%s\n' \
			"sqm.cake_wan=queue" \
			"sqm.cake_wan._cake_autorate_managed='wan'" \
			"sqm.cake_wan.enabled='1'" \
			"sqm.cake_wan.interface='eth0'" \
			"sqm.cake_wan.qdisc='cake'"
		[ -n "$config_dir" ] || [ ! -s "$CAKE_TEST_JOB_WORK/sqm-config-drift" ] || cat "$CAKE_TEST_JOB_WORK/sqm-config-drift"
		;;
	cake-autorate.wan.manage_sqm) printf '%s\n' "${CAKE_TEST_MANAGE_SQM:-1}" ;;
	cake-autorate.wan.sqm_enabled) printf '1\n' ;;
	cake-autorate.wan.sqm_section) printf 'cake_wan\n' ;;
	cake-autorate.wan.ul_if) printf 'eth0\n' ;;
	cake-autorate.wan.dl_if) printf 'ifb4eth0\n' ;;
	sqm.cake_wan._cake_autorate_managed) printf 'wan\n' ;;
	sqm.cake_wan.enabled) printf '1\n' ;;
	sqm.cake_wan.interface) printf 'eth0\n' ;;
	*) exit 1 ;;
esac
EOF
cat > "$work/bin/sqm-recover" <<'EOF'
#!/bin/sh
printf '%s %s %s %s %s %s %s\n' "$1" "$2" "$3" "$4" "$5" "$6" "$7" >> "$CAKE_TEST_JOB_WORK/sqm-recover-args"
[ "$#" -eq 7 ] && [ "$1" = wan ] && [ "$2" = cake_wan ] &&
	[ "$3" = eth0 ] && [ "$4" = eth0 ] && [ "$5" = ifb4eth0 ] || exit 1
case "$6" in sha256:????????????????????????????????????????????????????????????????) ;; *) exit 1 ;; esac
case "${6#sha256:}" in *[!0-9a-f]*) exit 1 ;; esac
case "$7" in "$CAKE_AUTORATE_RUNTIME_LOCK_ROOT"/sqm-snapshot.*) ;; *) exit 1 ;; esac
[ -f "$7" ] && [ ! -L "$7" ] || exit 1
snapshot_dir="$CAKE_TEST_JOB_WORK/snapshot-check.$$"
mkdir "$snapshot_dir" || exit 1
cp "$7" "$snapshot_dir/sqm" || exit 1
digest="$(uci -q -c "$snapshot_dir" show "sqm.$2" | LC_ALL=C sort | sha256sum | awk 'NR == 1 { print $1; exit }')"
rm -f "$snapshot_dir/sqm"
rmdir "$snapshot_dir"
[ "$6" = "sha256:$digest" ] || exit 1
[ "${CAKE_TEST_SQM_RECOVER_FAIL:-0}" != 1 ] || exit 1
"$CAKE_AUTORATE_SQM_RUN" start eth0 >/dev/null 2>&1 || exit 1
[ -f "$SQM_STATE_DIR/eth0.state" ]
EOF
chmod +x "$work/bin/uci" "$work/bin/sqm-recover"
: > "$work/sqm-recover-args"

# An active unmanaged queue has no immutable recovery identity. Refuse before
# pausing either autorate or SQM rather than promising state-file-only recovery.
: > "$work/sqm-state/eth0.state"
: > "$work/sqm-actions"
sh -c 'while :; do sleep 1; done' &
autorate_pid="$!"
CAKE_TEST_MANAGE_SQM=0 "$0" recovery-worker "$work" "$autorate_pid" unmanaged \
	> "$work/unmanaged.out" 2> "$work/unmanaged.err" ||
	fail_test "unmanaged SQM fail-closed harness failed"
[ -f "$work/sqm-state/eth0.state" ] || fail_test "unmanaged SQM was mutated"
wait_for_process_state_test "$autorate_pid" running || fail_test "unmanaged preflight paused autorate"
if grep -qx stop "$work/sqm-actions"; then fail_test "unmanaged preflight stopped SQM"; fi
wait_for_recovery_cleanup || fail_test "unmanaged preflight stranded recovery metadata"
kill "$autorate_pid"
wait "$autorate_pid" 2>/dev/null || true
autorate_pid=""

# If SIGSTOP cannot be delivered and observed for the exact autorate PID/start,
# the standalone calibration fails before invoking SQM stop.
: > "$work/sqm-state/eth0.state"
: > "$work/sqm-actions"
sh -c 'while :; do sleep 1; done' &
autorate_pid="$!"
"$0" stop-failure-worker "$work" "$autorate_pid" \
	> "$work/stop-failure.out" 2> "$work/stop-failure.err" ||
	fail_test "unverifiable autorate STOP harness failed"
[ -f "$work/sqm-state/eth0.state" ] || fail_test "SQM changed after autorate STOP failure"
if grep -qx stop "$work/sqm-actions"; then
	fail_test "SQM stop ran after autorate STOP failure"
fi
wait_for_recovery_cleanup || fail_test "STOP-failure recovery metadata was stranded"
kill "$autorate_pid"
wait "$autorate_pid" 2>/dev/null || true
autorate_pid=""

# If immutable SQM recovery fails, autorate must remain stopped and no direct
# run.sh start may bypass ownership/configuration revalidation. A later retry
# through the same helper restores SQM first and only then resumes autorate.
: > "$work/sqm-state/eth0.state"
: > "$work/sqm-actions"
sh -c 'while :; do sleep 1; done' &
autorate_pid="$!"
CAKE_TEST_SQM_RECOVER_FAIL=1 "$0" recovery-worker "$work" "$autorate_pid" restore-fail \
	> "$work/restore-fail.out" 2> "$work/restore-fail.err" ||
	{
		sed -n '1,80p' "$work/restore-fail.err" >&2 || true
		sed -n '1,80p' "$work/restore-fail.out" >&2 || true
		fail_test "immutable SQM restore-failure harness failed"
	}
wait_for_process_state_test "$autorate_pid" running || fail_test "successful recovery retry did not resume autorate"
[ -f "$work/sqm-state/eth0.state" ] || fail_test "successful recovery retry did not restore SQM"
[ "$(grep -c '^start$' "$work/sqm-actions")" -eq 1 ] || fail_test "SQM was started outside the one successful immutable recovery"
wait_for_recovery_cleanup || fail_test "restore-failure retry stranded recovery metadata"
kill "$autorate_pid"
wait "$autorate_pid" 2>/dev/null || true
autorate_pid=""

# A live SQM UCI change after STOP must not strand the link unshaped. Recovery
# consumes the caller-captured pre-stop RAM snapshot, restores that exact
# runtime, and only then resumes autorate; mutable live UCI is irrelevant.
: > "$work/sqm-state/eth0.state"
: > "$work/sqm-actions"
sh -c 'while :; do sleep 1; done' &
autorate_pid="$!"
"$0" recovery-worker "$work" "$autorate_pid" snapshot-drift \
	> "$work/snapshot-drift.out" 2> "$work/snapshot-drift.err" ||
	fail_test "SQM immutable-snapshot drift harness failed"
wait_for_process_state_test "$autorate_pid" running || fail_test "snapshot recovery did not resume autorate"
[ -f "$work/sqm-state/eth0.state" ] || fail_test "snapshot recovery did not restore SQM"
[ "$(grep -c '^start$' "$work/sqm-actions")" -eq 1 ] || fail_test "snapshot recovery performed an extra SQM start"
wait_for_recovery_cleanup || fail_test "snapshot recovery stranded recovery metadata"
kill "$autorate_pid"
wait "$autorate_pid" 2>/dev/null || true
autorate_pid=""

# A standalone unshaped test arms a separate recovery supervisor before its
# first mutation. SIGKILL of the worker must restart SQM, resume the exact
# stopped autorate process, and remove every journal/ownership record.
rm -f "$work/calibration.prepared" "$work/calibration.release"
: > "$work/sqm-state/eth0.state"
: > "$work/sqm-actions"
sh -c 'while :; do sleep 1; done' &
autorate_pid="$!"
"$0" recovery-worker "$work" "$autorate_pid" crash \
	> "$work/recovery-worker.out" 2> "$work/recovery-worker.err" &
recovery_worker_pid="$!"
wait_for_file "$work/calibration.prepared" "$recovery_worker_pid"
wait_for_process_state_test "$autorate_pid" stopped || fail_test "autorate was not verifiably stopped"
[ ! -e "$work/sqm-state/eth0.state" ] || {
	sed -n '1,20p' "$work/recovery-worker.err" >&2 || true
	sed -n '1,20p' "$work/sqm-actions" >&2 || true
	fail_test "SQM did not stop for unshaped test"
}
recovery_journal="$(first_recovery_journal)" || fail_test "recovery journal was not armed"
supervisor_pid="$(sed -n '1s/^pid=//p' "$recovery_journal.ready")"
[ -n "$supervisor_pid" ] || fail_test "recovery supervisor identity missing"
grep -qx 'version=4' "$recovery_journal"
grep -qx 'sqm_managed=1' "$recovery_journal"
grep -qx 'sqm_section=cake_wan' "$recovery_journal"
grep -qx 'sqm_target=eth0' "$recovery_journal"
grep -qx 'sqm_ul_if=eth0' "$recovery_journal"
grep -qx 'sqm_dl_if=ifb4eth0' "$recovery_journal"
sqm_config_fingerprint="$(sed -n 's/^sqm_config_fingerprint=//p' "$recovery_journal")"
case "$sqm_config_fingerprint" in sha256:????????????????????????????????????????????????????????????????) ;; *)
	fail_test "SIGKILL recovery journal has no strict SQM fingerprint" ;;
esac
case "${sqm_config_fingerprint#sha256:}" in *[!0-9a-f]*) fail_test "SQM fingerprint is not lowercase hex" ;; esac
sqm_config_snapshot="$(sed -n 's/^sqm_config_snapshot=//p' "$recovery_journal")"
case "$sqm_config_snapshot" in "$work/runtime-locks"/sqm-snapshot.*) ;; *)
	fail_test "SIGKILL recovery journal has no safe SQM snapshot path" ;;
esac
[ -f "$sqm_config_snapshot" ] && [ ! -L "$sqm_config_snapshot" ] ||
	fail_test "SIGKILL recovery snapshot is not a regular file"
kill -KILL "$recovery_worker_pid"
wait "$recovery_worker_pid" 2>/dev/null || true
recovery_worker_pid=""
wait_for_process_state_test "$autorate_pid" running || fail_test "SIGKILL recovery did not resume autorate"
attempt=0
while [ ! -e "$work/sqm-state/eth0.state" ]; do
	attempt=$((attempt + 1))
	[ "$attempt" -lt 300 ] || fail_test "SIGKILL recovery did not restart SQM"
	sleep 0.02
done
wait_for_recovery_cleanup || {
	printf '%s\n' 'SIGKILL recovery diagnostics:' >&2
	ls -la "$work/jobs" "$work/runtime-locks" >&2 || true
	for diagnostic_file in "$work/jobs"/recovery-*.journal "$work/runtime-locks"/interface-eth0.lock; do
		[ -e "$diagnostic_file" ] || continue
		printf '%s\n' "--- $diagnostic_file" >&2
		sed -n '1,40p' "$diagnostic_file" >&2 || true
	done
	[ -z "$supervisor_pid" ] || sed -n '1,30p' "/proc/$supervisor_pid/status" >&2 || true
	fail_test "SIGKILL recovery metadata was stranded"
}
wait_for_process_gone "$supervisor_pid" || fail_test "SIGKILL recovery supervisor was stranded"
supervisor_pid=""
[ "$(grep -c '^stop$' "$work/sqm-actions")" -eq 1 ] || fail_test "unexpected SQM stop count"
[ "$(grep -c '^start$' "$work/sqm-actions")" -eq 1 ] || fail_test "unexpected SQM start count"
grep -Fqx "wan cake_wan eth0 eth0 ifb4eth0 $sqm_config_fingerprint $sqm_config_snapshot" "$work/sqm-recover-args" ||
	fail_test "SIGKILL recovery did not use immutable managed-SQM arguments"
[ ! -e "$sqm_config_snapshot" ] || fail_test "successful SIGKILL recovery stranded its SQM snapshot"
kill "$autorate_pid"
wait "$autorate_pid" 2>/dev/null || true
autorate_pid=""

# Normal completion may race with a newly acquired interface lock. Freeze the
# settled supervisor, let the owner restore/release, acquire a foreign record,
# then resume the supervisor. It must leave the new owner untouched.
rm -f "$work/calibration.prepared" "$work/calibration.release"
: > "$work/sqm-state/eth0.state"
: > "$work/sqm-actions"
sh -c 'while :; do sleep 1; done' &
autorate_pid="$!"
"$0" recovery-worker "$work" "$autorate_pid" normal \
	> "$work/normal-worker.out" 2> "$work/normal-worker.err" &
recovery_worker_pid="$!"
wait_for_file "$work/calibration.prepared" "$recovery_worker_pid"
recovery_journal="$(first_recovery_journal)" || fail_test "normal recovery journal missing"
supervisor_pid="$(sed -n '1s/^pid=//p' "$recovery_journal.ready")"
kill -STOP "$supervisor_pid"
: > "$work/calibration.release"
wait "$recovery_worker_pid" || fail_test "normal recovery worker failed"
recovery_worker_pid=""
CAKE_FOREIGN_READY="$work/foreign.ready" CAKE_FOREIGN_RELEASE="$work/foreign.release" \
sh -c '
	set -eu
	. "$CAKE_AUTORATE_RUNTIME_LOCK_LIB"
	runtime_lock_acquire_global_shared
	runtime_lock_acquire_interface eth0 foreign "" foreigntoken
	trap '\''runtime_lock_release_interface >/dev/null 2>&1 || true; runtime_lock_release_global >/dev/null 2>&1 || true'\'' EXIT
	: > "$CAKE_FOREIGN_READY"
	while [ ! -e "$CAKE_FOREIGN_RELEASE" ]; do sleep 0.05; done
' &
foreign_holder_pid="$!"
wait_for_file "$work/foreign.ready" "$foreign_holder_pid"
kill -CONT "$supervisor_pid"
wait_for_process_gone "$supervisor_pid" || fail_test "settled recovery supervisor did not exit"
supervisor_pid=""
grep -qx 'role=foreign' "$work/runtime-locks/interface-eth0.lock" ||
	fail_test "settled supervisor overwrote the new owner role"
grep -qx 'token=foreigntoken' "$work/runtime-locks/interface-eth0.lock" ||
	fail_test "settled supervisor overwrote the new owner token"
: > "$work/foreign.release"
wait "$foreign_holder_pid"
foreign_holder_pid=""
wait_for_recovery_cleanup || {
	ls -la "$work/jobs" "$work/runtime-locks" >&2 || true
	fail_test "normal recovery metadata was stranded"
}
wait_for_process_state_test "$autorate_pid" running || fail_test "normal completion left autorate paused"
[ -f "$work/sqm-state/eth0.state" ] || fail_test "normal completion left SQM stopped"
kill "$autorate_pid"
wait "$autorate_pid" 2>/dev/null || true
autorate_pid=""

# If advisory metadata is replaced while a crash obligation is armed, the
# supervisor must not overwrite that foreign record or mutate network state.
# This is a deliberate corruption fixture: fail closed, then let the test
# harness perform the manual cleanup.
rm -f "$work/calibration.prepared" "$work/calibration.release"
: > "$work/sqm-state/eth0.state"
: > "$work/sqm-actions"
sh -c 'while :; do sleep 1; done' &
autorate_pid="$!"
"$0" recovery-worker "$work" "$autorate_pid" crash \
	> "$work/foreign-record-worker.out" 2> "$work/foreign-record-worker.err" &
recovery_worker_pid="$!"
wait_for_file "$work/calibration.prepared" "$recovery_worker_pid"
wait_for_process_state_test "$autorate_pid" stopped || fail_test "foreign-record fixture did not pause autorate"
recovery_journal="$(first_recovery_journal)" || fail_test "foreign-record recovery journal missing"
supervisor_pid="$(sed -n '1s/^pid=//p' "$recovery_journal.ready")"
foreign_start="$(speedtest_process_starttime "$$")"
cat > "$work/runtime-locks/interface-eth0.lock" <<EOF
version=1
pid=$$
proc_starttime=$foreign_start
role=foreign
token=foreignarmedtoken
recovery_journal=
EOF
kill -KILL "$recovery_worker_pid"
wait "$recovery_worker_pid" 2>/dev/null || true
recovery_worker_pid=""
wait_for_process_gone "$supervisor_pid" || fail_test "foreign-record supervisor did not fail closed"
supervisor_pid=""
[ ! -e "$work/sqm-state/eth0.state" ] || fail_test "foreign-record failure unexpectedly restarted SQM"
wait_for_process_state_test "$autorate_pid" stopped || fail_test "foreign-record failure unexpectedly resumed autorate"
if grep -qx start "$work/sqm-actions"; then
	fail_test "foreign-record failure invoked SQM start"
fi
grep -qx 'role=foreign' "$work/runtime-locks/interface-eth0.lock" ||
	fail_test "armed supervisor overwrote a foreign record"
grep -qx 'token=foreignarmedtoken' "$work/runtime-locks/interface-eth0.lock" ||
	fail_test "armed supervisor overwrote a foreign token"
rm -f "$work/runtime-locks/interface-eth0.lock" "$recovery_journal" "$recovery_journal.ready"
kill -CONT "$autorate_pid"
SQM_STATE_DIR="$work/sqm-state" "$work/bin/sqm-run" start eth0
kill "$autorate_pid"
wait "$autorate_pid" 2>/dev/null || true
autorate_pid=""

# A malformed pre-existing ownership record is not equivalent to a missing
# record. The crash supervisor must preserve it byte-for-byte and fail closed
# without starting SQM or resuming autorate.
rm -f "$work/calibration.prepared" "$work/calibration.release"
: > "$work/sqm-state/eth0.state"
: > "$work/sqm-actions"
sh -c 'while :; do sleep 1; done' &
autorate_pid="$!"
"$0" recovery-worker "$work" "$autorate_pid" crash \
	> "$work/malformed-record-worker.out" 2> "$work/malformed-record-worker.err" &
recovery_worker_pid="$!"
wait_for_file "$work/calibration.prepared" "$recovery_worker_pid"
wait_for_process_state_test "$autorate_pid" stopped || fail_test "malformed-record fixture did not pause autorate"
recovery_journal="$(first_recovery_journal)" || fail_test "malformed-record recovery journal missing"
supervisor_pid="$(sed -n '1s/^pid=//p' "$recovery_journal.ready")"
printf '%s\n' 'this-is-not-a-valid-runtime-lock-record' > "$work/runtime-locks/interface-eth0.lock"
cp "$work/runtime-locks/interface-eth0.lock" "$work/malformed-record.expected"
kill -KILL "$recovery_worker_pid"
wait "$recovery_worker_pid" 2>/dev/null || true
recovery_worker_pid=""
wait_for_process_gone "$supervisor_pid" || fail_test "malformed-record supervisor did not fail closed"
supervisor_pid=""
[ ! -e "$work/sqm-state/eth0.state" ] || fail_test "malformed record unexpectedly restarted SQM"
wait_for_process_state_test "$autorate_pid" stopped || fail_test "malformed record unexpectedly resumed autorate"
if grep -qx start "$work/sqm-actions"; then
	fail_test "malformed record invoked SQM start"
fi
cmp -s "$work/malformed-record.expected" "$work/runtime-locks/interface-eth0.lock" ||
	fail_test "malformed ownership record was overwritten"
rm -f "$work/runtime-locks/interface-eth0.lock" "$recovery_journal" "$recovery_journal.ready"
kill -CONT "$autorate_pid"
SQM_STATE_DIR="$work/sqm-state" "$work/bin/sqm-run" start eth0
kill "$autorate_pid"
wait "$autorate_pid" 2>/dev/null || true
autorate_pid=""

# Every managed-SQM harness above executes the live preflight fingerprint
# comparison.  This catches shell-valid but semantically split tests such as a
# newline after `!=`, which ash interprets as a missing `]` followed by an
# attempted command named after the SHA256 digest.
if grep -E '(^|[[:space:]])missing \]|sha256:[0-9a-f]{64}: not found' \
	"$work"/*.err "$work"/*.out >/dev/null 2>&1; then
	echo 'managed-SQM fingerprint comparison emitted an ash parse/runtime error' >&2
	grep -E '(^|[[:space:]])missing \]|sha256:[0-9a-f]{64}: not found' \
		"$work"/*.err "$work"/*.out >&2 || true
	exit 1
fi

echo "speedtest routing tests passed"
