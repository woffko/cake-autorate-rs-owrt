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
				prefix(source.settings) + '\nreturn { renderAutotuneDiagnostics, renderNativeAutotuneDiagnostics, renderNativeServerComparison, autotuneTrafficPolicyControl, autotuneTrafficPolicyForRun, disableAutotuneTrafficPolicy, settingsActionLayout: typeof settingsActionLayout === "function" ? settingsActionLayout : null };')(
				{}, {}, {}, {}, ui, {}, cakeUi, { declare: () => () => Promise.resolve(0) }, {}, E, value => value);
			verify('autotune-diagnostic-and-code', settings.renderAutotuneDiagnostics({ error: payload, reason: payload }), payload);
			const trafficDiagnostic = settings.renderAutotuneDiagnostics({ diagnostic: payload, traffic: {
				schema_version: 1, policy: 'capped', limit_bytes: 32000000000,
				consumed_bytes: 16381000000, remaining_bytes: 15619000000, overrun_bytes: 0,
				accounting: 'owned-ip-system-dns-estimate-v1', source: 'terminal_record'
			} });
			verify('autotune-traffic-terminal-diagnostic', trafficDiagnostic, payload);
			if (!trafficDiagnostic.textContent.includes('System DNS is retained') ||
			    !trafficDiagnostic.textContent.includes('not exact physical WAN accounting'))
				throw new Error('owned traffic accounting scope is missing');
			const failedComparison = settings.renderAutotuneDiagnostics({ state: 'failed', server_comparison_diagnostic: {
				schema_version: 1, report_sha256: 'a'.repeat(64), scope: 'no_sqm', selected_server_id: null,
				proof_status: 'identity-bound-diagnostic-only', state: 'failed', reason: 'speedtest-traffic-budget-exhausted',
				observations: [{ index: 1, candidate_id: 11, server_id: 11, name: payload, sponsor: payload,
					download_kbps: 90000, upload_kbps: 45000, elapsed_ms: 2000, valid_observation: true, reason: 'valid-observation' }]
			} });
			verify('failed-server-comparison-diagnostic', failedComparison, payload);
			if (failedComparison.querySelector('button') || !failedComparison.textContent.includes('Diagnostic snapshot only'))
				throw new Error('failed comparison must remain non-applicable diagnostics');
			if (!trafficDiagnostic.textContent.includes('16.381 GB') ||
			    !trafficDiagnostic.textContent.includes('15.619 GB')) throw new Error('terminal traffic projection is missing');
			const proposal = {
				native_job_id: 'a'.repeat(32), artifacts: {},
				public_apply_contract: { options: [{ option_id: 'recommended', preferred: true,
					selected_topology: 'both_shaped', required_acknowledgements: [payload],
					target_rates_kbps: { download: 1000, upload: 1000 } }] },
				_native_apply_ui: { selected: 'recommended', acknowledged: {}, pending: false, error: payload, receipt: null },
			};
			const originalProposal = JSON.stringify(proposal);
			const review = settings.renderNativeAutotuneDiagnostics(proposal);
			verify('apply-error-and-acknowledgement-text', review, payload);
			const capacityScope = review.querySelector('.cake-autotune-capacity-scope');
			if (!capacityScope || capacityScope.closest('details') ||
			    !capacityScope.textContent.includes('cannot exclude a shared bottleneck') ||
			    !review.textContent.includes('Proposed CAKE DL / UL: 1000 / 1000 kbit/s') ||
			    !review.textContent.includes('Observed test DL / UL: - / - kbit/s') ||
			    JSON.stringify(proposal) !== originalProposal)
				throw new Error('Review must distinguish observed throughput from settings and capacity without mutating the proposal');
			proposal.prior_raw_comparison = {
				schema_version: 1, proof_status: 'verified-prior-review-diagnostic-only',
				prior_job_id: 'b'.repeat(32), prior_created_unix_ms: 1,
				current_download_kbps: 170000, current_upload_kbps: 100000,
				prior_download_kbps: 900000, prior_upload_kbps: 110000
			};
			const historyReview = settings.renderNativeAutotuneDiagnostics(proposal);
			if (!historyReview.querySelector('.cake-autotune-prior-throughput').textContent.includes('18.9% / 90.9% retained'))
				throw new Error('Review must show verified prior raw rates rather than configured CAKE rates');
			proposal.prior_raw_comparison.new_apply_blocked = true;
			const blockedReview = settings.renderNativeAutotuneDiagnostics(proposal);
			if (!blockedReview.querySelector('button').disabled ||
			    blockedReview.textContent.includes('ready to apply') ||
			    blockedReview.textContent.includes('recommended for this profile') ||
			    !blockedReview.textContent.includes('Unconfirmed result'))
				throw new Error('large historical decline must not be presented as a recommended applicable result');
			proposal.prior_raw_comparison.prior_download_kbps = 0;
			if (settings.renderNativeAutotuneDiagnostics(proposal).querySelector('.cake-autotune-prior-throughput'))
				throw new Error('invalid history must not create a misleading comparison');
			verify('server-comparison-name-and-provider', settings.renderNativeServerComparison({
				schema_version: 1, report_sha256: 'a'.repeat(64), scope: 'no_sqm', selected_server_id: 2,
				proof_status: 'source-comparison-only', observations: Array.from({ length: 6 }, (_, index) => ({
					index: index + 1, candidate_id: index % 2 + 1, server_id: index % 2 + 1,
					name: payload, sponsor: payload, download_kbps: 90000, upload_kbps: 45000,
					elapsed_ms: 2000, valid_observation: true, reason: 'valid-observation'
				}))
			}), payload);
			const storageDescriptor = Object.getOwnPropertyDescriptor(window, 'localStorage');
			const preferences = new Map();
			let preferenceWrites = 0;
			Object.defineProperty(window, 'localStorage', { configurable: true, value: {
				getItem: key => preferences.get(key) || null,
				setItem: (key, value) => { preferenceWrites++; preferences.set(key, value); }
			} });
			try {
				const state = { sqm_download: '100000', sqm_upload: '50000' };
				const control = settings.autotuneTrafficPolicyControl(state, 'fixture_one', false);
				if (!control.querySelector('.cake-autotune-dns-accounting').textContent.includes('System DNS and local DNS filtering are preserved'))
					throw new Error('test launch must disclose preserved system DNS and approximate service accounting');
				document.body.appendChild(control);
				const mode = control.querySelector('select');
				const amount = control.querySelector('input[type=text]');
				const remember = control.querySelector('input[type=checkbox]');
				const planningRates = control.querySelectorAll('input[type=number]');
				const estimate = control.querySelector('.cake-autotune-traffic-estimate');
				if (planningRates.length !== 2 || !estimate.textContent.includes('6.19 GB'))
					throw new Error('full-plan scenario is missing or does not use configured hints');
				if (!estimate.textContent.includes('2.53 GB') || !estimate.textContent.includes('18 server runs total'))
					throw new Error('backup server planning traffic is missing');
				planningRates[0].value = '1000'; planningRates[0].dispatchEvent(new Event('input'));
				if (!estimate.textContent.includes('43.31 GB') || state.sqm_download !== '100000' || state.service_dl_cap_kbps != null)
					throw new Error('planning input failed to refresh or changed speed-limit authority');
				planningRates[1].value = ''; planningRates[1].dispatchEvent(new Event('input'));
				if (!estimate.textContent.includes('Enter both estimated rates'))
					throw new Error('missing planning rate retained a stale estimate');
				planningRates[1].value = '50'; planningRates[1].dispatchEvent(new Event('input'));
				if (mode.value !== '' || preferenceWrites !== 0) throw new Error('traffic policy was silently selected or saved');
				for (const gb of [1, 5, 10, 25, 50, 100]) {
					mode.value = 'gb:' + gb; mode.dispatchEvent(new Event('change'));
					amount.value = '999';
					if (gb < 15.75) {
						let refused = false;
						try { settings.autotuneTrafficPolicyForRun(state, 'fixture_one'); }
						catch (error) { refused = error.message.includes('initial-stage planning allowance of 15.75 GB'); }
						if (!refused) throw new Error('insufficient initial planning budget was admitted');
					} else if (settings.autotuneTrafficPolicyForRun(state, 'fixture_one').bytes !== gb * 1000000000)
						throw new Error('traffic preset is missing or replaced by hidden input');
					if (preferenceWrites !== 0) throw new Error('traffic preference saved implicitly');
					if (estimate.textContent.includes('below this planning scenario') !== (gb < 43.3125))
						throw new Error('planning warning did not track the selected total allowance');
				}
				mode.value = 'unlimited'; mode.dispatchEvent(new Event('change'));
				if (settings.autotuneTrafficPolicyForRun(state, 'fixture_one').mode !== 'unlimited' || preferenceWrites !== 0)
					throw new Error('one-run unlimited choice was not explicit');
				remember.checked = true; remember.dispatchEvent(new Event('change'));
				if (preferenceWrites !== 0) throw new Error('preference written before explicit start');
				settings.autotuneTrafficPolicyForRun(state, 'fixture_one');
				if (preferenceWrites !== 1) throw new Error('explicit remembered preference missing');
				const other = settings.autotuneTrafficPolicyControl(Object.assign({}, state), 'fixture_two', false);
				if (other.querySelector('select').value !== '') throw new Error('traffic preference crossed instance boundary');
				const restored = settings.autotuneTrafficPolicyControl({}, 'fixture_one', false);
				if (restored.querySelector('select').value !== 'unlimited') throw new Error('remembered choice was not restored');
				mode.value = 'capped'; mode.dispatchEvent(new Event('change'));
				remember.checked = false;
				amount.value = '15.749999999';
				let belowPlanRefused = false;
				try { settings.autotuneTrafficPolicyForRun(state, 'fixture_one'); }
				catch (error) { belowPlanRefused = error.message.includes('initial-stage planning allowance'); }
				if (!belowPlanRefused || preferenceWrites !== 1) throw new Error('planning boundary did not refuse without saving');
				amount.value = '15.75';
				if (settings.autotuneTrafficPolicyForRun(state, 'fixture_one').bytes !== 15750000000)
					throw new Error('exact initial-stage allowance was refused');
				// Browser-restored planning rates must also be read at submission.
				planningRates[0].value = '1'; planningRates[1].value = '1';
				remember.checked = true;
				amount.value = '0.25'; // Autofill need not emit an input event.
				if (settings.autotuneTrafficPolicyForRun(state, 'fixture_one').bytes !== 250000000)
					throw new Error('GB total did not convert to exact bytes');
				if (state._traffic_estimate_dl_mbps !== '1' ||
					JSON.parse(preferences.get('cake-autorate-autotune-traffic-v1:fixture_one')).planning != null)
					throw new Error('planning rates were not read or were persisted as traffic preference');
				amount.value = '0'; amount.dispatchEvent(new Event('input'));
				let refused = false;
				try { settings.autotuneTrafficPolicyForRun(state, 'fixture_one'); } catch (error) { refused = true; }
				if (!refused || preferenceWrites !== 2) throw new Error('invalid zero budget was accepted or saved');
				settings.disableAutotuneTrafficPolicy('fixture_one');
				if (Array.from(control.querySelectorAll('input,select')).some(input => !input.disabled))
					throw new Error('running traffic policy remained editable');
				control.remove();
				cases.push('traffic-policy-choice-and-instance-scope');
				cases.push('traffic-planning-not-rate-authority');
			} finally {
				if (storageDescriptor) Object.defineProperty(window, 'localStorage', storageDescriptor);
				else delete window.localStorage;
			}
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
			verify('lite-status-runtime-owner', liteStatus.renderRows([
				{ config: { '.name': 'fixture', enabled: '1' }, status: {
					runtime_control_degraded: true, runtime_control_held: true, runtime_control_error: payload } }
			]), payload);
			verify('full-status-runtime-owner', status.formatState({ state: 'RUNNING',
				runtime_control_degraded: true, runtime_control_held: true, runtime_control_error: payload }, true, {}, null), payload);
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
		assert.equal(result.cases.length, 26);
		assert.ok(result.cases.includes('failed-server-comparison-diagnostic'));
		assert.ok(result.cases.includes('autotune-traffic-terminal-diagnostic'));
		assert.ok(result.cases.includes('traffic-policy-choice-and-instance-scope'));
		assert.ok(result.cases.includes('traffic-planning-not-rate-authority'));
		assert.ok(result.cases.includes('server-comparison-name-and-provider'));
		assert.ok(result.cases.includes('full-status-runtime-owner'));
		assert.ok(result.cases.includes('lite-status-runtime-owner'));
		console.log('DOM_TEXT_BOUNDARY_PASS ' + result.cases.join(', '));
	} finally {
		await browser.close();
	}
})().catch(error => { console.error(error); process.exitCode = 1; });
