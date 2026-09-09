'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const sourcePath = path.join(__dirname, '..', 'htdocs', 'luci-static', 'resources',
	'view', 'cake-autorate-rs', 'settings.js');
const source = fs.readFileSync(sourcePath, 'utf8');
assert.doesNotMatch(source, /APPLIED BY NATIVE RUST/,
	'Auto-Tune user-facing receipts must describe the result, not the implementation language');
assert.equal((source.match(/window\.setTimeout\(/g) || []).length, 2,
	'settings timers are limited to native Speed Test and Full Auto-Tune polling helpers');
for (const helper of [ 'speedtestJobDelay', 'autotuneJobDelay' ]) {
	assert.match(source, new RegExp('function ' + helper + '\\([^)]*\\)\\s*\\{[\\s\\S]*?window\\.setTimeout\\('),
		`${helper} must remain an explicit polling/watchdog cadence helper`);
}
const prefix = source.slice(0, source.indexOf('return L.view.extend'));
assert.match(source, /function modal\(option\)\s*\{[\s\S]*?option\.modalonly = true;[\s\S]*?option\.retain = true;/,
	'all modal settings must retain dependency-hidden values instead of staging unrelated deletions');
assert.match(source,
	/handleReset: function\(ev\)[\s\S]*?discardStagedUciPackages\(\[ 'cake-autorate', 'sqm' \]\)[\s\S]*?reloadViewPage\(\)/,
	'Reset must revert exactly the settings-owned packages before rebuilding the page');
assert.match(source,
	/var callUciRevertStatus = rpc\.declare\(\{[\s\S]*?object: 'uci'[\s\S]*?method: 'revert'[\s\S]*?params: \[ 'config' \]/,
	'Reset must retain its exact package-scoped UCI revert RPC');
assert.match(source,
	/var callUciConfirmStatus = rpc\.declare\(\{[\s\S]*?object: 'uci'[\s\S]*?method: 'confirm'/,
	'the disabled Multi-WAN transaction must retain exact rollback confirmation');
assert.doesNotMatch(source, /rpcd-helper|apply-guard-abort/,
	'the browser must not retain the retired shell Apply Guard dispatcher');
assert.match(source, /'autotune-apply-start'/,
	'Apply must be admitted by the long-lived coordinator before any network mutation');
assert.match(source, /'autotune-apply-watch'/,
	'Apply progress must wait for a durable generation change through its private handle');
assert.doesNotMatch(source, /NATIVE_AUTOTUNE_APPLY_POLL_DELAY_MS|NATIVE_AUTOTUNE_APPLY_MAX_POLLS/,
	'Apply must not use timer-driven status polling');
assert.match(source, /'autotune-apply-result'/,
	'Apply completion must use a separately fetched durable receipt');
assert.doesNotMatch(source, /\[ '--calibrationctl', 'autotune-apply',/,
	'the browser must not invoke the retired synchronous Apply executor');
assert.doesNotMatch(source, /nativeApplyDeliveryUncertain|nativeApplyReloadRequired/,
	'Apply transport recovery must use durable status, never an ambiguous reload flag');
assert.match(source, /50% historical-throughput trust boundary/,
	'profile help must explain the manual historical-throughput trust boundary');
assert.match(source,
	/\.cake-autotune-profile-grid\{display:grid;grid-template-columns:repeat\(4,minmax\(0,1fr\)\);[^}]*align-items:stretch/,
	'the four Auto-Tune profiles must share one equal-width horizontal grid');
assert.match(source,
	/\.cake-autotune-profile-card\{[^}]*width:100%;height:100%;[^}]*box-sizing:border-box/,
	'profile cards must stretch to the same row height without overflowing');
assert.match(source,
	/@media\(max-width:800px\)\{\.cake-autotune-profile-grid\{grid-template-columns:minmax\(0,1fr\)\}\}/,
	'the equal four-card row must collapse safely on narrow screens');
assert.equal((source.match(/autotuneProfileGrid\(profileButtons\)/g) || []).length, 2,
	'single-WAN and sequential Multi-WAN profile selectors must use the same layout');
assert.equal((source.match(/diagnosticsNode\.style\.display = 'none'/g) || []).length, 2,
	'a repeated single-WAN or Multi-WAN run must hide stale diagnostics without replacing the live progress DOM');
assert.match(source,
	/o = iface\(section, 'interfaces', 'dl_if',[\s\S]*?o\.depends\('auto_interface_preset', '0'\);\s*o\.retain = true;/,
	'hidden automatic download interface must survive modal saves');
assert.match(source,
	/o = iface\(section, 'interfaces', 'ul_if',[\s\S]*?o\.depends\('auto_interface_preset', '0'\);\s*o\.retain = true;/,
	'hidden automatic upload interface must survive modal saves');
assert.match(source,
	/o = iface\(section, 'sqm_basic', 'sqm_interface',[\s\S]*?dependsManagedSqm\(o, \{ auto_interface_preset: '0' \}\);\s*o\.retain = true;/,
	'hidden automatic SQM interface must survive modal saves');
const diagnosticsRendererSource = source.slice(
	source.indexOf('function renderAutotuneDiagnostics'),
	source.indexOf('function replaceNodeContent'));
assert.doesNotMatch(diagnosticsRendererSource, /\bstate\./,
	'the global diagnostics renderer must not capture wizard-local state');
assert.doesNotMatch(source, /function runPlan\(index\)/,
	'Multi-WAN Auto-Tune must never recursively run every uplink without a user decision');
assert.match(source, /function renderMultiwanAutotuneStep\(\)/,
	'Multi-WAN Auto-Tune needs a dedicated per-uplink workflow');
assert.match(source, /Calibration profile for %s/,
	'each Multi-WAN uplink must expose its own profile choice');
assert.match(source,
	/applySequentialMultiwanPlan\(config_name, planItems\)[\s\S]*?multiwanAutotuneItemNativeApplied[\s\S]*?applyPlainRollbackTransaction\(\[ 'cake-autorate' \]\)[\s\S]*?reloadWizardUci\(\)/,
	'Multi-WAN must keep accepted native receipts and apply only skipped disabled instances through UCI');
assert.doesNotMatch(source, /multiwanAutotunePendingStagedApplyItems|A staged Multi-WAN proposal cannot be applied/,
	'the browser must not retain the retired staged-result fallback');
assert.doesNotMatch(source, /function (?:stageAutotuneApplyMarker|armAutotuneApplyGuards|runGuardedSaveApply)\b|apply-guard-arm/,
	'the browser must not expose or invoke the retired staged Apply Guard path');
assert.match(source, /Create & apply sequentially/,
	'the Multi-WAN final action must describe that it applies the accepted proposals');
assert.doesNotMatch(source,
	/state\.multiwan_set\s*\?\s*E\('pre'[\s\S]*?\)\s*:\s*null/,
	'the Multi-WAN plan must never pass a null child that LuCI renders as literal text');
assert.match(source,
	/E\('pre',\s*\{[\s\S]*?'style':\s*state\.multiwan_set\s*\?[\s\S]*?'display:none'[\s\S]*?\},\s*plan\)/,
	'the inactive Multi-WAN plan must remain a real hidden DOM node');
assert.match(source, /I accept all listed trade-offs\./,
	'Native Review must use one aggregate acknowledgement for the fully listed trade-offs');
assert.doesNotMatch(source, /Reload settings/,
	'a successful Apply must refresh committed settings without another user click');
assert.match(source,
	/function reloadAppliedUciPackages\(\)[\s\S]*?uci\.unload\('cake-autorate'\)[\s\S]*?uci\.unload\('sqm'\)[\s\S]*?uci\.load\('cake-autorate'\)[\s\S]*?uci\.load\('sqm'\)/,
	'post-Apply refresh must invalidate both LuCI UCI caches before reloading them');
assert.match(source,
	/runNativeAutotuneApply\(result, option\)[\s\S]*?typeof onApplied !== 'function'[\s\S]*?reloadAppliedSettingsPage\(\)/,
	'a successful single-instance Apply must reload authoritative settings automatically');
assert.match(source,
	/renderNativeAutotuneDiagnostics\(settledResult, function\(receipt, option\)[\s\S]*?reloadWizardUci\(\)[\s\S]*?advanceMultiwanAutotuneItem\(\)/,
	'a successful Multi-WAN Apply must refresh authoritative UCI before advancing');
assert.match(source,
	/nativeMobileDownloadBypassRequested\(proposal\)[\s\S]*?bypass_download_unavailable[\s\S]*?Unavailable for this run/,
	'explicit mobile access must keep a visible disabled download-bypass card when evidence is unavailable');
assert.doesNotMatch(source, /Rust calibration|Rust preserved|Rust did not infer/,
	'customer-facing Auto-Tune diagnostics must describe the work, not its implementation language');
for (const [index, button] of source.split("E('button', {").slice(1).entries()) {
	assert.match(button.slice(0, 180), /'type': 'button'/,
		`custom settings button ${index + 1} must not submit the surrounding LuCI form`);
}
const written = {};
const fixtureSections = { 'cake-autorate': [], mwan3: [] };
if (typeof String.prototype.format !== 'function') {
	String.prototype.format = function() {
		let index = 0;
		const args = arguments;
		return this.replace(/%[sd]/g, () => String(args[index++]));
	};
}
const uci = {
	set(config, section, key, value) {
		assert.equal(config, 'cake-autorate');
		written[key] = value;
	},
	unset(config, section, key) {
		assert.equal(config, 'cake-autorate');
		delete written[key];
	},
	get() {
		return null;
	},
	sections(config) {
		return fixtureSections[config] || [];
	},
};
function compileHelpers(fsImpl, uciImpl, lImpl, rpcImpl, eImpl) {
	return new Function(
		'fs', 'form', 'network', 'uci', 'ui', 'widgets', 'cakeUi', 'rpc', 'L', 'E', '_',
		`${prefix}\ninterfaceContext = { deviceNames: { eth1: true }, deviceNetworks: {}, ` +
			`networkDevices: {}, defaultDevice: 'eth1' };\nreturn { writeWizardConfig, validateTransportProbeUrl, parseExecJson, ` +
			`buildInterfaceContext, buildMwan3Context, uniqueMwan3Uplinks, managedUplinkOwner, managedTargetOwner, availableMwan3Uplinks, ` +
			`targetInterfaceChoices, targetInterfaceChoiceOptions, defaultWizardTarget, ` +
			`multiwanInstancePlans, wizardPlanConflicts, wizardSingleTargetConflicts, ` +
			`topicTab, autorateSubcategory, autorateSubcategoryDefinitions, adaptiveCeilingStatusText, ` +
			`formOrUci, accessMediumDefinitions, accessMediumTitle, accessMediumExplorationPercent, detectAccessMedium, resolvedAccessContext, ` +
			`recommendedCapacityLearningPolicy, canonicalCapacityLearningPolicy, autotuneAccessRequest, ` +
			`canonicalAutotuneProfile, autotuneProfileDefinitions, ` +
			`nativeAutotunePublicResultValidated, nativeAutotuneAcknowledgementLabel, nativeMobileDownloadBypassRequested, nativeDownloadBypassUnavailableReason, renderNativeAutotuneDiagnostics, nativeAutotuneApplyCheckValidated, nativeAutotuneApplyRetryableRpcError, ` +
			`nativeAutotuneApplyReceiptValidated, nativeAutotuneApplyHandleValidated, nativeAutotuneApplyStatusValidated, runNativeAutotuneApplyCheck, runNativeAutotuneApply, reloadAppliedUciPackages, reloadAppliedSettingsPage, ` +
			`visibleAutotuneProfile, autotuneRunProfile, storedAutotuneProfile, ` +
			`autotuneHasTrustedCapacityReferences, autotuneCalibrationStrategy, ` +
			`autotuneConservativeAvailable, ` +
			`multiwanAutotuneItemNativeApplied, multiwanAutotuneItemAccepted, ` +
			`multiwanAutotuneItemDecided, multiwanAutotuneBatchDecided, ` +
			`multiwanAutotunePendingPlans, ` +
			`multiwanAutotuneItemCanSkip, ` +
			`autotuneTypedTerminalDiagnostic, ` +
			`discardStagedUciPackages, changedUciPackages, ` +
			`requireCleanUciTransaction, applyPlainRollbackTransaction, ` +
			`clearAutotuneProposalState, recordAutotuneTerminalFailure, ` +
			`autotuneRetryableInconclusive, autotuneMeasurementTimeout, recordAutotuneRetryableInconclusive, ` +
			`manualSqmDirectionMode, validateManualSqmDirectionMode, writeManualSqmDirectionMode, ` +
			`positiveRateValue, shouldImportInterfaceRates, applyRatePreset, ` +
			`runSpeedtestJob, nativeEffectiveSpeedtestBackend, nativeSpeedtestCapabilityValidated, nativeSpeedtestIntentSupported, nativeSpeedtestLaunchArgs, nativeSpeedtestResultValidated, nativeOperationWorkerRunId, nativeSpeedtestStatusMatchesRequest, ` +
			`nativeAutotuneCapabilityValidated, nativeBootstrapAutotuneCapabilityValidated, ` +
			`nativeAutotuneIntentSupported, nativeAutotuneLaunchArgs, nativeAutotuneResultMatchesRequest, nativeAutotuneStatusMatchesRequest, ` +
			`nativeAutotuneProgressStepLabel, nativeAutotuneProgress, ` +
			`currentActiveNativeAutotuneJob, runPreferredAutotuneJob, cancelPreferredAutotuneJob, ` +
			`replaceNodeContent, ` +
			`setNativeAutotuneJob: function(section, jobId, request, workerRunId) { nativeAutotuneJobs[section] = { job_id: jobId, request: request, worker_run_id: workerRunId || null }; }, ` +
			`setInterfaceContext: function(value) { interfaceContext = value; }, ` +
			`setMwan3Context: function(value) { mwan3Context = value; } };`
	)(fsImpl || {}, {}, {}, uciImpl || uci, {}, {}, {
		text: value => value,
		// These tests mock the parsed readout API; the separate transport suite
		// covers CGI limits, partial JSON and the read-only command allowlist.
		readNativeResult: args => (fsImpl || {}).exec('/usr/sbin/cake-autorated', args)
			.then(result => {
				if (result.code != null && result.code !== 0)
					throw new Error(result.stderr || 'readout failed');
				return JSON.parse(result.stdout);
			})
	}, rpcImpl || {
		declare() { return () => Promise.resolve(0); },
	}, lImpl || {}, eImpl || (() => ({})), value => value);
}

const helpers = compileHelpers({});
assert.equal(helpers.autorateSubcategory('rates', '_adaptive_ceiling_status'), 'ceiling',
	'the always-visible adaptive-ceiling status must stay in the ceiling subpanel');
assert.match(helpers.adaptiveCeilingStatusText('best_overall', 'verified_only'),
	/Select Variable Link/,
	'non-Variable profiles must explain why adaptive-ceiling controls are unavailable');
assert.match(helpers.adaptiveCeilingStatusText('variable_link', 'passive_bounded'),
	/real traffic/,
	'Variable Link must describe its exact active runtime learning policy');
assert.match(source, /_adaptive_ceiling_status[\s\S]*?Adaptive ceiling status/,
	'the Edit form must always render an adaptive-ceiling availability/status card');
assert.equal(helpers.nativeMobileDownloadBypassRequested({
	access: { medium: 'cellular', source: 'user_selected' },
}), true);
assert.equal(helpers.nativeMobileDownloadBypassRequested({
	access: { medium: 'cellular', source: 'network_protocol' },
}), false, 'an inferred mobile medium must not reserve a mandatory manual bypass card');
assert.equal(helpers.nativeMobileDownloadBypassRequested({
	access: { medium: 'shared_wired', source: 'user_selected' },
}), false);
assert.equal(helpers.nativeMobileDownloadBypassRequested({
	access_medium: 'cellular', access_source: 'user_selected',
}), false, 'the UI must consume the nested public proposal access contract');
assert.match(helpers.nativeDownloadBypassUnavailableReason({
	download: { reason: 'traffic-budget-limited' },
}), /traffic budget/);
assert.match(helpers.nativeDownloadBypassUnavailableReason({
	download: { reason: 'comparison-not-requested' },
}), /Full raw capacity/);
assert.match(helpers.nativeDownloadBypassUnavailableReason({
	download: { reason: 'future-backend-reason' },
}), /future-backend-reason/,
	'an unknown backend reason must remain visible instead of collapsing to a generic message');
assert.match(helpers.nativeDownloadBypassUnavailableReason(null, {
	mobile_download_bypass: { available: false, reason: 'traffic_budget' },
}), /traffic allowance/,
	'the UI must expose the exact bounded reason why the mobile terminal control did not run');
assert.match(helpers.nativeDownloadBypassUnavailableReason(null, {
	mobile_download_bypass: { available: true, manual_apply_eligible: false },
}), /diagnostic only/,
	'a measured but unsafe mobile control must not be mislabeled as missing upload evidence');
const replacementA = { id: 'a' };
const replacementB = { id: 'b' };
const replaceTarget = {
	children: null,
	replaceChildren() {
		this.children = Array.from(arguments);
	},
	removeChild() {
		throw new Error('replaceNodeContent used the re-entrant removeChild path');
	}
};
helpers.replaceNodeContent(replaceTarget, [ replacementA, replacementB ]);
assert.deepEqual(replaceTarget.children, [ replacementA, replacementB ],
	'wizard re-render must use one native replacement instead of a re-entrant removeChild loop');
const legacyOldChild = { parentNode: null };
const legacyTarget = {
	children: [ legacyOldChild ],
	get firstChild() {
		return this.children[0] || null;
	},
	removeChild(child) {
		assert.equal(child, legacyOldChild);
		this.children.shift();
		child.parentNode = null;
		const error = new Error('removed by nested blur render');
		error.name = 'NotFoundError';
		throw error;
	},
	appendChild(child) {
		this.children.push(child);
		child.parentNode = this;
	}
};
legacyOldChild.parentNode = legacyTarget;
helpers.replaceNodeContent(legacyTarget, replacementA);
assert.deepEqual(legacyTarget.children, [ replacementA ],
	'legacy fallback must ignore only a proven stale-child NotFoundError and finish replacement');
const unexpectedError = new Error('unexpected DOM failure');
unexpectedError.name = 'HierarchyRequestError';
const brokenLegacyTarget = {
	firstChild: { parentNode: null },
	removeChild() { throw unexpectedError; },
	appendChild() {}
};
assert.throws(() => helpers.replaceNodeContent(brokenLegacyTarget, []),
	error => error === unexpectedError,
	'legacy fallback must not hide DOM errors other than a proven stale-child NotFoundError');
assert.deepEqual(helpers.parseExecJson({ code: 0, stdout: '{"state":"ok"}', stderr: '' }),
	{ state: 'ok' });
assert.throws(() => helpers.parseExecJson({ code: 1, stdout: '', stderr: 'ERROR: target is already managed\nignored' }),
	/target is already managed/);
assert.throws(() => helpers.parseExecJson({ code: 1, stdout: '{"state":"ok"}', stderr: '' }),
	/failed without a usable diagnostic/,
	'a non-zero native command must never promote a partial success payload');
assert.throws(() => helpers.parseExecJson({ code: 0, stdout: '', stderr: '' }),
	/returned no JSON result/);
assert.throws(() => helpers.parseExecJson({ code: 0, stdout: 'not-json', stderr: '' }),
	/returned malformed JSON/);
let boundedExecError;
try {
	helpers.parseExecJson({ code: 2, stdout: '', stderr: `ERROR:\u0001${'x'.repeat(400)}` });
} catch (error) {
	boundedExecError = error;
}
assert.ok(boundedExecError);
assert.ok(boundedExecError.message.length <= 240);
assert.doesNotMatch(boundedExecError.message, /[\x00-\x1f\x7f]/,
	'native stderr exposed to LuCI must be bounded and control-free');

function nativePublicFixture() {
	const names = [ 'proposal', 'download_search', 'upload_search',
		'pair_confirmation', 'topology_comparison' ];
	const schemas = [ 4, 4, 4, 6, 2 ];
	const artifacts = {};
	for (let i = 0; i < names.length; i++)
		artifacts[names[i]] = { sha256: String(i + 1).repeat(64), value: { schema_version: schemas[i] } };
	artifacts.proposal.value = {
		schema_version: 4,
		profile: 'variable_link',
		access: { medium: 'cellular', source: 'user_selected', confidence_percent: 100 },
		download: { base_kbps: 904900 },
		upload: { base_kbps: 915600 },
	};
	artifacts.download_search.value = {
		schema_version: 4, profile: 'variable_link', direction: 'download',
		action: 'complete', reason: 'latency-knee-confirmed',
		selected: {
			index: 1, candidate_kbps: 904900, transport_censored: false,
			manual_reviewable: true, safety_pass: true, target_met: true,
		},
		evaluated: [ {
			index: 1, candidate_kbps: 904900, transport_censored: false,
			measurement_reliable: true, manual_reviewable: true,
			safety_pass: true, target_met: true,
		} ],
		review_options: [ {
			role: 'recommended', observation_index: 1, candidate_kbps: 904900,
			transport_censored: false, manual_reviewable: true,
			target_met: true, auto_apply_candidate: true,
		} ],
	};
	artifacts.upload_search.value = {
		schema_version: 4, profile: 'variable_link', direction: 'upload',
		action: 'complete', reason: 'latency-knee-confirmed',
		selected: {
			index: 1, candidate_kbps: 915600, transport_censored: false,
			manual_reviewable: true, safety_pass: true, target_met: true,
		},
		evaluated: [ {
			index: 1, candidate_kbps: 915600, transport_censored: false,
			measurement_reliable: true, manual_reviewable: true,
			safety_pass: true, target_met: true,
		} ],
		review_options: [ {
			role: 'recommended', observation_index: 1, candidate_kbps: 915600,
			transport_censored: false, manual_reviewable: true,
			target_met: true, auto_apply_candidate: true,
		} ],
	};
	artifacts.pair_confirmation.value = {
		schema_version: 6,
		topology: 'both_shaped',
		target_rates_kbps: { download: 904900, upload: 915600 },
		achieved_kbps: { download: 839356, upload: 851212 },
		transport_censored: false,
		transport_timeout_count: 0,
		transport_timeout_total_us: 0,
		measurement_reliable: true,
		safety_pass: true,
		auto_apply_pass: false,
		options: [
			{
				option_id: 'recommended',
				target_rates_kbps: { download: 904900, upload: 915600 },
				achieved_kbps: { download: 839356, upload: 851212 },
				transport_censored: false, transport_timeout_count: 0,
				transport_timeout_total_us: 0, measurement_reliable: true,
				safety_pass: true, auto_apply_pass: false,
				physical_capacity_limited_review: { download: false, upload: false },
				physical_capacity_alignment_confirmed: { download: false, upload: false },
				manual_apply_eligible: true, manual_review_required: true,
				validation: { actual_grade: 'B' },
			},
			{
				option_id: 'quality_first',
				target_rates_kbps: { download: 710000, upload: 720000 },
				achieved_kbps: { download: 700000, upload: 710000 },
				transport_censored: false, transport_timeout_count: 0,
				transport_timeout_total_us: 0, measurement_reliable: true,
				safety_pass: true, auto_apply_pass: true,
				physical_capacity_limited_review: { download: false, upload: false },
				physical_capacity_alignment_confirmed: { download: false, upload: false },
				manual_apply_eligible: true, manual_review_required: false,
				validation: { actual_grade: 'A' },
			},
		],
		unavailable_options: [],
	};
	artifacts.topology_comparison.value = {
		schema_version: 2,
		selected_topology: 'upload_only_shaped',
		selected_rates_kbps: { download: null, upload: 915600 },
		auto_apply_pass: false,
		manual_review_required: true,
		download: {
			choice: 'unshaped',
			shaped: { achieved_kbps: 839356, grade: 'B', transport_censored: false },
			unshaped: {
				achieved_kbps: 900000, grade: 'B', transport_censored: false,
				target_met: true, measurement_reliable: true, safety_pass: true,
			},
		},
		upload: {
			choice: 'shaped',
			shaped: { achieved_kbps: 851212, grade: 'B', transport_censored: false },
			unshaped: {
				achieved_kbps: null, grade: null, transport_censored: false,
				target_met: false, measurement_reliable: false, safety_pass: false,
			},
		},
	};
	return {
		native_public_schema_version: 3,
		state: 'review_ready',
		producer: 'cake-autorated-native-autotune',
		source_review_sha256: 'a'.repeat(64),
		public_apply_contract: {
			schema_version: 3,
			state: 'selection_ready',
			executor_available: true,
			explicit_confirmation_required: true,
			native_job_id: 'b'.repeat(32),
			worker_run_id: 'c'.repeat(32),
			source_review_sha256: 'a'.repeat(64),
			selection_contract: 'option_id_plus_review_and_manifest_digests_and_acknowledgements',
			options: [
				{
					option_id: 'recommended', preferred: false,
					manifest_sha256: '8'.repeat(64),
					selected_topology: 'both_shaped', action: 'apply_sqm',
					sqm_direction_mode: 'both',
					target_rates_kbps: { download: 904900, upload: 915600 },
					auto_apply_evidence_pass: false, manual_review_required: true,
					required_acknowledgements: [ 'download-capacity-retention' ],
				},
				{
					option_id: 'quality_first', preferred: false,
					manifest_sha256: '7'.repeat(64),
					selected_topology: 'both_shaped', action: 'apply_sqm',
					sqm_direction_mode: 'both',
					target_rates_kbps: { download: 710000, upload: 720000 },
					auto_apply_evidence_pass: true, manual_review_required: false,
					required_acknowledgements: [],
				},
				{
					option_id: 'bypass_download', preferred: true,
					manifest_sha256: '9'.repeat(64),
					selected_topology: 'upload_only_shaped', action: 'apply_sqm',
					sqm_direction_mode: 'upload_only',
					target_rates_kbps: { download: null, upload: 915600 },
					auto_apply_evidence_pass: false, manual_review_required: true,
					required_acknowledgements: [ 'download-shaping-bypassed' ],
				},
			],
		},
		auto_apply_eligible: false,
		manual_apply_eligible: true,
		configuration_written: false,
		runtime_restored: true,
		recovery_pending: false,
		throughput_unit: 'kbit/s',
		proposal_rate_transform: 'none',
		native_job_id: 'b'.repeat(32),
		job_id: 'wan_sqm',
		run_id: 'c'.repeat(32),
		target_interface: 'pppoe-wan',
		resolved_interface: 'pppoe-wan',
		route_mode: 'main',
		mwan3_member: null,
		source_ip: '192.0.2.10',
		route_fingerprint: `sha256:${'d'.repeat(64)}`,
		config_fingerprint: `sha256:${'e'.repeat(64)}`,
		sqm_fingerprint: `sha256:${'f'.repeat(64)}`,
		profile: 'variable_link',
		calibration_strategy: 'full_raw',
		consumed_traffic_bytes: 17741550972,
		artifacts,
	};
}

function testUnavailableMobileDownloadBypassCardRenders() {
	const result = nativePublicFixture();
	result.public_apply_contract.options = result.public_apply_contract.options.slice(0, 2);
	result.public_apply_contract.options[0].preferred = true;
	result.artifacts.topology_comparison.value.selected_topology = 'both_shaped';
	result.artifacts.topology_comparison.value.selected_rates_kbps = {
		download: 904900, upload: 915600,
	};
	result.artifacts.topology_comparison.value.download.choice = 'shaped';
	result.artifacts.topology_comparison.value.download.reason = 'inconclusive-raw-evidence';
	result.artifacts.topology_comparison.value.upload.choice = 'shaped';
	assert.equal(helpers.nativeAutotunePublicResultValidated(result), true,
		'the unavailable-card fixture must remain a valid public result');

	function element(tag, attrs, children) {
		return {
			tag,
			attrs: attrs || {},
			children: Array.isArray(children) ? children : children == null ? [] : [ children ],
			replaceChildren() { this.children = Array.from(arguments); },
		};
	}
	function find(node, predicate) {
		if (!node || typeof node !== 'object')
			return null;
		if (predicate(node))
			return node;
		for (const child of node.children || []) {
			const match = find(child, predicate);
			if (match)
				return match;
		}
		return null;
	}
	const renderHelpers = compileHelpers({}, null, null, null, element);
	const rendered = renderHelpers.renderNativeAutotuneDiagnostics(result);
	const card = find(rendered, node =>
		node.attrs && node.attrs['data-option-id'] === 'bypass_download_unavailable');
	assert.ok(card, 'the disabled mobile download-bypass card must be in the rendered tree');
	assert.equal(card.attrs['aria-disabled'], 'true');
	assert.match(JSON.stringify(card), /noisy or incomplete/);
}

testUnavailableMobileDownloadBypassCardRenders();

function nativeRawFallbackPublicFixture() {
	const result = nativePublicFixture();
	const acknowledgements = [
		'download-shaping-bypassed',
		'upload-shaping-bypassed',
		'sqm-disabled',
	];
	const sample = (topology, achieved, delta) => ({
		topology,
		achieved_kbps: achieved,
		effective_delta_ms: delta,
		grade: 'A',
		transport_censored: false,
		loss_ppm: 0,
		cpu_milli_percent: 1200,
		background_confidence_percent: 95,
		contaminated: false,
	});
	const direction = (name, topologies, rates, deltas) => ({
		direction: name,
		sample_count: 2,
		representative_achieved_kbps: Math.min(rates[0], rates[1]),
		effective_delta_ms: Math.max(deltas[0], deltas[1]),
		grade: 'A',
		rate_consistent: true,
		target_status_consistent: true,
		target_met: true,
		measurement_reliable: true,
		contaminated: false,
		safety_pass: true,
		samples: [
			sample(topologies[0], rates[0], deltas[0]),
			sample(topologies[1], rates[1], deltas[1]),
		],
	});
	result.native_public_schema_version = 4;
	result.artifacts = {
		proposal: result.artifacts.proposal,
		raw_fallback: {
			sha256: '6'.repeat(64),
			value: {
				schema_version: 1,
				selected_topology: 'no_sqm',
				reason: 'incomplete-shaped-search',
				failed_direction: 'download',
				unobserved_candidates_kbps: [ 904900, 724000, 543000 ],
				discarded_shaped_observation_count: 0,
				target_grade: 'B',
				auto_apply_pass: false,
				manual_review_required: true,
				download: direction('download', [ 'download_unshaped', 'no_sqm' ],
					[ 920000, 910000 ], [ 8.25, 9.5 ]),
				upload: direction('upload', [ 'upload_unshaped', 'no_sqm' ],
					[ 100000, 98000 ], [ 7.75, 9.5 ]),
				required_acknowledgements: acknowledgements.slice(),
			},
		},
	};
	result.public_apply_contract.options = [ {
		option_id: 'no_sqm',
		preferred: true,
		manifest_sha256: '9'.repeat(64),
		selected_topology: 'no_sqm',
		action: 'disable_sqm',
		sqm_direction_mode: 'off',
		target_rates_kbps: { download: null, upload: null },
		auto_apply_evidence_pass: false,
		manual_review_required: true,
		required_acknowledgements: acknowledgements.slice(),
	} ];
	return result;
}

function nativeCensoredPublicFixture() {
	const result = nativePublicFixture();
	const pair = result.artifacts.pair_confirmation.value;
	const topology = result.artifacts.topology_comparison.value;
	const recommended = result.public_apply_contract.options[0];

	[ result.artifacts.download_search.value,
		result.artifacts.upload_search.value ].forEach(search => {
		search.action = 'fallback';
		search.reason = 'transport-deadline-censored-review';
		Object.assign(search.selected, {
			transport_censored: true, manual_reviewable: true,
			safety_pass: false, target_met: false,
		});
		Object.assign(search.evaluated[0], {
			transport_censored: true, measurement_reliable: false,
			manual_reviewable: true, safety_pass: false, target_met: false,
		});
		Object.assign(search.review_options[0], {
			transport_censored: true, manual_reviewable: true,
			target_met: false, auto_apply_candidate: false,
		});
	});
	Object.assign(pair, {
		transport_censored: true,
		transport_timeout_count: 6,
		transport_timeout_total_us: 30000000,
		measurement_reliable: false,
		safety_pass: false,
		auto_apply_pass: false,
	});
	Object.assign(pair.options[0], {
		transport_censored: true,
		transport_timeout_count: 6,
		transport_timeout_total_us: 30000000,
		measurement_reliable: false,
		safety_pass: false,
		auto_apply_pass: false,
		manual_apply_eligible: true,
		manual_review_required: true,
	});
	result.public_apply_contract.options.forEach(option => { option.preferred = false; });
	recommended.preferred = true;
	recommended.required_acknowledgements = [
		'download-capacity-retention', 'measurement-confidence',
	];
	result.public_apply_contract.options[2].required_acknowledgements.push(
		'measurement-confidence');
	topology.selected_topology = 'both_shaped';
	topology.selected_rates_kbps = { download: 904900, upload: 915600 };
	topology.download.choice = 'shaped';
	topology.upload.choice = 'shaped';
	topology.download.shaped.transport_censored = true;
	topology.upload.shaped.transport_censored = true;
	topology.auto_apply_pass = false;
	topology.manual_review_required = true;
	return result;
}

const nativePublic = nativePublicFixture();
Object.defineProperty(nativePublic, '_native_target_state', {
	value: 'existing_managed', enumerable: false,
});
assert.equal(helpers.nativeAutotunePublicResultValidated(nativePublic), true,
	'a digest-bound diagnostic native Review must pass its isolated public contract');
const noisyCanonicalPublic = nativePublicFixture();
const noisyDownload = noisyCanonicalPublic.artifacts.download_search.value;
noisyDownload.action = 'fallback';
noisyDownload.reason = 'noisy-link-safe-review';
noisyDownload.evaluated.push(
	{
		index: 2, candidate_kbps: 80800, transport_censored: false,
		measurement_reliable: true, manual_reviewable: false,
		safety_pass: true, target_met: false,
	},
	{
		index: 3, candidate_kbps: 80800, transport_censored: false,
		measurement_reliable: true, manual_reviewable: false,
		safety_pass: true, target_met: false,
	},
	{
		index: 4, candidate_kbps: 80800, transport_censored: false,
		measurement_reliable: true, manual_reviewable: false,
		safety_pass: true, target_met: false,
	});
assert.equal(helpers.nativeAutotunePublicResultValidated(noisyCanonicalPublic), true,
	'a noisy repeated candidate must remain valid when selected and recommended share the conservative frontier candidate');
const noisyDivergentPublic = JSON.parse(JSON.stringify(noisyCanonicalPublic));
Object.assign(noisyDivergentPublic.artifacts.download_search.value.selected, {
	index: 4, candidate_kbps: 80800,
});
assert.equal(helpers.nativeAutotunePublicResultValidated(noisyDivergentPublic), false,
	'the browser must reject a lucky noisy repeat selected outside the conservative recommended frontier');
assert.equal(nativePublic.artifacts.proposal.value.download.base_kbps, 904900,
	'the native public contract must preserve the exact download proposal rate');
assert.equal(nativePublic.artifacts.proposal.value.upload.base_kbps, 915600,
	'the native public contract must preserve the exact upload proposal rate');
assert.equal(nativePublic.manual_apply_eligible, true,
	'the confirmation contract must expose the crash-safe native executor');
const nativeTrafficBudgetLimited = nativePublicFixture();
const limitedTopology = nativeTrafficBudgetLimited.artifacts.topology_comparison.value;
limitedTopology.selected_topology = 'both_shaped';
limitedTopology.selected_rates_kbps = { download: 904900, upload: 915600 };
limitedTopology.download.choice = 'shaped';
limitedTopology.download.reason = 'shaped-quality-preferred';
limitedTopology.upload.reason = 'traffic-budget-limited';
Object.assign(limitedTopology.upload.unshaped, {
	needs_repeat: false,
	retry_exhausted: true,
});
limitedTopology.auto_apply_pass = false;
limitedTopology.manual_review_required = true;
nativeTrafficBudgetLimited.public_apply_contract.options.splice(2, 1);
nativeTrafficBudgetLimited.public_apply_contract.options[0].preferred = true;
for (const option of nativeTrafficBudgetLimited.public_apply_contract.options) {
	option.auto_apply_evidence_pass = false;
	option.manual_review_required = true;
	option.required_acknowledgements.push('topology-comparison-traffic-budget');
}
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeTrafficBudgetLimited), true,
	'a conservative raw-repeat budget limit must preserve every verified shaped option as manual Review');
assert.match(helpers.nativeAutotuneAcknowledgementLabel('topology-comparison-traffic-budget'),
	/fully verified shaped proposal/,
	'the explicit confirmation must explain that no raw result was inferred');
const nativeTrafficBudgetMissingAcknowledgement = JSON.parse(
	JSON.stringify(nativeTrafficBudgetLimited));
nativeTrafficBudgetMissingAcknowledgement.public_apply_contract.options[1]
	.required_acknowledgements.pop();
assert.equal(helpers.nativeAutotunePublicResultValidated(
	nativeTrafficBudgetMissingAcknowledgement), false,
	'every shaped option must retain the topology traffic-budget acknowledgement');
const nativeTrafficBudgetInventedAcknowledgement = nativePublicFixture();
nativeTrafficBudgetInventedAcknowledgement.public_apply_contract.options[0]
	.required_acknowledgements.push('topology-comparison-traffic-budget');
assert.equal(helpers.nativeAutotunePublicResultValidated(
	nativeTrafficBudgetInventedAcknowledgement), false,
	'the browser must reject a traffic-budget acknowledgement without matching Rust evidence');
const nativeTrafficBudgetAutoApply = JSON.parse(JSON.stringify(nativeTrafficBudgetLimited));
nativeTrafficBudgetAutoApply.artifacts.topology_comparison.value.auto_apply_pass = true;
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeTrafficBudgetAutoApply), false,
	'a budget-limited optional comparison can never be relabelled for Auto-Apply');
const nativeTrafficBudgetWithoutManualReview = JSON.parse(
	JSON.stringify(nativeTrafficBudgetLimited));
nativeTrafficBudgetWithoutManualReview.public_apply_contract.options[0]
	.manual_review_required = false;
assert.equal(helpers.nativeAutotunePublicResultValidated(
	nativeTrafficBudgetWithoutManualReview), false,
	'a budget-limited shaped option must never omit explicit manual Review');
const nativeTopologyUnmeasurable = nativePublicFixture();
const unmeasurableTopology = nativeTopologyUnmeasurable.artifacts.topology_comparison.value;
unmeasurableTopology.upload.reason = 'comparison-unmeasurable';
Object.assign(unmeasurableTopology.upload.unshaped, {
	achieved_kbps: null, grade: null, target_met: false,
	measurement_reliable: false, safety_pass: false,
	needs_repeat: false, retry_exhausted: true,
});
unmeasurableTopology.auto_apply_pass = false;
unmeasurableTopology.manual_review_required = true;
for (const option of nativeTopologyUnmeasurable.public_apply_contract.options) {
	option.auto_apply_evidence_pass = false;
	option.manual_review_required = true;
	option.required_acknowledgements.push('topology-comparison-unmeasurable');
}
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeTopologyUnmeasurable), true,
	'an unmeasurable optional raw direction must retain exact shaped options for manual Review');
assert.match(helpers.nativeAutotuneAcknowledgementLabel('topology-comparison-unmeasurable'),
	/fully verified shaped evidence/,
	'the aggregate confirmation must explain that no raw result was inferred');
const nativeTopologyUnmeasurableMissingAck = JSON.parse(
	JSON.stringify(nativeTopologyUnmeasurable));
nativeTopologyUnmeasurableMissingAck.public_apply_contract.options[0]
	.required_acknowledgements.pop();
assert.equal(helpers.nativeAutotunePublicResultValidated(
	nativeTopologyUnmeasurableMissingAck), false,
	'every retained option must acknowledge the unavailable optional comparison');
const nativeTopologyUnmeasurableInventedAck = nativePublicFixture();
nativeTopologyUnmeasurableInventedAck.public_apply_contract.options[0]
	.required_acknowledgements.push('topology-comparison-unmeasurable');
assert.equal(helpers.nativeAutotunePublicResultValidated(
	nativeTopologyUnmeasurableInventedAck), false,
	'the browser must reject an unavailable-comparison acknowledgement without Rust evidence');
const nativePartialPair = nativePublicFixture();
nativePartialPair.artifacts.pair_confirmation.value.unavailable_options = [ {
	target_rates_kbps: { download: 600000, upload: 610000 },
	failed_direction: 'download', reason: 'observation_starved', run_count: 3,
	traffic_debit_count: { download: 3, upload: 0 },
	samples: { icmp: 273, transport: 5, cpu: 3 },
} ];
nativePartialPair.artifacts.pair_confirmation.value.options.pop();
nativePartialPair.public_apply_contract.options.splice(1, 1);
assert.equal(helpers.nativeAutotunePublicResultValidated(nativePartialPair), true,
	'a valid primary pair must remain selectable when one optional pair is unavailable');
const nativePartialPairDuplicate = JSON.parse(JSON.stringify(nativePartialPair));
nativePartialPairDuplicate.artifacts.pair_confirmation.value.unavailable_options[0]
	.target_rates_kbps = { download: 904900, upload: 915600 };
assert.equal(helpers.nativeAutotunePublicResultValidated(nativePartialPairDuplicate), false,
	'an unavailable pair cannot duplicate a selectable exact rate pair');
const nativePartialPairImpossibleDebits = JSON.parse(JSON.stringify(nativePartialPair));
nativePartialPairImpossibleDebits.artifacts.pair_confirmation.value.unavailable_options[0]
	.traffic_debit_count.upload = 1;
assert.equal(helpers.nativeAutotunePublicResultValidated(nativePartialPairImpossibleDebits), false,
	'a failed download pair must not claim upload traffic debits');
const nativePartialPairCompleteObservation = JSON.parse(JSON.stringify(nativePartialPair));
nativePartialPairCompleteObservation.artifacts.pair_confirmation.value.unavailable_options[0]
	.samples = { icmp: 15, transport: 15, cpu: 1 };
assert.equal(helpers.nativeAutotunePublicResultValidated(
	nativePartialPairCompleteObservation), false,
	'a complete pair observation cannot be relabelled as starved');
const nativeRawFallbackPublic = nativeRawFallbackPublicFixture();
Object.defineProperty(nativeRawFallbackPublic, '_native_target_state', {
	value: 'absent_bootstrap', enumerable: false,
});
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeRawFallbackPublic), true,
	'a raw-only Review must expose one exact manual SQM-off option');

function nativeShapedCapacityFallbackPublicFixture() {
	const result = nativeRawFallbackPublicFixture();
	const acknowledgements = [
		'loaded-latency-unobservable', 'shaped-validation-incomplete' ];
	result.native_public_schema_version = 6;
	result.artifacts.raw_fallback.value = {
		schema_version: 6,
		selected_topology: 'both_shaped',
		reason: 'loaded-latency-unobservable',
		failed_direction: 'download',
		terminal_boundary: {
			kind: 'unobserved_floor_exhaustion', candidate_kbps: 892100,
		},
		selected_rates_kbps: { download: 892100, upload: 910000 },
		raw_capacity_kbps: {
			download: [ 883958, 892982 ], upload: [ 907001, 907908 ],
		},
		latency_grade: null,
		auto_apply_pass: false,
		manual_review_required: true,
		adaptive_ceiling: {
			download: { safe_kbps: 0, cap_kbps: 892100, evidence: 'legacy_unverified' },
			upload: { safe_kbps: 0, cap_kbps: 910000, evidence: 'legacy_unverified' },
		},
		required_acknowledgements: acknowledgements.slice(),
	};
	result.public_apply_contract.options = [ {
		option_id: 'capacity_only_shaped',
		preferred: true,
		manifest_sha256: '7'.repeat(64),
		selected_topology: 'both_shaped',
		action: 'apply_sqm',
		sqm_direction_mode: 'both',
		target_rates_kbps: { download: 892100, upload: 910000 },
		auto_apply_evidence_pass: false,
		manual_review_required: true,
		required_acknowledgements: acknowledgements.slice(),
	} ];
	return result;
}

const nativeShapedCapacityFallback = nativeShapedCapacityFallbackPublicFixture();
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeShapedCapacityFallback), true,
	'capacity-only evidence must expose one manual hard-capped both-shaped option');
