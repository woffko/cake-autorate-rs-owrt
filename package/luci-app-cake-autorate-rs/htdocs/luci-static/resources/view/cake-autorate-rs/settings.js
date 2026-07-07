'use strict';
'require fs';
'require form';
'require network';
'require uci';
'require tools.widgets as widgets';

function modal(option) {
	option.modalonly = true;
	return option;
}

function flag(section, tab, key, title, defaultValue) {
	var o = section.taboption(tab, form.Flag, key, title);
	modal(o);
	o.rmempty = false;
	if (defaultValue != null)
		o.default = defaultValue;
	return o;
}

function value(section, tab, key, title, datatype, placeholder) {
	var o = section.taboption(tab, form.Value, key, title);
	modal(o);
	o.rmempty = false;
	if (datatype)
		o.datatype = datatype;
	if (placeholder != null)
		o.placeholder = placeholder;
	return o;
}

function optionalValue(section, tab, key, title, datatype, placeholder) {
	var o = section.taboption(tab, form.Value, key, title);
	modal(o);
	o.rmempty = true;
	if (datatype)
		o.datatype = datatype;
	if (placeholder != null)
		o.placeholder = placeholder;
	return o;
}

function iface(section, tab, key, title) {
	var o = section.taboption(tab, widgets.DeviceSelect, key, title);
	modal(o);
	o.noaliases = true;
	o.rmempty = false;
	return o;
}

function listValue(section, tab, key, title, values, defaultValue) {
	var o = section.taboption(tab, form.ListValue, key, title);
	modal(o);
	for (var i = 0; i < values.length; i++) {
		if (Array.isArray(values[i]))
			o.value(values[i][0], values[i][1]);
		else
			o.value(values[i]);
	}
	if (defaultValue != null)
		o.default = defaultValue;
	o.rmempty = false;
	return o;
}

function selectedWan(section, section_id, fallback, useFormValue) {
	if (fallback)
		return fallback;

	if (useFormValue && section && typeof section.formvalue == 'function') {
		var formValue = section.formvalue(section_id, 'wan_if');
		if (formValue)
			return formValue;
	}

	return uci.get('cake-autorate', section_id, 'wan_if') ||
		uci.get('cake-autorate', section_id, 'sqm_interface') ||
		uci.get('cake-autorate', section_id, 'ul_if') ||
		'wan';
}

function autoInterfacePresetEnabled(section, section_id) {
	var value;

	if (section && typeof section.formvalue == 'function')
		value = section.formvalue(section_id, 'auto_interface_preset');

	if (value == null)
		value = uci.get('cake-autorate', section_id, 'auto_interface_preset');

	return value !== '0';
}

function ifbForWan(wan_if) {
	return wan_if ? 'ifb4' + wan_if : '';
}

function findSqmQueueForInterface(iface) {
	var queues;

	if (!iface)
		return null;

	queues = uci.sections('sqm', 'queue') || [];
	for (var i = 0; i < queues.length; i++)
		if (queues[i].interface === iface)
			return queues[i];

	return null;
}

function rateValue(value, fallback) {
	if (value != null && value !== '')
		return String(value);

	return fallback;
}

function halfRate(value) {
	var parsed = parseInt(value, 10);

	if (!isNaN(parsed) && parsed > 0)
		return String(Math.max(1, Math.round(parsed / 2)));

	return value;
}

