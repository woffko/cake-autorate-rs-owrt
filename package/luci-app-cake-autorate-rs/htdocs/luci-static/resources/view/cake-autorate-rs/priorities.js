'use strict';
'require fs';
'require form';
'require uci';
'require ui';
'require cake-autorate-rs.ui as cakeUi';

var PROFILE_LABELS = {
	auto: _('Automatic'),
	gaming: _('Gaming'),
	best_overall: _('Best overall'),
	fair: _('Fair'),
	custom: _('Custom')
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

var PRESET_LABEL_MAP = PRESET_LABELS.reduce(function(result, item) {
	result[item[0]] = item[1];
	return result;
}, {});

var CLASS_LABELS = {
	voice: _('Latency-critical / Voice'),
	video: _('Interactive / Video'),
	best_effort: _('Best effort'),
	background: _('Background / Bulk')
};

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

function canonicalTrafficProfile(value) {
	switch (value) {
	case 'auto':
	case 'gaming':
	case 'best_overall':
	case 'fair':
	case 'custom':
		return value;
	case 'best-overall':
	case 'balanced':
		return 'best_overall';
	default:
		return null;
	}
}

function configuredTrafficProfile(section) {
	var configured = canonicalTrafficProfile(section && section.traffic_profile);
	var autotune = canonicalProfile(section && section.autotune_profile) || 'best_overall';
	if (configured)
		return configured;
	return section && section['traffic_defaults_' + autotune] === '0' ? 'custom' : 'auto';
}

function resolvedTrafficProfile(configured, autotune) {
	return configured === 'auto' ? (canonicalProfile(autotune) || 'best_overall') : configured;
}

function effectiveRuleProfile(value) {
	var profile = canonicalTrafficProfile(value);
	return profile === 'gaming' || profile === 'best_overall' ||
		profile === 'fair' || profile === 'custom' ? profile : 'custom';
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

function parsePresetCatalog(result) {
	var text = result && result.stdout ? result.stdout.trim() : '';
	var parsed;
	try {
		parsed = JSON.parse(text);
	} catch (error) {
		return { schema_version: 0, profiles: {} };
	}
	if (!parsed || parsed.schema_version !== 1 || !parsed.profiles ||
	    typeof parsed.profiles !== 'object')
		return { schema_version: 0, profiles: {} };
	return parsed;
}

function profileCard(title, summary) {
	return E('span', { 'class': 'traffic-profile-card-copy' }, [
		E('strong', {}, title),
		E('small', {}, summary)
	]);
}

function rulePreview(profile, catalog) {
	var rules = catalog && catalog.profiles && catalog.profiles[profile];
	if (profile === 'custom')
		return E('div', { 'class': 'traffic-profile-empty' },
			_('Custom uses only enabled rules assigned to the Custom profile below.'));
	if (!Array.isArray(rules) || rules.length === 0)
		return E('div', { 'class': 'traffic-profile-empty' },
			_('The built-in rule catalog is unavailable. Save & Apply is not affected, but the preview cannot be shown.'));
	return E('table', { 'class': 'table traffic-profile-rule-table' }, [
		E('thead', {}, E('tr', {}, [
			E('th', {}, _('Traffic')),
			E('th', {}, _('Match')),
			E('th', {}, _('CAKE class')),
			E('th', {}, _('DSCP'))
		])),
		E('tbody', {}, rules.map(function(rule) {
			var match = String(rule.protocol || _('any')).toUpperCase();
			var trafficLabel = _('Traffic');
			var matchLabel = _('Match');
			var classLabel = _('CAKE class');
			var dscpLabel = _('DSCP');
			if (rule.destination_ports)
				match += ' · ' + _('destination %s').format(rule.destination_ports);
			if (rule.source_ports)
				match += ' · ' + _('source %s').format(rule.source_ports);
			return E('tr', {}, [
				E('td', { 'data-label': trafficLabel }, PRESET_LABEL_MAP[rule.preset] || rule.preset || rule.id),
				E('td', { 'data-label': matchLabel }, match),
				E('td', { 'data-label': classLabel }, CLASS_LABELS[rule['class']] || rule['class']),
				E('td', { 'data-label': dscpLabel }, String(rule.dscp || '').toUpperCase())
			]);
		}))
	]);
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
			),
			L.resolveDefault(
				fs.exec('/usr/libexec/cake-autorate-rs/traffic-classifier', [ 'presets' ])
					.then(parsePresetCatalog),
				{ schema_version: 0, profiles: {} }
			)
		]);
	},

	render: function(data) {
		var classifier = data[1] || {};
		var catalog = data[2] || { profiles: {} };
		var instances = uci.sections('cake-autorate', 'cake_autorate');
		var selectedInstance = selectedInstanceFromLocation(window.location);
		var selectedSection = instances.filter(function(section) {
			return section['.name'] === selectedInstance;
		})[0];
		var instanceValues = selectedSection ?
			[ [ selectedInstance, selectedInstance ] ] : [];
		var configuredProfile, resolvedProfile, stateText;

		cakeUi.ensureAppHeader();
		if (!selectedSection)
			return E('div', { 'class': 'cbi-map' }, [
				E('h2', {}, _('Traffic priorities')),
				E('div', { 'class': 'alert-message error' },
					_('Select an existing instance from the Settings page before editing traffic priorities.')),
				E('div', { 'class': 'cbi-page-actions' }, backToSettingsButton())
			]);
		configuredProfile = configuredTrafficProfile(selectedSection);
		resolvedProfile = resolvedTrafficProfile(configuredProfile, selectedSection.autotune_profile);

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

		function stagePresetCopy(sourceProfile) {
			var existing = uci.sections('cake-autorate', 'traffic_rule').filter(function(rule) {
				return rule.instance === selectedInstance &&
					effectiveRuleProfile(rule.profile) === 'custom';
			});
			var rules = catalog.profiles && catalog.profiles[sourceProfile];

			uci.set('cake-autorate', selectedInstance, 'traffic_profile', 'custom');
			uci.set('cake-autorate', selectedInstance, 'traffic_profile_migrated', '1');
			if (existing.length > 0) {
				ui.addNotification(null, E('p', {},
					_('Custom rules already exist. They were preserved and the Custom profile was selected.')),
					'info');
				return uci.save().then(function() { window.location.reload(); });
			}
			if (!Array.isArray(rules) || rules.length === 0)
				return Promise.reject(new Error(_('The selected built-in profile has no readable catalog rules.')));

			rules.forEach(function(rule, ruleIndex) {
				var sectionId = uci.add('cake-autorate', 'traffic_rule');
				uci.set('cake-autorate', sectionId, 'enabled', '1');
				uci.set('cake-autorate', sectionId, 'instance', selectedInstance);
				uci.set('cake-autorate', sectionId, 'profile', 'custom');
				uci.set('cake-autorate', sectionId, 'preset', 'custom');
				uci.set('cake-autorate', sectionId, 'name', PRESET_LABEL_MAP[rule.preset] || rule.id);
				uci.set('cake-autorate', sectionId, 'family', 'any');
				uci.set('cake-autorate', sectionId, 'protocol', rule.protocol || 'any');
				uci.set('cake-autorate', sectionId, 'source_ports', rule.source_ports || '');
				uci.set('cake-autorate', sectionId, 'destination_ports', rule.destination_ports || '');
				uci.set('cake-autorate', sectionId, 'class', rule['class']);
				uci.set('cake-autorate', sectionId, 'order', String((ruleIndex + 1) * 100));
			});
			return uci.save().then(function() {
				ui.addNotification(null, E('p', {},
					_('An editable Custom copy was staged as unsaved changes. Review it, then use Save & Apply.')),
					'info');
				window.location.reload();
			});
		}

		function requestPresetCopy() {
			var checked = document.querySelector('.traffic-profile-selector input[type="radio"]:checked');
			var source = checked ? canonicalTrafficProfile(checked.value) : configuredProfile;
			if (source === 'auto')
				source = canonicalProfile(selectedSection.autotune_profile) || 'best_overall';
			if (source === 'custom') {
				ui.addNotification(null, E('p', {}, _('Custom is already selected. Edit its rules below.')), 'info');
				return Promise.resolve();
			}
			ui.showModal(_('Customize %s').format(PROFILE_LABELS[source]), [
				E('p', {}, _('This creates an independent editable copy of the shown built-in rules and switches this instance to Custom. Future package upgrades will not overwrite the copy. Existing rules are never deleted.')),
				E('div', { 'class': 'right' }, [
					E('button', { 'class': 'btn', 'click': ui.hideModal }, _('Cancel')),
					' ',
					E('button', {
						'class': 'btn cbi-button-positive important',
						'click': function() {
							ui.hideModal();
							/* Preserve valid edits already present elsewhere on this page
							 * before the staged Custom copy causes a reload. This writes only
							 * LuCI's pending UCI delta; it never commits or applies it. */
							return m.save(null, true).then(function() {
								return stagePresetCopy(source);
							}).catch(function(error) {
								ui.addNotification(null, E('p', {}, error.message || String(error)), 'error');
							});
						}
					}, _('Create editable copy'))
				])
			]);
			return Promise.resolve();
		}

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

		o = s.option(form.DummyValue, '_active_profile', _('Auto-Tune profile'));
		o.cfgvalue = function() {
			return profileLabel(selectedSection.autotune_profile);
		};

		addFlag(s, 'traffic_rules_enabled', _('Enable outbound traffic prioritization'), '0',
			_('Tags forwarded and router-originated upload packets before outbound CAKE. Turning this off does not stop CAKE, SQM or Autorate and does not remove their bandwidth limits.'));

		o = s.option(form.ListValue, 'traffic_profile', _('Traffic profile'));
		o.widget = 'radio';
		o.orientation = 'horizontal';
		o.default = 'auto';
		o.rmempty = false;
		o.value('auto', profileCard(_('Automatic (recommended)'),
		_('Follow the Auto-Tune profile; currently resolves to %s.').format(PROFILE_LABELS[resolvedProfile])));
		o.value('gaming', profileCard(_('Gaming'), _('DNS, NTP and conservative game-platform rules.')));
		o.value('best_overall', profileCard(_('Best overall'), _('Interactive essentials without prioritizing bulk web traffic.')));
		o.value('fair', profileCard(_('Fair'), _('A minimal interactive set with throughput first.')));
		o.value('custom', profileCard(_('Custom'), _('Only your editable Custom rules are active.')));
		o.cfgvalue = function() { return configuredProfile; };
		o.renderWidget = function() {
			var node = form.ListValue.prototype.renderWidget.apply(this, arguments);
			node.classList.add('traffic-profile-selector');
			return node;
		};

		o = s.option(form.DummyValue, '_traffic_profile_preview', _('Included rules'));
		o.rawhtml = true;
		o.cfgvalue = function() {
			return E('div', { 'id': 'traffic-profile-preview', 'data-profile': configuredProfile },
				rulePreview(resolvedProfile, catalog));
		};

		o = s.option(form.Button, '_customize_profile', _('Editable copy'));
		o.inputtitle = _('Customize this preset');
		o.inputstyle = 'action';
		o.onclick = requestPresetCopy;
		o.description = _('Copies the selected built-in rules into UCI as independent Custom rules. It never overwrites or deletes existing rules.');

		s = m.section(form.GridSection, 'traffic_rule', _('Editable traffic rules'));
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
			[ 'custom', _('Custom') ],
			[ 'gaming', _('Gaming') ],
			[ 'best_overall', _('Best overall') ],
			[ 'fair', _('Fair') ]
		], 'custom');
		o.cfgvalue = function(sectionId) {
			return effectiveRuleProfile(uci.get('cake-autorate', sectionId, 'profile'));
		};
		o.description = _('A rule is active only when this value matches the resolved traffic profile. Other rules remain saved but inactive.');

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
			var style = E('style', {}, [
				'.traffic-profile-selector .cbi-radio{display:inline-flex;vertical-align:top;width:min(18rem,calc(50% - .6rem));min-height:6.2rem;margin:.3rem;padding:.75rem;border:1px solid var(--border-color-medium,#666);border-radius:.55rem;box-sizing:border-box;cursor:pointer}',
				'.traffic-profile-selector .cbi-radio:has(input:checked){border-color:#00a67d;box-shadow:0 0 0 1px #00a67d;background:rgba(0,166,125,.08)}',
				'.traffic-profile-selector .cbi-radio input{margin:.2rem .55rem 0 0;flex:0 0 auto}',
				'.traffic-profile-card-copy{display:flex;flex-direction:column;gap:.35rem;line-height:1.25}',
				'.traffic-profile-card-copy small{font-weight:normal;opacity:.82}',
				'.traffic-profile-rule-table{max-width:58rem}',
				'.traffic-profile-rule-table th,.traffic-profile-rule-table td{white-space:normal}',
				'.traffic-profile-empty{padding:.75rem;border-left:3px solid #777}',
				'@media(max-width:700px){.traffic-profile-selector .cbi-radio{display:flex;width:100%;margin:.3rem 0}.traffic-profile-rule-table{display:block;font-size:.92em}.traffic-profile-rule-table thead{display:none}.traffic-profile-rule-table tbody,.traffic-profile-rule-table tr{display:block}.traffic-profile-rule-table tr{margin:.55rem 0;padding:.4rem;border:1px solid var(--border-color-medium,#777);border-radius:.45rem}.traffic-profile-rule-table td{display:grid;grid-template-columns:minmax(6.5rem,36%) minmax(0,1fr);gap:.55rem;padding:.25rem .35rem;border:0;overflow-wrap:anywhere}.traffic-profile-rule-table td:before{content:attr(data-label);font-weight:600;opacity:.78}}'
			].join('\n'));
			var selector = node.querySelector('.traffic-profile-selector');
			var preview = node.querySelector('#traffic-profile-preview');
			var showPreview = function(value) {
				var profile = canonicalTrafficProfile(value) || 'auto';
				var resolved = resolvedTrafficProfile(profile, selectedSection.autotune_profile);
				if (!preview)
					return;
				preview.setAttribute('data-profile', profile);
				preview.replaceChildren(rulePreview(resolved, catalog));
			};
			if (selector) {
				selector.querySelectorAll('.cbi-radio').forEach(function(card) {
					var input = card.querySelector('input[type="radio"]');
					if (!input)
						return;
					input.addEventListener('change', function() {
						if (input.checked)
							showPreview(input.value);
					});
					card.addEventListener('mouseenter', function() { showPreview(input.value); });
					card.addEventListener('mouseleave', function() {
						var checked = selector.querySelector('input[type="radio"]:checked');
						showPreview(checked ? checked.value : configuredProfile);
					});
				});
			}
			return E('div', {}, [
				style,
				E('div', { 'class': 'cbi-page-actions' }, backToSettingsButton()),
				node
			]);
		});
	}
});
