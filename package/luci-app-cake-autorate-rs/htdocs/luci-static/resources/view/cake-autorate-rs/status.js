'use strict';
'require fs';
'require poll';
'require uci';
'require ui';
'require cake-autorate-rs.ui as cakeUi';

var STATUS_COLUMN_DEFINITIONS = [
	{ key: 'instance', title: _('Instance'), mandatory: true },
	{ key: 'uplink', title: _('Uplink / state'), mandatory: true },
	{ key: 'services', title: _('Services'), mandatory: true },
	{ key: 'quality', title: _('Quality'), mandatory: true },
	{ key: 'rating', title: _('Rating test'), mandatory: true },
	{ key: 'route', title: _('Route / external IP') },
	{ key: 'updated', title: _('Updated') },
	{ key: 'reflector', title: _('Reflector') },
	{ key: 'runtime_reflectors', title: _('Runtime reflectors') },
	{ key: 'rtt', title: _('RTT') },
	{ key: 'dl_achieved', title: _('DL achieved') },
	{ key: 'ul_achieved', title: _('UL achieved') },
	{ key: 'cake_dl', title: _('CAKE DL') },
	{ key: 'cake_ul', title: _('CAKE UL') },
	{ key: 'cpu', title: _('CPU') }
];

var STATUS_DEFAULT_COLUMNS = STATUS_COLUMN_DEFINITIONS.filter(function(column) {
	return column.mandatory;
}).map(function(column) { return column.key; });

function statusColumnSelection(globalSection) {
	var configured = globalSection && globalSection.status_columns;
	var values = Array.isArray(configured) ? configured :
		(typeof configured === 'string' ? configured.split(/[\s,]+/) : []);
	var selected = {};

	STATUS_DEFAULT_COLUMNS.forEach(function(key) { selected[key] = true; });
	values.forEach(function(key) {
		if (STATUS_COLUMN_DEFINITIONS.some(function(column) { return column.key === key; }))
			selected[key] = true;
	});

	return STATUS_COLUMN_DEFINITIONS.filter(function(column) {
		return selected[column.key];
	}).map(function(column) { return column.key; });
}

function selectedStatusColumns(keys) {
	var selected = {};
	(keys || []).forEach(function(key) { selected[key] = true; });
	STATUS_DEFAULT_COLUMNS.forEach(function(key) { selected[key] = true; });
	return STATUS_COLUMN_DEFINITIONS.filter(function(column) {
		return selected[column.key];
	});
}

function statusPath(section) {
	return '/var/run/cake-autorate/' + section + '/status.json';
}

function readStatus(section) {
	return L.resolveDefault(fs.read(statusPath(section)).then(JSON.parse), null);
}

function readPackageVersions() {
	return L.resolveDefault(
		fs.exec('/usr/libexec/cake-autorate-rs/package-versions', []).then(function(result) {
			var rows = JSON.parse(result && result.stdout || '[]');
			var versions = {};

			rows.forEach(function(row) {
				if (row && row.name)
					versions[row.name] = String(row.version || '-');
			});
			return versions;
		}),
		{}
	);
}

function parseExecJson(result) {
	var text = result && result.stdout ? result.stdout.trim() : '';

	if (!text)
		throw new Error(_('Helper returned no data.'));
	return JSON.parse(text);
}

function readRuntimeHealth() {
	return L.resolveDefault(
		fs.exec('/usr/libexec/cake-autorate-rs/runtime-health', []).then(parseExecJson).then(function(result) {
			return result && result.instances || {};
		}),
		{}
	);
}

function schedulerStatusSource(section, schedulerEngine, calibrationSummary) {
	if (schedulerEngine === 'native' && calibrationSummary &&
	    calibrationSummary.native_scheduler === true)
		return {
			owner: 'native',
			command: '/usr/sbin/cake-autorated',
			args: [ '--calibrationctl', 'scheduler-status' ]
		};
	if (schedulerEngine === 'legacy' && calibrationSummary &&
	    calibrationSummary.native_scheduler === false)
		return {
			owner: 'legacy',
			command: '/usr/libexec/cake-autorate-rs/autotune-scheduler',
			args: [ 'status', section ]
		};
	return null;
}

function schedulerUnavailableStatus(sectionData, owner) {
	return {
		owner: owner || 'unconfigured',
		available: false,
		instance: sectionData['.name'],
		enabled: sectionData.scheduled_autotune_enabled === '1',
		state: 'unavailable',
		message: owner === 'native' ?
			_('Native scheduler status is unavailable; legacy accounting was not substituted.') :
			(owner === 'legacy' ?
				_('Legacy scheduler ownership could not be attested; native accounting was not substituted.') :
				_('Scheduler ownership is missing or invalid; no accounting source was selected.')),
		accounting_error: false,
		initialized: false,
		budget_authoritative: false,
		warning: null,
		source: 'none'
	};
}

function nativeSchedulerStatusValidated(result, section) {
	var states = [ 'disabled', 'initializing', 'deferred', 'idle', 'running', 'blocked', 'error' ];
	function uintValid(value) {
		return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
	}

	function budgetValid(value) {
		return !!value && [ 'limit_bytes', 'used_bytes', 'reserved_bytes', 'remaining_bytes' ]
			.every(function(key) {
				return uintValid(value[key]);
			});
	}

	return !!result && result.instance === section && typeof result.enabled === 'boolean' &&
		typeof result.initialized === 'boolean' &&
		typeof result.budget_authoritative === 'boolean' &&
		states.indexOf(result.state) >= 0 && typeof result.message === 'string' &&
		result.message.length <= 512 && !/[\u0000-\u001f\u007f]/.test(result.message) &&
		(result.warning === null || (typeof result.warning === 'string' && result.warning &&
		 result.warning.length <= 512 && !/[\u0000-\u001f\u007f]/.test(result.warning))) &&
		uintValid(result.observed_at) && uintValid(result.updated_at) &&
		uintValid(result.next_due_at) && uintValid(result.last_success_at) &&
		uintValid(result.last_failure_due_at) && !!result.window &&
		uintValid(result.window.start_hour) && result.window.start_hour <= 23 &&
		uintValid(result.window.end_hour) && result.window.end_hour <= 23 &&
		budgetValid(result.daily) && budgetValid(result.monthly) &&
		typeof result.accounting_error === 'boolean';
}

function nativeSchedulerBatchValidated(result) {
	var seen = Object.create(null);

	if (!result || result.schema_version !== 1 || result.owner !== 'native' ||
	    result.available !== true ||
	    typeof result.observed_at !== 'number' || !Number.isSafeInteger(result.observed_at) ||
	    result.observed_at < 0 ||
	    typeof result.stale !== 'boolean' ||
	    (result.stale ?
		(typeof result.global_error !== 'string' || !result.global_error ||
		 result.global_error.length > 512 || /[\u0000-\u001f\u007f]/.test(result.global_error)) :
		result.global_error !== null) ||
	    !Array.isArray(result.instances) || !Array.isArray(result.issues) ||
	    result.instances.length + result.issues.length > 128)
		return false;

	return result.instances.concat(result.issues).every(function(instance) {
		var name = instance && instance.instance;
		if (typeof name !== 'string' || !name ||
		    Object.prototype.hasOwnProperty.call(seen, name) ||
		    instance.observed_at !== result.observed_at ||
		    !nativeSchedulerStatusValidated(instance, name))
			return false;
		seen[name] = true;
		return true;
	});
}

function legacySchedulerStatusValidated(result, section) {
	function uintValid(value) {
		return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
	}

	function budgetValid(value) {
		return !!value && [ 'limit_bytes', 'used_bytes', 'remaining_bytes' ].every(function(key) {
			return uintValid(value[key]);
		});
	}

	return !!result && result.instance === section && typeof result.enabled === 'boolean' &&
		typeof result.state === 'string' && typeof result.message === 'string' &&
		uintValid(result.updated_at) && uintValid(result.next_due_at) &&
		uintValid(result.window_start_hour) && result.window_start_hour <= 23 &&
		uintValid(result.window_end_hour) && result.window_end_hour <= 23 &&
		budgetValid(result.daily) && budgetValid(result.monthly) &&
		typeof result.accounting_error === 'boolean';
}

function normalizeLegacySchedulerStatus(result) {
	return Object.assign(Object.create(null), result, {
		owner: 'legacy',
		available: true,
		source: 'legacy_scheduler_helper',
		initialized: true,
		budget_authoritative: !result.accounting_error,
		warning: null,
		window: {
			start_hour: Number(result.window_start_hour),
			end_hour: Number(result.window_end_hour)
		},
		daily: Object.assign(Object.create(null), { reserved_bytes: 0 }, result.daily),
		monthly: Object.assign(Object.create(null), { reserved_bytes: 0 }, result.monthly)
	});
}