function applyRatePreset(section_id, wan_if, replaceExisting) {
	var queue = findSqmQueueForInterface(wan_if);
	var currentDl = uci.get('cake-autorate', section_id, 'sqm_download');
	var currentUl = uci.get('cake-autorate', section_id, 'sqm_upload');
	var dl = rateValue(queue ? queue.download : null,
		rateValue(currentDl,
			rateValue(uci.get('cake-autorate', section_id, 'base_dl_shaper_rate_kbps'), '20000')));
	var ul = rateValue(queue ? queue.upload : null,
		rateValue(currentUl,
			rateValue(uci.get('cake-autorate', section_id, 'base_ul_shaper_rate_kbps'), '20000')));

	if (replaceExisting || !currentDl)
		uci.set('cake-autorate', section_id, 'sqm_download', dl);

	if (replaceExisting || !currentUl)
		uci.set('cake-autorate', section_id, 'sqm_upload', ul);

	if (replaceExisting || !uci.get('cake-autorate', section_id, 'base_dl_shaper_rate_kbps'))
		uci.set('cake-autorate', section_id, 'base_dl_shaper_rate_kbps', dl);

	if (replaceExisting || !uci.get('cake-autorate', section_id, 'base_ul_shaper_rate_kbps'))
		uci.set('cake-autorate', section_id, 'base_ul_shaper_rate_kbps', ul);

	if (replaceExisting || !uci.get('cake-autorate', section_id, 'max_dl_shaper_rate_kbps'))
		uci.set('cake-autorate', section_id, 'max_dl_shaper_rate_kbps', dl);

	if (replaceExisting || !uci.get('cake-autorate', section_id, 'max_ul_shaper_rate_kbps'))
		uci.set('cake-autorate', section_id, 'max_ul_shaper_rate_kbps', ul);

	if (replaceExisting || !uci.get('cake-autorate', section_id, 'min_dl_shaper_rate_kbps'))
		uci.set('cake-autorate', section_id, 'min_dl_shaper_rate_kbps', halfRate(dl));

	if (replaceExisting || !uci.get('cake-autorate', section_id, 'min_ul_shaper_rate_kbps'))
		uci.set('cake-autorate', section_id, 'min_ul_shaper_rate_kbps', halfRate(ul));
}

function applyWanPreset(section_id, wan_if, importRates) {
	if (!wan_if)
		return;

	uci.set('cake-autorate', section_id, 'wan_if', wan_if);
	uci.set('cake-autorate', section_id, 'sqm_interface', wan_if);
	uci.set('cake-autorate', section_id, 'ul_if', wan_if);
	uci.set('cake-autorate', section_id, 'dl_if', ifbForWan(wan_if));

	if (importRates)
		applyRatePreset(section_id, wan_if, true);
}

function addUniqueValue(option, seen, value, title) {
	if (!value || seen[value])
		return;

	if (title != null)
		option.value(value, title);
	else
		option.value(value);

	seen[value] = true;
}

function addRateOptions(section) {
	value(section, 'rates', 'connection_active_thr_kbps', _('Active threshold'), 'uinteger', '2000');
}