assert.match(helpers.nativeAutotuneAcknowledgementLabel('loaded-latency-unobservable'),
	/no latency class is claimed/i);
assert.match(helpers.nativeAutotuneAcknowledgementLabel('shaped-validation-incomplete'),
	/cannot grow automatically/i);
const nativeShapedCapacityUnsafeGrowth = JSON.parse(
	JSON.stringify(nativeShapedCapacityFallback));
nativeShapedCapacityUnsafeGrowth.artifacts.raw_fallback.value
	.adaptive_ceiling.download.cap_kbps += 1;
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeShapedCapacityUnsafeGrowth), false,
	'capacity-only Review cannot raise its adaptive cap above the selected rate');
const nativeShapedCapacitySafeClaim = JSON.parse(JSON.stringify(nativeShapedCapacityFallback));
nativeShapedCapacitySafeClaim.artifacts.raw_fallback.value
	.adaptive_ceiling.download.safe_kbps = 892100;
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeShapedCapacitySafeClaim), false,
	'capacity-only Review cannot forge a tested-safe rate');
const nativeShapedCapacityMissingAck = JSON.parse(JSON.stringify(nativeShapedCapacityFallback));
nativeShapedCapacityMissingAck.public_apply_contract.options[0]
	.required_acknowledgements.pop();
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeShapedCapacityMissingAck), false,
	'capacity-only Review requires both exact aggregate acknowledgements');
