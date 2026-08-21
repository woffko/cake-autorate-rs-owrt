use super::autotune_runtime::RuntimeQdiscKind;
use super::protocol::{OperationKind, OperationRequest, SpeedtestDirection};
use super::speedtest::{self, SpeedtestTerminal};
use crate::quality_grade::QUALITY_GRADE_METHOD;
use crate::Config;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SNAPSHOT_HEADER: &str = "cake-autorate-rating-runtime\t5";
const TERMINAL_HEADER: &str = "cake-autorate-rating-terminal\t2";
pub const RATING_EVIDENCE_CONTRACT: &str = QUALITY_GRADE_METHOD;
const MAX_SNAPSHOT_BYTES: usize = 8 * 1024;
const MAX_REQUEST_BYTES: usize = 8 * 1024;
const POLL_INTERVAL: Duration = Duration::from_secs(1);
const PERMIT_POLL_INTERVAL: Duration = Duration::from_millis(20);
const MAX_AUTOMATIC_DIRECTION_RUNS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RatingEvidenceContract {
    WorstOfDirectionBoundIcmpAndTransport,
}

impl RatingEvidenceContract {
    fn as_str(self) -> &'static str {
        match self {
            Self::WorstOfDirectionBoundIcmpAndTransport => RATING_EVIDENCE_CONTRACT,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RatingResultSnapshot {
    pub grade: String,
    pub increase_ms: f64,
    pub started_unix_ms: u64,
    pub partial: bool,
    pub incomplete: bool,
    pub dl_grade: String,
    pub ul_grade: String,
    pub dl_samples: u64,
    pub ul_samples: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RatingRuntimeSnapshot {
    pub updated_unix_ms: u64,
    pub capture_observed_unix_ms: u64,
    pub runtime_generation: u64,
    pub uplink_state: String,
    pub route_active: bool,
    pub route_test_ready: bool,
    pub sqm_runtime_managed: bool,
    pub sqm_runtime_healthy: bool,
    pub transport_probe_trusted: bool,
    pub baseline_ready: bool,
    pub baseline_samples: u64,
    pub baseline_required_samples: u64,
    pub required_samples: u64,
    pub evidence_contract: RatingEvidenceContract,
    pub dl_samples: u64,
    pub ul_samples: u64,
    pub dl_achieved_kbps: f64,
    pub ul_achieved_kbps: f64,
    pub cake_dl_kbps: f64,
    pub cake_ul_kbps: f64,
    /// Exact kinds observed by the instance controller. `None` means the
    /// direction is unshaped or the kind has not yet been attested.
    pub download_qdisc_kind: Option<RuntimeQdiscKind>,
    pub upload_qdisc_kind: Option<RuntimeQdiscKind>,
    pub reference_dl_kbps: f64,
    pub reference_ul_kbps: f64,
    pub capture_active: bool,
    pub capture_job_id: String,
    pub capture_generation: u64,
    pub finalized_job_id: String,
    pub finalized_generation: u64,
    pub finalized_outcome: String,
    pub capture_phase: String,
    pub capture_contaminated: bool,
    pub current_capture_job_id: String,
    pub current_capture_generation: u64,
    pub current: Option<RatingResultSnapshot>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RatingTerminal {
    Complete(RatingResultSnapshot),
    Cancelled,
    Incomplete { dl_samples: u64, ul_samples: u64 },
    Failed { code: String },
}

#[derive(Clone, Debug, PartialEq)]
pub struct RatingTerminalRecord {
    pub job_id: String,
    pub worker_run_id: String,
    pub terminal: RatingTerminal,
}

struct CaptureGuard {
    path: PathBuf,
    job_id: String,
    mode: &'static str,
    deadline_s: u64,
    phase: &'static str,
    background_dl: f64,
    background_ul: f64,
    armed: bool,
}

impl CaptureGuard {
    fn new(path: PathBuf, job_id: String) -> Self {
        Self {
            path,
            job_id,
            mode: "client",
            deadline_s: 0,
            phase: "AUTO",
            background_dl: 0.0,
            background_ul: 0.0,
            armed: false,
        }
    }

    fn arm(
        &mut self,
        mode: &'static str,
        phase: &'static str,
        deadline_unix_ms: u64,
        background_dl: f64,
        background_ul: f64,
    ) -> Result<(), String> {
        if !matches!(mode, "client" | "automatic")
            || !matches!(phase, "AUTO" | "IDLE" | "DL" | "UL")
        {
            return Err("rating-capture-mode-or-phase-invalid".to_string());
        }
        self.mode = mode;
        self.deadline_s = deadline_unix_ms / 1000;
        self.phase = phase;
        self.background_dl = background_dl;
        self.background_ul = background_ul;
        self.publish()?;
        self.armed = true;
        Ok(())
    }

    fn set_phase(&mut self, phase: &'static str) -> Result<(), String> {
        if !self.armed || !matches!(phase, "AUTO" | "IDLE" | "DL" | "UL") {
            return Err("rating-capture-phase-transition-invalid".to_string());
        }
        self.phase = phase;
        self.publish()
    }

    fn publish(&self) -> Result<(), String> {
        let content = format!(
            "{}|{}|{}|{}|{:.3}|{:.3}\n",
            self.job_id,
            self.mode,
            self.deadline_s,
            self.phase,
            self.background_dl,
            self.background_ul,
        );
        atomic_private_write(&self.path, content.as_bytes())
    }

    fn cleanup(&mut self) -> Result<(), String> {
        if !self.armed {
            return Ok(());
        }
        remove_matching_capture(&self.path, &self.job_id)?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

#[derive(Clone, Debug)]
struct QuietWindow {
    needed: usize,
    consecutive: usize,
    sum_dl: f64,
    sum_ul: f64,
}

impl QuietWindow {
    fn new(needed: usize) -> Self {
        Self {
            needed,
            consecutive: 0,
            sum_dl: 0.0,
            sum_ul: 0.0,
        }
    }

    fn observe(
        &mut self,
        dl_kbps: f64,
        ul_kbps: f64,
        dl_limit: f64,
        ul_limit: f64,
    ) -> Option<(f64, f64)> {
        if dl_kbps <= dl_limit && ul_kbps <= ul_limit {
            self.consecutive += 1;
            self.sum_dl += dl_kbps;
            self.sum_ul += ul_kbps;
            if self.consecutive >= self.needed {
                return Some((
                    self.sum_dl / self.consecutive as f64,
                    self.sum_ul / self.consecutive as f64,
                ));
            }
        } else {
            self.consecutive = 0;
            self.sum_dl = 0.0;
            self.sum_ul = 0.0;
        }
        None
    }
}

impl RatingRuntimeSnapshot {
    pub fn validate(&self) -> Result<(), String> {
        if self.updated_unix_ms == 0
            || self.capture_observed_unix_ms == 0
            || self.capture_observed_unix_ms > self.updated_unix_ms
            || self.runtime_generation == 0
        {
            return Err("rating runtime timestamps and generation are inconsistent".to_string());
        }
        require_identifier("uplink state", &self.uplink_state, b"_")?;
        require_identifier("capture phase", &self.capture_phase, b"_")?;
        for (name, value) in [
            ("capture job id", self.capture_job_id.as_str()),
            ("finalized job id", self.finalized_job_id.as_str()),
            (
                "current capture job id",
                self.current_capture_job_id.as_str(),
            ),
        ] {
            if !value.is_empty() {
                require_exact_hex(name, value, 32)?;
            }
        }
        if self.download_qdisc_kind.is_some() && self.cake_dl_kbps < 100.0 {
            return Err("rating runtime download qdisc kind has no usable CAKE rate".to_string());
        }
        if self.upload_qdisc_kind.is_some() && self.cake_ul_kbps < 100.0 {
            return Err("rating runtime upload qdisc kind has no usable CAKE rate".to_string());
        }
        require_identifier("finalized outcome", &self.finalized_outcome, b"_-")?;
        if self.capture_active {
            if self.capture_job_id.is_empty() || self.capture_generation == 0 {
                return Err("active rating capture lacks an exact identity".to_string());
            }
        } else if !self.capture_job_id.is_empty() {
            return Err("inactive rating capture still publishes a job id".to_string());
        }
        if self.capture_contaminated && !self.capture_active {
            return Err("inactive rating capture is marked contaminated".to_string());
        }
        let finalized_present = !self.finalized_job_id.is_empty()
            || self.finalized_generation != 0
            || !self.finalized_outcome.is_empty();
        if finalized_present
            && (self.finalized_job_id.is_empty()
                || self.finalized_generation == 0
                || !matches!(
                    self.finalized_outcome.as_str(),
                    "removed" | "contaminated" | "expired"
                ))
        {
            return Err("rating finalization identity or outcome is inconsistent".to_string());
        }
        let current_identity_present =
            !self.current_capture_job_id.is_empty() || self.current_capture_generation != 0;
        if self.current_capture_job_id.is_empty() != (self.current_capture_generation == 0) {
            return Err("rating result capture identity is inconsistent".to_string());
        }
        if self.current.is_none() && current_identity_present {
            return Err("absent rating result still publishes a capture identity".to_string());
        }
        for (name, value) in [
            ("download achieved rate", self.dl_achieved_kbps),
            ("upload achieved rate", self.ul_achieved_kbps),
            ("download CAKE rate", self.cake_dl_kbps),
            ("upload CAKE rate", self.cake_ul_kbps),
            ("download reference", self.reference_dl_kbps),
            ("upload reference", self.reference_ul_kbps),
        ] {
            if !value.is_finite() || value < 0.0 || value > 100_000_000.0 {
                return Err(format!("{name} is outside its bound"));
            }
        }
        for (name, value) in [
            ("download CAKE rate", self.cake_dl_kbps),
            ("upload CAKE rate", self.cake_ul_kbps),
        ] {
            if value != value.round() {
                return Err(format!("{name} is not an exact kbit/s value"));
            }
        }
        if let Some(current) = &self.current {
            require_identifier("rating grade", &current.grade, b"+")?;
            require_identifier("download grade", &current.dl_grade, b"+")?;
            require_identifier("upload grade", &current.ul_grade, b"+")?;
            if current.started_unix_ms == 0
                || !current.increase_ms.is_finite()
                || current.increase_ms < 0.0
                || current.increase_ms > 1_000_000.0
            {
                return Err("current rating result is outside its bound".to_string());
            }
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<String, String> {
        self.validate()?;
        if self.evidence_contract != RatingEvidenceContract::WorstOfDirectionBoundIcmpAndTransport {
            return Err("rating runtime snapshot has an unsupported evidence contract".to_string());
        }
        let empty = RatingResultSnapshot {
            grade: String::new(),
            increase_ms: 0.0,
            started_unix_ms: 0,
            partial: false,
            incomplete: false,
            dl_grade: String::new(),
            ul_grade: String::new(),
            dl_samples: 0,
            ul_samples: 0,
        };
        let current = self.current.as_ref().unwrap_or(&empty);
        let mut fields = vec![
            ("updated_unix_ms", self.updated_unix_ms.to_string()),
            (
                "capture_observed_unix_ms",
                self.capture_observed_unix_ms.to_string(),
            ),
            ("runtime_generation", self.runtime_generation.to_string()),
            ("uplink_state", self.uplink_state.clone()),
            ("route_active", bool_text(self.route_active).to_string()),
            (
                "route_test_ready",
                bool_text(self.route_test_ready).to_string(),
            ),
            (
                "sqm_runtime_managed",
                bool_text(self.sqm_runtime_managed).to_string(),
            ),
            (
                "sqm_runtime_healthy",
                bool_text(self.sqm_runtime_healthy).to_string(),
            ),
            (
                "transport_probe_trusted",
                bool_text(self.transport_probe_trusted).to_string(),
            ),
            ("baseline_ready", bool_text(self.baseline_ready).to_string()),
            ("baseline_samples", self.baseline_samples.to_string()),
            (
                "baseline_required_samples",
                self.baseline_required_samples.to_string(),
            ),
            ("required_samples", self.required_samples.to_string()),
        ];
        fields.push((
            "evidence_contract",
            self.evidence_contract.as_str().to_string(),
        ));
        fields.extend([
            ("dl_samples", self.dl_samples.to_string()),
            ("ul_samples", self.ul_samples.to_string()),
            ("dl_achieved_kbps", finite_text(self.dl_achieved_kbps)),
            ("ul_achieved_kbps", finite_text(self.ul_achieved_kbps)),
            ("cake_dl_kbps", finite_text(self.cake_dl_kbps)),
            ("cake_ul_kbps", finite_text(self.cake_ul_kbps)),
        ]);
        fields.extend([
            (
                "download_qdisc_kind",
                optional_qdisc_kind(self.download_qdisc_kind),
            ),
            (
                "upload_qdisc_kind",
                optional_qdisc_kind(self.upload_qdisc_kind),
            ),
        ]);
        fields.extend([
            ("reference_dl_kbps", finite_text(self.reference_dl_kbps)),
            ("reference_ul_kbps", finite_text(self.reference_ul_kbps)),
            ("capture_active", bool_text(self.capture_active).to_string()),
            ("capture_job_id", self.capture_job_id.clone()),
            ("capture_generation", self.capture_generation.to_string()),
            ("finalized_job_id", self.finalized_job_id.clone()),
            (
                "finalized_generation",
                self.finalized_generation.to_string(),
            ),
            ("finalized_outcome", self.finalized_outcome.clone()),
            ("capture_phase", self.capture_phase.clone()),
            (
                "capture_contaminated",
                bool_text(self.capture_contaminated).to_string(),
            ),
            (
                "current_capture_job_id",
                self.current_capture_job_id.clone(),
            ),
            (
                "current_capture_generation",
                self.current_capture_generation.to_string(),
            ),
            (
                "current_present",
                bool_text(self.current.is_some()).to_string(),
            ),
            ("current_grade", current.grade.clone()),
            ("current_increase_ms", finite_text(current.increase_ms)),
            (
                "current_started_unix_ms",
                current.started_unix_ms.to_string(),
            ),
            ("current_partial", bool_text(current.partial).to_string()),
            (
                "current_incomplete",
                bool_text(current.incomplete).to_string(),
            ),
            ("current_dl_grade", current.dl_grade.clone()),
            ("current_ul_grade", current.ul_grade.clone()),
            ("current_dl_samples", current.dl_samples.to_string()),
            ("current_ul_samples", current.ul_samples.to_string()),
        ]);
        let mut output = String::from(SNAPSHOT_HEADER);
        output.push('\n');
        for (name, value) in fields {
            if value.contains(['\n', '\r', '=']) {
                return Err(format!("rating runtime field {name} is unsafe"));
            }
            output.push_str(name);
            output.push('=');
            output.push_str(&value);
            output.push('\n');
        }
        if output.len() > MAX_SNAPSHOT_BYTES {
            return Err("rating runtime snapshot exceeds its bound".to_string());
        }
        Ok(output)
    }

    pub fn decode(input: &str) -> Result<Self, String> {
        if input.is_empty() || input.len() > MAX_SNAPSHOT_BYTES || !input.ends_with('\n') {
            return Err("rating runtime snapshot is empty, oversized, or truncated".to_string());
        }
        let mut lines = input.lines();
        if lines.next() != Some(SNAPSHOT_HEADER) {
            return Err("unsupported rating runtime snapshot header".to_string());
        }
        let updated_unix_ms = number(&mut lines, "updated_unix_ms")?;
        let capture_observed_unix_ms = number(&mut lines, "capture_observed_unix_ms")?;
        let runtime_generation = number(&mut lines, "runtime_generation")?;
        let uplink_state = field(&mut lines, "uplink_state")?;
        let route_active = boolean(&mut lines, "route_active")?;
        let route_test_ready = boolean(&mut lines, "route_test_ready")?;
        let sqm_runtime_managed = boolean(&mut lines, "sqm_runtime_managed")?;
        let sqm_runtime_healthy = boolean(&mut lines, "sqm_runtime_healthy")?;
        let transport_probe_trusted = boolean(&mut lines, "transport_probe_trusted")?;
        let baseline_ready = boolean(&mut lines, "baseline_ready")?;
        let baseline_samples = number(&mut lines, "baseline_samples")?;
        let baseline_required_samples = number(&mut lines, "baseline_required_samples")?;
        let required_samples = number(&mut lines, "required_samples")?;
        let evidence_contract = match field(&mut lines, "evidence_contract")?.as_str() {
            RATING_EVIDENCE_CONTRACT => {
                RatingEvidenceContract::WorstOfDirectionBoundIcmpAndTransport
            }
            _ => return Err("rating runtime evidence contract is unsupported".to_string()),
        };
        let dl_samples = number(&mut lines, "dl_samples")?;
        let ul_samples = number(&mut lines, "ul_samples")?;
        let dl_achieved_kbps = decimal(&mut lines, "dl_achieved_kbps")?;
        let ul_achieved_kbps = decimal(&mut lines, "ul_achieved_kbps")?;
        let cake_dl_kbps = decimal(&mut lines, "cake_dl_kbps")?;
        let cake_ul_kbps = decimal(&mut lines, "cake_ul_kbps")?;
        let download_qdisc_kind = parse_optional_qdisc_kind(
            "download_qdisc_kind",
            &field(&mut lines, "download_qdisc_kind")?,
        )?;
        let upload_qdisc_kind = parse_optional_qdisc_kind(
            "upload_qdisc_kind",
            &field(&mut lines, "upload_qdisc_kind")?,
        )?;
        let reference_dl_kbps = decimal(&mut lines, "reference_dl_kbps")?;
        let reference_ul_kbps = decimal(&mut lines, "reference_ul_kbps")?;
        let capture_active = boolean(&mut lines, "capture_active")?;
        let capture_job_id = field(&mut lines, "capture_job_id")?;
        let capture_generation = number(&mut lines, "capture_generation")?;
        let finalized_job_id = field(&mut lines, "finalized_job_id")?;
        let finalized_generation = number(&mut lines, "finalized_generation")?;
        let finalized_outcome = field(&mut lines, "finalized_outcome")?;
        let capture_phase = field(&mut lines, "capture_phase")?;
        let capture_contaminated = boolean(&mut lines, "capture_contaminated")?;
        let current_capture_job_id = field(&mut lines, "current_capture_job_id")?;
        let current_capture_generation = number(&mut lines, "current_capture_generation")?;
        let current_present = boolean(&mut lines, "current_present")?;
        let current_grade = field(&mut lines, "current_grade")?;
        let current_increase_ms = decimal(&mut lines, "current_increase_ms")?;
        let current_started_unix_ms = number(&mut lines, "current_started_unix_ms")?;
        let current_partial = boolean(&mut lines, "current_partial")?;
        let current_incomplete = boolean(&mut lines, "current_incomplete")?;
        let current_dl_grade = field(&mut lines, "current_dl_grade")?;
        let current_ul_grade = field(&mut lines, "current_ul_grade")?;
        let current_dl_samples = number(&mut lines, "current_dl_samples")?;
        let current_ul_samples = number(&mut lines, "current_ul_samples")?;
        if lines.next().is_some() {
            return Err("rating runtime snapshot has unknown fields".to_string());
        }
        let current = current_present.then_some(RatingResultSnapshot {
            grade: current_grade,
            increase_ms: current_increase_ms,
            started_unix_ms: current_started_unix_ms,
            partial: current_partial,
            incomplete: current_incomplete,
            dl_grade: current_dl_grade,
            ul_grade: current_ul_grade,
            dl_samples: current_dl_samples,
            ul_samples: current_ul_samples,
        });
        let snapshot = Self {
            updated_unix_ms,
            capture_observed_unix_ms,
            runtime_generation,
            uplink_state,
            route_active,
            route_test_ready,
            sqm_runtime_managed,
            sqm_runtime_healthy,
            transport_probe_trusted,
            baseline_ready,
            baseline_samples,
            baseline_required_samples,
            required_samples,
            evidence_contract,
            dl_samples,
            ul_samples,
            dl_achieved_kbps,
            ul_achieved_kbps,
            cake_dl_kbps,
            cake_ul_kbps,
            download_qdisc_kind,
            upload_qdisc_kind,
            reference_dl_kbps,
            reference_ul_kbps,
            capture_active,
            capture_job_id,
            capture_generation,
            finalized_job_id,
            finalized_generation,
            finalized_outcome,
            capture_phase,
            capture_contaminated,
            current_capture_job_id,
            current_capture_generation,
            current,
        };
        snapshot.validate()?;
        if snapshot.encode()? != input {
            return Err("rating runtime snapshot is not canonically encoded".to_string());
        }
        Ok(snapshot)
    }

    pub fn write_atomic(&self, path: &Path) -> io::Result<()> {
        let content = match self.encode() {
            Ok(content) => content,
            Err(error) => {
                // Never leave an older, apparently fresh runtime identity behind
                // after the live state can no longer be encoded. Absence is the
                // fail-closed signal understood by every native consumer.
                let _ = fs::remove_file(path);
                return Err(io::Error::new(io::ErrorKind::InvalidData, error));
            }
        };
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "snapshot has no parent"))?;
        if let Err(error) = fs::create_dir_all(parent) {
            let _ = fs::remove_file(path);
            return Err(error);
        }
        let tmp = parent.join(format!(
            ".rating-runtime-{}-{}.tmp",
            std::process::id(),
            self.updated_unix_ms
        ));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
            fs::rename(&tmp, path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
            let _ = fs::remove_file(path);
        }
        result
    }
}

impl RatingTerminal {
    pub fn encode(&self, job_id: &str, worker_run_id: &str) -> Result<String, String> {
        require_exact_hex("job id", job_id, 32)?;
        require_exact_hex("worker run id", worker_run_id, 32)?;
        let (state, code, result) = match self {
            Self::Complete(result) => ("complete", "", Some(result.clone())),
            Self::Cancelled => ("cancelled", "", None),
            Self::Incomplete {
                dl_samples,
                ul_samples,
            } => {
                let result = RatingResultSnapshot {
                    grade: String::new(),
                    increase_ms: 0.0,
                    started_unix_ms: 0,
                    partial: true,
                    incomplete: true,
                    dl_grade: String::new(),
                    ul_grade: String::new(),
                    dl_samples: *dl_samples,
                    ul_samples: *ul_samples,
                };
                ("incomplete", "rating-incomplete", Some(result))
            }
            Self::Failed { code } => {
                require_identifier("rating terminal code", code, b"_-")?;
                ("failed", code.as_str(), None)
            }
        };
        let empty = RatingResultSnapshot {
            grade: String::new(),
            increase_ms: 0.0,
            started_unix_ms: 0,
            partial: false,
            incomplete: false,
            dl_grade: String::new(),
            ul_grade: String::new(),
            dl_samples: 0,
            ul_samples: 0,
        };
        let result = result.as_ref().unwrap_or(&empty);
        for (name, value) in [
            ("state", state),
            ("code", code),
            ("grade", result.grade.as_str()),
            ("dl_grade", result.dl_grade.as_str()),
            ("ul_grade", result.ul_grade.as_str()),
        ] {
            if value.contains(['\n', '\r', '=']) {
                return Err(format!("rating terminal {name} is unsafe"));
            }
        }
        Ok(format!(
            "{TERMINAL_HEADER}\njob_id={job_id}\nworker_run_id={worker_run_id}\nevidence_contract={RATING_EVIDENCE_CONTRACT}\nstate={state}\ncode={code}\ngrade={}\nincrease_ms={}\nstarted_unix_ms={}\npartial={}\nincomplete={}\ndl_grade={}\nul_grade={}\ndl_samples={}\nul_samples={}\n",
            result.grade,
            finite_text(result.increase_ms),
            result.started_unix_ms,
            bool_text(result.partial),
            bool_text(result.incomplete),
            result.dl_grade,
            result.ul_grade,
            result.dl_samples,
            result.ul_samples,
        ))
    }
}

impl RatingTerminalRecord {
    pub fn decode(content: &str) -> Result<Self, String> {
        if content.len() > MAX_SNAPSHOT_BYTES || !content.ends_with('\n') {
            return Err("rating terminal is oversized or unterminated".to_string());
        }
        let mut lines = content.lines();
        if lines.next() != Some(TERMINAL_HEADER) {
            return Err("rating terminal header is unsupported".to_string());
        }
        let job_id = terminal_field(&mut lines, "job_id")?;
        let worker_run_id = terminal_field(&mut lines, "worker_run_id")?;
        require_exact_hex("job id", &job_id, 32)?;
        require_exact_hex("worker run id", &worker_run_id, 32)?;
        if terminal_field(&mut lines, "evidence_contract")? != RATING_EVIDENCE_CONTRACT {
            return Err("rating terminal evidence contract is unsupported".to_string());
        }
        let state = terminal_field(&mut lines, "state")?;
        let code = terminal_field(&mut lines, "code")?;
        let grade = terminal_field(&mut lines, "grade")?;
        let increase_ms = terminal_decimal(&mut lines, "increase_ms")?;
        let started_unix_ms = terminal_number(&mut lines, "started_unix_ms")?;
        let partial = terminal_boolean(&mut lines, "partial")?;
        let incomplete = terminal_boolean(&mut lines, "incomplete")?;
        let dl_grade = terminal_field(&mut lines, "dl_grade")?;
        let ul_grade = terminal_field(&mut lines, "ul_grade")?;
        let dl_samples = terminal_number(&mut lines, "dl_samples")?;
        let ul_samples = terminal_number(&mut lines, "ul_samples")?;
        if lines.next().is_some() {
            return Err("rating terminal contains unknown fields".to_string());
        }
        let result = RatingResultSnapshot {
            grade,
            increase_ms,
            started_unix_ms,
            partial,
            incomplete,
            dl_grade,
            ul_grade,
            dl_samples,
            ul_samples,
        };
        let terminal = match state.as_str() {
            "complete"
                if code.is_empty()
                    && !result.partial
                    && !result.incomplete
                    && result.started_unix_ms > 0
                    && !result.grade.is_empty() =>
            {
                require_identifier("rating grade", &result.grade, b"+")?;
                require_identifier("download grade", &result.dl_grade, b"+")?;
                require_identifier("upload grade", &result.ul_grade, b"+")?;
                RatingTerminal::Complete(result)
            }
            "cancelled" if code.is_empty() && terminal_result_empty(&result) => {
                RatingTerminal::Cancelled
            }
            "incomplete"
                if code == "rating-incomplete"
                    && result.partial
                    && result.incomplete
                    && result.grade.is_empty()
                    && result.started_unix_ms == 0 =>
            {
                RatingTerminal::Incomplete {
                    dl_samples: result.dl_samples,
                    ul_samples: result.ul_samples,
                }
            }
            "failed" if terminal_result_empty(&result) => {
                require_identifier("rating terminal code", &code, b"_-")?;
                if code.is_empty() {
                    return Err("failed rating terminal has no diagnostic code".to_string());
                }
                RatingTerminal::Failed { code }
            }
            _ => return Err("rating terminal state and evidence disagree".to_string()),
        };
        Ok(Self {
            job_id,
            worker_run_id,
            terminal,
        })
    }
}

fn terminal_result_empty(result: &RatingResultSnapshot) -> bool {
    result.grade.is_empty()
        && result.increase_ms == 0.0
        && result.started_unix_ms == 0
        && !result.partial
        && !result.incomplete
        && result.dl_grade.is_empty()
        && result.ul_grade.is_empty()
        && result.dl_samples == 0
        && result.ul_samples == 0
}

fn owned_complete_result(
    snapshot: &RatingRuntimeSnapshot,
    job_id: &str,
    capture_generation: u64,
) -> Option<RatingResultSnapshot> {
    let current = snapshot.current.as_ref()?;
    (snapshot.evidence_contract == RatingEvidenceContract::WorstOfDirectionBoundIcmpAndTransport
        && snapshot.current_capture_job_id == job_id
        && snapshot.current_capture_generation == capture_generation
        && !current.partial
        && !current.incomplete
        && !current.grade.is_empty()
        && current.grade != "LEARNING")
        .then(|| current.clone())
}

fn matching_finalization(
    snapshot: &RatingRuntimeSnapshot,
    job_id: &str,
    capture_generation: u64,
) -> Option<Result<(), &'static str>> {
    if snapshot.finalized_job_id != job_id || snapshot.finalized_generation != capture_generation {
        return None;
    }
    Some(match snapshot.finalized_outcome.as_str() {
        "removed" => Ok(()),
        "contaminated" => Err("capture-contaminated"),
        "expired" => Err("capture-expired"),
        _ => Err("capture-finalization-outcome-invalid"),
    })
}

pub fn read_terminal_file(path: &Path) -> Result<RatingTerminalRecord, String> {
    RatingTerminalRecord::decode(&read_private_bounded(path, MAX_SNAPSHOT_BYTES)?)
}

pub fn run_rating_worker<I>(mut args: I, terminate: &AtomicBool) -> Result<(), String>
where
    I: Iterator<Item = String>,
{
    let mut request_path = None;
    let mut terminal_path = None;
    let mut permit_path = None;
    let mut worker_run_id = None;
    while let Some(argument) = args.next() {
        let destination = match argument.as_str() {
            "--request" => &mut request_path,
            "--terminal" => &mut terminal_path,
            "--permit" => &mut permit_path,
            "--worker-run-id" => &mut worker_run_id,
            _ => return Err(format!("unsupported rating worker argument: {argument}")),
        };
        if destination.is_some() {
            return Err(format!("duplicate rating worker argument: {argument}"));
        }
        *destination = Some(
            args.next()
                .ok_or_else(|| format!("{argument} requires a value"))?,
        );
    }
    let request_path =
        PathBuf::from(request_path.ok_or_else(|| "--request is required".to_string())?);
    let terminal_path =
        PathBuf::from(terminal_path.ok_or_else(|| "--terminal is required".to_string())?);
    let permit_path = PathBuf::from(permit_path.ok_or_else(|| "--permit is required".to_string())?);
    let worker_run_id = worker_run_id.ok_or_else(|| "--worker-run-id is required".to_string())?;
    require_exact_hex("worker run id", &worker_run_id, 32)?;
    let request = read_private_request(&request_path)?;
    if !matches!(
        request.identity.operation,
        OperationKind::GuidedRating | OperationKind::AutomaticRating
    ) {
        return Err("rating worker accepts rating operations only".to_string());
    }

    let terminal = match wait_for_permit(
        &permit_path,
        &request.identity.job_id,
        &worker_run_id,
        request.deadline_unix_ms,
        terminate,
    ) {
        Ok(true) => match run_rating(&request, &worker_run_id, &terminal_path, terminate) {
            Ok(terminal) => terminal,
            Err(error) if error == "rating-cancelled" => RatingTerminal::Cancelled,
            Err(error) => RatingTerminal::Failed {
                code: bounded_error_code(&error),
            },
        },
        Ok(false) => RatingTerminal::Cancelled,
        Err(error) => RatingTerminal::Failed {
            code: bounded_error_code(&error),
        },
    };
    let content = terminal.encode(&request.identity.job_id, &worker_run_id)?;
    atomic_private_write(&terminal_path, content.as_bytes())
}

pub(crate) fn wait_for_permit(
    path: &Path,
    job_id: &str,
    worker_run_id: &str,
    operation_deadline_unix_ms: u64,
    terminate: &AtomicBool,
) -> Result<bool, String> {
    let expected_name = format!("permit-{worker_run_id}");
    if path.file_name().and_then(|name| name.to_str()) != Some(expected_name.as_str()) {
        return Err("worker-permit-path-mismatch".to_string());
    }
    loop {
        if terminate.load(Ordering::Relaxed) {
            return Ok(false);
        }
        match fs::symlink_metadata(path) {
            Ok(_) => {
                let content = read_private_bounded(path, 256)?;
                decode_permit(&content, job_id, worker_run_id)?;
                return Ok(true);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err("worker-permit-inspection-failed".to_string()),
        }
        if epoch_ms()? >= operation_deadline_unix_ms {
            return Err("operation-deadline-expired-before-worker-permit".to_string());
        }
        thread::sleep(PERMIT_POLL_INTERVAL);
    }
}

pub fn encode_permit(job_id: &str, worker_run_id: &str) -> Result<String, String> {
    require_exact_hex("job id", job_id, 32)?;
    require_exact_hex("worker run id", worker_run_id, 32)?;
    Ok(format!(
        "cake-autorate-rating-permit\t1\njob_id={job_id}\nworker_run_id={worker_run_id}\n"
    ))
}

fn decode_permit(content: &str, job_id: &str, worker_run_id: &str) -> Result<(), String> {
    let expected = encode_permit(job_id, worker_run_id)?;
    if content != expected {
        return Err("worker-permit-identity-mismatch".to_string());
    }
    Ok(())
}

fn run_rating(
    request: &OperationRequest,
    worker_run_id: &str,
    terminal_path: &Path,
    terminate: &AtomicBool,
) -> Result<RatingTerminal, String> {
    let automatic = request.identity.operation == OperationKind::AutomaticRating;
    let cfg = Config::from_uci(&request.identity.instance)?;
    if !cfg.enabled {
        return Err("instance-disabled".to_string());
    }
    if !cfg.sqm_enabled {
        return Err("sqm-disabled".to_string());
    }
    if !cfg.transport_latency_enabled {
        return Err("transport-disabled".to_string());
    }
    if automatic && request.backend != "speedtest-go" {
        return Err("automatic-rating-backend-unsupported".to_string());
    }
    if cfg.sqm_interface != request.identity.target_interface
        || request.route.l3_device != request.identity.target_interface
    {
        return Err("runtime-target-mismatch".to_string());
    }
    let run_root = std::env::var_os("CAKE_AUTORATE_RUN_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/run/cake-autorate"));
    let run_dir = run_root.join(&request.identity.instance);
    let snapshot_path = run_dir.join("rating-runtime");
    let capture_path = run_dir.join("rating-capture");
    let mut initial = None;
    let mut stable_runtime_publication = None;
    while epoch_ms()? < request.deadline_unix_ms {
        if terminate.load(Ordering::Relaxed) {
            return Ok(RatingTerminal::Cancelled);
        }
        match read_rating_snapshot(&snapshot_path) {
            Ok(snapshot)
                if rating_runtime_readiness(&snapshot, epoch_ms()?, automatic, None)
                    == RatingRuntimeReadiness::Ready
                    && !snapshot.capture_active =>
            {
                if observe_stable_runtime_publication(&mut stable_runtime_publication, &snapshot) {
                    initial = Some(snapshot);
                    break;
                }
            }
            _ => stable_runtime_publication = None,
        }
        thread::sleep(POLL_INTERVAL);
    }
    let initial =
        initial.ok_or_else(|| "operation-deadline-expired-during-runtime-preflight".to_string())?;
    let runtime_generation = initial.runtime_generation;
    let reference_dl = positive_reference(initial.reference_dl_kbps, initial.cake_dl_kbps)?;
    let reference_ul = positive_reference(initial.reference_ul_kbps, initial.cake_ul_kbps)?;
    let dl_limit =
        (reference_dl * cfg.rating_capture_quiet_ratio).max(cfg.rating_capture_quiet_min_kbps);
    let ul_limit =
        (reference_ul * cfg.rating_capture_quiet_ratio).max(cfg.rating_capture_quiet_min_kbps);
    let background = wait_for_quiet(
        &snapshot_path,
        request,
        terminate,
        automatic,
        cfg.rating_capture_quiet_s,
        cfg.rating_capture_quiet_timeout_s,
        dl_limit,
        ul_limit,
        runtime_generation,
        None,
    )?;

    let mut capture = CaptureGuard::new(capture_path, request.identity.job_id.clone());
    let initial_phase = if automatic { "IDLE" } else { "AUTO" };
    capture.arm(
        if automatic { "automatic" } else { "client" },
        initial_phase,
        request.deadline_unix_ms,
        background.0,
        background.1,
    )?;
    let capture_generation = wait_capture_phase(
        &snapshot_path,
        request,
        terminate,
        automatic,
        initial_phase,
        runtime_generation,
        None,
    )?;
    if automatic {
        run_automatic_rating_load(
            request,
            worker_run_id,
            terminal_path,
            &snapshot_path,
            &mut capture,
            terminate,
            cfg.rating_capture_quiet_s,
            cfg.rating_capture_quiet_timeout_s,
            dl_limit,
            ul_limit,
            runtime_generation,
            capture_generation,
        )?;
    }

    let mut complete_result = None;
    let mut last_owned_dl_samples = 0;
    let mut last_owned_ul_samples = 0;
    while epoch_ms()? < request.deadline_unix_ms {
        if terminate.load(Ordering::Relaxed) {
            return Ok(RatingTerminal::Cancelled);
        }
        let snapshot =
            wait_for_rating_snapshot(&snapshot_path, request.deadline_unix_ms, terminate)?;
        if let Some(snapshot) = authoritative_rating_snapshot(
            &snapshot,
            epoch_ms()?,
            automatic,
            Some(runtime_generation),
        )? {
            if owned_capture_contaminated(snapshot, &request.identity.job_id, capture_generation) {
                return Err("capture-contaminated".to_string());
            }
            if snapshot.capture_active
                && snapshot.capture_job_id == request.identity.job_id
                && snapshot.capture_generation == capture_generation
            {
                last_owned_dl_samples = last_owned_dl_samples.max(snapshot.dl_samples);
                last_owned_ul_samples = last_owned_ul_samples.max(snapshot.ul_samples);
            }

            if complete_result.is_none() {
                if let Some(current) =
                    owned_complete_result(snapshot, &request.identity.job_id, capture_generation)
                {
                    last_owned_dl_samples = last_owned_dl_samples.max(current.dl_samples);
                    last_owned_ul_samples = last_owned_ul_samples.max(current.ul_samples);
                    complete_result = Some(current);
                    capture.cleanup()?;
                }
            }

            if let Some(finalization) =
                matching_finalization(snapshot, &request.identity.job_id, capture_generation)
            {
                match finalization {
                    Ok(()) => {
                        if let Some(current) = complete_result {
                            return Ok(RatingTerminal::Complete(current));
                        }
                        return Err("capture-finalized-before-complete-result".to_string());
                    }
                    Err(error) => return Err(error.to_string()),
                }
            }
        }

        thread::sleep(POLL_INTERVAL);
    }
    capture.cleanup()?;

    Ok(RatingTerminal::Incomplete {
        dl_samples: last_owned_dl_samples,
        ul_samples: last_owned_ul_samples,
    })
}

#[allow(clippy::too_many_arguments)]
fn run_automatic_rating_load(
    request: &OperationRequest,
    worker_run_id: &str,
    terminal_path: &Path,
    snapshot_path: &Path,
    capture: &mut CaptureGuard,
    terminate: &AtomicBool,
    quiet_samples: usize,
    quiet_timeout_s: usize,
    dl_limit: f64,
    ul_limit: f64,
    runtime_generation: u64,
    capture_generation: u64,
) -> Result<(), String> {
    let mut remaining_budget = request.traffic_budget_bytes;
    let mut server_id = request.speedtest_server_id;
    for direction in [SpeedtestDirection::Download, SpeedtestDirection::Upload] {
        run_automatic_direction(
            request,
            worker_run_id,
            terminal_path,
            snapshot_path,
            capture,
            terminate,
            direction,
            quiet_samples,
            quiet_timeout_s,
            dl_limit,
            ul_limit,
            runtime_generation,
            capture_generation,
            &mut remaining_budget,
            &mut server_id,
        )?;
    }
    capture.set_phase("IDLE")?;
    wait_capture_phase(
        snapshot_path,
        request,
        terminate,
        true,
        "IDLE",
        runtime_generation,
        Some(capture_generation),
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_automatic_direction(
    request: &OperationRequest,
    worker_run_id: &str,
    terminal_path: &Path,
    snapshot_path: &Path,
    capture: &mut CaptureGuard,
    terminate: &AtomicBool,
    direction: SpeedtestDirection,
    quiet_samples: usize,
    quiet_timeout_s: usize,
    dl_limit: f64,
    ul_limit: f64,
    runtime_generation: u64,
    capture_generation: u64,
    remaining_budget: &mut u64,
    server_id: &mut Option<u64>,
) -> Result<(), String> {
    let phase = match direction {
        SpeedtestDirection::Download => "DL",
        SpeedtestDirection::Upload => "UL",
        SpeedtestDirection::Both => return Err("automatic-rating-direction-invalid".to_string()),
    };
    for _ in 0..MAX_AUTOMATIC_DIRECTION_RUNS {
        let snapshot =
            wait_for_rating_snapshot(snapshot_path, request.deadline_unix_ms, terminate)?;
        if direction_samples_ready(&snapshot, direction) {
            return Ok(());
        }
        if *remaining_budget == 0 {
            return Err("automatic-rating-traffic-budget-exhausted".to_string());
        }

        // Every retry and direction transition gets a fresh, measured drain
        // window. This prevents tail packets and remote queues from being
        // attributed to the next rating direction.
        capture.set_phase("IDLE")?;
        wait_capture_phase(
            snapshot_path,
            request,
            terminate,
            true,
            "IDLE",
            runtime_generation,
            Some(capture_generation),
        )?;

        wait_for_quiet(
            snapshot_path,
            request,
            terminate,
            true,
            quiet_samples,
            quiet_timeout_s,
            dl_limit,
            ul_limit,
            runtime_generation,
            Some(capture_generation),
        )?;

        capture.set_phase(phase)?;
        wait_capture_phase(
            snapshot_path,
            request,
            terminate,
            true,
            phase,
            runtime_generation,
            Some(capture_generation),
        )?;
        let mut phase_request = request.clone();
        phase_request.speedtest_server_id = *server_id;
        phase_request.traffic_budget_bytes = *remaining_budget;
        let terminal = speedtest::run_embedded_speedtest(
            &phase_request,
            worker_run_id,
            direction,
            terminate,
            terminal_path,
        );

        // Close the measured direction before interpreting any result. Drop
        // also removes the whole capture on every error/cancellation path.
        capture.set_phase("IDLE")?;
        wait_capture_phase(
            snapshot_path,
            request,
            terminate,
            true,
            "IDLE",
            runtime_generation,
            Some(capture_generation),
        )?;

        let result = match terminal {
            Ok(SpeedtestTerminal::Complete(result)) => result,
            Ok(SpeedtestTerminal::Cancelled) => return Err("rating-cancelled".to_string()),
            Ok(SpeedtestTerminal::Failed { code }) => return Err(code),
            Err(error) => return Err(error),
        };
        consume_traffic_budget(remaining_budget, result.rx_bytes, result.tx_bytes)?;
        if server_id.is_none() {
            *server_id = result.server_id;
        }

        wait_for_quiet(
            snapshot_path,
            request,
            terminate,
            true,
            quiet_samples,
            quiet_timeout_s,
            dl_limit,
            ul_limit,
            runtime_generation,
            Some(capture_generation),
        )?;
        let drained = wait_for_rating_snapshot(snapshot_path, request.deadline_unix_ms, terminate)?;
        if owned_capture_contaminated(&drained, &request.identity.job_id, capture_generation) {
            return Err("capture-contaminated".to_string());
        }
        if direction_samples_ready(&drained, direction) {
            return Ok(());
        }
    }
    Err(match direction {
        SpeedtestDirection::Download => "automatic-download-incomplete",
        SpeedtestDirection::Upload => "automatic-upload-incomplete",
        SpeedtestDirection::Both => "automatic-rating-incomplete",
    }
    .to_string())
}

fn direction_samples_ready(
    snapshot: &RatingRuntimeSnapshot,
    direction: SpeedtestDirection,
) -> bool {
    match direction {
        SpeedtestDirection::Download => snapshot.dl_samples >= snapshot.required_samples,
        SpeedtestDirection::Upload => snapshot.ul_samples >= snapshot.required_samples,
        SpeedtestDirection::Both => {
            snapshot.dl_samples >= snapshot.required_samples
                && snapshot.ul_samples >= snapshot.required_samples
        }
    }
}

fn consume_traffic_budget(remaining: &mut u64, rx_bytes: u64, tx_bytes: u64) -> Result<(), String> {
    let consumed = rx_bytes
        .checked_add(tx_bytes)
        .ok_or_else(|| "automatic-rating-traffic-budget-overflow".to_string())?;
    *remaining = remaining
        .checked_sub(consumed)
        .ok_or_else(|| "automatic-rating-traffic-budget-exceeded".to_string())?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn wait_for_quiet(
    snapshot_path: &Path,
    request: &OperationRequest,
    terminate: &AtomicBool,
    require_test_route: bool,
    quiet_samples: usize,
    quiet_timeout_s: usize,
    dl_limit: f64,
    ul_limit: f64,
    runtime_generation: u64,
    expected_capture_generation: Option<u64>,
) -> Result<(f64, f64), String> {
    let deadline = epoch_ms()?
        .saturating_add((quiet_timeout_s as u64) * 1000)
        .min(request.deadline_unix_ms);
    let mut quiet = QuietWindow::new(quiet_samples.max(2));
    loop {
        if terminate.load(Ordering::Relaxed) {
            return Err("rating-cancelled".to_string());
        }
        if epoch_ms()? >= deadline {
            return Err("quiet-window-timeout".to_string());
        }
        let snapshot = wait_for_rating_snapshot(snapshot_path, deadline, terminate)?;
        if let Some(snapshot) = authoritative_rating_snapshot(
            &snapshot,
            epoch_ms()?,
            require_test_route,
            Some(runtime_generation),
        )? {
            if expected_capture_generation.is_some_and(|capture_generation| {
                owned_capture_contaminated(snapshot, &request.identity.job_id, capture_generation)
            }) {
                return Err("capture-contaminated".to_string());
            }
            if let Some(background) = quiet.observe(
                snapshot.dl_achieved_kbps,
                snapshot.ul_achieved_kbps,
                dl_limit,
                ul_limit,
            ) {
                return Ok(background);
            }
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn wait_capture_phase(
    snapshot_path: &Path,
    request: &OperationRequest,
    terminate: &AtomicBool,
    require_test_route: bool,
    expected: &str,
    runtime_generation: u64,
    expected_capture_generation: Option<u64>,
) -> Result<u64, String> {
    while epoch_ms()? < request.deadline_unix_ms {
        if terminate.load(Ordering::Relaxed) {
            return Err("rating-cancelled".to_string());
        }
        let snapshot =
            wait_for_rating_snapshot(snapshot_path, request.deadline_unix_ms, terminate)?;
        if let Some(snapshot) = authoritative_rating_snapshot(
            &snapshot,
            epoch_ms()?,
            require_test_route,
            Some(runtime_generation),
        )? {
            if expected_capture_generation.is_some_and(|capture_generation| {
                owned_capture_contaminated(snapshot, &request.identity.job_id, capture_generation)
            }) {
                return Err("capture-contaminated".to_string());
            }
            if snapshot.capture_active
                && snapshot.capture_job_id == request.identity.job_id
                && snapshot.capture_phase == expected
                && expected_capture_generation
                    .is_none_or(|generation| snapshot.capture_generation == generation)
            {
                return Ok(snapshot.capture_generation);
            }
        }
        thread::sleep(POLL_INTERVAL);
    }
    Err("operation-deadline-expired-during-capture-handshake".to_string())
}

fn owned_capture_contaminated(
    snapshot: &RatingRuntimeSnapshot,
    job_id: &str,
    capture_generation: u64,
) -> bool {
    snapshot.capture_contaminated
        && snapshot.capture_active
        && snapshot.capture_job_id == job_id
        && snapshot.capture_generation == capture_generation
}

fn observe_stable_runtime_publication(
    previous: &mut Option<(u64, u64)>,
    snapshot: &RatingRuntimeSnapshot,
) -> bool {
    let publication = (snapshot.runtime_generation, snapshot.updated_unix_ms);
    let stable = previous.is_some_and(|(generation, updated_unix_ms)| {
        generation == publication.0 && publication.1 > updated_unix_ms
    });
    *previous = Some(publication);
    stable
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RatingRuntimeReadiness {
    Ready,
    Stale,
    Unready(&'static str),
}

fn rating_runtime_readiness(
    snapshot: &RatingRuntimeSnapshot,
    now_ms: u64,
    require_test_route: bool,
    expected_runtime_generation: Option<u64>,
) -> RatingRuntimeReadiness {
    // Publication age is a liveness signal, not authoritative evidence that
    // routing or SQM became unsafe.  A stale snapshot must never contribute
    // samples or a completion acknowledgement, but it remains waitable until
    // the operation's existing immutable deadline.
    if now_ms.saturating_sub(snapshot.updated_unix_ms) > 5_000
        || now_ms.saturating_sub(snapshot.capture_observed_unix_ms) > 5_000
    {
        return RatingRuntimeReadiness::Stale;
    }
    if expected_runtime_generation
        .is_some_and(|generation| snapshot.runtime_generation != generation)
    {
        return RatingRuntimeReadiness::Unready("runtime-generation-changed");
    }
    if !matches!(snapshot.uplink_state.as_str(), "ACTIVE" | "STANDBY") {
        return RatingRuntimeReadiness::Unready("runtime-uplink-unready");
    }
    // Automatic Rating owns generated load and an explicitly attested test
    // route, so an online STANDBY member is valid when route_test_ready is
    // true. Client-guided Rating cannot redirect the client's traffic and
    // must remain bound to the currently active route.
    let route_ready = if require_test_route {
        snapshot.route_test_ready
    } else {
        snapshot.route_active
    };
    if !route_ready {
        return RatingRuntimeReadiness::Unready("runtime-route-unready");
    }
    if snapshot.sqm_runtime_managed && !snapshot.sqm_runtime_healthy {
        return RatingRuntimeReadiness::Unready("runtime-sqm-unready");
    }
    if !snapshot.transport_probe_trusted {
        return RatingRuntimeReadiness::Unready("runtime-transport-untrusted");
    }
    if !snapshot.baseline_ready {
        return RatingRuntimeReadiness::Unready("runtime-baseline-unready");
    }
    RatingRuntimeReadiness::Ready
}

fn authoritative_rating_snapshot(
    snapshot: &RatingRuntimeSnapshot,
    now_ms: u64,
    require_test_route: bool,
    expected_runtime_generation: Option<u64>,
) -> Result<Option<&RatingRuntimeSnapshot>, String> {
    match rating_runtime_readiness(
        snapshot,
        now_ms,
        require_test_route,
        expected_runtime_generation,
    ) {
        RatingRuntimeReadiness::Ready => Ok(Some(snapshot)),
        RatingRuntimeReadiness::Stale => Ok(None),
        RatingRuntimeReadiness::Unready(code) => Err(code.to_string()),
    }
}

fn positive_reference(primary: f64, fallback: f64) -> Result<f64, String> {
    let value = if primary > 0.0 { primary } else { fallback };
    if value.is_finite() && value > 0.0 {
        Ok(value)
    } else {
        Err("rate-reference-unavailable".to_string())
    }
}

pub(crate) fn read_rating_snapshot(path: &Path) -> Result<RatingRuntimeSnapshot, String> {
    RatingRuntimeSnapshot::decode(&read_private_bounded(path, MAX_SNAPSHOT_BYTES)?)
}

fn read_rating_snapshot_if_present(path: &Path) -> Result<Option<RatingRuntimeSnapshot>, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "unable to inspect private rating runtime snapshot: {error}"
            ))
        }
    }
    match read_rating_snapshot(path) {
        Ok(snapshot) => Ok(Some(snapshot)),
        Err(error) => match fs::symlink_metadata(path) {
            Err(missing) if missing.kind() == io::ErrorKind::NotFound => Ok(None),
            _ => Err(error),
        },
    }
}

fn wait_for_rating_snapshot(
    snapshot_path: &Path,
    deadline_unix_ms: u64,
    terminate: &AtomicBool,
) -> Result<RatingRuntimeSnapshot, String> {
    while epoch_ms()? < deadline_unix_ms {
        if terminate.load(Ordering::Relaxed) {
            return Err("rating-cancelled".to_string());
        }
        if let Some(snapshot) = read_rating_snapshot_if_present(snapshot_path)? {
            return Ok(snapshot);
        }
        thread::sleep(POLL_INTERVAL);
    }
    Err("operation-deadline-expired-waiting-for-rating-runtime".to_string())
}

pub(crate) fn read_private_request(path: &Path) -> Result<OperationRequest, String> {
    let request = OperationRequest::decode(&read_private_bounded(path, MAX_REQUEST_BYTES)?)?;
    request.validate_admission_policy()?;
    Ok(request)
}

pub(crate) fn read_private_bounded(path: &Path, limit: usize) -> Result<String, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("unable to inspect private operation file: {error}"))?;
    let parent = path
        .parent()
        .ok_or_else(|| "operation file has no parent directory".to_string())?;
    let parent_metadata = fs::symlink_metadata(parent)
        .map_err(|error| format!("unable to inspect operation file directory: {error}"))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o077 != 0
        || !parent_metadata.is_dir()
        || parent_metadata.file_type().is_symlink()
        || parent_metadata.permissions().mode() & 0o077 != 0
        || metadata.uid() != parent_metadata.uid()
    {
        return Err("operation file is not a private regular file".to_string());
    }
    if metadata.len() > limit as u64 {
        return Err("operation file exceeds its size bound".to_string());
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|file| file.take((limit + 1) as u64).read_to_end(&mut bytes))
        .map_err(|error| format!("unable to read private operation file: {error}"))?;
    if bytes.len() > limit {
        return Err("operation file exceeds its size bound".to_string());
    }
    String::from_utf8(bytes).map_err(|_| "operation file is not UTF-8".to_string())
}

pub(crate) fn atomic_private_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "operation output has no parent directory".to_string())?;
    let metadata = fs::symlink_metadata(parent)
        .map_err(|error| format!("unable to inspect operation output directory: {error}"))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err("operation output directory is not private".to_string());
    }
    let tmp = parent.join(format!(".operation-write-{}", std::process::id()));
    if tmp.exists() || tmp.symlink_metadata().is_ok() {
        return Err("operation output staging path already exists".to_string());
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|error| format!("unable to create operation output: {error}"))?;
    let result = file
        .write_all(bytes)
        .and_then(|_| file.sync_all())
        .and_then(|_| fs::rename(&tmp, path))
        .and_then(|_| fs::File::open(parent))
        .and_then(|directory| directory.sync_all());
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(|error| format!("unable to publish operation output: {error}"))
}

pub fn remove_matching_capture(path: &Path, job_id: &str) -> Result<(), String> {
    require_exact_hex("job id", job_id, 32)?;
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("unable to inspect rating capture: {error}")),
    };
    if content.split('|').next() == Some(job_id) {
        fs::remove_file(path)
            .map_err(|error| format!("unable to remove rating capture: {error}"))?;
    }
    Ok(())
}

pub(crate) fn epoch_ms() -> Result<u64, String> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock predates the Unix epoch".to_string())?;
    u64::try_from(duration.as_millis()).map_err(|_| "system clock exceeds u64".to_string())
}

