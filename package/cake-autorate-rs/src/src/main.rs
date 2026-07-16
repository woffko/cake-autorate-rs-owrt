use std::collections::{HashMap, VecDeque};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod adaptive_ceiling;
mod autotune;
mod quality_grade;
mod rating_load;
mod routing;
mod transport_probe;
mod transport_quality;

use adaptive_ceiling::{
    AdaptiveCeilingChange, AdaptiveCeilingDirection, AdaptiveCeilingObservation,
    AdaptiveCeilingPolicy, AdaptiveCeilingUpdate,
};
use quality_grade::{QualityGradeMetric, QualityGradeResult, QualityGradeTracker};
use rating_load::{RatingLoadConfig, RatingLoadDetector, RatingLoadSnapshot, RatingPhase};
use routing::{RouteInspector, RouteMode, RouteSnapshot, RouteSpec, UplinkLifecycle, UplinkState};
use transport_probe::{RouteBinding, TransportProbeBackend, TransportProbeEngine};
use transport_quality::{
    classify_quality, effective_latency_delta_ms, throughput_floor, transport_allows_growth,
    QualityClass, QualitySearchDirection, QualitySearchPolicy, ThroughputGuardInput,
    TransportLatencyTracker,
};

static TERMINATE: AtomicBool = AtomicBool::new(false);
const STALE_REFLECTOR_RESPONSE_MAX_AGE_S: f64 = 0.5;
const GRAPH_HISTORY_MIN_BUDGET_KIB: u64 = 256;
const GRAPH_HISTORY_HARD_MAX_KIB: u64 = 100 * 1024;
const GRAPH_HISTORY_BUDGET_REFRESH_S: u64 = 30;
const GRAPH_HISTORY_CRITICAL_AVAILABLE_KIB: u64 = 16 * 1024;
const SQM_RUNTIME_HEALTH_CHECK_FAST_S: u64 = 3;
const SQM_RUNTIME_HEALTH_CHECK_HEALTHY_S: u64 = 15;
const SQM_RUNTIME_RECOVERY_COOLDOWN_S: u64 = 30;
const STATUS_PUBLISH_INTERVAL: Duration = Duration::from_millis(250);
const CAKE_GROWTH_UPDATE_MIN_INTERVAL: Duration = Duration::from_millis(100);
const TRANSPORT_BASELINE_LEARNING_INTERVAL_S: f64 = 1.0;

const UPSTREAM_DEFAULT_REFLECTORS: &[&str] = &[
    "1.1.1.1",
    "1.0.0.1",
    "8.8.8.8",
    "8.8.4.4",
    "9.9.9.9",
    "9.9.9.10",
    "9.9.9.11",
    "94.140.14.15",
    "94.140.14.140",
    "94.140.14.141",
    "94.140.15.15",
    "94.140.15.16",
    "64.6.65.6",
    "156.154.70.1",
    "156.154.70.2",
    "156.154.70.3",
    "156.154.70.4",
    "156.154.70.5",
    "156.154.71.1",
    "156.154.71.2",
    "156.154.71.3",
    "156.154.71.4",
    "156.154.71.5",
    "208.67.220.2",
    "208.67.220.123",
    "208.67.220.220",
    "208.67.222.2",
    "208.67.222.123",
    "185.228.168.9",
    "185.228.168.10",
];

extern "C" fn handle_signal(_: i32) {
    TERMINATE.store(true, Ordering::SeqCst);
}

extern "C" {
    fn signal(signum: i32, handler: extern "C" fn(i32)) -> extern "C" fn(i32);
}

#[derive(Clone, Debug)]
struct Config {
    instance: String,
    enabled: bool,
    manage_sqm: bool,
    sqm_enabled: bool,
    sqm_interface: String,
    dl_if: String,
    ul_if: String,
    route_mode: String,
    mwan3_member: String,
    route_stability_s: f64,
    route_check_interval_s: f64,
    rx_bytes_path: String,
    tx_bytes_path: String,
    adjust_dl_shaper_rate: bool,
    adjust_ul_shaper_rate: bool,
    min_dl_shaper_rate_kbps: f64,
    base_dl_shaper_rate_kbps: f64,
    max_dl_shaper_rate_kbps: f64,
    adaptive_ceiling_enabled: bool,
    adaptive_ceiling_dl_cap_kbps: f64,
    adaptive_ceiling_ul_cap_kbps: f64,
    adaptive_ceiling_hold_time_s: f64,
    adaptive_ceiling_growth_percent: f64,
    adaptive_ceiling_probe_duration_s: f64,
    adaptive_ceiling_cooldown_s: f64,
    adaptive_ceiling_failed_bound_ttl_s: f64,
    transport_latency_enabled: bool,
    transport_controller_enabled: bool,
    transport_probe_backend: String,
    transport_probe_endpoint: String,
    transport_probe_urls: Vec<String>,
    transport_probe_idle_interval_s: f64,
    transport_probe_loaded_interval_s: f64,
    transport_probe_timeout_s: u64,
    transport_load_hold_s: f64,
    transport_cpu_max_percent: f64,
    rating_load_window_s: f64,
    rating_load_enter_ratio: f64,
    rating_load_exit_ratio: f64,
    rating_load_hold_s: f64,
    rating_load_dropout_s: f64,
    rating_load_min_kbps: f64,
    rating_load_dominance_ratio: f64,
    rating_capture_min_enter_ratio: f64,
    rating_capture_peak_factor: f64,
    rating_capture_contamination_ratio: f64,
    rating_capture_ack_ratio: f64,
    rating_episode_gap_s: f64,
    quality_target_delay_ms: f64,
    quality_search_max_steps: usize,
    quality_search_observe_s: f64,
    quality_search_cooldown_s: f64,
    throughput_guard_enabled: bool,
    throughput_guard_retention_percent: f64,
    throughput_guard_dl_floor_kbps: f64,
    throughput_guard_ul_floor_kbps: f64,
    throughput_reference_dl_p20_kbps: f64,
    throughput_reference_dl_p50_kbps: f64,
    throughput_reference_ul_p20_kbps: f64,
    throughput_reference_ul_p50_kbps: f64,
    min_ul_shaper_rate_kbps: f64,
    base_ul_shaper_rate_kbps: f64,
    max_ul_shaper_rate_kbps: f64,
    connection_active_thr_kbps: f64,
    enable_sleep_function: bool,
    sustained_idle_sleep_thr_s: f64,
    min_shaper_rates_enforcement: bool,
    stall_detection_thr: usize,
    connection_stall_thr_kbps: f64,
    global_ping_response_timeout_s: f64,
    pinger_method: String,
    ping_extra_args: String,
    ping_prefix_string: String,
    reflectors: Vec<String>,
    irtt_servers: Vec<String>,
    irtt_session_duration_m: f64,
    reflectors_url: String,
    reflectors_url_skip_lines: usize,
    randomize_reflectors: bool,
    retain_reflector_stats: bool,
    no_pingers: usize,
    reflector_ping_interval_s: f64,
    reflector_health_check_interval_s: f64,
    reflector_response_deadline_s: f64,
    reflector_misbehaving_detection_window: usize,
    reflector_misbehaving_detection_thr: usize,
    reflector_replacement_interval_mins: f64,
    reflector_comparison_interval_mins: f64,
    reflector_sum_owd_baselines_delta_thr_ms: f64,
    reflector_owd_delta_ewma_delta_thr_ms: f64,
    monitor_achieved_rates_interval_ms: u64,
    bufferbloat_detection_window: usize,
    bufferbloat_detection_thr: usize,
    high_load_thr: f64,
    dl_owd_delta_delay_thr_ms: f64,
    ul_owd_delta_delay_thr_ms: f64,
    dl_avg_owd_delta_max_adjust_up_thr_ms: f64,
    ul_avg_owd_delta_max_adjust_up_thr_ms: f64,
    dl_avg_owd_delta_max_adjust_down_thr_ms: f64,
    ul_avg_owd_delta_max_adjust_down_thr_ms: f64,
    alpha_baseline_increase: f64,
    alpha_baseline_decrease: f64,
    alpha_delta_ewma: f64,
    shaper_rate_min_adjust_down_bufferbloat: f64,
    shaper_rate_max_adjust_down_bufferbloat: f64,
    shaper_rate_min_adjust_up_load_high: f64,
    shaper_rate_max_adjust_up_load_high: f64,
    shaper_rate_adjust_down_load_low: f64,
    shaper_rate_adjust_up_load_low: f64,
    bufferbloat_refractory_period_ms: u64,
    decay_refractory_period_ms: u64,
    output_processing_stats: bool,
    output_summary_stats: bool,
    output_load_stats: bool,
    output_reflector_stats: bool,
    output_cake_changes: bool,
    output_cpu_stats: bool,
    output_cpu_raw_stats: bool,
    graph_history_enabled: bool,
    graph_history_interval_s: u64,
    graph_history_ram_budget_kib: Option<u64>,
    graph_history_instance_count: usize,
    log_to_file: bool,
    debug: bool,
    log_debug_messages_to_syslog: bool,
    log_file_max_time_mins: u64,
    log_file_max_size_kb: u64,
    log_file_path_override: String,
    log_file_buffer_size_b: u64,
    log_file_buffer_timeout_ms: u64,
    log_file_export_compress: bool,
    startup_wait_s: f64,
    if_up_check_interval_s: f64,
    monitor_cpu_usage_interval_ms: u64,
    dl_max_wire_packet_size_bits: u64,
    ul_max_wire_packet_size_bits: u64,
}

impl Config {
    fn defaults(instance: String) -> Self {
        Self {
            instance,
            enabled: false,
            manage_sqm: true,
            sqm_enabled: false,
            sqm_interface: String::new(),
            dl_if: "ifb-wan".to_string(),
            ul_if: "wan".to_string(),
            route_mode: "auto".to_string(),
            mwan3_member: String::new(),
            route_stability_s: 5.0,
            route_check_interval_s: 2.0,
            rx_bytes_path: String::new(),
            tx_bytes_path: String::new(),
            adjust_dl_shaper_rate: true,
            adjust_ul_shaper_rate: true,
            min_dl_shaper_rate_kbps: 5000.0,
            base_dl_shaper_rate_kbps: 20000.0,
            max_dl_shaper_rate_kbps: 80000.0,
            adaptive_ceiling_enabled: false,
            adaptive_ceiling_dl_cap_kbps: 80000.0,
            adaptive_ceiling_ul_cap_kbps: 35000.0,
            adaptive_ceiling_hold_time_s: 20.0,
            adaptive_ceiling_growth_percent: 3.0,
            adaptive_ceiling_probe_duration_s: 8.0,
            adaptive_ceiling_cooldown_s: 30.0,
            adaptive_ceiling_failed_bound_ttl_s: 900.0,
            transport_latency_enabled: false,
            transport_controller_enabled: false,
            transport_probe_backend: "websocket".to_string(),
            transport_probe_endpoint: "wss://ping-bufferbloat.libreqos.com/ws".to_string(),
            transport_probe_urls: vec![
                "https://speed.cloudflare.com/__down?bytes=0".to_string(),
                "https://www.google.com/generate_204".to_string(),
                "https://connectivitycheck.gstatic.com/generate_204".to_string(),
            ],
            transport_probe_idle_interval_s: 15.0,
            transport_probe_loaded_interval_s: 1.0,
            transport_probe_timeout_s: 5,
            transport_load_hold_s: 3.0,
            transport_cpu_max_percent: 85.0,
            rating_load_window_s: 2.0,
            rating_load_enter_ratio: 0.60,
            rating_load_exit_ratio: 0.40,
            rating_load_hold_s: 1.0,
            rating_load_dropout_s: 1.5,
            rating_load_min_kbps: 2000.0,
            rating_load_dominance_ratio: 1.5,
            rating_capture_min_enter_ratio: 0.15,
            rating_capture_peak_factor: 0.35,
            rating_capture_contamination_ratio: 0.10,
            rating_capture_ack_ratio: 0.08,
            rating_episode_gap_s: 30.0,
            quality_target_delay_ms: 30.0,
            quality_search_max_steps: 3,
            quality_search_observe_s: 6.0,
            quality_search_cooldown_s: 900.0,
            throughput_guard_enabled: true,
            throughput_guard_retention_percent: 80.0,
            throughput_guard_dl_floor_kbps: 0.0,
            throughput_guard_ul_floor_kbps: 0.0,
            throughput_reference_dl_p20_kbps: 0.0,
            throughput_reference_dl_p50_kbps: 0.0,
            throughput_reference_ul_p20_kbps: 0.0,
            throughput_reference_ul_p50_kbps: 0.0,
            min_ul_shaper_rate_kbps: 5000.0,
            base_ul_shaper_rate_kbps: 20000.0,
            max_ul_shaper_rate_kbps: 35000.0,
            connection_active_thr_kbps: 2000.0,
            enable_sleep_function: true,
            sustained_idle_sleep_thr_s: 60.0,
            min_shaper_rates_enforcement: false,
            stall_detection_thr: 5,
            connection_stall_thr_kbps: 10.0,
            global_ping_response_timeout_s: 10.0,
            pinger_method: "fping".to_string(),
            ping_extra_args: String::new(),
            ping_prefix_string: String::new(),
            reflectors: default_reflectors(),
            irtt_servers: Vec::new(),
            irtt_session_duration_m: 10.0,
            reflectors_url: String::new(),
            reflectors_url_skip_lines: 1,
            randomize_reflectors: true,
            retain_reflector_stats: true,
            no_pingers: 6,
            reflector_ping_interval_s: 0.3,
            reflector_health_check_interval_s: 1.0,
            reflector_response_deadline_s: 1.0,
            reflector_misbehaving_detection_window: 60,
            reflector_misbehaving_detection_thr: 3,
            reflector_replacement_interval_mins: 60.0,
            reflector_comparison_interval_mins: 1.0,
            reflector_sum_owd_baselines_delta_thr_ms: 20.0,
            reflector_owd_delta_ewma_delta_thr_ms: 10.0,
            monitor_achieved_rates_interval_ms: 200,
            bufferbloat_detection_window: 6,
            bufferbloat_detection_thr: 3,
            high_load_thr: 0.75,
            dl_owd_delta_delay_thr_ms: 30.0,
            ul_owd_delta_delay_thr_ms: 30.0,
            dl_avg_owd_delta_max_adjust_up_thr_ms: 10.0,
            ul_avg_owd_delta_max_adjust_up_thr_ms: 10.0,
            dl_avg_owd_delta_max_adjust_down_thr_ms: 60.0,
            ul_avg_owd_delta_max_adjust_down_thr_ms: 60.0,
            alpha_baseline_increase: 0.001,
            alpha_baseline_decrease: 0.9,
            alpha_delta_ewma: 0.095,
            shaper_rate_min_adjust_down_bufferbloat: 0.99,
            shaper_rate_max_adjust_down_bufferbloat: 0.75,
            shaper_rate_min_adjust_up_load_high: 1.0,
            shaper_rate_max_adjust_up_load_high: 1.04,
            shaper_rate_adjust_down_load_low: 0.99,
            shaper_rate_adjust_up_load_low: 1.01,
            bufferbloat_refractory_period_ms: 300,
            decay_refractory_period_ms: 1000,
            output_processing_stats: false,
            output_summary_stats: false,
            output_load_stats: false,
            output_reflector_stats: false,
            output_cake_changes: false,
            output_cpu_stats: false,
            output_cpu_raw_stats: false,
            graph_history_enabled: false,
            graph_history_interval_s: 10,
            graph_history_ram_budget_kib: None,
            graph_history_instance_count: 1,
            log_to_file: true,
            debug: true,
            log_debug_messages_to_syslog: false,
            log_file_max_time_mins: 10,
            log_file_max_size_kb: 2000,
            log_file_path_override: String::new(),
            log_file_buffer_size_b: 512,
            log_file_buffer_timeout_ms: 500,
            log_file_export_compress: true,
            startup_wait_s: 0.0,
            if_up_check_interval_s: 10.0,
            monitor_cpu_usage_interval_ms: 2000,
            dl_max_wire_packet_size_bits: 0,
            ul_max_wire_packet_size_bits: 0,
        }
    }

    fn from_uci(instance: &str) -> Result<Self, String> {
        let mut cfg = Self::defaults(instance.to_string());
        let query = format!("cake-autorate.{}", instance);
        let output = Command::new("uci")
            .arg("-q")
            .arg("show")
            .arg(&query)
            .output()
            .map_err(|e| format!("failed to execute uci: {e}"))?;

        if !output.status.success() {
            return Err(format!("UCI section {query} not found"));
        }

        let data = String::from_utf8_lossy(&output.stdout);
        let mut single: HashMap<String, String> = HashMap::new();
        let mut lists: HashMap<String, Vec<String>> = HashMap::new();

        for line in data.lines() {
            let Some((left, raw_value)) = line.split_once('=') else {
                continue;
            };
            let mut parts = left.split('.');
            let _package = parts.next();
            let _section = parts.next();
            let Some(key) = parts.next() else {
                continue;
            };
            if parts.next().is_some() {
                continue;
            }
            let values = parse_uci_values(raw_value);
            if let Some(value) = values.first() {
                single.insert(key.to_string(), value.clone());
                lists.entry(key.to_string()).or_default().extend(values);
            }
        }

        set_bool(&single, "enabled", &mut cfg.enabled)?;
        set_bool(&single, "manage_sqm", &mut cfg.manage_sqm)?;
        cfg.sqm_enabled = cfg.enabled;
        set_bool(&single, "sqm_enabled", &mut cfg.sqm_enabled)?;
        set_string(&single, "sqm_interface", &mut cfg.sqm_interface);
        set_string(&single, "dl_if", &mut cfg.dl_if);
        set_string(&single, "ul_if", &mut cfg.ul_if);
        set_string(&single, "route_mode", &mut cfg.route_mode);
        set_string(&single, "mwan3_member", &mut cfg.mwan3_member);
        set_f64(&single, "route_stability_s", &mut cfg.route_stability_s)?;
        set_f64(
            &single,
            "route_check_interval_s",
            &mut cfg.route_check_interval_s,
        )?;
        if single
            .get("auto_interface_preset")
            .map(|value| parse_bool(value).map_err(|e| format!("auto_interface_preset: {e}")))
            .transpose()?
            .unwrap_or(true)
        {
            if let Some(wan_if) = single
                .get("wan_if")
                .or_else(|| single.get("sqm_interface"))
                .or_else(|| single.get("ul_if"))
                .filter(|value| !value.is_empty())
            {
                cfg.ul_if = wan_if.clone();
                cfg.dl_if = format!("ifb4{wan_if}");
                if cfg.sqm_interface.is_empty() {
                    cfg.sqm_interface = wan_if.clone();
                }
            }
        }
        if cfg.sqm_interface.is_empty() {
            cfg.sqm_interface = cfg.ul_if.clone();
        }
        set_string(&single, "rx_bytes_path", &mut cfg.rx_bytes_path);
        set_string(&single, "tx_bytes_path", &mut cfg.tx_bytes_path);
        set_bool(
            &single,
            "adjust_dl_shaper_rate",
            &mut cfg.adjust_dl_shaper_rate,
        )?;
        set_bool(
            &single,
            "adjust_ul_shaper_rate",
            &mut cfg.adjust_ul_shaper_rate,
        )?;
        set_f64(
            &single,
            "min_dl_shaper_rate_kbps",
            &mut cfg.min_dl_shaper_rate_kbps,
        )?;
        set_f64(
            &single,
            "base_dl_shaper_rate_kbps",
            &mut cfg.base_dl_shaper_rate_kbps,
        )?;
        set_f64(
            &single,
            "max_dl_shaper_rate_kbps",
            &mut cfg.max_dl_shaper_rate_kbps,
        )?;
        set_f64(
            &single,
            "min_ul_shaper_rate_kbps",
            &mut cfg.min_ul_shaper_rate_kbps,
        )?;
        set_f64(
            &single,
            "base_ul_shaper_rate_kbps",
            &mut cfg.base_ul_shaper_rate_kbps,
        )?;
        set_f64(
            &single,
            "max_ul_shaper_rate_kbps",
            &mut cfg.max_ul_shaper_rate_kbps,
        )?;
        set_bool(
            &single,
            "adaptive_ceiling_enabled",
            &mut cfg.adaptive_ceiling_enabled,
        )?;
        let adaptive_dl_cap_configured = single.contains_key("adaptive_ceiling_dl_cap_kbps");
        let adaptive_ul_cap_configured = single.contains_key("adaptive_ceiling_ul_cap_kbps");
        set_f64(
            &single,
            "adaptive_ceiling_dl_cap_kbps",
            &mut cfg.adaptive_ceiling_dl_cap_kbps,
        )?;
        set_f64(
            &single,
            "adaptive_ceiling_ul_cap_kbps",
            &mut cfg.adaptive_ceiling_ul_cap_kbps,
        )?;
        set_f64(
            &single,
            "adaptive_ceiling_hold_time_s",
            &mut cfg.adaptive_ceiling_hold_time_s,
        )?;
        set_f64(
            &single,
            "adaptive_ceiling_growth_percent",
            &mut cfg.adaptive_ceiling_growth_percent,
        )?;
        set_f64(
            &single,
            "adaptive_ceiling_probe_duration_s",
            &mut cfg.adaptive_ceiling_probe_duration_s,
        )?;
        set_f64(
            &single,
            "adaptive_ceiling_cooldown_s",
            &mut cfg.adaptive_ceiling_cooldown_s,
        )?;
        set_f64(
            &single,
            "adaptive_ceiling_failed_bound_ttl_s",
            &mut cfg.adaptive_ceiling_failed_bound_ttl_s,
        )?;
        set_bool(
            &single,
            "transport_latency_enabled",
            &mut cfg.transport_latency_enabled,
        )?;
        set_bool(
            &single,
            "transport_controller_enabled",
            &mut cfg.transport_controller_enabled,
        )?;
        set_string(
            &single,
            "transport_probe_backend",
            &mut cfg.transport_probe_backend,
        );
        set_string(
            &single,
            "transport_probe_endpoint",
            &mut cfg.transport_probe_endpoint,
        );
        set_f64(
            &single,
            "transport_probe_idle_interval_s",
            &mut cfg.transport_probe_idle_interval_s,
        )?;
        set_f64(
            &single,
            "transport_probe_loaded_interval_s",
            &mut cfg.transport_probe_loaded_interval_s,
        )?;
        set_u64(
            &single,
            "transport_probe_timeout_s",
            &mut cfg.transport_probe_timeout_s,
        )?;
        set_f64(
            &single,
            "transport_load_hold_s",
            &mut cfg.transport_load_hold_s,
        )?;
        set_f64(
            &single,
            "transport_cpu_max_percent",
            &mut cfg.transport_cpu_max_percent,
        )?;
        set_f64(
            &single,
            "rating_load_window_s",
            &mut cfg.rating_load_window_s,
        )?;
        set_f64(
            &single,
            "rating_load_enter_ratio",
            &mut cfg.rating_load_enter_ratio,
        )?;
        set_f64(
            &single,
            "rating_load_exit_ratio",
            &mut cfg.rating_load_exit_ratio,
        )?;
        set_f64(&single, "rating_load_hold_s", &mut cfg.rating_load_hold_s)?;
        set_f64(
            &single,
            "rating_load_dropout_s",
            &mut cfg.rating_load_dropout_s,
        )?;
        set_f64(
            &single,
            "rating_load_min_kbps",
            &mut cfg.rating_load_min_kbps,
        )?;
        set_f64(
            &single,
            "rating_load_dominance_ratio",
            &mut cfg.rating_load_dominance_ratio,
        )?;
        set_f64(
            &single,
            "rating_capture_min_enter_ratio",
            &mut cfg.rating_capture_min_enter_ratio,
        )?;
        set_f64(
            &single,
            "rating_capture_peak_factor",
            &mut cfg.rating_capture_peak_factor,
        )?;
        set_f64(
            &single,
            "rating_capture_contamination_ratio",
            &mut cfg.rating_capture_contamination_ratio,
        )?;
        set_f64(
            &single,
            "rating_capture_ack_ratio",
            &mut cfg.rating_capture_ack_ratio,
        )?;
        set_f64(
            &single,
            "rating_episode_gap_s",
            &mut cfg.rating_episode_gap_s,
        )?;
        set_f64(
            &single,
            "quality_target_delay_ms",
            &mut cfg.quality_target_delay_ms,
        )?;
        set_usize(
            &single,
            "quality_search_max_steps",
            &mut cfg.quality_search_max_steps,
        )?;
        set_f64(
            &single,
            "quality_search_observe_s",
            &mut cfg.quality_search_observe_s,
        )?;
        set_f64(
            &single,
            "quality_search_cooldown_s",
            &mut cfg.quality_search_cooldown_s,
        )?;
        set_bool(
            &single,
            "throughput_guard_enabled",
            &mut cfg.throughput_guard_enabled,
        )?;
        set_f64(
            &single,
            "throughput_guard_retention_percent",
            &mut cfg.throughput_guard_retention_percent,
        )?;
        set_f64(
            &single,
            "throughput_guard_dl_floor_kbps",
            &mut cfg.throughput_guard_dl_floor_kbps,
        )?;
        set_f64(
            &single,
            "throughput_guard_ul_floor_kbps",
            &mut cfg.throughput_guard_ul_floor_kbps,
        )?;
        set_f64(
            &single,
            "throughput_reference_dl_p20_kbps",
            &mut cfg.throughput_reference_dl_p20_kbps,
        )?;
        set_f64(
            &single,
            "throughput_reference_dl_p50_kbps",
            &mut cfg.throughput_reference_dl_p50_kbps,
        )?;
        set_f64(
            &single,
            "throughput_reference_ul_p20_kbps",
            &mut cfg.throughput_reference_ul_p20_kbps,
        )?;
        set_f64(
            &single,
            "throughput_reference_ul_p50_kbps",
            &mut cfg.throughput_reference_ul_p50_kbps,
        )?;
        if !adaptive_dl_cap_configured {
            cfg.adaptive_ceiling_dl_cap_kbps = cfg.max_dl_shaper_rate_kbps;
        }
        if !adaptive_ul_cap_configured {
            cfg.adaptive_ceiling_ul_cap_kbps = cfg.max_ul_shaper_rate_kbps;
        }
        set_f64(
            &single,
            "connection_active_thr_kbps",
            &mut cfg.connection_active_thr_kbps,
        )?;
        set_bool(
            &single,
            "enable_sleep_function",
            &mut cfg.enable_sleep_function,
        )?;
        set_f64(
            &single,
            "sustained_idle_sleep_thr_s",
            &mut cfg.sustained_idle_sleep_thr_s,
        )?;
        set_bool(
            &single,
            "min_shaper_rates_enforcement",
            &mut cfg.min_shaper_rates_enforcement,
        )?;
        set_usize(&single, "stall_detection_thr", &mut cfg.stall_detection_thr)?;
        set_f64(
            &single,
            "connection_stall_thr_kbps",
            &mut cfg.connection_stall_thr_kbps,
        )?;
        set_f64(
            &single,
            "global_ping_response_timeout_s",
            &mut cfg.global_ping_response_timeout_s,
        )?;
        set_string(&single, "pinger_method", &mut cfg.pinger_method);
        set_string(&single, "ping_extra_args", &mut cfg.ping_extra_args);
        set_string(&single, "ping_prefix_string", &mut cfg.ping_prefix_string);
        set_f64(
            &single,
            "irtt_session_duration_m",
            &mut cfg.irtt_session_duration_m,
        )?;
        set_string(&single, "reflectors_url", &mut cfg.reflectors_url);
        set_usize(
            &single,
            "reflectors_url_skip_lines",
            &mut cfg.reflectors_url_skip_lines,
        )?;
        set_bool(
            &single,
            "randomize_reflectors",
            &mut cfg.randomize_reflectors,
        )?;
        set_bool(
            &single,
            "retain_reflector_stats",
            &mut cfg.retain_reflector_stats,
        )?;
        set_usize(&single, "no_pingers", &mut cfg.no_pingers)?;
        set_f64(
            &single,
            "reflector_ping_interval_s",
            &mut cfg.reflector_ping_interval_s,
        )?;
        set_f64(
            &single,
            "reflector_health_check_interval_s",
            &mut cfg.reflector_health_check_interval_s,
        )?;
        set_f64(
            &single,
            "reflector_response_deadline_s",
            &mut cfg.reflector_response_deadline_s,
        )?;
        set_usize(
            &single,
            "reflector_misbehaving_detection_window",
            &mut cfg.reflector_misbehaving_detection_window,
        )?;
        set_usize(
            &single,
            "reflector_misbehaving_detection_thr",
            &mut cfg.reflector_misbehaving_detection_thr,
        )?;
        set_f64(
            &single,
            "reflector_replacement_interval_mins",
            &mut cfg.reflector_replacement_interval_mins,
        )?;
        set_f64(
            &single,
            "reflector_comparison_interval_mins",
            &mut cfg.reflector_comparison_interval_mins,
        )?;
        set_f64(
            &single,
            "reflector_sum_owd_baselines_delta_thr_ms",
            &mut cfg.reflector_sum_owd_baselines_delta_thr_ms,
        )?;
        set_f64(
            &single,
            "reflector_owd_delta_ewma_delta_thr_ms",
            &mut cfg.reflector_owd_delta_ewma_delta_thr_ms,
        )?;
        set_u64(
            &single,
            "monitor_achieved_rates_interval_ms",
            &mut cfg.monitor_achieved_rates_interval_ms,
        )?;
        set_usize(
            &single,
            "bufferbloat_detection_window",
            &mut cfg.bufferbloat_detection_window,
        )?;
        set_usize(
            &single,
            "bufferbloat_detection_thr",
            &mut cfg.bufferbloat_detection_thr,
        )?;
        set_f64(&single, "high_load_thr", &mut cfg.high_load_thr)?;
        set_f64(
            &single,
            "dl_owd_delta_delay_thr_ms",
            &mut cfg.dl_owd_delta_delay_thr_ms,
        )?;
        set_f64(
            &single,
            "ul_owd_delta_delay_thr_ms",
            &mut cfg.ul_owd_delta_delay_thr_ms,
        )?;
        set_f64(
            &single,
            "dl_avg_owd_delta_max_adjust_up_thr_ms",
            &mut cfg.dl_avg_owd_delta_max_adjust_up_thr_ms,
        )?;
        set_f64(
            &single,
            "ul_avg_owd_delta_max_adjust_up_thr_ms",
            &mut cfg.ul_avg_owd_delta_max_adjust_up_thr_ms,
        )?;
        set_f64(
            &single,
            "dl_avg_owd_delta_max_adjust_down_thr_ms",
            &mut cfg.dl_avg_owd_delta_max_adjust_down_thr_ms,
        )?;
        set_f64(
            &single,
            "ul_avg_owd_delta_max_adjust_down_thr_ms",
            &mut cfg.ul_avg_owd_delta_max_adjust_down_thr_ms,
        )?;
        set_f64(
            &single,
            "alpha_baseline_increase",
            &mut cfg.alpha_baseline_increase,
        )?;
        set_f64(
            &single,
            "alpha_baseline_decrease",
            &mut cfg.alpha_baseline_decrease,
        )?;
        set_f64(&single, "alpha_delta_ewma", &mut cfg.alpha_delta_ewma)?;
        set_f64(
            &single,
            "shaper_rate_min_adjust_down_bufferbloat",
            &mut cfg.shaper_rate_min_adjust_down_bufferbloat,
        )?;
        set_f64(
            &single,
            "shaper_rate_max_adjust_down_bufferbloat",
            &mut cfg.shaper_rate_max_adjust_down_bufferbloat,
        )?;
        set_f64(
            &single,
            "shaper_rate_min_adjust_up_load_high",
            &mut cfg.shaper_rate_min_adjust_up_load_high,
        )?;
        set_f64(
            &single,
            "shaper_rate_max_adjust_up_load_high",
            &mut cfg.shaper_rate_max_adjust_up_load_high,
        )?;
        set_f64(
            &single,
            "shaper_rate_adjust_down_load_low",
            &mut cfg.shaper_rate_adjust_down_load_low,
        )?;
        set_f64(
            &single,
            "shaper_rate_adjust_up_load_low",
            &mut cfg.shaper_rate_adjust_up_load_low,
        )?;
        set_u64(
            &single,
            "bufferbloat_refractory_period_ms",
            &mut cfg.bufferbloat_refractory_period_ms,
        )?;
        set_u64(
            &single,
            "decay_refractory_period_ms",
            &mut cfg.decay_refractory_period_ms,
        )?;
        set_bool(
            &single,
            "output_processing_stats",
            &mut cfg.output_processing_stats,
        )?;
        set_bool(
            &single,
            "output_summary_stats",
            &mut cfg.output_summary_stats,
        )?;
        set_bool(&single, "output_load_stats", &mut cfg.output_load_stats)?;
        set_bool(
            &single,
            "output_reflector_stats",
            &mut cfg.output_reflector_stats,
        )?;
        set_bool(&single, "output_cake_changes", &mut cfg.output_cake_changes)?;
        set_bool(&single, "output_cpu_stats", &mut cfg.output_cpu_stats)?;
        set_bool(
            &single,
            "output_cpu_raw_stats",
            &mut cfg.output_cpu_raw_stats,
        )?;
        set_bool(
            &single,
            "graph_history_enabled",
            &mut cfg.graph_history_enabled,
        )?;
        set_u64(
            &single,
            "graph_history_interval_s",
            &mut cfg.graph_history_interval_s,
        )?;
        set_bool(&single, "log_to_file", &mut cfg.log_to_file)?;
        set_bool(&single, "debug", &mut cfg.debug)?;
        set_bool(
            &single,
            "log_DEBUG_messages_to_syslog",
            &mut cfg.log_debug_messages_to_syslog,
        )?;
        set_u64(
            &single,
            "log_file_max_time_mins",
            &mut cfg.log_file_max_time_mins,
        )?;
        set_u64(
            &single,
            "log_file_max_size_KB",
            &mut cfg.log_file_max_size_kb,
        )?;
        set_string(
            &single,
            "log_file_path_override",
            &mut cfg.log_file_path_override,
        );
        set_u64(
            &single,
            "log_file_buffer_size_B",
            &mut cfg.log_file_buffer_size_b,
        )?;
        set_u64(
            &single,
            "log_file_buffer_timeout_ms",
            &mut cfg.log_file_buffer_timeout_ms,
        )?;
        set_bool(
            &single,
            "log_file_export_compress",
            &mut cfg.log_file_export_compress,
        )?;
        set_f64(&single, "startup_wait_s", &mut cfg.startup_wait_s)?;
        set_f64(
            &single,
            "if_up_check_interval_s",
            &mut cfg.if_up_check_interval_s,
        )?;
        set_u64(
            &single,
            "monitor_cpu_usage_interval_ms",
            &mut cfg.monitor_cpu_usage_interval_ms,
        )?;

        if let Some(values) = lists.get("reflector") {
            cfg.reflectors = values.iter().filter(|v| !v.is_empty()).cloned().collect();
        } else if let Some(value) = single.get("reflectors") {
            cfg.reflectors = value
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .collect();
        }
        if let Some(values) = lists.get("transport_probe_url") {
            cfg.transport_probe_urls = values
                .iter()
                .filter(|value| value.starts_with("http://") || value.starts_with("https://"))
                .cloned()
                .collect();
        } else if let Some(value) = single.get("transport_probe_urls") {
            cfg.transport_probe_urls = value
                .split_whitespace()
                .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
                .map(str::to_string)
                .collect();
        }
        if let Some(values) = lists.get("irtt_server") {
            cfg.irtt_servers = values.iter().filter(|v| !v.is_empty()).cloned().collect();
        } else if let Some(value) = single
            .get("irtt_servers")
            .or_else(|| single.get("irtt_server"))
        {
            cfg.irtt_servers = value
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .collect();
        }
        deduplicate_list(&mut cfg.irtt_servers);
        cfg.load_reflectors_url();
        cfg.deduplicate_reflectors();
        if cfg.randomize_reflectors {
            randomize_reflectors(&mut cfg.reflectors);
            randomize_reflectors(&mut cfg.irtt_servers);
        }
        if cfg.pinger_method == "irtt" {
            cfg.reflectors = cfg.irtt_servers.clone();
        }

        let (history_budget_kib, history_instance_count) = load_global_history_config()?;
        cfg.graph_history_ram_budget_kib = history_budget_kib;
        cfg.graph_history_instance_count = history_instance_count;

        cfg.normalize_paths();
        cfg.refresh_wire_packet_sizes();
        cfg.validate()?;
        Ok(cfg)
    }

