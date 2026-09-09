'use strict';
'require fs';
'require form';
'require network';
'require rpc';
'require uci';
'require ui';
'require tools.widgets as widgets';
'require cake-autorate-rs.ui as cakeUi';

var AUTOTUNE_PROFILE_SEARCH_SCHEMA_VERSION = 4;

function modal(option) {
	option.modalonly = true;
	/* LuCI removes values of dependency-hidden options during parse unless
	 * `retain` is set.  On this large modal that used to turn an innocent Edit
	 * into dozens of unrelated deletions (disabled transport/MQTT/scheduler,
	 * inactive speed-test backends, collapsed SQM expert settings, and so on).
	 * Preserve dormant values; explicit parent writes still clear genuinely
	 * incompatible state such as mwan3_member when route_mode becomes main. */
	option.retain = true;
	return option;
}

function settingsActionLayout() {
	// LuCI writes a desktop-sized inline width for the action column. Override
	// it only in this map on narrow viewports and keep every action reachable.
	return E('style', {}, '@media(max-width:600px){' +
		'#cbi-cake-autorate .cbi-section-actions{min-width:0!important;width:100%!important;max-width:100%;white-space:normal}' +
		'#cbi-cake-autorate .cbi-section-actions>div{flex-wrap:wrap;gap:4px}' +
		'#cbi-cake-autorate .cbi-section-actions .cbi-button{flex:1 1 auto;min-width:0;max-width:100%;white-space:normal}' +
		'}');
}

function trafficPrioritiesUrl(sectionId) {
	if (!/^[A-Za-z0-9_]+$/.test(sectionId || ''))
		throw new TypeError(_('The instance name is unsafe.'));
	return L.url('admin/network/cake-autorate-rs/settings/priorities') +
		'?instance=' + encodeURIComponent(sectionId);
}