pub(crate) fn bounded_error_code(error: &str) -> String {
    let candidate: String = error
        .chars()
        .take(64)
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '-'
            }
        })
        .collect();
    if candidate.is_empty() {
        "rating-worker-failed".to_string()
    } else {
        candidate
    }
}

pub(crate) fn require_exact_hex(name: &str, value: &str, length: usize) -> Result<(), String> {
    if value.len() != length
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(format!(
            "{name} must be exactly {length} lowercase hexadecimal characters"
        ));
    }
    Ok(())
}

pub(crate) fn capture_job_id_is_valid(value: &str) -> bool {
    require_exact_hex("capture job id", value, 32).is_ok()
}

fn field(lines: &mut std::str::Lines<'_>, expected: &str) -> Result<String, String> {
    let line = lines
        .next()
        .ok_or_else(|| format!("rating runtime field {expected} is missing"))?;
    let (name, value) = line
        .split_once('=')
        .ok_or_else(|| format!("rating runtime field {expected} is malformed"))?;
    if name != expected || value.contains(['\n', '\r', '=']) {
        return Err(format!("expected rating runtime field {expected}"));
    }
    Ok(value.to_string())
}

fn terminal_field(lines: &mut std::str::Lines<'_>, expected: &str) -> Result<String, String> {
    let line = lines
        .next()
        .ok_or_else(|| format!("rating terminal field {expected} is missing"))?;
    let (name, value) = line
        .split_once('=')
        .ok_or_else(|| format!("rating terminal field {expected} is malformed"))?;
    if name != expected || value.contains(['\n', '\r', '=']) {
        return Err(format!("expected rating terminal field {expected}"));
    }
    Ok(value.to_string())
}

