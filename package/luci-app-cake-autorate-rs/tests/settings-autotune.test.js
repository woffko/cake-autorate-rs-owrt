'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

const sourcePath = path.join(__dirname, '..', 'htdocs', 'luci-static', 'resources',
	'view', 'cake-autorate-rs', 'settings.js');
const source = fs.readFileSync(sourcePath, 'utf8');
const prefix = source.slice(0, source.indexOf('return L.view.extend'));
assert.match(source,
	/Even an evidence-backed no-SQM recommendation[\s\S]*state\.autotune_action = 'apply_sqm';/,
	'disable-SQM comparison must never be the preselected Review action');
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
function compileHelpers(fsImpl, uciImpl, lImpl, rpcImpl) {
	return new Function(
		'fs', 'form', 'network', 'uci', 'ui', 'widgets', 'cakeUi', 'rpc', 'L', 'E', '_',
		`${prefix}\ninterfaceContext = { deviceNames: { eth1: true }, deviceNetworks: {}, ` +
			`networkDevices: {}, defaultDevice: 'eth1' };\nreturn { writeWizardConfig, validateTransportProbeUrl, ` +
			`buildMwan3Context, uniqueMwan3Uplinks, multiwanInstancePlans, wizardPlanConflicts, ` +
			`topicTab, autorateSubcategory, autorateSubcategoryDefinitions, ` +
			`canonicalAutotuneProfile, autotuneProfilePolicy, autotuneProfileDefinitions, ` +
			`autotuneProposalMatchesProfile, ` +
			`autotuneResultValidated, autotuneResultReviewable, ` +
			`autotuneResultHasReviewChoice, autotuneDisableSqmEvidenceValidated, ` +
			`autotuneAttemptDiagnostics, autotuneDiagnostics, ` +
			`autotuneRuntimeSettled, autotuneLegacyResult, ` +
			`autotuneRecoveryPending, autotuneRecoveryProgress, ` +
			`revalidateAutotuneProposal, ` +
			`stageAutotuneApplyMarker, pendingAutotuneApplyMarkers, ` +
			`armAutotuneApplyGuards, runGuardedSaveApply, ` +
			`clearAutotuneProposalState, recordAutotuneTerminalFailure, ` +
			`autotuneRetryableInconclusive, recordAutotuneRetryableInconclusive, ` +
			`adaptiveCeilingWritePlan, runAutotuneJob, ` +
			`setInterfaceContext: function(value) { interfaceContext = value; }, ` +
			`setMwan3Context: function(value) { mwan3Context = value; } };`
	)(fsImpl || {}, {}, {}, uciImpl || uci, {}, {}, {}, rpcImpl || {
		declare() { return () => Promise.resolve(0); },
	}, lImpl || {}, () => ({}), value => value);
}

const helpers = compileHelpers({});

assert.equal(helpers.topicTab('setup'), 'autorate');
assert.equal(helpers.topicTab('general'), 'autorate');
assert.equal(helpers.topicTab('sqm_qdisc'), 'sqm');
assert.equal(helpers.topicTab('speedtest'), 'testing');
assert.equal(helpers.topicTab('logging'), 'monitoring');
assert.equal(helpers.autorateSubcategory('setup', 'wan_if'), 'connection');
assert.equal(helpers.autorateSubcategory('setup', 'min_dl_shaper_rate_kbps'), 'limits');
assert.equal(helpers.autorateSubcategory('rates', 'adaptive_ceiling_enabled'), 'ceiling');
assert.equal(helpers.autorateSubcategory('reflectors', 'reflector'), 'probes');
assert.equal(helpers.autorateSubcategory('quality', 'transport_probe_backend'), 'probes');
assert.equal(helpers.autorateSubcategory('quality', 'rating_load_enter_ratio'), 'quality');
assert.equal(helpers.autorateSubcategory('controller', 'alpha_delta_ewma'), 'controller');
assert.deepEqual(helpers.autorateSubcategoryDefinitions().map(group => group.id),
	[ 'connection', 'limits', 'ceiling', 'probes', 'quality', 'controller' ]);
assert.equal(helpers.canonicalAutotuneProfile('balanced'), 'best_overall');
assert.equal(helpers.canonicalAutotuneProfile('unknown'), null);
assert.deepEqual(helpers.autotuneProfileDefinitions().map(profile => profile.id),
	[ 'gaming', 'best_overall', 'fair' ]);
