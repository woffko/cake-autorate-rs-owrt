'use strict';
'require cake-autorate-rs.candidate as candidateGuard';
'require cake-autorate-rs.sqm as sqmModes';
'require fs';
'require form';
'require rpc';
'require uci';
'require ui';

function validationMessage(result) {
	return E('p', {}, document.createTextNode(String(result)));
}

function modal(option) {
	option.modalonly = true;
	// An unrelated edit must not remove values hidden by dependencies.
	option.retain = true;
	return option;
}

function safeSqmScript(value) {
	return typeof value === 'string' && /^[A-Za-z0-9_-][A-Za-z0-9_.-]*\.qos$/.test(value);
}

function loadSqmScripts() {
	return L.resolveDefault(fs.list('/usr/lib/sqm'), []).then(function(entries) {
		return entries.filter(function(entry) {
			return (entry.type === 'file' || entry.type === 'link') && safeSqmScript(entry.name);
		}).map(function(entry) { return entry.name; });
	});
}

function addSqmScriptChoices(option, installed) {
	var seen = Object.create(null);
	var scripts = [ 'piece_of_cake.qos', 'cake.qos' ].concat(installed || []);
	(uci.sections('cake-autorate', 'cake_autorate') || []).forEach(function(section) {
		// Preserve a configured basename even if directory discovery is unavailable.
		scripts.push(section.sqm_script);
	});
	scripts.forEach(function(script) {
		if (safeSqmScript(script) && !seen[script]) {
			seen[script] = true;
			option.value(script);
		}
	});
}

var DEFAULT_REFLECTORS = [
	'1.1.1.1', '1.0.0.1', '8.8.8.8', '8.8.4.4', '9.9.9.9', '149.112.112.112'
];

function configured(section, sectionId, option, fallback) {
	var value = section.formvalue(sectionId, option);
	if (value == null)
		value = uci.get('cake-autorate', sectionId, option);
	return value == null || value === '' ? fallback : value;
}

function checked(section, sectionId, option, fallback) {
	return String(configured(section, sectionId, option, fallback ? '1' : '0')) === '1';
}

function integer(value) {
	return /^[0-9]+$/.test(String(value || '')) ? Number(value) : null;
}

function validateInterface(sectionId, value) {
	if (!/^[A-Za-z0-9_.:-]{1,15}$/.test(value || ''))
		return _('Use a Linux interface name containing at most 15 safe characters.');
	if (('ifb4' + value).length > 15)
		return _('The interface name is too long for the generated download IFB (ifb4%s).').format(value);
	return true;
}

function validateRateOrder(section, sectionId, direction) {
	var enabled = candidateGuard.directionEnabled(direction, function(key) {
		// Lite's enabled.write updates sqm_enabled during parse. Preview a
		// changed toggle so disabling and zeroing a direction can share a save.
		if (key === 'sqm_enabled') {
			var toggle = section.formvalue(sectionId, 'enabled');
			if (toggle != null && toggle !== uci.get('cake-autorate', sectionId, 'enabled')) return toggle;
		}
		var value = section.formvalue(sectionId, key);
		return value == null ? uci.get('cake-autorate', sectionId, key) : value;
	});
	return candidateGuard.validateRateTuple([ 'min', 'base', 'max' ].map(function(part) {
		var key = part + '_' + direction + '_shaper_rate_kbps';
		var value = section.formvalue(sectionId, key);
		return value == null ? uci.get('cake-autorate', sectionId, key) : value;
	}), direction, enabled);
}