function addSetupOptions(section) {
	var o;

	flag(section, 'setup', 'enabled', _('Enable autorate'));

	o = iface(section, 'setup', 'wan_if', _('Target interface'));
	o.default = 'wan';
	o.forcewrite = true;
	o.cfgvalue = function(section_id) {
		return selectedWan(null, section_id);
	};
	o.onchange = function(ev, section_id, value) {
		if (autoInterfacePresetEnabled(this.section, section_id))
			applyWanPreset(section_id, value, true);
	};
	o.write = function(section_id, formvalue) {
		var previous = selectedWan(null, section_id);
		var importRates = previous !== formvalue ||
			!uci.get('cake-autorate', section_id, 'sqm_download') ||
			!uci.get('cake-autorate', section_id, 'sqm_upload');

		uci.set('cake-autorate', section_id, 'wan_if', formvalue);

		if (autoInterfacePresetEnabled(this.section, section_id))
			applyWanPreset(section_id, formvalue, importRates);
	};

	o = flag(section, 'setup', 'auto_interface_preset', _('Auto SQM preset'), '1');
	o.forcewrite = true;
	o.write = function(section_id, formvalue) {
		uci.set('cake-autorate', section_id, 'auto_interface_preset', formvalue);

		if (formvalue === '1')
			applyWanPreset(section_id, selectedWan(this.section, section_id, null, true), false);
	};

	o = flag(section, 'setup', 'sqm_enabled', _('Enable SQM'));
	o.forcewrite = true;

	o = value(section, 'setup', 'sqm_download', _('Download speed'), 'and(uinteger,min(0))', '20000');
	o.forcewrite = true;
	o.cfgvalue = function(section_id) {
		var queue = findSqmQueueForInterface(selectedWan(null, section_id));

		return rateValue(uci.get('cake-autorate', section_id, 'sqm_download'),
			rateValue(queue ? queue.download : null,
				rateValue(uci.get('cake-autorate', section_id, 'base_dl_shaper_rate_kbps'), '20000')));
	};
	o.write = function(section_id, formvalue) {
		uci.set('cake-autorate', section_id, 'sqm_download', formvalue);
		uci.set('cake-autorate', section_id, 'base_dl_shaper_rate_kbps', formvalue);
		uci.set('cake-autorate', section_id, 'max_dl_shaper_rate_kbps', formvalue);

		if (!uci.get('cake-autorate', section_id, 'min_dl_shaper_rate_kbps'))
			uci.set('cake-autorate', section_id, 'min_dl_shaper_rate_kbps', halfRate(formvalue));
	};

	o = value(section, 'setup', 'sqm_upload', _('Upload speed'), 'and(uinteger,min(0))', '20000');
	o.forcewrite = true;
	o.cfgvalue = function(section_id) {
		var queue = findSqmQueueForInterface(selectedWan(null, section_id));

		return rateValue(uci.get('cake-autorate', section_id, 'sqm_upload'),
			rateValue(queue ? queue.upload : null,
				rateValue(uci.get('cake-autorate', section_id, 'base_ul_shaper_rate_kbps'), '20000')));
	};
	o.write = function(section_id, formvalue) {
		uci.set('cake-autorate', section_id, 'sqm_upload', formvalue);
		uci.set('cake-autorate', section_id, 'base_ul_shaper_rate_kbps', formvalue);
		uci.set('cake-autorate', section_id, 'max_ul_shaper_rate_kbps', formvalue);

		if (!uci.get('cake-autorate', section_id, 'min_ul_shaper_rate_kbps'))
			uci.set('cake-autorate', section_id, 'min_ul_shaper_rate_kbps', halfRate(formvalue));
	};

	value(section, 'setup', 'min_dl_shaper_rate_kbps', _('Min DL rate'), 'uinteger', '5000');
	value(section, 'setup', 'base_dl_shaper_rate_kbps', _('Base DL rate'), 'uinteger', '20000');
	value(section, 'setup', 'max_dl_shaper_rate_kbps', _('Max DL rate'), 'uinteger', '80000');
	value(section, 'setup', 'min_ul_shaper_rate_kbps', _('Min UL rate'), 'uinteger', '5000');
	value(section, 'setup', 'base_ul_shaper_rate_kbps', _('Base UL rate'), 'uinteger', '20000');
	value(section, 'setup', 'max_ul_shaper_rate_kbps', _('Max UL rate'), 'uinteger', '35000');
}

function addInterfaceOptions(section) {
	var o;

	o = iface(section, 'interfaces', 'dl_if', _('Download interface'));
	o.depends('auto_interface_preset', '0');

	o = iface(section, 'interfaces', 'ul_if', _('Upload interface'));
	o.depends('auto_interface_preset', '0');
}

function addLatencyOptions(section) {
	value(section, 'latency', 'dl_avg_owd_delta_max_adjust_up_thr_ms', _('DL adjust-up threshold'), 'ufloat', '10.0');
	value(section, 'latency', 'ul_avg_owd_delta_max_adjust_up_thr_ms', _('UL adjust-up threshold'), 'ufloat', '10.0');
	value(section, 'latency', 'dl_owd_delta_delay_thr_ms', _('DL delay threshold'), 'ufloat', '30.0');
	value(section, 'latency', 'ul_owd_delta_delay_thr_ms', _('UL delay threshold'), 'ufloat', '30.0');
	value(section, 'latency', 'dl_avg_owd_delta_max_adjust_down_thr_ms', _('DL adjust-down threshold'), 'ufloat', '60.0');
	value(section, 'latency', 'ul_avg_owd_delta_max_adjust_down_thr_ms', _('UL adjust-down threshold'), 'ufloat', '60.0');
}

