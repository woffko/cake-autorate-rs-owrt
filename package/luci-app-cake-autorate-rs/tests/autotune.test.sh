#!/bin/sh
set -eu

test_dir="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
package_dir="$(CDPATH= cd -- "$test_dir/../../cake-autorate-rs" && pwd)"
autotune="$test_dir/../root/usr/libexec/cake-autorate-rs/autotune"
fixtures="$test_dir/fixtures/autotune"
work="$(mktemp -d "${TMPDIR:-/tmp}/cake-autorate-autotune-test.XXXXXX")"
PATH="$fixtures:$PATH"
export PATH

cleanup() {
	[ -n "${blocking_pid:-}" ] && kill "$blocking_pid" 2>/dev/null || true
	[ "${AUTOTUNE_TEST_KEEP_WORK:-0}" = 1 ] || rm -rf "$work"
}
trap cleanup EXIT INT TERM

wait_for_job_cleanup() {
	cleanup_job="$1"
	cleanup_attempt=0
	while [ "$cleanup_attempt" -lt 100 ] && [ -e "$work/jobs/$cleanup_job/pid" ]; do
		cleanup_attempt=$((cleanup_attempt + 1))
		sleep 0.05
	done
	[ ! -e "$work/jobs/$cleanup_job/pid" ]
}

mkdir -p "$work/jobs"
: > "$work/counter"

export CAKE_AUTORATE_AUTOTUNE_DIR="$work/jobs"
export CAKE_AUTORATE_SPEEDTEST="$fixtures/speedtest"
export CAKE_AUTORATE_PINGER_PLAN="$fixtures/pinger-plan"
export CAKE_AUTORATE_JSONFILTER="$fixtures/jsonfilter"
export CAKE_AUTORATE_FPING="$fixtures/fping"
export CAKE_AUTORATE_DAEMON="$package_dir/src/target/debug/cake-autorated"
export CAKE_AUTORATE_TRANSPORT_PROBE="$fixtures/transport-probe"
export CAKE_AUTORATE_AUTOTUNE_RECOVER="${CAKE_AUTORATE_AUTOTUNE_RECOVER:-$test_dir/../root/usr/libexec/cake-autorate-rs/autotune-recover}"
export CAKE_AUTORATE_RUNTIME_LOCK_LIB="$package_dir/files/usr/libexec/cake-autorate-rs/runtime-lock"
export CAKE_AUTORATE_RUNTIME_LOCK_ROOT="$work/runtime-locks"
export CAKE_AUTORATE_AUTOTUNE_TEST_PREFLIGHT=1
export CAKE_AUTORATE_REFLECTOR_SCAN_COOLDOWN_S=0
export CAKE_AUTORATE_BASELINE_RETRY_BACKOFF_S=0
export CAKE_AUTORATE_AUTOTUNE_TEST_LINK_KIND=cellular
export CAKE_AUTORATE_AUTOTUNE_TEST_SHAPER=1
export AUTOTUNE_MOCK_SHAPER_RESTORE_MARKER="$work/shaper-restored"
export AUTOTUNE_MOCK_COUNTER="$work/counter"
export AUTOTUNE_MOCK_PIN_LOG="$work/server-pins"
export AUTOTUNE_MOCK_DIRECTION_LOG="$work/test-directions"
export AUTOTUNE_MOCK_CONFIGURED_DL_KBPS=50000
export AUTOTUNE_MOCK_CONFIGURED_UL_KBPS=10000

# Minimal OpenWrt images such as the production x86 Multi-WAN router provide
# neither od nor cksum. Worker/shaper identity must come directly from the
# kernel UUID source and stay a strict 32-hex value; no late fallback may turn
# it into an uptime-pid string after the worker has already launched.
printf '%s\n' '12345678-90AB-CDEF-1234-567890ABCDEF' > "$work/random-uuid"
mkdir -p "$work/sys-class-net"
CAKE_AUTORATE_RANDOM_UUID_FILE="$work/random-uuid" \
CAKE_AUTORATE_SYS_CLASS_NET="$work/sys-class-net" \
CAKE_AUTORATE_AUTOTUNE_SOURCE_ONLY=1 sh -c '
	set -eu
	. "$1"
	worker_token="$(new_unique_worker_token)"
	[ "$worker_token" = 1234567890abcdef1234567890abcdef ]
	configure_temp_shaper_identity
	[ "$temp_ifb" = catf12345678 ]
	[ "$temp_ifb_alias" = cake-autotune-1234567890abcdef1234567890abcdef ]
	[ "$temp_target_handle" = a123: ]
	[ "$temp_ifb_handle" = b123: ]
	[ "${#temp_ifb}" -le 15 ]
' sh "$autotune"
if grep -Eq '(^|[^A-Za-z0-9_])(od|cksum)([^A-Za-z0-9_]|$)' "$autotune"; then
	echo "autotune still has a hidden od/cksum runtime dependency" >&2
	exit 1
fi

# A pre-existing interface with the same candidate name is never reused or
# removed. The bounded generator retries and fails while the foreign path
# remains intact.
mkdir "$work/sys-class-net/catf12345678"
if CAKE_AUTORATE_RANDOM_UUID_FILE="$work/random-uuid" \
   CAKE_AUTORATE_SYS_CLASS_NET="$work/sys-class-net" \
   CAKE_AUTORATE_AUTOTUNE_SOURCE_ONLY=1 sh -c '
	. "$1"
	new_unique_worker_token
' sh "$autotune" >/dev/null 2>&1; then
	echo "colliding temporary IFB identity was accepted" >&2
	exit 1
fi
[ -d "$work/sys-class-net/catf12345678" ]

# Invalid/missing kernel randomness is rejected synchronously by the start RPC
# before a worker, interface lock, SQM pause, or recovery journal can exist.
printf '%s\n' 'not-a-kernel-uuid' > "$work/invalid-random-uuid"
export CAKE_AUTORATE_RANDOM_UUID_FILE="$work/invalid-random-uuid"
if "$autotune" baduuid lo start speedtest-go > "$work/baduuid.json"; then
	echo "invalid worker UUID unexpectedly started Full Auto-Tune" >&2
	exit 1
fi
grep -q 'Unable to generate a unique 32-hex Full Auto-Tune worker identity' "$work/baduuid.json"
[ ! -e "$work/jobs/baduuid/pid" ]
[ ! -d "$work/jobs/recovery" ] ||
	! find "$work/jobs/recovery" -name 'baduuid_*' -print -quit | grep -q .
unset CAKE_AUTORATE_RANDOM_UUID_FILE

# The fixture must model jsonfilter's top-level lookup semantics.  Greedy text
# extraction used to select nested proposal fields and report schema 1/state
# inner instead of the terminal envelope's schema 6/state complete.
nested_terminal='{"state":"complete","schema_version":5,"nested":{"state":"inner","schema_version":1}}'
test "$(printf '%s\n' "$nested_terminal" | "$CAKE_AUTORATE_JSONFILTER" -e '@.state')" = complete
test "$(printf '%s\n' "$nested_terminal" | "$CAKE_AUTORATE_JSONFILTER" -e '@.schema_version')" = 5

# Request matching must consume the identity snapshot authenticated by
# worker_identity_matches().  The worker is allowed to unlink pid_file as it
# settles; a second read would manufacture a false mismatch in that window.
CAKE_AUTORATE_AUTOTUNE_SOURCE_ONLY=1 sh -c '
	. "$1"
	pid_file="$2/identity-already-removed"
	target_if_input=lo
	backend_input=speedtest-go
	route_mode_override=
	mwan3_member_override=
	identity_target=lo
	identity_backend=speedtest-go
	identity_route_mode=
	identity_mwan3_member=
	autotune_profile=best_overall
	identity_profile=best_overall
	identity_conservative=0
	worker_request_matches any
' sh "$autotune" "$work"

# Typed pinger reconstruction keeps IRTT targets separate from the independently
# selected RTT baseline pool and rejects option/shell-like helper output.
CAKE_AUTORATE_AUTOTUNE_SOURCE_ONLY=1 sh -c '
	set -eu
	. "$1"
	actual="$(pinger_targets_json_array irtt "[2001:db8::1]:2112" "irtt.example")"
	expected="[\"[2001:db8::1]:2112\",\"irtt.example\"]"
	[ "$actual" = "$expected" ]
	! pinger_targets_json_array fping "--bad-option" >/dev/null
	strict_single_json_object "{\"state\":\"complete\",\"schema_version\":4}"
	! strict_single_json_object "$(printf "%s\n%s" "{\"state\":\"complete\",\"schema_version\":4}" "{\"forged\":true}")"
' sh "$autotune"

# Confidence is evidence-derived rather than inherited from the button used to
# start the worker. The weakest direction controls the overall percentage, and
# any sticky contamination is reviewable but cannot be labelled trusted.
CAKE_AUTORATE_AUTOTUNE_SOURCE_ONLY=1 sh -c '
	set -eu
	. "$1"
	observed_background_dl_max_share_percent=0
	observed_background_ul_max_share_percent=0
	baseline_quality_provisional=false
	unresolved_background_contamination=false
	validation_contaminated=false
	phase_contamination_seen=false
	build_autotune_confidence
	[ "$capacity_download_confidence_percent" = 100 ]
	[ "$capacity_upload_confidence_percent" = 100 ]
	[ "$quality_confidence_percent" = 100 ]
	[ "$overall_confidence_percent" = 100 ]
	[ "$result_class" = trusted ]
	[ "$confidence_json" = "{\"overall_percent\":100,\"capacity_download_percent\":100,\"capacity_upload_percent\":100,\"quality_percent\":100,\"reasons\":[]}" ]

	observed_background_dl_max_share_percent=12
	observed_background_ul_max_share_percent=35
	phase_contamination_seen=true
	unresolved_background_contamination=true
	build_autotune_confidence
	[ "$capacity_download_confidence_percent" = 88 ]
	[ "$capacity_upload_confidence_percent" = 60 ]
	[ "$quality_confidence_percent" = 70 ]
	[ "$overall_confidence_percent" = 60 ]
	[ "$result_class" = provisional ]
	case "$confidence_json" in
		*"\"code\":\"background-share-download\""*"\"code\":\"background-contamination-accepted\""*) ;;
		*) exit 1 ;;
	esac

	observed_background_dl_max_share_percent=0
	observed_background_ul_max_share_percent=0
	unresolved_background_contamination=false
	phase_contamination_seen=true
	build_autotune_confidence
	[ "$quality_confidence_percent" = 84 ]
	[ "$overall_confidence_percent" = 84 ]
	[ "$result_class" = provisional ]

	observed_background_dl_max_share_percent=81
	phase_contamination_seen=false
	build_autotune_confidence
	[ "$capacity_download_confidence_percent" = 25 ]
	[ "$overall_confidence_percent" = 25 ]
	[ "$result_class" = estimated ]
' sh "$autotune"

# The terminal-race distinction must not weaken live argv authentication.
# An exact live helper returns 0; the same PID/starttime with a missing
# immutable argument returns the dedicated live-mismatch code 1.
CAKE_AUTORATE_AUTOTUNE_SOURCE_ONLY=1 sh -c '
	set -eu
	. "$1"
	speedtest_bin=/bin/sh
	job_name=identity-job
	target_if=identity-if
	/bin/sh -c "while :; do sleep 1; done" identity-job identity-if &
	identity_pid="$!"
	identity_start="$(proc_starttime "$identity_pid")"
	speedtest_identity_matches "$identity_pid" "$identity_start"
	target_if=missing-if
	set +e
	speedtest_identity_matches "$identity_pid" "$identity_start"
	identity_rc="$?"
	set -e
	[ "$identity_rc" -eq 1 ]
	kill "$identity_pid" 2>/dev/null || true
	wait "$identity_pid" 2>/dev/null || true
' sh "$autotune"

"$autotune" fullauto lo start speedtest-go > "$work/start.json"

attempt=0
while [ "$attempt" -lt 800 ]; do
	if ! "$autotune" fullauto lo status speedtest-go > "$work/status.json"; then
		printf 'Full Auto-Tune status RPC failed during terminal-transition stress sample %s.\n' "$attempt" >&2
		cat "$work/status.json" >&2 || true
		exit 1
	fi
	status_state="$("$CAKE_AUTORATE_JSONFILTER" -e '@.state' < "$work/status.json")" || {
		printf 'Full Auto-Tune status returned malformed JSON during stress sample %s.\n' "$attempt" >&2
		cat "$work/status.json" >&2 || true
		exit 1
	}
	[ -n "$status_state" ] || exit 1
	if grep -q '"state":"complete"' "$work/status.json"; then
		break
	fi
	case "$status_state" in
		running|cancelling|recovery-pending) ;;
		*)
			printf 'Full Auto-Tune unexpectedly ended in state %s during the happy-path test.\n' \
				"$status_state" >&2
			cat "$work/status.json" >&2
			exit 1
			;;
	esac
	attempt=$((attempt + 1))
	sleep 0.02
done

