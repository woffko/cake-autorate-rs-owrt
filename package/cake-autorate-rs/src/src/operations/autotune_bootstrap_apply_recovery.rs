//! Flash-durable recovery authority for absent-target Apply v7.
//!
//! It publishes both exact originals and the only accepted deterministic
//! candidate pair before the executor may enter `MutationStarted`.
//! Pre-commit states can only restore the originals;
//! `CommitAccepted` can only roll the exact candidates forward.  Any live byte
//! pair outside those two durable identities is foreign and remains untouched.

use super::autotune_apply::validate_native_apply_option_id;
use super::autotune_apply_runtime::{
    atomic_restore, create_private_directory, ensure_private_directory, parse_mode, path_exists,
    read_config_snapshot, read_field, read_private_recovery_bounded, replace_private_file,
    require_lower_hex, require_private_directory, sync_directory, validate_config_mode,
    verify_restored_file, write_new_private_file, NativeApplyConfigPairLock,
};
use super::autotune_bootstrap_apply::{
    NativeBootstrapApplyMode, NativeBootstrapApplyPlan, MAX_NATIVE_BOOTSTRAP_APPLY_MANIFEST_BYTES,
};
use super::autotune_uci_materialization::NativeUciMaterializationPlan;
use super::protocol::{OperationKind, OperationOrigin, OperationRequest, OperationTargetState};
use super::sqm_identity;
use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

const RECOVERY_HEADER: &str = "cake-autorate-native-bootstrap-apply-recovery\t6";
const RECOVERY_HEADER_V7: &str = "cake-autorate-native-bootstrap-apply-recovery\t7";
const RECOVERY_NAMESPACE_DIRECTORY: &str = "bootstrap-v6";
const CURRENT_DIRECTORY: &str = "current";
const STAGING_DIRECTORY: &str = "staging";
const COMPLETED_DIRECTORY: &str = "completed";
const STATE_FILE: &str = "state";
const NEXT_STATE_FILE: &str = "state.next";
const CAKE_BACKUP_FILE: &str = "cake-autorate.before";
const SQM_BACKUP_FILE: &str = "sqm.before";
const CAKE_CANDIDATE_FILE: &str = "cake-autorate.after";
const SQM_CANDIDATE_FILE: &str = "sqm.after";
const REQUEST_FILE: &str = "request";
const MANIFEST_FILE: &str = "apply-manifest-v6.json";
const MATERIALIZATION_FILE: &str = "uci-materialization-v1.json";
const MAX_CONFIG_BYTES: usize = 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_MATERIALIZATION_BYTES: usize = 128 * 1024;
const MAX_STATE_BYTES: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeBootstrapApplyRecoveryState {
    Prepared,
    MutationStarted,
    ServiceRestarted,
    Verified,
    CommitAccepted,
    RollbackRequired,
    Restored,
}