var optionDescriptions = {
	enabled: 'Start autorate and its managed SQM queue together for this instance.',
	adjust_dl_shaper_rate: 'Allow autorate to change the download CAKE bandwidth.',
	adjust_ul_shaper_rate: 'Allow autorate to change the upload CAKE bandwidth.',
	wan_if: 'Main WAN interface for this instance. Auto preset also uses it for SQM and IFB setup.',
	route_mode: 'Select the main routing table or force every ICMP, HTTP, speed test, and Auto-Tune probe through one mwan3 member.',
	mwan3_member: 'Logical mwan3 interface/member used for this uplink. Its resolved L3 device must match the target interface.',
	route_check_interval_s: 'Interval for checking mwan3 state, L3 device, source address, fwmark, and route identity.',
	auto_interface_preset: 'Automatically derive SQM interface, upload interface, and download IFB from the target interface.',
	sqm_download: 'SQM download bandwidth in kbit/s. This also seeds the autorate base and max download rates.',
	sqm_upload: 'SQM upload bandwidth in kbit/s. This also seeds the autorate base and max upload rates.',
	speedtest_apply_percent: 'Percentage of measured throughput to write into SQM and autorate limits. 90 leaves headroom for CAKE.',
	_speedtest: 'Run a router-side speed test and fill SQM plus autorate limits from the measured throughput.',
	speedtest_backend: 'Auto and speedtest-go use the same native, route-bound measurement path.',
	speedtest_go_server_id: 'Optional speedtest-go server ID. Leave empty to automatically validate nearby servers and reuse the first good one; set an ID to pin a known-good server.',
	_wizard_sqm_queue: 'Existing unmanaged SQM queues on the selected interface are reused to avoid duplicate shapers.',
	_wizard_advanced_test_options: 'Show native backend selection, speed test headroom, and reflector planning. Auto defaults are suitable for normal setup.',
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
	access_medium_selection: 'How Variable Link identifies the provider-facing access medium. Auto is intentionally conservative: Ethernet and PPPoE alone do not prove fibre, cellular, satellite, or shared wireless service.',
	capacity_learning_policy: 'Choose whether runtime stays at validated bounds, learns from real sustained traffic, schedules traffic-generating recalibration, or obeys explicit service caps.',
	service_dl_cap_kbps: 'Optional provider/service-plan download hard cap. It can only tighten a measured bound and is never used to invent capacity above a raw control.',
	service_ul_cap_kbps: 'Optional provider/service-plan upload hard cap. It can only tighten a measured bound and is never used to invent capacity above a raw control.',
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
	autotune_profile: 'Profile used by the next manual or scheduled Full Auto-Tune run. Gaming targets A+ and keeps a 70% search floor; its wizard-only Extreme A+ opt-in can explore wide links deeper but is never persisted for scheduled runs. Best overall targets A, Variable link measures a CAKE-controlled latency knee for changing links, and Fair prioritizes throughput with a conditional class-C target and an explicit evidence-backed no-SQM fallback.',
	autotune_calibration_strategy: 'Shaped only keeps CAKE active and searches inside demonstrated bounds. Full raw capacity temporarily bypasses only the measured direction under the recovery watchdog. Reuse current trusted bounds avoids claiming a new physical line rate.',
	scheduled_autotune_enabled: 'Periodically run the validated Full Auto-Tune workflow only inside the configured quiet window. Disabled by default.',
	scheduled_autotune_interval_hours: 'Minimum hours between successful scheduled calibrations.',
	scheduled_autotune_idle_window_s: 'Traffic must remain below the active threshold for this long before a scheduled test may start.',
	scheduled_autotune_window_start_hour: 'Local hour when the permitted maintenance window begins (0-23).',
	scheduled_autotune_window_end_hour: 'Local hour when the permitted maintenance window ends (0-23). Equal start and end permits the whole day.',
	scheduled_autotune_max_traffic_mb_day: 'Hard daily interface-traffic allowance for scheduled calibration. The worker stops its current speed-test process when the remaining allowance is reached; accounting resets after reboot. A zero or invalid value blocks unattended tests rather than enabling unlimited traffic.',
	scheduled_autotune_max_traffic_mb_month: 'Hard monthly allowance for scheduled calibration. Usage is reserved before a run and settled atomically outside RAM so interrupted tests cannot be silently undercounted. Allow roughly one polling interval of possible overshoot on a very fast link; zero blocks unattended tests.',
	scheduled_autotune_auto_apply: 'Automatically commit and restart with a proposal only after shaped validation passes. Leave off to keep a review-only proposal.',
	dl_if: 'Interface whose RX byte counter represents shaped download traffic, usually the IFB created by SQM.',
	ul_if: 'Interface whose TX byte counter represents upload traffic, usually the WAN device.',
	manage_sqm: 'Mirror this instance into /etc/config/sqm and restart SQM before autorate starts.',
	sqm_section: 'Name of the managed SQM queue section. Leave empty to use cake_<instance>.',
	sqm_direction_mode: 'Choose which traffic directions receive managed CAKE. Upload only removes download/ingress CAKE and its IFB; Download only removes upload/egress CAKE. The unshaped direction has no local bufferbloat protection. Its Adjust switch is cleared; if you restore that CAKE direction later, choose separately whether Autorate may adjust it.',
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
	mqtt_enabled: 'Start the native MQTT telemetry publisher for this instance. It reads bounded daemon log records and publishes Home Assistant discovery, state, and availability directly.',
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
	_mqtt_status: 'Check whether the native publisher and required saved log settings are ready for this instance. Save pending MQTT edits before relying on this status.',
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
	deviceTypes: {},
	deviceProtocols: {},
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
		deviceTypes: {},
		deviceProtocols: {},
		networkDevices: {},
		defaultDevice: null
	};

	for (var i = 0; i < devices.length; i++) {
		var devName = devices[i].getName();
		var devType = devices[i].getType();

		if (!devName || devName === 'lo' || devType === 'alias')
			continue;

		ctx.deviceNames[devName] = true;
		ctx.deviceTypes[devName] = devType || '';

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
		var protocol = networks[i].getProtocol ? networks[i].getProtocol() : '';

		if (!netName || !ifName)
			continue;

		if (ifName.charAt(0) === '@')
			ifName = ifName.substring(1);

		ctx.networkDevices[netName] = ifName;
		if (!ctx.deviceProtocols[ifName])
			ctx.deviceProtocols[ifName] = [];
		if (protocol && ctx.deviceProtocols[ifName].indexOf(protocol) < 0)
			ctx.deviceProtocols[ifName].push(protocol);
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

function normalizeInterfaceNameWithContext(name, context, seen) {
	var mapped;

	if (!name)
		return name;

	if (name.charAt(0) === '@')
		name = name.substring(1);

	context = context || interfaceContext;
	seen = seen || {};
	if (seen[name])
		return name;
	seen[name] = true;
	mapped = context.networkDevices && context.networkDevices[name];
	if (mapped && mapped !== name)
		return normalizeInterfaceNameWithContext(mapped, context, seen);

	return name;
}

function normalizeInterfaceName(name) {
	return normalizeInterfaceNameWithContext(name, interfaceContext);
}

function defaultTargetInterface() {
	return normalizeInterfaceName(interfaceContext.defaultDevice || 'wan');
}

function accessMediumDefinitions() {
	return [
		[ 'auto', _('Auto (conservative when uncertain)') ],
		[ 'cellular', _('4G / 5G cellular') ],
		[ 'leo_satellite', _('LEO satellite') ],
		[ 'geo_satellite', _('GEO / high-latency satellite') ],
		[ 'fixed_wireless', _('WISP / fixed wireless / Wi-Fi bridge') ],
		[ 'shared_wired', _('Shared wired access') ],
		[ 'unknown', _('Other / unknown') ]
	];
}

function accessMediumTitle(medium) {
	var definitions = accessMediumDefinitions();
	for (var i = 0; i < definitions.length; i++)
		if (definitions[i][0] === medium)
			return definitions[i][1];
	return _('Other / unknown');
}

function accessMediumExplorationPercent(medium) {
	switch (medium) {
	case 'cellular':
	case 'leo_satellite':
		return 35;
	case 'geo_satellite':
	case 'fixed_wireless':
		return 40;
	case 'shared_wired':
	case 'unknown':
	default:
		return 50;
	}
}

function detectAccessMedium(device, context) {
	context = context || interfaceContext;
	device = normalizeInterfaceNameWithContext(device || '', context);
	var protocols = context.deviceProtocols && context.deviceProtocols[device] || [];
	var physicalDevice = context.devicePhysical && context.devicePhysical[device] || '';
	var deviceType = context.deviceTypes ?
		[ context.deviceTypes[device] || '', context.deviceTypes[physicalDevice] || '' ].join(' ') : '';
	var joinedProtocols = protocols.join(' ').toLowerCase();
	var lowerDevice = String(device || '').toLowerCase();

	/* Direct modem protocols are strong evidence. PPPoE, DHCP and a physical
	 * Ethernet carrier deliberately are not: all of them can sit in front of a
	 * cellular modem, satellite terminal, WISP CPE, or ordinary wired service. */
	if (/(^|\s)(qmi|mbim|ncm|3g|4g|modemmanager)(\s|$)/.test(joinedProtocols))
		return { medium: 'cellular', source: 'network_protocol', confidence_percent: 95,
			reason: _('A direct cellular modem protocol was found for this interface.') };
	if (/^(wwan|rmnet|wwp|qmi|mbim|modem|cell)/.test(lowerDevice))
		return { medium: 'cellular', source: 'interface_name', confidence_percent: 70,
			reason: _('The interface name strongly resembles a cellular modem, but should still be reviewed.') };
	if (/wifi|wireless|802\.11/i.test(deviceType))
		return { medium: 'fixed_wireless', source: 'device_type', confidence_percent: 65,
			reason: _('The selected WAN device is wireless; Auto cannot distinguish WISP from another Wi-Fi bridge.') };

	return { medium: 'unknown', source: 'auto_inconclusive', confidence_percent: 20,
		reason: _('No trustworthy physical-medium signal was found. Ethernet, DHCP and PPPoE are transport details, not proof of the provider medium.') };
}

function resolvedAccessContext(state, context) {
	var selection = state && state.access_medium_selection || 'auto';
	if (selection !== 'auto') {
		return {
			medium: accessMediumDefinitions().some(function(item) { return item[0] === selection; }) ?
				selection : 'unknown',
			source: 'user_selected',
			confidence_percent: 100,
			reason: _('Selected explicitly by the user.')
		};
	}
	return detectAccessMedium(state && state.wan_if, context);
}

function recommendedCapacityLearningPolicy(access) {
	if (!access || access.medium === 'unknown' || access.confidence_percent < 50)
		return 'verified_only';
	return 'passive_bounded';
}

function canonicalCapacityLearningPolicy(value) {
	switch (value) {
	case 'verified_only':
		return 'verified_only';
	case 'passive':
	case 'passive_bounded':
		return 'passive_bounded';
	case 'periodic_active':
	case 'scheduled_active':
		return 'scheduled_active';
	case 'fixed':
	case 'fixed_cap':
		return 'fixed_cap';
	default:
		return null;
	}
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

function managedUplinkOwner(member, ignoredInstance) {
	var device = member && normalizeInterfaceName(member.device);
	var existing = uci.sections('cake-autorate', 'cake_autorate') || [];

	for (var i = 0; i < existing.length; i++) {
		var section = existing[i];
		var name = section['.name'];
		if (!name || name === ignoredInstance || section.manage_sqm === '0')
			continue;
		var target = normalizeInterfaceName(section.sqm_interface || section.ul_if || section.wan_if);
		if ((section.mwan3_member && section.mwan3_member === member.name) ||
		    (device && target === device))
			return name;
	}

	return '';
}

function managedTargetOwner(device, ignoredInstance) {
	device = normalizeInterfaceName(device);
	var existing = uci.sections('cake-autorate', 'cake_autorate') || [];

	for (var i = 0; i < existing.length; i++) {
		var section = existing[i];
		var name = section['.name'];
		if (!name || name === ignoredInstance || section.manage_sqm === '0')
			continue;
		var target = normalizeInterfaceName(section.sqm_interface || section.ul_if || section.wan_if);
		if (device && target === device)
			return name;
	}

	return '';
}

function availableMwan3Uplinks(ignoredInstance) {
	return uniqueMwan3Uplinks().filter(function(member) {
		return !managedUplinkOwner(member, ignoredInstance);
	});
}

function wizardRouteChoices(ignoredInstance) {
	var choices = [ [ 'main', _('Main routing table') ] ];

	for (var i = 0; i < mwan3Context.members.length; i++) {
		var member = mwan3Context.members[i];
		if (managedUplinkOwner(member, ignoredInstance))
			continue;
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

function multiwanInstancePlans(state, ignoredInstance) {
	var uplinks = availableMwan3Uplinks(ignoredInstance);
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
			if (section.manage_sqm !== '0' && existingTarget === plan.device)
				conflicts.push(_('Instance "%s" already manages a CAKE queue on %s.').format(existingName, existingTarget));
		}
	}

	return conflicts.filter(function(message, index, all) {
		return all.indexOf(message) === index;
	});
}

function wizardSingleTargetConflicts(state, ignoredInstance) {
	if (!state || !state.wan_if)
		return [];
	return wizardPlanConflicts([ {
		name: state.name || '',
		device: normalizeInterfaceName(state.wan_if)
	} ], state.enabled, ignoredInstance);
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

	// cfgvalue() runs before form.Map.render() assigns map.root. Calling
	// getUIElement() in that phase makes LuCI form.js dereference an undefined
	// root through findElement(). Fall back to staged/UCI values until the map
	// has a live DOM root.
	if (section && section.map && section.map.root &&
	    typeof section.getUIElement == 'function') {
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
	var dlMax, ulMax, dlCap, ulCap, serviceDlCap, serviceUlCap;
	var learningPolicy = canonicalCapacityLearningPolicy(
		formOrUci(section, section_id, 'capacity_learning_policy'));

	if (!learningPolicy)
		return _('Select a current runtime capacity learning policy.');

	dlMax = adaptiveConfiguredMax(section, section_id, 'dl');
	ulMax = adaptiveConfiguredMax(section, section_id, 'ul');

	if (learningPolicy === 'fixed_cap') {
		serviceDlCap = parsePositiveRate(formOrUci(section, section_id, 'service_dl_cap_kbps'));
		serviceUlCap = parsePositiveRate(formOrUci(section, section_id, 'service_ul_cap_kbps'));

		if (serviceDlCap == null || serviceDlCap <= 0)
			return _('A positive download service hard cap is required for explicit fixed-cap learning.');

		if (serviceUlCap == null || serviceUlCap <= 0)
			return _('A positive upload service hard cap is required for explicit fixed-cap learning.');

		if (dlMax != null && serviceDlCap < dlMax)
			return _('Download service hard cap must be at least the configured maximum (%d kbit/s).').format(dlMax);

		if (ulMax != null && serviceUlCap < ulMax)
			return _('Upload service hard cap must be at least the configured maximum (%d kbit/s).').format(ulMax);

		return true;
	}

	if (learningPolicy === 'verified_only')
		return true;

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

function positiveRateValue(value) {
	var parsed = parseInt(value, 10);

	return !isNaN(parsed) && parsed > 0 ? String(value) : null;
}

function shouldImportInterfaceRates(previous, next, dl, ul) {
	return normalizeInterfaceName(previous) !== normalizeInterfaceName(next) ||
		!positiveRateValue(dl) || !positiveRateValue(ul);
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
	var currentDl = positiveRateValue(uci.get('cake-autorate', section_id, 'sqm_download'));
	var currentUl = positiveRateValue(uci.get('cake-autorate', section_id, 'sqm_upload'));
	var dl = positiveRateValue(queue ? queue.download : null) || currentDl ||
		positiveRateValue(uci.get('cake-autorate', section_id, 'base_dl_shaper_rate_kbps')) || '20000';
	var ul = positiveRateValue(queue ? queue.upload : null) || currentUl ||
		positiveRateValue(uci.get('cake-autorate', section_id, 'base_ul_shaper_rate_kbps')) || '20000';

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
		[ 'speedtest-go', _('speedtest-go (package: speedtest-go)') ]
	];
}

function speedtestBackendChoiceTitle(value) {
	var choices = speedtestBackendChoices();

	for (var i = 0; i < choices.length; i++)
		if (choices[i][0] === value)
			return choices[i][1];

	return value || _('Auto');
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

function parseExecJson(res) {
	res = res || {};
	var stdout = String(res.stdout || '').trim();
	var stderr = String(res.stderr || '').split(/\r?\n/, 1)[0]
		.replace(/[\x00-\x1f\x7f]+/g, ' ').replace(/\s+/g, ' ').trim()
		.replace(/^ERROR:\s*/i, '');
	if (stderr.length > 240)
		stderr = stderr.substring(0, 237) + '...';
	var code = res.code == null ? 0 : Number(res.code);
	var failed = !isFinite(code) || code !== 0;

	if (failed)
		throw new Error(stderr || _('The calibration service failed without a usable diagnostic.'));
	if (!stdout)
		throw new Error(stderr || _('The calibration service returned no JSON result.'));

	try {
		return JSON.parse(stdout);
	} catch (error) {
		throw new Error(stderr || _('The calibration service returned malformed JSON.'));
	}
}

function withRpcTimeout(minimum, callback) {
	var rpcEnv = L.env || (L.env = {});
	var previous = rpcEnv.rpctimeout;
	var timeout = parseInt(previous, 10);

	if (isNaN(timeout) || timeout < minimum)
		rpcEnv.rpctimeout = minimum;

	return Promise.resolve().then(callback).then(function(result) {
		rpcEnv.rpctimeout = previous;
		return result;
	}, function(err) {
		rpcEnv.rpctimeout = previous;
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
/* Highest public result schema advertised by the native coordinator. */
var NATIVE_AUTOTUNE_PUBLIC_SCHEMA_VERSION = 6;
var NATIVE_AUTOTUNE_APPLY_CONTRACT_SCHEMA_VERSION = 3;
var NATIVE_AUTOTUNE_PUBLIC_PRODUCER = 'cake-autorated-native-autotune';
var NATIVE_AUTOTUNE_COMMAND = '/usr/sbin/cake-autorated';
var NATIVE_AUTOTUNE_PROTOCOL_VERSION = 2;
var NATIVE_AUTOTUNE_INTERACTIVE_TRAFFIC_BUDGET_BYTES = 32000000000;
var NATIVE_AUTOTUNE_APPLY_MAX_WATCH_RESPONSES = 72;
var NATIVE_AUTOTUNE_APPLY_RPC_RETRIES = 3;
var nativeAutotuneJobs = {};
var NATIVE_AUTOTUNE_ACKNOWLEDGEMENT_CODES = {
	'download-candidate-realization': true,
	'upload-candidate-realization': true,
	'download-capacity-retention': true,
	'upload-capacity-retention': true,
	'download-throughput-safety-floor': true,
	'upload-throughput-safety-floor': true,
	'download-physical-capacity-limited': true,
	'upload-physical-capacity-limited': true,
	'download-icmp-latency': true,
	'download-transport-latency': true,
	'upload-icmp-latency': true,
	'upload-transport-latency': true,
	'measurement-contaminated': true,
	'measurement-confidence': true,
	'download-raw-contaminated': true,
	'download-raw-confidence': true,
	'download-raw-quality-target': true,
	'upload-raw-contaminated': true,
	'upload-raw-confidence': true,
	'upload-raw-quality-target': true,
	'topology-comparison-traffic-budget': true,
	'topology-comparison-unmeasurable': true,
	'download-shaping-bypassed': true,
	'upload-shaping-bypassed': true,
	'sqm-disabled': true,
	'loaded-latency-unobservable': true,
	'shaped-validation-incomplete': true
};

function nativeAutotuneTimeoutEvidenceValidated(evidence, maximumCount) {
	var count = evidence && evidence.transport_timeout_count;
	var total = evidence && evidence.transport_timeout_total_us;
	var censored = evidence && evidence.transport_censored;

	if (!evidence || typeof censored !== 'boolean' ||
	    !Number.isSafeInteger(count) || count < 0 || count > maximumCount ||
	    !Number.isSafeInteger(total) || total < 0 || total > count * 60000000)
		return false;
	if (!censored)
		return count === 0 && total === 0;
	return count >= 3 && total >= 15000000;
}

function nativeAutotuneSearchArtifactValidated(search, direction, profile) {
	var selected = search && search.selected;
	var evaluated = search && search.evaluated;
	var options = search && search.review_options;
	var recommended = Array.isArray(options) ? options[0] : null;
	var selectedObservation;

	if (!search || search.schema_version !== 4 || search.direction !== direction ||
	    canonicalAutotuneProfile(search.profile) !== profile || !selected ||
	    typeof selected.transport_censored !== 'boolean' ||
	    !Number.isSafeInteger(selected.index) || selected.index < 1 ||
	    !Array.isArray(evaluated) || evaluated.length < 1 || evaluated.length > 64 ||
	    !Array.isArray(options) || options.length < 1 || options.length > 3 ||
	    !recommended || recommended.role !== 'recommended' ||
	    recommended.observation_index !== selected.index ||
	    recommended.candidate_kbps !== selected.candidate_kbps ||
	    recommended.transport_censored !== selected.transport_censored)
		return false;

	for (var i = 0; i < evaluated.length; i++) {
		var observation = evaluated[i];
		if (!observation || !Number.isSafeInteger(observation.index) ||
		    observation.index !== i + 1 ||
		    typeof observation.transport_censored !== 'boolean' ||
		    typeof observation.measurement_reliable !== 'boolean' ||
		    typeof observation.manual_reviewable !== 'boolean' ||
		    typeof observation.safety_pass !== 'boolean' ||
		    typeof observation.target_met !== 'boolean')
			return false;
		if (observation.transport_censored &&
		    (observation.measurement_reliable !== false ||
		     observation.safety_pass !== false || observation.target_met !== false))
			return false;
		if (observation.index === selected.index)
			selectedObservation = observation;
	}
	if (!selectedObservation ||
	    selectedObservation.candidate_kbps !== selected.candidate_kbps ||
	    selectedObservation.transport_censored !== selected.transport_censored)
		return false;

	for (var o = 0; o < options.length; o++) {
		var option = options[o];
		if (!option || typeof option.transport_censored !== 'boolean' ||
		    !Number.isSafeInteger(option.observation_index) || option.observation_index < 1 ||
		    option.observation_index > evaluated.length)
			return false;
		if (option.transport_censored &&
		    (option.manual_reviewable !== true || option.target_met !== false ||
		     option.auto_apply_candidate !== false))
			return false;
	}

	if (selected.transport_censored) {
		if (search.action !== 'fallback' ||
		    search.reason !== 'transport-deadline-censored-review' ||
		    selected.manual_reviewable !== true || selected.safety_pass !== false ||
		    selected.target_met !== false || recommended.auto_apply_candidate !== false)
			return false;
	}
	else if (search.reason === 'transport-deadline-censored-review') {
		return false;
	}

	return true;
}

function nativeAutotunePairTransportValidated(pair) {
	var options = pair && pair.options;
	var unavailable = pair && pair.unavailable_options;
	var seenRates = Object.create(null);
	var directionalFlagsValidated = function(value) {
		return value && typeof value === 'object' && !Array.isArray(value) &&
			Object.keys(value).sort().join(',') === 'download,upload' &&
			typeof value.download === 'boolean' && typeof value.upload === 'boolean';
	};

	if (!pair || pair.schema_version !== 6 ||
	    !nativeAutotuneTimeoutEvidenceValidated(pair, 64) ||
	    typeof pair.measurement_reliable !== 'boolean' ||
	    typeof pair.safety_pass !== 'boolean' || typeof pair.auto_apply_pass !== 'boolean' ||
	    !Array.isArray(options) || options.length < 1 || options.length > 3 ||
	    !Array.isArray(unavailable) || options.length + unavailable.length > 3)
		return false;
	if (pair.transport_censored &&
	    (pair.measurement_reliable !== false || pair.safety_pass !== false ||
	     pair.auto_apply_pass !== false))
		return false;

	for (var i = 0; i < options.length; i++) {
		var option = options[i];
		var optionRates = option && option.target_rates_kbps;
		if (!nativeAutotuneTimeoutEvidenceValidated(option, 64) ||
		    !optionRates || !Number.isSafeInteger(optionRates.download) ||
		    optionRates.download < 100 || !Number.isSafeInteger(optionRates.upload) ||
		    optionRates.upload < 100 || seenRates[optionRates.download + ':' + optionRates.upload] ||
		    typeof option.measurement_reliable !== 'boolean' ||
		    typeof option.safety_pass !== 'boolean' ||
		    typeof option.auto_apply_pass !== 'boolean' ||
		    typeof option.manual_apply_eligible !== 'boolean' ||
		    typeof option.manual_review_required !== 'boolean' ||
		    !directionalFlagsValidated(option.physical_capacity_limited_review) ||
		    !directionalFlagsValidated(option.physical_capacity_alignment_confirmed) ||
		    (option.physical_capacity_alignment_confirmed.download &&
		     !option.physical_capacity_limited_review.download) ||
		    (option.physical_capacity_alignment_confirmed.upload &&
		     !option.physical_capacity_limited_review.upload) ||
		    ((option.physical_capacity_limited_review.download ||
		      option.physical_capacity_limited_review.upload) &&
		     (option.auto_apply_pass !== false || option.manual_apply_eligible !== true ||
		      option.manual_review_required !== true)))
			return false;
		if (option.transport_censored &&
		    (option.measurement_reliable !== false || option.safety_pass !== false ||
		     option.auto_apply_pass !== false || option.manual_apply_eligible !== true ||
		     option.manual_review_required !== true))
			return false;
		seenRates[optionRates.download + ':' + optionRates.upload] = true;
	}
	for (var u = 0; u < unavailable.length; u++) {
		var missing = unavailable[u];
		var missingFields = [ 'target_rates_kbps', 'failed_direction', 'reason',
			'run_count', 'traffic_debit_count', 'samples' ];
		var rates = missing && missing.target_rates_kbps;
		var debits = missing && missing.traffic_debit_count;
		var samples = missing && missing.samples;
		var rateKey = rates && rates.download + ':' + rates.upload;
		if (!missing || typeof missing !== 'object' || Array.isArray(missing) ||
		    Object.keys(missing).sort().join(',') !== missingFields.slice().sort().join(',') ||
		    !rates || Object.keys(rates).sort().join(',') !== 'download,upload' ||
		    !Number.isSafeInteger(rates.download) || rates.download < 100 ||
		    !Number.isSafeInteger(rates.upload) || rates.upload < 100 || seenRates[rateKey] ||
		    (missing.failed_direction !== 'download' && missing.failed_direction !== 'upload') ||
		    (missing.reason !== 'observation_starved' &&
		     missing.reason !== 'transfer_unmeasurable') ||
		    !Number.isSafeInteger(missing.run_count) || missing.run_count < 1 ||
		    missing.run_count > 3 || !debits ||
		    Object.keys(debits).sort().join(',') !== 'download,upload' ||
		    !Number.isSafeInteger(debits.download) || debits.download < 1 ||
		    !Number.isSafeInteger(debits.upload) || debits.upload < 0 ||
		    (missing.failed_direction === 'download' &&
		     (debits.download < missing.run_count || debits.upload !== 0)) ||
		    (missing.failed_direction === 'upload' &&
		     (debits.upload < missing.run_count || debits.download < 1)) ||
		    !samples || Object.keys(samples).sort().join(',') !== 'cpu,icmp,transport' ||
		    !Number.isSafeInteger(samples.icmp) || samples.icmp < 0 ||
		    !Number.isSafeInteger(samples.transport) || samples.transport < 0 ||
		    !Number.isSafeInteger(samples.cpu) || samples.cpu < 0 ||
		    (missing.reason === 'observation_starved' &&
		     samples.icmp >= 15 && samples.transport >= 15 && samples.cpu >= 1) ||
		    (missing.reason === 'transfer_unmeasurable' &&
		     (missing.run_count > 2 || samples.icmp !== 0 ||
		      samples.transport !== 0 || samples.cpu !== 0)))
			return false;
		seenRates[rateKey] = true;
	}
	return pair.measurement_reliable === options[0].measurement_reliable &&
		pair.safety_pass === options[0].safety_pass &&
		pair.auto_apply_pass === options[0].auto_apply_pass;
}

function nativeAutotuneTopologyTransportValidated(topology) {
	var directions = [ 'download', 'upload' ];
	var selectedCensored = false;
	var trafficBudgetLimited = false;

	if (!topology || topology.schema_version !== 2 ||
	    typeof topology.auto_apply_pass !== 'boolean' ||
	    typeof topology.manual_review_required !== 'boolean')
		return false;
	for (var i = 0; i < directions.length; i++) {
		var result = topology[directions[i]];
		var shaped = result && result.shaped;
		var unshaped = result && result.unshaped;
		if (!result || (result.choice !== 'shaped' && result.choice !== 'unshaped') ||
		    !shaped || typeof shaped.transport_censored !== 'boolean' ||
		    !unshaped || typeof unshaped.transport_censored !== 'boolean')
			return false;
		if (unshaped.transport_censored &&
		    (result.choice === 'unshaped' || unshaped.target_met !== false ||
		     unshaped.measurement_reliable !== false || unshaped.safety_pass !== false))
			return false;
		if (result.reason === 'traffic-budget-limited') {
			if (result.choice !== 'shaped' || unshaped.needs_repeat !== false ||
			    unshaped.retry_exhausted !== true)
				return false;
			trafficBudgetLimited = true;
		}
		selectedCensored = selectedCensored ||
			(result.choice === 'shaped' ? shaped.transport_censored : unshaped.transport_censored);
	}
	if (trafficBudgetLimited)
		return topology.auto_apply_pass === false && topology.manual_review_required === true;
	return !selectedCensored ||
		(topology.auto_apply_pass === false && topology.manual_review_required === true);
}

function nativeAutotuneRawFallbackValidated(raw, option, directionalOption) {
	if (raw && raw.schema_version === 6) {
		var fields = [ 'schema_version', 'selected_topology', 'reason',
			'failed_direction', 'terminal_boundary', 'selected_rates_kbps',
			'raw_capacity_kbps', 'latency_grade', 'auto_apply_pass',
			'manual_review_required', 'adaptive_ceiling', 'required_acknowledgements' ];
		var boundary = raw.terminal_boundary;
		var selected = raw.selected_rates_kbps;
		var capacities = raw.raw_capacity_kbps;
		var ceiling = raw.adaptive_ceiling;
		var exactAcknowledgements = [
			'loaded-latency-unobservable', 'shaped-validation-incomplete' ];
		var direction = function(value) {
			return value && typeof value === 'object' && !Array.isArray(value) &&
				Object.keys(value).sort().join(',') === 'cap_kbps,evidence,safe_kbps' &&
				value.safe_kbps === 0 && Number.isSafeInteger(value.cap_kbps) &&
				value.cap_kbps >= 100 && value.evidence === 'legacy_unverified';
		};
		return Object.keys(raw).sort().join(',') === fields.sort().join(',') &&
			raw.selected_topology === 'both_shaped' &&
			raw.reason === 'loaded-latency-unobservable' &&
			(raw.failed_direction === 'download' || raw.failed_direction === 'upload') &&
			boundary && typeof boundary === 'object' && !Array.isArray(boundary) &&
			Object.keys(boundary).sort().join(',') === 'candidate_kbps,kind' &&
			[ 'unobserved_floor_exhaustion', 'loaded_observation_starved',
				'candidate_transfer_unmeasurable' ].indexOf(boundary.kind) >= 0 &&
			Number.isSafeInteger(boundary.candidate_kbps) && boundary.candidate_kbps >= 100 &&
			selected && typeof selected === 'object' && !Array.isArray(selected) &&
			Object.keys(selected).sort().join(',') === 'download,upload' &&
			Number.isSafeInteger(selected.download) && selected.download >= 100 &&
			Number.isSafeInteger(selected.upload) && selected.upload >= 100 &&
			boundary.candidate_kbps === selected[raw.failed_direction] &&
			capacities && typeof capacities === 'object' && !Array.isArray(capacities) &&
			Object.keys(capacities).sort().join(',') === 'download,upload' &&
			[ capacities.download, capacities.upload ].every(function(values) {
				return Array.isArray(values) && values.length === 2 && values.every(function(value) {
					return Number.isSafeInteger(value) && value >= 1;
				});
			}) && raw.latency_grade === null && raw.auto_apply_pass === false &&
			raw.manual_review_required === true && ceiling &&
			Object.keys(ceiling).sort().join(',') === 'download,upload' &&
			direction(ceiling.download) && direction(ceiling.upload) &&
			ceiling.download.cap_kbps === selected.download &&
			ceiling.upload.cap_kbps === selected.upload &&
			Array.isArray(raw.required_acknowledgements) &&
			raw.required_acknowledgements.join('\n') === exactAcknowledgements.join('\n') &&
			option && option.option_id === 'capacity_only_shaped' && option.preferred === true &&
			option.selected_topology === 'both_shaped' && option.action === 'apply_sqm' &&
			option.sqm_direction_mode === 'both' &&
			option.auto_apply_evidence_pass === false && option.manual_review_required === true &&
			option.target_rates_kbps &&
			option.target_rates_kbps.download === selected.download &&
			option.target_rates_kbps.upload === selected.upload &&
			Array.isArray(option.required_acknowledgements) &&
			option.required_acknowledgements.join('\n') === exactAcknowledgements.join('\n') &&
			directionalOption == null;
	}
	var commonRawFields = [ 'schema_version', 'selected_topology', 'reason',
		'failed_direction',
		'discarded_shaped_observation_count', 'target_grade', 'auto_apply_pass',
		'manual_review_required', 'download', 'upload', 'required_acknowledgements' ];
	var directionFields = [ 'direction', 'sample_count',
		'representative_achieved_kbps', 'effective_delta_ms', 'grade',
		'rate_consistent', 'target_status_consistent', 'target_met',
		'measurement_reliable', 'contaminated', 'safety_pass', 'samples' ];
	var sampleFields = [ 'topology', 'achieved_kbps', 'effective_delta_ms', 'grade',
		'transport_censored', 'loss_ppm', 'cpu_milli_percent',
		'background_confidence_percent', 'contaminated' ];
	var grades = { 'A+': true, A: true, B: true, C: true, D: true, F: true };
	var digestAck = function(values) {
		return Array.isArray(values) && values.join('\n');
	};
	var scalarMeasurement = function(value, upload) {
		var fields = upload ? [ 'achieved_kbps', 'realized_kbps', 'effective_delta_ms',
			'candidate_realization_percent', 'capacity_retention_percent', 'loss_ppm',
			'background_confidence_percent', 'contaminated', 'transport_censored',
			'capacity_alignment_confirmed' ] : [ 'achieved_kbps', 'effective_delta_ms',
			'loss_ppm', 'background_confidence_percent', 'contaminated',
			'transport_censored' ];
		if (!value || typeof value !== 'object' || Array.isArray(value) ||
		    Object.keys(value).sort().join(',') !== fields.slice().sort().join(',') ||
		    !Number.isSafeInteger(value.achieved_kbps) || value.achieved_kbps < 1 ||
		    typeof value.effective_delta_ms !== 'number' ||
		    !Number.isFinite(value.effective_delta_ms) || value.effective_delta_ms < 0 ||
		    !Number.isSafeInteger(value.loss_ppm) || value.loss_ppm < 0 ||
		    !Number.isSafeInteger(value.background_confidence_percent) ||
		    value.background_confidence_percent < 0 ||
		    value.background_confidence_percent > 100 ||
		    typeof value.contaminated !== 'boolean' ||
		    typeof value.transport_censored !== 'boolean')
			return false;
		return !upload || (Number.isSafeInteger(value.realized_kbps) && value.realized_kbps >= 1 &&
			typeof value.candidate_realization_percent === 'number' &&
			Number.isFinite(value.candidate_realization_percent) &&
			value.candidate_realization_percent >= 0 &&
			typeof value.capacity_retention_percent === 'number' &&
			Number.isFinite(value.capacity_retention_percent) &&
			value.capacity_retention_percent >= 0 &&
			typeof value.capacity_alignment_confirmed === 'boolean');
	};
	var mobileBypassValidated = function(value, directionalOption) {
		if (!value || typeof value !== 'object' || Array.isArray(value) ||
		    value.selected_topology !== 'upload_only_shaped' ||
		    typeof value.available !== 'boolean')
			return false;
		if (value.available === false) {
			var unavailableFields = [ 'available', 'selected_topology', 'candidate_ul_kbps',
				'failed_direction', 'reason', 'run_count', 'debit_count', 'samples' ];
			var samples = value.samples;
			return directionalOption == null &&
				Object.keys(value).sort().join(',') === unavailableFields.sort().join(',') &&
				Number.isSafeInteger(value.candidate_ul_kbps) && value.candidate_ul_kbps >= 100 &&
				(value.failed_direction === 'download' || value.failed_direction === 'upload') &&
				[ 'traffic_budget', 'observation_starved', 'transfer_unmeasurable' ].indexOf(value.reason) >= 0 &&
				Number.isSafeInteger(value.run_count) && value.run_count >= 0 && value.run_count <= 3 &&
				Number.isSafeInteger(value.debit_count) && value.debit_count >= value.run_count &&
				samples && typeof samples === 'object' && !Array.isArray(samples) &&
				Object.keys(samples).sort().join(',') === 'cpu,icmp,transport' &&
				[ samples.icmp, samples.transport, samples.cpu ].every(function(sample) {
					return Number.isSafeInteger(sample) && sample >= 0;
				});
		}
		var confirmedFields = [ 'available', 'selected_topology', 'selected_ul_kbps',
			'runtime_minimum_ul_kbps', 'download', 'upload', 'manual_apply_eligible',
			'required_acknowledgements' ];
		if (Object.keys(value).sort().join(',') !== confirmedFields.sort().join(',') ||
		    !Number.isSafeInteger(value.selected_ul_kbps) || value.selected_ul_kbps < 100 ||
		    value.runtime_minimum_ul_kbps !== null &&
			(!Number.isSafeInteger(value.runtime_minimum_ul_kbps) ||
			 value.runtime_minimum_ul_kbps < 100 ||
			 value.runtime_minimum_ul_kbps > value.selected_ul_kbps) ||
		    !scalarMeasurement(value.download, false) || !scalarMeasurement(value.upload, true) ||
		    typeof value.manual_apply_eligible !== 'boolean' ||
		    !Array.isArray(value.required_acknowledgements) ||
		    value.required_acknowledgements.length < 1 ||
		    value.required_acknowledgements.length > 24 ||
		    value.required_acknowledgements.indexOf('download-shaping-bypassed') < 0 ||
		    value.required_acknowledgements.indexOf('upload-shaping-bypassed') >= 0 ||
		    value.required_acknowledgements.indexOf('sqm-disabled') >= 0)
			return false;
		for (var i = 0; i < value.required_acknowledgements.length; i++)
			if (!NATIVE_AUTOTUNE_ACKNOWLEDGEMENT_CODES[value.required_acknowledgements[i]] ||
			    value.required_acknowledgements.indexOf(value.required_acknowledgements[i]) !== i)
				return false;
		return value.manual_apply_eligible === (directionalOption != null) &&
			(!directionalOption ||
			 (directionalOption.target_rates_kbps.upload === value.selected_ul_kbps &&
			  digestAck(directionalOption.required_acknowledgements) ===
				digestAck(value.required_acknowledgements)));
	};
	var directionValidated = function(value, direction, topologies) {
		if (!value || typeof value !== 'object' || Array.isArray(value) ||
		    Object.keys(value).sort().join(',') !== directionFields.slice().sort().join(',') ||
		    value.direction !== direction || value.sample_count !== 2 ||
		    !Number.isSafeInteger(value.representative_achieved_kbps) ||
		    value.representative_achieved_kbps < 1 ||
		    typeof value.effective_delta_ms !== 'number' ||
		    !Number.isFinite(value.effective_delta_ms) || value.effective_delta_ms < 0 ||
		    !grades[value.grade] || typeof value.rate_consistent !== 'boolean' ||
		    typeof value.target_status_consistent !== 'boolean' ||
		    typeof value.target_met !== 'boolean' ||
		    typeof value.measurement_reliable !== 'boolean' ||
		    typeof value.contaminated !== 'boolean' ||
		    typeof value.safety_pass !== 'boolean' || !Array.isArray(value.samples) ||
		    value.samples.length !== 2)
			return false;
		for (var i = 0; i < value.samples.length; i++) {
			var sample = value.samples[i];
			if (!sample || typeof sample !== 'object' || Array.isArray(sample) ||
			    Object.keys(sample).sort().join(',') !== sampleFields.slice().sort().join(',') ||
			    sample.topology !== topologies[i] ||
			    !Number.isSafeInteger(sample.achieved_kbps) || sample.achieved_kbps < 1 ||
			    typeof sample.effective_delta_ms !== 'number' ||
			    !Number.isFinite(sample.effective_delta_ms) || sample.effective_delta_ms < 0 ||
			    !grades[sample.grade] || typeof sample.transport_censored !== 'boolean' ||
			    !Number.isSafeInteger(sample.loss_ppm) || sample.loss_ppm < 0 ||
			    !Number.isSafeInteger(sample.cpu_milli_percent) || sample.cpu_milli_percent < 0 ||
			    !Number.isSafeInteger(sample.background_confidence_percent) ||
			    sample.background_confidence_percent < 0 ||
			    sample.background_confidence_percent > 100 ||
			    typeof sample.contaminated !== 'boolean')
				return false;
		}
		return value.representative_achieved_kbps === Math.min(
			value.samples[0].achieved_kbps, value.samples[1].achieved_kbps) &&
			value.effective_delta_ms === Math.max(
				value.samples[0].effective_delta_ms, value.samples[1].effective_delta_ms);
	};

	if (!raw || typeof raw !== 'object' || Array.isArray(raw) ||
	    raw.selected_topology !== 'no_sqm' ||
	    !Number.isSafeInteger(raw.discarded_shaped_observation_count) ||
	    raw.discarded_shaped_observation_count < 0 || !grades[raw.target_grade] ||
	    raw.auto_apply_pass !== false || raw.manual_review_required !== true ||
	    !Array.isArray(raw.required_acknowledgements) ||
	    digestAck(raw.required_acknowledgements) !==
		digestAck(option && option.required_acknowledgements))
		return false;
	if (raw.schema_version === 1) {
		var v1Fields = commonRawFields.concat([ 'unobserved_candidates_kbps' ]);
		if (Object.keys(raw).sort().join(',') !== v1Fields.sort().join(',') ||
		    raw.reason !== 'incomplete-shaped-search' ||
		    (raw.failed_direction !== 'download' && raw.failed_direction !== 'upload') ||
		    !Array.isArray(raw.unobserved_candidates_kbps) ||
		    raw.unobserved_candidates_kbps.length < 1 ||
		    raw.unobserved_candidates_kbps.length > 64 ||
		    !raw.unobserved_candidates_kbps.every(function(value, index, values) {
			    return Number.isSafeInteger(value) && value >= 100 &&
				    (index === 0 || values[index - 1] > value);
		    }))
			return false;
	}
	else if (raw.schema_version === 2) {
		var v2Fields = commonRawFields.concat([ 'terminal_boundary' ]);
		var v2Boundary = raw.terminal_boundary;
		if (Object.keys(raw).sort().join(',') !== v2Fields.sort().join(',') ||
		    raw.reason !== 'measured-shaped-search-inconclusive' ||
		    (raw.failed_direction !== 'download' && raw.failed_direction !== 'upload') ||
		    !v2Boundary || typeof v2Boundary !== 'object' || Array.isArray(v2Boundary) ||
		    Object.keys(v2Boundary).sort().join(',') !== 'candidate_kbps,kind' ||
		    (v2Boundary.kind !== 'loaded_observation_starved' &&
		     v2Boundary.kind !== 'candidate_transfer_unmeasurable') ||
		    !Number.isSafeInteger(v2Boundary.candidate_kbps) ||
		    v2Boundary.candidate_kbps < 100)
			return false;
	}
	else if (raw.schema_version === 3) {
		var v3Fields = commonRawFields.concat([ 'terminal_boundary' ]);
		var v3Boundary = raw.terminal_boundary;
		if (Object.keys(raw).sort().join(',') !== v3Fields.sort().join(',') ||
		    raw.reason !== 'shaped-pair-options-exhausted' || raw.failed_direction !== null ||
		    !v3Boundary || typeof v3Boundary !== 'object' || Array.isArray(v3Boundary) ||
		    Object.keys(v3Boundary).sort().join(',') !== 'candidate_count,kind' ||
		    v3Boundary.kind !== 'pair_options_exhausted' ||
		    !Number.isSafeInteger(v3Boundary.candidate_count) ||
		    v3Boundary.candidate_count < 1 || v3Boundary.candidate_count > 3)
			return false;
	}
	else if (raw.schema_version === 4) {
		var v4Fields = commonRawFields.concat([ 'terminal_boundary' ]);
		var v4Boundary = raw.terminal_boundary;
		var v4Reasons = {
			'variable-candidate-resource-safety-inconclusive': true,
			'variable-candidate-realization-inconclusive': true,
			'noisy-link-candidate-did-not-converge': true,
			'nonmonotonic-variable-link-after-retries': true,
			'queue-outside-cake-control-target-unmet': true,
			'exploration-floor-reached-without-latency-knee': true,
			'bounded-attempt-limit-before-latency-knee': true,
			'variable-link-search-cannot-make-progress': true,
			'repeated-candidate-realization-unreliable': true,
			'low-candidate-realization-not-repeatable': true,
			'unable-to-establish-controlled-shaper-candidate': true,
			'resource-safety-failure-not-resolved': true,
			'bounded-attempt-limit-without-safe-candidate': true,
			'throughput-search-has-no-safe-candidate': true,
			'profile-search-has-no-safe-candidate': true
		};
		if (Object.keys(raw).sort().join(',') !== v4Fields.sort().join(',') ||
		    raw.reason !== 'shaped-search-inconclusive' ||
		    (raw.failed_direction !== 'download' && raw.failed_direction !== 'upload') ||
		    !v4Boundary || typeof v4Boundary !== 'object' || Array.isArray(v4Boundary) ||
		    Object.keys(v4Boundary).sort().join(',') !== 'kind,observation_count,optimizer_reason' ||
		    v4Boundary.kind !== 'shaped_search_inconclusive' ||
		    !Number.isSafeInteger(v4Boundary.observation_count) ||
		    v4Boundary.observation_count < 1 || v4Boundary.observation_count > 12 ||
		    !v4Reasons[v4Boundary.optimizer_reason])
			return false;
	}
	else if (raw.schema_version === 5) {
		var v5Fields = commonRawFields.concat([ 'terminal_boundary', 'mobile_download_bypass' ]);
		var v5Boundary = raw.terminal_boundary;
		if (Object.keys(raw).sort().join(',') !== v5Fields.sort().join(',') ||
		    raw.reason !== 'shaped-pair-options-exhausted' || raw.failed_direction !== null ||
		    !v5Boundary || typeof v5Boundary !== 'object' || Array.isArray(v5Boundary) ||
		    Object.keys(v5Boundary).sort().join(',') !== 'candidate_count,kind' ||
		    v5Boundary.kind !== 'pair_options_exhausted' ||
		    !Number.isSafeInteger(v5Boundary.candidate_count) ||
		    v5Boundary.candidate_count < 1 || v5Boundary.candidate_count > 3 ||
		    !mobileBypassValidated(raw.mobile_download_bypass, directionalOption || null))
			return false;
	}
	else {
		return false;
	}
	for (var a = 0; a < raw.required_acknowledgements.length; a++) {
		if (!NATIVE_AUTOTUNE_ACKNOWLEDGEMENT_CODES[raw.required_acknowledgements[a]] ||
		    raw.required_acknowledgements.indexOf(raw.required_acknowledgements[a]) !== a)
			return false;
	}
	var mandatory = [ 'download-shaping-bypassed', 'upload-shaping-bypassed', 'sqm-disabled' ];
	for (var m = 0; m < mandatory.length; m++)
		if (raw.required_acknowledgements.indexOf(mandatory[m]) < 0)
			return false;
	return directionValidated(raw.download, 'download', [ 'download_unshaped', 'no_sqm' ]) &&
		directionValidated(raw.upload, 'upload', [ 'upload_unshaped', 'no_sqm' ]);
}

function nativeAutotunePublicResultValidated(result) {
	var rawPublicResult = result &&
		(result.native_public_schema_version === 4 ||
		 result.native_public_schema_version === 5 ||
		 result.native_public_schema_version === 6);
	var artifactNames = rawPublicResult ? [ 'proposal', 'raw_fallback' ] :
		[ 'proposal', 'download_search', 'upload_search',
			'pair_confirmation', 'topology_comparison' ];
	var artifactSchemas = rawPublicResult ? [ 4, null ] : [ 4, 4, 4, 6, 2 ];
	var artifacts = result && result.artifacts;
	var contract = result && result.public_apply_contract;
	var contractFields = [ 'schema_version', 'state', 'executor_available',
		'explicit_confirmation_required', 'native_job_id', 'worker_run_id',
		'source_review_sha256', 'selection_contract', 'options' ];
	var optionFields = [ 'option_id', 'preferred', 'manifest_sha256',
		'selected_topology', 'action', 'sqm_direction_mode', 'target_rates_kbps',
		'auto_apply_evidence_pass', 'manual_review_required',
		'required_acknowledgements' ];
	var safeName = /^[A-Za-z0-9_.:@-]{1,64}$/;
	var optionId = /^[a-z_]{1,32}$/;
	var digest = /^[0-9a-f]{64}$/;

	if (!result || (result.native_public_schema_version !== 3 &&
	    result.native_public_schema_version !== 4 &&
	    result.native_public_schema_version !== 5 &&
	    result.native_public_schema_version !== NATIVE_AUTOTUNE_PUBLIC_SCHEMA_VERSION) ||
	    result.state !== 'review_ready' || result.producer !== NATIVE_AUTOTUNE_PUBLIC_PRODUCER ||
	    !digest.test(result.source_review_sha256 || '') ||
	    !contract || typeof contract !== 'object' || Array.isArray(contract) ||
	    Object.keys(contract).sort().join(',') !== contractFields.slice().sort().join(',') ||
	    contract.schema_version !== NATIVE_AUTOTUNE_APPLY_CONTRACT_SCHEMA_VERSION ||
	    contract.state !== 'selection_ready' || contract.executor_available !== true ||
	    contract.explicit_confirmation_required !== true ||
	    contract.native_job_id !== result.native_job_id ||
	    contract.worker_run_id !== result.run_id ||
	    contract.source_review_sha256 !== result.source_review_sha256 ||
	    contract.selection_contract !== 'option_id_plus_review_and_manifest_digests_and_acknowledgements' ||
	    !Array.isArray(contract.options) || contract.options.length < 1 ||
	    contract.options.length > 4 ||
	    result.auto_apply_eligible !== false ||
	    result.manual_apply_eligible !== true || result.configuration_written !== false ||
	    result.runtime_restored !== true || result.recovery_pending !== false ||
	    result.throughput_unit !== 'kbit/s' || result.proposal_rate_transform !== 'none' ||
	    !/^[0-9a-f]{32}$/.test(result.native_job_id || '') ||
	    !/^[0-9a-f]{32}$/.test(result.run_id || '') ||
	    !/^[A-Za-z0-9_-]{1,64}$/.test(result.job_id || '') ||
	    !safeName.test(result.target_interface || '') ||
	    !safeName.test(result.resolved_interface || '') ||
	    !/^sha256:[0-9a-f]{64}$/.test(result.route_fingerprint || '') ||
	    !/^sha256:[0-9a-f]{64}$/.test(result.config_fingerprint || '') ||
	    !/^sha256:[0-9a-f]{64}$/.test(result.sqm_fingerprint || '') ||
	    !canonicalAutotuneProfile(result.profile) ||
	    [ 'shaped_only', 'full_raw', 'reuse_trusted' ].indexOf(result.calibration_strategy) < 0 ||
	    !Number.isSafeInteger(result.consumed_traffic_bytes) || result.consumed_traffic_bytes < 0 ||
	    (result.route_mode !== 'main' && result.route_mode !== 'mwan3') ||
	    (result.route_mode === 'main' && result.mwan3_member !== null) ||
	    (result.route_mode === 'mwan3' && !safeName.test(result.mwan3_member || '')) ||
	    (result.source_ip !== null && typeof result.source_ip !== 'string') ||
	    !artifacts || typeof artifacts !== 'object' || Array.isArray(artifacts) ||
	    Object.keys(artifacts).sort().join(',') !== artifactNames.slice().sort().join(','))
		return false;

	for (var i = 0; i < artifactNames.length; i++) {
		var artifact = artifacts[artifactNames[i]];
		if (!artifact || !digest.test(artifact.sha256 || '') ||
		    !artifact.value || typeof artifact.value !== 'object' ||
		    Array.isArray(artifact.value) ||
		    (rawPublicResult && artifactNames[i] === 'raw_fallback' ?
			    [ 1, 2, 3, 4, 5, 6 ].indexOf(artifact.value.schema_version) < 0 :
			    artifact.value.schema_version !== artifactSchemas[i]))
			return false;
	}
	var proposal = artifacts.proposal.value;
	if (canonicalAutotuneProfile(proposal.profile) !== canonicalAutotuneProfile(result.profile))
		return false;
	if (rawPublicResult) {
		var rawContractOptionValidated = function(option) {
			if (!option || typeof option !== 'object' || Array.isArray(option) ||
			    Object.keys(option).sort().join(',') !== optionFields.slice().sort().join(',') ||
			    !optionId.test(option.option_id || '') || !digest.test(option.manifest_sha256 || '') ||
			    typeof option.preferred !== 'boolean' ||
			    option.auto_apply_evidence_pass !== false ||
			    option.manual_review_required !== true ||
			    !Array.isArray(option.required_acknowledgements) ||
			    option.required_acknowledgements.length < 1 ||
			    option.required_acknowledgements.length > 24)
				return false;
			for (var index = 0; index < option.required_acknowledgements.length; index++) {
				var acknowledgement = option.required_acknowledgements[index];
				if (!NATIVE_AUTOTUNE_ACKNOWLEDGEMENT_CODES[acknowledgement] ||
				    option.required_acknowledgements.indexOf(acknowledgement) !== index)
					return false;
			}
			return true;
		};
		var rawOption = contract.options.filter(function(option) {
			return option && option.option_id === 'no_sqm';
		})[0] || null;
		var directionalOption = contract.options.filter(function(option) {
			return option && option.option_id === 'bypass_download';
		})[0] || null;
		var rawRates = rawOption && rawOption.target_rates_kbps;
		var directionalRates = directionalOption && directionalOption.target_rates_kbps;
		if (artifacts.raw_fallback.value.schema_version === 6) {
			var shapedCapacityOption = contract.options[0] || null;
			return result.native_public_schema_version === 6 &&
				contract.options.length === 1 &&
				rawContractOptionValidated(shapedCapacityOption) &&
				nativeAutotuneRawFallbackValidated(
					artifacts.raw_fallback.value, shapedCapacityOption, null);
		}
		if (result.native_public_schema_version === 6)
			return false;
		var exactOptionCount = result.native_public_schema_version === 5 ? 2 : 1;
		return contract.options.length === exactOptionCount &&
			(result.native_public_schema_version !== 5 ||
			 artifacts.raw_fallback.value.schema_version === 5) &&
			rawContractOptionValidated(rawOption) &&
			rawOption.option_id === 'no_sqm' && rawOption.preferred === true &&
			rawOption.selected_topology === 'no_sqm' && rawOption.action === 'disable_sqm' &&
			rawOption.sqm_direction_mode === 'off' && rawOption.auto_apply_evidence_pass === false &&
			rawOption.manual_review_required === true && rawRates &&
			Object.keys(rawRates).sort().join(',') === 'download,upload' &&
			rawRates.download === null && rawRates.upload === null &&
			(result.native_public_schema_version === 4 ? directionalOption == null :
			 rawContractOptionValidated(directionalOption) && directionalOption.preferred === false &&
			 directionalOption.selected_topology === 'upload_only_shaped' &&
			 directionalOption.action === 'apply_sqm' &&
			 directionalOption.sqm_direction_mode === 'upload_only' &&
			 directionalOption.auto_apply_evidence_pass === false &&
			 directionalOption.manual_review_required === true && directionalRates &&
			 Object.keys(directionalRates).sort().join(',') === 'download,upload' &&
			 directionalRates.download === null &&
			 Number.isSafeInteger(directionalRates.upload) && directionalRates.upload >= 100) &&
			nativeAutotuneRawFallbackValidated(
				artifacts.raw_fallback.value, rawOption, directionalOption);
	}
	var download = artifacts.download_search.value;
	var upload = artifacts.upload_search.value;
	var pair = artifacts.pair_confirmation.value;
	var topology = artifacts.topology_comparison.value;
	var topologyContract = {
		both_shaped: [ 'apply_sqm', 'both' ],
		download_only_shaped: [ 'apply_sqm', 'download_only' ],
		upload_only_shaped: [ 'apply_sqm', 'upload_only' ],
		no_sqm: [ 'disable_sqm', 'off' ],
	};
	var pairOptions = Array.isArray(pair.options) ? pair.options : [];
	var selectedRates = topology.selected_rates_kbps || {};
	var topologyTrafficBudgetLimited = [ 'download', 'upload' ].some(function(direction) {
		return topology[direction] && topology[direction].reason === 'traffic-budget-limited';
	});
	var topologyComparisonUnmeasurable = [ 'download', 'upload' ].some(function(direction) {
		return topology[direction] && topology[direction].reason === 'comparison-unmeasurable';
	});
	var seenOptions = {};
	var preferredCount = 0;
	var preferredMatchesTopology = false;
	var rate = function(value, nullable) {
		return nullable && value === null || (Number.isSafeInteger(value) && value >= 100);
	};

	if (download.direction !== 'download' || upload.direction !== 'upload' ||
	    canonicalAutotuneProfile(download.profile) !== canonicalAutotuneProfile(result.profile) ||
	    canonicalAutotuneProfile(upload.profile) !== canonicalAutotuneProfile(result.profile) ||
	    !nativeAutotuneSearchArtifactValidated(download, 'download',
		canonicalAutotuneProfile(result.profile)) ||
	    !nativeAutotuneSearchArtifactValidated(upload, 'upload',
		canonicalAutotuneProfile(result.profile)) ||
	    !nativeAutotunePairTransportValidated(pair) ||
	    !nativeAutotuneTopologyTransportValidated(topology) ||
	    pair.topology !== 'both_shaped' || pairOptions.length < 1 || pairOptions.length > 3 ||
	    topologyContract[topology.selected_topology] == null)
		return false;

	for (var p = 0; p < pairOptions.length; p++) {
		var pairOption = pairOptions[p];
		var pairRates = pairOption && pairOption.target_rates_kbps;
		if (!pairOption || !optionId.test(pairOption.option_id || '') ||
		    !pairRates || !rate(pairRates.download, false) || !rate(pairRates.upload, false))
			return false;
	}

	for (var o = 0; o < contract.options.length; o++) {
		var option = contract.options[o];
		var rates = option && option.target_rates_kbps;
		var topologyRule = option && topologyContract[option.selected_topology];
		var acknowledgements = option && option.required_acknowledgements;
		var seenAcknowledgements = {};
		var pairEvidence;
		var selectedShapedCensored;
		if (!option || typeof option !== 'object' || Array.isArray(option) ||
		    Object.keys(option).sort().join(',') !== optionFields.slice().sort().join(',') ||
		    !optionId.test(option.option_id || '') || seenOptions[option.option_id] ||
		    !digest.test(option.manifest_sha256 || '') ||
		    typeof option.preferred !== 'boolean' ||
		    typeof option.auto_apply_evidence_pass !== 'boolean' ||
		    typeof option.manual_review_required !== 'boolean' ||
		    !Array.isArray(acknowledgements) || acknowledgements.length > 24 ||
		    !topologyRule || option.action !== topologyRule[0] ||
		    option.sqm_direction_mode !== topologyRule[1] ||
		    !rates || typeof rates !== 'object' || Array.isArray(rates) ||
		    Object.keys(rates).sort().join(',') !== 'download,upload' ||
		    !rate(rates.download, true) || !rate(rates.upload, true))
			return false;
		for (var a = 0; a < acknowledgements.length; a++) {
			var acknowledgement = acknowledgements[a];
			if (!NATIVE_AUTOTUNE_ACKNOWLEDGEMENT_CODES[acknowledgement] ||
			    seenAcknowledgements[acknowledgement])
				return false;
			seenAcknowledgements[acknowledgement] = true;
		}
		if ((option.selected_topology === 'upload_only_shaped') !==
		    (seenAcknowledgements['download-shaping-bypassed'] === true) ||
		    (option.selected_topology === 'download_only_shaped') !==
		    (seenAcknowledgements['upload-shaping-bypassed'] === true) ||
		    (option.selected_topology === 'no_sqm') !==
		    (seenAcknowledgements['sqm-disabled'] === true))
			return false;
		if ((seenAcknowledgements['topology-comparison-traffic-budget'] === true) !==
		    topologyTrafficBudgetLimited)
			return false;
		if ((seenAcknowledgements['topology-comparison-unmeasurable'] === true) !==
		    topologyComparisonUnmeasurable)
			return false;
		pairEvidence = pairOptions.find(function(candidate) {
			return candidate.option_id === option.option_id;
		});
		if (pairEvidence) {
			var limited = pairEvidence.physical_capacity_limited_review;
			var aligned = pairEvidence.physical_capacity_alignment_confirmed;
			var downloadPhysicalAck =
				seenAcknowledgements['download-physical-capacity-limited'] === true;
			var uploadPhysicalAck =
				seenAcknowledgements['upload-physical-capacity-limited'] === true;
			if (downloadPhysicalAck !== limited.download ||
			    uploadPhysicalAck !== limited.upload ||
			    (seenAcknowledgements['download-throughput-safety-floor'] === true &&
			     aligned.download !== true) ||
			    (seenAcknowledgements['upload-throughput-safety-floor'] === true &&
			     aligned.upload !== true) ||
			    option.auto_apply_evidence_pass !==
				(pairEvidence.auto_apply_pass && !topologyTrafficBudgetLimited &&
				 !topologyComparisonUnmeasurable) ||
			    option.manual_review_required !==
				(pairEvidence.manual_review_required || topologyTrafficBudgetLimited ||
				 topologyComparisonUnmeasurable))
				return false;
		}
		selectedShapedCensored =
			(option.selected_topology === 'both_shaped' ||
			 option.selected_topology === 'download_only_shaped') &&
			 topology.download.shaped.transport_censored ||
			(option.selected_topology === 'both_shaped' ||
			 option.selected_topology === 'upload_only_shaped') &&
			 topology.upload.shaped.transport_censored;
		if ((pairEvidence ? pairEvidence.transport_censored : selectedShapedCensored) &&
		    seenAcknowledgements['measurement-confidence'] !== true)
			return false;
		if (option.auto_apply_evidence_pass !== (acknowledgements.length === 0) ||
		    option.manual_review_required !== (acknowledgements.length > 0))
			return false;
		seenOptions[option.option_id] = true;
		preferredCount += option.preferred ? 1 : 0;

		if (option.selected_topology === 'both_shaped') {
			if (rates.download === null || rates.upload === null ||
			    !pairOptions.some(function(candidate) {
				    var candidateRates = candidate.target_rates_kbps || {};
				    return candidate.option_id === option.option_id &&
					    candidateRates.download === rates.download &&
					    candidateRates.upload === rates.upload;
			    }))
				return false;
		}
		else if ((option.selected_topology === 'download_only_shaped' &&
				(rates.download === null || rates.upload !== null)) ||
			(option.selected_topology === 'upload_only_shaped' &&
				(rates.download !== null || rates.upload === null)) ||
			(option.selected_topology === 'no_sqm' &&
				(rates.download !== null || rates.upload !== null)))
			return false;

		if (option.preferred) {
			preferredMatchesTopology = option.selected_topology === topology.selected_topology &&
				rates.download === (selectedRates.download == null ? null : selectedRates.download) &&
				rates.upload === (selectedRates.upload == null ? null : selectedRates.upload);
		}
	}

	return preferredCount === 1 && preferredMatchesTopology;
}

function nativeAutotuneAcknowledgementLabel(code) {
	switch (code) {
	case 'download-candidate-realization':
		return _('Download achieved less than the requested CAKE rate, but remains above the manual safety floor.');
	case 'upload-candidate-realization':
		return _('Upload achieved less than the requested CAKE rate, but remains above the manual safety floor.');
	case 'download-capacity-retention':
		return _('Download retention is below this profile\'s throughput objective.');
	case 'upload-capacity-retention':
		return _('Upload retention is below this profile\'s throughput objective.');
	case 'download-throughput-safety-floor':
		return _('Download goodput fell below the ordinary manual safety floor. The exact CAKE ceiling was tested, but the physical link was slower during calibration.');
	case 'upload-throughput-safety-floor':
		return _('Upload goodput fell below the ordinary manual safety floor. The exact CAKE ceiling was tested, but the physical link was slower during calibration.');
	case 'download-physical-capacity-limited':
		return _('Download CAKE wire rate tracked measured goodput, but the physical link remained below the configured CAKE ceiling. This ceiling is not proven to control the bottleneck and cannot be applied automatically.');
	case 'upload-physical-capacity-limited':
		return _('Upload CAKE wire rate tracked measured goodput, but the physical link remained below the configured CAKE ceiling. This ceiling is not proven to control the bottleneck and cannot be applied automatically.');
	case 'download-icmp-latency':
		return _('Download ICMP loaded latency missed the selected profile target.');
	case 'download-transport-latency':
		return _('Download transport-aware loaded latency missed the selected profile target.');
	case 'upload-icmp-latency':
		return _('Upload ICMP loaded latency missed the selected profile target.');
	case 'upload-transport-latency':
		return _('Upload transport-aware loaded latency missed the selected profile target.');
	case 'measurement-contaminated':
		return _('Background traffic contaminated the shaped pair measurement.');
	case 'measurement-confidence':
		return _('The shaped pair measurement confidence is below 80%.');
	case 'download-raw-contaminated':
		return _('Background traffic contaminated the download-without-shaping comparison.');
	case 'download-raw-confidence':
		return _('Download-without-shaping evidence is reviewable but not fully reliable.');
	case 'download-raw-quality-target':
		return _('Download without shaping did not reach the selected profile quality target.');
	case 'upload-raw-contaminated':
		return _('Background traffic contaminated the upload-without-shaping comparison.');
	case 'upload-raw-confidence':
		return _('Upload-without-shaping evidence is reviewable but not fully reliable.');
	case 'upload-raw-quality-target':
		return _('Upload without shaping did not reach the selected profile quality target.');
	case 'topology-comparison-traffic-budget':
		return _('The conservative traffic budget could not safely fund another unshaped comparison. This applies the fully verified shaped proposal; rerun Full raw with a larger explicit traffic allowance if you want another bypass comparison.');
	case 'topology-comparison-unmeasurable':
		return _('One optional without-shaping comparison could not produce a trustworthy transfer result. This option keeps shaping for that direction and uses only the fully verified shaped evidence.');
	case 'download-shaping-bypassed':
		return _('This option disables CAKE shaping for download.');
	case 'upload-shaping-bypassed':
		return _('This option disables CAKE shaping for upload.');
	case 'sqm-disabled':
		return _('This option disables SQM and CAKE shaping in both directions.');
	case 'loaded-latency-unobservable':
		return _('Loaded latency could not be measured reliably for the listed failed direction. No latency class is claimed.');
	case 'shaped-validation-incomplete':
		return _('The shaped pair did not complete validation in both directions. The selected rates are capacity-bounded and cannot grow automatically.');
	default:
		return code;
	}
}

function reloadAppliedUciPackages() {
	if (typeof uci.unload === 'function') {
		uci.unload('cake-autorate');
		uci.unload('sqm');
	}
	return Promise.all([
		uci.load('cake-autorate'),
		L.resolveDefault(uci.load('sqm'), null)
	]);
}

function reloadAppliedSettingsPage() {
	return reloadAppliedUciPackages().then(function() {
		window.location.reload();
	}, function() {
		/* Apply is already verified and must never be presented as failed merely
		 * because LuCI could not rebuild its client-side UCI cache. */
		window.location.reload();
	});
}

function nativeMobileDownloadBypassRequested(proposal) {
	var access = proposal && proposal.access || {};
	return access.source === 'user_selected' &&
		[ 'cellular', 'leo_satellite', 'geo_satellite', 'fixed_wireless' ]
			.indexOf(access.medium) >= 0;
}

function nativeDownloadBypassUnavailableReason(topology, rawFallback) {
	if (rawFallback) {
		var mobile = rawFallback.mobile_download_bypass;
		if (mobile && mobile.available === false) {
			switch (mobile.reason) {
			case 'traffic_budget':
				return _('The remaining traffic allowance could not fund the final download-without-shaping and upload-shaped check. The full SQM-disabled result is still available.');
			case 'observation_starved':
				return _('The final mobile-link check ran, but did not collect enough latency or CPU samples for a safe download-bypass proposal.');
			case 'transfer_unmeasurable':
				return _('The final mobile-link transfer could not produce a trustworthy directional result, so download bypass is not offered.');
			}
		}
		if (mobile && mobile.available === true && mobile.manual_apply_eligible === false)
			return _('Download without shaping and upload with shaping were both measured, but the result failed the hard loss, rate-realization, or throughput safety checks. It remains diagnostic only.');
		var rawReason = String(rawFallback.reason || 'missing-reason')
			.replace(/[\x00-\x1f\x7f]/g, '?').slice(0, 80);
		return _('Only the full SQM-disabled raw fallback was verified in this run (%s). A download-only bypass also needs a verified upload-shaped rate, so it cannot be applied safely.').format(rawReason);
	}
	var download = topology && topology.download || {};
	switch (download.reason) {
	case 'comparison-not-requested':
		return _('This run did not measure download without shaping. Rerun with Full raw capacity to make this option reviewable.');
	case 'traffic-budget-limited':
		return _('The traffic budget could not fund enough download-without-shaping measurements. Rerun with a larger traffic allowance.');
	case 'repeat-required':
		return _('Download-without-shaping measurements disagreed and require another repeat before this option can be reviewed.');
	case 'inconclusive-raw-evidence':
		return _('Download-without-shaping evidence remained noisy or incomplete after the allowed repeats.');
	case 'no-material-throughput-gain':
		return _('Download without shaping did not provide a material throughput advantage in this run.');
	case 'shaped-quality-preferred':
		return _('The measured download-without-shaping result was worse than the shaped result.');
	default:
		var exactReason = String(download.reason || 'missing-reason')
			.replace(/[\x00-\x1f\x7f]/g, '?').slice(0, 80);
		return _('A verified download-without-shaping comparison is not available (%s).').format(exactReason);
	}
}

function renderNativeAutotuneDiagnostics(result, onApplied, onSkip) {
	var artifacts = result.artifacts;
	var proposal = artifacts.proposal && artifacts.proposal.value;
	var pair = artifacts.pair_confirmation && artifacts.pair_confirmation.value;
	var topology = artifacts.topology_comparison && artifacts.topology_comparison.value;
	var rawFallback = artifacts.raw_fallback && artifacts.raw_fallback.value;
	var disabledFallback = result.native_public_schema_version === 4;
	var topologyBudgetLimitedDirections = disabledFallback ? [] : [ 'download', 'upload' ].filter(function(direction) {
		return topology && topology[direction] &&
			topology[direction].reason === 'traffic-budget-limited';
	});
	var contract = result.public_apply_contract;
	var pairOptions = pair && pair.options || [];
	var roleLabels = {
		recommended: _('Recommended'),
		quality_first: _('Lowest measured latency'),
		throughput_first: _('Highest measured throughput'),
		balanced_alternative: _('Balanced alternative'),
		bypass_download: _('Download without shaping'),
		bypass_upload: _('Upload without shaping'),
		no_sqm: _('SQM disabled')
	};
	var preferred = contract.options.find(function(option) { return option.preferred; });
	if (!result._native_apply_ui) {
		Object.defineProperty(result, '_native_apply_ui', {
			value: {
				selected: preferred.option_id,
				acknowledged: {},
				pending: false,
				error: '',
				receipt: null
			},
			enumerable: false
		});
	}
	var state = result._native_apply_ui;
	var root = E('div', { 'class': 'alert-message warning cake-autotune-native-review' });

	var selectedOption = function() {
		return contract.options.find(function(option) {
			return option.option_id === state.selected;
		}) || preferred;
	};
	var optionEvidence = function(option) {
		if (rawFallback && option.selected_topology === 'no_sqm') {
			var rawDownload = rawFallback.download || {};
			var rawUpload = rawFallback.upload || {};
			var rawSamples = (rawDownload.samples || []).concat(rawUpload.samples || []);
			return {
				achieved: {
					download: rawDownload.representative_achieved_kbps,
					upload: rawUpload.representative_achieved_kbps
				},
				grade: _('%s / %s').format(rawDownload.grade || '-', rawUpload.grade || '-'),
				transportCensored: rawSamples.some(function(sample) {
					return sample.transport_censored === true;
				})
			};
		}
		if (rawFallback && option.option_id === 'bypass_download') {
			var mobileBypass = rawFallback.mobile_download_bypass || {};
			var bypassDownload = mobileBypass.download || {};
			var bypassUpload = mobileBypass.upload || {};
			var downloadGrade = autotuneGradeForDelta(bypassDownload.effective_delta_ms);
			var uploadGrade = autotuneGradeForDelta(bypassUpload.effective_delta_ms);
			return {
				achieved: {
					download: bypassDownload.achieved_kbps,
					upload: bypassUpload.achieved_kbps
				},
				grade: _('%s / %s').format(downloadGrade || '-', uploadGrade || '-'),
				transportCensored: bypassDownload.transport_censored === true ||
					bypassUpload.transport_censored === true
			};
		}
		var pairEvidence = pairOptions.find(function(candidate) {
			return candidate.option_id === option.option_id;
		});
		if (pairEvidence)
			return {
				achieved: pairEvidence.achieved_kbps || {},
				grade: pairEvidence.validation && pairEvidence.validation.actual_grade || '-',
				transportCensored: pairEvidence.transport_censored === true
			};
		var directionValue = function(direction) {
			var comparison = topology && topology[direction] || {};
			var choice = comparison.choice === 'unshaped' ? comparison.unshaped : comparison.shaped;
			return choice || {};
		};
		var download = directionValue('download');
		var upload = directionValue('upload');
		return {
			achieved: { download: download.achieved_kbps, upload: upload.achieved_kbps },
			grade: _('%s / %s').format(download.grade || '-', upload.grade || '-'),
			transportCensored: download.transport_censored === true ||
				upload.transport_censored === true
		};
	};

	var render = function() {
		var option = selectedOption();
		var required = option.required_acknowledgements || [];
		var accepted = state.acknowledged[option.option_id] || {};
		var allAccepted = required.every(function(code) { return accepted[code] === true; });
		var selectedEvidence = optionEvidence(option);

		if (state.receipt) {
			root.className = 'alert-message success cake-autotune-native-review';
			replaceNodeContent(root, [
			E('strong', {}, state.receipt.state === 'already_applied' ?
					_('The selected settings were already applied') : _('Settings applied successfully')),
				E('p', {}, disabledFallback ?
					_('The digest-bound disabled instance was written and verified without starting a controller or creating an SQM queue. No LuCI UCI changes were staged.') :
					_('The selected digest-bound option was written and the selected instance was restarted and verified. Refreshing the committed settings now...'))
			]);
			return;
		}

		root.className = 'alert-message warning cake-autotune-native-review';
		var optionCards = contract.options.map(function(candidate) {
			var rates = candidate.target_rates_kbps || {};
			var evidence = optionEvidence(candidate);
			var achieved = evidence.achieved;
			var isSelected = candidate.option_id === option.option_id;
			var border = isSelected ? 'var(--primary-color,#00a0d2)' : 'rgba(127,127,127,.4)';
			var flags = [];
			if (candidate.preferred)
				flags.push(_('recommended for this profile'));
			if (candidate.auto_apply_evidence_pass)
				flags.push(_('all evidence gates passed'));
			else
				flags.push(_('%d confirmations required').format(candidate.required_acknowledgements.length));
			if (evidence.transportCensored)
				flags.push(_('transport delay is a lower bound'));
			return E('label', {
				'class': 'cake-autotune-native-option',
				'data-option-id': candidate.option_id,
				'style': 'min-width:210px;flex:1 1 240px;padding:10px;border:2px solid %s;border-radius:6px;cursor:pointer'.format(border)
			}, [
				E('input', {
					'type': 'radio',
					'name': 'native-autotune-option-' + result.native_job_id,
					'value': candidate.option_id,
					'checked': isSelected ? 'checked' : null,
					'disabled': state.pending ? 'disabled' : null,
					'change': function() {
						state.selected = candidate.option_id;
						state.error = '';
						render();
					}
				}), ' ',
				E('strong', {}, roleLabels[candidate.option_id] || candidate.option_id.replace(/_/g, ' ')),
				E('div', { 'style': 'margin-top:5px' }, _('Class %s · %s').format(evidence.grade,
					candidate.selected_topology.replace(/_/g, ' '))),
				E('div', {}, _('CAKE DL / UL: %s / %s kbit/s').format(
					rates.download == null ? _('off') : rates.download,
					rates.upload == null ? _('off') : rates.upload)),
				E('div', {}, _('Measured DL / UL: %s / %s kbit/s').format(
					achieved.download == null ? '-' : achieved.download,
					achieved.upload == null ? '-' : achieved.upload)),
				E('small', { 'style': 'display:block;margin-top:5px' }, flags.join(' · '))
			]);
		});
		var hasDownloadBypass = contract.options.some(function(candidate) {
			return candidate.option_id === 'bypass_download';
		});
		if (nativeMobileDownloadBypassRequested(proposal) && !hasDownloadBypass) {
			optionCards.push(E('div', {
				'class': 'cake-autotune-native-option cake-autotune-native-option-unavailable',
				'data-option-id': 'bypass_download_unavailable',
				'aria-disabled': 'true',
				'style': 'min-width:210px;flex:1 1 240px;padding:10px;border:2px dashed rgba(127,127,127,.4);border-radius:6px;opacity:.8'
			}, [
				E('strong', {}, _('Download without shaping')),
				E('div', { 'style': 'margin-top:5px' }, _('Unavailable for this run')),
				E('p', { 'style': 'margin:5px 0' }, cakeUi.text(nativeDownloadBypassUnavailableReason(
					topology, disabledFallback ? rawFallback : null))),
				E('small', {}, _('No untested rate or topology can be applied from this card.'))
			]));
		}

		var acknowledgementNodes = required.length ? [
			E('strong', {}, _('Confirm measured trade-offs for this option:')),
			E('div', { 'style': 'display:flex;flex-direction:column;gap:7px;margin-top:7px' },
				[ E('ul', { 'style': 'margin:0 0 4px 20px' }, required.map(function(code) {
					return E('li', {}, [ cakeUi.text(nativeAutotuneAcknowledgementLabel(code)),
						E('small', { 'style': 'display:block;opacity:.75' }, cakeUi.text(code)) ]);
				})), (function() {
					var checkbox = E('input', {
						'type': 'checkbox',
						'checked': allAccepted ? 'checked' : null,
						'disabled': state.pending ? 'disabled' : null,
						'change': function() {
							required.forEach(function(code) { accepted[code] = checkbox.checked; });
							state.acknowledged[option.option_id] = accepted;
							render();
						}
					});
					return E('label', { 'style': 'display:flex;gap:8px;align-items:flex-start' }, [
						checkbox, E('strong', {}, _('I accept all listed trade-offs.'))
					]);
				})() ]
			)
		] : [ E('span', {}, _('All automatic evidence gates passed; clicking Apply is still an explicit confirmation.')) ];

		var applyButton = E('button', {
			'type': 'button',
			'class': 'btn cbi-button cbi-button-positive important',
			'disabled': state.pending || !allAccepted ? 'disabled' : null,
			'click': function() {
				state.pending = true;
				state.error = '';
				render();
				return runNativeAutotuneApply(result, option).then(function(receipt) {
					state.pending = false;
					state.receipt = receipt;
					if (typeof onApplied !== 'function') {
						render();
						return reloadAppliedSettingsPage().then(function() { return receipt; });
					}
					try {
						return Promise.resolve(onApplied(receipt, option)).catch(function(error) {
							state.error = _('Apply completed, but the wizard could not advance: %s').format(
								error.message || String(error));
							render();
							return receipt;
						});
					}
					catch (error) {
						state.error = _('Apply completed, but the wizard could not advance: %s').format(
							error.message || String(error));
						render();
						return receipt;
					}
				}, function(error) {
					state.pending = false;
					state.error = error.message || String(error);
					render();
				});
			}
		}, state.pending ? _('Applying and verifying...') : _('Apply selected option'));
		var actionButtons = [ applyButton ];
		if (typeof onSkip === 'function') {
			actionButtons.push(' ', E('button', {
				'type': 'button',
				'class': 'btn cbi-button',
				'disabled': state.pending ? 'disabled' : null,
				'click': function() {
					state.error = '';
					try {
						return onSkip(result);
					}
					catch (error) {
						state.error = error.message || String(error);
						render();
					}
				}
			}, _('Skip calibration and create disabled')));
		}

		var nodes = [
			E('strong', {}, _('Full Auto-Tune Review · ready to apply')),
			E('p', {}, disabledFallback ?
				_('The shaped search could not produce an observable candidate. The verified raw controls were preserved and can create one disabled, uncalibrated instance. No rate is invented and SQM remains absent until you calibrate or configure it later.') :
				_('Calibration completed and restored runtime state. Every option is reconstructed from private evidence and bound to its own manifest; browser rate values are never Apply authority.')),
		];
		if (selectedEvidence.transportCensored) {
			nodes.push(E('div', {
				'class': 'alert-message warning',
				'style': 'margin:8px 0'
			}, _('A verified transport probe reached its deadline under load. The displayed transport delay is only a lower bound, not an exact RTT. It cannot satisfy the selected latency class or Auto-Apply; applying this option requires explicit manual acknowledgement.')));
		}
		if (topologyBudgetLimitedDirections.length) {
			var limitedDirectionLabels = topologyBudgetLimitedDirections.map(function(direction) {
				return direction === 'download' ? _('download') : _('upload');
			});
			nodes.push(E('div', {
				'class': 'alert-message warning',
				'style': 'margin:8px 0'
			}, _('The conservative traffic budget could not safely fund another unshaped comparison for: %s. No raw result was inferred: the fully verified shaped proposal was kept and runtime was restored. Applying requires explicit confirmation; rerun Full raw with a larger traffic allowance if you want to retry the bypass comparison.').format(limitedDirectionLabels.join(', '))));
		}
		nodes.push(
			E('div', { 'style': 'display:flex;flex-wrap:wrap;gap:8px;margin-top:8px' }, optionCards),
			E('div', { 'style': 'margin-top:10px;padding:9px;border:1px solid rgba(127,127,127,.35);border-radius:5px' }, acknowledgementNodes),
			E('p', { 'style': 'margin:9px 0' }, disabledFallback ?
				_('Apply writes only the disabled autorate section, leaves the entire SQM package unchanged, and proves that no selected controller or managed qdisc exists. A timeout has an unknown outcome; retrying this exact option is safe and idempotent.') :
				_('Apply writes UCI, restarts only the selected instance, and verifies the resulting CAKE/SQM topology. A timeout has an unknown outcome; retrying this exact option is safe and idempotent.')),
			E('div', { 'style': 'display:flex;flex-wrap:wrap;gap:8px' }, actionButtons)
		);
		if (state.error)
			nodes.push(E('div', { 'class': 'alert-message error', 'style': 'margin-top:8px' }, cakeUi.text(state.error)));
		replaceNodeContent(root, nodes);
	};

	render();
	return root;
}

function canonicalAutotuneProfile(value) {
	switch (value) {
	case 'gaming':
		return 'gaming';
	case 'gaming-extreme':
	case 'extreme_gaming':
	case 'gaming_extreme':
		return 'gaming_extreme';
	case 'balanced':
	case 'best-overall':
	case 'best_overall':
		return 'best_overall';
	case 'variable':
	case 'variable-link':
	case 'variable_link':
		return 'variable_link';
	case 'fair':
		return 'fair';
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
			description: _('Starts from the highest measured candidate and reduces it only after an adverse loaded test. Finds the highest throughput that still proves A+; no fixed percentage is subtracted up front. Retaining 70% is only the Auto-Apply objective, while falling below the separate 50% historical-throughput trust boundary forces explicit manual review. If A+ is unattainable, Review offers the best measured grade at its fastest safe rate. Uses diffserv4, supports optional native outbound rules, and preserves ingress DSCP.')
		},
		{
			id: 'gaming_extreme',
			hidden: true,
			title: _('Gaming · Extreme A+'),
			target: _('Opt-in deep A+ search · manual-only below 70% retention'),
			description: _('Starts at the measured upper bound and explores only wide links below the ordinary Gaming boundary when A+ was not reached, down to a capacity-aware 25% floor. Every proposed minimum is a tested CAKE rate; no fixed initial haircut is applied. This mode is not recommended for continuous household use.')
		},
		{
			id: 'best_overall',
			title: _('Best overall'),
			target: _('Target A or better · under 30 ms'),
			description: _('Starts from the highest measured candidate and reduces it only when a loaded test proves that necessary. Finds the highest throughput that still proves A; no fixed percentage is subtracted up front. Retaining 80% is only the Auto-Apply objective, while falling below the separate 50% historical-throughput trust boundary forces explicit manual review. If A is unattainable, Review offers the best balanced safe candidate. Optional outbound rules use diffserv4 while download stays best effort.')
		},
		{
			id: 'variable_link',
			title: _('Variable link'),
			target: _('Measured knee · target B or better · under 60 ms'),
			description: _('For 4G/5G, satellite, wireless, and other changing links. Starts from measured raw capacity without a fixed haircut. Its medium-specific 35–50% value is only the deepest allowed exploration boundary; a runtime minimum is written only when two consecutive reductions prove a latency plateau. Retaining 70% is only the Auto-Apply objective; noisy or uncontrolled results require Review.')
		},
		{
			id: 'fair',
			title: _('Fair'),
			target: _('Throughput first · aim for C or better · under 200 ms'),
			description: _('Starts from the highest measured candidate and maximizes safe throughput without a fixed initial haircut. Retaining 90% is only the Auto-Apply objective and falling below the separate 50% historical-throughput trust boundary forces explicit manual review. Rating is the secondary tie-breaker and C is a soft target. Optional outbound rules use diffserv4, download stays best effort, and Review may offer evidence-backed directional or full SQM bypass.')
		}
	];
}

function visibleAutotuneProfile(value) {
	return canonicalAutotuneProfile(value) === 'gaming_extreme' ?
		'gaming' : canonicalAutotuneProfile(value);
}

function autotuneRunProfile(state) {
	var profile = visibleAutotuneProfile(state && state.autotune_profile) || 'best_overall';
	return profile === 'gaming' && state && state.autotune_extreme_a_plus === true ?
		'gaming_extreme' : profile;
}

function autotuneHasTrustedCapacityReferences(state) {
	var dl = parsePositiveRate(state && state.throughput_reference_dl_p50_kbps);
	var ul = parsePositiveRate(state && state.throughput_reference_ul_p50_kbps);

	return dl != null && dl > 0 && ul != null && ul > 0;
}

function autotuneCalibrationStrategy(state) {
	var strategy = state && state.autotune_calibration_strategy;
	if (strategy === 'reuse_trusted' && !autotuneHasTrustedCapacityReferences(state))
		return 'shaped_only';
	return [ 'shaped_only', 'full_raw', 'reuse_trusted' ].indexOf(strategy) >= 0 ?
		strategy : 'shaped_only';
}

function autotuneAccessRequest(state, bootstrapRequired) {
	var dlCap = String(state && state.service_dl_cap_kbps || '');
	var ulCap = String(state && state.service_ul_cap_kbps || '');
	var capsValid = validatePositiveInteger(dlCap) && validatePositiveInteger(ulCap) &&
		Number(dlCap) >= 100 && Number(dlCap) <= 100000000 &&
		Number(ulCap) >= 100 && Number(ulCap) <= 100000000;
	if (bootstrapRequired === true && !capsValid)
		throw new Error(_('Creating a native Auto-Tune instance requires download and upload service caps between 100 and 100000000 kbit/s. They are hard search ceilings, not measured or proposed rates.'));

	if (visibleAutotuneProfile(state && state.autotune_profile) !== 'variable_link') {
		return {
			medium: 'unknown', source: 'legacy_default', confidence_percent: 0,
			policy: '',
			service_dl_cap_kbps: bootstrapRequired === true ? dlCap : '',
			service_ul_cap_kbps: bootstrapRequired === true ? ulCap : ''
		};
	}
	var access = resolvedAccessContext(state);
	var policy = canonicalCapacityLearningPolicy(state.capacity_learning_policy) ||
		recommendedCapacityLearningPolicy(access);
	if (policy === 'fixed_cap' &&
	    !capsValid)
		throw new Error(_('Explicit fixed-cap learning requires download and upload service caps between 100 and 100000000 kbit/s.'));
	return {
		medium: access.medium,
		source: access.source,
		confidence_percent: access.confidence_percent,
		policy: policy,
		service_dl_cap_kbps: dlCap,
		service_ul_cap_kbps: ulCap
	};
}

function autotuneCalibrationStrategyControl(state, disabled, onChange, bootstrapRequired) {
	var reuseAvailable = autotuneHasTrustedCapacityReferences(state);
	var descriptions = {
		shaped_only: _('Keeps managed CAKE active while measuring. Safest default; it searches only inside capacity the current bounds can demonstrate.'),
		full_raw: _('Temporarily bypasses only the direction being measured, under the recovery watchdog. This can consume more traffic and briefly removes shaping for that direction.'),
		reuse_trusted: reuseAvailable ?
			_('Revalidates the currently trusted/configured bounds without opening a raw-capacity path. It does not claim a new physical line rate.') :
			_('Requires saved DL and UL P50 capacity references from a completed calibration. Run Shaped only or Full raw capacity first and apply its proposal.')
	};
	var selected = bootstrapRequired === true ? 'full_raw' : autotuneCalibrationStrategy(state);
	if (bootstrapRequired === true)
		state.autotune_calibration_strategy = 'full_raw';
	if (state && state.autotune_calibration_strategy === 'reuse_trusted' && !reuseAvailable)
		state.autotune_calibration_strategy = selected;
	var help = E('div', { 'style': 'margin-top:6px;color:var(--text-color-medium,#777)' }, descriptions[selected]);
	var select = E('select', {
		'class': 'cbi-input-select',
		'disabled': disabled ? 'disabled' : null,
		'change': function(ev) {
			state.autotune_calibration_strategy = ev.currentTarget.value;
			help.textContent = descriptions[state.autotune_calibration_strategy];
			if (onChange)
				onChange();
		}
	}, [
		E('option', {
			'value': 'shaped_only',
			'selected': selected === 'shaped_only' ? 'selected' : null,
			'disabled': bootstrapRequired === true ? 'disabled' : null
		}, _('Shaped only (recommended)')),
		E('option', { 'value': 'full_raw', 'selected': selected === 'full_raw' ? 'selected' : null },
			bootstrapRequired === true ? _('Full raw capacity (required for a new instance)') : _('Full raw capacity')),
		E('option', {
			'value': 'reuse_trusted',
			'selected': selected === 'reuse_trusted' ? 'selected' : null,
			'disabled': reuseAvailable && bootstrapRequired !== true ? null : 'disabled'
		}, reuseAvailable ? _('Reuse current trusted bounds') :
			_('Reuse current trusted bounds (requires prior calibration)'))
	]);
	return E('div', {}, [ select, help ]);
}

function nativeBootstrapCapacityControl(state, existingInstance, disabled, onChange) {
	if (existingInstance === true)
		return E('div', { 'style': 'display:none' });

	var dlCap = wizardTextInput(state.service_dl_cap_kbps || '',
		'and(uinteger,min(100),max(100000000))');
	var ulCap = wizardTextInput(state.service_ul_cap_kbps || '',
		'and(uinteger,min(100),max(100000000))');
	dlCap.disabled = disabled;
	ulCap.disabled = disabled;
	var changed = function() {
		state.service_dl_cap_kbps = dlCap.value.trim();
		state.service_ul_cap_kbps = ulCap.value.trim();
		if (onChange)
			onChange();
	};
	dlCap.addEventListener('change', changed);
	ulCap.addEventListener('change', changed);
	dlCap.addEventListener('input', function() {
		state.service_dl_cap_kbps = dlCap.value.trim();
	});
	ulCap.addEventListener('input', function() {
		state.service_ul_cap_kbps = ulCap.value.trim();
	});

	return E('div', {
		'class': 'cake-native-bootstrap-capacity',
		'style': 'margin-top:10px;padding:10px;border:1px solid var(--border-color-medium,#bbb);border-radius:4px'
	}, [
		E('div', { 'class': 'alert-message warning', 'style': 'margin:0 0 10px' }, [
			E('strong', {}, _('New-instance measurement authority. ')),
			_('Enter the service-plan or other defensible hard maximum for both directions. Full Auto-Tune measures raw capacity first and derives every proposed rate from test evidence; these values only bound exploration and can never become measurements by themselves.')
		]),
		wizardField(_('Download service cap'), dlCap, optionDescriptions.service_dl_cap_kbps),
		wizardField(_('Upload service cap'), ulCap, optionDescriptions.service_ul_cap_kbps)
	]);
}

function storedAutotuneProfile(value) {
	return canonicalAutotuneProfile(value) === 'gaming_extreme' ?
		'gaming' : canonicalAutotuneProfile(value);
}

function autotuneExtremeGamingControl(state, disabled, onChange) {
	if (visibleAutotuneProfile(state && state.autotune_profile) !== 'gaming')
		return E('div', { 'style': 'display:none' });
	return E('div', {
		'class': 'alert-message warning',
		'style': 'margin:8px 0 0'
	}, [
		E('label', { 'style': 'display:flex;align-items:flex-start;gap:8px;font-weight:600' }, [
			E('input', {
				'type': 'checkbox',
				'checked': state.autotune_extreme_a_plus === true ? 'checked' : null,
				'disabled': disabled ? 'disabled' : null,
				'change': function(ev) {
					state.autotune_extreme_a_plus = !!ev.currentTarget.checked;
					if (onChange)
						onChange();
				}
			}),
			E('span', {}, _('Enable Extreme A+ search for this run'))
		]),
		E('div', { 'style': 'margin:5px 0 0 25px' },
			_('Wide links may be tested down to a capacity-aware 25% floor only when searching for A+. Results below 70% retention are manual-only and are not recommended for continuous household use. Narrow links keep a higher floor.'))
	]);
}

function autotuneProfileGrid(buttons) {
	return E('div', { 'class': 'cake-autotune-profile-grid' }, [
		E('style', {},
			'.cake-autotune-profile-grid{display:grid;grid-template-columns:repeat(4,minmax(0,1fr));gap:8px;align-items:stretch;width:100%;min-width:0}' +
			'.cake-autotune-profile-card{display:flex!important;flex-direction:column;align-items:flex-start;gap:4px;width:100%;height:100%;min-width:0;min-height:100%;box-sizing:border-box;padding:10px;text-align:left;white-space:normal;word-break:normal;overflow-wrap:break-word;hyphens:none;line-height:1.4}' +
			'@media(max-width:800px){.cake-autotune-profile-grid{grid-template-columns:minmax(0,1fr)}}')
	].concat(buttons));
}

function variableLinkContextControl(state, disabled, onChange) {
	if (visibleAutotuneProfile(state && state.autotune_profile) !== 'variable_link')
		return E('div', { 'style': 'display:none' });

	var access = resolvedAccessContext(state);
	state.access_medium = access.medium;
	state.access_medium_source = access.source;
	state.access_medium_confidence_percent = access.confidence_percent;
	if (!canonicalCapacityLearningPolicy(state.capacity_learning_policy))
		state.capacity_learning_policy = recommendedCapacityLearningPolicy(access);

	var accessSelect = wizardSelectOptions(accessMediumDefinitions(),
		state.access_medium_selection || 'auto');
	accessSelect.disabled = disabled;
	accessSelect.addEventListener('change', function() {
		state.access_medium_selection = accessSelect.value;
		var next = resolvedAccessContext(state);
		state.access_medium = next.medium;
		state.access_medium_source = next.source;
		state.access_medium_confidence_percent = next.confidence_percent;
		if (!state.capacity_learning_policy_touched)
			state.capacity_learning_policy = recommendedCapacityLearningPolicy(next);
		if (onChange)
			onChange();
	});

	var policyDescriptions = {
		verified_only: _('Keep the exact validated ceiling. Runtime may reduce rates for latency, but it will not promote a higher ceiling.'),
		passive_bounded: _('Learn upward only while real sustained traffic proves both clean latency and a measurable throughput gain. No synthetic traffic is generated.'),
		scheduled_active: _('Use passive bounded learning and periodically rerun traffic-generating Full Auto-Tune inside the configured maintenance window and traffic budgets.'),
		fixed_cap: _('Use explicit provider/service-plan caps as hard upper bounds. Both directions are required; the caps may tighten but never expand measured capacity.')
	};
	var policySelect = wizardSelectOptions([
		[ 'verified_only', _('Validated ceiling only (safest)') ],
		[ 'passive_bounded', _('Bounded learning from real traffic') ],
		[ 'scheduled_active', _('Bounded + scheduled active calibration') ],
		[ 'fixed_cap', _('Explicit service hard caps') ]
	], state.capacity_learning_policy);
	policySelect.disabled = disabled;
	policySelect.addEventListener('change', function() {
		state.capacity_learning_policy = policySelect.value;
		state.capacity_learning_policy_touched = true;
		if (onChange)
			onChange();
	});

	var capFields = [];
	if (state.capacity_learning_policy === 'fixed_cap') {
		var dlCap = wizardTextInput(state.service_dl_cap_kbps || '',
			'and(uinteger,min(100),max(100000000))');
		var ulCap = wizardTextInput(state.service_ul_cap_kbps || '',
			'and(uinteger,min(100),max(100000000))');
		dlCap.disabled = disabled;
		ulCap.disabled = disabled;
		dlCap.addEventListener('input', function() { state.service_dl_cap_kbps = dlCap.value; });
		ulCap.addEventListener('input', function() { state.service_ul_cap_kbps = ulCap.value; });
		capFields.push(
			wizardField(_('Download service cap'), dlCap, optionDescriptions.service_dl_cap_kbps),
			wizardField(_('Upload service cap'), ulCap, optionDescriptions.service_ul_cap_kbps)
		);
	}

	var detectionTone = access.source === 'auto_inconclusive' ? 'warning' : 'notice';
	var explorationPercent = accessMediumExplorationPercent(access.medium);
	var children = [
		E('div', { 'class': 'alert-message ' + detectionTone, 'style': 'margin:0 0 10px' }, [
			E('strong', {}, _('Resolved access: %s · confidence %d%%. ').format(
				accessMediumTitle(access.medium), access.confidence_percent)),
			cakeUi.text(access.reason),
			E('div', { 'style': 'margin-top:5px' },
				_('Exploration floor: %d%% of the conservative raw reference. The runtime minimum is written only at an exact tested CAKE point.').format(explorationPercent))
		]),
		wizardField(_('Access medium'), accessSelect, optionDescriptions.access_medium_selection),
		wizardField(_('Capacity learning'), E('div', {}, [
			policySelect,
			E('div', { 'class': 'cbi-value-description', 'style': 'margin-top:6px' },
				policyDescriptions[state.capacity_learning_policy])
		]), optionDescriptions.capacity_learning_policy)
	].concat(capFields);

	if (state.capacity_learning_policy === 'scheduled_active')
		children.push(E('div', { 'class': 'alert-message warning', 'style': 'margin-top:8px' },
			_('Traffic warning: scheduled active calibration performs repeated download and upload controls. It remains review-only unless Auto-Apply is enabled separately, and is bounded by the saved daily/monthly traffic budgets.')));

	return E('div', {
		'class': 'cake-variable-link-context',
		'style': 'margin-top:10px;padding:10px;border:1px solid var(--border-color-medium,#bbb);border-radius:4px'
	}, [ E('h4', { 'style': 'margin:0 0 8px' }, _('Variable Link setup')) ].concat(children));
}

function autotuneGradeForDelta(delta) {
	delta = autotuneNumber(delta);
	if (delta == null || delta < 0)
		return null;
	return delta < 5 ? 'A+' : (delta < 30 ? 'A' : (delta < 60 ? 'B' :
		(delta < 200 ? 'C' : (delta < 400 ? 'D' : 'F'))));
}

function autotuneJobDelay(delayMs) {
	return new Promise(function(resolve) {
		window.setTimeout(resolve, delayMs);
	});
}

function autotuneTransientRpcError(error) {
	var message = String(error && (error.message || error) || '');
	return /XHR request timed out|request timed out|network error|failed to fetch|connection (?:closed|reset)/i.test(message);
}

function autotuneExecWithRetry(command, args, attempts, delayMs) {
	return fs.exec(command, args).catch(function(error) {
		if (!autotuneTransientRpcError(error) || attempts <= 0)
			throw error;
		return autotuneJobDelay(delayMs).then(function() {
			return autotuneExecWithRetry(command, args, attempts - 1,
				Math.min(delayMs * 2, 5000));
		});
	});
}

function autotuneReadResultWithRetry(args, attempts, delayMs) {
	return cakeUi.readNativeResult(args).catch(function(error) {
		if (!autotuneTransientRpcError(error) || attempts <= 0)
			throw error;
		return autotuneJobDelay(delayMs).then(function() {
			return autotuneReadResultWithRetry(args, attempts - 1, Math.min(delayMs * 2, 5000));
		});
	});
}

function nativeEffectiveSpeedtestBackend(backend) {
	return backend === 'auto' || backend === 'speedtest-go' ? 'speedtest-go' : null;
}

function nativeSpeedtestCapabilityValidated(summary, configuredBackend, existingInstance) {
	var existing = existingInstance !== false;
	return !!(summary && summary.state !== 'recovery_required' &&
		summary.protocol_version === NATIVE_AUTOTUNE_PROTOCOL_VERSION &&
		summary.admission_enabled === true && summary.native_speedtest === true &&
		(existing || summary.native_bootstrap_speedtest === true) &&
		(configuredBackend !== 'auto' || summary.native_speedtest_auto_backend === true) &&
		summary.native_operation_status_identity_version === 1 &&
		summary.native_public_result_version === NATIVE_AUTOTUNE_PUBLIC_SCHEMA_VERSION);
}

function nativeSpeedtestIntentSupported(backend, routeMode, existingInstance, topology,
		plannedSqmSection) {
	if (nativeEffectiveSpeedtestBackend(backend) == null ||
	    (routeMode !== 'main' && routeMode !== 'mwan3'))
		return false;
	if (existingInstance === true)
		return true;
	return topology === 'unshaped' && /^[A-Za-z0-9_]+$/.test(plannedSqmSection || '');
}

function nativeSpeedtestLaunchArgs(section_id, wan, routeMode, mwan3Member, serverId, topology,
		existingInstance, plannedSqmSection) {
	var bootstrap = existingInstance === false;
	var args = [ '--calibrationctl', bootstrap ? 'speedtest-bootstrap-start' : 'speedtest-start' ];
	if (bootstrap)
		args.push(plannedSqmSection);
	args.push(
		'--instance', section_id,
		'--expected-target', wan,
		'--backend', 'speedtest-go',
		'--direction', 'both',
		'--topology', topology,
		'--route-mode', routeMode);
	if (routeMode === 'mwan3')
		args.push('--mwan3-member', mwan3Member || '');
	if (String(serverId || '').match(/^[1-9][0-9]*$/))
		args.push('--server-id', String(serverId));
	return args;
}

function nativeSpeedtestResultValidated(result, publicJobId, topology) {
	var unshaped = topology === 'unshaped';
	return !!(result && result.state === 'complete' && result.job_id === publicJobId &&
		result.backend === 'speedtest-go' && result.calibration === topology &&
		result.shaper_bypassed === unshaped && result.runtime_mutated === false &&
		result.runtime_restored === unshaped && result.limits_changed === false &&
		(Number(result.download_kbps) > 0 || Number(result.upload_kbps) > 0));
}

function nativeOperationWorkerRunId(status, previousWorkerRunId, requirePublished) {
	var candidate = status && status.worker_run_id;
	if (candidate == null)
		return previousWorkerRunId == null && requirePublished !== true ? null : undefined;
	if (!/^[0-9a-f]{32}$/.test(candidate) ||
	    (previousWorkerRunId != null && candidate !== previousWorkerRunId))
		return undefined;
	return candidate;
}

function nativeSpeedtestStatusMatchesRequest(status, publicJobId, section_id, wan, routeMode,
		mwan3Member, serverId, topology, existingInstance, plannedSqmSection) {
	var expectedMember = routeMode === 'mwan3' ? (mwan3Member || '') : null;
	var expectedServer = String(serverId || '').match(/^[1-9][0-9]*$/) ? String(serverId) : null;
	var bootstrap = existingInstance === false;
	return !!status && status.job_id === publicJobId && status.operation === 'speedtest' &&
		status.instance === section_id && status.request_identity_schema_version === 1 &&
		status.target_interface === wan && status.backend === 'speedtest-go' &&
		status.speedtest_direction === 'both' && status.speedtest_topology === topology &&
		status.speedtest_server_id === expectedServer && status.route_mode === routeMode &&
		status.mwan3_member === expectedMember &&
		status.target_state === (bootstrap ? 'absent_bootstrap' : 'existing_managed') &&
		status.managed_sqm_section === (bootstrap ? plannedSqmSection : null) &&
		status.origin === 'luci';
}

function currentActiveNativeSpeedtestJob(section_id, wan, routeMode, mwan3Member, serverId,
		topology, existingInstance, plannedSqmSection) {
	if (existingInstance === false)
		return Promise.resolve(null);
	return fs.exec(NATIVE_AUTOTUNE_COMMAND,
		[ '--calibrationctl', 'speedtest-current', section_id ]).then(parseExecJson).then(function(status) {
		if (status.error)
			throw new Error(status.error);
		if (status.state === 'idle')
			return null;
		if (!/^[0-9a-f]{32}$/.test(status.job_id || ''))
			throw new Error(_('The measurement service returned an invalid current Speed Test job ID.'));
		if ([ 'queued', 'starting', 'running', 'cancelling', 'recovering' ].indexOf(status.state) >= 0) {
			if (!nativeSpeedtestStatusMatchesRequest(status, status.job_id, section_id, wan,
					routeMode, mwan3Member, serverId, topology, existingInstance,
					plannedSqmSection)) {
				var mismatch = new Error(_('A different Speed Test request is already active for this instance. Wait for it to finish in the session that started it.'));
				mismatch.speedtestActiveRequestMismatch = true;
				throw mismatch;
			}
			return status;
		}
		return null;
	});
}

function runNativeSpeedtestJob(section_id, wan, onProgress, routeMode, mwan3Member, serverId,
		topology, existingInstance, plannedSqmSection) {
	var launchArgs = nativeSpeedtestLaunchArgs(section_id, wan, routeMode, mwan3Member, serverId,
		topology, existingInstance, plannedSqmSection);
	var publicJobId;
	var workerRunId = null;
	var startAttempted = false;

	var poll = function() {
		return speedtestJobDelay().then(function() {
			return autotuneExecWithRetry(NATIVE_AUTOTUNE_COMMAND,
				[ '--calibrationctl', 'speedtest-status', publicJobId ], 3, 1000);
		}).then(parseExecJson).then(function(status) {
			if (!nativeSpeedtestStatusMatchesRequest(status, publicJobId, section_id, wan,
					routeMode, mwan3Member, serverId, topology, existingInstance,
					plannedSqmSection))
				throw new Error(_('The measurement service changed the Speed Test request identity.'));
			workerRunId = nativeOperationWorkerRunId(status, workerRunId,
				status.state === 'completed');
			if (workerRunId === undefined)
				throw new Error(_('The measurement service changed the Speed Test worker identity.'));
			if ([ 'queued', 'starting', 'running', 'cancelling', 'recovering' ].indexOf(status.state) >= 0) {
				if (onProgress)
					onProgress(status);
				return poll();
			}
			if (status.state !== 'completed')
				throw new Error(status.diagnostic || status.error ||
					_('Speed Test ended without a usable result.'));
			return autotuneReadResultWithRetry(
				[ '--calibrationctl', 'speedtest-result', publicJobId ], 2, 1000)
				.then(function(result) {
					if (!nativeSpeedtestResultValidated(result, publicJobId, topology))
						throw new Error(_('The native Speed Test result failed its restore-first contract.'));
					return { stdout: JSON.stringify(result) };
				});
		});
	};

	var attach = function(status) {
		if (!/^[0-9a-f]{32}$/.test(status.job_id || ''))
			throw new Error(_('The measurement service returned no valid Speed Test job ID.'));
		publicJobId = status.job_id;
		if (!nativeSpeedtestStatusMatchesRequest(status, publicJobId, section_id, wan, routeMode,
				mwan3Member, serverId, topology, existingInstance, plannedSqmSection))
			throw new Error(_('The measurement service returned a Speed Test job for a different request.'));
		workerRunId = nativeOperationWorkerRunId(status, workerRunId, false);
		if (workerRunId === undefined)
			throw new Error(_('The measurement service returned an invalid Speed Test worker identity.'));
		if (onProgress)
			onProgress(status);
		return poll();
	};

	return currentActiveNativeSpeedtestJob(section_id, wan, routeMode, mwan3Member, serverId,
		topology, existingInstance, plannedSqmSection).then(function(current) {
		if (current)
			return attach(current);
		startAttempted = true;
		return fs.exec(NATIVE_AUTOTUNE_COMMAND, launchArgs).then(parseExecJson).then(function(started) {
			if (started.error)
				throw new Error(started.error);
			return attach(started);
		});
	}).catch(function(error) {
		/* Admission may have succeeded even if the RPC response was lost. Never
		 * replay the same traffic through the legacy helper after this point. */
		if (startAttempted)
			error.nativeSpeedtestStartAttempted = true;
		throw error;
	});
}

function runSpeedtestJob(section_id, wan, backend, onProgress, routeMode, mwan3Member,
		serverId, existingInstance, topology, plannedSqmSection) {
	var mode = routeMode || 'main';
	topology = topology || 'current';
	if (!nativeSpeedtestIntentSupported(backend, mode, existingInstance === true, topology,
			plannedSqmSection))
		return Promise.reject(new Error(_('This Speed Test request is not supported by the native measurement service. No fallback measurement was started.')));

	return nativeAutotuneSummary().then(function(summary) {
		if (!nativeSpeedtestCapabilityValidated(summary, backend, existingInstance === true))
			throw new Error(_('Speed Test is unavailable or has an incompatible native protocol. No fallback measurement was started.'));
		if (topology !== 'current' && topology !== 'unshaped')
			throw new Error(_('Unsupported native Speed Test topology.'));
		return runNativeSpeedtestJob(section_id, wan, onProgress, mode, mwan3Member, serverId,
			topology, existingInstance === true, plannedSqmSection);
	});
}

function nativeAutotuneCapabilityValidated(summary, configuredBackend) {
	return !!(summary && summary.state !== 'recovery_required' &&
		summary.protocol_version === NATIVE_AUTOTUNE_PROTOCOL_VERSION &&
		summary.admission_enabled === true && summary.native_full_autotune === true &&
		(configuredBackend !== 'auto' || summary.native_autotune_auto_backend === true) &&
		summary.native_operation_status_identity_version === 1 &&
		summary.native_autotune_status_identity_version === 1 &&
		summary.native_public_result_version === NATIVE_AUTOTUNE_PUBLIC_SCHEMA_VERSION);
}

function nativeBootstrapAutotuneCapabilityValidated(summary, configuredBackend) {
	return nativeAutotuneCapabilityValidated(summary, configuredBackend) &&
		summary.native_bootstrap_autotune === true;
}

function nativeAutotuneCoordinatorRecognized(summary) {
	return !!(summary &&
		summary.protocol_version === NATIVE_AUTOTUNE_PROTOCOL_VERSION &&
		summary.native_full_autotune === true &&
		summary.native_public_result_version === NATIVE_AUTOTUNE_PUBLIC_SCHEMA_VERSION);
}

function nativeAutotuneSummary() {
	return autotuneExecWithRetry(NATIVE_AUTOTUNE_COMMAND,
		[ '--calibrationctl', 'summary' ], 2, 500).then(parseExecJson).catch(function() {
			/* A missing or malformed native summary is an unavailable service,
			 * never authority to guess capabilities or start another executor. */
			return null;
		});
}

function nativeAutotuneIntentSupported(backend, routeMode, existingInstance,
		calibrationStrategy, accessRequest) {
	var routeSupported = !routeMode || routeMode === 'main' || routeMode === 'mwan3';
	if (nativeEffectiveSpeedtestBackend(backend) == null || !routeSupported)
		return false;
	if (existingInstance === true)
		return true;
	var dlCap = accessRequest && String(accessRequest.service_dl_cap_kbps || '');
	var ulCap = accessRequest && String(accessRequest.service_ul_cap_kbps || '');
	return calibrationStrategy === 'full_raw' && validatePositiveInteger(dlCap) &&
		validatePositiveInteger(ulCap) && Number(dlCap) >= 100 && Number(dlCap) <= 100000000 &&
		Number(ulCap) >= 100 && Number(ulCap) <= 100000000;
}

function nativeAutotuneLaunchArgs(section_id, wan, backend, routeMode, mwan3Member,
		profile, conservative, calibrationStrategy, accessRequest, existingInstance,
		plannedSqmSection) {
	var mode = routeMode || 'main';
	var bootstrap = existingInstance !== true;
	var nativeBackend = nativeEffectiveSpeedtestBackend(backend);
	if (nativeBackend == null)
		throw new Error(_('Unsupported native speed-test backend.'));
	if (bootstrap && !/^[A-Za-z0-9_]+$/.test(plannedSqmSection || ''))
		throw new Error(_('New-instance calibration requires an exact planned SQM section.'));
	var args = [ '--calibrationctl', bootstrap ? 'autotune-bootstrap-start' : 'autotune-start' ];
	if (bootstrap)
		args.push(plannedSqmSection);
	args.push(
		'--instance', section_id,
		'--expected-target', wan,
		'--backend', nativeBackend,
		'--route-mode', mode,
		'--profile', profile,
		'--strategy', calibrationStrategy,
		'--access-medium', accessRequest.medium || 'unknown',
		'--access-source', accessRequest.source || 'legacy_default',
		'--access-confidence-percent', String(accessRequest.confidence_percent || 0),
		'--capacity-learning-policy', accessRequest.policy || 'verified_only',
		'--traffic-budget-bytes', String(NATIVE_AUTOTUNE_INTERACTIVE_TRAFFIC_BUDGET_BYTES));

	if (mode === 'mwan3')
		args.push('--mwan3-member', mwan3Member || '');
	if (accessRequest.service_dl_cap_kbps)
		args.push('--service-dl-cap-kbps', String(accessRequest.service_dl_cap_kbps));
	if (accessRequest.service_ul_cap_kbps)
		args.push('--service-ul-cap-kbps', String(accessRequest.service_ul_cap_kbps));
	if (calibrationStrategy === 'full_raw')
		args.push('--allow-sqm-disable');
	if (conservative)
		args.push('--allow-active-traffic');
	return args;
}

function nativeAutotuneProgressStepLabel(step) {
	var labels = {
		waiting_for_slot: _('Waiting for the calibration slot...'),
		starting_calibration: _('Starting Full Auto-Tune...'),
		preparing_route: _('Checking the selected route and current settings...'),
		measuring_idle_latency: _('Preparing the test connection and measuring idle latency...'),
		preparing_measurements: _('Preparing controlled measurements...'),
		measuring_download_without_sqm: _('Measuring download without SQM...'),
		measuring_upload_without_sqm: _('Measuring upload without SQM...'),
		measuring_full_capacity_download: _('Measuring full download capacity...'),
		measuring_full_capacity_upload: _('Measuring full upload capacity...'),
		measuring_shaped_download: _('Measuring shaped download...'),
		measuring_shaped_upload: _('Measuring shaped upload...'),
		preparing_search: _('Preparing the rate search...'),
		searching_download_limit: _('Searching for the best download limit...'),
		searching_upload_limit: _('Searching for the best upload limit...'),
		confirming_candidate: _('Confirming a candidate configuration...'),
		confirming_mobile_download_bypass: _('Checking download without shaping while upload shaping stays active...'),
		comparing_download_without_sqm: _('Comparing download without SQM...'),
		comparing_upload_without_sqm: _('Comparing upload without SQM...'),
		preparing_raw_proposal: _('Preparing the best unshaped alternative...'),
		restoring_settings: _('Restoring the previous runtime settings...'),
		preparing_diagnostics: _('Preparing the calibration diagnostics...'),
		preparing_proposals: _('Preparing verified proposals...'),
		proposals_ready: _('Proposals are ready for review.')
	};
	return labels[step] || null;
}

function nativeAutotuneProgress(status, previousPercent) {
	var state = status && status.state || 'running';
	var terminalReady = state === 'review_ready' || state === 'completed';
	var fallback = state === 'queued' ? 1 : state === 'starting' ? 2 :
		(state === 'recovering' || state === 'cancelling') ? 96 : 3;
	var validSchema = status && status.progress_schema_version === 1;
	var backendPercent = validSchema && typeof status.progress_percent === 'number' &&
		Number.isInteger(status.progress_percent) && status.progress_percent >= 1 &&
		status.progress_percent <= 100 ? status.progress_percent : null;
	var step = validSchema && typeof status.progress_step === 'string' &&
		nativeAutotuneProgressStepLabel(status.progress_step) ? status.progress_step : null;
	var progress = terminalReady ? 100 : Math.min(99, backendPercent == null ? fallback : backendPercent);
	if (!terminalReady && Number.isInteger(previousPercent))
		progress = Math.max(Math.min(99, previousPercent), progress);

	var message = step ? nativeAutotuneProgressStepLabel(step) :
		state === 'queued' ? _('Waiting for the calibration slot...') :
		(state === 'recovering' || state === 'cancelling') ?
			_('Restoring the previous runtime settings...') :
			_('Full Auto-Tune is working; detailed progress is temporarily unavailable.');
	var completed = validSchema && Number.isInteger(status.progress_completed_units) &&
		status.progress_completed_units >= 0 ? status.progress_completed_units : null;
	var total = validSchema && Number.isInteger(status.progress_total_units) &&
		status.progress_total_units > 0 ? status.progress_total_units : null;
	var attempt = validSchema && Number.isInteger(status.progress_attempt) &&
		status.progress_attempt > 0 ? status.progress_attempt : null;
	if (completed != null && total != null && completed <= total)
		message += ' ' + _('Completed %d of %d.').format(completed, total);
	else if (attempt != null)
		message += ' ' + _('Attempt %d.').format(attempt);

	return {
		state: state,
		phase: step || 'calibration_progress_unavailable',
		progress: progress,
		message: message
	};
}

function nativeAutotuneResultMatchesRequest(result, publicJobId, section_id, wan,
		routeMode, mwan3Member, profile, calibrationStrategy, workerRunId) {
	var mode = routeMode || 'main';
	var expectedMember = mode === 'mwan3' ? (mwan3Member || '') : null;
	return !!result && result.native_job_id === publicJobId && result.job_id === section_id &&
		result.target_interface === wan && result.route_mode === mode &&
		result.mwan3_member === expectedMember && result.profile === profile &&
		result.calibration_strategy === calibrationStrategy &&
		(workerRunId == null || result.run_id === workerRunId);
}

function nativeAutotuneStatusMatchesRequest(status, section_id, wan, routeMode,
		mwan3Member, profile, calibrationStrategy, backend, existingInstance,
		plannedSqmSection) {
	var mode = routeMode || 'main';
	var expectedMember = mode === 'mwan3' ? (mwan3Member || '') : null;
	var expectedTargetState = existingInstance === true ? 'existing_managed' : 'absent_bootstrap';
	return !!status && status.request_identity_schema_version === 1 &&
		status.operation === 'full_autotune' && status.instance === section_id &&
		status.target_interface === wan && status.backend === backend &&
		status.speedtest_direction === null && status.speedtest_topology === null &&
		status.route_mode === mode &&
		status.mwan3_member === expectedMember && status.profile === profile &&
		status.calibration_strategy === calibrationStrategy &&
		status.target_state === expectedTargetState &&
		status.managed_sqm_section === plannedSqmSection && status.origin === 'luci';
}

function nativeAutotuneApplyCheckValidated(confirmation, result, option) {
	var expectedAcknowledgements = option.required_acknowledgements || [];
	var expectedTargetState = result && result._native_target_state;
	var disabled = option && option.action === 'disable_sqm';
	var targetSchema = confirmation && confirmation.target_state === 'existing_managed' ?
		(disabled ? 5 : (result && result.native_public_schema_version === 5 &&
			option && option.option_id === 'bypass_download' ? 6 :
			(result && result.native_public_schema_version === 6 &&
			 option && option.option_id === 'capacity_only_shaped' ? 9 : 4))) :
		(confirmation && confirmation.target_state === 'absent_bootstrap' ?
			(disabled ? 8 : (result && result.native_public_schema_version === 6 &&
			 option && option.option_id === 'capacity_only_shaped' ? 10 : 7)) : null);
	return !!confirmation && confirmation.state === 'confirmation_ready' &&
		confirmation.apply_enabled === true && confirmation.validation_only === false &&
		confirmation.runtime_attested === true && typeof confirmation.already_applied === 'boolean' &&
		confirmation.job_id === result.native_job_id && confirmation.worker_run_id === result.run_id &&
		confirmation.option_id === option.option_id &&
		confirmation.review_sha256 === result.source_review_sha256 &&
		confirmation.source_manifest_sha256 === option.manifest_sha256 &&
		(!expectedTargetState || confirmation.target_state === expectedTargetState) &&
		targetSchema != null && confirmation.manifest_schema_version === targetSchema &&
		/^[0-9a-f]{64}$/.test(confirmation.manifest_sha256 || '') &&
		Array.isArray(confirmation.required_acknowledgements) &&
		confirmation.required_acknowledgements.length === expectedAcknowledgements.length &&
		confirmation.required_acknowledgements.every(function(code, index) {
			return code === expectedAcknowledgements[index];
		});
}

function nativeAutotuneApplyReceiptValidated(receipt, result, option, confirmation) {
	var expectedAcknowledgements = option.required_acknowledgements || [];
	return !!receipt && [ 'applied', 'already_applied' ].indexOf(receipt.state) >= 0 &&
		receipt.configuration_written === true && receipt.recovery_cleared === true &&
		receipt.job_id === result.native_job_id && receipt.worker_run_id === result.run_id &&
		receipt.option_id === option.option_id &&
		receipt.review_sha256 === result.source_review_sha256 &&
		receipt.source_manifest_sha256 === option.manifest_sha256 &&
		!!confirmation && receipt.manifest_sha256 === confirmation.manifest_sha256 &&
		receipt.manifest_schema_version === confirmation.manifest_schema_version &&
		receipt.target_state === confirmation.target_state &&
		Array.isArray(receipt.acknowledged) &&
		receipt.acknowledged.length === expectedAcknowledgements.length &&
		receipt.acknowledged.every(function(code, index) {
			return code === expectedAcknowledgements[index];
		});
}

function nativeAutotuneApplyHandleValidated(handle, result, option) {
	return !!handle && handle.state === 'accepted' &&
		/^[0-9a-f]{32}$/.test(handle.apply_job_id || '') &&
		/^[0-9a-f]{64}$/.test(handle.apply_job_token || '') &&
		Number.isSafeInteger(handle.generation) && handle.generation >= 1 &&
		handle.job_id === result.native_job_id &&
		handle.option_id === option.option_id;
}

function nativeAutotuneApplyStatusValidated(status, handle, result, option) {
	var active = [ 'accepted', 'validating', 'applying' ].indexOf(status && status.state) >= 0;
	var terminal = [ 'applied', 'already_applied', 'rolled_back', 'failed' ]
		.indexOf(status && status.state) >= 0;
	return !!status && (active || terminal) &&
		status.terminal === terminal &&
		Number.isSafeInteger(status.generation) && status.generation >= handle.generation &&
		status.apply_job_id === handle.apply_job_id &&
		status.job_id === result.native_job_id &&
		status.option_id === option.option_id &&
		(!terminal || typeof status.recovery_cleared === 'boolean');
}

function nativeAutotuneApplyRetryableRpcError(error) {
	var message = String(error && (error.message || error) || '');
	return autotuneTransientRpcError(error) ||
		// LuCI reports a reset XHR this way; only exact-idempotent Apply may replay it.
		message === 'XHR request aborted by browser' ||
		/unable to read calibration control response:.*(?:Resource temporarily unavailable|os error 11|operation would block)/i.test(message) ||
		/calibration service returned (?:no JSON result|malformed JSON)/i.test(message);
}

function runNativeAutotuneApplyControl(args, retries) {
	retries = retries == null ? NATIVE_AUTOTUNE_APPLY_RPC_RETRIES : retries;
	var timeout = args[1] === 'autotune-apply-watch' ? 35 : 10;
	return withRpcTimeout(timeout, function() {
		return fs.exec(NATIVE_AUTOTUNE_COMMAND, args);
	}).then(parseExecJson).catch(function(error) {
		if (!nativeAutotuneApplyRetryableRpcError(error) || retries <= 0)
			throw error;
		return runNativeAutotuneApplyControl(args, retries - 1);
	});
}

function fetchNativeAutotuneApplyResult(handle, status, result, option, confirmation) {
	return runNativeAutotuneApplyControl(
		[ '--calibrationctl', 'autotune-apply-result',
			handle.apply_job_id, handle.apply_job_token ],
		NATIVE_AUTOTUNE_APPLY_RPC_RETRIES + 3).then(function(receipt) {
		if (receipt.error)
			throw new Error(receipt.error);
		if ([ 'applied', 'already_applied' ].indexOf(status.state) < 0)
			throw new Error(_('The selected configuration was not applied.'));
		if (!nativeAutotuneApplyReceiptValidated(receipt, result, option, confirmation))
			throw new Error(_('The Apply receipt failed its identity and digest contract.'));
		return receipt;
	});
}

function waitForNativeAutotuneApply(handle, current, result, option, confirmation, watches) {
	watches = watches || 0;
	var observedGeneration = current.generation;
	return runNativeAutotuneApplyControl(
		[ '--calibrationctl', 'autotune-apply-watch',
			handle.apply_job_id, handle.apply_job_token, String(observedGeneration) ], 0)
		.then(function(status) {
		if (status.error)
			throw new Error(status.error);
		if (!nativeAutotuneApplyStatusValidated(status, handle, result, option))
			throw new Error(_('The Apply status failed its job identity contract.'));
		if (status.generation < observedGeneration)
			throw new Error(_('The Apply status generation moved backwards.'));
		if (status.terminal === true)
			return fetchNativeAutotuneApplyResult(handle, status, result, option, confirmation);
		if (watches >= NATIVE_AUTOTUNE_APPLY_MAX_WATCH_RESPONSES)
			throw new Error(_('Applying the selected configuration did not finish before the safety watchdog expired.'));
		return waitForNativeAutotuneApply(
			handle, status, result, option, confirmation, watches + 1);
	}, function(error) {
		if (!nativeAutotuneApplyRetryableRpcError(error) ||
		    watches >= NATIVE_AUTOTUNE_APPLY_MAX_WATCH_RESPONSES)
			throw error;
		return waitForNativeAutotuneApply(
			handle, current, result, option, confirmation, watches + 1);
	});
}

function runNativeAutotuneApplyCheck(result, option) {
	return withRpcTimeout(180, function() {
		return fs.exec(NATIVE_AUTOTUNE_COMMAND,
			[ '--calibrationctl', 'autotune-apply-check', result.native_job_id,
				option.option_id ]);
	}).then(parseExecJson).then(function(confirmation) {
		if (confirmation.error)
			throw new Error(confirmation.error);
		if (!nativeAutotuneApplyCheckValidated(confirmation, result, option))
			throw new Error(_('The native Apply check failed its identity and digest contract.'));
		return confirmation;
	});
}

function runNativeAutotuneApply(result, option) {
	return runNativeAutotuneApplyCheck(result, option).then(function(confirmation) {
		var args = [ '--calibrationctl', 'autotune-apply-start', result.native_job_id,
			option.option_id, result.source_review_sha256, confirmation.manifest_sha256 ];
		(option.required_acknowledgements || []).forEach(function(code) {
			args.push('--ack', code);
		});
		return runNativeAutotuneApplyControl(args).then(function(handle) {
			if (handle.error)
				throw new Error(handle.error);
			if (!nativeAutotuneApplyHandleValidated(handle, result, option))
				throw new Error(_('The Apply start response failed its job identity contract.'));
			return waitForNativeAutotuneApply(
				handle, handle, result, option, confirmation, 0);
		});
	});
}

function bindNativeAutotuneTargetState(result, existingInstance) {
	Object.defineProperty(result, '_native_target_state', {
		value: existingInstance === true ? 'existing_managed' : 'absent_bootstrap',
		enumerable: false
	});
	return result;
}

function currentActiveNativeAutotuneJob(section_id, existingInstance, wan, routeMode,
		mwan3Member, profile, calibrationStrategy, backend, plannedSqmSection) {
	if (existingInstance !== true)
		return Promise.resolve(null);
	return fs.exec(NATIVE_AUTOTUNE_COMMAND,
		[ '--calibrationctl', 'autotune-current', section_id ]).then(parseExecJson).then(function(status) {
		if (status.error)
			throw new Error(status.error);
		if (status.state === 'idle') {
			if (status.instance !== section_id)
				throw new Error(_('The calibration service returned an invalid idle identity.'));
			return null;
		}
		if (!/^[0-9a-f]{32}$/.test(status.job_id || '') || status.instance !== section_id)
			throw new Error(_('The calibration service returned an invalid current job identity.'));
		if ([ 'queued', 'starting', 'running', 'cancelling', 'recovering' ].indexOf(status.state) >= 0) {
			if (!nativeAutotuneStatusMatchesRequest(status, section_id, wan, routeMode,
					mwan3Member, profile, calibrationStrategy, backend, existingInstance,
					plannedSqmSection)) {
				var conflict = new Error(_('A different Full Auto-Tune request is already active for this instance. Wait for it to finish or cancel it from the session that started it.'));
				conflict.autotuneActiveRequestMismatch = true;
				throw conflict;
			}
			return { status: status, result: null };
		}
		if (status.state === 'review_ready' && status.runtime_mutated !== true &&
		    status.recovery_required !== true) {
			/* This function is called only after an explicit Start/Run again click.
			 * A completed Review is historical output, not an in-flight operation to
			 * resume.  Let admission create a fresh job; otherwise Run again merely
			 * reopens the old proposals without performing any measurements. */
			return null;
		}
		if (status.state !== 'review_ready' || status.runtime_mutated === true ||
		    status.recovery_required === true)
			throw new Error(_('The calibration service returned an unsafe current Review state.'));
	});
}

function runNativeAutotuneJob(section_id, wan, backend, onProgress, routeMode, mwan3Member,
		profile, conservative, calibrationStrategy, accessRequest, existingInstance,
		plannedSqmSection) {
	backend = nativeEffectiveSpeedtestBackend(backend);
	if (backend == null)
		return Promise.reject(new Error(_('Unsupported native speed-test backend.')));
	var launchArgs = nativeAutotuneLaunchArgs(section_id, wan, backend, routeMode,
		mwan3Member, profile, conservative, calibrationStrategy, accessRequest,
		existingInstance, plannedSqmSection);
	var publicJobId;
	var workerRunId = null;
	var lastProgress = 0;
	var startAttempted = false;

	var poll = function() {
		return autotuneJobDelay(1000).then(function() {
			return autotuneExecWithRetry(NATIVE_AUTOTUNE_COMMAND,
				[ '--calibrationctl', 'autotune-status', publicJobId ], 3, 1000);
		}).then(parseExecJson).then(function(status) {
			if (status.error)
				throw new Error(status.error);
			if (status.job_id !== publicJobId)
				throw new Error(_('The calibration service changed the job identity.'));
			if (!nativeAutotuneStatusMatchesRequest(status, section_id, wan, routeMode,
					mwan3Member, profile, calibrationStrategy, backend, existingInstance,
					plannedSqmSection)) {
				delete nativeAutotuneJobs[section_id];
				var changed = new Error(_('The active calibration no longer matches this request.'));
				changed.autotuneActiveRequestMismatch = true;
				throw changed;
			}
			workerRunId = nativeOperationWorkerRunId(status, workerRunId,
				status.state === 'review_ready' || status.state === 'completed');
			if (workerRunId === undefined) {
				delete nativeAutotuneJobs[section_id];
				throw new Error(_('The calibration service changed the worker identity.'));
			}
			if (nativeAutotuneJobs[section_id])
				nativeAutotuneJobs[section_id].worker_run_id = workerRunId;
			if ([ 'queued', 'starting', 'running', 'cancelling', 'recovering' ].indexOf(status.state) >= 0) {
				if (onProgress) {
					var projected = nativeAutotuneProgress(status, lastProgress);
					lastProgress = projected.progress;
					onProgress(projected);
				}
				return poll();
			}
			if (status.state === 'review_ready' || status.state === 'completed') {
				if (onProgress)
					onProgress(nativeAutotuneProgress(status, lastProgress));
				return withRpcTimeout(180, function() {
					return autotuneReadResultWithRetry(
						[ '--calibrationctl', 'autotune-result', publicJobId ], 2, 1000);
				}).then(function(result) {
					if (!nativeAutotunePublicResultValidated(result)) {
						delete nativeAutotuneJobs[section_id];
						var invalid = new Error(_('The calibration result failed its verification contract.'));
						invalid.autotuneRejectedResult = result;
						throw invalid;
					}
					if (!nativeAutotuneResultMatchesRequest(result, publicJobId, section_id, wan,
							routeMode, mwan3Member, profile, calibrationStrategy, workerRunId)) {
						delete nativeAutotuneJobs[section_id];
						var mismatched = new Error(_('The calibration result no longer matches this request.'));
						mismatched.autotuneRejectedResult = result;
						throw mismatched;
					}
					delete nativeAutotuneJobs[section_id];
					return bindNativeAutotuneTargetState(result, existingInstance);
				});
			}

			delete nativeAutotuneJobs[section_id];
			var terminal = new Error(status.diagnostic || status.error ||
				_('Full Auto-Tune ended without a reviewable result.'));
			terminal.autotuneResult = status;
			throw terminal;
		});
	};

	var attach = function(status) {
		publicJobId = status.job_id;
		if (!nativeAutotuneStatusMatchesRequest(status, section_id, wan, routeMode,
				mwan3Member, profile, calibrationStrategy, backend, existingInstance,
				plannedSqmSection))
			throw new Error(_('The calibration service returned a job for a different request.'));
		workerRunId = nativeOperationWorkerRunId(status, workerRunId, false);
		if (workerRunId === undefined)
			throw new Error(_('The calibration service returned an invalid worker identity.'));
		nativeAutotuneJobs[section_id] = {
			job_id: publicJobId,
			worker_run_id: workerRunId,
			request: {
				wan: wan,
				route_mode: routeMode,
				mwan3_member: mwan3Member,
				profile: profile,
				calibration_strategy: calibrationStrategy,
				backend: backend,
				existing_instance: existingInstance,
				planned_sqm_section: plannedSqmSection,
			},
		};
		if (onProgress) {
			var projected = nativeAutotuneProgress(status, lastProgress);
			lastProgress = projected.progress;
			onProgress(projected);
		}
		return poll();
	};

	return currentActiveNativeAutotuneJob(section_id, existingInstance, wan, routeMode,
		mwan3Member, profile, calibrationStrategy, backend, plannedSqmSection).then(function(current) {
		if (current)
			return attach(current.status);

		startAttempted = true;
		return fs.exec(NATIVE_AUTOTUNE_COMMAND, launchArgs).then(parseExecJson).then(function(started) {
			if (started.error)
				throw new Error(started.error);
			if (!/^[0-9a-f]{32}$/.test(started.job_id || ''))
				throw new Error(_('The calibration service returned no valid job ID.'));
			return attach(started);
		});
	}).catch(function(error) {
		/* Once a native start was attempted, a timeout is ambiguous and a second
		 * executor must never mutate the same SQM. */
		if (startAttempted)
			error.nativeAutotuneStartAttempted = true;
		throw error;
	});
}

function runPreferredAutotuneJob(section_id, wan, backend, onProgress, routeMode, mwan3Member,
		profile, conservative, calibrationStrategy, accessRequest, existingInstance,
		plannedSqmSection) {
	profile = canonicalAutotuneProfile(profile) || 'best_overall';
	calibrationStrategy = [ 'shaped_only', 'full_raw', 'reuse_trusted' ].indexOf(calibrationStrategy) >= 0 ?
		calibrationStrategy : 'shaped_only';
	accessRequest = accessRequest || autotuneAccessRequest({ autotune_profile: profile });

	if (existingInstance !== true && nativeEffectiveSpeedtestBackend(backend) != null &&
		(!routeMode || routeMode === 'main' || routeMode === 'mwan3') &&
		!nativeAutotuneIntentSupported(backend, routeMode, existingInstance,
			calibrationStrategy, accessRequest)) {
		return Promise.reject(new Error(_('New-instance calibration requires Full raw capacity and explicit download/upload service caps.')));
	}

	if (!nativeAutotuneIntentSupported(backend, routeMode, existingInstance,
		calibrationStrategy, accessRequest)) {
		return Promise.reject(new Error(_('This Full Auto-Tune request is not supported by the native calibration service. No fallback calibration was started.')));
	}

	return nativeAutotuneSummary().then(function(summary) {
		var capabilityReady = existingInstance === true ?
			nativeAutotuneCapabilityValidated(summary, backend) :
			nativeBootstrapAutotuneCapabilityValidated(summary, backend);
		if (!capabilityReady) {
			if (nativeAutotuneCoordinatorRecognized(summary) &&
			    summary.state === 'recovery_required')
				throw new Error(_('Full Auto-Tune is restoring an earlier settings transaction. No second calibration was started.'));
			if (nativeAutotuneCoordinatorRecognized(summary) && summary.admission_enabled !== true)
				throw new Error(_('Full Auto-Tune is temporarily not accepting work. No second calibration was started.'));
			throw new Error(existingInstance === true ?
				_('Full Auto-Tune is unavailable or has an incompatible protocol. No fallback calibration was started.') :
				_('New-instance Full Auto-Tune is unavailable or has an incompatible protocol. No fallback calibration was started.'));
		}
		return runNativeAutotuneJob(section_id, wan, backend, onProgress, routeMode,
			mwan3Member, profile, conservative, calibrationStrategy, accessRequest,
			existingInstance, plannedSqmSection);
	});
}

function cancelNativeAutotuneJob(section_id) {
	var handle = nativeAutotuneJobs[section_id];
	var publicJobId = handle && handle.job_id;
	var request = handle && handle.request;
	if (!/^[0-9a-f]{32}$/.test(publicJobId || ''))
		return Promise.reject(new Error(_('No authenticated calibration handle is available on this page.')));
	if (!request)
		return Promise.reject(new Error(_('No immutable calibration request is available on this page.')));

	return fs.exec(NATIVE_AUTOTUNE_COMMAND,
		[ '--calibrationctl', 'autotune-cancel', publicJobId ]).then(parseExecJson).then(function(cancelled) {
		if (cancelled.error)
			throw new Error(cancelled.error);

		var polls = 0;
		var waitForSettlement = function(status) {
			if (status.job_id !== publicJobId)
				throw new Error(_('The calibration service changed the job identity during cancellation.'));
			if (!nativeAutotuneStatusMatchesRequest(status, section_id, request.wan,
					request.route_mode, request.mwan3_member, request.profile,
					request.calibration_strategy, request.backend,
					request.existing_instance, request.planned_sqm_section))
				throw new Error(_('The calibration service changed the request identity during cancellation.'));
			handle.worker_run_id = nativeOperationWorkerRunId(status, handle.worker_run_id, false);
			if (handle.worker_run_id === undefined)
				throw new Error(_('The calibration service changed the worker identity during cancellation.'));

			if ([ 'cancelled', 'failed', 'review_ready', 'completed' ].indexOf(status.state) >= 0 &&
			    status.runtime_mutated !== true && status.recovery_required !== true) {
				delete nativeAutotuneJobs[section_id];
				return status;
			}

			if (polls++ >= AUTOTUNE_RECOVERY_MAX_POLLS) {
				var pending = new Error(_('Cancellation was requested, but runtime recovery is still pending.'));
				pending.autotuneRecoveryPending = true;
				pending.autotuneRecoveryStatus = status;
				throw pending;
			}

			return autotuneJobDelay(Math.min(1000 * Math.pow(2, Math.min(polls, 3)),
				AUTOTUNE_RECOVERY_MAX_DELAY_MS)).then(function() {
				return autotuneExecWithRetry(NATIVE_AUTOTUNE_COMMAND,
					[ '--calibrationctl', 'autotune-status', publicJobId ], 3, 1000);
			}).then(parseExecJson).then(waitForSettlement);
		};

		return waitForSettlement(cancelled);
	});
}

function cancelPreferredAutotuneJob(section_id, wan, backend, profile, routeMode, mwan3Member) {
	return cancelNativeAutotuneJob(section_id);
}

function runPingerPlan(section_id, mode, routeMode, mwan3Member) {
	return fs.exec('/usr/sbin/cake-autorated', [
		'--pinger-plan',
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

	return fs.exec('/usr/sbin/cake-autorated', [
		'--pinger-plan',
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
	return fs.exec('/usr/sbin/cake-autorated', [
		'--mqtt-status', section_id,
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
	lines.push(_('Native MQTT publisher: %s').format(yesNo(result.installed)));
	lines.push(_('Broker host configured: %s').format(yesNo(result.configured_host)));
	lines.push(_('Log to file: %s').format(yesNo(result.log_to_file)));
	lines.push(_('Summary stats: %s').format(yesNo(result.summary_enabled)));
	lines.push(_('CPU stats: %s').format(yesNo(result.cpu_enabled)));
	lines.push(_('Publish CPU sensors: %s').format(yesNo(result.publish_cpu)));
	lines.push(_('Ready: %s').format(yesNo(result.available)));

	if (result.reason)
		lines.push(_('Status: %s').format(result.reason));

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

function targetInterfaceChoiceOptions(ignoredInstance) {
	return targetInterfaceChoices().map(function(name) {
		var owner = managedTargetOwner(name, ignoredInstance);
		return [ name, owner ? _('%s — already managed by instance "%s"').format(
			targetInterfaceLabel(name), owner) : targetInterfaceLabel(name) ];
	});
}

function defaultWizardTarget(ignoredInstance) {
	var choices = targetInterfaceChoices();
	var free = choices.filter(function(name) {
		return !managedTargetOwner(name, ignoredInstance);
	});
	var preferred = defaultTargetInterface();

	if (free.indexOf(preferred) >= 0)
		return preferred;

	var uplinks = availableMwan3Uplinks(ignoredInstance);
	for (var i = 0; i < uplinks.length; i++)
		if (free.indexOf(uplinks[i].device) >= 0)
			return uplinks[i].device;

	for (i = 0; i < free.length; i++)
		if ((interfaceContext.deviceNetworks[free[i]] || []).length)
			return free[i];

	if (free.length)
		return free[0];
	return choices.length ? choices[0] : '';
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

function writeWizardConfig(section_id, state, allowUncalibrated) {
	var uncalibrated = allowUncalibrated === true;
	if (uncalibrated) {
		if (state.mode !== 'autotune')
			throw new Error(_('Invalid disabled, uncalibrated fallback state.'));
		state.enabled = false;
		state.sqm_enabled = false;
	}

	var wan = normalizeInterfaceName(state.wan_if);
	var dl = rateValue(state.sqm_download, '20000');
	var ul = rateValue(state.sqm_upload, '20000');
	var sqmSection = state.sqm_section || managedSqmSectionName(section_id);
	var pingExtraArgs = state.ping_extra_args || pingerInterfaceArgs(wan, state.pinger_method || 'fping');
	var selectedAutotuneProfile = storedAutotuneProfile(state.autotune_profile);
	var effectiveRouteMode = state.route_mode === 'auto' ?
		(state.mwan3_member ? 'mwan3' : 'main') : (state.route_mode || 'main');
	var effectiveMwan3Member = effectiveRouteMode === 'mwan3' ?
		(state.mwan3_member || '') : '';

	uci.set('cake-autorate', section_id, 'enabled', state.enabled ? '1' : '0');
	uci.set('cake-autorate', section_id, 'wan_if', wan);
	uci.set('cake-autorate', section_id, 'route_mode', effectiveRouteMode);
	if (effectiveMwan3Member)
		uci.set('cake-autorate', section_id, 'mwan3_member', effectiveMwan3Member);
	else
		uci.unset('cake-autorate', section_id, 'mwan3_member');
	uci.unset('cake-autorate', section_id, 'ping_prefix_string');
	uci.set('cake-autorate', section_id, 'auto_interface_preset', '1');
	var sqmDirectionMode = state.sqm_direction_mode || 'both';
	if ([ 'both', 'upload_only', 'download_only' ].indexOf(sqmDirectionMode) < 0)
		throw new Error(_('The selected SQM direction topology is invalid.'));
	uci.set('cake-autorate', section_id, 'sqm_direction_mode', sqmDirectionMode);
	uci.set('cake-autorate', section_id, 'adjust_dl_shaper_rate',
		sqmDirectionMode === 'upload_only' ? '0' : '1');
	uci.set('cake-autorate', section_id, 'adjust_ul_shaper_rate',
		sqmDirectionMode === 'download_only' ? '0' : '1');
	uci.set('cake-autorate', section_id, 'manage_sqm', '1');
	uci.set('cake-autorate', section_id, 'sqm_section', sqmSection);
	uci.set('cake-autorate', section_id, 'sqm_enabled', state.enabled ? '1' : '0');
	uci.set('cake-autorate', section_id, 'autotune_profile',
		selectedAutotuneProfile || 'best_overall');
	uci.set('cake-autorate', section_id, 'autotune_calibration_strategy',
		autotuneCalibrationStrategy(state));
	var learningPolicy = canonicalCapacityLearningPolicy(state.capacity_learning_policy) ||
		'verified_only';
	uci.set('cake-autorate', section_id, 'capacity_learning_policy', learningPolicy);
	uci.set('cake-autorate', section_id, 'runtime_learning_mode',
		learningPolicy === 'scheduled_active' ? 'periodic_active' :
			(learningPolicy === 'passive_bounded' ? 'passive' : 'fixed'));
	uci.set('cake-autorate', section_id, 'adaptive_ceiling_enabled',
		(learningPolicy === 'passive_bounded' || learningPolicy === 'scheduled_active') ? '1' : '0');
	uci.set('cake-autorate', section_id, 'scheduled_autotune_enabled',
		learningPolicy === 'scheduled_active' ? '1' : '0');
	if (state.is_new_instance) {
		uci.set('cake-autorate', section_id, 'traffic_profile', 'auto');
		uci.set('cake-autorate', section_id, 'traffic_rules_enabled', '0');
	}
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

function autotuneConservativeAvailable(result) {
	if (!result || result.background_blocked !== true || result.retryable !== true ||
	    result.conservative_available === false)
		return false;
	/* Background-aware continuation always starts a fresh measurement job.  It
	 * therefore re-measures an idle baseline instead of reusing or inventing the
	 * failed strict attempt. Technical baseline failures set availability false. */
	return true;
}

var AUTOTUNE_APPLY_TIMEOUT_S = 30;
var callUciConfirmStatus = rpc.declare({
	object: 'uci',
	method: 'confirm',
	reject: false
});
var callUciRevertStatus = rpc.declare({
	object: 'uci',
	method: 'revert',
	params: [ 'config' ],
	reject: false
});

function reloadViewPage() {
	if (!window.location)
		return;
	var pageUrl = typeof window.location === 'string' ? window.location : window.location.href;
	var cleanUrl = pageUrl ? pageUrl.split('#')[0] : null;
	if (!cleanUrl)
		return;
	if (typeof window.location.replace === 'function')
		window.location.replace(cleanUrl);
	else
		window.location = cleanUrl;
}

function discardStagedUciPackages(packages) {
	packages = packages || [];
	return Promise.all(packages.map(function(config) {
		return callUciRevertStatus(config).then(function(status) {
			return { config: config, status: status };
		}, function(error) {
			return { config: config, error: error };
		});
	})).then(function(results) {
		var configs = results.filter(function(result) {
			return !result.error && result.status === 0;
		}).map(function(result) {
			return result.config;
		});
		var failed = results.filter(function(result) {
			return result.error || result.status !== 0;
		});

		/* Even a partially successful server cleanup must invalidate the matching
		 * local cache entries. Waiting for every request above avoids a split-brain
		 * cache when one package fails after another was already reverted. */
		if (typeof uci.unload === 'function' && configs.length)
			uci.unload(configs);
		if (failed.length) {
			var names = failed.map(function(result) { return result.config; }).join(', ');
			var firstError = failed[0].error;
			throw new Error(_('UCI could not discard the staged transaction for: %s.%s').format(
				names, firstError && firstError.message ? ' ' + firstError.message : ''));
		}
		return configs;
	});
}

function changedUciPackages(changes) {
	return Object.keys(changes || {}).filter(function(config) {
		return changes[config] && changes[config].length;
	});
}

function requireCleanUciTransaction(message) {
	return uci.changes().then(function(changes) {
		var packages = changedUciPackages(changes);
		if (packages.length)
			throw new Error(message || _('Apply or revert the existing pending changes before creating a Multi-WAN set.'));
	});
}

function applyPlainRollbackTransaction(allowedPackages) {
	var applyStarted = false;

	return uci.save().then(function() {
		return uci.changes();
	}).then(function(changes) {
		var packages = changedUciPackages(changes);
		if (!packages.length)
			return false;
		if (packages.some(function(config) { return allowedPackages.indexOf(config) < 0; }))
			throw new Error(_('The Multi-WAN transaction contains unrelated pending changes.'));
		applyStarted = true;
		return uci.callApply(AUTOTUNE_APPLY_TIMEOUT_S, true).then(function(status) {
			if (status !== 0)
				throw new Error(_('UCI rejected the rollback-enabled Multi-WAN configuration apply.'));
			return callUciConfirmStatus();
		}).then(function(status) {
			if (status !== 0)
				throw new Error(_('UCI could not confirm the Multi-WAN configuration transaction.'));
			applyStarted = false;
			return true;
		});
	}).catch(function(error) {
		/* Never confirm an uncertain apply. rpcd's timeout remains authoritative;
		 * discard the client cache so it cannot recommit the same transaction. */
		if (applyStarted)
			reloadViewPage();
		throw error;
	});
}

function clearAutotuneProposalState(state) {
	state.autotune_running = false;
	state.autotune_progress = 0;
	state.autotune_background_block = null;
	state.autotune_batch = null;
	state.autotune_batch_index = 0;
	state.autotune_active_plan = null;
	state.autotune_cancel_requested = false;
	state.autotune_cancelled = false;
	state.native_autotune_skipped = false;
}

function multiwanAutotuneItemNativeApplied(item) {
	if (item && item.native_apply_receipt) {
		var receipt = item.native_apply_receipt;
		var result = item.diagnostics || item.state && item.state.autotune_diagnostics;
		var expectedManifestSchema = result && result.native_public_schema_version === 4 ? 8 :
			(result && result.native_public_schema_version === 6 ? 10 : 7);
		return !!result && nativeAutotunePublicResultValidated(result) &&
			(!result._native_target_state || result._native_target_state === 'absent_bootstrap') &&
			[ 'applied', 'already_applied' ].indexOf(receipt.state) >= 0 &&
			receipt.configuration_written === true && receipt.recovery_cleared === true &&
			receipt.target_state === 'absent_bootstrap' &&
			receipt.manifest_schema_version === expectedManifestSchema &&
			receipt.job_id === result.native_job_id && receipt.worker_run_id === result.run_id;
	}
	return false;
}

function multiwanAutotuneItemAccepted(item) {
	if (!(item && item.decision === 'accepted'))
		return false;
	return multiwanAutotuneItemNativeApplied(item);
}

function multiwanAutotunePendingPlans(plans, items) {
	var applied = Object.create(null);
	(items || []).forEach(function(item) {
		if (item && item.plan && multiwanAutotuneItemNativeApplied(item))
			applied[item.plan.name] = true;
	});
	return (plans || []).filter(function(plan) {
		return !(plan && applied[plan.name] === true);
	});
}

function multiwanAutotuneItemDecided(item) {
	return multiwanAutotuneItemAccepted(item) ||
		!!(item && item.decision === 'skipped' && item.uncalibrated === true);
}

function multiwanAutotuneBatchDecided(items) {
	return Array.isArray(items) && items.length > 0 &&
		items.every(multiwanAutotuneItemDecided);
}

function multiwanAutotuneItemCanSkip(item, running) {
	return !!item && running !== true && item.recovery_pending !== true;
}

function recordAutotuneTerminalFailure(state, result, message) {
	clearAutotuneProposalState(state);
	state.autotune_diagnostics = result && Object.keys(result).length &&
		!nativeAutotunePublicResultValidated(result) ? result : {
		state: 'failed',
		error: message || _('Full Auto-Tune failed.'),
		configuration_written: false
	};
	state.autotune_failure_message = message || _('Full Auto-Tune failed.');

	return state;
}

function autotuneTypedTerminalDiagnostic(result) {
	if (!(result && result.terminal_state === 'inconclusive'))
		return null;
	if (result.diagnostic_code === 'pair-options-unreviewable') {
		return {
			code: result.diagnostic_code,
			message: _('Every measured shaped pair was outside the manual safety boundary. The previous runtime settings were restored and no proposal was applied.')
		};
	}
	if (result.diagnostic_code === 'search-options-unreviewable') {
		return {
			code: result.diagnostic_code,
			message: _('The shaped rate search reached its evidence boundary without finding a reviewable point. The previous runtime settings were restored and no proposal was applied.')
		};
	}
	return null;
}

function autotuneRetryableInconclusive(result) {
	return !!(result && result.state === 'inconclusive' && result.retryable === true) ||
		!!autotuneTypedTerminalDiagnostic(result);
}

function autotuneMeasurementTimeout(result) {
	return !!(autotuneRetryableInconclusive(result) &&
		result.reason === 'speedtest-timeout' &&
		result.search_state === 'measurement_timeout');
}

function recordAutotuneRetryableInconclusive(state, result) {
	clearAutotuneProposalState(state);
	state.autotune_diagnostics = result;
	state.autotune_failure_message = '';
	state.autotune_background_block = result && result.background_blocked ? result : null;

	return state;
}

function renderAutotuneDiagnostics(result) {
	if (nativeAutotunePublicResultValidated(result))
		return renderNativeAutotuneDiagnostics(result);

	var terminal = autotuneTypedTerminalDiagnostic(result);
	var message = terminal ? terminal.message :
		(result && (result.diagnostic || result.error) ||
			_('Full Auto-Tune ended without a reviewable result.'));
	var code = terminal ? terminal.code :
		(result && (result.diagnostic_code || result.reason || result.state) || 'failed');
	return E('div', { 'class': 'alert-message error' }, [
		E('strong', {}, _('Calibration did not produce an applicable proposal.')),
		E('p', { 'style': 'white-space:normal;margin:6px 0 0' }, cakeUi.text(message)),
		E('p', { 'style': 'white-space:normal;margin:6px 0 0;font-size:12px' },
			cakeUi.text(_('Diagnostic code: %s').format(code)))
	]);
}
function replaceNodeContent(node, children) {
	var replacements = Array.isArray(children) ? children : (children ? [ children ] : []);

	/* Removing a focused wizard input may synchronously dispatch its blur/change
	 * handler, which can render this same node again.  A hand-written
	 * firstChild/removeChild loop then resumes with a child already removed by
	 * the nested render and throws NotFoundError.  The native replacement is one
	 * DOM mutation operation and remains well-defined across that re-entrancy. */
	if (typeof node.replaceChildren === 'function') {
		node.replaceChildren.apply(node, replacements);
		return;
	}

	while (node.firstChild)
	{
		var child = node.firstChild;
		try {
			node.removeChild(child);
		}
		catch (error) {
			/* Old engines without replaceChildren may still dispatch a nested
			 * render from removeChild().  Ignore only the proven already-removed
			 * child; every other DOM error remains fatal. */
			if (!error || error.name !== 'NotFoundError' || child.parentNode === node)
				throw error;
		}
	}

	for (var i = 0; i < replacements.length; i++)
		node.appendChild(replacements[i]);
}

function showCreateWizard(grid, name, existingName) {
	var rerun = !!existingName;
	if (rerun)
		name = existingName;
	var defaultWan = defaultWizardTarget(rerun ? existingName : null);
	var defaultMembers = availableMwan3Uplinks(rerun ? existingName : null).filter(function(member) {
		return member.device === defaultWan;
	});
	var defaultMember = defaultMembers.length ? defaultMembers[0].name : '';
	var state = {
		name: name,
		step: rerun ? 1 : 0,
		mode: 'autotune',
		wan_if: defaultWan,
		route_mode: defaultMember ? 'mwan3' : 'main',
		mwan3_member: defaultMember,
		route_selection: defaultMember ? 'mwan3:' + defaultMember : 'main',
		multiwan_enabled: !!defaultMember,
		multiwan_set: false,
		autotune_batch_index: 0,
		native_autotune_skipped: false,
		enabled: true,
		sqm_enabled: true,
		sqm_direction_mode: 'both',
		autotune_profile: 'best_overall',
		autotune_calibration_strategy: rerun ? 'shaped_only' : 'full_raw',
		access_medium_selection: 'auto',
		access_medium: 'unknown',
		access_medium_source: 'auto_inconclusive',
		access_medium_confidence_percent: 20,
		capacity_learning_policy: '',
		capacity_learning_policy_touched: false,
		service_dl_cap_kbps: '',
		service_ul_cap_kbps: '',
		throughput_reference_dl_p50_kbps: '',
		throughput_reference_ul_p50_kbps: '',
		autotune_extreme_a_plus: false,
		speedtest_backend: rerun ? 'auto' : 'speedtest-go',
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
		state.multiwan_enabled = !!state.mwan3_member &&
			(state.route_mode === 'mwan3' || state.route_mode === 'auto');
		state.enabled = uci.get('cake-autorate', existingName, 'enabled') === '1';
		state.sqm_enabled = uci.get('cake-autorate', existingName, 'sqm_enabled') === '1';
		state.sqm_direction_mode = uci.get('cake-autorate', existingName,
			'sqm_direction_mode') || 'both';
		state.autotune_profile = canonicalAutotuneProfile(
			uci.get('cake-autorate', existingName, 'autotune_profile')) || 'best_overall';
		state.autotune_calibration_strategy = uci.get('cake-autorate', existingName,
			'autotune_calibration_strategy') || 'shaped_only';
		state.access_medium_selection = uci.get('cake-autorate', existingName,
			'access_medium_selection') ||
			(uci.get('cake-autorate', existingName, 'access_medium_source') === 'user_selected' ?
				uci.get('cake-autorate', existingName, 'access_medium') : 'auto');
		state.access_medium = uci.get('cake-autorate', existingName, 'access_medium') || 'unknown';
		state.access_medium_source = uci.get('cake-autorate', existingName,
			'access_medium_source') || 'legacy_default';
		state.access_medium_confidence_percent = parseInt(uci.get('cake-autorate', existingName,
			'access_medium_confidence_percent') || '0', 10);
		state.capacity_learning_policy = canonicalCapacityLearningPolicy(
			uci.get('cake-autorate', existingName, 'capacity_learning_policy')) || 'verified_only';
		state.capacity_learning_policy_touched = true;
		state.service_dl_cap_kbps = uci.get('cake-autorate', existingName,
			'service_dl_cap_kbps') || '';
		state.service_ul_cap_kbps = uci.get('cake-autorate', existingName,
			'service_ul_cap_kbps') || '';
		state.throughput_reference_dl_p50_kbps = uci.get('cake-autorate', existingName,
			'throughput_reference_dl_p50_kbps') || '';
		state.throughput_reference_ul_p50_kbps = uci.get('cake-autorate', existingName,
			'throughput_reference_ul_p50_kbps') || '';
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
			state.autotune_diagnostics ? _('Review diagnostics') : _('Review')
		][state.step];
	}

	function renderSteps() {
		var labels = [
			_('Interface'),
			state.mode === 'autotune' ? _('Full Auto-Tune') : _('Speed test'),
			state.autotune_diagnostics ? _('Review diagnostics') : _('Review')
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
		var detectedUplinks = availableMwan3Uplinks(rerun ? existingName : null);
		var target = wizardSelectOptions(
			targetInterfaceChoiceOptions(rerun ? existingName : null), state.wan_if);
		var route = wizardSelectOptions(wizardRouteChoices(rerun ? existingName : null), state.route_selection);
		var enabled = wizardCheckbox(state.enabled);
		var multiwan = wizardCheckbox(state.multiwan_enabled);
		var multiwanAll = wizardCheckbox(state.multiwan_set);
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
					state.autotune_diagnostics = null;
					state.autotune_failure_message = '';
					state.autotune_batch = null;
					state.autotune_cancelled = false;
					state.native_autotune_skipped = false;
					if (state.mode === 'autotune') {
						state.enabled = true;
						state.sqm_enabled = true;
					}
					syncSqmForInterface();
					render();
				}
			}, [
				E('strong', {}, mode[1]),
				E('span', { 'style': 'font-size:12px;white-space:normal;word-break:normal;overflow-wrap:break-word;hyphens:none' }, mode[2])
			]);
		});

		target.addEventListener('change', function() {
			var newWan = normalizeInterfaceName(target.value);
			if (newWan !== state.wan_if) {
				state.autotune_diagnostics = null;
				state.autotune_failure_message = '';
				state.autotune_batch = null;
				state.autotune_cancelled = false;
				state.native_autotune_skipped = false;
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
			state.autotune_diagnostics = null;
			state.autotune_failure_message = '';
			state.autotune_batch = null;
			state.autotune_cancelled = false;
			state.native_autotune_skipped = false;
			state.route_selection = route.value;
			if (route.value.indexOf('mwan3:') === 0) {
				state.multiwan_enabled = true;
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
				state.multiwan_enabled = false;
				state.multiwan_set = false;
			}
			render();
		});

		enabled.addEventListener('change', function() {
			state.enabled = enabled.checked;
			state.sqm_enabled = state.enabled;
		});

		multiwan.addEventListener('change', function() {
			state.autotune_batch = null;
			state.autotune_cancelled = false;
			state.native_autotune_skipped = false;
			state.multiwan_enabled = multiwan.checked;
			if (state.multiwan_enabled) {
				state.enabled = true;
				state.sqm_enabled = true;
				var matching = detectedUplinks.filter(function(member) {
					return member.device === normalizeInterfaceName(state.wan_if);
				});
				var selected = matching.length ? matching[0] : detectedUplinks[0];
				if (selected) {
					state.route_mode = 'mwan3';
					state.mwan3_member = selected.name;
					state.route_selection = 'mwan3:' + selected.name;
					state.wan_if = selected.device;
				}
			} else {
				state.multiwan_set = false;
				state.route_mode = 'main';
				state.mwan3_member = '';
				state.route_selection = 'main';
			}
			render();
		});

		multiwanAll.addEventListener('change', function() {
			state.autotune_batch = null;
			state.autotune_cancelled = false;
			state.native_autotune_skipped = false;
			state.multiwan_set = multiwanAll.checked;
			if (state.multiwan_set) {
				state.multiwan_enabled = true;
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
		if (detectedUplinks.length) {
			var plan = multiwanInstancePlans(state, rerun ? existingName : null).map(function(item) {
				return '%s: %s → %s → %s'.format(item.name, item.member, item.device, item.sqmSection);
			}).join('\n');
			fields.push(wizardField(
				_('Multi-WAN routing'),
				E('div', {}, [
					E('label', { 'style': 'display:block;white-space:normal' }, [
						multiwan, ' ', _('Route calibration through the selected mwan3 member.')
					]),
					E('label', { 'style': !rerun && state.multiwan_enabled && detectedUplinks.length > 1 ?
						'display:block;margin-top:8px;white-space:normal' : 'display:none' }, [
						multiwanAll, ' ', _('Create and calibrate every unused detected uplink sequentially.')
					]),
					E('pre', {
						'style': state.multiwan_set ?
							'white-space:pre-wrap;margin-top:6px' : 'display:none'
					}, plan)
				]),
				_('Each instance owns one L3 device and one CAKE queue. Members already owned by another instance are excluded. Full Auto-Tune keeps the selected mwan3 route and does not change the global default route.')));
		}

		return fields;
	}

	function cloneStateForPlan(plan) {
		var instanceState = {};
		for (var key in state)
			if (Object.prototype.hasOwnProperty.call(state, key) &&
			    key !== 'autotune_batch' && key !== 'autotune_active_plan')
				instanceState[key] = state[key];
		instanceState.name = plan.name;
		instanceState.wan_if = plan.device;
		instanceState.route_mode = 'mwan3';
		instanceState.mwan3_member = plan.member;
		instanceState.route_selection = 'mwan3:' + plan.member;
		instanceState.sqm_section = plan.sqmSection;
		instanceState.ping_extra_args = pingerInterfaceArgs(plan.device,
			instanceState.pinger_method || 'fping');
		return instanceState;
	}

	function batchAutotuneReady() {
		return state.multiwan_set && multiwanAutotuneBatchDecided(state.autotune_batch);
	}

	function autotuneReadyForReview() {
		return state.multiwan_set ? batchAutotuneReady() :
			state.native_autotune_skipped === true;
	}

	function ensureMultiwanAutotuneBatch() {
		if (Array.isArray(state.autotune_batch) && state.autotune_batch.length)
			return state.autotune_batch;

		state.autotune_batch = multiwanInstancePlans(state, rerun ? existingName : null).map(function(plan) {
			return {
				plan: plan,
				state: cloneStateForPlan(plan),
				status: 'profile',
				decision: 'pending',
				uncalibrated: false,
				error: '',
				diagnostics: null,
				recovery_pending: false,
				native_apply_receipt: null,
				native_option_id: ''
			};
		});
		state.autotune_batch_index = 0;
		return state.autotune_batch;
	}

	function resetMultiwanAutotuneItem(item) {
		if (!item || !item.state)
			return;
		item.state.autotune_diagnostics = null;
		item.state.autotune_failure_message = '';
		item.state.autotune_background_block = null;
		item.state.autotune_recovery_pending = null;
		item.state.autotune_cancelled = false;
		item.state.native_autotune_skipped = false;
		item.status = 'profile';
		item.decision = 'pending';
		item.uncalibrated = false;
		item.error = '';
		item.diagnostics = null;
		item.recovery_pending = false;
		item.native_apply_receipt = null;
		item.native_option_id = '';
	}

	function advanceMultiwanAutotuneItem() {
		state.autotune_active_plan = null;
		state.autotune_progress = 0;
		state.autotune_batch_index = (state.autotune_batch_index || 0) + 1;
		if (state.autotune_batch_index >= state.autotune_batch.length)
			state.step = 2;
		render();
	}

	function skipMultiwanAutotuneItem(item) {
		var diagnostics = item.state.autotune_diagnostics ||
			item.diagnostics || null;
		var reason = item.error || (diagnostics && diagnostics.error) ||
			_('Calibration was skipped by the user.');
		resetMultiwanAutotuneItem(item);
		item.diagnostics = diagnostics;
		item.error = reason;
		item.state.enabled = false;
		item.state.sqm_enabled = false;
		item.status = 'skipped';
		item.decision = 'skipped';
		item.uncalibrated = true;
		advanceMultiwanAutotuneItem();
	}

	function renderMultiwanAutotuneStep() {
		var batch = ensureMultiwanAutotuneBatch();
		if (!batch.length) {
			return [ E('div', { 'class': 'alert-message error' },
				_('Every detected mwan3 uplink is already owned by an instance.')) ];
		}

		var index = Math.min(state.autotune_batch_index || 0, batch.length - 1);
		var item = batch[index];
		var itemState = item.state;
		var settledResult = itemState.autotune_diagnostics ||
			item.diagnostics || null;
		var diagnosticsNode = settledResult ?
			(nativeAutotunePublicResultValidated(settledResult) ?
				renderNativeAutotuneDiagnostics(settledResult, function(receipt, option) {
					item.native_apply_receipt = receipt;
					item.native_option_id = option.option_id;
					item.status = 'accepted';
					item.decision = 'accepted';
					item.uncalibrated = false;
					item.error = '';
					return reloadWizardUci().then(function() {
						advanceMultiwanAutotuneItem();
					});
				}) : renderAutotuneDiagnostics(settledResult)) : null;
		var canSkip = multiwanAutotuneItemCanSkip(item, state.autotune_running);
		var statusLabels = {
			profile: _('Choose profile'),
			running: _('Running'),
			review: _('Awaiting decision'),
			diagnostic: _('Calibration Review'),
			accepted: _('Accepted'),
			skipped: _('Skipped'),
			recovery: _('Restoring runtime')
		};
		var timeline = E('div', {
			'style': 'display:flex;flex-wrap:wrap;gap:6px;margin:0 0 14px'
		}, batch.map(function(batchItem, batchIndex) {
			var active = batchIndex === index;
			return E('div', {
				'style': 'flex:1 1 170px;padding:8px;border:1px solid %s;border-radius:4px;white-space:normal'.format(
					active ? '#00b77a' : 'rgba(127,127,127,.4)')
			}, [
				E('strong', {}, _('%d/%d · %s').format(batchIndex + 1, batch.length,
					batchItem.plan.member)),
				E('br'),
				E('span', { 'style': 'font-size:12px' }, _('%s → %s · %s').format(
					batchItem.plan.device,
					batchItem.plan.name,
					statusLabels[batchItem.status] || batchItem.status || _('Pending')))
			]);
		}));

		var profileButtons = autotuneProfileDefinitions().filter(function(definition) {
			return !definition.hidden;
		}).map(function(definition) {
			var selected = visibleAutotuneProfile(itemState.autotune_profile) === definition.id;
			return E('button', {
				'type': 'button',
				'class': 'btn cbi-button cake-autotune-profile-card %s'.format(
					selected ? 'cbi-button-positive' : ''),
				'disabled': state.autotune_running || item.recovery_pending ? 'disabled' : null,
				'data-profile': definition.id,
				'aria-pressed': selected ? 'true' : 'false',
				'click': function(ev) {
					var selectedProfile = canonicalAutotuneProfile(
						ev.currentTarget.getAttribute('data-profile'));
					if (!selectedProfile || selectedProfile === itemState.autotune_profile)
						return;
					resetMultiwanAutotuneItem(item);
					itemState.autotune_profile = selectedProfile;
					itemState.autotune_extreme_a_plus = false;
					render();
				}
			}, [
				E('strong', {}, definition.title),
				E('span', { 'style': 'font-size:12px;font-weight:600' }, definition.target),
				E('span', { 'style': 'font-size:12px' }, definition.description)
			]);
		});

		var progress = E('progress', {
			'max': '100',
			'value': state.autotune_progress || '0',
			'style': 'width:min(620px,100%);' +
				(state.autotune_running ? 'display:block' : 'display:none!important') +
				';margin-top:8px'
		});
		var status = E('div', { 'style': 'margin-top:8px;white-space:normal' },
			cakeUi.text(item.error || (item.status === 'diagnostic' ?
				_('Full Auto-Tune Review is ready. Choose an exact option below, confirm its trade-offs, and apply it; or skip this uplink.') :
			(item.status === 'review' ?
				_('Calibration finished without an applicable proposal. Retry or skip this uplink.') :
				_('Select the quality profile for this uplink, then start its calibration.')))));

			var startCalibration = function(conservative) {
				var generation = (state.autotune_generation || 0) + 1;
				var accessRequest;
				try {
					accessRequest = autotuneAccessRequest(itemState, !rerun);
				}
				catch (error) {
					showError(error.message || String(error));
					return Promise.resolve();
				}
				showError(null);
			if (diagnosticsNode)
				diagnosticsNode.style.display = 'none';
			resetMultiwanAutotuneItem(item);
			state.autotune_generation = generation;
			state.autotune_cancel_requested = false;
			state.autotune_cancelled = false;
			state.native_autotune_skipped = false;
			state.autotune_running = true;
			state.autotune_progress = 0;
			state.autotune_active_plan = item;
			item.status = 'running';
			status.textContent = conservative ?
				_('Starting conservative Full Auto-Tune for %s...').format(item.plan.member) :
				_('Starting Full Auto-Tune for %s...').format(item.plan.member);
			runButton.disabled = true;
			conservativeButton.disabled = true;
			cancelButton.disabled = true;
			for (var profileIndex = 0; profileIndex < profileButtons.length; profileIndex++)
				profileButtons[profileIndex].disabled = true;
			progress.style.display = 'block';
			var decisionNodes = decisionButtons.querySelectorAll('button');
			for (var decisionIndex = 0; decisionIndex < decisionNodes.length; decisionIndex++)
				decisionNodes[decisionIndex].disabled = true;

			return runPreferredAutotuneJob(item.plan.name, item.plan.device,
				itemState.speedtest_backend, function(job) {
					if (generation !== state.autotune_generation || state.autotune_cancel_requested)
						return;
					state.autotune_progress = job.progress || 0;
					cancelButton.disabled = false;
					progress.value = state.autotune_progress;
					status.textContent = _('[%d/%d] %s · %d%% · %s').format(index + 1, batch.length,
						item.plan.member, state.autotune_progress,
						job.message || _('Full Auto-Tune is working...'));
			}, 'mwan3', item.plan.member, autotuneRunProfile(itemState), conservative,
				autotuneCalibrationStrategy(itemState), accessRequest, rerun,
				item.plan.sqmSection)
				.then(function(result) {
					if (generation !== state.autotune_generation || state.autotune_cancel_requested)
						return;
					if (!nativeAutotunePublicResultValidated(result))
						throw new Error(_('The calibration result failed its verification contract.'));
					clearAutotuneProposalState(itemState);
					itemState.autotune_diagnostics = result;
					item.diagnostics = result;
					item.status = 'diagnostic';
					item.error = '';
					state.autotune_running = false;
					state.autotune_active_plan = null;
					state.autotune_progress = 100;
					render();
				}).catch(function(err) {
					if (generation !== state.autotune_generation)
						return;
					if (state.autotune_cancel_requested)
						return;
					state.autotune_running = false;
					state.autotune_active_plan = null;
					state.autotune_progress = 0;
					state.autotune_cancel_requested = false;

					if (err.autotuneRecoveryPending) {
						item.status = 'recovery';
						item.recovery_pending = true;
						item.error = err.message || _('Runtime recovery is still pending.');
						itemState.autotune_recovery_pending = err.autotuneRecoveryStatus || {};
						render();
						return;
					}

					var result = err.autotuneResult || null;
					item.diagnostics = result;
					item.error = err.message || String(err);
					if (autotuneRetryableInconclusive(result))
						recordAutotuneRetryableInconclusive(itemState, result);
					else if (result && result.background_blocked && result.retryable) {
						itemState.autotune_diagnostics = result;
						itemState.autotune_background_block = result;
					}
					else
						recordAutotuneTerminalFailure(itemState, result || {}, item.error);
					item.status = 'review';
					render();
				});
		};

		var runButton = E('button', {
			'type': 'button',
			'class': 'btn cbi-button cbi-button-action',
			'disabled': state.autotune_running || item.recovery_pending ? 'disabled' : null,
			'click': function() { return startCalibration(false); }
		}, autotuneMeasurementTimeout(itemState.autotune_diagnostics) ? _('Retry calibration') :
				(settledResult ? _('Run again') : _('Start Full Auto-Tune')));
		var conservativeButton = E('button', {
			'type': 'button',
			'class': 'btn cbi-button cbi-button-positive',
			'style': autotuneConservativeAvailable(itemState.autotune_background_block) ? '' : 'display:none',
			'disabled': state.autotune_running || item.recovery_pending ? 'disabled' : null,
			'click': function() { return startCalibration(true); }
		}, _('Continue conservatively'));
		var cancelButton = E('button', {
			'type': 'button',
			'class': 'btn cbi-button cbi-button-negative',
			'disabled': state.autotune_running && item.status === 'running' ? null : 'disabled',
			'click': function() {
				state.autotune_cancel_requested = true;
				status.textContent = _('Cancelling and restoring %s...').format(item.plan.member);
				var generation = state.autotune_generation;
				return cancelPreferredAutotuneJob(itemState.name, itemState.wan_if,
					itemState.speedtest_backend, autotuneRunProfile(itemState),
					itemState.route_mode, itemState.mwan3_member).then(function() {
					if (generation !== state.autotune_generation)
						return;
					state.autotune_generation++;
					state.autotune_running = false;
					state.autotune_active_plan = null;
					state.autotune_cancel_requested = false;
					state.autotune_progress = 0;
					item.status = 'review';
					item.recovery_pending = false;
					item.error = _('Calibration cancelled. Previous runtime state was restored.');
					render();
				}).catch(function(err) {
					state.autotune_running = false;
					state.autotune_active_plan = null;
					state.autotune_cancel_requested = false;
					item.status = 'recovery';
					item.recovery_pending = true;
					item.error = err.message || String(err);
					render();
				});
			}
		}, _('Cancel test'));
		var restoreButton = E('button', {
			'type': 'button',
			'class': 'btn cbi-button cbi-button-negative',
			'style': item.recovery_pending ? '' : 'display:none',
			'disabled': state.autotune_running ? 'disabled' : null,
			'click': function() {
				state.autotune_running = true;
				state.autotune_active_plan = item;
				item.status = 'recovery';
				render();
				return cancelPreferredAutotuneJob(itemState.name, itemState.wan_if,
					itemState.speedtest_backend, autotuneRunProfile(itemState),
					itemState.route_mode, itemState.mwan3_member).then(function() {
					state.autotune_running = false;
					state.autotune_active_plan = null;
					item.recovery_pending = false;
					item.status = 'review';
					item.error = _('Runtime was restored. Retry or skip this uplink.');
					render();
				}).catch(function(err) {
					state.autotune_running = false;
					state.autotune_active_plan = null;
					item.error = err.message || String(err);
					render();
				});
			}
		}, _('Restore and check'));

		var decisionButtons = E('div', { 'style': 'display:flex;flex-wrap:wrap;gap:8px;margin-top:12px' }, [
			E('button', {
				'type': 'button',
				'class': 'btn cbi-button',
				'disabled': canSkip ? null : 'disabled',
				'click': function() { skipMultiwanAutotuneItem(item); }
			}, settledResult ? _('Skip this uplink') : _('Skip calibration'))
		]);

		var fields = [
			timeline,
			E('h5', {}, _('%s — %s → %s').format(item.plan.member,
				item.plan.device, item.plan.name)),
			wizardField(_('Calibration strategy for %s').format(item.plan.member),
				autotuneCalibrationStrategyControl(itemState,
					state.autotune_running || item.recovery_pending, function() {
						resetMultiwanAutotuneItem(item);
						render();
					}, !rerun),
				_('This uplink is calibrated independently; raw bypass, when selected, affects only its currently measured direction.')),
			wizardField(_('Calibration profile for %s').format(item.plan.member),
				E('div', {}, [
					autotuneProfileGrid(profileButtons),
					autotuneExtremeGamingControl(itemState,
						state.autotune_running || item.recovery_pending, function() {
							resetMultiwanAutotuneItem(item);
							render();
						}),
					variableLinkContextControl(itemState,
						state.autotune_running || item.recovery_pending, function() {
							resetMultiwanAutotuneItem(item);
							render();
						}),
					nativeBootstrapCapacityControl(itemState, rerun,
						state.autotune_running || item.recovery_pending, function() {
							resetMultiwanAutotuneItem(item);
							render();
						})
				]),
				_('This choice applies only to the current uplink. Other uplinks may use different profiles.')),
			E('div', { 'class': 'alert-message warning' }, [
				E('strong', {}, _('Traffic warning: ')),
				_('Only this mwan3 member is calibrated now. Client traffic is not intentionally interrupted, but concurrent traffic on this uplink can reduce measurement confidence.')
			]),
			wizardField(_('Calibration'), E('div', {}, [
				runButton, ' ', conservativeButton, ' ', cancelButton, ' ', restoreButton,
				progress, status, decisionButtons
			]), _('Each native option is verified and applied only after its Review card is explicitly confirmed.'))
		];
		if (diagnosticsNode)
			fields.push(diagnosticsNode);
		return fields;
	}

	function renderAutotuneStep() {
		if (state.multiwan_set)
			return renderMultiwanAutotuneStep();
		var profileButtons = autotuneProfileDefinitions().filter(function(definition) {
			return !definition.hidden;
		}).map(function(definition) {
			var selected = visibleAutotuneProfile(state.autotune_profile) === definition.id;
			return E('button', {
				'type': 'button',
				'class': 'btn cbi-button cake-autotune-profile-card %s'.format(
					selected ? 'cbi-button-positive' : ''),
				'disabled': state.autotune_running ? 'disabled' : null,
				'data-profile': definition.id,
				'aria-pressed': selected ? 'true' : 'false',
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
					state.autotune_extreme_a_plus = false;
					render();
				}
			}, [
				E('strong', {}, definition.title),
				E('span', { 'style': 'font-size:12px;font-weight:600' }, definition.target),
				E('span', { 'style': 'font-size:12px' }, definition.description)
			]);
		});
		var status = E('div', { 'style': 'margin-top:8px;white-space:normal' },
			state.autotune_cancelled ? _('Calibration cancelled. Previous runtime state was restored.') :
				(state.autotune_diagnostics ?
					(nativeAutotunePublicResultValidated(state.autotune_diagnostics) ?
						_('Full Auto-Tune Review is ready. Choose an exact option and confirm its listed trade-offs below.') :
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
		var diagnosticsNode = state.autotune_diagnostics ?
			(nativeAutotunePublicResultValidated(state.autotune_diagnostics) ?
				renderNativeAutotuneDiagnostics(state.autotune_diagnostics, null,
					!rerun ? function() {
						clearAutotuneProposalState(state);
						state.autotune_diagnostics = null;
						state.native_autotune_skipped = true;
						state.enabled = false;
						state.sqm_enabled = false;
						state.autotune_failure_message = _('Calibration was skipped by the user.');
						state.step = 2;
						render();
					} : null) : renderAutotuneDiagnostics(state.autotune_diagnostics)) : null;
			var startCalibration = function(conservative) {
				var generation = (state.autotune_generation || 0) + 1;
				var accessRequest;
				try {
					accessRequest = autotuneAccessRequest(state, !rerun);
				}
				catch (error) {
					showError(error.message || String(error));
					return Promise.resolve();
				}
				showError(null);
			if (diagnosticsNode)
				diagnosticsNode.style.display = 'none';
			state.autotune_generation = generation;
			state.autotune_cancel_requested = false;
			state.autotune_cancelled = false;
			state.native_autotune_skipped = false;
			state.autotune_diagnostics = null;
			state.autotune_failure_message = '';
			state.autotune_recovery_pending = null;
			state.autotune_running = true;
			state.autotune_progress = 0;
			state.autotune_background_block = null;
			state.autotune_batch = null;
			state.autotune_active_plan = null;
			runButton.disabled = true;
			conservativeButton.style.display = 'none';
			cancelButton.disabled = true;
			status.textContent = conservative ?
				_('Starting conservative Full Auto-Tune...') : _('Starting Full Auto-Tune...');

			var progressCallback = function(job, prefix) {
				if (generation !== state.autotune_generation || state.autotune_cancel_requested)
					return;
				state.autotune_progress = job.progress || 0;
				cancelButton.disabled = false;
				progress.value = state.autotune_progress;
				status.textContent = (prefix || '') + _('%d%% · %s').format(
					state.autotune_progress,
					job.message || _('Full Auto-Tune is working...'));
			};

			return runPreferredAutotuneJob(state.name, state.wan_if, state.speedtest_backend, function(job) {
				progressCallback(job, '');
			}, state.route_mode, state.mwan3_member, autotuneRunProfile(state), conservative,
				autotuneCalibrationStrategy(state), accessRequest, rerun,
				state.sqm_section || managedSqmSectionName(state.name)).then(function(result) {
				if (generation !== state.autotune_generation || state.autotune_cancel_requested)
					return;
				if (!nativeAutotunePublicResultValidated(result))
					throw new Error(_('The calibration result failed its verification contract.'));
				clearAutotuneProposalState(state);
				state.autotune_diagnostics = result;
				state.autotune_failure_message = '';
				state.autotune_running = false;
				state.step = 1;
				render();
			}).catch(function(err) {
				if (generation !== state.autotune_generation || state.autotune_cancel_requested ||
				    (err.autotuneResult && err.autotuneResult.state === 'cancelled'))
					return;
				clearAutotuneProposalState(state);
				state.autotune_diagnostics = null;
				state.autotune_failure_message = '';
				state.autotune_recovery_pending = null;
				progress.value = 0;
				progress.style.display = 'none';
				runButton.disabled = false;
				cancelButton.disabled = true;

				/* Exhausting the bounded recovery poll is not a terminal job
				 * result. Preserve no Review and make that explicit so Apply remains
				 * unavailable. */
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
					if (autotuneConservativeAvailable(result)) {
						conservativeButton.style.display = '';
						showError(_('The strict run stopped before throughput testing. Retry when quiet, continue once with conservative safeguards, or cancel.'));
					} else {
						conservativeButton.style.display = 'none';
						showError(_('A trustworthy idle baseline could not be established. Retry when this uplink is quieter; conservative mode is unavailable for this stage.'));
					}
				} else {
					recordAutotuneTerminalFailure(state, result, err.message || String(err));
					render();
					showError(_('Full Auto-Tune failed: %s').format(state.autotune_failure_message));
				}
			});
		};
		var runButton = E('button', {
			'type': 'button',
			'class': 'btn cbi-button cbi-button-action',
			'disabled': state.autotune_running ? 'disabled' : null,
			'click': function() {
				return startCalibration(false);
			}
		}, autotuneMeasurementTimeout(state.autotune_diagnostics) ? _('Retry calibration') :
			(state.autotune_diagnostics || state.autotune_batch ?
				_('Run again') : _('Start Full Auto-Tune')));
		var conservativeButton = E('button', {
			'type': 'button',
			'class': 'btn cbi-button cbi-button-positive',
			'style': autotuneConservativeAvailable(state.autotune_background_block) ? '' : 'display:none',
			'click': function() { return startCalibration(true); }
		}, _('Continue conservatively'));
		var cancelButton = E('button', {
			'type': 'button',
			'class': 'btn cbi-button cbi-button-negative',
			'disabled': state.autotune_running ? null : 'disabled',
			'click': function() {
				cancelButton.disabled = true;
				status.textContent = _('Cancelling and restoring the previous SQM state...');
				state.autotune_cancel_requested = true;
				var generation = state.autotune_generation;
				var active = state.autotune_active_plan && state.autotune_active_plan.state || state;
				return cancelPreferredAutotuneJob(active.name, active.wan_if, state.speedtest_backend,
					autotuneRunProfile(state), active.route_mode, active.mwan3_member).then(function() {
					if (generation !== state.autotune_generation)
						return;
					state.autotune_generation++;
					state.autotune_running = false;
					state.autotune_active_plan = null;
					state.autotune_batch = null;
					state.autotune_progress = 0;
					state.autotune_cancelled = true;
					state.autotune_cancel_requested = false;
					render();
				}).catch(function(err) {
					state.autotune_cancel_requested = false;
					showError(err.message || String(err));
				});
			}
		}, _('Cancel test'));

		var fields = [
			wizardField(_('Calibration strategy'),
				autotuneCalibrationStrategyControl(state, state.autotune_running, function() {
					clearAutotuneProposalState(state);
					state.autotune_diagnostics = null;
					state.autotune_failure_message = '';
					render();
				}, !rerun),
				_('Full raw capacity is explicit opt-in because it temporarily bypasses shaping for the direction under test and can transfer substantially more data.')),
			wizardField(_('Calibration profile'),
				E('div', {}, [
					autotuneProfileGrid(profileButtons),
					autotuneExtremeGamingControl(state, state.autotune_running, function() {
						clearAutotuneProposalState(state);
						state.autotune_diagnostics = null;
						state.autotune_failure_message = '';
						state.autotune_background_block = null;
						render();
					}),
					variableLinkContextControl(state, state.autotune_running, function() {
						clearAutotuneProposalState(state);
						state.autotune_diagnostics = null;
						state.autotune_failure_message = '';
						state.autotune_background_block = null;
						render();
					}),
					nativeBootstrapCapacityControl(state, rerun, state.autotune_running,
						function() {
							clearAutotuneProposalState(state);
							state.autotune_diagnostics = null;
							state.autotune_failure_message = '';
							state.autotune_background_block = null;
							render();
						})
				]),
				_('The profile chooses how Auto-Tune ranks safe points on the measured throughput/latency boundary. Missed throughput objectives and the 50% historical trust boundary prevent Auto-Apply but remain manually reviewable when latency, loss, routing, and measurement evidence are clean.')),
			E('div', { 'class': 'alert-message warning' }, [
				E('strong', {}, _('Traffic warning: ')),
				_('Full Auto-Tune measures one bidirectional control plus repeated download-only and upload-only controls, then searches each shaped direction independently. A direction may be measured again while Auto-Tune raises or brackets its candidate, but it never lowers the profile capacity floor. Other WAN traffic can reduce confidence, but is never counted as test throughput.')
			]),
			wizardField(_('Calibration'), E('div', {}, [ runButton, ' ', conservativeButton, ' ', cancelButton, progress, status ]),
				_('All intermediate state stays in RAM. No UCI configuration is written until you confirm the Review step.'))
		];

		if (diagnosticsNode)
			fields.push(diagnosticsNode);

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
		var runButton = E('button', {
			'type': 'button',
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
				}, state.route_mode, state.mwan3_member, state.speedtest_go_server_id, rerun,
				'unshaped', state.sqm_section || managedSqmSectionName(state.name)).then(function(res) {
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
			'type': 'button',
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
		var autorateDecision = state.enabled ? _('enabled') : _('disabled');
		var rows = state.multiwan_set ? [
			[ _('Setup'), _('Sequential per-uplink Full Auto-Tune') ],
			[ _('Decision rule'), _('Each uplink has its own profile and explicit Accept or Skip decision.') ]
		] : [
			[ _('Target interface'), targetInterfaceLabel(wan) ],
			[ _('Probe routing'), state.route_mode === 'mwan3' ? _('mwan3 member %s').format(state.mwan3_member) : _('Main routing table') ],
			[ _('Autorate + SQM'), autorateDecision ],
			[ _('SQM queue'), wizardSqmQueueText(state) ],
			[ _('Download speed'), state.sqm_download + ' kbit/s' ],
			[ _('Upload speed'), state.sqm_upload + ' kbit/s' ],
			[ _('Preferred backend'), speedtestBackendChoiceTitle(state.speedtest_backend) ],
			[ _('Pinger plan'), _('%s, %d active / %d candidates').format(state.pinger_method || 'fping', activeCount, reflectors.length) ]
		];
		if (state.native_autotune_skipped === true)
			rows.push([ _('Calibration'), _('Skipped; create the instance disabled and mark it for calibration.') ]);
		if (state.autotune_diagnostics)
			reviewNodes.push(renderAutotuneDiagnostics(state.autotune_diagnostics));
		if (state.multiwan_set) {
			var multiwanPlans = multiwanInstancePlans(state, rerun ? existingName : null);
			var unappliedMultiwanPlans = multiwanAutotunePendingPlans(
				multiwanPlans, state.autotune_batch);
			var multiwanConflicts = wizardPlanConflicts(unappliedMultiwanPlans, state.enabled,
				rerun ? existingName : null);
			rows.push([ _('Multi-WAN instances'), E('pre', {
				'style': 'margin:0;white-space:pre-wrap;font:inherit'
			}, cakeUi.text(multiwanPlans.map(function(item) {
				return '%s: %s → %s; %s'.format(item.name, item.member, item.device, item.sqmSection);
			}).join('\n'))) ]);
			rows.push([ _('Detected conflicts'), multiwanConflicts.length ? multiwanConflicts.join('\n') : _('None') ]);
			(state.autotune_batch || []).forEach(function(item) {
				var profile = autotuneProfileDefinitions().filter(function(definition) {
					return definition.id === item.state.autotune_profile;
				})[0];
				if (multiwanAutotuneItemAccepted(item) && item.native_apply_receipt) {
					rows.push([ _('Result: %s').format(item.plan.member),
						_('APPLIED · %s · option %s · manifest v%d').format(
							profile ? profile.title : item.state.autotune_profile,
							item.native_option_id || item.native_apply_receipt.option_id,
							item.native_apply_receipt.manifest_schema_version) ]);
				}
				else if (item.decision === 'skipped') {
						rows.push([ _('Result: %s').format(item.plan.member),
							_('SKIPPED · %s · the instance will be created disabled and marked for calibration. %s').format(
								profile ? profile.title : item.state.autotune_profile,
								item.error || _('The selected route was unavailable or validation did not complete.')) ]);
					}
				if (item.diagnostics) {
						reviewNodes.push(E('details', {
							'style': 'margin:8px 0;padding:8px;border:1px solid rgba(127,127,127,.35);border-radius:4px'
						}, [
							E('summary', { 'style': 'cursor:pointer;font-weight:600' },
								_('%s diagnostics — %s').format(item.plan.member,
									item.decision === 'accepted' ? _('accepted') : _('skipped'))),
							renderAutotuneDiagnostics(item.diagnostics)
						]));
				}
			});
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
					E('td', { 'class': 'td' }, cakeUi.text(row[0])),
					E('td', { 'class': 'td' }, row[1] instanceof Node ? row[1] : cakeUi.text(row[1]))
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
		if (step === 0) {
			var targetConflicts = wizardSingleTargetConflicts(state,
				rerun ? existingName : null);
			if (targetConflicts.length) {
				showError(targetConflicts.join(' '));
				return false;
			}
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
			if (state.mode === 'autotune' && !autotuneReadyForReview()) {
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
		if (state.autotune_running) {
			showError(_('Wait for the current calibration to stop and restore its runtime state.'));
			return;
		}
		var currentBatchItem = state.multiwan_set && Array.isArray(state.autotune_batch) ?
			state.autotune_batch[state.autotune_batch_index || 0] : null;
		if (currentBatchItem && currentBatchItem.recovery_pending) {
			showError(_('Runtime recovery is still pending for the current uplink.'));
			return;
		}

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
		if (rerun && state.multiwan_set) {
			showError(_('Calibrating every uplink is available only while creating new instances. Re-run calibrates the selected existing instance.'));
			return false;
		}
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
			if (state.mode === 'autotune' && state.multiwan_set && !batchAutotuneReady()) {
				showError(_('Every Multi-WAN plan must have either a validated proposal or an explicit disabled, uncalibrated fallback.'));
				return false;
			}
			var member = mwan3Context.byName[state.mwan3_member];
			if (!member || member.device !== normalizeInterfaceName(state.wan_if)) {
				showError(_('Select an online-configured mwan3 member matching the target interface.'));
				return false;
			}
		}

		var plans = state.multiwan_set ? multiwanInstancePlans(state, rerun ? existingName : null) : [ {
			name: state.name,
			device: normalizeInterfaceName(state.wan_if)
		} ];
		if (state.multiwan_set && state.mode === 'autotune')
			plans = multiwanAutotunePendingPlans(plans, state.autotune_batch);
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

	function stageWizardPlanItem(config_name, item) {
		var plan = item.plan;
		var instanceState = item.state;
		var existingQueue = state.mode === 'autotune' ? null :
			findImportableSqmQueueForInterface(plan.device);

		instanceState.is_new_instance = !rerun;
		if (existingQueue) {
			instanceState.sqm_section = queueSectionName(existingQueue);
			instanceState.sqm_download = rateValue(existingQueue.download, instanceState.sqm_download);
			instanceState.sqm_upload = rateValue(existingQueue.upload, instanceState.sqm_upload);
		}

		var section_id = rerun ? existingName :
			grid.map.data.add(config_name, grid.sectiontype, plan.name);
		writeWizardConfig(section_id, instanceState, item.uncalibrated === true);
		if (item.uncalibrated) {
			uci.set('cake-autorate', section_id, 'enabled', '0');
			uci.set('cake-autorate', section_id, 'sqm_enabled', '0');
			uci.set('cake-autorate', section_id, 'autotune_pending', '1');
		}
		return section_id;
	}

	function reloadWizardUci() {
		return reloadAppliedUciPackages();
	}

	function applySequentialMultiwanPlan(config_name, planItems) {
		var skipped = planItems.filter(function(item) {
			return item.uncalibrated === true;
		});
		var created = planItems.filter(function(item) {
			return multiwanAutotuneItemNativeApplied(item);
		}).map(function(item) { return item.plan.name; });

		return requireCleanUciTransaction().then(function() {
			if (!skipped.length)
				return created;

			for (var i = 0; i < skipped.length; i++)
				created.push(stageWizardPlanItem(config_name, skipped[i]));

			/* Skipped uplinks carry no measured proposal and are deliberately
			 * written disabled. They share no active SQM side effects, so a plain
			 * rollback-enabled cake-autorate transaction is sufficient. */
			return applyPlainRollbackTransaction([ 'cake-autorate' ]).then(function() {
				return reloadWizardUci();
			}).then(function() {
				return created;
			});
		});
	}

	function finish() {
		var config_name = grid.uciconfig || grid.map.config;

		if (!validateWizard())
			return;

		var cleanTransaction = state.mode === 'autotune' && !state.multiwan_set ?
			requireCleanUciTransaction(_('Apply or revert existing pending changes before creating a disabled, uncalibrated instance.')) :
			Promise.resolve();
		return cleanTransaction.then(function() {
			return Promise.resolve();
		}).then(function() {
			var section_id, created = [];
			var planItems = state.multiwan_set && state.mode === 'autotune' ? state.autotune_batch :
				(state.multiwan_set ? multiwanInstancePlans(state, rerun ? existingName : null).map(function(plan) {
					return { plan: plan, state: cloneStateForPlan(plan), uncalibrated: false };
				}) : [ {
				plan: {
					name: state.name,
					member: state.mwan3_member,
					device: state.wan_if,
					sqmSection: state.sqm_section || managedSqmSectionName(state.name)
				},
				state: state,
				uncalibrated: state.native_autotune_skipped === true
			} ]);

			if (state.multiwan_set && state.mode === 'autotune')
				return applySequentialMultiwanPlan(config_name, planItems);

			for (var planIndex = 0; planIndex < planItems.length; planIndex++)
				created.push(stageWizardPlanItem(config_name, planItems[planIndex]));

			/* stageWizardPlanItem() has already written the exact reviewed proposal
			 * into the shared UCI model.  Re-parsing the GridSection here would use
			 * stale or unrendered modal widgets and can overwrite that proposal.
			 * Persist only the staged UCI delta; Save & Apply remains a separate,
			 * guarded action. */
			return uci.save().then(function() { return created; });
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
				if (state.multiwan_set && state.mode === 'autotune')
					notification = _('%d Multi-WAN instance(s) created and applied sequentially: %s.').format(
						created.length, created.join(', '));
				else if (state.native_autotune_skipped === true)
					notification = _('%d disabled, uncalibrated instance(s) created: %s.').format(
						created.length, created.join(', '));
				else
					notification = _('%d instance(s) created: %s. Review pending changes, then Save & Apply.').format(
						created.length, created.join(', '));
				ui.addNotification(null, E('p', {}, cakeUi.text(notification)), 'info');
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
				'type': 'button',
				'class': 'btn cbi-button',
				'click': function() {
					if (state.autotune_running) {
						state.autotune_cancel_requested = true;
						state.autotune_generation = (state.autotune_generation || 0) + 1;
						var active = state.autotune_active_plan && state.autotune_active_plan.state || state;
						cancelPreferredAutotuneJob(active.name, active.wan_if,
							active.speedtest_backend, autotuneRunProfile(active),
							active.route_mode, active.mwan3_member).catch(function() {});
					}
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
				'type': 'button',
				'class': 'btn cbi-button',
				'disabled': state.autotune_running ? 'disabled' : null,
				'click': function() {
					navigateWizardStep(state.step - 1);
				}
			}, _('Back')));
			buttons.push(' ');
		}

		var invalidAutotune = state.mode === 'autotune' && !autotuneReadyForReview();
		if (state.step < 2 && !(state.step === 1 && invalidAutotune)) {
			buttons.push(E('button', {
				'type': 'button',
				'class': 'btn cbi-button cbi-button-positive',
				'click': function() {
					navigateWizardStep(state.step + 1);
				}
			}, _('Next')));
		}
		else if (invalidAutotune && state.autotune_diagnostics) {
			buttons.push(E('button', {
				'type': 'button',
				'class': 'btn cbi-button cbi-button-positive',
				'click': function() { ui.hideModal(); }
			}, _('Close diagnostics')));
		}
		else if (!invalidAutotune) {
			buttons.push(E('button', {
				'type': 'button',
				'class': 'btn cbi-button cbi-button-positive important',
				'click': finish
			}, state.native_autotune_skipped === true ? _('Create disabled') :
				(state.multiwan_set && state.mode === 'autotune' ?
					_('Create & apply sequentially') : _('Create'))));
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
	if (optionName === '_adaptive_ceiling_status')
		return 'ceiling';

	if (tab === 'general')
		return 'limits';

	if (tab === 'rates')
		return optionName === 'runtime_learning_mode' ||
			optionName === 'capacity_learning_policy' ||
			optionName === 'access_medium_selection' ||
			optionName === '_access_medium_status' ||
			optionName.indexOf('service_') === 0 ||
			optionName.indexOf('adaptive_ceiling_') === 0 ? 'ceiling' : 'limits';

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

function adaptiveCeilingStatusText(profile, policy) {
	profile = canonicalAutotuneProfile(profile);
	if (profile !== 'variable_link')
		return _('Adaptive ceiling is inactive for this profile. Select Variable Link when the connection needs bounded runtime capacity learning.');

	policy = canonicalCapacityLearningPolicy(policy) || 'verified_only';
	var labels = {
		verified_only: _('Validated ceiling only; runtime may reduce rates but cannot promote a higher ceiling.'),
		passive_bounded: _('Bounded learning from sustained real traffic is enabled.'),
		scheduled_active: _('Bounded learning plus scheduled traffic-generating calibration is enabled.'),
		fixed_cap: _('Explicit service hard caps bound the Variable Link search and runtime ceiling.')
	};
	return labels[policy];
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
		'.cake-autorate-subnav>li>a{white-space:nowrap;word-break:normal;overflow-wrap:normal;hyphens:none}',
		'.cake-autorate-subpanel{min-width:0}',
		'.cake-autorate-subdescription{margin:0 0 12px;color:var(--text-color-medium,#777)}',
		'@media(max-width:600px){.cake-autorate-subnav{display:grid!important;grid-template-columns:repeat(2,minmax(0,1fr));gap:6px;overflow:visible;padding:0}.cake-autorate-subnav>li{min-width:0;margin:0!important}.cake-autorate-subnav>li>a{display:flex;align-items:center;justify-content:center;min-height:42px;padding:6px!important;text-align:center;white-space:normal}}'
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

	o = section.taboption('rates', form.DummyValue, '_adaptive_ceiling_status',
		_('Adaptive ceiling status'));
	modal(o);
	o.rawhtml = true;
	o.cfgvalue = function(section_id) {
		return E('div', { 'class': 'alert-message notice' }, adaptiveCeilingStatusText(
			formOrUci(section, section_id, 'autotune_profile'),
			formOrUci(section, section_id, 'capacity_learning_policy')));
	};
	o.write = function() {};
	o.remove = function() {};

	o = listValue(section, 'rates', 'access_medium_selection', _('Variable Link access medium'),
		accessMediumDefinitions(), 'auto');
	o.depends('autotune_profile', 'variable_link');
	o.write = function(section_id, selected) {
		var detected = selected === 'auto' ? detectAccessMedium(
			normalizeInterfaceName(formOrUci(section, section_id, 'wan_if'))) : {
				medium: selected, source: 'user_selected', confidence_percent: 100
			};
		uci.set('cake-autorate', section_id, 'access_medium_selection', selected);
		uci.set('cake-autorate', section_id, 'access_medium', detected.medium);
		uci.set('cake-autorate', section_id, 'access_medium_source', detected.source);
		uci.set('cake-autorate', section_id, 'access_medium_confidence_percent',
			String(detected.confidence_percent));
	};

	o = section.taboption('rates', form.DummyValue, '_access_medium_status',
		_('Resolved Variable Link context'));
	modal(o);
	o.rawhtml = true;
	o.depends('autotune_profile', 'variable_link');
	o.cfgvalue = function(section_id) {
		var selection = formOrUci(section, section_id, 'access_medium_selection') || 'auto';
		var access = selection === 'auto' ? detectAccessMedium(
			normalizeInterfaceName(formOrUci(section, section_id, 'wan_if'))) : {
				medium: selection,
				source: 'user_selected',
				confidence_percent: 100,
				reason: _('Selected explicitly by the user.')
			};
		return E('div', { 'class': 'alert-message ' +
			(access.source === 'auto_inconclusive' ? 'warning' : 'notice') }, [
			E('strong', {}, _('%s · %d%% confidence').format(
				accessMediumTitle(access.medium), access.confidence_percent)),
			E('div', { 'style': 'margin-top:4px' }, cakeUi.text(access.reason)),
			E('div', { 'style': 'margin-top:4px' },
				_('Exploration floor: %d%%. PPPoE, DHCP, and Ethernet alone never prove the provider medium.').format(
					accessMediumExplorationPercent(access.medium)))
		]);
	};
	o.write = function() {};
	o.remove = function() {};

	o = listValue(section, 'rates', 'capacity_learning_policy', _('Runtime capacity learning'), [
		[ 'verified_only', _('Validated ceiling only (safest)') ],
		[ 'passive_bounded', _('Bounded learning from real traffic') ],
		[ 'scheduled_active', _('Bounded + scheduled active calibration') ],
		[ 'fixed_cap', _('Explicit service hard caps') ]
	], 'verified_only');
	o.depends('autotune_profile', 'variable_link');
	o.cfgvalue = function(section_id) {
		return canonicalCapacityLearningPolicy(
			uci.get('cake-autorate', section_id, 'capacity_learning_policy')) || 'verified_only';
	};
	o.write = function(section_id, selected) {
		uci.set('cake-autorate', section_id, 'capacity_learning_policy', selected);
		uci.set('cake-autorate', section_id, 'runtime_learning_mode',
			selected === 'scheduled_active' ? 'periodic_active' :
				(selected === 'passive_bounded' ? 'passive' : 'fixed'));
		uci.set('cake-autorate', section_id, 'adaptive_ceiling_enabled',
			(selected === 'passive_bounded' || selected === 'scheduled_active') ? '1' : '0');
		uci.set('cake-autorate', section_id, 'scheduled_autotune_enabled',
			selected === 'scheduled_active' ? '1' : '0');
	};
	o.remove = function(section_id) {
		uci.unset('cake-autorate', section_id, 'capacity_learning_policy');
		uci.unset('cake-autorate', section_id, 'runtime_learning_mode');
		uci.set('cake-autorate', section_id, 'adaptive_ceiling_enabled', '0');
		uci.set('cake-autorate', section_id, 'scheduled_autotune_enabled', '0');
	};
	o.validate = function(section_id) {
		return validateAdaptiveCeiling(validationSection(this), section_id);
	};

	o = value(section, 'rates', 'adaptive_ceiling_dl_cap_kbps', _('DL absolute cap'), 'and(uinteger,min(1))', '80000');
	o.depends('capacity_learning_policy', 'passive_bounded');
	o.depends('capacity_learning_policy', 'scheduled_active');
	o.cfgvalue = function(section_id) {
		return rateValue(uci.get('cake-autorate', section_id, 'adaptive_ceiling_dl_cap_kbps'),
			rateValue(uci.get('cake-autorate', section_id, 'max_dl_shaper_rate_kbps'), '80000'));
	};
	o.validate = function(section_id) {
		return validateAdaptiveCeiling(validationSection(this), section_id);
	};

	o = value(section, 'rates', 'adaptive_ceiling_ul_cap_kbps', _('UL absolute cap'), 'and(uinteger,min(1))', '35000');
	o.depends('capacity_learning_policy', 'passive_bounded');
	o.depends('capacity_learning_policy', 'scheduled_active');
	o.cfgvalue = function(section_id) {
		return rateValue(uci.get('cake-autorate', section_id, 'adaptive_ceiling_ul_cap_kbps'),
			rateValue(uci.get('cake-autorate', section_id, 'max_ul_shaper_rate_kbps'), '35000'));
	};
	o.validate = function(section_id) {
		return validateAdaptiveCeiling(validationSection(this), section_id);
	};

	o = value(section, 'rates', 'adaptive_ceiling_hold_time_s', _('Qualification time'), 'and(ufloat,min(1))', '20.0');
	o.depends('capacity_learning_policy', 'passive_bounded');
	o.depends('capacity_learning_policy', 'scheduled_active');

	o = value(section, 'rates', 'adaptive_ceiling_growth_percent', _('Open probe step'), 'and(ufloat,min(0.1),max(10))', '3.0');
	o.depends('capacity_learning_policy', 'passive_bounded');
	o.depends('capacity_learning_policy', 'scheduled_active');

	o = value(section, 'rates', 'adaptive_ceiling_probe_duration_s', _('Probe observation'), 'and(ufloat,min(1))', '8.0');
	o.depends('capacity_learning_policy', 'passive_bounded');
	o.depends('capacity_learning_policy', 'scheduled_active');

	o = value(section, 'rates', 'adaptive_ceiling_cooldown_s', _('Probe cooldown'), 'and(ufloat,min(0))', '30.0');
	o.depends('capacity_learning_policy', 'passive_bounded');
	o.depends('capacity_learning_policy', 'scheduled_active');

	o = value(section, 'rates', 'adaptive_ceiling_failed_bound_ttl_s', _('Failed-bound memory'), 'and(ufloat,min(1))', '900.0');
	o.depends('capacity_learning_policy', 'passive_bounded');
	o.depends('capacity_learning_policy', 'scheduled_active');

	o = value(section, 'rates', 'service_dl_cap_kbps', _('Download service hard cap'),
		'and(uinteger,min(100),max(100000000))');
	o.depends('capacity_learning_policy', 'fixed_cap');
	o.validate = function(section_id) {
		return validateAdaptiveCeiling(validationSection(this), section_id);
	};
	o = value(section, 'rates', 'service_ul_cap_kbps', _('Upload service hard cap'),
		'and(uinteger,min(100),max(100000000))');
	o.depends('capacity_learning_policy', 'fixed_cap');
	o.validate = function(section_id) {
		return validateAdaptiveCeiling(validationSection(this), section_id);
	};
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
	o.description = _('Runtime controller safety floor relative to its learned throughput reference. This does not scale the initial Full Auto-Tune candidate.');
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
		[ 'variable_link', _('Variable link — measured knee, target B') ],
		[ 'fair', _('Fair — throughput first, aim for C') ]
	], 'best_overall');
	o.description = _('Every profile starts from the highest measured/testable rate. Profile percentages are exploration limits or Auto-Apply objectives, never an unconditional rate reduction.');
	describe(o, 'autotune_profile');
	o = listValue(section, 'testing', 'autotune_calibration_strategy', _('Calibration strategy'), [
		[ 'shaped_only', _('Shaped only (recommended)') ],
		[ 'full_raw', _('Full raw capacity (temporary directional bypass)') ],
		[ 'reuse_trusted', _('Reuse current trusted bounds') ]
	], 'shaped_only');
	describe(o, 'autotune_calibration_strategy');

	o = value(section, 'testing', 'scheduled_autotune_interval_hours', _('Retune interval'), 'and(uinteger,min(1),max(8760))', '24');
	o.depends('runtime_learning_mode', 'periodic_active');
	o = value(section, 'testing', 'scheduled_autotune_idle_window_s', _('Required quiet time'), 'and(uinteger,min(30),max(3600))', '60');
	o.depends('runtime_learning_mode', 'periodic_active');
	o = value(section, 'testing', 'scheduled_autotune_window_start_hour', _('Window starts'), 'and(uinteger,min(0),max(23))', '2');
	o.depends('runtime_learning_mode', 'periodic_active');
	o = value(section, 'testing', 'scheduled_autotune_window_end_hour', _('Window ends'), 'and(uinteger,min(0),max(23))', '5');
	o.depends('runtime_learning_mode', 'periodic_active');
	o = value(section, 'testing', 'scheduled_autotune_max_traffic_mb_day', _('Daily traffic budget'), 'and(uinteger,min(100),max(1048576))', '4096');
	o.depends('runtime_learning_mode', 'periodic_active');
	describe(o, 'scheduled_autotune_max_traffic_mb_day');
	o = value(section, 'testing', 'scheduled_autotune_max_traffic_mb_month', _('Monthly traffic budget'), 'and(uinteger,min(100),max(1048576))', '16384');
	o.depends('runtime_learning_mode', 'periodic_active');
	describe(o, 'scheduled_autotune_max_traffic_mb_month');
	o = flag(section, 'testing', 'scheduled_autotune_auto_apply', _('Apply validated proposal automatically'), '0');
	o.depends('runtime_learning_mode', 'periodic_active');
}

function addSpeedtestOptions(section) {
	var o;

	o = section.taboption('speedtest', form.ListValue, 'speedtest_backend', _('Preferred backend'));
	modal(o);
	describe(o, 'speedtest_backend');
	o.rmempty = false;
	o.default = 'auto';
	o.value('auto', _('Auto'));
	o.value('speedtest-go', _('speedtest-go (package: speedtest-go)'));
	o.validate = function(section_id, formvalue) {
		if (formvalue !== 'auto' && formvalue !== 'speedtest-go')
			return _('This backend is no longer supported. Choose Auto or speedtest-go.');
		return true;
	};
	o.onchange = function(ev, section_id) {
		refreshSpeedtestSummaries(this.section, section_id);
	};
	o = optionalValue(section, 'speedtest', 'speedtest_go_server_id', _('speedtest-go server ID'), 'uinteger', '');
	describe(o, 'speedtest_go_server_id');
	dependsAny(o, 'speedtest_backend', [ 'auto', 'speedtest-go' ]);
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
		value = normalizeInterfaceName(value);
		var previous = selectedWan(null, section_id);
		var importRates = shouldImportInterfaceRates(previous, value,
			uci.get('cake-autorate', section_id, 'sqm_download'),
			uci.get('cake-autorate', section_id, 'sqm_upload'));

		if (autoInterfacePresetEnabled(this.section, section_id))
			applyWanPreset(section_id, value, importRates, this.section);

		syncManagedSqmEnabled(this.section, section_id);
		refreshSpeedtestSummaries(this.section, section_id);
	};
	o.write = function(section_id, formvalue) {
		formvalue = normalizeInterfaceName(formvalue);

		var previous = selectedWan(null, section_id);
		var importRates = shouldImportInterfaceRates(previous, formvalue,
			uci.get('cake-autorate', section_id, 'sqm_download'),
			uci.get('cake-autorate', section_id, 'sqm_upload'));

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
				'type': 'button',
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

		return runSpeedtestJob(section_id, wan, backend, null,
			formOrUci(activeSection, section_id, 'route_mode') || 'main',
			formOrUci(activeSection, section_id, 'mwan3_member') || '',
			formOrUci(activeSection, section_id, 'speedtest_go_server_id') || '', true,
			'unshaped', null).then(function(res) {
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

			ui.addNotification(null, E('p', {}, cakeUi.text(message)), result.warning ? 'warning' : 'info');
		}).catch(function(err) {
			ui.addNotification(null, E('p', {}, cakeUi.text(_('Speed test failed: %s').format(err.message || err))), 'error');
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
	o.retain = true;
	o.validate = function(section_id) {
		return validateDifferentInterfaces(validationSection(this), section_id);
	};

	o = iface(section, 'interfaces', 'ul_if', _('Upload interface'));
	o.depends('auto_interface_preset', '0');
	o.retain = true;
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
			ui.addNotification(null, E('pre', { 'style': 'white-space:pre-wrap' }, cakeUi.text(formatPingerPlan(result))), 'info');
		}).catch(function(err) {
			ui.addNotification(null, E('p', {}, cakeUi.text(_('Pinger status check failed: %s').format(err.message || err))), 'error');
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
			ui.addNotification(null, E('p', {}, cakeUi.text(formatPingerInstall(result))), result.available ? 'info' : 'warning');
		}).catch(function(err) {
			ui.addNotification(null, E('p', {}, cakeUi.text(_('Pinger install failed: %s').format(err.message || err))), 'error');
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
			ui.addNotification(null, E('pre', { 'style': 'white-space:pre-wrap' }, cakeUi.text(formatPingerPlan(result))), level);
		}).catch(function(err) {
			ui.addNotification(null, E('p', {}, cakeUi.text(_('Reflector scan failed: %s').format(err.message || err))), 'error');
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
				cakeUi.text(formatPingerPlan(result) + '\n\n' + _('Recommendation applied to pending changes. Use Save & Apply to commit it.'))), 'info');
		}).catch(function(err) {
			ui.addNotification(null, E('p', {}, cakeUi.text(_('Applying reflector recommendation failed: %s').format(err.message || err))), 'error');
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
			ui.addNotification(null, E('pre', { 'style': 'white-space:pre-wrap' }, cakeUi.text(formatMqttStatus(result))), result.available ? 'info' : 'warning');
		}).catch(function(err) {
			ui.addNotification(null, E('p', {}, cakeUi.text(_('MQTT status check failed: %s').format(err.message || err))), 'error');
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
	value(section, 'advanced', 'route_check_interval_s', _('Route check interval'), 'and(ufloat,min(1),max(60))', '2.0');
	optionalValue(section, 'advanced', 'rx_bytes_path', _('RX bytes path'), null, '');
	optionalValue(section, 'advanced', 'tx_bytes_path', _('TX bytes path'), null, '');
}

function manualSqmDirectionMode(value) {
	return [ 'both', 'upload_only', 'download_only', 'off' ].indexOf(value) >= 0 ? value : 'both';
}

function validateManualSqmDirectionMode(section, section_id, selected) {
	if ([ 'both', 'upload_only', 'download_only', 'off' ].indexOf(selected) < 0)
		return _('CAKE directions must be Both, Upload only, Download only, or Off.');

	if (selected === 'off' &&
	    (checkedFormOrUci(section, section_id, 'enabled', false) ||
	     checkedFormOrUci(section, section_id, 'sqm_enabled', false)))
		return _('Disable autorate and managed SQM before keeping CAKE directions Off.');

	return true;
}

function writeManualSqmDirectionMode(section_id, selected, section) {
	var validation = validateManualSqmDirectionMode(section, section_id, selected);

	if (validation !== true)
		throw new TypeError(validation);

	uci.set('cake-autorate', section_id, 'sqm_direction_mode', selected);

	/* A missing CAKE direction cannot be adjusted by autorate. Preserve an
	 * intentional fixed-rate choice on every active or restored direction. */
	if (selected === 'upload_only' || selected === 'off')
		uci.set('cake-autorate', section_id, 'adjust_dl_shaper_rate', '0');

	if (selected === 'download_only' || selected === 'off')
		uci.set('cake-autorate', section_id, 'adjust_ul_shaper_rate', '0');
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

	o = listValue(section, 'sqm_basic', 'sqm_direction_mode', _('CAKE directions'), [
		[ 'both', _('Both — download and upload') ],
		[ 'upload_only', _('Upload only — no download/ingress CAKE') ],
		[ 'download_only', _('Download only — no upload/egress CAKE') ]
	], 'both');
	dependsManagedSqm(o);
	o.cfgvalue = function(section_id) {
		var selected = manualSqmDirectionMode(
			uci.get('cake-autorate', section_id, 'sqm_direction_mode'));

		/* Off is an exact terminal state written by verified no-SQM Apply. Keep
		 * it visible when editing that state, but do not advertise it as a normal
		 * running preset. Re-enabling the instance requires an active direction. */
		if (selected === 'off' &&
		    (!Array.isArray(this.keylist) || this.keylist.indexOf('off') < 0))
			this.value('off', _('Off — SQM disabled'));

		return selected;
	};
	o.write = function(section_id, selected) {
		writeManualSqmDirectionMode(section_id, selected, this.section);
	};
	o.validate = function(section_id, selected) {
		return validateManualSqmDirectionMode(validationSection(this), section_id, selected);
	};

	o = iface(section, 'sqm_basic', 'sqm_interface', _('SQM interface'));
	dependsManagedSqm(o, { auto_interface_preset: '0' });
	o.retain = true;
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
	handleReset: function(ev) {
		/* Modal Save materializes retained/default values into this LuCI RPC
		 * session. The stock view reset only rebuilds the form and can therefore
		 * leave those package deltas behind as an invisible Unsaved Changes
		 * transaction. Revert exactly the two packages owned by this page, unload
		 * their local cache, and rebuild from authoritative UCI. Never discard
		 * unrelated packages staged by another LuCI page. */
		return discardStagedUciPackages([ 'cake-autorate', 'sqm' ]).then(function() {
			reloadViewPage();
			return true;
		});
	},

	handleSave: function(ev) {
		return this.super('handleSave', [ ev ]);
	},

	handleSaveApply: function(ev, mode) {
		return this.super('handleSaveApply', [ ev, mode ]);
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
			L.resolveDefault(fs.exec('/usr/sbin/cake-autorated', [ '--mwan3-info' ]).then(function(result) {
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
					'type': 'button',
					'title': _('Re-run Auto-Tune'),
					'class': 'btn cbi-button cbi-button-action cake-autotune-rerun',
					'click': ui.createHandlerFn(this, function() {
						showCreateWizard(this, section_id, section_id);
					})
				}, _('Re-run Auto-Tune')), container.firstChild);
				container.insertBefore(E('button', {
					'type': 'button',
					'title': _('Traffic priorities'),
					'class': 'btn cbi-button cbi-button-neutral cake-traffic-priorities',
					'click': ui.createHandlerFn(this, function() {
						window.location = trafficPrioritiesUrl(section_id);
					})
				}, _('Traffic priorities')), container.firstChild);
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
					ui.addNotification(null, E('p', {}, cakeUi.text(validation)), 'error');
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
			_('Run speed tests and Full Auto-Tune. Scheduled active calibration is opt-in and can transfer many gigabytes; its quiet window and hard daily/monthly budgets apply per uplink.'));
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

		return m.render().then(function(node) {
			node.insertBefore(settingsActionLayout(), node.firstChild);
			return node;
		});
	}
});
