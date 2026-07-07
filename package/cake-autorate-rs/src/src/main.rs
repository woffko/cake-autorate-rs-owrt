use std::collections::{HashMap, VecDeque};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
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
    reflectors: Vec<String>,
    reflectors_url: String,
    reflectors_url_skip_lines: usize,
    randomize_reflectors: bool,
    no_pingers: usize,
    reflector_ping_interval_s: f64,
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
    output_cake_changes: bool,
    log_to_file: bool,
    debug: bool,
    log_file_path_override: String,
    startup_wait_s: f64,
    if_up_check_interval_s: f64,
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
            no_pingers: 6,
            reflector_ping_interval_s: 0.3,
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
            output_cake_changes: false,
            log_to_file: true,
            debug: true,
            log_file_path_override: String::new(),
            startup_wait_s: 0.0,
            if_up_check_interval_s: 10.0,
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
        set_usize(&single, "no_pingers", &mut cfg.no_pingers)?;
        set_f64(
            &single,
            "reflector_ping_interval_s",
            &mut cfg.reflector_ping_interval_s,
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
        set_bool(&single, "output_cake_changes", &mut cfg.output_cake_changes)?;
        set_bool(&single, "log_to_file", &mut cfg.log_to_file)?;
        set_bool(&single, "debug", &mut cfg.debug)?;
        set_string(
            &single,
            "log_file_path_override",
            &mut cfg.log_file_path_override,
        );
        set_f64(&single, "startup_wait_s", &mut cfg.startup_wait_s)?;
        set_f64(
            &single,
            "if_up_check_interval_s",
            &mut cfg.if_up_check_interval_s,
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
        if self.pinger_method != "fping" {
            return Err(format!(
                "pinger_method={} is configured, but this Rust MVP currently supports only fping",
                self.pinger_method
            ));
        }
        if self.reflectors.is_empty() {
            return Err("at least one reflector is required".to_string());
        }
        if self.no_pingers == 0 {
            return Err("no_pingers must be greater than zero".to_string());
        }
        if self.no_pingers > self.reflectors.len() {
            return Err("no_pingers cannot exceed reflector count".to_string());
        }
        if self.bufferbloat_detection_thr > self.bufferbloat_detection_window {
            return Err(
                "bufferbloat_detection_thr cannot exceed bufferbloat_detection_window".to_string(),
            );
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

#[derive(Debug)]
struct Sample {
    reflector: String,
    seq: String,
    timestamp: f64,
    rtt_ms: f64,
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

struct Controller {
    cfg: Config,
    log: Option<File>,
    rate_monitor: RateMonitor,
    baseline_us: HashMap<String, f64>,
    ewma_us: HashMap<String, f64>,
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
    started_at: f64,
}

impl Controller {
    fn new(cfg: Config) -> Result<Self, String> {
        ensure_run_dir(&cfg.run_dir())
            .map_err(|e| format!("failed to create run directory: {e}"))?;
        wait_for_path(&cfg.rx_bytes_path, cfg.if_up_check_interval_s)?;
        wait_for_path(&cfg.tx_bytes_path, cfg.if_up_check_interval_s)?;

        let log = if cfg.log_to_file {
            let path = cfg.log_path();
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| {
                    format!("failed to create log directory {}: {e}", parent.display())
                })?;
            }
            Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .map_err(|e| format!("failed to open log file {}: {e}", path.display()))?,
            )
        } else {
            None
        };

        let rate_monitor = RateMonitor::new(&cfg.rx_bytes_path, &cfg.tx_bytes_path)
            .map_err(|e| format!("failed to create rate monitor: {e}"))?;
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
            baseline_us: HashMap::new(),
            ewma_us: HashMap::new(),
            dl_delays: filled_bool_window(cfg.bufferbloat_detection_window),
            ul_delays: filled_bool_window(cfg.bufferbloat_detection_window),
            dl_delta_us: filled_f64_window(cfg.bufferbloat_detection_window),
            ul_delta_us: filled_f64_window(cfg.bufferbloat_detection_window),
            started_at: epoch_secs(),
            cfg,
            log,
            rate_monitor,
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

        let owd_us = sample.rtt_ms * 500.0;
        let baseline = self
            .baseline_us
            .entry(sample.reflector.clone())
            .or_insert(100_000.0);
        let alpha = if owd_us >= *baseline {
            self.cfg.alpha_baseline_increase
        } else {
            self.cfg.alpha_baseline_decrease
        };
        *baseline = alpha * owd_us + (1.0 - alpha) * *baseline;
        let delta_us = owd_us - *baseline;

        if dl_load_pct < self.cfg.high_load_thr * 100.0
            && ul_load_pct < self.cfg.high_load_thr * 100.0
        {
            let ewma = self.ewma_us.entry(sample.reflector.clone()).or_insert(0.0);
            *ewma =
                self.cfg.alpha_delta_ewma * delta_us + (1.0 - self.cfg.alpha_delta_ewma) * *ewma;
        }

        push_window(
            &mut self.dl_delays,
            delta_us > self.cfg.dl_owd_delta_delay_thr_ms * 1000.0,
        );
        push_window(
            &mut self.ul_delays,
            delta_us > self.cfg.ul_owd_delta_delay_thr_ms * 1000.0,
        );
        push_window(&mut self.dl_delta_us, delta_us);
        push_window(&mut self.ul_delta_us, delta_us);

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
            "{{\"instance\":\"{}\",\"version\":\"0.1.0\",\"started_at\":{:.6},\"updated_at\":{:.6},\"dl_if\":\"{}\",\"ul_if\":\"{}\",\"reflector\":\"{}\",\"seq\":\"{}\",\"probe_timestamp\":{:.6},\"rtt_ms\":{:.3},\"dl_achieved_rate_kbps\":{:.1},\"ul_achieved_rate_kbps\":{:.1},\"dl_load_percent\":{:.1},\"ul_load_percent\":{:.1},\"dl_sum_delays\":{},\"ul_sum_delays\":{},\"dl_avg_owd_delta_us\":{:.1},\"ul_avg_owd_delta_us\":{:.1},\"cake_dl_rate_kbps\":{:.0},\"cake_ul_rate_kbps\":{:.0}}}",
            json_escape(&self.cfg.instance),
            self.started_at,
            epoch_secs(),
            json_escape(&self.cfg.dl_if),
            json_escape(&self.cfg.ul_if),
            json_escape(&sample.reflector),
            json_escape(&sample.seq),
            sample.timestamp,
            sample.rtt_ms,
            dl_rate,
            ul_rate,
            dl_load_pct,
            ul_load_pct,
            dl_delay_count,
            ul_delay_count,
            avg_dl_delta,
            avg_ul_delta,
            self.shaper_dl,
            self.shaper_ul
        )?;
        fs::rename(tmp, path)
    }

    fn write_initial_status(&mut self) -> io::Result<()> {
        let sample = Sample {
            reflector: String::new(),
            seq: String::new(),
            timestamp: epoch_secs(),
            rtt_ms: 0.0,
        };
        self.write_status(0.0, 0.0, 0.0, 0.0, 0, 0, 0.0, 0.0, &sample)
    }

    fn log(&mut self, kind: &str, msg: &str) {
        if kind == "DEBUG" && !self.cfg.debug {
            return;
        }
        let line = format!("{kind}; {:.6}; {msg}", epoch_secs());
        if let Some(file) = &mut self.log {
            let _ = writeln!(file, "{line}");
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

    let mut child = spawn_fping(&cfg)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to capture fping stdout".to_string())?;
    let reader = BufReader::new(stdout);

    for line in reader.lines() {
        if TERMINATE.load(Ordering::SeqCst) {
            break;
        }
        let line = line.map_err(|e| format!("failed to read fping output: {e}"))?;
        if let Some(sample) = parse_fping_line(&line) {
            controller.on_sample(sample);
        }
    }

    stop_child(&mut child);
    Ok(())
}

fn spawn_fping(cfg: &Config) -> Result<Child, String> {
    let period_ms = (cfg.reflector_ping_interval_s * 1000.0).round().max(1.0) as u64;
    let interval_ms = (period_ms / cfg.no_pingers.max(1) as u64).max(1);
    let targets: Vec<&str> = cfg
        .reflectors
        .iter()
        .take(cfg.no_pingers)
        .map(String::as_str)
        .collect();

    Command::new("fping")
        .arg("--timestamp")
        .arg("--loop")
        .arg("--period")
        .arg(period_ms.to_string())
        .arg("--interval")
        .arg(interval_ms.to_string())
        .arg("--timeout")
        .arg("10000")
        .args(targets)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("failed to start fping: {e}"))
}

fn stop_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
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
        });
    }

    None
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
    use super::{parse_reflector_candidates, parse_uci_values};

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
}