    fn load_reflectors_url(&mut self) {
        if self.reflectors_url.is_empty() {
            return;
        }

        let configured_reflectors = self.reflectors.clone();
        match fetch_url_text(&self.reflectors_url) {
            Ok(data) => {
                let reflectors = parse_reflector_candidates(&data, self.reflectors_url_skip_lines);
                if reflectors.is_empty() {
                    eprintln!(
                        "WARNING: reflectors_url {} returned no usable reflectors; using configured list",
                        self.reflectors_url
                    );
                } else {
                    let mut merged = configured_reflectors;
                    merged.extend(reflectors);
                    self.reflectors = merged;
                }
            }
            Err(e) => eprintln!(
                "WARNING: failed to fetch reflectors_url {}: {e}; using configured list",
                self.reflectors_url
            ),
        }
    }

    fn deduplicate_reflectors(&mut self) {
        deduplicate_list(&mut self.reflectors);
    }

    fn normalize_paths(&mut self) {
        if self.rx_bytes_path.is_empty() {
            self.rx_bytes_path = format!("/sys/class/net/{}/statistics/tx_bytes", self.dl_if);
        }
        if self.tx_bytes_path.is_empty() {
            let counter = if self.ul_if.starts_with("ifb") || self.ul_if.starts_with("veth") {
                "rx_bytes"
            } else {
                "tx_bytes"
            };
            self.tx_bytes_path = format!("/sys/class/net/{}/statistics/{counter}", self.ul_if);
        }
    }

    fn refresh_wire_packet_sizes(&mut self) {
        self.dl_max_wire_packet_size_bits = interface_max_wire_packet_size_bits(&self.dl_if);
        self.ul_max_wire_packet_size_bits = interface_max_wire_packet_size_bits(&self.ul_if);
    }

    fn validate(&self) -> Result<(), String> {
        self.route_spec().validate()?;
        if !(1.0..=300.0).contains(&self.route_stability_s) {
            return Err("route_stability_s must be between 1 and 300".to_string());
        }
        if !(1.0..=60.0).contains(&self.route_check_interval_s) {
            return Err("route_check_interval_s must be between 1 and 60".to_string());
        }
        if self.pinger_method != "fping"
            && self.pinger_method != "fping-ts"
            && self.pinger_method != "tsping"
            && self.pinger_method != "irtt"
            && self.pinger_method != "ping"
        {
            return Err(format!(
                "pinger_method={} is configured, but this Rust package currently supports fping, fping-ts, tsping, irtt, and ping",
                self.pinger_method
            ));
        }
        if self.pinger_method == "irtt" && self.irtt_servers.is_empty() {
            return Err("pinger_method=irtt requires at least one irtt_server".to_string());
        }
        if self.reflectors.is_empty() {
            return Err("at least one reflector is required".to_string());
        }
        if self.pinger_method == "irtt" {
            if self
                .reflectors
                .iter()
                .any(|server| !is_valid_irtt_server_candidate(server))
            {
                return Err(
                    "irtt_server may contain only host, IPv4, IPv6, and optional port characters"
                        .to_string(),
                );
            }
        } else if self
            .reflectors
            .iter()
            .any(|reflector| !is_valid_reflector_candidate(reflector))
        {
            return Err(
                "reflectors may contain only host, IPv4, or IPv6 address characters".to_string(),
            );
        }
        if self.no_pingers == 0 {
            return Err("no_pingers must be greater than zero".to_string());
        }
        if self.no_pingers > self.reflectors.len() {
            return Err("no_pingers cannot exceed reflector count".to_string());
        }
        if self.connection_active_thr_kbps > self.min_dl_shaper_rate_kbps {
            return Err(
                "connection_active_thr_kbps cannot be greater than min_dl_shaper_rate_kbps"
                    .to_string(),
            );
        }
        if self.connection_active_thr_kbps > self.min_ul_shaper_rate_kbps {
            return Err(
                "connection_active_thr_kbps cannot be greater than min_ul_shaper_rate_kbps"
                    .to_string(),
            );
        }
        if self.adaptive_ceiling_enabled {
            if !self.adaptive_ceiling_dl_cap_kbps.is_finite()
                || self.adaptive_ceiling_dl_cap_kbps < self.max_dl_shaper_rate_kbps
            {
                return Err(
                    "adaptive_ceiling_dl_cap_kbps cannot be lower than max_dl_shaper_rate_kbps"
                        .to_string(),
                );
            }
            if !self.adaptive_ceiling_ul_cap_kbps.is_finite()
                || self.adaptive_ceiling_ul_cap_kbps < self.max_ul_shaper_rate_kbps
            {
                return Err(
                    "adaptive_ceiling_ul_cap_kbps cannot be lower than max_ul_shaper_rate_kbps"
                        .to_string(),
                );
            }
            if !self.adaptive_ceiling_hold_time_s.is_finite()
                || self.adaptive_ceiling_hold_time_s <= 0.0
            {
                return Err("adaptive_ceiling_hold_time_s must be greater than zero".to_string());
            }
            if !self.adaptive_ceiling_growth_percent.is_finite()
                || self.adaptive_ceiling_growth_percent <= 0.0
                || self.adaptive_ceiling_growth_percent > 10.0
            {
                return Err(
                    "adaptive_ceiling_growth_percent must be greater than zero and no more than 10"
                        .to_string(),
                );
            }
            if !self.adaptive_ceiling_probe_duration_s.is_finite()
                || self.adaptive_ceiling_probe_duration_s <= 0.0
            {
                return Err(
                    "adaptive_ceiling_probe_duration_s must be greater than zero".to_string(),
                );
            }
            if !self.adaptive_ceiling_cooldown_s.is_finite()
                || self.adaptive_ceiling_cooldown_s < 0.0
            {
                return Err("adaptive_ceiling_cooldown_s must not be negative".to_string());
            }
            if !self.adaptive_ceiling_failed_bound_ttl_s.is_finite()
                || self.adaptive_ceiling_failed_bound_ttl_s <= 0.0
            {
                return Err(
                    "adaptive_ceiling_failed_bound_ttl_s must be greater than zero".to_string(),
                );
            }
        }
        if self.transport_controller_enabled && !self.transport_latency_enabled {
            return Err(
                "transport_controller_enabled requires transport_latency_enabled".to_string(),
            );
        }
        if self.transport_latency_enabled {
            let backend = TransportProbeBackend::parse(&self.transport_probe_backend)
                .ok_or_else(|| "transport_probe_backend is unsupported".to_string())?;
            if self.transport_probe_endpoint.is_empty()
                || self.transport_probe_endpoint.len() > 512
                || self
                    .transport_probe_endpoint
                    .chars()
                    .any(char::is_whitespace)
            {
                return Err("transport_probe_endpoint is invalid".to_string());
            }
            let endpoint_matches = match backend {
                TransportProbeBackend::WebSocket => {
                    self.transport_probe_endpoint.starts_with("ws://")
                        || self.transport_probe_endpoint.starts_with("wss://")
                }
                TransportProbeBackend::TcpConnect => {
                    self.transport_probe_endpoint.starts_with("tcp://")
                }
                TransportProbeBackend::PersistentHttp => {
                    self.transport_probe_endpoint.starts_with("https://")
                }
                TransportProbeBackend::LegacyHttp => {
                    self.transport_probe_endpoint.starts_with("http://")
                        || self.transport_probe_endpoint.starts_with("https://")
                }
            };
            if !endpoint_matches {
                return Err(
                    "transport_probe_endpoint scheme does not match transport_probe_backend"
                        .to_string(),
                );
            }
            if self.transport_controller_enabled && !backend.trusted() {
                return Err(
                    "transport_controller_enabled requires a trusted native transport backend"
                        .to_string(),
                );
            }
            if !self.transport_probe_idle_interval_s.is_finite()
                || self.transport_probe_idle_interval_s < 5.0
                || self.transport_probe_idle_interval_s > 3600.0
            {
                return Err(
                    "transport_probe_idle_interval_s must be between 5 and 3600".to_string()
                );
            }
            if !self.transport_probe_loaded_interval_s.is_finite()
                || self.transport_probe_loaded_interval_s < 0.5
                || self.transport_probe_loaded_interval_s > 60.0
            {
                return Err(
                    "transport_probe_loaded_interval_s must be between 0.5 and 60".to_string(),
                );
            }
            if !(1..=30).contains(&self.transport_probe_timeout_s) {
                return Err("transport_probe_timeout_s must be between 1 and 30".to_string());
            }
            if !self.transport_load_hold_s.is_finite()
                || !(1.0..=30.0).contains(&self.transport_load_hold_s)
            {
                return Err("transport_load_hold_s must be between 1 and 30".to_string());
            }
            if !self.transport_cpu_max_percent.is_finite()
                || !(50.0..=100.0).contains(&self.transport_cpu_max_percent)
            {
                return Err("transport_cpu_max_percent must be between 50 and 100".to_string());
            }
            if !self.rating_load_window_s.is_finite()
                || !(0.5..=10.0).contains(&self.rating_load_window_s)
            {
                return Err("rating_load_window_s must be between 0.5 and 10".to_string());
            }
            if !self.rating_load_enter_ratio.is_finite()
                || !(0.10..=1.0).contains(&self.rating_load_enter_ratio)
            {
                return Err("rating_load_enter_ratio must be between 0.10 and 1.0".to_string());
            }
            if !self.rating_load_exit_ratio.is_finite()
                || !(0.05..1.0).contains(&self.rating_load_exit_ratio)
                || self.rating_load_exit_ratio >= self.rating_load_enter_ratio
            {
                return Err(
                    "rating_load_exit_ratio must be below rating_load_enter_ratio".to_string(),
                );
            }
            if !self.rating_load_hold_s.is_finite()
                || !(0.2..=10.0).contains(&self.rating_load_hold_s)
            {
                return Err("rating_load_hold_s must be between 0.2 and 10".to_string());
            }
            if !self.rating_load_dropout_s.is_finite()
                || !(0.2..=10.0).contains(&self.rating_load_dropout_s)
            {
                return Err("rating_load_dropout_s must be between 0.2 and 10".to_string());
            }
            if !self.rating_load_min_kbps.is_finite() || self.rating_load_min_kbps < 0.0 {
                return Err("rating_load_min_kbps must not be negative".to_string());
            }
            if !self.rating_load_dominance_ratio.is_finite()
                || !(1.1..=10.0).contains(&self.rating_load_dominance_ratio)
            {
                return Err("rating_load_dominance_ratio must be between 1.1 and 10".to_string());
            }
            if !self.rating_capture_min_enter_ratio.is_finite()
                || !(0.05..=0.50).contains(&self.rating_capture_min_enter_ratio)
            {
                return Err(
                    "rating_capture_min_enter_ratio must be between 0.05 and 0.50".to_string(),
                );
            }
            if !self.rating_capture_peak_factor.is_finite()
                || !(0.20..=0.80).contains(&self.rating_capture_peak_factor)
            {
                return Err("rating_capture_peak_factor must be between 0.20 and 0.80".to_string());
            }
            if !self.rating_capture_contamination_ratio.is_finite()
                || !(0.05..=0.50).contains(&self.rating_capture_contamination_ratio)
            {
                return Err(
                    "rating_capture_contamination_ratio must be between 0.05 and 0.50".to_string(),
                );
            }
            if !self.rating_capture_ack_ratio.is_finite()
                || !(0.01..=0.25).contains(&self.rating_capture_ack_ratio)
            {
                return Err("rating_capture_ack_ratio must be between 0.01 and 0.25".to_string());
            }
            if !self.rating_episode_gap_s.is_finite()
                || !(5.0..=120.0).contains(&self.rating_episode_gap_s)
            {
                return Err("rating_episode_gap_s must be between 5 and 120".to_string());
            }
            if !self.quality_target_delay_ms.is_finite()
                || !(5.0..=200.0).contains(&self.quality_target_delay_ms)
            {
                return Err("quality_target_delay_ms must be between 5 and 200".to_string());
            }
            if !(1..=10).contains(&self.quality_search_max_steps) {
                return Err("quality_search_max_steps must be between 1 and 10".to_string());
            }
            if !self.quality_search_observe_s.is_finite()
                || !(2.0..=120.0).contains(&self.quality_search_observe_s)
            {
                return Err("quality_search_observe_s must be between 2 and 120".to_string());
            }
            if !self.quality_search_cooldown_s.is_finite()
                || !(30.0..=86400.0).contains(&self.quality_search_cooldown_s)
            {
                return Err("quality_search_cooldown_s must be between 30 and 86400".to_string());
            }
        }
        if !(50.0..=100.0).contains(&self.throughput_guard_retention_percent) {
            return Err(
                "throughput_guard_retention_percent must be between 50 and 100".to_string(),
            );
        }
        for (name, value) in [
            (
                "throughput_guard_dl_floor_kbps",
                self.throughput_guard_dl_floor_kbps,
            ),
            (
                "throughput_guard_ul_floor_kbps",
                self.throughput_guard_ul_floor_kbps,
            ),
            (
                "throughput_reference_dl_p20_kbps",
                self.throughput_reference_dl_p20_kbps,
            ),
            (
                "throughput_reference_dl_p50_kbps",
                self.throughput_reference_dl_p50_kbps,
            ),
            (
                "throughput_reference_ul_p20_kbps",
                self.throughput_reference_ul_p20_kbps,
            ),
            (
                "throughput_reference_ul_p50_kbps",
                self.throughput_reference_ul_p50_kbps,
            ),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(format!("{name} must not be negative"));
            }
        }
        if self.sustained_idle_sleep_thr_s < 0.0 {
            return Err("sustained_idle_sleep_thr_s must not be negative".to_string());
        }
        if self.stall_detection_thr == 0 {
            return Err("stall_detection_thr must be greater than zero".to_string());
        }
        if self.connection_stall_thr_kbps < 0.0 {
            return Err("connection_stall_thr_kbps must not be negative".to_string());
        }
        if self.global_ping_response_timeout_s <= 0.0 {
            return Err("global_ping_response_timeout_s must be greater than zero".to_string());
        }
        if self.pinger_method == "irtt" && self.irtt_session_duration_m <= 0.0 {
            return Err("irtt_session_duration_m must be greater than zero".to_string());
        }
        if self.bufferbloat_detection_thr > self.bufferbloat_detection_window {
            return Err(
                "bufferbloat_detection_thr cannot exceed bufferbloat_detection_window".to_string(),
            );
        }
        if !(1..=60).contains(&self.graph_history_interval_s) {
            return Err("graph_history_interval_s must be between 1 and 60".to_string());
        }
        if let Some(budget_kib) = self.graph_history_ram_budget_kib {
            if !(GRAPH_HISTORY_MIN_BUDGET_KIB..=GRAPH_HISTORY_HARD_MAX_KIB).contains(&budget_kib) {
                return Err(format!(
                    "graph_history_ram_budget_kib must be auto or between {} and {}",
                    GRAPH_HISTORY_MIN_BUDGET_KIB, GRAPH_HISTORY_HARD_MAX_KIB
                ));
            }
        }
        if self.reflector_health_check_interval_s <= 0.0 {
            return Err("reflector_health_check_interval_s must be greater than zero".to_string());
        }
        if self.reflector_response_deadline_s <= 0.0 {
            return Err("reflector_response_deadline_s must be greater than zero".to_string());
        }
        if self.reflector_response_deadline_s < self.reflector_ping_interval_s {
            return Err(
                "reflector_response_deadline_s cannot be lower than reflector_ping_interval_s"
                    .to_string(),
            );
        }
        if self.reflector_misbehaving_detection_window == 0 {
            return Err(
                "reflector_misbehaving_detection_window must be greater than zero".to_string(),
            );
        }
        if self.reflector_misbehaving_detection_thr == 0 {
            return Err(
                "reflector_misbehaving_detection_thr must be greater than zero".to_string(),
            );
        }
        if self.reflector_misbehaving_detection_thr > self.reflector_misbehaving_detection_window {
            return Err(
                "reflector_misbehaving_detection_thr cannot exceed reflector_misbehaving_detection_window"
                    .to_string(),
            );
        }
        if self.reflector_replacement_interval_mins < 0.0 {
            return Err("reflector_replacement_interval_mins must not be negative".to_string());
        }
        if self.reflector_comparison_interval_mins < 0.0 {
            return Err("reflector_comparison_interval_mins must not be negative".to_string());
        }
        if self.reflector_sum_owd_baselines_delta_thr_ms < 0.0 {
            return Err(
                "reflector_sum_owd_baselines_delta_thr_ms must not be negative".to_string(),
            );
        }
        if self.reflector_owd_delta_ewma_delta_thr_ms < 0.0 {
            return Err("reflector_owd_delta_ewma_delta_thr_ms must not be negative".to_string());
        }
        if self.dl_if == self.ul_if {
            return Err("dl_if and ul_if must be different".to_string());
        }
        Ok(())
    }

    fn route_spec(&self) -> RouteSpec {
        RouteSpec::new(&self.route_mode, &self.mwan3_member, &self.ul_if)
    }

    fn rating_load_config(&self) -> RatingLoadConfig {
        RatingLoadConfig {
            window: Duration::from_secs_f64(self.rating_load_window_s),
            enter_ratio: self.rating_load_enter_ratio,
            exit_ratio: self.rating_load_exit_ratio,
            hold: Duration::from_secs_f64(self.rating_load_hold_s),
            dropout: Duration::from_secs_f64(self.rating_load_dropout_s),
            min_rate_kbps: self.rating_load_min_kbps,
            dominance_ratio: self.rating_load_dominance_ratio,
            capture_min_enter_ratio: self.rating_capture_min_enter_ratio,
            capture_peak_factor: self.rating_capture_peak_factor,
            capture_contamination_ratio: self.rating_capture_contamination_ratio,
            capture_ack_ratio: self.rating_capture_ack_ratio,
        }
    }

    fn run_dir(&self) -> PathBuf {
        PathBuf::from(format!("/var/run/cake-autorate/{}", self.instance))
    }

    fn log_path(&self) -> PathBuf {
        let name = format!("cake-autorate.{}.log", self.instance);
        if self.log_file_path_override.is_empty() {
            PathBuf::from("/var/log").join(name)
        } else {
            PathBuf::from(&self.log_file_path_override).join(name)
        }
    }

    fn graph_history_path(&self) -> PathBuf {
        self.run_dir().join("history.csv")
    }

    fn rating_capture_path(&self) -> PathBuf {
        self.run_dir().join("rating-capture")
    }
}

#[derive(Clone, Debug)]
struct Sample {
    reflector: String,
    seq: String,
    timestamp: f64,
    rtt_ms: f64,
    dl_owd_us: f64,
    ul_owd_us: f64,
    timestamped_owd: bool,
}

#[derive(Clone, Debug)]
struct ReflectorState {
    last_seen: Instant,
    offences: VecDeque<bool>,
    offence_sum: usize,
    samples: u64,
    last_rtt_ms: f64,
}

impl ReflectorState {
    fn new(now: Instant, window: usize) -> Self {
        Self {
            last_seen: now,
            offences: filled_bool_window(window),
            offence_sum: 0,
            samples: 0,
            last_rtt_ms: 0.0,
        }
    }

    fn push_offence(&mut self, offence: bool) {
        if self.offences.len() == self.offences.capacity()
            && self.offences.pop_front().unwrap_or(false)
        {
            self.offence_sum = self.offence_sum.saturating_sub(1);
        }

        self.offences.push_back(offence);
        if offence {
            self.offence_sum = self.offence_sum.saturating_add(1);
        }
    }
}

#[derive(Clone, Debug)]
struct ReflectorHealth {
    states: HashMap<String, ReflectorState>,
    last_health_check: Instant,
    last_replacement: Instant,
    last_comparison: Instant,
    next_candidate_idx: usize,
    replacement_slot: usize,
}

impl ReflectorHealth {
    fn new(cfg: &Config, active: &[String]) -> Self {
        let now = Instant::now();
        let mut states = HashMap::new();

        for reflector in active {
            states.insert(
                reflector.clone(),
                ReflectorState::new(now, cfg.reflector_misbehaving_detection_window),
            );
        }

        Self {
            states,
            last_health_check: now,
            last_replacement: now,
            last_comparison: now,
            next_candidate_idx: active.len(),
            replacement_slot: 0,
        }
    }

    fn observe_sample(&mut self, cfg: &Config, sample: &Sample) {
        let now = Instant::now();
        let state = self
            .states
            .entry(sample.reflector.clone())
            .or_insert_with(|| {
                ReflectorState::new(now, cfg.reflector_misbehaving_detection_window)
            });
        state.last_seen = now;
        state.samples = state.samples.saturating_add(1);
        state.last_rtt_ms = sample.rtt_ms;

        let late = sample.rtt_ms > cfg.reflector_response_deadline_s * 1000.0;
        if late {
            state.push_offence(true);
        }
    }

    fn timeout(&self, cfg: &Config) -> Duration {
        let interval = Duration::from_secs_f64(cfg.reflector_health_check_interval_s.max(0.1));
        interval
            .checked_sub(self.last_health_check.elapsed())
            .unwrap_or_else(|| Duration::from_millis(1))
            .min(Duration::from_secs(1))
    }

    fn check(&mut self, cfg: &Config, active: &mut [String], controller: &mut Controller) -> bool {
        let now = Instant::now();
        let health_interval =
            Duration::from_secs_f64(cfg.reflector_health_check_interval_s.max(0.1));

        if now.duration_since(self.last_health_check) < health_interval {
            return false;
        }

        self.last_health_check = now;
        self.ensure_active_states(cfg, active, now);

        if self.maybe_compare_reflectors(cfg, active, controller) {
            return true;
        }

        if self.maybe_periodic_refresh(cfg, active, controller) {
            return true;
        }

        self.check_response_deadlines(cfg, active, controller)
    }

    fn ensure_active_states(&mut self, cfg: &Config, active: &[String], now: Instant) {
        for reflector in active {
            self.states.entry(reflector.clone()).or_insert_with(|| {
                ReflectorState::new(now, cfg.reflector_misbehaving_detection_window)
            });
        }
    }

    fn maybe_compare_reflectors(
        &mut self,
        cfg: &Config,
        active: &mut [String],
        controller: &mut Controller,
    ) -> bool {
        let interval =
            Duration::from_secs_f64((cfg.reflector_comparison_interval_mins * 60.0).max(0.0));
        if interval == Duration::ZERO || self.last_comparison.elapsed() < interval {
            return false;
        }

        self.last_comparison = Instant::now();

        let mut stats = Vec::new();
        for reflector in active.iter() {
            let Some(dl_baseline) = controller.dl_baseline_us.get(reflector).copied() else {
                return false;
            };
            let Some(ul_baseline) = controller.ul_baseline_us.get(reflector).copied() else {
                return false;
            };
            let Some(dl_ewma) = controller.dl_ewma_us.get(reflector).copied() else {
                return false;
            };
            let Some(ul_ewma) = controller.ul_ewma_us.get(reflector).copied() else {
                return false;
            };
            stats.push((
                reflector.clone(),
                dl_baseline + ul_baseline,
                dl_ewma,
                ul_ewma,
            ));
        }

        if stats.is_empty() {
            return false;
        }

        let min_sum = stats
            .iter()
            .map(|(_, sum, _, _)| *sum)
            .fold(f64::INFINITY, f64::min);
        let min_dl_ewma = stats
            .iter()
            .map(|(_, _, dl, _)| *dl)
            .fold(f64::INFINITY, f64::min);
        let min_ul_ewma = stats
            .iter()
            .map(|(_, _, _, ul)| *ul)
            .fold(f64::INFINITY, f64::min);
        let sum_thr_us = cfg.reflector_sum_owd_baselines_delta_thr_ms * 1000.0;
        let ewma_thr_us = cfg.reflector_owd_delta_ewma_delta_thr_ms * 1000.0;

        for (idx, (reflector, sum, dl_ewma, ul_ewma)) in stats.iter().enumerate() {
            let sum_delta = sum - min_sum;
            let dl_delta = dl_ewma - min_dl_ewma;
            let ul_delta = ul_ewma - min_ul_ewma;

            if cfg.output_reflector_stats {
                controller.log(
                    "REFLECTOR",
                    &format!(
                        "{}; {:.0}; {:.0}; {:.0}; {:.0}; {:.0}; {:.0}; {:.0}; {:.0}; {:.0}; {:.0}; {:.0}; {:.0}",
                        reflector,
                        min_sum,
                        sum,
                        sum_delta,
                        sum_thr_us,
                        min_dl_ewma,
                        dl_ewma,
                        dl_delta,
                        ewma_thr_us,
                        min_ul_ewma,
                        ul_ewma,
                        ul_delta,
                        ewma_thr_us
                    ),
                );
            }

            if sum_delta > sum_thr_us {
                return self.replace_active_reflector(
                    cfg,
                    active,
                    idx,
                    "baseline delta above threshold",
                    controller,
                );
            }

            if dl_delta > ewma_thr_us || ul_delta > ewma_thr_us {
                return self.replace_active_reflector(
                    cfg,
                    active,
                    idx,
                    "EWMA delta above threshold",
                    controller,
                );
            }
        }

        false
    }

    fn maybe_periodic_refresh(
        &mut self,
        cfg: &Config,
        active: &mut [String],
        controller: &mut Controller,
    ) -> bool {
        let interval =
            Duration::from_secs_f64((cfg.reflector_replacement_interval_mins * 60.0).max(0.0));
        if interval == Duration::ZERO || self.last_replacement.elapsed() < interval {
            return false;
        }

        if active.is_empty() || cfg.reflectors.len() <= active.len() {
            self.last_replacement = Instant::now();
            return false;
        }

        let slot = self.replacement_slot % active.len();
        self.replacement_slot = self.replacement_slot.wrapping_add(1);
        self.replace_active_reflector(cfg, active, slot, "periodic refresh", controller)
    }

    fn check_response_deadlines(
        &mut self,
        cfg: &Config,
        active: &mut [String],
        controller: &mut Controller,
    ) -> bool {
        let deadline = Duration::from_secs_f64(cfg.reflector_response_deadline_s.max(0.1));
        let now = Instant::now();

        for idx in 0..active.len() {
            let reflector = active[idx].clone();
            let state = self.states.entry(reflector.clone()).or_insert_with(|| {
                ReflectorState::new(now, cfg.reflector_misbehaving_detection_window)
            });
            let offence = now.duration_since(state.last_seen) > deadline;
            state.push_offence(offence);

            if offence {
                controller.log(
                    "DEBUG",
                    &format!(
                        "no ping response from reflector {reflector} within reflector_response_deadline_s={}",
                        cfg.reflector_response_deadline_s
                    ),
                );
            }

            if state.offence_sum >= cfg.reflector_misbehaving_detection_thr {
                return self.replace_active_reflector(
                    cfg,
                    active,
                    idx,
                    "response deadline offences",
                    controller,
                );
            }
        }

        false
    }

    fn replace_active_reflector(
        &mut self,
        cfg: &Config,
        active: &mut [String],
        index: usize,
        reason: &str,
        controller: &mut Controller,
    ) -> bool {
        let Some(next) = next_spare_reflector(&cfg.reflectors, active, self.next_candidate_idx)
        else {
            let reflector = active.get(index).cloned().unwrap_or_default();
            controller.log(
                "DEBUG",
                &format!("reflector {reflector} needs replacement ({reason}) but no spare reflector is configured"),
            );
            if let Some(state) = self.states.get_mut(&reflector) {
                state.offences.clear();
                state.offence_sum = 0;
            }
            return false;
        };

        self.next_candidate_idx = next.0.wrapping_add(1);
        let old = active[index].clone();
        active[index] = next.1.clone();
        self.last_replacement = Instant::now();

        if !cfg.retain_reflector_stats {
            controller.dl_baseline_us.remove(&old);
            controller.ul_baseline_us.remove(&old);
            controller.dl_ewma_us.remove(&old);
            controller.ul_ewma_us.remove(&old);
            self.states.remove(&old);
        }

        self.states.insert(
            next.1.clone(),
            ReflectorState::new(Instant::now(), cfg.reflector_misbehaving_detection_window),
        );

        controller.log(
            "DEBUG",
            &format!("replacing reflector {old} with {}: {reason}", next.1),
        );
        true
    }
}

fn next_spare_reflector(
    candidates: &[String],
    active: &[String],
    start: usize,
) -> Option<(usize, String)> {
    if candidates.len() <= active.len() {
        return None;
    }

    for offset in 0..candidates.len() {
        let idx = (start + offset) % candidates.len();
        let candidate = &candidates[idx];
        if !active.iter().any(|reflector| reflector == candidate) {
            return Some((idx, candidate.clone()));
        }
    }

    None
}

#[derive(Clone, Copy)]
enum LoadKind {
    High,
    Low,
    Idle,
}

struct RateMonitor {
    rx_path: PathBuf,
    tx_path: PathBuf,
    min_interval: Duration,
    prev_rx: u64,
    prev_tx: u64,
    last: Instant,
    last_dl_kbps: f64,
    last_ul_kbps: f64,
}

#[derive(Clone, Copy, Debug)]
struct RateSample {
    dl_kbps: f64,
    ul_kbps: f64,
    fresh: bool,
}

impl RateMonitor {
    fn new(rx_path: &str, tx_path: &str, interval_ms: u64) -> io::Result<Self> {
        Ok(Self {
            rx_path: PathBuf::from(rx_path),
            tx_path: PathBuf::from(tx_path),
            min_interval: Duration::from_millis(interval_ms.max(25)),
            prev_rx: read_u64_file(rx_path).unwrap_or(0),
            prev_tx: read_u64_file(tx_path).unwrap_or(0),
            last: Instant::now(),
            last_dl_kbps: 0.0,
            last_ul_kbps: 0.0,
        })
    }

    fn sample(&mut self) -> RateSample {
        let now = Instant::now();
        let interval = now.duration_since(self.last);
        if interval < self.min_interval {
            return RateSample {
                dl_kbps: self.last_dl_kbps,
                ul_kbps: self.last_ul_kbps,
                fresh: false,
            };
        }
        let elapsed = interval.as_secs_f64();
        let rx = read_u64_file(&self.rx_path).unwrap_or(self.prev_rx);
        let tx = read_u64_file(&self.tx_path).unwrap_or(self.prev_tx);
        let dl = rx.saturating_sub(self.prev_rx) as f64 * 8.0 / elapsed / 1000.0;
        let ul = tx.saturating_sub(self.prev_tx) as f64 * 8.0 / elapsed / 1000.0;
        self.prev_rx = rx;
        self.prev_tx = tx;
        self.last = now;
        self.last_dl_kbps = dl;
        self.last_ul_kbps = ul;
        RateSample {
            dl_kbps: dl,
            ul_kbps: ul,
            fresh: true,
        }
    }
}

fn qdisc_output_has_cake(output: &str) -> bool {
    output.lines().any(|line| {
        let mut fields = line.split_whitespace();
        fields.next() == Some("qdisc")
            && fields
                .next()
                .map(|kind| kind == "cake" || kind == "cake_mq")
                .unwrap_or(false)
    })
}

fn ingress_output_targets_ifb(output: &str, ifb: &str) -> bool {
    !ifb.is_empty() && output.lines().any(|line| line.contains(ifb))
}