grep -q '"state":"complete"' "$work/status.json"
grep -q '"configuration_written":false' "$work/status.json"
grep -q '"download_kbps":50000' "$work/status.json"
grep -q '"download_kbps":110000' "$work/status.json"
grep -q '"enabled":true' "$work/status.json"
grep -q '"kind":"cellular"' "$work/status.json"
grep -q '"median_ms":13.000' "$work/status.json"
grep -q '"transport_median_ms":12.500' "$work/status.json"
grep -q '"transport_latency":{' "$work/status.json"
grep -q '"validation":{"profile":"best_overall","pass":true' "$work/status.json"
grep -q '"manual_apply_eligible":true' "$work/status.json"
grep -q '"comparison":"direction-matched-observed-low"' "$work/status.json"
grep -q '"profile_outcome":{"mode":"target-a-met"' "$work/status.json"
grep -q '"profile_search":{"download":{"schema_version":1' "$work/status.json"
grep -q '"auto_apply_eligible":true' "$work/status.json"
grep -q '"phase_evidence_complete":true' "$work/status.json"
grep -Eq '"config_fingerprint":"sha256:[0-9a-f]{64}"' "$work/status.json"
grep -q '"phase_contamination_seen":false' "$work/status.json"
grep -q '"phase":"baseline"' "$work/status.json"
grep -q '"direction_phases":{"download":' "$work/status.json"
grep -q '"signals":{"download":' "$work/status.json"
grep -q '"code":"download-transport-latency"' "$work/status.json"
grep -q '"code":"upload-transport-latency"' "$work/status.json"
test -f "$work/shaper-restored"
test "$(sed -n '1p' "$work/server-pins")" = automatic
test "$(sed -n '2p' "$work/server-pins")" = 17372
test "$(sed -n '3p' "$work/server-pins")" = 17372
test "$(sed -n '4p' "$work/server-pins")" = 17372
test "$(sed -n '5p' "$work/server-pins")" = 17372
test "$(sed -n '1p' "$work/test-directions")" = both
test "$(sed -n '2p' "$work/test-directions")" = download
test "$(sed -n '3p' "$work/test-directions")" = download
test "$(sed -n '4p' "$work/test-directions")" = upload
test "$(sed -n '5p' "$work/test-directions")" = upload
test "$(sed -n '6p' "$work/test-directions")" = download
test "$(sed -n '7p' "$work/test-directions")" = upload
wait_for_job_cleanup fullauto

# Regression: choosing conservative continuation is consent to measure in the
# presence of background traffic, not a permanent low-confidence verdict. A
# fresh conservative run whose actual evidence is clean and whose final
# validation scores 100/100 must remain manually applicable (and may regain
# trusted/auto eligibility).
: > "$work/counter"
export AUTOTUNE_MOCK_CONSERVATIVE_PHASE=1
"$autotune" conservativeclean lo start-conservative speedtest-go > "$work/conservativeclean-start.json"
attempt=0
while [ "$attempt" -lt 800 ]; do
	"$autotune" conservativeclean lo status speedtest-go > "$work/conservativeclean-status.json"
	grep -q '"state":"complete"' "$work/conservativeclean-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.02
done
node - "$work/conservativeclean-status.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
if (result.schema_version !== 8 || result.state !== 'complete' ||
    result.conservative !== true || result.phase_contamination_seen !== false ||
    !result.validation || result.validation.score !== 100 ||
    result.validation.safety_pass !== true ||
    !Array.isArray(result.validation_attempts) ||
    !result.validation_attempts.some(attempt => attempt.score === 100) ||
    result.manual_apply_eligible !== true || result.auto_apply_eligible !== true ||
    result.result_class !== 'trusted' || !result.confidence ||
    result.confidence.overall_percent !== 100 ||
    result.confidence.capacity_download_percent !== 100 ||
    result.confidence.capacity_upload_percent !== 100 ||
    result.confidence.quality_percent !== 100 ||
    !Array.isArray(result.confidence.reasons) || result.confidence.reasons.length !== 0 ||
    !result.proposal || typeof result.proposal.confidence !== 'number')
	throw new Error('clean conservative continuation remained ineligible or lost trusted confidence');
EOF
wait_for_job_cleanup conservativeclean
unset AUTOTUNE_MOCK_CONSERVATIVE_PHASE

# A helper may publish its complete JSON and exit while one finishing child
# briefly keeps the supervised process group alive.  The leader is then a
# zombie with an empty /proc/<pid>/cmdline.  That normal terminal race must be
# reaped as success, not misclassified as an identity-mismatch attack.
: > "$work/counter"
export AUTOTUNE_MOCK_POST_RESULT_DESCENDANT_AT_COUNT=7
"$autotune" terminalrace lo start speedtest-go > "$work/terminalrace-start.json"
attempt=0
while [ "$attempt" -lt 800 ]; do
	"$autotune" terminalrace lo status speedtest-go > "$work/terminalrace-status.json"
	grep -q '"state":"complete"' "$work/terminalrace-status.json" && break
	if grep -q '"state":"failed"' "$work/terminalrace-status.json"; then
		cat "$work/terminalrace-status.json" >&2
		exit 1
	fi
	attempt=$((attempt + 1))
	sleep 0.02
done
grep -q '"state":"complete"' "$work/terminalrace-status.json"
! grep -q 'identity-mismatch' "$work/terminalrace-status.json"
wait_for_job_cleanup terminalrace
unset AUTOTUNE_MOCK_POST_RESULT_DESCENDANT_AT_COUNT

# A helper success code with malformed output must yield valid terminal JSON;
# untrusted text must never be spliced into the diagnostic object.
: > "$work/counter"
export AUTOTUNE_MOCK_MALFORMED_PINGER=1
"$autotune" malformedplan lo start speedtest-go > "$work/malformedplan-start.json"
attempt=0
while [ "$attempt" -lt 220 ]; do
	"$autotune" malformedplan lo status speedtest-go > "$work/malformedplan-status.json"
	grep -q 'malformed or empty JSON' "$work/malformedplan-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q 'malformed or empty JSON' "$work/malformedplan-status.json"
node -e 'JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"))' "$work/malformedplan-status.json"
! grep -q '"forged":true' "$work/malformedplan-status.json"
wait_for_job_cleanup malformedplan
unset AUTOTUNE_MOCK_MALFORMED_PINGER

# A helper exit code remains authoritative even when the helper managed to
# print a well-formed result first. The raw diagnostic is retained in RAM, but
# it must never be promoted to a successful measurement.
: > "$work/counter"
export AUTOTUNE_MOCK_VALID_JSON_EXIT_CODE=7
export AUTOTUNE_MOCK_VALID_JSON_EXIT_AT_COUNT=1
"$autotune" nonzerojson lo start speedtest-go > "$work/nonzerojson-start.json"
attempt=0
while [ "$attempt" -lt 220 ]; do
	"$autotune" nonzerojson lo status speedtest-go > "$work/nonzerojson-status.json"
	grep -q '"reason":"helper-exit:7"' "$work/nonzerojson-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"failed"' "$work/nonzerojson-status.json"
grep -q '"reason":"helper-exit:7","exit_code":7,"raw_available":true' "$work/nonzerojson-status.json"
node - "$work/nonzerojson-status.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
if (result.speedtest_supervisor.raw_bytes < 2)
	throw new Error('valid helper diagnostic was not retained');
if (result.auto_apply_eligible !== false || result.manual_apply_eligible !== false)
	throw new Error('failed helper result became reviewable');
EOF
wait_for_job_cleanup nonzerojson
unset AUTOTUNE_MOCK_VALID_JSON_EXIT_CODE AUTOTUNE_MOCK_VALID_JSON_EXIT_AT_COUNT

# A structured speed-test helper error must survive the supervisor boundary.
# This is the actionable reason shown by Full Auto-Tune instead of a bare
# helper-exit:1 message.
: > "$work/counter"
export AUTOTUNE_MOCK_JSON_ERROR="Managed SQM restore failed: ingress redirect is missing."
export AUTOTUNE_MOCK_JSON_ERROR_AT_COUNT=1
export AUTOTUNE_MOCK_JSON_ERROR_EXIT_CODE=9
"$autotune" helperdetail lo start speedtest-go > "$work/helperdetail-start.json"
attempt=0
while [ "$attempt" -lt 220 ]; do
	"$autotune" helperdetail lo status speedtest-go > "$work/helperdetail-status.json"
	grep -q 'Managed SQM restore failed' "$work/helperdetail-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
node - "$work/helperdetail-status.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
if (result.state !== 'failed')
	throw new Error(`expected failed state, received ${result.state}`);
if (result.speedtest_supervisor.reason !== 'helper-exit:9')
	throw new Error(`unexpected supervisor reason ${result.speedtest_supervisor.reason}`);
if (result.speedtest_supervisor.detail !==
    'Managed SQM restore failed: ingress redirect is missing.')
	throw new Error(`structured helper detail was lost: ${result.speedtest_supervisor.detail}`);
if (!result.error.includes('Managed SQM restore failed'))
	throw new Error(`actionable helper detail was not promoted to the user error: ${result.error}`);
EOF
wait_for_job_cleanup helperdetail
unset AUTOTUNE_MOCK_JSON_ERROR AUTOTUNE_MOCK_JSON_ERROR_AT_COUNT \
	AUTOTUNE_MOCK_JSON_ERROR_EXIT_CODE

# One remote helper reset during shaped validation is retried at the same
# candidate, direction and pinned server. The failed attempt remains visible
# in RAM-only evidence and cannot itself become an accepted measurement.
: > "$work/counter"
export AUTOTUNE_MOCK_JSON_ERROR="Transient remote transfer failure."
export AUTOTUNE_MOCK_JSON_ERROR_AT_COUNT=7
"$autotune" transientphase lo start speedtest-go > "$work/transientphase-start.json"
attempt=0
while [ "$attempt" -lt 600 ]; do
	"$autotune" transientphase lo status speedtest-go > "$work/transientphase-status.json"
	grep -q '"state":"complete"' "$work/transientphase-status.json" && break
	if grep -q '"state":"failed"' "$work/transientphase-status.json"; then
		cat "$work/transientphase-status.json" >&2
		exit 1
	fi
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"complete"' "$work/transientphase-status.json"
grep -q '"reason":"helper-exit:1"' "$work/transientphase-status.json"
grep -q '"attempt":2,"measured_kbps"' "$work/transientphase-status.json"
wait_for_job_cleanup transientphase
unset AUTOTUNE_MOCK_JSON_ERROR AUTOTUNE_MOCK_JSON_ERROR_AT_COUNT

# Two consecutive helper exits fail closed. Evidence from an earlier
# candidate is never returned as if it described the now-unstable link.
: > "$work/counter"
export AUTOTUNE_MOCK_JSON_ERROR="Persistent remote transfer failure."
export AUTOTUNE_MOCK_JSON_ERROR_AT_COUNT=7
export AUTOTUNE_MOCK_JSON_ERROR_AT_COUNT_2=8
"$autotune" persistentphase lo start speedtest-go > "$work/persistentphase-start.json"
attempt=0
while [ "$attempt" -lt 300 ]; do
	"$autotune" persistentphase lo status speedtest-go > "$work/persistentphase-status.json"
	grep -q '"state":"failed"' "$work/persistentphase-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"failed"' "$work/persistentphase-status.json"
grep -q '"reason":"helper-exit:1"' "$work/persistentphase-status.json"
grep -q '"manual_apply_eligible":false' "$work/persistentphase-status.json"
grep -q '"configuration_written":false' "$work/persistentphase-status.json"
wait_for_job_cleanup persistentphase
unset AUTOTUNE_MOCK_JSON_ERROR AUTOTUNE_MOCK_JSON_ERROR_AT_COUNT \
	AUTOTUNE_MOCK_JSON_ERROR_AT_COUNT_2

# A helper that silently runs both directions is not valid evidence for either
# directional candidate, even when it returns positive rates.
: > "$work/counter"
export AUTOTUNE_MOCK_RESULT_DIRECTION=both
"$autotune" wrongdirection lo start speedtest-go > "$work/wrongdirection-start.json"
attempt=0
while [ "$attempt" -lt 220 ]; do
	"$autotune" wrongdirection lo status speedtest-go > "$work/wrongdirection-status.json"
	grep -q 'reported both for the unshaped download-only control' "$work/wrongdirection-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q 'reported both for the unshaped download-only control' "$work/wrongdirection-status.json"
wait_for_job_cleanup wrongdirection
unset AUTOTUNE_MOCK_RESULT_DIRECTION

: > "$work/counter"
export AUTOTUNE_MOCK_CORRECT=1
"$autotune" corrected lo start speedtest-go > "$work/correct-start.json"
attempt=0
while [ "$attempt" -lt 600 ]; do
	"$autotune" corrected lo status speedtest-go > "$work/correct-status.json"
	grep -q '"state":"complete"' "$work/correct-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
node - "$work/correct-status.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const first = result.validation_attempts && result.validation_attempts[0];
if (result.state !== 'complete' || result.auto_apply_eligible !== true ||
    !first || first.pass !== false || first.safety_pass !== false ||
    first.profile_objectives_met !== false || first.correction.action !== 'retry-measurement' ||
    !first.reasons.some(reason => reason.code === 'download-candidate-realization') ||
    !first.warnings.some(warning => warning.code === 'download-throughput-safety-floor') ||
    result.validation.pass !== true || result.validation.profile_objectives_met !== true)
	throw new Error('realization did not block the unsafe candidate while trust remained advisory');
EOF
wait_for_job_cleanup corrected
unset AUTOTUNE_MOCK_CORRECT

# Fair is throughput-first but remains evidence-driven. When its class-C target
# conflicts with the 90% floor and a complete no-SQM control is no worse while
# materially faster in both directions, Review may offer an explicit disable
# action. It is deliberately not eligible for unattended application.
: > "$work/counter"
export AUTOTUNE_MOCK_FAIR_SHAPED_BAD=1
"$autotune" fairdisable lo start speedtest-go '' '' fair > "$work/fairdisable-start.json"
attempt=0
while [ "$attempt" -lt 260 ]; do
	"$autotune" fairdisable lo status speedtest-go '' '' fair > "$work/fairdisable-status.json"
	grep -q '"state":"complete"' "$work/fairdisable-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"profile":"fair"' "$work/fairdisable-status.json"
