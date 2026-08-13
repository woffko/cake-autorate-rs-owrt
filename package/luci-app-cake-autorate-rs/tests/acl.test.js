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
const rpcdHelperSource = fs.readFileSync(path.join(
	__dirname,
	'..',
	'root',
	'usr',
	'libexec',
	'cake-autorate-rs',
	'rpcd-helper',
), 'utf8');
const applyGuardSource = fs.readFileSync(path.join(
	__dirname,
	'..',
	'root',
	'usr',
	'libexec',
	'cake-autorate-rs',
	'apply-guard',
), 'utf8');
const group = document['luci-app-cake-autorate-rs'];

assert(group, 'CAKE Autorate ACL group is missing');
assert.deepStrictEqual(
	group.read.uci,
	[ 'cake-autorate', 'sqm', 'mwan3', 'network' ],
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
	group.read.file['/usr/libexec/cake-autorate-rs/runtime-health'],
	[ 'exec' ],
	'Status must have read-only execution access to the runtime reconciliation helper',
);
assert.deepStrictEqual(
	group.read.file['/usr/libexec/cake-autorate-rs/autotune-scheduler status *'],
	[ 'exec' ],
	'Status must be able to read scheduler state and traffic budgets without write access',
);
assert.deepStrictEqual(
	group.read.file['/usr/libexec/cake-autorate-rs/traffic-classifier status'],
	[ 'exec' ],
	'Traffic priorities must retain read-only global classifier diagnostics',
);
assert.deepStrictEqual(
	group.read.file['/usr/libexec/cake-autorate-rs/traffic-classifier status *'],
	[ 'exec' ],
	'Traffic priorities must only receive read-only access to instance-scoped classifier status',
);
for (const command of [
	'/usr/sbin/cake-autorated --calibrationctl summary',
	'/usr/sbin/cake-autorated --calibrationctl rating-current *',
	'/usr/sbin/cake-autorated --calibrationctl rating-status *',
	'/usr/sbin/cake-autorated --calibrationctl rating-result *',
	'/usr/sbin/cake-autorated --calibrationctl speedtest-current *',
	'/usr/sbin/cake-autorated --calibrationctl speedtest-status *',
	'/usr/sbin/cake-autorated --calibrationctl speedtest-result *',
	'/usr/sbin/cake-autorated --calibrationctl autotune-status *',
	'/usr/sbin/cake-autorated --calibrationctl autotune-result *',
	'/usr/sbin/cake-autorated --calibrationctl autotune-apply-check *',
	'/usr/sbin/cake-autorated --calibrationctl scheduler-status',
]) {
	assert.deepStrictEqual(group.read.file[command], [ 'exec' ],
		`Native read-only transport ACL is missing: ${command}`);
}
for (const command of [
	'/usr/sbin/cake-autorated --calibrationctl autotune-start *',
	'/usr/sbin/cake-autorated --calibrationctl autotune-bootstrap-start *',
	'/usr/sbin/cake-autorated --calibrationctl autotune-cancel *',
	'/usr/sbin/cake-autorated --calibrationctl autotune-apply *',
	'/usr/sbin/cake-autorated --calibrationctl rating-start *',
	'/usr/sbin/cake-autorated --calibrationctl rating-cancel *',
	'/usr/sbin/cake-autorated --calibrationctl speedtest-start *',
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
assert.equal(group.write.file['/usr/libexec/cake-autorate-rs/quality-test *'], undefined,
	'LuCI must not retain a competing shell Rating mutation path after native cutover');
const rpcdCommands = [
	'/usr/libexec/cake-autorate-rs/rpcd-helper speedtest-job-start *',
	'/usr/libexec/cake-autorate-rs/rpcd-helper speedtest-job-status *',
	'/usr/libexec/cake-autorate-rs/rpcd-helper speedtest-status *',
	'/usr/libexec/cake-autorate-rs/rpcd-helper speedtest-install *',
	'/usr/libexec/cake-autorate-rs/rpcd-helper autotune-start *',
	'/usr/libexec/cake-autorate-rs/rpcd-helper autotune-start-conservative *',
	'/usr/libexec/cake-autorate-rs/rpcd-helper autotune-status-summary *',
	'/usr/libexec/cake-autorate-rs/rpcd-helper autotune-result *',
	'/usr/libexec/cake-autorate-rs/rpcd-helper autotune-cancel *',
	'/usr/libexec/cake-autorate-rs/rpcd-helper autotune-status *',
	'/usr/libexec/cake-autorate-rs/rpcd-helper autotune-attest *',
	'/usr/libexec/cake-autorate-rs/rpcd-helper apply-guard-arm *',
	'/usr/libexec/cake-autorate-rs/rpcd-helper apply-guard-abort *',
	'/usr/libexec/cake-autorate-rs/rpcd-helper apply-guard-verify-rollback *',
	'/usr/libexec/cake-autorate-rs/rpcd-helper apply-guard-finalize *',
	'/usr/libexec/cake-autorate-rs/rpcd-helper apply-guard-reconcile *',
	'/usr/libexec/cake-autorate-rs/rpcd-helper apply-guard-status *',
];
for (const command of rpcdCommands) {
	assert.deepStrictEqual(group.write.file[command], [ 'exec' ],
		`Required gradual-cutover helper verb is missing: ${command}`);
	const operation = command.split(' ')[1];
	assert.match(settingsSource, new RegExp(`["']${operation}["']`),
		`ACL operation has no LuCI caller: ${operation}`);
}
const aclOperations = rpcdCommands.map(command => command.split(' ')[1]).sort();
const dispatcherOperations = Array.from(rpcdHelperSource.matchAll(
	/^\t([a-z][a-z0-9-]*)\)\n/gm,
), match => match[1]).sort();
assert.deepStrictEqual(dispatcherOperations, aclOperations,
	'every dispatcher operation must have exactly one matching ACL entry and vice versa');
const expectedApplyGuardOperations = [
	'apply-guard-abort',
	'apply-guard-arm',
	'apply-guard-finalize',
	'apply-guard-reconcile',
	'apply-guard-status',
	'apply-guard-verify-rollback',
];
assert.deepStrictEqual(
	aclOperations.filter(operation => operation.startsWith('apply-guard-')),
	expectedApplyGuardOperations,
	'ACL must expose exactly the six browser-owned Apply Guard operations',
);
assert.deepStrictEqual(
	dispatcherOperations.filter(operation => operation.startsWith('apply-guard-')),
	expectedApplyGuardOperations,
	'dispatcher must expose exactly the six browser-owned Apply Guard operations',
);
const dispatcherApplyBackends = rpcdHelperSource.match(
	/case "\$\{3:-\}" in ([A-Za-z0-9|-]+)\)/,
);
const guardApplyBackends = applyGuardSource.match(
	/safe_backend\(\) \{\s*case "\$1" in ([A-Za-z0-9|-]+)\)/,
);
assert(dispatcherApplyBackends && guardApplyBackends,
	'Apply Guard backend allowlists must remain structurally visible');
assert.equal(dispatcherApplyBackends[1], guardApplyBackends[1],
	'rpcd dispatcher and Apply Guard must accept the same backend set');
assert.equal(group.write.file['/usr/libexec/cake-autorate-rs/speedtest *'], undefined,
	'Legacy Speed Test must not expose worker, recovery, or arbitrary helper verbs');
assert.equal(group.write.file['/usr/libexec/cake-autorate-rs/autotune *'], undefined,
	'Legacy Auto-Tune must not expose worker, recovery, or arbitrary helper verbs');
assert.equal(group.write.file['/usr/libexec/cake-autorate-rs/apply-guard *'], undefined,
	'LuCI must not expose internal Apply Guard recovery and supervisor verbs');
assert.equal(group.write.file['/usr/libexec/cake-autorate-rs/rpcd-helper *'], undefined,
	'The rpcd dispatcher operation must be pinned before any wildcard');
assert.deepStrictEqual(
	Object.keys(group.write.file).filter((command) =>
		command.startsWith('/usr/libexec/cake-autorate-rs/rpcd-helper ')).sort(),
	rpcdCommands.slice().sort(),
	'Only reviewed and positionally pinned migration operations may remain reachable through rpcd',
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

console.log('ACL tests passed');
