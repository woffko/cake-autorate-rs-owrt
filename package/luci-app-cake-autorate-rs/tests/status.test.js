'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

if (typeof String.prototype.format !== 'function') {
	String.prototype.format = function() {
		let index = 0;
		const values = arguments;
		return this.replace(/%[sd]/g, () => String(values[index++]));
	};
}

const sourcePath = path.join(__dirname, '..', 'htdocs', 'luci-static', 'resources',
	'view', 'cake-autorate-rs', 'status.js');
const source = fs.readFileSync(sourcePath, 'utf8');
for (const [index, button] of source.split("E('button', {").slice(1).entries()) {
	assert.match(button.slice(0, 160), /'type': 'button'/,
		`custom status button ${index + 1} must never act as a form submitter`);
}
const prefix = source.slice(0, source.indexOf('return L.view.extend'));
const E = (tag, attrs, children) => ({ tag, attrs: attrs || {}, children: children || [] });
const helpers = new Function('fs', 'poll', 'uci', 'ui', 'cakeUi', 'L', 'E', '_',
	`${prefix}\nreturn { formatQuality, formatRoute, formatState, formatServices, qualityReadiness, qualityProgressText, ` +
		`statusColumnSelection, selectedStatusColumns, formatShaperRate };`
)({}, {}, {}, {}, {}, {}, E, value => value);

const passiveCapacity = helpers.formatShaperRate({
	configured_max_dl_shaper_rate_kbps: 100000,
	effective_max_dl_shaper_rate_kbps: 110000,
	adaptive_ceiling_dl_cap_kbps: 150000,
	adaptive_ceiling_dl_last_reason: 'clean transport saturation',
	adaptive_capacity: {
		enabled: true,
		route_epoch: 'mwan3|wan|pppoe-wan|198.51.100.1',
		download: {
			phase: 'hold_no_effect', current_rate_kbps: 90000,
			safe_ceiling_kbps: 110000, failed_bound_kbps: 120000,
			runtime_minimum_kbps: 70000, probe_target_kbps: null,
			no_cake_effect: true, causal_state: 'no_cake_effect',
			confidence: { percent: 82 }
		}
	}
}, 'dl');
assert.match(passiveCapacity.children[1].children, /safe: 110000 kbps.*confidence: 82%/);
assert.match(passiveCapacity.attrs.title, /Runtime minimum: 70000 kbps/);
assert.match(passiveCapacity.attrs.title, /No CAKE effect: yes/);

assert.deepEqual(helpers.statusColumnSelection({}),
	[ 'instance', 'uplink', 'services', 'quality', 'rating' ]);
assert.deepEqual(helpers.statusColumnSelection({ status_columns: [ 'cpu', 'route' ] }),
	[ 'instance', 'uplink', 'services', 'quality', 'rating', 'route', 'cpu' ]);
assert.deepEqual(helpers.selectedStatusColumns([ 'cpu' ]).map(column => column.key),
	[ 'instance', 'uplink', 'services', 'quality', 'rating', 'cpu' ],
	'mandatory columns must remain visible even when omitted by saved preferences');

const quality = helpers.formatQuality({
	transport_latency_enabled: true,
	transport_status: 'baseline_ready',
	quality_class: 'LEARNING',
	quality_dl_class: 'A+',
	quality_ul_class: 'A',
	quality_confidence: 50,
	throughput_floor_dl_kbps: 50000,
	throughput_floor_ul_kbps: 10000,
});
assert.equal(quality.children[0].children, 'BASELINE READY');
assert.match(quality.children[1].children, /Waiting for loaded traffic.*50%/);