grep -q '"auto_apply_eligible":false' "$work/fairdisable-status.json"
grep -q '"manual_apply_eligible":true' "$work/fairdisable-status.json"
grep -q '"pass":false,"hard_pass":true,"safety_pass":true,"profile_objectives_met":true,"quality_target_met":false' "$work/fairdisable-status.json"
grep -q '"profile_outcome":{"mode":"throughput-optimum-quality-fallback"' "$work/fairdisable-status.json"
grep -q '"manual_only":true' "$work/fairdisable-status.json"
grep -q '"mode":"sqm-disable-recommended"' "$work/fairdisable-status.json"
grep -q '"recommended_action":"disable_sqm"' "$work/fairdisable-status.json"
grep -q '"allowed_actions":\["apply_sqm","keep_current","disable_sqm"\]' "$work/fairdisable-status.json"
grep -q '"apply_sqm_available":true' "$work/fairdisable-status.json"
grep -q '"disable_sqm_available":true' "$work/fairdisable-status.json"
node - "$work/fairdisable-status.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const control = result.fair_outcome.no_sqm_control;
if (!control.available || !control.measurement_evidence.valid ||
    control.measurement_evidence.test_direction !== 'both' ||
    !control.measurement_evidence.shaper_bypassed ||
    !control.measurement_evidence.sqm_paused ||
    !control.forwarded_background ||
    control.forwarded_background.available !== true ||
    control.forwarded_background.contaminated !== false ||
    control.forwarded_background.download_kbps > control.forwarded_background.download_limit_kbps ||
    control.forwarded_background.upload_kbps > control.forwarded_background.upload_limit_kbps)
	throw new Error('no-SQM recommendation lacks complete control evidence');
if (result.fair_outcome.throughput_gain_without_sqm.download_percent < 2 ||
    result.fair_outcome.throughput_gain_without_sqm.upload_percent < 2)
	throw new Error('no-SQM recommendation lacks bidirectional throughput gain');
if (result.configuration_written !== false)
	throw new Error('Fair calibration changed configuration');
EOF
wait_for_job_cleanup fairdisable
unset AUTOTUNE_MOCK_FAIR_SHAPED_BAD

# A candidate that is apparently exceeded by more than 10% did not prove that
# the temporary CAKE rate was enforced. Retry the same candidate once, then
# report retryable measurement uncertainty rather than a passing proposal.
: > "$work/counter"
export AUTOTUNE_MOCK_REALIZATION_OVERSHOOT=1
"$autotune" overshoot lo start speedtest-go > "$work/overshoot-start.json"
attempt=0
while [ "$attempt" -lt 260 ]; do
	"$autotune" overshoot lo status speedtest-go > "$work/overshoot-status.json"
	grep -q 'candidate-realization-too-high-after-bounded-retry' "$work/overshoot-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"inconclusive"' "$work/overshoot-status.json"
grep -q '"retryable":true' "$work/overshoot-status.json"
grep -q '"action":"retry-measurement"' "$work/overshoot-status.json"
grep -q '"code":"download-candidate-realization-maximum","scope":"download","required":true,"pass":false' "$work/overshoot-status.json"
grep -q '"configuration_written":false' "$work/overshoot-status.json"
wait_for_job_cleanup overshoot
unset AUTOTUNE_MOCK_REALIZATION_OVERSHOOT

# High CPU is evidence worth showing to the operator, but it is not proof that
# the candidate is unsafe. A reliable, low-latency calibration must remain
# applyable while retaining directional CPU warnings.
: > "$work/counter"
export AUTOTUNE_MOCK_COMPUTE_CEILING=1
"$autotune" cpuadvisory lo start speedtest-go > "$work/cpuadvisory-start.json"
attempt=0
while [ "$attempt" -lt 260 ]; do
	"$autotune" cpuadvisory lo status speedtest-go > "$work/cpuadvisory-status.json"
	grep -q '"state":"complete"' "$work/cpuadvisory-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
node - "$work/cpuadvisory-status.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
if (result.state !== 'complete' || result.validation.pass !== true ||
    result.validation.hard_pass !== true || result.validation.safety_pass !== true ||
    result.validation.correction.action !== 'none' ||
    result.auto_apply_eligible !== true || result.manual_apply_eligible !== true)
	throw new Error('advisory CPU load incorrectly blocked a reliable proposal');
for (const direction of ['download', 'upload']) {
	const gate = result.validation.gates.find(item => item.code === `${direction}-cpu`);
	if (!gate || gate.required !== false || gate.pass !== false)
		throw new Error(`${direction} CPU gate is not an advisory warning`);
	if (!result.validation.warnings.some(warning => warning.code === `${direction}-cpu`))
		throw new Error(`${direction} CPU warning was not retained`);
	if (result.validation.reasons.some(reason => reason.code === `${direction}-cpu`))
		throw new Error(`${direction} CPU warning leaked into failure reasons`);
	const search = result.profile_search[direction];
	if (!search || search.selected.safety_pass !== true ||
	    String(search.reason || '').includes('compute'))
		throw new Error(`${direction} search was incorrectly constrained by CPU load`);
}
EOF
wait_for_job_cleanup cpuadvisory
unset AUTOTUNE_MOCK_COMPUTE_CEILING

# Stable low-realization samples must make the optimizer step down and prove a
# controlled candidate. A resulting retention shortfall remains manual-only.
: > "$work/counter"
export AUTOTUNE_MOCK_REALIZATION_ADVISORY=1
"$autotune" lowrealization lo start speedtest-go > "$work/lowrealization-start.json"
attempt=0
while [ "$attempt" -lt 600 ]; do
	"$autotune" lowrealization lo status speedtest-go > "$work/lowrealization-status.json"
	grep -q '"state":"complete"' "$work/lowrealization-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
node - "$work/lowrealization-status.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
if (result.state !== 'complete' || result.auto_apply_eligible !== false ||
    result.manual_apply_eligible !== true || result.configuration_written !== false ||
    result.validation.safety_pass !== true ||
    result.validation.profile_objectives_met !== false)
	throw new Error(`controlled low-throughput evidence was not retained for manual review: ${JSON.stringify({
		state: result.state,
		auto: result.auto_apply_eligible,
		manual: result.manual_apply_eligible,
		written: result.configuration_written,
		safety: result.validation && result.validation.safety_pass,
		objectives: result.validation && result.validation.profile_objectives_met
	})}`);
for (const direction of ['download', 'upload']) {
	const search = result.profile_search[direction];
	if (!search || search.action !== 'complete' || search.selected.safety_pass !== true ||
	    search.selected.realization_percent < 80)
		throw new Error(`${direction} lacks a controlled low-throughput candidate`);
}
EOF
wait_for_job_cleanup lowrealization
unset AUTOTUNE_MOCK_REALIZATION_ADVISORY

# On Fair, a controlled result below the 50% historical-throughput trust
# boundary is still a manual-only proposal when latency/loss evidence is clean.
# High CPU and the old throughput comparison both remain advisory; neither may
# erase usable evidence or make the result Auto-Applyable.
: > "$work/counter"
export AUTOTUNE_MOCK_REALIZATION_LOW=1
export AUTOTUNE_MOCK_COMPUTE_CEILING=1
"$autotune" faircpuceiling lo start speedtest-go '' '' fair > "$work/faircpuceiling-start.json"
attempt=0
while [ "$attempt" -lt 900 ]; do
	"$autotune" faircpuceiling lo status speedtest-go '' '' fair > "$work/faircpuceiling-status.json"
	grep -q '"state":"complete"' "$work/faircpuceiling-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
node - "$work/faircpuceiling-status.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
if (result.state !== 'complete' || result.profile !== 'fair' ||
    result.validation.safety_pass !== true ||
    result.validation.profile_objectives_met !== false ||
    result.profile_outcome.mode !== 'latency-safe-throughput-advisory' ||
    result.profile_outcome.capacity_floor_met !== false ||
    result.auto_apply_eligible !== false || result.manual_apply_eligible !== true ||
    result.configuration_written !== false)
	throw new Error(`Fair controlled low-throughput evidence did not produce manual review: ${JSON.stringify({
		state: result.state,
		profile: result.profile,
		safety: result.validation && result.validation.safety_pass,
		objectives: result.validation && result.validation.profile_objectives_met,
		mode: result.profile_outcome && result.profile_outcome.mode,
		floor: result.profile_outcome && result.profile_outcome.capacity_floor_met,
		auto: result.auto_apply_eligible,
		manual: result.manual_apply_eligible,
		written: result.configuration_written
	})}`);
for (const direction of ['download', 'upload']) {
	const search = result.profile_search[direction];
	if (search.action !== 'complete' || search.selected.safety_pass !== true ||
	    search.selected.realization_percent < 80)
		throw new Error(`${direction} lacks a proven controlled candidate`);
}
if (!result.validation.warnings.some(warning => warning.code === 'download-cpu') ||
    !result.validation.warnings.some(warning => warning.code === 'upload-cpu') ||
    !result.validation.warnings.some(warning => warning.code === 'download-throughput-safety-floor') ||
    !result.validation.warnings.some(warning => warning.code === 'upload-throughput-safety-floor'))
	throw new Error('CPU or historical-throughput evidence was not retained as an advisory warning');
EOF
wait_for_job_cleanup faircpuceiling
unset AUTOTUNE_MOCK_REALIZATION_LOW AUTOTUNE_MOCK_COMPUTE_CEILING

# Shaped evidence must explicitly attest that the speed-test helper neither
# bypassed the candidate nor paused its temporary CAKE path.
: > "$work/counter"
export AUTOTUNE_MOCK_SHAPED_PROOF_INVALID=1
"$autotune" invalidshaperproof lo start speedtest-go > "$work/invalidshaperproof-start.json"
attempt=0
while [ "$attempt" -lt 260 ]; do
	"$autotune" invalidshaperproof lo status speedtest-go > "$work/invalidshaperproof-status.json"
	grep -q 'shaped-shaper-proof-invalid' "$work/invalidshaperproof-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"inconclusive"' "$work/invalidshaperproof-status.json"
grep -q '"retryable":true' "$work/invalidshaperproof-status.json"
grep -q '"configuration_written":false' "$work/invalidshaperproof-status.json"
wait_for_job_cleanup invalidshaperproof
unset AUTOTUNE_MOCK_SHAPED_PROOF_INVALID

# Helper flags and plausible rates are insufficient if the exact temporary
# CAKE/IFB ownership disappears while the shaped speed test is running.  The
# post-test ownership proof must reject the JSON before any evidence is parsed.
: > "$work/counter"
export AUTOTUNE_MOCK_SHAPER_OWNERSHIP_LOST_MARKER="$work/shaper-ownership-lost"
export AUTOTUNE_MOCK_SHAPER_LOSE_DURING_SPEEDTEST=1
rm -f "$AUTOTUNE_MOCK_SHAPER_OWNERSHIP_LOST_MARKER"
"$autotune" lostshaperownership lo start speedtest-go > "$work/lostshaperownership-start.json"
attempt=0
while [ "$attempt" -lt 260 ]; do
	"$autotune" lostshaperownership lo status speedtest-go > "$work/lostshaperownership-status.json"
	grep -q 'temporary-shaper-postcondition-failed' "$work/lostshaperownership-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"inconclusive"' "$work/lostshaperownership-status.json"
grep -q '"stage":"shaped-download"' "$work/lostshaperownership-status.json"
grep -q '"retryable":true' "$work/lostshaperownership-status.json"
grep -q '"configuration_written":false' "$work/lostshaperownership-status.json"
if grep -q '"state":"complete"' "$work/lostshaperownership-status.json"; then exit 1; fi
wait_for_job_cleanup lostshaperownership
unset AUTOTUNE_MOCK_SHAPER_OWNERSHIP_LOST_MARKER AUTOTUNE_MOCK_SHAPER_LOSE_DURING_SPEEDTEST

# Ownership can also disappear after a phase passed its postcondition but
# before the next direction starts.  The next shaped helper must never launch.
: > "$work/counter"
: > "$work/shaper-ownership-checks"
export AUTOTUNE_MOCK_SHAPER_OWNERSHIP_LOST_MARKER="$work/shaper-ownership-between-phases-lost"
export AUTOTUNE_MOCK_SHAPER_OWNERSHIP_CHECK_COUNTER="$work/shaper-ownership-checks"
export AUTOTUNE_MOCK_SHAPER_INVALIDATE_AFTER_CHECK=2
rm -f "$AUTOTUNE_MOCK_SHAPER_OWNERSHIP_LOST_MARKER"
"$autotune" preshaperownership lo start speedtest-go > "$work/preshaperownership-start.json"
attempt=0
while [ "$attempt" -lt 260 ]; do
	"$autotune" preshaperownership lo status speedtest-go > "$work/preshaperownership-status.json"
	grep -q 'temporary-shaper-precondition-failed' "$work/preshaperownership-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"inconclusive"' "$work/preshaperownership-status.json"
grep -q '"stage":"shaped-upload"' "$work/preshaperownership-status.json"
grep -q '"retryable":true' "$work/preshaperownership-status.json"
grep -q '"configuration_written":false' "$work/preshaperownership-status.json"
if grep -q '"state":"complete"' "$work/preshaperownership-status.json"; then exit 1; fi
# One bidirectional and four direction-matched raw controls precede the first
# shaped download. The upload helper must not become the seventh invocation.
test "$(sed -n '1p' "$work/counter")" = 6
wait_for_job_cleanup preshaperownership
unset AUTOTUNE_MOCK_SHAPER_OWNERSHIP_LOST_MARKER
unset AUTOTUNE_MOCK_SHAPER_OWNERSHIP_CHECK_COUNTER AUTOTUNE_MOCK_SHAPER_INVALIDATE_AFTER_CHECK

# A loaded-only rate-limited reflector cannot replace the accepted idle pool:
# establishing a new baseline requires a fresh quiet calibration run.
: > "$work/counter"
: > "$work/fping-restart.calls"
export AUTOTUNE_MOCK_ICMP_RATE_LIMIT=1
export AUTOTUNE_MOCK_FPING_CALLS="$work/fping-restart.calls"
"$autotune" ratelimitedreflector lo start speedtest-go > "$work/ratelimitedreflector-start.json"
attempt=0
while [ "$attempt" -lt 260 ]; do
	"$autotune" ratelimitedreflector lo status speedtest-go > "$work/ratelimitedreflector-status.json"
	grep -q 'icmp-rate-limit-suspected:1.1.1.1-restart-required' "$work/ratelimitedreflector-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"inconclusive"' "$work/ratelimitedreflector-status.json"
