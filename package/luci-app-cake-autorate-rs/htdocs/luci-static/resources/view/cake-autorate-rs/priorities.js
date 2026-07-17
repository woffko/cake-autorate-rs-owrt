'use strict';
'require fs';
'require form';
'require uci';
'require ui';
'require cake-autorate-rs.ui as cakeUi';

var PROFILE_LABELS = {
	gaming: _('Gaming'),
	best_overall: _('Best overall'),
	fair: _('Fair')
};

var PRESET_LABELS = [
	[ 'custom', _('Custom ports') ],
	[ 'dns', _('DNS') ],
	[ 'ntp', _('NTP') ],
	[ 'web', _('Web (HTTP/HTTPS)') ],
	[ 'quic', _('QUIC / HTTP/3') ],
	[ 'ssh', _('SSH') ],
	[ 'steam_realtime', _('Steam real-time traffic') ],
	[ 'xbox_live', _('Xbox Live') ],
	[ 'playstation', _('PlayStation Network') ],
	[ 'wireguard', _('WireGuard') ]
];

function canonicalProfile(value) {
	switch (value) {
	case 'gaming':
		return 'gaming';
	case 'balanced':
	case 'best-overall':
	case 'best_overall':
	case '':
	case null:
	case undefined:
		return 'best_overall';
	case 'fair':
		return 'fair';
	default:
		return null;
	}
}

function validatePortList(sectionId, value) {
	var entries, index, match, first, last;

	if (value == null || value === '')
		return true;
	if (!/^[0-9,-]+$/.test(value))
		return _('Use comma-separated ports or ranges, for example 53,443,27000-27100.');

	entries = value.split(',');
	for (index = 0; index < entries.length; index++) {
		match = entries[index].match(/^([0-9]+)(?:-([0-9]+))?$/);
		if (!match)
			return _('Every port must be a number or an ascending range.');
		first = Number(match[1]);
		last = match[2] == null ? first : Number(match[2]);
		if (!Number.isInteger(first) || !Number.isInteger(last) ||
		    first < 1 || first > 65535 || last < first || last > 65535)
			return _('Ports must be between 1 and 65535 and ranges must be ascending.');
	}

	return true;
}

function validateNetwork(sectionId, value) {
	var address, prefix, octets, index, limit;

	if (value == null || value === '')
		return true;
	if (/\s/.test(value) || value.length > 64)
		return _('Enter one IPv4 or IPv6 address/prefix without spaces.');

	address = value;
	prefix = null;
	if (value.indexOf('/') >= 0) {
		if (value.indexOf('/') !== value.lastIndexOf('/'))
			return _('Enter one IPv4 or IPv6 address/prefix.');
		address = value.slice(0, value.indexOf('/'));
		prefix = value.slice(value.indexOf('/') + 1);
		if (!/^[0-9]+$/.test(prefix))
			return _('The network prefix must be a number.');
	}

	if (address.indexOf(':') >= 0) {
		if (!/^[0-9A-Fa-f:]+$/.test(address) || address.indexOf(':::', 0) >= 0 ||
		    address.indexOf(':') < 0)
			return _('Enter a valid IPv6 address or prefix.');
		limit = 128;
	} else {
		octets = address.split('.');
		if (octets.length !== 4)
			return _('Enter a valid IPv4 address or prefix.');
		for (index = 0; index < octets.length; index++)
			if (!/^[0-9]{1,3}$/.test(octets[index]) ||
			    Number(octets[index]) < 0 || Number(octets[index]) > 255)
				return _('Enter a valid IPv4 address or prefix.');
		limit = 32;
	}

	if (prefix != null && Number(prefix) > limit)
		return _('The prefix is outside the valid range for this address family.');
	return true;
}

function profileLabel(value) {
	return PROFILE_LABELS[canonicalProfile(value)] || _('Unknown');
}

function safeInstanceName(value) {
	return typeof value === 'string' && /^[A-Za-z0-9_]+$/.test(value);
}

function selectedInstanceFromLocation(locationObject) {
	var search = locationObject && typeof locationObject.search === 'string' ?
		locationObject.search : '';
	var match = search.match(/(?:^\?|&)instance=([^&]*)/);
	var value;

	if (!match)
		return null;
	try {
		value = decodeURIComponent(match[1].replace(/\+/g, ' '));
	} catch (error) {
		return null;
	}
	return safeInstanceName(value) ? value : null;
}

function settingsUrl() {
	return L.url('admin/network/cake-autorate-rs/settings');
}

function backToSettingsButton() {
	return E('button', {
		'class': 'btn cbi-button cbi-button-neutral',
		'click': function() { window.location = settingsUrl(); }
	}, [ '\u2190 ', _('Back to instances') ]);
}

