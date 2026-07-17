#!/bin/sh
set -eu

root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
REAL_UCI_BIN="$(command -v uci 2>/dev/null || true)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM
mkdir -p "$tmp/bin" "$tmp/state" "$tmp/locks"
log="$tmp/commands.log"
count="$tmp/service-count"
sha_count="$tmp/sha-count"
committed="$tmp/committed"
stage_seen="$tmp/stage-seen"
route_count="$tmp/route-count"
health_count="$tmp/health-count"

cat > "$tmp/bin/jsonfilter" <<'EOF'
#!/bin/sh
emit() {
	[ "$1" != __missing__ ] || exit 1
	printf '%s\n' "$1"
}
while [ "$#" -gt 0 ]; do
	case "$1" in
		-e) expression="$2"; shift 2 ;;
		-s) source="$2"; shift 2 ;;
		*) shift ;;
	esac
done
case "${source:-}" in *'"attestation":true'*) attestation=1 ;; *) attestation=0 ;; esac
case "$expression" in
	@.state)
		[ "$attestation" = 0 ] && emit "${RESULT_STATE-complete}" || emit "${ATTEST_STATE-ready}"
		;;
	@.schema_version)
		[ "$attestation" = 0 ] && emit "${RESULT_SCHEMA_VERSION-6}" || emit "${ATTEST_SCHEMA_VERSION-1}"
		;;
	@.producer) emit "${RESULT_PRODUCER-cake-autorate-rs-autotune}" ;;
	@.profile) emit "${RESULT_PROFILE-best_overall}" ;;
	@.run_id) emit "${RESULT_RUN_ID-scheduler-test-run}" ;;
	@.validation.profile) emit "${RESULT_VALIDATION_PROFILE-${RESULT_PROFILE-best_overall}}" ;;
	@.proposal.schema_version) emit "${RESULT_PROPOSAL_SCHEMA_VERSION-3}" ;;
	@.proposal.profile) emit "${RESULT_PROPOSAL_PROFILE-${RESULT_PROFILE-best_overall}}" ;;
	@.proposal.target_grade) emit "${RESULT_TARGET_GRADE-A}" ;;
	@.proposal.quality_target_required) emit "${RESULT_QUALITY_TARGET_REQUIRED-true}" ;;
	@.proposal.throughput_priority) emit "${RESULT_THROUGHPUT_PRIORITY-false}" ;;
	@.validation.pass) emit "${RESULT_VALIDATION_PASS-true}" ;;
	@.validation.hard_pass) emit "${RESULT_VALIDATION_HARD_PASS-true}" ;;
	@.validation.safety_pass) emit "${RESULT_VALIDATION_SAFETY_PASS-true}" ;;
	@.validation.quality_target_met) emit "${RESULT_QUALITY_TARGET_MET-true}" ;;
	@.profile_outcome.target_met) emit "${RESULT_PROFILE_TARGET_MET-true}" ;;
	@.profile_outcome.manual_only) emit "${RESULT_PROFILE_MANUAL_ONLY-false}" ;;
	@.auto_apply_eligible) emit "${RESULT_AUTO_APPLY_ELIGIBLE-true}" ;;
	@.manual_apply_eligible) emit "${RESULT_MANUAL_APPLY_ELIGIBLE-true}" ;;
	@.confidence_mode) emit "${RESULT_CONFIDENCE_MODE-normal}" ;;
	@.validation.contaminated) emit "${RESULT_VALIDATION_CONTAMINATED-false}" ;;
	@.phase_evidence_complete) emit "${RESULT_PHASE_EVIDENCE_COMPLETE-true}" ;;
	@.phase_contamination_seen) emit "${RESULT_PHASE_CONTAMINATION_SEEN-false}" ;;
	@.runtime_restored) emit "${RESULT_RUNTIME_RESTORED-true}" ;;
	@.recovery_pending) emit "${RESULT_RECOVERY_PENDING-false}" ;;
	@.configuration_written) emit "${RESULT_CONFIGURATION_WRITTEN-false}" ;;
	@.validation.correction.action) emit "${RESULT_CORRECTION_ACTION-none}" ;;
	@.validation.correction.feasible) emit "${RESULT_CORRECTION_FEASIBLE-true}" ;;
	@.validation.gates\[\*\].code)
		emit "${RESULT_GATE_CODES-download-candidate-realization upload-candidate-realization download-candidate-realization-maximum upload-candidate-realization-maximum download-capacity-retention upload-capacity-retention download-icmp-latency download-transport-latency download-packet-loss download-cpu upload-icmp-latency upload-transport-latency upload-packet-loss upload-cpu}"
		;;
	@.validation.gates\[\*\].required) emit "${RESULT_GATE_REQUIRED-true true true true true true true true true true true true true true}" ;;
	@.validation.gates\[\*\].pass) emit "${RESULT_GATE_PASSES-true true true true true true true true true true true true true true}" ;;
	@.validation.gates\[\*\].actual) emit "${RESULT_GATE_ACTUALS-90 90 90 90 90 90 10 10 0 40 10 10 0 40}" ;;
	@.validation.gates\[\*\].limit) emit "${RESULT_GATE_LIMITS-80 80 110 110 80 80 30 30 3 85 30 30 3 85}" ;;
	@.validation.gates\[\*\].comparison) emit "${RESULT_GATE_COMPARISONS-minimum minimum maximum maximum minimum minimum exclusive-maximum exclusive-maximum maximum maximum exclusive-maximum exclusive-maximum maximum maximum}" ;;
	@.config_fingerprint) emit "${RESULT_CONFIG_FINGERPRINT-__missing__}" ;;
	@.job_id) emit "${RESULT_JOB_ID-test}" ;;
	@.target_interface)
		[ "$attestation" = 0 ] && emit "${RESULT_TARGET_INTERFACE-eth0}" || emit "${ATTEST_TARGET_INTERFACE-${RESULT_TARGET_INTERFACE-eth0}}"
		;;
	@.resolved_interface)
		[ "$attestation" = 0 ] && emit "${RESULT_RESOLVED_INTERFACE-eth0}" || emit "${ATTEST_RESOLVED_INTERFACE-${RESULT_RESOLVED_INTERFACE-eth0}}"
		;;
	@.route_interface)
		[ "$attestation" = 0 ] && emit "${RESULT_ROUTE_INTERFACE-eth0}" || emit "${ATTEST_ROUTE_INTERFACE-${RESULT_ROUTE_INTERFACE-eth0}}"
		;;
	@.source_ip)
		[ "$attestation" = 0 ] && emit "${RESULT_SOURCE_IP-192.0.2.2}" || emit "${ATTEST_SOURCE_IP-${RESULT_SOURCE_IP-192.0.2.2}}"
		;;
	@.external_ip)
		[ "$attestation" = 0 ] && emit "${RESULT_EXTERNAL_IP-198.51.100.2}" || emit "${ATTEST_EXTERNAL_IP-${RESULT_EXTERNAL_IP-198.51.100.2}}"
		;;
	@.route_mode)
		[ "$attestation" = 0 ] && emit "${RESULT_ROUTE_MODE-main}" || emit "${ATTEST_ROUTE_MODE-${RESULT_ROUTE_MODE-main}}"
		;;
	@.mwan3_member)
		[ "$attestation" = 0 ] && emit "${RESULT_MWAN3_MEMBER-}" || emit "${ATTEST_MWAN3_MEMBER-${RESULT_MWAN3_MEMBER-}}"
		;;
	@.route_identity)
		[ "$attestation" = 0 ] && emit "${RESULT_ROUTE_IDENTITY-main||eth0|192.0.2.2||main}" || emit "${ATTEST_ROUTE_IDENTITY-${RESULT_ROUTE_IDENTITY-main||eth0|192.0.2.2||main}}"
		;;
	@.proposal.validation.candidate_realization_min_percent|@.validation_thresholds.candidate_realization_min_percent)
		emit "${RESULT_REALIZATION_MIN_PERCENT-80}"
		;;
	@.proposal.validation.candidate_realization_max_percent|@.validation_thresholds.candidate_realization_max_percent)
		emit "${RESULT_REALIZATION_MAX_PERCENT-110}"
		;;
	@.proposal.validation.capacity_retention_min_percent|@.validation_thresholds.capacity_retention_min_percent)
		emit "${RESULT_RETENTION_PERCENT-80}"
		;;
	@.proposal.validation.icmp_delta_max_ms) emit "${RESULT_ICMP_DELTA_MAX_MS-30}" ;;
	@.proposal.validation.transport_delta_max_ms|@.validation_thresholds.delay_max_ms)
		emit "${RESULT_DELAY_MAX_MS-30}"
		;;
	@.proposal.validation.loss_max_percent|@.validation_thresholds.loss_max_percent)
		emit "${RESULT_LOSS_MAX_PERCENT-3}"
		;;
	@.proposal.validation.cpu_max_percent|@.validation_thresholds.cpu_max_percent)
		emit "${RESULT_CPU_MAX_PERCENT-85}"
		;;
	@.proposal.sqm.qdisc) emit "${RESULT_SQM_QDISC-cake}" ;;
	@.proposal.sqm.script) emit "${RESULT_SQM_SCRIPT-layer_cake.qos}" ;;
	@.proposal.sqm.classification) emit "${RESULT_SQM_CLASSIFICATION-diffserv4}" ;;
	@.proposal.sqm.squash_dscp) emit "${RESULT_SQM_SQUASH_DSCP-true}" ;;
	@.proposal.sqm.squash_ingress) emit "${RESULT_SQM_SQUASH_INGRESS-true}" ;;
	@.proposal.sqm.ingress_ecn) emit "${RESULT_SQM_INGRESS_ECN-ECN}" ;;
	@.proposal.sqm.egress_ecn) emit "${RESULT_SQM_EGRESS_ECN-NOECN}" ;;
	@.proposal.sqm.iqdisc_opts) emit "${RESULT_SQM_IQDISC_OPTS-besteffort}" ;;
	@.proposal.sqm.eqdisc_opts) emit "${RESULT_SQM_EQDISC_OPTS-diffserv4}" ;;
	*.minimum_kbps) echo "${RESULT_MINIMUM_KBPS-5000}" ;;
	*.base_kbps) echo "${RESULT_BASE_KBPS-20000}" ;;
	*.maximum_kbps) echo "${RESULT_MAXIMUM_KBPS-80000}" ;;
	*.absolute_cap_kbps) echo "${RESULT_CAP_KBPS-90000}" ;;
	*.observed_low_kbps) echo "${RESULT_OBSERVED_LOW_KBPS-75000}" ;;
	*.observed_median_kbps) echo "${RESULT_OBSERVED_MEDIAN_KBPS-80000}" ;;
	*.observed_high_kbps) echo "${RESULT_OBSERVED_HIGH_KBPS-85000}" ;;
	*.active_threshold_kbps) echo "${RESULT_ACTIVE_THRESHOLD_KBPS-2000}" ;;
	*.thresholds_ms.adjust_up) echo "${RESULT_ADJUST_UP_MS-5}" ;;
	*.thresholds_ms.delay) echo "${RESULT_DELAY_MS-15}" ;;
	*.thresholds_ms.adjust_down) echo "${RESULT_ADJUST_DOWN_MS-30}" ;;
	*.adaptive_ceiling.hold_s) echo "${RESULT_HOLD_S-20}" ;;
	*.adaptive_ceiling.growth_percent) echo "${RESULT_GROWTH_PERCENT-3}" ;;
	*.adaptive_ceiling.probe_s) echo "${RESULT_PROBE_S-8}" ;;
	*.adaptive_ceiling.cooldown_s) echo "${RESULT_COOLDOWN_S-30}" ;;
	*.adaptive_ceiling.failed_bound_ttl_s) echo "${RESULT_TTL_S-900}" ;;
	*.link.overhead) echo "${RESULT_LINK_OVERHEAD-44}" ;;
	*.link.mpu) echo "${RESULT_LINK_MPU-84}" ;;
	*.confidence) echo "${RESULT_CONFIDENCE-85}" ;;
	*.adaptive_ceiling.enabled) echo "${RESULT_PROPOSAL_ADAPTIVE_ENABLED-false}" ;;
	*.link.layer) echo ethernet ;;
	*) exit 1 ;;
