//! Pure controller invariants. This boundary never resolves interfaces or
//! contacts a reflector, and is shared by runtime parsing and candidate checks.
use super::Config;

// At most 72 KiB of logical bufferbloat history per instance (2 bool + 2 f64
// windows), independently of allocator rounding. Defaults use only 6 samples.
pub(crate) const MAX_WINDOW_SAMPLES: usize = 4096;
// Existing Lite UI limit, now enforced by the common native parser as well.
pub(crate) const MAX_PINGERS: usize = 64;
// Common portable timer domain: signed poll milliseconds, approximately25d.
pub(crate) const MAX_TIMER_S: f64 = i32::MAX as f64 / 1000.0;
pub(crate) const MIN_TIMER_S: f64 = 1e-9;
const MAX_HISTORY_BYTES: usize = 1024 * 1024;
const MIN_ENABLED_RATE_KBPS: f64 = 1.0;

pub(crate) fn rate_schema() -> serde_json::Value {
    let defaults = Config::defaults(String::new());
    serde_json::json!({"min": MIN_ENABLED_RATE_KBPS, "max": super::rate_limits::MAX_RATE_KBPS,
    "disabled_zero_tuple": true,
    "defaults": {
        "dl": [defaults.min_dl_shaper_rate_kbps, defaults.base_dl_shaper_rate_kbps, defaults.max_dl_shaper_rate_kbps],
        "ul": [defaults.min_ul_shaper_rate_kbps, defaults.base_ul_shaper_rate_kbps, defaults.max_ul_shaper_rate_kbps]
    }})
}

fn bounded(name: &str, value: f64, minimum: f64, maximum: f64) -> Result<(), String> {
    if !value.is_finite() || value < minimum || value > maximum {
        return Err(format!(
            "{name} must be finite and between {minimum} and {maximum}"
        ));
    }
    Ok(())
}

