#!/bin/sh
set -eu

base="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
helper="$base/root/usr/libexec/cake-autorate-rs/apply-guard"
fixtures="$base/tests/fixtures/apply-guard"
runtime_lock="$(CDPATH= cd -- "$base/../cake-autorate-rs/files/usr/libexec/cake-autorate-rs" && pwd)/runtime-lock"
work="${TMPDIR:-/tmp}/cake-apply-guard-test.$$"
config="$work/config"
autotune="$work/autotune"
guard="$work/guard"
mkdir -p "$config" "$autotune/wan_sqm" "$work/sys/pppoe-wan" "$work/sys/ifb4pppoe-wan" "$work/proc/200"
trap 'rm -rf "$work"' EXIT INT TERM
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
  "state":"complete", "schema_version":6,
  "producer":"cake-autorate-rs-autotune", "profile":"best_overall",
  "run_id":"apply-guard-test-run", "auto_apply_eligible":true,
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
  "validation_thresholds":{"candidate_realization_min_percent":80,
    "candidate_realization_max_percent":110,"capacity_retention_min_percent":80,
    "delay_max_ms":30,"loss_max_percent":3,"cpu_max_percent":85},
  "validation":{"profile":"best_overall","pass":true,"hard_pass":true,"safety_pass":true,
    "quality_target_met":true,"actual_grade":"A","effective_delta_ms":10,
    "contaminated":false,"candidate_base":{"download_kbps":80000,"upload_kbps":20000},
    "correction":{"action":"none","feasible":true}},
	  "profile_outcome":{"mode":"target-a-met","objective":"balanced-quality-throughput",
	    "target_grade":"A","target_met":true,"actual_grade":"A","capacity_floor_percent":80,
	    "capacity_floor_met":true,"infeasible_reason":"","manual_only":false,
	    "selected_pair":{"download_kbps":80000,"upload_kbps":20000}},
  "profile_search":{
    "download":{"schema_version":1,"profile":"best_overall","direction":"download",
      "action":"complete","selected":{"candidate_kbps":80000,"safety_pass":true,"target_met":true}},
    "upload":{"schema_version":1,"profile":"best_overall","direction":"upload",
      "action":"complete","selected":{"candidate_kbps":20000,"safety_pass":true,"target_met":true}}},
  "pinger_plan":{"recommended_method":"fping","recommended_no_pingers":3,
    "recommended_reflectors":["1.1.1.1","9.9.9.9","8.8.8.8"]},
  "proposal":{
    "schema_version":3,"profile":"best_overall","target_grade":"A",
    "quality_target_required":true,"throughput_priority":false,
    "download":{"minimum_kbps":40000,"base_kbps":80000,"maximum_kbps":90000,
      "absolute_cap_kbps":95000,"observed_low_kbps":85000,"observed_median_kbps":88000,
      "observed_high_kbps":92000},
    "upload":{"minimum_kbps":10000,"base_kbps":20000,"maximum_kbps":24000,
      "absolute_cap_kbps":25000,"observed_low_kbps":21000,"observed_median_kbps":23000,
      "observed_high_kbps":24500},
    "active_threshold_kbps":2000,
    "thresholds_ms":{"adjust_up":6,"delay":15,"adjust_down":40},
    "adaptive_ceiling":{"enabled":true,"hold_s":15,"growth_percent":3,
      "probe_s":8,"cooldown_s":45,"failed_bound_ttl_s":900},
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
result.validation_thresholds.loss_max_percent = 1;
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
fs.writeFileSync(process.argv[3], JSON.stringify(result));
EOF
gaming_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint")"
gaming_token="$(printf '%s\n' "$gaming_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
[ "$(uci -c "$guard/$gaming_token/expected" -q get cake-autorate.wan_sqm.autotune_profile)" = gaming ]
[ "$(uci -c "$guard/$gaming_token/expected" -q get cake-autorate.wan_sqm.quality_target_delay_ms)" = 5 ]
[ "$(uci -c "$guard/$gaming_token/expected" -q get cake-autorate.wan_sqm.sqm_script)" = layer_cake.qos ]
[ "$(uci -c "$guard/$gaming_token/expected" -q get cake-autorate.wan_sqm.sqm_squash_dscp)" = 0 ]
[ "$(uci -c "$guard/$gaming_token/expected" -q get cake-autorate.wan_sqm.sqm_iqdisc_opts)" = diffserv4 ]
$helper abort "$gaming_token" >/dev/null
cp "$work/result.valid" "$autotune/wan_sqm/result.json"

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
if $helper verify-init >/dev/null 2>&1; then
	echo "apply guard accepted a stale CAKE manifest" >&2
	exit 1
fi
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

# The init-launched server supervisor owns postcheck, marker cleanup and UCI
# confirmation. Closing or refreshing LuCI after callApply therefore cannot
# strand a token-dependent marker in persistent configuration.
cp "$work/result.valid" "$autotune/wan_sqm/result.json"
export APPLY_GUARD_DAEMON_RUNNING=0
supervised_arm="$($helper arm wan_sqm pppoe-wan speedtest-go main '' 1 0 apply_sqm "$fingerprint")"
supervised_token="$(printf '%s\n' "$supervised_arm" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
cp "$guard/$supervised_token/expected/cake-autorate" "$config/cake-autorate"
cp "$guard/$supervised_token/expected/sqm" "$config/sqm"
$helper verify-init | grep -q '"state":"verified"'
export APPLY_GUARD_DAEMON_RUNNING=1
$helper supervise "$supervised_token" | grep -q '"state":"finalized"'
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
result.validation_thresholds.loss_max_percent = 5;
result.validation = {
	profile: 'fair',
	pass: false,
	hard_pass: true,
	safety_pass: true,
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
	capacity_floor_met: true, infeasible_reason: '',
	selected_pair: { download_kbps: 80000, upload_kbps: 20000 }
};
result.profile_search = {
	download: { schema_version: 1, profile: 'fair', direction: 'download', action: 'complete',
		selected: { candidate_kbps: 80000, safety_pass: true, target_met: false } },
	upload: { schema_version: 1, profile: 'fair', direction: 'upload', action: 'complete',
		selected: { candidate_kbps: 20000, safety_pass: true, target_met: false } }
};
result.fair_outcome = {
	mode: 'sqm-disable-recommended',
	target_grade: 'C',
	target_delta_ms: 200,
	capacity_floor_percent: 90,
	capacity_floor_met: true,
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
			sqm_paused: true
		},
		grade: 'D',
		effective_delta_ms: 218,
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
fs.writeFileSync(process.argv[3], JSON.stringify(result));
EOF
cp "$autotune/wan_sqm/result.json" "$work/result.fair-disable"
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

# A proven repeatable compute ceiling may support the same explicit no-SQM
# action even though no shaped candidate can satisfy Fair's immutable 90%
# floor. The unsafe candidate is diagnostic only and can never be applied.
node - "$work/result.fair-disable" "$autotune/wan_sqm/result.json" <<'EOF'
const fs = require('node:fs');
const result = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
result.validation.hard_pass = false;
result.validation.safety_pass = false;
result.validation.quality_target_met = true;
result.validation.actual_grade = 'A';
result.validation.effective_delta_ms = 10;
result.profile_outcome.mode = 'capacity-floor-infeasible';
result.profile_outcome.target_met = true;
result.profile_outcome.actual_grade = 'A';
result.profile_outcome.capacity_floor_met = false;
result.profile_outcome.infeasible_reason = 'download:repeatable-compute-ceiling-below-capacity-floor;upload:repeatable-compute-ceiling-below-capacity-floor';
for (const direction of [ 'download', 'upload' ]) {
	result.profile_search[direction].action = 'fallback';
	result.profile_search[direction].reason = 'repeatable-compute-ceiling-below-capacity-floor';
	result.profile_search[direction].selected.safety_pass = false;
}
result.fair_outcome.capacity_floor_met = false;
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

# Once the same-boot transaction has expired, only its metadata is removed.
# The selected instance settings and managed queue remain intact.
uci -q set cake-autorate.wan_sqm._autotune_apply_expires=0
before_setting="$(uci -q get cake-autorate.wan_sqm.unrelated_preserved)"
before_queue="$(uci -q get sqm.cake_wan_sqm._cake_autorate_managed)"
$helper recover-stale | grep -q '"state":"recovered"'
[ "$(uci -q get cake-autorate.wan_sqm.unrelated_preserved)" = "$before_setting" ]
[ "$(uci -q get sqm.cake_wan_sqm._cake_autorate_managed)" = "$before_queue" ]
if uci -q show cake-autorate.wan_sqm | grep -q '_autotune_apply_'; then
	echo "expired marker recovery left CAKE transaction metadata" >&2
	exit 1
fi
if uci -q get sqm.cake_autorate_apply_wan_sqm >/dev/null 2>&1; then
	echo "expired marker recovery left the reserved SQM section" >&2
	exit 1
fi
$helper verify-init | grep -q '"state":"clear"'

# A reboot is authoritative: rpcd rollback state and tmpfs tokens cannot cross
# boot IDs, so a well-formed foreign-boot marker is recovered immediately even
# if its wall-clock expiry is still in the future.
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
$helper verify-init | grep -q '"state":"clear"'

# RC19 did not record a boot ID. A paired legacy marker with no surviving
# token is recoverable during an upgrade instead of permanently bricking init.
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
