'use strict';
'require fs';
'require form';
'require network';
'require rpc';
'require uci';
'require ui';
'require tools.widgets as widgets';
'require cake-autorate-rs.ui as cakeUi';

function modal(option) {
	option.modalonly = true;
	return option;
}

var optionDescriptions = {
	enabled: 'Start autorate and its managed SQM queue together for this instance.',
	adjust_dl_shaper_rate: 'Allow autorate to change the download CAKE bandwidth.',
	adjust_ul_shaper_rate: 'Allow autorate to change the upload CAKE bandwidth.',
	wan_if: 'Main WAN interface for this instance. Auto preset also uses it for SQM and IFB setup.',
	route_mode: 'Select the main routing table or force every ICMP, HTTP, speed test, and Auto-Tune probe through one mwan3 member.',
	mwan3_member: 'Logical mwan3 interface/member used for this uplink. Its resolved L3 device must match the target interface.',
	route_stability_s: 'Time the selected route must stay online with the same device and source address before probes restart.',
	route_check_interval_s: 'Interval for checking mwan3 state, L3 device, source address, fwmark, and route identity.',
	auto_interface_preset: 'Automatically derive SQM interface, upload interface, and download IFB from the target interface.',
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
	speedtest_go_server_id: 'Optional speedtest-go server ID. Leave empty to automatically validate nearby servers and reuse the first good one; set an ID to pin a known-good server.',
	speedtest_duration_s: 'Test duration in seconds for optional CLI backends that support a duration setting.',
	speedtest_iperf3_server: 'Optional iperf3 server host or address. iperf3 is only used when this is set and the iperf3 package is installed.',
	speedtest_iperf3_port: 'Optional iperf3 server port. Leave empty to use the iperf3 default.',
	_speedtest_backend_order: 'Backend autodetect order used by the speed test helper.',
	_speedtest_backend_status: 'Check which optional speed test backends are currently installed or configured on this router.',
	_speedtest_backend_install: 'Install the selected optional backend package on this router. Auto and built-in HTTP do not need installation.',
	_wizard_sqm_queue: 'Existing unmanaged SQM queues on the selected interface are reused to avoid duplicate shapers.',
	_wizard_advanced_test_options: 'Show backend selection, speed test headroom, package checks, and reflector planning. Auto defaults are suitable for normal setup.',
	manual_rate_limits: 'Show explicit min, base, and max autorate limits. Leave off to derive them from download and upload speeds.',
	advanced_settings: 'Show detailed SQM, reflector, controller, logging, and daemon tuning settings.',
	min_dl_shaper_rate_kbps: 'Lowest download shaper rate autorate may apply, in kbit/s.',
	base_dl_shaper_rate_kbps: 'Starting download shaper rate before autorate adjusts it, in kbit/s.',
	max_dl_shaper_rate_kbps: 'Highest download shaper rate autorate may apply, in kbit/s.',
	min_ul_shaper_rate_kbps: 'Lowest upload shaper rate autorate may apply, in kbit/s.',
	base_ul_shaper_rate_kbps: 'Starting upload shaper rate before autorate adjusts it, in kbit/s.',
	max_ul_shaper_rate_kbps: 'Highest upload shaper rate autorate may apply, in kbit/s.',
	adaptive_ceiling_enabled: 'Bounded probe mode. The configured maximum becomes a learned-safe starting ceiling. Under sustained clean high load the daemon briefly tests a higher ceiling, keeps successful values, and rolls back while remembering failed values when latency rises.',
	adaptive_ceiling_dl_cap_kbps: 'Absolute download safety cap for adaptive ceiling growth, in kbit/s. It must not be below the configured download maximum.',
	adaptive_ceiling_ul_cap_kbps: 'Absolute upload safety cap for adaptive ceiling growth, in kbit/s. It must not be below the configured upload maximum.',
	adaptive_ceiling_hold_time_s: 'Clean high-load qualification time before a probe starts. Brief load or delay-classification fluctuations are tolerated; a sustained interruption, global probe gap, or stall cancels qualification.',
	adaptive_ceiling_growth_percent: 'Open-ended probe step as a percentage of the learned-safe ceiling. Once a failed upper bound is known, probes use the midpoint instead.',
	adaptive_ceiling_probe_duration_s: 'Time a candidate ceiling must carry clean high load before it is accepted as the new learned-safe ceiling.',
	adaptive_ceiling_cooldown_s: 'Recovery pause after a successful or failed probe before qualification may start again.',
	adaptive_ceiling_failed_bound_ttl_s: 'How long a failed upper ceiling remains remembered. It prevents repeatedly testing a known-bad value, but expires so the link can be relearned after conditions change.',
	transport_latency_enabled: 'Measure real network RTT with a persistent native transport connection. DNS, process startup, and the TLS/WebSocket handshake are excluded. Rating is passive unless the controller is enabled separately.',
	transport_controller_enabled: 'Allow confirmed transport RTT windows to reduce CAKE rates. Disabled by default for safe upgrades. A bad direction must be confirmed twice and can never cross the configured throughput floor.',
	transport_probe_backend: 'WebSocket is the recommended LibreQoS-compatible persistent RTT method. TCP connect and persistent HTTP are comparison fallbacks. Legacy HTTP includes process and handshake overhead, is diagnostic-only, and cannot drive the controller.',
	transport_probe_endpoint: 'Endpoint for the selected native backend. Probes are bound to this instance route, source address, device, and mwan3 mark.',
	transport_probe_idle_interval_s: 'Seconds between baseline probes while traffic is below the high-load threshold.',
	transport_probe_loaded_interval_s: 'Seconds between probes while download or upload is highly loaded.',
	transport_probe_timeout_s: 'Maximum seconds allowed for one asynchronous transport probe.',
	transport_load_hold_s: 'High load must remain in the same download/upload phase for this long before a loaded RTT probe starts.',
	transport_cpu_max_percent: 'Discard a transport sample when total router CPU is above this percentage so local saturation is not mistaken for WAN latency.',
	rating_load_window_s: 'Independent rolling throughput window used only to detect rating load. It does not change the autorate controller high-load threshold.',
	rating_load_enter_ratio: 'Smoothed share of the current CAKE rate required to enter a download or upload rating phase. 0.60 means 60%. An explicit Get rating capture may learn a lower safe trigger from the observed peak.',
	rating_load_exit_ratio: 'Lower hysteresis threshold used to leave a latched rating phase. It must remain below the enter ratio.',
	rating_load_hold_s: 'How long one direction must satisfy the enter threshold before its rating phase is latched.',
	rating_load_dropout_s: 'How long a short traffic gap is tolerated without losing the latched download or upload phase.',
	rating_load_min_kbps: 'Absolute minimum traffic rate required for passive rating detection, independent of the percentage threshold.',
	rating_load_dominance_ratio: 'When both directions are active, one direction must exceed the other by this ratio to avoid classifying the sample as bidirectional.',
	rating_capture_min_enter_ratio: 'Lowest per-direction trigger allowed during Get rating. It is measured against the current CAKE rate and prevents an irregular browser phase from being missed.',
	rating_capture_peak_factor: 'Fraction of each direction\'s own learned peak used by Get rating. DL and UL learn independently, and the threshold is frozen while a candidate is being confirmed.',
	rating_capture_contamination_ratio: 'Unexpected opposite-direction traffic above this share of its current CAKE rate marks an automatic rating phase as contaminated instead of silently mixing it into the result.',
	rating_capture_ack_ratio: 'Maximum reverse traffic treated as expected TCP acknowledgements, as a share of the requested direction. Contamination must exceed both this allowance and the opposite-direction CAKE limit.',
	rating_capture_quiet_s: 'Consecutive quiet seconds required before Get rating records its background baseline.',
	rating_capture_quiet_timeout_s: 'Maximum time Get rating waits for a quiet window before refusing a contaminated test.',
	rating_capture_quiet_ratio: 'Maximum background share of the current CAKE rate accepted during the pre-test quiet window.',
	rating_capture_quiet_min_kbps: 'Absolute background allowance used when the percentage allowance would be too small.',
	rating_episode_gap_s: 'Idle time after loaded traffic before the current rating episode is finalized. This keeps short browser-test gaps inside one result.',
	quality_target_delay_ms: 'Target loaded transport-delay increase. The default 30 ms corresponds to an estimated A-like target.',
	quality_search_max_steps: 'Maximum bounded rate reductions in one search before cooldown and rollback to the best useful candidate.',
	quality_search_observe_s: 'Observation time after each candidate rate change.',
	quality_search_cooldown_s: 'Pause after the target cannot be reached safely or a candidate does not improve latency.',
	throughput_guard_enabled: 'Never let transport-driven search reduce a direction below its robust throughput floor.',
	throughput_guard_retention_percent: 'Percentage of the robust capacity reference retained as the safety floor.',
	throughput_guard_dl_floor_kbps: 'Optional absolute download floor. Zero uses the calculated floor.',
	throughput_guard_ul_floor_kbps: 'Optional absolute upload floor. Zero uses the calculated floor.',
	throughput_reference_dl_p20_kbps: 'Optional download 20th-percentile capacity from Full Auto-Tune.',
	throughput_reference_dl_p50_kbps: 'Optional download median capacity from Full Auto-Tune.',
	throughput_reference_ul_p20_kbps: 'Optional upload 20th-percentile capacity from Full Auto-Tune.',
	throughput_reference_ul_p50_kbps: 'Optional upload median capacity from Full Auto-Tune.',
	autotune_profile: 'Profile used by the next manual or scheduled Full Auto-Tune run. Gaming targets A+ and enables diffserv4, Best overall targets A, and Fair prioritizes throughput with a conditional class-C target and an explicit evidence-backed no-SQM fallback.',
	scheduled_autotune_enabled: 'Periodically run the validated Full Auto-Tune workflow only inside the configured quiet window. Disabled by default.',
	scheduled_autotune_interval_hours: 'Minimum hours between successful scheduled calibrations.',
	scheduled_autotune_idle_window_s: 'Traffic must remain below the active threshold for this long before a scheduled test may start.',
	scheduled_autotune_window_start_hour: 'Local hour when the permitted maintenance window begins (0-23).',
	scheduled_autotune_window_end_hour: 'Local hour when the permitted maintenance window ends (0-23). Equal start and end permits the whole day.',
	scheduled_autotune_max_traffic_mb_day: 'Maximum interface traffic attributed to scheduled calibration per local day. Accounting lives only in RAM.',
	scheduled_autotune_auto_apply: 'Automatically commit and restart with a proposal only after shaped validation passes. Leave off to keep a review-only proposal.',
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
	sqm_squash_dscp: 'Clear DSCP markings from inbound packets as they leave the download shaper.',
	sqm_squash_ingress: 'Ignore inbound DSCP markings when CAKE selects a download tin.',
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
	deviceNetworks: {},
	devicePhysical: {},
	networkDevices: {},
	defaultDevice: 'wan'
};

var mwan3Context = {
	members: [],
	byName: {}
};

var mwan3Capability = {};

var speedtestLastResults = {};
var autorateSubcategoryStates = {};

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
		deviceNetworks: {},
		devicePhysical: {},
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
		var l2Device = networks[i].getL2Device ? networks[i].getL2Device() : null;
		var l2Name = l2Device && l2Device.getName ? l2Device.getName() : null;

		if (!netName || !ifName)
			continue;

		if (ifName.charAt(0) === '@')
			ifName = ifName.substring(1);

		ctx.networkDevices[netName] = ifName;
		if (l2Name && l2Name !== ifName)
			ctx.devicePhysical[ifName] = l2Name;
	}

	function resolveNetworkDevice(name, seen) {
		var mapped;

		if (!name)
			return name;

		if (name.charAt(0) === '@')
			name = name.substring(1);

		seen = seen || {};
		if (seen[name])
			return name;
		seen[name] = true;

		mapped = ctx.networkDevices[name];
		return mapped && mapped !== name ? resolveNetworkDevice(mapped, seen) : name;
	}

	for (var networkName in ctx.networkDevices) {
		var deviceName = resolveNetworkDevice(ctx.networkDevices[networkName]);

		if (!ctx.deviceNames[deviceName])
			continue;

		if (!ctx.deviceNetworks[deviceName])
			ctx.deviceNetworks[deviceName] = [];

		ctx.deviceNetworks[deviceName].push(networkName);
	}

	for (var device in ctx.deviceNetworks)
		ctx.deviceNetworks[device].sort();

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

function buildMwan3Context() {
	var ctx = { members: [], byName: {} };
	var sections = uci.sections('mwan3', 'interface') || [];

	for (var i = 0; i < sections.length; i++) {
		var name = sections[i]['.name'];
		var device = normalizeInterfaceName(name);

		if (!name || !device || sections[i].enabled === '0' || sections[i].family === 'ipv6')
			continue;

		var member = {
			name: name,
			device: device,
			label: interfacePathLabel(name, device)
		};
		ctx.members.push(member);
		ctx.byName[name] = member;
	}

	ctx.members.sort(function(a, b) { return a.name.localeCompare(b.name); });
	return ctx;
}

function mwan3MembersForDevice(device) {
	device = normalizeInterfaceName(device);
	return mwan3Context.members.filter(function(member) {
		return member.device === device;
	});
}

function wizardRouteChoices() {
	var choices = [ [ 'main', _('Main routing table') ] ];

	for (var i = 0; i < mwan3Context.members.length; i++) {
		var member = mwan3Context.members[i];
		choices.push([ 'mwan3:' + member.name, _('mwan3: %s').format(member.label) ]);
	}
	return choices;
}

function uniqueMwan3Uplinks() {
	var byDevice = {};
	var uplinks = [];

	for (var i = 0; i < mwan3Context.members.length; i++) {
		var member = mwan3Context.members[i];
		var current = byDevice[member.device];
		if (!current || (/6$/.test(current.name) && !/6$/.test(member.name)))
			byDevice[member.device] = member;
	}
	for (var device in byDevice)
		uplinks.push(byDevice[device]);
	uplinks.sort(function(a, b) { return a.name.localeCompare(b.name); });
	return uplinks;
}

function multiwanInstancePlans(state) {
	var uplinks = uniqueMwan3Uplinks();
	return uplinks.map(function(member) {
		var instanceName = member.device === normalizeInterfaceName(state.wan_if) ?
			state.name : member.name.replace(/[^A-Za-z0-9_]/g, '_') + '_sqm';
		return {
			name: instanceName,
			member: member.name,
			device: member.device,
			sqmSection: managedSqmSectionName(instanceName)
		};
	});
}

