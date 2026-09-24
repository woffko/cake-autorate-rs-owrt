//! Bounded cold-start baseline qualification; never learn from one RTT.
use std::time::{Duration, Instant};

pub(crate) struct BaselineWarmup {
    epoch: Instant,
    last_load: Option<Instant>,
    quiet_since: Option<Instant>,
    values: [[f64; 2]; 3],
    count: usize,
    next: usize,
}

impl BaselineWarmup {
    pub fn new(epoch: Instant) -> Self {
        Self {
            epoch,
            last_load: None,
            quiet_since: None,
            values: [[0.0; 2]; 3],
            count: 0,
            next: 0,
        }
    }

    pub fn observe(
        &mut self,
        observation: BaselineObservation,
        policy: BaselinePolicy,
    ) -> Option<[f64; 2]> {
        let BaselineObservation {
            owd_us,
            now,
            load_epoch,
            quiet,
        } = observation;
        let recent = load_epoch > self.epoch
            && load_epoch <= now
            && now.duration_since(load_epoch) <= policy.maximum_load_age;
        if !recent || !quiet || !owd_us.iter().all(|value| value.is_finite()) {
            self.quiet_since = None;
            self.count = 0;
            self.next = 0;
            return None;
        }
        // A recent cached counter frame may be used once, never once per
        // queued reply. Distinct monotonic counter epochs prove new load data.
        if self.last_load.is_some_and(|last| load_epoch <= last) {
            return None;
        }
        self.last_load = Some(load_epoch);
        let since = *self.quiet_since.get_or_insert(load_epoch);
        self.values[self.next] = owd_us;
        self.next = (self.next + 1) % self.values.len();
        self.count = (self.count + 1).min(self.values.len());
        if self.count != self.values.len() || now.duration_since(since) < policy.minimum_quiet_span
        {
            return None;
        }
        let mut median = [0.0; 2];
        for direction in 0..2 {
            let mut values = self.values.map(|pair| pair[direction]);
            values.sort_by(f64::total_cmp);
            if values[2] - values[0] > policy.maximum_spread_us[direction] {
                return None;
            }
            median[direction] = values[1];
        }
        Some(median)
    }
}

pub(crate) struct BaselineObservation {
    pub owd_us: [f64; 2],
    pub now: Instant,
    pub load_epoch: Instant,
    pub quiet: bool,
}

pub(crate) struct BaselinePolicy {
    pub maximum_load_age: Duration,
    pub minimum_quiet_span: Duration,
    pub maximum_spread_us: [f64; 2],
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn r5_cold_baseline_requires_distinct_recent_quiet_frames_and_rejects_jitter() {
        let start = Instant::now();
        let mut warmup = BaselineWarmup::new(start);
        let observe =
            |warmup: &mut BaselineWarmup, millis: u64, load: u64, quiet: bool, value: f64| {
                warmup.observe(
                    BaselineObservation {
                        owd_us: [value, value],
                        now: start + Duration::from_millis(millis),
                        load_epoch: start + Duration::from_millis(load),
                        quiet,
                    },
                    BaselinePolicy {
                        maximum_load_age: Duration::from_millis(200),
                        minimum_quiet_span: Duration::from_millis(600),
                        maximum_spread_us: [10_000.0; 2],
                    },
                )
            };
        assert!(observe(&mut warmup, 100, 100, true, 300_000.0).is_none());
        assert!(observe(&mut warmup, 150, 100, true, 300_000.0).is_none());
        assert_eq!(warmup.count, 1);
        assert!(observe(&mut warmup, 400, 400, true, 301_000.0).is_none());
        assert_eq!(
            observe(&mut warmup, 700, 700, true, 299_000.0),
            Some([300_000.0; 2])
        );
        assert!(observe(&mut warmup, 800, 800, false, 500_000.0).is_none());
        assert_eq!(warmup.count, 0);
        assert!(observe(&mut warmup, 1000, 1000, true, 300_000.0).is_none());
        assert!(observe(&mut warmup, 1300, 1300, true, 500_000.0).is_none());
        assert!(observe(&mut warmup, 1600, 1600, true, 300_000.0).is_none());
        assert!(observe(&mut warmup, 2000, 1700, true, 300_000.0).is_none());
        assert_eq!(warmup.count, 0);
        assert!(observe(&mut warmup, 2100, 0, true, 300_000.0).is_none());
    }
}