assert.equal(helpers.autotuneProfilePolicy('gaming').sqm.classification, 'diffserv4');
assert.equal(helpers.autotuneProfilePolicy('gaming').delayMaxMs, 5);
assert.equal(helpers.autotuneProfilePolicy('best_overall').retentionPercent, 80);
assert.equal(helpers.autotuneProfilePolicy('best_overall').sqm.script, 'layer_cake.qos');
assert.equal(helpers.autotuneProfilePolicy('best_overall').sqm.iqdiscOpts, 'besteffort');
assert.equal(helpers.autotuneProfilePolicy('best_overall').sqm.eqdiscOpts, 'diffserv4');
assert.equal(helpers.autotuneProfilePolicy('fair').retentionPercent, 90);

const proposal = {
	schema_version: 3,
	profile: 'best_overall',
	target_grade: 'A',
	quality_target_required: true,
	throughput_priority: false,
	download: {
		minimum_kbps: 16700,
		base_kbps: 35500,
		maximum_kbps: 141800,
		absolute_cap_kbps: 204200,
		observed_low_kbps: 41700,
		observed_median_kbps: 95000,
		observed_high_kbps: 170000,
	},
	upload: {
		minimum_kbps: 6500,
		base_kbps: 14300,
		maximum_kbps: 17200,
		absolute_cap_kbps: 19000,
		observed_low_kbps: 16200,
		observed_median_kbps: 16800,
		observed_high_kbps: 18000,
	},
	active_threshold_kbps: 1600,
	thresholds_ms: { adjust_up: 6, delay: 15, adjust_down: 40 },
	adaptive_ceiling: {
		enabled: true,
		hold_s: 15,
		growth_percent: 3,
		probe_s: 8,
		cooldown_s: 45,
		failed_bound_ttl_s: 900,
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
assert.equal(written.max_dl_shaper_rate_kbps, '141800');
assert.equal(written.min_ul_shaper_rate_kbps, '6500');
assert.equal(written.base_ul_shaper_rate_kbps, '14300');
assert.equal(written.max_ul_shaper_rate_kbps, '17200');
assert.equal(written.connection_active_thr_kbps, '1600');
assert.equal(written.dl_avg_owd_delta_max_adjust_up_thr_ms, '6');
assert.equal(written.ul_owd_delta_delay_thr_ms, '15');
assert.equal(written.dl_avg_owd_delta_max_adjust_down_thr_ms, '40');
assert.equal(written.adaptive_ceiling_enabled, '1');
assert.equal(written.adaptive_ceiling_dl_cap_kbps, '204200');
assert.equal(written.adaptive_ceiling_ul_cap_kbps, '19000');
assert.equal(written.adaptive_ceiling_cooldown_s, '45');
assert.equal(written.transport_latency_enabled, '1');
assert.equal(written.throughput_guard_enabled, '1');
assert.equal(written.autotune_profile, 'best_overall');
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
	quality_target_met: true,
	actual_grade: 'A',
	effective_delta_ms: 10,
	contaminated: false,
	gates: passingGateCodes.map(code => ({
		code,
		required: true,
		pass: true,
		actual: code.includes('candidate-realization') ||
			code.includes('capacity-retention') ? 100 : 0,
		limit: gateLimit(code),
	})),
	correction: { action: 'none', feasible: true },
	direction_phases: {
		download: cleanDirectionPhase('download'),
		upload: cleanDirectionPhase('upload'),
	},
};
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
	schema_version: 5,
	producer: 'cake-autorate-rs-autotune',
	run_id: 'settings-test-run',
	profile: 'best_overall',
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
		delay_max_ms: 30,
		loss_max_percent: 3,
		cpu_max_percent: 85,
	},
	proposal,
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
const resultForProfile = (profile, targetGrade, retention, delay, loss, sqm) => ({
	...validResult,
	profile,
	validation: {
		...validValidation,
		profile,
		gates: validValidation.gates.map(gate => ({
			...gate,
			required: profile === 'fair' && gate.code.includes('latency') ? false : true,
			limit: gate.code.includes('capacity-retention') ? retention :
				(gate.code.includes('latency') ? delay :
					(gate.code.includes('packet-loss') ? loss : gate.limit)),
		})),
	},
	validation_thresholds: {
		...validResult.validation_thresholds,
		capacity_retention_min_percent: retention,
		delay_max_ms: delay,
		loss_max_percent: loss,
	},
	proposal: {
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
	},
});
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
const fairResult = {
	...resultForProfile('fair', 'C', 90, 200, 5, proposal.sqm),
	fair_outcome: {
		mode: 'quality-target-met',
		target_grade: 'C',
		target_delta_ms: 200,
		capacity_floor_percent: 90,
		actual_grade: 'A',
		actual_effective_delta_ms: 10,
		recommended_action: 'apply_sqm',
		allowed_actions: [ 'apply_sqm', 'keep_current' ],
		disable_sqm_available: false,
		comparison_reason: 'quality-target-met',
		no_sqm_control: { available: false },
		throughput_gain_without_sqm: { download_percent: 0, upload_percent: 0 },
	},
};
assert.equal(helpers.autotuneResultValidated(gamingResult), true);
assert.equal(helpers.autotuneResultValidated(fairResult), true);
const fairFallback = {
	...fairResult,
	auto_apply_eligible: false,
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
			},
			grade: 'D',
			effective_delta_ms: 218,
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
assert.equal(helpers.autotuneResultReviewable(fairDisable, 'disable_sqm'), true);
assert.equal(helpers.autotuneResultReviewable({
	...fairDisable,
	fair_outcome: {
		...fairDisable.fair_outcome,
		throughput_gain_without_sqm: { download_percent: 3, upload_percent: 1.9 },
	},
}, 'disable_sqm'), false, 'both no-SQM directions must improve by at least 2%');
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
assert.equal(preserveAdaptive.dl_cap_kbps, 150000);
assert.equal(preserveAdaptive.ul_cap_kbps, 17200,
	'preserved cap must be raised only as far as the new maximum requires');
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
assert.equal(written.adaptive_ceiling_dl_cap_kbps, '150000');
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
const duplicatePlans = [ plans[0], { ...plans[1], name: 'primary_sqm' } ];
assert.match(helpers.wizardPlanConflicts(duplicatePlans, false).join(' '), /duplicated/);

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
assert.match(source, /alert-message %s.*warning/s,
	'retryable inconclusive diagnostics must render as a warning, not a red failure');
