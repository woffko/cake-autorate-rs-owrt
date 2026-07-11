'use strict';
'require fs';
'require form';
'require network';
'require uci';
'require ui';
'require tools.widgets as widgets';

function modal(option) {
	option.modalonly = true;
	return option;
}

var optionDescriptions = {
	enabled: 'Start the autorate daemon for this instance when the service runs.',
	adjust_dl_shaper_rate: 'Allow autorate to change the download CAKE bandwidth.',
	adjust_ul_shaper_rate: 'Allow autorate to change the upload CAKE bandwidth.',
	wan_if: 'Main WAN interface for this instance. Auto preset also uses it for SQM and IFB setup.',
	auto_interface_preset: 'Automatically derive SQM interface, upload interface, and download IFB from the target interface.',
	sqm_enabled: 'Enable the matching SQM queue managed by this instance.',
	sqm_download: 'SQM download bandwidth in kbit/s. This also seeds the autorate base and max download rates.',
	sqm_upload: 'SQM upload bandwidth in kbit/s. This also seeds the autorate base and max upload rates.',
	speedtest_apply_percent: 'Percentage of measured throughput to write into SQM and autorate limits. 90 leaves headroom for CAKE.',
	_speedtest: 'Run a router-side speed test and fill SQM plus autorate limits from the measured throughput.',
	speedtest_bind_interface: 'Try to run the speed test through the selected target interface. Uses curl --interface when curl is installed, otherwise checks the route used by the built-in fetcher.',
	speedtest_force_ipv4: 'Force IPv4 for the built-in HTTP speed test. This keeps route checks predictable on simple WAN setups.',
	speedtest_route_probe: 'Address used to check which interface the router would use for the speed test when hard binding is unavailable.',
	speedtest_download_url: 'Optional download URL for the built-in HTTP speed test. Leave empty to use Cloudflare speed test.',
	speedtest_upload_url: 'Optional upload URL for the built-in HTTP speed test. Leave empty to use Cloudflare speed test.',
	speedtest_download_bytes: 'Download payload size requested by the built-in HTTP speed test.',
	speedtest_upload_bytes: 'Initial upload payload size sent by the built-in HTTP speed test. Set to 0 to skip upload testing.',
	speedtest_upload_retry_bytes: 'Space-separated smaller upload payload sizes to try if the initial upload test fails.',
	speedtest_timeout_s: 'Per-request timeout for built-in speed test download and upload requests.',
	speedtest_backend: 'Speed test backend preference. Auto tries optional CLI backends first and falls back to the built-in HTTP test. A forced backend must be installed and configured.',
	speedtest_duration_s: 'Test duration in seconds for optional CLI backends that support a duration setting.',
	speedtest_iperf3_server: 'Optional iperf3 server host or address. iperf3 is only used when this is set and the iperf3 package is installed.',
	speedtest_iperf3_port: 'Optional iperf3 server port. Leave empty to use the iperf3 default.',
	_speedtest_backend_order: 'Backend autodetect order used by the speed test helper.',
	_speedtest_backend_status: 'Check which optional speed test backends are currently installed or configured on this router.',
	_speedtest_backend_install: 'Install the selected optional backend package on this router. Auto and built-in HTTP do not need installation.',
	_wizard_sqm_queue: 'Existing unmanaged SQM queues on the selected interface are reused to avoid duplicate shapers.',
	manual_rate_limits: 'Show explicit min, base, and max autorate limits. Leave off to derive them from download and upload speeds.',
	advanced_settings: 'Show detailed SQM, reflector, controller, logging, and daemon tuning settings.',
	min_dl_shaper_rate_kbps: 'Lowest download shaper rate autorate may apply, in kbit/s.',
	base_dl_shaper_rate_kbps: 'Starting download shaper rate before autorate adjusts it, in kbit/s.',
	max_dl_shaper_rate_kbps: 'Highest download shaper rate autorate may apply, in kbit/s.',
	min_ul_shaper_rate_kbps: 'Lowest upload shaper rate autorate may apply, in kbit/s.',
	base_ul_shaper_rate_kbps: 'Starting upload shaper rate before autorate adjusts it, in kbit/s.',
	max_ul_shaper_rate_kbps: 'Highest upload shaper rate autorate may apply, in kbit/s.',
	dl_if: 'Interface whose RX byte counter represents shaped download traffic, usually the IFB created by SQM.',
	ul_if: 'Interface whose TX byte counter represents upload traffic, usually the WAN device.',
	manage_sqm: 'Mirror this instance into /etc/config/sqm and restart SQM before autorate starts.',
	sqm_section: 'Name of the managed SQM queue section. Leave empty to use cake_<instance>.',
	sqm_interface: 'Network device where SQM should attach the CAKE queue.',
	sqm_debug_logging: 'Enable SQM script debug logging for this queue.',
	sqm_verbosity: 'Verbosity level passed to SQM scripts.',
	sqm_qdisc: 'Queueing discipline used by SQM. CAKE is the recommended default.',
	sqm_script: 'SQM setup script that builds the traffic control rules.',
	sqm_qdisc_advanced: 'Show DSCP and ECN queueing options from luci-app-sqm.',
	sqm_squash_dscp: 'Rewrite DSCP markings before packets enter the shaper.',
	sqm_squash_ingress: 'Ignore DSCP markings on ingress traffic.',
	sqm_ingress_ecn: 'Enable or disable ECN handling for ingress traffic.',
	sqm_egress_ecn: 'Enable or disable ECN handling for egress traffic.',
	sqm_qdisc_really_really_advanced: 'Show raw qdisc limits and options. Use only when you know the SQM script expects them.',
	sqm_ilimit: 'Optional hard queue limit for ingress, passed through to SQM.',
	sqm_elimit: 'Optional hard queue limit for egress, passed through to SQM.',
	sqm_itarget: 'Optional ingress latency target passed through to SQM.',
	sqm_etarget: 'Optional egress latency target passed through to SQM.',
	sqm_iqdisc_opts: 'Extra raw qdisc options for ingress. Invalid options can break SQM startup.',
	sqm_eqdisc_opts: 'Extra raw qdisc options for egress. Invalid options can break SQM startup.',
	sqm_linklayer: 'Link layer model used by SQM to account for packet overhead.',
	sqm_overhead: 'Per-packet overhead in bytes for the selected link layer.',
	sqm_linklayer_advanced: 'Show advanced link layer table and minimum packet size controls.',
	sqm_tcMTU: 'Maximum packet size used when SQM builds link layer rate tables.',
	sqm_tcTSIZE: 'Rate table size used by SQM link layer compensation.',
	sqm_tcMPU: 'Minimum packet unit in bytes for link layer compensation.',
	sqm_linklayer_adaptation_mechanism: 'Mechanism SQM uses to apply link layer overhead compensation.',
	connection_active_thr_kbps: 'Traffic rate above which the connection is treated as active, in kbit/s.',
	dl_avg_owd_delta_max_adjust_up_thr_ms: 'Download delay delta below which autorate may increase the download shaper.',
	ul_avg_owd_delta_max_adjust_up_thr_ms: 'Upload delay delta below which autorate may increase the upload shaper.',
	dl_owd_delta_delay_thr_ms: 'Download delay delta considered bufferbloat for detection.',
	ul_owd_delta_delay_thr_ms: 'Upload delay delta considered bufferbloat for detection.',
	dl_avg_owd_delta_max_adjust_down_thr_ms: 'Download delay delta at which autorate backs off more aggressively.',
	ul_avg_owd_delta_max_adjust_down_thr_ms: 'Upload delay delta at which autorate backs off more aggressively.',
	bufferbloat_detection_window: 'Number of recent samples considered for bufferbloat detection.',
	bufferbloat_detection_thr: 'Samples within the detection window that must exceed delay thresholds.',
	alpha_baseline_increase: 'EWMA factor for slowly increasing the delay baseline.',
	alpha_baseline_decrease: 'EWMA factor for lowering the delay baseline after better samples.',
	alpha_delta_ewma: 'EWMA factor for smoothing delay deltas from reflectors.',
	shaper_rate_min_adjust_down_bufferbloat: 'Smallest multiplicative backoff used when bufferbloat is detected.',
	shaper_rate_max_adjust_down_bufferbloat: 'Largest multiplicative backoff used when bufferbloat is severe.',
	shaper_rate_min_adjust_up_load_high: 'Minimum multiplicative increase while load is high and delay is acceptable.',
	shaper_rate_max_adjust_up_load_high: 'Maximum multiplicative increase while load is high and delay is acceptable.',
	shaper_rate_adjust_down_load_low: 'Multiplicative decay used while load is low.',
	shaper_rate_adjust_up_load_low: 'Multiplicative increase used while load is low and delay is clean.',
	high_load_thr: 'Fraction of current shaper rate that counts as high load.',
	bufferbloat_refractory_period_ms: 'Minimum time after a bufferbloat response before another backoff may happen.',
	decay_refractory_period_ms: 'Minimum time between low-load decay adjustments.',
	pinger_method: 'Probe backend used to measure reflector latency. fping supports concurrent RTT reflectors; fping-ts and tsping use ICMP timestamp OWD probes; irtt uses explicit IRTT servers with synchronized clocks; ping is a basic fallback using one ping process per active reflector.',
	_pinger_backend_status: 'Show which pinger binaries are available and which backend the planner would prefer.',
	_pinger_backend_install: 'Install the package for the selected pinger when automatic installation is supported. tsping remains a manual binary install; irtt also needs explicit IRTT servers and NTP-synchronized clocks.',
	_reflector_scan: 'Probe configured reflectors plus the upstream default pool, classify timestamp support, and suggest an active set plus spare pool.',
	_reflector_apply: 'Scan configured reflectors plus upstream defaults, then write the recommended pinger, active count, and ordered active plus spare reflector list into pending changes.',
	_wizard_reflector_plan: 'Scan the upstream default reflector pool and fill the new instance with the recommended pinger and active/spare reflector set.',
	reflector: 'Hosts to probe for latency. Defaults match the upstream cake-autorate anycast reflector pool.',
	irtt_server: 'Explicit IRTT server hosts or addresses. These are used only when Pinger is set to irtt; the router and servers need synchronized clocks so one-way delays are valid.',
	reflectors_url: 'Optional URL to fetch reflector candidates from at daemon startup. Falls back to the configured list if the URL is unavailable.',
	reflectors_url_skip_lines: 'Number of header lines to skip when parsing reflector URL data.',
	randomize_reflectors: 'Shuffle reflector order before selecting active probes.',
	retain_reflector_stats: 'Keep reflector statistics when replacing or restarting probes.',
	no_pingers: 'Number of concurrent reflector probes to run.',
	reflector_ping_interval_s: 'Seconds between pings sent by each reflector probe.',
	ping_extra_args: 'Additional safe arguments passed to pingers. In multi-WAN setups, upstream cake-autorate requires using this or Ping prefix so probes leave through the target interface, for example -I eth2.',
	ping_prefix_string: 'Optional command prefix for launching pingers, for example mwan3 use wan2 exec. Use this instead of Extra ping args when policy-routing wrappers should select the uplink.',
	irtt_session_duration_m: 'Duration of each IRTT client session in minutes. Longer sessions reduce restart gaps but use more memory inside irtt.',
	output_processing_stats: 'Log detailed controller processing statistics.',
	output_load_stats: 'Log achieved load and traffic rate statistics.',
	output_reflector_stats: 'Log per-reflector latency statistics.',
	output_summary_stats: 'Log compact periodic summaries.',
	output_cake_changes: 'Log every CAKE bandwidth change command.',
	output_cpu_stats: 'Log CPU usage summaries and expose the latest total CPU percentage in status.',
	output_cpu_raw_stats: 'Log raw /proc/stat CPU counter lines for diagnostics.',
	debug: 'Enable extra debug output from the daemon.',
	log_DEBUG_messages_to_syslog: 'Send debug messages to syslog instead of only the normal log path.',
	log_to_file: 'Write daemon logs to files in addition to stdout/syslog.',
	log_file_max_time_mins: 'Maximum age of a log file before rotation, in minutes.',
	log_file_max_size_KB: 'Maximum log file size before rotation, in KiB.',
	log_file_path_override: 'Directory for daemon log files. Leave empty for the default path.',
	log_file_buffer_size_B: 'Buffered log write size in bytes.',
	log_file_buffer_timeout_ms: 'Maximum time before flushing buffered log output.',
	log_file_export_compress: 'Compress rotated daemon logs with gzip when available.',
	mqtt_enabled: 'Start a separate MQTT publisher for this instance. It reads daemon log files and publishes Home Assistant discovery and status through mosquitto_pub.',
	mqtt_host: 'MQTT broker host or address. Required only when the MQTT publisher is enabled.',
	mqtt_port: 'MQTT broker port. Leave empty to use 1883.',
	mqtt_username: 'Optional MQTT broker username.',
	mqtt_password: 'Optional MQTT broker password.',
	mqtt_discovery_prefix: 'Home Assistant MQTT discovery prefix.',
	mqtt_base_topic: 'Base MQTT topic used for instance state and availability.',
	mqtt_device_id: 'Home Assistant device identifier prefix. The instance name is appended automatically.',
	mqtt_device_name: 'Home Assistant device display name prefix. The instance name is appended automatically.',
	mqtt_min_interval_s: 'Minimum seconds between MQTT state publications.',
	mqtt_publish_cpu_stats: 'Publish CPU sensors through MQTT. Requires CPU stats logging.',
	_mqtt_status: 'Check whether the MQTT client package and required saved log settings are ready for this instance. Save pending MQTT edits before relying on this status.',
	_mqtt_install: 'Install the default MQTT client package. The publisher needs mosquitto_pub from mosquitto-client-nossl or mosquitto-client-ssl.',
	enable_sleep_function: 'Allow the controller to sleep during sustained idle periods.',
	sustained_idle_sleep_thr_s: 'Idle duration before sleep behavior may engage.',
	min_shaper_rates_enforcement: 'Prevent shaper rates from dropping below configured minimums.',
	startup_wait_s: 'Delay after service start before probing and adjusting rates.',
	monitor_achieved_rates_interval_ms: 'Interval for sampling interface byte counters.',
	monitor_cpu_usage_interval_ms: 'Interval for CPU usage sampling.',
	reflector_health_check_interval_s: 'Interval between reflector health checks.',
	reflector_response_deadline_s: 'Maximum acceptable reflector response time before it is considered late.',
	reflector_misbehaving_detection_window: 'Number of recent health samples used to detect bad reflectors.',
	reflector_misbehaving_detection_thr: 'Bad samples required before a reflector is treated as misbehaving.',
	reflector_replacement_interval_mins: 'How often eligible reflectors may be replaced.',
	reflector_comparison_interval_mins: 'How often active reflectors are compared against alternatives.',
	reflector_sum_owd_baselines_delta_thr_ms: 'Baseline delay difference threshold for reflector comparison.',
	reflector_owd_delta_ewma_delta_thr_ms: 'EWMA delay delta threshold for reflector comparison.',
	stall_detection_thr: 'Consecutive failed or stalled samples required to detect a stall.',
	connection_stall_thr_kbps: 'Traffic rate below which the connection may be considered stalled.',
	global_ping_response_timeout_s: 'Global timeout for ping responses before a probe is considered failed.',
	if_up_check_interval_s: 'Interval for checking whether configured interfaces are up.',
	rx_bytes_path: 'Override path for the download RX byte counter. Leave empty to use /sys/class/net.',
	tx_bytes_path: 'Override path for the upload TX byte counter. Leave empty to use /sys/class/net.'
};

