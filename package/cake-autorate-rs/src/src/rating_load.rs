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

    pub fn from_capture(value: &str) -> Option<Self> {
        match value {
            "IDLE" => Some(Self::Idle),
            "DL" => Some(Self::Download),
            "UL" => Some(Self::Upload),
            _ => None,
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
    pub capture_min_enter_ratio: f64,
    pub capture_peak_factor: f64,
    pub capture_contamination_ratio: f64,
    pub capture_ack_ratio: f64,
}

#[derive(Clone, Debug)]
pub struct RatingLoadSnapshot {
    pub phase: RatingPhase,
    pub candidate: RatingPhase,
    pub raw_dl_percent: f64,
    pub raw_ul_percent: f64,
    pub smoothed_dl_percent: f64,
    pub smoothed_ul_percent: f64,
    pub aggregate_dl_rate_kbps: f64,
    pub aggregate_ul_rate_kbps: f64,
    pub effective_dl_rate_kbps: f64,
    pub effective_ul_rate_kbps: f64,
    pub reference_dl_kbps: f64,
    pub reference_ul_kbps: f64,
    pub enter_dl_percent: f64,
    pub enter_ul_percent: f64,
    pub exit_dl_percent: f64,
    pub exit_ul_percent: f64,
    pub enter_dl_kbps: f64,
    pub enter_ul_kbps: f64,
    pub phase_age_s: f64,
    pub capture_active: bool,
    pub capture_mode: &'static str,
    pub capture_requested_phase: &'static str,
    pub capture_background_dl_kbps: f64,
    pub capture_background_ul_kbps: f64,
    pub capture_peak_dl_percent: f64,
    pub capture_peak_ul_percent: f64,
    pub capture_contaminated: bool,
    pub capture_contamination_reason: &'static str,
}

#[derive(Clone, Copy, Debug)]
struct LoadPoint {
    at: Instant,
    dl_rate_kbps: f64,
    ul_rate_kbps: f64,
    dl_ratio: f64,
    ul_ratio: f64,
}

#[derive(Clone, Copy, Debug)]
struct LoadSignal {
    dl_ratio: f64,
    ul_ratio: f64,
    dl_rate_kbps: f64,
    ul_rate_kbps: f64,
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
    capture_requested_phase: Option<RatingPhase>,
    capture_background_dl_kbps: f64,
    capture_background_ul_kbps: f64,
    capture_peak_dl_ratio: f64,
    capture_peak_ul_ratio: f64,
    capture_contaminated: bool,
    capture_contamination_reason: &'static str,
    candidate_enter_dl_ratio: f64,
    candidate_enter_ul_ratio: f64,
    latched_exit_dl_ratio: f64,
    latched_exit_ul_ratio: f64,
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
            capture_requested_phase: None,
            capture_background_dl_kbps: 0.0,
            capture_background_ul_kbps: 0.0,
            capture_peak_dl_ratio: 0.0,
            capture_peak_ul_ratio: 0.0,
            capture_contaminated: false,
            capture_contamination_reason: "",
            candidate_enter_dl_ratio: 0.0,
            candidate_enter_ul_ratio: 0.0,
            latched_exit_dl_ratio: 0.0,
            latched_exit_ul_ratio: 0.0,
        }
    }

    pub fn set_capture(
        &mut self,
        token: Option<&str>,
        mode: Option<&str>,
        requested_phase: Option<RatingPhase>,
        background_dl_kbps: f64,
        background_ul_kbps: f64,
        now: Instant,
    ) -> bool {
        let token = token.unwrap_or("");
        let new_token = self.capture_token != token;
        let phase_changed = self.capture_requested_phase != requested_phase;
        if !new_token && !phase_changed {
            self.capture_background_dl_kbps = finite_nonnegative(background_dl_kbps);
            self.capture_background_ul_kbps = finite_nonnegative(background_ul_kbps);
            return false;
        }
        if new_token {
            self.capture_token.clear();
            self.capture_token.push_str(token);
            self.capture_mode.clear();
            self.capture_mode.push_str(mode.unwrap_or(""));
            self.capture_peak_dl_ratio = 0.0;
            self.capture_peak_ul_ratio = 0.0;
            self.capture_contaminated = false;
            self.capture_contamination_reason = "";
        }
        self.capture_requested_phase = requested_phase;
        self.capture_background_dl_kbps = finite_nonnegative(background_dl_kbps);
        self.capture_background_ul_kbps = finite_nonnegative(background_ul_kbps);
        self.samples.clear();
        self.transition(RatingPhase::Idle, now);
        new_token && !token.is_empty()
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
        let aggregate_dl_rate_kbps = finite_nonnegative(dl_rate_kbps);
        let aggregate_ul_rate_kbps = finite_nonnegative(ul_rate_kbps);
        let capture_active = !self.capture_token.is_empty();
        let dl_rate_kbps = if capture_active {
            (aggregate_dl_rate_kbps - self.capture_background_dl_kbps).max(0.0)
        } else {
            aggregate_dl_rate_kbps
        };
        let ul_rate_kbps = if capture_active {
            (aggregate_ul_rate_kbps - self.capture_background_ul_kbps).max(0.0)
        } else {
            aggregate_ul_rate_kbps
        };
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
        let (entry_dl_rate, entry_ul_rate, entry_dl, entry_ul) = self.window_peaks();
        if capture_active {
            self.capture_peak_dl_ratio = self.capture_peak_dl_ratio.max(smoothed_dl);
            self.capture_peak_ul_ratio = self.capture_peak_ul_ratio.max(smoothed_ul);
        }
        let (learned_enter_dl, learned_enter_ul) = self.learned_enter_thresholds(cfg);
        let (enter_dl, enter_ul) = if self.phase == RatingPhase::Idle
            && self.candidate != RatingPhase::Idle
            && self.candidate_enter_dl_ratio > 0.0
            && self.candidate_enter_ul_ratio > 0.0
        {
            (self.candidate_enter_dl_ratio, self.candidate_enter_ul_ratio)
        } else {
            (learned_enter_dl, learned_enter_ul)
        };
        let learned_exit_dl = exit_threshold(learned_enter_dl, capture_active, cfg);
        let learned_exit_ul = exit_threshold(learned_enter_ul, capture_active, cfg);
        let exit_dl = if self.phase.loaded() && self.latched_exit_dl_ratio > 0.0 {
            self.latched_exit_dl_ratio
        } else {
            learned_exit_dl
        };
        let exit_ul = if self.phase.loaded() && self.latched_exit_ul_ratio > 0.0 {
            self.latched_exit_ul_ratio
        } else {
            learned_exit_ul
        };

        let smoothed_signal = LoadSignal {
            dl_ratio: smoothed_dl,
            ul_ratio: smoothed_ul,
            dl_rate_kbps: smoothed_dl_rate,
            ul_rate_kbps: smoothed_ul_rate,
        };
        let entry_signal = LoadSignal {
            dl_ratio: entry_dl,
            ul_ratio: entry_ul,
            dl_rate_kbps: entry_dl_rate,
            ul_rate_kbps: entry_ul_rate,
        };
        self.detect_contamination(smoothed_signal, cfg);
        let target = self.classify_for_capture(entry_signal, enter_dl, enter_ul, cfg);
        let supported = classify(
            smoothed_signal,
            exit_dl,
            exit_ul,
            cfg.min_rate_kbps * 0.5,
            cfg.dominance_ratio,
        );

        self.advance(now, target, supported, enter_dl, enter_ul, cfg);
        RatingLoadSnapshot {
            phase: self.phase,
            candidate: self.candidate,
            raw_dl_percent: dl_ratio * 100.0,
            raw_ul_percent: ul_ratio * 100.0,
            smoothed_dl_percent: smoothed_dl * 100.0,
            smoothed_ul_percent: smoothed_ul * 100.0,
            aggregate_dl_rate_kbps,
            aggregate_ul_rate_kbps,
            effective_dl_rate_kbps: dl_rate_kbps,
            effective_ul_rate_kbps: ul_rate_kbps,
            reference_dl_kbps: finite_nonnegative(dl_shaper_kbps),
            reference_ul_kbps: finite_nonnegative(ul_shaper_kbps),
            enter_dl_percent: enter_dl * 100.0,
            enter_ul_percent: enter_ul * 100.0,
            exit_dl_percent: exit_dl * 100.0,
            exit_ul_percent: exit_ul * 100.0,
            enter_dl_kbps: enter_dl * finite_nonnegative(dl_shaper_kbps),
            enter_ul_kbps: enter_ul * finite_nonnegative(ul_shaper_kbps),
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
            capture_requested_phase: self.capture_requested_phase_name(),
            capture_background_dl_kbps: self.capture_background_dl_kbps,
            capture_background_ul_kbps: self.capture_background_ul_kbps,
            capture_peak_dl_percent: self.capture_peak_dl_ratio * 100.0,
            capture_peak_ul_percent: self.capture_peak_ul_ratio * 100.0,
            capture_contaminated: self.capture_contaminated,
            capture_contamination_reason: self.capture_contamination_reason,
        }
    }

    pub fn snapshot(
        &self,
        now: Instant,
        cfg: RatingLoadConfig,
        dl_shaper_kbps: f64,
        ul_shaper_kbps: f64,
    ) -> RatingLoadSnapshot {
        let (dl_rate, ul_rate, dl, ul) = self.smoothed();
        let (enter_dl, enter_ul) = self.learned_enter_thresholds(cfg);
        let capture_active = !self.capture_token.is_empty();
        let exit_dl = exit_threshold(enter_dl, capture_active, cfg);
        let exit_ul = exit_threshold(enter_ul, capture_active, cfg);
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
            aggregate_dl_rate_kbps: dl_rate + self.capture_background_dl_kbps,
            aggregate_ul_rate_kbps: ul_rate + self.capture_background_ul_kbps,
            effective_dl_rate_kbps: dl_rate,
            effective_ul_rate_kbps: ul_rate,
            reference_dl_kbps: finite_nonnegative(dl_shaper_kbps),
            reference_ul_kbps: finite_nonnegative(ul_shaper_kbps),
            enter_dl_percent: enter_dl * 100.0,
            enter_ul_percent: enter_ul * 100.0,
            exit_dl_percent: exit_dl * 100.0,
            exit_ul_percent: exit_ul * 100.0,
            enter_dl_kbps: enter_dl * finite_nonnegative(dl_shaper_kbps),
            enter_ul_kbps: enter_ul * finite_nonnegative(ul_shaper_kbps),
            phase_age_s: now
                .saturating_duration_since(self.phase_since)
                .as_secs_f64(),
            capture_active,
            capture_mode: if !capture_active {
                "passive"
            } else if self.capture_mode == "automatic" {
                "automatic"
            } else {
                "client"
            },
            capture_requested_phase: self.capture_requested_phase_name(),
            capture_background_dl_kbps: self.capture_background_dl_kbps,
            capture_background_ul_kbps: self.capture_background_ul_kbps,
            capture_peak_dl_percent: self.capture_peak_dl_ratio * 100.0,
            capture_peak_ul_percent: self.capture_peak_ul_ratio * 100.0,
            capture_contaminated: self.capture_contaminated,
            capture_contamination_reason: self.capture_contamination_reason,
        }
    }

    fn learned_enter_thresholds(&self, cfg: RatingLoadConfig) -> (f64, f64) {
        if self.capture_token.is_empty() {
            return (cfg.enter_ratio, cfg.enter_ratio);
        }
        (
            learned_capture_threshold(self.capture_peak_dl_ratio, cfg),
            learned_capture_threshold(self.capture_peak_ul_ratio, cfg),
        )
    }

    fn capture_requested_phase_name(&self) -> &'static str {
        match self.capture_requested_phase {
            None => "AUTO",
            Some(phase) => phase.as_str(),
        }
    }

    fn classify_for_capture(
        &self,
        signal: LoadSignal,
        dl_threshold: f64,
        ul_threshold: f64,
        cfg: RatingLoadConfig,
    ) -> RatingPhase {
        if self.capture_contaminated {
            return RatingPhase::Idle;
        }
        match self.capture_requested_phase {
            Some(RatingPhase::Idle) => RatingPhase::Idle,
            Some(RatingPhase::Download) => {
                if signal.dl_ratio >= dl_threshold && signal.dl_rate_kbps >= cfg.min_rate_kbps {
                    RatingPhase::Download
                } else {
                    RatingPhase::Idle
                }
            }
            Some(RatingPhase::Upload) => {
                if signal.ul_ratio >= ul_threshold && signal.ul_rate_kbps >= cfg.min_rate_kbps {
                    RatingPhase::Upload
                } else {
                    RatingPhase::Idle
                }
            }
            Some(RatingPhase::Bidirectional) | None => classify(
                signal,
                dl_threshold,
                ul_threshold,
                cfg.min_rate_kbps,
                cfg.dominance_ratio,
            ),
        }
    }

    fn detect_contamination(&mut self, signal: LoadSignal, cfg: RatingLoadConfig) {
        if self.capture_token.is_empty() || self.capture_contaminated {
            return;
        }
        let min_rate = cfg.min_rate_kbps * 0.5;
        match self.capture_requested_phase {
            Some(RatingPhase::Download)
                if signal.dl_ratio >= cfg.capture_min_enter_ratio
                    && signal.dl_rate_kbps >= cfg.min_rate_kbps
                    && signal.ul_ratio >= cfg.capture_contamination_ratio
                    && signal.ul_rate_kbps >= min_rate
                    && signal.ul_rate_kbps > signal.dl_rate_kbps * cfg.capture_ack_ratio =>
            {
                self.capture_contaminated = true;
                self.capture_contamination_reason = "unexpected_upload_during_download";
            }
            Some(RatingPhase::Upload)
                if signal.ul_ratio >= cfg.capture_min_enter_ratio
                    && signal.ul_rate_kbps >= cfg.min_rate_kbps
                    && signal.dl_ratio >= cfg.capture_contamination_ratio
                    && signal.dl_rate_kbps >= min_rate
                    && signal.dl_rate_kbps > signal.ul_rate_kbps * cfg.capture_ack_ratio =>
            {
                self.capture_contaminated = true;
                self.capture_contamination_reason = "unexpected_download_during_upload";
            }
            _ => {}
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

    fn window_peaks(&self) -> (f64, f64, f64, f64) {
        self.samples.iter().fold(
            (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64),
            |(dl_rate, ul_rate, dl_ratio, ul_ratio), sample| {
                (
                    dl_rate.max(sample.dl_rate_kbps),
                    ul_rate.max(sample.ul_rate_kbps),
                    dl_ratio.max(sample.dl_ratio),
                    ul_ratio.max(sample.ul_ratio),
                )
            },
        )
    }

    fn advance(
        &mut self,
        now: Instant,
        target: RatingPhase,
        supported: RatingPhase,
        enter_dl: f64,
        enter_ul: f64,
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
            self.candidate_enter_dl_ratio = enter_dl;
            self.candidate_enter_ul_ratio = enter_ul;
            return;
        }
        if now.saturating_duration_since(self.candidate_since) >= cfg.hold {
            if target.loaded() {
                let capture_active = !self.capture_token.is_empty();
                self.latched_exit_dl_ratio =
                    exit_threshold(self.candidate_enter_dl_ratio, capture_active, cfg);
                self.latched_exit_ul_ratio =
                    exit_threshold(self.candidate_enter_ul_ratio, capture_active, cfg);
            }
            self.transition(target, now);
        }
    }

    fn transition(&mut self, phase: RatingPhase, now: Instant) {
        self.phase = phase;
        self.phase_since = now;
        if phase == RatingPhase::Idle {
            self.latched_exit_dl_ratio = 0.0;
            self.latched_exit_ul_ratio = 0.0;
        }
        self.clear_candidate(now);
        self.unsupported_since = None;
    }

    fn clear_candidate(&mut self, now: Instant) {
        self.candidate = self.phase;
        self.candidate_since = now;
        self.candidate_enter_dl_ratio = 0.0;
        self.candidate_enter_ul_ratio = 0.0;
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

fn learned_capture_threshold(peak: f64, cfg: RatingLoadConfig) -> f64 {
    cfg.enter_ratio
        .min((peak.clamp(0.0, 1.5) * cfg.capture_peak_factor).max(cfg.capture_min_enter_ratio))
}

fn exit_threshold(enter: f64, capture_active: bool, cfg: RatingLoadConfig) -> f64 {
    if capture_active {
        cfg.exit_ratio
            .min((enter * 0.67).max(cfg.capture_min_enter_ratio * 0.67))
    } else {
        cfg.exit_ratio
    }
}

fn classify(
    signal: LoadSignal,
    dl_threshold: f64,
    ul_threshold: f64,
    min_rate: f64,
    dominance: f64,
) -> RatingPhase {
    let dl = signal.dl_ratio >= dl_threshold && signal.dl_rate_kbps >= min_rate;
    let ul = signal.ul_ratio >= ul_threshold && signal.ul_rate_kbps >= min_rate;
    match (dl, ul) {
        (false, false) => RatingPhase::Idle,
        (true, false) => RatingPhase::Download,
        (false, true) => RatingPhase::Upload,
        (true, true) if signal.dl_ratio >= signal.ul_ratio * dominance => RatingPhase::Download,
        (true, true) if signal.ul_ratio >= signal.dl_ratio * dominance => RatingPhase::Upload,
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
            capture_min_enter_ratio: 0.15,
            capture_peak_factor: 0.35,
            capture_contamination_ratio: 0.10,
            capture_ack_ratio: 0.08,
        }
    }

    fn feed(
        detector: &mut RatingLoadDetector,
        start: Instant,
        rates: &[(f64, f64)],
    ) -> RatingLoadSnapshot {
        let mut snapshot = detector.snapshot(start, cfg(), 900_000.0, 860_000.0);
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
    fn passive_browser_download_uses_the_window_peak_for_entry() {
        let start = Instant::now();
        let mut detector = RatingLoadDetector::new(start);
        let rates = [
            310_000.0, 340_000.0, 734_000.0, 358_000.0, 310_000.0, 370_000.0, 345_000.0, 330_000.0,
            390_000.0, 360_000.0,
        ];
        let mut snapshot = detector.snapshot(start, cfg(), 900_000.0, 860_000.0);
        for (index, dl) in rates.iter().cycle().take(30).enumerate() {
            snapshot = detector.observe(
                start + Duration::from_millis(index as u64 * 200),
                *dl,
                8_000.0,
                900_000.0,
                860_000.0,
                cfg(),
            );
        }
        assert_eq!(snapshot.phase, RatingPhase::Download);
        assert!(snapshot.smoothed_dl_percent < 60.0);
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
        detector.set_capture(Some("token-1"), Some("client"), None, 0.0, 0.0, start);
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
        let mut snapshot = detector.snapshot(later, cfg(), 900_000.0, 860_000.0);
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

    #[test]
    fn capture_learns_download_and_upload_thresholds_independently() {
        let start = Instant::now();
        let mut detector = RatingLoadDetector::new(start);
        detector.set_capture(
            Some("token-independent"),
            Some("client"),
            None,
            0.0,
            0.0,
            start,
        );

        let download = vec![(810_000.0, 20_000.0); 20];
        assert_eq!(
            feed(&mut detector, start, &download).phase,
            RatingPhase::Download
        );
        let idle_start = start + Duration::from_secs(5);
        for index in 0..20 {
            detector.observe(
                idle_start + Duration::from_millis(index * 200),
                0.0,
                0.0,
                900_000.0,
                860_000.0,
                cfg(),
            );
        }
        let upload_start = idle_start + Duration::from_secs(5);
        let mut snapshot = detector.snapshot(upload_start, cfg(), 900_000.0, 860_000.0);
        for index in 0..20 {
            snapshot = detector.observe(
                upload_start + Duration::from_millis(index * 200),
                10_000.0,
                215_000.0,
                900_000.0,
                860_000.0,
                cfg(),
            );
        }
        assert_eq!(snapshot.phase, RatingPhase::Upload);
        assert!(snapshot.enter_dl_percent > snapshot.enter_ul_percent);
    }

    #[test]
    fn capture_freezes_candidate_threshold_across_a_bursty_browser_phase() {
        let start = Instant::now();
        let mut detector = RatingLoadDetector::new(start);
        detector.set_capture(Some("token-bursty"), Some("client"), None, 0.0, 0.0, start);
        let rates = [
            310_000.0, 340_000.0, 734_000.0, 358_000.0, 310_000.0, 370_000.0, 345_000.0, 330_000.0,
            390_000.0, 360_000.0,
        ];
        let mut snapshot = detector.snapshot(start, cfg(), 900_000.0, 860_000.0);
        for (index, dl) in rates.iter().cycle().take(30).enumerate() {
            snapshot = detector.observe(
                start + Duration::from_millis(index as u64 * 200),
                *dl,
                8_000.0,
                900_000.0,
                860_000.0,
                cfg(),
            );
        }
        assert_eq!(snapshot.phase, RatingPhase::Download);
        assert!(snapshot.capture_peak_dl_percent > 50.0);
    }

    #[test]
    fn capture_subtracts_measured_background_before_classification() {
        let start = Instant::now();
        let mut detector = RatingLoadDetector::new(start);
        detector.set_capture(
            Some("token-background"),
            Some("client"),
            None,
            100_000.0,
            20_000.0,
            start,
        );
        let trace = vec![(460_000.0, 30_000.0); 20];
        let snapshot = feed(&mut detector, start, &trace);
        assert_eq!(snapshot.phase, RatingPhase::Download);
        assert!((snapshot.aggregate_dl_rate_kbps - 460_000.0).abs() < 1.0);
        assert!((snapshot.effective_dl_rate_kbps - 360_000.0).abs() < 1.0);
        assert!((snapshot.capture_background_dl_kbps - 100_000.0).abs() < 1.0);
    }

    #[test]
    fn forced_download_rejects_unexpected_upload_contamination() {
        let start = Instant::now();
        let mut detector = RatingLoadDetector::new(start);
        detector.set_capture(
            Some("token-contamination"),
            Some("automatic"),
            Some(RatingPhase::Download),
            0.0,
            0.0,
            start,
        );
        let trace = vec![(800_000.0, 120_000.0); 20];
        let snapshot = feed(&mut detector, start, &trace);
        assert_eq!(snapshot.phase, RatingPhase::Idle);
        assert!(snapshot.capture_contaminated);
        assert_eq!(
            snapshot.capture_contamination_reason,
            "unexpected_upload_during_download"
        );
    }

    #[test]
    fn forced_download_allows_expected_tcp_ack_traffic_on_asymmetric_link() {
        let start = Instant::now();
        let mut detector = RatingLoadDetector::new(start);
        detector.set_capture(
            Some("token-acks"),
            Some("automatic"),
            Some(RatingPhase::Download),
            0.0,
            0.0,
            start,
        );
        let mut snapshot = detector.snapshot(start, cfg(), 85_000.0, 10_000.0);
        for index in 0..30 {
            snapshot = detector.observe(
                start + Duration::from_millis(index * 200),
                44_000.0,
                1_100.0,
                85_000.0,
                10_000.0,
                cfg(),
            );
        }
        assert_eq!(snapshot.phase, RatingPhase::Download);
        assert!(!snapshot.capture_contaminated);
    }

    #[test]
    fn explicit_idle_marker_stops_a_completed_upload_phase() {
        let start = Instant::now();
        let mut detector = RatingLoadDetector::new(start);
        detector.set_capture(
            Some("token-phases"),
            Some("automatic"),
            Some(RatingPhase::Upload),
            0.0,
            0.0,
            start,
        );
        let upload = vec![(10_000.0, 800_000.0); 20];
        assert_eq!(
            feed(&mut detector, start, &upload).phase,
            RatingPhase::Upload
        );
        let idle_at = start + Duration::from_secs(5);
        detector.set_capture(
            Some("token-phases"),
            Some("automatic"),
            Some(RatingPhase::Idle),
            0.0,
            0.0,
            idle_at,
        );
        let snapshot = detector.observe(
            idle_at + Duration::from_millis(200),
            10_000.0,
            800_000.0,
            900_000.0,
            860_000.0,
            cfg(),
        );
        assert_eq!(snapshot.phase, RatingPhase::Idle);
        assert_eq!(snapshot.capture_requested_phase, "IDLE");
    }
}
