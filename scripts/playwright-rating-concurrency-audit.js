'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require('/home/tester/.local/share/playwright/node_modules/playwright');

const baseUrl = process.env.LUCI_BASE_URL;
const storageState = process.env.LUCI_STORAGE_STATE;
const outputDir = process.env.OUTPUT_DIR;
const instance = process.env.CAKE_INSTANCE;

if (!baseUrl || !storageState || !outputDir || !instance)
	throw new Error('LUCI_BASE_URL, LUCI_STORAGE_STATE, OUTPUT_DIR and CAKE_INSTANCE are required');
fs.mkdirSync(outputDir, { recursive: true });

function rpcSpec(request, action) {
	let batch;
	try { batch = JSON.parse(request.postData() || '[]'); }
	catch (_) { return null; }
	if (!Array.isArray(batch)) batch = [ batch ];
	for (let index = 0; index < batch.length; index++) {
		const entry = batch[index];
		const params = entry && entry.params;
		const spec = Array.isArray(params) ? params[3] : null;
		if (Array.isArray(params) && params[1] === 'file' && params[2] === 'exec' &&
		    spec && spec.command === '/usr/sbin/cake-autorated' && Array.isArray(spec.params) &&
		    spec.params[0] === '--calibrationctl' && spec.params[1] === action)
			return { id: entry.id, index };
	}
	return null;
}

async function rpcResult(response, action) {
	const spec = rpcSpec(response.request(), action);
	if (!spec) throw new Error(`${action} request identity disappeared`);
	const payload = await response.json();
	const replies = Array.isArray(payload) ? payload : [ payload ];
	const reply = spec.id == null ? replies[spec.index] :
		replies.find(item => item && item.id === spec.id);
	const envelope = reply && reply.result;
	if (!Array.isArray(envelope) || envelope[0] !== 0 || !envelope[1])
		throw new Error(`${action} has no successful RPC envelope`);
	if (envelope[1].code !== 0)
		throw new Error(`${action} exited ${envelope[1].code}`);
	return JSON.parse(envelope[1].stdout || '{}');
}

async function nativeCtl(page, operation, argument) {
	return page.evaluate(async request => {
		const fsModule = await L.require('fs');
		const args = [ '--calibrationctl', request.operation ];
		if (request.argument) args.push(request.argument);
		const response = await fsModule.exec('/usr/sbin/cake-autorated', args);
		return {
			code: response.code,
			stderr: response.stderr || '',
			parsed: JSON.parse(response.stdout || '{}'),
		};
	}, { operation, argument: argument || '' });
}

async function waitForJobState(page, jobId, terminalStates, timeoutMs = 60000) {
	const deadline = Date.now() + timeoutMs;
	let current = null;
	while (Date.now() < deadline) {
		current = (await nativeCtl(page, 'rating-status', jobId)).parsed;
		if (terminalStates.includes(current.state)) return current;
		if (![ 'queued', 'starting', 'running', 'cancelling', 'recovering' ].includes(current.state))
			throw new Error(`Rating ${jobId} reached unexpected state ${current.state}`);
		await page.waitForTimeout(250);
	}
	throw new Error(`Rating ${jobId} did not reach ${terminalStates.join('/')} from ${current && current.state}`);
}

async function openStatus(page) {
	await page.goto(`${baseUrl}/admin/network/cake-autorate-rs/status`, {
		waitUntil: 'networkidle', timeout: 45000,
	});
	if ((await page.locator('body').innerText()).includes('Authorization Required'))
		throw new Error('Saved LuCI session expired');
}

async function openRating(page) {
	const row = page.locator('.cake-status-table tbody tr, tr').filter({ hasText: instance }).first();
	await row.getByText('Get rating', { exact: true }).click();
	const modal = page.locator('.cbi-modal, .modal').filter({
		hasText: `Get rating — ${instance}`,
	}).first();
	await modal.waitFor({ state: 'visible', timeout: 15000 });
	return modal;
}

async function waitEnabled(page, locator, timeoutMs = 45000) {
	const handle = await locator.elementHandle();
	if (!handle) throw new Error('Rating Start button is missing');
	await page.waitForFunction(node => !node.disabled, handle, { timeout: timeoutMs });
}

async function startRating(page, modal, mode) {
	await modal.locator('select.cbi-input-select').selectOption(mode);
	const start = modal.getByText('Start rating', { exact: true });
	await waitEnabled(page, start);
	const responsePromise = page.waitForResponse(response =>
		Boolean(rpcSpec(response.request(), 'rating-start')), { timeout: 15000 });
	await start.click();
	const response = await rpcResult(await responsePromise, 'rating-start');
	if (response.state === 'error' || !/^[0-9a-f]{32}$/.test(response.job_id || ''))
		throw new Error(`${mode} Rating was not admitted: ${JSON.stringify(response)}`);
	return response;
}

async function startGuided(page, modal) {
	return startRating(page, modal, 'client');
}

function assertNoRawLeaseText(text) {
	for (const forbidden of [ 'runtime lease', 'Instance(', 'already owned by job' ])
		if (text.includes(forbidden)) throw new Error(`Modal leaked raw lease text: ${forbidden}`);
}