impl NativeBootstrapApplyRecoveryState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::MutationStarted => "mutation_started",
            Self::ServiceRestarted => "service_restarted",
            Self::Verified => "verified",
            Self::CommitAccepted => "commit_accepted",
            Self::RollbackRequired => "rollback_required",
            Self::Restored => "restored",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "prepared" => Some(Self::Prepared),
            "mutation_started" => Some(Self::MutationStarted),
            "service_restarted" => Some(Self::ServiceRestarted),
            "verified" => Some(Self::Verified),
            "commit_accepted" => Some(Self::CommitAccepted),
            "rollback_required" => Some(Self::RollbackRequired),
            "restored" => Some(Self::Restored),
            _ => None,
        }
    }

    fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Prepared, Self::MutationStarted)
                | (Self::Prepared, Self::RollbackRequired)
                | (Self::MutationStarted, Self::ServiceRestarted)
                | (Self::MutationStarted, Self::Verified)
                | (Self::MutationStarted, Self::RollbackRequired)
                | (Self::ServiceRestarted, Self::Verified)
                | (Self::ServiceRestarted, Self::RollbackRequired)
                | (Self::Verified, Self::CommitAccepted)
                | (Self::Verified, Self::RollbackRequired)
                | (Self::RollbackRequired, Self::Restored)
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeBootstrapRecoveryDirection {
    Rollback,
    RollForward,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeBootstrapApplyRecoveryRecord {
    pub(crate) state: NativeBootstrapApplyRecoveryState,
    pub(crate) mode: NativeBootstrapApplyMode,
    pub(crate) job_id: String,
    pub(crate) worker_run_id: String,
    pub(crate) option_id: String,
    pub(crate) manifest_sha256: String,
    pub(crate) request_sha256: String,
    pub(crate) composite_candidate_id: String,
    pub(crate) materialization_sha256: String,
    pub(crate) kernel_topology_fingerprint: String,
    pub(crate) kernel_namespace_seed: String,
    pub(crate) target_ifindex: u32,
    pub(crate) original_cake_sha256: String,
    pub(crate) original_sqm_sha256: String,
    pub(crate) candidate_cake_sha256: String,
    pub(crate) candidate_sqm_sha256: String,
    pub(crate) cake_mode: u32,
    pub(crate) sqm_mode: u32,
}

impl NativeBootstrapApplyRecoveryRecord {
    fn validate(&self) -> Result<(), String> {
        require_lower_hex("bootstrap recovery job ID", &self.job_id, 32)?;
        require_lower_hex("bootstrap recovery worker run ID", &self.worker_run_id, 32)?;
        validate_native_apply_option_id(&self.option_id)?;
        for (label, value) in [
            ("manifest", &self.manifest_sha256),
            ("request", &self.request_sha256),
            ("composite candidate", &self.composite_candidate_id),
            ("materialization", &self.materialization_sha256),
            ("kernel topology", &self.kernel_topology_fingerprint),
            ("original cake", &self.original_cake_sha256),
            ("original SQM", &self.original_sqm_sha256),
            ("candidate cake", &self.candidate_cake_sha256),
            ("candidate SQM", &self.candidate_sqm_sha256),
        ] {
            require_lower_hex(&format!("bootstrap recovery {label} digest"), value, 64)?;
        }
        require_lower_hex(
            "bootstrap recovery kernel namespace seed",
            &self.kernel_namespace_seed,
            32,
        )?;
        if self.target_ifindex == 0 || self.target_ifindex > i32::MAX as u32 {
            return Err("bootstrap recovery target ifindex is invalid".to_string());
        }
        validate_config_mode("bootstrap cake-autorate", self.cake_mode)?;
        validate_config_mode("bootstrap sqm", self.sqm_mode)
    }

    fn validate_request(&self, request: &OperationRequest) -> Result<(), String> {
        require_bootstrap_recovery_request(request)?;
        if self.job_id != request.identity.job_id {
            return Err("bootstrap recovery request job identity mismatch".to_string());
        }
        Ok(())
    }

    pub(crate) fn recovery_direction(&self) -> NativeBootstrapRecoveryDirection {
        if self.state == NativeBootstrapApplyRecoveryState::CommitAccepted {
            NativeBootstrapRecoveryDirection::RollForward
        } else {
            NativeBootstrapRecoveryDirection::Rollback
        }
    }

    fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let mode = if self.mode == NativeBootstrapApplyMode::DisabledInactive {
            format!("candidate_mode={}\n", self.mode.as_str())
        } else {
            String::new()
        };
        let header = if self.mode == NativeBootstrapApplyMode::DisabledInactive {
            RECOVERY_HEADER_V7
        } else {
            RECOVERY_HEADER
        };
        Ok(format!(
            concat!(
                "{}\n",
                "state={}\n",
                "{}",
                "job_id={}\n",
                "worker_run_id={}\n",
                "option_id={}\n",
                "manifest_sha256={}\n",
                "request_sha256={}\n",
                "composite_candidate_id={}\n",
                "materialization_sha256={}\n",
                "kernel_topology_fingerprint={}\n",
                "kernel_namespace_seed={}\n",
                "target_ifindex={}\n",
                "original_cake_sha256={}\n",
                "original_sqm_sha256={}\n",
                "candidate_cake_sha256={}\n",
                "candidate_sqm_sha256={}\n",
                "cake_mode={}\n",
                "sqm_mode={}\n\n"
            ),
            header,
            self.state.as_str(),
            mode,
            self.job_id,
            self.worker_run_id,
            self.option_id,
            self.manifest_sha256,
            self.request_sha256,
            self.composite_candidate_id,
            self.materialization_sha256,
            self.kernel_topology_fingerprint,
            self.kernel_namespace_seed,
            self.target_ifindex,
            self.original_cake_sha256,
            self.original_sqm_sha256,
            self.candidate_cake_sha256,
            self.candidate_sqm_sha256,
            self.cake_mode,
            self.sqm_mode,
        )
        .into_bytes())
    }

    fn decode(bytes: &[u8]) -> Result<Self, String> {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| "bootstrap Apply recovery state is not UTF-8".to_string())?;
        let mut lines = text.split('\n');
        let header = lines.next();
        if !matches!(header, Some(RECOVERY_HEADER | RECOVERY_HEADER_V7)) {
            return Err("bootstrap Apply recovery state has an unsupported header".to_string());
        }
        let state = NativeBootstrapApplyRecoveryState::parse(&read_field(&mut lines, "state")?)
            .ok_or_else(|| "bootstrap Apply recovery state is unsupported".to_string())?;
        let mode = if header == Some(RECOVERY_HEADER_V7) {
            NativeBootstrapApplyMode::parse(&read_field(&mut lines, "candidate_mode")?).ok_or_else(
                || "bootstrap Apply recovery candidate mode is unsupported".to_string(),
            )?
        } else {
            NativeBootstrapApplyMode::ShapedRuntime
        };
        let record = Self {
            state,
            mode,
            job_id: read_field(&mut lines, "job_id")?,
            worker_run_id: read_field(&mut lines, "worker_run_id")?,
            option_id: read_field(&mut lines, "option_id")?,
            manifest_sha256: read_field(&mut lines, "manifest_sha256")?,
            request_sha256: read_field(&mut lines, "request_sha256")?,
            composite_candidate_id: read_field(&mut lines, "composite_candidate_id")?,
            materialization_sha256: read_field(&mut lines, "materialization_sha256")?,
            kernel_topology_fingerprint: read_field(&mut lines, "kernel_topology_fingerprint")?,
            kernel_namespace_seed: read_field(&mut lines, "kernel_namespace_seed")?,
            target_ifindex: read_field(&mut lines, "target_ifindex")?
                .parse::<u32>()
                .map_err(|_| "bootstrap recovery target ifindex is invalid".to_string())?,
            original_cake_sha256: read_field(&mut lines, "original_cake_sha256")?,
            original_sqm_sha256: read_field(&mut lines, "original_sqm_sha256")?,
            candidate_cake_sha256: read_field(&mut lines, "candidate_cake_sha256")?,
            candidate_sqm_sha256: read_field(&mut lines, "candidate_sqm_sha256")?,
            cake_mode: parse_mode(&read_field(&mut lines, "cake_mode")?)?,
            sqm_mode: parse_mode(&read_field(&mut lines, "sqm_mode")?)?,
        };
        if lines.next() != Some("") || lines.next() != Some("") || lines.next().is_some() {
            return Err("bootstrap Apply recovery state has trailing fields".to_string());
        }
        record.validate()?;
        if record.encode()? != bytes {
            return Err("bootstrap Apply recovery state is not canonical".to_string());
        }
        Ok(record)
    }
}

pub(crate) struct NativeBootstrapApplyRecoveryStore {
    root: PathBuf,
}

/// Mutation guard which keeps the exact two configuration inodes locked from
/// the final original-byte attestation through candidate installation.  A
/// future executor receives no free-standing candidate accessor: the only
/// writer consumes the already durable bytes held by this guard.
pub(crate) struct NativeBootstrapPreparedMutation {
    _locks: NativeApplyConfigPairLock,
    recovery_root: PathBuf,
    cake_config: PathBuf,
    sqm_config: PathBuf,
    record: NativeBootstrapApplyRecoveryRecord,
    cake_candidate: Vec<u8>,
    sqm_candidate: Vec<u8>,
}

impl NativeBootstrapPreparedMutation {
    pub(crate) fn install_candidate_files(&self) -> Result<(), String> {
        let store = NativeBootstrapApplyRecoveryStore {
            root: self.recovery_root.clone(),
        };
        let live_record = store
            .read_record()?
            .ok_or_else(|| "bootstrap recovery disappeared before candidate write".to_string())?;
        if live_record != self.record
            || live_record.state != NativeBootstrapApplyRecoveryState::MutationStarted
        {
            return Err("bootstrap recovery authority changed before candidate write".to_string());
        }
        let bundle = store.read_and_verify_bundle(&live_record)?;
        if bundle.cake_candidate != self.cake_candidate
            || bundle.sqm_candidate != self.sqm_candidate
        {
            return Err("bootstrap durable candidate bytes changed before write".to_string());
        }
        let before = classify_live_pair(&self.cake_config, &self.sqm_config, &live_record)?;
        if !before
            .cake
            .matches(NativeBootstrapRecoveryDirection::Rollback)
            || !before
                .sqm
                .matches(NativeBootstrapRecoveryDirection::Rollback)
        {
            return Err(
                "bootstrap live originals changed before candidate write; recovery remains pending"
                    .to_string(),
            );
        }
        atomic_restore(
            &self.cake_config,
            &self.cake_candidate,
            live_record.cake_mode,
        )?;
        atomic_restore(&self.sqm_config, &self.sqm_candidate, live_record.sqm_mode)?;
        store.verify_live_candidate_files(&self.cake_config, &self.sqm_config)
    }
}

impl NativeBootstrapApplyRecoveryStore {
    pub(crate) fn new(root: &Path) -> Self {
        Self {
            // V4 and V6 deliberately cannot publish `current/state` into the
            // same directory.  The caller supplies their common private
            // parent; this constructor always derives the V6 namespace.
            root: root.join(RECOVERY_NAMESPACE_DIRECTORY),
        }
    }

