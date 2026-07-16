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
	rm -rf "$work"
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
export CAKE_AUTORATE_AUTOTUNE_RECOVER="$test_dir/../root/usr/libexec/cake-autorate-rs/autotune-recover"
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

# The fixture must model jsonfilter's top-level lookup semantics.  Greedy text
# extraction used to select nested proposal fields and report schema 1/state
# inner instead of the terminal envelope's schema 3/state complete.
nested_terminal='{"state":"complete","schema_version":3,"nested":{"state":"inner","schema_version":1}}'
test "$(printf '%s\n' "$nested_terminal" | "$CAKE_AUTORATE_JSONFILTER" -e '@.state')" = complete
test "$(printf '%s\n' "$nested_terminal" | "$CAKE_AUTORATE_JSONFILTER" -e '@.schema_version')" = 3

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
	strict_single_json_object "{\"state\":\"complete\",\"schema_version\":3}"
	! strict_single_json_object "$(printf "%s\n%s" "{\"state\":\"complete\",\"schema_version\":3}" "{\"forged\":true}")"
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
grep -q '"validation":{"pass":true' "$work/status.json"
grep -q '"comparison":"observed-low"' "$work/status.json"
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
test "$(sed -n '1p' "$work/test-directions")" = both
test "$(sed -n '2p' "$work/test-directions")" = both
test "$(sed -n '3p' "$work/test-directions")" = download
test "$(sed -n '4p' "$work/test-directions")" = upload
wait_for_job_cleanup fullauto

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

# A helper that silently runs both directions is not valid evidence for either
# directional candidate, even when it returns positive rates.
: > "$work/counter"
export AUTOTUNE_MOCK_RESULT_DIRECTION=both
"$autotune" wrongdirection lo start speedtest-go > "$work/wrongdirection-start.json"
attempt=0
while [ "$attempt" -lt 220 ]; do
	"$autotune" wrongdirection lo status speedtest-go > "$work/wrongdirection-status.json"
	grep -q 'reported both for the requested download-only' "$work/wrongdirection-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q 'reported both for the requested download-only' "$work/wrongdirection-status.json"
wait_for_job_cleanup wrongdirection
unset AUTOTUNE_MOCK_RESULT_DIRECTION

: > "$work/counter"
export AUTOTUNE_MOCK_CORRECT=1
"$autotune" corrected lo start speedtest-go > "$work/correct-start.json"
attempt=0
while [ "$attempt" -lt 220 ]; do
	"$autotune" corrected lo status speedtest-go > "$work/correct-status.json"
	grep -q '"state":"complete"' "$work/correct-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"validation_attempts":\[{"pass":false' "$work/correct-status.json"
grep -q '},{"pass":true' "$work/correct-status.json"
grep -q '"validation":{"pass":true' "$work/correct-status.json"
grep -q '"action":"retry-measurement"' "$work/correct-status.json"
wait_for_job_cleanup corrected
unset AUTOTUNE_MOCK_CORRECT

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
grep -q '"code":"download-candidate-realization-maximum","scope":"download","pass":false' "$work/overshoot-status.json"
grep -q '"configuration_written":false' "$work/overshoot-status.json"
wait_for_job_cleanup overshoot
unset AUTOTUNE_MOCK_REALIZATION_OVERSHOOT

# The existing low-realization retry has the same terminal evidence semantics:
# exhausting the bounded retry is inconclusive, not a proven candidate failure.
: > "$work/counter"
export AUTOTUNE_MOCK_REALIZATION_LOW=1
"$autotune" lowrealization lo start speedtest-go > "$work/lowrealization-start.json"
attempt=0
while [ "$attempt" -lt 260 ]; do
	"$autotune" lowrealization lo status speedtest-go > "$work/lowrealization-status.json"
	grep -q 'candidate-realization-too-low-after-bounded-retry' "$work/lowrealization-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"inconclusive"' "$work/lowrealization-status.json"
grep -q '"retryable":true' "$work/lowrealization-status.json"
if grep -q '"state":"failed"' "$work/lowrealization-status.json"; then exit 1; fi
wait_for_job_cleanup lowrealization
unset AUTOTUNE_MOCK_REALIZATION_LOW

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
test "$(sed -n '1p' "$work/counter")" = 3
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

# A clean capacity shortfall raises only the failing direction.  The typed
# correction target is composed against the unscaled proposal, so UL must not
# inherit the DL scale.
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
grep -q '"correction":{"action":"increase"' "$work/directional-status.json"
grep -q '"download":{"action":"increase"' "$work/directional-status.json"
grep -q '"upload":{"action":"none"' "$work/directional-status.json"
grep -q '"candidate_base":{"download_kbps":44600,"upload_kbps":8500}' "$work/directional-status.json"
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
"$autotune" baselineblocked lo start-conservative speedtest-go > "$work/baselineblocked-start.json"
attempt=0
while [ "$attempt" -lt 220 ]; do
	"$autotune" baselineblocked lo status speedtest-go > "$work/baselineblocked-status.json"
	grep -q 'Conservative mode cannot reuse a contaminated idle p95' "$work/baselineblocked-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q 'Conservative mode cannot reuse a contaminated idle p95' "$work/baselineblocked-status.json"
