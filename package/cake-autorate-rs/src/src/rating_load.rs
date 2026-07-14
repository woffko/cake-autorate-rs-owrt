use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RatingPhase {
    Idle,
    Download,
    Upload,
    Bidirectional,
}

impl RatingPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "IDLE",
            Self::Download => "DL",
            Self::Upload => "UL",
            Self::Bidirectional => "BIDIRECTIONAL",
        }
    }

    pub fn loaded(self) -> bool {
        self != Self::Idle
    }

    pub fn direction_flags(self) -> (bool, bool) {
        match self {
            Self::Idle => (false, false),
            Self::Download => (true, false),
            Self::Upload => (false, true),
            Self::Bidirectional => (true, true),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RatingLoadConfig {
    pub window: Duration,
    pub enter_ratio: f64,
    pub exit_ratio: f64,
    pub hold: Duration,
    pub dropout: Duration,
    pub min_rate_kbps: f64,
    pub dominance_ratio: f64,
}

#[derive(Clone, Debug)]
pub struct RatingLoadSnapshot {
    pub phase: RatingPhase,
    pub candidate: RatingPhase,
    pub raw_dl_percent: f64,
    pub raw_ul_percent: f64,
    pub smoothed_dl_percent: f64,
    pub smoothed_ul_percent: f64,
    pub enter_percent: f64,
    pub exit_percent: f64,
    pub phase_age_s: f64,
    pub capture_active: bool,
    pub capture_mode: &'static str,
    pub capture_peak_dl_percent: f64,
    pub capture_peak_ul_percent: f64,
}

#[derive(Clone, Copy, Debug)]
struct LoadPoint {
    at: Instant,
    dl_rate_kbps: f64,
    ul_rate_kbps: f64,
    dl_ratio: f64,
    ul_ratio: f64,
}

pub struct RatingLoadDetector {
    samples: VecDeque<LoadPoint>,
    phase: RatingPhase,
    phase_since: Instant,
    candidate: RatingPhase,
    candidate_since: Instant,
    unsupported_since: Option<Instant>,
    capture_token: String,
    capture_mode: String,
    capture_peak_dl_ratio: f64,
    capture_peak_ul_ratio: f64,
}

impl RatingLoadDetector {
    pub fn new(now: Instant) -> Self {
        Self {
            samples: VecDeque::new(),
            phase: RatingPhase::Idle,
            phase_since: now,
            candidate: RatingPhase::Idle,
            candidate_since: now,
            unsupported_since: None,
            capture_token: String::new(),
            capture_mode: String::new(),
            capture_peak_dl_ratio: 0.0,
            capture_peak_ul_ratio: 0.0,
        }
    }

    pub fn set_capture(&mut self, token: Option<&str>, mode: Option<&str>, now: Instant) {
        let token = token.unwrap_or("");
        if self.capture_token == token {
            return;
        }
        self.capture_token.clear();
        self.capture_token.push_str(token);
        self.capture_mode.clear();
        self.capture_mode.push_str(mode.unwrap_or(""));
        self.capture_peak_dl_ratio = 0.0;
        self.capture_peak_ul_ratio = 0.0;
        self.samples.clear();
        self.transition(RatingPhase::Idle, now);
    }

    pub fn observe(
        &mut self,
        now: Instant,
        dl_rate_kbps: f64,
        ul_rate_kbps: f64,
        dl_shaper_kbps: f64,
        ul_shaper_kbps: f64,
        cfg: RatingLoadConfig,
    ) -> RatingLoadSnapshot {
        let dl_rate_kbps = finite_nonnegative(dl_rate_kbps);
        let ul_rate_kbps = finite_nonnegative(ul_rate_kbps);
        let dl_ratio = safe_ratio(dl_rate_kbps, dl_shaper_kbps);
        let ul_ratio = safe_ratio(ul_rate_kbps, ul_shaper_kbps);
        self.samples.push_back(LoadPoint {
            at: now,
            dl_rate_kbps,
            ul_rate_kbps,
            dl_ratio,
            ul_ratio,
        });
        while self
            .samples
            .front()
            .map(|sample| now.saturating_duration_since(sample.at) > cfg.window)
            .unwrap_or(false)
        {
            self.samples.pop_front();
        }

        let (smoothed_dl_rate, smoothed_ul_rate, smoothed_dl, smoothed_ul) = self.smoothed();
        self.capture_peak_dl_ratio = self.capture_peak_dl_ratio.max(smoothed_dl);
        self.capture_peak_ul_ratio = self.capture_peak_ul_ratio.max(smoothed_ul);
        let capture_active = !self.capture_token.is_empty();
        let enter_ratio = if capture_active {
            let learned_peak = self
                .capture_peak_dl_ratio
                .max(self.capture_peak_ul_ratio)
                .clamp(0.0, 1.5);
            cfg.enter_ratio.min((learned_peak * 0.55).max(0.15))
        } else {
            cfg.enter_ratio
        };
        let exit_ratio = if capture_active {
            cfg.exit_ratio.min((enter_ratio * 0.67).max(0.10))
        } else {
            cfg.exit_ratio
        };

        let target = classify(
            smoothed_dl,
            smoothed_ul,
            smoothed_dl_rate,
            smoothed_ul_rate,
            enter_ratio,
            cfg.min_rate_kbps,
            cfg.dominance_ratio,
        );
        let supported = classify(
            smoothed_dl,
            smoothed_ul,
            smoothed_dl_rate,
            smoothed_ul_rate,
            exit_ratio,
            cfg.min_rate_kbps * 0.5,
            cfg.dominance_ratio,
        );

        self.advance(now, target, supported, cfg);
        RatingLoadSnapshot {
            phase: self.phase,
            candidate: self.candidate,
            raw_dl_percent: dl_ratio * 100.0,
            raw_ul_percent: ul_ratio * 100.0,
            smoothed_dl_percent: smoothed_dl * 100.0,
            smoothed_ul_percent: smoothed_ul * 100.0,
            enter_percent: enter_ratio * 100.0,
            exit_percent: exit_ratio * 100.0,
            phase_age_s: now
                .saturating_duration_since(self.phase_since)
                .as_secs_f64(),
            capture_active,
            capture_mode: if capture_active {
                if self.capture_mode == "automatic" {
                    "automatic"
                } else {
                    "client"
                }
            } else {
                "passive"
            },
            capture_peak_dl_percent: self.capture_peak_dl_ratio * 100.0,
            capture_peak_ul_percent: self.capture_peak_ul_ratio * 100.0,
        }
    }

    pub fn snapshot(&self, now: Instant, cfg: RatingLoadConfig) -> RatingLoadSnapshot {
        let (_, _, dl, ul) = self.smoothed();
        RatingLoadSnapshot {
            phase: self.phase,
            candidate: self.candidate,
            raw_dl_percent: self
                .samples
                .back()
                .map(|value| value.dl_ratio * 100.0)
                .unwrap_or(0.0),
            raw_ul_percent: self
                .samples
                .back()
                .map(|value| value.ul_ratio * 100.0)
                .unwrap_or(0.0),
            smoothed_dl_percent: dl * 100.0,
            smoothed_ul_percent: ul * 100.0,
            enter_percent: cfg.enter_ratio * 100.0,
            exit_percent: cfg.exit_ratio * 100.0,
            phase_age_s: now
                .saturating_duration_since(self.phase_since)
                .as_secs_f64(),
            capture_active: !self.capture_token.is_empty(),
            capture_mode: if self.capture_token.is_empty() {
                "passive"
            } else if self.capture_mode == "automatic" {
                "automatic"
            } else {
                "client"
            },
            capture_peak_dl_percent: self.capture_peak_dl_ratio * 100.0,
            capture_peak_ul_percent: self.capture_peak_ul_ratio * 100.0,
        }
    }

    fn smoothed(&self) -> (f64, f64, f64, f64) {
        if self.samples.is_empty() {
            return (0.0, 0.0, 0.0, 0.0);
        }
        let count = self.samples.len() as f64;
        let dl_rate = self
            .samples
            .iter()
            .map(|sample| sample.dl_rate_kbps)
            .sum::<f64>()
            / count;
        let ul_rate = self
            .samples
            .iter()
            .map(|sample| sample.ul_rate_kbps)
            .sum::<f64>()
            / count;
        let dl = self
            .samples
            .iter()
            .map(|sample| sample.dl_ratio)
            .sum::<f64>()
            / count;
        let ul = self
            .samples
            .iter()
            .map(|sample| sample.ul_ratio)
            .sum::<f64>()
            / count;
        (dl_rate, ul_rate, dl, ul)
    }

    fn advance(
        &mut self,
        now: Instant,
        target: RatingPhase,
        supported: RatingPhase,
        cfg: RatingLoadConfig,
    ) {
        if target == self.phase {
            self.clear_candidate(now);
            self.unsupported_since = None;
            return;
        }

        if self.phase.loaded() && target == RatingPhase::Idle {
            if supports(self.phase, supported) {
                self.clear_candidate(now);
                self.unsupported_since = None;
                return;
            }
            let since = *self.unsupported_since.get_or_insert(now);
            if now.saturating_duration_since(since) >= cfg.dropout {
                self.transition(RatingPhase::Idle, now);
            }
            return;
        }
        self.unsupported_since = None;

        if target != self.candidate {
            self.candidate = target;
            self.candidate_since = now;
            return;
        }
        if now.saturating_duration_since(self.candidate_since) >= cfg.hold {
            self.transition(target, now);
        }
    }

    fn transition(&mut self, phase: RatingPhase, now: Instant) {
        self.phase = phase;
        self.phase_since = now;
        self.clear_candidate(now);
        self.unsupported_since = None;
    }

    fn clear_candidate(&mut self, now: Instant) {
        self.candidate = self.phase;
        self.candidate_since = now;
    }
}

fn finite_nonnegative(value: f64) -> f64 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn safe_ratio(rate: f64, shaper: f64) -> f64 {
    if shaper.is_finite() && shaper > 0.0 {
        (rate / shaper).clamp(0.0, 10.0)
    } else {
        0.0
    }
}

fn classify(
    dl_ratio: f64,
    ul_ratio: f64,
    dl_rate: f64,
    ul_rate: f64,
    threshold: f64,
    min_rate: f64,
    dominance: f64,
) -> RatingPhase {
    let dl = dl_ratio >= threshold && dl_rate >= min_rate;
    let ul = ul_ratio >= threshold && ul_rate >= min_rate;
    match (dl, ul) {
        (false, false) => RatingPhase::Idle,
        (true, false) => RatingPhase::Download,
        (false, true) => RatingPhase::Upload,
        (true, true) if dl_ratio >= ul_ratio * dominance => RatingPhase::Download,
        (true, true) if ul_ratio >= dl_ratio * dominance => RatingPhase::Upload,
        (true, true) => RatingPhase::Bidirectional,
    }
}

fn supports(current: RatingPhase, observed: RatingPhase) -> bool {
    current == observed
        || matches!(
            (current, observed),
            (RatingPhase::Download, RatingPhase::Bidirectional)
                | (RatingPhase::Upload, RatingPhase::Bidirectional)
                | (RatingPhase::Bidirectional, RatingPhase::Download)
                | (RatingPhase::Bidirectional, RatingPhase::Upload)
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> RatingLoadConfig {
        RatingLoadConfig {
            window: Duration::from_secs(2),
            enter_ratio: 0.60,
            exit_ratio: 0.40,
            hold: Duration::from_secs(1),
            dropout: Duration::from_millis(1500),
            min_rate_kbps: 2_000.0,
            dominance_ratio: 1.5,
        }
    }

    fn feed(
        detector: &mut RatingLoadDetector,
        start: Instant,
        rates: &[(f64, f64)],
    ) -> RatingLoadSnapshot {
        let mut snapshot = detector.snapshot(start, cfg());
        for (index, (dl, ul)) in rates.iter().enumerate() {
            snapshot = detector.observe(
                start + Duration::from_millis(index as u64 * 200),
                *dl,
                *ul,
                900_000.0,
                860_000.0,
                cfg(),
            );
        }
        snapshot
    }

    #[test]
    fn bursty_browser_download_latches_and_survives_short_dips() {
        let start = Instant::now();
        let mut detector = RatingLoadDetector::new(start);
        let mut trace = Vec::new();
        for index in 0..50 {
            let dl = if index % 7 == 0 { 220_000.0 } else { 835_000.0 };
            trace.push((dl, 28_000.0));
        }
        let snapshot = feed(&mut detector, start, &trace);
        assert_eq!(snapshot.phase, RatingPhase::Download);
        assert!(snapshot.smoothed_dl_percent > 60.0);
    }

    #[test]
    fn background_traffic_never_becomes_loaded() {
        let start = Instant::now();
        let mut detector = RatingLoadDetector::new(start);
        let trace = vec![(200_000.0, 20_000.0); 60];
        assert_eq!(feed(&mut detector, start, &trace).phase, RatingPhase::Idle);
    }

    #[test]
    fn ack_traffic_does_not_turn_download_into_bidirectional() {
        let start = Instant::now();
        let mut detector = RatingLoadDetector::new(start);
        let trace = vec![(820_000.0, 90_000.0); 30];
        assert_eq!(
            feed(&mut detector, start, &trace).phase,
            RatingPhase::Download
        );
    }

    #[test]
    fn sustained_symmetric_load_is_bidirectional() {
        let start = Instant::now();
        let mut detector = RatingLoadDetector::new(start);
        let trace = vec![(760_000.0, 720_000.0); 30];
        assert_eq!(
            feed(&mut detector, start, &trace).phase,
            RatingPhase::Bidirectional
        );
    }

    #[test]
    fn guided_capture_adapts_below_configured_ceiling() {
        let start = Instant::now();
        let mut detector = RatingLoadDetector::new(start);
        detector.set_capture(Some("token-1"), Some("client"), start);
        let trace = vec![(360_000.0, 10_000.0); 30];
        assert_eq!(
            feed(&mut detector, start, &trace).phase,
            RatingPhase::Download
        );
    }

    #[test]
    fn brief_idle_gap_does_not_drop_latched_phase() {
        let start = Instant::now();
        let mut detector = RatingLoadDetector::new(start);
        let loaded = vec![(820_000.0, 20_000.0); 30];
        assert_eq!(
            feed(&mut detector, start, &loaded).phase,
            RatingPhase::Download
        );
        let later = start + Duration::from_secs(6);
        let mut snapshot = detector.snapshot(later, cfg());
        for index in 0..5 {
            snapshot = detector.observe(
                later + Duration::from_millis(index * 200),
                0.0,
                0.0,
                900_000.0,
                860_000.0,
                cfg(),
            );
        }
        assert_eq!(snapshot.phase, RatingPhase::Download);
    }
}