grep -q '"auto_apply_eligible":false' "$work/ratelimitedreflector-status.json"
test "$(wc -l < "$work/fping-restart.calls" | tr -d ' ')" = 1
wait_for_job_cleanup ratelimitedreflector
unset AUTOTUNE_MOCK_ICMP_RATE_LIMIT AUTOTUNE_MOCK_FPING_CALLS

# A clean, safety-floor-passing capacity shortfall tests the raw upper bound
# directly. The independent typed searches must not leak the DL scale into UL
# evidence or treat the advisory profile objective as a safety failure.
: > "$work/counter"
export AUTOTUNE_MOCK_DIRECTIONAL_CORRECT=1
"$autotune" directional lo start speedtest-go > "$work/directional-start.json"
attempt=0
while [ "$attempt" -lt 220 ]; do
	"$autotune" directional lo status speedtest-go > "$work/directional-status.json"
	grep -q '"state":"complete"' "$work/directional-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"complete"' "$work/directional-status.json"
node - "$work/directional-status.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const dl = result.profile_search.download.evaluated;
const ul = result.profile_search.upload.evaluated;
if (JSON.stringify(dl.map(item => item.candidate_kbps)) !==
      JSON.stringify([42500, 50000]) ||
    JSON.stringify(ul.map(item => item.candidate_kbps)) !==
      JSON.stringify([8800, 10000]) ||
    dl[0].safety_pass !== true || dl[0].capacity_objective_met !== false ||
    ul[0].safety_pass !== true ||
    result.validation.candidate_base.download_kbps !== 50000 ||
    result.validation.candidate_base.upload_kbps !== 10000)
	throw new Error('directional profile search crossed DL and UL candidates');
EOF
wait_for_job_cleanup directional
unset AUTOTUNE_MOCK_DIRECTIONAL_CORRECT

: > "$work/counter"
export AUTOTUNE_MOCK_ROUTE_MISMATCH=1
"$autotune" routebad lo start speedtest-go > "$work/routebad-start.json"
attempt=0
while [ "$attempt" -lt 220 ]; do
	"$autotune" routebad lo status speedtest-go > "$work/routebad-status.json"
	grep -q '"state":"failed"' "$work/routebad-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"failed"' "$work/routebad-status.json"
grep -q 'route identity changed' "$work/routebad-status.json"
wait_for_job_cleanup routebad
unset AUTOTUNE_MOCK_ROUTE_MISMATCH

# If an optional detailed fragment becomes unavailable during a real route
# loss, fail() must preserve the original reason in a compact terminal instead
# of leaving recovery to report only an unstructured worker interruption.
compact_failure_marker="$work/compact-failure-terminal.json"
set +e
(
	export CAKE_AUTORATE_AUTOTUNE_SOURCE_ONLY=1
	# shellcheck disable=SC1090
	. "$autotune"
	compact_stage_attempt=0
	stage_terminal_json() {
		compact_stage_attempt=$((compact_stage_attempt + 1))
		[ "$compact_stage_attempt" -gt 1 ] || return 1
		printf '%s\n' "$2" > "$compact_failure_marker"
		return 0
	}
	worker_trace() { :; }
	cancel_file="$work/compact-failure-cancelled"
	current_phase=shaped
	phase_contamination_seen=false
	fail "Selected route went offline during calibration."
)
compact_failure_rc="$?"
set -e
test "$compact_failure_rc" -eq 1
grep -q '"stage":"shaped"' "$compact_failure_marker"
grep -q 'Selected route went offline during calibration' "$compact_failure_marker"
grep -q '"phase_evidence_complete":false' "$compact_failure_marker"

# Auto must resolve once to a concrete backend. Backends without a pinnable
# server identity and any backend drift in a result fail before proposal math.
: > "$work/counter"
export AUTOTUNE_MOCK_SELECTED_BACKEND=librespeed-cli
"$autotune" backendunsupported lo start librespeed-cli > "$work/backendunsupported-start.json"
attempt=0
while [ "$attempt" -lt 220 ]; do
	"$autotune" backendunsupported lo status librespeed-cli > "$work/backendunsupported-status.json"
	grep -q 'pinnable, verifiable server identity' "$work/backendunsupported-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q 'pinnable, verifiable server identity' "$work/backendunsupported-status.json" || {
	cat "$work/backendunsupported-status.json" >&2
	cat "$work/jobs/backendunsupported/job.log" >&2 || true
	exit 1
}
wait_for_job_cleanup backendunsupported
unset AUTOTUNE_MOCK_SELECTED_BACKEND

# Even when generic auto would prefer librespeed-cli, Full Auto-Tune asks for
# speedtest-go directly and therefore keeps its pinnable server semantics.
: > "$work/counter"
export AUTOTUNE_MOCK_AUTO_SELECTED_BACKEND=librespeed-cli
export AUTOTUNE_MOCK_STATUS_BACKEND_LOG="$work/status-backends"
"$autotune" autoresolve lo start auto > "$work/autoresolve-start.json"
attempt=0
while [ "$attempt" -lt 260 ]; do
	"$autotune" autoresolve lo status auto > "$work/autoresolve-status.json"
	grep -q '"state":"complete"' "$work/autoresolve-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"complete"' "$work/autoresolve-status.json"
grep -qx speedtest-go "$work/status-backends"
wait_for_job_cleanup autoresolve
unset AUTOTUNE_MOCK_AUTO_SELECTED_BACKEND AUTOTUNE_MOCK_STATUS_BACKEND_LOG

: > "$work/counter"
export AUTOTUNE_MOCK_RESULT_BACKEND=librespeed-cli
"$autotune" backenddrift lo start auto > "$work/backenddrift-start.json"
attempt=0
while [ "$attempt" -lt 220 ]; do
	"$autotune" backenddrift lo status auto > "$work/backenddrift-status.json"
	grep -q 'backend changed from speedtest-go' "$work/backenddrift-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q 'backend changed from speedtest-go' "$work/backenddrift-status.json"
wait_for_job_cleanup backenddrift
unset AUTOTUNE_MOCK_RESULT_BACKEND

: > "$work/counter"
export AUTOTUNE_MOCK_MISSING_SERVER_ID=1
"$autotune" noserver lo start auto > "$work/noserver-start.json"
attempt=0
while [ "$attempt" -lt 220 ]; do
	"$autotune" noserver lo status auto > "$work/noserver-status.json"
	grep -q 'server identity was missing' "$work/noserver-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q 'server identity was missing' "$work/noserver-status.json"
wait_for_job_cleanup noserver
unset AUTOTUNE_MOCK_MISSING_SERVER_ID

# Flow offload can bypass an inet/forward counter entirely, so calibration is
# rejected before the first baseline rather than treating zero as evidence.
: > "$work/counter"
export AUTOTUNE_MOCK_NFT_FLOWTABLE=1
"$autotune" flowoffload lo start auto > "$work/flowoffload-start.json"
attempt=0
while [ "$attempt" -lt 220 ]; do
	"$autotune" flowoffload lo status auto > "$work/flowoffload-status.json"
	grep -q 'flowtable/offload' "$work/flowoffload-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q 'flowtable/offload' "$work/flowoffload-status.json"
wait_for_job_cleanup flowoffload
unset AUTOTUNE_MOCK_NFT_FLOWTABLE

# The full ICMP+transport baseline is retried as one unit when its own forward
# counter sees traffic. A clean retry remains reviewable but not auto-applyable.
: > "$work/counter"
export CAKE_AUTORATE_AUTOTUNE_TEST_BASELINE_BACKGROUND_FIRST_DL_KBPS=999999
"$autotune" baselineretry lo start speedtest-go > "$work/baselineretry-start.json"
attempt=0
while [ "$attempt" -lt 260 ]; do
	"$autotune" baselineretry lo status speedtest-go > "$work/baselineretry-status.json"
	grep -q '"state":"complete"' "$work/baselineretry-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"complete"' "$work/baselineretry-status.json"
grep -q '"phase":"baseline","attempt":1' "$work/baselineretry-status.json"
grep -q '"phase":"baseline","attempt":2' "$work/baselineretry-status.json"
grep -q '"phase_contamination_seen":true' "$work/baselineretry-status.json"
grep -q '"auto_apply_eligible":false' "$work/baselineretry-status.json"
wait_for_job_cleanup baselineretry
unset CAKE_AUTORATE_AUTOTUNE_TEST_BASELINE_BACKGROUND_FIRST_DL_KBPS

: > "$work/counter"
export CAKE_AUTORATE_AUTOTUNE_TEST_BASELINE_BACKGROUND_DL_KBPS=999999
export AUTOTUNE_MOCK_CONSERVATIVE_PHASE=1
"$autotune" baselineblocked lo start-conservative speedtest-go > "$work/baselineblocked-start.json"
attempt=0
while [ "$attempt" -lt 800 ]; do
	"$autotune" baselineblocked lo status speedtest-go > "$work/baselineblocked-status.json"
	grep -q '"state":"complete"' "$work/baselineblocked-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"complete"' "$work/baselineblocked-status.json"
grep -q '"result_class":"estimated"' "$work/baselineblocked-status.json"
grep -q '"capacity_download_percent":25' "$work/baselineblocked-status.json"
grep -q '"quality_percent":55' "$work/baselineblocked-status.json"
grep -q '"manual_apply_eligible":true' "$work/baselineblocked-status.json"
wait_for_job_cleanup baselineblocked
unset CAKE_AUTORATE_AUTOTUNE_TEST_BASELINE_BACKGROUND_DL_KBPS AUTOTUNE_MOCK_CONSERVATIVE_PHASE

: > "$work/counter"
export AUTOTUNE_MOCK_FPING_SPARSE_FIRST_ONLY=1
export AUTOTUNE_MOCK_FPING_SPARSE_MARKER="$work/sparse-first.marker"
"$autotune" sparsefirst lo start speedtest-go > "$work/sparsefirst-start.json"
attempt=0
while [ "$attempt" -lt 260 ]; do
	"$autotune" sparsefirst lo status speedtest-go > "$work/sparsefirst-status.json"
	grep -q '"state":"complete"' "$work/sparsefirst-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"complete"' "$work/sparsefirst-status.json"
grep -q '"phase":"baseline","attempt":1[^}]*"icmp_valid":false' "$work/sparsefirst-status.json"
grep -q '"phase":"baseline","attempt":2[^}]*"icmp_valid":true' "$work/sparsefirst-status.json"
grep -q '"phase_contamination_seen":false' "$work/sparsefirst-status.json"
grep -q '"auto_apply_eligible":true' "$work/sparsefirst-status.json"
wait_for_job_cleanup sparsefirst
unset AUTOTUNE_MOCK_FPING_SPARSE_FIRST_ONLY AUTOTUNE_MOCK_FPING_SPARSE_MARKER

: > "$work/counter"
export AUTOTUNE_MOCK_FPING_SPARSE_REFLECTOR=1
"$autotune" sparsebaseline lo start speedtest-go > "$work/sparsebaseline-start.json"
attempt=0
while [ "$attempt" -lt 220 ]; do
	"$autotune" sparsebaseline lo status speedtest-go > "$work/sparsebaseline-status.json"
	grep -q 'icmp-insufficient-per-reflector-baseline' "$work/sparsebaseline-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"inconclusive"' "$work/sparsebaseline-status.json"
grep -q 'icmp-insufficient-per-reflector-baseline' "$work/sparsebaseline-status.json"
wait_for_job_cleanup sparsebaseline
unset AUTOTUNE_MOCK_FPING_SPARSE_REFLECTOR

: > "$work/counter"
export AUTOTUNE_MOCK_TRANSPORT_UNTRUSTED=1
"$autotune" untrustedtransport lo start speedtest-go > "$work/untrustedtransport-start.json"
attempt=0
while [ "$attempt" -lt 220 ]; do
	"$autotune" untrustedtransport lo status speedtest-go > "$work/untrustedtransport-status.json"
	grep -q 'transport-untrusted' "$work/untrustedtransport-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"inconclusive"' "$work/untrustedtransport-status.json"
grep -q '"reason":"transport-untrusted"' "$work/untrustedtransport-status.json"
grep -q '"configuration_written":false' "$work/untrustedtransport-status.json"
test "$(sed -n '1p' "$work/counter")" = ""
wait_for_job_cleanup untrustedtransport
unset AUTOTUNE_MOCK_TRANSPORT_UNTRUSTED

: > "$work/counter"
export CAKE_AUTORATE_AUTOTUNE_BACKGROUND_DL_KBPS=2500
export CAKE_AUTORATE_AUTOTUNE_BACKGROUND_UL_KBPS=1500
"$autotune" background lo start speedtest-go > "$work/background-start.json"
attempt=0
while [ "$attempt" -lt 220 ]; do
	"$autotune" background lo status speedtest-go > "$work/background-status.json"
	grep -q '"state":"background-blocked"' "$work/background-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"background_blocked":true' "$work/background-status.json"
grep -q '"retryable":true' "$work/background-status.json"
grep -q '"conservative_available":true' "$work/background-status.json"
grep -q '"download_kbps":2500' "$work/background-status.json"
wait_for_job_cleanup background

: > "$work/counter"
export AUTOTUNE_MOCK_CONSERVATIVE_PHASE=1
"$autotune" background lo start-conservative speedtest-go > "$work/background-conservative-start.json"
attempt=0
while [ "$attempt" -lt 800 ]; do
	"$autotune" background lo status speedtest-go > "$work/background-conservative-status.json"
	grep -q '"state":"complete"' "$work/background-conservative-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"complete"' "$work/background-conservative-status.json"
grep -q '"result_class":"provisional"' "$work/background-conservative-status.json"
grep -q '"manual_apply_eligible":true' "$work/background-conservative-status.json"
grep -q '"quality_percent":55' "$work/background-conservative-status.json"
grep -q '"code":"baseline-background"' "$work/background-conservative-status.json"
grep -q '"configuration_written":false' "$work/background-conservative-status.json"
wait_for_job_cleanup background
unset CAKE_AUTORATE_AUTOTUNE_BACKGROUND_DL_KBPS CAKE_AUTORATE_AUTOTUNE_BACKGROUND_UL_KBPS \
	AUTOTUNE_MOCK_CONSERVATIVE_PHASE

