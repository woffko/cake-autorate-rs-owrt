use std::collections::{HashMap, VecDeque};

use crate::transport_quality::{classify_quality, QualityClass};

pub const MIN_BASELINE_SAMPLES: usize = 20;
pub const MIN_LOADED_SAMPLES: usize = 20;
const BASELINE_WINDOW: usize = 120;

#[derive(Clone, Debug)]
pub struct QualityGradeMetric {
    pub class: QualityClass,
    pub increase_ms: f64,
    pub loaded_p90_ms: f64,
    pub samples: usize,
}

#[derive(Clone, Debug)]
pub struct QualityGradeResult {
    pub class: QualityClass,
    pub increase_ms: f64,
    pub baseline_p5_ms: f64,
    pub endpoint: String,
    pub started_at: f64,
    pub completed_at: Option<f64>,
    pub route_identity: String,
    pub partial: bool,
    pub incomplete: bool,
    pub dl_samples: usize,
    pub ul_samples: usize,
    pub bidirectional_samples: usize,
    pub completion_reason: String,
    pub dl: Option<QualityGradeMetric>,
    pub ul: Option<QualityGradeMetric>,
    pub bidirectional: Option<QualityGradeMetric>,
}

impl QualityGradeResult {
    pub fn samples(&self) -> usize {
        self.dl_samples + self.ul_samples + self.bidirectional_samples
    }
}

#[derive(Clone, Debug)]
pub struct QualityGradeSnapshot {
    pub state: &'static str,
    pub current: Option<QualityGradeResult>,
    pub previous: Option<QualityGradeResult>,
    pub collected_samples: usize,
    pub required_samples: usize,
    pub baseline_samples: usize,
    pub baseline_required_samples: usize,
    pub dl_samples: usize,
    pub ul_samples: usize,
    pub bidirectional_samples: usize,
    pub finalize_remaining_s: Option<f64>,
    pub baseline_ready: bool,
    pub current_stale: bool,
    pub previous_stale: bool,
}

#[derive(Clone, Debug)]
struct ActiveWindow {
    endpoint: Option<String>,
    baseline_p5_ms: Option<f64>,
    started_at: f64,
    last_loaded_at: f64,
    route_identity: String,
    dl: Vec<f64>,
    ul: Vec<f64>,
    bidirectional: Vec<f64>,
}

impl ActiveWindow {
    fn new(started_at: f64, route_identity: &str) -> Self {
        Self {
            endpoint: None,
            baseline_p5_ms: None,
            started_at,
            last_loaded_at: started_at,
            route_identity: route_identity.to_string(),
            dl: Vec::new(),
            ul: Vec::new(),
            bidirectional: Vec::new(),
        }
    }

    fn samples(&self) -> usize {
        self.dl.len() + self.ul.len() + self.bidirectional.len()
    }

    fn result(&self, completed_at: Option<f64>) -> Option<QualityGradeResult> {
        let endpoint = self.endpoint.as_ref()?;
        let baseline_p5_ms = self.baseline_p5_ms?;
        let dl = metric(&self.dl, baseline_p5_ms);
        let ul = metric(&self.ul, baseline_p5_ms);
        let bidirectional = metric(&self.bidirectional, baseline_p5_ms);

        let (class, increase_ms, partial, incomplete, completion_reason) = match (&dl, &ul) {
            (Some(dl), Some(ul)) => {
                if quality_rank(dl.class) <= quality_rank(ul.class) {
                    (
                        dl.class,
                        dl.increase_ms.max(ul.increase_ms),
                        false,
                        false,
                        "complete",
                    )
                } else {
                    (
                        ul.class,
                        dl.increase_ms.max(ul.increase_ms),
                        false,
                        false,
                        "complete",
                    )
                }
            }
            (Some(dl), None) => (dl.class, dl.increase_ms, true, false, "upload_incomplete"),
            (None, Some(ul)) => (ul.class, ul.increase_ms, true, false, "download_incomplete"),
            (None, None) => (
                QualityClass::Learning,
                0.0,
                false,
                true,
                "insufficient_directional_samples",
            ),
        };

        Some(QualityGradeResult {
            class,
            increase_ms,
            baseline_p5_ms,
            endpoint: endpoint.clone(),
            started_at: self.started_at,
            completed_at,
            route_identity: self.route_identity.clone(),
            partial,
            incomplete,
            dl_samples: self.dl.len(),
            ul_samples: self.ul.len(),
            bidirectional_samples: self.bidirectional.len(),
            completion_reason: completion_reason.to_string(),
            dl,
            ul,
            bidirectional,
        })
    }
}

#[derive(Clone, Debug)]
pub struct QualityGradeTracker {
    baselines: HashMap<String, VecDeque<f64>>,
    active: Option<ActiveWindow>,
    latest: Option<QualityGradeResult>,
    previous: Option<QualityGradeResult>,
    route_identity: String,
    session_grace_s: f64,
}

impl QualityGradeTracker {
    pub fn new(session_grace_s: f64) -> Self {
        Self {
            baselines: HashMap::new(),
            active: None,
            latest: None,
            previous: None,
            route_identity: String::new(),
            session_grace_s: session_grace_s.max(1.0),
        }
    }

