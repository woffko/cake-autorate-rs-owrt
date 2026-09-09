'use strict';
// Run with the locally installed Playwright module/browser paths as optional
// arguments. No router, credentials, test traffic or network access is needed.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require(process.argv[2] || 'playwright');
const root = path.resolve(__dirname, '../htdocs/luci-static/resources');
const sources = Object.fromEntries([
	['ui', 'cake-autorate-rs/ui.js'],
	['status', 'view/cake-autorate-rs/status.js'],
	['priorities', 'view/cake-autorate-rs/priorities.js'],
	['settings', 'view/cake-autorate-rs/settings.js']
].map(([key, name]) => [key, fs.readFileSync(path.join(root, name), 'utf8')]));
// Optional frozen Status source makes the same deterministic regression fail
// on the pre-fix implementation, while retaining the tested text helper.
if (process.argv[4]) sources.status = fs.readFileSync(process.argv[4], 'utf8');
sources.lite = fs.readFileSync(path.resolve(__dirname,
	'../../luci-app-cake-autorate-rs-lite/htdocs/luci-static/resources/view/cake-autorate-rs/settings.js'), 'utf8');
sources.liteStatus = fs.readFileSync(path.resolve(__dirname,
	'../../luci-app-cake-autorate-rs-lite/htdocs/luci-static/resources/view/cake-autorate-rs/status.js'), 'utf8');