# A new instance has no configured capacity.  Its idle baseline is provisional
# until the first routed unshaped control supplies a real direction-specific
# reference.  Eight Mbit/s is acceptable on this mocked ~900 Mbit/s link and
# must not be compared with the old synthetic 20 Mbit/s fallback.
unset AUTOTUNE_MOCK_CONFIGURED_DL_KBPS AUTOTUNE_MOCK_CONFIGURED_UL_KBPS
: > "$work/counter"
export AUTOTUNE_MOCK_FAIR_BOUNDARY=1
export CAKE_AUTORATE_AUTOTUNE_BACKGROUND_DL_KBPS=8000
export CAKE_AUTORATE_AUTOTUNE_BACKGROUND_UL_KBPS=8000
export CAKE_AUTORATE_AUTOTUNE_TEST_BASELINE_BACKGROUND_DL_KBPS=8000
export CAKE_AUTORATE_AUTOTUNE_TEST_BASELINE_BACKGROUND_UL_KBPS=8000
"$autotune" deferredhigh lo start speedtest-go '' '' fair > "$work/deferredhigh-start.json"
attempt=0
while [ "$attempt" -lt 800 ]; do
	"$autotune" deferredhigh lo status speedtest-go '' '' fair > "$work/deferredhigh-status.json"
	grep -q '"state":"complete"' "$work/deferredhigh-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"complete"' "$work/deferredhigh-status.json"
grep -q '"phase":"baseline-retrospective","passed":true' "$work/deferredhigh-status.json"
grep -q '"reference_deferred":true' "$work/deferredhigh-status.json"
grep -q '"download_reference_kbps":902700' "$work/deferredhigh-status.json"
grep -q '"upload_reference_kbps":900000' "$work/deferredhigh-status.json"
wait_for_job_cleanup deferredhigh
unset AUTOTUNE_MOCK_FAIR_BOUNDARY

# When a new instance had no initial capacity reference, retrospective scoring
# must not leave its previously deferred background share at zero. A 90%
# baseline remains measurable after explicit conservative consent, but is an
# estimated result with low direction-specific capacity confidence.
: > "$work/counter"
export CAKE_AUTORATE_AUTOTUNE_BACKGROUND_DL_KBPS=45000
export CAKE_AUTORATE_AUTOTUNE_BACKGROUND_UL_KBPS=9000
export CAKE_AUTORATE_AUTOTUNE_TEST_BASELINE_BACKGROUND_DL_KBPS=45000
export CAKE_AUTORATE_AUTOTUNE_TEST_BASELINE_BACKGROUND_UL_KBPS=9000
export AUTOTUNE_MOCK_CONSERVATIVE_PHASE=1
"$autotune" deferredestimated lo start-conservative speedtest-go > "$work/deferredestimated-start.json"
attempt=0
while [ "$attempt" -lt 800 ]; do
	"$autotune" deferredestimated lo status speedtest-go > "$work/deferredestimated-status.json"
	grep -q '"state":"complete"' "$work/deferredestimated-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"complete"' "$work/deferredestimated-status.json"
grep -q '"phase":"baseline-retrospective","passed":false' "$work/deferredestimated-status.json"
grep -q '"download_share_percent":90.0' "$work/deferredestimated-status.json"
grep -q '"upload_share_percent":90.0' "$work/deferredestimated-status.json"
grep -q '"result_class":"estimated"' "$work/deferredestimated-status.json"
grep -q '"capacity_download_percent":25' "$work/deferredestimated-status.json"
grep -q '"capacity_upload_percent":25' "$work/deferredestimated-status.json"
grep -q '"manual_apply_eligible":true' "$work/deferredestimated-status.json"
grep -q '"code":"baseline-background"' "$work/deferredestimated-status.json"
grep -q '"code":"background-contamination-accepted"' "$work/deferredestimated-status.json"
wait_for_job_cleanup deferredestimated
unset AUTOTUNE_MOCK_CONSERVATIVE_PHASE
export CAKE_AUTORATE_AUTOTUNE_BACKGROUND_DL_KBPS=8000
export CAKE_AUTORATE_AUTOTUNE_BACKGROUND_UL_KBPS=8000
export CAKE_AUTORATE_AUTOTUNE_TEST_BASELINE_BACKGROUND_DL_KBPS=8000
export CAKE_AUTORATE_AUTOTUNE_TEST_BASELINE_BACKGROUND_UL_KBPS=8000

# The same provisional traffic is unsafe on the default 50/10 Mbit/s fixture.
# Retrospective validation must fail at the baseline stage before a proposal.
: > "$work/counter"
"$autotune" deferredlow lo start speedtest-go > "$work/deferredlow-start.json"
attempt=0
while [ "$attempt" -lt 260 ]; do
	"$autotune" deferredlow lo status speedtest-go > "$work/deferredlow-status.json"
	grep -q 'baseline-retrospective' "$work/deferredlow-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"background-blocked"' "$work/deferredlow-status.json"
grep -q '"stage":"baseline-retrospective"' "$work/deferredlow-status.json"
grep -q '"conservative_available":true' "$work/deferredlow-status.json"
grep -q '"manual_apply_eligible":false' "$work/deferredlow-status.json"
grep -q '"phase":"baseline-retrospective","passed":false' "$work/deferredlow-status.json"
wait_for_job_cleanup deferredlow
unset CAKE_AUTORATE_AUTOTUNE_BACKGROUND_DL_KBPS CAKE_AUTORATE_AUTOTUNE_BACKGROUND_UL_KBPS
unset CAKE_AUTORATE_AUTOTUNE_TEST_BASELINE_BACKGROUND_DL_KBPS CAKE_AUTORATE_AUTOTUNE_TEST_BASELINE_BACKGROUND_UL_KBPS
export AUTOTUNE_MOCK_CONFIGURED_DL_KBPS=50000
export AUTOTUNE_MOCK_CONFIGURED_UL_KBPS=10000

# Forwarded client traffic is accounted independently in every heavy phase.
# Normal mode fails closed after one retry; explicit conservative mode may
# still show a low-confidence proposal but can never become auto-applyable.
: > "$work/counter"
export CAKE_AUTORATE_AUTOTUNE_TEST_PHASE_BACKGROUND_DL_KBPS=999999
export CAKE_AUTORATE_AUTOTUNE_TEST_PHASE_BACKGROUND_UL_KBPS=999999
"$autotune" phaseblocked lo start speedtest-go > "$work/phaseblocked-start.json"
attempt=0
while [ "$attempt" -lt 220 ]; do
	"$autotune" phaseblocked lo status speedtest-go > "$work/phaseblocked-status.json"
	grep -q '"state":"background-blocked"' "$work/phaseblocked-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"background-blocked"' "$work/phaseblocked-status.json"
grep -q '"stage":"throughput"' "$work/phaseblocked-status.json"
grep -q '"phase_contamination_seen":true' "$work/phaseblocked-status.json"
grep -q '"auto_apply_eligible":false' "$work/phaseblocked-status.json"
wait_for_job_cleanup phaseblocked
test ! -e "$work/jobs/phaseblocked/status.json"

: > "$work/counter"
export AUTOTUNE_MOCK_CONSERVATIVE_PHASE=1
"$autotune" phaseconservative lo start-conservative speedtest-go > "$work/phaseconservative-start.json"
attempt=0
while [ "$attempt" -lt 600 ]; do
	"$autotune" phaseconservative lo status speedtest-go > "$work/phaseconservative-status.json"
	grep -q '"state":"complete"' "$work/phaseconservative-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"complete"' "$work/phaseconservative-status.json"
grep -q '"phase_contamination_seen":true' "$work/phaseconservative-status.json"
grep -q '"contaminated":true' "$work/phaseconservative-status.json"
grep -q '"auto_apply_eligible":false' "$work/phaseconservative-status.json"
grep -q '"manual_apply_eligible":true' "$work/phaseconservative-status.json"
grep -q '"result_class":"estimated"' "$work/phaseconservative-status.json"
grep -q '"overall_percent":25' "$work/phaseconservative-status.json"
grep -q '"confidence_mode":"low"' "$work/phaseconservative-status.json"
wait_for_job_cleanup phaseconservative
unset AUTOTUNE_MOCK_CONSERVATIVE_PHASE
unset CAKE_AUTORATE_AUTOTUNE_TEST_PHASE_BACKGROUND_DL_KBPS CAKE_AUTORATE_AUTOTUNE_TEST_PHASE_BACKGROUND_UL_KBPS

# Conservative consent may accept measured background, but never missing
# telemetry. Two unavailable observations must remain a terminal failure with
# the baseline and per-phase evidence retained for Review diagnostics.
: > "$work/counter"
export CAKE_AUTORATE_AUTOTUNE_TEST_PHASE_BACKGROUND_UNAVAILABLE=1
"$autotune" phaseunknown lo start-conservative speedtest-go > "$work/phaseunknown-start.json"
attempt=0
while [ "$attempt" -lt 220 ]; do
	"$autotune" phaseunknown lo status speedtest-go > "$work/phaseunknown-status.json"
	grep -q '"state":"failed"' "$work/phaseunknown-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"failed"' "$work/phaseunknown-status.json"
grep -q 'Forwarded-traffic evidence was unavailable' "$work/phaseunknown-status.json"
grep -q '"phase_evidence_complete":false' "$work/phaseunknown-status.json"
grep -q '"baseline":{' "$work/phaseunknown-status.json"
grep -q '"phase_background":\[' "$work/phaseunknown-status.json"
grep -q '"auto_apply_eligible":false' "$work/phaseunknown-status.json"
wait_for_job_cleanup phaseunknown
test ! -e "$work/jobs/phaseunknown/status.json"
unset CAKE_AUTORATE_AUTOTUNE_TEST_PHASE_BACKGROUND_UNAVAILABLE

export AUTOTUNE_MOCK_BLOCK=1
export AUTOTUNE_MOCK_RESTORE_MARKER="$work/restored"
export AUTOTUNE_MOCK_BLOCK_STARTED="$work/block-started"
"$autotune" cancelled lo start speedtest-go > "$work/cancel-start.json"

attempt=0
while [ "$attempt" -lt 220 ]; do
	"$autotune" cancelled lo status speedtest-go > "$work/cancel-status.json"
	[ -e "$AUTOTUNE_MOCK_BLOCK_STARTED" ] && break
	attempt=$((attempt + 1))
	sleep 0.05
done
[ -e "$AUTOTUNE_MOCK_BLOCK_STARTED" ]

# A live job is bound to its immutable target/backend/route tuple. A second
# tab must not attach to it or launch another heavy test under the same name.
if "$autotune" cancelled eth9 start speedtest-go > "$work/mismatched-live-start.json" 2>/dev/null; then
	echo 'mismatched live Auto-Tune request unexpectedly attached to the existing worker' >&2
	exit 1
fi
grep -q 'different Full Auto-Tune request is already running' "$work/mismatched-live-start.json"
if "$autotune" cancelled eth9 status speedtest-go > "$work/mismatched-live-status.json" 2>/dev/null; then
	echo 'mismatched live Auto-Tune status unexpectedly exposed the existing worker' >&2
	exit 1
fi
grep -q 'status was not attached to this uplink' "$work/mismatched-live-status.json"
kill -0 "$(sed -n '1p' "$work/jobs/cancelled/pid")"

"$autotune" cancelled lo cancel speedtest-go > "$work/cancel.json"
attempt=0
while [ "$attempt" -lt 220 ]; do
	"$autotune" cancelled lo status speedtest-go > "$work/cancel-status.json"
	grep -q '"state":"cancelled"' "$work/cancel-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done

grep -q '"state":"cancelled"' "$work/cancel-status.json"
test -f "$work/restored"
test ! -e "$work/jobs/cancelled/http.running"
wait_for_job_cleanup cancelled
unset AUTOTUNE_MOCK_BLOCK

# Cancellation remains armed after a successful speedtest helper has returned;
# the second phase blocks and must still be terminated/restored as a group.
: > "$work/counter"
export AUTOTUNE_MOCK_BLOCK_AT_COUNT=2
export AUTOTUNE_MOCK_RESTORE_MARKER="$work/restored-after-helper"
export AUTOTUNE_MOCK_BLOCK_STARTED="$work/block-after-helper-started"
"$autotune" cancelledafter lo start speedtest-go > "$work/cancel-after-start.json"
attempt=0
while [ "$attempt" -lt 260 ]; do
	[ -e "$AUTOTUNE_MOCK_BLOCK_STARTED" ] && break
	attempt=$((attempt + 1))
	sleep 0.05
done
[ -e "$AUTOTUNE_MOCK_BLOCK_STARTED" ]
"$autotune" cancelledafter lo cancel speedtest-go > "$work/cancel-after.json"
attempt=0
while [ "$attempt" -lt 220 ]; do
	"$autotune" cancelledafter lo status speedtest-go > "$work/cancel-after-status.json"
	grep -q '"state":"cancelled"' "$work/cancel-after-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"cancelled"' "$work/cancel-after-status.json"
test -f "$work/restored-after-helper"
wait_for_job_cleanup cancelledafter
unset AUTOTUNE_MOCK_BLOCK_AT_COUNT AUTOTUNE_MOCK_RESTORE_MARKER AUTOTUNE_MOCK_BLOCK_STARTED

# Exercise the real monitor helpers rather than only the fixed shaped-test
# fixture.  One invocation per second is the safety contract; -p on fping is
# not sufficient when each process sends only one packet.
export CAKE_AUTORATE_AUTOTUNE_SOURCE_ONLY=1
. "$autotune"
unset CAKE_AUTORATE_AUTOTUNE_SOURCE_ONLY

