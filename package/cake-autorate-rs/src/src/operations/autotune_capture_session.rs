//! Shared, bounded state machine for one native Auto-Tune capture owner.
//!
//! Probe, counter, route and topology I/O remain outside this module.  Both a
//! normal managed instance and the job-scoped bootstrap owner feed only
//! freshly attested observations into this session, so they cannot diverge in
//! observation sequencing, load-evidence handling, or terminal snapshots.

use super::autotune_capture::{
    loaded_traffic_observation_kind, AutotuneCaptureAccumulator, AutotuneCaptureObservation,
    AutotuneCaptureObservationKind,
};
use super::full_autotune::{
    AutotuneCapturePhase, AutotuneCaptureRequest, AutotuneCaptureSnapshot, AutotuneLoadProof,
};

#[derive(Clone, Debug, Default)]
pub struct AutotuneCaptureSession {
    accumulator: AutotuneCaptureAccumulator,
    observation_sequence: u64,
    load_evidence_capture_id: Option<String>,
}

impl AutotuneCaptureSession {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.accumulator.clear();
        self.load_evidence_capture_id = None;
    }

    pub fn active_request(&self) -> Option<&AutotuneCaptureRequest> {
        self.accumulator.active_request()
    }

    pub fn accepting_observations(&self) -> bool {
        self.accumulator.accepting_observations()
    }

    pub fn admit(
        &mut self,
        request: &AutotuneCaptureRequest,
        boot_ms: u64,
    ) -> Result<AutotuneCaptureSnapshot, String> {
        if self.active_request() != Some(request) {
            self.load_evidence_capture_id = None;
        }
        self.accumulator.admit(request, boot_ms)
    }

    pub fn snapshot(&self) -> Result<AutotuneCaptureSnapshot, String> {
        self.accumulator.snapshot()
    }

    pub fn observe(
        &mut self,
        request: &AutotuneCaptureRequest,
        kind: AutotuneCaptureObservationKind,
        observed_boot_ms: u64,
    ) -> Result<AutotuneCaptureSnapshot, String> {
        self.observe_batch(request, std::iter::once(kind), observed_boot_ms)
    }

    /// Apply one already-attested reactor batch without rebuilding and sorting
    /// a public snapshot after every individual observation.
    ///
    /// The caller owns the I/O and attestation boundary.  Every observation in
    /// the batch receives its own monotonic sequence number, while the shared
    /// observation time makes the batch one exact authority epoch.  Processing
    /// stops at the first terminal state, so surplus probe lines can never be
    /// appended after completion.
    pub fn observe_batch<I>(
        &mut self,
        request: &AutotuneCaptureRequest,
        kinds: I,
        observed_boot_ms: u64,
    ) -> Result<AutotuneCaptureSnapshot, String>
    where
        I: IntoIterator<Item = AutotuneCaptureObservationKind>,
    {
        if self.active_request() != Some(request) {
            return Err(
                "native Auto-Tune observation does not match the active capture".to_string(),
            );
        }
        for kind in kinds {
            if !self.accumulator.accepting_observations() {
                break;
            }
            let observation_id = self
                .observation_sequence
                .checked_add(1)
                .ok_or_else(|| "native Auto-Tune observation sequence exhausted".to_string())?;
            self.accumulator.observe(AutotuneCaptureObservation {
                capture_id: request.capture_id.clone(),
                request_sequence: request.sequence,
                observation_id,
                observed_boot_ms,
                kind,
            })?;
            self.observation_sequence = observation_id;
        }
        self.accumulator.snapshot()
    }

    pub fn observe_load_evidence<E: AutotuneLoadProof + ?Sized>(
        &mut self,
        request: &AutotuneCaptureRequest,
        evidence: &E,
        current_boot_ms: u64,
    ) -> Result<Option<AutotuneCaptureSnapshot>, String> {
        if request.phase != AutotuneCapturePhase::LoadedMeasurement
            || self.load_evidence_capture_id.as_deref() == Some(request.capture_id.as_str())
            || evidence.load_is_immediately_prior_to(request)
        {
            return Ok(None);
        }
        let kind = loaded_traffic_observation_kind(request, evidence, current_boot_ms)?;
        let snapshot = self.observe(request, kind, current_boot_ms)?;
        self.load_evidence_capture_id = Some(request.capture_id.clone());
        Ok(Some(snapshot))
    }

    pub fn reject(
        &mut self,
        diagnostic_code: &str,
        boot_ms: u64,
    ) -> Result<AutotuneCaptureSnapshot, String> {
        self.accumulator.reject_current(diagnostic_code, boot_ms)
    }

    pub fn reject_at_last_update(
        &mut self,
        diagnostic_code: &str,
    ) -> Result<AutotuneCaptureSnapshot, String> {
        self.accumulator
            .reject_current_at_last_update(diagnostic_code)
    }

    pub fn replace_with_rejection(
        &mut self,
        request: &AutotuneCaptureRequest,
        diagnostic_code: &str,
        boot_ms: u64,
    ) -> Result<AutotuneCaptureSnapshot, String> {
        self.clear();
        let bounded_boot_ms = boot_ms.max(1).min(request.deadline_boot_ms);
        self.accumulator.admit(request, bounded_boot_ms)?;
        self.accumulator
            .reject_current(diagnostic_code, bounded_boot_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::autotune_capture::AutotuneCaptureObservationKind;
    use crate::operations::full_autotune::{
        AutotuneCaptureState, AutotuneLoadEvidence, MeasurementTopology,
    };
    use crate::operations::protocol::SpeedtestDirection;

    fn request(phase: AutotuneCapturePhase, byte: &str) -> AutotuneCaptureRequest {
        let loaded = phase == AutotuneCapturePhase::LoadedMeasurement;
        AutotuneCaptureRequest {
            capture_id: byte.repeat(32),
            job_id: "22".repeat(16),
            worker_run_id: "33".repeat(16),
            permit_id: "44".repeat(16),
            instance_name: "wan_sqm".to_string(),
            sequence: if loaded { 2 } else { 1 },
            control_sequence: if loaded { 1 } else { 0 },
            deadline_boot_ms: 100_000,
            phase,
            topology: if loaded {
                MeasurementTopology::ShapedBoth
            } else {
                MeasurementTopology::RawBoth
            },
            direction: loaded.then_some(SpeedtestDirection::Download),
            candidate_dl_kbps: loaded.then_some(100_000),
            candidate_ul_kbps: loaded.then_some(50_000),
            load_reference_kbps: loaded.then_some(100_000),
            transport_baseline_us: loaded.then_some(10_000),
            route_fingerprint: "55".repeat(32),
            sqm_fingerprint: "66".repeat(32),
        }
    }

    fn complete_loaded_evidence(request: &AutotuneCaptureRequest) -> AutotuneLoadEvidence {
        AutotuneLoadEvidence {
            request: request.clone(),
            published_boot_ms: 1_500,
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
        }
    }

    #[test]
    fn session_sequences_observations_and_resets_only_request_state() {
        let idle = request(AutotuneCapturePhase::IdleBaseline, "1");
        let mut session = AutotuneCaptureSession::new();
        session.admit(&idle, 1_000).unwrap();
        let first = session
            .observe(
                &idle,
                AutotuneCaptureObservationKind::IcmpSuccess {
                    latency_us: Some(10_000),
                    delta_us: None,
                },
                1_001,
            )
            .unwrap();
        assert_eq!(first.icmp_samples, 1);

        let loaded = request(AutotuneCapturePhase::LoadedMeasurement, "2");
        session.admit(&loaded, 2_000).unwrap();
        let second = session
            .observe(&loaded, AutotuneCaptureObservationKind::IcmpTimeout, 2_001)
            .unwrap();
        assert_eq!(second.icmp_samples, 0);
        assert_eq!(second.state, AutotuneCaptureState::Collecting);

        session.clear();
        assert!(session.active_request().is_none());
        assert!(!session.accepting_observations());
    }

    #[test]
    fn attested_batch_sequences_once_and_stops_at_the_terminal_boundary() {
        let idle = request(AutotuneCapturePhase::IdleBaseline, "7");
        let mut session = AutotuneCaptureSession::new();
        session.admit(&idle, 1_000).unwrap();

        let icmp = (0..512).map(|offset| AutotuneCaptureObservationKind::IcmpSuccess {
            latency_us: Some(10_000 + offset),
            delta_us: None,
        });
        let snapshot = session.observe_batch(&idle, icmp, 1_001).unwrap();
        assert_eq!(snapshot.icmp_samples, 512);
        assert_eq!(snapshot.state, AutotuneCaptureState::Collecting);

        let terminal = std::iter::once(AutotuneCaptureObservationKind::Traffic {
            background_confidence_percent: 90,
            contaminated: false,
        })
        .chain(
            (0..20).map(|offset| AutotuneCaptureObservationKind::TransportSuccess {
                latency_us: Some(20_000 + offset),
                delta_us: None,
            }),
        );
        let snapshot = session.observe_batch(&idle, terminal, 1_002).unwrap();
        assert_eq!(snapshot.state, AutotuneCaptureState::Complete);
        assert_eq!(snapshot.transport_samples, 15);
        assert_eq!(session.observation_sequence, 512 + 1 + 15);
    }

    #[test]
    fn worker_load_evidence_is_consumed_once_and_only_for_the_exact_capture() {
        let loaded = request(AutotuneCapturePhase::LoadedMeasurement, "2");
        let evidence = complete_loaded_evidence(&loaded);
        let mut session = AutotuneCaptureSession::new();
        session.admit(&loaded, 1_000).unwrap();
        assert!(session
            .observe_load_evidence(&loaded, &evidence, 1_600)
            .unwrap()
            .is_some());
        assert!(session
            .observe_load_evidence(&loaded, &evidence, 1_700)
            .unwrap()
            .is_none());

        let foreign = request(AutotuneCapturePhase::LoadedMeasurement, "3");
        assert!(session
            .observe_load_evidence(&foreign, &evidence, 1_800)
            .is_err());
    }

    #[test]
    fn rejected_request_is_terminal_even_after_its_deadline() {
        let idle = request(AutotuneCapturePhase::IdleBaseline, "1");
        let mut session = AutotuneCaptureSession::new();
        let rejected = session
            .replace_with_rejection(&idle, "capture-runtime-mismatch", 200_000)
            .unwrap();
        assert_eq!(rejected.state, AutotuneCaptureState::Rejected);
        assert_eq!(rejected.updated_boot_ms, idle.deadline_boot_ms);
        assert_eq!(
            rejected.diagnostic_code.as_deref(),
            Some("capture-runtime-mismatch")
        );
        assert!(!session.accepting_observations());
    }
}