var interfaceContext = {
	deviceNames: {},
	networkDevices: {},
	defaultDevice: 'wan'
};

var speedtestLastResults = {};

function describe(option, key) {
	var description = optionDescriptions[key];

	if (description)
		option.description = _(description);

	return option;
}

function flag(section, tab, key, title, defaultValue) {
	var o = section.taboption(tab, form.Flag, key, title);
	modal(o);
	describe(o, key);
	o.rmempty = false;
	if (defaultValue != null)
		o.default = defaultValue;
	return o;
}

function value(section, tab, key, title, datatype, placeholder) {
	var o = section.taboption(tab, form.Value, key, title);
	modal(o);
	describe(o, key);
	o.rmempty = false;
	if (datatype)
		o.datatype = datatype;
	if (placeholder != null) {
		o.default = placeholder;
		o.placeholder = placeholder;
	}
	return o;
}

function optionalValue(section, tab, key, title, datatype, placeholder) {
	var o = section.taboption(tab, form.Value, key, title);
	modal(o);
	describe(o, key);
	o.rmempty = true;
	if (datatype)
		o.datatype = datatype;
	if (placeholder != null)
		o.placeholder = placeholder;
	return o;
}

function dependsManagedSqm(option, extra) {
	var deps = { manage_sqm: '1' };
	var extraDeps = extra || {};

	for (var key in extraDeps)
		if (extraDeps.hasOwnProperty(key))
			deps[key] = extraDeps[key];

	option.depends(deps);
	return option;
}

function dependsAny(option, key, values, extra) {
	var extraDeps = extra || {};

	for (var i = 0; i < values.length; i++) {
		var deps = {};

		deps[key] = values[i];
		for (var extraKey in extraDeps)
			if (extraDeps.hasOwnProperty(extraKey))
				deps[extraKey] = extraDeps[extraKey];

		option.depends(deps);
	}

	return option;
}

function iface(section, tab, key, title) {
	var o = section.taboption(tab, widgets.DeviceSelect, key, title);
	modal(o);
	describe(o, key);
	o.noaliases = true;
	o.rmempty = false;
	return o;
}

function buildInterfaceContext(devices, networks) {
	var ctx = {
		deviceNames: {},
		networkDevices: {},
		defaultDevice: null
	};

	for (var i = 0; i < devices.length; i++) {
		var devName = devices[i].getName();
		var devType = devices[i].getType();

		if (!devName || devName === 'lo' || devType === 'alias')
			continue;

		ctx.deviceNames[devName] = true;

		if (!ctx.defaultDevice)
			ctx.defaultDevice = devName;

		if (devices[i].isUp && devices[i].isUp() && !ctx.firstUpDevice)
			ctx.firstUpDevice = devName;
	}

	for (i = 0; i < networks.length; i++) {
		var netName = networks[i].getName();
		var ifName = networks[i].getIfname();

		if (!netName || !ifName)
			continue;

		if (ifName.charAt(0) === '@')
			ifName = ifName.substring(1);

		ctx.networkDevices[netName] = ifName;
	}

	ctx.defaultDevice = ctx.networkDevices.wan ||
		ctx.networkDevices.wwan ||
		ctx.networkDevices.wan6 ||
		ctx.firstUpDevice ||
		ctx.defaultDevice ||
		'wan';

	return ctx;
}

function normalizeInterfaceName(name) {
	var mapped;

	if (!name)
		return name;

	if (name.charAt(0) === '@')
		name = name.substring(1);

	mapped = interfaceContext.networkDevices[name];
	if (mapped && mapped !== name)
		return normalizeInterfaceName(mapped);

	return name;
}

function defaultTargetInterface() {
	return normalizeInterfaceName(interfaceContext.defaultDevice || 'wan');
}

function listValue(section, tab, key, title, values, defaultValue) {
	var o = section.taboption(tab, form.ListValue, key, title);
	modal(o);
	describe(o, key);
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
		return normalizeInterfaceName(fallback);

	if (useFormValue && section && typeof section.formvalue == 'function') {
		var formValue = section.formvalue(section_id, 'wan_if');
		if (formValue)
			return normalizeInterfaceName(formValue);
	}

	return normalizeInterfaceName(uci.get('cake-autorate', section_id, 'wan_if') ||
		uci.get('cake-autorate', section_id, 'sqm_interface') ||
		uci.get('cake-autorate', section_id, 'ul_if') ||
		defaultTargetInterface());
}

function autoInterfacePresetEnabled(section, section_id) {
	var value;

	if (section && typeof section.formvalue == 'function')
		value = section.formvalue(section_id, 'auto_interface_preset');

	if (value == null)
		value = uci.get('cake-autorate', section_id, 'auto_interface_preset');

	return value !== '0';
}

function manualRateLimitsEnabled(section, section_id) {
	var value;

	if (section && typeof section.formvalue == 'function')
		value = section.formvalue(section_id, 'manual_rate_limits');

	if (value == null)
		value = uci.get('cake-autorate', section_id, 'manual_rate_limits');

	return value === '1';
}

function formOrUci(section, section_id, key) {
	var element, value;

	if (section && typeof section.getUIElement == 'function') {
		element = section.getUIElement(section_id, key);

		if (element && typeof element.getValue == 'function')
			value = element.getValue();

		if (value == null && element && typeof element.isChecked == 'function')
			value = element.isChecked() ? '1' : '0';
	}

	if ((value == null || value === '') && section && typeof section.formvalue == 'function')
		value = section.formvalue(section_id, key);

	if (value == null || value === '')
		value = uci.get('cake-autorate', section_id, key);

	return value;
}

function listFormOrUci(section, section_id, key) {
	var value = formOrUci(section, section_id, key);

	if (Array.isArray(value))
		return value.filter(function(item) {
			return item != null && item !== '';
		}).map(String);

	if (value == null || value === '')
		return [];

	return String(value).split(/\s+/).filter(function(item) {
		return item !== '';
	});
}

function validationSection(option) {
	if (option && option.section)
		return option.section;

	if (option && option.map && option.map.children) {
		for (var i = 0; i < option.map.children.length; i++)
			if (typeof option.map.children[i].formvalue == 'function')
				return option.map.children[i];
	}

	return null;
}

function checkedFormOrUci(section, section_id, key, fallback) {
	var value = formOrUci(section, section_id, key);

	if (value == null || value === '')
		return fallback;

	return value === '1';
}

function checkedFromEvent(ev, value) {
	if (value === true || value === '1' || value === 1 || value === 'on')
		return true;

	if (value === false || value === '0' || value === 0 || value === 'off')
		return false;

	if (ev && ev.target && typeof ev.target.checked == 'boolean')
		return ev.target.checked;

	if (ev && ev.currentTarget && typeof ev.currentTarget.checked == 'boolean')
		return ev.currentTarget.checked;

	return false;
}

function ifbForWan(wan_if) {
	return wan_if ? 'ifb4' + wan_if : '';
}

function pingerSupportsInterfaceArg(method) {
	return method !== 'irtt';
}

function pingerInterfaceArgs(wan_if, method) {
	wan_if = normalizeInterfaceName(wan_if);
	method = method || 'fping';

	if (!wan_if || !pingerSupportsInterfaceArg(method))
		return '';

	return '-I ' + wan_if;
}

function generatedPingerInterfaceArgs(value) {
	return /^-I [A-Za-z0-9_.:-]+$/.test(value || '');
}

function maybeSetPingerInterfaceArgs(section, section_id, wan_if, method) {
	var currentArgs = formOrUci(section, section_id, 'ping_extra_args');
	var currentPrefix = formOrUci(section, section_id, 'ping_prefix_string');
	var args;

	if (currentPrefix || (currentArgs && !generatedPingerInterfaceArgs(currentArgs)))
		return;

	args = pingerInterfaceArgs(wan_if, method || formOrUci(section, section_id, 'pinger_method') || 'fping');

	if (args)
		setCakeOption(section, section_id, 'ping_extra_args', args);
}

function parsePositiveRate(value) {
	var parsed;

	if (value == null || value === '')
		return null;

	parsed = parseInt(value, 10);
	return isNaN(parsed) || parsed < 0 ? null : parsed;
}

function validateRateOrder(section, section_id, direction) {
	var min = parsePositiveRate(formOrUci(section, section_id, 'min_' + direction + '_shaper_rate_kbps'));
	var base = parsePositiveRate(formOrUci(section, section_id, 'base_' + direction + '_shaper_rate_kbps'));
	var max = parsePositiveRate(formOrUci(section, section_id, 'max_' + direction + '_shaper_rate_kbps'));
	var label = direction === 'dl' ? _('download') : _('upload');

	if (min == null || base == null || max == null)
		return true;

	if (min > base)
		return _('Minimum %s rate must not exceed the base rate.').format(label);

	if (base > max)
		return _('Base %s rate must not exceed the maximum rate.').format(label);

	return true;
}

function validateDifferentInterfaces(section, section_id) {
	var dl = normalizeInterfaceName(formOrUci(section, section_id, 'dl_if'));
	var ul = normalizeInterfaceName(formOrUci(section, section_id, 'ul_if'));

	if (!dl || !ul || dl !== ul)
		return true;

	return _('Download and upload interfaces must be different.');
}

function validatePingerCount(section, section_id) {
	var method = formOrUci(section, section_id, 'pinger_method') || 'fping';
	var count = parseInt(formOrUci(section, section_id, 'no_pingers') || '6', 10);
	var irttServers;

	if (method === 'irtt') {
		irttServers = listFormOrUci(section, section_id, 'irtt_server');

		if (!irttServers.length)
			return _('IRTT requires at least one explicit IRTT server.');

		if (!isNaN(count) && count > irttServers.length)
			return _('IRTT Pingers cannot exceed the configured IRTT server count.');

		return true;
	}

	return true;
}

function validateIrttServerValue(value) {
	var values = Array.isArray(value) ? value : [ value ];

	for (var i = 0; i < values.length; i++) {
		var item = values[i];

		if (item == null || item === '')
			continue;

		if (!/^[0-9A-Za-z:._\[\]-]+$/.test(String(item)))
			return _('IRTT servers may contain only host, IPv4, IPv6, and optional port characters.');
	}

	return true;
}

function selectedSqmSection(section, section_id) {
	return formOrUci(section, section_id, 'sqm_section') || managedSqmSectionName(section_id);
}

function validateSqmSectionUnique(section, section_id) {
	var manage = checkedFormOrUci(section, section_id, 'manage_sqm', true);
	var target = selectedSqmSection(section, section_id);
	var sections;

	if (!manage || !target)
		return true;

	sections = uci.sections('cake-autorate', 'cake_autorate') || [];
	for (var i = 0; i < sections.length; i++) {
		var other = sections[i]['.name'];
		var otherManage;
		var otherTarget;

		if (!other || other === section_id)
			continue;

		otherManage = sections[i].manage_sqm !== '0';
		if (!otherManage)
			continue;

		otherTarget = sections[i].sqm_section || managedSqmSectionName(other);
		if (otherTarget === target)
			return _('SQM section "%s" is already managed by instance "%s".').format(target, other);
	}

	return true;
}

function validateMqttConfig(section, section_id) {
	if (!checkedFormOrUci(section, section_id, 'mqtt_enabled', false))
		return true;

	if (!formOrUci(section, section_id, 'mqtt_host'))
		return _('MQTT broker host is required when MQTT publisher is enabled.');

	if (!checkedFormOrUci(section, section_id, 'log_to_file', true))
		return _('MQTT publisher needs Log to file enabled because it reads SUMMARY/CPU records from daemon log files.');

	if (!checkedFormOrUci(section, section_id, 'output_summary_stats', false))
		return _('MQTT publisher needs Summary stats enabled.');

	if (checkedFormOrUci(section, section_id, 'mqtt_publish_cpu_stats', false) &&
	    !checkedFormOrUci(section, section_id, 'output_cpu_stats', false))
		return _('MQTT CPU sensors need CPU stats enabled.');

	return true;
}

function hasEnabledSqmBacking(section, section_id) {
	var manage = checkedFormOrUci(section, section_id, 'manage_sqm', true);
	var enabled = checkedFormOrUci(section, section_id, 'enabled', false);
	var sqmEnabled = checkedFormOrUci(section, section_id, 'sqm_enabled', enabled);
	var selected = formOrUci(section, section_id, 'sqm_section');
	var queue;

	if (manage)
		return sqmEnabled;

	if (selected)
		return uci.get('sqm', selected, 'enabled') === '1';

	queue = findSqmQueueForInterface(selectedWan(section, section_id, null, true));

	return Boolean(queue && queue.enabled === '1');
}

