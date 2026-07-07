use std::collections::{HashMap, VecDeque};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static TERMINATE: AtomicBool = AtomicBool::new(false);

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
    dl_if: String,
    ul_if: String,
    rx_bytes_path: String,
    tx_bytes_path: String,
    adjust_dl_shaper_rate: bool,
    adjust_ul_shaper_rate: bool,
    min_dl_shaper_rate_kbps: f64,
    base_dl_shaper_rate_kbps: f64,
    max_dl_shaper_rate_kbps: f64,
    min_ul_shaper_rate_kbps: f64,
    base_ul_shaper_rate_kbps: f64,
    max_ul_shaper_rate_kbps: f64,
    connection_active_thr_kbps: f64,
    pinger_method: String,
    ping_extra_args: String,
    reflectors: Vec<String>,
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
    output_summary_stats: bool,
    output_load_stats: bool,
    output_reflector_stats: bool,
    output_cake_changes: bool,
    output_cpu_stats: bool,
    output_cpu_raw_stats: bool,
    log_to_file: bool,
    debug: bool,
    log_file_max_time_mins: u64,
    log_file_max_size_kb: u64,
    log_file_path_override: String,
    log_file_export_compress: bool,
    startup_wait_s: f64,
    if_up_check_interval_s: f64,
    monitor_cpu_usage_interval_ms: u64,
}

impl Config {
    fn defaults(instance: String) -> Self {
        Self {
            instance,
            enabled: false,
            dl_if: "ifb-wan".to_string(),
            ul_if: "wan".to_string(),
            rx_bytes_path: String::new(),
            tx_bytes_path: String::new(),
            adjust_dl_shaper_rate: true,
            adjust_ul_shaper_rate: true,
            min_dl_shaper_rate_kbps: 5000.0,
            base_dl_shaper_rate_kbps: 20000.0,
            max_dl_shaper_rate_kbps: 80000.0,
            min_ul_shaper_rate_kbps: 5000.0,
            base_ul_shaper_rate_kbps: 20000.0,
            max_ul_shaper_rate_kbps: 35000.0,
            connection_active_thr_kbps: 2000.0,
            pinger_method: "fping".to_string(),
            ping_extra_args: String::new(),
            reflectors: vec![
                "1.1.1.1".to_string(),
                "1.0.0.1".to_string(),
                "8.8.8.8".to_string(),
                "8.8.4.4".to_string(),
                "9.9.9.9".to_string(),
                "9.9.9.10".to_string(),
            ],
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
            output_summary_stats: true,
            output_load_stats: false,
            output_reflector_stats: false,
            output_cake_changes: false,
            output_cpu_stats: false,
            output_cpu_raw_stats: false,
            log_to_file: true,
            debug: true,
            log_file_max_time_mins: 10,
            log_file_max_size_kb: 2000,
            log_file_path_override: String::new(),
            log_file_export_compress: true,
            startup_wait_s: 0.0,
            if_up_check_interval_s: 10.0,
            monitor_cpu_usage_interval_ms: 2000,
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
        set_string(&single, "dl_if", &mut cfg.dl_if);
        set_string(&single, "ul_if", &mut cfg.ul_if);
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
            }
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
        set_f64(
            &single,
            "connection_active_thr_kbps",
            &mut cfg.connection_active_thr_kbps,
        )?;
        set_string(&single, "pinger_method", &mut cfg.pinger_method);
        set_string(&single, "ping_extra_args", &mut cfg.ping_extra_args);
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
        set_bool(&single, "log_to_file", &mut cfg.log_to_file)?;
        set_bool(&single, "debug", &mut cfg.debug)?;
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
        cfg.load_reflectors_url();
        cfg.deduplicate_reflectors();
        if cfg.randomize_reflectors {
            randomize_reflectors(&mut cfg.reflectors);
        }

        cfg.normalize_paths();
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
                let mut reflectors =
                    parse_reflector_candidates(&data, self.reflectors_url_skip_lines);
                if reflectors.is_empty() {
                    eprintln!(
                        "WARNING: reflectors_url {} returned no usable reflectors; using configured list",
                        self.reflectors_url
                    );
                } else {
                    reflectors.extend(configured_reflectors);
                    self.reflectors = reflectors;
                }
            }
            Err(e) => eprintln!(
                "WARNING: failed to fetch reflectors_url {}: {e}; using configured list",
                self.reflectors_url
            ),
        }
    }

    fn deduplicate_reflectors(&mut self) {
        let mut seen: Vec<String> = Vec::new();
        self.reflectors.retain(|reflector| {
            if seen.iter().any(|value| value == reflector) {
                false
            } else {
                seen.push(reflector.clone());
                true
            }
        });
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

    fn validate(&self) -> Result<(), String> {
        if self.pinger_method != "fping"
            && self.pinger_method != "fping-ts"
            && self.pinger_method != "tsping"
            && self.pinger_method != "ping"
        {
            return Err(format!(
                "pinger_method={} is configured, but this Rust package currently supports fping, fping-ts, tsping, and ping",
                self.pinger_method
            ));
        }
        if self.reflectors.is_empty() {
            return Err("at least one reflector is required".to_string());
        }
        if self
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
        if self.pinger_method == "ping" && self.no_pingers > 1 {
            return Err("pinger_method=ping supports only no_pingers=1".to_string());
        }
        if self.no_pingers > self.reflectors.len() {
            return Err("no_pingers cannot exceed reflector count".to_string());
        }
        if self.bufferbloat_detection_thr > self.bufferbloat_detection_window {
            return Err(
                "bufferbloat_detection_thr cannot exceed bufferbloat_detection_window".to_string(),
            );
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

#[derive(Debug)]
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
        if self.offences.len() == self.offences.capacity() {
            if self.offences.pop_front().unwrap_or(false) {
                self.offence_sum = self.offence_sum.saturating_sub(1);
            }
        }

        self.offences.push_back(offence);
        if offence {
            self.offence_sum = self.offence_sum.saturating_add(1);
        }
    }
}

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
    prev_rx: u64,
    prev_tx: u64,
    last: Instant,
}