fn tc_output(args: &[&str]) -> Result<String, String> {
    let output = Command::new("tc")
        .args(args)
        .output()
        .map_err(|error| format!("failed to execute tc: {error}"))?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if error.is_empty() {
            format!("tc {} failed with {}", args.join(" "), output.status)
        } else {
            error
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn inspect_managed_sqm(cfg: &Config) -> Result<(), String> {
    if !cfg.manage_sqm || !cfg.sqm_enabled {
        return Ok(());
    }
    if !Path::new(&cfg.rx_bytes_path).is_file() {
        return Err(format!("download counter is missing for {}", cfg.dl_if));
    }
    if !Path::new(&cfg.tx_bytes_path).is_file() {
        return Err(format!("upload counter is missing for {}", cfg.ul_if));
    }

    let dl_qdisc = tc_output(&["qdisc", "show", "dev", &cfg.dl_if])?;
    if !qdisc_output_has_cake(&dl_qdisc) {
        return Err(format!("CAKE qdisc is missing on {}", cfg.dl_if));
    }
    let ul_qdisc = tc_output(&["qdisc", "show", "dev", &cfg.ul_if])?;
    if !qdisc_output_has_cake(&ul_qdisc) {
        return Err(format!("CAKE qdisc is missing on {}", cfg.ul_if));
    }
    if cfg.dl_if.starts_with("ifb") {
        let ingress = tc_output(&["filter", "show", "dev", &cfg.sqm_interface, "ingress"])?;
        if !ingress_output_targets_ifb(&ingress, &cfg.dl_if) {
            return Err(format!(
                "ingress redirect from {} to {} is missing",
                cfg.sqm_interface, cfg.dl_if
            ));
        }
    }
    Ok(())
}

fn recover_managed_sqm(cfg: &Config) -> Result<(), String> {
    let helper = env::var("CAKE_AUTORATE_SQM_RECOVER")
        .unwrap_or_else(|_| "/usr/libexec/cake-autorate-rs/sqm-recover".to_string());
    let output = Command::new(&helper)
        .arg(&cfg.instance)
        .output()
        .map_err(|error| format!("failed to execute {helper}: {error}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!("SQM recovery helper failed with {}", output.status)
        } else {
            detail
        });
    }
    inspect_managed_sqm(cfg)
}

#[derive(Clone, Debug)]
struct TransportProbeRequest {
    control_valid: bool,
    dl_loaded: bool,
    ul_loaded: bool,
    rating_phase: RatingPhase,
}

#[derive(Clone, Debug)]
struct TransportProbeResult {
    control_valid: bool,
    endpoint: String,
    dl_loaded: bool,
    ul_loaded: bool,
    rating_phase: RatingPhase,
    latency_ms: Option<f64>,
    error: Option<String>,
    route_identity: Option<String>,
    backend: String,
    trusted: bool,
    raw_samples_ms: Vec<f64>,
    discarded_samples: usize,
    server_processing_ms: f64,
    connection_reused: bool,
}

struct TransportProbeRuntime {
    requests: SyncSender<TransportProbeRequest>,
    results: Receiver<TransportProbeResult>,
    in_flight: bool,
    last_started: Instant,
    load_candidate: (bool, bool),
    load_candidate_since: Instant,
}

#[derive(Clone, Debug)]
struct ExternalIpResult {
    value: Option<String>,
    error: Option<String>,
    route_identity: Option<String>,
}

struct ExternalIpRuntime {
    requests: SyncSender<()>,
    results: Receiver<ExternalIpResult>,
    in_flight: bool,
    last_started: Instant,
}

fn transport_result_matches_route(
    result_identity: Option<&str>,
    current_identity: Option<&str>,
) -> bool {
    result_identity.is_some() && result_identity == current_identity
}

fn uplink_error_code(state: UplinkState, reason: &str) -> Option<&'static str> {
    if state != UplinkState::Offline {
        return None;
    }
    let reason = reason.to_ascii_lowercase();
    if reason.contains("route mismatch") || reason.contains("default route uses") {
        Some("route_mismatch")
    } else if reason.contains("disabled")
        || reason.contains("offline")
        || reason.contains("interface is down")
    {
        Some("member_offline")
    } else if reason.contains("unavailable") || reason.contains("not found") {
        Some("interface_unavailable")
    } else {
        Some("route_unavailable")
    }
}

fn transport_error_code(error: Option<&str>) -> Option<&'static str> {
    let error = error?.to_ascii_lowercase();
    if error.contains("timeout") || error.contains("timed out") {
        Some("transport_timeout")
    } else if error.contains("route changed") || error.contains("different uplink") {
        Some("route_mismatch")
    } else {
        Some("transport_error")
    }
}

impl TransportProbeRuntime {
    fn spawn(cfg: &Config) -> Self {
        let (request_tx, request_rx) = mpsc::sync_channel::<TransportProbeRequest>(1);
        let (result_tx, result_rx) = mpsc::channel::<TransportProbeResult>();
        let route_spec = cfg.route_spec();
        let timeout_s = cfg.transport_probe_timeout_s;
        let backend = TransportProbeBackend::parse(&cfg.transport_probe_backend)
            .unwrap_or(TransportProbeBackend::LegacyHttp);
        let endpoint = cfg.transport_probe_endpoint.clone();
        thread::spawn(move || {
            let mut engine: Option<(String, TransportProbeEngine)> = None;
            let mut route_inspector = RouteInspector::new(route_spec.clone());
            while let Ok(request) = request_rx.recv() {
                let before = route_inspector.inspect();
                let measurement = match before.as_ref() {
                    Ok(snapshot) if snapshot.online => {
                        let identity = snapshot.stable_key();
                        if backend == TransportProbeBackend::LegacyHttp {
                            let started = Instant::now();
                            match run_transport_probe(&route_spec, timeout_s, &endpoint, snapshot) {
                                Ok(()) => Ok(transport_probe::TransportProbeSample {
                                    backend,
                                    endpoint: endpoint.clone(),
                                    rtt_ms: started.elapsed().as_secs_f64() * 1000.0,
                                    raw_samples_ms: Vec::new(),
                                    discarded_samples: 0,
                                    server_processing_ms: 0.0,
                                    trusted: false,
                                    connection_reused: false,
                                }),
                                Err(error) => Err(error),
                            }
                        } else {
                            (|| -> Result<transport_probe::TransportProbeSample, String> {
                                let replace = engine
                                    .as_ref()
                                    .map(|(key, _)| key != &identity)
                                    .unwrap_or(true);
                                if replace {
                                    let binding = RouteBinding {
                                        device: snapshot.identity.device.clone(),
                                        source_ip: snapshot.identity.source_ip.clone(),
                                        fwmark: snapshot.identity.fwmark.clone(),
                                    };
                                    engine = Some((
                                        identity.clone(),
                                        TransportProbeEngine::new(
                                            backend,
                                            endpoint.clone(),
                                            binding,
                                            Duration::from_secs(timeout_s),
                                        )?,
                                    ));
                                }
                                engine
                                    .as_mut()
                                    .ok_or_else(|| "transport engine is unavailable".to_string())?
                                    .1
                                    .probe()
                            })()
                        }
                    }
                    Ok(snapshot) => Err(if snapshot.reason.is_empty() {
                        "transport route is offline".to_string()
                    } else {
                        snapshot.reason.clone()
                    }),
                    Err(error) => Err(error.clone()),
                };
                let after = route_inspector.inspect();
                let stable_identity = match (&before, &after) {
                    (Ok(before), Ok(after))
                        if before.online
                            && after.online
                            && before.stable_key() == after.stable_key() =>
                    {
                        Some(after.stable_key())
                    }
                    _ => None,
                };
                let (
                    latency_ms,
                    error,
                    backend_name,
                    trusted,
                    raw_samples_ms,
                    discarded_samples,
                    server_processing_ms,
                    connection_reused,
                ) = match measurement {
                    Ok(sample) if stable_identity.is_some() => (
                        Some(sample.rtt_ms),
                        None,
                        sample.backend.as_str().to_string(),
                        sample.trusted,
                        sample.raw_samples_ms,
                        sample.discarded_samples,
                        sample.server_processing_ms,
                        sample.connection_reused,
                    ),
                    Ok(_) => (
                        None,
                        Some("route changed during native transport probe".to_string()),
                        backend.as_str().to_string(),
                        false,
                        Vec::new(),
                        0,
                        0.0,
                        false,
                    ),
                    Err(error) => (
                        None,
                        Some(error),
                        backend.as_str().to_string(),
                        false,
                        Vec::new(),
                        0,
                        0.0,
                        false,
                    ),
                };
                if result_tx
                    .send(TransportProbeResult {
                        control_valid: request.control_valid,
                        endpoint: endpoint.clone(),
                        dl_loaded: request.dl_loaded,
                        ul_loaded: request.ul_loaded,
                        rating_phase: request.rating_phase,
                        latency_ms,
                        error,
                        route_identity: stable_identity,
                        backend: backend_name,
                        trusted,
                        raw_samples_ms,
                        discarded_samples,
                        server_processing_ms,
                        connection_reused,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        Self {
            requests: request_tx,
            results: result_rx,
            in_flight: false,
            last_started: Instant::now()
                .checked_sub(Duration::from_secs_f64(cfg.transport_probe_idle_interval_s))
                .unwrap_or_else(Instant::now),
            load_candidate: (false, false),
            load_candidate_since: Instant::now(),
        }
    }

    fn drain(&mut self, controller: &mut Controller) {
        while let Ok(result) = self.results.try_recv() {
            self.in_flight = false;
            controller.on_transport_probe(result);
        }
    }

    fn maybe_start(
        &mut self,
        cfg: &Config,
        dl_rate: f64,
        ul_rate: f64,
        shaper_rates: (f64, f64),
        rating: &RatingLoadSnapshot,
        quality_baseline_ready: bool,
    ) {
        let (dl_shaper, ul_shaper) = shaper_rates;
        let raw_dl_loaded = percent(dl_rate, dl_shaper) >= cfg.high_load_thr * 100.0;
        let raw_ul_loaded = percent(ul_rate, ul_shaper) >= cfg.high_load_thr * 100.0;
        let phase = (raw_dl_loaded, raw_ul_loaded);
        if phase != self.load_candidate {
            self.load_candidate = phase;
            self.load_candidate_since = Instant::now();
        }
        let raw_loaded = raw_dl_loaded || raw_ul_loaded;
        let control_valid = !raw_loaded
            || self.load_candidate_since.elapsed()
                >= Duration::from_secs_f64(cfg.transport_load_hold_s);
        if self.in_flight || (raw_loaded && !control_valid && !rating.phase.loaded()) {
            return;
        }
        let dl_loaded = control_valid && raw_dl_loaded;
        let ul_loaded = control_valid && raw_ul_loaded;
        let loaded = dl_loaded || ul_loaded;
        let any_loaded = loaded || rating.phase.loaded();
        let interval = transport_probe_interval_s(cfg, any_loaded, quality_baseline_ready);
        if self.last_started.elapsed() < Duration::from_secs_f64(interval) {
            return;
        }

        if self
            .requests
            .try_send(TransportProbeRequest {
                control_valid,
                dl_loaded,
                ul_loaded,
                rating_phase: rating.phase,
            })
            .is_ok()
        {
            self.in_flight = true;
            self.last_started = Instant::now();
        }
    }
}

fn transport_probe_interval_s(cfg: &Config, any_loaded: bool, baseline_ready: bool) -> f64 {
    if any_loaded {
        cfg.transport_probe_loaded_interval_s
    } else if !baseline_ready {
        cfg.transport_probe_idle_interval_s
            .min(TRANSPORT_BASELINE_LEARNING_INTERVAL_S)
    } else {
        cfg.transport_probe_idle_interval_s
    }
}

impl ExternalIpRuntime {
    fn spawn(route_spec: RouteSpec) -> Self {
        let (request_tx, request_rx) = mpsc::sync_channel::<()>(1);
        let (result_tx, result_rx) = mpsc::channel::<ExternalIpResult>();
        thread::spawn(move || {
            while request_rx.recv().is_ok() {
                let result = run_external_ip_probe(&route_spec, 5);
                let (value, error, route_identity) = match result {
                    Ok((value, snapshot)) => (Some(value), None, Some(snapshot.stable_key())),
                    Err(error) => (None, Some(error), None),
                };
                if result_tx
                    .send(ExternalIpResult {
                        value,
                        error,
                        route_identity,
                    })
                    .is_err()
                {
                    break;
                }
            }
        });

        Self {
            requests: request_tx,
            results: result_rx,
            in_flight: false,
            last_started: Instant::now()
                .checked_sub(Duration::from_secs(60))
                .unwrap_or_else(Instant::now),
        }
    }

    fn drain(&mut self, controller: &mut Controller) {
        while let Ok(result) = self.results.try_recv() {
            self.in_flight = false;
            match (result.value, result.error) {
                (Some(value), _) => {
                    if transport_result_matches_route(
                        result.route_identity.as_deref(),
                        controller.route_identity.as_deref(),
                    ) {
                        controller.set_route_external_ip(value);
                    } else {
                        controller.log(
                            "DEBUG",
                            "discarded external IP result from a stale or different uplink route",
                        );
                    }
                }
                (_, Some(error)) => controller.log(
                    "DEBUG",
                    &format!("failed to refresh routed external IP: {error}"),
                ),
                _ => {}
            }
        }
    }

    fn maybe_start(&mut self, allowed: bool) {
        if !allowed || self.in_flight || self.last_started.elapsed() < Duration::from_secs(60) {
            return;
        }
        if self.requests.try_send(()).is_ok() {
            self.in_flight = true;
            self.last_started = Instant::now();
        }
    }
}

fn run_external_ip_probe(
    route_spec: &RouteSpec,
    timeout_s: u64,
) -> Result<(String, RouteSnapshot), String> {
    let before = routing::inspect_route(route_spec)?;
    if !before.online {
        return Err(if before.reason.is_empty() {
            "uplink route is offline".to_string()
        } else {
            before.reason
        });
    }
    let value = routing::external_ipv4(route_spec, timeout_s)?;
    let after = routing::inspect_route(route_spec)?;
    if !after.online || before.stable_key() != after.stable_key() {
        return Err("route changed during external IP query".to_string());
    }
    Ok((value, after))
}

fn run_transport_probe(
    route_spec: &RouteSpec,
    timeout_s: u64,
    endpoint: &str,
    snapshot: &RouteSnapshot,
) -> Result<(), String> {
    if !snapshot.online {
        return Err(if snapshot.reason.is_empty() {
            format!("route {} is offline", snapshot.identity.mode)
        } else {
            snapshot.reason.clone()
        });
    }
    if route_spec.effective_mode()? == RouteMode::Main && !snapshot.active {
        return Err(if snapshot.reason.is_empty() {
            format!("main route does not use {}", snapshot.identity.device)
        } else {
            snapshot.reason.clone()
        });
    }

    let mut command = if route_spec.effective_mode()? == RouteMode::Main {
        let mut curl = Command::new("curl");
        curl.arg("-4")
            .arg("-fsS")
            .arg("--max-time")
            .arg(timeout_s.to_string())
            .arg("--interface")
            .arg(&route_spec.expected_device)
            .arg("-o")
            .arg("/dev/null")
            .arg(endpoint);
        match curl.stdout(Stdio::null()).stderr(Stdio::null()).status() {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => return Err(format!("curl exited with {status}")),
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                return Err(format!("failed to execute curl: {error}"));
            }
            Err(_) => routing::routed_command(route_spec, "", "uclient-fetch")?,
        }
    } else {
        routing::routed_command(route_spec, "", "uclient-fetch")?
    };
    let status = command
        .arg("-4")
        .arg("-q")
        .arg("-T")
        .arg(timeout_s.to_string())
        .arg("-O")
        .arg("/dev/null")
        .arg(endpoint)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| format!("failed to execute routed uclient-fetch: {error}"))?;
    if !status.success() {
        return Err(format!("routed uclient-fetch exited with {status}"));
    }

    Ok(())
}

#[derive(Clone, Debug)]
struct CpuCounters {
    total: u64,
    idle: u64,
}

#[derive(Clone, Debug)]
struct CpuSnapshot {
    counters: Vec<CpuCounters>,
    raw_lines: Vec<String>,
}

#[derive(Clone, Debug)]
struct CpuStats {
    total_percent: f64,
    core_percentages: Vec<f64>,
    raw_lines: Vec<String>,
}

struct CpuMonitor {
    previous: CpuSnapshot,
}

impl CpuMonitor {
    fn new() -> io::Result<Self> {
        Ok(Self {
            previous: read_cpu_snapshot()?,
        })
    }

    fn sample(&mut self) -> io::Result<Option<CpuStats>> {
        let current = read_cpu_snapshot()?;
        let mut percentages = Vec::new();

        for (prev, next) in self.previous.counters.iter().zip(current.counters.iter()) {
            let total_delta = next.total.saturating_sub(prev.total);
            let idle_delta = next.idle.saturating_sub(prev.idle);

            if total_delta == 0 {
                percentages.push(0.0);
            } else {
                let busy = total_delta.saturating_sub(idle_delta) as f64;
                percentages.push((busy * 100.0 / total_delta as f64).clamp(0.0, 100.0));
            }
        }

        self.previous = current.clone();

        if percentages.is_empty() {
            return Ok(None);
        }

        Ok(Some(CpuStats {
            total_percent: percentages[0],
            core_percentages: percentages.iter().skip(1).copied().collect(),
            raw_lines: current.raw_lines,
        }))
    }
}

#[derive(Clone, Debug)]
struct StatusSnapshot {
    dl_rate: f64,
    ul_rate: f64,
    dl_load_pct: f64,
    ul_load_pct: f64,
    dl_delay_count: usize,
    ul_delay_count: usize,
    avg_dl_delta: f64,
    avg_ul_delta: f64,
    sample: Sample,
    active_reflectors: Vec<String>,
    health: Option<ReflectorHealth>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct MemoryInfo {
    total_kib: u64,
    available_kib: u64,
}

#[derive(Clone, Debug, Default)]
struct HistoryBudgetSnapshot {
    configured_kib: Option<u64>,
    safe_max_kib: u64,
    effective_total_kib: u64,
    instance_budget_kib: u64,
    used_total_kib: u64,
    used_instance_kib: u64,
    memory: MemoryInfo,
    instances: usize,
    paused_low_memory: bool,
}

fn read_memory_info() -> io::Result<MemoryInfo> {
    let data = fs::read_to_string("/proc/meminfo")?;
    let mut total_kib = 0;
    let mut available_kib = 0;
    for line in data.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let parsed = value
            .split_whitespace()
            .next()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0);
        match key {
            "MemTotal" => total_kib = parsed,
            "MemAvailable" => available_kib = parsed,
            _ => {}
        }
    }
    if total_kib == 0 || available_kib == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "MemTotal or MemAvailable is missing from /proc/meminfo",
        ));
    }
    Ok(MemoryInfo {
        total_kib,
        available_kib,
    })
}

fn history_usage_bytes(root: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| fs::metadata(entry.path().join("history.csv")).ok())
        .map(|metadata| metadata.len())
        .sum()
}

fn history_safe_max_kib(available_plus_history_kib: u64) -> u64 {
    match available_plus_history_kib {
        value if value < 64 * 1024 => 256,
        value if value < 128 * 1024 => 1024,
        value if value < 256 * 1024 => 2 * 1024,
        value if value < 512 * 1024 => 8 * 1024,
        value if value < 768 * 1024 => 16 * 1024,
        value if value < 1024 * 1024 => 32 * 1024,
        _ => GRAPH_HISTORY_HARD_MAX_KIB,
    }
}

fn automatic_history_budget_kib(safe_max_kib: u64) -> u64 {
    const PRESETS: &[u64] = &[
        256,
        512,
        1024,
        2 * 1024,
        4 * 1024,
        8 * 1024,
        16 * 1024,
        32 * 1024,
        64 * 1024,
        100 * 1024,
    ];
    let target = (safe_max_kib / 4).max(GRAPH_HISTORY_MIN_BUDGET_KIB);
    PRESETS
        .iter()
        .copied()
        .filter(|value| *value <= target && *value <= safe_max_kib)
        .max()
        .unwrap_or_else(|| safe_max_kib.min(GRAPH_HISTORY_MIN_BUDGET_KIB))
}

fn compute_history_budget(
    configured_kib: Option<u64>,
    instances: usize,
    memory: MemoryInfo,
    used_total_kib: u64,
    used_instance_kib: u64,
) -> HistoryBudgetSnapshot {
    let instances = instances.max(1);
    let safe_max_kib = history_safe_max_kib(memory.available_kib.saturating_add(used_total_kib));
    let requested_kib = configured_kib
        .unwrap_or_else(|| automatic_history_budget_kib(safe_max_kib))
        .min(GRAPH_HISTORY_HARD_MAX_KIB);
    let reserve_kib = (memory.total_kib / 20).max(32 * 1024);
    let pressure_cap_kib = used_total_kib
        .saturating_add(memory.available_kib)
        .saturating_sub(reserve_kib);
    let paused_low_memory = memory.available_kib < GRAPH_HISTORY_CRITICAL_AVAILABLE_KIB;
    let effective_total_kib = if paused_low_memory {
        0
    } else {
        requested_kib.min(safe_max_kib).min(pressure_cap_kib)
    };

    HistoryBudgetSnapshot {
        configured_kib,
        safe_max_kib,
        effective_total_kib,
        instance_budget_kib: effective_total_kib / instances as u64,
        used_total_kib,
        used_instance_kib,
        memory,
        instances,
        paused_low_memory,
    }
}

fn history_budget_snapshot(cfg: &Config) -> HistoryBudgetSnapshot {
    let memory = read_memory_info().unwrap_or_default();
    let root = Path::new("/var/run/cake-autorate");
    let used_total_kib = history_usage_bytes(root).div_ceil(1024);
    let used_instance_kib = fs::metadata(cfg.graph_history_path())
        .map(|metadata| metadata.len().div_ceil(1024))
        .unwrap_or(0);
    let (configured_kib, instances) = load_global_history_config().unwrap_or((
        cfg.graph_history_ram_budget_kib,
        cfg.graph_history_instance_count,
    ));
    compute_history_budget(
        configured_kib,
        instances,
        memory,
        used_total_kib,
        used_instance_kib,
    )
}

struct LogFile {
    path: PathBuf,
    file: BufWriter<File>,
    opened_at: Instant,
    bytes_written: u64,
    bytes_pending: u64,
    last_flush: Instant,
}

impl LogFile {
    fn open(path: PathBuf) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let bytes_written = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        let file = BufWriter::new(OpenOptions::new().create(true).append(true).open(&path)?);