    pub(crate) fn prepare(
        &self,
        plan: &NativeBootstrapApplyPlan,
        manifest: &[u8],
        cake_config: &Path,
        sqm_config: &Path,
    ) -> Result<NativeBootstrapApplyRecoveryRecord, String> {
        let expected_manifest = plan.canonical_manifest_bytes()?;
        if manifest != expected_manifest {
            return Err("bootstrap Apply manifest changed after plan construction".to_string());
        }
        if manifest.len() > MAX_NATIVE_BOOTSTRAP_APPLY_MANIFEST_BYTES {
            return Err("bootstrap Apply manifest exceeds its recovery bound".to_string());
        }
        ensure_private_directory(&self.root)?;
        if path_exists(&self.current_path())? {
            return Err("a bootstrap Apply recovery transaction is already pending".to_string());
        }
        if path_exists(&self.staging_path())? {
            return Err("an incomplete bootstrap recovery staging directory exists".to_string());
        }

        let (cake_original, cake_mode) =
            read_config_snapshot(cake_config, "bootstrap cake-autorate")?;
        let (sqm_original, sqm_mode) = read_config_snapshot(sqm_config, "bootstrap sqm")?;
        let materialization =
            NativeUciMaterializationPlan::from_managed_config(plan.managed_config())?;
        materialization.ensure_bound_to(plan.managed_config())?;
        let materialization_bytes = materialization.canonical_bytes()?;
        if materialization_bytes.len() > MAX_MATERIALIZATION_BYTES {
            return Err("bootstrap UCI materialization exceeds its recovery bound".to_string());
        }
        let candidate =
            materialization.deterministic_candidate_pair(&cake_original, &sqm_original)?;
        let request_bytes = plan.request().encode()?.into_bytes();
        if request_bytes.len() > MAX_REQUEST_BYTES {
            return Err("bootstrap Apply request exceeds its recovery bound".to_string());
        }
        let identity = plan.identity();
        let record = NativeBootstrapApplyRecoveryRecord {
            state: NativeBootstrapApplyRecoveryState::Prepared,
            mode: plan.mode(),
            job_id: identity.job_id.clone(),
            worker_run_id: identity.worker_run_id.clone(),
            option_id: identity.option_id.clone(),
            manifest_sha256: sqm_identity::sha256sum(manifest)?,
            request_sha256: sqm_identity::sha256sum(&request_bytes)?,
            composite_candidate_id: identity.composite_candidate_id.clone(),
            materialization_sha256: materialization.canonical_sha256()?,
            kernel_topology_fingerprint: plan.absent_baseline().kernel_topology_fingerprint.clone(),
            kernel_namespace_seed: plan.absent_baseline().kernel_namespace_seed.clone(),
            target_ifindex: plan.absent_baseline().target_ifindex,
            original_cake_sha256: sqm_identity::sha256sum(&cake_original)?,
            original_sqm_sha256: sqm_identity::sha256sum(&sqm_original)?,
            candidate_cake_sha256: sqm_identity::sha256sum(candidate.cake())?,
            candidate_sqm_sha256: sqm_identity::sha256sum(candidate.sqm())?,
            cake_mode,
            sqm_mode,
        };
        record.validate_request(plan.request())?;

        let staging = self.staging_path();
        create_private_directory(&staging)?;
        write_new_private_file(&staging.join(CAKE_BACKUP_FILE), &cake_original)?;
        write_new_private_file(&staging.join(SQM_BACKUP_FILE), &sqm_original)?;
        write_new_private_file(&staging.join(CAKE_CANDIDATE_FILE), candidate.cake())?;
        write_new_private_file(&staging.join(SQM_CANDIDATE_FILE), candidate.sqm())?;
        write_new_private_file(&staging.join(REQUEST_FILE), &request_bytes)?;
        write_new_private_file(&staging.join(MANIFEST_FILE), manifest)?;
        write_new_private_file(&staging.join(MATERIALIZATION_FILE), &materialization_bytes)?;
        write_new_private_file(&staging.join(STATE_FILE), &record.encode()?)?;
        sync_directory(&staging)?;
        fs::rename(&staging, self.current_path())
            .map_err(|error| format!("unable to publish bootstrap Apply recovery: {error}"))?;
        sync_directory(&self.root)?;

        let stored = self
            .read_record()?
            .ok_or_else(|| "published bootstrap recovery state disappeared".to_string())?;
        if stored != record {
            return Err("published bootstrap recovery state changed".to_string());
        }
        self.verify_exact_plan(plan)?;
        Ok(stored)
    }

    pub(crate) fn read_record(&self) -> Result<Option<NativeBootstrapApplyRecoveryRecord>, String> {
        let current = self.current_path();
        if !path_exists(&current)? {
            return Ok(None);
        }
        require_private_directory(&current)?;
        let bytes = read_private_recovery_bounded(
            &current.join(STATE_FILE),
            MAX_STATE_BYTES,
            "bootstrap recovery state",
        )?;
        Ok(Some(NativeBootstrapApplyRecoveryRecord::decode(&bytes)?))
    }

    pub(crate) fn read_request(&self) -> Result<OperationRequest, String> {
        let record = self
            .read_record()?
            .ok_or_else(|| "bootstrap Apply recovery transaction is missing".to_string())?;
        let bundle = self.read_and_verify_bundle(&record)?;
        let text = std::str::from_utf8(&bundle.request)
            .map_err(|_| "bootstrap recovery request is not UTF-8".to_string())?;
        let request = OperationRequest::decode(text)?;
        record.validate_request(&request)?;
        Ok(request)
    }

    pub(crate) fn verify_exact_plan(&self, plan: &NativeBootstrapApplyPlan) -> Result<(), String> {
        let record = self
            .read_record()?
            .ok_or_else(|| "bootstrap Apply recovery transaction is missing".to_string())?;
        let bundle = self.read_and_verify_bundle(&record)?;
        let request_text = std::str::from_utf8(&bundle.request)
            .map_err(|_| "bootstrap recovery request is not UTF-8".to_string())?;
        let request = OperationRequest::decode(request_text)?;
        record.validate_request(&request)?;
        if request != *plan.request()
            || bundle.manifest != plan.canonical_manifest_bytes()?
            || record.job_id != plan.identity().job_id
            || record.worker_run_id != plan.identity().worker_run_id
            || record.option_id != plan.identity().option_id
            || record.mode != plan.mode()
            || record.composite_candidate_id != plan.identity().composite_candidate_id
            || record.kernel_topology_fingerprint
                != plan.absent_baseline().kernel_topology_fingerprint
            || record.kernel_namespace_seed != plan.absent_baseline().kernel_namespace_seed
            || record.target_ifindex != plan.absent_baseline().target_ifindex
        {
            return Err("bootstrap recovery authority differs from the selected plan".to_string());
        }
        let materialization =
            NativeUciMaterializationPlan::from_managed_config(plan.managed_config())?;
        if bundle.materialization != materialization.canonical_bytes()?
            || record.materialization_sha256 != materialization.canonical_sha256()?
        {
            return Err("bootstrap recovery materialization identity changed".to_string());
        }
        materialization.verify_deterministic_candidate_files(
            &bundle.cake_original,
            &bundle.sqm_original,
            &bundle.cake_candidate,
            &bundle.sqm_candidate,
        )
    }

