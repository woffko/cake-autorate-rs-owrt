'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const sourcePath = path.join(__dirname, '..', 'htdocs', 'luci-static', 'resources',
	'view', 'cake-autorate-rs', 'priorities.js');
const source = fs.readFileSync(sourcePath, 'utf8');
const prefix = source.slice(0, source.indexOf('return L.view.extend'));
const helpers = new Function('fs', 'form', 'uci', 'ui', 'cakeUi', 'L', 'E', '_',
	`${prefix}\nreturn { canonicalProfile, validatePortList, validateNetwork, profileLabel, ` +
		`parseClassifierStatus, safeInstanceName, selectedInstanceFromLocation };`
)({}, {}, {}, {}, {}, {}, () => {}, value => value);

assert.equal(helpers.canonicalProfile('balanced'), 'best_overall');
assert.equal(helpers.canonicalProfile('gaming'), 'gaming');
assert.equal(helpers.canonicalProfile('invalid'), null);

assert.equal(helpers.validatePortList(null, '53,443,27000-27100'), true);
assert.notEqual(helpers.validatePortList(null, '53; delete table'), true);
assert.notEqual(helpers.validatePortList(null, '65536'), true);
assert.notEqual(helpers.validatePortList(null, '500-100'), true);

assert.equal(helpers.validateNetwork(null, '192.168.1.50/32'), true);
assert.equal(helpers.validateNetwork(null, '2001:db8::1/128'), true);
assert.notEqual(helpers.validateNetwork(null, '192.168.1.999/32'), true);
assert.notEqual(helpers.validateNetwork(null, '192.168.1.1;drop'), true);

assert.equal(helpers.parseClassifierStatus({ stdout: '{"state":"active","table_present":true}\n' }).state,
	'active');
assert.equal(helpers.parseClassifierStatus({ stdout: 'not-json' }).state, 'invalid');
assert.equal(helpers.selectedInstanceFromLocation({ search: '?instance=wan_sqm' }), 'wan_sqm');
assert.equal(helpers.selectedInstanceFromLocation({ search: '?foo=1&instance=wanb_sqm' }), 'wanb_sqm');
assert.equal(helpers.selectedInstanceFromLocation({ search: '?instance=wan%20sqm' }), null);
assert.equal(helpers.selectedInstanceFromLocation({ search: '?instance=../../etc/passwd' }), null);

assert(source.includes('Configure profile-specific outbound DSCP rules for this instance') &&
	source.includes('remains the only owner of SQM, CAKE, IFB devices and bandwidth rates'),
	'the per-instance ownership boundary must be explicit in LuCI');
assert(source.includes('Download packets reach the SQM IFB before these nftables hooks'),
	'LuCI must not claim that outbound nft rules classify download traffic');
assert(source.includes('traffic_defaults_gaming') &&
	source.includes('traffic_defaults_best_overall') &&
	source.includes('traffic_defaults_fair'),
	'each profile must have independently controllable built-in rules');
assert(source.includes("addFlag(s, 'traffic_rules_enabled', _('Outbound rules'), '0'"),
	'upgrades must not enable a new packet policy without explicit opt-in');
assert(source.includes('steam_realtime') && source.includes('xbox_live') &&
	source.includes('playstation'),
	'conservative game-platform presets must be available');
assert(source.includes("s.filter = function(sectionId)"),
	'custom rules must be filtered to the selected instance');
assert(source.includes("fs.exec('/usr/libexec/cake-autorate-rs/traffic-classifier', classifierArgs)"),
	'the nested page must request instance-scoped classifier status');

const menu = JSON.parse(fs.readFileSync(path.join(__dirname, '..', 'root', 'usr', 'share',
	'luci', 'menu.d', 'luci-app-cake-autorate-rs.json'), 'utf8'));
assert(!menu['admin/network/cake-autorate-rs/priorities'],
	'Traffic priorities must not remain a top-level tab');
assert(menu['admin/network/cake-autorate-rs/settings/priorities'],
	'Traffic priorities nested Settings route is missing');
assert.equal(menu['admin/network/cake-autorate-rs/settings/priorities'].title, undefined,
	'the nested route must not render another tab');

const settingsSource = fs.readFileSync(path.join(__dirname, '..', 'htdocs', 'luci-static',
	'resources', 'view', 'cake-autorate-rs', 'settings.js'), 'utf8');
assert(settingsSource.includes("_('Traffic priorities')"),
	'Settings rows must expose the per-instance Traffic priorities action');
assert(settingsSource.includes("L.url('admin/network/cake-autorate-rs/settings/priorities')"),
	'Settings must navigate to the nested instance-scoped route');

console.log('Traffic priorities tests passed');