        Ok(Self {
            path,
            file,
            opened_at: Instant::now(),
            bytes_written,
            bytes_pending: 0,
            last_flush: Instant::now(),
        })
    }

    fn write_line(
        &mut self,
        line: &str,
        max_age: Duration,
        max_size_bytes: u64,
        buffer_size_bytes: u64,
        buffer_timeout: Duration,
        compress: bool,
    ) -> io::Result<()> {
        let pending = line.len() as u64 + 1;
        let age_exceeded = max_age > Duration::ZERO && self.opened_at.elapsed() >= max_age;
        let size_exceeded =
            max_size_bytes > 0 && self.bytes_written.saturating_add(pending) > max_size_bytes;

        if age_exceeded || size_exceeded {
            self.rotate(compress)?;
        }

        writeln!(self.file, "{line}")?;
        self.bytes_written = self.bytes_written.saturating_add(pending);
        self.bytes_pending = self.bytes_pending.saturating_add(pending);

        let flush_by_size = buffer_size_bytes == 0 || self.bytes_pending >= buffer_size_bytes;
        let flush_by_time =
            buffer_timeout == Duration::ZERO || self.last_flush.elapsed() >= buffer_timeout;

        if flush_by_size || flush_by_time {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()?;
        self.bytes_pending = 0;
        self.last_flush = Instant::now();
        Ok(())
    }

    fn rotate(&mut self, compress: bool) -> io::Result<()> {
        let _ = self.flush();

        let rotated = rotated_log_path(&self.path);
        match fs::rename(&self.path, &rotated) {
            Ok(()) => {
                if compress {
                    let _ = Command::new("gzip").arg("-f").arg(&rotated).status();
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }

        self.file = BufWriter::new(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?,
        );
        self.opened_at = Instant::now();
        self.bytes_written = 0;
        self.bytes_pending = 0;
        self.last_flush = Instant::now();
        Ok(())
    }
}

struct Controller {
    cfg: Config,
    log: Option<LogFile>,
    rate_monitor: RateMonitor,
    cpu_monitor: Option<CpuMonitor>,
    dl_baseline_us: HashMap<String, f64>,
    ul_baseline_us: HashMap<String, f64>,
    dl_ewma_us: HashMap<String, f64>,
    ul_ewma_us: HashMap<String, f64>,
    dl_delays: VecDeque<bool>,
    ul_delays: VecDeque<bool>,
    dl_delta_us: VecDeque<f64>,
    ul_delta_us: VecDeque<f64>,
    shaper_dl: f64,
    shaper_ul: f64,
    adaptive_dl: AdaptiveCeilingDirection,
    adaptive_ul: AdaptiveCeilingDirection,
    transport_latency: TransportLatencyTracker,
    transport_latency_dl: TransportLatencyTracker,
    transport_latency_ul: TransportLatencyTracker,
    quality_grade: QualityGradeTracker,
    rating_load: RatingLoadDetector,
    rating_load_snapshot: RatingLoadSnapshot,
    quality_search_dl: QualitySearchDirection,
    quality_search_ul: QualitySearchDirection,
    quality_dl_class: QualityClass,
    quality_ul_class: QualityClass,
    transport_backend: String,
    transport_trusted: bool,
    transport_raw_samples: usize,
    transport_discarded_samples: usize,
    transport_server_processing_ms: f64,
    transport_connection_reused: bool,
    transport_rejected_reason: Option<String>,
    transport_last_rejected_reason: Option<String>,
    transport_last_rejected_at: Option<f64>,
    transport_bad_windows_dl: u8,
    transport_bad_windows_ul: u8,
    throughput_floor_dl: f64,
    throughput_floor_ul: f64,
    last_set_dl: u64,
    last_set_ul: u64,
    last_shaper_attempt_dl: Instant,
    last_shaper_attempt_ul: Instant,
    last_bb_dl: Instant,
    last_bb_ul: Instant,
    last_decay_dl: Instant,
    last_decay_ul: Instant,
    last_cpu_sample: Instant,
    last_graph_history_sample: Instant,
    last_history_budget_refresh: Instant,
    history_budget: HistoryBudgetSnapshot,
    history_sample_count: u64,
    cpu_total_percent: Option<f64>,
    cpu_core_percentages: Vec<f64>,
    started_at: f64,
    run_state: String,
    uplink_state: UplinkState,
    uplink_reason: String,
    route_snapshot: Option<RouteSnapshot>,
    route_identity: Option<String>,
    route_external_ip: String,
    sqm_runtime_state: String,
    sqm_runtime_healthy: bool,
    sqm_runtime_reason: String,
    sqm_recovery_attempts: u64,
    sqm_last_recovery_attempt: Option<Instant>,
    sqm_last_recovery_at: Option<f64>,
    last_status: Option<StatusSnapshot>,
    last_status_publish: Instant,
}

impl Controller {
    fn new(mut cfg: Config) -> Result<Self, String> {
        ensure_run_dir(&cfg.run_dir())
            .map_err(|e| format!("failed to create run directory: {e}"))?;
        let history_budget = history_budget_snapshot(&cfg);
        if cfg.graph_history_enabled && history_budget.instance_budget_kib > 0 {
            if let Err(e) = File::create(cfg.graph_history_path()) {
                eprintln!("WARNING: failed to initialize graph history: {e}");
            }
        } else {
            let _ = fs::remove_file(cfg.graph_history_path());
        }
        wait_for_path(&cfg.rx_bytes_path, cfg.if_up_check_interval_s)?;
        wait_for_path(&cfg.tx_bytes_path, cfg.if_up_check_interval_s)?;
        cfg.refresh_wire_packet_sizes();

        let log = if cfg.log_to_file {
            Some(LogFile::open(cfg.log_path()).map_err(|e| {
                format!("failed to open log file {}: {e}", cfg.log_path().display())
            })?)
        } else {
            None
        };

        let rate_monitor = RateMonitor::new(
            &cfg.rx_bytes_path,
            &cfg.tx_bytes_path,
            cfg.monitor_achieved_rates_interval_ms,
        )
        .map_err(|e| format!("failed to create rate monitor: {e}"))?;
        let cpu_monitor = match CpuMonitor::new() {
            Ok(monitor) => Some(monitor),
            Err(e) => {
                eprintln!("WARNING: failed to initialize CPU monitor: {e}");
                None
            }
        };
        let now = Instant::now();
        let rating_load = RatingLoadDetector::new(now);
        let rating_load_snapshot = rating_load.snapshot(
            now,
            cfg.rating_load_config(),
            cfg.base_dl_shaper_rate_kbps,
            cfg.base_ul_shaper_rate_kbps,
        );
        let adaptive_dl = AdaptiveCeilingDirection::new(
            cfg.max_dl_shaper_rate_kbps,
            cfg.adaptive_ceiling_dl_cap_kbps,
        );
        let adaptive_ul = AdaptiveCeilingDirection::new(
            cfg.max_ul_shaper_rate_kbps,
            cfg.adaptive_ceiling_ul_cap_kbps,
        );
        let throughput_floor_dl = throughput_floor(ThroughputGuardInput {
            enabled: cfg.transport_controller_enabled && cfg.throughput_guard_enabled,
            configured_min_kbps: cfg.min_dl_shaper_rate_kbps,
            configured_base_kbps: cfg.base_dl_shaper_rate_kbps,
            observed_p20_kbps: cfg.throughput_reference_dl_p20_kbps,
            observed_p50_kbps: cfg.throughput_reference_dl_p50_kbps,
            absolute_floor_kbps: cfg.throughput_guard_dl_floor_kbps,
            retention_percent: cfg.throughput_guard_retention_percent,
        })
        .min(adaptive_dl.absolute_cap_kbps());
        let throughput_floor_ul = throughput_floor(ThroughputGuardInput {
            enabled: cfg.transport_controller_enabled && cfg.throughput_guard_enabled,
            configured_min_kbps: cfg.min_ul_shaper_rate_kbps,
            configured_base_kbps: cfg.base_ul_shaper_rate_kbps,
            observed_p20_kbps: cfg.throughput_reference_ul_p20_kbps,
            observed_p50_kbps: cfg.throughput_reference_ul_p50_kbps,
            absolute_floor_kbps: cfg.throughput_guard_ul_floor_kbps,
            retention_percent: cfg.throughput_guard_retention_percent,
        })
        .min(adaptive_ul.absolute_cap_kbps());

        Ok(Self {
            shaper_dl: cfg.base_dl_shaper_rate_kbps,
            shaper_ul: cfg.base_ul_shaper_rate_kbps,
            adaptive_dl,
            adaptive_ul,
            transport_latency: TransportLatencyTracker::new(),
            transport_latency_dl: TransportLatencyTracker::new(),
            transport_latency_ul: TransportLatencyTracker::new(),
            quality_grade: QualityGradeTracker::new(cfg.rating_episode_gap_s),
            rating_load,
            rating_load_snapshot,
            quality_search_dl: QualitySearchDirection::new(),
            quality_search_ul: QualitySearchDirection::new(),
            quality_dl_class: QualityClass::Learning,
            quality_ul_class: QualityClass::Learning,
            transport_backend: cfg.transport_probe_backend.clone(),
            transport_trusted: false,
            transport_raw_samples: 0,
            transport_discarded_samples: 0,
            transport_server_processing_ms: 0.0,
            transport_connection_reused: false,
            transport_rejected_reason: None,
            transport_last_rejected_reason: None,
            transport_last_rejected_at: None,
            transport_bad_windows_dl: 0,
            transport_bad_windows_ul: 0,
            throughput_floor_dl,
            throughput_floor_ul,
            last_set_dl: 0,
            last_set_ul: 0,
            last_shaper_attempt_dl: now,
            last_shaper_attempt_ul: now,
            last_bb_dl: now,
            last_bb_ul: now,
            last_decay_dl: now,
            last_decay_ul: now,
            last_cpu_sample: now,
            last_graph_history_sample: now,
            last_history_budget_refresh: now,
            history_budget,
            history_sample_count: 0,
            cpu_total_percent: None,
            cpu_core_percentages: Vec::new(),
            run_state: "RUNNING".to_string(),
            uplink_state: UplinkState::Offline,
            uplink_reason: "route not checked".to_string(),
            route_snapshot: None,
            route_identity: None,
            route_external_ip: String::new(),
            sqm_runtime_state: if cfg.manage_sqm && cfg.sqm_enabled {
                "HEALTHY".to_string()
            } else {
                "UNMANAGED".to_string()
            },
            sqm_runtime_healthy: true,
            sqm_runtime_reason: String::new(),
            sqm_recovery_attempts: 0,
            sqm_last_recovery_attempt: None,
            sqm_last_recovery_at: None,
            last_status: None,
            last_status_publish: now.checked_sub(STATUS_PUBLISH_INTERVAL).unwrap_or(now),
            dl_baseline_us: HashMap::new(),
            ul_baseline_us: HashMap::new(),
            dl_ewma_us: HashMap::new(),
            ul_ewma_us: HashMap::new(),
            dl_delays: filled_bool_window(cfg.bufferbloat_detection_window),
            ul_delays: filled_bool_window(cfg.bufferbloat_detection_window),
            dl_delta_us: filled_f64_window(cfg.bufferbloat_detection_window),
            ul_delta_us: filled_f64_window(cfg.bufferbloat_detection_window),
            started_at: epoch_secs(),
            cfg,
            log,
            rate_monitor,
            cpu_monitor,
        })
    }

    fn start(&mut self) {
        self.log("INFO", "starting cake-autorate-rs");
        self.apply_shaper("dl");
        self.apply_shaper("ul");
    }

    fn sample_rates(&mut self) -> RateSample {
        self.rate_monitor.sample()
    }

    fn set_sqm_runtime_status(&mut self, state: &str, healthy: bool, reason: &str) {
        let changed = self.sqm_runtime_state != state
            || self.sqm_runtime_healthy != healthy
            || self.sqm_runtime_reason != reason;
        self.sqm_runtime_state = state.to_string();
        self.sqm_runtime_healthy = healthy;
        self.sqm_runtime_reason = reason.to_string();
        if changed {
            self.log(
                if healthy { "INFO" } else { "ERROR" },
                &format!(
                    "Managed SQM runtime {}{}",
                    state.to_ascii_lowercase(),
                    if reason.is_empty() {
                        String::new()
                    } else {
                        format!(": {reason}")
                    }
                ),
            );
        }
        let _ = self.refresh_status_from_last_sample();
    }

    fn accept_recovered_sqm(&mut self, reason: &str) -> Result<(), String> {
        self.rate_monitor = RateMonitor::new(
            &self.cfg.rx_bytes_path,
            &self.cfg.tx_bytes_path,
            self.cfg.monitor_achieved_rates_interval_ms,
        )
        .map_err(|error| format!("failed to reset interface counters: {error}"))?;
        self.last_set_dl = 0;
        self.last_set_ul = 0;
        self.quality_grade.reset();
        self.reset_uplink_learning(reason);
        self.sqm_last_recovery_at = Some(epoch_secs());
        self.set_sqm_runtime_status("HEALTHY", true, reason);
        self.set_run_state("RUNNING");
        self.apply_shaper("dl");
        self.apply_shaper("ul");
        Ok(())
    }

    fn ensure_managed_sqm(&mut self) -> (bool, bool) {
        if !self.cfg.manage_sqm || !self.cfg.sqm_enabled {
            self.set_sqm_runtime_status("UNMANAGED", true, "");
            return (true, false);
        }

        match inspect_managed_sqm(&self.cfg) {
            Ok(()) if self.sqm_runtime_healthy => (true, false),
            Ok(()) => match self.accept_recovered_sqm("runtime recovered externally") {
                Ok(()) => (true, true),
                Err(error) => {
                    self.set_sqm_runtime_status("ERROR", false, &error);
                    self.set_run_state("ERROR");
                    (false, false)
                }
            },
            Err(initial_reason) => {
                if self
                    .sqm_last_recovery_attempt
                    .map(|attempt| {
                        attempt.elapsed() < Duration::from_secs(SQM_RUNTIME_RECOVERY_COOLDOWN_S)
                    })
                    .unwrap_or(false)
                {
                    self.set_sqm_runtime_status("ERROR", false, &initial_reason);
                    self.set_run_state("ERROR");
                    return (false, false);
                }
                self.sqm_last_recovery_attempt = Some(Instant::now());
                self.sqm_recovery_attempts = self.sqm_recovery_attempts.saturating_add(1);
                self.set_sqm_runtime_status("RECOVERING", false, &initial_reason);
                self.set_run_state("RECOVERING");
                match recover_managed_sqm(&self.cfg) {
                    Ok(()) => match self.accept_recovered_sqm("automatic SQM recovery completed") {
                        Ok(()) => (true, true),
                        Err(error) => {
                            self.set_sqm_runtime_status("ERROR", false, &error);
                            self.set_run_state("ERROR");
                            (false, false)
                        }
                    },
                    Err(error) => {
                        let reason = format!("{initial_reason}; recovery failed: {error}");
                        self.set_sqm_runtime_status("ERROR", false, &reason);
                        self.set_run_state("ERROR");
                        (false, false)
                    }
                }
            }
        }
    }

    fn update_rating_load(
        &mut self,
        now: Instant,
        dl_rate: f64,
        ul_rate: f64,
    ) -> RatingLoadSnapshot {
        self.sync_rating_capture(now);
        self.rating_load_snapshot = self.rating_load.observe(
            now,
            dl_rate,
            ul_rate,
            self.shaper_dl,
            self.shaper_ul,
            self.cfg.rating_load_config(),
        );
        self.rating_load_snapshot.clone()
    }

    fn sync_rating_capture(&mut self, now: Instant) {
        let content = fs::read_to_string(self.cfg.rating_capture_path()).unwrap_or_default();
        let mut fields = content.trim().split('|');
        let token = fields.next().unwrap_or("");
        let mode = fields.next().unwrap_or("");
        let deadline = fields.next().and_then(|value| value.parse::<f64>().ok());
        let requested_phase = fields.next().and_then(RatingPhase::from_capture);
        let background_dl_kbps = fields
            .next()
            .and_then(|value| value.parse::<f64>().ok())
            .unwrap_or(0.0);
        let background_ul_kbps = fields
            .next()
            .and_then(|value| value.parse::<f64>().ok())
            .unwrap_or(0.0);
        if !token.is_empty()
            && matches!(mode, "automatic" | "client")
            && deadline.map(|value| value > epoch_secs()).unwrap_or(false)
        {
            let capture_started = self.rating_load.set_capture(
                Some(token),
                Some(mode),
                requested_phase,
                background_dl_kbps,
                background_ul_kbps,
                now,
            );
            if capture_started {
                self.quality_grade.begin_capture(epoch_secs());
            }
        } else {
            if self.rating_load_snapshot.capture_active {
                if self.rating_load_snapshot.capture_contaminated {
                    self.quality_grade.cancel_capture();
                } else {
                    self.quality_grade.end_capture(epoch_secs());
                }
            }
            self.rating_load
                .set_capture(None, None, None, 0.0, 0.0, now);
            if !content.is_empty() {
                let _ = fs::remove_file(self.cfg.rating_capture_path());
            }
        }
    }

    fn record_transport_rejection(&mut self, reason: &str) {
        self.transport_rejected_reason = Some(reason.to_string());
        self.transport_last_rejected_reason = Some(reason.to_string());
        self.transport_last_rejected_at = Some(epoch_secs());
    }

    fn shaper_rates(&self) -> (f64, f64) {
        (self.shaper_dl, self.shaper_ul)
    }

    fn transport_max_age(&self) -> Duration {
        Duration::from_secs_f64(
            self.cfg.transport_probe_loaded_interval_s * 3.0
                + self.cfg.transport_probe_timeout_s as f64,
        )
    }

    fn transport_clean_for_growth(&mut self, now: Instant) -> bool {
        if !self.cfg.transport_controller_enabled {
            return true;
        }
        let max_age = self.transport_max_age();
        self.transport_latency.expire_loaded(now, max_age);
        let snapshot = self.transport_latency.snapshot(now, true);
        transport_allows_growth(
            true,
            snapshot.confirmed,
            snapshot.sample_age_s,
            max_age.as_secs_f64(),
            snapshot.delta_ms,
            self.cfg.quality_target_delay_ms,
        )
    }

    fn quality_policy(&self, is_dl: bool) -> QualitySearchPolicy {
        QualitySearchPolicy {
            target_delay_ms: self.cfg.quality_target_delay_ms,
            floor_kbps: if is_dl {
                self.throughput_floor_dl
            } else {
                self.throughput_floor_ul
            },
            max_steps: self.cfg.quality_search_max_steps.min(u8::MAX as usize) as u8,
            observe_duration: Duration::from_secs_f64(self.cfg.quality_search_observe_s),
            cooldown: Duration::from_secs_f64(self.cfg.quality_search_cooldown_s),
        }
    }

    fn on_transport_probe(&mut self, result: TransportProbeResult) {
        let now = Instant::now();
        self.transport_backend = result.backend.clone();
        self.transport_trusted = result.trusted;
        self.transport_raw_samples = result.raw_samples_ms.len();
        self.transport_discarded_samples = result.discarded_samples;
        self.transport_server_processing_ms = result.server_processing_ms;
        self.transport_connection_reused = result.connection_reused;
        self.transport_rejected_reason = None;

        if let Some(error) = result.error.as_deref() {
            self.record_transport_rejection("probe_error");
            self.transport_latency.observe_failure(error);
            self.log("DEBUG", &format!("transport latency probe failed: {error}"));
            let _ = self.refresh_status_from_last_sample();
            return;
        }
        if result.latency_ms.is_some()
            && !transport_result_matches_route(
                result.route_identity.as_deref(),
                self.route_identity.as_deref(),
            )
        {
            self.record_transport_rejection("route_changed");
            self.transport_latency
                .observe_failure("route changed before transport result was accepted");
            self.log(
                "DEBUG",
                "discarded transport probe from a stale or different uplink route",
            );
            let _ = self.refresh_status_from_last_sample();
            return;
        }
        if !result.trusted {
            self.record_transport_rejection("untrusted_backend");
            self.transport_latency
                .observe_failure("untrusted transport backend is diagnostic-only");
            self.log("DEBUG", "discarded untrusted legacy transport measurement");
            let _ = self.refresh_status_from_last_sample();
            return;
        }
        let Some(latency_ms) = result.latency_ms else {
            self.record_transport_rejection("empty_result");
            self.transport_latency
                .observe_failure("transport probe returned no result");
            let _ = self.refresh_status_from_last_sample();
            return;
        };
        if self
            .cpu_total_percent
            .map(|cpu| cpu > self.cfg.transport_cpu_max_percent)
            .unwrap_or(false)
        {
            self.record_transport_rejection("cpu_pressure");
            self.log(
                "DEBUG",
                "discarded transport probe while router CPU was above the configured limit",
            );
            let _ = self.refresh_status_from_last_sample();
            return;
        }
        let current_control_phase = self.last_status.as_ref().map(|status| {
            (
                status.dl_load_pct >= self.cfg.high_load_thr * 100.0,
                status.ul_load_pct >= self.cfg.high_load_thr * 100.0,
            )
        });
        let controller_phase_valid = result.control_valid
            && current_control_phase == Some((result.dl_loaded, result.ul_loaded));
        let rating_phase_valid = self.rating_load_snapshot.phase == result.rating_phase;
        if !rating_phase_valid {
            self.record_transport_rejection("rating_phase_changed");
            self.log(
                "DEBUG",
                "rating ignored a transport probe because its latched load phase changed",
            );
        }
        if !controller_phase_valid && !rating_phase_valid {
            let _ = self.refresh_status_from_last_sample();
            return;
        }

        let samples = if result.raw_samples_ms.is_empty() {
            vec![latency_ms]
        } else {
            result.raw_samples_ms.clone()
        };
        let grade_route = self.quality_grade_route_key();
        let mut confirmed_delta = None;
        let mut confirmed_dl_delta = None;
        let mut confirmed_ul_delta = None;
        let rating_flags = result.rating_phase.direction_flags();
        let controller_measurement_valid = if self.cfg.transport_controller_enabled {
            controller_phase_valid
        } else {
            rating_phase_valid
        };
        let tracker_flags = if self.cfg.transport_controller_enabled {
            (result.dl_loaded, result.ul_loaded)
        } else {
            rating_flags
        };
        let tracker_loaded = tracker_flags.0 || tracker_flags.1;
        for sample_ms in samples {
            if rating_phase_valid {
                self.quality_grade.observe(
                    &result.endpoint,
                    sample_ms,
                    rating_flags.0,
                    rating_flags.1,
                    epoch_secs(),
                    &grade_route,
                );
            }
            if !controller_measurement_valid {
                continue;
            }
            if let Some(delta_ms) = self.transport_latency.observe_success(
                &result.endpoint,
                sample_ms,
                tracker_loaded,
                now,
            ) {
                confirmed_delta = Some(delta_ms);
            }
            if !tracker_loaded {
                self.transport_latency_dl
                    .observe_success(&result.endpoint, sample_ms, false, now);
                self.transport_latency_ul
                    .observe_success(&result.endpoint, sample_ms, false, now);
            } else {
                if tracker_flags.0 {
                    if let Some(delta_ms) = self.transport_latency_dl.observe_success(
                        &result.endpoint,
                        sample_ms,
                        true,
                        now,
                    ) {
                        confirmed_dl_delta = Some(delta_ms);
                    }
                }
                if tracker_flags.1 {
                    if let Some(delta_ms) = self.transport_latency_ul.observe_success(
                        &result.endpoint,
                        sample_ms,
                        true,
                        now,
                    ) {
                        confirmed_ul_delta = Some(delta_ms);
                    }
                }
            }
        }

        let Some(delta_ms) = confirmed_delta else {
            let _ = self.refresh_status_from_last_sample();
            return;
        };
        let (icmp_dl_delta_us, icmp_ul_delta_us) = self
            .last_status
            .as_ref()
            .map(|status| (status.avg_dl_delta, status.avg_ul_delta))
            .unwrap_or((0.0, 0.0));
        let controller_enabled = self.cfg.transport_controller_enabled;
        let target_ms = self.cfg.quality_target_delay_ms;
        if let Some(dl_delta_ms) = confirmed_dl_delta {
            self.quality_dl_class = classify_quality(Some(effective_latency_delta_ms(
                icmp_dl_delta_us,
                0.0,
                Some(dl_delta_ms),
            )));
            self.transport_bad_windows_dl = if dl_delta_ms > target_ms {
                self.transport_bad_windows_dl.saturating_add(1)
            } else {
                0
            };
        }
        if let Some(ul_delta_ms) = confirmed_ul_delta {
            self.quality_ul_class = classify_quality(Some(effective_latency_delta_ms(
                0.0,
                icmp_ul_delta_us,
                Some(ul_delta_ms),
            )));
            self.transport_bad_windows_ul = if ul_delta_ms > target_ms {
                self.transport_bad_windows_ul.saturating_add(1)
            } else {
                0
            };
        }
        let dl_policy = self.quality_policy(true);
        let ul_policy = self.quality_policy(false);
        let dl_update = if let Some(dl_delta_ms) = confirmed_dl_delta {
            if controller_enabled
                && (dl_delta_ms <= target_ms || self.transport_bad_windows_dl >= 2)
            {
                Some(self.quality_search_dl.observe(
                    now,
                    self.shaper_dl,
                    dl_delta_ms,
                    true,
                    dl_policy,
                ))
            } else {
                None
            }
        } else {
            None
        };
        let ul_update = if let Some(ul_delta_ms) = confirmed_ul_delta {
            if controller_enabled
                && (ul_delta_ms <= target_ms || self.transport_bad_windows_ul >= 2)
            {
                Some(self.quality_search_ul.observe(
                    now,
                    self.shaper_ul,
                    ul_delta_ms,
                    true,
                    ul_policy,
                ))
            } else {
                None
            }
        } else {
            None
        };

        let mut changed = false;
        if let Some(update) = dl_update {
            if let Some(rate) = update.requested_rate_kbps {
                if self.cfg.adjust_dl_shaper_rate {
                    self.shaper_dl = rate;
                    changed = true;
                }
            }
            if update.requested_rate_kbps.is_some() || update.limited {
                self.log(
                    "INFO",
                    &format!(
                        "Transport DL quality {} at {:.1} ms: {} (floor {:.0} kbit/s)",
                        self.quality_dl_class.as_str(),
                        confirmed_dl_delta.unwrap_or(delta_ms),
                        update.reason,
                        self.throughput_floor_dl
                    ),
                );
            }
        }
        if let Some(update) = ul_update {
            if let Some(rate) = update.requested_rate_kbps {
                if self.cfg.adjust_ul_shaper_rate {
                    self.shaper_ul = rate;
                    changed = true;
                }
            }
            if update.requested_rate_kbps.is_some() || update.limited {
                self.log(
                    "INFO",
                    &format!(
                        "Transport UL quality {} at {:.1} ms: {} (floor {:.0} kbit/s)",
                        self.quality_ul_class.as_str(),
                        confirmed_ul_delta.unwrap_or(delta_ms),
                        update.reason,
                        self.throughput_floor_ul
                    ),
                );
            }
        }
        if changed {
            self.clamp_rates();
            self.apply_shaper("dl");
            self.apply_shaper("ul");
        }
        let _ = self.refresh_status_from_last_sample();
    }

    fn set_min_shaper_rates(&mut self, reason: &str) {
        self.log(
            "DEBUG",
            &format!("Enforcing minimum shaper rates: {reason}"),
        );
        self.shaper_dl = self.throughput_floor_dl;
        self.shaper_ul = self.throughput_floor_ul;
        self.apply_shaper("dl");
        self.apply_shaper("ul");
        let _ = self.refresh_status_from_last_sample();
    }

    fn set_run_state(&mut self, state: &str) {
        if self.run_state == state {
            return;
        }

        self.log(
            "DEBUG",
            &format!("Changing main state from: {} to: {state}", self.run_state),
        );
        self.run_state = state.to_string();

        if self.cfg.adaptive_ceiling_enabled && state != "RUNNING" {
            let now = Instant::now();
            let (dl_update, ul_update) = if state == "STALL" {
                (
                    self.adaptive_dl.reset_to_configured(now),
                    self.adaptive_ul.reset_to_configured(now),
                )
            } else {
                (
                    self.adaptive_dl.pause(now, "autorate state paused"),
                    self.adaptive_ul.pause(now, "autorate state paused"),
                )
            };
            self.apply_adaptive_updates(dl_update, ul_update);
        }

        let _ = self.refresh_status_from_last_sample();
    }

    fn set_uplink_route(
        &mut self,
        snapshot: Option<RouteSnapshot>,
        state: UplinkState,
        reason: &str,
        reset_learning: bool,
    ) {
        let previous_state = self.uplink_state;
        self.uplink_state = state;
        self.uplink_reason = reason.to_string();
        self.route_identity = snapshot.as_ref().map(RouteSnapshot::stable_key);
        self.route_snapshot = snapshot;

        let grade_route = self.quality_grade_route_key();
        self.quality_grade.set_route(&grade_route);

        if reset_learning {
            self.reset_uplink_learning("uplink route identity changed");
        }
        if previous_state != state {
            self.log(
                "INFO",
                &format!(
                    "Uplink state changed from {} to {}: {}",
                    previous_state.as_str(),
                    state.as_str(),
                    if reason.is_empty() {
                        "route ready"
                    } else {
                        reason
                    }
                ),
            );
        }
        let _ = self.refresh_status_from_last_sample();
    }

    fn reset_uplink_learning(&mut self, reason: &str) {
        self.dl_baseline_us.clear();
        self.ul_baseline_us.clear();
        self.dl_ewma_us.clear();
        self.ul_ewma_us.clear();
        self.dl_delays = filled_bool_window(self.cfg.bufferbloat_detection_window);
        self.ul_delays = filled_bool_window(self.cfg.bufferbloat_detection_window);
        self.dl_delta_us = filled_f64_window(self.cfg.bufferbloat_detection_window);
        self.ul_delta_us = filled_f64_window(self.cfg.bufferbloat_detection_window);
        self.transport_latency.reset();
        self.transport_latency_dl.reset();
        self.transport_latency_ul.reset();
        let grade_route = self.quality_grade_route_key();
        self.quality_grade.set_route(&grade_route);
        self.quality_search_dl.reset();
        self.quality_search_ul.reset();
        self.transport_bad_windows_dl = 0;
        self.transport_bad_windows_ul = 0;
        self.quality_dl_class = QualityClass::Learning;
        self.quality_ul_class = QualityClass::Learning;
        let now = Instant::now();
        self.rating_load = RatingLoadDetector::new(now);
        self.rating_load_snapshot = self.rating_load.snapshot(
            now,
            self.cfg.rating_load_config(),
            self.shaper_dl,
            self.shaper_ul,
        );
        let dl_update = self.adaptive_dl.reset_to_configured(now);
        let ul_update = self.adaptive_ul.reset_to_configured(now);
        self.log_adaptive_update("DL", dl_update);
        self.log_adaptive_update("UL", ul_update);
        self.log("INFO", &format!("Reset uplink latency learning: {reason}"));
    }

    fn set_route_external_ip(&mut self, value: String) {
        if self.route_external_ip == value {
            return;
        }
        let changed_existing = !self.route_external_ip.is_empty() && !value.is_empty();
        self.route_external_ip = value;
        if changed_existing {
            self.transport_latency.reset();
            self.transport_latency_dl.reset();
            self.transport_latency_ul.reset();
            self.quality_search_dl.reset();
            self.quality_search_ul.reset();
            self.transport_bad_windows_dl = 0;
            self.transport_bad_windows_ul = 0;
            self.quality_dl_class = QualityClass::Learning;
            self.quality_ul_class = QualityClass::Learning;
            self.log(
                "INFO",
                "Reset transport and displayed quality learning after external IP change",
            );
        }
        let grade_route = self.quality_grade_route_key();
        self.quality_grade.set_route(&grade_route);
        let _ = self.refresh_status_from_last_sample();
    }

    fn quality_grade_route_key(&self) -> String {
        format!(
            "{}|external={}",
            self.route_identity.as_deref().unwrap_or("unresolved"),
            self.route_external_ip
        )
    }

    fn note_probe_gap(&mut self) {
        if self.cfg.adaptive_ceiling_enabled {
            let now = Instant::now();
            let dl_update = self.adaptive_dl.abort_probe_gap(now);
            let ul_update = self.adaptive_ul.abort_probe_gap(now);
            self.apply_adaptive_updates(dl_update, ul_update);
        }
    }

    fn apply_adaptive_updates(
        &mut self,
        dl_update: AdaptiveCeilingUpdate,
        ul_update: AdaptiveCeilingUpdate,
    ) {
        let ceiling_changed = dl_update.change.is_some() || ul_update.change.is_some();
        self.log_adaptive_update("DL", dl_update);
        self.log_adaptive_update("UL", ul_update);

        if ceiling_changed {
            self.clamp_rates();
            self.apply_shaper("dl");
            self.apply_shaper("ul");
        }
    }

    fn log_adaptive_update(&mut self, direction: &str, update: AdaptiveCeilingUpdate) {
        let reason = update
            .transition
            .map(|transition| transition.reason)
            .unwrap_or("bounded probe update");
        if let Some(change) = update.change {
            self.log_adaptive_change(direction, change, reason);
        }

        let Some(transition) = update.transition else {
            return;
        };
        let controller = if direction == "DL" {
            &self.adaptive_dl
        } else {
            &self.adaptive_ul
        };
        let safe = controller.safe_ceiling_kbps();
        let failed = controller
            .failed_ceiling_kbps()
            .map(|value| format!("{value:.0}"))
            .unwrap_or_else(|| "-".to_string());
        let target = controller
            .probe_target_kbps()
            .map(|value| format!("{value:.0}"))
            .unwrap_or_else(|| "-".to_string());
        self.log(
            "INFO",
            &format!(
                "Adaptive {direction} phase {} -> {} ({reason}; safe {safe:.0}, failed {failed}, target {target} kbit/s)",
                transition.from.as_str(),
                transition.to.as_str(),
            ),
        );
    }

    fn log_adaptive_change(
        &mut self,
        direction: &str,
        change: AdaptiveCeilingChange,
        reason: &str,
    ) {
        let cap = if direction == "DL" {
            self.adaptive_dl.absolute_cap_kbps()
        } else {
            self.adaptive_ul.absolute_cap_kbps()
        };
        let (action, from_kbps, to_kbps) = match change {
            AdaptiveCeilingChange::Raised { from_kbps, to_kbps } => ("raised", from_kbps, to_kbps),
            AdaptiveCeilingChange::Lowered { from_kbps, to_kbps } => {
                ("lowered", from_kbps, to_kbps)
            }
        };

        self.log(
            "INFO",
            &format!(
                "Adaptive {direction} ceiling {action}: {from_kbps:.0} -> {to_kbps:.0} kbit/s ({reason}; absolute cap {cap:.0} kbit/s)"
            ),
        );
    }

    fn on_sample(
        &mut self,
        sample: Sample,
        active_reflectors: &[String],
        health: &ReflectorHealth,
    ) -> RateSample {
        let now = Instant::now();
        let rate_sample = self.rate_monitor.sample();
        let dl_rate = rate_sample.dl_kbps;
        let ul_rate = rate_sample.ul_kbps;
        let dl_load_pct = percent(dl_rate, self.shaper_dl);
        let ul_load_pct = percent(ul_rate, self.shaper_ul);

        let dl_baseline = self
            .dl_baseline_us
            .entry(sample.reflector.clone())
            .or_insert(100_000.0);
        let ul_baseline = self
            .ul_baseline_us
            .entry(sample.reflector.clone())
            .or_insert(100_000.0);
        let mut dl_delta_us = sample.dl_owd_us - *dl_baseline;
        let mut ul_delta_us = sample.ul_owd_us - *ul_baseline;

        if sample.timestamped_owd && (dl_delta_us.abs() + ul_delta_us.abs()) >= 3_000_000_000.0 {
            *dl_baseline = sample.dl_owd_us;
            *ul_baseline = sample.ul_owd_us;
            dl_delta_us = 0.0;
            ul_delta_us = 0.0;
        } else {
            let dl_alpha = if sample.dl_owd_us >= *dl_baseline {
                self.cfg.alpha_baseline_increase
            } else {
                self.cfg.alpha_baseline_decrease
            };
            let ul_alpha = if sample.ul_owd_us >= *ul_baseline {
                self.cfg.alpha_baseline_increase
            } else {
                self.cfg.alpha_baseline_decrease
            };

            *dl_baseline = dl_alpha * sample.dl_owd_us + (1.0 - dl_alpha) * *dl_baseline;
            *ul_baseline = ul_alpha * sample.ul_owd_us + (1.0 - ul_alpha) * *ul_baseline;
            dl_delta_us = sample.dl_owd_us - *dl_baseline;
            ul_delta_us = sample.ul_owd_us - *ul_baseline;
        }

        if dl_load_pct < self.cfg.high_load_thr * 100.0
            && ul_load_pct < self.cfg.high_load_thr * 100.0
        {
            let dl_ewma = self
                .dl_ewma_us
                .entry(sample.reflector.clone())
                .or_insert(0.0);
            *dl_ewma = self.cfg.alpha_delta_ewma * dl_delta_us
                + (1.0 - self.cfg.alpha_delta_ewma) * *dl_ewma;
            let ul_ewma = self
                .ul_ewma_us
                .entry(sample.reflector.clone())
                .or_insert(0.0);
            *ul_ewma = self.cfg.alpha_delta_ewma * ul_delta_us
                + (1.0 - self.cfg.alpha_delta_ewma) * *ul_ewma;
        }

        let dl_baseline_current_us = *dl_baseline;
        let ul_baseline_current_us = *ul_baseline;
        let dl_delay_thr_us = self.delay_thr_us(true);
        let ul_delay_thr_us = self.delay_thr_us(false);
        let dl_up_thr_us = self.avg_adjust_up_thr_us(true);
        let ul_up_thr_us = self.avg_adjust_up_thr_us(false);
        let dl_down_thr_us = self.avg_adjust_down_thr_us(true);
        let ul_down_thr_us = self.avg_adjust_down_thr_us(false);

        push_window(&mut self.dl_delays, dl_delta_us > dl_delay_thr_us);
        push_window(&mut self.ul_delays, ul_delta_us > ul_delay_thr_us);
        push_window(&mut self.dl_delta_us, dl_delta_us);
        push_window(&mut self.ul_delta_us, ul_delta_us);

        let dl_delay_count = self.dl_delays.iter().filter(|v| **v).count();
        let ul_delay_count = self.ul_delays.iter().filter(|v| **v).count();
        let dl_bb = dl_delay_count >= self.cfg.bufferbloat_detection_thr;
        let ul_bb = ul_delay_count >= self.cfg.bufferbloat_detection_thr;
        let avg_dl_delta = average(&self.dl_delta_us);
        let avg_ul_delta = average(&self.ul_delta_us);
        let high_load_pct = self.cfg.high_load_thr * 100.0;
        let dl_kind = classify_load(
            dl_load_pct,
            dl_rate,
            self.cfg.connection_active_thr_kbps,
            high_load_pct,
        );
        let ul_kind = classify_load(
            ul_load_pct,
            ul_rate,
            self.cfg.connection_active_thr_kbps,
            high_load_pct,
        );

        let transport_clean = self.transport_clean_for_growth(now);
        let transport_delta_ms = self
            .transport_latency
            .snapshot(now, self.cfg.transport_latency_enabled)
            .delta_ms;
        let transport_bloat = self.cfg.transport_latency_enabled
            && transport_delta_ms
                .map(|delta| delta > self.cfg.quality_target_delay_ms)
                .unwrap_or(false);
        self.update_direction(true, dl_kind, dl_bb, avg_dl_delta, transport_clean, now);
        self.update_direction(false, ul_kind, ul_bb, avg_ul_delta, transport_clean, now);
        self.update_adaptive_ceilings(
            dl_kind,
            ul_kind,
            dl_bb || (matches!(dl_kind, LoadKind::High) && transport_bloat),
            ul_bb || (matches!(ul_kind, LoadKind::High) && transport_bloat),
            dl_delay_count,
            ul_delay_count,
            avg_dl_delta,
            avg_ul_delta,
            transport_clean,
            now,
        );
        self.clamp_rates();
        self.apply_shaper("dl");
        self.apply_shaper("ul");

        if self.cfg.output_processing_stats {
            let dl_ewma_us = self
                .dl_ewma_us
                .get(&sample.reflector)
                .copied()
                .unwrap_or(0.0);
            let ul_ewma_us = self
                .ul_ewma_us
                .get(&sample.reflector)
                .copied()
                .unwrap_or(0.0);
            self.log(
                "DATA",
                &format!(
                    "{:.0}; {:.0}; {:.1}; {:.1}; {:.6}; {}; {}; {:.0}; {:.0}; {:.0}; {:.0}; {:.0}; {:.0}; {:.0}; {:.0}; {:.0}; {:.0}; {}; {:.0}; {:.0}; {:.0}; {}; {:.0}; {:.0}; {:.0}; {}; {}; {:.0}; {:.0}",
                    dl_rate,
                    ul_rate,
                    dl_load_pct,
                    ul_load_pct,
                    sample.timestamp,
                    sample.reflector,
                    sample.seq,
                    dl_baseline_current_us,
                    sample.dl_owd_us,
                    dl_ewma_us,
                    dl_delta_us,
                    dl_delay_thr_us,
                    ul_baseline_current_us,
                    sample.ul_owd_us,
                    ul_ewma_us,
                    ul_delta_us,
                    ul_delay_thr_us,
                    dl_delay_count,
                    avg_dl_delta,
                    dl_up_thr_us,
                    dl_down_thr_us,
                    ul_delay_count,
                    avg_ul_delta,
                    ul_up_thr_us,
                    ul_down_thr_us,
                    load_label(dl_kind, dl_bb, "dl"),
                    load_label(ul_kind, ul_bb, "ul"),
                    self.shaper_dl,
                    self.shaper_ul
                ),
            );
        }

        if self.cfg.output_load_stats {
            self.log(
                "LOAD",
                &format!(
                    "{:.6}; {:.0}; {:.0}; {:.0}; {:.0}",
                    epoch_secs(),
                    dl_rate,
                    ul_rate,
                    self.shaper_dl,
                    self.shaper_ul
                ),
            );
        }

        if self.cfg.output_summary_stats {
            self.log(
                "SUMMARY",
                &format!(
                    "{:.0}; {:.0}; {}; {}; {:.0}; {:.0}; {}; {}; {:.0}; {:.0}",
                    dl_rate,
                    ul_rate,
                    dl_delay_count,
                    ul_delay_count,
                    avg_dl_delta,
                    avg_ul_delta,
                    load_label(dl_kind, dl_bb, "dl"),
                    load_label(ul_kind, ul_bb, "ul"),
                    self.shaper_dl,
                    self.shaper_ul
                ),
            );
        }

        self.maybe_sample_cpu();

        let _ = self.write_status(
            dl_rate,
            ul_rate,
            dl_load_pct,
            ul_load_pct,
            dl_delay_count,
            ul_delay_count,
            avg_dl_delta,
            avg_ul_delta,
            &sample,
            active_reflectors,
            Some(health),
        );
        rate_sample
    }

    fn maybe_sample_cpu(&mut self) -> bool {
        let interval = Duration::from_millis(self.cfg.monitor_cpu_usage_interval_ms.max(1));
        if self.last_cpu_sample.elapsed() < interval {
            return false;
        }

        self.last_cpu_sample = Instant::now();

        let sample = match self.cpu_monitor.as_mut() {
            Some(monitor) => match monitor.sample() {
                Ok(sample) => sample,
                Err(e) => {
                    self.log("ERROR", &format!("failed to sample CPU usage: {e}"));
                    None
                }
            },
            None => None,
        };

        let Some(stats) = sample else {
            return false;
        };

        self.cpu_total_percent = Some(stats.total_percent);
        self.cpu_core_percentages = stats.core_percentages.clone();

        if self.cfg.output_cpu_raw_stats {
            for raw_line in stats.raw_lines {
                self.log("CPU_RAW", &raw_line);
            }
        }

        if self.cfg.output_cpu_stats {
            let mut values = vec![format!("{:.1}", stats.total_percent)];
            values.extend(
                stats
                    .core_percentages
                    .iter()
                    .map(|percent| format!("{percent:.1}")),
            );
            self.log("CPU", &values.join("; "));
        }

        true
    }

    fn maybe_record_graph_history(&mut self, dl_rate_kbps: f64, ul_rate_kbps: f64) {
        if !self.cfg.graph_history_enabled {
            return;
        }

        if self.last_history_budget_refresh.elapsed()
            >= Duration::from_secs(GRAPH_HISTORY_BUDGET_REFRESH_S)
        {
            self.history_budget = history_budget_snapshot(&self.cfg);
            self.last_history_budget_refresh = Instant::now();
            let instance_cap = self.history_budget.instance_budget_kib.saturating_mul(1024);
            let current_size = fs::metadata(self.cfg.graph_history_path())
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            if instance_cap == 0 {
                let _ = fs::remove_file(self.cfg.graph_history_path());
                self.history_sample_count = 0;
                self.history_budget.used_instance_kib = 0;
            } else if current_size > instance_cap {
                let target = instance_cap.saturating_mul(3) / 4;
                match compact_graph_history_file(&self.cfg.graph_history_path(), target) {
                    Ok(samples) => self.history_sample_count = samples,
                    Err(error) => self.log(
                        "ERROR",
                        &format!("failed to enforce reduced graph history budget: {error}"),
                    ),
                }
            }
        }
        if self.history_budget.paused_low_memory
            || self.history_budget.instance_budget_kib == 0
            || self.last_graph_history_sample.elapsed()
                < Duration::from_secs(self.cfg.graph_history_interval_s)
        {
            return;
        }

        self.last_graph_history_sample = Instant::now();
        let now = epoch_secs();
        let rtt_ms = self.last_status.as_ref().and_then(|snapshot| {
            let age_s = now - snapshot.sample.timestamp;
            if self.run_state == "RUNNING" && (-1.0..=5.0).contains(&age_s) {
                Some(snapshot.sample.rtt_ms)
            } else {
                None
            }
        });
        let transport = self
            .transport_latency
            .snapshot(Instant::now(), self.cfg.transport_latency_enabled);
        let effective_delta_ms = self.last_status.as_ref().map(|snapshot| {
            effective_latency_delta_ms(
                snapshot.avg_dl_delta,
                snapshot.avg_ul_delta,
                transport.delta_ms,
            )
        });
        let grade_snapshot = self.quality_grade.snapshot(epoch_secs());
        let grade_result = grade_snapshot.last_known.as_ref();
        let line = graph_history_line(
            now,
            rtt_ms,
            self.cpu_total_percent,
            dl_rate_kbps,
            ul_rate_kbps,
            transport.delta_ms,
            effective_delta_ms,
            Some(self.throughput_floor_dl),
            Some(self.throughput_floor_ul),
            self.uplink_state.as_str(),
            self.route_identity.as_deref().unwrap_or(""),
            grade_result.map(|result| result.class.as_str()),
            grade_snapshot.state,
            grade_result.map(|result| result.increase_ms),
            self.rating_load_snapshot.phase.as_str(),
            grade_snapshot.dl_samples,
            grade_snapshot.ul_samples,
        );
        let path = self.cfg.graph_history_path();
        let instance_cap = self.history_budget.instance_budget_kib.saturating_mul(1024);

        if fs::metadata(&path)
            .map(|metadata| metadata.len().saturating_add(line.len() as u64))
            .unwrap_or(0)
            > instance_cap
        {
            let target = instance_cap.saturating_mul(3) / 4;
            match compact_graph_history_file(&path, target) {
                Ok(samples) => self.history_sample_count = samples,
                Err(e) => {
                    self.log("ERROR", &format!("failed to compact graph history: {e}"));
                    return;
                }
            }
        }

        let result = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut file| file.write_all(line.as_bytes()));
        if let Err(e) = result {
            self.log("ERROR", &format!("failed to append graph history: {e}"));
        } else {
            self.history_sample_count = self.history_sample_count.saturating_add(1);
            self.history_budget.used_instance_kib = fs::metadata(&path)
                .map(|metadata| metadata.len().div_ceil(1024))
                .unwrap_or(0);
            self.history_budget.used_total_kib =
                history_usage_bytes(Path::new("/var/run/cake-autorate")).div_ceil(1024);
        }
    }

    fn direction_compensation_us(&self, is_dl: bool) -> f64 {
        if is_dl {
            packet_compensation_us(self.cfg.dl_max_wire_packet_size_bits, self.shaper_dl)
        } else {
            packet_compensation_us(self.cfg.ul_max_wire_packet_size_bits, self.shaper_ul)
        }
    }

    fn delay_thr_us(&self, is_dl: bool) -> f64 {
        let base = if is_dl {
            self.cfg.dl_owd_delta_delay_thr_ms
        } else {
            self.cfg.ul_owd_delta_delay_thr_ms
        };
        base * 1000.0 + self.direction_compensation_us(is_dl)
    }

    fn avg_adjust_up_thr_us(&self, is_dl: bool) -> f64 {
        let base = if is_dl {
            self.cfg.dl_avg_owd_delta_max_adjust_up_thr_ms
        } else {
            self.cfg.ul_avg_owd_delta_max_adjust_up_thr_ms
        };
        base * 1000.0 + self.direction_compensation_us(is_dl)
    }

    fn avg_adjust_down_thr_us(&self, is_dl: bool) -> f64 {
        let base = if is_dl {
            self.cfg.dl_avg_owd_delta_max_adjust_down_thr_ms
        } else {
            self.cfg.ul_avg_owd_delta_max_adjust_down_thr_ms
        };
        base * 1000.0 + self.direction_compensation_us(is_dl)
    }

    fn update_direction(
        &mut self,
        is_dl: bool,
        kind: LoadKind,
        bufferbloat: bool,
        avg_delta_us: f64,
        allow_growth: bool,
        now: Instant,
    ) {
        let mut shaper = if is_dl {
            self.shaper_dl
        } else {
            self.shaper_ul
        };
        let base = if is_dl {
            self.cfg.base_dl_shaper_rate_kbps
        } else {
            self.cfg.base_ul_shaper_rate_kbps
        };
        let delay_thr_us = self.delay_thr_us(is_dl);
        let up_thr_us = self.avg_adjust_up_thr_us(is_dl);
        let down_thr_us = self.avg_adjust_down_thr_us(is_dl);
        let mut last_bb = if is_dl {
            self.last_bb_dl
        } else {
            self.last_bb_ul
        };
        let mut last_decay = if is_dl {
            self.last_decay_dl
        } else {
            self.last_decay_ul
        };
        let bb_ready = now.duration_since(last_bb)
            >= Duration::from_millis(self.cfg.bufferbloat_refractory_period_ms);
        let decay_ready = now.duration_since(last_decay)
            >= Duration::from_millis(self.cfg.decay_refractory_period_ms);

        if bufferbloat && bb_ready {
            let factor = if down_thr_us <= delay_thr_us {
                1.0
            } else if avg_delta_us > delay_thr_us {
                ((avg_delta_us - delay_thr_us) / (down_thr_us - delay_thr_us)).min(1.0)
            } else {
                0.0
            };
            let adjust = self.cfg.shaper_rate_min_adjust_down_bufferbloat
                - factor
                    * (self.cfg.shaper_rate_min_adjust_down_bufferbloat
                        - self.cfg.shaper_rate_max_adjust_down_bufferbloat);
            shaper *= adjust;
            last_bb = now;
            last_decay = now;
        } else if matches!(kind, LoadKind::High) && bb_ready && allow_growth {
            let factor = if delay_thr_us <= up_thr_us {
                1.0
            } else if delay_thr_us > avg_delta_us {
                ((delay_thr_us - avg_delta_us) / (delay_thr_us - up_thr_us)).min(1.0)
            } else {
                0.0
            };
            let adjust = self.cfg.shaper_rate_min_adjust_up_load_high
                - factor
                    * (self.cfg.shaper_rate_min_adjust_up_load_high
                        - self.cfg.shaper_rate_max_adjust_up_load_high);
            shaper *= adjust;
            last_decay = now;
        } else if matches!(kind, LoadKind::Low | LoadKind::Idle) && decay_ready {
            if shaper > base {
                shaper = (shaper * self.cfg.shaper_rate_adjust_down_load_low).max(base);
            } else if shaper < base {
                shaper = (shaper * self.cfg.shaper_rate_adjust_up_load_low).min(base);
            }
            last_decay = now;
        }

        if is_dl {
            self.shaper_dl = shaper;
            self.last_bb_dl = last_bb;
            self.last_decay_dl = last_decay;
        } else {
            self.shaper_ul = shaper;
            self.last_bb_ul = last_bb;
            self.last_decay_ul = last_decay;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn update_adaptive_ceilings(
        &mut self,
        dl_kind: LoadKind,
        ul_kind: LoadKind,
        dl_bufferbloat: bool,
        ul_bufferbloat: bool,
        dl_delay_count: usize,
        ul_delay_count: usize,
        avg_dl_delta_us: f64,
        avg_ul_delta_us: f64,
        transport_clean: bool,
        now: Instant,
    ) {
        if !self.cfg.adaptive_ceiling_enabled
            || matches!(
                self.uplink_state,
                UplinkState::Offline | UplinkState::Learning
            )
        {
            return;
        }

        let policy = AdaptiveCeilingPolicy {
            hold_time: Duration::from_secs_f64(self.cfg.adaptive_ceiling_hold_time_s),
            probe_step_percent: self.cfg.adaptive_ceiling_growth_percent,
            probe_duration: Duration::from_secs_f64(self.cfg.adaptive_ceiling_probe_duration_s),
            cooldown: Duration::from_secs_f64(self.cfg.adaptive_ceiling_cooldown_s),
            failed_bound_ttl: Duration::from_secs_f64(self.cfg.adaptive_ceiling_failed_bound_ttl_s),
            eligibility_grace: Duration::from_secs_f64(
                self.cfg.reflector_response_deadline_s.max(1.0),
            ),
        };
        let dl_eligible = self.cfg.adjust_dl_shaper_rate
            && matches!(dl_kind, LoadKind::High)
            && !dl_bufferbloat
            && dl_delay_count < self.cfg.bufferbloat_detection_thr
            && avg_dl_delta_us <= self.avg_adjust_up_thr_us(true)
            && transport_clean;
        let ul_eligible = self.cfg.adjust_ul_shaper_rate
            && matches!(ul_kind, LoadKind::High)
            && !ul_bufferbloat
            && ul_delay_count < self.cfg.bufferbloat_detection_thr
            && avg_ul_delta_us <= self.avg_adjust_up_thr_us(false)
            && transport_clean;

        let dl_update = self.adaptive_dl.observe(
            now,
            AdaptiveCeilingObservation {
                eligible: dl_eligible,
                bufferbloat: dl_bufferbloat,
                shaper_rate_kbps: self.shaper_dl,
            },
            policy,
        );
        let ul_update = self.adaptive_ul.observe(
            now,
            AdaptiveCeilingObservation {
                eligible: ul_eligible,
                bufferbloat: ul_bufferbloat,
                shaper_rate_kbps: self.shaper_ul,
            },
            policy,
        );
        self.log_adaptive_update("DL", dl_update);
        self.log_adaptive_update("UL", ul_update);
    }

    fn clamp_rates(&mut self) {
        self.shaper_dl = self
            .shaper_dl
            .max(self.throughput_floor_dl)
            .min(self.adaptive_dl.effective_max_kbps());
        self.shaper_ul = self
            .shaper_ul
            .max(self.throughput_floor_ul)
            .min(self.adaptive_ul.effective_max_kbps());
    }

    fn apply_shaper(&mut self, direction: &str) {
        let is_dl = direction == "dl";
        let (interface, adjust, rate, last, last_attempt_elapsed) = if is_dl {
            (
                self.cfg.dl_if.clone(),
                self.cfg.adjust_dl_shaper_rate,
                self.shaper_dl,
                self.last_set_dl,
                self.last_shaper_attempt_dl.elapsed(),
            )
        } else {
            (
                self.cfg.ul_if.clone(),
                self.cfg.adjust_ul_shaper_rate,
                self.shaper_ul,
                self.last_set_ul,
                self.last_shaper_attempt_ul.elapsed(),
            )
        };
        let rounded = rate.round().max(1.0) as u64;
        if !shaper_update_due(last, rounded, last_attempt_elapsed) {
            return;
        }
        if is_dl {
            self.last_shaper_attempt_dl = Instant::now();
        } else {
            self.last_shaper_attempt_ul = Instant::now();
        }

        if self.cfg.output_cake_changes {
            self.log(
                "SHAPER",
                &format!("tc qdisc change root dev {interface} cake bandwidth {rounded}Kbit"),
            );
        }

        if !adjust {
            if is_dl {
                self.last_set_dl = rounded;
            } else {
                self.last_set_ul = rounded;
            }
            return;
        }

        let status = Command::new("tc")
            .arg("qdisc")
            .arg("change")
            .arg("root")
            .arg("dev")
            .arg(&interface)
            .arg("cake")
            .arg("bandwidth")
            .arg(format!("{rounded}Kbit"))
            .status();

        match status {
            Ok(s) if s.success() => {
                if is_dl {
                    self.last_set_dl = rounded;
                } else {
                    self.last_set_ul = rounded;
                }
            }
            Ok(s) => self.log(
                "ERROR",
                &format!("tc failed for {interface} with status {s}"),
            ),
            Err(e) => self.log(
                "ERROR",
                &format!("failed to execute tc for {interface}: {e}"),
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn write_status(
        &mut self,
        dl_rate: f64,
        ul_rate: f64,
        dl_load_pct: f64,
        ul_load_pct: f64,
        dl_delay_count: usize,
        ul_delay_count: usize,
        avg_dl_delta: f64,
        avg_ul_delta: f64,
        sample: &Sample,
        active_reflectors: &[String],
        health: Option<&ReflectorHealth>,
    ) -> io::Result<()> {
        let publish_due = status_publish_due(self.last_status_publish.elapsed());
        if let Some(snapshot) = self.last_status.as_mut() {
            snapshot.dl_rate = dl_rate;
            snapshot.ul_rate = ul_rate;
            snapshot.dl_load_pct = dl_load_pct;
            snapshot.ul_load_pct = ul_load_pct;
            snapshot.dl_delay_count = dl_delay_count;
            snapshot.ul_delay_count = ul_delay_count;
            snapshot.avg_dl_delta = avg_dl_delta;
            snapshot.avg_ul_delta = avg_ul_delta;
            snapshot.sample = sample.clone();
            if publish_due {
                snapshot.active_reflectors = active_reflectors.to_vec();
                snapshot.health = health.cloned();
            }
        } else {
            self.last_status = Some(StatusSnapshot {
                dl_rate,
                ul_rate,
                dl_load_pct,
                ul_load_pct,
                dl_delay_count,
                ul_delay_count,
                avg_dl_delta,
                avg_ul_delta,
                sample: sample.clone(),
                active_reflectors: active_reflectors.to_vec(),
                health: health.cloned(),
            });
        }

        if !publish_due {
            return Ok(());
        }

        self.write_status_file(
            dl_rate,
            ul_rate,
            dl_load_pct,
            ul_load_pct,
            dl_delay_count,
            ul_delay_count,
            avg_dl_delta,
            avg_ul_delta,
            sample,
            active_reflectors,
            health,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn write_status_file(
        &mut self,
        dl_rate: f64,
        ul_rate: f64,
        dl_load_pct: f64,
        ul_load_pct: f64,
        dl_delay_count: usize,
        ul_delay_count: usize,
        avg_dl_delta: f64,
        avg_ul_delta: f64,
        sample: &Sample,
        active_reflectors: &[String],
        health: Option<&ReflectorHealth>,
    ) -> io::Result<()> {
        let path = self.cfg.run_dir().join("status.json");
        let tmp = self.cfg.run_dir().join("status.json.tmp");
        let spare_reflectors = reflector_spare_reflectors(&self.cfg, active_reflectors);
        let bad_reflectors = reflector_bad_reflectors(&self.cfg, health);
        let reflector_health = reflector_health_json(&self.cfg, active_reflectors, health);
        let adaptive_now = Instant::now();
        let dl_phase_elapsed_s = adaptive_now
            .saturating_duration_since(self.adaptive_dl.phase_since())
            .as_secs_f64();
        let ul_phase_elapsed_s = adaptive_now
            .saturating_duration_since(self.adaptive_ul.phase_since())
            .as_secs_f64();
        let transport = self
            .transport_latency
            .snapshot(adaptive_now, self.cfg.transport_latency_enabled);
        let effective_delta_ms =
            effective_latency_delta_ms(avg_dl_delta, avg_ul_delta, transport.delta_ms);
        let quality_class = if transport.confirmed {
            classify_quality(Some(effective_delta_ms))
        } else {
            QualityClass::Learning
        };
        let quality_reason = if !self.cfg.transport_controller_enabled {
            "detected_only_controller_disabled"
        } else if self.quality_search_dl.limited() {
            self.quality_search_dl.last_reason()
        } else if self.quality_search_ul.limited() {
            self.quality_search_ul.last_reason()
        } else if transport.confirmed {
            "estimated_from_icmp_and_transport_latency"
        } else {
            transport.status
        };
        let quality_grade = self.quality_grade.snapshot(epoch_secs());
        let mut file = File::create(&tmp)?;
        writeln!(
            file,
            "{{\"instance\":\"{}\",\"version\":\"{}\",\"state\":\"{}\",\"sqm_runtime_managed\":{},\"sqm_runtime_state\":\"{}\",\"sqm_runtime_healthy\":{},\"sqm_runtime_reason\":\"{}\",\"sqm_recovery_attempts\":{},\"sqm_last_recovery_at\":{},\"started_at\":{:.6},\"updated_at\":{:.6},\"dl_if\":\"{}\",\"ul_if\":\"{}\",\"reflector\":\"{}\",\"seq\":\"{}\",\"probe_timestamp\":{:.6},\"rtt_ms\":{:.3},\"dl_owd_us\":{:.1},\"ul_owd_us\":{:.1},\"dl_achieved_rate_kbps\":{:.1},\"ul_achieved_rate_kbps\":{:.1},\"dl_load_percent\":{:.1},\"ul_load_percent\":{:.1},\"dl_sum_delays\":{},\"ul_sum_delays\":{},\"dl_avg_owd_delta_us\":{:.1},\"ul_avg_owd_delta_us\":{:.1},\"cake_dl_rate_kbps\":{:.0},\"cake_ul_rate_kbps\":{:.0},\"adaptive_ceiling_enabled\":{},\"configured_max_dl_shaper_rate_kbps\":{:.0},\"configured_max_ul_shaper_rate_kbps\":{:.0},\"effective_max_dl_shaper_rate_kbps\":{:.0},\"effective_max_ul_shaper_rate_kbps\":{:.0},\"adaptive_ceiling_dl_cap_kbps\":{:.0},\"adaptive_ceiling_ul_cap_kbps\":{:.0},\"adaptive_ceiling_dl_phase\":\"{}\",\"adaptive_ceiling_ul_phase\":\"{}\",\"adaptive_ceiling_safe_dl_kbps\":{:.0},\"adaptive_ceiling_safe_ul_kbps\":{:.0},\"adaptive_ceiling_failed_dl_kbps\":{},\"adaptive_ceiling_failed_ul_kbps\":{},\"adaptive_ceiling_probe_dl_kbps\":{},\"adaptive_ceiling_probe_ul_kbps\":{},\"adaptive_ceiling_dl_phase_elapsed_s\":{:.3},\"adaptive_ceiling_ul_phase_elapsed_s\":{:.3},\"adaptive_ceiling_dl_last_reason\":\"{}\",\"adaptive_ceiling_ul_last_reason\":\"{}\",\"cpu_total_percent\":{},\"cpu_core_percentages\":{},\"active_reflectors\":{},\"spare_reflectors\":{},\"bad_reflectors\":{},\"reflector_health\":{}}}",
            json_escape(&self.cfg.instance),
            env!("CARGO_PKG_VERSION"),
            json_escape(&self.run_state),
            self.cfg.manage_sqm && self.cfg.sqm_enabled,
            json_escape(&self.sqm_runtime_state),
            self.sqm_runtime_healthy,
            json_escape(&self.sqm_runtime_reason),
            self.sqm_recovery_attempts,
            json_f64_or_null(self.sqm_last_recovery_at, 3),
            self.started_at,
            epoch_secs(),
            json_escape(&self.cfg.dl_if),
            json_escape(&self.cfg.ul_if),
            json_escape(&sample.reflector),
            json_escape(&sample.seq),
            sample.timestamp,
            sample.rtt_ms,
            sample.dl_owd_us,
            sample.ul_owd_us,
            dl_rate,
            ul_rate,
            dl_load_pct,
            ul_load_pct,
            dl_delay_count,
            ul_delay_count,
            avg_dl_delta,
            avg_ul_delta,
            self.shaper_dl,
            self.shaper_ul,
            self.cfg.adaptive_ceiling_enabled,
            self.adaptive_dl.configured_max_kbps(),
            self.adaptive_ul.configured_max_kbps(),
            self.adaptive_dl.effective_max_kbps(),
            self.adaptive_ul.effective_max_kbps(),
            self.adaptive_dl.absolute_cap_kbps(),
            self.adaptive_ul.absolute_cap_kbps(),
            self.adaptive_dl.phase().as_str(),
            self.adaptive_ul.phase().as_str(),
            self.adaptive_dl.safe_ceiling_kbps(),
            self.adaptive_ul.safe_ceiling_kbps(),
            json_f64_or_null(self.adaptive_dl.failed_ceiling_kbps(), 0),
            json_f64_or_null(self.adaptive_ul.failed_ceiling_kbps(), 0),
            json_f64_or_null(self.adaptive_dl.probe_target_kbps(), 0),
            json_f64_or_null(self.adaptive_ul.probe_target_kbps(), 0),
            dl_phase_elapsed_s,
            ul_phase_elapsed_s,
            json_escape(self.adaptive_dl.last_transition_reason()),
            json_escape(self.adaptive_ul.last_transition_reason()),
            json_f64_or_null(self.cpu_total_percent, 1),
            json_f64_array(&self.cpu_core_percentages, 1),
            json_string_array(active_reflectors),
            json_string_array(&spare_reflectors),
            json_string_array(&bad_reflectors),
            reflector_health
        )?;
        file.seek(SeekFrom::End(-2))?;
        writeln!(
            file,
            ",\"transport_latency_enabled\":{},\"transport_controller_enabled\":{},\"transport_probe_method\":\"network_rtt_v3\",\"transport_probe_backend\":\"{}\",\"transport_probe_trusted\":{},\"transport_probe_raw_samples\":{},\"transport_probe_discarded_samples\":{},\"transport_probe_server_processing_ms\":{:.3},\"transport_probe_connection_reused\":{},\"transport_probe_rejected_reason\":{},\"transport_probe_last_rejected_reason\":{},\"transport_probe_last_rejected_at\":{},\"transport_status\":\"{}\",\"transport_endpoint\":{},\"transport_latency_ms\":{},\"transport_baseline_ms\":{},\"transport_delta_ms\":{},\"transport_sample_age_s\":{},\"transport_confidence\":{},\"transport_successful_samples\":{},\"transport_failed_samples\":{},\"transport_last_error\":{},\"effective_latency_delta_ms\":{:.3},\"quality_estimated\":true,\"quality_class\":\"{}\",\"quality_dl_class\":\"{}\",\"quality_ul_class\":\"{}\",\"quality_confidence\":{},\"quality_reason\":\"{}\",\"throughput_guard_enabled\":{},\"throughput_floor_dl_kbps\":{:.0},\"throughput_floor_ul_kbps\":{:.0},\"quality_limited\":{},\"quality_limited_dl\":{},\"quality_limited_ul\":{}}}",
            self.cfg.transport_latency_enabled,
            self.cfg.transport_controller_enabled,
            json_escape(&self.transport_backend),
            self.transport_trusted,
            self.transport_raw_samples,
            self.transport_discarded_samples,
            self.transport_server_processing_ms,
            self.transport_connection_reused,
            json_string_or_null(self.transport_rejected_reason.as_deref()),
            json_string_or_null(self.transport_last_rejected_reason.as_deref()),
            json_f64_or_null(self.transport_last_rejected_at, 3),
            json_escape(transport.status),
            json_string_or_null(transport.endpoint.as_deref()),
            json_f64_or_null(transport.latency_ms, 3),
            json_f64_or_null(transport.baseline_ms, 3),
            json_f64_or_null(transport.delta_ms, 3),
            json_f64_or_null(transport.sample_age_s, 3),
            transport.confidence,
            transport.successful_samples,
            transport.failed_samples,
            json_string_or_null(transport.last_error.as_deref()),
            effective_delta_ms,
            quality_class.as_str(),
            self.quality_dl_class.as_str(),
            self.quality_ul_class.as_str(),
            transport.confidence,
            json_escape(quality_reason),
            self.cfg.transport_controller_enabled && self.cfg.throughput_guard_enabled,
            self.throughput_floor_dl,
            self.throughput_floor_ul,
            self.quality_search_dl.limited() || self.quality_search_ul.limited(),
            self.quality_search_dl.limited(),
            self.quality_search_ul.limited(),
        )?;
        file.seek(SeekFrom::End(-2))?;
        write!(
            file,
            ",\"quality_grade_method\":\"transport_rtt_p90_loaded_minus_p5_idle_v4\",\"quality_grade_state\":\"{}\",\"quality_grade_collected_samples\":{},\"quality_grade_required_samples\":{},\"quality_grade_baseline_ready\":{},\"quality_grade_baseline_samples\":{},\"quality_grade_baseline_required_samples\":{},\"quality_grade_dl_samples\":{},\"quality_grade_ul_samples\":{},\"quality_grade_bidirectional_samples\":{},\"quality_grade_finalize_remaining_s\":{},\"quality_grade_current\":{},\"quality_grade_last_known\":{},\"rating_load_phase\":\"{}\",\"rating_load_candidate\":\"{}\",\"rating_load_raw_dl_percent\":{:.3},\"rating_load_raw_ul_percent\":{:.3},\"rating_load_smoothed_dl_percent\":{:.3},\"rating_load_smoothed_ul_percent\":{:.3},\"rating_load_aggregate_dl_kbps\":{:.3},\"rating_load_aggregate_ul_kbps\":{:.3},\"rating_load_effective_dl_kbps\":{:.3},\"rating_load_effective_ul_kbps\":{:.3},\"rating_load_reference_dl_kbps\":{:.3},\"rating_load_reference_ul_kbps\":{:.3},\"rating_load_enter_percent\":{:.3},\"rating_load_exit_percent\":{:.3},\"rating_load_enter_dl_percent\":{:.3},\"rating_load_enter_ul_percent\":{:.3},\"rating_load_exit_dl_percent\":{:.3},\"rating_load_exit_ul_percent\":{:.3},\"rating_load_enter_dl_kbps\":{:.3},\"rating_load_enter_ul_kbps\":{:.3},\"rating_load_phase_age_s\":{:.3},\"rating_capture_active\":{},\"rating_capture_mode\":\"{}\",\"rating_capture_requested_phase\":\"{}\",\"rating_capture_background_dl_kbps\":{:.3},\"rating_capture_background_ul_kbps\":{:.3},\"rating_capture_peak_dl_percent\":{:.3},\"rating_capture_peak_ul_percent\":{:.3},\"rating_capture_contaminated\":{},\"rating_capture_contamination_reason\":\"{}\",\"graph_history_enabled\":{},\"graph_history_budget_mode\":\"{}\",\"graph_history_configured_budget_kib\":{},\"graph_history_safe_max_kib\":{},\"graph_history_effective_total_kib\":{},\"graph_history_instance_budget_kib\":{},\"graph_history_used_total_kib\":{},\"graph_history_used_instance_kib\":{},\"graph_history_stored_samples\":{},\"graph_history_instances\":{},\"graph_history_mem_total_kib\":{},\"graph_history_mem_available_kib\":{},\"graph_history_paused_low_memory\":{}",
            json_escape(quality_grade.state),
            quality_grade.collected_samples,
            quality_grade.required_samples,
            quality_grade.baseline_ready,
            quality_grade.baseline_samples,
            quality_grade.baseline_required_samples,
            quality_grade.dl_samples,
            quality_grade.ul_samples,
            quality_grade.bidirectional_samples,
            json_f64_or_null(quality_grade.finalize_remaining_s, 1),
            quality_grade_result_json(
                quality_grade.current.as_ref(),
                quality_grade.current_stale,
            ),
            quality_grade_result_json(
                quality_grade.last_known.as_ref(),
                quality_grade.last_known_stale,
            ),
            self.rating_load_snapshot.phase.as_str(),
            self.rating_load_snapshot.candidate.as_str(),
            self.rating_load_snapshot.raw_dl_percent,
            self.rating_load_snapshot.raw_ul_percent,
            self.rating_load_snapshot.smoothed_dl_percent,
            self.rating_load_snapshot.smoothed_ul_percent,
            self.rating_load_snapshot.aggregate_dl_rate_kbps,
            self.rating_load_snapshot.aggregate_ul_rate_kbps,
            self.rating_load_snapshot.effective_dl_rate_kbps,
            self.rating_load_snapshot.effective_ul_rate_kbps,
            self.rating_load_snapshot.reference_dl_kbps,
            self.rating_load_snapshot.reference_ul_kbps,
            self.rating_load_snapshot.enter_dl_percent.max(self.rating_load_snapshot.enter_ul_percent),
            self.rating_load_snapshot.exit_dl_percent.max(self.rating_load_snapshot.exit_ul_percent),
            self.rating_load_snapshot.enter_dl_percent,
            self.rating_load_snapshot.enter_ul_percent,
            self.rating_load_snapshot.exit_dl_percent,
            self.rating_load_snapshot.exit_ul_percent,
            self.rating_load_snapshot.enter_dl_kbps,
            self.rating_load_snapshot.enter_ul_kbps,
            self.rating_load_snapshot.phase_age_s,
            self.rating_load_snapshot.capture_active,
            self.rating_load_snapshot.capture_mode,
            self.rating_load_snapshot.capture_requested_phase,
            self.rating_load_snapshot.capture_background_dl_kbps,
            self.rating_load_snapshot.capture_background_ul_kbps,
            self.rating_load_snapshot.capture_peak_dl_percent,
            self.rating_load_snapshot.capture_peak_ul_percent,
            self.rating_load_snapshot.capture_contaminated,
            json_escape(self.rating_load_snapshot.capture_contamination_reason),
            self.cfg.graph_history_enabled,
            if self.history_budget.configured_kib.is_some() {
                "manual"
            } else {
                "auto"
            },
            self.history_budget
                .configured_kib
                .map(|value| value.to_string())
                .unwrap_or_else(|| "null".to_string()),
            self.history_budget.safe_max_kib,
            self.history_budget.effective_total_kib,
            self.history_budget.instance_budget_kib,
            self.history_budget.used_total_kib,
            self.history_budget.used_instance_kib,
            self.history_sample_count,
            self.history_budget.instances,
            self.history_budget.memory.total_kib,
            self.history_budget.memory.available_kib,
            self.history_budget.paused_low_memory,
        )?;
        writeln!(file, "}}")?;
        let route_mode = self
            .route_snapshot
            .as_ref()
            .map(|snapshot| snapshot.identity.mode.as_str())
            .unwrap_or(self.cfg.route_mode.as_str());
        let route_member = self
            .route_snapshot
            .as_ref()
            .map(|snapshot| snapshot.identity.member.as_str())
            .unwrap_or(self.cfg.mwan3_member.as_str());
        let route_device = self
            .route_snapshot
            .as_ref()
            .map(|snapshot| snapshot.identity.device.as_str())
            .unwrap_or(self.cfg.ul_if.as_str());
        let route_source_ip = self
            .route_snapshot
            .as_ref()
            .map(|snapshot| snapshot.identity.source_ip.as_str())
            .unwrap_or("");
        let route_fwmark = self
            .route_snapshot
            .as_ref()
            .map(|snapshot| snapshot.identity.fwmark.as_str())
            .unwrap_or("");
        let route_table = self
            .route_snapshot
            .as_ref()
            .map(|snapshot| snapshot.identity.table.as_str())
            .unwrap_or("");
        let member_status = self
            .route_snapshot
            .as_ref()
            .map(|snapshot| snapshot.member_status.as_str())
            .unwrap_or("unknown");
        let route_active = self
            .route_snapshot
            .as_ref()
            .map(|snapshot| snapshot.active)
            .unwrap_or(false);
        let uplink_error = uplink_error_code(self.uplink_state, &self.uplink_reason);
        let transport_error = transport_error_code(transport.last_error.as_deref());
        file.seek(SeekFrom::End(-2))?;
        writeln!(
            file,
            ",\"uplink_state\":\"{}\",\"uplink_reason\":\"{}\",\"uplink_error_code\":{},\"transport_error_code\":{},\"route_mode_configured\":\"{}\",\"route_mode\":\"{}\",\"mwan3_member\":\"{}\",\"route_device\":\"{}\",\"route_source_ip\":\"{}\",\"route_external_ip\":\"{}\",\"route_fwmark\":\"{}\",\"route_table\":\"{}\",\"mwan3_member_status\":\"{}\",\"route_active\":{},\"route_identity\":{}}}",
            self.uplink_state.as_str(),
            json_escape(&self.uplink_reason),
            json_string_or_null(uplink_error),
            json_string_or_null(transport_error),
            json_escape(&self.cfg.route_mode),
            json_escape(route_mode),
            json_escape(route_member),
            json_escape(route_device),
            json_escape(route_source_ip),
            json_escape(&self.route_external_ip),
            json_escape(route_fwmark),
            json_escape(route_table),
            json_escape(member_status),
            route_active,
            json_string_or_null(self.route_identity.as_deref()),
        )?;
        fs::rename(tmp, path)?;
        self.last_status_publish = Instant::now();
        Ok(())
    }

    fn refresh_status_from_last_sample(&mut self) -> io::Result<()> {
        let Some(snapshot) = self.last_status.clone() else {
            return Ok(());
        };

        self.write_status_file(
            snapshot.dl_rate,
            snapshot.ul_rate,
            snapshot.dl_load_pct,
            snapshot.ul_load_pct,
            snapshot.dl_delay_count,
            snapshot.ul_delay_count,
            snapshot.avg_dl_delta,
            snapshot.avg_ul_delta,
            &snapshot.sample,
            &snapshot.active_reflectors,
            snapshot.health.as_ref(),
        )
    }

    fn write_initial_status(
        &mut self,
        active_reflectors: &[String],
        health: Option<&ReflectorHealth>,
    ) -> io::Result<()> {
        let sample = Sample {
            reflector: String::new(),
            seq: String::new(),
            timestamp: epoch_secs(),
            rtt_ms: 0.0,
            dl_owd_us: 0.0,
            ul_owd_us: 0.0,
            timestamped_owd: false,
        };
        self.write_status(
            0.0,
            0.0,
            0.0,
            0.0,
            0,
            0,
            0.0,
            0.0,
            &sample,
            active_reflectors,
            health,
        )
    }

    fn log(&mut self, kind: &str, msg: &str) {
        if kind == "DEBUG" && !self.cfg.debug {
            return;
        }
        let line = format!("{kind}; {:.6}; {msg}", epoch_secs());
        if kind == "DEBUG" && self.cfg.log_debug_messages_to_syslog {
            let _ = Command::new("logger")
                .arg("-t")
                .arg("cake-autorate-rs")
                .arg(&line)
                .status();
        }
        if let Some(file) = &mut self.log {
            let max_age = Duration::from_secs(self.cfg.log_file_max_time_mins.saturating_mul(60));
            let max_size = self.cfg.log_file_max_size_kb.saturating_mul(1024);
            let buffer_timeout = Duration::from_millis(self.cfg.log_file_buffer_timeout_ms);
            if let Err(e) = file.write_line(
                &line,
                max_age,
                max_size,
                self.cfg.log_file_buffer_size_b,
                buffer_timeout,
                self.cfg.log_file_export_compress,
            ) {
                eprintln!("failed to write log file: {e}");
            }
        } else {
            eprintln!("{line}");
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MainState {
    Running,
    Idle,
    Stall,
}

fn pinger_response_interval_s(cfg: &Config) -> f64 {
    cfg.reflector_ping_interval_s / cfg.no_pingers.max(1) as f64
}

fn stall_detection_timeout(cfg: &Config) -> Duration {
    Duration::from_secs_f64(
        (cfg.stall_detection_thr as f64 * pinger_response_interval_s(cfg)).max(0.1),
    )
}

fn monitor_tick_timeout(cfg: &Config) -> Duration {
    let configured_us = cfg
        .monitor_achieved_rates_interval_ms
        .max(100)
        .saturating_mul(1000);
    let compensated_us = (10.0
        * max_wire_packet_rtt_us(
            cfg,
            cfg.min_dl_shaper_rate_kbps,
            cfg.min_ul_shaper_rate_kbps,
        ))
    .ceil() as u64;

    Duration::from_micros(configured_us.max(compensated_us))
}

fn run(mut cfg: Config, once: bool) -> Result<(), String> {
    if !cfg.enabled {
        println!("cake-autorate-rs instance '{}' is disabled", cfg.instance);
        return Ok(());
    }

    if cfg.startup_wait_s > 0.0 {
        std::thread::sleep(Duration::from_secs_f64(cfg.startup_wait_s));
    }
    cfg.refresh_wire_packet_sizes();

    let mut controller = Controller::new(cfg.clone())?;
    let route_spec = cfg.route_spec();
    let mut route_inspector = RouteInspector::new(route_spec.clone());
    let mut uplink_lifecycle = UplinkLifecycle::new();
    let initial_inspected = route_inspector.inspect_fresh();
    let (mut current_route_snapshot, initial_route_error) = match initial_inspected {
        Ok(snapshot) => (Some(snapshot), None),
        Err(error) => (None, Some(error)),
    };
    let initial_transition = uplink_lifecycle.observe(
        current_route_snapshot.as_ref().map(Ok).unwrap_or_else(|| {
            Err(initial_route_error
                .as_deref()
                .unwrap_or("route unavailable"))
        }),
        Instant::now(),
        Duration::from_secs_f64(cfg.route_stability_s),
    );
    controller.set_uplink_route(
        current_route_snapshot.clone(),
        initial_transition.state,
        &initial_transition.reason,
        initial_transition.reset_learning,
    );
    controller.start();
    let mut active_reflectors: Vec<String> = cfg
        .reflectors
        .iter()
        .take(cfg.no_pingers)
        .cloned()
        .collect();
    let mut health = ReflectorHealth::new(&cfg, &active_reflectors);
    controller
        .write_initial_status(&active_reflectors, Some(&health))
        .map_err(|e| format!("failed to write status: {e}"))?;

    if once {
        println!(
            "cake-autorate-rs wrote initial status for '{}'",
            cfg.instance
        );
        return Ok(());
    }

    let mut pinger: Option<PingerRuntime> = None;
    let mut transport_probe = cfg
        .transport_latency_enabled
        .then(|| TransportProbeRuntime::spawn(&cfg));
    let mut external_ip_probe = ExternalIpRuntime::spawn(route_spec.clone());
    let mut main_state = MainState::Running;
    let mut idle_since: Option<Instant> = None;
    let mut last_reflector_response = Instant::now();
    let mut stall_started: Option<Instant> = None;
    let mut global_timeout_fired = false;
    let stall_timeout = stall_detection_timeout(&cfg);
    let global_timeout = Duration::from_secs_f64(cfg.global_ping_response_timeout_s.max(0.1));
    let idle_timeout = Duration::from_secs_f64(cfg.sustained_idle_sleep_thr_s.max(0.0));
    let route_check_interval = Duration::from_secs_f64(cfg.route_check_interval_s);
    let route_stability = Duration::from_secs_f64(cfg.route_stability_s);
    let mut last_route_check = Instant::now();
    let sqm_health_fast_interval = Duration::from_secs(SQM_RUNTIME_HEALTH_CHECK_FAST_S);
    let sqm_health_healthy_interval = Duration::from_secs(SQM_RUNTIME_HEALTH_CHECK_HEALTHY_S);
    let mut last_sqm_health_check = Instant::now()
        .checked_sub(sqm_health_healthy_interval)
        .unwrap_or_else(Instant::now);
    let mut route_probes_allowed = initial_transition.probes_allowed;
    external_ip_probe.maybe_start(route_probes_allowed);

    while !TERMINATE.load(Ordering::SeqCst) {
        let sqm_health_interval = if controller.sqm_runtime_healthy {
            sqm_health_healthy_interval
        } else {
            sqm_health_fast_interval
        };
        if last_sqm_health_check.elapsed() >= sqm_health_interval {
            last_sqm_health_check = Instant::now();
            let (sqm_ready, sqm_recovered) = controller.ensure_managed_sqm();
            if sqm_recovered {
                if let Some(mut old) = pinger.take() {
                    old.stop();
                }
                controller.note_probe_gap();
                main_state = MainState::Running;
                idle_since = None;
                stall_started = None;
                global_timeout_fired = false;
                health = ReflectorHealth::new(&cfg, &active_reflectors);
                if route_probes_allowed {
                    pinger = Some(PingerRuntime::spawn(&cfg, &active_reflectors)?);
                    last_reflector_response = Instant::now();
                }
            }
            if !sqm_ready {
                if let Some(mut old) = pinger.take() {
                    old.stop();
                }
                controller.note_probe_gap();
                thread::sleep(monitor_tick_timeout(&cfg));
                continue;
            }
        }
        external_ip_probe.drain(&mut controller);
        if last_route_check.elapsed() >= route_check_interval {
            last_route_check = Instant::now();
            let inspected = route_inspector.inspect();
            let transition = uplink_lifecycle.observe(
                inspected.as_ref().map_err(|error| error.as_str()),
                last_route_check,
                route_stability,
            );
            let must_stop = transition.became_offline || transition.identity_changed;
            if must_stop {
                if let Some(mut old) = pinger.take() {
                    old.stop();
                }
                controller.note_probe_gap();
            }
            current_route_snapshot = inspected.ok();
            if transition.identity_changed || transition.state == UplinkState::Offline {
                controller.set_route_external_ip(String::new());
            }
            route_probes_allowed = transition.probes_allowed;
            controller.set_uplink_route(
                current_route_snapshot.clone(),
                transition.state,
                &transition.reason,
                transition.reset_learning,
            );
            external_ip_probe.maybe_start(route_probes_allowed);
            if route_probes_allowed
                && pinger.is_none()
                && current_route_snapshot.is_some()
                && (main_state != MainState::Idle || transition.state == UplinkState::Learning)
            {
                if main_state == MainState::Idle {
                    main_state = MainState::Running;
                    controller.set_run_state("RUNNING");
                    idle_since = None;
                }
                health = ReflectorHealth::new(&cfg, &active_reflectors);
                pinger = Some(PingerRuntime::spawn(&cfg, &active_reflectors)?);
                last_reflector_response = Instant::now();
            }
        }
        if let Some(runtime) = transport_probe.as_mut() {
            runtime.drain(&mut controller);
        }
        let mut sampled_rates = None;
        if pinger.is_some() {
            let timeout = if main_state == MainState::Running {
                health.timeout(&cfg)
            } else {
                monitor_tick_timeout(&cfg)
            };
            let result = pinger.as_ref().unwrap().lines.recv_timeout(timeout);

            match result {
                Ok(Ok(line)) => {
                    if let Some(sample) = parse_sample_line(&cfg, &line.line, &line.reflector) {
                        if sample_is_stale(&sample, epoch_secs()) {
                            controller.note_probe_gap();
                            controller.log(
                                "DEBUG",
                                &format!(
                                    "processed response from [{}] that is > 500ms old. Skipping.",
                                    sample.reflector
                                ),
                            );
                            continue;
                        }
                        last_reflector_response = Instant::now();
                        if main_state == MainState::Stall {
                            controller.log("DEBUG", "Reflector response detected.");
                            controller.log(
                                "DEBUG",
                                "Connection stall ended. Resuming normal operation.",
                            );
                            main_state = MainState::Running;
                            controller.set_run_state("RUNNING");
                            stall_started = None;
                            global_timeout_fired = false;
                        }
                        health.observe_sample(&cfg, &sample);
                        sampled_rates =
                            Some(controller.on_sample(sample, &active_reflectors, &health));
                        if uplink_lifecycle
                            .record_learning_sample(cfg.no_pingers.max(1).saturating_mul(3))
                        {
                            controller.set_uplink_route(
                                current_route_snapshot.clone(),
                                uplink_lifecycle.state(),
                                uplink_lifecycle.reason(),
                                false,
                            );
                        }
                    }
                }
                Ok(Err(e)) => {
                    let inspected = route_inspector.inspect_fresh();
                    let route_is_online = inspected
                        .as_ref()
                        .map(|snapshot| snapshot.online)
                        .unwrap_or(false);
                    if route_is_online {
                        return Err(e);
                    }
                    if let Some(mut old) = pinger.take() {
                        old.stop();
                    }
                    let transition = uplink_lifecycle.observe(
                        inspected.as_ref().map_err(|error| error.as_str()),
                        Instant::now(),
                        route_stability,
                    );
                    current_route_snapshot = inspected.ok();
                    route_probes_allowed = false;
                    controller.set_uplink_route(
                        current_route_snapshot.clone(),
                        transition.state,
                        &transition.reason,
                        transition.reset_learning,
                    );
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    if TERMINATE.load(Ordering::SeqCst) {
                        break;
                    }

                    if cfg.pinger_method == "irtt" {
                        controller.note_probe_gap();
                        controller.log(
                            "DEBUG",
                            "irtt session ended; restarting irtt clients for active servers",
                        );
                        if let Some(mut old) = pinger.take() {
                            old.stop();
                        }
                        if route_probes_allowed {
                            pinger = Some(PingerRuntime::spawn(&cfg, &active_reflectors)?);
                        }
                        continue;
                    }

                    let inspected = route_inspector.inspect_fresh();
                    let route_is_online = inspected
                        .as_ref()
                        .map(|snapshot| snapshot.online)
                        .unwrap_or(false);
                    if route_is_online {
                        return Err(format!("{} output closed unexpectedly", cfg.pinger_method));
                    }
                    if let Some(mut old) = pinger.take() {
                        old.stop();
                    }
                    let transition = uplink_lifecycle.observe(
                        inspected.as_ref().map_err(|error| error.as_str()),
                        Instant::now(),
                        route_stability,
                    );
                    current_route_snapshot = inspected.ok();
                    route_probes_allowed = false;
                    controller.set_uplink_route(
                        current_route_snapshot.clone(),
                        transition.state,
                        &transition.reason,
                        transition.reset_learning,
                    );
                    continue;
                }
            }
        } else {
            thread::sleep(monitor_tick_timeout(&cfg));
        }

        let now = Instant::now();
        if now.duration_since(last_reflector_response)
            > Duration::from_secs_f64(cfg.reflector_response_deadline_s.max(0.1))
        {
            controller.note_probe_gap();
        }
        let rate_sample = sampled_rates.unwrap_or_else(|| controller.sample_rates());
        let dl_rate = rate_sample.dl_kbps;
        let ul_rate = rate_sample.ul_kbps;
        let rating_load = if rate_sample.fresh {
            controller.update_rating_load(now, dl_rate, ul_rate)
        } else {
            controller.rating_load_snapshot.clone()
        };
        if route_probes_allowed {
            if let Some(runtime) = transport_probe.as_mut() {
                runtime.drain(&mut controller);
                let (dl_shaper, ul_shaper) = controller.shaper_rates();
                let quality_baseline_ready = controller
                    .quality_grade
                    .snapshot(epoch_secs())
                    .baseline_ready;
                runtime.maybe_start(
                    &cfg,
                    dl_rate,
                    ul_rate,
                    (dl_shaper, ul_shaper),
                    &rating_load,
                    quality_baseline_ready,
                );
            }
        }
        if controller.maybe_sample_cpu() {
            let _ = controller.refresh_status_from_last_sample();
        }
        controller.maybe_record_graph_history(dl_rate, ul_rate);
        let connection_active =
            dl_rate > cfg.connection_active_thr_kbps || ul_rate > cfg.connection_active_thr_kbps;
        let stall_load_active =
            dl_rate > cfg.connection_stall_thr_kbps && ul_rate > cfg.connection_stall_thr_kbps;

        match main_state {
            MainState::Running => {
                if cfg.enable_sleep_function && uplink_lifecycle.state() != UplinkState::Learning {
                    if connection_active {
                        idle_since = None;
                    } else {
                        let idle_start = *idle_since.get_or_insert(now);
                        if now.duration_since(idle_start) >= idle_timeout {
                            controller.log("DEBUG", "Connection idle. Waiting for minimum load.");
                            if cfg.min_shaper_rates_enforcement {
                                controller.set_min_shaper_rates("sustained idle");
                            }
                            if let Some(mut old) = pinger.take() {
                                old.stop();
                            }
                            main_state = MainState::Idle;
                            controller.set_run_state("IDLE");
                            idle_since = None;
                            continue;
                        }
                    }
                }

                if now.duration_since(last_reflector_response) > stall_timeout {
                    controller.log(
                        "DEBUG",
                        &format!(
                            "Warning: no reflector response within: {:.2} seconds. Checking loads.",
                            stall_timeout.as_secs_f64()
                        ),
                    );
                    controller.log(
                        "DEBUG",
                        &format!(
                            "load check is: (( {:.0} kbps > {:.0} kbps for download && {:.0} kbps > {:.0} kbps for upload ))",
                            dl_rate,
                            cfg.connection_stall_thr_kbps,
                            ul_rate,
                            cfg.connection_stall_thr_kbps
                        ),
                    );

                    if stall_load_active {
                        controller.log(
                            "DEBUG",
                            "load above connection stall threshold so resuming normal operation.",
                        );
                        last_reflector_response = now;
                    } else {
                        controller.log("DEBUG", "Connection stall detected.");
                        main_state = MainState::Stall;
                        controller.set_run_state("STALL");
                        stall_started = Some(now);
                        global_timeout_fired = false;
                    }
                }

                if route_probes_allowed
                    && main_state == MainState::Running
                    && health.check(&cfg, &mut active_reflectors, &mut controller)
                {
                    controller.note_probe_gap();
                    if let Some(mut old) = pinger.take() {
                        old.stop();
                    }
                    pinger = Some(PingerRuntime::spawn(&cfg, &active_reflectors)?);
                }
            }
            MainState::Idle => {
                if connection_active && route_probes_allowed {
                    controller.log(
                        "DEBUG",
                        &format!(
                            "dl achieved rate: {:.0} kbps or ul achieved rate: {:.0} kbps exceeded connection active threshold: {:.0} kbps. Resuming normal operation.",
                            dl_rate,
                            ul_rate,
                            cfg.connection_active_thr_kbps
                        ),
                    );
                    main_state = MainState::Running;
                    controller.set_run_state("RUNNING");
                    last_reflector_response = Instant::now();
                    health = ReflectorHealth::new(&cfg, &active_reflectors);
                    pinger = Some(PingerRuntime::spawn(&cfg, &active_reflectors)?);
                }
            }
            MainState::Stall => {
                if stall_load_active {
                    controller.log(
                        "DEBUG",
                        &format!(
                            "dl achieved rate: {:.0} kbps and ul achieved rate: {:.0} kbps exceeded connection stall threshold: {:.0} kbps.",
                            dl_rate,
                            ul_rate,
                            cfg.connection_stall_thr_kbps
                        ),
                    );
                    controller.log(
                        "DEBUG",
                        "Connection stall ended. Resuming normal operation.",
                    );
                    main_state = MainState::Running;
                    controller.set_run_state("RUNNING");
                    stall_started = None;
                    global_timeout_fired = false;
                    last_reflector_response = now;
                } else if route_probes_allowed
                    && !global_timeout_fired
                    && now.duration_since(last_reflector_response) > global_timeout
                {
                    global_timeout_fired = true;
                    controller.log(
                        "SYSLOG",
                        &format!(
                            "Warning: Configured global ping response timeout: {} seconds exceeded.",
                            cfg.global_ping_response_timeout_s
                        ),
                    );
                    if cfg.min_shaper_rates_enforcement {
                        controller.set_min_shaper_rates("global ping response timeout");
                    }
                    controller.log("DEBUG", "Restarting pingers.");
                    if let Some(mut old) = pinger.take() {
                        old.stop();
                    }
                    pinger = Some(PingerRuntime::spawn(&cfg, &active_reflectors)?);
                    last_reflector_response = now;
                    stall_started = Some(now);
                } else if stall_started.is_none() {
                    stall_started = Some(now);
                }
            }
        }
    }

    if let Some(mut pinger) = pinger {
        pinger.stop();
    }
    Ok(())
}

struct PingerLine {
    line: String,
    reflector: String,
}

struct PingerRuntime {
    children: Vec<Child>,
    readers: Vec<JoinHandle<()>>,
    lines: Receiver<Result<PingerLine, String>>,
}

impl PingerRuntime {
    fn spawn(cfg: &Config, active_reflectors: &[String]) -> Result<Self, String> {
        let mut children = spawn_pingers(cfg, active_reflectors)?;
        let (tx, lines) = mpsc::channel();
        let mut readers = Vec::new();

        for idx in 0..children.len() {
            let stdout = match children[idx].stdout.take() {
                Some(stdout) => stdout,
                None => {
                    for child in &mut children {
                        stop_child(child);
                    }
                    return Err(format!("failed to capture {} stdout", cfg.pinger_method));
                }
            };
            let tx = tx.clone();
            let method = cfg.pinger_method.clone();
            let reflector = if method == "ping" || method == "irtt" {
                active_reflectors.get(idx).cloned().unwrap_or_default()
            } else {
                String::new()
            };
            readers.push(thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    match line {
                        Ok(line) => {
                            let event = PingerLine {
                                line,
                                reflector: reflector.clone(),
                            };
                            if tx.send(Ok(event)).is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Err(format!("failed to read {method} output: {e}")));
                            break;
                        }
                    }
                }
            }));
        }

        Ok(Self {
            children,
            readers,
            lines,
        })
    }

    fn stop(&mut self) {
        for child in &mut self.children {
            stop_child(child);
        }
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}

fn spawn_pingers(cfg: &Config, active_reflectors: &[String]) -> Result<Vec<Child>, String> {
    match cfg.pinger_method.as_str() {
        "fping" => Ok(vec![spawn_fping(cfg, active_reflectors, false)?]),
        "fping-ts" => Ok(vec![spawn_fping(cfg, active_reflectors, true)?]),
        "tsping" => Ok(vec![spawn_tsping(cfg, active_reflectors)?]),
        "irtt" => spawn_irtt(cfg, active_reflectors),
        "ping" => spawn_ping(cfg, active_reflectors),
        other => Err(format!("unsupported pinger_method={other}")),
    }
}

fn spawn_fping(
    cfg: &Config,
    active_reflectors: &[String],
    icmp_timestamp: bool,
) -> Result<Child, String> {
    let period_ms = (cfg.reflector_ping_interval_s * 1000.0).round().max(1.0) as u64;
    let interval_ms = (period_ms / active_reflectors.len().max(1) as u64).max(1);
    let targets: Vec<&str> = active_reflectors.iter().map(String::as_str).collect();

    if targets.is_empty() {
        return Err("at least one reflector is required".to_string());
    }

    let mut cmd = pinger_command(cfg, "fping")?;
    for arg in safe_extra_args(&cfg.ping_extra_args) {
        cmd.arg(arg);
    }
    cmd.arg("--timestamp")
        .arg("--loop")
        .arg("--period")
        .arg(period_ms.to_string())
        .arg("--interval")
        .arg(interval_ms.to_string())
        .arg("--timeout")
        .arg("10000");
    if icmp_timestamp {
        cmd.arg("--icmp-timestamp");
    }

    cmd.args(targets)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to start fping: {e}"))
}

fn spawn_tsping(cfg: &Config, active_reflectors: &[String]) -> Result<Child, String> {
    let period_ms = (cfg.reflector_ping_interval_s * 1000.0).round().max(1.0) as u64;
    let spacing_ms = (period_ms / active_reflectors.len().max(1) as u64).max(1);
    let sleep_ms = if active_reflectors.len() == 1 {
        spacing_ms
    } else {
        0
    };
    let targets: Vec<&str> = active_reflectors.iter().map(String::as_str).collect();

    if targets.is_empty() {
        return Err("at least one reflector is required".to_string());
    }

    let mut cmd = pinger_command(cfg, "tsping")?;
    for arg in safe_extra_args(&cfg.ping_extra_args) {
        cmd.arg(arg);
    }
    cmd.arg("--print-timestamps")
        .arg("--machine-readable=,")
        .arg("--sleep-time")
        .arg(sleep_ms.to_string())
        .arg("--target-spacing")
        .arg(spacing_ms.to_string())
        .args(targets)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to start tsping: {e}"))
}

fn spawn_ping(cfg: &Config, active_reflectors: &[String]) -> Result<Vec<Child>, String> {
    if active_reflectors.is_empty() {
        return Err("at least one reflector is required".to_string());
    }

    let mut children = Vec::new();
    for target in active_reflectors {
        match spawn_ping_child(cfg, target) {
            Ok(child) => children.push(child),
            Err(e) => {
                for child in &mut children {
                    stop_child(child);
                }
                return Err(e);
            }
        }
    }

    Ok(children)
}

fn spawn_ping_child(cfg: &Config, target: &str) -> Result<Child, String> {
    let interval_s = cfg.reflector_ping_interval_s.ceil().max(1.0) as u64;

    let mut cmd = pinger_command(cfg, "ping")?;
    cmd.arg("-n")
        .arg("-i")
        .arg(interval_s.to_string())
        .arg("-W")
        .arg("10");

    for arg in safe_extra_args(&cfg.ping_extra_args) {
        cmd.arg(arg);
    }

    cmd.arg(target)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to start ping: {e}"))
}

fn spawn_irtt(cfg: &Config, active_reflectors: &[String]) -> Result<Vec<Child>, String> {
    if active_reflectors.is_empty() {
        return Err("at least one irtt_server is required".to_string());
    }

    let interval = format!("{}s", cfg.reflector_ping_interval_s);
    let duration = format!("{}m", cfg.irtt_session_duration_m);
    let mut children = Vec::new();

    for target in active_reflectors {
        let mut cmd = pinger_command(cfg, "irtt")?;
        cmd.arg("client");
        for arg in safe_extra_args(&cfg.ping_extra_args) {
            cmd.arg(arg);
        }
        cmd.arg("-i")
            .arg(&interval)
            .arg("-d")
            .arg(&duration)
            .arg(irtt_target_arg(target))
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        match cmd.spawn() {
            Ok(child) => children.push(child),
            Err(e) => {
                for child in &mut children {
                    stop_child(child);
                }
                return Err(format!("failed to start irtt for {target}: {e}"));
            }
        }
    }

    Ok(children)
}

fn pinger_command(cfg: &Config, binary: &str) -> Result<Command, String> {
    routing::routed_command(&cfg.route_spec(), &cfg.ping_prefix_string, binary)
}

fn stop_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn parse_sample_line(cfg: &Config, line: &str, ping_reflector: &str) -> Option<Sample> {
    match cfg.pinger_method.as_str() {
        "ping" => parse_ping_line(line, ping_reflector),
        "irtt" => parse_irtt_line(line, ping_reflector),
        "fping-ts" => parse_fping_ts_line(line),
        "tsping" => parse_tsping_line(line),
        _ => parse_fping_line(line),
    }
}

fn parse_fping_line(line: &str) -> Option<Sample> {
    let tokens: Vec<&str> = line
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|v| !v.is_empty())
        .collect();

    if tokens.len() >= 7 {
        let timestamp = tokens[0].trim_matches(['[', ']']).parse::<f64>().ok()?;
        let reflector = tokens[1].trim_end_matches(':').to_string();
        let seq = tokens[3].trim_matches(['[', ']']).to_string();
        let rtt_ms = tokens[6].parse::<f64>().ok()?;
        return Some(Sample {
            reflector,
            seq,
            timestamp,
            rtt_ms,
            dl_owd_us: rtt_ms * 500.0,
            ul_owd_us: rtt_ms * 500.0,
            timestamped_owd: false,
        });
    }

    None
}

fn parse_fping_ts_line(line: &str) -> Option<Sample> {
    let tokens: Vec<&str> = line
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|v| !v.is_empty())
        .collect();

    if tokens.len() < 17 || !tokens.contains(&"timestamps:") {
        return None;
    }

    let timestamp = tokens[0].trim_matches(['[', ']']).parse::<f64>().ok()?;
    let reflector = tokens[1].trim_end_matches(':').to_string();
    let seq = tokens[3].trim_matches(['[', ']']).to_string();
    let rtt_ms = tokens[6].parse::<f64>().ok()?;
    let originate = parse_prefixed_f64(tokens[13], "Originate=")?;
    let received = parse_prefixed_f64(tokens[14], "Receive=")?;
    let transmit = parse_prefixed_f64(tokens[15], "Transmit=")?;
    let finished = parse_prefixed_f64(tokens[16], "Localreceive=")?;
    let dl_owd_us = (finished - transmit) * 1000.0;
    let ul_owd_us = (received - originate) * 1000.0;

    Some(Sample {
        reflector,
        seq,
        timestamp,
        rtt_ms,
        dl_owd_us,
        ul_owd_us,
        timestamped_owd: true,
    })
}

fn parse_tsping_line(line: &str) -> Option<Sample> {
    let tokens: Vec<&str> = line
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect();

    if tokens.len() != 10 {
        return None;
    }

    let timestamp = tokens[0].parse::<f64>().ok()?;
    let reflector = tokens[1].trim_end_matches(':').to_string();
    let seq = tokens[2].to_string();
    let dl_owd_ms = tokens[8].parse::<f64>().ok()?;
    let ul_owd_ms = tokens[9].parse::<f64>().ok()?;
    let dl_owd_us = dl_owd_ms * 1000.0;
    let ul_owd_us = ul_owd_ms * 1000.0;

    Some(Sample {
        reflector,
        seq,
        timestamp,
        rtt_ms: dl_owd_ms + ul_owd_ms,
        dl_owd_us,
        ul_owd_us,
        timestamped_owd: true,
    })
}

fn parse_irtt_line(line: &str, reflector: &str) -> Option<Sample> {
    if reflector.is_empty()
        || !line.contains("seq=")
        || !line.contains("rd=")
        || !line.contains("sd=")
    {
        return None;
    }

    let seq = parse_irtt_token(line, "seq=")?.to_string();
    let dl_owd_us = parse_irtt_duration_us(parse_irtt_token(line, "rd=")?)?;
    let ul_owd_us = parse_irtt_duration_us(parse_irtt_token(line, "sd=")?)?;

    Some(Sample {
        reflector: reflector.to_string(),
        seq,
        timestamp: epoch_secs(),
        rtt_ms: (dl_owd_us + ul_owd_us) / 1000.0,
        dl_owd_us,
        ul_owd_us,
        timestamped_owd: true,
    })
}

fn parse_ping_line(line: &str, reflector: &str) -> Option<Sample> {
    if reflector.is_empty() {
        return None;
    }

    let rtt_ms = parse_ping_number_after(line, "time=")
        .or_else(|| parse_ping_number_after(line, "time<"))?;
    let seq = parse_ping_token_after(line, "icmp_seq=")
        .or_else(|| parse_ping_token_after(line, "seq="))
        .unwrap_or_else(|| "0".to_string());

    Some(Sample {
        reflector: reflector.to_string(),
        seq,
        timestamp: epoch_secs(),
        rtt_ms,
        dl_owd_us: rtt_ms * 500.0,
        ul_owd_us: rtt_ms * 500.0,
        timestamped_owd: false,
    })
}

fn sample_is_stale(sample: &Sample, now_secs: f64) -> bool {
    sample.timestamp.is_finite()
        && now_secs.is_finite()
        && now_secs - sample.timestamp > STALE_REFLECTOR_RESPONSE_MAX_AGE_S
}

fn parse_prefixed_f64(value: &str, prefix: &str) -> Option<f64> {
    value.strip_prefix(prefix)?.parse::<f64>().ok()
}

fn parse_ping_number_after(line: &str, marker: &str) -> Option<f64> {
    let start = line.find(marker)? + marker.len();
    let value: String = line[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
        .collect();

    if value.is_empty() {
        None
    } else {
        value.parse::<f64>().ok()
    }
}

fn parse_ping_token_after(line: &str, marker: &str) -> Option<String> {
    let start = line.find(marker)? + marker.len();
    let value: String = line[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        .collect();

    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn parse_irtt_token<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    line.split_whitespace()
        .map(|token| token.trim_matches(|ch| matches!(ch, ',' | ';' | ')' | '(')))
        .find_map(|token| token.strip_prefix(prefix))
        .map(|value| value.trim_matches(|ch| matches!(ch, ',' | ';' | ')' | '(')))
        .filter(|value| !value.is_empty())
}

fn parse_irtt_duration_us(value: &str) -> Option<f64> {
    if value.starts_with('-') {
        return None;
    }

    let (number, multiplier) = if let Some(number) = value.strip_suffix("ms") {
        (number, 1000.0)
    } else if let Some(number) = value
        .strip_suffix("us")
        .or_else(|| value.strip_suffix("\u{00b5}s"))
    {
        (number, 1.0)
    } else if let Some(number) = value.strip_suffix("ns") {
        (number, 0.001)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1_000_000.0)
    } else {
        return None;
    };

    Some(number.parse::<f64>().ok()? * multiplier)
}

fn safe_extra_args(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .filter(|arg| {
            !arg.is_empty()
                && arg.len() <= 64
                && arg.chars().all(|ch| {
                    ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':' | '/' | '=')
                })
        })
        .map(str::to_string)
        .collect()
}

fn fetch_url_text(url: &str) -> Result<String, String> {
    let commands: &[(&str, &[&str])] = &[
        ("curl", &["-fsSL", "--max-time", "20"]),
        ("uclient-fetch", &["-q", "-O", "-", "--timeout=20"]),
        ("wget", &["-q", "-O", "-"]),
    ];

    let mut last_error = String::new();
    for (bin, args) in commands {
        let mut cmd = Command::new(bin);
        cmd.args(*args).arg(url);

        match cmd.output() {
            Ok(output) if output.status.success() => {
                return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
            }
            Ok(output) => {
                last_error = format!("{bin} exited with {}", output.status);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => last_error = format!("{bin}: {e}"),
        }
    }

    if last_error.is_empty() {
        Err("no curl, uclient-fetch, or wget binary found".to_string())
    } else {
        Err(last_error)
    }
}

fn parse_reflector_candidates(data: &str, skip_lines: usize) -> Vec<String> {
    let mut reflectors = Vec::new();

    for line in data.lines().skip(skip_lines) {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }

        for token in line.split(|c: char| c == ',' || c == ';' || c.is_whitespace()) {
            let token = token.trim_matches(['"', '\'']).trim();
            if is_valid_reflector_candidate(token) {
                reflectors.push(token.to_string());
                break;
            }
        }
    }

    reflectors
}

fn is_valid_reflector_candidate(value: &str) -> bool {
    if value.is_empty() || value.len() > 253 || value.contains("://") {
        return false;
    }

    value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | ':'))
}

fn is_valid_irtt_server_candidate(value: &str) -> bool {
    if value.is_empty() || value.len() > 253 || value.contains("://") {
        return false;
    }

    value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | ':' | '[' | ']'))
}

fn deduplicate_list(values: &mut Vec<String>) {
    let mut seen: Vec<String> = Vec::new();
    values.retain(|value| {
        if seen.iter().any(|existing| existing == value) {
            false
        } else {
            seen.push(value.clone());
            true
        }
    });
}

fn irtt_target_arg(target: &str) -> String {
    let colon_count = target
        .as_bytes()
        .iter()
        .filter(|byte| **byte == b':')
        .count();
    if colon_count > 1 && !target.starts_with('[') {
        format!("[{target}]")
    } else {
        target.to_string()
    }
}

fn randomize_reflectors(reflectors: &mut [String]) {
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;

    reflectors.sort_by_key(|reflector| stable_hash(reflector) ^ seed);
}

fn stable_hash(value: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in value.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn read_cpu_snapshot() -> io::Result<CpuSnapshot> {
    let data = fs::read_to_string("/proc/stat")?;
    let mut counters = Vec::new();
    let mut raw_lines = Vec::new();

    for line in data.lines() {
        if !line.starts_with("cpu") {
            continue;
        }

        let mut parts = line.split_whitespace();
        let Some(name) = parts.next() else {
            continue;
        };
        if name != "cpu" && !name[3..].chars().all(|ch| ch.is_ascii_digit()) {
            continue;
        }

        let values: Vec<u64> = parts
            .filter_map(|value| value.parse::<u64>().ok())
            .collect();
        if values.len() < 4 {
            continue;
        }

        let idle = values
            .get(3)
            .copied()
            .unwrap_or(0)
            .saturating_add(values.get(4).copied().unwrap_or(0));
        let total = values.iter().copied().sum();

        counters.push(CpuCounters { total, idle });
        raw_lines.push(line.to_string());
    }

    Ok(CpuSnapshot {
        counters,
        raw_lines,
    })
}

fn rotated_log_path(path: &Path) -> PathBuf {
    let timestamp = epoch_secs().round().max(0.0) as u64;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("cake-autorate.log");
    let rotated = format!("{name}.{timestamp}");

    path.parent()
        .map(|parent| parent.join(&rotated))
        .unwrap_or_else(|| PathBuf::from(rotated))
}

fn ensure_run_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

fn wait_for_path(path: &str, interval_s: f64) -> Result<(), String> {
    let p = Path::new(path);
    while !p.exists() {
        if TERMINATE.load(Ordering::SeqCst) {
            return Err("terminated while waiting for interface counters".to_string());
        }
        eprintln!("waiting for {path}");
        std::thread::sleep(Duration::from_secs_f64(interval_s.max(1.0)));
    }
    Ok(())
}

fn read_u64_file<P: AsRef<Path>>(path: P) -> io::Result<u64> {
    let value = fs::read_to_string(path)?;
    value
        .trim()
        .parse::<u64>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn interface_max_wire_packet_size_bits(interface: &str) -> u64 {
    let mtu_path = format!("/sys/class/net/{interface}/mtu");
    let mtu_bytes = read_u64_file(&mtu_path).unwrap_or(1500);
    let tc_output = Command::new("tc")
        .arg("qdisc")
        .arg("show")
        .arg("dev")
        .arg(interface)
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    let (atm, overhead_bytes) = parse_tc_linklayer_overhead(&tc_output);

    max_wire_packet_size_bits_from_mtu(mtu_bytes, overhead_bytes, atm)
}

fn parse_tc_linklayer_overhead(output: &str) -> (bool, u64) {
    let tokens: Vec<&str> = output.split_whitespace().collect();

    for window in tokens.windows(3) {
        if (window[0] == "atm" || window[0] == "noatm") && window[1] == "overhead" {
            if let Ok(overhead) = window[2].parse::<u64>() {
                return (window[0] == "atm", overhead);
            }
        }
    }

    (false, 0)
}

fn max_wire_packet_size_bits_from_mtu(mtu_bytes: u64, overhead_bytes: u64, atm: bool) -> u64 {
    let bits = mtu_bytes.saturating_add(overhead_bytes).saturating_mul(8);
    if atm {
        424_u64.saturating_mul(bits.saturating_add(376) / 384)
    } else {
        bits
    }
}

fn packet_compensation_us(packet_size_bits: u64, shaper_rate_kbps: f64) -> f64 {
    if packet_size_bits == 0 || shaper_rate_kbps <= 0.0 {
        0.0
    } else {
        1000.0 * packet_size_bits as f64 / shaper_rate_kbps
    }
}

fn max_wire_packet_rtt_us(cfg: &Config, dl_rate_kbps: f64, ul_rate_kbps: f64) -> f64 {
    packet_compensation_us(cfg.dl_max_wire_packet_size_bits, dl_rate_kbps)
        + packet_compensation_us(cfg.ul_max_wire_packet_size_bits, ul_rate_kbps)
}

fn filled_bool_window(len: usize) -> VecDeque<bool> {
    let mut out = VecDeque::with_capacity(len);
    for _ in 0..len {
        out.push_back(false);
    }
    out
}

fn filled_f64_window(len: usize) -> VecDeque<f64> {
    let mut out = VecDeque::with_capacity(len);
    for _ in 0..len {
        out.push_back(0.0);
    }
    out
}

fn push_window<T>(window: &mut VecDeque<T>, value: T) {
    if window.len() == window.capacity() {
        window.pop_front();
    }
    window.push_back(value);
}

fn average(values: &VecDeque<f64>) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

fn classify_load(
    load_pct: f64,
    achieved_kbps: f64,
    active_thr_kbps: f64,
    high_load_pct: f64,
) -> LoadKind {
    if load_pct > high_load_pct {
        LoadKind::High
    } else if achieved_kbps > active_thr_kbps {
        LoadKind::Low
    } else {
        LoadKind::Idle
    }
}

fn shaper_update_due(last: u64, target: u64, since_last_attempt: Duration) -> bool {
    if target == last {
        return false;
    }
    last == 0 || target < last || since_last_attempt >= CAKE_GROWTH_UPDATE_MIN_INTERVAL
}

fn status_publish_due(since_last_publish: Duration) -> bool {
    since_last_publish >= STATUS_PUBLISH_INTERVAL
}

fn load_label(kind: LoadKind, bb: bool, prefix: &str) -> String {
    let base = match kind {
        LoadKind::High => "high",
        LoadKind::Low => "low",
        LoadKind::Idle => "idle",
    };
    if bb {
        format!("{prefix}_{base}_bb")
    } else {
        format!("{prefix}_{base}")
    }
}

fn percent(value: f64, base: f64) -> f64 {
    if base <= 0.0 {
        0.0
    } else {
        value * 100.0 / base
    }
}

fn parse_uci_values(value: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    let mut token_started = false;
    let mut chars = value.trim().chars();

    while let Some(ch) = chars.next() {
        if in_quote {
            match ch {
                '\'' => in_quote = false,
                '\\' => {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                }
                _ => current.push(ch),
            }
            token_started = true;
        } else {
            match ch {
                '\'' => {
                    in_quote = true;
                    token_started = true;
                }
                c if c.is_whitespace() => {
                    if token_started {
                        values.push(std::mem::take(&mut current));
                        token_started = false;
                    }
                }
                _ => {
                    current.push(ch);
                    token_started = true;
                }
            }
        }
    }

    if token_started {
        values.push(current);
    }

    values
}

fn load_global_history_config() -> Result<(Option<u64>, usize), String> {
    let output = match Command::new("uci")
        .arg("-q")
        .arg("show")
        .arg("cake-autorate")
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return Ok((None, 1)),
    };
    let data = String::from_utf8_lossy(&output.stdout);
    let mut types = HashMap::<String, String>::new();
    let mut enabled = HashMap::<String, bool>::new();
    let mut history_enabled = HashMap::<String, bool>::new();
    let mut budget = None;

    for line in data.lines() {
        let Some((left, raw_value)) = line.split_once('=') else {
            continue;
        };
        let values = parse_uci_values(raw_value);
        let Some(value) = values.first() else {
            continue;
        };
        let parts = left.split('.').collect::<Vec<_>>();
        if parts.len() == 2 && parts[0] == "cake-autorate" {
            types.insert(parts[1].to_string(), value.to_string());
            continue;
        }
        if parts.len() != 3 || parts[0] != "cake-autorate" {
            continue;
        }
        match parts[2] {
            "enabled" => {
                enabled.insert(
                    parts[1].to_string(),
                    parse_bool(value).map_err(|error| format!("{}.enabled: {error}", parts[1]))?,
                );
            }
            "graph_history_enabled" => {
                history_enabled.insert(
                    parts[1].to_string(),
                    parse_bool(value)
                        .map_err(|error| format!("{}.graph_history_enabled: {error}", parts[1]))?,
                );
            }
            "graph_history_ram_budget_kib"
                if parts[1] == "globals" && value != "auto" && !value.is_empty() =>
            {
                budget = Some(
                    value
                        .parse::<u64>()
                        .map_err(|error| format!("graph_history_ram_budget_kib: {error}"))?,
                );
            }
            _ => {}
        }
    }

    let count = types
        .iter()
        .filter(|(section, section_type)| {
            section_type.as_str() == "cake_autorate"
                && enabled.get(*section).copied().unwrap_or(false)
                && history_enabled.get(*section).copied().unwrap_or(false)
        })
        .count()
        .max(1);
    Ok((budget, count))
}

fn parse_bool(value: &str) -> Result<bool, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" | "enabled" => Ok(true),
        "0" | "false" | "no" | "off" | "disabled" => Ok(false),
        _ => Err(format!("invalid boolean value '{value}'")),
    }
}

fn set_string(map: &HashMap<String, String>, key: &str, out: &mut String) {
    if let Some(value) = map.get(key) {
        *out = value.clone();
    }
}

fn set_bool(map: &HashMap<String, String>, key: &str, out: &mut bool) -> Result<(), String> {
    if let Some(value) = map.get(key) {
        *out = parse_bool(value).map_err(|e| format!("{key}: {e}"))?;
    }
    Ok(())
}

fn set_f64(map: &HashMap<String, String>, key: &str, out: &mut f64) -> Result<(), String> {
    if let Some(value) = map.get(key) {
        *out = value.parse::<f64>().map_err(|e| format!("{key}: {e}"))?;
    }
    Ok(())
}

fn set_u64(map: &HashMap<String, String>, key: &str, out: &mut u64) -> Result<(), String> {
    if let Some(value) = map.get(key) {
        *out = value.parse::<u64>().map_err(|e| format!("{key}: {e}"))?;
    }
    Ok(())
}

fn set_usize(map: &HashMap<String, String>, key: &str, out: &mut usize) -> Result<(), String> {
    if let Some(value) = map.get(key) {
        *out = value.parse::<usize>().map_err(|e| format!("{key}: {e}"))?;
    }
    Ok(())
}

fn epoch_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn json_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn json_f64_or_null(value: Option<f64>, precision: usize) -> String {
    match value {
        Some(value) if value.is_finite() => format!("{:.*}", precision, value),
        _ => "null".to_string(),
    }
}

fn json_string_or_null(value: Option<&str>) -> String {
    value
        .map(|value| format!("\"{}\"", json_escape(value)))
        .unwrap_or_else(|| "null".to_string())
}

fn quality_grade_metric_json(metric: Option<&QualityGradeMetric>) -> String {
    let Some(metric) = metric else {
        return "null".to_string();
    };
    format!(
        "{{\"grade\":\"{}\",\"increase_ms\":{:.3},\"loaded_p90_ms\":{:.3},\"samples\":{}}}",
        metric.class.as_str(),
        metric.increase_ms,
        metric.loaded_p90_ms,
        metric.samples,
    )
}

fn quality_grade_result_json(result: Option<&QualityGradeResult>, stale: bool) -> String {
    let Some(result) = result else {
        return "null".to_string();
    };
    format!(
        "{{\"grade\":\"{}\",\"increase_ms\":{:.3},\"baseline_p5_ms\":{:.3},\"endpoint\":\"{}\",\"started_at\":{:.3},\"completed_at\":{},\"route_identity\":\"{}\",\"partial\":{},\"incomplete\":{},\"completion_reason\":\"{}\",\"stale\":{},\"samples\":{},\"dl_samples\":{},\"ul_samples\":{},\"bidirectional_samples\":{},\"dl\":{},\"ul\":{},\"bidirectional\":{}}}",
        result.class.as_str(),
        result.increase_ms,
        result.baseline_p5_ms,
        json_escape(&result.endpoint),
        result.started_at,
        json_f64_or_null(result.completed_at, 3),
        json_escape(&result.route_identity),
        result.partial,
        result.incomplete,
        json_escape(&result.completion_reason),
        stale,
        result.samples(),
        result.dl_samples,
        result.ul_samples,
        result.bidirectional_samples,
        quality_grade_metric_json(result.dl.as_ref()),
        quality_grade_metric_json(result.ul.as_ref()),
        quality_grade_metric_json(result.bidirectional.as_ref()),
    )
}

fn json_f64_or_empty(value: Option<f64>, precision: usize) -> String {
    match value {
        Some(value) if value.is_finite() => format!("{:.*}", precision, value),
        _ => String::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn graph_history_line(
    timestamp: f64,
    rtt_ms: Option<f64>,
    cpu_percent: Option<f64>,
    dl_rate_kbps: f64,
    ul_rate_kbps: f64,
    transport_delta_ms: Option<f64>,
    effective_delta_ms: Option<f64>,
    dl_floor_kbps: Option<f64>,
    ul_floor_kbps: Option<f64>,
    uplink_state: &str,
    route_identity: &str,
    grade: Option<&str>,
    grade_state: &str,
    grade_increase_ms: Option<f64>,
    rating_phase: &str,
    rating_dl_samples: usize,
    rating_ul_samples: usize,
) -> String {
    format!(
        "{:.0},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
        timestamp,
        json_f64_or_empty(rtt_ms, 3),
        json_f64_or_empty(cpu_percent, 1),
        json_f64_or_empty(Some(dl_rate_kbps), 1),
        json_f64_or_empty(Some(ul_rate_kbps), 1),
        json_f64_or_empty(transport_delta_ms, 3),
        json_f64_or_empty(effective_delta_ms, 3),
        json_f64_or_empty(dl_floor_kbps, 1),
        json_f64_or_empty(ul_floor_kbps, 1),
        uplink_state.replace(',', ""),
        route_identity.replace(',', ""),
        grade.unwrap_or("").replace(',', ""),
        grade_state.replace(',', ""),
        json_f64_or_empty(grade_increase_ms, 3),
        rating_phase.replace(',', ""),
        rating_dl_samples,
        rating_ul_samples,
    )
}

#[cfg(test)]
fn compact_graph_history_data(data: &str, max_bytes: usize) -> String {
    let mut newest = Vec::new();
    let mut bytes = 0usize;

    for line in data.lines().rev() {
        let line_bytes = line.len().saturating_add(1);
        if !newest.is_empty() && bytes.saturating_add(line_bytes) > max_bytes {
            break;
        }
        newest.push(line);
        bytes = bytes.saturating_add(line_bytes);
    }

    newest.reverse();
    if newest.is_empty() {
        String::new()
    } else {
        format!("{}\n", newest.join("\n"))
    }
}

fn compact_graph_history_file(path: &Path, max_bytes: u64) -> io::Result<u64> {
    if max_bytes == 0 {
        return match fs::remove_file(path) {
            Ok(()) => Ok(0),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(error),
        };
    }
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    if metadata.len() <= max_bytes {
        return Ok(0);
    }

    let mut source = File::open(path)?;
    let start = metadata.len().saturating_sub(max_bytes);
    let starts_at_line = if start > 0 {
        source.seek(SeekFrom::Start(start - 1))?;
        let mut previous = [0u8; 1];
        source.read_exact(&mut previous)?;
        previous[0] == b'\n'
    } else {
        true
    };
    source.seek(SeekFrom::Start(start))?;
    let mut reader = BufReader::new(source);
    if !starts_at_line {
        let mut partial = Vec::new();
        reader.read_until(b'\n', &mut partial)?;
    }
    let tmp = path.with_extension("csv.tmp");
    let mut output = BufWriter::new(File::create(&tmp)?);
    let mut samples = 0u64;
    loop {
        let mut line = Vec::new();
        if reader.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        output.write_all(&line)?;
        samples = samples.saturating_add(1);
    }
    output.flush()?;
    fs::rename(tmp, path)?;
    Ok(samples)
}

fn json_f64_array(values: &[f64], precision: usize) -> String {
    let out: Vec<String> = values
        .iter()
        .map(|value| {
            if value.is_finite() {
                format!("{:.*}", precision, value)
            } else {
                "null".to_string()
            }
        })
        .collect();
    format!("[{}]", out.join(","))
}

fn default_reflectors() -> Vec<String> {
    UPSTREAM_DEFAULT_REFLECTORS
        .iter()
        .map(|reflector| (*reflector).to_string())
        .collect()
}

fn json_bool(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

fn json_string_array(values: &[String]) -> String {
    let out: Vec<String> = values
        .iter()
        .map(|value| format!("\"{}\"", json_escape(value)))
        .collect();
    format!("[{}]", out.join(","))
}

fn reflector_spare_reflectors(cfg: &Config, active: &[String]) -> Vec<String> {
    cfg.reflectors
        .iter()
        .filter(|reflector| !active.iter().any(|active| active == *reflector))
        .cloned()
        .collect()
}

fn reflector_bad_reflectors(cfg: &Config, health: Option<&ReflectorHealth>) -> Vec<String> {
    let Some(health) = health else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for reflector in &cfg.reflectors {
        if health
            .states
            .get(reflector)
            .map(|state| state.offence_sum >= cfg.reflector_misbehaving_detection_thr)
            .unwrap_or(false)
        {
            out.push(reflector.clone());
        }
    }

    for (reflector, state) in &health.states {
        if state.offence_sum >= cfg.reflector_misbehaving_detection_thr
            && !out.iter().any(|value| value == reflector)
        {
            out.push(reflector.clone());
        }
    }

    out
}

fn reflector_health_json(
    cfg: &Config,
    active: &[String],
    health: Option<&ReflectorHealth>,
) -> String {
    let now = Instant::now();
    let mut reflectors = cfg.reflectors.clone();
    for reflector in active {
        if !reflectors.iter().any(|value| value == reflector) {
            reflectors.push(reflector.clone());
        }
    }

    if let Some(health) = health {
        for reflector in health.states.keys() {
            if !reflectors.iter().any(|value| value == reflector) {
                reflectors.push(reflector.clone());
            }
        }
    }

    let out: Vec<String> = reflectors
        .iter()
        .map(|reflector| {
            let state = health.and_then(|health| health.states.get(reflector));
            let active = active.iter().any(|value| value == reflector);
            let bad = state
                .map(|state| state.offence_sum >= cfg.reflector_misbehaving_detection_thr)
                .unwrap_or(false);
            let last_seen_age_s = state.map(|state| now.duration_since(state.last_seen).as_secs_f64());
            let last_rtt_ms = state.and_then(|state| {
                if state.samples > 0 {
                    Some(state.last_rtt_ms)
                } else {
                    None
                }
            });

            format!(
                "{{\"host\":\"{}\",\"active\":{},\"spare\":{},\"bad\":{},\"samples\":{},\"offence_sum\":{},\"offence_threshold\":{},\"last_rtt_ms\":{},\"last_seen_age_s\":{}}}",
                json_escape(reflector),
                json_bool(active),
                json_bool(!active),
                json_bool(bad),
                state.map(|state| state.samples).unwrap_or(0),
                state.map(|state| state.offence_sum).unwrap_or(0),
                cfg.reflector_misbehaving_detection_thr,
                json_f64_or_null(last_rtt_ms, 3),
                json_f64_or_null(last_seen_age_s, 3)
            )
        })
        .collect();

    format!("[{}]", out.join(","))
}

fn print_usage() {
    eprintln!("usage: cake-autorated [--instance NAME] [--once] [--dump-config]");
    eprintln!(
        "       cake-autorated --autotune-proposal --dl-samples LIST --ul-samples LIST \\\n         --idle-median-ms N --idle-p95-ms N --idle-samples N [--link-kind KIND] \\\n         [--profile gaming|best_overall|fair] \\\n         [--base-scale N | --dl-base-scale N --ul-base-scale N]"
    );
    eprintln!("       cake-autorated --autotune-validate [--profile gaming|best_overall|fair] --dl-observed-low-kbps N --ul-observed-low-kbps N --dl-candidate-kbps N --ul-candidate-kbps N --dl-achieved-kbps N --ul-achieved-kbps N --dl-min-kbps N --ul-min-kbps N --dl-max-kbps N --ul-max-kbps N --icmp-delta-ms N --transport-delta-ms N --loss-percent N --cpu-percent N");
    eprintln!("         [--dl-icmp-delta-ms N --ul-icmp-delta-ms N --dl-transport-delta-ms N --ul-transport-delta-ms N --dl-loss-percent N --ul-loss-percent N --dl-cpu-percent N --ul-cpu-percent N]");
    eprintln!("       cake-autorated --transport-probe --backend websocket|tcp|http|legacy-http [--endpoint URL] [--device IFACE] [--source-ip IPv4] [--fwmark HEX] [--count N] [--timeout SEC] [--interval-ms N]");
}

fn parse_rate_samples(value: &str) -> Result<Vec<f64>, String> {
    if value.trim().is_empty() {
        return Err("rate sample list must not be empty".to_string());
    }
    let mut samples = Vec::new();
    for (index, value) in value.split(',').enumerate() {
        if index >= autotune::MAX_THROUGHPUT_SAMPLES {
            return Err(format!(
                "rate sample count must not exceed {}",
                autotune::MAX_THROUGHPUT_SAMPLES
            ));
        }
        let value = value.trim();
        if value.is_empty() {
            return Err(format!("rate sample {} is empty", index + 1));
        }
        let sample = value
            .parse::<f64>()
            .map_err(|_| format!("invalid rate sample {}: {value}", index + 1))?;
        if !sample.is_finite() || sample <= 0.0 || sample > autotune::MAX_RATE_KBPS as f64 {
            return Err(format!(
                "rate sample {} must be finite and between 0 and {} kbit/s",
                index + 1,
                autotune::MAX_RATE_KBPS
            ));
        }
        samples.push(sample);
    }
    Ok(samples)
}

fn parse_optional_rate(value: &str) -> Result<Option<u64>, String> {
    let rate = value
        .parse::<u64>()
        .map_err(|_| format!("invalid current rate: {value}"))?;
    if rate > autotune::MAX_RATE_KBPS {
        return Err(format!(
            "current rate must not exceed {} kbit/s",
            autotune::MAX_RATE_KBPS
        ));
    }
    Ok((rate > 0).then_some(rate))
}

fn parse_cli_u64(name: &str, value: &str) -> Result<u64, String> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| format!("invalid {name}: {value}"))?;
    if parsed == 0 || parsed > autotune::MAX_RATE_KBPS {
        return Err(format!(
            "{name} must be between 1 and {} kbit/s",
            autotune::MAX_RATE_KBPS
        ));
    }
    Ok(parsed)
}

fn parse_cli_f64(name: &str, value: &str) -> Result<f64, String> {
    let parsed = value
        .parse::<f64>()
        .map_err(|_| format!("invalid {name}: {value}"))?;
    if !parsed.is_finite() {
        return Err(format!("{name} must be finite"));
    }
    Ok(parsed)
}

fn parse_strict_bool(name: &str, value: &str) -> Result<bool, String> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(format!("{name} must be exactly 0 or 1")),
    }
}

