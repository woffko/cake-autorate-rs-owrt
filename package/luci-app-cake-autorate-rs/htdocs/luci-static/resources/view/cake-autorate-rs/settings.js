'use strict';
'require form';
'require network';
'require uci';
'require tools.widgets as widgets';

function flag(section, tab, key, title) {
	var o = section.taboption(tab, form.Flag, key, title);
	o.rmempty = false;
	return o;
}

function value(section, tab, key, title, datatype, placeholder) {
	var o = section.taboption(tab, form.Value, key, title);
	o.rmempty = false;
	if (datatype)
		o.datatype = datatype;
	if (placeholder != null)
		o.placeholder = placeholder;
	return o;
}

function iface(section, tab, key, title) {
	var o = section.taboption(tab, widgets.DeviceSelect, key, title);
	o.noaliases = true;
	o.rmempty = false;
	return o;
}

function addRateOptions(section) {
	value(section, 'rates', 'min_dl_shaper_rate_kbps', _('Min DL rate'), 'uinteger', '5000');
	value(section, 'rates', 'base_dl_shaper_rate_kbps', _('Base DL rate'), 'uinteger', '20000');
	value(section, 'rates', 'max_dl_shaper_rate_kbps', _('Max DL rate'), 'uinteger', '80000');
	value(section, 'rates', 'min_ul_shaper_rate_kbps', _('Min UL rate'), 'uinteger', '5000');
	value(section, 'rates', 'base_ul_shaper_rate_kbps', _('Base UL rate'), 'uinteger', '20000');
	value(section, 'rates', 'max_ul_shaper_rate_kbps', _('Max UL rate'), 'uinteger', '35000');
	value(section, 'rates', 'connection_active_thr_kbps', _('Active threshold'), 'uinteger', '2000');
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
	o.value('fping', 'fping');
	o.value('fping-ts', 'fping-ts');
	o.value('tsping', 'tsping');
	o.value('irtt', 'irtt');
	o.value('ping', 'ping');
	o.rmempty = false;

	o = section.taboption('reflectors', form.DynamicList, 'reflector', _('Reflectors'));
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

return L.view.extend({
	load: function() {
		return Promise.all([
			network.getDevices(),
			uci.load('cake-autorate')
		]);
	},

	render: function() {
		var m, s;

		m = new form.Map('cake-autorate', _('CAKE Autorate'));
		s = m.section(form.GridSection, 'cake_autorate', _('Instances'));
		s.anonymous = false;
		s.addremove = true;
		s.nodescriptions = true;

		s.tab('general', _('General'));
		s.tab('interfaces', _('Interfaces'));
		s.tab('rates', _('Rates'));
		s.tab('reflectors', _('Reflectors'));
		s.tab('latency', _('Latency'));
		s.tab('controller', _('Controller'));
		s.tab('logging', _('Logging'));
		s.tab('advanced', _('Advanced'));

		flag(s, 'general', 'enabled', _('Enabled'));
		flag(s, 'general', 'adjust_dl_shaper_rate', _('Adjust DL'));
		flag(s, 'general', 'adjust_ul_shaper_rate', _('Adjust UL'));

		iface(s, 'interfaces', 'dl_if', _('Download interface'));
		iface(s, 'interfaces', 'ul_if', _('Upload interface'));

		addRateOptions(s);
		addReflectorOptions(s);
		addLatencyOptions(s);
		addControllerOptions(s);
		addLoggingOptions(s);
		addAdvancedOptions(s);

		return m.render();
	}
});
