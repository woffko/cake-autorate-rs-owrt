'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const sourcePath = path.join(__dirname, '..', 'htdocs', 'luci-static', 'resources',
	'view', 'cake-autorate-rs', 'priorities.js');
const source = fs.readFileSync(sourcePath, 'utf8');
const prefix = source.slice(0, source.indexOf('return L.view.extend'));
const helpers = new Function('fs', 'form', 'uci', 'ui', 'cakeUi', 'L', 'E', '_',
	`${prefix}\nreturn { canonicalProfile, validatePortList, validateNetwork, profileLabel, parseClassifierStatus };`
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

assert(source.includes('Create and modify profile-specific DSCP rules without qosify or eBPF'),
	'the ownership boundary must be explicit in LuCI');
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

const menu = JSON.parse(fs.readFileSync(path.join(__dirname, '..', 'root', 'usr', 'share',
	'luci', 'menu.d', 'luci-app-cake-autorate-rs.json'), 'utf8'));
assert(menu['admin/network/cake-autorate-rs/priorities'],
	'Traffic priorities page is missing from the LuCI menu');

console.log('Traffic priorities tests passed');