esac
EOF

cat > "$tmp/bin/sha256sum" <<'EOF'
#!/bin/sh
cat >/dev/null
value=0
[ ! -s "$TEST_SHA_COUNT" ] || value="$(cat "$TEST_SHA_COUNT")"
value=$((value + 1))
printf '%s\n' "$value" > "$TEST_SHA_COUNT"
digit=a
[ "${FINGERPRINT_DRIFT_AT:-0}" -ne "$value" ] || digit=b
hash="$(printf '%064d' 0 | tr 0 "$digit")"
printf '%s  -\n' "$hash"
EOF

cat > "$tmp/bin/uci" <<'EOF'
#!/bin/sh
savedir=""
while [ "$#" -gt 0 ]; do
	case "$1" in
		-q) shift ;;
		-P) savedir="$2"; shift 2 ;;
		*) break ;;
	esac
done
command="${1:-}"
shift || true

assert_locked() {
	exec 7>>"$TEST_LOCK_ROOT/runtime.guard"
	if flock -sn 7; then
		flock -u 7
		exec 7>&-
		echo lock:escaped >> "$TEST_LOG"
		exit 90
	fi
	exec 7>&-
}

state_value() {
	state_key="$1"
	state_line="$(awk -F '|' -v key="$state_key" '$2 == key { line=$0 } END { print line }' "$TEST_COMMITTED" 2>/dev/null)"
	case "$state_line" in
		S'|'*) printf '%s\n' "${state_line#S|$state_key|}"; return 0 ;;
		D'|'*) return 1 ;;
	esac
	[ "${MISSING_KEY:-}" != "$state_key" ] || return 1
	case "$state_key" in
		cake-autorate.test) echo cake_autorate ;;
		cake-autorate.test.sqm_section) echo cake_test ;;
		cake-autorate.test.speedtest_backend) echo auto ;;
		cake-autorate.test.autotune_profile) echo best_overall ;;
		cake-autorate.test.route_mode) echo "${CONFIG_ROUTE_MODE:-main}" ;;
		cake-autorate.test.mwan3_member)
			[ -n "${CONFIG_MWAN3_MEMBER:-}" ] || return 1
			echo "$CONFIG_MWAN3_MEMBER"
			;;
		sqm.cake_test) echo queue ;;
		sqm.cake_test._cake_autorate_managed) echo test ;;
		sqm.cake_test.enabled) echo 1 ;;
		sqm.cake_test.interface) echo eth0 ;;
		cake-autorate.test.*) echo 111 ;;
		*) return 1 ;;
	esac
}

