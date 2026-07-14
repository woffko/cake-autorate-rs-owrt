'use strict';
'require fs';
'require poll';
'require uci';
'require ui';
'require cake-autorate-rs.ui as cakeUi';

function statusPath(section) {
	return '/var/run/cake-autorate/' + section + '/status.json';
}

function readStatus(section) {
	return L.resolveDefault(fs.read_direct(statusPath(section)).then(JSON.parse), null);
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

function formatShaperRate(status, direction) {
	var rateKey = direction === 'dl' ? 'cake_dl_rate_kbps' : 'cake_ul_rate_kbps';
	var configuredKey = direction === 'dl' ? 'configured_max_dl_shaper_rate_kbps' : 'configured_max_ul_shaper_rate_kbps';
	var effectiveKey = direction === 'dl' ? 'effective_max_dl_shaper_rate_kbps' : 'effective_max_ul_shaper_rate_kbps';
	var capKey = direction === 'dl' ? 'adaptive_ceiling_dl_cap_kbps' : 'adaptive_ceiling_ul_cap_kbps';
	var phaseKey = 'adaptive_ceiling_' + direction + '_phase';
	var safeKey = direction === 'dl' ? 'adaptive_ceiling_safe_dl_kbps' : 'adaptive_ceiling_safe_ul_kbps';
	var failedKey = direction === 'dl' ? 'adaptive_ceiling_failed_dl_kbps' : 'adaptive_ceiling_failed_ul_kbps';
	var probeKey = direction === 'dl' ? 'adaptive_ceiling_probe_dl_kbps' : 'adaptive_ceiling_probe_ul_kbps';
	var reasonKey = 'adaptive_ceiling_' + direction + '_last_reason';
	var rate = formatRate(status[rateKey]);
	var phase, failed, probe, detail, title;

	if (!status.adaptive_ceiling_enabled)
		return rate;

	phase = String(status[phaseKey] || 'cruise').replace(/_/g, ' ');
	failed = status[failedKey] == null ? '-' : formatRate(status[failedKey]);
	probe = status[probeKey] == null ? '-' : formatRate(status[probeKey]);
	detail = _('Phase: %s · safe: %s').format(phase, formatRate(status[safeKey]));
	title = [
		_('Configured max: %s').format(formatRate(status[configuredKey])),
		_('Effective ceiling: %s').format(formatRate(status[effectiveKey])),
		_('Absolute cap: %s').format(formatRate(status[capKey])),
		_('Failed bound: %s').format(failed),
		_('Probe target: %s').format(probe),
		_('Last transition: %s').format(status[reasonKey] || '-')
	].join('\n');

	return E('div', {
		'title': title
	}, [
		E('div', {}, rate),
		E('small', { 'style': 'white-space:nowrap' },
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

function renderDetectedGrade(label, result, state, collected, required) {
	var value, detail, classes = 'cake-quality-detected';

	if (!result) {
		if (state === 'none') {
			value = '-';
			detail = _('No completed rating yet');
		} else if (state === 'collecting') {
			value = _('COLLECTING');
			detail = _('%d / %d scored samples').format(Number(collected || 0), Number(required || 0));
		} else if (state === 'baseline_ready') {
			value = _('BASELINE READY');
			detail = _('Waiting for loaded traffic');
		} else {
			value = _('LEARNING');
			detail = _('Collecting idle baseline');
		}
	} else {
		value = result.partial ? _('PARTIAL') : (result.grade || '-');
		detail = _('+%s ms · %s · %s').format(
			Number(result.increase_ms || 0).toFixed(1),
			qualityDirectionSummary(result),
			qualityAge(result));
		if (!result.partial)
			classes += ' ' + qualityGradeClass(value);
		if (result.stale)
			classes += ' cake-quality-stale';
	}

	return E('div', { 'class': classes }, [
		E('span', { 'class': 'cake-quality-label' }, label),
		E('strong', {}, value),
		E('small', {}, detail + (result && result.partial ? ' · ' + _('partial') : '') +
			(result && result.stale ? ' · ' + _('STALE') : ''))
	]);
}

function formatQuality(status) {
	if (!status || !status.transport_latency_enabled)
		return E('span', { 'title': _('Transport-aware estimation is disabled.') }, '-');

	if (status.quality_grade_state) {
		var current = status.quality_grade_current || null;
		var previous = status.quality_grade_previous || null;
		var state = String(status.quality_grade_state || 'learning_baseline');
		var title = [
			_('Detected rating uses network RTT loaded p90 minus the preceding idle p5. DNS, process startup, and connection handshake time are excluded.'),
			_('Download and upload are scored independently; the worse grade is shown.'),
			_('A one-direction result is labeled PARTIAL and is never presented as the final connection rating.'),
			_('Bidirectional latency is diagnostic and does not affect the total grade.'),
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
			_('Transport error code: %s').format(status.transport_error_code || '-'),
			_('Safe floors: DL %s · UL %s').format(formatRate(status.throughput_floor_dl_kbps), formatRate(status.throughput_floor_ul_kbps))
		].join('\n');

		return E('div', { 'class': 'cake-quality-stack', 'title': title }, [
			renderDetectedGrade(_('CURRENT'), current, state,
				status.quality_grade_collected_samples, status.quality_grade_required_samples),
			renderDetectedGrade(_('PREVIOUS'), previous, previous ? 'final' : 'none', 0, 0)
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

function probeWarning(status, enabled) {
	var started, runtime;

	if (!enabled || !status || !status.state || hasProbeSample(status))
		return null;
	if (status.uplink_state === 'OFFLINE' || status.uplink_state === 'LEARNING')
		return null;

	started = Number(status.started_at || 0);
	if (!isFinite(started) || started <= 0)
		return null;

	runtime = Date.now() / 1000 - started;
	if (!isFinite(runtime) || runtime < 10)
		return null;

	return _('No probe replies. Check the pinger and multi-WAN policy routing.');
}

function formatState(status, enabled) {
	var value = status && (status.uplink_state || status.state);
	var warning;

	if (!value)
		return '-';

	value = String(value).toUpperCase();
	warning = probeWarning(status, enabled);

	if (!warning)
		return E('div', { 'title': status.uplink_reason || '' }, [
			E('strong', {}, value),
			E('small', { 'style': 'display:block;white-space:nowrap' },
				_('Controller: %s').format(String(status.state || '-').toUpperCase()))
		]);

	return E('div', { 'title': warning }, [
		E('div', {}, value),
		E('small', {
			'style': 'display:block;color:#b00;white-space:nowrap'
		}, [ '⚠ ', _('No probe replies') ])
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

function renderTable(sections, statuses) {
	var rows = [];
	var children;

	for (var i = 0; i < sections.length; i++) {
		var sectionData = sections[i];
		var section = sectionData['.name'];
		var st = statuses[i] || {};
		var enabled = String(sectionData.enabled || '0') === '1';
		var disabledRow;

		if (!enabled) {
				disabledRow = [
					section,
					_('DISABLED'),
					'-', '-', '-', '-', '-', '-', '-', '-', '-', '-', '-'
			];
			rows.push(disabledRow);
			continue;
		}

		rows.push([
			section,
			formatState(st, enabled),
			formatRoute(st),
			st.updated_at ? E('div', { 'class': 'cake-status-timestamp' }, [
				E('span', {}, new Date(st.updated_at * 1000).toLocaleDateString()),
				E('small', {}, new Date(st.updated_at * 1000).toLocaleTimeString())
			]) : '-',
			st.reflector || '-',
			reflectorSummary(st),
			st.rtt_ms != null ? Number(st.rtt_ms).toFixed(2) + ' ms' : '-',
			formatQuality(st),
			formatRate(st.dl_achieved_rate_kbps),
			formatRate(st.ul_achieved_rate_kbps),
			formatShaperRate(st, 'dl'),
			formatShaperRate(st, 'ul'),
			formatPercent(st.cpu_total_percent)
		]);
	}

	children = [
		E('tr', { 'class': 'tr table-titles' }, [
			E('th', { 'class': 'th' }, _('Instance')),
			E('th', { 'class': 'th' }, _('Uplink')),
			E('th', { 'class': 'th' }, _('Route')),
			E('th', { 'class': 'th' }, _('Updated')),
			E('th', { 'class': 'th' }, _('Reflector')),
			E('th', { 'class': 'th' }, _('Runtime reflectors')),
			E('th', { 'class': 'th' }, _('RTT')),
			E('th', { 'class': 'th' }, _('Quality')),
			E('th', { 'class': 'th' }, _('DL achieved')),
			E('th', { 'class': 'th' }, _('UL achieved')),
			E('th', { 'class': 'th' }, _('CAKE DL')),
			E('th', { 'class': 'th' }, _('CAKE UL')),
			E('th', { 'class': 'th' }, _('CPU'))
		])
	];

	if (rows.length) {
		for (var i = 0; i < rows.length; i++)
			children.push(E('tr', { 'class': 'tr cake-status-row' }, rows[i].map(function(cell) {
				return E('td', { 'class': 'td cake-status-cell' }, cell);
			})));
	} else {
		children.push(E('tr', { 'class': 'tr' }, [
			E('td', { 'class': 'td', 'colspan': '13' }, _('No instances configured.'))
		]));
	}

	return E('table', { 'class': 'table cake-status-table' }, children);
}

return L.view.extend({
	load: function() {
		return uci.load('cake-autorate').then(function() {
			var sections = uci.sections('cake-autorate', 'cake_autorate');
			return Promise.all([
				Promise.all(sections.map(function(section) {
					return readStatus(section['.name']);
				})),
				readPackageVersions()
			]).then(function(result) {
				return [ sections, result[0], result[1] ];
			});
		});
	},

	render: function(data) {
		cakeUi.ensureAppHeader();
		var sections = data[0];
		var statuses = data[1];
		var versions = data[2] || {};
		var table = renderTable(sections, statuses);

		poll.add(function() {
			return Promise.all(sections.map(function(section) {
				return readStatus(section['.name']);
			})).then(function(nextStatuses) {
				var nextTable = renderTable(sections, nextStatuses);
				if (table.parentNode) {
					table.parentNode.replaceChild(nextTable, table);
					table = nextTable;
				}
			});
		}, 5);

		return E('div', {}, [
			E('style', {}, [
				'.cake-status-table{width:100%;table-layout:auto;margin-top:18px}',
				'.cake-status-table th{vertical-align:bottom!important;padding-top:10px!important;padding-bottom:10px!important}',
				'.cake-status-table td{vertical-align:top!important;padding-top:13px!important;padding-bottom:13px!important;line-height:1.35}',
				'.cake-status-row{border-bottom:1px solid rgba(127,127,127,.25)}',
				'.cake-status-cell>div{min-height:100%;display:flex;flex-direction:column;align-items:flex-start}',
				'.cake-status-cell small{display:block;margin-top:3px;line-height:1.3}',
				'.cake-status-timestamp span,.cake-status-timestamp small{white-space:nowrap}',
				'.cake-quality-stack{gap:7px;min-width:210px}',
				'.cake-quality-detected{display:grid!important;grid-template-columns:66px minmax(30px,auto);column-gap:7px;align-items:baseline!important}',
				'.cake-quality-detected small{grid-column:1 / -1;color:#888;white-space:normal}',
				'.cake-quality-label{font-size:10px;font-weight:700;letter-spacing:.04em;color:#888}',
				'.cake-quality-grade-a-plus strong,.cake-quality-grade-a strong{color:#16a085}',
				'.cake-quality-grade-b strong{color:#8eae2f}.cake-quality-grade-c strong{color:#d08b20}',
				'.cake-quality-grade-d strong,.cake-quality-grade-f strong{color:#d34b4b}',
				'.cake-quality-stale{opacity:.65}',
				'.cake-status-actions{display:flex;align-items:center;gap:7px;flex-wrap:wrap;width:100%;box-sizing:border-box}',
				'@media(max-width:900px){.cake-status-table{display:block;overflow-x:auto}.cake-status-table th,.cake-status-table td{min-width:92px}.cake-status-table th:nth-child(6),.cake-status-table td:nth-child(6){min-width:180px}}'
			].join('')),
			renderVersions(versions),
			E('div', { 'class': 'cbi-page-actions cake-status-actions' }, [
				E('button', {
					'class': 'btn cbi-button cbi-button-action',
					'click': ui.createHandlerFn(this, function() { return serviceAction('start'); })
				}, _('Start')),
				' ',
				E('button', {
					'class': 'btn cbi-button cbi-button-action',
					'click': ui.createHandlerFn(this, function() { return serviceAction('restart'); })
				}, _('Restart')),
				' ',
				E('button', {
					'class': 'btn cbi-button cbi-button-remove',
					'click': ui.createHandlerFn(this, function() { return serviceAction('stop'); })
				}, _('Stop')),
				' ',
				E('button', {
					'class': 'btn cbi-button cbi-button-action',
					'click': ui.createHandlerFn(this, exportLogs)
				}, _('Export logs'))
			]),
			table
		]);
	}
});
