'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const source = fs.readFileSync(path.join(__dirname, '../htdocs/luci-static/resources/view/cake-autorate-rs/status.js'), 'utf8');
const prefix = source.slice(0, source.indexOf('return L.view.extend'));
const uiSource = fs.readFileSync(path.join(__dirname, '../htdocs/luci-static/resources/cake-autorate-rs/ui.js'), 'utf8');
const cakeUi = new Function('document', 'L', uiSource)(
	{ createTextNode: value => ({ nodeType: 3, textContent: value }) },
	{ Class: { extend: methods => methods } });

function fixture(exec) {
	const notices = [];
	const api = new Function('fs', 'poll', 'uci', 'ui', 'cakeUi', 'L', 'E', '_',
		prefix + '\nreturn { serviceAction };')(
		{ exec }, {}, {}, { addNotification: (_, node, kind) => notices.push({ node, kind }) }, cakeUi, {},
		(tag, attrs, children) => {
			const content = children === undefined ? attrs : children;
			return { tag, content, text: content && content.nodeType === 3 ? content.textContent : content };
		}, value => value);
	return { api, notices };
}

(async function() {
	for (const result of [{ code: 1, stderr: 'start failed' }, { code: 1 }, null, {}, { code: '0' }]) {
		const f = fixture(async () => result);
		let refreshes = 0;
		await f.api.serviceAction('restart', () => { refreshes++; });
		assert(!f.notices.some(n => n.node.text === 'Service action completed.'), 'failure must never display success');
		assert(f.notices.some(n => n.kind === 'error'));
		assert.equal(refreshes, 0);
	}
	const success = fixture(async () => ({ code: 0 }));
	let refreshed = 0;
	assert.equal(await success.api.serviceAction('start', () => { refreshed++; }), true);
	assert.equal(refreshed, 1);
	const transport = fixture(async () => { throw new Error('RPC disconnected'); });
	assert.equal(await transport.api.serviceAction('stop'), false);
	assert(transport.notices[0].node.text.includes('RPC disconnected'));
	const statusError = fixture(async () => ({ code: 0 }));
	assert.equal(await statusError.api.serviceAction('restart', () => { throw new Error('status unavailable'); }), true);
	assert(statusError.notices.some(n => n.kind === 'warning'));
	let resolve, calls = 0;
	const duplicate = fixture(() => { calls++; return new Promise(done => { resolve = done; }); });
	const first = duplicate.api.serviceAction('restart');
	const second = duplicate.api.serviceAction('restart');
	assert.equal(await duplicate.api.serviceAction('stop'), false,
		'a different action must not silently inherit the previous action result');
	assert(duplicate.notices.some(n => n.kind === 'warning'));
	await Promise.resolve();
	assert.equal(calls, 1);
	resolve({ code: 0 });
	await Promise.all([first, second]);
	const third = duplicate.api.serviceAction('stop');
	await Promise.resolve();
	assert.equal(calls, 2);
	resolve({ code: 0 });
	await third;
	const html = fixture(async () => ({ code: 1, stderr: '<script>payload</script>' + 'x'.repeat(2000) }));
	await html.api.serviceAction('restart');
	assert.equal(html.notices[0].node.tag, 'p');
	assert.equal(html.notices[0].node.content.nodeType, 3,
		'backend error must reach LuCI E as an actual text node, never an HTML string');
	assert(html.notices[0].node.text.length < 700);
	const secret = fixture(async () => ({ code: 1, stderr: 'password=synthetic-secret' }));
	await secret.api.serviceAction('restart');
	assert(!secret.notices[0].node.text.includes('synthetic-secret'));
	console.log('service action behavior tests passed');
})().catch(error => { console.error(error); process.exitCode = 1; });