(async () => {
	let browser;
	let pageA;
	let pageB;
	const activeJobs = new Set();
	const report = {
		pass: false, instance, closeDuringStart: {}, sequentialWorkerIdentity: {},
		delayedAttestation: {}, conflictRecovery: {},
	};
	try {
		browser = await chromium.launch({
			headless: true,
			executablePath: '/home/tester/.cache/ms-playwright/chromium-1228/chrome-linux64/chrome',
		});
		const options = {
			storageState, viewport: { width: 1500, height: 920 }, ignoreHTTPSErrors: true,
			extraHTTPHeaders: { 'Cache-Control': 'no-cache', Pragma: 'no-cache' },
		};
		const contextA = await browser.newContext(options);
		const contextB = await browser.newContext(options);
		pageA = await contextA.newPage();
		pageB = await contextB.newPage();
		await Promise.all([ openStatus(pageA), openStatus(pageB) ]);

		let heldReceiptSeen;
		let releaseHeldReceipt;
		const heldReceiptSeenPromise = new Promise(resolve => { heldReceiptSeen = resolve; });
		const releaseHeldReceiptPromise = new Promise(resolve => { releaseHeldReceipt = resolve; });
		let heldReceipt = false;
		await pageA.route('**/ubus/**', async route => {
			if (!heldReceipt && rpcSpec(route.request(), 'rating-start')) {
				heldReceipt = true;
				heldReceiptSeen();
				await releaseHeldReceiptPromise;
			}
			await route.continue();
		});
		let modalA = await openRating(pageA);
		await modalA.locator('select.cbi-input-select').selectOption('client');
		const closeRaceStart = modalA.getByText('Start rating', { exact: true });
		await waitEnabled(pageA, closeRaceStart);
		const closeRaceResponsePromise = pageA.waitForResponse(response =>
			Boolean(rpcSpec(response.request(), 'rating-start')), { timeout: 30000 });
		const closeRaceClick = closeRaceStart.click();
		await heldReceiptSeenPromise;
		await modalA.getByText('Cancel', { exact: true }).click();
		releaseHeldReceipt();
		await closeRaceClick;
		const closeRaceJob = await rpcResult(await closeRaceResponsePromise, 'rating-start');
		if (!/^[0-9a-f]{32}$/.test(closeRaceJob.job_id || ''))
			throw new Error(`Close-during-Start returned no job ID: ${JSON.stringify(closeRaceJob)}`);
		activeJobs.add(closeRaceJob.job_id);
		report.closeDuringStart = {
			jobId: closeRaceJob.job_id,
			cancelled: await waitForJobState(pageA, closeRaceJob.job_id, [ 'cancelled' ]),
		};
		activeJobs.delete(closeRaceJob.job_id);
		await pageA.unroute('**/ubus/**');
		await openStatus(pageA);

		modalA = await openRating(pageA);
		const automatic = await startRating(pageA, modalA, 'automatic');
		activeJobs.add(automatic.job_id);
		const automaticCompleted = await waitForJobState(
			pageA, automatic.job_id, [ 'completed' ], 10 * 60 * 1000);
		activeJobs.delete(automatic.job_id);
		await modalA.locator('.cake-quality-job-state').filter({
			hasText: /Rating .* complete:/,
		}).waitFor({ state: 'visible', timeout: 30000 });
		const sequential = await startGuided(pageA, modalA);
		activeJobs.add(sequential.job_id);
		await waitForJobState(pageA, sequential.job_id, [ 'running' ], 30000);
		await modalA.locator('.cake-quality-job-state').filter({
			hasText: /Baseline \d+\/\d+/,
		}).waitFor({ state: 'visible', timeout: 30000 });
		const sequentialText = await modalA.innerText();
		if (sequentialText.includes('invalid worker identity'))
			throw new Error('Automatic-to-Guided transition retained the previous worker identity');
		assertNoRawLeaseText(sequentialText);
		report.sequentialWorkerIdentity = {
			automaticJobId: automatic.job_id,
			automaticCompleted,
			guidedJobId: sequential.job_id,
		};
		await pageA.screenshot({ path: path.join(outputDir, 'automatic-to-guided-running.png'), fullPage: true });
		await modalA.getByText('Cancel', { exact: true }).click();
		report.sequentialWorkerIdentity.guidedCancelled = await waitForJobState(
			pageA, sequential.job_id, [ 'cancelled' ]);
		activeJobs.delete(sequential.job_id);

		await openStatus(pageA);
		await openStatus(pageB);
		modalA = await openRating(pageA);
		const first = await startGuided(pageA, modalA);
		activeJobs.add(first.job_id);
		await waitForJobState(pageA, first.job_id, [ 'running' ], 30000);

		let releaseCurrent;
		let currentSeen;
		const currentSeenPromise = new Promise(resolve => { currentSeen = resolve; });
		const releaseCurrentPromise = new Promise(resolve => { releaseCurrent = resolve; });
		let heldCurrent = false;
		let pageBStarts = 0;
		pageB.on('request', request => {
			if (rpcSpec(request, 'rating-start')) pageBStarts++;
		});
		await pageB.route('**/ubus/**', async route => {
			if (!heldCurrent && rpcSpec(route.request(), 'rating-current')) {
				heldCurrent = true;
				currentSeen();
				await releaseCurrentPromise;
			}
			await route.continue();
		});
		const modalB = await openRating(pageB);
		await currentSeenPromise;
		const startB = modalB.getByText('Start rating', { exact: true });
		if (await startB.isEnabled())
			throw new Error('Start became enabled before rating-current attestation completed');
		report.delayedAttestation.startDisabled = true;
		releaseCurrent();
		await modalB.getByText('Cancel', { exact: true }).waitFor({ state: 'visible', timeout: 30000 });
		if (pageBStarts !== 0)
			throw new Error(`Second session sent ${pageBStarts} rating-start requests during adoption`);
		const adoptedText = await modalB.innerText();
		assertNoRawLeaseText(adoptedText);
		report.delayedAttestation.adoptedJobId = first.job_id;
		report.delayedAttestation.startRequests = pageBStarts;
		await pageB.screenshot({ path: path.join(outputDir, 'active-rating-adopted.png'), fullPage: true });
		await modalB.getByText('Cancel', { exact: true }).click();
		const firstCancelled = await waitForJobState(pageA, first.job_id, [ 'cancelled' ]);
		activeJobs.delete(first.job_id);
		report.delayedAttestation.cancelled = firstCancelled;
		await modalA.getByText('Close', { exact: true }).click().catch(() => {});

		await pageB.unroute('**/ubus/**');
		await openStatus(pageA);
		await openStatus(pageB);
		const idleModalB = await openRating(pageB);
		const idleStartB = idleModalB.getByText('Start rating', { exact: true });
		await idleModalB.locator('select.cbi-input-select').selectOption('client');
		await waitEnabled(pageB, idleStartB);

		let releaseStart;
		let heldStartSeen;
		const heldStartSeenPromise = new Promise(resolve => { heldStartSeen = resolve; });
		const releaseStartPromise = new Promise(resolve => { releaseStart = resolve; });
		let heldStart = false;
		await pageB.route('**/ubus/**', async route => {
			if (!heldStart && rpcSpec(route.request(), 'rating-start')) {
				heldStart = true;
				heldStartSeen();
				await releaseStartPromise;
			}
			await route.continue();
		});
		pageBStarts = 0;
		const conflictResponsePromise = pageB.waitForResponse(response =>
			Boolean(rpcSpec(response.request(), 'rating-start')), { timeout: 30000 });
		const losingClick = idleStartB.click();
		await heldStartSeenPromise;

		const winnerModalA = await openRating(pageA);
		const winner = await startGuided(pageA, winnerModalA);
		activeJobs.add(winner.job_id);
		await waitForJobState(pageA, winner.job_id, [ 'running' ], 30000);

		releaseStart();
		await losingClick;
		const conflict = await rpcResult(await conflictResponsePromise, 'rating-start');
		if (conflict.state !== 'error' || conflict.error_code !== 'lease-conflict' ||
		    conflict.conflict_kind !== 'instance')
			throw new Error(`Race did not return the typed instance conflict: ${JSON.stringify(conflict)}`);
		if (/Instance\(|already owned by job|runtime lease/.test(conflict.error || ''))
			throw new Error(`Typed conflict still carries raw user text: ${conflict.error}`);
		await idleModalB.getByText('Cancel', { exact: true }).waitFor({ state: 'visible', timeout: 30000 });
		await pageB.unroute('**/ubus/**');
		if (pageBStarts !== 1)
			throw new Error(`Conflict recovery sent ${pageBStarts} rating-start requests instead of one`);
		const recoveredText = await idleModalB.innerText();
		assertNoRawLeaseText(recoveredText);
		report.conflictRecovery = {
			winnerJobId: winner.job_id,
			conflict,
			startRequests: pageBStarts,
		};
		await pageB.screenshot({ path: path.join(outputDir, 'post-attestation-conflict-adopted.png'), fullPage: true });
		await idleModalB.getByText('Cancel', { exact: true }).click();
		report.conflictRecovery.cancelled = await waitForJobState(pageA, winner.job_id, [ 'cancelled' ]);
		activeJobs.delete(winner.job_id);

		const current = (await nativeCtl(pageA, 'rating-current', instance)).parsed;
		if (current.state !== 'idle')
			throw new Error(`Rating ownership did not settle to idle: ${JSON.stringify(current)}`);
		report.finalCurrent = current;
		report.pass = true;
		await contextA.close();
		await contextB.close();
	} catch (error) {
		report.error = error.stack || String(error);
		if (pageA) {
			for (const jobId of activeJobs) {
				try { await nativeCtl(pageA, 'rating-cancel', jobId); }
				catch (cleanupError) { report.cleanupError = cleanupError.stack || String(cleanupError); }
			}
		}
	} finally {
		if (browser) await browser.close();
		fs.writeFileSync(path.join(outputDir, 'result.json'), `${JSON.stringify(report, null, 2)}\n`);
	}
	console.log(JSON.stringify(report));
	if (!report.pass) process.exitCode = 1;
})();
