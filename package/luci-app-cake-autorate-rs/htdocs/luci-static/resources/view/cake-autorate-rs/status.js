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

	started = Number(status.started_at || 0);
	if (!isFinite(started) || started <= 0)
		return null;

	runtime = Date.now() / 1000 - started;
	if (!isFinite(runtime) || runtime < 10)
		return null;

	return _('No probe replies. Check the pinger and multi-WAN policy routing.');
}

function formatState(status, enabled) {
	var value = status && status.state;
	var warning;

	if (!value)
		return '-';

	value = String(value).toUpperCase();
	warning = probeWarning(status, enabled);

	if (!warning)
		return value;

	return E('div', { 'title': warning }, [
		E('div', {}, value),
		E('small', {
			'style': 'display:block;color:#b00;white-space:nowrap'
		}, [ '⚠ ', _('No probe replies') ])
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
				'-', '-', '-', '-', '-', '-', '-', '-', '-'
			];
			rows.push(disabledRow);
			continue;
		}

		rows.push([
			section,
			formatState(st, enabled),
			st.updated_at ? new Date(st.updated_at * 1000).toLocaleString() : '-',
			st.reflector || '-',
			reflectorSummary(st),
			st.rtt_ms != null ? Number(st.rtt_ms).toFixed(2) + ' ms' : '-',
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
			E('th', { 'class': 'th' }, _('State')),
			E('th', { 'class': 'th' }, _('Updated')),
			E('th', { 'class': 'th' }, _('Reflector')),
			E('th', { 'class': 'th' }, _('Runtime reflectors')),
			E('th', { 'class': 'th' }, _('RTT')),
			E('th', { 'class': 'th' }, _('DL achieved')),
			E('th', { 'class': 'th' }, _('UL achieved')),
			E('th', { 'class': 'th' }, _('CAKE DL')),
			E('th', { 'class': 'th' }, _('CAKE UL')),
			E('th', { 'class': 'th' }, _('CPU'))
		])
	];

	if (rows.length) {
		for (var i = 0; i < rows.length; i++)
			children.push(E('tr', { 'class': 'tr' }, rows[i].map(function(cell) {
				return E('td', { 'class': 'td' }, cell);
			})));
	} else {
		children.push(E('tr', { 'class': 'tr' }, [
			E('td', { 'class': 'td', 'colspan': '11' }, _('No instances configured.'))
		]));
	}

	return E('table', { 'class': 'table' }, children);
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
			renderVersions(versions),
			E('div', { 'class': 'cbi-page-actions' }, [
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