case "$command" in
	get)
		state_value "$1"
		;;
	show)
		case "$1" in
			cake-autorate.test)
				printf '%s\n' \
					"cake-autorate.test=cake_autorate" \
					"cake-autorate.test.route_mode='main'" \
					"cake-autorate.test.sqm_section='cake_test'"
				;;
			sqm.cake_test)
				printf '%s\n' \
					"sqm.cake_test=queue" \
					"sqm.cake_test._cake_autorate_managed='test'" \
					"sqm.cake_test.enabled='1'" \
					"sqm.cake_test.interface='eth0'"
				;;
			*) exit 1 ;;
		esac
		;;
	changes)
		if [ "${PENDING_CHANGES:-0}" = 1 ] ||
		   { [ "${PENDING_SQM_ONLY:-0}" = 1 ] && [ "$1" = sqm ]; } ||
		   { [ "${PENDING_AFTER_STAGE:-0}" = 1 ] && [ -e "$TEST_STAGE_SEEN" ]; }; then
			printf "%s.test.enabled='1'\n" "$1"
		fi
		;;
	set|delete)
		[ -n "$savedir" ] || {
			echo "uci:unexpected-global-$command:$*" >> "$TEST_LOG"
			exit 1
		}
		assert_locked
		mkdir -p "$savedir"
		if [ "$command" = set ]; then
			key="${1%%=*}"
			value="${1#*=}"
			printf 'S|%s|%s\n' "$key" "$value" >> "$savedir/delta"
		else
			printf 'D|%s|\n' "$1" >> "$savedir/delta"
		fi
		printf 'uci:stage:%s:%s\n' "$command" "$1" >> "$TEST_LOG"
		: > "$TEST_STAGE_SEEN"
		;;
	commit)
		[ -n "$savedir" ] || exit 1
		assert_locked
		case "$savedir" in
			*/rollback-uci) kind=rollback ;;
			*) kind=candidate ;;
		esac
		printf 'uci:commit:%s\n' "$kind" >> "$TEST_LOG"
		[ "${FAIL_CANDIDATE_COMMIT:-0}" != 1 ] || [ "$kind" != candidate ] || exit 1
		[ "${FAIL_ROLLBACK_COMMIT:-0}" != 1 ] || [ "$kind" != rollback ] || exit 1
		[ ! -s "$savedir/delta" ] || cat "$savedir/delta" >> "$TEST_COMMITTED"
		;;
	revert)
		echo uci:revert >> "$TEST_LOG"
		exit 1
		;;
	*) exit 1 ;;
esac
EOF

cat > "$tmp/service" <<'EOF'
#!/bin/sh
[ "${CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_BORROW:-0}" = 1 ] || exit 88
[ "${CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_FD:-}" = 8 ] || exit 89
[ "${CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_MODE:-}" = exclusive ] || exit 90
exec 7>>"$TEST_LOCK_ROOT/runtime.guard"
if flock -sn 7; then
	flock -u 7
	exec 7>&-
	echo service:lock-escaped >> "$TEST_LOG"
	exit 91
fi
exec 7>&-
value=0
[ ! -s "$TEST_COUNT" ] || value="$(cat "$TEST_COUNT")"
value=$((value + 1))
echo "$value" > "$TEST_COUNT"
echo "service:$value:$*" >> "$TEST_LOG"
if [ "${KILL_CALLER_ON_RESTART:-0}" = 1 ] && [ "$value" -eq 1 ]; then
	kill -KILL "$PPID"
	exit 0
fi
if [ "$value" -eq 1 ] && [ -n "${SERVICE_MUTATE_KEY:-}" ]; then
	printf 'S|%s|%s\n' "$SERVICE_MUTATE_KEY" "${SERVICE_MUTATE_VALUE:-admin}" >> "$TEST_COMMITTED"
fi
[ "${FAIL_FIRST_RESTART:-0}" = 1 ] && [ "$value" -eq 1 ] && exit 1
[ "${FAIL_ALL_RESTARTS:-0}" != 1 ] || exit 1
exit 0
EOF
cat > "$tmp/route-helper" <<'EOF'
#!/bin/sh
[ "$3" = route-status ] || exit 97
[ "$4" = auto ] || exit 98
value=0
[ ! -s "$TEST_ROUTE_COUNT" ] || value="$(cat "$TEST_ROUTE_COUNT")"
value=$((value + 1))
printf '%s\n' "$value" > "$TEST_ROUTE_COUNT"
[ "${FAIL_ATTEST_AT:-0}" -ne "$value" ] || exit 1
printf '%s\n' '{"attestation":true}'
EOF
chmod +x "$tmp/bin/jsonfilter" "$tmp/bin/sha256sum" "$tmp/bin/uci" "$tmp/service" "$tmp/route-helper"

export PATH="$tmp/bin:$PATH"
export TEST_LOG="$log"
export TEST_COUNT="$count"
export TEST_SHA_COUNT="$sha_count"
export TEST_COMMITTED="$committed"
export TEST_STAGE_SEEN="$stage_seen"
export TEST_ROUTE_COUNT="$route_count"
export TEST_LOCK_ROOT="$tmp/locks"
export CAKE_AUTOTUNE_SCHEDULER_STATE_ROOT="$tmp/state"
export CAKE_AUTOTUNE_SERVICE="$tmp/service"
export CAKE_AUTOTUNE_ROUTE_HELPER="$tmp/route-helper"
export CAKE_AUTORATE_RUNTIME_LOCK_ROOT="$tmp/locks"
export CAKE_AUTORATE_RUNTIME_LOCK_LIB="$root/../cake-autorate-rs/files/usr/libexec/cake-autorate-rs/runtime-lock"
export CAKE_AUTOTUNE_SCHEDULER_SOURCE_ONLY=1
. "$root/root/usr/libexec/cake-autorate-rs/autotune-scheduler"

[ "$daemon_bin" = /usr/sbin/cake-autorated ] || {
	echo 'scheduler default daemon path does not match the installed binary' >&2
	exit 1
}

# Exercise the production daemon identity check against a synthetic /proc
# tree before replacing health probes with deterministic apply-test stubs.
mkdir -p "$tmp/proc/123"
: > "$tmp/fake-daemon"
chmod +x "$tmp/fake-daemon"
ln -s "$tmp/fake-daemon" "$tmp/proc/123/exe"
printf '%s\0--instance\0test\0' "$tmp/fake-daemon" > "$tmp/proc/123/cmdline"
printf '%s\n' 'S|cake-autorate.test.enabled|1' > "$committed"
daemon_bin="$tmp/fake-daemon"
proc_root="$tmp/proc"
daemon_identity_healthy test
mkdir -p "$tmp/proc/124"
ln -s "$tmp/fake-daemon" "$tmp/proc/124/exe"
printf '%s\0--instance\0test\0' "$tmp/fake-daemon" > "$tmp/proc/124/cmdline"
if daemon_identity_healthy test; then
	echo 'duplicate daemon identity was accepted' >&2
	exit 1
fi
rm -rf "$tmp/proc/124"
printf '%s\n' 'S|cake-autorate.test.enabled|0' > "$committed"
if daemon_identity_healthy test; then
	echo 'disabled instance accepted a stale daemon process' >&2
	exit 1
fi
rm -rf "$tmp/proc/123"
daemon_identity_healthy test
: > "$committed"

cat > "$tmp/sqm-health" <<'EOF'
#!/bin/sh
[ "${CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_BORROW:-0}" = 1 ] || exit 88
printf '%s\n' "$*" > "$TEST_SQM_HEALTH_CALL"
EOF
chmod +x "$tmp/sqm-health"
export TEST_SQM_HEALTH_CALL="$tmp/sqm-health.call"
printf '%s\n' \
	'S|cake-autorate.test.manage_sqm|1' \
	'S|cake-autorate.test.sqm_enabled|1' > "$committed"