const nativeShapedCapacityLegacyPublicSchema = JSON.parse(
	JSON.stringify(nativeShapedCapacityFallback));
nativeShapedCapacityLegacyPublicSchema.native_public_schema_version = 5;
assert.equal(helpers.nativeAutotunePublicResultValidated(
	nativeShapedCapacityLegacyPublicSchema), false,
	'a capacity-only artifact cannot be downgraded to public schema 5');
const nativeSchemaSixNoSqm = nativeRawFallbackPublicFixture();
nativeSchemaSixNoSqm.native_public_schema_version = 6;
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeSchemaSixNoSqm), false,
	'public schema 6 is reserved for the exact shaped-capacity option');

function testRawFallbackMobileDownloadBypassUnavailableCardRenders() {
	function element(tag, attrs, children) {
		return {
			tag, attrs: attrs || {},
			children: Array.isArray(children) ? children : children == null ? [] : [ children ],
			replaceChildren() { this.children = Array.from(arguments); },
		};
	}
	function find(node, predicate) {
		if (!node || typeof node !== 'object')
			return null;
		if (predicate(node))
			return node;
		for (const child of node.children || []) {
			const match = find(child, predicate);
			if (match)
				return match;
		}
		return null;
	}
	const result = nativeRawFallbackPublicFixture();
	const renderHelpers = compileHelpers({}, null, null, null, element);
	const rendered = renderHelpers.renderNativeAutotuneDiagnostics(result);
	const card = find(rendered, node =>
		node.attrs && node.attrs['data-option-id'] === 'bypass_download_unavailable');
	assert.ok(card,
		'a selected mobile medium must retain an explicit download-bypass card on raw fallback');
	assert.equal(card.attrs['aria-disabled'], 'true');
	assert.match(JSON.stringify(card), /verified upload-shaped rate/);
	assert.match(JSON.stringify(card), /incomplete-shaped-search/);
}

testRawFallbackMobileDownloadBypassUnavailableCardRenders();
const nativeMeasuredRawFallback = nativeRawFallbackPublicFixture();
Object.assign(nativeMeasuredRawFallback.artifacts.raw_fallback.value, {
	schema_version: 2,
	reason: 'measured-shaped-search-inconclusive',
	terminal_boundary: { kind: 'loaded_observation_starved', candidate_kbps: 724000 },
});
delete nativeMeasuredRawFallback.artifacts.raw_fallback.value.unobserved_candidates_kbps;
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeMeasuredRawFallback), true,
	'a measured directional starvation may expose the exact manual no-SQM fallback');
const nativePairExhaustedRawFallback = nativeRawFallbackPublicFixture();
Object.assign(nativePairExhaustedRawFallback.artifacts.raw_fallback.value, {
	schema_version: 3,
	reason: 'shaped-pair-options-exhausted',
	failed_direction: null,
	terminal_boundary: { kind: 'pair_options_exhausted', candidate_count: 3 },
});
delete nativePairExhaustedRawFallback.artifacts.raw_fallback.value.unobserved_candidates_kbps;
assert.equal(helpers.nativeAutotunePublicResultValidated(nativePairExhaustedRawFallback), true,
	'an exhausted shaped pair set may expose the exact manual no-SQM fallback');
const nativeDirectionalRawFallback = JSON.parse(JSON.stringify(nativePairExhaustedRawFallback));
nativeDirectionalRawFallback.native_public_schema_version = 5;
nativeDirectionalRawFallback.artifacts.raw_fallback.value.schema_version = 5;
nativeDirectionalRawFallback.artifacts.raw_fallback.value.mobile_download_bypass = {
	available: true,
	selected_topology: 'upload_only_shaped',
	selected_ul_kbps: 915600,
	runtime_minimum_ul_kbps: 700000,
	download: {
		achieved_kbps: 920000, effective_delta_ms: 9.5, loss_ppm: 0,
		background_confidence_percent: 95, contaminated: false,
		transport_censored: false,
	},
	upload: {
		achieved_kbps: 900000, realized_kbps: 890000, effective_delta_ms: 8.5,
		candidate_realization_percent: 97.205, capacity_retention_percent: 98.0,
		loss_ppm: 0, background_confidence_percent: 95, contaminated: false,
		transport_censored: false, capacity_alignment_confirmed: false,
	},
	manual_apply_eligible: true,
	required_acknowledgements: [ 'download-shaping-bypassed' ],
};
nativeDirectionalRawFallback.public_apply_contract.options.push({
	option_id: 'bypass_download',
	preferred: false,
	manifest_sha256: '8'.repeat(64),
	selected_topology: 'upload_only_shaped',
	action: 'apply_sqm',
	sqm_direction_mode: 'upload_only',
	target_rates_kbps: { download: null, upload: 915600 },
	auto_apply_evidence_pass: false,
	manual_review_required: true,
	required_acknowledgements: [ 'download-shaping-bypassed' ],
});
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeDirectionalRawFallback), true,
	'a verified mobile raw fallback may expose no-SQM plus one manual upload-only SQM option');

function testDirectionalRawFallbackRendersBothEvidenceCards() {
	function element(tag, attrs, children) {
		return {
			tag, attrs: attrs || {},
			children: Array.isArray(children) ? children : children == null ? [] : [ children ],
			replaceChildren() { this.children = Array.from(arguments); },
		};
	}
	function find(node, predicate) {
		if (!node || typeof node !== 'object')
			return null;
		if (predicate(node))
			return node;
		for (const child of node.children || []) {
			const match = find(child, predicate);
			if (match)
				return match;
		}
		return null;
	}
	const renderHelpers = compileHelpers({}, null, null, null, element);
	const rendered = renderHelpers.renderNativeAutotuneDiagnostics(
		JSON.parse(JSON.stringify(nativeDirectionalRawFallback)));
	const noSqm = find(rendered, node =>
		node.attrs && node.attrs['data-option-id'] === 'no_sqm');
	const bypassDownload = find(rendered, node =>
		node.attrs && node.attrs['data-option-id'] === 'bypass_download');
	assert.ok(noSqm, 'schema-5 raw Review must render its no-SQM option');
	assert.ok(bypassDownload,
		'schema-5 raw Review must render its verified download-bypass option');
	assert.match(JSON.stringify(noSqm), /910000/,
		'no-SQM card must use the exact raw download aggregate');
	assert.match(JSON.stringify(noSqm), /98000/,
		'no-SQM card must use the exact raw upload aggregate');
	assert.match(JSON.stringify(bypassDownload), /920000/,
		'download-bypass card must use its exact terminal download evidence');
	assert.match(JSON.stringify(bypassDownload), /900000/,
		'download-bypass card must use its exact terminal upload evidence');
}

testDirectionalRawFallbackRendersBothEvidenceCards();
const nativeDirectionalPublicWithLegacyArtifact = JSON.parse(
	JSON.stringify(nativeDirectionalRawFallback));
nativeDirectionalPublicWithLegacyArtifact.artifacts.raw_fallback.value.schema_version = 3;
delete nativeDirectionalPublicWithLegacyArtifact.artifacts.raw_fallback.value
	.mobile_download_bypass;
assert.equal(helpers.nativeAutotunePublicResultValidated(
	nativeDirectionalPublicWithLegacyArtifact), false,
	'public schema 5 cannot invent a directional option from a legacy raw artifact');
const nativeLegacyPublicWithDirectionalOption = JSON.parse(
	JSON.stringify(nativeDirectionalRawFallback));
nativeLegacyPublicWithDirectionalOption.native_public_schema_version = 4;
assert.equal(helpers.nativeAutotunePublicResultValidated(
	nativeLegacyPublicWithDirectionalOption), false,
	'public schema 4 must retain its exact one-option compatibility contract');
const nativeDirectionalRawFallbackMissingOption = JSON.parse(
	JSON.stringify(nativeDirectionalRawFallback));
nativeDirectionalRawFallbackMissingOption.public_apply_contract.options.pop();
assert.equal(helpers.nativeAutotunePublicResultValidated(
	nativeDirectionalRawFallbackMissingOption), false,
	'public schema 5 must not hide its evidence-bound directional option');