fn subtract_background_samples(samples: &mut [f64], background_kbps: f64) -> Result<(), String> {
    if !background_kbps.is_finite()
        || !(0.0..=autotune::MAX_RATE_KBPS as f64).contains(&background_kbps)
    {
        return Err(format!(
            "conservative background must be finite and between 0 and {} kbit/s",
            autotune::MAX_RATE_KBPS
        ));
    }
    let safety_background = background_kbps * 1.25;
    for sample in samples {
        *sample = (*sample - safety_background).max(100.0);
    }
    Ok(())
}

fn current_direction(
    minimum: Option<u64>,
    base: Option<u64>,
    maximum: Option<u64>,
    cap: Option<u64>,
    observed: &autotune::DirectionProposal,
) -> Result<autotune::DirectionProposal, String> {
    let minimum =
        minimum.ok_or_else(|| "retained direction has no confirmed minimum".to_string())?;
    let base = base.ok_or_else(|| "retained direction has no confirmed base".to_string())?;
    let maximum =
        maximum.ok_or_else(|| "retained direction has no confirmed maximum".to_string())?;
    let cap = cap.unwrap_or(maximum);
    if minimum > base || base > maximum || maximum > cap {
        return Err(
            "retained direction limits are not ordered min <= base <= max <= cap".to_string(),
        );
    }
    Ok(autotune::DirectionProposal {
        minimum_kbps: minimum,
        base_kbps: base,
        maximum_kbps: maximum,
        absolute_cap_kbps: cap,
        observed_low_kbps: observed.observed_low_kbps,
        observed_median_kbps: observed.observed_median_kbps,
        observed_high_kbps: observed.observed_high_kbps,
        variability: observed.variability,
    })
}