function readSchedulerStatuses(sections, schedulerEngine, calibrationSummary) {
	var unavailable = sections.map(function(section) {
		return schedulerUnavailableStatus(section, schedulerEngine);
	});
	var source = schedulerStatusSource('', schedulerEngine, calibrationSummary);

	if (!source)
		return Promise.resolve({ rows: unavailable, diagnostics: [] });

	if (source.owner === 'native') {
		return L.resolveDefault(
			fs.exec(source.command, source.args).then(parseExecJson).then(function(result) {
				var byInstance = Object.create(null);
				var configured = Object.create(null);
				var diagnostics = [];
				if (!nativeSchedulerBatchValidated(result))
					throw new Error(_('Native scheduler returned an invalid batch status contract.'));
				if (result.stale) {
					diagnostics.push({
						instance: _('Native scheduler'),
						message: result.global_error
					});
				}
				sections.forEach(function(section) { configured[section['.name']] = true; });
				result.instances.concat(result.issues).forEach(function(instance) {
					byInstance[instance.instance] = Object.assign(Object.create(null), instance, {
						owner: 'native', schema_version: result.schema_version,
						available: true, source: 'native_scheduler_snapshot',
						stale: result.stale, global_error: result.global_error
					});
				});
				return {
					rows: sections.map(function(section, index) {
						return Object.prototype.hasOwnProperty.call(byInstance, section['.name']) ?
							byInstance[section['.name']] : unavailable[index];
					}),
					diagnostics: diagnostics.concat(result.issues.filter(function(issue) {
						return !Object.prototype.hasOwnProperty.call(configured, issue.instance);
					}).map(function(issue) {
						return { instance: issue.instance, message: issue.message };
					}))
				};
			}),
			{ rows: unavailable, diagnostics: [] }
		);
	}

	return Promise.all(sections.map(function(section, index) {
		var sectionSource = schedulerStatusSource(section['.name'], schedulerEngine,
			calibrationSummary);
		return L.resolveDefault(
			fs.exec(sectionSource.command, sectionSource.args).then(parseExecJson).then(function(result) {
				if (!legacySchedulerStatusValidated(result, section['.name']))
					throw new Error(_('Legacy scheduler returned an invalid status contract.'));
				return normalizeLegacySchedulerStatus(result);
			}),
			unavailable[index]
		);
	})).then(function(rows) { return { rows: rows, diagnostics: [] }; });
}

function readInstanceStatuses(sections, schedulerEngine, calibrationSummary) {
	return Promise.all([
		Promise.all(sections.map(function(section) { return readStatus(section['.name']); })),
		readSchedulerStatuses(sections, schedulerEngine, calibrationSummary)
	]).then(function(result) {
		return {
			rows: sections.map(function(section, index) {
				var status = Object.assign({}, result[0][index] || {});
				status.scheduled_autotune = result[1].rows[index];
				return status;
			}),
			diagnostics: result[1].diagnostics
		};
	});
}

var CALIBRATION_DAEMON = '/usr/sbin/cake-autorated';

function calibrationExec(args) {
	return fs.exec(CALIBRATION_DAEMON, [ '--calibrationctl' ].concat(args))
		.then(parseExecJson);
}

function readCalibrationSummary() {
	return L.resolveDefault(calibrationExec([ 'summary' ]), {
		state: 'unavailable', native_rating: false, native_scheduler: null
	});
}

function nativeRatingRoute(section) {
	var configured = String(section.route_mode || 'auto');
	var member = String(section.mwan3_member || '');
	var mode = configured === 'auto' ? (member ? 'mwan3' : 'main') : configured;

	return { mode: mode, member: mode === 'mwan3' ? member : '' };
}

function nativeRatingStartArgs(section, mode) {
	var route = nativeRatingRoute(section);
	var target = section.sqm_interface || section.ul_if || section.wan_if || '';
	var automatic = mode === 'automatic';
	var args = [
		'rating-start',
		'--instance', section['.name'],
		'--expected-target', target,
		'--mode', automatic ? 'automatic' : 'client',
		'--backend', automatic ? 'speedtest-go' : 'client',
		'--route-mode', route.mode
	];

	if (route.mode === 'mwan3')
		args.push('--mwan3-member', route.member);
	return args;
}

function nativeRatingProgress(status, job) {
	status = status || {};
	return Object.assign({}, job || {}, {
		phase: status.rating_load_phase || 'IDLE',
		requested_phase: status.rating_capture_requested_phase || 'AUTO',
		message: status.quality_grade_state || (job && job.diagnostic) || _('Collecting rating samples.'),
		baseline_samples: Number(status.quality_grade_baseline_samples || 0),
		baseline_required: Number(status.quality_grade_baseline_required_samples || 20),
		dl_samples: Number(status.quality_grade_dl_samples || 0),
		ul_samples: Number(status.quality_grade_ul_samples || 0),
		required_samples: Number(status.quality_grade_required_samples || 20),
		finalize_remaining_s: status.quality_grade_finalize_remaining_s,
		smoothed_dl_percent: Number(status.rating_load_smoothed_dl_percent || 0),
		smoothed_ul_percent: Number(status.rating_load_smoothed_ul_percent || 0),
		effective_dl_kbps: Number(status.rating_load_effective_dl_kbps || 0),
		effective_ul_kbps: Number(status.rating_load_effective_ul_kbps || 0),
		reference_dl_kbps: Number(status.rating_load_reference_dl_kbps || 0),
		reference_ul_kbps: Number(status.rating_load_reference_ul_kbps || 0),
		enter_dl_kbps: Number(status.rating_load_enter_dl_kbps || 0),
		enter_ul_kbps: Number(status.rating_load_enter_ul_kbps || 0),
		background_dl_kbps: Number(status.rating_capture_background_dl_kbps || 0),
		background_ul_kbps: Number(status.rating_capture_background_ul_kbps || 0),
		contaminated: status.rating_capture_contaminated === true,
		contamination_reason: status.rating_capture_contamination_reason || '',
		last_rejected_reason: status.transport_probe_last_rejected_reason || ''
	});
}

function qualityTestDelay() {
	return new Promise(function(resolve) {
		window.setTimeout(resolve, 1000);
	});
}

function qualityReadiness(section, status, mode, calibrationSummary) {
	var uplinkState = String(status && status.uplink_state || '').toUpperCase();
	var testMode = mode || 'automatic';
	var routeTestReady;

	if (calibrationSummary && calibrationSummary.native_rating !== true)
		return {
			ready: false,
			reason: _('Native Rating is unavailable in the installed daemon. Upgrade both the daemon and LuCI package, then restart the calibration service.')
		};
	if (String(section.enabled || '0') !== '1')
		return { ready: false, reason: _('Autorate instance is disabled.') };
	if (String(section.sqm_enabled || '0') !== '1')
		return { ready: false, reason: _('Managed SQM must be enabled.') };
	if (status && status.sqm_runtime_managed && !status.sqm_runtime_healthy)
		return {
			ready: false,
			reason: _('Managed SQM runtime is unhealthy: %s').format(status.sqm_runtime_reason || '-')
		};
	if (!status || !status.transport_latency_enabled)
		return { ready: false, reason: _('Transport-aware latency must be enabled.') };
	if (uplinkState === 'OFFLINE')
		return { ready: false, reason: _('The selected uplink is offline.') };
	if (uplinkState === 'LEARNING')
		return { ready: false, reason: _('The selected uplink is still learning its idle baseline.') };
	if (uplinkState === 'RECHECKING')
		return { ready: false, reason: _('The selected uplink route is being rechecked after a temporary routing error.') };
	if (testMode === 'client' && !status.route_active)
		return {
			ready: false,
			reason: _('Guided client mode requires client traffic to be routed through this uplink. It is currently online but not default-active; add or select an mwan3 client rule, or use Automatic router-side test.')
		};
	routeTestReady = status.route_test_ready;
	if (routeTestReady == null)
		routeTestReady = uplinkState === 'ACTIVE' || uplinkState === 'STANDBY' || status.route_active;
	if (testMode === 'automatic' && !routeTestReady)
		return { ready: false, reason: _('The selected uplink cannot currently prove an isolated test route.') };
	if (!status.transport_probe_trusted)
		return { ready: false, reason: _('A trusted native transport backend is required.') };
	if (!status.quality_grade_baseline_ready)
		return {
			ready: false,
			reason: _('Idle baseline: %d / %d samples.').format(
				Number(status.quality_grade_baseline_samples || 0),
				Number(status.quality_grade_baseline_required_samples || 20))
		};
	if (testMode === 'automatic' && uplinkState === 'STANDBY')
		return { ready: true, reason: _('Ready through the isolated mwan3 member. The uplink is online but not default-active; SQM and autorate remain enabled.') };
	return { ready: true, reason: _('Ready. SQM and autorate remain enabled during this test.') };
}

function qualityProgressText(job) {
	var required = Number(job.required_samples || 20);
	var baselineRequired = Number(job.baseline_required || 20);
	var parts = [
		_('Baseline %d/%d').format(Number(job.baseline_samples || 0), baselineRequired),
		_('DL %d/%d').format(Number(job.dl_samples || 0), required),
		_('UL %d/%d').format(Number(job.ul_samples || 0), required),
		_('Phase %s').format(job.phase || 'IDLE'),
		_('Requested %s').format(job.requested_phase || 'AUTO'),
		_('Load DL %s / UL %s').format(
			formatPercent(job.smoothed_dl_percent),
			formatPercent(job.smoothed_ul_percent)),
		_('Effective DL %s / UL %s').format(
			formatRate(job.effective_dl_kbps),
			formatRate(job.effective_ul_kbps)),
		_('Trigger DL %s / UL %s').format(
			formatRate(job.enter_dl_kbps),
			formatRate(job.enter_ul_kbps)),
		_('CAKE reference DL %s / UL %s').format(
			formatRate(job.reference_dl_kbps),
			formatRate(job.reference_ul_kbps)),
		_('Background DL %s / UL %s').format(
			formatRate(job.background_dl_kbps),
			formatRate(job.background_ul_kbps))
	];

	if (job.finalize_remaining_s != null)
		parts.push(_('Finalize in about %d s').format(Math.ceil(Number(job.finalize_remaining_s || 0))));
	if (job.last_rejected_reason)
		parts.push(_('Last rejected: %s').format(job.last_rejected_reason));
	if (job.contaminated)
		parts.push(_('CONTAMINATED: %s').format(job.contamination_reason || _('unexpected opposite-direction traffic')));
	return parts.join(' · ');
}