function validateInterfaceBacking(section, section_id) {
	var enabled = checkedFormOrUci(section, section_id, 'enabled', false);

	if (!enabled || !checkedFormOrUci(section, section_id, 'auto_interface_preset', true))
		return true;

	if (hasEnabledSqmBacking(section, section_id))
		return true;

	return _('Auto SQM preset uses an IFB download interface. Enable SQM for this instance or use an already enabled SQM queue before enabling autorate.');
}

function validateInstanceSection(section, section_id) {
	var result;

	if (checkedFormOrUci(section, section_id, 'manual_rate_limits', false)) {
		result = validateRateOrder(section, section_id, 'dl');
		if (result !== true)
			return result;

		result = validateRateOrder(section, section_id, 'ul');
		if (result !== true)
			return result;
	}

	if (!checkedFormOrUci(section, section_id, 'auto_interface_preset', true)) {
		result = validateDifferentInterfaces(section, section_id);
		if (result !== true)
			return result;
	}

	result = validatePingerCount(section, section_id);
	if (result !== true)
		return result;

	result = validateSqmSectionUnique(section, section_id);
	if (result !== true)
		return result;

	result = validateInterfaceBacking(section, section_id);
	if (result !== true)
		return result;

	result = validateMqttConfig(section, section_id);
	if (result !== true)
		return result;

	return true;
}

function findSqmQueueForInterface(iface) {
	var queues, fallback = null;

	if (!iface)
		return null;

	iface = normalizeInterfaceName(iface);

	queues = uci.sections('sqm', 'queue') || [];
	for (var i = 0; i < queues.length; i++) {
		if (normalizeInterfaceName(queues[i].interface) !== iface)
			continue;

		if (!queues[i]._cake_autorate_managed)
			return queues[i];

		if (!fallback)
			fallback = queues[i];
	}

	return fallback;
}

function rateValue(value, fallback) {
	if (value != null && value !== '')
		return String(value);

	return fallback;
}

function optionByName(section, key) {
	if (!section || !section.children)
		return null;

	for (var i = 0; i < section.children.length; i++)
		if (section.children[i].option === key)
			return section.children[i];

	return null;
}

function setFormOptionValue(section, section_id, key, value) {
	var option = optionByName(section, key);
	var element;

	if (!option || typeof option.getUIElement != 'function')
		return;

	element = option.getUIElement(section_id);
	if (element && typeof element.setValue == 'function')
		element.setValue(Array.isArray(value) ? value : String(value));
}

function setCakeOption(section, section_id, key, value) {
	value = String(value);

	uci.set('cake-autorate', section_id, key, value);
	setFormOptionValue(section, section_id, key, value);
}

function setCakeListOption(section, section_id, key, values) {
	values = (values || []).filter(function(value) {
		return value != null && value !== '';
	}).map(String);

	uci.set('cake-autorate', section_id, key, values);
	setFormOptionValue(section, section_id, key, values);
}

function halfRate(value) {
	var parsed = parseInt(value, 10);

	if (!isNaN(parsed) && parsed > 0)
		return String(Math.max(1, Math.round(parsed / 2)));

	return value;
}

function applyRatePreset(section_id, wan_if, replaceExisting, section) {
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
		setCakeOption(section, section_id, 'sqm_download', dl);

	if (replaceExisting || !currentUl)
		setCakeOption(section, section_id, 'sqm_upload', ul);

	if (replaceExisting || !uci.get('cake-autorate', section_id, 'base_dl_shaper_rate_kbps'))
		setCakeOption(section, section_id, 'base_dl_shaper_rate_kbps', dl);

	if (replaceExisting || !uci.get('cake-autorate', section_id, 'base_ul_shaper_rate_kbps'))
		setCakeOption(section, section_id, 'base_ul_shaper_rate_kbps', ul);

	if (replaceExisting || !uci.get('cake-autorate', section_id, 'max_dl_shaper_rate_kbps'))
		setCakeOption(section, section_id, 'max_dl_shaper_rate_kbps', dl);

	if (replaceExisting || !uci.get('cake-autorate', section_id, 'max_ul_shaper_rate_kbps'))
		setCakeOption(section, section_id, 'max_ul_shaper_rate_kbps', ul);

	if (replaceExisting || !uci.get('cake-autorate', section_id, 'min_dl_shaper_rate_kbps'))
		setCakeOption(section, section_id, 'min_dl_shaper_rate_kbps', halfRate(dl));

	if (replaceExisting || !uci.get('cake-autorate', section_id, 'min_ul_shaper_rate_kbps'))
		setCakeOption(section, section_id, 'min_ul_shaper_rate_kbps', halfRate(ul));
}

function applyWanPreset(section_id, wan_if, importRates, section) {
	wan_if = normalizeInterfaceName(wan_if);

	if (!wan_if)
		return;

	setCakeOption(section, section_id, 'wan_if', wan_if);
	setCakeOption(section, section_id, 'sqm_interface', wan_if);
	setCakeOption(section, section_id, 'ul_if', wan_if);
	setCakeOption(section, section_id, 'dl_if', ifbForWan(wan_if));
	maybeSetPingerInterfaceArgs(section, section_id, wan_if);
	applySqmSectionPreset(section_id, wan_if, importRates, section);

	if (importRates)
		applyRatePreset(section_id, wan_if, true, section);
}

function maybeEnableSqmForAutoPreset(section, section_id, enabledOverride) {
	var enabled = enabledOverride != null ?
		(enabledOverride === true || enabledOverride === '1') :
		checkedFormOrUci(section, section_id, 'enabled', false);

	if (!enabled ||
	    !autoInterfacePresetEnabled(section, section_id) ||
	    !checkedFormOrUci(section, section_id, 'manage_sqm', true))
		return;

	setCakeOption(section, section_id, 'manage_sqm', '1');
	setCakeOption(section, section_id, 'sqm_enabled', '1');
}

function speedtestApplyPercent(section, section_id) {
	var value;
	var percent;

	if (section && typeof section.formvalue == 'function')
		value = section.formvalue(section_id, 'speedtest_apply_percent');

	if (value == null || value === '')
		value = uci.get('cake-autorate', section_id, 'speedtest_apply_percent');

	percent = parseInt(value || '90', 10);
	if (isNaN(percent) || percent < 1 || percent > 100)
		percent = 90;

	return percent;
}

function measuredRate(value, percent) {
	value = parseInt(value, 10);

	if (isNaN(value) || value <= 0)
		return null;

	return String(Math.max(1, Math.round(value * percent / 100)));
}

function parseSpeedtestResult(stdout) {
	var result = JSON.parse((stdout || '').trim());

	if (!result || (!result.download_kbps && !result.upload_kbps))
		throw new Error(_('Speed test returned no usable rate.'));

	return result;
}

function speedtestBackendTitle(result) {
	return result.backend_title || result.backend || result.source || _('selected backend');
}

function speedtestBackendChoices() {
	return [
		[ 'auto', _('Auto') ],
		[ 'librespeed-cli', _('LibreSpeed CLI (package: librespeed-cli)') ],
		[ 'speedtest-go', _('speedtest-go (package: speedtest-go)') ],
		[ 'iperf3', _('configured iperf3 (package: iperf3)') ],
		[ 'builtin-http', _('built-in HTTP') ]
	];
}

function speedtestBackendChoiceTitle(value) {
	var choices = speedtestBackendChoices();

	for (var i = 0; i < choices.length; i++)
		if (choices[i][0] === value)
			return choices[i][1];

	return value || _('Auto');
}

function speedtestBackendInstallable(value) {
	return value && value !== 'auto' && value !== 'builtin-http';
}

function speedtestRateText(dl, ul) {
	return '%s / %s kbit/s'.format(dl || '-', ul || '-');
}

function speedtestSummaryText(backend, percent, dl, ul, last) {
	var backendTitle = speedtestBackendChoiceTitle(backend || 'auto');
	var lines = [
		_('Backend: %s.').format(backendTitle),
		_('Apply: %d%%. Current limits: %s.').format(percent, speedtestRateText(dl, ul))
	];

	if (last && last.result) {
		lines.push(_('Last measured: %s using %s.').format(
			speedtestRateText(last.result.download_kbps, last.result.upload_kbps),
			speedtestBackendTitle(last.result)));
		lines.push(_('Last applied: %s.').format(speedtestRateText(last.applied && last.applied.dl, last.applied && last.applied.ul)));

		if (last.result.shaper_bypassed)
			lines.push(_('Calibration: unshaped.'));

		if (last.result.warning)
			lines.push(_('Warning: %s').format(last.result.warning));
	}
	else {
		lines.push(_('Last result: none yet.'));
	}

	return lines.join(' ');
}

function speedtestFormSummaryText(section, section_id) {
	return speedtestSummaryText(
		formOrUci(section, section_id, 'speedtest_backend') || 'auto',
		speedtestApplyPercent(section, section_id),
		formOrUci(section, section_id, 'sqm_download') || uci.get('cake-autorate', section_id, 'base_dl_shaper_rate_kbps'),
		formOrUci(section, section_id, 'sqm_upload') || uci.get('cake-autorate', section_id, 'base_ul_shaper_rate_kbps'),
		speedtestLastResults[section_id]);
}

function setSpeedtestSummaryNode(node, section, section_id) {
	if (node)
		node.textContent = speedtestFormSummaryText(section, section_id);
}

function speedtestSummaryElement(section, section_id) {
	var node = E('div', {
		'class': 'cake-autorate-speedtest-summary',
		'data-section': section_id,
		'style': 'display:inline-block;vertical-align:middle;margin-left:10px;max-width:680px;white-space:normal;color:#555;font-size:12px;line-height:1.35'
	});

	setSpeedtestSummaryNode(node, section, section_id);

	return node;
}

function refreshSpeedtestSummaries(section, section_id) {
	var nodes = document.querySelectorAll('.cake-autorate-speedtest-summary');

	for (var i = 0; i < nodes.length; i++)
		if (!section_id || nodes[i].getAttribute('data-section') === section_id)
			setSpeedtestSummaryNode(nodes[i], section, nodes[i].getAttribute('data-section'));
}

function formatSpeedtestBackendInstall(result) {
	var title = result.backend_title || speedtestBackendChoiceTitle(result.backend);
	var pkg = result.package ? ' (' + result.package + ')' : '';
	var reason = result.reason ? ' ' + result.reason : '';

	if (result.available)
		return _('Backend ready: %s%s.').format(title, pkg) + reason;

	return _('Backend installed but not ready: %s%s.').format(title, pkg) + reason;
}

function parseExecJson(res) {
	return JSON.parse((res.stdout || '').trim());
}

function withSpeedtestRpcTimeout(callback) {
	var previous = L.env.rpctimeout;
	var timeout = parseInt(previous, 10);

	if (isNaN(timeout) || timeout < 180)
		L.env.rpctimeout = 180;

	return Promise.resolve().then(callback).then(function(result) {
		L.env.rpctimeout = previous;
		return result;
	}, function(err) {
		L.env.rpctimeout = previous;
		throw err;
	});
}

function installSpeedtestBackend(section_id, wan, backend) {
	if (!speedtestBackendInstallable(backend))
		return Promise.reject(new Error(_('Select LibreSpeed CLI, speedtest-go, or configured iperf3 before installing.')));

	return withSpeedtestRpcTimeout(function() {
		return fs.exec('/usr/libexec/cake-autorate-rs/speedtest', [
			section_id,
			wan,
			'install',
			backend
		]);
	}).then(parseExecJson);
}

function formatSpeedtestBackendStatus(result) {
	var backends = result.backends || [];
	var lines = [];

	if (result.preferred_title)
		lines.push(_('Preferred backend: %s').format(result.preferred_title));

	if (result.selected_title)
		lines.push(_('Selected backend: %s').format(result.selected_title));
	else
		lines.push(_('No speed test backend is currently available.'));

	for (var i = 0; i < backends.length; i++) {
		var backend = backends[i];
		var state = backend.available ? _('available') : _('unavailable');
		var reason = backend.reason ? ' - ' + backend.reason : '';
		var install = (!backend.available && backend.install_hint) ? ' - ' + backend.install_hint : '';
		var marker = backend.selected ? ' *' : '';

		lines.push('%s: %s%s%s%s'.format(backend.title || backend.name, state, marker, reason, install));
	}

	if (result.warning)
		lines.push(result.warning);

	return lines.join('\n');
}

function runPingerPlan(section_id, mode) {
	return fs.exec('/usr/libexec/cake-autorate-rs/pinger-plan', [
		section_id,
		mode || 'status'
	]).then(parseExecJson);
}

function pingerBackendInstallable(value) {
	return value === 'fping' || value === 'fping-ts' || value === 'irtt';
}

function installPingerBackend(section_id, backend) {
	if (!pingerBackendInstallable(backend))
		return Promise.reject(new Error(_('Only fping/fping-ts/irtt can be installed automatically. tsping is a manual binary install.')));

	return fs.exec('/usr/libexec/cake-autorate-rs/pinger-plan', [
		section_id,
		'install',
		backend
	]).then(parseExecJson);
}

function formatPingerInstall(result) {
	var title = result.backend_title || result.backend || _('pinger');
	var pkg = result.package ? ' (' + result.package + ')' : '';
	var reason = result.reason ? ' ' + result.reason : '';

	if (result.available)
		return _('Pinger ready: %s%s.').format(title, pkg) + reason;

	return _('Pinger package installed but backend is not ready: %s%s.').format(title, pkg) + reason;
}

