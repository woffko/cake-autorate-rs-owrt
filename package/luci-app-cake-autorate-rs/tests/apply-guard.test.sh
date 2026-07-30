#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
helper_real="$base/root/usr/libexec/cake-autorate-rs/apply-guard"
fixtures="$base/tests/fixtures/apply-guard"
runtime_lock="$(CDPATH= cd -- "$base/../cake-autorate-rs/files/usr/libexec/cake-autorate-rs" && pwd)/runtime-lock"
work="${TMPDIR:-/tmp}/cake-apply-guard-test.$$"
helper="$work/apply-guard-wrapper"
config="$work/config"
autotune="$work/autotune"
guard="$work/guard"
mkdir -p "$config" "$autotune/wan_sqm" "$work/sys/pppoe-wan" "$work/sys/ifb4pppoe-wan" "$work/proc/200"
cat > "$helper" <<EOF
#!/bin/sh
if [ "\${1:-}" = arm ]; then
	case "\${9:-}" in
		disable_sqm) exec "$helper_real" "\$@" p-222222222222222222222222 ;;
		*) exec "$helper_real" "\$@" p-111111111111111111111111 ;;
	esac
fi
exec "$helper_real" "\$@"
EOF
chmod 700 "$helper"
cleanup_test_work() {
	if [ "${CAKE_AUTORATE_KEEP_TEST_WORK:-0}" = 1 ]; then
		printf 'Preserved apply-guard test workspace: %s\n' "$work" >&2
	else
		rm -rf "$work"
	fi
}
trap cleanup_test_work EXIT INT TERM
printf '%s\n' '11111111-2222-3333-4444-555555555555' > "$work/boot-id"

export PATH="$fixtures:$PATH"
export CAKE_AUTORATE_CONFIG_DIR="$config"
export CAKE_AUTORATE_AUTOTUNE_DIR="$autotune"
export CAKE_AUTORATE_APPLY_GUARD_DIR="$guard"
export CAKE_AUTORATE_APPLY_RECEIPT_DIR="$work/receipts"
export CAKE_AUTORATE_AUTOTUNE="$fixtures/autotune"
export CAKE_AUTORATE_SPEEDTEST="$fixtures/speedtest"
export CAKE_AUTORATE_JSONFILTER="$fixtures/jsonfilter"
export CAKE_AUTORATE_RUNTIME_LOCK_LIB="$runtime_lock"
export CAKE_AUTORATE_DAEMON=/usr/sbin/cake-autorated
export CAKE_AUTORATE_TC="$fixtures/tc"
export CAKE_AUTORATE_SYS_CLASS_NET="$work/sys"
export CAKE_AUTORATE_PROC_ROOT="$work/proc"
export CAKE_AUTORATE_SQM_INIT="$fixtures/sqm-init"
export CAKE_AUTORATE_UBUS="$fixtures/ubus"
export CAKE_AUTORATE_BOOT_ID_FILE="$work/boot-id"
export APPLY_GUARD_SQM_INIT_STATE="$work/sqm-init.state"
export CAKE_AUTORATE_APPLY_GUARD_TTL_S=180
export CAKE_AUTORATE_APPLY_GUARD_POSTCHECK_WAIT_S=1
export CAKE_AUTORATE_APPLY_GUARD_ORPHAN_AGE_S=60
export APPLY_GUARD_DAEMON_RUNNING=0
printf '%s\n' disabled > "$APPLY_GUARD_SQM_INIT_STATE"
printf '%s\n' '200 (cake-autorated) S 1 1 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 424242' > "$work/proc/200/stat"

cat > "$config/cake-autorate" <<'EOF'
wan_sqm|__type|cake_autorate
wan_sqm|enabled|1
wan_sqm|wan_if|pppoe-wan
wan_sqm|route_mode|main
wan_sqm|sqm_section|cake_wan_sqm
wan_sqm|sqm_qdisc|cake
wan_sqm|sqm_script|piece_of_cake.qos
wan_sqm|sqm_linklayer|none
wan_sqm|sqm_overhead|0
wan_sqm|sqm_tcMPU|0
wan_sqm|adaptive_ceiling_enabled|0
wan_sqm|unrelated_preserved|keep-me
EOF
cat > "$config/sqm" <<'EOF'
cake_wan_sqm|__type|queue
cake_wan_sqm|_cake_autorate_managed|wan_sqm
cake_wan_sqm|enabled|1
cake_wan_sqm|interface|pppoe-wan
cake_wan_sqm|download|50000
cake_wan_sqm|upload|10000
EOF

fingerprint="sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
cat > "$autotune/wan_sqm/result.json" <<EOF
{
  "state":"complete", "schema_version":8, "result_available":true,
  "producer":"cake-autorate-rs-autotune", "profile":"best_overall",
  "run_id":"apply-guard-test-run", "result_class":"trusted",
  "confidence":{"overall_percent":100,"capacity_download_percent":100,
    "capacity_upload_percent":100,"quality_percent":100,"reasons":[]},
  "auto_apply_eligible":true,
  "manual_apply_eligible":true,
  "phase_evidence_complete":true, "phase_contamination_seen":false,
  "runtime_restored":true, "recovery_pending":false,
  "configuration_written":false, "conservative":false,
  "confidence_mode":"normal", "job_id":"wan_sqm",
  "target_interface":"pppoe-wan", "resolved_interface":"pppoe-wan",
  "route_interface":"pppoe-wan", "route_mode":"main", "mwan3_member":"",
  "source_ip":"192.0.2.10", "external_ip":"192.0.2.20",
  "route_identity":"main||pppoe-wan|192.0.2.10||main",
  "config_fingerprint":"$fingerprint",
  "runs":[{"backend":"speedtest-go","server_id":"17372"}],
	"proposals":[
	  {"schema_version":1,"proposal_id":"p-111111111111111111111111","rank":1,
	    "action":"apply_sqm","topology":"both_shaped","is_primary":true,
	    "applicable":true,"hard_safety_pass":true,"profile_target_met":true,
	    "profile_objectives_met":true,"grade":"A","effective_delta_ms":10,
	    "confidence_percent":100,"unmet_objectives":[],
	    "evidence":{"validation":"validation","confirmation":"bidirectional_confirmation"},
	    "configuration":{}},
	  {"schema_version":1,"proposal_id":"p-222222222222222222222222","rank":2,
	    "action":"disable_sqm","topology":"no_sqm","is_primary":false,
	    "applicable":true,"hard_safety_pass":true,"profile_target_met":true,
	    "profile_objectives_met":true,"grade":"A","effective_delta_ms":10,
	    "confidence_percent":100,"unmet_objectives":[],
	    "evidence":{"control":"fair_outcome.no_sqm_control"},"configuration":null}
	],
  "validation_thresholds":{"candidate_realization_min_percent":80,
    "candidate_realization_max_percent":110,"capacity_retention_min_percent":80,
    "throughput_safety_floor_percent":50,
    "delay_max_ms":30,"manual_latency_review_max_ms":60,
    "loss_max_percent":3,"cpu_max_percent":85},
  "validation":{"profile":"best_overall","pass":true,"hard_pass":true,"safety_pass":true,
    "profile_objectives_met":true,"quality_target_met":true,"actual_grade":"A","effective_delta_ms":10,
    "contaminated":false,"candidate_base":{"download_kbps":80000,"upload_kbps":20000},
    "correction":{"action":"none","feasible":true}},
	  "profile_outcome":{"mode":"target-a-met","objective":"balanced-quality-throughput",
	    "target_grade":"A","target_met":true,"actual_grade":"A","capacity_floor_percent":80,
	    "capacity_floor_met":true,"throughput_safety_floor_percent":50,
	    "throughput_safety_floor_met":true,"deep_runtime_minimum":false,
	    "runtime_minimum_retention":null,"infeasible_reason":"","manual_only":false,
	    "selected_pair":{"download_kbps":80000,"upload_kbps":20000},
	    "bidirectional_confirmation":{"tested":true,"safety_pass":true,"auto_apply_pass":true,
	      "effective_delta_ms":10,"loss_percent":0}},
	"bidirectional_confirmation":{"tested":true,"safety_pass":true,"auto_apply_pass":true,
	  "grade":"A","target_rates_kbps":{"download":80000,"upload":20000},
	  "achieved_kbps":{"download":80000,"upload":20000},
	  "realization_percent":{"download":100,"upload":100},
	  "effective_delta_ms":10,"icmp_delta_ms":5,"transport_delta_ms":10,
	  "loss_percent":0,"cpu_peak_percent":40,"cpu_warning":false,"advisory_reason":"none"},
  "profile_search":{
    "download":{"schema_version":2,"profile":"best_overall","direction":"download",
      "action":"complete","selected":{"candidate_kbps":80000,"safety_pass":true,"target_met":true}},
    "upload":{"schema_version":2,"profile":"best_overall","direction":"upload",
      "action":"complete","selected":{"candidate_kbps":20000,"safety_pass":true,"target_met":true}}},
  "pinger_plan":{"recommended_method":"fping","recommended_no_pingers":3,
    "recommended_reflectors":["1.1.1.1","9.9.9.9","8.8.8.8"]},
  "proposal":{
    "schema_version":4,"profile":"best_overall","target_grade":"A",
    "quality_target_required":true,"throughput_priority":false,
    "download":{"minimum_kbps":40000,"exploration_minimum_kbps":40000,
      "runtime_minimum_kbps":null,"base_kbps":80000,"maximum_kbps":80000,
      "tested_safe_maximum_kbps":80000,"exploration_cap_kbps":95000,
      "absolute_cap_kbps":95000,"service_hard_cap_kbps":null,
      "ceiling_evidence":"shaped_validation","cap_source":"measured_raw",
      "observed_low_kbps":85000,"observed_median_kbps":88000,
      "observed_high_kbps":92000},
    "upload":{"minimum_kbps":10000,"exploration_minimum_kbps":10000,
      "runtime_minimum_kbps":null,"base_kbps":20000,"maximum_kbps":20000,
      "tested_safe_maximum_kbps":20000,"exploration_cap_kbps":25000,
      "absolute_cap_kbps":25000,"service_hard_cap_kbps":null,
      "ceiling_evidence":"shaped_validation","cap_source":"measured_raw",
      "observed_low_kbps":21000,"observed_median_kbps":23000,
      "observed_high_kbps":24500},
    "active_threshold_kbps":2000,
    "thresholds_ms":{"adjust_up":6,"delay":15,"adjust_down":40},
	"adaptive_ceiling":{"enabled":true,"policy":"passive_bounded","hold_s":15,"growth_percent":3,
	  "probe_s":8,"cooldown_s":45,"failed_bound_ttl_s":900},
	"access":{"medium":"unknown","source":"legacy_default","confidence_percent":0},
    "validation":{"candidate_realization_min_percent":80,
      "candidate_realization_max_percent":110,"capacity_retention_min_percent":80,
      "icmp_delta_max_ms":30,"transport_delta_max_ms":30,
      "loss_max_percent":3,"cpu_max_percent":85},
    "sqm":{"qdisc":"cake","script":"layer_cake.qos",
      "classification":"diffserv4","squash_dscp":true,"squash_ingress":true,
      "ingress_ecn":"ECN","egress_ecn":"NOECN",
      "iqdisc_opts":"besteffort","eqdisc_opts":"diffserv4"},
    "link":{"kind":"ethernet","layer":"none","overhead":0,"mpu":0}
  }
}
EOF
node - "$autotune/wan_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const file = process.argv[2];
const result = JSON.parse(fs.readFileSync(file, 'utf8'));
result.proposals[0].configuration = structuredClone(result.proposal);
fs.writeFileSync(file, JSON.stringify(result));
EOF
chmod 600 "$autotune/wan_sqm/result.json"

cp "$autotune/wan_sqm/result.json" "$work/result.valid"
sed -i 's/"base_kbps":80000/"base_kbps":0/' "$autotune/wan_sqm/result.json"
if $helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint" >/dev/null 2>&1; then
	echo "apply guard armed an invalid proposal" >&2
	exit 1
fi
if find "$guard" -mindepth 1 -print -quit 2>/dev/null | grep -q .; then
	echo "failed arm leaked a root-owned apply token" >&2
	exit 1
fi
cp "$work/result.valid" "$autotune/wan_sqm/result.json"

