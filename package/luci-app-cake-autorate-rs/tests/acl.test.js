'use strict';

const assert = require('assert');
const fs = require('fs');
const path = require('path');

const aclPath = path.join(
	__dirname,
	'..',
	'root',
	'usr',
	'share',
	'rpcd',
	'acl.d',
	'luci-app-cake-autorate-rs.json',
);
const document = JSON.parse(fs.readFileSync(aclPath, 'utf8'));
const settingsSource = fs.readFileSync(path.join(
	__dirname,
	'..',
	'htdocs',
	'luci-static',
	'resources',
	'view',
	'cake-autorate-rs',
	'settings.js',
), 'utf8');
const retiredRpcdHelper = path.join(
	__dirname,
	'..',
	'root',
	'usr',
	'libexec',
	'cake-autorate-rs',
	'rpcd-helper',
);
const group = document['luci-app-cake-autorate-rs'];

assert(group, 'CAKE Autorate ACL group is missing');
assert.deepStrictEqual(
	group.read.uci,
	[ 'cake-autorate', 'sqm', 'mwan3', 'network', 'cake-autorate-ui' ],
	'Settings must be able to read logical/device mappings from UCI network',
);
assert.deepStrictEqual(
	group.write.uci,
	[ 'cake-autorate', 'sqm' ],
	'The app must not receive UCI write access to network or mwan3',
);
assert.deepStrictEqual(
	group.write.ubus.uci,
	[ 'revert' ],
	'Guarded apply reconciliation needs per-package revert while the write.uci scope confines it to cake-autorate and sqm',
);
assert.deepStrictEqual(
	group.read.file['/usr/sbin/cake-autorated --runtime-health'],
	[ 'exec' ],
	'Status must have exact read-only execution access to native runtime reconciliation',
);
assert.equal(group.read.file['/usr/libexec/cake-autorate-rs/runtime-health'], undefined,
	'the retired shell runtime-health helper must have no browser ACL authority');
for (const command of [
	'/usr/sbin/cake-autorated --package-versions',
	'/usr/sbin/cake-autorated --mwan3-info',
	'/usr/sbin/cake-autorated --graph-history *',
	'/usr/sbin/cake-autorated --log-bundle *',
])
	assert.deepStrictEqual(group.read.file[command], [ 'exec' ],
		`native LuCI readout ACL is missing: ${command}`);
for (const command of [
	'/usr/libexec/cake-autorate-rs/package-versions',
	'/usr/libexec/cake-autorate-rs/mwan3-info',
	'/usr/libexec/cake-autorate-rs/graph-history *',
	'/usr/libexec/cake-autorate-rs/log-bundle *',
]) {
	assert.equal(group.read.file[command], undefined,
		`retired LuCI readout retains read authority: ${command}`);
	assert.equal(group.write.file[command], undefined,
		`retired LuCI readout retains write authority: ${command}`);
}
assert.equal(group.write.file['/usr/libexec/cake-autorate-rs/status-columns *'], undefined,
	'the retired Status column helper must not retain root-exec authority');
assert.deepStrictEqual(group.write.file['/usr/sbin/cake-autorated --status-columns *'],
	[ 'exec' ], 'Status columns must use the native bounded commit endpoint');
assert.deepStrictEqual(group.read.file['/usr/sbin/cake-autorated --mqtt-status *'],
	[ 'exec' ], 'MQTT readiness must use the native read-only endpoint');
assert.equal(group.write.file['/usr/sbin/cake-autorated --mqtt-status *'], undefined,
	'MQTT readiness no longer needs package-install or write authority');
assert.equal(group.write.file['/usr/libexec/cake-autorate-rs/mqtt-status *'], undefined,
	'the retired MQTT shell helper must not retain root-exec authority');
for (const command of Object.keys(group.write.file).concat(Object.keys(group.read.file)))
	assert.doesNotMatch(command, /cake-autorate-mqtt/,
		'the retired second MQTT init service must have no browser ACL authority');
assert.equal(group.read.file['/usr/libexec/cake-autorate-rs/autotune-scheduler status *'],
	undefined, 'the retired shell scheduler must have no browser ACL authority');
assert.deepStrictEqual(group.write.file['/usr/sbin/cake-autorated --pinger-plan *'],
	[ 'exec' ], 'the pinger wizard must use the native bounded planner');
assert.equal(group.write.file['/usr/libexec/cake-autorate-rs/pinger-plan *'], undefined,
	'the retired shell pinger planner must have no browser ACL authority');
assert.deepStrictEqual(
	group.read.file['/usr/sbin/cake-autorated --traffic-classifier status'],
	[ 'exec' ],
	'Traffic priorities must retain read-only global classifier diagnostics',
);
assert.deepStrictEqual(
	group.read.file['/usr/sbin/cake-autorated --traffic-classifier status *'],
	[ 'exec' ],
	'Traffic priorities must only receive read-only access to instance-scoped classifier status',
);
for (const retired of [
	'/usr/libexec/cake-autorate-rs/traffic-classifier presets',
	'/usr/libexec/cake-autorate-rs/traffic-classifier status',
	'/usr/libexec/cake-autorate-rs/traffic-classifier status *',
])
	assert.equal(group.read.file[retired], undefined,
		'the retired shell classifier must have no browser ACL authority');