(async () => {
	const browser = await chromium.launch({ headless: true, executablePath: process.argv[3] });
	try {
		const page = await browser.newPage();
		// Abort every attempted resource request. The fixture never reaches any
		// host; DOM structure, not request timing or an execution flag, is proof.
		await page.route('**/*', route => route.abort());
		const result = await page.evaluate(async source => {
			String.prototype.format = function() {
				let i = 0; const args = arguments;
				return this.replace(/%%|%[sd]/g, value => value === '%%' ? '%' : String(args[i++]));
			};
			// Deliberately model LuCI's unsafe string-child boundary using the
			// real browser HTML parser. A positive control proves it is exercised.
			function E(tag, attrs, children) {
				if (arguments.length === 2 && (typeof attrs !== 'object' || attrs instanceof Node || Array.isArray(attrs))) {
					children = attrs; attrs = {};
				}
				const node = document.createElement(tag);
				for (const [key, value] of Object.entries(attrs || {})) {
					if (typeof value === 'function') node.addEventListener(key, value);
					else if (value != null) node.setAttribute(key, String(value));
				}
				function append(value) {
					if (value == null) return;
					// Match LuCI dom.append: array entries are nodes or coerced
					// text (not recursively parsed); only scalar strings are HTML.
					if (Array.isArray(value)) value.forEach(item => node.appendChild(
						item instanceof Node ? item : document.createTextNode(String(item))));
					else if (value instanceof Node) node.appendChild(value);
					else node.insertAdjacentHTML('beforeend', String(value));
				}
				append(children);
				return node;
			}
			const payload = '<img src="http://127.0.0.1:9/cake-injection-probe" onerror="window.__audit=true"> <svg onload="window.__audit=true"></svg> & Unicode ✓';
			const unsafe = E('p', payload);
			if (unsafe.querySelectorAll('img, svg').length !== 2) throw new Error('unsafe positive control not exercised');
			const cakeUi = new Function('document', 'L', 'E', source.ui)(document, { Class: { extend: methods => methods } }, E);
			const notices = [];
			let notify;
			const ui = {
				addNotification: (_, node) => { notices.push(node); if (notify) notify(node); },
				createHandlerFn: (_, callback) => callback,
			};
			let mode = 'service';
			const backend = { exec: () => Promise.resolve({ code: 1, stderr: payload }),
				exec_direct: () => Promise.reject(new Error(payload)) };
			const prefix = text => text.slice(0, text.indexOf('return L.view.extend'));
			const status = new Function('fs', 'poll', 'uci', 'ui', 'cakeUi', 'L', 'E', '_',
				prefix(source.status) + '\nreturn { serviceAction, exportLogs, renderColumnChooser, formatServices, formatState, formatRoute, renderStatusData };')(
				backend, {}, {}, ui, cakeUi, {}, E, value => value);
			const cases = [];
			function verify(name, node, text) {
				if (node.querySelector('img, svg, script, iframe, object, embed')) throw new Error(name + ': parsed payload markup');
				if ([node, ...node.querySelectorAll('*')].some(n => [...n.attributes].some(a => /^on/i.test(a.name))))
					throw new Error(name + ': payload event attribute');
				if (!node.textContent.includes(text)) throw new Error(name + ': lost literal text');
				cases.push(name);
			}
			verify('text-node-helper', E('p', {}, cakeUi.text(payload)), payload);
			const rich = E('strong', {}, 'trusted layout');
			const mixed = cakeUi.textElement('p', {}, [rich, [payload, 0, false, null, undefined]]);
			verify('plain-display-preserves-nodes', mixed, payload);
			if (mixed.querySelector('strong') !== rich || !mixed.textContent.endsWith('0'))
				throw new Error('plain display changed a trusted node or numeric zero');
			if (cakeUi.textElement('p', null).childNodes.length !== 0)
				throw new Error('empty display rendered a null child');
			verify('plain-display-two-argument-overload', cakeUi.textElement('p', payload), payload);
			await status.serviceAction('restart');
			verify('service-error', notices.pop(), payload);
			mode = 'export';
			await status.exportLogs({ currentTarget: document.createElement('button') });
			verify('export-error', notices.pop(), payload);
			mode = 'columns';
			const picker = status.renderColumnChooser({}, ['instance', 'uplink', 'services', 'quality', 'rating'], () => {});
			const notification = new Promise(resolve => { notify = resolve; });
			picker.querySelector('button').click();
			verify('column-save-error', await notification, payload);
			notify = null;
			const settings = new Function('fs', 'form', 'network', 'uci', 'ui', 'widgets', 'cakeUi', 'rpc', 'L', 'E', '_',
				prefix(source.settings) + '\nreturn { renderAutotuneDiagnostics, renderNativeAutotuneDiagnostics, settingsActionLayout: typeof settingsActionLayout === "function" ? settingsActionLayout : null };')(
				{}, {}, {}, {}, ui, {}, cakeUi, { declare: () => () => Promise.resolve(0) }, {}, E, value => value);
			verify('autotune-diagnostic-and-code', settings.renderAutotuneDiagnostics({ error: payload, reason: payload }), payload);
			const proposal = {
				native_job_id: 'a'.repeat(32), artifacts: {},
				public_apply_contract: { options: [{ option_id: 'recommended', preferred: true,
					selected_topology: 'both_shaped', required_acknowledgements: [payload],
					target_rates_kbps: { download: 1000, upload: 1000 } }] },
				_native_apply_ui: { selected: 'recommended', acknowledged: {}, pending: false, error: payload, receipt: null },
			};
			verify('apply-error-and-acknowledgement-text', settings.renderNativeAutotuneDiagnostics(proposal), payload);
			verify('controller-issue', status.formatServices({ overall_state: 'WAITING', controller_reason: payload, issues: payload }), payload);
			verify('scheduler-message', status.formatState({ state: 'RUNNING', scheduled_autotune: {
				enabled: true, available: false, message: payload } }, true, {}, null), payload);
			verify('scheduler-global-diagnostic', status.renderStatusData([], {}, [], {}, {},
				[{ instance: 'lab', message: payload }]), payload);
			const priorities = new Function('fs', 'form', 'uci', 'ui', 'cakeUi', 'L', 'E', '_',
				prefix(source.priorities) + '\nreturn { notifyPresetError };')({}, {}, {}, ui, cakeUi, {}, E, value => value);
			priorities.notifyPresetError(new Error(payload));
			verify('priorities-error', notices.pop(), payload);
			const lite = new Function('form', 'uci', 'ui', 'rpc', 'L', 'E', '_',
				prefix(source.lite) + '\nreturn { validationMessage };')(
				{}, {}, ui, { declare: () => () => Promise.resolve(0) }, {}, E, value => value);
			verify('lite-validation', lite.validationMessage(payload), payload);
			const liteStatus = new Function('fs', 'poll', 'uci', 'L', 'E', '_',
				prefix(source.liteStatus) + '\nreturn { renderRows };')({}, {}, {}, {}, E, value => value);
			verify('lite-status-config-name-and-route', liteStatus.renderRows([
				{ config: { '.name': payload, wan_if: payload, enabled: '0' }, status: null }
			]), payload);
			verify('full-route-metadata', status.formatRoute({ route_mode: 'main',
				mwan3_member: payload, route_device: payload, route_external_ip: payload }), payload);
			verify('full-status-name-and-reflector', status.renderStatusData([
				{ '.name': payload, enabled: '1' }], [{ reflector: payload }], ['instance', 'reflector'], {}, {}), payload);
			// LuCI stabilizes this column with an inline desktop width. Keep all
			// actions visible on mobile, without changing desktop or another page.
			for (const [width, scoped] of [[390, true], [1440, true], [390, false]]) {
				const frame = document.createElement('iframe');
				frame.style.cssText = `width:${width}px;height:250px;border:0`;
				document.body.appendChild(frame);
				const doc = frame.contentDocument;
				doc.body.innerHTML = '<style>*{box-sizing:border-box}body{margin:5px}.cbi-section-actions{display:block;white-space:nowrap}.cbi-section-actions>div{display:flex;white-space:nowrap}.cbi-button{white-space:pre;padding:5px}</style>' +
					`<div id="${scoped ? 'cbi-cake-autorate' : 'another-map'}"><div class="cbi-section-actions" style="min-width:400px;width:400px"><div>` +
					['Traffic priorities', 'Re-run Auto-Tune', 'Edit', 'Delete'].map(t => `<button class="cbi-button">${t}</button>`).join(' ') + '</div></div></div>';
				const actions = doc.querySelector('.cbi-section-actions');
				if (actions.getBoundingClientRect().width !== 400) throw new Error('action width negative control missing');
				if (settings.settingsActionLayout) {
					const style = doc.createElement('style');
					style.textContent = settings.settingsActionLayout().textContent;
					doc.head.appendChild(style);
				}
				if (scoped && width === 390) {
					if (doc.documentElement.scrollWidth > width) throw new Error('settings mobile actions overflow');
					for (const button of doc.querySelectorAll('button')) {
						const rect = button.getBoundingClientRect();
						if (rect.width <= 0 || rect.right > width) throw new Error('settings mobile action hidden or clipped');
					}
					cases.push('settings-mobile-row-actions');
				} else {
					if (actions.getBoundingClientRect().width !== 400) throw new Error('action width changed outside mobile scope');
					cases.push(scoped ? 'settings-desktop-row-actions' : 'settings-style-page-scope');
				}
				frame.remove();
			}
			return { positiveControl: true, cases };
		}, sources);
		assert.equal(result.positiveControl, true);
		assert.equal(result.cases.length, 19);
		console.log('DOM_TEXT_BOUNDARY_PASS ' + result.cases.join(', '));
	} finally {
		await browser.close();
	}
})().catch(error => { console.error(error); process.exitCode = 1; });