sqm_health_helper="$tmp/sqm-health"
managed_sqm_healthy test
[ "$(cat "$TEST_SQM_HEALTH_CALL")" = test ]
: > "$committed"

runtime_health_exact() {
	health_value=0
	[ ! -s "$health_count" ] || health_value="$(cat "$health_count")"
	health_value=$((health_value + 1))
	printf '%s\n' "$health_value" > "$health_count"
	[ "${FAIL_HEALTH_AT:-0}" -ne "$health_value" ]
}

fingerprint_a="sha256:$(printf '%064d' 0 | tr 0 a)"
fingerprint_b="sha256:$(printf '%064d' 0 | tr 0 b)"

reset_case() {
	: > "$log"
	: > "$count"
	: > "$sha_count"
	: > "$committed"
	: > "$route_count"
	: > "$health_count"
	rm -rf "$CAKE_AUTOTUNE_SCHEDULER_STATE_ROOT/recovery"
	rm -f "$stage_seen"
	unset FAIL_FIRST_RESTART FAIL_ALL_RESTARTS FAIL_CANDIDATE_COMMIT FAIL_ROLLBACK_COMMIT FAIL_ATTEST_AT FAIL_HEALTH_AT KILL_CALLER_ON_RESTART FINGERPRINT_DRIFT_AT PENDING_CHANGES PENDING_SQM_ONLY PENDING_AFTER_STAGE
	unset SERVICE_MUTATE_KEY SERVICE_MUTATE_VALUE MISSING_KEY
	unset CONFIG_ROUTE_MODE CONFIG_MWAN3_MEMBER
	unset ATTEST_STATE ATTEST_SCHEMA_VERSION ATTEST_TARGET_INTERFACE ATTEST_RESOLVED_INTERFACE ATTEST_ROUTE_INTERFACE ATTEST_SOURCE_IP ATTEST_EXTERNAL_IP ATTEST_ROUTE_MODE ATTEST_MWAN3_MEMBER ATTEST_ROUTE_IDENTITY
	unset RESULT_CORRECTION_ACTION RESULT_CORRECTION_FEASIBLE RESULT_GATE_CODES RESULT_GATE_REQUIRED RESULT_GATE_PASSES RESULT_GATE_ACTUALS RESULT_GATE_LIMITS RESULT_GATE_COMPARISONS
	unset RESULT_PRODUCER RESULT_PROFILE RESULT_RUN_ID RESULT_VALIDATION_PROFILE RESULT_PROPOSAL_SCHEMA_VERSION RESULT_PROPOSAL_PROFILE RESULT_TARGET_GRADE
	unset RESULT_QUALITY_TARGET_REQUIRED RESULT_THROUGHPUT_PRIORITY RESULT_VALIDATION_HARD_PASS RESULT_VALIDATION_SAFETY_PASS RESULT_QUALITY_TARGET_MET RESULT_PROFILE_TARGET_MET RESULT_PROFILE_MANUAL_ONLY RESULT_MANUAL_APPLY_ELIGIBLE
	unset RESULT_REALIZATION_MIN_PERCENT RESULT_REALIZATION_MAX_PERCENT RESULT_RETENTION_PERCENT RESULT_ICMP_DELTA_MAX_MS RESULT_DELAY_MAX_MS RESULT_LOSS_MAX_PERCENT RESULT_CPU_MAX_PERCENT
	unset RESULT_SQM_QDISC RESULT_SQM_SCRIPT RESULT_SQM_CLASSIFICATION RESULT_SQM_SQUASH_DSCP RESULT_SQM_SQUASH_INGRESS RESULT_SQM_INGRESS_ECN RESULT_SQM_EGRESS_ECN RESULT_SQM_IQDISC_OPTS RESULT_SQM_EQDISC_OPTS
	unset RESULT_MINIMUM_KBPS RESULT_BASE_KBPS RESULT_MAXIMUM_KBPS RESULT_CAP_KBPS RESULT_OBSERVED_LOW_KBPS RESULT_OBSERVED_MEDIAN_KBPS RESULT_OBSERVED_HIGH_KBPS RESULT_ACTIVE_THRESHOLD_KBPS RESULT_ADJUST_UP_MS RESULT_DELAY_MS RESULT_ADJUST_DOWN_MS RESULT_HOLD_S RESULT_GROWTH_PERCENT RESULT_PROBE_S RESULT_COOLDOWN_S RESULT_TTL_S RESULT_LINK_OVERHEAD RESULT_LINK_MPU RESULT_CONFIDENCE
	export RESULT_STATE=complete
	export RESULT_SCHEMA_VERSION=6
	export RESULT_PRODUCER=cake-autorate-rs-autotune
	export RESULT_PROFILE=best_overall
	export RESULT_RUN_ID=scheduler-test-run
	export RESULT_VALIDATION_PROFILE=best_overall
	export RESULT_PROPOSAL_SCHEMA_VERSION=3
	export RESULT_PROPOSAL_PROFILE=best_overall
	export RESULT_TARGET_GRADE=A
	export RESULT_QUALITY_TARGET_REQUIRED=true
	export RESULT_THROUGHPUT_PRIORITY=false
	export RESULT_VALIDATION_PASS=true
	export RESULT_VALIDATION_HARD_PASS=true
	export RESULT_VALIDATION_SAFETY_PASS=true
	export RESULT_QUALITY_TARGET_MET=true
	export RESULT_PROFILE_TARGET_MET=true
	export RESULT_PROFILE_MANUAL_ONLY=false
	export RESULT_AUTO_APPLY_ELIGIBLE=true
	export RESULT_MANUAL_APPLY_ELIGIBLE=true
	export RESULT_CONFIDENCE_MODE=normal
	export RESULT_VALIDATION_CONTAMINATED=false
	export RESULT_PHASE_EVIDENCE_COMPLETE=true
	export RESULT_PHASE_CONTAMINATION_SEEN=false
	export RESULT_RUNTIME_RESTORED=true
	export RESULT_RECOVERY_PENDING=false
	export RESULT_CONFIGURATION_WRITTEN=false
	export RESULT_CONFIG_FINGERPRINT="$fingerprint_a"
	export RESULT_JOB_ID=test
	export RESULT_TARGET_INTERFACE=eth0
	export RESULT_RESOLVED_INTERFACE=eth0
	export RESULT_ROUTE_INTERFACE=eth0
	export RESULT_SOURCE_IP=192.0.2.2
	export RESULT_EXTERNAL_IP=198.51.100.2
	export RESULT_ROUTE_MODE=main
	export RESULT_MWAN3_MEMBER=
	export RESULT_ROUTE_IDENTITY='main||eth0|192.0.2.2||main'
}

assert_global_lock_released() {
	exec 7>>"$TEST_LOCK_ROOT/runtime.guard"
	flock -sn 7
	flock -u 7
	exec 7>&-
}

expect_gate_rejection() {
	description="$1"
	if apply_result test '{}' "$fingerprint_a" eth0; then
		echo "apply_result unexpectedly accepted $description" >&2
		exit 1
	fi
	[ ! -s "$log" ] || {
		echo "apply_result staged or committed configuration for $description" >&2
		exit 1
	}
	assert_global_lock_released
}