    pub fn set_route(&mut self, route_identity: &str) {
        if self.route_identity == route_identity {
            return;
        }
        self.route_identity = route_identity.to_string();
        self.baselines.clear();
        self.active = None;
    }

    pub fn observe(
        &mut self,
        endpoint: &str,
        latency_ms: f64,
        dl_loaded: bool,
        ul_loaded: bool,
        timestamp: f64,
        route_identity: &str,
    ) {
        if !latency_ms.is_finite() || latency_ms <= 0.0 || !timestamp.is_finite() {
            return;
        }
        self.set_route(route_identity);

        if !dl_loaded && !ul_loaded {
            let samples = self.baselines.entry(endpoint.to_string()).or_default();
            samples.push_back(latency_ms);
            while samples.len() > BASELINE_WINDOW {
                samples.pop_front();
            }
            if self
                .active
                .as_ref()
                .map(|active| timestamp - active.last_loaded_at >= self.session_grace_s)
                .unwrap_or(false)
            {
                self.finish_active(timestamp);
            }
            return;
        }

        if self
            .active
            .as_ref()
            .map(|active| timestamp - active.last_loaded_at >= self.session_grace_s)
            .unwrap_or(false)
        {
            self.finish_active(timestamp);
        }

        let baseline = self.baseline_p5(endpoint);
        let active = self
            .active
            .get_or_insert_with(|| ActiveWindow::new(timestamp, route_identity));
        if active.endpoint.is_none() {
            if let Some(baseline_p5_ms) = baseline {
                active.endpoint = Some(endpoint.to_string());
                active.baseline_p5_ms = Some(baseline_p5_ms);
            }
        }
        if active.endpoint.as_deref() != Some(endpoint) {
            return;
        }
        active.last_loaded_at = timestamp;

        if dl_loaded && ul_loaded {
            active.bidirectional.push(latency_ms);
        } else if dl_loaded {
            active.dl.push(latency_ms);
        } else {
            active.ul.push(latency_ms);
        }
    }

    pub fn snapshot(&self, now: f64) -> QualityGradeSnapshot {
        let baseline_samples = self
            .baselines
            .values()
            .map(VecDeque::len)
            .max()
            .unwrap_or(0);
        let baseline_ready = self
            .baselines
            .values()
            .any(|samples| samples.len() >= MIN_BASELINE_SAMPLES);
        let (state, current, previous, collected_samples) = if let Some(active) = &self.active {
            let current = active.result(None);
            let state = if current.is_some() {
                "provisional"
            } else {
                "collecting"
            };
            (state, current, self.latest.clone(), active.samples())
        } else if let Some(latest) = &self.latest {
            ("final", Some(latest.clone()), self.previous.clone(), 0)
        } else if baseline_ready {
            ("baseline_ready", None, None, 0)
        } else {
            ("learning_baseline", None, None, 0)
        };
        let current_stale = current
            .as_ref()
            .map(|result| result.route_identity != self.route_identity)
            .unwrap_or(false);
        let previous_stale = previous
            .as_ref()
            .map(|result| result.route_identity != self.route_identity)
            .unwrap_or(false);
        let (dl_samples, ul_samples, bidirectional_samples, finalize_remaining_s) = self
            .active
            .as_ref()
            .map(|active| {
                (
                    active.dl.len(),
                    active.ul.len(),
                    active.bidirectional.len(),
                    Some((self.session_grace_s - (now - active.last_loaded_at)).max(0.0)),
                )
            })
            .unwrap_or((0, 0, 0, None));

        QualityGradeSnapshot {
            state,
            current,
            previous,
            collected_samples,
            required_samples: MIN_LOADED_SAMPLES,
            baseline_samples,
            baseline_required_samples: MIN_BASELINE_SAMPLES,
            dl_samples,
            ul_samples,
            bidirectional_samples,
            finalize_remaining_s,
            baseline_ready,
            current_stale,
            previous_stale,
        }
    }

    fn finish_active(&mut self, timestamp: f64) {
        let Some(active) = self.active.take() else {
            return;
        };
        if active.samples() == 0 {
            return;
        }
        let Some(result) = active.result(Some(timestamp)) else {
            return;
        };
        self.previous = self.latest.replace(result);
    }

    fn baseline_p5(&self, endpoint: &str) -> Option<f64> {
        let samples = self.baselines.get(endpoint)?;
        if samples.len() < MIN_BASELINE_SAMPLES {
            return None;
        }
        percentile(samples.iter().copied(), 5.0)
    }
}

fn metric(samples: &[f64], baseline_p5_ms: f64) -> Option<QualityGradeMetric> {
    if samples.len() < MIN_LOADED_SAMPLES {
        return None;
    }
    let loaded_p90_ms = percentile(samples.iter().copied(), 90.0)?;
    let raw_increase = loaded_p90_ms - baseline_p5_ms;
    let increase_ms = if raw_increase.abs() < 2.0 {
        0.0
    } else {
        raw_increase.max(0.0)
    };
    Some(QualityGradeMetric {
        class: classify_quality(Some(increase_ms)),
        increase_ms,
        loaded_p90_ms,
        samples: samples.len(),
    })
}