for (const command of [
	'/usr/sbin/cake-autorated --calibrationctl summary',
	'/usr/sbin/cake-autorated --calibrationctl rating-current *',
	'/usr/sbin/cake-autorated --calibrationctl rating-status *',
	'/usr/sbin/cake-autorated --calibrationctl rating-result *',
	'/usr/sbin/cake-autorated --calibrationctl speedtest-current *',
	'/usr/sbin/cake-autorated --calibrationctl speedtest-status *',
	'/usr/sbin/cake-autorated --calibrationctl speedtest-result *',
	'/usr/sbin/cake-autorated --calibrationctl autotune-current *',
	'/usr/sbin/cake-autorated --calibrationctl autotune-status *',
	'/usr/sbin/cake-autorated --calibrationctl autotune-result *',
	'/usr/sbin/cake-autorated --calibrationctl autotune-apply-check *',
	'/usr/sbin/cake-autorated --calibrationctl autotune-apply-status *',
	'/usr/sbin/cake-autorated --calibrationctl autotune-apply-watch *',
	'/usr/sbin/cake-autorated --calibrationctl autotune-apply-result *',
	'/usr/sbin/cake-autorated --calibrationctl scheduler-status',
]) {
	assert.deepStrictEqual(group.read.file[command], [ 'exec' ],
		`Native read-only transport ACL is missing: ${command}`);
}
for (const command of [
	'/usr/sbin/cake-autorated --calibrationctl autotune-start *',
	'/usr/sbin/cake-autorated --calibrationctl autotune-bootstrap-start *',
	'/usr/sbin/cake-autorated --calibrationctl autotune-cancel *',
	'/usr/sbin/cake-autorated --calibrationctl autotune-apply-start *',
	'/usr/sbin/cake-autorated --calibrationctl rating-start *',
	'/usr/sbin/cake-autorated --calibrationctl rating-cancel *',
	'/usr/sbin/cake-autorated --calibrationctl speedtest-start *',
	'/usr/sbin/cake-autorated --calibrationctl speedtest-bootstrap-start *',
	'/usr/sbin/cake-autorated --calibrationctl speedtest-cancel *',
	'/usr/sbin/cake-autorated --calibrationctl scheduler-acknowledge-accounting *',
]) {
	assert.deepStrictEqual(group.write.file[command], [ 'exec' ],
		`Native mutating transport ACL is missing: ${command}`);
}
assert.equal(group.read.file['/usr/sbin/cake-autorated --calibrationctl *'], undefined,
	'Native coordinator read access must never use a broad calibrationctl wildcard');
assert.equal(group.read.file['/usr/sbin/cake-autorated --calibrationctl scheduler-status *'], undefined,
	'Native scheduler status is one exact zero-argument batch read, never an instance wildcard');
assert.equal(group.write.file['/usr/sbin/cake-autorated --calibrationctl *'], undefined,
	'Native coordinator write access must never use a broad calibrationctl wildcard');
assert.equal(group.write.file['/usr/sbin/cake-autorated --calibrationctl autotune-apply *'], undefined,
	'the retired synchronous Apply command must have no browser mutation authority');
assert.equal(group.write.file['/usr/libexec/cake-autorate-rs/quality-test *'], undefined,
	'LuCI must not retain a competing shell Rating mutation path after native cutover');
assert.equal(fs.existsSync(retiredRpcdHelper), false,
	'the retired one-operation rpcd shell dispatcher must not remain shipped');
assert.equal(group.write.file['/usr/libexec/cake-autorate-rs/speedtest *'], undefined,
	'Legacy Speed Test must not expose worker, recovery, or arbitrary helper verbs');
assert.equal(group.write.file['/usr/libexec/cake-autorate-rs/autotune *'], undefined,
	'Legacy Auto-Tune must not expose worker, recovery, or arbitrary helper verbs');
assert.equal(group.write.file['/usr/libexec/cake-autorate-rs/apply-guard *'], undefined,
	'LuCI must not expose internal Apply Guard recovery and supervisor verbs');
assert.equal(group.write.file['/usr/libexec/cake-autorate-rs/rpcd-helper *'], undefined,
	'the retired rpcd dispatcher must not retain wildcard authority');
assert.deepStrictEqual(
	Object.keys(group.write.file).filter((command) =>
		command.startsWith('/usr/libexec/cake-autorate-rs/rpcd-helper ')).sort(),
	[],
	'no retired shell migration operation may remain reachable through rpcd',
);
assert.deepStrictEqual(
	Object.keys(group.write.file).filter(command =>
		/^\/usr\/libexec\/cake-autorate-rs\/(?:speedtest|autotune|quality-test|apply-guard)(?: |$)/.test(command)),
	[],
	'LuCI must never execute the large legacy helpers directly through rpcd',
);
assert.doesNotMatch(settingsSource,
	/fs\.exec\(['"]\/usr\/libexec\/cake-autorate-rs\/(?:speedtest|autotune|apply-guard)['"]/,
	'LuCI must not bypass the positionally pinned rpcd dispatcher');
assert.match(settingsSource, /fs\.exec\('\/usr\/sbin\/cake-autorated', \[\n\s*'--pinger-plan'/,
	'LuCI must call the native pinger planner directly');
assert.doesNotMatch(settingsSource, /\/usr\/libexec\/cake-autorate-rs\/pinger-plan/,
	'LuCI must not retain the retired shell pinger planner path');

console.log('ACL tests passed');