function wizardPlanConflicts(plans, enabled, ignoredInstance) {
	var conflicts = [];
	var names = {};
	var devices = {};
	var existing = uci.sections('cake-autorate', 'cake_autorate') || [];

	for (var planIndex = 0; planIndex < plans.length; planIndex++) {
		var plan = plans[planIndex];
		if (names[plan.name])
			conflicts.push(_('Generated instance name "%s" is duplicated.').format(plan.name));
		if (devices[plan.device])
			conflicts.push(_('Two generated instances would manage the same CAKE target %s.').format(plan.device));
		names[plan.name] = true;
		devices[plan.device] = true;

		for (var existingIndex = 0; existingIndex < existing.length; existingIndex++) {
			var section = existing[existingIndex];
			var existingName = section['.name'];
			if (existingName === ignoredInstance)
				continue;
			var existingTarget = normalizeInterfaceName(section.sqm_interface || section.ul_if || section.wan_if);
			if (existingName === plan.name)
				conflicts.push(_('Instance "%s" already exists.').format(plan.name));
			if (enabled && section.enabled === '1' && section.manage_sqm !== '0' && existingTarget === plan.device)
				conflicts.push(_('Instance "%s" already manages a CAKE queue on %s.').format(existingName, existingTarget));
		}
	}

	return conflicts.filter(function(message, index, all) {
		return all.indexOf(message) === index;
	});
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

function adaptiveConfiguredMax(section, section_id, direction) {
	var key;

	if (manualRateLimitsEnabled(section, section_id))
		key = 'max_' + direction + '_shaper_rate_kbps';
	else
		key = direction === 'dl' ? 'sqm_download' : 'sqm_upload';

	return parsePositiveRate(formOrUci(section, section_id, key));
}

function validateAdaptiveCeiling(section, section_id) {
	var dlMax, ulMax, dlCap, ulCap;

	if (!checkedFormOrUci(section, section_id, 'adaptive_ceiling_enabled', false))
		return true;

	dlMax = adaptiveConfiguredMax(section, section_id, 'dl');
	ulMax = adaptiveConfiguredMax(section, section_id, 'ul');
	dlCap = parsePositiveRate(formOrUci(section, section_id, 'adaptive_ceiling_dl_cap_kbps'));
	ulCap = parsePositiveRate(formOrUci(section, section_id, 'adaptive_ceiling_ul_cap_kbps'));

	if (dlCap == null)
		return _('Adaptive download safety cap is required when adaptive ceiling is enabled.');

	if (ulCap == null)
		return _('Adaptive upload safety cap is required when adaptive ceiling is enabled.');

	if (dlMax != null && dlCap < dlMax)
		return _('Adaptive download safety cap must be at least the configured maximum (%d kbit/s).').format(dlMax);

	if (ulMax != null && ulCap < ulMax)
		return _('Adaptive upload safety cap must be at least the configured maximum (%d kbit/s).').format(ulMax);

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

function validateTransportProbeUrl(backend, value) {
	backend = String(backend || '');
	value = String(value || '');

	switch (backend) {
	case 'websocket':
		if (/^wss?:\/\/\S+$/.test(value))
			return true;
		return _('Persistent WebSocket requires a ws:// or wss:// endpoint without spaces.');
	case 'tcp':
		if (/^tcp:\/\/\S+$/.test(value))
			return true;
		return _('TCP connect requires a tcp:// endpoint without spaces.');
	case 'http':
		if (/^https:\/\/\S+$/.test(value))
			return true;
		return _('Persistent HTTP requires an https:// endpoint without spaces.');
	case 'legacy-http':
		if (/^https?:\/\/\S+$/.test(value))
			return true;
		return _('Legacy HTTP requires an http:// or https:// endpoint without spaces.');
	default:
		return _('Select a supported transport probe backend.');
	}
}

function validateRatingLoadRatios(section, section_id) {
	var enter = parseFloat(formOrUci(section, section_id, 'rating_load_enter_ratio') || '0.60');
	var exit = parseFloat(formOrUci(section, section_id, 'rating_load_exit_ratio') || '0.40');

	if (isFinite(enter) && isFinite(exit) && exit >= enter)
		return _('Rating exit ratio must be lower than the enter ratio.');
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

function validateManagedSqmTargetUnique(section, section_id) {
	var enabled = checkedFormOrUci(section, section_id, 'enabled', false);
	var manage = checkedFormOrUci(section, section_id, 'manage_sqm', true);
	var target = selectedWan(section, section_id, null, true);
	var sections;

	if (!enabled || !manage || !target)
		return true;

	sections = uci.sections('cake-autorate', 'cake_autorate') || [];
	for (var i = 0; i < sections.length; i++) {
		var other = sections[i];
		var otherName = other['.name'];
		if (!otherName || otherName === section_id || other.enabled !== '1' || other.manage_sqm === '0')
			continue;
		var otherTarget = normalizeInterfaceName(other.sqm_interface || other.ul_if || other.wan_if);
		if (otherTarget === target)
			return _('Instance "%s" already has an active managed CAKE queue on %s.').format(otherName, target);
	}
	return true;
}

function validateRouteSelection(section, section_id) {
	var mode = formOrUci(section, section_id, 'route_mode') || 'auto';
	var memberName = formOrUci(section, section_id, 'mwan3_member') || '';
	var target = selectedWan(section, section_id, null, true);
	var member;

	if ([ 'auto', 'main', 'mwan3' ].indexOf(mode) < 0)
		return _('Route mode must be Auto, Main routing, or mwan3.');
	if (mode === 'main' && memberName)
		return _('Main routing must not define an mwan3 member.');
	if (mode === 'mwan3' && !memberName)
		return _('Select an mwan3 member.');
	if (!memberName)
		return true;
	if (!mwan3Capability.available || !mwan3Capability.nft || !mwan3Capability.scoped_status_api)
		return _('Structured routing requires the nftables mwan3 backend and member-scoped status API.');

	member = mwan3Context.byName[memberName];
	if (!member)
		return _('mwan3 member "%s" is not present or enabled.').format(memberName);
	if (member.device !== target)
		return _('mwan3 member "%s" resolves to %s, but this instance targets %s.').format(memberName, member.device, target);
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

	return _('Autorate requires an enabled external SQM queue when Manage SQM is disabled.');
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

	result = validateAdaptiveCeiling(section, section_id);
	if (result !== true)
		return result;

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

	result = validateManagedSqmTargetUnique(section, section_id);
	if (result !== true)
		return result;

	result = validateRouteSelection(section, section_id);
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

function syncManagedSqmEnabled(section, section_id, enabledOverride) {
	var enabled = enabledOverride != null ?
		(enabledOverride === true || enabledOverride === '1') :
		checkedFormOrUci(section, section_id, 'enabled', false);

	if (!checkedFormOrUci(section, section_id, 'manage_sqm', true))
		return;

	setCakeOption(section, section_id, 'sqm_enabled', enabled ? '1' : '0');
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

function speedtestServerTitle(result) {
	var parts = [];

	if (!result)
		return '';

	if (result.server_sponsor)
		parts.push(result.server_sponsor);
	else if (result.server_name)
		parts.push(result.server_name);

	if (result.server_id)
		parts.push('#' + result.server_id);

	return parts.join(' ');
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
		if (speedtestServerTitle(last.result))
			lines.push(_('Test server: %s.').format(speedtestServerTitle(last.result)));
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

function speedtestJobDelay() {
	return new Promise(function(resolve) {
		window.setTimeout(resolve, 1000);
	});
}

var AUTOTUNE_RECOVERY_MAX_POLLS = 12;
var AUTOTUNE_RECOVERY_MAX_DELAY_MS = 5000;
var AUTOTUNE_RESULT_SCHEMA_VERSION = 5;
var AUTOTUNE_RESULT_PRODUCER = 'cake-autorate-rs-autotune';

function canonicalAutotuneProfile(value) {
	switch (value) {
	case 'gaming':
		return 'gaming';
	case 'balanced':
	case 'best-overall':
	case 'best_overall':
		return 'best_overall';
	case 'fair':
		return 'fair';
	default:
		return null;
	}
}

function autotuneProfilePolicy(value) {
	var profile = canonicalAutotuneProfile(value);

	switch (profile) {
	case 'gaming':
		return {
			id: profile,
			targetGrade: 'A+',
			qualityTargetRequired: true,
			throughputPriority: false,
			retentionPercent: 70,
			delayMaxMs: 5,
			lossMaxPercent: 1,
			cpuMaxPercent: 85,
			sqm: {
				qdisc: 'cake',
				script: 'layer_cake.qos',
				classification: 'diffserv4',
				squashDscp: false,
				squashIngress: false,
				ingressEcn: 'ECN',
				egressEcn: 'NOECN',
				iqdiscOpts: 'diffserv4',
				eqdiscOpts: 'diffserv4'
			}
		};
	case 'best_overall':
		return {
			id: profile,
			targetGrade: 'A',
			qualityTargetRequired: true,
			throughputPriority: false,
			retentionPercent: 80,
			delayMaxMs: 30,
			lossMaxPercent: 3,
			cpuMaxPercent: 85,
			sqm: {
				qdisc: 'cake',
				script: 'layer_cake.qos',
				classification: 'diffserv4',
				squashDscp: true,
				squashIngress: true,
				ingressEcn: 'ECN',
				egressEcn: 'NOECN',
				iqdiscOpts: 'besteffort',
				eqdiscOpts: 'diffserv4'
			}
		};
	case 'fair':
		return {
			id: profile,
			targetGrade: 'C',
			qualityTargetRequired: false,
			throughputPriority: true,
			retentionPercent: 90,
			delayMaxMs: 200,
			lossMaxPercent: 5,
			cpuMaxPercent: 85,
			sqm: {
				qdisc: 'cake',
				script: 'layer_cake.qos',
				classification: 'diffserv4',
				squashDscp: true,
				squashIngress: true,
				ingressEcn: 'ECN',
				egressEcn: 'NOECN',
				iqdiscOpts: 'besteffort',
				eqdiscOpts: 'diffserv4'
			}
		};
	default:
		return null;
	}
}

function autotuneProfileDefinitions() {
	return [
		{
			id: 'gaming',
			title: _('Gaming'),
			target: _('Target A+ · under 5 ms loaded-latency increase'),
			description: _('Maximum latency headroom with a 70% capacity safety floor. Uses diffserv4, supports optional native outbound rules, and preserves ingress DSCP.')
		},
		{
			id: 'best_overall',
			title: _('Best overall'),
			target: _('Target A or better · under 30 ms'),
			description: _('Recommended balance with an 80% capacity safety floor. Optional outbound rules use diffserv4 while download stays best effort.')
		},
		{
			id: 'fair',
			title: _('Fair'),
			target: _('Throughput first · aim for C or better · under 200 ms'),
			description: _('Keeps at least 90% of measured capacity. Optional outbound rules use diffserv4, download stays best effort, and Review may offer an evidence-backed option to disable SQM.')
		}
	];
}

function autotuneProposalMatchesProfile(result) {
	var proposal = result && result.proposal;
	var profile = canonicalAutotuneProfile(result && result.profile);
	var policy = autotuneProfilePolicy(profile);
	var validation = proposal && proposal.validation;
	var thresholds = result && result.validation_thresholds;
	var sqm = proposal && proposal.sqm;
	var sameNumber = function(first, second) {
		first = autotuneNumber(first);
		second = autotuneNumber(second);
		return first != null && second != null && Math.abs(first - second) < 0.000001;
	};

	if (!proposal || !policy || autotuneNumber(proposal.schema_version) !== 3 ||
	    canonicalAutotuneProfile(proposal.profile) !== profile ||
	    proposal.target_grade !== policy.targetGrade ||
	    proposal.quality_target_required !== policy.qualityTargetRequired ||
	    proposal.throughput_priority !== policy.throughputPriority ||
	    !validation || !thresholds || !sqm)
		return false;

	if (!sameNumber(validation.candidate_realization_min_percent, 80) ||
	    !sameNumber(validation.candidate_realization_max_percent, 110) ||
	    !sameNumber(validation.capacity_retention_min_percent, policy.retentionPercent) ||
	    !sameNumber(validation.icmp_delta_max_ms, policy.delayMaxMs) ||
	    !sameNumber(validation.transport_delta_max_ms, policy.delayMaxMs) ||
	    !sameNumber(validation.loss_max_percent, policy.lossMaxPercent) ||
	    !sameNumber(validation.cpu_max_percent, policy.cpuMaxPercent) ||
	    !sameNumber(thresholds.candidate_realization_min_percent, 80) ||
	    !sameNumber(thresholds.candidate_realization_max_percent, 110) ||
	    !sameNumber(thresholds.capacity_retention_min_percent, policy.retentionPercent) ||
	    !sameNumber(thresholds.delay_max_ms, policy.delayMaxMs) ||
	    !sameNumber(thresholds.loss_max_percent, policy.lossMaxPercent) ||
	    !sameNumber(thresholds.cpu_max_percent, policy.cpuMaxPercent))
		return false;

	return sqm.qdisc === policy.sqm.qdisc &&
		sqm.script === policy.sqm.script &&
		sqm.classification === policy.sqm.classification &&
		sqm.squash_dscp === policy.sqm.squashDscp &&
		sqm.squash_ingress === policy.sqm.squashIngress &&
		sqm.ingress_ecn === policy.sqm.ingressEcn &&
		sqm.egress_ecn === policy.sqm.egressEcn &&
		(sqm.iqdisc_opts || '') === policy.sqm.iqdiscOpts &&
		(sqm.eqdisc_opts || '') === policy.sqm.eqdiscOpts;
}

function autotuneJobDelay(delayMs) {
	return new Promise(function(resolve) {
		window.setTimeout(resolve, delayMs);
	});
}

function autotuneRuntimeSettled(result) {
	return !!(result && result.recovery_pending === false &&
		result.runtime_restored === true);
}

function autotuneLegacyResult(result) {
	var schema;

	if (!result)
		return null;
	if (result.state === 'legacy' && result.legacy_result)
		return result.legacy_result;
	if (result.recovery_pending === true || result.runtime_restored === false)
		return null;
	if (result.state === 'running' || result.state === 'cancelling' ||
	    result.state === 'recovering' || result.state === 'recovery-pending' ||
	    result.state === 'idle')
		return null;
	schema = autotuneNumber(result.schema_version);
	if (schema != null && (schema < AUTOTUNE_RESULT_SCHEMA_VERSION ||
	    result.producer !== AUTOTUNE_RESULT_PRODUCER))
		return result;
	if (schema == null && (result.proposal || result.validation ||
	    Array.isArray(result.validation_attempts)))
		return result;
	return null;
}

function autotuneRecoveryPending(result) {
	return !!(result && (result.recovery_pending === true ||
		result.runtime_restored === false));
}

function autotuneRecoveryProgress(result) {
	var progress = {};

	for (var key in (result || {}))
		if (Object.prototype.hasOwnProperty.call(result, key) && key !== 'error')
			progress[key] = result[key];

	progress.state = 'recovering';
	progress.phase = 'recovery';
	progress.progress = 0;
	progress.message = result && result.recovery_message ? result.recovery_message :
		_('Restoring the previous SQM and autorate runtime state...');

	return progress;
}

function runSpeedtestJob(section_id, wan, backend, onProgress, routeMode, mwan3Member) {
	var command = '/usr/libexec/cake-autorate-rs/speedtest';

	return fs.exec(command, [ section_id, wan, 'job-start', backend, '', routeMode || '', mwan3Member || '' ]).then(function(res) {
		var started = parseExecJson(res);

		if (started.error)
			throw new Error(started.error);

		if (started.state !== 'running')
			throw new Error(_('Unable to start the speed test job.'));

		var poll = function() {
			return speedtestJobDelay().then(function() {
				return fs.exec(command, [ section_id, wan, 'job-status', backend ]);
			}).then(function(status) {
				var result = parseExecJson(status);

				if (result.state === 'running') {
					if (onProgress)
						onProgress(result);
					return poll();
				}

				if (result.error)
					throw new Error(result.error);

				return { stdout: JSON.stringify(result) };
			});
		};

		return poll();
	});
}

function runAutotuneJob(section_id, wan, backend, onProgress, routeMode, mwan3Member,
		profile, conservative) {
	var command = '/usr/libexec/cake-autorate-rs/autotune';
	var action = conservative ? 'start-conservative' : 'start';
	profile = canonicalAutotuneProfile(profile) || 'best_overall';

	return fs.exec(command, [ section_id, wan, action, backend, routeMode || '',
		mwan3Member || '', profile, conservative ? '1' : '0' ]).then(function(res) {
		var started = parseExecJson(res);

		if (started.error && !autotuneRecoveryPending(started))
			throw new Error(started.error);

		var recoveryPolls = 0;
		var pollDelayMs = 1000;
		var poll = function() {
			return autotuneJobDelay(pollDelayMs).then(function() {
				return fs.exec(command, [ section_id, wan, 'status', backend,
					routeMode || '', mwan3Member || '', profile, conservative ? '1' : '0' ]);
			}).then(function(status) {
				var result = parseExecJson(status);
				var active = result.state === 'running' || result.state === 'cancelling';
				var settled = autotuneRuntimeSettled(result);
				var legacy = autotuneLegacyResult(result);

				/* Package upgrades can leave a RAM-only RC16 terminal file until
				 * the next start.  It is settled diagnostics, not recovery and
				 * never a current proposal. */
				if (legacy) {
					var legacyError = new Error(result.error ||
						_('Saved Auto-Tune diagnostics use an older result schema. Run Full Auto-Tune again.'));
					legacyError.autotuneResult = result.state === 'legacy' ? result : {
						state: 'legacy',
						schema_version: AUTOTUNE_RESULT_SCHEMA_VERSION,
						producer: AUTOTUNE_RESULT_PRODUCER,
						legacy_schema_version: result.schema_version == null ? 'unknown' : result.schema_version,
						legacy_state: result.state || 'unknown',
						legacy_result: result,
						error: legacyError.message,
						auto_apply_eligible: false,
						configuration_written: false,
						runtime_restored: true,
						recovery_pending: false
					};
					throw legacyError;
				}

				/* A terminal error is authoritative only after the recovery helper
				 * has restored runtime state and published both completion flags.
				 * Check it before stale state=running/progress=87 fields. */
				if (settled && result.error) {
					var error = new Error(result.error);
					error.autotuneResult = result;
					throw error;
				}

				/* A running payload without recovery flags is a normal active job.
				 * Once either recovery flag says otherwise, progress=87 is stale:
				 * clear it and use bounded exponential-backoff recovery polling. */
				if (active && !autotuneRecoveryPending(result)) {
					recoveryPolls = 0;
					pollDelayMs = 1000;
					if (onProgress)
						onProgress(result);
					return poll();
				}

				if (!settled) {
					recoveryPolls++;
					if (onProgress)
						onProgress(autotuneRecoveryProgress(result));

					if (recoveryPolls >= AUTOTUNE_RECOVERY_MAX_POLLS) {
						var pending = new Error(_('Runtime recovery is still pending; no Auto-Tune result was accepted.'));
						pending.autotuneRecoveryPending = true;
						pending.autotuneRecoveryStatus = result;
						throw pending;
					}

					pollDelayMs = Math.min(1000 * Math.pow(2, recoveryPolls),
						AUTOTUNE_RECOVERY_MAX_DELAY_MS);
					return poll();
				}

				if (!autotuneResultHasReviewChoice(result)) {
					var invalid = new Error(_('Full Auto-Tune ended without a safe reviewable result.'));
					invalid.autotuneResult = result;
					throw invalid;
				}

				return result;
			});
		};

		return poll();
	});
}

function cancelAutotuneJob(section_id, wan, backend, profile) {
	return fs.exec('/usr/libexec/cake-autorate-rs/autotune', [
		section_id,
		wan,
		'cancel',
		backend || 'auto',
		'',
		'',
		canonicalAutotuneProfile(profile) || 'best_overall'
	]).then(parseExecJson);
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

function runPingerPlan(section_id, mode, routeMode, mwan3Member) {
	return fs.exec('/usr/libexec/cake-autorate-rs/pinger-plan', [
		section_id,
		mode || 'status',
		'',
		routeMode || '',
		mwan3Member || ''
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

function targetInterfaceLabel(name) {
	var networks = interfaceContext.deviceNetworks[name] || [];
	var logical = networks.filter(function(networkName) { return !/(?:_?6)$/.test(networkName); })[0] || networks[0];
	var physical = interfaceContext.devicePhysical && interfaceContext.devicePhysical[name];
	var parts = [];

	if (logical)
		parts.push(logical);
	parts.push(name);
	if (physical && physical !== name)
		parts.push(physical);
	return parts.join(' \u2014 ');
}

function interfacePathLabel(logical, device) {
	var physical = interfaceContext.devicePhysical && interfaceContext.devicePhysical[device];
	var parts = [ logical ];

	if (device && device !== logical)
		parts.push(device);
	if (physical && physical !== device)
		parts.push(physical);
	return parts.join(' \u2014 ');
}

function targetInterfaceChoiceOptions() {
	return targetInterfaceChoices().map(function(name) {
		return [ name, targetInterfaceLabel(name) ];
	});
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

function importSqmQueueIntoState(state, allowReuse) {
	var queue = allowReuse === false ? null : findImportableSqmQueueForInterface(state.wan_if);

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
	var autotuneAction = state.autotune_action || 'apply_sqm';
	/* Auto-Tune data is untrusted until the complete result passes the same
	 * fail-closed predicate used by Next, Review and Apply.  Keep this guard at
	 * the staging boundary as a final defence against stale wizard state. */
	if ((state.mode === 'autotune' ||
	    (state.autotune_result && state.autotune_proposal)) &&
	    !autotuneResultReviewable(state.autotune_result, autotuneAction))
		throw new Error(_('Refusing to stage an unvalidated Auto-Tune proposal.'));
	if (autotuneAction === 'keep_current')
		throw new Error(_('Keeping the current settings must not create a configuration transaction.'));
	if (autotuneAction === 'disable_sqm') {
		if (!uci.get('cake-autorate', section_id))
			throw new Error(_('SQM can be disabled only for an existing instance.'));
		/* Preserve every learned and user-edited parameter. The guarded service
		 * restart disables this instance and its owned queue, then proves that
		 * the daemon, CAKE qdiscs, redirect and IFB are all gone. */
		uci.set('cake-autorate', section_id, 'enabled', '0');
		uci.set('cake-autorate', section_id, 'sqm_enabled', '0');
		state.enabled = false;
		state.sqm_enabled = false;
		return;
	}

	var wan = normalizeInterfaceName(state.wan_if);
	var dl = rateValue(state.sqm_download, '20000');
	var ul = rateValue(state.sqm_upload, '20000');
	var sqmSection = state.sqm_section || managedSqmSectionName(section_id);
	var pingExtraArgs = state.ping_extra_args || pingerInterfaceArgs(wan, state.pinger_method || 'fping');
	var selectedAutotuneProfile = state.autotune_proposal ?
		canonicalAutotuneProfile(state.autotune_proposal.profile) :
		canonicalAutotuneProfile(state.autotune_profile);

	uci.set('cake-autorate', section_id, 'enabled', state.enabled ? '1' : '0');
	uci.set('cake-autorate', section_id, 'wan_if', wan);
	uci.set('cake-autorate', section_id, 'route_mode', state.route_mode || 'main');
	if (state.mwan3_member)
		uci.set('cake-autorate', section_id, 'mwan3_member', state.mwan3_member);
	else
		uci.unset('cake-autorate', section_id, 'mwan3_member');
	uci.unset('cake-autorate', section_id, 'ping_prefix_string');
	uci.set('cake-autorate', section_id, 'auto_interface_preset', '1');
	uci.set('cake-autorate', section_id, 'adjust_dl_shaper_rate', '1');
	uci.set('cake-autorate', section_id, 'adjust_ul_shaper_rate', '1');
	uci.set('cake-autorate', section_id, 'manage_sqm', '1');
	uci.set('cake-autorate', section_id, 'sqm_section', sqmSection);
	uci.set('cake-autorate', section_id, 'sqm_enabled', state.enabled ? '1' : '0');
	uci.set('cake-autorate', section_id, 'autotune_profile',
		selectedAutotuneProfile || 'best_overall');
	uci.set('cake-autorate', section_id, 'speedtest_backend', state.speedtest_backend || 'auto');
	if (state.speedtest_go_server_id)
		uci.set('cake-autorate', section_id, 'speedtest_go_server_id', state.speedtest_go_server_id);
	else
		uci.unset('cake-autorate', section_id, 'speedtest_go_server_id');
	uci.set('cake-autorate', section_id, 'speedtest_apply_percent', String(state.speedtest_apply_percent || '90'));
	uci.set('cake-autorate', section_id, 'pinger_method', state.pinger_method || 'fping');
	uci.set('cake-autorate', section_id, 'no_pingers', String(state.no_pingers || '6'));
	if (pingExtraArgs)
		uci.set('cake-autorate', section_id, 'ping_extra_args', pingExtraArgs);
	uci.set('cake-autorate', section_id, 'reflector', (state.reflectors && state.reflectors.length) ? state.reflectors : defaultReflectors());
	uci.set('cake-autorate', section_id, 'manual_rate_limits', state.autotune_proposal ? '1' : '0');
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

	if (state.autotune_proposal) {
		var proposal = state.autotune_proposal;
		var dlProposal = proposal.download;
		var ulProposal = proposal.upload;
		var thresholds = proposal.thresholds_ms;
		var adaptive = adaptiveCeilingWritePlan(state, proposal);
		var validationPolicy = proposal.validation;
		var sqmPolicy = proposal.sqm;

		uci.set('cake-autorate', section_id, 'min_dl_shaper_rate_kbps', String(dlProposal.minimum_kbps));
		uci.set('cake-autorate', section_id, 'base_dl_shaper_rate_kbps', String(dlProposal.base_kbps));
		uci.set('cake-autorate', section_id, 'max_dl_shaper_rate_kbps', String(dlProposal.maximum_kbps));
		uci.set('cake-autorate', section_id, 'min_ul_shaper_rate_kbps', String(ulProposal.minimum_kbps));
		uci.set('cake-autorate', section_id, 'base_ul_shaper_rate_kbps', String(ulProposal.base_kbps));
		uci.set('cake-autorate', section_id, 'max_ul_shaper_rate_kbps', String(ulProposal.maximum_kbps));
		uci.set('cake-autorate', section_id, 'connection_active_thr_kbps', String(proposal.active_threshold_kbps));
		uci.set('cake-autorate', section_id, 'dl_avg_owd_delta_max_adjust_up_thr_ms', String(thresholds.adjust_up));
		uci.set('cake-autorate', section_id, 'ul_avg_owd_delta_max_adjust_up_thr_ms', String(thresholds.adjust_up));
		uci.set('cake-autorate', section_id, 'dl_owd_delta_delay_thr_ms', String(thresholds.delay));
		uci.set('cake-autorate', section_id, 'ul_owd_delta_delay_thr_ms', String(thresholds.delay));
		uci.set('cake-autorate', section_id, 'dl_avg_owd_delta_max_adjust_down_thr_ms', String(thresholds.adjust_down));
		uci.set('cake-autorate', section_id, 'ul_avg_owd_delta_max_adjust_down_thr_ms', String(thresholds.adjust_down));
		uci.set('cake-autorate', section_id, 'adaptive_ceiling_enabled', adaptive.enabled ? '1' : '0');
		uci.set('cake-autorate', section_id, 'adaptive_ceiling_dl_cap_kbps', String(adaptive.dl_cap_kbps));
		uci.set('cake-autorate', section_id, 'adaptive_ceiling_ul_cap_kbps', String(adaptive.ul_cap_kbps));
		uci.set('cake-autorate', section_id, 'adaptive_ceiling_hold_time_s', String(adaptive.hold_s));
		uci.set('cake-autorate', section_id, 'adaptive_ceiling_growth_percent', String(adaptive.growth_percent));
		uci.set('cake-autorate', section_id, 'adaptive_ceiling_probe_duration_s', String(adaptive.probe_s));
		uci.set('cake-autorate', section_id, 'adaptive_ceiling_cooldown_s', String(adaptive.cooldown_s));
		uci.set('cake-autorate', section_id, 'adaptive_ceiling_failed_bound_ttl_s', String(adaptive.failed_bound_ttl_s));
		uci.set('cake-autorate', section_id, 'transport_latency_enabled', '1');
		uci.set('cake-autorate', section_id, 'throughput_guard_enabled', '1');
		uci.set('cake-autorate', section_id, 'throughput_guard_retention_percent',
			String(validationPolicy.capacity_retention_min_percent));
		uci.set('cake-autorate', section_id, 'quality_target_delay_ms',
			String(validationPolicy.transport_delta_max_ms));
		uci.set('cake-autorate', section_id, 'throughput_reference_dl_p20_kbps', String(dlProposal.observed_low_kbps));
		uci.set('cake-autorate', section_id, 'throughput_reference_dl_p50_kbps', String(dlProposal.observed_median_kbps));
		uci.set('cake-autorate', section_id, 'throughput_reference_ul_p20_kbps', String(ulProposal.observed_low_kbps));
		uci.set('cake-autorate', section_id, 'throughput_reference_ul_p50_kbps', String(ulProposal.observed_median_kbps));
		state.autotune_profile = canonicalAutotuneProfile(proposal.profile) || 'best_overall';
		uci.set('cake-autorate', section_id, 'autotune_profile', state.autotune_profile);
		state.sqm_qdisc = sqmPolicy.qdisc;
		state.sqm_script = sqmPolicy.script;
		state.sqm_qdisc_advanced = '1';
		state.sqm_qdisc_really_really_advanced = '1';
		state.sqm_squash_dscp = sqmPolicy.squash_dscp ? '1' : '0';
		state.sqm_squash_ingress = sqmPolicy.squash_ingress ? '1' : '0';
		state.sqm_ingress_ecn = sqmPolicy.ingress_ecn;
		state.sqm_egress_ecn = sqmPolicy.egress_ecn;
		state.sqm_iqdisc_opts = sqmPolicy.iqdisc_opts || '';
		state.sqm_eqdisc_opts = sqmPolicy.eqdisc_opts || '';
	}

	for (var i = 0; i < sqmImportOptionMap.length; i++) {
		var key = sqmImportOptionMap[i][0];
		var fallback = sqmImportOptionMap[i][2];
		var value = state[key] != null ? state[key] : fallback;

		if (value !== '')
			uci.set('cake-autorate', section_id, key, String(value));
		else
			uci.unset('cake-autorate', section_id, key);
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

function autotuneNumber(value) {
	if (value == null || value === '' || typeof value === 'boolean')
		return null;
	value = Number(value);
	return isFinite(value) ? value : null;
}

function autotunePercent(numerator, denominator) {
	numerator = autotuneNumber(numerator);
	denominator = autotuneNumber(denominator);

	if (numerator == null || denominator == null || denominator <= 0)
		return null;

	return Math.round(numerator * 1000 / denominator) / 10;
}

function firstAutotuneNumber(values) {
	for (var i = 0; i < values.length; i++) {
		var value = autotuneNumber(values[i]);
		if (value != null)
			return value;
	}

	return null;
}

function autotuneGateValue(validation, names) {
	var gates = validation && validation.gates;
	if (!gates)
		return null;
	var normalize = function(value) {
		return String(value || '').toLowerCase().replace(/_/g, '-');
	};
	var wanted = names.map(normalize);
	var matched = [];

	if (Array.isArray(gates)) {
		for (var i = 0; i < gates.length; i++) {
			var item = gates[i];
			if (!item || wanted.indexOf(normalize(item.code || item.id || item.name)) < 0)
				continue;
			if (typeof item.pass === 'boolean')
				matched.push(item.pass);
			else if (typeof item.passed === 'boolean')
				matched.push(item.passed);
		}
	}
	else {
		for (var j = 0; j < names.length; j++) {
			var candidates = [ names[j], normalize(names[j]), names[j].replace(/-/g, '_') ];
			var gate;
			for (var k = 0; k < candidates.length; k++) {
				if (Object.prototype.hasOwnProperty.call(gates, candidates[k])) {
					gate = gates[candidates[k]];
					break;
				}
			}
			if (typeof gate === 'boolean')
				matched.push(gate);
			else if (gate && typeof gate.pass === 'boolean')
				matched.push(gate.pass);
			else if (gate && typeof gate.passed === 'boolean')
				matched.push(gate.passed);
		}
	}

	return matched.length ? matched.every(function(pass) { return pass; }) : null;
}

function autotuneGateMetric(validation, names) {
	var gates = validation && validation.gates;
	if (!Array.isArray(gates))
		return null;
	var wanted = names.map(function(value) {
		return String(value || '').toLowerCase().replace(/_/g, '-');
	});

	for (var i = 0; i < gates.length; i++) {
		var gate = gates[i];
		var code = gate && String(gate.code || gate.id || gate.name || '').toLowerCase().replace(/_/g, '-');
		if (wanted.indexOf(code) >= 0)
			return autotuneNumber(gate.actual);
	}

	return null;
}

function autotuneValidationAttempts(result) {
	var attempts = result && Array.isArray(result.validation_attempts) ?
		result.validation_attempts.slice() : [];

	if (!attempts.length && result && result.validation)
		attempts.push(result.validation);

	return attempts;
}

function autotuneValidationGatesComplete(validation, profile, requireQualityTarget) {
	var gates = validation && validation.gates;
	var policy = autotuneProfilePolicy(profile);
	var required = [
		'download-candidate-realization', 'upload-candidate-realization',
		'download-candidate-realization-maximum', 'upload-candidate-realization-maximum',
		'download-capacity-retention', 'upload-capacity-retention',
		'download-icmp-latency', 'download-transport-latency',
		'download-packet-loss', 'download-cpu',
		'upload-icmp-latency', 'upload-transport-latency',
		'upload-packet-loss', 'upload-cpu'
	];
	var reported = {};

	if (!policy || !Array.isArray(gates) || gates.length !== required.length ||
	    canonicalAutotuneProfile(validation.profile) !== policy.id)
		return false;

	for (var i = 0; i < gates.length; i++) {
		var gate = gates[i];
		var code = gate && String(gate.code || '').toLowerCase().replace(/_/g, '-');
		var latencyGate = code === 'download-icmp-latency' ||
			code === 'download-transport-latency' ||
			code === 'upload-icmp-latency' ||
			code === 'upload-transport-latency';
		var gateRequired = latencyGate ? policy.qualityTargetRequired : true;

		if (required.indexOf(code) < 0 || reported[code] ||
		    gate.required !== gateRequired || typeof gate.pass !== 'boolean' ||
		    (gateRequired && gate.pass !== true) ||
		    (requireQualityTarget && gate.pass !== true) ||
		    autotuneNumber(gate.actual) == null || autotuneNumber(gate.limit) == null)
			return false;
		reported[code] = true;
	}

	return required.every(function(code) { return reported[code] === true; });
}

function autotuneBackgroundEvidenceClean(background) {
	return !!(background && background.available === true &&
		background.contaminated === false);
}

function autotunePhaseEvidenceClean(result) {
	var entries = result && result.phase_background;
	var validation = result && result.validation;
	var directionPhases = validation && validation.direction_phases;
	var directions = [ 'download', 'upload' ];
	var cleanBaselineSeen = false;

	/* A clean current-schema run has an idle baseline, two unshaped samples, and a
	 * download-only plus upload-only shaped phase.  Treat missing evidence as
	 * incomplete rather than trusting a top-level boolean. */
	if (!Array.isArray(entries) || entries.length < 5)
		return false;

	for (var i = 0; i < entries.length; i++) {
		var entry = entries[i];
		if (!autotuneBackgroundEvidenceClean(entry && entry.forwarded_background))
			return false;
		if (entry && entry.phase === 'baseline' && entry.icmp_valid === true &&
		    entry.transport_valid === true)
			cleanBaselineSeen = true;
	}
	if (!cleanBaselineSeen)
		return false;

	if (!directionPhases)
		return false;

	for (var j = 0; j < directions.length; j++) {
		var direction = directions[j];
		var phase = directionPhases[direction];
		if (!phase || phase.direction !== direction ||
		    autotuneNumber(phase.throughput_kbps) == null ||
		    autotuneNumber(phase.throughput_kbps) <= 0 ||
		    !autotuneBackgroundEvidenceClean(phase.forwarded_background) ||
		    autotuneNumber(phase.icmp_latency && phase.icmp_latency.samples) == null ||
		    autotuneNumber(phase.icmp_latency && phase.icmp_latency.samples) <= 0 ||
		    autotuneNumber(phase.transport_latency && phase.transport_latency.samples) == null ||
		    autotuneNumber(phase.transport_latency && phase.transport_latency.samples) <= 0 ||
		    autotuneNumber(phase.cpu_peak_percent) == null)
			return false;
	}

	return true;
}

function autotuneHasInfeasibleDecision(result) {
	var attempts = autotuneValidationAttempts(result);

	for (var i = 0; i < attempts.length; i++) {
		var correction = attempts[i] && attempts[i].correction;
		if (!correction && attempts[i] && attempts[i].decision)
			correction = attempts[i].decision.correction;
		if (correction && (correction.action === 'infeasible' ||
		    correction.download && correction.download.action === 'infeasible' ||
		    correction.upload && correction.upload.action === 'infeasible'))
			return true;
	}

	return false;
}

function autotuneFairAllowedActions(outcome) {
	var actions = outcome && outcome.allowed_actions;
	var known = [ 'apply_sqm', 'keep_current', 'disable_sqm' ];
	var seen = {};

	if (!Array.isArray(actions) || !actions.length)
		return null;
	for (var i = 0; i < actions.length; i++) {
		if (known.indexOf(actions[i]) < 0 || seen[actions[i]])
			return null;
		seen[actions[i]] = true;
	}
	if (!seen.apply_sqm || !seen.keep_current ||
	    !!seen.disable_sqm !== (outcome.disable_sqm_available === true))
		return null;
	return seen;
}

function autotuneFairOutcomeValidated(result) {
	var outcome = result && result.fair_outcome;
	var validation = result && result.validation;
	var actions = autotuneFairAllowedActions(outcome);
	var outcomeDelta = autotuneNumber(outcome && outcome.actual_effective_delta_ms);
	var validationDelta = autotuneNumber(validation && validation.effective_delta_ms);

	if (canonicalAutotuneProfile(result && result.profile) !== 'fair' ||
	    !outcome || !validation || !actions ||
	    outcome.target_grade !== 'C' ||
	    autotuneNumber(outcome.target_delta_ms) !== 200 ||
	    autotuneNumber(outcome.capacity_floor_percent) !== 90 ||
	    outcome.actual_grade !== validation.actual_grade ||
	    outcomeDelta == null || validationDelta == null ||
	    Math.abs(outcomeDelta - validationDelta) > 0.001)
		return false;

	if (validation.quality_target_met === true)
		return outcome.mode === 'quality-target-met' &&
			outcome.recommended_action === 'apply_sqm' &&
			outcome.disable_sqm_available === false;

	return (outcome.mode === 'throughput-fallback' ||
		outcome.mode === 'sqm-disable-recommended') &&
		(actions[outcome.recommended_action] === true);
}

function autotuneResultEvidenceValidated(result) {
	if (!(result && result.state === 'complete' && result.proposal &&
		result.validation && !result.error))
		return false;
	if (!result.job_id || !result.target_interface || !result.resolved_interface ||
	    !result.route_interface || !result.source_ip || !result.route_identity || !result.external_ip ||
	    (result.route_mode !== 'main' && result.route_mode !== 'mwan3'))
		return false;

	/* Current results are intentionally fail-closed. A bare legacy `pass` is
	 * not enough because it did not prove phase-scoped background telemetry or
	 * bind the review to one immutable run. */
	if (autotuneNumber(result.schema_version) !== AUTOTUNE_RESULT_SCHEMA_VERSION ||
	    result.producer !== AUTOTUNE_RESULT_PRODUCER ||
	    !/^[A-Za-z0-9_-]{1,128}$/.test(result.run_id || '') ||
	    result.phase_evidence_complete !== true || result.runtime_restored !== true ||
	    result.recovery_pending !== false || result.manual_apply_eligible !== true ||
	    result.configuration_written !== false ||
	    !/^sha256:[0-9a-f]{64}$/.test(result.config_fingerprint || '') ||
	    !autotuneProposalMatchesProfile(result))
		return false;

	/* Conservative/contaminated output remains useful diagnostics, but it is
	 * never a configuration proposal. */
	if (result.conservative === true || result.confidence_mode !== 'normal' ||
	    result.phase_contamination_seen !== false ||
	    result.validation.contaminated !== false ||
	    !autotunePhaseEvidenceClean(result))
		return false;

	return true;
}

function autotuneResultValidated(result) {
	if (!autotuneResultEvidenceValidated(result) ||
	    result.validation.pass !== true ||
	    result.validation.hard_pass !== true ||
	    result.validation.quality_target_met !== true ||
	    result.auto_apply_eligible !== true ||
	    !autotuneValidationGatesComplete(result.validation, result.profile, true))
		return false;

	var correction = result.validation.correction;
	if (!(correction && correction.action === 'none' && correction.feasible === true))
		return false;

	return canonicalAutotuneProfile(result.profile) !== 'fair' ||
		autotuneFairOutcomeValidated(result);
}

function autotuneDisableSqmEvidenceValidated(result) {
	var outcome = result && result.fair_outcome;
	var control = outcome && outcome.no_sqm_control;
	var evidence = control && control.measurement_evidence;
	var gains = outcome && outcome.throughput_gain_without_sqm;
	var validation = result && result.validation;
	var grades = { 'A+': 0, 'A': 1, 'B': 2, 'C': 3, 'D': 4, 'F': 5 };
	var dlGain = autotuneNumber(gains && gains.download_percent);
	var ulGain = autotuneNumber(gains && gains.upload_percent);
	var controlDelta = autotuneNumber(control && control.effective_delta_ms);
	var shapedDelta = autotuneNumber(validation && validation.effective_delta_ms);

	return !!(autotuneFairOutcomeValidated(result) &&
		outcome.mode === 'sqm-disable-recommended' &&
		outcome.recommended_action === 'disable_sqm' &&
		outcome.disable_sqm_available === true &&
		control && control.available === true &&
		evidence && evidence.valid === true &&
		evidence.test_direction === 'both' &&
		evidence.shaper_bypassed === true &&
		evidence.sqm_paused === true &&
		autotuneBackgroundEvidenceClean(control.forwarded_background) &&
		autotuneNumber(control.forwarded_background.download_kbps) != null &&
		autotuneNumber(control.forwarded_background.upload_kbps) != null &&
		autotuneNumber(control.forwarded_background.download_limit_kbps) != null &&
		autotuneNumber(control.forwarded_background.upload_limit_kbps) != null &&
		autotuneNumber(control.forwarded_background.download_kbps) <=
			autotuneNumber(control.forwarded_background.download_limit_kbps) &&
		autotuneNumber(control.forwarded_background.upload_kbps) <=
			autotuneNumber(control.forwarded_background.upload_limit_kbps) &&
		Object.prototype.hasOwnProperty.call(grades, control.grade) &&
		Object.prototype.hasOwnProperty.call(grades, validation.actual_grade) &&
		grades[control.grade] <= grades[validation.actual_grade] &&
		controlDelta != null && shapedDelta != null &&
		controlDelta <= shapedDelta + 10 &&
		dlGain != null && ulGain != null && dlGain >= 2 && ulGain >= 2);
}

function autotuneResultReviewable(result, action) {
	action = action || 'apply_sqm';
	if (!autotuneResultEvidenceValidated(result))
		return false;

	if (action === 'keep_current')
		return autotuneResultValidated(result) ||
			(autotuneFairOutcomeValidated(result) &&
				autotuneFairAllowedActions(result.fair_outcome).keep_current === true);

	if (action === 'apply_sqm') {
		if (autotuneResultValidated(result))
			return true;
		return canonicalAutotuneProfile(result.profile) === 'fair' &&
			result.auto_apply_eligible === false &&
			result.validation.pass === false &&
			result.validation.hard_pass === true &&
			result.validation.quality_target_met === false &&
			autotuneValidationGatesComplete(result.validation, result.profile, false) &&
			autotuneFairOutcomeValidated(result) &&
			autotuneFairAllowedActions(result.fair_outcome).apply_sqm === true;
	}

	if (action === 'disable_sqm')
		return canonicalAutotuneProfile(result.profile) === 'fair' &&
			result.auto_apply_eligible === false &&
			result.validation.pass === false &&
			result.validation.hard_pass === true &&
			result.validation.quality_target_met === false &&
			autotuneValidationGatesComplete(result.validation, result.profile, false) &&
			autotuneDisableSqmEvidenceValidated(result);

	return false;
}

function autotuneResultHasReviewChoice(result) {
	return autotuneResultReviewable(result, 'apply_sqm') ||
		autotuneResultReviewable(result, 'keep_current') ||
		autotuneResultReviewable(result, 'disable_sqm');
}

function revalidateAutotuneProposal(section_id, wan, backend, expected, routeMode, mwan3Member,
		action) {
	action = action || 'apply_sqm';
	if (!autotuneResultReviewable(expected, action))
		return Promise.reject(new Error(_('The Auto-Tune proposal is no longer valid. Run Auto-Tune again.')));
	var selectedMode = routeMode === 'auto' ? (mwan3Member ? 'mwan3' : 'main') : routeMode;
	selectedMode = selectedMode || 'main';
	var selectedMember = selectedMode === 'mwan3' ? (mwan3Member || '') : '';
	if (expected.job_id !== section_id ||
	    normalizeInterfaceName(expected.resolved_interface) !== normalizeInterfaceName(wan) ||
	    expected.route_mode !== selectedMode ||
	    (expected.mwan3_member || '') !== selectedMember)
		return Promise.reject(new Error(_('The selected uplink no longer matches the validated Auto-Tune result. Run Auto-Tune again.')));

	var expectedFingerprint = expected.config_fingerprint;
	var expectedProfile = canonicalAutotuneProfile(expected.profile);
	return fs.exec('/usr/libexec/cake-autorate-rs/autotune', [
		section_id,
		wan,
		'status',
		backend || 'auto',
		selectedMode,
		selectedMember,
		expectedProfile || 'best_overall'
	]).then(function(status) {
		var current = parseExecJson(status);

		if (!autotuneResultReviewable(current, action) ||
		    current.config_fingerprint !== expectedFingerprint ||
		    current.run_id !== expected.run_id ||
		    current.job_id !== expected.job_id ||
		    current.resolved_interface !== expected.resolved_interface ||
		    current.route_identity !== expected.route_identity ||
		    JSON.stringify(current.proposal) !== JSON.stringify(expected.proposal))
			throw new Error(_('Configuration or Auto-Tune state changed after validation. Run Auto-Tune again before staging this proposal.'));

		return fs.exec('/usr/libexec/cake-autorate-rs/autotune', [
			section_id,
			wan,
			'attest',
			backend || 'auto',
			selectedMode,
			selectedMember,
			expectedProfile || 'best_overall'
		]).then(function(attestationStatus) {
			var attestation = parseExecJson(attestationStatus);

			if (!attestation || attestation.state !== 'ready' ||
			    autotuneNumber(attestation.schema_version) !== 1 ||
			    attestation.config_fingerprint !== expectedFingerprint ||
			    attestation.target_interface !== expected.target_interface ||
			    attestation.resolved_interface !== expected.resolved_interface ||
			    attestation.route_interface !== expected.route_interface ||
			    attestation.route_mode !== expected.route_mode ||
			    (attestation.mwan3_member || '') !== (expected.mwan3_member || '') ||
			    attestation.source_ip !== expected.source_ip ||
			    attestation.external_ip !== expected.external_ip ||
			    attestation.route_identity !== expected.route_identity)
				throw new Error(_('Configuration or selected uplink route changed after validation. Run Auto-Tune again before staging this proposal.'));

			return current;
		});
	});
}

var AUTOTUNE_APPLY_GUARD = '/usr/libexec/cake-autorate-rs/apply-guard';
var AUTOTUNE_APPLY_TIMEOUT_S = 30;
var UBUS_STATUS_NO_DATA = 5;
var callUciConfirmStatus = rpc.declare({
	object: 'uci',
	method: 'confirm',
	reject: false
});

function autotuneSqmRollbackSection(section_id) {
	return 'cake_autorate_apply_' + section_id;
}

function stageAutotuneApplyMarker(section_id, state) {
	var result = state && state.autotune_result;
	var action = state && state.autotune_action || 'apply_sqm';
	var enabled = action === 'apply_sqm';

	if (!autotuneResultReviewable(result, action) ||
	    (action !== 'apply_sqm' && action !== 'disable_sqm'))
		throw new Error(_('Refusing to create an apply marker for an invalid Full Auto-Tune result.'));
	if (result.job_id !== section_id)
		throw new Error(_('The Full Auto-Tune result belongs to a different instance.'));

	uci.set('cake-autorate', section_id, '_autotune_apply_guard', '1');
	uci.set('cake-autorate', section_id, '_autotune_apply_fingerprint', result.config_fingerprint);
	uci.set('cake-autorate', section_id, '_autotune_apply_target', result.target_interface);
	uci.set('cake-autorate', section_id, '_autotune_apply_backend',
		(result.runs && result.runs[0] && result.runs[0].backend) || state.speedtest_backend || 'speedtest-go');
	uci.set('cake-autorate', section_id, '_autotune_apply_route_mode', result.route_mode);
	if (result.route_mode === 'mwan3')
		uci.set('cake-autorate', section_id, '_autotune_apply_mwan3_member', result.mwan3_member);
	else
		uci.unset('cake-autorate', section_id, '_autotune_apply_mwan3_member');
	uci.set('cake-autorate', section_id, '_autotune_apply_enabled', enabled ? '1' : '0');
	uci.set('cake-autorate', section_id, '_autotune_apply_disable_adaptive',
		state.adaptive_ceiling_disable_confirmed === true ? '1' : '0');
	uci.set('cake-autorate', section_id, '_autotune_apply_action', action);
	/* A no-op metadata section enrolls the entire sqm package in rpcd's same
	 * rollback snapshot. Init commits its generated queue (and may disable a
	 * conflicting legacy queue), so protecting only cake-autorate would leave
	 * persistent SQM side effects after a failed validation. */
	var sqmGuard = autotuneSqmRollbackSection(section_id);
	if (uci.get('sqm', sqmGuard))
		throw new Error(_('The reserved SQM rollback section already exists. Reconcile or remove the stale section before applying.'));
	uci.add('sqm', 'cake_autorate_apply_guard', sqmGuard);
	uci.set('sqm', sqmGuard, '_autotune_apply_guard', '1');
	uci.set('sqm', sqmGuard, '_autotune_apply_job', section_id);
	uci.set('sqm', sqmGuard, '_autotune_apply_fingerprint', result.config_fingerprint);
	uci.unset('sqm', sqmGuard, '_autotune_apply_token');
	/* A token is minted only when this page itself starts the rollback-enabled
	 * apply.  Committing this global marker from another LuCI page therefore
	 * makes init fail closed before it can stop or rewrite SQM. */
	uci.unset('cake-autorate', section_id, '_autotune_apply_token');
	uci.unset('cake-autorate', section_id, '_autotune_apply_expires');
}

function pendingAutotuneApplyMarkers() {
	var sections = uci.sections('cake-autorate', 'cake_autorate') || [];
	var markers = [];

	for (var i = 0; i < sections.length; i++) {
		var section = sections[i];
		if (section._autotune_apply_guard !== '1')
			continue;

		var marker = {
			job: section['.name'],
			target: section._autotune_apply_target,
			backend: section._autotune_apply_backend,
			routeMode: section._autotune_apply_route_mode,
			member: section._autotune_apply_mwan3_member || '',
			enabled: section._autotune_apply_enabled,
			disableAdaptive: section._autotune_apply_disable_adaptive,
			action: section._autotune_apply_action,
			fingerprint: section._autotune_apply_fingerprint
		};
		if (!/^[A-Za-z0-9_]+$/.test(marker.job || '') ||
		    !/^[A-Za-z0-9_.:@-]+$/.test(marker.target || '') ||
		    (marker.backend !== 'auto' && marker.backend !== 'speedtest-go') ||
		    (marker.routeMode !== 'main' && marker.routeMode !== 'mwan3') ||
		    (marker.routeMode === 'main' && marker.member) ||
		    (marker.routeMode === 'mwan3' && !/^[A-Za-z0-9_.:@-]+$/.test(marker.member)) ||
		    (marker.enabled !== '0' && marker.enabled !== '1') ||
		    (marker.disableAdaptive !== '0' && marker.disableAdaptive !== '1') ||
		    (marker.action !== 'apply_sqm' && marker.action !== 'disable_sqm') ||
		    (marker.action === 'apply_sqm' && marker.enabled !== '1') ||
		    (marker.action === 'disable_sqm' && marker.enabled !== '0') ||
		    !/^sha256:[0-9a-f]{64}$/.test(marker.fingerprint || ''))
			throw new Error(_('A staged Full Auto-Tune apply marker is incomplete or unsafe. Run Auto-Tune again.'));

		markers.push(marker);
	}

	return markers;
}

function parseApplyGuardResult(result, expectedState) {
	if (!result || result.code !== 0)
		throw new Error(result && (result.stderr || result.stdout) || _('The Full Auto-Tune apply guard failed.'));

	var parsed = parseExecJson(result);
	if (!parsed || parsed.state !== expectedState || parsed.schema_version !== 1)
		throw new Error(_('The Full Auto-Tune apply guard returned an invalid response.'));

	return parsed;
}

function abortAutotuneApplyGuards(guards) {
	var tasks = [];
	for (var i = 0; i < (guards || []).length; i++)
		if (guards[i].token)
			tasks.push(L.resolveDefault(fs.exec(AUTOTUNE_APPLY_GUARD,
				[ 'abort', guards[i].token ]), null));
	return Promise.all(tasks);
}

function armAutotuneApplyGuards() {
	var markers = pendingAutotuneApplyMarkers();
	var armed = [];
	var chain = Promise.resolve();
	if (markers.length !== 1)
		return Promise.reject(new Error(_('Exactly one Full Auto-Tune proposal must be applied at a time.')));

	markers.forEach(function(marker) {
		chain = chain.then(function() {
			return fs.exec(AUTOTUNE_APPLY_GUARD, [
				'arm', marker.job, marker.target, marker.backend,
				marker.routeMode, marker.member, marker.enabled,
				marker.disableAdaptive, marker.action, marker.fingerprint
			]);
		}).then(function(result) {
			var parsed = parseApplyGuardResult(result, 'armed');
			if (!/^[0-9a-f]{64}$/.test(parsed.token || '') ||
			    !Number.isSafeInteger(parsed.expires_epoch) || parsed.expires_epoch <= 0 ||
			    !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(parsed.boot_id || ''))
				throw new Error(_('The Full Auto-Tune apply guard returned an invalid token.'));
			marker.token = parsed.token;
			marker.expires = String(parsed.expires_epoch);
			marker.bootId = parsed.boot_id;
			armed.push(marker);
		});
	});

	return chain.then(function() {
		for (var i = 0; i < armed.length; i++) {
			uci.set('cake-autorate', armed[i].job, '_autotune_apply_token', armed[i].token);
			uci.set('cake-autorate', armed[i].job, '_autotune_apply_expires', armed[i].expires);
			uci.set('cake-autorate', armed[i].job, '_autotune_apply_boot_id', armed[i].bootId);
			uci.set('sqm', autotuneSqmRollbackSection(armed[i].job),
				'_autotune_apply_token', armed[i].token);
		}
		return uci.save().then(function() {
			return uci.changes();
		}).then(function(changes) {
			var changedPackages = Object.keys(changes || {}).filter(function(config) {
				return changes[config] && changes[config].length;
			});
			if (changedPackages.indexOf('cake-autorate') < 0 || changedPackages.indexOf('sqm') < 0 ||
			    changedPackages.some(function(config) {
				    return config !== 'cake-autorate' && config !== 'sqm';
			    }))
				throw new Error(_('Full Auto-Tune Save & Apply must contain only its exact CAKE and SQM changes. Apply or revert all other pending changes first.'));
			return armed;
		});
	}).catch(function(error) {
		return abortAutotuneApplyGuards(armed).then(function() { throw error; });
	});
}

function applyGuardDelay(ms) {
	return new Promise(function(resolve) { window.setTimeout(resolve, ms); });
}

function verifyGuardOperation(guards, operation, expectedState) {
	var chain = Promise.resolve();
	guards.forEach(function(guard) {
		chain = chain.then(function() {
			return fs.exec(AUTOTUNE_APPLY_GUARD, [ operation, guard.token ]);
		}).then(function(result) {
			var parsed = parseApplyGuardResult(result, expectedState);
			if (parsed.token !== guard.token)
				throw new Error(_('The Full Auto-Tune apply guard verified a different transaction token.'));
		});
	});
	return chain;
}

function abortAutotuneApplyGuardsStrict(guards) {
	var chain = Promise.resolve();
	guards.forEach(function(guard) {
		chain = chain.then(function() {
			return fs.exec(AUTOTUNE_APPLY_GUARD, [ 'abort', guard.token ]);
		}).then(function(result) {
			parseApplyGuardResult(result, 'aborted');
		});
	});
	return chain;
}

function rollbackGuardedApply(guards, error, applyDeadlineMs) {
	/* Never call confirm on this path. rpcd restores the pre-apply UCI snapshot
	 * when the rollback timer expires. Keep the root-owned snapshots alive
	 * until both cake-autorate and sqm are proven restored, restart the old
	 * runtime, prove the files again, and only then invalidate the tokens. */
	var delayMs = Math.max(0, (applyDeadlineMs || Date.now()) + 2000 - Date.now());
	var transaction = applyGuardDelay(delayMs)
		.then(function() { return verifyGuardOperation(guards, 'verify-rollback', 'rolled-back'); })
		.then(function() { return applyGuardDelay(1000); })
		.then(function() { return verifyGuardOperation(guards, 'verify-rollback', 'rolled-back'); })
		.then(function() { return abortAutotuneApplyGuardsStrict(guards); });

	return transaction.then(function() {
		throw error;
	}, function(rollbackError) {
		var failure = new Error(_('Full Auto-Tune apply failed and the exact UCI rollback could not be verified: %s').format(
			rollbackError && rollbackError.message ? rollbackError.message : rollbackError));
		failure.applyError = error;
		failure.rollbackError = rollbackError;
		throw failure;
	});
}

function finalizeGuardedApply(guards) {
	var chain = Promise.resolve();
	guards.forEach(function(guard) {
		chain = chain.then(function() {
			return fs.exec(AUTOTUNE_APPLY_GUARD, [ 'finalize', guard.token ]);
		}).then(function(result) {
			parseApplyGuardResult(result, 'finalized');
		});
	});
	return chain;
}

function reconcilePreparedApply(guards) {
	var chain = Promise.resolve();
	var terminalState = null;
	guards.forEach(function(guard) {
		chain = chain.then(function() {
			return fs.exec(AUTOTUNE_APPLY_GUARD, [ 'reconcile', guard.token ]);
		}).then(function(result) {
			if (!result || result.code !== 0)
				throw new Error(result && (result.stderr || result.stdout) || _('Unable to reconcile the Full Auto-Tune transaction.'));
			var parsed = parseExecJson(result);
			if (!parsed || parsed.schema_version !== 1 || parsed.token !== guard.token ||
			    (parsed.state !== 'confirmed' && parsed.state !== 'rolled-back'))
				throw new Error(_('The Full Auto-Tune reconciliation response is invalid.'));
			if (terminalState && terminalState !== parsed.state)
				throw new Error(_('Full Auto-Tune guards disagree about the terminal transaction state.'));
			terminalState = parsed.state;
		});
	});
	return chain.then(function() { return terminalState; });
}

function reconcileAuthoritativeNoPending(guards, error, applyDeadlineMs) {
	var delayMs = Math.max(0, applyDeadlineMs + 2000 - Date.now());
	return applyGuardDelay(delayMs).then(function() {
		return reconcilePreparedApply(guards).then(function(state) {
			if (state === 'confirmed')
				return finalizeGuardedApply(guards);
			return abortAutotuneApplyGuardsStrict(guards).then(function() {
				error.transactionRolledBack = true;
				throw error;
		});
		});
	});
}

function confirmationIndeterminate(error, retryError) {
	var failure = new Error(_('The confirmation outcome remains unknown. The marker-free transaction proof was retained for explicit reconciliation.'));
	failure.confirmIndeterminate = true;
	failure.confirmError = error;
	failure.retryError = retryError;
	return failure;
}

function confirmPreparedApply(guards, applyDeadlineMs) {
	function finalizeConfirmed() {
		return finalizeGuardedApply(guards).catch(function(error) {
			error.applyConfirmed = true;
			throw error;
		});
	}

	function retryOnce(firstError) {
		return callUciConfirmStatus().then(function(status) {
			if (status === 0)
				return finalizeConfirmed();
			if (status === UBUS_STATUS_NO_DATA)
				return reconcileAuthoritativeNoPending(guards, firstError, applyDeadlineMs);
			throw confirmationIndeterminate(firstError,
				new Error(_('The confirmation retry returned ubus status %s.').format(status)));
		}, function(retryError) {
			throw confirmationIndeterminate(firstError, retryError);
		});
	}

	return callUciConfirmStatus().then(function(status) {
		if (status === 0)
			return finalizeConfirmed();
		return retryOnce(new Error(_('UCI could not confirm the verified Full Auto-Tune transaction.')));
	}, function(error) {
		return retryOnce(error);
	});
}

function guardedApplyStatus(guard) {
	return fs.exec(AUTOTUNE_APPLY_GUARD, [ 'status', guard.token ]).then(function(result) {
		if (!result || result.code !== 0)
			throw new Error(result && (result.stderr || result.stdout) ||
				_('Unable to read the server-side Full Auto-Tune apply state.'));
		var parsed = parseExecJson(result);
		var allowed = [ 'armed', 'applying', 'verified', 'confirming', 'running',
			'complete', 'rolled-back', 'failed', 'indeterminate' ];
		if (!parsed || parsed.schema_version !== 1 || parsed.token !== guard.token ||
		    allowed.indexOf(parsed.state) < 0)
			throw new Error(_('The server-side Full Auto-Tune apply state is invalid.'));
		return parsed;
	});
}

function waitForGuardedApplySupervisor(guards, applyDeadlineMs) {
	var deadline = applyDeadlineMs + 15000;

	function poll() {
		return Promise.all(guards.map(guardedApplyStatus)).then(function(states) {
			for (var i = 0; i < states.length; i++) {
				var state = states[i];
				if (state.state === 'rolled-back') {
					var rollback = new Error(state.message ||
						_('The router rejected the Full Auto-Tune configuration and restored the previous state.'));
					rollback.transactionRolledBack = true;
					throw rollback;
				}
				if (state.state === 'indeterminate') {
					var uncertain = confirmationIndeterminate(new Error(state.message ||
						_('The router could not prove whether Full Auto-Tune was confirmed.')), null);
					throw uncertain;
				}
				if (state.state === 'failed')
					throw new Error(state.message || _('The server-side Full Auto-Tune apply failed.'));
			}
			if (states.every(function(state) { return state.state === 'complete'; }))
				return states;
			if (Date.now() >= deadline) {
				var timeout = new Error(_('Timed out waiting for the router to finish the guarded Full Auto-Tune apply.'));
				timeout.supervisorTimedOut = true;
				throw timeout;
			}
			return applyGuardDelay(500).then(poll);
		}, function(error) {
			if (Date.now() >= deadline) {
				error.supervisorTimedOut = true;
				throw error;
			}
			return applyGuardDelay(500).then(poll);
		});
	}

	return poll();
}

function runGuardedSaveApply(view, ev) {
	var guards = [];
	var applyStarted = false;
	var applyDeadlineMs = 0;

	return view.handleSave(ev).then(function() {
		return armAutotuneApplyGuards();
	}).then(function(armed) {
		guards = armed;
		applyStarted = true;
		applyDeadlineMs = Date.now() + AUTOTUNE_APPLY_TIMEOUT_S * 1000;
		return uci.callApply(AUTOTUNE_APPLY_TIMEOUT_S, true);
	}).then(function(result) {
		if (result !== 0)
			throw new Error(_('UCI rejected the rollback-enabled configuration apply.'));
		return waitForGuardedApplySupervisor(guards, applyDeadlineMs);
	}).then(function() {
		applyStarted = false;
		window.location = window.location.href.split('#')[0];
		return view;
	}).catch(function(error) {
		if (error.confirmIndeterminate || error.transactionRolledBack || error.applyConfirmed)
			throw error;
		if (applyStarted)
			return rollbackGuardedApply(guards, error, applyDeadlineMs);
		return abortAutotuneApplyGuards(guards).then(function() { throw error; });
	});
}

function clearAutotuneProposalState(state) {
	state.autotune_running = false;
	state.autotune_progress = 0;
	state.autotune_result = null;
	state.autotune_proposal = null;
	state.autotune_action = 'apply_sqm';
	state.disable_sqm_confirmed = false;
	state.autotune_background_block = null;
}

function recordAutotuneTerminalFailure(state, result, message) {
	clearAutotuneProposalState(state);
	state.autotune_diagnostics = result && Object.keys(result).length ? result : {
		state: 'failed',
		error: message || _('Full Auto-Tune failed.'),
		configuration_written: false
	};
	state.autotune_failure_message = message || _('Full Auto-Tune failed.');

	return state;
}

function autotuneRetryableInconclusive(result) {
	return !!(result && result.state === 'inconclusive' && result.retryable === true);
}

function recordAutotuneRetryableInconclusive(state, result) {
	clearAutotuneProposalState(state);
	state.autotune_diagnostics = result;
	state.autotune_failure_message = '';
	state.autotune_background_block = result && result.background_blocked ? result : null;

	return state;
}

function autotuneAttemptDiagnostics(validation, result, index) {
	validation = validation || {};
	result = result || {};
	var decision = validation.decision || validation;
	var throughput = validation.throughput || {};
	var candidate = validation.candidate_base || validation.candidate || {};
	var proposal = result.proposal || {};
	var proposalDl = proposal.download || {};
	var proposalUl = proposal.upload || {};
	var metrics = decision.metrics || validation.metrics || {};
	var metricsDl = metrics.download || {};
	var metricsUl = metrics.upload || {};
	var signals = decision.signals || validation.signals || {};
	var directionPhases = validation.direction_phases || {};
	var downloadPhase = directionPhases.download || {};
	var uploadPhase = directionPhases.upload || {};
	var downloadIcmp = downloadPhase.icmp_latency || {};
	var uploadIcmp = uploadPhase.icmp_latency || {};
	var downloadTransport = downloadPhase.transport_latency || {};
	var uploadTransport = uploadPhase.transport_latency || {};
	var downloadSignals = signals.download || {};
	var uploadSignals = signals.upload || {};
	var achievedDl = firstAutotuneNumber([ throughput.download_kbps, validation.download_kbps ]);
	var achievedUl = firstAutotuneNumber([ throughput.upload_kbps, validation.upload_kbps ]);
	var candidateDl = firstAutotuneNumber([ candidate.download_kbps, candidate.dl_kbps,
		proposalDl.base_kbps ]);
	var candidateUl = firstAutotuneNumber([ candidate.upload_kbps, candidate.ul_kbps,
		proposalUl.base_kbps ]);
	var realization = validation.candidate_realization || throughput.candidate_realization || {};
	var retention = validation.capacity_retention || throughput.capacity_retention || {};
	var realizationDl = firstAutotuneNumber([
		realization.download_percent, realization.dl_percent,
		metricsDl.candidate_realization_percent,
		throughput.download_realization_percent,
		throughput.download_candidate_realization_percent,
		autotuneGateMetric(validation, [ 'download-candidate-realization' ]),
		autotunePercent(achievedDl, candidateDl)
	]);
	var realizationUl = firstAutotuneNumber([
		realization.upload_percent, realization.ul_percent,
		metricsUl.candidate_realization_percent,
		throughput.upload_realization_percent,
		throughput.upload_candidate_realization_percent,
		autotuneGateMetric(validation, [ 'upload-candidate-realization' ]),
		autotunePercent(achievedUl, candidateUl)
	]);
	var retentionDl = firstAutotuneNumber([
		retention.download_percent, retention.dl_percent,
		metricsDl.capacity_retention_percent,
		throughput.download_capacity_retention_percent,
		throughput.download_retention_percent,
		autotuneGateMetric(validation, [ 'download-capacity-retention' ])
	]);
	var retentionUl = firstAutotuneNumber([
		retention.upload_percent, retention.ul_percent,
		metricsUl.capacity_retention_percent,
		throughput.upload_capacity_retention_percent,
		throughput.upload_retention_percent,
		autotuneGateMetric(validation, [ 'upload-capacity-retention' ])
	]);
	var candidateCapacityDl = firstAutotuneNumber([
		metricsDl.candidate_capacity_percent,
		throughput.download_candidate_capacity_percent,
		autotunePercent(candidateDl, proposalDl.observed_low_kbps)
	]);
	var candidateCapacityUl = firstAutotuneNumber([
		metricsUl.candidate_capacity_percent,
		throughput.upload_candidate_capacity_percent,
		autotunePercent(candidateUl, proposalUl.observed_low_kbps)
	]);
	var icmp = validation.icmp_latency || validation.latency || {};
	var transport = validation.transport_latency || validation.http_latency || {};
	var background = validation.background || validation.background_traffic ||
		result.validation_background || null;
	if (!background && (downloadPhase.forwarded_background || uploadPhase.forwarded_background)) {
		var dlBackground = downloadPhase.forwarded_background || {};
		var ulBackground = uploadPhase.forwarded_background || {};
		background = {
			clean: dlBackground.contaminated === false && ulBackground.contaminated === false,
			contaminated: dlBackground.contaminated === true || ulBackground.contaminated === true,
			download_kbps: Math.max(autotuneNumber(dlBackground.download_kbps) || 0,
				autotuneNumber(ulBackground.download_kbps) || 0),
			upload_kbps: Math.max(autotuneNumber(dlBackground.upload_kbps) || 0,
				autotuneNumber(ulBackground.upload_kbps) || 0)
		};
	}
	var icmpDelta = firstAutotuneNumber([ icmp.delta_p95_ms, validation.icmp_delta_ms,
		autotuneGateMetric(decision, [ 'icmp-latency' ]) ]);
	var loss = firstAutotuneNumber([ icmp.loss_percent, validation.loss_percent,
		autotuneGateMetric(decision, [ 'packet-loss' ]) ]);
	var transportDelta = firstAutotuneNumber([ transport.delta_p95_ms,
		transport.delta_ms, validation.transport_delta_ms,
		autotuneGateMetric(decision, [ 'transport-latency' ]) ]);
	var cpu = firstAutotuneNumber([ validation.cpu_peak_percent,
		validation.cpu && validation.cpu.peak_percent,
		autotuneGateMetric(decision, [ 'cpu' ]) ]);
	var dlLoad = {
		icmp_delta_ms: firstAutotuneNumber([ downloadSignals.icmp_delta_ms,
			downloadIcmp.delta_p95_ms, icmpDelta ]),
		transport_delta_ms: firstAutotuneNumber([ downloadSignals.transport_delta_ms,
			downloadTransport.delta_p95_ms, transportDelta ]),
		loss_percent: firstAutotuneNumber([ downloadSignals.loss_percent,
			downloadIcmp.loss_percent, loss ]),
		cpu_percent: firstAutotuneNumber([ downloadSignals.cpu_percent,
			downloadPhase.cpu_peak_percent, cpu ])
	};
	var ulLoad = {
		icmp_delta_ms: firstAutotuneNumber([ uploadSignals.icmp_delta_ms,
			uploadIcmp.delta_p95_ms, icmpDelta ]),
		transport_delta_ms: firstAutotuneNumber([ uploadSignals.transport_delta_ms,
			uploadTransport.delta_p95_ms, transportDelta ]),
		loss_percent: firstAutotuneNumber([ uploadSignals.loss_percent,
			uploadIcmp.loss_percent, loss ]),
		cpu_percent: firstAutotuneNumber([ uploadSignals.cpu_percent,
			uploadPhase.cpu_peak_percent, cpu ])
	};
	var dlRealizationGate = autotuneGateValue(decision,
		[ 'download-candidate-realization' ]);
	var ulRealizationGate = autotuneGateValue(decision,
		[ 'upload-candidate-realization' ]);
	var dlRealizationMaximumGate = autotuneGateValue(decision,
		[ 'download-candidate-realization-maximum' ]);
	var ulRealizationMaximumGate = autotuneGateValue(decision,
		[ 'upload-candidate-realization-maximum' ]);
	var dlCapacityGate = autotuneGateValue(decision,
		[ 'download-capacity-retention', 'download_capacity_retention' ]);
	var ulCapacityGate = autotuneGateValue(decision,
		[ 'upload-capacity-retention', 'upload_capacity_retention' ]);
	var legacyCapacityGate = autotuneGateValue(decision,
		[ 'capacity_retention', 'throughput_retention', 'throughput' ]);
	var icmpGate = autotuneGateValue(decision,
		[ 'icmp_latency', 'latency', 'icmp' ]);
	var lossGate = autotuneGateValue(decision,
		[ 'icmp_loss', 'packet_loss', 'packet-loss', 'loss' ]);
	var transportGate = autotuneGateValue(decision,
		[ 'transport_latency', 'http_latency', 'transport' ]);
	var cpuGate = autotuneGateValue(decision, [ 'cpu', 'cpu_peak' ]);
	var dlIcmpGate = autotuneGateValue(decision, [ 'download-icmp-latency' ]);
	var dlLossGate = autotuneGateValue(decision, [ 'download-packet-loss' ]);
	var dlTransportGate = autotuneGateValue(decision, [ 'download-transport-latency' ]);
	var dlCpuGate = autotuneGateValue(decision, [ 'download-cpu' ]);
	var ulIcmpGate = autotuneGateValue(decision, [ 'upload-icmp-latency' ]);
	var ulLossGate = autotuneGateValue(decision, [ 'upload-packet-loss' ]);
	var ulTransportGate = autotuneGateValue(decision, [ 'upload-transport-latency' ]);
	var ulCpuGate = autotuneGateValue(decision, [ 'upload-cpu' ]);
	var backgroundGate = autotuneGateValue(validation,
		[ 'background', 'background_traffic', 'traffic_contamination' ]);

	if (dlRealizationGate == null && realizationDl != null)
		dlRealizationGate = realizationDl >= 80;
	if (ulRealizationGate == null && realizationUl != null)
		ulRealizationGate = realizationUl >= 80;
	if (dlCapacityGate == null)
		dlCapacityGate = legacyCapacityGate != null ? legacyCapacityGate :
			(retentionDl == null ? null : retentionDl >= 80);
	if (ulCapacityGate == null)
		ulCapacityGate = legacyCapacityGate != null ? legacyCapacityGate :
			(retentionUl == null ? null : retentionUl >= 80);
	if (icmpGate == null && icmpDelta != null)
		icmpGate = icmpDelta <= 100;
	if (lossGate == null && loss != null)
		lossGate = loss <= 5;
	if (transportGate == null && transportDelta != null)
		transportGate = transportDelta <= 100;
	if (cpuGate == null && cpu != null)
		cpuGate = cpu <= 95;
	if (dlIcmpGate == null)
		dlIcmpGate = icmpGate != null ? icmpGate : (dlLoad.icmp_delta_ms == null ? null : dlLoad.icmp_delta_ms <= 100);
	if (dlLossGate == null)
		dlLossGate = lossGate != null ? lossGate : (dlLoad.loss_percent == null ? null : dlLoad.loss_percent <= 5);
	if (dlTransportGate == null)
		dlTransportGate = transportGate != null ? transportGate :
			(dlLoad.transport_delta_ms == null ? null : dlLoad.transport_delta_ms <= 100);
	if (dlCpuGate == null)
		dlCpuGate = cpuGate != null ? cpuGate : (dlLoad.cpu_percent == null ? null : dlLoad.cpu_percent <= 95);
	if (ulIcmpGate == null)
		ulIcmpGate = icmpGate != null ? icmpGate : (ulLoad.icmp_delta_ms == null ? null : ulLoad.icmp_delta_ms <= 100);
	if (ulLossGate == null)
		ulLossGate = lossGate != null ? lossGate : (ulLoad.loss_percent == null ? null : ulLoad.loss_percent <= 5);
	if (ulTransportGate == null)
		ulTransportGate = transportGate != null ? transportGate :
			(ulLoad.transport_delta_ms == null ? null : ulLoad.transport_delta_ms <= 100);
	if (ulCpuGate == null)
		ulCpuGate = cpuGate != null ? cpuGate : (ulLoad.cpu_percent == null ? null : ulLoad.cpu_percent <= 95);
	if (backgroundGate == null && background) {
		if (typeof background.clean === 'boolean')
			backgroundGate = background.clean;
		else if (typeof background.contaminated === 'boolean')
			backgroundGate = !background.contaminated;
		else if (typeof background.detected === 'boolean')
			backgroundGate = !background.detected;
	}

	return {
		index: index,
		pass: validation.pass === true,
		score: autotuneNumber(validation.score),
		candidate: { download_kbps: candidateDl, upload_kbps: candidateUl },
		achieved: { download_kbps: achievedDl, upload_kbps: achievedUl },
		candidate_realization: { download_percent: realizationDl, upload_percent: realizationUl },
		capacity_retention: { download_percent: retentionDl, upload_percent: retentionUl },
		candidate_capacity: { download_percent: candidateCapacityDl, upload_percent: candidateCapacityUl },
		icmp: {
			median_ms: autotuneNumber(icmp.median_ms),
			p95_ms: autotuneNumber(icmp.p95_ms),
			max_ms: autotuneNumber(icmp.max_ms),
			delta_p95_ms: icmpDelta,
			loss_percent: loss,
			samples: autotuneNumber(icmp.samples)
		},
		transport: {
			backend: transport.backend || transport.method || '',
			url: transport.url || transport.endpoint || '',
			median_ms: autotuneNumber(transport.median_ms),
			p95_ms: autotuneNumber(transport.p95_ms),
			max_ms: autotuneNumber(transport.max_ms),
			delta_p95_ms: transportDelta,
			samples: autotuneNumber(transport.samples)
		},
		cpu_peak_percent: cpu,
		direction_load: { download: dlLoad, upload: ulLoad },
		directional_load_reported: !!(signals.download || signals.upload ||
			directionPhases.download || directionPhases.upload),
		correction: decision.correction || validation.correction || null,
		reasons: decision.reasons || validation.reasons || [],
		background: background,
		gates: [
			{ id: 'download_candidate_realization', pass: dlRealizationGate },
			{ id: 'upload_candidate_realization', pass: ulRealizationGate },
			{ id: 'download_candidate_realization_maximum', pass: dlRealizationMaximumGate },
			{ id: 'upload_candidate_realization_maximum', pass: ulRealizationMaximumGate },
			{ id: 'download_capacity_retention', pass: dlCapacityGate },
			{ id: 'upload_capacity_retention', pass: ulCapacityGate },
			{ id: 'icmp_latency', pass: icmpGate },
			{ id: 'icmp_loss', pass: lossGate },
			{ id: 'transport_latency', pass: transportGate },
			{ id: 'cpu', pass: cpuGate },
			{ id: 'download_icmp', pass: dlIcmpGate },
			{ id: 'download_loss', pass: dlLossGate },
			{ id: 'download_transport', pass: dlTransportGate },
			{ id: 'download_cpu', pass: dlCpuGate },
			{ id: 'upload_icmp', pass: ulIcmpGate },
			{ id: 'upload_loss', pass: ulLossGate },
			{ id: 'upload_transport', pass: ulTransportGate },
			{ id: 'upload_cpu', pass: ulCpuGate },
			{ id: 'background', pass: backgroundGate }
		]
	};
}

function autotuneDiagnostics(result) {
	var legacy = autotuneLegacyResult(result);
	var diagnosticResult = legacy || result;
	var attempts = autotuneValidationAttempts(diagnosticResult);

	return {
		validated: autotuneResultValidated(result),
		legacy: !!legacy,
		legacy_schema_version: result && result.legacy_schema_version != null ?
			result.legacy_schema_version :
			(legacy && legacy.schema_version != null ? legacy.schema_version : null),
		state: result && result.state || 'unknown',
		stage: diagnosticResult && diagnosticResult.stage || '',
		reason: diagnosticResult && diagnosticResult.reason || '',
		error: result && result.error ||
			(diagnosticResult && diagnosticResult.error) || '',
		configuration_written: !!(diagnosticResult && diagnosticResult.configuration_written),
		attempts: attempts.map(function(attempt, index) {
			return autotuneAttemptDiagnostics(attempt, diagnosticResult, index + 1);
		})
	};
}

function adaptiveCeilingWritePlan(state, proposal) {
	state = state || {};
	proposal = proposal || {};
	var adaptive = proposal.adaptive_ceiling || {};
	var dl = proposal.download || {};
	var ul = proposal.upload || {};
	var original = state.original_adaptive_ceiling || {};
	var preserve = original.enabled === true && adaptive.enabled === false &&
		state.adaptive_ceiling_disable_confirmed !== true;
	var maxRate = function(current, minimum, fallback) {
		var currentNumber = autotuneNumber(current);
		var minimumNumber = autotuneNumber(minimum);
		var fallbackNumber = autotuneNumber(fallback);
		var value = currentNumber != null ? currentNumber : fallbackNumber;

		if (minimumNumber != null && (value == null || value < minimumNumber))
			value = minimumNumber;
		return value;
	};

	if (preserve) {
		return {
			enabled: true,
			preserved: true,
			dl_cap_kbps: maxRate(original.dl_cap_kbps, dl.maximum_kbps, dl.absolute_cap_kbps),
			ul_cap_kbps: maxRate(original.ul_cap_kbps, ul.maximum_kbps, ul.absolute_cap_kbps),
			hold_s: firstAutotuneNumber([ original.hold_s, adaptive.hold_s ]),
			growth_percent: firstAutotuneNumber([ original.growth_percent, adaptive.growth_percent ]),
			probe_s: firstAutotuneNumber([ original.probe_s, adaptive.probe_s ]),
			cooldown_s: firstAutotuneNumber([ original.cooldown_s, adaptive.cooldown_s ]),
			failed_bound_ttl_s: firstAutotuneNumber([
				original.failed_bound_ttl_s, adaptive.failed_bound_ttl_s
			])
		};
	}

	return {
		enabled: adaptive.enabled === true,
		preserved: false,
		dl_cap_kbps: firstAutotuneNumber([ dl.absolute_cap_kbps, dl.maximum_kbps ]),
		ul_cap_kbps: firstAutotuneNumber([ ul.absolute_cap_kbps, ul.maximum_kbps ]),
		hold_s: autotuneNumber(adaptive.hold_s),
		growth_percent: autotuneNumber(adaptive.growth_percent),
		probe_s: autotuneNumber(adaptive.probe_s),
		cooldown_s: autotuneNumber(adaptive.cooldown_s),
		failed_bound_ttl_s: autotuneNumber(adaptive.failed_bound_ttl_s)
	};
}

function autotuneMetric(value, suffix) {
	value = autotuneNumber(value);
	if (value == null)
		return '-';

	return String(Math.round(value * 10) / 10) + (suffix || '');
}

function autotuneGate(attempt, id) {
	for (var i = 0; i < attempt.gates.length; i++)
		if (attempt.gates[i].id === id)
			return attempt.gates[i].pass;

	return null;
}

function renderAutotuneGate(pass) {
	var text = pass === true ? _('PASS') : (pass === false ? _('FAIL') : _('NOT REPORTED'));
	var color = pass === true ? '#0a8f5a' : (pass === false ? '#d94141' : '#777');

	return E('strong', { 'style': 'color:%s;white-space:nowrap'.format(color) }, text);
}

function renderAutotuneDiagnostics(result) {
	var diagnostics = autotuneDiagnostics(result);
	var retryableInconclusive = autotuneRetryableInconclusive(result);
	var nodes = [ E('div', {
		'class': 'alert-message %s'.format(diagnostics.validated ? 'success' :
			(retryableInconclusive ? 'warning' : 'error')),
		'style': 'margin:10px 0'
	}, [
		E('strong', {}, diagnostics.legacy ? _('Legacy diagnostics. ') :
			(diagnostics.validated ? _('Validated proposal. ') :
			(retryableInconclusive ? _('Calibration was inconclusive. ') : _('Proposal rejected. '))),
		diagnostics.error || (diagnostics.validated ?
			_('Every required validation gate passed.') :
			(retryableInconclusive ?
				_('No proposal was accepted. Retry the calibration; Review and Apply remain unavailable.') :
				_('No settings can be applied from this result. Review the failed gates below.'))))
	]) ];

	if (diagnostics.legacy)
		nodes.push(E('p', {}, _('Result schema: %s. This saved result is read-only and cannot be reused by the current validator.').format(
			diagnostics.legacy_schema_version == null ? _('unknown') :
				diagnostics.legacy_schema_version)));

	if (diagnostics.stage || diagnostics.reason)
		nodes.push(E('p', {}, _('Stage: %s. Diagnostic reason: %s.').format(
			diagnostics.stage || '-', diagnostics.reason || '-')));

	if (!diagnostics.attempts.length) {
		nodes.push(E('p', {}, _('The job returned no structured validation attempts.')));
		return E('div', { 'class': 'cake-autotune-diagnostics' }, nodes);
	}

	for (var i = 0; i < diagnostics.attempts.length; i++) {
		var attempt = diagnostics.attempts[i];
		var dlSummary = _('%s candidate → %s achieved').format(
			autotuneMetric(attempt.candidate.download_kbps, ' kbit/s'),
			autotuneMetric(attempt.achieved.download_kbps, ' kbit/s'));
		var ulSummary = _('%s candidate → %s achieved').format(
			autotuneMetric(attempt.candidate.upload_kbps, ' kbit/s'),
			autotuneMetric(attempt.achieved.upload_kbps, ' kbit/s'));
		var icmpSummary = _('%s median / %s p95 / +%s delta; %s loss; %s samples').format(
			autotuneMetric(attempt.icmp.median_ms, ' ms'),
			autotuneMetric(attempt.icmp.p95_ms, ' ms'),
			autotuneMetric(attempt.icmp.delta_p95_ms, ' ms'),
			autotuneMetric(attempt.icmp.loss_percent, '%'),
			autotuneMetric(attempt.icmp.samples, ''));
		var transportName = attempt.transport.backend || attempt.transport.url || _('transport probe');
		var transportSummary = _('%s: %s median / %s p95 / +%s delta; %s samples').format(
			transportName,
			autotuneMetric(attempt.transport.median_ms, ' ms'),
			autotuneMetric(attempt.transport.p95_ms, ' ms'),
			autotuneMetric(attempt.transport.delta_p95_ms, ' ms'),
			autotuneMetric(attempt.transport.samples, ''));
		var background = attempt.background;
		var backgroundSummary = background ? _('%s; DL %s, UL %s kbit/s').format(
			background.clean === true ? _('clean') :
				(background.contaminated === true || background.detected === true ? _('detected') : _('reported')),
			background.download_kbps == null ? '-' : background.download_kbps,
			background.upload_kbps == null ? '-' : background.upload_kbps) :
			_('Not reported for this validation attempt');
		var dlLoad = attempt.direction_load.download;
		var ulLoad = attempt.direction_load.upload;
		var correction = attempt.correction || {};
		var correctionSummary = correction.action ? _('%s — %s; DL %s → %s kbit/s, UL %s → %s kbit/s').format(
			correction.action,
			correction.reason || '-',
			correction.download && correction.download.action || '-',
			correction.download && correction.download.proposed_kbps || '-',
			correction.upload && correction.upload.action || '-',
			correction.upload && correction.upload.proposed_kbps || '-') : _('Not reported');
		var reasonSummary = attempt.reasons && attempt.reasons.length ? attempt.reasons.map(function(reason) {
			return reason.code || reason.reason || String(reason);
		}).join(', ') : _('none');
		var rows = [
			[ _('Download throughput'), dlSummary, '-' ],
			[ _('DL candidate realization'), autotuneMetric(attempt.candidate_realization.download_percent, '%'),
				renderAutotuneGate(autotuneGate(attempt, 'download_candidate_realization')) ],
			[ _('DL candidate realization maximum'), autotuneMetric(attempt.candidate_realization.download_percent, '%'),
				renderAutotuneGate(autotuneGate(attempt, 'download_candidate_realization_maximum')) ],
			[ _('DL candidate / raw capacity'), autotuneMetric(attempt.candidate_capacity.download_percent, '%'), '-' ],
			[ _('DL capacity retained'), autotuneMetric(attempt.capacity_retention.download_percent, '%'),
				renderAutotuneGate(autotuneGate(attempt, 'download_capacity_retention')) ],
			[ _('Upload throughput'), ulSummary, '-' ],
			[ _('UL candidate realization'), autotuneMetric(attempt.candidate_realization.upload_percent, '%'),
				renderAutotuneGate(autotuneGate(attempt, 'upload_candidate_realization')) ],
			[ _('UL candidate realization maximum'), autotuneMetric(attempt.candidate_realization.upload_percent, '%'),
				renderAutotuneGate(autotuneGate(attempt, 'upload_candidate_realization_maximum')) ],
			[ _('UL candidate / raw capacity'), autotuneMetric(attempt.candidate_capacity.upload_percent, '%'), '-' ],
			[ _('UL capacity retained'), autotuneMetric(attempt.capacity_retention.upload_percent, '%'),
				renderAutotuneGate(autotuneGate(attempt, 'upload_capacity_retention')) ]
		];
		if (attempt.directional_load_reported) {
			rows.push(
				[ _('DL ICMP loaded delta'), autotuneMetric(dlLoad.icmp_delta_ms, ' ms'),
					renderAutotuneGate(autotuneGate(attempt, 'download_icmp')) ],
				[ _('DL ICMP packet loss'), autotuneMetric(dlLoad.loss_percent, '%'),
					renderAutotuneGate(autotuneGate(attempt, 'download_loss')) ],
				[ _('DL transport loaded delta'), autotuneMetric(dlLoad.transport_delta_ms, ' ms'),
					renderAutotuneGate(autotuneGate(attempt, 'download_transport')) ],
				[ _('DL CPU peak'), autotuneMetric(dlLoad.cpu_percent, '%'),
					renderAutotuneGate(autotuneGate(attempt, 'download_cpu')) ],
				[ _('UL ICMP loaded delta'), autotuneMetric(ulLoad.icmp_delta_ms, ' ms'),
					renderAutotuneGate(autotuneGate(attempt, 'upload_icmp')) ],
				[ _('UL ICMP packet loss'), autotuneMetric(ulLoad.loss_percent, '%'),
					renderAutotuneGate(autotuneGate(attempt, 'upload_loss')) ],
				[ _('UL transport loaded delta'), autotuneMetric(ulLoad.transport_delta_ms, ' ms'),
					renderAutotuneGate(autotuneGate(attempt, 'upload_transport')) ],
				[ _('UL CPU peak'), autotuneMetric(ulLoad.cpu_percent, '%'),
					renderAutotuneGate(autotuneGate(attempt, 'upload_cpu')) ]
			);
		}
		else {
			rows.push(
				[ _('ICMP latency'), icmpSummary, renderAutotuneGate(autotuneGate(attempt, 'icmp_latency')) ],
				[ _('ICMP packet loss'), autotuneMetric(attempt.icmp.loss_percent, '%'),
					renderAutotuneGate(autotuneGate(attempt, 'icmp_loss')) ],
				[ _('Transport latency'), transportSummary,
					renderAutotuneGate(autotuneGate(attempt, 'transport_latency')) ],
				[ _('CPU peak'), autotuneMetric(attempt.cpu_peak_percent, '%'),
					renderAutotuneGate(autotuneGate(attempt, 'cpu')) ]
			);
		}
		rows.push(
			[ _('Typed correction'), correctionSummary, '-' ],
			[ _('Failed gate reasons'), reasonSummary, '-' ],
			[ _('Background traffic'), backgroundSummary,
				renderAutotuneGate(autotuneGate(attempt, 'background')) ]
		);
		var table = E('table', { 'class': 'table', 'style': 'margin-top:8px' }, [
			E('tr', { 'class': 'tr table-titles' }, [
				E('th', { 'class': 'th' }, _('Gate / metric')),
				E('th', { 'class': 'th' }, _('Measured result')),
				E('th', { 'class': 'th' }, _('Gate'))
			])
		].concat(rows.map(function(row) {
			return E('tr', { 'class': 'tr' }, [
				E('td', { 'class': 'td' }, row[0]),
				E('td', { 'class': 'td', 'style': 'white-space:normal' }, row[1]),
				E('td', { 'class': 'td' }, row[2])
			]);
		})));

		nodes.push(E('details', {
			'open': i === diagnostics.attempts.length - 1 ? '' : null,
			'style': 'margin:8px 0;padding:8px;border:1px solid rgba(127,127,127,.35);border-radius:4px'
		}, [
			E('summary', { 'style': 'cursor:pointer;font-weight:600' },
				_('Validation attempt %d — %s%s').format(attempt.index,
					attempt.pass ? _('passed') : _('failed'),
					attempt.score == null ? '' : _(' · score %s/100').format(attempt.score))),
			table
		]));
	}

	if (!diagnostics.configuration_written)
		nodes.push(E('p', { 'style': 'font-weight:600' },
			_('No UCI configuration was written by this Auto-Tune job.')));

	return E('div', { 'class': 'cake-autotune-diagnostics' }, nodes);
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

function showCreateWizard(grid, name, existingName) {
	var rerun = !!existingName;
	if (rerun)
		name = existingName;
	var defaultWan = defaultTargetInterface();
	var defaultMembers = mwan3MembersForDevice(defaultWan);
	var defaultMember = defaultMembers.length ? defaultMembers[0].name : '';
	var state = {
		name: name,
		step: rerun ? 1 : 0,
		mode: 'autotune',
		wan_if: defaultWan,
		route_mode: defaultMember ? 'mwan3' : 'main',
		mwan3_member: defaultMember,
		route_selection: defaultMember ? 'mwan3:' + defaultMember : 'main',
		multiwan_set: false,
		enabled: true,
		sqm_enabled: true,
		autotune_profile: 'best_overall',
		autotune_action: 'apply_sqm',
		disable_sqm_confirmed: false,
		speedtest_backend: 'auto',
		speedtest_go_server_id: '',
		speedtest_apply_percent: '90',
		advanced_test_options: false,
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

	if (rerun) {
		state.wan_if = selectedWan(null, existingName);
		state.route_mode = uci.get('cake-autorate', existingName, 'route_mode') || 'auto';
		state.mwan3_member = uci.get('cake-autorate', existingName, 'mwan3_member') || '';
		state.route_selection = state.mwan3_member ? 'mwan3:' + state.mwan3_member : 'main';
		state.enabled = uci.get('cake-autorate', existingName, 'enabled') === '1';
		state.sqm_enabled = uci.get('cake-autorate', existingName, 'sqm_enabled') === '1';
		state.autotune_profile = canonicalAutotuneProfile(
			uci.get('cake-autorate', existingName, 'autotune_profile')) || 'best_overall';
		state.speedtest_backend = uci.get('cake-autorate', existingName, 'speedtest_backend') || 'auto';
		state.speedtest_go_server_id = uci.get('cake-autorate', existingName, 'speedtest_go_server_id') || '';
		state.speedtest_apply_percent = uci.get('cake-autorate', existingName, 'speedtest_apply_percent') || '90';
		state.pinger_method = uci.get('cake-autorate', existingName, 'pinger_method') || 'fping';
		state.no_pingers = uci.get('cake-autorate', existingName, 'no_pingers') || '6';
		state.reflectors = listFormOrUci(null, existingName, 'reflector');
		state.sqm_section = uci.get('cake-autorate', existingName, 'sqm_section') || managedSqmSectionName(existingName);
		state.sqm_download = uci.get('cake-autorate', existingName, 'sqm_download') ||
			uci.get('cake-autorate', existingName, 'base_dl_shaper_rate_kbps') || '20000';
		state.sqm_upload = uci.get('cake-autorate', existingName, 'sqm_upload') ||
			uci.get('cake-autorate', existingName, 'base_ul_shaper_rate_kbps') || '20000';
		state.current_limits = {
			download: {
				minimum_kbps: uci.get('cake-autorate', existingName, 'min_dl_shaper_rate_kbps'),
				base_kbps: uci.get('cake-autorate', existingName, 'base_dl_shaper_rate_kbps'),
				maximum_kbps: uci.get('cake-autorate', existingName, 'max_dl_shaper_rate_kbps'),
				absolute_cap_kbps: uci.get('cake-autorate', existingName, 'adaptive_ceiling_dl_cap_kbps')
			},
			upload: {
				minimum_kbps: uci.get('cake-autorate', existingName, 'min_ul_shaper_rate_kbps'),
				base_kbps: uci.get('cake-autorate', existingName, 'base_ul_shaper_rate_kbps'),
				maximum_kbps: uci.get('cake-autorate', existingName, 'max_ul_shaper_rate_kbps'),
				absolute_cap_kbps: uci.get('cake-autorate', existingName, 'adaptive_ceiling_ul_cap_kbps')
			}
		};
		state.original_adaptive_ceiling = {
			enabled: uci.get('cake-autorate', existingName, 'adaptive_ceiling_enabled') === '1',
			dl_cap_kbps: uci.get('cake-autorate', existingName, 'adaptive_ceiling_dl_cap_kbps'),
			ul_cap_kbps: uci.get('cake-autorate', existingName, 'adaptive_ceiling_ul_cap_kbps'),
			hold_s: uci.get('cake-autorate', existingName, 'adaptive_ceiling_hold_time_s'),
			growth_percent: uci.get('cake-autorate', existingName, 'adaptive_ceiling_growth_percent'),
			probe_s: uci.get('cake-autorate', existingName, 'adaptive_ceiling_probe_duration_s'),
			cooldown_s: uci.get('cake-autorate', existingName, 'adaptive_ceiling_cooldown_s'),
			failed_bound_ttl_s: uci.get('cake-autorate', existingName, 'adaptive_ceiling_failed_bound_ttl_s')
		};
		state.adaptive_ceiling_disable_confirmed = false;
		for (var importIndex = 0; importIndex < sqmImportOptionMap.length; importIndex++) {
			var importKey = sqmImportOptionMap[importIndex][0];
			state[importKey] = uci.get('cake-autorate', existingName, importKey) || sqmImportOptionMap[importIndex][2];
		}
		} else {
			importSqmQueueIntoState(state, state.mode !== 'autotune');
		}

		function syncSqmForInterface() {
			importSqmQueueIntoState(state, state.mode !== 'autotune');
	}

	function stepTitle() {
		return [
			_('Interface'),
			state.mode === 'autotune' ? _('Full Auto-Tune') : _('Speed test'),
			state.autotune_diagnostics && !state.autotune_result ? _('Review diagnostics') : _('Review')
		][state.step];
	}

	function renderSteps() {
		var labels = [
			_('Interface'),
			state.mode === 'autotune' ? _('Full Auto-Tune') : _('Speed test'),
			state.autotune_diagnostics && !state.autotune_result ? _('Review diagnostics') : _('Review')
		];
		var steps = [];

		for (var i = 0; i < labels.length; i++) {
			var active = i === state.step;
			var completed = i < state.step;

			steps.push(E('button', {
				'type': 'button',
				'class': 'btn cbi-button cake-autorate-wizard-step %s'.format(
					active ? 'cbi-button-positive' : (completed ? 'cbi-button-action' : '')),
				'data-step': String(i),
				'aria-current': active ? 'step' : null,
				'title': _('Go to step %d: %s').format(i + 1, labels[i]),
				'style': 'display:inline-flex;align-items:center;justify-content:flex-start;gap:8px;flex:1 1 150px;min-height:42px;text-align:left',
				'click': function(ev) {
					ev.preventDefault();
					navigateWizardStep(parseInt(ev.currentTarget.getAttribute('data-step'), 10));
				}
			}, [
				E('span', {
					'style': 'display:inline-flex;align-items:center;justify-content:center;width:24px;height:24px;border:2px solid currentColor;border-radius:50%;font-weight:700;flex:0 0 24px'
				}, String(i + 1)),
				E('span', { 'style': 'font-weight:600' }, labels[i])
			]));
		}

		return E('div', {
			'class': 'cake-autorate-wizard-steps',
			'style': 'display:flex;flex-wrap:wrap;gap:8px;margin-bottom:16px'
		}, steps);
	}

	function renderInterfaceStep() {
		var detectedUplinks = uniqueMwan3Uplinks();
		var target = wizardSelectOptions(targetInterfaceChoiceOptions(), state.wan_if);
		var route = wizardSelectOptions(wizardRouteChoices(), state.route_selection);
		var enabled = wizardCheckbox(state.enabled);
		var multiwan = wizardCheckbox(state.multiwan_set);
		var queueInfo = E('div', { 'class': 'cbi-value-dummy' }, wizardSqmQueueText(state));
		var modeButtons = [
			[ 'autotune', _('Full Auto-Tune'), _('Measures the link, calculates limits and presents a complete proposal. Uses significant traffic.') ],
			[ 'manual', _('Manual wizard'), _('Keep full control over speed testing and all derived values.') ]
		].map(function(mode) {
			return E('button', {
				'type': 'button',
				'class': 'btn cbi-button %s'.format(state.mode === mode[0] ? 'cbi-button-positive' : ''),
				'data-mode': mode[0],
				'style': 'display:flex;flex-direction:column;align-items:flex-start;gap:3px;flex:1 1 260px;min-height:68px;padding:10px;text-align:left',
				'click': function(ev) {
					state.mode = ev.currentTarget.getAttribute('data-mode');
					state.autotune_result = null;
					state.autotune_proposal = null;
					state.autotune_diagnostics = null;
					state.autotune_failure_message = '';
					if (state.mode === 'autotune') {
						state.enabled = true;
						state.sqm_enabled = true;
					}
					syncSqmForInterface();
					render();
				}
			}, [
				E('strong', {}, mode[1]),
				E('span', { 'style': 'font-size:12px;white-space:normal' }, mode[2])
			]);
		});

		target.addEventListener('change', function() {
			var newWan = normalizeInterfaceName(target.value);
			if (newWan !== state.wan_if) {
				state.autotune_result = null;
				state.autotune_proposal = null;
				state.autotune_diagnostics = null;
				state.autotune_failure_message = '';
			}
			state.wan_if = newWan;
			var matchingMembers = mwan3MembersForDevice(newWan);
			if (state.route_mode === 'mwan3') {
				state.mwan3_member = matchingMembers.length ? matchingMembers[0].name : '';
				state.route_selection = state.mwan3_member ? 'mwan3:' + state.mwan3_member : 'main';
				state.route_mode = state.mwan3_member ? 'mwan3' : 'main';
				render();
				return;
			}
			state.ping_extra_args = pingerInterfaceArgs(state.wan_if, state.pinger_method || 'fping');
			syncSqmForInterface();
			queueInfo.textContent = wizardSqmQueueText(state);
		});

		route.addEventListener('change', function() {
			state.autotune_result = null;
			state.autotune_proposal = null;
			state.autotune_diagnostics = null;
			state.autotune_failure_message = '';
			state.route_selection = route.value;
			if (route.value.indexOf('mwan3:') === 0) {
				state.route_mode = 'mwan3';
				state.mwan3_member = route.value.substring(6);
				var member = mwan3Context.byName[state.mwan3_member];
				if (member && member.device !== state.wan_if) {
					state.wan_if = member.device;
					state.ping_extra_args = pingerInterfaceArgs(state.wan_if, state.pinger_method || 'fping');
					syncSqmForInterface();
				}
			} else {
				state.route_mode = 'main';
				state.mwan3_member = '';
			}
			render();
		});

		enabled.addEventListener('change', function() {
			state.enabled = enabled.checked;
			state.sqm_enabled = state.enabled;
		});

		multiwan.addEventListener('change', function() {
			state.multiwan_set = multiwan.checked;
			if (state.multiwan_set) {
				state.mode = 'manual';
				state.enabled = true;
				state.sqm_enabled = true;
			}
			render();
		});

		var fields = [
			wizardField(_('Setup mode'), E('div', { 'style': 'display:flex;flex-wrap:wrap;gap:8px' }, modeButtons)),
			wizardField(_('Target interface'), target, optionDescriptions.wan_if),
			wizardField(_('Probe routing'), route, optionDescriptions.route_mode),
			wizardField(_('SQM queue'), queueInfo, optionDescriptions._wizard_sqm_queue),
			wizardField(_('Enable autorate'), enabled, optionDescriptions.enabled)
		];
		if (detectedUplinks.length > 1) {
			var plan = multiwanInstancePlans(state).map(function(item) {
				return '%s: %s → %s → %s'.format(item.name, item.member, item.device, item.sqmSection);
			}).join('\n');
			fields.push(wizardField(
				_('Multi-WAN set'),
				E('div', {}, [ multiwan, ' ', _('Create one isolated instance per detected uplink.'),
					E('pre', { 'style': 'white-space:pre-wrap;margin-top:6px' }, plan) ]),
				_('mwan3 IPv4/IPv6 entries resolving to the same L3 device are grouped into one uplink so duplicate CAKE queues cannot be created. Each instance keeps independent probes, baselines, limits, and schedules.')));
		}

		return fields;
	}

	function applyAutotuneResult(result) {
		if (!autotuneResultHasReviewChoice(result))
			throw new Error(_('Refusing to stage an Auto-Tune result without a safe review choice.'));

		var proposal = result.proposal;
		var firstRun = result.runs && result.runs.length ? result.runs[0] : {};
		state.autotune_result = result;
		state.autotune_proposal = proposal;
		/* Even an evidence-backed no-SQM recommendation is a destructive
		 * alternative, not a default. Keep the safe shaped candidate selected
		 * until the user deliberately chooses and confirms disable. */
		state.autotune_action = 'apply_sqm';
		state.disable_sqm_confirmed = false;
		state.autotune_profile = canonicalAutotuneProfile(result.profile) || 'best_overall';
		state.autotune_diagnostics = null;
		state.autotune_failure_message = '';
		state.adaptive_ceiling_disable_confirmed = false;
		if (!rerun) {
			state.enabled = true;
			state.sqm_enabled = true;
		}
		state.sqm_download = String(proposal.download.base_kbps);
		state.sqm_upload = String(proposal.upload.base_kbps);
		state.sqm_linklayer = proposal.link.layer;
		state.sqm_overhead = String(proposal.link.overhead);
		state.sqm_tcMPU = String(proposal.link.mpu);
		state.sqm_linklayer_advanced = proposal.link.layer === 'none' ? '0' : '1';
		state.sqm_qdisc = proposal.sqm.qdisc;
		state.sqm_script = proposal.sqm.script;
		state.sqm_qdisc_advanced = '1';
		state.sqm_qdisc_really_really_advanced = '1';
		state.sqm_squash_dscp = proposal.sqm.squash_dscp ? '1' : '0';
		state.sqm_squash_ingress = proposal.sqm.squash_ingress ? '1' : '0';
		state.sqm_ingress_ecn = proposal.sqm.ingress_ecn;
		state.sqm_egress_ecn = proposal.sqm.egress_ecn;
		state.sqm_iqdisc_opts = proposal.sqm.iqdisc_opts || '';
		state.sqm_eqdisc_opts = proposal.sqm.eqdisc_opts || '';
		state.speedtest_backend = firstRun.backend || state.speedtest_backend || 'auto';
		state.speedtest_go_server_id = firstRun.server_id || '';
		applyPingerPlanToState(state, result.pinger_plan);
	}

	function renderAutotuneStep() {
		var profileButtons = autotuneProfileDefinitions().map(function(definition) {
			var selected = state.autotune_profile === definition.id;
			return E('button', {
				'type': 'button',
				'class': 'btn cbi-button %s'.format(selected ? 'cbi-button-positive' : ''),
				'disabled': state.autotune_running ? 'disabled' : null,
				'data-profile': definition.id,
				'aria-pressed': selected ? 'true' : 'false',
				'style': 'display:flex;flex-direction:column;align-items:flex-start;gap:4px;flex:1 1 220px;min-height:104px;padding:10px;text-align:left;white-space:normal;word-break:normal;overflow-wrap:break-word;hyphens:none;line-height:1.4',
				'click': function(ev) {
					var selectedProfile = canonicalAutotuneProfile(
						ev.currentTarget.getAttribute('data-profile'));
					if (!selectedProfile || selectedProfile === state.autotune_profile)
						return;
					clearAutotuneProposalState(state);
					state.autotune_diagnostics = null;
					state.autotune_failure_message = '';
					state.autotune_background_block = null;
					state.autotune_profile = selectedProfile;
					render();
				}
			}, [
				E('strong', {}, definition.title),
				E('span', { 'style': 'font-size:12px;font-weight:600' }, definition.target),
				E('span', { 'style': 'font-size:12px' }, definition.description)
			]);
		});
		var status = E('div', { 'style': 'margin-top:8px;white-space:normal' },
			state.autotune_result ? _('Calibration complete. Review the validated proposed parameters.') :
				(state.autotune_diagnostics ?
					(autotuneLegacyResult(state.autotune_diagnostics) ?
						_('Saved diagnostics use an older result schema. Review them if useful, then run calibration again.') :
					(autotuneRetryableInconclusive(state.autotune_diagnostics) ?
						_('Calibration was inconclusive. Retry when ready; this result cannot be reviewed or applied.') :
						_('Calibration did not validate. Review the diagnostics; this result cannot be applied.'))) : ''));
		var progress = E('progress', {
			'max': '100',
			'value': state.autotune_progress || '0',
			'style': 'width:min(620px,100%);display:' +
				(!state.autotune_running && state.autotune_diagnostics ? 'none' : 'block') +
				';margin-top:8px'
		});
		var startCalibration = function(conservative) {
			showError(null);
			state.autotune_result = null;
			state.autotune_proposal = null;
			state.autotune_diagnostics = null;
			state.autotune_failure_message = '';
			state.autotune_recovery_pending = null;
			state.autotune_running = true;
			state.autotune_progress = 0;
			state.autotune_background_block = null;
			runButton.disabled = true;
			conservativeButton.style.display = 'none';
			cancelButton.disabled = false;
			status.textContent = conservative ?
				_('Starting conservative Full Auto-Tune...') : _('Starting Full Auto-Tune...');

			return runAutotuneJob(state.name, state.wan_if, state.speedtest_backend, function(job) {
				state.autotune_progress = job.progress || 0;
				progress.value = state.autotune_progress;
				status.textContent = job.message || job.phase || _('Full Auto-Tune is running...');
			}, state.route_mode, state.mwan3_member, state.autotune_profile,
			conservative).then(function(result) {
				applyAutotuneResult(result);
				state.autotune_running = false;
				state.step = 2;
				render();
			}).catch(function(err) {
				clearAutotuneProposalState(state);
				state.autotune_diagnostics = null;
				state.autotune_failure_message = '';
				state.autotune_recovery_pending = null;
				progress.value = 0;
				progress.style.display = 'none';
				runButton.disabled = false;
				cancelButton.disabled = true;

				/* Exhausting the bounded recovery poll is not a terminal job
				 * result.  Preserve no proposal/diagnostics and make that explicit
				 * so Review and staging remain unavailable. */
				if (err.autotuneRecoveryPending) {
					state.autotune_recovery_pending = err.autotuneRecoveryStatus || {};
					status.textContent = _('Runtime recovery is still pending. No result or proposal was accepted.');
					showError(err.message || _('Runtime recovery is still pending.'));
					return;
				}

				var result = err.autotuneResult || {};
				if (autotuneRetryableInconclusive(result)) {
					recordAutotuneRetryableInconclusive(state, result);
					render();
				} else if (result.background_blocked && result.retryable) {
					state.autotune_background_block = result;
					var background = result.background || {};
					status.textContent = _('Background traffic blocked strict calibration at %s: DL %s kbit/s, UL %s kbit/s. Usable directions: DL %s, UL %s.').format(
						result.stage || 'quiet-check',
						background.download_kbps || 0,
						background.upload_kbps || 0,
						background.download_usable ? _('yes') : _('no'),
						background.upload_usable ? _('yes') : _('no'));
					conservativeButton.style.display = '';
					showError(_('The strict run stopped before throughput testing. Retry when quiet, continue once with conservative safeguards, or cancel.'));
				} else {
					recordAutotuneTerminalFailure(state, result, err.message || String(err));
					render();
					showError(_('Full Auto-Tune failed: %s').format(state.autotune_failure_message));
				}
			});
		};
		var runButton = E('button', {
			'class': 'btn cbi-button cbi-button-action',
			'disabled': state.autotune_running ? 'disabled' : null,
			'click': function() {
				return startCalibration(false);
			}
		}, state.autotune_result || state.autotune_diagnostics ? _('Run again') : _('Start Full Auto-Tune'));
		var conservativeButton = E('button', {
			'class': 'btn cbi-button cbi-button-positive',
			'style': state.autotune_background_block ? '' : 'display:none',
			'click': function() { return startCalibration(true); }
		}, _('Continue conservatively'));
		var cancelButton = E('button', {
			'class': 'btn cbi-button cbi-button-negative',
			'disabled': state.autotune_running ? null : 'disabled',
			'click': function() {
				cancelButton.disabled = true;
				status.textContent = _('Cancelling and restoring the previous SQM state...');
				cancelAutotuneJob(state.name, state.wan_if, state.speedtest_backend,
					state.autotune_profile);
			}
		}, _('Cancel test'));

		var fields = [
			wizardField(_('Calibration profile'),
				E('div', { 'style': 'display:flex;flex-wrap:wrap;gap:8px' }, profileButtons),
				_('The profile fixes the latency target and minimum retained capacity for this run. If the target and safety floor cannot both be met, Auto-Tune rejects the candidate instead of destroying throughput.')),
			E('div', { 'class': 'alert-message warning' }, [
				E('strong', {}, _('Traffic warning: ')),
				_('Full Auto-Tune runs two unshaped samples followed by separate router-side download-only and upload-only validation phases. An unreliable measurement may be repeated once and one bounded directional correction may be validated. Other WAN traffic can reduce confidence, but is never counted as test throughput.')
			]),
			wizardField(_('Calibration'), E('div', {}, [ runButton, ' ', conservativeButton, ' ', cancelButton, progress, status ]),
				_('All intermediate state stays in RAM. No UCI configuration is written until you confirm the Review step.'))
		];

		if (state.autotune_diagnostics)
			fields.push(renderAutotuneDiagnostics(state.autotune_diagnostics));

		return fields;
	}

	function renderSpeedStep() {
		var backend = wizardSelectOptions(speedtestBackendChoices(), state.speedtest_backend);
		var speedtestGoServerId = wizardTextInput(state.speedtest_go_server_id, 'uinteger');
		var percent = wizardTextInput(state.speedtest_apply_percent, 'and(uinteger,min(1),max(100))');
		var download = wizardTextInput(state.sqm_download, 'uinteger');
		var upload = wizardTextInput(state.sqm_upload, 'uinteger');
		var advancedOptions = wizardCheckbox(state.advanced_test_options);
		var advancedFields = [];
		var backendStatus = E('pre', { 'style': 'white-space:pre-wrap;margin:6px 0 0 0' }, '');
		var status = E('div', { 'class': 'cake-autorate-speedtest-status' }, '');
		var summary = E('div', {
			'class': 'cake-autorate-speedtest-summary',
			'style': 'display:inline-block;vertical-align:middle;margin-left:10px;max-width:680px;white-space:normal;color:#555;font-size:12px;line-height:1.35'
		});
		var pingerStatus = E('pre', { 'style': 'white-space:pre-wrap;margin:6px 0 0 0' }, '');
		var syncInputs = function() {
			state.speedtest_backend = backend.value || 'auto';
			state.speedtest_go_server_id = speedtestGoServerId.value.trim();
			state.speedtest_apply_percent = percent.value || '90';
			state.advanced_test_options = advancedOptions.checked;
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

				runSpeedtestJob(state.name, state.wan_if, state.speedtest_backend, function() {
					status.textContent = _('Running speed test...');
				}, state.route_mode, state.mwan3_member).then(function(res) {
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

				runPingerPlan(state.name, 'scan', state.route_mode, state.mwan3_member).then(function(result) {
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

		var updateAdvancedVisibility = function() {
			state.advanced_test_options = advancedOptions.checked;

			for (var i = 0; i < advancedFields.length; i++)
				advancedFields[i].style.display = state.advanced_test_options ? '' : 'none';

			installButton.style.display = state.advanced_test_options &&
				speedtestBackendInstallable(backend.value || 'auto') ? '' : 'none';
		};

		backend.addEventListener('change', function() {
			syncInputs();
			updateSummary();
			updateAdvancedVisibility();
		});
		speedtestGoServerId.addEventListener('input', syncInputs);
		speedtestGoServerId.addEventListener('change', syncInputs);
		advancedOptions.addEventListener('change', updateAdvancedVisibility);
		percent.addEventListener('input', function() { syncInputs(); updateSummary(); });
		percent.addEventListener('change', function() { syncInputs(); updateSummary(); });
		download.addEventListener('input', function() { syncInputs(); updateSummary(); });
		download.addEventListener('change', function() { syncInputs(); updateSummary(); });
		upload.addEventListener('input', function() { syncInputs(); updateSummary(); });
		upload.addEventListener('change', function() { syncInputs(); updateSummary(); });
		advancedFields = [
			wizardField(_('Preferred backend'), backend, optionDescriptions.speedtest_backend),
			wizardField(_('speedtest-go server ID'), speedtestGoServerId, optionDescriptions.speedtest_go_server_id),
			wizardField(_('Check backends'), E('div', {}, [ checkButton, ' ', installButton, backendStatus ]), optionDescriptions._speedtest_backend_status),
			wizardField(_('Speed test apply percent'), percent, optionDescriptions.speedtest_apply_percent),
			wizardField(_('Reflector plan'), E('div', {}, [ scanReflectorsButton, pingerStatus ]), optionDescriptions._wizard_reflector_plan)
		];

		updateSummary();
		updateAdvancedVisibility();

		return [
			wizardField(_('Download speed'), download, optionDescriptions.sqm_download),
			wizardField(_('Upload speed'), upload, optionDescriptions.sqm_upload),
			wizardField(_('Run speed test'), E('div', {}, [ runButton, summary, status ]), optionDescriptions._speedtest),
			wizardField(_('Advanced test options'), advancedOptions, optionDescriptions._wizard_advanced_test_options)
		].concat(advancedFields);
	}

	function renderReviewStep() {
		var wan = normalizeInterfaceName(state.wan_if);
		var reflectors = (state.reflectors && state.reflectors.length) ? state.reflectors : defaultReflectors();
		var activeCount = Math.min(parseInt(state.no_pingers || '6', 10), reflectors.length);
		var reviewNodes = [];
		var selectedAutotuneAction = state.autotune_action || 'apply_sqm';
		var autorateDecision = selectedAutotuneAction === 'disable_sqm' ?
			_('disable autorate and SQM') :
			(selectedAutotuneAction === 'keep_current' ?
				_('keep current settings') : (state.enabled ? _('enabled') : _('disabled')));
		var rows = [
			[ _('Target interface'), targetInterfaceLabel(wan) ],
			[ _('Probe routing'), state.route_mode === 'mwan3' ? _('mwan3 member %s').format(state.mwan3_member) : _('Main routing table') ],
			[ _('Autorate + SQM'), autorateDecision ],
			[ _('SQM queue'), wizardSqmQueueText(state) ],
			[ _('Download speed'), state.sqm_download + ' kbit/s' ],
			[ _('Upload speed'), state.sqm_upload + ' kbit/s' ],
			[ _('Preferred backend'), speedtestBackendChoiceTitle(state.speedtest_backend) ],
			[ _('Pinger plan'), _('%s, %d active / %d candidates').format(state.pinger_method || 'fping', activeCount, reflectors.length) ]
		];
		if (state.autotune_diagnostics && !state.autotune_result)
			reviewNodes.push(renderAutotuneDiagnostics(state.autotune_diagnostics));
		if (state.multiwan_set) {
			var multiwanPlans = multiwanInstancePlans(state);
			var multiwanConflicts = wizardPlanConflicts(multiwanPlans, state.enabled,
				rerun ? existingName : null);
			rows.push([ _('Multi-WAN instances'), multiwanPlans.map(function(item) {
				return '%s: %s → %s; %s'.format(item.name, item.member, item.device, item.sqmSection);
			}).join('\n') ]);
			rows.push([ _('Detected conflicts'), multiwanConflicts.length ? multiwanConflicts.join('\n') : _('None') ]);
		}
		/* Never render a proposal in Review unless it is also eligible for
		 * staging.  This keeps a stale result object from becoming an implied
		 * approval surface after a failed or interrupted run. */
		var autotune = autotuneResultHasReviewChoice(state.autotune_result) ?
			state.autotune_result : null;
		if (autotune) {
			var proposal = autotune.proposal;
			var validation = autotune.validation;
			var dl = proposal.download;
			var ul = proposal.upload;
			var adaptive = proposal.adaptive_ceiling;
			var adaptiveDecision = adaptiveCeilingWritePlan(state, proposal);
			var needsAdaptiveConsent = !!(state.original_adaptive_ceiling &&
				state.original_adaptive_ceiling.enabled === true && adaptive.enabled === false);
			var thresholds = proposal.thresholds_ms;
			var firstRun = autotune.runs && autotune.runs.length ? autotune.runs[0] : {};
			var profilePolicy = autotuneProfilePolicy(autotune.profile);
			rows.push(
				[ _('Calibration profile'), profilePolicy ?
					_('%s · target %s or better').format(
						autotuneProfileDefinitions().filter(function(item) {
							return item.id === profilePolicy.id;
						})[0].title, profilePolicy.targetGrade) : '-' ],
				[ _('Calibration confidence'), (autotune.confidence_mode === 'low' ? _('LOW · ') : '') + String(proposal.confidence) + '%' ],
				[ _('Idle latency'), _('%s ms median / %s ms p95').format(autotune.baseline.median_ms, autotune.baseline.p95_ms) ],
				[ _('Observed download'), _('%d / %d / %d kbit/s low / median / high').format(dl.observed_low_kbps, dl.observed_median_kbps, dl.observed_high_kbps) ],
				[ _('Observed upload'), _('%d / %d / %d kbit/s low / median / high').format(ul.observed_low_kbps, ul.observed_median_kbps, ul.observed_high_kbps) ],
				[ _('Proposed DL min / base / max'), _('%d / %d / %d kbit/s').format(dl.minimum_kbps, dl.base_kbps, dl.maximum_kbps) ],
				[ _('Proposed UL min / base / max'), _('%d / %d / %d kbit/s').format(ul.minimum_kbps, ul.base_kbps, ul.maximum_kbps) ],
				[ _('Delay thresholds'), _('%d / %d / %d ms adjust-up / delay / adjust-down').format(thresholds.adjust_up, thresholds.delay, thresholds.adjust_down) ],
				[ _('Adaptive ceiling'), adaptive.enabled ? _('enabled; caps %d / %d kbit/s').format(dl.absolute_cap_kbps, ul.absolute_cap_kbps) :
					(adaptiveDecision.preserved ?
						_('Auto-Tune recommends disabling it, but the existing enabled setting and tuning parameters will be preserved.') :
						_('disabled as proposed for the measured stable link')) ],
				[ _('CAKE traffic classes'), proposal.sqm.classification === 'diffserv4' ?
					_('diffserv4; preserve existing DSCP (no application guessing)') :
					_('best effort; ignore external DSCP') ],
				[ _('Detected link layer'), _('%s; overhead %d, MPU %d').format(proposal.link.kind, proposal.link.overhead, proposal.link.mpu) ],
				[ _('Test server'), firstRun.server_sponsor ? _('%s #%s').format(firstRun.server_sponsor, firstRun.server_id || '-') : _('automatic') ]
			);
			if (autotune.profile === 'fair' && validation &&
			    validation.quality_target_met === false) {
				var outcome = autotune.fair_outcome;
				var actionChoices = [
					{
						id: 'apply_sqm',
						title: _('Apply the best safe Fair SQM candidate'),
						description: _('Keeps the measured 90% throughput floor. The loaded-latency target was not reached, so this is an explicit manual choice.')
					},
					{
						id: 'keep_current',
						title: _('Keep current settings'),
						description: rerun ? _('Close Auto-Tune without writing any configuration.') :
							_('Do not create this instance.')
					}
				];
				if (rerun && autotuneResultReviewable(autotune, 'disable_sqm')) {
					actionChoices.push({
						id: 'disable_sqm',
						title: _('Disable autorate and SQM (comparison suggestion)'),
						description: _('The unshaped control was no worse for latency and improved both download and upload by at least 2%. This is reversible, but traffic will no longer be shaped.')
					});
				}
				var actionGroup = E('div', {
					'style': 'display:flex;flex-direction:column;gap:8px'
				}, actionChoices.map(function(choice) {
					var input = E('input', {
						'type': 'radio',
						'name': 'cake-autotune-action-' + state.name,
						'value': choice.id,
						'checked': selectedAutotuneAction === choice.id ? 'checked' : null
					});
					input.addEventListener('change', function() {
						if (!input.checked)
							return;
						state.autotune_action = choice.id;
						state.disable_sqm_confirmed = false;
						render();
					});
					return E('label', {
						'style': 'display:flex;gap:8px;align-items:flex-start;white-space:normal'
					}, [
						input,
						E('span', {}, [
							E('strong', {}, choice.title),
							E('br'),
							E('span', { 'style': 'font-size:12px' }, choice.description)
						])
					]);
				}));
				rows.push(
					[ _('Fair result'), _('%s · +%s ms effective loaded latency; target C is +200 ms or less').format(
						validation.actual_grade || '-',
						autotuneNumber(validation.effective_delta_ms) == null ? '-' :
							autotuneNumber(validation.effective_delta_ms).toFixed(1)) ],
					[ _('Review action'), actionGroup ]
				);
				if (outcome && outcome.no_sqm_control && outcome.no_sqm_control.available === true) {
					rows.push([ _('Unshaped control'), _('%s · +%s ms; throughput change DL %s%% / UL %s%%').format(
						outcome.no_sqm_control.grade || '-',
						autotuneNumber(outcome.no_sqm_control.effective_delta_ms).toFixed(1),
						autotuneNumber(outcome.throughput_gain_without_sqm.download_percent).toFixed(1),
						autotuneNumber(outcome.throughput_gain_without_sqm.upload_percent).toFixed(1)) ]);
				}
				if (selectedAutotuneAction === 'disable_sqm') {
					var disableSqm = wizardCheckbox(state.disable_sqm_confirmed === true);
					disableSqm.addEventListener('change', function() {
						state.disable_sqm_confirmed = disableSqm.checked;
					});
					rows.push([ _('Disable-SQM confirmation'), E('label', {
						'style': 'white-space:normal;color:#d9534f;font-weight:600'
					}, [
						disableSqm, ' ',
						_('I understand that this disables CAKE shaping and may increase latency under load.')
					]) ]);
				}
			}
			if (needsAdaptiveConsent) {
				var disableAdaptive = wizardCheckbox(state.adaptive_ceiling_disable_confirmed === true);
				disableAdaptive.addEventListener('change', function() {
					state.adaptive_ceiling_disable_confirmed = disableAdaptive.checked;
					render();
				});
				rows.push([ _('Adaptive ceiling consent'), E('label', { 'style': 'white-space:normal' }, [
					disableAdaptive, ' ',
					_('Explicitly allow this proposal to disable the currently enabled adaptive ceiling.')
				]) ]);
			}
			if (autotune.conservative) {
				var background = autotune.background || {};
				var usable = autotune.usable_directions || {};
				rows.push(
					[ _('Conservative override'), _('One run only; background DL %s / UL %s kbit/s was subtracted with an extra safety margin.').format(
						background.download_kbps || 0, background.upload_kbps || 0) ],
					[ _('Download decision'), usable.download === false ? _('Retain all confirmed download limits; the direction was unusable.') : _('Use the lower conservative proposal; never raise the confirmed maximum or absolute cap.') ],
					[ _('Upload decision'), usable.upload === false ? _('Retain all confirmed upload limits; the direction was unusable.') : _('Use the lower conservative proposal; never raise the confirmed maximum or absolute cap.') ]
				);
			}
			if (autotune.baseline.http_median_ms != null) {
				rows.push([ _('Idle TCP/HTTPS latency'), _('%s ms median / %s ms p95').format(
					autotune.baseline.http_median_ms,
					autotune.baseline.http_p95_ms) ]);
			}
			if (validation)
				reviewNodes.push(renderAutotuneDiagnostics(autotune));
			if (proposal.warnings && proposal.warnings.length)
				rows.push([ _('Auto-Tune warnings'), proposal.warnings.join(' ') ]);
		}

		if (state.advanced_test_options) {
			rows.push(
				[ _('Download interface'), ifbForWan(wan) ],
				[ _('Upload interface'), wan ],
				[ _('Queueing discipline'), state.sqm_qdisc || 'cake' ],
				[ _('Queue setup script'), state.sqm_script || 'piece_of_cake.qos' ],
				[ _('Speed test apply percent'), String(state.speedtest_apply_percent || '90') + '%' ],
				[ _('speedtest-go server ID'), state.speedtest_go_server_id || _('automatic') ],
				[ _('Extra ping args'), state.ping_extra_args || '-' ],
				[ _('Active reflectors'), reflectors.slice(0, activeCount).join(', ') ],
				[ _('Derived minimum rates'), '%s / %s kbit/s'.format(halfRate(state.sqm_download), halfRate(state.sqm_upload)) ]
			);
		}

		reviewNodes.push(
			E('table', { 'class': 'table' }, rows.map(function(row) {
				return E('tr', { 'class': 'tr' }, [
					E('td', { 'class': 'td' }, row[0]),
					E('td', { 'class': 'td' }, row[1])
				]);
			}))
		);

		return reviewNodes;
	}

	function validateStep(step) {
		showError(null);
		step = step == null ? state.step : step;

		if (step === 0 && !state.wan_if) {
			showError(_('Target interface is required.'));
			return false;
		}
		if (step === 0 && state.route_mode === 'mwan3') {
			if (!mwan3Capability.available || !mwan3Capability.nft || !mwan3Capability.scoped_status_api) {
				showError(_('This router does not provide the required nftables mwan3 member API.'));
				return false;
			}
			var selectedMember = mwan3Context.byName[state.mwan3_member];
			if (!selectedMember || selectedMember.device !== normalizeInterfaceName(state.wan_if)) {
				showError(_('The selected mwan3 member does not match the target interface.'));
				return false;
			}
		}

		if (step === 1) {
			if (state.mode === 'autotune' && !autotuneResultHasReviewChoice(state.autotune_result)) {
				showError(state.autotune_diagnostics ?
					_('This Auto-Tune run produced diagnostics only. Run it again successfully before continuing to Review.') :
					_('Run Full Auto-Tune before continuing to Review.'));
				return false;
			}

			if (state.mode === 'autotune')
				return true;

			if (state.speedtest_go_server_id && !validatePositiveInteger(state.speedtest_go_server_id)) {
				showError(_('speedtest-go server ID must be a positive integer.'));
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

		}

		return true;
	}

	function navigateWizardStep(target) {
		if (isNaN(target) || target < 0 || target > 2 || target === state.step)
			return;

		if (target > state.step) {
			if (!validateStep(state.step))
				return;

			if (state.step === 0 && target === 2 && !validateStep(1))
				return;
		}

		state.step = target;
		render();
	}

	function validateWizard() {
		showError(null);
		var selectedAction = state.autotune_action || 'apply_sqm';
		if (state.mode === 'autotune' &&
		    !autotuneResultReviewable(state.autotune_result, selectedAction)) {
			showError(_('The selected Auto-Tune action is not supported by the validated evidence.'));
			return false;
		}
		if (selectedAction === 'disable_sqm' && (!rerun ||
		    state.disable_sqm_confirmed !== true)) {
			showError(rerun ?
				_('Confirm that you understand the effect of disabling SQM.') :
				_('SQM can be disabled only for an existing instance.'));
			return false;
		}
		if (selectedAction === 'keep_current')
			return true;

		if (!state.name) {
			showError(_('Instance name is required.'));
			return false;
		}

		if (!state.wan_if) {
			showError(_('Target interface is required.'));
			return false;
		}

		if (state.route_mode === 'mwan3') {
			if (!mwan3Capability.available || !mwan3Capability.nft || !mwan3Capability.scoped_status_api) {
				showError(_('This router does not provide the required nftables mwan3 member API.'));
				return false;
			}
			var member = mwan3Context.byName[state.mwan3_member];
			if (!member || member.device !== normalizeInterfaceName(state.wan_if)) {
				showError(_('Select an online-configured mwan3 member matching the target interface.'));
				return false;
			}
		}

		var plans = state.multiwan_set ? multiwanInstancePlans(state) : [ {
			name: state.name,
			device: normalizeInterfaceName(state.wan_if)
		} ];
		var planConflicts = wizardPlanConflicts(plans, state.enabled, rerun ? existingName : null);
		if (planConflicts.length) {
			showError(planConflicts.join(' '));
			return false;
		}

		if (!validatePositiveInteger(state.speedtest_apply_percent) ||
		    parseInt(state.speedtest_apply_percent, 10) > 100) {
			showError(_('Speed test apply percent must be between 1 and 100.'));
			return false;
		}

		if (state.speedtest_go_server_id && !validatePositiveInteger(state.speedtest_go_server_id)) {
			showError(_('speedtest-go server ID must be a positive integer.'));
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

		if (parseInt(state.no_pingers || '1', 10) >
		    ((state.reflectors && state.reflectors.length) ? state.reflectors.length : defaultReflectors().length)) {
			showError(_('Pingers cannot exceed reflector count.'));
			return false;
		}

		return true;
	}

	function finish() {
		var config_name = grid.uciconfig || grid.map.config;

		if (!validateWizard())
			return;
		if (state.autotune_action === 'keep_current') {
			ui.hideModal();
			ui.addNotification(null, E('p', rerun ?
				_('Current settings were kept; no configuration was written.') :
				_('Instance creation was cancelled; no configuration was written.')), 'info');
			return;
		}

		var freshness = state.mode === 'autotune' ?
			revalidateAutotuneProposal(state.name, state.wan_if,
				state.speedtest_backend, state.autotune_result,
				state.route_mode, state.mwan3_member,
				state.autotune_action || 'apply_sqm') : Promise.resolve();

		return freshness.then(function() {
			var section_id, created = [];
			var plans = state.multiwan_set ? multiwanInstancePlans(state) : [ {
				name: state.name,
				member: state.mwan3_member,
				device: state.wan_if,
				sqmSection: state.sqm_section || managedSqmSectionName(state.name)
			} ];

			for (var planIndex = 0; planIndex < plans.length; planIndex++) {
				var plan = plans[planIndex];
				var instanceState = {};
				for (var key in state)
					if (state.hasOwnProperty(key))
						instanceState[key] = state[key];
				instanceState.name = plan.name;
				instanceState.wan_if = plan.device;
				instanceState.route_mode = plan.member ? 'mwan3' : state.route_mode;
				instanceState.mwan3_member = plan.member || '';
				instanceState.route_selection = plan.member ? 'mwan3:' + plan.member : 'main';
				instanceState.sqm_section = plan.sqmSection;
				instanceState.ping_extra_args = pingerInterfaceArgs(plan.device, instanceState.pinger_method || 'fping');
					var existingQueue = state.mode === 'autotune' ? null :
						findImportableSqmQueueForInterface(plan.device);
				if (existingQueue) {
					instanceState.sqm_section = queueSectionName(existingQueue);
					instanceState.sqm_download = rateValue(existingQueue.download, instanceState.sqm_download);
					instanceState.sqm_upload = rateValue(existingQueue.upload, instanceState.sqm_upload);
				}

				section_id = rerun ? existingName : grid.map.data.add(config_name, grid.sectiontype, plan.name);
				writeWizardConfig(section_id, instanceState);
				if (state.mode === 'autotune')
					stageAutotuneApplyMarker(section_id, instanceState);
				created.push(section_id);
			}

			return grid.map.save(null, true).then(function() { return created; });
		})
			.then(function(created) {
				return L.bind(grid.map.load, grid.map)().then(function() { return created; });
			})
			.then(function(created) {
				return L.bind(grid.map.reset, grid.map)().then(function() { return created; });
			})
			.then(function(created) {
				var notification;
				ui.hideModal();
				if (state.autotune_action === 'disable_sqm')
					notification = _('SQM disable is staged for %s. Review pending changes, then Save & Apply.').format(existingName);
				else if (rerun)
					notification =
						_('Auto-Tune proposal staged for %s. Review pending changes, then Save & Apply.').format(existingName);
				else
					notification = _('%d instance(s) created: %s. Review pending changes, then Save & Apply.').format(
						created.length, created.join(', '));
				ui.addNotification(null, E('p', notification), 'info');
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
					if (state.autotune_running)
						cancelAutotuneJob(state.name, state.wan_if,
							state.speedtest_backend, state.autotune_profile);
					ui.hideModal();
				}
			}, _('Cancel')),
			' '
		];
		var stepFields = state.step === 0 ? renderInterfaceStep() :
			state.step === 1 ? (state.mode === 'autotune' ? renderAutotuneStep() : renderSpeedStep()) :
			renderReviewStep();

		for (var i = 0; i < stepFields.length; i++)
			content.push(stepFields[i]);

		if (state.step > 0) {
			buttons.push(E('button', {
				'class': 'btn cbi-button',
				'click': function() {
					navigateWizardStep(state.step - 1);
				}
			}, _('Back')));
			buttons.push(' ');
		}

		var invalidAutotune = state.mode === 'autotune' &&
			!autotuneResultHasReviewChoice(state.autotune_result);
		if (state.step < 2 && !(state.step === 1 && invalidAutotune)) {
			buttons.push(E('button', {
				'class': 'btn cbi-button cbi-button-positive',
				'click': function() {
					navigateWizardStep(state.step + 1);
				}
			}, _('Next')));
		}
		else if (invalidAutotune && state.autotune_diagnostics) {
			buttons.push(E('button', {
				'class': 'btn cbi-button cbi-button-positive',
				'click': function() { ui.hideModal(); }
			}, _('Close diagnostics')));
		}
		else if (!invalidAutotune) {
			buttons.push(E('button', {
				'class': 'btn cbi-button cbi-button-positive important',
				'click': finish
			}, state.autotune_action === 'keep_current' ? _('Keep current') :
				(state.autotune_action === 'disable_sqm' ? _('Stage SQM disable') :
					(rerun ? _('Use proposal') : _('Create')))));
		}

		content.push(E('div', { 'class': 'button-row' }, buttons));

		replaceNodeContent(body, content);
	}

	ui.showModal(rerun ? _('Re-run Auto-Tune — %s').format(name) :
		_('Create CAKE Autorate - %s').format(name), body, 'cbi-modal');
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
	for (var i = 0; i < section.children.length; i++) {
		var option = section.children[i];

		if (!option.modalonly || option.tab !== 'advanced')
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

function topicTab(tab) {
	var topics = {
		autorate: 'autorate', sqm: 'sqm', testing: 'testing', monitoring: 'monitoring', advanced: 'advanced',
		setup: 'autorate', general: 'autorate', rates: 'autorate', quality: 'autorate',
		reflectors: 'autorate', latency: 'autorate', controller: 'autorate',
		interfaces: 'sqm', sqm_basic: 'sqm', sqm_qdisc: 'sqm', sqm_linklayer: 'sqm',
		speedtest: 'testing', testing: 'testing', logging: 'monitoring', advanced: 'advanced'
	};
	return topics[tab] || 'advanced';
}

function autorateSubcategory(tab, optionName) {
	if (optionName === '_autorate_topic')
		return null;

	if (tab === 'general')
		return 'limits';

	if (tab === 'rates')
		return optionName.indexOf('adaptive_ceiling_') === 0 ? 'ceiling' : 'limits';

	if (tab === 'reflectors')
		return 'probes';

	if (tab === 'quality') {
		if (optionName === 'transport_latency_enabled' ||
			optionName.indexOf('transport_probe_') === 0 ||
			optionName === 'transport_load_hold_s' ||
			optionName === 'transport_cpu_max_percent')
			return 'probes';

		return 'quality';
	}

	if (tab === 'latency' || tab === 'controller')
		return 'controller';

	if (tab === 'setup') {
		if (optionName === 'manual_rate_limits' ||
			optionName.indexOf('min_') === 0 || optionName.indexOf('base_') === 0 ||
			optionName.indexOf('max_') === 0)
			return 'limits';

		return 'connection';
	}

	return 'connection';
}

function autorateSubcategoryDefinitions() {
	return [
		{
			id: 'connection',
			title: _('Connection & routing'),
			description: _('Enable the instance, select its uplink and route, and set the normal download and upload rates.')
		},
		{
			id: 'limits',
			title: _('Rate limits'),
			description: _('Control which directions may change and, when needed, set explicit minimum, base, and maximum rates.')
		},
		{
			id: 'ceiling',
			title: _('Adaptive ceiling'),
			description: _('Configure bounded clean-load probes that can raise the learned-safe ceiling without exceeding absolute caps.')
		},
		{
			id: 'probes',
			title: _('Latency probes'),
			description: _('Select ICMP/OWD reflectors and the route-bound transport RTT signal used for quality measurement.')
		},
		{
			id: 'quality',
			title: _('Quality & rating'),
			description: _('Tune load detection, guided rating capture, optional transport control, and throughput safety floors.')
		},
		{
			id: 'controller',
			title: _('Controller'),
			description: _('Advanced delay thresholds, smoothing, detection windows, and CAKE rate adjustment factors.')
		}
	];
}

function decorateAutorateSubcategories(section, sectionId, containers) {
	var autorateContainer = containers.querySelector('[data-tab="autorate"]');
	if (!autorateContainer || autorateContainer.querySelector('.cake-autorate-subnav'))
		return containers;

	var optionGroups = {};
	section.children.forEach(function(option) {
		if (option.cakeAutorateGroup)
			optionGroups[option.option] = option.cakeAutorateGroup;
	});

	var definitions = autorateSubcategoryDefinitions();
	var panels = {};
	var tabs = {};
	var tabItems = {};
	var nav = E('ul', {
		'class': 'cbi-tabmenu cake-autorate-subnav',
		'role': 'tablist',
		'aria-label': _('Autorate settings sections')
	});
	var panelRoot = E('div', { 'class': 'cake-autorate-subpanels' });

	definitions.forEach(function(definition) {
		var panelId = 'cake-autorate-subpanel-%s-%s'.format(sectionId, definition.id);
		var tab = E('a', {
			'href': '#',
			'role': 'tab',
			'aria-controls': panelId,
			'aria-selected': 'false'
		}, definition.title);
		var tabItem = E('li', {
			'class': 'cbi-tab-disabled',
			'role': 'presentation',
			'data-subtab': definition.id
		}, [ tab ]);
		var panel = E('div', {
			'id': panelId,
			'class': 'cake-autorate-subpanel',
			'role': 'tabpanel'
		}, [
			E('p', { 'class': 'cake-autorate-subdescription' }, definition.description)
		]);

		tab.addEventListener('click', function(ev) {
			ev.preventDefault();
			activate(definition.id);
		});
		tab.addEventListener('keydown', function(ev) {
			var index = definitions.findIndex(function(item) { return item.id === definition.id; });
			var target = null;
			if (ev.key === 'ArrowLeft' || ev.key === 'ArrowUp')
				target = definitions[(index + definitions.length - 1) % definitions.length].id;
			else if (ev.key === 'ArrowRight' || ev.key === 'ArrowDown')
				target = definitions[(index + 1) % definitions.length].id;
			else if (ev.key === 'Home')
				target = definitions[0].id;
			else if (ev.key === 'End')
				target = definitions[definitions.length - 1].id;
			if (target) {
				ev.preventDefault();
				activate(target);
				tabs[target].focus();
			}
		});
		tabs[definition.id] = tab;
		tabItems[definition.id] = tabItem;
		panels[definition.id] = panel;
		nav.appendChild(tabItem);
		panelRoot.appendChild(panel);
	});

	Array.prototype.slice.call(autorateContainer.children).forEach(function(node) {
		var group = node.getAttribute && optionGroups[node.getAttribute('data-name')];
		if (group && panels[group])
			panels[group].appendChild(node);
	});

	function activate(group) {
		if (!panels[group])
			group = definitions[0].id;
		autorateSubcategoryStates[sectionId] = group;

		definitions.forEach(function(definition) {
			var active = definition.id === group;
			panels[definition.id].style.display = active ? '' : 'none';
			tabItems[definition.id].className = active ? 'cbi-tab' : 'cbi-tab-disabled';
			tabs[definition.id].setAttribute('aria-selected', active ? 'true' : 'false');
			tabs[definition.id].setAttribute('tabindex', active ? '0' : '-1');
		});
	}

	autorateContainer.appendChild(E('style', {}, [
		'.cake-autorate-subnav{margin:12px 0 14px;max-width:100%;overflow-x:auto;overflow-y:hidden;flex-wrap:nowrap;scrollbar-width:thin}',
		'.cake-autorate-subnav>li{flex:0 0 auto}',
		'.cake-autorate-subnav>li>a{white-space:nowrap}',
		'.cake-autorate-subpanel{min-width:0}',
		'.cake-autorate-subdescription{margin:0 0 12px;color:var(--text-color-medium,#777)}'
	].join('')));
	autorateContainer.appendChild(nav);
	autorateContainer.appendChild(panelRoot);
	activate(autorateSubcategoryStates[sectionId] || definitions[0].id);
	return containers;
}

function addTopicIntroduction(section, tab, name, text) {
	var option = section.taboption(tab, form.DummyValue, name, '');
	modal(option);
	option.rawhtml = true;
	option.cfgvalue = function() {
		return E('div', { 'class': 'alert-message notice cake-settings-topic-intro' }, text);
	};
	option.write = function() {};
	option.remove = function() {};
}

function addRateOptions(section) {
	var o;

	value(section, 'rates', 'connection_active_thr_kbps', _('Active threshold'), 'uinteger', '2000');

	o = flag(section, 'rates', 'adaptive_ceiling_enabled', _('Adaptive ceiling'), '0');
	o.validate = function(section_id) {
		return validateAdaptiveCeiling(validationSection(this), section_id);
	};

	o = value(section, 'rates', 'adaptive_ceiling_dl_cap_kbps', _('DL absolute cap'), 'and(uinteger,min(1))', '80000');
	o.depends('adaptive_ceiling_enabled', '1');
	o.cfgvalue = function(section_id) {
		return rateValue(uci.get('cake-autorate', section_id, 'adaptive_ceiling_dl_cap_kbps'),
			rateValue(uci.get('cake-autorate', section_id, 'max_dl_shaper_rate_kbps'), '80000'));
	};
	o.validate = function(section_id) {
		return validateAdaptiveCeiling(validationSection(this), section_id);
	};

	o = value(section, 'rates', 'adaptive_ceiling_ul_cap_kbps', _('UL absolute cap'), 'and(uinteger,min(1))', '35000');
	o.depends('adaptive_ceiling_enabled', '1');
	o.cfgvalue = function(section_id) {
		return rateValue(uci.get('cake-autorate', section_id, 'adaptive_ceiling_ul_cap_kbps'),
			rateValue(uci.get('cake-autorate', section_id, 'max_ul_shaper_rate_kbps'), '35000'));
	};
	o.validate = function(section_id) {
		return validateAdaptiveCeiling(validationSection(this), section_id);
	};

	o = value(section, 'rates', 'adaptive_ceiling_hold_time_s', _('Qualification time'), 'and(ufloat,min(1))', '20.0');
	o.depends('adaptive_ceiling_enabled', '1');

	o = value(section, 'rates', 'adaptive_ceiling_growth_percent', _('Open probe step'), 'and(ufloat,min(0.1),max(10))', '3.0');
	o.depends('adaptive_ceiling_enabled', '1');

	o = value(section, 'rates', 'adaptive_ceiling_probe_duration_s', _('Probe observation'), 'and(ufloat,min(1))', '8.0');
	o.depends('adaptive_ceiling_enabled', '1');

	o = value(section, 'rates', 'adaptive_ceiling_cooldown_s', _('Probe cooldown'), 'and(ufloat,min(0))', '30.0');
	o.depends('adaptive_ceiling_enabled', '1');

	o = value(section, 'rates', 'adaptive_ceiling_failed_bound_ttl_s', _('Failed-bound memory'), 'and(ufloat,min(1))', '900.0');
	o.depends('adaptive_ceiling_enabled', '1');
}

function addQualityOptions(section) {
	var o;

	o = flag(section, 'quality', 'transport_latency_enabled', _('Transport-aware latency'), '0');

	o = listValue(section, 'quality', 'transport_probe_backend', _('Probe backend'), [
		['websocket', _('Persistent WebSocket (recommended)')],
		['tcp', _('TCP connect RTT')],
		['http', _('Persistent HTTP')],
		['legacy-http', _('Legacy HTTP (diagnostic only)')]
	], 'websocket');
	o.depends('transport_latency_enabled', '1');

	o = value(section, 'quality', 'transport_probe_endpoint', _('Probe endpoint'), null,
		'wss://ping-bufferbloat.libreqos.com/ws');
	o.depends('transport_latency_enabled', '1');
	o.rmempty = false;
	o.validate = function(section_id, value) {
		return validateTransportProbeUrl(
			formOrUci(validationSection(this), section_id, 'transport_probe_backend'),
			value
		);
	};

	o = value(section, 'quality', 'transport_probe_idle_interval_s', _('Idle probe interval'), 'and(ufloat,min(5),max(3600))', '15.0');
	o.depends('transport_latency_enabled', '1');
	o = value(section, 'quality', 'transport_probe_loaded_interval_s', _('Loaded probe interval'), 'and(ufloat,min(0.5),max(60))', '1.0');
	o.depends('transport_latency_enabled', '1');
	o = value(section, 'quality', 'transport_probe_timeout_s', _('Probe timeout'), 'and(uinteger,min(1),max(30))', '5');
	o.depends('transport_latency_enabled', '1');
	o = value(section, 'quality', 'transport_load_hold_s', _('Stable load hold'), 'and(ufloat,min(1),max(30))', '3.0');
	o.depends('transport_latency_enabled', '1');
	o = value(section, 'quality', 'transport_cpu_max_percent', _('CPU rejection threshold'), 'and(ufloat,min(50),max(100))', '85.0');
	o.depends('transport_latency_enabled', '1');
	o = value(section, 'quality', 'rating_load_window_s', _('Rating load window'), 'and(ufloat,min(0.5),max(10))', '2.0');
	o.depends('transport_latency_enabled', '1');
	o = value(section, 'quality', 'rating_load_enter_ratio', _('Rating enter ratio'), 'and(ufloat,min(0.1),max(1))', '0.60');
	o.depends('transport_latency_enabled', '1');
	o.validate = function(section_id) {
		return validateRatingLoadRatios(validationSection(this), section_id);
	};
	o = value(section, 'quality', 'rating_load_exit_ratio', _('Rating exit ratio'), 'and(ufloat,min(0.05),max(0.99))', '0.40');
	o.depends('transport_latency_enabled', '1');
	o.validate = function(section_id) {
		return validateRatingLoadRatios(validationSection(this), section_id);
	};
	o = value(section, 'quality', 'rating_load_hold_s', _('Rating phase hold'), 'and(ufloat,min(0.2),max(10))', '1.0');
	o.depends('transport_latency_enabled', '1');
	o = value(section, 'quality', 'rating_load_dropout_s', _('Rating dropout tolerance'), 'and(ufloat,min(0.2),max(10))', '1.5');
	o.depends('transport_latency_enabled', '1');
	o = value(section, 'quality', 'rating_load_min_kbps', _('Rating minimum traffic'), 'and(ufloat,min(0))', '2000');
	o.depends('transport_latency_enabled', '1');
	o = value(section, 'quality', 'rating_load_dominance_ratio', _('Direction dominance ratio'), 'and(ufloat,min(1.1),max(10))', '1.5');
	o.depends('transport_latency_enabled', '1');
	o = value(section, 'quality', 'rating_capture_min_enter_ratio', _('Capture minimum trigger'), 'and(ufloat,min(0.05),max(0.5))', '0.15');
	o.depends('transport_latency_enabled', '1');
	o = value(section, 'quality', 'rating_capture_peak_factor', _('Capture peak fraction'), 'and(ufloat,min(0.2),max(0.8))', '0.35');
	o.depends('transport_latency_enabled', '1');
	o = value(section, 'quality', 'rating_capture_contamination_ratio', _('Opposite traffic limit'), 'and(ufloat,min(0.05),max(0.5))', '0.10');
	o = value(section, 'quality', 'rating_capture_ack_ratio', _('TCP acknowledgement allowance'), 'and(ufloat,min(0.01),max(0.25))', '0.08');
	o.depends('transport_latency_enabled', '1');
	o = value(section, 'quality', 'rating_capture_quiet_s', _('Pre-test quiet window'), 'and(uinteger,min(2),max(30))', '5');
	o.depends('transport_latency_enabled', '1');
	o = value(section, 'quality', 'rating_capture_quiet_timeout_s', _('Quiet-window timeout'), 'and(uinteger,min(5),max(120))', '30');
	o.depends('transport_latency_enabled', '1');
	o = value(section, 'quality', 'rating_capture_quiet_ratio', _('Allowed background ratio'), 'and(ufloat,min(0.01),max(0.25))', '0.05');
	o.depends('transport_latency_enabled', '1');
	o = value(section, 'quality', 'rating_capture_quiet_min_kbps', _('Allowed background minimum'), 'and(ufloat,min(0))', '1000');
	o.depends('transport_latency_enabled', '1');
	o = value(section, 'quality', 'rating_episode_gap_s', _('Rating finalize gap'), 'and(ufloat,min(5),max(120))', '30.0');
	o.depends('transport_latency_enabled', '1');

	o = flag(section, 'quality', 'transport_controller_enabled', _('Allow transport control'), '0');
	o.depends('transport_latency_enabled', '1');
	o = value(section, 'quality', 'quality_target_delay_ms', _('Target loaded delay'), 'and(ufloat,min(5),max(200))', '30.0');
	o.depends({ transport_latency_enabled: '1', transport_controller_enabled: '1' });
	o = value(section, 'quality', 'quality_search_max_steps', _('Maximum search steps'), 'and(uinteger,min(1),max(10))', '3');
	o.depends({ transport_latency_enabled: '1', transport_controller_enabled: '1' });
	o = value(section, 'quality', 'quality_search_observe_s', _('Candidate observation'), 'and(ufloat,min(2),max(120))', '6.0');
	o.depends({ transport_latency_enabled: '1', transport_controller_enabled: '1' });
	o = value(section, 'quality', 'quality_search_cooldown_s', _('Limited cooldown'), 'and(ufloat,min(30),max(86400))', '900.0');
	o.depends({ transport_latency_enabled: '1', transport_controller_enabled: '1' });

	o = flag(section, 'quality', 'throughput_guard_enabled', _('Protect throughput floor'), '1');
	o.depends({ transport_latency_enabled: '1', transport_controller_enabled: '1' });
	o = value(section, 'quality', 'throughput_guard_retention_percent', _('Capacity retained'), 'and(ufloat,min(50),max(100))', '80.0');
	o.depends({ transport_latency_enabled: '1', transport_controller_enabled: '1', throughput_guard_enabled: '1' });
	o = value(section, 'quality', 'throughput_guard_dl_floor_kbps', _('Absolute DL floor'), 'uinteger', '0');
	o.depends({ transport_latency_enabled: '1', transport_controller_enabled: '1', throughput_guard_enabled: '1' });
	o = value(section, 'quality', 'throughput_guard_ul_floor_kbps', _('Absolute UL floor'), 'uinteger', '0');
	o.depends({ transport_latency_enabled: '1', transport_controller_enabled: '1', throughput_guard_enabled: '1' });
	o = value(section, 'quality', 'throughput_reference_dl_p20_kbps', _('DL capacity P20'), 'uinteger', '0');
	o.depends({ transport_latency_enabled: '1', transport_controller_enabled: '1', throughput_guard_enabled: '1' });
	o = value(section, 'quality', 'throughput_reference_dl_p50_kbps', _('DL capacity P50'), 'uinteger', '0');
	o.depends({ transport_latency_enabled: '1', transport_controller_enabled: '1', throughput_guard_enabled: '1' });
	o = value(section, 'quality', 'throughput_reference_ul_p20_kbps', _('UL capacity P20'), 'uinteger', '0');
	o.depends({ transport_latency_enabled: '1', transport_controller_enabled: '1', throughput_guard_enabled: '1' });
	o = value(section, 'quality', 'throughput_reference_ul_p50_kbps', _('UL capacity P50'), 'uinteger', '0');
	o.depends({ transport_latency_enabled: '1', transport_controller_enabled: '1', throughput_guard_enabled: '1' });

	o = listValue(section, 'testing', 'autotune_profile', _('Auto-Tune profile'), [
		[ 'gaming', _('Gaming — target A+, diffserv4') ],
		[ 'best_overall', _('Best overall — target A (recommended)') ],
		[ 'fair', _('Fair — throughput first, aim for C') ]
	], 'best_overall');
	describe(o, 'autotune_profile');

	o = flag(section, 'testing', 'scheduled_autotune_enabled', _('Scheduled Full Auto-Tune'), '0');
	o = value(section, 'testing', 'scheduled_autotune_interval_hours', _('Retune interval'), 'and(uinteger,min(1),max(8760))', '24');
	o.depends('scheduled_autotune_enabled', '1');
	o = value(section, 'testing', 'scheduled_autotune_idle_window_s', _('Required quiet time'), 'and(uinteger,min(30),max(3600))', '60');
	o.depends('scheduled_autotune_enabled', '1');
	o = value(section, 'testing', 'scheduled_autotune_window_start_hour', _('Window starts'), 'and(uinteger,min(0),max(23))', '2');
	o.depends('scheduled_autotune_enabled', '1');
	o = value(section, 'testing', 'scheduled_autotune_window_end_hour', _('Window ends'), 'and(uinteger,min(0),max(23))', '5');
	o.depends('scheduled_autotune_enabled', '1');
	o = value(section, 'testing', 'scheduled_autotune_max_traffic_mb_day', _('Daily traffic budget'), 'and(uinteger,min(100),max(1048576))', '4096');
	o.depends('scheduled_autotune_enabled', '1');
	o = flag(section, 'testing', 'scheduled_autotune_auto_apply', _('Apply validated proposal automatically'), '0');
	o.depends('scheduled_autotune_enabled', '1');
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
	o = optionalValue(section, 'speedtest', 'speedtest_go_server_id', _('speedtest-go server ID'), 'uinteger', '');
	describe(o, 'speedtest_go_server_id');
	dependsAny(o, 'speedtest_backend', [ 'auto', 'speedtest-go' ]);
	o = optionalValue(section, 'speedtest', 'speedtest_iperf3_server', _('iperf3 server'), null, '');
	o.depends('speedtest_backend', 'iperf3');
	o = optionalValue(section, 'speedtest', 'speedtest_iperf3_port', _('iperf3 port'), 'port', '');
	o.depends('speedtest_backend', 'iperf3');
}

function addSetupOptions(section) {
	var o;

	o = section.taboption('setup', form.DummyValue, '_mwan3_capability', _('mwan3 routing backend'));
	modal(o);
	o.rawhtml = true;
	o.cfgvalue = function() {
		if (!mwan3Capability.available)
			return E('span', { 'style': 'color:#b00' }, _('Unavailable; use Main routing.'));
		var safe = mwan3Capability.nft && mwan3Capability.scoped_status_api;
		return E('span', { 'style': safe ? 'color:#198754' : 'color:#b00' },
			_('%s · nftables: %s · member API: %s · %s').format(
				mwan3Capability.version || 'mwan3',
				mwan3Capability.nft ? _('yes') : _('no'),
				mwan3Capability.scoped_status_api ? _('yes') : _('no'),
				mwan3Capability.reason || '-'));
	};
	o.write = function() {};
	o.remove = function() {};

	o = flag(section, 'setup', 'enabled', _('Enable autorate'));
	o.forcewrite = true;
	o.onchange = function(ev, section_id, value) {
		var enabled = checkedFromEvent(ev, value);

		uci.set('cake-autorate', section_id, 'enabled', enabled ? '1' : '0');
		syncManagedSqmEnabled(this.section, section_id, enabled);
	};
	o.write = function(section_id, formvalue) {
		uci.set('cake-autorate', section_id, 'enabled', formvalue);
		syncManagedSqmEnabled(this.section, section_id, formvalue);
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

		syncManagedSqmEnabled(this.section, section_id);
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

		syncManagedSqmEnabled(this.section, section_id);
	};

	o = listValue(section, 'setup', 'route_mode', _('Probe routing'), [
		[ 'auto', _('Auto (main unless a member is selected)') ],
		[ 'main', _('Main routing table') ],
		[ 'mwan3', _('Specific mwan3 member') ]
	], 'auto');
	o.forcewrite = true;
	o.write = function(section_id, formvalue) {
		uci.set('cake-autorate', section_id, 'route_mode', formvalue);
		if (formvalue === 'main') {
			uci.unset('cake-autorate', section_id, 'mwan3_member');
			uci.unset('cake-autorate', section_id, 'ping_prefix_string');
		}
	};

	o = section.taboption('setup', form.ListValue, 'mwan3_member', _('mwan3 member'));
	modal(o);
	describe(o, 'mwan3_member');
	o.rmempty = true;
	o.value('', _('Select member'));
	for (var memberIndex = 0; memberIndex < mwan3Context.members.length; memberIndex++)
		o.value(mwan3Context.members[memberIndex].name, mwan3Context.members[memberIndex].label);
	o.depends('route_mode', 'auto');
	o.depends('route_mode', 'mwan3');
	o.validate = function(section_id, formvalue) {
		var mode = this.section.formvalue(section_id, 'route_mode') || 'auto';
		var target = selectedWan(this.section, section_id, null, true);
		if (mode === 'mwan3' && !formvalue)
			return _('Select an mwan3 member.');
		if (formvalue && (!mwan3Context.byName[formvalue] || mwan3Context.byName[formvalue].device !== target))
			return _('The selected member must resolve to the target interface.');
		return true;
	};
	o.write = function(section_id, formvalue) {
		if (formvalue) {
			uci.set('cake-autorate', section_id, 'mwan3_member', formvalue);
			uci.unset('cake-autorate', section_id, 'ping_prefix_string');
		} else {
			uci.unset('cake-autorate', section_id, 'mwan3_member');
		}
	};

	o = flag(section, 'setup', 'auto_interface_preset', _('Auto SQM preset'), '1');
	o.forcewrite = true;
	o.retain = true;
	o.depends('advanced_settings', '1');
	o.write = function(section_id, formvalue) {
		uci.set('cake-autorate', section_id, 'auto_interface_preset', formvalue);

		if (formvalue === '1')
			applyWanPreset(section_id, selectedWan(this.section, section_id, null, true), false, this.section);

		syncManagedSqmEnabled(this.section, section_id);
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
		if (!manualRateLimits) {
			setCakeOption(null, section_id, 'base_dl_shaper_rate_kbps', formvalue);
			setCakeOption(null, section_id, 'max_dl_shaper_rate_kbps', formvalue);
			setCakeOption(null, section_id, 'min_dl_shaper_rate_kbps', halfRate(formvalue));
		}
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
		if (!manualRateLimits) {
			setCakeOption(null, section_id, 'base_ul_shaper_rate_kbps', formvalue);
			setCakeOption(null, section_id, 'max_ul_shaper_rate_kbps', formvalue);
			setCakeOption(null, section_id, 'min_ul_shaper_rate_kbps', halfRate(formvalue));
		}
	};

	o = value(section, 'setup', 'speedtest_apply_percent', _('Speed test apply percent'), 'and(uinteger,min(1),max(100))', '90');
	o.default = '90';
	o.forcewrite = true;
	o.retain = true;
	o.depends('advanced_settings', '1');
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

		return runSpeedtestJob(section_id, wan, backend).then(function(res) {
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
	o.retain = true;
	o.depends('advanced_settings', '1');

	o = flag(section, 'setup', 'advanced_settings', _('Show expert options'), '0');
	o.forcewrite = true;

	o = value(section, 'setup', 'min_dl_shaper_rate_kbps', _('Min DL rate'), 'uinteger', '5000');
	o.depends({ advanced_settings: '1', manual_rate_limits: '1' });
	o.retain = true;
	o.validate = function(section_id) {
		return validateRateOrder(validationSection(this), section_id, 'dl');
	};

	o = value(section, 'setup', 'base_dl_shaper_rate_kbps', _('Base DL rate'), 'uinteger', '20000');
	o.depends({ advanced_settings: '1', manual_rate_limits: '1' });
	o.retain = true;
	o.validate = function(section_id) {
		return validateRateOrder(validationSection(this), section_id, 'dl');
	};

	o = value(section, 'setup', 'max_dl_shaper_rate_kbps', _('Max DL rate'), 'uinteger', '80000');
	o.depends({ advanced_settings: '1', manual_rate_limits: '1' });
	o.retain = true;
	o.validate = function(section_id) {
		return validateRateOrder(validationSection(this), section_id, 'dl');
	};

	o = value(section, 'setup', 'min_ul_shaper_rate_kbps', _('Min UL rate'), 'uinteger', '5000');
	o.depends({ advanced_settings: '1', manual_rate_limits: '1' });
	o.retain = true;
	o.validate = function(section_id) {
		return validateRateOrder(validationSection(this), section_id, 'ul');
	};

	o = value(section, 'setup', 'base_ul_shaper_rate_kbps', _('Base UL rate'), 'uinteger', '20000');
	o.depends({ advanced_settings: '1', manual_rate_limits: '1' });
	o.retain = true;
	o.validate = function(section_id) {
		return validateRateOrder(validationSection(this), section_id, 'ul');
	};

	o = value(section, 'setup', 'max_ul_shaper_rate_kbps', _('Max UL rate'), 'uinteger', '35000');
	o.depends({ advanced_settings: '1', manual_rate_limits: '1' });
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
	value(section, 'advanced', 'route_stability_s', _('Route stability wait'), 'and(ufloat,min(1),max(300))', '5.0');
	value(section, 'advanced', 'route_check_interval_s', _('Route check interval'), 'and(ufloat,min(1),max(60))', '2.0');
	optionalValue(section, 'advanced', 'rx_bytes_path', _('RX bytes path'), null, '');
	optionalValue(section, 'advanced', 'tx_bytes_path', _('TX bytes path'), null, '');
}

function addSqmOptions(section, qdiscs, scripts) {
	var o, seen;

	o = flag(section, 'sqm_basic', 'manage_sqm', _('Manage SQM'), '1');
	o.validate = function(section_id) {
		return validateSqmSectionUnique(validationSection(this), section_id);
	};
	o.write = function(section_id, formvalue) {
		uci.set('cake-autorate', section_id, 'manage_sqm', formvalue);

		if (formvalue === '1')
			syncManagedSqmEnabled(this.section, section_id);
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
	handleSaveApply: function(ev, mode) {
		var markers;
		try {
			markers = pendingAutotuneApplyMarkers();
		}
		catch (error) {
			return Promise.reject(error);
		}
		if (!markers.length)
			return this.super('handleSaveApply', [ ev, mode ]);
		if (markers.length !== 1)
			return Promise.reject(new Error(_('Apply one Full Auto-Tune proposal at a time.')));
		return runGuardedSaveApply(this, ev);
	},

	load: function() {
		return Promise.all([
			network.getDevices(),
			network.getNetworks(),
			L.resolveDefault(fs.list('/var/run/sqm/available_qdiscs'), []),
			loadSqmScripts(),
			uci.load('cake-autorate'),
			L.resolveDefault(uci.load('sqm'), null),
			L.resolveDefault(uci.load('mwan3'), null),
			L.resolveDefault(fs.exec('/usr/libexec/cake-autorate-rs/mwan3-info', []).then(function(result) {
				return JSON.parse(result.stdout || '{}');
			}), {})
		]);
	},

	render: function(data) {
		cakeUi.ensureAppHeader();
		var m, s;
		var qdiscs = data[2];
		var scripts = data[3];

		interfaceContext = buildInterfaceContext(data[0], data[1]);
		mwan3Context = buildMwan3Context();
		mwan3Capability = data[7] || {};

		m = new form.Map('cake-autorate', _('CAKE Autorate'));
		s = m.section(form.GridSection, 'cake_autorate', _('Instances'));
		s.anonymous = false;
		s.addremove = true;
		s.addbtntitle = _('Create instance');
		s.nodescriptions = true;
		s.handleAdd = function(ev, name) {
			showCreateWizard(this, name);
		};
		var renderDefaultRowActions = s.renderRowActions;
		s.renderRowActions = function(section_id) {
			var actions = renderDefaultRowActions.call(this, section_id);
			var container = actions && actions.lastElementChild;
			if (container) {
				container.insertBefore(E('button', {
					'title': _('Re-run Auto-Tune'),
					'class': 'btn cbi-button cbi-button-action cake-autotune-rerun',
					'click': ui.createHandlerFn(this, function() {
						showCreateWizard(this, section_id, section_id);
					})
				}, _('Re-run Auto-Tune')), container.firstChild);
			}
			return actions;
		};
		s.addModalOptions = function(modalSection, section_id) {
			var parse = modalSection.parse;
			var renderTabContainers = modalSection.renderTabContainers;

			modalSection.renderTabContainers = function(renderSectionId, nodes) {
				var containers = renderTabContainers.call(this, renderSectionId, nodes);
				return decorateAutorateSubcategories(this, renderSectionId, containers);
			};

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

		s.tab('autorate', _('Autorate setup'));
		s.tab('sqm', _('SQM setup'));
		s.tab('testing', _('Testing & Auto-Tune'));
		s.tab('monitoring', _('Monitoring'));
		s.tab('advanced', _('Advanced'));
		var originalTabOption = s.taboption;
		s.taboption = function(tab) {
			var args = Array.prototype.slice.call(arguments);
			var logicalTab = tab;
			args[0] = topicTab(logicalTab);
			var option = originalTabOption.apply(this, args);
			if (args[0] === 'autorate')
				option.cakeAutorateGroup = autorateSubcategory(logicalTab, option.option);
			return option;
		};

		addTopicIntroduction(s, 'autorate', '_autorate_topic',
			_('Choose the uplink and route, then tune autorate limits, adaptive ceiling, latency signals, reflectors, quality and controller behavior.'));
		addTopicIntroduction(s, 'sqm', '_sqm_topic',
			_('Configure the managed SQM interface, CAKE queue, link-layer overhead and PPPoE/Ethernet details.'));
		addTopicIntroduction(s, 'testing', '_testing_topic',
			_('Run speed tests and Full Auto-Tune, select test backends, and control optional scheduled recalibration.'));
		addTopicIntroduction(s, 'monitoring', '_monitoring_topic',
			_('Configure RAM-only graph sampling, logging, MQTT and diagnostic export behavior. Graph memory limits remain on the Graphs page.'));
		addTopicIntroduction(s, 'advanced', '_advanced_topic',
			_('Low-level timing, recovery and compatibility controls. Change these only when diagnosing a specific problem.'));

		flag(s, 'general', 'adjust_dl_shaper_rate', _('Adjust DL'));
		flag(s, 'general', 'adjust_ul_shaper_rate', _('Adjust UL'));

		addSetupOptions(s);
		addInterfaceOptions(s);
		addSqmOptions(s, qdiscs, scripts);
		addRateOptions(s);
		addQualityOptions(s);
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
