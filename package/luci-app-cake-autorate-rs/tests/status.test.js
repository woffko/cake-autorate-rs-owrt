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