impl RateMonitor {
    fn new(rx_path: &str, tx_path: &str) -> io::Result<Self> {
        Ok(Self {
            rx_path: PathBuf::from(rx_path),
            tx_path: PathBuf::from(tx_path),
            prev_rx: read_u64_file(rx_path).unwrap_or(0),
            prev_tx: read_u64_file(tx_path).unwrap_or(0),
            last: Instant::now(),
        })
    }

    fn sample(&mut self) -> (f64, f64) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64().max(0.001);
        let rx = read_u64_file(&self.rx_path).unwrap_or(self.prev_rx);
        let tx = read_u64_file(&self.tx_path).unwrap_or(self.prev_tx);
        let dl = rx.saturating_sub(self.prev_rx) as f64 * 8.0 / elapsed / 1000.0;
        let ul = tx.saturating_sub(self.prev_tx) as f64 * 8.0 / elapsed / 1000.0;
        self.prev_rx = rx;
        self.prev_tx = tx;
        self.last = now;
        (dl, ul)
    }
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

struct LogFile {
    path: PathBuf,
    file: File,
    opened_at: Instant,
    bytes_written: u64,
}

impl LogFile {
    fn open(path: PathBuf) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let bytes_written = fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;

        Ok(Self {
            path,
            file,
            opened_at: Instant::now(),
            bytes_written,
        })
    }

    fn write_line(
        &mut self,
        line: &str,
        max_age: Duration,
        max_size_bytes: u64,
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
        Ok(())
    }

    fn rotate(&mut self, compress: bool) -> io::Result<()> {
        let _ = self.file.flush();

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

        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.opened_at = Instant::now();
        self.bytes_written = 0;
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
    last_set_dl: u64,
    last_set_ul: u64,
    last_bb_dl: Instant,
    last_bb_ul: Instant,
    last_decay_dl: Instant,
    last_decay_ul: Instant,
    last_cpu_sample: Instant,
    cpu_total_percent: Option<f64>,
    cpu_core_percentages: Vec<f64>,
    started_at: f64,
}