pub(crate) fn validate_core(c: &Config) -> Result<(), String> {
    let max_rate = super::rate_limits::MAX_RATE_KBPS as f64;
    for (direction, enabled, min, base, max) in [
        (
            "download",
            c.adjust_dl_shaper_rate,
            c.min_dl_shaper_rate_kbps,
            c.base_dl_shaper_rate_kbps,
            c.max_dl_shaper_rate_kbps,
        ),
        (
            "upload",
            c.adjust_ul_shaper_rate,
            c.min_ul_shaper_rate_kbps,
            c.base_ul_shaper_rate_kbps,
            c.max_ul_shaper_rate_kbps,
        ),
    ] {
        // A disabled direction may be explicitly unshaped (all zero), or retain
        // a valid rate tuple for later re-enabling. Mixed zero tuples are invalid.
        if !enabled && min == 0.0 && base == 0.0 && max == 0.0 {
            continue;
        }
        for (name, value) in [("minimum", min), ("base", base), ("maximum", max)] {
            bounded(
                &format!("{direction} {name} rate"),
                value,
                MIN_ENABLED_RATE_KBPS,
                max_rate,
            )?;
        }
        if min > base || base > max {
            return Err(format!("{direction} rates must satisfy min <= base <= max"));
        }
    }
    macro_rules! range {
        ($min:expr, $max:expr; $($field:ident),+ $(,)?) => {
            $(bounded(stringify!($field), c.$field, $min, $max)?;)+
        };
    }
    range!(0.0, max_rate;
        connection_active_thr_kbps, connection_stall_thr_kbps,
        adaptive_ceiling_dl_cap_kbps, adaptive_ceiling_ul_cap_kbps,
        adaptive_ceiling_dl_safe_kbps, adaptive_ceiling_ul_safe_kbps);
    range!(0.01, 1.0; high_load_thr);
    range!(0.0, 1.0; alpha_baseline_increase, alpha_baseline_decrease, alpha_delta_ewma);
    range!(f64::MIN_POSITIVE, 1.0;
        shaper_rate_min_adjust_down_bufferbloat, shaper_rate_max_adjust_down_bufferbloat,
        shaper_rate_adjust_down_load_low);
    // A larger multiplier cannot yield another usable rate within [1, MAX_RATE]
    // and needlessly risks overflow before the normal rate clamp.
    range!(1.0, max_rate;
        shaper_rate_min_adjust_up_load_high, shaper_rate_max_adjust_up_load_high,
        shaper_rate_adjust_up_load_low);
    if c.shaper_rate_max_adjust_down_bufferbloat > c.shaper_rate_min_adjust_down_bufferbloat {
        return Err("shaper_rate_max_adjust_down_bufferbloat must not exceed shaper_rate_min_adjust_down_bufferbloat".to_string());
    }
    if c.shaper_rate_min_adjust_up_load_high > c.shaper_rate_max_adjust_up_load_high {
        return Err("shaper_rate_min_adjust_up_load_high must not exceed shaper_rate_max_adjust_up_load_high".to_string());
    }
    range!(0.0, MAX_TIMER_S * 1000.0;
        dl_owd_delta_delay_thr_ms, ul_owd_delta_delay_thr_ms,
        dl_avg_owd_delta_max_adjust_up_thr_ms, ul_avg_owd_delta_max_adjust_up_thr_ms,
        dl_avg_owd_delta_max_adjust_down_thr_ms, ul_avg_owd_delta_max_adjust_down_thr_ms,
        reflector_sum_owd_baselines_delta_thr_ms, reflector_owd_delta_ewma_delta_thr_ms);
    for (direction, up, delay, down) in [
        (
            "download",
            c.dl_avg_owd_delta_max_adjust_up_thr_ms,
            c.dl_owd_delta_delay_thr_ms,
            c.dl_avg_owd_delta_max_adjust_down_thr_ms,
        ),
        (
            "upload",
            c.ul_avg_owd_delta_max_adjust_up_thr_ms,
            c.ul_owd_delta_delay_thr_ms,
            c.ul_avg_owd_delta_max_adjust_down_thr_ms,
        ),
    ] {
        if up > delay || delay > down {
            return Err(format!(
                "{direction} delay thresholds must satisfy growth <= bloat <= maximum backoff"
            ));
        }
    }
    range!(0.05, MAX_TIMER_S; reflector_ping_interval_s);
    range!(MIN_TIMER_S, MAX_TIMER_S;
        reflector_health_check_interval_s, reflector_response_deadline_s,
        global_ping_response_timeout_s, if_up_check_interval_s, route_check_interval_s,
        adaptive_ceiling_hold_time_s, adaptive_ceiling_probe_duration_s, adaptive_ceiling_failed_bound_ttl_s);
    range!(0.0, MAX_TIMER_S; startup_wait_s, sustained_idle_sleep_thr_s, adaptive_ceiling_cooldown_s);
    range!(0.0, MAX_TIMER_S / 60.0; reflector_replacement_interval_mins, reflector_comparison_interval_mins);
    #[cfg(feature = "calibration")]
    range!(MIN_TIMER_S / 60.0, MAX_TIMER_S / 60.0; irtt_session_duration_m);
    for (name, value) in [
        (
            "monitor_achieved_rates_interval_ms",
            c.monitor_achieved_rates_interval_ms,
        ),
        (
            "monitor_cpu_usage_interval_ms",
            c.monitor_cpu_usage_interval_ms,
        ),
    ] {
        if value == 0 || value > i32::MAX as u64 {
            return Err(format!("{name} must be between 1 and {}", i32::MAX));
        }
    }
    for (name, window, threshold) in [
        (
            "bufferbloat_detection",
            c.bufferbloat_detection_window,
            c.bufferbloat_detection_thr,
        ),
        (
            "reflector_misbehaving_detection",
            c.reflector_misbehaving_detection_window,
            c.reflector_misbehaving_detection_thr,
        ),
    ] {
        if !(1..=MAX_WINDOW_SAMPLES).contains(&window) {
            return Err(format!(
                "{name}_window must be between 1 and {MAX_WINDOW_SAMPLES}"
            ));
        }
        if threshold == 0 || threshold > window {
            return Err(format!(
                "{name}_thr must be between 1 and its window length"
            ));
        }
    }
    if !(1..=MAX_PINGERS).contains(&c.no_pingers) {
        return Err(format!("no_pingers must be between 1 and {MAX_PINGERS}"));
    }
    let history_bytes = c
        .bufferbloat_detection_window
        .saturating_mul(18)
        .saturating_add(
            c.reflectors
                .len()
                .saturating_mul(c.reflector_misbehaving_detection_window),
        );
    if history_bytes > MAX_HISTORY_BYTES {
        return Err(
            "configured reflector and controller history exceeds the 1 MiB logical sample budget"
                .to_string(),
        );
    }
    Ok(())
}