fn run_autotune_proposal_cli<I>(args: I) -> Result<(), String>
where
    I: Iterator<Item = String>,
{
    use autotune::{build_proposal_for_profile, AutotuneProfile, LatencyBaseline, LinkKind};

    let mut download = None;
    let mut upload = None;
    let mut idle_median_ms = None;
    let mut idle_p95_ms = None;
    let mut idle_samples = None;
    let mut base_scale = 1.0;
    let mut download_base_scale = None;
    let mut upload_base_scale = None;
    let mut link_kind = LinkKind::Unknown;
    let mut profile = AutotuneProfile::BestOverall;
    let mut conservative_background_dl_kbps = None;
    let mut conservative_background_ul_kbps = None;
    let mut retain_dl = false;
    let mut retain_ul = false;
    let mut current_dl_min = None;
    let mut current_dl_base = None;
    let mut current_dl_max = None;
    let mut current_dl_cap = None;
    let mut current_ul_min = None;
    let mut current_ul_base = None;
    let mut current_ul_max = None;
    let mut current_ul_cap = None;
    let mut args = args;

    while let Some(arg) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {arg}"))?;
        match arg.as_str() {
            "--dl-samples" => download = Some(parse_rate_samples(&value)?),
            "--ul-samples" => upload = Some(parse_rate_samples(&value)?),
            "--idle-median-ms" => idle_median_ms = Some(parse_cli_f64("idle median", &value)?),
            "--idle-p95-ms" => idle_p95_ms = Some(parse_cli_f64("idle p95", &value)?),
            "--idle-samples" => {
                idle_samples = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| "invalid idle sample count".to_string())?,
                )
            }
            "--base-scale" => base_scale = parse_cli_f64("base-rate scale", &value)?,
            "--dl-base-scale" => {
                download_base_scale = Some(parse_cli_f64("download base-rate scale", &value)?)
            }
            "--ul-base-scale" => {
                upload_base_scale = Some(parse_cli_f64("upload base-rate scale", &value)?)
            }
            "--conservative-background-dl-kbps" => {
                conservative_background_dl_kbps =
                    Some(parse_cli_f64("conservative download background", &value)?)
            }
            "--conservative-background-ul-kbps" => {
                conservative_background_ul_kbps =
                    Some(parse_cli_f64("conservative upload background", &value)?)
            }
            "--retain-dl" => retain_dl = parse_strict_bool("retain-dl", &value)?,
            "--retain-ul" => retain_ul = parse_strict_bool("retain-ul", &value)?,
            "--current-dl-min-kbps" => current_dl_min = parse_optional_rate(&value)?,
            "--current-dl-base-kbps" => current_dl_base = parse_optional_rate(&value)?,
            "--current-dl-max-kbps" => current_dl_max = parse_optional_rate(&value)?,
            "--current-dl-cap-kbps" => current_dl_cap = parse_optional_rate(&value)?,
            "--current-ul-min-kbps" => current_ul_min = parse_optional_rate(&value)?,
            "--current-ul-base-kbps" => current_ul_base = parse_optional_rate(&value)?,
            "--current-ul-max-kbps" => current_ul_max = parse_optional_rate(&value)?,
            "--current-ul-cap-kbps" => current_ul_cap = parse_optional_rate(&value)?,
            "--link-kind" => {
                link_kind = LinkKind::parse(&value)
                    .ok_or_else(|| format!("unsupported link kind: {value}"))?
            }
            "--profile" => {
                profile = AutotuneProfile::parse(&value)
                    .ok_or_else(|| format!("unsupported autotune profile: {value}"))?
            }
            _ => return Err(format!("unsupported autotune option: {arg}")),
        }
    }

    let mut download = download.ok_or_else(|| "--dl-samples is required".to_string())?;
    let mut upload = upload.ok_or_else(|| "--ul-samples is required".to_string())?;
    let conservative =
        conservative_background_dl_kbps.is_some() || conservative_background_ul_kbps.is_some();
    if conservative {
        subtract_background_samples(
            &mut download,
            conservative_background_dl_kbps.unwrap_or(0.0),
        )?;
        subtract_background_samples(&mut upload, conservative_background_ul_kbps.unwrap_or(0.0))?;
    }

    let mut proposal = build_proposal_for_profile(
        &download,
        &upload,
        LatencyBaseline {
            median_ms: idle_median_ms.ok_or_else(|| "--idle-median-ms is required".to_string())?,
            p95_ms: idle_p95_ms.ok_or_else(|| "--idle-p95-ms is required".to_string())?,
            samples: idle_samples.ok_or_else(|| "--idle-samples is required".to_string())?,
        },
        link_kind,
        profile,
    )?;
    if download_base_scale.is_none() && upload_base_scale.is_none() {
        proposal.revise_base_rates(base_scale)?;
    } else {
        proposal.revise_base_rates_by_direction(
            download_base_scale.unwrap_or(base_scale),
            upload_base_scale.unwrap_or(base_scale),
        )?;
    }
    if conservative {
        let retained_download = if retain_dl {
            Some(current_direction(
                current_dl_min,
                current_dl_base,
                current_dl_max,
                current_dl_cap,
                &proposal.download,
            )?)
        } else {
            None
        };
        let retained_upload = if retain_ul {
            Some(current_direction(
                current_ul_min,
                current_ul_base,
                current_ul_max,
                current_ul_cap,
                &proposal.upload,
            )?)
        } else {
            None
        };
        proposal.apply_conservative_constraints(
            retained_download,
            retained_upload,
            current_dl_max,
            current_dl_cap,
            current_ul_max,
            current_ul_cap,
        );
    }
    println!("{}", proposal.to_json());
    Ok(())
}

