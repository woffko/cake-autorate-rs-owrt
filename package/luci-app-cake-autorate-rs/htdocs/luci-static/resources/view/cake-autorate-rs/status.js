'use strict';
'require fs';
'require poll';
'require uci';
'require ui';

function statusPath(section) {
	return '/var/run/cake-autorate/' + section + '/status.json';
}

function readStatus(section) {
	return L.resolveDefault(fs.read_direct(statusPath(section)).then(JSON.parse), null);
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

function formatPercent(value) {
	if (value == null)
		return '-';

	value = Number(value);
	return isNaN(value) ? '-' : value.toFixed(1) + '%';
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
		var section = sections[i]['.name'];
		var st = statuses[i] || {};

		rows.push([
			section,
			st.updated_at ? new Date(st.updated_at * 1000).toLocaleString() : '-',
			st.reflector || '-',
			reflectorSummary(st),
			st.rtt_ms != null ? Number(st.rtt_ms).toFixed(2) + ' ms' : '-',
			formatRate(st.dl_achieved_rate_kbps),
			formatRate(st.ul_achieved_rate_kbps),
			formatRate(st.cake_dl_rate_kbps),
			formatRate(st.cake_ul_rate_kbps),
			formatPercent(st.cpu_total_percent)
		]);
	}

	children = [
		E('tr', { 'class': 'tr table-titles' }, [
			E('th', { 'class': 'th' }, _('Instance')),
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
			E('td', { 'class': 'td', 'colspan': '10' }, _('No instances configured.'))
		]));
	}

	return E('table', { 'class': 'table' }, children);
}

return L.view.extend({
	load: function() {
		return uci.load('cake-autorate').then(function() {
			var sections = uci.sections('cake-autorate', 'cake_autorate');
			return Promise.all(sections.map(function(section) {
				return readStatus(section['.name']);
			})).then(function(statuses) {
				return [ sections, statuses ];
			});
		});
	},

	render: function(data) {
		var sections = data[0];
		var statuses = data[1];
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