reset_case
export RESULT_PROPOSAL_ADAPTIVE_ENABLED=false
apply_result test '{}' "$fingerprint_a" eth0
commit_line="$(sed -n '/^uci:commit:candidate$/=' "$log")"
restart_line="$(sed -n '/^service:1:restart$/=' "$log")"
[ -n "$commit_line" ] && [ -n "$restart_line" ] && [ "$commit_line" -lt "$restart_line" ]
! grep -q '^uci:revert$' "$log"
! grep -q 'adaptive_ceiling_enabled' "$log"
! grep -q 'lock-escaped' "$log"
[ "$(cat "$route_count")" = 3 ]
[ "$(cat "$health_count")" = 1 ]
! recovery_transactions_pending
[ "$(uci -q get cake-autorate.test.sqm_script)" = layer_cake.qos ]
[ "$(uci -q get cake-autorate.test.sqm_qdisc_advanced)" = 1 ]
[ "$(uci -q get cake-autorate.test.sqm_iqdisc_opts)" = besteffort ]
[ "$(uci -q get cake-autorate.test.sqm_eqdisc_opts)" = diffserv4 ]
assert_global_lock_released

# Gaming is a complete policy, not just a label: scheduled apply must atomically
# switch to layer_cake + diffserv4, preserve DSCP and stage the A+ validation
# targets under the same rollback journal.
reset_case
export RESULT_PROFILE=gaming
export RESULT_VALIDATION_PROFILE=gaming
export RESULT_PROPOSAL_PROFILE=gaming
export RESULT_TARGET_GRADE=A+
export RESULT_RETENTION_PERCENT=70
export RESULT_ICMP_DELTA_MAX_MS=5
export RESULT_DELAY_MAX_MS=5
export RESULT_LOSS_MAX_PERCENT=1
export RESULT_SQM_SCRIPT=layer_cake.qos
export RESULT_SQM_CLASSIFICATION=diffserv4
export RESULT_SQM_SQUASH_DSCP=false
export RESULT_SQM_SQUASH_INGRESS=false
export RESULT_SQM_IQDISC_OPTS=diffserv4
export RESULT_SQM_EQDISC_OPTS=diffserv4
export RESULT_GATE_ACTUALS='90 90 90 90 90 90 2 2 0 40 2 2 0 40'
export RESULT_GATE_LIMITS='80 80 110 110 70 70 5 5 1 85 5 5 1 85'
apply_result test '{}' "$fingerprint_a" eth0
[ "$(uci -q get cake-autorate.test.autotune_profile)" = gaming ]
[ "$(uci -q get cake-autorate.test.quality_target_delay_ms)" = 5 ]
[ "$(uci -q get cake-autorate.test.throughput_guard_retention_percent)" = 70 ]
[ "$(uci -q get cake-autorate.test.sqm_script)" = layer_cake.qos ]
[ "$(uci -q get cake-autorate.test.sqm_squash_dscp)" = 0 ]
[ "$(uci -q get cake-autorate.test.sqm_squash_ingress)" = 0 ]
[ "$(uci -q get cake-autorate.test.sqm_iqdisc_opts)" = diffserv4 ]
[ "$(uci -q get cake-autorate.test.sqm_eqdisc_opts)" = diffserv4 ]
assert_global_lock_released

# Scheduled re-runs preserve the administrator's adaptive-ceiling choice.
reset_case
export RESULT_PROPOSAL_ADAPTIVE_ENABLED=true
apply_result test '{}' "$fingerprint_a" eth0
! grep -q 'adaptive_ceiling_enabled' "$log"

reset_case; export RESULT_SCHEMA_VERSION=4; expect_gate_rejection 'legacy schema'
reset_case; export RESULT_SCHEMA_VERSION=__missing__; expect_gate_rejection 'unknown schema'
reset_case; export RESULT_PRODUCER=foreign; expect_gate_rejection 'foreign result producer'
reset_case; export RESULT_RUN_ID=__missing__; expect_gate_rejection 'missing immutable run identity'
reset_case; export RESULT_PROFILE=gaming; expect_gate_rejection 'profile/policy mismatch'
reset_case; export RESULT_VALIDATION_PROFILE=fair; expect_gate_rejection 'validation profile mismatch'
reset_case; export RESULT_PROPOSAL_SCHEMA_VERSION=2; expect_gate_rejection 'legacy proposal schema'
reset_case; export RESULT_PROPOSAL_PROFILE=fair; expect_gate_rejection 'proposal profile mismatch'
reset_case; export RESULT_QUALITY_TARGET_REQUIRED=false; expect_gate_rejection 'tampered quality-target policy'
reset_case; export RESULT_THROUGHPUT_PRIORITY=true; expect_gate_rejection 'tampered throughput-priority policy'
reset_case; export RESULT_STATE=failed; expect_gate_rejection 'non-complete result'
reset_case; export RESULT_VALIDATION_PASS=false; expect_gate_rejection 'failed validation'
reset_case; export RESULT_VALIDATION_HARD_PASS=false; expect_gate_rejection 'failed hard safety gates'
reset_case; export RESULT_VALIDATION_SAFETY_PASS=false; expect_gate_rejection 'failed selected-candidate safety gates'
reset_case; export RESULT_QUALITY_TARGET_MET=false; expect_gate_rejection 'unmet unattended quality target'
reset_case; export RESULT_PROFILE_TARGET_MET=false; expect_gate_rejection 'manual profile fallback'
reset_case; export RESULT_PROFILE_MANUAL_ONLY=true; expect_gate_rejection 'manual-only profile result'
reset_case; export RESULT_AUTO_APPLY_ELIGIBLE=false; expect_gate_rejection 'helper-ineligible result'
reset_case; export RESULT_MANUAL_APPLY_ELIGIBLE=false; expect_gate_rejection 'non-reviewable result'
reset_case; export RESULT_CONFIDENCE_MODE=low; expect_gate_rejection 'low-confidence result'
reset_case; export RESULT_VALIDATION_CONTAMINATED=true; expect_gate_rejection 'contaminated validation'
reset_case; export RESULT_PHASE_EVIDENCE_COMPLETE=false; expect_gate_rejection 'incomplete phase evidence'
reset_case; export RESULT_PHASE_CONTAMINATION_SEEN=true; expect_gate_rejection 'phase contamination'
reset_case; export RESULT_RUNTIME_RESTORED=false; expect_gate_rejection 'unrestored runtime state'
reset_case; export RESULT_RECOVERY_PENDING=true; expect_gate_rejection 'pending recovery'
reset_case; export RESULT_RECOVERY_PENDING=__missing__; expect_gate_rejection 'unknown recovery state'
reset_case; export RESULT_CONFIGURATION_WRITTEN=true; expect_gate_rejection 'helper-written configuration'
reset_case; export RESULT_CONFIGURATION_WRITTEN=__missing__; expect_gate_rejection 'unknown configuration-write state'
reset_case; export RESULT_CORRECTION_ACTION=increase; expect_gate_rejection 'non-none correction'
reset_case; export RESULT_CORRECTION_FEASIBLE=false; expect_gate_rejection 'infeasible correction'
reset_case; export RESULT_GATE_PASSES='true true true true true true true false true true true true true true'; expect_gate_rejection 'one failed directional gate'
reset_case; export RESULT_GATE_REQUIRED='true true true true true true false true true true true true true true'; expect_gate_rejection 'forged optional best-overall latency gate'
reset_case; export RESULT_GATE_CODES='download-candidate-realization upload-candidate-realization download-candidate-realization-maximum upload-candidate-realization-maximum download-capacity-retention upload-capacity-retention download-icmp-latency download-transport-latency download-packet-loss download-cpu upload-icmp-latency upload-transport-latency upload-packet-loss'; expect_gate_rejection 'missing directional gate'
reset_case; export RESULT_GATE_CODES='download-candidate-realization upload-candidate-realization download-candidate-realization-maximum upload-candidate-realization-maximum download-capacity-retention upload-capacity-retention download-icmp-latency download-transport-latency download-packet-loss download-cpu upload-icmp-latency upload-transport-latency upload-packet-loss upload-packet-loss'; expect_gate_rejection 'duplicate directional gate'
reset_case; export RESULT_GATE_ACTUALS='90 90 90 90 90 90 10 10 nan 40 10 10 0 40'; expect_gate_rejection 'malformed gate metric'
reset_case; export RESULT_GATE_LIMITS='80 80 110 110 80 80 100 100 0 95 100 100 5 95'; expect_gate_rejection 'non-positive gate limit'
reset_case; export RESULT_GATE_ACTUALS='79 90 90 90 90 90 10 10 0 40 10 10 0 40'; expect_gate_rejection 'forged passing minimum gate'
reset_case; export RESULT_GATE_ACTUALS='90 90 111 90 90 90 10 10 0 40 10 10 0 40'; expect_gate_rejection 'forged passing maximum gate'
reset_case; export RESULT_GATE_COMPARISONS='minimum minimum minimum maximum minimum minimum maximum maximum maximum maximum maximum maximum maximum maximum'; expect_gate_rejection 'mislabeled gate comparison'
reset_case; export RESULT_GATE_ACTUALS='90 90 90 90 90 90 30 10 0 40 10 10 0 40'; expect_gate_rejection 'quality exactly on exclusive grade boundary'