    pub(crate) fn transition(
        &self,
        expected: NativeBootstrapApplyRecoveryState,
        next: NativeBootstrapApplyRecoveryState,
    ) -> Result<NativeBootstrapApplyRecoveryRecord, String> {
        if !expected.can_transition_to(next) {
            return Err(format!(
                "bootstrap Apply recovery transition {} -> {} is invalid",
                expected.as_str(),
                next.as_str()
            ));
        }
        let mut record = self
            .read_record()?
            .ok_or_else(|| "bootstrap Apply recovery transaction is missing".to_string())?;
        if record.state != expected {
            return Err(format!(
                "bootstrap Apply recovery state changed from expected {} to {}",
                expected.as_str(),
                record.state.as_str()
            ));
        }
        if expected == NativeBootstrapApplyRecoveryState::MutationStarted
            && next == NativeBootstrapApplyRecoveryState::Verified
            && record.mode != NativeBootstrapApplyMode::DisabledInactive
        {
            return Err(
                "only an explicitly disabled bootstrap candidate may skip runtime restart"
                    .to_string(),
            );
        }
        if next == NativeBootstrapApplyRecoveryState::ServiceRestarted
            && record.mode != NativeBootstrapApplyMode::ShapedRuntime
        {
            return Err(
                "an explicitly disabled bootstrap candidate must not enter service-restarted state"
                    .to_string(),
            );
        }
        self.read_and_verify_bundle(&record)?;
        record.state = next;
        let current = self.current_path();
        replace_private_file(
            &current.join(STATE_FILE),
            &current.join(NEXT_STATE_FILE),
            &record.encode()?,
        )?;
        sync_directory(&current)?;
        let stored = self
            .read_record()?
            .ok_or_else(|| "bootstrap recovery state disappeared after transition".to_string())?;
        if stored != record {
            return Err("bootstrap recovery transition changed canonical data".to_string());
        }
        self.read_and_verify_bundle(&stored)?;
        Ok(stored)
    }

    pub(crate) fn verify_live_original_files(
        &self,
        cake_config: &Path,
        sqm_config: &Path,
    ) -> Result<(), String> {
        let record = self
            .read_record()?
            .ok_or_else(|| "bootstrap Apply recovery transaction is missing".to_string())?;
        self.read_and_verify_bundle(&record)?;
        let identity = classify_live_pair(cake_config, sqm_config, &record)?;
        if !identity
            .cake
            .matches(NativeBootstrapRecoveryDirection::Rollback)
            || !identity
                .sqm
                .matches(NativeBootstrapRecoveryDirection::Rollback)
        {
            return Err(
                "bootstrap live config pair differs from the durable originals".to_string(),
            );
        }
        Ok(())
    }

    pub(crate) fn verify_live_candidate_files(
        &self,
        cake_config: &Path,
        sqm_config: &Path,
    ) -> Result<(), String> {
        let record = self
            .read_record()?
            .ok_or_else(|| "bootstrap Apply recovery transaction is missing".to_string())?;
        if matches!(
            record.state,
            NativeBootstrapApplyRecoveryState::Prepared
                | NativeBootstrapApplyRecoveryState::RollbackRequired
                | NativeBootstrapApplyRecoveryState::Restored
        ) {
            return Err("bootstrap recovery state has no live candidate authority".to_string());
        }
        self.read_and_verify_bundle(&record)?;
        let identity = classify_live_pair(cake_config, sqm_config, &record)?;
        if !identity
            .cake
            .matches(NativeBootstrapRecoveryDirection::RollForward)
            || !identity
                .sqm
                .matches(NativeBootstrapRecoveryDirection::RollForward)
        {
            return Err(
                "bootstrap live config pair differs from the durable candidates".to_string(),
            );
        }
        Ok(())
    }

    /// Atomically closes the prepare-to-mutation TOCTOU window for the config
    /// pair.  Both originals are re-read under the pair lock, the durable
    /// state advances first, and only then may the returned guard install the
    /// exact staged candidates while retaining those locks.
    pub(crate) fn begin_mutation(
        &self,
        cake_config: &Path,
        sqm_config: &Path,
    ) -> Result<NativeBootstrapPreparedMutation, String> {
        let record = self
            .read_record()?
            .ok_or_else(|| "bootstrap Apply recovery transaction is missing".to_string())?;
        if record.state != NativeBootstrapApplyRecoveryState::Prepared {
            return Err("bootstrap mutation may begin only from Prepared".to_string());
        }
        let bundle = self.read_and_verify_bundle(&record)?;
        let locks = NativeApplyConfigPairLock::acquire(cake_config, sqm_config)?;
        self.verify_live_original_files(cake_config, sqm_config)?;
        let mutation_record = self.transition(
            NativeBootstrapApplyRecoveryState::Prepared,
            NativeBootstrapApplyRecoveryState::MutationStarted,
        )?;
        Ok(NativeBootstrapPreparedMutation {
            _locks: locks,
            recovery_root: self.root.clone(),
            cake_config: cake_config.to_path_buf(),
            sqm_config: sqm_config.to_path_buf(),
            record: mutation_record,
            cake_candidate: bundle.cake_candidate,
            sqm_candidate: bundle.sqm_candidate,
        })
    }

    pub(crate) fn restore_original_files(
        &self,
        cake_config: &Path,
        sqm_config: &Path,
    ) -> Result<NativeBootstrapApplyRecoveryRecord, String> {
        self.reconcile_files(
            cake_config,
            sqm_config,
            NativeBootstrapRecoveryDirection::Rollback,
            NativeBootstrapApplyRecoveryState::RollbackRequired,
            "bootstrap originals require durable rollback intent",
        )
    }

    /// Reconstruct the exact candidate pair solely to recover its immutable
    /// owner context before a rollback stop.  This is not roll-forward or
    /// commit authority: the durable state must already be RollbackRequired,
    /// and the caller must subsequently restore the original pair.
    pub(crate) fn reconstruct_candidate_files_for_rollback(
        &self,
        cake_config: &Path,
        sqm_config: &Path,
    ) -> Result<NativeBootstrapApplyRecoveryRecord, String> {
        self.reconcile_files(
            cake_config,
            sqm_config,
            NativeBootstrapRecoveryDirection::RollForward,
            NativeBootstrapApplyRecoveryState::RollbackRequired,
            "bootstrap rollback candidate reconstruction requires durable rollback intent",
        )
    }

    pub(crate) fn restore_candidate_files(
        &self,
        cake_config: &Path,
        sqm_config: &Path,
    ) -> Result<NativeBootstrapApplyRecoveryRecord, String> {
        self.reconcile_files(
            cake_config,
            sqm_config,
            NativeBootstrapRecoveryDirection::RollForward,
            NativeBootstrapApplyRecoveryState::CommitAccepted,
            "bootstrap candidates require durable commit acceptance",
        )
    }