# Marker-driven monitors normally exit by themselves.  If the child vanishes
# between the first state check and the pre-signal identity guard, cleanup must
# succeed without signalling a missing or PID-reused process.  This used to
# reject otherwise valid repeated Auto-Tune runs under load.
CAKE_AUTORATE_AUTOTUNE_SOURCE_ONLY=1 sh -c '
	. "$1"
	monitor_calls="$2/monitor-starttime.calls"
	monitor_kill="$2/monitor-kill.called"
	: > "$monitor_calls"
	proc_starttime() {
		call_count="$(wc -l < "$monitor_calls" | tr -d " ")"
		printf "call\n" >> "$monitor_calls"
		[ "$call_count" = 0 ] || return 1
		printf "111\n"
	}
	proc_state() { printf "R\n"; }
	kill() { : > "$monitor_kill"; return 1; }
	stop_owned_monitor 4242 111
	[ ! -e "$monitor_kill" ]
' sh "$autotune" "$work"

# A reused PID also proves that the original monitor is gone.  It must not be
# killed and must not poison the completed calibration result.
CAKE_AUTORATE_AUTOTUNE_SOURCE_ONLY=1 sh -c '
	. "$1"
	monitor_kill="$2/monitor-reused-kill.called"
	proc_starttime() { printf "222\n"; }
	proc_state() { printf "R\n"; }
	kill() { : > "$monitor_kill"; return 1; }
	stop_owned_monitor 4242 111
	[ ! -e "$monitor_kill" ]
' sh "$autotune" "$work"

# The tracked transport PID must be the native probe itself.  A background ash
# wrapper would die while leaving its long-running child to contaminate later
# validation attempts and accelerate PID reuse.
transport_monitor_fixture="$work/transport-monitor-fixture"
printf '%s\n' \
	'#!/bin/sh' \
	'printf "%s\n" "$$" > "$AUTOTUNE_MONITOR_PID_FILE"' \
	'exec sleep 30' > "$transport_monitor_fixture"
chmod +x "$transport_monitor_fixture"
transport_probe_bin="$transport_monitor_fixture"
transport_probe_backend=websocket
transport_probe_endpoint=wss://example.test/ws
transport_probe_timeout_s=5
target_if=lo
source_ip=127.0.0.1
route_mark=""
export AUTOTUNE_MONITOR_PID_FILE="$work/transport-monitor.pid"
start_transport_latency_monitor "$work/transport-monitor.raw" "$work/transport-monitor.errors"
monitor_ready_attempt=0
while [ ! -s "$AUTOTUNE_MONITOR_PID_FILE" ] && [ "$monitor_ready_attempt" -lt 50 ]; do
	monitor_ready_attempt=$((monitor_ready_attempt + 1))
	sleep 0.02
done
test "$(sed -n '1p' "$AUTOTUNE_MONITOR_PID_FILE")" = "$transport_pid"
stopped_transport_pid="$transport_pid"
stop_transport_latency_monitor
if kill -0 "$stopped_transport_pid" 2>/dev/null; then
	echo 'transport monitor remained after owned cleanup' >&2
	exit 1
fi
unset AUTOTUNE_MONITOR_PID_FILE

# Missing an old 5G capacity comparison does not make a currently controlled
# Fair candidate unsafe and must never produce a disable-SQM recommendation.
profile_safety_floor_met=false
validation_actual_grade=C
effective_loaded_delta_ms=120
shaped_dl=40000
shaped_ul=8000
fair_control_json='{"available":true,"grade":"C","effective_delta_ms":125,"throughput":{"download_kbps":50000,"upload_kbps":10000},"forwarded_background":{"available":true,"contaminated":false,"download_kbps":0,"upload_kbps":0,"download_limit_kbps":100,"upload_limit_kbps":100}}'
build_fair_outcome throughput-fallback historical-trust-missed capacity-objective-missed
printf '%s\n' "$fair_outcome_json" | grep -q '"apply_sqm_available":true'
printf '%s\n' "$fair_outcome_json" | grep -q '"disable_sqm_available":false'
printf '%s\n' "$fair_outcome_json" | grep -q '"recommended_action":"apply_sqm"'
printf '%s\n' "$fair_outcome_json" | grep -q '"comparison_reason":"historical-capacity-comparison-warning"'

# Aggregate CPU can hide a saturated packet-processing core. The effective
# safety signal is therefore the worse of total utilization and the busiest
# core, while softirq remains an explicit diagnostic.
cpu_previous='cpu 100 80 2
cpu0 50 45 1
cpu1 50 35 1'
cpu_current='cpu 200 160 6
cpu0 100 90 2
cpu1 100 70 4'
test "$(cpu_sample_fields "$cpu_previous" "$cpu_current")" = '30.0 20.0 30.0 6.0'
printf '%s\n' '30.0 20.0 30.0 6.0' '25.0 25.0 20.0 8.0' > "$work/cpu-samples"
test "$(cpu_peak_fields "$work/cpu-samples")" = '30.0 25.0 30.0 8.0'
test "$(cpu_sustained_fields "$work/cpu-samples" 28)" = '27.5 30.0 2 1 1 8.0'
printf '%s\n' \
	'96.0 58.0 96.0 85.0' \
	'99.0 60.0 99.0 92.0' \
	'100.0 61.0 100.0 94.0' \
	'87.0 55.0 87.0 80.0' > "$work/cpu-sustained"
test "$(cpu_sustained_fields "$work/cpu-sustained" 85)" = '95.5 100.0 4 4 4 94.0'
rps_mask_selects_one_cpu 8
rps_mask_selects_one_cpu 00000000,00000010
if rps_mask_selects_one_cpu f; then exit 1; fi
if rps_mask_selects_one_cpu 3; then exit 1; fi
if rps_mask_selects_one_cpu 0; then exit 1; fi

# Previous terminal diagnostics are retained in bounded RAM history. Count
# pruning is deterministic even when several runs finish in the same second,
# and the public history endpoint exposes metadata rather than arbitrary paths.
job_name=historyunit
job_paths
mkdir -p "$job_dir"
export CAKE_AUTORATE_AUTOTUNE_HISTORY_RUNS=2
export CAKE_AUTORATE_AUTOTUNE_HISTORY_KIB=128
for history_test_run in a b c; do
	printf '{"state":"complete","schema_version":8,"producer":"cake-autorate-rs-autotune","profile":"fair","run_id":"run-%s"}\n' \
		"$history_test_run" > "$result_file"
	printf 'diagnostic for run-%s\n' "$history_test_run" > "$log_file"
	archive_previous_terminal
done
test "$(find "$history_dir" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')" = 2
history_job > "$work/history.json"
grep -q '"run_id":"run-c"' "$work/history.json"
grep -q '"run_id":"run-b"' "$work/history.json"
if grep -q '"run_id":"run-a"' "$work/history.json"; then exit 1; fi
node -e 'JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"))' "$work/history.json"
unset CAKE_AUTORATE_AUTOTUNE_HISTORY_RUNS CAKE_AUTORATE_AUTOTUNE_HISTORY_KIB

autotune_profile=best_overall
if proposal_json_valid 'not-json},"forged":true,{' ; then exit 1; fi
download_samples=50000,110000
upload_samples=10000,20000
idle_median_ms=13
idle_p95_ms=16
baseline_samples=15
link_kind=cellular
conservative_mode=0
autotune_profile=gaming
gaming_proposal="$(calculate_proposal)"
proposal_json_valid "$gaming_proposal"
printf '%s\n' "$gaming_proposal" | grep -q '"profile":"gaming","target_grade":"A+"'
printf '%s\n' "$gaming_proposal" | grep -q '"script":"layer_cake.qos","classification":"diffserv4"'
printf '%s\n' "$gaming_proposal" | grep -q '"iqdisc_opts":"diffserv4","eqdisc_opts":"diffserv4"'
tampered_gaming="$(printf '%s\n' "$gaming_proposal" | sed 's/"classification":"diffserv4"/"classification":"besteffort"/')"
if proposal_json_valid "$tampered_gaming"; then exit 1; fi
autotune_profile=fair
fair_proposal="$(calculate_proposal)"
proposal_json_valid "$fair_proposal"
printf '%s\n' "$fair_proposal" | grep -q '"profile":"fair","target_grade":"C","quality_target_required":false,"throughput_priority":true'
printf '%s\n' "$fair_proposal" | grep -q '"capacity_retention_min_percent":90.0'
printf '%s\n' "$fair_proposal" | grep -q '"script":"layer_cake.qos","classification":"diffserv4"'
printf '%s\n' "$fair_proposal" | grep -q '"iqdisc_opts":"besteffort","eqdisc_opts":"diffserv4"'
autotune_profile=best_overall
route_mode=main
target_if=lo
reflectors="1.1.1.1 9.9.9.9 8.8.8.8"
pace_marker="$work/icmp.running"
pace_output="$work/icmp.raw"
export AUTOTUNE_MOCK_FPING_CALLS="$work/fping.calls"
: > "$pace_marker"
: > "$AUTOTUNE_MOCK_FPING_CALLS"
icmp_latency_monitor "$pace_output" "$pace_marker" &
pace_pid="$!"
sleep 2.4
rm -f "$pace_marker"
wait "$pace_pid"
pace_calls="$(wc -l < "$AUTOTUNE_MOCK_FPING_CALLS" | tr -d ' ')"
test "$pace_calls" -ge 2
test "$pace_calls" -le 3
unset AUTOTUNE_MOCK_FPING_CALLS

diverse="$(select_diverse_reflectors 1.1.1.1 1.0.0.1 9.9.9.9 9.9.9.10 8.8.8.8)"
test "$diverse" = "1.1.1.1 9.9.9.9 8.8.8.8"

printf '%s\n' \
	'{"raw_ms":[210.0,220.0,420.0]}' \
	'{"raw_ms":[230.0,240.0,480.0]}' > "$work/transport.raw"
extract_transport_values "$work/transport.raw" "$work/transport.values"
test "$(wc -l < "$work/transport.values" | tr -d ' ')" = 6
grep -qx '480.0' "$work/transport.values"

# Robust per-batch medians are diagnostic only. Full Auto-Tune's aggregate p95
# must retain every valid high-tail RTT from the native probe.
printf '%s\n' \
	'{"backend":"websocket","endpoint":"wss://example.test/ws","raw_ms":[10,10,10,200],"discarded":1,"trusted":true,"connection_reused":false}' \
	'{"backend":"websocket","endpoint":"wss://example.test/ws","raw_ms":[10,10,10,200],"discarded":1,"trusted":true,"connection_reused":true}' \
	'{"backend":"websocket","endpoint":"wss://example.test/ws","raw_ms":[10,10,10,200],"discarded":1,"trusted":true,"connection_reused":true}' \
	'{"backend":"websocket","endpoint":"wss://example.test/ws","raw_ms":[10,10,10,200],"discarded":1,"trusted":true,"connection_reused":true}' > "$work/transport-high-tail.raw"
transport_probe_reported_backend=websocket
transport_probe_endpoint=wss://example.test/ws
validate_transport_evidence "$work/transport-high-tail.raw" "$work/transport-high-tail.values" 4 12
high_tail_stats="$(numeric_latency_stats "$work/transport-high-tail.values" "$work/transport-high-tail.sorted")"
set -- $high_tail_stats
test "$2" = 200.000

# Native transport samples are accepted only when their provenance and
# persistent-session semantics match the configured probe exactly.
transport_probe_reported_backend=websocket
transport_probe_endpoint=wss://example.test/ws
printf '%s\n' \
	'{"backend":"websocket","endpoint":"wss://example.test/ws","raw_ms":[10,11,12,13],"trusted":true,"connection_reused":false}' \
	'{"backend":"websocket","endpoint":"wss://example.test/ws","raw_ms":[11,12,13,14],"trusted":true,"connection_reused":true}' \
	'{"backend":"websocket","endpoint":"wss://example.test/ws","raw_ms":[12,13,14,15],"trusted":true,"connection_reused":true}' > "$work/transport-evidence.raw"
validate_transport_evidence "$work/transport-evidence.raw" "$work/transport-evidence.values" 3 12
test "$transport_evidence_valid" = true
test "$transport_evidence_rows" = 3

sed 's/"trusted":true/"trusted":false/' "$work/transport-evidence.raw" > "$work/transport-untrusted.raw"
if validate_transport_evidence "$work/transport-untrusted.raw" "$work/transport-untrusted.values" 3 12; then exit 1; fi
test "$transport_evidence_reason" = transport-untrusted
sed '2s/"connection_reused":true/"connection_reused":false/' \
	"$work/transport-evidence.raw" > "$work/transport-nonpersistent.raw"
if validate_transport_evidence "$work/transport-nonpersistent.raw" "$work/transport-nonpersistent.values" 3 12; then exit 1; fi
test "$transport_evidence_reason" = transport-not-persistent
sed 's#wss://example.test/ws#wss://wrong.example/ws#' \
	"$work/transport-evidence.raw" > "$work/transport-wrong-endpoint.raw"
if validate_transport_evidence "$work/transport-wrong-endpoint.raw" "$work/transport-wrong-endpoint.values" 3 12; then exit 1; fi
test "$transport_evidence_reason" = transport-endpoint-mismatch

# Reflector tokens must be real DNS/IP targets, never command options or
# pathname patterns.  Compressed IPv6 remains supported.
valid_reflector 1.1.1.1
valid_reflector reflector.example.net
valid_reflector 2001:4860:4860::8888
for invalid_reflector in -I --help '*.example' '1::2::3' 'bad..example' '999.1.1.1'; do
	if valid_reflector "$invalid_reflector"; then exit 1; fi
done
newline_reflector="$(printf '1.1.1.1\n-bad')"
if valid_reflector "$newline_reflector"; then exit 1; fi

# BusyBox awk rejects literal braces in a bracket expression.  Both copies of
# the reflector validator must rely on their shell allow-list instead of
# reintroducing that GNU-only expression.
for reflector_validator in \
	"$test_dir/../root/usr/libexec/cake-autorate-rs/autotune" \
	"$test_dir/../root/usr/libexec/cake-autorate-rs/pinger-plan"; do
	if grep -Fq 'value ~ /[[:space:]\*?\[\]{}$`\\\/]/' "$reflector_validator"; then
		exit 1
	fi