const nativeDirectionalRawFallbackAckDrift = JSON.parse(
	JSON.stringify(nativeDirectionalRawFallback));
nativeDirectionalRawFallbackAckDrift.public_apply_contract.options[1]
	.required_acknowledgements.push('sqm-disabled');
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeDirectionalRawFallbackAckDrift), false,
	'the directional option acknowledgements must remain exact and cannot disable all SQM');
const nativeUnavailableDirectionalRawFallback = JSON.parse(
	JSON.stringify(nativePairExhaustedRawFallback));
nativeUnavailableDirectionalRawFallback.artifacts.raw_fallback.value.schema_version = 5;
nativeUnavailableDirectionalRawFallback.artifacts.raw_fallback.value.mobile_download_bypass = {
	available: false, selected_topology: 'upload_only_shaped', candidate_ul_kbps: 915600,
	failed_direction: 'download', reason: 'traffic_budget', run_count: 0, debit_count: 0,
	samples: { icmp: 0, transport: 0, cpu: 0 },
};
assert.equal(helpers.nativeAutotunePublicResultValidated(
	nativeUnavailableDirectionalRawFallback), true,
	'a bounded unavailable mobile control must remain visible without inventing a proposal');
const nativeInconclusiveRawFallback = nativeRawFallbackPublicFixture();
Object.assign(nativeInconclusiveRawFallback.artifacts.raw_fallback.value, {
	schema_version: 4,
	reason: 'shaped-search-inconclusive',
	terminal_boundary: {
		kind: 'shaped_search_inconclusive', observation_count: 3,
		optimizer_reason: 'variable-candidate-realization-inconclusive',
	},
});
delete nativeInconclusiveRawFallback.artifacts.raw_fallback.value.unobserved_candidates_kbps;
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeInconclusiveRawFallback), true,
	'a measured optimizer dead end may expose only the exact manual no-SQM fallback');
const nativeInconclusiveWrongReason = JSON.parse(JSON.stringify(nativeInconclusiveRawFallback));
nativeInconclusiveWrongReason.artifacts.raw_fallback.value.terminal_boundary.optimizer_reason =
	'arbitrary-inconclusive';
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeInconclusiveWrongReason), false,
	'the browser must reject an optimizer reason outside the Rust allowlist');
const nativeInconclusiveWrongCount = JSON.parse(JSON.stringify(nativeInconclusiveRawFallback));
nativeInconclusiveWrongCount.artifacts.raw_fallback.value.terminal_boundary.observation_count = 0;
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeInconclusiveWrongCount), false,
	'the browser must reject an empty shaped-search boundary');
const nativePairExhaustedInventedDirection = JSON.parse(
	JSON.stringify(nativePairExhaustedRawFallback));
nativePairExhaustedInventedDirection.artifacts.raw_fallback.value.failed_direction = 'download';
assert.equal(helpers.nativeAutotunePublicResultValidated(
	nativePairExhaustedInventedDirection), false,
	'pair exhaustion must not invent one failed direction');
const nativeRawWithRate = nativeRawFallbackPublicFixture();
nativeRawWithRate.public_apply_contract.options[0].target_rates_kbps.download = 900000;
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeRawWithRate), false,
	'a disabled raw fallback must never invent a shaped target rate');
const nativeRawMissingAck = nativeRawFallbackPublicFixture();
nativeRawMissingAck.public_apply_contract.options[0].required_acknowledgements.pop();
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeRawMissingAck), false,
	'the public raw evidence and exact mandatory acknowledgements must stay bound');
const nativeRawWrongTopology = nativeRawFallbackPublicFixture();
nativeRawWrongTopology.artifacts.raw_fallback.value.download.samples[0].topology = 'shaped_both';
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeRawWrongTopology), false,
	'raw fallback cannot accept shaped evidence as a no-SQM control');
const nativeRawInternalTopologyAlias = nativeRawFallbackPublicFixture();
nativeRawInternalTopologyAlias.artifacts.raw_fallback.value.download.samples[0].topology =
	'raw_download';
nativeRawInternalTopologyAlias.artifacts.raw_fallback.value.download.samples[1].topology =
	'raw_both';
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeRawInternalTopologyAlias), false,
	'raw fallback must use the canonical public topology names emitted by Rust');
const nativeCensoredPublic = nativeCensoredPublicFixture();
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeCensoredPublic), true,
	'a count-and-coverage-bound transport deadline may remain an explicit manual Review');
const nativePhysicalCapacityReview = nativePublicFixture();
Object.assign(nativePhysicalCapacityReview.artifacts.pair_confirmation.value.options[0], {
	physical_capacity_limited_review: { download: true, upload: false },
	physical_capacity_alignment_confirmed: { download: true, upload: false },
});
nativePhysicalCapacityReview.public_apply_contract.options[0].required_acknowledgements = [
	'download-capacity-retention',
	'download-throughput-safety-floor',
	'download-physical-capacity-limited',
];
assert.equal(helpers.nativeAutotunePublicResultValidated(nativePhysicalCapacityReview), true,
	'a repeatable physical-capacity ceiling must remain an explicit manual-only native Review');
const nativePhysicalCapacityWithoutAcknowledgement = JSON.parse(
	JSON.stringify(nativePhysicalCapacityReview));
nativePhysicalCapacityWithoutAcknowledgement.public_apply_contract.options[0]
	.required_acknowledgements.pop();
assert.equal(helpers.nativeAutotunePublicResultValidated(
	nativePhysicalCapacityWithoutAcknowledgement), false,
	'the physical-capacity exception must retain its direction-specific acknowledgement');
const nativeInventedPhysicalCapacityAcknowledgement = nativePublicFixture();
nativeInventedPhysicalCapacityAcknowledgement.public_apply_contract.options[0]
	.required_acknowledgements.push('download-physical-capacity-limited');
assert.equal(helpers.nativeAutotunePublicResultValidated(
	nativeInventedPhysicalCapacityAcknowledgement), false,
	'a physical-capacity acknowledgement must be bound to exact pair evidence');
const nativeUnalignedThroughputOverride = JSON.parse(JSON.stringify(nativePhysicalCapacityReview));
nativeUnalignedThroughputOverride.artifacts.pair_confirmation.value.options[0]
	.physical_capacity_alignment_confirmed.download = false;
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeUnalignedThroughputOverride), false,
	'a sub-floor throughput acknowledgement must require independent wire/goodput alignment');
const nativeAlignedWithoutCapacityOrigin = nativePublicFixture();
nativeAlignedWithoutCapacityOrigin.artifacts.pair_confirmation.value.options[0]
	.physical_capacity_alignment_confirmed.download = true;
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeAlignedWithoutCapacityOrigin), false,
	'pair alignment cannot invent a physical-capacity-limited search origin');
const nativePairAutoApplyMismatch = nativePublicFixture();
nativePairAutoApplyMismatch.artifacts.pair_confirmation.value.auto_apply_pass = true;
assert.equal(helpers.nativeAutotunePublicResultValidated(nativePairAutoApplyMismatch), false,
	'the pair summary cannot promote a manual primary option to Auto-Apply');
const nativeCensoredWithoutAcknowledgement = nativeCensoredPublicFixture();
nativeCensoredWithoutAcknowledgement.public_apply_contract.options[0].required_acknowledgements = [
	'download-capacity-retention',
];
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeCensoredWithoutAcknowledgement), false,
	'a censored selected option must retain the measurement-confidence acknowledgement');
const nativeCensoredShortCoverage = nativeCensoredPublicFixture();
nativeCensoredShortCoverage.artifacts.pair_confirmation.value.transport_timeout_total_us = 14999999;
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeCensoredShortCoverage), false,
	'three timeout flights without the exact cumulative coverage floor must fail closed');
const nativeCensoredInventedTarget = nativeCensoredPublicFixture();
nativeCensoredInventedTarget.artifacts.download_search.value.selected.target_met = true;
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeCensoredInventedTarget), false,
	'a lower-bound timeout must never be presented as satisfying a latency target');
const nativeCensoredUnshapedWin = nativeCensoredPublicFixture();
Object.assign(nativeCensoredUnshapedWin.artifacts.topology_comparison.value.download, {
	choice: 'unshaped',
});
Object.assign(nativeCensoredUnshapedWin.artifacts.topology_comparison.value.download.unshaped, {
	transport_censored: true,
	target_met: false,
	measurement_reliable: false,
	safety_pass: false,
});
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeCensoredUnshapedWin), false,
	'a censored raw direction must never win the topology comparison');
const nativeManifestMismatch = nativePublicFixture();
nativeManifestMismatch.public_apply_contract.source_review_sha256 = '0'.repeat(64);
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeManifestMismatch), false,
	'the public confirmation contract must remain bound to the exact private Review digest');
const nativeTopologyMismatch = nativePublicFixture();
nativeTopologyMismatch.public_apply_contract.options[2].sqm_direction_mode = 'both';
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeTopologyMismatch), false,
	'the public confirmation contract must match the selected native topology exactly');
const nativeInjectedApplyValue = nativePublicFixture();
nativeInjectedApplyValue.public_apply_contract.options[0].download_kbps = 1;
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeInjectedApplyValue), false,
	'the browser confirmation contract must reject injected proposal values');
const nativeRateMismatch = nativePublicFixture();
nativeRateMismatch.public_apply_contract.options[0].target_rates_kbps.download = 723920;
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeRateMismatch), false,
	'a transformed or mismatched pair target must fail closed');
const nativeDuplicateOption = nativePublicFixture();
nativeDuplicateOption.public_apply_contract.options[1].option_id = 'recommended';
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeDuplicateOption), false,
	'duplicate option IDs must never create ambiguous Apply authority');
const nativeMissingAcknowledgement = nativePublicFixture();
nativeMissingAcknowledgement.public_apply_contract.options[0].required_acknowledgements = [];
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeMissingAcknowledgement), false,
	'manual-review eligibility must remain bound to a non-empty acknowledgement list');
const nativeUnknownAcknowledgement = nativePublicFixture();
nativeUnknownAcknowledgement.public_apply_contract.options[0].required_acknowledgements = [ 'invented' ];
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeUnknownAcknowledgement), false,
	'unknown native acknowledgement codes must fail closed');
const nativeDuplicateAcknowledgement = nativePublicFixture();
nativeDuplicateAcknowledgement.public_apply_contract.options[0].required_acknowledgements = [
	'download-capacity-retention', 'download-capacity-retention',
];
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeDuplicateAcknowledgement), false,
	'duplicate native acknowledgement codes must fail closed');
const nativeNoPreferredOption = nativePublicFixture();
nativeNoPreferredOption.public_apply_contract.options[2].preferred = false;
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeNoPreferredOption), false,
	'the bounded option set must retain one exact preferred option');
const nativeInventedPair = nativePublicFixture();
nativeInventedPair.public_apply_contract.options[1].target_rates_kbps = {
	download: 710000,
	upload: 915600,
};
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeInventedPair), false,
	'the browser must not cross-combine independently measured direction candidates');
const nativeTransform = nativePublicFixture();
nativeTransform.proposal_rate_transform = '0.8';
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeTransform), false,
	'any second native proposal rate transform must fail closed');
const nativeExtraArtifact = nativePublicFixture();
nativeExtraArtifact.artifacts.unbound = { sha256: '0'.repeat(64), value: { schema_version: 1 } };
assert.equal(helpers.nativeAutotunePublicResultValidated(nativeExtraArtifact), false,
	'unbound public Review artifacts must fail closed');
const nativeSelected = nativePublic.public_apply_contract.options[2];
const nativeConfirmation = {
	state: 'confirmation_ready',
	apply_enabled: true,
	validation_only: false,
	runtime_attested: true,
	already_applied: false,
	job_id: nativePublic.native_job_id,
	worker_run_id: nativePublic.run_id,
	option_id: nativeSelected.option_id,
	review_sha256: nativePublic.source_review_sha256,
	source_manifest_sha256: nativeSelected.manifest_sha256,
	manifest_sha256: nativeSelected.manifest_sha256,
	manifest_schema_version: 4,
	target_state: 'existing_managed',
	required_acknowledgements: nativeSelected.required_acknowledgements.slice(),
};
assert.equal(helpers.nativeAutotuneApplyCheckValidated(
	nativeConfirmation, nativePublic, nativeSelected), true,
	'the Apply check must bind the source Review to one effective server manifest');
assert.equal(helpers.nativeAutotuneApplyCheckValidated({
	...nativeConfirmation, target_state: 'absent_bootstrap',
}, nativePublic, nativeSelected), false,
	'an absent target cannot reuse the existing-instance v4 manifest schema');
assert.equal(helpers.nativeAutotuneApplyCheckValidated({
	...nativeConfirmation, source_manifest_sha256: '0'.repeat(64),
}, nativePublic, nativeSelected), false,
	'the effective manifest must remain bound to the selected public source manifest');
const nativeDirectionalSelected = nativeDirectionalRawFallback.public_apply_contract.options[1];
const nativeDirectionalConfirmation = {
	...nativeConfirmation,
	job_id: nativeDirectionalRawFallback.native_job_id,
	worker_run_id: nativeDirectionalRawFallback.run_id,
	option_id: nativeDirectionalSelected.option_id,
	review_sha256: nativeDirectionalRawFallback.source_review_sha256,
	source_manifest_sha256: nativeDirectionalSelected.manifest_sha256,
	manifest_sha256: nativeDirectionalSelected.manifest_sha256,
	manifest_schema_version: 6,
	required_acknowledgements: nativeDirectionalSelected.required_acknowledgements.slice(),
};
assert.equal(helpers.nativeAutotuneApplyCheckValidated(
	nativeDirectionalConfirmation, nativeDirectionalRawFallback, nativeDirectionalSelected), true,
	'the exact existing-instance download-bypass option must require manifest schema 6');
assert.equal(helpers.nativeAutotuneApplyCheckValidated({
	...nativeDirectionalConfirmation, manifest_schema_version: 4,
}, nativeDirectionalRawFallback, nativeDirectionalSelected), false,
	'a directional existing-instance option cannot be downgraded to shaped manifest schema 4');
const nativeShapedCapacitySelected =
	nativeShapedCapacityFallback.public_apply_contract.options[0];
const nativeShapedCapacityExistingConfirmation = {
	...nativeConfirmation,
	job_id: nativeShapedCapacityFallback.native_job_id,
	worker_run_id: nativeShapedCapacityFallback.run_id,
	option_id: nativeShapedCapacitySelected.option_id,
	review_sha256: nativeShapedCapacityFallback.source_review_sha256,
	source_manifest_sha256: nativeShapedCapacitySelected.manifest_sha256,
	manifest_sha256: nativeShapedCapacitySelected.manifest_sha256,
	manifest_schema_version: 9,
	target_state: 'existing_managed',
	required_acknowledgements: nativeShapedCapacitySelected.required_acknowledgements.slice(),
};
assert.equal(helpers.nativeAutotuneApplyCheckValidated(
	nativeShapedCapacityExistingConfirmation, nativeShapedCapacityFallback,
	nativeShapedCapacitySelected), true,
	'the shaped-capacity option must require existing-instance manifest schema 9');
assert.equal(helpers.nativeAutotuneApplyCheckValidated({
	...nativeShapedCapacityExistingConfirmation,
	manifest_schema_version: 4,
}, nativeShapedCapacityFallback, nativeShapedCapacitySelected), false,
	'a shaped-capacity option cannot be downgraded to ordinary shaped schema 4');
const nativeShapedCapacityBootstrapResult = nativeShapedCapacityFallbackPublicFixture();
Object.defineProperty(nativeShapedCapacityBootstrapResult, '_native_target_state', {
	value: 'absent_bootstrap', enumerable: false,
});
const nativeShapedCapacityBootstrapSelected =
	nativeShapedCapacityBootstrapResult.public_apply_contract.options[0];
const nativeShapedCapacityBootstrapConfirmation = {
	...nativeShapedCapacityExistingConfirmation,
	job_id: nativeShapedCapacityBootstrapResult.native_job_id,
	worker_run_id: nativeShapedCapacityBootstrapResult.run_id,
	option_id: nativeShapedCapacityBootstrapSelected.option_id,
	review_sha256: nativeShapedCapacityBootstrapResult.source_review_sha256,
	source_manifest_sha256: nativeShapedCapacityBootstrapSelected.manifest_sha256,
	manifest_sha256: nativeShapedCapacityBootstrapSelected.manifest_sha256,
	manifest_schema_version: 10,
	target_state: 'absent_bootstrap',
	required_acknowledgements:
		nativeShapedCapacityBootstrapSelected.required_acknowledgements.slice(),
};
assert.equal(helpers.nativeAutotuneApplyCheckValidated(
	nativeShapedCapacityBootstrapConfirmation, nativeShapedCapacityBootstrapResult,
	nativeShapedCapacityBootstrapSelected), true,
	'the shaped-capacity bootstrap option must require manifest schema 10');
assert.equal(helpers.nativeAutotuneApplyCheckValidated({
	...nativeShapedCapacityBootstrapConfirmation,
	manifest_schema_version: 9,
}, nativeShapedCapacityBootstrapResult, nativeShapedCapacityBootstrapSelected), false,
	'an absent shaped-capacity option cannot reuse existing-instance schema 9');
const nativeReceipt = {
	state: 'applied',
	configuration_written: true,
	recovery_cleared: true,
	job_id: nativePublic.native_job_id,
	worker_run_id: nativePublic.run_id,
	option_id: nativeSelected.option_id,
	review_sha256: nativePublic.source_review_sha256,
	source_manifest_sha256: nativeSelected.manifest_sha256,
	manifest_sha256: nativeSelected.manifest_sha256,
	manifest_schema_version: 4,
	target_state: 'existing_managed',
	acknowledged: nativeSelected.required_acknowledgements.slice(),
};
assert.equal(helpers.nativeAutotuneApplyReceiptValidated(
	nativeReceipt, nativePublic, nativeSelected, nativeConfirmation), true,
	'the native Apply receipt must bind the exact job, run, option, Review, manifest and ACK list');
