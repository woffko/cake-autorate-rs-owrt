'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const sourcePath = path.join(__dirname, '..', 'htdocs', 'luci-static', 'resources',
	'view', 'cake-autorate-rs', 'settings.js');
const source = fs.readFileSync(sourcePath, 'utf8');
assert.doesNotMatch(source, /APPLIED BY NATIVE RUST/,
	'Auto-Tune user-facing receipts must describe the result, not the implementation language');
assert.equal((source.match(/window\.setTimeout\(/g) || []).length, 3,
	'settings timers are limited to speedtest, Auto-Tune and guarded-apply polling helpers');
for (const helper of [ 'speedtestJobDelay', 'autotuneJobDelay', 'applyGuardDelay' ]) {
	assert.match(source, new RegExp('function ' + helper + '\\([^)]*\\)\\s*\\{[\\s\\S]*?window\\.setTimeout\\('),
		`${helper} must remain an explicit polling/watchdog cadence helper`);
}
const prefix = source.slice(0, source.indexOf('return L.view.extend'));
assert.match(source, /function modal\(option\)\s*\{[\s\S]*?option\.modalonly = true;[\s\S]*?option\.retain = true;/,
	'all modal settings must retain dependency-hidden values instead of staging unrelated deletions');
assert.match(source,
	/return uci\.save\(\)\.then\(function\(\) \{ return created; \}\);/,
	'the wizard must persist its exact staged proposal without reparsing stale GridSection widgets');
assert.match(source,
	/requireCleanUciTransaction\(_\('Apply or revert existing pending changes before staging a Full Auto-Tune proposal\.'\)\)/,
	'single-WAN guarded Auto-Tune must refuse to stage on top of stale pending UCI deltas');
assert.doesNotMatch(source,
	/return grid\.map\.save\(null, true\)\.then\(function\(\) \{ return created; \}\);/,
	'the wizard must not overwrite its staged proposal from stale modal widgets');
assert.match(source,
	/handleSave: function\(ev\)[\s\S]*?pendingAutotuneApplyMarkers\(\)[\s\S]*?cannot be stored as ordinary pending changes[\s\S]*?this\.super\('handleSave'/,
	'plain Save must refuse to strand a guarded Auto-Tune proposal as ordinary pending changes');
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
assert.match(source, /function renderAutotuneAdvisoryGate[\s\S]*?_\('WARN'\)/,
	'CPU pressure must render as an advisory warning instead of a failed gate');
assert(source.includes('variable-throughput-advisory') &&
	source.includes('Variable-link advisory'),
	'Auto-Tune review must explain bounded volatile-throughput results');
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
assert.match(source, /Accept proposal[\s\S]*Skip this uplink/,
	'each Multi-WAN result must require an explicit Accept or Skip decision');
assert.match(source,
	/runSequentialAutotuneApplies\(accepted,[\s\S]*?stageWizardPlanItem\(config_name, item\)[\s\S]*?runGuardedSaveApply\(null, null, false\)[\s\S]*?reloadWizardUci\(\)/,
	'Multi-WAN proposals must be guarded, applied and reloaded one at a time');
assert.match(source, /Create & apply sequentially/,
	'the Multi-WAN final action must describe that it applies the accepted proposals');
assert.doesNotMatch(source,
	/state\.multiwan_set\s*\?\s*E\('pre'[\s\S]*?\)\s*:\s*null/,
	'the Multi-WAN plan must never pass a null child that LuCI renders as literal text');
assert.match(source,
	/E\('pre',\s*\{[\s\S]*?'style':\s*state\.multiwan_set\s*\?[\s\S]*?'display:none'[\s\S]*?\},\s*plan\)/,
	'the inactive Multi-WAN plan must remain a real hidden DOM node');
assert.match(source,
	/pendingAutotuneApplyMarkers\(\)\.length[\s\S]*?cake_autorate_apply_guard[\s\S]*?refusing to mix it with disabled uplinks/,
	'disabled fallback instances must never share a transaction with stale apply markers');
assert.match(source, /I accept all listed trade-offs\./,
	'Native Review must use one aggregate acknowledgement for the fully listed trade-offs');
assert.doesNotMatch(source, /Reload settings/,
	'a successful Apply must refresh committed settings without another user click');
assert.match(source,
	/function reloadAppliedUciPackages\(\)[\s\S]*?uci\.unload\('cake-autorate'\)[\s\S]*?uci\.unload\('sqm'\)[\s\S]*?uci\.load\('cake-autorate'\)[\s\S]*?uci\.load\('sqm'\)/,
	'post-Apply refresh must invalidate both LuCI UCI caches before reloading them');
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
			`topicTab, autorateSubcategory, autorateSubcategoryDefinitions, ` +
			`formOrUci, accessMediumDefinitions, accessMediumTitle, accessMediumExplorationPercent, detectAccessMedium, resolvedAccessContext, ` +
			`recommendedCapacityLearningPolicy, canonicalCapacityLearningPolicy, autotuneAccessRequest, ` +
			`canonicalAutotuneProfile, autotuneProfilePolicy, autotuneProfileDefinitions, ` +
			`nativeAutotunePublicResultValidated, nativeAutotuneAcknowledgementLabel, nativeMobileDownloadBypassRequested, nativeDownloadBypassUnavailableReason, renderNativeAutotuneDiagnostics, nativeAutotuneApplyCheckValidated, ` +
			`nativeAutotuneApplyReceiptValidated, runNativeAutotuneApplyCheck, runNativeAutotuneApply, reloadAppliedUciPackages, ` +
			`visibleAutotuneProfile, autotuneRunProfile, storedAutotuneProfile, ` +
			`autotuneHasTrustedCapacityReferences, autotuneCalibrationStrategy, ` +
			`autotuneRunningRequestMatches, ` +
			`autotuneAchievedGrade, autotuneGradeTone, ` +
			`autotuneProposalMatchesProfile, autotuneProposalCandidates, ` +
			`autotuneResultEnvelopeValidated, autotuneResultEvidenceValidated, ` +
			`autotuneValidationGatesComplete, autotuneProfileOutcomeValidated, ` +
			`autotuneFairOutcomeValidated, autotuneResultValidated, autotuneResultReviewable, ` +
			`autotuneDirectionalProposalEvidenceValidated, autotuneCandidateAcknowledgementRequirements, ` +
			`autotuneCandidateRealizationReconciled, ` +
			`autotuneAcknowledgableGateFailures, autotuneGateAcknowledgementsComplete, ` +
			`autotuneResultHasReviewChoice, autotuneDefaultReviewAction, ` +
			`autotuneConfidence, autotuneResultClass, autotuneBackgroundAwareResult, ` +
			`autotunePhaseEvidenceUsable, ` +
			`autotuneConservativeAvailable, autotunePhaseEvidenceClean, ` +
			`multiwanAutotuneItemNativeApplied, multiwanAutotuneItemAccepted, ` +
			`multiwanAutotuneItemDecided, multiwanAutotuneBatchDecided, ` +
			`multiwanAutotunePendingPlans, multiwanAutotunePendingStagedApplyItems, ` +
			`multiwanAutotuneItemCanSkip, ` +
			`autotuneDisableSqmEvidenceValidated, autotuneRawNoSqmEvidenceValidated, ` +
			`autotuneAttemptDiagnostics, autotuneDiagnostics, ` +
			`autotuneRawControlRows, ` +
			`autotuneCpuSustainedSummary, ` +
			`autotuneRuntimeSettled, autotuneLegacyResult, ` +
			`autotuneRecoveryPending, autotuneRecoveryProgress, ` +
			`revalidateAutotuneProposal, ` +
			`stageAutotuneApplyMarker, pendingAutotuneApplyMarkers, ` +
			`armAutotuneApplyGuards, runGuardedSaveApply, discardStagedUciPackages, ` +
			`reconcileConfirmedUciPackages, ` +
			`changedUciPackages, validateAutotuneStagedTransaction, requireCleanUciTransaction, applyPlainRollbackTransaction, ` +
			`runSequentialAutotuneApplies, ` +
			`clearAutotuneProposalState, recordAutotuneTerminalFailure, ` +
			`autotuneRetryableInconclusive, autotuneMeasurementTimeout, autotuneRecommendedProfile, recordAutotuneRetryableInconclusive, ` +
			`manualSqmDirectionMode, writeManualSqmDirectionMode, ` +
			`positiveRateValue, shouldImportInterfaceRates, applyRatePreset, ` +
			`adaptiveCeilingWritePlan, runAutotuneJob, cancelAutotuneJob, ` +
			`runSpeedtestJob, nativeSpeedtestCapabilityValidated, nativeSpeedtestLaunchArgs, nativeSpeedtestResultValidated, ` +
			`nativeAutotuneCapabilityValidated, nativeBootstrapAutotuneCapabilityValidated, ` +
			`nativeAutotuneIntentSupported, nativeAutotuneLaunchArgs, nativeAutotuneResultMatchesRequest, ` +
			`nativeAutotuneProgressStepLabel, nativeAutotuneProgress, ` +
			`currentActiveNativeAutotuneJob, runPreferredAutotuneJob, cancelPreferredAutotuneJob, ` +
			`replaceNodeContent, ` +
			`setNativeAutotuneJob: function(section, jobId) { nativeAutotuneJobs[section] = jobId; autotuneTransportModes[section] = 'native'; }, ` +
			`setLegacyAutotuneJob: function(section, runId) { legacyAutotuneJobs[section] = runId; autotuneTransportModes[section] = 'legacy'; }, ` +
			`setInterfaceContext: function(value) { interfaceContext = value; }, ` +
			`setMwan3Context: function(value) { mwan3Context = value; } };`
	)(fsImpl || {}, {}, {}, uciImpl || uci, {}, {}, {}, rpcImpl || {
		declare() { return () => Promise.resolve(0); },
	}, lImpl || {}, eImpl || (() => ({})), value => value);
}

const helpers = compileHelpers({});
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
assert.equal(helpers.manualSqmDirectionMode('off'), 'both',
	'the manual selector must not expose the stopped/no-SQM topology');
assert.match(source,
	/listValue\(section, 'sqm_basic', 'sqm_direction_mode', _\('CAKE directions'\)[\s\S]*?Upload only — no download\/ingress CAKE[\s\S]*?Download only — no upload\/egress CAKE/,
	'Edit -> SQM setup must expose all three running direction topologies');

const directionState = { sqm_direction_mode: 'both' };
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
assert.throws(() => directionHelpers.writeManualSqmDirectionMode('wan_sqm', 'off'),
	/CAKE directions must be Both, Upload only, or Download only/,
	'the manual selector must fail closed instead of stopping SQM implicitly');

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
assert.match(source, /Simultaneous DL\+UL confirmation/,
	'Review must expose the final simultaneous direction confirmation');
assert.match(source, /Directional CAKE proposal/,
	'Review must expose repeatable one-sided CAKE proposals');
assert.match(source, /directional_comparisons/,
	'Review must consume both upload-only and download-only comparisons');
assert.match(source, /Raw capacity controls/,
	'Review must expose the raw physical-capacity controls instead of hiding them in JSON');
assert.match(source, /No measured throughput benefit from disabling SQM/,
	'raw no-SQM throughput trade-offs must be rendered as user-facing text rather than backend codes');
assert.match(source, /Unshaped loaded latency is worse than shaped/,
	'raw no-SQM latency trade-offs must be rendered as user-facing text rather than backend codes');
assert.match(source,
	/selectedAutotuneAction === 'disable_sqm'[\s\S]*?disable_sqm_confirmed[\s\S]*?disables CAKE shaping/,
	'every no-SQM proposal must retain an explicit disable-SQM acknowledgement');
const typedProposalConfiguration = { download: { base_kbps: 100000 }, upload: { base_kbps: 20000 } };
const typedCandidates = [ {
	schema_version: 1,
	proposal_id: 'p-0123456789abcdef01234567',
	rank: 1,
	action: 'apply_sqm',
	topology: 'both_shaped',
	is_primary: true,
	applicable: true,
	hard_safety_pass: true,
	profile_target_met: false,
	profile_objectives_met: true,
	grade: 'B',
	effective_delta_ms: 42,
	confidence_percent: 73,
	unmet_objectives: [ 'profile-target' ],
	evidence: { validation: 'validation', confirmation: 'bidirectional_confirmation' },
	configuration: typedProposalConfiguration,
} ];
assert.equal(helpers.autotuneProposalCandidates({
	proposal: typedProposalConfiguration, proposals: typedCandidates,
}), typedCandidates, 'a safe typed proposal list must retain its exact candidate objects');
assert.equal(helpers.autotuneProposalCandidates({
	proposal: typedProposalConfiguration,
	proposals: [ { ...typedCandidates[0], hard_safety_pass: false } ],
}), null, 'a candidate which fails hard safety must never enter the review list');
assert.equal(helpers.autotuneProposalCandidates({
	proposal: typedProposalConfiguration,
	proposals: [ typedCandidates[0], { ...typedCandidates[0], rank: 2 } ],
}), null, 'proposal identifiers must be unique within one immutable result');
const uploadOnlyCandidate = {
	...typedCandidates[0],
	proposal_id: 'p-111111111111111111111111',
	topology: 'upload_only_shaped',
	grade: 'B',
	effective_delta_ms: 35,
	profile_objectives_met: false,
	unmet_objectives: [ 'profile-target', 'retention-objective', 'download-sqm-disabled' ],
	evidence: { recommendation: 'directional_comparisons.upload_only' },
};
const uploadOnlyResult = {
	profile: 'best_overall',
	proposal: {
		...typedProposalConfiguration,
		download: { ...typedProposalConfiguration.download, base_kbps: 100000 },
		upload: { ...typedProposalConfiguration.upload, base_kbps: 20000 },
	},
	validation_thresholds: {
		candidate_realization_min_percent: 80,
		candidate_realization_max_percent: 110,
		delay_max_ms: 45,
		manual_latency_review_max_ms: 60,
		loss_max_percent: 3,
	},
	validation: {
		effective_delta_ms: 40,
	},
	bidirectional_confirmation: {
		effective_delta_ms: 40,
		achieved_kbps: { download: 80000, upload: 18000 },
	},
	directional_comparisons: {
		upload_only: {
			tested: true,
			recommended_topology: 'upload_only_shaped',
			reason: 'repeatable-download-bypass-benefit',
			repeatable: true,
			observations: [
				{
					pass: true, candidate_pass: true, hard_safety_pass: true, material_benefit: true, grade: 'B',
					effective_delta_ms: 35, loss_percent: 0,
					upload_realization_percent: 90, download_gain_percent: 10,
					delay_improvement_ms: 5,
					observation: {
						topology: 'upload_only_shaped', direction: 'both',
						throughput_kbps: { download_kbps: 88000, upload_kbps: 18000 },
						measurement_evidence: { valid: true, shaper_bypassed: true,
							sqm_paused: false, sqm_bypass_mode: 'ingress-only-autotune' },
					},
				},
				{
					pass: true, candidate_pass: true, hard_safety_pass: true, material_benefit: true, grade: 'B',
					effective_delta_ms: 34, loss_percent: 0,
					upload_realization_percent: 91, download_gain_percent: 8.75,
					delay_improvement_ms: 6,
					observation: {
						topology: 'upload_only_shaped', direction: 'both',
						throughput_kbps: { download_kbps: 87000, upload_kbps: 18200 },
						measurement_evidence: { valid: true, shaper_bypassed: true,
							sqm_paused: false, sqm_bypass_mode: 'ingress-only-autotune' },
					},
				},
			],
		},
	},
};
assert.equal(helpers.autotuneDirectionalProposalEvidenceValidated(
	uploadOnlyResult, uploadOnlyCandidate), true,
'repeatable one-sided evidence must independently validate its selected proposal');
const nonRepeatableStrictUpload = structuredClone(uploadOnlyResult);
const nonRepeatableComparison = nonRepeatableStrictUpload.directional_comparisons.upload_only;
nonRepeatableComparison.recommended_topology = 'manual_review';
nonRepeatableComparison.reason = 'upload-only-benefit-not-repeatable';
nonRepeatableComparison.repeatable = false;
nonRepeatableComparison.observations[0].effective_delta_ms = 10;
nonRepeatableComparison.observations[0].grade = 'A';
nonRepeatableComparison.observations[0].delay_improvement_ms = 30;
nonRepeatableComparison.observations[0].pass = true;
nonRepeatableComparison.observations[1].effective_delta_ms = 25;
nonRepeatableComparison.observations[1].grade = 'A';
nonRepeatableComparison.observations[1].delay_improvement_ms = 15;
nonRepeatableComparison.observations[1].pass = true;
const nonRepeatableStrictCandidate = {
	...uploadOnlyCandidate,
	grade: 'A',
	effective_delta_ms: 25,
	confidence_percent: 40,
	unmet_objectives: uploadOnlyCandidate.unmet_objectives.concat('measurement-confidence'),
};
assert.equal(helpers.autotuneDirectionalProposalEvidenceValidated(
	nonRepeatableStrictUpload, nonRepeatableStrictCandidate), true,
	'two strict but latency-variable one-sided observations may be offered at low confidence');
assert.equal(helpers.autotuneDirectionalProposalEvidenceValidated(
	nonRepeatableStrictUpload, { ...nonRepeatableStrictCandidate,
		unmet_objectives: uploadOnlyCandidate.unmet_objectives }), false,
	'a non-repeatable one-sided proposal must require its measurement-confidence acknowledgement');
assert.equal(helpers.autotuneDirectionalProposalEvidenceValidated(
	nonRepeatableStrictUpload, { ...nonRepeatableStrictCandidate,
		confidence_percent: null }), false,
	'a non-repeatable one-sided proposal must carry an explicit bounded confidence');
const forgedStrictPass = structuredClone(nonRepeatableStrictUpload);
forgedStrictPass.directional_comparisons.upload_only.observations[1].effective_delta_ms = 50;
forgedStrictPass.directional_comparisons.upload_only.observations[1].grade = 'B';
forgedStrictPass.directional_comparisons.upload_only.observations[1].delay_improvement_ms = -10;
assert.equal(helpers.autotuneDirectionalProposalEvidenceValidated(
	forgedStrictPass, { ...nonRepeatableStrictCandidate,
		grade: 'B', effective_delta_ms: 50 }), false,
	'a copied strict-pass flag must not bypass the profile delay objective');
const tamperedUploadOnly = structuredClone(uploadOnlyResult);
tamperedUploadOnly.directional_comparisons.upload_only.observations[1].upload_realization_percent = 100;
assert.equal(helpers.autotuneDirectionalProposalEvidenceValidated(
	tamperedUploadOnly, uploadOnlyCandidate), false,
'reported directional realization must be recomputed from measured throughput');
const safeNonBeneficialUpload = structuredClone(uploadOnlyResult);
safeNonBeneficialUpload.directional_comparisons.upload_only.recommended_topology = 'manual_review';
safeNonBeneficialUpload.directional_comparisons.upload_only.reason = 'upload-only-benefit-not-repeatable';
for (const item of safeNonBeneficialUpload.directional_comparisons.upload_only.observations) {
	item.candidate_pass = false;
	item.material_benefit = false;
	item.pass = false;
	item.download_gain_percent = 0;
	item.observation.throughput_kbps.download_kbps = 80000;
}
const safeNonBeneficialCandidate = {
	...uploadOnlyCandidate,
	unmet_objectives: uploadOnlyCandidate.unmet_objectives.concat('throughput-benefit-unproven'),
};
assert.equal(helpers.autotuneDirectionalProposalEvidenceValidated(
	safeNonBeneficialUpload, safeNonBeneficialCandidate), true,
'a repeatable one-sided topology inside every hard gate must remain reviewable when utility benefit is unproven');
assert.equal(helpers.autotuneDirectionalProposalEvidenceValidated(
	safeNonBeneficialUpload, uploadOnlyCandidate), false,
'the one-sided utility warning must be bound to the selected candidate');
const higherValidationReferenceUpload = structuredClone(uploadOnlyResult);
higherValidationReferenceUpload.validation.effective_delta_ms = 60;
higherValidationReferenceUpload.directional_comparisons.upload_only.recommended_topology = 'manual_review';
higherValidationReferenceUpload.directional_comparisons.upload_only.reason = 'upload-only-benefit-not-repeatable';
for (const item of higherValidationReferenceUpload.directional_comparisons.upload_only.observations) {
	item.effective_delta_ms = item === higherValidationReferenceUpload.directional_comparisons.upload_only.observations[0] ? 55 : 54;
	item.delay_improvement_ms =
		higherValidationReferenceUpload.bidirectional_confirmation.effective_delta_ms - item.effective_delta_ms;
	item.material_benefit = false;
	item.candidate_pass = false;
	item.pass = false;
}
const higherValidationReferenceCandidate = {
	...uploadOnlyCandidate,
	effective_delta_ms: 55,
	unmet_objectives: uploadOnlyCandidate.unmet_objectives.concat('throughput-benefit-unproven'),
};
assert.equal(helpers.autotuneDirectionalProposalEvidenceValidated(
	higherValidationReferenceUpload, higherValidationReferenceCandidate), true,
'the latency warning must use the worse shaped validation and bidirectional reference');
const wrongImprovementReferenceUpload = structuredClone(higherValidationReferenceUpload);
wrongImprovementReferenceUpload.directional_comparisons.upload_only.observations[0]
	.delay_improvement_ms = 5;
assert.equal(helpers.autotuneDirectionalProposalEvidenceValidated(
	wrongImprovementReferenceUpload, higherValidationReferenceCandidate), false,
'directional delay improvement must remain relative to the bidirectional confirmation');
const lowRealizationUpload = structuredClone(uploadOnlyResult);
lowRealizationUpload.directional_comparisons.upload_only.observations[0]
	.upload_realization_percent = 70;
lowRealizationUpload.directional_comparisons.upload_only.observations[0]
	.observation.throughput_kbps.upload_kbps = 14000;
lowRealizationUpload.directional_comparisons.upload_only.observations[0].pass = false;
lowRealizationUpload.directional_comparisons.upload_only.observations[1]
	.upload_realization_percent = 71;
lowRealizationUpload.directional_comparisons.upload_only.observations[1]
	.observation.throughput_kbps.upload_kbps = 14200;
lowRealizationUpload.directional_comparisons.upload_only.observations[1].pass = false;
const lowRealizationCandidate = {
	...uploadOnlyCandidate,
	unmet_objectives: uploadOnlyCandidate.unmet_objectives.concat('candidate-realization'),
};
assert.equal(helpers.autotuneDirectionalProposalEvidenceValidated(
	lowRealizationUpload, lowRealizationCandidate), true,
'a 50-80% one-sided realization must remain reviewable with an explicit acknowledgement');
assert.equal(helpers.autotuneDirectionalProposalEvidenceValidated(
	lowRealizationUpload, uploadOnlyCandidate), false,
'a one-sided realization objective miss must not be accepted without its warning');
const downloadOnlyCandidate = {
	...uploadOnlyCandidate,
	proposal_id: 'p-222222222222222222222222',
	topology: 'download_only_shaped',
	unmet_objectives: [ 'profile-target', 'retention-objective', 'upload-sqm-disabled' ],
	evidence: { recommendation: 'directional_comparisons.download_only' },
};
const downloadOnlyResult = structuredClone(uploadOnlyResult);
downloadOnlyResult.directional_comparisons = {
	download_only: {
		tested: true,
		recommended_topology: 'download_only_shaped',
		reason: 'repeatable-upload-bypass-benefit',
		repeatable: true,
		observations: [
			{
				pass: true, candidate_pass: true, hard_safety_pass: true, material_benefit: true, grade: 'B',
				effective_delta_ms: 35, loss_percent: 0,
				download_realization_percent: 90, upload_gain_percent: 10,
				delay_improvement_ms: 5,
				observation: {
					topology: 'download_only_shaped', direction: 'both',
					throughput_kbps: { download_kbps: 90000, upload_kbps: 19800 },
					measurement_evidence: { valid: true, shaper_bypassed: true,
						sqm_paused: false, sqm_bypass_mode: 'egress-only-autotune' },
				},
			},
			{
				pass: true, candidate_pass: true, hard_safety_pass: true, material_benefit: true, grade: 'B',
				effective_delta_ms: 34, loss_percent: 0,
				download_realization_percent: 91, upload_gain_percent: 8.89,
				delay_improvement_ms: 6,
				observation: {
					topology: 'download_only_shaped', direction: 'both',
					throughput_kbps: { download_kbps: 91000, upload_kbps: 19600 },
					measurement_evidence: { valid: true, shaper_bypassed: true,
						sqm_paused: false, sqm_bypass_mode: 'egress-only-autotune' },
				},
			},
		],
	},
};
assert.equal(helpers.autotuneDirectionalProposalEvidenceValidated(
	downloadOnlyResult, downloadOnlyCandidate), true,
'repeatable download-only evidence must receive the same independent validation');
const fourTopologyCandidates = [
	{ ...typedCandidates[0], rank: 1 },
	{ ...uploadOnlyCandidate, rank: 2, is_primary: false },
	{ ...downloadOnlyCandidate, rank: 3, is_primary: false },
	{
		...typedCandidates[0], proposal_id: 'p-333333333333333333333333', rank: 4,
		action: 'disable_sqm', topology: 'no_sqm', is_primary: false,
		unmet_objectives: [ 'profile-target' ],
		evidence: { control: 'raw_control' }, configuration: null,
	},
];
assert.equal(helpers.autotuneProposalCandidates({
	proposal: typedProposalConfiguration, proposals: fourTopologyCandidates,
}), fourTopologyCandidates, 'all four independently evidenced topologies must fit in Review');
assert.equal(helpers.autotuneProposalCandidates({
	proposal: typedProposalConfiguration,
	proposals: fourTopologyCandidates.concat({
		...fourTopologyCandidates[3], proposal_id: 'p-444444444444444444444444', rank: 5,
	}),
}), null, 'a fifth candidate must be rejected until the schema explicitly raises its bound');
const candidateRequirements = helpers.autotuneCandidateAcknowledgementRequirements({
	proposal: typedProposalConfiguration, proposals: [ uploadOnlyCandidate ],
}, 'apply_sqm', uploadOnlyCandidate.proposal_id);
assert.deepEqual(candidateRequirements.map((item) => item.code), [
	'proposal-profile-target', 'proposal-retention-objective', 'proposal-download-sqm-disabled'
], 'every selected proposal trade-off must require its own acknowledgement');
const rawControlRows = helpers.autotuneRawControlRows({ runs: [
	{
		test_direction: 'both', download_kbps: 462580, upload_kbps: 57526,
		shaper_bypassed: true, sqm_bypass_mode: 'paused-managed', sqm_paused: true,
		route_traffic_proof: { available: true, passed: true },
		backend_title: 'speedtest-go', server_name: 'Test server',
	},
	{
		test_direction: 'download', download_kbps: 423205, upload_kbps: null,
		shaper_bypassed: true, sqm_bypass_mode: 'ingress-only-managed', sqm_paused: false,
		route_traffic_proof: { available: true, passed: true },
		backend: 'speedtest-go', server_name: 'Test server',
	},
	{
		test_direction: 'upload', download_kbps: null, upload_kbps: 59943,
		shaper_bypassed: true, sqm_bypass_mode: 'egress-only-managed', sqm_paused: false,
		route_traffic_proof: { available: true, passed: false },
		backend: 'speedtest-go', server_name: 'Test server',
	},
	{ test_direction: 'download', download_kbps: null, upload_kbps: null },
] });
assert.deepEqual(rawControlRows.map(row => ({
	direction: row.direction,
	download_kbps: row.download_kbps,
	upload_kbps: row.upload_kbps,
	verified: row.verified,
})), [
	{ direction: 'both', download_kbps: 462580, upload_kbps: 57526, verified: true },
	{ direction: 'download', download_kbps: 423205, upload_kbps: null, verified: true },
	{ direction: 'upload', download_kbps: null, upload_kbps: 59943, verified: false },
], 'raw controls must validate direction-specific bypass and route proof independently');
assert.match(source, /no topology change is selected automatically/,
	'directional evidence must require an explicit proposal selection before changing runtime topology');
assert.match(source, /directional-bypass-not-authorized/,
	'shaped-only calibration must explain why no raw directional comparison was attempted');
assert.match(source, /Monthly traffic budget/,
	'scheduled active calibration must expose a persistent monthly allowance');

assert.equal(helpers.autotuneAchievedGrade({
	validation: { actual_grade: 'b' },
	profile_outcome: { actual_grade: 'A+' }
}), 'A+', 'the final profile outcome must be the authoritative achieved class');
assert.equal(helpers.autotuneAchievedGrade({
	profile_outcome: { actual_grade: 'C' }
}), 'C', 'the profile outcome may supply the achieved class when validation omitted it');
assert.equal(helpers.autotuneAchievedGrade({ validation: { actual_grade: 'incomplete' } }), null,
	'incomplete or unknown grades must not produce a misleading badge');
assert.equal(helpers.autotuneGradeTone('A+'), 'good');
assert.equal(helpers.autotuneGradeTone('A'), 'good');
assert.equal(helpers.autotuneGradeTone('B'), 'warning');
assert.equal(helpers.autotuneGradeTone('C'), 'warning');
assert.equal(helpers.autotuneGradeTone('D'), 'bad');
assert.equal(helpers.autotuneGradeTone('F'), 'bad');
assert.match(source, /Achieved class:/,
	'proposal diagnostics must show the actual class achieved during calibration');
assert.match(source, /Selected target: %s/,
	'the achieved class must remain distinct from a different selected target');

async function testSequentialMultiwanTransactions() {
	const events = [];
	const results = await helpers.runSequentialAutotuneApplies([ 'wan', 'wanb' ], async item => {
		events.push(`start:${item}`);
		await Promise.resolve();
		events.push(`end:${item}`);
		return `${item}_sqm`;
	});
	assert.deepEqual(events, [ 'start:wan', 'end:wan', 'start:wanb', 'end:wanb' ],
		'Multi-WAN applies must never overlap');
	assert.deepEqual(results, [ 'wan_sqm', 'wanb_sqm' ]);

	const stopped = [];
	await assert.rejects(helpers.runSequentialAutotuneApplies([ 'wan', 'wanb', 'third' ], item => {
		stopped.push(item);
		return item === 'wanb' ? Promise.reject(new Error('guard rejected')) : Promise.resolve(item);
	}), /guard rejected/);
	assert.deepEqual(stopped, [ 'wan', 'wanb' ],
		'a failed guarded uplink must stop later transactions');

	const calls = [];
	const cleanUci = {
		save() { calls.push('save'); return Promise.resolve(); },
		changes() { calls.push('changes'); return Promise.resolve({ 'cake-autorate': [ [ 'set' ] ] }); },
		callApply(timeout, rollback) {
			calls.push(`apply:${timeout}:${rollback}`);
			return Promise.resolve(0);
		},
	};
	const transaction = compileHelpers({}, cleanUci, {}, {
		declare() { return () => { calls.push('confirm'); return Promise.resolve(0); }; },
	});
	assert.equal(await transaction.applyPlainRollbackTransaction([ 'cake-autorate' ]), true);
	assert.deepEqual(calls, [ 'save', 'changes', 'apply:30:true', 'confirm' ]);

	const reverted = [];
	const cacheEvents = [];
	const cleanup = compileHelpers({}, {
		unload(packages) { cacheEvents.push([ 'unload', packages ]); },
	}, {}, {
		declare(spec) {
			if (spec.method === 'revert')
				return config => { reverted.push(config); return Promise.resolve(0); };
			return () => Promise.resolve(0);
		},
	});
	assert.deepEqual(await cleanup.discardStagedUciPackages([ 'cake-autorate', 'sqm' ]),
		[ 'cake-autorate', 'sqm' ]);
	assert.deepEqual(reverted, [ 'cake-autorate', 'sqm' ],
		'rollback cleanup must revert both packages in the active browser RPC session');
	assert.deepEqual(cacheEvents, [ [ 'unload', [ 'cake-autorate', 'sqm' ] ] ],
		'rollback cleanup must unload the rejected local UCI cache before reload');

	const dirtyGuard = compileHelpers({}, {
		changes() {
			return Promise.resolve({
				'cake-autorate': [
					[ 'set', 'wan_sqm', 'min_dl_shaper_rate_kbps', '620900' ],
				],
			});
		},
	}, {}, {
		declare() { return () => Promise.resolve(0); },
	});
	await assert.rejects(
		dirtyGuard.requireCleanUciTransaction('clean transaction required'),
		/clean transaction required/,
		'single-WAN Full Auto-Tune staging must stop before mixing with stale UCI deltas'
	);

	const disableMarker = {
		job: 'wan_sqm',
		target: 'pppoe-wan',
		backend: 'speedtest-go',
		routeMode: 'main',
		member: '',
		enabled: '0',
		disableAdaptive: '0',
		action: 'disable_sqm',
		proposalId: `p-${'3'.repeat(24)}`,
		fingerprint: `sha256:${'4'.repeat(64)}`,
		token: '5'.repeat(64),
		expires: '2000000000',
		bootId: '11111111-2222-3333-4444-555555555555',
	};
	assert.doesNotThrow(() => helpers.validateAutotuneStagedTransaction([ disableMarker ], {
		'cake-autorate': [
			[ 'set', 'cake-autorate', 'wan_sqm', 'enabled', '0' ],
			[ 'set', 'wan_sqm', 'sqm_enabled', '0' ],
			[ 'set', 'wan_sqm', 'sqm_direction_mode', 'off' ],
			[ 'set', 'wan_sqm', '_autotune_apply_guard', '1' ],
			[ 'set', 'wan_sqm', '_autotune_apply_fingerprint', disableMarker.fingerprint ],
			[ 'set', 'wan_sqm', '_autotune_apply_target', 'pppoe-wan' ],
			[ 'set', 'wan_sqm', '_autotune_apply_backend', 'speedtest-go' ],
			[ 'set', 'wan_sqm', '_autotune_apply_route_mode', 'main' ],
			[ 'delete', 'wan_sqm', '_autotune_apply_mwan3_member' ],
			[ 'set', 'wan_sqm', '_autotune_apply_enabled', '0' ],
			[ 'set', 'wan_sqm', '_autotune_apply_disable_adaptive', '0' ],
			[ 'set', 'wan_sqm', '_autotune_apply_action', 'disable_sqm' ],
			[ 'set', 'wan_sqm', '_autotune_apply_proposal_id', disableMarker.proposalId ],
			[ 'set', 'wan_sqm', '_autotune_apply_token', disableMarker.token ],
			[ 'set', 'wan_sqm', '_autotune_apply_expires', '2000000000' ],
			[ 'set', 'wan_sqm', '_autotune_apply_boot_id', disableMarker.bootId ],
		],
		sqm: [
			[ 'add', 'sqm', 'cake_autorate_apply_guard', 'cake_autorate_apply_wan_sqm' ],
			[ 'set', 'cake_autorate_apply_wan_sqm', '_autotune_apply_guard', '1' ],
			[ 'set', 'cake_autorate_apply_wan_sqm', '_autotune_apply_job', 'wan_sqm' ],
			[ 'set', 'cake_autorate_apply_wan_sqm', '_autotune_apply_fingerprint', disableMarker.fingerprint ],
			[ 'set', 'cake_autorate_apply_wan_sqm', '_autotune_apply_token', disableMarker.token ],
		],
	}), 'the exact disable-SQM browser transaction must pass the client allowlist');
	assert.throws(() => helpers.validateAutotuneStagedTransaction([ disableMarker ], {
		'cake-autorate': [
			[ 'set', 'wan_sqm', 'enabled', '0' ],
			[ 'set', 'wan_sqm', 'sqm_enabled', '0' ],
			[ 'set', 'wan_sqm', 'sqm_direction_mode', 'off' ],
			[ 'set', 'wan_sqm', 'min_dl_shaper_rate_kbps', '620900' ],
		],
		sqm: [],
	}), /unrelated CAKE change/,
	'a stale shaped-proposal rate delta must never ride inside a disable-SQM transaction');
	assert.throws(() => helpers.validateAutotuneStagedTransaction([ disableMarker ], {
		'cake-autorate': [
			[ 'set', 'wan_sqm', 'enabled', '0' ],
			[ 'set', 'wan_sqm', 'sqm_enabled', '0' ],
			[ 'set', 'wan_sqm', 'sqm_direction_mode', 'off' ],
		],
		sqm: [
			[ 'set', 'cake_wan_sqm', 'enabled', '0' ],
		],
	}), /unrelated SQM change/,
	'only the rollback guard marker may touch the SQM package before service apply');
}

assert.equal(helpers.topicTab('setup'), 'autorate');
assert.equal(helpers.topicTab('general'), 'autorate');
assert.equal(helpers.topicTab('sqm_qdisc'), 'sqm');
assert.equal(helpers.topicTab('speedtest'), 'testing');
assert.equal(helpers.topicTab('logging'), 'monitoring');
assert.equal(helpers.autorateSubcategory('setup', 'wan_if'), 'connection');
assert.equal(helpers.autorateSubcategory('setup', 'min_dl_shaper_rate_kbps'), 'limits');
assert.equal(helpers.autorateSubcategory('rates', 'adaptive_ceiling_enabled'), 'ceiling');
assert.equal(helpers.autorateSubcategory('rates', 'runtime_learning_mode'), 'ceiling');
assert.equal(helpers.autorateSubcategory('reflectors', 'reflector'), 'probes');
assert.equal(helpers.autorateSubcategory('quality', 'transport_probe_backend'), 'probes');
assert.equal(helpers.autorateSubcategory('quality', 'rating_load_enter_ratio'), 'quality');
assert.equal(helpers.autorateSubcategory('controller', 'alpha_delta_ewma'), 'controller');
assert.deepEqual(helpers.autorateSubcategoryDefinitions().map(group => group.id),
	[ 'connection', 'limits', 'ceiling', 'probes', 'quality', 'controller' ]);
assert.equal(helpers.canonicalAutotuneProfile('balanced'), 'best_overall');
assert.equal(helpers.canonicalAutotuneProfile('gaming-extreme'), 'gaming_extreme');
assert.equal(helpers.canonicalAutotuneProfile('unknown'), null);
assert.deepEqual(helpers.autotuneProfileDefinitions().map(profile => profile.id),
	[ 'gaming', 'gaming_extreme', 'best_overall', 'variable_link', 'fair' ]);
assert.equal(helpers.autotuneProfileDefinitions()[1].hidden, true);
assert.equal(helpers.autotuneProfileDefinitions().every(profile =>
	/(?:no|without a) fixed/i.test(profile.description)), true,
	'profile help must say that retention and exploration policy do not pre-cut the initial rate');
assert.equal(helpers.autotuneProfilePolicy('gaming').sqm.classification, 'diffserv4');
assert.equal(helpers.autotuneProfilePolicy('gaming').delayMaxMs, 5);
assert.equal(helpers.autotuneProfilePolicy('gaming_extreme').retentionPercent, 70);
assert.equal(helpers.visibleAutotuneProfile('gaming_extreme'), 'gaming');
assert.equal(helpers.autotuneRunProfile({
	autotune_profile: 'gaming', autotune_extreme_a_plus: true,
}), 'gaming_extreme');
assert.equal(helpers.storedAutotuneProfile('gaming_extreme'), 'gaming');
assert.equal(helpers.autotuneProfilePolicy('best_overall').retentionPercent, 80);
assert.equal(helpers.autotuneProfilePolicy('best_overall').sqm.script, 'layer_cake.qos');
assert.equal(helpers.autotuneProfilePolicy('best_overall').sqm.iqdiscOpts, 'besteffort');
assert.equal(helpers.autotuneProfilePolicy('best_overall').sqm.eqdiscOpts, 'diffserv4');
assert.equal(helpers.autotuneProfilePolicy('variable_link').targetGrade, 'B');
assert.equal(helpers.autotuneProfilePolicy('variable_link').retentionPercent, 70);
assert.equal(helpers.autotuneProfilePolicy('variable_link').delayMaxMs, 60);
assert.equal(helpers.autotuneProfilePolicy('fair').retentionPercent, 90);
assert.equal(helpers.accessMediumExplorationPercent('cellular'), 35);
assert.equal(helpers.accessMediumExplorationPercent('leo_satellite'), 35);
assert.equal(helpers.accessMediumExplorationPercent('fixed_wireless'), 40);
assert.equal(helpers.accessMediumExplorationPercent('shared_wired'), 50);
assert.equal(helpers.accessMediumExplorationPercent('unknown'), 50);
assert.equal(helpers.accessMediumTitle('leo_satellite'), 'LEO satellite');
assert.deepEqual(helpers.detectAccessMedium('pppoe-wan', {
	deviceProtocols: { 'pppoe-wan': [ 'pppoe' ] },
	deviceTypes: { 'pppoe-wan': 'ethernet' },
	networkDevices: {},
}), {
	medium: 'unknown',
	source: 'auto_inconclusive',
	confidence_percent: 20,
	reason: 'No trustworthy physical-medium signal was found. Ethernet, DHCP and PPPoE are transport details, not proof of the provider medium.',
}, 'PPPoE and Ethernet encapsulation must not be mistaken for a fixed provider medium');
assert.deepEqual(helpers.detectAccessMedium('wwan0', {
	deviceProtocols: { wwan0: [ 'qmi' ] },
	deviceTypes: {},
	networkDevices: {},
}), {
	medium: 'cellular',
	source: 'network_protocol',
	confidence_percent: 95,
	reason: 'A direct cellular modem protocol was found for this interface.',
}, 'a direct modem protocol is strong enough for automatic cellular classification');
const qmiContext = helpers.buildInterfaceContext([
	{ getName: () => 'wwan0', getType: () => 'ethernet', isUp: () => true },
], [
	{ getName: () => 'wwan', getIfname: () => 'wwan0', getProtocol: () => 'qmi',
		getL2Device: () => null },
]);
assert.equal(helpers.detectAccessMedium('wwan', qmiContext).medium, 'cellular',
	'a logical network selection must resolve to its direct modem protocol');
const wirelessContext = helpers.buildInterfaceContext([
	{ getName: () => 'wlan0', getType: () => 'wifi', isUp: () => true },
	{ getName: () => 'uplink0', getType: () => 'network', isUp: () => true },
], [
	{ getName: () => 'wan', getIfname: () => 'uplink0', getProtocol: () => 'dhcp',
		getL2Device: () => ({ getName: () => 'wlan0' }) },
]);
assert.equal(helpers.detectAccessMedium('wan', wirelessContext).medium, 'fixed_wireless',
	'a wireless L2 carrier must be visible through its logical WAN device');
assert.equal(helpers.recommendedCapacityLearningPolicy({
	medium: 'unknown', confidence_percent: 20,
}), 'verified_only');
assert.equal(helpers.recommendedCapacityLearningPolicy({
	medium: 'cellular', confidence_percent: 95,
}), 'passive_bounded');
assert.throws(() => helpers.autotuneAccessRequest({
	autotune_profile: 'variable_link', access_medium_selection: 'cellular',
	capacity_learning_policy: 'fixed_cap', service_dl_cap_kbps: '100000',
}), /requires download and upload service caps between 100 and 100000000/,
'fixed-cap Variable Link calibration must not start with a missing direction');
assert.throws(() => helpers.autotuneAccessRequest({
	autotune_profile: 'variable_link', access_medium_selection: 'cellular',
	capacity_learning_policy: 'fixed_cap', service_dl_cap_kbps: '99',
	service_ul_cap_kbps: '10000',
}), /requires download and upload service caps between 100 and 100000000/,
'fixed-cap Variable Link calibration must reject an unsupported tiny service cap');
assert.throws(() => helpers.autotuneAccessRequest({
	autotune_profile: 'best_overall', service_dl_cap_kbps: '100000',
}, true), /Creating a native Auto-Tune instance requires download and upload service caps/,
'bootstrap authority must require both caps for every profile, not only Variable Link');
assert.deepEqual(helpers.autotuneAccessRequest({
	autotune_profile: 'best_overall', service_dl_cap_kbps: '100000',
	service_ul_cap_kbps: '50000',
}, true), {
	medium: 'unknown', source: 'legacy_default', confidence_percent: 0,
	policy: '', service_dl_cap_kbps: '100000', service_ul_cap_kbps: '50000',
}, 'ordinary profiles must preserve explicit bootstrap caps as policy authority');
assert.equal(helpers.autotuneRunningRequestMatches({
	state: 'running', job_id: 'wan_sqm', requested_target_interface: 'pppoe-wan',
	requested_backend: 'speedtest-go', requested_route_mode: 'mwan3',
	requested_mwan3_member: 'wan', requested_profile: 'best_overall',
	requested_conservative: false, requested_calibration_strategy: 'shaped_only',
}, 'wan_sqm', 'pppoe-wan', 'speedtest-go', 'mwan3', 'wan', 'best_overall', false,
'shaped_only'), true);
assert.equal(helpers.autotuneRunningRequestMatches({
	state: 'running', job_id: 'wan_sqm', requested_target_interface: 'pppoe-wan',
	requested_backend: 'speedtest-go', requested_route_mode: 'mwan3',
	requested_mwan3_member: 'wan', requested_profile: 'best_overall',
	requested_conservative: false, requested_calibration_strategy: 'full_raw',
}, 'wan_sqm', 'pppoe-wan', 'speedtest-go', 'mwan3', 'wan', 'best_overall', false,
'shaped_only'), false, 'an ambiguous start must not attach to another calibration strategy');

const proposal = {
	schema_version: 4,
	profile: 'best_overall',
	target_grade: 'A',
	quality_target_required: true,
	throughput_priority: false,
	download: {
		minimum_kbps: 16700,
		exploration_minimum_kbps: 16700,
		runtime_minimum_kbps: null,
		base_kbps: 35500,
		maximum_kbps: 35500,
		tested_safe_maximum_kbps: 35500,
		exploration_cap_kbps: 204200,
		absolute_cap_kbps: 204200,
		service_hard_cap_kbps: null,
		ceiling_evidence: 'shaped_validation',
		cap_source: 'measured_raw',
		observed_low_kbps: 41700,
		observed_median_kbps: 95000,
		observed_high_kbps: 170000,
	},
	upload: {
		minimum_kbps: 6500,
		exploration_minimum_kbps: 6500,
		runtime_minimum_kbps: null,
		base_kbps: 14300,
		maximum_kbps: 14300,
		tested_safe_maximum_kbps: 14300,
		exploration_cap_kbps: 19000,
		absolute_cap_kbps: 19000,
		service_hard_cap_kbps: null,
		ceiling_evidence: 'shaped_validation',
		cap_source: 'measured_raw',
		observed_low_kbps: 16200,
		observed_median_kbps: 16800,
		observed_high_kbps: 18000,
	},
	active_threshold_kbps: 1600,
	thresholds_ms: { adjust_up: 6, delay: 15, adjust_down: 40 },
	adaptive_ceiling: {
		enabled: true,
		policy: 'passive_bounded',
		hold_s: 15,
		growth_percent: 3,
		probe_s: 8,
		cooldown_s: 45,
		failed_bound_ttl_s: 900,
	},
	access: {
		medium: 'unknown',
		source: 'legacy_default',
		confidence_percent: 0,
	},
	validation: {
		candidate_realization_min_percent: 80,
		candidate_realization_max_percent: 110,
		capacity_retention_min_percent: 80,
		icmp_delta_max_ms: 30,
		transport_delta_max_ms: 30,
		loss_max_percent: 3,
		cpu_max_percent: 85,
	},
	sqm: {
		qdisc: 'cake',
		script: 'layer_cake.qos',
		classification: 'diffserv4',
		squash_dscp: true,
		squash_ingress: true,
		ingress_ecn: 'ECN',
		egress_ecn: 'NOECN',
		iqdisc_opts: 'besteffort',
		eqdisc_opts: 'diffserv4',
	},
	link: { kind: 'cellular', layer: 'none', overhead: 0, mpu: 0 },
};

helpers.writeWizardConfig('auto_wwan', {
	is_new_instance: true,
	name: 'auto_wwan',
	wan_if: 'eth1',
	enabled: true,
	sqm_section: 'cake_auto_wwan',
	speedtest_backend: 'speedtest-go',
	speedtest_go_server_id: '17372',
	speedtest_apply_percent: '90',
	pinger_method: 'fping',
	no_pingers: '3',
	ping_extra_args: '-I eth1',
	reflectors: [ '1.1.1.1', '9.9.9.9', '8.8.8.8' ],
	sqm_download: String(proposal.download.base_kbps),
	sqm_upload: String(proposal.upload.base_kbps),
	sqm_linklayer: proposal.link.layer,
	sqm_overhead: String(proposal.link.overhead),
	sqm_tcMPU: String(proposal.link.mpu),
	sqm_linklayer_advanced: '0',
	autotune_proposal: proposal,
});

assert.equal(written.manual_rate_limits, '1');
assert.equal(written.sqm_download, '35500');
assert.equal(written.sqm_upload, '14300');
assert.equal(written.min_dl_shaper_rate_kbps, '16700');
assert.equal(written.base_dl_shaper_rate_kbps, '35500');
assert.equal(written.max_dl_shaper_rate_kbps, '35500');
assert.equal(written.min_ul_shaper_rate_kbps, '6500');
assert.equal(written.base_ul_shaper_rate_kbps, '14300');
assert.equal(written.max_ul_shaper_rate_kbps, '14300');
assert.equal(written.connection_active_thr_kbps, '1600');
assert.equal(written.dl_avg_owd_delta_max_adjust_up_thr_ms, '6');
assert.equal(written.ul_owd_delta_delay_thr_ms, '15');
assert.equal(written.dl_avg_owd_delta_max_adjust_down_thr_ms, '40');
assert.equal(written.adaptive_ceiling_enabled, '1');
assert.equal(written.capacity_learning_policy, 'passive_bounded');
assert.equal(written.runtime_learning_mode, 'passive');
assert.equal(written.scheduled_autotune_enabled, '0');
assert.equal(written.access_medium, 'unknown');
assert.equal(written.access_medium_source, 'legacy_default');
assert.equal(written.access_medium_confidence_percent, '0');
assert.equal(written.adaptive_ceiling_dl_cap_kbps, '204200');
assert.equal(written.adaptive_ceiling_ul_cap_kbps, '19000');
assert.equal(written.adaptive_ceiling_dl_safe_kbps, '35500');
assert.equal(written.adaptive_ceiling_ul_safe_kbps, '14300');
assert.equal(written.adaptive_ceiling_dl_evidence, 'shaped_validation');
assert.equal(written.adaptive_ceiling_ul_evidence, 'shaped_validation');
assert.equal(written.adaptive_ceiling_dl_cap_source, 'measured_raw');
assert.equal(written.adaptive_ceiling_ul_cap_source, 'measured_raw');
assert.equal(written.adaptive_ceiling_cooldown_s, '45');
assert.equal(written.transport_latency_enabled, '1');
assert.equal(written.throughput_guard_enabled, '1');
assert.equal(written.autotune_profile, 'best_overall');
assert.equal(written.traffic_profile, 'auto');
assert.equal(written.traffic_profile_migrated, '1');
assert.equal(written.traffic_rules_enabled, '0');
assert.equal(written.throughput_guard_retention_percent, '80');
assert.equal(written.quality_target_delay_ms, '30');
assert.equal(written.throughput_reference_dl_p20_kbps, '41700');
assert.equal(written.throughput_reference_dl_p50_kbps, '95000');
assert.equal(written.throughput_reference_ul_p20_kbps, '16200');
assert.equal(written.throughput_reference_ul_p50_kbps, '16800');
assert.equal(written.sqm_linklayer, 'none');
assert.equal(written.sqm_overhead, '0');
assert.equal(written.sqm_tcMPU, '0');
assert.equal(written.speedtest_go_server_id, '17372');
assert.equal(written.route_mode, 'main');
assert.equal(written.sqm_qdisc, 'cake');
assert.equal(written.sqm_script, 'layer_cake.qos');
assert.equal(written.sqm_qdisc_advanced, '1');
assert.equal(written.sqm_qdisc_really_really_advanced, '1');
assert.equal(written.sqm_squash_dscp, '1');
assert.equal(written.sqm_squash_ingress, '1');
assert.equal(written.sqm_iqdisc_opts, 'besteffort');
assert.equal(written.sqm_eqdisc_opts, 'diffserv4');

const failedResult = {
	state: 'failed',
	error: 'Candidate failed shaped validation',
	configuration_written: false,
	proposal,
	validation_attempts: [ {
		pass: false,
		score: 0,
		candidate_base: { download_kbps: 738500, upload_kbps: 755500 },
		throughput: {
			download_kbps: 683153,
			upload_kbps: 698955,
			download_retention_percent: 77.3,
			upload_retention_percent: 77.3,
		},
		latency: {
			median_ms: 9.45,
			p95_ms: 10.1,
			max_ms: 11.9,
			delta_p95_ms: 0.5,
			samples: 215,
			loss_percent: 7.59,
		},
		http_latency: {
			url: 'https://speed.cloudflare.com/__down?bytes=0',
			median_ms: 250,
			p95_ms: 480,
			max_ms: 500,
			delta_p95_ms: 260,
			samples: 20,
		},
		cpu_peak_percent: 53.2,
		background: { clean: false, contaminated: true, download_kbps: 1200, upload_kbps: 20 },
	} ],
};
const failedDiagnostics = helpers.autotuneDiagnostics(failedResult);
assert.equal(failedDiagnostics.validated, false);
assert.equal(failedDiagnostics.configuration_written, false);
assert.equal(failedDiagnostics.attempts.length, 1);
assert.equal(failedDiagnostics.attempts[0].candidate.download_kbps, 738500);
assert.equal(failedDiagnostics.attempts[0].achieved.upload_kbps, 698955);
assert.equal(failedDiagnostics.attempts[0].candidate_realization.download_percent, 92.5);
assert.equal(failedDiagnostics.attempts[0].candidate_realization.upload_percent, 92.5);
assert.equal(failedDiagnostics.attempts[0].capacity_retention.download_percent, 77.3);
const failedGates = Object.fromEntries(failedDiagnostics.attempts[0].gates.map(gate => [ gate.id, gate.pass ]));
assert.equal(failedGates.download_candidate_realization, true);
assert.equal(failedGates.upload_candidate_realization, true);
assert.equal(failedGates.download_capacity_retention, false);
assert.equal(failedGates.upload_capacity_retention, false);
assert.equal(failedGates.icmp_latency, true);
assert.equal(failedGates.icmp_loss, false);
assert.equal(failedGates.transport_latency, false);
assert.equal(failedGates.cpu, true);
assert.equal(failedGates.background, false);

const legacyWrapped = {
	state: 'legacy',
	schema_version: 3,
	producer: 'cake-autorate-rs-autotune',
	legacy_schema_version: 2,
	error: 'Saved Full Auto-Tune diagnostics were created by an older result schema.',
	runtime_restored: true,
	recovery_pending: false,
	legacy_result: Object.assign({ schema_version: 2 }, failedResult),
};
assert.equal(helpers.autotuneLegacyResult(legacyWrapped).schema_version, 2);
const legacyDiagnostics = helpers.autotuneDiagnostics(legacyWrapped);
assert.equal(legacyDiagnostics.legacy, true);
assert.equal(legacyDiagnostics.legacy_schema_version, 2);
assert.equal(legacyDiagnostics.validated, false);
assert.equal(legacyDiagnostics.attempts.length, 1);
assert.equal(legacyDiagnostics.attempts[0].candidate_realization.download_percent, 92.5);
assert.equal(helpers.autotuneLegacyResult({
	state: 'failed',
	schema_version: 2,
}).schema_version, 2, 'a raw legacy terminal payload must be classified without recovery polling');
const inconclusiveDiagnostics = helpers.autotuneDiagnostics({
	state: 'inconclusive',
	stage: 'baseline',
	reason: 'icmp-insufficient-per-reflector-baseline',
	error: 'Measurement evidence remained invalid.',
});
assert.equal(inconclusiveDiagnostics.stage, 'baseline');
assert.equal(inconclusiveDiagnostics.reason, 'icmp-insufficient-per-reflector-baseline');
const structuredDiagnostics = helpers.autotuneDiagnostics({
	state: 'failed',
	proposal,
	validation_attempts: [ {
		pass: false,
		metrics: {
			download: { candidate_realization_percent: 91, capacity_retention_percent: 79 },
			upload: { candidate_realization_percent: 93, capacity_retention_percent: 82 },
		},
		gates: [
			{ code: 'download-candidate-realization', pass: true, actual: 91, limit: 80 },
			{ code: 'upload-candidate-realization', pass: true, actual: 93, limit: 80 },
			{ code: 'download-candidate-realization-maximum', pass: true, actual: 91, limit: 115 },
			{ code: 'upload-candidate-realization-maximum', pass: true, actual: 93, limit: 115 },
			{ code: 'download-capacity-retention', pass: false, actual: 79, limit: 80 },
			{ code: 'upload-capacity-retention', pass: true, actual: 82, limit: 80 },
			{ code: 'icmp-latency', pass: true, actual: 3, limit: 100 },
			{ code: 'transport-latency', pass: true, actual: 20, limit: 100 },
			{ code: 'packet-loss', pass: true, actual: 0.5, limit: 5 },
			{ code: 'cpu', pass: true, actual: 60, limit: 95 },
		],
	} ],
});
const structuredAttempt = structuredDiagnostics.attempts[0];
const structuredGates = Object.fromEntries(structuredAttempt.gates.map(gate => [ gate.id, gate.pass ]));
assert.equal(structuredAttempt.candidate_realization.download_percent, 91);
assert.equal(structuredAttempt.capacity_retention.upload_percent, 82);
assert.equal(structuredAttempt.icmp.delta_p95_ms, 3);
assert.equal(structuredAttempt.transport.delta_p95_ms, 20);
assert.equal(structuredAttempt.icmp.loss_percent, 0.5);
assert.equal(structuredAttempt.cpu_peak_percent, 60);
assert.equal(structuredGates.download_capacity_retention, false);
assert.equal(structuredGates.upload_capacity_retention, true);
assert.equal(structuredGates.download_candidate_realization_maximum, true);
assert.equal(structuredGates.upload_candidate_realization_maximum, true);
const directionalDiagnostics = helpers.autotuneDiagnostics({
	state: 'failed',
	proposal,
	validation_attempts: [ {
		pass: false,
		metrics: {
			download: {
				candidate_realization_percent: 92.5,
				capacity_retention_percent: 77.3,
				candidate_capacity_percent: 83.6,
			},
			upload: {
				candidate_realization_percent: 95,
				capacity_retention_percent: 82,
				candidate_capacity_percent: 86.3,
			},
		},
		signals: {
			download: { icmp_delta_ms: 2, transport_delta_ms: 40, loss_percent: 0, cpu_percent: 86 },
			upload: { icmp_delta_ms: 3, transport_delta_ms: 20, loss_percent: 0.2, cpu_percent: 50 },
		},
		gates: [
			{ code: 'download-candidate-realization', pass: true, actual: 92.5, limit: 80 },
			{ code: 'upload-candidate-realization', pass: true, actual: 95, limit: 80 },
			{ code: 'download-candidate-realization-maximum', pass: true, actual: 92.5, limit: 115 },
			{ code: 'upload-candidate-realization-maximum', pass: true, actual: 95, limit: 115 },
			{ code: 'download-capacity-retention', pass: false, actual: 77.3, limit: 80 },
			{ code: 'upload-capacity-retention', pass: true, actual: 82, limit: 80 },
			{ code: 'download-icmp-latency', pass: true, actual: 2, limit: 30 },
			{ code: 'download-transport-latency', pass: false, actual: 40, limit: 30 },
			{ code: 'download-packet-loss', pass: true, actual: 0, limit: 5 },
			{ code: 'download-cpu', pass: false, actual: 86, limit: 85 },
			{ code: 'upload-icmp-latency', pass: true, actual: 3, limit: 30 },
			{ code: 'upload-transport-latency', pass: true, actual: 20, limit: 30 },
			{ code: 'upload-packet-loss', pass: true, actual: 0.2, limit: 5 },
			{ code: 'upload-cpu', pass: true, actual: 50, limit: 85 },
		],
		reasons: [ { code: 'download-transport-latency' }, { code: 'download-cpu' } ],
		correction: {
			action: 'infeasible', reason: 'safety-floor-blocks-rate-reduction',
			download: { action: 'infeasible', proposed_kbps: 738500 },
			upload: { action: 'none', proposed_kbps: 755500 },
		},
	} ],
});
const directionalAttempt = directionalDiagnostics.attempts[0];
const directionalGates = Object.fromEntries(directionalAttempt.gates.map(gate => [ gate.id, gate.pass ]));
assert.equal(directionalAttempt.directional_load_reported, true);
assert.equal(directionalAttempt.candidate_capacity.download_percent, 83.6);
assert.equal(directionalAttempt.direction_load.download.transport_delta_ms, 40);
assert.equal(directionalGates.download_transport, false,
	'configured 30 ms typed gate must not fall back to the legacy 100 ms threshold');
assert.equal(directionalGates.download_cpu, false,
	'configured 85% typed gate must not fall back to the legacy 95% threshold');
assert.equal(directionalGates.upload_transport, true);
assert.equal(directionalAttempt.correction.action, 'infeasible');
assert.deepEqual(directionalAttempt.reasons.map(reason => reason.code),
	[ 'download-transport-latency', 'download-cpu' ]);

const sustainedDiagnostics = helpers.autotuneAttemptDiagnostics({
	direction_phases: {
		download: {
			cpu_peak_percent: 100,
			cpu: {
				total_peak_percent: 60.4,
				max_core_peak_percent: 100,
				softirq_peak_percent: 94.1,
				mean_effective_percent: 97.3,
				p95_effective_percent: 100,
				samples: 10,
				over_limit_samples: 9,
				longest_over_limit_samples: 9,
				p95_softirq_percent: 94.1,
			},
		},
	},
}, { proposal }, 1);
assert.equal(sustainedDiagnostics.direction_load.download.cpu_mean_percent, 97.3);
assert.equal(sustainedDiagnostics.direction_load.download.cpu_over_limit_samples, 9);
assert.equal(helpers.autotuneCpuSustainedSummary(sustainedDiagnostics.direction_load.download),
	'97.3% mean · 100% p95 · 9/10 samples above limit · longest 9 s · 94.1% softirq p95');

const datapathDiagnostics = helpers.autotuneDiagnostics({
	state: 'failed',
	route: {
		datapath: {
			available: true,
			ingress_device: 'eth2',
			rx_queues: 4,
			packet_steering_mode: '1',
			rps_masks: [ '8', '8', '8', '8' ],
			single_cpu_rps: true,
		},
	},
});
assert.equal(datapathDiagnostics.datapath.ingress_device, 'eth2');
assert.equal(datapathDiagnostics.datapath.single_cpu_rps, true);
const objectGateDiagnostics = helpers.autotuneAttemptDiagnostics({
	metrics: {
		download: { candidate_realization_percent: 95, capacity_retention_percent: 95 },
		upload: { candidate_realization_percent: 95, capacity_retention_percent: 95 },
	},
	gates: { 'download-capacity-retention': false, 'upload-capacity-retention': true },
}, { proposal }, 1);
assert.equal(Object.fromEntries(objectGateDiagnostics.gates.map(gate => [ gate.id, gate.pass ]))
	.download_capacity_retention, false);
assert.equal(helpers.autotuneResultValidated(failedResult), false);

const passingGateCodes = [
	'download-candidate-realization', 'upload-candidate-realization',
	'download-candidate-realization-maximum', 'upload-candidate-realization-maximum',
	'download-capacity-retention', 'upload-capacity-retention',
	'download-throughput-safety-floor', 'upload-throughput-safety-floor',
	'download-icmp-latency', 'download-transport-latency',
	'download-packet-loss', 'download-cpu',
	'upload-icmp-latency', 'upload-transport-latency',
	'upload-packet-loss', 'upload-cpu',
];
const cleanBackground = () => ({
	available: true,
	contaminated: false,
	download_kbps: 0,
	upload_kbps: 0,
});
const cleanDirectionPhase = direction => ({
	direction,
	throughput_kbps: direction === 'download' ? 850000 : 820000,
	forwarded_background: cleanBackground(),
	icmp_latency: { samples: 20, delta_p95_ms: 2, loss_percent: 0 },
	transport_latency: { samples: 20, delta_p95_ms: 5 },
	cpu_peak_percent: 55,
});
const gateLimit = code => {
	if (code.includes('candidate-realization-maximum'))
		return 110;
	if (code.includes('candidate-realization'))
		return 80;
	if (code.includes('capacity-retention'))
		return 80;
	if (code.includes('throughput-safety-floor'))
		return 50;
	if (code.includes('latency'))
		return 30;
	if (code.includes('packet-loss'))
		return 3;
	return 85;
};
const validValidation = {
	profile: 'best_overall',
	pass: true,
	hard_pass: true,
	safety_pass: true,
	profile_objectives_met: true,
	quality_target_met: true,
	actual_grade: 'A',
	effective_delta_ms: 10,
	contaminated: false,
	gates: passingGateCodes.map(code => ({
		code,
		required: !code.endsWith('-cpu') && !code.includes('capacity-retention') &&
			!code.includes('throughput-safety-floor'),
		pass: true,
		actual: code.includes('candidate-realization') ||
			code.includes('capacity-retention') ||
			code.includes('throughput-safety-floor') ? 100 : 0,
		limit: gateLimit(code),
	})),
	correction: { action: 'none', feasible: true },
	candidate_base: {
		download_kbps: proposal.download.base_kbps,
		upload_kbps: proposal.upload.base_kbps,
	},
	direction_phases: {
		download: cleanDirectionPhase('download'),
		upload: cleanDirectionPhase('upload'),
	},
};
const profileSearchReviewOptions = (candidateKbps, targetMet, grade, deltaMs) => [ {
	role: 'recommended',
	observation_index: 1,
	candidate_kbps: candidateKbps,
	conservative_achieved_kbps: candidateKbps,
	worst_delta_ms: deltaMs,
	grade,
	controlled: true,
	manual_reviewable: true,
	target_met: targetMet,
	capacity_objective_met: true,
	auto_apply_candidate: targetMet,
	direction_candidate_only: true,
	pair_confirmation_required: true,
} ];
const profileSearchFor = (profile, targetGrade, retention, candidate, targetMet = true, action = 'complete') => ({
	download: {
		schema_version: 3,
		profile,
		direction: 'download',
		target_grade: targetGrade,
		capacity_floor_percent: retention,
		action,
		reason: targetMet ? 'maximum-target-grade-confirmed' : 'bounded-attempt-limit',
		selected: {
			candidate_kbps: candidate.download.base_kbps,
			achieved_kbps: candidate.download.base_kbps,
			retention_percent: retention,
			effective_delta_ms: targetMet ? 10 : 220,
			grade: targetMet ? 'A' : 'D',
			safety_pass: true,
			target_met: targetMet,
		},
		review_options: profileSearchReviewOptions(candidate.download.base_kbps,
			targetMet, targetMet ? 'A' : 'D', targetMet ? 10 : 220),
		evaluated: [],
		...(profile === 'gaming_extreme' ? {
			exploration_minimum_kbps: Math.floor(candidate.download.base_kbps / 2),
			runtime_minimum_kbps: candidate.download.base_kbps,
			runtime_minimum_observation_index: 1,
			inconclusive: false,
			evaluated: [ { candidate_kbps: candidate.download.base_kbps } ],
		} : {}),
	},
	upload: {
		schema_version: 3,
		profile,
		direction: 'upload',
		target_grade: targetGrade,
		capacity_floor_percent: retention,
		action,
		reason: targetMet ? 'maximum-target-grade-confirmed' : 'bounded-attempt-limit',
		selected: {
			candidate_kbps: candidate.upload.base_kbps,
			achieved_kbps: candidate.upload.base_kbps,
			retention_percent: retention,
			effective_delta_ms: targetMet ? 10 : 220,
			grade: targetMet ? 'A' : 'D',
			safety_pass: true,
			target_met: targetMet,
		},
		review_options: profileSearchReviewOptions(candidate.upload.base_kbps,
			targetMet, targetMet ? 'A' : 'D', targetMet ? 10 : 220),
		evaluated: [],
		...(profile === 'gaming_extreme' ? {
			exploration_minimum_kbps: Math.floor(candidate.upload.base_kbps / 2),
			runtime_minimum_kbps: candidate.upload.base_kbps,
			runtime_minimum_observation_index: 1,
			inconclusive: false,
			evaluated: [ { candidate_kbps: candidate.upload.base_kbps } ],
		} : {}),
	},
});
const profileOutcomeMode = (profile, targetMet) => {
	if (targetMet)
		return profile === 'gaming' ? 'target-a-plus-met' :
			(profile === 'gaming_extreme' ? 'extreme-a-plus-met' :
				(profile === 'fair' ? 'throughput-optimum-c-or-better' : 'target-a-met'));
	return profile === 'gaming' ? 'best-attainable-quality-fallback' :
		(profile === 'gaming_extreme' ? 'extreme-best-attainable-quality-fallback' :
			(profile === 'fair' ? 'throughput-optimum-quality-fallback' : 'balanced-fallback'));
};
const profileOutcomeFor = (profile, targetGrade, retention, candidate, targetMet = true) => ({
	mode: profileOutcomeMode(profile, targetMet),
	target_grade: targetGrade,
	target_met: targetMet,
	actual_grade: targetMet ? 'A' : 'D',
	capacity_floor_percent: retention,
	capacity_floor_met: true,
	throughput_safety_floor_percent: 50,
	throughput_safety_floor_met: true,
	deep_runtime_minimum: false,
	runtime_minimum_retention: null,
	infeasible_reason: '',
	manual_only: !targetMet,
	selected_pair: {
		download_kbps: candidate.download.base_kbps,
		upload_kbps: candidate.upload.base_kbps,
	},
});
const bidirectionalConfirmationFor = (candidate, options = {}) => {
	const downloadRealization = options.downloadRealization ?? 100;
	const uploadRealization = options.uploadRealization ?? 100;
	const effectiveDelta = options.effectiveDelta ?? 10;
	const loss = options.loss ?? 0;
	const delayLimit = options.delayLimit ?? 30;
	const lossLimit = options.lossLimit ?? 3;
	const minimum = options.minimum ?? 80;
	const maximum = options.maximum ?? 110;
	const latencyPass = effectiveDelta <= delayLimit;
	const lossPass = loss <= lossLimit;
	const safetyPass = lossPass && downloadRealization >= 50 && uploadRealization >= 50 &&
		downloadRealization <= maximum && uploadRealization <= maximum;
	return {
		tested: true,
		safety_pass: safetyPass,
		auto_apply_pass: safetyPass && latencyPass &&
			downloadRealization >= minimum && uploadRealization >= minimum,
		grade: effectiveDelta < 5 ? 'A+' : (effectiveDelta < 30 ? 'A' :
			(effectiveDelta < 60 ? 'B' : (effectiveDelta < 200 ? 'C' :
				(effectiveDelta < 400 ? 'D' : 'F')))),
		target_rates_kbps: {
			download: candidate.download.base_kbps,
			upload: candidate.upload.base_kbps,
		},
		achieved_kbps: {
			download: Math.round(candidate.download.base_kbps * downloadRealization / 100),
			upload: Math.round(candidate.upload.base_kbps * uploadRealization / 100),
		},
		realization_percent: {
			download: downloadRealization,
			upload: uploadRealization,
		},
		effective_delta_ms: effectiveDelta,
		icmp_delta_ms: effectiveDelta,
		transport_delta_ms: effectiveDelta,
		loss_percent: loss,
		cpu_peak_percent: 50,
		cpu_warning: false,
		advisory_reason: safetyPass ? 'none' :
			(latencyPass ? 'packet-loss-limit-exceeded' : 'loaded-latency-target-missed'),
	};
};
const baseBidirectionalConfirmation = bidirectionalConfirmationFor(proposal);
const legacyRunId = 'd'.repeat(32);
function legacyRunningFixture(overrides = {}) {
	return {
		state: 'running', run_id: legacyRunId, progress: 0,
		job_id: 'wan_sqm', requested_target_interface: 'pppoe-wan',
		requested_backend: 'speedtest-go', requested_route_mode: 'main',
		requested_mwan3_member: '', requested_profile: 'best_overall',
		requested_conservative: false, requested_calibration_strategy: 'shaped_only',
		requested_access_medium: 'unknown', requested_access_source: 'legacy_default',
		requested_access_confidence_percent: 0, requested_capacity_learning_policy: '',
		requested_service_dl_cap_kbps: '', requested_service_ul_cap_kbps: '',
		...overrides,
	};
}
const validResult = {
	state: 'complete',
	job_id: 'wan_sqm',
	target_interface: 'pppoe-wan',
	resolved_interface: 'pppoe-wan',
	route_interface: 'pppoe-wan',
	route_mode: 'main',
	mwan3_member: '',
	source_ip: '192.0.2.10',
	route_identity: 'main||pppoe-wan|192.0.2.10||main',
	external_ip: '192.0.2.20',
	schema_version: 8,
	producer: 'cake-autorate-rs-autotune',
	run_id: legacyRunId,
	profile: 'best_overall',
	result_class: 'trusted',
	confidence: {
		overall_percent: 100,
		capacity_download_percent: 100,
		capacity_upload_percent: 100,
		quality_percent: 100,
		reasons: [],
	},
	auto_apply_eligible: true,
	manual_apply_eligible: true,
	phase_evidence_complete: true,
	phase_contamination_seen: false,
	runtime_restored: true,
	recovery_pending: false,
	configuration_written: false,
	config_fingerprint: `sha256:${'a'.repeat(64)}`,
	conservative: false,
	confidence_mode: 'normal',
	validation_thresholds: {
		candidate_realization_min_percent: 80,
		candidate_realization_max_percent: 110,
		capacity_retention_min_percent: 80,
		throughput_safety_floor_percent: 50,
		delay_max_ms: 30,
		manual_latency_review_max_ms: 60,
		loss_max_percent: 3,
		cpu_max_percent: 85,
	},
	proposal,
	profile_outcome: {
		...profileOutcomeFor('best_overall', 'A', 80, proposal),
		bidirectional_confirmation: baseBidirectionalConfirmation,
	},
	profile_search: profileSearchFor('best_overall', 'A', 80, proposal),
	bidirectional_confirmation: baseBidirectionalConfirmation,
	phase_background: [
		{ phase: 'baseline', icmp_valid: true, transport_valid: true, forwarded_background: cleanBackground() },
		{ phase: 'unshaped', sample: 1, forwarded_background: cleanBackground() },
		{ phase: 'unshaped', sample: 2, forwarded_background: cleanBackground() },
		{ phase: 'shaped', direction: 'download', forwarded_background: cleanBackground() },
		{ phase: 'shaped', direction: 'upload', forwarded_background: cleanBackground() },
	],
	validation: validValidation,
};
const validAttestation = {
	state: 'ready',
	schema_version: 1,
	config_fingerprint: validResult.config_fingerprint,
	target_interface: validResult.target_interface,
	resolved_interface: validResult.resolved_interface,
	route_interface: validResult.route_interface,
	route_mode: validResult.route_mode,
	mwan3_member: validResult.mwan3_member,
	source_ip: validResult.source_ip,
	external_ip: validResult.external_ip,
	route_identity: validResult.route_identity,
};
assert.equal(helpers.autotuneResultValidated(validResult), true);
assert.equal(helpers.autotuneResultClass(validResult), 'trusted',
	'a complete schema-8 confidence envelope must keep its trusted classification');
const staleProfileSearchResult = structuredClone(validResult);
staleProfileSearchResult.profile_search.download.schema_version = 2;
assert.equal(helpers.autotuneProfileOutcomeValidated(staleProfileSearchResult), false,
	'a stale direction-search schema must not satisfy the current profile outcome contract');
const missingRecommendedSearchResult = structuredClone(validResult);
delete missingRecommendedSearchResult.profile_search.download.review_options;
assert.equal(helpers.autotuneProfileOutcomeValidated(missingRecommendedSearchResult), false,
	'a terminal direction search without its recommended measured option must fail closed');
const legacyConfidenceResult = {
	...validResult,
	schema_version: 7,
	result_class: undefined,
	confidence: undefined,
	proposal: { ...validResult.proposal, confidence: 90 },
};
assert.deepEqual(helpers.autotuneConfidence(legacyConfidenceResult), {
	overall_percent: 90,
	capacity_download_percent: 90,
	capacity_upload_percent: 90,
	quality_percent: 90,
	reasons: [],
	legacy: true,
}, 'legacy proposal confidence must remain readable');
assert.equal(helpers.autotuneResultClass(legacyConfidenceResult), 'technical_failure',
	'legacy diagnostics without a typed result class must not inherit trust');
assert.equal(helpers.autotuneResultReviewable(legacyConfidenceResult, 'apply_sqm'), false,
	'legacy result schemas must remain read-only');
assert.equal(helpers.autotuneResultReviewable({
	...legacyConfidenceResult,
	state: 'inconclusive',
	manual_apply_eligible: true,
	proposals: typedCandidates,
}, 'apply_sqm', typedCandidates[0].proposal_id), false,
	'a schema-7 inconclusive result must remain diagnostic-only even if it carries proposal-shaped data');

const rawNoSqmCandidate = {
	schema_version: 1,
	proposal_id: 'p-555555555555555555555555',
	rank: 1,
	action: 'disable_sqm',
	topology: 'no_sqm',
	is_primary: true,
	applicable: true,
	hard_safety_pass: true,
	profile_target_met: false,
	profile_objectives_met: false,
	grade: 'B',
	effective_delta_ms: 45,
	confidence_percent: 100,
	unmet_objectives: [
		'profile-target',
		'throughput-benefit-unproven',
		'latency-worse-than-shaped',
	],
	evidence: { control: 'raw_control' },
	configuration: null,
};
const rawNoSqmResult = {
	...validResult,
	auto_apply_eligible: false,
	proposals: [ rawNoSqmCandidate ],
	raw_control: {
		available: true,
		grade: 'B',
		effective_delta_ms: 45,
		throughput: { download_kbps: 1000, upload_kbps: 100 },
		icmp_latency: { loss_percent: 0 },
		measurement_evidence: {
			valid: true,
			reason: 'ok',
			test_direction: 'both',
			shaper_bypassed: true,
			sqm_paused: true,
			sqm_bypass_mode: 'paused-managed',
		},
		forwarded_background: {
			available: true,
			contaminated: false,
			download_kbps: 10,
			upload_kbps: 2,
			download_limit_kbps: 100,
			upload_limit_kbps: 20,
		},
	},
};
assert.equal(helpers.autotuneRawNoSqmEvidenceValidated(
	rawNoSqmResult, rawNoSqmCandidate), true,
	'a raw no-SQM proposal must depend on its own hard safety gates, not a shaped +2% throughput or +10 ms utility comparison');
assert.equal(helpers.autotuneResultReviewable(rawNoSqmResult, 'disable_sqm',
	rawNoSqmCandidate.proposal_id), true,
	'a clean independently safe raw proposal must remain reviewable despite advisory utility trade-offs');
assert.deepEqual(helpers.autotuneCandidateAcknowledgementRequirements(rawNoSqmResult,
	'disable_sqm', rawNoSqmCandidate.proposal_id).map(item => item.code), [
	'proposal-profile-target',
	'proposal-throughput-benefit-unproven',
	'proposal-latency-worse-than-shaped',
], 'raw utility trade-offs must be rendered as explicit proposal acknowledgements');
assert.equal(helpers.autotuneResultReviewable(rawNoSqmResult, 'disable_sqm',
	'p-666666666666666666666666'), false,
	'Review must preserve the exact selected raw proposal ID');
assert.equal(helpers.autotuneResultReviewable({
	...rawNoSqmResult,
	proposals: [],
}, 'disable_sqm', rawNoSqmCandidate.proposal_id), false,
	'an explicitly empty proposal list must never unlock Review');
assert.equal(helpers.autotuneResultReviewable({
	...rawNoSqmResult,
	raw_control: {
		...rawNoSqmResult.raw_control,
		measurement_evidence: {
			...rawNoSqmResult.raw_control.measurement_evidence,
			reason: 'partial',
		},
	},
}, 'disable_sqm', rawNoSqmCandidate.proposal_id), false,
	'raw control with a non-clean completion reason must remain a hard blocker');
assert.equal(helpers.autotuneResultReviewable({
	...rawNoSqmResult,
	raw_control: {
		...rawNoSqmResult.raw_control,
		forwarded_background: {
			...rawNoSqmResult.raw_control.forwarded_background,
			download_kbps: 101,
		},
	},
}, 'disable_sqm', rawNoSqmCandidate.proposal_id), false,
	'raw background traffic above its independently measured limit must remain a hard blocker');
assert.equal(helpers.autotuneResultReviewable({
	...rawNoSqmResult,
	raw_control: {
		...rawNoSqmResult.raw_control,
		effective_delta_ms: 61,
		grade: 'C',
	},
	proposals: [ {
		...rawNoSqmCandidate,
		effective_delta_ms: 61,
		grade: 'C',
	} ],
}, 'disable_sqm', rawNoSqmCandidate.proposal_id), false,
	'raw latency above the manual safety limit must remain a hard blocker');

const backgroundAwareConfidence = {
	overall_percent: 68,
	capacity_download_percent: 81,
	capacity_upload_percent: 72,
	quality_percent: 68,
	reasons: [ {
		code: 'background-variation',
		scope: 'upload',
		message: 'Upload background varied during validation',
	} ],
};
const provisionalResult = JSON.parse(JSON.stringify(validResult));
Object.assign(provisionalResult, {
	result_class: 'provisional',
	confidence: backgroundAwareConfidence,
	conservative: true,
	confidence_mode: 'low',
	auto_apply_eligible: false,
	manual_apply_eligible: true,
	phase_contamination_seen: true,
});
provisionalResult.validation.contaminated = true;
provisionalResult.phase_background[3].forwarded_background.contaminated = true;
provisionalResult.validation.direction_phases.download.forwarded_background.contaminated = true;
assert.equal(helpers.autotuneBackgroundAwareResult(provisionalResult), true);
assert.equal(helpers.autotunePhaseEvidenceUsable(provisionalResult), true,
	'contaminated but structured phase evidence must remain usable');
assert.equal(helpers.autotuneResultValidated(provisionalResult), false,
	'a provisional result must never become Auto-Apply eligible');
assert.equal(helpers.autotuneResultReviewable(provisionalResult, 'apply_sqm'), true,
	'a safe server-authorized provisional result must be manually applicable');
assert.equal(helpers.autotuneResultHasReviewChoice(provisionalResult), true);
assert.equal(helpers.autotuneResultClass(provisionalResult), 'provisional');
assert.deepEqual(helpers.autotuneConfidence(provisionalResult), {
	...backgroundAwareConfidence,
	reasons: backgroundAwareConfidence.reasons,
	legacy: false,
});
const estimatedResult = JSON.parse(JSON.stringify(provisionalResult));
estimatedResult.result_class = 'estimated';
estimatedResult.confidence.overall_percent = 34;
estimatedResult.confidence.quality_percent = 34;
assert.equal(helpers.autotuneResultReviewable(estimatedResult, 'apply_sqm'), true,
	'an estimated result remains manually applicable only when the backend explicitly authorizes it');

const trustedResult = JSON.parse(JSON.stringify(validResult));
trustedResult.result_class = 'trusted';
trustedResult.confidence = {
	overall_percent: 96,
	capacity_download_percent: 97,
	capacity_upload_percent: 96,
	quality_percent: 98,
	reasons: [],
};
assert.equal(helpers.autotuneResultValidated(trustedResult), true,
	'a clean background-aware trusted result must retain Auto-Apply eligibility');
const recoveredProvisional = JSON.parse(JSON.stringify(provisionalResult));
Object.assign(recoveredProvisional, {
	result_class: 'provisional',
	auto_apply_eligible: false,
	confidence: {
		overall_percent: 82,
		capacity_download_percent: 100,
		capacity_upload_percent: 100,
		quality_percent: 82,
		reasons: [ { code: 'background-contamination-resolved', scope: 'all' } ],
	},
});
recoveredProvisional.validation.contaminated = false;
recoveredProvisional.validation.direction_phases.download.forwarded_background.contaminated = false;
assert.equal(helpers.autotuneResultValidated(recoveredProvisional), false,
	'earlier discarded background must keep automatic application fail-closed');
assert.equal(helpers.autotuneResultReviewable(recoveredProvisional, 'apply_sqm'), true,
	'later clean evidence may still leave an explicit safe provisional proposal');

const unsafeProvisional = JSON.parse(JSON.stringify(provisionalResult));
unsafeProvisional.validation.safety_pass = false;
assert.equal(helpers.autotuneResultReviewable(unsafeProvisional, 'apply_sqm'), false,
	'confidence must never bypass the hard safety verdict');
const unauthorizedProvisional = JSON.parse(JSON.stringify(provisionalResult));
unauthorizedProvisional.manual_apply_eligible = false;
assert.equal(helpers.autotuneResultReviewable(unauthorizedProvisional, 'apply_sqm'), false,
	'LuCI must require explicit server-side manual eligibility');
const malformedConfidence = JSON.parse(JSON.stringify(provisionalResult));
malformedConfidence.confidence.quality_percent = 101;
assert.equal(helpers.autotuneResultReviewable(malformedConfidence, 'apply_sqm'), false,
	'out-of-range confidence must fail closed');
const mismatchedConfidenceMinimum = JSON.parse(JSON.stringify(provisionalResult));
mismatchedConfidenceMinimum.confidence.overall_percent = 70;
assert.equal(helpers.autotuneResultReviewable(mismatchedConfidenceMinimum, 'apply_sqm'), false,
	'overall confidence must equal the weakest capacity/quality dimension');
const provisionalAboveBand = JSON.parse(JSON.stringify(provisionalResult));
Object.assign(provisionalAboveBand.confidence, {
	overall_percent: 90,
	capacity_download_percent: 90,
	capacity_upload_percent: 90,
	quality_percent: 90,
});
assert.equal(helpers.autotuneResultReviewable(provisionalAboveBand, 'apply_sqm'), false,
	'a provisional class must not claim trusted-band confidence');
const estimatedAtBoundary = JSON.parse(JSON.stringify(estimatedResult));
estimatedAtBoundary.confidence.overall_percent = 40;
estimatedAtBoundary.confidence.quality_percent = 40;
assert.equal(helpers.autotuneResultReviewable(estimatedAtBoundary, 'apply_sqm'), false,
	'an estimated class must remain below the 40-percent boundary');
const trustedBelowBand = JSON.parse(JSON.stringify(trustedResult));
trustedBelowBand.confidence.overall_percent = 84;
trustedBelowBand.confidence.capacity_download_percent = 84;
assert.equal(helpers.autotuneResultValidated(trustedBelowBand), false,
	'a trusted class must not fall below the 85-percent boundary');
const unknownClass = JSON.parse(JSON.stringify(provisionalResult));
unknownClass.result_class = 'optimistic';
assert.equal(helpers.autotuneResultReviewable(unknownClass, 'apply_sqm'), false,
	'unknown result classes must fail closed instead of falling back to legacy handling');
const deferredWithoutProof = JSON.parse(JSON.stringify(validResult));
deferredWithoutProof.phase_background[0].forwarded_background.reference_deferred = true;
deferredWithoutProof.phase_background[0].total_interface_background = {
	reference_deferred: true,
};
assert.equal(helpers.autotunePhaseEvidenceClean(deferredWithoutProof), false,
	'a provisional baseline must not validate without a measured-capacity proof');
assert.equal(helpers.autotuneResultValidated(deferredWithoutProof), false,
	'a forged complete result must not bypass deferred baseline validation');
const deferredWithProof = JSON.parse(JSON.stringify(deferredWithoutProof));
deferredWithProof.phase_background.push({
	phase: 'baseline-retrospective',
	passed: true,
	measured_capacity: { download_kbps: 900000, upload_kbps: 900000 },
	forwarded_background: {
		...cleanBackground(),
		reference_deferred: false,
		download_reference_kbps: 900000,
		upload_reference_kbps: 900000,
	},
});
assert.equal(helpers.autotunePhaseEvidenceClean(deferredWithProof), true,
	'a clean retrospective proof must complete provisional baseline evidence');
assert.equal(helpers.autotuneResultValidated(deferredWithProof), true,
	'a correctly attested deferred baseline remains reviewable');
assert.equal(helpers.autotuneConservativeAvailable({
	background_blocked: true, retryable: true, conservative_available: true, stage: 'baseline',
}), true, 'background-aware continuation must rerun rather than manufacture the idle baseline');
assert.equal(helpers.autotuneConservativeAvailable({
	background_blocked: true, retryable: true, conservative_available: true,
	stage: 'baseline-retrospective',
}), true, 'retrospective contamination may trigger a fresh background-aware run');
assert.equal(helpers.autotuneConservativeAvailable({
	background_blocked: true, retryable: true, conservative_available: false, stage: 'baseline',
}), false, 'a technical baseline failure must remain fail-closed');
assert.equal(helpers.autotuneConservativeAvailable({
	background_blocked: true, retryable: true, stage: 'throughput',
}), true, 'measured throughput contamination may retain explicit conservative review');
const acceptedWan = {
	decision: 'accepted', uncalibrated: false,
	plan: { name: 'wan_sqm' },
	state: { autotune_profile: 'gaming', autotune_result: validResult },
};
const skippedWan = {
	decision: 'skipped', uncalibrated: true,
	plan: { name: 'wanb_sqm' },
	state: { autotune_profile: 'fair', autotune_result: null },
};
const nativeMultiwanResult = nativePublicFixture();
Object.defineProperty(nativeMultiwanResult, '_native_target_state', {
	value: 'absent_bootstrap', enumerable: false,
});
const nativeAcceptedWan = {
	decision: 'accepted', uncalibrated: false,
	plan: { name: 'wanc_sqm' },
	state: { autotune_profile: 'variable_link', autotune_diagnostics: nativeMultiwanResult },
	diagnostics: nativeMultiwanResult,
	native_apply_receipt: {
		...nativeReceipt,
		target_state: 'absent_bootstrap',
		manifest_schema_version: 7,
	},
};
assert.equal(helpers.multiwanAutotuneItemAccepted(acceptedWan), true);
assert.equal(helpers.multiwanAutotuneItemNativeApplied(nativeAcceptedWan), true);
assert.equal(helpers.multiwanAutotuneItemAccepted(nativeAcceptedWan), true,
	'a strictly validated bootstrap receipt must count as the per-uplink Accept decision');
assert.equal(helpers.multiwanAutotuneItemNativeApplied({
	...nativeAcceptedWan,
	native_apply_receipt: { ...nativeAcceptedWan.native_apply_receipt, manifest_schema_version: 8 },
}), false, 'a shaped bootstrap result must not accept the raw/no-SQM manifest schema');
const nativeRawMultiwanResult = nativeRawFallbackPublicFixture();
Object.defineProperty(nativeRawMultiwanResult, '_native_target_state', {
	value: 'absent_bootstrap', enumerable: false,
});
const nativeRawAcceptedWan = {
	decision: 'accepted', uncalibrated: false,
	plan: { name: 'wand_sqm' },
	state: { autotune_profile: 'variable_link', autotune_diagnostics: nativeRawMultiwanResult },
	diagnostics: nativeRawMultiwanResult,
	native_apply_receipt: {
		...nativeReceipt,
		job_id: nativeRawMultiwanResult.native_job_id,
		worker_run_id: nativeRawMultiwanResult.run_id,
		target_state: 'absent_bootstrap',
		manifest_schema_version: 8,
	},
};
assert.equal(helpers.multiwanAutotuneItemNativeApplied(nativeRawAcceptedWan), true,
	'a validated raw/no-SQM bootstrap receipt must use schema 8 and count as already applied');
assert.equal(helpers.multiwanAutotuneItemNativeApplied({
	...nativeRawAcceptedWan,
	native_apply_receipt: { ...nativeRawAcceptedWan.native_apply_receipt, manifest_schema_version: 7 },
}), false, 'a raw/no-SQM bootstrap result must not accept the shaped manifest schema');
assert.deepEqual(helpers.multiwanAutotunePendingPlans([
	{ name: 'wan_sqm' }, { name: 'wanb_sqm' }, { name: 'wanc_sqm' }, { name: 'wand_sqm' },
], [ acceptedWan, skippedWan, nativeAcceptedWan, nativeRawAcceptedWan ]).map(plan => plan.name),
[ 'wan_sqm', 'wanb_sqm' ],
'already-created shaped and raw/no-SQM native instances must be removed from the final conflict/staging set');
assert.deepEqual(helpers.multiwanAutotunePendingStagedApplyItems([
	acceptedWan, skippedWan, nativeAcceptedWan, nativeRawAcceptedWan,
]), [ acceptedWan ],
'final sequential staging must contain neither skipped nor either kind of already-applied native item');
assert.equal(helpers.multiwanAutotuneItemDecided(skippedWan), true);
assert.equal(helpers.multiwanAutotuneBatchDecided([ acceptedWan, skippedWan ]), true,
	'independent profiles and Accept/Skip decisions must complete the batch');
assert.equal(helpers.multiwanAutotuneBatchDecided([
	acceptedWan,
	{ decision: 'pending', uncalibrated: false, state: { autotune_profile: 'fair' } },
]), false, 'the next uplink cannot be bypassed without an explicit decision');
assert.equal(helpers.multiwanAutotuneItemCanSkip({ recovery_pending: true }, false), false,
	'Skip must remain locked until runtime restoration is proven');
assert.equal(helpers.multiwanAutotuneItemCanSkip({ recovery_pending: false }, true), false,
	'Skip must remain locked while a calibration process is active');
assert.equal(helpers.multiwanAutotuneItemCanSkip({ recovery_pending: false }, false), true,
	'a settled failed result must always remain explicitly skippable');
assert.equal(helpers.multiwanAutotuneItemAccepted({
	decision: 'accepted', state: { autotune_result: failedResult },
}), false, 'a failed proposal must never become acceptable through UI state alone');
const resultForProfile = (profile, targetGrade, retention, delay, loss, sqm) => {
	const measuredDelta = profile === 'gaming' || profile === 'gaming_extreme' ? 4 : 10;
	const reviewDelay = profile === 'gaming' || profile === 'gaming_extreme' ? 30 :
		(profile === 'best_overall' ? 60 : (profile === 'variable_link' ? 200 : 400));
	let candidateProposal = {
		...proposal,
		profile,
		target_grade: targetGrade,
		quality_target_required: profile !== 'fair',
		throughput_priority: profile === 'fair',
		validation: {
			...proposal.validation,
			capacity_retention_min_percent: retention,
			icmp_delta_max_ms: delay,
			transport_delta_max_ms: delay,
			loss_max_percent: loss,
		},
		sqm,
	};
	if (profile === 'variable_link') {
		candidateProposal = {
			...candidateProposal,
			download: {
				...candidateProposal.download,
				exploration_cap_kbps: candidateProposal.download.observed_high_kbps,
				absolute_cap_kbps: candidateProposal.download.observed_high_kbps,
			},
			upload: {
				...candidateProposal.upload,
				exploration_cap_kbps: candidateProposal.upload.observed_high_kbps,
				absolute_cap_kbps: candidateProposal.upload.observed_high_kbps,
			},
		};
	}
	const profileBidirectional = bidirectionalConfirmationFor(candidateProposal, {
		delayLimit: delay, lossLimit: loss, effectiveDelta: Math.min(measuredDelta, delay),
	});
	return {
		...validResult,
		profile,
		validation: {
			...validValidation,
			profile,
			actual_grade: profile === 'gaming' || profile === 'gaming_extreme' ? 'A+' : 'A',
			effective_delta_ms: Math.min(measuredDelta, delay),
			gates: validValidation.gates.map(gate => ({
				...gate,
				required: gate.code.endsWith('-cpu') ||
					gate.code.includes('capacity-retention') ||
					gate.code.includes('throughput-safety-floor') ? false :
					(profile === 'fair' && gate.code.includes('latency') ? false : true),
				limit: gate.code.includes('capacity-retention') ? retention :
					(gate.code.includes('latency') ? delay :
						(gate.code.includes('packet-loss') ? loss : gate.limit)),
			})),
		},
		validation_thresholds: {
			...validResult.validation_thresholds,
			capacity_retention_min_percent: retention,
			delay_max_ms: delay,
			manual_latency_review_max_ms: reviewDelay,
			loss_max_percent: loss,
		},
		proposal: candidateProposal,
		profile_outcome: {
			...profileOutcomeFor(profile, targetGrade, retention, candidateProposal),
			actual_grade: profile === 'gaming' || profile === 'gaming_extreme' ? 'A+' : 'A',
			bidirectional_confirmation: profileBidirectional,
		},
		profile_search: profileSearchFor(profile, targetGrade, retention, candidateProposal),
		bidirectional_confirmation: profileBidirectional,
	};
};
const variableBaseResult = resultForProfile('variable_link', 'B', 70, 60, 3, proposal.sqm);
const variableProposal = {
	...variableBaseResult.proposal,
	download: {
		...variableBaseResult.proposal.download,
		minimum_kbps: variableBaseResult.proposal.download.base_kbps,
	},
	upload: {
		...variableBaseResult.proposal.upload,
		minimum_kbps: variableBaseResult.proposal.upload.base_kbps,
	},
};
const variableDirection = (direction, noCakeEffect) => {
	const rate = variableProposal[direction].base_kbps;
	return {
		schema_version: 3,
		profile: 'variable_link',
		direction,
		target_grade: 'B',
		capacity_floor_percent: 70,
		action: noCakeEffect ? 'fallback' : 'complete',
		reason: noCakeEffect ? 'queue-outside-cake-control' : 'latency-knee-confirmed',
		selected: {
			candidate_kbps: rate,
			achieved_kbps: rate,
			retention_percent: 70,
			effective_delta_ms: 30,
			grade: 'B',
			safety_pass: true,
			target_met: true,
		},
		review_options: [ {
			...profileSearchReviewOptions(rate, true, 'B', 30)[0],
			auto_apply_candidate: !noCakeEffect,
		} ],
		exploration_minimum_kbps: Math.floor(rate * 0.35),
		runtime_minimum_kbps: rate,
		runtime_minimum_observation_index: 1,
		knee_detected: !noCakeEffect,
		no_cake_effect: noCakeEffect,
		noisy: false,
		inconclusive: false,
		evaluated: [ { candidate_kbps: rate } ],
	};
};
const variableBidirectional = bidirectionalConfirmationFor(variableProposal, {
	delayLimit: 60,
});
const variableNoEffectResult = {
	...variableBaseResult,
	proposal: variableProposal,
	auto_apply_eligible: false,
	validation: {
		...variableBaseResult.validation,
		candidate_base: {
			download_kbps: variableProposal.download.base_kbps,
			upload_kbps: variableProposal.upload.base_kbps,
		},
	},
	profile_search: {
		download: variableDirection('download', false),
		upload: variableDirection('upload', true),
	},
	bidirectional_confirmation: variableBidirectional,
	profile_outcome: {
		...variableBaseResult.profile_outcome,
		mode: 'directional-no-cake-effect-review',
		manual_only: true,
		selected_pair: {
			download_kbps: variableProposal.download.base_kbps,
			upload_kbps: variableProposal.upload.base_kbps,
		},
		bidirectional_confirmation: variableBidirectional,
	},
};
assert.equal(helpers.autotuneResultValidated(variableNoEffectResult), false,
	'a directional no-effect result must never be Auto-Apply validated');
assert.equal(helpers.autotuneResultReviewable(variableNoEffectResult, 'apply_sqm'), true,
	'a safe target-meeting tested no-effect hold point must remain manually reviewable');
assert.equal(helpers.autotuneResultReviewable({
	...variableNoEffectResult,
	profile_search: {
		...variableNoEffectResult.profile_search,
		upload: {
			...variableNoEffectResult.profile_search.upload,
			runtime_minimum_kbps: variableProposal.upload.base_kbps - 1,
		},
	},
}, 'apply_sqm'), false, 'an untested no-effect runtime minimum must fail closed');
assert.equal(helpers.autotuneResultReviewable({
	...variableNoEffectResult,
	profile_search: {
		...variableNoEffectResult.profile_search,
		upload: {
			...variableNoEffectResult.profile_search.upload,
			selected: { ...variableNoEffectResult.profile_search.upload.selected, target_met: false },
		},
	},
}, 'apply_sqm'), false, 'a below-target no-effect point must not become a proposal');
assert.equal(helpers.autotuneResultReviewable({
	...variableNoEffectResult,
	bidirectional_confirmation: { tested: true, safety_pass: false },
}, 'apply_sqm'), false, 'an unsafe simultaneous final pair must reject the no-effect proposal');
const variableFloorResult = {
	...variableNoEffectResult,
	profile_search: {
		...variableNoEffectResult.profile_search,
		upload: {
			...variableNoEffectResult.profile_search.upload,
			reason: 'exploration-floor-reached',
			no_cake_effect: false,
		},
	},
	profile_outcome: {
		...variableNoEffectResult.profile_outcome,
		mode: 'variable-link-bounded-evidence-review',
	},
};
assert.equal(helpers.autotuneResultValidated(variableFloorResult), false,
	'an exploration-floor result must never be Auto-Apply validated');
assert.equal(helpers.autotuneResultReviewable(variableFloorResult, 'apply_sqm'), true,
	'an exact tested target-meeting exploration-floor fallback must be manually reviewable');
assert.equal(helpers.autotuneResultReviewable({
	...variableFloorResult,
	profile_search: {
		...variableFloorResult.profile_search,
		upload: {
			...variableFloorResult.profile_search.upload,
			runtime_minimum_kbps: variableProposal.upload.base_kbps - 1,
		},
	},
}, 'apply_sqm'), false, 'an invented exploration-floor minimum must fail closed');
const variableBoundedLowResult = JSON.parse(JSON.stringify(variableFloorResult));
const variableBoundedSearch = variableBoundedLowResult.profile_search.upload;
Object.assign(variableBoundedSearch, {
	reason: 'bounded-low-realization-review',
	knee_detected: false,
	no_cake_effect: false,
	noisy: false,
	inconclusive: false,
});
Object.assign(variableBoundedSearch.selected, {
	safety_pass: false,
	manual_reviewable: true,
	realization_percent: 75,
	retention_percent: 70,
});
variableBoundedLowResult.auto_apply_eligible = false;
variableBoundedLowResult.validation.pass = false;
variableBoundedLowResult.validation.hard_pass = false;
const variableBoundedGate = variableBoundedLowResult.validation.gates.find(gate =>
	gate.code === 'upload-candidate-realization');
variableBoundedGate.pass = false;
variableBoundedGate.actual = 75;
assert.equal(helpers.autotuneResultReviewable(variableBoundedLowResult, 'apply_sqm'), true,
	'a typed exact-tested 50-80 percent Variable Link point may be reviewed manually');
assert.deepEqual(helpers.autotuneAcknowledgableGateFailures(
	variableBoundedLowResult, 'apply_sqm').map(gate => gate.code),
	[ 'upload-candidate-realization' ],
	'the bounded directional realization miss must have its own acknowledgement');
assert.equal(helpers.autotuneGateAcknowledgementsComplete(
	variableBoundedLowResult, 'apply_sqm', {}), false);
assert.equal(helpers.autotuneGateAcknowledgementsComplete(
	variableBoundedLowResult, 'apply_sqm', { 'upload-candidate-realization': true }), true);
const variableSubFloor = JSON.parse(JSON.stringify(variableBoundedLowResult));
Object.assign(variableSubFloor.profile_search.upload.selected, {
	manual_reviewable: false,
	realization_percent: 49,
	retention_percent: 49,
});
assert.equal(helpers.autotuneResultReviewable(variableSubFloor, 'apply_sqm'), false,
	'a Variable Link direction below 50 percent must remain non-overridable');
const variableNoisyResult = {
	...variableFloorResult,
	validation: {
		...variableFloorResult.validation,
		quality_target_met: false,
	},
	profile_search: {
		...variableFloorResult.profile_search,
		upload: {
			...variableFloorResult.profile_search.upload,
			reason: 'noisy-link-safe-review',
			noisy: true,
			selected: {
				...variableFloorResult.profile_search.upload.selected,
				target_met: false,
			},
		},
	},
	profile_outcome: {
		...variableFloorResult.profile_outcome,
		target_met: false,
	},
};
assert.equal(helpers.autotuneResultReviewable(variableNoisyResult, 'apply_sqm'), true,
	'a target-unmet noisy direction may expose its best exact tested safe point for manual review');
const variableNoisyRealizationReview = JSON.parse(JSON.stringify(variableNoisyResult));
const noisyDownloadSearch = variableNoisyRealizationReview.profile_search.download;
Object.assign(noisyDownloadSearch, {
	action: 'fallback',
	reason: 'noisy-link-safe-review',
	knee_detected: false,
	noisy: true,
	inconclusive: false,
});
Object.assign(noisyDownloadSearch.selected, {
	realization_percent: 83,
	retention_percent: 66,
	safety_pass: true,
	target_met: false,
});
const noisyRealizationGate = variableNoisyRealizationReview.validation.gates.find(gate =>
	gate.code === 'download-candidate-realization');
noisyRealizationGate.pass = false;
noisyRealizationGate.actual = 76;
variableNoisyRealizationReview.validation.pass = false;
variableNoisyRealizationReview.validation.hard_pass = false;
variableNoisyRealizationReview.validation.safety_pass = true;
variableNoisyRealizationReview.validation.quality_target_met = false;
const noisyFinalConfirmation = bidirectionalConfirmationFor(variableProposal, {
	downloadRealization: 77,
	uploadRealization: 87,
	effectiveDelta: 67,
	delayLimit: 60,
});
variableNoisyRealizationReview.bidirectional_confirmation = noisyFinalConfirmation;
variableNoisyRealizationReview.profile_outcome.bidirectional_confirmation = noisyFinalConfirmation;
variableNoisyRealizationReview.profile_outcome.target_met = false;
variableNoisyRealizationReview.profile_outcome.actual_grade = 'C';
assert.equal(helpers.autotuneCandidateRealizationReconciled(
	variableNoisyRealizationReview, 'download'), true,
	'a noisy Variable Link point above the 50 percent floor may reconcile a strict realization miss');
assert.equal(helpers.autotuneResultEnvelopeValidated(variableNoisyRealizationReview), true,
	'the noisy realization fixture must retain a valid result envelope');
assert.equal(helpers.autotuneProfileOutcomeValidated(variableNoisyRealizationReview), true,
	'the noisy realization fixture must retain a valid profile outcome');
assert.equal(helpers.autotuneResultEvidenceValidated(variableNoisyRealizationReview), true,
	'the noisy realization fixture must retain safe evidence');
assert.equal(helpers.autotuneResultReviewable(variableNoisyRealizationReview, 'apply_sqm'), true,
	'a safe noisy Variable Link candidate must remain manually reviewable below 80 percent realization');
assert.equal(helpers.autotuneDefaultReviewAction(variableNoisyRealizationReview), 'apply_sqm',
	'a safe shaped Variable Link candidate must remain the default ahead of disabling SQM');
assert.equal(helpers.autotuneGateAcknowledgementsComplete(
	variableNoisyRealizationReview, 'apply_sqm', {}), false,
	'the relaxed realization objective must still require explicit consent');
assert.equal(helpers.autotuneGateAcknowledgementsComplete(
	variableNoisyRealizationReview, 'apply_sqm', {
		'download-candidate-realization': true,
		'bidirectional-latency-target': true,
		'bidirectional-download-realization': true,
	}), true,
	'the safe noisy realization miss may be accepted explicitly');
assert.equal(helpers.autotuneResultReviewable({
	...variableNoisyResult,
	profile_search: {
		...variableNoisyResult.profile_search,
		upload: {
			...variableNoisyResult.profile_search.upload,
			selected: {
				...variableNoisyResult.profile_search.upload.selected,
				retention_percent: 49.9,
			},
		},
	},
}, 'apply_sqm'), false, 'a noisy fallback below the 50 percent trust floor must fail closed');
const acknowledgedAdvisoryResult = JSON.parse(JSON.stringify(variableNoisyResult));
const advisoryGate = acknowledgedAdvisoryResult.validation.gates.find(gate =>
	gate.code === 'download-transport-latency');
advisoryGate.pass = false;
advisoryGate.actual = advisoryGate.limit + 5;
assert.equal(helpers.autotuneResultReviewable(acknowledgedAdvisoryResult, 'apply_sqm'), true,
	'a typed manual proposal may remain reviewable when only a selected quality objective is missed');
assert.deepEqual(helpers.autotuneAcknowledgableGateFailures(
	acknowledgedAdvisoryResult, 'apply_sqm').map(gate => gate.code),
	[ 'download-transport-latency' ]);
assert.equal(helpers.autotuneGateAcknowledgementsComplete(
	acknowledgedAdvisoryResult, 'apply_sqm', {}), false,
	'the proposal action must stay disabled until every listed advisory is accepted');
assert.equal(helpers.autotuneGateAcknowledgementsComplete(
	acknowledgedAdvisoryResult, 'apply_sqm', { 'download-transport-latency': true }), true,
	'explicitly accepting the exact listed advisory must unlock the manual proposal action');
const reconciledDirectionalRealization = JSON.parse(JSON.stringify(variableNoisyResult));
const reconciledDirectionalGate = reconciledDirectionalRealization.validation.gates.find(gate =>
	gate.code === 'download-candidate-realization');
Object.assign(reconciledDirectionalGate, { pass: false, actual: 73.1 });
Object.assign(reconciledDirectionalRealization.validation, {
	pass: false,
	hard_pass: false,
	safety_pass: true,
});
reconciledDirectionalRealization.profile_search.download.selected.realization_percent = 83.9;
assert.equal(helpers.autotuneCandidateRealizationReconciled(
	reconciledDirectionalRealization, 'download'), true,
	'two independent exact-rate realization proofs must reconcile one noisy directional miss');
assert.equal(helpers.autotuneResultReviewable(
	reconciledDirectionalRealization, 'apply_sqm'), true,
	'a reconciled directional realization outlier must remain an explicit manual proposal');
assert.deepEqual(helpers.autotuneAcknowledgableGateFailures(
	reconciledDirectionalRealization, 'apply_sqm').map(gate => gate.code),
	[ 'download-candidate-realization' ],
	'the reconciled outlier must remain visible as an explicit user acknowledgement');
const unreconciledSearchRealization = JSON.parse(JSON.stringify(reconciledDirectionalRealization));
unreconciledSearchRealization.profile_search.download.selected.realization_percent = 79.9;
assert.equal(helpers.autotuneResultReviewable(
	unreconciledSearchRealization, 'apply_sqm'), false,
	'a failed directional phase without an independently passing search point must fail closed');
const unreconciledConfirmationRealization = JSON.parse(JSON.stringify(reconciledDirectionalRealization));
unreconciledConfirmationRealization.bidirectional_confirmation.realization_percent.download = 79.9;
unreconciledConfirmationRealization.bidirectional_confirmation.safety_pass = false;
unreconciledConfirmationRealization.profile_outcome.bidirectional_confirmation =
	JSON.parse(JSON.stringify(unreconciledConfirmationRealization.bidirectional_confirmation));
assert.equal(helpers.autotuneResultReviewable(
	unreconciledConfirmationRealization, 'apply_sqm'), false,
	'a failed directional phase without a passing simultaneous confirmation must fail closed');
const subFloorDirectionalRealization = JSON.parse(JSON.stringify(reconciledDirectionalRealization));
subFloorDirectionalRealization.validation.gates.find(gate =>
	gate.code === 'download-candidate-realization').actual = 49.9;
assert.equal(helpers.autotuneResultReviewable(
	subFloorDirectionalRealization, 'apply_sqm'), false,
	'a sub-50 percent directional realization must not be reconciled away');
const finalLatencyMiss = resultForProfile('best_overall', 'A', 80, 30, 3, proposal.sqm);
const finalLatencyConfirmation = bidirectionalConfirmationFor(finalLatencyMiss.proposal, {
	effectiveDelta: 45, delayLimit: 30,
});
finalLatencyMiss.auto_apply_eligible = false;
finalLatencyMiss.bidirectional_confirmation = finalLatencyConfirmation;
Object.assign(finalLatencyMiss.profile_outcome, {
	mode: 'balanced-fallback', target_met: false, actual_grade: 'B', manual_only: true,
	bidirectional_confirmation: finalLatencyConfirmation,
});
assert.equal(helpers.autotuneResultReviewable(finalLatencyMiss, 'apply_sqm'), true,
	'a finite final simultaneous latency miss may remain an explicit manual proposal');
assert.deepEqual(helpers.autotuneAcknowledgableGateFailures(finalLatencyMiss, 'apply_sqm')
	.map(gate => gate.code), [ 'bidirectional-latency-target' ],
	'the final simultaneous latency miss must be exposed as its own acknowledgement');
assert.equal(helpers.autotuneGateAcknowledgementsComplete(finalLatencyMiss, 'apply_sqm', {}), false);
assert.equal(helpers.autotuneGateAcknowledgementsComplete(finalLatencyMiss, 'apply_sqm', {
	'bidirectional-latency-target': true,
}), true);
const finalFarLatency = JSON.parse(JSON.stringify(finalLatencyMiss));
Object.assign(finalFarLatency.bidirectional_confirmation, {
	effective_delta_ms: 61, icmp_delta_ms: 61, transport_delta_ms: 61,
});
Object.assign(finalFarLatency.profile_outcome.bidirectional_confirmation, {
	effective_delta_ms: 61,
});
assert.equal(helpers.autotuneResultReviewable(finalFarLatency, 'apply_sqm'), false,
	'a Best overall miss worse than the adjacent B class must not be overrideable');
const finalLowRealization = resultForProfile('best_overall', 'A', 80, 30, 3, proposal.sqm);
const finalLowRealizationConfirmation = bidirectionalConfirmationFor(finalLowRealization.proposal, {
	downloadRealization: 75, uploadRealization: 90, delayLimit: 30,
});
finalLowRealization.auto_apply_eligible = false;
finalLowRealization.bidirectional_confirmation = finalLowRealizationConfirmation;
Object.assign(finalLowRealization.profile_outcome, {
	manual_only: true,
	bidirectional_confirmation: finalLowRealizationConfirmation,
});
assert.equal(helpers.autotuneResultReviewable(finalLowRealization, 'apply_sqm'), true,
	'low but bounded final simultaneous realization may be accepted manually');
assert.deepEqual(helpers.autotuneAcknowledgableGateFailures(finalLowRealization, 'apply_sqm')
	.map(gate => gate.code), [ 'bidirectional-download-realization' ]);
const finalSub50Realization = JSON.parse(JSON.stringify(finalLowRealization));
Object.assign(finalSub50Realization.bidirectional_confirmation, {
	achieved_kbps: {
		download: Math.round(finalSub50Realization.proposal.download.base_kbps * 0.49),
		upload: finalSub50Realization.bidirectional_confirmation.achieved_kbps.upload,
	},
	realization_percent: { download: 49, upload: 90 },
	safety_pass: false,
});
Object.assign(finalSub50Realization.profile_outcome.bidirectional_confirmation, {
	safety_pass: false,
});
assert.equal(helpers.autotuneResultReviewable(finalSub50Realization, 'apply_sqm'), false,
	'final realization below 50 percent must remain non-overridable');
const finalLossFailure = resultForProfile('best_overall', 'A', 80, 30, 3, proposal.sqm);
const finalLossConfirmation = bidirectionalConfirmationFor(finalLossFailure.proposal, {
	loss: 4, lossLimit: 3, delayLimit: 30,
});
finalLossFailure.auto_apply_eligible = false;
finalLossFailure.bidirectional_confirmation = finalLossConfirmation;
Object.assign(finalLossFailure.profile_outcome, {
	manual_only: true,
	bidirectional_confirmation: finalLossConfirmation,
});
assert.equal(helpers.autotuneResultReviewable(finalLossFailure, 'apply_sqm'), false,
	'final simultaneous packet loss must remain non-overridable');
const finalBypassFailure = resultForProfile('best_overall', 'A', 80, 30, 3, proposal.sqm);
const finalBypassConfirmation = bidirectionalConfirmationFor(finalBypassFailure.proposal, {
	downloadRealization: 111, delayLimit: 30,
});
finalBypassFailure.auto_apply_eligible = false;
finalBypassFailure.bidirectional_confirmation = finalBypassConfirmation;
Object.assign(finalBypassFailure.profile_outcome, {
	manual_only: true,
	bidirectional_confirmation: finalBypassConfirmation,
});
assert.equal(helpers.autotuneResultReviewable(finalBypassFailure, 'apply_sqm'), false,
	'excessive final realization must remain a non-overridable shaper-bypass failure');
const belowTrustResult = JSON.parse(JSON.stringify(variableNoisyResult));
for (const gate of belowTrustResult.validation.gates)
	if (gate.code === 'download-throughput-safety-floor')
		gate.pass = false;
belowTrustResult.profile_outcome.throughput_safety_floor_met = false;
assert.equal(helpers.autotuneResultReviewable(belowTrustResult, 'apply_sqm'), true,
	'a missed historical trust comparison must remain manually reviewable when current hard safety passes');
assert.ok(helpers.autotuneAcknowledgableGateFailures(belowTrustResult, 'apply_sqm').some(gate =>
	gate.code === 'download-throughput-safety-floor'),
	'the missed historical comparison must require explicit acknowledgement');
const gamingResult = resultForProfile('gaming', 'A+', 70, 5, 1, {
	qdisc: 'cake',
	script: 'layer_cake.qos',
	classification: 'diffserv4',
	squash_dscp: false,
	squash_ingress: false,
	ingress_ecn: 'ECN',
	egress_ecn: 'NOECN',
	iqdisc_opts: 'diffserv4',
	eqdisc_opts: 'diffserv4',
});
const extremeProposal = {
	...gamingResult.proposal,
	profile: 'gaming_extreme',
	download: {
		...gamingResult.proposal.download,
		minimum_kbps: gamingResult.proposal.download.base_kbps,
	},
	upload: {
		...gamingResult.proposal.upload,
		minimum_kbps: gamingResult.proposal.upload.base_kbps,
	},
};
const extremeGamingResult = {
	...gamingResult,
	profile: 'gaming_extreme',
	proposal: extremeProposal,
	validation: { ...gamingResult.validation, profile: 'gaming_extreme' },
	profile_outcome: {
		...profileOutcomeFor('gaming_extreme', 'A+', 70, extremeProposal),
		actual_grade: 'A+',
		bidirectional_confirmation: gamingResult.bidirectional_confirmation,
		runtime_minimum_retention: {
			download_percent: Math.round(extremeProposal.download.minimum_kbps * 1000 /
				extremeProposal.download.observed_low_kbps) / 10,
			upload_percent: Math.round(extremeProposal.upload.minimum_kbps * 1000 /
				extremeProposal.upload.observed_low_kbps) / 10,
		},
	},
	profile_search: profileSearchFor('gaming_extreme', 'A+', 70, extremeProposal),
};
const fairResult = {
	...resultForProfile('fair', 'C', 90, 200, 5, proposal.sqm),
	fair_outcome: {
		mode: 'quality-target-met',
		target_grade: 'C',
		target_delta_ms: 200,
		capacity_floor_percent: 90,
		capacity_floor_met: true,
		throughput_safety_floor_percent: 50,
		throughput_safety_floor_met: true,
		actual_grade: 'A',
		actual_effective_delta_ms: 10,
		recommended_action: 'apply_sqm',
		allowed_actions: [ 'apply_sqm', 'keep_current' ],
		apply_sqm_available: true,
		disable_sqm_available: false,
		comparison_reason: 'quality-target-met',
		no_sqm_control: { available: false },
		throughput_gain_without_sqm: { download_percent: 0, upload_percent: 0 },
	},
};
assert.equal(helpers.autotuneResultValidated(gamingResult), true);
assert.equal(helpers.autotuneResultValidated(extremeGamingResult), true,
	'an explicitly selected Extreme A+ result may auto-apply only while it still retains 70%');
const extremeSacrifice = {
	...extremeGamingResult,
	auto_apply_eligible: false,
	proposal: {
		...extremeGamingResult.proposal,
		download: {
			...extremeGamingResult.proposal.download,
			minimum_kbps: Math.round(extremeGamingResult.proposal.download.observed_low_kbps * 0.6),
		},
		upload: {
			...extremeGamingResult.proposal.upload,
			minimum_kbps: Math.round(extremeGamingResult.proposal.upload.observed_low_kbps * 0.6),
		},
	},
	validation: {
		...extremeGamingResult.validation,
		profile_objectives_met: false,
		gates: extremeGamingResult.validation.gates.map(gate =>
			gate.code.includes('capacity-retention') ?
				{ ...gate, required: false, pass: false, actual: 60, limit: 70 } : { ...gate }),
	},
	profile_outcome: {
		...extremeGamingResult.profile_outcome,
		mode: 'extreme-a-plus-throughput-sacrifice',
		capacity_floor_met: false,
		deep_runtime_minimum: true,
		runtime_minimum_retention: { download_percent: 60, upload_percent: 60 },
		manual_only: true,
	},
	profile_search: {
		...extremeGamingResult.profile_search,
		download: {
			...extremeGamingResult.profile_search.download,
			runtime_minimum_kbps: Math.round(extremeGamingResult.proposal.download.observed_low_kbps * 0.6),
			evaluated: [ { candidate_kbps:
				Math.round(extremeGamingResult.proposal.download.observed_low_kbps * 0.6) } ],
		},
		upload: {
			...extremeGamingResult.profile_search.upload,
			runtime_minimum_kbps: Math.round(extremeGamingResult.proposal.upload.observed_low_kbps * 0.6),
			evaluated: [ { candidate_kbps:
				Math.round(extremeGamingResult.proposal.upload.observed_low_kbps * 0.6) } ],
		},
	},
};
assert.equal(helpers.autotuneResultValidated(extremeSacrifice), false,
	'an Extreme A+ throughput sacrifice must never qualify for Auto-Apply');
assert.equal(helpers.autotuneResultReviewable(extremeSacrifice, 'apply_sqm'), true,
	'a technically controlled Extreme A+ point remains available only for explicit review');
const extremeUntestedMinimum = {
	...extremeSacrifice,
	profile_search: {
		...extremeSacrifice.profile_search,
		download: {
			...extremeSacrifice.profile_search.download,
			evaluated: [ { candidate_kbps:
				extremeSacrifice.profile_search.download.runtime_minimum_kbps + 1 } ],
		},
	},
};
assert.equal(helpers.autotuneResultReviewable(extremeUntestedMinimum, 'apply_sqm'), false,
	'an Extreme A+ minimum absent from typed search evidence must fail closed');
assert.equal(helpers.autotuneProfileOutcomeValidated(fairResult), true,
	'Fair profile outcome fixture must match directional and simultaneous evidence');
assert.equal(helpers.autotuneFairOutcomeValidated(fairResult), true,
	'Fair typed outcome fixture must report the authoritative final grade and delay');
assert.equal(helpers.autotuneResultValidated(fairResult), true);
const gamingCpuWarning = {
	...gamingResult,
	validation: {
		...gamingResult.validation,
		warnings: [ { code: 'download-cpu', required: false, pass: false, actual: 90.1, limit: 85 } ],
		gates: gamingResult.validation.gates.map(gate => gate.code === 'download-cpu' ?
			{ ...gate, required: false, pass: false, actual: 90.1, limit: 85 } : { ...gate }),
	},
};
assert.equal(helpers.autotuneResultValidated(gamingCpuWarning), true,
	'advisory CPU pressure must not block an otherwise valid Gaming proposal');
const gamingCpuDiagnostics = helpers.autotuneDiagnostics(gamingCpuWarning);
assert.equal(gamingCpuDiagnostics.validated, true);
assert.deepEqual(gamingCpuDiagnostics.attempts[0].warnings.map(warning => warning.code),
	[ 'download-cpu' ]);
const fairVariable5gReview = {
	...fairResult,
	auto_apply_eligible: false,
	validation: {
		...fairResult.validation,
		profile_objectives_met: false,
		score: 77.7,
		warnings: [
			{ code: 'download-capacity-retention', required: false, pass: false, actual: 40, limit: 90 },
			{ code: 'upload-capacity-retention', required: false, pass: false, actual: 45, limit: 90 },
			{ code: 'download-throughput-safety-floor', required: false, pass: false, actual: 40, limit: 50 },
			{ code: 'upload-throughput-safety-floor', required: false, pass: false, actual: 45, limit: 50 },
		],
		gates: fairResult.validation.gates.map(gate => {
			if (gate.code === 'download-capacity-retention')
				return { ...gate, required: false, pass: false, actual: 40, limit: 90 };
			if (gate.code === 'upload-capacity-retention')
				return { ...gate, required: false, pass: false, actual: 45, limit: 90 };
			if (gate.code === 'download-throughput-safety-floor')
				return { ...gate, required: false, pass: false, actual: 40, limit: 50 };
			if (gate.code === 'upload-throughput-safety-floor')
				return { ...gate, required: false, pass: false, actual: 45, limit: 50 };
			return { ...gate };
		}),
	},
	profile_outcome: {
		...fairResult.profile_outcome,
		mode: 'latency-safe-throughput-advisory',
		capacity_floor_met: false,
		throughput_safety_floor_met: false,
		manual_only: true,
	},
	fair_outcome: {
		...fairResult.fair_outcome,
		capacity_floor_met: false,
		throughput_safety_floor_met: false,
		comparison_reason: 'throughput-retention-below-profile-objective',
	},
};
assert.equal(helpers.autotuneResultValidated(fairVariable5gReview), false,
	'variable 5G advisory result must not auto-apply');
assert.equal(helpers.autotuneResultReviewable(fairVariable5gReview, 'apply_sqm'), true,
	'a Fair result below the historical trust comparison must remain an explicit manual proposal');
const fairCandidateRealizationShortfall = {
	...fairVariable5gReview,
	result_class: 'provisional',
	confidence: {
		overall_percent: 73,
		capacity_download_percent: 73,
		capacity_upload_percent: 100,
		quality_percent: 100,
		reasons: [ {
			code: 'candidate-realization-download',
			scope: 'download',
			message: 'The selected CAKE rate was not fully exercised.',
		} ],
	},
	validation: {
		...fairVariable5gReview.validation,
		gates: fairVariable5gReview.validation.gates.map(gate => {
			if (gate.code === 'download-candidate-realization')
				return { ...gate, required: true, pass: false, actual: 73, limit: 80 };
			if (gate.code === 'download-capacity-retention')
				return { ...gate, required: false, pass: false, actual: 69, limit: 90 };
			if (gate.code === 'upload-capacity-retention')
				return { ...gate, required: false, pass: true, actual: 94, limit: 90 };
			if (gate.code === 'download-throughput-safety-floor')
				return { ...gate, required: false, pass: true, actual: 69, limit: 50 };
			if (gate.code === 'upload-throughput-safety-floor')
				return { ...gate, required: false, pass: true, actual: 94, limit: 50 };
			return { ...gate };
		}),
	},
	profile_outcome: {
		...fairVariable5gReview.profile_outcome,
		throughput_safety_floor_met: true,
	},
	fair_outcome: {
		...fairVariable5gReview.fair_outcome,
		throughput_safety_floor_met: true,
	},
};
assert.equal(helpers.autotuneResultValidated(fairCandidateRealizationShortfall), false,
	'a provisional Fair realization shortfall must never become Auto-Apply eligible');
assert.equal(helpers.autotuneResultReviewable(fairCandidateRealizationShortfall, 'apply_sqm'), true,
	'a Fair candidate explicitly authorized above the safety floor must remain manually reviewable');
assert.equal(helpers.autotuneDefaultReviewAction(fairCandidateRealizationShortfall), 'apply_sqm',
	'the safe Fair manual proposal must not silently fall back to Keep current');
assert.equal(helpers.autotuneResultReviewable({
	...fairCandidateRealizationShortfall,
	validation: {
		...fairCandidateRealizationShortfall.validation,
		gates: fairCandidateRealizationShortfall.validation.gates.map(gate =>
			gate.code === 'download-packet-loss' ? { ...gate, pass: false } : { ...gate }),
	},
}, 'apply_sqm'), false, 'Fair manual review must not relax packet-loss or other safety gates');
assert.equal(helpers.autotuneResultReviewable({
	...fairCandidateRealizationShortfall,
	profile: 'best_overall',
}, 'apply_sqm'), false, 'candidate-realization exceptions must never leak into other profiles');
const fairFallback = {
	...fairResult,
	auto_apply_eligible: false,
	profile_outcome: {
		...profileOutcomeFor('fair', 'C', 90, fairResult.proposal, false),
		bidirectional_confirmation: fairResult.bidirectional_confirmation,
	},
	profile_search: profileSearchFor('fair', 'C', 90, fairResult.proposal, false, 'complete'),
	validation: {
		...fairResult.validation,
		pass: false,
		hard_pass: true,
		quality_target_met: false,
		actual_grade: 'D',
		effective_delta_ms: 220,
		gates: fairResult.validation.gates.map(gate =>
			gate.code.includes('latency') ?
				{ ...gate, required: false, pass: false, actual: 220, limit: 200 } :
				{ ...gate }),
		correction: { action: 'infeasible', feasible: false },
	},
	fair_outcome: {
		...fairResult.fair_outcome,
		mode: 'throughput-fallback',
		actual_grade: 'D',
		actual_effective_delta_ms: 220,
		recommended_action: 'apply_sqm',
		comparison_reason: 'quality-target-unreachable-above-throughput-floor',
	},
};
assert.equal(helpers.autotuneResultValidated(fairFallback), false,
	'Fair throughput fallback must never masquerade as an automatic pass');
assert.equal(helpers.autotuneResultReviewable(fairFallback, 'apply_sqm'), true);
assert.equal(helpers.autotuneResultReviewable(fairFallback, 'keep_current'), true);
assert.equal(helpers.autotuneResultReviewable(fairFallback, 'disable_sqm'), false);
const fairSimultaneousDegradationConfirmation = bidirectionalConfirmationFor(fairResult.proposal, {
	effectiveDelta: 224.2,
	delayLimit: 200,
	lossLimit: 5,
	downloadRealization: 91.7,
	uploadRealization: 96.3,
});
const fairSimultaneousDegradation = {
	...fairResult,
	auto_apply_eligible: false,
	validation: {
		...fairResult.validation,
		profile_objectives_met: false,
		quality_target_met: true,
		actual_grade: 'C',
		effective_delta_ms: 191.5,
		gates: fairResult.validation.gates.map(gate => {
			if (gate.code === 'download-capacity-retention')
				return { ...gate, required: false, pass: false, actual: 89.5, limit: 90 };
			if (gate.code === 'upload-capacity-retention')
				return { ...gate, required: false, pass: true, actual: 94.8, limit: 90 };
			if (gate.code === 'download-throughput-safety-floor')
				return { ...gate, required: false, pass: true, actual: 89.5, limit: 50 };
			if (gate.code === 'upload-throughput-safety-floor')
				return { ...gate, required: false, pass: true, actual: 94.8, limit: 50 };
			if (gate.code.includes('latency'))
				return { ...gate, required: false, pass: true, actual: 191.5, limit: 200 };
			return { ...gate };
		}),
	},
	bidirectional_confirmation: fairSimultaneousDegradationConfirmation,
	profile_outcome: {
		...fairResult.profile_outcome,
		mode: 'quality-and-throughput-advisory-review',
		target_met: false,
		actual_grade: 'D',
		capacity_floor_met: false,
		manual_only: true,
		bidirectional_confirmation: fairSimultaneousDegradationConfirmation,
	},
	fair_outcome: {
		...fairResult.fair_outcome,
		mode: 'throughput-fallback',
		capacity_floor_met: false,
		actual_grade: 'D',
		actual_effective_delta_ms: 224.2,
		comparison_reason: 'quality-target-unreachable-above-throughput-floor',
	},
};
assert.equal(helpers.autotuneResultReviewable(fairSimultaneousDegradation, 'apply_sqm'), true,
	'a Fair result degraded only by simultaneous load must remain an explicit bounded review choice');
assert.equal(helpers.autotuneAchievedGrade(fairSimultaneousDegradation), 'D',
	'the displayed class must include simultaneous DL+UL degradation');
const legacyDirectionalOnlyFairOutcome = {
	...fairSimultaneousDegradation,
	fair_outcome: {
		...fairSimultaneousDegradation.fair_outcome,
		actual_grade: 'C',
		actual_effective_delta_ms: 191.5,
	},
};
assert.equal(helpers.autotuneResultHasReviewChoice(legacyDirectionalOnlyFairOutcome), false,
	'an older schema-8 Fair outcome that omits simultaneous degradation must remain diagnostic-only');
const gamingFallback = {
	...gamingResult,
	auto_apply_eligible: false,
	profile_outcome: {
		...profileOutcomeFor('gaming', 'A+', 70, gamingResult.proposal, false),
		actual_grade: 'A',
		bidirectional_confirmation: gamingResult.bidirectional_confirmation,
	},
	profile_search: profileSearchFor('gaming', 'A+', 70, gamingResult.proposal, false, 'fallback'),
	validation: {
		...gamingResult.validation,
		pass: false,
		hard_pass: false,
		safety_pass: true,
		quality_target_met: false,
		actual_grade: 'A',
		effective_delta_ms: 8,
		gates: gamingResult.validation.gates.map(gate => gate.code.includes('latency') ?
			{ ...gate, pass: false, actual: 8, limit: 5 } : { ...gate }),
	},
};
assert.equal(helpers.autotuneResultValidated(gamingFallback), false);
assert.equal(helpers.autotuneResultReviewable(gamingFallback, 'apply_sqm'), true,
	'a safe best-attainable Gaming fallback must be explicitly reviewable');
const fairDisable = {
	...fairFallback,
	fair_outcome: {
		...fairFallback.fair_outcome,
		mode: 'sqm-disable-recommended',
		recommended_action: 'disable_sqm',
		allowed_actions: [ 'apply_sqm', 'keep_current', 'disable_sqm' ],
		disable_sqm_available: true,
		comparison_reason: 'no-material-latency-benefit-with-throughput-cost',
		no_sqm_control: {
			available: true,
			measurement_evidence: {
				valid: true,
				reason: 'ok',
				test_direction: 'both',
					shaper_bypassed: true,
					sqm_paused: true,
					sqm_bypass_mode: 'paused-managed',
			},
			grade: 'D',
			effective_delta_ms: 218,
			icmp_latency: { loss_percent: 0 },
			throughput: { download_kbps: 900000, upload_kbps: 850000 },
			forwarded_background: {
				available: true,
				contaminated: false,
				duration_s: 20,
				download_kbps: 100,
				upload_kbps: 50,
				download_limit_kbps: 18000,
				upload_limit_kbps: 17000,
			},
		},
		throughput_gain_without_sqm: { download_percent: 3, upload_percent: 2.5 },
	},
};
assert.equal(helpers.autotuneDisableSqmEvidenceValidated(fairDisable), true);
const fairAlreadyUnshaped = {
	...fairDisable,
	fair_outcome: {
		...fairDisable.fair_outcome,
		no_sqm_control: {
			...fairDisable.fair_outcome.no_sqm_control,
			measurement_evidence: {
				...fairDisable.fair_outcome.no_sqm_control.measurement_evidence,
				sqm_paused: false,
				sqm_bypass_mode: 'already-unshaped',
			},
		},
	},
};
assert.equal(helpers.autotuneDisableSqmEvidenceValidated(fairAlreadyUnshaped), true,
	'a verified already-unshaped target must not pretend that an SQM queue was paused');
assert.equal(helpers.autotuneResultReviewable(fairDisable, 'disable_sqm'), true);

const fairDisableAfterUnsafeShapedConfirmation = {
	...fairDisable,
	bidirectional_confirmation: {
		...fairDisable.bidirectional_confirmation,
		safety_pass: false,
		auto_apply_pass: false,
		grade: 'F',
		effective_delta_ms: 466,
		transport_delta_ms: 466,
	},
	profile_outcome: {
		...fairDisable.profile_outcome,
		actual_grade: 'F',
		bidirectional_confirmation: {
			...fairDisable.profile_outcome.bidirectional_confirmation,
			safety_pass: false,
			auto_apply_pass: false,
			grade: 'F',
			effective_delta_ms: 466,
			transport_delta_ms: 466,
		},
	},
	fair_outcome: {
		...fairDisable.fair_outcome,
		actual_grade: 'F',
		actual_effective_delta_ms: 466,
	},
};
assert.equal(helpers.autotuneResultReviewable(
	fairDisableAfterUnsafeShapedConfirmation, 'apply_sqm'), false,
	'an unsafe shaped confirmation must never apply its SQM proposal');
assert.equal(helpers.autotuneResultReviewable(
	fairDisableAfterUnsafeShapedConfirmation, 'disable_sqm'), true,
	'a clean independent no-SQM control must remain reviewable when shaped latency is unsafe');
assert.equal(helpers.autotuneResultReviewable({
	...fairDisableAfterUnsafeShapedConfirmation,
	fair_outcome: {
		...fairDisableAfterUnsafeShapedConfirmation.fair_outcome,
		no_sqm_control: {
			...fairDisableAfterUnsafeShapedConfirmation.fair_outcome.no_sqm_control,
			icmp_latency: { loss_percent: 6 },
		},
	},
}, 'disable_sqm'), false, 'no-SQM packet loss above the Fair limit must block disable');
assert.equal(helpers.autotuneDefaultReviewAction(fairDisable), 'apply_sqm',
	'disable SQM must never be preselected while a safe shaped candidate exists');
assert.equal(helpers.autotuneResultReviewable({
	...fairDisable,
	fair_outcome: {
		...fairDisable.fair_outcome,
		throughput_gain_without_sqm: { download_percent: 3, upload_percent: 1.9 },
	},
}, 'disable_sqm'), true,
	'a Fair no-SQM utility shortfall must be advisory once its independent raw control is safe');
const fairNoSqmUtilityTradeoffs = {
	...fairDisable,
	fair_outcome: {
		...fairDisable.fair_outcome,
		no_sqm_control: {
			...fairDisable.fair_outcome.no_sqm_control,
			grade: 'D',
			effective_delta_ms: 250,
			throughput: { download_kbps: 1000, upload_kbps: 100 },
		},
		throughput_gain_without_sqm: { download_percent: -50, upload_percent: -50 },
	},
};
assert.equal(helpers.autotuneResultReviewable(
	fairNoSqmUtilityTradeoffs, 'disable_sqm'), true,
	'Fair raw throughput loss and latency more than 10 ms worse than shaped are ranking trade-offs, not hard eligibility gates');
assert.equal(helpers.autotuneResultReviewable({
	...fairNoSqmUtilityTradeoffs,
	fair_outcome: {
		...fairNoSqmUtilityTradeoffs.fair_outcome,
		no_sqm_control: {
			...fairNoSqmUtilityTradeoffs.fair_outcome.no_sqm_control,
			grade: 'F',
			effective_delta_ms: 401,
		},
	},
}, 'disable_sqm'), false,
	'Fair no-SQM latency beyond its independent manual safety limit must still be blocked');
assert.equal(helpers.autotuneResultReviewable({
	...fairDisable,
	fair_outcome: {
		...fairDisable.fair_outcome,
		no_sqm_control: {
			...fairDisable.fair_outcome.no_sqm_control,
			measurement_evidence: {
				...fairDisable.fair_outcome.no_sqm_control.measurement_evidence,
				sqm_paused: false,
			},
		},
	},
}, 'disable_sqm'), false, 'disable must fail closed without an unshaped control proof');
assert.equal(helpers.autotuneResultReviewable({
	...fairDisable,
	fair_outcome: {
		...fairDisable.fair_outcome,
		no_sqm_control: {
			...fairDisable.fair_outcome.no_sqm_control,
			forwarded_background: {
				...fairDisable.fair_outcome.no_sqm_control.forwarded_background,
				contaminated: true,
			},
		},
	},
}, 'disable_sqm'), false, 'disable must fail closed when background traffic contaminated the control');

const fairComputeCeiling = {
	...fairDisable,
	profile_outcome: {
		...fairDisable.profile_outcome,
		mode: 'safety-floor-infeasible',
		capacity_floor_met: false,
		throughput_safety_floor_met: true,
		infeasible_reason: 'download:repeatable-compute-ceiling-below-capacity-floor;upload:repeatable-compute-ceiling-below-capacity-floor',
		manual_only: true,
	},
	profile_search: Object.fromEntries([ 'download', 'upload' ].map(direction => [ direction, {
		...fairDisable.profile_search[direction],
		action: 'fallback',
		reason: 'repeatable-compute-ceiling-below-capacity-floor',
		selected: {
			...fairDisable.profile_search[direction].selected,
			safety_pass: false,
		},
	} ])),
	validation: {
		...fairDisable.validation,
		hard_pass: false,
		safety_pass: false,
	},
	fair_outcome: {
		...fairDisable.fair_outcome,
		capacity_floor_met: false,
		throughput_safety_floor_met: true,
		allowed_actions: [ 'keep_current', 'disable_sqm' ],
		apply_sqm_available: false,
	},
};
assert.equal(helpers.autotuneResultReviewable(fairComputeCeiling, 'apply_sqm'), false,
	'an unsafe compute-ceiling candidate must never be applyable');
assert.equal(helpers.autotuneResultReviewable(fairComputeCeiling, 'keep_current'), true);
assert.equal(helpers.autotuneResultReviewable(fairComputeCeiling, 'disable_sqm'), true,
	'a clean no-SQM comparison may support explicit disable after a proven compute ceiling');
assert.equal(helpers.autotuneDefaultReviewAction(fairComputeCeiling), 'keep_current',
	'an unsafe compute-ceiling candidate must default to the non-writing action');
const staleFairComputeCeiling = structuredClone(fairComputeCeiling);
staleFairComputeCeiling.profile_search.download.schema_version = 2;
assert.equal(helpers.autotuneResultReviewable(staleFairComputeCeiling, 'disable_sqm'), false,
	'a stale direction-search schema must not authorize the Fair no-SQM alternative');
assert.equal(helpers.autotuneResultReviewable({
	...fairComputeCeiling,
	profile_search: {
		...fairComputeCeiling.profile_search,
		download: {
			...fairComputeCeiling.profile_search.download,
			reason: 'untrusted-unsafe-fallback',
		},
	},
}, 'disable_sqm'), false, 'unsafe fallback reasons must be strictly allowlisted');
assert.equal(helpers.autotuneResultValidated({
	...gamingResult,
	proposal: {
		...gamingResult.proposal,
		sqm: { ...gamingResult.proposal.sqm, classification: 'besteffort' },
	},
}), false, 'Gaming must fail closed if diffserv4 is removed');
assert.equal(helpers.autotuneResultValidated({
	...fairResult,
	validation_thresholds: {
		...fairResult.validation_thresholds,
		capacity_retention_min_percent: 80,
	},
}), false, 'Fair must fail closed if its throughput floor is weakened');
assert.equal(helpers.autotuneResultValidated({
	...validResult,
	phase_background: [
		{ phase: 'baseline', attempt: 1, icmp_valid: false, transport_valid: true, forwarded_background: cleanBackground() },
		...validResult.phase_background,
	],
}), true, 'a clean second baseline remains applicable after a measurement-only retry');
assert.equal(helpers.autotuneResultValidated({
	...validResult,
	phase_background: validResult.phase_background.map(entry => entry.phase === 'baseline' ?
		{ ...entry, icmp_valid: false } : entry),
}), false, 'at least one fully valid clean baseline is required');
assert.equal(helpers.autotuneResultValidated({
	...validResult,
	validation: { ...validValidation, pass: false },
}), false);
assert.equal(helpers.autotuneResultValidated({
	state: 'complete', proposal, validation: { pass: true },
}), false, 'legacy pass without current phase evidence must fail closed');
assert.equal(helpers.autotuneResultValidated({
	...validResult,
	phase_evidence_complete: false,
}), false, 'missing phase telemetry must not be manually applicable');
assert.equal(helpers.autotuneResultValidated({
	...validResult,
	phase_contamination_seen: true,
	conservative: true,
	confidence_mode: 'low',
	auto_apply_eligible: false,
	validation: { ...validValidation, contaminated: true },
}), false, 'conservative contaminated output is diagnostic-only and cannot be applied');
assert.equal(helpers.autotuneResultValidated({
	...validResult,
	phase_contamination_seen: true,
	validation: { ...validValidation, contaminated: true },
}), false, 'normal-confidence contamination must fail closed');
assert.equal(helpers.autotuneResultValidated({
	...validResult,
	runtime_restored: false,
}), false, 'unrestored runtime state must fail closed');
assert.equal(helpers.autotuneResultValidated({
	...validResult,
	runtime_restored: undefined,
}), false, 'missing runtime restoration evidence must fail closed');
assert.equal(helpers.autotuneResultValidated({
	...validResult,
	recovery_pending: true,
}), false, 'pending recovery must fail closed');
assert.equal(helpers.autotuneResultValidated({
	...validResult,
	config_fingerprint: undefined,
}), false, 'missing configuration fingerprint must fail closed');
assert.equal(helpers.autotuneResultValidated({
	...validResult,
	config_fingerprint: 'sha256:not-a-digest',
}), false, 'malformed configuration fingerprint must fail closed');
assert.equal(helpers.autotuneResultValidated({
	...validResult,
	auto_apply_eligible: false,
}), false, 'a result rejected for auto-apply must also be diagnostic-only in LuCI');
assert.equal(helpers.autotuneResultValidated({
	...validResult,
	phase_background: validResult.phase_background.slice(0, 4),
}), false, 'incomplete phase evidence must fail closed even when the summary flag is true');
assert.equal(helpers.autotuneResultValidated({
	...validResult,
	validation: {
		...validValidation,
		correction: { action: 'infeasible', feasible: false },
	},
}), false, 'an infeasible typed decision must never reach Review or Apply');
assert.equal(helpers.autotuneResultValidated({
	...validResult,
	validation: {
		...validValidation,
		gates: validValidation.gates.filter(gate =>
			gate.code !== 'upload-candidate-realization-maximum'),
	},
}), false, 'missing a required candidate-maximum gate must fail closed');
assert.equal(helpers.autotuneResultValidated({
	...validResult,
	validation: {
		...validValidation,
		gates: validValidation.gates.concat({ code: 'unexpected-gate', pass: true }),
	},
}), false, 'the validation gate set must be the exact 14-gate allowlist');
assert.equal(helpers.autotuneResultValidated({
	...validResult,
	validation: {
		...validValidation,
		gates: validValidation.gates.concat(validValidation.gates[0]),
	},
}), false, 'duplicate validation gates must fail closed');

assert.equal(helpers.autotuneRuntimeSettled(validResult), true);
assert.equal(helpers.autotuneRuntimeSettled({
	recovery_pending: false,
	runtime_restored: false,
}), false);
assert.equal(helpers.autotuneRecoveryPending({
	recovery_pending: false,
	runtime_restored: false,
}), true, 'either incomplete runtime flag must keep the UI in recovery polling');
assert.equal(helpers.autotuneRecoveryPending({
	recovery_pending: true,
	runtime_restored: true,
}), true, 'pending recovery must win over a prematurely true restoration flag');

const staleWizardState = {
	autotune_running: true,
	autotune_progress: 87,
	autotune_result: validResult,
	autotune_proposal: proposal,
	autotune_background_block: { retryable: true },
	autotune_diagnostics: null,
};
helpers.recordAutotuneTerminalFailure(staleWizardState, failedResult,
	'Candidate failed shaped validation');
assert.equal(staleWizardState.autotune_running, false);
assert.equal(staleWizardState.autotune_progress, 0);
assert.equal(staleWizardState.autotune_result, null);
assert.equal(staleWizardState.autotune_proposal, null,
	'terminal failure must discard a stale previously validated proposal');
assert.equal(staleWizardState.autotune_background_block, null);
assert.equal(staleWizardState.autotune_diagnostics, failedResult);

const inconclusiveResult = {
	state: 'inconclusive',
	retryable: true,
	error: 'Shaped validation was inconclusive',
	configuration_written: false,
	validation_attempts: [],
};
assert.equal(helpers.autotuneRetryableInconclusive(inconclusiveResult), true);
assert.equal(helpers.autotuneRetryableInconclusive({ ...inconclusiveResult, retryable: false }), false);
const timeoutResult = {
	...inconclusiveResult,
	reason: 'speedtest-timeout',
	search_state: 'measurement_timeout',
	speedtest_supervisor: { reason: 'timeout', exit_code: 124 },
};
assert.equal(helpers.autotuneMeasurementTimeout(timeoutResult), true);
assert.equal(helpers.autotuneMeasurementTimeout({ ...timeoutResult, search_state: 'noisy' }), false);
assert.equal(helpers.autotuneMeasurementTimeout({ ...timeoutResult, state: 'failed' }), false);
assert.equal(helpers.autotuneResultValidated(inconclusiveResult), false,
	'retryable inconclusive output must never unlock Review or Apply');
const inconclusiveState = {
	autotune_running: true,
	autotune_progress: 62,
	autotune_result: validResult,
	autotune_proposal: proposal,
	autotune_background_block: null,
	autotune_diagnostics: null,
	autotune_failure_message: 'stale failure',
};
helpers.recordAutotuneRetryableInconclusive(inconclusiveState, inconclusiveResult);
assert.equal(inconclusiveState.autotune_running, false);
assert.equal(inconclusiveState.autotune_result, null);
assert.equal(inconclusiveState.autotune_proposal, null);
assert.equal(inconclusiveState.autotune_diagnostics, inconclusiveResult);
assert.equal(inconclusiveState.autotune_failure_message, '',
	'inconclusive output must use warning/retry UX rather than terminal-failure state');

const jointUnsafeVariableResult = {
	state: 'inconclusive',
	profile: 'variable_link',
	retryable: true,
	search_state: 'joint_unsafe',
	reason: 'directional-manual-fallback-final-pair-unsafe',
	recommended_profile: 'fair',
	auto_apply_eligible: false,
	manual_apply_eligible: false,
};
assert.equal(helpers.autotuneRecommendedProfile(jointUnsafeVariableResult), 'fair');
assert.equal(helpers.autotuneResultValidated(jointUnsafeVariableResult), false);
assert.equal(helpers.autotuneResultReviewable(jointUnsafeVariableResult, 'apply_sqm'), false);
assert.equal(helpers.autotuneRecommendedProfile({
	...jointUnsafeVariableResult, manual_apply_eligible: true,
}), null, 'a recommendation must disappear if the fail-closed envelope is inconsistent');
assert.equal(helpers.autotuneRecommendedProfile({
	...jointUnsafeVariableResult, profile: 'fair',
}), null, 'Fair must not recursively recommend itself from an unrelated result');

const writtenBeforeRejectedStage = { ...written };
assert.throws(() => helpers.writeWizardConfig('reject_missing_autotune', {
	mode: 'autotune',
	wan_if: 'eth1',
	enabled: true,
}), /Refusing to stage an unvalidated Auto-Tune proposal/,
'an incomplete Auto-Tune wizard must not stage default values');
assert.throws(() => helpers.writeWizardConfig('reject_failed_autotune', {
	mode: 'autotune',
	wan_if: 'eth1',
	enabled: true,
	autotune_result: failedResult,
	autotune_proposal: proposal,
}), /Refusing to stage an unvalidated Auto-Tune proposal/);
assert.deepEqual(written, writtenBeforeRejectedStage,
	'a rejected Auto-Tune proposal must not stage even one UCI option');

assert.doesNotThrow(() => helpers.writeWizardConfig('accept_valid_autotune', {
	mode: 'autotune',
	wan_if: 'eth1',
	enabled: true,
	sqm_download: String(proposal.download.base_kbps),
	sqm_upload: String(proposal.upload.base_kbps),
	autotune_result: validResult,
	autotune_proposal: proposal,
}));

assert.doesNotThrow(() => helpers.writeWizardConfig('legacy_auto_route', {
	mode: 'autotune',
	wan_if: 'eth1',
	route_mode: 'auto',
	mwan3_member: '',
	enabled: true,
	sqm_download: String(proposal.download.base_kbps),
	sqm_upload: String(proposal.upload.base_kbps),
	autotune_result: validResult,
	autotune_proposal: proposal,
}));
assert.equal(written.route_mode, 'main',
	'a legacy auto route must be persisted as the concrete route attested by Auto-Tune');
assert.equal(written.mwan3_member, undefined,
	'a main-route result must discard any stale mwan3 member before guarded apply');

const disabledFallback = {
	mode: 'autotune',
	wan_if: 'eth1',
	route_mode: 'mwan3',
	mwan3_member: 'wanb',
	enabled: true,
	sqm_enabled: true,
	sqm_download: '20000',
	sqm_upload: '20000',
};
assert.doesNotThrow(() => helpers.writeWizardConfig('uncalibrated_wanb', disabledFallback, true),
	'a failed batch member must stage as a disabled uncalibrated instance');
assert.equal(disabledFallback.enabled, false);
assert.equal(disabledFallback.sqm_enabled, false);
assert.equal(written.enabled, '0');
assert.equal(written.sqm_enabled, '0');
assert.equal(written.route_mode, 'mwan3');
assert.equal(written.mwan3_member, 'wanb');
assert.equal(written.manual_rate_limits, '0');
assert.throws(() => helpers.writeWizardConfig('invalid_uncalibrated', {
	...disabledFallback,
	autotune_result: validResult,
	autotune_proposal: proposal,
}, true), /Invalid disabled, uncalibrated fallback state/,
	'a disabled fallback must never carry a stale proposal');

const stableProposal = {
	...proposal,
	adaptive_ceiling: { ...proposal.adaptive_ceiling, enabled: false },
};
const preserveAdaptive = helpers.adaptiveCeilingWritePlan({
	original_adaptive_ceiling: {
		enabled: true,
		dl_cap_kbps: '150000',
		ul_cap_kbps: '16000',
		hold_s: '60',
		growth_percent: '1',
		probe_s: '10',
		cooldown_s: '90',
		failed_bound_ttl_s: '1800',
	},
	adaptive_ceiling_disable_confirmed: false,
}, stableProposal);
assert.equal(preserveAdaptive.enabled, true);
assert.equal(preserveAdaptive.preserved, true);
assert.equal(preserveAdaptive.dl_cap_kbps, 204200);
assert.equal(preserveAdaptive.ul_cap_kbps, 19000,
	'a fresh measured cap must replace provenance-free legacy bounds');
assert.equal(preserveAdaptive.dl_safe_kbps, 35500);
assert.equal(preserveAdaptive.dl_evidence, 'shaped_validation');
assert.equal(preserveAdaptive.hold_s, 60);
const disableAdaptive = helpers.adaptiveCeilingWritePlan({
	original_adaptive_ceiling: { enabled: true, dl_cap_kbps: '150000', ul_cap_kbps: '16000' },
	adaptive_ceiling_disable_confirmed: true,
}, stableProposal);
assert.equal(disableAdaptive.enabled, false);
assert.equal(disableAdaptive.preserved, false);

helpers.writeWizardConfig('rerun_wwan', {
	name: 'rerun_wwan',
	wan_if: 'eth1',
	enabled: true,
	sqm_section: 'cake_rerun_wwan',
	speedtest_backend: 'speedtest-go',
	speedtest_apply_percent: '90',
	pinger_method: 'fping',
	no_pingers: '3',
	reflectors: [ '1.1.1.1', '9.9.9.9', '8.8.8.8' ],
	sqm_download: String(stableProposal.download.base_kbps),
	sqm_upload: String(stableProposal.upload.base_kbps),
	sqm_linklayer: stableProposal.link.layer,
	sqm_overhead: String(stableProposal.link.overhead),
	sqm_tcMPU: String(stableProposal.link.mpu),
	autotune_proposal: stableProposal,
	original_adaptive_ceiling: {
		enabled: true,
		dl_cap_kbps: '150000',
		ul_cap_kbps: '19000',
		hold_s: '60',
		growth_percent: '1',
		probe_s: '10',
		cooldown_s: '90',
		failed_bound_ttl_s: '1800',
	},
	adaptive_ceiling_disable_confirmed: false,
});
assert.equal(written.adaptive_ceiling_enabled, '1',
	'Re-run must preserve an explicitly enabled adaptive ceiling by default');
assert.equal(written.adaptive_ceiling_dl_cap_kbps, '204200');
assert.equal(written.adaptive_ceiling_dl_safe_kbps, '35500');
assert.equal(written.adaptive_ceiling_hold_time_s, '60');

assert.equal(helpers.validateTransportProbeUrl('websocket', 'wss://ping-bufferbloat.libreqos.com/ws'), true);
assert.equal(helpers.validateTransportProbeUrl('tcp', 'tcp://example.test:443'), true);
assert.equal(helpers.validateTransportProbeUrl('http', 'https://www.google.com/generate_204'), true);
assert.equal(helpers.validateTransportProbeUrl('legacy-http', 'http://example.test/probe?bytes=0'), true);
assert.equal(helpers.validateTransportProbeUrl('http', 'http://example.test/probe'), 'Persistent HTTP requires an https:// endpoint without spaces.');
assert.equal(helpers.validateTransportProbeUrl('websocket', 'https://example.test/probe'), 'Persistent WebSocket requires a ws:// or wss:// endpoint without spaces.');
assert.equal(helpers.validateTransportProbeUrl('tcp', 'https://example.test/has space'), 'TCP connect requires a tcp:// endpoint without spaces.');

helpers.setInterfaceContext({
	deviceNames: { 'pppoe-wan': true, eth0: true },
	deviceNetworks: { 'pppoe-wan': [ 'wan', 'wan6' ], eth0: [ 'wanb', 'wanb6' ] },
	devicePhysical: { 'pppoe-wan': 'eth2' },
	networkDevices: { wan: 'pppoe-wan', wan6: 'pppoe-wan', wanb: 'eth0', wanb6: 'eth0' },
	defaultDevice: 'pppoe-wan',
});
fixtureSections.mwan3 = [
	{ '.name': 'wan', enabled: '1', family: 'ipv4' },
	{ '.name': 'wan_6', enabled: '1', family: 'ipv6' },
	{ '.name': 'wanb', enabled: '1', family: 'ipv4' },
];
const mwan3 = helpers.buildMwan3Context();
helpers.setMwan3Context(mwan3);
assert.deepEqual(mwan3.members.map(member => [ member.name, member.device ]), [
	[ 'wan', 'pppoe-wan' ], [ 'wanb', 'eth0' ],
]);
assert.equal(mwan3.byName.wan.label, 'wan — pppoe-wan — eth2');
assert.equal(mwan3.byName.wanb.label, 'wanb — eth0');
assert.equal(helpers.uniqueMwan3Uplinks().length, 2);
const plans = helpers.multiwanInstancePlans({ name: 'primary_sqm', wan_if: 'pppoe-wan' });
assert.deepEqual(plans.map(plan => [ plan.name, plan.member, plan.device, plan.sqmSection ]), [
	[ 'primary_sqm', 'wan', 'pppoe-wan', 'cake_primary_sqm' ],
	[ 'wanb_sqm', 'wanb', 'eth0', 'cake_wanb_sqm' ],
]);
assert.deepEqual(helpers.wizardPlanConflicts(plans, true), []);
fixtureSections['cake-autorate'] = [
	{ '.name': 'old_wanb', enabled: '1', manage_sqm: '1', wan_if: 'eth0' },
];
assert.match(helpers.wizardPlanConflicts(plans, true).join(' '), /old_wanb.*eth0/);
assert.equal(helpers.managedUplinkOwner(mwan3.byName.wanb), 'old_wanb');
assert.equal(helpers.managedTargetOwner('eth0'), 'old_wanb');
assert.equal(helpers.managedTargetOwner('eth0', 'old_wanb'), '',
	'a rerun must retain authority to select its own target');
assert.match(helpers.targetInterfaceChoiceOptions().find(option => option[0] === 'eth0')[1],
	/already managed by instance "old_wanb"/,
	'an occupied target remains visible but must identify its exact owner');
assert.match(helpers.wizardSingleTargetConflicts({
	name: 'new_wanb', wan_if: 'eth0', enabled: true,
}).join(' '), /old_wanb.*eth0/,
	'a new single-WAN wizard must reject an occupied target before calibration');
assert.deepEqual(helpers.wizardSingleTargetConflicts({
	name: 'old_wanb', wan_if: 'eth0', enabled: true,
}, 'old_wanb'), [], 'a rerun must not conflict with itself');
fixtureSections['cake-autorate'][0].manage_sqm = '0';
assert.deepEqual(helpers.wizardSingleTargetConflicts({
	name: 'new_wanb', wan_if: 'eth0', enabled: true,
}), [], 'an instance with manage_sqm=0 owns no CAKE target');
fixtureSections['cake-autorate'][0].manage_sqm = '1';
assert.deepEqual(helpers.availableMwan3Uplinks().map(member => member.name), [ 'wan' ],
	'an uplink already reserved by another instance must not be selectable again');
assert.deepEqual(helpers.availableMwan3Uplinks('old_wanb').map(member => member.name),
	[ 'wan', 'wanb' ], 'editing an instance must retain its own uplink selection');
const duplicatePlans = [ plans[0], { ...plans[1], name: 'primary_sqm' } ];
assert.match(helpers.wizardPlanConflicts(duplicatePlans, false).join(' '), /duplicated/);

const savedCakeSections = fixtureSections['cake-autorate'];
helpers.setInterfaceContext({
	deviceNames: { eth0: true, eth1: true, eth2: true, 'br-lan': true },
	deviceNetworks: { eth1: [ 'wan' ], eth2: [ 'wan2' ], 'br-lan': [ 'lan' ] },
	devicePhysical: {},
	networkDevices: { wan: 'eth1', wan2: 'eth2', lan: 'br-lan' },
	defaultDevice: 'eth1',
});
fixtureSections['cake-autorate'] = [
	{ '.name': 'existing_wan', enabled: '1', manage_sqm: '1', wan_if: 'eth1' },
	{ '.name': 'existing_lan', enabled: '1', manage_sqm: '1', wan_if: 'br-lan' },
];
helpers.setMwan3Context({
	members: [ { name: 'wan2', device: 'eth2', label: 'wan2 — eth2' } ],
	byName: { wan2: { name: 'wan2', device: 'eth2', label: 'wan2 — eth2' } },
});
assert.equal(helpers.defaultWizardTarget(), 'eth2',
	'a new wizard must prefer a free configured uplink over an unconfigured raw device');
assert.deepEqual(helpers.targetInterfaceChoices(), [ 'eth1', 'br-lan', 'eth0', 'eth2' ],
	'occupied targets remain visible so the validation error is explicit rather than hidden');
fixtureSections['cake-autorate'] = savedCakeSections;
helpers.setMwan3Context(mwan3);
helpers.setInterfaceContext({
	deviceNames: { 'pppoe-wan': true, eth0: true },
	deviceNetworks: { 'pppoe-wan': [ 'wan', 'wan6' ], eth0: [ 'wanb', 'wanb6' ] },
	devicePhysical: { 'pppoe-wan': 'eth2' },
	networkDevices: { wan: 'pppoe-wan', wan6: 'pppoe-wan', wanb: 'eth0', wanb6: 'eth0' },
	defaultDevice: 'pppoe-wan',
});

assert.doesNotMatch(source, /state\.multiwan_set = multiwan\.checked;[\s\S]{0,120}state\.mode = 'manual'/,
	'enabling Multi-WAN must not silently replace Full Auto-Tune with Manual');
assert.match(source, /autotune_calibration_strategy: rerun \? 'shaped_only' : 'full_raw'/,
	'a new instance must default to the only bootstrap strategy with raw-capacity authority');
assert.match(source, /speedtest_backend: rerun \? 'auto' : 'speedtest-go'/,
	'a new instance must default to the native backend instead of the shell auto selector');
assert.match(source,
	/function validateStep\(step\)[\s\S]*?step === 0[\s\S]*?wizardSingleTargetConflicts\(state,[\s\S]*?return false;/,
	'an occupied single-WAN target must be rejected while leaving the Interface step, before any calibration RPC');
assert.match(source, /nativeBootstrapCapacityControl\(itemState, rerun/,
	'each sequential Multi-WAN item must request its own explicit service caps');
assert.match(source, /Skip calibration and create disabled/,
	'a new single-WAN native Review must retain the requested uncalibrated fallback');
assert.match(source,
	/state\.native_autotune_skipped = true;[\s\S]*?state\.enabled = false;[\s\S]*?state\.sqm_enabled = false;/,
	'the single-WAN Skip path must be explicit disabled state, not an implicit default proposal');
assert.match(source, /uncalibrated: state\.native_autotune_skipped === true/,
	'final staging must persist the skipped native instance as uncalibrated');
assert.match(source, /Create and calibrate every unused detected uplink sequentially/);
assert.match(source, /runPreferredAutotuneJob\(item\.plan\.name, item\.plan\.device/,
	'batch Auto-Tune must launch one route-bound job per uplink instead of cloning one proposal');
assert.match(source, /item\.state\.enabled = false;[\s\S]*item\.state\.sqm_enabled = false;/,
	'failed batch members must remain disabled and uncalibrated');

assert.match(source, /Re-run Auto-Tune/);
assert.match(source, /Review diagnostics/);
assert.match(source, /Close diagnostics/);
assert.match(source, /Explicitly allow this proposal to disable the currently enabled adaptive ceiling/);
assert.match(source, /No UCI configuration was written by this Auto-Tune job/);
assert.match(source, /DL candidate \/ raw capacity/);
assert.match(source, /Typed correction/);
assert.match(source, /Failed gate reasons/);
assert.match(source, /DL candidate realization maximum/);
assert.match(source, /UL candidate realization maximum/);
assert.match(source, /Calibration was inconclusive/);
assert.match(source, /Speed test timed out/);
assert.match(source, /Retry diagnostics/);
assert.match(source, /Bounded retries were exhausted/);
assert.match(source, /Suggested next test: Fair/);
assert.match(source, /nothing is selected, disabled, or applied automatically/);
assert.match(source, /alert-message %s.*warning/s,
	'retryable inconclusive diagnostics must render as a warning, not a red failure');
assert.match(source, /autotuneResultReviewable\(state\.autotune_result, selectedAction,\s*state\.autotune_proposal_id\)/);
assert.match(source, /Disable autorate and SQM/);
assert.match(source, /Keep current settings/);
assert.match(source, /I understand that this disables CAKE shaping/);
assert.match(source, /start-conservative/);
assert.match(source, /Continue conservatively/);
assert.match(source, /Deferred baseline traffic check/);
assert.match(source, /autotuneConservativeAvailable\(itemState\.autotune_background_block\)/,
	'Multi-WAN must not offer a conservative override for an untrusted baseline');
assert.match(source, /autotuneConservativeAvailable\(state\.autotune_background_block\)/,
	'single-uplink Auto-Tune must not offer a conservative override for an untrusted baseline');
assert.match(source, /Valid provisional proposal/);
assert.match(source, /Estimated result/);
assert.match(source, /Technical failure/);
assert.match(source, /Accept safe proposal/);
assert.match(source, /Retry for higher confidence/);
assert.match(source, /Capacity confidence/);
assert.match(source, /Quality confidence/);
assert.match(source, /Why confidence was reduced/);
assert.match(source, /s\.tab\('autorate', _\('Autorate setup'\)\)/);
assert.match(source, /s\.tab\('sqm', _\('SQM setup'\)\)/);
assert.match(source, /s\.tab\('testing', _\('Testing & Auto-Tune'\)\)/);
assert.match(source, /decorateAutorateSubcategories/);
for (const label of [ 'Connection & routing', 'Rate limits', 'Adaptive ceiling',
	'Latency probes', 'Quality & rating', 'Controller' ])
	assert(source.includes(label), `missing Autorate subcategory: ${label}`);
assert(source.includes("'class': 'cbi-tabmenu cake-autorate-subnav'"),
	'Autorate subcategories must use native LuCI tab styling');
assert(source.includes('grid-template-columns:repeat(2,minmax(0,1fr))'),
	'Autorate subtabs must remain visibly reachable on narrow mobile dialogs');
assert(source.includes('word-break:normal;overflow-wrap:break-word;hyphens:none'),
	'wizard descriptions must not split ordinary words on narrow screens');
assert(source.includes("tabItems[definition.id].className = active ? 'cbi-tab' : 'cbi-tab-disabled'"),
	'Autorate subcategories must use native LuCI active/inactive tab states');
assert(!source.includes("'btn cbi-button cbi-button-action' : 'btn cbi-button cbi-button-neutral'"),
	'Autorate subcategories must not be rendered as action buttons');
assert.doesNotMatch(source, /s\.tab\('general'/,
	'General must be merged into the Autorate setup category');

async function testAutotuneTerminalPrecedence() {
	const previousWindow = global.window;
	let timerDelays = [];
	global.window = { setTimeout(resolve, delayMs) { timerDelays.push(delayMs); resolve(); } };
	const legacyRpcArgs = call => call.command === '/usr/libexec/cake-autorate-rs/rpcd-helper' ?
		call.args.slice(1) : call.args;

	function pollingHelpers(payloads, calls) {
		return compileHelpers({
			exec(command, args) {
				calls.push({ command, args });
				assert(payloads.length, 'unexpected extra Auto-Tune poll');
				let payload = payloads.shift();
				const legacyArgs = command === '/usr/libexec/cake-autorate-rs/rpcd-helper' ?
					args.slice(1) : args;
				if (payload && typeof payload === 'object' && !Array.isArray(payload) &&
				    [ 'start', 'start-conservative', 'status-summary', 'result', 'cancel' ].includes(legacyArgs[2])) {
					payload = { ...payload, run_id: payload.run_id || legacyRunId };
					if ((legacyArgs[2] === 'start' || legacyArgs[2] === 'start-conservative') && payload.state === 'running') {
						payload = {
							job_id: legacyArgs[0], requested_target_interface: legacyArgs[1],
							requested_backend: legacyArgs[3], requested_route_mode: legacyArgs[4] || '',
							requested_mwan3_member: legacyArgs[5] || '', requested_profile: legacyArgs[6],
							requested_conservative: legacyArgs[2] === 'start-conservative',
							requested_calibration_strategy: legacyArgs[10] || 'shaped_only',
							requested_access_medium: legacyArgs[11] || 'unknown',
							requested_access_source: legacyArgs[12] || 'legacy_default',
							requested_access_confidence_percent: Number(legacyArgs[13] || 0),
							requested_capacity_learning_policy: legacyArgs[14] || '',
							requested_service_dl_cap_kbps: legacyArgs[15] || '',
							requested_service_ul_cap_kbps: legacyArgs[16] || '',
							...payload,
						};
					}
				}
				return Promise.resolve({ stdout: JSON.stringify(payload) });
			},
		});
	}

	try {
		const freshCalls = [];
		const freshHelpers = pollingHelpers([ validResult, validAttestation ], freshCalls);
		assert.deepEqual(await freshHelpers.revalidateAutotuneProposal(
			'wan_sqm', 'pppoe-wan', 'speedtest-go', validResult, 'main', ''), validResult);
		assert.deepEqual(freshCalls, [
			{
				command: '/usr/libexec/cake-autorate-rs/rpcd-helper',
				args: [ 'autotune-status', 'wan_sqm', 'pppoe-wan', 'status', 'speedtest-go',
					'main', '', 'best_overall' ],
			},
			{
				command: '/usr/libexec/cake-autorate-rs/rpcd-helper',
				args: [ 'autotune-attest', 'wan_sqm', 'pppoe-wan', 'attest', 'speedtest-go',
					'main', '', 'best_overall' ],
			},
		], 'proposal staging must re-read the terminal result and recompute live UCI/route identity');

		const changedHelpers = pollingHelpers([ validResult, {
			...validAttestation,
			config_fingerprint: `sha256:${'b'.repeat(64)}`,
		} ], []);
		await assert.rejects(
			changedHelpers.revalidateAutotuneProposal(
				'wan_sqm', 'pppoe-wan', 'speedtest-go', validResult, 'main', ''),
			/Configuration or selected uplink route changed/
		);
		const replacedProposalHelpers = pollingHelpers([ {
			...validResult,
			proposal: {
				...proposal,
				download: { ...proposal.download, base_kbps: proposal.download.base_kbps + 100 },
			},
		} ], []);
		await assert.rejects(
			replacedProposalHelpers.revalidateAutotuneProposal(
				'wan_sqm', 'pppoe-wan', 'speedtest-go', validResult, 'main', ''),
			/Configuration or Auto-Tune state changed/,
			'a newer terminal result must not silently replace the reviewed proposal'
		);
		const changedRouteHelpers = pollingHelpers([ validResult, {
			...validAttestation,
			external_ip: '192.0.2.99',
		} ], []);
		await assert.rejects(
			changedRouteHelpers.revalidateAutotuneProposal(
				'wan_sqm', 'pppoe-wan', 'speedtest-go', validResult, 'main', ''),
			/selected uplink route changed/,
			'a changed live external address must invalidate the reviewed proposal'
		);
		await assert.rejects(
			freshHelpers.revalidateAutotuneProposal(
				'wan_sqm', 'pppoe-wan', 'speedtest-go', {
					...validResult,
					config_fingerprint: undefined,
				}, 'main', ''),
			/no longer valid/
		);
		await assert.rejects(
			freshHelpers.revalidateAutotuneProposal(
				'wan_sqm', 'eth1', 'speedtest-go', validResult, 'main', ''),
			/no longer matches/,
			'a proposal must stay bound to its measured uplink'
		);

		const failedCalls = [];
		const failedProgress = [];
		const failedHelpers = pollingHelpers([
			{ state: 'running', progress: 0 },
			{ state: 'running', phase: 'shaped', progress: 87 },
			{ state: 'failed', error: 'Terminal validation failure', configuration_written: false,
				recovery_pending: false, runtime_restored: true },
		], failedCalls);
		await assert.rejects(
			failedHelpers.runAutotuneJob('wan_sqm', 'pppoe-wan', 'speedtest-go',
				job => failedProgress.push(job.progress)),
			err => {
				assert.equal(err.message, 'Terminal validation failure');
				assert.equal(err.autotuneResult.state, 'failed');
				return true;
			}
		);
		assert.deepEqual(failedProgress, [ 87 ],
			'a stale 87% update may render once, but the next terminal error must stop polling');
		assert.equal(failedCalls.length, 3);

		const mixedCalls = [];
		const mixedProgress = [];
		const mixedHelpers = pollingHelpers([
			{ state: 'running', progress: 0 },
			{ state: 'running', phase: 'shaped', progress: 87,
				error: 'Terminal error overrides stale running fields',
				recovery_pending: false, runtime_restored: true },
		], mixedCalls);
		await assert.rejects(
			mixedHelpers.runAutotuneJob('wan_sqm', 'pppoe-wan', 'speedtest-go',
				job => mixedProgress.push(job.progress)),
			/Terminal error overrides stale running fields/
		);
		assert.deepEqual(mixedProgress, [],
			'terminal error fields must be checked before state=running/progress');
		assert.equal(mixedCalls.length, 2, 'terminal error must not trigger another poll');

		timerDelays = [];
		const recoveryCalls = [];
		const recoveryProgress = [];
		const recoveryHelpers = pollingHelpers([
			{ state: 'running', progress: 0 },
			{ state: 'running', phase: 'shaped', progress: 87,
				error: 'Not terminal until recovery completes',
				recovery_pending: true, runtime_restored: false },
			{ state: 'failed', progress: 87, error: 'Still restoring',
				recovery_pending: false, runtime_restored: false },
			{ state: 'running', progress: 87,
				error: 'Terminal after restoration',
				recovery_pending: false, runtime_restored: true },
		], recoveryCalls);
		await assert.rejects(
			recoveryHelpers.runAutotuneJob('wan_sqm', 'pppoe-wan', 'speedtest-go',
				job => recoveryProgress.push({
					state: job.state,
					phase: job.phase,
					progress: job.progress,
					error: job.error,
				})),
			err => {
				assert.equal(err.message, 'Terminal after restoration');
				assert.equal(err.autotuneResult.runtime_restored, true);
				return true;
			}
		);
		assert.deepEqual(recoveryProgress, [
			{ state: 'recovering', phase: 'recovery', progress: 0, error: undefined },
			{ state: 'recovering', phase: 'recovery', progress: 0, error: undefined },
		], 'pending recovery must clear stale 87% and suppress its provisional error');
		assert.deepEqual(timerDelays, [ 1000, 2000, 4000 ],
			'recovery polling must back off deterministically');
		assert.equal(recoveryCalls.length, 4);

		timerDelays = [];
		const successCalls = [];
		const successProgress = [];
		const successHelpers = pollingHelpers([
			{ state: 'running', progress: 0 },
			{ state: 'running', phase: 'review', progress: 87 },
			validResult,
		], successCalls);
		const completed = await successHelpers.runAutotuneJob(
			'wan_sqm', 'pppoe-wan', 'speedtest-go', job => successProgress.push(job.progress));
		assert.deepEqual(completed, validResult);
		assert.deepEqual(successProgress, [ 87 ]);
		assert.equal(successCalls.length, 3);
		assert.deepEqual(legacyRpcArgs(successCalls[0]).slice(8), [ '', '0', 'shaped_only',
			'unknown', 'legacy_default', '0', '', '', '' ],
			'manual Auto-Tune must pass explicit traffic, strategy, and access-context arguments');

		timerDelays = [];
		const compactCalls = [];
		const compactHelpers = pollingHelpers([
			{ state: 'running', progress: 0 },
			{ state: 'complete', terminal_available: true, terminal_kind: 'result',
				runtime_restored: true, recovery_pending: false },
			validResult,
		], compactCalls);
		assert.deepEqual(await compactHelpers.runAutotuneJob(
			'wan_sqm', 'pppoe-wan', 'speedtest-go', null), validResult);
		assert.deepEqual(compactCalls.map(call => legacyRpcArgs(call)[2]),
			[ 'start', 'status-summary', 'result' ],
			'a compact terminal poll must fetch the large result exactly once');

		timerDelays = [];
		const timeoutCalls = [];
		const matchingRunning = {
			state: 'running', run_id: legacyRunId,
			job_id: 'wan_sqm', requested_target_interface: 'pppoe-wan',
			requested_backend: 'speedtest-go', requested_route_mode: '',
			requested_mwan3_member: '', requested_profile: 'best_overall',
			requested_conservative: false, requested_calibration_strategy: 'shaped_only',
			requested_access_medium: 'unknown', requested_access_source: 'legacy_default',
			requested_access_confidence_percent: 0, requested_capacity_learning_policy: '',
			requested_service_dl_cap_kbps: '', requested_service_ul_cap_kbps: '',
		};
		const timeoutPayloads = [ matchingRunning, {
			state: 'complete', terminal_available: true, terminal_kind: 'result',
			run_id: legacyRunId,
			runtime_restored: true, recovery_pending: false,
		}, validResult ];
		let firstStart = true;
		const timeoutHelpers = compileHelpers({
			exec(command, args) {
				timeoutCalls.push({ command, args });
				if (firstStart) {
					firstStart = false;
					return Promise.reject(new Error('XHR request timed out'));
				}
				assert(timeoutPayloads.length, 'unexpected retry poll');
				return Promise.resolve({ stdout: JSON.stringify(timeoutPayloads.shift()) });
			},
		});
		assert.deepEqual(await timeoutHelpers.runAutotuneJob(
			'wan_sqm', 'pppoe-wan', 'speedtest-go', null), validResult);
		assert.deepEqual(timeoutCalls.map(call => legacyRpcArgs(call)[2]),
			[ 'start', 'status-summary', 'status-summary', 'result' ],
			'an ambiguous start timeout must reattach only through the exact request identity');

		timerDelays = [];
		const cancelCalls = [];
		const cancelledTerminal = {
			state: 'cancelled',
			run_id: legacyRunId,
			error: 'Full Auto-Tune was cancelled; no configuration was written.',
			runtime_restored: true,
			recovery_pending: false,
		};
		const cancelHelpers = pollingHelpers([
			{ state: 'cancelling', run_id: legacyRunId,
				runtime_restored: false, recovery_pending: true },
			cancelledTerminal,
		], cancelCalls);
		cancelHelpers.setLegacyAutotuneJob('wan_sqm', legacyRunId);
		assert.deepEqual(await cancelHelpers.cancelAutotuneJob(
			'wan_sqm', 'pppoe-wan', 'speedtest-go', 'best_overall', 'mwan3', 'wan'),
		cancelledTerminal, 'user cancellation must wait for runtime restoration and return neutrally');
		assert.deepEqual(cancelCalls.map(legacyRpcArgs), [
			[ 'wan_sqm', 'pppoe-wan', 'cancel', 'speedtest-go', '', '', 'best_overall' ],
			[ 'wan_sqm', 'pppoe-wan', 'status-summary', 'speedtest-go', 'mwan3', 'wan', 'best_overall' ],
		]);
		assert.deepEqual(timerDelays, [ 2000 ]);
		await assert.rejects(cancelHelpers.cancelAutotuneJob(
			'wan_sqm', 'pppoe-wan', 'speedtest-go', 'best_overall', 'mwan3', 'wan'),
			/still registering/,
			'a settled cancellation must consume its authenticated legacy handle');

		const mismatchedCancelHelpers = pollingHelpers([
			{ state: 'cancelling', run_id: 'e'.repeat(32),
				runtime_restored: false, recovery_pending: true },
		], []);
		mismatchedCancelHelpers.setLegacyAutotuneJob('wan_sqm', legacyRunId);
		await assert.rejects(mismatchedCancelHelpers.cancelAutotuneJob(
			'wan_sqm', 'pppoe-wan', 'speedtest-go', 'best_overall', 'mwan3', 'wan'),
			/different calibration run/,
			'legacy cancellation must never accept restored evidence from another generation');

		timerDelays = [];
		const delayedResultCalls = [];
		const delayedResultProgress = [];
		const delayedResultHelpers = pollingHelpers([
			{ state: 'running', progress: 0 },
			{ ...validResult, recovery_pending: true, runtime_restored: false },
			validResult,
		], delayedResultCalls);
		const delayedCompleted = await delayedResultHelpers.runAutotuneJob(
			'wan_sqm', 'pppoe-wan', 'speedtest-go',
			job => delayedResultProgress.push(job.progress));
		assert.deepEqual(delayedCompleted, validResult,
			'a complete result must not be accepted before both runtime flags settle');
		assert.deepEqual(delayedResultProgress, [ 0 ]);
		assert.deepEqual(timerDelays, [ 1000, 2000 ]);
		assert.equal(delayedResultCalls.length, 3);

		const contaminatedCalls = [];
		const contaminated = {
			...validResult,
			auto_apply_eligible: false,
			phase_contamination_seen: true,
			conservative: true,
			confidence_mode: 'low',
			validation: { ...validValidation, contaminated: true },
		};
		const contaminatedHelpers = pollingHelpers([
			{ state: 'running', progress: 0 },
			contaminated,
		], contaminatedCalls);
		await assert.rejects(
			contaminatedHelpers.runAutotuneJob('wan_sqm', 'pppoe-wan', 'speedtest-go'),
			err => {
				assert.match(err.message, /without a safe reviewable result/);
				assert.deepEqual(err.autotuneResult, contaminated,
					'diagnostic payload must be retained for the failure UI');
				return true;
			}
		);
		assert.equal(contaminatedCalls.length, 2);

		timerDelays = [];
		const boundedCalls = [];
		const pendingPayload = {
			state: 'failed', progress: 87, error: 'Recovery has not published terminal state',
			recovery_pending: true, runtime_restored: false,
		};
		const boundedHelpers = pollingHelpers([
			{ state: 'running', progress: 0 },
			...Array.from({ length: 12 }, () => ({ ...pendingPayload })),
		], boundedCalls);
		await assert.rejects(
			boundedHelpers.runAutotuneJob('wan_sqm', 'pppoe-wan', 'speedtest-go'),
			err => {
				assert.equal(err.autotuneRecoveryPending, true);
				assert.equal(err.autotuneResult, undefined,
					'a polling timeout is not a terminal Auto-Tune result');
				assert.equal(err.autotuneRecoveryStatus.recovery_pending, true);
				return true;
			}
		);
		assert.equal(boundedCalls.length, 13,
			'recovery polling must stop at its deterministic bound');
		assert.deepEqual(timerDelays,
			[ 1000, 2000, 4000, 5000, 5000, 5000, 5000, 5000, 5000, 5000, 5000, 5000 ]);
	}
	finally {
		if (previousWindow === undefined)
			delete global.window;
		else
			global.window = previousWindow;
	}
}

async function testNativeAutotuneTransport() {
	const previousWindow = global.window;
	const delays = [];
	global.window = { setTimeout(resolve, delayMs) { delays.push(delayMs); resolve(); } };
	const publicJobId = 'b'.repeat(32);
	const capability = {
		state: 'idle',
		protocol_version: 2,
		admission_enabled: true,
		native_full_autotune: true,
		native_bootstrap_autotune: true,
		native_public_result_version: 5,
	};
	const access = {
		medium: 'cellular',
		source: 'user_selected',
		confidence_percent: 100,
		policy: 'scheduled_active',
		service_dl_cap_kbps: '1000000',
		service_ul_cap_kbps: '500000',
	};

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
		const readyProgress = helpers.nativeAutotuneProgress({
			state: 'review_ready', progress_schema_version: 1, progress_percent: 99,
			progress_step: 'proposals_ready', progress_completed_units: 0,
			progress_total_units: 0,
		}, cappedProgress.progress);
		assert.equal(readyProgress.progress, 100);
		assert.match(readyProgress.message, /Proposals are ready/);
		for (const progressResult of [ queuedProgress, downloadProgress, mobileBypassProgress, regressedProgress,
			cappedProgress, readyProgress ])
			assert.doesNotMatch(progressResult.message, /native|rust|worker|coordinator/i);
		const unknownProgress = helpers.nativeAutotuneProgress({
			state: 'running', progress_schema_version: 1, progress_percent: 'NaN',
			progress_step: 'internal_variant_name', progress_attempt: -1,
		}, 0);
		assert.equal(unknownProgress.progress, 3);
		assert.match(unknownProgress.message, /detailed progress is temporarily unavailable/);

		assert.equal(helpers.nativeAutotuneCapabilityValidated(capability), true);
		assert.equal(helpers.nativeBootstrapAutotuneCapabilityValidated(capability), true);
		assert.equal(helpers.nativeAutotuneCapabilityValidated({
			...capability,
			admission_enabled: false,
		}), false, 'native routing must remain dormant until admission is explicitly advertised');

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
		const applyCalls = [];
		const applyPayloads = [ bootstrapConfirmation, bootstrapReceipt ];
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
			[ '--calibrationctl', 'autotune-apply' ],
		]);
		assert.equal(applyCalls[1].args[5], bootstrapManifest,
			'Apply must use the effective v7 manifest returned by the server check');

		const replayCalls = [];
		const replayTimeouts = [];
		const replayL = { env: { rpctimeout: 90 } };
		const replayReceipt = { ...bootstrapReceipt, state: 'already_applied' };
		const replayHelpers = compileHelpers({
			exec(command, args) {
				replayCalls.push({ command, args });
				replayTimeouts.push(replayL.env.rpctimeout);
				if (replayCalls.length === 1)
					return Promise.resolve({ stdout: JSON.stringify(bootstrapConfirmation) });
				if (replayCalls.length === 2)
					return Promise.reject(new Error('truncated Apply transport'));
				return Promise.resolve({ stdout: JSON.stringify(replayReceipt) });
			},
		}, null, replayL);
		assert.deepEqual(await replayHelpers.runNativeAutotuneApply(
			bootstrapApplyResult, nativeSelected), replayReceipt,
			'a lost Apply response must reconcile through one exact idempotent replay');
		assert.equal(replayCalls.length, 3);
		assert.deepEqual(replayCalls[1].args, replayCalls[2].args,
			'the uncertain delivery retry must reuse the exact job, option, digests and ACKs');
		assert.deepEqual(replayTimeouts, [ 180, 30, 180 ],
			'the first mutation response gets an exact short bound and the replay keeps the full recovery bound');
		assert.equal(replayL.env.rpctimeout, 90,
			'every Apply phase must restore the caller RPC timeout');

		const truncatedCalls = [];
		const truncatedHelpers = compileHelpers({
			exec(command, args) {
				truncatedCalls.push({ command, args });
				if (truncatedCalls.length === 1)
					return Promise.resolve({ stdout: JSON.stringify(bootstrapConfirmation) });
				if (truncatedCalls.length === 2)
					return Promise.resolve({ code: 0, stdout: '{"state":"already_' });
				return Promise.resolve({ stdout: JSON.stringify(replayReceipt) });
			},
		});
		assert.deepEqual(await truncatedHelpers.runNativeAutotuneApply(
			bootstrapApplyResult, nativeSelected), replayReceipt,
			'a truncated code-zero Apply receipt must use the same bounded exact replay');
		assert.deepEqual(truncatedCalls[1].args, truncatedCalls[2].args);

		const exhaustedCalls = [];
		const exhaustedHelpers = compileHelpers({
			exec(command, args) {
				exhaustedCalls.push({ command, args });
				if (exhaustedCalls.length === 1)
					return Promise.resolve({ stdout: JSON.stringify(bootstrapConfirmation) });
				return Promise.reject(new Error('response connection interrupted'));
			},
		});
		let exhaustedError;
		try {
			await exhaustedHelpers.runNativeAutotuneApply(
				bootstrapApplyResult, nativeSelected);
		}
		catch (error) {
			exhaustedError = error;
		}
		assert(exhaustedError, 'two uncertain Apply deliveries must remain distinguishable');
		assert.equal(exhaustedError.nativeApplyReloadRequired, true,
			'two uncertain deliveries must request an authoritative fresh-page reconciliation');
		assert.equal(exhaustedCalls.length, 3);
		assert.deepEqual(exhaustedCalls[1].args, exhaustedCalls[2].args,
			'the final uncertain result must still follow an exact single replay');
		assert.match(source,
			/if \(error\.nativeApplyReloadRequired\)\s*return reloadAppliedSettingsPage\(\);/,
			'the Review UI must reload authoritative UCI after both Apply responses are lost');

		const logicalFailureCalls = [];
		const logicalFailureHelpers = compileHelpers({
			exec(command, args) {
				logicalFailureCalls.push({ command, args });
				if (logicalFailureCalls.length === 1)
					return Promise.resolve({ stdout: JSON.stringify(bootstrapConfirmation) });
				return Promise.resolve({ code: 1, stderr: 'unsafe Apply state' });
			},
		});
		await assert.rejects(
			logicalFailureHelpers.runNativeAutotuneApply(bootstrapApplyResult, nativeSelected),
			/unsafe Apply state/);
		assert.equal(logicalFailureCalls.length, 2,
			'a structured server rejection must fail closed without replay');

		const nativeCalls = [];
		const nativePayloads = [
			capability,
			{ state: 'idle', instance: 'wan_sqm' },
			{ state: 'queued', job_id: publicJobId },
			{ state: 'review_ready', job_id: publicJobId,
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
			{ state: 'queued', job_id: freshPublicJobId },
			{ state: 'review_ready', job_id: freshPublicJobId,
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
				runtime_mutated: true, recovery_required: true,
				progress_schema_version: 1, progress_percent: 40,
				progress_step: 'searching_download_limit' },
			{ state: 'review_ready', job_id: publicJobId, instance: 'wan_sqm',
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
			{ state: 'queued', job_id: publicJobId },
			{ state: 'review_ready', job_id: publicJobId,
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

		const mismatched = nativePublicFixture();
		mismatched.target_interface = 'eth9';
		assert.equal(helpers.nativeAutotunePublicResultValidated(mismatched), true,
			'the public schema alone deliberately does not know the active dialog');
		assert.equal(helpers.nativeAutotuneResultMatchesRequest(mismatched, publicJobId,
			'wan_sqm', 'pppoe-wan', 'main', '', 'variable_link', 'full_raw'), false,
			'the transport must independently bind a valid public result to the active dialog');
		const mismatchPayloads = [ capability,
			{ state: 'idle', instance: 'wan_sqm' },
			{ state: 'queued', job_id: publicJobId },
			{ state: 'review_ready', job_id: publicJobId,
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
			/no longer matches this request/);
		await assert.rejects(mismatchHelpers.cancelPreferredAutotuneJob(
			'wan_sqm', 'pppoe-wan', 'speedtest-go', 'variable_link', 'main', ''),
			/No authenticated calibration handle/,
			'a terminal invalid result must not leave a stale cancellable native handle');

		delays.length = 0;
		const cancelCalls = [];
		const cancelPayloads = [
			{ state: 'cancelling', job_id: publicJobId,
				runtime_mutated: true, recovery_required: false },
			{ state: 'recovering', job_id: publicJobId,
				runtime_mutated: true, recovery_required: true },
			{ state: 'cancelled', job_id: publicJobId,
				runtime_mutated: false, recovery_required: false },
		];
		const cancelHelpers = compileHelpers({
			exec(command, args) {
				cancelCalls.push({ command, args });
				return Promise.resolve({ stdout: JSON.stringify(cancelPayloads.shift()) });
			},
		});
		cancelHelpers.setNativeAutotuneJob('wan_sqm', publicJobId);
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
		native_public_result_version: 5,
	};
	const result = {
		state: 'complete', job_id: publicJobId,
		backend: 'speedtest-go', calibration: 'unshaped',
		shaper_bypassed: true, runtime_mutated: false, runtime_restored: true,
		limits_changed: false, download_kbps: 900000, upload_kbps: 800000,
	};

	try {
		assert.equal(helpers.nativeSpeedtestCapabilityValidated(capability), true);
		assert.equal(helpers.nativeSpeedtestCapabilityValidated({
			...capability, native_speedtest: false,
		}), false);
		assert.deepEqual(helpers.nativeSpeedtestLaunchArgs(
			'wanb_sqm', 'eth1', 'mwan3', 'wanb', '42', 'unshaped'), [
			'--calibrationctl', 'speedtest-start', '--instance', 'wanb_sqm',
			'--expected-target', 'eth1', '--backend', 'speedtest-go',
			'--direction', 'both', '--topology', 'unshaped', '--route-mode', 'mwan3',
			'--mwan3-member', 'wanb', '--server-id', '42',
		]);
		assert.equal(helpers.nativeSpeedtestResultValidated(result, publicJobId, 'unshaped'), true);
		assert.equal(helpers.nativeSpeedtestResultValidated({
			...result, calibration: 'current', shaper_bypassed: false, runtime_restored: false,
		}, publicJobId, 'current'), true);

		const calls = [];
		const payloads = [ capability, { state: 'queued', job_id: publicJobId },
			{ state: 'completed', job_id: publicJobId }, result ];
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
			[ '/usr/sbin/cake-autorated', 'speedtest-start' ],
			[ '/usr/sbin/cake-autorated', 'speedtest-status' ],
			[ '/usr/sbin/cake-autorated', 'speedtest-result' ],
		]);
		assert(!calls.some(call => call.command.endsWith('/speedtest')),
			'native admission must own the complete lifecycle');

		const failedCalls = [];
		const failedHelpers = compileHelpers({
			exec(command, args) {
				failedCalls.push({ command, args });
				if (args[1] === 'summary')
					return Promise.resolve({ stdout: JSON.stringify(capability) });
				return Promise.reject(new Error('XHR request timed out'));
			},
		});
		await assert.rejects(failedHelpers.runSpeedtestJob(
			'wan_sqm', 'pppoe-wan', 'speedtest-go', null, 'main', '', '', true, 'unshaped'), err => {
			assert.equal(err.nativeSpeedtestStartAttempted, true);
			return true;
		});
		assert.equal(failedCalls.length, 2,
			'an ambiguous native start must never be replayed through the legacy helper');

		const legacyCalls = [];
		const legacyPayloads = [
			{ state: 'running' },
			{ state: 'complete', download_kbps: 100000, upload_kbps: 20000 },
		];
		const legacyHelpers = compileHelpers({
			exec(command, args) {
				legacyCalls.push({ command, args });
				return Promise.resolve({ stdout: JSON.stringify(legacyPayloads.shift()) });
			},
		});
		await legacyHelpers.runSpeedtestJob(
			'wan_sqm', 'pppoe-wan', 'librespeed-cli', null, 'main', '', '', true);
		assert(legacyCalls.every(call => call.command === '/usr/libexec/cake-autorate-rs/rpcd-helper'),
			'an explicitly selected legacy backend must retain the established helper path');
		assert.deepEqual(legacyCalls.map(call => call.args[0]), [
			'speedtest-job-start', 'speedtest-job-status',
		], 'legacy Speed Test must use only pinned rpcd dispatcher operations');
	}
	finally {
		if (previousWindow === undefined)
			delete global.window;
		else
			global.window = previousWindow;
	}
}

async function testApplyGuardTransaction() {
	const previousWindow = global.window;
	global.window = {
		location: { href: 'https://router/settings#pending' },
		setTimeout(resolve) { resolve(); },
	};

	function transactionFixture(options = {}) {
		global.window.location = { href: 'https://router/settings#pending' };
		const values = {
			'cake-autorate': { wan_sqm: {
				'.name': 'wan_sqm', '.type': 'cake_autorate', enabled: '1', sqm_enabled: '1',
			} },
			sqm: {},
		};
			const calls = [];
			const sessionPackages = new Set([ 'cake-autorate', 'sqm' ]);
			let changesCalls = 0;
			let postcheckCalls = 0;
			let confirmCalls = 0;
			let finalized = false;
			if (options.preexistingGuard)
				values.sqm.cake_autorate_apply_wan_sqm = {
					'.name': 'cake_autorate_apply_wan_sqm', '.type': 'queue',
					enabled: '1', interface: 'user-owned',
				};
		const fakeUci = {
			set(config, section, key, value) {
				assert(values[config], `unexpected config ${config}`);
				assert(values[config][section], `missing section ${config}.${section}`);
				values[config][section][key] = value;
			},
			unset(config, section, key) {
				if (values[config] && values[config][section])
					delete values[config][section][key];
			},
			get(config, section, key) {
				const entry = values[config] && values[config][section];
				return key == null ? entry : entry && entry[key];
			},
			add(config, type, section) {
				assert(values[config]);
				values[config][section] = { '.name': section, '.type': type };
				return section;
			},
			sections(config, type) {
				return Object.values(values[config] || {}).filter(section => !type || section['.type'] === type)
					.map(section => ({ ...section }));
			},
			save() { calls.push('uci.save-token'); return Promise.resolve([]); },
			changes() {
				changesCalls++;
				const changes = {};
				sessionPackages.forEach(config => { changes[config] = [ [ 'set' ] ]; });
				if (options.otherChanges || (options.lateOtherChanges && changesCalls > 1))
					changes.network = [ [ 'set' ] ];
				return Promise.resolve(changes);
			},
			unload(packages) {
				calls.push(`uci.unload:${packages.join(',')}`);
			},
			callApply(timeout, rollback) {
				calls.push(`callApply:${timeout}:${rollback}`);
				if (options.applyReject)
					return Promise.reject(new Error('apply response lost'));
				return Promise.resolve(0);
			},
				callConfirmStatus() {
					calls.push('callConfirm');
					confirmCalls++;
					if (options.confirmRetryAck && confirmCalls === 1)
						return Promise.reject(new Error('confirm response lost'));
					if (options.confirmReject && confirmCalls === 1)
						return Promise.reject(new Error('confirm response lost'));
					if (options.confirmIndeterminate)
						return Promise.reject(new Error('confirm response lost'));
					if (options.confirmReject || options.confirmNonzero || options.rollbackAfterNoData)
						return Promise.resolve(5);
					return Promise.resolve(0);
				},
		};
		const token = 'b'.repeat(64);
		const fakeFs = {
			exec(command, args) {
				const rpcOperation = args[0];
				const operation = rpcOperation.replace(/^apply-guard-/, '');
				calls.push(`${command}:${rpcOperation}`);
				if (operation === 'arm') {
					const marker = values['cake-autorate'].wan_sqm;
					assert.deepEqual(args, [
						'apply-guard-arm', 'wan_sqm', marker._autotune_apply_target,
						marker._autotune_apply_backend, marker._autotune_apply_route_mode,
						marker._autotune_apply_mwan3_member || '', marker._autotune_apply_enabled,
						marker._autotune_apply_disable_adaptive, marker._autotune_apply_action,
						marker._autotune_apply_fingerprint, marker._autotune_apply_proposal_id,
					], 'LuCI Apply Guard argv must preserve the staged positional contract');
				}
				else if (rpcOperation.startsWith('apply-guard-')) {
					assert.deepEqual(args.slice(1), [ token ],
						'non-arm Apply Guard operations must carry exactly one authenticated token');
				}
				if (operation === 'arm')
					return Promise.resolve({ code: 0, stdout: JSON.stringify({
						state: 'armed', schema_version: 1, token, expires_epoch: 2000000000,
						boot_id: '11111111-2222-3333-4444-555555555555',
					}) });
				if (operation === 'status') {
					let state = finalized ? 'complete' : 'confirming';
					if (options.serverRolledBack)
						state = 'rolled-back';
					else if (options.serverIndeterminate)
						state = 'indeterminate';
					return Promise.resolve({ code: 0, stdout: JSON.stringify({
						state, schema_version: 1, token,
						message: state === 'complete' ? '' : `server ${state}`,
					}) });
				}
					if (operation === 'postcheck') {
					postcheckCalls++;
					return Promise.resolve(options.failPostcheck ||
						(options.confirmIndeterminate && postcheckCalls > 1) ?
						{ code: 1, stderr: 'postcheck rejected' } :
						{ code: 0, stdout: JSON.stringify({ state: 'verified', schema_version: 1, token }) });
					}
					if (operation === 'prepare-confirm')
						return Promise.resolve({ code: 0, stdout: JSON.stringify({
							state: 'prepared', schema_version: 1, token,
						}) });
					if (operation === 'reconcile') {
						if (options.confirmIndeterminate)
							return Promise.resolve({ code: 1, stderr: 'reconcile rejected' });
						return Promise.resolve({ code: 0, stdout: JSON.stringify({
							state: options.rollbackAfterNoData ? 'rolled-back' : 'confirmed',
							schema_version: 1, token,
						}) });
					}
				if (operation === 'verify-rollback')
					return Promise.resolve(options.confirmReject || options.confirmNonzero || options.confirmIndeterminate ?
						{ code: 1, stderr: 'not rolled back' } :
						{ code: 0, stdout: JSON.stringify({ state: 'rolled-back', schema_version: 1, token }) });
				if (operation === 'finalize') {
					finalized = true;
					return Promise.resolve({ code: 0, stdout: JSON.stringify({ state: 'finalized', schema_version: 1 }) });
				}
				if (operation === 'abort')
					return Promise.resolve({ code: 0, stdout: JSON.stringify({ state: 'aborted', schema_version: 1 }) });
				throw new Error(`unexpected exec ${command} ${args.join(' ')}`);
			},
		};
			const fakeL = {
			resolveDefault(promise, fallback) { return Promise.resolve(promise).catch(() => fallback); },
			};
			const fakeRpc = {
				declare(spec) {
					if (spec.method === 'revert')
						return config => {
							calls.push(`uci.revert:${config}`);
							if (options.cleanupReject && config === 'sqm')
								return Promise.resolve(4);
							sessionPackages.delete(config);
							return Promise.resolve(0);
						};
					return () => fakeUci.callConfirmStatus();
				},
			};
			const helpers = compileHelpers(fakeFs, fakeUci, fakeL, fakeRpc);
			const guardedResult = {
				...validResult,
				runs: [ { backend: 'speedtest-go' } ],
				proposals: [ {
					schema_version: 1,
					proposal_id: `p-${'1'.repeat(24)}`,
					rank: 1,
					action: 'apply_sqm',
					topology: 'both_shaped',
					is_primary: true,
					applicable: true,
					hard_safety_pass: true,
					profile_target_met: true,
					profile_objectives_met: true,
					grade: 'A',
					effective_delta_ms: 10,
					confidence_percent: 100,
					unmet_objectives: [],
					evidence: {
						validation: 'validation',
						confirmation: 'bidirectional_confirmation',
					},
					configuration: proposal,
				} ],
			};
			if (options.preexistingGuard) {
				assert.throws(() => helpers.stageAutotuneApplyMarker('wan_sqm', {
					autotune_result: guardedResult,
					speedtest_backend: 'speedtest-go', enabled: true,
					adaptive_ceiling_disable_confirmed: false,
				}), /already exists/);
				assert.equal(values.sqm.cake_autorate_apply_wan_sqm.interface, 'user-owned');
				return { helpers, calls, values };
			}
			helpers.stageAutotuneApplyMarker('wan_sqm', {
			autotune_result: guardedResult,
			speedtest_backend: 'speedtest-go', enabled: true,
			adaptive_ceiling_disable_confirmed: false,
		});
		assert.equal(values['cake-autorate'].wan_sqm._autotune_apply_guard, '1');
		assert.equal(values['cake-autorate'].wan_sqm._autotune_apply_token, undefined,
			'a staged proposal must not carry a token before Save & Apply arms it');
		assert.equal(values.sqm.cake_autorate_apply_wan_sqm._autotune_apply_guard, '1',
			'SQM must be enrolled in the same rollback transaction before apply');
		const view = {
			handleSave() {
				calls.push('view.handleSave');
				values['cake-autorate'].wan_sqm.enabled = '0';
				values['cake-autorate'].wan_sqm.sqm_enabled = '0';
				return Promise.resolve();
			},
		};
		return { helpers, view, calls, token, values };
	}

	try {
			transactionFixture({ preexistingGuard: true });

		const success = transactionFixture();
		await success.helpers.runGuardedSaveApply(success.view, {});
		assert.equal(success.values['cake-autorate'].wan_sqm.enabled, '1');
		assert.equal(success.values['cake-autorate'].wan_sqm.sqm_enabled, '1',
			'guarded apply must preserve the wizard-staged enabled service state');
		assert.deepEqual(success.calls, [
			'/usr/libexec/cake-autorate-rs/rpcd-helper:apply-guard-arm',
			'uci.save-token',
			'callApply:30:true',
			'/usr/libexec/cake-autorate-rs/rpcd-helper:apply-guard-status',
			'callConfirm',
			'/usr/libexec/cake-autorate-rs/rpcd-helper:apply-guard-finalize',
			'/usr/libexec/cake-autorate-rs/rpcd-helper:apply-guard-status',
			'uci.revert:cake-autorate',
			'uci.revert:sqm',
			'uci.unload:cake-autorate,sqm',
		], 'the independent supervisor prepares the transaction and LuCI confirms with its authenticated RPC session');

		const cleanupFailure = transactionFixture({ cleanupReject: true });
		await assert.rejects(cleanupFailure.helpers.runGuardedSaveApply(cleanupFailure.view, {}),
			/applied and confirmed, but the browser UCI transaction could not be cleared/);
		assert(cleanupFailure.calls.includes('/usr/libexec/cake-autorate-rs/rpcd-helper:apply-guard-finalize'),
			'cleanup is attempted only after authoritative guard finalization');
		assert(!cleanupFailure.calls.some(call => call.endsWith(':apply-guard-verify-rollback')),
			'a browser reconciliation failure must not roll back a confirmed configuration');
		assert(!cleanupFailure.calls.includes('/usr/libexec/cake-autorate-rs/rpcd-helper:apply-guard-abort'),
			'a finalized guard must not be aborted after a browser reconciliation failure');
		assert.deepEqual(cleanupFailure.calls.slice(-3), [
			'uci.revert:cake-autorate',
			'uci.revert:sqm',
			'uci.unload:cake-autorate',
		], 'a partial server cleanup must unload every successfully reverted package before surfacing failure');
		assert.equal(global.window.location.href, 'https://router/settings#pending',
			'a confirmed apply reconciliation failure must remain visible instead of navigating away');

		const lateUnrelated = transactionFixture({ lateOtherChanges: true });
		await lateUnrelated.helpers.runGuardedSaveApply(lateUnrelated.view, {});
		assert(!lateUnrelated.calls.includes('uci.revert:network'),
			'success reconciliation must never discard a package staged concurrently by another view');

		const lostApply = transactionFixture({ applyReject: true });
		await assert.rejects(lostApply.helpers.runGuardedSaveApply(lostApply.view, {}),
			/apply response lost/);
		assert.equal(global.window.location, 'https://router/settings',
			'exact client-side rollback must discard the staged LuCI model by reloading');
		assert.equal(lostApply.calls[0], '/usr/libexec/cake-autorate-rs/rpcd-helper:apply-guard-arm');
		assert(!lostApply.calls.includes('view.handleSave'),
			'a staged exact proposal must not be rewritten through hidden modal fields');
		assert.equal(lostApply.calls.filter(call => call.endsWith(':apply-guard-verify-rollback')).length, 2);
		assert(lostApply.calls.includes('/usr/libexec/cake-autorate-rs/rpcd-helper:apply-guard-abort'),
			'an unknown apply response must retain snapshots until exact rollback is proven');

		const serverRollback = transactionFixture({ serverRolledBack: true });
		await assert.rejects(serverRollback.helpers.runGuardedSaveApply(serverRollback.view, {}),
			/server rolled-back/);
		assert.equal(global.window.location, 'https://router/settings',
			'an authoritative server rollback must reload the marker-free configuration');
		assert.equal(serverRollback.calls[0], '/usr/libexec/cake-autorate-rs/rpcd-helper:apply-guard-arm');
		assert(!serverRollback.calls.some(call => call.endsWith(':apply-guard-verify-rollback')),
			'a server rollback receipt is already authoritative');
		assert(!serverRollback.calls.includes('/usr/libexec/cake-autorate-rs/rpcd-helper:apply-guard-abort'),
			'the server supervisor owns token cleanup');

		const serverIndeterminate = transactionFixture({ serverIndeterminate: true });
		await assert.rejects(serverIndeterminate.helpers.runGuardedSaveApply(serverIndeterminate.view, {}),
			/confirmation outcome remains unknown/);
		assert(!serverIndeterminate.calls.includes('/usr/libexec/cake-autorate-rs/rpcd-helper:apply-guard-abort'),
			'an indeterminate server state must retain its proof for recovery');
		assert.equal(global.window.location.href, 'https://router/settings#pending',
			'an indeterminate confirmation must remain visible instead of navigating away');

		const unrelated = transactionFixture({ otherChanges: true });
		await assert.rejects(unrelated.helpers.runGuardedSaveApply(unrelated.view, {}),
			/only its exact CAKE and SQM changes/);
		assert(!unrelated.calls.some(call => call.startsWith('callApply:')),
			'unrelated pending UCI packages must be rejected before apply');
		assert(unrelated.calls.includes('/usr/libexec/cake-autorate-rs/rpcd-helper:apply-guard-abort'));
	}
	finally {
		if (previousWindow === undefined)
			delete global.window;
		else
			global.window = previousWindow;
	}
}

testSequentialMultiwanTransactions().then(() => testAutotuneTerminalPrecedence()).then(() =>
	testNativeSpeedtestTransport()).then(() => testNativeAutotuneTransport()).then(() =>
	testApplyGuardTransaction()).then(() => {
	console.log('settings autotune tests passed');
}).catch(err => {
	console.error(err);
	process.exitCode = 1;
});