reset_case; export RESULT_MINIMUM_KBPS=0; expect_gate_rejection 'zero minimum rate'
reset_case; export RESULT_BASE_KBPS=4000; expect_gate_rejection 'base below minimum'
reset_case; export RESULT_CAP_KBPS=70000; expect_gate_rejection 'cap below maximum'
reset_case; export RESULT_OBSERVED_HIGH_KBPS=95000; expect_gate_rejection 'observed high above cap'
reset_case; export RESULT_ACTIVE_THRESHOLD_KBPS=6000; expect_gate_rejection 'active threshold above minimum'
reset_case; export RESULT_ADJUST_UP_MS=31; expect_gate_rejection 'latency threshold order'
reset_case; export RESULT_GROWTH_PERCENT=10.1; expect_gate_rejection 'unsafe adaptive growth'
reset_case; export RESULT_LINK_OVERHEAD=1501; expect_gate_rejection 'unsafe link overhead'
reset_case; export RESULT_CONFIDENCE=0; expect_gate_rejection 'zero proposal confidence'

reset_case
export RESULT_CONFIG_FINGERPRINT=__missing__
expect_gate_rejection 'missing helper fingerprint'

reset_case
export RESULT_CONFIG_FINGERPRINT="$fingerprint_b"
expect_gate_rejection 'helper fingerprint mismatch'

reset_case
export FINGERPRINT_DRIFT_AT=1
expect_gate_rejection 'configuration changed before staging'

reset_case
export FINGERPRINT_DRIFT_AT=2
if apply_result test '{}' "$fingerprint_a" eth0; then
	echo 'apply_result accepted a fingerprint change immediately before staging' >&2
	exit 1
fi
! grep -q '^uci:stage:set:' "$log"
! grep -q '^uci:commit:' "$log"
assert_global_lock_released

reset_case
export FINGERPRINT_DRIFT_AT=3
if apply_result test '{}' "$fingerprint_a" eth0; then
	echo 'apply_result accepted a pre-commit fingerprint change' >&2
	exit 1
fi
grep -q '^uci:stage:set:' "$log"
! grep -q '^uci:commit:' "$log"
! grep -q '^service:' "$log"
assert_global_lock_released

reset_case
export FAIL_ATTEST_AT=1
expect_gate_rejection 'failed pre-stage live route attestation'

reset_case
export FAIL_ATTEST_AT=2
if apply_result test '{}' "$fingerprint_a" eth0; then
	echo 'apply_result accepted a failed pre-commit route attestation' >&2
	exit 1
fi
grep -q '^uci:stage:set:' "$log"
! grep -q '^uci:commit:' "$log"
! recovery_transactions_pending
assert_global_lock_released

reset_case
export FAIL_ATTEST_AT=3
if apply_result test '{}' "$fingerprint_a" eth0; then
	echo 'apply_result accepted a failed post-restart route attestation' >&2
	exit 1
fi
grep -q '^uci:commit:candidate$' "$log"
grep -q '^uci:commit:rollback$' "$log"
[ "$(uci -q get cake-autorate.test.base_dl_shaper_rate_kbps)" = 111 ]
! recovery_transactions_pending
assert_global_lock_released

reset_case
export ATTEST_EXTERNAL_IP=203.0.113.9
expect_gate_rejection 'live route external-IP mismatch'

reset_case
export FAIL_HEALTH_AT=1
if apply_result test '{}' "$fingerprint_a" eth0; then
	echo 'apply_result accepted failed exact CAKE/daemon runtime health' >&2
	exit 1
fi
grep -q '^uci:commit:candidate$' "$log"
grep -q '^uci:commit:rollback$' "$log"
grep -q '^service:2:restart$' "$log"
[ "$(uci -q get cake-autorate.test.base_dl_shaper_rate_kbps)" = 111 ]
! recovery_transactions_pending
assert_global_lock_released

reset_case
export RESULT_ROUTE_INTERFACE=eth9
expect_gate_rejection 'route-interface mismatch'

reset_case
export RESULT_ROUTE_IDENTITY='main||eth0|192.0.2.99||main'
expect_gate_rejection 'route-identity mismatch'

reset_case
export RESULT_EXTERNAL_IP=__missing__
expect_gate_rejection 'missing routed external IP'

for malformed_ipv4 in '' 1.2.3.999 1.2.3.4.5 1.2.3,4 '2001:db8::1' '192.0.2.2
198.51.100.2'; do
	reset_case
	export RESULT_SOURCE_IP="$malformed_ipv4"
	expect_gate_rejection "malformed result source IPv4 $malformed_ipv4"
done

reset_case
export ATTEST_SOURCE_IP=1.2.3.999
expect_gate_rejection 'malformed live-attestation source IPv4'

reset_case
export CONFIG_ROUTE_MODE=mwan3
export CONFIG_MWAN3_MEMBER=wanb
export RESULT_ROUTE_MODE=mwan3
export RESULT_MWAN3_MEMBER=wanb
export RESULT_ROUTE_IDENTITY='mwan3|wanb|eth0|192.0.2.2|0x200|201'
apply_result test '{}' "$fingerprint_a" eth0
grep -q '^uci:commit:candidate$' "$log"
grep -q '^service:1:restart$' "$log"

reset_case
export CONFIG_ROUTE_MODE=mwan3
export CONFIG_MWAN3_MEMBER=wanb
export RESULT_ROUTE_MODE=mwan3
export RESULT_MWAN3_MEMBER=wan
export RESULT_ROUTE_IDENTITY='mwan3|wan|eth0|192.0.2.2|0x100|200'
expect_gate_rejection 'mwan3 member mismatch'

reset_case
export PENDING_CHANGES=1
expect_gate_rejection 'pending administrator changes'

reset_case
export PENDING_SQM_ONLY=1
expect_gate_rejection 'pending managed SQM changes'

reset_case
runtime_lock_acquire_global_shared
if apply_result test '{}' "$fingerprint_a" eth0; then
	echo 'apply_result unexpectedly bypassed an active runtime operation' >&2
	exit 1
fi
[ ! -s "$log" ]
runtime_lock_release_global
assert_global_lock_released

reset_case
export PENDING_AFTER_STAGE=1
if apply_result test '{}' "$fingerprint_a" eth0; then
	echo 'apply_result accepted administrator changes created during staging' >&2
	exit 1