const detected = helpers.formatQuality({
	transport_latency_enabled: true,
	quality_grade_state: 'provisional',
	quality_grade_collected_samples: 5,
	quality_grade_required_samples: 3,
	quality_grade_current: {
		grade: 'B', increase_ms: 45.5, started_at: Date.now() / 1000 - 10,
		dl: { grade: 'A+' }, ul: { grade: 'B' }, partial: false, stale: false,
	},
	quality_grade_last_known: {
		grade: 'A+', increase_ms: 2.5, completed_at: Date.now() / 1000 - 60,
		dl: { grade: 'A+' }, ul: { grade: 'A+' }, partial: false, incomplete: false, stale: false,
	},
	quality_class: 'C',
	effective_latency_delta_ms: 80,
	rating_load_phase: 'DL',
	rating_capture_requested_phase: 'DL',
	rating_load_reference_dl_kbps: 900000,
	rating_load_reference_ul_kbps: 860000,
	rating_load_enter_dl_kbps: 135000,
	rating_load_enter_ul_kbps: 129000,
	rating_load_enter_dl_percent: 15,
	rating_load_enter_ul_percent: 15,
	rating_load_aggregate_dl_kbps: 400000,
	rating_load_aggregate_ul_kbps: 10000,
	rating_load_effective_dl_kbps: 398000,
	rating_load_effective_ul_kbps: 8000,
	rating_capture_background_dl_kbps: 2000,
	rating_capture_background_ul_kbps: 2000,
	rating_capture_contaminated: false,
});
assert.equal(detected.attrs.class, 'cake-quality-stack');
assert.equal(detected.children[0].children[1].children, 'B');
assert.match(detected.children[0].children[2].children, /DL A\+.*UL B/);
assert.equal(detected.children[1].children[0].children, 'LAST KNOWN');
assert.equal(detected.children[1].children[1].children, 'A+');
assert.match(detected.attrs.title, /Current CAKE reference: DL 900000 kbps · UL 860000 kbps/);
assert.match(detected.attrs.title, /Current triggers: DL 135000 kbps \(15\.0%\)/);

const collecting = helpers.formatQuality({
	transport_latency_enabled: true,
	quality_grade_state: 'collecting',
	quality_grade_collected_samples: 2,
	quality_grade_required_samples: 3,
	quality_grade_dl_samples: 2,
	quality_grade_ul_samples: 0,
	quality_grade_current: null,
	quality_grade_last_known: detected.children ? {
		grade: 'A', increase_ms: 10, completed_at: Date.now() / 1000 - 30,
		dl: { grade: 'A' }, ul: { grade: 'A' }, partial: false, incomplete: false, stale: false,
	} : null,
});
assert.equal(collecting.children[0].children[1].children, 'COLLECTING');
assert.match(collecting.children[0].children[2].children, /DL 2\/3.*UL 0\/3/);
assert.equal(collecting.children[1].children[1].children, 'A');

const waitingWithLastKnown = helpers.formatQuality({
	transport_latency_enabled: true,
	quality_grade_state: 'baseline_ready',
	quality_grade_collected_samples: 0,
	quality_grade_required_samples: 20,
	quality_grade_current: null,
	quality_grade_last_known: {
		grade: 'B', increase_ms: 45, completed_at: Date.now() / 1000 - 60,
		dl: { grade: 'A' }, ul: { grade: 'B' }, partial: false, incomplete: false, stale: false,
	},
});
assert.equal(waitingWithLastKnown.children[0].children[1].children, 'WAITING FOR DATA');
assert.match(waitingWithLastKnown.children[0].children[2].children, /Run Get rating/);
assert.equal(waitingWithLastKnown.children[1].children[1].children, 'B');

const noLastKnown = helpers.formatQuality({
	transport_latency_enabled: true,
	quality_grade_state: 'learning_baseline',
	quality_grade_collected_samples: 0,
	quality_grade_required_samples: 3,
	quality_grade_current: null,
	quality_grade_last_known: null,
});
assert.equal(noLastKnown.children[0].children[1].children, 'LEARNING');
assert.equal(noLastKnown.children[1].children[0].children, 'LAST KNOWN');
assert.equal(noLastKnown.children[1].children[1].children, '-');
assert.equal(noLastKnown.children[1].children[2].children, 'No complete rating known yet');