function formatPingerPlan(result) {
	var backends = result.backends || [];
	var warnings = result.warnings || [];
	var lines = [];

	lines.push(_('Configured pinger: %s').format(result.configured_method || '-'));
	if (result.configured_irtt_server_count != null)
		lines.push(_('Configured IRTT servers: %d').format(result.configured_irtt_server_count || 0));
	lines.push(_('Recommended pinger: %s').format(result.recommended_method || '-'));
	lines.push(_('Recommended active pingers: %s').format(result.recommended_no_pingers || '-'));

	if (result.recommended_reason)
		lines.push(result.recommended_reason);

	if (result.mode === 'scan') {
		if (result.candidate_source || result.default_pool_count)
			lines.push(_('Candidate pool: %d reflectors (%s, upstream defaults: %d)').format(result.valid_count || 0, result.candidate_source || '-', result.default_pool_count || 0));
		lines.push(_('RTT-capable reflectors: %d/%d').format(result.rtt_ok_count || 0, result.valid_count || 0));
		lines.push(_('Timestamp-capable reflectors: %d/%d').format(result.timestamp_ok_count || 0, result.valid_count || 0));
		if (result.timestamp_probe_backend)
			lines.push(_('Timestamp probe: %s').format(result.timestamp_probe_backend));
	}

	if (result.active && result.active.length)
		lines.push(_('Active set: %s').format(result.active.join(', ')));

	if (result.spare && result.spare.length)
		lines.push(_('Spare pool: %s').format(result.spare.join(', ')));

	if (result.bad && result.bad.length)
		lines.push(_('Bad or unsuitable: %s').format(result.bad.join(', ')));

	lines.push('');
	lines.push(_('Pinger backends:'));
	for (var i = 0; i < backends.length; i++) {
		var backend = backends[i];
		var state = backend.supported ? (backend.available ? _('available') : _('unavailable')) : _('pending');
		var meta = [];
		var markers = [];
		var support = backend.supported ? '' : ' - ' + _('daemon support pending');
		var reason = backend.reason ? ' - ' + backend.reason : '';
		var install = '';

		if (backend.delay_type)
			meta.push(backend.delay_type);
		if (backend.target_mode)
			meta.push(backend.target_mode);
		if (backend.configured)
			markers.push(_('configured'));
		if (backend.recommended)
			markers.push(_('recommended'));
		if (!backend.available && backend.install_hint)
			install = ' - ' + (backend.installable ? _('install: %s').format(backend.install_hint) : _('action: %s').format(backend.install_hint));

		lines.push('%s%s%s: %s%s%s%s'.format(
			backend.title || backend.name,
			meta.length ? ' [' + meta.join(', ') + ']' : '',
			markers.length ? ' (' + markers.join(', ') + ')' : '',
			state,
			support,
			reason,
			install
		));
	}

	if (warnings.length) {
		lines.push('');
		lines.push(_('Warnings:'));
		for (i = 0; i < warnings.length; i++)
			lines.push(warnings[i]);
	}

	return lines.join('\n');
}

function runMqttStatus(section_id, mode) {
	return fs.exec('/usr/libexec/cake-autorate-rs/mqtt-status', [
		section_id,
		mode || 'status'
	]).then(parseExecJson);
}

function yesNo(value) {
	return value ? _('yes') : _('no');
}

function formatMqttStatus(result) {
	var lines = [];

	lines.push(_('Instance: %s').format(result.section || '-'));
	lines.push(_('MQTT publisher enabled: %s').format(yesNo(result.enabled)));
	lines.push(_('MQTT client installed: %s').format(yesNo(result.installed)));
	lines.push(_('Broker host configured: %s').format(yesNo(result.configured_host)));
	lines.push(_('Log to file: %s').format(yesNo(result.log_to_file)));
	lines.push(_('Summary stats: %s').format(yesNo(result.summary_enabled)));
	lines.push(_('CPU stats: %s').format(yesNo(result.cpu_enabled)));
	lines.push(_('Publish CPU sensors: %s').format(yesNo(result.publish_cpu)));
	lines.push(_('Ready: %s').format(yesNo(result.available)));

	if (result.reason)
		lines.push(_('Status: %s').format(result.reason));

	if (!result.installed && result.install_hint)
		lines.push(_('Install hint: %s').format(result.install_hint));

	return lines.join('\n');
}

function defaultReflectors() {
	return [
		'1.1.1.1', '1.0.0.1',
		'8.8.8.8', '8.8.4.4',
		'9.9.9.9', '9.9.9.10', '9.9.9.11',
		'94.140.14.15', '94.140.14.140', '94.140.14.141', '94.140.15.15', '94.140.15.16',
		'64.6.65.6', '156.154.70.1', '156.154.70.2', '156.154.70.3', '156.154.70.4', '156.154.70.5',
		'156.154.71.1', '156.154.71.2', '156.154.71.3', '156.154.71.4', '156.154.71.5',
		'208.67.220.2', '208.67.220.123', '208.67.220.220', '208.67.222.2', '208.67.222.123',
		'185.228.168.9', '185.228.168.10'
	];
}

function pingerPlanReflectors(result) {
	var reflectors = result.recommended_reflectors || [];

	if (!reflectors.length)
		reflectors = (result.active || []).concat(result.spare || []);

	return reflectors.filter(function(reflector) {
		return reflector != null && reflector !== '';
	}).map(String);
}

function applyPingerPlanToState(state, result) {
	var reflectors = pingerPlanReflectors(result);

	if (!result || !result.recommended_method || !result.recommended_no_pingers || !reflectors.length)
		throw new Error(_('Pinger planner did not return a usable recommendation.'));

	state.pinger_method = result.recommended_method;
	state.no_pingers = String(result.recommended_no_pingers);
	state.reflectors = reflectors;
	state.ping_extra_args = pingerInterfaceArgs(state.wan_if, state.pinger_method);
	state.pinger_plan = result;
}

function applyPingerPlanToSection(section, section_id, result) {
	var reflectors = pingerPlanReflectors(result);

	if (!result || !result.recommended_method || !result.recommended_no_pingers || !reflectors.length)
		throw new Error(_('Pinger planner did not return a usable recommendation.'));

	setCakeOption(section, section_id, 'pinger_method', result.recommended_method);
	setCakeOption(section, section_id, 'no_pingers', result.recommended_no_pingers);
	maybeSetPingerInterfaceArgs(section, section_id, selectedWan(section, section_id, null, true), result.recommended_method);
	setCakeListOption(section, section_id, 'reflector', reflectors);
}

function applySpeedtestRates(section, section_id, result, percent) {
	var dl = measuredRate(result.download_kbps, percent);
	var ul = measuredRate(result.upload_kbps, percent);

	if (dl) {
		setCakeOption(section, section_id, 'sqm_download', dl);
		setCakeOption(section, section_id, 'base_dl_shaper_rate_kbps', dl);
		setCakeOption(section, section_id, 'max_dl_shaper_rate_kbps', dl);
		setCakeOption(section, section_id, 'min_dl_shaper_rate_kbps', halfRate(dl));
	}

	if (ul) {
		setCakeOption(section, section_id, 'sqm_upload', ul);
		setCakeOption(section, section_id, 'base_ul_shaper_rate_kbps', ul);
		setCakeOption(section, section_id, 'max_ul_shaper_rate_kbps', ul);
		setCakeOption(section, section_id, 'min_ul_shaper_rate_kbps', halfRate(ul));
	}

	return {
		dl: dl,
		ul: ul
	};
}

function sectionNameExists(name) {
	var sections = uci.sections('cake-autorate', 'cake_autorate') || [];

	for (var i = 0; i < sections.length; i++)
		if (sections[i]['.name'] === name)
			return true;

	return false;
}

function targetInterfaceChoices() {
	var choices = [];
	var seen = {};

	function add(name) {
		name = normalizeInterfaceName(name);
		if (!name || seen[name])
			return;

		choices.push(name);
		seen[name] = true;
	}

	add(defaultTargetInterface());

	for (var name in interfaceContext.deviceNames)
		add(name);

	choices.sort(function(a, b) {
		if (a === defaultTargetInterface())
			return -1;
		if (b === defaultTargetInterface())
			return 1;

		return a.localeCompare(b);
	});

	return choices;
}

function managedSqmSectionName(section_id) {
	return 'cake_' + section_id;
}

var sqmImportOptionMap = [
	[ 'sqm_debug_logging', 'debug_logging', '0' ],
	[ 'sqm_verbosity', 'verbosity', '5' ],
	[ 'sqm_qdisc', 'qdisc', 'cake' ],
	[ 'sqm_script', 'script', 'piece_of_cake.qos' ],
	[ 'sqm_qdisc_advanced', 'qdisc_advanced', '0' ],
	[ 'sqm_squash_dscp', 'squash_dscp', '1' ],
	[ 'sqm_squash_ingress', 'squash_ingress', '1' ],
	[ 'sqm_ingress_ecn', 'ingress_ecn', 'ECN' ],
	[ 'sqm_egress_ecn', 'egress_ecn', 'NOECN' ],
	[ 'sqm_qdisc_really_really_advanced', 'qdisc_really_really_advanced', '0' ],
	[ 'sqm_ilimit', 'ilimit', '' ],
	[ 'sqm_elimit', 'elimit', '' ],
	[ 'sqm_itarget', 'itarget', '' ],
	[ 'sqm_etarget', 'etarget', '' ],
	[ 'sqm_iqdisc_opts', 'iqdisc_opts', '' ],
	[ 'sqm_eqdisc_opts', 'eqdisc_opts', '' ],
	[ 'sqm_linklayer', 'linklayer', 'none' ],
	[ 'sqm_overhead', 'overhead', '0' ],
	[ 'sqm_linklayer_advanced', 'linklayer_advanced', '0' ],
	[ 'sqm_tcMTU', 'tcMTU', '2047' ],
	[ 'sqm_tcTSIZE', 'tcTSIZE', '128' ],
	[ 'sqm_tcMPU', 'tcMPU', '0' ],
	[ 'sqm_linklayer_adaptation_mechanism', 'linklayer_adaptation_mechanism', 'default' ]
];

function queueSectionName(queue) {
	return queue ? queue['.name'] : null;
}

function findImportableSqmQueueForInterface(iface) {
	var queues;

	if (!iface)
		return null;

	iface = normalizeInterfaceName(iface);
	queues = uci.sections('sqm', 'queue') || [];

	for (var i = 0; i < queues.length; i++) {
		if (normalizeInterfaceName(queues[i].interface) !== iface)
			continue;

		if (!queues[i]._cake_autorate_managed)
			return queues[i];
	}

	return null;
}

function applySqmSectionPreset(section_id, wan_if, replaceExisting, section) {
	var queue = findImportableSqmQueueForInterface(wan_if);
	var sectionName = queueSectionName(queue) || managedSqmSectionName(section_id);

	if (replaceExisting || !uci.get('cake-autorate', section_id, 'sqm_section'))
		setCakeOption(section, section_id, 'sqm_section', sectionName);

	if (!queue)
		return;

	if (replaceExisting || !uci.get('cake-autorate', section_id, 'sqm_enabled'))
		setCakeOption(section, section_id, 'sqm_enabled', queue.enabled === '1' ? '1' : '0');

	for (var i = 0; i < sqmImportOptionMap.length; i++) {
		var target = sqmImportOptionMap[i][0];
		var source = sqmImportOptionMap[i][1];
		var fallback = sqmImportOptionMap[i][2];
		var value = rateValue(queue[source], fallback);

		if (value !== '' && (replaceExisting || !uci.get('cake-autorate', section_id, target)))
			setCakeOption(section, section_id, target, value);
	}
}

function importSqmQueueIntoState(state) {
	var queue = findImportableSqmQueueForInterface(state.wan_if);

	state.imported_sqm_queue = queueSectionName(queue) || '';
	state.sqm_section = state.imported_sqm_queue || managedSqmSectionName(state.name);
	state.sqm_enabled = queue ? queue.enabled === '1' : false;
	state.sqm_download = rateValue(queue ? queue.download : null, '20000');
	state.sqm_upload = rateValue(queue ? queue.upload : null, '20000');

	for (var i = 0; i < sqmImportOptionMap.length; i++) {
		var target = sqmImportOptionMap[i][0];
		var source = sqmImportOptionMap[i][1];
		var fallback = sqmImportOptionMap[i][2];

		state[target] = rateValue(queue ? queue[source] : null, fallback);
	}
}

function wizardSqmQueueText(state) {
	if (state.imported_sqm_queue)
		return _('Use existing SQM queue "%s"').format(state.imported_sqm_queue);

	return _('Create managed SQM queue "%s"').format(state.sqm_section);
}

function writeWizardConfig(section_id, state) {
	var wan = normalizeInterfaceName(state.wan_if);
	var dl = rateValue(state.sqm_download, '20000');
	var ul = rateValue(state.sqm_upload, '20000');
	var sqmSection = state.sqm_section || managedSqmSectionName(section_id);
	var pingExtraArgs = state.ping_extra_args || pingerInterfaceArgs(wan, state.pinger_method || 'fping');

	uci.set('cake-autorate', section_id, 'enabled', state.enabled ? '1' : '0');
	uci.set('cake-autorate', section_id, 'wan_if', wan);
	uci.set('cake-autorate', section_id, 'auto_interface_preset', '1');
	uci.set('cake-autorate', section_id, 'adjust_dl_shaper_rate', '1');
	uci.set('cake-autorate', section_id, 'adjust_ul_shaper_rate', '1');
	uci.set('cake-autorate', section_id, 'manage_sqm', '1');
	uci.set('cake-autorate', section_id, 'sqm_section', sqmSection);
	uci.set('cake-autorate', section_id, 'sqm_enabled', state.sqm_enabled ? '1' : '0');
	uci.set('cake-autorate', section_id, 'speedtest_backend', state.speedtest_backend || 'auto');
	uci.set('cake-autorate', section_id, 'speedtest_apply_percent', String(state.speedtest_apply_percent || '90'));
	uci.set('cake-autorate', section_id, 'pinger_method', state.pinger_method || 'fping');
	uci.set('cake-autorate', section_id, 'no_pingers', String(state.no_pingers || '6'));
	if (pingExtraArgs)
		uci.set('cake-autorate', section_id, 'ping_extra_args', pingExtraArgs);
	uci.set('cake-autorate', section_id, 'reflector', (state.reflectors && state.reflectors.length) ? state.reflectors : defaultReflectors());
	uci.set('cake-autorate', section_id, 'manual_rate_limits', '0');
	uci.set('cake-autorate', section_id, 'advanced_settings', '0');
	uci.set('cake-autorate', section_id, 'sqm_interface', wan);
	uci.set('cake-autorate', section_id, 'ul_if', wan);
	uci.set('cake-autorate', section_id, 'dl_if', ifbForWan(wan));
	uci.set('cake-autorate', section_id, 'sqm_download', dl);
	uci.set('cake-autorate', section_id, 'sqm_upload', ul);
	uci.set('cake-autorate', section_id, 'base_dl_shaper_rate_kbps', dl);
	uci.set('cake-autorate', section_id, 'base_ul_shaper_rate_kbps', ul);
	uci.set('cake-autorate', section_id, 'max_dl_shaper_rate_kbps', dl);
	uci.set('cake-autorate', section_id, 'max_ul_shaper_rate_kbps', ul);
	uci.set('cake-autorate', section_id, 'min_dl_shaper_rate_kbps', halfRate(dl));
	uci.set('cake-autorate', section_id, 'min_ul_shaper_rate_kbps', halfRate(ul));

	for (var i = 0; i < sqmImportOptionMap.length; i++) {
		var key = sqmImportOptionMap[i][0];
		var fallback = sqmImportOptionMap[i][2];
		var value = state[key] != null ? state[key] : fallback;

		if (value !== '')
			uci.set('cake-autorate', section_id, key, String(value));
	}
}

