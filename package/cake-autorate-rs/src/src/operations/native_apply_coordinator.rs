//! Durable coordinator ownership for interactive native Apply.
//!
//! The browser-facing CLI publishes a bounded request to the long-lived
//! calibration coordinator.  The coordinator acknowledges a durable handle
//! before any UCI or runtime mutation and drives the existing native Apply
//! transaction only on the following state-machine tick.  The native Apply
//! recovery record remains the sole configuration-transaction authority; this
//! module stores only dispatch identity and the final public receipt.

use super::autotune_apply::{
    validate_native_apply_option_id, NativeApplyAcknowledgement, MAX_NATIVE_APPLY_ACKNOWLEDGEMENTS,
};
use super::autotune_apply_runtime::{
    ensure_private_directory, path_exists, read_private_recovery_bounded, replace_private_file,
    sync_directory, write_new_private_file,
};
use super::identity::ProcessIdentity;
use std::fs;
use std::path::{Path, PathBuf};

const CONTROL_HEADER_BASE: &str = "cake-autorate-native-apply\t1\tcontrol";
const CONTROL_HEADER_WATCH: &str = "cake-autorate-native-apply\t2\tcontrol";
const DISPATCH_HEADER: &str = "cake-autorate-native-apply\t2\tdispatch";
const TERMINAL_HEADER: &str = "cake-autorate-native-apply\t2\tterminal";
const STORE_DIRECTORY: &str = "coordinator";
const ACTIVE_FILE: &str = "active";
const ACTIVE_NEXT_FILE: &str = ".active.next";
const TERMINAL_FILE: &str = "terminal";
const TERMINAL_NEXT_FILE: &str = ".terminal.next";
const WORKER_CLAIM_FILE: &str = "worker-claim";
const WORKER_CLAIM_HEADER: &str = "cake-autorate-native-apply\t1\tworker-claim";
const MAX_RECORD_BYTES: usize = 32 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeApplyControlCommand {
    Start,
    Status,
    Watch,
    Result,
}

impl NativeApplyControlCommand {
    fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Status => "status",
            Self::Watch => "watch",
            Self::Result => "result",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "start" => Some(Self::Start),
            "status" => Some(Self::Status),
            "watch" => Some(Self::Watch),
            "result" => Some(Self::Result),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeApplyControlRequest {
    pub(crate) request_id: String,
    pub(crate) command: NativeApplyControlCommand,
    pub(crate) source_job_id: Option<String>,
    pub(crate) apply_job_id: Option<String>,
    pub(crate) apply_job_token: Option<String>,
    pub(crate) option_id: Option<String>,
    pub(crate) review_sha256: Option<String>,
    pub(crate) manifest_sha256: Option<String>,
    pub(crate) acknowledgements: Vec<NativeApplyAcknowledgement>,
    pub(crate) observed_generation: Option<u64>,
}

impl NativeApplyControlRequest {
    pub(crate) fn start(
        request_id: String,
        source_job_id: String,
        option_id: String,
        review_sha256: String,
        manifest_sha256: String,
        acknowledgements: Vec<NativeApplyAcknowledgement>,
    ) -> Self {
        Self {
            request_id,
            command: NativeApplyControlCommand::Start,
            source_job_id: Some(source_job_id),
            apply_job_id: None,
            apply_job_token: None,
            option_id: Some(option_id),
            review_sha256: Some(review_sha256),
            manifest_sha256: Some(manifest_sha256),
            acknowledgements,
            observed_generation: None,
        }
    }

    pub(crate) fn query(
        request_id: String,
        command: NativeApplyControlCommand,
        apply_job_id: String,
        apply_job_token: String,
    ) -> Self {
        Self {
            request_id,
            command,
            source_job_id: None,
            apply_job_id: Some(apply_job_id),
            apply_job_token: Some(apply_job_token),
            option_id: None,
            review_sha256: None,
            manifest_sha256: None,
            acknowledgements: Vec::new(),
            observed_generation: None,
        }
    }

