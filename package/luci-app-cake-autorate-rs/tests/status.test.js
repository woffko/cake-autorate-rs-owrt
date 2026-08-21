'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

if (typeof String.prototype.format !== 'function') {
	String.prototype.format = function() {
		let index = 0;
		const values = arguments;
		return this.replace(/%%|%[sd]/g, token => token === '%%' ? '%' : String(values[index++]));
	};
}

const sourcePath = path.join(__dirname, '..', 'htdocs', 'luci-static', 'resources',
	'view', 'cake-autorate-rs', 'status.js');
const source = fs.readFileSync(sourcePath, 'utf8');
const QUALITY_GRADE_METHOD = 'worst_of_direction_bound_icmp_and_transport_v5';
assert.doesNotMatch(source, /transport_rtt_p90_loaded_minus_p5_idle_v4/,
	'stale transport-only Rating metadata must not remain in the UI');
assert.match(source, /!nativeRatingResultValidated\(job, jobId\)/,
	'a completed Rating receipt must be bound to the current evidence contract');
assert.equal((source.match(/window\.setTimeout\(/g) || []).length, 2,
	'status timers are limited to job polling cadence and deferred Blob URL cleanup');
assert.match(source,
	/function qualityTestDelay\(\)\s*\{[\s\S]*?window\.setTimeout\(resolve, 1000\);[\s\S]*?\}/,
	'Get Rating may use a polling cadence but not a timer-owned success transition');
assert.match(source,
	/function downloadText\([\s\S]*?window\.setTimeout\(function\(\)\s*\{[\s\S]*?URL\.revokeObjectURL\(url\);/,
	'the only non-polling status timer is bounded browser resource cleanup');
assert.match(source,
	/fs\.exec\('\/usr\/sbin\/cake-autorated', \[ '--log-bundle', 'all' \]\)/,
	'diagnostic export must use the bounded native implementation');
assert.doesNotMatch(source, /\/usr\/libexec\/cake-autorate-rs\/log-bundle/,
	'diagnostic export must not retain the retired shell helper');
assert.match(source,
	/function serviceAction\(action\)\s*\{[\s\S]*?fs\.exec\('\/etc\/init\.d\/cake-autorate', \[ action \]\)/,
	'service actions must use the one unified controller plus MQTT lifecycle');
assert.doesNotMatch(source, /cake-autorate-mqtt/,
	'the browser must not invoke a retired second MQTT init service');
for (const [index, button] of source.split("E('button', {").slice(1).entries()) {
	assert.match(button.slice(0, 160), /'type': 'button'/,
		`custom status button ${index + 1} must never act as a form submitter`);
}
const prefix = source.slice(0, source.indexOf('return L.view.extend'));
const E = (tag, attrs, children) => ({ tag, attrs: attrs || {}, children: children || [] });
const helpers = new Function('fs', 'poll', 'uci', 'ui', 'cakeUi', 'L', 'E', '_',
	`${prefix}\nreturn { formatQuality, formatRoute, formatState, formatServices, qualityReadiness, qualityProgressText, ` +
		`accessMediumLabel, capacityLearningLabel, ` +
		`statusColumnSelection, selectedStatusColumns, formatShaperRate, ` +
		`runtimeHealthWithNativeOperations, ` +
		`nativeRatingRoute, nativeRatingStartArgs, nativeRatingProgress, nativeRatingStatusMatchesRequest, nativeRatingWorkerRunId, nativeRatingResultValidated, ` +
		`schedulerStatusSource, schedulerUnavailableStatus, nativeSchedulerStatusValidated, ` +
		`nativeSchedulerBatchValidated, ` +
		`readSchedulerStatuses, renderStatusData };`
)({}, {}, {}, {}, {}, {}, E, value => value);

const activeHealth = helpers.runtimeHealthWithNativeOperations({
	wan_sqm: { operation_state: 'IDLE', apply_state: 'IDLE' },
}, {
	active_operations: [ {
		instance: 'wan_sqm', operation: 'full_autotune', state: 'running',
		runtime_mutated: true,
	} ],
});
assert.deepEqual(activeHealth.wan_sqm,
	{ operation_state: 'AUTOTUNE/RUNNING', apply_state: 'NATIVE' },
	'Status must overlay the coordinator-owned operation snapshot onto runtime health');
const unsafeHealth = helpers.runtimeHealthWithNativeOperations({
	wan_sqm: { operation_state: 'IDLE', apply_state: 'IDLE' },
}, {
	active_operations: [ {
		instance: '../../wan', operation: 'full_autotune', state: 'running',
		runtime_mutated: true,
	} ],
});
assert.deepEqual(unsafeHealth.wan_sqm,
	{ operation_state: 'IDLE', apply_state: 'IDLE' },
	'unsafe coordinator operation identities must not enter the rendered status map');

assert.deepEqual(helpers.schedulerStatusSource({ native_scheduler: true }), {
	owner: 'native',
	command: '/usr/sbin/cake-autorated',
	args: [ '--calibrationctl', 'scheduler-status' ],
}, 'native scheduler availability must use one batch request');
assert.equal(helpers.schedulerStatusSource({ native_scheduler: false }), null,
	'an unavailable native scheduler must not query an alternate ledger');
assert.equal(helpers.schedulerStatusSource(null), null,
	'an unavailable runtime attestation must not guess scheduler availability');
const nativeSchedule = {
	instance: 'wan_sqm', enabled: true, initialized: true, budget_authoritative: true,
	observed_at: 1200,
	state: 'deferred', message: 'quiet-window-pending', updated_at: 1000,
	next_due_at: 1100, window: { start_hour: 2, end_hour: 5 },
	daily: { limit_bytes: 10000, used_bytes: 2000, reserved_bytes: 1000, remaining_bytes: 7000 },
	monthly: { limit_bytes: 50000, used_bytes: 4000, reserved_bytes: 1000, remaining_bytes: 45000 },
	accounting_error: false, warning: null, last_success_at: 900, last_failure_due_at: 0,
};
assert.equal(helpers.nativeSchedulerStatusValidated(nativeSchedule, 'wan_sqm'), true);
assert.equal(helpers.nativeSchedulerBatchValidated({
	schema_version: 1, owner: 'native', available: true, observed_at: 1200,
	stale: false, global_error: null,
	instances: [ nativeSchedule ], issues: [],
}), true, 'one canonical batch must carry all native per-instance scheduler status');
assert.equal(helpers.nativeSchedulerBatchValidated({
	schema_version: 1, owner: 'native', available: true, observed_at: 1200,
	stale: false, global_error: null,
	instances: [ nativeSchedule ], issues: [ Object.assign({}, nativeSchedule) ],
}), false, 'a duplicate instance in the native batch must fail closed');
assert.equal(helpers.nativeSchedulerBatchValidated({
	schema_version: 1, owner: 'native', available: true, observed_at: 1200,
	stale: false, global_error: null,
	instances: [ Object.assign({}, nativeSchedule, { instance: 'constructor' }) ], issues: [],
}), true, 'valid UCI names must not collide with JavaScript object prototype properties');
assert.equal(helpers.nativeSchedulerBatchValidated({
	schema_version: 1, owner: 'native', available: true, observed_at: 1200,
	stale: true, global_error: 'Native scheduler refresh failed; showing the last complete snapshot.',
	instances: [ nativeSchedule ], issues: [],
}), true, 'a stale last-good snapshot must carry one explicit bounded global warning');
assert.equal(helpers.nativeSchedulerBatchValidated({
	schema_version: 1, owner: 'native', available: true, observed_at: 1200,
	stale: true, global_error: null, instances: [ nativeSchedule ], issues: [],
}), false, 'stale budgets without an explicit global warning must fail closed');
assert.equal(helpers.nativeSchedulerBatchValidated({
	schema_version: 1, owner: 'native', available: true, observed_at: 1200,
	stale: false, global_error: null,
	instances: [ Object.assign({}, nativeSchedule, { observed_at: 1199 }) ], issues: [],
}), false, 'every native row must belong to the same observed batch epoch');
assert.equal(helpers.nativeSchedulerBatchValidated({
	schema_version: 1, owner: 'legacy', available: true, observed_at: 1200,
	stale: false, global_error: null, instances: [ nativeSchedule ], issues: [],
}), false, 'the native batch validator must reject a different owner');
assert.equal(helpers.nativeSchedulerBatchValidated({
	schema_version: 1, owner: 'native', available: false, observed_at: 1200,
	stale: false, global_error: null, instances: [ nativeSchedule ], issues: [],
}), false, 'an unavailable payload cannot be accepted as an authoritative native snapshot');
assert.equal(helpers.nativeSchedulerStatusValidated(Object.assign({}, nativeSchedule, {
	observed_at: -1,
}), 'wan_sqm'), false, 'negative or malformed native timestamps must fail closed');
assert.equal(helpers.nativeSchedulerStatusValidated(Object.assign({}, nativeSchedule, {
	updated_at: 1.5,
}), 'wan_sqm'), false, 'fractional native timestamps must fail closed');
assert.equal(helpers.nativeSchedulerStatusValidated(Object.assign({}, nativeSchedule, {
	window: { start_hour: 24, end_hour: 5 },
}), 'wan_sqm'), false, 'native quiet-window hours must remain inside the clock domain');
assert.equal(helpers.nativeSchedulerStatusValidated(Object.assign({}, nativeSchedule, {
	message: 'invalid\nmessage',
}), 'wan_sqm'), false, 'native status messages must reject control characters');
assert.equal(helpers.nativeSchedulerStatusValidated(Object.assign({}, nativeSchedule, {
	warning: 'invalid\twarning',
}), 'wan_sqm'), false, 'native status warnings must reject control characters');
assert.equal(helpers.nativeSchedulerBatchValidated({
	schema_version: 1, owner: 'native', available: true, observed_at: 1200,
	stale: true, global_error: 'invalid\nglobal warning',
	instances: [ nativeSchedule ], issues: [],
}), false, 'native global warnings must reject control characters');
assert.equal(helpers.nativeSchedulerStatusValidated(Object.assign({}, nativeSchedule, {
	daily: Object.assign({}, nativeSchedule.daily, { remaining_bytes: -1 }),
}), 'wan_sqm'), false, 'negative or malformed native budgets must fail closed');
assert.equal(helpers.nativeSchedulerStatusValidated(Object.assign({}, nativeSchedule, {
	daily: Object.assign({}, nativeSchedule.daily, { remaining_bytes: '7000' }),
}), 'wan_sqm'), false, 'numeric-looking strings must not satisfy the versioned JSON contract');
const unavailableSchedule = helpers.schedulerUnavailableStatus({
	'.name': 'wan_sqm', scheduled_autotune_enabled: '1',
});
assert.equal(unavailableSchedule.available, false);
assert.equal(unavailableSchedule.accounting_error, false,
	'unavailable ownership must not be mislabeled as a durable traffic-ledger block');
assert.match(unavailableSchedule.message, /no accounting source was substituted/);

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
	quality_grade_method: QUALITY_GRADE_METHOD,
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
	quality_grade_method: QUALITY_GRADE_METHOD,
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
	quality_grade_method: QUALITY_GRADE_METHOD,
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
	quality_grade_method: QUALITY_GRADE_METHOD,
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
	quality_grade_method: QUALITY_GRADE_METHOD,
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

const exactObservedPartial = helpers.formatQuality({
	transport_latency_enabled: true,
	quality_grade_method: QUALITY_GRADE_METHOD,
	quality_grade_state: 'final',
	quality_grade_current: {
		grade: 'A', increase_ms: 14.308, completed_at: Date.now() / 1000,
		partial: true, incomplete: false, completion_reason: 'download_incomplete',
		dl_samples: 0, ul_samples: 28, dl: null, ul: { grade: 'A' },
	},
	quality_grade_last_known: null,
	quality_class: 'LEARNING',
	quality_controller_class: 'A',
});
assert.equal(exactObservedPartial.children[0].children[1].children, 'PARTIAL');
assert.doesNotMatch(exactObservedPartial.children[0].attrs.class, /cake-quality-grade-a(?:\s|$)/,
	'the observed one-direction A must never receive final-grade styling');
assert.equal(exactObservedPartial.children[1].children[1].children, '-');

const unsupportedRatingContract = helpers.formatQuality({
	transport_latency_enabled: true,
	quality_grade_method: 'transport_rtt_p90_loaded_minus_p5_idle_v4',
	quality_grade_state: 'final',
	quality_grade_current: {
		grade: 'A', partial: false, incomplete: false,
	},
});
assert.equal(unsupportedRatingContract.children, 'UNAVAILABLE');
assert.match(unsupportedRatingContract.attrs.title, /unsupported evidence contract/);

const rejectedLastKnown = helpers.formatQuality({
	transport_latency_enabled: true,
	quality_grade_state: 'final',
	quality_grade_method: QUALITY_GRADE_METHOD,
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
const nativeRatingUnavailable = helpers.qualityReadiness(
	{ enabled: '1', sqm_enabled: '1' },
	{
		uplink_state: 'ACTIVE', transport_latency_enabled: true,
		route_active: true, route_test_ready: true, transport_probe_trusted: true,
		quality_grade_baseline_ready: true,
	},
	'automatic',
	{ state: 'idle', native_rating: false },
);
assert.equal(nativeRatingUnavailable.ready, false);
assert.match(nativeRatingUnavailable.reason, /Rating is unavailable/);
const ratingIdentityUnavailable = helpers.qualityReadiness(
	{ enabled: '1', sqm_enabled: '1' },
	{
		uplink_state: 'ACTIVE', transport_latency_enabled: true,
		route_active: true, route_test_ready: true, transport_probe_trusted: true,
		quality_grade_baseline_ready: true,
	},
	'automatic',
	{ state: 'idle', native_rating: true, native_operation_status_identity_version: 0 },
);
assert.equal(ratingIdentityUnavailable.ready, false);
assert.match(ratingIdentityUnavailable.reason, /Rating is unavailable/);
const ratingAlreadyActive = helpers.qualityReadiness(
	{ '.name': 'wan_sqm', enabled: '1', sqm_enabled: '1' },
	{
		uplink_state: 'ACTIVE', transport_latency_enabled: true,
		route_active: true, route_test_ready: true, transport_probe_trusted: true,
		quality_grade_baseline_ready: true,
	},
	'client',
	{
		state: 'busy', native_rating: true, native_operation_status_identity_version: 1,
		active_operations: [ {
			instance: 'wan_sqm', operation: 'guided_rating', state: 'running',
		} ],
	},
);
assert.equal(ratingAlreadyActive.ready, false);
assert.match(ratingAlreadyActive.reason, /Rating operation is already active.*reconnect/);
const autotuneAlreadyActive = helpers.qualityReadiness(
	{ '.name': 'wan_sqm', enabled: '1', sqm_enabled: '1' },
	{
		uplink_state: 'ACTIVE', transport_latency_enabled: true,
		route_active: true, route_test_ready: true, transport_probe_trusted: true,
		quality_grade_baseline_ready: true,
	},
	'automatic',
	{
		state: 'busy', native_rating: true, native_operation_status_identity_version: 1,
		active_operations: [ {
			instance: 'wan_sqm', operation: 'full_autotune', state: 'recovering',
		} ],
	},
);
assert.equal(autotuneAlreadyActive.ready, false);
assert.match(autotuneAlreadyActive.reason, /Another calibration operation is already active/);
const otherInstanceOperation = helpers.qualityReadiness(
	{ '.name': 'wan_sqm', enabled: '1', sqm_enabled: '1' },
	{
		uplink_state: 'ACTIVE', transport_latency_enabled: true,
		route_active: true, route_test_ready: true, transport_probe_trusted: true,
		quality_grade_baseline_ready: true,
	},
	'automatic',
	{
		state: 'busy', native_rating: true, native_operation_status_identity_version: 1,
		active_operations: [ {
			instance: 'wanb_sqm', operation: 'full_autotune', state: 'running',
		} ],
	},
);
assert.equal(otherInstanceOperation.ready, true,
	'an operation on another instance must not block local Rating readiness');
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
const unavailableScheduledState = helpers.formatState({
	state: 'RUNNING', uplink_state: 'ACTIVE',
	scheduled_autotune: unavailableSchedule,
}, true, {
	autotune_profile: 'best_overall', traffic_rules_enabled: '0',
	scheduled_autotune_enabled: '1',
}, null);
assert.match(JSON.stringify(unavailableScheduledState), /no accounting source was substituted/);
assert.doesNotMatch(JSON.stringify(unavailableScheduledState), /Active budget/,
	'an unavailable native ledger must not render plausible zero traffic budgets');
const initializingScheduledState = helpers.formatState({
	state: 'RUNNING', uplink_state: 'ACTIVE',
	scheduled_autotune: Object.assign({}, nativeSchedule, {
		available: true, initialized: false, budget_authoritative: false,
		state: 'initializing', message: 'Native scheduler state is initializing.',
		daily: { limit_bytes: 10000, used_bytes: 0, reserved_bytes: 0, remaining_bytes: 0 },
		monthly: { limit_bytes: 50000, used_bytes: 0, reserved_bytes: 0, remaining_bytes: 0 },
	}),
}, true, {
	autotune_profile: 'best_overall', traffic_rules_enabled: '0',
	scheduled_autotune_enabled: '1',
}, null);
assert.match(JSON.stringify(initializingScheduledState), /scheduler state is initializing/i);
assert.doesNotMatch(JSON.stringify(initializingScheduledState), /Active budget/,
	'non-authoritative initialization must never render a plausible zero budget');
const invalidScheduledState = helpers.formatState({
	state: 'RUNNING', uplink_state: 'ACTIVE',
	scheduled_autotune: Object.assign({}, nativeSchedule, {
		available: true, initialized: false, budget_authoritative: false,
		state: 'error', message: 'Native scheduler configuration is invalid.',
		accounting_error: false,
	}),
}, true, {
	autotune_profile: 'best_overall', traffic_rules_enabled: '0',
	scheduled_autotune_enabled: '1',
}, null);
assert.match(JSON.stringify(invalidScheduledState), /configuration is invalid/i);
assert.doesNotMatch(JSON.stringify(invalidScheduledState), /initializing/i,
	'an instance-local scheduler error must not be disguised as initialization');
const reviewWarningState = helpers.formatState({
	state: 'RUNNING', uplink_state: 'ACTIVE',
	scheduled_autotune: Object.assign({}, nativeSchedule, {
		available: true, state: 'idle',
		message: 'Native scheduler is ready.',
		warning: 'Scheduled result requires explicit Review.',
	}),
}, true, {
	autotune_profile: 'best_overall', traffic_rules_enabled: '0',
	scheduled_autotune_enabled: '1',
}, null);
assert.match(JSON.stringify(reviewWarningState), /Active budget/);
assert.match(JSON.stringify(reviewWarningState), /requires explicit Review/,
	'a successful calibration awaiting manual Review must be a warning, not scheduler failure');
const globalSchedulerDiagnostic = helpers.renderStatusData([], [], [], {}, {}, [ {
	instance: 'retired_sqm', message: 'Durable state has no current UCI configuration.',
} ]);
assert.match(JSON.stringify(globalSchedulerDiagnostic), /Calibration scheduler diagnostics/);
assert.match(JSON.stringify(globalSchedulerDiagnostic), /retired_sqm/,
	'orphan durable state must remain visible even without a matching Status row');
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
const variableState = helpers.formatState({
	state: 'RUNNING', uplink_state: 'ACTIVE',
}, true, {
	autotune_profile: 'variable_link', traffic_rules_enabled: '0',
	capacity_learning_policy: 'passive_bounded', access_medium: 'cellular',
	access_medium_source: 'network_protocol', access_medium_confidence_percent: '95',
}, null);
assert.equal(variableState.children[2].children, 'Auto-Tune: Variable link');
assert.equal(variableState.children[3].children, 'Learning: Bounded passive learning');
assert.equal(variableState.children[4].children, 'Access: 4G / 5G cellular · 95% confidence');
assert.match(variableState.children[4].attrs.title, /network_protocol/);
const variableRuntimeState = helpers.formatState({
	state: 'RUNNING', uplink_state: 'ACTIVE',
}, true, {
	autotune_profile: 'variable_link', traffic_rules_enabled: '0',
	capacity_learning_policy: 'passive_bounded', access_medium: 'cellular',
	access_medium_source: 'network_protocol', access_medium_confidence_percent: '95',
}, {
	capacity_learning_policy: 'scheduled_active', access_medium: 'fixed_wireless',
	access_medium_source: 'device_type', access_medium_confidence_percent: 65,
});
assert.equal(variableRuntimeState.children[3].children,
	'Learning: Passive + scheduled active');
assert.equal(variableRuntimeState.children[4].children,
	'Access: Fixed wireless / WISP · 65% confidence');
assert.match(variableRuntimeState.children[4].attrs.title, /device_type/);
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

const automaticRatingArgs = helpers.nativeRatingStartArgs({
	'.name': 'wan_sqm', sqm_interface: 'pppoe-wan', route_mode: 'auto',
	mwan3_member: 'wan_member',
}, 'automatic');
assert.deepEqual(automaticRatingArgs, [
	'rating-start', '--instance', 'wan_sqm', '--expected-target', 'pppoe-wan',
	'--mode', 'automatic', '--backend', 'speedtest-go', '--route-mode', 'mwan3',
	'--mwan3-member', 'wan_member',
]);
const guidedRatingArgs = helpers.nativeRatingStartArgs({
	'.name': 'wan_sqm', sqm_interface: 'pppoe-wan', route_mode: 'main',
}, 'client');
assert.deepEqual(guidedRatingArgs, [
	'rating-start', '--instance', 'wan_sqm', '--expected-target', 'pppoe-wan',
	'--mode', 'client', '--backend', 'client', '--route-mode', 'main',
]);
assert.deepEqual(helpers.nativeRatingProgress({
	quality_grade_baseline_samples: 20,
	quality_grade_dl_samples: 9,
	quality_grade_ul_samples: 7,
	quality_grade_required_samples: 20,
	rating_load_phase: 'DL',
	rating_capture_requested_phase: 'DL',
	rating_load_effective_dl_kbps: 700000,
	rating_capture_contaminated: true,
	rating_capture_contamination_reason: 'opposite-direction-load',
}, { state: 'running', job_id: 'a'.repeat(32) }).dl_samples, 9);

const ratingJobId = 'a'.repeat(32);
const ratingSection = {
	'.name': 'wan_sqm', sqm_interface: 'pppoe-wan', route_mode: 'mwan3',
	mwan3_member: 'wan_member',
};
const ratingIdentity = {
	request_identity_schema_version: 1,
	target_interface: 'pppoe-wan',
	backend: 'speedtest-go',
	speedtest_direction: null,
	speedtest_server_id: null,
	speedtest_topology: null,
	route_mode: 'mwan3',
	mwan3_member: 'wan_member',
	target_state: 'existing_managed',
	origin: 'luci',
};
const ratingStatus = {
	state: 'running', job_id: ratingJobId,
	operation: 'automatic_rating', instance: 'wan_sqm',
	...ratingIdentity,
};
assert.equal(helpers.nativeRatingStatusMatchesRequest(
	ratingStatus, ratingSection, 'automatic_rating', ratingJobId), true);
assert.equal(helpers.nativeRatingStatusMatchesRequest(
	Object.assign({}, ratingStatus, { instance: 'wanb_sqm' }),
	ratingSection, 'automatic_rating', ratingJobId), false,
	'a Rating status from another instance must fail closed');
assert.equal(helpers.nativeRatingStatusMatchesRequest(
	Object.assign({}, ratingStatus, { operation: 'guided_rating' }),
	ratingSection, 'automatic_rating', ratingJobId), false,
	'a running Rating mode must remain bound to the started request');
for (const [ field, value ] of [
	[ 'target_interface', 'eth1' ], [ 'backend', 'client' ],
	[ 'route_mode', 'main' ], [ 'mwan3_member', 'other_member' ],
	[ 'target_state', 'absent_bootstrap' ], [ 'origin', 'scheduler' ],
]) {
	assert.equal(helpers.nativeRatingStatusMatchesRequest({
		...ratingStatus, [field]: value,
	}, ratingSection, 'automatic_rating', ratingJobId), false,
	`Rating status mutation ${field} must fail closed`);
}
const ratingWorkerRunId = 'b'.repeat(32);
assert.equal(helpers.nativeRatingWorkerRunId({ worker_run_id: null }, null, false), null);
assert.equal(helpers.nativeRatingWorkerRunId({ worker_run_id: ratingWorkerRunId }, null, true),
	ratingWorkerRunId);
assert.equal(helpers.nativeRatingWorkerRunId({ worker_run_id: 'c'.repeat(32) },
	ratingWorkerRunId, false), undefined,
	'a Rating status cannot switch worker identity during one browser operation');
assert.equal(helpers.nativeRatingWorkerRunId({ worker_run_id: null }, ratingWorkerRunId, false),
	undefined, 'a published Rating worker identity cannot disappear');
const ratingResult = {
	state: 'complete', job_id: ratingJobId,
	partial: false, incomplete: false, rating_method: QUALITY_GRADE_METHOD,
	grade: 'B', dl_grade: 'B', ul_grade: 'A', dl_samples: 20, ul_samples: 20,
	limits_changed: false,
};
assert.equal(helpers.nativeRatingResultValidated(ratingResult, ratingJobId), true);
assert.equal(helpers.nativeRatingResultValidated(
	Object.assign({}, ratingResult, { job_id: 'b'.repeat(32) }), ratingJobId), false,
	'a complete Rating result from another job must never be displayed as this capture');

for (const command of [ 'rating-current', 'rating-status', 'rating-result', 'rating-cancel' ])
	assert.match(source, new RegExp("calibrationExec\\(\\[ '" + command));
assert.match(source, /CALIBRATION_DAEMON = '\/usr\/sbin\/cake-autorated'/);
assert.match(source, /readCalibrationSummary\(\)[\s\S]*native_rating: false/,
	'a missing or old daemon capability must disable native Rating fail-closed');
assert.match(source, /job\.state === 'completed'[\s\S]*rating-result/,
	'a completed journal must be converted through the identity-checked native Rating result');

assert.match(source, /cake-status-table td\{vertical-align:top!important/);
assert.match(source, /cake-status-table th\{vertical-align:bottom!important/);
assert.doesNotMatch(source, /\/usr\/libexec\/cake-autorate-rs\/quality-test/,
	'Get rating must not invoke the competing shell Rating supervisor');
assert.match(source, /Get rating/);
assert.match(source, /refreshReadiness\(false\)[\s\S]*calibrationExec\(nativeRatingStartArgs\(section, mode\.value\)\)/,
	'Get rating must refresh daemon readiness immediately before launching a job');
assert.match(source, /refreshReadiness\(false\)\.then\(function\(freshReadiness\) \{\s*if \(closed\)/,
	'closing during the final readiness read must prevent a background rating launch');
assert.match(source, /var currentAttestation = 'pending'/,
	'Rating Start must begin behind a current-operation attestation barrier');
assert.match(source,
	/function start\(\) \{\s*if \(running \|\| starting \|\| currentAttestation !== 'settled'\)/,
	'a synthetic or rapid Start must not bypass the current-operation attestation');
assert.match(source,
	/function attestCurrentRating\(\)[\s\S]*currentAttestation = 'pending';[\s\S]*calibrationExec\(\[ 'rating-current', instance \]\)/,
	'the Start barrier must be released only by the exact native rating-current request');
assert.match(source,
	/job\.error_code === 'lease-conflict'[\s\S]*recoverRatingConflict\(\)/,
	'a conflict that wins after attestation must re-attest and adopt current state');
assert.match(source,
	/starting = false;\s*jobId = null;\s*workerRunId = null;\s*jobOperation = mode\.value === 'automatic' \? 'automatic_rating' : 'guided_rating';\s*running = true;/,
	'each newly admitted Rating must reset the previous job and worker identity before Start');
assert.match(source,
	/jobId = job\.job_id;\s*if \(closed\)\s*return calibrationExec\(\[ 'rating-cancel', jobId \]\)/,
	'closing while rating-start is in flight must cancel the exact job immediately after its receipt');
assert.match(source,
	/if \(job\.state === 'idle'\) \{\s*jobId = null;\s*workerRunId = null;\s*\}/,
	'an idle current-operation attestation must discard stale Rating identities');
assert.match(source,
	/function recoverRatingConflict\(\)[\s\S]*attestCurrentRating\(\)/,
	'lease-conflict recovery must reuse the same state-driven attestation path');
assert.doesNotMatch(source, /throw new Error\(job\.error \|\|/,
	'Rating Start must never render raw backend lease/debug text');
assert.match(source, /'disabled': ''[\s\S]*_\('Start rating'\)/,
	'the Rating button must be rendered disabled before attestation completes');
assert.match(source, /if \(!freshStatus\) \{[\s\S]*Waiting for fresh runtime status from the controller/,
	'a missing runtime snapshot must be reported as transient readiness, not a misleading configuration error');
assert.match(source, /function pollReadiness\(\)[\s\S]*refreshReadiness\(true\)[\s\S]*then\(pollReadiness\)/,
	'an open idle rating dialog must follow recovery and baseline changes live');
assert.match(source, /List columns/);
assert.match(source, /Reset default/);
assert.doesNotMatch(source, /\/usr\/libexec\/cake-autorate-rs\/status-columns/,
	'column preferences must not retain the redundant root-exec helper');
assert.match(source,
	/fs\.exec\('\/usr\/sbin\/cake-autorated', args\)/,
	'column preferences must use the native committed Status-column transaction');
assert.match(source, /fs\.exec\('\/usr\/sbin\/cake-autorated', \[ '--runtime-health' \]\)/,
	'Status must reconcile configured intent with the native read-only runtime endpoint');
assert.doesNotMatch(source, /\/usr\/libexec\/cake-autorate-rs\/runtime-health/,
	'Status must not retain the retired shell runtime-health helper');
assert.match(source,
	/return \[ sections, status\.rows,[\s\S]*status\.diagnostics \][\s\S]*readInstanceStatuses\(sections, result\[1\]\)/,
	'initial load and polling must use only the attested native scheduler summary');
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

function schedulerReader(fsMock) {
	return new Function('fs', 'poll', 'uci', 'ui', 'cakeUi', 'L', 'E', '_',
		`${prefix}\nreturn { readSchedulerStatuses };`
	)(fsMock, {}, {}, {}, {}, {
		resolveDefault: function(promise, fallback) {
			return Promise.resolve(promise).catch(function() { return fallback; });
		},
	}, E, value => value).readSchedulerStatuses;
}

async function schedulerReadTests() {
	const sections = [
		{ '.name': 'constructor', scheduled_autotune_enabled: '1' },
		{ '.name': 'wanb_sqm', scheduled_autotune_enabled: '1' },
	];
	const nativeCalls = [];
	const nativeBatch = {
		schema_version: 1, owner: 'native', available: true, observed_at: 1200,
		stale: false, global_error: null,
		instances: [ Object.assign({}, nativeSchedule, {
			instance: 'constructor', owner: 'legacy', source: 'untrusted', available: false,
		}) ],
		issues: [ Object.assign({}, nativeSchedule, {
			instance: 'wanb_sqm', initialized: false, budget_authoritative: false,
			state: 'error', message: 'Native scheduler configuration is invalid.',
			accounting_error: false,
		}), Object.assign({}, nativeSchedule, {
			instance: 'retired_sqm', initialized: false, budget_authoritative: false,
			state: 'error', message: 'Durable state has no current UCI configuration.',
			accounting_error: false,
		}) ],
	};
	const nativeRead = schedulerReader({
		exec: function(command, args) {
			nativeCalls.push([ command, args ]);
			return Promise.resolve({ stdout: JSON.stringify(nativeBatch) });
		},
	});
	const nativeResult = await nativeRead(sections, { native_scheduler: true });
	const nativeRows = nativeResult.rows;
	assert.equal(nativeCalls.length, 1, 'one Status refresh must execute one native batch call');
	assert.deepEqual(nativeCalls[0], [ '/usr/sbin/cake-autorated',
		[ '--calibrationctl', 'scheduler-status' ] ]);
	assert.equal(nativeRows[0].owner, 'native', 'nested data must not override the attested owner');
	assert.equal(nativeRows[0].source, 'native_scheduler_snapshot',
		'nested data must not override the pinned source');
	assert.equal(nativeRows[0].available, true,
		'nested data must not override batch availability');
	assert.equal(nativeRows[1].state, 'error',
		'an invalid WAN must remain isolated without hiding independent native rows');
	assert.deepEqual(nativeResult.diagnostics, [ {
		instance: 'retired_sqm', message: 'Durable state has no current UCI configuration.',
	} ], 'orphan durable state must remain visible as a global scheduler diagnostic');

	const orphanOnlyCalls = [];
	const orphanOnlyResult = await schedulerReader({
		exec: function(command, args) {
			orphanOnlyCalls.push([ command, args ]);
			return Promise.resolve({ stdout: JSON.stringify({
				schema_version: 1, owner: 'native', available: true, observed_at: 1200,
				stale: false, global_error: null, instances: [],
				issues: [ Object.assign({}, nativeSchedule, {
					instance: 'retired_sqm', initialized: false,
					budget_authoritative: false, state: 'error',
					message: 'Durable state has no current UCI configuration.',
				}) ],
			}) });
		},
	})([], { native_scheduler: true });
	assert.deepEqual(orphanOnlyResult.rows, [],
		'a native batch with no configured instances must not invent a Status row');
	assert.deepEqual(orphanOnlyResult.diagnostics, [ {
		instance: 'retired_sqm', message: 'Durable state has no current UCI configuration.',
	} ], 'an orphan diagnostic must survive even when zero instances are configured');
	assert.equal(orphanOnlyCalls.length, 1,
		'zero configured instances must still use exactly one native batch read');
	assert.deepEqual(orphanOnlyCalls[0], [ '/usr/sbin/cake-autorated',
		[ '--calibrationctl', 'scheduler-status' ] ]);
	const staleMessage = 'Native scheduler refresh failed; showing the last complete snapshot.';
	const staleResult = await schedulerReader({
		exec: function() {
			return Promise.resolve({ stdout: JSON.stringify(Object.assign({}, nativeBatch, {
				stale: true, global_error: staleMessage,
			})) });
		},
	})(sections, { native_scheduler: true });
	assert.equal(staleResult.rows[0].stale, true);
	assert.deepEqual(staleResult.diagnostics[0], {
		instance: 'Calibration scheduler', message: staleMessage,
	}, 'stale cached budgets must be accompanied by a visible global warning');

	let failedCalls = 0;
	const failedRead = schedulerReader({
		exec: function() {
			failedCalls += 1;
			return Promise.reject(new Error('native status unavailable'));
		},
	});
	const failedRows = (await failedRead(sections, { native_scheduler: true })).rows;
	assert.equal(failedCalls, 1);
	assert(failedRows.every(row => row.source === 'none' && row.available === false),
		'a failed native batch must never synthesize an accounting ledger');

	const invalidCalls = [];
	const invalidResult = await schedulerReader({
		exec: function(command, args) {
			invalidCalls.push([ command, args ]);
			return Promise.resolve({ stdout: JSON.stringify(Object.assign({}, nativeBatch, {
				owner: 'legacy',
			})) });
		},
	})(sections, { native_scheduler: true });
	assert.equal(invalidCalls.length, 1,
		'an invalid native contract must stop after one batch attempt');
	assert.deepEqual(invalidCalls[0], [ '/usr/sbin/cake-autorated',
		[ '--calibrationctl', 'scheduler-status' ] ]);
	assert(invalidResult.rows.every(function(row) {
		return row.source === 'none' && row.available === false;
	}), 'an invalid native contract must yield unavailable rows without substitution');
	assert.deepEqual(invalidResult.diagnostics, []);

	let mismatchCalls = 0;
	const mismatchRows = (await schedulerReader({
		exec: function() { mismatchCalls += 1; return Promise.resolve({}); },
	})(sections, { native_scheduler: false })).rows;
	assert.equal(mismatchCalls, 0, 'an unavailable native scheduler must not execute a status backend');
	assert(mismatchRows.every(row => row.source === 'none'));
}

schedulerReadTests().then(function() {
	console.log('status.js tests passed');
}).catch(function(error) {
	console.error(error);
	process.exitCode = 1;
});