function addControllerOptions(section) {
	value(section, 'controller', 'bufferbloat_detection_window', _('Detection window'), 'uinteger', '6');
	value(section, 'controller', 'bufferbloat_detection_thr', _('Detection threshold'), 'uinteger', '3');
	value(section, 'controller', 'alpha_baseline_increase', _('Baseline increase alpha'), 'ufloat', '0.001');
	value(section, 'controller', 'alpha_baseline_decrease', _('Baseline decrease alpha'), 'ufloat', '0.9');
	value(section, 'controller', 'alpha_delta_ewma', _('Delta EWMA alpha'), 'ufloat', '0.095');
	value(section, 'controller', 'shaper_rate_min_adjust_down_bufferbloat', _('Min down factor'), 'ufloat', '0.99');
	value(section, 'controller', 'shaper_rate_max_adjust_down_bufferbloat', _('Max down factor'), 'ufloat', '0.75');
	value(section, 'controller', 'shaper_rate_min_adjust_up_load_high', _('Min up factor'), 'ufloat', '1.0');
	value(section, 'controller', 'shaper_rate_max_adjust_up_load_high', _('Max up factor'), 'ufloat', '1.04');
	value(section, 'controller', 'shaper_rate_adjust_down_load_low', _('Low-load down factor'), 'ufloat', '0.99');
	value(section, 'controller', 'shaper_rate_adjust_up_load_low', _('Low-load up factor'), 'ufloat', '1.01');
	value(section, 'controller', 'high_load_thr', _('High-load threshold'), 'ufloat', '0.75');
	value(section, 'controller', 'bufferbloat_refractory_period_ms', _('Bufferbloat refractory'), 'uinteger', '300');
	value(section, 'controller', 'decay_refractory_period_ms', _('Decay refractory'), 'uinteger', '1000');
}

function addReflectorOptions(section) {
	var o = section.taboption('reflectors', form.ListValue, 'pinger_method', _('Pinger'));
	modal(o);
	o.value('fping', 'fping');
	o.value('fping-ts', 'fping-ts');
	o.value('tsping', 'tsping');
	o.value('irtt', 'irtt');
	o.value('ping', 'ping');
	o.rmempty = false;

	o = section.taboption('reflectors', form.DynamicList, 'reflector', _('Reflectors'));
	modal(o);
	o.datatype = 'host';
	o.rmempty = false;

	value(section, 'reflectors', 'reflectors_url', _('Reflectors URL'), 'string', '');
	value(section, 'reflectors', 'reflectors_url_skip_lines', _('URL skip lines'), 'uinteger', '1');
	flag(section, 'reflectors', 'randomize_reflectors', _('Randomize reflectors'));
	flag(section, 'reflectors', 'retain_reflector_stats', _('Retain reflector stats'));
	value(section, 'reflectors', 'no_pingers', _('Pingers'), 'uinteger', '6');
	value(section, 'reflectors', 'reflector_ping_interval_s', _('Ping interval'), 'ufloat', '0.3');
	value(section, 'reflectors', 'ping_extra_args', _('Extra ping args'), 'string', '');
	value(section, 'reflectors', 'ping_prefix_string', _('Ping prefix'), 'string', '');
	value(section, 'reflectors', 'irtt_session_duration_m', _('IRTT session minutes'), 'uinteger', '10');
}

function addLoggingOptions(section) {
	flag(section, 'logging', 'output_processing_stats', _('Processing stats'));
	flag(section, 'logging', 'output_load_stats', _('Load stats'));
	flag(section, 'logging', 'output_reflector_stats', _('Reflector stats'));
	flag(section, 'logging', 'output_summary_stats', _('Summary stats'));
	flag(section, 'logging', 'output_cake_changes', _('CAKE changes'));
	flag(section, 'logging', 'output_cpu_stats', _('CPU stats'));
	flag(section, 'logging', 'output_cpu_raw_stats', _('CPU raw stats'));
	flag(section, 'logging', 'debug', _('Debug'));
	flag(section, 'logging', 'log_DEBUG_messages_to_syslog', _('Debug to syslog'));
	flag(section, 'logging', 'log_to_file', _('Log to file'));
	value(section, 'logging', 'log_file_max_time_mins', _('Log max minutes'), 'uinteger', '10');
	value(section, 'logging', 'log_file_max_size_KB', _('Log max KB'), 'uinteger', '2000');
	value(section, 'logging', 'log_file_path_override', _('Log directory'), 'directory', '');
	value(section, 'logging', 'log_file_buffer_size_B', _('Log buffer bytes'), 'uinteger', '512');
	value(section, 'logging', 'log_file_buffer_timeout_ms', _('Log buffer timeout'), 'uinteger', '500');
	flag(section, 'logging', 'log_file_export_compress', _('Compress exports'));
}