const incomplete = helpers.formatQuality({
	transport_latency_enabled: true,
	quality_grade_state: 'final',
	quality_grade_current: {
		grade: 'LEARNING', increase_ms: 0, completed_at: Date.now() / 1000,
		partial: false, incomplete: true, dl_samples: 4, ul_samples: 0,
	},
	quality_grade_last_known: {
		grade: 'A', increase_ms: 10, completed_at: Date.now() / 1000 - 30,
		dl: { grade: 'A' }, ul: { grade: 'A' }, partial: false, incomplete: false,
	},
});
assert.equal(incomplete.children[0].children[1].children, 'INCOMPLETE');
assert.equal(incomplete.children[1].children[1].children, 'A');

const rejectedLastKnown = helpers.formatQuality({
	transport_latency_enabled: true,
	quality_grade_state: 'final',
	quality_grade_current: null,
	quality_grade_last_known: {
		grade: 'B', increase_ms: 50, completed_at: Date.now() / 1000 - 30,
		partial: true, incomplete: false,
	},
});
assert.equal(rejectedLastKnown.children[1].children[1].children, '-');

const ready = helpers.qualityReadiness({ enabled: '1', sqm_enabled: '1' }, {
	uplink_state: 'ACTIVE',
	transport_latency_enabled: true,
	route_active: true,
	route_test_ready: true,
	transport_probe_trusted: true,
	quality_grade_baseline_ready: true,
});
assert.equal(ready.ready, true);
const standbyAutomatic = helpers.qualityReadiness({ enabled: '1', sqm_enabled: '1' }, {
	uplink_state: 'STANDBY',
	transport_latency_enabled: true,
	route_active: false,
	route_test_ready: true,
	transport_probe_trusted: true,
	quality_grade_baseline_ready: true,
}, 'automatic');
assert.equal(standbyAutomatic.ready, true);
assert.match(standbyAutomatic.reason, /isolated mwan3 member/);
const standbyGuided = helpers.qualityReadiness({ enabled: '1', sqm_enabled: '1' }, {
	uplink_state: 'STANDBY',
	transport_latency_enabled: true,
	route_active: false,
	route_test_ready: true,
	transport_probe_trusted: true,
	quality_grade_baseline_ready: true,
}, 'client');
assert.equal(standbyGuided.ready, false);
assert.match(standbyGuided.reason, /Guided client mode requires client traffic/);
const rechecking = helpers.qualityReadiness({ enabled: '1', sqm_enabled: '1' }, {
	uplink_state: 'RECHECKING',
	transport_latency_enabled: true,
	route_active: false,
	route_test_ready: false,
	transport_probe_trusted: true,
	quality_grade_baseline_ready: true,
});
assert.equal(rechecking.ready, false);
assert.match(rechecking.reason, /being rechecked/);
const unhealthySqm = helpers.qualityReadiness({ enabled: '1', sqm_enabled: '1' }, {
	sqm_runtime_managed: true,
	sqm_runtime_healthy: false,
	sqm_runtime_reason: 'CAKE qdisc is missing on ifb4eth0',
	transport_latency_enabled: true,
	route_active: true,
	transport_probe_trusted: true,
	quality_grade_baseline_ready: true,
});
assert.equal(unhealthySqm.ready, false);
assert.match(unhealthySqm.reason, /CAKE qdisc is missing on ifb4eth0/);
const unhealthyState = helpers.formatState({
	state: 'RUNNING',
	sqm_runtime_managed: true,
	sqm_runtime_healthy: false,
	sqm_runtime_state: 'ERROR',
	sqm_runtime_reason: 'download counter is missing for ifb4eth0',
}, true, { autotune_profile: 'best_overall', traffic_rules_enabled: '0' }, null);
assert.equal(unhealthyState.children[0].children, 'ERROR');
assert.equal(unhealthyState.children[1].children, 'CAKE/IFB unavailable');
assert.equal(unhealthyState.children[3].children, 'Auto-Tune: Best overall');
assert.equal(unhealthyState.children[4].children, 'Learning: Configured bounds');
assert.equal(unhealthyState.children[5].children, 'Priorities: Off');
assert.match(unhealthyState.attrs.title, /download counter is missing/);
const waitingState = helpers.formatState({
	state: 'WAITING_SQM',
	uplink_state: 'LEARNING',
	sqm_runtime_managed: true,
	sqm_runtime_healthy: false,
	sqm_runtime_state: 'WAITING_SQM',
	sqm_runtime_reason: 'waiting for native SQM hotplug',
	started_at: Date.now() / 1000 - 120,
}, true, { autotune_profile: 'best_overall', traffic_rules_enabled: '0' }, null);
assert.equal(waitingState.children[0].children, 'WAITING');
assert.equal(waitingState.children[1].children, 'Waiting for SQM hotplug to settle');
assert.match(waitingState.children[0].attrs.style, /d08b20/);
assert.doesNotMatch(JSON.stringify(waitingState), /No probe replies/);
const scheduledState = helpers.formatState({
	state: 'RUNNING', uplink_state: 'ACTIVE',
	scheduled_autotune: {
		enabled: true, state: 'deferred', message: 'waiting for window',
		next_due_at: Date.now() / 1000 + 3600,
		daily: { remaining_bytes: 1024 * 1024 * 1024 },
		monthly: { remaining_bytes: 8 * 1024 * 1024 * 1024 },
		accounting_error: false,
	},
}, true, {
	autotune_profile: 'best_overall', traffic_rules_enabled: '0',
	scheduled_autotune_enabled: '1',
}, null);
assert.match(JSON.stringify(scheduledState), /Active budget: 1\.00 GiB today.*8\.00 GiB this month/);
assert.match(JSON.stringify(scheduledState), /due/);
const waitingLinkState = helpers.formatState({
	state: 'WAITING_LINK', uplink_state: 'OFFLINE',
	sqm_runtime_managed: true, sqm_runtime_healthy: false,
	sqm_runtime_state: 'WAITING_LINK',
	sqm_runtime_reason: 'target interface pppoe-wan is unavailable',
}, true, { autotune_profile: 'best_overall', traffic_rules_enabled: '0' }, null);
assert.equal(waitingLinkState.children[0].children, 'WAITING');
assert.equal(waitingLinkState.children[1].children,
	'WAN link unavailable · automatic recovery armed');