# The selected proposal carries a complete immutable copy of every setting
# which may reach UCI.  A valid canonical proposal must not authorize a
# candidate whose embedded configuration was changed after measurement.
node - "$work/result.valid" "$autotune/wan_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
result.proposals[0].configuration.download.observed_median_kbps++;
fs.writeFileSync(process.argv[3], JSON.stringify(result));
EOF
if $helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint" >/dev/null 2>&1; then
	echo "apply guard accepted a selected-candidate configuration mismatch" >&2
	exit 1
fi
if find "$guard" -mindepth 1 -print -quit 2>/dev/null | grep -q .; then
	echo "rejected selected-candidate configuration mismatch leaked an apply token" >&2
	exit 1
fi
cp "$work/result.valid" "$autotune/wan_sqm/result.json"

# Background-aware confidence changes whether a result may be unattended, not
# whether an explicitly reviewed, otherwise safe proposal may be applied.
# Generate the positive non-trusted cases and deliberately malformed/tampered
# variants from the same attested result so all unrelated hard gates stay live.
node - "$work/result.valid" "$work" <<'EOF'
const fs = require('node:fs');
const path = require('node:path');
const source = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const root = process.argv[3];

function write(name, mutate) {
	const result = structuredClone(source);
	mutate(result);
	result.proposals[0].configuration = structuredClone(result.proposal);
	fs.writeFileSync(path.join(root, `confidence-${name}.json`), JSON.stringify(result));
}

function makeNonTrusted(result, resultClass, overall, download, upload, quality) {
	result.result_class = resultClass;
	result.confidence = {
		overall_percent: overall,
		capacity_download_percent: download,
		capacity_upload_percent: upload,
		quality_percent: quality,
		reasons: [ { code: 'background-share', scope: 'all', message: 'Concurrent traffic reduced confidence.' } ]
	};
	result.auto_apply_eligible = false;
	result.conservative = true;
	result.confidence_mode = 'low';
	result.phase_contamination_seen = true;
	result.validation.contaminated = true;
}

write('provisional', result => makeNonTrusted(result, 'provisional', 70, 70, 88, 75));
write('estimated', result => makeNonTrusted(result, 'estimated', 25, 25, 35, 40));
write('malformed', result => { result.confidence.overall_percent = 'seventy'; });
write('missing-top-level', result => {
	delete result.confidence;
	result.proposal.confidence = 100;
});
write('unknown-class', result => { result.result_class = 'high'; });
write('minimum-mismatch', result => {
	result.confidence = { overall_percent: 71, capacity_download_percent: 70,
		capacity_upload_percent: 88, quality_percent: 75, reasons: [] };
});
write('trusted-low', result => {
	result.result_class = 'trusted';
	result.confidence = { overall_percent: 70, capacity_download_percent: 70,
		capacity_upload_percent: 88, quality_percent: 75, reasons: [] };
});
write('provisional-clean-high', result => {
	result.result_class = 'provisional';
	result.confidence = { overall_percent: 90, capacity_download_percent: 90,
		capacity_upload_percent: 95, quality_percent: 92, reasons: [] };
	result.auto_apply_eligible = false;
});
write('provisional-contaminated-high', result =>
	makeNonTrusted(result, 'provisional', 90, 90, 95, 92));
write('estimated-high', result => makeNonTrusted(result, 'estimated', 40, 40, 55, 45));
write('nontrusted-auto', result => {
	makeNonTrusted(result, 'provisional', 70, 70, 88, 75);
	result.auto_apply_eligible = true;
});
write('trusted-contaminated', result => {
	result.result_class = 'trusted';
	result.confidence = { overall_percent: 90, capacity_download_percent: 90,
		capacity_upload_percent: 95, quality_percent: 92, reasons: [] };
	result.phase_contamination_seen = true;
	result.validation.contaminated = true;
});
write('legacy-schema', result => { result.schema_version = 7; });
EOF

for confidence_class in provisional estimated; do
	cp "$work/confidence-$confidence_class.json" "$autotune/wan_sqm/result.json"
	confidence_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint")"
	confidence_token="$(printf '%s\n' "$confidence_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
	[ "${#confidence_token}" -eq 64 ]
	$helper abort "$confidence_token" >/dev/null
done

for rejected_contract in malformed missing-top-level unknown-class minimum-mismatch trusted-low provisional-clean-high provisional-contaminated-high estimated-high nontrusted-auto trusted-contaminated legacy-schema; do
	cp "$work/confidence-$rejected_contract.json" "$autotune/wan_sqm/result.json"
	if $helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint" >/dev/null 2>&1; then
		echo "apply guard accepted invalid confidence contract: $rejected_contract" >&2
		exit 1
	fi
	if find "$guard" -mindepth 1 -print -quit 2>/dev/null | grep -q .; then
		echo "rejected confidence contract leaked an apply token: $rejected_contract" >&2
		exit 1
	fi
done
cp "$work/result.valid" "$autotune/wan_sqm/result.json"

# Final simultaneous evidence has its own typed manual-review boundary. A
# finite profile-latency miss and 50..80% candidate realization may be accepted
# explicitly; packet loss, sub-50%, shaper overshoot, and inconsistent verdicts
# remain non-overridable.
node - "$work/result.valid" "$work" <<'EOF'
const fs = require('node:fs');
const path = require('node:path');
const source = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const root = process.argv[3];

function sync(result) {
	result.profile_outcome.bidirectional_confirmation = {
		tested: result.bidirectional_confirmation.tested,
		safety_pass: result.bidirectional_confirmation.safety_pass,
		auto_apply_pass: result.bidirectional_confirmation.auto_apply_pass,
		effective_delta_ms: result.bidirectional_confirmation.effective_delta_ms,
		loss_percent: result.bidirectional_confirmation.loss_percent,
	};
}
function write(name, mutate) {
	const result = structuredClone(source);
	mutate(result);
	sync(result);
	fs.writeFileSync(path.join(root, `bidi-${name}.json`), JSON.stringify(result));
}
write('latency', result => {
	result.auto_apply_eligible = false;
	Object.assign(result.bidirectional_confirmation, {
		safety_pass: true, auto_apply_pass: false, grade: 'B',
		effective_delta_ms: 45, icmp_delta_ms: 20, transport_delta_ms: 45,
		advisory_reason: 'loaded-latency-target-missed',
	});
	Object.assign(result.profile_outcome, {
		mode: 'balanced-fallback', target_met: false, actual_grade: 'B', manual_only: true,
	});
});
write('far-latency', result => {
	result.auto_apply_eligible = false;
	Object.assign(result.bidirectional_confirmation, {
		safety_pass: false, auto_apply_pass: false, grade: 'C',
		effective_delta_ms: 61, icmp_delta_ms: 61, transport_delta_ms: 61,
		advisory_reason: 'loaded-latency-target-missed',
	});
	Object.assign(result.profile_outcome, {
		mode: 'balanced-fallback', target_met: false, actual_grade: 'C', manual_only: true,
	});
});
write('low-realization', result => {
	result.auto_apply_eligible = false;
	Object.assign(result.bidirectional_confirmation, {
		safety_pass: true, auto_apply_pass: false,
		achieved_kbps: { download: 60000, upload: 18000 },
		realization_percent: { download: 75, upload: 90 },
		advisory_reason: 'simultaneous-throughput-confidence-low',
	});
	result.profile_outcome.manual_only = true;
});
write('loss', result => {
	result.auto_apply_eligible = false;
	Object.assign(result.bidirectional_confirmation, {
		safety_pass: false, auto_apply_pass: false, loss_percent: 4,
		advisory_reason: 'packet-loss-limit-exceeded',
	});
	result.profile_outcome.manual_only = true;
});
write('sub50', result => {
	result.auto_apply_eligible = false;
	Object.assign(result.bidirectional_confirmation, {
		safety_pass: false, auto_apply_pass: false,
		achieved_kbps: { download: 39200, upload: 20000 },
		realization_percent: { download: 49, upload: 100 },
		advisory_reason: 'simultaneous-throughput-confidence-low',
	});
	result.profile_outcome.manual_only = true;
});
write('overshoot', result => {
	result.auto_apply_eligible = false;
	Object.assign(result.bidirectional_confirmation, {
		safety_pass: false, auto_apply_pass: false,
		achieved_kbps: { download: 88800, upload: 20000 },
		realization_percent: { download: 111, upload: 100 },
		advisory_reason: 'simultaneous-throughput-confidence-low',
	});
	result.profile_outcome.manual_only = true;
});
write('inconsistent', result => {
	result.bidirectional_confirmation.auto_apply_pass = false;
});
EOF
for reviewable_bidi in latency low-realization; do
	cp "$work/bidi-$reviewable_bidi.json" "$autotune/wan_sqm/result.json"
	bidi_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint")"
	bidi_token="$(printf '%s\n' "$bidi_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
	[ "${#bidi_token}" -eq 64 ]
	$helper abort "$bidi_token" >/dev/null
done
for rejected_bidi in far-latency loss sub50 overshoot inconsistent; do
	cp "$work/bidi-$rejected_bidi.json" "$autotune/wan_sqm/result.json"
	if $helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint" >/dev/null 2>&1; then
		echo "apply guard accepted non-overridable bidirectional failure: $rejected_bidi" >&2
		exit 1
	fi
done
cp "$work/result.valid" "$autotune/wan_sqm/result.json"

# A repeatable upload-only comparison is independently applicable even when
# the old both-shaped confirmation is outside its manual latency boundary.
# Every derived number is recomputed by apply-guard from the underlying
# throughput and delay measurements; copied pass flags alone are insufficient.
node - "$work/result.valid" "$work" <<'EOF'
const fs = require('node:fs');
const path = require('node:path');
const source = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const root = process.argv[3];

function uploadObservation(download, upload, delta) {
	const downloadGain = (download - 80000) * 100 / 80000;
	const uploadRealization = upload * 100 / 20000;
	return {
		pass: true,
		candidate_pass: true,
		hard_safety_pass: true,
		material_benefit: true,
		profile_target_met: true,
		effective_delta_ms: delta,
		loss_percent: 0,
		upload_realization_percent: uploadRealization,
		download_gain_percent: downloadGain,
		delay_improvement_ms: 61 - delta,
		grade: delta < 5 ? 'A+' : delta < 30 ? 'A' : delta < 60 ? 'B' : 'C',
		observation: {
			topology: 'upload_only_shaped',
			direction: 'both',
			throughput_kbps: { download_kbps: download, upload_kbps: upload },
			measurement_evidence: {
				valid: true,
				shaper_bypassed: true,
				sqm_paused: false,
				sqm_bypass_mode: 'ingress-only-autotune',
			},
		},
	};
}

function makeUploadOnly() {
	const result = structuredClone(source);
	result.auto_apply_eligible = false;
	Object.assign(result.bidirectional_confirmation, {
		safety_pass: false,
		auto_apply_pass: false,
		grade: 'C',
		effective_delta_ms: 61,
		icmp_delta_ms: 61,
		transport_delta_ms: 61,
		advisory_reason: 'loaded-latency-target-missed',
	});
	result.profile_outcome.bidirectional_confirmation = {
		tested: true,
		safety_pass: false,
		auto_apply_pass: false,
		effective_delta_ms: 61,
		loss_percent: 0,
	};
	Object.assign(result.profile_outcome, {
		mode: 'balanced-fallback',
		target_met: false,
		actual_grade: 'C',
		manual_only: true,
	});
	result.proposals[0] = {
		schema_version: 1,
		proposal_id: 'p-111111111111111111111111',
		rank: 1,
		action: 'apply_sqm',
		topology: 'upload_only_shaped',
		is_primary: true,
		applicable: true,
		hard_safety_pass: true,
		profile_target_met: true,
		profile_objectives_met: true,
		grade: 'A',
		effective_delta_ms: 27,
		confidence_percent: 90,
		unmet_objectives: [],
		evidence: { recommendation: 'directional_comparisons.upload_only' },
		configuration: structuredClone(result.proposal),
	};
	result.directional_comparisons = {
		upload_only: {
		tested: true,
		recommended_topology: 'upload_only_shaped',
		repeatable: true,
		observations: [
			uploadObservation(84000, 19000, 25),
			uploadObservation(83200, 18800, 27),
		],
		},
	};
	/* Download shaping is disabled by this topology, so an absent or
	 * inconclusive download search cannot invalidate the tested upload CAKE
	 * configuration. */
	delete result.profile_search.download;
	return result;
}