done
(
	export CAKE_AUTORATE_PINGER_PLAN_SOURCE_ONLY=1
	set -- primary status '' '' ''
	. "$test_dir/../root/usr/libexec/cake-autorate-rs/pinger-plan"
	valid_reflector 2001:db8::1
	for invalid_reflector in -I --help '*.example' '1::2::3'; do
		if valid_reflector "$invalid_reflector"; then exit 1; fi
	done
	newline_reflector="$(printf '1.1.1.1\n-bad')"
	if valid_reflector "$newline_reflector"; then exit 1; fi
	pinger_argv="$work/pinger-plan-fping.argv"
	route_exec() {
		printf '%s\n' "$*" > "$pinger_argv"
		printf '%s\n' \
			'1.1.1.1 : xmt/rcv/%loss = 1/1/0%, min/avg/max = 1.00/1.00/1.00'
	}
	fping_bin=/usr/bin/fping
	test "$(probe_fping_batch '1.1.1.1 8.8.8.8' '')" = '1.1.1.1|1.00'
	grep -q -- '/usr/bin/fping -i 100 -c 1 -t 1000 -- 1.1.1.1 8.8.8.8' "$pinger_argv"
)

# Backend/endpoint mismatches must fail explicitly. Auto-Tune must never hide a
# user's invalid endpoint by silently testing a different server.
uci() {
	case "${3:-}" in
		*.transport_probe_backend) printf '%s\n' http ;;
		*.transport_probe_endpoint) printf '%s\n' http://example.invalid/ping ;;
		*.transport_probe_timeout_s) printf '%s\n' 5 ;;
		*) return 1 ;;
	esac
}
job_name=transportfallback
if configure_transport_probe; then exit 1; fi
test "$transport_probe_configuration_error" = "The configured transport probe backend and endpoint do not match."
unset -f uci

uci() {
	case "${3:-}" in
		*.transport_probe_backend) printf '%s\n' tcp ;;
		*.transport_probe_endpoint) printf '%s\n' tcp://example.test:443 ;;
		*.transport_probe_timeout_s) printf '%s\n' 5 ;;
		*) return 1 ;;
	esac
}
job_name=transporttcp
if configure_transport_probe; then exit 1; fi
test "$transport_probe_configuration_error" = "Full Auto-Tune requires persistent WebSocket or HTTPS transport evidence; TCP connect probes are diagnostic-only."
unset -f uci

uci() { return 1; }
job_name=transportdefault
configure_transport_probe
test "$transport_probe_backend" = websocket
test "$transport_probe_endpoint" = wss://ping-bufferbloat.libreqos.com/ws
unset -f uci

# Per-reflector evidence distinguishes a likely single-family ICMP limiter
# from real multi-family loaded latency, and disagreement never reaches the
# typed candidate validator.
printf '%s\n' \
	'1.1.1.1       : 10.0 11.0 12.0 13.0 14.0' \
	'9.9.9.9       : 12.0 13.0 14.0 15.0 16.0' \
	'2001:db8::1   : 11.0 12.0 13.0 14.0 15.0' > "$work/fping-aligned.raw"
build_icmp_evidence "$work/fping-aligned.raw" \
	'1.1.1.1 9.9.9.9 2001:db8::1' "$work/fping-aligned.tsv" baseline 5 3
test "$(wc -l < "$work/fping-aligned.tsv")" -eq 3
awk -F '\t' '$2 != 5 || $3 != 5 || $4 != "0.00" { exit 1 }' "$work/fping-aligned.tsv"

printf '%s\n' \
	'1.1.1.1 10 10 0.00 15.0' \
	'9.9.9.9 10 10 0.00 16.0' \
	'8.8.8.8 10 10 0.00 17.0' > "$work/icmp-idle.tsv"
printf '%s\n' \
	'1.1.1.1 10 7 30.00 20.0' \
	'9.9.9.9 10 10 0.00 21.0' \
	'8.8.8.8 10 10 0.00 22.0' > "$work/icmp-rate-limit.tsv"
if classify_loaded_icmp_evidence "$work/icmp-rate-limit.tsv" "$work/icmp-idle.tsv"; then exit 1; fi
test "$icmp_evidence_class" = rate-limit
test "$icmp_evidence_detail" = 1.1.1.1
printf '%s\n' \
	'1.1.1.1 10 10 0.00 20.0' \
	'9.9.9.9 10 10 0.00 21.0' \
	'8.8.8.8 10 10 0.00 200.0' > "$work/icmp-disagreement.tsv"
if classify_loaded_icmp_evidence "$work/icmp-disagreement.tsv" "$work/icmp-idle.tsv"; then exit 1; fi
test "$icmp_evidence_class" = disagreement

# Fingerprints bind the review to exact existing managed UCI, or to the
# explicit absence used by the create wizard.  Misownership/foreign queues are
# not fingerprintable.
job_name=fingerprint
job_paths
mkdir -p "$job_dir"
existing_fingerprint="$(compute_config_fingerprint)"
printf '%s\n' "$existing_fingerprint" | grep -Eq '^sha256:[0-9a-f]{64}$'
config_fingerprint="$existing_fingerprint"
config_fingerprint_captured=true
export AUTOTUNE_MOCK_UCI_REVISION=1
changed_fingerprint="$(compute_config_fingerprint)"
test "$changed_fingerprint" != "$existing_fingerprint"
if config_fingerprint_matches; then exit 1; fi
unset AUTOTUNE_MOCK_UCI_REVISION
config_fingerprint_captured=false
export AUTOTUNE_MOCK_RULE_REVISION=1
changed_rule_fingerprint="$(compute_config_fingerprint)"
test "$changed_rule_fingerprint" != "$existing_fingerprint"
unset AUTOTUNE_MOCK_RULE_REVISION
export AUTOTUNE_MOCK_UCI_MISOWNED=1
if compute_config_fingerprint >/dev/null; then exit 1; fi
unset AUTOTUNE_MOCK_UCI_MISOWNED
export AUTOTUNE_MOCK_UCI_ABSENT=1
absent_fingerprint="$(compute_config_fingerprint)"
printf '%s\n' "$absent_fingerprint" | grep -Eq '^sha256:[0-9a-f]{64}$'
test "$absent_fingerprint" != "$existing_fingerprint"
export AUTOTUNE_MOCK_UCI_FOREIGN_SQM=1
if compute_config_fingerprint >/dev/null; then exit 1; fi
unset AUTOTUNE_MOCK_UCI_FOREIGN_SQM AUTOTUNE_MOCK_UCI_ABSENT

# Live proposal attestation must recompute UCI and route identity instead of
# replaying the settled result file.  The fixture route helper supplies an
# independently generated current identity.
target_if_input=lo
backend=speedtest-go
route_mode_override=main
mwan3_member_override=""
attest_job > "$work/live-attestation.json"
grep -q '"state":"ready"' "$work/live-attestation.json"
grep -Eq '"config_fingerprint":"sha256:[0-9a-f]{64}"' "$work/live-attestation.json"
grep -q '"route_identity":"main||lo|192.0.2.1||main"' "$work/live-attestation.json"
grep -q '"external_ip":"203.0.113.1"' "$work/live-attestation.json"

export AUTOTUNE_MOCK_UCI_REVISION=1
attest_job > "$work/live-attestation-changed.json"
changed_live_fingerprint="$(sed -n 's/.*"config_fingerprint":"\([^"]*\)".*/\1/p' "$work/live-attestation-changed.json")"
test "$changed_live_fingerprint" != "$existing_fingerprint"
unset AUTOTUNE_MOCK_UCI_REVISION

# A settled file cannot outrank an armed recovery journal in public status.
job_name=statuspending
job_paths
mkdir -p "$job_dir" "$job_root/recovery"
printf '%s\n' '{"state":"complete","config_fingerprint":"sha256:deadbeef"}' > "$result_file"
: > "$job_root/recovery/statuspending_fixture.journal"
status_job > "$work/status-pending.json"
grep -q '"state":"recovery-pending"' "$work/status-pending.json"
if grep -q '"state":"complete"' "$work/status-pending.json"; then exit 1; fi
rm -f "$job_root/recovery/statuspending_fixture.journal"
status_job > "$work/status-settled.json"
grep -q '"state":"legacy"' "$work/status-settled.json"
grep -q '"legacy_result":{"state":"complete"' "$work/status-settled.json"
grep -q '"auto_apply_eligible":false' "$work/status-settled.json"

printf '%s\n' '{"state":"complete","schema_version":8,"producer":"cake-autorate-rs-autotune","profile":"best_overall"}' > "$result_file"
status_job > "$work/status-current.json"
grep -q '"state":"complete"' "$work/status-current.json"
if grep -q '"state":"legacy"' "$work/status-current.json"; then exit 1; fi

# Route metadata used by mwan3 must be explicit and shell-safe before it can
# participate in the stable route identity.
valid_route_mark 0x200
valid_route_mark 512
if valid_route_mark ''; then exit 1; fi
if valid_route_mark '0x2/0xff'; then exit 1; fi
valid_route_table 2
valid_route_table mwan_wanb
if valid_route_table main; then exit 1; fi
if valid_route_table '2;reboot'; then exit 1; fi

# mwan3's socket wrapper does not affect netlink route queries.  Route
# attestation must therefore pass the independently discovered member mark to
# ip route get instead of wrapping the ip process with mwan3 use.
ip() {
	test "$*" = '-4 route get 1.1.1.1 from 10.0.100.102 mark 0x200'
	printf '%s\n' '1.1.1.1 from 10.0.100.102 via 10.0.100.1 dev eth0 table 2 mark 0x200'
}
test "$(route_device_for_mark 10.0.100.102 0x200)" = eth0
unset -f ip 2>/dev/null || true

# A stale or reused PID is not a worker identity: status may report failure,
# but cancel must not signal the unrelated process.
job_name=staleidentity
job_paths
mkdir -p "$job_dir"
printf '%s\n%s\n%s\n' "$$" "$(proc_starttime "$$")" not-the-worker-token > "$pid_file"
status_job > "$work/stale-status.json"
cancel_job > "$work/stale-cancel.json"
grep -q 'identity is stale' "$work/stale-status.json"
grep -q 'identity is stale' "$work/stale-cancel.json"
if grep -q '"state":"idle"' "$work/stale-cancel.json"; then exit 1; fi

# A new instance may use only a clean target. Existing CAKE/fq_codel/pfifo or
# ingress state without matching managed-SQM ownership is intentionally not
# snapshotted or replaced.
export SQM_STATE_DIR="$work/sqm-state"
mkdir -p "$SQM_STATE_DIR"
tc() {
	[ "${AUTOTUNE_MOCK_TC_FAIL:-0}" != 1 ] || return 1
	printf '%s\n' "${AUTOTUNE_MOCK_QDISC_STATE:-}"
}
uci() {
	return 1
}
job_name=newunmanaged
target_if=lo
export AUTOTUNE_MOCK_TC_FAIL=1
if inspect_sqm_ownership; then exit 1; fi
printf '%s\n' "$sqm_ownership_error" | grep -q 'will not assume the target is clean'
unset AUTOTUNE_MOCK_TC_FAIL
export AUTOTUNE_MOCK_QDISC_STATE='qdisc cake 8001: root refcnt 2 bandwidth 100Mbit'
if inspect_sqm_ownership; then exit 1; fi
printf '%s\n' "$sqm_ownership_error" | grep -q 'not owned by this new instance'
export AUTOTUNE_MOCK_QDISC_STATE='qdisc noqueue 0: root refcnt 2'
inspect_sqm_ownership

# Hidden auto-preset aliases used to be removed by LuCI after disabling and
# re-enabling an instance. The canonical owner marker, owned SQM queue and WAN
# identity are sufficient to recover that representation, but only when they
# all resolve to the exact requested target.
: > "$SQM_STATE_DIR/lo.state"
job_name=missingaliases
target_if=lo
tc() {
	case "$*" in
		'qdisc show dev lo')
			printf '%s\n' \
				'qdisc cake 8001: root refcnt 2 bandwidth 100Mbit' \
				'qdisc ingress ffff: parent ffff:fff1 ----------------'
			;;
		'filter show dev lo ingress')
			printf '%s\n' 'action order 1: mirred (Egress Redirect to device lo) stolen'
			;;
	esac
}
uci() {
	[ "${1:-}" = -q ] && [ "${2:-}" = get ] || return 1
	case "${3:-}" in
		cake-autorate.missingaliases) printf '%s\n' cake_autorate ;;
		cake-autorate.missingaliases.manage_sqm|cake-autorate.missingaliases.sqm_enabled) printf '%s\n' 1 ;;
		cake-autorate.missingaliases.auto_interface_preset) printf '%s\n' "${AUTOTUNE_MOCK_AUTO_PRESET:-1}" ;;
		cake-autorate.missingaliases.sqm_interface) [ "${AUTOTUNE_MOCK_STALE_ALIAS:-0}" = 1 ] && printf '%s\n' definitely-missing-if || return 1 ;;
		cake-autorate.missingaliases.sqm_section) printf '%s\n' cake_missingaliases ;;
		cake-autorate.missingaliases.wan_if|cake-autorate.missingaliases.dl_if) printf '%s\n' lo ;;
		sqm.cake_missingaliases._cake_autorate_managed) printf '%s\n' missingaliases ;;
		sqm.cake_missingaliases.enabled) printf '%s\n' 1 ;;
		sqm.cake_missingaliases.interface) printf '%s\n' lo ;;
		*) return 1 ;;
	esac
}
inspect_sqm_ownership
export AUTOTUNE_MOCK_STALE_ALIAS=1
inspect_sqm_ownership
export AUTOTUNE_MOCK_AUTO_PRESET=0
if inspect_sqm_ownership; then
	echo 'manual SQM ownership accepted a stale explicit interface alias' >&2
	exit 1
fi
printf '%s\n' "$sqm_ownership_error" | grep -q 'no resolved SQM interface'
unset AUTOTUNE_MOCK_STALE_ALIAS AUTOTUNE_MOCK_AUTO_PRESET
target_if=eth0
if inspect_sqm_ownership; then
	echo 'managed SQM ownership accepted a different target interface' >&2
	exit 1
