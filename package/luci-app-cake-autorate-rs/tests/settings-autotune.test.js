'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const sourcePath = path.join(__dirname, '..', 'htdocs', 'luci-static', 'resources',
	'view', 'cake-autorate-rs', 'settings.js');
const source = fs.readFileSync(sourcePath, 'utf8');
const prefix = source.slice(0, source.indexOf('return L.view.extend'));
const written = {};
const fixtureSections = { 'cake-autorate': [], mwan3: [] };
if (typeof String.prototype.format !== 'function') {
	String.prototype.format = function() {
		let index = 0;
		const args = arguments;
		return this.replace(/%[sd]/g, () => String(args[index++]));
	};
}
const uci = {
	set(config, section, key, value) {
		assert.equal(config, 'cake-autorate');
		written[key] = value;
	},
	unset(config, section, key) {
		assert.equal(config, 'cake-autorate');
		delete written[key];
	},
	get() {
		return null;
	},
	sections(config) {
		return fixtureSections[config] || [];
	},
};
const helpers = new Function(
	'fs', 'form', 'network', 'uci', 'ui', 'widgets', 'cakeUi', 'L', 'E', '_',
	`${prefix}\ninterfaceContext = { deviceNames: { eth1: true }, deviceNetworks: {}, ` +
		`networkDevices: {}, defaultDevice: 'eth1' };\nreturn { writeWizardConfig, validateTransportProbeUrl, ` +
		`buildMwan3Context, uniqueMwan3Uplinks, multiwanInstancePlans, wizardPlanConflicts, ` +
		`setInterfaceContext: function(value) { interfaceContext = value; }, ` +
		`setMwan3Context: function(value) { mwan3Context = value; } };`
)({}, {}, {}, uci, {}, {}, {}, {}, () => ({}), value => value);

const proposal = {
	download: {
		minimum_kbps: 16700,
		base_kbps: 35500,
		maximum_kbps: 141800,
		absolute_cap_kbps: 204200,
		observed_low_kbps: 41700,
		observed_median_kbps: 95000,
	},
	upload: {
		minimum_kbps: 6500,
		base_kbps: 14300,
		maximum_kbps: 17200,
		absolute_cap_kbps: 19000,
		observed_low_kbps: 16200,
		observed_median_kbps: 16800,
	},
	active_threshold_kbps: 1600,
	thresholds_ms: { adjust_up: 6, delay: 15, adjust_down: 40 },
	adaptive_ceiling: {
		enabled: true,
		hold_s: 15,
		growth_percent: 3,
		probe_s: 8,
		cooldown_s: 45,
		failed_bound_ttl_s: 900,
	},
	link: { kind: 'cellular', layer: 'none', overhead: 0, mpu: 0 },
};

helpers.writeWizardConfig('auto_wwan', {
	name: 'auto_wwan',
	wan_if: 'eth1',
	enabled: true,
	sqm_section: 'cake_auto_wwan',
	speedtest_backend: 'speedtest-go',
	speedtest_go_server_id: '17372',
	speedtest_apply_percent: '90',
	pinger_method: 'fping',
	no_pingers: '3',
	ping_extra_args: '-I eth1',
	reflectors: [ '1.1.1.1', '9.9.9.9', '8.8.8.8' ],
	sqm_download: String(proposal.download.base_kbps),
	sqm_upload: String(proposal.upload.base_kbps),
	sqm_linklayer: proposal.link.layer,
	sqm_overhead: String(proposal.link.overhead),
	sqm_tcMPU: String(proposal.link.mpu),
	sqm_linklayer_advanced: '0',
	autotune_proposal: proposal,
});