fn terminal_boolean(lines: &mut std::str::Lines<'_>, name: &str) -> Result<bool, String> {
    match terminal_field(lines, name)?.as_str() {
        "1" => Ok(true),
        "0" => Ok(false),
        _ => Err(format!("rating terminal field {name} is not boolean")),
    }
}

fn terminal_number(lines: &mut std::str::Lines<'_>, name: &str) -> Result<u64, String> {
    let value = terminal_field(lines, name)?;
    if value.len() > 20 || value.starts_with('+') || (value.starts_with('0') && value.len() > 1) {
        return Err(format!("rating terminal field {name} is not canonical"));
    }
    value
        .parse::<u64>()
        .map_err(|_| format!("rating terminal field {name} is not an integer"))
}

fn terminal_decimal(lines: &mut std::str::Lines<'_>, name: &str) -> Result<f64, String> {
    let value = terminal_field(lines, name)?;
    if value.len() > 32 || value.starts_with('+') {
        return Err(format!("rating terminal field {name} is not canonical"));
    }
    let number = value
        .parse::<f64>()
        .map_err(|_| format!("rating terminal field {name} is not decimal"))?;
    if !number.is_finite() || number < 0.0 || number > 1_000_000.0 {
        return Err(format!("rating terminal field {name} is outside its bound"));
    }
    Ok(number)
}