function wizardField(label, control, description) {
	var field = E('div', { 'class': 'cbi-value' }, [
		E('label', { 'class': 'cbi-value-title' }, label),
		E('div', { 'class': 'cbi-value-field' }, control)
	]);

	if (description)
		field.lastChild.appendChild(E('div', { 'class': 'cbi-value-description' }, description));

	return field;
}

function wizardTextInput(value, datatype) {
	return E('input', {
		'type': 'text',
		'class': 'cbi-input-text',
		'value': value || '',
		'data-datatype': datatype || null
	});
}

function wizardCheckbox(checked) {
	return E('input', {
		'type': 'checkbox',
		'class': 'cbi-input-checkbox',
		'checked': checked ? 'checked' : null
	});
}

function wizardSelect(values, selected) {
	var options = [];

	for (var i = 0; i < values.length; i++)
		options.push(E('option', {
			'value': values[i],
			'selected': values[i] === selected ? 'selected' : null
		}, values[i]));

	return E('select', { 'class': 'cbi-input-select' }, options);
}

function wizardSelectOptions(values, selected) {
	var options = [];

	for (var i = 0; i < values.length; i++)
		options.push(E('option', {
			'value': values[i][0],
			'selected': values[i][0] === selected ? 'selected' : null
		}, values[i][1]));

	return E('select', { 'class': 'cbi-input-select' }, options);
}

function validatePositiveInteger(value) {
	value = parseInt(value, 10);

	return !isNaN(value) && value > 0;
}

function replaceNodeContent(node, children) {
	while (node.firstChild)
		node.removeChild(node.firstChild);

	if (Array.isArray(children)) {
		for (var i = 0; i < children.length; i++)
			node.appendChild(children[i]);
	}
	else if (children) {
		node.appendChild(children);
	}
}