function addAdvancedOptions(section) {
	flag(section, 'advanced', 'enable_sleep_function', _('Sleep on idle'));
	value(section, 'advanced', 'sustained_idle_sleep_thr_s', _('Idle sleep seconds'), 'ufloat', '60.0');
	flag(section, 'advanced', 'min_shaper_rates_enforcement', _('Enforce min rates'));
	value(section, 'advanced', 'startup_wait_s', _('Startup wait'), 'ufloat', '0.0');
	value(section, 'advanced', 'monitor_achieved_rates_interval_ms', _('Rate monitor interval'), 'uinteger', '200');
	value(section, 'advanced', 'monitor_cpu_usage_interval_ms', _('CPU monitor interval'), 'uinteger', '2000');
	value(section, 'advanced', 'reflector_health_check_interval_s', _('Reflector health interval'), 'ufloat', '1.0');
	value(section, 'advanced', 'reflector_response_deadline_s', _('Reflector deadline'), 'ufloat', '1.0');
	value(section, 'advanced', 'reflector_misbehaving_detection_window', _('Reflector offence window'), 'uinteger', '60');
	value(section, 'advanced', 'reflector_misbehaving_detection_thr', _('Reflector offence threshold'), 'uinteger', '3');
	value(section, 'advanced', 'reflector_replacement_interval_mins', _('Reflector replacement minutes'), 'uinteger', '60');
	value(section, 'advanced', 'reflector_comparison_interval_mins', _('Reflector comparison minutes'), 'uinteger', '1');
	value(section, 'advanced', 'reflector_sum_owd_baselines_delta_thr_ms', _('Baseline delta threshold'), 'ufloat', '20.0');
	value(section, 'advanced', 'reflector_owd_delta_ewma_delta_thr_ms', _('EWMA delta threshold'), 'ufloat', '10.0');
	value(section, 'advanced', 'stall_detection_thr', _('Stall detection threshold'), 'uinteger', '5');
	value(section, 'advanced', 'connection_stall_thr_kbps', _('Stall rate threshold'), 'uinteger', '10');
	value(section, 'advanced', 'global_ping_response_timeout_s', _('Global ping timeout'), 'ufloat', '10.0');
	value(section, 'advanced', 'if_up_check_interval_s', _('Interface check interval'), 'ufloat', '10.0');
	value(section, 'advanced', 'rx_bytes_path', _('RX bytes path'), 'file', '');
	value(section, 'advanced', 'tx_bytes_path', _('TX bytes path'), 'file', '');
}