const profiledState = helpers.formatState({
	state: 'RUNNING', uplink_state: 'ACTIVE',
}, true, {
	autotune_profile: 'gaming', traffic_rules_enabled: '1', traffic_profile: 'auto',
}, {
	autotune_profile: 'gaming', traffic_profile_mode: 'auto',
	traffic_profile_resolved: 'gaming', classifier_state: 'ACTIVE',
});
assert.equal(profiledState.children[2].children, 'Auto-Tune: Gaming');
assert.equal(profiledState.children[3].children, 'Learning: Configured bounds');
assert.equal(profiledState.children[4].children, 'Priorities: Gaming · linked');
const healthyServices = helpers.formatServices({
	overall_state: 'HEALTHY',
	autorate_state: 'RUNNING',
	autorate_processes: 1,
	sqm_config_state: 'ENABLED',
	sqm_section: 'cake_wan_sqm',
	cake_ul_state: 'ACTIVE',
	cake_ul_rate_kbps: 806000,
	cake_ul_mode: 'diffserv4',
	cake_dl_state: 'ACTIVE',
	cake_dl_rate_kbps: 801000,
	ul_interface: 'pppoe-wan',
	dl_interface: 'ifb4pppoe-wan',
	ifb_state: 'PRESENT',
	ingress_state: 'ACTIVE',
	classifier_state: 'ACTIVE',
	classifier_profile: 'best_overall',
	traffic_profile_mode: 'auto',
	traffic_profile_resolved: 'best_overall',
	classifier_target: 'pppoe-wan',
	classifier_applied_profile: 'best_overall',
	classifier_applied_autotune_profile: 'best_overall',
	classifier_applied_configured_profile: 'auto',
	classifier_applied_resolved_profile: 'best_overall',
	operation_state: 'IDLE',
	apply_state: 'IDLE',
	issues: '',
});
assert.equal(healthyServices.children[0].children, 'HEALTHY');
assert.match(healthyServices.attrs.class, /cake-services-healthy/);
assert.equal(healthyServices.attrs.title, 'Overall: HEALTHY');
assert.match(JSON.stringify(healthyServices), /Upload CAKE: ACTIVE on pppoe-wan at 806 Mbps/);
assert.match(JSON.stringify(healthyServices), /Traffic rules: ACTIVE.*configured auto.*resolved best_overall/);
assert.match(JSON.stringify(healthyServices), /Attested rules: pppoe-wan.*Auto-Tune best_overall/);
const waitingServices = helpers.formatServices({
	overall_state: 'WAITING',
	autorate_state: 'RUNNING',
	autorate_processes: 1,
	controller_state: 'WAITING_LINK',
	controller_reason: 'target interface pppoe-wan is unavailable',
	controller_status_fresh: true,
	sqm_config_state: 'ENABLED',
	sqm_section: 'cake_wan_sqm',
	cake_ul_state: 'MISSING',
	cake_dl_state: 'MISSING',
	ifb_state: 'MISSING',
	ingress_state: 'MISSING',
	classifier_state: 'MISSING',
	operation_state: 'IDLE',
	apply_state: 'IDLE',
});
assert.equal(waitingServices.children[0].children, 'WAITING');
assert.match(waitingServices.attrs.class, /cake-services-waiting/);
assert.match(waitingServices.attrs.title, /Overall: WAITING/);
assert.match(waitingServices.attrs.title, /target interface pppoe-wan is unavailable/);
assert.doesNotMatch(waitingServices.attrs.title, /Upload CAKE/);
assert.match(JSON.stringify(waitingServices), /automatic recovery is armed|target interface pppoe-wan/);
const orphanedServices = helpers.formatServices({
	overall_state: 'ORPHANED',
	autorate_state: 'DISABLED',
	autorate_processes: 0,
	sqm_config_state: 'DISABLED',
	sqm_section: 'cake_wan_sqm',
	cake_ul_state: 'ORPHANED',
	cake_ul_rate_kbps: 794400,
	cake_dl_state: 'ORPHANED',
	cake_dl_rate_kbps: 792000,
	ul_interface: 'pppoe-wan',
	dl_interface: 'ifb4pppoe-wan',
	ifb_state: 'ORPHANED',
	ingress_state: 'ORPHANED',
	operation_state: 'IDLE',
	apply_state: 'IDLE',
	issues: 'Upload CAKE still limits traffic although the instance is disabled.',
});
assert.equal(orphanedServices.children[0].children, 'ORPHANED');
assert.match(orphanedServices.attrs.class, /cake-services-orphaned/);
assert.equal(orphanedServices.attrs.title, 'Overall: ORPHANED');
assert.match(JSON.stringify(orphanedServices), /Detected issue: Upload CAKE still limits traffic/);
const learning = helpers.qualityReadiness({ enabled: '1', sqm_enabled: '1' }, {
	transport_latency_enabled: true,
	route_active: true,
	transport_probe_trusted: true,
	quality_grade_baseline_ready: false,
	quality_grade_baseline_samples: 7,
	quality_grade_baseline_required_samples: 20,
});
assert.equal(learning.ready, false);
assert.match(learning.reason, /7 \/ 20/);
assert.match(helpers.qualityProgressText({
	baseline_samples: 20, baseline_required: 20,
	dl_samples: 12, ul_samples: 9, required_samples: 20,
	phase: 'DL', smoothed_dl_percent: 81, smoothed_ul_percent: 3,
	requested_phase: 'DL', effective_dl_kbps: 730000, effective_ul_kbps: 1000,
	enter_dl_kbps: 135000, enter_ul_kbps: 129000,
	reference_dl_kbps: 900000, reference_ul_kbps: 860000,
	background_dl_kbps: 2000, background_ul_kbps: 1000,
}), /DL 12\/20.*UL 9\/20.*Phase DL.*Requested DL.*Effective DL 730000 kbps.*Trigger DL 135000 kbps.*CAKE reference DL 900000 kbps.*Background DL 2000 kbps/);