function write(name, mutate) {
	const result = makeUploadOnly();
	if (mutate)
		mutate(result);
	fs.writeFileSync(path.join(root, `upload-only-${name}.json`), JSON.stringify(result));
}

write('valid');
write('nonrepeatable-review', result => {
	const comparison = result.directional_comparisons.upload_only;
	comparison.recommended_topology = 'manual_review';
	comparison.reason = 'upload-only-benefit-not-repeatable';
	comparison.repeatable = false;
	comparison.observations[0].effective_delta_ms = 10;
	comparison.observations[0].delay_improvement_ms = 51;
	comparison.observations[0].grade = 'A';
	comparison.observations[1].effective_delta_ms = 25;
	comparison.observations[1].delay_improvement_ms = 36;
	comparison.observations[1].grade = 'A';
	result.proposals[0].effective_delta_ms = 25;
	result.proposals[0].grade = 'A';
	result.proposals[0].confidence_percent = 40;
	result.proposals[0].unmet_objectives = [ 'measurement-confidence' ];
});
write('nonrepeatable-missing-ack', result => {
	const comparison = result.directional_comparisons.upload_only;
	comparison.recommended_topology = 'manual_review';
	comparison.reason = 'upload-only-benefit-not-repeatable';
	comparison.repeatable = false;
	comparison.observations[0].effective_delta_ms = 10;
	comparison.observations[0].delay_improvement_ms = 51;
	comparison.observations[0].grade = 'A';
	comparison.observations[1].effective_delta_ms = 25;
	comparison.observations[1].delay_improvement_ms = 36;
	comparison.observations[1].grade = 'A';
	result.proposals[0].effective_delta_ms = 25;
	result.proposals[0].grade = 'A';
	result.proposals[0].confidence_percent = 40;
});
write('realization', result => {
	result.directional_comparisons.upload_only.observations[0].upload_realization_percent = 99;
});
write('gain', result => {
	result.directional_comparisons.upload_only.observations[0].download_gain_percent = 7;
});
write('delay', result => {
	result.directional_comparisons.upload_only.observations[0].delay_improvement_ms = 35;
});
write('repeatability', result => {
	const observation = result.directional_comparisons.upload_only.observations[1];
	observation.observation.throughput_kbps.upload_kbps = 15000;
	observation.upload_realization_percent = 75;
});
write('missing-active-search', result => {
	delete result.profile_search.upload;
});
write('realization-review', result => {
	const observations = result.directional_comparisons.upload_only.observations;
	for (let i = 0; i < observations.length; i++) {
		const upload = i === 0 ? 15000 : 14800;
		observations[i].observation.throughput_kbps.upload_kbps = upload;
		observations[i].upload_realization_percent = upload * 100 / 20000;
		observations[i].pass = false;
	}
	result.proposals[0].unmet_objectives = [ 'candidate-realization' ];
});
write('realization-review-missing-ack', result => {
	const observations = result.directional_comparisons.upload_only.observations;
	for (let i = 0; i < observations.length; i++) {
		const upload = i === 0 ? 15000 : 14800;
		observations[i].observation.throughput_kbps.upload_kbps = upload;
		observations[i].upload_realization_percent = upload * 100 / 20000;
		observations[i].pass = false;
	}
});
write('realization-review-duplicate-ack', result => {
	const observations = result.directional_comparisons.upload_only.observations;
	for (let i = 0; i < observations.length; i++) {
		const upload = i === 0 ? 15000 : 14800;
		observations[i].observation.throughput_kbps.upload_kbps = upload;
		observations[i].upload_realization_percent = upload * 100 / 20000;
		observations[i].pass = false;
	}
	result.proposals[0].unmet_objectives = [ 'candidate-realization', 'candidate-realization' ];
});
write('utility-review', result => {
	const observations = result.directional_comparisons.upload_only.observations;
	result.bidirectional_confirmation.effective_delta_ms = 10;
	result.bidirectional_confirmation.icmp_delta_ms = 10;
	result.bidirectional_confirmation.transport_delta_ms = 10;
	result.bidirectional_confirmation.safety_pass = true;
	result.bidirectional_confirmation.auto_apply_pass = false;
	result.bidirectional_confirmation.achieved_kbps.download = 60000;
	result.bidirectional_confirmation.realization_percent.download = 75;
	result.profile_outcome.bidirectional_confirmation = {
		tested: true, safety_pass: true, auto_apply_pass: false,
		effective_delta_ms: 10, loss_percent: 0,
	};
	result.profile_outcome.mode = 'target-a-met';
	result.profile_outcome.target_met = true;
	result.profile_outcome.actual_grade = 'A';
	for (let i = 0; i < observations.length; i++) {
		const download = i === 0 ? 61200 : 60600;
		const delta = i === 0 ? 25 : 27;
		observations[i].candidate_pass = false;
		observations[i].material_benefit = false;
		observations[i].pass = false;
		observations[i].effective_delta_ms = delta;
		observations[i].delay_improvement_ms = 10 - delta;
		observations[i].grade = 'A';
		observations[i].observation.throughput_kbps.download_kbps = download;
		observations[i].download_gain_percent = (download - 60000) * 100 / 60000;
	}
	result.directional_comparisons.upload_only.recommended_topology = 'manual_review';
	result.proposals[0].unmet_objectives = [
		'throughput-benefit-unproven', 'latency-worse-than-shaped'
	];
});
write('utility-review-missing-benefit-ack', result => {
	const observations = result.directional_comparisons.upload_only.observations;
	for (const observation of observations) {
		observation.candidate_pass = false;
		observation.material_benefit = false;
		observation.pass = false;
		observation.download_gain_percent = 0;
		observation.observation.throughput_kbps.download_kbps = 80000;
	}
	result.directional_comparisons.upload_only.recommended_topology = 'manual_review';
});
write('unexpected-utility-ack', result => {
	result.proposals[0].unmet_objectives = [ 'throughput-benefit-unproven' ];
});
EOF

cp "$work/upload-only-valid.json" "$autotune/wan_sqm/result.json"
upload_only_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint")"
upload_only_token="$(printf '%s\n' "$upload_only_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
[ "${#upload_only_token}" -eq 64 ]
$helper abort "$upload_only_token" >/dev/null

for accepted_upload_only in realization-review utility-review nonrepeatable-review; do
	cp "$work/upload-only-$accepted_upload_only.json" "$autotune/wan_sqm/result.json"
	upload_only_review_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint")"
	upload_only_review_token="$(printf '%s\n' "$upload_only_review_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
	[ "${#upload_only_review_token}" -eq 64 ]
	$helper abort "$upload_only_review_token" >/dev/null
done

for rejected_upload_only in realization gain delay repeatability missing-active-search \
	realization-review-missing-ack realization-review-duplicate-ack \
	utility-review-missing-benefit-ack unexpected-utility-ack nonrepeatable-missing-ack; do
	cp "$work/upload-only-$rejected_upload_only.json" "$autotune/wan_sqm/result.json"
	if $helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint" >/dev/null 2>&1; then
		echo "apply guard accepted tampered upload-only evidence: $rejected_upload_only" >&2
		exit 1
	fi
	if find "$guard" -mindepth 1 -print -quit 2>/dev/null | grep -q .; then
		echo "rejected upload-only evidence leaked an apply token: $rejected_upload_only" >&2
		exit 1
	fi
done
cp "$work/result.valid" "$autotune/wan_sqm/result.json"

# A global both-shaped safety-floor failure does not erase a separately tested
# one-sided topology.  Only its still-shaped direction needs a valid search;
# the exact selected proposal remains manual-only and independently guarded.
node - "$work/upload-only-valid.json" "$autotune/wan_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
result.validation.pass = false;
result.validation.hard_pass = false;
result.validation.safety_pass = false;
result.validation.profile_objectives_met = false;
result.profile_outcome.mode = 'safety-floor-infeasible';
result.profile_outcome.manual_only = true;
result.profile_outcome.capacity_floor_met = false;
result.profile_outcome.throughput_safety_floor_met = false;
result.profile_outcome.infeasible_reason =
	'download:repeatable-shaper-ceiling-below-safety-floor';
fs.writeFileSync(process.argv[3], JSON.stringify(result));
EOF
directional_floor_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint")"
directional_floor_token="$(printf '%s\n' "$directional_floor_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
[ "${#directional_floor_token}" -eq 64 ]
$helper abort "$directional_floor_token" >/dev/null
cp "$work/result.valid" "$autotune/wan_sqm/result.json"

# A flat, target-meeting Variable-link direction is a typed manual fallback,
# not an invented knee.  It may arm only when its runtime minimum is the exact
# tested selected point and the final simultaneous pair is safe.
node - "$work/result.valid" "$autotune/wan_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
result.profile = 'variable_link';
result.auto_apply_eligible = false;
result.validation.profile = 'variable_link';
result.validation.actual_grade = 'B';
result.validation_thresholds.capacity_retention_min_percent = 70;
result.validation_thresholds.delay_max_ms = 60;
result.validation_thresholds.manual_latency_review_max_ms = 200;
result.validation_thresholds.loss_max_percent = 3;
result.proposal.profile = 'variable_link';
result.proposal.target_grade = 'B';
result.proposal.validation.capacity_retention_min_percent = 70;
result.proposal.validation.icmp_delta_max_ms = 60;
result.proposal.validation.transport_delta_max_ms = 60;
result.profile_outcome.mode = 'directional-no-cake-effect-review';
result.profile_outcome.target_grade = 'B';
result.profile_outcome.actual_grade = 'B';
result.profile_outcome.capacity_floor_percent = 70;
result.profile_outcome.manual_only = true;
result.profile_outcome.bidirectional_confirmation =
	structuredClone(result.bidirectional_confirmation);
for (const direction of [ 'download', 'upload' ]) {
	const search = result.profile_search[direction];
	const rate = search.selected.candidate_kbps;
	search.profile = 'variable_link';
	search.action = direction === 'upload' ? 'fallback' : 'complete';
	search.reason = direction === 'upload' ?
		'queue-outside-cake-control' : 'latency-knee-confirmed';
	search.selected.retention_percent = 70;
	search.selected.target_met = true;
	search.exploration_minimum_kbps = Math.floor(rate * 0.35);
	search.runtime_minimum_kbps = rate;
	search.runtime_minimum_observation_index = 1;
	search.knee_detected = direction === 'download';
	search.no_cake_effect = direction === 'upload';
	search.noisy = false;
	search.inconclusive = false;
	search.evaluated = [ { candidate_kbps: rate } ];
	result.proposal[direction].minimum_kbps = rate;
	result.proposal[direction].exploration_cap_kbps =
		result.proposal[direction].observed_high_kbps;
	result.proposal[direction].absolute_cap_kbps =
		result.proposal[direction].observed_high_kbps;
}
result.proposals[0].configuration = structuredClone(result.proposal);
fs.writeFileSync(process.argv[3], JSON.stringify(result));
EOF
variable_no_effect_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint")"
variable_no_effect_token="$(printf '%s\n' "$variable_no_effect_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
[ "${#variable_no_effect_token}" -eq 64 ]
$helper abort "$variable_no_effect_token" >/dev/null

node - "$autotune/wan_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const path = process.argv[2];
const result = JSON.parse(fs.readFileSync(path, 'utf8'));
result.profile_search.upload.runtime_minimum_kbps--;
result.proposal.upload.minimum_kbps--;
fs.writeFileSync(path, JSON.stringify(result));
EOF
if $helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint" >/dev/null 2>&1; then
	echo "apply guard accepted an invented Variable-link no-effect minimum" >&2
	exit 1
fi
cp "$work/result.valid" "$autotune/wan_sqm/result.json"