function showQualityTest(section, status, calibrationSummary) {
	var instance = section['.name'];
	var readiness = qualityReadiness(section, status, 'automatic', calibrationSummary);
	var mode = E('select', { 'class': 'cbi-input-select' }, [
		E('option', { 'value': 'automatic' }, _('Automatic router-side test')),
		E('option', { 'value': 'client' }, _('Guided client capture'))
	]);
	var state = E('div', { 'class': 'alert-message notice cake-quality-job-state' }, readiness.reason);
	var detail = E('div', { 'class': 'cake-quality-job-detail' },
		_('Automatic mode first waits for a quiet link, measures background traffic, then runs explicit download-only and upload-only phases through this uplink. Unexpected opposite-direction traffic rejects a contaminated phase. It may take 1–3 passes and transfer several gigabytes on a fast line. Guided mode uses independent download and upload triggers while you run a sequential test from a LAN client. Triggers are percentages of the current CAKE rates, not the physical link or adaptive ceiling caps. Neither mode disables SQM or autorate, changes CAKE limits, or writes samples to flash.'));
	var running = false;
	var starting = false;
	var closed = false;
	var readinessPolling = false;
	var jobId = null;
	var startButton;
	var closeButton;

	function setState(text, error) {
		state.className = 'alert-message ' + (error ? 'error' : 'notice') + ' cake-quality-job-state';
		state.textContent = text;
	}

	function finish(job) {
		running = false;
		starting = false;
		startButton.disabled = !readiness.ready;
		mode.disabled = false;
		closeButton.textContent = _('Close');
		if (job.state === 'complete') {
			setState(_('Rating %s complete: +%s ms · DL %s · UL %s. Limits were not changed.').format(
				job.grade || '-', Number(job.increase_ms || 0).toFixed(1),
				job.dl_grade || '-', job.ul_grade || '-'), false);
		} else if (job.state === 'cancelled') {
			setState(_('Rating capture cancelled.'), false);
		} else {
			setState(job.error || job.message || _('Rating capture did not complete.'), true);
		}
	}

	function refreshReadiness(announce) {
		return Promise.all([ readStatus(instance), readCalibrationSummary() ]).then(function(result) {
			var freshStatus = result[0];
			calibrationSummary = result[1];
			if (!freshStatus) {
				readiness = {
					ready: false,
					reason: _('Waiting for fresh runtime status from the controller.')
				};
				if (startButton)
					startButton.disabled = true;
				if (announce && !running && !starting && !closed)
					setState(readiness.reason, false);
				return readiness;
			}
			readiness = qualityReadiness(section, freshStatus, mode.value, calibrationSummary);
			if (!running && !starting && startButton)
				startButton.disabled = !readiness.ready;
			if (announce && !running && !starting && !closed)
				setState(readiness.reason, false);
			return readiness;
		}).catch(function(error) {
			readiness = {
				ready: false,
				reason: _('Waiting for fresh runtime status: %s').format(error.message || String(error))
			};
			if (startButton)
				startButton.disabled = true;
			if (announce && !running && !starting)
				setState(readiness.reason, false);
			return readiness;
		});
	}

	function pollReadiness() {
		if (closed || running || starting) {
			readinessPolling = false;
			return Promise.resolve();
		}
		return qualityTestDelay().then(function() {
			return refreshReadiness(true);
		}).then(pollReadiness);
	}

	function ensureReadinessPoll() {
		if (!readinessPolling && !closed && !running && !starting) {
			readinessPolling = true;
			pollReadiness();
		}
	}

	function pollJob() {
		if (!running || closed || !jobId)
			return Promise.resolve();
		return qualityTestDelay().then(function() {
			return calibrationExec([ 'rating-status', jobId ]);
		}).then(function(job) {
			if (job.state === 'completed')
				return calibrationExec([ 'rating-result', jobId ]).then(finish);
			if (job.state === 'queued' || job.state === 'starting' || job.state === 'running' ||
			    job.state === 'cancelling' || job.state === 'recovering')
				return readStatus(instance).then(function(freshStatus) {
					var progress = nativeRatingProgress(freshStatus, job);
					setState((progress.message || _('Collecting rating samples.')) + '\n' +
						qualityProgressText(progress), false);
					return pollJob();
				});
			finish({
				state: job.state,
				error: job.diagnostic || job.error || _('Rating capture did not complete.')
			});
		}).catch(function(error) {
			finish({ state: 'error', error: error.message || String(error) });
		});
	}

	function start() {
		if (running || starting)
			return Promise.resolve();
		starting = true;
		startButton.disabled = true;
		return refreshReadiness(false).then(function(freshReadiness) {
			if (closed) {
				starting = false;
				return;
			}
			if (!freshReadiness.ready) {
				starting = false;
				setState(freshReadiness.reason, false);
				ensureReadinessPoll();
				return;
			}
			starting = false;
			running = true;
			mode.disabled = true;
			closeButton.textContent = _('Cancel');
			setState(_('Starting rating capture…'), false);
			return calibrationExec(nativeRatingStartArgs(section, mode.value)).then(function(job) {
				if (job.state === 'error' || job.error)
					throw new Error(job.error || job.error_code || _('Unable to start native Rating.'));
				jobId = job.job_id;
				if (!jobId)
					throw new Error(_('Native Rating returned no job ID.'));
				return pollJob();
			});
		}).catch(function(error) {
			if (!closed)
				finish({ state: 'error', error: error.message || String(error) });
		});
	}

	function close() {
		closed = true;
		starting = false;
		if (running && jobId)
			return calibrationExec([ 'rating-cancel', jobId ]).catch(function() {}).then(function() {
				ui.hideModal();
			});
		ui.hideModal();
		return Promise.resolve();
	}

	startButton = E('button', {
		'type': 'button',
		'class': 'btn cbi-button cbi-button-action',
		'disabled': readiness.ready ? null : '',
		'click': ui.createHandlerFn(null, start)
	}, _('Start rating'));
	closeButton = E('button', {
		'type': 'button',
		'class': 'btn cbi-button cbi-button-neutral',
		'click': ui.createHandlerFn(null, close)
	}, _('Close'));
	mode.addEventListener('change', function() {
		if (!running && !starting && !closed)
			refreshReadiness(true);
	});

	ui.showModal(_('Get rating — %s').format(instance), [
		E('div', { 'class': 'cbi-section' }, [
			E('label', { 'class': 'cbi-value' }, [
				E('span', { 'class': 'cbi-value-title' }, _('Test mode')),
				E('span', { 'class': 'cbi-value-field' }, mode)
			]),
			detail,
			state
		]),
		E('div', { 'class': 'right' }, [ startButton, ' ', closeButton ])
	]);

	calibrationExec([ 'rating-current', instance ]).then(function(job) {
		if (job.operation === 'automatic_rating')
			mode.value = 'automatic';
		else if (job.operation === 'guided_rating')
			mode.value = 'client';
		if (job.state === 'completed' && job.job_id && !closed) {
			jobId = job.job_id;
			return calibrationExec([ 'rating-result', jobId ]).then(finish);
		}
		if ((job.state === 'queued' || job.state === 'starting' || job.state === 'running' ||
		     job.state === 'cancelling' || job.state === 'recovering') && !closed) {
			jobId = job.job_id;
			running = true;
			startButton.disabled = true;
			mode.disabled = true;
			closeButton.textContent = _('Cancel');
			return readStatus(instance).then(function(freshStatus) {
				var progress = nativeRatingProgress(freshStatus, job);
				setState((progress.message || _('Collecting rating samples.')) + '\n' +
					qualityProgressText(progress), false);
				return pollJob();
			});
		} else if (!closed) {
			return refreshReadiness(true).then(ensureReadinessPoll);
		}
	}).catch(function() {
		if (!closed)
			return refreshReadiness(true).then(ensureReadinessPoll);
	});
}

function renderVersions(versions) {
	return E('div', { 'class': 'alert-message notice cake-package-versions' }, [
		E('strong', {}, _('Installed versions: ')),
		_('daemon %s · LuCI %s').format(
			versions['cake-autorate-rs'] || '-',
			versions['luci-app-cake-autorate-rs'] || '-')
	]);
}

function serviceAction(action) {
	var mqttAction = function() {
		if (action === 'start' || action === 'restart')
			return L.resolveDefault(fs.exec('/etc/init.d/cake-autorate-mqtt', [ 'enable' ]), null)
				.then(function() {
					return L.resolveDefault(fs.exec('/etc/init.d/cake-autorate-mqtt', [ action ]), null);
				});

		if (action === 'stop')
			return L.resolveDefault(fs.exec('/etc/init.d/cake-autorate-mqtt', [ action ]), null);

		return Promise.resolve();
	};

	return fs.exec('/etc/init.d/cake-autorate', [ action ]).then(mqttAction).then(function() {
		ui.addNotification(null, E('p', _('Service action completed.')));
	});
}

function downloadText(filename, text) {
	var blob = new Blob([ text ], { type: 'text/plain;charset=utf-8' });
	var url = URL.createObjectURL(blob);
	var link = document.createElement('a');

	link.href = url;
	link.download = filename;
	document.body.appendChild(link);
	link.click();
	document.body.removeChild(link);
	window.setTimeout(function() {
		URL.revokeObjectURL(url);
	}, 1000);
}

function exportLogs(ev) {
	var button = ev.currentTarget;

	button.disabled = true;

	return fs.exec('/usr/libexec/cake-autorate-rs/log-bundle', [ 'all' ]).then(function(res) {
		var stdout = res && res.stdout ? res.stdout : '';
		var stamp = new Date().toISOString().replace(/[:.]/g, '-');

		if (!stdout)
			throw new Error(_('Log bundle helper returned no data.'));

		downloadText('cake-autorate-rs-log-bundle-' + stamp + '.txt', stdout);
		ui.addNotification(null, E('p', _('Log bundle exported.')));
	}).catch(function(err) {
		ui.addNotification(null, E('p', _('Log bundle export failed: %s').format(err.message || err)), 'error');
	}).then(function() {
		button.disabled = false;
	});
}

