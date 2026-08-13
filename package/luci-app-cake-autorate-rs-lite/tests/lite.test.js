'use strict';

const assert = require('assert');
const fs = require('fs');
const path = require('path');

const root = path.resolve(__dirname, '..');
const settings = fs.readFileSync(path.join(root,
	'htdocs/luci-static/resources/view/cake-autorate-rs/settings.js'), 'utf8');
const status = fs.readFileSync(path.join(root,
	'htdocs/luci-static/resources/view/cake-autorate-rs/status.js'), 'utf8');
const acl = JSON.parse(fs.readFileSync(path.join(root,
	'root/usr/share/rpcd/acl.d/luci-app-cake-autorate-rs-lite.json'), 'utf8'));
const menu = JSON.parse(fs.readFileSync(path.join(root,
	'root/usr/share/luci/menu.d/luci-app-cake-autorate-rs.json'), 'utf8'));
const makefile = fs.readFileSync(path.join(root, 'Makefile'), 'utf8');
const uiSource = [ settings, status, JSON.stringify(acl), JSON.stringify(menu) ].join('\n');

assert.match(makefile, /\+cake-autorate-rs-lite/);
assert.match(makefile, /PROVIDES:=luci-app-cake-autorate-rs luci-app-sqm/);
assert.doesNotMatch(makefile, /uclient-fetch|jsonfilter|nftables-json/);
assert.match(makefile, /cake-autorate-autotune disable/);
assert.match(makefile, /cake-autorate-apply-guard disable/);
assert.match(makefile, /rm -f \/etc\/rc\.d\/S\*cake-autorate-autotune \/etc\/rc\.d\/K\*cake-autorate-autotune/);
assert.match(makefile, /rm -f \/etc\/rc\.d\/S\*cake-autorate-apply-guard \/etc\/rc\.d\/K\*cake-autorate-apply-guard/);

assert.deepEqual(Object.keys(menu).sort(), [
	'admin/network/cake-autorate-rs',
	'admin/network/cake-autorate-rs/settings',
	'admin/network/cake-autorate-rs/status'
]);
assert.deepEqual(Object.keys(acl['luci-app-cake-autorate-rs-lite'].write.file || {}), []);
assert.deepEqual(acl['luci-app-cake-autorate-rs-lite'].write.uci.sort(), [ 'cake-autorate', 'sqm' ]);
assert.match(settings, /form\.GridSection/);
assert.match(settings, /sqm_direction_mode/);
assert.match(settings, /adaptive_ceiling_enabled/);
assert.match(status, /cake_dl_rate_kbps/);
assert.match(status, /cake_ul_rate_kbps/);

[
	'calibrationctl', 'rpcd-helper', 'autotune-start', 'rating-start', 'speedtest-start',
	'autotune-scheduler', 'apply-guard', 'graph-history', 'runtime-health'
].forEach((forbidden) => assert.doesNotMatch(uiSource, new RegExp(forbidden, 'i')));

console.log('lite LuCI package tests passed');
