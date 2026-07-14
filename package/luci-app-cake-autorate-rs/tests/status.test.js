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
const prefix = source.slice(0, source.indexOf('return L.view.extend'));
const E = (tag, attrs, children) => ({ tag, attrs: attrs || {}, children: children || [] });
const helpers = new Function('fs', 'poll', 'uci', 'ui', 'cakeUi', 'L', 'E', '_',
	`${prefix}\nreturn { formatQuality, formatRoute, formatState, qualityReadiness, qualityProgressText, ` +
		`statusColumnSelection, selectedStatusColumns };`
)({}, {}, {}, {}, {}, {}, E, value => value);

assert.deepEqual(helpers.statusColumnSelection({}), [ 'instance', 'uplink', 'quality', 'rating' ]);
assert.deepEqual(helpers.statusColumnSelection({ status_columns: [ 'cpu', 'route' ] }),
	[ 'instance', 'uplink', 'quality', 'rating', 'route', 'cpu' ]);
assert.deepEqual(helpers.selectedStatusColumns([ 'cpu' ]).map(column => column.key),
	[ 'instance', 'uplink', 'quality', 'rating', 'cpu' ],
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
	transport_latency_enabled: true,
	route_active: true,
	transport_probe_trusted: true,
	quality_grade_baseline_ready: true,
});
assert.equal(ready.ready, true);
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
}, true);
assert.equal(unhealthyState.children[0].children, 'ERROR');
assert.equal(unhealthyState.children[1].children, 'CAKE/IFB unavailable');
assert.match(unhealthyState.attrs.title, /download counter is missing/);
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
assert.match(source, /List columns/);
assert.match(source, /Reset default/);
assert.match(source, /\/usr\/libexec\/cake-autorate-rs\/status-columns/,
	'column preferences must use the isolated persistence helper');
assert.doesNotMatch(source, /return uci\.save\(\)/,
	'Status preferences must not leave an uncommitted LuCI UCI transaction');
assert.match(source, /column\.mandatory \? '' : null/,
	'mandatory column checkboxes must be checked and disabled');
assert.match(source, /cake-status-cards\{display:none\}/);
assert.match(source, /@media\(max-width:900px\).*cake-status-cards\{display:grid/,
	'narrow Status pages must switch to instance cards');
assert.match(source, /width:calc\(100vw - 48px\)/,
	'Status must use the available desktop viewport width');
assert.doesNotMatch(source, /'disabled':\s*!/,
	'Boolean false must not be serialized as an HTML disabled attribute');

const route = helpers.formatRoute({
	route_mode: 'mwan3',
	mwan3_member: 'wanb',
	mwan3_member_status: 'offline',
	route_device: 'eth0',
	route_source_ip: '10.0.100.101',
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