function formatRate(value) {
	value = Number(value || 0);
	return value.toFixed(0) + ' kbps';
}

function formatBytes(value) {
	var units = [ 'B', 'KiB', 'MiB', 'GiB', 'TiB' ];
	var unit = 0;

	value = Number(value || 0);
	if (!isFinite(value) || value < 0)
		return '-';
	while (value >= 1024 && unit < units.length - 1) {
		value /= 1024;
		unit++;
	}
	return (unit === 0 ? value.toFixed(0) : value.toFixed(value >= 10 ? 1 : 2)) + ' ' + units[unit];
}

function formatShaperRate(status, direction) {
	var capacityRoot = status.adaptive_capacity || null;
	var capacity = capacityRoot && capacityRoot[direction === 'dl' ? 'download' : 'upload'];
	var rateKey = direction === 'dl' ? 'cake_dl_rate_kbps' : 'cake_ul_rate_kbps';
	var configuredKey = direction === 'dl' ? 'configured_max_dl_shaper_rate_kbps' : 'configured_max_ul_shaper_rate_kbps';
	var effectiveKey = direction === 'dl' ? 'effective_max_dl_shaper_rate_kbps' : 'effective_max_ul_shaper_rate_kbps';
	var capKey = direction === 'dl' ? 'adaptive_ceiling_dl_cap_kbps' : 'adaptive_ceiling_ul_cap_kbps';
	var phaseKey = 'adaptive_ceiling_' + direction + '_phase';
	var safeKey = direction === 'dl' ? 'adaptive_ceiling_safe_dl_kbps' : 'adaptive_ceiling_safe_ul_kbps';
	var failedKey = direction === 'dl' ? 'adaptive_ceiling_failed_dl_kbps' : 'adaptive_ceiling_failed_ul_kbps';
	var probeKey = direction === 'dl' ? 'adaptive_ceiling_probe_dl_kbps' : 'adaptive_ceiling_probe_ul_kbps';
	var reasonKey = 'adaptive_ceiling_' + direction + '_last_reason';
	var rate = formatRate(capacity && capacity.current_rate_kbps != null ?
		capacity.current_rate_kbps : status[rateKey]);
	var phase, failed, probe, safe, detail, title, confidence;

	if (!(capacityRoot ? capacityRoot.enabled : status.adaptive_ceiling_enabled))
		return rate;

	phase = String(capacity && capacity.phase || status[phaseKey] || 'cruise').replace(/_/g, ' ');
	failed = (capacity ? capacity.failed_bound_kbps : status[failedKey]) == null ? '-' :
		formatRate(capacity ? capacity.failed_bound_kbps : status[failedKey]);
	probe = (capacity ? capacity.probe_target_kbps : status[probeKey]) == null ? '-' :
		formatRate(capacity ? capacity.probe_target_kbps : status[probeKey]);
	safe = formatRate(capacity && capacity.safe_ceiling_kbps != null ?
		capacity.safe_ceiling_kbps : status[safeKey]);
	confidence = Number(capacity && capacity.confidence && capacity.confidence.percent || 0);
	detail = _('Phase: %s · safe: %s · confidence: %d%%').format(phase, safe, confidence);
	title = [
		_('Configured max: %s').format(formatRate(status[configuredKey])),
		_('Effective ceiling: %s').format(formatRate(status[effectiveKey])),
		_('Absolute cap: %s').format(formatRate(status[capKey])),
		_('Runtime minimum: %s').format(capacity && capacity.runtime_minimum_kbps != null ?
			formatRate(capacity.runtime_minimum_kbps) : '-'),
		_('Failed bound: %s').format(failed),
		_('Probe target: %s').format(probe),
		_('Causal result: %s').format(capacity && capacity.causal_state || '-'),
		_('No CAKE effect: %s').format(capacity && capacity.no_cake_effect === true ? _('yes') :
			(capacity && capacity.no_cake_effect === false ? _('no') : '-')),
		_('Route epoch: %s').format(capacityRoot && capacityRoot.route_epoch || '-'),
		_('Last transition: %s').format(status[reasonKey] || '-')
	].join('\n');

	return E('div', {
		'title': title
	}, [
		E('div', {}, rate),
		E('small', { 'style': 'display:block;white-space:normal;overflow-wrap:anywhere' },
			detail)
	]);
}

function formatPercent(value) {
	if (value == null)
		return '-';

	value = Number(value);
	return isNaN(value) ? '-' : value.toFixed(1) + '%';
}

function qualityGradeClass(grade) {
	return 'cake-quality-grade-' + String(grade || 'unknown').toLowerCase().replace('+', '-plus');
}

function qualityDirectionSummary(result) {
	var values = [];

	if (result && result.dl)
		values.push(_('DL %s').format(result.dl.grade || '-'));
	if (result && result.ul)
		values.push(_('UL %s').format(result.ul.grade || '-'));
	if (result && result.bidirectional)
		values.push(_('Bidi +%s ms').format(Number(result.bidirectional.increase_ms || 0).toFixed(1)));

	return values.length ? values.join(' · ') : '-';
}

function qualityAge(result) {
	var timestamp = Number(result && (result.completed_at || result.started_at) || 0);
	var seconds;

	if (!isFinite(timestamp) || timestamp <= 0)
		return '-';
	seconds = Math.max(0, Math.round(Date.now() / 1000 - timestamp));
	if (seconds < 60)
		return _('%d s ago').format(seconds);
	if (seconds < 3600)
		return _('%d min ago').format(Math.round(seconds / 60));
	if (seconds < 86400)
		return _('%d h ago').format(Math.round(seconds / 3600));
	return _('%d d ago').format(Math.round(seconds / 86400));
}

function renderDetectedGrade(label, result, state, collected, required, dlSamples, ulSamples) {
	var value, detail, classes = 'cake-quality-detected';

	if (!result) {
		if (state === 'none') {
			value = '-';
			detail = _('No complete rating known yet');
		} else if (state === 'collecting') {
			value = _('COLLECTING');
			detail = _('DL %d/%d · UL %d/%d').format(
				Number(dlSamples || 0), Number(required || 0),
				Number(ulSamples || 0), Number(required || 0));
		} else if (state === 'baseline_ready') {
			value = _('WAITING FOR DATA');
			detail = _('Run Get rating or generate new loaded traffic');
		} else {
			value = _('LEARNING');
			detail = _('Collecting idle baseline');
		}
	} else {
		value = result.incomplete ?
			(result.completed_at == null ? _('COLLECTING') : _('INCOMPLETE')) :
			(result.partial ? _('PARTIAL') : (result.grade || '-'));
		detail = _('+%s ms · %s · %s').format(
			Number(result.increase_ms || 0).toFixed(1),
			qualityDirectionSummary(result),
			qualityAge(result));
		if (!result.partial && !result.incomplete)
			classes += ' ' + qualityGradeClass(value);
		if (result.stale)
			classes += ' cake-quality-stale';
	}

	return E('div', { 'class': classes }, [
		E('span', { 'class': 'cake-quality-label' }, label),
		E('strong', {}, value),
		E('small', {}, detail + (result && result.partial ? ' · ' + _('partial') : '') +
			(result && result.incomplete ? ' · ' + _('incomplete') : '') +
			(result && result.stale ? ' · ' + _('STALE') : ''))
	]);
}

