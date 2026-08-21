//! Job-scoped runtime owner for calibrating a target which has no persistent
//! CAKE Autorate/SQM instance yet.
//!
//! It is a separately supervised process boundary, so worker or coordinator
//! death cannot orphan the calibration-private IFB/qdisc/filter namespace.

use super::autotune_apply_openwrt::{
    validate_bootstrap_request_baseline, OpenWrtNativeApplyBackend,
};
use super::autotune_capture_session::AutotuneCaptureSession;
use super::autotune_runtime::{
    AutotuneRuntimePermit, RuntimeBaseline, RuntimeRestoreBaseline, RuntimeSnapshot,
    TemporaryTopologyStage,
};
use super::autotune_runtime_driver::{
    runtime_snapshot_for_control, RuntimeActuatorError, RuntimeDriverOutcome,
    RuntimeOverrideActuator, RuntimeOverrideDriver, RuntimeRestoreBlocker,
    RuntimeRestoreObservation,
};
use super::autotune_runtime_store::{RuntimeOverrideCheckpoint, RuntimeOverrideStore};
use super::event_loop::CalibrationEventLoop;
use super::full_autotune::{
    attest_capture_admission, publish_capture_snapshot, read_capture_request,
    read_capture_snapshot, read_controlled_load_evidence, validate_runtime_permit_admission,
    AutotuneCapturePhase, AutotuneCaptureRequest, AutotuneCaptureSnapshot, AutotuneRuntimeAck,
    AutotuneRuntimeControl,
};
use super::identity::{
    monotonic_boot_ms, read_kernel_uuid, ProcessIdentity, DEFAULT_PROC_ROOT,
    DEFAULT_RANDOM_UUID_PATH,
};
use super::protocol::{
    OperationRequest, OperationTargetState, SpeedtestDirection, MAX_OPERATION_RECORD_BYTES,
};
use ring::digest::{digest, SHA256};
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const OWNER_LOCK_FILE: &str = ".bootstrap-runtime-owner.lock";
const OWNER_CLAIM_FILE: &str = "bootstrap-runtime-owner.claim";
const OWNER_CLAIM_HEADER: &str = "cake-autorate-bootstrap-runtime\t2\towner";
const CAPTURE_REQUEST_FILE: &str = "autotune-capture-request";
const CAPTURE_SNAPSHOT_FILE: &str = "autotune-capture-snapshot";
const LOAD_EVIDENCE_FILE: &str = "autotune-load-evidence";
const IDLE_BASELINE_FILE: &str = "autotune-idle-baseline";
const IDLE_BASELINE_HEADER: &str = "cake-autorate-bootstrap-capture\t1\tidle-baseline";
const MAX_IDLE_BASELINE_BYTES: usize = 4 * 1024;
const MAX_IDLE_BASELINE_US: u64 = 60_000_000;
const MAX_LOADED_PHASE_WINDOWS: usize = 64;
const ACK_CREDIT_HORIZON: Duration = Duration::from_secs(1);
const MAX_ACK_CREDIT_SLICES: usize = 64;
const ACK_CREDIT_SCALE: u128 = 1_000_000;
const MAX_PENDING_ICMP_SAMPLES: usize = 512;
const MAX_PINGER_LINES_PER_DRAIN: usize = 512;
const MAX_PENDING_ICMP_WAIT: Duration = Duration::from_secs(4);
const MAX_LOADED_COUNTER_BURST_COMPLETIONS: u8 = 32;
const MAX_LOADED_TRANSPORT_FLIGHTS_PER_BURST: u8 = 4;
const MAX_LOADED_TRANSPORT_DIAGNOSTICS: u8 = MAX_LOADED_TRANSPORT_FLIGHTS_PER_BURST;
const PROBE_STOPPING_DETAIL: &str =
    "bootstrap capture probes are still stopping before private topology removal";

type CaptureRuntimeResult<T = ()> = Result<T, (&'static str, String)>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct BootstrapIdleBaseline {
    capture_id: String,
    job_id: String,
    worker_run_id: String,
    permit_id: String,
    instance_name: String,
    idle_sequence: u32,
    observed_boot_ms: u64,
    route_fingerprint: String,
    sqm_fingerprint: String,
    policy_id: String,
    policy_sha256: String,
    icmp_baseline_us: u64,
    transport_baseline_us: u64,
}

impl BootstrapIdleBaseline {
    fn from_snapshot(
        snapshot: &AutotuneCaptureSnapshot,
        policy: &super::autotune_capture_policy::AutotuneCapturePolicy,
    ) -> Result<Self, String> {
        snapshot.validate()?;
        if snapshot.state != super::full_autotune::AutotuneCaptureState::Complete
            || snapshot.request.phase != AutotuneCapturePhase::IdleBaseline
        {
            return Err("bootstrap idle baseline requires a complete idle capture".to_string());
        }
        let icmp_baseline_us = snapshot
            .idle_median_us
            .ok_or_else(|| "bootstrap idle capture has no ICMP baseline".to_string())?;
        let transport_baseline_us = snapshot
            .idle_transport_baseline_us
            .ok_or_else(|| "bootstrap idle capture has no transport baseline".to_string())?;
        let value = Self {
            capture_id: snapshot.request.capture_id.clone(),
            job_id: snapshot.request.job_id.clone(),
            worker_run_id: snapshot.request.worker_run_id.clone(),
            permit_id: snapshot.request.permit_id.clone(),
            instance_name: snapshot.request.instance_name.clone(),
            idle_sequence: snapshot.request.sequence,
            observed_boot_ms: snapshot.updated_boot_ms,
            route_fingerprint: snapshot.request.route_fingerprint.clone(),
            sqm_fingerprint: snapshot.request.sqm_fingerprint.clone(),
            policy_id: policy.id().as_str().to_string(),
            policy_sha256: policy.canonical_sha256()?,
            icmp_baseline_us,
            transport_baseline_us,
        };
        value.validate()?;
        Ok(value)
    }

    fn validate(&self) -> Result<(), String> {
        for (name, value, length) in [
            ("capture id", self.capture_id.as_str(), 32),
            ("job id", self.job_id.as_str(), 32),
            ("worker run id", self.worker_run_id.as_str(), 32),
            ("permit id", self.permit_id.as_str(), 32),
            ("route fingerprint", self.route_fingerprint.as_str(), 64),
            ("SQM fingerprint", self.sqm_fingerprint.as_str(), 64),
            ("policy digest", self.policy_sha256.as_str(), 64),
        ] {
            if value.len() != length
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(format!(
                    "bootstrap idle baseline {name} must be {length} lowercase hex bytes"
                ));
            }
        }
        if self.instance_name.is_empty()
            || self.instance_name.len() > 64
            || self
                .instance_name
                .bytes()
                .any(|byte| !byte.is_ascii_alphanumeric() && !b"_-".contains(&byte))
        {
            return Err("bootstrap idle baseline instance name is invalid".to_string());
        }
        let policy_id =
            super::autotune_capture_policy::AutotuneCapturePolicyId::parse(&self.policy_id)
                .ok_or_else(|| "bootstrap idle baseline policy is unsupported".to_string())?;
        if policy_id.canonical_sha256()? != self.policy_sha256 {
            return Err("bootstrap idle baseline policy digest is invalid".to_string());
        }
        if self.idle_sequence == 0
            || self.observed_boot_ms == 0
            || !(1..=MAX_IDLE_BASELINE_US).contains(&self.icmp_baseline_us)
            || !(1..=MAX_IDLE_BASELINE_US).contains(&self.transport_baseline_us)
        {
            return Err("bootstrap idle baseline measurements are invalid".to_string());
        }
        Ok(())
    }

    fn attest_loaded(
        &self,
        request: &AutotuneCaptureRequest,
        policy: &super::autotune_capture_policy::AutotuneCapturePolicy,
    ) -> Result<(), String> {
        self.validate()?;
        request.validate()?;
        if request.phase != AutotuneCapturePhase::LoadedMeasurement
            || self.job_id != request.job_id
            || self.worker_run_id != request.worker_run_id
            || self.permit_id != request.permit_id
            || self.instance_name != request.instance_name
            || self.route_fingerprint != request.route_fingerprint
            || self.sqm_fingerprint != request.sqm_fingerprint
            || self.idle_sequence >= request.sequence
            || self.observed_boot_ms > request.deadline_boot_ms
            || self.policy_id != policy.id().as_str()
            || self.policy_sha256 != policy.canonical_sha256()?
            || request.transport_baseline_us != Some(self.transport_baseline_us)
        {
            return Err(
                "bootstrap idle baseline does not authorize this loaded capture".to_string(),
            );
        }
        Ok(())
    }

    fn same_idle_capture(&self, request: &AutotuneCaptureRequest) -> bool {
        request.phase == AutotuneCapturePhase::IdleBaseline
            && self.capture_id == request.capture_id
            && self.job_id == request.job_id
            && self.worker_run_id == request.worker_run_id
            && self.permit_id == request.permit_id
            && self.instance_name == request.instance_name
            && self.idle_sequence == request.sequence
            && self.route_fingerprint == request.route_fingerprint
            && self.sqm_fingerprint == request.sqm_fingerprint
    }

    fn same_job(&self, request: &AutotuneCaptureRequest) -> bool {
        self.job_id == request.job_id
            && self.worker_run_id == request.worker_run_id
            && self.permit_id == request.permit_id
            && self.instance_name == request.instance_name
            && self.route_fingerprint == request.route_fingerprint
            && self.sqm_fingerprint == request.sqm_fingerprint
    }

    fn encode(&self) -> Result<String, String> {
        self.validate()?;
        Ok(format!(
            concat!(
                "{}\n",
                "capture_id={}\njob_id={}\nworker_run_id={}\npermit_id={}\n",
                "instance_name={}\nidle_sequence={}\nobserved_boot_ms={}\n",
                "route_fingerprint={}\nsqm_fingerprint={}\npolicy_id={}\npolicy_sha256={}\n",
                "icmp_baseline_us={}\ntransport_baseline_us={}\n"
            ),
            IDLE_BASELINE_HEADER,
            self.capture_id,
            self.job_id,
            self.worker_run_id,
            self.permit_id,
            self.instance_name,
            self.idle_sequence,
            self.observed_boot_ms,
            self.route_fingerprint,
            self.sqm_fingerprint,
            self.policy_id,
            self.policy_sha256,
            self.icmp_baseline_us,
            self.transport_baseline_us,
        ))
    }

    fn decode(input: &str) -> Result<Self, String> {
        if input.len() > MAX_IDLE_BASELINE_BYTES || !input.ends_with('\n') {
            return Err("bootstrap idle baseline is unbounded or unterminated".to_string());
        }
        let mut lines = input.lines();
        if lines.next() != Some(IDLE_BASELINE_HEADER) {
            return Err("bootstrap idle baseline header is unsupported".to_string());
        }
        let capture_id = idle_baseline_field(&mut lines, "capture_id")?;
        let job_id = idle_baseline_field(&mut lines, "job_id")?;
        let worker_run_id = idle_baseline_field(&mut lines, "worker_run_id")?;
        let permit_id = idle_baseline_field(&mut lines, "permit_id")?;
        let instance_name = idle_baseline_field(&mut lines, "instance_name")?;
        let idle_sequence = parse_idle_baseline_number(
            "idle_sequence",
            &idle_baseline_field(&mut lines, "idle_sequence")?,
        )?;
        let observed_boot_ms = parse_idle_baseline_number(
            "observed_boot_ms",
            &idle_baseline_field(&mut lines, "observed_boot_ms")?,
        )?;
        let route_fingerprint = idle_baseline_field(&mut lines, "route_fingerprint")?;
        let sqm_fingerprint = idle_baseline_field(&mut lines, "sqm_fingerprint")?;
        let policy_id = idle_baseline_field(&mut lines, "policy_id")?;
        let policy_sha256 = idle_baseline_field(&mut lines, "policy_sha256")?;
        let icmp_baseline_us = parse_idle_baseline_number(
            "icmp_baseline_us",
            &idle_baseline_field(&mut lines, "icmp_baseline_us")?,
        )?;
        let transport_baseline_us = parse_idle_baseline_number(
            "transport_baseline_us",
            &idle_baseline_field(&mut lines, "transport_baseline_us")?,
        )?;
        if lines.next().is_some() {
            return Err("bootstrap idle baseline has trailing fields".to_string());
        }
        let value = Self {
            capture_id,
            job_id,
            worker_run_id,
            permit_id,
            instance_name,
            idle_sequence,
            observed_boot_ms,
            route_fingerprint,
            sqm_fingerprint,
            policy_id,
            policy_sha256,
            icmp_baseline_us,
            transport_baseline_us,
        };
        value.validate()?;
        if value.encode()? != input {
            return Err("bootstrap idle baseline is not canonical".to_string());
        }
        Ok(value)
    }
}

fn idle_baseline_field<'a>(
    lines: &mut impl Iterator<Item = &'a str>,
    expected: &str,
) -> Result<String, String> {
    let line = lines
        .next()
        .ok_or_else(|| format!("bootstrap idle baseline is missing {expected}"))?;
    let (name, value) = line
        .split_once('=')
        .ok_or_else(|| format!("bootstrap idle baseline field {expected} is malformed"))?;
    if name != expected || value.is_empty() || value.contains(['\r', '\n', '=']) {
        return Err(format!("bootstrap idle baseline expected field {expected}"));
    }
    Ok(value.to_string())
}

fn parse_idle_baseline_number<T>(name: &str, value: &str) -> Result<T, String>
where
    T: std::str::FromStr,
{
    value
        .parse::<T>()
        .map_err(|_| format!("bootstrap idle baseline {name} is invalid"))
}

fn read_idle_baseline(runtime_dir: &Path) -> Result<Option<BootstrapIdleBaseline>, String> {
    let path = runtime_dir.join(IDLE_BASELINE_FILE);
    match fs::symlink_metadata(&path) {
        Ok(_) => BootstrapIdleBaseline::decode(&super::rating::read_private_bounded(
            &path,
            MAX_IDLE_BASELINE_BYTES,
        )?)
        .map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "unable to inspect bootstrap idle baseline: {error}"
        )),
    }
}

fn publish_idle_baseline(
    runtime_dir: &Path,
    baseline: &BootstrapIdleBaseline,
) -> Result<(), String> {
    if let Some(existing) = read_idle_baseline(runtime_dir)? {
        return if &existing == baseline {
            Ok(())
        } else {
            Err("bootstrap idle baseline cannot rewrite durable evidence".to_string())
        };
    }
    let bytes = baseline.encode()?;
    super::rating::atomic_private_write(&runtime_dir.join(IDLE_BASELINE_FILE), bytes.as_bytes())
}

#[derive(Clone, Debug)]
struct LoadedPhaseWindow {
    request: AutotuneCaptureRequest,
    observed_start: Instant,
    observed_end: Instant,
    phase: (bool, bool),
}

#[derive(Clone, Copy, Debug)]
struct LoadedPhysicalDelta {
    delta: super::autotune_counter::AutotuneCounterDelta,
    phase: Option<(bool, bool)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LoadedPhaseLookup {
    Covered((bool, bool)),
    Pending,
    Uncovered,
}

#[derive(Clone, Debug)]
struct CounterPhaseUpdate {
    observed_at: Instant,
    window: Option<LoadedPhaseWindow>,
    physical_delta: Option<LoadedPhysicalDelta>,
    topology_attestation: Option<LoadedTopologyAttestation>,
    authority: Option<LoadedTransportAuthority>,
    purpose: LoadedCounterReadPurpose,
    preserve_ack_credit: bool,
    diagnostic: LoadedCounterDiagnostic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LoadedCounterReadPurpose {
    AttestedBaseline,
    Evidence,
    Rebase,
    FlightEvidence,
    FlightPost,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LoadedCounterCycleState {
    NeedAttestedBaseline,
    BaselineReady,
    NeedRebase,
    NeedFlightEvidence,
    FlightReady,
    FlightActive,
}

impl LoadedCounterCycleState {
    fn next_read(self, transport_result_pending: bool) -> Option<LoadedCounterReadPurpose> {
        match self {
            Self::NeedAttestedBaseline => Some(LoadedCounterReadPurpose::AttestedBaseline),
            Self::BaselineReady => Some(LoadedCounterReadPurpose::Evidence),
            Self::NeedRebase => Some(LoadedCounterReadPurpose::Rebase),
            Self::NeedFlightEvidence => Some(LoadedCounterReadPurpose::FlightEvidence),
            Self::FlightReady => None,
            Self::FlightActive if transport_result_pending => {
                Some(LoadedCounterReadPurpose::FlightPost)
            }
            Self::FlightActive => None,
        }
    }

    fn after_read(
        self,
        purpose: LoadedCounterReadPurpose,
        endpoint_present: bool,
    ) -> Result<Self, String> {
        match (self, purpose) {
            (Self::NeedAttestedBaseline, LoadedCounterReadPurpose::AttestedBaseline) => {
                Ok(if endpoint_present {
                    Self::BaselineReady
                } else {
                    Self::NeedAttestedBaseline
                })
            }
            (Self::BaselineReady, LoadedCounterReadPurpose::Evidence) => Ok(Self::NeedRebase),
            (Self::NeedRebase, LoadedCounterReadPurpose::Rebase) => Ok(if endpoint_present {
                Self::NeedFlightEvidence
            } else {
                Self::NeedAttestedBaseline
            }),
            (Self::NeedFlightEvidence, LoadedCounterReadPurpose::FlightEvidence) => {
                Ok(if endpoint_present {
                    Self::FlightReady
                } else {
                    Self::NeedAttestedBaseline
                })
            }
            (Self::FlightActive, LoadedCounterReadPurpose::FlightPost) => Ok(Self::NeedRebase),
            _ => Err(format!(
                "bootstrap loaded counter transition is invalid ({self:?}, {purpose:?})"
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LoadedCounterDiagnosticOutcome {
    CountersMissing,
    Priming,
    DeltaStale,
    WindowAccumulating,
    WindowAccepted,
}

impl LoadedCounterDiagnosticOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::CountersMissing => "counters-missing",
            Self::Priming => "priming",
            Self::DeltaStale => "delta-stale",
            Self::WindowAccumulating => "window-accumulating",
            Self::WindowAccepted => "window-accepted",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LoadedCounterDiagnostic {
    outcome: LoadedCounterDiagnosticOutcome,
    read_latency_ms: u64,
    delta_span_ms: Option<u64>,
    window_span_ms: Option<u64>,
    download_kbps: Option<u64>,
    upload_kbps: Option<u64>,
}

fn bounded_duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn bounded_rate_kbps(value: f64) -> Option<u64> {
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    Some(value.round().min(u64::MAX as f64) as u64)
}

fn invalid_loaded_transport_settlement_diagnostic(
    attestation: crate::AutotuneTransportAttestation,
    expected_phase: (bool, bool),
) -> Option<String> {
    if attestation.valid {
        return None;
    }
    let coverage = attestation.coverage;
    let show = |value: Option<Duration>| {
        value.map_or_else(
            || "-".to_string(),
            |value| bounded_duration_ms(value).to_string(),
        )
    };
    Some(format!(
        "reason={} total_ms={} matching_ms={} longest_mismatch_ms={} expected_phase={}:{}",
        attestation.reason,
        show(coverage.map(|value| value.total)),
        show(coverage.map(|value| value.matching)),
        show(coverage.map(|value| value.longest_mismatch)),
        u8::from(expected_phase.0),
        u8::from(expected_phase.1),
    ))
}

fn loaded_transport_backend_failure_diagnostic(
    probe_id: u64,
    failure: &crate::transport_probe::TransportProbeFailure,
) -> String {
    let kind = match failure.kind() {
        crate::transport_probe::TransportProbeFailureKind::DeadlineExceeded => "deadline",
        crate::transport_probe::TransportProbeFailureKind::Other => "other",
    };
    // Backend messages may contain host-library or endpoint detail. Preserve
    // the typed outcome needed to distinguish apparatus failure from physical
    // settlement without copying an unbounded backend string into daemon logs.
    format!("reason=backend-failure probe_id={probe_id} kind={kind}")
}

fn loaded_counter_diagnostic(
    fence: &CounterReadFence,
    completed_at: Instant,
    counters_present: bool,
    observation: super::autotune_counter::AutotuneCounterObservation,
    window_accepted: bool,
) -> LoadedCounterDiagnostic {
    let outcome = if !counters_present {
        LoadedCounterDiagnosticOutcome::CountersMissing
    } else if observation.delta.is_none() {
        LoadedCounterDiagnosticOutcome::Priming
    } else if observation
        .delta
        .is_some_and(|delta| !delta.within_maximum_span)
    {
        LoadedCounterDiagnosticOutcome::DeltaStale
    } else if observation.rate_window.is_none() {
        LoadedCounterDiagnosticOutcome::WindowAccumulating
    } else if window_accepted {
        LoadedCounterDiagnosticOutcome::WindowAccepted
    } else {
        // A present, structurally valid rate window is either accepted or the
        // capture is rejected before this diagnostic is emitted.  Keep the
        // fail-closed value useful if that invariant is strengthened later.
        LoadedCounterDiagnosticOutcome::WindowAccumulating
    };
    LoadedCounterDiagnostic {
        outcome,
        read_latency_ms: bounded_duration_ms(
            completed_at.saturating_duration_since(fence.scheduled_at),
        ),
        delta_span_ms: observation.delta.map(|delta| {
            bounded_duration_ms(
                delta
                    .observed_end
                    .saturating_duration_since(delta.observed_start),
            )
        }),
        window_span_ms: observation.rate_window.map(|window| {
            bounded_duration_ms(
                window
                    .observed_end
                    .saturating_duration_since(window.observed_start),
            )
        }),
        download_kbps: observation
            .rate_window
            .and_then(|window| bounded_rate_kbps(window.download_kbps)),
        upload_kbps: observation
            .rate_window
            .and_then(|window| bounded_rate_kbps(window.upload_kbps)),
    }
}

/// One exact asynchronous counter read and its immutable role in the loaded
/// interval state machine.  Full topology attestation is deliberately outside
/// every admitted byte interval: `AttestedBaseline` is read immediately after
/// a successful pre-attestation, while `Evidence` and `FlightPost` are accepted
/// only after their completion passes a full post-attestation. `Rebase` then
/// establishes a new endpoint after that slow attestation and
/// `FlightEvidence` proves the current loaded phase before dispatch.
#[derive(Clone, Debug)]
struct CounterReadFence {
    request: AutotuneCaptureRequest,
    epoch: u64,
    scheduled_at: Instant,
    purpose: LoadedCounterReadPurpose,
}

/// One full topology attestation completed before the exact rebase endpoint.
/// It cannot by itself authorize transport: a later, cadence-spaced physical
/// delta must begin after this token and match the immutable loaded phase.
#[derive(Clone, Debug)]
struct LoadedTopologyAttestation {
    request: AutotuneCaptureRequest,
    epoch: u64,
    attested_at: Instant,
}

/// A fresh, directionally classified Rebase-to-FlightEvidence counter delta
/// may authorize one transport dispatch. Reusing it for another flight would
/// widen the external-mutation race, so scheduling consumes it.
#[derive(Clone, Debug)]
struct LoadedTransportAuthority {
    request: AutotuneCaptureRequest,
    epoch: u64,
    attested_at: Instant,
    observed_start: Instant,
    observed_end: Instant,
}

/// One exact, non-overlapping physical counter slice.  Remaining forward
/// credit may be consumed only by this or a later reverse observation in the
/// same bounded request/epoch window.  Uncovered reverse units are immutable:
/// later forward traffic cannot retroactively authorize earlier traffic.
#[derive(Clone, Debug)]
struct LoadedAckCreditSlice {
    observed_start: Instant,
    observed_end: Instant,
    remaining_forward_credit_units: u128,
    total_reverse_bytes: u64,
    uncovered_reverse_units: u128,
}

/// Causal byte authority for compressed TCP ACKs.  Rolling rate windows may
/// overlap, so they are never used to mint or consume this state.  Only exact
/// consecutive physical counter deltas enter the FIFO.
#[derive(Clone, Debug)]
struct LoadedAckCredit {
    request: AutotuneCaptureRequest,
    epoch: u64,
    slices: VecDeque<LoadedAckCreditSlice>,
    remaining_forward_credit_units: u128,
}

impl LoadedAckCredit {
    fn reset_for(request: &AutotuneCaptureRequest, epoch: u64) -> Self {
        Self {
            request: request.clone(),
            epoch,
            slices: VecDeque::with_capacity(MAX_ACK_CREDIT_SLICES),
            remaining_forward_credit_units: 0,
        }
    }

    fn checked_mul(left: u128, right: u128, context: &str) -> Result<u128, String> {
        left.checked_mul(right)
            .ok_or_else(|| format!("bootstrap ACK credit {context} overflow"))
    }

    fn scaled_bytes_for_rate(
        kbps: u128,
        duration: Duration,
        scale: u128,
        context: &str,
    ) -> Result<u128, String> {
        let numerator = Self::checked_mul(kbps, 1_000, context)?;
        let numerator = Self::checked_mul(numerator, duration.as_nanos(), context)?;
        let numerator = Self::checked_mul(numerator, scale, context)?;
        Ok(numerator / 8_000_000_000)
    }

    fn exact_threshold_kbps(active_threshold_kbps: f64) -> Result<u128, String> {
        if !active_threshold_kbps.is_finite()
            || active_threshold_kbps <= 0.0
            || active_threshold_kbps.fract() != 0.0
            || active_threshold_kbps > u64::MAX as f64
        {
            return Err("bootstrap ACK credit load threshold is invalid".to_string());
        }
        Ok(active_threshold_kbps as u128)
    }

    fn expire(&mut self, observed_at: Instant) -> Result<(), String> {
        while self.slices.front().is_some_and(|slice| {
            slice
                .observed_end
                .checked_add(ACK_CREDIT_HORIZON)
                .is_none_or(|expires_at| expires_at <= observed_at)
        }) {
            let expired = self
                .slices
                .pop_front()
                .ok_or_else(|| "bootstrap ACK credit FIFO underflow".to_string())?;
            self.remaining_forward_credit_units = self
                .remaining_forward_credit_units
                .checked_sub(expired.remaining_forward_credit_units)
                .ok_or_else(|| "bootstrap ACK credit accounting underflow".to_string())?;
        }
        Ok(())
    }

    fn consume_reverse(&mut self, reverse_bytes: u64) -> Result<u128, String> {
        let mut required =
            Self::checked_mul(u128::from(reverse_bytes), ACK_CREDIT_SCALE, "reverse-byte")?;
        for slice in &mut self.slices {
            if required == 0 {
                break;
            }
            let consumed = required.min(slice.remaining_forward_credit_units);
            slice.remaining_forward_credit_units -= consumed;
            self.remaining_forward_credit_units = self
                .remaining_forward_credit_units
                .checked_sub(consumed)
                .ok_or_else(|| "bootstrap ACK credit accounting underflow".to_string())?;
            required -= consumed;
        }
        Ok(required)
    }

    fn material_reverse(&self, active_threshold_kbps: f64) -> Result<bool, String> {
        let threshold_kbps = Self::exact_threshold_kbps(active_threshold_kbps)?;
        let mut observed = Duration::ZERO;
        let mut total_reverse_bytes = 0u128;
        let mut uncovered_reverse_units = 0u128;
        for slice in &self.slices {
            observed = observed
                .checked_add(
                    slice
                        .observed_end
                        .checked_duration_since(slice.observed_start)
                        .ok_or_else(|| {
                            "bootstrap ACK credit slice interval is invalid".to_string()
                        })?,
                )
                .ok_or_else(|| "bootstrap ACK credit duration overflow".to_string())?;
            total_reverse_bytes = total_reverse_bytes
                .checked_add(u128::from(slice.total_reverse_bytes))
                .ok_or_else(|| "bootstrap ACK credit reverse-byte overflow".to_string())?;
            uncovered_reverse_units = uncovered_reverse_units
                .checked_add(slice.uncovered_reverse_units)
                .ok_or_else(|| "bootstrap ACK credit uncovered-byte overflow".to_string())?;
        }
        let base_units =
            Self::scaled_bytes_for_rate(threshold_kbps, observed, ACK_CREDIT_SCALE, "base")?;
        let total_reverse_units =
            Self::checked_mul(total_reverse_bytes, ACK_CREDIT_SCALE, "reverse-byte")?;
        Ok(total_reverse_units > base_units && uncovered_reverse_units > 0)
    }

    fn observe(
        slot: &mut Option<Self>,
        request: &AutotuneCaptureRequest,
        epoch: u64,
        delta: super::autotune_counter::AutotuneCounterDelta,
        active_threshold_kbps: f64,
        ack_ratio_ppm: u32,
    ) -> Result<Option<bool>, String> {
        let (forward_bytes, reverse_bytes) = match request.direction {
            Some(SpeedtestDirection::Download) => (delta.download_bytes, delta.upload_bytes),
            Some(SpeedtestDirection::Upload) => (delta.upload_bytes, delta.download_bytes),
            Some(SpeedtestDirection::Both) | None => {
                *slot = None;
                return Ok(None);
            }
        };
        if !delta.within_maximum_span || delta.observed_end <= delta.observed_start {
            *slot = None;
            return Ok(None);
        }
        if ack_ratio_ppm > ACK_CREDIT_SCALE as u32 {
            return Err("bootstrap ACK credit ratio is invalid".to_string());
        }
        if slot
            .as_ref()
            .is_some_and(|credit| credit.request != *request || credit.epoch != epoch)
        {
            *slot = None;
        }
        let credit = slot.get_or_insert_with(|| Self::reset_for(request, epoch));
        credit.expire(delta.observed_end)?;
        if credit
            .slices
            .back()
            .is_some_and(|slice| slice.observed_end > delta.observed_start)
        {
            *slot = None;
            return Err("bootstrap ACK credit deltas overlap".to_string());
        }
        if credit.slices.len() >= MAX_ACK_CREDIT_SLICES {
            *slot = None;
            return Err("bootstrap ACK credit slice bound exceeded".to_string());
        }

        let reference_kbps = request
            .load_reference_kbps
            .filter(|value| *value > 0)
            .ok_or_else(|| "bootstrap ACK credit has no load reference".to_string())?;
        let capacity_units = Self::scaled_bytes_for_rate(
            u128::from(reference_kbps),
            ACK_CREDIT_HORIZON,
            u128::from(ack_ratio_ppm),
            "capacity",
        )?;
        let minted_units = Self::checked_mul(
            u128::from(forward_bytes),
            u128::from(ack_ratio_ppm),
            "forward-byte",
        )?;
        let available_capacity = capacity_units
            .checked_sub(credit.remaining_forward_credit_units)
            .ok_or_else(|| "bootstrap ACK credit exceeds its capacity".to_string())?;
        let admitted_units = minted_units.min(available_capacity);
        credit.remaining_forward_credit_units = credit
            .remaining_forward_credit_units
            .checked_add(admitted_units)
            .ok_or_else(|| "bootstrap ACK credit accounting overflow".to_string())?;
        credit.slices.push_back(LoadedAckCreditSlice {
            observed_start: delta.observed_start,
            observed_end: delta.observed_end,
            remaining_forward_credit_units: admitted_units,
            total_reverse_bytes: reverse_bytes,
            uncovered_reverse_units: 0,
        });
        let uncovered_reverse_units = credit.consume_reverse(reverse_bytes)?;
        credit
            .slices
            .back_mut()
            .ok_or_else(|| "bootstrap ACK credit FIFO is empty".to_string())?
            .uncovered_reverse_units = uncovered_reverse_units;
        credit.material_reverse(active_threshold_kbps).map(Some)
    }
}

impl LoadedTransportAuthority {
    fn permits(
        &self,
        request: &AutotuneCaptureRequest,
        epoch: u64,
        newest_delta: Option<&LoadedPhysicalDelta>,
        now: Instant,
        freshness: Duration,
    ) -> bool {
        let expected = crate::AutotuneTransportControl::expected_phase(request).ok();
        &self.request == request
            && self.epoch == epoch
            && newest_delta.is_some_and(|delta| {
                delta.delta.observed_start == self.observed_start
                    && delta.delta.observed_end == self.observed_end
                    && delta.phase == expected
            })
            && self
                .observed_start
                .checked_duration_since(self.attested_at)
                .is_some_and(|age| age <= freshness)
            && now
                .checked_duration_since(self.attested_at)
                .is_some_and(|age| age <= freshness)
            && now
                .checked_duration_since(self.observed_end)
                .is_some_and(|age| age <= freshness)
    }
}

impl LoadedTopologyAttestation {
    fn authorize(
        &self,
        request: &AutotuneCaptureRequest,
        epoch: u64,
        delta: &LoadedPhysicalDelta,
        expected: (bool, bool),
        freshness: Duration,
    ) -> Option<LoadedTransportAuthority> {
        (&self.request == request
            && self.epoch == epoch
            && delta.phase == Some(expected)
            && delta.delta.within_maximum_span
            && delta.delta.observed_end > delta.delta.observed_start
            && delta
                .delta
                .observed_start
                .checked_duration_since(self.attested_at)
                .is_some_and(|age| age <= freshness))
        .then(|| LoadedTransportAuthority {
            request: request.clone(),
            epoch,
            attested_at: self.attested_at,
            observed_start: delta.delta.observed_start,
            observed_end: delta.delta.observed_end,
        })
    }
}

fn loaded_reactor_deadline(rate: Duration, cpu: Duration, pending_counter_fence: bool) -> Duration {
    if pending_counter_fence {
        // The controlled-counter completion writes the shared eventfd.  A
        // zero rate deadline while that read is in flight turns the reactor
        // into a busy loop and lets unrelated synchronous attestations consume
        // the short completion fence.  CPU remains a bounded safety wake.
        cpu
    } else {
        rate.min(cpu)
    }
}

fn loaded_counter_barrier_required(
    accepting_observations: bool,
    loaded_capture: bool,
    counter_in_flight: bool,
    counter_fence_pending: bool,
) -> Result<bool, String> {
    if !accepting_observations || !loaded_capture {
        return Ok(false);
    }
    if counter_in_flight != counter_fence_pending {
        return Err("bootstrap loaded counter sampler and exact read fence disagree".to_string());
    }
    Ok(counter_in_flight)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LoadedCounterBurstState {
    Idle,
    Priming {
        remaining_completions: u8,
        remaining_flights: u8,
    },
    BracketingFlight {
        remaining_completions: u8,
        remaining_flights: u8,
    },
}

impl LoadedCounterBurstState {
    fn begin() -> Self {
        Self::Priming {
            remaining_completions: MAX_LOADED_COUNTER_BURST_COMPLETIONS,
            remaining_flights: MAX_LOADED_TRANSPORT_FLIGHTS_PER_BURST,
        }
    }

    fn active(self) -> bool {
        self != Self::Idle
    }

    fn bracketing_flight(self) -> bool {
        matches!(self, Self::BracketingFlight { .. })
    }

    fn after_reactor(
        self,
        accepting_observations: bool,
        counter_completed: bool,
        counter_active: bool,
        transport_active: bool,
    ) -> Self {
        if !accepting_observations {
            return Self::Idle;
        }
        match self {
            Self::Idle => Self::Idle,
            Self::Priming {
                mut remaining_completions,
                remaining_flights,
            } => {
                if counter_completed {
                    remaining_completions = remaining_completions.saturating_sub(1);
                }
                if transport_active {
                    // Once the worker accepts the flight, retain its exact
                    // post-counter settlement fence.  The bounded completion
                    // and flight budgets decide when fairness yields; variable
                    // attestation latency is never a transition source.
                    Self::BracketingFlight {
                        remaining_completions,
                        remaining_flights,
                    }
                } else if counter_active {
                    // Keep the immutable read-purpose/cycle pair until the
                    // completion is consumed; resetting the cycle here would
                    // turn that legitimate completion into an identity
                    // mismatch.  This branch never admits another read.
                    Self::Priming {
                        remaining_completions,
                        remaining_flights,
                    }
                } else if remaining_completions == 0 {
                    Self::Idle
                } else {
                    Self::Priming {
                        remaining_completions,
                        remaining_flights,
                    }
                }
            }
            Self::BracketingFlight {
                mut remaining_completions,
                remaining_flights,
            } => {
                if counter_completed {
                    remaining_completions = remaining_completions.saturating_sub(1);
                }
                if transport_active || counter_active {
                    return Self::BracketingFlight {
                        remaining_completions,
                        remaining_flights,
                    };
                }
                if remaining_completions == 0 || remaining_flights <= 1 {
                    Self::Idle
                } else {
                    // The post-flight physical endpoint may become the exact
                    // pre-flight authority for the next probe.  Keep it only
                    // inside this bounded burst and continue counter events
                    // while the immutable transport cadence/load-hold gate is
                    // not yet ready.  No delayed action is synthesized here.
                    Self::Priming {
                        remaining_completions,
                        remaining_flights: remaining_flights - 1,
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LoadedCounterReactorWait {
    Inactive,
    CounterCompletion,
    TransportCompletion,
    SamplingCadence(Duration),
}

#[derive(Debug)]
struct PendingIcmpSample {
    sample: crate::Sample,
    observed_at: Instant,
}

enum PendingIcmpDisposition {
    Wait,
    Ignore,
    Observe(super::autotune_capture::AutotuneCaptureObservationKind),
}

#[cfg(test)]
fn loaded_phase_window(
    request: &AutotuneCaptureRequest,
    sample: super::autotune_counter::AutotuneCounterRateWindow,
    policy: &super::autotune_capture_policy::AutotuneCapturePolicy,
) -> Result<LoadedPhaseWindow, String> {
    if !sample.fresh
        || sample.observed_end <= sample.observed_start
        || !sample.download_kbps.is_finite()
        || sample.download_kbps < 0.0
        || !sample.upload_kbps.is_finite()
        || sample.upload_kbps < 0.0
    {
        return Err("bootstrap controlled counter window is invalid".to_string());
    }
    let active_threshold = super::autotune_capture::bounded_load_trigger_kbps(
        request,
        policy.load_threshold_kbps() as f64,
    )?;
    let phase = super::autotune_capture::directional_load_phase(
        request,
        sample.download_kbps,
        sample.upload_kbps,
        active_threshold,
        f64::from(policy.reverse_ack_ratio_ppm()) / 1_000_000.0,
    )?;
    Ok(LoadedPhaseWindow {
        request: request.clone(),
        observed_start: sample.observed_start,
        observed_end: sample.observed_end,
        phase,
    })
}

fn loaded_phase_window_with_ack_credit(
    request: &AutotuneCaptureRequest,
    observation: super::autotune_counter::AutotuneCounterObservation,
    policy: &super::autotune_capture_policy::AutotuneCapturePolicy,
    counter_epoch: u64,
    ack_credit: &mut Option<LoadedAckCredit>,
) -> Result<(Option<LoadedPhaseWindow>, Option<LoadedPhysicalDelta>, bool), String> {
    let Some(delta) = observation.delta else {
        *ack_credit = None;
        return Ok((None, None, false));
    };
    if !delta.within_maximum_span || delta.observed_end <= delta.observed_start {
        *ack_credit = None;
        return Ok((
            None,
            (delta.observed_end > delta.observed_start)
                .then_some(LoadedPhysicalDelta { delta, phase: None }),
            false,
        ));
    }
    if request.phase != AutotuneCapturePhase::LoadedMeasurement {
        *ack_credit = None;
        return Err("bootstrap ACK credit requires a loaded capture".to_string());
    }
    let active_threshold = super::autotune_capture::bounded_load_trigger_kbps(
        request,
        policy.load_threshold_kbps() as f64,
    )?;
    let material_reverse = LoadedAckCredit::observe(
        ack_credit,
        request,
        counter_epoch,
        delta,
        active_threshold,
        policy.reverse_ack_ratio_ppm(),
    )?;
    let preserve_ack_credit = material_reverse.is_some();
    let elapsed = delta.observed_end.duration_since(delta.observed_start);
    let physical_phase = if elapsed > BootstrapTransportRuntime::dropout(policy) {
        None
    } else {
        let seconds = elapsed.as_secs_f64();
        let download_kbps = delta.download_bytes as f64 * 8.0 / seconds / 1_000.0;
        let upload_kbps = delta.upload_bytes as f64 * 8.0 / seconds / 1_000.0;
        Some(match request.direction {
            Some(SpeedtestDirection::Download) => {
                if download_kbps < active_threshold {
                    (false, false)
                } else {
                    (true, material_reverse.unwrap_or(true))
                }
            }
            Some(SpeedtestDirection::Upload) => {
                if upload_kbps < active_threshold {
                    (false, false)
                } else {
                    (material_reverse.unwrap_or(true), true)
                }
            }
            Some(SpeedtestDirection::Both) => (
                download_kbps >= active_threshold,
                upload_kbps >= active_threshold,
            ),
            None => (false, false),
        })
    };
    let physical_delta = Some(LoadedPhysicalDelta {
        delta,
        phase: physical_phase,
    });
    let Some(sample) = observation.rate_window else {
        return Ok((None, physical_delta, preserve_ack_credit));
    };
    if !sample.fresh
        || sample.observed_end <= sample.observed_start
        || sample.observed_end != delta.observed_end
        || sample.observed_start > delta.observed_start
        || !sample.download_kbps.is_finite()
        || sample.download_kbps < 0.0
        || !sample.upload_kbps.is_finite()
        || sample.upload_kbps < 0.0
    {
        *ack_credit = None;
        return Err("bootstrap controlled counter window is invalid".to_string());
    }
    let phase = match request.direction {
        Some(SpeedtestDirection::Download) => {
            if sample.download_kbps < active_threshold {
                (false, false)
            } else {
                (true, material_reverse.unwrap_or(true))
            }
        }
        Some(SpeedtestDirection::Upload) => {
            if sample.upload_kbps < active_threshold {
                (false, false)
            } else {
                (material_reverse.unwrap_or(true), true)
            }
        }
        Some(SpeedtestDirection::Both) => (
            sample.download_kbps >= active_threshold,
            sample.upload_kbps >= active_threshold,
        ),
        None => (false, false),
    };
    Ok((
        Some(LoadedPhaseWindow {
            request: request.clone(),
            // Byte accounting must use the non-overlapping physical delta,
            // but load authority belongs to the rolling rate window which
            // actually proved the threshold.  Shrinking this interval to the
            // latest delta leaves ICMP/transport samples temporally uncovered.
            observed_start: sample.observed_start,
            observed_end: sample.observed_end,
            phase,
        }),
        physical_delta,
        preserve_ack_credit,
    ))
}

fn loaded_phase_at(
    request: &AutotuneCaptureRequest,
    windows: &VecDeque<LoadedPhaseWindow>,
    observed_at: Instant,
) -> LoadedPhaseLookup {
    let matching = windows.iter().filter(|window| &window.request == request);
    let Some(newest_end) = matching.clone().map(|window| window.observed_end).max() else {
        return LoadedPhaseLookup::Pending;
    };
    if observed_at > newest_end {
        return LoadedPhaseLookup::Pending;
    }

    // A below-threshold rolling average is absence of positive load evidence;
    // it is not proof that every instant in that window was idle.  Conversely,
    // an active rolling window positively authorizes only its own bounded
    // interval.  Let agreeing active evidence dominate overlapping idle
    // windows, but fail closed when two active windows disagree about the
    // direction.  Picking the newest end in either case can erase a genuine
    // short control or attribute latency to the wrong direction.
    let mut covered = false;
    let mut active_phase = None;
    for window in matching
        .filter(|window| window.observed_start <= observed_at && observed_at <= window.observed_end)
    {
        covered = true;
        if window.phase == (false, false) {
            continue;
        }
        match active_phase {
            None => active_phase = Some(window.phase),
            Some(existing) if existing != window.phase => {
                return LoadedPhaseLookup::Uncovered;
            }
            Some(_) => {}
        }
    }
    match (covered, active_phase) {
        (false, _) => LoadedPhaseLookup::Uncovered,
        (true, Some(phase)) => LoadedPhaseLookup::Covered(phase),
        (true, None) => LoadedPhaseLookup::Covered((false, false)),
    }
}

fn loaded_phase_at_with_pending_bound(
    request: &AutotuneCaptureRequest,
    windows: &VecDeque<LoadedPhaseWindow>,
    observed_at: Instant,
    now: Instant,
) -> LoadedPhaseLookup {
    let lookup = loaded_phase_at(request, windows, observed_at);
    if lookup == LoadedPhaseLookup::Pending
        && now
            .checked_duration_since(observed_at)
            .is_some_and(|age| age > MAX_PENDING_ICMP_WAIT)
    {
        LoadedPhaseLookup::Uncovered
    } else {
        lookup
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LoadedPhaseTimelineObservation {
    at: Instant,
    phase: Option<(bool, bool)>,
}

/// Reconstruct the bounded phase timeline already used by the ICMP temporal
/// join.  Counter rate samples are intervals, not point observations: reducing
/// every 800 ms--1 s window to its completion instant can insert a synthetic
/// dropout between otherwise overlapping windows.  That lets ICMP accept the
/// interval evidence while transport can never satisfy its load hold.
///
/// Each segment is classified with the same positive-evidence consensus as
/// `loaded_phase_at`.  A real uncovered or conflicting-active segment is
/// retained at both ends, so it still breaks candidate continuity and flight
/// coverage.
fn loaded_phase_timeline(
    request: &AutotuneCaptureRequest,
    windows: &VecDeque<LoadedPhaseWindow>,
) -> Vec<LoadedPhaseTimelineObservation> {
    let mut boundaries = Vec::with_capacity(windows.len().saturating_mul(2));
    for window in windows.iter().filter(|window| &window.request == request) {
        if window.observed_start < window.observed_end {
            boundaries.push(window.observed_start);
            boundaries.push(window.observed_end);
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    if boundaries.len() < 2 {
        return Vec::new();
    }

    let phase_at = |at| match loaded_phase_at(request, windows, at) {
        LoadedPhaseLookup::Covered(phase) => Some(phase),
        LoadedPhaseLookup::Pending | LoadedPhaseLookup::Uncovered => None,
    };
    let mut timeline: Vec<LoadedPhaseTimelineObservation> =
        Vec::with_capacity(boundaries.len().saturating_mul(2));
    let mut push = |at, phase| {
        if let Some(last) = timeline.last_mut() {
            if last.at == at {
                last.phase = phase;
                return;
            }
        }
        timeline.push(LoadedPhaseTimelineObservation { at, phase });
    };

    for pair in boundaries.windows(2) {
        let start = pair[0];
        let end = pair[1];
        let elapsed = end.saturating_duration_since(start);
        if elapsed.is_zero() {
            continue;
        }
        let midpoint = start.checked_add(elapsed / 2).unwrap_or(start);
        let phase = phase_at(midpoint);
        push(start, phase);
        if let Some(tail) = end.checked_sub(Duration::from_nanos(1)) {
            if tail > start {
                push(tail, phase);
            }
        }
    }
    if let Some(end) = boundaries.last().copied() {
        push(end, phase_at(end));
    }
    timeline
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CounterCompletionStatus {
    Current,
    Stale,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CounterCompletionError {
    Identity,
    Expired,
}

fn counter_completion_is_current(
    request: &AutotuneCaptureRequest,
    expected_epoch: u64,
    fence: &CounterReadFence,
    completion: &super::autotune_counter::AutotuneCounterCompletion,
    maximum_age: Duration,
) -> Result<CounterCompletionStatus, CounterCompletionError> {
    if fence.epoch != expected_epoch
        || &fence.request != request
        || completion.epoch != expected_epoch
        || &completion.request != request
    {
        return if completion.epoch < expected_epoch {
            Ok(CounterCompletionStatus::Stale)
        } else {
            Err(CounterCompletionError::Identity)
        };
    }
    let Some(read_age) = completion
        .completed_at
        .checked_duration_since(fence.scheduled_at)
    else {
        return Err(CounterCompletionError::Identity);
    };
    if read_age > maximum_age {
        return Ok(CounterCompletionStatus::Stale);
    }
    if completion.completed_boot_ms == 0 || completion.completed_boot_ms > request.deadline_boot_ms
    {
        return Err(CounterCompletionError::Expired);
    }
    Ok(CounterCompletionStatus::Current)
}

#[derive(Clone, Debug)]
struct BootstrapTransportWork {
    probe_id: u64,
    capture: AutotuneCaptureRequest,
}

#[derive(Debug)]
struct BootstrapTransportCompletion {
    probe_id: u64,
    capture: AutotuneCaptureRequest,
    started_at: Instant,
    completed_at: Instant,
    outcome: Result<
        crate::transport_probe::TransportProbeSample,
        crate::transport_probe::TransportProbeFailure,
    >,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BootstrapTransportScheduleDecision {
    InFlight,
    ReadinessBlocked(&'static str),
    CadencePending,
    Scheduled,
}

struct BootstrapTransportWakeGuard(Arc<OwnedFd>);

impl Drop for BootstrapTransportWakeGuard {
    fn drop(&mut self) {
        crate::notify_pinger_wake(Some(self.0.as_ref()));
    }
}

struct BootstrapTransportRuntime {
    request_tx: Option<SyncSender<BootstrapTransportWork>>,
    result_rx: Receiver<BootstrapTransportCompletion>,
    worker: Option<JoinHandle<()>>,
    _wake: Arc<OwnedFd>,
    control: crate::AutotuneTransportControl,
    in_flight: Option<crate::AutotuneTransportFlight>,
    next_probe_id: u64,
    last_started: Option<Instant>,
    last_readiness_block: Option<&'static str>,
    idle_samples_ms: VecDeque<f64>,
    idle_baseline_ms: Option<f64>,
}

impl BootstrapTransportRuntime {
    fn spawn(
        operation: &OperationRequest,
        policy: &super::autotune_capture_policy::AutotuneCapturePolicy,
        wake: Arc<OwnedFd>,
    ) -> Result<Self, String> {
        operation.validate()?;
        if operation.target_state != OperationTargetState::AbsentBootstrap {
            return Err("bootstrap transport requires an absent target request".to_string());
        }
        if operation
            .route
            .source_ip
            .is_some_and(|value| !value.is_ipv4())
        {
            return Err("bootstrap transport currently requires an IPv4 route source".to_string());
        }
        let backend =
            crate::transport_probe::TransportProbeBackend::parse(policy.transport_backend())
                .ok_or_else(|| "bootstrap transport backend is unsupported".to_string())?;
        if backend != crate::transport_probe::TransportProbeBackend::WebSocket || !backend.trusted()
        {
            return Err("bootstrap transport policy is not trusted".to_string());
        }
        let endpoint = policy.transport_endpoint().to_string();
        let binding = crate::transport_probe::RouteBinding {
            device: operation.route.l3_device.clone(),
            source_ip: operation
                .route
                .source_ip
                .map(|value| value.to_string())
                .unwrap_or_default(),
            fwmark: operation
                .route
                .fwmark
                .map(|value| format!("{value:x}"))
                .unwrap_or_default(),
        };
        let timeout = Duration::from_millis(u64::from(policy.transport_timeout_ms()));
        let (request_tx, request_rx) = mpsc::sync_channel::<BootstrapTransportWork>(1);
        let (result_tx, result_rx) = mpsc::channel::<BootstrapTransportCompletion>();
        let worker_wake = Arc::clone(&wake);
        let worker = thread::Builder::new()
            .name("cake-bootstrap-transport".to_string())
            .spawn(move || {
                let _wake_on_exit = BootstrapTransportWakeGuard(Arc::clone(&worker_wake));
                let mut engine = crate::transport_probe::TransportProbeEngine::new(
                    backend, endpoint, binding, timeout,
                );
                while let Ok(work) = request_rx.recv() {
                    let started_at = Instant::now();
                    let outcome = match engine.as_mut() {
                        Ok(engine) => match work.capture.phase {
                            AutotuneCapturePhase::IdleBaseline => engine.probe_classified(),
                            AutotuneCapturePhase::LoadedMeasurement => {
                                engine.probe_classified_loaded_autotune()
                            }
                        },
                        Err(error) => Err(crate::transport_probe::TransportProbeFailure::other(
                            error.clone(),
                        )),
                    };
                    let completed_at = Instant::now();
                    if result_tx
                        .send(BootstrapTransportCompletion {
                            probe_id: work.probe_id,
                            capture: work.capture,
                            started_at,
                            completed_at,
                            outcome,
                        })
                        .is_err()
                    {
                        break;
                    }
                    crate::notify_pinger_wake(Some(&worker_wake));
                }
            })
            .map_err(|error| format!("unable to start bootstrap transport worker: {error}"))?;
        Ok(Self {
            request_tx: Some(request_tx),
            result_rx,
            worker: Some(worker),
            _wake: wake,
            control: crate::AutotuneTransportControl::default(),
            in_flight: None,
            next_probe_id: 1,
            last_started: None,
            last_readiness_block: None,
            idle_samples_ms: VecDeque::with_capacity(128),
            idle_baseline_ms: None,
        })
    }

    fn stop(&mut self) {
        self.request_stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }

    fn request_stop(&mut self) {
        self.request_tx.take();
        self.in_flight = None;
        self.control.reset();
        self.last_readiness_block = None;
    }

    fn begin_request(&mut self, request: &AutotuneCaptureRequest) {
        self.control.reset();
        self.last_started = None;
        self.last_readiness_block = None;
        if request.phase == AutotuneCapturePhase::IdleBaseline {
            self.idle_samples_ms.clear();
            self.idle_baseline_ms = None;
        }
    }

    fn finish_stop(&mut self) -> bool {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            return false;
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        true
    }

    fn wake_fd(&self) -> i32 {
        self._wake.as_raw_fd()
    }

    fn drain_wake(&self) -> Result<(), String> {
        loop {
            let mut value = 0_u64;
            let read = unsafe {
                libc::read(
                    self._wake.as_raw_fd(),
                    (&mut value as *mut u64).cast::<libc::c_void>(),
                    std::mem::size_of::<u64>(),
                )
            };
            if read == std::mem::size_of::<u64>() as isize {
                continue;
            }
            if read < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    return Ok(());
                }
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(format!("failed to drain bootstrap transport wake: {error}"));
            }
            return Err("bootstrap transport wake descriptor returned a short read".to_string());
        }
    }

    fn dropout(policy: &super::autotune_capture_policy::AutotuneCapturePolicy) -> Duration {
        Duration::from_millis(
            u64::from(policy.rate_sample_interval_ms())
                .saturating_mul(3)
                .min(u64::from(policy.transport_load_hold_ms()) / 2),
        )
    }

    fn observe_phase(
        &mut self,
        request: &AutotuneCaptureRequest,
        phase: Option<(bool, bool)>,
        policy: &super::autotune_capture_policy::AutotuneCapturePolicy,
        now: Instant,
    ) -> Result<(), String> {
        let key = crate::AutotuneTransportCaptureKey::new(request, &request.route_fingerprint);
        let expected = crate::AutotuneTransportControl::expected_phase(request)?;
        let dropout = Self::dropout(policy);
        let history_window = Duration::from_millis(u64::from(policy.transport_timeout_ms()))
            .saturating_add(Duration::from_millis(u64::from(
                policy.transport_load_hold_ms(),
            )))
            .saturating_add(dropout)
            .saturating_add(Duration::from_millis(
                u64::from(policy.rate_sample_interval_ms()).saturating_mul(3),
            ));
        self.control
            .observe(key, phase, expected, now, dropout, history_window);
        Ok(())
    }

    fn replace_loaded_phase_history(
        &mut self,
        request: &AutotuneCaptureRequest,
        timeline: &[LoadedPhaseTimelineObservation],
        physical_deltas: &VecDeque<LoadedPhysicalDelta>,
        policy: &super::autotune_capture_policy::AutotuneCapturePolicy,
    ) -> Result<(), String> {
        if request.phase != AutotuneCapturePhase::LoadedMeasurement || timeline.is_empty() {
            return Err("bootstrap loaded transport phase timeline is invalid".to_string());
        }
        let key = crate::AutotuneTransportCaptureKey::new(request, &request.route_fingerprint);
        let expected = crate::AutotuneTransportControl::expected_phase(request)?;
        let dropout = Self::dropout(policy);
        let history_window = Duration::from_millis(u64::from(policy.transport_timeout_ms()))
            .saturating_add(Duration::from_millis(u64::from(
                policy.transport_load_hold_ms(),
            )))
            .saturating_add(dropout)
            .saturating_add(Duration::from_millis(
                u64::from(policy.rate_sample_interval_ms()).saturating_mul(3),
            ));
        self.control.reset();
        for observation in timeline {
            self.control.observe(
                key.clone(),
                observation.phase,
                expected,
                observation.at,
                dropout,
                history_window,
            );
        }
        for observation in physical_deltas {
            self.control.observe_physical_delta(
                &key,
                observation.delta,
                observation.phase,
                history_window,
            )?;
        }
        Ok(())
    }

    fn append_loaded_physical_delta(
        &mut self,
        request: &AutotuneCaptureRequest,
        observation: LoadedPhysicalDelta,
        policy: &super::autotune_capture_policy::AutotuneCapturePolicy,
    ) -> Result<(), String> {
        let key = crate::AutotuneTransportCaptureKey::new(request, &request.route_fingerprint);
        let dropout = Self::dropout(policy);
        let history_window = Duration::from_millis(u64::from(policy.transport_timeout_ms()))
            .saturating_add(Duration::from_millis(u64::from(
                policy.transport_load_hold_ms(),
            )))
            .saturating_add(dropout)
            .saturating_add(Duration::from_millis(
                u64::from(policy.rate_sample_interval_ms()).saturating_mul(3),
            ));
        self.control.observe_physical_delta(
            &key,
            observation.delta,
            observation.phase,
            history_window,
        )
    }

    fn readiness(
        &self,
        request: &AutotuneCaptureRequest,
        policy: &super::autotune_capture_policy::AutotuneCapturePolicy,
        now: Instant,
    ) -> Result<crate::AutotuneTransportReadiness, String> {
        let key = crate::AutotuneTransportCaptureKey::new(request, &request.route_fingerprint);
        self.control.ready_phase_diagnostic(
            request,
            &key,
            now,
            Duration::from_millis(u64::from(policy.transport_load_hold_ms())),
            Self::dropout(policy),
        )
    }

    fn record_readiness(&mut self, readiness: crate::AutotuneTransportReadiness) {
        let blocked = (!readiness.ready).then_some(readiness.reason);
        if self.last_readiness_block == blocked {
            return;
        }
        match (self.last_readiness_block, blocked) {
            (_, Some(reason)) => {
                eprintln!("bootstrap transport scheduling blocked: {reason}");
            }
            (Some(reason), None) => {
                eprintln!("bootstrap transport scheduling recovered from: {reason}");
            }
            (None, None) => {}
        }
        self.last_readiness_block = blocked;
    }

    fn probe_interval(
        request: &AutotuneCaptureRequest,
        policy: &super::autotune_capture_policy::AutotuneCapturePolicy,
    ) -> Duration {
        Duration::from_millis(u64::from(match request.phase {
            AutotuneCapturePhase::IdleBaseline => policy.transport_baseline_learning_interval_ms(),
            AutotuneCapturePhase::LoadedMeasurement => policy.transport_loaded_interval_ms(),
        }))
    }

    fn try_schedule(
        &mut self,
        request: &AutotuneCaptureRequest,
        policy: &super::autotune_capture_policy::AutotuneCapturePolicy,
        now: Instant,
    ) -> Result<BootstrapTransportScheduleDecision, String> {
        if self.in_flight.is_some() {
            return Ok(BootstrapTransportScheduleDecision::InFlight);
        }
        let readiness = self.readiness(request, policy, now)?;
        self.record_readiness(readiness);
        if !readiness.ready {
            return Ok(BootstrapTransportScheduleDecision::ReadinessBlocked(
                readiness.reason,
            ));
        }
        // Idle learning is intentionally cadence-limited.  A loaded flight,
        // however, has a stronger and already bounded authority: the caller
        // must present one fresh, exact Rebase-to-FlightEvidence delta whose
        // start follows a full topology attestation, and consumes that authority
        // after a successful dispatch. Applying the
        // wall-clock cadence as a second gate can deterministically starve a
        // short high-rate transfer (three four-sample flights yield only
        // 12/15 required transport samples).  Let physical counter-window
        // completions, plus the single in-flight fence above, pace loaded
        // probes instead of manufacturing a timer dependency.
        if request.phase == AutotuneCapturePhase::IdleBaseline
            && self.last_started.is_some_and(|last| {
                now.saturating_duration_since(last) < Self::probe_interval(request, policy)
            })
        {
            return Ok(BootstrapTransportScheduleDecision::CadencePending);
        }
        let probe_id = self.next_probe_id;
        let next_probe_id = probe_id
            .checked_add(1)
            .ok_or_else(|| "bootstrap transport probe sequence exhausted".to_string())?;
        let key = crate::AutotuneTransportCaptureKey::new(request, &request.route_fingerprint);
        let work = BootstrapTransportWork {
            probe_id,
            capture: request.clone(),
        };
        let sender = self
            .request_tx
            .as_ref()
            .ok_or_else(|| "bootstrap transport worker is stopped".to_string())?;
        match sender.try_send(work) {
            Ok(()) => {
                self.next_probe_id = next_probe_id;
                self.last_started = Some(now);
                self.in_flight = Some(crate::AutotuneTransportFlight {
                    probe_id,
                    key,
                    expected_phase: readiness.phase,
                    control_valid: true,
                    submitted_at: now,
                    physical_delta_required: request.phase
                        == AutotuneCapturePhase::LoadedMeasurement,
                });
                eprintln!(
                    "bootstrap transport scheduled probe {probe_id} for phase {}:{}",
                    u8::from(readiness.phase.0),
                    u8::from(readiness.phase.1)
                );
                Ok(BootstrapTransportScheduleDecision::Scheduled)
            }
            Err(TrySendError::Full(_)) => {
                Err("bootstrap transport queue is unexpectedly full".to_string())
            }
            Err(TrySendError::Disconnected(_)) => {
                Err("bootstrap transport worker is unavailable".to_string())
            }
        }
    }

    fn next_schedule_wake(
        &self,
        request: &AutotuneCaptureRequest,
        policy: &super::autotune_capture_policy::AutotuneCapturePolicy,
        now: Instant,
    ) -> Result<Option<Duration>, String> {
        if self.in_flight.is_some() {
            // Completion writes the shared eventfd.
            return Ok(None);
        }
        if !self.readiness(request, policy, now)?.ready {
            // A new physical-rate observation is the only positive authority
            // that can make the phase ready; its own deadline wakes the owner.
            return Ok(None);
        }
        if request.phase == AutotuneCapturePhase::LoadedMeasurement {
            // A fresh counter completion or transport completion writes the
            // shared eventfd.  Loaded scheduling has no wall-clock cadence;
            // only the exact single-use counter authority may wake and pace
            // another flight.
            return Ok(None);
        }
        let interval = Self::probe_interval(request, policy);
        Ok(Some(self.last_started.map_or(Duration::ZERO, |last| {
            interval.saturating_sub(now.saturating_duration_since(last))
        })))
    }

    fn try_take(
        &mut self,
    ) -> Result<Option<(crate::AutotuneTransportFlight, BootstrapTransportCompletion)>, String>
    {
        match self.result_rx.try_recv() {
            Ok(completion) => {
                let flight = self.in_flight.take().ok_or_else(|| {
                    "bootstrap transport result has no in-flight authority".to_string()
                })?;
                if flight.probe_id != completion.probe_id
                    || completion.capture.capture_id != flight.key.capture_id
                    || completion.capture.sequence != flight.key.request_sequence
                    || completion.started_at < flight.submitted_at
                    || completion.completed_at <= completion.started_at
                {
                    return Err("bootstrap transport result identity is invalid".to_string());
                }
                eprintln!(
                    "bootstrap transport completed probe {} in {} ms",
                    completion.probe_id,
                    completion
                        .completed_at
                        .duration_since(completion.started_at)
                        .as_millis()
                );
                Ok(Some((flight, completion)))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                Err("bootstrap transport worker terminated unexpectedly".to_string())
            }
        }
    }

    fn observe_idle_samples(&mut self, samples: &[f64]) -> Result<(), String> {
        for sample in samples {
            if !sample.is_finite() || *sample <= 0.0 {
                return Err("bootstrap transport returned an invalid idle sample".to_string());
            }
            self.idle_samples_ms.push_back(*sample);
            while self.idle_samples_ms.len() > 128 {
                self.idle_samples_ms.pop_front();
            }
        }
        if self.idle_samples_ms.len()
            >= super::full_autotune::MIN_AUTOTUNE_TRANSPORT_SAMPLES as usize
        {
            let mut sorted = self.idle_samples_ms.iter().copied().collect::<Vec<_>>();
            sorted.sort_by(f64::total_cmp);
            let index = ((sorted.len() - 1) as f64 * 0.05).floor() as usize;
            self.idle_baseline_ms = sorted.get(index).copied();
        }
        Ok(())
    }
}

impl Drop for BootstrapTransportRuntime {
    fn drop(&mut self) {
        self.stop();
    }
}

trait BootstrapCaptureAuthority {
    fn attest_capture(
        &mut self,
        store: &RuntimeOverrideStore,
        request: &AutotuneCaptureRequest,
        current_boot_ms: u64,
    ) -> Result<(u64, u64), (&'static str, String)>;

    fn attest_loaded_transport_durable_authority(
        &mut self,
        runtime_dir: &Path,
        store: &RuntimeOverrideStore,
        request: &AutotuneCaptureRequest,
        current_boot_ms: u64,
    ) -> CaptureRuntimeResult;
}

struct BootstrapCaptureRuntime {
    session: AutotuneCaptureSession,
    idle_rate_reference: Option<(u64, u64)>,
    idle_icmp_baseline_ms: Option<f64>,
    idle_transport_baseline_ms: Option<f64>,
    pinger: Option<crate::PingerRuntime>,
    transport: Option<BootstrapTransportRuntime>,
    stopping_transport: Option<BootstrapTransportRuntime>,
    probes_stopped: Arc<AtomicBool>,
    policy: Option<super::autotune_capture_policy::AutotuneCapturePolicy>,
    rate_monitor: Option<crate::RateMonitor>,
    counter_sampler: Option<super::autotune_counter::AutotuneCounterSampler>,
    counter_rates: Option<super::autotune_counter::AutotuneCounterRateTracker>,
    counter_physical: Option<super::autotune_counter::AutotuneCounterRateTracker>,
    counter_request: Option<AutotuneCaptureRequest>,
    counter_epoch: u64,
    pending_counter_fence: Option<CounterReadFence>,
    loaded_counter_cycle: LoadedCounterCycleState,
    loaded_counter_burst: LoadedCounterBurstState,
    loaded_topology_attestation: Option<LoadedTopologyAttestation>,
    loaded_transport_authority: Option<LoadedTransportAuthority>,
    loaded_ack_credit: Option<LoadedAckCredit>,
    loaded_phase_windows: VecDeque<LoadedPhaseWindow>,
    loaded_physical_deltas: VecDeque<LoadedPhysicalDelta>,
    pending_transport_completion:
        Option<(crate::AutotuneTransportFlight, BootstrapTransportCompletion)>,
    loaded_phase_expired: bool,
    loaded_counter_diagnostic_budget: usize,
    loaded_transport_diagnostic_budget: u8,
    pending_icmp_samples: VecDeque<PendingIcmpSample>,
    cpu_monitor: Option<crate::CpuMonitor>,
    next_rate_sample: Instant,
    next_cpu_sample: Instant,
}

impl BootstrapCaptureRuntime {
    fn new(probes_stopped: Arc<AtomicBool>) -> Self {
        probes_stopped.store(true, Ordering::Release);
        let now = Instant::now();
        Self {
            session: AutotuneCaptureSession::new(),
            idle_rate_reference: None,
            idle_icmp_baseline_ms: None,
            idle_transport_baseline_ms: None,
            pinger: None,
            transport: None,
            stopping_transport: None,
            probes_stopped,
            policy: None,
            rate_monitor: None,
            counter_sampler: None,
            counter_rates: None,
            counter_physical: None,
            counter_request: None,
            counter_epoch: 0,
            pending_counter_fence: None,
            loaded_counter_cycle: LoadedCounterCycleState::NeedAttestedBaseline,
            loaded_counter_burst: LoadedCounterBurstState::Idle,
            loaded_topology_attestation: None,
            loaded_transport_authority: None,
            loaded_ack_credit: None,
            loaded_phase_windows: VecDeque::with_capacity(MAX_LOADED_PHASE_WINDOWS),
            loaded_physical_deltas: VecDeque::with_capacity(MAX_LOADED_PHASE_WINDOWS),
            pending_transport_completion: None,
            loaded_phase_expired: true,
            loaded_counter_diagnostic_budget: 0,
            loaded_transport_diagnostic_budget: 0,
            pending_icmp_samples: VecDeque::with_capacity(MAX_PENDING_ICMP_SAMPLES),
            cpu_monitor: None,
            next_rate_sample: now,
            next_cpu_sample: now,
        }
    }

    fn ensure_pinger(&mut self, operation: &OperationRequest) -> Result<(), String> {
        self.poll_probe_shutdown()?;
        if self.stopping_transport.is_some() {
            return Err("bootstrap transport shutdown is still in progress".to_string());
        }
        let policy = operation
            .capture_policy
            .ok_or_else(|| "bootstrap capture has no immutable policy".to_string())?
            .expand()?;
        if self
            .policy
            .as_ref()
            .is_some_and(|current| current != &policy)
        {
            return Err("bootstrap capture policy changed after probe startup".to_string());
        }
        if self.policy.is_none() {
            let statistics = crate::sqm_sys_class_net()
                .join(&operation.route.l3_device)
                .join("statistics");
            let rx_path = statistics.join("rx_bytes");
            let tx_path = statistics.join("tx_bytes");
            let rx = rx_path
                .to_str()
                .ok_or_else(|| "bootstrap receive counter path is not UTF-8".to_string())?;
            let tx = tx_path
                .to_str()
                .ok_or_else(|| "bootstrap transmit counter path is not UTF-8".to_string())?;
            self.rate_monitor = Some(
                crate::RateMonitor::new(rx, tx, u64::from(policy.rate_sample_interval_ms()))
                    .map_err(|error| {
                        format!("unable to initialize bootstrap route rate monitor: {error}")
                    })?,
            );
            self.cpu_monitor =
                Some(crate::CpuMonitor::new().map_err(|error| {
                    format!("unable to initialize bootstrap CPU monitor: {error}")
                })?);
            self.counter_rates = Some(
                super::autotune_counter::AutotuneCounterRateTracker::new_with_maximum_delta_span(
                    u64::from(policy.rate_sample_interval_ms()),
                    u64::from(policy.maximum_counter_delta_span_ms()),
                )?,
            );
            self.counter_physical = Some(
                super::autotune_counter::AutotuneCounterRateTracker::new_with_maximum_delta_span(
                    u64::from(policy.rate_sample_interval_ms()),
                    u64::from(policy.maximum_counter_delta_span_ms()),
                )?,
            );
            self.policy = Some(policy);
            let now = Instant::now();
            self.next_rate_sample = now;
            self.next_cpu_sample = now;
        }
        if self.pinger.is_none() {
            let pinger = crate::PingerRuntime::spawn_bootstrap(operation)?;
            self.pinger = Some(pinger);
        }
        if self.counter_sampler.is_none() {
            let wake = self
                .pinger
                .as_ref()
                .and_then(|pinger| pinger.wake.clone())
                .ok_or_else(|| "bootstrap pinger has no shared wake descriptor".to_string())?;
            match super::autotune_counter::AutotuneCounterSampler::new_with_wake(wake) {
                Ok(sampler) => self.counter_sampler = Some(sampler),
                Err(error) => {
                    self.stop_probes();
                    return Err(error);
                }
            }
        }
        if self.transport.is_none() {
            let wake = self
                .pinger
                .as_ref()
                .and_then(|pinger| pinger.wake.clone())
                .ok_or_else(|| "bootstrap pinger has no shared wake descriptor".to_string())?;
            match BootstrapTransportRuntime::spawn(
                operation,
                self.policy
                    .as_ref()
                    .expect("bootstrap capture policy was initialized"),
                wake,
            ) {
                Ok(transport) => self.transport = Some(transport),
                Err(error) => {
                    self.stop_probes();
                    return Err(error);
                }
            }
        }
        self.probes_stopped.store(false, Ordering::Release);
        Ok(())
    }

    fn stop_probes(&mut self) {
        if let Some(mut transport) = self.transport.take() {
            transport.request_stop();
            if transport.finish_stop() {
                drop(transport);
            } else if self.stopping_transport.is_none() {
                self.stopping_transport = Some(transport);
            } else {
                // This branch is unreachable under the single-probe owner
                // invariant. Preserve the fence even if that invariant is
                // violated rather than losing a live worker handle.
                transport.stop();
            }
        }
        if let Some(mut pinger) = self.pinger.take() {
            pinger.stop();
        }
        if let Some(sampler) = self.counter_sampler.as_mut() {
            sampler.invalidate_current();
        }
        self.counter_sampler = None;
        self.counter_rates = None;
        self.counter_physical = None;
        self.counter_request = None;
        self.pending_counter_fence = None;
        self.loaded_counter_cycle = LoadedCounterCycleState::NeedAttestedBaseline;
        self.loaded_counter_burst = LoadedCounterBurstState::Idle;
        self.loaded_topology_attestation = None;
        self.loaded_transport_authority = None;
        self.loaded_ack_credit = None;
        self.loaded_phase_windows.clear();
        self.loaded_physical_deltas.clear();
        self.pending_transport_completion = None;
        self.loaded_phase_expired = true;
        self.loaded_counter_diagnostic_budget = 0;
        self.loaded_transport_diagnostic_budget = 0;
        self.pending_icmp_samples.clear();
        self.rate_monitor = None;
        self.cpu_monitor = None;
        self.policy = None;
        let _ = self.poll_probe_shutdown();
    }

    fn poll_probe_shutdown(&mut self) -> Result<(), String> {
        let finished = if let Some(transport) = self.stopping_transport.as_mut() {
            transport.drain_wake()?;
            transport.finish_stop()
        } else {
            true
        };
        if finished {
            self.stopping_transport.take();
        }
        self.probes_stopped.store(
            self.pinger.is_none() && self.transport.is_none() && self.stopping_transport.is_none(),
            Ordering::Release,
        );
        Ok(())
    }

    fn probe_shutdown_pending(&self) -> bool {
        self.stopping_transport.is_some()
    }

    fn poll_fd(&self) -> i32 {
        if let Some(pinger) = self.pinger.as_ref() {
            pinger.wake_fd()
        } else {
            self.stopping_transport
                .as_ref()
                .map_or(-1, BootstrapTransportRuntime::wake_fd)
        }
    }

    fn drain_probe_wake(&self) -> Result<(), String> {
        if let Some(pinger) = self.pinger.as_ref() {
            pinger.drain_wake()
        } else if let Some(transport) = self.stopping_transport.as_ref() {
            transport.drain_wake()
        } else {
            Ok(())
        }
    }

    /// Consume a loaded counter/transport completion before the general
    /// runtime-health pass performs synchronous netlink and qdisc reads.
    ///
    /// This is intentionally not used for idle sampling: only a loaded
    /// controlled-counter read carries a single-use exact read fence.
    fn poll_loaded_completion_before_owner_poll(
        &mut self,
        runtime_dir: &Path,
        store: &RuntimeOverrideStore,
        actuator: &mut OpenWrtBootstrapRuntimeActuator,
    ) -> CaptureRuntimeResult {
        if self
            .clear_if_private_request_removed(runtime_dir)
            .map_err(|error| ("capture-runtime-unavailable", error))?
        {
            return Ok(());
        }
        if !self.session.accepting_observations()
            || !self
                .session
                .active_request()
                .is_some_and(|request| request.phase == AutotuneCapturePhase::LoadedMeasurement)
        {
            return Ok(());
        }
        self.drain_probe_wake()
            .map_err(|error| ("capture-probe-wake-unavailable", error))?;
        let burst_before = self.loaded_counter_burst;
        let had_counter_fence = self.pending_counter_fence.is_some();
        self.poll_rate_measurement(runtime_dir, store, actuator)?;
        let counter_completed = had_counter_fence && self.pending_counter_fence.is_none();
        self.poll_transport(
            runtime_dir,
            store,
            actuator,
            !burst_before.bracketing_flight(),
        )?;
        self.advance_loaded_counter_burst_after_poll(
            runtime_dir,
            store,
            actuator,
            burst_before,
            counter_completed,
        )
    }

    fn advance_loaded_counter_burst_after_poll<A: BootstrapCaptureAuthority>(
        &mut self,
        runtime_dir: &Path,
        store: &RuntimeOverrideStore,
        actuator: &mut A,
        burst_before: LoadedCounterBurstState,
        counter_completed: bool,
    ) -> CaptureRuntimeResult {
        let mut transport_active = self.loaded_transport_work_active();
        let counter_active = self.pending_counter_fence.is_some();

        // Consume the just-completed single-use FlightEvidence authority
        // before applying the event budgets.  The positive event is the exact
        // counter completion; scheduling still repeats durable authority and
        // readiness checks, and a successful dispatch enters
        // BracketingFlight so its mandatory FlightPost fence must settle
        // before fairness can yield.
        if burst_before.active()
            && counter_completed
            && self.loaded_counter_cycle == LoadedCounterCycleState::FlightReady
            && !transport_active
        {
            self.try_schedule_transport(runtime_dir, store, actuator)?;
            transport_active = self.loaded_transport_work_active();
            if transport_active {
                self.loaded_counter_cycle = LoadedCounterCycleState::FlightActive;
            } else {
                self.loaded_topology_attestation = None;
                self.loaded_transport_authority = None;
                self.loaded_counter_cycle = LoadedCounterCycleState::BaselineReady;
            }
        }
        self.loaded_counter_burst = burst_before.after_reactor(
            self.session.accepting_observations(),
            counter_completed,
            counter_active,
            transport_active,
        );
        if self.loaded_counter_burst.active()
            && self.loaded_counter_cycle == LoadedCounterCycleState::FlightReady
            && !transport_active
        {
            // A post-attested evidence endpoint is never reused directly.
            // Only the cadence-spaced Rebase-to-FlightEvidence delta may open
            // the single-use dispatch boundary. Readiness and durable
            // authority are checked synchronously here; if no flight is
            // admitted, that endpoint becomes the ordinary rolling baseline.
            self.try_schedule_transport(runtime_dir, store, actuator)?;
            if self.loaded_transport_work_active() {
                self.loaded_counter_cycle = LoadedCounterCycleState::FlightActive;
                self.loaded_counter_burst = self.loaded_counter_burst.after_reactor(
                    self.session.accepting_observations(),
                    false,
                    false,
                    true,
                );
            } else {
                self.loaded_topology_attestation = None;
                self.loaded_transport_authority = None;
                self.loaded_counter_cycle = LoadedCounterCycleState::BaselineReady;
            }
        }
        if !self.loaded_counter_burst.active() {
            // A burst brackets only its explicit bounded flight budget. Never
            // carry leftover authority into the full CPU/ICMP/runtime turn.
            self.loaded_topology_attestation = None;
            self.loaded_transport_authority = None;
            self.loaded_counter_cycle = match self.loaded_counter_cycle {
                LoadedCounterCycleState::FlightReady => LoadedCounterCycleState::BaselineReady,
                LoadedCounterCycleState::NeedRebase
                | LoadedCounterCycleState::NeedFlightEvidence
                | LoadedCounterCycleState::FlightActive => {
                    LoadedCounterCycleState::NeedAttestedBaseline
                }
                state => state,
            };
        }
        Ok(())
    }

    /// A pinger completion may wake the shared eventfd while the exact loaded
    /// counter read is still in flight.  Re-entering netlink/qdisc health at
    /// that point would let the counter complete behind unrelated synchronous
    /// work and consume its immutable freshness budget.  Keep the owner on the
    /// event reactor until that exact counter completion or a durable
    /// revocation/process event arrives.
    fn loaded_counter_barrier_active<A: BootstrapCaptureAuthority>(
        &self,
        runtime_dir: &Path,
        store: &RuntimeOverrideStore,
        actuator: &mut A,
    ) -> CaptureRuntimeResult<bool> {
        let active_request = self.session.active_request();
        let loaded_capture = active_request
            .is_some_and(|request| request.phase == AutotuneCapturePhase::LoadedMeasurement);
        let counter_in_flight = self
            .counter_sampler
            .as_ref()
            .is_some_and(super::autotune_counter::AutotuneCounterSampler::has_in_flight);
        let barrier = loaded_counter_barrier_required(
            self.session.accepting_observations(),
            loaded_capture,
            counter_in_flight,
            self.pending_counter_fence.is_some(),
        )
        .map_err(|error| ("capture-speedtest-counters-invalid", error))?;
        if !barrier {
            return Ok(false);
        }
        let request = active_request.ok_or_else(|| {
            (
                "capture-observation-invalid",
                "bootstrap loaded counter barrier lost its active request".to_string(),
            )
        })?;
        let boot_ms = monotonic_boot_ms().map_err(|error| ("capture-clock-unavailable", error))?;
        actuator.attest_loaded_transport_durable_authority(runtime_dir, store, request, boot_ms)?;
        Ok(true)
    }

    fn loaded_transport_work_active(&self) -> bool {
        self.pending_transport_completion.is_some()
            || self
                .transport
                .as_ref()
                .is_some_and(|transport| transport.in_flight.is_some())
    }

    /// Drive only the bounded loaded counter burst.  A normal owner turn
    /// starts it, physical endpoints prime fresh single-use authority, and up
    /// to the explicit transport-flight budget may be bracketed before the
    /// state yields. The 200 ms policy cadence is measurement spacing;
    /// completions and durable revocations are the state transitions.
    fn drive_loaded_counter_burst<A: BootstrapCaptureAuthority>(
        &mut self,
        runtime_dir: &Path,
        store: &RuntimeOverrideStore,
        actuator: &mut A,
    ) -> CaptureRuntimeResult<LoadedCounterReactorWait> {
        if !self.loaded_counter_burst.active() {
            return Ok(LoadedCounterReactorWait::Inactive);
        }
        if !self.session.accepting_observations()
            || !self
                .session
                .active_request()
                .is_some_and(|request| request.phase == AutotuneCapturePhase::LoadedMeasurement)
        {
            self.loaded_counter_burst = LoadedCounterBurstState::Idle;
            self.loaded_topology_attestation = None;
            self.loaded_transport_authority = None;
            return Ok(LoadedCounterReactorWait::Inactive);
        }
        if self.loaded_counter_barrier_active(runtime_dir, store, actuator)? {
            return Ok(LoadedCounterReactorWait::CounterCompletion);
        }
        if self.loaded_counter_cycle == LoadedCounterCycleState::FlightActive
            && self.pending_transport_completion.is_none()
        {
            if self.loaded_transport_work_active() {
                return Ok(LoadedCounterReactorWait::TransportCompletion);
            }
            return Err((
                "capture-transport-invalid",
                "bootstrap loaded counter cycle lost its active transport flight".to_string(),
            ));
        }
        let now = Instant::now();
        if self.loaded_counter_cycle == LoadedCounterCycleState::FlightReady {
            self.try_schedule_transport(runtime_dir, store, actuator)?;
            if self.loaded_transport_work_active() {
                self.loaded_counter_cycle = LoadedCounterCycleState::FlightActive;
                self.loaded_counter_burst = self.loaded_counter_burst.after_reactor(
                    self.session.accepting_observations(),
                    false,
                    false,
                    true,
                );
                return Ok(LoadedCounterReactorWait::TransportCompletion);
            }
            self.loaded_topology_attestation = None;
            self.loaded_transport_authority = None;
            self.loaded_counter_cycle = LoadedCounterCycleState::BaselineReady;
        }
        if matches!(
            self.loaded_counter_cycle,
            LoadedCounterCycleState::BaselineReady | LoadedCounterCycleState::NeedFlightEvidence
        ) && now < self.next_rate_sample
        {
            // This wait only spaces two physical counter endpoints.  A
            // synchronous durable topology attestation here becomes part of
            // that physical interval and can consume the unchanged 600 ms
            // attribution wall before the counter read is even scheduled.
            // Runtime-dir/process revocations still wake the owner.  The
            // counter scheduler performs an exact capture attestation at the
            // read boundary, and transport dispatch independently performs
            // the stronger durable authority attestation before it can use
            // the resulting endpoint.  Therefore the cadence itself carries
            // no authority and must perform no synchronous work.
            return Ok(LoadedCounterReactorWait::SamplingCadence(
                self.next_rate_sample.saturating_duration_since(now),
            ));
        }
        if self.schedule_loaded_counter(runtime_dir, store, actuator)? {
            return Ok(LoadedCounterReactorWait::CounterCompletion);
        }
        if self.loaded_counter_cycle == LoadedCounterCycleState::FlightActive
            && self.loaded_transport_work_active()
        {
            return Ok(LoadedCounterReactorWait::TransportCompletion);
        }
        // A tracker reset or a same-turn completion can make the internal
        // cadence gate slightly newer than `next_rate_sample`.  That is not a
        // capture failure and must not spin.  End this bounded burst and let
        // the normal owner turn re-establish the next exact endpoint.
        self.loaded_counter_burst = LoadedCounterBurstState::Idle;
        self.loaded_topology_attestation = None;
        self.loaded_transport_authority = None;
        Ok(LoadedCounterReactorWait::Inactive)
    }

    fn next_sample_deadline(&self) -> Result<Option<Duration>, String> {
        if !self.session.accepting_observations() {
            return Ok(None);
        }
        let Some(request) = self.session.active_request() else {
            return Ok(None);
        };
        let now = Instant::now();
        let rate = self.next_rate_sample.saturating_duration_since(now);
        let mut deadline = if request.phase == AutotuneCapturePhase::IdleBaseline {
            rate
        } else {
            let cpu = self.next_cpu_sample.saturating_duration_since(now);
            loaded_reactor_deadline(rate, cpu, self.pending_counter_fence.is_some())
        };
        let policy = self
            .policy
            .as_ref()
            .ok_or_else(|| "active bootstrap capture has no immutable policy".to_string())?;
        let transport = self
            .transport
            .as_ref()
            .ok_or_else(|| "active bootstrap capture has no transport runtime".to_string())?;
        if let Some(transport) = transport.next_schedule_wake(request, policy, now)? {
            deadline = deadline.min(transport);
        }
        Ok(Some(deadline))
    }

    fn record_attested_observation(
        &mut self,
        runtime_dir: &Path,
        store: &RuntimeOverrideStore,
        actuator: &mut OpenWrtBootstrapRuntimeActuator,
        request: &AutotuneCaptureRequest,
        kind: super::autotune_capture::AutotuneCaptureObservationKind,
    ) -> CaptureRuntimeResult<AutotuneCaptureSnapshot> {
        self.record_attested_observations(
            runtime_dir,
            store,
            actuator,
            request,
            std::iter::once(kind),
        )
    }

    fn record_attested_observations<I>(
        &mut self,
        runtime_dir: &Path,
        store: &RuntimeOverrideStore,
        actuator: &mut OpenWrtBootstrapRuntimeActuator,
        request: &AutotuneCaptureRequest,
        kinds: I,
    ) -> CaptureRuntimeResult<AutotuneCaptureSnapshot>
    where
        I: IntoIterator<Item = super::autotune_capture::AutotuneCaptureObservationKind>,
    {
        self.record_observations_with_attestor(runtime_dir, request, kinds, |request, boot_ms| {
            actuator.attest_capture(store, request, boot_ms)
        })
    }

    fn record_observations_with_attestor<I, F>(
        &mut self,
        runtime_dir: &Path,
        request: &AutotuneCaptureRequest,
        kinds: I,
        attest: F,
    ) -> CaptureRuntimeResult<AutotuneCaptureSnapshot>
    where
        I: IntoIterator<Item = super::autotune_capture::AutotuneCaptureObservationKind>,
        F: FnOnce(&AutotuneCaptureRequest, u64) -> CaptureRuntimeResult<(u64, u64)>,
    {
        let boot_ms = monotonic_boot_ms().map_err(|error| ("capture-clock-unavailable", error))?;
        let reference = attest(request, boot_ms)?;
        if self.idle_rate_reference != Some(reference) {
            return Err((
                "capture-runtime-mismatch",
                "bootstrap capture rate authority changed after admission".to_string(),
            ));
        }
        let snapshot = self
            .session
            .observe_batch(request, kinds, boot_ms)
            .map_err(|error| ("capture-observation-invalid", error))?;
        if request.phase == AutotuneCapturePhase::IdleBaseline
            && snapshot.state == super::full_autotune::AutotuneCaptureState::Complete
        {
            self.idle_icmp_baseline_ms =
                snapshot.idle_median_us.map(|value| value as f64 / 1_000.0);
            self.persist_completed_idle_baseline(runtime_dir, &snapshot)?;
        }
        Self::publish_if_changed(runtime_dir, &snapshot)
            .map_err(|error| ("capture-publication-unavailable", error))?;
        Ok(snapshot)
    }

    fn persist_completed_idle_baseline(
        &mut self,
        runtime_dir: &Path,
        snapshot: &AutotuneCaptureSnapshot,
    ) -> CaptureRuntimeResult {
        let policy = self.policy.as_ref().ok_or_else(|| {
            (
                "capture-policy-unavailable",
                "bootstrap capture policy is not initialized".to_string(),
            )
        })?;
        let baseline = BootstrapIdleBaseline::from_snapshot(snapshot, policy)
            .map_err(|error| ("capture-idle-baseline-invalid", error))?;
        publish_idle_baseline(runtime_dir, &baseline)
            .map_err(|error| ("capture-idle-baseline-unavailable", error))?;
        self.idle_icmp_baseline_ms = Some(baseline.icmp_baseline_us as f64 / 1_000.0);
        self.idle_transport_baseline_ms = Some(baseline.transport_baseline_us as f64 / 1_000.0);
        Ok(())
    }

    fn restore_loaded_idle_baseline(
        &mut self,
        runtime_dir: &Path,
        request: &AutotuneCaptureRequest,
    ) -> Result<(), String> {
        let policy = self
            .policy
            .as_ref()
            .ok_or_else(|| "bootstrap capture policy is not initialized".to_string())?;
        let baseline = read_idle_baseline(runtime_dir)?
            .ok_or_else(|| "bootstrap loaded capture has no durable idle baseline".to_string())?;
        baseline.attest_loaded(request, policy)?;
        self.idle_icmp_baseline_ms = Some(baseline.icmp_baseline_us as f64 / 1_000.0);
        self.idle_transport_baseline_ms = Some(baseline.transport_baseline_us as f64 / 1_000.0);
        if let Some(transport) = self.transport.as_mut() {
            transport.idle_baseline_ms = self.idle_transport_baseline_ms;
        }
        Ok(())
    }

    fn prepare_idle_request(
        &mut self,
        runtime_dir: &Path,
        request: &AutotuneCaptureRequest,
    ) -> Result<(), String> {
        let path = runtime_dir.join(IDLE_BASELINE_FILE);
        if let Some(baseline) = read_idle_baseline(runtime_dir)? {
            if baseline.same_idle_capture(request) {
                let policy = self
                    .policy
                    .as_ref()
                    .ok_or_else(|| "bootstrap capture policy is not initialized".to_string())?;
                if baseline.policy_id != policy.id().as_str()
                    || baseline.policy_sha256 != policy.canonical_sha256()?
                {
                    return Err("bootstrap idle baseline policy changed after capture".to_string());
                }
                self.idle_icmp_baseline_ms = Some(baseline.icmp_baseline_us as f64 / 1_000.0);
                self.idle_transport_baseline_ms =
                    Some(baseline.transport_baseline_us as f64 / 1_000.0);
                if let Some(transport) = self.transport.as_mut() {
                    transport.idle_baseline_ms = self.idle_transport_baseline_ms;
                }
                return Ok(());
            }
            if !baseline.same_job(request) {
                return Err(
                    "bootstrap idle baseline belongs to a different job identity".to_string(),
                );
            }
            fs::remove_file(&path)
                .map_err(|error| format!("unable to remove stale idle baseline: {error}"))?;
        }
        self.idle_icmp_baseline_ms = None;
        self.idle_transport_baseline_ms = None;
        if let Some(transport) = self.transport.as_mut() {
            transport.idle_samples_ms.clear();
            transport.idle_baseline_ms = None;
        }
        Ok(())
    }

    fn restore_terminal_idle_baseline(
        &mut self,
        runtime_dir: &Path,
        snapshot: &AutotuneCaptureSnapshot,
        operation: &OperationRequest,
    ) -> Result<(), String> {
        snapshot.validate()?;
        let policy = operation
            .capture_policy
            .ok_or_else(|| "bootstrap operation has no immutable capture policy".to_string())?
            .expand()?;
        let baseline = read_idle_baseline(runtime_dir)?
            .ok_or_else(|| "complete bootstrap idle capture has no durable baseline".to_string())?;
        if snapshot.state != super::full_autotune::AutotuneCaptureState::Complete
            || !baseline.same_idle_capture(&snapshot.request)
            || baseline.policy_id != policy.id().as_str()
            || baseline.policy_sha256 != policy.canonical_sha256()?
            || snapshot.idle_median_us != Some(baseline.icmp_baseline_us)
            || snapshot.idle_transport_baseline_us != Some(baseline.transport_baseline_us)
            || snapshot.updated_boot_ms != baseline.observed_boot_ms
        {
            return Err("complete bootstrap idle baseline is not authoritative".to_string());
        }
        self.idle_icmp_baseline_ms = Some(baseline.icmp_baseline_us as f64 / 1_000.0);
        self.idle_transport_baseline_ms = Some(baseline.transport_baseline_us as f64 / 1_000.0);
        Ok(())
    }

    fn reset_counter_identity(&mut self, request: &AutotuneCaptureRequest) {
        if self.counter_request.as_ref() == Some(request) {
            return;
        }
        if let Some(sampler) = self.counter_sampler.as_mut() {
            sampler.invalidate_current();
            self.counter_epoch = sampler.current_epoch();
        }
        self.counter_request = Some(request.clone());
        if let Some(rates) = self.counter_rates.as_mut() {
            rates.reset(Instant::now());
        }
        if let Some(physical) = self.counter_physical.as_mut() {
            physical.reset(Instant::now());
        }
        self.pending_counter_fence = None;
        self.loaded_counter_cycle = LoadedCounterCycleState::NeedAttestedBaseline;
        self.loaded_counter_burst = LoadedCounterBurstState::Idle;
        self.loaded_topology_attestation = None;
        self.loaded_transport_authority = None;
        self.loaded_ack_credit = None;
        self.loaded_phase_windows.clear();
        self.loaded_physical_deltas.clear();
        self.pending_transport_completion = None;
        self.loaded_phase_expired = true;
        self.loaded_counter_diagnostic_budget =
            if request.phase == AutotuneCapturePhase::LoadedMeasurement {
                MAX_LOADED_PHASE_WINDOWS
            } else {
                0
            };
        self.loaded_transport_diagnostic_budget =
            if request.phase == AutotuneCapturePhase::LoadedMeasurement {
                MAX_LOADED_TRANSPORT_DIAGNOSTICS
            } else {
                0
            };
        self.pending_icmp_samples.clear();
    }

    fn observe_transport_phase(
        &mut self,
        request: &AutotuneCaptureRequest,
        phase: Option<(bool, bool)>,
        policy: &super::autotune_capture_policy::AutotuneCapturePolicy,
        now: Instant,
    ) -> CaptureRuntimeResult {
        self.transport
            .as_mut()
            .ok_or_else(|| {
                (
                    "capture-transport-unavailable",
                    "bootstrap transport runtime is not initialized".to_string(),
                )
            })?
            .observe_phase(request, phase, policy, now)
            .map_err(|error| ("capture-transport-phase-invalid", error))
    }

    fn accept_counter_completion<A: BootstrapCaptureAuthority>(
        &mut self,
        store: &RuntimeOverrideStore,
        actuator: &mut A,
        request: &AutotuneCaptureRequest,
    ) -> CaptureRuntimeResult<Option<CounterPhaseUpdate>> {
        let completion = self
            .counter_sampler
            .as_mut()
            .ok_or_else(|| {
                (
                    "capture-speedtest-counters-unavailable",
                    "bootstrap counter sampler is not initialized".to_string(),
                )
            })?
            .try_take()
            .map_err(|error| ("capture-speedtest-counters-unavailable", error))?;
        let Some(completion) = completion else {
            return Ok(None);
        };
        let policy = self.policy.clone().ok_or_else(|| {
            (
                "capture-policy-unavailable",
                "bootstrap capture policy is not initialized".to_string(),
            )
        })?;
        let fence = match self.pending_counter_fence.take() {
            Some(fence) => fence,
            None if completion.epoch < self.counter_epoch => return Ok(None),
            None => {
                return Err((
                    "capture-speedtest-counters-invalid",
                    "bootstrap counter completion has no exact read fence".to_string(),
                ))
            }
        };
        match counter_completion_is_current(
            request,
            self.counter_epoch,
            &fence,
            &completion,
            BootstrapTransportRuntime::dropout(&policy),
        ) {
            Ok(CounterCompletionStatus::Current) => {}
            Ok(CounterCompletionStatus::Stale) => {
                self.loaded_topology_attestation = None;
                self.loaded_transport_authority = None;
                self.loaded_ack_credit = None;
                return Ok(None);
            }
            Err(CounterCompletionError::Expired) => {
                return Err((
                    "capture-speedtest-counters-expired",
                    "bootstrap counter completion is outside the capture deadline".to_string(),
                ))
            }
            Err(CounterCompletionError::Identity) => {
                return Err((
                    "capture-speedtest-counters-invalid",
                    "bootstrap counter completion does not match the active capture".to_string(),
                ))
            }
        }
        let counters = completion
            .outcome
            .map_err(|error| ("capture-speedtest-counters-unavailable", error))?;
        let counters_present = counters.is_some();
        let topology_attestation = if matches!(
            fence.purpose,
            LoadedCounterReadPurpose::Evidence | LoadedCounterReadPurpose::FlightPost
        ) {
            let boot_ms =
                monotonic_boot_ms().map_err(|error| ("capture-clock-unavailable", error))?;
            actuator.attest_capture(store, request, boot_ms)?;
            Some(LoadedTopologyAttestation {
                request: request.clone(),
                epoch: self.counter_epoch,
                attested_at: Instant::now(),
            })
        } else {
            None
        };
        let rolling = self
            .counter_rates
            .as_mut()
            .ok_or_else(|| {
                (
                    "capture-speedtest-counters-unavailable",
                    "bootstrap rolling counter tracker is not initialized".to_string(),
                )
            })?
            .observe_counters_with_delta(completion.completed_at, counters);
        let physical = self.counter_physical.as_mut().ok_or_else(|| {
            (
                "capture-speedtest-counters-unavailable",
                "bootstrap physical counter tracker is not initialized".to_string(),
            )
        })?;
        let observation = match fence.purpose {
            LoadedCounterReadPurpose::AttestedBaseline | LoadedCounterReadPurpose::Rebase => {
                physical.reset(completion.completed_at);
                let primed =
                    physical.observe_counters_with_delta(completion.completed_at, counters);
                if primed.delta.is_some() {
                    return Err((
                        "capture-speedtest-counters-invalid",
                        "bootstrap counter rebase unexpectedly produced a physical delta"
                            .to_string(),
                    ));
                }
                super::autotune_counter::AutotuneCounterObservation {
                    rate_window: rolling.rate_window,
                    delta: None,
                }
            }
            LoadedCounterReadPurpose::Evidence
            | LoadedCounterReadPurpose::FlightEvidence
            | LoadedCounterReadPurpose::FlightPost => {
                let physical =
                    physical.observe_counters_with_delta(completion.completed_at, counters);
                super::autotune_counter::AutotuneCounterObservation {
                    rate_window: rolling.rate_window,
                    delta: physical.delta,
                }
            }
        };
        let interval = Duration::from_millis(u64::from(policy.rate_sample_interval_ms()));
        self.next_rate_sample = completion
            .completed_at
            .checked_add(interval)
            .unwrap_or(completion.completed_at);
        if fence.purpose == LoadedCounterReadPurpose::FlightPost && observation.delta.is_none() {
            return Err((
                "capture-speedtest-counters-invalid",
                "bootstrap post-flight counter read has no exact physical delta".to_string(),
            ));
        }
        let (window, physical_delta, preserve_ack_credit) = if matches!(
            fence.purpose,
            LoadedCounterReadPurpose::Evidence
                | LoadedCounterReadPurpose::FlightEvidence
                | LoadedCounterReadPurpose::FlightPost
        ) {
            loaded_phase_window_with_ack_credit(
                request,
                observation,
                &policy,
                self.counter_epoch,
                &mut self.loaded_ack_credit,
            )
            .map_err(|error| ("capture-speedtest-counters-invalid", error))?
        } else {
            (None, None, true)
        };
        let diagnostic = loaded_counter_diagnostic(
            &fence,
            completion.completed_at,
            counters_present,
            observation,
            window.is_some(),
        );
        let expected = crate::AutotuneTransportControl::expected_phase(request)
            .map_err(|error| ("capture-speedtest-counters-invalid", error))?;
        let authority = (fence.purpose == LoadedCounterReadPurpose::FlightEvidence)
            .then(|| {
                self.loaded_topology_attestation
                    .as_ref()
                    .and_then(|attestation| {
                        physical_delta.as_ref().and_then(|delta| {
                            attestation.authorize(
                                request,
                                self.counter_epoch,
                                delta,
                                expected,
                                BootstrapTransportRuntime::dropout(&policy),
                            )
                        })
                    })
            })
            .flatten();
        Ok(Some(CounterPhaseUpdate {
            observed_at: completion.completed_at,
            topology_attestation,
            authority,
            window,
            physical_delta,
            purpose: fence.purpose,
            preserve_ack_credit,
            diagnostic,
        }))
    }

    fn apply_loaded_counter_update(
        &mut self,
        request: &AutotuneCaptureRequest,
        policy: &super::autotune_capture_policy::AutotuneCapturePolicy,
        update: CounterPhaseUpdate,
        now: Instant,
    ) -> CaptureRuntimeResult {
        let CounterPhaseUpdate {
            observed_at,
            window,
            physical_delta,
            topology_attestation,
            authority,
            purpose,
            preserve_ack_credit,
            diagnostic,
        } = update;
        let endpoint_present = match purpose {
            LoadedCounterReadPurpose::FlightEvidence => physical_delta.is_some(),
            _ => diagnostic.outcome != LoadedCounterDiagnosticOutcome::CountersMissing,
        };
        let next_cycle = self
            .loaded_counter_cycle
            .after_read(purpose, endpoint_present)
            .map_err(|error| ("capture-speedtest-counters-invalid", error))?;
        match purpose {
            LoadedCounterReadPurpose::AttestedBaseline => {
                self.loaded_topology_attestation = None;
                self.loaded_transport_authority = None;
                self.loaded_ack_credit = None;
                self.loaded_counter_cycle = next_cycle;
                self.log_loaded_counter_diagnostic(request, policy, diagnostic, now);
                return Ok(());
            }
            LoadedCounterReadPurpose::Rebase => {
                self.loaded_transport_authority = None;
                if endpoint_present {
                    self.loaded_counter_cycle = next_cycle;
                } else {
                    self.loaded_topology_attestation = None;
                    self.loaded_transport_authority = None;
                    self.loaded_ack_credit = None;
                    self.loaded_counter_cycle = next_cycle;
                }
                self.log_loaded_counter_diagnostic(request, policy, diagnostic, now);
                return Ok(());
            }
            LoadedCounterReadPurpose::Evidence | LoadedCounterReadPurpose::FlightPost => {
                self.loaded_topology_attestation = topology_attestation;
                self.loaded_transport_authority = None;
                self.loaded_counter_cycle = next_cycle;
            }
            LoadedCounterReadPurpose::FlightEvidence => {
                self.loaded_topology_attestation = None;
                self.loaded_transport_authority = authority.clone();
                self.loaded_counter_cycle = next_cycle;
            }
        }
        if let Some(physical_delta) = physical_delta {
            self.loaded_physical_deltas.push_back(physical_delta);
            while self.loaded_physical_deltas.len() > MAX_LOADED_PHASE_WINDOWS {
                self.loaded_physical_deltas.pop_front();
            }
        }
        if purpose != LoadedCounterReadPurpose::FlightEvidence {
            debug_assert!(authority.is_none());
        }
        if let Some(window) = window {
            self.loaded_phase_windows.push_back(window);
            while self.loaded_phase_windows.len() > MAX_LOADED_PHASE_WINDOWS {
                self.loaded_phase_windows.pop_front();
            }
            self.loaded_phase_expired = false;
            let timeline = loaded_phase_timeline(request, &self.loaded_phase_windows);
            self.transport
                .as_mut()
                .ok_or_else(|| {
                    (
                        "capture-transport-unavailable",
                        "bootstrap transport runtime is not initialized".to_string(),
                    )
                })?
                .replace_loaded_phase_history(
                    request,
                    &timeline,
                    &self.loaded_physical_deltas,
                    policy,
                )
                .map_err(|error| ("capture-transport-phase-invalid", error))?;
        } else {
            // A first 200 ms physical delta can be authoritative before the
            // 800 ms rolling-rate window exists.  Seed the exact capture key,
            // then append that non-overlapping delta.  This authorizes only
            // speculative dispatch; post-flight acceptance still requires the
            // complete physical bracket.
            self.observe_transport_phase(request, None, policy, observed_at)?;
            if let Some(physical_delta) = physical_delta {
                self.transport
                    .as_mut()
                    .ok_or_else(|| {
                        (
                            "capture-transport-unavailable",
                            "bootstrap transport runtime is not initialized".to_string(),
                        )
                    })?
                    .append_loaded_physical_delta(request, physical_delta, policy)
                    .map_err(|error| ("capture-transport-phase-invalid", error))?;
            }
            if !preserve_ack_credit {
                self.loaded_ack_credit = None;
            }
            self.loaded_phase_expired = true;
        }
        self.log_loaded_counter_diagnostic(request, policy, diagnostic, now);
        Ok(())
    }

    fn log_loaded_counter_diagnostic(
        &mut self,
        request: &AutotuneCaptureRequest,
        policy: &super::autotune_capture_policy::AutotuneCapturePolicy,
        diagnostic: LoadedCounterDiagnostic,
        now: Instant,
    ) {
        if request.phase != AutotuneCapturePhase::LoadedMeasurement
            || self.loaded_counter_diagnostic_budget == 0
        {
            return;
        }
        self.loaded_counter_diagnostic_budget -= 1;
        let newest = self.loaded_phase_windows.back();
        let phase = newest.map(|window| window.phase);
        let newest_age_ms = newest
            .map(|window| bounded_duration_ms(now.saturating_duration_since(window.observed_end)));
        let tail_resolution = newest.map_or_else(
            || "none".to_string(),
            |window| match loaded_phase_at(request, &self.loaded_phase_windows, window.observed_end)
            {
                LoadedPhaseLookup::Covered((download, upload)) => {
                    format!("{}:{}", u8::from(download), u8::from(upload))
                }
                LoadedPhaseLookup::Pending => "pending".to_string(),
                LoadedPhaseLookup::Uncovered => "uncovered".to_string(),
            },
        );
        let readiness = self
            .transport
            .as_ref()
            .and_then(|transport| transport.readiness(request, policy, now).ok());
        let authority = self
            .loaded_transport_authority
            .as_ref()
            .is_some_and(|authority| {
                authority.permits(
                    request,
                    self.counter_epoch,
                    self.loaded_physical_deltas.back(),
                    now,
                    BootstrapTransportRuntime::dropout(policy),
                )
            });
        let show = |value: Option<u64>| value.map_or_else(|| "-".to_string(), |v| v.to_string());
        let phase = phase.map_or_else(
            || "-".to_string(),
            |(download, upload)| format!("{}:{}", u8::from(download), u8::from(upload)),
        );
        eprintln!(
            "bootstrap loaded counter diagnostic: outcome={} read_ms={} delta_ms={} window_ms={} dl_kbps={} ul_kbps={} phase={} tail={} windows={} pending_icmp={} newest_age_ms={} readiness={} ready={} authority={} remaining={}",
            diagnostic.outcome.as_str(),
            diagnostic.read_latency_ms,
            show(diagnostic.delta_span_ms),
            show(diagnostic.window_span_ms),
            show(diagnostic.download_kbps),
            show(diagnostic.upload_kbps),
            phase,
            tail_resolution,
            self.loaded_phase_windows.len(),
            self.pending_icmp_samples.len(),
            show(newest_age_ms),
            readiness.map_or("readiness-invalid", |value| value.reason),
            u8::from(readiness.is_some_and(|value| value.ready)),
            u8::from(authority),
            self.loaded_counter_diagnostic_budget,
        );
    }

    fn poll_rate_sample(
        &mut self,
        runtime_dir: &Path,
        store: &RuntimeOverrideStore,
        actuator: &mut OpenWrtBootstrapRuntimeActuator,
        request: &AutotuneCaptureRequest,
        now: Instant,
    ) -> CaptureRuntimeResult {
        let policy = self.policy.clone().ok_or_else(|| {
            (
                "capture-policy-unavailable",
                "bootstrap capture policy is not initialized".to_string(),
            )
        })?;
        let interval = Duration::from_millis(u64::from(policy.rate_sample_interval_ms()));
        match request.phase {
            AutotuneCapturePhase::IdleBaseline => {
                if now < self.next_rate_sample {
                    return Ok(());
                }
                self.reset_counter_identity(request);
                let rates = self
                    .rate_monitor
                    .as_mut()
                    .ok_or_else(|| {
                        (
                            "capture-rate-unavailable",
                            "bootstrap physical rate monitor is not initialized".to_string(),
                        )
                    })?
                    .try_sample_at(now)
                    .map_err(|error| ("capture-rate-unavailable", error.to_string()))?;
                if !rates.fresh {
                    // A min-interval miss is not evidence that the route left
                    // the idle phase.  Keep the last measured phase until the
                    // readiness freshness bound revokes it; writing a
                    // synthetic `None` here aliases scheduler jitter into a
                    // false phase change and can suppress the next probe.
                    self.next_rate_sample = now.checked_add(interval).unwrap_or(now);
                    return Ok(());
                }
                let (download_reference, upload_reference) =
                    self.idle_rate_reference.ok_or_else(|| {
                        (
                            "capture-idle-reference-missing",
                            "bootstrap idle capture has no rate authority".to_string(),
                        )
                    })?;
                let kind = super::autotune_capture::idle_traffic_observation_kind(
                    request,
                    rates.dl_kbps,
                    rates.ul_kbps,
                    download_reference as f64,
                    upload_reference as f64,
                )
                .map_err(|error| ("capture-traffic-invalid", error))?;
                if let Some(kind) = kind {
                    self.record_attested_observation(runtime_dir, store, actuator, request, kind)?;
                }
                // Traffic observation publication performs exact UCI, route
                // and kernel-topology attestation. On real routers that work
                // can exceed the immutable transport phase freshness wall.
                // Never retimestamp the pre-attestation sample: re-read the
                // physical counters after the attestation and authorize the
                // phase only from that positive, fresh measurement event.
                let attested_at = Instant::now();
                let freshness = BootstrapTransportRuntime::dropout(&policy);
                let phase_observed_at =
                    post_attestation_idle_phase_time(now, attested_at, freshness, || {
                        self.rate_monitor
                            .as_mut()
                            .expect("bootstrap physical rate monitor was initialized")
                            .try_sample_at(attested_at)
                            .map(|sample| sample.fresh)
                            .map_err(|error| ("capture-rate-unavailable", error.to_string()))
                    })?;
                let Some(phase_observed_at) = phase_observed_at else {
                    self.next_rate_sample =
                        attested_at.checked_add(interval).unwrap_or(attested_at);
                    return Ok(());
                };
                self.observe_transport_phase(
                    request,
                    Some((false, false)),
                    &policy,
                    phase_observed_at,
                )?;
                self.next_rate_sample = phase_observed_at
                    .checked_add(interval)
                    .unwrap_or(phase_observed_at);
            }
            AutotuneCapturePhase::LoadedMeasurement => {
                self.reset_counter_identity(request);
                if let Some(update) = self.accept_counter_completion(store, actuator, request)? {
                    self.apply_loaded_counter_update(request, &policy, update, Instant::now())?;
                }

                let maximum_age = Duration::from_millis(
                    u64::from(policy.rate_sample_interval_ms()).saturating_mul(3),
                );
                if !self.loaded_phase_expired
                    && self.loaded_phase_windows.back().is_some_and(|window| {
                        now.checked_duration_since(window.observed_end)
                            .is_some_and(|age| age > maximum_age)
                    })
                {
                    self.observe_transport_phase(request, None, &policy, now)?;
                    self.loaded_topology_attestation = None;
                    self.loaded_transport_authority = None;
                    self.loaded_ack_credit = None;
                    self.loaded_phase_expired = true;
                }
            }
        }
        Ok(())
    }

    fn schedule_loaded_counter<A: BootstrapCaptureAuthority>(
        &mut self,
        runtime_dir: &Path,
        store: &RuntimeOverrideStore,
        actuator: &mut A,
    ) -> CaptureRuntimeResult<bool> {
        if !self.session.accepting_observations() {
            self.pending_transport_completion = None;
            return Ok(false);
        }
        let request = self.session.active_request().cloned().ok_or_else(|| {
            (
                "capture-observation-invalid",
                "bootstrap counter scheduler lost its active request".to_string(),
            )
        })?;
        if request.phase != AutotuneCapturePhase::LoadedMeasurement
            || self.pending_counter_fence.is_some()
        {
            return Ok(false);
        }
        let Some(purpose) = self
            .loaded_counter_cycle
            .next_read(self.pending_transport_completion.is_some())
        else {
            return Ok(false);
        };
        let now = Instant::now();
        if matches!(
            purpose,
            LoadedCounterReadPurpose::Evidence | LoadedCounterReadPurpose::FlightEvidence
        ) {
            if now < self.next_rate_sample {
                return Ok(false);
            }
            let due = self
                .counter_rates
                .as_ref()
                .ok_or_else(|| {
                    (
                        "capture-speedtest-counters-unavailable",
                        "bootstrap counter rate tracker is not initialized".to_string(),
                    )
                })?
                .sample_due(now);
            if !due {
                return Ok(false);
            }
        }
        let boot_ms = monotonic_boot_ms().map_err(|error| ("capture-clock-unavailable", error))?;
        match purpose {
            LoadedCounterReadPurpose::AttestedBaseline => {
                actuator.attest_capture(store, &request, boot_ms)?;
            }
            LoadedCounterReadPurpose::Evidence
            | LoadedCounterReadPurpose::Rebase
            | LoadedCounterReadPurpose::FlightEvidence
            | LoadedCounterReadPurpose::FlightPost => {
                actuator.attest_loaded_transport_durable_authority(
                    runtime_dir,
                    store,
                    &request,
                    boot_ms,
                )?;
            }
        }
        let scheduled_at = Instant::now();
        let scheduled = self
            .counter_sampler
            .as_mut()
            .ok_or_else(|| {
                (
                    "capture-speedtest-counters-unavailable",
                    "bootstrap counter sampler is not initialized".to_string(),
                )
            })?
            .try_schedule(&request)
            .map_err(|error| ("capture-speedtest-counters-unavailable", error))?;
        if scheduled {
            if purpose == LoadedCounterReadPurpose::Rebase {
                // The durable attestation immediately preceding Rebase is the
                // causal topology fence for the next physical delta. Rebase
                // establishes the byte baseline after this timestamp;
                // FlightEvidence therefore measures only later traffic. Do
                // not retain the older Evidence attestation across a slow
                // OpenWrt UCI/route/netlink pass.
                self.loaded_topology_attestation = Some(LoadedTopologyAttestation {
                    request: request.clone(),
                    epoch: self.counter_epoch,
                    attested_at: scheduled_at,
                });
                self.loaded_transport_authority = None;
            }
            self.pending_counter_fence = Some(CounterReadFence {
                request,
                epoch: self.counter_epoch,
                scheduled_at,
                purpose,
            });
        }
        Ok(scheduled)
    }

    /// Start one bounded loaded counter burst only after every unrelated
    /// synchronous observation in the normal owner turn has completed.  The
    /// burst may continue across only the explicit completion and flight
    /// bounds; it must then yield back to CPU, ICMP and runtime health.
    fn schedule_loaded_counter_after_observations<A: BootstrapCaptureAuthority>(
        &mut self,
        runtime_dir: &Path,
        store: &RuntimeOverrideStore,
        actuator: &mut A,
    ) -> CaptureRuntimeResult {
        if self.schedule_loaded_counter(runtime_dir, store, actuator)? {
            self.loaded_counter_burst = LoadedCounterBurstState::begin();
        }
        Ok(())
    }

    fn poll_cpu_sample(
        &mut self,
        runtime_dir: &Path,
        store: &RuntimeOverrideStore,
        actuator: &mut OpenWrtBootstrapRuntimeActuator,
        request: &AutotuneCaptureRequest,
        now: Instant,
    ) -> CaptureRuntimeResult {
        if request.phase != AutotuneCapturePhase::LoadedMeasurement || now < self.next_cpu_sample {
            return Ok(());
        }
        let interval = self
            .policy
            .as_ref()
            .map(|policy| Duration::from_millis(u64::from(policy.cpu_sample_interval_ms())))
            .ok_or_else(|| {
                (
                    "capture-policy-unavailable",
                    "bootstrap capture policy is not initialized".to_string(),
                )
            })?;
        self.next_cpu_sample = now.checked_add(interval).unwrap_or(now);
        let stats = self
            .cpu_monitor
            .as_mut()
            .ok_or_else(|| {
                (
                    "capture-cpu-unavailable",
                    "bootstrap CPU monitor is not initialized".to_string(),
                )
            })?
            .sample()
            .map_err(|error| ("capture-cpu-unavailable", error.to_string()))?;
        let Some(stats) = stats else {
            return Ok(());
        };
        if !stats.total_percent.is_finite() || !(0.0..=100.0).contains(&stats.total_percent) {
            return Err((
                "capture-cpu-invalid",
                "bootstrap CPU sample is outside its valid range".to_string(),
            ));
        }
        self.record_attested_observation(
            runtime_dir,
            store,
            actuator,
            request,
            super::autotune_capture::AutotuneCaptureObservationKind::Cpu {
                milli_percent: (stats.total_percent * 1_000.0).round() as u32,
            },
        )?;
        Ok(())
    }

    fn poll_rate_measurement(
        &mut self,
        runtime_dir: &Path,
        store: &RuntimeOverrideStore,
        actuator: &mut OpenWrtBootstrapRuntimeActuator,
    ) -> CaptureRuntimeResult {
        if !self.session.accepting_observations() {
            return Ok(());
        }
        let request = self.session.active_request().cloned().ok_or_else(|| {
            (
                "capture-observation-invalid",
                "bootstrap capture session lost its active request".to_string(),
            )
        })?;
        let now = Instant::now();
        self.poll_rate_sample(runtime_dir, store, actuator, &request, now)
    }

    fn poll_cpu_measurement(
        &mut self,
        runtime_dir: &Path,
        store: &RuntimeOverrideStore,
        actuator: &mut OpenWrtBootstrapRuntimeActuator,
    ) -> CaptureRuntimeResult {
        if !self.session.accepting_observations() {
            return Ok(());
        }
        let request = self.session.active_request().cloned().ok_or_else(|| {
            (
                "capture-observation-invalid",
                "bootstrap capture session lost its active request".to_string(),
            )
        })?;
        self.poll_cpu_sample(runtime_dir, store, actuator, &request, Instant::now())
    }

    fn try_schedule_transport<A: BootstrapCaptureAuthority>(
        &mut self,
        runtime_dir: &Path,
        store: &RuntimeOverrideStore,
        actuator: &mut A,
    ) -> CaptureRuntimeResult {
        if !self.session.accepting_observations() {
            return Ok(());
        }
        if self.pending_transport_completion.is_some() {
            return Ok(());
        }
        let request = self.session.active_request().cloned().ok_or_else(|| {
            (
                "capture-observation-invalid",
                "bootstrap transport lost its active request".to_string(),
            )
        })?;
        let policy = self.policy.clone().ok_or_else(|| {
            (
                "capture-policy-unavailable",
                "bootstrap capture policy is not initialized".to_string(),
            )
        })?;
        if request.phase == AutotuneCapturePhase::LoadedMeasurement
            && self.loaded_counter_cycle != LoadedCounterCycleState::FlightReady
        {
            return Ok(());
        }
        let now = Instant::now();
        let readiness = self
            .transport
            .as_ref()
            .ok_or_else(|| {
                (
                    "capture-transport-unavailable",
                    "bootstrap transport runtime is not initialized".to_string(),
                )
            })?
            .readiness(&request, &policy, now)
            .map_err(|error| ("capture-transport-phase-invalid", error))?;
        let schedule_at = if readiness.ready {
            let boot_ms =
                monotonic_boot_ms().map_err(|error| ("capture-clock-unavailable", error))?;
            match request.phase {
                AutotuneCapturePhase::IdleBaseline => {
                    actuator.attest_capture(store, &request, boot_ms)?;
                }
                AutotuneCapturePhase::LoadedMeasurement => {
                    actuator.attest_loaded_transport_durable_authority(
                        runtime_dir,
                        store,
                        &request,
                        boot_ms,
                    )?;
                }
            }
            // Either attestation can consume some of the immutable freshness
            // budget.  Always decide with its completion time.
            Instant::now()
        } else {
            now
        };
        if readiness.ready && request.phase == AutotuneCapturePhase::IdleBaseline {
            // The dispatch attestation above is intentionally stronger than
            // the rolling rate observation, and on physical routers its UCI,
            // route and netlink reads may outlive the immutable phase
            // freshness wall. Never retimestamp the earlier observation.
            // Re-read physical counters after attestation and, when the
            // minimum counter interval has elapsed, replace phase authority
            // with this new positive measurement before try_schedule()
            // re-evaluates readiness.
            let refreshed = self
                .rate_monitor
                .as_mut()
                .ok_or_else(|| {
                    (
                        "capture-rate-unavailable",
                        "bootstrap physical rate monitor is not initialized".to_string(),
                    )
                })?
                .try_sample_at(schedule_at)
                .map_err(|error| ("capture-rate-unavailable", error.to_string()))?;
            let phase_authority = dispatch_idle_phase_time(
                now,
                schedule_at,
                BootstrapTransportRuntime::dropout(&policy),
                refreshed.fresh,
            );
            if phase_authority == Some(schedule_at) && refreshed.fresh {
                self.observe_transport_phase(&request, Some((false, false)), &policy, schedule_at)?;
                let interval = Duration::from_millis(u64::from(policy.rate_sample_interval_ms()));
                self.next_rate_sample = schedule_at.checked_add(interval).unwrap_or(schedule_at);
            } else if phase_authority.is_none() {
                self.transport
                    .as_mut()
                    .expect("bootstrap transport runtime was initialized")
                    .record_readiness(crate::AutotuneTransportReadiness {
                        phase: readiness.phase,
                        ready: false,
                        reason: "post-attestation-rate-sample-unavailable",
                    });
                return Ok(());
            }
        }
        if readiness.ready && request.phase == AutotuneCapturePhase::LoadedMeasurement {
            let freshness = BootstrapTransportRuntime::dropout(&policy);
            let authorized = self
                .loaded_transport_authority
                .as_ref()
                .is_some_and(|authority| {
                    authority.permits(
                        &request,
                        self.counter_epoch,
                        self.loaded_physical_deltas.back(),
                        schedule_at,
                        freshness,
                    )
                });
            if !authorized {
                self.transport
                    .as_mut()
                    .expect("bootstrap transport runtime was initialized")
                    .record_readiness(crate::AutotuneTransportReadiness {
                        phase: readiness.phase,
                        ready: false,
                        reason: "counter-attestation-fence-missing",
                    });
                return Ok(());
            }
        }
        let decision = self
            .transport
            .as_mut()
            .expect("bootstrap transport runtime was initialized")
            .try_schedule(&request, &policy, schedule_at)
            .map_err(|error| ("capture-transport-unavailable", error))?;
        if decision == BootstrapTransportScheduleDecision::Scheduled
            && request.phase == AutotuneCapturePhase::LoadedMeasurement
        {
            self.loaded_topology_attestation = None;
            self.loaded_transport_authority = None;
            self.loaded_counter_cycle = LoadedCounterCycleState::FlightActive;
        }
        Ok(())
    }

    fn poll_transport<A: BootstrapCaptureAuthority>(
        &mut self,
        runtime_dir: &Path,
        store: &RuntimeOverrideStore,
        actuator: &mut A,
        allow_schedule: bool,
    ) -> CaptureRuntimeResult {
        if !self.session.accepting_observations() {
            self.pending_transport_completion = None;
            return Ok(());
        }
        let policy = self.policy.clone().ok_or_else(|| {
            (
                "capture-policy-unavailable",
                "bootstrap capture policy is not initialized".to_string(),
            )
        })?;
        let mut completed = self
            .pending_transport_completion
            .take()
            .into_iter()
            .collect::<Vec<_>>();
        loop {
            let completion = self
                .transport
                .as_mut()
                .ok_or_else(|| {
                    (
                        "capture-transport-unavailable",
                        "bootstrap transport runtime is not initialized".to_string(),
                    )
                })?
                .try_take()
                .map_err(|error| ("capture-transport-unavailable", error))?;
            let Some(completion) = completion else {
                break;
            };
            if !completed.is_empty() {
                return Err((
                    "capture-transport-invalid",
                    "bootstrap transport produced more than one pending result".to_string(),
                ));
            }
            completed.push(completion);
        }

        for (flight, completion) in completed {
            let physical_attestation = if flight.physical_delta_required {
                match self
                    .transport
                    .as_ref()
                    .expect("bootstrap transport runtime was initialized")
                    .control
                    .settle_physical_interval_diagnostic(
                        &flight,
                        completion.probe_id,
                        completion.started_at,
                        completion.completed_at,
                        BootstrapTransportRuntime::dropout(&policy),
                    ) {
                    crate::AutotuneTransportSettlement::Pending(_) => {
                        self.pending_transport_completion = Some((flight, completion));
                        continue;
                    }
                    crate::AutotuneTransportSettlement::Final(attestation) => Some(attestation),
                }
            } else {
                None
            };
            let Some(active) = self.session.active_request().cloned() else {
                continue;
            };
            if active != completion.capture || !self.session.accepting_observations() {
                continue;
            }
            let (sample, deadline_failure) = match completion.outcome {
                Ok(sample) => (Some(sample), None),
                Err(failure)
                    if failure.kind()
                        == crate::transport_probe::TransportProbeFailureKind::DeadlineExceeded =>
                {
                    (None, Some(failure))
                }
                // DNS, routing, connection, protocol and apparatus failures
                // carry no latency authority.  The immutable capture may
                // schedule a later probe while it remains live.
                Err(failure) => {
                    if active.phase == AutotuneCapturePhase::LoadedMeasurement
                        && self.loaded_transport_diagnostic_budget > 0
                    {
                        self.loaded_transport_diagnostic_budget -= 1;
                        let diagnostic = loaded_transport_backend_failure_diagnostic(
                            completion.probe_id,
                            &failure,
                        );
                        eprintln!("bootstrap loaded transport backend rejected: {diagnostic}");
                    }
                    continue;
                }
            };
            if sample.as_ref().is_some_and(|sample| {
                sample.backend != crate::transport_probe::TransportProbeBackend::WebSocket
                    || !sample.trusted
                    || sample.endpoint != policy.transport_endpoint()
            }) {
                return Err((
                    "capture-transport-untrusted",
                    "bootstrap transport result does not match its immutable policy".to_string(),
                ));
            }
            let boot_ms =
                monotonic_boot_ms().map_err(|error| ("capture-clock-unavailable", error))?;
            actuator.attest_capture(store, &active, boot_ms)?;
            let expected_phase = flight.expected_phase;
            let rating_phase = match expected_phase {
                (false, false) => crate::RatingPhase::Idle,
                (true, false) => crate::RatingPhase::Download,
                (false, true) => crate::RatingPhase::Upload,
                (true, true) => crate::RatingPhase::Bidirectional,
            };
            let diagnostic = crate::TransportProbeResult {
                probe_id: completion.probe_id,
                started_at: completion.started_at,
                completed_at: completion.completed_at,
                control_valid: true,
                capture_interval_valid: None,
                endpoint: sample
                    .as_ref()
                    .map(|sample| sample.endpoint.clone())
                    .unwrap_or_else(|| policy.transport_endpoint().to_string()),
                dl_loaded: expected_phase.0,
                ul_loaded: expected_phase.1,
                rating_phase,
                autotune_capture: Some(active.clone()),
                latency_ms: sample.as_ref().map(|sample| sample.rtt_ms),
                error: deadline_failure
                    .as_ref()
                    .map(|failure| failure.message().to_string()),
                failure_kind: deadline_failure.as_ref().map(|failure| failure.kind()),
                failure_deadline_us: deadline_failure
                    .as_ref()
                    .and_then(|failure| failure.deadline_us()),
                route_identity: Some(active.route_fingerprint.clone()),
                backend: sample
                    .as_ref()
                    .map(|sample| sample.backend.as_str())
                    .unwrap_or("websocket")
                    .to_string(),
                trusted: sample.as_ref().map_or(true, |sample| sample.trusted),
                raw_samples_ms: sample
                    .as_ref()
                    .map(|sample| sample.raw_samples_ms.clone())
                    .unwrap_or_default(),
                discarded_samples: sample.as_ref().map_or(0, |sample| sample.discarded_samples),
                server_processing_ms: sample
                    .as_ref()
                    .map_or(0.0, |sample| sample.server_processing_ms),
                connection_reused: sample
                    .as_ref()
                    .is_some_and(|sample| sample.connection_reused),
            };
            let attestation = physical_attestation.unwrap_or_else(|| {
                self.transport
                    .as_ref()
                    .expect("bootstrap transport runtime was initialized")
                    .control
                    .attest_diagnostic(
                        &flight,
                        &diagnostic,
                        BootstrapTransportRuntime::dropout(&policy),
                    )
            });
            if !attestation.valid {
                if active.phase == AutotuneCapturePhase::LoadedMeasurement
                    && self.loaded_transport_diagnostic_budget > 0
                {
                    self.loaded_transport_diagnostic_budget -= 1;
                    if let Some(diagnostic) =
                        invalid_loaded_transport_settlement_diagnostic(attestation, expected_phase)
                    {
                        eprintln!("bootstrap loaded transport settlement rejected: {diagnostic}");
                    }
                }
                continue;
            }
            if let Some(failure) = deadline_failure {
                // An idle timeout cannot establish the exact baseline needed
                // to interpret later loaded lower bounds.
                if active.phase == AutotuneCapturePhase::IdleBaseline {
                    continue;
                }
                let deadline_us = failure.deadline_us().ok_or_else(|| {
                    (
                        "capture-transport-invalid",
                        "typed transport deadline has no duration".to_string(),
                    )
                })?;
                let kind = super::autotune_capture::transport_deadline_observation_kind(
                    &active,
                    deadline_us,
                    expected_phase.0,
                    expected_phase.1,
                )
                .map_err(|error| ("capture-transport-invalid", error))?;
                if let Some(kind) = kind {
                    let _ = self
                        .session
                        .observe(&active, kind, boot_ms)
                        .map_err(|error| ("capture-observation-invalid", error))?;
                }
                continue;
            }
            let sample = sample.expect("successful transport result was classified above");
            let samples = if sample.raw_samples_ms.is_empty() {
                vec![sample.rtt_ms]
            } else {
                sample.raw_samples_ms
            };
            if active.phase == AutotuneCapturePhase::IdleBaseline {
                self.transport
                    .as_mut()
                    .expect("bootstrap transport runtime was initialized")
                    .observe_idle_samples(&samples)
                    .map_err(|error| ("capture-transport-invalid", error))?;
                self.idle_transport_baseline_ms = self
                    .transport
                    .as_ref()
                    .expect("bootstrap transport runtime was initialized")
                    .idle_baseline_ms;
            }
            let mut latest = None;
            for latency_ms in samples {
                if !self.session.accepting_observations() {
                    break;
                }
                let kind = super::autotune_capture::transport_observation_kind(
                    &active,
                    latency_ms,
                    expected_phase.0,
                    expected_phase.1,
                )
                .map_err(|error| ("capture-transport-invalid", error))?;
                let Some(kind) = kind else {
                    continue;
                };
                latest = Some(
                    self.session
                        .observe(&active, kind, boot_ms)
                        .map_err(|error| ("capture-observation-invalid", error))?,
                );
            }
            if let Some(snapshot) = latest {
                if active.phase == AutotuneCapturePhase::IdleBaseline
                    && snapshot.state == super::full_autotune::AutotuneCaptureState::Complete
                {
                    self.idle_icmp_baseline_ms =
                        snapshot.idle_median_us.map(|value| value as f64 / 1_000.0);
                    self.persist_completed_idle_baseline(runtime_dir, &snapshot)?;
                }
                Self::publish_if_changed(runtime_dir, &snapshot)
                    .map_err(|error| ("capture-publication-unavailable", error))?;
            }
        }

        if allow_schedule
            && self.session.active_request().is_some_and(|request| {
                request.phase == AutotuneCapturePhase::IdleBaseline
                    || self.loaded_counter_cycle == LoadedCounterCycleState::FlightReady
            })
            && self.session.accepting_observations()
            && self.pending_transport_completion.is_none()
        {
            // The completed flight is settled before another is admitted.
            // Loaded scheduling is event-driven by the next fresh counter
            // authority; idle scheduling retains its ordinary cadence.
            self.try_schedule_transport(runtime_dir, store, actuator)?;
        }
        Ok(())
    }

    fn clear(&mut self, runtime_dir: &Path) -> Result<(), String> {
        self.stop_probes();
        self.session.clear();
        self.idle_rate_reference = None;
        let snapshot_path = runtime_dir.join(CAPTURE_SNAPSHOT_FILE);
        match fs::remove_file(&snapshot_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!(
                "unable to remove bootstrap Auto-Tune capture snapshot: {error}"
            )),
        }
    }

    /// The worker owns the private capture request lifetime.  Its exact
    /// removal is the positive end-of-control event, including worker failure
    /// followed by exact runtime restoration.  Observe that event before the
    /// counter reactor touches durable authority so a normal terminal cleanup
    /// is never rewritten into a capture rejection.
    fn clear_if_private_request_removed(&mut self, runtime_dir: &Path) -> Result<bool, String> {
        if self.session.active_request().is_none() {
            return Ok(false);
        }
        match fs::symlink_metadata(runtime_dir.join(CAPTURE_REQUEST_FILE)) {
            Ok(_) => Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.clear(runtime_dir)?;
                Ok(true)
            }
            Err(error) => Err(format!(
                "unable to inspect bootstrap Auto-Tune capture request: {error}"
            )),
        }
    }

    fn publish_if_changed(
        runtime_dir: &Path,
        snapshot: &AutotuneCaptureSnapshot,
    ) -> Result<(), String> {
        let path = runtime_dir.join(CAPTURE_SNAPSHOT_FILE);
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                let existing = read_capture_snapshot(&path)?;
                if &existing == snapshot {
                    return Ok(());
                }
                let same_request = existing.request == snapshot.request;
                if !capture_snapshot_matches_or_immediately_precedes_request(
                    &existing,
                    &snapshot.request,
                    snapshot.updated_boot_ms,
                ) {
                    return Err(
                        "bootstrap capture snapshot belongs to a different request".to_string()
                    );
                }
                if same_request
                    && existing.state != super::full_autotune::AutotuneCaptureState::Collecting
                {
                    return Err(
                        "bootstrap capture snapshot cannot rewrite a terminal result".to_string(),
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "unable to inspect bootstrap Auto-Tune capture snapshot: {error}"
                ))
            }
        }
        publish_capture_snapshot(&path, snapshot)
    }

    fn classify_loaded_icmp_sample(
        &self,
        request: &AutotuneCaptureRequest,
        pending: &PendingIcmpSample,
        now: Instant,
    ) -> Result<PendingIcmpDisposition, String> {
        let (download_loaded, upload_loaded) = match loaded_phase_at_with_pending_bound(
            request,
            &self.loaded_phase_windows,
            pending.observed_at,
            now,
        ) {
            LoadedPhaseLookup::Pending => return Ok(PendingIcmpDisposition::Wait),
            LoadedPhaseLookup::Uncovered => return Ok(PendingIcmpDisposition::Ignore),
            LoadedPhaseLookup::Covered(phase) => phase,
        };
        let Some(baseline_ms) = self.idle_icmp_baseline_ms else {
            return Ok(PendingIcmpDisposition::Ignore);
        };
        let delta_us = (pending.sample.rtt_ms - baseline_ms).max(0.0) * 1_000.0;
        let Some(kind) = super::autotune_capture::icmp_observation_kind(
            request,
            pending.sample.rtt_ms,
            delta_us,
            delta_us,
            download_loaded,
            upload_loaded,
        )?
        else {
            return Ok(PendingIcmpDisposition::Ignore);
        };
        Ok(PendingIcmpDisposition::Observe(kind))
    }

    fn classify_loaded_icmp_samples<I>(
        &mut self,
        request: &AutotuneCaptureRequest,
        new_samples: I,
        now: Instant,
    ) -> Result<Vec<super::autotune_capture::AutotuneCaptureObservationKind>, String>
    where
        I: IntoIterator<Item = PendingIcmpSample>,
    {
        let mut candidates = std::mem::take(&mut self.pending_icmp_samples);
        candidates.extend(new_samples);
        let mut pending = VecDeque::with_capacity(MAX_PENDING_ICMP_SAMPLES);
        let mut observations = Vec::new();
        while let Some(sample) = candidates.pop_front() {
            match self.classify_loaded_icmp_sample(request, &sample, now)? {
                PendingIcmpDisposition::Wait => pending.push_back(sample),
                PendingIcmpDisposition::Ignore => {}
                PendingIcmpDisposition::Observe(kind) => observations.push(kind),
            }
        }
        if pending.len() > MAX_PENDING_ICMP_SAMPLES {
            return Err("bootstrap loaded ICMP temporal-join queue exceeded its bound".to_string());
        }
        self.pending_icmp_samples = pending;
        Ok(observations)
    }

    fn rearm_pinger_wake(pinger: &crate::PingerRuntime) -> Result<(), String> {
        let Some(wake) = pinger.wake.as_ref() else {
            return Ok(());
        };
        let value = 1_u64;
        loop {
            let written = unsafe {
                libc::write(
                    wake.as_raw_fd(),
                    (&value as *const u64).cast::<libc::c_void>(),
                    std::mem::size_of::<u64>(),
                )
            };
            if written == std::mem::size_of::<u64>() as isize {
                return Ok(());
            }
            if written < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(format!("failed to re-arm bootstrap pinger wake: {error}"));
            }
            return Err("bootstrap pinger wake descriptor returned a short write".to_string());
        }
    }

    fn read_bounded_pinger_lines(
        pinger: &crate::PingerRuntime,
    ) -> Result<Vec<crate::PingerLine>, String> {
        let mut lines = Vec::with_capacity(MAX_PINGER_LINES_PER_DRAIN);
        while lines.len() < MAX_PINGER_LINES_PER_DRAIN {
            match pinger.lines.try_recv() {
                Ok(value) => lines.push(value?),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err("bootstrap pinger output closed unexpectedly".to_string())
                }
            }
        }
        if lines.len() == MAX_PINGER_LINES_PER_DRAIN {
            // eventfd reads consume the aggregate counter.  Re-arm after a
            // full slice so any unread channel entries get another event-loop
            // turn without relying on a sampling timer or a future ping.
            Self::rearm_pinger_wake(pinger)?;
        }
        Ok(lines)
    }

    /// Drain the shared pinger channel during a bounded counter burst without
    /// performing capture mutation or synchronous runtime attestation.  The
    /// timestamped samples remain pending until physical counter windows can
    /// classify them in the normal fairness turn.
    fn collect_loaded_pinger_during_burst(&mut self) -> Result<(), String> {
        if !self.loaded_counter_burst.active()
            || !self.session.accepting_observations()
            || !self
                .session
                .active_request()
                .is_some_and(|request| request.phase == AutotuneCapturePhase::LoadedMeasurement)
        {
            return Ok(());
        }
        let lines = {
            let Some(pinger) = self.pinger.as_ref() else {
                return Ok(());
            };
            Self::read_bounded_pinger_lines(pinger)?
        };
        for line in lines {
            let Some(sample) = crate::parse_fping_line(&line.line) else {
                continue;
            };
            if crate::sample_is_stale(&sample, line.observed_epoch_secs) {
                continue;
            }
            if self.pending_icmp_samples.len() >= MAX_PENDING_ICMP_SAMPLES {
                return Err(
                    "bootstrap loaded ICMP temporal-join queue exceeded its bound".to_string(),
                );
            }
            self.pending_icmp_samples.push_back(PendingIcmpSample {
                sample,
                observed_at: line.observed_at,
            });
        }
        Ok(())
    }

    fn drain_pinger(
        &mut self,
        runtime_dir: &Path,
        store: &RuntimeOverrideStore,
        actuator: &mut OpenWrtBootstrapRuntimeActuator,
    ) -> Result<(), String> {
        let lines = {
            let Some(pinger) = self.pinger.as_ref() else {
                return Ok(());
            };
            Self::read_bounded_pinger_lines(pinger)?
        };
        let Some(request) = self.session.active_request().cloned() else {
            self.pending_icmp_samples.clear();
            return Ok(());
        };
        if !self.session.accepting_observations() {
            self.pending_icmp_samples.clear();
            return Ok(());
        }
        let mut observations = Vec::with_capacity(lines.len());
        let mut new_loaded_samples = VecDeque::with_capacity(lines.len());
        for line in lines {
            let Some(sample) = crate::parse_fping_line(&line.line) else {
                if crate::pinger_line_is_timeout("fping", &line.line)
                    && request.phase == AutotuneCapturePhase::IdleBaseline
                {
                    observations
                        .push(super::autotune_capture::AutotuneCaptureObservationKind::IcmpTimeout);
                }
                continue;
            };
            if crate::sample_is_stale(&sample, line.observed_epoch_secs) {
                continue;
            }
            match request.phase {
                AutotuneCapturePhase::IdleBaseline => {
                    let Some(kind) = super::autotune_capture::icmp_observation_kind(
                        &request,
                        sample.rtt_ms,
                        0.0,
                        0.0,
                        false,
                        false,
                    )?
                    else {
                        continue;
                    };
                    observations.push(kind);
                }
                AutotuneCapturePhase::LoadedMeasurement => {
                    new_loaded_samples.push_back(PendingIcmpSample {
                        sample,
                        observed_at: line.observed_at,
                    });
                }
            }
        }

        if request.phase == AutotuneCapturePhase::LoadedMeasurement {
            observations.extend(self.classify_loaded_icmp_samples(
                &request,
                new_loaded_samples,
                Instant::now(),
            )?);
        }
        if observations.is_empty() {
            return Ok(());
        }
        // Counter windows were independently attested when captured and the
        // temporal join above is pure.  The shared recorder performs the one
        // fresh attestation immediately before the batched state mutation.
        self.record_attested_observations(runtime_dir, store, actuator, &request, observations)
            .map_err(|(_, detail)| detail)?;
        Ok(())
    }

    fn admit(
        &mut self,
        runtime_dir: &Path,
        request: &AutotuneCaptureRequest,
        boot_ms: u64,
        idle_rate_reference: (u64, u64),
    ) -> Result<(), String> {
        let changed = self.session.active_request() != Some(request);
        if changed {
            if let Some(transport) = self.transport.as_mut() {
                transport.begin_request(request);
            }
            if let Some(sampler) = self.counter_sampler.as_mut() {
                sampler.invalidate_current();
                self.counter_epoch = sampler.current_epoch();
            }
            self.counter_request = None;
            if let Some(rates) = self.counter_rates.as_mut() {
                rates.reset(Instant::now());
            }
            if let Some(physical) = self.counter_physical.as_mut() {
                physical.reset(Instant::now());
            }
            self.loaded_counter_cycle = LoadedCounterCycleState::NeedAttestedBaseline;
            self.loaded_topology_attestation = None;
            self.loaded_transport_authority = None;
            self.loaded_phase_windows.clear();
            self.loaded_physical_deltas.clear();
            self.pending_transport_completion = None;
            self.loaded_ack_credit = None;
            self.loaded_phase_expired = true;
            self.pending_icmp_samples.clear();
            let now = Instant::now();
            self.next_rate_sample = now;
            self.next_cpu_sample = now;
            if request.phase == AutotuneCapturePhase::IdleBaseline {
                self.prepare_idle_request(runtime_dir, request)?;
            }
        }
        let mut snapshot = self.session.admit(request, boot_ms)?;
        self.idle_rate_reference = Some(idle_rate_reference);
        if request.phase == AutotuneCapturePhase::LoadedMeasurement {
            if changed {
                if let Err(error) = self.restore_loaded_idle_baseline(runtime_dir, request) {
                    let rejected = self
                        .session
                        .reject("capture-idle-baseline-unavailable", boot_ms)?;
                    Self::publish_if_changed(runtime_dir, &rejected)?;
                    self.stop_probes();
                    return Err(error);
                }
            }
            if let Some(updated) = self.consume_load_evidence(runtime_dir, request)? {
                snapshot = updated;
            }
        }
        Self::publish_if_changed(runtime_dir, &snapshot)
    }

    /// Consume the worker-owned controlled-load proof independently of
    /// capture admission.
    ///
    /// The worker normally publishes this file after the capture request has
    /// already been admitted.  The runtime directory is watched by the owner
    /// event loop, so publication wakes the reactor and this method observes
    /// the exact proof once.  Re-admitting the request here would repeat the
    /// expensive runtime attestation which loaded transport deliberately
    /// avoids; `AutotuneCaptureSession` instead provides the single-consumer
    /// identity fence.
    fn consume_load_evidence(
        &mut self,
        runtime_dir: &Path,
        request: &AutotuneCaptureRequest,
    ) -> Result<Option<AutotuneCaptureSnapshot>, String> {
        if request.phase != AutotuneCapturePhase::LoadedMeasurement
            || !self.session.accepting_observations()
        {
            return Ok(None);
        }
        let evidence_path = runtime_dir.join(LOAD_EVIDENCE_FILE);
        match fs::symlink_metadata(&evidence_path) {
            Ok(_) => {
                let evidence = read_controlled_load_evidence(&evidence_path)?;
                self.session
                    .observe_load_evidence(request, &evidence, monotonic_boot_ms()?)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(format!(
                "unable to inspect bootstrap Auto-Tune load evidence: {error}"
            )),
        }
    }

    fn reject(
        &mut self,
        runtime_dir: &Path,
        request: &AutotuneCaptureRequest,
        code: &str,
        boot_ms: u64,
    ) -> Result<(), String> {
        let snapshot = self
            .session
            .replace_with_rejection(request, code, boot_ms)?;
        self.idle_rate_reference = None;
        Self::publish_if_changed(runtime_dir, &snapshot)
    }

    fn reject_active(&mut self, runtime_dir: &Path, code: &str) -> Result<(), String> {
        let Some(request) = self.session.active_request().cloned() else {
            return Ok(());
        };
        self.reject(runtime_dir, &request, code, monotonic_boot_ms()?)
    }
}

impl Drop for BootstrapCaptureRuntime {
    fn drop(&mut self) {
        self.stop_probes();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BootstrapRuntimeOwnerClaim {
    pub job_id: String,
    pub worker_run_id: String,
    pub request_sha256: String,
    pub process: ProcessIdentity,
    pub baseline: super::autotune_runtime::AbsentRuntimeBaseline,
}

impl BootstrapRuntimeOwnerClaim {
    pub(crate) fn baseline(&self) -> &super::autotune_runtime::AbsentRuntimeBaseline {
        &self.baseline
    }

    fn for_current_process(
        request: &OperationRequest,
        worker_run_id: &str,
        baseline: super::autotune_runtime::AbsentRuntimeBaseline,
    ) -> Result<Self, String> {
        let process = ProcessIdentity::inspect(Path::new(DEFAULT_PROC_ROOT), std::process::id())?;
        if process.process_group != process.pid {
            return Err("bootstrap runtime owner must run in its own process group".to_string());
        }
        Self::for_process(request, worker_run_id, process, baseline)
    }

    pub(crate) fn for_process(
        request: &OperationRequest,
        worker_run_id: &str,
        process: ProcessIdentity,
        baseline: super::autotune_runtime::AbsentRuntimeBaseline,
    ) -> Result<Self, String> {
        let claim = Self {
            job_id: request.identity.job_id.clone(),
            worker_run_id: worker_run_id.to_string(),
            request_sha256: request_sha256(request)?,
            process,
            baseline,
        };
        claim.validate()?;
        validate_bootstrap_request_baseline(request, &claim.baseline)?;
        Ok(claim)
    }

    fn validate(&self) -> Result<(), String> {
        require_lower_hex("job id", &self.job_id, 32)?;
        require_lower_hex("worker run id", &self.worker_run_id, 32)?;
        require_lower_hex("request digest", &self.request_sha256, 64)?;
        if self.process.pid == 0
            || self.process.process_group != self.process.pid
            || self.process.starttime_ticks == 0
        {
            return Err("bootstrap runtime owner process identity is invalid".to_string());
        }
        self.baseline.validate()?;
        Ok(())
    }

    pub(crate) fn encode(&self) -> Result<String, String> {
        self.validate()?;
        let encoded = format!(
            "{OWNER_CLAIM_HEADER}\njob_id={}\nworker_run_id={}\nrequest_sha256={}\nprocess_pid={}\nprocess_group={}\nprocess_starttime={}\nbaseline_planned_sqm_section={}\nbaseline_target_interface={}\nbaseline_target_ifindex={}\nbaseline_route_fingerprint={}\nbaseline_config_fingerprint={}\nbaseline_sqm_fingerprint={}\nbaseline_kernel_topology_fingerprint={}\nbaseline_kernel_namespace_seed={}\n",
            self.job_id,
            self.worker_run_id,
            self.request_sha256,
            self.process.pid,
            self.process.process_group,
            self.process.starttime_ticks,
            self.baseline.planned_sqm_section,
            self.baseline.target_interface,
            self.baseline.target_ifindex,
            self.baseline.route_fingerprint,
            self.baseline.config_fingerprint,
            self.baseline.sqm_fingerprint,
            self.baseline.kernel_topology_fingerprint,
            self.baseline.kernel_namespace_seed,
        );
        if encoded.len() > MAX_OPERATION_RECORD_BYTES {
            return Err("bootstrap runtime owner claim exceeds its size bound".to_string());
        }
        Ok(encoded)
    }

    fn decode(input: &str) -> Result<Self, String> {
        if input.len() > MAX_OPERATION_RECORD_BYTES || !input.ends_with('\n') {
            return Err("bootstrap runtime owner claim is not bounded and terminated".to_string());
        }
        let mut lines = input.lines();
        if lines.next() != Some(OWNER_CLAIM_HEADER) {
            return Err("bootstrap runtime owner claim header is unsupported".to_string());
        }
        let job_id = claim_field(&mut lines, "job_id")?;
        let worker_run_id = claim_field(&mut lines, "worker_run_id")?;
        let request_sha256 = claim_field(&mut lines, "request_sha256")?;
        let process = ProcessIdentity {
            pid: claim_field(&mut lines, "process_pid")?
                .parse::<u32>()
                .map_err(|_| "bootstrap runtime owner pid is invalid".to_string())?,
            process_group: claim_field(&mut lines, "process_group")?
                .parse::<u32>()
                .map_err(|_| "bootstrap runtime owner process group is invalid".to_string())?,
            starttime_ticks: claim_field(&mut lines, "process_starttime")?
                .parse::<u64>()
                .map_err(|_| "bootstrap runtime owner start time is invalid".to_string())?,
        };
        let baseline = super::autotune_runtime::AbsentRuntimeBaseline {
            planned_sqm_section: claim_field(&mut lines, "baseline_planned_sqm_section")?,
            target_interface: claim_field(&mut lines, "baseline_target_interface")?,
            target_ifindex: claim_field(&mut lines, "baseline_target_ifindex")?
                .parse::<u32>()
                .map_err(|_| {
                    "bootstrap runtime owner baseline target ifindex is invalid".to_string()
                })?,
            route_fingerprint: claim_field(&mut lines, "baseline_route_fingerprint")?,
            config_fingerprint: claim_field(&mut lines, "baseline_config_fingerprint")?,
            sqm_fingerprint: claim_field(&mut lines, "baseline_sqm_fingerprint")?,
            kernel_topology_fingerprint: claim_field(
                &mut lines,
                "baseline_kernel_topology_fingerprint",
            )?,
            kernel_namespace_seed: claim_field(&mut lines, "baseline_kernel_namespace_seed")?,
        };
        if lines.next().is_some() {
            return Err("bootstrap runtime owner claim contains unknown fields".to_string());
        }
        let claim = Self {
            job_id,
            worker_run_id,
            request_sha256,
            process,
            baseline,
        };
        claim.validate()?;
        if claim.encode()? != input {
            return Err("bootstrap runtime owner claim is not canonical".to_string());
        }
        Ok(claim)
    }

    pub(crate) fn attest(
        &self,
        request: &OperationRequest,
        worker_run_id: &str,
    ) -> Result<(), String> {
        self.validate()?;
        if self.job_id != request.identity.job_id
            || self.worker_run_id != worker_run_id
            || self.request_sha256 != request_sha256(request)?
        {
            return Err("bootstrap runtime owner claim authority changed".to_string());
        }
        validate_bootstrap_request_baseline(request, &self.baseline)?;
        Ok(())
    }
}

struct BootstrapRuntimeOwnerLock {
    _file: File,
}

impl BootstrapRuntimeOwnerLock {
    fn acquire(runtime_dir: &Path, terminate: &AtomicBool) -> Result<Self, String> {
        let path = runtime_dir.join(OWNER_LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|error| format!("unable to open bootstrap runtime owner lock: {error}"))?;
        let metadata = file
            .metadata()
            .map_err(|error| format!("unable to inspect bootstrap runtime owner lock: {error}"))?;
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o777 != 0o600
            || metadata.nlink() != 1
        {
            return Err("bootstrap runtime owner lock is unsafe".to_string());
        }
        loop {
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
                return Ok(Self { _file: file });
            }
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                if terminate.load(Ordering::SeqCst) {
                    return Err("bootstrap runtime owner lock wait was cancelled".to_string());
                }
                continue;
            }
            return Err(format!(
                "unable to acquire bootstrap runtime owner lock: {error}"
            ));
        }
    }
}

fn request_sha256(request: &OperationRequest) -> Result<String, String> {
    let bytes = request.encode()?;
    Ok(digest(&SHA256, bytes.as_bytes())
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn require_lower_hex(name: &str, value: &str, length: usize) -> Result<(), String> {
    if value.len() != length
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!(
            "bootstrap runtime owner {name} must be {length} lowercase hex bytes"
        ));
    }
    Ok(())
}

fn claim_field<'a>(
    lines: &mut impl Iterator<Item = &'a str>,
    expected: &str,
) -> Result<String, String> {
    let line = lines
        .next()
        .ok_or_else(|| format!("bootstrap runtime owner claim is missing {expected}"))?;
    let (name, value) = line
        .split_once('=')
        .ok_or_else(|| format!("bootstrap runtime owner claim field {expected} is malformed"))?;
    if name != expected || value.is_empty() || value.contains(['\r', '\n', '=']) {
        return Err(format!(
            "bootstrap runtime owner claim expected field {expected}"
        ));
    }
    Ok(value.to_string())
}

pub(crate) fn read_bootstrap_runtime_owner_claim(
    runtime_dir: &Path,
    request: &OperationRequest,
    worker_run_id: &str,
) -> Result<Option<BootstrapRuntimeOwnerClaim>, String> {
    let path = runtime_dir.join(OWNER_CLAIM_FILE);
    match fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "unable to inspect bootstrap runtime owner claim: {error}"
            ))
        }
    }
    let contents = super::rating::read_private_bounded(&path, MAX_OPERATION_RECORD_BYTES)?;
    let claim = BootstrapRuntimeOwnerClaim::decode(&contents)?;
    claim.attest(request, worker_run_id)?;
    Ok(Some(claim))
}

fn validate_loaded_transport_durable_authority(
    operation: &OperationRequest,
    request: &AutotuneCaptureRequest,
    permit: &AutotuneRuntimePermit,
    control: &AutotuneRuntimeControl,
    ack: &AutotuneRuntimeAck,
    checkpoint: &RuntimeOverrideCheckpoint,
    restore_intent: Option<&AutotuneRuntimeControl>,
    current_boot_ms: u64,
) -> Result<(), String> {
    if request.phase != AutotuneCapturePhase::LoadedMeasurement {
        return Err("loaded transport authority requires a loaded capture".to_string());
    }
    if restore_intent.is_some() {
        return Err("loaded transport authority is revoked by runtime restoration".to_string());
    }
    validate_runtime_permit_admission(
        permit,
        operation,
        &permit.worker_run_id,
        &permit.worker,
        &permit.boot_id,
    )?;
    attest_capture_admission(request, permit, Some(control), Some(ack), current_boot_ms)?;
    checkpoint.validate_against(permit)?;
    if checkpoint.temporary_stage != TemporaryTopologyStage::Active {
        return Err("loaded transport authority requires an active private topology".to_string());
    }
    match &checkpoint.baseline {
        RuntimeBaseline::Absent(absent) => validate_bootstrap_request_baseline(operation, absent)?,
        RuntimeBaseline::Managed(_) => {
            return Err("loaded bootstrap transport authority has a managed baseline".to_string())
        }
    }
    Ok(())
}

pub(crate) struct OpenWrtBootstrapRuntimeActuator {
    request: OperationRequest,
    backend: OpenWrtNativeApplyBackend,
    capture_probes_stopped: Arc<AtomicBool>,
}

impl OpenWrtBootstrapRuntimeActuator {
    fn new(
        request: OperationRequest,
        capture_probes_stopped: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        request.validate()?;
        if request.target_state != OperationTargetState::AbsentBootstrap {
            return Err("bootstrap runtime owner requires an absent target request".to_string());
        }
        Ok(Self {
            request,
            backend: OpenWrtNativeApplyBackend,
            capture_probes_stopped,
        })
    }

    fn absent_permit_baseline<'a>(
        &self,
        permit: &'a AutotuneRuntimePermit,
    ) -> Result<&'a super::autotune_runtime::AbsentRuntimeBaseline, String> {
        validate_runtime_permit_admission(
            permit,
            &self.request,
            &permit.worker_run_id,
            &permit.worker,
            &permit.boot_id,
        )?;
        match &permit.baseline {
            RuntimeBaseline::Absent(absent) => Ok(absent),
            RuntimeBaseline::Managed(_) => {
                Err("bootstrap runtime owner received a managed baseline".to_string())
            }
        }
    }

    fn checkpoint_absent_baseline<'a>(
        &self,
        checkpoint: &'a RuntimeOverrideCheckpoint,
    ) -> Result<&'a super::autotune_runtime::AbsentRuntimeBaseline, String> {
        match &checkpoint.baseline {
            RuntimeBaseline::Absent(absent) => Ok(absent),
            RuntimeBaseline::Managed(_) => {
                Err("bootstrap runtime checkpoint carries a managed baseline".to_string())
            }
        }
    }

    fn attest_target_ifindex(
        &self,
        baseline: &super::autotune_runtime::AbsentRuntimeBaseline,
    ) -> Result<(), String> {
        let ifindex = fs::read_to_string(
            crate::sqm_sys_class_net()
                .join(&baseline.target_interface)
                .join("ifindex"),
        )
        .map_err(|error| format!("unable to read bootstrap target ifindex: {error}"))?
        .trim()
        .parse::<u32>()
        .map_err(|_| "bootstrap target ifindex is invalid".to_string())?;
        if ifindex != baseline.target_ifindex {
            return Err("bootstrap target ifindex changed".to_string());
        }
        Ok(())
    }

    fn attest_identity_while_owned(
        &self,
        baseline: &super::autotune_runtime::AbsentRuntimeBaseline,
    ) -> Result<(), String> {
        self.backend
            .attest_bootstrap_runtime_identity(&self.request, baseline)?;
        self.attest_target_ifindex(baseline)
    }

    fn attest_absent(
        &self,
        baseline: &super::autotune_runtime::AbsentRuntimeBaseline,
    ) -> Result<(), String> {
        self.backend
            .attest_bootstrap_runtime_absence(&self.request, baseline)?;
        self.attest_target_ifindex(baseline)
    }

    fn attest_capture(
        &mut self,
        store: &RuntimeOverrideStore,
        request: &AutotuneCaptureRequest,
        current_boot_ms: u64,
    ) -> Result<(u64, u64), (&'static str, String)> {
        if request.instance_name != self.request.identity.instance {
            return Err((
                "capture-instance-mismatch",
                "bootstrap Auto-Tune capture belongs to another instance".to_string(),
            ));
        }
        let permit = store
            .read_permit()
            .map_err(|error| ("capture-runtime-unavailable", error))?
            .ok_or_else(|| {
                (
                    "capture-permit-missing",
                    "bootstrap Auto-Tune capture has no runtime permit".to_string(),
                )
            })?;
        validate_runtime_permit_admission(
            &permit,
            &self.request,
            &permit.worker_run_id,
            &permit.worker,
            &permit.boot_id,
        )
        .map_err(|error| ("capture-runtime-mismatch", error))?;
        let control = store
            .read_control()
            .map_err(|error| ("capture-runtime-unavailable", error))?;
        let ack = store
            .read_ack()
            .map_err(|error| ("capture-runtime-unavailable", error))?;
        attest_capture_admission(
            request,
            &permit,
            control.as_ref(),
            ack.as_ref(),
            current_boot_ms,
        )
        .map_err(|error| ("capture-runtime-mismatch", error))?;
        if !permit
            .worker
            .still_matches(Path::new(DEFAULT_PROC_ROOT))
            .map_err(|error| ("capture-worker-unavailable", error))?
        {
            return Err((
                "capture-worker-unavailable",
                "bootstrap Auto-Tune capture worker identity is no longer live".to_string(),
            ));
        }
        let baseline = self
            .absent_permit_baseline(&permit)
            .map_err(|error| ("capture-runtime-mismatch", error))?;
        let control = control.as_ref();
        let expected = match request.phase {
            AutotuneCapturePhase::IdleBaseline => None,
            AutotuneCapturePhase::LoadedMeasurement => Some(runtime_snapshot_for_control(
                &permit,
                control.ok_or_else(|| {
                    (
                        "capture-runtime-mismatch",
                        "bootstrap loaded capture lost its applied runtime control".to_string(),
                    )
                })?,
            )),
        };
        match request.phase {
            AutotuneCapturePhase::IdleBaseline => {
                if store
                    .read_checkpoint()
                    .map_err(|error| ("capture-runtime-unavailable", error))?
                    .is_some()
                {
                    return Err((
                        "capture-runtime-mismatch",
                        "bootstrap idle capture observed a private runtime checkpoint".to_string(),
                    ));
                }
                self.attest_absent(baseline)
                    .map_err(|error| ("capture-runtime-mismatch", error))?;
            }
            AutotuneCapturePhase::LoadedMeasurement => {
                let expected = expected.as_ref().ok_or_else(|| {
                    (
                        "capture-runtime-mismatch",
                        "bootstrap loaded capture has no canonical runtime snapshot".to_string(),
                    )
                })?;
                let checkpoint = store
                    .read_checkpoint()
                    .map_err(|error| ("capture-runtime-unavailable", error))?
                    .ok_or_else(|| {
                        (
                            "capture-runtime-mismatch",
                            "bootstrap loaded capture has no private runtime checkpoint"
                                .to_string(),
                        )
                    })?;
                let actual = self
                    .attest_runtime(expected, &checkpoint)
                    .map_err(|error| ("capture-runtime-mismatch", error.to_string()))?;
                if actual != *expected {
                    return Err((
                        "capture-runtime-mismatch",
                        "bootstrap capture runtime differs from its requested topology".to_string(),
                    ));
                }
            }
        }
        Ok((permit.initial_download_kbps, permit.initial_upload_kbps))
    }

    /// Re-check only the durable authority which can revoke or replace the
    /// current post-attested/rebased controlled-counter cycle.
    ///
    /// Full kernel topology attestation occurs before the first baseline and
    /// after every evidence/post-flight endpoint. Re-running it inside the
    /// clean baseline-to-endpoint interval would spend the transport
    /// freshness budget under a saturated uplink. Kernel mutation is
    /// serialized in this owner, so between those boundaries the legitimate
    /// revocations are changed durable request/control or a restore intent.
    /// Those records are all re-read and matched here without mutating state.
    fn attest_loaded_transport_durable_authority(
        &self,
        runtime_dir: &Path,
        store: &RuntimeOverrideStore,
        request: &AutotuneCaptureRequest,
        current_boot_ms: u64,
    ) -> CaptureRuntimeResult {
        let request_path = runtime_dir.join(CAPTURE_REQUEST_FILE);
        let active_request = match read_capture_request(&request_path) {
            Ok(request) => request,
            Err(error) => match fs::symlink_metadata(&request_path) {
                Err(metadata_error) if metadata_error.kind() == std::io::ErrorKind::NotFound => {
                    return Err((
                        "capture-ended",
                        "bootstrap loaded transport request was removed by its worker".to_string(),
                    ))
                }
                _ => return Err(("capture-runtime-unavailable", error)),
            },
        };
        if active_request != *request {
            return Err((
                "capture-runtime-mismatch",
                "bootstrap loaded transport request changed before dispatch".to_string(),
            ));
        }
        let snapshot = read_capture_snapshot(&runtime_dir.join(CAPTURE_SNAPSHOT_FILE))
            .map_err(|error| ("capture-runtime-unavailable", error))?;
        if snapshot.request != *request
            || snapshot.state != super::full_autotune::AutotuneCaptureState::Collecting
        {
            return Err((
                "capture-runtime-mismatch",
                "bootstrap loaded transport snapshot is no longer collecting".to_string(),
            ));
        }
        let permit = store
            .read_permit()
            .map_err(|error| ("capture-runtime-unavailable", error))?
            .ok_or_else(|| {
                (
                    "capture-permit-missing",
                    "bootstrap loaded transport has no runtime permit".to_string(),
                )
            })?;
        let control = store
            .read_control()
            .map_err(|error| ("capture-runtime-unavailable", error))?
            .ok_or_else(|| {
                (
                    "capture-runtime-mismatch",
                    "bootstrap loaded transport has no runtime control".to_string(),
                )
            })?;
        let ack = store
            .read_ack()
            .map_err(|error| ("capture-runtime-unavailable", error))?
            .ok_or_else(|| {
                (
                    "capture-runtime-mismatch",
                    "bootstrap loaded transport has no applied runtime ACK".to_string(),
                )
            })?;
        let checkpoint = store
            .read_checkpoint()
            .map_err(|error| ("capture-runtime-unavailable", error))?
            .ok_or_else(|| {
                (
                    "capture-runtime-mismatch",
                    "bootstrap loaded transport has no runtime checkpoint".to_string(),
                )
            })?;
        let restore_intent = store
            .read_restore_intent()
            .map_err(|error| ("capture-runtime-unavailable", error))?;
        validate_loaded_transport_durable_authority(
            &self.request,
            request,
            &permit,
            &control,
            &ack,
            &checkpoint,
            restore_intent.as_ref(),
            current_boot_ms,
        )
        .map_err(|error| ("capture-runtime-mismatch", error))?;
        if !permit
            .worker
            .still_matches(Path::new(DEFAULT_PROC_ROOT))
            .map_err(|error| ("capture-worker-unavailable", error))?
        {
            return Err((
                "capture-worker-unavailable",
                "bootstrap loaded transport worker identity is no longer live".to_string(),
            ));
        }
        Ok(())
    }
}

impl BootstrapCaptureAuthority for OpenWrtBootstrapRuntimeActuator {
    fn attest_capture(
        &mut self,
        store: &RuntimeOverrideStore,
        request: &AutotuneCaptureRequest,
        current_boot_ms: u64,
    ) -> Result<(u64, u64), (&'static str, String)> {
        OpenWrtBootstrapRuntimeActuator::attest_capture(self, store, request, current_boot_ms)
    }

    fn attest_loaded_transport_durable_authority(
        &mut self,
        runtime_dir: &Path,
        store: &RuntimeOverrideStore,
        request: &AutotuneCaptureRequest,
        current_boot_ms: u64,
    ) -> CaptureRuntimeResult {
        OpenWrtBootstrapRuntimeActuator::attest_loaded_transport_durable_authority(
            self,
            runtime_dir,
            store,
            request,
            current_boot_ms,
        )
    }
}

impl RuntimeOverrideActuator for OpenWrtBootstrapRuntimeActuator {
    fn current_boot_ms(&self) -> u64 {
        monotonic_boot_ms().unwrap_or(0)
    }

    fn capture_baseline(
        &mut self,
        permit: &AutotuneRuntimePermit,
    ) -> Result<RuntimeRestoreBaseline, String> {
        let baseline = self.absent_permit_baseline(permit)?;
        self.attest_absent(baseline)?;
        Ok(RuntimeBaseline::Absent(baseline.clone()))
    }

    fn current_route_identity(&mut self) -> Result<String, String> {
        super::runtime::attest_openwrt_route_identity(&self.request)
            .map_err(|(_, message, _)| message)?;
        super::coordinator::runtime_route_identity(&self.request)
    }

    fn baseline_ready_for_apply(&mut self, permit: &AutotuneRuntimePermit) -> Result<bool, String> {
        let baseline = self.absent_permit_baseline(permit)?;
        self.attest_absent(baseline).map(|()| true)
    }

    fn active_identity_matches(&mut self, permit: &AutotuneRuntimePermit) -> Result<bool, String> {
        let baseline = self.absent_permit_baseline(permit)?;
        self.attest_identity_while_owned(baseline).map(|()| true)
    }

    fn worker_identity_matches(&self, worker: &ProcessIdentity) -> bool {
        worker
            .still_matches(Path::new(DEFAULT_PROC_ROOT))
            .unwrap_or(false)
    }

    fn runtime_matches(
        &mut self,
        expected: &RuntimeSnapshot,
        checkpoint: &RuntimeOverrideCheckpoint,
    ) -> Result<bool, String> {
        let baseline = self.checkpoint_absent_baseline(checkpoint)?;
        self.attest_identity_while_owned(baseline)?;
        Ok(
            crate::attest_private_runtime(&baseline.target_interface, expected, checkpoint)?
                == *expected,
        )
    }

    fn prepare_baseline(
        &mut self,
        checkpoint: &RuntimeOverrideCheckpoint,
    ) -> Result<(), RuntimeActuatorError> {
        if checkpoint.temporary_stage != TemporaryTopologyStage::Planned {
            return Err(RuntimeActuatorError::Unsafe(
                "bootstrap absence preparation has the wrong durable stage".to_string(),
            ));
        }
        let baseline = self
            .checkpoint_absent_baseline(checkpoint)
            .map_err(RuntimeActuatorError::Unsafe)?;
        self.attest_absent(baseline)
            .map_err(RuntimeActuatorError::Unsafe)
    }

    fn create_temporary_ifb(
        &mut self,
        checkpoint: &RuntimeOverrideCheckpoint,
    ) -> Result<u32, RuntimeActuatorError> {
        if checkpoint.temporary_stage != TemporaryTopologyStage::AbsenceAttested {
            return Err(RuntimeActuatorError::Unsafe(
                "bootstrap IFB creation skipped the durable absence attestation".to_string(),
            ));
        }
        let baseline = self
            .checkpoint_absent_baseline(checkpoint)
            .map_err(RuntimeActuatorError::Unsafe)?;
        self.attest_identity_while_owned(baseline)
            .map_err(RuntimeActuatorError::Unsafe)?;
        crate::create_private_ifb(checkpoint).map_err(RuntimeActuatorError::Unsafe)
    }

    fn apply_override(
        &mut self,
        control: &AutotuneRuntimeControl,
        expected: &RuntimeSnapshot,
        checkpoint: &RuntimeOverrideCheckpoint,
    ) -> Result<(), RuntimeActuatorError> {
        let baseline = self
            .checkpoint_absent_baseline(checkpoint)
            .map_err(RuntimeActuatorError::Unsafe)?;
        self.attest_identity_while_owned(baseline)
            .map_err(RuntimeActuatorError::Unsafe)?;
        crate::apply_private_runtime_topology(&baseline.target_interface, control, checkpoint)
            .map_err(RuntimeActuatorError::Unsafe)?;
        let actual =
            crate::attest_private_runtime(&baseline.target_interface, expected, checkpoint)
                .map_err(RuntimeActuatorError::Unsafe)?;
        if actual != *expected {
            return Err(RuntimeActuatorError::Unsafe(
                "bootstrap private topology postcondition failed".to_string(),
            ));
        }
        Ok(())
    }

    fn remove_temporary_topology(
        &mut self,
        checkpoint: &RuntimeOverrideCheckpoint,
    ) -> Result<(), RuntimeActuatorError> {
        if !self.capture_probes_stopped.load(Ordering::Acquire) {
            return Err(RuntimeActuatorError::Blocked(
                RuntimeRestoreBlocker::TopologySettling {
                    target_interface: self.request.identity.target_interface.clone(),
                    detail: PROBE_STOPPING_DETAIL.to_string(),
                },
            ));
        }
        let baseline = self
            .checkpoint_absent_baseline(checkpoint)
            .map_err(RuntimeActuatorError::Unsafe)?;
        self.attest_identity_while_owned(baseline)
            .map_err(RuntimeActuatorError::Unsafe)?;
        crate::remove_private_runtime_topology(&baseline.target_interface, checkpoint)
            .map_err(RuntimeActuatorError::Unsafe)?;
        self.attest_absent(baseline)
            .map_err(RuntimeActuatorError::Unsafe)
    }

    fn restore_baseline(
        &mut self,
        baseline: &RuntimeRestoreBaseline,
    ) -> Result<(), RuntimeActuatorError> {
        if !self.capture_probes_stopped.load(Ordering::Acquire) {
            return Err(RuntimeActuatorError::Unsafe(
                "bootstrap capture probes must stop before baseline restoration".to_string(),
            ));
        }
        let RuntimeBaseline::Absent(absent) = baseline else {
            return Err(RuntimeActuatorError::Unsafe(
                "bootstrap runtime owner cannot restore a managed baseline".to_string(),
            ));
        };
        self.attest_absent(absent)
            .map_err(RuntimeActuatorError::Unsafe)
    }

    fn observe_restore_blocker(
        &mut self,
        blocker: &RuntimeRestoreBlocker,
        baseline: &RuntimeRestoreBaseline,
    ) -> Result<RuntimeRestoreObservation, String> {
        if let Some(observation) = observe_probe_stopping(
            blocker,
            &self.request.identity.target_interface,
            self.capture_probes_stopped.load(Ordering::Acquire),
        ) {
            return Ok(observation);
        }
        let RuntimeBaseline::Absent(absent) = baseline else {
            return Err("bootstrap restore blocker carries a managed baseline".to_string());
        };
        self.attest_absent(absent)?;
        Ok(RuntimeRestoreObservation::Ready)
    }

    fn attest_runtime(
        &mut self,
        expected: &RuntimeSnapshot,
        checkpoint: &RuntimeOverrideCheckpoint,
    ) -> Result<RuntimeSnapshot, RuntimeActuatorError> {
        let baseline = self
            .checkpoint_absent_baseline(checkpoint)
            .map_err(RuntimeActuatorError::Unsafe)?;
        self.attest_identity_while_owned(baseline)
            .map_err(RuntimeActuatorError::Unsafe)?;
        crate::attest_private_runtime(&baseline.target_interface, expected, checkpoint)
            .map_err(RuntimeActuatorError::Unsafe)
    }

    fn attest_restored_baseline(
        &mut self,
        baseline: &RuntimeRestoreBaseline,
        checkpoint: &RuntimeOverrideCheckpoint,
    ) -> Result<RuntimeRestoreBaseline, RuntimeActuatorError> {
        if checkpoint.temporary_stage != TemporaryTopologyStage::BaselineRestored {
            return Err(RuntimeActuatorError::Unsafe(
                "bootstrap absence cannot be attested before baseline restoration".to_string(),
            ));
        }
        let RuntimeBaseline::Absent(absent) = baseline else {
            return Err(RuntimeActuatorError::Unsafe(
                "bootstrap restored baseline is unexpectedly managed".to_string(),
            ));
        };
        self.attest_absent(absent)
            .map_err(RuntimeActuatorError::Unsafe)?;
        Ok(RuntimeBaseline::Absent(absent.clone()))
    }

    fn finish_restored(&mut self) {}

    fn recover_safe_configuration(&mut self) -> Result<(), String> {
        Err(
            "bootstrap runtime recovery requires the exact readable ownership checkpoint"
                .to_string(),
        )
    }
}

fn ensure_runtime_directory(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder.create(path).map_err(|error| {
                format!("unable to create bootstrap runtime directory: {error}")
            })?;
        }
        Err(error) => {
            return Err(format!(
                "unable to inspect bootstrap runtime directory: {error}"
            ))
        }
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("unable to inspect bootstrap runtime directory: {error}"))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(
            "bootstrap runtime directory must be private, owner-controlled and non-symlinked"
                .to_string(),
        );
    }
    Ok(())
}

fn read_request(path: &Path) -> Result<OperationRequest, String> {
    OperationRequest::decode(&super::rating::read_private_bounded(
        path,
        MAX_OPERATION_RECORD_BYTES,
    )?)
}

fn capture_snapshot_matches_or_immediately_precedes_request(
    snapshot: &AutotuneCaptureSnapshot,
    request: &AutotuneCaptureRequest,
    current_boot_ms: u64,
) -> bool {
    snapshot.request == *request
        || super::full_autotune::is_immediately_prior_capture_snapshot(
            snapshot,
            request,
            current_boot_ms,
        )
}

fn synchronize_bootstrap_capture(
    runtime_dir: &Path,
    store: &RuntimeOverrideStore,
    actuator: &mut OpenWrtBootstrapRuntimeActuator,
    capture: &mut BootstrapCaptureRuntime,
) -> Result<(), String> {
    capture.poll_probe_shutdown()?;
    let request_path = runtime_dir.join(CAPTURE_REQUEST_FILE);
    match fs::symlink_metadata(&request_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if capture.session.active_request().is_some()
                || fs::symlink_metadata(runtime_dir.join(CAPTURE_SNAPSHOT_FILE)).is_ok()
            {
                capture.clear(runtime_dir)?;
            }
            return Ok(());
        }
        Err(error) => {
            return Err(format!(
                "unable to inspect bootstrap Auto-Tune capture request: {error}"
            ))
        }
        Ok(_) => {}
    }
    let request = read_capture_request(&request_path)?;
    let boot_ms = monotonic_boot_ms()?;
    let snapshot_path = runtime_dir.join(CAPTURE_SNAPSHOT_FILE);
    match fs::symlink_metadata(&snapshot_path) {
        Ok(_) => {
            let snapshot = read_capture_snapshot(&snapshot_path)?;
            let same_request = snapshot.request == request;
            if !capture_snapshot_matches_or_immediately_precedes_request(
                &snapshot, &request, boot_ms,
            ) {
                return Err(
                    "bootstrap capture snapshot does not match its active request".to_string(),
                );
            }
            if same_request
                && snapshot.state != super::full_autotune::AutotuneCaptureState::Collecting
            {
                if snapshot.state == super::full_autotune::AutotuneCaptureState::Complete
                    && request.phase == AutotuneCapturePhase::IdleBaseline
                {
                    if let Err(error) = capture.restore_terminal_idle_baseline(
                        runtime_dir,
                        &snapshot,
                        &actuator.request,
                    ) {
                        let rejected = capture.session.replace_with_rejection(
                            &request,
                            "capture-idle-baseline-unavailable",
                            boot_ms,
                        )?;
                        publish_capture_snapshot(&snapshot_path, &rejected)?;
                        capture.idle_rate_reference = None;
                        capture.stop_probes();
                        return Err(error);
                    }
                }
                return Ok(());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "unable to inspect bootstrap Auto-Tune capture snapshot: {error}"
            ))
        }
    }
    if capture.probe_shutdown_pending() {
        return Ok(());
    }
    // The initial admission (and restart reconstruction) needs one exact
    // runtime attestation.  Re-attesting the unchanged active request on every
    // reactor turn is both redundant and harmful: an asynchronous controlled
    // counter read can complete while this synchronous check runs, so its
    // single-use exact read fence is already stale before the owner
    // accepts the completion.  Every counter read, transport result, CPU batch
    // and ICMP batch still crosses its own exact attestation boundary below.
    if capture.session.active_request() == Some(&request) {
        if let Some(snapshot) = capture.consume_load_evidence(runtime_dir, &request)? {
            BootstrapCaptureRuntime::publish_if_changed(runtime_dir, &snapshot)?;
        }
        return Ok(());
    }
    match actuator.attest_capture(store, &request, boot_ms) {
        Ok(idle_rate_reference) => match capture.ensure_pinger(&actuator.request) {
            Ok(()) => capture.admit(runtime_dir, &request, boot_ms, idle_rate_reference),
            Err(error) => {
                capture.reject(runtime_dir, &request, "capture-icmp-unavailable", boot_ms)?;
                Err(error)
            }
        },
        Err((code, detail)) => {
            capture.reject(runtime_dir, &request, code, boot_ms)?;
            Err(detail)
        }
    }
}

fn post_attestation_idle_phase_time<E, F>(
    sampled_at: Instant,
    attested_at: Instant,
    freshness: Duration,
    refresh: F,
) -> Result<Option<Instant>, E>
where
    F: FnOnce() -> Result<bool, E>,
{
    if attested_at.saturating_duration_since(sampled_at) <= freshness {
        return Ok(Some(sampled_at));
    }
    refresh().map(|fresh| fresh.then_some(attested_at))
}

fn dispatch_idle_phase_time(
    sampled_at: Instant,
    attested_at: Instant,
    freshness: Duration,
    refreshed: bool,
) -> Option<Instant> {
    if refreshed {
        Some(attested_at)
    } else if attested_at.saturating_duration_since(sampled_at) <= freshness {
        Some(sampled_at)
    } else {
        None
    }
}

fn owner_deadline(
    store: &RuntimeOverrideStore,
    request: &OperationRequest,
    outcome: RuntimeDriverOutcome,
) -> Result<Option<Duration>, String> {
    if outcome == RuntimeDriverOutcome::Restored {
        return Ok(None);
    }
    owner_authority_deadline(store, request)
}

fn owner_authority_deadline(
    store: &RuntimeOverrideStore,
    request: &OperationRequest,
) -> Result<Option<Duration>, String> {
    if let Some(permit) = store.read_permit()? {
        let now = monotonic_boot_ms()?;
        return Ok(Some(Duration::from_millis(
            permit.deadline_boot_ms.saturating_sub(now),
        )));
    }
    let now = super::rating::epoch_ms()?;
    Ok(Some(Duration::from_millis(
        request.deadline_unix_ms.saturating_sub(now),
    )))
}

fn earliest_deadline(owner: Option<Duration>, sampling: Option<Duration>) -> Option<Duration> {
    match (owner, sampling) {
        (Some(owner), Some(sampling)) => Some(owner.min(sampling)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn observe_probe_stopping(
    blocker: &RuntimeRestoreBlocker,
    target_interface: &str,
    probes_stopped: bool,
) -> Option<RuntimeRestoreObservation> {
    if !matches!(
        blocker,
        RuntimeRestoreBlocker::TopologySettling {
            target_interface: blocker_target,
            detail,
        } if blocker_target == target_interface && detail == PROBE_STOPPING_DETAIL
    ) {
        return None;
    }
    Some(if probes_stopped {
        RuntimeRestoreObservation::Ready
    } else {
        RuntimeRestoreObservation::Blocked(blocker.clone())
    })
}

fn bootstrap_owner_readiness_baseline(
    request: &OperationRequest,
    worker_run_id: &str,
    store: &RuntimeOverrideStore,
) -> Result<super::autotune_runtime::AbsentRuntimeBaseline, String> {
    if let Some(permit) = store.read_permit()? {
        validate_runtime_permit_admission(
            &permit,
            request,
            worker_run_id,
            &permit.worker,
            &permit.boot_id,
        )?;
        return match permit.baseline {
            RuntimeBaseline::Absent(baseline) => {
                validate_bootstrap_request_baseline(request, &baseline)?;
                Ok(baseline)
            }
            RuntimeBaseline::Managed(_) => Err(
                "bootstrap runtime owner found a managed permit in its private store".to_string(),
            ),
        };
    }
    if store.read_control()?.is_some()
        || store.read_restore_intent()?.is_some()
        || store.read_ack()?.is_some()
        || store.read_checkpoint()?.is_some()
    {
        return Err(
            "bootstrap runtime owner found state without its exact runtime permit".to_string(),
        );
    }
    let namespace_seed = read_kernel_uuid(
        Path::new(DEFAULT_RANDOM_UUID_PATH),
        "bootstrap runtime namespace seed",
    )?;
    OpenWrtNativeApplyBackend::new().capture_bootstrap_runtime_baseline(request, &namespace_seed)
}

pub(crate) fn run_bootstrap_runtime_owner<I>(args: I, terminate: &AtomicBool) -> Result<(), String>
where
    I: Iterator<Item = String>,
{
    let mut request_path = None;
    let mut runtime_dir = None;
    let mut worker_run_id = None;
    let mut args = args;
    while let Some(argument) = args.next() {
        let destination = match argument.as_str() {
            "--request" => &mut request_path,
            "--runtime-dir" => &mut runtime_dir,
            "--worker-run-id" => &mut worker_run_id,
            _ => {
                return Err(format!(
                    "unsupported bootstrap runtime argument: {argument}"
                ))
            }
        };
        if destination.is_some() {
            return Err(format!("duplicate bootstrap runtime argument: {argument}"));
        }
        *destination = Some(
            args.next()
                .ok_or_else(|| format!("{argument} requires a value"))?,
        );
    }
    let request_path =
        PathBuf::from(request_path.ok_or_else(|| "--request is required".to_string())?);
    let runtime_dir =
        PathBuf::from(runtime_dir.ok_or_else(|| "--runtime-dir is required".to_string())?);
    let worker_run_id = worker_run_id.ok_or_else(|| "--worker-run-id is required".to_string())?;
    if worker_run_id.len() != 32
        || !worker_run_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("bootstrap runtime worker run id is invalid".to_string());
    }
    let job_dir = request_path
        .parent()
        .ok_or_else(|| "bootstrap runtime request has no job directory".to_string())?;
    let expected_name = format!("bootstrap-runtime-{worker_run_id}");
    if request_path.file_name().and_then(|name| name.to_str()) != Some("request")
        || runtime_dir.parent() != Some(job_dir)
        || runtime_dir.file_name().and_then(|name| name.to_str()) != Some(expected_name.as_str())
    {
        return Err("bootstrap runtime paths are not bound to the exact worker run".to_string());
    }
    let request = read_request(&request_path)?;
    if request.identity.job_id
        != job_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
    {
        return Err("bootstrap runtime request is not in its exact job directory".to_string());
    }
    ensure_runtime_directory(&runtime_dir)?;
    let _owner_lock = BootstrapRuntimeOwnerLock::acquire(&runtime_dir, terminate)?;
    if terminate.load(Ordering::SeqCst) {
        return Ok(());
    }
    let store = RuntimeOverrideStore::open(&runtime_dir)?;
    let readiness_baseline = bootstrap_owner_readiness_baseline(&request, &worker_run_id, &store)?;
    let owner_claim = BootstrapRuntimeOwnerClaim::for_current_process(
        &request,
        &worker_run_id,
        readiness_baseline,
    )?;
    super::rating::atomic_private_write(
        &runtime_dir.join(OWNER_CLAIM_FILE),
        owner_claim.encode()?.as_bytes(),
    )?;
    let mut driver = RuntimeOverrideDriver::open(request.identity.instance.clone(), &runtime_dir)?;
    let capture_probes_stopped = Arc::new(AtomicBool::new(true));
    let mut actuator =
        OpenWrtBootstrapRuntimeActuator::new(request.clone(), Arc::clone(&capture_probes_stopped))?;
    let mut events = CalibrationEventLoop::new()?;
    events.watch_tree(&runtime_dir, 1, true)?;
    let mut saw_permit = false;
    let mut restore_requested = false;
    let mut capture = BootstrapCaptureRuntime::new(capture_probes_stopped);
    let mut last_capture_error: Option<String> = None;

    loop {
        capture.poll_probe_shutdown()?;
        if terminate.load(Ordering::SeqCst) && !restore_requested {
            capture.stop_probes();
            let checkpoint = store.read_checkpoint()?;
            if let Some(control) = store.read_control()? {
                store.publish_restore_intent(&control)?;
                restore_requested = true;
            } else if checkpoint.is_none() {
                // A parent-bound owner which has not crossed the durable
                // checkpoint boundary must never consume a newly published
                // permit after its coordinator has already terminated.
                return Ok(());
            }
        }
        /* A worker-published restore intent is a durable revocation event.  It
         * must outrank every loaded counter/transport completion and its early
         * reactor continue; otherwise a long burst can starve RuntimeDriver
         * restore until the worker's safety deadline expires. */
        let restore_intent_present = store.read_restore_intent()?.is_some();
        if restore_intent_present && !restore_requested {
            restore_requested = true;
            capture.stop_probes();
        }

        // A loaded completion is already bound to the exact request, epoch
        // and exact counter-read fence. Consume it before driver/runtime
        // health performs the expensive synchronous kernel reads which are
        // deliberately outside the 600 ms transport freshness budget.  The
        // loaded scheduler independently re-reads every durable revocation
        // record before it dispatches a probe.  Termination keeps priority.
        let mut loaded_counter_wait = LoadedCounterReactorWait::Inactive;
        if !terminate.load(Ordering::SeqCst) && !restore_requested {
            if let Err((code, error)) = capture.poll_loaded_completion_before_owner_poll(
                &runtime_dir,
                &store,
                &mut actuator,
            ) {
                if code == "capture-ended" {
                    capture.clear(&runtime_dir)?;
                    last_capture_error = None;
                } else {
                    capture.stop_probes();
                    let _ = capture.reject_active(&runtime_dir, code);
                    if last_capture_error.as_deref() != Some(error.as_str()) {
                        eprintln!("bootstrap Auto-Tune early completion rejected: {error}");
                        last_capture_error = Some(error);
                    }
                }
            }
            match capture.drive_loaded_counter_burst(&runtime_dir, &store, &mut actuator) {
                Ok(wait) => loaded_counter_wait = wait,
                Err(("capture-ended", _)) => {
                    capture.clear(&runtime_dir)?;
                    last_capture_error = None;
                }
                Err((code, error)) => {
                    capture.stop_probes();
                    let _ = capture.reject_active(&runtime_dir, code);
                    if last_capture_error.as_deref() != Some(error.as_str()) {
                        eprintln!("bootstrap Auto-Tune counter burst rejected: {error}");
                        last_capture_error = Some(error);
                    }
                }
            }
        }
        if loaded_counter_wait != LoadedCounterReactorWait::Inactive {
            if let Err(error) = capture.collect_loaded_pinger_during_burst() {
                capture.stop_probes();
                let _ = capture.reject_active(&runtime_dir, "capture-icmp-unavailable");
                if last_capture_error.as_deref() != Some(error.as_str()) {
                    eprintln!("bootstrap Auto-Tune burst ICMP capture rejected: {error}");
                    last_capture_error = Some(error);
                }
                loaded_counter_wait = LoadedCounterReactorWait::Inactive;
            }
        }
        if loaded_counter_wait != LoadedCounterReactorWait::Inactive {
            // While a read is active, its completion event owns the wake.  In
            // the bounded priming/bracketing gap, the immutable 200 ms sample
            // cadence is the only timer; it spaces physical endpoints but
            // does not perform a delayed state transition.
            let workers = store
                .read_permit()?
                .map(|permit| vec![permit.worker])
                .unwrap_or_default();
            if events.refresh_processes(&workers, Path::new(DEFAULT_PROC_ROOT))? {
                continue;
            }
            let sampling = match loaded_counter_wait {
                LoadedCounterReactorWait::SamplingCadence(wait) => Some(wait),
                LoadedCounterReactorWait::CounterCompletion
                | LoadedCounterReactorWait::TransportCompletion
                | LoadedCounterReactorWait::Inactive => None,
            };
            let timeout = earliest_deadline(owner_authority_deadline(&store, &request)?, sampling);
            let _ = events.wait(capture.poll_fd(), timeout)?;
            continue;
        }
        let preflight_permit = store.read_permit()?;
        let preflight_control = store.read_control()?;
        let preflight_checkpoint = store.read_checkpoint()?;
        let worker_unavailable = preflight_permit.as_ref().is_some_and(|permit| {
            !permit
                .worker
                .still_matches(Path::new(DEFAULT_PROC_ROOT))
                .unwrap_or(false)
        });
        let active_runtime_unhealthy = match (
            preflight_checkpoint.as_ref(),
            preflight_permit.as_ref(),
            preflight_control.as_ref(),
        ) {
            (Some(checkpoint), Some(permit), Some(control)) => {
                let expected = runtime_snapshot_for_control(permit, control);
                !actuator
                    .runtime_matches(&expected, checkpoint)
                    .unwrap_or(false)
            }
            (Some(_), _, _) => true,
            (None, _, _) => false,
        };
        if restore_intent_present || worker_unavailable || active_runtime_unhealthy {
            capture.stop_probes();
        }

        let outcome = driver.poll(&mut actuator)?;
        if outcome == RuntimeDriverOutcome::UnsafeRecoveryRequired {
            return Err(driver
                .unsafe_recovery_reason()
                .unwrap_or("bootstrap runtime recovery is unsafe")
                .to_string());
        }
        let permit = store.read_permit()?;
        saw_permit |= permit.is_some();
        let control = store.read_control()?;
        match synchronize_bootstrap_capture(&runtime_dir, &store, &mut actuator, &mut capture) {
            Ok(()) => last_capture_error = None,
            Err(error) => {
                if last_capture_error.as_deref() != Some(error.as_str()) {
                    eprintln!("bootstrap Auto-Tune capture rejected: {error}");
                    last_capture_error = Some(error);
                }
            }
        }
        // The pinger, transport worker and controlled-counter worker share one
        // eventfd.  Drain it exactly once, before consuming any of their
        // channels.  A completion which arrives after this point leaves the
        // descriptor readable for the next reactor turn instead of being
        // accidentally consumed by the final pinger-line drain.
        capture.drain_probe_wake()?;
        if let Err((code, error)) =
            capture.poll_rate_measurement(&runtime_dir, &store, &mut actuator)
        {
            capture.stop_probes();
            let _ = capture.reject_active(&runtime_dir, code);
            if last_capture_error.as_deref() != Some(error.as_str()) {
                eprintln!("bootstrap Auto-Tune sampling rejected: {error}");
                last_capture_error = Some(error);
            }
        }
        // A completed controlled counter window carries one short-lived exact
        // authority.  Give transport the first chance to consume it before a
        // CPU or ICMP observation performs an unrelated synchronous
        // attestation.  This is event ordering, not a retry timer.
        if let Err((code, error)) =
            capture.poll_transport(&runtime_dir, &store, &mut actuator, true)
        {
            capture.stop_probes();
            let _ = capture.reject_active(&runtime_dir, code);
            if last_capture_error.as_deref() != Some(error.as_str()) {
                eprintln!("bootstrap Auto-Tune transport capture rejected: {error}");
                last_capture_error = Some(error);
            }
        }
        if let Err((code, error)) =
            capture.poll_cpu_measurement(&runtime_dir, &store, &mut actuator)
        {
            capture.stop_probes();
            let _ = capture.reject_active(&runtime_dir, code);
            if last_capture_error.as_deref() != Some(error.as_str()) {
                eprintln!("bootstrap Auto-Tune sampling rejected: {error}");
                last_capture_error = Some(error);
            }
        }
        if let Err(error) = capture.drain_pinger(&runtime_dir, &store, &mut actuator) {
            capture.stop_probes();
            let _ = capture.reject_active(&runtime_dir, "capture-icmp-unavailable");
            if last_capture_error.as_deref() != Some(error.as_str()) {
                eprintln!("bootstrap Auto-Tune ICMP capture rejected: {error}");
                last_capture_error = Some(error);
            }
        }
        // Mint the next counter fence only after transport has consumed any
        // authority from the previous completion and every unrelated
        // synchronous observation has finished.  Counter completion/eventfd
        // is now the next positive event; no arbitrary retry delay is needed.
        if let Err((code, error)) =
            capture.schedule_loaded_counter_after_observations(&runtime_dir, &store, &mut actuator)
        {
            capture.stop_probes();
            let _ = capture.reject_active(&runtime_dir, code);
            if last_capture_error.as_deref() != Some(error.as_str()) {
                eprintln!("bootstrap Auto-Tune counter scheduling rejected: {error}");
                last_capture_error = Some(error);
            }
        }
        if outcome == RuntimeDriverOutcome::Rejected {
            if store.read_checkpoint()?.is_some() {
                return Err(
                    "bootstrap runtime driver rejected state after its durable checkpoint"
                        .to_string(),
                );
            }
            return Ok(());
        }
        if outcome == RuntimeDriverOutcome::Idle
            && saw_permit
            && permit.is_none()
            && control.is_none()
        {
            return Ok(());
        }
        if terminate.load(Ordering::SeqCst)
            && matches!(
                outcome,
                RuntimeDriverOutcome::Idle
                    | RuntimeDriverOutcome::Restored
                    | RuntimeDriverOutcome::Rejected
            )
        {
            return Ok(());
        }
        if permit.is_none() && super::rating::epoch_ms()? >= request.deadline_unix_ms {
            return Err(
                "bootstrap runtime permit was not published before the deadline".to_string(),
            );
        }

        let workers = permit
            .as_ref()
            .map(|permit| vec![permit.worker.clone()])
            .unwrap_or_default();
        if events.refresh_processes(&workers, Path::new(DEFAULT_PROC_ROOT))? {
            continue;
        }
        let timeout = earliest_deadline(
            owner_deadline(&store, &request, outcome)?,
            capture.next_sample_deadline()?,
        );
        let _ = events.wait(capture.poll_fd(), timeout)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autotune::{
        AccessEvidenceSource, AccessMedium, AutotuneProfile, CapacityLearningPolicy,
    };
    use crate::operations::full_autotune::{AutotuneCaptureState, MeasurementTopology};
    use crate::operations::protocol::{
        CalibrationStrategy, OperationIdentity, OperationKind, OperationOrigin,
        OperationRouteIdentity, OperationRouteMode, SpeedtestDirection,
    };
    use std::cell::Cell;
    use std::net::{IpAddr, Ipv4Addr};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::symlink;
    use std::sync::atomic::AtomicU64;

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

    #[derive(Default)]
    struct FakeCaptureAuthority {
        capture_attestations: u32,
        loaded_dispatch_attestations: u32,
        fail_capture: bool,
        fail_loaded: bool,
    }

    impl BootstrapCaptureAuthority for FakeCaptureAuthority {
        fn attest_capture(
            &mut self,
            _store: &RuntimeOverrideStore,
            _request: &AutotuneCaptureRequest,
            _current_boot_ms: u64,
        ) -> Result<(u64, u64), (&'static str, String)> {
            self.capture_attestations += 1;
            if self.fail_capture {
                return Err((
                    "capture-runtime-invalid",
                    "synthetic post-attestation failure".to_string(),
                ));
            }
            Ok((1_000_000, 500_000))
        }

        fn attest_loaded_transport_durable_authority(
            &mut self,
            _runtime_dir: &Path,
            _store: &RuntimeOverrideStore,
            _request: &AutotuneCaptureRequest,
            _current_boot_ms: u64,
        ) -> CaptureRuntimeResult {
            self.loaded_dispatch_attestations += 1;
            if self.fail_loaded {
                return Err((
                    "capture-runtime-invalid",
                    "synthetic durable-authority revocation".to_string(),
                ));
            }
            Ok(())
        }
    }

    fn request() -> OperationRequest {
        OperationRequest {
            identity: OperationIdentity {
                job_id: "11".repeat(16),
                job_token: "22".repeat(32),
                instance: "wan_sqm".to_string(),
                operation: OperationKind::FullAutotune,
                target_interface: "pppoe-wan".to_string(),
                route_fingerprint: "33".repeat(32),
                config_fingerprint: "44".repeat(32),
                sqm_fingerprint: "55".repeat(32),
            },
            created_unix_ms: 1_000,
            deadline_unix_ms: 2_000,
            origin: OperationOrigin::Luci,
            backend: "speedtest-go".to_string(),
            speedtest_direction: None,
            speedtest_server_id: Some(17_372),
            speedtest_topology: None,
            route: OperationRouteIdentity {
                mode: OperationRouteMode::Main,
                mwan3_member: None,
                l3_device: "pppoe-wan".to_string(),
                source_ip: Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2))),
                fwmark: None,
                routing_table: None,
            },
            target_state: OperationTargetState::AbsentBootstrap,
            capture_policy: Some(
                crate::operations::autotune_capture_policy::AutotuneCapturePolicyId::StandardV1,
            ),
            managed_sqm_section: Some("cake_wan_sqm".to_string()),
            profile: Some(AutotuneProfile::BestOverall),
            strategy: Some(CalibrationStrategy::FullRaw),
            access_medium: Some(AccessMedium::SharedWired),
            access_source: Some(AccessEvidenceSource::UserSelected),
            access_confidence_percent: 100,
            capacity_learning_policy: Some(CapacityLearningPolicy::VerifiedOnly),
            service_dl_cap_kbps: Some(1_000_000),
            service_ul_cap_kbps: Some(500_000),
            allow_sqm_disable: true,
            allow_active_traffic: false,
            scheduled_auto_apply_requested: false,
            traffic_budget_bytes: 1_000_000_000,
        }
    }

    fn absent_baseline(
        request: &OperationRequest,
    ) -> super::super::autotune_runtime::AbsentRuntimeBaseline {
        super::super::autotune_runtime::AbsentRuntimeBaseline {
            planned_sqm_section: request.managed_sqm_section.clone().unwrap(),
            target_interface: request.identity.target_interface.clone(),
            target_ifindex: 7,
            route_fingerprint: request.identity.route_fingerprint.clone(),
            config_fingerprint: request.identity.config_fingerprint.clone(),
            sqm_fingerprint: request.identity.sqm_fingerprint.clone(),
            kernel_topology_fingerprint: "77".repeat(32),
            kernel_namespace_seed: "88".repeat(16),
        }
    }

    fn claim(request: &OperationRequest) -> BootstrapRuntimeOwnerClaim {
        BootstrapRuntimeOwnerClaim {
            job_id: request.identity.job_id.clone(),
            worker_run_id: "66".repeat(16),
            request_sha256: request_sha256(request).unwrap(),
            process: ProcessIdentity {
                pid: 42,
                process_group: 42,
                starttime_ticks: 1_234,
            },
            baseline: absent_baseline(request),
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "cake-bootstrap-owner-{}-{}-{name}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed),
        ))
    }

    fn private_dir(name: &str) -> PathBuf {
        let path = temp_dir(name);
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    fn idle_capture_request() -> AutotuneCaptureRequest {
        AutotuneCaptureRequest {
            capture_id: "77".repeat(16),
            job_id: "11".repeat(16),
            worker_run_id: "66".repeat(16),
            permit_id: "88".repeat(16),
            instance_name: "wan_sqm".to_string(),
            sequence: 1,
            control_sequence: 0,
            deadline_boot_ms: 100_000,
            phase: AutotuneCapturePhase::IdleBaseline,
            topology: MeasurementTopology::RawBoth,
            direction: None,
            candidate_dl_kbps: None,
            candidate_ul_kbps: None,
            load_reference_kbps: None,
            transport_baseline_us: None,
            route_fingerprint: "33".repeat(32),
            sqm_fingerprint: "55".repeat(32),
        }
    }

    fn loaded_capture_request() -> AutotuneCaptureRequest {
        let mut request = idle_capture_request();
        request.capture_id = "99".repeat(16);
        request.sequence = 2;
        request.control_sequence = 1;
        request.phase = AutotuneCapturePhase::LoadedMeasurement;
        request.topology = MeasurementTopology::ShapedBoth;
        request.direction = Some(SpeedtestDirection::Download);
        request.candidate_dl_kbps = Some(100_000);
        request.candidate_ul_kbps = Some(50_000);
        request.load_reference_kbps = Some(100_000);
        request.transport_baseline_us = Some(9_500);
        request
    }

    fn loaded_transport_authority_fixture() -> (
        OperationRequest,
        AutotuneCaptureRequest,
        AutotuneRuntimePermit,
        AutotuneRuntimeControl,
        AutotuneRuntimeAck,
        RuntimeOverrideCheckpoint,
    ) {
        let operation = request();
        let policy =
            super::super::full_autotune::native_autotune_runtime_permit_policy(&operation).unwrap();
        let worker = ProcessIdentity {
            pid: 42,
            process_group: 42,
            starttime_ticks: 1_234,
        };
        let permit = AutotuneRuntimePermit {
            kind: super::super::autotune_runtime::RuntimePermitKind::Autotune,
            permit_id: "88".repeat(16),
            job_id: operation.identity.job_id.clone(),
            worker_run_id: "66".repeat(16),
            boot_id: "aa".repeat(16),
            coordinator_generation: "bb".repeat(16),
            worker: worker.clone(),
            instance_name: operation.identity.instance.clone(),
            target_interface: operation.identity.target_interface.clone(),
            route_identity: "main||pppoe-wan|192.0.2.2||254".to_string(),
            route_fingerprint: operation.identity.route_fingerprint.clone(),
            sqm_fingerprint: operation.identity.sqm_fingerprint.clone(),
            deadline_boot_ms: 100_000,
            maximum_sequence: policy.maximum_sequence,
            profile: operation.profile.unwrap(),
            link_kind: crate::autotune::LinkKind::Unknown,
            baseline: RuntimeBaseline::Absent(absent_baseline(&operation)),
            initial_download_kbps: 1_000_000,
            initial_upload_kbps: 500_000,
            download_qdisc_kind: super::super::autotune_runtime::RuntimeQdiscKind::Cake,
            upload_qdisc_kind: super::super::autotune_runtime::RuntimeQdiscKind::Cake,
            allow_bypass_download: policy.allow_directional_bypass,
            allow_bypass_upload: policy.allow_directional_bypass,
            download_bounds: policy.download_bounds,
            upload_bounds: policy.upload_bounds,
        };
        permit.validate().unwrap();
        let capture_request = loaded_capture_request();
        let control = AutotuneRuntimeControl {
            permit_id: permit.permit_id.clone(),
            job_id: permit.job_id.clone(),
            worker_run_id: permit.worker_run_id.clone(),
            boot_id: permit.boot_id.clone(),
            coordinator_generation: permit.coordinator_generation.clone(),
            worker,
            sequence: capture_request.control_sequence,
            deadline_boot_ms: 90_000,
            target_interface: permit.target_interface.clone(),
            route_fingerprint: permit.route_fingerprint.clone(),
            sqm_fingerprint: permit.sqm_fingerprint.clone(),
            topology: capture_request.topology,
            download_kbps: capture_request.candidate_dl_kbps,
            upload_kbps: capture_request.candidate_ul_kbps,
        };
        control.validate().unwrap();
        let ack = AutotuneRuntimeAck {
            permit_id: permit.permit_id.clone(),
            job_id: permit.job_id.clone(),
            worker_run_id: permit.worker_run_id.clone(),
            sequence: control.sequence,
            updated_boot_ms: 2_000,
            target_interface: permit.target_interface.clone(),
            route_fingerprint: permit.route_fingerprint.clone(),
            sqm_fingerprint: permit.sqm_fingerprint.clone(),
            state: super::super::full_autotune::RuntimeAckState::Applied,
            topology: Some(control.topology),
            download_kbps: control.download_kbps,
            upload_kbps: control.upload_kbps,
            diagnostic_code: None,
        };
        ack.validate().unwrap();
        let checkpoint = RuntimeOverrideCheckpoint::new(
            &permit,
            1_000,
            RuntimeBaseline::Absent(absent_baseline(&operation)),
        )
        .unwrap()
        .advance_temporary_stage(TemporaryTopologyStage::AbsenceAttested, None)
        .unwrap()
        .advance_temporary_stage(TemporaryTopologyStage::LinkOwned, Some(9))
        .unwrap()
        .advance_temporary_stage(TemporaryTopologyStage::Active, None)
        .unwrap();
        (operation, capture_request, permit, control, ack, checkpoint)
    }

    fn counter_observation(
        observed_start: Instant,
        observed_end: Instant,
        download_bytes: u64,
        upload_bytes: u64,
        rates: Option<(f64, f64)>,
    ) -> super::super::autotune_counter::AutotuneCounterObservation {
        super::super::autotune_counter::AutotuneCounterObservation {
            rate_window: rates.map(|(download_kbps, upload_kbps)| {
                super::super::autotune_counter::AutotuneCounterRateWindow {
                    download_kbps,
                    upload_kbps,
                    observed_start,
                    observed_end,
                    fresh: true,
                }
            }),
            delta: Some(super::super::autotune_counter::AutotuneCounterDelta {
                download_bytes,
                upload_bytes,
                observed_start,
                observed_end,
                within_maximum_span: true,
            }),
        }
    }

    fn complete_idle_snapshot() -> AutotuneCaptureSnapshot {
        let snapshot = AutotuneCaptureSnapshot {
            request: idle_capture_request(),
            state: AutotuneCaptureState::Complete,
            updated_boot_ms: 2_000,
            icmp_samples: super::super::full_autotune::MIN_AUTOTUNE_IDLE_ICMP_SAMPLES,
            transport_samples: super::super::full_autotune::MIN_AUTOTUNE_TRANSPORT_SAMPLES,
            transport_timeout_count: 0,
            transport_timeout_total_us: 0,
            transport_censored: false,
            cpu_samples: 0,
            idle_median_us: Some(8_000),
            idle_p95_us: Some(12_000),
            idle_transport_baseline_us: Some(9_500),
            icmp_delta_us: None,
            transport_delta_us: None,
            loss_ppm: None,
            cpu_milli_percent: None,
            background_confidence_percent: Some(100),
            contaminated: false,
            diagnostic_code: None,
        };
        snapshot.validate().unwrap();
        snapshot
    }

    fn icmp_sample(rtt_ms: f64) -> crate::Sample {
        crate::Sample {
            reflector: "192.0.2.1".to_string(),
            seq: "1".to_string(),
            timestamp: 1.0,
            rtt_ms,
            dl_owd_us: 0.0,
            ul_owd_us: 0.0,
            timestamped_owd: false,
        }
    }

    #[test]
    fn owner_claim_is_canonical_and_bound_to_the_exact_request_and_worker() {
        let request = request();
        let claim = claim(&request);
        let encoded = claim.encode().unwrap();
        assert!(encoded.starts_with("cake-autorate-bootstrap-runtime\t2\towner\n"));
        assert_eq!(BootstrapRuntimeOwnerClaim::decode(&encoded).unwrap(), claim);
        claim.attest(&request, &"66".repeat(16)).unwrap();

        let mut changed = request.clone();
        changed.identity.route_fingerprint = "77".repeat(32);
        assert!(claim.attest(&changed, &"66".repeat(16)).is_err());
        assert!(claim.attest(&request, &"88".repeat(16)).is_err());
        assert!(BootstrapRuntimeOwnerClaim::decode(
            &encoded.replace("process_group=42", "process_group=41")
        )
        .is_err());
        let foreign_baseline = BootstrapRuntimeOwnerClaim::decode(&encoded.replace(
            &format!(
                "baseline_route_fingerprint={}\n",
                request.identity.route_fingerprint
            ),
            &format!("baseline_route_fingerprint={}\n", "aa".repeat(32)),
        ))
        .unwrap();
        assert!(foreign_baseline.attest(&request, &"66".repeat(16)).is_err());
    }

    #[test]
    fn owner_claim_reader_preserves_missing_and_rejects_foreign_authority() {
        let directory = private_dir("claim-reader");
        let request = request();
        let worker_run_id = "66".repeat(16);
        assert!(
            read_bootstrap_runtime_owner_claim(&directory, &request, &worker_run_id)
                .unwrap()
                .is_none()
        );

        let claim = claim(&request);
        super::super::rating::atomic_private_write(
            &directory.join(OWNER_CLAIM_FILE),
            claim.encode().unwrap().as_bytes(),
        )
        .unwrap();
        assert_eq!(
            read_bootstrap_runtime_owner_claim(&directory, &request, &worker_run_id)
                .unwrap()
                .unwrap(),
            claim
        );
        assert!(
            read_bootstrap_runtime_owner_claim(&directory, &request, &"88".repeat(16)).is_err()
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn runtime_owner_lock_rejects_a_symlink_before_waiting() {
        let directory = private_dir("lock-symlink");
        let target = directory.join("foreign");
        fs::write(&target, b"").unwrap();
        symlink(&target, directory.join(OWNER_LOCK_FILE)).unwrap();
        let terminate = AtomicBool::new(false);
        assert!(BootstrapRuntimeOwnerLock::acquire(&directory, &terminate).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn bootstrap_capture_publishes_raw_admission_once_and_removes_it_exactly() {
        let directory = private_dir("capture-session");
        let request = idle_capture_request();
        let mut capture = BootstrapCaptureRuntime::new(Arc::new(AtomicBool::new(true)));
        capture
            .admit(&directory, &request, 1_000, (100_000, 50_000))
            .unwrap();
        let snapshot_path = directory.join(CAPTURE_SNAPSHOT_FILE);
        let snapshot = read_capture_snapshot(&snapshot_path).unwrap();
        assert_eq!(snapshot.state, AutotuneCaptureState::Collecting);
        assert_eq!(snapshot.request.topology, MeasurementTopology::RawBoth);
        assert_eq!(capture.idle_rate_reference, Some((100_000, 50_000)));
        let first_inode = fs::metadata(&snapshot_path).unwrap().ino();

        capture
            .admit(&directory, &request, 1_001, (100_000, 50_000))
            .unwrap();
        assert_eq!(fs::metadata(&snapshot_path).unwrap().ino(), first_inode);

        capture
            .reject(&directory, &request, "capture-runtime-mismatch", 1_002)
            .unwrap();
        assert_eq!(
            read_capture_snapshot(&snapshot_path).unwrap().state,
            AutotuneCaptureState::Rejected
        );
        capture.clear(&directory).unwrap();
        assert!(!snapshot_path.exists());
        assert!(capture.session.active_request().is_none());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn worker_request_removal_ends_the_loaded_reactor_without_a_rejection() {
        let directory = private_dir("capture-worker-ended");
        let request = loaded_capture_request();
        let request_path = directory.join(CAPTURE_REQUEST_FILE);
        super::super::rating::atomic_private_write(
            &request_path,
            request.encode().unwrap().as_bytes(),
        )
        .unwrap();
        let mut capture = BootstrapCaptureRuntime::new(Arc::new(AtomicBool::new(true)));
        let snapshot = capture.session.admit(&request, 1_000).unwrap();
        BootstrapCaptureRuntime::publish_if_changed(&directory, &snapshot).unwrap();
        capture.loaded_counter_burst = LoadedCounterBurstState::begin();
        assert!(!capture
            .clear_if_private_request_removed(&directory)
            .unwrap());

        fs::remove_file(&request_path).unwrap();
        assert!(capture
            .clear_if_private_request_removed(&directory)
            .unwrap());
        assert!(capture.session.active_request().is_none());
        assert_eq!(capture.loaded_counter_burst, LoadedCounterBurstState::Idle);
        assert!(!directory.join(CAPTURE_SNAPSHOT_FILE).exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn raw_idle_capture_cannot_claim_a_direction_or_shaped_rate() {
        let mut request = idle_capture_request();
        request.direction = Some(SpeedtestDirection::Download);
        assert!(request.validate().is_err());
        request.direction = None;
        request.candidate_dl_kbps = Some(100_000);
        assert!(request.validate().is_err());
    }

    #[test]
    fn slow_idle_attestation_requires_a_fresh_post_attestation_rate_sample() {
        let sampled_at = Instant::now();
        let freshness = Duration::from_millis(600);
        let refresh_calls = Cell::new(0_u32);
        let fast = post_attestation_idle_phase_time::<(), _>(
            sampled_at,
            sampled_at + freshness,
            freshness,
            || {
                refresh_calls.set(refresh_calls.get() + 1);
                Ok(true)
            },
        )
        .unwrap();
        assert_eq!(fast, Some(sampled_at));
        assert_eq!(refresh_calls.get(), 0);

        let attested_at = sampled_at + freshness + Duration::from_millis(1);
        let refreshed =
            post_attestation_idle_phase_time::<(), _>(sampled_at, attested_at, freshness, || {
                refresh_calls.set(refresh_calls.get() + 1);
                Ok(true)
            })
            .unwrap();
        assert_eq!(refreshed, Some(attested_at));
        assert_eq!(refresh_calls.get(), 1);

        let unavailable =
            post_attestation_idle_phase_time::<(), _>(sampled_at, attested_at, freshness, || {
                refresh_calls.set(refresh_calls.get() + 1);
                Ok(false)
            })
            .unwrap();
        assert_eq!(unavailable, None);
        assert_eq!(refresh_calls.get(), 2);

        assert_eq!(
            dispatch_idle_phase_time(sampled_at, sampled_at + freshness, freshness, false),
            Some(sampled_at)
        );
        assert_eq!(
            dispatch_idle_phase_time(sampled_at, attested_at, freshness, true),
            Some(attested_at)
        );
        assert_eq!(
            dispatch_idle_phase_time(sampled_at, attested_at, freshness, false),
            None
        );
    }

    #[test]
    fn controlled_counter_window_is_directional_and_has_a_real_interval() {
        let capture_request = loaded_capture_request();
        let policy = request().capture_policy.unwrap().expand().unwrap();
        let started = Instant::now();
        let window = loaded_phase_window(
            &capture_request,
            super::super::autotune_counter::AutotuneCounterRateWindow {
                download_kbps: 10_000.0,
                upload_kbps: 500.0,
                observed_start: started,
                observed_end: started + Duration::from_secs(1),
                fresh: true,
            },
            &policy,
        )
        .unwrap();
        assert_eq!(window.phase, (true, false));
        assert!(loaded_phase_window(
            &capture_request,
            super::super::autotune_counter::AutotuneCounterRateWindow {
                download_kbps: 10_000.0,
                upload_kbps: 500.0,
                observed_start: started,
                observed_end: started + Duration::from_secs(1),
                fresh: false,
            },
            &policy,
        )
        .is_err());
        assert!(loaded_phase_window(
            &capture_request,
            super::super::autotune_counter::AutotuneCounterRateWindow {
                observed_end: started,
                ..super::super::autotune_counter::AutotuneCounterRateWindow {
                    download_kbps: 10_000.0,
                    upload_kbps: 500.0,
                    observed_start: started,
                    observed_end: started + Duration::from_secs(1),
                    fresh: true,
                }
            },
            &policy,
        )
        .is_err());
    }

    #[test]
    fn causal_upload_ack_credit_uses_each_physical_delta_once() {
        let mut capture_request = loaded_capture_request();
        capture_request.direction = Some(SpeedtestDirection::Upload);
        capture_request.load_reference_kbps = Some(100_000);
        let policy = request().capture_policy.unwrap().expand().unwrap();
        let started = Instant::now();
        let mut credit = None;

        for index in 0..4 {
            let slice_start = started + Duration::from_millis(index * 200);
            let (window, _physical, preserve) = loaded_phase_window_with_ack_credit(
                &capture_request,
                counter_observation(
                    slice_start,
                    slice_start + Duration::from_millis(200),
                    0,
                    2_500_000,
                    None,
                ),
                &policy,
                7,
                &mut credit,
            )
            .unwrap();
            assert!(window.is_none());
            assert!(preserve, "early exact deltas must retain causal credit");
        }

        let (displaced_acks, _physical, preserve) = loaded_phase_window_with_ack_credit(
            &capture_request,
            counter_observation(
                started + Duration::from_millis(800),
                started + Duration::from_secs(1),
                600_000,
                2_500_000,
                Some((24_000.0, 100_000.0)),
            ),
            &policy,
            7,
            &mut credit,
        )
        .unwrap();
        let displaced_acks = displaced_acks.unwrap();
        assert_eq!(
            displaced_acks.phase,
            (false, true),
            "compressed reverse ACKs remain upload-only when every byte is causally covered"
        );
        assert!(preserve);
        let credit = credit.as_ref().unwrap();
        assert_eq!(credit.slices.len(), 5);
        assert_eq!(
            credit.remaining_forward_credit_units,
            u128::from(400_000u64) * ACK_CREDIT_SCALE,
            "overlapping rolling rate windows must not mint duplicate byte credit"
        );
    }

    #[test]
    fn ack_credit_phase_authority_covers_the_full_rolling_rate_window() {
        let mut capture_request = loaded_capture_request();
        capture_request.direction = Some(SpeedtestDirection::Download);
        capture_request.load_reference_kbps = Some(100_000);
        let policy = request().capture_policy.unwrap().expand().unwrap();
        let started = Instant::now();
        let observed_end = started + Duration::from_secs(1);
        let delta_start = observed_end - Duration::from_millis(200);
        let mut credit = None;
        let observation = super::super::autotune_counter::AutotuneCounterObservation {
            rate_window: Some(super::super::autotune_counter::AutotuneCounterRateWindow {
                download_kbps: 100_000.0,
                upload_kbps: 1_000.0,
                observed_start: started,
                observed_end,
                fresh: true,
            }),
            delta: Some(super::super::autotune_counter::AutotuneCounterDelta {
                download_bytes: 2_500_000,
                upload_bytes: 20_000,
                observed_start: delta_start,
                observed_end,
                within_maximum_span: true,
            }),
        };

        let (window, _physical, preserve) = loaded_phase_window_with_ack_credit(
            &capture_request,
            observation,
            &policy,
            12,
            &mut credit,
        )
        .unwrap();
        let window = window.expect("the rolling rate window must mint load authority");
        assert_eq!(window.phase, (true, false));
        assert_eq!(window.observed_start, started);
        assert_eq!(window.observed_end, observed_end);
        assert_ne!(window.observed_start, delta_start);
        assert!(preserve);
    }

    #[test]
    fn rolling_rate_window_must_cover_the_latest_physical_delta() {
        let capture_request = loaded_capture_request();
        let policy = request().capture_policy.unwrap().expand().unwrap();
        let started = Instant::now();
        let observed_end = started + Duration::from_secs(1);
        let mut credit = None;
        let observation = super::super::autotune_counter::AutotuneCounterObservation {
            rate_window: Some(super::super::autotune_counter::AutotuneCounterRateWindow {
                download_kbps: 100_000.0,
                upload_kbps: 1_000.0,
                observed_start: started + Duration::from_millis(900),
                observed_end,
                fresh: true,
            }),
            delta: Some(super::super::autotune_counter::AutotuneCounterDelta {
                download_bytes: 2_500_000,
                upload_bytes: 20_000,
                observed_start: started + Duration::from_millis(800),
                observed_end,
                within_maximum_span: true,
            }),
        };

        assert!(loaded_phase_window_with_ack_credit(
            &capture_request,
            observation,
            &policy,
            12,
            &mut credit,
        )
        .is_err());
        assert!(credit.is_none());
    }

    #[test]
    fn uncovered_reverse_is_held_until_its_exact_physical_slice_expires() {
        let mut capture_request = loaded_capture_request();
        capture_request.direction = Some(SpeedtestDirection::Upload);
        capture_request.load_reference_kbps = Some(100_000);
        let policy = request().capture_policy.unwrap().expand().unwrap();
        let started = Instant::now();
        let mut credit = None;

        let (material, _physical, _) = loaded_phase_window_with_ack_credit(
            &capture_request,
            counter_observation(
                started,
                started + Duration::from_secs(1),
                1_200_000,
                12_500_000,
                Some((9_600.0, 100_000.0)),
            ),
            &policy,
            7,
            &mut credit,
        )
        .unwrap();
        assert_eq!(material.unwrap().phase, (true, true));

        let (held, _physical, _) = loaded_phase_window_with_ack_credit(
            &capture_request,
            counter_observation(
                started + Duration::from_secs(1),
                started + Duration::from_millis(1_200),
                0,
                2_500_000,
                Some((0.0, 100_000.0)),
            ),
            &policy,
            7,
            &mut credit,
        )
        .unwrap();
        assert_eq!(held.unwrap().phase, (true, true));

        let (expired, _physical, _) = loaded_phase_window_with_ack_credit(
            &capture_request,
            counter_observation(
                started + Duration::from_millis(2_200),
                started + Duration::from_millis(2_400),
                0,
                2_500_000,
                Some((0.0, 100_000.0)),
            ),
            &policy,
            7,
            &mut credit,
        )
        .unwrap();
        assert_eq!(expired.unwrap().phase, (false, true));
    }

    #[test]
    fn ack_credit_never_establishes_forward_load_and_rotates_with_epoch() {
        let mut capture_request = loaded_capture_request();
        capture_request.direction = Some(SpeedtestDirection::Upload);
        capture_request.load_reference_kbps = Some(100_000);
        let policy = request().capture_policy.unwrap().expand().unwrap();
        let started = Instant::now();
        let mut credit = None;

        loaded_phase_window_with_ack_credit(
            &capture_request,
            counter_observation(
                started,
                started + Duration::from_millis(200),
                0,
                2_500_000,
                None,
            ),
            &policy,
            7,
            &mut credit,
        )
        .unwrap();
        let (below_threshold, _physical, _) = loaded_phase_window_with_ack_credit(
            &capture_request,
            counter_observation(
                started + Duration::from_millis(200),
                started + Duration::from_millis(400),
                100_000,
                2_500_000,
                Some((4_000.0, 1_999.0)),
            ),
            &policy,
            7,
            &mut credit,
        )
        .unwrap();
        assert_eq!(below_threshold.unwrap().phase, (false, false));

        let (rotated_epoch, _physical, _) = loaded_phase_window_with_ack_credit(
            &capture_request,
            counter_observation(
                started + Duration::from_millis(400),
                started + Duration::from_millis(600),
                100_000,
                0,
                Some((4_000.0, 100_000.0)),
            ),
            &policy,
            8,
            &mut credit,
        )
        .unwrap();
        assert_eq!(
            rotated_epoch.unwrap().phase,
            (true, true),
            "a previous counter epoch cannot authorize reverse traffic"
        );
        assert_eq!(credit.as_ref().map(|value| value.epoch), Some(8));
    }

    #[test]
    fn download_ack_credit_is_symmetric_and_stale_deltas_clear_authority() {
        let mut capture_request = loaded_capture_request();
        capture_request.direction = Some(SpeedtestDirection::Download);
        capture_request.load_reference_kbps = Some(100_000);
        let policy = request().capture_policy.unwrap().expand().unwrap();
        let started = Instant::now();
        let mut credit = None;

        let (covered, _physical, _) = loaded_phase_window_with_ack_credit(
            &capture_request,
            counter_observation(
                started,
                started + Duration::from_secs(1),
                12_500_000,
                800_000,
                Some((100_000.0, 6_400.0)),
            ),
            &policy,
            9,
            &mut credit,
        )
        .unwrap();
        assert_eq!(covered.unwrap().phase, (true, false));

        let mut stale = counter_observation(
            started + Duration::from_secs(1),
            started + Duration::from_millis(1_800),
            2_500_000,
            0,
            Some((100_000.0, 0.0)),
        );
        stale.delta.as_mut().unwrap().within_maximum_span = false;
        let (window, _physical, preserve) =
            loaded_phase_window_with_ack_credit(&capture_request, stale, &policy, 9, &mut credit)
                .unwrap();
        assert!(window.is_none());
        assert!(!preserve);
        assert!(credit.is_none());
    }

    #[test]
    fn ack_credit_capacity_and_slice_count_are_bounded() {
        let mut capture_request = loaded_capture_request();
        capture_request.direction = Some(SpeedtestDirection::Upload);
        capture_request.load_reference_kbps = Some(100_000);
        let policy = request().capture_policy.unwrap().expand().unwrap();
        let started = Instant::now();
        let mut credit = None;

        for index in 0..MAX_ACK_CREDIT_SLICES {
            let slice_start = started + Duration::from_millis(index as u64);
            loaded_phase_window_with_ack_credit(
                &capture_request,
                counter_observation(
                    slice_start,
                    slice_start + Duration::from_millis(1),
                    0,
                    u64::MAX / 2,
                    None,
                ),
                &policy,
                11,
                &mut credit,
            )
            .unwrap();
        }
        assert_eq!(credit.as_ref().unwrap().slices.len(), MAX_ACK_CREDIT_SLICES);
        assert_eq!(
            credit.as_ref().unwrap().remaining_forward_credit_units,
            u128::from(1_000_000u64) * ACK_CREDIT_SCALE,
            "one-second service-reference capacity must cap stored credit"
        );
        let overflow_start = started + Duration::from_millis(MAX_ACK_CREDIT_SLICES as u64);
        assert!(loaded_phase_window_with_ack_credit(
            &capture_request,
            counter_observation(
                overflow_start,
                overflow_start + Duration::from_millis(1),
                0,
                1,
                None,
            ),
            &policy,
            11,
            &mut credit,
        )
        .is_err());
        assert!(credit.is_none());
    }

    #[test]
    fn idle_baseline_roundtrip_is_policy_bound_and_restores_loaded_capture() {
        let directory = private_dir("idle-baseline-roundtrip");
        let policy = request().capture_policy.unwrap().expand().unwrap();
        let baseline =
            BootstrapIdleBaseline::from_snapshot(&complete_idle_snapshot(), &policy).unwrap();
        let encoded = baseline.encode().unwrap();
        assert_eq!(BootstrapIdleBaseline::decode(&encoded).unwrap(), baseline);
        publish_idle_baseline(&directory, &baseline).unwrap();
        publish_idle_baseline(&directory, &baseline).unwrap();
        let mut rewritten = baseline.clone();
        rewritten.transport_baseline_us += 1;
        assert!(publish_idle_baseline(&directory, &rewritten).is_err());
        assert_eq!(
            read_idle_baseline(&directory).unwrap(),
            Some(baseline.clone())
        );

        let loaded = loaded_capture_request();
        baseline.attest_loaded(&loaded, &policy).unwrap();
        let mut capture = BootstrapCaptureRuntime::new(Arc::new(AtomicBool::new(true)));
        capture.policy = Some(policy.clone());
        capture.idle_transport_baseline_ms = Some(99.0);
        capture
            .persist_completed_idle_baseline(&directory, &complete_idle_snapshot())
            .unwrap();
        assert_eq!(
            read_idle_baseline(&directory).unwrap(),
            Some(baseline.clone()),
            "the terminal capture snapshot, not a later local tracker value, owns the durable baseline"
        );
        assert_eq!(capture.idle_transport_baseline_ms, Some(9.5));
        capture
            .restore_loaded_idle_baseline(&directory, &loaded)
            .unwrap();
        assert_eq!(capture.idle_icmp_baseline_ms, Some(8.0));
        assert_eq!(capture.idle_transport_baseline_ms, Some(9.5));

        let mut foreign = loaded.clone();
        foreign.route_fingerprint = "aa".repeat(32);
        assert!(baseline.attest_loaded(&foreign, &policy).is_err());
        let mut drifted = loaded.clone();
        drifted.transport_baseline_us = Some(baseline.transport_baseline_us + 1);
        assert!(baseline.attest_loaded(&drifted, &policy).is_err());
        assert!(capture
            .restore_loaded_idle_baseline(&directory, &drifted)
            .is_err());
        let mut regressed = loaded;
        regressed.sequence = baseline.idle_sequence;
        assert!(baseline.attest_loaded(&regressed, &policy).is_err());
        assert!(BootstrapIdleBaseline::decode(
            &encoded.replace("transport_baseline_us=9500", "transport_baseline_us=0")
        )
        .is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_new_idle_capture_removes_only_same_job_stale_baseline() {
        let directory = private_dir("idle-baseline-ownership");
        let policy = request().capture_policy.unwrap().expand().unwrap();
        let baseline =
            BootstrapIdleBaseline::from_snapshot(&complete_idle_snapshot(), &policy).unwrap();
        publish_idle_baseline(&directory, &baseline).unwrap();

        let mut next = idle_capture_request();
        next.capture_id = "aa".repeat(16);
        next.sequence = 3;
        let mut capture = BootstrapCaptureRuntime::new(Arc::new(AtomicBool::new(true)));
        capture.prepare_idle_request(&directory, &next).unwrap();
        assert!(!directory.join(IDLE_BASELINE_FILE).exists());

        publish_idle_baseline(&directory, &baseline).unwrap();
        let mut foreign = next;
        foreign.job_id = "bb".repeat(16);
        assert!(capture.prepare_idle_request(&directory, &foreign).is_err());
        assert!(directory.join(IDLE_BASELINE_FILE).exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn terminal_capture_snapshot_cannot_regress_or_cross_request_identity() {
        let directory = private_dir("terminal-capture-fence");
        let terminal = complete_idle_snapshot();
        publish_capture_snapshot(&directory.join(CAPTURE_SNAPSHOT_FILE), &terminal).unwrap();

        let mut collecting = terminal.clone();
        collecting.state = AutotuneCaptureState::Collecting;
        collecting.idle_median_us = None;
        collecting.idle_p95_us = None;
        collecting.idle_transport_baseline_us = None;
        collecting.background_confidence_percent = None;
        collecting.validate().unwrap();
        assert!(BootstrapCaptureRuntime::publish_if_changed(&directory, &collecting).is_err());

        let mut foreign = collecting;
        foreign.request.capture_id = "aa".repeat(16);
        assert!(BootstrapCaptureRuntime::publish_if_changed(&directory, &foreign).is_err());
        assert_eq!(
            read_capture_snapshot(&directory.join(CAPTURE_SNAPSHOT_FILE)).unwrap(),
            terminal
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn exact_adjacent_capture_replaces_a_prior_terminal_snapshot_only() {
        let directory = private_dir("adjacent-capture-transition");
        let prior_request = loaded_capture_request();
        let mut prior_session = AutotuneCaptureSession::new();
        prior_session.admit(&prior_request, 1_000).unwrap();
        let prior = prior_session
            .reject("capture-runtime-mismatch", 1_001)
            .unwrap();
        publish_capture_snapshot(&directory.join(CAPTURE_SNAPSHOT_FILE), &prior).unwrap();

        let mut next_request = prior_request.clone();
        next_request.capture_id = "aa".repeat(16);
        next_request.sequence += 1;
        next_request.direction = Some(SpeedtestDirection::Upload);
        let mut next_session = AutotuneCaptureSession::new();
        let next = next_session.admit(&next_request, 1_002).unwrap();
        assert!(capture_snapshot_matches_or_immediately_precedes_request(
            &prior,
            &next_request,
            1_002,
        ));
        BootstrapCaptureRuntime::publish_if_changed(&directory, &next).unwrap();
        assert_eq!(
            read_capture_snapshot(&directory.join(CAPTURE_SNAPSHOT_FILE)).unwrap(),
            next,
        );

        let same_request_terminal = next_session
            .reject("capture-runtime-mismatch", 1_003)
            .unwrap();
        publish_capture_snapshot(
            &directory.join(CAPTURE_SNAPSHOT_FILE),
            &same_request_terminal,
        )
        .unwrap();
        assert!(BootstrapCaptureRuntime::publish_if_changed(&directory, &next).is_err());

        let mut skipped_request = prior_request.clone();
        skipped_request.capture_id = "bb".repeat(16);
        skipped_request.sequence += 2;
        let mut skipped_session = AutotuneCaptureSession::new();
        let skipped = skipped_session.admit(&skipped_request, 1_004).unwrap();
        assert!(!capture_snapshot_matches_or_immediately_precedes_request(
            &prior,
            &skipped_request,
            1_004,
        ));

        let foreign_directory = private_dir("foreign-adjacent-capture-transition");
        publish_capture_snapshot(&foreign_directory.join(CAPTURE_SNAPSHOT_FILE), &prior).unwrap();
        assert!(BootstrapCaptureRuntime::publish_if_changed(&foreign_directory, &skipped).is_err());

        let mut foreign_request = next_request;
        foreign_request.capture_id = "cc".repeat(16);
        foreign_request.route_fingerprint = "dd".repeat(32);
        let mut foreign_session = AutotuneCaptureSession::new();
        let foreign = foreign_session.admit(&foreign_request, 1_005).unwrap();
        assert!(!capture_snapshot_matches_or_immediately_precedes_request(
            &prior,
            &foreign_request,
            1_005,
        ));
        assert!(BootstrapCaptureRuntime::publish_if_changed(&foreign_directory, &foreign).is_err());

        fs::remove_dir_all(directory).unwrap();
        fs::remove_dir_all(foreign_directory).unwrap();
    }

    #[test]
    fn transport_stop_request_is_nonblocking_and_keeps_its_wake_fence() {
        let raw_wake = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        assert!(raw_wake >= 0);
        let wake = Arc::new(unsafe { OwnedFd::from_raw_fd(raw_wake) });
        let (request_tx, _request_rx) = mpsc::sync_channel(1);
        let (_result_tx, result_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let worker_wake = Arc::clone(&wake);
        let worker = thread::spawn(move || {
            let _guard = BootstrapTransportWakeGuard(worker_wake);
            ready_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        ready_rx.recv().unwrap();
        let mut transport = BootstrapTransportRuntime {
            request_tx: Some(request_tx),
            result_rx,
            worker: Some(worker),
            _wake: wake,
            control: crate::AutotuneTransportControl::default(),
            in_flight: None,
            next_probe_id: 1,
            last_started: None,
            last_readiness_block: None,
            idle_samples_ms: VecDeque::new(),
            idle_baseline_ms: None,
        };
        transport.request_stop();
        assert!(!transport.finish_stop());
        assert!(transport.wake_fd() >= 0);
        release_tx.send(()).unwrap();
        transport.stop();
    }

    #[test]
    fn pinger_drain_is_bounded_and_never_attests_inside_the_per_line_loop() {
        let source = include_str!("bootstrap_runtime_owner.rs");
        let collect_start = source
            .find("    fn collect_loaded_pinger_during_burst(")
            .unwrap();
        let collect_tail = &source[collect_start..];
        let collect_end = collect_tail.find("\n    fn drain_pinger(").unwrap();
        let collect = &collect_tail[..collect_end];
        assert!(collect.contains("Self::read_bounded_pinger_lines(pinger)?"));
        assert!(collect.contains("self.pending_icmp_samples.push_back("));
        assert!(!collect.contains("attest_capture("));
        assert!(!collect.contains("record_attested_observation"));

        let start = source.find("    fn drain_pinger(").unwrap();
        let tail = &source[start..];
        let end = tail.find("\n    fn admit(").unwrap();
        let body = &tail[..end];

        assert!(body.contains("Self::read_bounded_pinger_lines(pinger)?"));
        assert_eq!(body.matches(".attest_capture(").count(), 0);
        assert_eq!(body.matches("record_attested_observations(").count(), 1);
        assert!(!body.contains("record_attested_observation("));
        assert!(MAX_PINGER_LINES_PER_DRAIN <= MAX_PENDING_ICMP_SAMPLES);
    }

    #[test]
    fn active_loaded_capture_consumes_late_evidence_once_without_readmission() {
        let directory = private_dir("late-load-evidence");
        let now = monotonic_boot_ms().unwrap();
        let mut request = loaded_capture_request();
        request.deadline_boot_ms = now + 60_000;
        let mut capture = BootstrapCaptureRuntime::new(Arc::new(AtomicBool::new(true)));
        capture.session.admit(&request, now).unwrap();

        let mut observations = Vec::new();
        for _ in 0..super::super::full_autotune::MIN_AUTOTUNE_LOADED_ICMP_SAMPLES {
            observations.push(
                super::super::autotune_capture::AutotuneCaptureObservationKind::IcmpSuccess {
                    latency_us: None,
                    delta_us: Some(1_000),
                },
            );
        }
        for _ in 0..super::super::full_autotune::MIN_AUTOTUNE_TRANSPORT_SAMPLES {
            observations.push(
                super::super::autotune_capture::AutotuneCaptureObservationKind::TransportSuccess {
                    latency_us: None,
                    delta_us: Some(2_000),
                },
            );
        }
        observations.push(
            super::super::autotune_capture::AutotuneCaptureObservationKind::Cpu {
                milli_percent: 10_000,
            },
        );
        let collecting = capture
            .session
            .observe_batch(&request, observations, now)
            .unwrap();
        assert_eq!(collecting.state, AutotuneCaptureState::Collecting);
        assert_eq!(collecting.background_confidence_percent, None);

        let evidence = super::super::full_autotune::AutotuneLoadEvidence {
            request: request.clone(),
            published_boot_ms: now,
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
        super::super::full_autotune::publish_load_evidence(
            &directory.join(LOAD_EVIDENCE_FILE),
            &evidence,
        )
        .unwrap();

        let completed = capture
            .consume_load_evidence(&directory, &request)
            .unwrap()
            .unwrap();
        assert_eq!(completed.state, AutotuneCaptureState::Complete);
        assert_eq!(completed.background_confidence_percent, Some(90));
        assert!(capture
            .consume_load_evidence(&directory, &request)
            .unwrap()
            .is_none());
        assert_eq!(capture.session.active_request(), Some(&request));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn active_counter_completion_precedes_unrelated_capture_attestations() {
        let source = include_str!("bootstrap_runtime_owner.rs");
        let sync_start = source.find("fn synchronize_bootstrap_capture(").unwrap();
        let sync_tail = &source[sync_start..];
        let sync_end = sync_tail.find("\nfn owner_deadline(").unwrap();
        let sync = &sync_tail[..sync_end];
        let active_short_circuit = sync
            .find("if capture.session.active_request() == Some(&request)")
            .unwrap();
        let late_evidence = sync.find("capture.consume_load_evidence(").unwrap();
        let admission_attestation = sync.find("match actuator.attest_capture(").unwrap();
        assert!(active_short_circuit < late_evidence && late_evidence < admission_attestation);

        let loop_start = source
            .find("pub(crate) fn run_bootstrap_runtime_owner")
            .unwrap();
        let loop_tail = &source[loop_start..];
        let loop_end = loop_tail.find("\n#[cfg(test)]").unwrap();
        let owner_loop = &loop_tail[..loop_end];
        let termination = owner_loop
            .find("if terminate.load(Ordering::SeqCst) && !restore_requested")
            .unwrap();
        let runtime_preflight = owner_loop
            .find("let restore_intent_present = store.read_restore_intent()")
            .unwrap();
        let loaded_work_gate = owner_loop
            .find("if !terminate.load(Ordering::SeqCst) && !restore_requested")
            .unwrap();
        let early_completion = owner_loop
            .find("capture.poll_loaded_completion_before_owner_poll(")
            .unwrap();
        let burst_drive = owner_loop
            .find("capture.drive_loaded_counter_burst(")
            .unwrap();
        let barrier_start = owner_loop
            .find("if loaded_counter_wait != LoadedCounterReactorWait::Inactive")
            .unwrap();
        let driver_preflight = owner_loop
            .find("let preflight_permit = store.read_permit()")
            .unwrap();
        let driver_poll = owner_loop.find("let outcome = driver.poll(").unwrap();
        let wake = owner_loop.find("capture.drain_probe_wake()").unwrap();
        let rate = owner_loop.find("capture.poll_rate_measurement(").unwrap();
        let transport = owner_loop.find("capture.poll_transport(").unwrap();
        let cpu = owner_loop.find("capture.poll_cpu_measurement(").unwrap();
        let icmp = owner_loop.find("capture.drain_pinger(").unwrap();
        let next_counter = owner_loop
            .find("capture.schedule_loaded_counter_after_observations(")
            .unwrap();
        let outcome = owner_loop
            .find("if outcome == RuntimeDriverOutcome::Rejected")
            .unwrap();
        assert!(
            termination < early_completion
                && termination < runtime_preflight
                && runtime_preflight < loaded_work_gate
                && loaded_work_gate < early_completion
                && early_completion < burst_drive
                && burst_drive < barrier_start
                && barrier_start < driver_preflight
                && driver_preflight < driver_poll
                && driver_poll < wake
                && wake < rate
                && rate < transport
                && transport < cpu
                && cpu < icmp
                && icmp < next_counter
                && next_counter < outcome
        );
        let revocation = &owner_loop[runtime_preflight..early_completion];
        assert!(revocation.contains("restore_requested = true"));
        assert!(revocation.contains("capture.stop_probes()"));
        let barrier = &owner_loop[barrier_start..driver_preflight];
        assert!(barrier.contains("events.refresh_processes"));
        assert!(barrier.contains("owner_authority_deadline"));
        assert!(barrier.contains("LoadedCounterReactorWait::SamplingCadence"));
        assert!(barrier.contains("events.wait(capture.poll_fd(), timeout)"));
        assert!(barrier.contains("continue;"));
        assert!(!barrier.contains("driver.poll"));
        assert!(!barrier.contains("next_sample_deadline"));
        assert!(!barrier.contains("sleep"));

        let durable_barrier_start = source
            .find("    fn loaded_counter_barrier_active<")
            .unwrap();
        let durable_barrier_tail = &source[durable_barrier_start..];
        let durable_barrier_end = durable_barrier_tail
            .find("\n    fn loaded_transport_work_active(")
            .unwrap();
        let durable_barrier = &durable_barrier_tail[..durable_barrier_end];
        assert!(durable_barrier.contains("attest_loaded_transport_durable_authority("));

        let barrier_method_start = source.find("    fn drive_loaded_counter_burst<").unwrap();
        let barrier_method_tail = &source[barrier_method_start..];
        let barrier_method_end = barrier_method_tail
            .find("\n    fn next_sample_deadline(")
            .unwrap();
        let barrier_method = &barrier_method_tail[..barrier_method_end];
        assert!(barrier_method.contains("loaded_counter_barrier_active("));
        assert!(barrier_method.contains("schedule_loaded_counter(runtime_dir, store, actuator)"));
        let cadence_start = barrier_method
            .find("        if matches!(\n            self.loaded_counter_cycle,")
            .unwrap();
        let cadence_tail = &barrier_method[cadence_start..];
        let cadence_end = cadence_tail
            .find("\n        if self.schedule_loaded_counter(")
            .unwrap();
        let cadence = &cadence_tail[..cadence_end];
        assert!(cadence.contains("LoadedCounterCycleState::BaselineReady"));
        assert!(cadence.contains("LoadedCounterCycleState::NeedFlightEvidence"));
        assert!(!cadence.contains("attest_capture("));
        assert!(!cadence.contains("attest_loaded_transport_durable_authority("));

        let poll_rate_start = source.find("    fn poll_rate_sample(").unwrap();
        let poll_rate_tail = &source[poll_rate_start..];
        let poll_rate_end = poll_rate_tail
            .find("\n    fn schedule_loaded_counter<")
            .unwrap();
        let poll_rate = &poll_rate_tail[..poll_rate_end];
        let loaded_rate_start = poll_rate
            .find("            AutotuneCapturePhase::LoadedMeasurement => {")
            .unwrap();
        let loaded_rate_tail = &poll_rate[loaded_rate_start..];
        let loaded_rate_end = loaded_rate_tail.find("\n        Ok(())\n    }").unwrap();
        assert!(!loaded_rate_tail[..loaded_rate_end].contains("try_schedule(request)"));
        assert!(!loaded_rate_tail[..loaded_rate_end].contains("self.next_rate_sample ="));

        let accept_start = source.find("    fn accept_counter_completion<").unwrap();
        let accept_tail = &source[accept_start..];
        let accept_end = accept_tail
            .find("\n    fn apply_loaded_counter_update(")
            .unwrap();
        let accept = &accept_tail[..accept_end];
        let post_attest = accept
            .find("actuator.attest_capture(store, request, boot_ms)")
            .unwrap();
        let rolling_mutation = accept.find("let rolling = self").unwrap();
        let physical_mutation = accept.find("let physical = self.counter_physical").unwrap();
        assert!(post_attest < rolling_mutation);
        assert!(post_attest < physical_mutation);

        let schedule_start = source.find("    fn schedule_loaded_counter<").unwrap();
        let schedule_tail = &source[schedule_start..];
        let schedule_end = schedule_tail
            .find("\n    /// Start one bounded loaded counter burst")
            .unwrap();
        let schedule = &schedule_tail[..schedule_end];
        assert!(schedule.contains("LoadedCounterReadPurpose::AttestedBaseline =>"));
        assert!(schedule.contains("actuator.attest_capture(store, &request, boot_ms)"));
        assert!(schedule.contains("attest_loaded_transport_durable_authority("));
        let boundary_attestation = schedule.find("match purpose {").unwrap();
        let scheduled_at = schedule.find("let scheduled_at = Instant::now();").unwrap();
        let async_read = schedule.find(".try_schedule(&request)").unwrap();
        assert!(boundary_attestation < scheduled_at && scheduled_at < async_read);

        let pinger_start = source.find("    fn drain_pinger(").unwrap();
        let pinger_tail = &source[pinger_start..];
        let pinger_end = pinger_tail.find("\n    fn admit(").unwrap();
        assert!(!pinger_tail[..pinger_end].contains("drain_wake("));

        let transport_start = source.find("    fn poll_transport<").unwrap();
        let transport_tail = &source[transport_start..];
        let transport_end = transport_tail.find("\n    fn clear(").unwrap();
        let transport_poll = &transport_tail[..transport_end];
        assert!(transport_poll.contains("allow_schedule"));
        let physical_settlement = transport_poll
            .find(".settle_physical_interval_diagnostic(")
            .unwrap();
        let completion_attestation = transport_poll
            .find("actuator.attest_capture(store, &active, boot_ms)")
            .unwrap();
        let loaded_schedule = transport_poll
            .rfind("self.try_schedule_transport(runtime_dir, store, actuator)")
            .unwrap();
        assert!(physical_settlement < completion_attestation);
        assert!(completion_attestation < loaded_schedule);

        let early_method_start = source
            .find("    fn poll_loaded_completion_before_owner_poll(")
            .unwrap();
        let early_method_tail = &source[early_method_start..];
        let early_method_end = early_method_tail
            .find("\n    fn loaded_counter_barrier_active<")
            .unwrap();
        let early_method = &early_method_tail[..early_method_end];
        let terminal_request = early_method
            .find("clear_if_private_request_removed(runtime_dir)")
            .unwrap();
        let counter_poll = early_method.find("self.poll_rate_measurement(").unwrap();
        let transport_poll = early_method.find("self.poll_transport(").unwrap();
        let chained_advance = early_method
            .find("self.advance_loaded_counter_burst_after_poll(")
            .unwrap();
        assert!(terminal_request < counter_poll);
        assert!(counter_poll < transport_poll);
        assert!(transport_poll < chained_advance);

        let transition_start = early_method
            .find("    fn advance_loaded_counter_burst_after_poll<")
            .unwrap();
        let transition = &early_method[transition_start..];
        let exact_completion = transition.find("&& counter_completed").unwrap();
        let chained_dispatch = transition
            .find("self.try_schedule_transport(runtime_dir, store, actuator)")
            .unwrap();
        let flight_budget = transition.find("burst_before.after_reactor(").unwrap();
        assert!(exact_completion < chained_dispatch);
        assert!(chained_dispatch < flight_budget);
        assert!(transition[flight_budget..].contains("transport_active"));
        assert!(owner_loop.contains("Err((\"capture-ended\", _))"));
    }

    #[test]
    fn loaded_counter_reactor_barrier_requires_the_exact_in_flight_fence_pair() {
        assert_eq!(
            loaded_counter_barrier_required(true, true, true, true).unwrap(),
            true
        );
        assert_eq!(
            loaded_counter_barrier_required(true, true, false, false).unwrap(),
            false
        );
        assert!(loaded_counter_barrier_required(true, true, true, false).is_err());
        assert!(loaded_counter_barrier_required(true, true, false, true).is_err());
        assert_eq!(
            loaded_counter_barrier_required(false, true, true, true).unwrap(),
            false,
            "a rejected/stopping capture must never be held in the barrier"
        );
        assert_eq!(
            loaded_counter_barrier_required(true, false, true, false).unwrap(),
            false,
            "an invalidated previous loaded read cannot block a new idle capture"
        );
    }

    #[test]
    fn loaded_counter_cadence_wait_performs_no_synchronous_authority_work() {
        let directory = private_dir("loaded-cadence-no-authority-work");
        let store = RuntimeOverrideStore::open(&directory).unwrap();
        let boot_ms = monotonic_boot_ms().unwrap();
        let mut request = loaded_capture_request();
        request.deadline_boot_ms = boot_ms + 60_000;

        let mut capture = BootstrapCaptureRuntime::new(Arc::new(AtomicBool::new(false)));
        capture.session.admit(&request, boot_ms).unwrap();
        capture.loaded_counter_burst = LoadedCounterBurstState::begin();
        capture.loaded_counter_cycle = LoadedCounterCycleState::BaselineReady;
        capture.next_rate_sample = Instant::now() + Duration::from_millis(200);

        let mut authority = FakeCaptureAuthority::default();
        let wait = capture
            .drive_loaded_counter_burst(&directory, &store, &mut authority)
            .unwrap();

        let LoadedCounterReactorWait::SamplingCadence(remaining) = wait else {
            panic!("a future physical-counter cadence must remain a pure wait");
        };
        assert!(!remaining.is_zero());
        assert!(remaining <= Duration::from_millis(200));
        assert_eq!(authority.capture_attestations, 0);
        assert_eq!(authority.loaded_dispatch_attestations, 0);
        assert!(capture.loaded_counter_burst.active());
        assert!(capture.loaded_transport_authority.is_none());
        assert_eq!(
            capture.loaded_counter_cycle,
            LoadedCounterCycleState::BaselineReady
        );

        let mut flight_capture = BootstrapCaptureRuntime::new(Arc::new(AtomicBool::new(false)));
        flight_capture.session.admit(&request, boot_ms).unwrap();
        flight_capture.loaded_counter_burst = LoadedCounterBurstState::begin();
        flight_capture.loaded_counter_cycle = LoadedCounterCycleState::NeedFlightEvidence;
        flight_capture.loaded_topology_attestation = Some(LoadedTopologyAttestation {
            request: request.clone(),
            epoch: 7,
            attested_at: Instant::now(),
        });
        flight_capture.next_rate_sample = Instant::now() + Duration::from_millis(200);
        let wait = flight_capture
            .drive_loaded_counter_burst(&directory, &store, &mut authority)
            .unwrap();
        assert!(matches!(wait, LoadedCounterReactorWait::SamplingCadence(_)));
        assert_eq!(authority.capture_attestations, 0);
        assert_eq!(authority.loaded_dispatch_attestations, 0);
        assert_eq!(
            flight_capture.loaded_counter_cycle,
            LoadedCounterCycleState::NeedFlightEvidence
        );
        assert!(flight_capture.loaded_topology_attestation.is_some());
        assert!(flight_capture.loaded_transport_authority.is_none());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn loaded_counter_cycle_requires_post_attestation_rebase_and_flight_evidence() {
        assert_eq!(
            LoadedCounterCycleState::NeedAttestedBaseline.next_read(false),
            Some(LoadedCounterReadPurpose::AttestedBaseline)
        );
        assert_eq!(
            LoadedCounterCycleState::NeedAttestedBaseline
                .after_read(LoadedCounterReadPurpose::AttestedBaseline, true)
                .unwrap(),
            LoadedCounterCycleState::BaselineReady
        );
        assert_eq!(
            LoadedCounterCycleState::BaselineReady
                .after_read(LoadedCounterReadPurpose::Evidence, true)
                .unwrap(),
            LoadedCounterCycleState::NeedRebase
        );
        assert_eq!(
            LoadedCounterCycleState::NeedRebase
                .after_read(LoadedCounterReadPurpose::Rebase, true)
                .unwrap(),
            LoadedCounterCycleState::NeedFlightEvidence
        );
        assert_eq!(
            LoadedCounterCycleState::NeedFlightEvidence.next_read(false),
            Some(LoadedCounterReadPurpose::FlightEvidence)
        );
        assert_eq!(
            LoadedCounterCycleState::NeedFlightEvidence
                .after_read(LoadedCounterReadPurpose::FlightEvidence, true)
                .unwrap(),
            LoadedCounterCycleState::FlightReady
        );
        assert_eq!(LoadedCounterCycleState::FlightReady.next_read(false), None);
        assert_eq!(LoadedCounterCycleState::FlightActive.next_read(false), None);
        assert_eq!(
            LoadedCounterCycleState::FlightActive.next_read(true),
            Some(LoadedCounterReadPurpose::FlightPost)
        );
        assert_eq!(
            LoadedCounterCycleState::FlightActive
                .after_read(LoadedCounterReadPurpose::FlightPost, true)
                .unwrap(),
            LoadedCounterCycleState::NeedRebase
        );
        assert_eq!(
            LoadedCounterCycleState::NeedRebase
                .after_read(LoadedCounterReadPurpose::Rebase, false)
                .unwrap(),
            LoadedCounterCycleState::NeedAttestedBaseline
        );
        assert_eq!(
            LoadedCounterCycleState::NeedFlightEvidence
                .after_read(LoadedCounterReadPurpose::FlightEvidence, false)
                .unwrap(),
            LoadedCounterCycleState::NeedAttestedBaseline
        );
        assert!(LoadedCounterCycleState::BaselineReady
            .after_read(LoadedCounterReadPurpose::FlightPost, true)
            .unwrap_err()
            .contains("transition is invalid"));
    }

    #[test]
    fn need_rebase_drives_the_exact_counter_read_before_any_transport_dispatch() {
        let directory = private_dir("need-rebase-before-dispatch");
        let store = RuntimeOverrideStore::open(&directory).unwrap();
        let boot_ms = monotonic_boot_ms().unwrap();
        let mut request = loaded_capture_request();
        request.deadline_boot_ms = boot_ms + 60_000;
        let raw_wake = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        assert!(raw_wake >= 0);
        let wake = Arc::new(unsafe { OwnedFd::from_raw_fd(raw_wake) });
        let sampler =
            super::super::autotune_counter::AutotuneCounterSampler::with_test_reader_and_wake(
                |_| {
                    Ok(Some(super::super::speedtest::SpeedtestTrafficCounters {
                        rx_bytes: 1,
                        tx_bytes: 1,
                    }))
                },
                wake,
            )
            .unwrap();

        let mut capture = BootstrapCaptureRuntime::new(Arc::new(AtomicBool::new(false)));
        capture.session.admit(&request, boot_ms).unwrap();
        capture.counter_epoch = sampler.current_epoch();
        capture.counter_sampler = Some(sampler);
        capture.counter_rates = Some(
            super::super::autotune_counter::AutotuneCounterRateTracker::new_with_maximum_delta_span(
                200, 600,
            )
            .unwrap(),
        );
        capture.counter_physical = Some(
            super::super::autotune_counter::AutotuneCounterRateTracker::new_with_maximum_delta_span(
                200, 600,
            )
            .unwrap(),
        );
        capture.loaded_counter_cycle = LoadedCounterCycleState::NeedRebase;
        capture.loaded_counter_burst = LoadedCounterBurstState::begin();

        let mut authority = FakeCaptureAuthority::default();
        assert_eq!(
            capture
                .drive_loaded_counter_burst(&directory, &store, &mut authority)
                .unwrap(),
            LoadedCounterReactorWait::CounterCompletion
        );
        assert_eq!(authority.capture_attestations, 0);
        assert_eq!(authority.loaded_dispatch_attestations, 1);
        assert!(capture.transport.is_none());
        assert!(capture
            .pending_counter_fence
            .as_ref()
            .is_some_and(|fence| fence.purpose == LoadedCounterReadPurpose::Rebase));
        let rebase_fence = capture
            .pending_counter_fence
            .as_ref()
            .expect("Rebase must retain its exact read fence");
        let topology_fence = capture
            .loaded_topology_attestation
            .as_ref()
            .expect("Rebase must publish a fresh causal topology fence");
        assert_eq!(topology_fence.request, request);
        assert_eq!(topology_fence.epoch, capture.counter_epoch);
        assert_eq!(topology_fence.attested_at, rebase_fence.scheduled_at);
        let delta_start = rebase_fence.scheduled_at + Duration::from_millis(1);
        let delta_end = delta_start + Duration::from_millis(200);
        let causal_delta = LoadedPhysicalDelta {
            delta: super::super::autotune_counter::AutotuneCounterDelta {
                download_bytes: 20_000_000,
                upload_bytes: 200_000,
                observed_start: delta_start,
                observed_end: delta_end,
                within_maximum_span: true,
            },
            phase: Some((true, false)),
        };
        assert!(topology_fence
            .authorize(
                &request,
                capture.counter_epoch,
                &causal_delta,
                (true, false),
                Duration::from_millis(600),
            )
            .is_some());
        assert_eq!(
            capture.loaded_counter_cycle,
            LoadedCounterCycleState::NeedRebase
        );
        let pending = capture.loaded_counter_burst;
        capture
            .advance_loaded_counter_burst_after_poll(
                &directory,
                &store,
                &mut authority,
                pending,
                false,
            )
            .unwrap();
        assert_eq!(capture.loaded_counter_burst, pending);
        assert_eq!(
            capture.loaded_counter_cycle,
            LoadedCounterCycleState::NeedRebase,
            "fairness must not rewrite the cycle while its exact read fence is pending"
        );
        assert!(capture
            .pending_counter_fence
            .as_ref()
            .is_some_and(|fence| fence.purpose == LoadedCounterReadPurpose::Rebase));
        drop(capture);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn flight_evidence_read_is_cadence_spaced_and_uses_only_durable_precheck() {
        let directory = private_dir("flight-evidence-before-dispatch");
        let store = RuntimeOverrideStore::open(&directory).unwrap();
        let boot_ms = monotonic_boot_ms().unwrap();
        let mut request = loaded_capture_request();
        request.deadline_boot_ms = boot_ms + 60_000;
        let raw_wake = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        assert!(raw_wake >= 0);
        let wake = Arc::new(unsafe { OwnedFd::from_raw_fd(raw_wake) });
        let sampler =
            super::super::autotune_counter::AutotuneCounterSampler::with_test_reader_and_wake(
                |_| {
                    Ok(Some(super::super::speedtest::SpeedtestTrafficCounters {
                        rx_bytes: 20_000_000,
                        tx_bytes: 200_000,
                    }))
                },
                wake,
            )
            .unwrap();

        let mut capture = BootstrapCaptureRuntime::new(Arc::new(AtomicBool::new(false)));
        capture.session.admit(&request, boot_ms).unwrap();
        capture.counter_epoch = sampler.current_epoch();
        capture.counter_sampler = Some(sampler);
        capture.counter_rates = Some(
            super::super::autotune_counter::AutotuneCounterRateTracker::new_with_maximum_delta_span(
                200, 600,
            )
            .unwrap(),
        );
        capture.counter_physical = Some(
            super::super::autotune_counter::AutotuneCounterRateTracker::new_with_maximum_delta_span(
                200, 600,
            )
            .unwrap(),
        );
        capture.loaded_counter_cycle = LoadedCounterCycleState::NeedFlightEvidence;
        capture.loaded_counter_burst = LoadedCounterBurstState::begin();
        capture.loaded_topology_attestation = Some(LoadedTopologyAttestation {
            request: request.clone(),
            epoch: capture.counter_epoch,
            attested_at: Instant::now() - Duration::from_millis(1),
        });
        capture.next_rate_sample = Instant::now();

        let mut authority = FakeCaptureAuthority::default();
        assert_eq!(
            capture
                .drive_loaded_counter_burst(&directory, &store, &mut authority)
                .unwrap(),
            LoadedCounterReactorWait::CounterCompletion
        );
        assert_eq!(authority.capture_attestations, 0);
        assert_eq!(authority.loaded_dispatch_attestations, 1);
        assert!(capture.transport.is_none());
        assert!(capture
            .pending_counter_fence
            .as_ref()
            .is_some_and(|fence| fence.purpose == LoadedCounterReadPurpose::FlightEvidence));
        assert_eq!(
            capture.loaded_counter_cycle,
            LoadedCounterCycleState::NeedFlightEvidence
        );
        assert!(capture.loaded_topology_attestation.is_some());
        assert!(capture.loaded_transport_authority.is_none());

        drop(capture);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn slow_post_attestation_is_outside_the_rebased_physical_interval() {
        let request = loaded_capture_request();
        let started = Instant::now();
        let evidence_end = started + Duration::from_millis(200);
        let post_attested = evidence_end + Duration::from_millis(400);
        let rebase_end = post_attested + Duration::from_millis(1);
        let flight_evidence_end = rebase_end + Duration::from_millis(250);
        let freshness = Duration::from_millis(600);
        let baseline = super::super::speedtest::SpeedtestTrafficCounters {
            rx_bytes: 0,
            tx_bytes: 0,
        };
        let evidence_counters = super::super::speedtest::SpeedtestTrafficCounters {
            rx_bytes: 20_000_000,
            tx_bytes: 200_000,
        };
        let flight = super::super::speedtest::SpeedtestTrafficCounters {
            rx_bytes: 40_000_000,
            tx_bytes: 400_000,
        };
        let mut physical =
            super::super::autotune_counter::AutotuneCounterRateTracker::new_with_maximum_delta_span(
                200, 600,
            )
            .unwrap();
        assert!(physical
            .observe_counters_with_delta(started, Some(baseline))
            .delta
            .is_none());
        let evidence_delta = physical
            .observe_counters_with_delta(evidence_end, Some(evidence_counters))
            .delta
            .unwrap();
        let evidence = LoadedPhysicalDelta {
            delta: evidence_delta,
            phase: Some((true, false)),
        };
        let topology = LoadedTopologyAttestation {
            request: request.clone(),
            epoch: 7,
            attested_at: post_attested,
        };
        assert!(
            topology
                .authorize(&request, 7, &evidence, (true, false), freshness)
                .is_none(),
            "the pre-attestation evidence delta cannot authorize a later flight"
        );

        physical.reset(rebase_end);
        assert!(physical
            .observe_counters_with_delta(rebase_end, Some(evidence_counters))
            .delta
            .is_none());
        let flight_evidence = physical
            .observe_counters_with_delta(flight_evidence_end, Some(flight))
            .delta
            .unwrap();
        assert_eq!(flight_evidence.observed_start, rebase_end);
        assert_eq!(flight_evidence.observed_end, flight_evidence_end);
        assert_eq!(
            flight_evidence
                .observed_end
                .duration_since(flight_evidence.observed_start),
            Duration::from_millis(250)
        );
        assert!(flight_evidence.within_maximum_span);
        let flight_evidence = LoadedPhysicalDelta {
            delta: flight_evidence,
            phase: Some((true, false)),
        };
        let authority = topology
            .authorize(&request, 7, &flight_evidence, (true, false), freshness)
            .expect("the post-rebase physical delta must carry the fresh topology token");
        assert!(authority.permits(
            &request,
            7,
            Some(&flight_evidence),
            flight_evidence_end,
            freshness,
        ));
    }

    #[test]
    fn failed_post_attestation_accepts_no_loaded_interval() {
        let directory = private_dir("failed-post-attestation");
        let store = RuntimeOverrideStore::open(&directory).unwrap();
        let boot_ms = monotonic_boot_ms().unwrap();
        let mut request = loaded_capture_request();
        request.deadline_boot_ms = boot_ms + 60_000;
        let policy = super::super::autotune_capture_policy::AutotuneCapturePolicyId::StandardV2
            .expand()
            .unwrap();
        let counters = super::super::speedtest::SpeedtestTrafficCounters {
            rx_bytes: 20_000_000,
            tx_bytes: 200_000,
        };
        let raw_wake = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        assert!(raw_wake >= 0);
        let wake = Arc::new(unsafe { OwnedFd::from_raw_fd(raw_wake) });
        let mut sampler =
            super::super::autotune_counter::AutotuneCounterSampler::with_test_reader_and_wake(
                move |_| Ok(Some(counters)),
                Arc::clone(&wake),
            )
            .unwrap();
        let epoch = sampler.current_epoch();
        let scheduled_at = Instant::now();
        assert!(sampler.try_schedule(&request).unwrap());

        let baseline_at = scheduled_at - Duration::from_millis(200);
        let baseline = Some(super::super::speedtest::SpeedtestTrafficCounters {
            rx_bytes: 0,
            tx_bytes: 0,
        });
        let mut rolling =
            super::super::autotune_counter::AutotuneCounterRateTracker::new_with_maximum_delta_span(
                200, 600,
            )
            .unwrap();
        let mut physical =
            super::super::autotune_counter::AutotuneCounterRateTracker::new_with_maximum_delta_span(
                200, 600,
            )
            .unwrap();
        rolling.observe_counters_with_delta(baseline_at, baseline);
        physical.observe_counters_with_delta(baseline_at, baseline);

        let mut capture = BootstrapCaptureRuntime::new(Arc::new(AtomicBool::new(false)));
        capture.session.admit(&request, boot_ms).unwrap();
        capture.policy = Some(policy);
        capture.counter_sampler = Some(sampler);
        capture.counter_rates = Some(rolling);
        capture.counter_physical = Some(physical);
        capture.counter_request = Some(request.clone());
        capture.counter_epoch = epoch;
        capture.pending_counter_fence = Some(CounterReadFence {
            request: request.clone(),
            epoch,
            scheduled_at,
            purpose: LoadedCounterReadPurpose::Evidence,
        });
        capture.loaded_counter_cycle = LoadedCounterCycleState::BaselineReady;

        let mut authority = FakeCaptureAuthority {
            fail_capture: true,
            ..FakeCaptureAuthority::default()
        };
        let failure = (0..10_000)
            .find_map(|_| {
                match capture.accept_counter_completion(&store, &mut authority, &request) {
                    Ok(None) => {
                        std::thread::yield_now();
                        None
                    }
                    Ok(Some(_)) => panic!("a failed post-attestation accepted evidence"),
                    Err(error) => Some(error),
                }
            })
            .expect("counter worker did not complete without a timer");
        assert_eq!(failure.0, "capture-runtime-invalid");
        assert_eq!(authority.capture_attestations, 1);
        assert!(capture.loaded_physical_deltas.is_empty());
        assert!(capture.loaded_transport_authority.is_none());
        assert!(capture.pending_transport_completion.is_none());
        assert_eq!(
            capture.loaded_counter_cycle,
            LoadedCounterCycleState::BaselineReady
        );
        let mut wake_count = 0_u64;
        let wake_read = unsafe {
            libc::read(
                wake.as_raw_fd(),
                (&mut wake_count as *mut u64).cast::<libc::c_void>(),
                std::mem::size_of::<u64>(),
            )
        };
        assert_eq!(wake_read, std::mem::size_of::<u64>() as isize);
        assert!(wake_count >= 1);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn durable_revocation_blocks_every_loaded_counter_read_before_dispatch() {
        let directory = private_dir("loaded-read-revocation");
        let store = RuntimeOverrideStore::open(&directory).unwrap();
        let boot_ms = monotonic_boot_ms().unwrap();
        let mut request = loaded_capture_request();
        request.deadline_boot_ms = boot_ms + 60_000;

        for state in [
            LoadedCounterCycleState::BaselineReady,
            LoadedCounterCycleState::NeedRebase,
            LoadedCounterCycleState::NeedFlightEvidence,
            LoadedCounterCycleState::FlightActive,
        ] {
            let mut capture = BootstrapCaptureRuntime::new(Arc::new(AtomicBool::new(false)));
            capture.session.admit(&request, boot_ms).unwrap();
            capture.counter_rates = Some(
                super::super::autotune_counter::AutotuneCounterRateTracker::new_with_maximum_delta_span(
                    200, 600,
                )
                .unwrap(),
            );
            capture.next_rate_sample = Instant::now();
            capture.loaded_counter_cycle = state;
            if state == LoadedCounterCycleState::FlightActive {
                let now = Instant::now();
                capture.pending_transport_completion = Some((
                    crate::AutotuneTransportFlight {
                        probe_id: 1,
                        key: crate::AutotuneTransportCaptureKey::new(&request, "route"),
                        expected_phase: (true, false),
                        control_valid: true,
                        submitted_at: now,
                        physical_delta_required: true,
                    },
                    BootstrapTransportCompletion {
                        probe_id: 1,
                        capture: request.clone(),
                        started_at: now,
                        completed_at: now,
                        outcome: Err(crate::transport_probe::TransportProbeFailure::other(
                            "synthetic pending result".to_string(),
                        )),
                    },
                ));
            }
            let mut authority = FakeCaptureAuthority {
                fail_loaded: true,
                ..FakeCaptureAuthority::default()
            };
            let failure = capture
                .schedule_loaded_counter(&directory, &store, &mut authority)
                .unwrap_err();
            assert_eq!(failure.0, "capture-runtime-invalid");
            assert_eq!(authority.loaded_dispatch_attestations, 1);
            assert!(capture.pending_counter_fence.is_none());
            assert_eq!(capture.loaded_counter_cycle, state);
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn loaded_counter_burst_collects_a_bounded_event_batch_then_yields() {
        let priming = LoadedCounterBurstState::begin();
        assert_eq!(
            priming,
            LoadedCounterBurstState::Priming {
                remaining_completions: MAX_LOADED_COUNTER_BURST_COMPLETIONS,
                remaining_flights: MAX_LOADED_TRANSPORT_FLIGHTS_PER_BURST,
            }
        );

        let priming = priming.after_reactor(true, true, false, false);
        assert_eq!(
            priming,
            LoadedCounterBurstState::Priming {
                remaining_completions: MAX_LOADED_COUNTER_BURST_COMPLETIONS - 1,
                remaining_flights: MAX_LOADED_TRANSPORT_FLIGHTS_PER_BURST,
            },
            "counter completions, not elapsed time, advance the bounded batch"
        );
        let bracketing = priming.after_reactor(true, false, false, true);
        assert!(bracketing.bracketing_flight());
        assert_eq!(
            bracketing.after_reactor(true, false, false, true),
            bracketing,
            "an in-flight probe must retain its post-counter settlement fence"
        );
        assert_eq!(
            priming.after_reactor(true, false, true, false),
            priming,
            "an exact pending counter read must retain its cycle identity"
        );
        assert_eq!(
            priming
                .after_reactor(true, false, true, false)
                .after_reactor(true, true, false, false),
            LoadedCounterBurstState::Priming {
                remaining_completions: MAX_LOADED_COUNTER_BURST_COMPLETIONS - 2,
                remaining_flights: MAX_LOADED_TRANSPORT_FLIGHTS_PER_BURST,
            },
            "the retained counter read settles exactly once and the event budget remains"
        );

        assert_eq!(
            bracketing.after_reactor(true, false, true, false),
            bracketing,
            "a post-flight counter read must retain the bracketing state"
        );
        let priming = bracketing.after_reactor(true, true, false, false);
        assert_eq!(
            priming,
            LoadedCounterBurstState::Priming {
                remaining_completions: MAX_LOADED_COUNTER_BURST_COMPLETIONS - 2,
                remaining_flights: MAX_LOADED_TRANSPORT_FLIGHTS_PER_BURST - 1,
            },
            "the post-flight endpoint remains inside the same bounded batch"
        );

        let last_flight = LoadedCounterBurstState::BracketingFlight {
            remaining_completions: 8,
            remaining_flights: 1,
        };
        assert_eq!(
            last_flight.after_reactor(true, true, false, false),
            LoadedCounterBurstState::Idle,
            "the fourth flight forces a CPU/ICMP/runtime fairness turn"
        );
        assert_eq!(
            LoadedCounterBurstState::Priming {
                remaining_completions: 1,
                remaining_flights: 3,
            }
            .after_reactor(true, true, false, false),
            LoadedCounterBurstState::Idle,
            "the completion budget is an independent hard bound"
        );
        assert_eq!(
            bracketing.after_reactor(false, false, false, true),
            LoadedCounterBurstState::Idle,
            "capture completion or rejection terminates the batch immediately"
        );
    }

    #[test]
    fn three_proven_loaded_flights_reach_the_transport_threshold() {
        let mut burst = LoadedCounterBurstState::begin();

        // The first VM-shaped interval is longer than the unchanged 600 ms
        // authority wall. It advances the event budget but intentionally
        // cannot dispatch a transport flight.  Wall-clock duration is not an
        // input to this state machine, so slow attestations cannot consume the
        // four-flight budget.
        burst = burst.after_reactor(true, true, false, false);
        assert!(matches!(burst, LoadedCounterBurstState::Priming { .. }));

        let mut transport_samples = 0_u32;
        let proven_flights = 3;
        for flight in 0..proven_flights {
            // A following short exact delta mints the single-use pre-flight
            // authority. The consecutive post-flight endpoint settles it.
            burst = burst.after_reactor(true, true, false, true);
            assert!(burst.bracketing_flight());

            burst = burst.after_reactor(true, true, false, false);
            transport_samples +=
                u32::try_from(crate::transport_probe::LOADED_AUTOTUNE_WS_STREAMS).unwrap();
            if flight + 1 < proven_flights {
                assert!(burst.active(), "fairness ran before the third flight");
            }
        }

        assert!(burst.active());
        let samples_per_flight =
            u32::try_from(crate::transport_probe::LOADED_AUTOTUNE_WS_STREAMS).unwrap();
        assert!(
            2 * samples_per_flight < super::super::full_autotune::MIN_AUTOTUNE_TRANSPORT_SAMPLES
        );
        assert_eq!(
            transport_samples,
            u32::try_from(super::super::full_autotune::MIN_AUTOTUNE_TRANSPORT_SAMPLES).unwrap()
        );
        assert!(
            transport_samples >= super::super::full_autotune::MIN_AUTOTUNE_TRANSPORT_SAMPLES,
            "three fully bracketed five-sample flights must satisfy the unchanged transport threshold"
        );
    }

    #[test]
    fn post_flight_physical_endpoint_chains_four_real_batches_before_fairness() {
        let directory = private_dir("post-flight-chain");
        let store = RuntimeOverrideStore::open(&directory).unwrap();
        let mut request = loaded_capture_request();
        let boot_ms = monotonic_boot_ms().unwrap();
        request.deadline_boot_ms = boot_ms + 60_000;
        let policy = super::super::autotune_capture_policy::AutotuneCapturePolicyId::StandardV2
            .expand()
            .unwrap();

        let raw_wake = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        assert!(raw_wake >= 0);
        let wake = Arc::new(unsafe { OwnedFd::from_raw_fd(raw_wake) });
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        let (result_tx, result_rx) = mpsc::channel();
        let transport = BootstrapTransportRuntime {
            request_tx: Some(request_tx),
            result_rx,
            worker: None,
            _wake: wake,
            control: crate::AutotuneTransportControl::default(),
            in_flight: None,
            next_probe_id: 1,
            last_started: None,
            last_readiness_block: None,
            idle_samples_ms: VecDeque::new(),
            idle_baseline_ms: Some(2.0),
        };
        let mut capture = BootstrapCaptureRuntime::new(Arc::new(AtomicBool::new(false)));
        capture.session.admit(&request, boot_ms).unwrap();
        capture.policy = Some(policy.clone());
        capture.transport = Some(transport);
        capture.reset_counter_identity(&request);
        capture.counter_epoch = 7;

        let pre_end = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .unwrap();
        let pre_start = pre_end.checked_sub(Duration::from_millis(200)).unwrap();
        let pre_delta = LoadedPhysicalDelta {
            delta: super::super::autotune_counter::AutotuneCounterDelta {
                download_bytes: 20_000_000,
                upload_bytes: 200_000,
                observed_start: pre_start,
                observed_end: pre_end,
                within_maximum_span: true,
            },
            phase: Some((true, false)),
        };
        capture.loaded_counter_cycle = LoadedCounterCycleState::BaselineReady;
        capture
            .apply_loaded_counter_update(
                &request,
                &policy,
                CounterPhaseUpdate {
                    observed_at: pre_end,
                    window: None,
                    physical_delta: Some(pre_delta),
                    topology_attestation: Some(LoadedTopologyAttestation {
                        request: request.clone(),
                        epoch: 7,
                        attested_at: pre_end,
                    }),
                    authority: None,
                    purpose: LoadedCounterReadPurpose::Evidence,
                    preserve_ack_credit: false,
                    diagnostic: LoadedCounterDiagnostic {
                        outcome: LoadedCounterDiagnosticOutcome::WindowAccumulating,
                        read_latency_ms: 1,
                        delta_span_ms: Some(200),
                        window_span_ms: None,
                        download_kbps: None,
                        upload_kbps: None,
                    },
                },
                Instant::now(),
            )
            .unwrap();

        let initial_rebase = Instant::now();
        capture
            .apply_loaded_counter_update(
                &request,
                &policy,
                CounterPhaseUpdate {
                    observed_at: initial_rebase,
                    window: None,
                    physical_delta: None,
                    topology_attestation: None,
                    authority: None,
                    purpose: LoadedCounterReadPurpose::Rebase,
                    preserve_ack_credit: true,
                    diagnostic: LoadedCounterDiagnostic {
                        outcome: LoadedCounterDiagnosticOutcome::Priming,
                        read_latency_ms: 1,
                        delta_span_ms: None,
                        window_span_ms: None,
                        download_kbps: None,
                        upload_kbps: None,
                    },
                },
                initial_rebase,
            )
            .unwrap();

        let initial_flight_evidence_end = loop {
            let now = Instant::now();
            if now > initial_rebase {
                break now;
            }
        };
        let initial_flight_evidence = LoadedPhysicalDelta {
            delta: super::super::autotune_counter::AutotuneCounterDelta {
                download_bytes: 20_000_000,
                upload_bytes: 200_000,
                observed_start: initial_rebase,
                observed_end: initial_flight_evidence_end,
                within_maximum_span: true,
            },
            phase: Some((true, false)),
        };
        let initial_authority = capture
            .loaded_topology_attestation
            .as_ref()
            .and_then(|attestation| {
                attestation.authorize(
                    &request,
                    7,
                    &initial_flight_evidence,
                    (true, false),
                    BootstrapTransportRuntime::dropout(&policy),
                )
            })
            .expect("the initial post-rebase evidence must be authorized");
        capture
            .apply_loaded_counter_update(
                &request,
                &policy,
                CounterPhaseUpdate {
                    observed_at: initial_flight_evidence_end,
                    window: None,
                    physical_delta: Some(initial_flight_evidence),
                    topology_attestation: None,
                    authority: Some(initial_authority),
                    purpose: LoadedCounterReadPurpose::FlightEvidence,
                    preserve_ack_credit: false,
                    diagnostic: LoadedCounterDiagnostic {
                        outcome: LoadedCounterDiagnosticOutcome::WindowAccumulating,
                        read_latency_ms: 1,
                        delta_span_ms: Some(bounded_duration_ms(
                            initial_flight_evidence_end.duration_since(initial_rebase),
                        )),
                        window_span_ms: None,
                        download_kbps: None,
                        upload_kbps: None,
                    },
                },
                initial_flight_evidence_end,
            )
            .unwrap();

        let mut authority = FakeCaptureAuthority::default();
        capture.loaded_counter_burst = LoadedCounterBurstState::begin();
        capture
            .try_schedule_transport(&directory, &store, &mut authority)
            .unwrap();
        capture.loaded_counter_burst = capture.loaded_counter_burst.after_reactor(
            true,
            true,
            false,
            capture.loaded_transport_work_active(),
        );
        assert!(capture.loaded_counter_burst.bracketing_flight());

        let mut physical_start = initial_flight_evidence_end;
        for flight_index in 0..MAX_LOADED_TRANSPORT_FLIGHTS_PER_BURST {
            let work = request_rx
                .try_recv()
                .expect("each post-flight endpoint must dispatch the next request immediately");
            let submitted_at = capture
                .transport
                .as_ref()
                .and_then(|transport| transport.in_flight.as_ref())
                .expect("scheduled request has no in-flight authority")
                .submitted_at;
            let completed_at = loop {
                let now = Instant::now();
                if now > submitted_at {
                    break now;
                }
            };
            result_tx
                .send(BootstrapTransportCompletion {
                    probe_id: work.probe_id,
                    capture: work.capture,
                    started_at: submitted_at,
                    completed_at,
                    outcome: Ok(crate::transport_probe::TransportProbeSample {
                        backend: crate::transport_probe::TransportProbeBackend::WebSocket,
                        endpoint: policy.transport_endpoint().to_string(),
                        rtt_ms: 10.0,
                        raw_samples_ms: vec![9.0, 10.0, 11.0, 12.0],
                        discarded_samples: 0,
                        server_processing_ms: 0.0,
                        trusted: true,
                        connection_reused: true,
                    }),
                })
                .unwrap();

            let post_end = loop {
                let now = Instant::now();
                if now > completed_at {
                    break now;
                }
            };
            let post_delta = LoadedPhysicalDelta {
                delta: super::super::autotune_counter::AutotuneCounterDelta {
                    download_bytes: 20_000_000,
                    upload_bytes: 200_000,
                    observed_start: physical_start,
                    observed_end: post_end,
                    within_maximum_span: true,
                },
                phase: Some((true, false)),
            };
            capture
                .apply_loaded_counter_update(
                    &request,
                    &policy,
                    CounterPhaseUpdate {
                        observed_at: post_end,
                        window: None,
                        physical_delta: Some(post_delta),
                        topology_attestation: Some(LoadedTopologyAttestation {
                            request: request.clone(),
                            epoch: 7,
                            attested_at: post_end,
                        }),
                        authority: None,
                        purpose: LoadedCounterReadPurpose::FlightPost,
                        preserve_ack_credit: false,
                        diagnostic: LoadedCounterDiagnostic {
                            outcome: LoadedCounterDiagnosticOutcome::WindowAccumulating,
                            read_latency_ms: 1,
                            delta_span_ms: Some(bounded_duration_ms(
                                post_end.duration_since(physical_start),
                            )),
                            window_span_ms: None,
                            download_kbps: None,
                            upload_kbps: None,
                        },
                    },
                    post_end,
                )
                .unwrap();
            physical_start = post_end;

            let burst_before = capture.loaded_counter_burst;
            capture
                .poll_transport(&directory, &store, &mut authority, false)
                .unwrap();
            assert_eq!(
                capture.session.snapshot().unwrap().transport_samples,
                u32::from(flight_index + 1) * 4,
                "settled WebSocket samples must be published through the real capture session"
            );
            capture
                .advance_loaded_counter_burst_after_poll(
                    &directory,
                    &store,
                    &mut authority,
                    burst_before,
                    true,
                )
                .unwrap();

            if flight_index + 1 < MAX_LOADED_TRANSPORT_FLIGHTS_PER_BURST {
                assert!(matches!(
                    capture.loaded_counter_burst,
                    LoadedCounterBurstState::Priming { .. }
                ));
                assert_eq!(
                    capture.loaded_counter_cycle,
                    LoadedCounterCycleState::NeedRebase
                );
                assert!(!capture.loaded_transport_work_active());

                let rebase_end = Instant::now();
                capture
                    .apply_loaded_counter_update(
                        &request,
                        &policy,
                        CounterPhaseUpdate {
                            observed_at: rebase_end,
                            window: None,
                            physical_delta: None,
                            topology_attestation: None,
                            authority: None,
                            purpose: LoadedCounterReadPurpose::Rebase,
                            preserve_ack_credit: true,
                            diagnostic: LoadedCounterDiagnostic {
                                outcome: LoadedCounterDiagnosticOutcome::Priming,
                                read_latency_ms: 1,
                                delta_span_ms: None,
                                window_span_ms: None,
                                download_kbps: None,
                                upload_kbps: None,
                            },
                        },
                        rebase_end,
                    )
                    .unwrap();
                let rebase_burst = capture.loaded_counter_burst;
                capture
                    .advance_loaded_counter_burst_after_poll(
                        &directory,
                        &store,
                        &mut authority,
                        rebase_burst,
                        true,
                    )
                    .unwrap();
                assert_eq!(
                    capture.loaded_counter_cycle,
                    LoadedCounterCycleState::NeedFlightEvidence
                );
                assert!(!capture.loaded_transport_work_active());

                let flight_evidence_end = loop {
                    let now = Instant::now();
                    if now > rebase_end {
                        break now;
                    }
                };
                let flight_evidence = LoadedPhysicalDelta {
                    delta: super::super::autotune_counter::AutotuneCounterDelta {
                        download_bytes: 20_000_000,
                        upload_bytes: 200_000,
                        observed_start: rebase_end,
                        observed_end: flight_evidence_end,
                        within_maximum_span: true,
                    },
                    phase: Some((true, false)),
                };
                let flight_authority = capture
                    .loaded_topology_attestation
                    .as_ref()
                    .and_then(|attestation| {
                        attestation.authorize(
                            &request,
                            7,
                            &flight_evidence,
                            (true, false),
                            BootstrapTransportRuntime::dropout(&policy),
                        )
                    })
                    .expect("the next post-rebase evidence must be authorized");
                capture
                    .apply_loaded_counter_update(
                        &request,
                        &policy,
                        CounterPhaseUpdate {
                            observed_at: flight_evidence_end,
                            window: None,
                            physical_delta: Some(flight_evidence),
                            topology_attestation: None,
                            authority: Some(flight_authority),
                            purpose: LoadedCounterReadPurpose::FlightEvidence,
                            preserve_ack_credit: false,
                            diagnostic: LoadedCounterDiagnostic {
                                outcome: LoadedCounterDiagnosticOutcome::WindowAccumulating,
                                read_latency_ms: 1,
                                delta_span_ms: Some(bounded_duration_ms(
                                    flight_evidence_end.duration_since(rebase_end),
                                )),
                                window_span_ms: None,
                                download_kbps: None,
                                upload_kbps: None,
                            },
                        },
                        flight_evidence_end,
                    )
                    .unwrap();
                let flight_evidence_burst = capture.loaded_counter_burst;
                capture
                    .advance_loaded_counter_burst_after_poll(
                        &directory,
                        &store,
                        &mut authority,
                        flight_evidence_burst,
                        true,
                    )
                    .unwrap();
                physical_start = flight_evidence_end;
                assert!(capture.loaded_counter_burst.bracketing_flight());
                assert!(capture.loaded_transport_work_active());
            }
        }

        assert_eq!(capture.loaded_counter_burst, LoadedCounterBurstState::Idle);
        assert!(!capture.loaded_transport_work_active());
        assert!(capture.loaded_transport_authority.is_none());
        assert_eq!(authority.capture_attestations, 4);
        assert_eq!(authority.loaded_dispatch_attestations, 4);
        assert!(matches!(request_rx.try_recv(), Err(TryRecvError::Empty)));
        assert_eq!(
            capture.session.snapshot().unwrap().transport_samples,
            16,
            "the unchanged 15-sample threshold must be crossed before fairness"
        );

        drop(capture);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn invalid_physical_settlement_diagnostic_is_bounded_and_explains_coverage() {
        let diagnostic = invalid_loaded_transport_settlement_diagnostic(
            crate::AutotuneTransportAttestation {
                valid: false,
                reason: "physical-flight-coverage-insufficient",
                coverage: Some(crate::AutotuneTransportCoverage {
                    total: Duration::from_millis(400),
                    matching: Duration::from_millis(250),
                    longest_mismatch: Duration::from_millis(150),
                }),
            },
            (true, false),
        )
        .unwrap();
        assert_eq!(
            diagnostic,
            "reason=physical-flight-coverage-insufficient total_ms=400 matching_ms=250 longest_mismatch_ms=150 expected_phase=1:0"
        );
        assert!(invalid_loaded_transport_settlement_diagnostic(
            crate::AutotuneTransportAttestation {
                valid: true,
                reason: "physical-loaded-flight-valid",
                coverage: None,
            },
            (true, false),
        )
        .is_none());

        let backend_failure = crate::transport_probe::TransportProbeFailure::other(
            "backend detail must not enter the bounded daemon diagnostic\n".repeat(1_024),
        );
        assert_eq!(
            loaded_transport_backend_failure_diagnostic(42, &backend_failure),
            "reason=backend-failure probe_id=42 kind=other"
        );

        let mut capture = BootstrapCaptureRuntime::new(Arc::new(AtomicBool::new(true)));
        let loaded = loaded_capture_request();
        capture.reset_counter_identity(&loaded);
        assert_eq!(
            capture.loaded_transport_diagnostic_budget,
            MAX_LOADED_TRANSPORT_DIAGNOSTICS
        );
        capture.loaded_transport_diagnostic_budget = 0;
        capture.reset_counter_identity(&loaded);
        assert_eq!(
            capture.loaded_transport_diagnostic_budget, 0,
            "polling the same request must not refill a bounded diagnostic budget"
        );
    }

    #[test]
    fn backend_failure_consumes_one_bounded_diagnostic_and_no_sample_authority() {
        let directory = private_dir("loaded-backend-failure-diagnostic");
        let store = RuntimeOverrideStore::open(&directory).unwrap();
        let boot_ms = monotonic_boot_ms().unwrap();
        let mut request = loaded_capture_request();
        request.deadline_boot_ms = boot_ms + 60_000;
        let policy = super::super::autotune_capture_policy::AutotuneCapturePolicyId::StandardV2
            .expand()
            .unwrap();
        let raw_wake = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        assert!(raw_wake >= 0);
        let wake = Arc::new(unsafe { OwnedFd::from_raw_fd(raw_wake) });
        let (request_tx, _request_rx) = mpsc::sync_channel(1);
        let (_result_tx, result_rx) = mpsc::channel();
        let transport = BootstrapTransportRuntime {
            request_tx: Some(request_tx),
            result_rx,
            worker: None,
            _wake: wake,
            control: crate::AutotuneTransportControl::default(),
            in_flight: None,
            next_probe_id: 2,
            last_started: None,
            last_readiness_block: None,
            idle_samples_ms: VecDeque::new(),
            idle_baseline_ms: Some(2.0),
        };
        let now = Instant::now();
        let mut capture = BootstrapCaptureRuntime::new(Arc::new(AtomicBool::new(false)));
        capture.session.admit(&request, boot_ms).unwrap();
        capture.policy = Some(policy);
        capture.transport = Some(transport);
        capture.loaded_transport_diagnostic_budget = 1;
        capture.pending_transport_completion = Some((
            crate::AutotuneTransportFlight {
                probe_id: 1,
                key: crate::AutotuneTransportCaptureKey::new(&request, &request.route_fingerprint),
                expected_phase: (true, false),
                control_valid: true,
                submitted_at: now,
                physical_delta_required: false,
            },
            BootstrapTransportCompletion {
                probe_id: 1,
                capture: request,
                started_at: now,
                completed_at: now + Duration::from_millis(1),
                outcome: Err(crate::transport_probe::TransportProbeFailure::other(
                    "synthetic backend failure with untrusted detail".repeat(1_024),
                )),
            },
        ));
        let mut authority = FakeCaptureAuthority::default();
        capture
            .poll_transport(&directory, &store, &mut authority, false)
            .unwrap();
        assert_eq!(capture.loaded_transport_diagnostic_budget, 0);
        assert_eq!(capture.session.snapshot().unwrap().transport_samples, 0);
        assert!(capture.pending_transport_completion.is_none());
        assert_eq!(authority.capture_attestations, 0);

        drop(capture);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn loaded_transport_durable_authority_is_revoked_before_dispatch() {
        let (operation, request, permit, control, ack, checkpoint) =
            loaded_transport_authority_fixture();
        validate_loaded_transport_durable_authority(
            &operation,
            &request,
            &permit,
            &control,
            &ack,
            &checkpoint,
            None,
            5_000,
        )
        .unwrap();

        assert!(validate_loaded_transport_durable_authority(
            &operation,
            &request,
            &permit,
            &control,
            &ack,
            &checkpoint,
            Some(&control),
            5_000,
        )
        .unwrap_err()
        .contains("revoked"));

        let mut changed_request = request.clone();
        changed_request.control_sequence += 1;
        assert!(validate_loaded_transport_durable_authority(
            &operation,
            &changed_request,
            &permit,
            &control,
            &ack,
            &checkpoint,
            None,
            5_000,
        )
        .unwrap_err()
        .contains("does not match"));

        let inactive = checkpoint
            .advance_temporary_stage(TemporaryTopologyStage::TemporaryAbsent, None)
            .unwrap();
        assert!(validate_loaded_transport_durable_authority(
            &operation, &request, &permit, &control, &ack, &inactive, None, 5_000,
        )
        .unwrap_err()
        .contains("active private topology"));
    }

    #[test]
    fn pending_counter_completion_owns_the_reactor_wake_without_rate_spin() {
        let rate_due = Duration::ZERO;
        let cpu_due = Duration::from_millis(250);
        assert_eq!(loaded_reactor_deadline(rate_due, cpu_due, false), rate_due);
        assert_eq!(loaded_reactor_deadline(rate_due, cpu_due, true), cpu_due);
    }

    #[test]
    fn loaded_counter_diagnostic_distinguishes_every_nonfatal_window_state() {
        let completed_at = Instant::now();
        let started_at = completed_at - Duration::from_millis(250);
        let fence = CounterReadFence {
            request: loaded_capture_request(),
            epoch: 7,
            scheduled_at: started_at,
            purpose: LoadedCounterReadPurpose::Evidence,
        };
        let empty = super::super::autotune_counter::AutotuneCounterObservation {
            rate_window: None,
            delta: None,
        };
        assert_eq!(
            loaded_counter_diagnostic(&fence, completed_at, false, empty, false).outcome,
            LoadedCounterDiagnosticOutcome::CountersMissing
        );
        assert_eq!(
            loaded_counter_diagnostic(&fence, completed_at, true, empty, false).outcome,
            LoadedCounterDiagnosticOutcome::Priming
        );

        let mut stale = counter_observation(started_at, completed_at, 1_000_000, 1_000, None);
        stale.delta.as_mut().unwrap().within_maximum_span = false;
        let diagnostic = loaded_counter_diagnostic(&fence, completed_at, true, stale, false);
        assert_eq!(
            diagnostic.outcome,
            LoadedCounterDiagnosticOutcome::DeltaStale
        );
        assert_eq!(diagnostic.read_latency_ms, 250);
        assert_eq!(diagnostic.delta_span_ms, Some(250));

        let accumulating = counter_observation(started_at, completed_at, 1_000_000, 1_000, None);
        assert_eq!(
            loaded_counter_diagnostic(&fence, completed_at, true, accumulating, false).outcome,
            LoadedCounterDiagnosticOutcome::WindowAccumulating
        );

        let accepted = counter_observation(
            started_at,
            completed_at,
            1_000_000,
            1_000,
            Some((32_000.4, 31.6)),
        );
        let diagnostic = loaded_counter_diagnostic(&fence, completed_at, true, accepted, true);
        assert_eq!(
            diagnostic.outcome,
            LoadedCounterDiagnosticOutcome::WindowAccepted
        );
        assert_eq!(diagnostic.window_span_ms, Some(250));
        assert_eq!(diagnostic.download_kbps, Some(32_000));
        assert_eq!(diagnostic.upload_kbps, Some(32));
    }

    #[test]
    fn loaded_counter_diagnostic_budget_is_reset_only_for_a_loaded_identity() {
        let probes_stopped = Arc::new(AtomicBool::new(false));
        let mut runtime = BootstrapCaptureRuntime::new(probes_stopped);
        runtime.reset_counter_identity(&idle_capture_request());
        assert_eq!(runtime.loaded_counter_diagnostic_budget, 0);
        runtime.reset_counter_identity(&loaded_capture_request());
        assert_eq!(
            runtime.loaded_counter_diagnostic_budget,
            MAX_LOADED_PHASE_WINDOWS
        );
        runtime.loaded_counter_diagnostic_budget = 1;
        runtime.reset_counter_identity(&loaded_capture_request());
        assert_eq!(
            runtime.loaded_counter_diagnostic_budget, 1,
            "polling the same request must not refill an exhausted log budget"
        );
    }

    #[test]
    fn vm_proven_delta_span_does_not_widen_endpoint_or_transport_authority() {
        let capture = loaded_capture_request();
        let policy = super::super::autotune_capture_policy::AutotuneCapturePolicyId::StandardV2
            .expand()
            .unwrap();
        let started = Instant::now();
        let completed_at = started + Duration::from_millis(840);
        let scheduled_at = completed_at - Duration::from_millis(36);
        let attested_at = completed_at + Duration::from_millis(36);
        let mut rates =
            super::super::autotune_counter::AutotuneCounterRateTracker::new_with_maximum_delta_span(
                u64::from(policy.rate_sample_interval_ms()),
                u64::from(policy.maximum_counter_delta_span_ms()),
            )
            .unwrap();
        let initial = rates.observe_counters_with_delta(
            started,
            Some(super::super::speedtest::SpeedtestTrafficCounters {
                rx_bytes: 0,
                tx_bytes: 0,
            }),
        );
        assert!(initial.delta.is_none());
        let observation = rates.observe_counters_with_delta(
            completed_at,
            Some(super::super::speedtest::SpeedtestTrafficCounters {
                rx_bytes: 40_000_000,
                tx_bytes: 40_000,
            }),
        );
        assert!(observation.delta.unwrap().within_maximum_span);
        let rate_window = observation
            .rate_window
            .expect("the VM-proven span must complete a bounded rate window");
        assert_eq!(rate_window.observed_start, started);
        assert_eq!(rate_window.observed_end, completed_at);

        let fence = CounterReadFence {
            request: capture.clone(),
            epoch: 7,
            scheduled_at,
            purpose: LoadedCounterReadPurpose::Evidence,
        };
        let completion = super::super::autotune_counter::AutotuneCounterCompletion {
            request: capture.clone(),
            epoch: 7,
            completed_at,
            completed_boot_ms: capture.deadline_boot_ms,
            outcome: Ok(Some(super::super::speedtest::SpeedtestTrafficCounters {
                rx_bytes: 40_000_000,
                tx_bytes: 40_000,
            })),
        };
        let dispatch_freshness = BootstrapTransportRuntime::dropout(&policy);
        assert_eq!(dispatch_freshness, Duration::from_millis(600));
        assert_eq!(
            counter_completion_is_current(&capture, 7, &fence, &completion, dispatch_freshness,),
            Ok(CounterCompletionStatus::Current)
        );

        let mut ack_credit = None;
        let (window, physical, _) =
            loaded_phase_window_with_ack_credit(&capture, observation, &policy, 7, &mut ack_credit)
                .unwrap();
        let window = window.expect("the bounded delta must produce loaded phase authority");
        let physical = physical.expect("the bounded delta must preserve physical authority");
        assert_eq!(window.phase, (true, false));
        assert_eq!(physical.phase, None);
        let authority = LoadedTransportAuthority {
            request: capture.clone(),
            epoch: 7,
            attested_at,
            observed_start: physical.delta.observed_start,
            observed_end: physical.delta.observed_end,
        };
        assert!(!authority.permits(
            &capture,
            7,
            Some(&physical),
            attested_at,
            dispatch_freshness,
        ), "an 840 ms rolling window may prove load but cannot widen the 600 ms physical dispatch authority");

        let burst_completed_at = completed_at + Duration::from_millis(200);
        let burst_attested_at = completed_at - Duration::from_millis(1);
        let burst_observation = rates.observe_counters_with_delta(
            burst_completed_at,
            Some(super::super::speedtest::SpeedtestTrafficCounters {
                rx_bytes: 50_000_000,
                tx_bytes: 50_000,
            }),
        );
        let (_, burst_physical, _) = loaded_phase_window_with_ack_credit(
            &capture,
            burst_observation,
            &policy,
            7,
            &mut ack_credit,
        )
        .unwrap();
        let burst_physical =
            burst_physical.expect("the event-driven follow-up must preserve its physical delta");
        assert_eq!(burst_physical.phase, Some((true, false)));
        let burst_authority = LoadedTransportAuthority {
            request: capture.clone(),
            epoch: 7,
            attested_at: burst_attested_at,
            observed_start: burst_physical.delta.observed_start,
            observed_end: burst_physical.delta.observed_end,
        };
        assert!(
            burst_authority.permits(
                &capture,
                7,
                Some(&burst_physical),
                burst_completed_at,
                dispatch_freshness,
            ),
            "a 200 ms follow-up endpoint may authorize without weakening the 600 ms wall"
        );
    }

    #[test]
    fn bounded_pinger_drain_rearms_unread_channel_entries() {
        let raw_wake = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        assert!(raw_wake >= 0);
        let wake = Arc::new(unsafe { OwnedFd::from_raw_fd(raw_wake) });
        let (tx, lines) = mpsc::channel();
        for sequence in 0..=MAX_PINGER_LINES_PER_DRAIN {
            tx.send(Ok(crate::PingerLine {
                line: format!("192.0.2.1 : [0], 64 bytes, 1.{sequence} ms"),
                reflector: String::new(),
                observed_at: Instant::now(),
                observed_epoch_secs: 1.0,
            }))
            .unwrap();
        }
        let pinger = crate::PingerRuntime {
            children: Vec::new(),
            readers: Vec::new(),
            lines,
            wake: Some(wake),
        };
        BootstrapCaptureRuntime::rearm_pinger_wake(&pinger).unwrap();
        pinger.drain_wake().unwrap();

        let first = BootstrapCaptureRuntime::read_bounded_pinger_lines(&pinger).unwrap();
        assert_eq!(first.len(), MAX_PINGER_LINES_PER_DRAIN);
        let mut descriptor = libc::pollfd {
            fd: pinger.wake_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 0) }, 1);
        assert_ne!(descriptor.revents & libc::POLLIN, 0);

        pinger.drain_wake().unwrap();
        let second = BootstrapCaptureRuntime::read_bounded_pinger_lines(&pinger).unwrap();
        assert_eq!(second.len(), 1);
        assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 0) }, 0);
        drop(tx);
    }

    #[test]
    fn loaded_pending_bound_is_checked_after_expired_samples_are_removed() {
        let probes_stopped = Arc::new(AtomicBool::new(false));
        let mut runtime = BootstrapCaptureRuntime::new(probes_stopped);
        let request = loaded_capture_request();
        let now = Instant::now();
        let expired = now.checked_sub(MAX_PENDING_ICMP_WAIT * 2).unwrap();
        for _ in 0..MAX_PENDING_ICMP_SAMPLES {
            runtime.pending_icmp_samples.push_back(PendingIcmpSample {
                sample: icmp_sample(10.0),
                observed_at: expired,
            });
        }
        let observations = runtime
            .classify_loaded_icmp_samples(
                &request,
                std::iter::once(PendingIcmpSample {
                    sample: icmp_sample(11.0),
                    observed_at: now,
                }),
                now,
            )
            .unwrap();
        assert!(observations.is_empty());
        assert_eq!(runtime.pending_icmp_samples.len(), 1);

        runtime.pending_icmp_samples.clear();
        for _ in 0..MAX_PENDING_ICMP_SAMPLES {
            runtime.pending_icmp_samples.push_back(PendingIcmpSample {
                sample: icmp_sample(10.0),
                observed_at: now,
            });
        }
        let error = runtime
            .classify_loaded_icmp_samples(
                &request,
                std::iter::once(PendingIcmpSample {
                    sample: icmp_sample(11.0),
                    observed_at: now,
                }),
                now,
            )
            .unwrap_err();
        assert!(error.contains("queue exceeded its bound"));
        assert!(runtime.pending_icmp_samples.is_empty());
    }

    #[test]
    fn one_attestation_authorizes_one_observation_batch() {
        let directory = private_dir("one-attestation-batch");
        let probes_stopped = Arc::new(AtomicBool::new(false));
        let mut runtime = BootstrapCaptureRuntime::new(probes_stopped);
        let mut request = loaded_capture_request();
        let boot_ms = monotonic_boot_ms().unwrap();
        request.deadline_boot_ms = boot_ms + 60_000;
        runtime.session.admit(&request, boot_ms).unwrap();
        runtime.idle_rate_reference = Some((100_000, 50_000));
        let calls = Cell::new(0_u32);

        let snapshot = runtime
            .record_observations_with_attestor(
                &directory,
                &request,
                [
                    super::super::autotune_capture::AutotuneCaptureObservationKind::Cpu {
                        milli_percent: 1_000,
                    },
                ],
                |_, _| {
                    calls.set(calls.get() + 1);
                    Ok((100_000, 50_000))
                },
            )
            .unwrap();
        assert_eq!(calls.get(), 1);
        assert_eq!(snapshot.cpu_samples, 1);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn loaded_transport_reconstructs_overlapping_counter_windows_before_scheduling() {
        let capture = loaded_capture_request();
        let policy = request().capture_policy.unwrap().expand().unwrap();
        let dropout = BootstrapTransportRuntime::dropout(&policy);
        let history = Duration::from_secs(10);
        let now = Instant::now();
        let start = now.checked_sub(Duration::from_secs(4)).unwrap();
        let offsets = [(0, 1_000), (900, 1_900), (1_800, 2_800), (2_700, 3_700)];
        let mut windows = offsets
            .into_iter()
            .map(|(from, to)| LoadedPhaseWindow {
                request: capture.clone(),
                observed_start: start + Duration::from_millis(from),
                observed_end: start + Duration::from_millis(to),
                phase: (true, false),
            })
            .collect::<VecDeque<_>>();

        // This is the lossy point model used before r185: each asynchronous
        // completion is followed by the 600 ms freshness expiry before the
        // next roughly 900 ms completion.  It can never retain the 3 s hold.
        let key = crate::AutotuneTransportCaptureKey::new(&capture, &capture.route_fingerprint);
        let expected = crate::AutotuneTransportControl::expected_phase(&capture).unwrap();
        let mut point_control = crate::AutotuneTransportControl::default();
        for (index, window) in windows.iter().enumerate() {
            point_control.observe(
                key.clone(),
                Some(window.phase),
                expected,
                window.observed_end,
                dropout,
                history,
            );
            if index + 1 < windows.len() {
                point_control.observe(
                    key.clone(),
                    None,
                    expected,
                    window.observed_end + dropout + Duration::from_millis(1),
                    dropout,
                    history,
                );
            }
        }
        assert!(
            !point_control
                .ready_phase_diagnostic(
                    &capture,
                    &key,
                    now,
                    Duration::from_millis(u64::from(policy.transport_load_hold_ms())),
                    dropout,
                )
                .unwrap()
                .ready
        );

        let raw_wake = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        assert!(raw_wake >= 0);
        let wake = Arc::new(unsafe { OwnedFd::from_raw_fd(raw_wake) });
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        let (result_tx, result_rx) = mpsc::channel();
        let mut transport = BootstrapTransportRuntime {
            request_tx: Some(request_tx),
            result_rx,
            worker: None,
            _wake: wake,
            control: crate::AutotuneTransportControl::default(),
            in_flight: None,
            next_probe_id: 1,
            last_started: None,
            last_readiness_block: None,
            idle_samples_ms: VecDeque::new(),
            idle_baseline_ms: Some(2.0),
        };
        let timeline = loaded_phase_timeline(&capture, &windows);
        transport
            .replace_loaded_phase_history(&capture, &timeline, &VecDeque::new(), &policy)
            .unwrap();
        assert!(transport.readiness(&capture, &policy, now).unwrap().ready);
        assert_eq!(
            transport.try_schedule(&capture, &policy, now).unwrap(),
            BootstrapTransportScheduleDecision::Scheduled,
            "the counter-window event, not a timer, authorizes the loaded probe"
        );
        let first = request_rx.try_recv().unwrap();
        assert_eq!(first.capture, capture);
        result_tx
            .send(BootstrapTransportCompletion {
                probe_id: first.probe_id,
                capture: first.capture,
                started_at: now + Duration::from_millis(10),
                completed_at: now + Duration::from_millis(100),
                outcome: Ok(crate::transport_probe::TransportProbeSample {
                    backend: crate::transport_probe::TransportProbeBackend::WebSocket,
                    endpoint: policy.transport_endpoint().to_string(),
                    rtt_ms: 10.0,
                    raw_samples_ms: vec![9.0, 10.0, 11.0, 12.0],
                    discarded_samples: 0,
                    server_processing_ms: 0.0,
                    trusted: true,
                    connection_reused: true,
                }),
            })
            .unwrap();
        assert!(transport.try_take().unwrap().is_some());

        // A new exact counter window is the next physical scheduling event.
        // It arrives well before the legacy one-second loaded cadence, yet it
        // must be able to authorize the next sequential flight immediately.
        let next_window_end = now + Duration::from_millis(200);
        windows.push_back(LoadedPhaseWindow {
            request: capture.clone(),
            observed_start: now - Duration::from_millis(800),
            observed_end: next_window_end,
            phase: (true, false),
        });
        let timeline = loaded_phase_timeline(&capture, &windows);
        transport
            .replace_loaded_phase_history(&capture, &timeline, &VecDeque::new(), &policy)
            .unwrap();
        assert_eq!(
            transport
                .next_schedule_wake(&capture, &policy, next_window_end)
                .unwrap(),
            None,
            "loaded counter and completion events, not a cadence timer, wake scheduling"
        );
        assert_eq!(
            transport
                .try_schedule(&capture, &policy, next_window_end)
                .unwrap(),
            BootstrapTransportScheduleDecision::Scheduled
        );
        assert_eq!(request_rx.try_recv().unwrap().capture, capture);
    }

    #[test]
    fn bursty_loaded_transport_defers_result_until_physical_delta_brackets_flight() {
        let capture = loaded_capture_request();
        let policy = request().capture_policy.unwrap().expand().unwrap();
        let dropout = BootstrapTransportRuntime::dropout(&policy);
        let start = Instant::now();
        let first_end = start + Duration::from_millis(800);
        let windows = VecDeque::from([LoadedPhaseWindow {
            request: capture.clone(),
            observed_start: start,
            observed_end: first_end,
            phase: (true, false),
        }]);
        let timeline = loaded_phase_timeline(&capture, &windows);
        let mut physical = VecDeque::from([LoadedPhysicalDelta {
            delta: super::super::autotune_counter::AutotuneCounterDelta {
                download_bytes: 100_000_000,
                upload_bytes: 1_000_000,
                observed_start: start,
                observed_end: first_end,
                within_maximum_span: true,
            },
            phase: Some((true, false)),
        }]);
        let raw_wake = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        assert!(raw_wake >= 0);
        let wake = Arc::new(unsafe { OwnedFd::from_raw_fd(raw_wake) });
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        let (result_tx, result_rx) = mpsc::channel();
        let mut transport = BootstrapTransportRuntime {
            request_tx: Some(request_tx),
            result_rx,
            worker: None,
            _wake: wake,
            control: crate::AutotuneTransportControl::default(),
            in_flight: None,
            next_probe_id: 1,
            last_started: None,
            last_readiness_block: None,
            idle_samples_ms: VecDeque::new(),
            idle_baseline_ms: Some(2.0),
        };
        transport
            .replace_loaded_phase_history(&capture, &timeline, &physical, &policy)
            .unwrap();
        let readiness = transport.readiness(&capture, &policy, first_end).unwrap();
        assert!(readiness.ready);
        assert_eq!(readiness.reason, "loaded-physical-delta-ready");
        assert_eq!(
            transport
                .try_schedule(&capture, &policy, first_end)
                .unwrap(),
            BootstrapTransportScheduleDecision::Scheduled
        );
        let work = request_rx.try_recv().unwrap();
        let started_at = first_end + Duration::from_millis(10);
        let completed_at = first_end + Duration::from_millis(200);
        result_tx
            .send(BootstrapTransportCompletion {
                probe_id: work.probe_id,
                capture: work.capture,
                started_at,
                completed_at,
                outcome: Ok(crate::transport_probe::TransportProbeSample {
                    backend: crate::transport_probe::TransportProbeBackend::WebSocket,
                    endpoint: policy.transport_endpoint().to_string(),
                    rtt_ms: 10.0,
                    raw_samples_ms: vec![9.0, 10.0, 11.0, 12.0],
                    discarded_samples: 0,
                    server_processing_ms: 0.0,
                    trusted: true,
                    connection_reused: true,
                }),
            })
            .unwrap();
        let (flight, completion) = transport.try_take().unwrap().unwrap();
        assert!(matches!(
            transport.control.settle_physical_interval_diagnostic(
                &flight,
                completion.probe_id,
                completion.started_at,
                completion.completed_at,
                dropout,
            ),
            crate::AutotuneTransportSettlement::Pending("physical-bracket-pending")
        ));

        let next = LoadedPhysicalDelta {
            delta: super::super::autotune_counter::AutotuneCounterDelta {
                download_bytes: 40_000_000,
                upload_bytes: 400_000,
                observed_start: first_end,
                observed_end: first_end + Duration::from_millis(250),
                within_maximum_span: true,
            },
            phase: Some((true, false)),
        };
        physical.push_back(next);
        transport
            .append_loaded_physical_delta(&capture, next, &policy)
            .unwrap();
        let crate::AutotuneTransportSettlement::Final(attestation) =
            transport.control.settle_physical_interval_diagnostic(
                &flight,
                completion.probe_id,
                completion.started_at,
                completion.completed_at,
                dropout,
            )
        else {
            panic!("bracketed bootstrap transport result remained pending");
        };
        assert!(attestation.valid);
        assert_eq!(attestation.reason, "physical-loaded-flight-valid");
    }

    #[test]
    fn loaded_transport_timeline_preserves_real_gaps_and_direction_mismatches() {
        let capture = loaded_capture_request();
        let policy = request().capture_policy.unwrap().expand().unwrap();
        let now = Instant::now();
        let start = now.checked_sub(Duration::from_secs(4)).unwrap();
        let windows = VecDeque::from([
            LoadedPhaseWindow {
                request: capture.clone(),
                observed_start: start,
                observed_end: start + Duration::from_secs(1),
                phase: (true, false),
            },
            LoadedPhaseWindow {
                request: capture.clone(),
                observed_start: start + Duration::from_secs(2),
                observed_end: start + Duration::from_secs(3),
                phase: (true, true),
            },
            LoadedPhaseWindow {
                request: capture.clone(),
                observed_start: start + Duration::from_secs(3),
                observed_end: now,
                phase: (true, false),
            },
        ]);
        let timeline = loaded_phase_timeline(&capture, &windows);
        assert!(timeline
            .iter()
            .any(|observation| observation.phase.is_none()));
        assert!(timeline
            .iter()
            .any(|observation| observation.phase == Some((true, true))));

        let raw_wake = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        assert!(raw_wake >= 0);
        let wake = Arc::new(unsafe { OwnedFd::from_raw_fd(raw_wake) });
        let (request_tx, _request_rx) = mpsc::sync_channel(1);
        let (_result_tx, result_rx) = mpsc::channel();
        let mut transport = BootstrapTransportRuntime {
            request_tx: Some(request_tx),
            result_rx,
            worker: None,
            _wake: wake,
            control: crate::AutotuneTransportControl::default(),
            in_flight: None,
            next_probe_id: 1,
            last_started: None,
            last_readiness_block: None,
            idle_samples_ms: VecDeque::new(),
            idle_baseline_ms: Some(2.0),
        };
        transport
            .replace_loaded_phase_history(&capture, &timeline, &VecDeque::new(), &policy)
            .unwrap();
        assert!(
            !transport.readiness(&capture, &policy, now).unwrap().ready,
            "an uncovered interval and a reverse-loaded interval must break the hold"
        );
    }

    #[test]
    fn loaded_transport_timeline_keeps_only_evidence_bounded_microgaps() {
        let capture = loaded_capture_request();
        let policy = request().capture_policy.unwrap().expand().unwrap();
        let now = Instant::now();
        let start = now.checked_sub(Duration::from_secs(4)).unwrap();
        let windows = VecDeque::from([
            LoadedPhaseWindow {
                request: capture.clone(),
                observed_start: start,
                observed_end: start + Duration::from_secs(2),
                phase: (true, false),
            },
            LoadedPhaseWindow {
                request: capture.clone(),
                observed_start: start + Duration::from_millis(2_100),
                observed_end: now,
                phase: (true, false),
            },
        ]);
        let timeline = loaded_phase_timeline(&capture, &windows);
        assert!(timeline
            .iter()
            .any(|observation| observation.phase.is_none()));

        let key = crate::AutotuneTransportCaptureKey::new(&capture, &capture.route_fingerprint);
        let expected = crate::AutotuneTransportControl::expected_phase(&capture).unwrap();
        let dropout = BootstrapTransportRuntime::dropout(&policy);
        let mut control = crate::AutotuneTransportControl::default();
        for observation in timeline {
            control.observe(
                key.clone(),
                observation.phase,
                expected,
                observation.at,
                dropout,
                Duration::from_secs(10),
            );
        }
        let readiness = control
            .ready_phase_diagnostic(
                &capture,
                &key,
                now,
                Duration::from_millis(u64::from(policy.transport_load_hold_ms())),
                dropout,
            )
            .unwrap();
        assert!(readiness.ready);
        assert_eq!(readiness.phase, (true, false));
    }

    #[test]
    fn transport_reschedules_after_partial_success_and_transient_failure() {
        let raw_wake = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        assert!(raw_wake >= 0);
        let wake = Arc::new(unsafe { OwnedFd::from_raw_fd(raw_wake) });
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        let (result_tx, result_rx) = mpsc::channel();
        let mut transport = BootstrapTransportRuntime {
            request_tx: Some(request_tx),
            result_rx,
            worker: None,
            _wake: wake,
            control: crate::AutotuneTransportControl::default(),
            in_flight: None,
            next_probe_id: 1,
            last_started: None,
            last_readiness_block: None,
            idle_samples_ms: VecDeque::new(),
            idle_baseline_ms: None,
        };
        let capture = idle_capture_request();
        let policy = request().capture_policy.unwrap().expand().unwrap();
        let interval = BootstrapTransportRuntime::probe_interval(&capture, &policy);
        let mut now = Instant::now();

        transport.last_started = Some(now);
        transport.idle_samples_ms.push_back(99.0);
        transport.idle_baseline_ms = Some(99.0);
        transport.begin_request(&capture);
        assert!(transport.last_started.is_none());
        assert!(transport.idle_samples_ms.is_empty());
        assert!(transport.idle_baseline_ms.is_none());

        for batch in 0..3 {
            transport
                .observe_phase(&capture, Some((false, false)), &policy, now)
                .unwrap();
            assert_eq!(
                transport.try_schedule(&capture, &policy, now).unwrap(),
                BootstrapTransportScheduleDecision::Scheduled
            );
            let work = request_rx.try_recv().unwrap();
            assert_eq!(work.capture, capture);
            let samples = vec![
                10.0 + f64::from(batch),
                11.0 + f64::from(batch),
                12.0 + f64::from(batch),
                13.0 + f64::from(batch),
            ];
            result_tx
                .send(BootstrapTransportCompletion {
                    probe_id: work.probe_id,
                    capture: work.capture,
                    started_at: now + Duration::from_millis(1),
                    completed_at: now + Duration::from_millis(2),
                    outcome: Ok(crate::transport_probe::TransportProbeSample {
                        backend: crate::transport_probe::TransportProbeBackend::WebSocket,
                        endpoint: policy.transport_endpoint().to_string(),
                        rtt_ms: samples[1],
                        raw_samples_ms: samples.clone(),
                        discarded_samples: 0,
                        server_processing_ms: 0.0,
                        trusted: true,
                        connection_reused: batch > 0,
                    }),
                })
                .unwrap();
            let (_, completion) = transport.try_take().unwrap().unwrap();
            let sample = completion.outcome.unwrap();
            transport
                .observe_idle_samples(&sample.raw_samples_ms)
                .unwrap();
            now += interval;
        }
        assert_eq!(transport.idle_samples_ms.len(), 12);

        transport
            .observe_phase(&capture, Some((false, false)), &policy, now)
            .unwrap();
        assert_eq!(
            transport.try_schedule(&capture, &policy, now).unwrap(),
            BootstrapTransportScheduleDecision::Scheduled
        );
        let failed = request_rx.try_recv().unwrap();
        result_tx
            .send(BootstrapTransportCompletion {
                probe_id: failed.probe_id,
                capture: failed.capture,
                started_at: now + Duration::from_millis(1),
                completed_at: now + Duration::from_millis(2),
                outcome: Err(crate::transport_probe::TransportProbeFailure::other(
                    "transient connection failure".to_string(),
                )),
            })
            .unwrap();
        let (_, completion) = transport.try_take().unwrap().unwrap();
        assert_eq!(
            completion.outcome.unwrap_err().kind(),
            crate::transport_probe::TransportProbeFailureKind::Other
        );
        assert!(transport.in_flight.is_none());

        let retry_at = now + interval;
        assert_eq!(
            transport.try_schedule(&capture, &policy, retry_at).unwrap(),
            BootstrapTransportScheduleDecision::ReadinessBlocked("phase-observation-stale")
        );
        assert_eq!(
            transport
                .next_schedule_wake(&capture, &policy, retry_at)
                .unwrap(),
            None,
            "a cadence deadline must never manufacture stale phase authority"
        );
        assert!(request_rx.try_recv().is_err());

        transport
            .observe_phase(&capture, Some((false, false)), &policy, retry_at)
            .unwrap();
        assert_eq!(
            transport
                .next_schedule_wake(&capture, &policy, retry_at)
                .unwrap(),
            Some(Duration::ZERO)
        );
        assert_eq!(
            transport.try_schedule(&capture, &policy, retry_at).unwrap(),
            BootstrapTransportScheduleDecision::Scheduled
        );
        assert_eq!(request_rx.try_recv().unwrap().capture, capture);
    }

    #[test]
    fn probe_shutdown_blocker_is_ready_only_after_the_exact_fence() {
        let blocker = RuntimeRestoreBlocker::TopologySettling {
            target_interface: "pppoe-wan".to_string(),
            detail: PROBE_STOPPING_DETAIL.to_string(),
        };
        assert_eq!(
            observe_probe_stopping(&blocker, "pppoe-wan", false),
            Some(RuntimeRestoreObservation::Blocked(blocker.clone()))
        );
        assert_eq!(
            observe_probe_stopping(&blocker, "pppoe-wan", true),
            Some(RuntimeRestoreObservation::Ready)
        );
        assert_eq!(observe_probe_stopping(&blocker, "wanb", true), None);
    }

    #[test]
    fn loaded_phase_joins_the_observation_time_not_the_later_drain_time() {
        let capture_request = loaded_capture_request();
        capture_request.validate().unwrap();
        let policy = request().capture_policy.unwrap().expand().unwrap();
        let started = Instant::now();
        let first = loaded_phase_window(
            &capture_request,
            super::super::autotune_counter::AutotuneCounterRateWindow {
                download_kbps: 10_000.0,
                upload_kbps: 500.0,
                observed_start: started,
                observed_end: started + Duration::from_secs(1),
                fresh: true,
            },
            &policy,
        )
        .unwrap();
        let mut windows = VecDeque::from([first]);
        assert_eq!(
            loaded_phase_at(
                &capture_request,
                &windows,
                started + Duration::from_millis(500),
            ),
            LoadedPhaseLookup::Covered((true, false)),
            "a sample remains attributable even when the owner drains it much later"
        );
        assert_eq!(
            loaded_phase_at_with_pending_bound(
                &capture_request,
                &windows,
                started + Duration::from_millis(500),
                started + Duration::from_secs(10),
            ),
            LoadedPhaseLookup::Covered((true, false)),
            "elapsed time cannot revoke an already closed covering window"
        );
        assert_eq!(
            loaded_phase_at_with_pending_bound(
                &capture_request,
                &windows,
                started + Duration::from_millis(1_100),
                started + Duration::from_millis(1_200),
            ),
            LoadedPhaseLookup::Pending
        );
        assert_eq!(
            loaded_phase_at_with_pending_bound(
                &capture_request,
                &VecDeque::new(),
                started,
                started + MAX_PENDING_ICMP_WAIT + Duration::from_millis(1),
            ),
            LoadedPhaseLookup::Uncovered,
            "a timer can only revoke an unmatched sample, never create phase authority"
        );
        let mut second = windows.back().unwrap().clone();
        second.observed_start = started + Duration::from_secs(2);
        second.observed_end = started + Duration::from_secs(3);
        windows.push_back(second);
        assert_eq!(
            loaded_phase_at(
                &capture_request,
                &windows,
                started + Duration::from_millis(1_500),
            ),
            LoadedPhaseLookup::Uncovered
        );
        let mut foreign = capture_request.clone();
        foreign.capture_id = "aa".repeat(16);
        assert_eq!(
            loaded_phase_at(&foreign, &windows, started + Duration::from_millis(500)),
            LoadedPhaseLookup::Pending
        );
    }

    #[test]
    fn loaded_phase_overlap_is_positive_bounded_and_conflict_closed() {
        let capture = loaded_capture_request();
        let started = Instant::now();
        let window = |request: &AutotuneCaptureRequest,
                      start_ms: u64,
                      end_ms: u64,
                      phase: (bool, bool)| LoadedPhaseWindow {
            request: request.clone(),
            observed_start: started + Duration::from_millis(start_ms),
            observed_end: started + Duration::from_millis(end_ms),
            phase,
        };

        let active_then_idle = VecDeque::from([
            window(&capture, 0, 1_000, (true, false)),
            window(&capture, 800, 1_800, (false, false)),
        ]);
        assert_eq!(
            loaded_phase_at(
                &capture,
                &active_then_idle,
                started + Duration::from_millis(900),
            ),
            LoadedPhaseLookup::Covered((true, false)),
            "trailing below-threshold evidence cannot erase a covering active window"
        );
        assert_eq!(
            loaded_phase_at(
                &capture,
                &active_then_idle,
                started + Duration::from_millis(1_100),
            ),
            LoadedPhaseLookup::Covered((false, false)),
            "active authority cannot extend past its observed end"
        );

        let idle_only = VecDeque::from([
            window(&capture, 0, 1_000, (false, false)),
            window(&capture, 200, 1_200, (false, false)),
        ]);
        assert_eq!(
            loaded_phase_at(&capture, &idle_only, started + Duration::from_millis(500),),
            LoadedPhaseLookup::Covered((false, false))
        );

        let conflicting_active = VecDeque::from([
            window(&capture, 0, 1_000, (true, false)),
            window(&capture, 200, 1_200, (false, true)),
        ]);
        assert_eq!(
            loaded_phase_at(
                &capture,
                &conflicting_active,
                started + Duration::from_millis(500),
            ),
            LoadedPhaseLookup::Uncovered,
            "conflicting active directions must fail closed"
        );

        let agreeing_active = VecDeque::from([
            window(&capture, 0, 1_000, (true, false)),
            window(&capture, 200, 1_200, (true, false)),
        ]);
        assert_eq!(
            loaded_phase_at(
                &capture,
                &agreeing_active,
                started + Duration::from_millis(500),
            ),
            LoadedPhaseLookup::Covered((true, false))
        );

        let mut foreign = capture.clone();
        foreign.capture_id = "aa".repeat(16);
        let foreign_active = VecDeque::from([
            window(&foreign, 0, 1_000, (true, false)),
            window(&capture, 0, 1_000, (false, false)),
        ]);
        assert_eq!(
            loaded_phase_at(
                &capture,
                &foreign_active,
                started + Duration::from_millis(500),
            ),
            LoadedPhaseLookup::Covered((false, false)),
            "foreign positive evidence must not influence this request"
        );

        let real_gap = VecDeque::from([
            window(&capture, 0, 1_000, (true, false)),
            window(&capture, 2_000, 3_000, (true, false)),
        ]);
        assert_eq!(
            loaded_phase_at(&capture, &real_gap, started + Duration::from_millis(1_500),),
            LoadedPhaseLookup::Uncovered,
            "an uncovered interval remains a real gap"
        );
    }

    #[test]
    fn counter_completion_requires_exact_request_epoch_and_live_deadline() {
        let request = loaded_capture_request();
        let attested_at = Instant::now();
        let fence = CounterReadFence {
            request: request.clone(),
            epoch: 7,
            scheduled_at: attested_at,
            purpose: LoadedCounterReadPurpose::Evidence,
        };
        let freshness = Duration::from_millis(600);
        let mut completion = super::super::autotune_counter::AutotuneCounterCompletion {
            request: request.clone(),
            epoch: 7,
            completed_at: attested_at + Duration::from_millis(100),
            completed_boot_ms: request.deadline_boot_ms,
            outcome: Ok(None),
        };
        assert_eq!(
            counter_completion_is_current(&request, 7, &fence, &completion, freshness),
            Ok(CounterCompletionStatus::Current)
        );
        completion.epoch = 6;
        assert_eq!(
            counter_completion_is_current(&request, 7, &fence, &completion, freshness),
            Ok(CounterCompletionStatus::Stale)
        );
        completion.epoch = 8;
        assert_eq!(
            counter_completion_is_current(&request, 7, &fence, &completion, freshness),
            Err(CounterCompletionError::Identity)
        );
        completion.epoch = 7;
        completion.request.capture_id = "aa".repeat(16);
        assert_eq!(
            counter_completion_is_current(&request, 7, &fence, &completion, freshness),
            Err(CounterCompletionError::Identity)
        );
        completion.request = request.clone();
        completion.completed_boot_ms = request.deadline_boot_ms + 1;
        assert_eq!(
            counter_completion_is_current(&request, 7, &fence, &completion, freshness),
            Err(CounterCompletionError::Expired)
        );
        completion.completed_boot_ms = 0;
        assert_eq!(
            counter_completion_is_current(&request, 7, &fence, &completion, freshness),
            Err(CounterCompletionError::Expired)
        );
        completion.completed_boot_ms = request.deadline_boot_ms;
        completion.completed_at = attested_at + freshness + Duration::from_nanos(1);
        assert_eq!(
            counter_completion_is_current(&request, 7, &fence, &completion, freshness),
            Ok(CounterCompletionStatus::Stale),
            "a slow counter read cannot extend one exact attestation fence"
        );
        completion.completed_at = attested_at.checked_sub(Duration::from_nanos(1)).unwrap();
        assert_eq!(
            counter_completion_is_current(&request, 7, &fence, &completion, freshness),
            Err(CounterCompletionError::Identity),
            "a completion cannot predate the attestation which authorized it"
        );
    }

    #[test]
    fn loaded_transport_authority_is_one_fresh_post_rebase_physical_delta() {
        let request = loaded_capture_request();
        let observed_start = Instant::now();
        let observed_end = observed_start + Duration::from_millis(200);
        let attested_at = observed_start - Duration::from_millis(100);
        let freshness = Duration::from_millis(600);
        let delta = LoadedPhysicalDelta {
            delta: super::super::autotune_counter::AutotuneCounterDelta {
                download_bytes: 40_000_000,
                upload_bytes: 40_000,
                observed_start,
                observed_end,
                within_maximum_span: true,
            },
            phase: Some((true, false)),
        };
        let authority = LoadedTransportAuthority {
            request: request.clone(),
            epoch: 7,
            attested_at,
            observed_start,
            observed_end,
        };

        assert!(authority.permits(
            &request,
            7,
            Some(&delta),
            observed_end + Duration::from_millis(200),
            freshness,
        ));
        assert!(
            !authority.permits(
                &request,
                8,
                Some(&delta),
                observed_end + Duration::from_millis(200),
                freshness,
            ),
            "rotating the capture epoch revokes the fence"
        );
        assert!(
            !authority.permits(
                &request,
                7,
                Some(&delta),
                attested_at + freshness + Duration::from_nanos(1),
                freshness,
            ),
            "a historical physical delta cannot authorize a current flight"
        );
        let early_end = observed_end - Duration::from_millis(10);
        let early_delta = LoadedPhysicalDelta {
            delta: super::super::autotune_counter::AutotuneCounterDelta {
                observed_end: early_end,
                ..delta.delta
            },
            ..delta
        };
        let expired_fence = LoadedTransportAuthority {
            observed_end: early_end,
            ..authority.clone()
        };
        assert!(
            !expired_fence.permits(
                &request,
                7,
                Some(&early_delta),
                attested_at + freshness + Duration::from_nanos(1),
                freshness,
            ),
            "dispatch is bounded from topology attestation, not only from the counter endpoint"
        );
        let mut later = delta;
        later.delta.observed_end += Duration::from_millis(1);
        assert!(
            !authority.permits(&request, 7, Some(&later), attested_at, freshness,),
            "the fence is bound to the exact counter endpoint"
        );
        let impossible = LoadedTransportAuthority {
            attested_at: observed_start + Duration::from_nanos(1),
            ..authority
        };
        assert!(
            !impossible.permits(&request, 7, Some(&delta), observed_end, freshness),
            "an attestation after the counter interval began cannot prove that physical delta"
        );
    }

    #[test]
    fn topology_attestation_binds_request_epoch_phase_and_precedes_the_delta() {
        let request = loaded_capture_request();
        let attested_at = Instant::now();
        let observed_start = attested_at + Duration::from_millis(1);
        let delta = LoadedPhysicalDelta {
            delta: super::super::autotune_counter::AutotuneCounterDelta {
                download_bytes: 20_000_000,
                upload_bytes: 200_000,
                observed_start,
                observed_end: observed_start + Duration::from_millis(200),
                within_maximum_span: true,
            },
            phase: Some((true, false)),
        };
        let topology = LoadedTopologyAttestation {
            request: request.clone(),
            epoch: 7,
            attested_at,
        };
        let freshness = Duration::from_millis(600);
        assert!(topology
            .authorize(&request, 7, &delta, (true, false), freshness)
            .is_some());
        assert!(topology
            .authorize(&request, 8, &delta, (true, false), freshness)
            .is_none());
        let mut other_request = request.clone();
        other_request.capture_id = "aa".repeat(16);
        assert!(topology
            .authorize(&other_request, 7, &delta, (true, false), freshness)
            .is_none());
        assert!(topology
            .authorize(&request, 7, &delta, (false, true), freshness)
            .is_none());
        let late_topology = LoadedTopologyAttestation {
            attested_at: observed_start + Duration::from_nanos(1),
            ..topology.clone()
        };
        assert!(late_topology
            .authorize(&request, 7, &delta, (true, false), freshness)
            .is_none());
        let stale_delta = LoadedPhysicalDelta {
            delta: super::super::autotune_counter::AutotuneCounterDelta {
                observed_start: attested_at + freshness + Duration::from_nanos(1),
                observed_end: attested_at + freshness + Duration::from_millis(200),
                ..delta.delta
            },
            ..delta
        };
        assert!(topology
            .authorize(&request, 7, &stale_delta, (true, false), freshness)
            .is_none());
    }

    #[test]
    fn rolling_rate_window_may_start_before_its_exact_endpoint_attestation() {
        let request = loaded_capture_request();
        let started = Instant::now();
        let mut rates =
            super::super::autotune_counter::AutotuneCounterRateTracker::new_with_maximum_delta_span(
                200, 1_500,
            )
            .unwrap();
        assert!(rates
            .observe_counters(
                started,
                Some(super::super::speedtest::SpeedtestTrafficCounters {
                    rx_bytes: 0,
                    tx_bytes: 0,
                }),
            )
            .is_none());

        let attested_at = started + Duration::from_millis(790);
        let completed_at = started + Duration::from_millis(800);
        let fence = CounterReadFence {
            request: request.clone(),
            epoch: 7,
            scheduled_at: attested_at,
            purpose: LoadedCounterReadPurpose::Evidence,
        };
        let completion = super::super::autotune_counter::AutotuneCounterCompletion {
            request: request.clone(),
            epoch: 7,
            completed_at,
            completed_boot_ms: request.deadline_boot_ms,
            outcome: Ok(Some(super::super::speedtest::SpeedtestTrafficCounters {
                rx_bytes: 80_000_000,
                tx_bytes: 800_000,
            })),
        };
        assert_eq!(
            counter_completion_is_current(
                &request,
                7,
                &fence,
                &completion,
                Duration::from_millis(600),
            ),
            Ok(CounterCompletionStatus::Current)
        );
        let window = rates
            .observe_counters(completed_at, completion.outcome.unwrap())
            .expect("the attested endpoint should complete the physical 800 ms rate window");
        assert_eq!(window.observed_start, started);
        assert_eq!(window.observed_end, completed_at);
        assert!(window.observed_start < attested_at);
        assert!(loaded_phase_window(
            &request,
            window,
            &super::super::autotune_capture_policy::AutotuneCapturePolicyId::StandardV2
                .expand()
                .unwrap(),
        )
        .is_ok());
    }

    #[test]
    fn first_loaded_physical_delta_authorizes_before_a_rolling_window_exists() {
        let request = loaded_capture_request();
        let policy = super::super::autotune_capture_policy::AutotuneCapturePolicyId::StandardV2
            .expand()
            .unwrap();
        let started = Instant::now();
        let completed_at = started + Duration::from_millis(200);
        let attested_at = started - Duration::from_millis(10);
        let mut rates =
            super::super::autotune_counter::AutotuneCounterRateTracker::new_with_maximum_delta_span(
                200, 1_500,
            )
            .unwrap();
        let initial = rates.observe_counters_with_delta(
            started,
            Some(super::super::speedtest::SpeedtestTrafficCounters {
                rx_bytes: 0,
                tx_bytes: 0,
            }),
        );
        assert!(initial.rate_window.is_none());
        assert!(initial.delta.is_none());

        let observation = rates.observe_counters_with_delta(
            completed_at,
            Some(super::super::speedtest::SpeedtestTrafficCounters {
                rx_bytes: 20_000_000,
                tx_bytes: 200_000,
            }),
        );
        assert!(observation.rate_window.is_none());
        assert!(observation.delta.is_some());

        let mut ack_credit = None;
        let (window, physical, _) =
            loaded_phase_window_with_ack_credit(&request, observation, &policy, 7, &mut ack_credit)
                .unwrap();
        assert!(window.is_none());
        let physical = physical.expect("the first 200 ms delta must remain available");
        assert_eq!(physical.phase, Some((true, false)));

        let authority = LoadedTransportAuthority {
            request: request.clone(),
            epoch: 7,
            attested_at,
            observed_start: physical.delta.observed_start,
            observed_end: physical.delta.observed_end,
        };
        assert!(authority.permits(
            &request,
            7,
            Some(&physical),
            completed_at,
            BootstrapTransportRuntime::dropout(&policy),
        ));
    }

    #[test]
    fn flight_evidence_dispatches_once_and_requires_flight_post() {
        let mut request = loaded_capture_request();
        let boot_ms = monotonic_boot_ms().unwrap();
        request.deadline_boot_ms = boot_ms + 60_000;
        let policy = super::super::autotune_capture_policy::AutotuneCapturePolicyId::StandardV2
            .expand()
            .unwrap();
        let completed_at = Instant::now()
            .checked_sub(Duration::from_millis(10))
            .unwrap();
        let started = completed_at
            .checked_sub(Duration::from_millis(200))
            .unwrap();
        let physical = LoadedPhysicalDelta {
            delta: super::super::autotune_counter::AutotuneCounterDelta {
                download_bytes: 20_000_000,
                upload_bytes: 200_000,
                observed_start: started,
                observed_end: completed_at,
                within_maximum_span: true,
            },
            phase: Some((true, false)),
        };
        let raw_wake = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        assert!(raw_wake >= 0);
        let wake = Arc::new(unsafe { OwnedFd::from_raw_fd(raw_wake) });
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        let (_result_tx, result_rx) = mpsc::channel();
        let transport = BootstrapTransportRuntime {
            request_tx: Some(request_tx),
            result_rx,
            worker: None,
            _wake: wake,
            control: crate::AutotuneTransportControl::default(),
            in_flight: None,
            next_probe_id: 1,
            last_started: None,
            last_readiness_block: None,
            idle_samples_ms: VecDeque::new(),
            idle_baseline_ms: Some(2.0),
        };
        let mut capture = BootstrapCaptureRuntime::new(Arc::new(AtomicBool::new(true)));
        capture.session.admit(&request, boot_ms).unwrap();
        capture.policy = Some(policy.clone());
        capture.counter_epoch = 7;
        capture.transport = Some(transport);
        capture.loaded_counter_cycle = LoadedCounterCycleState::NeedFlightEvidence;
        capture
            .apply_loaded_counter_update(
                &request,
                &policy,
                CounterPhaseUpdate {
                    observed_at: completed_at,
                    window: None,
                    physical_delta: Some(physical),
                    topology_attestation: None,
                    authority: Some(LoadedTransportAuthority {
                        request: request.clone(),
                        epoch: 7,
                        attested_at: started - Duration::from_millis(10),
                        observed_start: started,
                        observed_end: completed_at,
                    }),
                    purpose: LoadedCounterReadPurpose::FlightEvidence,
                    preserve_ack_credit: false,
                    diagnostic: LoadedCounterDiagnostic {
                        outcome: LoadedCounterDiagnosticOutcome::WindowAccumulating,
                        read_latency_ms: 10,
                        delta_span_ms: Some(200),
                        window_span_ms: None,
                        download_kbps: None,
                        upload_kbps: None,
                    },
                },
                completed_at,
            )
            .unwrap();

        assert!(capture.loaded_phase_windows.is_empty());
        assert_eq!(capture.loaded_physical_deltas.len(), 1);
        let authority = capture
            .loaded_transport_authority
            .as_ref()
            .expect("the exact physical fence must survive without a rolling window");
        assert!(authority.permits(
            &request,
            7,
            capture.loaded_physical_deltas.back(),
            completed_at,
            BootstrapTransportRuntime::dropout(&policy),
        ));
        let readiness = capture
            .transport
            .as_ref()
            .unwrap()
            .readiness(&request, &policy, completed_at)
            .unwrap();
        assert_eq!(readiness.reason, "loaded-physical-delta-ready");
        assert!(readiness.ready);

        let directory = private_dir("flight-evidence-event-bounded");
        let store = RuntimeOverrideStore::open(&directory).unwrap();
        let burst_before = LoadedCounterBurstState::Priming {
            remaining_completions: 8,
            remaining_flights: 2,
        };
        capture.loaded_counter_burst = burst_before;
        let mut actuator = FakeCaptureAuthority::default();
        capture
            .advance_loaded_counter_burst_after_poll(
                &directory,
                &store,
                &mut actuator,
                burst_before,
                true,
            )
            .unwrap();

        assert!(capture.loaded_counter_burst.bracketing_flight());
        assert_eq!(
            capture.loaded_counter_cycle,
            LoadedCounterCycleState::FlightActive
        );
        assert!(capture.loaded_transport_work_active());
        assert!(capture.loaded_transport_authority.is_none());
        assert_eq!(actuator.loaded_dispatch_attestations, 1);
        assert!(request_rx.try_recv().is_ok());
        assert_eq!(
            capture
                .drive_loaded_counter_burst(&directory, &store, &mut actuator)
                .unwrap(),
            LoadedCounterReactorWait::TransportCompletion,
            "an authority dispatch must wait for its exact FlightPost settlement"
        );

        drop(capture);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn sampling_deadline_can_only_wake_earlier_than_the_owner_deadline() {
        assert_eq!(
            earliest_deadline(
                Some(Duration::from_secs(5)),
                Some(Duration::from_millis(200))
            ),
            Some(Duration::from_millis(200))
        );
        assert_eq!(
            earliest_deadline(Some(Duration::from_secs(5)), None),
            Some(Duration::from_secs(5))
        );
        assert_eq!(earliest_deadline(None, None), None);
    }
}
