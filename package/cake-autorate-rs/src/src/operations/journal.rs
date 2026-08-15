use super::identity::{CoordinatorIdentity, ProcessIdentity};
use super::protocol::{OperationRequest, OperationState, MAX_OPERATION_RECORD_BYTES};
use super::state;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

const RUNTIME_OWNER_JOURNAL_HEADER: &str = "cake-autorate-calibration\t3\tjournal";
const PUBLICATION_JOURNAL_HEADER: &str = "cake-autorate-calibration\t4\tjournal";
const JOURNAL_HEADER: &str = "cake-autorate-calibration\t2\tjournal";
const LEGACY_JOURNAL_HEADER: &str = "cake-autorate-calibration\t1\tjournal";
pub(crate) const MAX_JOURNAL_JOBS: usize = 64;
pub(crate) const JOURNAL_RETENTION_TARGET: usize = 48;
const MAX_JOURNAL_DIRECTORY_ENTRIES: usize = 256;
const JOBS_DIR: &str = "jobs";
const REQUEST_FILE: &str = "request";
const STATE_FILE: &str = "state";
const RETIRED_JOB_PREFIX: &str = ".retired-";

pub(crate) fn bootstrap_runtime_directory(
    job_dir: &Path,
    worker_run_id: &str,
) -> Result<PathBuf, String> {
    require_lower_hex("worker_run_id", worker_run_id, 32)?;
    Ok(job_dir.join(format!("bootstrap-runtime-{worker_run_id}")))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeJobPaths {
    pub request: PathBuf,
    pub terminal: PathBuf,
    pub review: PathBuf,
    pub apply_manifest: PathBuf,
    pub public_result: PathBuf,
    pub permit: PathBuf,
    pub stdout: PathBuf,
    pub stderr: PathBuf,
    pub bootstrap_runtime_dir: PathBuf,
    pub bootstrap_runtime_stdout: PathBuf,
    pub bootstrap_runtime_stderr: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobJournal {
    pub job_id: String,
    pub boot_id: String,
    pub coordinator_generation: String,
    pub state: OperationState,
    pub sequence: u64,
    pub heavy_traffic: bool,
    pub heavy_lease_acquired: bool,
    pub instance: String,
    pub target_interface: String,
    pub sqm_fingerprint: String,
    pub process: Option<ProcessIdentity>,
    pub runtime_owner_process: Option<ProcessIdentity>,
    pub worker_run_id: Option<String>,
    pub terminal_kind: Option<String>,
    pub terminal_state: Option<String>,
    pub publication_boot_id: Option<String>,
    pub publication_generation: Option<String>,
    pub diagnostic_code: Option<String>,
    pub reconcile_attempts: u32,
    pub runtime_mutated: bool,
    pub recovery_required: bool,
}

impl JobJournal {
    pub fn queued(
        request: &OperationRequest,
        coordinator: &CoordinatorIdentity,
        heavy_traffic: bool,
    ) -> Result<Self, String> {
        request.validate()?;
        let journal = Self {
            job_id: request.identity.job_id.clone(),
            boot_id: coordinator.boot_id.clone(),
            coordinator_generation: coordinator.generation.clone(),
            state: OperationState::Queued,
            sequence: 1,
            heavy_traffic,
            heavy_lease_acquired: false,
            instance: request.identity.instance.clone(),
            target_interface: request.identity.target_interface.clone(),
            sqm_fingerprint: request.identity.sqm_fingerprint.clone(),
            process: None,
            runtime_owner_process: None,
            worker_run_id: None,
            terminal_kind: None,
            terminal_state: None,
            publication_boot_id: None,
            publication_generation: None,
            diagnostic_code: None,
            reconcile_attempts: 0,
            runtime_mutated: false,
            recovery_required: false,
        };
        journal.validate()?;
        Ok(journal)
    }

    pub fn validate(&self) -> Result<(), String> {
        require_lower_hex("job_id", &self.job_id, 32)?;
        require_lower_hex("boot_id", &self.boot_id, 32)?;
        require_lower_hex("coordinator_generation", &self.coordinator_generation, 32)?;
        require_identifier("instance", &self.instance, b"_-")?;
        require_identifier("target_interface", &self.target_interface, b"._:@-")?;
        require_lower_hex("sqm_fingerprint", &self.sqm_fingerprint, 64)?;
        if self.sequence == 0 {
            return Err("journal sequence must be non-zero".to_string());
        }
        if self.recovery_required && !self.runtime_mutated {
            return Err("recovery cannot be required without a runtime mutation".to_string());
        }
        if self.heavy_lease_acquired && !self.heavy_traffic {
            return Err("light operation cannot own the heavy-traffic lease".to_string());
        }
        if let Some(process) = &self.process {
            if process.pid == 0 || process.process_group == 0 || process.starttime_ticks == 0 {
                return Err("journal process identity must be non-zero".to_string());
            }
        }
        if let Some(process) = &self.runtime_owner_process {
            if process.pid == 0 || process.process_group == 0 || process.starttime_ticks == 0 {
                return Err("runtime owner process identity must be non-zero".to_string());
            }
            if self.worker_run_id.is_none() || !self.heavy_traffic || state::terminal(self.state) {
                return Err("runtime owner requires a live, armed heavy-traffic job".to_string());
            }
            if self.process.as_ref().is_some_and(|worker| {
                worker == process || worker.process_group == process.process_group
            }) {
                return Err(
                    "runtime owner must be isolated from the operation worker process group"
                        .to_string(),
                );
            }
        }
        if let Some(run_id) = &self.worker_run_id {
            require_lower_hex("worker_run_id", run_id, 32)?;
        }
        if let Some(kind) = &self.terminal_kind {
            if kind != "result" && kind != "error" {
                return Err("journal terminal kind is unsupported".to_string());
            }
        }
        if let Some(state) = &self.terminal_state {
            require_identifier("terminal_state", state, b"_-")?;
        }
        if let Some(code) = &self.diagnostic_code {
            require_identifier("diagnostic_code", code, b"_-")?;
        }
        if self.reconcile_attempts > 1_000_000 {
            return Err("journal reconciliation attempt count is unreasonable".to_string());
        }
        if self.process.is_some() && self.worker_run_id.is_none() {
            return Err("journal process requires an attached worker run identity".to_string());
        }
        if self.terminal_kind.is_some() != self.terminal_state.is_some() {
            return Err("journal terminal evidence is partial".to_string());
        }
        if self.terminal_kind.is_some() && !state::terminal(self.state) {
            return Err("journal terminal evidence requires a terminal state".to_string());
        }
        match (
            self.publication_boot_id.as_deref(),
            self.publication_generation.as_deref(),
        ) {
            (None, None) => {}
            (Some(boot_id), Some(generation)) => {
                require_lower_hex("publication_boot_id", boot_id, 32)?;
                require_lower_hex("publication_generation", generation, 32)?;
                if !matches!(
                    self.state,
                    OperationState::ReviewReady | OperationState::Completed
                ) || self.terminal_kind.as_deref() != Some("result")
                    || self.terminal_state.as_deref() != Some("complete")
                    || self.runtime_mutated
                    || self.recovery_required
                    || self.process.is_some()
                    || self.runtime_owner_process.is_some()
                {
                    return Err(
                        "native publication identity requires an inert completed Review"
                            .to_string(),
                    );
                }
            }
            _ => return Err("native publication identity is partial".to_string()),
        }
        Ok(())
    }

    pub fn publication_identity(&self) -> Result<(&str, &str), String> {
        match (
            self.publication_boot_id.as_deref(),
            self.publication_generation.as_deref(),
        ) {
            (Some(boot_id), Some(generation)) => Ok((boot_id, generation)),
            _ => Err("native Review has no immutable publication identity".to_string()),
        }
    }

    pub fn transition(&mut self, next: OperationState) -> Result<(), String> {
        state::validate_transition(self.state, next)?;
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| "journal sequence overflow".to_string())?;
        self.state = next;
        self.validate()
    }

    pub fn fail_before_runtime_mutation(&mut self, diagnostic_code: &str) -> Result<(), String> {
        if self.runtime_mutated || self.recovery_required || self.process.is_some() {
            return Err("preflight failure cannot settle a runtime-owning journal".to_string());
        }
        self.transition(OperationState::Failed)?;
        require_identifier("diagnostic_code", diagnostic_code, b"_-")?;
        self.diagnostic_code = Some(diagnostic_code.to_string());
        self.heavy_lease_acquired = false;
        self.validate()
    }

    pub fn mark_heavy_lease_acquired(&mut self) -> Result<(), String> {
        if !self.heavy_traffic || self.heavy_lease_acquired {
            return Err("heavy lease is not required or is already journalled".to_string());
        }
        if self.state != OperationState::Queued
            || self.runtime_mutated
            || self.recovery_required
            || self.process.is_some()
        {
            return Err("heavy lease can be armed only before runtime mutation".to_string());
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| "journal sequence overflow".to_string())?;
        self.heavy_lease_acquired = true;
        self.validate()
    }

    /// Acquire the global heavy-traffic lease only after an absent-target
    /// worker and its independent runtime owner have published a durable,
    /// read-only readiness witness.  Existing managed jobs retain the older
    /// queued-before-spawn ordering.
    pub fn mark_bootstrap_heavy_lease_acquired(&mut self) -> Result<(), String> {
        if !self.heavy_traffic || self.heavy_lease_acquired {
            return Err(
                "bootstrap heavy lease is not required or is already journalled".to_string(),
            );
        }
        if self.state != OperationState::Running
            || self.runtime_mutated
            || self.recovery_required
            || self.process.is_none()
            || self.runtime_owner_process.is_none()
            || self.worker_run_id.is_none()
        {
            return Err(
                "bootstrap heavy lease requires an attached parked worker and runtime owner"
                    .to_string(),
            );
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| "journal sequence overflow".to_string())?;
        self.heavy_lease_acquired = true;
        self.validate()
    }

    pub fn release_heavy_lease_during_recovery(&mut self) -> Result<(), String> {
        if !self.heavy_traffic
            || !self.heavy_lease_acquired
            || self.state != OperationState::Recovering
            || !self.runtime_mutated
            || !self.recovery_required
        {
            return Err(
                "heavy lease can be detached only from an exact runtime recovery".to_string(),
            );
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| "journal sequence overflow".to_string())?;
        self.heavy_lease_acquired = false;
        self.validate()
    }

    pub fn arm_runtime_mutation(&mut self, worker_run_id: String) -> Result<(), String> {
        self.transition(OperationState::Starting)?;
        require_lower_hex("worker_run_id", &worker_run_id, 32)?;
        self.worker_run_id = Some(worker_run_id);
        self.runtime_mutated = true;
        self.recovery_required = true;
        self.validate()
    }

    pub fn arm_native_worker(&mut self, worker_run_id: String) -> Result<(), String> {
        if self.runtime_mutated || self.recovery_required || self.process.is_some() {
            return Err("native worker cannot arm a runtime-owning journal".to_string());
        }
        self.transition(OperationState::Starting)?;
        require_lower_hex("worker_run_id", &worker_run_id, 32)?;
        self.worker_run_id = Some(worker_run_id);
        self.validate()
    }

    pub fn attach_native_running(
        &mut self,
        process: ProcessIdentity,
        worker_run_id: String,
    ) -> Result<(), String> {
        if self.state != OperationState::Starting
            || self.runtime_mutated
            || self.recovery_required
            || self.process.is_some()
        {
            return Err(
                "native worker can attach only to an armed non-mutating journal".to_string(),
            );
        }
        if self.worker_run_id.as_deref() != Some(&worker_run_id) {
            return Err("native worker identity differs from the armed journal".to_string());
        }
        self.transition(OperationState::Running)?;
        self.process = Some(process);
        self.validate()
    }

    /// Durably declare that an already attached native worker may mutate the
    /// managed runtime.  The coordinator must persist this transition before
    /// publishing the worker-bound runtime permit.  Until then the worker is
    /// parked and a coordinator crash is a pre-mutation failure, not a false
    /// recovery transaction.
    pub fn arm_attached_runtime_mutation(&mut self) -> Result<(), String> {
        if self.state != OperationState::Running
            || self.runtime_mutated
            || self.recovery_required
            || self.process.is_none()
            || self.worker_run_id.is_none()
            || !self.heavy_traffic
            || !self.heavy_lease_acquired
        {
            return Err(
                "runtime mutation can arm only for an attached, leased native worker".to_string(),
            );
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| "journal sequence overflow".to_string())?;
        self.runtime_mutated = true;
        self.recovery_required = true;
        self.validate()
    }

    /// Bind the independently supervised bootstrap runtime owner before any
    /// absent-target permit can authorize a topology mutation.  The worker
    /// and owner must live in different process groups so worker death cannot
    /// take the restore authority down with it.
    pub fn attach_bootstrap_runtime_owner(
        &mut self,
        process: ProcessIdentity,
    ) -> Result<(), String> {
        if self.state != OperationState::Running
            || self.runtime_mutated
            || self.recovery_required
            || self.process.is_none()
            || self.worker_run_id.is_none()
            || !self.heavy_traffic
            || self.runtime_owner_process.is_some()
        {
            return Err(
                "bootstrap runtime owner can attach only to a parked heavy-traffic worker"
                    .to_string(),
            );
        }
        let mut staged = self.clone();
        staged.sequence = staged
            .sequence
            .checked_add(1)
            .ok_or_else(|| "journal sequence overflow".to_string())?;
        staged.runtime_owner_process = Some(process);
        staged.validate()?;
        *self = staged;
        Ok(())
    }

    /// Bootstrap counterpart of `arm_attached_runtime_mutation`.  Persisting
    /// this transition is the final coordinator-side gate before either the
    /// worker or runtime owner receives a permit.
    pub fn arm_attached_bootstrap_runtime_mutation(&mut self) -> Result<(), String> {
        if self.runtime_owner_process.is_none() {
            return Err(
                "bootstrap runtime mutation requires an attached runtime owner".to_string(),
            );
        }
        self.arm_attached_runtime_mutation()
    }

    pub fn clear_bootstrap_runtime_owner(&mut self) -> Result<(), String> {
        if self.runtime_owner_process.is_none() || state::terminal(self.state) {
            return Err("bootstrap runtime owner is not attached to a live job".to_string());
        }
        let mut staged = self.clone();
        staged.sequence = staged
            .sequence
            .checked_add(1)
            .ok_or_else(|| "journal sequence overflow".to_string())?;
        staged.runtime_owner_process = None;
        staged.validate()?;
        *self = staged;
        Ok(())
    }

    pub fn replace_bootstrap_runtime_owner_during_recovery(
        &mut self,
        process: ProcessIdentity,
    ) -> Result<(), String> {
        if self.state != OperationState::Recovering
            || !self.runtime_mutated
            || !self.recovery_required
            || self.worker_run_id.is_none()
            || !self.heavy_traffic
        {
            return Err(
                "bootstrap runtime owner can be replaced only during exact recovery".to_string(),
            );
        }
        let mut staged = self.clone();
        staged.sequence = staged
            .sequence
            .checked_add(1)
            .ok_or_else(|| "journal sequence overflow".to_string())?;
        staged.runtime_owner_process = Some(process);
        staged.validate()?;
        *self = staged;
        Ok(())
    }

    /// Persist a fatal pre-mutation decision before stopping a parked worker.
    /// Reconciliation after coordinator restart can then finish the same
    /// failure instead of accidentally admitting the worker later.
    pub fn stop_attached_before_runtime_mutation(
        &mut self,
        diagnostic_code: &str,
    ) -> Result<(), String> {
        if self.state != OperationState::Running
            || self.runtime_mutated
            || self.recovery_required
            || self.process.is_none()
            || self.worker_run_id.is_none()
        {
            return Err("only an attached parked worker can stop before mutation".to_string());
        }
        require_identifier("diagnostic_code", diagnostic_code, b"_-")?;
        self.transition(OperationState::Cancelling)?;
        self.diagnostic_code = Some(diagnostic_code.to_string());
        self.validate()
    }

    pub fn attach_native_cancelling(
        &mut self,
        process: ProcessIdentity,
        worker_run_id: String,
    ) -> Result<(), String> {
        if self.state != OperationState::Cancelling
            || self.runtime_mutated
            || self.recovery_required
            || self.process.is_some()
        {
            return Err(
                "native worker can attach to cancellation only before runtime mutation".to_string(),
            );
        }
        if self.worker_run_id.as_deref() != Some(&worker_run_id) {
            return Err(
                "cancelled native worker identity differs from the armed journal".to_string(),
            );
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| "journal sequence overflow".to_string())?;
        self.process = Some(process);
        self.validate()
    }

    pub fn settle_native_terminal(
        &mut self,
        terminal_state: &str,
        diagnostic_code: Option<&str>,
    ) -> Result<(), String> {
        if self.runtime_mutated || self.recovery_required || self.worker_run_id.is_none() {
            return Err(
                "native terminal cannot settle a runtime-owning or unarmed journal".to_string(),
            );
        }
        let (next, terminal_kind) = match terminal_state {
            "complete" => (OperationState::Completed, "result"),
            "cancelled" => (OperationState::Cancelled, "error"),
            "incomplete" | "failed" => (OperationState::Failed, "error"),
            _ => return Err("native terminal state is unsupported".to_string()),
        };
        // The terminal state and both process identities form one journal
        // invariant.  Calling `transition()` here would validate a forbidden
        // intermediate state in which a terminal bootstrap job still owns its
        // runtime process.  Validate the edge first, then publish the complete
        // terminal representation atomically through the caller's durable
        // journal replacement.
        state::validate_transition(self.state, next)?;
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| "journal sequence overflow".to_string())?;
        self.state = next;
        self.process = None;
        self.runtime_owner_process = None;
        self.terminal_kind = Some(terminal_kind.to_string());
        self.terminal_state = Some(terminal_state.to_string());
        self.heavy_lease_acquired = false;
        self.diagnostic_code = diagnostic_code.map(str::to_string);
        self.validate()
    }

    pub fn attach_running_process(
        &mut self,
        process: ProcessIdentity,
        worker_run_id: String,
    ) -> Result<(), String> {
        if self.state != OperationState::Starting || !self.runtime_mutated {
            return Err("worker can attach only to an armed starting journal".to_string());
        }
        if self.worker_run_id.as_deref() != Some(&worker_run_id) {
            return Err("attached worker run identity differs from the armed journal".to_string());
        }
        self.transition(OperationState::Running)?;
        self.process = Some(process);
        // A live legacy worker may already own temporary SQM, nftables, route,
        // or autorate state.  Recovery remains armed until a separately
        // attested terminal result confirms restoration.
        self.recovery_required = true;
        self.validate()
    }

    pub fn attach_cancelling_process(
        &mut self,
        process: ProcessIdentity,
        worker_run_id: String,
    ) -> Result<(), String> {
        if self.state != OperationState::Cancelling || !self.runtime_mutated {
            return Err(
                "worker can attach to cancellation only after dispatch was armed".to_string(),
            );
        }
        if self.process.is_some() {
            return Err("cancelling journal already has a worker process".to_string());
        }
        if self.worker_run_id.as_deref() != Some(&worker_run_id) {
            return Err("cancelled worker run identity differs from the armed journal".to_string());
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| "journal sequence overflow".to_string())?;
        self.process = Some(process);
        self.recovery_required = true;
        self.validate()
    }

    pub fn settle_legacy_terminal(
        &mut self,
        terminal_kind: &str,
        terminal_state: &str,
    ) -> Result<(), String> {
        if !self.runtime_mutated || !self.recovery_required || self.worker_run_id.is_none() {
            return Err("legacy terminal cannot settle an unarmed journal".to_string());
        }
        let next = match (terminal_kind, terminal_state) {
            ("result", "complete") => OperationState::ReviewReady,
            ("error", "cancelled") => OperationState::Cancelled,
            ("error", _) => OperationState::Failed,
            _ => return Err("legacy terminal kind/state combination is unsupported".to_string()),
        };
        self.transition(next)?;
        self.process = None;
        self.terminal_kind = Some(terminal_kind.to_string());
        self.terminal_state = Some(terminal_state.to_string());
        self.runtime_mutated = false;
        self.recovery_required = false;
        self.heavy_lease_acquired = false;
        self.diagnostic_code = None;
        self.validate()
    }

    pub fn settle_runtime_recovery(
        &mut self,
        cancelled: bool,
        diagnostic_code: &str,
    ) -> Result<(), String> {
        if self.state != OperationState::Recovering
            || !self.runtime_mutated
            || !self.recovery_required
            || self.worker_run_id.is_none()
        {
            return Err("runtime recovery cannot settle an unarmed journal".to_string());
        }
        require_identifier("runtime recovery diagnostic", diagnostic_code, b"_-")?;
        self.transition(if cancelled {
            OperationState::Cancelled
        } else {
            OperationState::Failed
        })?;
        self.process = None;
        self.terminal_kind = Some("error".to_string());
        self.terminal_state = Some(if cancelled { "cancelled" } else { "failed" }.to_string());
        self.runtime_mutated = false;
        self.recovery_required = false;
        self.heavy_lease_acquired = false;
        self.diagnostic_code = Some(diagnostic_code.to_string());
        self.validate()
    }

    /// Settle a native runtime-owning operation only after the instance daemon
    /// has restored and attested its exact checkpoint.  Unlike the generic
    /// recovery fallback, a verified `complete` terminal preserves the Review
    /// hand-off instead of converting a successful Auto-Tune into a failure.
    pub fn settle_native_runtime_terminal(
        &mut self,
        terminal_state: &str,
        diagnostic_code: Option<&str>,
    ) -> Result<(), String> {
        if self.state != OperationState::Recovering
            || !self.runtime_mutated
            || !self.recovery_required
            || self.worker_run_id.is_none()
            || self.process.is_some()
        {
            return Err(
                "native runtime terminal requires an exactly restored recovering journal"
                    .to_string(),
            );
        }
        let (next, terminal_kind) = match terminal_state {
            "complete" if diagnostic_code.is_none() => (OperationState::ReviewReady, "result"),
            "cancelled" => (OperationState::Cancelled, "error"),
            "inconclusive" | "failed" => (OperationState::Failed, "error"),
            _ => return Err("native runtime terminal state is unsupported".to_string()),
        };
        self.transition(next)?;
        self.terminal_kind = Some(terminal_kind.to_string());
        self.terminal_state = Some(terminal_state.to_string());
        if terminal_state == "complete" && diagnostic_code.is_none() {
            self.publication_boot_id = Some(self.boot_id.clone());
            self.publication_generation = Some(self.coordinator_generation.clone());
        } else {
            self.publication_boot_id = None;
            self.publication_generation = None;
        }
        self.runtime_mutated = false;
        self.recovery_required = false;
        self.heavy_lease_acquired = false;
        self.diagnostic_code = diagnostic_code.map(str::to_string);
        self.validate()
    }

    /// Settle a restore-first standalone Speed Test after the same exact
    /// instance-owned checkpoint restoration, but without inventing an
    /// Auto-Tune Review state for its completed measurement terminal.
    pub fn settle_native_speedtest_runtime_terminal(
        &mut self,
        terminal_state: &str,
        diagnostic_code: Option<&str>,
    ) -> Result<(), String> {
        if self.state != OperationState::Recovering
            || !self.runtime_mutated
            || !self.recovery_required
            || self.worker_run_id.is_none()
            || self.process.is_some()
        {
            return Err(
                "native Speed Test runtime terminal requires an exactly restored recovering journal"
                    .to_string(),
            );
        }
        let (next, terminal_kind) = match terminal_state {
            "complete" if diagnostic_code.is_none() => (OperationState::Completed, "result"),
            "cancelled" => (OperationState::Cancelled, "error"),
            "failed" => (OperationState::Failed, "error"),
            _ => return Err("native Speed Test runtime terminal state is unsupported".to_string()),
        };
        self.transition(next)?;
        self.terminal_kind = Some(terminal_kind.to_string());
        self.terminal_state = Some(terminal_state.to_string());
        self.runtime_mutated = false;
        self.recovery_required = false;
        self.heavy_lease_acquired = false;
        self.diagnostic_code = diagnostic_code.map(str::to_string);
        self.validate()
    }

    pub fn require_recovery(&mut self, diagnostic_code: &str) -> Result<(), String> {
        if self.state != OperationState::Recovering {
            self.transition(OperationState::Recovering)?;
        } else {
            self.sequence = self
                .sequence
                .checked_add(1)
                .ok_or_else(|| "journal sequence overflow".to_string())?;
        }
        self.process = None;
        self.runtime_mutated = true;
        self.recovery_required = true;
        self.diagnostic_code = Some(diagnostic_code.to_string());
        self.validate()
    }

    pub fn record_reconciliation_failure(&mut self, diagnostic_code: &str) -> Result<(), String> {
        if self.state != OperationState::Recovering || !self.recovery_required {
            return Err("only a recovering journal can record reconciliation failure".to_string());
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| "journal sequence overflow".to_string())?;
        self.reconcile_attempts = self
            .reconcile_attempts
            .checked_add(1)
            .ok_or_else(|| "reconciliation attempt count overflow".to_string())?;
        self.diagnostic_code = Some(diagnostic_code.to_string());
        self.validate()
    }

    pub fn encode(&self) -> Result<String, String> {
        let schema = if self.publication_boot_id.is_some() {
            4
        } else if self.runtime_owner_process.is_some() {
            3
        } else {
            2
        };
        self.encode_for_schema(schema)
    }

    fn encode_for_schema(&self, schema: u8) -> Result<String, String> {
        self.validate()?;
        if !matches!(schema, 2..=4) {
            return Err("unsupported journal encoding schema".to_string());
        }
        if schema == 2 && self.runtime_owner_process.is_some() {
            return Err("journal schema 2 cannot encode a runtime owner".to_string());
        }
        if schema == 3 && self.runtime_owner_process.is_none() {
            return Err("journal schema 3 requires a runtime owner".to_string());
        }
        if schema == 4
            && (self.runtime_owner_process.is_some()
                || self.publication_boot_id.is_none()
                || self.publication_generation.is_none())
        {
            return Err(
                "journal schema 4 requires an inert native publication identity".to_string(),
            );
        }
        if schema != 4
            && (self.publication_boot_id.is_some() || self.publication_generation.is_some())
        {
            return Err("journal schema 2/3 cannot encode native publication identity".to_string());
        }
        let (pid, process_group, starttime) = match &self.process {
            Some(process) => (
                process.pid.to_string(),
                process.process_group.to_string(),
                process.starttime_ticks.to_string(),
            ),
            None => (String::new(), String::new(), String::new()),
        };
        let mut fields = vec![
            ("job_id", self.job_id.clone()),
            ("boot_id", self.boot_id.clone()),
            (
                "coordinator_generation",
                self.coordinator_generation.clone(),
            ),
            ("state", self.state.as_str().to_string()),
            ("sequence", self.sequence.to_string()),
            ("heavy_traffic", bool_text(self.heavy_traffic).to_string()),
            (
                "heavy_lease_acquired",
                bool_text(self.heavy_lease_acquired).to_string(),
            ),
            ("instance", self.instance.clone()),
            ("target_interface", self.target_interface.clone()),
            ("sqm_fingerprint", self.sqm_fingerprint.clone()),
            ("process_pid", pid),
            ("process_group", process_group),
            ("process_starttime", starttime),
            (
                "worker_run_id",
                self.worker_run_id.clone().unwrap_or_default(),
            ),
            (
                "terminal_kind",
                self.terminal_kind.clone().unwrap_or_default(),
            ),
            (
                "terminal_state",
                self.terminal_state.clone().unwrap_or_default(),
            ),
            (
                "diagnostic_code",
                self.diagnostic_code.clone().unwrap_or_default(),
            ),
            ("reconcile_attempts", self.reconcile_attempts.to_string()),
            (
                "runtime_mutated",
                bool_text(self.runtime_mutated).to_string(),
            ),
            (
                "recovery_required",
                bool_text(self.recovery_required).to_string(),
            ),
        ];
        if schema == 3 {
            let owner = self
                .runtime_owner_process
                .as_ref()
                .expect("schema 3 runtime owner was validated");
            fields.extend([
                ("runtime_owner_pid", owner.pid.to_string()),
                ("runtime_owner_group", owner.process_group.to_string()),
                ("runtime_owner_starttime", owner.starttime_ticks.to_string()),
            ]);
        } else if schema == 4 {
            fields.extend([
                (
                    "publication_boot_id",
                    self.publication_boot_id
                        .clone()
                        .expect("schema 4 publication boot ID was validated"),
                ),
                (
                    "publication_generation",
                    self.publication_generation
                        .clone()
                        .expect("schema 4 publication generation was validated"),
                ),
            ]);
        }
        let mut output = String::from(match schema {
            2 => JOURNAL_HEADER,
            3 => RUNTIME_OWNER_JOURNAL_HEADER,
            4 => PUBLICATION_JOURNAL_HEADER,
            _ => unreachable!("journal schema was validated"),
        });
        output.push('\n');
        for (name, value) in fields {
            output.push_str(name);
            output.push('=');
            output.push_str(&value);
            output.push('\n');
        }
        if output.len() > MAX_OPERATION_RECORD_BYTES {
            return Err("journal record exceeds its size bound".to_string());
        }
        Ok(output)
    }

    pub fn decode(input: &str) -> Result<Self, String> {
        if input.len() > MAX_OPERATION_RECORD_BYTES || !input.ends_with('\n') {
            return Err("journal record is not bounded and newline-terminated".to_string());
        }
        let mut lines = input.lines();
        let header = lines
            .next()
            .ok_or_else(|| "journal record header is missing".to_string())?;
        let schema = match header {
            LEGACY_JOURNAL_HEADER => 1,
            JOURNAL_HEADER => 2,
            RUNTIME_OWNER_JOURNAL_HEADER => 3,
            PUBLICATION_JOURNAL_HEADER => 4,
            _ => return Err("unsupported journal record header".to_string()),
        };
        let legacy = schema == 1;
        let job_id = field(&mut lines, "job_id")?;
        let boot_id = field(&mut lines, "boot_id")?;
        let coordinator_generation = field(&mut lines, "coordinator_generation")?;
        let state = OperationState::parse(&field(&mut lines, "state")?)
            .ok_or_else(|| "unsupported journal state".to_string())?;
        let sequence = parse_u64("sequence", &field(&mut lines, "sequence")?)?;
        let heavy_traffic = parse_bool(&field(&mut lines, "heavy_traffic")?)?;
        let heavy_lease_acquired = if legacy {
            // Version 1 acquired the heavy lease at admission time.
            heavy_traffic
        } else {
            parse_bool(&field(&mut lines, "heavy_lease_acquired")?)?
        };
        let instance = field(&mut lines, "instance")?;
        let target_interface = field(&mut lines, "target_interface")?;
        let sqm_fingerprint = field(&mut lines, "sqm_fingerprint")?;
        let process_pid = field(&mut lines, "process_pid")?;
        let process_group = field(&mut lines, "process_group")?;
        let process_starttime = field(&mut lines, "process_starttime")?;
        let worker_run_id = optional_field(field(&mut lines, "worker_run_id")?);
        let terminal_kind = optional_field(field(&mut lines, "terminal_kind")?);
        let terminal_state = optional_field(field(&mut lines, "terminal_state")?);
        let diagnostic_code = optional_field(field(&mut lines, "diagnostic_code")?);
        let reconcile_attempts = parse_u32(
            "reconcile_attempts",
            &field(&mut lines, "reconcile_attempts")?,
        )?;
        let runtime_mutated = parse_bool(&field(&mut lines, "runtime_mutated")?)?;
        let recovery_required = parse_bool(&field(&mut lines, "recovery_required")?)?;
        let runtime_owner_process = if schema == 3 {
            let pid = field(&mut lines, "runtime_owner_pid")?;
            let process_group = field(&mut lines, "runtime_owner_group")?;
            let starttime = field(&mut lines, "runtime_owner_starttime")?;
            Some(ProcessIdentity {
                pid: parse_u32("runtime_owner_pid", &pid)?,
                process_group: parse_u32("runtime_owner_group", &process_group)?,
                starttime_ticks: parse_u64("runtime_owner_starttime", &starttime)?,
            })
        } else {
            None
        };
        let (publication_boot_id, publication_generation) = if schema == 4 {
            (
                Some(field(&mut lines, "publication_boot_id")?),
                Some(field(&mut lines, "publication_generation")?),
            )
        } else {
            (None, None)
        };
        if lines.next().is_some() {
            return Err("journal record contains unknown fields".to_string());
        }
        let process = match (
            process_pid.is_empty(),
            process_group.is_empty(),
            process_starttime.is_empty(),
        ) {
            (true, true, true) => None,
            (false, false, false) => Some(ProcessIdentity {
                pid: parse_u32("process_pid", &process_pid)?,
                process_group: parse_u32("process_group", &process_group)?,
                starttime_ticks: parse_u64("process_starttime", &process_starttime)?,
            }),
            _ => return Err("journal process identity is partial".to_string()),
        };
        let journal = Self {
            job_id,
            boot_id,
            coordinator_generation,
            state,
            sequence,
            heavy_traffic,
            heavy_lease_acquired,
            instance,
            target_interface,
            sqm_fingerprint,
            process,
            runtime_owner_process,
            worker_run_id,
            terminal_kind,
            terminal_state,
            publication_boot_id,
            publication_generation,
            diagnostic_code,
            reconcile_attempts,
            runtime_mutated,
            recovery_required,
        };
        journal.validate()?;
        if !legacy && journal.encode_for_schema(schema)? != input {
            return Err("journal record is not canonically encoded".to_string());
        }
        Ok(journal)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalDisposition {
    Queued,
    Launching,
    LiveProcess,
    DeadBeforeMutation,
    Settled,
    RecoveryRequired,
    StaleBoot,
}

#[derive(Clone, Debug)]
pub struct ScannedJob {
    pub request: OperationRequest,
    pub journal: JobJournal,
    pub disposition: JournalDisposition,
}

#[derive(Default)]
pub struct JournalScan {
    pub jobs: Vec<ScannedJob>,
    pub unsafe_entries: Vec<String>,
}

pub struct JournalStore {
    jobs_dir: PathBuf,
    coordinator: CoordinatorIdentity,
}

impl JournalStore {
    pub fn open(state_dir: &Path, coordinator: CoordinatorIdentity) -> Result<Self, String> {
        let jobs_dir = state_dir.join(JOBS_DIR);
        ensure_secure_dir(&jobs_dir)?;
        let store = Self {
            jobs_dir,
            coordinator,
        };
        store.finish_interrupted_retirements()?;
        Ok(store)
    }

    pub fn create(&self, request: &OperationRequest, journal: &JobJournal) -> Result<(), String> {
        request.validate()?;
        request.validate_admission_policy()?;
        journal.validate()?;
        if request.identity.job_id != journal.job_id
            || request.identity.instance != journal.instance
            || request.identity.target_interface != journal.target_interface
            || request.identity.sqm_fingerprint != journal.sqm_fingerprint
        {
            return Err("journal identity does not match its operation request".to_string());
        }
        if journal.boot_id != self.coordinator.boot_id
            || journal.coordinator_generation != self.coordinator.generation
        {
            return Err("journal coordinator identity is stale".to_string());
        }
        let job_dir = self.jobs_dir.join(&journal.job_id);
        fs::create_dir(&job_dir)
            .map_err(|error| format!("unable to create job journal directory: {error}"))?;
        fs::set_permissions(&job_dir, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("unable to secure job journal directory: {error}"))?;
        if let Err(error) = atomic_write(
            &job_dir,
            REQUEST_FILE,
            &request.encode()?,
            &self.coordinator.generation,
            0,
        )
        .and_then(|_| {
            atomic_write(
                &job_dir,
                STATE_FILE,
                &journal.encode()?,
                &self.coordinator.generation,
                journal.sequence,
            )
        }) {
            return Err(format!("unable to initialize job journal: {error}"));
        }
        Ok(())
    }

    pub fn update(&self, journal: &JobJournal) -> Result<(), String> {
        journal.validate()?;
        if journal.boot_id != self.coordinator.boot_id
            || journal.coordinator_generation != self.coordinator.generation
        {
            return Err("journal update belongs to another coordinator generation".to_string());
        }
        let job_dir = self.jobs_dir.join(&journal.job_id);
        secure_existing_job_dir(&job_dir)?;
        let request = OperationRequest::decode(&read_bounded(&job_dir.join(REQUEST_FILE))?)?;
        if request.identity.job_id != journal.job_id
            || request.identity.instance != journal.instance
            || request.identity.target_interface != journal.target_interface
            || request.identity.sqm_fingerprint != journal.sqm_fingerprint
        {
            return Err("journal update identity does not match its request".to_string());
        }
        atomic_write(
            &job_dir,
            STATE_FILE,
            &journal.encode()?,
            &self.coordinator.generation,
            journal.sequence,
        )
    }

    pub fn native_job_paths(
        &self,
        job_id: &str,
        worker_run_id: &str,
    ) -> Result<NativeJobPaths, String> {
        require_lower_hex("job_id", job_id, 32)?;
        require_lower_hex("worker_run_id", worker_run_id, 32)?;
        let job_dir = self.jobs_dir.join(job_id);
        secure_existing_job_dir(&job_dir)?;
        Ok(NativeJobPaths {
            request: job_dir.join(REQUEST_FILE),
            terminal: job_dir.join(format!("terminal-{worker_run_id}")),
            review: job_dir.join(format!("review-{worker_run_id}.json")),
            apply_manifest: job_dir.join(format!("apply-manifest-{worker_run_id}.json")),
            public_result: job_dir.join(format!("public-review-{worker_run_id}.json")),
            permit: job_dir.join(format!("permit-{worker_run_id}")),
            stdout: job_dir.join(format!("stdout-{worker_run_id}.log")),
            stderr: job_dir.join(format!("stderr-{worker_run_id}.log")),
            bootstrap_runtime_dir: bootstrap_runtime_directory(&job_dir, worker_run_id)?,
            bootstrap_runtime_stdout: job_dir
                .join(format!("bootstrap-runtime-stdout-{worker_run_id}.log")),
            bootstrap_runtime_stderr: job_dir
                .join(format!("bootstrap-runtime-stderr-{worker_run_id}.log")),
        })
    }

    pub fn publish_native_permit(
        &self,
        job_id: &str,
        worker_run_id: &str,
        contents: &str,
    ) -> Result<(), String> {
        let paths = self.native_job_paths(job_id, worker_run_id)?;
        let name = paths
            .permit
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "native permit filename is invalid".to_string())?;
        let job_dir = paths
            .permit
            .parent()
            .ok_or_else(|| "native permit has no job directory".to_string())?;
        atomic_write(job_dir, name, contents, &self.coordinator.generation, 0)
    }

    pub fn adopt_generation(&self, journal: &JobJournal) -> Result<JobJournal, String> {
        journal.validate()?;
        if journal.boot_id != self.coordinator.boot_id {
            return Err("journal from another boot cannot be adopted".to_string());
        }
        let mut adopted = journal.clone();
        adopted.coordinator_generation = self.coordinator.generation.clone();
        adopted.sequence = adopted
            .sequence
            .checked_add(1)
            .ok_or_else(|| "journal sequence overflow during adoption".to_string())?;
        adopted.validate()?;
        let job_dir = self.jobs_dir.join(&adopted.job_id);
        secure_existing_job_dir(&job_dir)?;
        atomic_write(
            &job_dir,
            STATE_FILE,
            &adopted.encode()?,
            &self.coordinator.generation,
            adopted.sequence,
        )?;
        Ok(adopted)
    }

    pub fn scan(&self, proc_root: &Path) -> Result<JournalScan, String> {
        let mut scan = JournalScan::default();
        let entries = fs::read_dir(&self.jobs_dir)
            .map_err(|error| format!("unable to scan calibration journals: {error}"))?;
        for (index, entry) in entries.enumerate() {
            if index >= MAX_JOURNAL_DIRECTORY_ENTRIES {
                return Err(
                    "journal directory entry count exceeds its hard safety bound".to_string(),
                );
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    scan.unsafe_entries
                        .push(format!("unable to inspect journal entry: {error}"));
                    continue;
                }
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if require_lower_hex("job directory", &name, 32).is_err() {
                scan.unsafe_entries
                    .push(format!("unsafe journal directory name: {name}"));
                continue;
            }
            let job_dir = entry.path();
            if let Err(error) = secure_existing_job_dir(&job_dir) {
                scan.unsafe_entries.push(format!("{name}: {error}"));
                continue;
            }
            match self.scan_one(&job_dir, &name, proc_root) {
                Ok(job) => scan.jobs.push(job),
                Err(error) => scan.unsafe_entries.push(format!("{name}: {error}")),
            }
        }
        scan.jobs.sort_by(|left, right| {
            left.journal
                .sequence
                .cmp(&right.journal.sequence)
                .then_with(|| left.journal.job_id.cmp(&right.journal.job_id))
        });
        Ok(scan)
    }

    pub fn retire_settled_job(
        &self,
        expected: &ScannedJob,
        proc_root: &Path,
    ) -> Result<(), String> {
        if expected.disposition != JournalDisposition::Settled
            || !state::terminal(expected.journal.state)
            || expected.journal.runtime_mutated
            || expected.journal.recovery_required
            || expected.journal.heavy_lease_acquired
            || expected.journal.process.is_some()
            || expected.journal.runtime_owner_process.is_some()
        {
            return Err("only an inert settled journal can be retired".to_string());
        }
        let job_dir = self.jobs_dir.join(&expected.journal.job_id);
        secure_existing_job_dir(&job_dir)?;
        let current = self.scan_one(&job_dir, &expected.journal.job_id, proc_root)?;
        if current.request != expected.request
            || current.journal != expected.journal
            || current.disposition != JournalDisposition::Settled
        {
            return Err("journal changed while retirement was being prepared".to_string());
        }
        let retired = self
            .jobs_dir
            .join(format!("{RETIRED_JOB_PREFIX}{}", expected.journal.job_id));
        match fs::symlink_metadata(&retired) {
            Ok(_) => return Err("journal retirement staging path already exists".to_string()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "unable to inspect journal retirement staging path: {error}"
                ));
            }
        }
        fs::rename(&job_dir, &retired)
            .map_err(|error| format!("unable to stage settled journal retirement: {error}"))?;
        sync_directory(&self.jobs_dir)?;
        fs::remove_dir_all(&retired)
            .map_err(|error| format!("unable to remove retired settled journal: {error}"))?;
        sync_directory(&self.jobs_dir)
    }

    fn finish_interrupted_retirements(&self) -> Result<(), String> {
        let entries = fs::read_dir(&self.jobs_dir)
            .map_err(|error| format!("unable to inspect journal retirements: {error}"))?;
        for entry in entries {
            let entry = entry
                .map_err(|error| format!("unable to inspect journal retirement entry: {error}"))?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "journal retirement entry name is not UTF-8".to_string())?;
            let Some(job_id) = name.strip_prefix(RETIRED_JOB_PREFIX) else {
                continue;
            };
            if require_lower_hex("retired job directory", job_id, 32).is_err() {
                continue;
            }
            let path = entry.path();
            secure_existing_job_dir(&path)?;
            fs::remove_dir_all(&path).map_err(|error| {
                format!("unable to finish interrupted journal retirement: {error}")
            })?;
            sync_directory(&self.jobs_dir)?;
        }
        Ok(())
    }

    fn scan_one(
        &self,
        job_dir: &Path,
        directory_job_id: &str,
        proc_root: &Path,
    ) -> Result<ScannedJob, String> {
        let journal = JobJournal::decode(&read_bounded(&job_dir.join(STATE_FILE))?)?;
        let request = OperationRequest::decode(&read_bounded(&job_dir.join(REQUEST_FILE))?)?;
        let inert_terminal = state::terminal(journal.state)
            && !journal.runtime_mutated
            && !journal.recovery_required;
        if !inert_terminal {
            request.validate_admission_policy()?;
        }
        if request.identity.job_id != directory_job_id || journal.job_id != directory_job_id {
            return Err("journal directory and record identity differ".to_string());
        }
        if request.identity.instance != journal.instance
            || request.identity.target_interface != journal.target_interface
            || request.identity.sqm_fingerprint != journal.sqm_fingerprint
        {
            return Err("journal request and state identity differ".to_string());
        }
        let disposition = if journal.boot_id != self.coordinator.boot_id {
            JournalDisposition::StaleBoot
        } else if state::terminal(journal.state) && !journal.runtime_mutated {
            JournalDisposition::Settled
        } else if state::terminal(journal.state) {
            JournalDisposition::RecoveryRequired
        } else if let Some(process) = &journal.process {
            match process.still_matches(proc_root) {
                Ok(true) => JournalDisposition::LiveProcess,
                Ok(false) if journal.runtime_mutated => JournalDisposition::RecoveryRequired,
                Ok(false) => JournalDisposition::DeadBeforeMutation,
                Err(_) => JournalDisposition::RecoveryRequired,
            }
        } else if journal.runtime_mutated || journal.recovery_required {
            JournalDisposition::RecoveryRequired
        } else if journal.state == OperationState::Starting {
            JournalDisposition::DeadBeforeMutation
        } else {
            JournalDisposition::Queued
        };
        Ok(ScannedJob {
            request,
            journal,
            disposition,
        })
    }
}