function formatQuality(status) {
	if (!status || !status.transport_latency_enabled)
		return E('span', { 'title': _('Transport-aware estimation is disabled.') }, '-');

	if (status.quality_grade_state) {
		var current = status.quality_grade_current || null;
		var lastKnown = status.quality_grade_last_known || null;
		if (lastKnown && (lastKnown.partial || lastKnown.incomplete))
			lastKnown = null;
		var state = String(status.quality_grade_state || 'learning_baseline');
		var title = [
			_('Detected rating uses network RTT loaded p90 minus the preceding idle p5. DNS, process startup, and connection handshake time are excluded.'),
			_('Download and upload are scored independently; the worse grade is shown.'),
			_('A one-direction result is labeled PARTIAL and is never presented as the final connection rating.'),
			_('Bidirectional latency is diagnostic and does not affect the total grade.'),
			_('Load detector: %s · requested phase: %s').format(
				status.rating_load_phase || 'IDLE',
				status.rating_capture_requested_phase || 'AUTO'),
			_('Current CAKE reference: DL %s · UL %s').format(
				formatRate(status.rating_load_reference_dl_kbps),
				formatRate(status.rating_load_reference_ul_kbps)),
			_('Current triggers: DL %s (%s) · UL %s (%s)').format(
				formatRate(status.rating_load_enter_dl_kbps),
				formatPercent(status.rating_load_enter_dl_percent),
				formatRate(status.rating_load_enter_ul_kbps),
				formatPercent(status.rating_load_enter_ul_percent)),
			_('Aggregate traffic: DL %s · UL %s; effective after background subtraction: DL %s · UL %s').format(
				formatRate(status.rating_load_aggregate_dl_kbps),
				formatRate(status.rating_load_aggregate_ul_kbps),
				formatRate(status.rating_load_effective_dl_kbps),
				formatRate(status.rating_load_effective_ul_kbps)),
			_('Capture background: DL %s · UL %s; contaminated: %s (%s)').format(
				formatRate(status.rating_capture_background_dl_kbps),
				formatRate(status.rating_capture_background_ul_kbps),
				status.rating_capture_contaminated ? _('yes') : _('no'),
				status.rating_capture_contamination_reason || '-'),
			_('Backend: %s · trusted: %s · reused connection: %s').format(
				status.transport_probe_backend || '-',
				status.transport_probe_trusted ? _('yes') : _('no'),
				status.transport_probe_connection_reused ? _('yes') : _('no')),
			_('Accepted raw samples: %d · discarded: %d · confidence: %d%%').format(
				Number(status.transport_probe_raw_samples || 0),
				Number(status.transport_probe_discarded_samples || 0),
				Number(status.transport_confidence || 0)),
			_('Controller: %s · signal: %s · effective delta: %s ms').format(
				status.transport_controller_enabled ? _('enabled') : _('disabled'),
				status.quality_class || 'LEARNING',
				status.effective_latency_delta_ms == null ? '-' : Number(status.effective_latency_delta_ms).toFixed(1)),
			_('Rejected sample: %s').format(status.transport_probe_rejected_reason || '-'),
			_('Last rejected sample: %s').format(status.transport_probe_last_rejected_reason || '-'),
			_('Transport error code: %s').format(status.transport_error_code || '-'),
			_('Safe floors: DL %s · UL %s').format(formatRate(status.throughput_floor_dl_kbps), formatRate(status.throughput_floor_ul_kbps))
		].join('\n');

		return E('div', { 'class': 'cake-quality-stack', 'title': title }, [
			renderDetectedGrade(_('CURRENT'), current, state,
				status.quality_grade_collected_samples, status.quality_grade_required_samples,
				status.quality_grade_dl_samples, status.quality_grade_ul_samples),
			renderDetectedGrade(_('LAST KNOWN'), lastKnown, lastKnown ? 'final' : 'none', 0, 0, 0, 0)
		]);
	}

	var value = status.quality_class || 'LEARNING';
	var confidence = Number(status.quality_confidence || 0);
	var limited = !!status.quality_limited;
	var baselineReady = status.transport_status === 'baseline_ready';
	if (baselineReady)
		value = _('BASELINE READY');
	var title = [
		_('Estimated from ICMP and HTTP/TCP latency; this is not an external benchmark grade.'),
		_('DL: %s · UL: %s').format(status.quality_dl_class || 'LEARNING', status.quality_ul_class || 'LEARNING'),
		_('Confidence: %d%%').format(confidence),
		_('Transport delta: %s ms').format(status.transport_delta_ms == null ? '-' : Number(status.transport_delta_ms).toFixed(1)),
		_('Effective delta: %s ms').format(status.effective_latency_delta_ms == null ? '-' : Number(status.effective_latency_delta_ms).toFixed(1)),
		_('Transport error code: %s').format(status.transport_error_code || '-'),
		_('Reason: %s').format(status.quality_reason || '-'),
		_('Safe floors: DL %s · UL %s').format(formatRate(status.throughput_floor_dl_kbps), formatRate(status.throughput_floor_ul_kbps))
	].join('\n');

	return E('div', { 'title': title }, [
		E('strong', { 'style': limited ? 'color:#d66' : '' }, value),
		E('small', { 'style': 'display:block;white-space:nowrap' },
			limited ? _('Estimated · safety floor') :
				(baselineReady ? _('Waiting for loaded traffic · %d%%').format(confidence) : _('Estimated · %d%%').format(confidence)))
	]);
}

function hasProbeSample(status) {
	if (status.reflector)
		return true;

	return Array.isArray(status.reflector_health) && status.reflector_health.some(function(item) {
		return Number(item && item.samples || 0) > 0;
	});
}

function sqmRuntimePending(status) {
	var state = String(status && (status.sqm_runtime_state || status.state) || '').toUpperCase();

	return state === 'WAITING_LINK' || state === 'WAITING_SQM' ||
		state === 'WAITING_EXTERNAL_SQM' || state === 'WAITING_OPERATION' ||
		state === 'RECOVERING';
}

function probeWarning(status, enabled) {
	var started, runtime;

	if (!enabled || !status || !status.state || hasProbeSample(status))
		return null;
	if (sqmRuntimePending(status))
		return null;
	if (status.uplink_state === 'OFFLINE' || status.uplink_state === 'LEARNING' ||
	    status.uplink_state === 'RECHECKING')
		return null;

	started = Number(status.started_at || 0);
	if (!isFinite(started) || started <= 0)
		return null;

	runtime = Date.now() / 1000 - started;
	if (!isFinite(runtime) || runtime < 10)
		return null;

	return _('No probe replies. Check the pinger and multi-WAN policy routing.');
}

function trafficProfileLabel(value) {
	switch (value) {
	case 'gaming':
	case 'gaming-extreme':
	case 'extreme_gaming':
	case 'gaming_extreme': return _('Gaming');
	case 'best-overall':
	case 'balanced':
	case 'best_overall': return _('Best overall');
	case 'variable':
	case 'variable-link':
	case 'variable_link': return _('Variable link');
	case 'fair': return _('Fair');
	case 'custom': return _('Custom');
	default: return _('Unknown');
	}
}

function accessMediumLabel(value) {
	switch (value) {
	case 'cellular': return _('4G / 5G cellular');
	case 'leo_satellite': return _('LEO satellite');
	case 'geo_satellite': return _('GEO satellite');
	case 'fixed_wireless': return _('Fixed wireless / WISP');
	case 'shared_wired': return _('Shared wired');
	default: return _('Unknown');
	}
}

function capacityLearningLabel(sectionData, health) {
	var policy = health && health.capacity_learning_policy ||
		sectionData && sectionData.capacity_learning_policy;
	switch (policy) {
	case 'verified_only': return _('Validated ceiling only');
	case 'passive_bounded': return _('Bounded passive learning');
	case 'scheduled_active': return _('Passive + scheduled active');
	case 'fixed_cap': return _('Explicit service caps');
	}
	if (sectionData && sectionData.scheduled_autotune_enabled === '1')
		return _('Passive + scheduled active');
	if (sectionData && sectionData.adaptive_ceiling_enabled === '1')
		return _('Passive only');
	return _('Configured bounds');
}

function formatState(status, enabled, sectionData, health) {
	var value = status && (status.uplink_state || status.state);
	var warning, autotuneProfile, priorities, priorityTitle, learningMode, lines, schedule;

	autotuneProfile = trafficProfileLabel(
		health && health.autotune_profile || sectionData && sectionData.autotune_profile);
	if (!sectionData || sectionData.traffic_rules_enabled !== '1') {
		priorities = _('Off');
	} else {
		var sectionAutotune = (function(value) {
			switch (value) {
			case 'gaming':
			case 'gaming-extreme':
			case 'extreme_gaming':
			case 'gaming_extreme': return 'gaming';
			case 'fair': return 'fair';
			default: return 'best_overall';
			}
		})(sectionData.autotune_profile);
		var mode = health && health.traffic_profile_mode || sectionData.traffic_profile ||
			(sectionData['traffic_defaults_' + sectionAutotune] === '0' ? 'custom' : 'auto');
		var resolved = health && health.traffic_profile_resolved ||
			(mode === 'auto' ?
				(sectionData.autotune_profile === 'variable_link' ? 'best_overall' :
					sectionData.autotune_profile) : mode);
		priorities = trafficProfileLabel(resolved);
		if (mode === 'auto')
			priorities += ' · ' + _('linked');
		else if (mode !== 'custom')
			priorities += ' · ' + _('manual');
		priorityTitle = health && health.classifier_state ?
			_('Runtime classifier: %s').format(health.classifier_state) : '';
	}
	value = value ? String(value).toUpperCase() : (enabled ? '-' : _('DISABLED'));
	learningMode = capacityLearningLabel(sectionData, health);
	lines = [
		E('strong', {}, value),
		E('small', { 'style': 'display:block;white-space:nowrap' },
			_('Controller: %s').format(String(status && status.state || (enabled ? '-' : 'disabled')).toUpperCase())),
		E('small', { 'style': 'display:block;white-space:nowrap' },
			_('Auto-Tune: %s').format(autotuneProfile)),
		E('small', { 'style': 'display:block;white-space:normal' },
			_('Learning: %s').format(learningMode)),
		E('small', { 'style': 'display:block;white-space:nowrap', 'title': priorityTitle || '' },
			_('Priorities: %s').format(priorities))
	];
	if (sectionData && sectionData.autotune_profile === 'variable_link') {
		var accessMedium = health && health.access_medium || sectionData.access_medium;
		var accessSource = health && health.access_medium_source ||
			sectionData.access_medium_source || 'legacy_default';
		var accessConfidence = Number(health && health.access_medium_confidence_percent != null ?
			health.access_medium_confidence_percent :
			(sectionData.access_medium_confidence_percent || 0));
		if (!isFinite(accessConfidence) || accessConfidence < 0 || accessConfidence > 100)
			accessConfidence = 0;
		lines.splice(4, 0, E('small', {
			'style': 'display:block;white-space:normal',
			'title': _('Evidence source: %s').format(accessSource)
		}, _('Access: %s · %d%% confidence').format(
			accessMediumLabel(accessMedium), accessConfidence)));
	}
	schedule = status && status.scheduled_autotune;
	var scheduleVisible = schedule && (schedule.enabled ||
		(sectionData && sectionData.scheduled_autotune_enabled === '1'));
	if (scheduleVisible && schedule.available === false) {
		lines.push(E('small', {
			'style': 'display:block;color:#f66;white-space:normal',
			'class': 'cake-schedule-error'
		}, schedule.message || _('Scheduled Auto-Tune status is unavailable.')));
	}
	else if (scheduleVisible && schedule.accounting_error) {
		lines.push(E('small', {
			'style': 'display:block;color:#f66;white-space:normal',
			'class': 'cake-schedule-error'
		}, schedule.message || _('Scheduled traffic accounting is blocked until it is repaired.')));
	}
	else if (scheduleVisible && schedule.state === 'error') {
		lines.push(E('small', {
			'style': 'display:block;color:#f66;white-space:normal',
			'class': 'cake-schedule-error'
		}, schedule.message || _('Native scheduler status is invalid for this instance.')));
	}
	else if (scheduleVisible && schedule.budget_authoritative === false) {
		lines.push(E('small', {
			'style': 'display:block;color:#d08b20;white-space:normal',
			'class': 'cake-schedule-initializing'
		}, schedule.message || _('Scheduled traffic accounting is initializing.')));
	}
	else if (scheduleVisible) {
		var dailyRemaining = Number(schedule.daily && schedule.daily.remaining_bytes || 0);
		var monthlyRemaining = Number(schedule.monthly && schedule.monthly.remaining_bytes || 0);
		var nextDue = Number(schedule.next_due_at || 0);
		var scheduleText = _('Active budget: %s today · %s this month').format(
			formatBytes(dailyRemaining), formatBytes(monthlyRemaining));

		if (nextDue > 0)
			scheduleText += ' · ' + _('due %s').format(new Date(nextDue * 1000).toLocaleString());
		lines.push(E('small', {
			'style': 'display:block;white-space:normal',
			'class': '',
			'title': schedule.message || ''
		}, scheduleText));
		if (schedule.warning) {
			lines.push(E('small', {
				'style': 'display:block;color:#d08b20;white-space:normal',
				'class': 'cake-schedule-warning'
			}, schedule.warning));
		}
	}
	if (status && status.sqm_runtime_managed && !status.sqm_runtime_healthy) {
		var pending = sqmRuntimePending(status);
		var runtimeState = String(status.sqm_runtime_state || '').toUpperCase();
		var runtimeLabel = pending ? _('WAITING') : _('ERROR');
		var runtimeDetail = pending ? _('Waiting for link/SQM') : _('CAKE/IFB unavailable');

		switch (runtimeState) {
		case 'WAITING_LINK':
			runtimeDetail = _('WAN link unavailable · automatic recovery armed');
			break;
		case 'WAITING_SQM':
			runtimeDetail = _('Waiting for SQM hotplug to settle');
			break;
		case 'WAITING_OPERATION':
			runtimeDetail = _('Waiting for another SQM operation');
			break;
		case 'RECOVERING':
			runtimeLabel = _('RECOVERING');
			runtimeDetail = _('Restoring managed CAKE/SQM');
			break;
		}
		lines[0] = E('strong', {
			'style': pending ? 'color:#d08b20' : 'color:#f44'
		}, runtimeLabel);
		lines.splice(1, 0, E('small', {
			'style': pending ?
				'display:block;color:#d08b20;white-space:normal;overflow-wrap:anywhere' :
				'display:block;color:#f66;white-space:normal;overflow-wrap:anywhere'
		}, runtimeDetail));
		return E('div', {
			'title': status.sqm_runtime_reason || _('Managed SQM runtime is unhealthy.')
		}, lines);
	}

	warning = probeWarning(status, enabled);

	if (!warning)
		return E('div', { 'title': status.uplink_reason || '' }, lines);

	lines.push(E('small', {
			'style': 'display:block;color:#b00;white-space:nowrap'
		}, [ '⚠ ', _('No probe replies') ]));
	return E('div', { 'title': warning }, lines);
}

