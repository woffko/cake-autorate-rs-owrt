use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

const IDLE_WINDOW: usize = 120;
const LOADED_WINDOW: usize = 40;
const MIN_IDLE_SAMPLES: usize = 20;
const MIN_LOADED_SAMPLES: usize = 20;

#[derive(Clone, Copy, Debug)]
pub struct ThroughputGuardInput {
    pub enabled: bool,
    pub configured_min_kbps: f64,
    pub configured_base_kbps: f64,
    pub observed_p20_kbps: f64,
    pub observed_p50_kbps: f64,
    pub absolute_floor_kbps: f64,
    pub retention_percent: f64,
}

pub fn throughput_floor(input: ThroughputGuardInput) -> f64 {
    if !input.enabled {
        return input.configured_min_kbps.max(1.0);
    }

    let reference = if input.observed_p20_kbps > 0.0 && input.observed_p50_kbps > 0.0 {
        input.observed_p20_kbps.max(0.75 * input.observed_p50_kbps)
    } else if input.observed_p20_kbps > 0.0 {
        input.observed_p20_kbps
    } else if input.observed_p50_kbps > 0.0 {
        0.75 * input.observed_p50_kbps
    } else {
        0.75 * input.configured_base_kbps
    };
    let retention = (input.retention_percent / 100.0).clamp(0.0, 1.0);

    input
        .configured_min_kbps
        .max(input.absolute_floor_kbps)
        .max(reference * retention)
        .max(1.0)
}