fn ensure_secure_dir(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || metadata.uid() != unsafe { geteuid() }
            {
                return Err(format!("{} is not a secure directory", path.display()));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path)
                .map_err(|error| format!("unable to create {}: {error}", path.display()))?;
        }
        Err(error) => return Err(format!("unable to inspect {}: {error}", path.display())),
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("unable to secure {}: {error}", path.display()))
}

fn sync_directory(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("unable to sync journal directory: {error}"))
}

fn secure_existing_job_dir(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("unable to inspect job journal directory: {error}"))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err("job journal directory is unsafe or foreign-owned".to_string());
    }
    Ok(())
}

fn atomic_write(
    directory: &Path,
    name: &str,
    contents: &str,
    generation: &str,
    sequence: u64,
) -> Result<(), String> {
    if contents.len() > MAX_OPERATION_RECORD_BYTES {
        return Err("journal payload exceeds its size bound".to_string());
    }
    let temp_name = format!(".{name}.{generation}.{sequence}.tmp");
    let temp_path = directory.join(temp_name);
    let final_path = directory.join(name);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .map_err(|error| format!("unable to create journal temporary file: {error}"))?;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("unable to secure journal temporary file: {error}"))?;
        file.write_all(contents.as_bytes())
            .map_err(|error| format!("unable to write journal temporary file: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("unable to sync journal temporary file: {error}"))?;
        fs::rename(&temp_path, &final_path)
            .map_err(|error| format!("unable to publish journal file: {error}"))?;
        File::open(directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("unable to sync journal directory: {error}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn read_bounded(path: &Path) -> Result<String, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("unable to inspect {}: {error}", path.display()))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { geteuid() }
    {
        return Err(format!("{} is unsafe or foreign-owned", path.display()));
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| {
            file.take((MAX_OPERATION_RECORD_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
        })
        .map_err(|error| format!("unable to read {}: {error}", path.display()))?;
    if bytes.len() > MAX_OPERATION_RECORD_BYTES {
        return Err(format!("{} exceeds its size bound", path.display()));
    }
    String::from_utf8(bytes).map_err(|_| format!("{} is not UTF-8", path.display()))
}

fn field<'a>(lines: &mut impl Iterator<Item = &'a str>, expected: &str) -> Result<String, String> {
    let line = lines
        .next()
        .ok_or_else(|| format!("journal is missing field {expected}"))?;
    let (name, value) = line
        .split_once('=')
        .ok_or_else(|| format!("journal field {expected} is malformed"))?;
    if name != expected || value.contains(['\r', '\n', '=']) {
        return Err(format!("journal expected field {expected}"));
    }
    Ok(value.to_string())
}

fn parse_bool(value: &str) -> Result<bool, String> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err("journal boolean is not canonical".to_string()),
    }
}

fn optional_field(value: String) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn bool_text(value: bool) -> &'static str {
    if value {
        "1"
    } else {
        "0"
    }
}

fn parse_u64(name: &str, value: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|_| format!("journal field {name} is not an unsigned integer"))
}

