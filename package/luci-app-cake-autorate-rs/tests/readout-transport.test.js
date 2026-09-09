'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const root = path.resolve(__dirname, '../htdocs/luci-static/resources');
const uiSource = fs.readFileSync(path.join(root, 'cake-autorate-rs/ui.js'), 'utf8');
const statusSource = fs.readFileSync(path.join(root, 'view/cake-autorate-rs/status.js'), 'utf8');
const prefix = statusSource.slice(0, statusSource.indexOf('return L.view.extend'));
const { parseLogBundleReply } = new Function('L', '_', prefix + '\nreturn { parseLogBundleReply };')({}, value => value);
const LIMIT = 8 * 1024 * 1024;

function envelope(text, extra) {
	return JSON.stringify(Object.assign({ schema_version: 1, format: 'cake-autorate-log-bundle',
		byte_length: new Blob([text]).size, text }, extra));
}

const large = '✓\n'.repeat(100000);
assert(new Blob([large]).size > 256 * 1024);
assert.equal(parseLogBundleReply(envelope(large)), large);
for (const input of ['', 'plain legacy output', '{"schema_version":1', 'null', '[]',
	envelope('x', { schema_version: 2 }), envelope('x', { format: 'other' }),
	envelope('x', { byte_length: 0 }), envelope('x', { byte_length: 0.5 }),
	envelope('x', { byte_length: '1' }), envelope('x', { byte_length: 2 }),
	envelope('✓', { byte_length: 1 }), envelope('x', { text: null }),
	envelope('x'.repeat(LIMIT + 1))]) {
	assert.throws(() => parseLogBundleReply(input));
}
const exact = 'x'.repeat(LIMIT);
assert.equal(parseLogBundleReply(envelope(exact)).length, LIMIT);
assert.throws(() => parseLogBundleReply(envelope(large).slice(0, -1)), /incomplete or malformed/);

(async () => {
	const calls = [];
	let output = JSON.stringify({ job_id: 'a'.repeat(32), padding: 'x'.repeat(300 * 1024) });
	const backend = {
		exec() { throw new Error('large readout must not use rpcd exec'); },
		exec_direct(command, args, type) {
			calls.push({ command, args, type });
			return Promise.resolve(output);
		}
	};
	const ui = new Function('fs', 'L', '_', uiSource)(backend, { Class: { extend: methods => methods } }, value => value);
	for (const command of ['rating-result', 'speedtest-result', 'autotune-result']) {
		const args = ['--calibrationctl', command, 'a'.repeat(32)];
		const result = await ui.readNativeResult(args);
		assert.equal(result.job_id, 'a'.repeat(32));
		assert.equal(result.padding.length, 300 * 1024);
		assert.equal(Object.hasOwn(result, 'code'), false, 'no fabricated child exit status');
		assert.deepEqual(calls.at(-1), { command: '/usr/sbin/cake-autorated', args, type: 'text' });
	}
	const count = calls.length;
	for (const command of ['start', 'cancel', 'rating-start', 'autotune-start', 'autotune-apply-start',
		'autotune-apply-result', 'summary'])
		await assert.rejects(ui.readNativeResult(['--calibrationctl', command, 'a'.repeat(32)]));
	await assert.rejects(ui.readNativeResult(['--calibrationctl', 'autotune-result', '../unsafe']));
	await assert.rejects(ui.readNativeResult(['--calibrationctl', 'autotune-result', 'a'.repeat(32), 'extra']));
	assert.equal(calls.length, count, 'invalid/mutating commands must never reach CGI');
	for (const invalid of ['', '{"job_id":', 'null', '[]', 'true', '"text"',
		JSON.stringify({ padding: '✓'.repeat(200000) })]) {
		output = invalid;
		await assert.rejects(ui.readNativeResult(['--calibrationctl', 'autotune-result', 'a'.repeat(32)]));
	}
	console.log('large readout transport, complete JSON and byte-boundary tests passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
