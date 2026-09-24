'use strict';
'require fs';
'require poll';
'require uci';

function safeInstance(value) {
	return typeof value === 'string' && /^[A-Za-z0-9_]{1,64}$/.test(value);
}

function statusPath(instance) {
	return '/var/run/cake-autorate/' + instance + '/status.json';
}

function readStatus(instance) {
	if (!safeInstance(instance))
		return Promise.resolve(null);

	return L.resolveDefault(fs.read(statusPath(instance)).then(function(data) {
		return JSON.parse(data);
	}), null);
}

function loadRows() {
	var sections = uci.sections('cake-autorate', 'cake_autorate') || [];

	return Promise.all(sections.map(function(section) {
		return readStatus(section['.name']).then(function(status) {
			return { config: section, status: status };
		});
	}));
}

function text(value, fallback) {
	return value == null || value === '' ? (fallback || '-') : String(value);
}

function textNode(value) {
	return document.createTextNode(value == null ? '' : String(value));
}

function rate(value) {
	value = Number(value);
	return Number.isFinite(value) && value > 0 ? Math.round(value) + ' kbit/s' : '-';
}

function number(value, suffix, digits) {
	value = Number(value);
	return Number.isFinite(value) ? value.toFixed(digits || 0) + (suffix || '') : '-';
}

function stateCell(row) {
	var status = row.status;
	var enabled = row.config.enabled === '1';
	var state = status && status.state || (enabled ? 'STARTING' : 'DISABLED');
	var degraded = status && status.runtime_control_degraded === true;
	var healthy = !status || (status.sqm_runtime_healthy !== false && !degraded);
	var color = !enabled ? '#777' : (healthy ? '#0a8f5a' : '#d94141');
	var label = E('span', { 'style': 'font-weight:600;color:' + color,
		'title': status && status.sqm_runtime_reason || '' }, textNode(state));
	if (degraded) {
		return E('div', {}, [label,
			E('small', { 'role': 'alert', 'style': 'display:block;color:#d94141' },
				textNode(_('Rate control paused: runtime owner could not be verified.'))),
			E('small', { 'style': 'display:block' }, textNode(status.runtime_control_error || ''))
		]);
	}
	if (status && status.runtime_control_held === true)
		return E('div', {}, [label, E('small', { 'style': 'display:block' },
			textNode(_('Rate control held by an active operation.')))]);
	return label;
}

function routeCell(row) {
	var status = row.status || {};
	var config = row.config;
	var route = status.route_interface || status.route_device || status.ul_if ||
		config.wan_if || config.sqm_interface || config.ul_if || '-';
	var uplink = status.uplink_state ? ' · ' + status.uplink_state : '';

	return text(route) + uplink;
}

function directionLabel(config) {
	switch (config.sqm_direction_mode || 'both') {
	case 'download_only': return _('Download only');
	case 'upload_only': return _('Upload only');
	case 'off': return _('Off');
	default: return _('Both');
	}
}

function renderRows(rows) {
	if (!rows.length)
		return E('div', { 'class': 'alert-message notice' },
			_('No manual controller instances are configured. Open Manual settings to create one.'));

	return E('table', { 'class': 'table cbi-section-table' }, [
		E('tr', { 'class': 'tr table-titles' }, [
			E('th', { 'class': 'th' }, _('Instance')),
			E('th', { 'class': 'th' }, _('State')),
			E('th', { 'class': 'th' }, _('Route')),
			E('th', { 'class': 'th' }, _('CAKE directions')),
			E('th', { 'class': 'th' }, _('Download')),
			E('th', { 'class': 'th' }, _('Upload')),
			E('th', { 'class': 'th' }, _('RTT')),
			E('th', { 'class': 'th' }, _('CPU'))
		])
	].concat(rows.map(function(row) {
		var status = row.status || {};
		return E('tr', { 'class': 'tr cbi-section-table-row' }, [
			E('td', { 'class': 'td' }, textNode(row.config['.name'])),
			E('td', { 'class': 'td' }, stateCell(row)),
			E('td', { 'class': 'td' }, textNode(routeCell(row))),
			E('td', { 'class': 'td' }, directionLabel(row.config)),
			E('td', { 'class': 'td' }, rate(status.cake_dl_rate_kbps)),
			E('td', { 'class': 'td' }, rate(status.cake_ul_rate_kbps)),
			E('td', { 'class': 'td' }, number(status.rtt_ms, ' ms', 1)),
			E('td', { 'class': 'td' }, number(status.cpu_total_percent, '%', 1))
		]);
	})));
}

return L.view.extend({
	load: function() {
		return uci.load('cake-autorate').then(loadRows);
	},

	render: function(rows) {
		var content = renderRows(rows);
		var root = E('div', {}, [
			E('h2', {}, _('CAKE Autorate RS — Lite')),
			E('div', { 'class': 'alert-message notice' },
				_('Manual controller mode. Rating, speed-test calibration and Full Auto-Tune are not installed.')),
			content
		]);

		poll.add(function() {
			return loadRows().then(function(nextRows) {
				var next = renderRows(nextRows);
				if (content.parentNode) {
					content.parentNode.replaceChild(next, content);
					content = next;
				}
			});
		}, 5);

		return root;
	},

	handleSaveApply: null,
	handleSave: null,
	handleReset: null
});