fn parse_u32(name: &str, value: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .map_err(|_| format!("journal field {name} is not an unsigned integer"))
}

fn require_lower_hex(name: &str, value: &str, length: usize) -> Result<(), String> {
    if value.len() != length
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!(
            "journal {name} must be {length} lowercase hex bytes"
        ));
    }
    Ok(())
}

fn require_identifier(name: &str, value: &str, extra: &[u8]) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || extra.contains(&byte))
    {
        return Err(format!("journal {name} is unsafe"));
    }
    Ok(())
}

extern "C" {
    fn geteuid() -> u32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autotune::{
        AccessEvidenceSource, AccessMedium, AutotuneProfile, CapacityLearningPolicy,
    };
    use crate::operations::protocol::{
        CalibrationStrategy, OperationIdentity, OperationKind, OperationOrigin,
        OperationRouteIdentity, OperationRouteMode, OperationTargetState,
    };

    fn coordinator() -> CoordinatorIdentity {
        CoordinatorIdentity {
            boot_id: "11111111111111111111111111111111".to_string(),
            generation: "22222222222222222222222222222222".to_string(),
            process: ProcessIdentity {
                pid: 10,
                process_group: 10,
                starttime_ticks: 100,
            },
        }
    }

    fn request() -> OperationRequest {
        OperationRequest {
            identity: OperationIdentity {
                job_id: "33333333333333333333333333333333".to_string(),
                job_token: "4444444444444444444444444444444444444444444444444444444444444444"
                    .to_string(),
                instance: "wan_sqm".to_string(),
                operation: OperationKind::FullAutotune,
                target_interface: "pppoe-wan".to_string(),
                route_fingerprint:
                    "5555555555555555555555555555555555555555555555555555555555555555".to_string(),
                config_fingerprint:
                    "6666666666666666666666666666666666666666666666666666666666666666".to_string(),
                sqm_fingerprint: "7777777777777777777777777777777777777777777777777777777777777777"
                    .to_string(),
            },
            created_unix_ms: 1,
            deadline_unix_ms: 2,
            origin: OperationOrigin::Luci,
            backend: "speedtest-go".to_string(),
            speedtest_direction: None,
            speedtest_server_id: None,
            speedtest_topology: None,
            route: OperationRouteIdentity {
                mode: OperationRouteMode::Main,
                mwan3_member: None,
                l3_device: "pppoe-wan".to_string(),
                source_ip: None,
                fwmark: None,
                routing_table: None,
            },
            target_state: OperationTargetState::ExistingManaged,
            capture_policy: None,
            managed_sqm_section: Some("wan_sqm".to_string()),
            profile: Some(AutotuneProfile::VariableLink),
            strategy: Some(CalibrationStrategy::FullRaw),
            access_medium: Some(AccessMedium::Cellular),
            access_source: Some(AccessEvidenceSource::UserSelected),
            access_confidence_percent: 100,
            capacity_learning_policy: Some(CapacityLearningPolicy::PassiveBounded),
            service_dl_cap_kbps: None,
            service_ul_cap_kbps: None,
            allow_sqm_disable: true,
            allow_active_traffic: true,
            scheduled_auto_apply_requested: false,
            traffic_budget_bytes: 1_000_000,
        }
    }

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("cake-journal-{}-{name}", unsafe { geteuid() }))
    }

    fn fake_proc_stat(pid: u32, process_group: u32, starttime: u64) -> String {
        let mut tail = vec![
            "S".to_string(),
            "1".to_string(),
            process_group.to_string(),
            process_group.to_string(),
        ];
        while tail.len() < 19 {
            tail.push("0".to_string());
        }
        tail.push(starttime.to_string());
        format!("{pid} (worker) {} 0", tail.join(" "))
    }

    fn cleanup(root: &Path) {
        if root.exists() {
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn journal_record_is_strict_and_round_trips() {
        let mut journal = JobJournal::queued(&request(), &coordinator(), true).unwrap();
        journal.state = OperationState::Running;
        journal.sequence = 3;
        journal.process = Some(ProcessIdentity {
            pid: 42,
            process_group: 42,
            starttime_ticks: 1234,
        });
        journal.worker_run_id = Some("8".repeat(32));
        journal.runtime_mutated = true;
        journal.recovery_required = true;
        let encoded = journal.encode().unwrap();
        assert!(encoded.starts_with(JOURNAL_HEADER));
        assert!(!encoded.contains("runtime_owner_pid="));
        assert_eq!(JobJournal::decode(&encoded).unwrap(), journal);
        assert!(JobJournal::decode(&encoded.replace("sequence=3", "unknown=3")).is_err());
        assert!(
            JobJournal::decode(&encoded.replace("heavy_traffic=1", "heavy_traffic=true")).is_err()
        );
    }

    #[test]
    fn bootstrap_runtime_owner_uses_v3_without_changing_the_v2_worker_record() {
        let mut journal = JobJournal::queued(&request(), &coordinator(), true).unwrap();
        let worker_run_id = "8".repeat(32);
        journal.arm_native_worker(worker_run_id.clone()).unwrap();
        journal
            .attach_native_running(
                ProcessIdentity {
                    pid: 42,
                    process_group: 42,
                    starttime_ticks: 1234,
                },
                worker_run_id,
            )
            .unwrap();
        let worker_only = journal.encode().unwrap();
        assert!(worker_only.starts_with(JOURNAL_HEADER));
        assert_eq!(JobJournal::decode(&worker_only).unwrap(), journal);

        journal
            .attach_bootstrap_runtime_owner(ProcessIdentity {
                pid: 84,
                process_group: 84,
                starttime_ticks: 5678,
            })
            .unwrap();
        let with_owner = journal.encode().unwrap();
        assert!(with_owner.starts_with(RUNTIME_OWNER_JOURNAL_HEADER));
        assert!(with_owner.contains("runtime_owner_pid=84\n"));
        assert_eq!(JobJournal::decode(&with_owner).unwrap(), journal);
        assert!(journal.encode_for_schema(2).is_err());
        assert!(!journal.heavy_lease_acquired);

        journal.mark_bootstrap_heavy_lease_acquired().unwrap();
        journal.arm_attached_bootstrap_runtime_mutation().unwrap();
        assert!(journal.runtime_mutated);
        assert!(journal.recovery_required);
        journal.clear_bootstrap_runtime_owner().unwrap();
        let cleared = journal.encode().unwrap();
        assert!(cleared.starts_with(JOURNAL_HEADER));
        assert!(!cleared.contains("runtime_owner_pid="));
        assert_eq!(JobJournal::decode(&cleared).unwrap(), journal);
    }

    #[test]
    fn bootstrap_runtime_owner_isolated_readiness_precedes_the_heavy_lease() {
        let mut journal = JobJournal::queued(&request(), &coordinator(), true).unwrap();
        let worker_run_id = "8".repeat(32);
        journal.arm_native_worker(worker_run_id.clone()).unwrap();
        journal
            .attach_native_running(
                ProcessIdentity {
                    pid: 42,
                    process_group: 42,
                    starttime_ticks: 1234,
                },
                worker_run_id,
            )
            .unwrap();
        assert!(journal
            .attach_bootstrap_runtime_owner(ProcessIdentity {
                pid: 84,
                process_group: 42,
                starttime_ticks: 5678,
            })
            .is_err());
        assert!(journal.arm_attached_bootstrap_runtime_mutation().is_err());
        assert!(journal.runtime_owner_process.is_none());
        assert!(!journal.runtime_mutated);

        journal
            .attach_bootstrap_runtime_owner(ProcessIdentity {
                pid: 84,
                process_group: 84,
                starttime_ticks: 5678,
            })
            .unwrap();
        assert!(!journal.heavy_lease_acquired);
        assert!(journal.arm_attached_bootstrap_runtime_mutation().is_err());
        journal.mark_bootstrap_heavy_lease_acquired().unwrap();
        journal.arm_attached_bootstrap_runtime_mutation().unwrap();
        assert!(journal.runtime_mutated);
    }

    #[test]
    fn staged_heavy_lease_is_durable_and_version_one_is_conservatively_adopted() {
        let coordinator = coordinator();
        let request = request();
        let mut journal = JobJournal::queued(&request, &coordinator, true).unwrap();
        assert!(!journal.heavy_lease_acquired);
        journal.mark_heavy_lease_acquired().unwrap();
        assert!(journal.heavy_lease_acquired);
        assert!(
            JobJournal::decode(&journal.encode().unwrap())
                .unwrap()
                .heavy_lease_acquired
        );

        let legacy = journal
            .encode()
            .unwrap()
            .replacen(JOURNAL_HEADER, LEGACY_JOURNAL_HEADER, 1)
            .replace("heavy_lease_acquired=1\n", "");
        let adopted = JobJournal::decode(&legacy).unwrap();
        assert!(adopted.heavy_traffic);
        assert!(adopted.heavy_lease_acquired);
    }

    #[test]
    fn prolonged_runtime_recovery_can_durably_detach_only_the_heavy_lease() {
        let mut journal = JobJournal::queued(&request(), &coordinator(), true).unwrap();
        journal.mark_heavy_lease_acquired().unwrap();
        journal.arm_runtime_mutation("8".repeat(32)).unwrap();
        journal
            .require_recovery("native-runtime-reconciliation-required")
            .unwrap();

        journal.release_heavy_lease_during_recovery().unwrap();
        assert!(!journal.heavy_lease_acquired);
        assert!(journal.heavy_traffic);
        assert!(journal.runtime_mutated);
        assert!(journal.recovery_required);
        assert_eq!(journal.state, OperationState::Recovering);
        let decoded = JobJournal::decode(&journal.encode().unwrap()).unwrap();
        assert_eq!(decoded, journal);
        assert!(journal.release_heavy_lease_during_recovery().is_err());
    }

    #[test]
    fn atomic_store_round_trips_a_queued_job() {
        let root = temp_root("roundtrip");
        cleanup(&root);
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let coordinator = coordinator();
        let store = JournalStore::open(&root, coordinator.clone()).unwrap();
        let request = request();
        let journal = JobJournal::queued(&request, &coordinator, true).unwrap();
        store.create(&request, &journal).unwrap();
        let proc_root = root.join("proc");
        fs::create_dir(&proc_root).unwrap();
        let scan = store.scan(&proc_root).unwrap();
        assert!(scan.unsafe_entries.is_empty());
        assert_eq!(scan.jobs.len(), 1);
        assert_eq!(scan.jobs[0].disposition, JournalDisposition::Queued);
        cleanup(&root);
    }

    #[test]
    fn pid_reuse_after_mutation_requires_recovery() {
        let root = temp_root("pid-reuse");
        cleanup(&root);
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let coordinator = coordinator();
        let store = JournalStore::open(&root, coordinator.clone()).unwrap();
        let request = request();
        let mut journal = JobJournal::queued(&request, &coordinator, true).unwrap();
        journal.state = OperationState::Running;
        journal.sequence = 2;
        journal.runtime_mutated = true;
        journal.process = Some(ProcessIdentity {
            pid: 42,
            process_group: 42,
            starttime_ticks: 100,
        });
        journal.worker_run_id = Some("8".repeat(32));
        store.create(&request, &journal).unwrap();
        let proc_root = root.join("proc");
        fs::create_dir_all(proc_root.join("42")).unwrap();
        fs::write(proc_root.join("42/stat"), fake_proc_stat(42, 42, 101)).unwrap();
        let scan = store.scan(&proc_root).unwrap();
        assert_eq!(
            scan.jobs[0].disposition,
            JournalDisposition::RecoveryRequired
        );
        cleanup(&root);
    }

    #[test]
    fn malformed_journal_is_retained_as_an_unsafe_entry() {
        let root = temp_root("malformed");
        cleanup(&root);
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let coordinator = coordinator();
        let store = JournalStore::open(&root, coordinator.clone()).unwrap();
        let request = request();
        let journal = JobJournal::queued(&request, &coordinator, true).unwrap();
        store.create(&request, &journal).unwrap();
        fs::write(
            root.join(JOBS_DIR).join(&journal.job_id).join(STATE_FILE),
            b"truncated\n",
        )
        .unwrap();
        let proc_root = root.join("proc");
        fs::create_dir(&proc_root).unwrap();
        let scan = store.scan(&proc_root).unwrap();
        assert!(scan.jobs.is_empty());
        assert_eq!(scan.unsafe_entries.len(), 1);
        assert!(root.join(JOBS_DIR).join(&journal.job_id).exists());
        cleanup(&root);
    }

    #[test]
    fn scanner_classifies_more_than_the_retention_limit_without_truncation() {
        let root = temp_root("complete-over-retention-scan");
        cleanup(&root);
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let coordinator = coordinator();
        let store = JournalStore::open(&root, coordinator.clone()).unwrap();
        for index in 0..=MAX_JOURNAL_JOBS {
            let mut request = request();
            request.identity.job_id = format!("{index:032x}");
            request.identity.job_token = format!("{index:064x}");
            request.created_unix_ms = index as u64 + 1;
            request.deadline_unix_ms = index as u64 + 2;
            let mut journal = JobJournal::queued(&request, &coordinator, true).unwrap();
            store.create(&request, &journal).unwrap();
            journal.state = OperationState::Cancelled;
            journal.sequence += 1;
            store.update(&journal).unwrap();
        }
        let proc_root = root.join("proc");
        fs::create_dir(&proc_root).unwrap();
        let scan = store.scan(&proc_root).unwrap();
        assert!(scan.unsafe_entries.is_empty());
        assert_eq!(scan.jobs.len(), MAX_JOURNAL_JOBS + 1);
        assert!(scan
            .jobs
            .iter()
            .all(|job| job.disposition == JournalDisposition::Settled));
        cleanup(&root);
    }

    #[test]
    fn scanner_never_hides_recovery_behind_settled_history() {
        let root = temp_root("complete-recovery-scan");
        cleanup(&root);
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let coordinator = coordinator();
        let store = JournalStore::open(&root, coordinator.clone()).unwrap();
        for index in 0..=MAX_JOURNAL_JOBS {
            let mut request = request();
            request.identity.job_id = format!("{index:032x}");
            request.identity.job_token = format!("{index:064x}");
            let mut journal = JobJournal::queued(&request, &coordinator, true).unwrap();
            store.create(&request, &journal).unwrap();
            if index == MAX_JOURNAL_JOBS {
                journal.arm_runtime_mutation("f".repeat(32)).unwrap();
                journal
                    .require_recovery("startup-reconciliation-required")
                    .unwrap();
            } else {
                journal.state = OperationState::Cancelled;
                journal.sequence += 1;
            }
            store.update(&journal).unwrap();
        }
        let proc_root = root.join("proc");
        fs::create_dir(&proc_root).unwrap();
        let scan = store.scan(&proc_root).unwrap();
        assert!(scan.unsafe_entries.is_empty());
        assert_eq!(scan.jobs.len(), MAX_JOURNAL_JOBS + 1);
        assert_eq!(
            scan.jobs
                .iter()
                .filter(|job| job.disposition == JournalDisposition::RecoveryRequired)
                .count(),
            1
        );
        cleanup(&root);
    }

    #[test]
    fn settled_retirement_is_revalidated_and_interrupted_staging_is_idempotent() {
        let root = temp_root("settled-retirement");
        cleanup(&root);
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let coordinator = coordinator();
        let store = JournalStore::open(&root, coordinator.clone()).unwrap();
        let first_request = request();
        let mut journal = JobJournal::queued(&first_request, &coordinator, true).unwrap();
        store.create(&first_request, &journal).unwrap();
        journal.state = OperationState::Cancelled;
        journal.sequence += 1;
        store.update(&journal).unwrap();
        let proc_root = root.join("proc");
        fs::create_dir(&proc_root).unwrap();
        let settled = store.scan(&proc_root).unwrap().jobs.remove(0);
        store.retire_settled_job(&settled, &proc_root).unwrap();
        assert!(!root.join(JOBS_DIR).join(&journal.job_id).exists());

        let mut second_request = request();
        second_request.identity.job_id = "a".repeat(32);
        second_request.identity.job_token = "b".repeat(64);
        let mut second = JobJournal::queued(&second_request, &coordinator, true).unwrap();
        store.create(&second_request, &second).unwrap();
        second.state = OperationState::Cancelled;
        second.sequence += 1;
        store.update(&second).unwrap();
        let original = root.join(JOBS_DIR).join(&second.job_id);
        let staged = root
            .join(JOBS_DIR)
            .join(format!("{RETIRED_JOB_PREFIX}{}", second.job_id));
        fs::rename(&original, &staged).unwrap();
        drop(store);
        let reopened = JournalStore::open(&root, coordinator).unwrap();
        assert!(!staged.exists());
        assert!(reopened.scan(&proc_root).unwrap().jobs.is_empty());
        cleanup(&root);
    }

    #[test]
    fn retirement_never_removes_a_nonterminal_or_mutated_job() {
        let root = temp_root("unsafe-retirement");
        cleanup(&root);
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let coordinator = coordinator();
        let store = JournalStore::open(&root, coordinator.clone()).unwrap();
        let request = request();
        let journal = JobJournal::queued(&request, &coordinator, true).unwrap();
        store.create(&request, &journal).unwrap();
        let proc_root = root.join("proc");
        fs::create_dir(&proc_root).unwrap();
        let queued = store.scan(&proc_root).unwrap().jobs.remove(0);
        assert!(store.retire_settled_job(&queued, &proc_root).is_err());
        assert!(root.join(JOBS_DIR).join(&journal.job_id).exists());
        cleanup(&root);
    }

    #[test]
    fn cancelled_job_without_runtime_mutation_is_settled() {
        let root = temp_root("settled");
        cleanup(&root);
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let coordinator = coordinator();
        let store = JournalStore::open(&root, coordinator.clone()).unwrap();
        let request = request();
        let mut journal = JobJournal::queued(&request, &coordinator, true).unwrap();
        store.create(&request, &journal).unwrap();
        journal.state = OperationState::Cancelled;
        journal.sequence += 1;
        store.update(&journal).unwrap();

        let proc_root = root.join("proc");
        fs::create_dir(&proc_root).unwrap();
        let scan = store.scan(&proc_root).unwrap();
        assert!(scan.unsafe_entries.is_empty());
        assert_eq!(scan.jobs.len(), 1);
        assert_eq!(scan.jobs[0].disposition, JournalDisposition::Settled);
        cleanup(&root);
    }

    #[test]
    fn historical_terminal_policy_drift_is_auditable_but_never_resumable() {
        let root = temp_root("terminal-policy-drift");
        cleanup(&root);
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let coordinator = coordinator();
        let store = JournalStore::open(&root, coordinator.clone()).unwrap();
        let valid_request = request();
        let mut journal = JobJournal::queued(&valid_request, &coordinator, true).unwrap();
        store.create(&valid_request, &journal).unwrap();
        journal.state = OperationState::Cancelled;
        journal.sequence += 1;
        store.update(&journal).unwrap();

        let mut historical_request = valid_request.clone();
        historical_request.capacity_learning_policy = Some(CapacityLearningPolicy::FixedCap);
        historical_request.service_dl_cap_kbps = None;
        historical_request.service_ul_cap_kbps = None;
        assert!(historical_request.validate().is_ok());
        assert!(historical_request.validate_admission_policy().is_err());
        let request_path = root.join(JOBS_DIR).join(&journal.job_id).join(REQUEST_FILE);
        fs::write(&request_path, historical_request.encode().unwrap()).unwrap();

        let proc_root = root.join("proc");
        fs::create_dir(&proc_root).unwrap();
        let scan = store.scan(&proc_root).unwrap();
        assert!(scan.unsafe_entries.is_empty());
        assert_eq!(scan.jobs.len(), 1);
        assert_eq!(scan.jobs[0].disposition, JournalDisposition::Settled);
        assert_eq!(scan.jobs[0].request, historical_request);

        journal.state = OperationState::Queued;
        fs::write(
            root.join(JOBS_DIR).join(&journal.job_id).join(STATE_FILE),
            journal.encode().unwrap(),
        )
        .unwrap();
        let scan = store.scan(&proc_root).unwrap();
        assert!(scan.jobs.is_empty());
        assert_eq!(scan.unsafe_entries.len(), 1);
        assert!(scan.unsafe_entries[0].contains(
            "fixed-cap capacity learning requires download and upload service hard caps"
        ));
        cleanup(&root);
    }

    #[test]
    fn dispatch_is_armed_fail_closed_before_a_worker_is_attached() {
        let mut journal = JobJournal::queued(&request(), &coordinator(), true).unwrap();
        journal.arm_runtime_mutation("8".repeat(32)).unwrap();
        assert_eq!(journal.state, OperationState::Starting);
        assert!(journal.runtime_mutated);
        assert!(journal.recovery_required);
        journal
            .attach_running_process(
                ProcessIdentity {
                    pid: 42,
                    process_group: 42,
                    starttime_ticks: 100,
                },
                "8".repeat(32),
            )
            .unwrap();
        assert_eq!(journal.state, OperationState::Running);
        assert!(journal.recovery_required);
        assert!(journal.process.is_some());
        assert!(journal
            .attach_running_process(
                ProcessIdentity {
                    pid: 43,
                    process_group: 43,
                    starttime_ticks: 101,
                },
                "9".repeat(32),
            )
            .is_err());
    }

    #[test]
    fn native_worker_is_durable_without_claiming_runtime_mutation() {
        let coordinator = coordinator();
        let mut request = request();
        request.identity.job_id = "0123456789abcdef0123456789abcdef".to_string();
        let mut journal = JobJournal::queued(&request, &coordinator, true).unwrap();
        journal.mark_heavy_lease_acquired().unwrap();
        let run_id = "abcdef0123456789abcdef0123456789".to_string();
        journal.arm_native_worker(run_id.clone()).unwrap();
        assert_eq!(journal.state, OperationState::Starting);
        assert!(!journal.runtime_mutated);
        assert!(!journal.recovery_required);

        journal
            .attach_native_running(
                ProcessIdentity {
                    pid: 123,
                    process_group: 123,
                    starttime_ticks: 456,
                },
                run_id,
            )
            .unwrap();
        assert_eq!(journal.state, OperationState::Running);
        journal.settle_native_terminal("complete", None).unwrap();
        assert_eq!(journal.state, OperationState::Completed);
        assert_eq!(journal.terminal_kind.as_deref(), Some("result"));
        assert!(!journal.heavy_lease_acquired);
        assert!(journal.process.is_none());
    }

    #[test]
    fn attached_native_worker_arms_runtime_only_immediately_before_permit() {
        let mut journal = JobJournal::queued(&request(), &coordinator(), true).unwrap();
        journal.mark_heavy_lease_acquired().unwrap();
        let run_id = "abcdef0123456789abcdef0123456789".to_string();
        journal.arm_native_worker(run_id.clone()).unwrap();
        journal
            .attach_native_running(
                ProcessIdentity {
                    pid: 123,
                    process_group: 123,
                    starttime_ticks: 456,
                },
                run_id,
            )
            .unwrap();
        let attached_sequence = journal.sequence;
        assert!(!journal.runtime_mutated);
        journal.arm_attached_runtime_mutation().unwrap();
        assert_eq!(journal.state, OperationState::Running);
        assert_eq!(journal.sequence, attached_sequence + 1);
        assert!(journal.runtime_mutated);
        assert!(journal.recovery_required);
        assert!(journal.process.is_some());
        assert!(journal.arm_attached_runtime_mutation().is_err());
    }

    #[test]
    fn fatal_parked_worker_decision_is_durable_without_false_recovery() {
        let mut journal = JobJournal::queued(&request(), &coordinator(), true).unwrap();
        journal.mark_heavy_lease_acquired().unwrap();
        let run_id = "bbcdef0123456789abcdef0123456789".to_string();
        journal.arm_native_worker(run_id.clone()).unwrap();
        journal
            .attach_native_running(
                ProcessIdentity {
                    pid: 124,
                    process_group: 124,
                    starttime_ticks: 457,
                },
                run_id,
            )
            .unwrap();
        journal
            .stop_attached_before_runtime_mutation("runtime-route-drift")
            .unwrap();
        assert_eq!(journal.state, OperationState::Cancelling);
        assert_eq!(
            journal.diagnostic_code.as_deref(),
            Some("runtime-route-drift")
        );
        assert!(!journal.runtime_mutated);
        assert!(!journal.recovery_required);
        assert!(journal.process.is_some());
    }

    #[test]
    fn native_failure_never_enters_runtime_recovery() {
        let coordinator = coordinator();
        let mut request = request();
        request.identity.job_id = "1123456789abcdef0123456789abcdef".to_string();
        let mut journal = JobJournal::queued(&request, &coordinator, false).unwrap();
        journal
            .arm_native_worker("bbcdef0123456789abcdef0123456789".to_string())
            .unwrap();
        journal
            .settle_native_terminal("failed", Some("worker-permit-timeout"))
            .unwrap();
        assert_eq!(journal.state, OperationState::Failed);
        assert!(!journal.runtime_mutated);
        assert!(!journal.recovery_required);
        assert_eq!(
            journal.diagnostic_code.as_deref(),
            Some("worker-permit-timeout")
        );
    }

    #[test]
    fn attested_legacy_terminal_clears_runtime_only_after_exact_settlement() {
        let mut journal = JobJournal::queued(&request(), &coordinator(), true).unwrap();
        journal.arm_runtime_mutation("8".repeat(32)).unwrap();
        journal
            .attach_running_process(
                ProcessIdentity {
                    pid: 42,
                    process_group: 42,
                    starttime_ticks: 100,
                },
                "8".repeat(32),
            )
            .unwrap();
        journal.require_recovery("worker-exited").unwrap();
        assert!(journal.runtime_mutated);
        assert!(journal.recovery_required);
        journal
            .record_reconciliation_failure("terminal-missing")
            .unwrap();
        journal
            .record_reconciliation_failure("terminal-missing")
            .unwrap();
        assert_eq!(journal.reconcile_attempts, 2);
        assert_eq!(
            JobJournal::decode(&journal.encode().unwrap())
                .unwrap()
                .reconcile_attempts,
            2
        );
        journal
            .settle_legacy_terminal("result", "complete")
            .unwrap();
        assert_eq!(journal.state, OperationState::ReviewReady);
        assert!(!journal.runtime_mutated);
        assert!(!journal.recovery_required);
        assert_eq!(
            journal.worker_run_id.as_deref(),
            Some("88888888888888888888888888888888")
        );
        assert_eq!(journal.terminal_kind.as_deref(), Some("result"));
    }

    #[test]
    fn native_runtime_recovery_clears_ownership_only_after_recovering() {
        let mut journal = JobJournal::queued(&request(), &coordinator(), true).unwrap();
        journal.arm_runtime_mutation("9".repeat(32)).unwrap();
        journal
            .attach_running_process(
                ProcessIdentity {
                    pid: 43,
                    process_group: 43,
                    starttime_ticks: 101,
                },
                "9".repeat(32),
            )
            .unwrap();
        assert!(journal
            .settle_runtime_recovery(false, "native-runtime-worker-exited")
            .is_err());
        journal
            .require_recovery("native-runtime-reconciliation-required")
            .unwrap();
        journal
            .settle_runtime_recovery(false, "native-runtime-worker-exited")
            .unwrap();
        assert_eq!(journal.state, OperationState::Failed);
        assert!(!journal.runtime_mutated);
        assert!(!journal.recovery_required);
        assert!(!journal.heavy_lease_acquired);
        assert_eq!(journal.terminal_kind.as_deref(), Some("error"));
        assert_eq!(journal.terminal_state.as_deref(), Some("failed"));
        assert_eq!(
            journal.diagnostic_code.as_deref(),
            Some("native-runtime-worker-exited")
        );
        assert_eq!(
            JobJournal::decode(&journal.encode().unwrap()).unwrap(),
            journal
        );
    }

    #[test]
    fn verified_native_autotune_terminal_reaches_review_only_after_restore() {
        let mut journal = JobJournal::queued(&request(), &coordinator(), true).unwrap();
        journal.mark_heavy_lease_acquired().unwrap();
        journal.arm_runtime_mutation("a".repeat(32)).unwrap();
        journal
            .attach_running_process(
                ProcessIdentity {
                    pid: 44,
                    process_group: 44,
                    starttime_ticks: 102,
                },
                "a".repeat(32),
            )
            .unwrap();
        assert!(journal
            .settle_native_runtime_terminal("complete", None)
            .is_err());
        journal
            .require_recovery("native-runtime-worker-exited")
            .unwrap();
        journal
            .settle_native_runtime_terminal("complete", None)
            .unwrap();
        assert_eq!(journal.state, OperationState::ReviewReady);
        assert_eq!(journal.terminal_kind.as_deref(), Some("result"));
        assert_eq!(journal.terminal_state.as_deref(), Some("complete"));
        assert!(!journal.runtime_mutated);
        assert!(!journal.recovery_required);
        assert!(!journal.heavy_lease_acquired);
        assert!(journal.diagnostic_code.is_none());
        assert_eq!(
            journal.publication_identity().unwrap(),
            (
                "11111111111111111111111111111111",
                "22222222222222222222222222222222"
            )
        );
        assert!(journal
            .encode()
            .unwrap()
            .starts_with(PUBLICATION_JOURNAL_HEADER));
        assert_eq!(
            JobJournal::decode(&journal.encode().unwrap()).unwrap(),
            journal
        );
    }

    #[test]
    fn verified_native_speedtest_terminal_completes_only_after_restore() {
        let mut journal = JobJournal::queued(&request(), &coordinator(), true).unwrap();
        journal.mark_heavy_lease_acquired().unwrap();
        journal.arm_runtime_mutation("c".repeat(32)).unwrap();
        journal
            .attach_running_process(
                ProcessIdentity {
                    pid: 45,
                    process_group: 45,
                    starttime_ticks: 103,
                },
                "c".repeat(32),
            )
            .unwrap();
        assert!(journal
            .settle_native_speedtest_runtime_terminal("complete", None)
            .is_err());
        journal
            .require_recovery("native-runtime-worker-exited")
            .unwrap();
        journal
            .settle_native_speedtest_runtime_terminal("complete", None)
            .unwrap();
        assert_eq!(journal.state, OperationState::Completed);
        assert_eq!(journal.terminal_kind.as_deref(), Some("result"));
        assert_eq!(journal.terminal_state.as_deref(), Some("complete"));
        assert!(!journal.runtime_mutated);
        assert!(!journal.recovery_required);
        assert!(!journal.heavy_lease_acquired);
        assert!(journal.diagnostic_code.is_none());
        assert_eq!(
            JobJournal::decode(&journal.encode().unwrap()).unwrap(),
            journal
        );
    }

    #[test]
    fn cancellation_can_bind_the_exact_worker_without_reentering_running() {
        let mut journal = JobJournal::queued(&request(), &coordinator(), true).unwrap();
        let worker_run_id = "b".repeat(32);
        journal.arm_runtime_mutation(worker_run_id.clone()).unwrap();
        journal.transition(OperationState::Cancelling).unwrap();
        let process = ProcessIdentity {
            pid: 101,
            process_group: 101,
            starttime_ticks: 303,
        };
        journal
            .attach_cancelling_process(process.clone(), worker_run_id)
            .unwrap();
        assert_eq!(journal.state, OperationState::Cancelling);
        assert_eq!(journal.process, Some(process));
        assert!(journal.runtime_mutated);
        assert!(journal.recovery_required);
    }

    #[test]
    fn current_boot_journal_is_atomically_adopted_by_a_new_generation() {
        let root = temp_root("generation-adoption");
        cleanup(&root);
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let first = coordinator();
        let first_store = JournalStore::open(&root, first.clone()).unwrap();
        let request = request();
        let journal = JobJournal::queued(&request, &first, true).unwrap();
        first_store.create(&request, &journal).unwrap();

        let mut second = first.clone();
        second.generation = "99999999999999999999999999999999".to_string();
        let second_store = JournalStore::open(&root, second.clone()).unwrap();
        let adopted = second_store.adopt_generation(&journal).unwrap();
        assert_eq!(adopted.coordinator_generation, second.generation);
        assert_eq!(adopted.sequence, journal.sequence + 1);
        second_store.update(&adopted).unwrap();
        cleanup(&root);
    }

    #[test]
    fn native_review_publication_identity_survives_generation_adoption() {
        let root = temp_root("review-generation-adoption");
        cleanup(&root);
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let first = coordinator();
        let first_store = JournalStore::open(&root, first.clone()).unwrap();
        let request = request();
        let mut journal = JobJournal::queued(&request, &first, true).unwrap();
        journal.mark_heavy_lease_acquired().unwrap();
        journal.arm_runtime_mutation("a".repeat(32)).unwrap();
        journal
            .attach_running_process(
                ProcessIdentity {
                    pid: 44,
                    process_group: 44,
                    starttime_ticks: 102,
                },
                "a".repeat(32),
            )
            .unwrap();
        journal
            .require_recovery("native-runtime-worker-exited")
            .unwrap();
        journal
            .settle_native_runtime_terminal("complete", None)
            .unwrap();
        first_store.create(&request, &journal).unwrap();

        let publication = journal.publication_identity().unwrap();
        assert_eq!(publication, (&first.boot_id[..], &first.generation[..]));
        let mut second = first.clone();
        second.generation = "99999999999999999999999999999999".to_string();
        let second_store = JournalStore::open(&root, second.clone()).unwrap();
        let adopted = second_store.adopt_generation(&journal).unwrap();
        assert_eq!(adopted.coordinator_generation, second.generation);
        assert_eq!(adopted.publication_identity().unwrap(), publication);
        assert!(adopted
            .encode()
            .unwrap()
            .starts_with(PUBLICATION_JOURNAL_HEADER));
        second_store.update(&adopted).unwrap();
        cleanup(&root);
    }
}