fn percentile<I>(values: I, percentile: f64) -> Option<f64>
where
    I: IntoIterator<Item = f64>,
{
    let mut values = values
        .into_iter()
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let index = (percentile.clamp(0.0, 100.0) / 100.0) * (values.len() - 1) as f64;
    let lower = index.floor() as usize;
    let upper = index.ceil() as usize;
    if lower == upper {
        return values.get(lower).copied();
    }
    let weight = index - lower as f64;
    Some(values[lower] * (1.0 - weight) + values[upper] * weight)
}

fn quality_rank(class: QualityClass) -> u8 {
    match class {
        QualityClass::Learning => 0,
        QualityClass::F => 1,
        QualityClass::D => 2,
        QualityClass::C => 3,
        QualityClass::B => 4,
        QualityClass::A => 5,
        QualityClass::APlus => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_baseline(tracker: &mut QualityGradeTracker, endpoint: &str, route: &str) {
        for index in 0..MIN_BASELINE_SAMPLES {
            tracker.observe(
                endpoint,
                10.0 + (index % 3) as f64,
                false,
                false,
                1.0 + index as f64,
                route,
            );
        }
    }

    #[test]
    fn current_and_previous_survive_the_next_collection_window() {
        let mut tracker = QualityGradeTracker::new(30.0);
        seed_baseline(&mut tracker, "endpoint", "route-a");
        for index in 0..MIN_LOADED_SAMPLES {
            tracker.observe(
                "endpoint",
                12.0 + (index % 3) as f64,
                true,
                false,
                30.0 + index as f64,
                "route-a",
            );
        }
        let live = tracker.snapshot(50.0);
        assert_eq!(live.state, "provisional");
        assert_eq!(live.current.as_ref().unwrap().class, QualityClass::APlus);
        assert!(live.previous.is_none());

        tracker.observe("endpoint", 11.0, false, false, 85.0, "route-a");
        let finished = tracker.snapshot(90.0);
        assert_eq!(finished.state, "final");
        assert!(finished.current.as_ref().unwrap().completed_at.is_some());

        tracker.observe("endpoint", 80.0, false, true, 100.0, "route-a");
        let collecting = tracker.snapshot(110.0);
        assert_eq!(collecting.state, "provisional");
        assert!(collecting.current.as_ref().unwrap().incomplete);
        assert_eq!(
            collecting.previous.as_ref().unwrap().class,
            QualityClass::APlus
        );
    }

    #[test]
    fn bidirectional_is_diagnostic_and_does_not_set_overall_grade() {
        let mut tracker = QualityGradeTracker::new(30.0);
        seed_baseline(&mut tracker, "endpoint", "route-a");
        for index in 0..MIN_LOADED_SAMPLES {
            tracker.observe(
                "endpoint",
                500.0,
                true,
                true,
                30.0 + index as f64,
                "route-a",
            );
        }
        let snapshot = tracker.snapshot(100.0);
        assert_eq!(snapshot.state, "provisional");
        assert!(snapshot.current.as_ref().unwrap().incomplete);
        assert_eq!(snapshot.bidirectional_samples, MIN_LOADED_SAMPLES);
    }

    #[test]
    fn overall_grade_is_the_worse_of_download_and_upload() {
        let active = ActiveWindow {
            endpoint: Some("endpoint".to_string()),
            baseline_p5_ms: Some(10.0),
            started_at: 1.0,
            last_loaded_at: 20.0,
            route_identity: "route-a".to_string(),
            dl: vec![12.0; MIN_LOADED_SAMPLES],
            ul: vec![100.0; MIN_LOADED_SAMPLES],
            bidirectional: Vec::new(),
        };
        let result = active.result(Some(20.0)).unwrap();
        assert_eq!(result.dl.as_ref().unwrap().class, QualityClass::APlus);
        assert_eq!(result.ul.as_ref().unwrap().class, QualityClass::C);
        assert_eq!(result.class, QualityClass::C);
        assert!(!result.partial);
    }

    #[test]
    fn route_change_keeps_last_result_but_marks_it_stale() {
        let mut tracker = QualityGradeTracker::new(30.0);
        seed_baseline(&mut tracker, "endpoint", "route-a");
        for index in 0..MIN_LOADED_SAMPLES {
            tracker.observe(
                "endpoint",
                12.0,
                true,
                false,
                30.0 + index as f64,
                "route-a",
            );
        }
        tracker.observe("endpoint", 11.0, false, false, 85.0, "route-a");
        tracker.set_route("route-b");
        let snapshot = tracker.snapshot(100.0);
        assert!(snapshot.current_stale);
        assert!(!snapshot.baseline_ready);
    }

    #[test]
    fn tiny_delta_is_clamped_and_percentiles_are_interpolated() {
        let metric = metric(&[11.0; MIN_LOADED_SAMPLES], 11.0).unwrap();
        assert_eq!(metric.increase_ms, 0.0);
        assert_eq!(metric.class, QualityClass::APlus);
        assert_eq!(percentile([0.0, 10.0], 90.0), Some(9.0));
    }
}