fn run_autotune_validation_cli<I>(args: I) -> Result<(), String>
where
    I: Iterator<Item = String>,
{
    use autotune::{
        validate_shaped_candidate, AutotuneProfile, DirectionLoadInput, DirectionValidationInput,
        ValidationInput, ValidationThresholds,
    };

    let mut dl_observed_low = None;
    let mut ul_observed_low = None;
    let mut dl_candidate = None;
    let mut ul_candidate = None;
    let mut dl_achieved = None;
    let mut ul_achieved = None;
    let mut dl_minimum = None;
    let mut ul_minimum = None;
    let mut dl_maximum = None;
    let mut ul_maximum = None;
    let mut icmp_delta_ms = None;
    let mut transport_delta_ms = None;
    let mut loss_percent = None;
    let mut cpu_percent = None;
    let mut dl_icmp_delta_ms = None;
    let mut ul_icmp_delta_ms = None;
    let mut dl_transport_delta_ms = None;
    let mut ul_transport_delta_ms = None;
    let mut dl_loss_percent = None;
    let mut ul_loss_percent = None;
    let mut dl_cpu_percent = None;
    let mut ul_cpu_percent = None;
    let mut profile = AutotuneProfile::BestOverall;
    let mut thresholds = ValidationThresholds::default();
    let mut args = args;

    while let Some(arg) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {arg}"))?;
        match arg.as_str() {
            "--profile" => {
                profile = AutotuneProfile::parse(&value)
                    .ok_or_else(|| format!("unsupported Auto-Tune profile: {value}"))?
            }
            "--dl-observed-low-kbps" => {
                dl_observed_low = Some(parse_cli_u64("download observed-low rate", &value)?)
            }
            "--ul-observed-low-kbps" => {
                ul_observed_low = Some(parse_cli_u64("upload observed-low rate", &value)?)
            }
            "--dl-candidate-kbps" => {
                dl_candidate = Some(parse_cli_u64("download candidate rate", &value)?)
            }
            "--ul-candidate-kbps" => {
                ul_candidate = Some(parse_cli_u64("upload candidate rate", &value)?)
            }
            "--dl-achieved-kbps" => {
                dl_achieved = Some(parse_cli_u64("download achieved rate", &value)?)
            }
            "--ul-achieved-kbps" => {
                ul_achieved = Some(parse_cli_u64("upload achieved rate", &value)?)
            }
            "--dl-min-kbps" => dl_minimum = Some(parse_cli_u64("download minimum rate", &value)?),
            "--ul-min-kbps" => ul_minimum = Some(parse_cli_u64("upload minimum rate", &value)?),
            "--dl-max-kbps" => dl_maximum = Some(parse_cli_u64("download maximum rate", &value)?),
            "--ul-max-kbps" => ul_maximum = Some(parse_cli_u64("upload maximum rate", &value)?),
            "--icmp-delta-ms" => {
                icmp_delta_ms = Some(parse_cli_f64("ICMP same-quantile delta", &value)?)
            }
            "--transport-delta-ms" => {
                transport_delta_ms = Some(parse_cli_f64("transport same-quantile delta", &value)?)
            }
            "--loss-percent" => loss_percent = Some(parse_cli_f64("packet loss percent", &value)?),
            "--cpu-percent" => cpu_percent = Some(parse_cli_f64("CPU percent", &value)?),
            "--dl-icmp-delta-ms" => {
                dl_icmp_delta_ms = Some(parse_cli_f64("download ICMP same-quantile delta", &value)?)
            }
            "--ul-icmp-delta-ms" => {
                ul_icmp_delta_ms = Some(parse_cli_f64("upload ICMP same-quantile delta", &value)?)
            }
            "--dl-transport-delta-ms" => {
                dl_transport_delta_ms = Some(parse_cli_f64(
                    "download transport same-quantile delta",
                    &value,
                )?)
            }
            "--ul-transport-delta-ms" => {
                ul_transport_delta_ms = Some(parse_cli_f64(
                    "upload transport same-quantile delta",
                    &value,
                )?)
            }
            "--dl-loss-percent" => {
                dl_loss_percent = Some(parse_cli_f64("download packet loss percent", &value)?)
            }
            "--ul-loss-percent" => {
                ul_loss_percent = Some(parse_cli_f64("upload packet loss percent", &value)?)
            }
            "--dl-cpu-percent" => {
                dl_cpu_percent = Some(parse_cli_f64("download CPU percent", &value)?)
            }
            "--ul-cpu-percent" => {
                ul_cpu_percent = Some(parse_cli_f64("upload CPU percent", &value)?)
            }
            "--candidate-realization-min-percent" => {
                thresholds.candidate_realization_min_percent =
                    parse_cli_f64("candidate realization minimum", &value)?
            }
            "--candidate-realization-max-percent" => {
                thresholds.candidate_realization_max_percent =
                    parse_cli_f64("candidate realization maximum", &value)?
            }
            "--capacity-retention-min-percent" => {
                thresholds.capacity_retention_min_percent =
                    parse_cli_f64("capacity retention minimum", &value)?
            }
            "--icmp-delta-max-ms" => {
                thresholds.icmp_delta_max_ms = parse_cli_f64("ICMP delta maximum", &value)?
            }
            "--transport-delta-max-ms" => {
                thresholds.transport_delta_max_ms =
                    parse_cli_f64("transport delta maximum", &value)?
            }
            "--loss-max-percent" => {
                thresholds.loss_max_percent = parse_cli_f64("packet loss maximum", &value)?
            }
            "--cpu-max-percent" => {
                thresholds.cpu_max_percent = parse_cli_f64("CPU maximum", &value)?
            }
            _ => return Err(format!("unsupported autotune validation option: {arg}")),
        }
    }

    let required_rate =
        |value: Option<u64>, name: &str| value.ok_or_else(|| format!("--{name} is required"));
    let required_metric =
        |value: Option<f64>, name: &str| value.ok_or_else(|| format!("--{name} is required"));
    let directional_metric = |specific: Option<f64>, shared: Option<f64>, name: &str| {
        required_metric(specific.or(shared), name)
    };
    let result = validate_shaped_candidate(ValidationInput {
        profile,
        download: DirectionValidationInput {
            observed_low_kbps: required_rate(dl_observed_low, "dl-observed-low-kbps")?,
            candidate_kbps: required_rate(dl_candidate, "dl-candidate-kbps")?,
            achieved_kbps: required_rate(dl_achieved, "dl-achieved-kbps")?,
            minimum_kbps: required_rate(dl_minimum, "dl-min-kbps")?,
            maximum_kbps: required_rate(dl_maximum, "dl-max-kbps")?,
        },
        upload: DirectionValidationInput {
            observed_low_kbps: required_rate(ul_observed_low, "ul-observed-low-kbps")?,
            candidate_kbps: required_rate(ul_candidate, "ul-candidate-kbps")?,
            achieved_kbps: required_rate(ul_achieved, "ul-achieved-kbps")?,
            minimum_kbps: required_rate(ul_minimum, "ul-min-kbps")?,
            maximum_kbps: required_rate(ul_maximum, "ul-max-kbps")?,
        },
        download_load: DirectionLoadInput {
            icmp_delta_ms: directional_metric(dl_icmp_delta_ms, icmp_delta_ms, "dl-icmp-delta-ms")?,
            transport_delta_ms: directional_metric(
                dl_transport_delta_ms,
                transport_delta_ms,
                "dl-transport-delta-ms",
            )?,
            loss_percent: directional_metric(dl_loss_percent, loss_percent, "dl-loss-percent")?,
            cpu_percent: directional_metric(dl_cpu_percent, cpu_percent, "dl-cpu-percent")?,
        },
        upload_load: DirectionLoadInput {
            icmp_delta_ms: directional_metric(ul_icmp_delta_ms, icmp_delta_ms, "ul-icmp-delta-ms")?,
            transport_delta_ms: directional_metric(
                ul_transport_delta_ms,
                transport_delta_ms,
                "ul-transport-delta-ms",
            )?,
            loss_percent: directional_metric(ul_loss_percent, loss_percent, "ul-loss-percent")?,
            cpu_percent: directional_metric(ul_cpu_percent, cpu_percent, "ul-cpu-percent")?,
        },
        thresholds,
    })?;
    println!("{}", result.to_json());
    Ok(())
}