fi
grep -q '^uci:stage:set:' "$log"
! grep -q '^uci:commit:' "$log"
assert_global_lock_released

# A failed lifecycle validation rolls back only scheduler-owned committed
# values, while the same exclusive descriptor remains held for both restarts.
reset_case
export FAIL_FIRST_RESTART=1
if apply_result test '{}' "$fingerprint_a" eth0; then
	echo 'apply_result unexpectedly succeeded after a failed restart' >&2
	exit 1
fi
grep -q '^uci:commit:candidate$' "$log"
grep -q '^uci:commit:rollback$' "$log"
grep -q '^service:1:restart$' "$log"
grep -q '^service:2:restart$' "$log"
! grep -q '^uci:revert$' "$log"
[ "$(uci -q get cake-autorate.test.min_dl_shaper_rate_kbps)" = 111 ]
assert_global_lock_released

# A missing preimage is restored as absence, but only after the complete owned
# set has passed candidate/preimage classification.
reset_case
export FAIL_FIRST_RESTART=1
export MISSING_KEY=cake-autorate.test.throughput_guard_enabled
if apply_result test '{}' "$fingerprint_a" eth0; then
	echo 'apply_result unexpectedly succeeded in missing-preimage rollback test' >&2
	exit 1
fi
if uci -q get cake-autorate.test.throughput_guard_enabled >/dev/null 2>&1; then
	echo 'rollback did not delete a scheduler-created option with absent preimage' >&2
	exit 1
fi
! recovery_transactions_pending
assert_global_lock_released

# SIGKILL after candidate commit leaves a tmpfs journal. A later scheduler
# startup/check acquires the exclusive lock, restores the full preimage and
# verifies runtime before removing the obligation.
reset_case
export KILL_CALLER_ON_RESTART=1
(
	# Keep an actual subshell between the mock service and the test harness.
	# Some /bin/sh implementations tail-exec the final background command,
	# which would make the deliberate SIGKILL hit this entire test script.
	apply_result test '{}' "$fingerprint_a" eth0
	crash_status="$?"
	:
	exit "$crash_status"
) &
crashed_apply_pid="$!"
if wait "$crashed_apply_pid" 2>/dev/null; then
	echo 'SIGKILL apply harness unexpectedly succeeded' >&2
	exit 1
fi
recovery_transactions_pending
[ "$(uci -q get cake-autorate.test.base_dl_shaper_rate_kbps)" = 20000 ]
unset KILL_CALLER_ON_RESTART
recover_pending_transactions
[ "$(uci -q get cake-autorate.test.base_dl_shaper_rate_kbps)" = 111 ]
! recovery_transactions_pending
grep -q '"state":"recovered"' "$state_root/test.json"
assert_global_lock_released

# Rollback commit and rollback restart failures remain explicit obligations;
# they are never swallowed or reported as a clean failure.
reset_case
export FAIL_FIRST_RESTART=1
export FAIL_ROLLBACK_COMMIT=1
if apply_result test '{}' "$fingerprint_a" eth0; then
	echo 'rollback-commit failure was accepted' >&2
	exit 1
fi
recovery_transactions_pending
grep -q '"state":"recovery-pending"' "$state_root/test.json"
unset FAIL_FIRST_RESTART FAIL_ROLLBACK_COMMIT
recover_pending_transactions
[ "$(uci -q get cake-autorate.test.base_dl_shaper_rate_kbps)" = 111 ]
! recovery_transactions_pending

reset_case
export FAIL_ALL_RESTARTS=1
if apply_result test '{}' "$fingerprint_a" eth0; then
	echo 'rollback-restart failure was accepted' >&2
	exit 1
fi
recovery_transactions_pending
[ "$(uci -q get cake-autorate.test.base_dl_shaper_rate_kbps)" = 111 ]
grep -q '"state":"recovery-pending"' "$state_root/test.json"
unset FAIL_ALL_RESTARTS
recover_pending_transactions
! recovery_transactions_pending
assert_global_lock_released

# If another actor changed one candidate after commit, recovery is atomic and
# fail-closed: it restores none of the set, leaves an explicit RAM obligation,
# and blocks later tuning rather than producing a mixed candidate/preimage.
reset_case
export FAIL_FIRST_RESTART=1
export SERVICE_MUTATE_KEY=cake-autorate.test.min_dl_shaper_rate_kbps
export SERVICE_MUTATE_VALUE=admin-override
if apply_result test '{}' "$fingerprint_a" eth0; then
	echo 'apply_result unexpectedly succeeded after mutated failed restart' >&2
	exit 1
fi
[ "$(uci -q get cake-autorate.test.min_dl_shaper_rate_kbps)" = admin-override ]
[ "$(uci -q get cake-autorate.test.base_dl_shaper_rate_kbps)" = 20000 ]
! grep -q '^uci:commit:rollback$' "$log"
grep -q '"state":"recovery-pending"' "$state_root/test.json"
recovery_transactions_pending
! grep -q '^uci:revert$' "$log"
assert_global_lock_released

# The scheduler snapshot is taken before launching the helper.  If the current
# instance/SQM fingerprint no longer equals that snapshot, no measurement is
# started and therefore no terminal result can ever reach staging.
reset_case
helper_called="$tmp/helper-called"
cat > "$tmp/helper" <<EOF
#!/bin/sh
: > "$helper_called"
exit 1
EOF
chmod +x "$tmp/helper"
helper="$tmp/helper"
run_section test eth0 "$(date +%s)" 0 "$fingerprint_b"
[ ! -e "$helper_called" ]
grep -q '"state":"deferred"' "$state_root/test.json"

# Exhaustive polling: recovery is transient, retryable measurement outcomes
# finish immediately as deferred, unknown states fail closed, and every
# started attempt consumes budget + advances the RAM retry throttle.
cat > "$tmp/poll-helper" <<'EOF'
#!/bin/sh
[ "$7" = best_overall ] || exit 3
[ "$8" = 0 ] || exit 4
case "$3" in
	start)
		printf '%s\n' start >> "$TEST_POLL_LOG"
		exit "${POLL_START_RC:-0}"
		;;
	status)
		value=0
		[ ! -s "$TEST_POLL_COUNT" ] || value="$(cat "$TEST_POLL_COUNT")"
		value=$((value + 1))
		printf '%s\n' "$value" > "$TEST_POLL_COUNT"
		state="$(sed -n "${value}p" "$TEST_POLL_SEQUENCE")"
		[ -n "$state" ] || state="$(tail -n 1 "$TEST_POLL_SEQUENCE")"
		[ "$state" != command-error ] || exit 1
		printf 'SCHED:%s:%s\n' "$state" "${POLL_REASON:-}"
		;;
	cancel)
		printf '%s\n' cancel >> "$TEST_POLL_LOG"
		exit "${POLL_CANCEL_RC:-0}"
		;;
	*) exit 2 ;;
esac
EOF
chmod +x "$tmp/poll-helper"
export TEST_POLL_LOG="$tmp/poll.log"
export TEST_POLL_COUNT="$tmp/poll.count"
export TEST_POLL_SEQUENCE="$tmp/poll.sequence"
helper="$tmp/poll-helper"
poll_interval_s=1
interface_bytes() { printf '%s\n' "${TEST_INTERFACE_BYTES:-3000}"; }
extract() {
	case "$1" in
		SCHED:*)
			poll_payload="${1#SCHED:}"
			poll_state="${poll_payload%%:*}"
			poll_reason="${poll_payload#*:}"
			case "$2" in
				@.state) printf '%s\n' "$poll_state" ;;
				@.reason) [ -n "$poll_reason" ] && printf '%s\n' "$poll_reason" ;;
				*) jsonfilter -s "$1" -e "$2" 2>/dev/null ;;
			esac
			;;
		*) jsonfilter -s "$1" -e "$2" 2>/dev/null ;;
	esac
}