function validateUnique(section, sectionId) {
	var target = configured(section, sectionId, 'wan_if', '');
	var sqm = configured(section, sectionId, 'sqm_section', 'cake_' + sectionId);
	var enabled = checked(section, sectionId, 'enabled', false);
	var manage = checked(section, sectionId, 'manage_sqm', true);
	var sections = uci.sections('cake-autorate', 'cake_autorate') || [];

	for (var i = 0; i < sections.length; i++) {
		var other = sections[i];
		if (other['.name'] === sectionId)
			continue;
		if (manage && other.manage_sqm !== '0' &&
			(other.sqm_section || 'cake_' + other['.name']) === sqm)
			return _('SQM section "%s" is already managed by %s.').format(sqm, other['.name']);
		if (enabled && manage && other.enabled === '1' && other.manage_sqm !== '0' &&
			(other.wan_if || other.sqm_interface || other.ul_if) === target)
			return _('Enabled instance %s already manages %s.').format(other['.name'], target);
	}
	return true;
}

function validateInstance(section, sectionId) {
	var result = validateInterface(sectionId, configured(section, sectionId, 'wan_if', ''));
	var mode = configured(section, sectionId, 'route_mode', 'auto');
	var direction = configured(section, sectionId, 'sqm_direction_mode', 'both');
	var reflectors = section.formvalue(sectionId, 'reflector') ||
		uci.get('cake-autorate', sectionId, 'reflector') || DEFAULT_REFLECTORS;
	var pingers = integer(configured(section, sectionId, 'no_pingers', '6'));

	if (result !== true)
		return result;
	if ([ 'auto', 'main', 'mwan3' ].indexOf(mode) < 0)
		return _('Select a supported route mode.');
	if (mode === 'mwan3' && !configured(section, sectionId, 'mwan3_member', ''))
		return _('An mwan3 member is required in mwan3 route mode.');
	if ([ 'both', 'download_only', 'upload_only' ].indexOf(direction) < 0)
		return _('Select a supported CAKE direction mode.');
	result = validateRateOrder(section, sectionId, 'dl');
	if (result !== true)
		return result;
	result = validateRateOrder(section, sectionId, 'ul');
	if (result !== true)
		return result;
	if (!Array.isArray(reflectors))
		reflectors = [ reflectors ];
	if (pingers == null || pingers < 1 || pingers > reflectors.length)
		return _('Pingers must be between 1 and the number of configured reflectors.');
	return validateUnique(section, sectionId);
}

function setDefaults(sectionId) {
	var defaults = {
		enabled: '0', manage_sqm: '1', sqm_enabled: '0', route_mode: 'auto',
		auto_interface_preset: '1', sqm_section: 'cake_' + sectionId,
		sqm_direction_mode: 'both', adjust_dl_shaper_rate: '1', adjust_ul_shaper_rate: '1',
		min_dl_shaper_rate_kbps: '5000', base_dl_shaper_rate_kbps: '20000',
		max_dl_shaper_rate_kbps: '80000', sqm_download: '20000',
		min_ul_shaper_rate_kbps: '5000', base_ul_shaper_rate_kbps: '20000',
		max_ul_shaper_rate_kbps: '35000', sqm_upload: '20000',
		connection_active_thr_kbps: '2000', adaptive_ceiling_enabled: '0',
		adaptive_ceiling_dl_cap_kbps: '80000', adaptive_ceiling_ul_cap_kbps: '35000',
		adaptive_ceiling_hold_time_s: '20', adaptive_ceiling_growth_percent: '3',
		adaptive_ceiling_probe_duration_s: '8', adaptive_ceiling_cooldown_s: '30',
		adaptive_ceiling_failed_bound_ttl_s: '900', sqm_qdisc: 'cake',
		sqm_script: 'piece_of_cake.qos', sqm_linklayer: 'none', sqm_overhead: '0',
		pinger_method: 'fping', no_pingers: '6', reflector_ping_interval_s: '0.3',
		high_load_thr: '0.75', bufferbloat_detection_window: '6',
		bufferbloat_detection_thr: '3', dl_owd_delta_delay_thr_ms: '30',
		ul_owd_delta_delay_thr_ms: '30', dl_avg_owd_delta_max_adjust_up_thr_ms: '10',
		ul_avg_owd_delta_max_adjust_up_thr_ms: '10',
		dl_avg_owd_delta_max_adjust_down_thr_ms: '60',
		ul_avg_owd_delta_max_adjust_down_thr_ms: '60'
	};

	Object.keys(defaults).forEach(function(key) {
		uci.set('cake-autorate', sectionId, key, defaults[key]);
	});
	uci.set('cake-autorate', sectionId, 'reflector', DEFAULT_REFLECTORS);
}