#[derive(Clone, Debug)]
pub struct TransportSnapshot {
    pub status: &'static str,
    pub endpoint: Option<String>,
    pub latency_ms: Option<f64>,
    pub baseline_ms: Option<f64>,
    pub delta_ms: Option<f64>,
    pub confirmed: bool,
    pub confidence: u8,
    pub sample_age_s: Option<f64>,
    pub successful_samples: u64,
    pub failed_samples: u64,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct TransportLatencyTracker {
    idle_samples: HashMap<String, VecDeque<f64>>,
    loaded_deltas: VecDeque<(Instant, f64)>,
    last_endpoint: Option<String>,
    last_latency_ms: Option<f64>,
    last_baseline_ms: Option<f64>,
    last_success_at: Option<Instant>,
    successful_samples: u64,
    failed_samples: u64,
    last_error: Option<String>,
}

impl TransportLatencyTracker {
    pub fn new() -> Self {
        Self {
            idle_samples: HashMap::new(),
            loaded_deltas: VecDeque::new(),
            last_endpoint: None,
            last_latency_ms: None,
            last_baseline_ms: None,
            last_success_at: None,
            successful_samples: 0,
            failed_samples: 0,
            last_error: None,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn observe_success(
        &mut self,
        endpoint: &str,
        latency_ms: f64,
        loaded: bool,
        now: Instant,
    ) -> Option<f64> {
        if !latency_ms.is_finite() || latency_ms <= 0.0 {
            self.observe_failure("invalid transport latency sample");
            return None;
        }

        self.successful_samples = self.successful_samples.saturating_add(1);
        self.last_endpoint = Some(endpoint.to_string());
        self.last_latency_ms = Some(latency_ms);
        self.last_success_at = Some(now);
        self.last_error = None;

        let baseline = self.baseline(endpoint);
        if !loaded {
            let samples = self.idle_samples.entry(endpoint.to_string()).or_default();
            samples.push_back(latency_ms);
            while samples.len() > IDLE_WINDOW {
                samples.pop_front();
            }
            self.last_baseline_ms = self.baseline(endpoint);
            return None;
        }

        self.last_baseline_ms = baseline;
        let baseline = baseline?;
        let delta_ms = (latency_ms - baseline).max(0.0);
        self.loaded_deltas.push_back((now, delta_ms));
        while self.loaded_deltas.len() > LOADED_WINDOW {
            self.loaded_deltas.pop_front();
        }
        self.confirmed_delta_ms()
    }

    pub fn observe_failure(&mut self, error: &str) {
        self.failed_samples = self.failed_samples.saturating_add(1);
        self.last_error = Some(error.to_string());
    }

    pub fn expire_loaded(&mut self, now: Instant, max_age: Duration) {
        while self
            .loaded_deltas
            .front()
            .map(|(at, _)| now.saturating_duration_since(*at) > max_age)
            .unwrap_or(false)
        {
            self.loaded_deltas.pop_front();
        }
    }

    pub fn confirmed_delta_ms(&self) -> Option<f64> {
        if self.loaded_deltas.len() < MIN_LOADED_SAMPLES {
            return None;
        }
        percentile(
            &self
                .loaded_deltas
                .iter()
                .map(|(_, value)| *value)
                .collect::<Vec<_>>(),
            90.0,
        )
    }

    pub fn snapshot(&self, now: Instant, enabled: bool) -> TransportSnapshot {
        let delta_ms = self.confirmed_delta_ms();
        let baseline_count = self
            .last_endpoint
            .as_ref()
            .and_then(|endpoint| self.idle_samples.get(endpoint))
            .map(VecDeque::len)
            .unwrap_or(0);
        let baseline_progress =
            (baseline_count.min(MIN_IDLE_SAMPLES) * 50 / MIN_IDLE_SAMPLES) as u8;
        let loaded_progress =
            (self.loaded_deltas.len().min(MIN_LOADED_SAMPLES) * 50 / MIN_LOADED_SAMPLES) as u8;
        let confidence = baseline_progress.saturating_add(loaded_progress);
        let status = if !enabled {
            "disabled"
        } else if self.last_error.is_some() && self.last_success_at.is_none() {
            "error"
        } else if delta_ms.is_some() {
            "ready"
        } else if baseline_count >= MIN_IDLE_SAMPLES && self.loaded_deltas.is_empty() {
            "baseline_ready"
        } else if baseline_count >= MIN_IDLE_SAMPLES {
            "learning_loaded"
        } else {
            "learning_baseline"
        };

        TransportSnapshot {
            status,
            endpoint: self.last_endpoint.clone(),
            latency_ms: self.last_latency_ms,
            baseline_ms: self.last_baseline_ms,
            delta_ms,
            confirmed: delta_ms.is_some(),
            confidence,
            sample_age_s: self
                .last_success_at
                .map(|at| now.saturating_duration_since(at).as_secs_f64()),
            successful_samples: self.successful_samples,
            failed_samples: self.failed_samples,
            last_error: self.last_error.clone(),
        }
    }

    fn baseline(&self, endpoint: &str) -> Option<f64> {
        let samples = self.idle_samples.get(endpoint)?;
        if samples.len() < MIN_IDLE_SAMPLES {
            return None;
        }
        let mut sorted = samples.iter().copied().collect::<Vec<_>>();
        sorted.sort_by(f64::total_cmp);
        let index = ((sorted.len() - 1) as f64 * 0.05).floor() as usize;
        sorted.get(index).copied()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QualityClass {
    Learning,
    APlus,
    A,
    B,
    C,
    D,
    F,
}

impl QualityClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Learning => "LEARNING",
            Self::APlus => "A+",
            Self::A => "A",
            Self::B => "B",
            Self::C => "C",
            Self::D => "D",
            Self::F => "F",
        }
    }
}

pub fn classify_quality(delta_ms: Option<f64>) -> QualityClass {
    let Some(delta_ms) = delta_ms.filter(|value| value.is_finite() && *value >= 0.0) else {
        return QualityClass::Learning;
    };

    if delta_ms < 5.0 {
        QualityClass::APlus
    } else if delta_ms < 30.0 {
        QualityClass::A
    } else if delta_ms < 60.0 {
        QualityClass::B
    } else if delta_ms < 200.0 {
        QualityClass::C
    } else if delta_ms < 400.0 {
        QualityClass::D
    } else {
        QualityClass::F
    }
}

pub fn effective_latency_delta_ms(
    icmp_dl_delta_us: f64,
    icmp_ul_delta_us: f64,
    transport_delta_ms: Option<f64>,
) -> f64 {
    let icmp_ms = (icmp_dl_delta_us.max(icmp_ul_delta_us) / 1000.0).max(0.0);
    transport_delta_ms.unwrap_or(0.0).max(icmp_ms)
}

pub fn transport_allows_growth(
    enabled: bool,
    confirmed: bool,
    sample_age_s: Option<f64>,
    max_age_s: f64,
    delta_ms: Option<f64>,
    target_delay_ms: f64,
) -> bool {
    !enabled
        || (confirmed
            && sample_age_s.map(|age| age <= max_age_s) == Some(true)
            && delta_ms.map(|delta| delta <= target_delay_ms) == Some(true))
}

#[derive(Clone, Copy, Debug)]
pub struct QualitySearchPolicy {
    pub target_delay_ms: f64,
    pub floor_kbps: f64,
    pub max_steps: u8,
    pub observe_duration: Duration,
    pub cooldown: Duration,
}

#[derive(Clone, Debug)]
pub struct QualitySearchUpdate {
    pub requested_rate_kbps: Option<f64>,
    pub limited: bool,
    pub reason: &'static str,
}

#[derive(Clone, Debug)]
pub struct QualitySearchDirection {
    steps: u8,
    start_rate_kbps: Option<f64>,
    start_delta_ms: Option<f64>,
    best_rate_kbps: Option<f64>,
    best_delta_ms: Option<f64>,
    previous_rate_kbps: Option<f64>,
    previous_delta_ms: Option<f64>,
    last_step_at: Option<Instant>,
    cooldown_until: Option<Instant>,
    limited: bool,
    last_reason: &'static str,
}

impl QualitySearchDirection {
    pub fn new() -> Self {
        Self {
            steps: 0,
            start_rate_kbps: None,
            start_delta_ms: None,
            best_rate_kbps: None,
            best_delta_ms: None,
            previous_rate_kbps: None,
            previous_delta_ms: None,
            last_step_at: None,
            cooldown_until: None,
            limited: false,
            last_reason: "learning",
        }
    }