function addSqmOptions(section, qdiscs, scripts) {
	var o, seen;

	flag(section, 'sqm_basic', 'manage_sqm', _('Manage SQM'));
	value(section, 'sqm_basic', 'sqm_section', _('SQM section'), 'uciname', '');
	o = iface(section, 'sqm_basic', 'sqm_interface', _('SQM interface'));
	o.depends('auto_interface_preset', '0');
	flag(section, 'sqm_basic', 'sqm_debug_logging', _('SQM debug logging'));
	listValue(section, 'sqm_basic', 'sqm_verbosity', _('SQM log verbosity'), [
		[ '0', 'silent' ],
		[ '1', 'error' ],
		[ '2', 'warning' ],
		[ '5', 'info' ],
		[ '8', 'debug' ],
		[ '10', 'trace' ]
	], '5');

	o = section.taboption('sqm_qdisc', form.ListValue, 'sqm_qdisc', _('Queueing discipline'));
	modal(o);
	seen = {};
	addUniqueValue(o, seen, 'cake');
	for (var i = 0; i < qdiscs.length; i++)
		addUniqueValue(o, seen, qdiscs[i].name);
	o.default = 'cake';
	o.rmempty = false;

	o = section.taboption('sqm_qdisc', form.ListValue, 'sqm_script', _('Queue setup script'));
	modal(o);
	seen = {};
	addUniqueValue(o, seen, 'piece_of_cake.qos');
	addUniqueValue(o, seen, 'cake.qos');
	for (i = 0; i < scripts.length; i++)
		addUniqueValue(o, seen, scripts[i]);
	o.default = 'piece_of_cake.qos';
	o.rmempty = false;

	o = flag(section, 'sqm_qdisc', 'sqm_qdisc_advanced', _('Advanced qdisc'));

	o = listValue(section, 'sqm_qdisc', 'sqm_squash_dscp', _('Squash DSCP'), [
		[ '1', 'SQUASH' ],
		[ '0', 'DO NOT SQUASH' ]
	], '1');
	o.depends('sqm_qdisc_advanced', '1');

	o = listValue(section, 'sqm_qdisc', 'sqm_squash_ingress', _('Ignore DSCP'), [
		[ '1', 'Ignore' ],
		[ '0', 'Allow' ]
	], '1');
	o.depends('sqm_qdisc_advanced', '1');

	o = listValue(section, 'sqm_qdisc', 'sqm_ingress_ecn', _('ECN ingress'), [ 'ECN', 'NOECN' ], 'ECN');
	o.depends('sqm_qdisc_advanced', '1');

	o = listValue(section, 'sqm_qdisc', 'sqm_egress_ecn', _('ECN egress'), [ 'NOECN', 'ECN' ], 'NOECN');
	o.depends('sqm_qdisc_advanced', '1');

	o = flag(section, 'sqm_qdisc', 'sqm_qdisc_really_really_advanced', _('Dangerous qdisc'));
	o.depends('sqm_qdisc_advanced', '1');

	o = optionalValue(section, 'sqm_qdisc', 'sqm_ilimit', _('Hard queue limit ingress'), 'and(uinteger,min(0))', '');
	o.depends('sqm_qdisc_really_really_advanced', '1');

	o = optionalValue(section, 'sqm_qdisc', 'sqm_elimit', _('Hard queue limit egress'), 'and(uinteger,min(0))', '');
	o.depends('sqm_qdisc_really_really_advanced', '1');

	o = optionalValue(section, 'sqm_qdisc', 'sqm_itarget', _('Latency target ingress'), 'string', '');
	o.depends('sqm_qdisc_really_really_advanced', '1');

	o = optionalValue(section, 'sqm_qdisc', 'sqm_etarget', _('Latency target egress'), 'string', '');
	o.depends('sqm_qdisc_really_really_advanced', '1');

	o = optionalValue(section, 'sqm_qdisc', 'sqm_iqdisc_opts', _('Qdisc options ingress'), 'string', '');
	o.depends('sqm_qdisc_really_really_advanced', '1');

	o = optionalValue(section, 'sqm_qdisc', 'sqm_eqdisc_opts', _('Qdisc options egress'), 'string', '');
	o.depends('sqm_qdisc_really_really_advanced', '1');

	listValue(section, 'sqm_linklayer', 'sqm_linklayer', _('Link layer'), [
		[ 'none', 'none' ],
		[ 'ethernet', 'ethernet' ],
		[ 'atm', 'atm' ]
	], 'none');

	o = value(section, 'sqm_linklayer', 'sqm_overhead', _('Per packet overhead'), 'and(integer,min(-1500))', '0');
	o.depends('sqm_linklayer', 'ethernet');
	o.depends('sqm_linklayer', 'atm');

	o = flag(section, 'sqm_linklayer', 'sqm_linklayer_advanced', _('Advanced link layer'));
	o.depends('sqm_linklayer', 'ethernet');
	o.depends('sqm_linklayer', 'atm');

	o = value(section, 'sqm_linklayer', 'sqm_tcMTU', _('Maximum packet size'), 'and(uinteger,min(0))', '2047');
	o.depends('sqm_linklayer_advanced', '1');

	o = value(section, 'sqm_linklayer', 'sqm_tcTSIZE', _('Rate table size'), 'and(uinteger,min(0))', '128');
	o.depends('sqm_linklayer_advanced', '1');

	o = value(section, 'sqm_linklayer', 'sqm_tcMPU', _('Minimum packet size'), 'and(uinteger,min(0))', '0');
	o.depends('sqm_linklayer_advanced', '1');

	o = listValue(section, 'sqm_linklayer', 'sqm_linklayer_adaptation_mechanism', _('Link layer mechanism'), [
		'default',
		'cake',
		'htb_private',
		'tc_stab'
	], 'default');
	o.depends('sqm_linklayer_advanced', '1');
}

