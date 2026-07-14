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
	`${prefix}\nreturn { formatQuality, formatRoute, formatState };`
)({}, {}, {}, {}, {}, {}, E, value => value);

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
	quality_grade_previous: {
		grade: 'A+', increase_ms: 2.5, completed_at: Date.now() / 1000 - 60,
		dl: { grade: 'A+' }, partial: true, stale: false,
	},
	quality_class: 'C',
	effective_latency_delta_ms: 80,
});
assert.equal(detected.attrs.class, 'cake-quality-stack');
assert.equal(detected.children[0].children[1].children, 'B');
assert.match(detected.children[0].children[2].children, /DL A\+.*UL B/);
assert.equal(detected.children[1].children[1].children, 'PARTIAL');

const collecting = helpers.formatQuality({
	transport_latency_enabled: true,
	quality_grade_state: 'collecting',
	quality_grade_collected_samples: 2,
	quality_grade_required_samples: 3,
	quality_grade_current: null,
	quality_grade_previous: detected.children ? {
		grade: 'A', increase_ms: 10, completed_at: Date.now() / 1000 - 30,
		dl: { grade: 'A' }, stale: false,
	} : null,
});
assert.equal(collecting.children[0].children[1].children, 'COLLECTING');
assert.match(collecting.children[0].children[2].children, /2 \/ 3/);
assert.equal(collecting.children[1].children[1].children, 'A');

const noPrevious = helpers.formatQuality({
	transport_latency_enabled: true,
	quality_grade_state: 'learning_baseline',
	quality_grade_collected_samples: 0,
	quality_grade_required_samples: 3,
	quality_grade_current: null,
	quality_grade_previous: null,
});
assert.equal(noPrevious.children[0].children[1].children, 'LEARNING');
assert.equal(noPrevious.children[1].children[1].children, '-');
assert.equal(noPrevious.children[1].children[2].children, 'No completed rating yet');

assert.match(source, /cake-status-table td\{vertical-align:top!important/);
assert.match(source, /cake-status-table th\{vertical-align:bottom!important/);

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