function formatHealthRate(value) {
	value = Number(value || 0);
	if (!isFinite(value) || value <= 0)
		return '-';
	if (value >= 1000000)
		return (value / 1000000).toFixed(value >= 10000000 ? 1 : 2) + ' Gbps';
	if (value >= 1000)
		return (value / 1000).toFixed(value >= 100000 ? 0 : 1) + ' Mbps';
	return value.toFixed(0) + ' kbps';
}

function formatServices(health) {
	var overall, diagnostics, details, transient, summaryTitle;

	if (!health)
		return E('div', { 'class': 'cake-services-stack cake-services-unavailable' }, [
			E('strong', {}, _('UNAVAILABLE')),
			E('small', {}, _('Runtime reconciliation returned no data.'))
		]);

	overall = String(health.overall_state || 'UNKNOWN').toUpperCase();
	diagnostics = [
		_('Overall: %s').format(overall),
		_('Autorate: %s (%d process(es))').format(
			health.autorate_state || '-', Number(health.autorate_processes || 0)),
		_('Controller: %s%s').format(
			health.controller_state || '-',
			health.controller_status_fresh ? '' : _(' (status stale or absent)')),
		_('Managed SQM: %s (%s)').format(health.sqm_config_state || '-', health.sqm_section || '-'),
		_('Upload CAKE: %s on %s at %s').format(
			health.cake_ul_state || '-', health.ul_interface || '-',
			formatHealthRate(health.cake_ul_rate_kbps)),
		_('Download CAKE: %s on %s at %s').format(
			health.cake_dl_state || '-', health.dl_interface || '-',
			formatHealthRate(health.cake_dl_rate_kbps)),
		_('Traffic rules: %s (configured %s; resolved %s; upload CAKE %s)').format(
			health.classifier_state || '-', health.traffic_profile_mode || '-',
			health.traffic_profile_resolved || health.classifier_profile || '-',
			health.cake_ul_mode || '-'),
		_('Attested rules: %s (Auto-Tune %s; configured %s; resolved %s)').format(
			health.classifier_target || '-',
			health.classifier_applied_autotune_profile || '-',
			health.classifier_applied_configured_profile || '-',
			health.classifier_applied_resolved_profile || health.classifier_applied_profile || '-'),
		_('IFB: %s').format(health.ifb_state || '-'),
		_('Ingress redirect: %s').format(health.ingress_state || '-'),
		_('Operation: %s').format(health.operation_state || '-'),
		_('Apply transaction: %s').format(health.apply_state || '-')
	];
	if (health.issues)
		diagnostics.push(_('Detected issue: %s').format(health.issues));

	transient = overall === 'WAITING' || overall === 'RECOVERING';
	summaryTitle = _('Overall: %s').format(overall);
	if (health.controller_reason)
		summaryTitle += '\n' + health.controller_reason;

	details = transient ? [
		E('span', { 'class': 'cake-services-wait-reason' },
			health.controller_reason ||
			(health.target_state === 'MISSING' ?
				_('WAN link unavailable; automatic recovery is armed.') :
				_('Managed SQM is settling; automatic recovery is armed.'))),
		E('span', {}, [
			_('Autorate %s').format(health.autorate_state || '-'),
			' · ',
			_('SQM %s').format(health.sqm_config_state || '-')
		]),
		E('span', {}, _('Operation %s').format(health.operation_state || '-'))
	] : [
		E('span', {}, [
			_('Autorate %s').format(health.autorate_state || '-'),
			' · ',
			_('Controller %s').format(health.controller_state || '-')
		]),
		E('span', {}, [
			_('SQM %s').format(health.sqm_config_state || '-'),
			' · ',
			_('Runtime %s').format(health.controller_status_fresh ? _('fresh') : _('stale'))
		]),
		E('span', {}, [
			_('UL %s %s').format(
				health.cake_ul_state || '-', formatHealthRate(health.cake_ul_rate_kbps)),
			' · ',
			_('DL %s %s').format(
				health.cake_dl_state || '-', formatHealthRate(health.cake_dl_rate_kbps))
		]),
		E('span', {}, [
			_('IFB %s').format(health.ifb_state || '-'),
			' · ',
			_('Ingress %s').format(health.ingress_state || '-')
		]),
		E('span', {}, [
			_('Rules %s').format(health.classifier_state || '-'),
			' · ',
			_('Profile %s').format(health.traffic_profile_resolved || health.classifier_profile || '-')
		]),
		E('span', {}, _('Operation %s').format(health.operation_state || '-'))
	];
	if (health.issues) {
		details.push(E('small', {
			'class': transient ? 'cake-services-note' : 'cake-services-issue'
		}, health.issues));
	}
	details.push(E('details', { 'class': 'cake-services-technical' }, [
		E('summary', {}, _('Technical details')),
		E('div', {}, diagnostics.map(function(line) {
			return E('small', {}, line);
		}))
	]));

	return E('div', {
		'class': 'cake-services-stack cake-services-' + overall.toLowerCase().replace(/[^a-z0-9_-]/g, '-'),
		'title': summaryTitle
	}, [
		E('strong', { 'class': 'cake-services-overall' }, overall),
		E('div', { 'class': 'cake-services-details' }, details)
	]);
}

function formatRoute(status) {
	if (!status || !status.route_mode)
		return '-';

	var member = status.mwan3_member || _('main');
	var device = status.route_device || status.ul_if || '-';
	var external = status.route_external_ip || '-';
	var title = [
		_('Mode: %s').format(status.route_mode),
		_('Member: %s (%s)').format(member, status.mwan3_member_status || '-'),
		_('Device: %s').format(device),
		_('Source IP: %s').format(status.route_source_ip || '-'),
		_('External IP: %s').format(external),
		_('fwmark: %s').format(status.route_fwmark || '-'),
		_('Routing table: %s').format(status.route_table || '-'),
		_('Default-active: %s').format(status.route_active ? _('yes') : _('no')),
		_('Forced-test ready: %s').format(status.route_test_ready ? _('yes') : _('no')),
		_('Uplink error code: %s').format(status.uplink_error_code || '-'),
		_('Reason: %s').format(status.uplink_reason || '-')
	].join('\n');

	return E('div', { 'title': title }, [
		E('strong', {}, '%s → %s'.format(member, device)),
		E('small', { 'style': 'display:block;white-space:nowrap' },
			_('External: %s').format(external))
	]);
}

function reflectorList(values) {
	if (!Array.isArray(values))
		return [];

	return values.filter(function(value) {
		return value != null && value !== '';
	}).map(String);
}