wait_for_job_cleanup baselineblocked
unset CAKE_AUTORATE_AUTOTUNE_TEST_BASELINE_BACKGROUND_DL_KBPS

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
grep -q '"download_kbps":2500' "$work/background-status.json"
wait_for_job_cleanup background

: > "$work/counter"
"$autotune" background lo start-conservative speedtest-go > "$work/background-conservative-start.json"
attempt=0
while [ "$attempt" -lt 220 ]; do
	"$autotune" background lo status speedtest-go > "$work/background-conservative-status.json"
	grep -q '"state":"background-blocked"' "$work/background-conservative-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"background-blocked"' "$work/background-conservative-status.json"
grep -q 'conservative mode cannot invent an idle p95' "$work/background-conservative-status.json"
grep -q '"configuration_written":false' "$work/background-conservative-status.json"
wait_for_job_cleanup background
unset CAKE_AUTORATE_AUTOTUNE_BACKGROUND_DL_KBPS CAKE_AUTORATE_AUTOTUNE_BACKGROUND_UL_KBPS

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

if proposal_json_valid 'not-json},"forged":true,{' ; then exit 1; fi
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

printf '%s\n' '{"state":"complete","schema_version":3,"producer":"cake-autorate-rs-autotune"}' > "$result_file"
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

# A stale or reused PID is not a worker identity: status may report failure,
# but cancel must not signal the unrelated process.
job_name=staleidentity
job_paths
mkdir -p "$job_dir"
printf '%s\n%s\n%s\n' "$$" "$(proc_starttime "$$")" not-the-worker-token > "$pid_file"
status_job > "$work/stale-status.json"
cancel_job > "$work/stale-cancel.json"
grep -q 'identity is stale' "$work/stale-status.json"
grep -q '"state":"idle"' "$work/stale-cancel.json"

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
unset AUTOTUNE_MOCK_QDISC_STATE SQM_STATE_DIR
unset -f tc uci

# Temporary-shaper ownership includes the exact configured CAKE bandwidth, not
# merely a matching qdisc kind and handle.
tc() {
	printf '%s\n' "${AUTOTUNE_MOCK_QDISC_STATE:-}"
}
export AUTOTUNE_MOCK_QDISC_STATE='qdisc cake a123: root refcnt 2 bandwidth 806.473Mbit besteffort'
exact_root_qdisc lo a123: 806473
if exact_root_qdisc lo a123: 806472; then exit 1; fi
export AUTOTUNE_MOCK_QDISC_STATE='qdisc cake a123: root refcnt 2 besteffort'
if exact_root_qdisc lo a123: 806473; then exit 1; fi
unset AUTOTUNE_MOCK_QDISC_STATE
unset -f tc

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
if timeout_output="$(run_speedtest_with_timeout timeoutprobe lo run speedtest-go)"; then
	exit 1
else
	timeout_rc="$?"
fi
test "$timeout_rc" -eq 124
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

# Exact RC16 values also traverse the complete worker pipeline.  Trusted
# transport p95 is compared to trusted transport p95 (480 - 420 = 60), and the
# real typed validator rejects the old blind 0.95-style reduction at its
# safety floor without ever writing configuration.
: > "$work/counter"
real_daemon="$CAKE_AUTORATE_DAEMON"
export AUTOTUNE_REAL_DAEMON="$real_daemon"
export CAKE_AUTORATE_DAEMON="$fixtures/daemon-rc16"
export AUTOTUNE_MOCK_RC16=1
"$autotune" rc16e2e lo start speedtest-go > "$work/rc16e2e-start.json"
attempt=0
while [ "$attempt" -lt 260 ]; do
	"$autotune" rc16e2e lo status speedtest-go > "$work/rc16e2e-status.json"
	grep -q 'safety-floor-blocks-rate-reduction' "$work/rc16e2e-status.json" && break
	attempt=$((attempt + 1))
	sleep 0.05
done
grep -q '"state":"failed"' "$work/rc16e2e-status.json"
grep -q '"candidate_base":{"download_kbps":738500,"upload_kbps":755500}' "$work/rc16e2e-status.json"
grep -q '"candidate_realization_percent":92.505' "$work/rc16e2e-status.json"
grep -q '"capacity_retention_percent":77.323' "$work/rc16e2e-status.json"
grep -q '"candidate_realization_percent":92.516' "$work/rc16e2e-status.json"
grep -q '"capacity_retention_percent":77.275' "$work/rc16e2e-status.json"
grep -q '"delta_p95_ms":60.0' "$work/rc16e2e-status.json"
grep -q '"action":"infeasible"' "$work/rc16e2e-status.json"
grep -q '"required_floor_kbps":764100' "$work/rc16e2e-status.json"
grep -q '"required_floor_kbps":782200' "$work/rc16e2e-status.json"
grep -q '"configuration_written":false' "$work/rc16e2e-status.json"
grep -Eq '"config_fingerprint":"sha256:[0-9a-f]{64}"' "$work/rc16e2e-status.json"
if grep -q '"scale":0.950000' "$work/rc16e2e-status.json"; then exit 1; fi
wait_for_job_cleanup rc16e2e
export CAKE_AUTORATE_DAEMON="$real_daemon"
unset AUTOTUNE_MOCK_RC16 AUTOTUNE_REAL_DAEMON

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