function showCreateWizard(grid, name) {
	var choices = targetInterfaceChoices();
	var defaultWan = defaultTargetInterface();
	var state = {
		name: name,
		step: 0,
		wan_if: defaultWan,
		enabled: false,
		sqm_enabled: false,
		speedtest_backend: 'auto',
		speedtest_apply_percent: '90',
		pinger_method: 'fping',
		no_pingers: '6',
		ping_extra_args: pingerInterfaceArgs(defaultWan, 'fping'),
		reflectors: defaultReflectors(),
		sqm_download: '20000',
		sqm_upload: '20000'
	};
	var body = E('div', { 'class': 'cake-autorate-create-wizard' });
	var errorNode = E('div', {
		'class': 'alert-message error',
		'style': 'display:none'
	});

	function showError(message) {
		errorNode.textContent = message || '';
		errorNode.style.display = message ? '' : 'none';
	}

	importSqmQueueIntoState(state);

	function syncSqmForInterface() {
		importSqmQueueIntoState(state);
	}

	function stepTitle() {
		return [
			_('Interface'),
			_('Speed test'),
			_('Review')
		][state.step];
	}

	function renderSteps() {
		var steps = [];

		for (var i = 0; i < 3; i++)
			steps.push(E('span', {
				'class': 'badge %s'.format(i === state.step ? 'primary' : 'secondary'),
				'style': 'margin-right:6px'
			}, String(i + 1)));

		return E('div', { 'style': 'margin-bottom:12px' }, steps);
	}

	function renderInterfaceStep() {
		var target = wizardSelect(choices, state.wan_if);
		var enabled = wizardCheckbox(state.enabled);
		var sqmEnabled = wizardCheckbox(state.sqm_enabled);
		var queueInfo = E('div', { 'class': 'cbi-value-dummy' }, wizardSqmQueueText(state));

		target.addEventListener('change', function() {
			state.wan_if = normalizeInterfaceName(target.value);
			state.ping_extra_args = pingerInterfaceArgs(state.wan_if, state.pinger_method || 'fping');
			syncSqmForInterface();
			queueInfo.textContent = wizardSqmQueueText(state);
			sqmEnabled.checked = state.sqm_enabled;
		});

		enabled.addEventListener('change', function() {
			state.enabled = enabled.checked;
			if (state.enabled && !state.sqm_enabled) {
				state.sqm_enabled = true;
				sqmEnabled.checked = true;
			}
		});

		sqmEnabled.addEventListener('change', function() {
			state.sqm_enabled = sqmEnabled.checked;
		});

		return [
			wizardField(_('Target interface'), target, optionDescriptions.wan_if),
			wizardField(_('SQM queue'), queueInfo, optionDescriptions._wizard_sqm_queue),
			wizardField(_('Enable autorate'), enabled, optionDescriptions.enabled),
			wizardField(_('Enable SQM'), sqmEnabled, optionDescriptions.sqm_enabled)
		];
	}

	function renderSpeedStep() {
		var backend = wizardSelectOptions(speedtestBackendChoices(), state.speedtest_backend);
		var percent = wizardTextInput(state.speedtest_apply_percent, 'and(uinteger,min(1),max(100))');
		var download = wizardTextInput(state.sqm_download, 'uinteger');
		var upload = wizardTextInput(state.sqm_upload, 'uinteger');
		var backendStatus = E('pre', { 'style': 'white-space:pre-wrap;margin:6px 0 0 0' }, '');
		var status = E('div', { 'class': 'cake-autorate-speedtest-status' }, '');
		var summary = E('div', {
			'class': 'cake-autorate-speedtest-summary',
			'style': 'display:inline-block;vertical-align:middle;margin-left:10px;max-width:680px;white-space:normal;color:#555;font-size:12px;line-height:1.35'
		});
		var pingerStatus = E('pre', { 'style': 'white-space:pre-wrap;margin:6px 0 0 0' }, '');
		var syncInputs = function() {
			state.speedtest_backend = backend.value || 'auto';
			state.speedtest_apply_percent = percent.value || '90';
			state.sqm_download = download.value;
			state.sqm_upload = upload.value;
		};
		var updateSummary = function() {
			var pct = parseInt(state.speedtest_apply_percent || '90', 10);

			if (isNaN(pct) || pct < 1 || pct > 100)
				pct = 90;

			summary.textContent = speedtestSummaryText(
				state.speedtest_backend || 'auto',
				pct,
				state.sqm_download,
				state.sqm_upload,
				state.speedtest_last);
		};
		var checkButton = E('button', {
			'class': 'btn cbi-button',
			'click': function() {
				syncInputs();
				updateSummary();
				showError(null);
				checkButton.disabled = true;
				backendStatus.textContent = _('Checking backends...');

				withSpeedtestRpcTimeout(function() {
					return fs.exec('/usr/libexec/cake-autorate-rs/speedtest', [
						state.name,
						state.wan_if,
						'status',
						state.speedtest_backend
					]);
				}).then(function(res) {
					backendStatus.textContent = formatSpeedtestBackendStatus(JSON.parse((res.stdout || '').trim()));
				}).catch(function(err) {
					showError(_('Speed test backend check failed: %s').format(err.message || err));
					backendStatus.textContent = '';
				}).then(function() {
					checkButton.disabled = false;
				});
			}
		}, _('Check backends'));
		var installButton = E('button', {
			'class': 'btn cbi-button',
			'click': function() {
				syncInputs();
				updateSummary();
				showError(null);

				if (!speedtestBackendInstallable(state.speedtest_backend)) {
					showError(_('Select LibreSpeed CLI, speedtest-go, or configured iperf3 before installing.'));
					return;
				}

				installButton.disabled = true;
				backendStatus.textContent = _('Installing backend...');

				installSpeedtestBackend(state.name, state.wan_if, state.speedtest_backend).then(function(result) {
					backendStatus.textContent = formatSpeedtestBackendInstall(result);
				}).catch(function(err) {
					showError(_('Speed test backend install failed: %s').format(err.message || err));
					backendStatus.textContent = '';
				}).then(function() {
					installButton.disabled = false;
				});
			}
		}, _('Install backend'));
		var runButton = E('button', {
			'class': 'btn cbi-button cbi-button-action',
			'click': function() {
				var pct = parseInt(percent.value || '90', 10);

				if (isNaN(pct) || pct < 1 || pct > 100) {
					showError(_('Speed test apply percent must be between 1 and 100.'));
					return;
				}

				syncInputs();
				updateSummary();
				showError(null);
				runButton.disabled = true;
				status.textContent = _('Running speed test...');

				withSpeedtestRpcTimeout(function() {
					return fs.exec('/usr/libexec/cake-autorate-rs/speedtest', [
						state.name,
						state.wan_if,
						'run',
						state.speedtest_backend
					]);
				}).then(function(res) {
					var result = parseSpeedtestResult(res.stdout);
					var dl = measuredRate(result.download_kbps, pct);
					var ul = measuredRate(result.upload_kbps, pct);

					if (dl) {
						state.sqm_download = dl;
						download.value = dl;
					}

					if (ul) {
						state.sqm_upload = ul;
						upload.value = ul;
					}

					state.speedtest_last = {
						result: result,
						applied: {
							dl: dl,
							ul: ul
						}
					};
					updateSummary();

					status.textContent = result.warning ||
						_('Speed test completed using %s.').format(speedtestBackendTitle(result));
				}).catch(function(err) {
					showError(_('Speed test failed: %s').format(err.message || err));
					status.textContent = '';
				}).then(function() {
					runButton.disabled = false;
				});
			}
		}, _('Run speed test'));
		var scanReflectorsButton = E('button', {
			'class': 'btn cbi-button',
			'click': function() {
				syncInputs();
				showError(null);
				scanReflectorsButton.disabled = true;
				pingerStatus.textContent = _('Scanning reflectors...');

				runPingerPlan(state.name, 'scan').then(function(result) {
					applyPingerPlanToState(state, result);
					pingerStatus.textContent = formatPingerPlan(result);
				}).catch(function(err) {
					showError(_('Reflector scan failed: %s').format(err.message || err));
					pingerStatus.textContent = '';
				}).then(function() {
					scanReflectorsButton.disabled = false;
				});
			}
		}, _('Scan reflectors'));

		backend.addEventListener('change', syncInputs);
		backend.addEventListener('change', updateSummary);
		percent.addEventListener('input', function() { syncInputs(); updateSummary(); });
		percent.addEventListener('change', function() { syncInputs(); updateSummary(); });
		download.addEventListener('input', function() { syncInputs(); updateSummary(); });
		download.addEventListener('change', function() { syncInputs(); updateSummary(); });
		upload.addEventListener('input', function() { syncInputs(); updateSummary(); });
		upload.addEventListener('change', function() { syncInputs(); updateSummary(); });
		updateSummary();

		return [
			wizardField(_('Preferred backend'), backend, optionDescriptions.speedtest_backend),
			wizardField(_('Check backends'), E('div', {}, [ checkButton, ' ', installButton, backendStatus ]), optionDescriptions._speedtest_backend_install),
			wizardField(_('Speed test apply percent'), percent, optionDescriptions.speedtest_apply_percent),
			wizardField(_('Download speed'), download, optionDescriptions.sqm_download),
			wizardField(_('Upload speed'), upload, optionDescriptions.sqm_upload),
			wizardField(_('Run speed test'), E('div', {}, [ runButton, summary, status ]), optionDescriptions._speedtest),
			wizardField(_('Reflector plan'), E('div', {}, [ scanReflectorsButton, pingerStatus ]), optionDescriptions._wizard_reflector_plan)
		];
	}

	function renderReviewStep() {
		var wan = normalizeInterfaceName(state.wan_if);
		var rows = [
			[ _('Target interface'), wan ],
			[ _('SQM queue'), wizardSqmQueueText(state) ],
			[ _('Download interface'), ifbForWan(wan) ],
			[ _('Upload interface'), wan ],
			[ _('Queueing discipline'), state.sqm_qdisc || 'cake' ],
			[ _('Queue setup script'), state.sqm_script || 'piece_of_cake.qos' ],
			[ _('Preferred backend'), speedtestBackendChoiceTitle(state.speedtest_backend) ],
			[ _('Pinger'), state.pinger_method || 'fping' ],
			[ _('Extra ping args'), state.ping_extra_args || '-' ],
			[ _('Pingers'), String(state.no_pingers || '6') ],
			[ _('Reflectors'), ((state.reflectors && state.reflectors.length) ? state.reflectors : defaultReflectors()).join(', ') ],
			[ _('Download speed'), state.sqm_download + ' kbit/s' ],
			[ _('Upload speed'), state.sqm_upload + ' kbit/s' ],
			[ _('Min DL rate'), halfRate(state.sqm_download) + ' kbit/s' ],
			[ _('Min UL rate'), halfRate(state.sqm_upload) + ' kbit/s' ]
		];

		return [
			E('table', { 'class': 'table' }, rows.map(function(row) {
				return E('tr', { 'class': 'tr' }, [
					E('td', { 'class': 'td' }, row[0]),
					E('td', { 'class': 'td' }, row[1])
				]);
			}))
		];
	}

	function validateStep() {
		showError(null);

		if (state.step === 0 && !state.wan_if) {
			showError(_('Target interface is required.'));
			return false;
		}

		if (state.step === 1) {
			if (!validatePositiveInteger(state.speedtest_apply_percent) ||
			    parseInt(state.speedtest_apply_percent, 10) > 100) {
				showError(_('Speed test apply percent must be between 1 and 100.'));
				return false;
			}

			if (!validatePositiveInteger(state.sqm_download) ||
			    !validatePositiveInteger(state.sqm_upload)) {
				showError(_('Download and upload speeds must be positive integers.'));
				return false;
			}

		}

		return true;
	}

	function validateWizard() {
		showError(null);

		if (!state.name) {
			showError(_('Instance name is required.'));
			return false;
		}

		if (!state.wan_if) {
			showError(_('Target interface is required.'));
			return false;
		}

		if (!validatePositiveInteger(state.speedtest_apply_percent) ||
		    parseInt(state.speedtest_apply_percent, 10) > 100) {
			showError(_('Speed test apply percent must be between 1 and 100.'));
			return false;
		}

		if (!validatePositiveInteger(state.sqm_download) ||
		    !validatePositiveInteger(state.sqm_upload)) {
			showError(_('Download and upload speeds must be positive integers.'));
			return false;
		}

		if (!validatePositiveInteger(state.no_pingers)) {
			showError(_('Pingers must be a positive integer.'));
			return false;
		}

		if (state.enabled && !state.sqm_enabled) {
			showError(_('Enable SQM before enabling autorate. The automatic preset uses an IFB download interface created by SQM.'));
			return false;
		}

		if (parseInt(state.no_pingers || '1', 10) >
		    ((state.reflectors && state.reflectors.length) ? state.reflectors.length : defaultReflectors().length)) {
			showError(_('Pingers cannot exceed reflector count.'));
			return false;
		}

		return true;
	}

	function finish() {
		var config_name = grid.uciconfig || grid.map.config;
		var section_id;

		if (sectionNameExists(state.name)) {
			showError(_('Instance "%s" already exists.').format(state.name));
			return;
		}

		if (!validateWizard())
			return;

		section_id = grid.map.data.add(config_name, grid.sectiontype, state.name);
		writeWizardConfig(section_id, state);

		return grid.map.save(null, true)
			.then(L.bind(grid.map.load, grid.map))
			.then(L.bind(grid.map.reset, grid.map))
			.then(function() {
				ui.hideModal();
				ui.addNotification(null, E('p', _('Instance "%s" created. Review pending changes, then Save & Apply.').format(section_id)), 'info');
			})
			.catch(function(err) {
				showError(err.message || err);
			});
	}

	function render() {
		var content = [
			renderSteps(),
			E('h5', {}, stepTitle()),
			errorNode
		];
		var buttons = [
			E('button', {
				'class': 'btn cbi-button',
				'click': function() {
					ui.hideModal();
				}
			}, _('Cancel')),
			' '
		];
		var stepFields = state.step === 0 ? renderInterfaceStep() :
			state.step === 1 ? renderSpeedStep() :
			renderReviewStep();

		for (var i = 0; i < stepFields.length; i++)
			content.push(stepFields[i]);

		if (state.step > 0) {
			buttons.push(E('button', {
				'class': 'btn cbi-button',
				'click': function() {
					state.step--;
					render();
				}
			}, _('Back')));
			buttons.push(' ');
		}

		if (state.step < 2) {
			buttons.push(E('button', {
				'class': 'btn cbi-button cbi-button-positive',
				'click': function() {
					if (!validateStep())
						return;

					state.step++;
					render();
				}
			}, _('Next')));
		}
		else {
			buttons.push(E('button', {
				'class': 'btn cbi-button cbi-button-positive important',
				'click': finish
			}, _('Create')));
		}

		content.push(E('div', { 'class': 'button-row' }, buttons));

		replaceNodeContent(body, content);
	}

	ui.showModal(_('Create CAKE Autorate - %s').format(name), body, 'cbi-modal');
	render();
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

function requireAdvancedSettings(section) {
	var basicEditTabs = {
		setup: true
	};

	for (var i = 0; i < section.children.length; i++) {
		var option = section.children[i];

		if (!option.modalonly || !option.tab || basicEditTabs[option.tab])
			continue;

		option.retain = true;

		if (option.deps && option.deps.length) {
			for (var j = 0; j < option.deps.length; j++)
				option.deps[j].advanced_settings = '1';
		}
		else {
			option.depends('advanced_settings', '1');
		}
	}
}

function addRateOptions(section) {
	value(section, 'rates', 'connection_active_thr_kbps', _('Active threshold'), 'uinteger', '2000');
}

function addSpeedtestOptions(section) {
	var o;

	o = section.taboption('speedtest', form.ListValue, 'speedtest_backend', _('Preferred backend'));
	modal(o);
	describe(o, 'speedtest_backend');
	o.rmempty = false;
	o.default = 'auto';
	o.value('auto', _('Auto'));
	o.value('librespeed-cli', _('LibreSpeed CLI (package: librespeed-cli)'));
	o.value('speedtest-go', _('speedtest-go (package: speedtest-go)'));
	o.value('iperf3', _('configured iperf3 (package: iperf3)'));
	o.value('builtin-http', _('built-in HTTP'));
	o.onchange = function(ev, section_id) {
		refreshSpeedtestSummaries(this.section, section_id);
	};

	o = section.taboption('speedtest', form.DummyValue, '_speedtest_backend_order', _('Backend order'));
	modal(o);
	describe(o, '_speedtest_backend_order');
	o.cfgvalue = function() {
		return _('LibreSpeed CLI -> speedtest-go -> configured iperf3 -> built-in HTTP fallback');
	};

	o = section.taboption('speedtest', form.Button, '_speedtest_backend_status', _('Check backends'));
	modal(o);
	describe(o, '_speedtest_backend_status');
	o.inputtitle = _('Check backends');
	o.inputstyle = 'action';
	o.rmempty = true;
	o.write = function() {};
	o.remove = function() {};
	o.onclick = function(ev, section_id) {
		var activeSection = this.section;
		var wan = selectedWan(activeSection, section_id, null, true);
		var backend = formOrUci(activeSection, section_id, 'speedtest_backend') || 'auto';
		var button = ev.currentTarget;

		button.disabled = true;

		return withSpeedtestRpcTimeout(function() {
			return fs.exec('/usr/libexec/cake-autorate-rs/speedtest', [ section_id, wan, 'status', backend ]);
		}).then(function(res) {
			var result = JSON.parse((res.stdout || '').trim());
			var message = formatSpeedtestBackendStatus(result);

			ui.addNotification(null, E('pre', { 'style': 'white-space:pre-wrap' }, message), result.available ? 'info' : 'warning');
		}).catch(function(err) {
			ui.addNotification(null, E('p', _('Speed test backend check failed: %s').format(err.message || err)), 'error');
		}).then(function() {
			button.disabled = false;
		});
	};

	o = section.taboption('speedtest', form.Button, '_speedtest_backend_install', _('Install backend'));
	modal(o);
	describe(o, '_speedtest_backend_install');
	dependsAny(o, 'speedtest_backend', [ 'librespeed-cli', 'speedtest-go', 'iperf3' ]);
	o.inputtitle = _('Install backend');
	o.inputstyle = 'action';
	o.rmempty = true;
	o.write = function() {};
	o.remove = function() {};
	o.onclick = function(ev, section_id) {
		var activeSection = this.section;
		var wan = selectedWan(activeSection, section_id, null, true);
		var backend = null;
		var backendOption = optionByName(activeSection, 'speedtest_backend');
		var button = ev.currentTarget;

		if (backendOption && typeof backendOption.formvalue == 'function')
			backend = backendOption.formvalue(section_id);

		if (!backend)
			backend = uci.get('cake-autorate', section_id, 'speedtest_backend') || 'auto';

		button.disabled = true;

		return installSpeedtestBackend(section_id, wan, backend).then(function(result) {
			ui.addNotification(null, E('p', formatSpeedtestBackendInstall(result)), result.available ? 'info' : 'warning');
		}).catch(function(err) {
			ui.addNotification(null, E('p', _('Speed test backend install failed: %s').format(err.message || err)), 'error');
		}).then(function() {
			button.disabled = false;
		});
	};

	flag(section, 'speedtest', 'speedtest_bind_interface', _('Bind to target interface'), '1');
	o = flag(section, 'speedtest', 'speedtest_force_ipv4', _('Force IPv4'), '1');
	dependsAny(o, 'speedtest_backend', [ 'auto', 'librespeed-cli', 'builtin-http' ]);
	o = optionalValue(section, 'speedtest', 'speedtest_route_probe', _('Route probe'), 'host', '1.1.1.1');
	o.depends('speedtest_bind_interface', '1');
	o = optionalValue(section, 'speedtest', 'speedtest_download_url', _('Download URL'), null, '');
	dependsAny(o, 'speedtest_backend', [ 'auto', 'builtin-http' ]);
	o = optionalValue(section, 'speedtest', 'speedtest_upload_url', _('Upload URL'), null, '');
	dependsAny(o, 'speedtest_backend', [ 'auto', 'builtin-http' ]);
	o = optionalValue(section, 'speedtest', 'speedtest_download_bytes', _('Download bytes'), 'and(uinteger,min(1))', '25000000');
	dependsAny(o, 'speedtest_backend', [ 'auto', 'builtin-http' ]);
	o = optionalValue(section, 'speedtest', 'speedtest_upload_bytes', _('Upload bytes'), 'and(uinteger,min(0))', '4000000');
	dependsAny(o, 'speedtest_backend', [ 'auto', 'librespeed-cli', 'speedtest-go', 'builtin-http' ]);
	o = optionalValue(section, 'speedtest', 'speedtest_upload_retry_bytes', _('Upload retry bytes'), null, '1000000 262144');
	dependsAny(o, 'speedtest_backend', [ 'auto', 'builtin-http' ]);
	o = optionalValue(section, 'speedtest', 'speedtest_timeout_s', _('Request timeout'), 'and(uinteger,min(1))', '45');
	dependsAny(o, 'speedtest_backend', [ 'auto', 'librespeed-cli', 'builtin-http' ]);
	o = optionalValue(section, 'speedtest', 'speedtest_duration_s', _('Test duration'), 'and(uinteger,min(1))', '15');
	dependsAny(o, 'speedtest_backend', [ 'auto', 'librespeed-cli', 'iperf3' ]);
	o = optionalValue(section, 'speedtest', 'speedtest_iperf3_server', _('iperf3 server'), null, '');
	o.depends('speedtest_backend', 'iperf3');
	o = optionalValue(section, 'speedtest', 'speedtest_iperf3_port', _('iperf3 port'), 'port', '');
	o.depends('speedtest_backend', 'iperf3');
}

function addSetupOptions(section) {
	var o;

	o = flag(section, 'setup', 'enabled', _('Enable autorate'));
	o.forcewrite = true;
	o.onchange = function(ev, section_id, value) {
		var enabled = checkedFromEvent(ev, value);

		uci.set('cake-autorate', section_id, 'enabled', enabled ? '1' : '0');
		maybeEnableSqmForAutoPreset(this.section, section_id, enabled);
	};
	o.write = function(section_id, formvalue) {
		uci.set('cake-autorate', section_id, 'enabled', formvalue);
		maybeEnableSqmForAutoPreset(this.section, section_id, formvalue);
	};

	o = iface(section, 'setup', 'wan_if', _('Target interface'));
	o.default = defaultTargetInterface();
	o.forcewrite = true;
	o.cfgvalue = function(section_id) {
		return selectedWan(null, section_id);
	};
	o.onchange = function(ev, section_id, value) {
		if (autoInterfacePresetEnabled(this.section, section_id))
			applyWanPreset(section_id, value, true, this.section);

		maybeEnableSqmForAutoPreset(this.section, section_id);
		refreshSpeedtestSummaries(this.section, section_id);
	};
	o.write = function(section_id, formvalue) {
		formvalue = normalizeInterfaceName(formvalue);

		var previous = selectedWan(null, section_id);
		var importRates = previous !== formvalue ||
			!uci.get('cake-autorate', section_id, 'sqm_download') ||
			!uci.get('cake-autorate', section_id, 'sqm_upload');

		uci.set('cake-autorate', section_id, 'wan_if', formvalue);

		if (autoInterfacePresetEnabled(this.section, section_id))
			applyWanPreset(section_id, formvalue, importRates);

		maybeEnableSqmForAutoPreset(this.section, section_id);
	};

	o = flag(section, 'setup', 'auto_interface_preset', _('Auto SQM preset'), '1');
	o.forcewrite = true;
	o.write = function(section_id, formvalue) {
		uci.set('cake-autorate', section_id, 'auto_interface_preset', formvalue);

		if (formvalue === '1')
			applyWanPreset(section_id, selectedWan(this.section, section_id, null, true), false, this.section);

		maybeEnableSqmForAutoPreset(this.section, section_id);
	};

	o = flag(section, 'setup', 'sqm_enabled', _('Enable SQM'));
	o.forcewrite = true;
	o.onchange = function(ev, section_id, value) {
		var enabled = checkedFromEvent(ev, value);

		uci.set('cake-autorate', section_id, 'sqm_enabled', enabled ? '1' : '0');

		if (enabled)
			setCakeOption(this.section, section_id, 'manage_sqm', '1');
		else
			maybeEnableSqmForAutoPreset(this.section, section_id);
	};
	o.write = function(section_id, formvalue) {
		uci.set('cake-autorate', section_id, 'sqm_enabled', formvalue);

		if (formvalue === '1')
			setCakeOption(this.section, section_id, 'manage_sqm', '1');
		else
			maybeEnableSqmForAutoPreset(this.section, section_id);
	};

	o = value(section, 'setup', 'sqm_download', _('Download speed'), 'and(uinteger,min(0))', '20000');
	o.forcewrite = true;
	o.onchange = function(ev, section_id) {
		refreshSpeedtestSummaries(this.section, section_id);
	};
	o.cfgvalue = function(section_id) {
		var queue = findSqmQueueForInterface(selectedWan(null, section_id));

		return rateValue(uci.get('cake-autorate', section_id, 'sqm_download'),
			rateValue(queue ? queue.download : null,
				rateValue(uci.get('cake-autorate', section_id, 'base_dl_shaper_rate_kbps'), '20000')));
	};
	o.write = function(section_id, formvalue) {
		var manualRateLimits = manualRateLimitsEnabled(this.section, section_id);

		setCakeOption(null, section_id, 'sqm_download', formvalue);
		setCakeOption(null, section_id, 'base_dl_shaper_rate_kbps', formvalue);
		setCakeOption(null, section_id, 'max_dl_shaper_rate_kbps', formvalue);

		if (!manualRateLimits || !uci.get('cake-autorate', section_id, 'min_dl_shaper_rate_kbps'))
			setCakeOption(null, section_id, 'min_dl_shaper_rate_kbps', halfRate(formvalue));
	};

	o = value(section, 'setup', 'sqm_upload', _('Upload speed'), 'and(uinteger,min(0))', '20000');
	o.forcewrite = true;
	o.onchange = function(ev, section_id) {
		refreshSpeedtestSummaries(this.section, section_id);
	};
	o.cfgvalue = function(section_id) {
		var queue = findSqmQueueForInterface(selectedWan(null, section_id));

		return rateValue(uci.get('cake-autorate', section_id, 'sqm_upload'),
			rateValue(queue ? queue.upload : null,
				rateValue(uci.get('cake-autorate', section_id, 'base_ul_shaper_rate_kbps'), '20000')));
	};
	o.write = function(section_id, formvalue) {
		var manualRateLimits = manualRateLimitsEnabled(this.section, section_id);

		setCakeOption(null, section_id, 'sqm_upload', formvalue);
		setCakeOption(null, section_id, 'base_ul_shaper_rate_kbps', formvalue);
		setCakeOption(null, section_id, 'max_ul_shaper_rate_kbps', formvalue);

		if (!manualRateLimits || !uci.get('cake-autorate', section_id, 'min_ul_shaper_rate_kbps'))
			setCakeOption(null, section_id, 'min_ul_shaper_rate_kbps', halfRate(formvalue));
	};

	o = value(section, 'setup', 'speedtest_apply_percent', _('Speed test apply percent'), 'and(uinteger,min(1),max(100))', '90');
	o.default = '90';
	o.forcewrite = true;
	o.onchange = function(ev, section_id) {
		refreshSpeedtestSummaries(this.section, section_id);
	};

	o = section.taboption('setup', form.Button, '_speedtest', _('Run speed test'));
	modal(o);
	describe(o, '_speedtest');
	o.inputtitle = _('Run speed test');
	o.inputstyle = 'action';
	o.rmempty = true;
	o.write = function() {};
	o.remove = function() {};
	o.renderWidget = function(section_id) {
		var self = this;
		var title = this.titleFn('inputtitle', section_id) || this.titleFn('title', section_id);

		return E('div', {}, [
			E('button', {
				'class': 'cbi-button cbi-button-%s'.format(this.inputstyle || 'button'),
				'click': function(ev) {
					return self.onclick(ev, section_id);
				},
				'disabled': (this.readonly || this.map.readonly) || null
			}, [ title ]),
			speedtestSummaryElement(this.section, section_id),
			E('input', {
				'id': this.cbid(section_id),
				'type': 'hidden',
				'value': ''
			})
		]);
	};
	o.onclick = function(ev, section_id) {
		var button = ev.currentTarget;
		var activeSection = this.section;
		var percent = speedtestApplyPercent(activeSection, section_id);
		var wan = selectedWan(activeSection, section_id, null, true);
		var backend = formOrUci(activeSection, section_id, 'speedtest_backend') || 'auto';

		if (autoInterfacePresetEnabled(activeSection, section_id))
			applyWanPreset(section_id, wan, false, activeSection);

		refreshSpeedtestSummaries(activeSection, section_id);
		button.disabled = true;

		return withSpeedtestRpcTimeout(function() {
			return fs.exec('/usr/libexec/cake-autorate-rs/speedtest', [ section_id, wan, backend ]);
		}).then(function(res) {
			var result = parseSpeedtestResult(res.stdout);
			var applied = applySpeedtestRates(activeSection, section_id, result, percent);
			var message = _('Speed test applied at %d%%: download %s kbit/s, upload %s kbit/s.').format(
				percent,
				applied.dl || _('unchanged'),
				applied.ul || _('unchanged'));

			speedtestLastResults[section_id] = {
				result: result,
				applied: applied
			};
			refreshSpeedtestSummaries(activeSection, section_id);

			message += ' ' + _('Backend: %s.').format(speedtestBackendTitle(result));

			if (result.warning)
				message += ' ' + result.warning;

			ui.addNotification(null, E('p', message), result.warning ? 'warning' : 'info');
		}).catch(function(err) {
			ui.addNotification(null, E('p', _('Speed test failed: %s').format(err.message || err)), 'error');
		}).then(function() {
			button.disabled = false;
		});
	};

	o = flag(section, 'setup', 'manual_rate_limits', _('Manual rate limits'), '0');
	o.forcewrite = true;

	o = flag(section, 'setup', 'advanced_settings', _('Show advanced settings'), '0');
	o.forcewrite = true;

	o = value(section, 'setup', 'min_dl_shaper_rate_kbps', _('Min DL rate'), 'uinteger', '5000');
	o.depends('manual_rate_limits', '1');
	o.retain = true;
	o.validate = function(section_id) {
		return validateRateOrder(validationSection(this), section_id, 'dl');
	};

	o = value(section, 'setup', 'base_dl_shaper_rate_kbps', _('Base DL rate'), 'uinteger', '20000');
	o.depends('manual_rate_limits', '1');
	o.retain = true;
	o.validate = function(section_id) {
		return validateRateOrder(validationSection(this), section_id, 'dl');
	};

	o = value(section, 'setup', 'max_dl_shaper_rate_kbps', _('Max DL rate'), 'uinteger', '80000');
	o.depends('manual_rate_limits', '1');
	o.retain = true;
	o.validate = function(section_id) {
		return validateRateOrder(validationSection(this), section_id, 'dl');
	};

	o = value(section, 'setup', 'min_ul_shaper_rate_kbps', _('Min UL rate'), 'uinteger', '5000');
	o.depends('manual_rate_limits', '1');
	o.retain = true;
	o.validate = function(section_id) {
		return validateRateOrder(validationSection(this), section_id, 'ul');
	};

	o = value(section, 'setup', 'base_ul_shaper_rate_kbps', _('Base UL rate'), 'uinteger', '20000');
	o.depends('manual_rate_limits', '1');
	o.retain = true;
	o.validate = function(section_id) {
		return validateRateOrder(validationSection(this), section_id, 'ul');
	};

	o = value(section, 'setup', 'max_ul_shaper_rate_kbps', _('Max UL rate'), 'uinteger', '35000');
	o.depends('manual_rate_limits', '1');
	o.retain = true;
	o.validate = function(section_id) {
		return validateRateOrder(validationSection(this), section_id, 'ul');
	};
}

function addInterfaceOptions(section) {
	var o;

	o = iface(section, 'interfaces', 'dl_if', _('Download interface'));
	o.depends('auto_interface_preset', '0');
	o.validate = function(section_id) {
		return validateDifferentInterfaces(validationSection(this), section_id);
	};

	o = iface(section, 'interfaces', 'ul_if', _('Upload interface'));
	o.depends('auto_interface_preset', '0');
	o.validate = function(section_id) {
		return validateDifferentInterfaces(validationSection(this), section_id);
	};
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
	var o;

	o = section.taboption('reflectors', form.Button, '_pinger_backend_status', _('Check pingers'));
	modal(o);
	describe(o, '_pinger_backend_status');
	o.inputtitle = _('Check pingers');
	o.inputstyle = 'action';
	o.rmempty = true;
	o.write = function() {};
	o.remove = function() {};
	o.onclick = function(ev, section_id) {
		var button = ev.currentTarget;

		button.disabled = true;

		return runPingerPlan(section_id, 'status').then(function(result) {
			ui.addNotification(null, E('pre', { 'style': 'white-space:pre-wrap' }, formatPingerPlan(result)), 'info');
		}).catch(function(err) {
			ui.addNotification(null, E('p', _('Pinger status check failed: %s').format(err.message || err)), 'error');
		}).then(function() {
			button.disabled = false;
		});
	};

	o = section.taboption('reflectors', form.Button, '_pinger_backend_install', _('Install selected pinger'));
	modal(o);
	describe(o, '_pinger_backend_install');
	dependsAny(o, 'pinger_method', [ 'fping', 'fping-ts', 'irtt' ]);
	o.inputtitle = _('Install selected pinger');
	o.inputstyle = 'action';
	o.rmempty = true;
	o.write = function() {};
	o.remove = function() {};
	o.onclick = function(ev, section_id) {
		var button = ev.currentTarget;
		var method = formOrUci(section, section_id, 'pinger_method') || 'fping';

		if (!pingerBackendInstallable(method)) {
			ui.addNotification(null, E('p',
				_('Only fping/fping-ts/irtt can be installed automatically. tsping is a manual binary install.')), 'warning');
			return Promise.resolve();
		}

		button.disabled = true;

		return installPingerBackend(section_id, method).then(function(result) {
			ui.addNotification(null, E('p', formatPingerInstall(result)), result.available ? 'info' : 'warning');
		}).catch(function(err) {
			ui.addNotification(null, E('p', _('Pinger install failed: %s').format(err.message || err)), 'error');
		}).then(function() {
			button.disabled = false;
		});
	};

	o = section.taboption('reflectors', form.Button, '_reflector_scan', _('Scan reflectors'));
	modal(o);
	describe(o, '_reflector_scan');
	o.inputtitle = _('Scan reflectors');
	o.inputstyle = 'action';
	o.rmempty = true;
	o.write = function() {};
	o.remove = function() {};
	o.onclick = function(ev, section_id) {
		var button = ev.currentTarget;

		button.disabled = true;

		return runPingerPlan(section_id, 'scan').then(function(result) {
			var level = (result.warnings && result.warnings.length) ? 'warning' : 'info';
			ui.addNotification(null, E('pre', { 'style': 'white-space:pre-wrap' }, formatPingerPlan(result)), level);
		}).catch(function(err) {
			ui.addNotification(null, E('p', _('Reflector scan failed: %s').format(err.message || err)), 'error');
		}).then(function() {
			button.disabled = false;
		});
	};

	o = section.taboption('reflectors', form.Button, '_reflector_apply', _('Apply recommendation'));
	modal(o);
	describe(o, '_reflector_apply');
	o.inputtitle = _('Apply recommendation');
	o.inputstyle = 'action';
	o.rmempty = true;
	o.write = function() {};
	o.remove = function() {};
	o.onclick = function(ev, section_id) {
		var button = ev.currentTarget;

		button.disabled = true;

		return runPingerPlan(section_id, 'scan').then(function(result) {
			applyPingerPlanToSection(section, section_id, result);
			ui.addNotification(null, E('pre', { 'style': 'white-space:pre-wrap' },
				formatPingerPlan(result) + '\n\n' + _('Recommendation applied to pending changes. Use Save & Apply to commit it.')), 'info');
		}).catch(function(err) {
			ui.addNotification(null, E('p', _('Applying reflector recommendation failed: %s').format(err.message || err)), 'error');
		}).then(function() {
			button.disabled = false;
		});
	};

	o = section.taboption('reflectors', form.ListValue, 'pinger_method', _('Pinger'));
	modal(o);
	describe(o, 'pinger_method');
	o.value('fping', 'fping');
	o.value('fping-ts', 'fping-ts');
	o.value('tsping', _('tsping'));
	o.value('irtt', _('irtt'));
	o.value('ping', _('ping fallback'));
	o.default = 'fping';
	o.rmempty = false;
	o.validate = function(section_id) {
		return validatePingerCount(validationSection(this), section_id);
	};

	o = section.taboption('reflectors', form.DynamicList, 'reflector', _('Reflectors'));
	modal(o);
	describe(o, 'reflector');
	dependsAny(o, 'pinger_method', [ 'fping', 'fping-ts', 'tsping', 'ping' ]);
	o.datatype = 'host';
	o.default = defaultReflectors();
	o.rmempty = false;

	o = section.taboption('reflectors', form.DynamicList, 'irtt_server', _('IRTT servers'));
	modal(o);
	describe(o, 'irtt_server');
	o.rmempty = true;
	o.depends('pinger_method', 'irtt');
	o.validate = function(section_id, value) {
		var valid = validateIrttServerValue(value);
		return valid === true ? validatePingerCount(validationSection(this), section_id) : valid;
	};

	o = optionalValue(section, 'reflectors', 'reflectors_url', _('Reflectors URL'), null, '');
	dependsAny(o, 'pinger_method', [ 'fping', 'fping-ts', 'tsping', 'ping' ]);
	o = value(section, 'reflectors', 'reflectors_url_skip_lines', _('URL skip lines'), 'uinteger', '1');
	dependsAny(o, 'pinger_method', [ 'fping', 'fping-ts', 'tsping', 'ping' ]);
	flag(section, 'reflectors', 'randomize_reflectors', _('Randomize reflectors'));
	flag(section, 'reflectors', 'retain_reflector_stats', _('Retain reflector stats'));
	o = value(section, 'reflectors', 'no_pingers', _('Pingers'), 'uinteger', '6');
	o.validate = function(section_id) {
		return validatePingerCount(validationSection(this), section_id);
	};
	value(section, 'reflectors', 'reflector_ping_interval_s', _('Ping interval'), 'ufloat', '0.3');
	optionalValue(section, 'reflectors', 'ping_extra_args', _('Extra ping args'), null, '');
	optionalValue(section, 'reflectors', 'ping_prefix_string', _('Ping prefix'), null, '');
	o = value(section, 'reflectors', 'irtt_session_duration_m', _('IRTT session minutes'), 'uinteger', '10');
	o.depends('pinger_method', 'irtt');
}

function addLoggingOptions(section) {
	var o;

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
	optionalValue(section, 'logging', 'log_file_path_override', _('Log directory'), null, '');
	value(section, 'logging', 'log_file_buffer_size_B', _('Log buffer bytes'), 'uinteger', '512');
	value(section, 'logging', 'log_file_buffer_timeout_ms', _('Log buffer timeout'), 'uinteger', '500');
	flag(section, 'logging', 'log_file_export_compress', _('Compress exports'));

	flag(section, 'logging', 'mqtt_enabled', _('MQTT publisher'), '0');

	o = optionalValue(section, 'logging', 'mqtt_host', _('MQTT host'), 'host', '');
	o.depends('mqtt_enabled', '1');
	o.validate = function(section_id, value) {
		if (checkedFormOrUci(validationSection(this), section_id, 'mqtt_enabled', false) && !value)
			return _('MQTT broker host is required when MQTT publisher is enabled.');

		return true;
	};

	o = optionalValue(section, 'logging', 'mqtt_port', _('MQTT port'), 'port', '1883');
	o.depends('mqtt_enabled', '1');

	o = optionalValue(section, 'logging', 'mqtt_username', _('MQTT username'), null, '');
	o.depends('mqtt_enabled', '1');

	o = optionalValue(section, 'logging', 'mqtt_password', _('MQTT password'), null, '');
	o.depends('mqtt_enabled', '1');
	o.password = true;

	o = optionalValue(section, 'logging', 'mqtt_discovery_prefix', _('MQTT discovery prefix'), null, 'homeassistant');
	o.depends('mqtt_enabled', '1');

	o = optionalValue(section, 'logging', 'mqtt_base_topic', _('MQTT base topic'), null, 'cake-autorate');
	o.depends('mqtt_enabled', '1');

	o = optionalValue(section, 'logging', 'mqtt_device_id', _('MQTT device ID'), null, 'cake_autorate');
	o.depends('mqtt_enabled', '1');

	o = optionalValue(section, 'logging', 'mqtt_device_name', _('MQTT device name'), null, 'cake-autorate');
	o.depends('mqtt_enabled', '1');

	o = optionalValue(section, 'logging', 'mqtt_min_interval_s', _('MQTT interval'), 'and(uinteger,min(1))', '1');
	o.depends('mqtt_enabled', '1');

	o = flag(section, 'logging', 'mqtt_publish_cpu_stats', _('MQTT CPU sensors'), '0');
	o.depends('mqtt_enabled', '1');

	o = section.taboption('logging', form.Button, '_mqtt_status', _('Check MQTT'));
	modal(o);
	describe(o, '_mqtt_status');
	o.inputtitle = _('Check MQTT');
	o.inputstyle = 'action';
	o.rmempty = true;
	o.depends('mqtt_enabled', '1');
	o.write = function() {};
	o.remove = function() {};
	o.onclick = function(ev, section_id) {
		var button = ev.currentTarget;

		button.disabled = true;

		return runMqttStatus(section_id, 'status').then(function(result) {
			ui.addNotification(null, E('pre', { 'style': 'white-space:pre-wrap' }, formatMqttStatus(result)), result.available ? 'info' : 'warning');
		}).catch(function(err) {
			ui.addNotification(null, E('p', _('MQTT status check failed: %s').format(err.message || err)), 'error');
		}).then(function() {
			button.disabled = false;
		});
	};

	o = section.taboption('logging', form.Button, '_mqtt_install', _('Install MQTT client'));
	modal(o);
	describe(o, '_mqtt_install');
	o.inputtitle = _('Install MQTT client');
	o.inputstyle = 'action';
	o.rmempty = true;
	o.depends('mqtt_enabled', '1');
	o.write = function() {};
	o.remove = function() {};
	o.onclick = function(ev, section_id) {
		var button = ev.currentTarget;

		button.disabled = true;

		return runMqttStatus(section_id, 'install').then(function(result) {
			ui.addNotification(null, E('pre', { 'style': 'white-space:pre-wrap' }, formatMqttStatus(result)), result.available ? 'info' : 'warning');
		}).catch(function(err) {
			ui.addNotification(null, E('p', _('MQTT client install failed: %s').format(err.message || err)), 'error');
		}).then(function() {
			button.disabled = false;
		});
	};
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
	optionalValue(section, 'advanced', 'rx_bytes_path', _('RX bytes path'), null, '');
	optionalValue(section, 'advanced', 'tx_bytes_path', _('TX bytes path'), null, '');
}

function addSqmOptions(section, qdiscs, scripts) {
	var o, seen;

	o = flag(section, 'sqm_basic', 'manage_sqm', _('Manage SQM'), '1');
	o.validate = function(section_id) {
		return validateSqmSectionUnique(validationSection(this), section_id);
	};

	o = optionalValue(section, 'sqm_basic', 'sqm_section', _('SQM section'), 'uciname', '');
	dependsManagedSqm(o);
	o.validate = function(section_id) {
		return validateSqmSectionUnique(validationSection(this), section_id);
	};

	o = iface(section, 'sqm_basic', 'sqm_interface', _('SQM interface'));
	dependsManagedSqm(o, { auto_interface_preset: '0' });
	dependsManagedSqm(flag(section, 'sqm_basic', 'sqm_debug_logging', _('SQM debug logging')));
	dependsManagedSqm(listValue(section, 'sqm_basic', 'sqm_verbosity', _('SQM log verbosity'), [
		[ '0', 'silent' ],
		[ '1', 'error' ],
		[ '2', 'warning' ],
		[ '5', 'info' ],
		[ '8', 'debug' ],
		[ '10', 'trace' ]
	], '5'));

	o = section.taboption('sqm_qdisc', form.ListValue, 'sqm_qdisc', _('Queueing discipline'));
	modal(o);
	describe(o, 'sqm_qdisc');
	dependsManagedSqm(o);
	seen = {};
	addUniqueValue(o, seen, 'cake');
	for (var i = 0; i < qdiscs.length; i++)
		addUniqueValue(o, seen, qdiscs[i].name);
	o.default = 'cake';
	o.rmempty = false;

	o = section.taboption('sqm_qdisc', form.ListValue, 'sqm_script', _('Queue setup script'));
	modal(o);
	describe(o, 'sqm_script');
	dependsManagedSqm(o);
	seen = {};
	addUniqueValue(o, seen, 'piece_of_cake.qos');
	addUniqueValue(o, seen, 'cake.qos');
	for (i = 0; i < scripts.length; i++)
		addUniqueValue(o, seen, scripts[i]);
	o.default = 'piece_of_cake.qos';
	o.rmempty = false;

	o = flag(section, 'sqm_qdisc', 'sqm_qdisc_advanced', _('Advanced qdisc'));
	dependsManagedSqm(o);

	o = listValue(section, 'sqm_qdisc', 'sqm_squash_dscp', _('Squash DSCP'), [
		[ '1', 'SQUASH' ],
		[ '0', 'DO NOT SQUASH' ]
	], '1');
	dependsManagedSqm(o, { sqm_qdisc_advanced: '1' });

	o = listValue(section, 'sqm_qdisc', 'sqm_squash_ingress', _('Ignore DSCP'), [
		[ '1', 'Ignore' ],
		[ '0', 'Allow' ]
	], '1');
	dependsManagedSqm(o, { sqm_qdisc_advanced: '1' });

	o = listValue(section, 'sqm_qdisc', 'sqm_ingress_ecn', _('ECN ingress'), [ 'ECN', 'NOECN' ], 'ECN');
	dependsManagedSqm(o, { sqm_qdisc_advanced: '1' });

	o = listValue(section, 'sqm_qdisc', 'sqm_egress_ecn', _('ECN egress'), [ 'NOECN', 'ECN' ], 'NOECN');
	dependsManagedSqm(o, { sqm_qdisc_advanced: '1' });

	o = flag(section, 'sqm_qdisc', 'sqm_qdisc_really_really_advanced', _('Dangerous qdisc'));
	dependsManagedSqm(o, { sqm_qdisc_advanced: '1' });

	o = optionalValue(section, 'sqm_qdisc', 'sqm_ilimit', _('Hard queue limit ingress'), 'and(uinteger,min(0))', '');
	dependsManagedSqm(o, { sqm_qdisc_advanced: '1', sqm_qdisc_really_really_advanced: '1' });

	o = optionalValue(section, 'sqm_qdisc', 'sqm_elimit', _('Hard queue limit egress'), 'and(uinteger,min(0))', '');
	dependsManagedSqm(o, { sqm_qdisc_advanced: '1', sqm_qdisc_really_really_advanced: '1' });

	o = optionalValue(section, 'sqm_qdisc', 'sqm_itarget', _('Latency target ingress'), 'string', '');
	dependsManagedSqm(o, { sqm_qdisc_advanced: '1', sqm_qdisc_really_really_advanced: '1' });

	o = optionalValue(section, 'sqm_qdisc', 'sqm_etarget', _('Latency target egress'), 'string', '');
	dependsManagedSqm(o, { sqm_qdisc_advanced: '1', sqm_qdisc_really_really_advanced: '1' });

	o = optionalValue(section, 'sqm_qdisc', 'sqm_iqdisc_opts', _('Qdisc options ingress'), 'string', '');
	dependsManagedSqm(o, { sqm_qdisc_advanced: '1', sqm_qdisc_really_really_advanced: '1' });

	o = optionalValue(section, 'sqm_qdisc', 'sqm_eqdisc_opts', _('Qdisc options egress'), 'string', '');
	dependsManagedSqm(o, { sqm_qdisc_advanced: '1', sqm_qdisc_really_really_advanced: '1' });

	dependsManagedSqm(listValue(section, 'sqm_linklayer', 'sqm_linklayer', _('Link layer'), [
		[ 'none', 'none' ],
		[ 'ethernet', 'ethernet' ],
		[ 'atm', 'atm' ]
	], 'none'));

	o = value(section, 'sqm_linklayer', 'sqm_overhead', _('Per packet overhead'), 'and(integer,min(-1500))', '0');
	dependsManagedSqm(o, { sqm_linklayer: 'ethernet' });
	dependsManagedSqm(o, { sqm_linklayer: 'atm' });

	o = flag(section, 'sqm_linklayer', 'sqm_linklayer_advanced', _('Advanced link layer'));
	dependsManagedSqm(o, { sqm_linklayer: 'ethernet' });
	dependsManagedSqm(o, { sqm_linklayer: 'atm' });

	o = value(section, 'sqm_linklayer', 'sqm_tcMTU', _('Maximum packet size'), 'and(uinteger,min(0))', '2047');
	dependsManagedSqm(o, { sqm_linklayer: 'ethernet', sqm_linklayer_advanced: '1' });
	dependsManagedSqm(o, { sqm_linklayer: 'atm', sqm_linklayer_advanced: '1' });

	o = value(section, 'sqm_linklayer', 'sqm_tcTSIZE', _('Rate table size'), 'and(uinteger,min(0))', '128');
	dependsManagedSqm(o, { sqm_linklayer: 'ethernet', sqm_linklayer_advanced: '1' });
	dependsManagedSqm(o, { sqm_linklayer: 'atm', sqm_linklayer_advanced: '1' });

	o = value(section, 'sqm_linklayer', 'sqm_tcMPU', _('Minimum packet size'), 'and(uinteger,min(0))', '0');
	dependsManagedSqm(o, { sqm_linklayer: 'ethernet', sqm_linklayer_advanced: '1' });
	dependsManagedSqm(o, { sqm_linklayer: 'atm', sqm_linklayer_advanced: '1' });

	o = listValue(section, 'sqm_linklayer', 'sqm_linklayer_adaptation_mechanism', _('Link layer mechanism'), [
		'default',
		'cake',
		'htb_private',
		'tc_stab'
	], 'default');
	dependsManagedSqm(o, { sqm_linklayer: 'ethernet', sqm_linklayer_advanced: '1' });
	dependsManagedSqm(o, { sqm_linklayer: 'atm', sqm_linklayer_advanced: '1' });
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
			network.getNetworks(),
			L.resolveDefault(fs.list('/var/run/sqm/available_qdiscs'), []),
			loadSqmScripts(),
			uci.load('cake-autorate'),
			L.resolveDefault(uci.load('sqm'), null)
		]);
	},

	render: function(data) {
		var m, s;
		var qdiscs = data[2];
		var scripts = data[3];

		interfaceContext = buildInterfaceContext(data[0], data[1]);

		m = new form.Map('cake-autorate', _('CAKE Autorate'));
		s = m.section(form.GridSection, 'cake_autorate', _('Instances'));
		s.anonymous = false;
		s.addremove = true;
		s.addbtntitle = _('Create instance');
		s.nodescriptions = true;
		s.handleAdd = function(ev, name) {
			showCreateWizard(this, name);
		};
		s.addModalOptions = function(modalSection, section_id) {
			var parse = modalSection.parse;

			modalSection.parse = function() {
				var validation = validateInstanceSection(this, section_id);

				if (validation !== true) {
					ui.addNotification(null, E('p', validation), 'error');
					return Promise.reject(new TypeError(validation));
				}

				return parse.apply(this, arguments);
			};
		};

		addSummaryColumns(s);

		s.tab('setup', _('Setup'));
		s.tab('general', _('General'));
		s.tab('interfaces', _('Interfaces'));
		s.tab('sqm_basic', _('SQM Basic'));
		s.tab('sqm_qdisc', _('SQM Queue'));
		s.tab('sqm_linklayer', _('SQM Link Layer'));
		s.tab('rates', _('Rates'));
		s.tab('speedtest', _('Speed Test'));
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
		addSpeedtestOptions(s);
		addReflectorOptions(s);
		addLatencyOptions(s);
		addControllerOptions(s);
		addLoggingOptions(s);
		addAdvancedOptions(s);
		requireAdvancedSettings(s);

		return m.render();
	}
});