# Reaching the Variable-link exploration floor without a measured knee may
# expose only the highest exact-tested, target-meeting point above 50%, and it
# must remain an explicit manual review with a safe simultaneous confirmation.
node - "$work/result.valid" "$autotune/wan_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
result.profile = 'variable_link';
result.auto_apply_eligible = false;
result.validation.profile = 'variable_link';
result.validation.actual_grade = 'B';
result.validation_thresholds.capacity_retention_min_percent = 70;
result.validation_thresholds.delay_max_ms = 60;
result.validation_thresholds.manual_latency_review_max_ms = 200;
result.validation_thresholds.loss_max_percent = 3;
result.proposal.profile = 'variable_link';
result.proposal.target_grade = 'B';
result.proposal.validation.capacity_retention_min_percent = 70;
result.proposal.validation.icmp_delta_max_ms = 60;
result.proposal.validation.transport_delta_max_ms = 60;
result.profile_outcome.mode = 'variable-link-bounded-evidence-review';
result.profile_outcome.target_grade = 'B';
result.profile_outcome.actual_grade = 'B';
result.profile_outcome.capacity_floor_percent = 70;
result.profile_outcome.manual_only = true;
result.profile_outcome.bidirectional_confirmation =
	structuredClone(result.bidirectional_confirmation);
for (const direction of [ 'download', 'upload' ]) {
	const search = result.profile_search[direction];
	const rate = search.selected.candidate_kbps;
	search.profile = 'variable_link';
	search.action = direction === 'upload' ? 'fallback' : 'complete';
	search.reason = direction === 'upload' ?
		'exploration-floor-reached' : 'latency-knee-confirmed';
	search.selected.retention_percent = 70;
	search.selected.target_met = true;
	search.exploration_minimum_kbps = Math.floor(rate * 0.35);
	search.runtime_minimum_kbps = rate;
	search.runtime_minimum_observation_index = 1;
	search.knee_detected = direction === 'download';
	search.no_cake_effect = false;
	search.noisy = false;
	search.inconclusive = false;
	search.evaluated = [ { candidate_kbps: rate } ];
	result.proposal[direction].minimum_kbps = rate;
	result.proposal[direction].exploration_cap_kbps =
		result.proposal[direction].observed_high_kbps;
	result.proposal[direction].absolute_cap_kbps =
		result.proposal[direction].observed_high_kbps;
}
result.proposals[0].configuration = structuredClone(result.proposal);
fs.writeFileSync(process.argv[3], JSON.stringify(result));
EOF
variable_floor_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint")"
variable_floor_token="$(printf '%s\n' "$variable_floor_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
[ "${#variable_floor_token}" -eq 64 ]
$helper abort "$variable_floor_token" >/dev/null

node - "$autotune/wan_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const path = process.argv[2];
const result = JSON.parse(fs.readFileSync(path, 'utf8'));
result.profile_search.upload.runtime_minimum_kbps--;
result.proposal.upload.minimum_kbps--;
fs.writeFileSync(path, JSON.stringify(result));
EOF
if $helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint" >/dev/null 2>&1; then
	echo "apply guard accepted an invented Variable-link exploration-floor minimum" >&2
	exit 1
fi
cp "$work/result.valid" "$autotune/wan_sqm/result.json"

# A Variable-link direction that repeatedly realizes only 50..80% may expose
# one exact-tested hold point for explicit review after lower-rate exploration.
# It is never strict-safe or Auto-Apply eligible, and sub-50 evidence remains
# non-overridable.
node - "$work/result.valid" "$autotune/wan_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
result.profile = 'variable_link';
result.auto_apply_eligible = false;
result.validation.profile = 'variable_link';
result.validation.actual_grade = 'B';
result.validation_thresholds.capacity_retention_min_percent = 70;
result.validation_thresholds.delay_max_ms = 60;
result.validation_thresholds.manual_latency_review_max_ms = 200;
result.validation_thresholds.loss_max_percent = 3;
result.proposal.profile = 'variable_link';
result.proposal.target_grade = 'B';
result.proposal.validation.capacity_retention_min_percent = 70;
result.proposal.validation.icmp_delta_max_ms = 60;
result.proposal.validation.transport_delta_max_ms = 60;
result.profile_outcome.mode = 'variable-link-bounded-evidence-review';
result.profile_outcome.target_grade = 'B';
result.profile_outcome.actual_grade = 'B';
result.profile_outcome.capacity_floor_percent = 70;
result.profile_outcome.manual_only = true;
result.profile_outcome.bidirectional_confirmation =
	structuredClone(result.bidirectional_confirmation);
for (const direction of [ 'download', 'upload' ]) {
	const search = result.profile_search[direction];
	const rate = search.selected.candidate_kbps;
	search.profile = 'variable_link';
	search.action = direction === 'upload' ? 'fallback' : 'complete';
	search.reason = direction === 'upload' ?
		'bounded-low-realization-review' : 'latency-knee-confirmed';
	search.selected.retention_percent = 70;
	search.selected.target_met = true;
	search.selected.manual_reviewable = direction === 'upload';
	search.selected.realization_percent = direction === 'upload' ? 75 : 95;
	search.selected.safety_pass = direction !== 'upload';
	search.exploration_minimum_kbps = Math.floor(rate * 0.35);
	search.runtime_minimum_kbps = rate;
	search.runtime_minimum_observation_index = 1;
	search.knee_detected = direction === 'download';
	search.no_cake_effect = false;
	search.noisy = false;
	search.inconclusive = false;
	search.evaluated = [ { candidate_kbps: rate } ];
	result.proposal[direction].minimum_kbps = rate;
	result.proposal[direction].exploration_cap_kbps =
		result.proposal[direction].observed_high_kbps;
	result.proposal[direction].absolute_cap_kbps =
		result.proposal[direction].observed_high_kbps;
}
result.proposals[0].configuration = structuredClone(result.proposal);
fs.writeFileSync(process.argv[3], JSON.stringify(result));
EOF
variable_bounded_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint")"
variable_bounded_token="$(printf '%s\n' "$variable_bounded_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
[ "${#variable_bounded_token}" -eq 64 ]
$helper abort "$variable_bounded_token" >/dev/null

node - "$autotune/wan_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const path = process.argv[2];
const result = JSON.parse(fs.readFileSync(path, 'utf8'));
Object.assign(result.profile_search.upload.selected, {
	manual_reviewable: false,
	realization_percent: 49,
	retention_percent: 49,
});
fs.writeFileSync(path, JSON.stringify(result));
EOF
if $helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint" >/dev/null 2>&1; then
	echo "apply guard accepted a Variable-link fallback below 50 percent" >&2
	exit 1
fi
cp "$work/result.valid" "$autotune/wan_sqm/result.json"

# Gaming must arm an exact diffserv4 manifest rather than merely relabeling a
# best-effort proposal. The token is aborted before the main lifecycle test.
node - "$work/result.valid" "$autotune/wan_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
result.profile = 'gaming';
result.validation.profile = 'gaming';
result.validation.actual_grade = 'A+';
result.validation_thresholds.capacity_retention_min_percent = 70;
result.validation_thresholds.delay_max_ms = 5;
result.validation_thresholds.manual_latency_review_max_ms = 30;
result.validation_thresholds.loss_max_percent = 1;
Object.assign(result.bidirectional_confirmation, {
	grade: 'A+', effective_delta_ms: 4, icmp_delta_ms: 4,
	transport_delta_ms: 4,
});
result.profile_outcome.bidirectional_confirmation =
	structuredClone(result.bidirectional_confirmation);
result.proposal.profile = 'gaming';
result.proposal.target_grade = 'A+';
result.proposal.quality_target_required = true;
result.proposal.throughput_priority = false;
result.proposal.validation.capacity_retention_min_percent = 70;
result.proposal.validation.icmp_delta_max_ms = 5;
result.proposal.validation.transport_delta_max_ms = 5;
result.proposal.validation.loss_max_percent = 1;
result.proposal.sqm = {
	qdisc: 'cake',
	script: 'layer_cake.qos',
	classification: 'diffserv4',
	squash_dscp: false,
	squash_ingress: false,
	ingress_ecn: 'ECN',
	egress_ecn: 'NOECN',
	iqdisc_opts: 'diffserv4',
	eqdisc_opts: 'diffserv4'
};
result.profile_outcome = {
	...result.profile_outcome,
	mode: 'target-a-plus-met', target_grade: 'A+', actual_grade: 'A+',
	capacity_floor_percent: 70
};
for (const direction of [ 'download', 'upload' ]) {
	result.profile_search[direction].profile = 'gaming';
}
result.proposals[0].configuration = structuredClone(result.proposal);
let serialized = JSON.stringify(result);
/* Match serde_json's representation of integral f64 policy values. LuCI
 * stages these through JavaScript String(), which intentionally normalizes
 * them back to integer-looking UCI strings. */
serialized = serialized
	.replaceAll('"capacity_retention_min_percent":70', '"capacity_retention_min_percent":70.0')
	.replace('"delay_max_ms":5', '"delay_max_ms":5.0')
	.replace('"icmp_delta_max_ms":5', '"icmp_delta_max_ms":5.0')
	.replace('"transport_delta_max_ms":5', '"transport_delta_max_ms":5.0');
fs.writeFileSync(process.argv[3], serialized);
EOF
gaming_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint")"
gaming_token="$(printf '%s\n' "$gaming_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
[ "$(uci -c "$guard/$gaming_token/expected" -q get cake-autorate.wan_sqm.autotune_profile)" = gaming ]
[ "$(uci -c "$guard/$gaming_token/expected" -q get cake-autorate.wan_sqm.quality_target_delay_ms)" = 5 ]
[ "$(uci -c "$guard/$gaming_token/expected" -q get cake-autorate.wan_sqm.throughput_guard_retention_percent)" = 70 ]
[ "$(uci -c "$guard/$gaming_token/expected" -q get cake-autorate.wan_sqm.sqm_script)" = layer_cake.qos ]
[ "$(uci -c "$guard/$gaming_token/expected" -q get cake-autorate.wan_sqm.sqm_squash_dscp)" = 0 ]
[ "$(uci -c "$guard/$gaming_token/expected" -q get cake-autorate.wan_sqm.sqm_iqdisc_opts)" = diffserv4 ]
$helper abort "$gaming_token" >/dev/null

# Extreme A+ is a one-run calibration mode. Its result must attest a tested
# runtime minimum, while the persistent runtime profile remains ordinary
# Gaming so scheduled calibration cannot silently repeat the deep search.
node - "$autotune/wan_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const path = process.argv[2];
const result = JSON.parse(fs.readFileSync(path, 'utf8'));
result.profile = 'gaming_extreme';
result.validation.profile = 'gaming_extreme';
result.proposal.profile = 'gaming_extreme';
result.profile_outcome.mode = 'extreme-a-plus-met';
result.profile_outcome.deep_runtime_minimum = false;
for (const direction of [ 'download', 'upload' ]) {
	const search = result.profile_search[direction];
	const selected = search.selected.candidate_kbps;
	search.profile = 'gaming_extreme';
	search.exploration_minimum_kbps = result.proposal[direction].minimum_kbps;
	search.runtime_minimum_kbps = selected;
	search.runtime_minimum_observation_index = 1;
	search.inconclusive = false;
	search.evaluated = [ { candidate_kbps: selected } ];
	result.proposal[direction].minimum_kbps = selected;
}
result.profile_outcome.runtime_minimum_retention = {
	download_percent: Math.round(result.proposal.download.minimum_kbps * 1000 /
		result.proposal.download.observed_low_kbps) / 10,
	upload_percent: Math.round(result.proposal.upload.minimum_kbps * 1000 /
		result.proposal.upload.observed_low_kbps) / 10,
};
result.proposals[0].configuration = structuredClone(result.proposal);
fs.writeFileSync(path, JSON.stringify(result));
EOF
extreme_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint")"
extreme_token="$(printf '%s\n' "$extreme_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
[ "$(uci -c "$guard/$extreme_token/expected" -q get cake-autorate.wan_sqm.autotune_profile)" = gaming ]
[ "$(uci -c "$guard/$extreme_token/expected" -q get cake-autorate.wan_sqm.min_dl_shaper_rate_kbps)" = \
  "$(uci -c "$guard/$extreme_token/expected" -q get cake-autorate.wan_sqm.base_dl_shaper_rate_kbps)" ]
$helper abort "$extreme_token" >/dev/null