    pub fn observe(
        &mut self,
        now: Instant,
        current_rate_kbps: f64,
        delta_ms: f64,
        high_load: bool,
        policy: QualitySearchPolicy,
    ) -> QualitySearchUpdate {
        if !high_load || !delta_ms.is_finite() {
            return self.no_change("waiting_for_high_load");
        }

        if delta_ms <= policy.target_delay_ms {
            self.reset_episode();
            self.last_reason = "target_met";
            return self.no_change("target_met");
        }

        if self
            .cooldown_until
            .map(|until| now < until)
            .unwrap_or(false)
        {
            return self.no_change("quality_limited_cooldown");
        }
        if self.cooldown_until.is_some() {
            self.reset_episode();
        }

        if self.start_rate_kbps.is_none() {
            self.start_rate_kbps = Some(current_rate_kbps);
            self.start_delta_ms = Some(delta_ms);
            self.best_rate_kbps = Some(current_rate_kbps);
            self.best_delta_ms = Some(delta_ms);
        }

        if let Some(last_step_at) = self.last_step_at {
            if now.saturating_duration_since(last_step_at) < policy.observe_duration {
                return self.no_change("observing_candidate");
            }

            let previous_delta = self.previous_delta_ms.unwrap_or(delta_ms);
            let improvement = previous_delta - delta_ms;
            if improvement >= 10.0 {
                self.note_candidate(current_rate_kbps, delta_ms);
            } else {
                let fallback = self.fallback_rate();
                return self.limit(
                    now,
                    policy.cooldown,
                    Some(fallback),
                    if delta_ms > previous_delta + 5.0 {
                        "candidate_worsened_latency"
                    } else {
                        "candidate_no_meaningful_gain"
                    },
                );
            }
        }

        if current_rate_kbps <= policy.floor_kbps * 1.01 {
            return self.limit(
                now,
                policy.cooldown,
                Some(self.fallback_rate()),
                "target_unreachable_above_throughput_floor",
            );
        }

        if self.steps >= policy.max_steps {
            return self.limit(
                now,
                policy.cooldown,
                Some(self.fallback_rate()),
                "quality_search_step_limit",
            );
        }

        let factor = (policy.target_delay_ms / delta_ms)
            .max(0.0)
            .sqrt()
            .clamp(0.70, 0.97);
        let candidate = (current_rate_kbps * factor).max(policy.floor_kbps);
        self.previous_rate_kbps = Some(current_rate_kbps);
        self.previous_delta_ms = Some(delta_ms);
        self.last_step_at = Some(now);
        self.steps = self.steps.saturating_add(1);
        self.last_reason = "transport_latency_backoff";

        QualitySearchUpdate {
            requested_rate_kbps: Some(candidate),
            limited: false,
            reason: self.last_reason,
        }
    }

    pub fn limited(&self) -> bool {
        self.limited
    }

    pub fn last_reason(&self) -> &'static str {
        self.last_reason
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    fn note_candidate(&mut self, rate_kbps: f64, delta_ms: f64) {
        let replace = self
            .best_delta_ms
            .map(|best| delta_ms + 1.0 < best)
            .unwrap_or(true);
        if replace {
            self.best_rate_kbps = Some(rate_kbps);
            self.best_delta_ms = Some(delta_ms);
        }
    }