fn boolean(lines: &mut std::str::Lines<'_>, name: &str) -> Result<bool, String> {
    match field(lines, name)?.as_str() {
        "1" => Ok(true),
        "0" => Ok(false),
        _ => Err(format!("rating runtime field {name} is not boolean")),
    }
}

fn number(lines: &mut std::str::Lines<'_>, name: &str) -> Result<u64, String> {
    let value = field(lines, name)?;
    if value.len() > 20 || value.starts_with('+') || (value.starts_with('0') && value.len() > 1) {
        return Err(format!("rating runtime field {name} is not canonical"));
    }
    value
        .parse::<u64>()
        .map_err(|_| format!("rating runtime field {name} is not an integer"))
}

fn decimal(lines: &mut std::str::Lines<'_>, name: &str) -> Result<f64, String> {
    let value = field(lines, name)?;
    if value.len() > 32 || value.starts_with('+') {
        return Err(format!("rating runtime field {name} is not canonical"));
    }
    value
        .parse::<f64>()
        .map_err(|_| format!("rating runtime field {name} is not decimal"))
}

fn finite_text(value: f64) -> String {
    format!("{value:.3}")
}

fn optional_qdisc_kind(value: Option<RuntimeQdiscKind>) -> String {
    value
        .map(|kind| kind.as_str().to_string())
        .unwrap_or_default()
}