fn run_transport_probe_cli<I>(args: I) -> Result<(), String>
where
    I: Iterator<Item = String>,
{
    let mut backend = TransportProbeBackend::WebSocket;
    let mut endpoint = None;
    let mut binding = RouteBinding::default();
    let mut count = 5usize;
    let mut timeout_s = 5u64;
    let mut interval_ms = 250u64;
    let mut args = args;

    while let Some(arg) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {arg}"))?;
        match arg.as_str() {
            "--backend" => {
                backend = TransportProbeBackend::parse(&value)
                    .ok_or_else(|| format!("unsupported transport backend: {value}"))?
            }
            "--endpoint" => endpoint = Some(value),
            "--device" => binding.device = value,
            "--source-ip" => binding.source_ip = value,
            "--fwmark" => binding.fwmark = value,
            "--count" => {
                count = value
                    .parse::<usize>()
                    .map_err(|_| "invalid transport probe count".to_string())?
            }
            "--timeout" => {
                timeout_s = value
                    .parse::<u64>()
                    .map_err(|_| "invalid transport probe timeout".to_string())?
            }
            "--interval-ms" => {
                interval_ms = value
                    .parse::<u64>()
                    .map_err(|_| "invalid transport probe interval".to_string())?
            }
            _ => return Err(format!("unsupported transport probe option: {arg}")),
        }
    }
    if !(1..=100).contains(&count) {
        return Err("transport probe count must be between 1 and 100".to_string());
    }
    if !(1..=30).contains(&timeout_s) {
        return Err("transport probe timeout must be between 1 and 30 seconds".to_string());
    }
    if interval_ms > 60_000 {
        return Err("transport probe interval must not exceed 60000 ms".to_string());
    }
    let endpoint = endpoint.unwrap_or_else(|| match backend {
        TransportProbeBackend::WebSocket => "wss://ping-bufferbloat.libreqos.com/ws".to_string(),
        TransportProbeBackend::TcpConnect => "tcp://ping-bufferbloat.libreqos.com:443".to_string(),
        TransportProbeBackend::PersistentHttp => {
            "https://ping-bufferbloat.libreqos.com/ping".to_string()
        }
        TransportProbeBackend::LegacyHttp => {
            "https://speed.cloudflare.com/__down?bytes=0".to_string()
        }
    });
    let mut engine =
        TransportProbeEngine::new(backend, endpoint, binding, Duration::from_secs(timeout_s))?;
    let mut accepted = 0usize;
    let mut failed = 0usize;
    for index in 0..count {
        match engine.probe() {
            Ok(sample) => {
                accepted += 1;
                let raw = sample
                    .raw_samples_ms
                    .iter()
                    .map(|value| format!("{value:.3}"))
                    .collect::<Vec<_>>()
                    .join(",");
                println!(
                    "{{\"index\":{},\"backend\":\"{}\",\"endpoint\":\"{}\",\"rtt_ms\":{:.3},\"raw_ms\":[{}],\"discarded\":{},\"server_processing_ms\":{:.3},\"trusted\":{},\"connection_reused\":{}}}",
                    index + 1,
                    sample.backend.as_str(),
                    json_escape(&sample.endpoint),
                    sample.rtt_ms,
                    raw,
                    sample.discarded_samples,
                    sample.server_processing_ms,
                    json_bool(sample.trusted),
                    json_bool(sample.connection_reused)
                );
            }
            Err(error) => {
                failed += 1;
                println!(
                    "{{\"index\":{},\"backend\":\"{}\",\"error\":\"{}\"}}",
                    index + 1,
                    backend.as_str(),
                    json_escape(&error)
                );
            }
        }
        if index + 1 < count && interval_ms > 0 {
            thread::sleep(Duration::from_millis(interval_ms));
        }
    }
    eprintln!(
        "transport-probe backend={} accepted={} failed={} trusted={}",
        backend.as_str(),
        accepted,
        failed,
        backend.trusted()
    );
    if accepted == 0 {
        return Err("transport probe produced no accepted sample".to_string());
    }
    Ok(())
}

fn main() {
    let mut initial_args = env::args();
    let _program = initial_args.next();
    match initial_args.next().as_deref() {
        Some("--autotune-proposal") => {
            if let Err(error) = run_autotune_proposal_cli(initial_args) {
                eprintln!("ERROR: {error}");
                std::process::exit(2);
            }
            return;
        }
        Some("--autotune-validate") => {
            if let Err(error) = run_autotune_validation_cli(initial_args) {
                eprintln!("ERROR: {error}");
                std::process::exit(2);
            }
            return;
        }
        Some("--transport-probe") => {
            if let Err(error) = run_transport_probe_cli(initial_args) {
                eprintln!("ERROR: {error}");
                std::process::exit(2);
            }
            return;
        }
        _ => {}
    }

    unsafe {
        signal(2, handle_signal);
        signal(15, handle_signal);
    }

    let mut instance = "primary".to_string();
    let mut once = false;
    let mut dump_config = false;
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--instance" => {
                let Some(value) = args.next() else {
                    print_usage();
                    std::process::exit(2);
                };
                instance = value;
            }
            "--once" => once = true,
            "--dump-config" => dump_config = true,
            "-h" | "--help" => {
                print_usage();
                return;
            }
            _ => {
                print_usage();
                std::process::exit(2);
            }
        }
    }

    let cfg = match Config::from_uci(&instance) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(1);
        }
    };

    if dump_config {
        println!("{:#?}", cfg);
        return;
    }

    if let Err(e) = run(cfg, once) {
        eprintln!("ERROR: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        autotune, compact_graph_history_data, compact_graph_history_file, compute_history_budget,
        default_reflectors, graph_history_line, history_safe_max_kib, ingress_output_targets_ifb,
        irtt_target_arg, max_wire_packet_size_bits_from_mtu, monitor_tick_timeout,
        next_spare_reflector, packet_compensation_us, parse_cli_f64, parse_fping_line,
        parse_fping_ts_line, parse_irtt_duration_us, parse_irtt_line, parse_rate_samples,
        parse_reflector_candidates, parse_strict_bool, parse_tc_linklayer_overhead,
        parse_tsping_line, parse_uci_values, pinger_command, pinger_response_interval_s,
        qdisc_output_has_cake, reflector_bad_reflectors, reflector_health_json,
        reflector_spare_reflectors, sample_is_stale, shaper_update_due, stall_detection_timeout,
        status_publish_due, subtract_background_samples, throughput_floor, transport_error_code,
        transport_probe_interval_s, transport_result_matches_route, uplink_error_code, Config,
        MemoryInfo, RateMonitor, ReflectorHealth, ReflectorState, Sample, ThroughputGuardInput,
        UplinkState, CAKE_GROWTH_UPDATE_MIN_INTERVAL, STATUS_PUBLISH_INTERVAL,
        TRANSPORT_BASELINE_LEARNING_INTERVAL_S,
    };
    use std::fs;
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    #[test]
    fn parses_single_quoted_value() {
        assert_eq!(parse_uci_values("'eth1'"), vec!["eth1"]);
    }

    #[test]
    fn parses_uci_list_values() {
        assert_eq!(
            parse_uci_values("'1.1.1.1' '1.0.0.1' '8.8.8.8'"),
            vec!["1.1.1.1", "1.0.0.1", "8.8.8.8"]
        );
    }

    #[test]
    fn preserves_spaces_inside_quotes() {
        assert_eq!(parse_uci_values("'foo bar' baz"), vec!["foo bar", "baz"]);
    }

    #[test]
    fn parses_reflector_candidates_from_text() {
        let data = "host,notes\n# comment\n1.1.1.1,cloudflare\nbad://url\n9.9.9.9 quad9\n";
        assert_eq!(
            parse_reflector_candidates(data, 1),
            vec!["1.1.1.1", "9.9.9.9"]
        );
    }

    #[test]
    fn default_reflectors_match_upstream_pool() {
        let reflectors = default_reflectors();

        assert_eq!(reflectors.len(), 30);
        assert_eq!(reflectors.first().map(String::as_str), Some("1.1.1.1"));
        assert!(reflectors.iter().any(|reflector| reflector == "9.9.9.11"));
        assert_eq!(
            reflectors.last().map(String::as_str),
            Some("185.228.168.10")
        );
    }

    #[test]
    fn parses_openwrt_fping_success_line() {
        let line = "[1783743970.24147] 1.1.1.1 : [0], 64 bytes, 3.80 ms (3.80 avg, 0% loss)";
        let sample = parse_fping_line(line).expect("expected fping sample");

        assert_eq!(sample.reflector, "1.1.1.1");
        assert_eq!(sample.seq, "0");
        assert_eq!(sample.timestamp, 1783743970.24147);
        assert_eq!(sample.rtt_ms, 3.80);
        assert_eq!(sample.dl_owd_us, 1900.0);
        assert_eq!(sample.ul_owd_us, 1900.0);
        assert!(!sample.timestamped_owd);
    }

    #[test]
    fn ignores_fping_timeout_line() {
        let line = "[1783743970.24147] 1.1.1.1 : [0], timed out (NaN avg, 100% loss)";
        assert!(parse_fping_line(line).is_none());
    }

    #[test]
    fn parses_fping_ts_success_line() {
        let line = "[1783449038.70892] 127.0.0.1 : [0], 20 bytes, 0.080 ms (0.080 avg, 0% loss), timestamps: Originate=66638708 Receive=66638708 Transmit=66638708 Localreceive=66638709";
        let sample = parse_fping_ts_line(line).expect("expected fping-ts sample");

        assert_eq!(sample.reflector, "127.0.0.1");
        assert_eq!(sample.seq, "0");
        assert_eq!(sample.rtt_ms, 0.080);
        assert_eq!(sample.dl_owd_us, 1000.0);
        assert_eq!(sample.ul_owd_us, 0.0);
        assert!(sample.timestamped_owd);
    }

    #[test]
    fn ignores_fping_ts_timeout_line() {
        let line = "[1783449025.44098] 8.8.8.8 : [0], timed out (NaN avg, 100% loss)";
        assert!(parse_fping_ts_line(line).is_none());
    }

    #[test]
    fn parses_tsping_machine_readable_line() {
        let line = "1783449500.123456,127.0.0.1,42,0,0,0,0,0,1.25,2.75";
        let sample = parse_tsping_line(line).expect("expected tsping sample");

        assert_eq!(sample.reflector, "127.0.0.1");
        assert_eq!(sample.seq, "42");
        assert_eq!(sample.rtt_ms, 4.0);
        assert_eq!(sample.dl_owd_us, 1250.0);
        assert_eq!(sample.ul_owd_us, 2750.0);
        assert!(sample.timestamped_owd);
    }

    #[test]
    fn ignores_incomplete_tsping_line() {
        assert!(parse_tsping_line("1783449500.123456,127.0.0.1,42").is_none());
    }

    #[test]
    fn parses_irtt_client_line() {
        let line = "[0] seq=7 send=1.2ms delay=2.3ms rd=450us sd=1.25ms ipdv=20us";
        let sample = parse_irtt_line(line, "irtt.example.net").expect("expected irtt sample");

        assert_eq!(sample.reflector, "irtt.example.net");
        assert_eq!(sample.seq, "7");
        assert_eq!(sample.rtt_ms, 1.7);
        assert_eq!(sample.dl_owd_us, 450.0);
        assert_eq!(sample.ul_owd_us, 1250.0);
        assert!(sample.timestamped_owd);
    }

    #[test]
    fn parses_irtt_duration_units() {
        assert_eq!(parse_irtt_duration_us("2s"), Some(2_000_000.0));
        assert_eq!(parse_irtt_duration_us("3.5ms"), Some(3500.0));
        assert_eq!(parse_irtt_duration_us("450us"), Some(450.0));
        assert_eq!(parse_irtt_duration_us("900ns"), Some(0.9));
        assert!(parse_irtt_duration_us("-1ms").is_none());
        assert!(parse_irtt_duration_us("10m").is_none());
    }

    #[test]
    fn stale_reflector_response_guard_matches_upstream_age() {
        let mut sample = Sample {
            reflector: "1.1.1.1".to_string(),
            seq: "1".to_string(),
            timestamp: 100.0,
            rtt_ms: 1.0,
            dl_owd_us: 500.0,
            ul_owd_us: 500.0,
            timestamped_owd: false,
        };

        assert!(!sample_is_stale(&sample, 100.500));
        assert!(sample_is_stale(&sample, 100.501));
        sample.timestamp = 101.0;
        assert!(!sample_is_stale(&sample, 100.0));
    }

    #[test]
    fn formats_irtt_target_for_ipv6_only() {
        assert_eq!(irtt_target_arg("2001:db8::1"), "[2001:db8::1]");
        assert_eq!(irtt_target_arg("[2001:db8::1]:2112"), "[2001:db8::1]:2112");
        assert_eq!(
            irtt_target_arg("irtt.example.net:2112"),
            "irtt.example.net:2112"
        );
    }

    #[test]
    fn upstream_sleep_and_stall_defaults_are_loaded() {
        let cfg = Config::defaults("test".to_string());

        assert!(cfg.enable_sleep_function);
        assert_eq!(cfg.sustained_idle_sleep_thr_s, 60.0);
        assert!(!cfg.min_shaper_rates_enforcement);
        assert_eq!(cfg.stall_detection_thr, 5);
        assert_eq!(cfg.connection_stall_thr_kbps, 10.0);
        assert_eq!(cfg.global_ping_response_timeout_s, 10.0);
        assert!((pinger_response_interval_s(&cfg) - 0.05).abs() < 0.000001);
        assert!((stall_detection_timeout(&cfg).as_secs_f64() - 0.25).abs() < 0.000001);
    }

    #[test]
    fn adaptive_ceiling_defaults_preserve_upstream_hard_max() {
        let cfg = Config::defaults("test".to_string());

        assert!(!cfg.adaptive_ceiling_enabled);
        assert_eq!(
            cfg.adaptive_ceiling_dl_cap_kbps,
            cfg.max_dl_shaper_rate_kbps
        );
        assert_eq!(
            cfg.adaptive_ceiling_ul_cap_kbps,
            cfg.max_ul_shaper_rate_kbps
        );
        assert_eq!(cfg.adaptive_ceiling_hold_time_s, 20.0);
        assert_eq!(cfg.adaptive_ceiling_growth_percent, 3.0);
        assert_eq!(cfg.adaptive_ceiling_probe_duration_s, 8.0);
        assert_eq!(cfg.adaptive_ceiling_cooldown_s, 30.0);
        assert_eq!(cfg.adaptive_ceiling_failed_bound_ttl_s, 900.0);
    }

    #[test]
    fn transport_quality_defaults_are_safe_and_opt_in() {
        let cfg = Config::defaults("test".to_string());
        assert!(!cfg.transport_latency_enabled);
        assert!(!cfg.transport_controller_enabled);
        assert_eq!(cfg.transport_probe_backend, "websocket");
        assert!(cfg.throughput_guard_enabled);
        assert_eq!(cfg.throughput_guard_retention_percent, 80.0);
        assert_eq!(cfg.quality_target_delay_ms, 30.0);

        let dl_floor = throughput_floor(ThroughputGuardInput {
            enabled: true,
            configured_min_kbps: cfg.min_dl_shaper_rate_kbps,
            configured_base_kbps: cfg.base_dl_shaper_rate_kbps,
            observed_p20_kbps: 0.0,
            observed_p50_kbps: 0.0,
            absolute_floor_kbps: 0.0,
            retention_percent: cfg.throughput_guard_retention_percent,
        });
        assert_eq!(dl_floor, cfg.base_dl_shaper_rate_kbps * 0.60);
    }

    #[test]
    fn transport_baseline_learning_uses_a_short_temporary_interval() {
        let cfg = Config::defaults("test".to_string());

        assert_eq!(
            transport_probe_interval_s(&cfg, false, false),
            TRANSPORT_BASELINE_LEARNING_INTERVAL_S
        );
        assert_eq!(
            transport_probe_interval_s(&cfg, false, true),
            cfg.transport_probe_idle_interval_s
        );
        assert_eq!(
            transport_probe_interval_s(&cfg, true, false),
            cfg.transport_probe_loaded_interval_s
        );
    }

    #[test]
    fn transport_quality_validation_rejects_unsafe_values() {
        let mut cfg = Config::defaults("test".to_string());
        cfg.transport_latency_enabled = true;
        cfg.transport_probe_endpoint = "https://wrong-scheme.example/".to_string();
        assert!(cfg.validate().is_err());

        cfg.transport_probe_endpoint = "wss://ping-bufferbloat.libreqos.com/ws".to_string();
        cfg.quality_search_max_steps = 0;
        assert!(cfg.validate().is_err());

        cfg.quality_search_max_steps = 3;
        cfg.throughput_guard_retention_percent = 49.0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn parses_live_cake_linklayer_overhead() {
        let noatm = "qdisc cake 8001: root refcnt 2 bandwidth 10Mbit diffserv3 noatm overhead 44";
        let atm = "qdisc cake 8002: root refcnt 2 bandwidth 2Mbit besteffort atm overhead 18";

        assert_eq!(parse_tc_linklayer_overhead(noatm), (false, 44));
        assert_eq!(parse_tc_linklayer_overhead(atm), (true, 18));
        assert_eq!(
            parse_tc_linklayer_overhead("qdisc fq_codel 0: root"),
            (false, 0)
        );
    }

    #[test]
    fn wire_packet_compensation_matches_upstream_units() {
        assert_eq!(max_wire_packet_size_bits_from_mtu(1500, 44, false), 12_352);
        assert_eq!(max_wire_packet_size_bits_from_mtu(1500, 44, true), 13_992);
        assert_eq!(packet_compensation_us(12_000, 1_000.0), 12_000.0);
    }

    #[test]
    fn monitor_tick_timeout_is_compensated_at_low_rates() {
        let mut cfg = Config::defaults("test".to_string());
        cfg.monitor_achieved_rates_interval_ms = 100;
        cfg.min_dl_shaper_rate_kbps = 100.0;
        cfg.min_ul_shaper_rate_kbps = 100.0;
        cfg.dl_max_wire_packet_size_bits = 12_000;
        cfg.ul_max_wire_packet_size_bits = 12_000;

        assert_eq!(monitor_tick_timeout(&cfg).as_micros(), 2_400_000);
    }

    #[test]
    fn upstream_logging_defaults_are_loaded() {
        let cfg = Config::defaults("test".to_string());

        assert!(!cfg.output_processing_stats);
        assert!(!cfg.output_load_stats);
        assert!(!cfg.output_reflector_stats);
        assert!(!cfg.output_summary_stats);
        assert!(!cfg.output_cake_changes);
        assert!(!cfg.output_cpu_stats);
        assert!(!cfg.output_cpu_raw_stats);
        assert!(cfg.debug);
        assert!(!cfg.log_debug_messages_to_syslog);
        assert!(cfg.log_to_file);
        assert_eq!(cfg.log_file_max_time_mins, 10);
        assert_eq!(cfg.log_file_max_size_kb, 2000);
        assert_eq!(cfg.log_file_buffer_size_b, 512);
        assert_eq!(cfg.log_file_buffer_timeout_ms, 500);
        assert!(cfg.log_file_export_compress);
    }

    #[test]
    fn graph_history_is_opt_in_and_compaction_keeps_newest_samples() {
        let mut cfg = Config::defaults("test".to_string());
        assert!(!cfg.graph_history_enabled);
        assert_eq!(cfg.graph_history_interval_s, 10);

        cfg.graph_history_interval_s = 1;
        assert!(cfg.validate().is_ok());
        cfg.graph_history_interval_s = 60;
        assert!(cfg.validate().is_ok());
        cfg.graph_history_interval_s = 0;
        assert!(cfg.validate().is_err());
        cfg.graph_history_interval_s = 61;
        assert!(cfg.validate().is_err());

        assert_eq!(
            graph_history_line(
                123.4,
                Some(1.23456),
                Some(2.34),
                1000.04,
                50.54,
                Some(10.1234),
                Some(11.9876),
                Some(600.0),
                Some(30.0),
                "ACTIVE",
                "mwan3|wan|pppoe-wan|84.1.1.1|0x100|1",
                Some("A+"),
                "final",
                Some(1.25),
                "DL",
                20,
                7,
            ),
            "123,1.235,2.3,1000.0,50.5,10.123,11.988,600.0,30.0,ACTIVE,mwan3|wan|pppoe-wan|84.1.1.1|0x100|1,A+,final,1.250,DL,20,7\n"
        );

        let data = "1,1,1\n2,2,2\n3,3,3\n";
        assert_eq!(compact_graph_history_data(data, 12), "2,2,2\n3,3,3\n");

        let path = std::env::temp_dir().join(format!(
            "cake-autorate-history-{}-{}.csv",
            std::process::id(),
            super::epoch_secs()
        ));
        fs::write(&path, data).unwrap();
        assert_eq!(compact_graph_history_file(&path, 12).unwrap(), 2);
        assert_eq!(fs::read_to_string(&path).unwrap(), "2,2,2\n3,3,3\n");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn graph_history_budget_scales_with_available_ram_and_never_breaks_reserve() {
        assert_eq!(history_safe_max_kib(100 * 1024), 1024);
        assert_eq!(history_safe_max_kib(1024 * 1024), 100 * 1024);

        let small = compute_history_budget(
            Some(100 * 1024),
            2,
            MemoryInfo {
                total_kib: 128 * 1024,
                available_kib: 100 * 1024,
            },
            0,
            0,
        );
        assert_eq!(small.safe_max_kib, 1024);
        assert_eq!(small.effective_total_kib, 1024);
        assert_eq!(small.instance_budget_kib, 512);

        let large = compute_history_budget(
            Some(100 * 1024),
            1,
            MemoryInfo {
                total_kib: 2 * 1024 * 1024,
                available_kib: 1024 * 1024,
            },
            0,
            0,
        );
        assert_eq!(large.safe_max_kib, 100 * 1024);
        assert_eq!(large.effective_total_kib, 100 * 1024);

        let critical = compute_history_budget(
            None,
            1,
            MemoryInfo {
                total_kib: 128 * 1024,
                available_kib: 15 * 1024,
            },
            1024,
            1024,
        );
        assert!(critical.paused_low_memory);
        assert_eq!(critical.effective_total_kib, 0);
    }

    #[test]
    fn rejects_active_threshold_above_minimum_rates() {
        let mut cfg = Config::defaults("test".to_string());
        cfg.connection_active_thr_kbps = 6000.0;

        let err = cfg.validate().expect_err("expected active threshold guard");
        assert!(err.contains("connection_active_thr_kbps"));
    }

    #[test]
    fn adaptive_ceiling_validation_is_strict_only_when_enabled() {
        let mut cfg = Config::defaults("test".to_string());
        cfg.adaptive_ceiling_dl_cap_kbps = 1.0;
        cfg.adaptive_ceiling_growth_percent = 99.0;
        assert!(cfg.validate().is_ok());

        cfg.adaptive_ceiling_enabled = true;
        let err = cfg.validate().expect_err("expected adaptive DL cap guard");
        assert!(err.contains("adaptive_ceiling_dl_cap_kbps"));

        cfg.adaptive_ceiling_dl_cap_kbps = cfg.max_dl_shaper_rate_kbps;
        cfg.adaptive_ceiling_growth_percent = 1.0;
        assert!(cfg.validate().is_ok());

        cfg.adaptive_ceiling_probe_duration_s = 0.0;
        assert!(cfg
            .validate()
            .expect_err("expected probe duration guard")
            .contains("adaptive_ceiling_probe_duration_s"));
        cfg.adaptive_ceiling_probe_duration_s = 8.0;

        cfg.adaptive_ceiling_cooldown_s = -1.0;
        assert!(cfg
            .validate()
            .expect_err("expected cooldown guard")
            .contains("adaptive_ceiling_cooldown_s"));
        cfg.adaptive_ceiling_cooldown_s = 30.0;

        cfg.adaptive_ceiling_failed_bound_ttl_s = 0.0;
        assert!(cfg
            .validate()
            .expect_err("expected failed-bound TTL guard")
            .contains("adaptive_ceiling_failed_bound_ttl_s"));
    }

    #[test]
    fn ping_fallback_allows_multiple_active_reflectors_like_upstream() {
        let mut cfg = Config::defaults("test".to_string());
        cfg.pinger_method = "ping".to_string();
        cfg.no_pingers = 2;
        cfg.reflectors = vec!["1.1.1.1".to_string(), "8.8.8.8".to_string()];

        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn prefixes_pinger_commands_without_shell() {
        let mut cfg = Config::defaults("test".to_string());
        cfg.ping_prefix_string = "mwan3 use gpon exec".to_string();

        let cmd = pinger_command(&cfg, "fping").expect("expected prefixed command");
        let args: Vec<String> = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();

        assert_eq!(cmd.get_program().to_string_lossy(), "mwan3");
        assert_eq!(args, vec!["use", "gpon", "exec", "fping"]);
    }

    #[test]
    fn rejects_unsafe_pinger_prefix_tokens() {
        let mut cfg = Config::defaults("test".to_string());
        cfg.ping_prefix_string = "mwan3 use wan; reboot".to_string();

        assert!(pinger_command(&cfg, "fping").is_err());
    }

    #[test]
    fn transport_samples_cannot_cross_uplink_route_identities() {
        assert!(transport_result_matches_route(
            Some("mwan3|wan|pppoe-wan|84.1.1.1|0x100|1"),
            Some("mwan3|wan|pppoe-wan|84.1.1.1|0x100|1")
        ));
        assert!(!transport_result_matches_route(
            Some("mwan3|wan|pppoe-wan|84.1.1.1|0x100|1"),
            Some("mwan3|wanb|eth0|10.0.100.101|0x200|2")
        ));
        assert!(!transport_result_matches_route(None, Some("main|||")));
    }

    #[test]
    fn route_and_transport_failures_have_stable_codes() {
        assert_eq!(
            uplink_error_code(UplinkState::Offline, "route mismatch: expected eth0"),
            Some("route_mismatch")
        );
        assert_eq!(
            uplink_error_code(UplinkState::Offline, "member wanb is offline"),
            Some("member_offline")
        );
        assert_eq!(uplink_error_code(UplinkState::Active, ""), None);
        assert_eq!(
            transport_error_code(Some("transport probe timed out")),
            Some("transport_timeout")
        );
        assert_eq!(
            transport_error_code(Some("route changed during transport probe")),
            Some("route_mismatch")
        );
    }

    #[test]
    fn finds_next_spare_reflector_from_rotating_index() {
        let candidates = vec![
            "1.1.1.1".to_string(),
            "1.0.0.1".to_string(),
            "8.8.8.8".to_string(),
            "9.9.9.9".to_string(),
        ];
        let active = vec!["1.1.1.1".to_string(), "8.8.8.8".to_string()];

        assert_eq!(
            next_spare_reflector(&candidates, &active, 2),
            Some((3, "9.9.9.9".to_string()))
        );
        assert_eq!(
            next_spare_reflector(&candidates, &active, 4),
            Some((1, "1.0.0.1".to_string()))
        );
    }

    #[test]
    fn tracks_reflector_offences_as_rolling_window() {
        let mut state = ReflectorState::new(Instant::now(), 3);

        state.push_offence(true);
        state.push_offence(false);
        state.push_offence(true);
        assert_eq!(state.offence_sum, 2);

        state.push_offence(false);
        assert_eq!(state.offence_sum, 1);
    }

    #[test]
    fn reports_runtime_reflector_sets() {
        let mut cfg = Config::defaults("test".to_string());
        cfg.reflectors = vec![
            "1.1.1.1".to_string(),
            "1.0.0.1".to_string(),
            "8.8.8.8".to_string(),
        ];
        cfg.reflector_misbehaving_detection_thr = 2;
        let active = vec!["1.1.1.1".to_string(), "1.0.0.1".to_string()];
        let mut health = ReflectorHealth::new(&cfg, &active);
        let state = health.states.get_mut("1.0.0.1").unwrap();
        state.samples = 3;
        state.last_rtt_ms = 12.5;
        state.offence_sum = 2;

        assert_eq!(
            reflector_spare_reflectors(&cfg, &active),
            vec!["8.8.8.8".to_string()]
        );
        assert_eq!(
            reflector_bad_reflectors(&cfg, Some(&health)),
            vec!["1.0.0.1".to_string()]
        );

        let json = reflector_health_json(&cfg, &active, Some(&health));
        assert!(json.contains("\"host\":\"1.0.0.1\""));
        assert!(json.contains("\"active\":true"));
        assert!(json.contains("\"bad\":true"));
        assert!(json.contains("\"spare\":true"));
        assert!(json.contains("\"last_rtt_ms\":12.500"));
    }

    #[test]
    fn recognizes_cake_and_exact_ifb_redirects() {
        assert!(qdisc_output_has_cake(
            "qdisc cake 8001: root bandwidth 100Mbit\nqdisc ingress ffff: parent ffff:fff1"
        ));
        assert!(qdisc_output_has_cake(
            "qdisc cake_mq 8002: root bandwidth 1Gbit"
        ));
        assert!(!qdisc_output_has_cake(
            "qdisc mq 0: root\nqdisc fq_codel 0: parent :1"
        ));

        let redirect = "action order 1: mirred (Egress Redirect to device ifb4eth0)";
        assert!(ingress_output_targets_ifb(redirect, "ifb4eth0"));
        assert!(!ingress_output_targets_ifb(redirect, "ifb4eth1"));
    }

    #[test]
    fn rate_monitor_coalesces_sub_interval_counter_bursts() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cake-autorate-rate-monitor-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let rx = root.join("rx_bytes");
        let tx = root.join("tx_bytes");
        fs::write(&rx, "0\n").unwrap();
        fs::write(&tx, "0\n").unwrap();
        let mut monitor =
            RateMonitor::new(rx.to_str().unwrap(), tx.to_str().unwrap(), 200).unwrap();

        fs::write(&rx, "1000000\n").unwrap();
        fs::write(&tx, "500000\n").unwrap();
        monitor.last = Instant::now();
        let stale = monitor.sample();
        assert!(!stale.fresh);
        assert_eq!((stale.dl_kbps, stale.ul_kbps), (0.0, 0.0));

        thread::sleep(Duration::from_millis(210));
        let fresh = monitor.sample();
        assert!(fresh.fresh);
        let dl = fresh.dl_kbps;
        let ul = fresh.ul_kbps;
        assert!(dl > 30_000.0, "download delta was not retained: {dl}");
        assert!(ul > 15_000.0, "upload delta was not retained: {ul}");
        assert!((dl / ul - 2.0).abs() < 0.01);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cake_growth_is_coalesced_but_reductions_are_immediate() {
        assert!(!shaper_update_due(
            800_000,
            804_000,
            Duration::from_millis(50)
        ));
        assert!(shaper_update_due(
            800_000,
            804_000,
            CAKE_GROWTH_UPDATE_MIN_INTERVAL
        ));
        assert!(shaper_update_due(
            800_000,
            700_000,
            Duration::from_millis(0)
        ));
        assert!(shaper_update_due(0, 800_000, Duration::from_millis(0)));
    }

    #[test]
    fn status_publication_is_bounded_independently_of_control_samples() {
        assert!(!status_publish_due(Duration::from_millis(249)));
        assert!(status_publish_due(STATUS_PUBLISH_INTERVAL));
    }

    #[test]
    fn autotune_cli_rate_lists_are_strict_and_bounded() {
        for invalid in [
            "",
            " ",
            ",",
            "1,",
            ",1",
            "1,,2",
            "0",
            "-1",
            "NaN",
            "inf",
            "100000001",
        ] {
            assert!(
                parse_rate_samples(invalid).is_err(),
                "accepted invalid sample list {invalid:?}"
            );
        }
        assert_eq!(
            parse_rate_samples("0.1, 100000000").unwrap(),
            vec![0.1, 100_000_000.0]
        );
        assert!(parse_rate_samples(
            &std::iter::repeat_n("1", autotune::MAX_THROUGHPUT_SAMPLES + 1)
                .collect::<Vec<_>>()
                .join(",")
        )
        .is_err());
    }

    #[test]
    fn autotune_cli_booleans_floats_and_background_are_fail_closed() {
        assert_eq!(parse_strict_bool("retain", "0"), Ok(false));
        assert_eq!(parse_strict_bool("retain", "1"), Ok(true));
        for invalid in ["", "true", "false", "2", "-1"] {
            assert!(parse_strict_bool("retain", invalid).is_err());
        }
        for invalid in ["NaN", "inf", "-inf"] {
            assert!(parse_cli_f64("metric", invalid).is_err());
        }
        let mut samples = [1_000.0];
        for invalid in [
            f64::NAN,
            f64::INFINITY,
            -1.0,
            autotune::MAX_RATE_KBPS as f64 + 1.0,
        ] {
            assert!(subtract_background_samples(&mut samples, invalid).is_err());
        }
    }

    #[test]
    fn config_rejects_plaintext_persistent_http() {
        let mut cfg = Config::defaults("test".to_string());
        cfg.transport_latency_enabled = true;
        cfg.transport_probe_backend = "persistent-http".to_string();
        cfg.transport_probe_endpoint = "http://example.invalid/ping".to_string();
        assert!(cfg.validate().is_err());
        cfg.transport_probe_endpoint = "https://example.invalid/ping".to_string();
        assert!(cfg.validate().is_ok());
    }
}
