//! Counter-based history averages; peaks are maxima of fresh counter intervals.
use std::time::Instant;

#[derive(Clone, Copy)]
struct Observation {
    at: Instant,
    rx: u64,
    tx: u64,
}

#[derive(Default)]
pub(crate) struct Window {
    first: Option<Observation>,
    last: Option<Observation>,
    peak_dl: f64,
    peak_ul: f64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Summary {
    pub average_dl_kbps: f64,
    pub average_ul_kbps: f64,
    pub peak_dl_kbps: f64,
    pub peak_ul_kbps: f64,
    pub observed_seconds: f64,
}

impl Window {
    pub(crate) fn observe(&mut self, at: Instant, rx: u64, tx: u64) {
        let current = Observation { at, rx, tx };
        if let Some(previous) = self.last {
            if at <= previous.at {
                return;
            }
            if rx < previous.rx || tx < previous.tx {
                *self = Self::default();
            } else {
                let seconds = at.duration_since(previous.at).as_secs_f64();
                self.peak_dl = self
                    .peak_dl
                    .max((rx - previous.rx) as f64 * 0.008 / seconds);
                self.peak_ul = self
                    .peak_ul
                    .max((tx - previous.tx) as f64 * 0.008 / seconds);
            }
        }
        if self.first.is_none() {
            self.first = Some(current);
        }
        self.last = Some(current);
    }

    pub(crate) fn finish(&mut self) -> Option<Summary> {
        let first = self.first?;
        let last = self.last?;
        if last.at <= first.at {
            return None;
        }
        let observed_seconds = last.at.duration_since(first.at).as_secs_f64();
        let summary = Summary {
            average_dl_kbps: (last.rx - first.rx) as f64 * 0.008 / observed_seconds,
            average_ul_kbps: (last.tx - first.tx) as f64 * 0.008 / observed_seconds,
            peak_dl_kbps: self.peak_dl,
            peak_ul_kbps: self.peak_ul,
            observed_seconds,
        };
        self.first = Some(last);
        self.peak_dl = 0.0;
        self.peak_ul = 0.0;
        Some(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn r7_history_preserves_bursts_with_time_weighted_average_and_separate_peak() {
        let now = Instant::now();
        let mut window = Window::default();
        window.observe(now, 0, 0);
        window.observe(now + Duration::from_secs(1), 125_000, 0);
        // Cached/out-of-order calls must not change either counters or peaks.
        window.observe(now + Duration::from_secs(1), u64::MAX, u64::MAX);
        window.observe(now, u64::MAX, u64::MAX);
        window.observe(now + Duration::from_secs(10), 125_000, 125_000);
        let result = window.finish().unwrap();
        assert_eq!(result.average_dl_kbps, 100.0);
        assert_eq!(result.average_ul_kbps, 100.0);
        assert_eq!(result.peak_dl_kbps, 1000.0);
        assert!((result.peak_ul_kbps - 1000.0 / 9.0).abs() < 0.001);
        assert_eq!(result.observed_seconds, 10.0);
        assert!(window.finish().is_none());
        window.observe(now + Duration::from_secs(20), 125_000, 125_000);
        let quiet = window.finish().unwrap();
        assert_eq!(quiet.average_dl_kbps, 0.0);
        assert_eq!(quiet.peak_dl_kbps, 0.0);
    }

    #[test]
    fn r7_history_reset_or_missing_observations_never_fabricate_zero_or_huge_rates() {
        let now = Instant::now();
        let mut window = Window::default();
        assert!(window.finish().is_none());
        window.observe(now, 9000, 9000);
        assert!(window.finish().is_none());
        window.observe(now + Duration::from_secs(1), 0, 0);
        assert!(window.finish().is_none());
        window.observe(now + Duration::from_secs(3), 250_000, 0);
        assert_eq!(window.finish().unwrap().average_dl_kbps, 1000.0);
    }
}
