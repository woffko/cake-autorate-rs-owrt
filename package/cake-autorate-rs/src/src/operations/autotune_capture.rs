//! Bounded, instance-owned native Auto-Tune measurement accumulation.
//!
//! The long-lived autorate instance is the only owner of the observations fed
//! into this accumulator.  This module deliberately performs no probing,
//! traffic generation, counter reads, or OpenWrt mutation of its own.

use super::full_autotune::{
    AutotuneCapturePhase, AutotuneCaptureRequest, AutotuneCaptureSnapshot, AutotuneCaptureState,
    AutotuneLoadProof,
};
use super::protocol::SpeedtestDirection;
use std::collections::VecDeque;

const MAX_ICMP_OBSERVATIONS: usize = 192;
const MAX_TRANSPORT_OBSERVATIONS: usize = 128;
const MAX_TRANSPORT_TIMEOUT_OBSERVATIONS: usize = 32;
const MAX_TRANSPORT_DEADLINE_US: u64 = 60_000_000;
const MAX_CPU_OBSERVATIONS: usize = 128;
const MAX_CONFIDENCE_OBSERVATIONS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutotuneCaptureObservationKind {
    IcmpSuccess {
        latency_us: Option<u64>,
        delta_us: Option<u64>,
    },
    IcmpTimeout,
    TransportSuccess {
        latency_us: Option<u64>,
        delta_us: Option<u64>,
    },
    TransportDeadlineExceeded {
        deadline_us: u64,
        delta_lower_bound_us: u64,
    },
    Cpu {
        milli_percent: u32,
    },
    Traffic {
        background_confidence_percent: u8,
        contaminated: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutotuneCaptureObservation {
    pub capture_id: String,
    pub request_sequence: u32,
    pub observation_id: u64,
    pub observed_boot_ms: u64,
    pub kind: AutotuneCaptureObservationKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutotuneCaptureObservationResult {
    Accepted,
    Duplicate,
    Complete,
}

pub fn icmp_observation_kind(
    request: &AutotuneCaptureRequest,
    rtt_ms: f64,
    download_delta_us: f64,
    upload_delta_us: f64,
    download_loaded: bool,
    upload_loaded: bool,
) -> Result<Option<AutotuneCaptureObservationKind>, String> {
    match request.phase {
        AutotuneCapturePhase::IdleBaseline => {
            Ok(Some(AutotuneCaptureObservationKind::IcmpSuccess {
                latency_us: Some(positive_us(rtt_ms * 1_000.0)?),
                delta_us: None,
            }))
        }
        AutotuneCapturePhase::LoadedMeasurement => {
            let phase_matches = match request.direction {
                Some(SpeedtestDirection::Download) => download_loaded && !upload_loaded,
                Some(SpeedtestDirection::Upload) => !download_loaded && upload_loaded,
                Some(SpeedtestDirection::Both) => download_loaded && upload_loaded,
                None => false,
            };
            if !phase_matches {
                return Ok(None);
            }
            let delta_us = match request.direction {
                Some(SpeedtestDirection::Download) => nonnegative_us(download_delta_us)?,
                Some(SpeedtestDirection::Upload) => nonnegative_us(upload_delta_us)?,
                Some(SpeedtestDirection::Both) => {
                    nonnegative_us(download_delta_us.max(upload_delta_us))?
                }
                None => {
                    return Err(
                        "loaded Auto-Tune ICMP observation has no requested direction".to_string(),
                    )
                }
            };
            Ok(Some(AutotuneCaptureObservationKind::IcmpSuccess {
                latency_us: None,
                delta_us: Some(delta_us),
            }))
        }
    }
}

pub fn transport_observation_kind(
    request: &AutotuneCaptureRequest,
    latency_ms: f64,
    baseline_ms: Option<f64>,
    download_loaded: bool,
    upload_loaded: bool,
) -> Result<Option<AutotuneCaptureObservationKind>, String> {
    let latency_us = positive_us(latency_ms * 1_000.0)?;
    match request.phase {
        AutotuneCapturePhase::IdleBaseline => {
            if download_loaded || upload_loaded {
                return Ok(None);
            }
            Ok(Some(AutotuneCaptureObservationKind::TransportSuccess {
                latency_us: Some(latency_us),
                delta_us: None,
            }))
        }
        AutotuneCapturePhase::LoadedMeasurement => {
            let phase_matches = match request.direction {
                Some(SpeedtestDirection::Download) => download_loaded && !upload_loaded,
                Some(SpeedtestDirection::Upload) => !download_loaded && upload_loaded,
                Some(SpeedtestDirection::Both) => download_loaded && upload_loaded,
                None => false,
            };
            if !phase_matches {
                return Ok(None);
            }
            let Some(baseline_ms) = baseline_ms else {
                return Ok(None);
            };
            let baseline_us = positive_us(baseline_ms * 1_000.0)?;
            Ok(Some(AutotuneCaptureObservationKind::TransportSuccess {
                latency_us: None,
                delta_us: Some(latency_us.saturating_sub(baseline_us)),
            }))
        }
    }
}

/// Convert one exact transport deadline exhaustion into a censored loaded
/// latency observation.  The caller must separately prove that the whole
/// probe flight remained inside the requested directional load phase.
///
/// This never creates an idle baseline and never turns a fast DNS, route,
/// connection or protocol failure into latency evidence.
pub fn transport_deadline_observation_kind(
    request: &AutotuneCaptureRequest,
    deadline_us: u64,
    baseline_ms: Option<f64>,
    download_loaded: bool,
    upload_loaded: bool,
) -> Result<Option<AutotuneCaptureObservationKind>, String> {
    if request.phase != AutotuneCapturePhase::LoadedMeasurement {
        return Err("transport deadline cannot establish an idle baseline".to_string());
    }
    if deadline_us == 0 || deadline_us > MAX_TRANSPORT_DEADLINE_US {
        return Err("native Auto-Tune transport deadline is invalid".to_string());
    }
    let phase_matches = match request.direction {
        Some(SpeedtestDirection::Download) => download_loaded && !upload_loaded,
        Some(SpeedtestDirection::Upload) => !download_loaded && upload_loaded,
        Some(SpeedtestDirection::Both) => download_loaded && upload_loaded,
        None => false,
    };
    if !phase_matches {
        return Ok(None);
    }
    let Some(baseline_ms) = baseline_ms else {
        return Ok(None);
    };
    let baseline_us = positive_us(baseline_ms * 1_000.0)?;
    let delta_lower_bound_us = deadline_us.saturating_sub(baseline_us);
    if delta_lower_bound_us == 0 {
        return Err("transport deadline does not exceed its idle baseline".to_string());
    }
    Ok(Some(
        AutotuneCaptureObservationKind::TransportDeadlineExceeded {
            deadline_us,
            delta_lower_bound_us,
        },
    ))
}

/// Existing aggregate interface counters are sufficient for an idle baseline.
/// A loaded capture deliberately gets no confidence here: that requires the
/// later native load supervisor's own byte evidence to separate test traffic
/// from unrelated traffic on the same uplink.
pub fn idle_traffic_observation_kind(
    request: &AutotuneCaptureRequest,
    download_kbps: f64,
    upload_kbps: f64,
    download_reference_kbps: f64,
    upload_reference_kbps: f64,
) -> Result<Option<AutotuneCaptureObservationKind>, String> {
    if request.phase != AutotuneCapturePhase::IdleBaseline {
        return Ok(None);
    }
    if !download_kbps.is_finite()
        || download_kbps < 0.0
        || !upload_kbps.is_finite()
        || upload_kbps < 0.0
        || !download_reference_kbps.is_finite()
        || download_reference_kbps <= 0.0
        || !upload_reference_kbps.is_finite()
        || upload_reference_kbps <= 0.0
    {
        return Err("idle Auto-Tune traffic observation is invalid".to_string());
    }
    let maximum_share_percent = ((download_kbps / download_reference_kbps)
        .max(upload_kbps / upload_reference_kbps)
        * 100.0)
        .clamp(0.0, 999.0);
    Ok(Some(AutotuneCaptureObservationKind::Traffic {
        background_confidence_percent: capacity_confidence_for_share(maximum_share_percent),
        contaminated: maximum_share_percent > 5.0,
    }))
}

/// Convert exact worker-owned load evidence into the conservative confidence
/// understood by the accumulator. Receive-side payload is a known subset of
/// route-interface bytes. On transmit, socket-accepted payload may briefly
/// lead the interface counter while queued/GSO data drains, so validation
/// admits only the bounded lag defined by `AutotuneLoadEvidence`.  Either sign
/// of the difference remains uncertainty and lowers confidence instead of
/// being hidden by an assumed overhead multiplier.
pub fn loaded_traffic_observation_kind<E: AutotuneLoadProof + ?Sized>(
    request: &AutotuneCaptureRequest,
    evidence: &E,
    current_boot_ms: u64,
) -> Result<AutotuneCaptureObservationKind, String> {
    evidence.attests_load(request, current_boot_ms)?;
    let known_percent = evidence.load_byte_confidence_percent()?;
    Ok(AutotuneCaptureObservationKind::Traffic {
        background_confidence_percent: known_percent,
        contaminated: known_percent < 80,
    })
}

/// Detect the controlled direction without requiring it to realize an
/// arbitrary percentage of the candidate shaper. This keeps weak/variable
/// links measurable while rejecting material reverse-direction traffic; small
/// reverse traffic is treated as TCP ACK overhead for the requested direction.
pub fn bounded_load_trigger_kbps(
    request: &AutotuneCaptureRequest,
    configured_threshold_kbps: f64,
) -> Result<f64, String> {
    if !configured_threshold_kbps.is_finite() || configured_threshold_kbps <= 0.0 {
        return Err("native Auto-Tune configured load threshold is invalid".to_string());
    }
    if request.phase != AutotuneCapturePhase::LoadedMeasurement {
        return Ok(configured_threshold_kbps.max(1.0));
    }
    let reference = request
        .load_reference_kbps
        .filter(|value| *value > 0)
        .ok_or_else(|| "native Auto-Tune loaded capture has no load reference".to_string())?;
    let relative_trigger = reference.saturating_add(19) / 20;
    Ok((relative_trigger.max(10) as f64).min(configured_threshold_kbps.max(10.0)))
}

pub fn bounded_directional_load_phase(
    request: &AutotuneCaptureRequest,
    download_kbps: f64,
    upload_kbps: f64,
    configured_threshold_kbps: f64,
    ack_ratio: f64,
) -> Result<(bool, bool), String> {
    let active_threshold = bounded_load_trigger_kbps(request, configured_threshold_kbps)?;
    directional_load_phase(
        request,
        download_kbps,
        upload_kbps,
        active_threshold,
        ack_ratio,
    )
}

pub fn directional_load_phase(
    request: &AutotuneCaptureRequest,
    download_kbps: f64,
    upload_kbps: f64,
    active_threshold_kbps: f64,
    ack_ratio: f64,
) -> Result<(bool, bool), String> {
    directional_load_phase_with_forward_reference(
        request,
        download_kbps,
        upload_kbps,
        active_threshold_kbps,
        ack_ratio,
        None,
    )
}

/// Classify a directional load while allowing already observed forward-load
/// evidence to bound temporally displaced reverse ACKs.  The reference can
/// only raise the reverse allowance; it never establishes current load.  The
/// requested forward direction must still meet `active_threshold_kbps` in the
/// current counter window.
pub(crate) fn directional_load_phase_with_forward_reference(
    request: &AutotuneCaptureRequest,
    download_kbps: f64,
    upload_kbps: f64,
    active_threshold_kbps: f64,
    ack_ratio: f64,
    forward_reference_kbps: Option<f64>,
) -> Result<(bool, bool), String> {
    if !download_kbps.is_finite()
        || download_kbps < 0.0
        || !upload_kbps.is_finite()
        || upload_kbps < 0.0
        || !active_threshold_kbps.is_finite()
        || active_threshold_kbps <= 0.0
        || !ack_ratio.is_finite()
        || !(0.0..=0.25).contains(&ack_ratio)
        || forward_reference_kbps.is_some_and(|value| !value.is_finite() || value < 0.0)
    {
        return Err("native Auto-Tune directional load inputs are invalid".to_string());
    }
    if request.phase != AutotuneCapturePhase::LoadedMeasurement {
        return Ok((false, false));
    }
    Ok(match request.direction {
        Some(SpeedtestDirection::Download) => {
            if download_kbps < active_threshold_kbps {
                (false, false)
            } else {
                let forward_reference = forward_reference_kbps
                    .unwrap_or(download_kbps)
                    .max(download_kbps);
                let reverse_limit = active_threshold_kbps.max(forward_reference * ack_ratio);
                (true, upload_kbps > reverse_limit)
            }
        }
        Some(SpeedtestDirection::Upload) => {
            if upload_kbps < active_threshold_kbps {
                (false, false)
            } else {
                let forward_reference = forward_reference_kbps
                    .unwrap_or(upload_kbps)
                    .max(upload_kbps);
                let reverse_limit = active_threshold_kbps.max(forward_reference * ack_ratio);
                (download_kbps > reverse_limit, true)
            }
        }
        Some(SpeedtestDirection::Both) => (
            download_kbps >= active_threshold_kbps,
            upload_kbps >= active_threshold_kbps,
        ),
        None => (false, false),
    })
}

#[derive(Clone, Debug)]
pub struct AutotuneCaptureAccumulator {
    request: Option<AutotuneCaptureRequest>,
    admitted_boot_ms: u64,
    updated_boot_ms: u64,
    last_observation: Option<AutotuneCaptureObservation>,
    icmp_values_us: VecDeque<u64>,
    transport_values_us: VecDeque<u64>,
    transport_timeouts_us: VecDeque<(u64, u64)>,
    cpu_milli_percent: VecDeque<u32>,
    background_confidence_percent: VecDeque<u8>,
    icmp_attempts: u32,
    icmp_successes: u32,
    contaminated: bool,
    rejected_code: Option<String>,
}

impl Default for AutotuneCaptureAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl AutotuneCaptureAccumulator {
    pub fn new() -> Self {
        Self {
            request: None,
            admitted_boot_ms: 0,
            updated_boot_ms: 0,
            last_observation: None,
            icmp_values_us: VecDeque::with_capacity(MAX_ICMP_OBSERVATIONS),
            transport_values_us: VecDeque::with_capacity(MAX_TRANSPORT_OBSERVATIONS),
            transport_timeouts_us: VecDeque::with_capacity(MAX_TRANSPORT_TIMEOUT_OBSERVATIONS),
            cpu_milli_percent: VecDeque::with_capacity(MAX_CPU_OBSERVATIONS),
            background_confidence_percent: VecDeque::with_capacity(MAX_CONFIDENCE_OBSERVATIONS),
            icmp_attempts: 0,
            icmp_successes: 0,
            contaminated: false,
            rejected_code: None,
        }
    }

    /// Admit one exact capture. Re-admitting the same request preserves its
    /// bounded samples; any different request starts with an empty accumulator.
    pub fn admit(
        &mut self,
        request: &AutotuneCaptureRequest,
        boot_ms: u64,
    ) -> Result<AutotuneCaptureSnapshot, String> {
        request.validate()?;
        if boot_ms == 0 || boot_ms > request.deadline_boot_ms {
            return Err("native Auto-Tune accumulator admission time is invalid".to_string());
        }
        if self.request.as_ref() != Some(request) {
            self.reset_samples();
            self.request = Some(request.clone());
            self.admitted_boot_ms = boot_ms;
            self.updated_boot_ms = boot_ms;
        }
        self.snapshot()
    }

    pub fn clear(&mut self) {
        self.request = None;
        self.admitted_boot_ms = 0;
        self.updated_boot_ms = 0;
        self.reset_samples();
    }

    pub fn active_request(&self) -> Option<&AutotuneCaptureRequest> {
        self.request.as_ref()
    }

    pub fn accepting_observations(&self) -> bool {
        self.request.is_some() && self.rejected_code.is_none() && !self.is_complete()
    }

    pub fn reject_current(
        &mut self,
        diagnostic_code: &str,
        boot_ms: u64,
    ) -> Result<AutotuneCaptureSnapshot, String> {
        validate_diagnostic_code(diagnostic_code)?;
        let request = self
            .request
            .as_ref()
            .ok_or_else(|| "native Auto-Tune accumulator has no active request".to_string())?;
        if self.is_complete() {
            return Err("native Auto-Tune accumulator is terminally complete".to_string());
        }
        if boot_ms == 0 || boot_ms > request.deadline_boot_ms {
            return Err("native Auto-Tune accumulator rejection time is invalid".to_string());
        }
        self.updated_boot_ms = boot_ms.max(self.admitted_boot_ms);
        self.rejected_code = Some(diagnostic_code.to_string());
        self.snapshot()
    }

    /// Reject the active capture without inventing a later observation time.
    ///
    /// This is the fail-closed fallback for a controller that has already
    /// admitted the request but cannot read the kernel monotonic clock while
    /// invalidating its measurement basis. The last accepted accumulator time
    /// is already bounded by the request lifetime, so it is sufficient to make
    /// the epoch terminal without mixing subsequent observations into it.
    pub fn reject_current_at_last_update(
        &mut self,
        diagnostic_code: &str,
    ) -> Result<AutotuneCaptureSnapshot, String> {
        self.reject_current(
            diagnostic_code,
            self.updated_boot_ms.max(self.admitted_boot_ms),
        )
    }

    pub fn observe(
        &mut self,
        observation: AutotuneCaptureObservation,
    ) -> Result<AutotuneCaptureObservationResult, String> {
        let request = self
            .request
            .as_ref()
            .ok_or_else(|| "native Auto-Tune accumulator has no active request".to_string())?;
        if self.rejected_code.is_some() {
            return Err("native Auto-Tune accumulator is terminally rejected".to_string());
        }
        if self.is_complete() {
            return Err("native Auto-Tune accumulator is terminally complete".to_string());
        }
        if observation.capture_id != request.capture_id
            || observation.request_sequence != request.sequence
        {
            return Err("native Auto-Tune accumulator observation identity mismatch".to_string());
        }
        if observation.observation_id == 0
            || observation.observed_boot_ms < self.admitted_boot_ms
            || observation.observed_boot_ms < self.updated_boot_ms
            || observation.observed_boot_ms > request.deadline_boot_ms
        {
            return Err("native Auto-Tune accumulator observation time is invalid".to_string());
        }
        if let Some(last) = self.last_observation.as_ref() {
            if observation.observation_id == last.observation_id {
                if &observation == last {
                    return Ok(AutotuneCaptureObservationResult::Duplicate);
                }
                return Err("native Auto-Tune accumulator observation ID was rewritten".to_string());
            }
            if observation.observation_id < last.observation_id {
                return Err("native Auto-Tune accumulator observation order regressed".to_string());
            }
        }

        self.validate_observation(request.phase, observation.kind)?;
        self.last_observation = Some(observation.clone());
        self.updated_boot_ms = observation.observed_boot_ms;
        self.record_observation(observation.kind);
        if self.is_complete() {
            Ok(AutotuneCaptureObservationResult::Complete)
        } else {
            Ok(AutotuneCaptureObservationResult::Accepted)
        }
    }

    pub fn snapshot(&self) -> Result<AutotuneCaptureSnapshot, String> {
        let request = self
            .request
            .as_ref()
            .ok_or_else(|| "native Auto-Tune accumulator has no active request".to_string())?
            .clone();
        let rejected = self.rejected_code.clone();
        let complete = rejected.is_none() && self.is_complete();
        let state = if rejected.is_some() {
            AutotuneCaptureState::Rejected
        } else if complete {
            AutotuneCaptureState::Complete
        } else {
            AutotuneCaptureState::Collecting
        };
        let confidence = complete
            .then(|| self.background_confidence_percent.iter().copied().min())
            .flatten();
        let mut snapshot = AutotuneCaptureSnapshot {
            request,
            state,
            updated_boot_ms: self.updated_boot_ms,
            icmp_samples: self.icmp_successes,
            transport_samples: self.transport_values_us.len() as u32,
            transport_timeout_count: self.transport_timeouts_us.len() as u32,
            transport_timeout_total_us: self
                .transport_timeouts_us
                .iter()
                .map(|(deadline_us, _)| *deadline_us)
                .sum(),
            transport_censored: !self.transport_timeouts_us.is_empty(),
            cpu_samples: self.cpu_milli_percent.len() as u32,
            idle_median_us: None,
            idle_p95_us: None,
            icmp_delta_us: None,
            transport_delta_us: None,
            loss_ppm: None,
            cpu_milli_percent: None,
            background_confidence_percent: confidence,
            contaminated: self.contaminated,
            diagnostic_code: rejected,
        };
        if complete {
            match snapshot.request.phase {
                AutotuneCapturePhase::IdleBaseline => {
                    snapshot.idle_median_us = percentile_u64(&self.icmp_values_us, 50);
                    snapshot.idle_p95_us = percentile_u64(&self.icmp_values_us, 95);
                }
                AutotuneCapturePhase::LoadedMeasurement => {
                    snapshot.icmp_delta_us = percentile_u64(&self.icmp_values_us, 95);
                    snapshot.transport_delta_us = percentile_u64(&self.transport_values_us, 95)
                        .into_iter()
                        .chain(
                            self.transport_timeouts_us
                                .iter()
                                .map(|(_, delta_lower_bound_us)| *delta_lower_bound_us),
                        )
                        .max();
                    snapshot.loss_ppm = Some(loss_ppm(self.icmp_attempts, self.icmp_successes));
                    snapshot.cpu_milli_percent = percentile_u32(&self.cpu_milli_percent, 95);
                }
            }
        }
        snapshot.validate()?;
        Ok(snapshot)
    }

    fn reset_samples(&mut self) {
        self.last_observation = None;
        self.icmp_values_us.clear();
        self.transport_values_us.clear();
        self.transport_timeouts_us.clear();
        self.cpu_milli_percent.clear();
        self.background_confidence_percent.clear();
        self.icmp_attempts = 0;
        self.icmp_successes = 0;
        self.contaminated = false;
        self.rejected_code = None;
    }

    fn validate_observation(
        &self,
        phase: AutotuneCapturePhase,
        kind: AutotuneCaptureObservationKind,
    ) -> Result<(), String> {
        match kind {
            AutotuneCaptureObservationKind::IcmpSuccess {
                latency_us,
                delta_us,
            }
            | AutotuneCaptureObservationKind::TransportSuccess {
                latency_us,
                delta_us,
            } => match phase {
                AutotuneCapturePhase::IdleBaseline
                    if latency_us.is_some_and(|value| value > 0) && delta_us.is_none() => {}
                AutotuneCapturePhase::LoadedMeasurement
                    if latency_us.is_none() && delta_us.is_some() => {}
                _ => {
                    return Err(
                        "native Auto-Tune accumulator observation contradicts capture phase"
                            .to_string(),
                    )
                }
            },
            AutotuneCaptureObservationKind::TransportDeadlineExceeded {
                deadline_us,
                delta_lower_bound_us,
            } if phase == AutotuneCapturePhase::LoadedMeasurement
                && deadline_us > 0
                && deadline_us <= MAX_TRANSPORT_DEADLINE_US
                && delta_lower_bound_us > 0
                && delta_lower_bound_us <= deadline_us => {}
            AutotuneCaptureObservationKind::TransportDeadlineExceeded { .. } => {
                return Err(
                    "native Auto-Tune transport timeout contradicts capture phase".to_string(),
                )
            }
            AutotuneCaptureObservationKind::IcmpTimeout => {}
            AutotuneCaptureObservationKind::Cpu { milli_percent } if milli_percent <= 100_000 => {}
            AutotuneCaptureObservationKind::Cpu { .. } => {
                return Err("native Auto-Tune accumulator CPU sample is invalid".to_string())
            }
            AutotuneCaptureObservationKind::Traffic {
                background_confidence_percent,
                ..
            } if background_confidence_percent <= 100 => {}
            AutotuneCaptureObservationKind::Traffic { .. } => {
                return Err("native Auto-Tune accumulator confidence sample is invalid".to_string())
            }
        }
        Ok(())
    }

    fn record_observation(&mut self, kind: AutotuneCaptureObservationKind) {
        match kind {
            AutotuneCaptureObservationKind::IcmpSuccess {
                latency_us,
                delta_us,
            } => {
                self.icmp_attempts = self.icmp_attempts.saturating_add(1);
                self.icmp_successes = self.icmp_successes.saturating_add(1);
                push_bounded(
                    &mut self.icmp_values_us,
                    latency_us.or(delta_us).unwrap_or(0),
                    MAX_ICMP_OBSERVATIONS,
                );
            }
            AutotuneCaptureObservationKind::IcmpTimeout => {
                self.icmp_attempts = self.icmp_attempts.saturating_add(1);
            }
            AutotuneCaptureObservationKind::TransportSuccess {
                latency_us,
                delta_us,
            } => push_bounded(
                &mut self.transport_values_us,
                latency_us.or(delta_us).unwrap_or(0),
                MAX_TRANSPORT_OBSERVATIONS,
            ),
            AutotuneCaptureObservationKind::TransportDeadlineExceeded {
                deadline_us,
                delta_lower_bound_us,
            } => push_bounded(
                &mut self.transport_timeouts_us,
                (deadline_us, delta_lower_bound_us),
                MAX_TRANSPORT_TIMEOUT_OBSERVATIONS,
            ),
            AutotuneCaptureObservationKind::Cpu { milli_percent } => push_bounded(
                &mut self.cpu_milli_percent,
                milli_percent,
                MAX_CPU_OBSERVATIONS,
            ),
            AutotuneCaptureObservationKind::Traffic {
                background_confidence_percent,
                contaminated,
            } => {
                push_bounded(
                    &mut self.background_confidence_percent,
                    background_confidence_percent,
                    MAX_CONFIDENCE_OBSERVATIONS,
                );
                self.contaminated |= contaminated;
            }
        }
    }

    fn is_complete(&self) -> bool {
        let Some(request) = self.request.as_ref() else {
            return false;
        };
        let has_confidence = !self.background_confidence_percent.is_empty();
        match request.phase {
            AutotuneCapturePhase::IdleBaseline => {
                self.icmp_successes >= super::full_autotune::MIN_AUTOTUNE_IDLE_ICMP_SAMPLES
                    && self.transport_values_us.len()
                        >= super::full_autotune::MIN_AUTOTUNE_TRANSPORT_SAMPLES as usize
                    && has_confidence
            }
            AutotuneCapturePhase::LoadedMeasurement => self.icmp_successes
                >= super::full_autotune::MIN_AUTOTUNE_LOADED_ICMP_SAMPLES
                && (self.transport_values_us.len()
                    >= super::full_autotune::MIN_AUTOTUNE_TRANSPORT_SAMPLES as usize
                    || (self.transport_timeouts_us.len()
                        >= super::full_autotune::MIN_AUTOTUNE_TRANSPORT_TIMEOUTS as usize
                        && self
                            .transport_timeouts_us
                            .iter()
                            .map(|(deadline_us, _)| *deadline_us)
                            .sum::<u64>()
                            >= super::full_autotune::MIN_AUTOTUNE_TRANSPORT_TIMEOUT_COVERAGE_US))
                && !self.cpu_milli_percent.is_empty()
                && has_confidence,
        }
    }
}

fn push_bounded<T>(values: &mut VecDeque<T>, value: T, capacity: usize) {
    if values.len() == capacity {
        values.pop_front();
    }
    values.push_back(value);
}

fn percentile_u64(values: &VecDeque<u64>, percentile: usize) -> Option<u64> {
    let sorted = sorted_copy(values);
    nearest_rank(&sorted, percentile)
}

fn percentile_u32(values: &VecDeque<u32>, percentile: usize) -> Option<u32> {
    let sorted = sorted_copy(values);
    nearest_rank(&sorted, percentile)
}

fn sorted_copy<T: Copy + Ord>(values: &VecDeque<T>) -> Vec<T> {
    let mut sorted = values.iter().copied().collect::<Vec<_>>();
    sorted.sort_unstable();
    sorted
}

fn nearest_rank<T: Copy>(sorted: &[T], percentile: usize) -> Option<T> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (percentile.clamp(1, 100) * sorted.len()).div_ceil(100);
    sorted.get(rank.saturating_sub(1)).copied()
}

fn loss_ppm(attempts: u32, successes: u32) -> u32 {
    if attempts == 0 {
        return 0;
    }
    u32::try_from(u64::from(attempts.saturating_sub(successes)) * 1_000_000 / u64::from(attempts))
        .unwrap_or(1_000_000)
}

fn validate_diagnostic_code(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 96
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_alphanumeric() && !b"_-".contains(&byte))
    {
        return Err("native Auto-Tune accumulator diagnostic code is invalid".to_string());
    }
    Ok(())
}

fn positive_us(value: f64) -> Result<u64, String> {
    if !value.is_finite() || value <= 0.0 || value > u64::MAX as f64 {
        return Err("native Auto-Tune latency observation is invalid".to_string());
    }
    Ok(value.round().max(1.0) as u64)
}

fn nonnegative_us(value: f64) -> Result<u64, String> {
    if !value.is_finite() || value > u64::MAX as f64 {
        return Err("native Auto-Tune delta observation is invalid".to_string());
    }
    Ok(value.max(0.0).round() as u64)
}

fn capacity_confidence_for_share(share_percent: f64) -> u8 {
    if share_percent <= 2.0 {
        100
    } else if share_percent <= 5.0 {
        96
    } else if share_percent <= 15.0 {
        88
    } else if share_percent <= 30.0 {
        75
    } else if share_percent <= 50.0 {
        60
    } else if share_percent <= 80.0 {
        40
    } else {
        25
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::full_autotune::{
        AutotuneBidirectionalLoadEvidence, AutotuneLoadEvidence, MeasurementTopology,
    };
    use crate::operations::protocol::SpeedtestDirection;

    fn request(phase: AutotuneCapturePhase, capture_byte: &str) -> AutotuneCaptureRequest {
        let loaded = phase == AutotuneCapturePhase::LoadedMeasurement;
        AutotuneCaptureRequest {
            capture_id: capture_byte.repeat(32),
            job_id: "22".repeat(16),
            worker_run_id: "33".repeat(16),
            permit_id: "44".repeat(16),
            instance_name: "wan_sqm".to_string(),
            sequence: if loaded { 2 } else { 1 },
            control_sequence: if loaded { 1 } else { 0 },
            deadline_boot_ms: 100_000,
            phase,
            topology: MeasurementTopology::ShapedBoth,
            direction: loaded.then_some(SpeedtestDirection::Download),
            candidate_dl_kbps: Some(100_000),
            candidate_ul_kbps: Some(50_000),
            load_reference_kbps: loaded.then_some(100_000),
            route_fingerprint: "55".repeat(32),
            sqm_fingerprint: "66".repeat(32),
        }
    }

    fn observation(
        request: &AutotuneCaptureRequest,
        observation_id: u64,
        kind: AutotuneCaptureObservationKind,
    ) -> AutotuneCaptureObservation {
        AutotuneCaptureObservation {
            capture_id: request.capture_id.clone(),
            request_sequence: request.sequence,
            observation_id,
            observed_boot_ms: 1_000 + observation_id,
            kind,
        }
    }

    #[test]
    fn load_trigger_is_bounded_by_reference_and_a_hard_floor() {
        let idle = request(AutotuneCapturePhase::IdleBaseline, "1");
        assert_eq!(bounded_load_trigger_kbps(&idle, 2_000.0).unwrap(), 2_000.0);

        let mut loaded = request(AutotuneCapturePhase::LoadedMeasurement, "2");
        loaded.load_reference_kbps = Some(100_000);
        assert_eq!(
            bounded_load_trigger_kbps(&loaded, 2_000.0).unwrap(),
            2_000.0
        );
        loaded.load_reference_kbps = Some(1_000);
        assert_eq!(bounded_load_trigger_kbps(&loaded, 2_000.0).unwrap(), 50.0);
        loaded.load_reference_kbps = Some(100);
        assert_eq!(bounded_load_trigger_kbps(&loaded, 2_000.0).unwrap(), 10.0);
        loaded.load_reference_kbps = Some(5);
        assert_eq!(bounded_load_trigger_kbps(&loaded, 2_000.0).unwrap(), 10.0);
        assert_eq!(bounded_load_trigger_kbps(&loaded, 1.0).unwrap(), 10.0);
        loaded.load_reference_kbps = Some(100_000);
        assert_eq!(bounded_load_trigger_kbps(&loaded, 5.0).unwrap(), 10.0);
    }

    #[test]
    fn load_trigger_rejects_missing_reference_and_invalid_threshold() {
        let mut loaded = request(AutotuneCapturePhase::LoadedMeasurement, "2");
        loaded.load_reference_kbps = None;
        assert!(bounded_load_trigger_kbps(&loaded, 2_000.0).is_err());
        loaded.load_reference_kbps = Some(1_000);
        assert!(bounded_load_trigger_kbps(&loaded, 0.0).is_err());
        assert!(bounded_load_trigger_kbps(&loaded, f64::NAN).is_err());
    }

    #[test]
    fn bounded_directional_phase_propagates_reference_errors() {
        let mut loaded = request(AutotuneCapturePhase::LoadedMeasurement, "2");
        loaded.load_reference_kbps = None;
        assert!(bounded_directional_load_phase(&loaded, 100.0, 1.0, 2_000.0, 0.05).is_err());
        loaded.load_reference_kbps = Some(1_000);
        assert_eq!(
            bounded_directional_load_phase(&loaded, 50.0, 1.0, 2_000.0, 0.05).unwrap(),
            (true, false)
        );
    }

    #[test]
    fn idle_capture_completes_only_after_all_thresholds() {
        let request = request(AutotuneCapturePhase::IdleBaseline, "1");
        let mut accumulator = AutotuneCaptureAccumulator::new();
        assert_eq!(
            accumulator.admit(&request, 1_000).unwrap().state,
            AutotuneCaptureState::Collecting
        );
        let mut id = 1;
        for latency_us in 10_000..10_009 {
            accumulator
                .observe(observation(
                    &request,
                    id,
                    AutotuneCaptureObservationKind::IcmpSuccess {
                        latency_us: Some(latency_us),
                        delta_us: None,
                    },
                ))
                .unwrap();
            id += 1;
        }
        for latency_us in 20_000..20_015 {
            accumulator
                .observe(observation(
                    &request,
                    id,
                    AutotuneCaptureObservationKind::TransportSuccess {
                        latency_us: Some(latency_us),
                        delta_us: None,
                    },
                ))
                .unwrap();
            id += 1;
        }
        assert_eq!(
            accumulator.snapshot().unwrap().state,
            AutotuneCaptureState::Collecting
        );
        assert_eq!(
            accumulator
                .observe(observation(
                    &request,
                    id,
                    AutotuneCaptureObservationKind::Traffic {
                        background_confidence_percent: 97,
                        contaminated: false,
                    },
                ))
                .unwrap(),
            AutotuneCaptureObservationResult::Complete
        );
        let snapshot = accumulator.snapshot().unwrap();
        assert_eq!(snapshot.state, AutotuneCaptureState::Complete);
        assert_eq!(snapshot.icmp_samples, 9);
        assert_eq!(snapshot.transport_samples, 15);
        assert_eq!(snapshot.idle_median_us, Some(10_004));
        assert_eq!(snapshot.idle_p95_us, Some(10_008));
        assert_eq!(snapshot.background_confidence_percent, Some(97));
    }

    #[test]
    fn loaded_capture_keeps_contamination_as_reviewable_evidence() {
        let request = request(AutotuneCapturePhase::LoadedMeasurement, "7");
        let mut accumulator = AutotuneCaptureAccumulator::new();
        accumulator.admit(&request, 1_000).unwrap();
        let mut id = 1;
        for delta_us in 1_000..1_015 {
            accumulator
                .observe(observation(
                    &request,
                    id,
                    AutotuneCaptureObservationKind::IcmpSuccess {
                        latency_us: None,
                        delta_us: Some(delta_us),
                    },
                ))
                .unwrap();
            id += 1;
        }
        for delta_us in 2_000..2_015 {
            accumulator
                .observe(observation(
                    &request,
                    id,
                    AutotuneCaptureObservationKind::TransportSuccess {
                        latency_us: None,
                        delta_us: Some(delta_us),
                    },
                ))
                .unwrap();
            id += 1;
        }
        accumulator
            .observe(observation(
                &request,
                id,
                AutotuneCaptureObservationKind::IcmpTimeout,
            ))
            .unwrap();
        id += 1;
        accumulator
            .observe(observation(
                &request,
                id,
                AutotuneCaptureObservationKind::Cpu {
                    milli_percent: 72_500,
                },
            ))
            .unwrap();
        id += 1;
        accumulator
            .observe(observation(
                &request,
                id,
                AutotuneCaptureObservationKind::Traffic {
                    background_confidence_percent: 64,
                    contaminated: true,
                },
            ))
            .unwrap();
        let snapshot = accumulator.snapshot().unwrap();
        assert_eq!(snapshot.state, AutotuneCaptureState::Complete);
        assert!(snapshot.contaminated);
        assert_eq!(snapshot.icmp_delta_us, Some(1_014));
        assert_eq!(snapshot.transport_delta_us, Some(2_014));
        assert_eq!(snapshot.loss_ppm, Some(62_500));
        assert_eq!(snapshot.cpu_milli_percent, Some(72_500));
        assert_eq!(snapshot.background_confidence_percent, Some(64));
    }

    #[test]
    fn loaded_transport_deadlines_complete_only_after_count_and_coverage_thresholds() {
        let request = request(AutotuneCapturePhase::LoadedMeasurement, "9");
        let mut accumulator = AutotuneCaptureAccumulator::new();
        accumulator.admit(&request, 1_000).unwrap();
        let mut id = 1;
        for delta_us in 1_000..1_015 {
            accumulator
                .observe(observation(
                    &request,
                    id,
                    AutotuneCaptureObservationKind::IcmpSuccess {
                        latency_us: None,
                        delta_us: Some(delta_us),
                    },
                ))
                .unwrap();
            id += 1;
        }
        accumulator
            .observe(observation(
                &request,
                id,
                AutotuneCaptureObservationKind::Cpu {
                    milli_percent: 40_000,
                },
            ))
            .unwrap();
        id += 1;
        accumulator
            .observe(observation(
                &request,
                id,
                AutotuneCaptureObservationKind::Traffic {
                    background_confidence_percent: 92,
                    contaminated: false,
                },
            ))
            .unwrap();
        id += 1;

        for _ in 0..2 {
            accumulator
                .observe(observation(
                    &request,
                    id,
                    AutotuneCaptureObservationKind::TransportDeadlineExceeded {
                        deadline_us: 5_000_000,
                        delta_lower_bound_us: 4_990_000,
                    },
                ))
                .unwrap();
            id += 1;
        }
        let progress = accumulator.snapshot().unwrap();
        assert_eq!(progress.state, AutotuneCaptureState::Collecting);
        assert_eq!(progress.transport_timeout_count, 2);
        assert_eq!(progress.transport_timeout_total_us, 10_000_000);

        assert_eq!(
            accumulator
                .observe(observation(
                    &request,
                    id,
                    AutotuneCaptureObservationKind::TransportDeadlineExceeded {
                        deadline_us: 5_000_000,
                        delta_lower_bound_us: 4_990_000,
                    },
                ))
                .unwrap(),
            AutotuneCaptureObservationResult::Complete
        );
        let snapshot = accumulator.snapshot().unwrap();
        assert_eq!(snapshot.state, AutotuneCaptureState::Complete);
        assert_eq!(snapshot.transport_samples, 0);
        assert_eq!(snapshot.transport_timeout_count, 3);
        assert_eq!(snapshot.transport_timeout_total_us, 15_000_000);
        assert!(snapshot.transport_censored);
        assert_eq!(snapshot.transport_delta_us, Some(4_990_000));
    }

    #[test]
    fn timeout_never_creates_idle_evidence_and_mixed_loaded_evidence_stays_censored() {
        let idle = request(AutotuneCapturePhase::IdleBaseline, "1");
        assert!(
            transport_deadline_observation_kind(&idle, 5_000_000, Some(10.0), false, false)
                .is_err()
        );

        let loaded = request(AutotuneCapturePhase::LoadedMeasurement, "a");
        assert_eq!(
            transport_deadline_observation_kind(&loaded, 5_000_000, Some(10.0), true, false)
                .unwrap(),
            Some(AutotuneCaptureObservationKind::TransportDeadlineExceeded {
                deadline_us: 5_000_000,
                delta_lower_bound_us: 4_990_000,
            })
        );
        assert_eq!(
            transport_deadline_observation_kind(&loaded, 5_000_000, None, true, false).unwrap(),
            None
        );

        let mut accumulator = AutotuneCaptureAccumulator::new();
        accumulator.admit(&loaded, 1_000).unwrap();
        let mut id = 1;
        accumulator
            .observe(observation(
                &loaded,
                id,
                AutotuneCaptureObservationKind::TransportDeadlineExceeded {
                    deadline_us: 5_000_000,
                    delta_lower_bound_us: 4_990_000,
                },
            ))
            .unwrap();
        id += 1;
        for delta_us in 2_000..2_015 {
            accumulator
                .observe(observation(
                    &loaded,
                    id,
                    AutotuneCaptureObservationKind::TransportSuccess {
                        latency_us: None,
                        delta_us: Some(delta_us),
                    },
                ))
                .unwrap();
            id += 1;
        }
        for delta_us in 1_000..1_015 {
            accumulator
                .observe(observation(
                    &loaded,
                    id,
                    AutotuneCaptureObservationKind::IcmpSuccess {
                        latency_us: None,
                        delta_us: Some(delta_us),
                    },
                ))
                .unwrap();
            id += 1;
        }
        accumulator
            .observe(observation(
                &loaded,
                id,
                AutotuneCaptureObservationKind::Cpu {
                    milli_percent: 40_000,
                },
            ))
            .unwrap();
        id += 1;
        accumulator
            .observe(observation(
                &loaded,
                id,
                AutotuneCaptureObservationKind::Traffic {
                    background_confidence_percent: 92,
                    contaminated: false,
                },
            ))
            .unwrap();
        let snapshot = accumulator.snapshot().unwrap();
        assert_eq!(snapshot.state, AutotuneCaptureState::Complete);
        assert!(snapshot.transport_censored);
        assert_eq!(snapshot.transport_timeout_count, 1);
        assert_eq!(snapshot.transport_delta_us, Some(4_990_000));
    }

    #[test]
    fn bidirectional_load_confidence_uses_the_weaker_direction() {
        let mut request = request(AutotuneCapturePhase::LoadedMeasurement, "8");
        request.direction = Some(SpeedtestDirection::Both);
        request.load_reference_kbps = Some(50_000);
        let evidence = AutotuneBidirectionalLoadEvidence {
            request: request.clone(),
            published_boot_ms: 90_000,
            run_count: 1,
            aggregate_rx_bytes: 1_000_000,
            aggregate_tx_bytes: 700_000,
            confidence_rx_bytes: 1_000_000,
            confidence_tx_bytes: 700_000,
            controlled_rx_wire_bytes: 900_000,
            controlled_tx_wire_bytes: 700_000,
            controlled_rx_payload_bytes: 900_000,
            controlled_tx_payload_bytes: 700_000,
            counter_elapsed_ms: 22_000,
            download_elapsed_ms: 10_000,
            upload_elapsed_ms: 12_000,
            backend_reported_dl_kbps: 720,
            backend_reported_ul_kbps: 466,
            backend_dl_payload_only_runs: 0,
            backend_ul_payload_only_runs: 0,
            realized_dl_kbps: 800,
            realized_ul_kbps: 466,
            achieved_dl_kbps: 720,
            achieved_ul_kbps: 466,
        };
        assert_eq!(
            loaded_traffic_observation_kind(&request, &evidence, 95_000).unwrap(),
            AutotuneCaptureObservationKind::Traffic {
                background_confidence_percent: 90,
                contaminated: false,
            }
        );

        let mut weak_upload = evidence.clone();
        weak_upload.aggregate_tx_bytes = 1_000_000;
        weak_upload.confidence_tx_bytes = 1_000_000;
        weak_upload.realized_ul_kbps = 666;
        assert_eq!(
            loaded_traffic_observation_kind(&request, &weak_upload, 95_000).unwrap(),
            AutotuneCaptureObservationKind::Traffic {
                background_confidence_percent: 70,
                contaminated: true,
            }
        );

        let mut lagging_upload = evidence;
        lagging_upload.aggregate_tx_bytes = 700_000;
        lagging_upload.confidence_tx_bytes = 349_999;
        assert!(loaded_traffic_observation_kind(&request, &lagging_upload, 95_000).is_err());
    }

    #[test]
    fn a_new_capture_clears_samples_and_rejects_old_identity() {
        let first = request(AutotuneCapturePhase::IdleBaseline, "1");
        let second = request(AutotuneCapturePhase::IdleBaseline, "2");
        let mut accumulator = AutotuneCaptureAccumulator::new();
        accumulator.admit(&first, 1_000).unwrap();
        accumulator
            .observe(observation(
                &first,
                1,
                AutotuneCaptureObservationKind::IcmpSuccess {
                    latency_us: Some(10_000),
                    delta_us: None,
                },
            ))
            .unwrap();
        assert_eq!(accumulator.snapshot().unwrap().icmp_samples, 1);
        accumulator.admit(&second, 2_000).unwrap();
        assert_eq!(accumulator.snapshot().unwrap().icmp_samples, 0);
        assert!(accumulator
            .observe(observation(
                &first,
                2,
                AutotuneCaptureObservationKind::IcmpTimeout,
            ))
            .unwrap_err()
            .contains("identity mismatch"));
    }

    #[test]
    fn exact_duplicate_is_idempotent_but_rewrites_and_regression_fail_closed() {
        let request = request(AutotuneCapturePhase::IdleBaseline, "1");
        let mut accumulator = AutotuneCaptureAccumulator::new();
        accumulator.admit(&request, 1_000).unwrap();
        let sample = observation(
            &request,
            5,
            AutotuneCaptureObservationKind::IcmpSuccess {
                latency_us: Some(10_000),
                delta_us: None,
            },
        );
        assert_eq!(
            accumulator.observe(sample.clone()).unwrap(),
            AutotuneCaptureObservationResult::Accepted
        );
        assert_eq!(
            accumulator.observe(sample).unwrap(),
            AutotuneCaptureObservationResult::Duplicate
        );
        assert!(accumulator
            .observe(observation(
                &request,
                5,
                AutotuneCaptureObservationKind::IcmpTimeout,
            ))
            .unwrap_err()
            .contains("rewritten"));
        let mut regressed = observation(&request, 4, AutotuneCaptureObservationKind::IcmpTimeout);
        regressed.observed_boot_ms = 1_006;
        assert!(accumulator
            .observe(regressed)
            .unwrap_err()
            .contains("order regressed"));
        assert_eq!(accumulator.snapshot().unwrap().icmp_samples, 1);
    }

    #[test]
    fn completion_is_immutable_and_observation_time_is_monotonic() {
        let request = request(AutotuneCapturePhase::LoadedMeasurement, "7");
        let mut accumulator = AutotuneCaptureAccumulator::new();
        accumulator.admit(&request, 1_000).unwrap();
        accumulator
            .observe(observation(
                &request,
                1,
                AutotuneCaptureObservationKind::IcmpTimeout,
            ))
            .unwrap();
        let mut reversed_time =
            observation(&request, 2, AutotuneCaptureObservationKind::IcmpTimeout);
        reversed_time.observed_boot_ms = 1_000;
        assert!(accumulator
            .observe(reversed_time)
            .unwrap_err()
            .contains("time is invalid"));

        let mut id = 2;
        for delta_us in 1_000..1_015 {
            accumulator
                .observe(observation(
                    &request,
                    id,
                    AutotuneCaptureObservationKind::IcmpSuccess {
                        latency_us: None,
                        delta_us: Some(delta_us),
                    },
                ))
                .unwrap();
            id += 1;
        }
        for delta_us in 2_000..2_015 {
            accumulator
                .observe(observation(
                    &request,
                    id,
                    AutotuneCaptureObservationKind::TransportSuccess {
                        latency_us: None,
                        delta_us: Some(delta_us),
                    },
                ))
                .unwrap();
            id += 1;
        }
        accumulator
            .observe(observation(
                &request,
                id,
                AutotuneCaptureObservationKind::Cpu {
                    milli_percent: 40_000,
                },
            ))
            .unwrap();
        id += 1;
        assert_eq!(
            accumulator
                .observe(observation(
                    &request,
                    id,
                    AutotuneCaptureObservationKind::Traffic {
                        background_confidence_percent: 100,
                        contaminated: false,
                    },
                ))
                .unwrap(),
            AutotuneCaptureObservationResult::Complete
        );
        id += 1;
        assert!(accumulator
            .observe(observation(
                &request,
                id,
                AutotuneCaptureObservationKind::IcmpTimeout,
            ))
            .unwrap_err()
            .contains("terminally complete"));
        assert!(accumulator
            .reject_current("late-rejection", 2_000)
            .unwrap_err()
            .contains("terminally complete"));
    }

    #[test]
    fn every_observation_buffer_remains_bounded() {
        let request = request(AutotuneCapturePhase::LoadedMeasurement, "7");
        let mut accumulator = AutotuneCaptureAccumulator::new();
        accumulator.admit(&request, 1_000).unwrap();
        for index in 0..1_000 {
            for kind in [
                AutotuneCaptureObservationKind::IcmpSuccess {
                    latency_us: None,
                    delta_us: Some(index),
                },
                AutotuneCaptureObservationKind::TransportSuccess {
                    latency_us: None,
                    delta_us: Some(index),
                },
                AutotuneCaptureObservationKind::Cpu {
                    milli_percent: index as u32 % 100_001,
                },
                AutotuneCaptureObservationKind::Traffic {
                    background_confidence_percent: index as u8 % 101,
                    contaminated: false,
                },
            ] {
                accumulator.record_observation(kind);
            }
        }
        assert_eq!(accumulator.icmp_values_us.len(), MAX_ICMP_OBSERVATIONS);
        assert_eq!(
            accumulator.transport_values_us.len(),
            MAX_TRANSPORT_OBSERVATIONS
        );
        assert_eq!(accumulator.cpu_milli_percent.len(), MAX_CPU_OBSERVATIONS);
        assert_eq!(
            accumulator.background_confidence_percent.len(),
            MAX_CONFIDENCE_OBSERVATIONS
        );
    }

    #[test]
    fn phase_mismatch_and_terminal_rejection_fail_closed() {
        let request = request(AutotuneCapturePhase::IdleBaseline, "1");
        let mut accumulator = AutotuneCaptureAccumulator::new();
        accumulator.admit(&request, 1_000).unwrap();
        assert!(accumulator
            .observe(observation(
                &request,
                1,
                AutotuneCaptureObservationKind::IcmpSuccess {
                    latency_us: None,
                    delta_us: Some(1_000),
                },
            ))
            .unwrap_err()
            .contains("contradicts capture phase"));
        let rejected = accumulator
            .reject_current("capture-route-mismatch", 1_100)
            .unwrap();
        assert_eq!(rejected.state, AutotuneCaptureState::Rejected);
        assert_eq!(
            rejected.diagnostic_code.as_deref(),
            Some("capture-route-mismatch")
        );
        assert!(accumulator
            .observe(observation(
                &request,
                2,
                AutotuneCaptureObservationKind::IcmpTimeout,
            ))
            .unwrap_err()
            .contains("terminally rejected"));
    }

    #[test]
    fn clockless_terminal_rejection_uses_only_the_last_attested_time() {
        let request = request(AutotuneCapturePhase::IdleBaseline, "1");
        let mut accumulator = AutotuneCaptureAccumulator::new();
        accumulator.admit(&request, 1_000).unwrap();
        accumulator
            .observe(observation(
                &request,
                1,
                AutotuneCaptureObservationKind::IcmpSuccess {
                    latency_us: Some(10_000),
                    delta_us: None,
                },
            ))
            .unwrap();

        let rejected = accumulator
            .reject_current_at_last_update("capture-measurement-basis-reset")
            .unwrap();
        assert_eq!(rejected.state, AutotuneCaptureState::Rejected);
        assert_eq!(rejected.updated_boot_ms, 1_001);
        assert_eq!(
            rejected.diagnostic_code.as_deref(),
            Some("capture-measurement-basis-reset")
        );
        assert!(accumulator
            .observe(observation(
                &request,
                2,
                AutotuneCaptureObservationKind::IcmpTimeout,
            ))
            .unwrap_err()
            .contains("terminally rejected"));
    }

    #[test]
    fn normalized_observations_are_directional_and_phase_exact() {
        let idle = request(AutotuneCapturePhase::IdleBaseline, "1");
        assert_eq!(
            icmp_observation_kind(&idle, 12.5, 100.0, 200.0, false, false).unwrap(),
            Some(AutotuneCaptureObservationKind::IcmpSuccess {
                latency_us: Some(12_500),
                delta_us: None,
            })
        );
        assert!(transport_observation_kind(&idle, 20.0, None, true, false)
            .unwrap()
            .is_none());

        let mut loaded = request(AutotuneCapturePhase::LoadedMeasurement, "7");
        loaded.direction = Some(SpeedtestDirection::Download);
        assert_eq!(
            icmp_observation_kind(&loaded, 12.5, -50.0, 9_000.0, true, false).unwrap(),
            Some(AutotuneCaptureObservationKind::IcmpSuccess {
                latency_us: None,
                delta_us: Some(0),
            })
        );
        assert!(
            icmp_observation_kind(&loaded, 12.5, 1_000.0, 9_000.0, false, false)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            transport_observation_kind(&loaded, 45.0, Some(20.0), true, false).unwrap(),
            Some(AutotuneCaptureObservationKind::TransportSuccess {
                latency_us: None,
                delta_us: Some(25_000),
            })
        );
        assert!(
            transport_observation_kind(&loaded, 45.0, Some(20.0), false, true)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn aggregate_counters_grade_only_idle_background() {
        let idle = request(AutotuneCapturePhase::IdleBaseline, "1");
        assert_eq!(
            idle_traffic_observation_kind(&idle, 2_000.0, 1_000.0, 100_000.0, 50_000.0).unwrap(),
            Some(AutotuneCaptureObservationKind::Traffic {
                background_confidence_percent: 100,
                contaminated: false,
            })
        );
        assert_eq!(
            idle_traffic_observation_kind(&idle, 20_000.0, 1_000.0, 100_000.0, 50_000.0).unwrap(),
            Some(AutotuneCaptureObservationKind::Traffic {
                background_confidence_percent: 75,
                contaminated: true,
            })
        );
        let mut upload_only = idle.clone();
        upload_only.topology = MeasurementTopology::UploadOnlyShaped;
        upload_only.candidate_dl_kbps = None;
        assert!(upload_only.validate().is_ok());
        assert!(
            idle_traffic_observation_kind(&upload_only, 2_000.0, 1_000.0, 100_000.0, 50_000.0,)
                .is_ok()
        );
        assert!(
            idle_traffic_observation_kind(&upload_only, 2_000.0, 1_000.0, 0.0, 50_000.0,).is_err()
        );
        let loaded = request(AutotuneCapturePhase::LoadedMeasurement, "7");
        assert!(
            idle_traffic_observation_kind(&loaded, 90_000.0, 45_000.0, 100_000.0, 50_000.0)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn loaded_confidence_preserves_unexplained_wire_bytes() {
        let mut loaded = request(AutotuneCapturePhase::LoadedMeasurement, "7");
        loaded.direction = Some(SpeedtestDirection::Download);
        let mut evidence = AutotuneLoadEvidence {
            request: loaded.clone(),
            published_boot_ms: 2_000,
            run_count: 1,
            aggregate_rx_bytes: 1_100_000,
            aggregate_tx_bytes: 50_000,
            confidence_total_bytes: 1_100_000,
            controlled_wire_bytes: 1_000_000,
            controlled_payload_bytes: 1_000_000,
            counter_elapsed_ms: 10_000,
            direction_elapsed_ms: 10_000,
            backend_reported_kbps: 800,
            backend_consistent_runs: 1,
            backend_payload_only_runs: 0,
            realized_kbps: 880,
            goodput_kbps: 800,
        };
        assert_eq!(
            loaded_traffic_observation_kind(&loaded, &evidence, 2_100).unwrap(),
            AutotuneCaptureObservationKind::Traffic {
                background_confidence_percent: 90,
                contaminated: false,
            }
        );
        evidence.aggregate_rx_bytes = 2_000_000;
        evidence.confidence_total_bytes = 2_000_000;
        evidence.realized_kbps = 1_600;
        assert_eq!(
            loaded_traffic_observation_kind(&loaded, &evidence, 2_100).unwrap(),
            AutotuneCaptureObservationKind::Traffic {
                background_confidence_percent: 50,
                contaminated: true,
            }
        );
        evidence.request.capture_id = "8".repeat(32);
        assert!(loaded_traffic_observation_kind(&loaded, &evidence, 2_100).is_err());

        let mut upload = request(AutotuneCapturePhase::LoadedMeasurement, "9");
        upload.topology = MeasurementTopology::RawUpload;
        upload.direction = Some(SpeedtestDirection::Upload);
        upload.candidate_dl_kbps = Some(50_000);
        upload.candidate_ul_kbps = None;
        upload.load_reference_kbps = Some(50_000);
        let upload_evidence = AutotuneLoadEvidence {
            request: upload.clone(),
            published_boot_ms: 2_000,
            run_count: 1,
            aggregate_rx_bytes: 50_000,
            aggregate_tx_bytes: 970_000,
            confidence_total_bytes: 970_000,
            controlled_wire_bytes: 1_000_000,
            controlled_payload_bytes: 1_000_000,
            counter_elapsed_ms: 10_000,
            direction_elapsed_ms: 10_000,
            backend_reported_kbps: 800,
            backend_consistent_runs: 1,
            backend_payload_only_runs: 0,
            realized_kbps: 800,
            goodput_kbps: 800,
        };
        assert_eq!(
            loaded_traffic_observation_kind(&upload, &upload_evidence, 2_100).unwrap(),
            AutotuneCaptureObservationKind::Traffic {
                background_confidence_percent: 97,
                contaminated: false,
            }
        );
    }

    #[test]
    fn directional_load_accepts_low_realization_and_expected_acks_only() {
        let mut loaded = request(AutotuneCapturePhase::LoadedMeasurement, "7");
        loaded.direction = Some(SpeedtestDirection::Download);
        assert_eq!(
            directional_load_phase(&loaded, 20_000.0, 500.0, 1_000.0, 0.05).unwrap(),
            (true, false)
        );
        assert_eq!(
            directional_load_phase(&loaded, 20_000.0, 2_000.0, 1_000.0, 0.05).unwrap(),
            (true, true)
        );
        assert_eq!(
            directional_load_phase(&loaded, 900.0, 0.0, 1_000.0, 0.05).unwrap(),
            (false, false)
        );
        loaded.direction = Some(SpeedtestDirection::Upload);
        assert_eq!(
            directional_load_phase(&loaded, 400.0, 10_000.0, 1_000.0, 0.05).unwrap(),
            (false, true)
        );
        assert!(directional_load_phase(&loaded, f64::NAN, 1.0, 1.0, 0.05).is_err());
    }

    #[test]
    fn forward_reference_allows_displaced_upload_acks_without_creating_load() {
        let mut loaded = request(AutotuneCapturePhase::LoadedMeasurement, "7");
        loaded.direction = Some(SpeedtestDirection::Upload);

        // The same trailing ACK burst is a false bidirectional phase when the
        // reverse allowance is derived only from this dipped forward window.
        assert_eq!(
            directional_load_phase(&loaded, 6_000.0, 2_000.0, 2_000.0, 0.08).unwrap(),
            (true, true)
        );
        assert_eq!(
            directional_load_phase_with_forward_reference(
                &loaded,
                6_000.0,
                2_000.0,
                2_000.0,
                0.08,
                Some(100_000.0),
            )
            .unwrap(),
            (false, true)
        );

        // Prior evidence never manufactures current forward load.
        assert_eq!(
            directional_load_phase_with_forward_reference(
                &loaded,
                6_000.0,
                1_999.0,
                2_000.0,
                0.08,
                Some(100_000.0),
            )
            .unwrap(),
            (false, false)
        );
        // Material reverse traffic remains a bidirectional mismatch.
        assert_eq!(
            directional_load_phase_with_forward_reference(
                &loaded,
                20_000.0,
                2_000.0,
                2_000.0,
                0.08,
                Some(100_000.0),
            )
            .unwrap(),
            (true, true)
        );
    }
}