    fn fallback_rate(&self) -> f64 {
        let start_rate = self.start_rate_kbps.unwrap_or(1.0);
        let start_delta = self.start_delta_ms.unwrap_or(f64::INFINITY);
        let best_rate = self.best_rate_kbps.unwrap_or(start_rate);
        let best_delta = self.best_delta_ms.unwrap_or(start_delta);
        let meaningful = start_delta - best_delta >= 30.0
            || (start_delta > 0.0 && best_delta <= start_delta * 0.75);
        if meaningful {
            best_rate
        } else {
            start_rate
        }
    }

    fn limit(
        &mut self,
        now: Instant,
        cooldown: Duration,
        requested_rate_kbps: Option<f64>,
        reason: &'static str,
    ) -> QualitySearchUpdate {
        self.limited = true;
        self.last_reason = reason;
        self.cooldown_until = Some(now + cooldown);
        QualitySearchUpdate {
            requested_rate_kbps,
            limited: true,
            reason,
        }
    }

    fn reset_episode(&mut self) {
        self.steps = 0;
        self.start_rate_kbps = None;
        self.start_delta_ms = None;
        self.best_rate_kbps = None;
        self.best_delta_ms = None;
        self.previous_rate_kbps = None;
        self.previous_delta_ms = None;
        self.last_step_at = None;
        self.cooldown_until = None;
        self.limited = false;
    }