assert.equal(helpers.nativeAutotuneApplyReceiptValidated({
	...nativeReceipt, manifest_sha256: '0'.repeat(64),
}, nativePublic, nativeSelected, nativeConfirmation), false,
	'a receipt for another manifest must never be accepted');
assert.equal(helpers.nativeAutotuneApplyReceiptValidated({
	...nativeReceipt, acknowledged: [],
}, nativePublic, nativeSelected, nativeConfirmation), false,
	'a receipt that omits a required acknowledgement must never be accepted');
const nativeApplyTransportSource = source.slice(
	source.indexOf('function runNativeAutotuneApply'),
	source.indexOf('function runNativeAutotuneJob'));
assert.match(nativeApplyTransportSource,
	/\[ '--calibrationctl', 'autotune-apply-check', result\.native_job_id,\s*option\.option_id \]/,
	'LuCI must obtain the effective server manifest before native Apply');
assert.match(nativeApplyTransportSource,
	/option\.option_id, result\.source_review_sha256, confirmation\.manifest_sha256 \]/,
	'LuCI native Apply must send only identities and the server-confirmed effective manifest before ACK codes');
assert.doesNotMatch(nativeApplyTransportSource, /target_rates|download_kbps|upload_kbps|uci\./,
	'LuCI native Apply transport must never send browser rates or UCI values');

let coldUiLookupCalled = false;
assert.equal(helpers.formOrUci({
	map: {},
	getUIElement() {
		coldUiLookupCalled = true;
		throw new Error('must not inspect a form before map.root exists');
	},
	formvalue() { return null; },
}, 'wan_sqm', 'access_medium_selection'), null);
assert.equal(coldUiLookupCalled, false,
	'cfgvalue must fall back to staged/UCI data before LuCI assigns map.root');
assert.equal(helpers.formOrUci({
	map: { root: {} },
	getUIElement() { return { getValue() { return 'cellular'; } }; },
	formvalue() { return null; },
}, 'wan_sqm', 'access_medium_selection'), 'cellular',
	'live forms must still prefer their current UI value');
assert.equal(helpers.manualSqmDirectionMode('both'), 'both');
assert.equal(helpers.manualSqmDirectionMode('upload_only'), 'upload_only');
assert.equal(helpers.manualSqmDirectionMode('download_only'), 'download_only');
assert.equal(helpers.manualSqmDirectionMode('off'), 'off',
	'the Edit form must preserve a verified stopped/no-SQM topology');