    fn reconcile_files(
        &self,
        cake_config: &Path,
        sqm_config: &Path,
        direction: NativeBootstrapRecoveryDirection,
        expected_state: NativeBootstrapApplyRecoveryState,
        state_error: &'static str,
    ) -> Result<NativeBootstrapApplyRecoveryRecord, String> {
        let record = self
            .read_record()?
            .ok_or_else(|| "bootstrap Apply recovery transaction is missing".to_string())?;
        if record.state != expected_state {
            return Err(state_error.to_string());
        }
        let bundle = self.read_and_verify_bundle(&record)?;
        let _locks = NativeApplyConfigPairLock::acquire(cake_config, sqm_config)?;
        let before = classify_live_pair(cake_config, sqm_config, &record)?;
        if before.contains_foreign() {
            return Err(
                "bootstrap Apply recovery found foreign config bytes; refusing overwrite"
                    .to_string(),
            );
        }
        let (cake_target, sqm_target, cake_digest, sqm_digest) = match direction {
            NativeBootstrapRecoveryDirection::Rollback => (
                bundle.cake_original.as_slice(),
                bundle.sqm_original.as_slice(),
                record.original_cake_sha256.as_str(),
                record.original_sqm_sha256.as_str(),
            ),
            NativeBootstrapRecoveryDirection::RollForward => (
                bundle.cake_candidate.as_slice(),
                bundle.sqm_candidate.as_slice(),
                record.candidate_cake_sha256.as_str(),
                record.candidate_sqm_sha256.as_str(),
            ),
        };
        if !before.cake.matches(direction) {
            atomic_restore(cake_config, cake_target, record.cake_mode)?;
        }
        if !before.sqm.matches(direction) {
            atomic_restore(sqm_config, sqm_target, record.sqm_mode)?;
        }
        verify_restored_file(cake_config, cake_digest, record.cake_mode, "bootstrap cake")?;
        verify_restored_file(sqm_config, sqm_digest, record.sqm_mode, "bootstrap sqm")?;
        Ok(record)
    }

    pub(crate) fn clear_restored(&self) -> Result<(), String> {
        self.complete_current(NativeBootstrapApplyRecoveryState::Restored)
    }

    /// A prepared transaction has published recovery authority but has not
    /// crossed the persistent mutation boundary.  The executor may discard it
    /// only after re-verifying the exact original files and absent runtime.
    pub(crate) fn clear_prepared(
        &self,
        cake_config: &Path,
        sqm_config: &Path,
    ) -> Result<(), String> {
        self.verify_live_original_files(cake_config, sqm_config)?;
        self.complete_current(NativeBootstrapApplyRecoveryState::Prepared)
    }

    pub(crate) fn clear_committed(&self) -> Result<(), String> {
        self.complete_current(NativeBootstrapApplyRecoveryState::CommitAccepted)
    }

    fn complete_current(&self, expected: NativeBootstrapApplyRecoveryState) -> Result<(), String> {
        let record = self
            .read_record()?
            .ok_or_else(|| "bootstrap Apply recovery transaction is missing".to_string())?;
        if record.state != expected {
            return Err(format!(
                "bootstrap Apply recovery cannot complete from {}",
                record.state.as_str()
            ));
        }
        self.read_and_verify_bundle(&record)?;
        self.discard_completed()?;
        fs::rename(self.current_path(), self.completed_path()).map_err(|error| {
            format!("unable to atomically complete bootstrap Apply recovery: {error}")
        })?;
        sync_directory(&self.root)?;
        self.discard_completed()
    }

    pub(crate) fn discard_incomplete_staging(&self) -> Result<(), String> {
        self.remove_transaction_directory(&self.staging_path(), "incomplete staging")
    }

    pub(crate) fn discard_completed(&self) -> Result<(), String> {
        self.remove_transaction_directory(&self.completed_path(), "completed recovery")
    }

    fn remove_transaction_directory(&self, path: &Path, label: &str) -> Result<(), String> {
        if !path_exists(path)? {
            return Ok(());
        }
        require_private_directory(path)?;
        reject_unknown_entries(path)?;
        for name in known_entries() {
            match fs::remove_file(path.join(name)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "unable to remove bootstrap {label} file {name}: {error}"
                    ))
                }
            }
        }
        sync_directory(path)?;
        fs::remove_dir(path)
            .map_err(|error| format!("unable to remove bootstrap {label} directory: {error}"))?;
        sync_directory(&self.root)
    }

    fn read_and_verify_bundle(
        &self,
        record: &NativeBootstrapApplyRecoveryRecord,
    ) -> Result<RecoveryBundle, String> {
        record.validate()?;
        let current = self.current_path();
        require_private_directory(&current)?;
        reject_unknown_entries(&current)?;
        let bundle = RecoveryBundle {
            cake_original: read_private_recovery_bounded(
                &current.join(CAKE_BACKUP_FILE),
                MAX_CONFIG_BYTES,
                "bootstrap original cake",
            )?,
            sqm_original: read_private_recovery_bounded(
                &current.join(SQM_BACKUP_FILE),
                MAX_CONFIG_BYTES,
                "bootstrap original sqm",
            )?,
            cake_candidate: read_private_recovery_bounded(
                &current.join(CAKE_CANDIDATE_FILE),
                MAX_CONFIG_BYTES,
                "bootstrap candidate cake",
            )?,
            sqm_candidate: read_private_recovery_bounded(
                &current.join(SQM_CANDIDATE_FILE),
                MAX_CONFIG_BYTES,
                "bootstrap candidate sqm",
            )?,
            request: read_private_recovery_bounded(
                &current.join(REQUEST_FILE),
                MAX_REQUEST_BYTES,
                "bootstrap request",
            )?,
            manifest: read_private_recovery_bounded(
                &current.join(MANIFEST_FILE),
                MAX_NATIVE_BOOTSTRAP_APPLY_MANIFEST_BYTES,
                "bootstrap manifest",
            )?,
            materialization: read_private_recovery_bounded(
                &current.join(MATERIALIZATION_FILE),
                MAX_MATERIALIZATION_BYTES,
                "bootstrap materialization",
            )?,
        };
        for (label, bytes, expected) in [
            (
                "original cake",
                bundle.cake_original.as_slice(),
                record.original_cake_sha256.as_str(),
            ),
            (
                "original sqm",
                bundle.sqm_original.as_slice(),
                record.original_sqm_sha256.as_str(),
            ),
            (
                "candidate cake",
                bundle.cake_candidate.as_slice(),
                record.candidate_cake_sha256.as_str(),
            ),
            (
                "candidate sqm",
                bundle.sqm_candidate.as_slice(),
                record.candidate_sqm_sha256.as_str(),
            ),
            (
                "request",
                bundle.request.as_slice(),
                record.request_sha256.as_str(),
            ),
            (
                "manifest",
                bundle.manifest.as_slice(),
                record.manifest_sha256.as_str(),
            ),
            (
                "materialization",
                bundle.materialization.as_slice(),
                record.materialization_sha256.as_str(),
            ),
        ] {
            if sqm_identity::sha256sum(bytes)? != expected {
                return Err(format!("bootstrap recovery {label} digest mismatch"));
            }
        }
        Ok(bundle)
    }

    fn current_path(&self) -> PathBuf {
        self.root.join(CURRENT_DIRECTORY)
    }

    fn staging_path(&self) -> PathBuf {
        self.root.join(STAGING_DIRECTORY)
    }

    fn completed_path(&self) -> PathBuf {
        self.root.join(COMPLETED_DIRECTORY)
    }
}