    fn no_change(&self, reason: &'static str) -> QualitySearchUpdate {
        QualitySearchUpdate {
            requested_rate_kbps: None,
            limited: self.limited,
            reason,
        }
    }
}

fn percentile(values: &[f64], percentile: f64) -> Option<f64> {
    let mut sorted = values
        .iter()
        .copied()
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by(f64::total_cmp);
    let index = (percentile.clamp(0.0, 100.0) / 100.0) * (sorted.len() - 1) as f64;
    let lower = index.floor() as usize;
    let upper = index.ceil() as usize;
    if lower == upper {
        return sorted.get(lower).copied();
    }
    let weight = index - lower as f64;
    Some(sorted[lower] * (1.0 - weight) + sorted[upper] * weight)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_uses_robust_low_and_median_reference() {
        let floor = throughput_floor(ThroughputGuardInput {
            enabled: true,
            configured_min_kbps: 15_000.0,
            configured_base_kbps: 114_000.0,
            observed_p20_kbps: 41_800.0,
            observed_p50_kbps: 114_000.0,
            absolute_floor_kbps: 0.0,
            retention_percent: 80.0,
        });
        assert_eq!(floor, 68_400.0);
    }

    #[test]
    fn guard_fallback_preserves_sixty_percent_of_base() {
        let floor = throughput_floor(ThroughputGuardInput {
            enabled: true,
            configured_min_kbps: 5_000.0,
            configured_base_kbps: 100_000.0,
            observed_p20_kbps: 0.0,
            observed_p50_kbps: 0.0,
            absolute_floor_kbps: 0.0,
            retention_percent: 80.0,
        });
        assert_eq!(floor, 60_000.0);
    }

    #[test]
    fn transport_requires_twenty_idle_and_loaded_samples() {
        let now = Instant::now();
        let mut tracker = TransportLatencyTracker::new();
        for index in 0..MIN_IDLE_SAMPLES {
            assert_eq!(
                tracker.observe_success(
                    "a",
                    20.0 + (index % 2) as f64,
                    false,
                    now + Duration::from_millis(index as u64),
                ),
                None
            );
        }
        assert_eq!(tracker.snapshot(now, true).status, "baseline_ready");
        for index in 0..MIN_LOADED_SAMPLES - 1 {
            assert_eq!(
                tracker.observe_success(
                    "a",
                    120.0,
                    true,
                    now + Duration::from_secs(1) + Duration::from_millis(index as u64),
                ),
                None
            );
        }
        assert_eq!(
            tracker.snapshot(now + Duration::from_secs(1), true).status,
            "learning_loaded"
        );
        let delta = tracker
            .observe_success("a", 100.0, true, now + Duration::from_secs(2))
            .unwrap();
        assert!((99.0..=100.0).contains(&delta));
        assert_eq!(
            tracker.snapshot(now + Duration::from_secs(2), true).status,
            "ready"
        );
    }

    #[test]
    fn route_reset_discards_transport_baseline_and_loaded_samples() {
        let now = Instant::now();
        let mut tracker = TransportLatencyTracker::new();
        for index in 0..MIN_IDLE_SAMPLES {
            tracker.observe_success(
                "wan-endpoint",
                20.0,
                false,
                now + Duration::from_millis(index as u64),
            );
        }
        for index in 0..MIN_LOADED_SAMPLES {
            tracker.observe_success(
                "wan-endpoint",
                100.0,
                true,
                now + Duration::from_secs(1) + Duration::from_millis(index as u64),
            );
        }
        assert!(tracker.confirmed_delta_ms().is_some());

        tracker.reset();
        let snapshot = tracker.snapshot(now + Duration::from_secs(3), true);
        assert_eq!(snapshot.status, "learning_baseline");
        assert_eq!(snapshot.successful_samples, 0);
        assert!(snapshot.delta_ms.is_none());
    }

    #[test]
    fn quality_thresholds_are_stable() {
        assert_eq!(classify_quality(None), QualityClass::Learning);
        assert_eq!(classify_quality(Some(4.999)), QualityClass::APlus);
        assert_eq!(classify_quality(Some(5.0)), QualityClass::A);
        assert_eq!(classify_quality(Some(30.0)), QualityClass::B);
        assert_eq!(classify_quality(Some(60.0)), QualityClass::C);
        assert_eq!(classify_quality(Some(200.0)), QualityClass::D);
        assert_eq!(classify_quality(Some(400.0)), QualityClass::F);
        assert_eq!(classify_quality(Some(401.0)), QualityClass::F);
    }

    #[test]
    fn transport_delay_blocks_growth_when_icmp_looks_clean() {
        assert_eq!(
            effective_latency_delta_ms(2_000.0, 3_000.0, Some(85.0)),
            85.0
        );
        assert!(!transport_allows_growth(
            true,
            true,
            Some(1.0),
            8.0,
            Some(85.0),
            30.0,
        ));
        assert!(transport_allows_growth(
            true,
            true,
            Some(1.0),
            8.0,
            Some(20.0),
            30.0,
        ));
    }

    #[test]
    fn search_never_requests_below_floor() {
        let now = Instant::now();
        let mut search = QualitySearchDirection::new();
        let policy = QualitySearchPolicy {
            target_delay_ms: 30.0,
            floor_kbps: 70_000.0,
            max_steps: 3,
            observe_duration: Duration::from_secs(5),
            cooldown: Duration::from_secs(60),
        };
        let update = search.observe(now, 114_000.0, 237.0, true, policy);
        assert_eq!(update.requested_rate_kbps, Some(79_800.0));
        let update = search.observe(now + Duration::from_secs(6), 79_800.0, 169.0, true, policy);
        assert_eq!(update.requested_rate_kbps, Some(70_000.0));
    }

    #[test]
    fn worsening_candidate_rolls_back_and_limits_search() {
        let now = Instant::now();
        let mut search = QualitySearchDirection::new();
        let policy = QualitySearchPolicy {
            target_delay_ms: 30.0,
            floor_kbps: 25_000.0,
            max_steps: 3,
            observe_duration: Duration::from_secs(5),
            cooldown: Duration::from_secs(60),
        };
        let first = search.observe(now, 70_000.0, 169.0, true, policy);
        let first_rate = first.requested_rate_kbps.unwrap();
        let second = search.observe(
            now + Duration::from_secs(6),
            first_rate,
            180.0,
            true,
            policy,
        );
        assert!(second.limited);
        assert_eq!(second.reason, "candidate_worsened_latency");
        assert_eq!(second.requested_rate_kbps, Some(70_000.0));
    }

    #[test]
    fn expired_cooldown_starts_a_fresh_search_episode() {
        let now = Instant::now();
        let policy = QualitySearchPolicy {
            target_delay_ms: 30.0,
            floor_kbps: 70_000.0,
            max_steps: 1,
            observe_duration: Duration::from_secs(5),
            cooldown: Duration::from_secs(30),
        };
        let mut search = QualitySearchDirection::new();
        assert!(search
            .observe(now, 100_000.0, 100.0, true, policy)
            .requested_rate_kbps
            .is_some());
        assert!(
            search
                .observe(now + Duration::from_secs(5), 70_000.0, 90.0, true, policy)
                .limited
        );
        let update = search.observe(
            now + Duration::from_secs(36),
            100_000.0,
            100.0,
            true,
            policy,
        );
        assert!(update.requested_rate_kbps.is_some());
        assert!(!update.limited);
    }
}
