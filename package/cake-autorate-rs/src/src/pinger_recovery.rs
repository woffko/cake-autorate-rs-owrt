//! Bounded in-process pinger retries; independent controller/route state stays owned.
use super::{Config, Controller, PingerPlan, PingerRuntime};
use std::time::{Duration, Instant};

#[derive(Default)]
pub(crate) struct PingerRecovery {
    failures: usize,
    next_attempt: Option<Instant>,
    healthy_since: Option<Instant>,
    failure_since: Option<Instant>,
    contained: bool,
}

impl PingerRecovery {
    pub fn ready(&self, now: Instant) -> bool {
        self.next_attempt.is_none_or(|deadline| now >= deadline)
    }
    pub fn failed(&mut self, now: Instant) {
        const DELAYS_MS: [u64; 6] = [250, 1000, 2000, 5000, 10000, 60000];
        let delay = Duration::from_millis(DELAYS_MS[self.failures.min(5)]);
        self.failures = self.failures.saturating_add(1).min(6);
        self.next_attempt = now.checked_add(delay);
        self.healthy_since = None;
        self.failure_since.get_or_insert(now);
    }
    fn started(&mut self) {
        self.next_attempt = None;
        self.healthy_since = None;
    }
    pub fn sample_received(&mut self, now: Instant) {
        self.failure_since = None;
        self.contained = false;
        let since = *self.healthy_since.get_or_insert(now);
        if now.saturating_duration_since(since) >= Duration::from_secs(30) {
            self.failures = 0;
        }
    }
    pub fn recovering(&self) -> bool {
        self.failure_since.is_some()
    }
    pub fn containment_due(&mut self, now: Instant, timeout: Duration) -> bool {
        if !self.contained
            && self
                .failure_since
                .is_some_and(|start| now.saturating_duration_since(start) >= timeout)
        {
            self.contained = true;
            return true;
        }
        false
    }
}

pub(crate) fn try_spawn(
    cfg: &Config,
    active: &[String],
    plan: &mut PingerPlan,
    recovery: &mut PingerRecovery,
    controller: &mut Controller,
) -> Option<PingerRuntime> {
    if !recovery.ready(Instant::now()) {
        if recovery.recovering() {
            controller.set_run_state("RECOVERING");
        }
        return None;
    }
    let spawned = (|| {
        if !matches!(
            controller.uplink_state,
            super::UplinkState::Active | super::UplinkState::Standby | super::UplinkState::Learning
        ) || controller.route_snapshot.is_none()
        {
            return Err("pinger has no current route snapshot".to_string());
        }
        let producer = controller.permanent_probe_producer()?;
        controller
            .route_snapshot
            .as_ref()
            .ok_or_else(|| "pinger has no current route snapshot".to_string())
            .and_then(|route| PingerRuntime::spawn(cfg, active, plan, route, producer))
    })();
    match spawned {
        Ok(runtime) => {
            recovery.started();
            Some(runtime)
        }
        Err(error) => {
            recovery.failed(Instant::now());
            controller.note_pinger_failure(&error);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn r5_retry_backoff_is_bounded_and_one_reply_does_not_reset_the_failure_budget() {
        let now = Instant::now();
        let mut retry = PingerRecovery::default();
        assert!(retry.ready(now));
        let mut failed = now;
        for millis in [250, 1000, 2000, 5000, 10000, 60000, 60000] {
            retry.failed(failed);
            assert!(!retry.ready(failed + Duration::from_millis(millis - 1)));
            failed += Duration::from_millis(millis);
            assert!(retry.ready(failed));
            retry.started();
            retry.sample_received(failed); // A crash immediately after one reply.
        }
        retry.sample_received(failed + Duration::from_secs(30));
        retry.failed(failed + Duration::from_secs(31));
        assert!(retry.ready(failed + Duration::from_millis(31250)));
    }

    #[test]
    fn r5_repeated_spawns_do_not_postpone_prolonged_loss_containment() {
        let now = Instant::now();
        let mut retry = PingerRecovery::default();
        retry.failed(now);
        retry.started();
        retry.failed(now + Duration::from_secs(5));
        assert!(!retry.containment_due(now + Duration::from_secs(9), Duration::from_secs(10)));
        assert!(retry.containment_due(now + Duration::from_secs(10), Duration::from_secs(10)));
        assert!(!retry.containment_due(now + Duration::from_secs(11), Duration::from_secs(10)));
        retry.sample_received(now + Duration::from_secs(12));
        assert!(!retry.recovering());
    }
}