struct RecoveryBundle {
    cake_original: Vec<u8>,
    sqm_original: Vec<u8>,
    cake_candidate: Vec<u8>,
    sqm_candidate: Vec<u8>,
    request: Vec<u8>,
    manifest: Vec<u8>,
    materialization: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LiveFileIdentity {
    Original,
    Candidate,
    OriginalAndCandidate,
    Foreign,
}

impl LiveFileIdentity {
    fn matches(self, direction: NativeBootstrapRecoveryDirection) -> bool {
        matches!(
            (self, direction),
            (Self::Original, NativeBootstrapRecoveryDirection::Rollback)
                | (Self::OriginalAndCandidate, _)
                | (
                    Self::Candidate,
                    NativeBootstrapRecoveryDirection::RollForward
                )
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LivePairIdentity {
    cake: LiveFileIdentity,
    sqm: LiveFileIdentity,
}

impl LivePairIdentity {
    fn contains_foreign(self) -> bool {
        self.cake == LiveFileIdentity::Foreign || self.sqm == LiveFileIdentity::Foreign
    }
}

fn classify_live_pair(
    cake_config: &Path,
    sqm_config: &Path,
    record: &NativeBootstrapApplyRecoveryRecord,
) -> Result<LivePairIdentity, String> {
    let (cake, cake_mode) = read_config_snapshot(cake_config, "bootstrap live cake")?;
    let (sqm, sqm_mode) = read_config_snapshot(sqm_config, "bootstrap live sqm")?;
    Ok(LivePairIdentity {
        cake: classify_live_file(
            &cake,
            cake_mode,
            &record.original_cake_sha256,
            &record.candidate_cake_sha256,
            record.cake_mode,
        )?,
        sqm: classify_live_file(
            &sqm,
            sqm_mode,
            &record.original_sqm_sha256,
            &record.candidate_sqm_sha256,
            record.sqm_mode,
        )?,
    })
}

fn classify_live_file(
    bytes: &[u8],
    mode: u32,
    original: &str,
    candidate: &str,
    expected_mode: u32,
) -> Result<LiveFileIdentity, String> {
    if mode != expected_mode {
        return Ok(LiveFileIdentity::Foreign);
    }
    let digest = sqm_identity::sha256sum(bytes)?;
    Ok(if digest == original && digest == candidate {
        LiveFileIdentity::OriginalAndCandidate
    } else if digest == original {
        LiveFileIdentity::Original
    } else if digest == candidate {
        LiveFileIdentity::Candidate
    } else {
        LiveFileIdentity::Foreign
    })
}

fn require_bootstrap_recovery_request(request: &OperationRequest) -> Result<(), String> {
    request.validate()?;
    if request.target_state != OperationTargetState::AbsentBootstrap
        || request.identity.operation != OperationKind::FullAutotune
        || request.origin != OperationOrigin::Luci
        || request.scheduled_auto_apply_requested
        || request.managed_sqm_section.is_none()
    {
        return Err(
            "bootstrap recovery requires a manual LuCI absent Full Auto-Tune request".to_string(),
        );
    }
    Ok(())
}

fn known_entries() -> [&'static str; 9] {
    [
        NEXT_STATE_FILE,
        STATE_FILE,
        MATERIALIZATION_FILE,
        MANIFEST_FILE,
        REQUEST_FILE,
        SQM_CANDIDATE_FILE,
        CAKE_CANDIDATE_FILE,
        SQM_BACKUP_FILE,
        CAKE_BACKUP_FILE,
    ]
}

fn reject_unknown_entries(path: &Path) -> Result<(), String> {
    let allowed = known_entries().into_iter().collect::<BTreeSet<_>>();
    for entry in fs::read_dir(path)
        .map_err(|error| format!("unable to enumerate bootstrap recovery directory: {error}"))?
    {
        let entry = entry
            .map_err(|error| format!("unable to inspect bootstrap recovery entry: {error}"))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "bootstrap recovery entry name is not UTF-8".to_string())?;
        if !allowed.contains(name.as_str()) {
            return Err(format!("unknown bootstrap recovery entry: {name}"));
        }
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| format!("unable to inspect bootstrap recovery entry: {error}"))?;
        if !metadata.file_type().is_file() || metadata.nlink() != 1 {
            return Err(format!("unsafe bootstrap recovery entry: {name}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::autotune_bootstrap_apply::tests::{
        fixture_plan, fixture_raw_fallback_plan,
    };
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST: AtomicU64 = AtomicU64::new(1);

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "cake-bootstrap-apply-recovery-{}-{}-{name}",
                std::process::id(),
                NEXT_TEST.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct Fixture {
        _root: TestRoot,
        cake: PathBuf,
        sqm: PathBuf,
        recovery: PathBuf,
        cake_original: Vec<u8>,
        sqm_original: Vec<u8>,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = TestRoot::new(name);
            let cake = root.path().join("cake-autorate");
            let sqm = root.path().join("sqm");
            let recovery = root.path().join("recovery");
            let cake_original =
                b"# keep exact bytes\nconfig globals 'globals'\n\toption enabled '1'\n".to_vec();
            let sqm_original = b"config queue 'other_queue'\n\toption interface 'eth9'".to_vec();
            write_config(&cake, &cake_original, 0o600);
            write_config(&sqm, &sqm_original, 0o600);
            Self {
                _root: root,
                cake,
                sqm,
                recovery,
                cake_original,
                sqm_original,
            }
        }

        fn store(&self) -> NativeBootstrapApplyRecoveryStore {
            NativeBootstrapApplyRecoveryStore::new(&self.recovery)
        }

        fn prepare(&self) -> (NativeBootstrapApplyPlan, NativeBootstrapApplyRecoveryRecord) {
            let plan = fixture_plan();
            let manifest = plan.canonical_manifest_bytes().unwrap();
            let record = self
                .store()
                .prepare(&plan, &manifest, &self.cake, &self.sqm)
                .unwrap();
            (plan, record)
        }
    }

    fn write_config(path: &Path, bytes: &[u8], mode: u32) {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(path)
            .unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
    }

    fn candidate_bytes(store: &NativeBootstrapApplyRecoveryStore) -> (Vec<u8>, Vec<u8>) {
        let current = store.current_path();
        (
            fs::read(current.join(CAKE_CANDIDATE_FILE)).unwrap(),
            fs::read(current.join(SQM_CANDIDATE_FILE)).unwrap(),
        )
    }

    #[test]
    fn prepare_publishes_complete_exact_v6_namespace_authority_before_mutation() {
        let fixture = Fixture::new("prepare");
        let (plan, record) = fixture.prepare();
        let store = fixture.store();
        assert_eq!(record.state, NativeBootstrapApplyRecoveryState::Prepared);
        assert_eq!(record.kernel_namespace_seed, "ee".repeat(16));
        assert_eq!(
            record.recovery_direction(),
            NativeBootstrapRecoveryDirection::Rollback
        );
        assert_eq!(store.read_request().unwrap(), *plan.request());
        store.verify_exact_plan(&plan).unwrap();
        assert!(store
            .current_path()
            .starts_with(fixture.recovery.join(RECOVERY_NAMESPACE_DIRECTORY)));
        assert!(!fixture.recovery.join(CURRENT_DIRECTORY).exists());

        let (cake_candidate, sqm_candidate) = candidate_bytes(&store);
        assert!(cake_candidate.starts_with(&fixture.cake_original));
        assert!(sqm_candidate.starts_with(&fixture.sqm_original));
        assert_eq!(fs::read(&fixture.cake).unwrap(), fixture.cake_original);
        assert_eq!(fs::read(&fixture.sqm).unwrap(), fixture.sqm_original);
        assert_eq!(record.encode().unwrap(), record.encode().unwrap());
        assert_eq!(record.encode().unwrap().len(), 1_050);
        assert_eq!(
            sqm_identity::sha256sum(&record.encode().unwrap()).unwrap(),
            "f018d2e199de68c9ae033d6fe7cdcc61404a0d18014b3c178cf5f7b2273f9b76"
        );
        assert_eq!(
            NativeBootstrapApplyRecoveryRecord::decode(&record.encode().unwrap()).unwrap(),
            record
        );
        assert!(NativeBootstrapApplyRecoveryRecord::decode(
            record
                .encode()
                .unwrap()
                .strip_prefix(RECOVERY_HEADER.as_bytes())
                .map(|tail| {
                    let mut bytes = b"cake-autorate-native-bootstrap-apply-recovery\t5".to_vec();
                    bytes.extend_from_slice(tail);
                    bytes
                })
                .unwrap()
                .as_slice()
        )
        .unwrap_err()
        .contains("unsupported header"));
    }

    #[test]
    fn disabled_candidate_uses_v7_and_keeps_the_entire_sqm_file_identical() {
        let fixture = Fixture::new("disabled-v7");
        let plan = fixture_raw_fallback_plan();
        let store = fixture.store();
        let record = store
            .prepare(
                &plan,
                &plan.canonical_manifest_bytes().unwrap(),
                &fixture.cake,
                &fixture.sqm,
            )
            .unwrap();
        let encoded = record.encode().unwrap();
        let (_, sqm_candidate) = candidate_bytes(&store);

        assert_eq!(record.mode, NativeBootstrapApplyMode::DisabledInactive);
        assert!(encoded.starts_with(format!("{RECOVERY_HEADER_V7}\n").as_bytes()));
        assert!(String::from_utf8(encoded.clone())
            .unwrap()
            .contains("candidate_mode=disabled_inactive\n"));
        assert_eq!(
            NativeBootstrapApplyRecoveryRecord::decode(&encoded).unwrap(),
            record
        );
        assert_eq!(sqm_candidate, fixture.sqm_original);
        assert_eq!(record.candidate_sqm_sha256, record.original_sqm_sha256);

        store
            .transition(
                NativeBootstrapApplyRecoveryState::Prepared,
                NativeBootstrapApplyRecoveryState::MutationStarted,
            )
            .unwrap();
        assert!(store
            .transition(
                NativeBootstrapApplyRecoveryState::MutationStarted,
                NativeBootstrapApplyRecoveryState::ServiceRestarted,
            )
            .unwrap_err()
            .contains("must not enter service-restarted"));
        store
            .transition(
                NativeBootstrapApplyRecoveryState::MutationStarted,
                NativeBootstrapApplyRecoveryState::Verified,
            )
            .unwrap();
    }

    #[test]
    fn prepare_rejects_existing_target_without_publishing_authority() {
        let fixture = Fixture::new("target-exists");
        let plan = fixture_plan();
        fs::write(
            &fixture.cake,
            format!(
                "{}config cake_autorate 'wan_sqm'\n\toption enabled '1'\n",
                String::from_utf8(fixture.cake_original.clone()).unwrap()
            ),
        )
        .unwrap();
        let error = fixture
            .store()
            .prepare(
                &plan,
                &plan.canonical_manifest_bytes().unwrap(),
                &fixture.cake,
                &fixture.sqm,
            )
            .unwrap_err();
        assert!(error.contains("already exists"));
        assert!(!fixture.store().current_path().exists());
    }

    #[test]
    fn precommit_partial_candidate_is_rolled_back_exactly() {
        let fixture = Fixture::new("rollback-partial");
        let (_, _) = fixture.prepare();
        let store = fixture.store();
        let (cake_candidate, _) = candidate_bytes(&store);
        store
            .transition(
                NativeBootstrapApplyRecoveryState::Prepared,
                NativeBootstrapApplyRecoveryState::MutationStarted,
            )
            .unwrap();
        store
            .transition(
                NativeBootstrapApplyRecoveryState::MutationStarted,
                NativeBootstrapApplyRecoveryState::RollbackRequired,
            )
            .unwrap();
        fs::write(&fixture.cake, cake_candidate).unwrap();

        store
            .restore_original_files(&fixture.cake, &fixture.sqm)
            .unwrap();
        assert_eq!(fs::read(&fixture.cake).unwrap(), fixture.cake_original);
        assert_eq!(fs::read(&fixture.sqm).unwrap(), fixture.sqm_original);
        store
            .transition(
                NativeBootstrapApplyRecoveryState::RollbackRequired,
                NativeBootstrapApplyRecoveryState::Restored,
            )
            .unwrap();
        store.clear_restored().unwrap();
        assert!(store.read_record().unwrap().is_none());
    }

    #[test]
    fn rollback_candidate_context_is_reconstructed_only_after_durable_rollback_intent() {
        let fixture = Fixture::new("rollback-candidate-context");
        fixture.prepare();
        let store = fixture.store();
        let (cake_candidate, sqm_candidate) = candidate_bytes(&store);
        assert!(store
            .reconstruct_candidate_files_for_rollback(&fixture.cake, &fixture.sqm)
            .unwrap_err()
            .contains("requires durable rollback intent"));
        store
            .transition(
                NativeBootstrapApplyRecoveryState::Prepared,
                NativeBootstrapApplyRecoveryState::MutationStarted,
            )
            .unwrap();
        store
            .transition(
                NativeBootstrapApplyRecoveryState::MutationStarted,
                NativeBootstrapApplyRecoveryState::RollbackRequired,
            )
            .unwrap();
        fs::write(&fixture.cake, &cake_candidate).unwrap();

        store
            .reconstruct_candidate_files_for_rollback(&fixture.cake, &fixture.sqm)
            .unwrap();
        assert_eq!(fs::read(&fixture.cake).unwrap(), cake_candidate);
        assert_eq!(fs::read(&fixture.sqm).unwrap(), sqm_candidate);

        store
            .restore_original_files(&fixture.cake, &fixture.sqm)
            .unwrap();
        assert_eq!(fs::read(&fixture.cake).unwrap(), fixture.cake_original);
        assert_eq!(fs::read(&fixture.sqm).unwrap(), fixture.sqm_original);
    }

    #[test]
    fn begin_mutation_rechecks_originals_and_installs_only_durable_candidates() {
        let fixture = Fixture::new("begin-mutation");
        fixture.prepare();
        let store = fixture.store();
        let (cake_candidate, sqm_candidate) = candidate_bytes(&store);
        let guard = store.begin_mutation(&fixture.cake, &fixture.sqm).unwrap();
        assert_eq!(
            store.read_record().unwrap().unwrap().state,
            NativeBootstrapApplyRecoveryState::MutationStarted
        );
        guard.install_candidate_files().unwrap();
        assert_eq!(fs::read(&fixture.cake).unwrap(), cake_candidate);
        assert_eq!(fs::read(&fixture.sqm).unwrap(), sqm_candidate);
        store
            .verify_live_candidate_files(&fixture.cake, &fixture.sqm)
            .unwrap();

        let fixture = Fixture::new("begin-mutation-drift");
        fixture.prepare();
        let store = fixture.store();
        fs::write(&fixture.cake, b"concurrent foreign edit\n").unwrap();
        let error = match store.begin_mutation(&fixture.cake, &fixture.sqm) {
            Ok(_) => panic!("drifted originals unexpectedly entered mutation"),
            Err(error) => error,
        };
        assert!(error.contains("differs from the durable originals"));
        assert_eq!(
            store.read_record().unwrap().unwrap().state,
            NativeBootstrapApplyRecoveryState::Prepared
        );
        assert_eq!(
            fs::read(&fixture.cake).unwrap(),
            b"concurrent foreign edit\n"
        );
        assert_eq!(fs::read(&fixture.sqm).unwrap(), fixture.sqm_original);
    }

    #[test]
    fn commit_accepted_partial_candidate_is_rolled_forward_exactly() {
        let fixture = Fixture::new("roll-forward-partial");
        let (_, record) = fixture.prepare();
        let store = fixture.store();
        let (cake_candidate, sqm_candidate) = candidate_bytes(&store);
        let mut state = NativeBootstrapApplyRecoveryState::Prepared;
        for next in [
            NativeBootstrapApplyRecoveryState::MutationStarted,
            NativeBootstrapApplyRecoveryState::ServiceRestarted,
            NativeBootstrapApplyRecoveryState::Verified,
            NativeBootstrapApplyRecoveryState::CommitAccepted,
        ] {
            store.transition(state, next).unwrap();
            state = next;
        }
        fs::write(&fixture.cake, &cake_candidate).unwrap();
        assert_eq!(
            store.read_record().unwrap().unwrap().recovery_direction(),
            NativeBootstrapRecoveryDirection::RollForward
        );

        store
            .restore_candidate_files(&fixture.cake, &fixture.sqm)
            .unwrap();
        assert_eq!(fs::read(&fixture.cake).unwrap(), cake_candidate);
        assert_eq!(fs::read(&fixture.sqm).unwrap(), sqm_candidate);
        assert_ne!(record.candidate_cake_sha256, record.original_cake_sha256);
        store.clear_committed().unwrap();
        assert!(store.read_record().unwrap().is_none());
    }

    #[test]
    fn foreign_live_bytes_fail_closed_before_any_pair_write() {
        let fixture = Fixture::new("foreign");
        fixture.prepare();
        let store = fixture.store();
        let (_, sqm_candidate) = candidate_bytes(&store);
        store
            .transition(
                NativeBootstrapApplyRecoveryState::Prepared,
                NativeBootstrapApplyRecoveryState::RollbackRequired,
            )
            .unwrap();
        fs::write(&fixture.cake, b"foreign operator bytes\n").unwrap();
        fs::write(&fixture.sqm, &sqm_candidate).unwrap();
        let cake_before = fs::read(&fixture.cake).unwrap();
        let sqm_before = fs::read(&fixture.sqm).unwrap();
        assert!(store
            .restore_original_files(&fixture.cake, &fixture.sqm)
            .unwrap_err()
            .contains("foreign config bytes"));
        assert_eq!(fs::read(&fixture.cake).unwrap(), cake_before);
        assert_eq!(fs::read(&fixture.sqm).unwrap(), sqm_before);
    }

    #[test]
    fn sidecar_or_record_tamper_is_rejected_before_transition() {
        let fixture = Fixture::new("tamper");
        fixture.prepare();
        let store = fixture.store();
        fs::write(
            store.current_path().join(MATERIALIZATION_FILE),
            b"foreign\n",
        )
        .unwrap();
        assert!(store
            .transition(
                NativeBootstrapApplyRecoveryState::Prepared,
                NativeBootstrapApplyRecoveryState::MutationStarted,
            )
            .unwrap_err()
            .contains("materialization digest mismatch"));

        let fixture = Fixture::new("unknown-entry");
        fixture.prepare();
        let store = fixture.store();
        fs::write(store.current_path().join("unexpected"), b"x").unwrap();
        assert!(store
            .verify_exact_plan(&fixture_plan())
            .unwrap_err()
            .contains("unknown bootstrap recovery entry"));

        let fixture = Fixture::new("namespace-seed");
        let (plan, mut record) = fixture.prepare();
        let store = fixture.store();
        record.kernel_namespace_seed = "ff".repeat(16);
        fs::write(
            store.current_path().join(STATE_FILE),
            record.encode().unwrap(),
        )
        .unwrap();
        assert!(store
            .verify_exact_plan(&plan)
            .unwrap_err()
            .contains("selected plan"));
    }

    #[test]
    fn only_commit_accepted_is_roll_forward_authority() {
        let fixture = Fixture::new("direction-matrix");
        let (_, mut record) = fixture.prepare();
        for state in [
            NativeBootstrapApplyRecoveryState::Prepared,
            NativeBootstrapApplyRecoveryState::MutationStarted,
            NativeBootstrapApplyRecoveryState::ServiceRestarted,
            NativeBootstrapApplyRecoveryState::Verified,
            NativeBootstrapApplyRecoveryState::RollbackRequired,
            NativeBootstrapApplyRecoveryState::Restored,
        ] {
            record.state = state;
            assert_eq!(
                record.recovery_direction(),
                NativeBootstrapRecoveryDirection::Rollback
            );
        }
        record.state = NativeBootstrapApplyRecoveryState::CommitAccepted;
        assert_eq!(
            record.recovery_direction(),
            NativeBootstrapRecoveryDirection::RollForward
        );
    }
}