function parseClassifierStatus(result) {
	var text = result && result.stdout ? result.stdout.trim() : '';
	var parsed;

	if (!text)
		return { state: 'unavailable', table_present: false };
	try {
		parsed = JSON.parse(text);
	} catch (error) {
		return { state: 'invalid', table_present: false };
	}
	return parsed && typeof parsed === 'object' ? parsed :
		{ state: 'invalid', table_present: false };
}

function addFlag(section, name, title, defaultValue, description) {
	var option = section.option(form.Flag, name, title);
	option.default = defaultValue;
	option.rmempty = false;
	if (description)
		option.description = description;
	return option;
}

function addList(section, name, title, values, defaultValue) {
	var option = section.option(form.ListValue, name, title);
	var index;

	for (index = 0; index < values.length; index++)
		option.value(values[index][0], values[index][1]);
	if (defaultValue != null)
		option.default = defaultValue;
	option.rmempty = false;
	return option;
}

return L.view.extend({
	load: function() {
		var requestedInstance = selectedInstanceFromLocation(window.location);
		var classifierArgs = [ 'status' ];
		if (requestedInstance)
			classifierArgs.push(requestedInstance);
		return Promise.all([
			uci.load('cake-autorate'),
			L.resolveDefault(
				fs.exec('/usr/libexec/cake-autorate-rs/traffic-classifier', classifierArgs)
					.then(parseClassifierStatus),
				{ state: 'unavailable', table_present: false }
			)
		]);
	},

	render: function(data) {
		var classifier = data[1] || {};
		var instances = uci.sections('cake-autorate', 'cake_autorate');
		var selectedInstance = selectedInstanceFromLocation(window.location);
		var selectedSection = instances.filter(function(section) {
			return section['.name'] === selectedInstance;
		})[0];
		var instanceValues = selectedSection ?
			[ [ selectedInstance, selectedInstance ] ] : [];
		var stateText;

		cakeUi.ensureAppHeader();
		if (!selectedSection)
			return E('div', { 'class': 'cbi-map' }, [
				E('h2', {}, _('Traffic priorities')),
				E('div', { 'class': 'alert-message error' },
					_('Select an existing instance from the Settings page before editing traffic priorities.')),
				E('div', { 'class': 'cbi-page-actions' }, backToSettingsButton())
			]);

		switch (classifier.state) {
		case 'active':
			stateText = _('The native outbound classifier is active for this instance and its loaded rules match the attested runtime state.');
			break;
		case 'missing':
			stateText = _('The classifier is active globally, but this instance has no attested rules. Save & Apply if outbound rules are enabled below.');
			break;
		case 'drifted':
			stateText = _('The private nftables table changed after it was applied. Save & Apply to replace it, then inspect the Services column.');
			break;
		case 'untracked':
			stateText = _('A private nftables table exists without valid cake-autorate-rs runtime attestation. Save & Apply to replace it safely.');
			break;
		default:
			stateText = _('The native outbound classifier is inactive. Save & Apply or inspect the Services column if rules are expected.');
		}
		var m, s, o, index;

		m = new form.Map('cake-autorate', _('Traffic priorities \u2014 %s').format(selectedInstance),
			_('Configure profile-specific outbound DSCP rules for this instance. cake-autorate-rs remains the only owner of SQM, CAKE, IFB devices and bandwidth rates; the classifier owns only its isolated nftables table.'));

		s = m.section(form.NamedSection, selectedInstance, 'cake_autorate',
			_('Instance policy'),
			_('Active calibration profile: %s. Enable only the rules that should reach this instance\'s outbound CAKE queue.').format(
				profileLabel(selectedSection.autotune_profile)));
		s.addremove = false;
		o = s.option(form.DummyValue, '_traffic_priority_notice', _('Runtime'));
		o.rawhtml = true;
		o.cfgvalue = function() {
			return E('div', {}, [
				E('div', {
					'class': classifier.state === 'active' ?
						'alert-message success' :
						(classifier.state === 'drifted' || classifier.state === 'untracked' ?
							'alert-message error' : 'alert-message notice')
				}, stateText),
				E('p', {}, _('Rules affect forwarded and router-originated packets before outbound CAKE. Download packets reach the SQM IFB before these nftables hooks, so Best overall and Fair deliberately keep download classification at best effort.')),
				E('p', {}, _('Built-in defaults run first. Enabled custom rules run afterwards in ascending order; a later matching rule can override an earlier class.'))
			]);
		};

		o = s.option(form.DummyValue, '_active_profile', _('Active profile'));
		o.cfgvalue = function() {
			return profileLabel(selectedSection.autotune_profile);
		};

		addFlag(s, 'traffic_rules_enabled', _('Outbound rules'), '0',
			_('Enable the native classifier for this managed SQM instance.'));
		addFlag(s, 'traffic_defaults_gaming', _('Gaming defaults'), '1',
			_('DNS/NTP and conservative game-platform presets; web remains best effort.'));
		addFlag(s, 'traffic_defaults_best_overall', _('Best overall defaults'), '1',
			_('Prioritize DNS/NTP and interactive SSH while keeping web/QUIC best effort.'));
		addFlag(s, 'traffic_defaults_fair', _('Fair defaults'), '1',
			_('Keep a minimal interactive set while sustained transfers remain best effort.'));

		s = m.section(form.GridSection, 'traffic_rule', _('Custom profile rules'));
		s.anonymous = true;
		s.addremove = true;
		s.addbtntitle = _('Add traffic rule');
		s.nodescriptions = true;
		s.description = _('Only rules belonging to this instance are shown. Built-ins run first; higher ordered custom matches run later.');
		s.filter = function(sectionId) {
			return uci.get('cake-autorate', sectionId, 'instance') === selectedInstance;
		};
		s.sectiontitle = function(sectionId) {
			return uci.get('cake-autorate', sectionId, 'name') || sectionId;
		};

		o = addFlag(s, 'enabled', _('Enabled'), '1');

		o = s.option(form.Value, 'name', _('Rule name'));
		o.rmempty = false;
		o.placeholder = _('Game or application');

		o = addList(s, 'instance', _('Instance'), instanceValues,
			selectedInstance);
		o.modalonly = true;
		o.validate = function(sectionId, value) {
			return value === selectedInstance ?
				true : _('Select an existing CAKE Autorate instance.');
		};

		o = addList(s, 'profile', _('Profile'), [
			[ 'gaming', _('Gaming') ],
			[ 'best_overall', _('Best overall') ],
			[ 'fair', _('Fair') ]
		], 'best_overall');

		o = addList(s, 'preset', _('Preset'), PRESET_LABELS, 'custom');

		o = addList(s, 'class', _('CAKE class'), [
			[ 'voice', _('Latency-critical / Voice (CS5)') ],
			[ 'video', _('Interactive / Video (AF41)') ],
			[ 'best_effort', _('Best effort (CS0)') ],
			[ 'background', _('Background / Bulk (CS1)') ]
		], 'voice');

		o = s.option(form.Value, 'order', _('Order'));
		o.datatype = 'and(uinteger,min(0),max(9999))';
		o.default = '500';
		o.rmempty = false;

		o = addList(s, 'family', _('Address family'), [
			[ 'any', _('IPv4 and IPv6') ],
			[ 'ipv4', _('IPv4 only') ],
			[ 'ipv6', _('IPv6 only') ]
		], 'any');
		o.modalonly = true;

		o = addList(s, 'protocol', _('Protocol'), [
			[ 'any', _('Any') ],
			[ 'tcp', _('TCP') ],
			[ 'udp', _('UDP') ],
			[ 'tcp_udp', _('TCP and UDP') ],
			[ 'icmp', _('ICMP / ICMPv6') ]
		], 'udp');
		o.depends('preset', 'custom');
		o.modalonly = true;

		o = s.option(form.Value, 'source_ports', _('Source ports'));
		o.placeholder = '1024-65535';
		o.validate = validatePortList;
		o.depends('preset', 'custom');
		o.modalonly = true;

		o = s.option(form.Value, 'destination_ports', _('Destination ports'));
		o.placeholder = '53,443,27000-27100';
		o.validate = validatePortList;
		o.depends('preset', 'custom');
		o.modalonly = true;

		o = s.option(form.Value, 'source_network', _('Source address / prefix'));
		o.placeholder = '192.168.1.50/32';
		o.validate = validateNetwork;
		o.modalonly = true;

		o = s.option(form.Value, 'destination_network', _('Destination address / prefix'));
		o.placeholder = '203.0.113.0/24';
		o.validate = validateNetwork;
		o.modalonly = true;

		for (index = 0; index < s.children.length; index++)
			if (s.children[index].option !== 'enabled' &&
			    s.children[index].option !== 'name' &&
			    s.children[index].option !== 'instance' &&
			    s.children[index].option !== 'profile' &&
			    s.children[index].option !== 'preset' &&
			    s.children[index].option !== 'class' &&
			    s.children[index].option !== 'order')
				s.children[index].modalonly = true;

		return m.render().then(function(node) {
			return E('div', {}, [
				E('div', { 'class': 'cbi-page-actions' }, backToSettingsButton()),
				node
			]);
		});
	}
});