assert.match(source, /autotuneResultReviewable\(state\.autotune_result, selectedAction\)/);
assert.match(source, /Disable autorate and SQM/);
assert.match(source, /Keep current settings/);
assert.match(source, /I understand that this disables CAKE shaping/);
assert.match(source, /start-conservative/);
assert.match(source, /Continue conservatively/);
assert.match(source, /confidence_mode === 'low'/);
assert.match(source, /s\.tab\('autorate', _\('Autorate setup'\)\)/);
assert.match(source, /s\.tab\('sqm', _\('SQM setup'\)\)/);
assert.match(source, /s\.tab\('testing', _\('Testing & Auto-Tune'\)\)/);
assert.match(source, /decorateAutorateSubcategories/);
for (const label of [ 'Connection & routing', 'Rate limits', 'Adaptive ceiling',
	'Latency probes', 'Quality & rating', 'Controller' ])
	assert(source.includes(label), `missing Autorate subcategory: ${label}`);
assert(source.includes("'class': 'cbi-tabmenu cake-autorate-subnav'"),
	'Autorate subcategories must use native LuCI tab styling');
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

	function pollingHelpers(payloads, calls) {
		return compileHelpers({
			exec(command, args) {
				calls.push({ command, args });
				assert(payloads.length, 'unexpected extra Auto-Tune poll');
				return Promise.resolve({ stdout: JSON.stringify(payloads.shift()) });
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
				command: '/usr/libexec/cake-autorate-rs/autotune',
				args: [ 'wan_sqm', 'pppoe-wan', 'status', 'speedtest-go',
					'main', '', 'best_overall' ],
			},
			{
				command: '/usr/libexec/cake-autorate-rs/autotune',
				args: [ 'wan_sqm', 'pppoe-wan', 'attest', 'speedtest-go',
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

async function testApplyGuardTransaction() {
	const previousWindow = global.window;
	global.window = {
		location: { href: 'https://router/settings#pending' },
		setTimeout(resolve) { resolve(); },
	};

		function transactionFixture(options = {}) {
		global.window.location = { href: 'https://router/settings#pending' };
		const values = {
			'cake-autorate': { wan_sqm: { '.name': 'wan_sqm', '.type': 'cake_autorate' } },
			sqm: {},
		};
			const calls = [];
			let postcheckCalls = 0;
			let confirmCalls = 0;
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
				return Promise.resolve(options.otherChanges ?
					{ 'cake-autorate': [ [ 'set' ] ], sqm: [ [ 'set' ] ], network: [ [ 'set' ] ] } :
					{ 'cake-autorate': [ [ 'set' ] ], sqm: [ [ 'set' ] ] });
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
				const operation = args[0];
				calls.push(`${command}:${operation}`);
				if (operation === 'arm')
					return Promise.resolve({ code: 0, stdout: JSON.stringify({
						state: 'armed', schema_version: 1, token, expires_epoch: 2000000000,
						boot_id: '11111111-2222-3333-4444-555555555555',
					}) });
				if (operation === 'status') {
					let state = 'complete';
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
				if (operation === 'finalize')
					return Promise.resolve({ code: 0, stdout: JSON.stringify({ state: 'finalized', schema_version: 1 }) });
				if (operation === 'abort')
					return Promise.resolve({ code: 0, stdout: JSON.stringify({ state: 'aborted', schema_version: 1 }) });
				throw new Error(`unexpected exec ${command} ${args.join(' ')}`);
			},
		};
			const fakeL = {
			resolveDefault(promise, fallback) { return Promise.resolve(promise).catch(() => fallback); },
			};
			const fakeRpc = {
				declare() { return () => fakeUci.callConfirmStatus(); },
			};
			const helpers = compileHelpers(fakeFs, fakeUci, fakeL, fakeRpc);
			if (options.preexistingGuard) {
				assert.throws(() => helpers.stageAutotuneApplyMarker('wan_sqm', {
					autotune_result: { ...validResult, runs: [ { backend: 'speedtest-go' } ] },
					speedtest_backend: 'speedtest-go', enabled: true,
					adaptive_ceiling_disable_confirmed: false,
				}), /already exists/);
				assert.equal(values.sqm.cake_autorate_apply_wan_sqm.interface, 'user-owned');
				return { helpers, calls, values };
			}
			helpers.stageAutotuneApplyMarker('wan_sqm', {
			autotune_result: { ...validResult, runs: [ { backend: 'speedtest-go' } ] },
			speedtest_backend: 'speedtest-go', enabled: true,
			adaptive_ceiling_disable_confirmed: false,
		});
		assert.equal(values['cake-autorate'].wan_sqm._autotune_apply_guard, '1');
		assert.equal(values['cake-autorate'].wan_sqm._autotune_apply_token, undefined,
			'a staged proposal must not carry a token before Save & Apply arms it');
		assert.equal(values.sqm.cake_autorate_apply_wan_sqm._autotune_apply_guard, '1',
			'SQM must be enrolled in the same rollback transaction before apply');
		const view = {
			handleSave() { calls.push('view.handleSave'); return Promise.resolve(); },
		};
		return { helpers, view, calls, token };
	}

	try {
			transactionFixture({ preexistingGuard: true });

			const success = transactionFixture();
		await success.helpers.runGuardedSaveApply(success.view, {});
		assert.deepEqual(success.calls, [
			'view.handleSave',
			'/usr/libexec/cake-autorate-rs/apply-guard:arm',
			'uci.save-token',
			'callApply:30:true',
			'/usr/libexec/cake-autorate-rs/apply-guard:status',
		], 'the router-side supervisor owns verification and confirmation');

		const lostApply = transactionFixture({ applyReject: true });
		await assert.rejects(lostApply.helpers.runGuardedSaveApply(lostApply.view, {}),
			/apply response lost/);
		assert.equal(lostApply.calls.filter(call => call.endsWith(':verify-rollback')).length, 2);
		assert(lostApply.calls.includes('/usr/libexec/cake-autorate-rs/apply-guard:abort'),
			'an unknown apply response must retain snapshots until exact rollback is proven');

		const serverRollback = transactionFixture({ serverRolledBack: true });
		await assert.rejects(serverRollback.helpers.runGuardedSaveApply(serverRollback.view, {}),
			/server rolled-back/);
		assert(!serverRollback.calls.some(call => call.endsWith(':verify-rollback')),
			'a server rollback receipt is already authoritative');
		assert(!serverRollback.calls.includes('/usr/libexec/cake-autorate-rs/apply-guard:abort'),
			'the server supervisor owns token cleanup');

		const serverIndeterminate = transactionFixture({ serverIndeterminate: true });
		await assert.rejects(serverIndeterminate.helpers.runGuardedSaveApply(serverIndeterminate.view, {}),
			/confirmation outcome remains unknown/);
		assert(!serverIndeterminate.calls.includes('/usr/libexec/cake-autorate-rs/apply-guard:abort'),
			'an indeterminate server state must retain its proof for recovery');

		const unrelated = transactionFixture({ otherChanges: true });
		await assert.rejects(unrelated.helpers.runGuardedSaveApply(unrelated.view, {}),
			/only its exact CAKE and SQM changes/);
		assert(!unrelated.calls.some(call => call.startsWith('callApply:')),
			'unrelated pending UCI packages must be rejected before apply');
		assert(unrelated.calls.includes('/usr/libexec/cake-autorate-rs/apply-guard:abort'));
	}
	finally {
		if (previousWindow === undefined)
			delete global.window;
		else
			global.window = previousWindow;
	}
}

testAutotuneTerminalPrecedence().then(() => testApplyGuardTransaction()).then(() => {
	console.log('settings autotune tests passed');
}).catch(err => {
	console.error(err);
	process.exitCode = 1;
});