prepare_poll_case() {
	reset_case
	rm -f "$state_root/test.json" "$state_root/test.last" "$state_root/test.last-attempt" "$state_root/test.budget"
	: > "$TEST_POLL_LOG"
	: > "$TEST_POLL_COUNT"
	unset POLL_REASON POLL_START_RC POLL_CANCEL_RC
}

assert_attempt_accounted() {
	[ -s "$state_root/test.last-attempt" ]
	read -r budget_day budget_bytes < "$state_root/test.budget"
	[ -n "$budget_day" ] && [ "$budget_bytes" = 2000 ]
}

prepare_poll_case
printf '%s\n' recovery-pending complete > "$TEST_POLL_SEQUENCE"
run_section test eth0 "$(date +%s)" 1000 "$fingerprint_a"
grep -q '"state":"proposal_ready"' "$state_root/test.json"
[ "$(cat "$TEST_POLL_COUNT")" = 2 ]
assert_attempt_accounted

prepare_poll_case
printf '%s\n' background-blocked > "$TEST_POLL_SEQUENCE"
run_section test eth0 "$(date +%s)" 1000 "$fingerprint_a"
grep -q '"state":"deferred"' "$state_root/test.json"
[ "$(cat "$TEST_POLL_COUNT")" = 1 ]
assert_attempt_accounted

prepare_poll_case
export POLL_REASON=config-changed
printf '%s\n' inconclusive > "$TEST_POLL_SEQUENCE"
run_section test eth0 "$(date +%s)" 1000 "$fingerprint_a"
grep -q 'config-changed' "$state_root/test.json"
grep -q '"state":"deferred"' "$state_root/test.json"
assert_attempt_accounted

prepare_poll_case
printf '%s\n' future-unknown-state > "$TEST_POLL_SEQUENCE"
run_section test eth0 "$(date +%s)" 1000 "$fingerprint_a"
grep -q '"state":"failed"' "$state_root/test.json"
grep -q 'unknown state' "$state_root/test.json"
assert_attempt_accounted

prepare_poll_case
printf '%s\n' command-error > "$TEST_POLL_SEQUENCE"
run_section test eth0 "$(date +%s)" 1000 "$fingerprint_a"
grep -q 'status could not be read' "$state_root/test.json"
assert_attempt_accounted

prepare_poll_case
export POLL_START_RC=1
printf '%s\n' complete > "$TEST_POLL_SEQUENCE"
run_section test eth0 "$(date +%s)" 1000 "$fingerprint_a"
grep -q 'Unable to start' "$state_root/test.json"
[ "$(cat "$TEST_POLL_LOG")" = start ]
assert_attempt_accounted

for terminal_state in failed cancelled idle; do
	prepare_poll_case
	printf '%s\n' "$terminal_state" > "$TEST_POLL_SEQUENCE"
	run_section test eth0 "$(date +%s)" 1000 "$fingerprint_a"
	grep -q 'no configuration was written' "$state_root/test.json"
	[ "$(cat "$TEST_POLL_COUNT")" = 1 ]
	assert_attempt_accounted
done

prepare_poll_case
job_timeout_s=1
printf '%s\n' running > "$TEST_POLL_SEQUENCE"
run_section test eth0 "$(date +%s)" 1000 "$fingerprint_a"
grep -q 'timed out' "$state_root/test.json"
grep -qx cancel "$TEST_POLL_LOG"
assert_attempt_accounted
job_timeout_s=1800

prepare_poll_case
job_timeout_s=1
printf '%s\n' recovery-pending > "$TEST_POLL_SEQUENCE"
run_section test eth0 "$(date +%s)" 1000 "$fingerprint_a"
grep -q '"state":"recovery-pending"' "$state_root/test.json"
if grep -qx cancel "$TEST_POLL_LOG"; then
	echo 'scheduler cancelled a recovery-pending helper' >&2
	exit 1
fi
assert_attempt_accounted
job_timeout_s=1800

# check_section uses last-attempt for retry throttling independently of the
# successful-calibration interval and refuses to run while any recovery
# transaction remains.
prepare_poll_case
printf '%s\n' \
	'S|cake-autorate.test.scheduled_autotune_enabled|1' \
	'S|cake-autorate.test.scheduled_autotune_interval_hours|1' \
	'S|cake-autorate.test.scheduled_autotune_window_start_hour|0' \
	'S|cake-autorate.test.scheduled_autotune_window_end_hour|0' \
	'S|cake-autorate.test.wan_if|eth0' > "$committed"
resolve_interface() { printf '%s\n' eth0; }
idle_ready() { return 0; }
budget_ready() { return 0; }
configuration_fingerprint() { printf '%s\n' "$fingerprint_a"; }
scheduled_called="$tmp/scheduled-called"
rm -f "$scheduled_called"
run_section() { : > "$scheduled_called"; }
printf '%s\n' "$(date +%s)" > "$state_root/test.last-attempt"
check_section test
if [ -e "$scheduled_called" ]; then
	printf '%s\n' 'retry throttle unexpectedly invoked run_section' >&2
	ls -l "$scheduled_called" >&2 || true
	exit 1
fi
printf '%s\n' 0 > "$state_root/test.last-attempt"
check_section test
[ -e "$scheduled_called" ]
rm -f "$scheduled_called"
mkdir -p "$recovery_root/apply-blocked"
check_section test
[ ! -e "$scheduled_called" ]
grep -q '"state":"recovery-pending"' "$state_root/test.json"

# If a real libuci CLI exists on the test host, exercise private -P deltas
# against an isolated config directory. CI hosts without uci explicitly skip.
if [ -n "${REAL_UCI_BIN:-}" ] && [ -x "$REAL_UCI_BIN" ]; then
	mkdir -p "$tmp/real-uci/config" "$tmp/real-uci/candidate" "$tmp/real-uci/admin" "$tmp/real-uci/read"
	printf '%s\n' \
		"config cake_autorate 'test'" \
		"\toption rate '111'" \
		"\toption untouched 'original'" > "$tmp/real-uci/config/cake"
	"$REAL_UCI_BIN" -c "$tmp/real-uci/config" -P "$tmp/real-uci/candidate" set cake.test.rate=20000
	[ "$("$REAL_UCI_BIN" -c "$tmp/real-uci/config" -P "$tmp/real-uci/read" get cake.test.rate)" = 111 ]
	"$REAL_UCI_BIN" -c "$tmp/real-uci/config" -P "$tmp/real-uci/admin" set cake.test.untouched=admin
	"$REAL_UCI_BIN" -c "$tmp/real-uci/config" -P "$tmp/real-uci/admin" commit cake
	"$REAL_UCI_BIN" -c "$tmp/real-uci/config" -P "$tmp/real-uci/candidate" commit cake
	[ "$("$REAL_UCI_BIN" -c "$tmp/real-uci/config" -P "$tmp/real-uci/read" get cake.test.rate)" = 20000 ]
	[ "$("$REAL_UCI_BIN" -c "$tmp/real-uci/config" -P "$tmp/real-uci/read" get cake.test.untouched)" = admin ]
	echo 'real uci -P private-delta/concurrent-option test passed'
else
	echo 'real uci -P integration skipped (uci unavailable on host)'
fi

echo 'autotune scheduler fingerprint/transaction tests passed'