assert.match(source,
	/listValue\(section, 'sqm_basic', 'sqm_direction_mode', _\('CAKE directions'\)[\s\S]*?Upload only — no download\/ingress CAKE[\s\S]*?Download only — no upload\/egress CAKE/,
	'Edit -> SQM setup must expose all three running direction topologies');
assert.match(source,
	/selected === 'off'[\s\S]*?this\.value\('off', _\('Off — SQM disabled'\)\)/,
	'the Edit form must add the stopped state only when it is already committed');

const directionState = {
	enabled: '0',
	sqm_enabled: '0',
	sqm_direction_mode: 'both',
};
const directionHelpers = compileHelpers({}, {
	set(config, section, key, value) {
		assert.equal(config, 'cake-autorate');
		directionState[key] = value;
	},
	unset(config, section, key) {
		assert.equal(config, 'cake-autorate');
		delete directionState[key];
	},
	get(config, section, key) {
		assert.equal(config, 'cake-autorate');
		return directionState[key] ?? null;
	},
	sections(config) {
		return fixtureSections[config] || [];
	},
});
directionHelpers.writeManualSqmDirectionMode('wan_sqm', 'upload_only');
assert.equal(directionState.sqm_direction_mode, 'upload_only');
assert.equal(directionState.adjust_dl_shaper_rate, '0',
	'upload-only mode must disable download autorate adjustment');
delete directionState.adjust_dl_shaper_rate;
directionState.adjust_dl_shaper_rate = '0';
directionHelpers.writeManualSqmDirectionMode('wan_sqm', 'both');
assert.equal(directionState.adjust_dl_shaper_rate, '0',
	'restoring download CAKE must preserve a manual fixed-rate choice');
delete directionState.adjust_dl_shaper_rate;
directionHelpers.writeManualSqmDirectionMode('wan_sqm', 'download_only');
assert.equal(directionState.sqm_direction_mode, 'download_only');
assert.equal(directionState.adjust_ul_shaper_rate, '0',
	'download-only mode must disable upload autorate adjustment');
delete directionState.adjust_ul_shaper_rate;
directionState.adjust_ul_shaper_rate = '0';
directionHelpers.writeManualSqmDirectionMode('wan_sqm', 'both');
assert.equal(directionState.adjust_ul_shaper_rate, '0',
	'restoring upload CAKE must preserve a manual fixed-rate choice');
delete directionState.adjust_dl_shaper_rate;
delete directionState.adjust_ul_shaper_rate;
assert.equal(directionHelpers.validateManualSqmDirectionMode(null, 'wan_sqm', 'off'), true);
directionHelpers.writeManualSqmDirectionMode('wan_sqm', 'off');
assert.equal(directionState.sqm_direction_mode, 'off');
assert.equal(directionState.adjust_dl_shaper_rate, '0',
	'the stopped state must disable download autorate adjustment');
assert.equal(directionState.adjust_ul_shaper_rate, '0',
	'the stopped state must disable upload autorate adjustment');
directionState.enabled = '1';
directionState.sqm_enabled = '1';
assert.match(directionHelpers.validateManualSqmDirectionMode(null, 'wan_sqm', 'off'),
	/Disable autorate and managed SQM/,
	'an enabled instance must not retain the stopped direction state');
assert.throws(() => directionHelpers.writeManualSqmDirectionMode('wan_sqm', 'off'),
	/Disable autorate and managed SQM/,
	'the writer must fail closed even if form validation is bypassed');
assert.throws(() => directionHelpers.writeManualSqmDirectionMode('wan_sqm', 'invalid'),
	/CAKE directions must be Both, Upload only, Download only, or Off/);

assert.equal(helpers.shouldImportInterfaceRates('eth0', 'eth0', '108000', '14500'), false,
	'a spurious LuCI onchange on the same interface must not re-import runtime SQM rates');
assert.equal(helpers.shouldImportInterfaceRates('eth0', 'pppoe-wan', '108000', '14500'), true,
	'a real interface change must import the new link rates');
assert.equal(helpers.shouldImportInterfaceRates('eth0', 'eth0', '0', '14500'), true,
	'a missing or zero logical direction rate must be repaired from a usable preset');

const oneSidedRateState = {
	sqm_download: '108000',
	sqm_upload: '14500',
	base_dl_shaper_rate_kbps: '108000',
	base_ul_shaper_rate_kbps: '14500',
};
const oneSidedRateHelpers = compileHelpers({}, {
	set(config, section, key, value) {
		assert.equal(config, 'cake-autorate');
		oneSidedRateState[key] = value;
	},
	unset() {},
	get(config, section, key) {
		return config === 'cake-autorate' ? oneSidedRateState[key] ?? null : null;
	},
	sections(config) {
		return config === 'sqm' ? [{
			'.name': 'cake_wanb_sqm',
			'.type': 'queue',
			interface: 'eth0',
			download: '0',
			upload: '0',
			_cake_autorate_managed: 'wanb_sqm',
		}] : [];
	},
});
oneSidedRateHelpers.applyRatePreset('wanb_sqm', 'eth0', true, null);
assert.equal(oneSidedRateState.sqm_download, '108000',
	'a managed runtime download=0 must not overwrite the logical download capacity');
assert.equal(oneSidedRateState.sqm_upload, '14500',
	'a managed runtime upload=0 must not overwrite the logical upload capacity');
assert.equal(oneSidedRateState.min_dl_shaper_rate_kbps, '54000');
assert.equal(oneSidedRateState.min_ul_shaper_rate_kbps, '7250');
assert.match(source,
	/o\.onchange = function\(ev, section_id, value\)[\s\S]*?shouldImportInterfaceRates\(previous, value,[\s\S]*?applyWanPreset\(section_id, value, importRates/,
	'Target interface onchange must distinguish widget toggles from real WAN changes');

assert.equal(helpers.autotuneHasTrustedCapacityReferences({}), false,
	'reuse-trusted must stay unavailable without saved capacity references');
assert.equal(helpers.autotuneHasTrustedCapacityReferences({
	throughput_reference_dl_p50_kbps: '50000',
}), false, 'a saved DL reference without UL must not enable reuse-trusted');
assert.equal(helpers.autotuneHasTrustedCapacityReferences({
	throughput_reference_ul_p50_kbps: '10000',
}), false, 'a saved UL reference without DL must not enable reuse-trusted');
assert.equal(helpers.autotuneHasTrustedCapacityReferences({
	throughput_reference_dl_p50_kbps: '50000',
	throughput_reference_ul_p50_kbps: '0',
}), false, 'a non-positive directional reference must not enable reuse-trusted');
assert.equal(helpers.autotuneHasTrustedCapacityReferences({
	throughput_reference_dl_p50_kbps: '50000',
	throughput_reference_ul_p50_kbps: '10000',
}), true, 'positive saved DL and UL P50 references must enable reuse-trusted');
assert.equal(helpers.autotuneCalibrationStrategy({
	autotune_calibration_strategy: 'reuse_trusted',
}), 'shaped_only', 'an unavailable reuse-trusted request must fail closed to shaped-only');
assert.equal(helpers.autotuneCalibrationStrategy({
	autotune_calibration_strategy: 'reuse_trusted',
	throughput_reference_dl_p50_kbps: '50000',
}), 'shaped_only', 'a partial reuse-trusted request must fail closed to shaped-only');
assert.equal(helpers.autotuneCalibrationStrategy({
	autotune_calibration_strategy: 'reuse_trusted',
	throughput_reference_dl_p50_kbps: '50000',
	throughput_reference_ul_p50_kbps: '10000',
}), 'reuse_trusted', 'reuse-trusted must remain selected when both saved references exist');
assert.match(source,
	/'disabled': reuseAvailable && bootstrapRequired !== true \? null : 'disabled'[\s\S]*?Reuse current trusted bounds \(requires prior calibration\)/,
	'the wizard must visibly disable reuse-trusted until prior calibration references exist');
assert.doesNotMatch(source,
	/AUTOTUNE_RESULT_SCHEMA_VERSION|AUTOTUNE_RESULT_PRODUCER|cake-autorate-rs-autotune/,
	'the current browser must not retain the retired shell-result contract');
assert.doesNotMatch(source,
	/function autotuneResult(?:EnvelopeValidated|EvidenceValidated|Validated|Reviewable|Class|Confidence)/,
	'the current browser must not retain the retired shell-result decoder graph');
assert.doesNotMatch(source, /\bautotune_result\b|\bautotune_proposal\b/,
	'native Review and Apply must not be mirrored into browser-staged proposal state');
assert.match(source,
	/function runPreferredAutotuneJob[\s\S]*?return runNativeAutotuneJob/,
	'Full Auto-Tune must have one native execution path');
assert.doesNotMatch(source,
	/runPreferredAutotuneJob[\s\S]*?(?:rpcd-helper|applyAutotuneResultToState)/,
	'Full Auto-Tune must not fall back to a shell result or browser-side Apply');

for (const key of Object.keys(written))
	delete written[key];
helpers.writeWizardConfig('manual_wan', {
	mode: 'manual',
	name: 'manual_wan',
	is_new_instance: true,
	enabled: true,
	wan_if: 'eth1',
	route_mode: 'main',
	mwan3_member: '',
	sqm_direction_mode: 'both',
	sqm_section: 'cake_manual_wan',
	sqm_download: '100000',
	sqm_upload: '20000',
	autotune_profile: 'best_overall',
	autotune_calibration_strategy: 'shaped_only',
	speedtest_backend: 'speedtest-go',
	speedtest_go_server_id: '',
	speedtest_apply_percent: '90',
	pinger_method: 'fping',
	no_pingers: '6',
	reflectors: [ '1.1.1.1', '8.8.8.8', '9.9.9.9', '1.0.0.1', '8.8.4.4', '9.9.9.10' ],
}, false);
assert.equal(written.enabled, '1');
assert.equal(written.sqm_enabled, '1');
assert.equal(written.sqm_download, '100000');
assert.equal(written.sqm_upload, '20000');
assert.equal(written.traffic_profile, 'auto');
assert.equal(written.traffic_profile_migrated, undefined);
assert.equal(written.traffic_rules_enabled, '0');

const disabledFallback = {
	mode: 'autotune',
	name: 'disabled_wan',
	is_new_instance: true,
	enabled: true,
	wan_if: 'eth1',
	route_mode: 'main',
	mwan3_member: '',
	sqm_direction_mode: 'both',
	sqm_section: 'cake_disabled_wan',
	sqm_download: '100000',
	sqm_upload: '20000',
	autotune_profile: 'best_overall',
	autune_calibration_strategy: 'full_raw',
	speedtest_backend: 'speedtest-go',
	speedtest_apply_percent: '90',
	pinger_method: 'fping',
	no_pingers: '6',
	reflectors: [ '1.1.1.1', '8.8.8.8', '9.9.9.9', '1.0.0.1', '8.8.4.4', '9.9.9.10' ],
};
assert.doesNotThrow(() => helpers.writeWizardConfig('disabled_wan', disabledFallback, true));
assert.equal(written.enabled, '0');
assert.equal(written.sqm_enabled, '0');
assert.throws(() => helpers.writeWizardConfig('invalid_disabled', {
	...disabledFallback, mode: 'manual',
}, true));

const nativeMultiwanResult = nativePublicFixture();
const nativeAcceptedWan = {
	plan: { name: 'wanb_sqm' },
	decision: 'accepted',
	diagnostics: nativeMultiwanResult,
	state: { autotune_diagnostics: nativeMultiwanResult },
	native_apply_receipt: {
		state: 'applied',
		configuration_written: true,
		recovery_cleared: true,
		target_state: 'absent_bootstrap',
		manifest_schema_version: 7,
		job_id: nativeMultiwanResult.native_job_id,
		worker_run_id: nativeMultiwanResult.run_id,
	},
};
assert.equal(helpers.multiwanAutotuneItemNativeApplied(nativeAcceptedWan), true);
assert.equal(helpers.multiwanAutotuneItemAccepted(nativeAcceptedWan), true);
assert.equal(helpers.multiwanAutotuneItemAccepted({
	...nativeAcceptedWan, native_apply_receipt: null,
}), false, 'a browser-only decision cannot replace a native Apply receipt');
assert.deepEqual(helpers.multiwanAutotunePendingPlans([
	{ name: 'wan_sqm' }, { name: 'wanb_sqm' },
], [ nativeAcceptedWan ]), [ { name: 'wan_sqm' } ]);
const skippedWan = { decision: 'skipped', uncalibrated: true };
assert.equal(helpers.multiwanAutotuneItemDecided(skippedWan), true);
assert.equal(helpers.multiwanAutotuneBatchDecided([ nativeAcceptedWan, skippedWan ]), true);
assert.equal(helpers.multiwanAutotuneItemCanSkip({ recovery_pending: true }, false), false);
assert.equal(helpers.multiwanAutotuneItemCanSkip({ recovery_pending: false }, false), true);

assert.deepEqual(helpers.autotuneTypedTerminalDiagnostic({
	terminal_state: 'inconclusive',
	diagnostic_code: 'pair-options-unreviewable',
}), {
	code: 'pair-options-unreviewable',
	message: 'Every measured shaped pair was outside the manual safety boundary. The previous runtime settings were restored and no proposal was applied.',
});
assert.equal(helpers.autotuneTypedTerminalDiagnostic({
	terminal_state: 'failed', diagnostic_code: 'pair-options-unreviewable',
}), null);
assert.equal(helpers.autotuneRetryableInconclusive({
	state: 'inconclusive', retryable: true,
}), true);
assert.equal(helpers.autotuneMeasurementTimeout({
	state: 'inconclusive', retryable: true, reason: 'speedtest-timeout',
	search_state: 'measurement_timeout',
}), true);
async function testNativeAutotuneTransport() {
	const previousWindow = global.window;
	const delays = [];
	let pageReloads = 0;
	global.window = {
		setTimeout(resolve, delayMs) { delays.push(delayMs); resolve(); },
		location: { reload() { pageReloads++; } },
	};
	const publicJobId = 'b'.repeat(32);
	const capability = {
		state: 'idle',
		protocol_version: 2,
		admission_enabled: true,
		native_full_autotune: true,
		native_bootstrap_autotune: true,
		native_autotune_auto_backend: true,
		native_operation_status_identity_version: 1,
		native_autotune_status_identity_version: 1,
		native_public_result_version: 6,
	};
	const access = {
		medium: 'cellular',
		source: 'user_selected',
		confidence_percent: 100,
		policy: 'scheduled_active',
		service_dl_cap_kbps: '1000000',
		service_ul_cap_kbps: '500000',
	};
	const statusIdentity = (instance, target, routeMode, member, profile, strategy,
		targetState, managedSqmSection) => ({
		worker_run_id: nativePublic.run_id,
		request_identity_schema_version: 1,
		operation: 'full_autotune',
		instance,
		target_interface: target,
		backend: 'speedtest-go',
		speedtest_direction: null,
		speedtest_topology: null,
		route_mode: routeMode,
		mwan3_member: routeMode === 'mwan3' ? member : null,
		target_state: targetState || 'existing_managed',
		managed_sqm_section: managedSqmSection,
		profile,
		calibration_strategy: strategy,
		origin: 'luci',
	});

	try {
		const refreshCalls = [];
		const refreshHelpers = compileHelpers({}, {
			unload(packageName) { refreshCalls.push([ 'unload', packageName ]); },
			load(packageName) {
				refreshCalls.push([ 'load', packageName ]);
				return Promise.resolve(packageName);
			},
		}, { resolveDefault(value) { return value; } });
		await refreshHelpers.reloadAppliedUciPackages();
		assert.deepEqual(refreshCalls, [
			[ 'unload', 'cake-autorate' ], [ 'unload', 'sqm' ],
			[ 'load', 'cake-autorate' ], [ 'load', 'sqm' ],
		]);
		refreshCalls.length = 0;
		await refreshHelpers.reloadAppliedSettingsPage();
		assert.deepEqual(refreshCalls, [
			[ 'unload', 'cake-autorate' ], [ 'unload', 'sqm' ],
			[ 'load', 'cake-autorate' ], [ 'load', 'sqm' ],
		]);
		assert.equal(pageReloads, 1,
			'post-Apply refresh must reload the page after both UCI caches are authoritative');

		const queuedProgress = helpers.nativeAutotuneProgress({ state: 'queued' }, 0);
		assert.equal(queuedProgress.progress, 1);
		assert.match(queuedProgress.message, /Waiting for the calibration slot/);
		const downloadProgress = helpers.nativeAutotuneProgress({
			state: 'running', progress_schema_version: 1, progress_percent: 37,
			progress_step: 'searching_download_limit', progress_completed_units: 4,
			progress_total_units: 12, progress_direction: 'download', progress_attempt: 5,
		}, queuedProgress.progress);
		assert.equal(downloadProgress.progress, 37);
		assert.match(downloadProgress.message, /best download limit/);
		assert.match(downloadProgress.message, /Completed 4 of 12/);
		const mobileBypassProgress = helpers.nativeAutotuneProgress({
			state: 'running', progress_schema_version: 1, progress_percent: 87,
			progress_step: 'confirming_mobile_download_bypass', progress_completed_units: 1,
			progress_total_units: 2, progress_direction: 'upload', progress_attempt: 2,
		}, downloadProgress.progress);
		assert.equal(mobileBypassProgress.progress, 87);
		assert.match(mobileBypassProgress.message, /download without shaping/);
		assert.match(mobileBypassProgress.message, /Completed 1 of 2/);
		const regressedProgress = helpers.nativeAutotuneProgress({
			state: 'running', progress_schema_version: 1, progress_percent: 12,
			progress_step: 'measuring_idle_latency', progress_completed_units: 0,
			progress_total_units: 0,
		}, downloadProgress.progress);
		assert.equal(regressedProgress.progress, 37,
			'a malformed or stale active projection must not move the visible bar backward');
		assert.match(regressedProgress.message, /Preparing the test connection/);
		const cappedProgress = helpers.nativeAutotuneProgress({
			state: 'running', progress_schema_version: 1, progress_percent: 100,
			progress_step: 'preparing_proposals', progress_completed_units: 0,
			progress_total_units: 0,
		}, regressedProgress.progress);
		assert.equal(cappedProgress.progress, 99,
			'an active worker must never publish terminal 100 percent');
		const diagnosticsProgress = helpers.nativeAutotuneProgress({
			state: 'running', progress_schema_version: 1, progress_percent: 98,
			progress_step: 'preparing_diagnostics', progress_completed_units: 0,
			progress_total_units: 0,
		}, cappedProgress.progress);
		assert.equal(diagnosticsProgress.progress, 99,
			'the visible percentage must remain monotonic while diagnostics are prepared');
		assert.match(diagnosticsProgress.message, /calibration diagnostics/);
		const readyProgress = helpers.nativeAutotuneProgress({
			state: 'review_ready', progress_schema_version: 1, progress_percent: 99,
			progress_step: 'proposals_ready', progress_completed_units: 0,
			progress_total_units: 0,
		}, cappedProgress.progress);
		assert.equal(readyProgress.progress, 100);
		assert.match(readyProgress.message, /Proposals are ready/);
		for (const progressResult of [ queuedProgress, downloadProgress, mobileBypassProgress, regressedProgress,
			cappedProgress, diagnosticsProgress, readyProgress ])
			assert.doesNotMatch(progressResult.message, /native|rust|worker|coordinator/i);
		const unknownProgress = helpers.nativeAutotuneProgress({
			state: 'running', progress_schema_version: 1, progress_percent: 'NaN',
			progress_step: 'internal_variant_name', progress_attempt: -1,
		}, 0);
		assert.equal(unknownProgress.progress, 3);
		assert.match(unknownProgress.message, /detailed progress is temporarily unavailable/);

		assert.equal(helpers.nativeAutotuneCapabilityValidated(capability), true);
		assert.equal(helpers.nativeBootstrapAutotuneCapabilityValidated(capability), true);
		assert.equal(helpers.nativeAutotuneCapabilityValidated(capability, 'auto'), true);
		assert.equal(helpers.nativeAutotuneCapabilityValidated({
			...capability, native_autotune_auto_backend: false,
		}, 'auto'), false, 'automatic backend selection requires an explicit daemon capability');
		assert.equal(helpers.nativeAutotuneIntentSupported(
			'auto', 'main', true, 'full_raw', access), true);
		assert.equal(helpers.nativeAutotuneCapabilityValidated({
			...capability,
			admission_enabled: false,
		}), false, 'native routing must remain dormant until admission is explicitly advertised');
		assert.equal(helpers.nativeAutotuneCapabilityValidated({
			...capability,
			native_autotune_status_identity_version: 0,
		}), false, 'native resume requires the exact status identity contract');
		assert.equal(helpers.nativeAutotuneCapabilityValidated({
			...capability,
			native_operation_status_identity_version: 0,
		}), false, 'native resume requires the common operation identity contract');

		const launchArgs = helpers.nativeAutotuneLaunchArgs(
			'wan_sqm', 'pppoe-wan', 'speedtest-go', 'main', '',
			'variable_link', true, 'full_raw', access, true, 'cake_wan_sqm');
		assert.deepEqual(launchArgs.slice(0, 2), [ '--calibrationctl', 'autotune-start' ]);
		assert.equal(launchArgs[launchArgs.indexOf('--traffic-budget-bytes') + 1], '32000000000',
			'interactive native calibration must always carry the hard traffic budget');
		assert(launchArgs.includes('--allow-sqm-disable'));
		assert(launchArgs.includes('--allow-active-traffic'));
		assert(!launchArgs.includes('--mwan3-member'),
			'main routing must not carry an empty mwan3 member');
		assert(!launchArgs.some(arg => /token|fingerprint|job-id/i.test(arg)),
			'LuCI launch intent must contain no capability, job ID, or attestation hash');
		const autoLaunchArgs = helpers.nativeAutotuneLaunchArgs(
			'wan_sqm', 'pppoe-wan', 'auto', 'main', '',
			'variable_link', true, 'full_raw', access, true, 'cake_wan_sqm');
		assert.equal(autoLaunchArgs[autoLaunchArgs.indexOf('--backend') + 1], 'speedtest-go',
			'automatic backend policy must be resolved before native request publication');
		const bootstrapLaunchArgs = helpers.nativeAutotuneLaunchArgs(
			'new_sqm', 'eth1', 'speedtest-go', 'main', '',
			'best_overall', false, 'full_raw', access, false, 'cake_new_sqm');
		assert.deepEqual(bootstrapLaunchArgs.slice(0, 3),
			[ '--calibrationctl', 'autotune-bootstrap-start', 'cake_new_sqm' ]);
		assert.equal(helpers.nativeAutotuneIntentSupported(
			'speedtest-go', 'main', false, 'full_raw', access), true);
		assert.equal(helpers.nativeAutotuneIntentSupported(
			'speedtest-go', 'main', false, 'shaped_only', access), false,
			'a missing managed baseline must never enter shaped-only bootstrap');

		const bootstrapManifest = 'a'.repeat(64);
		const bootstrapApplyResult = nativePublicFixture();
		Object.defineProperty(bootstrapApplyResult, '_native_target_state', {
			value: 'absent_bootstrap', enumerable: false,
		});
		const bootstrapConfirmation = {
			...nativeConfirmation,
			target_state: 'absent_bootstrap',
			manifest_schema_version: 7,
			manifest_sha256: bootstrapManifest,
		};
		const bootstrapReceipt = {
			...nativeReceipt,
			target_state: 'absent_bootstrap',
			manifest_schema_version: 7,
			manifest_sha256: bootstrapManifest,
		};
		const applyJobId = 'c'.repeat(32);
		const applyJobToken = 'd'.repeat(64);
		const applyHandle = {
			state: 'accepted',
			apply_job_id: applyJobId,
			apply_job_token: applyJobToken,
			job_id: bootstrapApplyResult.native_job_id,
			option_id: nativeSelected.option_id,
			generation: 1,
		};
		const applyValidating = {
			state: 'validating', terminal: false,
			apply_job_id: applyJobId,
			job_id: bootstrapApplyResult.native_job_id,
			option_id: nativeSelected.option_id,
			generation: 2,
		};
		const applyApplying = {
			state: 'applying', terminal: false,
			apply_job_id: applyJobId,
			job_id: bootstrapApplyResult.native_job_id,
			option_id: nativeSelected.option_id,
			generation: 3,
		};
		const applyTerminal = {
			state: 'applied', terminal: true,
			apply_job_id: applyJobId,
			job_id: bootstrapApplyResult.native_job_id,
			option_id: nativeSelected.option_id,
			recovery_cleared: true,
			generation: 4,
		};
		assert.equal(helpers.nativeAutotuneApplyHandleValidated(
			applyHandle, bootstrapApplyResult, nativeSelected), true);
		const terminalReplayHandle = { ...applyHandle, generation: 3 };
		assert.equal(helpers.nativeAutotuneApplyHandleValidated(
			terminalReplayHandle, bootstrapApplyResult, nativeSelected), true,
			'a durable terminal retry must accept its current monotonic generation');
		assert.equal(helpers.nativeAutotuneApplyHandleValidated({
			...applyHandle, apply_job_token: 'not-a-token',
		}, bootstrapApplyResult, nativeSelected), false,
			'Apply must not poll with a malformed private handle');
		for (const invalidGeneration of [ 0, -1, 1.5, '1', Number.NaN,
			Number.MAX_SAFE_INTEGER + 1 ]) {
			assert.equal(helpers.nativeAutotuneApplyHandleValidated({
				...applyHandle, generation: invalidGeneration,
			}, bootstrapApplyResult, nativeSelected), false,
			`Apply must reject invalid handle generation ${String(invalidGeneration)}`);
		}
		assert.equal(helpers.nativeAutotuneApplyStatusValidated(
			applyValidating, applyHandle, bootstrapApplyResult, nativeSelected), true);
		assert.equal(helpers.nativeAutotuneApplyStatusValidated(
			applyTerminal, terminalReplayHandle, bootstrapApplyResult, nativeSelected), true,
			'a terminal status must advance from a replayed terminal handle');
		assert.equal(helpers.nativeAutotuneApplyStatusValidated({
			...applyTerminal, apply_job_id: 'e'.repeat(32),
		}, applyHandle, bootstrapApplyResult, nativeSelected), false,
			'Apply terminal status must stay bound to the accepted coordinator job');
		const applyCalls = [];
		const applyPayloads = [ bootstrapConfirmation, applyHandle, applyValidating,
			applyApplying, applyApplying, applyTerminal, bootstrapReceipt ];
		const applyHelpers = compileHelpers({
			exec(command, args) {
				applyCalls.push({ command, args });
				return Promise.resolve({ stdout: JSON.stringify(applyPayloads.shift()) });
			},
		});
		assert.deepEqual(await applyHelpers.runNativeAutotuneApply(
			bootstrapApplyResult, nativeSelected),
			bootstrapReceipt);
		assert.deepEqual(applyCalls.map(call => call.args.slice(0, 2)), [
			[ '--calibrationctl', 'autotune-apply-check' ],
			[ '--calibrationctl', 'autotune-apply-start' ],
			[ '--calibrationctl', 'autotune-apply-watch' ],
			[ '--calibrationctl', 'autotune-apply-watch' ],
			[ '--calibrationctl', 'autotune-apply-watch' ],
			[ '--calibrationctl', 'autotune-apply-watch' ],
			[ '--calibrationctl', 'autotune-apply-result' ],
		]);

		const terminalReplayCalls = [];
		const terminalReplayPayloads = [ bootstrapConfirmation, terminalReplayHandle,
			applyTerminal, bootstrapReceipt ];
		const terminalReplayHelpers = compileHelpers({
			exec(command, args) {
				terminalReplayCalls.push({ command, args });
				return Promise.resolve({ stdout: JSON.stringify(terminalReplayPayloads.shift()) });
			},
		});
		assert.deepEqual(await terminalReplayHelpers.runNativeAutotuneApply(
			bootstrapApplyResult, nativeSelected), bootstrapReceipt,
			'an exact successful retry must reuse its terminal handle and receipt');
		assert.deepEqual(terminalReplayCalls.map(call => call.args[1]), [
			'autotune-apply-check', 'autotune-apply-start',
			'autotune-apply-watch', 'autotune-apply-result',
		]);
		assert.equal(terminalReplayCalls.filter(call =>
			call.args[1] === 'autotune-apply-start').length, 1,
			'a terminal replay must not launch a second client-side Start');

		const rolledBackStatus = {
			...applyTerminal,
			state: 'rolled_back',
		};
		const rolledBackCalls = [];
		const rolledBackPayloads = [ bootstrapConfirmation, terminalReplayHandle,
			rolledBackStatus, { error: 'rolled_back: exact baseline restored' } ];
		const rolledBackHelpers = compileHelpers({
			exec(command, args) {
				rolledBackCalls.push({ command, args });
				return Promise.resolve({ stdout: JSON.stringify(rolledBackPayloads.shift()) });
			},
		});
		await assert.rejects(
			rolledBackHelpers.runNativeAutotuneApply(bootstrapApplyResult, nativeSelected),
			/rolled_back: exact baseline restored/,
			'an exact failed retry must report its known terminal outcome');
		assert.deepEqual(rolledBackCalls.map(call => call.args[1]), [
			'autotune-apply-check', 'autotune-apply-start',
			'autotune-apply-watch', 'autotune-apply-result',
		]);
		assert.equal(rolledBackCalls.filter(call =>
			call.args[1] === 'autotune-apply-start').length, 1,
			'a failed terminal replay must not silently relaunch Apply');
		assert.equal(applyCalls[1].args[5], bootstrapManifest,
			'Apply must use the effective v7 manifest returned by the server check');
		assert.deepEqual(applyCalls[2].args.slice(2), [ applyJobId, applyJobToken, '1' ],
			'watch reads must bind the accepted private handle and observed generation');
		assert.deepEqual(applyCalls[4].args.slice(2), [ applyJobId, applyJobToken, '3' ],
			'an unchanged watchdog response must chain another watch without a local delay');
		assert.deepEqual(applyCalls[6].args, [ '--calibrationctl',
			'autotune-apply-result', applyJobId, applyJobToken ],
			'the final receipt must be fetched through the same private handle');

		const replayCalls = [];
		const replayTimeouts = [];
		const replayL = { env: { rpctimeout: 90 } };
		const replayReceipt = { ...bootstrapReceipt, state: 'already_applied' };
		const replayTerminal = { ...applyTerminal, state: 'already_applied' };
		const replayHelpers = compileHelpers({
			exec(command, args) {
				replayCalls.push({ command, args });
				replayTimeouts.push(replayL.env.rpctimeout);
				if (replayCalls.length === 1)
					return Promise.resolve({ stdout: JSON.stringify(bootstrapConfirmation) });
				if (replayCalls.length === 2)
					return Promise.resolve({ code: 0, stdout: '{"state":"acce' });
				if (replayCalls.length === 3)
					return Promise.resolve({ stdout: JSON.stringify(applyHandle) });
				if (replayCalls.length === 4)
					return Promise.reject(new Error('connection reset'));
			if (replayCalls.length === 5)
				return Promise.resolve({ stdout: JSON.stringify(replayTerminal) });
				return Promise.resolve({ stdout: JSON.stringify(replayReceipt) });
			},
		}, null, replayL);
		assert.deepEqual(await replayHelpers.runNativeAutotuneApply(
			bootstrapApplyResult, nativeSelected), replayReceipt,
			'a lost start response and status transport must reconcile through one durable handle');
		assert.equal(replayCalls.length, 6);
		assert.deepEqual(replayCalls[1].args, replayCalls[2].args,
			'a malformed start response must retry the exact idempotent admission request');
		assert.deepEqual(replayCalls[3].args, replayCalls[4].args,
			'a transient watch failure must repeat only the same read-only generation query');
		assert(!replayCalls.slice(3).some(call => call.args[1] === 'autotune-apply-start'),
			'once a handle is accepted, network recovery must never replay the mutating start command');
		assert.deepEqual(replayTimeouts, [ 180, 90, 90, 90, 90, 90 ],
			'Apply admission and read phases must restore the caller RPC timeout after every request');
		assert.equal(replayL.env.rpctimeout, 90,
			'every Apply phase must restore the caller RPC timeout');
		assert.equal(replayHelpers.nativeAutotuneApplyRetryableRpcError(new Error(
			'unable to read calibration control response: Resource temporarily unavailable (os error 11)')),
		true, 'an ambiguous EAGAIN after exact Apply admission must retry idempotently');
		assert.equal(replayHelpers.nativeAutotuneApplyRetryableRpcError(new Error(
			'native Apply acknowledgement set is incomplete')),
		false, 'logical Apply failures must never be retried as transport loss');

		for (const abortedPhase of [ 'autotune-apply-start', 'autotune-apply-watch',
			'autotune-apply-result' ]) {
			const abortCalls = [];
			let aborted = false;
			const abortHelpers = compileHelpers({
				exec(command, args) {
					abortCalls.push({ command, args: args.slice() });
					if (args[1] === abortedPhase && !aborted) {
						aborted = true;
						return Promise.reject(new Error('XHR request aborted by browser'));
					}
					const payloads = {
						'autotune-apply-check': bootstrapConfirmation,
						'autotune-apply-start': applyHandle,
						'autotune-apply-watch': applyTerminal,
						'autotune-apply-result': bootstrapReceipt,
					};
					assert(Object.hasOwn(payloads, args[1]));
					return Promise.resolve({ stdout: JSON.stringify(payloads[args[1]]) });
				},
			});
			assert.deepEqual(await abortHelpers.runNativeAutotuneApply(
				bootstrapApplyResult, nativeSelected), bootstrapReceipt,
				`a browser-aborted ${abortedPhase} must recover through the same Apply identity`);
			const repeated = abortCalls.filter(call => call.args[1] === abortedPhase);
			assert.equal(repeated.length, 2);
			assert.deepEqual(repeated[0], repeated[1], 'retry must preserve every argument');
			assert.equal(abortCalls.filter(call => call.args[1] === 'autotune-apply-start').length,
				abortedPhase === 'autotune-apply-start' ? 2 : 1,
				'read recovery must never restart Apply admission');
			assert.equal(abortCalls.length, 5);
		}
		const exhaustedAbortCalls = [];
		const exhaustedAbortHelpers = compileHelpers({
			exec(command, args) {
				exhaustedAbortCalls.push({ command, args: args.slice() });
				if (args[1] === 'autotune-apply-check')
					return Promise.resolve({ stdout: JSON.stringify(bootstrapConfirmation) });
				return Promise.reject(new Error('XHR request aborted by browser'));
			},
		});
		await assert.rejects(exhaustedAbortHelpers.runNativeAutotuneApply(
			bootstrapApplyResult, nativeSelected), /XHR request aborted by browser/);
		assert.equal(exhaustedAbortCalls.length, 5, 'Apply admission retries must stay bounded');
		assert(exhaustedAbortCalls.slice(1).every(call =>
			JSON.stringify(call) === JSON.stringify(exhaustedAbortCalls[1])));
		for (const message of [ 'operation aborted by user', 'native Apply aborted',
			'permission denied', 'XHR request aborted by browser: permission denied' ]) {
			assert.equal(helpers.nativeAutotuneApplyRetryableRpcError(new Error(message)), false,
				`only the exact transport abort is retryable: ${message}`);
		}

		const resultRetryCalls = [];
		const resultRetryHelpers = compileHelpers({
			exec(command, args) {
				resultRetryCalls.push({ command, args });
				if (resultRetryCalls.length === 1)
					return Promise.resolve({ stdout: JSON.stringify(bootstrapConfirmation) });
				if (resultRetryCalls.length === 2)
					return Promise.resolve({ stdout: JSON.stringify(applyHandle) });
				if (resultRetryCalls.length === 3)
					return Promise.resolve({ stdout: JSON.stringify(applyTerminal) });
				if (resultRetryCalls.length === 4)
					return Promise.resolve({ code: 0, stdout: '{"state":"appl' });
				return Promise.resolve({ stdout: JSON.stringify(replayReceipt) });
			},
		});
		assert.deepEqual(await resultRetryHelpers.runNativeAutotuneApply(
			bootstrapApplyResult, nativeSelected), replayReceipt,
			'a truncated read-only result must retry without repeating Apply admission');
		assert.deepEqual(resultRetryCalls[3].args, resultRetryCalls[4].args);
		assert.equal(resultRetryCalls.filter(call =>
			call.args[1] === 'autotune-apply-start').length, 1);

		const malformedHandleCalls = [];
		const malformedHandleHelpers = compileHelpers({
			exec(command, args) {
				malformedHandleCalls.push({ command, args });
				if (malformedHandleCalls.length === 1)
					return Promise.resolve({ stdout: JSON.stringify(bootstrapConfirmation) });
				return Promise.resolve({ stdout: JSON.stringify({
					...applyHandle, apply_job_token: 'unsafe',
				}) });
			},
		});
		await assert.rejects(malformedHandleHelpers.runNativeAutotuneApply(
			bootstrapApplyResult, nativeSelected), /start response failed its job identity/);
		assert.equal(malformedHandleCalls.length, 2,
			'a malformed accepted handle must fail before any status query');

		const logicalFailureCalls = [];
		const logicalFailureHelpers = compileHelpers({
			exec(command, args) {
				logicalFailureCalls.push({ command, args });
				if (logicalFailureCalls.length === 1)
					return Promise.resolve({ stdout: JSON.stringify(bootstrapConfirmation) });
				if (logicalFailureCalls.length === 2)
					return Promise.resolve({ stdout: JSON.stringify(applyHandle) });
				if (logicalFailureCalls.length === 3)
					return Promise.resolve({ stdout: JSON.stringify({
						...applyTerminal, state: 'rolled_back',
					}) });
				return Promise.resolve({ stdout: JSON.stringify({
					state: 'rolled_back', error: 'unsafe Apply state',
				}) });
			},
		});
		await assert.rejects(
			logicalFailureHelpers.runNativeAutotuneApply(bootstrapApplyResult, nativeSelected),
			/unsafe Apply state/);
		assert.equal(logicalFailureCalls.length, 4,
			'a terminal rollback must fetch its durable diagnostic without replaying admission');
		assert.equal(logicalFailureCalls.filter(call =>
			call.args[1] === 'autotune-apply-start').length, 1);

		const nativeCalls = [];
		const nativePayloads = [
			capability,
			{ state: 'idle', instance: 'wan_sqm' },
			{ state: 'queued', job_id: publicJobId,
				...statusIdentity('wan_sqm', 'pppoe-wan', 'main', '',
					'variable_link', 'full_raw') },
			{ state: 'review_ready', job_id: publicJobId,
				...statusIdentity('wan_sqm', 'pppoe-wan', 'main', '',
					'variable_link', 'full_raw'),
				runtime_mutated: false, recovery_required: false },
			nativePublic,
		];
		const nativeHelpers = compileHelpers({
			exec(command, args) {
				nativeCalls.push({ command, args });
				assert(nativePayloads.length, 'unexpected native transport request');
				return Promise.resolve({ stdout: JSON.stringify(nativePayloads.shift()) });
			},
		});
		const nativeResult = await nativeHelpers.runPreferredAutotuneJob(
			'wan_sqm', 'pppoe-wan', 'speedtest-go', null, 'main', '',
			'variable_link', false, 'full_raw', access, true);
		assert.deepEqual(nativeResult, nativePublic);
		assert.deepEqual(nativeCalls.map(call => [ call.command, call.args.slice(0, 2) ]), [
			[ '/usr/sbin/cake-autorated', [ '--calibrationctl', 'summary' ] ],
			[ '/usr/sbin/cake-autorated', [ '--calibrationctl', 'autotune-current' ] ],
			[ '/usr/sbin/cake-autorated', [ '--calibrationctl', 'autotune-start' ] ],
			[ '/usr/sbin/cake-autorated', [ '--calibrationctl', 'autotune-status' ] ],
			[ '/usr/sbin/cake-autorated', [ '--calibrationctl', 'autotune-result' ] ],
		], 'advertised native capability must route the whole authenticated lifecycle through Rust');
		assert(!nativeCalls.some(call => call.command.endsWith('/autotune')),
			'a native start must never be followed by a legacy helper launch');

		const autoNativeCalls = [];
		const autoNativePayloads = [
			capability,
			{ state: 'idle', instance: 'wan_sqm' },
			{ state: 'queued', job_id: publicJobId,
				...statusIdentity('wan_sqm', 'pppoe-wan', 'main', '',
					'variable_link', 'full_raw') },
			{ state: 'review_ready', job_id: publicJobId,
				...statusIdentity('wan_sqm', 'pppoe-wan', 'main', '',
					'variable_link', 'full_raw'),
				runtime_mutated: false, recovery_required: false },
			nativePublic,
		];
		const autoNativeHelpers = compileHelpers({
			exec(command, args) {
				autoNativeCalls.push({ command, args });
				return Promise.resolve({ stdout: JSON.stringify(autoNativePayloads.shift()) });
			},
		});
		assert.deepEqual(await autoNativeHelpers.runPreferredAutotuneJob(
			'wan_sqm', 'pppoe-wan', 'auto', null, 'main', '',
			'variable_link', false, 'full_raw', access, true), nativePublic);
		const autoNativeStart = autoNativeCalls.find(call => call.args[1] === 'autotune-start');
		assert(autoNativeStart, 'automatic backend policy must stay on the native Auto-Tune path');
		assert.equal(autoNativeStart.args[autoNativeStart.args.indexOf('--backend') + 1],
			'speedtest-go');

		const rerunReviewCalls = [];
		const freshPublicJobId = 'a'.repeat(32);
		const freshNativePublic = nativePublicFixture();
		freshNativePublic.native_job_id = freshPublicJobId;
		freshNativePublic.public_apply_contract.native_job_id = freshPublicJobId;
		const rerunReviewPayloads = [
			capability,
			{ state: 'review_ready', job_id: publicJobId, instance: 'wan_sqm',
				runtime_mutated: false, recovery_required: false,
				progress_schema_version: 1, progress_percent: 100,
				progress_step: 'proposals_ready' },
			{ state: 'queued', job_id: freshPublicJobId,
				...statusIdentity('wan_sqm', 'pppoe-wan', 'main', '',
					'variable_link', 'full_raw') },
			{ state: 'review_ready', job_id: freshPublicJobId,
				...statusIdentity('wan_sqm', 'pppoe-wan', 'main', '',
					'variable_link', 'full_raw'),
				runtime_mutated: false, recovery_required: false },
			freshNativePublic,
		];
		const rerunReviewHelpers = compileHelpers({
			exec(command, args) {
				rerunReviewCalls.push({ command, args });
				return Promise.resolve({ stdout: JSON.stringify(rerunReviewPayloads.shift()) });
			},
		});
		assert.deepEqual(await rerunReviewHelpers.runPreferredAutotuneJob(
			'wan_sqm', 'pppoe-wan', 'speedtest-go', null, 'main', '',
			'variable_link', false, 'full_raw', access, true), freshNativePublic,
			'an explicit Run again must not return a matching historical Review');
		assert.deepEqual(rerunReviewCalls.map(call => call.args.slice(0, 2)), [
			[ '--calibrationctl', 'summary' ],
			[ '--calibrationctl', 'autotune-current' ],
			[ '--calibrationctl', 'autotune-start' ],
			[ '--calibrationctl', 'autotune-status' ],
			[ '--calibrationctl', 'autotune-result' ],
		]);
		assert.equal(rerunReviewCalls[2].args[1], 'autotune-start');

		const resumedActiveCalls = [];
		const resumedActivePayloads = [
			capability,
			{ state: 'running', job_id: publicJobId, instance: 'wan_sqm',
				...statusIdentity('wan_sqm', 'pppoe-wan', 'main', '',
					'variable_link', 'full_raw'),
				runtime_mutated: true, recovery_required: true,
				progress_schema_version: 1, progress_percent: 40,
				progress_step: 'searching_download_limit' },
			{ state: 'review_ready', job_id: publicJobId, instance: 'wan_sqm',
				...statusIdentity('wan_sqm', 'pppoe-wan', 'main', '',
					'variable_link', 'full_raw'),
				runtime_mutated: false, recovery_required: false },
			nativePublic,
		];
		const resumedActiveHelpers = compileHelpers({
			exec(command, args) {
				resumedActiveCalls.push({ command, args });
				return Promise.resolve({ stdout: JSON.stringify(resumedActivePayloads.shift()) });
			},
		});
		assert.deepEqual(await resumedActiveHelpers.runPreferredAutotuneJob(
			'wan_sqm', 'pppoe-wan', 'speedtest-go', null, 'main', '',
			'variable_link', false, 'full_raw', access, true), nativePublic);
		assert.deepEqual(resumedActiveCalls.map(call => call.args[1]),
			[ 'summary', 'autotune-current', 'autotune-status', 'autotune-result' ]);
		assert(!resumedActiveCalls.some(call => call.args[1] === 'autotune-start'),
			'an in-flight matching-instance operation must be observed rather than duplicated');

		const activeMismatchCalls = [];
		const activeMismatchHelpers = compileHelpers({
			exec(command, args) {
				activeMismatchCalls.push({ command, args });
				if (args[1] === 'summary')
					return Promise.resolve({ stdout: JSON.stringify(capability) });
				return Promise.resolve({ stdout: JSON.stringify({
					state: 'running', job_id: publicJobId,
					...statusIdentity('wan_sqm', 'pppoe-wan', 'main', '',
						'gaming', 'full_raw'),
					runtime_mutated: true, recovery_required: false,
				}) });
			},
		});
		await assert.rejects(activeMismatchHelpers.runPreferredAutotuneJob(
			'wan_sqm', 'pppoe-wan', 'speedtest-go', null, 'main', '',
			'gaming_extreme', false, 'full_raw', access, true), err => {
			assert.equal(err.autotuneActiveRequestMismatch, true);
			assert.equal(err.autotuneResult, undefined);
			return /different Full Auto-Tune request/.test(err.message);
		});
		assert.deepEqual(activeMismatchCalls.map(call => call.args[1]),
			[ 'summary', 'autotune-current' ],
			'a mismatched active job must be neither attached, duplicated nor cancelled');

		const recoveryCalls = [];
		const recoveryHelpers = compileHelpers({
			exec(command, args) {
				recoveryCalls.push({ command, args });
				return Promise.resolve({ stdout: JSON.stringify({
					...capability,
					state: 'recovery_required',
					admission_enabled: false,
				}) });
			},
		});
		await assert.rejects(recoveryHelpers.runPreferredAutotuneJob(
			'wan_sqm', 'pppoe-wan', 'speedtest-go', null, 'main', '',
			'best_overall', false, 'shaped_only', null, true),
			/restoring an earlier settings transaction/);
		assert.equal(recoveryCalls.length, 1);
		assert.equal(recoveryCalls[0].command, '/usr/sbin/cake-autorated');
		assert(!recoveryCalls.some(call => call.command.endsWith('/autotune')),
			'native recovery must fail closed instead of launching the legacy mutator');

		const bootstrapCalls = [];
		const bootstrapResult = nativePublicFixture();
		bootstrapResult.job_id = 'new_sqm';
		bootstrapResult.target_interface = 'eth1';
		bootstrapResult.resolved_interface = 'eth1';
		bootstrapResult.profile = 'best_overall';
		bootstrapResult.artifacts.proposal.value.profile = 'best_overall';
		bootstrapResult.artifacts.download_search.value.profile = 'best_overall';
		bootstrapResult.artifacts.upload_search.value.profile = 'best_overall';
		const bootstrapPayloads = [
			capability,
			{ state: 'queued', job_id: publicJobId,
				...statusIdentity('new_sqm', 'eth1', 'main', '',
					'best_overall', 'full_raw', 'absent_bootstrap', 'cake_new_sqm') },
			{ state: 'review_ready', job_id: publicJobId,
				...statusIdentity('new_sqm', 'eth1', 'main', '',
					'best_overall', 'full_raw', 'absent_bootstrap', 'cake_new_sqm'),
				runtime_mutated: false, recovery_required: false },
			bootstrapResult,
		];
		const bootstrapHelpers = compileHelpers({
			exec(command, args) {
				bootstrapCalls.push({ command, args });
				return Promise.resolve({ stdout: JSON.stringify(bootstrapPayloads.shift()) });
			},
		});
		const bootstrapResultReturned = await bootstrapHelpers.runPreferredAutotuneJob(
			'new_sqm', 'eth1', 'speedtest-go', null, 'main', '',
			'best_overall', false, 'full_raw', access, false, 'cake_new_sqm');
		assert.deepEqual(bootstrapResultReturned, bootstrapResult);
		assert(bootstrapCalls.every(call => call.command === '/usr/sbin/cake-autorated'),
			'new/unpersisted native instances must never launch the shell bootstrap mutator');
		assert.deepEqual(bootstrapCalls.map(call => call.args.slice(0, 3)), [
			[ '--calibrationctl', 'summary' ],
			[ '--calibrationctl', 'autotune-bootstrap-start', 'cake_new_sqm' ],
			[ '--calibrationctl', 'autotune-status', publicJobId ],
			[ '--calibrationctl', 'autotune-result', publicJobId ],
		], 'new-instance calibration must use the exact Rust bootstrap lifecycle');

		const timeoutCalls = [];
		const timeoutHelpers = compileHelpers({
			exec(command, args) {
				timeoutCalls.push({ command, args });
				if (args[1] === 'summary')
					return Promise.resolve({ stdout: JSON.stringify(capability) });
				if (args[1] === 'autotune-current')
					return Promise.resolve({ stdout: JSON.stringify({
						state: 'idle', instance: 'wan_sqm',
					}) });
				return Promise.reject(new Error('XHR request timed out'));
			},
		});
		await assert.rejects(timeoutHelpers.runPreferredAutotuneJob(
			'wan_sqm', 'pppoe-wan', 'speedtest-go', null, 'main', '',
			'variable_link', false, 'full_raw', access, true), err => {
			assert.equal(err.nativeAutotuneStartAttempted, true);
			return true;
		});
		assert.equal(timeoutCalls.length, 3,
			'an ambiguous native start timeout must not retry with the legacy mutating backend');
		const abortStartCalls = [];
		const abortStartHelpers = compileHelpers({
			exec(command, args) {
				abortStartCalls.push({ command, args });
				if (args[1] === 'summary')
					return Promise.resolve({ stdout: JSON.stringify(capability) });
				if (args[1] === 'autotune-current')
					return Promise.resolve({ stdout: JSON.stringify({
						state: 'idle', instance: 'wan_sqm',
					}) });
				return Promise.reject(new Error('XHR request aborted by browser'));
			},
		});
		await assert.rejects(abortStartHelpers.runPreferredAutotuneJob(
			'wan_sqm', 'pppoe-wan', 'speedtest-go', null, 'main', '',
			'variable_link', false, 'full_raw', access, true), err => {
			assert.equal(err.nativeAutotuneStartAttempted, true);
			assert.equal(err.message, 'XHR request aborted by browser');
			return true;
		});
		assert.equal(abortStartCalls.length, 3,
			'Apply retry support must not duplicate an ambiguously accepted calibration start');

		const mismatched = nativePublicFixture();
		mismatched.target_interface = 'eth9';
		assert.equal(helpers.nativeAutotunePublicResultValidated(mismatched), true,
			'the public schema alone deliberately does not know the active dialog');
		assert.equal(helpers.nativeAutotuneResultMatchesRequest(mismatched, publicJobId,
			'wan_sqm', 'pppoe-wan', 'main', '', 'variable_link', 'full_raw'), false,
			'the transport must independently bind a valid public result to the active dialog');
		const mismatchPayloads = [ capability,
			{ state: 'idle', instance: 'wan_sqm' },
			{ state: 'queued', job_id: publicJobId,
				...statusIdentity('wan_sqm', 'pppoe-wan', 'main', '',
					'variable_link', 'full_raw') },
			{ state: 'review_ready', job_id: publicJobId,
				...statusIdentity('wan_sqm', 'pppoe-wan', 'main', '',
					'variable_link', 'full_raw'),
				runtime_mutated: false, recovery_required: false },
			mismatched ];
		const mismatchHelpers = compileHelpers({
			exec() {
				assert(mismatchPayloads.length, 'an invalid terminal result must not trigger another RPC');
				return Promise.resolve({ stdout: JSON.stringify(mismatchPayloads.shift()) });
			},
		});
		await assert.rejects(mismatchHelpers.runPreferredAutotuneJob(
			'wan_sqm', 'pppoe-wan', 'speedtest-go', null, 'main', '',
			'variable_link', false, 'full_raw', access, true),
			err => {
				assert.equal(err.autotuneResult, undefined);
				assert.deepEqual(err.autotuneRejectedResult, mismatched);
				return /no longer matches this request/.test(err.message);
			});
		const rejectedState = { autotune_result: mismatched };
		helpers.recordAutotuneTerminalFailure(rejectedState, mismatched,
			'The calibration result no longer matches this request.');
		assert.equal(helpers.nativeAutotunePublicResultValidated(
			rejectedState.autotune_diagnostics), false,
			'a rejected but structurally valid public result must never re-enter Review/Apply');
		await assert.rejects(mismatchHelpers.cancelPreferredAutotuneJob(
			'wan_sqm', 'pppoe-wan', 'speedtest-go', 'variable_link', 'main', ''),
			/No authenticated calibration handle/,
			'a terminal invalid result must not leave a stale cancellable native handle');

		delays.length = 0;
		const cancelCalls = [];
		const cancelPayloads = [
			{ state: 'cancelling', job_id: publicJobId,
				...statusIdentity('wan_sqm', 'pppoe-wan', 'main', '',
					'variable_link', 'full_raw'),
				runtime_mutated: true, recovery_required: false },
			{ state: 'recovering', job_id: publicJobId,
				...statusIdentity('wan_sqm', 'pppoe-wan', 'main', '',
					'variable_link', 'full_raw'),
				runtime_mutated: true, recovery_required: true },
			{ state: 'cancelled', job_id: publicJobId,
				...statusIdentity('wan_sqm', 'pppoe-wan', 'main', '',
					'variable_link', 'full_raw'),
				runtime_mutated: false, recovery_required: false },
		];
		const cancelHelpers = compileHelpers({
			exec(command, args) {
				cancelCalls.push({ command, args });
				return Promise.resolve({ stdout: JSON.stringify(cancelPayloads.shift()) });
			},
		});
		cancelHelpers.setNativeAutotuneJob('wan_sqm', publicJobId, {
			wan: 'pppoe-wan', route_mode: 'main', mwan3_member: '',
			profile: 'variable_link', calibration_strategy: 'full_raw',
			backend: 'speedtest-go', existing_instance: true,
			planned_sqm_section: undefined,
		}, nativePublic.run_id);
		assert.equal((await cancelHelpers.cancelPreferredAutotuneJob(
			'wan_sqm', 'pppoe-wan', 'speedtest-go', 'variable_link', 'main', '')).state,
		'cancelled');
		assert.deepEqual(cancelCalls.map(call => call.args[1]),
			[ 'autotune-cancel', 'autotune-status', 'autotune-status' ]);
		assert.deepEqual(delays, [ 2000, 4000 ],
			'native cancellation must wait with bounded backoff until exact restoration is published');
	}
	finally {
		if (previousWindow === undefined)
			delete global.window;
		else
			global.window = previousWindow;
	}
}

async function testNativeSpeedtestTransport() {
	const previousWindow = global.window;
	global.window = { setTimeout(resolve) { resolve(); } };
	const publicJobId = 'c'.repeat(32);
	const capability = {
		state: 'idle',
		protocol_version: 2,
		admission_enabled: true,
		native_speedtest: true,
		native_bootstrap_speedtest: true,
		native_speedtest_auto_backend: true,
		native_operation_status_identity_version: 1,
		native_public_result_version: 6,
	};
	const speedtestIdentity = (instance, target, routeMode, member, serverId, topology,
		targetState, plannedSqmSection) => ({
		worker_run_id: 'e'.repeat(32),
		request_identity_schema_version: 1,
		operation: 'speedtest',
		instance,
		target_interface: target,
		backend: 'speedtest-go',
		speedtest_direction: 'both',
		speedtest_server_id: serverId == null ? null : String(serverId),
		speedtest_topology: topology,
		route_mode: routeMode,
		mwan3_member: routeMode === 'mwan3' ? member : null,
		target_state: targetState || 'existing_managed',
		managed_sqm_section: targetState === 'absent_bootstrap' ? plannedSqmSection : null,
		origin: 'luci',
	});
	const result = {
		state: 'complete', job_id: publicJobId,
		backend: 'speedtest-go', calibration: 'unshaped',
		shaper_bypassed: true, runtime_mutated: false, runtime_restored: true,
		limits_changed: false, download_kbps: 900000, upload_kbps: 800000,
	};

	try {
		assert.equal(helpers.nativeSpeedtestCapabilityValidated(capability), true);
		assert.equal(helpers.nativeSpeedtestCapabilityValidated(capability, 'auto'), true);
		assert.equal(helpers.nativeSpeedtestCapabilityValidated({
			...capability, native_speedtest_auto_backend: false,
		}, 'auto'), false, 'automatic Speed Test selection requires explicit daemon support');
		assert.equal(helpers.nativeEffectiveSpeedtestBackend('auto'), 'speedtest-go');
		assert.equal(helpers.nativeEffectiveSpeedtestBackend('speedtest-go'), 'speedtest-go');
		assert.equal(helpers.nativeEffectiveSpeedtestBackend('librespeed-cli'), null);
		assert.equal(helpers.nativeSpeedtestIntentSupported('auto', 'main', true), true);
		assert.equal(helpers.nativeSpeedtestIntentSupported(
			'auto', 'main', false, 'unshaped', 'cake_new_sqm'), true);
		assert.equal(helpers.nativeSpeedtestIntentSupported(
			'auto', 'main', false, 'current', 'cake_new_sqm'), false);
		assert.equal(helpers.nativeSpeedtestCapabilityValidated({
			...capability, native_speedtest: false,
		}), false);
		assert.equal(helpers.nativeSpeedtestCapabilityValidated({
			...capability, native_operation_status_identity_version: 0,
		}), false, 'native Speed Test requires the exact operation identity contract');
		assert.deepEqual(helpers.nativeSpeedtestLaunchArgs(
			'wanb_sqm', 'eth1', 'mwan3', 'wanb', '42', 'unshaped'), [
			'--calibrationctl', 'speedtest-start', '--instance', 'wanb_sqm',
			'--expected-target', 'eth1', '--backend', 'speedtest-go',
			'--direction', 'both', '--topology', 'unshaped', '--route-mode', 'mwan3',
			'--mwan3-member', 'wanb', '--server-id', '42',
		]);
		assert.deepEqual(helpers.nativeSpeedtestLaunchArgs(
			'new_sqm', 'eth1', 'main', '', '', 'unshaped', false, 'cake_new_sqm').slice(0, 4),
			[ '--calibrationctl', 'speedtest-bootstrap-start', 'cake_new_sqm', '--instance' ]);
		assert.equal(helpers.nativeSpeedtestResultValidated(result, publicJobId, 'unshaped'), true);
		assert.equal(helpers.nativeSpeedtestResultValidated({
			...result, calibration: 'current', shaper_bypassed: false, runtime_restored: false,
		}, publicJobId, 'current'), true);

		const calls = [];
		const payloads = [ capability, { state: 'idle', instance: 'wan_sqm' },
			{ state: 'queued', job_id: publicJobId,
				...speedtestIdentity('wan_sqm', 'pppoe-wan', 'main', '', null, 'unshaped') },
			{ state: 'completed', job_id: publicJobId,
				...speedtestIdentity('wan_sqm', 'pppoe-wan', 'main', '', null, 'unshaped') }, result ];
		const nativeHelpers = compileHelpers({
			exec(command, args) {
				calls.push({ command, args });
				return Promise.resolve({ stdout: JSON.stringify(payloads.shift()) });
			},
		});
		const response = await nativeHelpers.runSpeedtestJob(
			'wan_sqm', 'pppoe-wan', 'speedtest-go', null, 'main', '', '', true, 'unshaped');
		assert.deepEqual(JSON.parse(response.stdout), result);
		assert.deepEqual(calls.map(call => [ call.command, call.args[1] ]), [
			[ '/usr/sbin/cake-autorated', 'summary' ],
			[ '/usr/sbin/cake-autorated', 'speedtest-current' ],
			[ '/usr/sbin/cake-autorated', 'speedtest-start' ],
			[ '/usr/sbin/cake-autorated', 'speedtest-status' ],
			[ '/usr/sbin/cake-autorated', 'speedtest-result' ],
		]);
		assert(!calls.some(call => call.command.endsWith('/speedtest')),
			'native admission must own the complete lifecycle');

		const autoCalls = [];
		const autoPayloads = [ capability, { state: 'idle', instance: 'wan_sqm' },
			{ state: 'queued', job_id: publicJobId,
				...speedtestIdentity('wan_sqm', 'pppoe-wan', 'main', '', null, 'unshaped') },
			{ state: 'completed', job_id: publicJobId,
				...speedtestIdentity('wan_sqm', 'pppoe-wan', 'main', '', null, 'unshaped') }, result ];
		const autoHelpers = compileHelpers({
			exec(command, args) {
				autoCalls.push({ command, args });
				return Promise.resolve({ stdout: JSON.stringify(autoPayloads.shift()) });
			},
		});
		assert.deepEqual(JSON.parse((await autoHelpers.runSpeedtestJob(
			'wan_sqm', 'pppoe-wan', 'auto', null, 'main', '', '', true,
			'unshaped')).stdout), result);
		const autoStart = autoCalls.find(call => call.args[1] === 'speedtest-start');
		assert(autoStart, 'automatic backend policy must stay on the native lifecycle');
		assert.equal(autoStart.args[autoStart.args.indexOf('--backend') + 1], 'speedtest-go');

		const bootstrapCalls = [];
		const bootstrapPayloads = [ capability,
			{ state: 'queued', job_id: publicJobId,
				...speedtestIdentity('new_sqm', 'eth1', 'main', '', null, 'unshaped',
					'absent_bootstrap', 'cake_new_sqm') },
			{ state: 'completed', job_id: publicJobId,
				...speedtestIdentity('new_sqm', 'eth1', 'main', '', null, 'unshaped',
					'absent_bootstrap', 'cake_new_sqm') }, result ];
		const bootstrapHelpers = compileHelpers({
			exec(command, args) {
				bootstrapCalls.push({ command, args });
				return Promise.resolve({ stdout: JSON.stringify(bootstrapPayloads.shift()) });
			},
		});
		assert.deepEqual(JSON.parse((await bootstrapHelpers.runSpeedtestJob(
			'new_sqm', 'eth1', 'auto', null, 'main', '', '', false,
			'unshaped', 'cake_new_sqm')).stdout), result);
		assert.deepEqual(bootstrapCalls.map(call => call.args[1]), [
			'summary', 'speedtest-bootstrap-start', 'speedtest-status', 'speedtest-result',
		], 'new-instance Speed Test must use the mutation-free native bootstrap lifecycle');
		assert.equal(helpers.nativeSpeedtestStatusMatchesRequest({
			state: 'running', job_id: publicJobId,
			...speedtestIdentity('wan_sqm', 'pppoe-wan', 'main', '', null, 'unshaped'),
		}, publicJobId, 'wan_sqm', 'pppoe-wan', 'main', '', '', 'unshaped'), true);
		assert.equal(helpers.nativeSpeedtestStatusMatchesRequest({
			state: 'running', job_id: publicJobId,
			...speedtestIdentity('wanb_sqm', 'pppoe-wan', 'main', '', null, 'unshaped'),
		}, publicJobId, 'wan_sqm', 'pppoe-wan', 'main', '', '', 'unshaped'), false,
			'a Speed Test status from another instance must fail closed');
		const exactSpeedtestStatus = {
			state: 'running', job_id: publicJobId,
			...speedtestIdentity('wan_sqm', 'pppoe-wan', 'mwan3', 'wanb', 42, 'current'),
		};
		for (const [ field, value ] of [
			[ 'target_interface', 'eth1' ], [ 'backend', 'other' ],
			[ 'speedtest_direction', 'download' ], [ 'speedtest_server_id', '43' ],
			[ 'speedtest_topology', 'unshaped' ], [ 'route_mode', 'main' ],
			[ 'mwan3_member', 'wanc' ], [ 'target_state', 'absent_bootstrap' ],
			[ 'origin', 'scheduler' ],
		]) {
			assert.equal(helpers.nativeSpeedtestStatusMatchesRequest({
				...exactSpeedtestStatus, [field]: value,
			}, publicJobId, 'wan_sqm', 'pppoe-wan', 'mwan3', 'wanb', '42', 'current'), false,
			`Speed Test status mutation ${field} must fail closed`);
		}
		assert.equal(helpers.nativeOperationWorkerRunId({ worker_run_id: null }, null, false), null);
		assert.equal(helpers.nativeOperationWorkerRunId({ worker_run_id: 'e'.repeat(32) },
			null, true), 'e'.repeat(32));
		assert.equal(helpers.nativeOperationWorkerRunId({ worker_run_id: 'f'.repeat(32) },
			'e'.repeat(32), false), undefined,
			'a native operation status cannot switch worker identity');

		const failedCalls = [];
		const failedHelpers = compileHelpers({
			exec(command, args) {
				failedCalls.push({ command, args });
				if (args[1] === 'summary')
					return Promise.resolve({ stdout: JSON.stringify(capability) });
				if (args[1] === 'speedtest-current')
					return Promise.resolve({ stdout: JSON.stringify({ state: 'idle', instance: 'wan_sqm' }) });
				return Promise.reject(new Error('XHR request timed out'));
			},
		});
		await assert.rejects(failedHelpers.runSpeedtestJob(
			'wan_sqm', 'pppoe-wan', 'speedtest-go', null, 'main', '', '', true, 'unshaped'), err => {
			assert.equal(err.nativeSpeedtestStartAttempted, true);
			return true;
		});
		assert.equal(failedCalls.length, 3,
			'an ambiguous native start must never be replayed through the legacy helper');

		const resumedCalls = [];
		const resumedPayloads = [ capability,
			{ state: 'running', job_id: publicJobId,
				...speedtestIdentity('wan_sqm', 'pppoe-wan', 'main', '', null, 'unshaped') },
			{ state: 'completed', job_id: publicJobId,
				...speedtestIdentity('wan_sqm', 'pppoe-wan', 'main', '', null, 'unshaped') },
			result ];
		const resumedHelpers = compileHelpers({
			exec(command, args) {
				resumedCalls.push({ command, args });
				return Promise.resolve({ stdout: JSON.stringify(resumedPayloads.shift()) });
			},
		});
		assert.deepEqual(JSON.parse((await resumedHelpers.runSpeedtestJob(
			'wan_sqm', 'pppoe-wan', 'speedtest-go', null, 'main', '', '', true,
			'unshaped')).stdout), result);
		assert.deepEqual(resumedCalls.map(call => call.args[1]), [
			'summary', 'speedtest-current', 'speedtest-status', 'speedtest-result',
		], 'an exact active Speed Test must attach without a duplicate start');

		const activeMismatchCalls = [];
		const activeMismatchHelpers = compileHelpers({
			exec(command, args) {
				activeMismatchCalls.push({ command, args });
				if (args[1] === 'summary')
					return Promise.resolve({ stdout: JSON.stringify(capability) });
				return Promise.resolve({ stdout: JSON.stringify({
					state: 'running', job_id: publicJobId,
					...speedtestIdentity('wan_sqm', 'pppoe-wan', 'mwan3', 'wanb', null,
						'unshaped'),
				}) });
			},
		});
		await assert.rejects(activeMismatchHelpers.runSpeedtestJob(
			'wan_sqm', 'pppoe-wan', 'speedtest-go', null, 'main', '', '', true,
			'unshaped'), err => {
			assert.equal(err.speedtestActiveRequestMismatch, true);
			assert.equal(err.nativeSpeedtestStartAttempted, undefined);
			return /different Speed Test request/.test(err.message);
		});
		assert.deepEqual(activeMismatchCalls.map(call => call.args[1]),
			[ 'summary', 'speedtest-current' ],
			'a mismatched active Speed Test must be neither attached, duplicated nor cancelled');

		const legacyCalls = [];
		const legacyHelpers = compileHelpers({
			exec(command, args) {
				legacyCalls.push({ command, args });
				throw new Error('unsupported requests must fail before RPC');
			},
		});
		await assert.rejects(legacyHelpers.runSpeedtestJob(
			'wan_sqm', 'pppoe-wan', 'librespeed-cli', null, 'main', '', '', true),
			/not supported by the native measurement service/);
		assert.equal(legacyCalls.length, 0,
			'an explicitly configured retired backend must fail visibly without shell fallback');
	}
	finally {
		if (previousWindow === undefined)
			delete global.window;
		else
			global.window = previousWindow;
	}
}

testNativeSpeedtestTransport().then(() => testNativeAutotuneTransport()).then(() => {
	console.log('settings autotune tests passed');
}).catch(err => {
	console.error(err);
	process.exitCode = 1;
});