fi
printf '%s\n' "$sqm_ownership_error" | grep -q 'does not target eth0'
target_if=lo
unset AUTOTUNE_MOCK_QDISC_STATE SQM_STATE_DIR
unset -f tc uci

# Temporary-shaper creation uses a typed argv contract. Gaming validation must
# preserve DSCP and exercise diffserv4; throughput profiles use outbound
# diffserv4 while retaining the exact best-effort+wash download policy.
tc() {
	printf '%s\n' "$*" > "$work/temp-cake.argv"
}
autotune_profile=gaming
link_kind=pppoe
replace_temporary_cake_qdisc lo a123: 806473 upload
grep -qx 'qdisc replace dev lo root handle a123: cake bandwidth 806473kbit diffserv4 nat ethernet overhead 44 mpu 84' "$work/temp-cake.argv"
replace_temporary_cake_qdisc ifb-test b123: 806473 download
grep -qx 'qdisc replace dev ifb-test root handle b123: cake bandwidth 806473kbit diffserv4 nat ethernet overhead 44 mpu 84' "$work/temp-cake.argv"
autotune_profile=best_overall
link_kind=ethernet
replace_temporary_cake_qdisc lo a123: 806473 upload
grep -qx 'qdisc replace dev lo root handle a123: cake bandwidth 806473kbit diffserv4 nat ethernet overhead 18 mpu 64' "$work/temp-cake.argv"
autotune_profile=fair
link_kind=cellular
replace_temporary_cake_qdisc ifb-test b123: 806473 download
grep -qx 'qdisc replace dev ifb-test root handle b123: cake bandwidth 806473kbit besteffort nat wash raw' "$work/temp-cake.argv"
unset -f tc

# Temporary-shaper ownership includes the exact configured CAKE bandwidth,
# classification, DSCP policy, and direction, not merely a matching kind and
# handle.
tc() {
	printf '%s\n' "${AUTOTUNE_MOCK_QDISC_STATE:-}"
}
autotune_profile=best_overall
export AUTOTUNE_MOCK_QDISC_STATE='qdisc cake a123: root refcnt 2 bandwidth 806.473Mbit diffserv4 nat'
exact_root_qdisc lo a123: 806473 upload
if exact_root_qdisc lo a123: 806472 upload; then exit 1; fi
export AUTOTUNE_MOCK_QDISC_STATE='qdisc cake a123: root refcnt 2 diffserv4 nat'
if exact_root_qdisc lo a123: 806473 upload; then exit 1; fi
export AUTOTUNE_MOCK_QDISC_STATE='qdisc cake a123: root refcnt 2 bandwidth 806.473Mbit besteffort nat wash'
exact_root_qdisc lo a123: 806473 download
if exact_root_qdisc lo a123: 806473 upload; then exit 1; fi
autotune_profile=gaming
export AUTOTUNE_MOCK_QDISC_STATE='qdisc cake a123: root refcnt 2 bandwidth 806473Kbit diffserv4 nat'
exact_root_qdisc lo a123: 806473 upload
export AUTOTUNE_MOCK_QDISC_STATE='qdisc cake a123: root refcnt 2 bandwidth 806473Kbit diffserv4 nat wash'
if exact_root_qdisc lo a123: 806473 upload; then exit 1; fi
export AUTOTUNE_MOCK_QDISC_STATE='qdisc cake a123: root refcnt 2 bandwidth 806473Kbit besteffort nat'
if exact_root_qdisc lo a123: 806473 upload; then exit 1; fi
temp_ifb=ifb-test
temp_ifb_handle=b123:
temp_target_handle=a123:
target_if=lo
export AUTOTUNE_MOCK_QDISC_STATE='qdisc cake b123: root refcnt 2 bandwidth 806473Kbit besteffort nat wash
 Sent 12345 bytes 67 pkt (dropped 2, overlimits 3 requeues 4)'
test "$(temporary_qdisc_stats_json download)" = '{"available":true,"device":"ifb-test","handle":"b123:","bytes":12345,"packets":67,"dropped":2,"overlimits":3,"requeues":4}'
unset AUTOTUNE_MOCK_QDISC_STATE
unset -f tc
autotune_profile=best_overall

# Timeout supervision owns an isolated session/process group. TERM is bounded
# and followed by KILL, including a helper child that deliberately ignores
# TERM, so no backend process is orphaned.
(
job_name=timeoutprobe
job_paths
mkdir -p "$job_dir"
recovery_prepared=1
recovery_armed=1
recovery_journal="$job_dir/recovery.journal"
recovery_heartbeat="$job_dir/recovery.armed"
: > "$recovery_journal"
: > "$recovery_heartbeat"
write_recovery_journal() { return 0; }
backend=speedtest-go
selected_server_id=""
export AUTOTUNE_MOCK_BLOCK=1
export AUTOTUNE_MOCK_RESTORE_MARKER="$work/timeout-restored"
export AUTOTUNE_MOCK_BLOCK_STARTED="$work/timeout-started"
export AUTOTUNE_MOCK_ORPHAN_PID_FILE="$work/timeout-orphan.pid"
export CAKE_AUTORATE_AUTOTUNE_SPEEDTEST_TIMEOUT_S=1
export CAKE_AUTORATE_AUTOTUNE_SPEEDTEST_TERM_GRACE_S=1
timeout_result="$job_dir/timeout.result"
if run_speedtest_with_timeout "$timeout_result" timeoutprobe lo run speedtest-go; then
	exit 1
else
	timeout_rc="$?"
fi
test "$timeout_rc" -eq 124
test ! -e "$timeout_result"
test -f "$work/timeout-restored"
orphan_pid="$(sed -n '1p' "$work/timeout-orphan.pid")"
orphan_wait=0
while [ "$orphan_wait" -lt 50 ]; do
	orphan_state="$(proc_state "$orphan_pid" 2>/dev/null || true)"
	case "$orphan_state" in ''|Z) break ;; esac
	orphan_wait=$((orphan_wait + 1))
	sleep 0.05
done
case "$(proc_state "$orphan_pid" 2>/dev/null || true)" in ''|Z) ;; *) exit 1 ;; esac
unset AUTOTUNE_MOCK_BLOCK AUTOTUNE_MOCK_RESTORE_MARKER AUTOTUNE_MOCK_BLOCK_STARTED
unset AUTOTUNE_MOCK_ORPHAN_PID_FILE CAKE_AUTORATE_AUTOTUNE_SPEEDTEST_TIMEOUT_S
unset CAKE_AUTORATE_AUTOTUNE_SPEEDTEST_TERM_GRACE_S
recovery_prepared=0
recovery_armed=0
)

# nft evidence is complete only when both counters can be read.  Tables are
# PID-scoped so an uncleanly terminated older worker cannot block a new run.
original_path="$PATH"
export PATH="$fixtures:$PATH"
export CAKE_AUTORATE_AUTOTUNE_TEST_PREFLIGHT=0
export AUTOTUNE_MOCK_NFT_LOG="$work/nft.calls"
job_name=nftprobe
target_if=lo
phase_counter_created=0
phase_evidence_complete=true
start_phase_background_counter 100000 100000
stop_phase_background_counter
test "$phase_background_available" = true
test "$phase_evidence_complete" = true
grep -Eq '^add table inet cake_autotune_nftprobe_[0-9]+$' "$AUTOTUNE_MOCK_NFT_LOG"

# Only numeric PID-suffixed tables for this exact safe job name are stale.
# Another job and lookalike suffix must remain untouched.
: > "$AUTOTUNE_MOCK_NFT_LOG"
export AUTOTUNE_MOCK_NFT_TABLES='table inet cake_autotune_nftprobe_123
table inet cake_autotune_other_456
table inet cake_autotune_nftprobe_notpid'
cleanup_stale_phase_background_tables
grep -q '^delete table inet cake_autotune_nftprobe_123$' "$AUTOTUNE_MOCK_NFT_LOG"
if grep -q 'cake_autotune_other_456\|cake_autotune_nftprobe_notpid' "$AUTOTUNE_MOCK_NFT_LOG"; then
	exit 1
fi
unset AUTOTUNE_MOCK_NFT_TABLES

flowtable_ruleset='table inet fw4 {
 flowtable ft {
  hook ingress priority filter
 }
}'
active_nft_flowtable "$flowtable_ruleset"
if active_nft_flowtable 'table inet fw4 { chain forward { type filter hook forward priority filter; } }'; then
	exit 1
fi

export AUTOTUNE_MOCK_NFT_MISSING_COUNTER=1
phase_evidence_complete=true
start_phase_background_counter 100000 100000
stop_phase_background_counter
test "$phase_background_available" = false
test "$phase_background_contaminated" = true
test "$phase_evidence_complete" = false
unset AUTOTUNE_MOCK_NFT_MISSING_COUNTER

export AUTOTUNE_MOCK_NFT_FAIL_LIST=1
phase_evidence_complete=true
start_phase_background_counter 100000 100000
stop_phase_background_counter
test "$phase_background_available" = false
test "$phase_evidence_complete" = false
unset AUTOTUNE_MOCK_NFT_FAIL_LIST AUTOTUNE_MOCK_NFT_LOG
export CAKE_AUTORATE_AUTOTUNE_TEST_PREFLIGHT=1
export PATH="$original_path"

# The complete worker pipeline reproduces the observed Fair boundary result:
# the first candidate and the real upper bound retain less than Fair's 90%
# objective, but remain above the 50% historical trust boundary with good
# latency. Keep the fastest safe point for explicit review without silently
# lowering the profile objective or writing configuration itself.
: > "$work/counter"
real_daemon="$CAKE_AUTORATE_DAEMON"
export AUTOTUNE_REAL_DAEMON="$real_daemon"
export CAKE_AUTORATE_DAEMON="$fixtures/daemon-fair-boundary"
export AUTOTUNE_MOCK_FAIR_BOUNDARY=1
"$autotune" fairboundary lo start speedtest-go '' '' fair > "$work/fairboundary-start.json"
attempt=0
while [ "$attempt" -lt 320 ]; do
	"$autotune" fairboundary lo status speedtest-go '' '' fair > "$work/fairboundary-status.json"
	grep -q '"state":"complete"' "$work/fairboundary-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
node - "$work/fairboundary-status.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const download = result.profile_search && result.profile_search.download;
const upload = result.profile_search && result.profile_search.upload;
const candidates = download && download.evaluated.map(item => item.candidate_kbps);
if (result.state !== 'complete' || result.profile !== 'fair' ||
    result.configuration_written !== false || result.validation.safety_pass !== true ||
    result.validation.quality_target_met !== true ||
    result.validation.profile_objectives_met !== false ||
    result.auto_apply_eligible !== false || result.manual_apply_eligible !== true ||
    result.profile_outcome.mode !== 'latency-safe-throughput-advisory' ||
    result.profile_outcome.capacity_floor_met !== false ||
    result.profile_outcome.throughput_safety_floor_met !== true ||
    JSON.stringify(candidates) !== JSON.stringify([848500, 902700]) ||
    download.selected.candidate_kbps !== 902700 ||
    download.selected.retention_percent < 50 ||
    download.selected.capacity_objective_met !== false ||
    upload.selected.candidate_kbps !== 900000 ||
    upload.selected.retention_percent < 90 || upload.selected.capacity_objective_met !== true)
	throw new Error('Fair bounded search did not preserve the fastest reviewable safe solution');
EOF
grep -Eq '"config_fingerprint":"sha256:[0-9a-f]{64}"' "$work/fairboundary-status.json"
wait_for_job_cleanup fairboundary
export CAKE_AUTORATE_DAEMON="$real_daemon"
unset AUTOTUNE_MOCK_FAIR_BOUNDARY AUTOTUNE_REAL_DAEMON

# Exact RC16 regression: 92.5% candidate realization is distinct from 77.3%
# capacity retention, p95(loaded)-p95(idle) is 60 ms (not 260 ms), and a
# safety-floor conflict is infeasible rather than a blind 0.95 reduction.
test "$(nonnegative_delta 480 420)" = 60.0
test "$(nonnegative_delta 410 420)" = 0.0
rc16_decision="$("$CAKE_AUTORATE_DAEMON" --autotune-validate \
	--dl-observed-low-kbps 883500 --ul-observed-low-kbps 904500 \
	--dl-candidate-kbps 738500 --ul-candidate-kbps 755500 \
	--dl-achieved-kbps 683153 --ul-achieved-kbps 698955 \
	--dl-min-kbps 20000 --ul-min-kbps 20000 \
	--dl-max-kbps 1000000 --ul-max-kbps 1000000 \
	--dl-icmp-delta-ms 0.19 --ul-icmp-delta-ms 0.19 \
	--dl-transport-delta-ms 60 --ul-transport-delta-ms 60 \
	--dl-loss-percent 7.59 --ul-loss-percent 7.59 \
	--dl-cpu-percent 53.2 --ul-cpu-percent 53.2)"
printf '%s\n' "$rc16_decision" > "$work/rc16-decision.json"
grep -q '"candidate_realization_percent":92.505' "$work/rc16-decision.json"
grep -q '"capacity_retention_percent":77.323' "$work/rc16-decision.json"
grep -q '"candidate_realization_percent":92.516' "$work/rc16-decision.json"
grep -q '"capacity_retention_percent":77.275' "$work/rc16-decision.json"
grep -q '"transport_delta_ms":60.000' "$work/rc16-decision.json"
grep -q '"action":"infeasible"' "$work/rc16-decision.json"
grep -q '"reason":"safety-floor-blocks-rate-reduction"' "$work/rc16-decision.json"
grep -q '"required_floor_kbps":764100' "$work/rc16-decision.json"
grep -q '"required_floor_kbps":782200' "$work/rc16-decision.json"
if grep -q '"scale":0.950000' "$work/rc16-decision.json"; then
	exit 1
fi

printf '%s\n' 'autotune lifecycle tests passed'
