use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdaptiveCeilingPhase {
    Cruise,
    Qualify,
    ProbeRamp,
    ProbeObserve,
    Backoff,
}

impl AdaptiveCeilingPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cruise => "cruise",
            Self::Qualify => "qualify",
            Self::ProbeRamp => "probe_ramp",
            Self::ProbeObserve => "probe_observe",
            Self::Backoff => "backoff",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AdaptiveCeilingChange {
    Raised { from_kbps: f64, to_kbps: f64 },
    Lowered { from_kbps: f64, to_kbps: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdaptiveCeilingTransition {
    pub from: AdaptiveCeilingPhase,
    pub to: AdaptiveCeilingPhase,
    pub reason: &'static str,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AdaptiveCeilingUpdate {
    pub change: Option<AdaptiveCeilingChange>,
    pub transition: Option<AdaptiveCeilingTransition>,
}

#[derive(Clone, Copy, Debug)]
pub struct AdaptiveCeilingPolicy {
    pub hold_time: Duration,
    pub probe_step_percent: f64,
    pub probe_duration: Duration,
    pub cooldown: Duration,
    pub failed_bound_ttl: Duration,
    pub eligibility_grace: Duration,
}

#[derive(Clone, Copy, Debug)]
pub struct AdaptiveCeilingObservation {
    pub eligible: bool,
    pub bufferbloat: bool,
    pub shaper_rate_kbps: f64,
}

#[derive(Clone, Debug)]
pub struct AdaptiveCeilingDirection {
    configured_max_kbps: f64,
    effective_max_kbps: f64,
    absolute_cap_kbps: f64,
    safe_ceiling_kbps: f64,
    failed_ceiling_kbps: Option<f64>,
    failed_at: Option<Instant>,
    probe_target_kbps: Option<f64>,
    phase: AdaptiveCeilingPhase,
    phase_since: Instant,
    last_eligible_at: Option<Instant>,
    last_transition_reason: &'static str,
}

impl AdaptiveCeilingDirection {
    pub fn new(configured_max_kbps: f64, absolute_cap_kbps: f64) -> Self {
        Self::new_at(configured_max_kbps, absolute_cap_kbps, Instant::now())
    }

    pub fn new_at(configured_max_kbps: f64, absolute_cap_kbps: f64, now: Instant) -> Self {
        Self {
            configured_max_kbps,
            effective_max_kbps: configured_max_kbps,
            absolute_cap_kbps: absolute_cap_kbps.max(configured_max_kbps),
            safe_ceiling_kbps: configured_max_kbps,
            failed_ceiling_kbps: None,
            failed_at: None,
            probe_target_kbps: None,
            phase: AdaptiveCeilingPhase::Cruise,
            phase_since: now,
            last_eligible_at: None,
            last_transition_reason: "initialized",
        }
    }

    pub fn observe(
        &mut self,
        now: Instant,
        observation: AdaptiveCeilingObservation,
        policy: AdaptiveCeilingPolicy,
    ) -> AdaptiveCeilingUpdate {
        let mut update = self.expire_failed_bound(now, policy.failed_bound_ttl);

        if observation.eligible {
            self.last_eligible_at = Some(now);
        }

        if observation.bufferbloat {
            return self.handle_bufferbloat(now, observation.shaper_rate_kbps);
        }

        match self.phase {
            AdaptiveCeilingPhase::Cruise => {
                if observation.eligible
                    && self.shaper_at_ceiling(observation.shaper_rate_kbps)
                    && self.next_probe_target(policy.probe_step_percent).is_some()
                {
                    update.transition = Some(self.enter_phase(
                        now,
                        AdaptiveCeilingPhase::Qualify,
                        "clean high load qualification started",
                    ));
                }
            }
            AdaptiveCeilingPhase::Qualify => {
                if self.eligibility_expired(now, policy.eligibility_grace) {
                    update.transition = Some(self.enter_phase(
                        now,
                        AdaptiveCeilingPhase::Cruise,
                        "qualification grace expired",
                    ));
                } else if now.duration_since(self.phase_since) >= policy.hold_time
                    && observation.eligible
                    && self.shaper_at_ceiling(observation.shaper_rate_kbps)
                {
                    if let Some(target) = self.next_probe_target(policy.probe_step_percent) {
                        let previous = self.effective_max_kbps;
                        self.probe_target_kbps = Some(target);
                        self.effective_max_kbps = target;
                        update.change = rate_change(previous, target);
                        update.transition = Some(self.enter_phase(
                            now,
                            AdaptiveCeilingPhase::ProbeRamp,
                            "bounded probe opened",
                        ));
                    } else {
                        update.transition = Some(self.enter_phase(
                            now,
                            AdaptiveCeilingPhase::Cruise,
                            "no probe space remains",
                        ));
                    }
                }
            }
            AdaptiveCeilingPhase::ProbeRamp => {
                if self.eligibility_expired(now, policy.eligibility_grace) {
                    return self.abort_probe(now, "probe ramp grace expired");
                }

                let target = self.probe_target_kbps.unwrap_or(self.safe_ceiling_kbps);
                if observation.eligible && observation.shaper_rate_kbps >= target * 0.98 {
                    update.transition = Some(self.enter_phase(
                        now,
                        AdaptiveCeilingPhase::ProbeObserve,
                        "probe target reached",
                    ));
                } else if now.duration_since(self.phase_since) >= ramp_timeout(policy) {
                    return self.abort_probe(now, "probe ramp timed out");
                }
            }
            AdaptiveCeilingPhase::ProbeObserve => {
                if self.eligibility_expired(now, policy.eligibility_grace) {
                    return self.abort_probe(now, "probe observation grace expired");
                }

                if observation.eligible
                    && now.duration_since(self.phase_since) >= policy.probe_duration
                {
                    let target = self.probe_target_kbps.unwrap_or(self.safe_ceiling_kbps);
                    self.safe_ceiling_kbps = target;
                    self.effective_max_kbps = target;
                    self.probe_target_kbps = None;
                    update.transition = Some(self.enter_phase(
                        now,
                        AdaptiveCeilingPhase::Backoff,
                        "probe confirmed safe",
                    ));
                }
            }
            AdaptiveCeilingPhase::Backoff => {
                if now.duration_since(self.phase_since) >= policy.cooldown {
                    update.transition = Some(self.enter_phase(
                        now,
                        AdaptiveCeilingPhase::Cruise,
                        "probe cooldown complete",
                    ));
                }
            }
        }

        update
    }

    pub fn pause(&mut self, now: Instant, reason: &'static str) -> AdaptiveCeilingUpdate {
        let previous = self.effective_max_kbps;
        self.effective_max_kbps = self.safe_ceiling_kbps;
        self.probe_target_kbps = None;
        self.last_eligible_at = None;
        AdaptiveCeilingUpdate {
            change: rate_change(previous, self.effective_max_kbps),
            transition: Some(self.enter_phase(now, AdaptiveCeilingPhase::Cruise, reason)),
        }
    }

    pub fn abort_probe_gap(&mut self, now: Instant) -> AdaptiveCeilingUpdate {
        match self.phase {
            AdaptiveCeilingPhase::Qualify
            | AdaptiveCeilingPhase::ProbeRamp
            | AdaptiveCeilingPhase::ProbeObserve => self.abort_probe(now, "probe response gap"),
            AdaptiveCeilingPhase::Cruise | AdaptiveCeilingPhase::Backoff => {
                AdaptiveCeilingUpdate::default()
            }
        }
    }

    pub fn reset_to_configured(&mut self, now: Instant) -> AdaptiveCeilingUpdate {
        let previous = self.effective_max_kbps;
        self.effective_max_kbps = self.configured_max_kbps;
        self.safe_ceiling_kbps = self.configured_max_kbps;
        self.failed_ceiling_kbps = None;
        self.failed_at = None;
        self.probe_target_kbps = None;
        self.last_eligible_at = None;

        AdaptiveCeilingUpdate {
            change: rate_change(previous, self.effective_max_kbps),
            transition: Some(self.enter_phase(now, AdaptiveCeilingPhase::Cruise, "stall reset")),
        }
    }

    pub fn configured_max_kbps(&self) -> f64 {
        self.configured_max_kbps
    }

    pub fn effective_max_kbps(&self) -> f64 {
        self.effective_max_kbps
    }

    pub fn absolute_cap_kbps(&self) -> f64 {
        self.absolute_cap_kbps
    }

    pub fn safe_ceiling_kbps(&self) -> f64 {
        self.safe_ceiling_kbps
    }

    pub fn failed_ceiling_kbps(&self) -> Option<f64> {
        self.failed_ceiling_kbps
    }

    pub fn probe_target_kbps(&self) -> Option<f64> {
        self.probe_target_kbps
    }

    pub fn phase(&self) -> AdaptiveCeilingPhase {
        self.phase
    }

    pub fn phase_since(&self) -> Instant {
        self.phase_since
    }

    pub fn last_transition_reason(&self) -> &'static str {
        self.last_transition_reason
    }

    fn handle_bufferbloat(&mut self, now: Instant, shaper_rate_kbps: f64) -> AdaptiveCeilingUpdate {
        let previous = self.effective_max_kbps;
        let failed = self
            .probe_target_kbps
            .unwrap_or(self.effective_max_kbps)
            .max(self.configured_max_kbps);

        if failed > self.safe_ceiling_kbps {
            self.record_failed_bound(failed, now);
        } else if self.safe_ceiling_kbps > self.configured_max_kbps
            && shaper_rate_kbps < self.safe_ceiling_kbps
        {
            self.record_failed_bound(self.safe_ceiling_kbps, now);
            self.safe_ceiling_kbps = shaper_rate_kbps.max(self.configured_max_kbps);
        }

        self.probe_target_kbps = None;
        self.effective_max_kbps = self.safe_ceiling_kbps;
        self.last_eligible_at = None;
        AdaptiveCeilingUpdate {
            change: rate_change(previous, self.effective_max_kbps),
            transition: Some(self.enter_phase(
                now,
                AdaptiveCeilingPhase::Backoff,
                "confirmed bufferbloat",
            )),
        }
    }

    fn abort_probe(&mut self, now: Instant, reason: &'static str) -> AdaptiveCeilingUpdate {
        let previous = self.effective_max_kbps;
        self.probe_target_kbps = None;
        self.effective_max_kbps = self.safe_ceiling_kbps;
        self.last_eligible_at = None;
        AdaptiveCeilingUpdate {
            change: rate_change(previous, self.effective_max_kbps),
            transition: Some(self.enter_phase(now, AdaptiveCeilingPhase::Backoff, reason)),
        }
    }

    fn expire_failed_bound(
        &mut self,
        now: Instant,
        failed_bound_ttl: Duration,
    ) -> AdaptiveCeilingUpdate {
        let Some(failed_at) = self.failed_at else {
            return AdaptiveCeilingUpdate::default();
        };

        if now.duration_since(failed_at) < failed_bound_ttl {
            return AdaptiveCeilingUpdate::default();
        }

        self.failed_ceiling_kbps = None;
        self.failed_at = None;
        self.last_transition_reason = "failed bound expired";
        AdaptiveCeilingUpdate {
            change: None,
            transition: Some(AdaptiveCeilingTransition {
                from: self.phase,
                to: self.phase,
                reason: "failed bound expired",
            }),
        }
    }

    fn next_probe_target(&self, probe_step_percent: f64) -> Option<f64> {
        if self.safe_ceiling_kbps >= self.absolute_cap_kbps {
            return None;
        }

        if let Some(failed) = self.failed_ceiling_kbps {
            let minimum_gap = (self.safe_ceiling_kbps * 0.005).max(1_000.0);
            if failed <= self.safe_ceiling_kbps + minimum_gap {
                return None;
            }
        }

        let open_target = self.safe_ceiling_kbps * (1.0 + probe_step_percent / 100.0);
        let bounded_target = match self.failed_ceiling_kbps {
            Some(failed) if failed > self.safe_ceiling_kbps => {
                open_target.min(self.safe_ceiling_kbps + (failed - self.safe_ceiling_kbps) / 2.0)
            }
            Some(_) => return None,
            None => open_target,
        };
        let target = bounded_target
            .max(self.safe_ceiling_kbps + 1.0)
            .min(self.absolute_cap_kbps)
            .round();

        (target > self.safe_ceiling_kbps + f64::EPSILON).then_some(target)
    }

    fn record_failed_bound(&mut self, failed_kbps: f64, now: Instant) {
        self.failed_ceiling_kbps = Some(
            self.failed_ceiling_kbps
                .map(|current| current.min(failed_kbps))
                .unwrap_or(failed_kbps),
        );
        self.failed_at = Some(now);
    }

    fn shaper_at_ceiling(&self, shaper_rate_kbps: f64) -> bool {
        shaper_rate_kbps >= self.effective_max_kbps * 0.98
    }

    fn eligibility_expired(&self, now: Instant, grace: Duration) -> bool {
        self.last_eligible_at
            .map(|last| now.saturating_duration_since(last) > grace)
            .unwrap_or(true)
    }

    fn enter_phase(
        &mut self,
        now: Instant,
        phase: AdaptiveCeilingPhase,
        reason: &'static str,
    ) -> AdaptiveCeilingTransition {
        let from = self.phase;
        self.phase = phase;
        self.phase_since = now;
        self.last_transition_reason = reason;
        AdaptiveCeilingTransition {
            from,
            to: phase,
            reason,
        }
    }
}

fn ramp_timeout(policy: AdaptiveCeilingPolicy) -> Duration {
    policy
        .hold_time
        .max(policy.probe_duration.saturating_mul(3))
        .max(Duration::from_secs(15))
}

fn rate_change(from_kbps: f64, to_kbps: f64) -> Option<AdaptiveCeilingChange> {
    if to_kbps > from_kbps + f64::EPSILON {
        Some(AdaptiveCeilingChange::Raised { from_kbps, to_kbps })
    } else if to_kbps + f64::EPSILON < from_kbps {
        Some(AdaptiveCeilingChange::Lowered { from_kbps, to_kbps })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> AdaptiveCeilingPolicy {
        AdaptiveCeilingPolicy {
            hold_time: Duration::from_secs(10),
            probe_step_percent: 10.0,
            probe_duration: Duration::from_secs(5),
            cooldown: Duration::from_secs(10),
            failed_bound_ttl: Duration::from_secs(300),
            eligibility_grace: Duration::from_secs(1),
        }
    }

    fn clean(rate: f64) -> AdaptiveCeilingObservation {
        AdaptiveCeilingObservation {
            eligible: true,
            bufferbloat: false,
            shaper_rate_kbps: rate,
        }
    }

    #[test]
    fn completed_probe_promotes_safe_ceiling() {
        let start = Instant::now();
        let mut ceiling = AdaptiveCeilingDirection::new_at(100_000.0, 150_000.0, start);

        ceiling.observe(start, clean(100_000.0), policy());
        let opened = ceiling.observe(start + Duration::from_secs(10), clean(100_000.0), policy());
        assert_eq!(ceiling.phase(), AdaptiveCeilingPhase::ProbeRamp);
        assert_eq!(
            opened.change,
            Some(AdaptiveCeilingChange::Raised {
                from_kbps: 100_000.0,
                to_kbps: 110_000.0,
            })
        );

        ceiling.observe(start + Duration::from_secs(11), clean(109_000.0), policy());
        assert_eq!(ceiling.phase(), AdaptiveCeilingPhase::ProbeObserve);
        ceiling.observe(start + Duration::from_secs(16), clean(110_000.0), policy());

        assert_eq!(ceiling.phase(), AdaptiveCeilingPhase::Backoff);
        assert_eq!(ceiling.safe_ceiling_kbps(), 110_000.0);
        assert_eq!(ceiling.effective_max_kbps(), 110_000.0);
        assert_eq!(ceiling.failed_ceiling_kbps(), None);
    }

    #[test]
    fn failed_probe_rolls_back_and_sets_upper_bound() {
        let start = Instant::now();
        let mut ceiling = AdaptiveCeilingDirection::new_at(100_000.0, 150_000.0, start);

        ceiling.observe(start, clean(100_000.0), policy());
        ceiling.observe(start + Duration::from_secs(10), clean(100_000.0), policy());
        let failed = ceiling.observe(
            start + Duration::from_secs(11),
            AdaptiveCeilingObservation {
                eligible: false,
                bufferbloat: true,
                shaper_rate_kbps: 95_000.0,
            },
            policy(),
        );

        assert_eq!(ceiling.phase(), AdaptiveCeilingPhase::Backoff);
        assert_eq!(ceiling.safe_ceiling_kbps(), 100_000.0);
        assert_eq!(ceiling.failed_ceiling_kbps(), Some(110_000.0));
        assert_eq!(
            failed.change,
            Some(AdaptiveCeilingChange::Lowered {
                from_kbps: 110_000.0,
                to_kbps: 100_000.0,
            })
        );
    }

    #[test]
    fn failed_bound_causes_midpoint_probe() {
        let start = Instant::now();
        let mut ceiling = AdaptiveCeilingDirection::new_at(100_000.0, 150_000.0, start);

        ceiling.observe(start, clean(100_000.0), policy());
        ceiling.observe(start + Duration::from_secs(10), clean(100_000.0), policy());
        ceiling.observe(
            start + Duration::from_secs(11),
            AdaptiveCeilingObservation {
                eligible: false,
                bufferbloat: true,
                shaper_rate_kbps: 95_000.0,
            },
            policy(),
        );
        ceiling.observe(start + Duration::from_secs(21), clean(100_000.0), policy());
        ceiling.observe(start + Duration::from_secs(22), clean(100_000.0), policy());
        let opened = ceiling.observe(start + Duration::from_secs(32), clean(100_000.0), policy());

        assert_eq!(
            opened.change,
            Some(AdaptiveCeilingChange::Raised {
                from_kbps: 100_000.0,
                to_kbps: 105_000.0,
            })
        );
    }

    #[test]
    fn transient_delay_noise_uses_grace_without_losing_safe_bound() {
        let start = Instant::now();
        let mut ceiling = AdaptiveCeilingDirection::new_at(100_000.0, 150_000.0, start);

        ceiling.observe(start, clean(100_000.0), policy());
        ceiling.observe(start + Duration::from_secs(10), clean(100_000.0), policy());
        let transient = ceiling.observe(
            start + Duration::from_secs(11),
            AdaptiveCeilingObservation {
                eligible: false,
                bufferbloat: false,
                shaper_rate_kbps: 100_000.0,
            },
            policy(),
        );

        assert_eq!(ceiling.phase(), AdaptiveCeilingPhase::ProbeRamp);
        assert_eq!(transient.change, None);

        ceiling.observe(
            start + Duration::from_secs(13),
            AdaptiveCeilingObservation {
                eligible: false,
                bufferbloat: false,
                shaper_rate_kbps: 100_000.0,
            },
            policy(),
        );

        assert_eq!(ceiling.safe_ceiling_kbps(), 100_000.0);
        assert_eq!(ceiling.failed_ceiling_kbps(), None);
        assert_eq!(ceiling.effective_max_kbps(), 100_000.0);
    }

    #[test]
    fn idle_load_cancels_qualification_without_opening_a_probe() {
        let start = Instant::now();
        let mut ceiling = AdaptiveCeilingDirection::new_at(100_000.0, 150_000.0, start);

        ceiling.observe(start, clean(100_000.0), policy());
        let update = ceiling.observe(
            start + Duration::from_secs(6),
            AdaptiveCeilingObservation {
                eligible: false,
                bufferbloat: false,
                shaper_rate_kbps: 5_000.0,
            },
            policy(),
        );

        assert_eq!(ceiling.phase(), AdaptiveCeilingPhase::Cruise);
        assert_eq!(ceiling.effective_max_kbps(), 100_000.0);
        assert_eq!(ceiling.probe_target_kbps(), None);
        assert_eq!(
            update.transition.map(|transition| transition.reason),
            Some("qualification grace expired")
        );
    }

    #[test]
    fn probe_gap_aborts_without_failed_bound() {
        let start = Instant::now();
        let mut ceiling = AdaptiveCeilingDirection::new_at(100_000.0, 150_000.0, start);

        ceiling.observe(start, clean(100_000.0), policy());
        ceiling.observe(start + Duration::from_secs(10), clean(100_000.0), policy());
        let update = ceiling.abort_probe_gap(start + Duration::from_secs(11));

        assert!(matches!(
            update.change,
            Some(AdaptiveCeilingChange::Lowered { .. })
        ));
        assert_eq!(ceiling.failed_ceiling_kbps(), None);
        assert_eq!(ceiling.safe_ceiling_kbps(), 100_000.0);
    }

    #[test]
    fn stale_failed_bound_expires() {
        let start = Instant::now();
        let mut ceiling = AdaptiveCeilingDirection::new_at(100_000.0, 150_000.0, start);

        ceiling.observe(start, clean(100_000.0), policy());
        ceiling.observe(start + Duration::from_secs(10), clean(100_000.0), policy());
        ceiling.observe(
            start + Duration::from_secs(11),
            AdaptiveCeilingObservation {
                eligible: false,
                bufferbloat: true,
                shaper_rate_kbps: 95_000.0,
            },
            policy(),
        );
        ceiling.observe(start + Duration::from_secs(312), clean(100_000.0), policy());

        assert_eq!(ceiling.failed_ceiling_kbps(), None);
    }

    #[test]
    fn stall_reset_forgets_runtime_learning() {
        let start = Instant::now();
        let mut ceiling = AdaptiveCeilingDirection::new_at(100_000.0, 150_000.0, start);

        ceiling.observe(start, clean(100_000.0), policy());
        ceiling.observe(start + Duration::from_secs(10), clean(100_000.0), policy());
        ceiling.observe(start + Duration::from_secs(11), clean(109_000.0), policy());
        ceiling.observe(start + Duration::from_secs(16), clean(110_000.0), policy());
        ceiling.reset_to_configured(start + Duration::from_secs(17));

        assert_eq!(ceiling.phase(), AdaptiveCeilingPhase::Cruise);
        assert_eq!(ceiling.safe_ceiling_kbps(), 100_000.0);
        assert_eq!(ceiling.effective_max_kbps(), 100_000.0);
        assert_eq!(ceiling.failed_ceiling_kbps(), None);
    }

    #[derive(Debug)]
    struct SimulationMetrics {
        average_utilization: f64,
        confirmed_bloat_events: usize,
        first_95_percent_safe_s: Option<u64>,
        final_ceiling_kbps: f64,
    }

    fn simulation_policy() -> AdaptiveCeilingPolicy {
        AdaptiveCeilingPolicy {
            hold_time: Duration::from_secs(20),
            probe_step_percent: 3.0,
            probe_duration: Duration::from_secs(6),
            cooldown: Duration::from_secs(10),
            failed_bound_ttl: Duration::from_secs(900),
            eligibility_grace: Duration::from_secs(1),
        }
    }

    fn simulate_bounded<F, N>(duration_s: u64, capacity: F, noise: N) -> SimulationMetrics
    where
        F: Fn(u64) -> f64,
        N: Fn(u64) -> bool,
    {
        let start = Instant::now();
        let mut ceiling = AdaptiveCeilingDirection::new_at(800_000.0, 1_000_000.0, start);
        let mut shaper = 800_000.0;
        let mut utilization_sum = 0.0;
        let mut bloat_events = 0;
        let mut first_95 = None;

        for second in 0..duration_s {
            let now = start + Duration::from_secs(second);
            let available = capacity(second);
            let desired = ceiling.effective_max_kbps();
            if shaper < desired {
                shaper = (shaper * 1.04).min(desired);
            } else if shaper > desired {
                shaper = desired;
            }

            let bufferbloat = shaper > available;
            if bufferbloat {
                bloat_events += 1;
                shaper = (shaper * 0.90).max(400_000.0);
            }
            let achieved = shaper.min(available);
            utilization_sum += achieved / available;
            let eligible = !bufferbloat && !noise(second) && achieved >= shaper * 0.95;

            ceiling.observe(
                now,
                AdaptiveCeilingObservation {
                    eligible,
                    bufferbloat,
                    shaper_rate_kbps: shaper,
                },
                simulation_policy(),
            );

            if first_95.is_none() && ceiling.safe_ceiling_kbps() >= available * 0.95 {
                first_95 = Some(second);
            }
        }

        SimulationMetrics {
            average_utilization: utilization_sum / duration_s as f64,
            confirmed_bloat_events: bloat_events,
            first_95_percent_safe_s: first_95,
            final_ceiling_kbps: ceiling.safe_ceiling_kbps(),
        }
    }

    fn simulate_legacy<F, N>(duration_s: u64, capacity: F, noise: N) -> SimulationMetrics
    where
        F: Fn(u64) -> f64,
        N: Fn(u64) -> bool,
    {
        let configured: f64 = 800_000.0;
        let cap: f64 = 1_000_000.0;
        let mut ceiling: f64 = configured;
        let mut shaper: f64 = configured;
        let mut qualified_s = 0;
        let mut utilization_sum = 0.0;
        let mut bloat_events = 0;
        let mut first_95 = None;

        for second in 0..duration_s {
            let available = capacity(second);
            if shaper < ceiling {
                shaper = (shaper * 1.04).min(ceiling);
            } else if shaper > ceiling {
                shaper = ceiling;
            }

            let bufferbloat = shaper > available;
            if bufferbloat {
                bloat_events += 1;
                shaper = (shaper * 0.90).max(400_000.0);
                ceiling = ceiling.min(shaper.max(configured));
                qualified_s = 0;
            }

            let achieved = shaper.min(available);
            utilization_sum += achieved / available;
            let eligible = !bufferbloat
                && !noise(second)
                && achieved >= shaper * 0.95
                && shaper >= ceiling * 0.99;
            if eligible {
                qualified_s += 1;
                if qualified_s >= 60 && ceiling < cap {
                    ceiling = (ceiling * 1.01).min(cap);
                    qualified_s = 0;
                }
            } else {
                qualified_s = 0;
            }

            if first_95.is_none() && ceiling >= available * 0.95 {
                first_95 = Some(second);
            }
        }

        SimulationMetrics {
            average_utilization: utilization_sum / duration_s as f64,
            confirmed_bloat_events: bloat_events,
            first_95_percent_safe_s: first_95,
            final_ceiling_kbps: ceiling,
        }
    }

    #[test]
    fn simulation_converges_faster_on_clean_stable_link() {
        let bounded = simulate_bounded(1_200, |_| 950_000.0, |_| false);
        let legacy = simulate_legacy(1_200, |_| 950_000.0, |_| false);

        eprintln!("stable bounded={bounded:?} legacy={legacy:?}");
        assert!(
            bounded.first_95_percent_safe_s.unwrap() < legacy.first_95_percent_safe_s.unwrap() / 2
        );
        assert!(bounded.average_utilization > legacy.average_utilization);
        assert!(bounded.confirmed_bloat_events <= 2);
        assert!(bounded.final_ceiling_kbps >= 900_000.0);
    }

    #[test]
    fn simulation_handles_capacity_drop_and_recovery() {
        let capacity = |second| {
            if second < 300 {
                950_000.0
            } else if second < 650 {
                850_000.0
            } else {
                950_000.0
            }
        };
        let bounded = simulate_bounded(1_500, capacity, |_| false);
        let legacy = simulate_legacy(1_500, capacity, |_| false);

        eprintln!("variable bounded={bounded:?} legacy={legacy:?}");
        assert!(bounded.average_utilization >= legacy.average_utilization);
        assert!(bounded.confirmed_bloat_events <= 5);
        assert!(bounded.final_ceiling_kbps > 850_000.0);
    }

    #[test]
    fn simulation_tolerates_isolated_delay_noise() {
        let noise = |second| matches!(second, 95 | 211 | 407 | 733);
        let bounded = simulate_bounded(1_200, |_| 950_000.0, noise);
        let legacy = simulate_legacy(1_200, |_| 950_000.0, noise);

        eprintln!("noise bounded={bounded:?} legacy={legacy:?}");
        assert!(bounded.final_ceiling_kbps >= legacy.final_ceiling_kbps);
        assert!(bounded.confirmed_bloat_events <= 2);
    }

    #[test]
    fn simulation_learns_asymmetric_directions_independently() {
        let download = simulate_bounded(1_200, |_| 950_000.0, |_| false);
        let upload = simulate_bounded(1_200, |_| 870_000.0, |_| false);

        eprintln!("asymmetric download={download:?} upload={upload:?}");
        assert!(download.final_ceiling_kbps >= 900_000.0);
        assert!((850_000.0..=885_000.0).contains(&upload.final_ceiling_kbps));
        assert!(download.confirmed_bloat_events <= 2);
        assert!(upload.confirmed_bloat_events <= 5);
    }
}