# A tested A+ runtime minimum below 70% must remain reviewable but may never
# become an unattended apply, even when the selected upper candidate itself
# passes every profile objective.
node - "$autotune/wan_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const path = process.argv[2];
const result = JSON.parse(fs.readFileSync(path, 'utf8'));
result.auto_apply_eligible = false;
result.profile_outcome.mode = 'extreme-a-plus-throughput-sacrifice';
result.profile_outcome.manual_only = true;
result.profile_outcome.deep_runtime_minimum = true;
const retention = {};
for (const direction of [ 'download', 'upload' ]) {
	const proposal = result.proposal[direction];
	const runtimeMinimum = Math.round(proposal.observed_low_kbps * 0.6);
	const search = result.profile_search[direction];
	search.exploration_minimum_kbps = Math.round(proposal.observed_low_kbps * 0.25);
	search.runtime_minimum_kbps = runtimeMinimum;
	search.runtime_minimum_observation_index = 1;
	search.evaluated = [ { candidate_kbps: runtimeMinimum } ];
	proposal.minimum_kbps = runtimeMinimum;
	retention[direction + '_percent'] =
		Math.round(runtimeMinimum * 1000 / proposal.observed_low_kbps) / 10;
}
result.profile_outcome.runtime_minimum_retention = retention;
result.proposals[0].configuration = structuredClone(result.proposal);
fs.writeFileSync(path, JSON.stringify(result));
EOF
extreme_deep_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint")"
extreme_deep_token="$(printf '%s\n' "$extreme_deep_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
[ "${#extreme_deep_token}" -eq 64 ]
[ "$(uci -c "$guard/$extreme_deep_token/expected" -q get cake-autorate.wan_sqm.autotune_profile)" = gaming ]
[ "$(uci -c "$guard/$extreme_deep_token/expected" -q get cake-autorate.wan_sqm.min_dl_shaper_rate_kbps)" = 51000 ]
[ "$(uci -c "$guard/$extreme_deep_token/expected" -q get cake-autorate.wan_sqm.min_ul_shaper_rate_kbps)" = 12600 ]
$helper abort "$extreme_deep_token" >/dev/null
cp "$work/result.valid" "$autotune/wan_sqm/result.json"

# Falling below 50% of measured historical capacity is an explicit-review
# advisory, not proof that the currently measured CAKE candidate is unsafe.
# The guard may arm that exact candidate, but it must remain manual-only.
node - "$work/result.valid" "$autotune/wan_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
result.auto_apply_eligible = false;
result.validation.pass = false;
result.validation.profile_objectives_met = false;
result.profile_outcome.mode = 'target-a-throughput-advisory';
result.profile_outcome.capacity_floor_met = false;
result.profile_outcome.throughput_safety_floor_met = false;
result.profile_outcome.manual_only = true;
fs.writeFileSync(process.argv[3], JSON.stringify(result));
EOF
historical_advisory_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint")"
historical_advisory_token="$(printf '%s\n' "$historical_advisory_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
[ "${#historical_advisory_token}" -eq 64 ]
$helper abort "$historical_advisory_token" >/dev/null
cp "$work/result.valid" "$autotune/wan_sqm/result.json"

# A logical netifd target remains the attested route identity, while every
# runtime/SQM field in the exact manifest must use its resolved L3 device.
sed 's/"target_interface":"pppoe-wan"/"target_interface":"wan"/' \
	"$work/result.valid" > "$autotune/wan_sqm/result.json"
logical_arm="$($helper arm wan_sqm wan speedtest-go main '' 1 0 apply_sqm "$fingerprint")"
logical_token="$(printf '%s\n' "$logical_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
[ "$(uci -c "$guard/$logical_token/expected" -q get cake-autorate.wan_sqm.wan_if)" = pppoe-wan ]
[ "$(uci -c "$guard/$logical_token/expected" -q get cake-autorate.wan_sqm.sqm_interface)" = pppoe-wan ]
[ "$(uci -c "$guard/$logical_token/expected" -q get cake-autorate.wan_sqm.ul_if)" = pppoe-wan ]
[ "$(uci -c "$guard/$logical_token/expected" -q get cake-autorate.wan_sqm.dl_if)" = ifb4pppoe-wan ]
[ "$(uci -c "$guard/$logical_token/expected" -q get cake-autorate.wan_sqm.ping_extra_args)" = '-I pppoe-wan' ]
$helper abort "$logical_token" >/dev/null
cp "$work/result.valid" "$autotune/wan_sqm/result.json"

# A freshly created wizard instance has an explicit but inactive native traffic
# policy. It is part of the exact manifest; omitting it makes the guard reject
# the otherwise valid first Save & Apply transaction.
mkdir -p "$autotune/new_sqm"
node - "$work/result.valid" "$autotune/new_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
result.job_id = 'new_sqm';
fs.writeFileSync(process.argv[3], JSON.stringify(result));
EOF
chmod 600 "$autotune/new_sqm/result.json"
if ! new_arm="$($helper arm new_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint" 2>&1)"; then
	printf '%s\n' "$new_arm" >&2
	exit 1
fi
new_token="$(printf '%s\n' "$new_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
[ "$(uci -c "$guard/$new_token/expected" -q get cake-autorate.new_sqm.traffic_profile)" = auto ]
[ "$(uci -c "$guard/$new_token/expected" -q get cake-autorate.new_sqm.traffic_profile_migrated)" = 1 ]
[ "$(uci -c "$guard/$new_token/expected" -q get cake-autorate.new_sqm.traffic_rules_enabled)" = 0 ]
$helper abort "$new_token" >/dev/null

# The deterministic enrollment name is reserved. Never reuse or later delete
# a preexisting user section with the same name, regardless of its type.
cp "$config/sqm" "$work/sqm.before-collision"
uci -q set sqm.cake_autorate_apply_wan_sqm=queue
uci -q set sqm.cake_autorate_apply_wan_sqm.enabled=1
uci -q set sqm.cake_autorate_apply_wan_sqm.interface=user-owned
cp "$config/sqm" "$work/sqm.with-collision"
if $helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint" >/dev/null 2>&1; then
	echo "apply guard reused a preexisting SQM enrollment section" >&2
	exit 1
fi
cmp -s "$work/sqm.with-collision" "$config/sqm" || {
	echo "rejected SQM enrollment collision changed user configuration bytes" >&2
	exit 1
}
cp "$work/sqm.before-collision" "$config/sqm"

arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint")"
printf '%s\n' "$arm" | grep -q '"state":"armed"'
token="$(printf '%s\n' "$arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
[ "${#token}" -eq 64 ]
[ "$(uci -c "$guard/$token/expected" -q get cake-autorate.wan_sqm.sqm_script)" = layer_cake.qos ]
[ "$(uci -c "$guard/$token/expected" -q get cake-autorate.wan_sqm.sqm_qdisc_advanced)" = 1 ]
[ "$(uci -c "$guard/$token/expected" -q get cake-autorate.wan_sqm.sqm_iqdisc_opts)" = besteffort ]
[ "$(uci -c "$guard/$token/expected" -q get cake-autorate.wan_sqm.sqm_eqdisc_opts)" = diffserv4 ]

uci -q set sqm.cake_autorate_apply_wan_sqm=cake_autorate_apply_guard
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_guard=1
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_job=wan_sqm
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_fingerprint="$fingerprint"
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_token="$token"

# Simulate rpcd applying exactly the staged CAKE section. SQM remains at its
# arm-time value until the guarded init hook is allowed to run.
cp "$guard/$token/expected/cake-autorate" "$config/cake-autorate"
verified="$($helper verify-init)" || { printf '%s\n' "$verified" >&2; exit 1; }
printf '%s\n' "$verified" | grep -q '"state":"verified"'

# Any stale or additional protected-section edit is rejected before init.
uci -q set cake-autorate.wan_sqm.max_dl_shaper_rate_kbps=89999
if mismatch="$($helper verify-init 2>/dev/null)"; then
	echo "apply guard accepted a stale CAKE manifest" >&2
	exit 1
fi
printf '%s\n' "$mismatch" | grep -q 'cake-autorate.wan_sqm.max_dl_shaper_rate_kbps' || {
	echo "apply guard did not identify the mismatched UCI option without diff" >&2
	exit 1
}
cp "$guard/$token/expected/cake-autorate" "$config/cake-autorate"

# An extra key shifts sorted UCI output. Diagnostics must name that actual key,
# not the innocent option which happens to occupy the same line number.
uci -q set cake-autorate.wan_sqm.traffic_profile=auto
if mismatch="$($helper verify-init 2>/dev/null)"; then
	echo "apply guard accepted an extra CAKE manifest option" >&2
	exit 1
fi
printf '%s\n' "$mismatch" | grep -q 'cake-autorate.wan_sqm.traffic_profile' || {
	echo "apply guard misidentified the extra UCI option" >&2
	exit 1
}
cp "$guard/$token/expected/cake-autorate" "$config/cake-autorate"

# Ordered lists are semantic: active/spare reflector order must not be hidden
# by canonical option sorting.
uci -q delete cake-autorate.wan_sqm.reflector
uci -q add_list cake-autorate.wan_sqm.reflector=8.8.8.8
uci -q add_list cake-autorate.wan_sqm.reflector=9.9.9.9
uci -q add_list cake-autorate.wan_sqm.reflector=1.1.1.1
if $helper verify-init >/dev/null 2>&1; then
	echo "apply guard accepted a reordered reflector list" >&2
	exit 1
fi
cp "$guard/$token/expected/cake-autorate" "$config/cake-autorate"

uci -q set cake-autorate.unrelated=cake_autorate
uci -q set cake-autorate.unrelated.enabled=0
if $helper verify-init >/dev/null 2>&1; then
	echo "apply guard accepted an unrelated CAKE package edit" >&2
	exit 1
fi
cp "$guard/$token/expected/cake-autorate" "$config/cake-autorate"
uci -q set sqm.unrelated=queue
uci -q set sqm.unrelated.enabled=0
if $helper verify-init >/dev/null 2>&1; then
	echo "apply guard accepted an unrelated SQM package edit" >&2
	exit 1
fi
cp "$guard/$token/expected-pre/sqm" "$config/sqm"

# Route/source/external identity remains part of every pre/post attestation.
if APPLY_GUARD_EXTERNAL_IP=198.51.100.44 $helper verify-init >/dev/null 2>&1; then
	echo "apply guard accepted a changed external address" >&2
	exit 1
fi
$helper verify-init >/dev/null

# Tokens are time-bounded even when every marker field still matches.
expires="$(sed -n 's/^expires_epoch=//p' "$guard/$token/meta")"
sed -i 's/^expires_epoch=.*/expires_epoch=0/' "$guard/$token/meta"
if $helper verify-init >/dev/null 2>&1; then
	echo "expired apply token was accepted" >&2
	exit 1
fi
sed -i "s/^expires_epoch=.*/expires_epoch=$expires/" "$guard/$token/meta"

# Simulate init materializing the expected managed queue, then require the
# exact queue plus daemon state before the UCI transaction may be confirmed.
cp "$guard/$token/expected/sqm" "$config/sqm"
$helper verify-init >/dev/null
export APPLY_GUARD_DAEMON_RUNNING=1
other_token=ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff
cp -a "$guard/$token" "$guard/$other_token"
if $helper postcheck "$other_token" >/dev/null 2>&1; then
	echo "postcheck accepted a token different from the live marker" >&2
	exit 1
fi
$helper abort "$other_token" >/dev/null

# A live marker owns its root token. Neither abort nor expired-token GC may
# invalidate it and leave persistent UCI state tokenless.
if $helper abort "$token" >/dev/null 2>&1; then
	echo "abort removed a token referenced by live apply markers" >&2
	exit 1
fi
[ -d "$guard/$token" ]
postcheck="$($helper postcheck "$token")" || { printf '%s\n' "$postcheck" >&2; exit 1; }
printf '%s\n' "$postcheck" | grep -q '"state":"verified"'

# Runtime proof requires exactly one fresh daemon and exactly one root CAKE
# qdisc per managed direction.
mkdir -p "$work/proc/201"
printf '%s\n' '201 (cake-autorated) S 1 1 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 525252' > "$work/proc/201/stat"
sed -i 's/^original_daemon_identity=.*/original_daemon_identity=200:424242,/' "$guard/$token/meta"
if APPLY_GUARD_DAEMON_PID="200
201" $helper postcheck "$token" >/dev/null 2>&1; then
	echo "postcheck accepted a stale and fresh daemon simultaneously" >&2
	exit 1
fi
sed -i 's/^original_daemon_identity=.*/original_daemon_identity=none/' "$guard/$token/meta"
if APPLY_GUARD_TC_MODE=child $helper postcheck "$token" >/dev/null 2>&1; then
	echo "postcheck accepted child-only CAKE qdiscs" >&2
	exit 1