function addSummaryColumns(section) {
	var o;

	o = section.option(form.DummyValue, '_enabled', _('Enabled'));
	o.cfgvalue = function(section_id) {
		return uci.get('cake-autorate', section_id, 'enabled') === '1' ? _('yes') : _('no');
	};

	o = section.option(form.DummyValue, '_wan', _('WAN'));
	o.cfgvalue = function(section_id) {
		return selectedWan(null, section_id);
	};

	o = section.option(form.DummyValue, '_sqm', _('SQM'));
	o.cfgvalue = function(section_id) {
		if (uci.get('cake-autorate', section_id, 'manage_sqm') === '0')
			return _('off');

		var wan = selectedWan(null, section_id);
		var enabled = uci.get('cake-autorate', section_id, 'sqm_enabled') === '1';

		return '%s %s'.format(enabled ? _('on') : _('off'), wan);
	};

	o = section.option(form.DummyValue, '_rates', _('Rate'));
	o.cfgvalue = function(section_id) {
		var dl = uci.get('cake-autorate', section_id, 'sqm_download') ||
			uci.get('cake-autorate', section_id, 'base_dl_shaper_rate_kbps') ||
			'0';
		var ul = uci.get('cake-autorate', section_id, 'sqm_upload') ||
			uci.get('cake-autorate', section_id, 'base_ul_shaper_rate_kbps') ||
			'0';

		return '%s/%s'.format(dl, ul);
	};
}

function loadSqmScripts() {
	return L.resolveDefault(fs.list('/usr/lib/sqm'), []).then(function(entries) {
		var scripts = [];

		for (var i = 0; i < entries.length; i++)
			if (entries[i].name.match(/\.qos$/))
				scripts.push(entries[i].name);

		return scripts;
	});
}

return L.view.extend({
	load: function() {
		return Promise.all([
			network.getDevices(),
			L.resolveDefault(fs.list('/var/run/sqm/available_qdiscs'), []),
			loadSqmScripts(),
			uci.load('cake-autorate'),
			L.resolveDefault(uci.load('sqm'), null)
		]);
	},

	render: function(data) {
		var m, s;
		var qdiscs = data[1];
		var scripts = data[2];

		m = new form.Map('cake-autorate', _('CAKE Autorate'));
		s = m.section(form.GridSection, 'cake_autorate', _('Instances'));
		s.anonymous = false;
		s.addremove = true;
		s.nodescriptions = true;

		addSummaryColumns(s);

		s.tab('setup', _('Setup'));
		s.tab('general', _('General'));
		s.tab('interfaces', _('Interfaces'));
		s.tab('sqm_basic', _('SQM Basic'));
		s.tab('sqm_qdisc', _('SQM Queue'));
		s.tab('sqm_linklayer', _('SQM Link Layer'));
		s.tab('rates', _('Rates'));
		s.tab('reflectors', _('Reflectors'));
		s.tab('latency', _('Latency'));
		s.tab('controller', _('Controller'));
		s.tab('logging', _('Logging'));
		s.tab('advanced', _('Advanced'));

		flag(s, 'general', 'adjust_dl_shaper_rate', _('Adjust DL'));
		flag(s, 'general', 'adjust_ul_shaper_rate', _('Adjust UL'));

		addSetupOptions(s);
		addInterfaceOptions(s);
		addSqmOptions(s, qdiscs, scripts);
		addRateOptions(s);
		addReflectorOptions(s);
		addLatencyOptions(s);
		addControllerOptions(s);
		addLoggingOptions(s);
		addAdvancedOptions(s);

		return m.render();
	}
});