function previewList(values, limit) {
	values = reflectorList(values);

	if (!values.length)
		return '-';

	if (values.length <= limit)
		return values.join(', ');

	return '%s +%d'.format(values.slice(0, limit).join(', '), values.length - limit);
}

function reflectorSummary(status) {
	var active = reflectorList(status.active_reflectors);
	var spare = reflectorList(status.spare_reflectors);
	var bad = reflectorList(status.bad_reflectors);
	var title = [
		'Active: ' + (active.length ? active.join(', ') : '-'),
		'Spare: ' + (spare.length ? spare.join(', ') : '-'),
		'Bad: ' + (bad.length ? bad.join(', ') : '-')
	].join('\n');

	return E('div', { 'class': 'cake-reflector-summary', 'title': title }, [
		E('div', {}, _('Active: %s').format(previewList(active, 3))),
		E('div', {}, _('Spare: %s').format(previewList(spare, 2))),
		E('div', { 'class': bad.length ? 'cake-reflector-bad' : '' }, _('Bad: %s').format(previewList(bad, 2)))
	]);
}

function renderQualityAction(section, status, enabled, calibrationSummary) {
	var readiness = qualityReadiness(section, status, 'automatic', calibrationSummary);
	var available = enabled && calibrationSummary && calibrationSummary.native_rating === true;

	return E('div', { 'class': 'cake-quality-action' }, [
		E('button', {
			'type': 'button',
			'class': 'btn cbi-button cbi-button-action',
			'disabled': available ? null : '',
			'title': readiness.reason,
			'click': ui.createHandlerFn(null, function() {
				showQualityTest(section, status, calibrationSummary);
			})
		}, _('Get rating')),
		E('small', { 'class': readiness.ready ? 'cake-quality-ready' : 'cake-quality-not-ready' },
			readiness.ready ? _('Ready') : readiness.reason)
	]);
}

function statusCell(column, sectionData, status, enabled, health, calibrationSummary) {
	var section = sectionData['.name'];

	if (!enabled && column.key !== 'instance' && column.key !== 'uplink' &&
	    column.key !== 'services' && column.key !== 'rating')
		return '-';

	switch (column.key) {
	case 'instance': return section;
	case 'uplink': return formatState(status, enabled, sectionData, health);
	case 'services': return formatServices(health);
	case 'quality': return enabled ? formatQuality(status) : '-';
	case 'rating': return renderQualityAction(sectionData, status, enabled, calibrationSummary);
	case 'route': return formatRoute(status);
	case 'updated':
		return status.updated_at ? E('div', { 'class': 'cake-status-timestamp' }, [
			E('span', {}, new Date(status.updated_at * 1000).toLocaleDateString()),
			E('small', {}, new Date(status.updated_at * 1000).toLocaleTimeString())
		]) : '-';
	case 'reflector': return status.reflector || '-';
	case 'runtime_reflectors': return reflectorSummary(status);
	case 'rtt': return status.rtt_ms != null ? Number(status.rtt_ms).toFixed(2) + ' ms' : '-';
	case 'dl_achieved': return formatRate(status.dl_achieved_rate_kbps);
	case 'ul_achieved': return formatRate(status.ul_achieved_rate_kbps);
	case 'cake_dl': return formatShaperRate(status, 'dl');
	case 'cake_ul': return formatShaperRate(status, 'ul');
	case 'cpu': return formatPercent(status.cpu_total_percent);
	default: return '-';
	}
}

function renderTable(sections, statuses, selectedKeys, runtimeHealth, calibrationSummary) {
	var columns = selectedStatusColumns(selectedKeys);
	var children = [ E('tr', { 'class': 'tr table-titles' }, columns.map(function(column) {
		return E('th', { 'class': 'th', 'data-column': column.key }, column.title);
	})) ];

	if (!sections.length) {
		children.push(E('tr', { 'class': 'tr' }, [
			E('td', { 'class': 'td', 'colspan': String(columns.length) }, _('No instances configured.'))
		]));
	}

	sections.forEach(function(sectionData, index) {
		var status = statuses[index] || {};
		var enabled = String(sectionData.enabled || '0') === '1';
		var health = runtimeHealth && runtimeHealth[sectionData['.name']] || null;
		children.push(E('tr', { 'class': 'tr cake-status-row' }, columns.map(function(column) {
			return E('td', {
				'class': 'td cake-status-cell cake-status-column-' + column.key,
				'data-title': column.title,
				'data-column': column.key
			}, statusCell(column, sectionData, status, enabled, health, calibrationSummary));
		})));
	});

	return E('table', {
		'class': 'table cake-status-table ' +
			(columns.length === STATUS_DEFAULT_COLUMNS.length ?
				'cake-status-table-compact' : 'cake-status-table-expanded')
	}, children);
}

function renderCards(sections, statuses, selectedKeys, runtimeHealth, calibrationSummary) {
	var columns = selectedStatusColumns(selectedKeys);

	if (!sections.length)
		return E('div', { 'class': 'alert-message notice' }, _('No instances configured.'));

	return E('div', { 'class': 'cake-status-cards' }, sections.map(function(sectionData, index) {
		var status = statuses[index] || {};
		var enabled = String(sectionData.enabled || '0') === '1';
		var health = runtimeHealth && runtimeHealth[sectionData['.name']] || null;
		return E('section', { 'class': 'cake-status-card' }, columns.map(function(column) {
			return E('div', { 'class': 'cake-status-card-field cake-status-card-' + column.key }, [
				E('strong', { 'class': 'cake-status-card-label' }, column.title),
				E('div', { 'class': 'cake-status-card-value' },
					statusCell(column, sectionData, status, enabled, health, calibrationSummary))
			]);
		}));
	}));
}

function renderStatusData(sections, statuses, selectedKeys, runtimeHealth, calibrationSummary,
    schedulerDiagnostics) {
	var children = [];

	if (schedulerDiagnostics && schedulerDiagnostics.length) {
		children.push(E('div', {
			'class': 'alert-message warning cake-scheduler-diagnostics'
		}, [
			E('strong', {}, _('Native scheduler diagnostics')),
			E('div', {}, schedulerDiagnostics.map(function(issue) {
				return E('small', { 'style': 'display:block;white-space:normal' },
					_('%s: %s').format(issue.instance, issue.message));
			}))
		]));
	}
	children.push(
		E('div', { 'class': 'cake-status-table-scroll' },
			renderTable(sections, statuses, selectedKeys, runtimeHealth, calibrationSummary)),
		renderCards(sections, statuses, selectedKeys, runtimeHealth, calibrationSummary)
	);
	return E('div', { 'class': 'cake-status-data' }, children);
}

function renderColumnChooser(globalSection, selectedKeys, onChange) {
	var selected = {};
	selectedKeys.forEach(function(key) { selected[key] = true; });
	var details = E('details', { 'class': 'cake-status-column-picker' });
	var checks = {};
	var options = STATUS_COLUMN_DEFINITIONS.map(function(column) {
		var input = E('input', {
			'type': 'checkbox',
			'checked': selected[column.key] || column.mandatory ? '' : null,
			'disabled': column.mandatory ? '' : null
		});
		checks[column.key] = input;
		return E('label', { 'class': 'cake-status-column-option' }, [ input, column.title ]);
	});

	function saveSelection(reset) {
		var keys = STATUS_DEFAULT_COLUMNS.slice();
		if (!reset) {
			STATUS_COLUMN_DEFINITIONS.forEach(function(column) {
				if (!column.mandatory && checks[column.key].checked)
					keys.push(column.key);
			});
		}
		var storedKeys = keys.filter(function(key) {
			return STATUS_DEFAULT_COLUMNS.indexOf(key) < 0;
		});
		var args = [ reset ? 'reset' : 'set' ].concat(reset ? [] : storedKeys);

		return fs.exec('/usr/libexec/cake-autorate-rs/status-columns', args).then(function(result) {
			if (!result || result.code !== 0)
				throw new Error(result && result.stderr || _('status-columns helper failed'));
			details.open = false;
			onChange(keys);
		}).catch(function(error) {
			ui.addNotification(null, E('p', _('Unable to save Status columns: %s').format(
				error.message || error)), 'error');
		});
	}

	details.appendChild(E('summary', { 'class': 'btn cbi-button cbi-button-neutral' }, _('List columns')));
	details.appendChild(E('div', { 'class': 'cake-status-column-menu' }, [
		E('div', { 'class': 'cake-status-column-options' }, options),
		E('div', { 'class': 'cake-status-column-buttons' }, [
			E('button', {
				'type': 'button',
				'class': 'btn cbi-button cbi-button-positive',
				'click': ui.createHandlerFn(null, function() { return saveSelection(false); })
			}, _('Apply')),
			E('button', {
				'type': 'button',
				'class': 'btn cbi-button cbi-button-neutral',
				'click': ui.createHandlerFn(null, function() { return saveSelection(true); })
			}, _('Reset default'))
		])
	]));
	return details;
}