fi
if APPLY_GUARD_TC_MODE=duplicate $helper postcheck "$token" >/dev/null 2>&1; then
	echo "postcheck accepted duplicate root CAKE qdiscs" >&2
	exit 1
fi
uci -q set sqm.cake_wan_sqm.upload=19999
if $helper postcheck "$token" >/dev/null 2>&1; then
	echo "apply guard accepted a mismatched managed SQM queue" >&2
	exit 1
fi
cp "$guard/$token/expected/sqm" "$config/sqm"
$helper postcheck "$token" >/dev/null

# RPC rollback must restore both complete packages, including removing the
# SQM enrollment marker. Keep the token until that exact state is proven.
cp "$guard/$token/original/cake-autorate" "$config/cake-autorate"
cp "$guard/$token/original/sqm" "$config/sqm"
export APPLY_GUARD_DAEMON_RUNNING=0
$helper verify-rollback "$token" | grep -q '"state":"rolled-back"'
cp "$guard/$token/expected/cake-autorate" "$config/cake-autorate"
cp "$guard/$token/expected/sqm" "$config/sqm"
export APPLY_GUARD_DAEMON_RUNNING=1
$helper postcheck "$token" >/dev/null

# Once postcheck is complete, prepare-confirm removes both persistent markers
# while rpcd rollback is still armed and records the exact marker-free state.
prepare="$(APPLY_GUARD_NOW_EPOCH=$((expires + 1)) $helper prepare-confirm "$token")"
printf '%s\n' "$prepare" | grep -q '"state":"prepared"'
cmp -s "$guard/$token/expected-final/cake-autorate" "$config/cake-autorate"
cmp -s "$guard/$token/expected-final/sqm" "$config/sqm"
if uci -q show cake-autorate.wan_sqm | grep -q '_autotune_apply_'; then
	echo "prepare-confirm left a persistent CAKE marker" >&2
	exit 1
fi
if uci -q get sqm.cake_autorate_apply_wan_sqm >/dev/null 2>&1; then
	echo "prepare-confirm left an SQM enrollment marker" >&2
	exit 1
fi
$helper reconcile "$token" | grep -q '"state":"confirmed"'
if APPLY_GUARD_RPCD_PENDING=1 $helper finalize "$token" >/dev/null 2>&1; then
	echo "finalize accepted a transaction while rpcd rollback proof was still present" >&2
	exit 1
fi
$helper finalize "$token" | grep -q '"state":"finalized"'
[ ! -e "$guard/$token" ]
if uci -q get cake-autorate.wan_sqm._autotune_apply_guard >/dev/null 2>&1; then
	echo "finalize left a persistent apply marker" >&2
	exit 1
fi
if uci -q get sqm.cake_autorate_apply_wan_sqm >/dev/null 2>&1; then
	echo "finalize left an SQM rollback marker" >&2
	exit 1
fi
[ "$(sed -n '1p' "$APPLY_GUARD_SQM_INIT_STATE")" = enabled ]
$helper verify-init | grep -q '"state":"clear"'

# The init-launched server supervisor owns postcheck and marker cleanup. LuCI
# confirms with the authenticated ubus session that started the rpcd apply;
# after the deadline the independent supervisor proves final or rolled-back
# state and finishes cleanup if the browser disappeared mid-finalization.
cp "$work/result.valid" "$autotune/wan_sqm/result.json"
export APPLY_GUARD_DAEMON_RUNNING=0
supervised_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint")"
supervised_token="$(printf '%s\n' "$supervised_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
cp "$guard/$supervised_token/expected/cake-autorate" "$config/cake-autorate"
cp "$guard/$supervised_token/expected/sqm" "$config/sqm"
$helper verify-init | grep -q '"state":"verified"'
export APPLY_GUARD_DAEMON_RUNNING=1
# Simulate LuCI confirmation by retaining expected-final beyond an already
# elapsed rpcd deadline. The real browser normally finalizes immediately.
supervised_started=$(( $(date +%s) - 40 ))
printf '%s\n' "$supervised_started" > "$guard/$supervised_token/apply-started"
sed -i 's/^rollback_timeout_s=.*/rollback_timeout_s=10/' "$guard/$supervised_token/meta"
supervised_output="$($helper supervise "$supervised_token")" || {
	printf '%s\n' "$supervised_output" >&2
	$helper status "$supervised_token" >&2 || true
	exit 1
}
printf '%s\n' "$supervised_output" | grep -q '"state":"finalized"'
supervised_status="$($helper status "$supervised_token")" || {
	printf '%s\n' "$supervised_status" >&2
	exit 1
}
printf '%s\n' "$supervised_status" | grep -q '"state":"complete"' || {
	printf '%s\n' "$supervised_status" >&2
	exit 1
}
[ ! -e "$guard/$supervised_token" ]
if uci -q show cake-autorate.wan_sqm | grep -q '_autotune_apply_'; then
	echo "server-side confirmation left a persistent CAKE marker" >&2
	exit 1
fi
if uci -q get sqm.cake_autorate_apply_wan_sqm >/dev/null 2>&1; then
	echo "server-side confirmation left a persistent SQM marker" >&2
	exit 1
fi

# A failed server-side postcheck must wait for and prove rpcd's exact rollback,
# discard the token and leave a terminal receipt that LuCI can report after a
# refresh. Backdate apply-started so the fixture does not sleep for the real
# rollback window.
export APPLY_GUARD_DAEMON_RUNNING=0
rollback_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint")"
rollback_token="$(printf '%s\n' "$rollback_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
cp "$guard/$rollback_token/expected/cake-autorate" "$config/cake-autorate"
cp "$guard/$rollback_token/expected/sqm" "$config/sqm"
$helper verify-init | grep -q '"state":"verified"'
printf '0\n' > "$guard/$rollback_token/apply-started"
cp "$guard/$rollback_token/original/cake-autorate" "$config/cake-autorate"
cp "$guard/$rollback_token/original/sqm" "$config/sqm"
if $helper supervise "$rollback_token" >/dev/null 2>&1; then
	echo "server-side supervisor unexpectedly accepted a rolled-back transaction" >&2
	exit 1
fi
rollback_status="$($helper status "$rollback_token")" || {
	printf '%s\n' "$rollback_status" >&2
	exit 1
}
printf '%s\n' "$rollback_status" | grep -q '"state":"rolled-back"' || {
	printf '%s\n' "$rollback_status" >&2
	exit 1
}
[ ! -e "$guard/$rollback_token" ]
if uci -q show cake-autorate.wan_sqm | grep -q '_autotune_apply_'; then
	echo "server-side rollback left a persistent CAKE marker" >&2
	exit 1
fi
if uci -q get sqm.cake_autorate_apply_wan_sqm >/dev/null 2>&1; then
	echo "server-side rollback left a persistent SQM marker" >&2
	exit 1
fi

# A generic no-SQM proposal is independently guarded by its raw control.  An
# unsafe, low-realization shaped comparison and the absence of a 2% raw gain
# must not hide a clean raw topology which is itself inside the hard manual
# latency/loss boundary.
node - "$work/result.valid" "$autotune/wan_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
result.auto_apply_eligible = false;
result.manual_apply_eligible = true;
result.validation.pass = false;
result.validation.hard_pass = false;
result.validation.safety_pass = false;
result.validation.profile_objectives_met = false;
result.validation.quality_target_met = false;
result.validation.actual_grade = 'F';
result.validation.effective_delta_ms = 500;
result.validation.throughput = { download_kbps: 90000, upload_kbps: 25000 };
Object.assign(result.bidirectional_confirmation, {
	safety_pass: false,
	auto_apply_pass: false,
	grade: 'F',
	achieved_kbps: { download: 1000, upload: 500 },
	realization_percent: { download: 1.25, upload: 2.5 },
	effective_delta_ms: 500,
	transport_delta_ms: 500,
});
result.profile_outcome.manual_only = true;
result.profile_outcome.target_met = false;
result.profile_outcome.capacity_floor_met = false;
result.profile_outcome.actual_grade = 'F';
result.profile_outcome.bidirectional_confirmation = structuredClone(result.bidirectional_confirmation);
result.raw_control = {
	available: true,
	measurement_evidence: {
		valid: true,
		reason: 'ok',
		test_direction: 'both',
		shaper_bypassed: true,
		sqm_paused: true,
		sqm_bypass_mode: 'paused-managed',
	},
	grade: 'A',
	effective_delta_ms: 20,
	icmp_latency: { loss_percent: 0 },
	throughput: { download_kbps: 85000, upload_kbps: 22000 },
	forwarded_background: {
		available: true,
		contaminated: false,
		download_kbps: 100,
		upload_kbps: 50,
		download_limit_kbps: 1700,
		upload_limit_kbps: 1000,
	},
};
Object.assign(result.proposals[1], {
	action: 'disable_sqm',
	topology: 'no_sqm',
	applicable: true,
	hard_safety_pass: true,
	profile_target_met: true,
	profile_objectives_met: true,
	grade: 'A',
	effective_delta_ms: 20,
	confidence_percent: 100,
	unmet_objectives: [],
	evidence: { control: 'raw_control' },
	configuration: null,
});
fs.writeFileSync(process.argv[3], JSON.stringify(result));
EOF
cp "$autotune/wan_sqm/result.json" "$work/result.raw-independent"
raw_independent_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 0 0 disable_sqm "$fingerprint")"
raw_independent_token="$(printf '%s\n' "$raw_independent_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
[ "${#raw_independent_token}" -eq 64 ]
$helper abort "$raw_independent_token" >/dev/null

# auto_apply_eligible describes the top-level shaped candidate, not the
# explicitly selected no-SQM action.  A clean exact raw proposal must remain
# reviewable when a different shaped proposal is also Auto-Apply eligible.
node - "$work/result.raw-independent" "$autotune/wan_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
result.auto_apply_eligible = true;
fs.writeFileSync(process.argv[3], JSON.stringify(result));
EOF
raw_with_shaped_auto_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 0 0 disable_sqm "$fingerprint")"
raw_with_shaped_auto_token="$(printf '%s\n' "$raw_with_shaped_auto_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
[ "${#raw_with_shaped_auto_token}" -eq 64 ]
$helper abort "$raw_with_shaped_auto_token" >/dev/null

# Every hard fact in the independent raw proof remains fail-closed.  These
# mutations also prove that no shaped comparison is consulted on the accepted
# path above: only the raw topology and its exact candidate may vary here.
node - "$work/result.raw-independent" "$work" <<'EOF'
const fs = require('node:fs');
const input = process.argv[2];
const dir = process.argv[3];
const base = JSON.parse(fs.readFileSync(input, 'utf8'));
const cases = {
	measurement: r => { r.raw_control.measurement_evidence.valid = false; },
	direction: r => { r.raw_control.measurement_evidence.test_direction = 'download'; },
	bypass: r => { r.raw_control.measurement_evidence.shaper_bypassed = false; },
	background: r => { r.raw_control.forwarded_background.contaminated = true; },
	loss: r => { r.raw_control.icmp_latency.loss_percent = 4; },
	delay: r => { r.raw_control.effective_delta_ms = 61; r.raw_control.grade = 'C'; r.proposals[1].effective_delta_ms = 61; r.proposals[1].grade = 'C'; },
	throughput: r => { r.raw_control.throughput.download_kbps = 0; },
	proposal: r => { r.proposals[1].effective_delta_ms = 21; },
	configuration: r => { r.proposals[1].configuration = structuredClone(r.proposal); },
	route: r => { r.route_identity = 'main||eth9|192.0.2.10||main'; },
};
for (const [name, mutate] of Object.entries(cases)) {
	const result = structuredClone(base);
	mutate(result);
	fs.writeFileSync(`${dir}/raw-reject-${name}.json`, JSON.stringify(result));
}
EOF
for raw_rejection in measurement direction bypass background loss delay throughput proposal configuration route; do
	cp "$work/raw-reject-$raw_rejection.json" "$autotune/wan_sqm/result.json"
	if $helper arm wan_sqm pppoe-wan speedtest-go main '' 0 0 disable_sqm "$fingerprint" >/dev/null 2>&1; then
		echo "apply guard accepted tampered raw no-SQM evidence: $raw_rejection" >&2
		exit 1
	fi
done

