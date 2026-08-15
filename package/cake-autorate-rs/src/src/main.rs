use std::collections::{HashMap, VecDeque};
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, OwnedFd};
#[cfg(feature = "calibration")]
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod adaptive_ceiling;
#[cfg_attr(not(feature = "calibration"), allow(dead_code))]
mod autotune;
#[allow(dead_code)]
mod operations;
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
const STATUS_PUBLISH_INTERVAL: Duration = Duration::from_millis(250);
const CAKE_GROWTH_UPDATE_MIN_INTERVAL: Duration = Duration::from_millis(100);
const TRANSPORT_BASELINE_LEARNING_INTERVAL_S: f64 = 1.0;
#[cfg(feature = "calibration")]
const AUTOTUNE_TRANSPORT_MIN_LOADED_COVERAGE_PERCENT: u128 = 70;
#[cfg(feature = "calibration")]
const AUTOTUNE_CAPTURE_REATTEST_INTERVAL: Duration = Duration::from_secs(1);
#[cfg(feature = "calibration")]
const AUTOTUNE_CAPTURE_ATTESTATION_MAX_AGE: Duration = Duration::from_secs(3);
const SQM_RUNTIME_STOP_TIMEOUT: Duration = Duration::from_secs(30);
const SQM_RUNTIME_STOP_OUTPUT_LIMIT: usize = 8 * 1024;

extern "C" fn handle_signal(_: i32) {
    TERMINATE.store(true, Ordering::SeqCst);
    operations::event_loop::wake_from_signal();
}

fn install_signal_handlers() {
    unsafe {
        signal(2, handle_signal);
        signal(15, handle_signal);
    }
}

extern "C" {
    fn signal(signum: i32, handler: extern "C" fn(i32)) -> extern "C" fn(i32);
    fn kill(pid: i32, signal: i32) -> i32;
}

#[derive(Clone, Debug)]
struct Config {
    instance: String,
    enabled: bool,
    manage_sqm: bool,
    sqm_enabled: bool,
    sqm_direction_mode: String,
    sqm_section: String,
    sqm_interface: String,
    dl_if: String,
    ul_if: String,
    route_mode: String,
    mwan3_member: String,
    speedtest_backend: String,
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
    adaptive_ceiling_dl_safe_kbps: f64,
    adaptive_ceiling_ul_safe_kbps: f64,
    adaptive_ceiling_dl_evidence: String,
    adaptive_ceiling_ul_evidence: String,
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
    rating_capture_quiet_s: usize,
    rating_capture_quiet_timeout_s: usize,
    rating_capture_quiet_ratio: f64,
    rating_capture_quiet_min_kbps: f64,
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
        let sqm_section = format!("cake_{instance}");
        Self {
            instance,
            enabled: false,
            manage_sqm: true,
            sqm_enabled: false,
            sqm_direction_mode: "both".to_string(),
            sqm_section,
            sqm_interface: String::new(),
            dl_if: "ifb-wan".to_string(),
            ul_if: "wan".to_string(),
            route_mode: "auto".to_string(),
            mwan3_member: String::new(),
            speedtest_backend: "auto".to_string(),
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
            adaptive_ceiling_dl_safe_kbps: 0.0,
            adaptive_ceiling_ul_safe_kbps: 0.0,
            adaptive_ceiling_dl_evidence: "legacy_unverified".to_string(),
            adaptive_ceiling_ul_evidence: "legacy_unverified".to_string(),
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
            rating_capture_quiet_s: 5,
            rating_capture_quiet_timeout_s: 30,
            rating_capture_quiet_ratio: 0.05,
            rating_capture_quiet_min_kbps: 1000.0,
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
        set_string(&single, "sqm_direction_mode", &mut cfg.sqm_direction_mode);
        set_string(&single, "sqm_section", &mut cfg.sqm_section);
        set_string(&single, "sqm_interface", &mut cfg.sqm_interface);
        set_string(&single, "dl_if", &mut cfg.dl_if);
        set_string(&single, "ul_if", &mut cfg.ul_if);
        set_string(&single, "route_mode", &mut cfg.route_mode);
        set_string(&single, "mwan3_member", &mut cfg.mwan3_member);
        set_string(&single, "speedtest_backend", &mut cfg.speedtest_backend);
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
        if !cfg.download_shaping_enabled() {
            cfg.adjust_dl_shaper_rate = false;
        }
        if !cfg.upload_shaping_enabled() {
            cfg.adjust_ul_shaper_rate = false;
        }
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
            "adaptive_ceiling_dl_safe_kbps",
            &mut cfg.adaptive_ceiling_dl_safe_kbps,
        )?;
        set_f64(
            &single,
            "adaptive_ceiling_ul_safe_kbps",
            &mut cfg.adaptive_ceiling_ul_safe_kbps,
        )?;
        set_string(
            &single,
            "adaptive_ceiling_dl_evidence",
            &mut cfg.adaptive_ceiling_dl_evidence,
        );
        set_string(
            &single,
            "adaptive_ceiling_ul_evidence",
            &mut cfg.adaptive_ceiling_ul_evidence,
        );
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
        set_usize(
            &single,
            "rating_capture_quiet_s",
            &mut cfg.rating_capture_quiet_s,
        )?;
        set_usize(
            &single,
            "rating_capture_quiet_timeout_s",
            &mut cfg.rating_capture_quiet_timeout_s,
        )?;
        set_f64(
            &single,
            "rating_capture_quiet_ratio",
            &mut cfg.rating_capture_quiet_ratio,
        )?;
        set_f64(
            &single,
            "rating_capture_quiet_min_kbps",
            &mut cfg.rating_capture_quiet_min_kbps,
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
            self.rx_bytes_path = if self.download_shaping_enabled() {
                format!("/sys/class/net/{}/statistics/tx_bytes", self.dl_if)
            } else {
                format!("/sys/class/net/{}/statistics/rx_bytes", self.sqm_interface)
            };
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
        self.dl_max_wire_packet_size_bits =
            interface_max_wire_packet_size_bits(if self.download_shaping_enabled() {
                &self.dl_if
            } else {
                &self.sqm_interface
            });
        self.ul_max_wire_packet_size_bits = interface_max_wire_packet_size_bits(&self.ul_if);
    }

    fn download_shaping_enabled(&self) -> bool {
        self.sqm_enabled && matches!(self.sqm_direction_mode.as_str(), "both" | "download_only")
    }

    fn upload_shaping_enabled(&self) -> bool {
        self.sqm_enabled && matches!(self.sqm_direction_mode.as_str(), "both" | "upload_only")
    }

    fn validate(&self) -> Result<(), String> {
        self.route_spec().validate()?;
        if !matches!(
            self.sqm_direction_mode.as_str(),
            "both" | "upload_only" | "download_only" | "off"
        ) {
            return Err(
                "sqm_direction_mode must be both, upload_only, download_only, or off".to_string(),
            );
        }
        if self.sqm_enabled && self.sqm_direction_mode == "off" {
            return Err("sqm_direction_mode off requires sqm_enabled=0".to_string());
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
        if self.adjust_dl_shaper_rate
            && self.connection_active_thr_kbps > self.min_dl_shaper_rate_kbps
        {
            return Err(
                "connection_active_thr_kbps cannot be greater than min_dl_shaper_rate_kbps"
                    .to_string(),
            );
        }
        if self.adjust_ul_shaper_rate
            && self.connection_active_thr_kbps > self.min_ul_shaper_rate_kbps
        {
            return Err(
                "connection_active_thr_kbps cannot be greater than min_ul_shaper_rate_kbps"
                    .to_string(),
            );
        }
        if self.adaptive_ceiling_enabled {
            for (direction, safe, base, maximum, evidence) in [
                (
                    "download",
                    self.adaptive_ceiling_dl_safe_kbps,
                    self.base_dl_shaper_rate_kbps,
                    self.max_dl_shaper_rate_kbps,
                    self.adaptive_ceiling_dl_evidence.as_str(),
                ),
                (
                    "upload",
                    self.adaptive_ceiling_ul_safe_kbps,
                    self.base_ul_shaper_rate_kbps,
                    self.max_ul_shaper_rate_kbps,
                    self.adaptive_ceiling_ul_evidence.as_str(),
                ),
            ] {
                if !matches!(
                    evidence,
                    "legacy_unverified"
                        | "shaped_validation"
                        | "retained_configuration"
                        | "user_configured"
                ) {
                    return Err(format!(
                        "adaptive ceiling {direction} evidence is unsupported"
                    ));
                }
                if evidence == "legacy_unverified" {
                    if safe != 0.0 {
                        return Err(format!(
                            "legacy-unverified adaptive ceiling {direction} must not claim a safe rate"
                        ));
                    }
                } else if !safe.is_finite() || safe < base || safe > maximum {
                    return Err(format!(
                        "adaptive ceiling {direction} safe rate must stay between base and maximum"
                    ));
                }
            }
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
            if !(2..=30).contains(&self.rating_capture_quiet_s) {
                return Err("rating_capture_quiet_s must be between 2 and 30".to_string());
            }
            if !(5..=120).contains(&self.rating_capture_quiet_timeout_s) {
                return Err("rating_capture_quiet_timeout_s must be between 5 and 120".to_string());
            }
            if !self.rating_capture_quiet_ratio.is_finite()
                || !(0.01..=0.25).contains(&self.rating_capture_quiet_ratio)
            {
                return Err("rating_capture_quiet_ratio must be between 0.01 and 0.25".to_string());
            }
            if !self.rating_capture_quiet_min_kbps.is_finite()
                || self.rating_capture_quiet_min_kbps < 0.0
            {
                return Err("rating_capture_quiet_min_kbps must not be negative".to_string());
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
        env::var_os("CAKE_AUTORATE_RUN_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/var/run/cake-autorate"))
            .join(&self.instance)
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

    #[cfg(feature = "calibration")]
    fn rating_capture_path(&self) -> PathBuf {
        self.run_dir().join("rating-capture")
    }

    #[cfg(feature = "calibration")]
    fn autotune_capture_request_path(&self) -> PathBuf {
        self.run_dir().join("autotune-capture-request")
    }

    #[cfg(feature = "calibration")]
    fn autotune_capture_snapshot_path(&self) -> PathBuf {
        self.run_dir().join("autotune-capture-snapshot")
    }

    #[cfg(feature = "calibration")]
    fn autotune_load_evidence_path(&self) -> PathBuf {
        self.run_dir().join("autotune-load-evidence")
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
    #[cfg(feature = "calibration")]
    dl_observed_at: Instant,
    #[cfg(feature = "calibration")]
    ul_observed_at: Instant,
}

#[cfg(feature = "calibration")]
struct SpeedtestCounterRateMonitor {
    tracker: operations::autotune_counter::AutotuneCounterRateTracker,
}

#[cfg(feature = "calibration")]
struct AutotuneSpeedtestRateMonitor {
    request: operations::full_autotune::AutotuneCaptureRequest,
    counter_epoch: u64,
    monitor: SpeedtestCounterRateMonitor,
}

#[cfg(feature = "calibration")]
#[derive(Clone, Debug)]
struct IdentityBoundRateSample {
    request: operations::full_autotune::AutotuneCaptureRequest,
    sample: RateSample,
}

#[cfg(feature = "calibration")]
#[derive(Clone, Debug)]
struct IdentityBoundCounterDelta {
    request: operations::full_autotune::AutotuneCaptureRequest,
    delta: operations::autotune_counter::AutotuneCounterDelta,
}

#[cfg(feature = "calibration")]
#[derive(Clone, Debug)]
struct IdentityBoundIdleRateReference {
    request: operations::full_autotune::AutotuneCaptureRequest,
    download_kbps: f64,
    upload_kbps: f64,
}

#[cfg(feature = "calibration")]
impl AutotuneSpeedtestRateMonitor {
    fn matches(&self, request: &operations::full_autotune::AutotuneCaptureRequest) -> bool {
        &self.request == request
    }
}

#[cfg(feature = "calibration")]
fn autotune_counter_completion_matches(
    request: &operations::full_autotune::AutotuneCaptureRequest,
    monitor: &AutotuneSpeedtestRateMonitor,
    completion: &operations::autotune_counter::AutotuneCounterCompletion,
) -> bool {
    &completion.request == request && completion.epoch == monitor.counter_epoch
}

#[cfg(feature = "calibration")]
fn autotune_counter_completion_is_timely(
    request: &operations::full_autotune::AutotuneCaptureRequest,
    completion: &operations::autotune_counter::AutotuneCounterCompletion,
) -> bool {
    completion.completed_boot_ms > 0 && completion.completed_boot_ms <= request.deadline_boot_ms
}

#[cfg(feature = "calibration")]
fn autotune_capture_uses_identity_bound_speedtest_counters(
    request: &operations::full_autotune::AutotuneCaptureRequest,
) -> bool {
    // The ordinary RateMonitor is already normalized to the exact managed
    // baseline: a disabled direction uses the physical SQM-interface counter
    // while an enabled direction uses its shaped counter.  Idle capture must
    // include all background traffic and runs before a controlled speed-test
    // flight exists, so requiring job-owned nft counters here would starve a
    // valid directional baseline forever.
    //
    // Every loaded measurement runs under a permit-scoped runtime topology.
    // Its IFB may deliberately replace the configured SQM IFB, so the ordinary
    // shaped counter path is not a stable identity even for ShapedBoth. Only
    // the job-owned counters follow the exact controlled flight across every
    // loaded topology.
    request.phase == operations::full_autotune::AutotuneCapturePhase::LoadedMeasurement
}

#[cfg(feature = "calibration")]
fn select_autotune_capture_rates(
    topology: operations::full_autotune::MeasurementTopology,
    controlled: Option<RateSample>,
) -> Result<RateSample, String> {
    controlled.ok_or_else(|| {
        format!(
            "native Auto-Tune identity-bound speedtest counter sample is unavailable for {}",
            topology.as_str()
        )
    })
}

#[cfg(feature = "calibration")]
fn rate_sample_is_recent(sample: RateSample, now: Instant, max_age: Duration) -> bool {
    now.checked_duration_since(sample.dl_observed_at)
        .is_some_and(|age| age <= max_age)
        && now
            .checked_duration_since(sample.ul_observed_at)
            .is_some_and(|age| age <= max_age)
}

#[cfg(feature = "calibration")]
fn autotune_transport_control_phase(
    request: &operations::full_autotune::AutotuneCaptureRequest,
    rates: Option<RateSample>,
    now: Instant,
    max_age: Duration,
    configured_threshold_kbps: f64,
    ack_ratio: f64,
) -> Result<Option<(bool, bool)>, String> {
    let Some(rates) = rates else {
        return Ok(None);
    };
    if !rate_sample_is_recent(rates, now, max_age) {
        return Ok(None);
    }
    operations::autotune_capture::bounded_directional_load_phase(
        request,
        rates.dl_kbps,
        rates.ul_kbps,
        configured_threshold_kbps,
        ack_ratio,
    )
    .map(Some)
}

#[cfg(feature = "calibration")]
fn autotune_transport_delta_phase(
    request: &operations::full_autotune::AutotuneCaptureRequest,
    delta: operations::autotune_counter::AutotuneCounterDelta,
    maximum_span: Duration,
    configured_threshold_kbps: f64,
    ack_ratio: f64,
) -> Result<Option<(bool, bool)>, String> {
    if !delta.within_maximum_span || delta.observed_end <= delta.observed_start {
        return Ok(None);
    }
    let elapsed_duration = delta.observed_end.duration_since(delta.observed_start);
    if elapsed_duration > maximum_span {
        return Ok(None);
    }
    let elapsed = elapsed_duration.as_secs_f64();
    if !elapsed.is_finite() || elapsed <= 0.0 {
        return Ok(None);
    }
    operations::autotune_capture::bounded_directional_load_phase(
        request,
        delta.download_bytes as f64 * 8.0 / elapsed / 1_000.0,
        delta.upload_bytes as f64 * 8.0 / elapsed / 1_000.0,
        configured_threshold_kbps,
        ack_ratio,
    )
    .map(Some)
}

impl RateMonitor {
    fn new(rx_path: &str, tx_path: &str, interval_ms: u64) -> io::Result<Self> {
        Ok(Self {
            rx_path: PathBuf::from(rx_path),
            tx_path: PathBuf::from(tx_path),
            min_interval: Duration::from_millis(interval_ms.max(25)),
            prev_rx: read_u64_file(rx_path)?,
            prev_tx: read_u64_file(tx_path)?,
            last: Instant::now(),
            last_dl_kbps: 0.0,
            last_ul_kbps: 0.0,
        })
    }

    fn try_sample(&mut self) -> io::Result<RateSample> {
        self.try_sample_at(Instant::now())
    }

    fn try_sample_at(&mut self, now: Instant) -> io::Result<RateSample> {
        let interval = now.duration_since(self.last);
        if interval < self.min_interval {
            return Ok(RateSample {
                dl_kbps: self.last_dl_kbps,
                ul_kbps: self.last_ul_kbps,
                fresh: false,
                #[cfg(feature = "calibration")]
                dl_observed_at: self.last,
                #[cfg(feature = "calibration")]
                ul_observed_at: self.last,
            });
        }
        let elapsed = interval.as_secs_f64();
        let rx = read_u64_file(&self.rx_path)?;
        let tx = read_u64_file(&self.tx_path)?;
        let dl = rx.saturating_sub(self.prev_rx) as f64 * 8.0 / elapsed / 1000.0;
        let ul = tx.saturating_sub(self.prev_tx) as f64 * 8.0 / elapsed / 1000.0;
        self.prev_rx = rx;
        self.prev_tx = tx;
        self.last = now;
        self.last_dl_kbps = dl;
        self.last_ul_kbps = ul;
        Ok(RateSample {
            dl_kbps: dl,
            ul_kbps: ul,
            fresh: true,
            #[cfg(feature = "calibration")]
            dl_observed_at: now,
            #[cfg(feature = "calibration")]
            ul_observed_at: now,
        })
    }

    fn sample(&mut self) -> RateSample {
        self.try_sample().unwrap_or(RateSample {
            dl_kbps: self.last_dl_kbps,
            ul_kbps: self.last_ul_kbps,
            fresh: false,
            #[cfg(feature = "calibration")]
            dl_observed_at: self.last,
            #[cfg(feature = "calibration")]
            ul_observed_at: self.last,
        })
    }
}

#[cfg(feature = "calibration")]
impl SpeedtestCounterRateMonitor {
    fn new(interval_ms: u64) -> Self {
        Self {
            tracker: operations::autotune_counter::AutotuneCounterRateTracker::new(interval_ms),
        }
    }

    fn reset(&mut self, now: Instant) {
        self.tracker.reset(now);
    }

    fn sample_due(&self, now: Instant) -> bool {
        self.tracker.sample_due(now)
    }

    fn cached_sample(&self) -> Option<RateSample> {
        self.tracker.cached_sample().map(|sample| RateSample {
            dl_kbps: sample.download_kbps,
            ul_kbps: sample.upload_kbps,
            fresh: sample.fresh,
            dl_observed_at: sample.observed_end,
            ul_observed_at: sample.observed_end,
        })
    }

    #[cfg(test)]
    fn observe_counters(
        &mut self,
        now: Instant,
        current: Option<operations::speedtest::SpeedtestTrafficCounters>,
    ) -> Option<RateSample> {
        self.observe_counters_with_delta(now, current).0
    }

    fn observe_counters_with_delta(
        &mut self,
        now: Instant,
        current: Option<operations::speedtest::SpeedtestTrafficCounters>,
    ) -> (
        Option<RateSample>,
        Option<operations::autotune_counter::AutotuneCounterDelta>,
    ) {
        let observation = self.tracker.observe_counters_with_delta(now, current);
        let rate = observation.rate_window.map(|sample| RateSample {
            dl_kbps: sample.download_kbps,
            ul_kbps: sample.upload_kbps,
            fresh: sample.fresh,
            dl_observed_at: sample.observed_end,
            ul_observed_at: sample.observed_end,
        });
        (rate, observation.delta)
    }
}

fn root_cake_qdisc_count(output: &str) -> usize {
    output
        .lines()
        .filter(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            fields.first() == Some(&"qdisc")
                && fields
                    .get(1)
                    .map(|kind| *kind == "cake" || *kind == "cake_mq")
                    .unwrap_or(false)
                && fields.contains(&"root")
        })
        .count()
}

#[cfg(test)]
fn qdisc_output_has_cake(output: &str) -> bool {
    root_cake_qdisc_count(output) == 1
}

fn ingress_redirect_targets(output: &str) -> Vec<String> {
    fn clean(value: &str) -> &str {
        value.trim_matches(|character: char| matches!(character, '(' | ')' | '[' | ']' | ',' | ';'))
    }

    let mut targets = Vec::new();
    for line in output.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let target = fields
            .windows(3)
            .find(|window| {
                window[0].eq_ignore_ascii_case("redirect") && window[1].eq_ignore_ascii_case("dev")
            })
            .map(|window| clean(window[2]))
            .or_else(|| {
                fields.windows(4).find_map(|window| {
                    (window[0].eq_ignore_ascii_case("redirect")
                        && window[1].eq_ignore_ascii_case("to")
                        && window[2].eq_ignore_ascii_case("device"))
                    .then(|| clean(window[3]))
                })
            });
        if let Some(target) = target {
            if !target.is_empty() && !targets.iter().any(|known| known == target) {
                targets.push(target.to_string());
            }
        }
    }
    targets
}

#[cfg(test)]
fn ingress_output_targets_ifb(output: &str, ifb: &str) -> bool {
    !ifb.is_empty()
        && ingress_redirect_targets(output)
            .iter()
            .any(|target| target == ifb)
}

fn tc_output(args: &[&str]) -> Result<String, String> {
    let tc = env::var("CAKE_AUTORATE_TC").unwrap_or_else(|_| "tc".to_string());
    let output = Command::new(&tc)
        .args(args)
        .output()
        .map_err(|error| format!("failed to execute {tc}: {error}"))?;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CakeQdiscKind {
    Cake,
    CakeMq,
}

impl CakeQdiscKind {
    fn as_tc_kind(self) -> &'static str {
        match self {
            Self::Cake => "cake",
            Self::CakeMq => "cake_mq",
        }
    }
}

impl From<CakeQdiscKind> for operations::autotune_runtime::RuntimeQdiscKind {
    fn from(value: CakeQdiscKind) -> Self {
        match value {
            CakeQdiscKind::Cake => Self::Cake,
            CakeQdiscKind::CakeMq => Self::CakeMq,
        }
    }
}

#[cfg(feature = "calibration")]
fn published_runtime_qdisc_kind(
    shaping_enabled: bool,
    observed: Option<CakeQdiscKind>,
) -> Option<operations::autotune_runtime::RuntimeQdiscKind> {
    if shaping_enabled {
        observed.map(Into::into)
    } else {
        None
    }
}

fn published_applied_cake_rate_kbps(shaping_enabled: bool, last_applied_kbps: u64) -> f64 {
    if shaping_enabled {
        last_applied_kbps as f64
    } else {
        0.0
    }
}

fn change_cake_rate(
    interface: &str,
    rate_kbps: u64,
    qdisc_kind: CakeQdiscKind,
) -> Result<(), String> {
    if interface.is_empty() || rate_kbps < 100 || rate_kbps > autotune::MAX_RATE_KBPS {
        return Err("requested CAKE rate is outside the supported range".to_string());
    }
    let tc = env::var("CAKE_AUTORATE_TC").unwrap_or_else(|_| "tc".to_string());
    let output = Command::new(&tc)
        .arg("qdisc")
        .arg("change")
        .arg("root")
        .arg("dev")
        .arg(interface)
        .arg(qdisc_kind.as_tc_kind())
        .arg("bandwidth")
        .arg(format!("{rate_kbps}Kbit"))
        .output()
        .map_err(|error| format!("failed to execute {tc}: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if stderr.is_empty() {
        format!(
            "tc rate change for {interface} failed with {}",
            output.status
        )
    } else {
        stderr
    })
}

fn delete_qdisc(interface: &str, location: &str) -> Result<(), String> {
    if interface.is_empty() || !matches!(location, "root" | "ingress") {
        return Err("requested qdisc deletion is invalid".to_string());
    }
    let tc = env::var("CAKE_AUTORATE_TC").unwrap_or_else(|_| "tc".to_string());
    let output = Command::new(&tc)
        .args(["qdisc", "del", "dev", interface, location])
        .output()
        .map_err(|error| format!("failed to execute {tc}: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if stderr.is_empty() {
        format!(
            "tc qdisc deletion on {interface} failed with {}",
            output.status
        )
    } else {
        stderr
    })
}

#[cfg(test)]
fn root_cake_bandwidth_kbps(output: &str) -> Result<u64, String> {
    root_cake_qdisc(output).map(|(_, rate)| rate)
}

fn root_cake_qdisc(output: &str) -> Result<(CakeQdiscKind, u64), String> {
    let mut qdiscs = Vec::new();
    for line in output.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.first() != Some(&"qdisc") || !fields.contains(&"root") {
            continue;
        }
        let kind = match fields.get(1).copied() {
            Some("cake") => CakeQdiscKind::Cake,
            Some("cake_mq") => CakeQdiscKind::CakeMq,
            _ => continue,
        };
        let token = fields
            .windows(2)
            .find_map(|pair| (pair[0] == "bandwidth").then_some(pair[1]))
            .ok_or_else(|| "root CAKE qdisc has no bandwidth".to_string())?;
        qdiscs.push((kind, parse_tc_bandwidth_kbps(token)?));
    }
    match qdiscs.as_slice() {
        [qdisc] => Ok(*qdisc),
        [] => Err("root CAKE qdisc is missing".to_string()),
        _ => Err("multiple root CAKE qdiscs are present".to_string()),
    }
}

fn parse_tc_bandwidth_kbps(value: &str) -> Result<u64, String> {
    let (number, multiplier, divisor) = if let Some(number) = value.strip_suffix("Kbit") {
        (number, 1u64, 1u64)
    } else if let Some(number) = value.strip_suffix("Mbit") {
        (number, 1_000u64, 1u64)
    } else if let Some(number) = value.strip_suffix("Gbit") {
        (number, 1_000_000u64, 1u64)
    } else if let Some(number) = value.strip_suffix("bit") {
        (number, 1u64, 1_000u64)
    } else {
        return Err("CAKE bandwidth unit is unsupported".to_string());
    };
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if whole.is_empty()
        || whole.bytes().any(|byte| !byte.is_ascii_digit())
        || fraction.len() > 6
        || fraction.bytes().any(|byte| !byte.is_ascii_digit())
    {
        return Err("CAKE bandwidth is not a bounded decimal".to_string());
    }
    let whole = whole
        .parse::<u64>()
        .map_err(|_| "CAKE bandwidth overflows".to_string())?;
    let fractional_scale = 10u64.pow(fraction.len() as u32);
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction
            .parse::<u64>()
            .map_err(|_| "CAKE bandwidth fraction is invalid".to_string())?
    };
    let numerator = whole
        .checked_mul(fractional_scale)
        .and_then(|value| value.checked_add(fraction))
        .and_then(|value| value.checked_mul(multiplier))
        .ok_or_else(|| "CAKE bandwidth overflows".to_string())?;
    let denominator = fractional_scale
        .checked_mul(divisor)
        .ok_or_else(|| "CAKE bandwidth divisor overflows".to_string())?;
    let rounded = numerator
        .checked_add(denominator / 2)
        .ok_or_else(|| "CAKE bandwidth rounding overflows".to_string())?
        / denominator;
    if !(100..=autotune::MAX_RATE_KBPS).contains(&rounded) {
        return Err("CAKE bandwidth is outside the supported range".to_string());
    }
    Ok(rounded)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SqmTopologyErrorKind {
    Settling,
    Unsafe,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SqmTopologyError {
    pub kind: SqmTopologyErrorKind,
    pub code: &'static str,
    pub message: String,
}

impl SqmTopologyError {
    fn settling(code: &'static str, message: String) -> Self {
        Self {
            kind: SqmTopologyErrorKind::Settling,
            code,
            message,
        }
    }

    fn unsafe_state(code: &'static str, message: String) -> Self {
        Self {
            kind: SqmTopologyErrorKind::Unsafe,
            code,
            message,
        }
    }
}

impl std::fmt::Display for SqmTopologyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

fn topology_tc_output(args: &[&str]) -> Result<String, SqmTopologyError> {
    tc_output(args).map_err(|error| {
        SqmTopologyError::unsafe_state(
            "tc-query-failed",
            format!("tc {} could not be inspected: {error}", args.join(" ")),
        )
    })
}

fn attest_cake_direction(
    output: &str,
    enabled: bool,
    direction: &'static str,
    interface: &str,
) -> Result<(), SqmTopologyError> {
    let count = root_cake_qdisc_count(output);
    match (enabled, count) {
        (true, 1) | (false, 0) => Ok(()),
        (true, 0) => Err(SqmTopologyError::settling(
            if direction == "download" {
                "download-cake-missing"
            } else {
                "upload-cake-missing"
            },
            format!("CAKE qdisc is missing on {interface}"),
        )),
        (true, count) => Err(SqmTopologyError::unsafe_state(
            if direction == "download" {
                "download-cake-ambiguous"
            } else {
                "upload-cake-ambiguous"
            },
            format!("{count} root CAKE qdiscs were found on {interface}"),
        )),
        (false, _) => Err(SqmTopologyError::unsafe_state(
            if direction == "download" {
                "disabled-download-cake-remains"
            } else {
                "disabled-upload-cake-remains"
            },
            format!("CAKE qdisc remains on disabled {direction} interface {interface}"),
        )),
    }
}

fn attest_download_redirect(
    output: &str,
    enabled: bool,
    source_interface: &str,
    download_interface: &str,
) -> Result<(), SqmTopologyError> {
    if !download_interface.starts_with("ifb") {
        return Ok(());
    }
    let targets = ingress_redirect_targets(output);
    let expected = targets.iter().any(|target| target == download_interface);
    if !enabled {
        return if expected {
            Err(SqmTopologyError::unsafe_state(
                "disabled-download-redirect-remains",
                format!(
                    "ingress redirect from {source_interface} to disabled download interface {download_interface} remains"
                ),
            ))
        } else {
            Ok(())
        };
    }
    if !expected {
        let message =
            format!("ingress redirect from {source_interface} to {download_interface} is missing");
        return Err(if targets.is_empty() {
            SqmTopologyError::settling("download-redirect-missing", message)
        } else {
            SqmTopologyError::unsafe_state("download-redirect-mismatch", message)
        });
    }
    if targets.iter().any(|target| target != download_interface) {
        return Err(SqmTopologyError::unsafe_state(
            "download-redirect-ambiguous",
            format!(
                "ingress on {source_interface} redirects to multiple devices: {}",
                targets.join(", ")
            ),
        ));
    }
    Ok(())
}

fn attest_exclusive_sqm_ingress(
    output: &str,
    source_interface: &str,
    download_interface: &str,
) -> Result<(), SqmTopologyError> {
    attest_download_redirect(output, true, source_interface, download_interface)?;

    let mut base_headers = 0usize;
    let mut table_headers = 0usize;
    let mut rule_headers = 0usize;
    let mut match_lines = 0usize;
    let mut action_lines = 0usize;
    let mut preference: Option<&str> = None;

    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.first() == Some(&"filter") {
            let field = |name: &str| {
                fields
                    .windows(2)
                    .find_map(|pair| (pair[0] == name).then_some(pair[1]))
            };
            let valid_parent = field("parent") == Some("ffff:");
            let valid_protocol = field("protocol") == Some("all");
            let valid_kind = fields.contains(&"u32");
            let valid_chain = field("chain").is_none_or(|value| value == "0");
            let Some(pref) = field("pref") else {
                return Err(SqmTopologyError::unsafe_state(
                    "download-ingress-not-exclusive",
                    format!("ingress filter on {source_interface} has no preference"),
                ));
            };
            if !valid_parent
                || !valid_protocol
                || !valid_kind
                || !valid_chain
                || preference.is_some_and(|known| known != pref)
            {
                return Err(SqmTopologyError::unsafe_state(
                    "download-ingress-not-exclusive",
                    format!(
                        "ingress on {source_interface} contains a filter outside the standard SQM redirect"
                    ),
                ));
            }
            preference = Some(pref);
            match field("fh") {
                None => base_headers += 1,
                Some(handle) if handle.matches(':').count() >= 2 && fields.contains(&"order") => {
                    rule_headers += 1;
                }
                Some(_) if fields.windows(2).any(|pair| pair == ["ht", "divisor"]) => {
                    table_headers += 1;
                }
                Some(_) => {
                    return Err(SqmTopologyError::unsafe_state(
                        "download-ingress-not-exclusive",
                        format!(
                            "ingress on {source_interface} contains an unrecognized u32 filter"
                        ),
                    ));
                }
            }
        } else if fields.first() == Some(&"match") {
            if fields.get(1) != Some(&"00000000/00000000") || fields.get(2) != Some(&"at") {
                return Err(SqmTopologyError::unsafe_state(
                    "download-ingress-not-exclusive",
                    format!("ingress on {source_interface} contains an extra match"),
                ));
            }
            match_lines += 1;
        } else if fields.first() == Some(&"action") {
            if !fields
                .iter()
                .any(|field| field.eq_ignore_ascii_case("mirred"))
            {
                return Err(SqmTopologyError::unsafe_state(
                    "download-ingress-not-exclusive",
                    format!("ingress on {source_interface} contains a non-SQM action"),
                ));
            }
            action_lines += 1;
        } else if fields.first() != Some(&"index") {
            return Err(SqmTopologyError::unsafe_state(
                "download-ingress-not-exclusive",
                format!("ingress on {source_interface} contains unrecognized filter state"),
            ));
        }
    }

    if base_headers != 1
        || table_headers != 1
        || rule_headers != 1
        || match_lines != 1
        || action_lines != 1
    {
        return Err(SqmTopologyError::unsafe_state(
            "download-ingress-not-exclusive",
            format!(
                "ingress on {source_interface} is not exclusively the single standard SQM redirect to {download_interface}"
            ),
        ));
    }
    Ok(())
}

fn inspect_sqm_topology_for(
    cfg: &Config,
    download_enabled: bool,
    upload_enabled: bool,
) -> Result<(), SqmTopologyError> {
    if !Path::new(&cfg.rx_bytes_path).is_file() {
        return Err(SqmTopologyError::settling(
            "download-counter-missing",
            format!("download counter is missing for {}", cfg.dl_if),
        ));
    }
    if !Path::new(&cfg.tx_bytes_path).is_file() {
        return Err(SqmTopologyError::settling(
            "upload-counter-missing",
            format!("upload counter is missing for {}", cfg.ul_if),
        ));
    }

    let dl_output = topology_direction_qdisc_output(&cfg.dl_if, download_enabled, "download")?;
    attest_cake_direction(&dl_output, download_enabled, "download", &cfg.dl_if)?;
    let ul_output = topology_direction_qdisc_output(&cfg.ul_if, upload_enabled, "upload")?;
    attest_cake_direction(&ul_output, upload_enabled, "upload", &cfg.ul_if)?;

    if cfg.dl_if.starts_with("ifb") {
        let ingress =
            topology_tc_output(&["filter", "show", "dev", &cfg.sqm_interface, "ingress"])?;
        attest_download_redirect(&ingress, download_enabled, &cfg.sqm_interface, &cfg.dl_if)?;
    }
    Ok(())
}

fn topology_direction_qdisc_output(
    interface: &str,
    enabled: bool,
    direction: &'static str,
) -> Result<String, SqmTopologyError> {
    let interface_path = sqm_sys_class_net().join(interface);
    if !interface_path.exists() {
        return if enabled {
            Err(SqmTopologyError::settling(
                if direction == "download" {
                    "download-device-missing"
                } else {
                    "upload-device-missing"
                },
                format!("{direction} shaping device {interface} is missing"),
            ))
        } else {
            // A direction which is deliberately unshaped need not retain its
            // SQM-created IFB.  The source-interface ingress state is still
            // inspected below, so a stale redirect cannot be hidden here.
            Ok(String::new())
        };
    }

    match topology_tc_output(&["qdisc", "show", "dev", interface]) {
        Ok(output) => Ok(output),
        Err(_) if !interface_path.exists() && !enabled => Ok(String::new()),
        Err(_) if !interface_path.exists() => Err(SqmTopologyError::settling(
            if direction == "download" {
                "download-device-missing"
            } else {
                "upload-device-missing"
            },
            format!("{direction} shaping device {interface} disappeared during inspection"),
        )),
        Err(error) => Err(error),
    }
}

pub(crate) fn inspect_sqm_topology(cfg: &Config) -> Result<(), SqmTopologyError> {
    inspect_sqm_topology_for(
        cfg,
        cfg.download_shaping_enabled(),
        cfg.upload_shaping_enabled(),
    )
}

fn managed_sqm_target_ready(cfg: &Config) -> bool {
    let sys_class_net = env::var_os("CAKE_AUTORATE_SYS_CLASS_NET")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/sys/class/net"));

    sys_class_net.join(&cfg.sqm_interface).exists() || Path::new(&cfg.tx_bytes_path).is_file()
}

fn sqm_sys_class_net() -> PathBuf {
    env::var_os("CAKE_AUTORATE_SYS_CLASS_NET")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/sys/class/net"))
}

fn sqm_interface_ifindex(interface: &str) -> Option<u64> {
    fs::read_to_string(sqm_sys_class_net().join(interface).join("ifindex"))
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

fn sqm_topology_query_signature(args: &[&str]) -> u64 {
    match topology_tc_output(args) {
        Ok(output) => stable_hash(&format!("ok\n{output}")),
        Err(error) => stable_hash(&format!("error\n{}\n{}", error.code, error.message)),
    }
}

fn sqm_topology_generation(
    cfg: &Config,
    topology: &Result<(), SqmTopologyError>,
) -> operations::sqm_recovery::SqmTopologyGeneration {
    use operations::sqm_recovery::{SqmObservedTopologyState, SqmTopologyGeneration};

    let observed_topology = match topology {
        Ok(()) => SqmObservedTopologyState::Healthy,
        Err(error) if error.kind == SqmTopologyErrorKind::Settling => {
            SqmObservedTopologyState::Settling(error.code.to_string())
        }
        Err(error) => SqmObservedTopologyState::Unsafe(error.code.to_string()),
    };
    SqmTopologyGeneration {
        target_present: managed_sqm_target_ready(cfg),
        target_ifindex: sqm_interface_ifindex(&cfg.sqm_interface),
        upload_ifindex: sqm_interface_ifindex(&cfg.ul_if),
        download_ifindex: sqm_interface_ifindex(&cfg.dl_if),
        download_counter_present: Path::new(&cfg.rx_bytes_path).is_file(),
        upload_counter_present: Path::new(&cfg.tx_bytes_path).is_file(),
        download_qdisc_signature: sqm_topology_query_signature(&[
            "qdisc", "show", "dev", &cfg.dl_if,
        ]),
        upload_qdisc_signature: sqm_topology_query_signature(&["qdisc", "show", "dev", &cfg.ul_if]),
        ingress_signature: cfg.dl_if.starts_with("ifb").then(|| {
            sqm_topology_query_signature(&["filter", "show", "dev", &cfg.sqm_interface, "ingress"])
        }),
        topology: observed_topology,
    }
}

#[derive(Debug)]
enum SqmRecoveryError {
    Busy(String),
    Failed(String),
    Terminated,
}

impl SqmRecoveryError {
    fn message(&self) -> &str {
        match self {
            Self::Busy(message) | Self::Failed(message) => message,
            Self::Terminated => "terminated while recovering managed SQM",
        }
    }
}

fn terminate_helper_process_group(child: &mut Child) {
    let process_group = -(child.id() as i32);
    unsafe {
        kill(process_group, 15);
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(_) => break,
        }
    }
    unsafe {
        kill(process_group, 9);
    }
    let _ = child.wait();
}

fn run_sqm_helper(cfg: &Config, operation: Option<&str>) -> Result<(), SqmRecoveryError> {
    let helper = env::var("CAKE_AUTORATE_SQM_RECOVER")
        .unwrap_or_else(|_| "/usr/libexec/cake-autorate-rs/sqm-recover".to_string());
    let mut command = Command::new(&helper);
    command
        .arg(&cfg.instance)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .process_group(0);
    if let Some(operation) = operation {
        command.arg(operation);
    }
    let mut child = command.spawn().map_err(|error| {
        SqmRecoveryError::Failed(format!("failed to execute {helper}: {error}"))
    })?;
    let stderr = child.stderr.take();
    let stderr_reader = thread::spawn(move || {
        let mut detail = String::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_string(&mut detail);
        }
        detail
    });
    let status = loop {
        if TERMINATE.load(Ordering::SeqCst) {
            terminate_helper_process_group(&mut child);
            let _ = stderr_reader.join();
            return Err(SqmRecoveryError::Terminated);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(error) => {
                terminate_helper_process_group(&mut child);
                let _ = stderr_reader.join();
                return Err(SqmRecoveryError::Failed(format!(
                    "failed to wait for {helper}: {error}"
                )));
            }
        }
    };
    let detail = stderr_reader.join().unwrap_or_default();
    let detail = detail.trim().to_string();
    if !status.success() {
        let message = if detail.is_empty() {
            format!("SQM recovery helper failed with {status}")
        } else {
            detail
        };
        if status.code() == Some(75) {
            return Err(SqmRecoveryError::Busy(message));
        }
        return Err(SqmRecoveryError::Failed(message));
    }
    Ok(())
}

fn attest_managed_sqm(cfg: &Config) -> Result<(), SqmRecoveryError> {
    run_sqm_helper(cfg, Some("check"))?;
    inspect_sqm_topology(cfg).map_err(|error| SqmRecoveryError::Failed(error.to_string()))
}

fn recover_managed_sqm(cfg: &Config) -> Result<(), SqmRecoveryError> {
    run_sqm_helper(cfg, None)?;
    inspect_sqm_topology(cfg).map_err(|error| SqmRecoveryError::Failed(error.to_string()))
}

#[derive(Clone, Debug)]
struct TransportProbeRequest {
    #[cfg(feature = "calibration")]
    probe_id: u64,
    control_valid: bool,
    dl_loaded: bool,
    ul_loaded: bool,
    rating_phase: RatingPhase,
    #[cfg(feature = "calibration")]
    autotune_capture: Option<operations::full_autotune::AutotuneCaptureRequest>,
}

#[derive(Clone, Debug)]
struct TransportProbeResult {
    #[cfg(feature = "calibration")]
    probe_id: u64,
    #[cfg(feature = "calibration")]
    started_at: Instant,
    #[cfg(feature = "calibration")]
    completed_at: Instant,
    control_valid: bool,
    #[cfg(feature = "calibration")]
    capture_interval_valid: Option<bool>,
    endpoint: String,
    dl_loaded: bool,
    ul_loaded: bool,
    rating_phase: RatingPhase,
    #[cfg(feature = "calibration")]
    autotune_capture: Option<operations::full_autotune::AutotuneCaptureRequest>,
    latency_ms: Option<f64>,
    error: Option<String>,
    #[cfg(feature = "calibration")]
    failure_kind: Option<transport_probe::TransportProbeFailureKind>,
    #[cfg(feature = "calibration")]
    failure_deadline_us: Option<u64>,
    route_identity: Option<String>,
    backend: String,
    trusted: bool,
    raw_samples_ms: Vec<f64>,
    discarded_samples: usize,
    server_processing_ms: f64,
    connection_reused: bool,
}

#[cfg(feature = "calibration")]
#[derive(Clone, Debug, Eq, PartialEq)]
struct AutotuneTransportCaptureKey {
    capture_id: String,
    request_sequence: u32,
    route_identity: String,
}

#[cfg(feature = "calibration")]
impl AutotuneTransportCaptureKey {
    fn new(
        request: &operations::full_autotune::AutotuneCaptureRequest,
        route_identity: &str,
    ) -> Self {
        Self {
            capture_id: request.capture_id.clone(),
            request_sequence: request.sequence,
            route_identity: route_identity.to_string(),
        }
    }
}

#[cfg(feature = "calibration")]
#[derive(Clone, Copy, Debug)]
struct AutotuneTransportPhaseObservation {
    at: Instant,
    phase: Option<(bool, bool)>,
}

#[cfg(feature = "calibration")]
#[derive(Clone, Copy, Debug)]
struct AutotuneTransportDeltaObservation {
    start: Instant,
    end: Instant,
    phase: Option<(bool, bool)>,
}

#[cfg(feature = "calibration")]
#[derive(Clone, Debug)]
struct AutotuneTransportFlight {
    probe_id: u64,
    key: AutotuneTransportCaptureKey,
    expected_phase: (bool, bool),
    control_valid: bool,
    submitted_at: Instant,
    physical_delta_required: bool,
}

#[cfg(feature = "calibration")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct AutotuneTransportCoverage {
    total: Duration,
    matching: Duration,
    longest_mismatch: Duration,
}

#[cfg(feature = "calibration")]
#[derive(Clone, Copy, Debug)]
struct AutotuneTransportAttestation {
    valid: bool,
    reason: &'static str,
    coverage: Option<AutotuneTransportCoverage>,
}

#[cfg(feature = "calibration")]
#[derive(Clone, Copy, Debug)]
enum AutotuneTransportSettlement {
    Pending(&'static str),
    Final(AutotuneTransportAttestation),
}

#[cfg(feature = "calibration")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AutotuneTransportReadiness {
    phase: (bool, bool),
    ready: bool,
    reason: &'static str,
}

#[cfg(feature = "calibration")]
#[derive(Clone, Debug, Default)]
struct AutotuneTransportControl {
    key: Option<AutotuneTransportCaptureKey>,
    observations: VecDeque<AutotuneTransportPhaseObservation>,
    physical_deltas: VecDeque<AutotuneTransportDeltaObservation>,
    candidate_phase: (bool, bool),
    candidate_since: Option<Instant>,
    last_candidate_seen: Option<Instant>,
}

#[cfg(feature = "calibration")]
impl AutotuneTransportControl {
    fn reset(&mut self) {
        self.key = None;
        self.observations.clear();
        self.physical_deltas.clear();
        self.candidate_phase = (false, false);
        self.candidate_since = None;
        self.last_candidate_seen = None;
    }

    fn observe_physical_delta(
        &mut self,
        key: &AutotuneTransportCaptureKey,
        delta: operations::autotune_counter::AutotuneCounterDelta,
        phase: Option<(bool, bool)>,
        history_window: Duration,
    ) -> Result<(), String> {
        if self.key.as_ref() != Some(key) {
            return Err("native Auto-Tune transport counter identity changed".to_string());
        }
        if delta.observed_end <= delta.observed_start {
            return Err("native Auto-Tune transport counter delta is not causal".to_string());
        }
        if let Some(previous) = self.physical_deltas.back() {
            if delta.observed_start < previous.end {
                return Err(
                    "native Auto-Tune transport counter deltas overlap or move backwards"
                        .to_string(),
                );
            }
        }
        self.physical_deltas
            .push_back(AutotuneTransportDeltaObservation {
                start: delta.observed_start,
                end: delta.observed_end,
                phase,
            });
        if let Some(cutoff) = delta.observed_end.checked_sub(history_window) {
            while self
                .physical_deltas
                .front()
                .is_some_and(|observation| observation.end <= cutoff)
            {
                self.physical_deltas.pop_front();
            }
        }
        Ok(())
    }

    fn observe(
        &mut self,
        key: AutotuneTransportCaptureKey,
        phase: Option<(bool, bool)>,
        expected: (bool, bool),
        now: Instant,
        dropout: Duration,
        history_window: Duration,
    ) {
        if self.key.as_ref() != Some(&key) {
            self.reset();
            self.key = Some(key.clone());
        }

        if expected != (false, false) && phase == Some(expected) {
            if self.candidate_phase != expected {
                self.candidate_phase = expected;
                self.candidate_since = Some(now);
            }
            self.last_candidate_seen = Some(now);
        } else {
            let dropout_exceeded = self
                .last_candidate_seen
                .and_then(|seen| now.checked_duration_since(seen))
                .map_or(true, |elapsed| elapsed > dropout);
            if dropout_exceeded {
                self.candidate_phase = (false, false);
                self.candidate_since = None;
                self.last_candidate_seen = None;
            }
        }

        if let Some(last) = self.observations.back_mut() {
            if last.at == now {
                last.phase = phase;
            } else if last.at < now {
                self.observations
                    .push_back(AutotuneTransportPhaseObservation { at: now, phase });
            } else {
                self.reset();
                self.key = Some(key);
                self.observations
                    .push_back(AutotuneTransportPhaseObservation { at: now, phase });
            }
        } else {
            self.observations
                .push_back(AutotuneTransportPhaseObservation { at: now, phase });
        }

        if let Some(cutoff) = now.checked_sub(history_window) {
            while self.observations.len() > 1
                && self
                    .observations
                    .get(1)
                    .is_some_and(|next| next.at <= cutoff)
            {
                self.observations.pop_front();
            }
        }
    }

    fn expected_phase(
        request: &operations::full_autotune::AutotuneCaptureRequest,
    ) -> Result<(bool, bool), String> {
        use operations::full_autotune::AutotuneCapturePhase;
        use operations::protocol::SpeedtestDirection;

        match (request.phase, request.direction) {
            (AutotuneCapturePhase::IdleBaseline, None) => Ok((false, false)),
            (AutotuneCapturePhase::LoadedMeasurement, Some(SpeedtestDirection::Download)) => {
                Ok((true, false))
            }
            (AutotuneCapturePhase::LoadedMeasurement, Some(SpeedtestDirection::Upload)) => {
                Ok((false, true))
            }
            (AutotuneCapturePhase::LoadedMeasurement, Some(SpeedtestDirection::Both)) => {
                Ok((true, true))
            }
            _ => Err("native Auto-Tune transport capture direction is invalid".to_string()),
        }
    }

    fn ready_phase(
        &self,
        request: &operations::full_autotune::AutotuneCaptureRequest,
        key: &AutotuneTransportCaptureKey,
        now: Instant,
        hold: Duration,
        dropout: Duration,
    ) -> Result<((bool, bool), bool), String> {
        let readiness = self.ready_phase_diagnostic(request, key, now, hold, dropout)?;
        Ok((readiness.phase, readiness.ready))
    }

    fn ready_phase_diagnostic(
        &self,
        request: &operations::full_autotune::AutotuneCaptureRequest,
        key: &AutotuneTransportCaptureKey,
        now: Instant,
        hold: Duration,
        dropout: Duration,
    ) -> Result<AutotuneTransportReadiness, String> {
        let expected = Self::expected_phase(request)?;
        let blocked = |reason| AutotuneTransportReadiness {
            phase: expected,
            ready: false,
            reason,
        };
        if self.key.as_ref() != Some(key) {
            return Ok(blocked("capture-key-mismatch"));
        }
        let Some(current) = self.observations.back() else {
            return Ok(blocked("phase-observation-missing"));
        };
        let Some(observation_age) = now.checked_duration_since(current.at) else {
            return Ok(blocked("phase-observation-from-future"));
        };
        if observation_age > dropout {
            return Ok(blocked("phase-observation-stale"));
        };
        if expected == (false, false) {
            return Ok(if current.phase == Some(expected) {
                AutotuneTransportReadiness {
                    phase: expected,
                    ready: true,
                    reason: "idle-phase-ready",
                }
            } else {
                blocked("idle-phase-mismatch")
            });
        }
        if let Some(delta) = self.physical_deltas.back() {
            let Some(delta_age) = now.checked_duration_since(delta.end) else {
                return Ok(blocked("loaded-physical-delta-from-future"));
            };
            if delta_age <= dropout && delta.phase == Some(expected) {
                // This only authorizes a speculative probe.  Loaded evidence
                // is admitted later from the complete non-overlapping physical
                // counter-delta chain which brackets the probe flight.  The
                // rolling window is never used as post-flight proof.
                return Ok(AutotuneTransportReadiness {
                    phase: expected,
                    ready: true,
                    reason: "loaded-physical-delta-ready",
                });
            }
        }
        if current.phase != Some(expected) || self.candidate_phase != expected {
            return Ok(blocked("loaded-phase-mismatch"));
        }
        let Some(candidate_since) = self.candidate_since else {
            return Ok(blocked("loaded-phase-candidate-missing"));
        };
        let Some(window_start) = now.checked_sub(hold) else {
            return Ok(blocked("loaded-phase-hold-clock-invalid"));
        };
        if candidate_since > window_start {
            return Ok(blocked("loaded-phase-hold-pending"));
        }
        let Some(coverage) = self.coverage(window_start, now, expected) else {
            return Ok(blocked("loaded-phase-coverage-missing"));
        };
        if coverage.longest_mismatch > dropout {
            return Ok(blocked("loaded-phase-dropout-exceeded"));
        }
        if !loaded_coverage_sufficient(coverage) {
            return Ok(blocked("loaded-phase-coverage-insufficient"));
        }
        Ok(AutotuneTransportReadiness {
            phase: expected,
            ready: true,
            reason: "loaded-phase-ready",
        })
    }

    #[cfg(test)]
    fn attest(
        &self,
        flight: &AutotuneTransportFlight,
        result: &TransportProbeResult,
        dropout: Duration,
    ) -> bool {
        self.attest_diagnostic(flight, result, dropout).valid
    }

    fn attest_diagnostic(
        &self,
        flight: &AutotuneTransportFlight,
        result: &TransportProbeResult,
        dropout: Duration,
    ) -> AutotuneTransportAttestation {
        let invalid = |reason| AutotuneTransportAttestation {
            valid: false,
            reason,
            coverage: None,
        };
        if !flight.control_valid {
            return invalid("control-invalid");
        }
        if flight.probe_id != result.probe_id {
            return invalid("probe-identity-mismatch");
        }
        if self.key.as_ref() != Some(&flight.key) {
            return invalid("capture-identity-changed");
        }
        if result.started_at < flight.submitted_at {
            return invalid("noncausal-start");
        }
        if result.completed_at <= result.started_at {
            return invalid("invalid-flight-interval");
        }
        let Some(coverage) = self.coverage(
            result.started_at,
            result.completed_at,
            flight.expected_phase,
        ) else {
            return invalid("missing-flight-history");
        };
        let (valid, reason) = if flight.expected_phase == (false, false) {
            if coverage.matching == coverage.total {
                (true, "idle-flight-valid")
            } else {
                (false, "idle-flight-contaminated")
            }
        } else if coverage.longest_mismatch > dropout {
            (false, "flight-dropout-exceeded")
        } else if !loaded_coverage_sufficient(coverage) {
            (false, "flight-coverage-insufficient")
        } else {
            (true, "loaded-flight-valid")
        };
        AutotuneTransportAttestation {
            valid,
            reason,
            coverage: Some(coverage),
        }
    }

    fn settle_diagnostic(
        &self,
        flight: &AutotuneTransportFlight,
        result: &TransportProbeResult,
        dropout: Duration,
    ) -> AutotuneTransportSettlement {
        if !flight.physical_delta_required {
            return AutotuneTransportSettlement::Final(
                self.attest_diagnostic(flight, result, dropout),
            );
        }
        self.settle_physical_interval_diagnostic(
            flight,
            result.probe_id,
            result.started_at,
            result.completed_at,
            dropout,
        )
    }

    fn settle_physical_interval_diagnostic(
        &self,
        flight: &AutotuneTransportFlight,
        probe_id: u64,
        started_at: Instant,
        completed_at: Instant,
        dropout: Duration,
    ) -> AutotuneTransportSettlement {
        let invalid = |reason| {
            AutotuneTransportSettlement::Final(AutotuneTransportAttestation {
                valid: false,
                reason,
                coverage: None,
            })
        };
        if !flight.control_valid {
            return invalid("control-invalid");
        }
        if flight.probe_id != probe_id {
            return invalid("probe-identity-mismatch");
        }
        if self.key.as_ref() != Some(&flight.key) {
            return invalid("capture-identity-changed");
        }
        if started_at < flight.submitted_at {
            return invalid("noncausal-start");
        }
        if completed_at <= started_at {
            return invalid("invalid-flight-interval");
        }
        let Some(horizon) = self.physical_deltas.back().map(|delta| delta.end) else {
            return AutotuneTransportSettlement::Pending("physical-delta-missing");
        };
        if horizon < completed_at {
            return AutotuneTransportSettlement::Pending("physical-bracket-pending");
        }
        let Some(coverage) =
            self.physical_coverage(started_at, completed_at, flight.expected_phase)
        else {
            return invalid("physical-flight-history-missing");
        };
        let (valid, reason) = if coverage.longest_mismatch > dropout {
            (false, "physical-flight-dropout-exceeded")
        } else if !loaded_coverage_sufficient(coverage) {
            (false, "physical-flight-coverage-insufficient")
        } else {
            (true, "physical-loaded-flight-valid")
        };
        AutotuneTransportSettlement::Final(AutotuneTransportAttestation {
            valid,
            reason,
            coverage: Some(coverage),
        })
    }

    fn physical_coverage(
        &self,
        start: Instant,
        end: Instant,
        expected: (bool, bool),
    ) -> Option<AutotuneTransportCoverage> {
        if end <= start || self.physical_deltas.is_empty() {
            return None;
        }
        let mut cursor = start;
        let mut coverage = AutotuneTransportCoverage::default();
        let mut mismatch = Duration::ZERO;
        for observation in &self.physical_deltas {
            if observation.end <= cursor {
                continue;
            }
            if observation.start >= end {
                break;
            }
            let segment_start = observation.start.max(start);
            if segment_start > cursor {
                accumulate_autotune_transport_coverage(
                    &mut coverage,
                    &mut mismatch,
                    None,
                    expected,
                    segment_start.duration_since(cursor),
                );
                cursor = segment_start;
            }
            let segment_end = observation.end.min(end);
            if segment_end > cursor {
                accumulate_autotune_transport_coverage(
                    &mut coverage,
                    &mut mismatch,
                    observation.phase,
                    expected,
                    segment_end.duration_since(cursor),
                );
                cursor = segment_end;
            }
            if cursor >= end {
                break;
            }
        }
        if cursor < end {
            accumulate_autotune_transport_coverage(
                &mut coverage,
                &mut mismatch,
                None,
                expected,
                end.duration_since(cursor),
            );
        }
        Some(coverage)
    }

    fn coverage(
        &self,
        start: Instant,
        end: Instant,
        expected: (bool, bool),
    ) -> Option<AutotuneTransportCoverage> {
        if end <= start {
            return None;
        }
        let mut active = None;
        let mut future = Vec::new();
        for observation in &self.observations {
            if observation.at <= start {
                active = Some(observation.phase);
            } else if observation.at < end {
                future.push(*observation);
            } else {
                break;
            }
        }
        let mut phase = active?;
        let mut cursor = start;
        let mut coverage = AutotuneTransportCoverage::default();
        let mut mismatch = Duration::ZERO;
        for observation in future {
            accumulate_autotune_transport_coverage(
                &mut coverage,
                &mut mismatch,
                phase,
                expected,
                observation.at.duration_since(cursor),
            );
            cursor = observation.at;
            phase = observation.phase;
        }
        accumulate_autotune_transport_coverage(
            &mut coverage,
            &mut mismatch,
            phase,
            expected,
            end.duration_since(cursor),
        );
        Some(coverage)
    }
}

#[cfg(feature = "calibration")]
fn accumulate_autotune_transport_coverage(
    coverage: &mut AutotuneTransportCoverage,
    mismatch: &mut Duration,
    phase: Option<(bool, bool)>,
    expected: (bool, bool),
    elapsed: Duration,
) {
    coverage.total = coverage.total.saturating_add(elapsed);
    if phase == Some(expected) {
        coverage.matching = coverage.matching.saturating_add(elapsed);
        *mismatch = Duration::ZERO;
    } else {
        *mismatch = mismatch.saturating_add(elapsed);
        coverage.longest_mismatch = coverage.longest_mismatch.max(*mismatch);
    }
}

#[cfg(feature = "calibration")]
fn loaded_coverage_sufficient(coverage: AutotuneTransportCoverage) -> bool {
    coverage.total > Duration::ZERO
        && coverage.matching.as_nanos().saturating_mul(100)
            >= coverage
                .total
                .as_nanos()
                .saturating_mul(AUTOTUNE_TRANSPORT_MIN_LOADED_COVERAGE_PERCENT)
}

struct TransportProbeRuntime {
    requests: SyncSender<TransportProbeRequest>,
    results: Receiver<TransportProbeResult>,
    in_flight: bool,
    last_started: Instant,
    load_candidate: (bool, bool),
    load_candidate_since: Instant,
    #[cfg(feature = "calibration")]
    next_probe_id: u64,
    #[cfg(feature = "calibration")]
    capture_control: AutotuneTransportControl,
    #[cfg(feature = "calibration")]
    in_flight_capture: Option<AutotuneTransportFlight>,
    #[cfg(feature = "calibration")]
    pending_capture_result: Option<TransportProbeResult>,
    #[cfg(feature = "calibration")]
    pending_capture_wait_reason: Option<&'static str>,
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

/// Bind positive and negative transport outcomes separately.
///
/// A successful latency sample requires the route to remain online before
/// and after the whole probe.  A failed probe is already adverse evidence, so
/// it may retain the original route identity when the same device/source
/// identity became offline during the flight.  This lets an exact deadline
/// exhaustion remain censored negative evidence instead of being rewritten as
/// an untyped route-change result.  It never turns a successful sample from an
/// offline or failed-over route into latency evidence.
fn transport_probe_route_identities(
    before: Option<&RouteSnapshot>,
    after: Option<&RouteSnapshot>,
) -> (Option<String>, Option<String>) {
    let (Some(before), Some(after)) = (before, after) else {
        return (None, None);
    };
    if !before.online || before.stable_key() != after.stable_key() {
        return (None, None);
    }
    let failed = Some(before.stable_key());
    let successful = after.online.then(|| after.stable_key());
    (successful, failed)
}

#[cfg(feature = "calibration")]
fn censored_autotune_transport_observation(
    result: &TransportProbeResult,
    active_capture: Option<&operations::full_autotune::AutotuneCaptureRequest>,
    current_route_identity: Option<&str>,
) -> Result<Option<operations::autotune_capture::AutotuneCaptureObservationKind>, String> {
    if result.failure_kind != Some(transport_probe::TransportProbeFailureKind::DeadlineExceeded) {
        return Ok(None);
    }
    if result.error.is_none() || result.latency_ms.is_some() || result.failure_deadline_us.is_none()
    {
        return Err("typed transport deadline result is internally inconsistent".to_string());
    }
    let Some(active_capture) = active_capture else {
        return Ok(None);
    };
    if result.autotune_capture.as_ref() != Some(active_capture)
        || result.capture_interval_valid != Some(true)
        || !result.control_valid
        || !transport_result_matches_route(result.route_identity.as_deref(), current_route_identity)
        || !result.trusted
        || result.backend != "websocket"
    {
        return Ok(None);
    }
    operations::autotune_capture::transport_deadline_observation_kind(
        active_capture,
        result
            .failure_deadline_us
            .expect("typed deadline duration checked above"),
        result.dl_loaded,
        result.ul_loaded,
    )
}

fn uplink_error_code(state: UplinkState, reason: &str) -> Option<&'static str> {
    if state == UplinkState::Rechecking {
        return Some("route_rechecking");
    }
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
                #[cfg(feature = "calibration")]
                let started_at = Instant::now();
                let before = route_inspector.inspect();
                let measurement: Result<
                    transport_probe::TransportProbeSample,
                    transport_probe::TransportProbeFailure,
                > = match before.as_ref() {
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
                                Err(error) => {
                                    Err(transport_probe::TransportProbeFailure::other(error))
                                }
                            }
                        } else {
                            (|| -> Result<
                                transport_probe::TransportProbeSample,
                                transport_probe::TransportProbeFailure,
                            > {
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
                                        )
                                        .map_err(
                                            transport_probe::TransportProbeFailure::other,
                                        )?,
                                    ));
                                }
                                engine
                                    .as_mut()
                                    .ok_or_else(|| {
                                        transport_probe::TransportProbeFailure::other(
                                            "transport engine is unavailable".to_string(),
                                        )
                                    })?
                                    .1
                                    .probe_classified()
                            })()
                        }
                    }
                    Ok(snapshot) => Err(transport_probe::TransportProbeFailure::other(
                        if snapshot.reason.is_empty() {
                            "transport route is offline".to_string()
                        } else {
                            snapshot.reason.clone()
                        },
                    )),
                    Err(error) => Err(transport_probe::TransportProbeFailure::other(error.clone())),
                };
                let after = route_inspector.inspect();
                let (successful_route_identity, failed_route_identity) =
                    transport_probe_route_identities(before.as_ref().ok(), after.as_ref().ok());
                let (
                    latency_ms,
                    error,
                    _failure_kind,
                    _failure_deadline_us,
                    backend_name,
                    trusted,
                    raw_samples_ms,
                    discarded_samples,
                    server_processing_ms,
                    connection_reused,
                    result_route_identity,
                ) = match (measurement, successful_route_identity) {
                    (Ok(sample), Some(route_identity)) => (
                        Some(sample.rtt_ms),
                        None,
                        None,
                        None,
                        sample.backend.as_str().to_string(),
                        sample.trusted,
                        sample.raw_samples_ms,
                        sample.discarded_samples,
                        sample.server_processing_ms,
                        sample.connection_reused,
                        Some(route_identity),
                    ),
                    (Ok(_), None) => (
                        None,
                        Some("route changed during native transport probe".to_string()),
                        Some(transport_probe::TransportProbeFailureKind::Other),
                        None,
                        backend.as_str().to_string(),
                        false,
                        Vec::new(),
                        0,
                        0.0,
                        false,
                        None,
                    ),
                    (Err(failure), _) => {
                        let route_identity = failed_route_identity;
                        match route_identity {
                            Some(route_identity) => (
                                None,
                                Some(failure.message().to_string()),
                                Some(failure.kind()),
                                failure.deadline_us(),
                                backend.as_str().to_string(),
                                backend.trusted(),
                                Vec::new(),
                                0,
                                0.0,
                                false,
                                Some(route_identity),
                            ),
                            None => (
                                None,
                                Some("route changed during native transport probe".to_string()),
                                Some(transport_probe::TransportProbeFailureKind::Other),
                                None,
                                backend.as_str().to_string(),
                                false,
                                Vec::new(),
                                0,
                                0.0,
                                false,
                                None,
                            ),
                        }
                    }
                };
                #[cfg(feature = "calibration")]
                let completed_at = Instant::now();
                if result_tx
                    .send(TransportProbeResult {
                        #[cfg(feature = "calibration")]
                        probe_id: request.probe_id,
                        #[cfg(feature = "calibration")]
                        started_at,
                        #[cfg(feature = "calibration")]
                        completed_at,
                        control_valid: request.control_valid,
                        #[cfg(feature = "calibration")]
                        capture_interval_valid: None,
                        endpoint: endpoint.clone(),
                        dl_loaded: request.dl_loaded,
                        ul_loaded: request.ul_loaded,
                        rating_phase: request.rating_phase,
                        #[cfg(feature = "calibration")]
                        autotune_capture: request.autotune_capture,
                        latency_ms,
                        error,
                        #[cfg(feature = "calibration")]
                        failure_kind: _failure_kind,
                        #[cfg(feature = "calibration")]
                        failure_deadline_us: _failure_deadline_us,
                        route_identity: result_route_identity,
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
            #[cfg(feature = "calibration")]
            next_probe_id: 1,
            #[cfg(feature = "calibration")]
            capture_control: AutotuneTransportControl::default(),
            #[cfg(feature = "calibration")]
            in_flight_capture: None,
            #[cfg(feature = "calibration")]
            pending_capture_result: None,
            #[cfg(feature = "calibration")]
            pending_capture_wait_reason: None,
        }
    }

    #[cfg(feature = "calibration")]
    fn observe_autotune_capture_phase(
        &mut self,
        cfg: &Config,
        rates: Option<RateSample>,
        counter_delta: Option<operations::autotune_counter::AutotuneCounterDelta>,
        capture: Option<&operations::full_autotune::AutotuneCaptureRequest>,
        route_identity: Option<&str>,
        now: Instant,
    ) -> Result<(), String> {
        let (Some(capture), Some(route_identity)) = (capture, route_identity) else {
            self.capture_control.reset();
            return Ok(());
        };
        let key = AutotuneTransportCaptureKey::new(capture, route_identity);
        let phase = autotune_transport_control_phase(
            capture,
            rates,
            now,
            monitor_tick_timeout(cfg).saturating_mul(3),
            cfg.connection_active_thr_kbps,
            cfg.rating_capture_ack_ratio,
        )?;
        let expected = AutotuneTransportControl::expected_phase(capture)?;
        let dropout = autotune_transport_dropout(cfg);
        let history_window = Duration::from_secs(cfg.transport_probe_timeout_s)
            .saturating_add(Duration::from_secs_f64(cfg.transport_load_hold_s))
            .saturating_add(dropout)
            .saturating_add(monitor_tick_timeout(cfg).saturating_mul(3));
        self.capture_control
            .observe(key.clone(), phase, expected, now, dropout, history_window);
        if let Some(delta) = counter_delta {
            let delta_phase = autotune_transport_delta_phase(
                capture,
                delta,
                dropout,
                cfg.connection_active_thr_kbps,
                cfg.rating_capture_ack_ratio,
            )?;
            self.capture_control.observe_physical_delta(
                &key,
                delta,
                delta_phase,
                history_window,
            )?;
        }
        Ok(())
    }

    #[cfg(feature = "calibration")]
    fn drain(&mut self, controller: &mut Controller, cfg: &Config) {
        while let Ok(mut result) = self.results.try_recv() {
            self.in_flight = false;
            if result.autotune_capture.is_some() {
                if self.pending_capture_result.is_some() {
                    result.capture_interval_valid = Some(false);
                    controller.on_transport_probe(result);
                } else {
                    self.pending_capture_result = Some(result);
                    self.pending_capture_wait_reason = None;
                }
            } else {
                self.in_flight_capture = None;
                result.capture_interval_valid = None;
                controller.on_transport_probe(result);
            }
        }
        self.settle_pending_capture(controller, cfg);
    }

    #[cfg(feature = "calibration")]
    fn settle_pending_capture(&mut self, controller: &mut Controller, cfg: &Config) {
        let Some(result) = self.pending_capture_result.as_ref() else {
            return;
        };
        let settlement = self.in_flight_capture.as_ref().map_or(
            AutotuneTransportSettlement::Final(AutotuneTransportAttestation {
                valid: false,
                reason: "missing-flight",
                coverage: None,
            }),
            |flight| {
                self.capture_control.settle_diagnostic(
                    flight,
                    result,
                    autotune_transport_dropout(cfg),
                )
            },
        );
        let attestation = match settlement {
            AutotuneTransportSettlement::Pending(reason) => {
                if cfg.debug && self.pending_capture_wait_reason != Some(reason) {
                    controller.log(
                        "DEBUG",
                        &format!(
                            "native Auto-Tune transport result probe={} pending reason={reason}",
                            result.probe_id,
                        ),
                    );
                }
                self.pending_capture_wait_reason = Some(reason);
                return;
            }
            AutotuneTransportSettlement::Final(attestation) => attestation,
        };
        let mut result = self
            .pending_capture_result
            .take()
            .expect("pending capture result was checked above");
        self.in_flight_capture = None;
        self.pending_capture_wait_reason = None;
        result.capture_interval_valid = Some(attestation.valid);
        if cfg.debug {
            let coverage = attestation.coverage.unwrap_or_default();
            controller.log(
                "DEBUG",
                &format!(
                    "native Auto-Tune transport result probe={} valid={} reason={} total_ms={} matching_ms={} longest_mismatch_ms={}",
                    result.probe_id,
                    attestation.valid,
                    attestation.reason,
                    coverage.total.as_millis(),
                    coverage.matching.as_millis(),
                    coverage.longest_mismatch.as_millis(),
                ),
            );
        }
        controller.on_transport_probe(result);
    }

    #[cfg(feature = "calibration")]
    fn maybe_start(
        &mut self,
        cfg: &Config,
        rates: Option<RateSample>,
        shaper_rates: (f64, f64),
        rating: &RatingLoadSnapshot,
        quality_baseline_ready: bool,
        autotune_capture: Option<operations::full_autotune::AutotuneCaptureRequest>,
        route_identity: Option<&str>,
        now: Instant,
    ) -> Result<(), String> {
        let (dl_shaper, ul_shaper) = shaper_rates;
        let (phase, control_valid, capture_key) = match autotune_capture.as_ref() {
            Some(request) => {
                let Some(route_identity) = route_identity else {
                    return Ok(());
                };
                let key = AutotuneTransportCaptureKey::new(request, route_identity);
                let (phase, valid) = self.capture_control.ready_phase(
                    request,
                    &key,
                    now,
                    Duration::from_secs_f64(cfg.transport_load_hold_s),
                    autotune_transport_dropout(cfg),
                )?;
                (phase, valid, Some(key))
            }
            None => {
                let Some(rates) = rates else {
                    return Ok(());
                };
                let high_load_phase = (
                    percent(rates.dl_kbps, dl_shaper) >= cfg.high_load_thr * 100.0,
                    percent(rates.ul_kbps, ul_shaper) >= cfg.high_load_thr * 100.0,
                );
                if high_load_phase != self.load_candidate {
                    self.load_candidate = high_load_phase;
                    self.load_candidate_since = now;
                }
                let raw_loaded = high_load_phase.0 || high_load_phase.1;
                let valid = !raw_loaded
                    || now.duration_since(self.load_candidate_since)
                        >= Duration::from_secs_f64(cfg.transport_load_hold_s);
                (high_load_phase, valid, None)
            }
        };
        let raw_loaded = phase.0 || phase.1;
        if self.in_flight
            || self.pending_capture_result.is_some()
            || !transport_probe_control_allows_start(
                autotune_capture.is_some(),
                raw_loaded,
                control_valid,
                rating.phase.loaded(),
            )
        {
            return Ok(());
        }
        let dl_loaded = control_valid && phase.0;
        let ul_loaded = control_valid && phase.1;
        let loaded = dl_loaded || ul_loaded;
        let any_loaded = loaded || rating.phase.loaded();
        let interval = transport_probe_interval_s(cfg, any_loaded, quality_baseline_ready);
        if self.last_started.elapsed() < Duration::from_secs_f64(interval) {
            return Ok(());
        }

        let probe_id = self.next_probe_id;
        let Some(next_probe_id) = self.next_probe_id.checked_add(1) else {
            return Err("native transport probe sequence exhausted".to_string());
        };
        let request_capture_present = autotune_capture.is_some();
        if self
            .requests
            .try_send(TransportProbeRequest {
                probe_id,
                control_valid,
                dl_loaded,
                ul_loaded,
                rating_phase: rating.phase,
                autotune_capture,
            })
            .is_ok()
        {
            self.next_probe_id = next_probe_id;
            self.in_flight = true;
            self.last_started = now;
            self.in_flight_capture = capture_key.map(|key| AutotuneTransportFlight {
                probe_id,
                key,
                expected_phase: phase,
                control_valid,
                submitted_at: now,
                physical_delta_required: phase != (false, false),
            });
            debug_assert_eq!(request_capture_present, self.in_flight_capture.is_some());
        }
        Ok(())
    }

    #[cfg(not(feature = "calibration"))]
    fn drain(&mut self, controller: &mut Controller, _cfg: &Config) {
        while let Ok(result) = self.results.try_recv() {
            self.in_flight = false;
            controller.on_transport_probe(result);
        }
    }

    #[cfg(not(feature = "calibration"))]
    fn maybe_start(
        &mut self,
        cfg: &Config,
        rates: Option<RateSample>,
        shaper_rates: (f64, f64),
        rating: &RatingLoadSnapshot,
        quality_baseline_ready: bool,
        now: Instant,
    ) -> Result<(), String> {
        let Some(rates) = rates else {
            return Ok(());
        };
        let (dl_shaper, ul_shaper) = shaper_rates;
        let phase = (
            percent(rates.dl_kbps, dl_shaper) >= cfg.high_load_thr * 100.0,
            percent(rates.ul_kbps, ul_shaper) >= cfg.high_load_thr * 100.0,
        );
        if phase != self.load_candidate {
            self.load_candidate = phase;
            self.load_candidate_since = now;
        }
        let raw_loaded = phase.0 || phase.1;
        let control_valid = !raw_loaded
            || now.duration_since(self.load_candidate_since)
                >= Duration::from_secs_f64(cfg.transport_load_hold_s);
        if self.in_flight
            || !transport_probe_control_allows_start(
                false,
                raw_loaded,
                control_valid,
                rating.phase.loaded(),
            )
        {
            return Ok(());
        }
        let loaded = control_valid && raw_loaded;
        let interval = transport_probe_interval_s(
            cfg,
            loaded || rating.phase.loaded(),
            quality_baseline_ready,
        );
        if self.last_started.elapsed() < Duration::from_secs_f64(interval) {
            return Ok(());
        }
        if self
            .requests
            .try_send(TransportProbeRequest {
                control_valid,
                dl_loaded: control_valid && phase.0,
                ul_loaded: control_valid && phase.1,
                rating_phase: rating.phase,
            })
            .is_ok()
        {
            self.in_flight = true;
            self.last_started = now;
        }
        Ok(())
    }
}

#[cfg(feature = "calibration")]
fn autotune_transport_dropout(cfg: &Config) -> Duration {
    let hold = Duration::from_secs_f64(cfg.transport_load_hold_s);
    monitor_tick_timeout(cfg)
        .saturating_mul(3)
        .min(hold.div_f64(2.0))
}

fn transport_probe_control_allows_start(
    autotune_capture_present: bool,
    raw_loaded: bool,
    control_valid: bool,
    rating_loaded: bool,
) -> bool {
    if autotune_capture_present {
        // A native Auto-Tune capture is an exclusive measurement contract.
        // Its topology-specific phase and hold must be valid in their own
        // right; the concurrent rating state must never bypass that gate.
        control_valid
    } else {
        !raw_loaded || control_valid || rating_loaded
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

#[cfg(feature = "calibration")]
fn autotune_capture_attestation_lease_valid(
    last_attested: Instant,
    now: Instant,
    attested: &operations::full_autotune::AutotuneCaptureRequest,
    published: Option<&operations::full_autotune::AutotuneCaptureRequest>,
    current_boot_ms: u64,
) -> bool {
    now.checked_duration_since(last_attested)
        .is_some_and(|age| age < AUTOTUNE_CAPTURE_ATTESTATION_MAX_AGE)
        && published == Some(attested)
        && current_boot_ms <= attested.deadline_boot_ms
}

struct Controller {
    cfg: Config,
    log: Option<LogFile>,
    rate_monitor: RateMonitor,
    #[cfg(feature = "calibration")]
    autotune_counter_sampler: Option<operations::autotune_counter::AutotuneCounterSampler>,
    #[cfg(feature = "calibration")]
    autotune_speedtest_rate_monitor: Option<AutotuneSpeedtestRateMonitor>,
    #[cfg(feature = "calibration")]
    autotune_capture_rate_sample: Option<IdentityBoundRateSample>,
    #[cfg(feature = "calibration")]
    autotune_capture_counter_delta: Option<IdentityBoundCounterDelta>,
    #[cfg(feature = "calibration")]
    autotune_idle_rate_reference: Option<IdentityBoundIdleRateReference>,
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
    rating_load_observed_unix_ms: u64,
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
    #[cfg(feature = "calibration")]
    runtime_generation: u64,
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
    sqm_last_recovery_at: Option<f64>,
    sqm_recovery_gate: operations::sqm_recovery::SqmRecoveryGate,
    runtime_override_active: bool,
    /// A durable calibration owner is live even before a temporary topology
    /// is applied. Holding ordinary controller writes while the driver waits
    /// for its first control closes the permit-to-idle-capture rate race
    /// without changing the managed qdisc merely to freeze it.
    runtime_operation_active: bool,
    #[cfg(feature = "calibration")]
    autotune_capture_request: Option<operations::full_autotune::AutotuneCaptureRequest>,
    #[cfg(feature = "calibration")]
    autotune_capture_session: operations::autotune_capture_session::AutotuneCaptureSession,
    #[cfg(feature = "calibration")]
    autotune_capture_error: Option<String>,
    #[cfg(feature = "calibration")]
    autotune_capture_last_attestation: Instant,
    dl_qdisc_kind: Option<CakeQdiscKind>,
    ul_qdisc_kind: Option<CakeQdiscKind>,
    last_status: Option<StatusSnapshot>,
    last_status_publish: Instant,
    #[cfg(feature = "calibration")]
    rating_runtime_faulted: bool,
    #[cfg(feature = "calibration")]
    last_rejected_rating_capture_token: Option<String>,
}

#[cfg(test)]
fn loaded_autotune_traffic_observation_after_read<E, F>(
    request: &operations::full_autotune::AutotuneCaptureRequest,
    evidence: &E,
    monotonic_now: F,
) -> Result<operations::autotune_capture::AutotuneCaptureObservationKind, String>
where
    E: operations::full_autotune::AutotuneLoadProof + ?Sized,
    F: FnOnce() -> Result<u64, String>,
{
    let current_boot_ms = monotonic_now().map_err(|error| {
        format!(
            "unable to sample monotonic time after reading native Auto-Tune load evidence: {error}"
        )
    })?;
    operations::autotune_capture::loaded_traffic_observation_kind(
        request,
        evidence,
        current_boot_ms,
    )
}

#[cfg(test)]
fn reject_autotune_capture_after_io_with_clock<F>(
    accumulator: &mut operations::autotune_capture::AutotuneCaptureAccumulator,
    diagnostic_code: &str,
    monotonic_now: F,
) -> Option<operations::full_autotune::AutotuneCaptureSnapshot>
where
    F: FnOnce() -> Result<u64, String>,
{
    let rejection_boot_ms = monotonic_now().ok()?;
    accumulator
        .reject_current(diagnostic_code, rejection_boot_ms)
        .ok()
}

#[cfg(feature = "calibration")]
fn rating_capture_request_is_admissible(
    token: &str,
    mode: &str,
    deadline: Option<f64>,
    now_epoch: f64,
) -> bool {
    operations::rating::capture_job_id_is_valid(token)
        && matches!(mode, "automatic" | "client")
        && now_epoch.is_finite()
        && deadline
            .map(|value| value.is_finite() && value > now_epoch)
            .unwrap_or(false)
}

impl Controller {
    fn runtime_rate_control_suspended(&self) -> bool {
        self.runtime_override_active || self.runtime_operation_active
    }

    fn remember_attested_cake_rates(
        &mut self,
        snapshot: &operations::autotune_runtime::RuntimeSnapshot,
    ) {
        if let Some(rate) = snapshot.download_kbps {
            self.last_set_dl = rate;
        }
        if let Some(rate) = snapshot.upload_kbps {
            self.last_set_ul = rate;
        }
    }

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
        #[cfg(feature = "calibration")]
        let runtime_generation = operations::identity::ProcessIdentity::current()?.starttime_ticks;
        let rating_load = RatingLoadDetector::new(now);
        let rating_load_snapshot = rating_load.snapshot(
            now,
            cfg.rating_load_config(),
            cfg.base_dl_shaper_rate_kbps,
            cfg.base_ul_shaper_rate_kbps,
        );
        let rating_load_observed_unix_ms = (epoch_secs() * 1000.0).round() as u64;
        let adaptive_dl_safe = if cfg.adaptive_ceiling_enabled
            && cfg.adaptive_ceiling_dl_evidence == "legacy_unverified"
        {
            cfg.base_dl_shaper_rate_kbps
                .min(cfg.max_dl_shaper_rate_kbps)
        } else if cfg.adaptive_ceiling_enabled {
            cfg.adaptive_ceiling_dl_safe_kbps
        } else {
            cfg.max_dl_shaper_rate_kbps
        };
        let adaptive_ul_safe = if cfg.adaptive_ceiling_enabled
            && cfg.adaptive_ceiling_ul_evidence == "legacy_unverified"
        {
            cfg.base_ul_shaper_rate_kbps
                .min(cfg.max_ul_shaper_rate_kbps)
        } else if cfg.adaptive_ceiling_enabled {
            cfg.adaptive_ceiling_ul_safe_kbps
        } else {
            cfg.max_ul_shaper_rate_kbps
        };
        let adaptive_dl = AdaptiveCeilingDirection::new_with_verified_safe(
            cfg.max_dl_shaper_rate_kbps,
            cfg.adaptive_ceiling_dl_cap_kbps,
            adaptive_dl_safe,
        );
        let adaptive_ul = AdaptiveCeilingDirection::new_with_verified_safe(
            cfg.max_ul_shaper_rate_kbps,
            cfg.adaptive_ceiling_ul_cap_kbps,
            adaptive_ul_safe,
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
            rating_load_observed_unix_ms,
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
            #[cfg(feature = "calibration")]
            runtime_generation,
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
            sqm_last_recovery_at: None,
            sqm_recovery_gate: operations::sqm_recovery::SqmRecoveryGate::default(),
            runtime_override_active: false,
            runtime_operation_active: false,
            #[cfg(feature = "calibration")]
            autotune_capture_request: None,
            #[cfg(feature = "calibration")]
            autotune_capture_session:
                operations::autotune_capture_session::AutotuneCaptureSession::new(),
            #[cfg(feature = "calibration")]
            autotune_capture_error: None,
            #[cfg(feature = "calibration")]
            autotune_capture_last_attestation: now
                .checked_sub(AUTOTUNE_CAPTURE_ATTESTATION_MAX_AGE)
                .unwrap_or(now),
            dl_qdisc_kind: None,
            ul_qdisc_kind: None,
            last_status: None,
            last_status_publish: now.checked_sub(STATUS_PUBLISH_INTERVAL).unwrap_or(now),
            #[cfg(feature = "calibration")]
            rating_runtime_faulted: false,
            #[cfg(feature = "calibration")]
            last_rejected_rating_capture_token: None,
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
            #[cfg(feature = "calibration")]
            autotune_counter_sampler: None,
            #[cfg(feature = "calibration")]
            autotune_speedtest_rate_monitor: None,
            #[cfg(feature = "calibration")]
            autotune_capture_rate_sample: None,
            #[cfg(feature = "calibration")]
            autotune_capture_counter_delta: None,
            #[cfg(feature = "calibration")]
            autotune_idle_rate_reference: None,
            cpu_monitor,
        })
    }

    fn start(&mut self) {
        self.log("INFO", "starting cake-autorate-rs");
        self.apply_shaper("dl");
        self.apply_shaper("ul");
    }

    fn sample_rates(&mut self) -> RateSample {
        let shaped = self.rate_monitor.sample();
        #[cfg(feature = "calibration")]
        self.update_autotune_capture_rates(shaped);
        shaped
    }

    #[cfg(feature = "calibration")]
    fn invalidate_autotune_counter_sampler(&mut self) {
        let failed = self
            .autotune_counter_sampler
            .as_mut()
            .is_some_and(|sampler| {
                sampler.invalidate_current();
                sampler.try_take().is_err()
            });
        if failed {
            self.autotune_counter_sampler = None;
        }
    }

    #[cfg(feature = "calibration")]
    fn update_autotune_capture_rates(&mut self, shaped: RateSample) {
        self.autotune_capture_rate_sample = None;
        self.autotune_capture_counter_delta = None;
        // Phase tracking is scheduling evidence, not an admitted measurement.
        // Keep it bound to the already admitted capture identity while a slow
        // periodic re-attestation is in progress; otherwise one synthetic
        // missing rate sample after every lease lapse resets the three-second
        // transport hold forever.  Every ICMP, transport, traffic, and CPU
        // observation still passes through active_autotune_observation_request
        // and therefore remains fail-closed on the fresh attestation lease.
        let Some(request) = self.autotune_capture_control_request() else {
            self.invalidate_autotune_counter_sampler();
            self.autotune_speedtest_rate_monitor = None;
            return;
        };
        if !autotune_capture_uses_identity_bound_speedtest_counters(&request) {
            self.invalidate_autotune_counter_sampler();
            self.autotune_speedtest_rate_monitor = None;
            self.autotune_capture_rate_sample = Some(IdentityBoundRateSample {
                request,
                sample: shaped,
            });
            return;
        }

        if self.autotune_counter_sampler.is_none() {
            match operations::autotune_counter::AutotuneCounterSampler::new() {
                Ok(sampler) => self.autotune_counter_sampler = Some(sampler),
                Err(error) => {
                    self.reject_autotune_observations(
                        "capture-speedtest-counters-unavailable",
                        &format!("native Auto-Tune cannot start its counter worker: {error}"),
                    );
                    return;
                }
            }
        }

        let monitor_matches = self
            .autotune_speedtest_rate_monitor
            .as_ref()
            .is_some_and(|monitor| monitor.matches(&request));
        if !monitor_matches {
            let sampler = self
                .autotune_counter_sampler
                .as_mut()
                .expect("counter sampler was initialized");
            sampler.invalidate_current();
            self.autotune_speedtest_rate_monitor = Some(AutotuneSpeedtestRateMonitor {
                request: request.clone(),
                counter_epoch: sampler.current_epoch(),
                monitor: SpeedtestCounterRateMonitor::new(
                    self.cfg.monitor_achieved_rates_interval_ms,
                ),
            });
        }

        let completion = match self
            .autotune_counter_sampler
            .as_mut()
            .expect("counter sampler was initialized")
            .try_take()
        {
            Ok(result) => result,
            Err(error) => {
                self.reject_autotune_observations(
                    "capture-speedtest-counters-unavailable",
                    &format!("native Auto-Tune counter worker failed: {error}"),
                );
                return;
            }
        };
        let mut newest_controlled = None;
        if let Some(completion) = completion {
            let completion_matches =
                self.autotune_speedtest_rate_monitor
                    .as_ref()
                    .is_some_and(|monitor| {
                        autotune_counter_completion_matches(&request, monitor, &completion)
                    });
            if completion_matches {
                if !autotune_counter_completion_is_timely(&request, &completion) {
                    self.autotune_speedtest_rate_monitor
                        .as_mut()
                        .expect("speedtest rate monitor was initialized")
                        .monitor
                        .reset(completion.completed_at);
                    self.reject_autotune_observations(
                        "capture-speedtest-counters-expired",
                        "native Auto-Tune speedtest counters completed outside the request deadline",
                    );
                    return;
                }
                match completion.outcome {
                    Ok(counters) => {
                        let (rate, delta) = self
                            .autotune_speedtest_rate_monitor
                            .as_mut()
                            .expect("speedtest rate monitor was initialized")
                            .monitor
                            .observe_counters_with_delta(completion.completed_at, counters);
                        newest_controlled = rate;
                        if let Some(delta) = delta {
                            self.autotune_capture_counter_delta = Some(IdentityBoundCounterDelta {
                                request: request.clone(),
                                delta,
                            });
                        }
                    }
                    Err(error) => {
                        self.autotune_speedtest_rate_monitor
                            .as_mut()
                            .expect("speedtest rate monitor was initialized")
                            .monitor
                            .reset(completion.completed_at);
                        self.reject_autotune_observations(
                            "capture-speedtest-counters-unavailable",
                            &format!(
                                "native Auto-Tune cannot sample identity-bound speedtest counters: {error}"
                            ),
                        );
                        return;
                    }
                }
            }
        }

        let counter_read_due = self
            .autotune_speedtest_rate_monitor
            .as_ref()
            .expect("speedtest rate monitor was initialized")
            .monitor
            .sample_due(Instant::now());
        if counter_read_due {
            if let Err(error) = self
                .autotune_counter_sampler
                .as_mut()
                .expect("counter sampler was initialized")
                .try_schedule(&request)
            {
                self.reject_autotune_observations(
                    "capture-speedtest-counters-unavailable",
                    &format!("native Auto-Tune cannot schedule its counter worker: {error}"),
                );
                return;
            }
        }

        let controlled = newest_controlled.or_else(|| {
            self.autotune_speedtest_rate_monitor
                .as_ref()
                .expect("speedtest rate monitor was initialized")
                .monitor
                .cached_sample()
        });
        let Some(controlled) = controlled else {
            return;
        };
        match select_autotune_capture_rates(request.topology, Some(controlled)) {
            Ok(sample) => {
                self.autotune_capture_rate_sample =
                    Some(IdentityBoundRateSample { request, sample })
            }
            Err(error) => {
                self.reject_autotune_observations("capture-speedtest-counters-unavailable", &error)
            }
        }
    }

    #[cfg(feature = "calibration")]
    fn autotune_observation_rates(&self, shaped: RateSample) -> Option<RateSample> {
        let Some(request) = self.autotune_capture_request.as_ref() else {
            return Some(shaped);
        };
        let now = Instant::now();
        let max_age = monitor_tick_timeout(&self.cfg).saturating_mul(3);
        self.autotune_capture_rate_sample
            .as_ref()
            .filter(|sample| &sample.request == request)
            .map(|sample| sample.sample)
            .filter(|sample| rate_sample_is_recent(*sample, now, max_age))
    }

    #[cfg(feature = "calibration")]
    fn autotune_transport_counter_delta(
        &self,
    ) -> Option<operations::autotune_counter::AutotuneCounterDelta> {
        let request = self.autotune_capture_control_request()?;
        self.autotune_capture_counter_delta
            .as_ref()
            .filter(|sample| sample.request == request)
            .map(|sample| sample.delta)
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
        self.sqm_recovery_gate.observe_healthy();
        self.rate_monitor = RateMonitor::new(
            &self.cfg.rx_bytes_path,
            &self.cfg.tx_bytes_path,
            self.cfg.monitor_achieved_rates_interval_ms,
        )
        .map_err(|error| format!("failed to reset interface counters: {error}"))?;
        #[cfg(feature = "calibration")]
        {
            self.invalidate_autotune_counter_sampler();
            self.autotune_speedtest_rate_monitor = None;
            self.autotune_capture_rate_sample = None;
            self.autotune_idle_rate_reference = None;
        }
        self.last_set_dl = 0;
        self.last_set_ul = 0;
        self.dl_qdisc_kind = None;
        self.ul_qdisc_kind = None;
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
        use operations::sqm_recovery::SqmRecoveryAdmission;

        #[cfg(feature = "calibration")]
        if self.runtime_rate_control_suspended() {
            self.set_sqm_runtime_status(
                "WAITING_OPERATION",
                true,
                "runtime topology is owned by native Auto-Tune",
            );
            return (true, false);
        }
        if !self.cfg.manage_sqm || !self.cfg.sqm_enabled {
            self.sqm_recovery_gate.observe_healthy();
            self.set_sqm_runtime_status("UNMANAGED", true, "");
            return (true, false);
        }

        if !managed_sqm_target_ready(&self.cfg) {
            self.sqm_recovery_gate.observe_target_missing();
            let reason = format!(
                "target interface {} is unavailable; waiting for link and native SQM hotplug",
                self.cfg.sqm_interface
            );
            self.set_sqm_runtime_status("WAITING_LINK", false, &reason);
            self.set_run_state("WAITING_LINK");
            return (false, false);
        }

        let topology = inspect_sqm_topology(&self.cfg);
        match &topology {
            Ok(()) if self.sqm_runtime_healthy => {
                self.sqm_recovery_gate.observe_healthy();
                (true, false)
            }
            _ => {
                let initial_reason = topology
                    .as_ref()
                    .err()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "managed SQM failed exact attestation".to_string());
                match attest_managed_sqm(&self.cfg) {
                    Ok(()) if inspect_sqm_topology(&self.cfg).is_ok() => {
                        match self.accept_recovered_sqm("runtime recovered externally") {
                            Ok(()) => (true, true),
                            Err(error) => {
                                self.set_sqm_runtime_status("ERROR", false, &error);
                                self.set_run_state("ERROR");
                                (false, false)
                            }
                        }
                    }
                    Ok(()) => {
                        let reason = format!(
                            "{initial_reason}; exact SQM check observed a concurrent topology transition; re-reading current state"
                        );
                        self.set_sqm_runtime_status("WAITING_SQM", false, &reason);
                        self.set_run_state("WAITING_SQM");
                        (false, false)
                    }
                    Err(SqmRecoveryError::Busy(error)) => {
                        let reason =
                            format!("{initial_reason}; waiting for SQM operation: {error}");
                        self.set_sqm_runtime_status("WAITING_OPERATION", false, &reason);
                        self.set_run_state("WAITING_OPERATION");
                        (false, false)
                    }
                    Err(SqmRecoveryError::Terminated) => {
                        self.set_sqm_runtime_status(
                            "STOPPING",
                            false,
                            "termination requested while observing SQM recovery ownership",
                        );
                        self.set_run_state("STOPPING");
                        (false, false)
                    }
                    Err(SqmRecoveryError::Failed(check_error)) => {
                        let generation = sqm_topology_generation(&self.cfg, &topology);
                        if self.sqm_recovery_gate.admission(&generation)
                            == SqmRecoveryAdmission::WaitForStateChange
                        {
                            let reason = format!(
                                "{initial_reason}; previous recovery failed and the observed link/SQM topology is unchanged: {check_error}"
                            );
                            self.set_sqm_runtime_status("WAITING_SQM", false, &reason);
                            self.set_run_state("WAITING_SQM");
                            return (false, false);
                        }

                        self.sqm_recovery_attempts = self.sqm_recovery_attempts.saturating_add(1);
                        let reason = format!(
                            "{initial_reason}; exact read-only check is unhealthy: {check_error}"
                        );
                        self.set_sqm_runtime_status("RECOVERING", false, &reason);
                        self.set_run_state("RECOVERING");
                        match recover_managed_sqm(&self.cfg) {
                            Ok(()) => {
                                match self.accept_recovered_sqm("automatic SQM recovery completed")
                                {
                                    Ok(()) => (true, true),
                                    Err(error) => {
                                        self.set_sqm_runtime_status("ERROR", false, &error);
                                        self.set_run_state("ERROR");
                                        (false, false)
                                    }
                                }
                            }
                            Err(SqmRecoveryError::Busy(error)) => {
                                let reason =
                                    format!("{initial_reason}; recovery deferred: {error}");
                                self.set_sqm_runtime_status("WAITING_OPERATION", false, &reason);
                                self.set_run_state("WAITING_OPERATION");
                                (false, false)
                            }
                            Err(SqmRecoveryError::Terminated) => {
                                self.set_sqm_runtime_status(
                                    "STOPPING",
                                    false,
                                    "termination requested during SQM recovery",
                                );
                                self.set_run_state("STOPPING");
                                (false, false)
                            }
                            Err(error @ SqmRecoveryError::Failed(_)) => {
                                let post_topology = inspect_sqm_topology(&self.cfg);
                                let failed_generation =
                                    sqm_topology_generation(&self.cfg, &post_topology);
                                self.sqm_recovery_gate.record_failed(failed_generation);
                                let reason = format!(
                                    "{initial_reason}; recovery failed and is blocked until observed topology changes: {}",
                                    error.message()
                                );
                                self.set_sqm_runtime_status("ERROR", false, &reason);
                                self.set_run_state("ERROR");
                                (false, false)
                            }
                        }
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
        let was_contaminated = self.rating_load_snapshot.capture_contaminated;
        self.rating_load_snapshot = self.rating_load.observe(
            now,
            dl_rate,
            ul_rate,
            self.shaper_dl,
            self.shaper_ul,
            self.cfg.rating_load_config(),
        );
        if !was_contaminated && self.rating_load_snapshot.capture_contaminated {
            self.log(
                "ERROR",
                &format!(
                    "Rating capture contaminated: reason={}, requested_phase={}, effective_dl_kbps={:.3}, effective_ul_kbps={:.3}, background_dl_kbps={:.3}, background_ul_kbps={:.3}",
                    self.rating_load_snapshot.capture_contamination_reason,
                    self.rating_load_snapshot.capture_requested_phase,
                    self.rating_load_snapshot.effective_dl_rate_kbps,
                    self.rating_load_snapshot.effective_ul_rate_kbps,
                    self.rating_load_snapshot.capture_background_dl_kbps,
                    self.rating_load_snapshot.capture_background_ul_kbps,
                ),
            );
        }
        self.rating_load_observed_unix_ms = (epoch_secs() * 1000.0).round() as u64;
        self.rating_load_snapshot.clone()
    }

    #[cfg(feature = "calibration")]
    fn refresh_rating_load_snapshot(&mut self, now: Instant) -> RatingLoadSnapshot {
        self.rating_load_snapshot = self.rating_load.snapshot(
            now,
            self.cfg.rating_load_config(),
            self.shaper_dl,
            self.shaper_ul,
        );
        self.rating_load_observed_unix_ms = (epoch_secs() * 1000.0).round() as u64;
        self.rating_load_snapshot.clone()
    }

    #[cfg(feature = "calibration")]
    fn sync_rating_capture(&mut self, now: Instant) {
        let capture_path = self.cfg.rating_capture_path();
        let content = match fs::read_to_string(&capture_path) {
            Ok(content) => content,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(error) => {
                self.log(
                    "ERROR",
                    &format!("unable to inspect rating capture request: {error}"),
                );
                return;
            }
        };
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
        if rating_capture_request_is_admissible(token, mode, deadline, epoch_secs()) {
            self.last_rejected_rating_capture_token = None;
            let capture_started = self.rating_load.set_capture(
                Some(token),
                Some(mode),
                requested_phase,
                background_dl_kbps,
                background_ul_kbps,
                now,
            );
            if capture_started {
                let (job_id, generation) = self
                    .rating_load
                    .capture_identity()
                    .map(|(job_id, generation)| (job_id.to_string(), generation))
                    .expect("a started rating capture has an identity");
                self.quality_grade
                    .begin_capture(epoch_secs(), &job_id, generation);
            }
        } else {
            if !token.is_empty() && !operations::rating::capture_job_id_is_valid(token) {
                let rejection_key = if token.len() <= 64 {
                    token.to_string()
                } else {
                    "oversized".to_string()
                };
                if self.last_rejected_rating_capture_token.as_deref()
                    != Some(rejection_key.as_str())
                {
                    self.log(
                        "ERROR",
                        "rejected a rating capture request with an invalid job identity",
                    );
                    self.last_rejected_rating_capture_token = Some(rejection_key);
                }
            }
            if self.rating_load.capture_active() {
                let outcome = if self.rating_load.capture_contaminated() {
                    self.quality_grade.cancel_capture();
                    "contaminated"
                } else if content.is_empty() {
                    self.quality_grade.end_capture(epoch_secs());
                    "removed"
                } else {
                    self.quality_grade.end_capture(epoch_secs());
                    "expired"
                };
                let finalized = self.rating_load.finalize_capture_with_outcome(outcome, now);
                debug_assert!(finalized);
            }
            if !content.is_empty() && operations::rating::capture_job_id_is_valid(token) {
                let _ = operations::rating::remove_matching_capture(&capture_path, token);
            }
        }
        self.refresh_rating_load_snapshot(now);
    }

    #[cfg(feature = "calibration")]
    fn sync_autotune_capture_admission(&mut self) {
        let request_path = self.cfg.autotune_capture_request_path();
        let snapshot_path = self.cfg.autotune_capture_snapshot_path();
        match fs::symlink_metadata(&request_path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if self.autotune_capture_request.take().is_some() {
                    let _ = fs::remove_file(&snapshot_path);
                }
                self.autotune_capture_session.clear();
                self.autotune_idle_rate_reference = None;
                self.autotune_capture_error = None;
                return;
            }
            Err(error) => {
                self.autotune_capture_request = None;
                self.autotune_capture_session.clear();
                self.autotune_idle_rate_reference = None;
                self.record_autotune_capture_error(format!(
                    "unable to inspect native Auto-Tune capture request: {error}"
                ));
                return;
            }
            Ok(_) => {}
        }
        let request = match operations::full_autotune::read_capture_request(&request_path) {
            Ok(request) => request,
            Err(error) => {
                self.autotune_capture_request = None;
                self.autotune_capture_session.clear();
                self.autotune_idle_rate_reference = None;
                self.record_autotune_capture_error(error);
                return;
            }
        };
        if self.autotune_capture_request.as_ref() != Some(&request) {
            self.autotune_idle_rate_reference = None;
        }
        if self.autotune_capture_request.as_ref() == Some(&request)
            && self
                .autotune_idle_rate_reference
                .as_ref()
                .is_some_and(|reference| reference.request == request)
            && fs::symlink_metadata(&snapshot_path).is_ok()
            && self.autotune_capture_last_attestation.elapsed() < AUTOTUNE_CAPTURE_REATTEST_INTERVAL
            && self.autotune_capture_error.is_none()
        {
            return;
        }
        let boot_ms = match operations::identity::monotonic_boot_ms() {
            Ok(value) => value,
            Err(error) => {
                self.record_autotune_capture_error(error);
                return;
            }
        };
        let admission = self.attest_autotune_capture(&request, boot_ms);
        let (snapshot, idle_rate_reference) = match admission {
            Ok(idle_rate_reference) => {
                match self.autotune_capture_session.admit(&request, boot_ms) {
                    Ok(_) => {
                        if let Err(error) = self.consume_autotune_load_evidence(&request) {
                            let rejected = operations::identity::monotonic_boot_ms().ok().and_then(
                                |rejection_boot_ms| {
                                    self.autotune_capture_session
                                        .reject("capture-load-evidence-invalid", rejection_boot_ms)
                                        .ok()
                                },
                            );
                            self.record_autotune_capture_error(error);
                            match rejected {
                                Some(snapshot) => (snapshot, None),
                                None => return,
                            }
                        } else {
                            match self.autotune_capture_session.snapshot() {
                                Ok(snapshot) => (snapshot, Some(idle_rate_reference)),
                                Err(error) => {
                                    self.autotune_capture_session.clear();
                                    self.autotune_idle_rate_reference = None;
                                    self.record_autotune_capture_error(error);
                                    return;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        self.autotune_capture_session.clear();
                        self.autotune_idle_rate_reference = None;
                        self.record_autotune_capture_error(error);
                        return;
                    }
                }
            }
            Err((code, detail)) => {
                self.autotune_capture_session.clear();
                self.autotune_idle_rate_reference = None;
                self.record_autotune_capture_error(detail);
                (
                    operations::full_autotune::AutotuneCaptureSnapshot {
                        request: request.clone(),
                        state: operations::full_autotune::AutotuneCaptureState::Rejected,
                        updated_boot_ms: boot_ms.min(request.deadline_boot_ms),
                        icmp_samples: 0,
                        transport_samples: 0,
                        transport_timeout_count: 0,
                        transport_timeout_total_us: 0,
                        transport_censored: false,
                        cpu_samples: 0,
                        idle_median_us: None,
                        idle_p95_us: None,
                        idle_transport_baseline_us: None,
                        icmp_delta_us: None,
                        transport_delta_us: None,
                        loss_ppm: None,
                        cpu_milli_percent: None,
                        background_confidence_percent: None,
                        contaminated: false,
                        diagnostic_code: Some(code.to_string()),
                    },
                    None,
                )
            }
        };
        if let Err(error) =
            operations::full_autotune::publish_capture_snapshot(&snapshot_path, &snapshot)
        {
            self.record_autotune_capture_error(error);
            return;
        }
        // Start the lease only after the synchronous identity/SQM checks and
        // atomic snapshot publication have completed.  On a busy low-end
        // router those operations can exceed the short lease duration; using
        // the entry timestamp would publish a lease that was already stale.
        self.autotune_capture_last_attestation = Instant::now();
        let capture_accepts_reference = matches!(
            snapshot.state,
            operations::full_autotune::AutotuneCaptureState::Collecting
                | operations::full_autotune::AutotuneCaptureState::Complete
        );
        self.autotune_capture_request = Some(request);
        self.autotune_idle_rate_reference = capture_accepts_reference
            .then_some(idle_rate_reference)
            .flatten()
            .map(
                |(download_kbps, upload_kbps)| IdentityBoundIdleRateReference {
                    request: self
                        .autotune_capture_request
                        .as_ref()
                        .expect("capture request was just published")
                        .clone(),
                    download_kbps: download_kbps as f64,
                    upload_kbps: upload_kbps as f64,
                },
            );
        if capture_accepts_reference {
            self.autotune_capture_error = None;
        }
    }

    #[cfg(feature = "calibration")]
    fn consume_autotune_load_evidence(
        &mut self,
        request: &operations::full_autotune::AutotuneCaptureRequest,
    ) -> Result<(), String> {
        if request.phase != operations::full_autotune::AutotuneCapturePhase::LoadedMeasurement {
            return Ok(());
        }
        let path = self.cfg.autotune_load_evidence_path();
        let evidence = match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(format!(
                    "unable to inspect native Auto-Tune load evidence: {error}"
                ))
            }
            Ok(_) => operations::full_autotune::read_controlled_load_evidence(&path)?,
        };
        // The evidence writer samples `published_boot_ms` before its atomic
        // publication.  Sample the reader's clock only after that publication
        // has been opened and decoded: the loop-start timestamp may precede a
        // valid record that appeared during admission I/O.
        let current_boot_ms = operations::identity::monotonic_boot_ms().map_err(|error| {
            format!(
                "unable to sample monotonic time after reading native Auto-Tune load evidence: {error}"
            )
        })?;
        let _ = self.autotune_capture_session.observe_load_evidence(
            request,
            &evidence,
            current_boot_ms,
        )?;
        Ok(())
    }

    #[cfg(feature = "calibration")]
    fn attest_autotune_capture(
        &mut self,
        request: &operations::full_autotune::AutotuneCaptureRequest,
        boot_ms: u64,
    ) -> Result<(u64, u64), (&'static str, String)> {
        if request.instance_name != self.cfg.instance {
            return Err((
                "capture-instance-mismatch",
                "native Auto-Tune capture belongs to another instance".to_string(),
            ));
        }
        let store =
            operations::autotune_runtime_store::RuntimeOverrideStore::open(&self.cfg.run_dir())
                .map_err(|error| ("capture-runtime-unavailable", error))?;
        let permit = store
            .read_permit()
            .map_err(|error| ("capture-runtime-unavailable", error))?
            .ok_or_else(|| {
                (
                    "capture-permit-missing",
                    "native Auto-Tune capture has no runtime permit".to_string(),
                )
            })?;
        let control = store
            .read_control()
            .map_err(|error| ("capture-runtime-unavailable", error))?;
        let ack = store
            .read_ack()
            .map_err(|error| ("capture-runtime-unavailable", error))?;
        operations::full_autotune::attest_capture_admission(
            request,
            &permit,
            control.as_ref(),
            ack.as_ref(),
            boot_ms,
        )
        .map_err(|error| ("capture-runtime-mismatch", error))?;
        if !permit
            .worker
            .still_matches(Path::new(operations::identity::DEFAULT_PROC_ROOT))
            .map_err(|error| ("capture-worker-unavailable", error))?
        {
            return Err((
                "capture-worker-unavailable",
                "native Auto-Tune capture worker identity is no longer live".to_string(),
            ));
        }
        if self.route_identity.as_deref() != Some(permit.route_identity.as_str()) {
            return Err((
                "capture-route-mismatch",
                "native Auto-Tune capture route changed after admission".to_string(),
            ));
        }
        let live_sqm = operations::sqm_identity::managed_sqm_identity_fingerprint(
            &self.cfg.instance,
            &self.cfg.sqm_section,
            &self.cfg.sqm_interface,
        )
        .map_err(|error| ("capture-sqm-mismatch", error))?;
        if live_sqm != request.sqm_fingerprint {
            return Err((
                "capture-sqm-mismatch",
                "native Auto-Tune capture SQM identity changed".to_string(),
            ));
        }
        let expected = operations::autotune_runtime::RuntimeSnapshot {
            target_interface: permit.target_interface.clone(),
            route_fingerprint: permit.route_fingerprint.clone(),
            sqm_fingerprint: permit.sqm_fingerprint.clone(),
            topology: request.topology,
            download_kbps: request.candidate_dl_kbps,
            upload_kbps: request.candidate_ul_kbps,
            download_qdisc_kind: None,
            upload_qdisc_kind: None,
        };
        let actual = match request.phase {
            operations::full_autotune::AutotuneCapturePhase::IdleBaseline => {
                if store
                    .read_checkpoint()
                    .map_err(|error| ("capture-runtime-unavailable", error))?
                    .is_some()
                {
                    return Err((
                        "capture-runtime-mismatch",
                        "idle Auto-Tune capture observed a private runtime checkpoint".to_string(),
                    ));
                }
                attest_rate_only_runtime(self, &expected)
            }
            operations::full_autotune::AutotuneCapturePhase::LoadedMeasurement => {
                let checkpoint = store
                    .read_checkpoint()
                    .map_err(|error| ("capture-runtime-unavailable", error))?
                    .ok_or_else(|| {
                        (
                            "capture-runtime-mismatch",
                            "loaded Auto-Tune capture has no private runtime checkpoint"
                                .to_string(),
                        )
                    })?;
                attest_loaded_capture_runtime(&self.cfg, &permit, &expected, &checkpoint)
            }
        }
        .map_err(|error| ("capture-runtime-mismatch", error))?;
        let (authorized_download_kind, authorized_upload_kind) = match request.phase {
            operations::full_autotune::AutotuneCapturePhase::IdleBaseline => {
                (permit.download_qdisc_kind, permit.upload_qdisc_kind)
            }
            operations::full_autotune::AutotuneCapturePhase::LoadedMeasurement => (
                operations::autotune_runtime::RuntimeQdiscKind::Cake,
                operations::autotune_runtime::RuntimeQdiscKind::Cake,
            ),
        };
        let expected_download_qdisc_kind =
            request.candidate_dl_kbps.map(|_| authorized_download_kind);
        let expected_upload_qdisc_kind = request.candidate_ul_kbps.map(|_| authorized_upload_kind);
        if actual.download_qdisc_kind != expected_download_qdisc_kind
            || actual.upload_qdisc_kind != expected_upload_qdisc_kind
        {
            return Err((
                "capture-runtime-mismatch",
                "native Auto-Tune capture qdisc kind differs from its runtime permit".to_string(),
            ));
        }
        if actual.target_interface != expected.target_interface
            || actual.route_fingerprint != expected.route_fingerprint
            || actual.sqm_fingerprint != expected.sqm_fingerprint
            || actual.topology != expected.topology
        {
            return Err((
                "capture-runtime-mismatch",
                "native Auto-Tune capture runtime differs from its requested topology".to_string(),
            ));
        }
        if actual.download_kbps != expected.download_kbps
            || actual.upload_kbps != expected.upload_kbps
        {
            let code =
                if request.phase == operations::full_autotune::AutotuneCapturePhase::IdleBaseline {
                    // `attest_rate_only_runtime` has refreshed the applied-rate
                    // cache from this exact tc read.  Publish it immediately so
                    // the worker's event-driven retry cannot reuse the stale
                    // candidate merely because the ordinary status cadence has
                    // not elapsed yet.
                    let _ = self.refresh_status_from_last_sample();
                    "capture-rate-drift"
                } else {
                    "capture-runtime-mismatch"
                };
            return Err((
                code,
                format!(
                    "native Auto-Tune capture rate drifted (requested dl={:?}, ul={:?}; live dl={:?}, ul={:?})",
                    expected.download_kbps,
                    expected.upload_kbps,
                    actual.download_kbps,
                    actual.upload_kbps
                ),
            ));
        }
        Ok((permit.initial_download_kbps, permit.initial_upload_kbps))
    }

    #[cfg(feature = "calibration")]
    fn record_autotune_capture_error(&mut self, error: String) {
        if self.autotune_capture_error.as_deref() != Some(error.as_str()) {
            self.log(
                "ERROR",
                &format!("native Auto-Tune capture rejected: {error}"),
            );
            self.autotune_capture_error = Some(error);
        }
    }

    #[cfg(feature = "calibration")]
    fn active_autotune_observation_request(
        &self,
    ) -> Option<operations::full_autotune::AutotuneCaptureRequest> {
        if self.autotune_capture_error.is_some()
            || !self.autotune_capture_session.accepting_observations()
        {
            return None;
        }
        let request = self.autotune_capture_session.active_request()?;
        let now = Instant::now();
        let boot_ms = operations::identity::monotonic_boot_ms().ok()?;
        let published = operations::full_autotune::read_capture_request(
            &self.cfg.autotune_capture_request_path(),
        )
        .ok();
        (self.autotune_capture_request.as_ref() == Some(request)
            && autotune_capture_attestation_lease_valid(
                self.autotune_capture_last_attestation,
                now,
                request,
                published.as_ref(),
                boot_ms,
            ))
        .then(|| request.clone())
    }

    /// Return the admitted capture identity even while its short attestation
    /// lease is being refreshed.  Transport phase history belongs to the
    /// capture epoch, not to one successful filesystem poll: resetting it on
    /// a transient re-attestation gap can starve the next transport probe for
    /// the remainder of an otherwise valid loaded measurement.
    #[cfg(feature = "calibration")]
    fn autotune_capture_control_request(
        &self,
    ) -> Option<operations::full_autotune::AutotuneCaptureRequest> {
        if self.autotune_capture_error.is_some()
            || !self.autotune_capture_session.accepting_observations()
        {
            return None;
        }
        let request = self.autotune_capture_session.active_request()?;
        (self.autotune_capture_request.as_ref() == Some(request)).then(|| request.clone())
    }

    #[cfg(feature = "calibration")]
    fn record_autotune_observation(
        &mut self,
        request: &operations::full_autotune::AutotuneCaptureRequest,
        kind: operations::autotune_capture::AutotuneCaptureObservationKind,
    ) {
        let boot_ms = match operations::identity::monotonic_boot_ms() {
            Ok(value) => value,
            Err(error) => {
                self.reject_autotune_observations("capture-clock-unavailable", &error);
                return;
            }
        };
        match self
            .autotune_capture_session
            .observe(request, kind, boot_ms)
        {
            Ok(_) => {}
            Err(error) => self.reject_autotune_observations("capture-observation-invalid", &error),
        }
    }

    #[cfg(feature = "calibration")]
    fn reject_autotune_observations(&mut self, code: &str, detail: &str) {
        let boot_ms = operations::identity::monotonic_boot_ms()
            .ok()
            .and_then(|value| {
                self.autotune_capture_session
                    .active_request()
                    .map(|request| value.min(request.deadline_boot_ms))
            });
        let _ = match boot_ms {
            Some(boot_ms) => self.autotune_capture_session.reject(code, boot_ms),
            None => self.autotune_capture_session.reject_at_last_update(code),
        };
        self.record_autotune_capture_error(detail.to_string());
    }

    #[cfg(feature = "calibration")]
    fn reject_autotune_capture_measurement_basis(&mut self, reason: &str) {
        if self.autotune_capture_session.accepting_observations() {
            self.reject_autotune_observations(
                operations::full_autotune::CAPTURE_MEASUREMENT_BASIS_RESET,
                &format!("native Auto-Tune measurement basis reset: {reason}"),
            );
        }
    }

    #[cfg(feature = "calibration")]
    fn observe_autotune_icmp(
        &mut self,
        sample: &Sample,
        dl_delta_us: f64,
        ul_delta_us: f64,
        download_kbps: f64,
        upload_kbps: f64,
    ) {
        let Some(request) = self.active_autotune_observation_request() else {
            return;
        };
        let (download_loaded, upload_loaded) =
            match operations::autotune_capture::bounded_directional_load_phase(
                &request,
                download_kbps,
                upload_kbps,
                self.cfg.connection_active_thr_kbps,
                self.cfg.rating_capture_ack_ratio,
            ) {
                Ok(value) => value,
                Err(error) => {
                    self.reject_autotune_observations("capture-load-phase-invalid", &error);
                    return;
                }
            };
        match operations::autotune_capture::icmp_observation_kind(
            &request,
            sample.rtt_ms,
            dl_delta_us,
            ul_delta_us,
            download_loaded,
            upload_loaded,
        ) {
            Ok(Some(kind)) => self.record_autotune_observation(&request, kind),
            Ok(None) => {}
            Err(error) => self.reject_autotune_observations("capture-icmp-invalid", &error),
        }
    }

    #[cfg(feature = "calibration")]
    fn observe_autotune_icmp_timeout(&mut self) {
        let Some(request) = self.active_autotune_observation_request() else {
            return;
        };
        self.record_autotune_observation(
            &request,
            operations::autotune_capture::AutotuneCaptureObservationKind::IcmpTimeout,
        );
    }

    #[cfg(feature = "calibration")]
    fn observe_autotune_transport(
        &mut self,
        latency_ms: f64,
        download_loaded: bool,
        upload_loaded: bool,
    ) {
        let Some(request) = self.active_autotune_observation_request() else {
            return;
        };
        match operations::autotune_capture::transport_observation_kind(
            &request,
            latency_ms,
            download_loaded,
            upload_loaded,
        ) {
            Ok(Some(kind)) => self.record_autotune_observation(&request, kind),
            Ok(None) => {}
            Err(error) => self.reject_autotune_observations("capture-transport-invalid", &error),
        }
    }

    #[cfg(feature = "calibration")]
    fn observe_autotune_traffic(&mut self, rates: RateSample) {
        if !rates.fresh {
            return;
        }
        let Some(request) = self.active_autotune_observation_request() else {
            return;
        };
        if request.phase != operations::full_autotune::AutotuneCapturePhase::IdleBaseline {
            return;
        }
        let reference = self
            .autotune_idle_rate_reference
            .as_ref()
            .filter(|reference| reference.request == request)
            .map(|reference| (reference.download_kbps, reference.upload_kbps));
        let Some((download_reference_kbps, upload_reference_kbps)) = reference else {
            self.reject_autotune_observations(
                "capture-idle-reference-missing",
                "native Auto-Tune idle capture has no identity-bound rate reference",
            );
            return;
        };
        match operations::autotune_capture::idle_traffic_observation_kind(
            &request,
            rates.dl_kbps,
            rates.ul_kbps,
            download_reference_kbps,
            upload_reference_kbps,
        ) {
            Ok(Some(kind)) => self.record_autotune_observation(&request, kind),
            Ok(None) => {}
            Err(error) => self.reject_autotune_observations("capture-traffic-invalid", &error),
        }
    }

    #[cfg(feature = "calibration")]
    fn observe_autotune_cpu(&mut self, total_percent: f64) {
        let Some(request) = self.active_autotune_observation_request() else {
            return;
        };
        if request.phase != operations::full_autotune::AutotuneCapturePhase::LoadedMeasurement {
            return;
        }
        if !total_percent.is_finite() || !(0.0..=100.0).contains(&total_percent) {
            self.reject_autotune_observations(
                "capture-cpu-invalid",
                "native Auto-Tune CPU observation is invalid",
            );
            return;
        }
        self.record_autotune_observation(
            &request,
            operations::autotune_capture::AutotuneCaptureObservationKind::Cpu {
                milli_percent: (total_percent * 1_000.0).round() as u32,
            },
        );
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

        #[cfg(feature = "calibration")]
        {
            let active_capture = self.active_autotune_observation_request();
            match censored_autotune_transport_observation(
                &result,
                active_capture.as_ref(),
                self.route_identity.as_deref(),
            ) {
                Ok(Some(kind)) => {
                    self.record_autotune_observation(active_capture.as_ref().unwrap(), kind)
                }
                Ok(None) => {}
                Err(error) => {
                    self.reject_autotune_observations("capture-transport-deadline-invalid", &error)
                }
            }
        }

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
        let samples = if result.raw_samples_ms.is_empty() {
            vec![latency_ms]
        } else {
            result.raw_samples_ms.clone()
        };
        let status_rates = self.last_status.as_ref().map(|status| {
            (
                status.dl_rate,
                status.ul_rate,
                status.dl_load_pct,
                status.ul_load_pct,
            )
        });
        #[cfg(feature = "calibration")]
        let active_autotune_capture = self.active_autotune_observation_request();
        #[cfg(feature = "calibration")]
        let autotune_capture_present = result.autotune_capture.is_some();
        #[cfg(feature = "calibration")]
        let autotune_capture_matches = result.autotune_capture.is_some()
            && result.autotune_capture.as_ref() == active_autotune_capture.as_ref();
        #[cfg(feature = "calibration")]
        let controller_phase_valid = if autotune_capture_present {
            if !autotune_capture_matches {
                self.record_transport_rejection("capture_identity_changed");
                false
            } else if result.capture_interval_valid != Some(true) {
                self.record_transport_rejection("capture_interval_invalid");
                false
            } else {
                result.control_valid
            }
        } else {
            let current_control_phase = status_rates.map(|(_, _, dl_load_pct, ul_load_pct)| {
                (
                    dl_load_pct >= self.cfg.high_load_thr * 100.0,
                    ul_load_pct >= self.cfg.high_load_thr * 100.0,
                )
            });
            result.control_valid
                && current_control_phase == Some((result.dl_loaded, result.ul_loaded))
        };
        #[cfg(not(feature = "calibration"))]
        let controller_phase_valid = {
            let current_control_phase = status_rates.map(|(_, _, dl_load_pct, ul_load_pct)| {
                (
                    dl_load_pct >= self.cfg.high_load_thr * 100.0,
                    ul_load_pct >= self.cfg.high_load_thr * 100.0,
                )
            });
            result.control_valid
                && current_control_phase == Some((result.dl_loaded, result.ul_loaded))
        };
        #[cfg(feature = "calibration")]
        if controller_phase_valid && autotune_capture_matches {
            for sample_ms in samples.iter().copied() {
                self.observe_autotune_transport(sample_ms, result.dl_loaded, result.ul_loaded);
            }
        }
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
        let controller_enabled =
            self.cfg.transport_controller_enabled && !self.runtime_rate_control_suspended();
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
                && self.cfg.adjust_dl_shaper_rate
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
                && self.cfg.adjust_ul_shaper_rate
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
        if self.cfg.adjust_dl_shaper_rate {
            self.shaper_dl = self.throughput_floor_dl;
        }
        if self.cfg.adjust_ul_shaper_rate {
            self.shaper_ul = self.throughput_floor_ul;
        }
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
            let mut changed = false;
            if self.cfg.adjust_dl_shaper_rate {
                let update = if state == "STALL" {
                    self.adaptive_dl.reset_to_configured(now)
                } else {
                    self.adaptive_dl.pause(now, "autorate state paused")
                };
                changed |= update.change.is_some();
                self.log_adaptive_update("DL", update);
            }
            if self.cfg.adjust_ul_shaper_rate {
                let update = if state == "STALL" {
                    self.adaptive_ul.reset_to_configured(now)
                } else {
                    self.adaptive_ul.pause(now, "autorate state paused")
                };
                changed |= update.change.is_some();
                self.log_adaptive_update("UL", update);
            }
            if changed {
                self.clamp_rates();
                self.apply_shaper("dl");
                self.apply_shaper("ul");
            }
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
        #[cfg(feature = "calibration")]
        self.reject_autotune_capture_measurement_basis(reason);
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
            #[cfg(feature = "calibration")]
            self.reject_autotune_capture_measurement_basis("external route address changed");
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
            let mut changed = false;
            if self.cfg.adjust_dl_shaper_rate {
                let update = self.adaptive_dl.abort_probe_gap(now);
                changed |= update.change.is_some();
                self.log_adaptive_update("DL", update);
            }
            if self.cfg.adjust_ul_shaper_rate {
                let update = self.adaptive_ul.abort_probe_gap(now);
                changed |= update.change.is_some();
                self.log_adaptive_update("UL", update);
            }
            if changed {
                self.clamp_rates();
                self.apply_shaper("dl");
                self.apply_shaper("ul");
            }
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
        #[cfg(feature = "calibration")]
        self.update_autotune_capture_rates(rate_sample);
        let dl_rate = rate_sample.dl_kbps;
        let ul_rate = rate_sample.ul_kbps;
        #[cfg(feature = "calibration")]
        let autotune_rates = self.autotune_observation_rates(rate_sample);
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
        let rating_flags = self.rating_load_snapshot.phase.direction_flags();
        if rating_flags.0 || rating_flags.1 {
            let grade_route = self.quality_grade_route_key();
            self.quality_grade.observe_icmp_delta(
                dl_delta_us / 1_000.0,
                ul_delta_us / 1_000.0,
                rating_flags.0,
                rating_flags.1,
                epoch_secs(),
                &grade_route,
            );
        }
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
        #[cfg(feature = "calibration")]
        if let Some(autotune_rates) = autotune_rates {
            self.observe_autotune_icmp(
                &sample,
                dl_delta_us,
                ul_delta_us,
                autotune_rates.dl_kbps,
                autotune_rates.ul_kbps,
            );
        }

        let transport_clean = self.transport_clean_for_growth(now);
        let transport_delta_ms = self
            .transport_latency
            .snapshot(now, self.cfg.transport_latency_enabled)
            .delta_ms;
        let transport_bloat = self.cfg.transport_latency_enabled
            && transport_delta_ms
                .map(|delta| delta > self.cfg.quality_target_delay_ms)
                .unwrap_or(false);
        if !self.runtime_rate_control_suspended() {
            self.update_direction(true, dl_kind, dl_bb, avg_dl_delta, transport_clean, now);
            self.update_direction(false, ul_kind, ul_bb, avg_ul_delta, transport_clean, now);
            self.update_adaptive_ceilings(
                dl_kind,
                ul_kind,
                dl_rate,
                ul_rate,
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
        }

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
        #[cfg(feature = "calibration")]
        self.observe_autotune_cpu(stats.total_percent);

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
            self.adaptive_dl.phase().as_str(),
            self.adaptive_ul.phase().as_str(),
            self.adaptive_dl.last_transition_reason(),
            self.adaptive_ul.last_transition_reason(),
            self.quality_search_dl.causal_state(),
            self.quality_search_ul.causal_state(),
            &self.sqm_runtime_state,
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
        if (is_dl && !self.cfg.adjust_dl_shaper_rate) || (!is_dl && !self.cfg.adjust_ul_shaper_rate)
        {
            return;
        }
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
        dl_achieved_rate_kbps: f64,
        ul_achieved_rate_kbps: f64,
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
                UplinkState::Offline | UplinkState::Learning | UplinkState::Rechecking
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
            minimum_throughput_gain_percent: (self.cfg.adaptive_ceiling_growth_percent * 0.5)
                .clamp(1.0, 5.0),
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

        if self.cfg.adjust_dl_shaper_rate {
            let dl_update = self.adaptive_dl.observe(
                now,
                AdaptiveCeilingObservation {
                    eligible: dl_eligible,
                    bufferbloat: dl_bufferbloat,
                    shaper_rate_kbps: self.shaper_dl,
                    achieved_rate_kbps: dl_achieved_rate_kbps,
                },
                policy,
            );
            self.log_adaptive_update("DL", dl_update);
        }
        if self.cfg.adjust_ul_shaper_rate {
            let ul_update = self.adaptive_ul.observe(
                now,
                AdaptiveCeilingObservation {
                    eligible: ul_eligible,
                    bufferbloat: ul_bufferbloat,
                    shaper_rate_kbps: self.shaper_ul,
                    achieved_rate_kbps: ul_achieved_rate_kbps,
                },
                policy,
            );
            self.log_adaptive_update("UL", ul_update);
        }
    }

    fn clamp_rates(&mut self) {
        if self.cfg.adjust_dl_shaper_rate {
            self.shaper_dl = self
                .shaper_dl
                .max(self.throughput_floor_dl)
                .min(self.adaptive_dl.effective_max_kbps());
        }
        if self.cfg.adjust_ul_shaper_rate {
            self.shaper_ul = self
                .shaper_ul
                .max(self.throughput_floor_ul)
                .min(self.adaptive_ul.effective_max_kbps());
        }
    }

    fn apply_shaper(&mut self, direction: &str) {
        if self.runtime_rate_control_suspended() {
            return;
        }
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
        /* An unshaped direction is passive telemetry only.  Do not advance
         * retry clocks, emit a fictitious CAKE command, or remember a fake
         * applied bandwidth for a qdisc which intentionally does not exist. */
        if !adjust {
            return;
        }
        let rounded = rate.round().max(1.0) as u64;
        if !shaper_update_due(last, rounded, last_attempt_elapsed) {
            return;
        }
        if is_dl {
            self.last_shaper_attempt_dl = Instant::now();
        } else {
            self.last_shaper_attempt_ul = Instant::now();
        }

        let qdisc_kind = match self.qdisc_kind(is_dl, &interface) {
            Ok(kind) => kind,
            Err(error) => {
                self.log(
                    "ERROR",
                    &format!("unable to identify CAKE qdisc on {interface}: {error}"),
                );
                return;
            }
        };

        if self.cfg.output_cake_changes {
            self.log(
                "SHAPER",
                &format!(
                    "tc qdisc change root dev {interface} {} bandwidth {rounded}Kbit",
                    qdisc_kind.as_tc_kind()
                ),
            );
        }

        match change_cake_rate(&interface, rounded, qdisc_kind) {
            Ok(()) => {
                if is_dl {
                    self.last_set_dl = rounded;
                } else {
                    self.last_set_ul = rounded;
                }
            }
            Err(error) => {
                if is_dl {
                    self.dl_qdisc_kind = None;
                } else {
                    self.ul_qdisc_kind = None;
                }
                self.log("ERROR", &format!("tc failed for {interface}: {error}"));
            }
        }
    }

    fn qdisc_kind(&mut self, download: bool, interface: &str) -> Result<CakeQdiscKind, String> {
        let cached = if download {
            self.dl_qdisc_kind
        } else {
            self.ul_qdisc_kind
        };
        if let Some(kind) = cached {
            return Ok(kind);
        }
        let output = tc_output(&["qdisc", "show", "dev", interface])?;
        let (kind, _) = root_cake_qdisc(&output)?;
        if download {
            self.dl_qdisc_kind = Some(kind);
        } else {
            self.ul_qdisc_kind = Some(kind);
        }
        Ok(kind)
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
        let transport_dl = self
            .transport_latency_dl
            .snapshot(adaptive_now, self.cfg.transport_latency_enabled);
        let transport_ul = self
            .transport_latency_ul
            .snapshot(adaptive_now, self.cfg.transport_latency_enabled);
        let effective_delta_ms =
            effective_latency_delta_ms(avg_dl_delta, avg_ul_delta, transport.delta_ms);
        let controller_quality_class = if transport.confirmed {
            classify_quality(Some(effective_delta_ms))
        } else {
            QualityClass::Learning
        };
        let controller_quality_reason = if !self.cfg.transport_controller_enabled {
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
        let (quality_class, quality_dl_class, quality_ul_class) =
            quality_grade.authoritative_classes();
        let quality_reason = if quality_grade.authoritative_complete_result().is_some() {
            "complete_direction_bound_icmp_and_transport"
        } else if quality_grade.last_known_stale {
            "last_complete_rating_route_stale"
        } else if quality_grade
            .current
            .as_ref()
            .map(|result| result.partial)
            .unwrap_or(false)
        {
            "partial_directional_rating"
        } else if quality_grade
            .current
            .as_ref()
            .map(|result| result.incomplete)
            .unwrap_or(false)
        {
            "incomplete_directional_rating"
        } else {
            quality_grade.state
        };
        // `shaper_*` is controller intent and can be fractional or newer than
        // the coalesced tc write.  Status and calibration must expose the last
        // successfully applied integer CAKE rate instead.  The idle capture
        // handshake still reads tc back and proves that this candidate is the
        // live qdisc rate before collecting any measurement evidence.
        let reported_cake_dl =
            published_applied_cake_rate_kbps(self.cfg.download_shaping_enabled(), self.last_set_dl);
        let reported_cake_ul =
            published_applied_cake_rate_kbps(self.cfg.upload_shaping_enabled(), self.last_set_ul);
        let mut file = File::create(&tmp)?;
        writeln!(
            file,
            "{{\"instance\":\"{}\",\"version\":\"{}\",\"state\":\"{}\",\"sqm_runtime_managed\":{},\"sqm_direction_mode\":\"{}\",\"sqm_runtime_state\":\"{}\",\"sqm_runtime_healthy\":{},\"sqm_runtime_reason\":\"{}\",\"sqm_recovery_attempts\":{},\"sqm_last_recovery_at\":{},\"started_at\":{:.6},\"updated_at\":{:.6},\"dl_if\":\"{}\",\"ul_if\":\"{}\",\"reflector\":\"{}\",\"seq\":\"{}\",\"probe_timestamp\":{:.6},\"rtt_ms\":{:.3},\"dl_owd_us\":{:.1},\"ul_owd_us\":{:.1},\"dl_achieved_rate_kbps\":{:.1},\"ul_achieved_rate_kbps\":{:.1},\"dl_load_percent\":{:.1},\"ul_load_percent\":{:.1},\"dl_sum_delays\":{},\"ul_sum_delays\":{},\"dl_avg_owd_delta_us\":{:.1},\"ul_avg_owd_delta_us\":{:.1},\"cake_dl_rate_kbps\":{:.0},\"cake_ul_rate_kbps\":{:.0},\"adaptive_ceiling_enabled\":{},\"configured_max_dl_shaper_rate_kbps\":{:.0},\"configured_max_ul_shaper_rate_kbps\":{:.0},\"effective_max_dl_shaper_rate_kbps\":{:.0},\"effective_max_ul_shaper_rate_kbps\":{:.0},\"adaptive_ceiling_dl_cap_kbps\":{:.0},\"adaptive_ceiling_ul_cap_kbps\":{:.0},\"adaptive_ceiling_dl_phase\":\"{}\",\"adaptive_ceiling_ul_phase\":\"{}\",\"adaptive_ceiling_safe_dl_kbps\":{:.0},\"adaptive_ceiling_safe_ul_kbps\":{:.0},\"adaptive_ceiling_failed_dl_kbps\":{},\"adaptive_ceiling_failed_ul_kbps\":{},\"adaptive_ceiling_probe_dl_kbps\":{},\"adaptive_ceiling_probe_ul_kbps\":{},\"adaptive_ceiling_dl_phase_elapsed_s\":{:.3},\"adaptive_ceiling_ul_phase_elapsed_s\":{:.3},\"adaptive_ceiling_dl_last_reason\":\"{}\",\"adaptive_ceiling_ul_last_reason\":\"{}\",\"cpu_total_percent\":{},\"cpu_core_percentages\":{},\"active_reflectors\":{},\"spare_reflectors\":{},\"bad_reflectors\":{},\"reflector_health\":{}}}",
            json_escape(&self.cfg.instance),
            env!("CARGO_PKG_VERSION"),
            json_escape(&self.run_state),
            self.cfg.manage_sqm && self.cfg.sqm_enabled,
            json_escape(&self.cfg.sqm_direction_mode),
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
            reported_cake_dl,
            reported_cake_ul,
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
            ",\"transport_latency_enabled\":{},\"transport_controller_enabled\":{},\"transport_probe_method\":\"network_rtt_v3\",\"transport_probe_backend\":\"{}\",\"transport_probe_trusted\":{},\"transport_probe_raw_samples\":{},\"transport_probe_discarded_samples\":{},\"transport_probe_server_processing_ms\":{:.3},\"transport_probe_connection_reused\":{},\"transport_probe_rejected_reason\":{},\"transport_probe_last_rejected_reason\":{},\"transport_probe_last_rejected_at\":{},\"transport_status\":\"{}\",\"transport_endpoint\":{},\"transport_latency_ms\":{},\"transport_baseline_ms\":{},\"transport_delta_ms\":{},\"transport_sample_age_s\":{},\"transport_confidence\":{},\"transport_successful_samples\":{},\"transport_failed_samples\":{},\"transport_last_error\":{},\"effective_latency_delta_ms\":{:.3},\"quality_estimated\":false,\"quality_class\":\"{}\",\"quality_dl_class\":\"{}\",\"quality_ul_class\":\"{}\",\"quality_confidence\":{},\"quality_reason\":\"{}\",\"quality_controller_class\":\"{}\",\"quality_controller_dl_class\":\"{}\",\"quality_controller_ul_class\":\"{}\",\"quality_controller_confidence\":{},\"quality_controller_reason\":\"{}\",\"throughput_guard_enabled\":{},\"throughput_floor_dl_kbps\":{:.0},\"throughput_floor_ul_kbps\":{:.0},\"quality_limited\":{},\"quality_limited_dl\":{},\"quality_limited_ul\":{}}}",
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
            quality_dl_class.as_str(),
            quality_ul_class.as_str(),
            if quality_grade.authoritative_complete_result().is_some() {
                100
            } else {
                0
            },
            json_escape(quality_reason),
            controller_quality_class.as_str(),
            self.quality_dl_class.as_str(),
            self.quality_ul_class.as_str(),
            transport.confidence,
            json_escape(controller_quality_reason),
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
            ",\"quality_grade_method\":\"{}\",\"quality_grade_state\":\"{}\",\"quality_grade_collected_samples\":{},\"quality_grade_required_samples\":{},\"quality_grade_baseline_ready\":{},\"quality_grade_baseline_samples\":{},\"quality_grade_baseline_required_samples\":{},\"quality_grade_dl_samples\":{},\"quality_grade_ul_samples\":{},\"quality_grade_bidirectional_samples\":{},\"quality_grade_finalize_remaining_s\":{},\"quality_grade_current\":{},\"quality_grade_last_known\":{},\"rating_load_phase\":\"{}\",\"rating_load_candidate\":\"{}\",\"rating_load_raw_dl_percent\":{:.3},\"rating_load_raw_ul_percent\":{:.3},\"rating_load_smoothed_dl_percent\":{:.3},\"rating_load_smoothed_ul_percent\":{:.3},\"rating_load_aggregate_dl_kbps\":{:.3},\"rating_load_aggregate_ul_kbps\":{:.3},\"rating_load_effective_dl_kbps\":{:.3},\"rating_load_effective_ul_kbps\":{:.3},\"rating_load_reference_dl_kbps\":{:.3},\"rating_load_reference_ul_kbps\":{:.3},\"rating_load_enter_percent\":{:.3},\"rating_load_exit_percent\":{:.3},\"rating_load_enter_dl_percent\":{:.3},\"rating_load_enter_ul_percent\":{:.3},\"rating_load_exit_dl_percent\":{:.3},\"rating_load_exit_ul_percent\":{:.3},\"rating_load_enter_dl_kbps\":{:.3},\"rating_load_enter_ul_kbps\":{:.3},\"rating_load_phase_age_s\":{:.3},\"rating_capture_active\":{},\"rating_capture_mode\":\"{}\",\"rating_capture_requested_phase\":\"{}\",\"rating_capture_background_dl_kbps\":{:.3},\"rating_capture_background_ul_kbps\":{:.3},\"rating_capture_peak_dl_percent\":{:.3},\"rating_capture_peak_ul_percent\":{:.3},\"rating_capture_contaminated\":{},\"rating_capture_contamination_reason\":\"{}\",\"graph_history_enabled\":{},\"graph_history_budget_mode\":\"{}\",\"graph_history_configured_budget_kib\":{},\"graph_history_safe_max_kib\":{},\"graph_history_effective_total_kib\":{},\"graph_history_instance_budget_kib\":{},\"graph_history_used_total_kib\":{},\"graph_history_used_instance_kib\":{},\"graph_history_stored_samples\":{},\"graph_history_instances\":{},\"graph_history_mem_total_kib\":{},\"graph_history_mem_available_kib\":{},\"graph_history_paused_low_memory\":{}",
            quality_grade::QUALITY_GRADE_METHOD,
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
        let route_test_ready = self
            .route_snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot.online
                    && matches!(
                        self.uplink_state,
                        UplinkState::Active | UplinkState::Standby
                    )
            })
            .unwrap_or(false);
        let uplink_error = uplink_error_code(self.uplink_state, &self.uplink_reason);
        let transport_error = transport_error_code(transport.last_error.as_deref());
        let adaptive_capacity = adaptive_capacity_status_json(AdaptiveCapacityStatusContext {
            enabled: self.cfg.adaptive_ceiling_enabled,
            route_epoch: self.route_identity.as_deref(),
            download: &self.adaptive_dl,
            upload: &self.adaptive_ul,
            current_download_kbps: self.shaper_dl,
            current_upload_kbps: self.shaper_ul,
            runtime_minimum_download_kbps: self.cfg.min_dl_shaper_rate_kbps,
            runtime_minimum_upload_kbps: self.cfg.min_ul_shaper_rate_kbps,
            transport_confidence_download: transport_dl.confidence,
            transport_confidence_upload: transport_ul.confidence,
            no_cake_effect_download: self.quality_search_dl.no_cake_effect(),
            no_cake_effect_upload: self.quality_search_ul.no_cake_effect(),
            causal_state_download: self.quality_search_dl.causal_state(),
            causal_state_upload: self.quality_search_ul.causal_state(),
        });
        file.seek(SeekFrom::End(-2))?;
        writeln!(
            file,
            ",\"uplink_state\":\"{}\",\"uplink_reason\":\"{}\",\"uplink_error_code\":{},\"transport_error_code\":{},\"route_mode_configured\":\"{}\",\"route_mode\":\"{}\",\"mwan3_member\":\"{}\",\"route_device\":\"{}\",\"route_source_ip\":\"{}\",\"route_external_ip\":\"{}\",\"route_fwmark\":\"{}\",\"route_table\":\"{}\",\"mwan3_member_status\":\"{}\",\"route_active\":{},\"route_test_ready\":{},\"route_identity\":{},\"adaptive_capacity\":{}}}",
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
            route_test_ready,
            json_string_or_null(self.route_identity.as_deref()),
            adaptive_capacity,
        )?;
        fs::rename(tmp, path)?;
        #[cfg(feature = "calibration")]
        {
            let runtime_rating = quality_grade
                .capture_result
                .as_ref()
                .or(quality_grade.current.as_ref());
            let current_rating =
                runtime_rating.map(|current| operations::rating::RatingResultSnapshot {
                    grade: if current.partial || current.incomplete {
                        QualityClass::Learning.as_str().to_string()
                    } else {
                        current.class.as_str().to_string()
                    },
                    increase_ms: current.increase_ms,
                    started_unix_ms: (current.started_at.max(0.0) * 1000.0).round() as u64,
                    partial: current.partial,
                    incomplete: current.incomplete,
                    dl_grade: current
                        .dl
                        .as_ref()
                        .map(|metric| metric.class.as_str().to_string())
                        .unwrap_or_default(),
                    ul_grade: current
                        .ul
                        .as_ref()
                        .map(|metric| metric.class.as_str().to_string())
                        .unwrap_or_default(),
                    dl_samples: current.dl_samples as u64,
                    ul_samples: current.ul_samples as u64,
                });
            let updated_unix_ms = (epoch_secs() * 1000.0).round() as u64;
            let rating_runtime = operations::rating::RatingRuntimeSnapshot {
                updated_unix_ms,
                capture_observed_unix_ms: self.rating_load_observed_unix_ms.min(updated_unix_ms),
                runtime_generation: self.runtime_generation,
                uplink_state: self.uplink_state.as_str().to_string(),
                route_active,
                route_test_ready,
                sqm_runtime_managed: self.cfg.manage_sqm && self.cfg.sqm_enabled,
                sqm_runtime_healthy: self.sqm_runtime_healthy,
                transport_probe_trusted: self.transport_trusted,
                baseline_ready: quality_grade.baseline_ready,
                baseline_samples: quality_grade.baseline_samples as u64,
                baseline_required_samples: quality_grade.baseline_required_samples as u64,
                required_samples: quality_grade.required_samples as u64,
                evidence_contract:
                    operations::rating::RatingEvidenceContract::WorstOfDirectionBoundIcmpAndTransport,
                dl_samples: quality_grade.dl_samples as u64,
                ul_samples: quality_grade.ul_samples as u64,
                dl_achieved_kbps: dl_rate,
                ul_achieved_kbps: ul_rate,
                cake_dl_kbps: reported_cake_dl,
                cake_ul_kbps: reported_cake_ul,
                download_qdisc_kind: published_runtime_qdisc_kind(
                    self.cfg.download_shaping_enabled(),
                    self.dl_qdisc_kind,
                ),
                upload_qdisc_kind: published_runtime_qdisc_kind(
                    self.cfg.upload_shaping_enabled(),
                    self.ul_qdisc_kind,
                ),
                reference_dl_kbps: self.rating_load_snapshot.reference_dl_kbps,
                reference_ul_kbps: self.rating_load_snapshot.reference_ul_kbps,
                capture_active: self.rating_load_snapshot.capture_active,
                capture_job_id: self.rating_load_snapshot.capture_job_id.clone(),
                capture_generation: self.rating_load_snapshot.capture_generation,
                finalized_job_id: self.rating_load_snapshot.finalized_job_id.clone(),
                finalized_generation: self.rating_load_snapshot.finalized_generation,
                finalized_outcome: self.rating_load_snapshot.finalized_outcome.clone(),
                capture_phase: self
                    .rating_load_snapshot
                    .capture_requested_phase
                    .to_string(),
                capture_contaminated: self.rating_load_snapshot.capture_contaminated,
                current_capture_job_id: runtime_rating
                    .map(|current| current.capture_job_id.clone())
                    .unwrap_or_default(),
                current_capture_generation: runtime_rating
                    .map(|current| current.capture_generation)
                    .unwrap_or(0),
                current: current_rating,
            };
            match rating_runtime.write_atomic(&self.cfg.run_dir().join("rating-runtime")) {
                Ok(()) => {
                    if self.rating_runtime_faulted {
                        self.log("INFO", "rating runtime publication recovered");
                    }
                    self.rating_runtime_faulted = false;
                }
                Err(error) => {
                    if !self.rating_runtime_faulted {
                        self.log(
                            "ERROR",
                            &format!("rating runtime publication failed closed: {error}"),
                        );
                    }
                    self.rating_runtime_faulted = true;
                }
            }
        }
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

fn configured_measurement_topology(cfg: &Config) -> operations::full_autotune::MeasurementTopology {
    use operations::full_autotune::MeasurementTopology;
    match (cfg.download_shaping_enabled(), cfg.upload_shaping_enabled()) {
        (true, true) => MeasurementTopology::ShapedBoth,
        (true, false) => MeasurementTopology::DownloadOnlyShaped,
        (false, true) => MeasurementTopology::UploadOnlyShaped,
        (false, false) => MeasurementTopology::RawBoth,
    }
}

fn measurement_topology_directions(
    topology: operations::full_autotune::MeasurementTopology,
) -> (bool, bool) {
    use operations::full_autotune::MeasurementTopology;
    let download = matches!(
        topology,
        MeasurementTopology::ShapedBoth
            | MeasurementTopology::RawUpload
            | MeasurementTopology::DownloadOnlyShaped
    );
    let upload = matches!(
        topology,
        MeasurementTopology::ShapedBoth
            | MeasurementTopology::RawDownload
            | MeasurementTopology::UploadOnlyShaped
    );
    (download, upload)
}

fn topology_is_supported_by_config(
    cfg: &Config,
    topology: operations::full_autotune::MeasurementTopology,
) -> bool {
    let (download, upload) = measurement_topology_directions(topology);
    (!download || cfg.download_shaping_enabled()) && (!upload || cfg.upload_shaping_enabled())
}

fn attest_rate_only_runtime(
    controller: &mut Controller,
    expected: &operations::autotune_runtime::RuntimeSnapshot,
) -> Result<operations::autotune_runtime::RuntimeSnapshot, String> {
    if expected.target_interface != controller.cfg.sqm_interface
        || !topology_is_supported_by_config(&controller.cfg, expected.topology)
    {
        return Err(
            "runtime attestation requests shaping outside the configured SQM topology".to_string(),
        );
    }
    let current_sqm_fingerprint = operations::sqm_identity::managed_sqm_identity_fingerprint(
        &controller.cfg.instance,
        &controller.cfg.sqm_section,
        &controller.cfg.sqm_interface,
    )?;
    if current_sqm_fingerprint != expected.sqm_fingerprint {
        return Err("live SQM configuration fingerprint changed".to_string());
    }
    let (download_shaped, upload_shaped) = measurement_topology_directions(expected.topology);
    inspect_sqm_topology_for(&controller.cfg, download_shaped, upload_shaped)
        .map_err(|error| error.to_string())?;
    let download_kbps = if download_shaped {
        let output = topology_tc_output(&["qdisc", "show", "dev", &controller.cfg.dl_if])
            .map_err(|error| error.to_string())?;
        let (kind, rate) = root_cake_qdisc(&output)?;
        controller.dl_qdisc_kind = Some(kind);
        Some(rate)
    } else {
        controller.dl_qdisc_kind = None;
        None
    };
    let upload_kbps = if upload_shaped {
        let output = topology_tc_output(&["qdisc", "show", "dev", &controller.cfg.ul_if])
            .map_err(|error| error.to_string())?;
        let (kind, rate) = root_cake_qdisc(&output)?;
        controller.ul_qdisc_kind = Some(kind);
        Some(rate)
    } else {
        controller.ul_qdisc_kind = None;
        None
    };
    let snapshot = operations::autotune_runtime::RuntimeSnapshot {
        target_interface: controller.cfg.sqm_interface.clone(),
        route_fingerprint: expected.route_fingerprint.clone(),
        sqm_fingerprint: expected.sqm_fingerprint.clone(),
        topology: expected.topology,
        download_kbps,
        upload_kbps,
        download_qdisc_kind: controller.dl_qdisc_kind.map(Into::into),
        upload_qdisc_kind: controller.ul_qdisc_kind.map(Into::into),
    };
    // The desired controller rate deliberately remains unchanged: once a
    // calibration owner releases the hold, ordinary control may still apply
    // its newer target.  Only refresh the cache that describes the qdisc rate
    // most recently proven by tc.  This makes a rate-drift retry converge
    // without turning an observation into a controller decision.
    controller.remember_attested_cake_rates(&snapshot);
    Ok(snapshot)
}

#[cfg(feature = "calibration")]
fn attest_loaded_capture_runtime(
    cfg: &Config,
    permit: &operations::autotune_runtime::AutotuneRuntimePermit,
    expected: &operations::autotune_runtime::RuntimeSnapshot,
    checkpoint: &operations::autotune_runtime_store::RuntimeOverrideCheckpoint,
) -> Result<operations::autotune_runtime::RuntimeSnapshot, String> {
    use operations::autotune_runtime::TemporaryTopologyStage;

    // Bind the complete durable owner before consulting any live interface.
    // A loaded capture is never allowed to fall back to the persistent UCI
    // dl_if/ul_if pair: its CAKE objects belong to the permit-owned temporary
    // topology and may intentionally replace a directional managed baseline.
    checkpoint.validate_against(permit)?;
    if checkpoint.temporary_stage != TemporaryTopologyStage::Active {
        return Err(format!(
            "loaded capture runtime is not active (temporary stage {})",
            checkpoint.temporary_stage.as_str()
        ));
    }
    attest_private_runtime(&cfg.sqm_interface, expected, checkpoint)
}

struct OpenWrtRateOverrideActuator<'a> {
    controller: &'a mut Controller,
    boot_ms: u64,
}

fn unsafe_runtime_actuator_error(
    message: impl Into<String>,
) -> operations::autotune_runtime_driver::RuntimeActuatorError {
    operations::autotune_runtime_driver::RuntimeActuatorError::Unsafe(message.into())
}

fn target_unavailable_runtime_blocker(
    cfg: &Config,
    message: impl Into<String>,
) -> operations::autotune_runtime_driver::RuntimeActuatorError {
    operations::autotune_runtime_driver::RuntimeActuatorError::Blocked(
        operations::autotune_runtime_driver::RuntimeRestoreBlocker::TargetUnavailable {
            target_interface: cfg.sqm_interface.clone(),
            detail: message.into(),
        },
    )
}

fn sqm_recovery_busy_runtime_blocker(
    cfg: &Config,
    message: impl Into<String>,
) -> operations::autotune_runtime_driver::RuntimeActuatorError {
    operations::autotune_runtime_driver::RuntimeActuatorError::Blocked(
        operations::autotune_runtime_driver::RuntimeRestoreBlocker::SqmRecoveryBusy {
            target_interface: cfg.sqm_interface.clone(),
            detail: message.into(),
        },
    )
}

fn topology_settling_runtime_blocker(
    cfg: &Config,
    message: impl Into<String>,
) -> operations::autotune_runtime_driver::RuntimeActuatorError {
    operations::autotune_runtime_driver::RuntimeActuatorError::Blocked(
        operations::autotune_runtime_driver::RuntimeRestoreBlocker::TopologySettling {
            target_interface: cfg.sqm_interface.clone(),
            detail: message.into(),
        },
    )
}

fn recovery_interrupted_runtime_blocker(
    cfg: &Config,
    message: impl Into<String>,
) -> operations::autotune_runtime_driver::RuntimeActuatorError {
    operations::autotune_runtime_driver::RuntimeActuatorError::Blocked(
        operations::autotune_runtime_driver::RuntimeRestoreBlocker::RecoveryInterrupted {
            target_interface: cfg.sqm_interface.clone(),
            detail: message.into(),
        },
    )
}

fn classify_sqm_recovery_error(
    cfg: &Config,
    error: SqmRecoveryError,
) -> operations::autotune_runtime_driver::RuntimeActuatorError {
    match error {
        SqmRecoveryError::Busy(message) => sqm_recovery_busy_runtime_blocker(cfg, message),
        SqmRecoveryError::Failed(message) if !managed_sqm_target_ready(cfg) => {
            target_unavailable_runtime_blocker(cfg, message)
        }
        SqmRecoveryError::Failed(message) => unsafe_runtime_actuator_error(message),
        SqmRecoveryError::Terminated => {
            recovery_interrupted_runtime_blocker(cfg, "managed SQM recovery was interrupted")
        }
    }
}

fn classify_topology_error(
    cfg: &Config,
    error: SqmTopologyError,
) -> operations::autotune_runtime_driver::RuntimeActuatorError {
    match error.kind {
        SqmTopologyErrorKind::Settling => topology_settling_runtime_blocker(cfg, error.to_string()),
        SqmTopologyErrorKind::Unsafe => unsafe_runtime_actuator_error(error.to_string()),
    }
}

fn classify_tc_mutation_error(
    cfg: &Config,
    message: String,
) -> operations::autotune_runtime_driver::RuntimeActuatorError {
    if managed_sqm_target_ready(cfg) {
        unsafe_runtime_actuator_error(message)
    } else {
        target_unavailable_runtime_blocker(cfg, message)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrivateRootState {
    AbsentOrKernelDefault,
    Exact(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrivateIngressState {
    Absent,
    Exact,
}

fn runtime_ip_output(args: &[&str]) -> Result<String, String> {
    let ip = env::var("CAKE_AUTORATE_IP").unwrap_or_else(|_| "ip".to_string());
    let output = Command::new(&ip)
        .args(args)
        .output()
        .map_err(|error| format!("failed to execute {ip}: {error}"))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if detail.is_empty() {
        format!("ip {} failed with {}", args.join(" "), output.status)
    } else {
        detail
    })
}

fn runtime_tc_output(args: &[String]) -> Result<String, String> {
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    tc_output(&refs)
}

fn private_profile_matches(
    line: &str,
    profile: crate::autotune::AutotuneProfile,
    download: bool,
) -> bool {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    let has = |value: &str| fields.contains(&value);
    if !has("nat") {
        return false;
    }
    match profile {
        crate::autotune::AutotuneProfile::Gaming
        | crate::autotune::AutotuneProfile::GamingExtreme => has("diffserv4") && !has("wash"),
        crate::autotune::AutotuneProfile::BestOverall
        | crate::autotune::AutotuneProfile::VariableLink
        | crate::autotune::AutotuneProfile::Fair => {
            if download {
                has("besteffort") && has("wash")
            } else {
                has("diffserv4") && !has("wash")
            }
        }
    }
}

fn private_link_matches(line: &str, link_kind: crate::autotune::LinkKind) -> bool {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    let value_after = |name: &str| {
        fields
            .windows(2)
            .find_map(|pair| (pair[0] == name).then_some(pair[1]))
    };
    match link_kind {
        crate::autotune::LinkKind::Pppoe => {
            fields.contains(&"noatm")
                && !fields.contains(&"raw")
                && value_after("overhead") == Some("44")
                && value_after("mpu") == Some("84")
        }
        crate::autotune::LinkKind::Ethernet => {
            fields.contains(&"noatm")
                && !fields.contains(&"raw")
                && value_after("overhead") == Some("18")
                && value_after("mpu") == Some("64")
        }
        crate::autotune::LinkKind::Cellular | crate::autotune::LinkKind::Unknown => {
            fields.contains(&"raw")
        }
    }
}

fn inspect_private_root(
    device: &str,
    handle: &str,
    profile: crate::autotune::AutotuneProfile,
    link_kind: crate::autotune::LinkKind,
    download: bool,
) -> Result<PrivateRootState, String> {
    let output = tc_output(&["qdisc", "show", "dev", device])?;
    parse_private_root_output(&output, device, handle, profile, link_kind, download)
}

fn parse_private_root_output(
    output: &str,
    device: &str,
    handle: &str,
    profile: crate::autotune::AutotuneProfile,
    link_kind: crate::autotune::LinkKind,
    download: bool,
) -> Result<PrivateRootState, String> {
    let mut exact = None;
    let mut foreign = Vec::new();
    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.first() != Some(&"qdisc") || !fields.contains(&"root") {
            continue;
        }
        let kind = fields.get(1).copied().unwrap_or_default();
        let found_handle = fields.get(2).copied().unwrap_or_default();
        if kind == "cake" && found_handle == handle {
            if exact.is_some()
                || !private_profile_matches(line, profile, download)
                || !private_link_matches(line, link_kind)
            {
                return Err(format!(
                    "private CAKE qdisc on {device} is duplicated or has unexpected policy"
                ));
            }
            exact = Some(root_cake_qdisc(line)?.1);
        } else if !(found_handle == "0:"
            && matches!(kind, "mq" | "noqueue" | "fq_codel" | "pfifo_fast"))
        {
            foreign.push(line.to_string());
        }
    }
    if !foreign.is_empty() {
        return Err(format!(
            "refusing to replace foreign root qdisc state on {device}"
        ));
    }
    Ok(exact
        .map(PrivateRootState::Exact)
        .unwrap_or(PrivateRootState::AbsentOrKernelDefault))
}

fn inspect_private_ingress(
    target: &str,
    checkpoint: &operations::autotune_runtime_store::RuntimeOverrideCheckpoint,
) -> Result<PrivateIngressState, String> {
    let qdiscs = tc_output(&["qdisc", "show", "dev", target])?;
    let filters = tc_output(&["filter", "show", "dev", target, "ingress"])?;
    parse_private_ingress_output(&qdiscs, &filters, target, checkpoint)
}

fn parse_private_ingress_output(
    qdiscs: &str,
    filters: &str,
    target: &str,
    checkpoint: &operations::autotune_runtime_store::RuntimeOverrideCheckpoint,
) -> Result<PrivateIngressState, String> {
    let ingress = qdiscs
        .lines()
        .filter(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            fields.first() == Some(&"qdisc")
                && (fields.get(1) == Some(&"ingress") || fields.get(1) == Some(&"clsact"))
        })
        .collect::<Vec<_>>();
    if ingress.is_empty() && filters.trim().is_empty() {
        return Ok(PrivateIngressState::Absent);
    }
    if ingress.len() != 1 {
        return Err(format!(
            "refusing to replace ambiguous ingress state on {target}"
        ));
    }
    let qdisc_fields = ingress[0].split_whitespace().collect::<Vec<_>>();
    if qdisc_fields.get(1) != Some(&"ingress")
        || qdisc_fields.get(2).copied() != Some(checkpoint.temporary.ingress_qdisc_handle.as_str())
    {
        return Err(format!("refusing to replace foreign ingress on {target}"));
    }
    checkpoint.temporary.validate()?;
    let preference = checkpoint.temporary.redirect_preference.to_string();
    let filter_handle = checkpoint.temporary.redirect_filter_tc_handle()?;
    let action_index = checkpoint.temporary.redirect_action_index.to_string();
    let action_cookie = checkpoint.temporary.redirect_action_cookie.as_str();
    let mut filter_headers = 0_u8;
    let mut root_tables = 0_u8;
    let mut filter_nodes = 0_u8;
    let mut matches = 0_u8;
    let mut redirects = 0_u8;
    let mut action_indexes = 0_u8;
    let mut action_cookies = 0_u8;
    for line in filters
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.first() == Some(&"filter") {
            let line_pref = fields
                .windows(2)
                .find_map(|pair| (pair[0] == "pref").then_some(pair[1]));
            if line_pref != Some(preference.as_str()) {
                return Err(format!(
                    "refusing to replace foreign ingress filter on {target}"
                ));
            }
            filter_headers = filter_headers.saturating_add(1);
            if let Some(handle) = fields
                .windows(2)
                .find_map(|pair| (pair[0] == "fh").then_some(pair[1]))
            {
                if handle == "800:" {
                    if !fields.windows(3).any(|part| part == ["ht", "divisor", "1"]) {
                        return Err(format!(
                            "temporary ingress u32 root is not canonical on {target}"
                        ));
                    }
                    root_tables = root_tables.saturating_add(1);
                } else if handle == filter_handle {
                    if !fields.contains(&"terminal") {
                        return Err(format!(
                            "temporary ingress u32 node is not terminal on {target}"
                        ));
                    }
                    filter_nodes = filter_nodes.saturating_add(1);
                } else {
                    return Err(format!(
                        "refusing to replace foreign ingress filter on {target}"
                    ));
                }
            }
        }
        if fields.first() == Some(&"match") {
            if line != "match 00000000/00000000 at 0" {
                return Err(format!(
                    "temporary ingress classifier is not an exact match-all rule on {target}"
                ));
            }
            matches = matches.saturating_add(1);
        }
        if fields.first() == Some(&"action") {
            let expected = format!(
                "mirred (Egress Redirect to device {})",
                checkpoint.temporary.ifb_name
            );
            if !line.contains(&expected) {
                return Err(format!(
                    "temporary ingress action is not the owned redirect on {target}"
                ));
            }
            redirects = redirects.saturating_add(1);
        }
        if fields.first() == Some(&"index") {
            if fields.get(1).copied() != Some(action_index.as_str()) {
                return Err(format!(
                    "temporary ingress action index is foreign on {target}"
                ));
            }
            action_indexes = action_indexes.saturating_add(1);
        }
        if fields.first() == Some(&"cookie") {
            if fields.get(1).copied() != Some(action_cookie) {
                return Err(format!(
                    "temporary ingress action cookie is foreign on {target}"
                ));
            }
            action_cookies = action_cookies.saturating_add(1);
        }
    }
    if filter_headers != 3
        || root_tables != 1
        || filter_nodes != 1
        || matches != 1
        || redirects != 1
        || action_indexes != 1
        || action_cookies != 1
    {
        return Err(format!(
            "temporary ingress ownership could not be attested on {target}"
        ));
    }
    Ok(PrivateIngressState::Exact)
}

fn private_ingress_filter_args(
    target: &str,
    checkpoint: &operations::autotune_runtime_store::RuntimeOverrideCheckpoint,
) -> Result<Vec<String>, String> {
    checkpoint.temporary.validate()?;
    Ok(vec![
        "filter".to_string(),
        "add".to_string(),
        "dev".to_string(),
        target.to_string(),
        "parent".to_string(),
        checkpoint.temporary.ingress_qdisc_handle.clone(),
        "protocol".to_string(),
        "all".to_string(),
        "pref".to_string(),
        checkpoint.temporary.redirect_preference.to_string(),
        "handle".to_string(),
        checkpoint.temporary.redirect_filter_tc_handle()?,
        "u32".to_string(),
        "match".to_string(),
        "u32".to_string(),
        "0".to_string(),
        "0".to_string(),
        "action".to_string(),
        "mirred".to_string(),
        "egress".to_string(),
        "redirect".to_string(),
        "index".to_string(),
        checkpoint.temporary.redirect_action_index.to_string(),
        "dev".to_string(),
        checkpoint.temporary.ifb_name.clone(),
        "cookie".to_string(),
        checkpoint.temporary.redirect_action_cookie.clone(),
    ])
}

fn exact_private_ifb(
    checkpoint: &operations::autotune_runtime_store::RuntimeOverrideCheckpoint,
    require_ifindex: bool,
) -> Result<bool, String> {
    let root = sqm_sys_class_net();
    let path = root.join(&checkpoint.temporary.ifb_name);
    if !path.exists() {
        return Ok(false);
    }
    let alias = fs::read_to_string(path.join("ifalias"))
        .map_err(|error| format!("unable to read temporary IFB alias: {error}"))?;
    if alias.trim() != checkpoint.temporary.ifb_alias {
        return Err("temporary IFB alias does not match durable ownership".to_string());
    }
    let ifindex = fs::read_to_string(path.join("ifindex"))
        .map_err(|error| format!("unable to read temporary IFB ifindex: {error}"))?
        .trim()
        .parse::<u32>()
        .map_err(|_| "temporary IFB ifindex is invalid".to_string())?;
    if require_ifindex && checkpoint.temporary.ifb_ifindex != Some(ifindex) {
        return Err("temporary IFB ifindex does not match durable ownership".to_string());
    }
    let details = runtime_ip_output(&[
        "-details",
        "link",
        "show",
        "dev",
        &checkpoint.temporary.ifb_name,
    ])?;
    if !details.split_whitespace().any(|field| field == "ifb") {
        return Err("temporary owned link is not an IFB".to_string());
    }
    Ok(true)
}

fn replace_private_cake(
    device: &str,
    handle: &str,
    rate_kbps: u64,
    profile: crate::autotune::AutotuneProfile,
    link_kind: crate::autotune::LinkKind,
    download: bool,
) -> Result<(), String> {
    runtime_tc_output(&private_cake_args(
        device, handle, rate_kbps, profile, link_kind, download,
    )?)
    .map(|_| ())
}

fn private_cake_args(
    device: &str,
    handle: &str,
    rate_kbps: u64,
    profile: crate::autotune::AutotuneProfile,
    link_kind: crate::autotune::LinkKind,
    download: bool,
) -> Result<Vec<String>, String> {
    if !(100..=autotune::MAX_RATE_KBPS).contains(&rate_kbps) {
        return Err("private CAKE rate is outside the supported range".to_string());
    }
    let mut args = vec![
        "qdisc".to_string(),
        "replace".to_string(),
        "dev".to_string(),
        device.to_string(),
        "root".to_string(),
        "handle".to_string(),
        handle.to_string(),
        "cake".to_string(),
        "bandwidth".to_string(),
        format!("{rate_kbps}kbit"),
    ];
    match profile {
        crate::autotune::AutotuneProfile::Gaming
        | crate::autotune::AutotuneProfile::GamingExtreme => {
            args.extend(["diffserv4", "nat"].map(str::to_string));
        }
        crate::autotune::AutotuneProfile::BestOverall
        | crate::autotune::AutotuneProfile::VariableLink
        | crate::autotune::AutotuneProfile::Fair => {
            if download {
                args.extend(["besteffort", "nat", "wash"].map(str::to_string));
            } else {
                args.extend(["diffserv4", "nat"].map(str::to_string));
            }
        }
    }
    match link_kind {
        crate::autotune::LinkKind::Pppoe => {
            args.extend(["ethernet", "overhead", "44", "mpu", "84"].map(str::to_string));
        }
        crate::autotune::LinkKind::Ethernet => {
            args.extend(["ethernet", "overhead", "18", "mpu", "64"].map(str::to_string));
        }
        crate::autotune::LinkKind::Cellular | crate::autotune::LinkKind::Unknown => {
            args.push("raw".to_string());
        }
    }
    Ok(args)
}

fn delete_private_root_if_present(
    device: &str,
    handle: &str,
    profile: crate::autotune::AutotuneProfile,
    link_kind: crate::autotune::LinkKind,
    download: bool,
) -> Result<(), String> {
    match inspect_private_root(device, handle, profile, link_kind, download)? {
        PrivateRootState::AbsentOrKernelDefault => Ok(()),
        PrivateRootState::Exact(_) => delete_qdisc(device, "root"),
    }
}

fn attach_private_ingress(
    target: &str,
    checkpoint: &operations::autotune_runtime_store::RuntimeOverrideCheckpoint,
) -> Result<(), String> {
    match inspect_private_ingress(target, checkpoint)? {
        PrivateIngressState::Exact => return Ok(()),
        PrivateIngressState::Absent => {}
    }
    let qdisc = vec![
        "qdisc".to_string(),
        "add".to_string(),
        "dev".to_string(),
        target.to_string(),
        "handle".to_string(),
        checkpoint.temporary.ingress_qdisc_handle.clone(),
        "ingress".to_string(),
    ];
    runtime_tc_output(&qdisc)?;
    let filter = private_ingress_filter_args(target, checkpoint)?;
    if let Err(error) = runtime_tc_output(&filter) {
        let _ = delete_qdisc(target, "ingress");
        return Err(error);
    }
    if inspect_private_ingress(target, checkpoint)? != PrivateIngressState::Exact {
        return Err("temporary ingress postcondition failed".to_string());
    }
    Ok(())
}

fn delete_private_ingress_if_present(
    target: &str,
    checkpoint: &operations::autotune_runtime_store::RuntimeOverrideCheckpoint,
) -> Result<(), String> {
    match inspect_private_ingress(target, checkpoint)? {
        PrivateIngressState::Absent => Ok(()),
        PrivateIngressState::Exact => delete_qdisc(target, "ingress"),
    }
}

fn stop_managed_sqm_with<F>(
    helper: &Path,
    target_interface: &str,
    timeout: Duration,
    should_cancel: F,
) -> Result<(), String>
where
    F: Fn() -> bool,
{
    let spec = operations::process::SpawnSpec {
        program: helper.to_path_buf(),
        arguments: vec![OsString::from("stop"), OsString::from(target_interface)],
        environment: Vec::new(),
    };
    let output = operations::process::run_bounded_command_output(
        &spec,
        timeout,
        SQM_RUNTIME_STOP_OUTPUT_LIMIT,
        should_cancel,
    )
    .map_err(|error| format!("managed SQM stop helper failed safely: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if detail.is_empty() {
        format!("managed SQM stop failed with {}", output.status)
    } else {
        detail
    })
}

fn stop_managed_sqm_for_runtime(cfg: &Config) -> Result<(), String> {
    let helper = env::var_os("CAKE_AUTORATE_SQM_RUN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/lib/sqm/run.sh"));
    stop_managed_sqm_with(
        &helper,
        &cfg.sqm_interface,
        SQM_RUNTIME_STOP_TIMEOUT,
        || TERMINATE.load(Ordering::SeqCst),
    )
}

fn create_private_ifb(
    checkpoint: &operations::autotune_runtime_store::RuntimeOverrideCheckpoint,
) -> Result<u32, String> {
    if exact_private_ifb(checkpoint, false)? {
        return Err("refusing to reuse an existing temporary IFB".to_string());
    }
    runtime_ip_output(&[
        "link",
        "add",
        "name",
        &checkpoint.temporary.ifb_name,
        "type",
        "ifb",
    ])?;
    if let Err(error) = runtime_ip_output(&[
        "link",
        "set",
        "dev",
        &checkpoint.temporary.ifb_name,
        "alias",
        &checkpoint.temporary.ifb_alias,
    ]) {
        let _ = runtime_ip_output(&[
            "link",
            "delete",
            "dev",
            &checkpoint.temporary.ifb_name,
            "type",
            "ifb",
        ]);
        return Err(error);
    }
    runtime_ip_output(&["link", "set", "dev", &checkpoint.temporary.ifb_name, "up"])?;
    let ifindex = fs::read_to_string(
        sqm_sys_class_net()
            .join(&checkpoint.temporary.ifb_name)
            .join("ifindex"),
    )
    .map_err(|error| format!("unable to read created temporary IFB ifindex: {error}"))?
    .trim()
    .parse::<u32>()
    .map_err(|_| "created temporary IFB ifindex is invalid".to_string())?;
    if ifindex == 0 || !exact_private_ifb(checkpoint, false)? {
        return Err("created temporary IFB ownership could not be attested".to_string());
    }
    Ok(ifindex)
}

fn delete_private_ifb(
    checkpoint: &operations::autotune_runtime_store::RuntimeOverrideCheckpoint,
    require_ifindex: bool,
) -> Result<(), String> {
    if !exact_private_ifb(checkpoint, require_ifindex)? {
        return Ok(());
    }
    runtime_ip_output(&["link", "set", "dev", &checkpoint.temporary.ifb_name, "down"])?;
    runtime_ip_output(&[
        "link",
        "delete",
        "dev",
        &checkpoint.temporary.ifb_name,
        "type",
        "ifb",
    ])?;
    if sqm_sys_class_net()
        .join(&checkpoint.temporary.ifb_name)
        .exists()
    {
        return Err("temporary IFB remains after deletion".to_string());
    }
    Ok(())
}

fn apply_private_runtime_topology(
    target: &str,
    control: &operations::full_autotune::AutotuneRuntimeControl,
    checkpoint: &operations::autotune_runtime_store::RuntimeOverrideCheckpoint,
) -> Result<(), String> {
    if checkpoint.temporary_stage != operations::autotune_runtime::TemporaryTopologyStage::LinkOwned
        && checkpoint.temporary_stage
            != operations::autotune_runtime::TemporaryTopologyStage::Active
    {
        return Err("private runtime topology is not owned for mutation".to_string());
    }
    if !exact_private_ifb(checkpoint, true)? {
        return Err("owned temporary IFB is missing".to_string());
    }
    let ifb = checkpoint.temporary.ifb_name.as_str();
    let upload_state = inspect_private_root(
        target,
        &checkpoint.temporary.target_qdisc_handle,
        checkpoint.profile,
        checkpoint.link_kind,
        false,
    )?;
    let download_state = inspect_private_root(
        ifb,
        &checkpoint.temporary.ifb_qdisc_handle,
        checkpoint.profile,
        checkpoint.link_kind,
        true,
    )?;
    let _ = (upload_state, download_state);

    if let Some(rate) = control.upload_kbps {
        replace_private_cake(
            target,
            &checkpoint.temporary.target_qdisc_handle,
            rate,
            checkpoint.profile,
            checkpoint.link_kind,
            false,
        )?;
    } else {
        delete_private_root_if_present(
            target,
            &checkpoint.temporary.target_qdisc_handle,
            checkpoint.profile,
            checkpoint.link_kind,
            false,
        )?;
    }
    if let Some(rate) = control.download_kbps {
        replace_private_cake(
            ifb,
            &checkpoint.temporary.ifb_qdisc_handle,
            rate,
            checkpoint.profile,
            checkpoint.link_kind,
            true,
        )?;
        attach_private_ingress(target, checkpoint)?;
    } else {
        delete_private_ingress_if_present(target, checkpoint)?;
        delete_private_root_if_present(
            ifb,
            &checkpoint.temporary.ifb_qdisc_handle,
            checkpoint.profile,
            checkpoint.link_kind,
            true,
        )?;
    }
    Ok(())
}

fn attest_private_runtime(
    target: &str,
    expected: &operations::autotune_runtime::RuntimeSnapshot,
    checkpoint: &operations::autotune_runtime_store::RuntimeOverrideCheckpoint,
) -> Result<operations::autotune_runtime::RuntimeSnapshot, String> {
    if !exact_private_ifb(checkpoint, true)? {
        return Err("owned temporary IFB is missing during attestation".to_string());
    }
    let upload = inspect_private_root(
        target,
        &checkpoint.temporary.target_qdisc_handle,
        checkpoint.profile,
        checkpoint.link_kind,
        false,
    )?;
    let download = inspect_private_root(
        &checkpoint.temporary.ifb_name,
        &checkpoint.temporary.ifb_qdisc_handle,
        checkpoint.profile,
        checkpoint.link_kind,
        true,
    )?;
    let ingress = inspect_private_ingress(target, checkpoint)?;
    let download_kbps = match (expected.download_kbps, download, ingress) {
        (Some(expected_rate), PrivateRootState::Exact(actual), PrivateIngressState::Exact)
            if actual == expected_rate =>
        {
            Some(actual)
        }
        (None, PrivateRootState::AbsentOrKernelDefault, PrivateIngressState::Absent) => None,
        _ => return Err("private download topology does not match expected control".to_string()),
    };
    let upload_kbps = match (expected.upload_kbps, upload) {
        (Some(expected_rate), PrivateRootState::Exact(actual)) if actual == expected_rate => {
            Some(actual)
        }
        (None, PrivateRootState::AbsentOrKernelDefault) => None,
        _ => return Err("private upload topology does not match expected control".to_string()),
    };
    Ok(operations::autotune_runtime::RuntimeSnapshot {
        target_interface: expected.target_interface.clone(),
        route_fingerprint: expected.route_fingerprint.clone(),
        sqm_fingerprint: expected.sqm_fingerprint.clone(),
        topology: expected.topology,
        download_kbps,
        upload_kbps,
        download_qdisc_kind: download_kbps
            .map(|_| operations::autotune_runtime::RuntimeQdiscKind::Cake),
        upload_qdisc_kind: upload_kbps
            .map(|_| operations::autotune_runtime::RuntimeQdiscKind::Cake),
    })
}

fn remove_private_runtime_topology(
    target: &str,
    checkpoint: &operations::autotune_runtime_store::RuntimeOverrideCheckpoint,
) -> Result<(), String> {
    use operations::autotune_runtime::TemporaryTopologyStage;

    if checkpoint.temporary_stage == TemporaryTopologyStage::Planned {
        if exact_private_ifb(checkpoint, false)? {
            return Err("temporary IFB appeared before managed SQM suspension".to_string());
        }
        return Ok(());
    }
    if matches!(
        checkpoint.temporary_stage,
        TemporaryTopologyStage::TemporaryAbsent | TemporaryTopologyStage::BaselineRestored
    ) {
        return Ok(());
    }
    let require_ifindex = matches!(
        checkpoint.temporary_stage,
        TemporaryTopologyStage::LinkOwned | TemporaryTopologyStage::Active
    );
    let ifb_exists = exact_private_ifb(checkpoint, require_ifindex)?;
    delete_private_ingress_if_present(target, checkpoint)?;
    delete_private_root_if_present(
        target,
        &checkpoint.temporary.target_qdisc_handle,
        checkpoint.profile,
        checkpoint.link_kind,
        false,
    )?;
    if ifb_exists {
        delete_private_root_if_present(
            &checkpoint.temporary.ifb_name,
            &checkpoint.temporary.ifb_qdisc_handle,
            checkpoint.profile,
            checkpoint.link_kind,
            true,
        )?;
        delete_private_ifb(checkpoint, require_ifindex)?;
    }
    if exact_private_ifb(checkpoint, require_ifindex)? {
        return Err("temporary topology remains after cleanup".to_string());
    }
    Ok(())
}

impl operations::autotune_runtime_driver::RuntimeOverrideActuator
    for OpenWrtRateOverrideActuator<'_>
{
    fn current_boot_ms(&self) -> u64 {
        self.boot_ms
    }

    fn capture_baseline(
        &mut self,
        permit: &operations::autotune_runtime::AutotuneRuntimePermit,
    ) -> Result<operations::autotune_runtime::RuntimeRestoreBaseline, String> {
        if permit.instance_name != self.controller.cfg.instance
            || permit.target_interface != self.controller.cfg.sqm_interface
            || self.controller.route_identity.as_deref() != Some(permit.route_identity.as_str())
        {
            return Err("runtime permit does not match the live instance route".to_string());
        }
        let current_sqm_fingerprint = operations::sqm_identity::managed_sqm_identity_fingerprint(
            &self.controller.cfg.instance,
            &self.controller.cfg.sqm_section,
            &self.controller.cfg.sqm_interface,
        )?;
        if current_sqm_fingerprint != permit.sqm_fingerprint {
            return Err("runtime permit does not match the live SQM configuration".to_string());
        }
        let expected = operations::autotune_runtime::RuntimeSnapshot {
            target_interface: permit.target_interface.clone(),
            route_fingerprint: permit.route_fingerprint.clone(),
            sqm_fingerprint: permit.sqm_fingerprint.clone(),
            topology: configured_measurement_topology(&self.controller.cfg),
            download_kbps: self
                .controller
                .cfg
                .download_shaping_enabled()
                .then_some(self.controller.shaper_dl.round().max(100.0) as u64),
            upload_kbps: self
                .controller
                .cfg
                .upload_shaping_enabled()
                .then_some(self.controller.shaper_ul.round().max(100.0) as u64),
            download_qdisc_kind: None,
            upload_qdisc_kind: None,
        };
        attest_rate_only_runtime(self.controller, &expected)
            .map(operations::autotune_runtime::RuntimeBaseline::Managed)
    }

    fn current_route_identity(&mut self) -> Result<String, String> {
        self.controller
            .route_identity
            .clone()
            .ok_or_else(|| "live route identity is unavailable".to_string())
    }

    fn baseline_ready_for_apply(
        &mut self,
        permit: &operations::autotune_runtime::AutotuneRuntimePermit,
    ) -> Result<bool, String> {
        if !matches!(
            &permit.baseline,
            operations::autotune_runtime::RuntimeBaseline::Managed(_)
        ) {
            return Ok(false);
        }
        Ok(operations::sqm_identity::managed_sqm_identity_fingerprint(
            &self.controller.cfg.instance,
            &self.controller.cfg.sqm_section,
            &self.controller.cfg.sqm_interface,
        )? == permit.sqm_fingerprint)
    }

    fn active_identity_matches(
        &mut self,
        permit: &operations::autotune_runtime::AutotuneRuntimePermit,
    ) -> Result<bool, String> {
        self.baseline_ready_for_apply(permit)
    }

    fn worker_identity_matches(&self, worker: &operations::identity::ProcessIdentity) -> bool {
        worker
            .still_matches(Path::new(operations::identity::DEFAULT_PROC_ROOT))
            .unwrap_or(false)
    }

    fn runtime_matches(
        &mut self,
        expected: &operations::autotune_runtime::RuntimeSnapshot,
        checkpoint: &operations::autotune_runtime_store::RuntimeOverrideCheckpoint,
    ) -> Result<bool, String> {
        let attested = if checkpoint.temporary_stage
            == operations::autotune_runtime::TemporaryTopologyStage::Active
        {
            attest_private_runtime(&self.controller.cfg.sqm_interface, expected, checkpoint)?
        } else {
            attest_rate_only_runtime(self.controller, expected)?
        };
        Ok(attested == *expected)
    }

    fn prepare_baseline(
        &mut self,
        checkpoint: &operations::autotune_runtime_store::RuntimeOverrideCheckpoint,
    ) -> Result<(), operations::autotune_runtime_driver::RuntimeActuatorError> {
        let baseline = checkpoint
            .managed_baseline()
            .map_err(unsafe_runtime_actuator_error)?;
        if checkpoint.temporary_stage
            != operations::autotune_runtime::TemporaryTopologyStage::Planned
            || baseline.topology != configured_measurement_topology(&self.controller.cfg)
            || baseline.target_interface != self.controller.cfg.sqm_interface
        {
            return Err(unsafe_runtime_actuator_error(
                "temporary calibration suspension checkpoint is stale",
            ));
        }
        let attested = attest_rate_only_runtime(self.controller, baseline)
            .map_err(unsafe_runtime_actuator_error)?;
        if attested != *baseline {
            return Err(unsafe_runtime_actuator_error(
                "managed SQM changed before temporary calibration suspension",
            ));
        }
        if exact_private_ifb(checkpoint, false).map_err(unsafe_runtime_actuator_error)? {
            return Err(unsafe_runtime_actuator_error(
                "temporary calibration IFB already exists before suspension",
            ));
        }
        self.controller.runtime_override_active = true;
        stop_managed_sqm_for_runtime(&self.controller.cfg)
            .map_err(|error| classify_tc_mutation_error(&self.controller.cfg, error))?;
        if inspect_private_root(
            &self.controller.cfg.sqm_interface,
            &checkpoint.temporary.target_qdisc_handle,
            checkpoint.profile,
            checkpoint.link_kind,
            false,
        )
        .map_err(|error| classify_tc_mutation_error(&self.controller.cfg, error))?
            != PrivateRootState::AbsentOrKernelDefault
        {
            return Err(unsafe_runtime_actuator_error(
                "temporary upload qdisc exists immediately after managed SQM suspension",
            ));
        }
        if inspect_private_ingress(&self.controller.cfg.sqm_interface, checkpoint)
            .map_err(|error| classify_tc_mutation_error(&self.controller.cfg, error))?
            != PrivateIngressState::Absent
        {
            return Err(unsafe_runtime_actuator_error(
                "managed SQM ingress remains after suspension",
            ));
        }
        self.controller.dl_qdisc_kind = None;
        self.controller.ul_qdisc_kind = None;
        Ok(())
    }

    fn create_temporary_ifb(
        &mut self,
        checkpoint: &operations::autotune_runtime_store::RuntimeOverrideCheckpoint,
    ) -> Result<u32, operations::autotune_runtime_driver::RuntimeActuatorError> {
        if checkpoint.temporary_stage
            != operations::autotune_runtime::TemporaryTopologyStage::ManagedSqmSuspended
        {
            return Err(unsafe_runtime_actuator_error(
                "temporary IFB creation was requested from the wrong checkpoint stage",
            ));
        }
        create_private_ifb(checkpoint)
            .map_err(|error| classify_tc_mutation_error(&self.controller.cfg, error))
    }

    fn apply_override(
        &mut self,
        control: &operations::full_autotune::AutotuneRuntimeControl,
        expected: &operations::autotune_runtime::RuntimeSnapshot,
        checkpoint: &operations::autotune_runtime_store::RuntimeOverrideCheckpoint,
    ) -> Result<(), operations::autotune_runtime_driver::RuntimeActuatorError> {
        if !managed_sqm_target_ready(&self.controller.cfg) {
            return Err(target_unavailable_runtime_blocker(
                &self.controller.cfg,
                "managed SQM target is temporarily unavailable",
            ));
        }
        self.controller.runtime_override_active = true;
        apply_private_runtime_topology(&self.controller.cfg.sqm_interface, control, checkpoint)
            .map_err(|error| classify_tc_mutation_error(&self.controller.cfg, error))?;
        let attested =
            attest_private_runtime(&self.controller.cfg.sqm_interface, expected, checkpoint)
                .map_err(|error| classify_tc_mutation_error(&self.controller.cfg, error))?;
        if attested != *expected {
            return Err(unsafe_runtime_actuator_error(
                "private calibration topology postcondition failed",
            ));
        }
        if let Some(rate) = control.download_kbps {
            self.controller.shaper_dl = rate as f64;
            self.controller.last_set_dl = rate;
        }
        if let Some(rate) = control.upload_kbps {
            self.controller.shaper_ul = rate as f64;
            self.controller.last_set_ul = rate;
        }
        Ok(())
    }

    fn remove_temporary_topology(
        &mut self,
        checkpoint: &operations::autotune_runtime_store::RuntimeOverrideCheckpoint,
    ) -> Result<(), operations::autotune_runtime_driver::RuntimeActuatorError> {
        remove_private_runtime_topology(&self.controller.cfg.sqm_interface, checkpoint)
            .map_err(|error| classify_tc_mutation_error(&self.controller.cfg, error))
    }

    fn restore_baseline(
        &mut self,
        baseline: &operations::autotune_runtime::RuntimeRestoreBaseline,
    ) -> Result<(), operations::autotune_runtime_driver::RuntimeActuatorError> {
        let baseline = baseline
            .managed_snapshot()
            .map_err(unsafe_runtime_actuator_error)?;
        if !managed_sqm_target_ready(&self.controller.cfg) {
            return Err(target_unavailable_runtime_blocker(
                &self.controller.cfg,
                "managed SQM target is temporarily unavailable during restore",
            ));
        }
        if baseline.topology != configured_measurement_topology(&self.controller.cfg) {
            return Err(unsafe_runtime_actuator_error(
                "saved baseline topology no longer matches this instance",
            ));
        }
        let current_sqm_fingerprint = operations::sqm_identity::managed_sqm_identity_fingerprint(
            &self.controller.cfg.instance,
            &self.controller.cfg.sqm_section,
            &self.controller.cfg.sqm_interface,
        )
        .map_err(unsafe_runtime_actuator_error)?;
        if current_sqm_fingerprint != baseline.sqm_fingerprint {
            return Err(unsafe_runtime_actuator_error(
                "saved baseline SQM fingerprint no longer matches current UCI; stale restore refused"
                    .to_string(),
            ));
        }
        self.controller.runtime_override_active = true;
        let (download_shaped, upload_shaped) = measurement_topology_directions(baseline.topology);
        match inspect_sqm_topology_for(&self.controller.cfg, download_shaped, upload_shaped) {
            Ok(()) => {}
            Err(error) if error.kind == SqmTopologyErrorKind::Settling => {
                recover_managed_sqm(&self.controller.cfg)
                    .map_err(|error| classify_sqm_recovery_error(&self.controller.cfg, error))?;
                self.controller.dl_qdisc_kind = None;
                self.controller.ul_qdisc_kind = None;
            }
            Err(error) => return Err(classify_topology_error(&self.controller.cfg, error)),
        }
        if let Some(rate) = baseline.download_kbps {
            let interface = self.controller.cfg.dl_if.clone();
            let kind = self
                .controller
                .qdisc_kind(true, &interface)
                .map_err(unsafe_runtime_actuator_error)?;
            change_cake_rate(&interface, rate, kind)
                .map_err(|error| classify_tc_mutation_error(&self.controller.cfg, error))?;
        }
        if let Some(rate) = baseline.upload_kbps {
            let interface = self.controller.cfg.ul_if.clone();
            let kind = self
                .controller
                .qdisc_kind(false, &interface)
                .map_err(unsafe_runtime_actuator_error)?;
            change_cake_rate(&interface, rate, kind)
                .map_err(|error| classify_tc_mutation_error(&self.controller.cfg, error))?;
        }
        if let Some(rate) = baseline.download_kbps {
            self.controller.shaper_dl = rate as f64;
            self.controller.last_set_dl = rate;
        }
        if let Some(rate) = baseline.upload_kbps {
            self.controller.shaper_ul = rate as f64;
            self.controller.last_set_ul = rate;
        }
        Ok(())
    }

    fn observe_restore_blocker(
        &mut self,
        blocker: &operations::autotune_runtime_driver::RuntimeRestoreBlocker,
        baseline: &operations::autotune_runtime::RuntimeRestoreBaseline,
    ) -> Result<operations::autotune_runtime_driver::RuntimeRestoreObservation, String> {
        use operations::autotune_runtime_driver::{
            RuntimeActuatorError, RuntimeRestoreBlocker, RuntimeRestoreObservation,
        };

        let baseline = baseline.managed_snapshot()?;
        if baseline.target_interface != self.controller.cfg.sqm_interface {
            return Err("runtime restore blocker belongs to another target interface".to_string());
        }
        let blocker_target = match blocker {
            RuntimeRestoreBlocker::TargetUnavailable {
                target_interface, ..
            }
            | RuntimeRestoreBlocker::SqmRecoveryBusy {
                target_interface, ..
            }
            | RuntimeRestoreBlocker::TopologySettling {
                target_interface, ..
            }
            | RuntimeRestoreBlocker::RecoveryInterrupted {
                target_interface, ..
            } => target_interface,
        };
        if blocker_target != &baseline.target_interface {
            return Err("runtime restore blocker target identity changed".to_string());
        }
        let current_sqm_fingerprint = operations::sqm_identity::managed_sqm_identity_fingerprint(
            &self.controller.cfg.instance,
            &self.controller.cfg.sqm_section,
            &self.controller.cfg.sqm_interface,
        )?;
        if current_sqm_fingerprint != baseline.sqm_fingerprint {
            return Err(
                "saved baseline SQM fingerprint changed while restore was blocked".to_string(),
            );
        }

        match blocker {
            RuntimeRestoreBlocker::TargetUnavailable { .. } => {
                if managed_sqm_target_ready(&self.controller.cfg) {
                    Ok(RuntimeRestoreObservation::Ready)
                } else {
                    Ok(RuntimeRestoreObservation::Blocked(blocker.clone()))
                }
            }
            RuntimeRestoreBlocker::SqmRecoveryBusy { .. } => {
                // The native operation owns the interface lock while it is
                // restoring. Re-entering sqm-recover merely to observe that
                // restore can therefore return EX_TEMPFAIL forever even after
                // the helper has already installed the exact baseline. Inspect
                // the live topology directly under the existing owner instead
                // of trying to acquire the same lock recursively.
                let (download_shaped, upload_shaped) =
                    measurement_topology_directions(baseline.topology);
                match inspect_sqm_topology_for(&self.controller.cfg, download_shaped, upload_shaped)
                {
                    Ok(()) => {
                        let attested = attest_rate_only_runtime(self.controller, baseline)?;
                        if attested == *baseline {
                            Ok(RuntimeRestoreObservation::Ready)
                        } else {
                            Err(
                                "restored runtime does not exactly match the durable baseline"
                                    .to_string(),
                            )
                        }
                    }
                    Err(error) if error.kind == SqmTopologyErrorKind::Settling => {
                        let RuntimeActuatorError::Blocked(current) =
                            topology_settling_runtime_blocker(
                                &self.controller.cfg,
                                error.to_string(),
                            )
                        else {
                            unreachable!()
                        };
                        Ok(RuntimeRestoreObservation::Blocked(current))
                    }
                    Err(error) => Err(error.to_string()),
                }
            }
            RuntimeRestoreBlocker::TopologySettling { .. } => {
                let (download_shaped, upload_shaped) =
                    measurement_topology_directions(baseline.topology);
                match inspect_sqm_topology_for(&self.controller.cfg, download_shaped, upload_shaped)
                {
                    Ok(()) => {
                        let attested = attest_rate_only_runtime(self.controller, baseline)?;
                        if attested == *baseline {
                            Ok(RuntimeRestoreObservation::Ready)
                        } else {
                            Err(
                                "settled runtime does not exactly match the durable baseline"
                                    .to_string(),
                            )
                        }
                    }
                    Err(error) if error.kind == SqmTopologyErrorKind::Settling => {
                        let RuntimeActuatorError::Blocked(current) =
                            topology_settling_runtime_blocker(
                                &self.controller.cfg,
                                error.to_string(),
                            )
                        else {
                            unreachable!()
                        };
                        Ok(RuntimeRestoreObservation::Blocked(current))
                    }
                    Err(error) => Err(error.to_string()),
                }
            }
            RuntimeRestoreBlocker::RecoveryInterrupted { .. } => {
                if TERMINATE.load(Ordering::SeqCst) {
                    Ok(RuntimeRestoreObservation::Blocked(blocker.clone()))
                } else {
                    Ok(RuntimeRestoreObservation::Ready)
                }
            }
        }
    }

    fn attest_runtime(
        &mut self,
        expected: &operations::autotune_runtime::RuntimeSnapshot,
        checkpoint: &operations::autotune_runtime_store::RuntimeOverrideCheckpoint,
    ) -> Result<
        operations::autotune_runtime::RuntimeSnapshot,
        operations::autotune_runtime_driver::RuntimeActuatorError,
    > {
        if !managed_sqm_target_ready(&self.controller.cfg) {
            return Err(target_unavailable_runtime_blocker(
                &self.controller.cfg,
                "managed SQM target is temporarily unavailable during attestation",
            ));
        }
        if checkpoint.temporary_stage
            == operations::autotune_runtime::TemporaryTopologyStage::Active
        {
            attest_private_runtime(&self.controller.cfg.sqm_interface, expected, checkpoint)
                .map_err(unsafe_runtime_actuator_error)
        } else if checkpoint.temporary_stage
            == operations::autotune_runtime::TemporaryTopologyStage::BaselineRestored
        {
            let (download_shaped, upload_shaped) =
                measurement_topology_directions(expected.topology);
            inspect_sqm_topology_for(&self.controller.cfg, download_shaped, upload_shaped)
                .map_err(|error| classify_topology_error(&self.controller.cfg, error))?;
            attest_rate_only_runtime(self.controller, expected)
                .map_err(unsafe_runtime_actuator_error)
        } else {
            Err(unsafe_runtime_actuator_error(format!(
                "runtime attestation is not valid at temporary stage {}",
                checkpoint.temporary_stage.as_str()
            )))
        }
    }

    fn attest_restored_baseline(
        &mut self,
        baseline: &operations::autotune_runtime::RuntimeRestoreBaseline,
        checkpoint: &operations::autotune_runtime_store::RuntimeOverrideCheckpoint,
    ) -> Result<
        operations::autotune_runtime::RuntimeRestoreBaseline,
        operations::autotune_runtime_driver::RuntimeActuatorError,
    > {
        let managed = baseline
            .managed_snapshot()
            .map_err(unsafe_runtime_actuator_error)?;
        self.attest_runtime(managed, checkpoint)
            .map(operations::autotune_runtime::RuntimeBaseline::Managed)
    }

    fn finish_restored(&mut self) {
        self.controller.runtime_override_active = false;
        self.controller.last_shaper_attempt_dl = Instant::now();
        self.controller.last_shaper_attempt_ul = Instant::now();
    }

    fn recover_safe_configuration(&mut self) -> Result<(), String> {
        self.controller.runtime_override_active = true;
        let fresh_cfg = Config::from_uci(&self.controller.cfg.instance)
            .map_err(|error| format!("unable to reload current UCI for safe recovery: {error}"))?;
        if fresh_cfg.manage_sqm {
            recover_managed_sqm(&fresh_cfg).map_err(|error| error.message().to_string())?;
        }
        self.controller.set_sqm_runtime_status(
            "RECOVERY_REQUIRED",
            false,
            "native Auto-Tune restored current UCI and remains latched",
        );
        self.controller.set_run_state("RECOVERY_REQUIRED");
        // Keep the ordinary controller frozen.  The runtime driver remains
        // latched until an explicit recovery path resolves and removes the
        // untrusted ownership checkpoint; it must not resume with stale Config.
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MainState {
    Running,
    Idle,
    Stall,
}

fn probe_loop_required(connection_active: bool, operation_capture_active: bool) -> bool {
    connection_active || operation_capture_active
}

#[cfg(feature = "calibration")]
fn bounded_operation_capture_active(
    autotune_capture_active: bool,
    rating_capture_active: bool,
) -> bool {
    autotune_capture_active || rating_capture_active
}

#[cfg(feature = "calibration")]
fn transport_probe_runtime_required(
    configured: bool,
    autotune_capture_published: bool,
    rating_capture_active: bool,
) -> bool {
    configured || autotune_capture_published || rating_capture_active
}

#[cfg(feature = "calibration")]
fn runtime_driver_holds_controller(
    result: &Result<operations::autotune_runtime_driver::RuntimeDriverOutcome, String>,
) -> bool {
    !matches!(
        result,
        Ok(operations::autotune_runtime_driver::RuntimeDriverOutcome::Idle)
    )
}

fn idle_sleep_due(
    enable_sleep_function: bool,
    uplink_learning: bool,
    connection_active: bool,
    operation_capture_active: bool,
    idle_elapsed: Duration,
    idle_timeout: Duration,
) -> bool {
    enable_sleep_function
        && !uplink_learning
        && !probe_loop_required(connection_active, operation_capture_active)
        && idle_elapsed >= idle_timeout
}

fn idle_wake_due(
    connection_active: bool,
    operation_capture_active: bool,
    route_probes_allowed: bool,
) -> bool {
    route_probes_allowed && probe_loop_required(connection_active, operation_capture_active)
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
        if !interruptible_wait(Duration::from_secs_f64(cfg.startup_wait_s)) {
            return Ok(());
        }
    }
    cfg.refresh_wire_packet_sizes();
    match wait_for_runtime_topology(&cfg) {
        Ok(()) => {}
        Err(RuntimeTopologyWaitError::Terminated) => return Ok(()),
        Err(RuntimeTopologyWaitError::Failed(error)) => return Err(error),
    }

    let mut controller = Controller::new(cfg.clone())?;
    let route_spec = cfg.route_spec();
    let mut route_inspector = RouteInspector::new(route_spec.clone());
    let mut uplink_lifecycle = UplinkLifecycle::new();
    let initial_inspected = route_inspector.inspect_fresh();
    let (mut current_route_snapshot, initial_route_error) = match initial_inspected {
        Ok(snapshot) => (Some(snapshot), None),
        Err(error) => (None, Some(error)),
    };
    let initial_transition =
        uplink_lifecycle.observe(current_route_snapshot.as_ref().map(Ok).unwrap_or_else(|| {
            Err(initial_route_error
                .as_deref()
                .unwrap_or("route unavailable"))
        }));
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

    // The driver is part of the normal instance safety boundary even while
    // native operation admission remains disabled.  With no exact private
    // permit/control records it is inert; gating its construction would also
    // disable restart recovery for an override admitted before an instance
    // daemon restart.
    #[cfg(feature = "calibration")]
    let mut runtime_override_driver =
        operations::autotune_runtime_driver::RuntimeOverrideDriver::open(
            cfg.instance.clone(),
            &cfg.run_dir(),
        )
        .map_err(|error| format!("unable to initialize runtime override driver: {error}"))?;
    #[cfg(feature = "calibration")]
    let runtime_override_poll_interval = Duration::from_millis(100);
    #[cfg(feature = "calibration")]
    let mut last_runtime_override_poll = Instant::now()
        .checked_sub(runtime_override_poll_interval)
        .unwrap_or_else(Instant::now);
    #[cfg(feature = "calibration")]
    let mut last_runtime_override_error: Option<String> = None;

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
    let mut last_route_check = Instant::now();
    let sqm_health_fast_interval = Duration::from_secs(SQM_RUNTIME_HEALTH_CHECK_FAST_S);
    let sqm_health_healthy_interval = Duration::from_secs(SQM_RUNTIME_HEALTH_CHECK_HEALTHY_S);
    let mut last_sqm_health_check = Instant::now()
        .checked_sub(sqm_health_healthy_interval)
        .unwrap_or_else(Instant::now);
    let mut route_probes_allowed = initial_transition.probes_allowed;
    external_ip_probe.maybe_start(route_probes_allowed);

    while !TERMINATE.load(Ordering::SeqCst) {
        #[cfg(feature = "calibration")]
        if last_runtime_override_poll.elapsed() >= runtime_override_poll_interval {
            last_runtime_override_poll = Instant::now();
            let boot_ms = operations::identity::monotonic_boot_ms().unwrap_or(0);
            let mut actuator = OpenWrtRateOverrideActuator {
                controller: &mut controller,
                boot_ms,
            };
            let runtime_poll = runtime_override_driver.poll(&mut actuator);
            controller.runtime_operation_active = runtime_driver_holds_controller(&runtime_poll);
            match runtime_poll {
                Ok(_) => last_runtime_override_error = None,
                Err(error) => {
                    // An unreadable runtime owner is not proof that ordinary
                    // control may resume. The helper above keeps writes held
                    // until a later successful poll proves exact Idle state.
                    if last_runtime_override_error.as_deref() != Some(error.as_str()) {
                        controller.log(
                            "ERROR",
                            &format!("native Auto-Tune runtime control failed: {error}"),
                        );
                        last_runtime_override_error = Some(error);
                    }
                }
            }
            controller.sync_autotune_capture_admission();
        }
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
            let transition =
                uplink_lifecycle.observe(inspected.as_ref().map_err(|error| error.as_str()));
            let must_stop = transition.became_offline
                || transition.identity_changed
                || (route_probes_allowed && !transition.probes_allowed);
            if must_stop {
                if let Some(mut old) = pinger.take() {
                    old.stop();
                }
                controller.note_probe_gap();
            }
            match inspected {
                Ok(snapshot) => current_route_snapshot = Some(snapshot),
                Err(_) if transition.state == UplinkState::Rechecking => {}
                Err(_) => current_route_snapshot = None,
            }
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
                    } else if pinger_line_is_timeout(&cfg.pinger_method, &line.line) {
                        #[cfg(feature = "calibration")]
                        controller.observe_autotune_icmp_timeout();
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
                    let transition = uplink_lifecycle
                        .observe(inspected.as_ref().map_err(|error| error.as_str()));
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
                    let transition = uplink_lifecycle
                        .observe(inspected.as_ref().map_err(|error| error.as_str()));
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
        #[cfg(feature = "calibration")]
        let autotune_rate_sample = controller.autotune_observation_rates(rate_sample);
        #[cfg(feature = "calibration")]
        if let Some(autotune_rate_sample) = autotune_rate_sample {
            controller.observe_autotune_traffic(autotune_rate_sample);
        }
        #[cfg(feature = "calibration")]
        controller.sync_rating_capture(now);
        let rating_load = if rate_sample.fresh {
            controller.update_rating_load(now, dl_rate, ul_rate)
        } else {
            controller.rating_load_snapshot.clone()
        };
        #[cfg(feature = "calibration")]
        {
            // Rating and Full Auto-Tune require direction-bound transport
            // evidence even when continuous transport monitoring is disabled.
            // Own the probe runtime only for the exact published capture and
            // drop it again after cleanup; this never changes UCI or enables
            // the ordinary transport controller.
            let required = transport_probe_runtime_required(
                cfg.transport_latency_enabled,
                controller.autotune_capture_request.is_some(),
                rating_load.capture_active,
            );
            if required && transport_probe.is_none() {
                transport_probe = Some(TransportProbeRuntime::spawn(&cfg));
            } else if !required && transport_probe.is_some() {
                transport_probe = None;
            }
        }
        #[cfg(feature = "calibration")]
        if let Some(runtime) = transport_probe.as_mut() {
            let capture_control = controller.autotune_capture_control_request();
            let autotune_capture = controller.active_autotune_observation_request();
            let route_identity = controller.route_identity.clone();
            let transport_rates = if capture_control.is_some() {
                autotune_rate_sample
            } else {
                Some(rate_sample)
            };
            let transport_counter_delta = controller.autotune_transport_counter_delta();
            if let Err(error) = runtime.observe_autotune_capture_phase(
                &cfg,
                transport_rates,
                transport_counter_delta,
                capture_control.as_ref(),
                route_identity.as_deref(),
                now,
            ) {
                controller
                    .reject_autotune_observations("capture-transport-load-phase-invalid", &error);
            }
            // Drain only after this loop has refreshed and appended the
            // topology-specific phase observation.  Capture results are
            // admitted from their whole identity-bound flight interval, not
            // from a single rate sample taken after completion.
            runtime.drain(&mut controller, &cfg);
            if capture_control.is_some() && autotune_capture.is_none() {
                controller.record_transport_rejection("capture_attestation_lapsed");
            } else if autotune_capture.is_some() && transport_rates.is_none() {
                controller.record_transport_rejection("capture_rates_unavailable");
            }
            if route_probes_allowed && !(capture_control.is_some() && autotune_capture.is_none()) {
                let (dl_shaper, ul_shaper) = controller.shaper_rates();
                let quality_baseline_ready = controller
                    .quality_grade
                    .snapshot(epoch_secs())
                    .baseline_ready;
                if let Err(error) = runtime.maybe_start(
                    &cfg,
                    transport_rates,
                    (dl_shaper, ul_shaper),
                    &rating_load,
                    quality_baseline_ready,
                    autotune_capture,
                    route_identity.as_deref(),
                    now,
                ) {
                    controller.reject_autotune_observations(
                        "capture-transport-load-phase-invalid",
                        &error,
                    );
                }
            }
        }
        #[cfg(not(feature = "calibration"))]
        if let Some(runtime) = transport_probe.as_mut() {
            runtime.drain(&mut controller, &cfg);
            if route_probes_allowed {
                let (dl_shaper, ul_shaper) = controller.shaper_rates();
                let quality_baseline_ready = controller
                    .quality_grade
                    .snapshot(epoch_secs())
                    .baseline_ready;
                runtime.maybe_start(
                    &cfg,
                    Some(rate_sample),
                    (dl_shaper, ul_shaper),
                    &rating_load,
                    quality_baseline_ready,
                    now,
                )?;
            }
        }
        if controller.maybe_sample_cpu() {
            let _ = controller.refresh_status_from_last_sample();
        }
        controller.maybe_record_graph_history(dl_rate, ul_rate);
        let connection_active =
            dl_rate > cfg.connection_active_thr_kbps || ul_rate > cfg.connection_active_thr_kbps;
        // An admitted bounded operation capture is an identity-bound
        // measurement contract. Keep the configured probe loop awake even
        // when the link itself is quiet; otherwise the ordinary sustained-idle
        // transition can stop pingers before the capture reaches its bounded
        // sample minimum. Invalid, stale, or completed captures do not inhibit
        // sleep because neither capture source remains active.
        #[cfg(feature = "calibration")]
        let autotune_capture_active = controller.active_autotune_observation_request().is_some();
        #[cfg(feature = "calibration")]
        let operation_capture_active =
            bounded_operation_capture_active(autotune_capture_active, rating_load.capture_active);
        #[cfg(not(feature = "calibration"))]
        let operation_capture_active = false;
        let probes_required = probe_loop_required(connection_active, operation_capture_active);
        let stall_load_active =
            dl_rate > cfg.connection_stall_thr_kbps && ul_rate > cfg.connection_stall_thr_kbps;

        match main_state {
            MainState::Running => {
                if cfg.enable_sleep_function && uplink_lifecycle.state() != UplinkState::Learning {
                    if probes_required {
                        idle_since = None;
                    } else {
                        let idle_start = *idle_since.get_or_insert(now);
                        if idle_sleep_due(
                            cfg.enable_sleep_function,
                            uplink_lifecycle.state() == UplinkState::Learning,
                            connection_active,
                            operation_capture_active,
                            now.duration_since(idle_start),
                            idle_timeout,
                        ) {
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
                if idle_wake_due(
                    connection_active,
                    operation_capture_active,
                    route_probes_allowed,
                ) {
                    if connection_active {
                        controller.log(
                            "DEBUG",
                            &format!(
                                "dl achieved rate: {:.0} kbps or ul achieved rate: {:.0} kbps exceeded connection active threshold: {:.0} kbps. Resuming normal operation.",
                                dl_rate,
                                ul_rate,
                                cfg.connection_active_thr_kbps
                            ),
                        );
                    } else {
                        controller.log(
                            "DEBUG",
                            "Native Auto-Tune capture admitted. Resuming latency probes.",
                        );
                    }
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
    #[cfg(feature = "calibration")]
    observed_at: Instant,
    #[cfg(feature = "calibration")]
    observed_epoch_secs: f64,
}

struct PingerRuntime {
    children: Vec<Child>,
    readers: Vec<JoinHandle<()>>,
    lines: Receiver<Result<PingerLine, String>>,
    #[cfg(feature = "calibration")]
    wake: Option<Arc<OwnedFd>>,
}

impl PingerRuntime {
    fn spawn(cfg: &Config, active_reflectors: &[String]) -> Result<Self, String> {
        let children = spawn_pingers(cfg, active_reflectors)?;
        Self::from_children(children, cfg.pinger_method.clone(), active_reflectors, None)
    }

    #[cfg(feature = "calibration")]
    fn spawn_bootstrap(request: &operations::protocol::OperationRequest) -> Result<Self, String> {
        request.validate()?;
        if request.target_state != operations::protocol::OperationTargetState::AbsentBootstrap {
            return Err("bootstrap pinger requires an absent target request".to_string());
        }
        let policy = request
            .capture_policy
            .ok_or_else(|| "bootstrap pinger has no capture policy".to_string())?
            .expand()?;
        if policy.pinger_method() != "fping" {
            return Err("bootstrap capture policy requires an unsupported pinger".to_string());
        }
        let active_reflectors = policy
            .reflectors()
            .iter()
            .take(usize::from(policy.active_pingers()))
            .cloned()
            .collect::<Vec<_>>();
        if active_reflectors.is_empty() {
            return Err("bootstrap capture policy has no active reflectors".to_string());
        }
        let route_spec = RouteSpec::new(
            request.route.mode.as_str(),
            request.route.mwan3_member.as_deref().unwrap_or(""),
            &request.route.l3_device,
        );
        route_spec.validate()?;
        let period_ms = u64::from(policy.reflector_ping_interval_ms());
        let interval_ms = (period_ms / active_reflectors.len() as u64).max(1);
        let mut command = routing::routed_command(&route_spec, "", "fping")?;
        command
            .arg("-I")
            .arg(&request.route.l3_device)
            .arg("--timestamp")
            .arg("--loop")
            .arg("--period")
            .arg(period_ms.to_string())
            .arg("--interval")
            .arg(interval_ms.to_string())
            .arg("--timeout")
            .arg(policy.pinger_timeout_ms().to_string())
            .args(&active_reflectors)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let child = command
            .spawn()
            .map_err(|error| format!("failed to start bootstrap fping: {error}"))?;
        let raw_wake = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        if raw_wake < 0 {
            let mut child = child;
            stop_child(&mut child);
            return Err(format!(
                "failed to create bootstrap pinger wake descriptor: {}",
                io::Error::last_os_error()
            ));
        }
        let wake = Arc::new(unsafe { OwnedFd::from_raw_fd(raw_wake) });
        Self::from_children(
            vec![child],
            policy.pinger_method().to_string(),
            &active_reflectors,
            Some(wake),
        )
    }

    fn from_children(
        mut children: Vec<Child>,
        method: String,
        active_reflectors: &[String],
        wake: Option<Arc<OwnedFd>>,
    ) -> Result<Self, String> {
        let (tx, lines) = mpsc::channel();
        let mut readers = Vec::new();

        for idx in 0..children.len() {
            let stdout = match children[idx].stdout.take() {
                Some(stdout) => stdout,
                None => {
                    for child in &mut children {
                        stop_child(child);
                    }
                    return Err(format!("failed to capture {method} stdout"));
                }
            };
            let tx = tx.clone();
            let method = method.clone();
            let wake_writer = wake.clone();
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
                                #[cfg(feature = "calibration")]
                                observed_at: Instant::now(),
                                #[cfg(feature = "calibration")]
                                observed_epoch_secs: epoch_secs(),
                            };
                            if tx.send(Ok(event)).is_err() {
                                break;
                            }
                            notify_pinger_wake(wake_writer.as_deref());
                        }
                        Err(e) => {
                            let _ = tx.send(Err(format!("failed to read {method} output: {e}")));
                            notify_pinger_wake(wake_writer.as_deref());
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
            #[cfg(feature = "calibration")]
            wake,
        })
    }

    #[cfg(feature = "calibration")]
    fn wake_fd(&self) -> RawFd {
        self.wake.as_ref().map_or(-1, |wake| wake.as_raw_fd())
    }

    #[cfg(feature = "calibration")]
    fn drain_wake(&self) -> Result<(), String> {
        let Some(wake) = self.wake.as_ref() else {
            return Ok(());
        };
        loop {
            let mut value = 0_u64;
            let read = unsafe {
                libc::read(
                    wake.as_raw_fd(),
                    (&mut value as *mut u64).cast::<libc::c_void>(),
                    std::mem::size_of::<u64>(),
                )
            };
            if read == std::mem::size_of::<u64>() as isize {
                continue;
            }
            if read < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::WouldBlock {
                    return Ok(());
                }
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(format!("failed to drain bootstrap pinger wake: {error}"));
            }
            return Err("bootstrap pinger wake descriptor returned a short read".to_string());
        }
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

fn notify_pinger_wake(wake: Option<&OwnedFd>) {
    let Some(wake) = wake else {
        return;
    };
    let value = 1_u64;
    let _ = unsafe {
        libc::write(
            wake.as_raw_fd(),
            (&value as *const u64).cast::<libc::c_void>(),
            std::mem::size_of::<u64>(),
        )
    };
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

fn pinger_line_is_timeout(method: &str, line: &str) -> bool {
    match method {
        "fping" | "fping-ts" => line.contains("timed out") && line.contains("100% loss"),
        "ping" => line.contains("no answer yet"),
        _ => false,
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

fn interruptible_wait(duration: Duration) -> bool {
    let deadline = Instant::now() + duration;
    while !TERMINATE.load(Ordering::SeqCst) {
        let now = Instant::now();
        if now >= deadline {
            return true;
        }
        thread::sleep(
            deadline
                .saturating_duration_since(now)
                .min(Duration::from_millis(200)),
        );
    }
    false
}

fn write_bootstrap_status(
    cfg: &Config,
    state: &str,
    sqm_state: &str,
    reason: &str,
    attempts: u64,
    started_at: f64,
) -> io::Result<()> {
    ensure_run_dir(&cfg.run_dir())?;
    let path = cfg.run_dir().join("status.json");
    let tmp = cfg.run_dir().join("status.json.tmp");
    let uplink_state = if state == "WAITING_LINK" {
        "OFFLINE"
    } else {
        "LEARNING"
    };
    let mut file = File::create(&tmp)?;
    writeln!(
        file,
        "{{\"instance\":\"{}\",\"version\":\"{}\",\"state\":\"{}\",\"uplink_state\":\"{}\",\"uplink_reason\":\"{}\",\"sqm_runtime_managed\":{},\"sqm_runtime_state\":\"{}\",\"sqm_runtime_healthy\":false,\"sqm_runtime_reason\":\"{}\",\"sqm_recovery_attempts\":{},\"sqm_last_recovery_at\":null,\"started_at\":{:.6},\"updated_at\":{:.6},\"dl_if\":\"{}\",\"ul_if\":\"{}\",\"reflector\":\"\",\"seq\":\"\",\"rtt_ms\":0,\"dl_achieved_rate_kbps\":0,\"ul_achieved_rate_kbps\":0,\"cake_dl_rate_kbps\":{:.0},\"cake_ul_rate_kbps\":{:.0},\"quality_class\":\"LEARNING\",\"quality_dl_class\":\"LEARNING\",\"quality_ul_class\":\"LEARNING\",\"quality_confidence\":0,\"quality_reason\":\"{}\",\"active_reflectors\":[],\"spare_reflectors\":[],\"bad_reflectors\":[],\"reflector_health\":[]}}",
        json_escape(&cfg.instance),
        env!("CARGO_PKG_VERSION"),
        json_escape(state),
        uplink_state,
        json_escape(reason),
        cfg.manage_sqm && cfg.sqm_enabled,
        json_escape(sqm_state),
        json_escape(reason),
        attempts,
        started_at,
        epoch_secs(),
        json_escape(&cfg.dl_if),
        json_escape(&cfg.ul_if),
        cfg.base_dl_shaper_rate_kbps,
        cfg.base_ul_shaper_rate_kbps,
        json_escape(state)
    )?;
    file.sync_all()?;
    fs::rename(tmp, path)
}

#[derive(Debug, PartialEq, Eq)]
enum RuntimeTopologyWaitError {
    Terminated,
    Failed(String),
}

impl From<String> for RuntimeTopologyWaitError {
    fn from(error: String) -> Self {
        Self::Failed(error)
    }
}

fn wait_for_runtime_topology(cfg: &Config) -> Result<(), RuntimeTopologyWaitError> {
    use operations::sqm_recovery::{SqmRecoveryAdmission, SqmRecoveryGate};

    let started_at = epoch_secs();
    let poll_interval = Duration::from_secs_f64(cfg.if_up_check_interval_s.clamp(1.0, 10.0));
    let mut recovery_attempts = 0_u64;
    let mut last_report = (String::new(), String::new());
    let mut recovery_gate = SqmRecoveryGate::default();

    loop {
        if TERMINATE.load(Ordering::SeqCst) {
            return Err(RuntimeTopologyWaitError::Terminated);
        }

        let target_ready = managed_sqm_target_ready(cfg);
        if !target_ready {
            recovery_gate.observe_target_missing();
            let state = "WAITING_LINK";
            let reason = format!(
                "waiting for target interface {} and its counters",
                cfg.sqm_interface
            );
            if last_report != (state.to_string(), reason.clone()) {
                eprintln!("{state}: {reason}");
                last_report = (state.to_string(), reason.clone());
            }
            write_bootstrap_status(cfg, state, state, &reason, recovery_attempts, started_at)
                .map_err(|error| format!("failed to publish bootstrap status: {error}"))?;
            if !interruptible_wait(poll_interval) {
                return Err(RuntimeTopologyWaitError::Terminated);
            }
            continue;
        }

        let topology = inspect_sqm_topology(cfg);
        let initial_reason = topology
            .as_ref()
            .err()
            .map(ToString::to_string)
            .unwrap_or_else(|| "managed SQM failed exact attestation".to_string());

        if !cfg.manage_sqm || !cfg.sqm_enabled {
            if topology.is_ok() {
                recovery_gate.observe_healthy();
                return Ok(());
            }
            let state = "WAITING_EXTERNAL_SQM";
            if last_report != (state.to_string(), initial_reason.clone()) {
                eprintln!("{state}: {initial_reason}");
                last_report = (state.to_string(), initial_reason.clone());
            }
            write_bootstrap_status(
                cfg,
                state,
                state,
                &initial_reason,
                recovery_attempts,
                started_at,
            )
            .map_err(|error| format!("failed to publish bootstrap status: {error}"))?;
            if !interruptible_wait(poll_interval) {
                return Err(RuntimeTopologyWaitError::Terminated);
            }
            continue;
        }

        let check_error = match attest_managed_sqm(cfg) {
            Ok(()) if topology.is_ok() => {
                recovery_gate.observe_healthy();
                return Ok(());
            }
            Ok(()) => match inspect_sqm_topology(cfg) {
                Ok(()) => {
                    recovery_gate.observe_healthy();
                    return Ok(());
                }
                Err(current) => {
                    let state = "WAITING_SQM";
                    let reason = format!(
                        "{current}; exact SQM check observed a concurrent topology transition; re-reading current state"
                    );
                    if last_report != (state.to_string(), reason.clone()) {
                        eprintln!("{state}: {reason}");
                        last_report = (state.to_string(), reason.clone());
                    }
                    write_bootstrap_status(
                        cfg,
                        state,
                        state,
                        &reason,
                        recovery_attempts,
                        started_at,
                    )
                    .map_err(|error| format!("failed to publish bootstrap status: {error}"))?;
                    if !interruptible_wait(poll_interval) {
                        return Err(RuntimeTopologyWaitError::Terminated);
                    }
                    continue;
                }
            },
            Err(SqmRecoveryError::Busy(reason)) => {
                let state = "WAITING_OPERATION";
                if last_report != (state.to_string(), reason.clone()) {
                    eprintln!("{state}: {reason}");
                    last_report = (state.to_string(), reason.clone());
                }
                write_bootstrap_status(cfg, state, state, &reason, recovery_attempts, started_at)
                    .map_err(|error| format!("failed to publish bootstrap status: {error}"))?;
                if !interruptible_wait(poll_interval) {
                    return Err(RuntimeTopologyWaitError::Terminated);
                }
                continue;
            }
            Err(SqmRecoveryError::Failed(reason)) => reason,
            Err(SqmRecoveryError::Terminated) => {
                return Err(RuntimeTopologyWaitError::Terminated);
            }
        };

        let generation = sqm_topology_generation(cfg, &topology);
        if recovery_gate.admission(&generation) == SqmRecoveryAdmission::WaitForStateChange {
            let state = "WAITING_SQM";
            let reason = format!(
                "{initial_reason}; previous recovery failed and the observed link/SQM topology is unchanged: {check_error}"
            );
            if last_report != (state.to_string(), reason.clone()) {
                eprintln!("{state}: {reason}");
                last_report = (state.to_string(), reason.clone());
            }
            write_bootstrap_status(cfg, state, state, &reason, recovery_attempts, started_at)
                .map_err(|error| format!("failed to publish bootstrap status: {error}"))?;
            if !interruptible_wait(poll_interval) {
                return Err(RuntimeTopologyWaitError::Terminated);
            }
            continue;
        }

        recovery_attempts = recovery_attempts.saturating_add(1);
        let recovering_reason = format!(
            "{initial_reason}; exact read-only check is unhealthy: {check_error}; starting state-authorized recovery attempt {recovery_attempts}"
        );
        write_bootstrap_status(
            cfg,
            "RECOVERING",
            "RECOVERING",
            &recovering_reason,
            recovery_attempts,
            started_at,
        )
        .map_err(|error| format!("failed to publish bootstrap status: {error}"))?;
        match recover_managed_sqm(cfg) {
            Ok(()) => {
                recovery_gate.observe_healthy();
                return Ok(());
            }
            Err(SqmRecoveryError::Busy(reason)) => {
                let state = "WAITING_OPERATION";
                let detail = format!("{initial_reason}; recovery deferred: {reason}");
                if last_report != (state.to_string(), detail.clone()) {
                    eprintln!("{state}: {detail}");
                    last_report = (state.to_string(), detail.clone());
                }
                write_bootstrap_status(cfg, state, state, &detail, recovery_attempts, started_at)
                    .map_err(|error| format!("failed to publish bootstrap status: {error}"))?;
            }
            Err(SqmRecoveryError::Failed(reason)) => {
                let post_topology = inspect_sqm_topology(cfg);
                recovery_gate.record_failed(sqm_topology_generation(cfg, &post_topology));
                let state = "WAITING_SQM";
                let detail = format!(
                    "{initial_reason}; recovery attempt {recovery_attempts} failed: {reason}; no retry until observed topology changes"
                );
                if last_report != (state.to_string(), detail.clone()) {
                    eprintln!("{state}: {detail}");
                    last_report = (state.to_string(), detail.clone());
                }
                write_bootstrap_status(cfg, state, state, &detail, recovery_attempts, started_at)
                    .map_err(|error| format!("failed to publish bootstrap status: {error}"))?;
            }
            Err(SqmRecoveryError::Terminated) => {
                return Err(RuntimeTopologyWaitError::Terminated);
            }
        }

        if !interruptible_wait(poll_interval) {
            return Err(RuntimeTopologyWaitError::Terminated);
        }
    }
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

struct AdaptiveCapacityStatusContext<'a> {
    enabled: bool,
    route_epoch: Option<&'a str>,
    download: &'a AdaptiveCeilingDirection,
    upload: &'a AdaptiveCeilingDirection,
    current_download_kbps: f64,
    current_upload_kbps: f64,
    runtime_minimum_download_kbps: f64,
    runtime_minimum_upload_kbps: f64,
    transport_confidence_download: u8,
    transport_confidence_upload: u8,
    no_cake_effect_download: Option<bool>,
    no_cake_effect_upload: Option<bool>,
    causal_state_download: &'a str,
    causal_state_upload: &'a str,
}

fn adaptive_capacity_direction_status_json(
    direction: &AdaptiveCeilingDirection,
    current_rate_kbps: f64,
    runtime_minimum_kbps: f64,
    transport_confidence: u8,
    no_cake_effect: Option<bool>,
    causal_state: &str,
) -> String {
    let no_cake_effect_json = match no_cake_effect {
        Some(value) => value.to_string(),
        None => "null".to_string(),
    };
    format!(
        "{{\"phase\":\"{}\",\"current_rate_kbps\":{:.0},\"raw_capacity_kbps\":null,\"safe_ceiling_kbps\":{:.0},\"failed_bound_kbps\":{},\"runtime_minimum_kbps\":{:.0},\"exploration_minimum_kbps\":null,\"probe_target_kbps\":{},\"no_cake_effect\":{},\"causal_state\":\"{}\",\"last_validation_at\":null,\"confidence\":{{\"state\":\"passive_transport\",\"percent\":{},\"transport_percent\":{}}}}}",
        direction.phase().as_str(),
        current_rate_kbps,
        direction.safe_ceiling_kbps(),
        json_f64_or_null(direction.failed_ceiling_kbps(), 0),
        runtime_minimum_kbps,
        json_f64_or_null(direction.probe_target_kbps(), 0),
		no_cake_effect_json,
		json_escape(causal_state),
		transport_confidence,
        transport_confidence,
    )
}

fn adaptive_capacity_status_json(context: AdaptiveCapacityStatusContext<'_>) -> String {
    format!(
		"{{\"schema_version\":1,\"behavior_version\":\"passive_transport_v1\",\"enabled\":{},\"route_epoch\":{},\"download\":{},\"upload\":{}}}",
        context.enabled,
        json_string_or_null(context.route_epoch),
        adaptive_capacity_direction_status_json(
            context.download,
            context.current_download_kbps,
            context.runtime_minimum_download_kbps,
            context.transport_confidence_download,
			context.no_cake_effect_download,
			context.causal_state_download,
        ),
        adaptive_capacity_direction_status_json(
            context.upload,
            context.current_upload_kbps,
            context.runtime_minimum_upload_kbps,
            context.transport_confidence_upload,
			context.no_cake_effect_upload,
			context.causal_state_upload,
        ),
    )
}

fn quality_grade_metric_json(metric: Option<&QualityGradeMetric>) -> String {
    let Some(metric) = metric else {
        return "null".to_string();
    };
    format!(
        concat!(
            "{{\"grade\":\"{}\",\"increase_ms\":{:.3},",
            "\"loaded_p90_ms\":{:.3},\"samples\":{},",
            "\"evidence_source\":\"{}\",",
            "\"icmp_basis\":\"controller_reflector_adaptive_baseline\",",
            "\"transport_basis\":\"endpoint_loaded_p90_minus_idle_p5\",",
            "\"icmp_increase_ms\":{:.3},\"transport_increase_ms\":{:.3},",
            "\"icmp_samples\":{},\"transport_samples\":{}}}"
        ),
        metric.class.as_str(),
        metric.increase_ms,
        metric.loaded_p90_ms,
        metric.samples,
        metric.evidence_source,
        metric.icmp_increase_ms,
        metric.transport_increase_ms,
        metric.icmp_samples,
        metric.transport_samples,
    )
}

fn quality_grade_result_json(result: Option<&QualityGradeResult>, stale: bool) -> String {
    let Some(result) = result else {
        return "null".to_string();
    };
    let published_class = if result.partial || result.incomplete {
        QualityClass::Learning
    } else {
        result.class
    };
    format!(
        "{{\"grade\":\"{}\",\"increase_ms\":{:.3},\"baseline_p5_ms\":{:.3},\"endpoint\":\"{}\",\"started_at\":{:.3},\"completed_at\":{},\"route_identity\":\"{}\",\"partial\":{},\"incomplete\":{},\"completion_reason\":\"{}\",\"stale\":{},\"samples\":{},\"dl_samples\":{},\"ul_samples\":{},\"bidirectional_samples\":{},\"dl\":{},\"ul\":{},\"bidirectional\":{}}}",
        published_class.as_str(),
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
    adaptive_dl_phase: &str,
    adaptive_ul_phase: &str,
    adaptive_dl_reason: &str,
    adaptive_ul_reason: &str,
    causal_dl_state: &str,
    causal_ul_state: &str,
    sqm_runtime_state: &str,
) -> String {
    format!(
        "{:.0},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
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
        adaptive_dl_phase.replace(',', ""),
        adaptive_ul_phase.replace(',', ""),
        adaptive_dl_reason.replace(',', ""),
        adaptive_ul_reason.replace(',', ""),
        causal_dl_state.replace(',', ""),
        causal_ul_state.replace(',', ""),
        sqm_runtime_state.replace(',', ""),
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
    operations::autotune_capture_policy::standard_v1_reflectors()
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

#[cfg(feature = "calibration")]
const CALIBRATIONCTL_SCHEDULER_STATUS_USAGE: &str =
    "       cake-autorated --calibrationctl [--state-dir RAM_PATH] scheduler-status";

fn print_usage() {
    eprintln!("usage: cake-autorated [--instance NAME] [--once] [--dump-config]");
    #[cfg(feature = "calibration")]
    print_calibration_usage();
    eprintln!("       cake-autorated --transport-probe --backend websocket|tcp|http|legacy-http [--endpoint URL] [--device IFACE] [--source-ip IPv4] [--fwmark HEX] [--count N] [--timeout SEC] [--interval-ms N]");
}

#[cfg(feature = "calibration")]
fn print_calibration_usage() {
    eprintln!("       cake-autorated --calibrationd [--state-dir RAM_PATH] [--native-rating] [--native-speedtest] [--native-autotune]");
    eprintln!("         production scheduler: --native-scheduler --scheduler-store-dir /etc/cake-autorate-rs-scheduler");
    eprintln!("         laboratory only: [--lab-rust-rating] [--lab-rust-speedtest] [--lab-rust-autotune]");
    eprintln!(
        "         isolated scheduler: --lab-rust-scheduler --scheduler-store-dir PRIVATE_PATH"
    );
    eprintln!("       cake-autorated --calibrationctl [--state-dir RAM_PATH] ping|summary|start REQUEST|status REQUEST|result REQUEST|cancel REQUEST");
    eprintln!("       cake-autorated --calibrationctl [--state-dir RAM_PATH] scheduler-acknowledge-accounting INSTANCE");
    eprintln!("{CALIBRATIONCTL_SCHEDULER_STATUS_USAGE}");
    eprintln!("       cake-autorated --calibrationctl [--state-dir RAM_PATH] autotune-inspect OPTIONS | autotune-start OPTIONS | autotune-bootstrap-start SQM_SECTION OPTIONS | autotune-status JOB_ID | autotune-result JOB_ID | autotune-cancel JOB_ID");
    eprintln!("       cake-autorated --bootstrap-runtime-owner --request PATH --runtime-dir PATH --worker-run-id HEX");
    eprintln!("       cake-autorated --calibrationctl [--state-dir RAM_PATH] autotune-current INSTANCE | rating-start OPTIONS | rating-current INSTANCE | rating-status JOB_ID | rating-result JOB_ID | rating-cancel JOB_ID");
    eprintln!("       cake-autorated --calibrationctl [--state-dir RAM_PATH] speedtest-start OPTIONS | speedtest-current INSTANCE | speedtest-status JOB_ID | speedtest-result JOB_ID | speedtest-cancel JOB_ID");
    eprintln!("       cake-autorated --calibrationctl [--state-dir RAM_PATH] autotune-apply-check JOB_ID [OPTION_ID]");
    eprintln!("       cake-autorated --calibrationctl [--state-dir RAM_PATH] autotune-apply JOB_ID OPTION_ID REVIEW_SHA256 MANIFEST_SHA256 [--ack CODE]...");
    eprintln!("       cake-autorated --native-apply-recover");
    eprintln!("       cake-autorated --calibration-capabilities");
    eprintln!(
        "       cake-autorated --autotune-proposal --dl-samples LIST --ul-samples LIST \\\n         --idle-median-ms N --idle-p95-ms N --idle-samples N [--link-kind KIND] \\\n         [--profile gaming|best_overall|variable_link|fair] \\\n         [--base-scale N | --dl-base-scale N --ul-base-scale N] \\\n         [--dl-runtime-min-kbps N] [--ul-runtime-min-kbps N] \\\n         [--dl-measurement-base-kbps N --ul-measurement-base-kbps N]"
    );
    eprintln!("       cake-autorated --autotune-validate [--profile gaming|gaming_extreme|best_overall|variable_link|fair] --dl-observed-low-kbps N --ul-observed-low-kbps N --dl-candidate-kbps N --ul-candidate-kbps N --dl-achieved-kbps N --ul-achieved-kbps N --dl-min-kbps N --ul-min-kbps N --dl-max-kbps N --ul-max-kbps N --icmp-delta-ms N --transport-delta-ms N --loss-percent N --cpu-percent N");
    eprintln!("         [--dl-icmp-delta-ms N --ul-icmp-delta-ms N --dl-transport-delta-ms N --ul-transport-delta-ms N --dl-loss-percent N --ul-loss-percent N --dl-cpu-percent N --ul-cpu-percent N]");
    eprintln!("       cake-autorated --autotune-optimize-direction --profile gaming|gaming_extreme|best_overall|variable_link|fair --direction download|upload --observed-low-kbps N --minimum-kbps N --upper-kbps N --observations C,A,I,T,L,P[;...] [--uncertainty-percent N] [--max-attempts N] [--terminate-at-measured-boundary candidate-transfer-unmeasurable]");
}

#[cfg(feature = "calibration")]
const CALIBRATION_CAPABILITIES_V3: &str =
    "cake-autorate-calibration-capabilities 3 native-autotune native-rating native-scheduler native-speedtest";

#[cfg(feature = "calibration")]
fn calibration_capabilities<I>(mut args: I) -> Result<&'static str, String>
where
    I: Iterator<Item = String>,
{
    if args.next().is_some() {
        return Err("calibration-capabilities accepts no options".to_string());
    }
    Ok(CALIBRATION_CAPABILITIES_V3)
}

#[cfg(feature = "calibration")]
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

#[cfg(feature = "calibration")]
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

#[cfg(feature = "calibration")]
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

#[cfg(feature = "calibration")]
fn parse_cli_f64(name: &str, value: &str) -> Result<f64, String> {
    let parsed = value
        .parse::<f64>()
        .map_err(|_| format!("invalid {name}: {value}"))?;
    if !parsed.is_finite() {
        return Err(format!("{name} must be finite"));
    }
    Ok(parsed)
}

#[cfg(feature = "calibration")]
fn parse_strict_bool(name: &str, value: &str) -> Result<bool, String> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(format!("{name} must be exactly 0 or 1")),
    }
}

/// Validate background metadata without changing the isolated speed-test
/// samples. The speed-test helper reports only its own flow rate; forwarded
/// client traffic is measured separately and must not be subtracted here.
#[cfg(feature = "calibration")]
fn validated_conservative_samples(
    samples: &[f64],
    background_kbps: Option<f64>,
) -> Result<&[f64], String> {
    let Some(background_kbps) = background_kbps else {
        return Ok(samples);
    };
    if !background_kbps.is_finite()
        || !(0.0..=autotune::MAX_RATE_KBPS as f64).contains(&background_kbps)
    {
        return Err(format!(
            "conservative background must be finite and between 0 and {} kbit/s",
            autotune::MAX_RATE_KBPS
        ));
    }
    Ok(samples)
}

#[cfg(feature = "calibration")]
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
        exploration_minimum_kbps: minimum,
        runtime_minimum_kbps: Some(minimum),
        base_kbps: base,
        maximum_kbps: maximum,
        tested_safe_maximum_kbps: Some(maximum),
        exploration_cap_kbps: cap,
        absolute_cap_kbps: cap,
        service_hard_cap_kbps: None,
        ceiling_evidence: autotune::CeilingEvidence::RetainedConfiguration,
        cap_source: autotune::CeilingCapSource::RetainedConfiguration,
        observed_low_kbps: observed.observed_low_kbps,
        observed_median_kbps: observed.observed_median_kbps,
        observed_high_kbps: observed.observed_high_kbps,
        variability: observed.variability,
    })
}

#[cfg(feature = "calibration")]
fn run_autotune_proposal_cli<I>(args: I) -> Result<(), String>
where
    I: Iterator<Item = String>,
{
    use autotune::{
        build_proposal_for_profile_with_context, AccessEvidenceSource, AccessMedium,
        AutotuneProfile, CapacityLearningPolicy, LatencyBaseline, LinkKind, ProposalContext,
    };

    let mut download = None;
    let mut upload = None;
    let mut idle_median_ms = None;
    let mut idle_p95_ms = None;
    let mut idle_samples = None;
    let mut base_scale = 1.0;
    let mut download_base_scale = None;
    let mut upload_base_scale = None;
    let mut download_measurement_base = None;
    let mut upload_measurement_base = None;
    let mut download_runtime_minimum = None;
    let mut upload_runtime_minimum = None;
    let mut download_tested_safe_maximum = None;
    let mut upload_tested_safe_maximum = None;
    let mut link_kind = LinkKind::Unknown;
    let mut profile = AutotuneProfile::BestOverall;
    let mut access_medium = None;
    let mut access_source = AccessEvidenceSource::LegacyDefault;
    let mut access_confidence_percent = 0;
    let mut capacity_learning_policy = None;
    let mut download_service_cap_kbps = None;
    let mut upload_service_cap_kbps = None;
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
            "--dl-measurement-base-kbps" => {
                download_measurement_base =
                    Some(parse_cli_u64("download measurement base", &value)?)
            }
            "--ul-measurement-base-kbps" => {
                upload_measurement_base = Some(parse_cli_u64("upload measurement base", &value)?)
            }
            "--dl-runtime-min-kbps" => {
                download_runtime_minimum =
                    Some(parse_cli_u64("measured download runtime minimum", &value)?)
            }
            "--ul-runtime-min-kbps" => {
                upload_runtime_minimum =
                    Some(parse_cli_u64("measured upload runtime minimum", &value)?)
            }
            "--dl-tested-safe-max-kbps" => {
                download_tested_safe_maximum =
                    Some(parse_cli_u64("tested-safe download maximum", &value)?)
            }
            "--ul-tested-safe-max-kbps" => {
                upload_tested_safe_maximum =
                    Some(parse_cli_u64("tested-safe upload maximum", &value)?)
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
            "--access-medium" => {
                access_medium = Some(
                    AccessMedium::parse(&value)
                        .ok_or_else(|| format!("unsupported access medium: {value}"))?,
                )
            }
            "--access-source" => {
                access_source = AccessEvidenceSource::parse(&value)
                    .ok_or_else(|| format!("unsupported access-medium source: {value}"))?
            }
            "--access-confidence-percent" => {
                access_confidence_percent = value
                    .parse::<u64>()
                    .map_err(|_| format!("invalid access-medium confidence: {value}"))?;
                if access_confidence_percent > 100 {
                    return Err("access-medium confidence must be between 0 and 100".to_string());
                }
            }
            "--capacity-learning-policy" => {
                capacity_learning_policy = Some(
                    CapacityLearningPolicy::parse(&value)
                        .ok_or_else(|| format!("unsupported capacity-learning policy: {value}"))?,
                )
            }
            "--dl-service-cap-kbps" => {
                download_service_cap_kbps = Some(parse_cli_u64("download service cap", &value)?)
            }
            "--ul-service-cap-kbps" => {
                upload_service_cap_kbps = Some(parse_cli_u64("upload service cap", &value)?)
            }
            _ => return Err(format!("unsupported autotune option: {arg}")),
        }
    }

    let download = download.ok_or_else(|| "--dl-samples is required".to_string())?;
    let upload = upload.ok_or_else(|| "--ul-samples is required".to_string())?;
    let conservative =
        conservative_background_dl_kbps.is_some() || conservative_background_ul_kbps.is_some();
    let download = validated_conservative_samples(&download, conservative_background_dl_kbps)?;
    let upload = validated_conservative_samples(&upload, conservative_background_ul_kbps)?;

    let mut proposal = build_proposal_for_profile_with_context(
        &download,
        &upload,
        LatencyBaseline {
            median_ms: idle_median_ms.ok_or_else(|| "--idle-median-ms is required".to_string())?,
            p95_ms: idle_p95_ms.ok_or_else(|| "--idle-p95-ms is required".to_string())?,
            samples: idle_samples.ok_or_else(|| "--idle-samples is required".to_string())?,
        },
        link_kind,
        profile,
        ProposalContext {
            access_medium,
            access_source,
            access_confidence_percent,
            capacity_learning_policy,
            download_service_cap_kbps,
            upload_service_cap_kbps,
        },
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
    match (download_measurement_base, upload_measurement_base) {
        (Some(download), Some(upload)) => proposal.set_measurement_base_rates(download, upload)?,
        (None, None) => {}
        _ => {
            return Err(
                "download and upload measurement bases must be supplied together".to_string(),
            )
        }
    }
    match (download_runtime_minimum, upload_runtime_minimum) {
        (Some(download), Some(upload)) => {
            proposal.set_measured_runtime_minimums(download, upload)?
        }
        (Some(download), None) => proposal.set_measured_download_runtime_minimum(download)?,
        (None, Some(upload)) => proposal.set_measured_upload_runtime_minimum(upload)?,
        (None, None) => {}
    }
    proposal.set_tested_safe_maximums(download_tested_safe_maximum, upload_tested_safe_maximum)?;
    println!("{}", proposal.to_json());
    Ok(())
}

#[cfg(feature = "calibration")]
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
            realized_kbps: required_rate(dl_achieved, "dl-achieved-kbps")?,
            achieved_kbps: required_rate(dl_achieved, "dl-achieved-kbps")?,
            minimum_kbps: required_rate(dl_minimum, "dl-min-kbps")?,
            maximum_kbps: required_rate(dl_maximum, "dl-max-kbps")?,
        },
        upload: DirectionValidationInput {
            observed_low_kbps: required_rate(ul_observed_low, "ul-observed-low-kbps")?,
            candidate_kbps: required_rate(ul_candidate, "ul-candidate-kbps")?,
            realized_kbps: required_rate(ul_achieved, "ul-achieved-kbps")?,
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

#[cfg(feature = "calibration")]
fn parse_search_observations(value: &str) -> Result<Vec<autotune::SearchObservation>, String> {
    if value.is_empty() {
        return Err("search observation list must not be empty".to_string());
    }
    let mut observations = Vec::new();
    for (index, record) in value.split(';').enumerate() {
        if index >= autotune::MAX_PROFILE_SEARCH_OBSERVATIONS {
            return Err(format!(
                "search observation count must not exceed {}",
                autotune::MAX_PROFILE_SEARCH_OBSERVATIONS
            ));
        }
        let fields = record.split(',').collect::<Vec<_>>();
        if fields.len() != 6 || fields.iter().any(|field| field.is_empty()) {
            return Err(format!(
                "search observation {} must contain candidate,achieved,icmp,transport,loss,cpu",
                index + 1
            ));
        }
        let candidate_kbps = parse_cli_u64("search candidate rate", fields[0])?;
        let achieved_kbps = parse_cli_u64("search achieved rate", fields[1])?;
        observations.push(autotune::SearchObservation {
            candidate_kbps,
            realized_kbps: achieved_kbps,
            achieved_kbps,
            icmp_delta_ms: parse_cli_f64("search ICMP delta", fields[2])?,
            transport_delta_ms: parse_cli_f64("search transport delta", fields[3])?,
            transport_censored: false,
            loss_percent: parse_cli_f64("search loss percent", fields[4])?,
            cpu_percent: parse_cli_f64("search CPU percent", fields[5])?,
        });
    }
    Ok(observations)
}

#[cfg(feature = "calibration")]
fn run_autotune_optimize_direction_cli<I>(args: I) -> Result<(), String>
where
    I: Iterator<Item = String>,
{
    use autotune::{
        optimize_profile_direction, terminate_profile_direction_at_measured_boundary,
        AutotuneProfile, ProfileSearchInput, SearchDirection,
    };

    let mut profile = AutotuneProfile::BestOverall;
    let mut direction = None;
    let mut observed_low_kbps = None;
    let mut minimum_kbps = None;
    let mut upper_kbps = None;
    let mut observations = None;
    let mut uncertainty_percent = 1.5;
    let mut max_attempts = 6usize;
    let mut realization_min = None;
    let mut realization_max = None;
    let mut retention_min = None;
    let mut loss_max = None;
    let mut cpu_max = None;
    let mut terminal_reason = None;
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
            "--direction" => {
                direction = Some(
                    SearchDirection::parse(&value)
                        .ok_or_else(|| format!("unsupported search direction: {value}"))?,
                )
            }
            "--observed-low-kbps" => {
                observed_low_kbps = Some(parse_cli_u64("search observed-low rate", &value)?)
            }
            "--minimum-kbps" => minimum_kbps = Some(parse_cli_u64("search minimum rate", &value)?),
            "--upper-kbps" => upper_kbps = Some(parse_cli_u64("search upper rate", &value)?),
            "--observations" => observations = Some(parse_search_observations(&value)?),
            "--uncertainty-percent" => {
                uncertainty_percent = parse_cli_f64("search uncertainty", &value)?
            }
            "--max-attempts" => {
                max_attempts = value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid search max attempts: {value}"))?
            }
            "--candidate-realization-min-percent" => {
                realization_min = Some(parse_cli_f64("candidate realization minimum", &value)?)
            }
            "--candidate-realization-max-percent" => {
                realization_max = Some(parse_cli_f64("candidate realization maximum", &value)?)
            }
            "--capacity-retention-min-percent" => {
                retention_min = Some(parse_cli_f64("capacity retention minimum", &value)?)
            }
            "--loss-max-percent" => loss_max = Some(parse_cli_f64("packet loss maximum", &value)?),
            "--cpu-max-percent" => cpu_max = Some(parse_cli_f64("CPU maximum", &value)?),
            "--terminate-at-measured-boundary" => {
                if value != "candidate-transfer-unmeasurable" {
                    return Err(format!("unsupported measured-boundary reason: {value}"));
                }
                terminal_reason = Some("candidate-transfer-unmeasurable");
            }
            _ => return Err(format!("unsupported Auto-Tune search option: {arg}")),
        }
    }

    let mut thresholds = profile.validation_thresholds();
    if let Some(value) = realization_min {
        thresholds.candidate_realization_min_percent = value;
    }
    if let Some(value) = realization_max {
        thresholds.candidate_realization_max_percent = value;
    }
    if let Some(value) = retention_min {
        thresholds.capacity_retention_min_percent = value;
    }
    if let Some(value) = loss_max {
        thresholds.loss_max_percent = value;
    }
    if let Some(value) = cpu_max {
        thresholds.cpu_max_percent = value;
    }
    let input = ProfileSearchInput {
        profile,
        direction: direction.ok_or_else(|| "--direction is required".to_string())?,
        observed_low_kbps: observed_low_kbps
            .ok_or_else(|| "--observed-low-kbps is required".to_string())?,
        minimum_kbps: minimum_kbps.ok_or_else(|| "--minimum-kbps is required".to_string())?,
        upper_kbps: upper_kbps.ok_or_else(|| "--upper-kbps is required".to_string())?,
        thresholds,
        uncertainty_percent,
        max_attempts,
        observations: observations.ok_or_else(|| "--observations is required".to_string())?,
    };
    let result = match terminal_reason {
        Some(reason) => terminate_profile_direction_at_measured_boundary(input, reason)?,
        None => optimize_profile_direction(input)?,
    };
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
        #[cfg(feature = "calibration")]
        Some("--autotune-proposal") => {
            if let Err(error) = run_autotune_proposal_cli(initial_args) {
                eprintln!("ERROR: {error}");
                std::process::exit(2);
            }
            return;
        }
        #[cfg(feature = "calibration")]
        Some("--autotune-validate") => {
            if let Err(error) = run_autotune_validation_cli(initial_args) {
                eprintln!("ERROR: {error}");
                std::process::exit(2);
            }
            return;
        }
        #[cfg(feature = "calibration")]
        Some("--autotune-optimize-direction") => {
            if let Err(error) = run_autotune_optimize_direction_cli(initial_args) {
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
        #[cfg(feature = "calibration")]
        Some("--calibrationd") => {
            install_signal_handlers();
            if let Err(error) = operations::coordinator::run_calibrationd(initial_args, &TERMINATE)
            {
                eprintln!("ERROR: {error}");
                std::process::exit(1);
            }
            return;
        }
        #[cfg(feature = "calibration")]
        Some("--calibrationctl") => {
            if let Err(error) = operations::coordinator::run_calibrationctl(initial_args) {
                eprintln!("ERROR: {error}");
                std::process::exit(2);
            }
            return;
        }
        #[cfg(feature = "calibration")]
        Some("--native-apply-recover") => {
            if let Err(error) = operations::coordinator::run_native_apply_recovery(initial_args) {
                eprintln!("ERROR: {error}");
                std::process::exit(1);
            }
            return;
        }
        #[cfg(feature = "calibration")]
        Some("--calibration-capabilities") => {
            match calibration_capabilities(initial_args) {
                Ok(capabilities) => println!("{capabilities}"),
                Err(error) => {
                    eprintln!("ERROR: {error}");
                    std::process::exit(2);
                }
            }
            return;
        }
        #[cfg(feature = "calibration")]
        Some("--rating-worker") => {
            install_signal_handlers();
            if let Err(error) = operations::rating::run_rating_worker(initial_args, &TERMINATE) {
                eprintln!("ERROR: {error}");
                std::process::exit(1);
            }
            return;
        }
        #[cfg(feature = "calibration")]
        Some("--speedtest-worker") => {
            install_signal_handlers();
            if let Err(error) =
                operations::speedtest::run_speedtest_worker(initial_args, &TERMINATE)
            {
                eprintln!("ERROR: {error}");
                std::process::exit(1);
            }
            return;
        }
        #[cfg(feature = "calibration")]
        Some("--autotune-worker") => {
            install_signal_handlers();
            if let Err(error) =
                operations::full_autotune::run_autotune_worker(initial_args, &TERMINATE)
            {
                eprintln!("ERROR: {error}");
                std::process::exit(1);
            }
            return;
        }
        #[cfg(feature = "calibration")]
        Some("--bootstrap-runtime-owner") => {
            install_signal_handlers();
            if let Err(error) = operations::bootstrap_runtime_owner::run_bootstrap_runtime_owner(
                initial_args,
                &TERMINATE,
            ) {
                eprintln!("ERROR: {error}");
                std::process::exit(1);
            }
            return;
        }
        _ => {}
    }

    install_signal_handlers();

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

#[cfg(all(test, feature = "calibration"))]
mod tests {
    use super::routing::RouteIdentity;
    use super::{
        adaptive_capacity_status_json, attest_cake_direction, attest_download_redirect,
        attest_exclusive_sqm_ingress, autotune, autotune_capture_attestation_lease_valid,
        autotune_capture_uses_identity_bound_speedtest_counters,
        autotune_counter_completion_is_timely, autotune_counter_completion_matches,
        autotune_transport_control_phase, autotune_transport_delta_phase,
        bounded_operation_capture_active, calibration_capabilities,
        censored_autotune_transport_observation, compact_graph_history_data,
        compact_graph_history_file, compute_history_budget, default_reflectors, graph_history_line,
        history_safe_max_kib, idle_sleep_due, idle_wake_due, ingress_output_targets_ifb,
        irtt_target_arg, loaded_autotune_traffic_observation_after_read,
        max_wire_packet_size_bits_from_mtu, monitor_tick_timeout, next_spare_reflector,
        packet_compensation_us, parse_cli_f64, parse_fping_line, parse_fping_ts_line,
        parse_irtt_duration_us, parse_irtt_line, parse_private_ingress_output,
        parse_private_root_output, parse_rate_samples, parse_reflector_candidates,
        parse_strict_bool, parse_tc_bandwidth_kbps, parse_tc_linklayer_overhead, parse_tsping_line,
        parse_uci_values, pinger_command, pinger_line_is_timeout, pinger_response_interval_s,
        private_cake_args, probe_loop_required, published_applied_cake_rate_kbps,
        published_runtime_qdisc_kind, qdisc_output_has_cake, quality_grade_result_json,
        rate_sample_is_recent, rating_capture_request_is_admissible, reflector_bad_reflectors,
        reflector_health_json, reflector_spare_reflectors,
        reject_autotune_capture_after_io_with_clock, root_cake_bandwidth_kbps, root_cake_qdisc,
        run, run_autotune_proposal_cli, run_sqm_helper, runtime_driver_holds_controller,
        sample_is_stale, select_autotune_capture_rates, shaper_update_due, stall_detection_timeout,
        status_publish_due, stop_managed_sqm_with, throughput_floor, transport_error_code,
        transport_probe_control_allows_start, transport_probe_interval_s,
        transport_probe_route_identities, transport_probe_runtime_required,
        transport_result_matches_route, uplink_error_code, validated_conservative_samples,
        wait_for_runtime_topology, AdaptiveCapacityStatusContext, AdaptiveCeilingDirection,
        AutotuneSpeedtestRateMonitor, AutotuneTransportCaptureKey, AutotuneTransportControl,
        AutotuneTransportFlight, AutotuneTransportReadiness, AutotuneTransportSettlement,
        CakeQdiscKind, Config, Controller, MemoryInfo, PrivateIngressState, PrivateRootState,
        RateMonitor, RateSample, ReflectorHealth, ReflectorState, RouteSnapshot, Sample,
        SpeedtestCounterRateMonitor, SqmRecoveryError, SqmTopologyErrorKind, ThroughputGuardInput,
        TransportLatencyTracker, TransportProbeResult, UplinkState,
        AUTOTUNE_CAPTURE_ATTESTATION_MAX_AGE, CAKE_GROWTH_UPDATE_MIN_INTERVAL,
        CALIBRATIONCTL_SCHEDULER_STATUS_USAGE, CALIBRATION_CAPABILITIES_V3,
        STATUS_PUBLISH_INTERVAL, TERMINATE, TRANSPORT_BASELINE_LEARNING_INTERVAL_S,
    };
    use std::env;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;
    use std::sync::Mutex;
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    static HELPER_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn scheduler_status_usage_is_an_exact_zero_argument_batch_contract() {
        assert!(CALIBRATIONCTL_SCHEDULER_STATUS_USAGE.ends_with(" scheduler-status"));
        assert!(!CALIBRATIONCTL_SCHEDULER_STATUS_USAGE.contains("INSTANCE"));
        assert!(!CALIBRATIONCTL_SCHEDULER_STATUS_USAGE.ends_with(" *"));
    }

    #[test]
    fn partial_directional_rating_json_masks_the_overall_grade() {
        let upload = super::quality_grade::QualityGradeMetric {
            class: super::transport_quality::QualityClass::A,
            increase_ms: 14.308,
            loaded_p90_ms: 26.047,
            samples: 28,
            evidence_source: super::quality_grade::QUALITY_GRADE_EVIDENCE_SOURCE,
            icmp_increase_ms: 12.0,
            transport_increase_ms: 14.308,
            icmp_samples: 28,
            transport_samples: 28,
        };
        let partial = super::quality_grade::QualityGradeResult {
            class: super::transport_quality::QualityClass::A,
            increase_ms: 14.308,
            baseline_p5_ms: 11.739,
            endpoint: "test-endpoint".to_string(),
            started_at: 1.0,
            completed_at: Some(2.0),
            route_identity: "route-a".to_string(),
            partial: true,
            incomplete: false,
            dl_samples: 0,
            ul_samples: 28,
            bidirectional_samples: 0,
            completion_reason: "download_incomplete".to_string(),
            capture_job_id: String::new(),
            capture_generation: 0,
            dl: None,
            ul: Some(upload),
            bidirectional: None,
        };
        let json = quality_grade_result_json(Some(&partial), false);
        assert!(json.contains("\"grade\":\"LEARNING\""));
        assert!(json.contains("\"partial\":true"));
        assert!(json.contains("\"completion_reason\":\"download_incomplete\""));
        assert!(json.contains("\"ul\":{\"grade\":\"A\""));
    }

    #[test]
    fn rating_capture_admission_requires_exact_identity_mode_and_live_deadline() {
        let token = "0123456789abcdef0123456789abcdef";
        assert!(rating_capture_request_is_admissible(
            token,
            "client",
            Some(101.0),
            100.0
        ));
        assert!(rating_capture_request_is_admissible(
            token,
            "automatic",
            Some(101.0),
            100.0
        ));
        assert!(!rating_capture_request_is_admissible(
            "100-42",
            "client",
            Some(101.0),
            100.0
        ));
        assert!(!rating_capture_request_is_admissible(
            token,
            "manual",
            Some(101.0),
            100.0
        ));
        assert!(!rating_capture_request_is_admissible(
            token,
            "client",
            Some(100.0),
            100.0
        ));
        assert!(!rating_capture_request_is_admissible(
            token,
            "client",
            Some(f64::NAN),
            100.0
        ));
    }

    #[test]
    fn calibration_capability_protocol_is_exact_and_rejects_options() {
        assert_eq!(
            calibration_capabilities(std::iter::empty()).unwrap(),
            CALIBRATION_CAPABILITIES_V3
        );
        assert!(calibration_capabilities(vec!["unexpected".to_string()].into_iter()).is_err());
    }

    #[test]
    fn adaptive_capacity_status_exposes_passive_transport_and_causal_state() {
        let download = AdaptiveCeilingDirection::new(175_000.0, 350_000.0);
        let upload = AdaptiveCeilingDirection::new(15_773.0, 80_000.0);
        let json = adaptive_capacity_status_json(AdaptiveCapacityStatusContext {
            enabled: true,
            route_epoch: Some("main|wwan0|10.0.0.2|\"epoch\""),
            download: &download,
            upload: &upload,
            current_download_kbps: 154_092.0,
            current_upload_kbps: 11_175.0,
            runtime_minimum_download_kbps: 57_258.0,
            runtime_minimum_upload_kbps: 7_887.0,
            transport_confidence_download: 100,
            transport_confidence_upload: 50,
            no_cake_effect_download: Some(false),
            no_cake_effect_upload: Some(true),
            causal_state_download: "controlled",
            causal_state_upload: "no_cake_effect",
        });

        assert!(json.contains("\"schema_version\":1"));
        assert!(json.contains("\"behavior_version\":\"passive_transport_v1\""));
        assert!(json.contains("\"route_epoch\":\"main|wwan0|10.0.0.2|\\\"epoch\\\"\""));
        assert!(json.contains("\"current_rate_kbps\":154092"));
        assert!(json.contains("\"safe_ceiling_kbps\":175000"));
        assert!(json.contains("\"runtime_minimum_kbps\":57258"));
        assert!(json.contains("\"raw_capacity_kbps\":null"));
        assert!(json.contains("\"exploration_minimum_kbps\":null"));
        assert!(json.contains("\"no_cake_effect\":false"));
        assert!(json.contains("\"no_cake_effect\":true"));
        assert!(json.contains("\"causal_state\":\"controlled\""));
        assert!(json.contains("\"causal_state\":\"no_cake_effect\""));
        assert!(json.contains("\"state\":\"passive_transport\""));
        assert!(json.contains("\"percent\":100"));
        assert!(json.contains("\"transport_percent\":100"));
        assert!(json.contains("\"transport_percent\":50"));
    }

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
        assert!(pinger_line_is_timeout("fping", line));
        assert!(pinger_line_is_timeout("fping-ts", line));
        assert!(!pinger_line_is_timeout("tsping", line));
    }

    #[test]
    fn recognizes_only_explicit_ping_timeout_output() {
        assert!(pinger_line_is_timeout(
            "ping",
            "no answer yet for icmp_seq=18"
        ));
        assert!(!pinger_line_is_timeout(
            "ping",
            "64 bytes from 1.1.1.1: icmp_seq=18 ttl=57 time=2.1 ms"
        ));
        assert!(!pinger_line_is_timeout(
            "fping",
            "warning: timed out while parsing unrelated output"
        ));
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
    fn bounded_operation_capture_keeps_idle_probe_loop_awake() {
        let timeout = Duration::from_secs(60);

        assert!(!bounded_operation_capture_active(false, false));
        assert!(bounded_operation_capture_active(true, false));
        assert!(bounded_operation_capture_active(false, true));
        assert!(bounded_operation_capture_active(true, true));
        assert!(!probe_loop_required(false, false));
        assert!(probe_loop_required(true, false));
        assert!(probe_loop_required(false, true));
        assert!(probe_loop_required(true, true));
        assert!(idle_sleep_due(true, false, false, false, timeout, timeout));
        assert!(!idle_sleep_due(true, false, false, true, timeout, timeout));
        assert!(!idle_sleep_due(
            true,
            false,
            false,
            false,
            timeout - Duration::from_millis(1),
            timeout,
        ));
        assert!(!idle_sleep_due(true, true, false, false, timeout, timeout));
        assert!(!idle_sleep_due(
            false, false, false, false, timeout, timeout
        ));
        assert!(idle_wake_due(false, true, true));
        assert!(!transport_probe_runtime_required(false, false, false));
        assert!(transport_probe_runtime_required(true, false, false));
        assert!(transport_probe_runtime_required(false, true, false));
        assert!(transport_probe_runtime_required(false, false, true));
        assert!(transport_probe_runtime_required(false, true, true));
        use crate::operations::autotune_runtime_driver::RuntimeDriverOutcome;
        assert!(!runtime_driver_holds_controller(&Ok(
            RuntimeDriverOutcome::Idle
        )));
        for outcome in [
            RuntimeDriverOutcome::PermitAwaitingControl,
            RuntimeDriverOutcome::Applying,
            RuntimeDriverOutcome::Applied,
            RuntimeDriverOutcome::Restoring,
            RuntimeDriverOutcome::Restored,
            RuntimeDriverOutcome::Rejected,
            RuntimeDriverOutcome::UnsafeRecoveryRequired,
        ] {
            assert!(runtime_driver_holds_controller(&Ok(outcome)));
        }
        assert!(runtime_driver_holds_controller(&Err(
            "unreadable runtime owner".to_string()
        )));
        assert!(idle_wake_due(true, false, true));
        assert!(!idle_wake_due(false, true, false));
        assert!(!idle_wake_due(false, false, true));
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
                "mwan3|wan|pppoe-wan|198.51.100.1|0x100|1",
                Some("A+"),
                "final",
                Some(1.25),
                "DL",
                20,
                7,
                "probe_observe",
                "cruise",
                "probe target reached",
                "initialized",
                "monitoring",
                "no_cake_effect",
                "HEALTHY",
            ),
            "123,1.235,2.3,1000.0,50.5,10.123,11.988,600.0,30.0,ACTIVE,mwan3|wan|pppoe-wan|198.51.100.1|0x100|1,A+,final,1.250,DL,20,7,probe_observe,cruise,probe target reached,initialized,monitoring,no_cake_effect,HEALTHY\n"
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
    fn rejects_active_threshold_above_active_download_minimum() {
        let mut cfg = Config::defaults("test".to_string());
        cfg.connection_active_thr_kbps = 6000.0;
        cfg.min_dl_shaper_rate_kbps = 5000.0;
        cfg.min_ul_shaper_rate_kbps = 7000.0;

        let err = cfg.validate().expect_err("expected active threshold guard");
        assert!(err.contains("min_dl_shaper_rate_kbps"));
    }

    #[test]
    fn rejects_active_threshold_above_active_upload_minimum() {
        let mut cfg = Config::defaults("test".to_string());
        cfg.connection_active_thr_kbps = 6000.0;
        cfg.min_dl_shaper_rate_kbps = 7000.0;
        cfg.min_ul_shaper_rate_kbps = 5000.0;

        let err = cfg.validate().expect_err("expected active threshold guard");
        assert!(err.contains("min_ul_shaper_rate_kbps"));
    }

    #[test]
    fn upload_only_ignores_dormant_download_minimum_but_keeps_upload_guard() {
        let mut cfg = Config::defaults("upload_only".to_string());
        cfg.sqm_enabled = true;
        cfg.sqm_direction_mode = "upload_only".to_string();
        cfg.adjust_dl_shaper_rate = false;
        cfg.adjust_ul_shaper_rate = true;
        cfg.connection_active_thr_kbps = 7200.0;
        cfg.min_dl_shaper_rate_kbps = 5000.0;
        cfg.min_ul_shaper_rate_kbps = 25100.0;
        assert!(cfg.validate().is_ok());

        cfg.min_ul_shaper_rate_kbps = 7000.0;
        let err = cfg
            .validate()
            .expect_err("active upload minimum must remain guarded");
        assert!(err.contains("min_ul_shaper_rate_kbps"));
    }

    #[test]
    fn download_only_ignores_dormant_upload_minimum_but_keeps_download_guard() {
        let mut cfg = Config::defaults("download_only".to_string());
        cfg.sqm_enabled = true;
        cfg.sqm_direction_mode = "download_only".to_string();
        cfg.adjust_dl_shaper_rate = true;
        cfg.adjust_ul_shaper_rate = false;
        cfg.connection_active_thr_kbps = 7200.0;
        cfg.min_dl_shaper_rate_kbps = 25100.0;
        cfg.min_ul_shaper_rate_kbps = 5000.0;
        assert!(cfg.validate().is_ok());

        cfg.min_dl_shaper_rate_kbps = 7000.0;
        let err = cfg
            .validate()
            .expect_err("active download minimum must remain guarded");
        assert!(err.contains("min_dl_shaper_rate_kbps"));
    }

    #[test]
    fn inactive_rate_controllers_do_not_constrain_connection_threshold() {
        let mut cfg = Config::defaults("manual".to_string());
        cfg.adjust_dl_shaper_rate = false;
        cfg.adjust_ul_shaper_rate = false;
        cfg.connection_active_thr_kbps = 7200.0;
        cfg.min_dl_shaper_rate_kbps = 5000.0;
        cfg.min_ul_shaper_rate_kbps = 5000.0;
        assert!(cfg.validate().is_ok());
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
            Some("mwan3|wan|pppoe-wan|198.51.100.1|0x100|1"),
            Some("mwan3|wan|pppoe-wan|198.51.100.1|0x100|1")
        ));
        assert!(!transport_result_matches_route(
            Some("mwan3|wan|pppoe-wan|198.51.100.1|0x100|1"),
            Some("mwan3|wanb|eth0|192.0.2.101|0x200|2")
        ));
        assert!(!transport_result_matches_route(None, Some("main|||")));
    }

    #[test]
    fn route_loss_preserves_only_negative_transport_identity() {
        let snapshot = |online: bool, source_ip: &str| RouteSnapshot {
            identity: RouteIdentity {
                mode: "mwan3".to_string(),
                member: "wan".to_string(),
                device: "eth1".to_string(),
                source_ip: source_ip.to_string(),
                fwmark: "0x100".to_string(),
                table: "1".to_string(),
            },
            online,
            active: online,
            member_status: if online { "online" } else { "offline" }.to_string(),
            reason: String::new(),
        };
        let before = snapshot(true, "192.0.2.2");
        let online = snapshot(true, "192.0.2.2");
        let offline = snapshot(false, "192.0.2.2");
        let changed = snapshot(false, "192.0.2.3");
        let initially_offline = snapshot(false, "192.0.2.2");
        let expected = Some(before.stable_key());

        assert_eq!(
            transport_probe_route_identities(Some(&before), Some(&online)),
            (expected.clone(), expected.clone())
        );
        assert_eq!(
            transport_probe_route_identities(Some(&before), Some(&offline)),
            (None, expected)
        );
        assert_eq!(
            transport_probe_route_identities(Some(&before), Some(&changed)),
            (None, None)
        );
        assert_eq!(
            transport_probe_route_identities(Some(&initially_offline), Some(&online)),
            (None, None)
        );
        assert_eq!(
            transport_probe_route_identities(Some(&before), None),
            (None, None)
        );
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
        assert!(!qdisc_output_has_cake(
            "qdisc mq 0: root\nqdisc cake 8001: parent :1 bandwidth 100Mbit"
        ));
        assert!(!qdisc_output_has_cake(
            "qdisc cake 8001: root bandwidth 100Mbit\nqdisc cake 8002: root bandwidth 90Mbit"
        ));

        let redirect = "action order 1: mirred (Egress Redirect to device ifb4eth0)";
        assert!(ingress_output_targets_ifb(redirect, "ifb4eth0"));
        assert!(!ingress_output_targets_ifb(redirect, "ifb4eth1"));
        assert!(!ingress_output_targets_ifb(redirect, "ifb4eth"));
        assert!(!ingress_output_targets_ifb(redirect, "ifb4eth00"));
    }

    #[test]
    fn cake_bandwidth_parser_attests_one_exact_root_rate() {
        assert_eq!(parse_tc_bandwidth_kbps("100000Kbit").unwrap(), 100_000);
        assert_eq!(parse_tc_bandwidth_kbps("723.4Mbit").unwrap(), 723_400);
        assert_eq!(parse_tc_bandwidth_kbps("1Gbit").unwrap(), 1_000_000);
        assert_eq!(
            root_cake_bandwidth_kbps(
                "qdisc cake 8001: root refcnt 2 bandwidth 723400Kbit besteffort"
            )
            .unwrap(),
            723_400
        );
        assert_eq!(
            root_cake_bandwidth_kbps(
                "qdisc cake_mq 8002: root refcnt 2 bandwidth 1Gbit besteffort"
            )
            .unwrap(),
            1_000_000
        );
        assert_eq!(
            root_cake_qdisc("qdisc cake_mq 8002: root refcnt 2 bandwidth 1Gbit besteffort")
                .unwrap(),
            (CakeQdiscKind::CakeMq, 1_000_000)
        );
        assert_eq!(
            published_runtime_qdisc_kind(true, Some(CakeQdiscKind::CakeMq)),
            Some(crate::operations::autotune_runtime::RuntimeQdiscKind::CakeMq)
        );
        assert_eq!(
            published_runtime_qdisc_kind(false, Some(CakeQdiscKind::CakeMq)),
            None
        );
        assert_eq!(published_applied_cake_rate_kbps(true, 19_999), 19_999.0);
        assert_eq!(published_applied_cake_rate_kbps(false, 19_999), 0.0);
        assert!(root_cake_bandwidth_kbps("qdisc noqueue 0: root refcnt 2").is_err());
        assert!(root_cake_bandwidth_kbps(
            "qdisc cake 1: root bandwidth 10Mbit\nqdisc cake 2: root bandwidth 20Mbit"
        )
        .is_err());
        assert!(parse_tc_bandwidth_kbps("NaNMbit").is_err());
    }

    #[test]
    fn published_cake_rate_uses_applied_integer_not_fractional_controller_intent() {
        let desired_controller_rate = 19_999.375;
        let last_successfully_applied_rate = 20_000;

        let published = published_applied_cake_rate_kbps(true, last_successfully_applied_rate);
        assert_eq!(published, 20_000.0);
        assert_ne!(published, desired_controller_rate);
        assert_eq!(
            published_applied_cake_rate_kbps(false, last_successfully_applied_rate),
            0.0
        );
    }

    #[test]
    fn attested_rate_refresh_preserves_intent_and_publishes_live_integer() {
        use crate::operations::autotune_runtime::{RuntimeQdiscKind, RuntimeSnapshot};
        use crate::operations::full_autotune::MeasurementTopology;
        use crate::operations::rating::RatingRuntimeSnapshot;

        let _guard = HELPER_TEST_LOCK.lock().unwrap();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "cake-attested-rate-publication-{}-{unique}",
            std::process::id()
        ));
        let counters = root.join("counters");
        fs::create_dir_all(&counters).unwrap();
        let rx = counters.join("rx_bytes");
        let tx = counters.join("tx_bytes");
        fs::write(&rx, "0\n").unwrap();
        fs::write(&tx, "0\n").unwrap();

        let previous_run_root = env::var_os("CAKE_AUTORATE_RUN_ROOT");
        env::set_var("CAKE_AUTORATE_RUN_ROOT", root.join("run"));
        let mut cfg = Config::defaults("attested_rate".to_string());
        cfg.rx_bytes_path = rx.to_string_lossy().into_owned();
        cfg.tx_bytes_path = tx.to_string_lossy().into_owned();
        cfg.manage_sqm = true;
        cfg.sqm_enabled = true;
        cfg.log_to_file = false;
        let mut controller = Controller::new(cfg).unwrap();
        controller.write_initial_status(&[], None).unwrap();

        controller.shaper_dl = 20_000.625;
        controller.shaper_ul = 19_999.375;
        controller.last_set_dl = 20_000;
        controller.last_set_ul = 20_000;
        controller.dl_qdisc_kind = Some(CakeQdiscKind::Cake);
        controller.ul_qdisc_kind = Some(CakeQdiscKind::Cake);
        let actual = RuntimeSnapshot {
            target_interface: controller.cfg.sqm_interface.clone(),
            route_fingerprint: "11".repeat(32),
            sqm_fingerprint: "22".repeat(32),
            topology: MeasurementTopology::ShapedBoth,
            download_kbps: Some(19_750),
            upload_kbps: Some(19_500),
            download_qdisc_kind: Some(RuntimeQdiscKind::Cake),
            upload_qdisc_kind: Some(RuntimeQdiscKind::Cake),
        };
        controller.remember_attested_cake_rates(&actual);
        controller.refresh_status_from_last_sample().unwrap();

        assert_eq!(controller.shaper_dl, 20_000.625);
        assert_eq!(controller.shaper_ul, 19_999.375);
        assert_eq!(controller.last_set_dl, 19_750);
        assert_eq!(controller.last_set_ul, 19_500);
        let published = RatingRuntimeSnapshot::decode(
            &fs::read_to_string(controller.cfg.run_dir().join("rating-runtime")).unwrap(),
        )
        .unwrap();
        assert_eq!(published.cake_dl_kbps, 19_750.0);
        assert_eq!(published.cake_ul_kbps, 19_500.0);

        if let Some(value) = previous_run_root {
            env::set_var("CAKE_AUTORATE_RUN_ROOT", value);
        } else {
            env::remove_var("CAKE_AUTORATE_RUN_ROOT");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sqm_topology_classifies_off_and_one_sided_directions_without_text_matching() {
        let cake = "qdisc cake 8010: root refcnt 2 bandwidth 100Mbit";
        let none = "qdisc noqueue 0: root refcnt 2";
        let redirect = "action order 1: mirred (Egress Redirect to device ifb4wan)";

        // Fully disabled is valid only when neither direction nor ingress
        // still contains a managed CAKE artifact.
        assert!(attest_cake_direction(none, false, "download", "ifb4wan").is_ok());
        assert!(attest_cake_direction(none, false, "upload", "wan").is_ok());
        assert!(attest_download_redirect("", false, "wan", "ifb4wan").is_ok());

        // Download-only and upload-only are independently valid topologies.
        assert!(attest_cake_direction(cake, true, "download", "ifb4wan").is_ok());
        assert!(attest_cake_direction(none, false, "upload", "wan").is_ok());
        assert!(attest_download_redirect(redirect, true, "wan", "ifb4wan").is_ok());
        assert!(attest_cake_direction(none, false, "download", "ifb4wan").is_ok());
        assert!(attest_cake_direction(cake, true, "upload", "wan").is_ok());
        assert!(attest_download_redirect("", false, "wan", "ifb4wan").is_ok());

        assert_eq!(
            attest_cake_direction(cake, false, "download", "ifb4wan")
                .unwrap_err()
                .kind,
            SqmTopologyErrorKind::Unsafe
        );
        assert_eq!(
            attest_download_redirect(redirect, false, "wan", "ifb4wan")
                .unwrap_err()
                .kind,
            SqmTopologyErrorKind::Unsafe
        );
        assert_eq!(
            attest_cake_direction(none, true, "upload", "wan")
                .unwrap_err()
                .kind,
            SqmTopologyErrorKind::Settling
        );
    }

    #[test]
    fn download_bypass_requires_exclusive_standard_sqm_ingress_ownership() {
        let standard = "filter parent ffff: protocol all pref 10 u32 chain 0\n\
filter parent ffff: protocol all pref 10 u32 chain 0 fh 800: ht divisor 1\n\
filter parent ffff: protocol all pref 10 u32 chain 0 fh 800::800 order 2048 key ht 800 bkt 0 flowid 1:1 not_in_hw\n\
  match 00000000/00000000 at 0\n\
 action order 1: mirred (Egress Redirect to device ifb4wan) stolen\n\
 index 2 ref 1 bind 1\n";
        assert!(attest_exclusive_sqm_ingress(standard, "wan", "ifb4wan").is_ok());

        let with_foreign_filter =
            format!("{standard}filter parent ffff: protocol ip pref 20 flower chain 0\n");
        let error =
            attest_exclusive_sqm_ingress(&with_foreign_filter, "wan", "ifb4wan").unwrap_err();
        assert_eq!(error.kind, SqmTopologyErrorKind::Unsafe);
        assert_eq!(error.code, "download-ingress-not-exclusive");

        let with_foreign_action = standard.replace(
            "index 2 ref 1 bind 1",
            "action order 2: police rate 1Gbit\nindex 2 ref 1 bind 1",
        );
        assert!(attest_exclusive_sqm_ingress(&with_foreign_action, "wan", "ifb4wan").is_err());
    }

    #[test]
    fn private_calibration_parsers_accept_only_exact_job_owned_state() {
        use crate::autotune::{AutotuneProfile, LinkKind};
        use crate::operations::autotune_runtime::{
            AutotuneRuntimePermit, RuntimeBaseline, RuntimeQdiscKind, RuntimeRateBounds,
            RuntimeSnapshot,
        };
        use crate::operations::autotune_runtime_store::RuntimeOverrideCheckpoint;
        use crate::operations::full_autotune::MeasurementTopology;
        use crate::operations::identity::ProcessIdentity;

        let permit = AutotuneRuntimePermit {
            kind: crate::operations::autotune_runtime::RuntimePermitKind::Autotune,
            permit_id: "77".repeat(16),
            job_id: "11".repeat(16),
            worker_run_id: "22".repeat(16),
            boot_id: "33".repeat(16),
            coordinator_generation: "44".repeat(16),
            worker: ProcessIdentity {
                pid: 100,
                process_group: 100,
                starttime_ticks: 500,
            },
            instance_name: "wan_sqm".to_string(),
            target_interface: "pppoe-wan".to_string(),
            route_identity: "main||pppoe-wan|192.0.2.1||254".to_string(),
            route_fingerprint: "55".repeat(32),
            sqm_fingerprint: "66".repeat(32),
            deadline_boot_ms: 60_000,
            maximum_sequence: 32,
            profile: AutotuneProfile::VariableLink,
            link_kind: LinkKind::Pppoe,
            baseline: RuntimeBaseline::Managed(MeasurementTopology::UploadOnlyShaped),
            initial_download_kbps: 100_000,
            initial_upload_kbps: 50_000,
            download_qdisc_kind: RuntimeQdiscKind::Cake,
            upload_qdisc_kind: RuntimeQdiscKind::Cake,
            allow_bypass_download: true,
            allow_bypass_upload: true,
            download_bounds: RuntimeRateBounds {
                minimum_kbps: 10_000,
                maximum_kbps: 1_000_000,
            },
            upload_bounds: RuntimeRateBounds {
                minimum_kbps: 5_000,
                maximum_kbps: 500_000,
            },
        };
        let baseline = RuntimeSnapshot {
            target_interface: permit.target_interface.clone(),
            route_fingerprint: permit.route_fingerprint.clone(),
            sqm_fingerprint: permit.sqm_fingerprint.clone(),
            topology: MeasurementTopology::UploadOnlyShaped,
            download_kbps: None,
            upload_kbps: Some(45_000),
            download_qdisc_kind: None,
            upload_qdisc_kind: Some(RuntimeQdiscKind::Cake),
        };
        let checkpoint =
            RuntimeOverrideCheckpoint::new(&permit, 1_000, RuntimeBaseline::Managed(baseline))
                .unwrap();
        let upload = "qdisc cake a777: root refcnt 2 bandwidth 50Mbit diffserv4 nat noatm overhead 44 mpu 84";
        assert_eq!(
            parse_private_root_output(
                upload,
                "pppoe-wan",
                &checkpoint.temporary.target_qdisc_handle,
                checkpoint.profile,
                checkpoint.link_kind,
                false,
            )
            .unwrap(),
            PrivateRootState::Exact(50_000)
        );
        let download = "qdisc cake b777: root refcnt 2 bandwidth 100Mbit besteffort nat wash noatm overhead 44 mpu 84";
        assert_eq!(
            parse_private_root_output(
                download,
                &checkpoint.temporary.ifb_name,
                &checkpoint.temporary.ifb_qdisc_handle,
                checkpoint.profile,
                checkpoint.link_kind,
                true,
            )
            .unwrap(),
            PrivateRootState::Exact(100_000)
        );
        assert_eq!(
            parse_private_root_output(
                "qdisc mq 0: root\nqdisc fq_codel 0: parent :1",
                "pppoe-wan",
                &checkpoint.temporary.target_qdisc_handle,
                checkpoint.profile,
                checkpoint.link_kind,
                false,
            )
            .unwrap(),
            PrivateRootState::AbsentOrKernelDefault
        );
        assert!(parse_private_root_output(
            "qdisc cake 8001: root bandwidth 50Mbit diffserv4 nat",
            "pppoe-wan",
            &checkpoint.temporary.target_qdisc_handle,
            checkpoint.profile,
            checkpoint.link_kind,
            false,
        )
        .is_err());
        assert!(parse_private_root_output(
            "qdisc cake a777: root bandwidth 50Mbit diffserv4 nat noatm overhead 18 mpu 64",
            "pppoe-wan",
            &checkpoint.temporary.target_qdisc_handle,
            checkpoint.profile,
            checkpoint.link_kind,
            false,
        )
        .is_err());

        let variable_download = private_cake_args(
            &checkpoint.temporary.ifb_name,
            &checkpoint.temporary.ifb_qdisc_handle,
            100_000,
            AutotuneProfile::VariableLink,
            LinkKind::Pppoe,
            true,
        )
        .unwrap();
        assert!(variable_download
            .windows(3)
            .any(|part| part == ["besteffort", "nat", "wash"]));
        assert!(variable_download
            .windows(5)
            .any(|part| part == ["ethernet", "overhead", "44", "mpu", "84"]));
        let gaming_upload = private_cake_args(
            "eth0",
            &checkpoint.temporary.target_qdisc_handle,
            50_000,
            AutotuneProfile::Gaming,
            LinkKind::Ethernet,
            false,
        )
        .unwrap();
        assert!(gaming_upload
            .windows(2)
            .any(|part| part == ["diffserv4", "nat"]));
        assert!(!gaming_upload.iter().any(|part| part == "wash"));
        assert!(gaming_upload
            .windows(5)
            .any(|part| part == ["ethernet", "overhead", "18", "mpu", "64"]));

        let qdisc = "qdisc ingress ffff: parent ffff:fff1";
        let filter = format!(
            "filter parent ffff: protocol all pref {} u32 chain 0\n\
filter parent ffff: protocol all pref {} u32 chain 0 fh 800: ht divisor 1\n\
filter parent ffff: protocol all pref {} u32 chain 0 fh {} order 1911 key ht 800 bkt 0 terminal flowid not_in_hw\n\
  match 00000000/00000000 at 0\n\
 action order 1: mirred (Egress Redirect to device {}) stolen\n\
 index {} ref 1 bind 1\n\
 cookie {}\n",
            checkpoint.temporary.redirect_preference,
            checkpoint.temporary.redirect_preference,
            checkpoint.temporary.redirect_preference,
            checkpoint.temporary.redirect_filter_tc_handle().unwrap(),
            checkpoint.temporary.ifb_name,
            checkpoint.temporary.redirect_action_index,
            checkpoint.temporary.redirect_action_cookie,
        );
        assert_eq!(
            parse_private_ingress_output(qdisc, &filter, "pppoe-wan", &checkpoint).unwrap(),
            PrivateIngressState::Exact
        );
        assert_eq!(
            parse_private_ingress_output("", "", "pppoe-wan", &checkpoint).unwrap(),
            PrivateIngressState::Absent
        );
        let foreign = filter.replace(
            &checkpoint.temporary.redirect_preference.to_string(),
            "1234",
        );
        assert!(parse_private_ingress_output(qdisc, &foreign, "pppoe-wan", &checkpoint).is_err());
        let foreign_handle = filter.replace(
            &checkpoint.temporary.redirect_filter_tc_handle().unwrap(),
            "800::123",
        );
        assert!(
            parse_private_ingress_output(qdisc, &foreign_handle, "pppoe-wan", &checkpoint).is_err()
        );
        let foreign_index = filter.replace(
            &checkpoint.temporary.redirect_action_index.to_string(),
            "123",
        );
        assert!(
            parse_private_ingress_output(qdisc, &foreign_index, "pppoe-wan", &checkpoint).is_err()
        );
        let foreign_cookie = filter.replace(
            &checkpoint.temporary.redirect_action_cookie,
            "00112233445566778899aabbccddeeff",
        );
        assert!(
            parse_private_ingress_output(qdisc, &foreign_cookie, "pppoe-wan", &checkpoint).is_err()
        );
    }

    #[test]
    fn private_calibration_commands_are_exact_and_refuse_foreign_runtime_state() {
        use crate::autotune::{AutotuneProfile, LinkKind};
        use crate::operations::autotune_runtime::{
            AutotuneRuntimePermit, RuntimeBaseline, RuntimeQdiscKind, RuntimeRateBounds,
            RuntimeSnapshot, TemporaryTopologyStage,
        };
        use crate::operations::autotune_runtime_store::RuntimeOverrideCheckpoint;
        use crate::operations::full_autotune::{AutotuneRuntimeControl, MeasurementTopology};
        use crate::operations::identity::ProcessIdentity;

        let _guard = HELPER_TEST_LOCK.lock().unwrap();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "cake-private-topology-{}-{unique}",
            std::process::id()
        ));
        let bin = root.join("bin");
        let sys = root.join("sys/class/net");
        let state = root.join("tc-state");
        let tc_log = root.join("tc.log");
        let ip_log = root.join("ip.log");
        fs::create_dir_all(&bin).unwrap();
        fs::create_dir_all(&sys).unwrap();
        fs::create_dir_all(&state).unwrap();

        let fake_ip = bin.join("ip");
        fs::write(
            &fake_ip,
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$*\" >> \"$FAKE_IP_LOG\"\ncase \"$1:$2\" in\n\
link:add) name=\"$4\"; mkdir -p \"$FAKE_SYS/$name\"; printf '42\\n' > \"$FAKE_SYS/$name/ifindex\"; : > \"$FAKE_SYS/$name/ifalias\" ;;\n\
link:set) name=\"$4\"; if [ \"${5:-}\" = alias ]; then printf '%s\\n' \"$6\" > \"$FAKE_SYS/$name/ifalias\"; fi ;;\n\
-details:link) name=\"$5\"; [ -d \"$FAKE_SYS/$name\" ]; printf '42: %s: <UP> mtu 1500\\n    ifb\\n' \"$name\" ;;\n\
*) if [ \"$1:$2\" = link:delete ]; then rm -rf \"$FAKE_SYS/$4\"; else exit 2; fi ;;\n\
esac\n",
        )
        .unwrap();
        let fake_tc = bin.join("tc");
        fs::write(
            &fake_tc,
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$*\" >> \"$FAKE_TC_LOG\"\ncase \"$1:$2\" in\n\
qdisc:show) dev=\"$4\"; [ ! -f \"$FAKE_TC_STATE/root.$dev\" ] || cat \"$FAKE_TC_STATE/root.$dev\"; [ ! -f \"$FAKE_TC_STATE/ingress.$dev\" ] || cat \"$FAKE_TC_STATE/ingress.$dev\" ;;\n\
filter:show) dev=\"$4\"; [ ! -f \"$FAKE_TC_STATE/filter.$dev\" ] || cat \"$FAKE_TC_STATE/filter.$dev\" ;;\n\
qdisc:replace) dev=\"$4\"; handle=\"$7\"; rate=\"${10}\"; rate=\"${rate%kbit}Kbit\"; case \" $* \" in *' besteffort '*) policy='besteffort nat wash' ;; *) policy='diffserv4 nat' ;; esac; case \" $* \" in *' overhead 44 mpu 84 '*) link='noatm overhead 44 mpu 84' ;; *' overhead 18 mpu 64 '*) link='noatm overhead 18 mpu 64' ;; *) link='raw' ;; esac; printf 'qdisc cake %s root bandwidth %s %s %s\\n' \"$handle\" \"$rate\" \"$policy\" \"$link\" > \"$FAKE_TC_STATE/root.$dev\" ;;\n\
qdisc:add) dev=\"$4\"; printf 'qdisc ingress %s parent ffff:fff1\\n' \"$6\" > \"$FAKE_TC_STATE/ingress.$dev\" ;;\n\
filter:add) dev=\"$4\"; pref=''; handle=''; redirect=''; index=''; cookie=''; while [ \"$#\" -gt 0 ]; do case \"$1\" in pref) pref=\"$2\"; shift 2; continue ;; handle) handle=\"$2\"; shift 2; continue ;; redirect) [ \"$2\" = index ]; index=\"$3\"; [ \"$4\" = dev ]; redirect=\"$5\"; shift 5; continue ;; cookie) cookie=\"$2\"; shift 2; continue ;; esac; shift; done; printf 'filter parent ffff: protocol all pref %s u32 chain 0\\nfilter parent ffff: protocol all pref %s u32 chain 0 fh 800: ht divisor 1\\nfilter parent ffff: protocol all pref %s u32 chain 0 fh %s order 1911 key ht 800 bkt 0 terminal flowid not_in_hw\\n  match 00000000/00000000 at 0\\n action order 1: mirred (Egress Redirect to device %s) stolen\\n index %s ref 1 bind 1\\n cookie %s\\n' \"$pref\" \"$pref\" \"$pref\" \"$handle\" \"$redirect\" \"$index\" \"$cookie\" > \"$FAKE_TC_STATE/filter.$dev\" ;;\n\
qdisc:del) dev=\"$4\"; case \"$5\" in root) rm -f \"$FAKE_TC_STATE/root.$dev\" ;; ingress) rm -f \"$FAKE_TC_STATE/ingress.$dev\" \"$FAKE_TC_STATE/filter.$dev\" ;; *) exit 3 ;; esac ;;\n\
*) exit 4 ;;\n\
esac\n",
        )
        .unwrap();
        fs::set_permissions(&fake_ip, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&fake_tc, fs::Permissions::from_mode(0o700)).unwrap();

        let variable_names = [
            "CAKE_AUTORATE_IP",
            "CAKE_AUTORATE_TC",
            "CAKE_AUTORATE_SYS_CLASS_NET",
            "FAKE_IP_LOG",
            "FAKE_TC_LOG",
            "FAKE_SYS",
            "FAKE_TC_STATE",
        ];
        let previous_values: Vec<_> = variable_names
            .iter()
            .map(|name| env::var_os(name))
            .collect();
        env::set_var("CAKE_AUTORATE_IP", &fake_ip);
        env::set_var("CAKE_AUTORATE_TC", &fake_tc);
        env::set_var("CAKE_AUTORATE_SYS_CLASS_NET", &sys);
        env::set_var("FAKE_IP_LOG", &ip_log);
        env::set_var("FAKE_TC_LOG", &tc_log);
        env::set_var("FAKE_SYS", &sys);
        env::set_var("FAKE_TC_STATE", &state);

        let permit = AutotuneRuntimePermit {
            kind: crate::operations::autotune_runtime::RuntimePermitKind::Autotune,
            permit_id: "77".repeat(16),
            job_id: "11".repeat(16),
            worker_run_id: "22".repeat(16),
            boot_id: "33".repeat(16),
            coordinator_generation: "44".repeat(16),
            worker: ProcessIdentity {
                pid: 100,
                process_group: 100,
                starttime_ticks: 500,
            },
            instance_name: "wan_sqm".to_string(),
            target_interface: "pppoe-wan".to_string(),
            route_identity: "main||pppoe-wan|192.0.2.1||254".to_string(),
            route_fingerprint: "55".repeat(32),
            sqm_fingerprint: "66".repeat(32),
            deadline_boot_ms: 60_000,
            maximum_sequence: 32,
            profile: AutotuneProfile::VariableLink,
            link_kind: LinkKind::Pppoe,
            baseline: RuntimeBaseline::Managed(MeasurementTopology::UploadOnlyShaped),
            initial_download_kbps: 100_000,
            initial_upload_kbps: 50_000,
            download_qdisc_kind: RuntimeQdiscKind::Cake,
            upload_qdisc_kind: RuntimeQdiscKind::Cake,
            allow_bypass_download: true,
            allow_bypass_upload: true,
            download_bounds: RuntimeRateBounds {
                minimum_kbps: 10_000,
                maximum_kbps: 1_000_000,
            },
            upload_bounds: RuntimeRateBounds {
                minimum_kbps: 5_000,
                maximum_kbps: 500_000,
            },
        };
        let baseline = RuntimeSnapshot {
            target_interface: permit.target_interface.clone(),
            route_fingerprint: permit.route_fingerprint.clone(),
            sqm_fingerprint: permit.sqm_fingerprint.clone(),
            topology: MeasurementTopology::UploadOnlyShaped,
            download_kbps: None,
            upload_kbps: Some(45_000),
            download_qdisc_kind: None,
            upload_qdisc_kind: Some(RuntimeQdiscKind::Cake),
        };
        let planned =
            RuntimeOverrideCheckpoint::new(&permit, 1_000, RuntimeBaseline::Managed(baseline))
                .unwrap();
        let suspended = planned
            .advance_temporary_stage(TemporaryTopologyStage::ManagedSqmSuspended, None)
            .unwrap();
        let ifindex = super::create_private_ifb(&suspended).unwrap();
        let owned = suspended
            .advance_temporary_stage(TemporaryTopologyStage::LinkOwned, Some(ifindex))
            .unwrap();
        let control = AutotuneRuntimeControl {
            permit_id: permit.permit_id.clone(),
            job_id: permit.job_id.clone(),
            worker_run_id: permit.worker_run_id.clone(),
            boot_id: permit.boot_id.clone(),
            coordinator_generation: permit.coordinator_generation.clone(),
            worker: permit.worker.clone(),
            sequence: 1,
            deadline_boot_ms: 30_000,
            target_interface: permit.target_interface.clone(),
            route_fingerprint: permit.route_fingerprint.clone(),
            sqm_fingerprint: permit.sqm_fingerprint.clone(),
            topology: MeasurementTopology::ShapedBoth,
            download_kbps: Some(100_000),
            upload_kbps: Some(50_000),
        };
        super::apply_private_runtime_topology(&permit.target_interface, &control, &owned).unwrap();
        let expected = RuntimeSnapshot {
            target_interface: permit.target_interface.clone(),
            route_fingerprint: permit.route_fingerprint.clone(),
            sqm_fingerprint: permit.sqm_fingerprint.clone(),
            topology: MeasurementTopology::ShapedBoth,
            download_kbps: control.download_kbps,
            upload_kbps: control.upload_kbps,
            download_qdisc_kind: Some(RuntimeQdiscKind::Cake),
            upload_qdisc_kind: Some(RuntimeQdiscKind::Cake),
        };
        assert_eq!(
            super::attest_private_runtime(&permit.target_interface, &expected, &owned,).unwrap(),
            expected
        );
        let configured_ifb = "ifb4configured-must-not-be-read".to_string();
        let capture_cfg = Config {
            sqm_interface: permit.target_interface.clone(),
            ul_if: permit.target_interface.clone(),
            dl_if: configured_ifb.clone(),
            ..Config::defaults("wan_sqm".to_string())
        };
        let tc_bytes_before_stage_rejection = fs::read(&tc_log).unwrap().len();
        assert!(
            super::attest_loaded_capture_runtime(&capture_cfg, &permit, &expected, &owned,)
                .unwrap_err()
                .contains("temporary stage link_owned")
        );
        assert_eq!(
            fs::read(&tc_log).unwrap().len(),
            tc_bytes_before_stage_rejection,
            "checkpoint validation and stage binding must precede live tc reads"
        );
        let active = owned
            .advance_temporary_stage(TemporaryTopologyStage::Active, None)
            .unwrap();
        assert_eq!(
            super::attest_loaded_capture_runtime(&capture_cfg, &permit, &expected, &active,)
                .unwrap(),
            expected
        );
        assert!(!fs::read_to_string(&tc_log)
            .unwrap()
            .contains(&configured_ifb));
        super::remove_private_runtime_topology(&permit.target_interface, &active).unwrap();
        assert!(!sys.join(&active.temporary.ifb_name).exists());

        let ifindex = super::create_private_ifb(&suspended).unwrap();
        let owned = suspended
            .advance_temporary_stage(TemporaryTopologyStage::LinkOwned, Some(ifindex))
            .unwrap();
        super::apply_private_runtime_topology(&permit.target_interface, &control, &owned).unwrap();
        let active = owned
            .advance_temporary_stage(TemporaryTopologyStage::Active, None)
            .unwrap();
        let foreign =
            "qdisc cake 8001: root bandwidth 1Mbit diffserv4 nat noatm overhead 44 mpu 84\n";
        fs::write(state.join("root.pppoe-wan"), foreign).unwrap();
        assert!(
            super::remove_private_runtime_topology(&permit.target_interface, &active,).is_err()
        );
        assert_eq!(
            fs::read_to_string(state.join("root.pppoe-wan")).unwrap(),
            foreign
        );
        assert!(sys.join(&active.temporary.ifb_name).exists());

        let tc_commands = fs::read_to_string(&tc_log).unwrap();
        assert!(tc_commands.contains(&format!(
            "qdisc replace dev pppoe-wan root handle {} cake bandwidth 50000kbit",
            active.temporary.target_qdisc_handle
        )));
        assert!(tc_commands.contains(&format!(
            "handle {} u32 match u32 0 0 action mirred egress redirect index {} dev {} cookie {}",
            active.temporary.redirect_filter_tc_handle().unwrap(),
            active.temporary.redirect_action_index,
            active.temporary.ifb_name,
            active.temporary.redirect_action_cookie
        )));
        assert!(!tc_commands.contains("uci"));

        for (name, value) in variable_names.iter().zip(previous_values) {
            if let Some(value) = value {
                env::set_var(name, value);
            } else {
                env::remove_var(name);
            }
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn directional_topology_accepts_only_an_absent_disabled_device() {
        let _guard = HELPER_TEST_LOCK.lock().unwrap();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "cake-directional-topology-{}-{unique}",
            std::process::id()
        ));
        let sys = root.join("sys/class/net");
        let target_stats = sys.join("eth0/statistics");
        let tc = root.join("tc");
        let tc_log = root.join("tc.log");
        fs::create_dir_all(&target_stats).unwrap();
        fs::write(target_stats.join("rx_bytes"), "0\n").unwrap();
        fs::write(target_stats.join("tx_bytes"), "0\n").unwrap();
        fs::write(
            &tc,
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$*\" >> \"$FAKE_TC_LOG\"\ncase \"$*\" in\n\
'qdisc show dev eth0') printf '%s\\n' 'qdisc cake 8001: root bandwidth 1Mbit' ;;\n\
'qdisc show dev ifb4eth0') printf '%s\\n' 'qdisc cake 8002: root bandwidth 1Mbit' ;;\n\
'filter show dev eth0 ingress') : ;;\n\
*) exit 91 ;;\n\
esac\n",
        )
        .unwrap();
        fs::set_permissions(&tc, fs::Permissions::from_mode(0o700)).unwrap();

        let variable_names = [
            "CAKE_AUTORATE_TC",
            "CAKE_AUTORATE_SYS_CLASS_NET",
            "FAKE_TC_LOG",
        ];
        let previous_values: Vec<_> = variable_names
            .iter()
            .map(|name| env::var_os(name))
            .collect();
        env::set_var("CAKE_AUTORATE_TC", &tc);
        env::set_var("CAKE_AUTORATE_SYS_CLASS_NET", &sys);
        env::set_var("FAKE_TC_LOG", &tc_log);

        let mut cfg = Config::defaults("upload_only".to_string());
        cfg.sqm_interface = "eth0".to_string();
        cfg.ul_if = "eth0".to_string();
        cfg.dl_if = "ifb4eth0".to_string();
        cfg.rx_bytes_path = target_stats.join("rx_bytes").to_string_lossy().into_owned();
        cfg.tx_bytes_path = target_stats.join("tx_bytes").to_string_lossy().into_owned();

        super::inspect_sqm_topology_for(&cfg, false, true).unwrap();
        let log = fs::read_to_string(&tc_log).unwrap();
        assert!(log.contains("qdisc show dev eth0"));
        assert!(log.contains("filter show dev eth0 ingress"));
        assert!(!log.contains("qdisc show dev ifb4eth0"));

        let missing_enabled = super::inspect_sqm_topology_for(&cfg, true, true).unwrap_err();
        assert_eq!(missing_enabled.kind, SqmTopologyErrorKind::Settling);
        assert_eq!(missing_enabled.code, "download-device-missing");
        assert!(!fs::read_to_string(&tc_log)
            .unwrap()
            .contains("qdisc show dev ifb4eth0"));

        fs::create_dir_all(sys.join("ifb4eth0")).unwrap();
        let stale_disabled = super::inspect_sqm_topology_for(&cfg, false, true).unwrap_err();
        assert_eq!(stale_disabled.kind, SqmTopologyErrorKind::Unsafe);
        assert_eq!(stale_disabled.code, "disabled-download-cake-remains");

        for (name, value) in variable_names.iter().zip(previous_values) {
            if let Some(value) = value {
                env::set_var(name, value);
            } else {
                env::remove_var(name);
            }
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn direction_mode_disables_only_the_unshaped_controller_and_counter_path() {
        let mut upload_only = Config::defaults("upload_only".to_string());
        upload_only.sqm_enabled = true;
        upload_only.sqm_direction_mode = "upload_only".to_string();
        upload_only.sqm_interface = "eth0".to_string();
        upload_only.ul_if = "eth0".to_string();
        upload_only.dl_if = "ifb4eth0".to_string();
        upload_only.adjust_dl_shaper_rate = false;
        upload_only.normalize_paths();
        assert!(!upload_only.download_shaping_enabled());
        assert!(upload_only.upload_shaping_enabled());
        assert!(!upload_only.adjust_dl_shaper_rate);
        assert_eq!(
            upload_only.rx_bytes_path,
            "/sys/class/net/eth0/statistics/rx_bytes"
        );
        assert_eq!(
            upload_only.tx_bytes_path,
            "/sys/class/net/eth0/statistics/tx_bytes"
        );
        assert!(upload_only.validate().is_ok());

        let mut invalid = upload_only.clone();
        invalid.sqm_direction_mode = "sometimes".to_string();
        assert!(invalid.validate().is_err());
        invalid.sqm_direction_mode = "off".to_string();
        assert!(invalid.validate().is_err());
        invalid.sqm_enabled = false;
        assert!(invalid.validate().is_ok());

        let mut disabled = Config::defaults("disabled".to_string());
        disabled.sqm_enabled = false;
        disabled.sqm_direction_mode = "both".to_string();
        disabled.sqm_interface = "eth0".to_string();
        disabled.ul_if = "eth0".to_string();
        disabled.dl_if = "ifb4eth0".to_string();
        disabled.normalize_paths();
        assert!(!disabled.download_shaping_enabled());
        assert!(!disabled.upload_shaping_enabled());
        assert_eq!(
            disabled.rx_bytes_path,
            "/sys/class/net/eth0/statistics/rx_bytes"
        );
    }

    #[test]
    fn sigterm_while_waiting_for_target_is_a_clean_shutdown() {
        let _guard = HELPER_TEST_LOCK.lock().unwrap();
        let mut cfg = Config::defaults("offline_shutdown".to_string());
        cfg.manage_sqm = true;
        cfg.sqm_enabled = true;
        cfg.sqm_interface = "definitely-missing-interface".to_string();
        cfg.ul_if = cfg.sqm_interface.clone();
        cfg.dl_if = "ifb4definitely-missing-interface".to_string();
        cfg.tx_bytes_path = "/definitely/missing/tx_bytes".to_string();
        cfg.rx_bytes_path = "/definitely/missing/rx_bytes".to_string();
        cfg.startup_wait_s = 0.0;

        TERMINATE.store(true, Ordering::SeqCst);
        let result = run(cfg, true);
        TERMINATE.store(false, Ordering::SeqCst);

        assert_eq!(result, Ok(()));
    }

    #[test]
    fn terminating_sqm_helper_stops_its_process_group() {
        let _guard = HELPER_TEST_LOCK.lock().unwrap();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "cake-autorate-helper-termination-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let helper = root.join("helper");
        let child_pid_path = root.join("child.pid");
        fs::write(
            &helper,
            format!(
                "#!/bin/sh\nsleep 30 &\nprintf '%s\\n' \"$!\" > '{}'\nwait\n",
                child_pid_path.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();

        let previous_helper = env::var_os("CAKE_AUTORATE_SQM_RECOVER");
        env::set_var("CAKE_AUTORATE_SQM_RECOVER", &helper);
        TERMINATE.store(false, Ordering::SeqCst);
        let signal_path = child_pid_path.clone();
        let signal_thread = thread::spawn(move || {
            for _ in 0..100 {
                if signal_path.exists() {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            TERMINATE.store(true, Ordering::SeqCst);
        });

        let cfg = Config::defaults("termination_test".to_string());
        let result = run_sqm_helper(&cfg, Some("check"));
        signal_thread.join().unwrap();
        assert!(matches!(result, Err(SqmRecoveryError::Terminated)));

        let child_pid: u32 = fs::read_to_string(&child_pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let proc_stat = PathBuf::from(format!("/proc/{child_pid}/stat"));
        for _ in 0..20 {
            let running = fs::read_to_string(&proc_stat)
                .ok()
                .and_then(|stat| stat.rsplit_once(')').map(|(_, rest)| rest.to_string()))
                .and_then(|rest| rest.split_whitespace().next().map(str::to_string))
                .map(|state| state != "Z")
                .unwrap_or(false);
            if !running {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        let still_running = fs::read_to_string(&proc_stat)
            .ok()
            .and_then(|stat| stat.rsplit_once(')').map(|(_, rest)| rest.to_string()))
            .and_then(|rest| rest.split_whitespace().next().map(str::to_string))
            .map(|state| state != "Z")
            .unwrap_or(false);
        assert!(!still_running, "SQM helper left a running child process");

        TERMINATE.store(false, Ordering::SeqCst);
        if let Some(value) = previous_helper {
            env::set_var("CAKE_AUTORATE_SQM_RECOVER", value);
        } else {
            env::remove_var("CAKE_AUTORATE_SQM_RECOVER");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn timed_out_runtime_sqm_stop_kills_the_entire_helper_group() {
        let _guard = HELPER_TEST_LOCK.lock().unwrap();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "cake-autorate-runtime-stop-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let helper = root.join("sqm-run");
        let child_pid_path = root.join("child.pid");
        fs::write(
            &helper,
            format!(
                "#!/bin/sh\n[ \"$1\" = stop ] || exit 9\n[ \"$2\" = pppoe-wan ] || exit 10\nsleep 30 &\nprintf '%s\\n' \"$!\" > '{}'\nwait\n",
                child_pid_path.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();

        let error =
            stop_managed_sqm_with(&helper, "pppoe-wan", Duration::from_millis(150), || false)
                .expect_err("the deliberately stalled SQM stop must time out");
        assert!(error.contains("bounded-command-timeout"));

        let child_pid: u32 = fs::read_to_string(&child_pid_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let proc_stat = PathBuf::from(format!("/proc/{child_pid}/stat"));
        for _ in 0..20 {
            let running = fs::read_to_string(&proc_stat)
                .ok()
                .and_then(|stat| stat.rsplit_once(')').map(|(_, rest)| rest.to_string()))
                .and_then(|rest| rest.split_whitespace().next().map(str::to_string))
                .map(|state| state != "Z")
                .unwrap_or(false);
            if !running {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        let still_running = fs::read_to_string(&proc_stat)
            .ok()
            .and_then(|stat| stat.rsplit_once(')').map(|(_, rest)| rest.to_string()))
            .and_then(|rest| rest.split_whitespace().next().map(str::to_string))
            .map(|state| state != "Z")
            .unwrap_or(false);
        assert!(!still_running, "timed-out SQM stop left a child running");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn delayed_topology_recovers_once_then_requires_attestation() {
        let _guard = HELPER_TEST_LOCK.lock().unwrap();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "cake-autorate-delayed-topology-{}-{unique}",
            std::process::id()
        ));
        let sys = root.join("sys");
        let target = sys.join("eth0/statistics");
        let ifb = sys.join("ifb4eth0/statistics");
        let healthy = root.join("healthy");
        let helper_log = root.join("helper.log");
        let tc = root.join("tc");
        let helper = root.join("sqm-recover");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(&ifb).unwrap();
        fs::write(target.join("tx_bytes"), "0\n").unwrap();
        fs::write(ifb.join("tx_bytes"), "0\n").unwrap();
        fs::write(
            &tc,
            format!(
                "#!/bin/sh\n[ -e '{}' ] || exit 0\ncase \"$*\" in\n\
                 'qdisc show dev eth0') printf '%s\\n' 'qdisc cake 8001: root bandwidth 100Mbit' ;;\n\
                 'qdisc show dev ifb4eth0') printf '%s\\n' 'qdisc cake 8002: root bandwidth 500Mbit' ;;\n\
                 'filter show dev eth0 ingress') printf '%s\\n' 'action order 1: mirred (Egress Redirect to device ifb4eth0)' ;;\n\
                 esac\n",
                healthy.display()
            ),
        )
        .unwrap();
        fs::write(
            &helper,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"${{2:-}}\" = check ]; then [ -e '{}' ]; exit; fi\n: > '{}'\n",
                helper_log.display(),
                healthy.display(),
                healthy.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&tc, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();

        let variable_names = [
            "CAKE_AUTORATE_RUN_ROOT",
            "CAKE_AUTORATE_SYS_CLASS_NET",
            "CAKE_AUTORATE_TC",
            "CAKE_AUTORATE_SQM_RECOVER",
        ];
        let previous_values: Vec<_> = variable_names
            .iter()
            .map(|name| env::var_os(name))
            .collect();
        env::set_var("CAKE_AUTORATE_RUN_ROOT", root.join("run"));
        env::set_var("CAKE_AUTORATE_SYS_CLASS_NET", &sys);
        env::set_var("CAKE_AUTORATE_TC", &tc);
        env::set_var("CAKE_AUTORATE_SQM_RECOVER", &helper);
        TERMINATE.store(false, Ordering::SeqCst);

        let mut cfg = Config::defaults("late_wan".to_string());
        cfg.manage_sqm = true;
        cfg.sqm_enabled = true;
        cfg.sqm_interface = "eth0".to_string();
        cfg.ul_if = "eth0".to_string();
        cfg.dl_if = "ifb4eth0".to_string();
        cfg.tx_bytes_path = target.join("tx_bytes").to_string_lossy().into_owned();
        cfg.rx_bytes_path = ifb.join("tx_bytes").to_string_lossy().into_owned();
        cfg.if_up_check_interval_s = 1.0;

        wait_for_runtime_topology(&cfg).unwrap();
        assert_eq!(
            fs::read_to_string(&helper_log).unwrap(),
            "late_wan check\nlate_wan\n"
        );
        fs::write(&helper_log, "").unwrap();
        wait_for_runtime_topology(&cfg).unwrap();
        assert_eq!(fs::read_to_string(&helper_log).unwrap(), "late_wan check\n");

        for (name, value) in variable_names.iter().zip(previous_values) {
            if let Some(value) = value {
                env::set_var(name, value);
            } else {
                env::remove_var(name);
            }
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_link_loss_uses_lock_state_instead_of_a_hotplug_grace() {
        let _guard = HELPER_TEST_LOCK.lock().unwrap();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "cake-autorate-runtime-link-loss-{}-{unique}",
            std::process::id()
        ));
        let sys = root.join("sys");
        let target = sys.join("eth0/statistics");
        let ifb = sys.join("ifb4eth0/statistics");
        let healthy = root.join("healthy");
        let helper_log = root.join("helper.log");
        let tc = root.join("tc");
        let helper = root.join("sqm-recover");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(&ifb).unwrap();
        fs::write(target.join("tx_bytes"), "0\n").unwrap();
        fs::write(ifb.join("tx_bytes"), "0\n").unwrap();
        fs::write(
            &tc,
            format!(
                "#!/bin/sh\n[ -e '{}' ] || exit 0\ncase \"$*\" in\n\
                 'qdisc show dev eth0') printf '%s\\n' 'qdisc cake 8001: root bandwidth 100Mbit' ;;\n\
                 'qdisc show dev ifb4eth0') printf '%s\\n' 'qdisc cake 8002: root bandwidth 500Mbit' ;;\n\
                 'filter show dev eth0 ingress') printf '%s\\n' 'action order 1: mirred (Egress Redirect to device ifb4eth0)' ;;\n\
                 esac\n",
                healthy.display()
            ),
        )
        .unwrap();
        fs::write(
            &helper,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nif [ \"${{2:-}}\" = check ]; then [ -e '{}' ]; exit; fi\n: > '{}'\n",
                helper_log.display(),
                healthy.display(),
                healthy.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&tc, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();

        let variable_names = [
            "CAKE_AUTORATE_RUN_ROOT",
            "CAKE_AUTORATE_SYS_CLASS_NET",
            "CAKE_AUTORATE_TC",
            "CAKE_AUTORATE_SQM_RECOVER",
        ];
        let previous_values: Vec<_> = variable_names
            .iter()
            .map(|name| env::var_os(name))
            .collect();
        env::set_var("CAKE_AUTORATE_RUN_ROOT", root.join("run"));
        env::set_var("CAKE_AUTORATE_SYS_CLASS_NET", &sys);
        env::set_var("CAKE_AUTORATE_TC", &tc);
        env::set_var("CAKE_AUTORATE_SQM_RECOVER", &helper);

        let mut cfg = Config::defaults("runtime_link_loss".to_string());
        cfg.manage_sqm = true;
        cfg.sqm_enabled = true;
        cfg.sqm_interface = "eth0".to_string();
        cfg.ul_if = "eth0".to_string();
        cfg.dl_if = "ifb4eth0".to_string();
        cfg.tx_bytes_path = target.join("tx_bytes").to_string_lossy().into_owned();
        cfg.rx_bytes_path = ifb.join("tx_bytes").to_string_lossy().into_owned();
        cfg.if_up_check_interval_s = 1.0;
        cfg.log_to_file = false;
        cfg.adjust_dl_shaper_rate = false;
        cfg.adjust_ul_shaper_rate = false;
        let mut controller = Controller::new(cfg).unwrap();

        controller.runtime_operation_active = true;
        fs::remove_dir_all(sys.join("eth0")).unwrap();
        let (ready, recovered) = controller.ensure_managed_sqm();
        assert!(ready && !recovered);
        assert_eq!(controller.sqm_runtime_state, "WAITING_OPERATION");
        assert_eq!(controller.sqm_recovery_attempts, 0);
        assert!(!helper_log.exists());

        controller.runtime_operation_active = false;
        let (ready, recovered) = controller.ensure_managed_sqm();
        assert!(!ready && !recovered);
        assert_eq!(controller.sqm_runtime_state, "WAITING_LINK");
        assert_eq!(controller.sqm_recovery_attempts, 0);
        assert!(!helper_log.exists());

        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("tx_bytes"), "0\n").unwrap();
        let (ready, recovered) = controller.ensure_managed_sqm();
        assert!(ready && recovered);
        assert_eq!(controller.sqm_runtime_state, "HEALTHY");
        assert_eq!(controller.sqm_recovery_attempts, 1);
        assert_eq!(
            fs::read_to_string(&helper_log).unwrap(),
            "runtime_link_loss check\nruntime_link_loss\n"
        );

        fs::write(&helper_log, "").unwrap();
        let (ready, recovered) = controller.ensure_managed_sqm();
        assert!(ready && !recovered);
        assert!(fs::read_to_string(&helper_log).unwrap().is_empty());

        fs::remove_file(&healthy).unwrap();
        let (ready, recovered) = controller.ensure_managed_sqm();
        assert!(ready && recovered);
        assert_eq!(controller.sqm_recovery_attempts, 2);
        assert_eq!(
            fs::read_to_string(&helper_log).unwrap(),
            "runtime_link_loss check\nruntime_link_loss\n"
        );

        for (name, value) in variable_names.iter().zip(previous_values) {
            if let Some(value) = value {
                env::set_var(name, value);
            } else {
                env::remove_var(name);
            }
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unchanged_failed_sqm_generation_never_retries_until_topology_changes() {
        let _guard = HELPER_TEST_LOCK.lock().unwrap();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "cake-autorate-sqm-generation-gate-{}-{unique}",
            std::process::id()
        ));
        let sys = root.join("sys");
        let target = sys.join("eth0/statistics");
        let ifb = sys.join("ifb4eth0/statistics");
        let healthy = root.join("healthy");
        let helper_mode = root.join("helper-mode");
        let tc_generation = root.join("tc-generation");
        let helper_log = root.join("helper.log");
        let tc = root.join("tc");
        let helper = root.join("sqm-recover");
        fs::create_dir_all(&target).unwrap();
        fs::create_dir_all(&ifb).unwrap();
        fs::write(target.join("tx_bytes"), "0\n").unwrap();
        fs::write(ifb.join("tx_bytes"), "0\n").unwrap();
        fs::write(&tc_generation, "1\n").unwrap();
        fs::write(
            &tc,
            format!(
                "#!/bin/sh\nif [ -e '{}' ]; then\ncase \"$*\" in\n\
                 'qdisc show dev eth0') printf '%s\\n' 'qdisc cake 8001: root bandwidth 100Mbit' ;;\n\
                 'qdisc show dev ifb4eth0') printf '%s\\n' 'qdisc cake 8002: root bandwidth 500Mbit' ;;\n\
                 'filter show dev eth0 ingress') printf '%s\\n' 'action order 1: mirred (Egress Redirect to device ifb4eth0)' ;;\n\
                 esac\nelse\ngeneration=$(sed -n '1p' '{}')\ncase \"$*\" in\n\
                 'qdisc show dev eth0'|'qdisc show dev ifb4eth0') printf 'qdisc fq_codel %s: root\\n' \"$generation\" ;;\n\
                 esac\nfi\n",
                healthy.display(),
                tc_generation.display()
            ),
        )
        .unwrap();
        fs::write(
            &helper,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nmode=$(sed -n '1p' '{}')\nif [ \"${{2:-}}\" = check ]; then\n[ -e '{}' ] && exit 0\n[ \"$mode\" = busy ] && exit 75\nexit 1\nfi\n[ \"$mode\" = busy ] && exit 75\n[ \"$mode\" = fail ] && exit 1\n: > '{}'\n",
                helper_log.display(),
                helper_mode.display(),
                healthy.display(),
                healthy.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&tc, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();

        let variable_names = [
            "CAKE_AUTORATE_RUN_ROOT",
            "CAKE_AUTORATE_SYS_CLASS_NET",
            "CAKE_AUTORATE_TC",
            "CAKE_AUTORATE_SQM_RECOVER",
        ];
        let previous_values: Vec<_> = variable_names
            .iter()
            .map(|name| env::var_os(name))
            .collect();
        env::set_var("CAKE_AUTORATE_RUN_ROOT", root.join("run"));
        env::set_var("CAKE_AUTORATE_SYS_CLASS_NET", &sys);
        env::set_var("CAKE_AUTORATE_TC", &tc);
        env::set_var("CAKE_AUTORATE_SQM_RECOVER", &helper);
        TERMINATE.store(false, Ordering::SeqCst);

        let mut cfg = Config::defaults("generation_gate".to_string());
        cfg.manage_sqm = true;
        cfg.sqm_enabled = true;
        cfg.sqm_interface = "eth0".to_string();
        cfg.ul_if = "eth0".to_string();
        cfg.dl_if = "ifb4eth0".to_string();
        cfg.tx_bytes_path = target.join("tx_bytes").to_string_lossy().into_owned();
        cfg.rx_bytes_path = ifb.join("tx_bytes").to_string_lossy().into_owned();
        cfg.log_to_file = false;
        cfg.adjust_dl_shaper_rate = false;
        cfg.adjust_ul_shaper_rate = false;
        let mut controller = Controller::new(cfg).unwrap();

        fs::write(&helper_mode, "busy\n").unwrap();
        for _ in 0..8 {
            let (ready, recovered) = controller.ensure_managed_sqm();
            assert!(!ready && !recovered);
        }
        assert_eq!(controller.sqm_recovery_attempts, 0);
        let busy_log = fs::read_to_string(&helper_log).unwrap();
        assert_eq!(
            busy_log
                .lines()
                .filter(|line| line.ends_with(" check"))
                .count(),
            8
        );
        assert_eq!(
            busy_log
                .lines()
                .filter(|line| *line == "generation_gate")
                .count(),
            0
        );

        fs::write(&helper_mode, "fail\n").unwrap();
        let (ready, recovered) = controller.ensure_managed_sqm();
        assert!(!ready && !recovered);
        assert_eq!(controller.sqm_recovery_attempts, 1);
        for _ in 0..64 {
            let (ready, recovered) = controller.ensure_managed_sqm();
            assert!(!ready && !recovered);
        }
        let failed_log = fs::read_to_string(&helper_log).unwrap();
        assert_eq!(
            failed_log
                .lines()
                .filter(|line| *line == "generation_gate")
                .count(),
            1
        );
        assert_eq!(controller.sqm_recovery_attempts, 1);
        assert_eq!(controller.sqm_runtime_state, "WAITING_SQM");

        fs::write(&tc_generation, "2\n").unwrap();
        fs::write(&helper_mode, "recover\n").unwrap();
        let (ready, recovered) = controller.ensure_managed_sqm();
        assert!(ready && recovered);
        assert_eq!(controller.sqm_recovery_attempts, 2);
        let recovered_log = fs::read_to_string(&helper_log).unwrap();
        assert_eq!(
            recovered_log
                .lines()
                .filter(|line| *line == "generation_gate")
                .count(),
            2
        );

        for (name, value) in variable_names.iter().zip(previous_values) {
            if let Some(value) = value {
                env::set_var(name, value);
            } else {
                env::remove_var(name);
            }
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sqm_recovery_source_has_no_time_authorized_grace_backoff_or_cooldown() {
        let source = include_str!("main.rs");
        for forbidden in [
            concat!("SQM_BOOTSTRAP_", "HOTPLUG_GRACE_S"),
            concat!("SQM_BOOTSTRAP_RECOVERY_", "BACKOFF_INITIAL_S"),
            concat!("SQM_BOOTSTRAP_RECOVERY_", "BACKOFF_MAX_S"),
            concat!("SQM_RUNTIME_RECOVERY_", "COOLDOWN_S"),
            concat!("bootstrap_recovery_", "backoff"),
            concat!("bootstrap_hotplug_", "grace"),
            concat!("sqm_last_recovery_", "attempt"),
            concat!("sqm_topology_missing_", "since"),
        ] {
            assert!(
                !source.contains(forbidden),
                "managed SQM recovery still contains timer-owned state: {forbidden}"
            );
        }
        assert!(source.contains("SqmRecoveryGate"));
        assert!(source.contains("WaitForStateChange"));
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
        let base = Instant::now();
        monitor.last = base;
        let stale = monitor
            .try_sample_at(base + Duration::from_millis(199))
            .unwrap();
        assert!(!stale.fresh);
        assert_eq!((stale.dl_kbps, stale.ul_kbps), (0.0, 0.0));

        let fresh = monitor
            .try_sample_at(base + Duration::from_millis(200))
            .unwrap();
        assert!(fresh.fresh);
        let dl = fresh.dl_kbps;
        let ul = fresh.ul_kbps;
        assert!(dl > 30_000.0, "download delta was not retained: {dl}");
        assert!(ul > 15_000.0, "upload delta was not retained: {ul}");
        assert!((dl / ul - 2.0).abs() < 0.01);

        fs::write(&rx, "2000000\n").unwrap();
        fs::write(&tx, "1000000\n").unwrap();
        let cached = monitor
            .try_sample_at(base + Duration::from_millis(399))
            .unwrap();
        assert!(!cached.fresh);
        assert_eq!(cached.dl_kbps, fresh.dl_kbps);
        assert_eq!(cached.ul_kbps, fresh.ul_kbps);
        assert_eq!(cached.dl_observed_at, fresh.dl_observed_at);
        assert_eq!(cached.ul_observed_at, fresh.ul_observed_at);
        let next = monitor
            .try_sample_at(base + Duration::from_millis(400))
            .unwrap();
        assert!(next.fresh, "an exact cadence tick must not phase-jitter");
        assert!((next.dl_kbps / next.ul_kbps - 2.0).abs() < 0.01);
        fs::remove_dir_all(root).unwrap();
    }

    fn test_autotune_capture_request(
        sequence: u32,
        phase: crate::operations::full_autotune::AutotuneCapturePhase,
        topology: crate::operations::full_autotune::MeasurementTopology,
        direction: Option<crate::operations::protocol::SpeedtestDirection>,
    ) -> crate::operations::full_autotune::AutotuneCaptureRequest {
        crate::operations::full_autotune::AutotuneCaptureRequest {
            capture_id: format!("{:032x}", sequence),
            job_id: "b".repeat(32),
            worker_run_id: "c".repeat(32),
            permit_id: "d".repeat(32),
            instance_name: "wan_sqm".to_string(),
            sequence,
            control_sequence: sequence,
            deadline_boot_ms: 100_000,
            phase,
            topology,
            direction,
            candidate_dl_kbps: None,
            candidate_ul_kbps: Some(723_400),
            load_reference_kbps: Some(900_000),
            transport_baseline_us: (phase
                == crate::operations::full_autotune::AutotuneCapturePhase::LoadedMeasurement)
                .then_some(10_000),
            route_fingerprint: "e".repeat(64),
            sqm_fingerprint: "f".repeat(64),
        }
    }

    #[test]
    fn load_evidence_published_during_admission_uses_post_read_monotonic_time() {
        use crate::operations::autotune_capture::AutotuneCaptureObservationKind;
        use crate::operations::full_autotune::{
            AutotuneCapturePhase, AutotuneLoadEvidence, MeasurementTopology,
        };
        use crate::operations::protocol::SpeedtestDirection;

        let request = test_autotune_capture_request(
            41,
            AutotuneCapturePhase::LoadedMeasurement,
            MeasurementTopology::RawDownload,
            Some(SpeedtestDirection::Download),
        );
        let evidence = AutotuneLoadEvidence {
            request: request.clone(),
            published_boot_ms: 20_100,
            run_count: 1,
            aggregate_rx_bytes: 1_100_000,
            aggregate_tx_bytes: 50_000,
            confidence_total_bytes: 1_100_000,
            controlled_wire_bytes: 1_000_000,
            controlled_payload_bytes: 1_000_000,
            counter_elapsed_ms: 10_000,
            direction_elapsed_ms: 10_000,
            backend_reported_kbps: 800,
            backend_consistent_runs: 1,
            backend_payload_only_runs: 0,
            realized_kbps: 800,
            goodput_kbps: 800,
        };

        assert!(
            loaded_autotune_traffic_observation_after_read(&request, &evidence, || { Ok(20_000) })
                .unwrap_err()
                .contains("outside its monotonic lifetime")
        );
        assert_eq!(
            loaded_autotune_traffic_observation_after_read(&request, &evidence, || Ok(20_200))
                .unwrap(),
            AutotuneCaptureObservationKind::Traffic {
                background_confidence_percent: 90,
                contaminated: false,
            }
        );
        assert!(
            loaded_autotune_traffic_observation_after_read(&request, &evidence, || {
                Ok(request.deadline_boot_ms + 1)
            })
            .unwrap_err()
            .contains("outside its monotonic lifetime")
        );
        let mut wrong = evidence.clone();
        wrong.request.capture_id = "9".repeat(32);
        assert!(
            loaded_autotune_traffic_observation_after_read(&request, &wrong, || Ok(20_200))
                .unwrap_err()
                .contains("identity mismatch")
        );
        assert!(
            loaded_autotune_traffic_observation_after_read(&request, &evidence, || {
                Err("clock unavailable".to_string())
            })
            .unwrap_err()
            .contains("after reading")
        );
    }

    #[test]
    fn load_evidence_rejection_uses_fresh_time_without_clamping() {
        use crate::operations::autotune_capture::AutotuneCaptureAccumulator;
        use crate::operations::full_autotune::{AutotuneCapturePhase, MeasurementTopology};
        use crate::operations::protocol::SpeedtestDirection;

        let request = test_autotune_capture_request(
            41,
            AutotuneCapturePhase::LoadedMeasurement,
            MeasurementTopology::RawDownload,
            Some(SpeedtestDirection::Download),
        );
        let mut accumulator = AutotuneCaptureAccumulator::new();
        accumulator.admit(&request, 20_000).unwrap();
        let rejected = reject_autotune_capture_after_io_with_clock(
            &mut accumulator,
            "capture-load-evidence-invalid",
            || Ok(20_200),
        )
        .unwrap();
        assert_eq!(rejected.updated_boot_ms, 20_200);

        let mut no_clock = AutotuneCaptureAccumulator::new();
        no_clock.admit(&request, 20_000).unwrap();
        assert!(reject_autotune_capture_after_io_with_clock(
            &mut no_clock,
            "capture-load-evidence-invalid",
            || Err("clock unavailable".to_string()),
        )
        .is_none());
        assert!(no_clock.accepting_observations());

        let mut expired = AutotuneCaptureAccumulator::new();
        expired.admit(&request, 20_000).unwrap();
        assert!(reject_autotune_capture_after_io_with_clock(
            &mut expired,
            "capture-load-evidence-invalid",
            || Ok(request.deadline_boot_ms + 1),
        )
        .is_none());
        assert!(expired.accepting_observations());
    }

    #[test]
    fn autotune_counter_completion_requires_full_identity_and_current_epoch() {
        use crate::operations::autotune_counter::AutotuneCounterCompletion;
        use crate::operations::full_autotune::{AutotuneCapturePhase, MeasurementTopology};
        use crate::operations::protocol::SpeedtestDirection;

        let request = test_autotune_capture_request(
            41,
            AutotuneCapturePhase::LoadedMeasurement,
            MeasurementTopology::RawDownload,
            Some(SpeedtestDirection::Download),
        );
        let monitor = AutotuneSpeedtestRateMonitor {
            request: request.clone(),
            counter_epoch: 2,
            monitor: SpeedtestCounterRateMonitor::new(200),
        };
        let completion = AutotuneCounterCompletion {
            request: request.clone(),
            epoch: 1,
            completed_at: Instant::now(),
            completed_boot_ms: 99_000,
            outcome: Ok(None),
        };
        assert!(
            !autotune_counter_completion_matches(&request, &monitor, &completion),
            "an exact request replay from an invalidated sampler epoch must remain stale"
        );
        let current = AutotuneCounterCompletion {
            epoch: 2,
            ..completion
        };
        assert!(autotune_counter_completion_matches(
            &request, &monitor, &current
        ));
        assert!(autotune_counter_completion_is_timely(&request, &current));
        let late = AutotuneCounterCompletion {
            request: request.clone(),
            epoch: 2,
            completed_at: Instant::now(),
            completed_boot_ms: request.deadline_boot_ms + 1,
            outcome: Ok(None),
        };
        assert!(!autotune_counter_completion_is_timely(&request, &late));
        let rotated = test_autotune_capture_request(
            42,
            AutotuneCapturePhase::LoadedMeasurement,
            MeasurementTopology::RawDownload,
            Some(SpeedtestDirection::Download),
        );
        assert!(!autotune_counter_completion_matches(
            &rotated, &monitor, &current
        ));
    }

    #[test]
    fn autotune_capture_attestation_lease_bridges_one_slow_read_but_expires_fail_closed() {
        use crate::operations::full_autotune::{AutotuneCapturePhase, MeasurementTopology};
        use crate::operations::protocol::SpeedtestDirection;

        let request = test_autotune_capture_request(
            41,
            AutotuneCapturePhase::LoadedMeasurement,
            MeasurementTopology::RawDownload,
            Some(SpeedtestDirection::Download),
        );
        let mut rotated = request.clone();
        rotated.capture_id = "9".repeat(32);
        let attested_at = Instant::now();

        let slow_attestation_started = attested_at;
        let slow_attestation_completed = slow_attestation_started
            + AUTOTUNE_CAPTURE_ATTESTATION_MAX_AGE
            + Duration::from_secs(1);
        let first_observation = slow_attestation_completed + Duration::from_millis(1);
        assert!(autotune_capture_attestation_lease_valid(
            slow_attestation_completed,
            first_observation,
            &request,
            Some(&request),
            request.deadline_boot_ms,
        ));
        assert!(!autotune_capture_attestation_lease_valid(
            slow_attestation_started,
            first_observation,
            &request,
            Some(&request),
            request.deadline_boot_ms,
        ));

        assert!(autotune_capture_attestation_lease_valid(
            attested_at,
            attested_at + Duration::from_millis(1_250),
            &request,
            Some(&request),
            request.deadline_boot_ms,
        ));
        assert!(!autotune_capture_attestation_lease_valid(
            attested_at,
            attested_at + AUTOTUNE_CAPTURE_ATTESTATION_MAX_AGE,
            &request,
            Some(&request),
            request.deadline_boot_ms,
        ));
        assert!(!autotune_capture_attestation_lease_valid(
            attested_at,
            attested_at + Duration::from_millis(1_250),
            &request,
            None,
            request.deadline_boot_ms,
        ));
        assert!(!autotune_capture_attestation_lease_valid(
            attested_at,
            attested_at + Duration::from_millis(1_250),
            &request,
            Some(&rotated),
            request.deadline_boot_ms,
        ));
        assert!(!autotune_capture_attestation_lease_valid(
            attested_at,
            attested_at + Duration::from_millis(1_250),
            &request,
            Some(&request),
            request.deadline_boot_ms + 1,
        ));
    }

    fn test_transport_result(
        request: &crate::operations::full_autotune::AutotuneCaptureRequest,
        probe_id: u64,
        started_at: Instant,
        completed_at: Instant,
        phase: (bool, bool),
    ) -> TransportProbeResult {
        TransportProbeResult {
            probe_id,
            started_at,
            completed_at,
            control_valid: true,
            capture_interval_valid: None,
            endpoint: "wss://example.invalid/latency".to_string(),
            dl_loaded: phase.0,
            ul_loaded: phase.1,
            rating_phase: crate::rating_load::RatingPhase::Idle,
            autotune_capture: Some(request.clone()),
            latency_ms: Some(42.0),
            error: None,
            failure_kind: None,
            failure_deadline_us: None,
            route_identity: Some("mwan3|wan|pppoe-wan|192.0.2.2|0x100|100".to_string()),
            backend: "websocket".to_string(),
            trusted: true,
            raw_samples_ms: vec![42.0],
            discarded_samples: 0,
            server_processing_ms: 0.0,
            connection_reused: true,
        }
    }

    fn test_transport_delta(
        start: Instant,
        end: Instant,
    ) -> crate::operations::autotune_counter::AutotuneCounterDelta {
        crate::operations::autotune_counter::AutotuneCounterDelta {
            download_bytes: 0,
            upload_bytes: 0,
            observed_start: start,
            observed_end: end,
            within_maximum_span: true,
        }
    }

    #[test]
    fn autotune_transport_physical_delta_starts_speculative_probe_before_hold() {
        use crate::operations::full_autotune::{AutotuneCapturePhase, MeasurementTopology};
        use crate::operations::protocol::SpeedtestDirection;

        let request = test_autotune_capture_request(
            51,
            AutotuneCapturePhase::LoadedMeasurement,
            MeasurementTopology::RawUpload,
            Some(SpeedtestDirection::Upload),
        );
        let key = AutotuneTransportCaptureKey::new(&request, "route-a");
        let base = Instant::now();
        let end = base + Duration::from_millis(800);
        let dropout = Duration::from_millis(600);
        let mut control = AutotuneTransportControl::default();
        control.observe(
            key.clone(),
            None,
            (false, true),
            end,
            dropout,
            Duration::from_secs(10),
        );
        control
            .observe_physical_delta(
                &key,
                test_transport_delta(base, end),
                Some((false, true)),
                Duration::from_secs(10),
            )
            .unwrap();

        let readiness = control
            .ready_phase_diagnostic(&request, &key, end, Duration::from_secs(3), dropout)
            .unwrap();
        assert!(readiness.ready);
        assert_eq!(readiness.reason, "loaded-physical-delta-ready");
    }

    #[test]
    fn autotune_transport_physical_delta_rejects_a_span_beyond_dropout() {
        use crate::operations::full_autotune::{AutotuneCapturePhase, MeasurementTopology};
        use crate::operations::protocol::SpeedtestDirection;

        let request = test_autotune_capture_request(
            54,
            AutotuneCapturePhase::LoadedMeasurement,
            MeasurementTopology::RawUpload,
            Some(SpeedtestDirection::Upload),
        );
        let base = Instant::now();
        let mut bounded = test_transport_delta(base, base + Duration::from_millis(200));
        bounded.upload_bytes = 30_000_000;
        assert_eq!(
            autotune_transport_delta_phase(
                &request,
                bounded,
                Duration::from_millis(600),
                100.0,
                0.05,
            )
            .unwrap(),
            Some((false, true))
        );

        let mut overlong = test_transport_delta(base, base + Duration::from_millis(601));
        overlong.upload_bytes = 90_000_000;
        assert_eq!(
            autotune_transport_delta_phase(
                &request,
                overlong,
                Duration::from_millis(600),
                100.0,
                0.05,
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn autotune_transport_result_waits_for_physical_bracket_then_accepts() {
        use crate::operations::full_autotune::{AutotuneCapturePhase, MeasurementTopology};
        use crate::operations::protocol::SpeedtestDirection;

        let request = test_autotune_capture_request(
            52,
            AutotuneCapturePhase::LoadedMeasurement,
            MeasurementTopology::RawUpload,
            Some(SpeedtestDirection::Upload),
        );
        let key = AutotuneTransportCaptureKey::new(&request, "route-a");
        let base = Instant::now();
        let first_end = base + Duration::from_millis(800);
        let dropout = Duration::from_millis(600);
        let mut control = AutotuneTransportControl::default();
        control.observe(
            key.clone(),
            Some((false, true)),
            (false, true),
            first_end,
            dropout,
            Duration::from_secs(10),
        );
        control
            .observe_physical_delta(
                &key,
                test_transport_delta(base, first_end),
                Some((false, true)),
                Duration::from_secs(10),
            )
            .unwrap();
        let flight = AutotuneTransportFlight {
            probe_id: 52,
            key: key.clone(),
            expected_phase: (false, true),
            control_valid: true,
            submitted_at: first_end,
            physical_delta_required: true,
        };
        let result = test_transport_result(
            &request,
            52,
            first_end + Duration::from_millis(20),
            first_end + Duration::from_millis(300),
            (false, true),
        );
        assert!(matches!(
            control.settle_diagnostic(&flight, &result, dropout),
            AutotuneTransportSettlement::Pending("physical-bracket-pending")
        ));

        control
            .observe_physical_delta(
                &key,
                test_transport_delta(first_end, first_end + Duration::from_millis(400)),
                Some((false, true)),
                Duration::from_secs(10),
            )
            .unwrap();
        let AutotuneTransportSettlement::Final(attestation) =
            control.settle_diagnostic(&flight, &result, dropout)
        else {
            panic!("bracketed result remained pending");
        };
        assert!(attestation.valid);
        assert_eq!(attestation.reason, "physical-loaded-flight-valid");
    }

    #[test]
    fn autotune_transport_rolling_overhang_cannot_validate_idle_physical_flight() {
        use crate::operations::full_autotune::{AutotuneCapturePhase, MeasurementTopology};
        use crate::operations::protocol::SpeedtestDirection;

        let request = test_autotune_capture_request(
            53,
            AutotuneCapturePhase::LoadedMeasurement,
            MeasurementTopology::RawUpload,
            Some(SpeedtestDirection::Upload),
        );
        let key = AutotuneTransportCaptureKey::new(&request, "route-a");
        let base = Instant::now();
        let first_end = base + Duration::from_millis(800);
        let dropout = Duration::from_millis(600);
        let mut control = AutotuneTransportControl::default();
        control.observe(
            key.clone(),
            Some((false, true)),
            (false, true),
            first_end,
            dropout,
            Duration::from_secs(10),
        );
        control
            .observe_physical_delta(
                &key,
                test_transport_delta(base, first_end),
                Some((false, true)),
                Duration::from_secs(10),
            )
            .unwrap();
        let flight = AutotuneTransportFlight {
            probe_id: 53,
            key: key.clone(),
            expected_phase: (false, true),
            control_valid: true,
            submitted_at: first_end,
            physical_delta_required: true,
        };
        let result = test_transport_result(
            &request,
            53,
            first_end + Duration::from_millis(20),
            first_end + Duration::from_millis(300),
            (false, true),
        );
        // The smoothed rate still says upload-loaded, but the exact next
        // physical delta is idle.  Only the physical interval may settle the
        // completed probe.
        control.observe(
            key.clone(),
            Some((false, true)),
            (false, true),
            first_end + Duration::from_millis(400),
            dropout,
            Duration::from_secs(10),
        );
        control
            .observe_physical_delta(
                &key,
                test_transport_delta(first_end, first_end + Duration::from_millis(400)),
                Some((false, false)),
                Duration::from_secs(10),
            )
            .unwrap();
        let AutotuneTransportSettlement::Final(attestation) =
            control.settle_diagnostic(&flight, &result, dropout)
        else {
            panic!("bracketed idle result remained pending");
        };
        assert!(!attestation.valid);
        assert_eq!(attestation.reason, "physical-flight-coverage-insufficient");
    }

    #[test]
    fn autotune_transport_physical_coverage_is_exact_and_gaps_fail_closed() {
        use crate::operations::full_autotune::{AutotuneCapturePhase, MeasurementTopology};
        use crate::operations::protocol::SpeedtestDirection;

        let request = test_autotune_capture_request(
            55,
            AutotuneCapturePhase::LoadedMeasurement,
            MeasurementTopology::RawDownload,
            Some(SpeedtestDirection::Download),
        );
        let key = AutotuneTransportCaptureKey::new(&request, "route-a");
        let base = Instant::now();
        let end = base + Duration::from_secs(1);
        let dropout = Duration::from_millis(600);
        let flight = AutotuneTransportFlight {
            probe_id: 55,
            key: key.clone(),
            expected_phase: (true, false),
            control_valid: true,
            submitted_at: base,
            physical_delta_required: true,
        };
        let result = test_transport_result(&request, 55, base, end, (true, false));

        let mut exact = AutotuneTransportControl::default();
        exact.observe(
            key.clone(),
            Some((true, false)),
            (true, false),
            base,
            dropout,
            Duration::from_secs(10),
        );
        exact
            .observe_physical_delta(
                &key,
                test_transport_delta(base, base + Duration::from_millis(700)),
                Some((true, false)),
                Duration::from_secs(10),
            )
            .unwrap();
        exact
            .observe_physical_delta(
                &key,
                test_transport_delta(base + Duration::from_millis(700), end),
                Some((false, false)),
                Duration::from_secs(10),
            )
            .unwrap();
        let AutotuneTransportSettlement::Final(attestation) =
            exact.settle_diagnostic(&flight, &result, dropout)
        else {
            panic!("fully bracketed exact-threshold result remained pending");
        };
        assert!(attestation.valid, "exact 70% coverage must be accepted");

        let mut gap = AutotuneTransportControl::default();
        gap.observe(
            key.clone(),
            Some((true, false)),
            (true, false),
            base,
            dropout,
            Duration::from_secs(10),
        );
        gap.observe_physical_delta(
            &key,
            test_transport_delta(base, base + Duration::from_millis(100)),
            Some((true, false)),
            Duration::from_secs(10),
        )
        .unwrap();
        gap.observe_physical_delta(
            &key,
            test_transport_delta(base + Duration::from_millis(701), end),
            Some((true, false)),
            Duration::from_secs(10),
        )
        .unwrap();
        let AutotuneTransportSettlement::Final(attestation) =
            gap.settle_diagnostic(&flight, &result, dropout)
        else {
            panic!("fully bracketed gap result remained pending");
        };
        assert!(!attestation.valid);
        assert_eq!(attestation.reason, "physical-flight-dropout-exceeded");

        assert!(gap
            .observe_physical_delta(
                &key,
                test_transport_delta(base + Duration::from_millis(900), end),
                Some((true, false)),
                Duration::from_secs(10),
            )
            .unwrap_err()
            .contains("overlap"));
    }

    #[test]
    fn managed_transport_deadline_requires_exact_capture_route_and_flight_attestation() {
        use crate::operations::autotune_capture::AutotuneCaptureObservationKind;
        use crate::operations::full_autotune::{AutotuneCapturePhase, MeasurementTopology};
        use crate::operations::protocol::SpeedtestDirection;
        use crate::transport_probe::TransportProbeFailureKind;

        let request = test_autotune_capture_request(
            1,
            AutotuneCapturePhase::LoadedMeasurement,
            MeasurementTopology::RawDownload,
            Some(SpeedtestDirection::Download),
        );
        let started_at = Instant::now();
        let mut result = test_transport_result(
            &request,
            1,
            started_at,
            started_at + Duration::from_secs(5),
            (true, false),
        );
        result.latency_ms = None;
        result.raw_samples_ms.clear();
        result.error = Some("transport deadline exceeded".to_string());
        result.failure_kind = Some(TransportProbeFailureKind::DeadlineExceeded);
        result.failure_deadline_us = Some(5_000_000);
        result.capture_interval_valid = Some(true);
        let route = result.route_identity.clone().unwrap();
        assert_eq!(
            censored_autotune_transport_observation(&result, Some(&request), Some(&route),)
                .unwrap(),
            Some(AutotuneCaptureObservationKind::TransportDeadlineExceeded {
                deadline_us: 5_000_000,
                delta_lower_bound_us: 4_990_000,
            })
        );

        let mut invalid_flight = result.clone();
        invalid_flight.capture_interval_valid = Some(false);
        assert_eq!(
            censored_autotune_transport_observation(&invalid_flight, Some(&request), Some(&route),)
                .unwrap(),
            None
        );
        let mut ordinary_failure = result.clone();
        ordinary_failure.failure_kind = Some(TransportProbeFailureKind::Other);
        ordinary_failure.failure_deadline_us = None;
        assert_eq!(
            censored_autotune_transport_observation(
                &ordinary_failure,
                Some(&request),
                Some(&route),
            )
            .unwrap(),
            None
        );
        let mut malformed = result;
        malformed.failure_deadline_us = None;
        assert!(
            censored_autotune_transport_observation(&malformed, Some(&request), Some(&route),)
                .is_err()
        );
    }

    #[test]
    fn autotune_loaded_transport_uses_its_request_baseline_before_the_runtime_tracker_is_ready() {
        use crate::operations::autotune_capture::{
            transport_observation_kind, AutotuneCaptureObservationKind,
        };
        use crate::operations::full_autotune::{AutotuneCapturePhase, MeasurementTopology};
        use crate::operations::protocol::SpeedtestDirection;

        let now = Instant::now();
        let mut runtime_tracker = TransportLatencyTracker::new();
        for index in 0..15 {
            runtime_tracker.observe_success(
                "wss://example.invalid/probe",
                10.0 + f64::from(index) / 100.0,
                false,
                now,
            );
        }
        assert_eq!(runtime_tracker.snapshot(now, true).baseline_ms, None);

        let request = test_autotune_capture_request(
            1,
            AutotuneCapturePhase::LoadedMeasurement,
            MeasurementTopology::RawDownload,
            Some(SpeedtestDirection::Download),
        );
        assert_eq!(request.transport_baseline_us, Some(10_000));
        assert_eq!(
            transport_observation_kind(&request, 45.0, true, false).unwrap(),
            Some(AutotuneCaptureObservationKind::TransportSuccess {
                latency_us: None,
                delta_us: Some(35_000),
            })
        );
    }

    #[test]
    fn autotune_transport_hold_tolerates_bounded_bursty_download_dropouts() {
        use crate::operations::full_autotune::{AutotuneCapturePhase, MeasurementTopology};
        use crate::operations::protocol::SpeedtestDirection;

        let request = test_autotune_capture_request(
            1,
            AutotuneCapturePhase::LoadedMeasurement,
            MeasurementTopology::RawDownload,
            Some(SpeedtestDirection::Download),
        );
        let key = AutotuneTransportCaptureKey::new(&request, "route-a");
        let base = Instant::now();
        let hold = Duration::from_secs(3);
        let dropout = Duration::from_millis(600);
        let mut control = AutotuneTransportControl::default();

        for tick in 0..=20 {
            let phase = if tick % 5 == 4 {
                Some((false, false))
            } else {
                Some((true, false))
            };
            control.observe(
                key.clone(),
                phase,
                (true, false),
                base + Duration::from_millis(tick * 200),
                dropout,
                Duration::from_secs(10),
            );
        }
        assert_eq!(
            control
                .ready_phase(&request, &key, base + Duration::from_secs(4), hold, dropout,)
                .unwrap(),
            ((true, false), true),
            "short raw TCP gaps must not restart the whole three-second hold"
        );
    }

    #[test]
    fn autotune_transport_hold_tolerates_a_short_missing_rate_sample() {
        use crate::operations::full_autotune::{AutotuneCapturePhase, MeasurementTopology};
        use crate::operations::protocol::SpeedtestDirection;

        let request = test_autotune_capture_request(
            13,
            AutotuneCapturePhase::LoadedMeasurement,
            MeasurementTopology::ShapedBoth,
            Some(SpeedtestDirection::Download),
        );
        let key = AutotuneTransportCaptureKey::new(&request, "route-a");
        let base = Instant::now();
        let hold = Duration::from_secs(3);
        let dropout = Duration::from_millis(600);
        let mut control = AutotuneTransportControl::default();

        for tick in 0..=20 {
            let phase = if tick == 14 {
                None
            } else {
                Some((true, false))
            };
            control.observe(
                key.clone(),
                phase,
                (true, false),
                base + Duration::from_millis(tick * 200),
                dropout,
                Duration::from_secs(10),
            );
        }
        assert_eq!(
            control
                .ready_phase(&request, &key, base + Duration::from_secs(4), hold, dropout)
                .unwrap(),
            ((true, false), true),
            "a missing rate sample shorter than dropout must not restart the capture hold"
        );
    }

    #[test]
    fn autotune_transport_hold_survives_a_slow_reattest_without_synthetic_idle() {
        use crate::operations::full_autotune::{AutotuneCapturePhase, MeasurementTopology};
        use crate::operations::protocol::SpeedtestDirection;

        let request = test_autotune_capture_request(
            14,
            AutotuneCapturePhase::LoadedMeasurement,
            MeasurementTopology::ShapedBoth,
            Some(SpeedtestDirection::Download),
        );
        let key = AutotuneTransportCaptureKey::new(&request, "route-a");
        let base = Instant::now();
        let hold = Duration::from_secs(3);
        let dropout = Duration::from_millis(600);
        let mut control = AutotuneTransportControl::default();

        control.observe(
            key.clone(),
            Some((true, false)),
            (true, false),
            base,
            dropout,
            Duration::from_secs(10),
        );
        // A synchronous identity/SQM re-attestation can keep the main loop
        // busy for longer than the hold.  No observation during that interval
        // is not evidence of idle traffic; the first post-attestation sample
        // remains explicitly loaded and identity-bound.
        let after_reattest = base + Duration::from_secs(4);
        control.observe(
            key.clone(),
            Some((true, false)),
            (true, false),
            after_reattest,
            dropout,
            Duration::from_secs(10),
        );
        assert_eq!(
            control
                .ready_phase(&request, &key, after_reattest, hold, dropout)
                .unwrap(),
            ((true, false), true)
        );

        control.observe(
            key.clone(),
            None,
            (true, false),
            after_reattest + Duration::from_millis(1),
            dropout,
            Duration::from_secs(10),
        );
        assert_eq!(
            control
                .ready_phase(
                    &request,
                    &key,
                    after_reattest + Duration::from_secs(1),
                    hold,
                    dropout,
                )
                .unwrap(),
            ((true, false), false),
            "a real missing rate observation must still fail closed"
        );
    }

    #[test]
    fn autotune_transport_pair_requires_a_simultaneous_bidirectional_hold() {
        use crate::operations::full_autotune::{AutotuneCapturePhase, MeasurementTopology};
        use crate::operations::protocol::SpeedtestDirection;

        let request = test_autotune_capture_request(
            12,
            AutotuneCapturePhase::LoadedMeasurement,
            MeasurementTopology::ShapedBoth,
            Some(SpeedtestDirection::Both),
        );
        assert_eq!(
            AutotuneTransportControl::expected_phase(&request).unwrap(),
            (true, true)
        );
        let key = AutotuneTransportCaptureKey::new(&request, "route-a");
        let base = Instant::now();
        let hold = Duration::from_secs(3);
        let dropout = Duration::from_millis(600);
        let mut control = AutotuneTransportControl::default();
        for tick in 0..=20 {
            control.observe(
                key.clone(),
                Some((true, true)),
                (true, true),
                base + Duration::from_millis(tick * 200),
                dropout,
                Duration::from_secs(10),
            );
        }
        assert_eq!(
            control
                .ready_phase(&request, &key, base + Duration::from_secs(4), hold, dropout)
                .unwrap(),
            ((true, true), true)
        );

        let mut one_sided = AutotuneTransportControl::default();
        for tick in 0..=20 {
            one_sided.observe(
                key.clone(),
                Some((true, false)),
                (true, true),
                base + Duration::from_millis(tick * 200),
                dropout,
                Duration::from_secs(10),
            );
        }
        assert_eq!(
            one_sided
                .ready_phase(&request, &key, base + Duration::from_secs(4), hold, dropout)
                .unwrap(),
            ((true, true), false),
            "pair confirmation must not accept a one-sided loaded interval"
        );
    }

    #[test]
    fn autotune_transport_hold_tolerates_bounded_adjacent_phase_bursts() {
        use crate::operations::full_autotune::{AutotuneCapturePhase, MeasurementTopology};
        use crate::operations::protocol::SpeedtestDirection;

        let request = test_autotune_capture_request(
            7,
            AutotuneCapturePhase::LoadedMeasurement,
            MeasurementTopology::RawDownload,
            Some(SpeedtestDirection::Download),
        );
        let key = AutotuneTransportCaptureKey::new(&request, "route-a");
        let base = Instant::now();
        let hold = Duration::from_secs(3);
        let dropout = Duration::from_millis(600);
        let mut control = AutotuneTransportControl::default();

        for tick in 0..=20 {
            let phase = if tick % 5 == 4 {
                Some((true, true))
            } else {
                Some((true, false))
            };
            control.observe(
                key.clone(),
                phase,
                (true, false),
                base + Duration::from_millis(tick * 200),
                dropout,
                Duration::from_secs(10),
            );
        }
        assert_eq!(
            control
                .ready_phase(&request, &key, base + Duration::from_secs(4), hold, dropout,)
                .unwrap(),
            ((true, false), true),
            "short reverse-direction bursts must not restart the expected-direction hold"
        );
    }

    #[test]
    fn autotune_transport_hold_resets_after_adjacent_phase_dropout_bound() {
        use crate::operations::full_autotune::{AutotuneCapturePhase, MeasurementTopology};
        use crate::operations::protocol::SpeedtestDirection;

        let request = test_autotune_capture_request(
            8,
            AutotuneCapturePhase::LoadedMeasurement,
            MeasurementTopology::RawDownload,
            Some(SpeedtestDirection::Download),
        );
        let key = AutotuneTransportCaptureKey::new(&request, "route-a");
        let base = Instant::now();
        let hold = Duration::from_secs(3);
        let dropout = Duration::from_millis(600);
        let mut control = AutotuneTransportControl::default();

        for tick in 0..=24 {
            let phase = if (6..=10).contains(&tick) {
                Some((true, true))
            } else {
                Some((true, false))
            };
            control.observe(
                key.clone(),
                phase,
                (true, false),
                base + Duration::from_millis(tick * 200),
                dropout,
                Duration::from_secs(10),
            );
        }
        assert_eq!(
            control
                .ready_phase(
                    &request,
                    &key,
                    base + Duration::from_millis(4_800),
                    hold,
                    dropout,
                )
                .unwrap(),
            ((true, false), false),
            "a sustained adjacent phase must force a fresh expected-direction hold"
        );
    }

    #[test]
    fn autotune_transport_capture_cannot_bypass_control_with_rating_load() {
        assert!(!transport_probe_control_allows_start(
            true, true, false, true
        ));
        assert!(transport_probe_control_allows_start(
            true, true, true, false
        ));

        // Preserve the independent normal-rating fallback outside a native
        // capture: its smoothed loaded phase may legitimately keep probing
        // while the instantaneous control hold is being re-established.
        assert!(transport_probe_control_allows_start(
            false, true, false, true
        ));
        assert!(!transport_probe_control_allows_start(
            false, true, false, false
        ));
        assert!(transport_probe_control_allows_start(
            false, false, false, false
        ));
    }

    #[test]
    fn autotune_transport_hold_resets_after_dropout_bound() {
        use crate::operations::full_autotune::{AutotuneCapturePhase, MeasurementTopology};
        use crate::operations::protocol::SpeedtestDirection;

        let request = test_autotune_capture_request(
            2,
            AutotuneCapturePhase::LoadedMeasurement,
            MeasurementTopology::RawDownload,
            Some(SpeedtestDirection::Download),
        );
        let key = AutotuneTransportCaptureKey::new(&request, "route-a");
        let base = Instant::now();
        let hold = Duration::from_secs(3);
        let dropout = Duration::from_millis(600);
        let mut control = AutotuneTransportControl::default();

        for tick in 0..=5 {
            control.observe(
                key.clone(),
                Some((true, false)),
                (true, false),
                base + Duration::from_millis(tick * 200),
                dropout,
                Duration::from_secs(10),
            );
        }
        for tick in 6..=9 {
            control.observe(
                key.clone(),
                None,
                (true, false),
                base + Duration::from_millis(tick * 200),
                dropout,
                Duration::from_secs(10),
            );
        }
        for tick in 10..=24 {
            control.observe(
                key.clone(),
                Some((true, false)),
                (true, false),
                base + Duration::from_millis(tick * 200),
                dropout,
                Duration::from_secs(10),
            );
        }
        assert_eq!(
            control
                .ready_phase(
                    &request,
                    &key,
                    base + Duration::from_millis(4_800),
                    hold,
                    dropout,
                )
                .unwrap(),
            ((true, false), false),
            "a long counter gap must force a fresh hold"
        );
        control.observe(
            key.clone(),
            Some((true, false)),
            (true, false),
            base + Duration::from_secs(5),
            dropout,
            Duration::from_secs(10),
        );
        assert_eq!(
            control
                .ready_phase(&request, &key, base + Duration::from_secs(5), hold, dropout,)
                .unwrap(),
            ((true, false), true)
        );
    }

    #[test]
    fn autotune_transport_attests_the_loaded_flight_not_the_completion_tick() {
        use crate::operations::full_autotune::{AutotuneCapturePhase, MeasurementTopology};
        use crate::operations::protocol::SpeedtestDirection;

        let request = test_autotune_capture_request(
            3,
            AutotuneCapturePhase::LoadedMeasurement,
            MeasurementTopology::RawDownload,
            Some(SpeedtestDirection::Download),
        );
        let key = AutotuneTransportCaptureKey::new(&request, "route-a");
        let base = Instant::now();
        let dropout = Duration::from_millis(600);
        let mut control = AutotuneTransportControl::default();
        control.observe(
            key.clone(),
            Some((true, false)),
            (true, false),
            base,
            dropout,
            Duration::from_secs(10),
        );
        for (millis, phase) in [
            (200, Some((true, false))),
            (400, Some((true, false))),
            (600, Some((false, false))),
            (800, Some((true, false))),
            (1_000, Some((false, false))),
        ] {
            control.observe(
                key.clone(),
                phase,
                (true, false),
                base + Duration::from_millis(millis),
                dropout,
                Duration::from_secs(10),
            );
        }
        let flight = AutotuneTransportFlight {
            probe_id: 9,
            key,
            expected_phase: (true, false),
            control_valid: true,
            submitted_at: base,
            physical_delta_required: false,
        };
        let result = test_transport_result(
            &request,
            9,
            base + Duration::from_millis(50),
            base + Duration::from_millis(950),
            (true, false),
        );
        assert!(
            control.attest(&flight, &result, dropout),
            "one completion-adjacent idle tick inside the dropout bound must not erase a loaded flight"
        );
        assert_eq!(result.latency_ms, Some(42.0));
    }

    #[test]
    fn autotune_transport_rejects_idle_flight_and_identity_rotation() {
        use crate::operations::full_autotune::{AutotuneCapturePhase, MeasurementTopology};
        use crate::operations::protocol::SpeedtestDirection;

        let loaded = test_autotune_capture_request(
            4,
            AutotuneCapturePhase::LoadedMeasurement,
            MeasurementTopology::RawDownload,
            Some(SpeedtestDirection::Download),
        );
        let idle = test_autotune_capture_request(
            5,
            AutotuneCapturePhase::IdleBaseline,
            MeasurementTopology::ShapedBoth,
            None,
        );
        let base = Instant::now();
        let dropout = Duration::from_millis(600);
        let key = AutotuneTransportCaptureKey::new(&loaded, "route-a");
        let mut control = AutotuneTransportControl::default();
        control.observe(
            key.clone(),
            Some((true, false)),
            (true, false),
            base,
            dropout,
            Duration::from_secs(10),
        );
        control.observe(
            key.clone(),
            Some((false, false)),
            (true, false),
            base + Duration::from_millis(200),
            dropout,
            Duration::from_secs(10),
        );
        control.observe(
            key.clone(),
            Some((false, false)),
            (true, false),
            base + Duration::from_millis(900),
            dropout,
            Duration::from_secs(10),
        );
        let flight = AutotuneTransportFlight {
            probe_id: 10,
            key: key.clone(),
            expected_phase: (true, false),
            control_valid: true,
            submitted_at: base,
            physical_delta_required: false,
        };
        let result = test_transport_result(
            &loaded,
            10,
            base + Duration::from_millis(50),
            base + Duration::from_millis(850),
            (true, false),
        );
        assert!(!control.attest(&flight, &result, dropout));

        let rotated_key = AutotuneTransportCaptureKey::new(&idle, "route-b");
        control.observe(
            rotated_key,
            Some((false, false)),
            (false, false),
            base + Duration::from_secs(1),
            dropout,
            Duration::from_secs(10),
        );
        assert!(
            !control.attest(&flight, &result, dropout),
            "capture or route rotation must invalidate an old flight"
        );
    }

    #[test]
    fn autotune_transport_idle_readiness_is_fresh_and_typed() {
        use crate::operations::full_autotune::{AutotuneCapturePhase, MeasurementTopology};

        let request = test_autotune_capture_request(
            19,
            AutotuneCapturePhase::IdleBaseline,
            MeasurementTopology::RawBoth,
            None,
        );
        let key = AutotuneTransportCaptureKey::new(&request, "route-a");
        let foreign = AutotuneTransportCaptureKey::new(&request, "route-b");
        let base = Instant::now();
        let dropout = Duration::from_millis(600);
        let mut control = AutotuneTransportControl::default();
        control.observe(
            key.clone(),
            Some((false, false)),
            (false, false),
            base,
            dropout,
            Duration::from_secs(10),
        );

        assert_eq!(
            control
                .ready_phase_diagnostic(
                    &request,
                    &key,
                    base + Duration::from_millis(600),
                    Duration::from_secs(3),
                    dropout,
                )
                .unwrap(),
            AutotuneTransportReadiness {
                phase: (false, false),
                ready: true,
                reason: "idle-phase-ready",
            }
        );
        assert_eq!(
            control
                .ready_phase_diagnostic(
                    &request,
                    &key,
                    base + Duration::from_millis(601),
                    Duration::from_secs(3),
                    dropout,
                )
                .unwrap()
                .reason,
            "phase-observation-stale"
        );
        assert_eq!(
            control
                .ready_phase_diagnostic(&request, &foreign, base, Duration::from_secs(3), dropout,)
                .unwrap()
                .reason,
            "capture-key-mismatch"
        );
    }

    #[test]
    fn autotune_transport_idle_baseline_requires_an_entirely_idle_flight() {
        use crate::operations::full_autotune::{AutotuneCapturePhase, MeasurementTopology};

        let request = test_autotune_capture_request(
            6,
            AutotuneCapturePhase::IdleBaseline,
            MeasurementTopology::ShapedBoth,
            None,
        );
        let key = AutotuneTransportCaptureKey::new(&request, "route-a");
        let base = Instant::now();
        let dropout = Duration::from_millis(600);
        let mut control = AutotuneTransportControl::default();
        control.observe(
            key.clone(),
            Some((false, false)),
            (false, false),
            base,
            dropout,
            Duration::from_secs(10),
        );
        control.observe(
            key.clone(),
            Some((true, false)),
            (false, false),
            base + Duration::from_millis(400),
            dropout,
            Duration::from_secs(10),
        );
        control.observe(
            key.clone(),
            Some((false, false)),
            (false, false),
            base + Duration::from_millis(600),
            dropout,
            Duration::from_secs(10),
        );
        let flight = AutotuneTransportFlight {
            probe_id: 11,
            key,
            expected_phase: (false, false),
            control_valid: true,
            submitted_at: base,
            physical_delta_required: false,
        };
        let result = test_transport_result(
            &request,
            11,
            base + Duration::from_millis(100),
            base + Duration::from_millis(900),
            (false, false),
        );
        assert!(!control.attest(&flight, &result, dropout));
    }

    fn capture_request_for_counter_policy(
        sequence: u32,
        phase: crate::operations::full_autotune::AutotuneCapturePhase,
        topology: crate::operations::full_autotune::MeasurementTopology,
    ) -> crate::operations::full_autotune::AutotuneCaptureRequest {
        use crate::operations::full_autotune::AutotuneCapturePhase;
        use crate::operations::protocol::SpeedtestDirection;

        let direction = match (phase, topology) {
            (AutotuneCapturePhase::IdleBaseline, _) => None,
            (
                AutotuneCapturePhase::LoadedMeasurement,
                crate::operations::full_autotune::MeasurementTopology::RawDownload,
            ) => Some(SpeedtestDirection::Download),
            (
                AutotuneCapturePhase::LoadedMeasurement,
                crate::operations::full_autotune::MeasurementTopology::RawUpload,
            ) => Some(SpeedtestDirection::Upload),
            (AutotuneCapturePhase::LoadedMeasurement, _) => Some(SpeedtestDirection::Both),
        };
        let request = crate::operations::full_autotune::AutotuneCaptureRequest {
            capture_id: format!("{sequence:032x}"),
            job_id: "b".repeat(32),
            worker_run_id: "c".repeat(32),
            permit_id: "d".repeat(32),
            instance_name: "wan_sqm".to_string(),
            sequence,
            control_sequence: match phase {
                AutotuneCapturePhase::IdleBaseline => 0,
                AutotuneCapturePhase::LoadedMeasurement => sequence,
            },
            deadline_boot_ms: 100_000,
            phase,
            topology,
            direction,
            candidate_dl_kbps: topology.download_is_shaped().then_some(100_000),
            candidate_ul_kbps: topology.upload_is_shaped().then_some(20_000),
            load_reference_kbps: match phase {
                AutotuneCapturePhase::IdleBaseline => None,
                AutotuneCapturePhase::LoadedMeasurement => Some(100_000),
            },
            transport_baseline_us: match phase {
                AutotuneCapturePhase::IdleBaseline => None,
                AutotuneCapturePhase::LoadedMeasurement => Some(10_000),
            },
            route_fingerprint: "e".repeat(64),
            sqm_fingerprint: "f".repeat(64),
        };
        request.validate().unwrap();
        request
    }

    #[test]
    fn idle_baselines_use_managed_counters_before_any_speedtest_flight() {
        use crate::operations::full_autotune::{AutotuneCapturePhase, MeasurementTopology};

        for (sequence, topology) in [
            MeasurementTopology::ShapedBoth,
            MeasurementTopology::DownloadOnlyShaped,
            MeasurementTopology::UploadOnlyShaped,
        ]
        .into_iter()
        .enumerate()
        {
            let request = capture_request_for_counter_policy(
                sequence as u32 + 1,
                AutotuneCapturePhase::IdleBaseline,
                topology,
            );
            assert!(
                !autotune_capture_uses_identity_bound_speedtest_counters(&request),
                "{} idle baseline must use its topology-aware managed counters",
                topology.as_str()
            );
        }
    }

    #[test]
    fn every_loaded_capture_requires_job_owned_counters() {
        use crate::operations::full_autotune::{AutotuneCapturePhase, MeasurementTopology};

        for (sequence, topology) in [
            MeasurementTopology::ShapedBoth,
            MeasurementTopology::RawBoth,
            MeasurementTopology::RawDownload,
            MeasurementTopology::RawUpload,
            MeasurementTopology::DownloadOnlyShaped,
            MeasurementTopology::UploadOnlyShaped,
        ]
        .into_iter()
        .enumerate()
        {
            let request = capture_request_for_counter_policy(
                sequence as u32 + 10,
                AutotuneCapturePhase::LoadedMeasurement,
                topology,
            );
            assert_eq!(
                autotune_capture_uses_identity_bound_speedtest_counters(&request),
                true,
                "{} loaded counter policy",
                topology.as_str()
            );
        }
    }

    #[test]
    fn autotune_capture_rates_follow_each_runtime_topology_without_cross_direction_fallback() {
        use crate::operations::autotune_capture::bounded_directional_load_phase;
        use crate::operations::full_autotune::{
            AutotuneCapturePhase, AutotuneCaptureRequest, MeasurementTopology,
        };
        use crate::operations::protocol::SpeedtestDirection;

        let observed_at = Instant::now();
        let physical_observed_at = observed_at.checked_add(Duration::from_millis(1)).unwrap();
        let shaped_poison = RateSample {
            dl_kbps: f64::MAX,
            ul_kbps: f64::MAX,
            fresh: true,
            dl_observed_at: observed_at,
            ul_observed_at: observed_at,
        };
        let vanished_ifb_cache = RateSample {
            dl_kbps: 4_000.0,
            ul_kbps: 99_000.0,
            fresh: false,
            dl_observed_at: observed_at,
            ul_observed_at: observed_at,
        };
        let physical = RateSample {
            dl_kbps: 90_000.0,
            ul_kbps: 1_500.0,
            fresh: true,
            dl_observed_at: physical_observed_at,
            ul_observed_at: physical_observed_at,
        };
        for topology in [
            MeasurementTopology::ShapedBoth,
            MeasurementTopology::RawBoth,
            MeasurementTopology::RawDownload,
            MeasurementTopology::RawUpload,
            MeasurementTopology::DownloadOnlyShaped,
            MeasurementTopology::UploadOnlyShaped,
        ] {
            let selected = select_autotune_capture_rates(topology, Some(physical)).unwrap();
            assert_eq!(selected.dl_kbps, 90_000.0, "{} DL", topology.as_str());
            assert_eq!(selected.ul_kbps, 1_500.0, "{} UL", topology.as_str());
            assert!(selected.fresh, "{} freshness", topology.as_str());
            assert_ne!(selected.dl_kbps, shaped_poison.dl_kbps);
            assert_ne!(selected.ul_kbps, shaped_poison.ul_kbps);
        }

        let raw_download = AutotuneCaptureRequest {
            capture_id: "a".repeat(32),
            job_id: "b".repeat(32),
            worker_run_id: "c".repeat(32),
            permit_id: "d".repeat(32),
            instance_name: "wan_sqm".to_string(),
            sequence: 1,
            control_sequence: 1,
            deadline_boot_ms: 100_000,
            phase: AutotuneCapturePhase::LoadedMeasurement,
            topology: MeasurementTopology::RawDownload,
            direction: Some(SpeedtestDirection::Download),
            candidate_dl_kbps: None,
            candidate_ul_kbps: Some(723_400),
            load_reference_kbps: Some(900_000),
            transport_baseline_us: Some(10_000),
            route_fingerprint: "e".repeat(64),
            sqm_fingerprint: "f".repeat(64),
        };
        let selected =
            select_autotune_capture_rates(MeasurementTopology::RawDownload, Some(physical))
                .unwrap();
        assert_eq!(selected.dl_observed_at, physical_observed_at);
        assert_eq!(selected.ul_observed_at, physical_observed_at);
        assert_eq!(
            bounded_directional_load_phase(
                &raw_download,
                selected.dl_kbps,
                selected.ul_kbps,
                50_000.0,
                0.08,
            )
            .unwrap(),
            (true, false)
        );
        assert_eq!(
            bounded_directional_load_phase(
                &raw_download,
                vanished_ifb_cache.dl_kbps,
                vanished_ifb_cache.ul_kbps,
                50_000.0,
                0.08,
            )
            .unwrap(),
            (false, false),
            "the old IFB-only sample must remain an explicit fail-closed negative control"
        );
        assert_eq!(
            autotune_transport_control_phase(
                &raw_download,
                Some(selected),
                physical_observed_at,
                Duration::from_millis(600),
                50_000.0,
                0.08,
            )
            .unwrap(),
            Some((true, false)),
            "transport result re-attestation must use the same topology-aware counters"
        );
        assert_eq!(
            autotune_transport_control_phase(
                &raw_download,
                Some(RateSample {
                    fresh: false,
                    ..selected
                }),
                physical_observed_at,
                Duration::from_millis(600),
                50_000.0,
                0.08,
            )
            .unwrap(),
            Some((true, false)),
            "a recently cached counter sample remains valid for asynchronous re-attestation"
        );
        let expired_at = physical_observed_at
            .checked_sub(Duration::from_millis(601))
            .unwrap();
        assert_eq!(
            autotune_transport_control_phase(
                &raw_download,
                Some(RateSample {
                    fresh: false,
                    ul_observed_at: expired_at,
                    ..selected
                }),
                physical_observed_at,
                Duration::from_millis(600),
                50_000.0,
                0.08,
            )
            .unwrap(),
            None,
            "one expired hybrid direction must fail the whole re-attestation"
        );
        assert_eq!(
            autotune_transport_control_phase(
                &raw_download,
                Some(RateSample {
                    fresh: false,
                    dl_observed_at: expired_at,
                    ul_observed_at: expired_at,
                    ..selected
                }),
                physical_observed_at,
                Duration::from_millis(600),
                50_000.0,
                0.08,
            )
            .unwrap(),
            None,
            "an expired cached counter sample must fail closed"
        );
        assert_eq!(
            autotune_transport_control_phase(
                &raw_download,
                None,
                physical_observed_at,
                Duration::from_millis(600),
                50_000.0,
                0.08,
            )
            .unwrap(),
            None,
            "missing counter evidence must fail closed"
        );
    }

    #[test]
    fn autotune_capture_rate_selection_requires_identity_bound_counter_evidence() {
        use crate::operations::full_autotune::MeasurementTopology;

        let observed_at = Instant::now();
        let stale_controlled = RateSample {
            dl_kbps: 100_000.0,
            ul_kbps: 200_000.0,
            fresh: false,
            dl_observed_at: observed_at,
            ul_observed_at: observed_at,
        };
        assert!(select_autotune_capture_rates(MeasurementTopology::RawDownload, None).is_err());
        assert!(
            !select_autotune_capture_rates(MeasurementTopology::RawBoth, Some(stale_controlled))
                .unwrap()
                .fresh
        );
        assert!(
            !select_autotune_capture_rates(
                MeasurementTopology::RawDownload,
                Some(stale_controlled)
            )
            .unwrap()
            .fresh
        );
        assert!(select_autotune_capture_rates(MeasurementTopology::ShapedBoth, None).is_err());
    }

    #[test]
    fn autotune_rate_sample_recency_is_direction_symmetric() {
        let now = Instant::now();
        let max_age = Duration::from_millis(600);
        let recent = now.checked_sub(Duration::from_millis(600)).unwrap();
        let stale = now.checked_sub(Duration::from_millis(601)).unwrap();
        let sample = RateSample {
            dl_kbps: 90_000.0,
            ul_kbps: 1_500.0,
            fresh: false,
            dl_observed_at: recent,
            ul_observed_at: recent,
        };
        assert!(rate_sample_is_recent(sample, now, max_age));
        assert!(!rate_sample_is_recent(
            RateSample {
                dl_observed_at: stale,
                ..sample
            },
            now,
            max_age
        ));
        assert!(!rate_sample_is_recent(
            RateSample {
                ul_observed_at: stale,
                ..sample
            },
            now,
            max_age
        ));
    }

    #[test]
    fn speedtest_counter_monitor_is_exact_and_resets_fail_closed() {
        use crate::operations::speedtest::SpeedtestTrafficCounters;

        let mut monitor = SpeedtestCounterRateMonitor::new(200);
        let started = Instant::now();
        assert!(monitor
            .observe_counters(
                started,
                Some(SpeedtestTrafficCounters {
                    rx_bytes: 1_000,
                    tx_bytes: 2_000,
                })
            )
            .is_none());
        for quarter in 1..4 {
            assert!(monitor
                .observe_counters(
                    started + Duration::from_millis(quarter * 250),
                    Some(SpeedtestTrafficCounters {
                        rx_bytes: 1_000 + quarter * 25_000_000,
                        tx_bytes: 2_000 + quarter * 250_000,
                    }),
                )
                .is_none());
        }
        let loaded = monitor
            .observe_counters(
                started + Duration::from_millis(1_000),
                Some(SpeedtestTrafficCounters {
                    rx_bytes: 100_001_000,
                    tx_bytes: 1_002_000,
                }),
            )
            .unwrap();
        assert!((loaded.dl_kbps - 800_000.0).abs() < 0.1);
        assert!((loaded.ul_kbps - 8_000.0).abs() < 0.1);
        assert!(loaded.fresh);

        assert!(monitor
            .observe_counters(started + Duration::from_millis(1_250), None)
            .is_none());
        assert!(monitor.tracker.is_empty());
        assert!(monitor
            .observe_counters(
                started + Duration::from_millis(1_500),
                Some(SpeedtestTrafficCounters {
                    rx_bytes: 10,
                    tx_bytes: 20,
                })
            )
            .is_none());
        assert!(monitor
            .observe_counters(
                started + Duration::from_millis(1_750),
                Some(SpeedtestTrafficCounters {
                    rx_bytes: 9,
                    tx_bytes: 21,
                })
            )
            .is_none());
        assert!(monitor.tracker.cached_sample().is_none());
    }

    #[test]
    fn speedtest_counter_window_smooths_bursts_and_expires_true_idle() {
        use crate::operations::speedtest::SpeedtestTrafficCounters;

        let mut monitor = SpeedtestCounterRateMonitor::new(200);
        let started = Instant::now();
        let points = [
            (0, 0),
            (200, 0),
            (400, 40_000_000),
            (600, 40_000_000),
            (800, 80_000_000),
            (1_000, 100_000_000),
        ];
        let mut loaded = None;
        for (millis, rx_bytes) in points {
            loaded = monitor.observe_counters(
                started + Duration::from_millis(millis),
                Some(SpeedtestTrafficCounters {
                    rx_bytes,
                    tx_bytes: rx_bytes / 100,
                }),
            );
        }
        let loaded = loaded.expect("one-second burst window should produce a rate");
        assert!(loaded.dl_kbps > 20_000.0);

        let flat = SpeedtestTrafficCounters {
            rx_bytes: 100_000_000,
            tx_bytes: 1_000_000,
        };
        for millis in [1_200, 1_400, 1_600, 1_800] {
            assert!(monitor
                .observe_counters(started + Duration::from_millis(millis), Some(flat))
                .is_some_and(|sample| sample.dl_kbps > 0.0));
        }
        let idle = monitor
            .observe_counters(started + Duration::from_millis(2_000), Some(flat))
            .expect("bounded history should continue reporting an exact zero rate");
        assert_eq!(idle.dl_kbps, 0.0);
        assert_eq!(idle.ul_kbps, 0.0);
        assert!(monitor
            .observe_counters(started + Duration::from_millis(2_200), None)
            .is_none());
        assert!(monitor.tracker.is_empty());
    }

    #[test]
    fn rate_monitor_does_not_invent_zero_counters_when_a_source_is_missing() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cake-autorate-missing-rate-monitor-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let rx = root.join("rx_bytes");
        let tx = root.join("tx_bytes");
        fs::write(&rx, "0\n").unwrap();
        assert!(RateMonitor::new(rx.to_str().unwrap(), tx.to_str().unwrap(), 25).is_err());
        fs::write(&tx, "0\n").unwrap();
        let mut monitor = RateMonitor::new(rx.to_str().unwrap(), tx.to_str().unwrap(), 25).unwrap();
        fs::remove_file(&rx).unwrap();
        thread::sleep(Duration::from_millis(30));
        assert!(monitor.try_sample().is_err());
        let fallback = monitor.sample();
        assert!(!fallback.fresh);
        assert_eq!((fallback.dl_kbps, fallback.ul_kbps), (0.0, 0.0));
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
        for invalid in [
            f64::NAN,
            f64::INFINITY,
            -1.0,
            autotune::MAX_RATE_KBPS as f64 + 1.0,
        ] {
            assert!(validated_conservative_samples(&[1_000.0], Some(invalid)).is_err());
        }
    }

    #[test]
    fn autotune_cli_rejects_a_one_sided_measurement_base() {
        let args = [
            "--dl-samples",
            "1000,1100",
            "--ul-samples",
            "500,550",
            "--idle-median-ms",
            "10",
            "--idle-p95-ms",
            "15",
            "--idle-samples",
            "10",
            "--dl-measurement-base-kbps",
            "1000",
        ]
        .into_iter()
        .map(str::to_string);

        assert_eq!(
            run_autotune_proposal_cli(args),
            Err("download and upload measurement bases must be supplied together".to_string())
        );
    }

    #[test]
    fn conservative_background_does_not_reduce_isolated_speedtest_samples() {
        let samples = [780_162.0, 835_696.0];
        let validated = validated_conservative_samples(&samples, Some(250_000.0)).unwrap();

        assert_eq!(validated, samples);
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