fn parse_optional_qdisc_kind(name: &str, value: &str) -> Result<Option<RuntimeQdiscKind>, String> {
    if value.is_empty() {
        return Ok(None);
    }
    RuntimeQdiscKind::parse(value)
        .map(Some)
        .ok_or_else(|| format!("rating runtime field {name} has an unsupported qdisc kind"))
}

fn bool_text(value: bool) -> &'static str {
    if value {
        "1"
    } else {
        "0"
    }
}

fn require_identifier(name: &str, value: &str, extra: &[u8]) -> Result<(), String> {
    if value.len() > 64
        || (!value.is_empty()
            && !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || extra.contains(&byte)))
    {
        return Err(format!("{name} is not a bounded identifier"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(1);

    fn test_dir(label: &str) -> PathBuf {
        let suffix = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "cake-autorate-rating-{label}-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[test]
    fn rating_worker_rejects_autotune_review_path() {
        let terminate = AtomicBool::new(false);
        let error = run_rating_worker(
            ["--review".to_string(), "/tmp/review.json".to_string()].into_iter(),
            &terminate,
        )
        .unwrap_err();
        assert_eq!(error, "unsupported rating worker argument: --review");
    }

    fn snapshot() -> RatingRuntimeSnapshot {
        RatingRuntimeSnapshot {
            updated_unix_ms: 1_785_568_000_000,
            capture_observed_unix_ms: 1_785_568_000_000,
            runtime_generation: 17,
            uplink_state: "ACTIVE".to_string(),
            route_active: true,
            route_test_ready: true,
            sqm_runtime_managed: true,
            sqm_runtime_healthy: true,
            transport_probe_trusted: true,
            baseline_ready: true,
            baseline_samples: 20,
            baseline_required_samples: 20,
            required_samples: 20,
            evidence_contract: RatingEvidenceContract::WorstOfDirectionBoundIcmpAndTransport,
            dl_samples: 20,
            ul_samples: 21,
            dl_achieved_kbps: 500_000.0,
            ul_achieved_kbps: 100_000.0,
            cake_dl_kbps: 600_000.0,
            cake_ul_kbps: 120_000.0,
            download_qdisc_kind: Some(RuntimeQdiscKind::Cake),
            upload_qdisc_kind: Some(RuntimeQdiscKind::Cake),
            reference_dl_kbps: 600_000.0,
            reference_ul_kbps: 120_000.0,
            capture_active: true,
            capture_job_id: "a".repeat(32),
            capture_generation: 7,
            finalized_job_id: String::new(),
            finalized_generation: 0,
            finalized_outcome: String::new(),
            capture_phase: "AUTO".to_string(),
            capture_contaminated: false,
            current_capture_job_id: "a".repeat(32),
            current_capture_generation: 7,
            current: Some(RatingResultSnapshot {
                grade: "A+".to_string(),
                increase_ms: 4.25,
                started_unix_ms: 1_785_568_000_000,
                partial: false,
                incomplete: false,
                dl_grade: "A+".to_string(),
                ul_grade: "A".to_string(),
                dl_samples: 20,
                ul_samples: 21,
            }),
        }
    }

    #[test]
    fn rating_runtime_snapshot_round_trips_canonically() {
        let encoded = snapshot().encode().unwrap();
        assert!(encoded.starts_with(SNAPSHOT_HEADER));
        assert_eq!(RatingRuntimeSnapshot::decode(&encoded).unwrap(), snapshot());

        let mut cake_mq = snapshot();
        cake_mq.download_qdisc_kind = Some(RuntimeQdiscKind::CakeMq);
        cake_mq.upload_qdisc_kind = Some(RuntimeQdiscKind::CakeMq);
        let encoded_cake_mq = cake_mq.encode().unwrap();
        assert!(encoded_cake_mq.contains("download_qdisc_kind=cake_mq\n"));
        assert!(encoded_cake_mq.contains("upload_qdisc_kind=cake_mq\n"));
        assert_eq!(
            RatingRuntimeSnapshot::decode(&encoded_cake_mq).unwrap(),
            cake_mq
        );
    }

    #[test]
    fn rating_runtime_snapshot_rejects_reordering_unknowns_and_nonfinite_values() {
        let encoded = snapshot().encode().unwrap();
        assert!(RatingRuntimeSnapshot::decode(&encoded.replacen(
            "route_active=1\nroute_test_ready=1",
            "route_test_ready=1\nroute_active=1",
            1
        ))
        .is_err());
        assert!(RatingRuntimeSnapshot::decode(&(encoded.clone() + "unknown=1\n")).is_err());
        assert!(RatingRuntimeSnapshot::decode(
            &encoded.replace("dl_achieved_kbps=500000.000", "dl_achieved_kbps=NaN")
        )
        .is_err());
        assert!(RatingRuntimeSnapshot::decode(
            &encoded.replace("download_qdisc_kind=cake", "download_qdisc_kind=foreign")
        )
        .is_err());

        let mut contradictory = snapshot();
        contradictory.cake_dl_kbps = 0.0;
        assert!(contradictory.validate().is_err());

        let mut fractional_intent = snapshot();
        fractional_intent.cake_ul_kbps = 19_999.375;
        assert!(fractional_intent.validate().is_err());
    }

    #[test]
    fn rating_runtime_snapshot_preserves_absent_current_result() {
        let mut value = snapshot();
        value.current = None;
        value.current_capture_job_id.clear();
        value.current_capture_generation = 0;
        let encoded = value.encode().unwrap();
        assert_eq!(RatingRuntimeSnapshot::decode(&encoded).unwrap(), value);
    }

    #[test]
    fn rating_runtime_capture_identities_are_exact_lowercase_hex() {
        let mut value = snapshot();
        assert!(value.validate().is_ok());

        value.capture_job_id = "1785596800-1234".to_string();
        assert!(value.validate().is_err());

        value = snapshot();
        value.finalized_job_id = "A".repeat(32);
        value.finalized_generation = 9;
        value.finalized_outcome = "removed".to_string();
        assert!(value.validate().is_err());

        value = snapshot();
        value.current_capture_job_id = "g".repeat(32);
        assert!(value.validate().is_err());

        assert!(capture_job_id_is_valid("0123456789abcdef0123456789abcdef"));
        assert!(!capture_job_id_is_valid("01234567-89abcdef"));
        assert!(!capture_job_id_is_valid("0123456789ABCDEF0123456789ABCDEF"));
    }

    #[test]
    fn invalid_runtime_publication_removes_stale_snapshot() {
        let directory = test_dir("invalid-publication");
        let path = directory.join("rating-runtime");
        fs::write(&path, snapshot().encode().unwrap()).unwrap();

        let mut invalid = snapshot();
        invalid.capture_job_id = "invalid-1234".to_string();
        let error = invalid.write_atomic(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!path.exists());

        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn runtime_io_failure_also_removes_stale_snapshot() {
        let directory = test_dir("io-publication-failure");
        let path = directory.join("rating-runtime");
        let value = snapshot();
        fs::write(&path, value.encode().unwrap()).unwrap();
        let staging = directory.join(format!(
            ".rating-runtime-{}-{}.tmp",
            std::process::id(),
            value.updated_unix_ms
        ));
        fs::create_dir(&staging).unwrap();

        assert!(value.write_atomic(&path).is_err());
        assert!(!path.exists());

        fs::remove_dir(staging).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn absent_runtime_snapshot_is_a_waitable_state() {
        let directory = test_dir("absent-runtime");
        let path = directory.join("rating-runtime");
        assert_eq!(read_rating_snapshot_if_present(&path).unwrap(), None);

        snapshot().write_atomic(&path).unwrap();
        assert_eq!(
            read_rating_snapshot_if_present(&path).unwrap(),
            Some(snapshot())
        );

        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn in_flight_runtime_wait_survives_a_missing_publication() {
        let directory = test_dir("runtime-republication");
        let path = directory.join("rating-runtime");
        let publish_path = path.clone();
        let publisher = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            snapshot().write_atomic(&publish_path).unwrap();
        });
        let terminate = AtomicBool::new(false);
        // The deadline is only a watchdog for a broken waiter.  Keep it well
        // above host scheduling jitter so a loaded serial source gate cannot
        // turn a valid 20 ms republication into time-based test authority.
        let result = wait_for_rating_snapshot(&path, epoch_ms().unwrap() + 10_000, &terminate)
            .expect("temporary snapshot absence must remain waitable");
        assert_eq!(result, snapshot());
        publisher.join().unwrap();

        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn quiet_window_requires_consecutive_samples_and_resets_its_average() {
        let mut window = QuietWindow::new(3);
        assert_eq!(window.observe(10.0, 20.0, 100.0, 100.0), None);
        assert_eq!(window.observe(20.0, 30.0, 100.0, 100.0), None);
        assert_eq!(window.observe(101.0, 1.0, 100.0, 100.0), None);
        assert_eq!(window.observe(30.0, 40.0, 100.0, 100.0), None);
        assert_eq!(window.observe(60.0, 70.0, 100.0, 100.0), None);
        assert_eq!(
            window.observe(90.0, 100.0, 100.0, 100.0),
            Some((60.0, 70.0))
        );
    }

    #[test]
    fn automatic_capture_phases_are_exact_and_job_owned() {
        let directory = test_dir("automatic-phases");
        let path = directory.join("rating-capture");
        let job_id = "0123456789abcdef0123456789abcdef";
        let mut capture = CaptureGuard::new(path.clone(), job_id.to_string());
        capture
            .arm("automatic", "IDLE", 123_999, 12.25, 3.5)
            .unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("{job_id}|automatic|123|IDLE|12.250|3.500\n")
        );
        capture.set_phase("DL").unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("{job_id}|automatic|123|DL|12.250|3.500\n")
        );
        assert!(capture.set_phase("AUTO\nforeign").is_err());
        capture.cleanup().unwrap();
        assert!(!path.exists());
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn automatic_rating_requires_test_route_and_directional_samples() {
        let mut value = snapshot();
        assert_eq!(
            rating_runtime_readiness(
                &value,
                value.updated_unix_ms,
                true,
                Some(value.runtime_generation)
            ),
            RatingRuntimeReadiness::Ready
        );
        assert_eq!(
            rating_runtime_readiness(
                &value,
                value.updated_unix_ms,
                true,
                Some(value.runtime_generation + 1)
            ),
            RatingRuntimeReadiness::Unready("runtime-generation-changed")
        );
        value.route_test_ready = false;
        assert_eq!(
            rating_runtime_readiness(
                &value,
                value.updated_unix_ms,
                false,
                Some(value.runtime_generation)
            ),
            RatingRuntimeReadiness::Ready
        );
        assert_eq!(
            rating_runtime_readiness(
                &value,
                value.updated_unix_ms,
                true,
                Some(value.runtime_generation)
            ),
            RatingRuntimeReadiness::Unready("runtime-route-unready")
        );

        value.route_test_ready = true;
        value.route_active = false;
        value.uplink_state = "STANDBY".to_string();
        assert_eq!(
            rating_runtime_readiness(
                &value,
                value.updated_unix_ms,
                true,
                Some(value.runtime_generation)
            ),
            RatingRuntimeReadiness::Ready
        );
        assert_eq!(
            rating_runtime_readiness(
                &value,
                value.updated_unix_ms,
                false,
                Some(value.runtime_generation)
            ),
            RatingRuntimeReadiness::Unready("runtime-route-unready")
        );

        value.dl_samples = value.required_samples;
        value.ul_samples = value.required_samples - 1;
        assert!(direction_samples_ready(
            &value,
            SpeedtestDirection::Download
        ));
        assert!(!direction_samples_ready(&value, SpeedtestDirection::Upload));
        assert!(!direction_samples_ready(&value, SpeedtestDirection::Both));
    }

    #[test]
    fn stale_runtime_is_waitable_but_fresh_safety_failures_remain_terminal() {
        let value = snapshot();
        let now = value.updated_unix_ms;
        for stale in [
            {
                let mut stale = value.clone();
                stale.updated_unix_ms = now - 5_001;
                stale
            },
            {
                let mut stale = value.clone();
                stale.capture_observed_unix_ms = now - 5_001;
                stale
            },
            {
                let mut stale = value.clone();
                stale.updated_unix_ms = now - 5_001;
                stale.capture_observed_unix_ms = now - 5_001;
                stale.route_test_ready = false;
                stale
            },
        ] {
            assert_eq!(
                rating_runtime_readiness(&stale, now, true, Some(value.runtime_generation)),
                RatingRuntimeReadiness::Stale
            );
            assert_eq!(
                authoritative_rating_snapshot(&stale, now, true, Some(value.runtime_generation))
                    .unwrap(),
                None
            );
        }

        let cases = [
            (
                {
                    let mut unsafe_value = value.clone();
                    unsafe_value.runtime_generation += 1;
                    unsafe_value
                },
                "runtime-generation-changed",
            ),
            (
                {
                    let mut unsafe_value = value.clone();
                    unsafe_value.uplink_state = "OFFLINE".to_string();
                    unsafe_value
                },
                "runtime-uplink-unready",
            ),
            (
                {
                    let mut unsafe_value = value.clone();
                    unsafe_value.route_test_ready = false;
                    unsafe_value
                },
                "runtime-route-unready",
            ),
            (
                {
                    let mut unsafe_value = value.clone();
                    unsafe_value.sqm_runtime_healthy = false;
                    unsafe_value
                },
                "runtime-sqm-unready",
            ),
            (
                {
                    let mut unsafe_value = value.clone();
                    unsafe_value.transport_probe_trusted = false;
                    unsafe_value
                },
                "runtime-transport-untrusted",
            ),
            (
                {
                    let mut unsafe_value = value.clone();
                    unsafe_value.baseline_ready = false;
                    unsafe_value
                },
                "runtime-baseline-unready",
            ),
        ];
        for (unsafe_value, code) in cases {
            assert_eq!(
                authoritative_rating_snapshot(
                    &unsafe_value,
                    now,
                    true,
                    Some(value.runtime_generation)
                ),
                Err(code.to_string())
            );
        }
    }

    #[test]
    fn stale_runtime_cannot_supply_samples_or_finalization_evidence() {
        let mut value = snapshot();
        let now = value.updated_unix_ms + 5_001;
        value.dl_samples = value.required_samples;
        value.ul_samples = value.required_samples;
        value.finalized_job_id = value.capture_job_id.clone();
        value.finalized_generation = value.capture_generation;
        value.finalized_outcome = "removed".to_string();

        assert_eq!(
            authoritative_rating_snapshot(&value, now, true, Some(value.runtime_generation))
                .unwrap(),
            None
        );
    }

    #[test]
    fn result_ownership_uses_capture_identity_not_wall_clock() {
        let mut value = snapshot();
        let job_id = value.capture_job_id.clone();
        let generation = value.capture_generation;
        value.current.as_mut().unwrap().started_unix_ms = 1;
        assert!(owned_complete_result(&value, &job_id, generation).is_some());

        value.current.as_mut().unwrap().started_unix_ms = u64::MAX;
        value.current_capture_job_id = "b".repeat(32);
        assert!(owned_complete_result(&value, &job_id, generation).is_none());
    }

    #[test]
    fn finalization_ack_is_exact_and_outcome_qualified() {
        let mut value = snapshot();
        let job_id = value.capture_job_id.clone();
        let generation = value.capture_generation;
        value.finalized_job_id = job_id.clone();
        value.finalized_generation = generation;

        value.finalized_outcome = "contaminated".to_string();
        assert_eq!(
            matching_finalization(&value, &job_id, generation),
            Some(Err("capture-contaminated"))
        );
        value.finalized_outcome = "expired".to_string();
        assert_eq!(
            matching_finalization(&value, &job_id, generation),
            Some(Err("capture-expired"))
        );
        value.finalized_outcome = "removed".to_string();
        assert_eq!(
            matching_finalization(&value, &job_id, generation),
            Some(Ok(()))
        );
        assert_eq!(matching_finalization(&value, &job_id, generation + 1), None);
    }

    #[test]
    fn capture_identity_freshness_is_independent_from_publication_freshness() {
        let mut value = snapshot();
        value.capture_observed_unix_ms = value.updated_unix_ms - 5_001;
        assert_eq!(
            rating_runtime_readiness(
                &value,
                value.updated_unix_ms,
                true,
                Some(value.runtime_generation)
            ),
            RatingRuntimeReadiness::Stale
        );
    }

    #[test]
    fn retired_snapshot_headers_are_rejected_explicitly() {
        let current = snapshot().encode().unwrap();
        for version in [2, 3, 4] {
            let retired = current.replacen(
                SNAPSHOT_HEADER,
                &format!("cake-autorate-rating-runtime\t{version}"),
                1,
            );
            assert_eq!(
                RatingRuntimeSnapshot::decode(&retired),
                Err("unsupported rating runtime snapshot header".to_string())
            );
        }
    }

    #[test]
    fn cumulative_automatic_budget_fails_closed() {
        let mut remaining = 1_000;
        consume_traffic_budget(&mut remaining, 600, 200).unwrap();
        assert_eq!(remaining, 200);
        assert!(consume_traffic_budget(&mut remaining, 201, 0).is_err());
        assert_eq!(remaining, 200);
        assert!(consume_traffic_budget(&mut remaining, u64::MAX, 1).is_err());
        assert_eq!(remaining, 200);
    }

    #[test]
    fn permit_is_exact_and_bound_to_job_worker_and_path() {
        let job_id = "0123456789abcdef0123456789abcdef";
        let worker_run_id = "abcdef0123456789abcdef0123456789";
        let encoded = encode_permit(job_id, worker_run_id).unwrap();
        assert!(decode_permit(&encoded, job_id, worker_run_id).is_ok());
        assert!(
            decode_permit(&encoded, "1123456789abcdef0123456789abcdef", worker_run_id).is_err()
        );
        assert!(decode_permit(&encoded, job_id, "bbcdef0123456789abcdef0123456789").is_err());
        assert!(decode_permit(&(encoded + "unknown=1\n"), job_id, worker_run_id).is_err());
    }

    #[test]
    fn exact_capture_cleanup_preserves_foreign_owner() {
        let directory = test_dir("capture-cleanup");
        let path = directory.join("rating-capture");
        let owned = "0123456789abcdef0123456789abcdef";
        let foreign = "abcdef0123456789abcdef0123456789";

        fs::write(&path, format!("{foreign}|client|1|AUTO|0|0\n")).unwrap();
        remove_matching_capture(&path, owned).unwrap();
        assert!(path.exists());

        fs::write(&path, format!("{owned}|client|1|AUTO|0|0\n")).unwrap();
        remove_matching_capture(&path, owned).unwrap();
        assert!(!path.exists());
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn terminal_record_carries_exact_operation_identity() {
        let job_id = "0123456789abcdef0123456789abcdef";
        let worker_run_id = "abcdef0123456789abcdef0123456789";
        let encoded = RatingTerminal::Incomplete {
            dl_samples: 7,
            ul_samples: 9,
        }
        .encode(job_id, worker_run_id)
        .unwrap();
        assert!(encoded.starts_with(TERMINAL_HEADER));
        assert!(encoded.contains(&format!("evidence_contract={RATING_EVIDENCE_CONTRACT}\n")));
        assert!(encoded.contains(&format!("job_id={job_id}\n")));
        assert!(encoded.contains(&format!("worker_run_id={worker_run_id}\n")));
        assert!(encoded.contains("state=incomplete\n"));
        assert!(encoded.contains("dl_samples=7\nul_samples=9\n"));
        assert_eq!(
            RatingTerminalRecord::decode(&encoded).unwrap(),
            RatingTerminalRecord {
                job_id: job_id.to_string(),
                worker_run_id: worker_run_id.to_string(),
                terminal: RatingTerminal::Incomplete {
                    dl_samples: 7,
                    ul_samples: 9,
                },
            }
        );
        assert!(RatingTerminalRecord::decode(&(encoded + "unknown=1\n")).is_err());
    }

    #[test]
    fn retired_terminal_header_cannot_be_relabelled_as_current_combined_evidence() {
        let job_id = "0123456789abcdef0123456789abcdef";
        let worker_run_id = "abcdef0123456789abcdef0123456789";
        let encoded = RatingTerminal::Complete(RatingResultSnapshot {
            grade: "A".to_string(),
            increase_ms: 10.0,
            started_unix_ms: 1,
            partial: false,
            incomplete: false,
            dl_grade: "A".to_string(),
            ul_grade: "A".to_string(),
            dl_samples: 20,
            ul_samples: 20,
        })
        .encode(job_id, worker_run_id)
        .unwrap();
        let retired = encoded.replacen(TERMINAL_HEADER, "cake-autorate-rating-terminal\t1", 1);
        assert_eq!(
            RatingTerminalRecord::decode(&retired),
            Err("rating terminal header is unsupported".to_string())
        );
    }

    #[test]
    fn runtime_preflight_requires_two_distinct_publications_of_one_generation() {
        let mut previous = None;
        let mut current = snapshot();
        current.runtime_generation = 41;
        current.updated_unix_ms = 1_000;
        assert!(!observe_stable_runtime_publication(&mut previous, &current));
        assert!(!observe_stable_runtime_publication(&mut previous, &current));
        current.updated_unix_ms = 1_001;
        assert!(observe_stable_runtime_publication(&mut previous, &current));
        current.runtime_generation = 42;
        current.updated_unix_ms = 1_002;
        assert!(!observe_stable_runtime_publication(&mut previous, &current));
        current.updated_unix_ms = 1_003;
        assert!(observe_stable_runtime_publication(&mut previous, &current));
    }

    #[test]
    fn contamination_is_bound_to_the_exact_capture_owner() {
        let mut current = snapshot();
        current.capture_active = true;
        current.capture_contaminated = true;
        current.capture_job_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
        current.capture_generation = 7;
        assert!(owned_capture_contaminated(
            &current,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            7
        ));
        assert!(!owned_capture_contaminated(
            &current,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            7
        ));
        assert!(!owned_capture_contaminated(
            &current,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            8
        ));
        current.capture_active = false;
        assert!(!owned_capture_contaminated(
            &current,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            7
        ));
    }
}