# Disabling SQM is meaningful only for an existing, owned instance.  A clean
# raw control must not turn the create wizard into a disabled no-op instance.
mkdir -p "$autotune/raw_new_sqm"
node - "$work/result.raw-independent" "$autotune/raw_new_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
result.job_id = 'raw_new_sqm';
fs.writeFileSync(process.argv[3], JSON.stringify(result));
EOF
chmod 600 "$autotune/raw_new_sqm/result.json"
if $helper arm raw_new_sqm pppoe-wan speedtest-go main '' 0 0 disable_sqm "$fingerprint" >/dev/null 2>&1; then
	echo "apply guard created a new disabled no-SQM instance" >&2
	exit 1
fi
cp "$work/result.valid" "$autotune/wan_sqm/result.json"

# Fair may explicitly recommend disabling SQM only when a complete unshaped
# control is no worse for latency and improves both directions. The guarded
# transaction preserves the owned queue as disabled and proves all runtime
# shaping state has disappeared.
node - "$work/result.valid" "$autotune/wan_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
result.profile = 'fair';
result.auto_apply_eligible = false;
result.manual_apply_eligible = true;
result.validation_thresholds.capacity_retention_min_percent = 90;
result.validation_thresholds.delay_max_ms = 200;
result.validation_thresholds.manual_latency_review_max_ms = 400;
result.validation_thresholds.loss_max_percent = 5;
result.validation = {
	profile: 'fair',
	pass: false,
	hard_pass: true,
	safety_pass: true,
	profile_objectives_met: true,
	quality_target_met: false,
	actual_grade: 'D',
	effective_delta_ms: 220,
	contaminated: false,
	candidate_base: { download_kbps: 80000, upload_kbps: 20000 },
	correction: { action: 'infeasible', feasible: false }
};
result.proposal.profile = 'fair';
result.proposal.target_grade = 'C';
result.proposal.quality_target_required = false;
result.proposal.throughput_priority = true;
result.proposal.validation.capacity_retention_min_percent = 90;
result.proposal.validation.icmp_delta_max_ms = 200;
result.proposal.validation.transport_delta_max_ms = 200;
result.proposal.validation.loss_max_percent = 5;
result.profile_outcome = {
	mode: 'throughput-optimum-quality-fallback',
	objective: 'throughput-first-quality-tiebreak',
	target_grade: 'C', target_met: false, actual_grade: 'D',
	capacity_floor_percent: 90, manual_only: true,
	capacity_floor_met: true, throughput_safety_floor_percent: 50,
	throughput_safety_floor_met: true, deep_runtime_minimum: false,
	runtime_minimum_retention: null, infeasible_reason: '',
	selected_pair: { download_kbps: 80000, upload_kbps: 20000 },
	bidirectional_confirmation: structuredClone(result.bidirectional_confirmation)
};
result.profile_search = {
	download: { schema_version: 2, profile: 'fair', direction: 'download', action: 'complete',
		selected: { candidate_kbps: 80000, safety_pass: true, target_met: false } },
	upload: { schema_version: 2, profile: 'fair', direction: 'upload', action: 'complete',
		selected: { candidate_kbps: 20000, safety_pass: true, target_met: false } }
};
result.fair_outcome = {
	mode: 'sqm-disable-recommended',
	target_grade: 'C',
	target_delta_ms: 200,
	capacity_floor_percent: 90,
	capacity_floor_met: true,
	throughput_safety_floor_percent: 50,
	throughput_safety_floor_met: true,
	actual_grade: 'D',
	actual_effective_delta_ms: 220,
	recommended_action: 'disable_sqm',
	allowed_actions: [ 'apply_sqm', 'keep_current', 'disable_sqm' ],
	apply_sqm_available: true,
	disable_sqm_available: true,
	comparison_reason: 'no-material-latency-benefit-with-throughput-cost',
	no_sqm_control: {
		available: true,
		measurement_evidence: {
			valid: true,
			reason: 'ok',
			test_direction: 'both',
			shaper_bypassed: true,
			sqm_paused: true,
			sqm_bypass_mode: 'paused-managed'
		},
		grade: 'D',
		effective_delta_ms: 218,
		icmp_latency: { loss_percent: 0 },
		throughput: { download_kbps: 85000, upload_kbps: 22000 },
		forwarded_background: {
			available: true,
			contaminated: false,
			duration_s: 20,
			download_kbps: 100,
			upload_kbps: 50,
			download_limit_kbps: 1700,
			upload_limit_kbps: 1000
		}
	},
	throughput_gain_without_sqm: { download_percent: 3, upload_percent: 3 }
};
result.proposals[0].configuration = structuredClone(result.proposal);
fs.writeFileSync(process.argv[3], JSON.stringify(result));
EOF
cp "$autotune/wan_sqm/result.json" "$work/result.fair-disable"

# A schema-8 Fair result must carry the final simultaneous degradation into
# both typed outcome objects.  The older contradictory shape (directional C in
# fair_outcome while the final profile outcome is D) is diagnostic-only.
node - "$work/result.fair-disable" "$work/result.fair-simultaneous" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
result.validation.pass = true;
result.validation.hard_pass = true;
result.validation.safety_pass = true;
result.validation.profile_objectives_met = false;
result.validation.quality_target_met = true;
result.validation.actual_grade = 'C';
result.validation.effective_delta_ms = 191.5;
result.validation.correction = { action: 'none', feasible: true };
Object.assign(result.bidirectional_confirmation, {
	safety_pass: true,
	auto_apply_pass: false,
	grade: 'D',
	effective_delta_ms: 224.2,
	transport_delta_ms: 224.2,
});
Object.assign(result.profile_outcome, {
	mode: 'quality-and-throughput-advisory-review',
	target_met: false,
	actual_grade: 'D',
	capacity_floor_met: false,
	manual_only: true,
	bidirectional_confirmation: structuredClone(result.bidirectional_confirmation),
});
result.fair_outcome = {
	...result.fair_outcome,
	mode: 'throughput-fallback',
	capacity_floor_met: false,
	actual_grade: 'D',
	actual_effective_delta_ms: 224.2,
	recommended_action: 'apply_sqm',
	allowed_actions: [ 'apply_sqm', 'keep_current' ],
	apply_sqm_available: true,
	disable_sqm_available: false,
	comparison_reason: 'quality-target-unreachable-above-throughput-floor',
	no_sqm_control: { available: false, measurement_evidence: { valid: false, reason: 'not-tested' } },
	throughput_gain_without_sqm: { download_percent: 0, upload_percent: 0 },
};
fs.writeFileSync(process.argv[3], JSON.stringify(result));
EOF
cp "$work/result.fair-simultaneous" "$autotune/wan_sqm/result.json"
fair_simultaneous_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint")"
fair_simultaneous_token="$(printf '%s\n' "$fair_simultaneous_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
[ "${#fair_simultaneous_token}" -eq 64 ]
$helper abort "$fair_simultaneous_token" >/dev/null
node - "$work/result.fair-simultaneous" "$autotune/wan_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
result.fair_outcome.actual_grade = 'C';
result.fair_outcome.actual_effective_delta_ms = 191.5;
fs.writeFileSync(process.argv[3], JSON.stringify(result));
EOF
if $helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint" >/dev/null 2>&1; then
	echo "apply guard accepted a Fair outcome that omitted final simultaneous degradation" >&2
	exit 1
fi
cp "$work/result.fair-disable" "$autotune/wan_sqm/result.json"

# The shaped final pair may be worse than Fair's bounded manual-review range
# while an independently clean no-SQM control proves that disabling SQM is the
# safer action.  That must block apply_sqm without hiding disable_sqm.
node - "$work/result.fair-disable" "$autotune/wan_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
result.bidirectional_confirmation.safety_pass = false;
result.bidirectional_confirmation.auto_apply_pass = false;
result.bidirectional_confirmation.grade = 'F';
result.bidirectional_confirmation.effective_delta_ms = 466;
result.bidirectional_confirmation.transport_delta_ms = 466;
result.profile_outcome.actual_grade = 'F';
result.profile_outcome.bidirectional_confirmation = structuredClone(result.bidirectional_confirmation);
result.fair_outcome.actual_grade = 'F';
result.fair_outcome.actual_effective_delta_ms = 466;
fs.writeFileSync(process.argv[3], JSON.stringify(result));
EOF
if $helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint" >/dev/null 2>&1; then
	echo "apply guard accepted an unsafe shaped Fair confirmation" >&2
	exit 1
fi
unsafe_disable_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 0 0 disable_sqm "$fingerprint")"
unsafe_disable_token="$(printf '%s\n' "$unsafe_disable_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
[ "${#unsafe_disable_token}" -eq 64 ]
$helper abort "$unsafe_disable_token" >/dev/null

cp "$work/result.fair-disable" "$autotune/wan_sqm/result.json"
node - "$work/result.fair-disable" "$autotune/wan_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
result.fair_outcome.throughput_gain_without_sqm.upload_percent = 1.9;
fs.writeFileSync(process.argv[3], JSON.stringify(result));
EOF
if $helper arm wan_sqm pppoe-wan speedtest-go main '' 0 0 disable_sqm "$fingerprint" >/dev/null 2>&1; then
	echo "apply guard accepted a no-SQM recommendation below the bidirectional gain threshold" >&2
	exit 1
fi
node - "$work/result.fair-disable" "$autotune/wan_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
result.fair_outcome.no_sqm_control.measurement_evidence.sqm_paused = false;
fs.writeFileSync(process.argv[3], JSON.stringify(result));
EOF
if $helper arm wan_sqm pppoe-wan speedtest-go main '' 0 0 disable_sqm "$fingerprint" >/dev/null 2>&1; then
	echo "apply guard accepted a no-SQM recommendation without SQM-pause proof" >&2
	exit 1
fi
node - "$work/result.fair-disable" "$autotune/wan_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
result.fair_outcome.no_sqm_control.forwarded_background.contaminated = true;
fs.writeFileSync(process.argv[3], JSON.stringify(result));
EOF
if $helper arm wan_sqm pppoe-wan speedtest-go main '' 0 0 disable_sqm "$fingerprint" >/dev/null 2>&1; then
	echo "apply guard accepted a no-SQM recommendation with contaminated background traffic" >&2
	exit 1
fi
node - "$work/result.fair-disable" "$autotune/wan_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
result.fair_outcome.no_sqm_control.icmp_latency.loss_percent = 6;
fs.writeFileSync(process.argv[3], JSON.stringify(result));
EOF
if $helper arm wan_sqm pppoe-wan speedtest-go main '' 0 0 disable_sqm "$fingerprint" >/dev/null 2>&1; then
	echo "apply guard accepted a no-SQM control above the Fair packet-loss limit" >&2
	exit 1
fi

# A proven repeatable compute ceiling may support the same explicit no-SQM
# action even though no shaped candidate can satisfy Fair's immutable 90%
# floor. The unsafe candidate is diagnostic only and can never be applied.
node - "$work/result.fair-disable" "$autotune/wan_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
result.validation.hard_pass = false;
result.validation.safety_pass = false;
result.validation.profile_objectives_met = false;
result.validation.quality_target_met = true;
result.validation.actual_grade = 'A';
result.validation.effective_delta_ms = 10;
result.profile_outcome.mode = 'safety-floor-infeasible';
result.profile_outcome.target_met = true;
result.profile_outcome.actual_grade = 'A';
result.profile_outcome.capacity_floor_met = false;
result.profile_outcome.throughput_safety_floor_met = false;
result.profile_outcome.infeasible_reason = 'download:repeatable-compute-ceiling-below-safety-floor;upload:repeatable-compute-ceiling-below-safety-floor';
for (const direction of [ 'download', 'upload' ]) {
	result.profile_search[direction].action = 'fallback';
	result.profile_search[direction].reason = 'repeatable-shaper-ceiling-below-safety-floor';
	result.profile_search[direction].selected.safety_pass = false;
}
result.fair_outcome.capacity_floor_met = false;
result.fair_outcome.throughput_safety_floor_met = false;
result.fair_outcome.actual_grade = 'A';
result.fair_outcome.actual_effective_delta_ms = 10;
result.fair_outcome.allowed_actions = [ 'keep_current', 'disable_sqm' ];
result.fair_outcome.apply_sqm_available = false;
result.fair_outcome.no_sqm_control.grade = 'A';
result.fair_outcome.no_sqm_control.effective_delta_ms = 9;
fs.writeFileSync(process.argv[3], JSON.stringify(result));
EOF
if $helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint" >/dev/null 2>&1; then
	echo "apply guard accepted an unsafe capacity-floor candidate" >&2
	exit 1
