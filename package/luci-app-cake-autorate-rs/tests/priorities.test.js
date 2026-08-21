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
		`canonicalTrafficProfile, configuredTrafficProfile, resolvedTrafficProfile, ` +
		`effectiveRuleProfile, parseClassifierStatus, parsePresetCatalog, safeInstanceName, selectedInstanceFromLocation };`
)({}, {}, {}, {}, {}, {}, () => {}, value => value);

assert.equal(helpers.canonicalProfile('balanced'), 'best_overall');
assert.equal(helpers.canonicalProfile('gaming'), 'gaming');
assert.equal(helpers.canonicalProfile('invalid'), null);
assert.equal(helpers.canonicalTrafficProfile('auto'), 'auto');
assert.equal(helpers.canonicalTrafficProfile('custom'), 'custom');
assert.equal(helpers.canonicalTrafficProfile('invalid'), null);
assert.equal(helpers.configuredTrafficProfile({ autotune_profile: 'gaming' }), 'auto');
assert.equal(helpers.configuredTrafficProfile({ autotune_profile: 'gaming', traffic_defaults_gaming: '0' }), 'auto');
assert.equal(helpers.configuredTrafficProfile({ traffic_profile: 'custom' }), 'custom');
assert.equal(helpers.resolvedTrafficProfile('auto', 'fair'), 'fair');
assert.equal(helpers.effectiveRuleProfile(undefined), 'custom');
assert.equal(helpers.effectiveRuleProfile('gaming'), 'gaming');
assert.equal(helpers.effectiveRuleProfile('auto'), 'custom');

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
assert.equal(helpers.parsePresetCatalog({ stdout: '{"schema_version":1,"profiles":{"gaming":[]}}' }).schema_version, 1);
assert.equal(helpers.parsePresetCatalog({ stdout: 'not-json' }).schema_version, 0);
assert.equal(helpers.selectedInstanceFromLocation({ search: '?instance=wan_sqm' }), 'wan_sqm');
assert.equal(helpers.selectedInstanceFromLocation({ search: '?foo=1&instance=wanb_sqm' }), 'wanb_sqm');
assert.equal(helpers.selectedInstanceFromLocation({ search: '?instance=wan%20sqm' }), null);
assert.equal(helpers.selectedInstanceFromLocation({ search: '?instance=../../etc/passwd' }), null);

assert(source.includes('Configure profile-specific outbound DSCP rules for this instance') &&
	source.includes('remains the only owner of SQM, CAKE, IFB devices and bandwidth rates'),
	'the per-instance ownership boundary must be explicit in LuCI');
assert(source.includes('Download packets reach the SQM IFB before these nftables hooks'),
	'LuCI must not claim that outbound nft rules classify download traffic');
for (const match of source.matchAll(/o\.depends\('preset', 'custom'\);([\s\S]{0,100})/g))
	assert.match(match[1], /o\.retain = true;/,
		'custom rule values hidden by a preset must survive modal saves');
for (const [index, button] of source.split("E('button', {").slice(1).entries()) {
	assert.match(button.slice(0, 160), /'type': 'button'/,
		`custom priorities button ${index + 1} must not submit the surrounding LuCI form`);
}
assert(source.includes("s.option(form.ListValue, 'traffic_profile'") &&
	source.includes("o.widget = 'radio'") && source.includes("o.value('auto'") &&
	source.includes("o.value('custom'"),
	'traffic profiles must be one exclusive radio-card selection');
assert(!source.includes('traffic_profile_migrated'),
	'the current profile editor must not recreate the retired migration marker');
assert(source.includes("addFlag(s, 'traffic_rules_enabled', _('Enable outbound traffic prioritization'), '0'"),
	'upgrades must not enable a new packet policy without explicit opt-in');
assert(source.includes("fs.exec('/usr/sbin/cake-autorated', [ '--traffic-classifier', 'presets' ])"),
	'LuCI must obtain its preview from the backend rule catalog');
assert(source.includes("uci.add('cake-autorate', 'traffic_rule')") &&
	source.includes("uci.set('cake-autorate', sectionId, 'profile', 'custom')") &&
	source.includes("effectiveRuleProfile(rule.profile) === 'custom'") &&
	source.includes('Future package upgrades will not overwrite the copy'),
	'Customize must stage an independent UCI copy without duplicating legacy Custom rules');
assert(source.includes("s.filter = function(sectionId)"),
	'custom rules must be filtered to the selected instance');
assert(source.includes("fs.exec('/usr/sbin/cake-autorated', classifierArgs)") &&
	source.includes("var classifierArgs = [ '--traffic-classifier', 'status' ]"),
	'the nested page must request instance-scoped classifier status');
assert(source.includes("'data-label': trafficLabel") &&
	source.includes('.traffic-profile-rule-table td:before{content:attr(data-label)') &&
	source.includes('.traffic-profile-rule-table thead{display:none}'),
	'the preset preview must remain readable as labelled cards on narrow screens');
assert.match(source,
	/handleReset: function\(ev\)[\s\S]*?discardPrioritiesStagedUci\(\)[\s\S]*?window\.location\.replace/,
	'Traffic priorities Reset must revert its package-scoped RPC session before reload');
assert.match(source,
	/function discardPrioritiesStagedUci\(\)[\s\S]*?method: 'revert'[\s\S]*?callRevert\('cake-autorate'\)[\s\S]*?uci\.unload\('cake-autorate'\)/,
	'Traffic priorities rollback must revert and unload only cake-autorate');

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