function addValue(section, tab, option, title, datatype, fallback) {
	var o = modal(section.taboption(tab, form.Value, option, title));
	o.datatype = datatype;
	o.default = fallback;
	o.rmempty = false;
	return o;
}

function addFlag(section, tab, option, title, fallback) {
	var o = modal(section.taboption(tab, form.Flag, option, title));
	o.enabled = '1';
	o.disabled = '0';
	o.default = fallback == null ? '0' : fallback;
	o.rmempty = false;
	return o;
}

function writeDirection(sectionId, value) {
	uci.set('cake-autorate', sectionId, 'sqm_direction_mode', value);
	if (value === 'upload_only')
		uci.set('cake-autorate', sectionId, 'adjust_dl_shaper_rate', '0');
	if (value === 'download_only')
		uci.set('cake-autorate', sectionId, 'adjust_ul_shaper_rate', '0');
}

var callUciRevertStatus = rpc.declare({
	object: 'uci',
	method: 'revert',
	params: [ 'config' ],
	reject: false
});

return L.view.extend({
	handleSaveApply: function(ev, mode) {
		return this.handleSave(ev).then(function() {
			return candidateGuard.apply(function() { return ui.changes.apply(mode == '0'); }, [ 'cake-autorate' ]);
		});
	},
	handleReset: function() {
		// Modal Save stages values in this RPC session. The stock Map.reset()
		// only re-renders them. Lite writes just this config, not sqm or network.
		return callUciRevertStatus('cake-autorate').then(function(status) {
			if (status !== 0)
				throw new Error(_('Unable to discard the staged CAKE Autorate settings.'));
			uci.unload([ 'cake-autorate' ]);
			window.location.reload();
		});
	},

	load: function() {
		return Promise.all([
			uci.load('cake-autorate'),
			L.resolveDefault(uci.load('sqm'), null),
			L.resolveDefault(uci.load('mwan3'), null),
			loadSqmScripts(),
			L.resolveDefault(candidateGuard.loadSchema(), null)
		]);
	},

	render: function(data) {
		var m = new form.Map('cake-autorate', _('CAKE Autorate RS — Lite'),
			_('Minimal manual controller. Configure explicit bounds and latency policy; no rating or calibration code is installed.'));
		candidateGuard.attach(m, [ 'cake-autorate' ]);
		var s = m.section(form.GridSection, 'cake_autorate', _('Manual instances'));
		candidateGuard.protectModal(s);
		var o;

		s.anonymous = false;
		s.addremove = true;
		s.addbtntitle = _('Create manual instance');
		s.nodescriptions = true;
		s.handleAdd = function(ev, name) {
			if (!/^[A-Za-z0-9_]{1,32}$/.test(name || ''))
				return Promise.reject(new TypeError(_('Instance names may contain 1–32 letters, digits or underscores.')));
			if (uci.get('cake-autorate', name))
				return Promise.reject(new TypeError(_('This instance already exists.')));

			var sectionId = this.map.data.add('cake-autorate', this.sectiontype, name);
			setDefaults(sectionId);
			this.map.addedSection = sectionId;
			return this.renderMoreOptionsModal(sectionId);
		};
		s.addModalOptions = function(modalSection, sectionId) {
			candidateGuard.attach(modalSection.map, [ 'cake-autorate' ]);
			var parse = modalSection.parse;
			modalSection.parse = function() {
				var result = validateInstance(this, sectionId);
				if (result !== true) {
					ui.addNotification(null, validationMessage(result), 'error');
					return Promise.reject(new TypeError(result));
				}
				return parse.apply(this, arguments);
			};
		};

	o = s.option(form.DummyValue, '_enabled', _('Enabled'));
	o.cfgvalue = function(sectionId) {
		return uci.get('cake-autorate', sectionId, 'enabled') === '1' ? _('yes') : _('no');
	};
	o = s.option(form.DummyValue, '_target', _('Target'));
	o.cfgvalue = function(sectionId) {
		return uci.get('cake-autorate', sectionId, 'wan_if') || '-';
	};
	o = s.option(form.DummyValue, '_rates', _('DL / UL'));
	o.cfgvalue = function(sectionId) {
		return '%s / %s kbit/s'.format(
			uci.get('cake-autorate', sectionId, 'base_dl_shaper_rate_kbps') || '-',
			uci.get('cake-autorate', sectionId, 'base_ul_shaper_rate_kbps') || '-');
	};

		s.tab('connection', _('Connection'));
		s.tab('rates', _('Rate limits'));
		s.tab('sqm', _('SQM'));
		s.tab('latency', _('Latency probes'));
		s.tab('controller', _('Controller'));

		o = addFlag(s, 'connection', 'enabled', _('Enable instance'), '0');
		o.write = function(sectionId, value) {
			uci.set('cake-autorate', sectionId, 'enabled', value);
			uci.set('cake-autorate', sectionId, 'sqm_enabled', value);
		};
		o = addValue(s, 'connection', 'wan_if', _('WAN device'), 'string', '');
		o.placeholder = 'eth0.2';
		o.validate = validateInterface;
		o = modal(s.taboption('connection', form.ListValue, 'route_mode', _('Route mode')));
		o.value('auto', _('Auto'));
		o.value('main', _('Main routing table'));
		o.value('mwan3', _('mwan3 member'));
		o.default = 'auto';
		o.rmempty = false;
		o = addValue(s, 'connection', 'mwan3_member', _('mwan3 member'), 'uciname', '');
		o.depends('route_mode', 'mwan3');
		o.rmempty = true;
		addFlag(s, 'connection', 'auto_interface_preset', _('Derive upload and IFB devices'), '1');
		o = addFlag(s, 'connection', 'external_ip_check_enabled', _('Look up external IPv4 address'), '0');
		o.description = _('Optional: contacts the selected HTTPS service, which sees your public address and request times. Disabled by default; not required for rate control.');
		o = addValue(s, 'connection', 'external_ip_check_url', _('External IPv4 service URL'), 'string', 'https://api.ipify.org');
		o.description = _('HTTPS host/path returning one plain IPv4 address. No credentials, query or fragment.');
		o.depends('external_ip_check_enabled', '1');
		o = addValue(s, 'connection', 'external_ip_check_interval_s', _('External IPv4 lookup interval (seconds)'), 'and(uinteger,min(60),max(604800))', '3600');
		o.depends('external_ip_check_enabled', '1');
		o = addValue(s, 'connection', 'ul_if', _('Upload device'), 'string', '');
		o.depends('auto_interface_preset', '0');
		o.rmempty = true;
		o = addValue(s, 'connection', 'dl_if', _('Download IFB device'), 'string', '');
		o.depends('auto_interface_preset', '0');
		o.rmempty = true;

		o = modal(s.taboption('rates', form.ListValue, 'sqm_direction_mode', _('CAKE directions')));
		o.value('both', _('Download and upload'));
		o.value('download_only', _('Download only'));
		o.value('upload_only', _('Upload only'));
		o.default = 'both';
		o.rmempty = false;
		o.write = writeDirection;
		o = addFlag(s, 'rates', 'adjust_dl_shaper_rate', _('Adjust download'), '1');
		o.depends('sqm_direction_mode', 'both');
		o.depends('sqm_direction_mode', 'download_only');
		o = addFlag(s, 'rates', 'adjust_ul_shaper_rate', _('Adjust upload'), '1');
		o.depends('sqm_direction_mode', 'both');
		o.depends('sqm_direction_mode', 'upload_only');

		[ [ 'dl', _('Download'), '5000', '20000', '80000' ],
		  [ 'ul', _('Upload'), '5000', '20000', '35000' ] ].forEach(function(direction) {
			var key = direction[0];
			o = addValue(s, 'rates', 'min_' + key + '_shaper_rate_kbps',
				_('%s minimum').format(direction[1]), 'ufloat', direction[2]);
			o.validate = function(sectionId) { return validateRateOrder(this.section, sectionId, key); };
			o = addValue(s, 'rates', 'base_' + key + '_shaper_rate_kbps',
				_('%s base').format(direction[1]), 'ufloat', direction[3]);
			o.write = function(sectionId, value) {
				uci.set('cake-autorate', sectionId, this.option, value);
				uci.set('cake-autorate', sectionId, key === 'dl' ? 'sqm_download' : 'sqm_upload', value);
			};
			o.validate = function(sectionId) { return validateRateOrder(this.section, sectionId, key); };
			o = addValue(s, 'rates', 'max_' + key + '_shaper_rate_kbps',
				_('%s maximum').format(direction[1]), 'ufloat', direction[4]);
			o.validate = function(sectionId) { return validateRateOrder(this.section, sectionId, key); };
		});
		addValue(s, 'rates', 'connection_active_thr_kbps', _('Active traffic threshold'),
			'ufloat', '2000');
		o = addFlag(s, 'rates', 'adaptive_ceiling_enabled', _('Adaptive ceiling'), '0');
		o.description = _('Allow clean real traffic to probe upward within explicit absolute caps.');
		[ [ 'adaptive_ceiling_dl_cap_kbps', _('Download absolute cap'), '80000' ],
		  [ 'adaptive_ceiling_ul_cap_kbps', _('Upload absolute cap'), '35000' ],
		  [ 'adaptive_ceiling_hold_time_s', _('Qualification time'), '20' ],
		  [ 'adaptive_ceiling_growth_percent', _('Probe step percent'), '3' ],
		  [ 'adaptive_ceiling_probe_duration_s', _('Probe observation time'), '8' ],
		  [ 'adaptive_ceiling_cooldown_s', _('Probe cooldown'), '30' ],
		  [ 'adaptive_ceiling_failed_bound_ttl_s', _('Failed-bound memory'), '900' ]
		].forEach(function(item) {
			// The shared native candidate validator owns effective bounds and
			// cross-field rules; do not impose a separate UI timer/rate floor.
			o = addValue(s, 'rates', item[0], item[1], 'ufloat', item[2]);
			o.depends('adaptive_ceiling_enabled', '1');
		});

		addFlag(s, 'sqm', 'manage_sqm', _('Manage SQM section'), '1');
		o = addValue(s, 'sqm', 'sqm_section', _('Managed SQM section'), 'uciname', '');
		o.rmempty = true;
		o = modal(s.taboption('sqm', form.ListValue, 'sqm_qdisc', _('Queueing discipline')));
		var qdiscMode = o;
		o.value('cake', 'cake');
		o.default = 'cake';
		o.rmempty = false;
		o = modal(s.taboption('sqm', form.ListValue, 'sqm_script', _('Queue setup script')));
		addSqmScriptChoices(o, data && data[3]);
		o.default = 'piece_of_cake.qos';
		o.rmempty = false;
		var mqCheck = modal(s.taboption('sqm', form.Button, '_sqm_mq_check', _('Multi-queue capability')));
		sqmModes.bind(qdiscMode, o, mqCheck);
		o = modal(s.taboption('sqm', form.ListValue, 'sqm_linklayer', _('Link layer')));
		o.value('none', _('None'));
		o.value('ethernet', _('Ethernet'));
		o.value('atm', _('ATM'));
		o.default = 'none';
		o.rmempty = false;
		o = addValue(s, 'sqm', 'sqm_overhead', _('Per-packet overhead'),
			'and(integer,min(-1500),max(1500))', '0');
		o.depends('sqm_linklayer', 'ethernet');
		o.depends('sqm_linklayer', 'atm');
		addValue(s, 'sqm', 'sqm_iqdisc_opts', _('Ingress CAKE options'), 'string', '').rmempty = true;
		addValue(s, 'sqm', 'sqm_eqdisc_opts', _('Egress CAKE options'), 'string', '').rmempty = true;

		o = modal(s.taboption('latency', form.ListValue, 'pinger_method', _('Probe backend')));
		o.value('fping', 'fping');
		o.value('fping-ts', 'fping-ts');
		o.value('tsping', 'tsping');
		o.value('ping', _('ping fallback'));
		o.default = 'fping';
		o.rmempty = false;
		o = modal(s.taboption('latency', form.DynamicList, 'reflector', _('Reflectors')));
		o.datatype = 'host';
		o.default = DEFAULT_REFLECTORS;
		o.rmempty = false;
		addValue(s, 'latency', 'no_pingers', _('Parallel pingers'),
			'and(uinteger,min(1),max(64))', '6');
		addValue(s, 'latency', 'reflector_ping_interval_s', _('Ping interval'),
			'and(ufloat,min(0.05))', '0.3');
		o = addValue(s, 'latency', 'ping_extra_args', _('Extra ping arguments'), 'string', '');
		o.rmempty = true;
		o.description = _('Automatic binding uses the current route. User arguments are preserved; review conflicting legacy interface/source pins after changing the uplink.');

		addValue(s, 'controller', 'high_load_thr', _('High-load ratio'),
			'and(ufloat,min(0.01),max(1))', '0.75');
		addValue(s, 'controller', 'bufferbloat_detection_window', _('Detection window'),
			'and(uinteger,min(1))', '6');
		addValue(s, 'controller', 'bufferbloat_detection_thr', _('Detection threshold'),
			'and(uinteger,min(1))', '3');
		addValue(s, 'controller', 'dl_owd_delta_delay_thr_ms', _('Download delay threshold'),
			'and(ufloat,min(0))', '30');
		addValue(s, 'controller', 'ul_owd_delta_delay_thr_ms', _('Upload delay threshold'),
			'and(ufloat,min(0))', '30');
		addValue(s, 'controller', 'dl_avg_owd_delta_max_adjust_up_thr_ms',
			_('Download increase threshold'), 'and(ufloat,min(0))', '10');
		addValue(s, 'controller', 'ul_avg_owd_delta_max_adjust_up_thr_ms',
			_('Upload increase threshold'), 'and(ufloat,min(0))', '10');
		addValue(s, 'controller', 'dl_avg_owd_delta_max_adjust_down_thr_ms',
			_('Download decrease threshold'), 'and(ufloat,min(0))', '60');
		addValue(s, 'controller', 'ul_avg_owd_delta_max_adjust_down_thr_ms',
			_('Upload decrease threshold'), 'and(ufloat,min(0))', '60');
		addValue(s, 'controller', 'shaper_rate_max_adjust_down_bufferbloat',
			_('Maximum decrease factor'), 'and(ufloat,max(1))', '0.75');
		addValue(s, 'controller', 'shaper_rate_max_adjust_up_load_high',
			_('Maximum increase factor'), 'and(ufloat,min(1))', '1.04');
		addFlag(s, 'controller', 'enable_sleep_function', _('Sleep after sustained idle'), '1');
		addValue(s, 'controller', 'sustained_idle_sleep_thr_s', _('Idle time before sleep'),
			'ufloat', '60');

		return m.render();
	}
});