fi
floor_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 0 0 disable_sqm "$fingerprint")"
floor_token="$(printf '%s\n' "$floor_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
[ "${#floor_token}" -eq 64 ]
$helper abort "$floor_token" >/dev/null

cp "$work/result.fair-disable" "$autotune/wan_sqm/result.json"
export APPLY_GUARD_DAEMON_RUNNING=0
disabled_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 0 0 disable_sqm "$fingerprint")"
disabled_token="$(printf '%s\n' "$disabled_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
cp "$guard/$disabled_token/expected/cake-autorate" "$config/cake-autorate"
uci -q set sqm.cake_autorate_apply_wan_sqm=cake_autorate_apply_guard
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_guard=1
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_job=wan_sqm
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_fingerprint="$fingerprint"
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_token="$disabled_token"
$helper verify-init >/dev/null
cp "$guard/$disabled_token/expected/sqm" "$config/sqm"
rmdir "$work/sys/ifb4pppoe-wan"
APPLY_GUARD_TC_ACTIVE=0 $helper postcheck "$disabled_token" >/dev/null
[ "$(uci -q get sqm.cake_wan_sqm.enabled)" = 0 ]
[ "$(uci -q get sqm.cake_wan_sqm._cake_autorate_managed)" = wan_sqm ]
$helper prepare-confirm "$disabled_token" >/dev/null
# Simulate loss of all tmpfs proof after successful prepare-confirm. Persistent
# configuration must contain no token-dependent marker and init must stay clear.
rm -rf "$guard/$disabled_token"
$helper verify-init | grep -q '"state":"clear"'
if uci -q show cake-autorate.wan_sqm | grep -q '_autotune_apply_'; then
	echo "tmpfs loss after prepare-confirm exposed a tokenless CAKE marker" >&2
	exit 1
fi
if uci -q get sqm.cake_autorate_apply_wan_sqm >/dev/null 2>&1; then
	echo "tmpfs loss after prepare-confirm exposed a tokenless SQM marker" >&2
	exit 1
fi

# A same-boot marker with a future expiry and no root-owned token is suspicious:
# fail closed rather than treating an in-flight transaction as abandoned.
stale_token=eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee
stale_fingerprint=sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
stale_expiry=$(( $(date +%s) + 180 ))
uci -q set cake-autorate.wan_sqm._autotune_apply_guard=1
uci -q set cake-autorate.wan_sqm._autotune_apply_fingerprint="$stale_fingerprint"
uci -q set cake-autorate.wan_sqm._autotune_apply_target=pppoe-wan
uci -q set cake-autorate.wan_sqm._autotune_apply_backend=speedtest-go
uci -q set cake-autorate.wan_sqm._autotune_apply_route_mode=main
uci -q set cake-autorate.wan_sqm._autotune_apply_enabled=1
uci -q set cake-autorate.wan_sqm._autotune_apply_disable_adaptive=0
uci -q set cake-autorate.wan_sqm._autotune_apply_action=apply_sqm
uci -q set cake-autorate.wan_sqm._autotune_apply_token="$stale_token"
uci -q set cake-autorate.wan_sqm._autotune_apply_expires="$stale_expiry"
uci -q set cake-autorate.wan_sqm._autotune_apply_boot_id=11111111-2222-3333-4444-555555555555
uci -q set sqm.cake_autorate_apply_wan_sqm=cake_autorate_apply_guard
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_guard=1
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_job=wan_sqm
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_fingerprint="$stale_fingerprint"
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_token="$stale_token"
if $helper recover-stale >/dev/null 2>&1; then
	echo "same-boot unexpired tokenless marker was recovered prematurely" >&2
	exit 1
fi
if $helper verify-init >/dev/null 2>&1; then
	echo "same-boot tokenless apply marker was accepted" >&2
	exit 1
fi

# Once the same-boot transaction has expired, retain its diagnostics but make
# the unproven instance and owned queue inert before removing metadata.
uci -q set cake-autorate.wan_sqm._autotune_apply_expires=0
uci -q set cake-autorate.wan_sqm.enabled=1
uci -q set cake-autorate.wan_sqm.sqm_enabled=1
uci -q set sqm.cake_wan_sqm.enabled=1
before_setting="$(uci -q get cake-autorate.wan_sqm.unrelated_preserved)"
before_queue="$(uci -q get sqm.cake_wan_sqm._cake_autorate_managed)"
$helper recover-stale | grep -q '"state":"recovered"'
[ "$(uci -q get cake-autorate.wan_sqm.unrelated_preserved)" = "$before_setting" ]
[ "$(uci -q get sqm.cake_wan_sqm._cake_autorate_managed)" = "$before_queue" ]
[ "$(uci -q get cake-autorate.wan_sqm.enabled)" = 0 ]
[ "$(uci -q get cake-autorate.wan_sqm.sqm_enabled)" = 0 ]
[ "$(uci -q get sqm.cake_wan_sqm.enabled)" = 0 ]
if uci -q show cake-autorate.wan_sqm | grep -q '_autotune_apply_'; then
	echo "expired marker recovery left CAKE transaction metadata" >&2
	exit 1
fi
if uci -q get sqm.cake_autorate_apply_wan_sqm >/dev/null 2>&1; then
	echo "expired marker recovery left the reserved SQM section" >&2
	exit 1
fi
$helper verify-init | grep -q '"state":"clear"'

# A reboot destroys rpcd rollback snapshots and tmpfs proof. A well-formed
# foreign-boot marker is recovered immediately but can only be made inert, not
# silently accepted, even if its wall-clock expiry is still in the future.
uci -q set cake-autorate.wan_sqm.enabled=1
uci -q set cake-autorate.wan_sqm.sqm_enabled=1
uci -q set sqm.cake_wan_sqm.enabled=1
uci -q set cake-autorate.wan_sqm._autotune_apply_guard=1
uci -q set cake-autorate.wan_sqm._autotune_apply_fingerprint="$stale_fingerprint"
uci -q set cake-autorate.wan_sqm._autotune_apply_target=pppoe-wan
uci -q set cake-autorate.wan_sqm._autotune_apply_backend=speedtest-go
uci -q set cake-autorate.wan_sqm._autotune_apply_route_mode=main
uci -q set cake-autorate.wan_sqm._autotune_apply_enabled=1
uci -q set cake-autorate.wan_sqm._autotune_apply_disable_adaptive=0
uci -q set cake-autorate.wan_sqm._autotune_apply_action=apply_sqm
uci -q set cake-autorate.wan_sqm._autotune_apply_token="$stale_token"
uci -q set cake-autorate.wan_sqm._autotune_apply_expires="$stale_expiry"
uci -q set cake-autorate.wan_sqm._autotune_apply_boot_id=aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee
uci -q set sqm.cake_autorate_apply_wan_sqm=cake_autorate_apply_guard
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_guard=1
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_job=wan_sqm
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_fingerprint="$stale_fingerprint"
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_token="$stale_token"
$helper recover-stale | grep -q '"reason":"boot-changed"'
[ "$(uci -q get cake-autorate.wan_sqm.enabled)" = 0 ]
[ "$(uci -q get cake-autorate.wan_sqm.sqm_enabled)" = 0 ]
[ "$(uci -q get sqm.cake_wan_sqm.enabled)" = 0 ]
$helper verify-init | grep -q '"state":"clear"'

# RC19 did not record a boot ID. A paired legacy marker with no surviving
# token is recoverable during an upgrade instead of permanently bricking init.
uci -q set cake-autorate.wan_sqm.enabled=1
uci -q set cake-autorate.wan_sqm.sqm_enabled=1
uci -q set sqm.cake_wan_sqm.enabled=1
uci -q set cake-autorate.wan_sqm._autotune_apply_guard=1
uci -q set cake-autorate.wan_sqm._autotune_apply_fingerprint="$stale_fingerprint"
uci -q set cake-autorate.wan_sqm._autotune_apply_target=pppoe-wan
uci -q set cake-autorate.wan_sqm._autotune_apply_backend=speedtest-go
uci -q set cake-autorate.wan_sqm._autotune_apply_route_mode=main
uci -q set cake-autorate.wan_sqm._autotune_apply_enabled=1
uci -q set cake-autorate.wan_sqm._autotune_apply_disable_adaptive=0
uci -q set cake-autorate.wan_sqm._autotune_apply_action=apply_sqm
uci -q set cake-autorate.wan_sqm._autotune_apply_token="$stale_token"
uci -q set cake-autorate.wan_sqm._autotune_apply_expires="$stale_expiry"
uci -q set sqm.cake_autorate_apply_wan_sqm=cake_autorate_apply_guard
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_guard=1
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_job=wan_sqm
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_fingerprint="$stale_fingerprint"
uci -q set sqm.cake_autorate_apply_wan_sqm._autotune_apply_token="$stale_token"
$helper recover-stale | grep -q '"reason":"legacy-token-missing"'
[ "$(uci -q get cake-autorate.wan_sqm.enabled)" = 0 ]
[ "$(uci -q get cake-autorate.wan_sqm.sqm_enabled)" = 0 ]
[ "$(uci -q get sqm.cake_wan_sqm.enabled)" = 0 ]
$helper verify-init | grep -q '"state":"clear"'

# A symlinked/foreign runtime root is never trusted.
rmdir "$guard"
ln -s "$work/config" "$guard"
if $helper verify-init >/dev/null 2>&1; then
	echo "symlinked apply-guard root was accepted" >&2
	exit 1
fi
rm "$guard"
mkdir "$guard"

# Looking up a well-formed but nonexistent token must not allocate RAM/inodes.
missing_token=dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd
before_missing="$(find "$guard" -mindepth 1 -maxdepth 1 -type d | wc -l)"
if $helper postcheck "$missing_token" >/dev/null 2>&1; then
	echo "missing apply token unexpectedly passed" >&2
	exit 1
fi
after_missing="$(find "$guard" -mindepth 1 -maxdepth 1 -type d | wc -l)"
[ "$before_missing" = "$after_missing" ] || {
	echo "missing token lookup leaked a directory" >&2
	exit 1
}

# Old malformed, unreferenced arm directories are reclaimed, while capacity
# remains bounded for recent or live transaction state.
orphan_token=eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee
mkdir "$guard/$orphan_token"
touch -d '5 minutes ago' "$guard/$orphan_token"
orphan_replacement="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 0 0 disable_sqm "$fingerprint")"
orphan_replacement_token="$(printf '%s\n' "$orphan_replacement" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
[ ! -e "$guard/$orphan_token" ]
$helper abort "$orphan_replacement_token" >/dev/null

# Bound abandoned tokens, garbage-collect only an expired root-owned token,
# and still refuse a ninth live transaction.
capacity_tokens=
capacity_first=
capacity_index=0
while [ "$capacity_index" -lt 8 ]; do
	capacity_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 0 0 disable_sqm "$fingerprint")"
	capacity_token="$(printf '%s\n' "$capacity_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
	[ -n "$capacity_token" ]
	[ -n "$capacity_first" ] || capacity_first="$capacity_token"
	capacity_tokens="${capacity_tokens:+$capacity_tokens }$capacity_token"
	capacity_index=$((capacity_index + 1))
done
if $helper arm wan_sqm pppoe-wan speedtest-go main '' 0 0 disable_sqm "$fingerprint" >/dev/null 2>&1; then
	echo "apply guard exceeded its live-token capacity" >&2
	exit 1
fi
sed -i 's/^expires_epoch=.*/expires_epoch=0/' "$guard/$capacity_first/meta"
replacement_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 0 0 disable_sqm "$fingerprint")"
replacement_token="$(printf '%s\n' "$replacement_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
[ ! -e "$guard/$capacity_first" ]
if $helper arm wan_sqm pppoe-wan speedtest-go main '' 0 0 disable_sqm "$fingerprint" >/dev/null 2>&1; then
	echo "apply guard capacity was not re-enforced after GC" >&2
	exit 1
fi
for capacity_token in $capacity_tokens "$replacement_token"; do
	$helper abort "$capacity_token" >/dev/null
done
[ "$(find "$guard" -mindepth 1 -maxdepth 1 -type d | wc -l)" -eq 0 ]

echo "apply guard tests passed"
