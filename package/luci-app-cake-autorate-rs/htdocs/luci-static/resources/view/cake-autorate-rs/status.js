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
	return fs.exec('/etc/init.d/cake-autorate', [ action ]).then(function() {
		ui.addNotification(null, E('p', _('Service action completed.')));
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
			E('td', { 'class': 'td', 'colspan': '9' }, _('No instances configured.'))
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
				}, _('Stop'))
			]),
			table
		]);
	}
});
