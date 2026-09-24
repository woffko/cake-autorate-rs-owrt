'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const packages = path.resolve(__dirname, '../..');
const native = fs.readFileSync(path.join(packages, 'cake-autorate-rs/Makefile'), 'utf8');
const version = /^PKG_VERSION:=(\S+)$/m.exec(native)[1];
const revision = /^PKG_RELEASE:=(\d+)$/m.exec(native)[1];
const full = /define Package\/cake-autorate-rs\n([\s\S]*?)\nendef/.exec(native)[1];
const lite = /define Package\/cake-autorate-rs-lite\n([\s\S]*?)\nendef/.exec(native)[1];
const defaults = /define Package\/cake-autorate-rs\/Default\n([\s\S]*?)\nendef/.exec(native)[1];
assert.match(full, /DEPENDS\+=.*\+speedtest-go(?:\s|$)/m);
assert.match(full, /^\s+EXTRA_DEPENDS:=speedtest-go \(>=1\.7\.10-r2\)$/m,
	'Full must reject the unbounded-request r1 backend');
assert.doesNotMatch(lite + defaults, /speedtest-go/,
	'Lite must not acquire the Full calibration backend dependency');
const backend = fs.readFileSync(path.join(packages, 'speedtest-go/Makefile'), 'utf8');
assert.match(backend, /^PKG_VERSION:=1\.7\.10$/m);
assert.match(backend, /^PKG_RELEASE:=5$/m);
assert.match(backend, /^GO_PKG_BUILD_PKG:=\$\(GO_PKG\)$/m,
	'the backend must ship only the speedtest-go CLI, not the upstream examples');
assert.ok(fs.existsSync(path.join(packages, 'speedtest-go/patches/120-upload-redirect-resolution.patch')));
assert.ok(fs.existsSync(path.join(packages, 'speedtest-go/patches/110-explicit-route-dns.patch')));
assert.ok(fs.existsSync(path.join(packages, 'speedtest-go/patches/100-bounded-request-failures.patch')));
for (const [ui, daemon] of [
	['luci-app-cake-autorate-rs', 'cake-autorate-rs-full'],
	['luci-app-cake-autorate-rs-lite', 'cake-autorate-rs-lite'],
]) {
	const makefile = fs.readFileSync(path.join(packages, ui, 'Makefile'), 'utf8');
	assert.ok(makefile.includes('EXTRA_DEPENDS:=' + daemon + ' (>=' + version + '-r' + revision + ')'),
		ui + ' must require the matching native protocol revision');
	assert.match(makefile, /^PKG_RELEASE:=\d+$/m);
}
console.log('Full/Lite package protocol dependencies passed');