assert.match(helpers.qualityProgressText({
	contaminated: true,
	contamination_reason: 'unexpected_upload_during_download',
}), /CONTAMINATED: unexpected_upload_during_download/);

assert.match(source, /cake-status-table td\{vertical-align:top!important/);
assert.match(source, /cake-status-table th\{vertical-align:bottom!important/);
assert.match(source, /quality-test/);
assert.match(source, /Get rating/);
assert.match(source, /refreshReadiness\(false\)[\s\S]*qualityTestExec\(instance, 'start'/,
	'Get rating must refresh daemon readiness immediately before launching a job');
assert.match(source, /refreshReadiness\(false\)\.then\(function\(freshReadiness\) \{\s*if \(closed\)/,
	'closing during the final readiness read must prevent a background rating launch');
assert.match(source, /if \(!freshStatus\) \{[\s\S]*Waiting for fresh runtime status from the controller/,
	'a missing runtime snapshot must be reported as transient readiness, not a misleading configuration error');
assert.match(source, /function pollReadiness\(\)[\s\S]*refreshReadiness\(true\)[\s\S]*then\(pollReadiness\)/,
	'an open idle rating dialog must follow recovery and baseline changes live');
assert.match(source, /List columns/);
assert.match(source, /Reset default/);
assert.match(source, /\/usr\/libexec\/cake-autorate-rs\/status-columns/,
	'column preferences must use the isolated persistence helper');
assert.match(source, /\/usr\/libexec\/cake-autorate-rs\/runtime-health/,
	'Status must reconcile configured intent with actual daemon and kernel state');
assert.doesNotMatch(source, /return uci\.save\(\)/,
	'Status preferences must not leave an uncommitted LuCI UCI transaction');
assert.match(source, /column\.mandatory \? '' : null/,
	'mandatory column checkboxes must be checked and disabled');
assert.match(source, /cake-status-cards\{display:none\}/);
assert.match(source, /@media\(max-width:900px\).*cake-status-cards\{display:grid/,
	'narrow Status pages must switch to instance cards');
assert.match(source, /cake-status-root\{width:100%;max-width:100%;min-width:0;margin:0/,
	'Status must remain inside the LuCI content container');
assert.doesNotMatch(source, /cake-status-root\{[^}]*100vw/,
	'Status must not escape the LuCI content container through viewport units');
assert.match(source, /cake-status-table-compact\{min-width:0;table-layout:fixed\}/,
	'the five mandatory columns must use a compact fixed layout');
assert.match(source, /cake-status-table-compact th,.cake-status-table-compact td\{min-width:0!important;max-width:none!important;box-sizing:border-box!important/,
	'mandatory table cells must not expand past their assigned desktop columns');
assert.match(source, /cake-status-table-compact \[data-column="instance"\] \*.*\[data-column="uplink"\] \*.*white-space:normal!important;overflow-wrap:anywhere/,
	'inline no-wrap content must not bleed across compact mandatory columns');
assert.match(source, /cake-status-table-expanded\{min-width:max-content;table-layout:auto\}/,
	'optional columns must overflow only inside the table scroller');
assert.match(source, /cake-quality-action\{min-width:145px;display:flex;flex-direction:column;align-items:flex-start;gap:5px\}/,
	'rating action and readiness text must stack without overlapping in mobile cards');
assert.doesNotMatch(source, /'disabled':\s*!/,
	'Boolean false must not be serialized as an HTML disabled attribute');

const route = helpers.formatRoute({
	route_mode: 'mwan3',
	mwan3_member: 'wanb',
	mwan3_member_status: 'offline',
	route_device: 'eth0',
	route_source_ip: '192.0.2.101',
	route_external_ip: '203.0.113.10',
	route_fwmark: '0x200',
	route_table: '2',
	route_active: false,
	uplink_error_code: 'member_offline',
	uplink_reason: 'member wanb is offline',
});
assert.match(route.attrs.title, /fwmark: 0x200/);
assert.match(route.attrs.title, /Routing table: 2/);
assert.match(route.attrs.title, /Uplink error code: member_offline/);
assert.equal(route.children[0].children, 'wanb → eth0');

console.log('status.js tests passed');