return L.view.extend({
	load: function() {
		return uci.load('cake-autorate').then(function() {
			var sections = uci.sections('cake-autorate', 'cake_autorate');
			var globalSection = uci.sections('cake-autorate', 'globals').filter(function(section) {
				return section['.name'] === 'globals';
			})[0] || { '.name': 'globals' };
			var schedulerEngine = String(
				uci.get('cake-autorate', 'globals', 'autotune_scheduler_engine') || '');
			return Promise.all([
				readPackageVersions(),
				readRuntimeHealth(),
				readCalibrationSummary()
			]).then(function(result) {
				return readInstanceStatuses(sections, schedulerEngine, result[2]).then(function(status) {
					return [ sections, status.rows, result[0], globalSection, result[1], result[2],
						status.diagnostics, schedulerEngine ];
				});
			});
		});
	},

	render: function(data) {
		cakeUi.ensureAppHeader();
		var sections = data[0];
		var statuses = data[1];
		var versions = data[2] || {};
		var globalSection = data[3] || { '.name': 'globals' };
		var runtimeHealth = data[4] || {};
		var calibrationSummary = data[5] || { state: 'unavailable', native_rating: false };
		var schedulerDiagnostics = data[6] || [];
		var schedulerEngine = data[7] || '';
		var visibleColumns = statusColumnSelection(globalSection);
		var statusData = renderStatusData(sections, statuses, visibleColumns, runtimeHealth,
			calibrationSummary, schedulerDiagnostics);
		var columnChooser;
		var replaceStatusData = function(nextStatuses, nextColumns, nextRuntimeHealth,
		    nextCalibrationSummary, nextSchedulerDiagnostics) {
			statuses = nextStatuses || statuses;
			visibleColumns = nextColumns || visibleColumns;
			if (nextRuntimeHealth != null)
				runtimeHealth = nextRuntimeHealth;
			if (nextCalibrationSummary != null)
				calibrationSummary = nextCalibrationSummary;
			if (nextSchedulerDiagnostics != null)
				schedulerDiagnostics = nextSchedulerDiagnostics;
			var nextData = renderStatusData(sections, statuses, visibleColumns, runtimeHealth,
				calibrationSummary, schedulerDiagnostics);
			if (statusData.parentNode) {
				statusData.parentNode.replaceChild(nextData, statusData);
				statusData = nextData;
			}
		};
		columnChooser = renderColumnChooser(globalSection, visibleColumns, function(keys) {
			replaceStatusData(statuses, keys);
		});

		poll.add(function() {
			return Promise.all([
				readRuntimeHealth(),
				readCalibrationSummary()
			]).then(function(result) {
				return readInstanceStatuses(sections, schedulerEngine, result[1]).then(function(status) {
					replaceStatusData(status.rows, null, result[0], result[1], status.diagnostics);
				});
			});
		}, 5);

		return E('div', { 'class': 'cake-status-root' }, [
			E('style', {}, [
				'.cake-status-root{width:100%;max-width:100%;min-width:0;margin:0;box-sizing:border-box}',
				'.cake-status-toolbar{display:flex;align-items:center;justify-content:space-between;gap:12px;flex-wrap:wrap}',
				'.cake-status-table-scroll{width:100%;max-width:100%;min-width:0;overflow-x:auto;margin-top:18px}',
				'.cake-status-table{width:100%;margin:0}',
				'.cake-status-table-compact{min-width:0;table-layout:fixed}',
				'.cake-status-table-compact{width:100%!important;max-width:100%!important}',
				'.cake-status-table-compact th,.cake-status-table-compact td{min-width:0!important;max-width:none!important;box-sizing:border-box!important;white-space:normal!important;overflow-wrap:anywhere}',
				'.cake-status-table-compact [data-column="instance"]{width:9%}',
				'.cake-status-table-compact [data-column="uplink"]{width:15%}',
				'.cake-status-table-compact [data-column="services"]{width:28%}',
				'.cake-status-table-compact [data-column="quality"]{width:30%}',
				'.cake-status-table-compact [data-column="rating"]{width:18%}',
				'.cake-status-table-compact .cake-services-stack,.cake-status-table-compact .cake-quality-stack,.cake-status-table-compact .cake-quality-action{min-width:0}',
				'.cake-status-table-compact [data-column="instance"] *,.cake-status-table-compact [data-column="uplink"] *,.cake-status-table-compact [data-column="services"] *,.cake-status-table-compact [data-column="quality"] *,.cake-status-table-compact [data-column="rating"] *{max-width:100%;white-space:normal!important;overflow-wrap:anywhere}',
				'.cake-status-table-expanded{min-width:max-content;table-layout:auto}',
				'.cake-status-table th{vertical-align:bottom!important;padding-top:10px!important;padding-bottom:10px!important}',
				'.cake-status-table td{vertical-align:top!important;padding-top:13px!important;padding-bottom:13px!important;line-height:1.35}',
				'.cake-status-row{border-bottom:1px solid rgba(127,127,127,.25)}',
				'.cake-status-cell>div{min-height:100%;display:flex;flex-direction:column;align-items:flex-start}',
				'.cake-status-cell small{display:block;margin-top:3px;line-height:1.3}',
				'.cake-status-timestamp span,.cake-status-timestamp small{white-space:nowrap}',
				'.cake-services-stack{gap:5px;min-width:210px}.cake-services-overall{letter-spacing:.025em}',
				'.cake-services-details{display:flex!important;flex-direction:column;gap:2px;font-size:11px;line-height:1.3}',
				'.cake-services-healthy .cake-services-overall{color:#16a085}.cake-services-disabled .cake-services-overall{color:#888}',
				'.cake-services-unmanaged .cake-services-overall,.cake-services-degraded .cake-services-overall,.cake-services-waiting .cake-services-overall,.cake-services-recovering .cake-services-overall{color:#d08b20}',
				'.cake-services-orphaned .cake-services-overall,.cake-services-blocked .cake-services-overall,.cake-services-unavailable strong{color:#d34b4b}',
				'.cake-services-issue{white-space:normal!important;overflow-wrap:anywhere;color:#d34b4b!important;max-width:100%}',
				'.cake-services-note,.cake-services-wait-reason{white-space:normal!important;overflow-wrap:anywhere;color:#d08b20!important;max-width:100%}',
				'.cake-services-technical{max-width:100%;margin-top:2px}.cake-services-technical>summary{cursor:pointer;color:#888;font-size:11px}.cake-services-technical>div{display:flex;flex-direction:column;gap:2px;margin-top:5px;white-space:normal;overflow-wrap:anywhere}',
				'.cake-quality-stack{gap:7px;min-width:210px}',
				'.cake-quality-detected{display:grid!important;grid-template-columns:66px minmax(30px,auto);column-gap:7px;align-items:baseline!important}',
				'.cake-quality-detected small{grid-column:1 / -1;color:#888;white-space:normal}',
				'.cake-quality-label{font-size:10px;font-weight:700;letter-spacing:.04em;color:#888}',
				'.cake-quality-grade-a-plus strong,.cake-quality-grade-a strong{color:#16a085}',
				'.cake-quality-grade-b strong{color:#8eae2f}.cake-quality-grade-c strong{color:#d08b20}',
				'.cake-quality-grade-d strong,.cake-quality-grade-f strong{color:#d34b4b}',
				'.cake-quality-stale{opacity:.65}',
				'.cake-quality-action{min-width:145px;display:flex;flex-direction:column;align-items:flex-start;gap:5px}',
				'.cake-quality-action small{white-space:normal;max-width:190px;color:#888}',
				'.cake-quality-ready{color:#16a085!important}',
				'.cake-quality-job-state{white-space:pre-wrap;margin-top:14px}',
				'.cake-quality-job-detail{line-height:1.45;margin:12px 0;color:#888}',
				'.cake-status-actions{display:flex;align-items:center;gap:7px;flex-wrap:wrap;box-sizing:border-box;margin:0}',
				'.cake-status-column-picker{position:relative;margin-left:auto}.cake-status-column-picker>summary{list-style:none;cursor:pointer}.cake-status-column-picker>summary::-webkit-details-marker{display:none}',
				'.cake-status-column-menu{position:absolute;right:0;top:calc(100% + 6px);z-index:20;min-width:270px;padding:12px;border:1px solid rgba(127,127,127,.4);border-radius:6px;background:var(--background-color-high,#fff);box-shadow:0 5px 20px rgba(0,0,0,.22)}',
				'.cake-status-column-options{display:grid;grid-template-columns:1fr;gap:7px}.cake-status-column-option{display:flex;align-items:center;gap:7px;white-space:nowrap}.cake-status-column-buttons{display:flex;gap:7px;margin-top:12px}',
				'.cake-status-cards{display:none}.cake-status-card{border:1px solid rgba(127,127,127,.3);border-radius:6px;padding:12px;background:rgba(127,127,127,.04)}.cake-status-card-field{display:grid;grid-template-columns:minmax(105px,35%) minmax(0,1fr);gap:10px;padding:8px 0;border-bottom:1px solid rgba(127,127,127,.18)}.cake-status-card-field:last-child{border-bottom:0}.cake-status-card-label{font-size:12px}.cake-status-card-value{min-width:0;overflow-wrap:anywhere}',
				'@media(max-width:900px){.cake-status-table-scroll{display:none}.cake-status-cards{display:grid;grid-template-columns:1fr;gap:12px;margin-top:16px}.cake-status-toolbar{align-items:flex-start}.cake-status-column-picker{margin-left:0}.cake-status-column-menu{left:0;right:auto;max-width:min(270px,calc(100vw - 36px))}}'
			].join('')),
			renderVersions(versions),
			E('div', { 'class': 'cbi-section cake-status-toolbar' }, [
				E('div', { 'class': 'cake-status-actions' }, [
					E('button', {
						'type': 'button',
						'class': 'btn cbi-button cbi-button-action',
						'click': ui.createHandlerFn(this, function() { return serviceAction('start'); })
					}, _('Start')),
					E('button', {
						'type': 'button',
						'class': 'btn cbi-button cbi-button-action',
						'click': ui.createHandlerFn(this, function() { return serviceAction('restart'); })
					}, _('Restart')),
					E('button', {
						'type': 'button',
						'class': 'btn cbi-button cbi-button-remove',
						'click': ui.createHandlerFn(this, function() { return serviceAction('stop'); })
					}, _('Stop')),
					E('button', {
						'type': 'button',
						'class': 'btn cbi-button cbi-button-action',
						'click': ui.createHandlerFn(this, exportLogs)
					}, _('Export logs'))
				]),
				columnChooser
			]),
			statusData
		]);
	}
});