    pub(crate) fn watch(
        request_id: String,
        apply_job_id: String,
        apply_job_token: String,
        observed_generation: u64,
    ) -> Self {
        let mut request = Self::query(
            request_id,
            NativeApplyControlCommand::Watch,
            apply_job_id,
            apply_job_token,
        );
        request.observed_generation = Some(observed_generation);
        request
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        require_lower_hex("native Apply control request ID", &self.request_id, 32)?;
        require_sorted_acknowledgements(&self.acknowledgements)?;
        match self.command {
            NativeApplyControlCommand::Start => {
                require_lower_hex(
                    "native Apply source job ID",
                    required(&self.source_job_id, "source job ID")?,
                    32,
                )?;
                validate_native_apply_option_id(required(&self.option_id, "option ID")?)?;
                require_lower_hex(
                    "native Apply Review digest",
                    required(&self.review_sha256, "Review digest")?,
                    64,
                )?;
                require_lower_hex(
                    "native Apply manifest digest",
                    required(&self.manifest_sha256, "manifest digest")?,
                    64,
                )?;
                if self.apply_job_id.is_some() || self.apply_job_token.is_some() {
                    return Err("native Apply start must not carry an Apply handle".to_string());
                }
                if self.observed_generation.is_some() {
                    return Err("native Apply start must not carry a generation".to_string());
                }
            }
            NativeApplyControlCommand::Status
            | NativeApplyControlCommand::Watch
            | NativeApplyControlCommand::Result => {
                require_lower_hex(
                    "native Apply job ID",
                    required(&self.apply_job_id, "Apply job ID")?,
                    32,
                )?;
                require_lower_hex(
                    "native Apply job token",
                    required(&self.apply_job_token, "Apply job token")?,
                    64,
                )?;
                if self.source_job_id.is_some()
                    || self.option_id.is_some()
                    || self.review_sha256.is_some()
                    || self.manifest_sha256.is_some()
                    || !self.acknowledgements.is_empty()
                {
                    return Err(
                        "native Apply status/result must carry only its Apply handle".to_string(),
                    );
                }
                match self.command {
                    NativeApplyControlCommand::Watch => {
                        if self.observed_generation.is_none() {
                            return Err(
                                "native Apply watch requires an observed generation".to_string()
                            );
                        }
                    }
                    _ if self.observed_generation.is_some() => {
                        return Err(
                            "native Apply status/result must not carry a generation".to_string()
                        )
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    pub(crate) fn encode(&self) -> Result<String, String> {
        self.validate()?;
        let watch = self.command == NativeApplyControlCommand::Watch;
        let mut output = format!(
            concat!(
                "{}\n",
                "request_id={}\n",
                "command={}\n",
                "source_job_id={}\n",
                "apply_job_id={}\n",
                "apply_job_token={}\n",
                "option_id={}\n",
                "review_sha256={}\n",
                "manifest_sha256={}\n",
                "{}",
                "acknowledgement_count={}\n"
            ),
            if watch {
                CONTROL_HEADER_WATCH
            } else {
                CONTROL_HEADER_BASE
            },
            self.request_id,
            self.command.as_str(),
            optional(&self.source_job_id),
            optional(&self.apply_job_id),
            optional(&self.apply_job_token),
            optional(&self.option_id),
            optional(&self.review_sha256),
            optional(&self.manifest_sha256),
            if watch {
                format!(
                    "observed_generation={}\n",
                    self.observed_generation
                        .expect("validated native Apply watch has a generation")
                )
            } else {
                String::new()
            },
            self.acknowledgements.len(),
        );
        append_acknowledgements(&mut output, &self.acknowledgements);
        output.push('\n');
        if output.len() > MAX_RECORD_BYTES {
            return Err("native Apply control request exceeds its bound".to_string());
        }
        Ok(output)
    }

    pub(crate) fn decode(input: &str) -> Result<Self, String> {
        if input.len() > MAX_RECORD_BYTES || !input.ends_with("\n\n") {
            return Err("native Apply control request is not bounded and terminated".to_string());
        }
        let mut lines = input.split('\n');
        let watch_schema = match lines.next() {
            Some(CONTROL_HEADER_BASE) => false,
            Some(CONTROL_HEADER_WATCH) => true,
            _ => return Err("native Apply control request has an unsupported header".to_string()),
        };
        let request_id = field(&mut lines, "request_id")?;
        let command = NativeApplyControlCommand::parse(&field(&mut lines, "command")?)
            .ok_or_else(|| "native Apply control command is unsupported".to_string())?;
        let source_job_id = optional_string(field(&mut lines, "source_job_id")?);
        let apply_job_id = optional_string(field(&mut lines, "apply_job_id")?);
        let apply_job_token = optional_string(field(&mut lines, "apply_job_token")?);
        let option_id = optional_string(field(&mut lines, "option_id")?);
        let review_sha256 = optional_string(field(&mut lines, "review_sha256")?);
        let manifest_sha256 = optional_string(field(&mut lines, "manifest_sha256")?);
        let observed_generation = if watch_schema {
            Some(
                field(&mut lines, "observed_generation")?
                    .parse::<u64>()
                    .map_err(|_| "native Apply observed generation is invalid".to_string())?,
            )
        } else {
            None
        };
        let count = parse_count(&field(&mut lines, "acknowledgement_count")?)?;
        let acknowledgements = read_acknowledgements(&mut lines, count)?;
        finish(&mut lines)?;
        let request = Self {
            request_id,
            command,
            source_job_id,
            apply_job_id,
            apply_job_token,
            option_id,
            review_sha256,
            manifest_sha256,
            acknowledgements,
            observed_generation,
        };
        request.validate()?;
        if !watch_schema && request.command == NativeApplyControlCommand::Watch {
            return Err("native Apply base control does not support watch".to_string());
        }
        if request.encode()? != input {
            return Err("native Apply control request is not canonical".to_string());
        }
        Ok(request)
    }

    pub(crate) fn has_header(input: &str) -> bool {
        input.starts_with(CONTROL_HEADER_BASE) || input.starts_with(CONTROL_HEADER_WATCH)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeApplyDispatchState {
    Accepted,
    Validating,
    Applying,
}

impl NativeApplyDispatchState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Validating => "validating",
            Self::Applying => "applying",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "accepted" => Some(Self::Accepted),
            "validating" => Some(Self::Validating),
            "applying" => Some(Self::Applying),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeApplyVerifiedDispatchIdentity {
    pub(crate) worker_run_id: String,
    pub(crate) source_manifest_sha256: String,
    pub(crate) manifest_schema_version: u8,
    pub(crate) target_state: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeApplyWorkerClaim {
    pub(crate) apply_job_id: String,
    pub(crate) source_job_id: String,
    pub(crate) dispatch_generation: u64,
    pub(crate) process: ProcessIdentity,
}

impl NativeApplyWorkerClaim {
    pub(crate) fn for_process(
        dispatch: &NativeApplyDispatchRecord,
        process: ProcessIdentity,
    ) -> Result<Self, String> {
        dispatch.validate()?;
        if !matches!(
            dispatch.state,
            NativeApplyDispatchState::Validating | NativeApplyDispatchState::Applying
        ) {
            return Err("native Apply worker claim requires executable state".to_string());
        }
        let claim = Self {
            apply_job_id: dispatch.apply_job_id.clone(),
            source_job_id: dispatch.source_job_id.clone(),
            dispatch_generation: dispatch.generation,
            process,
        };
        claim.validate()?;
        Ok(claim)
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        require_lower_hex("native Apply worker job ID", &self.apply_job_id, 32)?;
        require_lower_hex("native Apply worker source job ID", &self.source_job_id, 32)?;
        if self.dispatch_generation < 2
            || self.process.pid <= 1
            || self.process.process_group != self.process.pid
            || self.process.starttime_ticks == 0
        {
            return Err("native Apply worker claim identity is invalid".to_string());
        }
        Ok(())
    }

    pub(crate) fn matches_dispatch(&self, dispatch: &NativeApplyDispatchRecord) -> bool {
        self.apply_job_id == dispatch.apply_job_id
            && self.source_job_id == dispatch.source_job_id
            && self.dispatch_generation <= dispatch.generation
            && matches!(
                dispatch.state,
                NativeApplyDispatchState::Validating | NativeApplyDispatchState::Applying
            )
    }

    fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let output = format!(
            concat!(
                "{}\n",
                "apply_job_id={}\n",
                "source_job_id={}\n",
                "dispatch_generation={}\n",
                "pid={}\n",
                "process_group={}\n",
                "starttime_ticks={}\n\n"
            ),
            WORKER_CLAIM_HEADER,
            self.apply_job_id,
            self.source_job_id,
            self.dispatch_generation,
            self.process.pid,
            self.process.process_group,
            self.process.starttime_ticks,
        );
        if output.len() > MAX_RECORD_BYTES {
            return Err("native Apply worker claim exceeds its bound".to_string());
        }
        Ok(output.into_bytes())
    }

    fn decode(bytes: &[u8]) -> Result<Self, String> {
        let input = std::str::from_utf8(bytes)
            .map_err(|_| "native Apply worker claim is not UTF-8".to_string())?;
        if input.len() > MAX_RECORD_BYTES || !input.ends_with("\n\n") {
            return Err("native Apply worker claim is not bounded and terminated".to_string());
        }
        let mut lines = input.split('\n');
        if lines.next() != Some(WORKER_CLAIM_HEADER) {
            return Err("native Apply worker claim has an unsupported header".to_string());
        }
        let claim = Self {
            apply_job_id: field(&mut lines, "apply_job_id")?,
            source_job_id: field(&mut lines, "source_job_id")?,
            dispatch_generation: field(&mut lines, "dispatch_generation")?
                .parse::<u64>()
                .map_err(|_| "native Apply worker generation is invalid".to_string())?,
            process: ProcessIdentity {
                pid: field(&mut lines, "pid")?
                    .parse::<u32>()
                    .map_err(|_| "native Apply worker PID is invalid".to_string())?,
                process_group: field(&mut lines, "process_group")?
                    .parse::<u32>()
                    .map_err(|_| "native Apply worker process group is invalid".to_string())?,
                starttime_ticks: field(&mut lines, "starttime_ticks")?
                    .parse::<u64>()
                    .map_err(|_| "native Apply worker start time is invalid".to_string())?,
            },
        };
        finish(&mut lines)?;
        claim.validate()?;
        if claim.encode()? != bytes {
            return Err("native Apply worker claim is not canonical".to_string());
        }
        Ok(claim)
    }
}

impl NativeApplyVerifiedDispatchIdentity {
    fn validate(&self) -> Result<(), String> {
        require_lower_hex("native Apply worker run ID", &self.worker_run_id, 32)?;
        require_lower_hex(
            "native Apply source manifest digest",
            &self.source_manifest_sha256,
            64,
        )?;
        if self.manifest_schema_version == 0 {
            return Err("native Apply manifest schema version is invalid".to_string());
        }
        if !matches!(
            self.target_state.as_str(),
            "existing_managed" | "absent_bootstrap"
        ) {
            return Err("native Apply target state is invalid".to_string());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeApplyDispatchRecord {
    pub(crate) state: NativeApplyDispatchState,
    pub(crate) generation: u64,
    pub(crate) apply_job_id: String,
    pub(crate) apply_job_token: String,
    pub(crate) source_job_id: String,
    pub(crate) worker_run_id: String,
    pub(crate) option_id: String,
    pub(crate) review_sha256: String,
    pub(crate) source_manifest_sha256: String,
    pub(crate) manifest_sha256: String,
    pub(crate) manifest_schema_version: u8,
    pub(crate) target_state: String,
    pub(crate) acknowledgements: Vec<NativeApplyAcknowledgement>,
}

impl NativeApplyDispatchRecord {
    pub(crate) fn validate(&self) -> Result<(), String> {
        require_lower_hex("native Apply job ID", &self.apply_job_id, 32)?;
        require_lower_hex("native Apply job token", &self.apply_job_token, 64)?;
        require_lower_hex("native Apply source job ID", &self.source_job_id, 32)?;
        if self.generation == 0 {
            return Err("native Apply dispatch generation is invalid".to_string());
        }
        validate_native_apply_option_id(&self.option_id)?;
        require_lower_hex("native Apply Review digest", &self.review_sha256, 64)?;
        require_lower_hex("native Apply manifest digest", &self.manifest_sha256, 64)?;
        match self.state {
            NativeApplyDispatchState::Accepted | NativeApplyDispatchState::Validating => {
                if self.worker_run_id != "none"
                    || self.source_manifest_sha256 != "none"
                    || self.manifest_schema_version != 0
                    || self.target_state != "none"
                {
                    return Err(
                        "unverified native Apply dispatch carries verified authority".to_string(),
                    );
                }
            }
            NativeApplyDispatchState::Applying => {
                self.verified_identity()?.validate()?;
            }
        }
        require_sorted_acknowledgements(&self.acknowledgements)
    }

    pub(crate) fn verified_identity(&self) -> Result<NativeApplyVerifiedDispatchIdentity, String> {
        if self.state != NativeApplyDispatchState::Applying {
            return Err("native Apply dispatch authority is not verified".to_string());
        }
        let identity = NativeApplyVerifiedDispatchIdentity {
            worker_run_id: self.worker_run_id.clone(),
            source_manifest_sha256: self.source_manifest_sha256.clone(),
            manifest_schema_version: self.manifest_schema_version,
            target_state: self.target_state.clone(),
        };
        identity.validate()?;
        Ok(identity)
    }

    pub(crate) fn matches_start(&self, request: &NativeApplyControlRequest) -> bool {
        request.command == NativeApplyControlCommand::Start
            && request.source_job_id.as_deref() == Some(self.source_job_id.as_str())
            && request.option_id.as_deref() == Some(self.option_id.as_str())
            && request.review_sha256.as_deref() == Some(self.review_sha256.as_str())
            && request.manifest_sha256.as_deref() == Some(self.manifest_sha256.as_str())
            && request.acknowledgements == self.acknowledgements
    }

    pub(crate) fn authorized(&self, request: &NativeApplyControlRequest) -> bool {
        request.apply_job_id.as_deref() == Some(self.apply_job_id.as_str())
            && request.apply_job_token.as_deref() == Some(self.apply_job_token.as_str())
    }

    fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let mut output = format!(
            concat!(
                "{}\n",
                "state={}\n",
                "generation={}\n",
                "apply_job_id={}\n",
                "apply_job_token={}\n",
                "source_job_id={}\n",
                "worker_run_id={}\n",
                "option_id={}\n",
                "review_sha256={}\n",
                "source_manifest_sha256={}\n",
                "manifest_sha256={}\n",
                "manifest_schema_version={}\n",
                "target_state={}\n",
                "acknowledgement_count={}\n"
            ),
            DISPATCH_HEADER,
            self.state.as_str(),
            self.generation,
            self.apply_job_id,
            self.apply_job_token,
            self.source_job_id,
            self.worker_run_id,
            self.option_id,
            self.review_sha256,
            self.source_manifest_sha256,
            self.manifest_sha256,
            self.manifest_schema_version,
            self.target_state,
            self.acknowledgements.len(),
        );
        append_acknowledgements(&mut output, &self.acknowledgements);
        output.push('\n');
        if output.len() > MAX_RECORD_BYTES {
            return Err("native Apply dispatch record exceeds its bound".to_string());
        }
        Ok(output.into_bytes())
    }

    fn decode(bytes: &[u8]) -> Result<Self, String> {
        let input = std::str::from_utf8(bytes)
            .map_err(|_| "native Apply dispatch record is not UTF-8".to_string())?;
        if input.len() > MAX_RECORD_BYTES || !input.ends_with("\n\n") {
            return Err("native Apply dispatch record is not bounded and terminated".to_string());
        }
        let mut lines = input.split('\n');
        if lines.next() != Some(DISPATCH_HEADER) {
            return Err("native Apply dispatch record has an unsupported header".to_string());
        }
        let state_value = field(&mut lines, "state")?;
        let state = NativeApplyDispatchState::parse(&state_value)
            .ok_or_else(|| "native Apply dispatch state is unsupported".to_string())?;
        let generation = field(&mut lines, "generation")?
            .parse::<u64>()
            .map_err(|_| "native Apply dispatch generation is invalid".to_string())?;
        let apply_job_id = field(&mut lines, "apply_job_id")?;
        let apply_job_token = field(&mut lines, "apply_job_token")?;
        let source_job_id = field(&mut lines, "source_job_id")?;
        let worker_run_id = field(&mut lines, "worker_run_id")?;
        let option_id = field(&mut lines, "option_id")?;
        let review_sha256 = field(&mut lines, "review_sha256")?;
        let source_manifest_sha256 = field(&mut lines, "source_manifest_sha256")?;
        let manifest_sha256 = field(&mut lines, "manifest_sha256")?;
        let manifest_schema_version = field(&mut lines, "manifest_schema_version")?
            .parse::<u8>()
            .map_err(|_| "native Apply manifest schema version is invalid".to_string())?;
        let target_state = field(&mut lines, "target_state")?;
        let count = parse_count(&field(&mut lines, "acknowledgement_count")?)?;
        let acknowledgements = read_acknowledgements(&mut lines, count)?;
        finish(&mut lines)?;
        let record = Self {
            state,
            generation,
            apply_job_id,
            apply_job_token,
            source_job_id,
            worker_run_id,
            option_id,
            review_sha256,
            source_manifest_sha256,
            manifest_sha256,
            manifest_schema_version,
            target_state,
            acknowledgements,
        };
        record.validate()?;
        if record.encode()? != bytes {
            return Err("native Apply dispatch record is not canonical".to_string());
        }
        Ok(record)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeApplyTerminalOutcome {
    Applied,
    AlreadyApplied,
    RolledBack,
    Failed,
}

impl NativeApplyTerminalOutcome {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::AlreadyApplied => "already_applied",
            Self::RolledBack => "rolled_back",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "applied" => Some(Self::Applied),
            "already_applied" => Some(Self::AlreadyApplied),
            "rolled_back" => Some(Self::RolledBack),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }

    pub(crate) fn success(self) -> bool {
        matches!(self, Self::Applied | Self::AlreadyApplied)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeApplyTerminalRecord {
    pub(crate) dispatch: NativeApplyDispatchRecord,
    pub(crate) outcome: NativeApplyTerminalOutcome,
    pub(crate) recovery_cleared: bool,
    pub(crate) diagnostic: String,
}

impl NativeApplyTerminalRecord {
    pub(crate) fn validate(&self) -> Result<(), String> {
        self.dispatch.validate()?;
        if self.dispatch.state == NativeApplyDispatchState::Accepted {
            return Err(
                "native Apply terminal record was not advanced by the coordinator".to_string(),
            );
        }
        if self.diagnostic.len() > MAX_DIAGNOSTIC_BYTES
            || !self
                .diagnostic
                .bytes()
                .all(|value| value == b' ' || value.is_ascii_graphic())
        {
            return Err("native Apply terminal diagnostic is invalid".to_string());
        }
        if self.outcome.success() && (!self.recovery_cleared || !self.diagnostic.is_empty()) {
            return Err("successful native Apply terminal evidence is inconsistent".to_string());
        }
        if self.outcome.success() && self.dispatch.state != NativeApplyDispatchState::Applying {
            return Err("successful native Apply terminal was not applied".to_string());
        }
        Ok(())
    }

    fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let dispatch = self.dispatch.encode()?;
        let mut output = format!(
            "{}\noutcome={}\nrecovery_cleared={}\ndiagnostic={}\n",
            TERMINAL_HEADER,
            self.outcome.as_str(),
            if self.recovery_cleared { "1" } else { "0" },
            self.diagnostic,
        )
        .into_bytes();
        output.extend_from_slice(&dispatch);
        if output.len() > MAX_RECORD_BYTES {
            return Err("native Apply terminal record exceeds its bound".to_string());
        }
        Ok(output)
    }

    fn decode(bytes: &[u8]) -> Result<Self, String> {
        let input = std::str::from_utf8(bytes)
            .map_err(|_| "native Apply terminal record is not UTF-8".to_string())?;
        if input.len() > MAX_RECORD_BYTES {
            return Err("native Apply terminal record exceeds its bound".to_string());
        }
        let dispatch_offset = input
            .find(DISPATCH_HEADER)
            .ok_or_else(|| "native Apply terminal record has no dispatch identity".to_string())?;
        let (terminal, dispatch) = input.split_at(dispatch_offset);
        let mut lines = terminal.split('\n');
        if lines.next() != Some(TERMINAL_HEADER) {
            return Err("native Apply terminal record has an unsupported header".to_string());
        }
        let outcome = NativeApplyTerminalOutcome::parse(&field(&mut lines, "outcome")?)
            .ok_or_else(|| "native Apply terminal outcome is unsupported".to_string())?;
        let recovery_cleared = match field(&mut lines, "recovery_cleared")?.as_str() {
            "1" => true,
            "0" => false,
            _ => return Err("native Apply terminal recovery flag is invalid".to_string()),
        };
        let diagnostic = field(&mut lines, "diagnostic")?;
        if lines.next() != Some("") || lines.next().is_some() {
            return Err("native Apply terminal prefix has trailing fields".to_string());
        }
        let record = Self {
            dispatch: NativeApplyDispatchRecord::decode(dispatch.as_bytes())?,
            outcome,
            recovery_cleared,
            diagnostic,
        };
        record.validate()?;
        if record.encode()? != bytes {
            return Err("native Apply terminal record is not canonical".to_string());
        }
        Ok(record)
    }
}

#[derive(Debug)]
pub(crate) enum NativeApplyAdmission {
    Created(NativeApplyDispatchRecord),
    Existing(NativeApplyDispatchRecord),
    Terminal(NativeApplyTerminalRecord),
}

pub(crate) struct NativeApplyCoordinatorStore {
    root: PathBuf,
}

impl NativeApplyCoordinatorStore {
    pub(crate) fn new(recovery_root: &Path) -> Self {
        Self {
            root: recovery_root.join(STORE_DIRECTORY),
        }
    }

    pub(crate) fn admit(
        &self,
        start: &NativeApplyControlRequest,
        candidate: NativeApplyDispatchRecord,
    ) -> Result<NativeApplyAdmission, String> {
        start.validate()?;
        if start.command != NativeApplyControlCommand::Start {
            return Err("native Apply admission requires a start request".to_string());
        }
        candidate.validate()?;
        if !candidate.matches_start(start) {
            return Err(
                "native Apply admitted identity differs from its start request".to_string(),
            );
        }
        ensure_private_directory(&self.root)?;
        let active = self.read_active()?;
        let terminal = self.read_terminal()?;
        if let (Some(active), Some(terminal)) = (&active, &terminal) {
            if terminal.dispatch == *active {
                if !terminal.dispatch.matches_start(start) {
                    return Err("another native Apply request is already active".to_string());
                }
                self.remove_active()?;
                return Ok(NativeApplyAdmission::Terminal(terminal.clone()));
            }
            // rearm_terminal() publishes the newer Active record before it
            // removes the old receipt.  A crash in that narrow window leaves
            // both files durable.  The strictly newer, same-intent Active
            // authority wins; an unrelated or non-monotonic pair remains a
            // hard conflict.
            if active.generation > terminal.dispatch.generation
                && active.matches_start(start)
                && terminal.dispatch.matches_start(start)
            {
                self.remove_terminal()?;
                return Ok(NativeApplyAdmission::Existing(active.clone()));
            }
            if !active.matches_start(start) || !terminal.dispatch.matches_start(start) {
                return Err("another native Apply request is already active".to_string());
            }
            return Err("native Apply active and terminal authorities conflict".to_string());
        }
        if let Some(active) = active {
            if active.matches_start(start) {
                return Ok(NativeApplyAdmission::Existing(active));
            }
            return Err("another native Apply request is already active".to_string());
        }
        if let Some(terminal) = terminal {
            if terminal.dispatch.matches_start(start) {
                return Ok(NativeApplyAdmission::Terminal(terminal));
            }
            self.remove_terminal()?;
        }
        write_new_private_file(&self.active_path(), &candidate.encode()?)?;
        sync_directory(&self.root)?;
        Ok(NativeApplyAdmission::Created(candidate))
    }

    pub(crate) fn rearm_terminal(
        &self,
        start: &NativeApplyControlRequest,
        expected: &NativeApplyTerminalRecord,
        mut candidate: NativeApplyDispatchRecord,
    ) -> Result<NativeApplyDispatchRecord, String> {
        start.validate()?;
        if start.command != NativeApplyControlCommand::Start {
            return Err("native Apply terminal rearm requires a start request".to_string());
        }
        expected.validate()?;
        candidate.validate()?;
        if !expected.dispatch.matches_start(start) || !candidate.matches_start(start) {
            return Err(
                "native Apply terminal rearm identity differs from its start request".to_string(),
            );
        }
        if candidate.state != NativeApplyDispatchState::Accepted {
            return Err("native Apply terminal rearm requires an accepted candidate".to_string());
        }
        ensure_private_directory(&self.root)?;
        if self.read_active()?.is_some() {
            return Err(
                "native Apply terminal cannot be rearmed while a dispatch is active".to_string(),
            );
        }
        if self.read_worker_claim()?.is_some() {
            return Err(
                "native Apply terminal cannot be rearmed while a worker claim exists".to_string(),
            );
        }
        let current = self
            .read_terminal()?
            .ok_or_else(|| "native Apply terminal disappeared before rearm".to_string())?;
        if current != *expected {
            return Err("native Apply terminal changed before rearm".to_string());
        }
        candidate.generation = expected.dispatch.generation.checked_add(1).ok_or_else(|| {
            "native Apply dispatch generation overflowed during rearm".to_string()
        })?;
        candidate.validate()?;

        // Publish the replacement authority first.  If power is lost before
        // the stale receipt is removed, admit() deterministically recovers
        // the strictly newer same-intent Active record.
        write_new_private_file(&self.active_path(), &candidate.encode()?)?;
        sync_directory(&self.root)?;
        self.remove_terminal()?;
        Ok(candidate)
    }

    pub(crate) fn mark_validating(
        &self,
        expected: &NativeApplyDispatchRecord,
    ) -> Result<NativeApplyDispatchRecord, String> {
        let mut current = self
            .read_active()?
            .ok_or_else(|| "native Apply dispatch disappeared before execution".to_string())?;
        if &current != expected {
            return Err("native Apply dispatch identity changed before execution".to_string());
        }
        if current.state != NativeApplyDispatchState::Accepted {
            return Err("native Apply validation transition requires accepted state".to_string());
        }
        current.state = NativeApplyDispatchState::Validating;
        current.generation = current
            .generation
            .checked_add(1)
            .ok_or_else(|| "native Apply dispatch generation overflowed".to_string())?;
        replace_private_file(
            &self.active_path(),
            &self.root.join(ACTIVE_NEXT_FILE),
            &current.encode()?,
        )?;
        sync_directory(&self.root)?;
        Ok(current)
    }

    pub(crate) fn mark_applying(
        &self,
        expected: &NativeApplyDispatchRecord,
        verified: NativeApplyVerifiedDispatchIdentity,
    ) -> Result<NativeApplyDispatchRecord, String> {
        verified.validate()?;
        let mut current = self
            .read_active()?
            .ok_or_else(|| "native Apply dispatch disappeared before mutation".to_string())?;
        if &current != expected {
            return Err("native Apply dispatch identity changed before mutation".to_string());
        }
        if current.state != NativeApplyDispatchState::Validating {
            return Err("native Apply mutation transition requires validating state".to_string());
        }
        current.state = NativeApplyDispatchState::Applying;
        current.generation = current
            .generation
            .checked_add(1)
            .ok_or_else(|| "native Apply dispatch generation overflowed".to_string())?;
        current.worker_run_id = verified.worker_run_id;
        current.source_manifest_sha256 = verified.source_manifest_sha256;
        current.manifest_schema_version = verified.manifest_schema_version;
        current.target_state = verified.target_state;
        replace_private_file(
            &self.active_path(),
            &self.root.join(ACTIVE_NEXT_FILE),
            &current.encode()?,
        )?;
        sync_directory(&self.root)?;
        Ok(current)
    }

    pub(crate) fn complete(&self, terminal: &NativeApplyTerminalRecord) -> Result<(), String> {
        terminal.validate()?;
        ensure_private_directory(&self.root)?;
        let active = self.read_active()?.ok_or_else(|| {
            "native Apply dispatch disappeared before terminal publication".to_string()
        })?;
        if active != terminal.dispatch {
            return Err("native Apply terminal identity differs from active dispatch".to_string());
        }
        replace_private_file(
            &self.terminal_path(),
            &self.root.join(TERMINAL_NEXT_FILE),
            &terminal.encode()?,
        )?;
        sync_directory(&self.root)?;
        self.remove_active()?;
        Ok(())
    }

    pub(crate) fn read_active(&self) -> Result<Option<NativeApplyDispatchRecord>, String> {
        read_optional_private(
            &self.root,
            &self.active_path(),
            "coordinator active dispatch",
        )?
        .map(|bytes| NativeApplyDispatchRecord::decode(&bytes))
        .transpose()
    }

    pub(crate) fn read_terminal(&self) -> Result<Option<NativeApplyTerminalRecord>, String> {
        read_optional_private(
            &self.root,
            &self.terminal_path(),
            "coordinator terminal receipt",
        )?
        .map(|bytes| NativeApplyTerminalRecord::decode(&bytes))
        .transpose()
    }

    pub(crate) fn authorized_active(
        &self,
        request: &NativeApplyControlRequest,
    ) -> Result<Option<NativeApplyDispatchRecord>, String> {
        Ok(self
            .read_active()?
            .filter(|record| record.authorized(request)))
    }

    pub(crate) fn authorized_terminal(
        &self,
        request: &NativeApplyControlRequest,
    ) -> Result<Option<NativeApplyTerminalRecord>, String> {
        Ok(self
            .read_terminal()?
            .filter(|record| record.dispatch.authorized(request)))
    }

    pub(crate) fn read_worker_claim(&self) -> Result<Option<NativeApplyWorkerClaim>, String> {
        read_optional_private(
            &self.root,
            &self.worker_claim_path(),
            "coordinator worker claim",
        )?
        .map(|bytes| NativeApplyWorkerClaim::decode(&bytes))
        .transpose()
    }

    pub(crate) fn claim_worker(
        &self,
        expected: &NativeApplyDispatchRecord,
        claim: &NativeApplyWorkerClaim,
    ) -> Result<Option<NativeApplyWorkerClaim>, String> {
        expected.validate()?;
        claim.validate()?;
        if !claim.matches_dispatch(expected) || claim.dispatch_generation != expected.generation {
            return Err("native Apply worker claim differs from its dispatch".to_string());
        }
        let active = self
            .read_active()?
            .ok_or_else(|| "native Apply dispatch disappeared before worker claim".to_string())?;
        if active != *expected {
            return Err("native Apply dispatch changed before worker claim".to_string());
        }
        if let Some(existing) = self.read_worker_claim()? {
            return Ok(Some(existing));
        }
        write_new_private_file(&self.worker_claim_path(), &claim.encode()?)?;
        sync_directory(&self.root)?;
        Ok(None)
    }

    pub(crate) fn remove_worker_claim(
        &self,
        expected: &NativeApplyWorkerClaim,
    ) -> Result<(), String> {
        let Some(current) = self.read_worker_claim()? else {
            return Ok(());
        };
        if current != *expected {
            return Err("native Apply worker claim changed before cleanup".to_string());
        }
        remove_private_file(&self.root, &self.worker_claim_path(), "worker claim")
    }

    pub(crate) fn worker_log_path(&self, stream: &str) -> Result<PathBuf, String> {
        if !matches!(stream, "stdout" | "stderr") {
            return Err("native Apply worker log stream is invalid".to_string());
        }
        Ok(self.root.join(format!("worker.{stream}")))
    }

    fn remove_active(&self) -> Result<(), String> {
        remove_private_file(&self.root, &self.active_path(), "active dispatch")
    }

    fn remove_terminal(&self) -> Result<(), String> {
        remove_private_file(&self.root, &self.terminal_path(), "terminal receipt")
    }

    fn active_path(&self) -> PathBuf {
        self.root.join(ACTIVE_FILE)
    }

    fn terminal_path(&self) -> PathBuf {
        self.root.join(TERMINAL_FILE)
    }

    fn worker_claim_path(&self) -> PathBuf {
        self.root.join(WORKER_CLAIM_FILE)
    }
}

fn read_optional_private(root: &Path, path: &Path, label: &str) -> Result<Option<Vec<u8>>, String> {
    if !path_exists(root)? {
        return Ok(None);
    }
    ensure_private_directory(root)?;
    if !path_exists(path)? {
        return Ok(None);
    }
    read_private_recovery_bounded(path, MAX_RECORD_BYTES, label).map(Some)
}

fn remove_private_file(root: &Path, path: &Path, label: &str) -> Result<(), String> {
    if !path_exists(path)? {
        return Ok(());
    }
    let _ = read_private_recovery_bounded(path, MAX_RECORD_BYTES, label)?;
    fs::remove_file(path)
        .map_err(|error| format!("unable to remove native Apply {label}: {error}"))?;
    sync_directory(root)
}

fn append_acknowledgements(output: &mut String, values: &[NativeApplyAcknowledgement]) {
    for (index, value) in values.iter().enumerate() {
        output.push_str(&format!("acknowledgement_{index}={}\n", value.as_str()));
    }
}

fn read_acknowledgements<'a>(
    lines: &mut impl Iterator<Item = &'a str>,
    count: usize,
) -> Result<Vec<NativeApplyAcknowledgement>, String> {
    if count > MAX_NATIVE_APPLY_ACKNOWLEDGEMENTS {
        return Err("native Apply acknowledgement set exceeds its bound".to_string());
    }
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        let value = field(lines, &format!("acknowledgement_{index}"))?;
        values.push(
            NativeApplyAcknowledgement::from_public_code(&value)
                .ok_or_else(|| "native Apply acknowledgement code is unknown".to_string())?,
        );
    }
    Ok(values)
}

fn require_sorted_acknowledgements(values: &[NativeApplyAcknowledgement]) -> Result<(), String> {
    if values.len() > MAX_NATIVE_APPLY_ACKNOWLEDGEMENTS {
        return Err("native Apply acknowledgement set exceeds its bound".to_string());
    }
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err("native Apply acknowledgement set is not strictly sorted".to_string());
    }
    Ok(())
}

fn required<'a>(value: &'a Option<String>, label: &str) -> Result<&'a str, String> {
    value
        .as_deref()
        .ok_or_else(|| format!("native Apply control request requires {label}"))
}

fn optional(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or("none")
}

fn optional_string(value: String) -> Option<String> {
    (value != "none").then_some(value)
}

fn parse_count(value: &str) -> Result<usize, String> {
    if value.is_empty() || (value.len() > 1 && value.starts_with('0')) {
        return Err("native Apply acknowledgement count is not canonical".to_string());
    }
    value
        .parse::<usize>()
        .map_err(|_| "native Apply acknowledgement count is invalid".to_string())
}

fn field<'a>(lines: &mut impl Iterator<Item = &'a str>, name: &str) -> Result<String, String> {
    let line = lines
        .next()
        .ok_or_else(|| format!("native Apply record is missing {name}"))?;
    let prefix = format!("{name}=");
    line.strip_prefix(&prefix)
        .map(str::to_string)
        .ok_or_else(|| format!("native Apply record field {name} is out of order"))
}

fn finish<'a>(lines: &mut impl Iterator<Item = &'a str>) -> Result<(), String> {
    if lines.next() != Some("") || lines.next() != Some("") || lines.next().is_some() {
        return Err("native Apply record has trailing fields".to_string());
    }
    Ok(())
}

fn require_lower_hex(label: &str, value: &str, length: usize) -> Result<(), String> {
    if value.len() != length
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!("{label} is invalid"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn start() -> NativeApplyControlRequest {
        NativeApplyControlRequest::start(
            "11".repeat(16),
            "22".repeat(16),
            "bypass_download".to_string(),
            "33".repeat(32),
            "44".repeat(32),
            vec![NativeApplyAcknowledgement::DownloadShapingBypassed],
        )
    }

    fn accepted_dispatch() -> NativeApplyDispatchRecord {
        NativeApplyDispatchRecord {
            state: NativeApplyDispatchState::Accepted,
            generation: 1,
            apply_job_id: "55".repeat(16),
            apply_job_token: "66".repeat(32),
            source_job_id: "22".repeat(16),
            worker_run_id: "none".to_string(),
            option_id: "bypass_download".to_string(),
            review_sha256: "33".repeat(32),
            source_manifest_sha256: "none".to_string(),
            manifest_sha256: "44".repeat(32),
            manifest_schema_version: 0,
            target_state: "none".to_string(),
            acknowledgements: vec![NativeApplyAcknowledgement::DownloadShapingBypassed],
        }
    }

    fn verified_identity() -> NativeApplyVerifiedDispatchIdentity {
        NativeApplyVerifiedDispatchIdentity {
            worker_run_id: "77".repeat(16),
            source_manifest_sha256: "88".repeat(32),
            manifest_schema_version: 6,
            target_state: "existing_managed".to_string(),
        }
    }

    fn applying_dispatch() -> NativeApplyDispatchRecord {
        let mut dispatch = accepted_dispatch();
        dispatch.state = NativeApplyDispatchState::Applying;
        dispatch.generation = 3;
        let verified = verified_identity();
        dispatch.worker_run_id = verified.worker_run_id;
        dispatch.source_manifest_sha256 = verified.source_manifest_sha256;
        dispatch.manifest_schema_version = verified.manifest_schema_version;
        dispatch.target_state = verified.target_state;
        dispatch
    }

    fn root(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "cake-native-apply-coordinator-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[test]
    fn control_codec_is_canonical_and_command_shapes_are_disjoint() {
        let start_request = start();
        assert!(start_request
            .encode()
            .unwrap()
            .starts_with(CONTROL_HEADER_BASE));
        assert_eq!(
            NativeApplyControlRequest::decode(&start_request.encode().unwrap()).unwrap(),
            start_request
        );
        let query = NativeApplyControlRequest::query(
            "aa".repeat(16),
            NativeApplyControlCommand::Status,
            "bb".repeat(16),
            "cc".repeat(32),
        );
        assert_eq!(
            NativeApplyControlRequest::decode(&query.encode().unwrap()).unwrap(),
            query
        );
        let watch =
            NativeApplyControlRequest::watch("dd".repeat(16), "ee".repeat(16), "ff".repeat(32), 7);
        assert!(watch.encode().unwrap().starts_with(CONTROL_HEADER_WATCH));
        assert_eq!(
            NativeApplyControlRequest::decode(&watch.encode().unwrap()).unwrap(),
            watch
        );
        let mut invalid = start();
        invalid.apply_job_id = Some("dd".repeat(16));
        assert!(invalid.validate().unwrap_err().contains("must not carry"));
        let mut invalid = query;
        invalid.option_id = Some("recommended".to_string());
        assert!(invalid
            .validate()
            .unwrap_err()
            .contains("only its Apply handle"));
        let mut invalid = NativeApplyControlRequest::query(
            "aa".repeat(16),
            NativeApplyControlCommand::Watch,
            "bb".repeat(16),
            "cc".repeat(32),
        );
        assert!(invalid
            .validate()
            .unwrap_err()
            .contains("observed generation"));
        invalid.observed_generation = Some(1);
        assert!(invalid.validate().is_ok());
    }

    #[test]
    fn dispatch_and_terminal_codecs_reject_identity_or_evidence_drift() {
        let dispatch = accepted_dispatch();
        assert_eq!(
            NativeApplyDispatchRecord::decode(&dispatch.encode().unwrap()).unwrap(),
            dispatch
        );
        let applying = applying_dispatch();
        assert_eq!(
            NativeApplyDispatchRecord::decode(&applying.encode().unwrap()).unwrap(),
            applying
        );
        let terminal = NativeApplyTerminalRecord {
            dispatch: applying,
            outcome: NativeApplyTerminalOutcome::Applied,
            recovery_cleared: true,
            diagnostic: String::new(),
        };
        assert_eq!(
            NativeApplyTerminalRecord::decode(&terminal.encode().unwrap()).unwrap(),
            terminal
        );
        let mut invalid = terminal;
        invalid.diagnostic = "should not exist".to_string();
        assert!(invalid.validate().unwrap_err().contains("inconsistent"));
    }

    #[test]
    fn accept_is_durable_before_validation_and_identical_retry_reuses_one_handle() {
        let root = root("idempotent");
        let store = NativeApplyCoordinatorStore::new(&root);
        let request = start();
        let candidate = accepted_dispatch();
        match store.admit(&request, candidate.clone()).unwrap() {
            NativeApplyAdmission::Created(record) => assert_eq!(record, candidate),
            other => panic!("unexpected first admission: {other:?}"),
        }
        assert_eq!(store.read_active().unwrap(), Some(candidate.clone()));
        match store.admit(&request, accepted_dispatch()).unwrap() {
            NativeApplyAdmission::Existing(record) => assert_eq!(record, candidate),
            other => panic!("unexpected retry admission: {other:?}"),
        }
        let mut divergent = start();
        divergent.option_id = Some("recommended".to_string());
        assert!(store
            .admit(&divergent, {
                let mut value = accepted_dispatch();
                value.option_id = "recommended".to_string();
                value
            })
            .unwrap_err()
            .contains("already active"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn generations_advance_only_after_durable_validation_boundaries() {
        let root = root("generations");
        let store = NativeApplyCoordinatorStore::new(&root);
        let request = start();
        let accepted = match store.admit(&request, accepted_dispatch()).unwrap() {
            NativeApplyAdmission::Created(record) => record,
            other => panic!("unexpected admission: {other:?}"),
        };
        assert_eq!(accepted.state, NativeApplyDispatchState::Accepted);
        assert_eq!(accepted.generation, 1);
        assert_eq!(accepted.worker_run_id, "none");

        let validating = store.mark_validating(&accepted).unwrap();
        assert_eq!(validating.state, NativeApplyDispatchState::Validating);
        assert_eq!(validating.generation, 2);
        assert_eq!(validating.worker_run_id, "none");

        let applying = store
            .mark_applying(&validating, verified_identity())
            .unwrap();
        assert_eq!(applying.state, NativeApplyDispatchState::Applying);
        assert_eq!(applying.generation, 3);
        assert_eq!(applying.worker_run_id, "77".repeat(16));
        assert_eq!(store.read_active().unwrap(), Some(applying));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn validation_failure_can_terminate_without_mutation_authority() {
        let root = root("validation-failure");
        let store = NativeApplyCoordinatorStore::new(&root);
        let request = start();
        let accepted = match store.admit(&request, accepted_dispatch()).unwrap() {
            NativeApplyAdmission::Created(record) => record,
            other => panic!("unexpected admission: {other:?}"),
        };
        let validating = store.mark_validating(&accepted).unwrap();
        let terminal = NativeApplyTerminalRecord {
            dispatch: validating,
            outcome: NativeApplyTerminalOutcome::Failed,
            recovery_cleared: true,
            diagnostic: "private Review replay failed before mutation".to_string(),
        };
        store.complete(&terminal).unwrap();
        assert_eq!(store.read_active().unwrap(), None);
        assert_eq!(store.read_terminal().unwrap(), Some(terminal));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn worker_claim_is_exact_durable_and_generation_bound() {
        let root = root("worker-claim");
        let store = NativeApplyCoordinatorStore::new(&root);
        let request = start();
        let accepted = match store.admit(&request, accepted_dispatch()).unwrap() {
            NativeApplyAdmission::Created(record) => record,
            other => panic!("unexpected admission: {other:?}"),
        };
        let validating = store.mark_validating(&accepted).unwrap();
        let claim = NativeApplyWorkerClaim::for_process(
            &validating,
            ProcessIdentity {
                pid: 42,
                process_group: 42,
                starttime_ticks: 9001,
            },
        )
        .unwrap();
        assert_eq!(
            NativeApplyWorkerClaim::decode(&claim.encode().unwrap()).unwrap(),
            claim
        );
        assert_eq!(store.claim_worker(&validating, &claim).unwrap(), None);
        assert_eq!(store.read_worker_claim().unwrap(), Some(claim.clone()));
        assert_eq!(
            store.claim_worker(&validating, &claim).unwrap(),
            Some(claim.clone())
        );

        let mut mismatched = claim.clone();
        mismatched.dispatch_generation = 3;
        assert!(store
            .claim_worker(&validating, &mismatched)
            .unwrap_err()
            .contains("differs from its dispatch"));
        assert!(store
            .remove_worker_claim(&mismatched)
            .unwrap_err()
            .contains("changed"));
        store.remove_worker_claim(&claim).unwrap();
        assert_eq!(store.read_worker_claim().unwrap(), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retired_v1_dispatch_and_terminal_headers_are_rejected() {
        let applying = applying_dispatch();
        let encoded = applying.encode().unwrap();
        let retired = String::from_utf8(encoded).unwrap().replacen(
            DISPATCH_HEADER,
            "cake-autorate-native-apply\t1\tdispatch",
            1,
        );
        assert!(NativeApplyDispatchRecord::decode(retired.as_bytes()).is_err());
        let terminal = NativeApplyTerminalRecord {
            dispatch: applying,
            outcome: NativeApplyTerminalOutcome::Applied,
            recovery_cleared: true,
            diagnostic: String::new(),
        };
        let encoded = String::from_utf8(terminal.encode().unwrap())
            .unwrap()
            .replacen(
                TERMINAL_HEADER,
                "cake-autorate-native-apply\t1\tterminal",
                1,
            );
        assert!(NativeApplyTerminalRecord::decode(encoded.as_bytes()).is_err());
    }

    #[test]
    fn terminal_is_published_before_active_is_removed_and_remains_queryable() {
        let root = root("terminal");
        let store = NativeApplyCoordinatorStore::new(&root);
        let request = start();
        let accepted = match store.admit(&request, accepted_dispatch()).unwrap() {
            NativeApplyAdmission::Created(record) => record,
            other => panic!("unexpected admission: {other:?}"),
        };
        let validating = store.mark_validating(&accepted).unwrap();
        let applying = store
            .mark_applying(&validating, verified_identity())
            .unwrap();
        let terminal = NativeApplyTerminalRecord {
            dispatch: applying,
            outcome: NativeApplyTerminalOutcome::Applied,
            recovery_cleared: true,
            diagnostic: String::new(),
        };
        store.complete(&terminal).unwrap();
        assert_eq!(store.read_active().unwrap(), None);
        assert_eq!(store.read_terminal().unwrap(), Some(terminal.clone()));
        match store.admit(&request, accepted_dispatch()).unwrap() {
            NativeApplyAdmission::Terminal(record) => assert_eq!(record, terminal),
            other => panic!("unexpected completed retry: {other:?}"),
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn terminal_rearm_publishes_a_fresh_monotonic_dispatch_before_retiring_receipt() {
        let root = root("terminal-rearm");
        let store = NativeApplyCoordinatorStore::new(&root);
        let request = start();
        let accepted = match store.admit(&request, accepted_dispatch()).unwrap() {
            NativeApplyAdmission::Created(record) => record,
            other => panic!("unexpected admission: {other:?}"),
        };
        let validating = store.mark_validating(&accepted).unwrap();
        let applying = store
            .mark_applying(&validating, verified_identity())
            .unwrap();
        let terminal = NativeApplyTerminalRecord {
            dispatch: applying.clone(),
            outcome: NativeApplyTerminalOutcome::Applied,
            recovery_cleared: true,
            diagnostic: String::new(),
        };
        store.complete(&terminal).unwrap();

        let mut retry = accepted_dispatch();
        retry.apply_job_id = "99".repeat(16);
        retry.apply_job_token = "aa".repeat(32);
        let rearmed = store.rearm_terminal(&request, &terminal, retry).unwrap();
        assert_eq!(rearmed.state, NativeApplyDispatchState::Accepted);
        assert_eq!(rearmed.generation, applying.generation + 1);
        assert_ne!(rearmed.apply_job_id, applying.apply_job_id);
        assert_eq!(store.read_active().unwrap(), Some(rearmed));
        assert_eq!(store.read_terminal().unwrap(), None);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn terminal_rearm_crash_window_recovers_only_a_newer_same_intent_active_record() {
        let root = root("terminal-rearm-crash-window");
        let store = NativeApplyCoordinatorStore::new(&root);
        let request = start();
        let accepted = match store.admit(&request, accepted_dispatch()).unwrap() {
            NativeApplyAdmission::Created(record) => record,
            other => panic!("unexpected admission: {other:?}"),
        };
        let validating = store.mark_validating(&accepted).unwrap();
        let applying = store
            .mark_applying(&validating, verified_identity())
            .unwrap();
        let terminal = NativeApplyTerminalRecord {
            dispatch: applying.clone(),
            outcome: NativeApplyTerminalOutcome::Applied,
            recovery_cleared: true,
            diagnostic: String::new(),
        };
        store.complete(&terminal).unwrap();

        // Model power loss after rearm published the replacement Active file
        // but before it removed the stale terminal receipt.
        let mut newer = accepted_dispatch();
        newer.generation = applying.generation + 1;
        newer.apply_job_id = "99".repeat(16);
        newer.apply_job_token = "aa".repeat(32);
        write_new_private_file(&store.active_path(), &newer.encode().unwrap()).unwrap();
        sync_directory(&store.root).unwrap();
        match store.admit(&request, accepted_dispatch()).unwrap() {
            NativeApplyAdmission::Existing(record) => assert_eq!(record, newer),
            other => panic!("unexpected rearm crash recovery: {other:?}"),
        }
        assert_eq!(store.read_active().unwrap(), Some(newer));
        assert_eq!(store.read_terminal().unwrap(), None);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn terminal_publish_crash_window_never_discloses_or_removes_a_foreign_handle() {
        let root = root("terminal-crash-window");
        let store = NativeApplyCoordinatorStore::new(&root);
        let request = start();
        let accepted = match store.admit(&request, accepted_dispatch()).unwrap() {
            NativeApplyAdmission::Created(record) => record,
            other => panic!("unexpected admission: {other:?}"),
        };
        let validating = store.mark_validating(&accepted).unwrap();
        let applying = store
            .mark_applying(&validating, verified_identity())
            .unwrap();
        let terminal = NativeApplyTerminalRecord {
            dispatch: applying.clone(),
            outcome: NativeApplyTerminalOutcome::Applied,
            recovery_cleared: true,
            diagnostic: String::new(),
        };
        // Model a power loss after the terminal record is durable but before
        // complete() removes the still-identical active record.
        write_new_private_file(&store.terminal_path(), &terminal.encode().unwrap()).unwrap();
        sync_directory(&store.root).unwrap();

        let mut foreign_request = start();
        foreign_request.option_id = Some("recommended".to_string());
        let mut foreign_candidate = accepted_dispatch();
        foreign_candidate.option_id = "recommended".to_string();
        assert!(store
            .admit(&foreign_request, foreign_candidate)
            .unwrap_err()
            .contains("another native Apply request is already active"));
        assert_eq!(store.read_active().unwrap(), Some(applying.clone()));
        assert_eq!(store.read_terminal().unwrap(), Some(terminal.clone()));

        match store.admit(&request, accepted_dispatch()).unwrap() {
            NativeApplyAdmission::Terminal(record) => assert_eq!(record, terminal),
            other => panic!("unexpected matching crash-window retry: {other:?}"),
        }
        assert_eq!(store.read_active().unwrap(), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn wrong_token_cannot_observe_active_or_terminal_receipts() {
        let root = root("token");
        let store = NativeApplyCoordinatorStore::new(&root);
        let request = start();
        let accepted = match store.admit(&request, accepted_dispatch()).unwrap() {
            NativeApplyAdmission::Created(record) => record,
            other => panic!("unexpected admission: {other:?}"),
        };
        let wrong = NativeApplyControlRequest::query(
            "aa".repeat(16),
            NativeApplyControlCommand::Status,
            accepted.apply_job_id.clone(),
            "ff".repeat(32),
        );
        assert_eq!(store.authorized_active(&wrong).unwrap(), None);
        let right = NativeApplyControlRequest::query(
            "bb".repeat(16),
            NativeApplyControlCommand::Status,
            accepted.apply_job_id.clone(),
            accepted.apply_job_token.clone(),
        );
        assert_eq!(store.authorized_active(&right).unwrap(), Some(accepted));
        fs::remove_dir_all(root).unwrap();
    }
}