impl Controller {
    fn new(cfg: Config) -> Result<Self, String> {
        ensure_run_dir(&cfg.run_dir())
            .map_err(|e| format!("failed to create run directory: {e}"))?;
        wait_for_path(&cfg.rx_bytes_path, cfg.if_up_check_interval_s)?;
        wait_for_path(&cfg.tx_bytes_path, cfg.if_up_check_interval_s)?;

        let log = if cfg.log_to_file {
            Some(LogFile::open(cfg.log_path()).map_err(|e| {
                format!("failed to open log file {}: {e}", cfg.log_path().display())
            })?)
        } else {
            None
        };

        let rate_monitor = RateMonitor::new(&cfg.rx_bytes_path, &cfg.tx_bytes_path)
            .map_err(|e| format!("failed to create rate monitor: {e}"))?;
        let cpu_monitor = if cfg.output_cpu_stats || cfg.output_cpu_raw_stats {
            match CpuMonitor::new() {
                Ok(monitor) => Some(monitor),
                Err(e) => {
                    eprintln!("WARNING: failed to initialize CPU monitor: {e}");
                    None
                }
            }
        } else {
            None
        };
        let now = Instant::now();

        Ok(Self {
            shaper_dl: cfg.base_dl_shaper_rate_kbps,
            shaper_ul: cfg.base_ul_shaper_rate_kbps,
            last_set_dl: 0,
            last_set_ul: 0,
            last_bb_dl: now,
            last_bb_ul: now,
            last_decay_dl: now,
            last_decay_ul: now,
            last_cpu_sample: now,
            cpu_total_percent: None,
            cpu_core_percentages: Vec::new(),
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

    fn on_sample(&mut self, sample: Sample) {
        let now = Instant::now();
        let (dl_rate, ul_rate) = self.rate_monitor.sample();
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

        push_window(
            &mut self.dl_delays,
            dl_delta_us > self.cfg.dl_owd_delta_delay_thr_ms * 1000.0,
        );
        push_window(
            &mut self.ul_delays,
            ul_delta_us > self.cfg.ul_owd_delta_delay_thr_ms * 1000.0,
        );
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

        self.update_direction(true, dl_kind, dl_bb, avg_dl_delta, now);
        self.update_direction(false, ul_kind, ul_bb, avg_ul_delta, now);
        self.clamp_rates();
        self.apply_shaper("dl");
        self.apply_shaper("ul");

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
        );
    }

    fn maybe_sample_cpu(&mut self) {
        if !self.cfg.output_cpu_stats && !self.cfg.output_cpu_raw_stats {
            return;
        }

        let interval = Duration::from_millis(self.cfg.monitor_cpu_usage_interval_ms.max(1));
        if self.last_cpu_sample.elapsed() < interval {
            return;
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
            return;
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
    }

    fn update_direction(
        &mut self,
        is_dl: bool,
        kind: LoadKind,
        bufferbloat: bool,
        avg_delta_us: f64,
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
        let delay_thr_us = if is_dl {
            self.cfg.dl_owd_delta_delay_thr_ms * 1000.0
        } else {
            self.cfg.ul_owd_delta_delay_thr_ms * 1000.0
        };
        let up_thr_us = if is_dl {
            self.cfg.dl_avg_owd_delta_max_adjust_up_thr_ms * 1000.0
        } else {
            self.cfg.ul_avg_owd_delta_max_adjust_up_thr_ms * 1000.0
        };
        let down_thr_us = if is_dl {
            self.cfg.dl_avg_owd_delta_max_adjust_down_thr_ms * 1000.0
        } else {
            self.cfg.ul_avg_owd_delta_max_adjust_down_thr_ms * 1000.0
        };
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
        } else if matches!(kind, LoadKind::High) && bb_ready {
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

    fn clamp_rates(&mut self) {
        self.shaper_dl = self
            .shaper_dl
            .max(self.cfg.min_dl_shaper_rate_kbps)
            .min(self.cfg.max_dl_shaper_rate_kbps);
        self.shaper_ul = self
            .shaper_ul
            .max(self.cfg.min_ul_shaper_rate_kbps)
            .min(self.cfg.max_ul_shaper_rate_kbps);
    }

    fn apply_shaper(&mut self, direction: &str) {
        let (interface, adjust, rate, last) = if direction == "dl" {
            (
                self.cfg.dl_if.clone(),
                self.cfg.adjust_dl_shaper_rate,
                self.shaper_dl,
                &mut self.last_set_dl,
            )
        } else {
            (
                self.cfg.ul_if.clone(),
                self.cfg.adjust_ul_shaper_rate,
                self.shaper_ul,
                &mut self.last_set_ul,
            )
        };
        let rounded = rate.round().max(1.0) as u64;
        if rounded == *last {
            return;
        }
        *last = rounded;

        if self.cfg.output_cake_changes {
            self.log(
                "SHAPER",
                &format!("tc qdisc change root dev {interface} cake bandwidth {rounded}Kbit"),
            );
        }

        if !adjust {
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
            Ok(s) if s.success() => {}
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
    ) -> io::Result<()> {
        let path = self.cfg.run_dir().join("status.json");
        let tmp = self.cfg.run_dir().join("status.json.tmp");
        let mut file = File::create(&tmp)?;
        writeln!(
            file,
            "{{\"instance\":\"{}\",\"version\":\"0.1.0\",\"started_at\":{:.6},\"updated_at\":{:.6},\"dl_if\":\"{}\",\"ul_if\":\"{}\",\"reflector\":\"{}\",\"seq\":\"{}\",\"probe_timestamp\":{:.6},\"rtt_ms\":{:.3},\"dl_owd_us\":{:.1},\"ul_owd_us\":{:.1},\"dl_achieved_rate_kbps\":{:.1},\"ul_achieved_rate_kbps\":{:.1},\"dl_load_percent\":{:.1},\"ul_load_percent\":{:.1},\"dl_sum_delays\":{},\"ul_sum_delays\":{},\"dl_avg_owd_delta_us\":{:.1},\"ul_avg_owd_delta_us\":{:.1},\"cake_dl_rate_kbps\":{:.0},\"cake_ul_rate_kbps\":{:.0},\"cpu_total_percent\":{},\"cpu_core_percentages\":{}}}",
            json_escape(&self.cfg.instance),
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
            json_f64_or_null(self.cpu_total_percent, 1),
            json_f64_array(&self.cpu_core_percentages, 1)
        )?;
        fs::rename(tmp, path)
    }

    fn write_initial_status(&mut self) -> io::Result<()> {
        let sample = Sample {
            reflector: String::new(),
            seq: String::new(),
            timestamp: epoch_secs(),
            rtt_ms: 0.0,
            dl_owd_us: 0.0,
            ul_owd_us: 0.0,
            timestamped_owd: false,
        };
        self.write_status(0.0, 0.0, 0.0, 0.0, 0, 0, 0.0, 0.0, &sample)
    }

    fn log(&mut self, kind: &str, msg: &str) {
        if kind == "DEBUG" && !self.cfg.debug {
            return;
        }
        let line = format!("{kind}; {:.6}; {msg}", epoch_secs());
        if let Some(file) = &mut self.log {
            let max_age = Duration::from_secs(self.cfg.log_file_max_time_mins.saturating_mul(60));
            let max_size = self.cfg.log_file_max_size_kb.saturating_mul(1024);
            if let Err(e) =
                file.write_line(&line, max_age, max_size, self.cfg.log_file_export_compress)
            {
                eprintln!("failed to write log file: {e}");
            }
        } else {
            eprintln!("{line}");
        }
    }
}

fn run(cfg: Config, once: bool) -> Result<(), String> {
    if !cfg.enabled {
        println!("cake-autorate-rs instance '{}' is disabled", cfg.instance);
        return Ok(());
    }

    if cfg.startup_wait_s > 0.0 {
        std::thread::sleep(Duration::from_secs_f64(cfg.startup_wait_s));
    }

    let mut controller = Controller::new(cfg.clone())?;
    controller.start();
    controller
        .write_initial_status()
        .map_err(|e| format!("failed to write status: {e}"))?;

    if once {
        println!(
            "cake-autorate-rs wrote initial status for '{}'",
            cfg.instance
        );
        return Ok(());
    }

    let mut active_reflectors: Vec<String> = cfg
        .reflectors
        .iter()
        .take(cfg.no_pingers)
        .cloned()
        .collect();
    let mut health = ReflectorHealth::new(&cfg, &active_reflectors);
    let mut pinger = PingerRuntime::spawn(&cfg, &active_reflectors)?;

    while !TERMINATE.load(Ordering::SeqCst) {
        match pinger.lines.recv_timeout(health.timeout(&cfg)) {
            Ok(Ok(line)) => {
                if let Some(sample) = parse_sample_line(&cfg, &line, &pinger.ping_reflector) {
                    health.observe_sample(&cfg, &sample);
                    controller.on_sample(sample);
                }
            }
            Ok(Err(e)) => return Err(e),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                if TERMINATE.load(Ordering::SeqCst) {
                    break;
                }

                return Err(format!("{} output closed unexpectedly", cfg.pinger_method));
            }
        }

        if health.check(&cfg, &mut active_reflectors, &mut controller) {
            pinger.stop();
            pinger = PingerRuntime::spawn(&cfg, &active_reflectors)?;
        }
    }

    pinger.stop();
    Ok(())
}

struct PingerRuntime {
    child: Child,
    reader: Option<JoinHandle<()>>,
    lines: Receiver<Result<String, String>>,
    ping_reflector: String,
}

impl PingerRuntime {
    fn spawn(cfg: &Config, active_reflectors: &[String]) -> Result<Self, String> {
        let mut child = spawn_pinger(cfg, active_reflectors)?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("failed to capture {} stdout", cfg.pinger_method))?;
        let (tx, lines) = mpsc::channel();
        let method = cfg.pinger_method.clone();
        let reader = thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        if tx.send(Ok(line)).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(format!("failed to read {method} output: {e}")));
                        break;
                    }
                }
            }
        });

        Ok(Self {
            child,
            reader: Some(reader),
            lines,
            ping_reflector: active_reflectors.first().cloned().unwrap_or_default(),
        })
    }

    fn stop(&mut self) {
        stop_child(&mut self.child);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn spawn_pinger(cfg: &Config, active_reflectors: &[String]) -> Result<Child, String> {
    match cfg.pinger_method.as_str() {
        "fping" => spawn_fping(cfg, active_reflectors, false),
        "fping-ts" => spawn_fping(cfg, active_reflectors, true),
        "tsping" => spawn_tsping(cfg, active_reflectors),
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

    let mut cmd = Command::new("fping");
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

    let mut cmd = Command::new("tsping");
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

fn spawn_ping(cfg: &Config, active_reflectors: &[String]) -> Result<Child, String> {
    let target = active_reflectors
        .first()
        .ok_or_else(|| "at least one reflector is required".to_string())?;
    let interval_s = cfg.reflector_ping_interval_s.ceil().max(1.0) as u64;

    let mut cmd = Command::new("ping");
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

fn stop_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn parse_sample_line(cfg: &Config, line: &str, ping_reflector: &str) -> Option<Sample> {
    match cfg.pinger_method.as_str() {
        "ping" => parse_ping_line(line, ping_reflector),
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

    if tokens.len() < 17 || !tokens.iter().any(|token| *token == "timestamps:") {
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

fn print_usage() {
    eprintln!("usage: cake-autorated [--instance NAME] [--once] [--dump-config]");
}

fn main() {
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
        next_spare_reflector, parse_fping_ts_line, parse_reflector_candidates, parse_tsping_line,
        parse_uci_values, ReflectorState,
    };
    use std::time::Instant;

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
}