assert.equal(written.manual_rate_limits, '1');
assert.equal(written.sqm_download, '35500');
assert.equal(written.sqm_upload, '14300');
assert.equal(written.min_dl_shaper_rate_kbps, '16700');
assert.equal(written.base_dl_shaper_rate_kbps, '35500');
assert.equal(written.max_dl_shaper_rate_kbps, '141800');
assert.equal(written.min_ul_shaper_rate_kbps, '6500');
assert.equal(written.base_ul_shaper_rate_kbps, '14300');
assert.equal(written.max_ul_shaper_rate_kbps, '17200');
assert.equal(written.connection_active_thr_kbps, '1600');
assert.equal(written.dl_avg_owd_delta_max_adjust_up_thr_ms, '6');
assert.equal(written.ul_owd_delta_delay_thr_ms, '15');
assert.equal(written.dl_avg_owd_delta_max_adjust_down_thr_ms, '40');
assert.equal(written.adaptive_ceiling_enabled, '1');
assert.equal(written.adaptive_ceiling_dl_cap_kbps, '204200');
assert.equal(written.adaptive_ceiling_ul_cap_kbps, '19000');
assert.equal(written.adaptive_ceiling_cooldown_s, '45');
assert.equal(written.transport_latency_enabled, '1');
assert.equal(written.throughput_guard_enabled, '1');
assert.equal(written.throughput_reference_dl_p20_kbps, '41700');
assert.equal(written.throughput_reference_dl_p50_kbps, '95000');
assert.equal(written.throughput_reference_ul_p20_kbps, '16200');
assert.equal(written.throughput_reference_ul_p50_kbps, '16800');
assert.equal(written.sqm_linklayer, 'none');
assert.equal(written.sqm_overhead, '0');
assert.equal(written.sqm_tcMPU, '0');
assert.equal(written.speedtest_go_server_id, '17372');
assert.equal(written.route_mode, 'main');

assert.equal(helpers.validateTransportProbeUrl(''), true);
assert.equal(helpers.validateTransportProbeUrl(null), true);
assert.equal(helpers.validateTransportProbeUrl('https://www.google.com/generate_204'), true);
assert.equal(helpers.validateTransportProbeUrl('http://example.test/probe?bytes=0'), true);
assert.equal(helpers.validateTransportProbeUrl('ftp://example.test/probe'), 'Enter an HTTP or HTTPS URL without spaces.');
assert.equal(helpers.validateTransportProbeUrl('https://example.test/has space'), 'Enter an HTTP or HTTPS URL without spaces.');

helpers.setInterfaceContext({
	deviceNames: { 'pppoe-wan': true, eth0: true },
	deviceNetworks: { 'pppoe-wan': [ 'wan', 'wan6' ], eth0: [ 'wanb', 'wanb6' ] },
	devicePhysical: { 'pppoe-wan': 'eth2' },
	networkDevices: { wan: 'pppoe-wan', wan6: 'pppoe-wan', wanb: 'eth0', wanb6: 'eth0' },
	defaultDevice: 'pppoe-wan',
});
fixtureSections.mwan3 = [
	{ '.name': 'wan', enabled: '1', family: 'ipv4' },
	{ '.name': 'wan_6', enabled: '1', family: 'ipv6' },
	{ '.name': 'wanb', enabled: '1', family: 'ipv4' },
];
const mwan3 = helpers.buildMwan3Context();
helpers.setMwan3Context(mwan3);
assert.deepEqual(mwan3.members.map(member => [ member.name, member.device ]), [
	[ 'wan', 'pppoe-wan' ], [ 'wanb', 'eth0' ],
]);
assert.equal(mwan3.byName.wan.label, 'wan — pppoe-wan — eth2');
assert.equal(mwan3.byName.wanb.label, 'wanb — eth0');
assert.equal(helpers.uniqueMwan3Uplinks().length, 2);
const plans = helpers.multiwanInstancePlans({ name: 'primary_sqm', wan_if: 'pppoe-wan' });
assert.deepEqual(plans.map(plan => [ plan.name, plan.member, plan.device, plan.sqmSection ]), [
	[ 'primary_sqm', 'wan', 'pppoe-wan', 'cake_primary_sqm' ],
	[ 'wanb_sqm', 'wanb', 'eth0', 'cake_wanb_sqm' ],
]);
assert.deepEqual(helpers.wizardPlanConflicts(plans, true), []);
fixtureSections['cake-autorate'] = [
	{ '.name': 'old_wanb', enabled: '1', manage_sqm: '1', wan_if: 'eth0' },
];
assert.match(helpers.wizardPlanConflicts(plans, true).join(' '), /old_wanb.*eth0/);
const duplicatePlans = [ plans[0], { ...plans[1], name: 'primary_sqm' } ];
assert.match(helpers.wizardPlanConflicts(duplicatePlans, false).join(' '), /duplicated/);

console.log('settings autotune tests passed');
