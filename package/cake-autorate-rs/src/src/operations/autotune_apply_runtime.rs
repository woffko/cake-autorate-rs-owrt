//! Flash-durable recovery substrate for transactional native Apply.
//!
//! This module deliberately does not invoke UCI, restart services, or touch
//! qdiscs.  It publishes one complete, root-only recovery transaction before
//! the future executor performs its first persistent mutation, provides strict
//! state transitions, and restores both configuration files idempotently.

use super::autotune_apply::{
    validate_native_apply_option_id, validate_native_uci_mutations, NativeApplyExecutionPlan,
    NativeUciMutationAction, MAX_NATIVE_APPLY_UCI_MUTATIONS,
};
use super::protocol::{OperationRequest, OperationTargetState};
use super::sqm_identity;
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const RECOVERY_HEADER_V4: &str = "cake-autorate-native-apply-recovery\t4";
const RECOVERY_HEADER_V5: &str = "cake-autorate-native-apply-recovery\t5";
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
const MANIFEST_FILE: &str = "apply-manifest.json";
const MATERIALIZATION_FILE: &str = "uci-materialization-v1.batch";
const MAX_CONFIG_BYTES: usize = 1024 * 1024;
const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_STATE_BYTES: usize = 8 * 1024;
const MAX_LAB_FAULT_PAUSE: Duration = Duration::from_secs(60);

#[derive(Debug)]
enum NativeApplyGlobalLockAcquireError {
    Busy,
    Fatal(String),
}

impl NativeApplyGlobalLockAcquireError {
    fn into_string(self) -> String {
        match self {
            Self::Busy => "native Apply global runtime lock is busy".to_string(),
            Self::Fatal(error) => error,
        }
    }
}

#[derive(Debug)]
pub(crate) struct NativeApplyGlobalLock {
    file: File,
}

pub(super) struct NativeApplyConfigPairLock {
    _cake: File,
    _sqm: File,
}

impl NativeApplyConfigPairLock {
    pub(super) fn acquire(cake_config: &Path, sqm_config: &Path) -> Result<Self, String> {
        let cake = lock_config_file(cake_config, "cake-autorate")?;
        let sqm = lock_config_file(sqm_config, "sqm")?;
        Ok(Self {
            _cake: cake,
            _sqm: sqm,
        })
    }
}

fn lock_config_file(path: &Path, label: &str) -> Result<File, String> {
    let before = strict_regular_metadata(path, label)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("unable to open {label} config for rollback locking: {error}"))?;
    let opened = file
        .metadata()
        .map_err(|error| format!("unable to inspect locked {label} config: {error}"))?;
    if !opened.file_type().is_file() || opened.dev() != before.dev() || opened.ino() != before.ino()
    {
        return Err(format!(
            "{label} config identity changed while acquiring rollback lock"
        ));
    }
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } < 0 {
        let error = std::io::Error::last_os_error();
        return if error
            .raw_os_error()
            .is_some_and(|code| code == libc::EAGAIN || code == libc::EWOULDBLOCK)
        {
            Err(format!(
                "{label} config is busy with another UCI writer; rollback remains pending"
            ))
        } else {
            Err(format!(
                "unable to lock {label} config for rollback: {error}"
            ))
        };
    }
    let live = strict_regular_metadata(path, label)?;
    if live.dev() != opened.dev() || live.ino() != opened.ino() {
        return Err(format!(
            "{label} config was replaced while acquiring rollback lock"
        ));
    }
    Ok(file)
}

impl NativeApplyGlobalLock {
    pub(crate) fn acquire(path: &Path) -> Result<Self, String> {
        Self::try_acquire(path).map_err(NativeApplyGlobalLockAcquireError::into_string)
    }

    pub(super) fn acquire_for_recovery(path: &Path) -> Result<Self, String> {
        let lock =
            Self::open_lock_file(path).map_err(NativeApplyGlobalLockAcquireError::into_string)?;
        loop {
            let result = unsafe { libc::flock(lock.file.as_raw_fd(), libc::LOCK_EX) };
            if result == 0 {
                return Ok(lock);
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(format!(
                    "unable to acquire native Apply recovery runtime lock: {error}"
                ));
            }
        }
    }

    fn try_acquire(path: &Path) -> Result<Self, NativeApplyGlobalLockAcquireError> {
        let lock = Self::open_lock_file(path)?;
        let result = unsafe { libc::flock(lock.file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            return if error.kind() == std::io::ErrorKind::WouldBlock {
                Err(NativeApplyGlobalLockAcquireError::Busy)
            } else {
                Err(NativeApplyGlobalLockAcquireError::Fatal(format!(
                    "unable to acquire native Apply global runtime lock: {error}"
                )))
            };
        }
        Ok(lock)
    }

    fn open_lock_file(path: &Path) -> Result<Self, NativeApplyGlobalLockAcquireError> {
        let parent = path.parent().ok_or_else(|| {
            NativeApplyGlobalLockAcquireError::Fatal(
                "native Apply global lock has no parent".to_string(),
            )
        })?;
        ensure_private_directory(parent).map_err(|error| {
            NativeApplyGlobalLockAcquireError::Fatal(format!(
                "unable to secure native Apply lock directory: {error}"
            ))
        })?;
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|error| {
                NativeApplyGlobalLockAcquireError::Fatal(format!(
                    "unable to open native Apply global lock: {error}"
                ))
            })?;
        let metadata = file.metadata().map_err(|error| {
            NativeApplyGlobalLockAcquireError::Fatal(format!(
                "unable to inspect native Apply global lock: {error}"
            ))
        })?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.uid() != effective_uid()
            || metadata.permissions().mode() & 0o777 != 0o600
        {
            return Err(NativeApplyGlobalLockAcquireError::Fatal(
                "native Apply global lock is not a single-linked regular file".to_string(),
            ));
        }
        Ok(Self { file })
    }

    /// Configure one direct init-script command to borrow the exact locked
    /// open-file description as FD 8.  The main cake-autorate init validates
    /// this descriptor and never performs a second flock in borrow mode.
    pub(crate) fn configure_borrowed_restart(&self, command: &mut Command) {
        let source_fd = self.file.as_raw_fd();
        command
            .env("CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_FD", "8")
            .env("CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_MODE", "exclusive")
            .env("CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_BORROW", "1");
        unsafe {
            command.pre_exec(move || install_inherited_lock_fd(source_fd));
        }
    }
}

fn install_inherited_lock_fd(source_fd: RawFd) -> std::io::Result<()> {
    if source_fd != 8 && unsafe { libc::dup2(source_fd, 8) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let flags = unsafe { libc::fcntl(8, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(8, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn canonical_native_uci_batch(
    plan: &NativeApplyExecutionPlan,
) -> Result<Vec<u8>, String> {
    require_existing_v4_apply_plan(plan)?;
    canonical_native_uci_batch_from_mutations(&plan.uci_mutations)
}

fn canonical_native_uci_batch_from_mutations(
    mutations: &[super::autotune_apply::NativeUciMutation],
) -> Result<Vec<u8>, String> {
    if mutations.is_empty() || mutations.len() > MAX_NATIVE_APPLY_UCI_MUTATIONS {
        return Err("native Apply UCI mutation count is outside its bound".to_string());
    }
    validate_native_uci_mutations(mutations)?;
    let mut output = String::new();
    let mut option_keys = BTreeSet::new();
    let mut packages = BTreeSet::new();
    for mutation in mutations {
        mutation.validate()?;
        packages.insert(mutation.package);
        let key = (
            mutation.package,
            mutation.section.as_str(),
            mutation.option.as_str(),
        );
        if !option_keys.insert(key) {
            return Err("native Apply UCI batch contains a duplicate option".to_string());
        }
    }

    for mutation in mutations {
        match (&mutation.action, mutation.value.as_deref()) {
            (NativeUciMutationAction::Set, Some(value)) => {
                output.push_str("set ");
                output.push_str(mutation.package);
                output.push('.');
                output.push_str(&mutation.section);
                output.push('.');
                output.push_str(&mutation.option);
                output.push_str("='");
                output.push_str(value);
                output.push_str("'\n");
            }
            (NativeUciMutationAction::Delete, None) => {
                output.push_str("delete ");
                output.push_str(mutation.package);
                output.push('.');
                output.push_str(&mutation.section);
                output.push('.');
                output.push_str(&mutation.option);
                output.push('\n');
            }
            _ => return Err("native Apply UCI action/value pair is inconsistent".to_string()),
        }
    }
    for package in packages {
        output.push_str("commit ");
        output.push_str(package);
        output.push('\n');
    }
    if output.len() > 128 * 1024 {
        return Err("native Apply UCI batch exceeds its size bound".to_string());
    }
    Ok(output.into_bytes())
}

pub(crate) fn legacy_native_apply_materialization(
    manifest: &[u8],
    request: &OperationRequest,
) -> Result<(Vec<super::autotune_apply::NativeUciMutation>, Vec<u8>), String> {
    if manifest.is_empty() || manifest.len() > MAX_MANIFEST_BYTES {
        return Err("legacy native Apply manifest is outside its size bound".to_string());
    }
    let marker = b"\"uci_mutations\":[";
    if manifest
        .windows(marker.len())
        .filter(|window| *window == marker)
        .count()
        != 1
    {
        return Err("legacy native Apply manifest lacks one canonical mutation array".to_string());
    }
    let managed_sqm_section = request
        .managed_sqm_section
        .as_deref()
        .ok_or_else(|| "legacy native Apply request has no managed SQM section".to_string())?;
    for (key, value) in [
        ("job_id", request.identity.job_id.as_str()),
        ("instance", request.identity.instance.as_str()),
        (
            "target_interface",
            request.identity.target_interface.as_str(),
        ),
        ("managed_sqm_section", managed_sqm_section),
        (
            "route_fingerprint",
            request.identity.route_fingerprint.as_str(),
        ),
        (
            "config_fingerprint",
            request.identity.config_fingerprint.as_str(),
        ),
        ("sqm_fingerprint", request.identity.sqm_fingerprint.as_str()),
    ] {
        require_canonical_manifest_string_field(manifest, key, value)?;
    }
    let start = manifest
        .windows(marker.len())
        .position(|window| window == marker)
        .ok_or_else(|| "legacy native Apply mutation array disappeared".to_string())?
        + marker.len()
        - 1;
    let mut cursor = CanonicalMutationCursor {
        input: manifest,
        position: start,
    };
    let mutations = cursor.parse_array()?;
    if cursor.remaining() != b"}}\n" {
        return Err("legacy native Apply manifest mutation suffix is not canonical".to_string());
    }
    if mutations
        .iter()
        .any(|mutation| mutation.section != request.identity.instance)
    {
        return Err(
            "legacy native Apply mutations are not bound to the recovered instance".to_string(),
        );
    }
    validate_native_uci_mutations(&mutations)?;
    let authority = canonical_native_uci_batch_from_mutations(&mutations)?;
    Ok((mutations, authority))
}

fn require_canonical_manifest_string_field(
    manifest: &[u8],
    key: &str,
    value: &str,
) -> Result<(), String> {
    let expected = format!(
        "\"{}\":\"{}\"",
        super::json_wire::json_escape(key),
        super::json_wire::json_escape(value),
    );
    if manifest
        .windows(expected.len())
        .filter(|window| *window == expected.as_bytes())
        .count()
        == 1
    {
        Ok(())
    } else {
        Err(format!(
            "legacy native Apply manifest {key} binding is not exact"
        ))
    }
}

struct CanonicalMutationCursor<'a> {
    input: &'a [u8],
    position: usize,
}

impl CanonicalMutationCursor<'_> {
    fn parse_array(&mut self) -> Result<Vec<super::autotune_apply::NativeUciMutation>, String> {
        self.expect(b"[")?;
        let mut mutations = Vec::new();
        loop {
            if self.consume(b"]") {
                break;
            }
            if !mutations.is_empty() {
                self.expect(b",")?;
            }
            self.expect(b"{\"action\":")?;
            let action = self.parse_string()?;
            self.expect(b",\"package\":")?;
            let package = self.parse_string()?;
            self.expect(b",\"section\":")?;
            let section = self.parse_string()?;
            self.expect(b",\"option\":")?;
            let option = self.parse_string()?;
            self.expect(b",\"value\":")?;
            let value = if self.consume(b"null") {
                None
            } else {
                Some(self.parse_string()?)
            };
            self.expect(b"}")?;
            let package = match package.as_str() {
                "cake-autorate" => "cake-autorate",
                "sqm" => "sqm",
                _ => {
                    return Err(
                        "legacy native Apply mutation package is outside its boundary".to_string(),
                    )
                }
            };
            let mut mutation = match (action.as_str(), value) {
                ("set", Some(value)) => {
                    super::autotune_apply::NativeUciMutation::set(&section, option, value)?
                }
                ("delete", None) => {
                    super::autotune_apply::NativeUciMutation::delete(&section, option)?
                }
                _ => {
                    return Err(
                        "legacy native Apply mutation action/value is inconsistent".to_string()
                    )
                }
            };
            mutation.package = package;
            mutations.push(mutation);
            if mutations.len() > MAX_NATIVE_APPLY_UCI_MUTATIONS {
                return Err("legacy native Apply mutation array exceeds its bound".to_string());
            }
        }
        if mutations.is_empty() {
            return Err("legacy native Apply mutation array is empty".to_string());
        }
        Ok(mutations)
    }

    fn parse_string(&mut self) -> Result<String, String> {
        self.expect(b"\"")?;
        let mut output = String::new();
        while self.position < self.input.len() {
            let byte = self.input[self.position];
            self.position += 1;
            match byte {
                b'"' => return Ok(output),
                b'\\' => {
                    let escaped = *self.input.get(self.position).ok_or_else(|| {
                        "legacy native Apply JSON escape is truncated".to_string()
                    })?;
                    self.position += 1;
                    match escaped {
                        b'"' => output.push('"'),
                        b'\\' => output.push('\\'),
                        b'n' => output.push('\n'),
                        b'r' => output.push('\r'),
                        b't' => output.push('\t'),
                        b'u' => {
                            let digits = self
                                .input
                                .get(self.position..self.position + 4)
                                .ok_or_else(|| {
                                    "legacy native Apply Unicode escape is truncated".to_string()
                                })?;
                            let text = std::str::from_utf8(digits).map_err(|_| {
                                "legacy native Apply Unicode escape is invalid".to_string()
                            })?;
                            let value = u32::from_str_radix(text, 16).map_err(|_| {
                                "legacy native Apply Unicode escape is invalid".to_string()
                            })?;
                            let character = char::from_u32(value).ok_or_else(|| {
                                "legacy native Apply Unicode escape is invalid".to_string()
                            })?;
                            output.push(character);
                            self.position += 4;
                        }
                        _ => {
                            return Err("legacy native Apply JSON escape is unsupported".to_string())
                        }
                    }
                }
                value if value < 0x20 => {
                    return Err(
                        "legacy native Apply JSON string contains a control byte".to_string()
                    )
                }
                value if value.is_ascii() => output.push(value as char),
                _ => {
                    let start = self.position - 1;
                    let tail = std::str::from_utf8(&self.input[start..])
                        .map_err(|_| "legacy native Apply JSON string is not UTF-8".to_string())?;
                    let character = tail.chars().next().ok_or_else(|| {
                        "legacy native Apply JSON string is truncated".to_string()
                    })?;
                    output.push(character);
                    self.position = start + character.len_utf8();
                }
            }
            if output.len() > 4096 {
                return Err("legacy native Apply JSON string exceeds its bound".to_string());
            }
        }
        Err("legacy native Apply JSON string is unterminated".to_string())
    }

    fn expect(&mut self, expected: &[u8]) -> Result<(), String> {
        if self.consume(expected) {
            Ok(())
        } else {
            Err("legacy native Apply mutation JSON is not canonical".to_string())
        }
    }

    fn consume(&mut self, expected: &[u8]) -> bool {
        if self
            .input
            .get(self.position..self.position + expected.len())
            == Some(expected)
        {
            self.position += expected.len();
            true
        } else {
            false
        }
    }

    fn remaining(&self) -> &[u8] {
        &self.input[self.position..]
    }
}

fn require_existing_v4_apply_plan(plan: &NativeApplyExecutionPlan) -> Result<(), String> {
    require_existing_v4_recovery_request(&plan.request)
}

fn require_existing_v4_recovery_request(request: &OperationRequest) -> Result<(), String> {
    if request.target_state != OperationTargetState::ExistingManaged {
        return Err(
            "native Apply schema-v4 runtime cannot execute absent-bootstrap authority".to_string(),
        );
    }
    Ok(())
}

pub(crate) struct NativeApplyTransactionPaths<'a> {
    pub(crate) recovery_root: &'a Path,
    pub(crate) global_lock: &'a Path,
    pub(crate) cake_config: &'a Path,
    pub(crate) sqm_config: &'a Path,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeApplyLabReceipt {
    pub(crate) job_id: String,
    pub(crate) worker_run_id: String,
    pub(crate) manifest_sha256: String,
    pub(crate) apply_verified: bool,
    pub(crate) rollback_verified: bool,
    pub(crate) recovery_cleared: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeApplyCommitDisposition {
    Applied,
    AlreadyApplied,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeApplyCommitReceipt {
    pub(crate) job_id: String,
    pub(crate) worker_run_id: String,
    pub(crate) manifest_sha256: String,
    pub(crate) disposition: NativeApplyCommitDisposition,
    pub(crate) recovery_cleared: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeApplyLabFaultInjection {
    None,
    PauseAfterServiceRestarted { timeout: Duration },
    PauseAfterVerifiedBeforeCommit { timeout: Duration },
    PauseAfterCommitAccepted { timeout: Duration },
}

impl NativeApplyLabFaultInjection {
    fn after_service_restarted(self, record: &NativeApplyRecoveryRecord) -> Result<(), String> {
        match self {
            Self::None => Ok(()),
            Self::PauseAfterServiceRestarted { timeout } => {
                if record.state != NativeApplyRecoveryState::ServiceRestarted {
                    return Err(
                        "native Apply lab fault gate was reached outside service_restarted"
                            .to_string(),
                    );
                }
                if timeout.is_zero() || timeout > MAX_LAB_FAULT_PAUSE {
                    return Err("native Apply lab fault pause is outside its bound".to_string());
                }
                // The returned transition record was already atomically
                // renamed, fsynced and byte-reverified by the recovery store.
                // A lab harness can therefore poll that durable state and
                // SIGKILL this exact process while the candidate is live.
                std::thread::sleep(timeout);
                Err(
                    "native Apply lab crash-after-service-restarted gate timed out without SIGKILL"
                        .to_string(),
                )
            }
            Self::PauseAfterVerifiedBeforeCommit { .. } | Self::PauseAfterCommitAccepted { .. } => {
                Ok(())
            }
        }
    }

    fn after_verified_before_commit(
        self,
        record: &NativeApplyRecoveryRecord,
    ) -> Result<(), String> {
        match self {
            Self::PauseAfterVerifiedBeforeCommit { timeout } => self.pause_at_state(
                record,
                NativeApplyRecoveryState::Verified,
                timeout,
                "verified-before-commit",
            ),
            _ => Ok(()),
        }
    }

    fn after_commit_accepted(self, record: &NativeApplyRecoveryRecord) -> Result<(), String> {
        match self {
            Self::PauseAfterCommitAccepted { timeout } => self.pause_at_state(
                record,
                NativeApplyRecoveryState::CommitAccepted,
                timeout,
                "commit-accepted",
            ),
            _ => Ok(()),
        }
    }

    fn pause_at_state(
        self,
        record: &NativeApplyRecoveryRecord,
        expected: NativeApplyRecoveryState,
        timeout: Duration,
        label: &str,
    ) -> Result<(), String> {
        if record.state != expected {
            return Err(format!(
                "native Apply lab {label} gate reached the wrong state"
            ));
        }
        if timeout.is_zero() || timeout > MAX_LAB_FAULT_PAUSE {
            return Err("native Apply lab fault pause is outside its bound".to_string());
        }
        std::thread::sleep(timeout);
        Err(format!(
            "native Apply lab {label} gate timed out without SIGKILL"
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeApplyRecoveryReceipt {
    pub(crate) job_id: String,
    pub(crate) worker_run_id: String,
    pub(crate) recovery_cleared: bool,
    pub(crate) rolled_forward: bool,
}

pub(crate) trait NativeApplyTransactionBackend {
    fn candidate_already_applied(
        &mut self,
        plan: &NativeApplyExecutionPlan,
    ) -> Result<bool, String>;
    fn attest_before_apply(&mut self, plan: &NativeApplyExecutionPlan) -> Result<(), String>;
    fn materialize_candidate(
        &mut self,
        plan: &NativeApplyExecutionPlan,
        original: &NativeApplyConfigPairSnapshot,
    ) -> Result<NativeApplyCandidateMaterialization, String>;
    fn reconstruct_legacy_candidate(
        &mut self,
        request: &OperationRequest,
        manifest: &[u8],
        original: &NativeApplyConfigPairSnapshot,
    ) -> Result<NativeApplyCandidateMaterialization, String>;
    fn restart_service(
        &mut self,
        request: &OperationRequest,
        lock: &NativeApplyGlobalLock,
    ) -> Result<(), String>;
    fn verify_applied(&mut self, plan: &NativeApplyExecutionPlan) -> Result<(), String>;
    fn discard_pending_uci_changes(&mut self) -> Result<(), String>;
    fn emergency_contain(
        &mut self,
        request: &OperationRequest,
        lock: &NativeApplyGlobalLock,
        authorities: &[NativeApplyConfigPairSnapshot],
    ) -> Result<(), String>;
    fn verify_restored(
        &mut self,
        request: &OperationRequest,
        record: &NativeApplyRecoveryRecord,
    ) -> Result<(), String>;
    fn verify_recovered_candidate(
        &mut self,
        request: &OperationRequest,
        record: &NativeApplyRecoveryRecord,
    ) -> Result<(), String>;
}

fn emergency_contain_native_apply<B: NativeApplyTransactionBackend>(
    backend: &mut B,
    request: &OperationRequest,
    store: &NativeApplyRecoveryStore,
    lock: &NativeApplyGlobalLock,
) -> Result<(), String> {
    let authorities = store.read_containment_snapshots()?;
    backend.emergency_contain(request, lock, &authorities)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeApplyConfigPairSnapshot {
    cake: Vec<u8>,
    sqm: Vec<u8>,
    cake_mode: u32,
    sqm_mode: u32,
}

impl NativeApplyConfigPairSnapshot {
    fn capture(cake_config: &Path, sqm_config: &Path) -> Result<Self, String> {
        let (cake, cake_mode) = read_config_snapshot(cake_config, "cake-autorate")?;
        let (sqm, sqm_mode) = read_config_snapshot(sqm_config, "sqm")?;
        Ok(Self {
            cake,
            sqm,
            cake_mode,
            sqm_mode,
        })
    }

    pub(crate) fn cake(&self) -> &[u8] {
        &self.cake
    }

    pub(crate) fn sqm(&self) -> &[u8] {
        &self.sqm
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeApplyCandidateMaterialization {
    cake: Vec<u8>,
    sqm: Vec<u8>,
    authority: Vec<u8>,
}

impl NativeApplyCandidateMaterialization {
    pub(crate) fn new(cake: Vec<u8>, sqm: Vec<u8>, authority: Vec<u8>) -> Result<Self, String> {
        if cake.len() > MAX_CONFIG_BYTES || sqm.len() > MAX_CONFIG_BYTES {
            return Err("native Apply materialized candidate exceeds its config bound".to_string());
        }
        if authority.is_empty() || authority.len() > MAX_MANIFEST_BYTES {
            return Err(
                "native Apply materialization authority is outside its size bound".to_string(),
            );
        }
        Ok(Self {
            cake,
            sqm,
            authority,
        })
    }

    pub(crate) fn cake(&self) -> &[u8] {
        &self.cake
    }

    pub(crate) fn sqm(&self) -> &[u8] {
        &self.sqm
    }

    pub(crate) fn authority(&self) -> &[u8] {
        &self.authority
    }
}

pub(crate) fn execute_native_apply_commit<B: NativeApplyTransactionBackend>(
    plan: &NativeApplyExecutionPlan,
    manifest: &[u8],
    paths: NativeApplyTransactionPaths<'_>,
    backend: &mut B,
) -> Result<NativeApplyCommitReceipt, String> {
    execute_native_apply_commit_with_fault(
        plan,
        manifest,
        paths,
        backend,
        NativeApplyLabFaultInjection::None,
    )
}

pub(crate) fn execute_native_apply_commit_with_fault<B: NativeApplyTransactionBackend>(
    plan: &NativeApplyExecutionPlan,
    manifest: &[u8],
    paths: NativeApplyTransactionPaths<'_>,
    backend: &mut B,
    fault: NativeApplyLabFaultInjection,
) -> Result<NativeApplyCommitReceipt, String> {
    require_existing_v4_apply_plan(plan)?;
    let expected_manifest = plan.canonical_manifest_bytes()?;
    if manifest != expected_manifest {
        return Err("native Apply manifest changed after plan construction".to_string());
    }
    let manifest_sha256 = sqm_identity::sha256sum(manifest)?;
    let lock = NativeApplyGlobalLock::acquire(paths.global_lock)?;
    let store = NativeApplyRecoveryStore::new(paths.recovery_root);
    store.discard_incomplete_staging()?;
    store.discard_completed()?;
    if let Some(record) = store.read_record()? {
        let request = store.read_request()?;
        if record.job_id != plan.request.identity.job_id
            || record.worker_run_id != plan.worker_run_id
            || record.option_id != plan.option_id
            || record.manifest_sha256 != manifest_sha256
            || request != plan.request
        {
            return Err(
                "a foreign native Apply recovery transaction is already pending".to_string(),
            );
        }
        if record.state == NativeApplyRecoveryState::CommitAccepted {
            if let Err(recovery_error) = roll_forward_native_apply(
                &request,
                Some(plan),
                &store,
                &lock,
                paths.cake_config,
                paths.sqm_config,
                backend,
            ) {
                let containment = emergency_contain_native_apply(backend, &request, &store, &lock);
                return Err(format!(
                    "native Apply commit recovery remains pending: {recovery_error}; {}",
                    containment.map_or_else(
                        |error| format!("emergency containment also failed: {error}"),
                        |_| "experimental qdiscs were emergency-contained".to_string(),
                    )
                ));
            }
            return Ok(NativeApplyCommitReceipt {
                job_id: record.job_id,
                worker_run_id: record.worker_run_id,
                manifest_sha256,
                disposition: NativeApplyCommitDisposition::AlreadyApplied,
                recovery_cleared: store.read_record()?.is_none(),
            });
        }
        if let Err(recovery_error) = rollback_native_apply(
            &request,
            &store,
            &lock,
            paths.cake_config,
            paths.sqm_config,
            backend,
        ) {
            let containment = emergency_contain_native_apply(backend, &request, &store, &lock);
            return Err(format!(
                "native Apply precommit recovery remains pending: {recovery_error}; {}",
                containment.map_or_else(
                    |error| format!("emergency containment also failed: {error}"),
                    |_| "experimental qdiscs were emergency-contained".to_string(),
                )
            ));
        }
    }
    if backend.candidate_already_applied(plan)? {
        return Ok(NativeApplyCommitReceipt {
            job_id: plan.request.identity.job_id.clone(),
            worker_run_id: plan.worker_run_id.clone(),
            manifest_sha256,
            disposition: NativeApplyCommitDisposition::AlreadyApplied,
            recovery_cleared: true,
        });
    }

    backend.attest_before_apply(plan)?;
    let original = NativeApplyConfigPairSnapshot::capture(paths.cake_config, paths.sqm_config)?;
    let candidate = backend.materialize_candidate(plan, &original)?;
    let prepared = store.prepare_write_ahead(plan, manifest, &original, &candidate)?;
    let precommit_result = (|| {
        prepared.validate_candidate_phase(&plan.request)?;
        backend.attest_before_apply(plan)?;
        store.verify_live_original_files(paths.cake_config, paths.sqm_config, &prepared)?;
        let mutation = store.begin_mutation(paths.cake_config, paths.sqm_config)?;
        mutation.install_candidate_files()?;
        backend.restart_service(&plan.request, &lock)?;
        let restarted = store.transition(
            NativeApplyRecoveryState::MutationStarted,
            NativeApplyRecoveryState::ServiceRestarted,
        )?;
        drop(mutation);
        store.verify_live_write_ahead_candidate_files(
            paths.cake_config,
            paths.sqm_config,
            &restarted,
        )?;
        backend.verify_applied(plan)?;
        let verified = store.transition(
            NativeApplyRecoveryState::ServiceRestarted,
            NativeApplyRecoveryState::Verified,
        )?;
        fault.after_verified_before_commit(&verified)?;
        // The first exact verification above is the commit predicate.  Publish
        // CommitAccepted immediately after it so an unrelated transport or
        // process failure cannot strand an already verified candidate on the
        // rollback side of the boundary.  The post-commit verification below
        // remains mandatory and any failure rolls forward from durable intent.
        store.verify_live_write_ahead_candidate_files(
            paths.cake_config,
            paths.sqm_config,
            &verified,
        )?;
        let accepted = store.transition(
            NativeApplyRecoveryState::Verified,
            NativeApplyRecoveryState::CommitAccepted,
        )?;
        fault.after_commit_accepted(&accepted)?;
        Ok::<NativeApplyRecoveryRecord, String>(accepted)
    })();

    let accepted = match precommit_result {
        Ok(record) => record,
        Err(apply_error) => {
            return match store.read_record() {
                Ok(Some(record)) if record.state == NativeApplyRecoveryState::CommitAccepted => {
                    Err(format!(
                        "native Apply commit was durably accepted but completion failed: {apply_error}; roll-forward recovery is required"
                    ))
                }
                Ok(Some(_)) => match rollback_native_apply(
                    &plan.request,
                    &store,
                    &lock,
                    paths.cake_config,
                    paths.sqm_config,
                    backend,
                ) {
                    Ok(()) => Err(format!(
                        "native Apply failed before commit acceptance: {apply_error}; exact rollback verified"
                    )),
                    Err(rollback_error) => {
                        let containment = emergency_contain_native_apply(
                            backend,
                            &plan.request,
                            &store,
                            &lock,
                        );
                        Err(format!(
                            "native Apply failed before commit acceptance: {apply_error}; rollback remains pending: {rollback_error}; {}",
                            containment.map_or_else(
                                |error| format!("emergency containment also failed: {error}"),
                                |_| "experimental qdiscs were emergency-contained".to_string(),
                            )
                        ))
                    }
                },
                Ok(None) => Err(format!(
                    "native Apply failed and its durable recovery transaction disappeared: {apply_error}"
                )),
                Err(state_error) => Err(format!(
                    "native Apply failed and its commit boundary cannot be classified safely: {apply_error}; {state_error}"
                )),
            };
        }
    };

    if accepted.state != NativeApplyRecoveryState::CommitAccepted {
        return Err("native Apply candidate was not durably commit-accepted".to_string());
    }
    if let Err(error) = store
        .verify_live_candidate_files(paths.cake_config, paths.sqm_config, &accepted)
        .and_then(|_| backend.verify_applied(plan))
        .and_then(|_| store.clear_committed())
    {
        return Err(format!(
            "native Apply commit was accepted but final attestation/cleanup failed: {error}; roll-forward recovery is required"
        ));
    }
    Ok(NativeApplyCommitReceipt {
        job_id: prepared.job_id,
        worker_run_id: prepared.worker_run_id,
        manifest_sha256: prepared.manifest_sha256,
        disposition: NativeApplyCommitDisposition::Applied,
        recovery_cleared: store.read_record()?.is_none(),
    })
}

/// Execute the first hardware-gate mode.  Even a fully verified Apply is
/// deliberately rolled back; this function has no commit-success branch.
#[cfg(test)]
pub(crate) fn execute_native_apply_forced_rollback<B: NativeApplyTransactionBackend>(
    plan: &NativeApplyExecutionPlan,
    manifest: &[u8],
    paths: NativeApplyTransactionPaths<'_>,
    backend: &mut B,
) -> Result<NativeApplyLabReceipt, String> {
    execute_native_apply_forced_rollback_with_fault(
        plan,
        manifest,
        paths,
        backend,
        NativeApplyLabFaultInjection::None,
    )
}

pub(crate) fn execute_native_apply_forced_rollback_with_fault<B: NativeApplyTransactionBackend>(
    plan: &NativeApplyExecutionPlan,
    manifest: &[u8],
    paths: NativeApplyTransactionPaths<'_>,
    backend: &mut B,
    fault: NativeApplyLabFaultInjection,
) -> Result<NativeApplyLabReceipt, String> {
    require_existing_v4_apply_plan(plan)?;
    let lock = NativeApplyGlobalLock::acquire(paths.global_lock)?;
    backend.attest_before_apply(plan)?;
    let store = NativeApplyRecoveryStore::new(paths.recovery_root);
    store.discard_incomplete_staging()?;
    let original = NativeApplyConfigPairSnapshot::capture(paths.cake_config, paths.sqm_config)?;
    let candidate = backend.materialize_candidate(plan, &original)?;
    let prepared = store.prepare_write_ahead(plan, manifest, &original, &candidate)?;

    let apply_result = (|| {
        prepared.validate_candidate_phase(&plan.request)?;
        backend.attest_before_apply(plan)?;
        store.verify_live_original_files(paths.cake_config, paths.sqm_config, &prepared)?;
        let mutation = store.begin_mutation(paths.cake_config, paths.sqm_config)?;
        mutation.install_candidate_files()?;
        backend.restart_service(&plan.request, &lock)?;
        let restarted = store.transition(
            NativeApplyRecoveryState::MutationStarted,
            NativeApplyRecoveryState::ServiceRestarted,
        )?;
        drop(mutation);
        store.verify_live_write_ahead_candidate_files(
            paths.cake_config,
            paths.sqm_config,
            &restarted,
        )?;
        fault.after_service_restarted(&restarted)?;
        backend.verify_applied(plan)?;
        store.transition(
            NativeApplyRecoveryState::ServiceRestarted,
            NativeApplyRecoveryState::Verified,
        )?;
        Ok::<(), String>(())
    })();

    let rollback_result = rollback_native_apply(
        &plan.request,
        &store,
        &lock,
        paths.cake_config,
        paths.sqm_config,
        backend,
    );
    let containment_error = rollback_result
        .as_ref()
        .err()
        .and_then(|_| emergency_contain_native_apply(backend, &plan.request, &store, &lock).err());
    match (apply_result, rollback_result) {
        (Ok(()), Ok(())) => Ok(NativeApplyLabReceipt {
            job_id: prepared.job_id,
            worker_run_id: prepared.worker_run_id,
            manifest_sha256: prepared.manifest_sha256,
            apply_verified: true,
            rollback_verified: true,
            recovery_cleared: store.read_record()?.is_none(),
        }),
        (Err(apply_error), Ok(())) => Err(format!(
            "native Apply lab transaction failed before acceptance: {apply_error}; exact rollback verified"
        )),
        (Ok(()), Err(rollback_error)) => Err(format!(
            "native Apply lab transaction verified its candidate but mandatory rollback failed: {rollback_error}; {}",
            containment_error.map_or_else(
                || "experimental qdiscs were emergency-contained; durable recovery remains pending".to_string(),
                |error| format!("emergency containment also failed: {error}; immediate operator recovery is required")
            )
        )),
        (Err(apply_error), Err(rollback_error)) => Err(format!(
            "native Apply lab transaction failed: {apply_error}; rollback remains pending: {rollback_error}; {}",
            containment_error.map_or_else(
                || "experimental qdiscs were emergency-contained".to_string(),
                |error| format!("emergency containment also failed: {error}; immediate operator recovery is required")
            )
        )),
    }
}

fn rollback_native_apply<B: NativeApplyTransactionBackend>(
    request: &OperationRequest,
    store: &NativeApplyRecoveryStore,
    lock: &NativeApplyGlobalLock,
    cake_config: &Path,
    sqm_config: &Path,
    backend: &mut B,
) -> Result<(), String> {
    let mut record = store.read_record()?.ok_or_else(|| {
        "native Apply recovery transaction disappeared before rollback".to_string()
    })?;
    record.validate_candidate_phase(request)?;
    match record.state {
        NativeApplyRecoveryState::Prepared => {
            store.clear_prepared()?;
            return Ok(());
        }
        NativeApplyRecoveryState::MutationStarted
        | NativeApplyRecoveryState::ServiceRestarted
        | NativeApplyRecoveryState::Verified => {
            record = store.transition(record.state, NativeApplyRecoveryState::RollbackRequired)?;
        }
        NativeApplyRecoveryState::RollbackRequired => {}
        NativeApplyRecoveryState::CommitAccepted => {
            return Err(
                "commit-accepted native Apply transaction cannot cross back into rollback"
                    .to_string(),
            )
        }
        NativeApplyRecoveryState::Restored => {
            backend.verify_restored(request, &record)?;
            store.verify_live_original_files(cake_config, sqm_config, &record)?;
            store.clear_restored()?;
            return Ok(());
        }
    }
    if record.schema_version == 4 && record.candidate_cake_sha256.is_none() {
        let original = store.read_original_snapshot()?;
        let manifest = store.read_manifest()?;
        let candidate = backend.reconstruct_legacy_candidate(request, &manifest, &original)?;
        record = store.upgrade_legacy_rollback_candidate(request, &candidate)?;
    }
    let restored = store.restore_config_files(cake_config, sqm_config)?;
    if restored != record {
        return Err("native Apply recovery identity changed before restart".to_string());
    }
    backend.discard_pending_uci_changes()?;
    backend.restart_service(request, lock)?;
    backend.verify_restored(request, &record)?;
    store.verify_live_original_files(cake_config, sqm_config, &record)?;
    store.transition(
        NativeApplyRecoveryState::RollbackRequired,
        NativeApplyRecoveryState::Restored,
    )?;
    store.clear_restored()
}

fn roll_forward_native_apply<B: NativeApplyTransactionBackend>(
    request: &OperationRequest,
    plan: Option<&NativeApplyExecutionPlan>,
    store: &NativeApplyRecoveryStore,
    lock: &NativeApplyGlobalLock,
    cake_config: &Path,
    sqm_config: &Path,
    backend: &mut B,
) -> Result<(), String> {
    if let Some(plan) = plan {
        let record = store
            .read_record()?
            .ok_or_else(|| "native Apply recovery transaction is missing".to_string())?;
        if record.option_id != plan.option_id {
            return Err(
                "native Apply recovery option ID differs from the selected plan".to_string(),
            );
        }
    }
    let phase_record = store
        .read_record()?
        .ok_or_else(|| "native Apply recovery transaction is missing".to_string())?;
    phase_record.validate_candidate_phase(request)?;
    let record = store.restore_candidate_files(cake_config, sqm_config)?;
    backend.discard_pending_uci_changes()?;
    backend.restart_service(request, lock)?;
    store.verify_live_candidate_files(cake_config, sqm_config, &record)?;
    backend.verify_recovered_candidate(request, &record)?;
    if let Some(plan) = plan {
        backend.verify_applied(plan)?;
    }
    store.clear_committed()
}

pub(crate) fn recover_native_apply<B: NativeApplyTransactionBackend>(
    paths: NativeApplyTransactionPaths<'_>,
    backend: &mut B,
) -> Result<Option<NativeApplyRecoveryReceipt>, String> {
    let store = NativeApplyRecoveryStore::new(paths.recovery_root);
    // A published schema-v4 transaction is readable without taking ownership.
    // Decode and classify it first so a malformed or absent-bootstrap request
    // cannot create/wait on the global lock, call a backend, or clean recovery
    // directories. The same bytes are re-read under the lock below; this first
    // observation is rejection authority only, never mutation authority.
    if store.read_record()?.is_some() {
        let request = store.read_request()?;
        require_existing_v4_recovery_request(&request)?;
    }

    // Recovery waits on the kernel lock-owner event itself.  No elapsed delay,
    // retry budget or polling cadence can authorize recovery mutation.
    let lock = NativeApplyGlobalLock::acquire_for_recovery(paths.global_lock)?;
    let Some(initial_record) = store.read_record()? else {
        // With no published native transaction there is no recovery authority.
        // Only private, non-authoritative staging debris may be removed.
        store.discard_incomplete_staging()?;
        store.discard_completed()?;
        return Ok(None);
    };
    let initial_request = store.read_request()?;
    require_existing_v4_recovery_request(&initial_request)?;
    store.discard_incomplete_staging()?;
    store.discard_completed()?;
    let record = store
        .read_record()?
        .ok_or_else(|| "native Apply recovery transaction disappeared while locked".to_string())?;
    if record != initial_record {
        return Err("native Apply recovery transaction changed while locked".to_string());
    }
    let request = store.read_request()?;
    require_existing_v4_recovery_request(&request)?;
    if request != initial_request {
        return Err("native Apply recovery request changed while locked".to_string());
    }
    if request.identity.job_id != record.job_id {
        return Err("native Apply recovery request identity mismatch".to_string());
    }
    record.validate_candidate_phase(&request)?;
    let rolled_forward = record.state == NativeApplyRecoveryState::CommitAccepted;
    let recovery_result = if rolled_forward {
        roll_forward_native_apply(
            &request,
            None,
            &store,
            &lock,
            paths.cake_config,
            paths.sqm_config,
            backend,
        )
    } else {
        rollback_native_apply(
            &request,
            &store,
            &lock,
            paths.cake_config,
            paths.sqm_config,
            backend,
        )
    };
    if let Err(recovery_error) = recovery_result {
        let containment = emergency_contain_native_apply(backend, &request, &store, &lock);
        return Err(format!(
            "native Apply recovery remains pending: {recovery_error}; {}",
            containment.map_or_else(
                |error| format!("emergency containment also failed: {error}"),
                |_| "experimental qdiscs were emergency-contained".to_string(),
            )
        ));
    }
    Ok(Some(NativeApplyRecoveryReceipt {
        job_id: record.job_id,
        worker_run_id: record.worker_run_id,
        recovery_cleared: store.read_record()?.is_none(),
        rolled_forward,
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeApplyRecoveryState {
    Prepared,
    MutationStarted,
    ServiceRestarted,
    Verified,
    CommitAccepted,
    RollbackRequired,
    Restored,
}

impl NativeApplyRecoveryState {
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
                | (Self::MutationStarted, Self::RollbackRequired)
                | (Self::ServiceRestarted, Self::Verified)
                | (Self::ServiceRestarted, Self::RollbackRequired)
                | (Self::Verified, Self::CommitAccepted)
                | (Self::Verified, Self::RollbackRequired)
                | (Self::RollbackRequired, Self::Restored)
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeApplyRecoveryRecord {
    schema_version: u8,
    pub(crate) state: NativeApplyRecoveryState,
    pub(crate) job_id: String,
    pub(crate) worker_run_id: String,
    pub(crate) option_id: String,
    pub(crate) manifest_sha256: String,
    pub(crate) request_sha256: String,
    pub(crate) cake_sha256: String,
    pub(crate) sqm_sha256: String,
    pub(crate) cake_mode: u32,
    pub(crate) sqm_mode: u32,
    pub(crate) candidate_cake_sha256: Option<String>,
    pub(crate) candidate_sqm_sha256: Option<String>,
    pub(crate) candidate_cake_mode: Option<u32>,
    pub(crate) candidate_sqm_mode: Option<u32>,
    materialization_sha256: Option<String>,
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeApplyCandidateSnapshot {
    cake_sha256: String,
    sqm_sha256: String,
    cake_mode: u32,
    sqm_mode: u32,
}

impl NativeApplyRecoveryRecord {
    fn validate(&self) -> Result<(), String> {
        if !matches!(self.schema_version, 4 | 5) {
            return Err("native Apply recovery schema version is unsupported".to_string());
        }
        require_lower_hex("recovery job ID", &self.job_id, 32)?;
        require_lower_hex("recovery worker run ID", &self.worker_run_id, 32)?;
        validate_native_apply_option_id(&self.option_id)?;
        require_lower_hex("recovery manifest digest", &self.manifest_sha256, 64)?;
        require_lower_hex("recovery request digest", &self.request_sha256, 64)?;
        require_lower_hex("recovery cake config digest", &self.cake_sha256, 64)?;
        require_lower_hex("recovery SQM config digest", &self.sqm_sha256, 64)?;
        validate_config_mode("cake-autorate", self.cake_mode)?;
        validate_config_mode("sqm", self.sqm_mode)?;
        let candidate_fields = (
            self.candidate_cake_sha256.as_deref(),
            self.candidate_sqm_sha256.as_deref(),
            self.candidate_cake_mode,
            self.candidate_sqm_mode,
        );
        match candidate_fields {
            (Some(cake_digest), Some(sqm_digest), Some(cake_mode), Some(sqm_mode)) => {
                require_lower_hex("candidate cake config digest", cake_digest, 64)?;
                require_lower_hex("candidate SQM config digest", sqm_digest, 64)?;
                validate_config_mode("candidate cake-autorate", cake_mode)?;
                validate_config_mode("candidate sqm", sqm_mode)?;
            }
            (None, None, None, None) => {
                if self.state == NativeApplyRecoveryState::CommitAccepted {
                    return Err(
                        "commit-accepted native Apply state lacks candidate snapshots".to_string(),
                    );
                }
            }
            _ => return Err("native Apply candidate evidence is incomplete".to_string()),
        }
        match (self.schema_version, self.materialization_sha256.as_deref()) {
            (4, None) => {}
            (5, Some(digest)) => {
                require_lower_hex("native Apply materialization digest", digest, 64)?;
                if self.candidate_cake_sha256.is_none() {
                    return Err(
                        "write-ahead native Apply recovery lacks candidate snapshots".to_string(),
                    );
                }
                if self.candidate_cake_mode != Some(self.cake_mode)
                    || self.candidate_sqm_mode != Some(self.sqm_mode)
                {
                    return Err(
                        "write-ahead native Apply candidate modes differ from the originals"
                            .to_string(),
                    );
                }
            }
            (4, Some(_)) => {
                return Err(
                    "legacy native Apply recovery unexpectedly has materialization evidence"
                        .to_string(),
                )
            }
            (5, None) => {
                return Err(
                    "write-ahead native Apply recovery lacks materialization evidence".to_string(),
                )
            }
            _ => unreachable!(),
        }
        Ok(())
    }

    fn validate_candidate_phase(&self, request: &OperationRequest) -> Result<(), String> {
        request.validate()?;
        require_existing_v4_recovery_request(request)?;
        let candidate_present = self.candidate_cake_sha256.is_some();
        if self.schema_version == 4
            && candidate_present
            && matches!(
                self.state,
                NativeApplyRecoveryState::Prepared | NativeApplyRecoveryState::MutationStarted
            )
        {
            return Err(
                "existing-managed candidate evidence precedes durable service restart".to_string(),
            );
        }
        Ok(())
    }

    fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        if self.schema_version == 4 {
            return Ok(format!(
                concat!(
                    "{}\n",
                    "state={}\n",
                    "job_id={}\n",
                    "worker_run_id={}\n",
                    "option_id={}\n",
                    "manifest_sha256={}\n",
                    "request_sha256={}\n",
                    "cake_sha256={}\n",
                    "sqm_sha256={}\n",
                    "cake_mode={}\n",
                    "sqm_mode={}\n",
                    "candidate_cake_sha256={}\n",
                    "candidate_sqm_sha256={}\n",
                    "candidate_cake_mode={}\n",
                    "candidate_sqm_mode={}\n\n"
                ),
                RECOVERY_HEADER_V4,
                self.state.as_str(),
                self.job_id,
                self.worker_run_id,
                self.option_id,
                self.manifest_sha256,
                self.request_sha256,
                self.cake_sha256,
                self.sqm_sha256,
                self.cake_mode,
                self.sqm_mode,
                self.candidate_cake_sha256.as_deref().unwrap_or("none"),
                self.candidate_sqm_sha256.as_deref().unwrap_or("none"),
                self.candidate_cake_mode
                    .map_or_else(|| "none".to_string(), |value| value.to_string()),
                self.candidate_sqm_mode
                    .map_or_else(|| "none".to_string(), |value| value.to_string()),
            )
            .into_bytes());
        }
        Ok(format!(
            concat!(
                "{}\n",
                "state={}\n",
                "job_id={}\n",
                "worker_run_id={}\n",
                "option_id={}\n",
                "manifest_sha256={}\n",
                "request_sha256={}\n",
                "materialization_sha256={}\n",
                "cake_sha256={}\n",
                "sqm_sha256={}\n",
                "cake_mode={}\n",
                "sqm_mode={}\n",
                "candidate_cake_sha256={}\n",
                "candidate_sqm_sha256={}\n",
                "candidate_cake_mode={}\n",
                "candidate_sqm_mode={}\n\n"
            ),
            RECOVERY_HEADER_V5,
            self.state.as_str(),
            self.job_id,
            self.worker_run_id,
            self.option_id,
            self.manifest_sha256,
            self.request_sha256,
            self.materialization_sha256
                .as_deref()
                .ok_or_else(|| "native Apply materialization digest is missing".to_string())?,
            self.cake_sha256,
            self.sqm_sha256,
            self.cake_mode,
            self.sqm_mode,
            self.candidate_cake_sha256.as_deref().unwrap_or("none"),
            self.candidate_sqm_sha256.as_deref().unwrap_or("none"),
            self.candidate_cake_mode
                .map_or_else(|| "none".to_string(), |value| value.to_string()),
            self.candidate_sqm_mode
                .map_or_else(|| "none".to_string(), |value| value.to_string()),
        )
        .into_bytes())
    }

    fn decode(bytes: &[u8]) -> Result<Self, String> {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| "native Apply recovery state is not UTF-8".to_string())?;
        let mut lines = text.split('\n');
        let header = lines.next();
        let schema_version = match header {
            Some(RECOVERY_HEADER_V4) => 4,
            Some(RECOVERY_HEADER_V5) => 5,
            _ => return Err("native Apply recovery state has an unsupported header".to_string()),
        };
        let state = NativeApplyRecoveryState::parse(&read_field(&mut lines, "state")?)
            .ok_or_else(|| "native Apply recovery state is unsupported".to_string())?;
        let record = Self {
            schema_version,
            state,
            job_id: read_field(&mut lines, "job_id")?,
            worker_run_id: read_field(&mut lines, "worker_run_id")?,
            option_id: read_field(&mut lines, "option_id")?,
            manifest_sha256: read_field(&mut lines, "manifest_sha256")?,
            request_sha256: read_field(&mut lines, "request_sha256")?,
            materialization_sha256: if schema_version == 5 {
                Some(read_field(&mut lines, "materialization_sha256")?)
            } else {
                None
            },
            cake_sha256: read_field(&mut lines, "cake_sha256")?,
            sqm_sha256: read_field(&mut lines, "sqm_sha256")?,
            cake_mode: parse_mode(&read_field(&mut lines, "cake_mode")?)?,
            sqm_mode: parse_mode(&read_field(&mut lines, "sqm_mode")?)?,
            candidate_cake_sha256: parse_optional_digest(&read_field(
                &mut lines,
                "candidate_cake_sha256",
            )?)?,
            candidate_sqm_sha256: parse_optional_digest(&read_field(
                &mut lines,
                "candidate_sqm_sha256",
            )?)?,
            candidate_cake_mode: parse_optional_mode(&read_field(
                &mut lines,
                "candidate_cake_mode",
            )?)?,
            candidate_sqm_mode: parse_optional_mode(&read_field(
                &mut lines,
                "candidate_sqm_mode",
            )?)?,
        };
        if lines.next() != Some("") || lines.next() != Some("") || lines.next().is_some() {
            return Err("native Apply recovery state has trailing fields".to_string());
        }
        record.validate()?;
        if record.encode()? != bytes {
            return Err("native Apply recovery state is not canonical".to_string());
        }
        Ok(record)
    }
}

pub(crate) struct NativeApplyRecoveryStore {
    root: PathBuf,
}

pub(crate) struct NativeApplyPreparedMutation {
    _locks: NativeApplyConfigPairLock,
    recovery_root: PathBuf,
    cake_config: PathBuf,
    sqm_config: PathBuf,
    record: NativeApplyRecoveryRecord,
    cake_candidate: Vec<u8>,
    sqm_candidate: Vec<u8>,
}

impl NativeApplyPreparedMutation {
    pub(crate) fn install_candidate_files(&self) -> Result<(), String> {
        let store = NativeApplyRecoveryStore {
            root: self.recovery_root.clone(),
        };
        let live_record = store.read_record()?.ok_or_else(|| {
            "native Apply recovery disappeared before candidate write".to_string()
        })?;
        if live_record != self.record
            || live_record.schema_version != 5
            || live_record.state != NativeApplyRecoveryState::MutationStarted
        {
            return Err(
                "native Apply write-ahead authority changed before candidate write".to_string(),
            );
        }
        store.verify_backups(&live_record)?;
        store.verify_live_original_files(&self.cake_config, &self.sqm_config, &live_record)?;
        atomic_restore(
            &self.cake_config,
            &self.cake_candidate,
            live_record.cake_mode,
        )?;
        atomic_restore(&self.sqm_config, &self.sqm_candidate, live_record.sqm_mode)?;
        store.verify_live_write_ahead_candidate_files(
            &self.cake_config,
            &self.sqm_config,
            &live_record,
        )
    }
}

impl NativeApplyRecoveryStore {
    pub(crate) fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    pub(crate) fn prepare_write_ahead(
        &self,
        plan: &NativeApplyExecutionPlan,
        manifest: &[u8],
        original: &NativeApplyConfigPairSnapshot,
        candidate: &NativeApplyCandidateMaterialization,
    ) -> Result<NativeApplyRecoveryRecord, String> {
        require_existing_v4_apply_plan(plan)?;
        let expected_manifest = plan.canonical_manifest_bytes()?;
        if manifest != expected_manifest {
            return Err(
                "native Apply recovery manifest changed after plan construction".to_string(),
            );
        }
        let expected_authority = canonical_native_uci_batch(plan)?;
        if candidate.authority() != expected_authority {
            return Err(
                "native Apply materialization authority differs from the selected plan".to_string(),
            );
        }
        if original.cake.len() > MAX_CONFIG_BYTES
            || original.sqm.len() > MAX_CONFIG_BYTES
            || candidate.cake.len() > MAX_CONFIG_BYTES
            || candidate.sqm.len() > MAX_CONFIG_BYTES
        {
            return Err("native Apply write-ahead config exceeds its size bound".to_string());
        }
        validate_config_mode("cake-autorate", original.cake_mode)?;
        validate_config_mode("sqm", original.sqm_mode)?;
        ensure_private_directory(&self.root)?;
        if path_exists(&self.current_path())? {
            return Err("a native Apply recovery transaction is already pending".to_string());
        }
        if path_exists(&self.staging_path())? {
            return Err("an incomplete native Apply recovery staging directory exists".to_string());
        }

        let request_bytes = plan.request.encode()?.into_bytes();
        if request_bytes.len() > MAX_REQUEST_BYTES {
            return Err("native Apply recovery request exceeds its size bound".to_string());
        }
        let record = NativeApplyRecoveryRecord {
            schema_version: 5,
            state: NativeApplyRecoveryState::Prepared,
            job_id: plan.request.identity.job_id.clone(),
            worker_run_id: plan.worker_run_id.clone(),
            option_id: plan.option_id.clone(),
            manifest_sha256: sqm_identity::sha256sum(manifest)?,
            request_sha256: sqm_identity::sha256sum(&request_bytes)?,
            cake_sha256: sqm_identity::sha256sum(original.cake())?,
            sqm_sha256: sqm_identity::sha256sum(original.sqm())?,
            cake_mode: original.cake_mode,
            sqm_mode: original.sqm_mode,
            candidate_cake_sha256: Some(sqm_identity::sha256sum(candidate.cake())?),
            candidate_sqm_sha256: Some(sqm_identity::sha256sum(candidate.sqm())?),
            candidate_cake_mode: Some(original.cake_mode),
            candidate_sqm_mode: Some(original.sqm_mode),
            materialization_sha256: Some(sqm_identity::sha256sum(candidate.authority())?),
        };
        record.validate_candidate_phase(&plan.request)?;

        let staging = self.staging_path();
        create_private_directory(&staging)?;
        let publish = (|| {
            write_new_private_file(&staging.join(CAKE_BACKUP_FILE), original.cake())?;
            write_new_private_file(&staging.join(SQM_BACKUP_FILE), original.sqm())?;
            write_new_private_file(&staging.join(CAKE_CANDIDATE_FILE), candidate.cake())?;
            write_new_private_file(&staging.join(SQM_CANDIDATE_FILE), candidate.sqm())?;
            write_new_private_file(&staging.join(REQUEST_FILE), &request_bytes)?;
            write_new_private_file(&staging.join(MANIFEST_FILE), manifest)?;
            write_new_private_file(&staging.join(MATERIALIZATION_FILE), candidate.authority())?;
            write_new_private_file(&staging.join(STATE_FILE), &record.encode()?)?;
            sync_directory(&staging)?;
            fs::rename(&staging, self.current_path()).map_err(|error| {
                format!("unable to publish native Apply recovery state: {error}")
            })?;
            sync_directory(&self.root)?;
            Ok::<(), String>(())
        })();
        if let Err(error) = publish {
            return Err(error);
        }

        let stored = self
            .read_record()?
            .ok_or_else(|| "published native Apply recovery state disappeared".to_string())?;
        if stored != record {
            return Err("published native Apply recovery state changed".to_string());
        }
        self.verify_backups(&stored)?;
        Ok(stored)
    }

    #[cfg(test)]
    pub(crate) fn prepare(
        &self,
        plan: &NativeApplyExecutionPlan,
        manifest: &[u8],
        cake_config: &Path,
        sqm_config: &Path,
    ) -> Result<NativeApplyRecoveryRecord, String> {
        require_existing_v4_apply_plan(plan)?;
        let expected_manifest = plan.canonical_manifest_bytes()?;
        if manifest != expected_manifest {
            return Err(
                "native Apply recovery manifest changed after plan construction".to_string(),
            );
        }
        ensure_private_directory(&self.root)?;
        if path_exists(&self.current_path())? {
            return Err("a native Apply recovery transaction is already pending".to_string());
        }
        if path_exists(&self.staging_path())? {
            return Err("an incomplete native Apply recovery staging directory exists".to_string());
        }

        let (cake_bytes, cake_mode) = read_config_snapshot(cake_config, "cake-autorate")?;
        let (sqm_bytes, sqm_mode) = read_config_snapshot(sqm_config, "sqm")?;
        let request_bytes = plan.request.encode()?.into_bytes();
        if request_bytes.len() > MAX_REQUEST_BYTES {
            return Err("native Apply recovery request exceeds its size bound".to_string());
        }
        let record = NativeApplyRecoveryRecord {
            schema_version: 4,
            state: NativeApplyRecoveryState::Prepared,
            job_id: plan.request.identity.job_id.clone(),
            worker_run_id: plan.worker_run_id.clone(),
            option_id: plan.option_id.clone(),
            manifest_sha256: sqm_identity::sha256sum(manifest)?,
            request_sha256: sqm_identity::sha256sum(&request_bytes)?,
            cake_sha256: sqm_identity::sha256sum(&cake_bytes)?,
            sqm_sha256: sqm_identity::sha256sum(&sqm_bytes)?,
            cake_mode,
            sqm_mode,
            candidate_cake_sha256: None,
            candidate_sqm_sha256: None,
            candidate_cake_mode: None,
            candidate_sqm_mode: None,
            materialization_sha256: None,
        };

        let staging = self.staging_path();
        create_private_directory(&staging)?;
        let publish = (|| {
            write_new_private_file(&staging.join(CAKE_BACKUP_FILE), &cake_bytes)?;
            write_new_private_file(&staging.join(SQM_BACKUP_FILE), &sqm_bytes)?;
            write_new_private_file(&staging.join(REQUEST_FILE), &request_bytes)?;
            write_new_private_file(&staging.join(MANIFEST_FILE), manifest)?;
            write_new_private_file(&staging.join(STATE_FILE), &record.encode()?)?;
            sync_directory(&staging)?;
            fs::rename(&staging, self.current_path()).map_err(|error| {
                format!("unable to publish native Apply recovery state: {error}")
            })?;
            sync_directory(&self.root)?;
            Ok::<(), String>(())
        })();
        if let Err(error) = publish {
            return Err(error);
        }

        let stored = self
            .read_record()?
            .ok_or_else(|| "published native Apply recovery state disappeared".to_string())?;
        if stored != record {
            return Err("published native Apply recovery state changed".to_string());
        }
        self.verify_backups(&stored)?;
        Ok(stored)
    }

    pub(crate) fn read_record(&self) -> Result<Option<NativeApplyRecoveryRecord>, String> {
        let current = self.current_path();
        if !path_exists(&current)? {
            return Ok(None);
        }
        require_private_directory(&current)?;
        let bytes =
            read_private_recovery_bounded(&current.join(STATE_FILE), MAX_STATE_BYTES, "state")?;
        Ok(Some(NativeApplyRecoveryRecord::decode(&bytes)?))
    }

    pub(crate) fn read_request(&self) -> Result<OperationRequest, String> {
        let record = self
            .read_record()?
            .ok_or_else(|| "native Apply recovery transaction is missing".to_string())?;
        self.verify_backups(&record)?;
        let bytes = read_private_recovery_bounded(
            &self.current_path().join(REQUEST_FILE),
            MAX_REQUEST_BYTES,
            "request",
        )?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| "native Apply recovery request is not UTF-8".to_string())?;
        let request = OperationRequest::decode(text)?;
        if request.encode()? != text {
            return Err("native Apply recovery request uses a retired wire schema".to_string());
        }
        Ok(request)
    }

    fn read_manifest(&self) -> Result<Vec<u8>, String> {
        let record = self
            .read_record()?
            .ok_or_else(|| "native Apply recovery transaction is missing".to_string())?;
        self.verify_backups(&record)?;
        read_private_recovery_bounded(
            &self.current_path().join(MANIFEST_FILE),
            MAX_MANIFEST_BYTES,
            "manifest",
        )
    }

    fn read_original_snapshot(&self) -> Result<NativeApplyConfigPairSnapshot, String> {
        let record = self
            .read_record()?
            .ok_or_else(|| "native Apply recovery transaction is missing".to_string())?;
        self.verify_backups(&record)?;
        let current = self.current_path();
        Ok(NativeApplyConfigPairSnapshot {
            cake: read_private_recovery_bounded(
                &current.join(CAKE_BACKUP_FILE),
                MAX_CONFIG_BYTES,
                "cake backup",
            )?,
            sqm: read_private_recovery_bounded(
                &current.join(SQM_BACKUP_FILE),
                MAX_CONFIG_BYTES,
                "SQM backup",
            )?,
            cake_mode: record.cake_mode,
            sqm_mode: record.sqm_mode,
        })
    }

    fn read_containment_snapshots(&self) -> Result<Vec<NativeApplyConfigPairSnapshot>, String> {
        let record = self
            .read_record()?
            .ok_or_else(|| "native Apply recovery transaction is missing".to_string())?;
        self.verify_backups(&record)?;
        let mut snapshots = vec![self.read_original_snapshot()?];
        if record.candidate_cake_sha256.is_some() {
            let current = self.current_path();
            let candidate = NativeApplyConfigPairSnapshot {
                cake: read_private_recovery_bounded(
                    &current.join(CAKE_CANDIDATE_FILE),
                    MAX_CONFIG_BYTES,
                    "containment candidate cake",
                )?,
                sqm: read_private_recovery_bounded(
                    &current.join(SQM_CANDIDATE_FILE),
                    MAX_CONFIG_BYTES,
                    "containment candidate SQM",
                )?,
                cake_mode: record.candidate_cake_mode.ok_or_else(|| {
                    "native Apply containment candidate cake mode is missing".to_string()
                })?,
                sqm_mode: record.candidate_sqm_mode.ok_or_else(|| {
                    "native Apply containment candidate SQM mode is missing".to_string()
                })?,
            };
            if candidate != snapshots[0] {
                snapshots.push(candidate);
            }
        }
        Ok(snapshots)
    }

    fn upgrade_legacy_rollback_candidate(
        &self,
        request: &OperationRequest,
        candidate: &NativeApplyCandidateMaterialization,
    ) -> Result<NativeApplyRecoveryRecord, String> {
        let mut record = self
            .read_record()?
            .ok_or_else(|| "native Apply recovery transaction is missing".to_string())?;
        if record.schema_version != 4
            || record.state != NativeApplyRecoveryState::RollbackRequired
            || record.candidate_cake_sha256.is_some()
            || record.candidate_sqm_sha256.is_some()
            || record.candidate_cake_mode.is_some()
            || record.candidate_sqm_mode.is_some()
        {
            return Err(
                "legacy candidate reconstruction lacks an exact rollback-only state".to_string(),
            );
        }
        self.verify_backups(&record)?;
        let manifest = self.read_manifest()?;
        require_canonical_manifest_string_field(&manifest, "worker_run_id", &record.worker_run_id)?;
        require_canonical_manifest_string_field(&manifest, "option_id", &record.option_id)?;
        let (_, authority) = legacy_native_apply_materialization(&manifest, request)?;
        if candidate.authority() != authority {
            return Err(
                "legacy candidate reconstruction differs from the durable manifest".to_string(),
            );
        }
        let current = self.current_path();
        write_or_verify_private_file(
            &current.join(CAKE_CANDIDATE_FILE),
            candidate.cake(),
            MAX_CONFIG_BYTES,
            "legacy candidate cake",
        )?;
        write_or_verify_private_file(
            &current.join(SQM_CANDIDATE_FILE),
            candidate.sqm(),
            MAX_CONFIG_BYTES,
            "legacy candidate SQM",
        )?;
        write_or_verify_private_file(
            &current.join(MATERIALIZATION_FILE),
            candidate.authority(),
            MAX_MANIFEST_BYTES,
            "legacy materialization authority",
        )?;
        sync_directory(&current)?;

        record.schema_version = 5;
        record.candidate_cake_sha256 = Some(sqm_identity::sha256sum(candidate.cake())?);
        record.candidate_sqm_sha256 = Some(sqm_identity::sha256sum(candidate.sqm())?);
        record.candidate_cake_mode = Some(record.cake_mode);
        record.candidate_sqm_mode = Some(record.sqm_mode);
        record.materialization_sha256 = Some(sqm_identity::sha256sum(candidate.authority())?);
        replace_private_file(
            &current.join(STATE_FILE),
            &current.join(NEXT_STATE_FILE),
            &record.encode()?,
        )?;
        sync_directory(&current)?;
        let stored = self
            .read_record()?
            .ok_or_else(|| "upgraded native Apply recovery state disappeared".to_string())?;
        if stored != record {
            return Err("upgraded native Apply recovery identity changed".to_string());
        }
        self.verify_backups(&stored)?;
        Ok(stored)
    }

    pub(crate) fn transition(
        &self,
        expected: NativeApplyRecoveryState,
        next: NativeApplyRecoveryState,
    ) -> Result<NativeApplyRecoveryRecord, String> {
        if !expected.can_transition_to(next) {
            return Err(format!(
                "native Apply recovery transition {} -> {} is invalid",
                expected.as_str(),
                next.as_str()
            ));
        }
        let mut record = self
            .read_record()?
            .ok_or_else(|| "native Apply recovery transaction is missing".to_string())?;
        if record.state != expected {
            return Err(format!(
                "native Apply recovery state changed from expected {} to {}",
                expected.as_str(),
                record.state.as_str()
            ));
        }
        self.verify_backups(&record)?;
        record.state = next;
        replace_private_file(
            &self.current_path().join(STATE_FILE),
            &self.current_path().join(NEXT_STATE_FILE),
            &record.encode()?,
        )?;
        sync_directory(&self.current_path())?;
        let stored = self.read_record()?.ok_or_else(|| {
            "native Apply recovery state disappeared after transition".to_string()
        })?;
        if stored != record {
            return Err(
                "native Apply recovery state transition changed canonical data".to_string(),
            );
        }
        Ok(stored)
    }

    pub(crate) fn begin_mutation(
        &self,
        cake_config: &Path,
        sqm_config: &Path,
    ) -> Result<NativeApplyPreparedMutation, String> {
        let record = self
            .read_record()?
            .ok_or_else(|| "native Apply recovery transaction is missing".to_string())?;
        if record.schema_version != 5 || record.state != NativeApplyRecoveryState::Prepared {
            return Err(
                "native Apply write-ahead mutation may begin only from schema-v5 Prepared"
                    .to_string(),
            );
        }
        self.verify_backups(&record)?;
        let current = self.current_path();
        let cake_candidate = read_private_recovery_bounded(
            &current.join(CAKE_CANDIDATE_FILE),
            MAX_CONFIG_BYTES,
            "write-ahead candidate cake config",
        )?;
        let sqm_candidate = read_private_recovery_bounded(
            &current.join(SQM_CANDIDATE_FILE),
            MAX_CONFIG_BYTES,
            "write-ahead candidate SQM config",
        )?;
        let locks = NativeApplyConfigPairLock::acquire(cake_config, sqm_config)?;
        self.verify_live_original_files(cake_config, sqm_config, &record)?;
        let mutation_record = self.transition(
            NativeApplyRecoveryState::Prepared,
            NativeApplyRecoveryState::MutationStarted,
        )?;
        Ok(NativeApplyPreparedMutation {
            _locks: locks,
            recovery_root: self.root.clone(),
            cake_config: cake_config.to_path_buf(),
            sqm_config: sqm_config.to_path_buf(),
            record: mutation_record,
            cake_candidate,
            sqm_candidate,
        })
    }

    #[cfg(test)]
    pub(crate) fn stage_candidate(
        &self,
        cake_config: &Path,
        sqm_config: &Path,
        expected_record: &NativeApplyRecoveryRecord,
    ) -> Result<NativeApplyCandidateSnapshot, String> {
        let mut record = self
            .read_record()?
            .ok_or_else(|| "native Apply recovery transaction is missing".to_string())?;
        if record != *expected_record {
            return Err(
                "native Apply recovery identity changed before candidate staging".to_string(),
            );
        }
        if !matches!(
            record.state,
            NativeApplyRecoveryState::ServiceRestarted | NativeApplyRecoveryState::Verified
        ) {
            return Err(
                "native Apply candidate evidence requires a durable service restart".to_string(),
            );
        }
        if record.candidate_cake_sha256.is_some()
            || record.candidate_sqm_sha256.is_some()
            || record.candidate_cake_mode.is_some()
            || record.candidate_sqm_mode.is_some()
        {
            return Err("native Apply candidate evidence is already staged".to_string());
        }
        self.verify_backups(&record)?;
        let (cake_bytes, cake_mode) = read_config_snapshot(cake_config, "candidate cake-autorate")?;
        let (sqm_bytes, sqm_mode) = read_config_snapshot(sqm_config, "candidate sqm")?;
        let current = self.current_path();
        write_new_private_file(&current.join(CAKE_CANDIDATE_FILE), &cake_bytes)?;
        write_new_private_file(&current.join(SQM_CANDIDATE_FILE), &sqm_bytes)?;
        sync_directory(&current)?;

        let snapshot = NativeApplyCandidateSnapshot {
            cake_sha256: sqm_identity::sha256sum(&cake_bytes)?,
            sqm_sha256: sqm_identity::sha256sum(&sqm_bytes)?,
            cake_mode,
            sqm_mode,
        };
        self.verify_staged_candidate_files(cake_config, sqm_config, &snapshot)?;
        record.candidate_cake_sha256 = Some(snapshot.cake_sha256.clone());
        record.candidate_sqm_sha256 = Some(snapshot.sqm_sha256.clone());
        record.candidate_cake_mode = Some(snapshot.cake_mode);
        record.candidate_sqm_mode = Some(snapshot.sqm_mode);
        replace_private_file(
            &current.join(STATE_FILE),
            &current.join(NEXT_STATE_FILE),
            &record.encode()?,
        )?;
        sync_directory(&current)?;
        let stored = self
            .read_record()?
            .ok_or_else(|| "native Apply candidate evidence disappeared".to_string())?;
        if stored != record {
            return Err("native Apply candidate evidence changed after publication".to_string());
        }
        self.verify_backups(&stored)?;
        Ok(snapshot)
    }

    #[cfg(test)]
    pub(crate) fn accept_candidate(
        &self,
        cake_config: &Path,
        sqm_config: &Path,
        snapshot: &NativeApplyCandidateSnapshot,
    ) -> Result<NativeApplyRecoveryRecord, String> {
        let mut record = self
            .read_record()?
            .ok_or_else(|| "native Apply recovery transaction is missing".to_string())?;
        if record.state != NativeApplyRecoveryState::Verified {
            return Err(
                "native Apply candidate may be accepted only after verification".to_string(),
            );
        }
        self.verify_backups(&record)?;
        if record.candidate_cake_sha256.as_deref() != Some(snapshot.cake_sha256.as_str())
            || record.candidate_sqm_sha256.as_deref() != Some(snapshot.sqm_sha256.as_str())
            || record.candidate_cake_mode != Some(snapshot.cake_mode)
            || record.candidate_sqm_mode != Some(snapshot.sqm_mode)
        {
            return Err("native Apply candidate evidence differs from staged snapshot".to_string());
        }
        self.verify_staged_candidate_files(cake_config, sqm_config, snapshot)?;

        record.state = NativeApplyRecoveryState::CommitAccepted;
        let current = self.current_path();
        replace_private_file(
            &current.join(STATE_FILE),
            &current.join(NEXT_STATE_FILE),
            &record.encode()?,
        )?;
        sync_directory(&current)?;
        let stored = self
            .read_record()?
            .ok_or_else(|| "commit-accepted native Apply state disappeared".to_string())?;
        if stored != record {
            return Err("commit-accepted native Apply state changed".to_string());
        }
        self.verify_backups(&stored)?;
        Ok(stored)
    }

    #[cfg(test)]
    pub(crate) fn verify_staged_candidate_files(
        &self,
        cake_config: &Path,
        sqm_config: &Path,
        snapshot: &NativeApplyCandidateSnapshot,
    ) -> Result<(), String> {
        self.verify_staged_snapshot(snapshot)?;
        verify_restored_file(
            cake_config,
            &snapshot.cake_sha256,
            snapshot.cake_mode,
            "staged candidate cake",
        )?;
        verify_restored_file(
            sqm_config,
            &snapshot.sqm_sha256,
            snapshot.sqm_mode,
            "staged candidate SQM",
        )
    }

    #[cfg(test)]
    fn verify_staged_snapshot(
        &self,
        snapshot: &NativeApplyCandidateSnapshot,
    ) -> Result<(), String> {
        require_lower_hex("staged candidate cake digest", &snapshot.cake_sha256, 64)?;
        require_lower_hex("staged candidate SQM digest", &snapshot.sqm_sha256, 64)?;
        validate_config_mode("staged candidate cake", snapshot.cake_mode)?;
        validate_config_mode("staged candidate SQM", snapshot.sqm_mode)?;
        let current = self.current_path();
        let cake = read_private_recovery_bounded(
            &current.join(CAKE_CANDIDATE_FILE),
            MAX_CONFIG_BYTES,
            "staged candidate cake config",
        )?;
        let sqm = read_private_recovery_bounded(
            &current.join(SQM_CANDIDATE_FILE),
            MAX_CONFIG_BYTES,
            "staged candidate SQM config",
        )?;
        if sqm_identity::sha256sum(&cake)? != snapshot.cake_sha256
            || sqm_identity::sha256sum(&sqm)? != snapshot.sqm_sha256
        {
            return Err("native Apply staged candidate snapshot changed".to_string());
        }
        Ok(())
    }

    pub(crate) fn verify_backups(&self, record: &NativeApplyRecoveryRecord) -> Result<(), String> {
        let current = self.current_path();
        require_private_directory(&current)?;
        let cake = read_private_recovery_bounded(
            &current.join(CAKE_BACKUP_FILE),
            MAX_CONFIG_BYTES,
            "cake backup",
        )?;
        let sqm = read_private_recovery_bounded(
            &current.join(SQM_BACKUP_FILE),
            MAX_CONFIG_BYTES,
            "SQM backup",
        )?;
        let manifest = read_private_recovery_bounded(
            &current.join(MANIFEST_FILE),
            MAX_MANIFEST_BYTES,
            "manifest",
        )?;
        let request = read_private_recovery_bounded(
            &current.join(REQUEST_FILE),
            MAX_REQUEST_BYTES,
            "request",
        )?;
        for (label, actual, expected) in [
            (
                "cake config",
                sqm_identity::sha256sum(&cake)?,
                &record.cake_sha256,
            ),
            (
                "SQM config",
                sqm_identity::sha256sum(&sqm)?,
                &record.sqm_sha256,
            ),
            (
                "manifest",
                sqm_identity::sha256sum(&manifest)?,
                &record.manifest_sha256,
            ),
            (
                "request",
                sqm_identity::sha256sum(&request)?,
                &record.request_sha256,
            ),
        ] {
            if actual != *expected {
                return Err(format!("native Apply recovery {label} digest mismatch"));
            }
        }
        if record.schema_version == 5 {
            let materialization = read_private_recovery_bounded(
                &current.join(MATERIALIZATION_FILE),
                MAX_MANIFEST_BYTES,
                "materialization authority",
            )?;
            if sqm_identity::sha256sum(&materialization)?
                != *record
                    .materialization_sha256
                    .as_ref()
                    .ok_or_else(|| "native Apply materialization digest is missing".to_string())?
            {
                return Err("native Apply recovery materialization digest mismatch".to_string());
            }
        }
        if record.candidate_cake_sha256.is_some() {
            let candidate_cake = read_private_recovery_bounded(
                &current.join(CAKE_CANDIDATE_FILE),
                MAX_CONFIG_BYTES,
                "candidate cake config",
            )?;
            let candidate_sqm = read_private_recovery_bounded(
                &current.join(SQM_CANDIDATE_FILE),
                MAX_CONFIG_BYTES,
                "candidate SQM config",
            )?;
            if sqm_identity::sha256sum(&candidate_cake)?
                != *record
                    .candidate_cake_sha256
                    .as_ref()
                    .ok_or_else(|| "candidate cake digest is missing".to_string())?
                || sqm_identity::sha256sum(&candidate_sqm)?
                    != *record
                        .candidate_sqm_sha256
                        .as_ref()
                        .ok_or_else(|| "candidate SQM digest is missing".to_string())?
            {
                return Err("native Apply recovery candidate snapshot digest mismatch".to_string());
            }
        }
        Ok(())
    }

    pub(crate) fn verify_live_candidate_files(
        &self,
        cake_config: &Path,
        sqm_config: &Path,
        record: &NativeApplyRecoveryRecord,
    ) -> Result<(), String> {
        if record.state != NativeApplyRecoveryState::CommitAccepted {
            return Err("native Apply live candidate has no commit authority".to_string());
        }
        self.verify_live_write_ahead_candidate_files(cake_config, sqm_config, record)
    }

    fn verify_live_write_ahead_candidate_files(
        &self,
        cake_config: &Path,
        sqm_config: &Path,
        record: &NativeApplyRecoveryRecord,
    ) -> Result<(), String> {
        self.verify_backups(record)?;
        verify_restored_file(
            cake_config,
            record
                .candidate_cake_sha256
                .as_deref()
                .ok_or_else(|| "candidate cake digest is missing".to_string())?,
            record
                .candidate_cake_mode
                .ok_or_else(|| "candidate cake mode is missing".to_string())?,
            "candidate cake",
        )?;
        verify_restored_file(
            sqm_config,
            record
                .candidate_sqm_sha256
                .as_deref()
                .ok_or_else(|| "candidate SQM digest is missing".to_string())?,
            record
                .candidate_sqm_mode
                .ok_or_else(|| "candidate SQM mode is missing".to_string())?,
            "candidate SQM",
        )
    }

    pub(crate) fn verify_live_original_files(
        &self,
        cake_config: &Path,
        sqm_config: &Path,
        record: &NativeApplyRecoveryRecord,
    ) -> Result<(), String> {
        self.verify_backups(record)?;
        verify_restored_file(
            cake_config,
            &record.cake_sha256,
            record.cake_mode,
            "original cake",
        )?;
        verify_restored_file(
            sqm_config,
            &record.sqm_sha256,
            record.sqm_mode,
            "original SQM",
        )
    }

    pub(crate) fn restore_candidate_files(
        &self,
        cake_config: &Path,
        sqm_config: &Path,
    ) -> Result<NativeApplyRecoveryRecord, String> {
        let record = self
            .read_record()?
            .ok_or_else(|| "native Apply recovery transaction is missing".to_string())?;
        if record.state != NativeApplyRecoveryState::CommitAccepted {
            return Err(
                "native Apply candidate may roll forward only after commit acceptance".to_string(),
            );
        }
        self.verify_backups(&record)?;
        let _config_locks = NativeApplyConfigPairLock::acquire(cake_config, sqm_config)?;
        remove_stale_restore_temporary(cake_config)?;
        remove_stale_restore_temporary(sqm_config)?;
        let restore_needed = self.roll_forward_restore_needed(cake_config, sqm_config, &record)?;
        let current = self.current_path();
        let cake = read_private_recovery_bounded(
            &current.join(CAKE_CANDIDATE_FILE),
            MAX_CONFIG_BYTES,
            "candidate cake config",
        )?;
        let sqm = read_private_recovery_bounded(
            &current.join(SQM_CANDIDATE_FILE),
            MAX_CONFIG_BYTES,
            "candidate SQM config",
        )?;
        if restore_needed {
            atomic_restore(
                cake_config,
                &cake,
                record
                    .candidate_cake_mode
                    .ok_or_else(|| "candidate cake mode is missing".to_string())?,
            )?;
            atomic_restore(
                sqm_config,
                &sqm,
                record
                    .candidate_sqm_mode
                    .ok_or_else(|| "candidate SQM mode is missing".to_string())?,
            )?;
        }
        self.verify_live_candidate_files(cake_config, sqm_config, &record)?;
        Ok(record)
    }

    pub(crate) fn restore_config_files(
        &self,
        cake_config: &Path,
        sqm_config: &Path,
    ) -> Result<NativeApplyRecoveryRecord, String> {
        let record = self
            .read_record()?
            .ok_or_else(|| "native Apply recovery transaction is missing".to_string())?;
        if record.state != NativeApplyRecoveryState::RollbackRequired {
            return Err(
                "native Apply configs may be restored only after rollback intent".to_string(),
            );
        }
        self.verify_backups(&record)?;
        let _config_locks = NativeApplyConfigPairLock::acquire(cake_config, sqm_config)?;
        remove_stale_restore_temporary(cake_config)?;
        remove_stale_restore_temporary(sqm_config)?;
        let restore_needed = self.rollback_restore_needed(cake_config, sqm_config, &record)?;
        let current = self.current_path();
        let cake = read_private_recovery_bounded(
            &current.join(CAKE_BACKUP_FILE),
            MAX_CONFIG_BYTES,
            "cake backup",
        )?;
        let sqm = read_private_recovery_bounded(
            &current.join(SQM_BACKUP_FILE),
            MAX_CONFIG_BYTES,
            "SQM backup",
        )?;
        if restore_needed {
            atomic_restore(cake_config, &cake, record.cake_mode)?;
            atomic_restore(sqm_config, &sqm, record.sqm_mode)?;
        }
        verify_restored_file(cake_config, &record.cake_sha256, record.cake_mode, "cake")?;
        verify_restored_file(sqm_config, &record.sqm_sha256, record.sqm_mode, "SQM")?;
        Ok(record)
    }

    fn roll_forward_restore_needed(
        &self,
        cake_config: &Path,
        sqm_config: &Path,
        record: &NativeApplyRecoveryRecord,
    ) -> Result<bool, String> {
        self.verify_backups(record)?;
        let (cake_bytes, cake_mode) = read_config_snapshot(cake_config, "roll-forward live cake")?;
        let (sqm_bytes, sqm_mode) = read_config_snapshot(sqm_config, "roll-forward live SQM")?;
        let cake_digest = sqm_identity::sha256sum(&cake_bytes)?;
        let sqm_digest = sqm_identity::sha256sum(&sqm_bytes)?;
        let cake_original = cake_digest == record.cake_sha256 && cake_mode == record.cake_mode;
        let sqm_original = sqm_digest == record.sqm_sha256 && sqm_mode == record.sqm_mode;
        let cake_candidate = record
            .candidate_cake_sha256
            .as_deref()
            .zip(record.candidate_cake_mode)
            .is_some_and(|(digest, mode)| cake_digest == digest && cake_mode == mode);
        let sqm_candidate = record
            .candidate_sqm_sha256
            .as_deref()
            .zip(record.candidate_sqm_mode)
            .is_some_and(|(digest, mode)| sqm_digest == digest && sqm_mode == mode);
        if (!cake_original && !cake_candidate) || (!sqm_original && !sqm_candidate) {
            return Err(
                "native Apply roll-forward found foreign config state; refusing whole-file overwrite"
                    .to_string(),
            );
        }
        Ok(!cake_candidate || !sqm_candidate)
    }

    fn rollback_restore_needed(
        &self,
        cake_config: &Path,
        sqm_config: &Path,
        record: &NativeApplyRecoveryRecord,
    ) -> Result<bool, String> {
        self.verify_backups(record)?;
        let (cake_bytes, cake_mode) = read_config_snapshot(cake_config, "rollback live cake")?;
        let (sqm_bytes, sqm_mode) = read_config_snapshot(sqm_config, "rollback live SQM")?;
        let cake_digest = sqm_identity::sha256sum(&cake_bytes)?;
        let sqm_digest = sqm_identity::sha256sum(&sqm_bytes)?;
        let cake_original = cake_digest == record.cake_sha256 && cake_mode == record.cake_mode;
        let sqm_original = sqm_digest == record.sqm_sha256 && sqm_mode == record.sqm_mode;
        let cake_candidate = record
            .candidate_cake_sha256
            .as_deref()
            .zip(record.candidate_cake_mode)
            .is_some_and(|(digest, mode)| cake_digest == digest && cake_mode == mode);
        let sqm_candidate = record
            .candidate_sqm_sha256
            .as_deref()
            .zip(record.candidate_sqm_mode)
            .is_some_and(|(digest, mode)| sqm_digest == digest && sqm_mode == mode);
        if (!cake_original && !cake_candidate) || (!sqm_original && !sqm_candidate) {
            return Err(
                "native Apply rollback found foreign or unproven config state; refusing whole-file overwrite"
                    .to_string(),
            );
        }
        Ok(!cake_original || !sqm_original)
    }

    pub(crate) fn clear_restored(&self) -> Result<(), String> {
        let record = self
            .read_record()?
            .ok_or_else(|| "native Apply recovery transaction is missing".to_string())?;
        if record.state != NativeApplyRecoveryState::Restored {
            return Err("native Apply recovery evidence is not restored".to_string());
        }
        self.complete_current_transaction(NativeApplyRecoveryState::Restored)
    }

    pub(crate) fn clear_prepared(&self) -> Result<(), String> {
        self.complete_current_transaction(NativeApplyRecoveryState::Prepared)
    }

    pub(crate) fn clear_committed(&self) -> Result<(), String> {
        self.complete_current_transaction(NativeApplyRecoveryState::CommitAccepted)
    }

    pub(crate) fn discard_incomplete_staging(&self) -> Result<(), String> {
        let staging = self.staging_path();
        if !path_exists(&staging)? {
            return Ok(());
        }
        self.remove_transaction_directory(&staging, "incomplete staging")
    }

    pub(crate) fn discard_completed(&self) -> Result<(), String> {
        let completed = self.completed_path();
        if !path_exists(&completed)? {
            return Ok(());
        }
        self.remove_transaction_directory(&completed, "completed recovery")
    }

    fn complete_current_transaction(
        &self,
        expected: NativeApplyRecoveryState,
    ) -> Result<(), String> {
        let record = self
            .read_record()?
            .ok_or_else(|| "native Apply recovery transaction is missing".to_string())?;
        if record.state != expected {
            return Err(format!(
                "native Apply transaction cannot complete from {}",
                record.state.as_str()
            ));
        }
        self.verify_backups(&record)?;
        self.discard_completed()?;
        let current = self.current_path();
        reject_unknown_entries(&current)?;
        fs::rename(&current, self.completed_path())
            .map_err(|error| format!("unable to atomically complete native Apply: {error}"))?;
        sync_directory(&self.root)?;
        self.discard_completed()
    }

    fn remove_transaction_directory(&self, path: &Path, label: &str) -> Result<(), String> {
        require_private_directory(path)?;
        reject_unknown_entries(path)?;
        for name in [
            NEXT_STATE_FILE,
            STATE_FILE,
            MANIFEST_FILE,
            MATERIALIZATION_FILE,
            REQUEST_FILE,
            SQM_CANDIDATE_FILE,
            CAKE_CANDIDATE_FILE,
            SQM_BACKUP_FILE,
            CAKE_BACKUP_FILE,
        ] {
            match fs::remove_file(path.join(name)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "unable to remove {label} native Apply file {name}: {error}"
                    ))
                }
            }
        }
        sync_directory(path)?;
        fs::remove_dir(path)
            .map_err(|error| format!("unable to remove {label} directory: {error}"))?;
        sync_directory(&self.root)
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

pub(super) fn read_config_snapshot(path: &Path, label: &str) -> Result<(Vec<u8>, u32), String> {
    let metadata = strict_regular_metadata(path, label)?;
    if metadata.len() > MAX_CONFIG_BYTES as u64 {
        return Err(format!("{label} config exceeds its size bound"));
    }
    let mode = metadata.permissions().mode() & 0o777;
    validate_config_mode(label, mode)?;
    let bytes = read_regular_bounded(path, MAX_CONFIG_BYTES, label)?;
    Ok((bytes, mode))
}

pub(super) fn atomic_restore(path: &Path, bytes: &[u8], mode: u32) -> Result<(), String> {
    validate_config_mode("restored config", mode)?;
    remove_stale_restore_temporary(path)?;
    let (parent, temp) = restore_temporary_path(path)?;
    let mut file = open_new_private(&temp)?;
    file.write_all(bytes)
        .map_err(|error| format!("unable to write restored config temporary: {error}"))?;
    file.set_permissions(fs::Permissions::from_mode(mode))
        .map_err(|error| format!("unable to set restored config mode: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("unable to sync restored config temporary: {error}"))?;
    drop(file);
    fs::rename(&temp, path)
        .map_err(|error| format!("unable to publish restored config: {error}"))?;
    sync_directory(&parent)
}

fn restore_temporary_path(path: &Path) -> Result<(PathBuf, PathBuf), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "restored config has no parent directory".to_string())?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "restored config has no safe filename".to_string())?;
    if name.is_empty() || name.contains(['/', '\0']) {
        return Err("restored config filename is unsafe".to_string());
    }
    Ok((
        parent.to_path_buf(),
        parent.join(format!(".{name}.native-apply-restore")),
    ))
}

pub(super) fn remove_stale_restore_temporary(path: &Path) -> Result<(), String> {
    let (parent, temp) = restore_temporary_path(path)?;
    if path_exists(&temp)? {
        let metadata = strict_regular_metadata(&temp, "stale restore temporary")?;
        if metadata.nlink() != 1 {
            return Err("stale restore temporary has multiple links".to_string());
        }
        fs::remove_file(&temp)
            .map_err(|error| format!("unable to remove stale restore temporary: {error}"))?;
        sync_directory(&parent)?;
    }
    Ok(())
}

pub(super) fn verify_restored_file(
    path: &Path,
    digest: &str,
    mode: u32,
    label: &str,
) -> Result<(), String> {
    let metadata = strict_regular_metadata(path, label)?;
    if metadata.permissions().mode() & 0o777 != mode {
        return Err(format!("restored {label} config mode mismatch"));
    }
    let bytes = read_regular_bounded(path, MAX_CONFIG_BYTES, label)?;
    if sqm_identity::sha256sum(&bytes)? != digest {
        return Err(format!("restored {label} config digest mismatch"));
    }
    Ok(())
}

pub(super) fn ensure_private_directory(path: &Path) -> Result<(), String> {
    if !path_exists(path)? {
        fs::create_dir_all(path)
            .map_err(|error| format!("unable to create native Apply recovery root: {error}"))?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("unable to secure native Apply recovery root: {error}"))?;
        sync_directory(
            path.parent()
                .ok_or_else(|| "native Apply recovery root has no parent".to_string())?,
        )?;
    } else {
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| format!("unable to inspect native Apply directory: {error}"))?;
        if !metadata.file_type().is_dir() || metadata.uid() != effective_uid() {
            return Err("native Apply directory is not safely owned".to_string());
        }
        if metadata.permissions().mode() & 0o777 != 0o700 {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .map_err(|error| format!("unable to secure native Apply directory: {error}"))?;
        }
    }
    require_private_directory(path)
}

pub(super) fn create_private_directory(path: &Path) -> Result<(), String> {
    fs::create_dir(path)
        .map_err(|error| format!("unable to create private recovery directory: {error}"))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("unable to secure private recovery directory: {error}"))?;
    require_private_directory(path)
}

pub(super) fn require_private_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("unable to inspect recovery directory: {error}"))?;
    if !metadata.file_type().is_dir()
        || metadata.uid() != effective_uid()
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err("native Apply recovery path is not a private directory".to_string());
    }
    Ok(())
}

pub(super) fn write_new_private_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = open_new_private(path)?;
    file.write_all(bytes)
        .map_err(|error| format!("unable to write private recovery file: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("unable to sync private recovery file: {error}"))?;
    drop(file);
    let metadata = strict_regular_metadata(path, "private recovery file")?;
    if metadata.permissions().mode() & 0o777 != 0o600 || metadata.nlink() != 1 {
        return Err("native Apply recovery file is not private and single-linked".to_string());
    }
    Ok(())
}

fn write_or_verify_private_file(
    path: &Path,
    bytes: &[u8],
    maximum: usize,
    label: &str,
) -> Result<(), String> {
    if path_exists(path)? {
        let existing = read_private_recovery_bounded(path, maximum, label)?;
        if existing != bytes {
            return Err(format!(
                "native Apply {label} residue differs from reconstruction"
            ));
        }
        return Ok(());
    }
    write_new_private_file(path, bytes)
}

pub(super) fn replace_private_file(path: &Path, temp: &Path, bytes: &[u8]) -> Result<(), String> {
    if path_exists(temp)? {
        let metadata = strict_regular_metadata(temp, "stale recovery state temporary")?;
        if metadata.nlink() != 1 {
            return Err("stale recovery state temporary has multiple links".to_string());
        }
        fs::remove_file(temp)
            .map_err(|error| format!("unable to remove stale recovery state: {error}"))?;
    }
    write_new_private_file(temp, bytes)?;
    fs::rename(temp, path)
        .map_err(|error| format!("unable to publish recovery state transition: {error}"))?;
    Ok(())
}

fn open_new_private(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("unable to create private native Apply file: {error}"))
}

fn read_regular_bounded(path: &Path, maximum: usize, label: &str) -> Result<Vec<u8>, String> {
    let metadata = strict_regular_metadata(path, label)?;
    if metadata.nlink() != 1 || metadata.len() > maximum as u64 {
        return Err(format!("native Apply {label} file is unsafe or oversized"));
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("unable to open native Apply {label}: {error}"))?;
    let opened = file
        .metadata()
        .map_err(|error| format!("unable to inspect opened native Apply {label}: {error}"))?;
    if !opened.is_file()
        || opened.dev() != metadata.dev()
        || opened.ino() != metadata.ino()
        || opened.len() != metadata.len()
    {
        return Err(format!("native Apply {label} changed while opening"));
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    file.read_to_end(&mut bytes)
        .map_err(|error| format!("unable to read native Apply {label}: {error}"))?;
    if bytes.len() > maximum {
        return Err(format!("native Apply {label} exceeded its size bound"));
    }
    Ok(bytes)
}

pub(super) fn read_private_recovery_bounded(
    path: &Path,
    maximum: usize,
    label: &str,
) -> Result<Vec<u8>, String> {
    let metadata = strict_regular_metadata(path, label)?;
    if metadata.permissions().mode() & 0o777 != 0o600 || metadata.nlink() != 1 {
        return Err(format!(
            "native Apply recovery {label} is not private and single-linked"
        ));
    }
    read_regular_bounded(path, maximum, label)
}

fn strict_regular_metadata(path: &Path, label: &str) -> Result<fs::Metadata, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("unable to inspect native Apply {label}: {error}"))?;
    if !metadata.file_type().is_file() || metadata.uid() != effective_uid() || metadata.nlink() != 1
    {
        return Err(format!("native Apply {label} is not a regular file"));
    }
    Ok(metadata)
}

fn effective_uid() -> u32 {
    unsafe { libc::geteuid() }
}

pub(super) fn reject_unknown_entries(path: &Path) -> Result<(), String> {
    let allowed = BTreeSet::from([
        STATE_FILE,
        NEXT_STATE_FILE,
        CAKE_BACKUP_FILE,
        SQM_BACKUP_FILE,
        CAKE_CANDIDATE_FILE,
        SQM_CANDIDATE_FILE,
        REQUEST_FILE,
        MANIFEST_FILE,
        MATERIALIZATION_FILE,
    ]);
    for entry in fs::read_dir(path)
        .map_err(|error| format!("unable to enumerate native Apply recovery directory: {error}"))?
    {
        let entry = entry
            .map_err(|error| format!("unable to inspect native Apply recovery entry: {error}"))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "native Apply recovery entry name is not UTF-8".to_string())?;
        if !allowed.contains(name.as_str()) {
            return Err(format!("unknown native Apply recovery entry: {name}"));
        }
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| format!("unable to inspect native Apply recovery entry: {error}"))?;
        if !metadata.file_type().is_file() || metadata.nlink() != 1 {
            return Err(format!("unsafe native Apply recovery entry: {name}"));
        }
    }
    Ok(())
}

pub(super) fn sync_directory(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("unable to sync native Apply directory: {error}"))
}

pub(super) fn path_exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("unable to inspect native Apply path: {error}")),
    }
}

pub(super) fn read_field<'a>(
    lines: &mut impl Iterator<Item = &'a str>,
    expected: &str,
) -> Result<String, String> {
    let line = lines
        .next()
        .ok_or_else(|| format!("native Apply recovery state is missing {expected}"))?;
    let (name, value) = line
        .split_once('=')
        .ok_or_else(|| format!("native Apply recovery field {expected} is malformed"))?;
    if name != expected || value.is_empty() || value.contains(['\r', '\0']) {
        return Err(format!(
            "native Apply recovery state expected field {expected}"
        ));
    }
    Ok(value.to_string())
}

pub(super) fn require_lower_hex(label: &str, value: &str, length: usize) -> Result<(), String> {
    if value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(format!(
            "{label} must be exactly {length} lowercase hex characters"
        ))
    }
}

pub(super) fn parse_mode(value: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .map_err(|_| "native Apply recovery config mode is invalid".to_string())
}

fn parse_optional_digest(value: &str) -> Result<Option<String>, String> {
    if value == "none" {
        return Ok(None);
    }
    require_lower_hex("candidate config digest", value, 64)?;
    Ok(Some(value.to_string()))
}

fn parse_optional_mode(value: &str) -> Result<Option<u32>, String> {
    if value == "none" {
        return Ok(None);
    }
    parse_mode(value).map(Some)
}

pub(super) fn validate_config_mode(label: &str, mode: u32) -> Result<(), String> {
    if mode == 0 || mode & !0o777 != 0 {
        return Err(format!("native Apply {label} config mode is unsafe"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autotune::{
        build_proposal_for_profile_with_context, AccessEvidenceSource, AccessMedium,
        AutotuneProfile, CapacityLearningPolicy, LatencyBaseline, LinkKind, ProposalContext,
    };
    use crate::operations::autotune_apply::{
        NativeApplyAcknowledgement, NativeApplyAction, NativeApplyArtifactDigests,
        NativeApplyDirectionInput, NativeApplyDirectionMode, NativeApplyManifestInput,
        NativeSqmDirectionMode,
    };
    use crate::operations::protocol::{
        CalibrationStrategy, OperationIdentity, OperationKind, OperationOrigin, OperationRequest,
        OperationRouteIdentity, OperationRouteMode, OperationTargetState,
    };
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST: AtomicU64 = AtomicU64::new(1);

    fn temp_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "cake-native-apply-recovery-{}-{}-{name}",
            std::process::id(),
            NEXT_TEST.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn plan() -> NativeApplyExecutionPlan {
        let request = OperationRequest {
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
            created_unix_ms: 1,
            deadline_unix_ms: 600_001,
            origin: OperationOrigin::Luci,
            backend: "speedtest-go".to_string(),
            speedtest_direction: None,
            speedtest_server_id: Some(17_372),
            speedtest_topology: None,
            route: OperationRouteIdentity {
                mode: OperationRouteMode::Main,
                mwan3_member: None,
                l3_device: "pppoe-wan".to_string(),
                source_ip: Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))),
                fwmark: None,
                routing_table: None,
            },
            target_state: OperationTargetState::ExistingManaged,
            capture_policy: None,
            managed_sqm_section: Some("wan_sqm".to_string()),
            profile: Some(AutotuneProfile::BestOverall),
            strategy: Some(CalibrationStrategy::FullRaw),
            access_medium: Some(AccessMedium::SharedWired),
            access_source: Some(AccessEvidenceSource::UserSelected),
            access_confidence_percent: 100,
            capacity_learning_policy: Some(CapacityLearningPolicy::VerifiedOnly),
            service_dl_cap_kbps: None,
            service_ul_cap_kbps: None,
            allow_sqm_disable: true,
            allow_active_traffic: false,
            scheduled_auto_apply_requested: false,
            traffic_budget_bytes: 1_000_000_000,
        };
        let proposal = build_proposal_for_profile_with_context(
            &[100_000.0, 101_000.0],
            &[50_000.0, 51_000.0],
            LatencyBaseline {
                median_ms: 5.0,
                p95_ms: 8.0,
                samples: 20,
            },
            LinkKind::Pppoe,
            AutotuneProfile::BestOverall,
            ProposalContext {
                access_medium: Some(AccessMedium::SharedWired),
                access_source: AccessEvidenceSource::UserSelected,
                access_confidence_percent: 100,
                capacity_learning_policy: Some(CapacityLearningPolicy::VerifiedOnly),
                download_service_cap_kbps: None,
                upload_service_cap_kbps: None,
            },
        )
        .unwrap();
        NativeApplyExecutionPlan::from_verified_input(NativeApplyManifestInput {
            option_id: "recommended",
            request: &request,
            worker_run_id: &"66".repeat(16),
            review_digest: &"aa".repeat(32),
            coordinator_boot_id: "boot-id",
            coordinator_generation: &"77".repeat(16),
            selected_topology: "both_shaped",
            action: NativeApplyAction::ApplySqm,
            sqm_direction_mode: NativeSqmDirectionMode::Both,
            download: NativeApplyDirectionInput {
                mode: NativeApplyDirectionMode::Shaped,
                selected_kbps: Some(proposal.download.base_kbps),
                measured_runtime_minimum_kbps: None,
                proposal: proposal.download,
            },
            upload: NativeApplyDirectionInput {
                mode: NativeApplyDirectionMode::Shaped,
                selected_kbps: Some(proposal.upload.base_kbps),
                measured_runtime_minimum_kbps: None,
                proposal: proposal.upload,
            },
            proposal: &proposal,
            auto_apply_evidence_pass: false,
            manual_review_required: true,
            required_acknowledgements: &[NativeApplyAcknowledgement::MeasurementConfidence],
            artifacts: NativeApplyArtifactDigests {
                proposal: &"bb".repeat(32),
                download_search: &"cc".repeat(32),
                upload_search: &"dd".repeat(32),
                pair_confirmation: &"ee".repeat(32),
                topology_comparison: &"ff".repeat(32),
            },
        })
        .unwrap()
    }

    fn absent_plan() -> NativeApplyExecutionPlan {
        let mut plan = plan();
        plan.request.target_state = OperationTargetState::AbsentBootstrap;
        plan.request.capture_policy =
            Some(crate::operations::autotune_capture_policy::AutotuneCapturePolicyId::StandardV1);
        plan
    }

    fn write_config(path: &Path, bytes: &[u8], mode: u32) {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(path)
            .unwrap();
        file.set_permissions(fs::Permissions::from_mode(mode))
            .unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
    }

    #[test]
    fn schema_v4_store_rejects_bootstrap_v5_recovery_header() {
        let root = temp_root("reject-v5-header");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let current = root.join(CURRENT_DIRECTORY);
        fs::create_dir(&current).unwrap();
        fs::set_permissions(&current, fs::Permissions::from_mode(0o700)).unwrap();
        write_config(
            &current.join(STATE_FILE),
            b"cake-autorate-native-bootstrap-apply-recovery\t5\n",
            0o600,
        );
        assert!(NativeApplyRecoveryStore::new(&root)
            .read_record()
            .unwrap_err()
            .contains("unsupported header"));
        fs::remove_dir_all(root).unwrap();
    }

    struct FakeBackend {
        cake: PathBuf,
        sqm: PathBuf,
        events: Vec<&'static str>,
        attest_count: u8,
        restart_count: u8,
        fail_attest: bool,
        drift_original_on_second_attest: bool,
        drift_sqm_on_second_attest: bool,
        fail_apply_after_partial_write: bool,
        fail_verify_applied: bool,
        fail_rollback_restart: bool,
        mutate_original_after_rollback_restart: bool,
        runtime_only_restored_verification: bool,
        fail_emergency_containment: bool,
    }

    impl FakeBackend {
        fn new(cake: &Path, sqm: &Path) -> Self {
            Self {
                cake: cake.to_path_buf(),
                sqm: sqm.to_path_buf(),
                events: Vec::new(),
                attest_count: 0,
                restart_count: 0,
                fail_attest: false,
                drift_original_on_second_attest: false,
                drift_sqm_on_second_attest: false,
                fail_apply_after_partial_write: false,
                fail_verify_applied: false,
                fail_rollback_restart: false,
                mutate_original_after_rollback_restart: false,
                runtime_only_restored_verification: false,
                fail_emergency_containment: false,
            }
        }
    }

    impl NativeApplyTransactionBackend for FakeBackend {
        fn candidate_already_applied(
            &mut self,
            _: &NativeApplyExecutionPlan,
        ) -> Result<bool, String> {
            Ok(fs::read(&self.cake).unwrap() == b"cake-after\n"
                && fs::read(&self.sqm).unwrap() == b"sqm-after\n")
        }

        fn attest_before_apply(&mut self, _: &NativeApplyExecutionPlan) -> Result<(), String> {
            self.events.push("attest-before");
            self.attest_count = self.attest_count.saturating_add(1);
            if self.fail_attest {
                Err("injected pre-attestation failure".to_string())
            } else {
                if self.attest_count == 2 && self.drift_original_on_second_attest {
                    fs::write(&self.cake, b"foreign-before-mutation\n").unwrap();
                }
                if self.attest_count == 2 && self.drift_sqm_on_second_attest {
                    fs::write(&self.sqm, b"foreign-sqm-before-mutation\n").unwrap();
                }
                Ok(())
            }
        }

        fn materialize_candidate(
            &mut self,
            plan: &NativeApplyExecutionPlan,
            _: &NativeApplyConfigPairSnapshot,
        ) -> Result<NativeApplyCandidateMaterialization, String> {
            self.events.push("uci-batch");
            let batch = canonical_native_uci_batch(plan)?;
            assert!(batch
                .windows(b"commit cake-autorate\n".len())
                .any(|window| window == b"commit cake-autorate\n"));
            assert!(batch.ends_with(b"commit cake-autorate\n") || batch.ends_with(b"commit sqm\n"));
            if self.fail_apply_after_partial_write {
                return Err("injected candidate materialization failure".to_string());
            }
            NativeApplyCandidateMaterialization::new(
                b"cake-after\n".to_vec(),
                b"sqm-after\n".to_vec(),
                batch,
            )
        }

        fn reconstruct_legacy_candidate(
            &mut self,
            request: &OperationRequest,
            manifest: &[u8],
            _: &NativeApplyConfigPairSnapshot,
        ) -> Result<NativeApplyCandidateMaterialization, String> {
            self.events.push("reconstruct-legacy");
            let (_, authority) = legacy_native_apply_materialization(manifest, request)?;
            NativeApplyCandidateMaterialization::new(
                b"cake-after\n".to_vec(),
                b"sqm-after\n".to_vec(),
                authority,
            )
        }

        fn restart_service(
            &mut self,
            _: &OperationRequest,
            _: &NativeApplyGlobalLock,
        ) -> Result<(), String> {
            self.restart_count += 1;
            if self.restart_count == 1 {
                self.events.push("restart-applied");
            } else {
                self.events.push("restart-restored");
                if self.fail_rollback_restart {
                    return Err("injected rollback restart failure".to_string());
                }
                if self.mutate_original_after_rollback_restart {
                    fs::write(&self.cake, b"restart-modified-original\n").unwrap();
                }
            }
            Ok(())
        }

        fn verify_applied(&mut self, _: &NativeApplyExecutionPlan) -> Result<(), String> {
            self.events.push("verify-applied");
            if self.fail_verify_applied {
                return Err("injected applied verification failure".to_string());
            }
            if fs::read(&self.cake).unwrap() != b"cake-after\n"
                || fs::read(&self.sqm).unwrap() != b"sqm-after\n"
            {
                return Err("fake applied configs are not exact".to_string());
            }
            Ok(())
        }

        fn discard_pending_uci_changes(&mut self) -> Result<(), String> {
            Ok(())
        }

        fn emergency_contain(
            &mut self,
            _: &OperationRequest,
            _: &NativeApplyGlobalLock,
            authorities: &[NativeApplyConfigPairSnapshot],
        ) -> Result<(), String> {
            self.events.push("emergency-contain");
            assert!(!authorities.is_empty());
            if self.fail_emergency_containment {
                Err("injected emergency containment failure".to_string())
            } else {
                Ok(())
            }
        }

        fn verify_restored(
            &mut self,
            _: &OperationRequest,
            _: &NativeApplyRecoveryRecord,
        ) -> Result<(), String> {
            self.events.push("verify-restored");
            if self.runtime_only_restored_verification {
                return Ok(());
            }
            if fs::read(&self.cake).unwrap() != b"cake-before\n"
                || fs::read(&self.sqm).unwrap() != b"sqm-before\n"
            {
                return Err("fake restored configs are not exact".to_string());
            }
            Ok(())
        }

        fn verify_recovered_candidate(
            &mut self,
            _: &OperationRequest,
            record: &NativeApplyRecoveryRecord,
        ) -> Result<(), String> {
            self.events.push("verify-recovered-candidate");
            if record.state != NativeApplyRecoveryState::CommitAccepted
                || fs::read(&self.cake).unwrap() != b"cake-after\n"
                || fs::read(&self.sqm).unwrap() != b"sqm-after\n"
            {
                return Err("fake recovered candidate is not exact".to_string());
            }
            Ok(())
        }
    }

    fn transaction_fixture(name: &str) -> (PathBuf, PathBuf, PathBuf, PathBuf, PathBuf) {
        let root = temp_root(name);
        let configs = root.join("configs");
        fs::create_dir_all(&configs).unwrap();
        let cake = configs.join("cake-autorate");
        let sqm = configs.join("sqm");
        write_config(&cake, b"cake-before\n", 0o640);
        write_config(&sqm, b"sqm-before\n", 0o644);
        let recovery = root.join("recovery");
        let lock = root.join("locks/runtime.guard");
        (root, cake, sqm, recovery, lock)
    }

    fn acquire_test_lock_after_fork_window(path: &Path) -> NativeApplyGlobalLock {
        // Rust's parallel test process also exercises fork/exec paths.  A child
        // forked by another test can briefly retain this O_CLOEXEC descriptor
        // until exec closes it, even after the owning RAII guard has dropped.
        // Keep production acquisition non-blocking, but allow that bounded
        // cross-test fork window here.  A real leak remains a hard failure.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match NativeApplyGlobalLock::acquire(path) {
                Ok(lock) => return lock,
                Err(error) if error.contains("is busy") && std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(error) => panic!("native Apply test lock did not become available: {error}"),
            }
        }
    }

    #[test]
    fn forced_rollback_has_no_commit_success_branch() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("forced-rollback");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let mut backend = FakeBackend::new(&cake, &sqm);
        let receipt = execute_native_apply_forced_rollback(
            &plan,
            &manifest,
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
        )
        .unwrap();
        assert!(receipt.apply_verified);
        assert!(receipt.rollback_verified);
        assert!(receipt.recovery_cleared);
        assert_eq!(fs::read(&cake).unwrap(), b"cake-before\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-before\n");
        assert_eq!(
            backend.events,
            [
                "attest-before",
                "uci-batch",
                "attest-before",
                "restart-applied",
                "verify-applied",
                "restart-restored",
                "verify-restored"
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn schema_v4_absent_plan_is_rejected_before_backend_or_recovery_mutation() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("absent-v4-rejected");
        let plan = absent_plan();
        assert!(plan.request.validate_admission_policy().is_err());
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let mut backend = FakeBackend::new(&cake, &sqm);
        let expected = "native Apply schema-v4 runtime cannot execute absent-bootstrap authority";

        assert_eq!(canonical_native_uci_batch(&plan).unwrap_err(), expected);
        assert_eq!(
            NativeApplyRecoveryStore::new(&recovery)
                .prepare(&plan, &manifest, &cake, &sqm)
                .unwrap_err(),
            expected
        );
        for forced_rollback in [false, true] {
            let result = if forced_rollback {
                execute_native_apply_forced_rollback(
                    &plan,
                    &manifest,
                    NativeApplyTransactionPaths {
                        recovery_root: &recovery,
                        global_lock: &lock,
                        cake_config: &cake,
                        sqm_config: &sqm,
                    },
                    &mut backend,
                )
                .map(|_| ())
            } else {
                execute_native_apply_commit(
                    &plan,
                    &manifest,
                    NativeApplyTransactionPaths {
                        recovery_root: &recovery,
                        global_lock: &lock,
                        cake_config: &cake,
                        sqm_config: &sqm,
                    },
                    &mut backend,
                )
                .map(|_| ())
            };
            assert_eq!(result.unwrap_err(), expected);
        }

        assert!(backend.events.is_empty());
        assert_eq!(fs::read(&cake).unwrap(), b"cake-before\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-before\n");
        assert!(!recovery.exists());
        assert!(!lock.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn crafted_schema_v4_absent_recovery_is_rejected_before_backend_calls() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("absent-v4-recovery-rejected");
        let existing = plan();
        let manifest = existing.canonical_manifest_bytes().unwrap();
        let store = NativeApplyRecoveryStore::new(&recovery);
        let mut record = store.prepare(&existing, &manifest, &cake, &sqm).unwrap();

        let absent = absent_plan();
        let absent_request = absent.request.encode().unwrap().into_bytes();
        fs::write(store.current_path().join(REQUEST_FILE), &absent_request).unwrap();
        record.request_sha256 = sqm_identity::sha256sum(&absent_request).unwrap();
        fs::write(
            store.current_path().join(STATE_FILE),
            record.encode().unwrap(),
        )
        .unwrap();

        let mut backend = FakeBackend::new(&cake, &sqm);
        let error = recover_native_apply(
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
        )
        .unwrap_err();

        assert_eq!(
            error,
            "native Apply schema-v4 runtime cannot execute absent-bootstrap authority"
        );
        assert!(backend.events.is_empty());
        assert_eq!(fs::read(&cake).unwrap(), b"cake-before\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-before\n");
        assert!(store.read_record().unwrap().is_some());
        assert!(!lock.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_schema_v4_recovery_is_rejected_before_lock_or_backend() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("malformed-v4-preflight");
        let existing = plan();
        let manifest = existing.canonical_manifest_bytes().unwrap();
        let store = NativeApplyRecoveryStore::new(&recovery);
        let mut record = store.prepare(&existing, &manifest, &cake, &sqm).unwrap();
        let malformed_request = b"not-a-request\n";
        fs::write(store.current_path().join(REQUEST_FILE), malformed_request).unwrap();
        record.request_sha256 = sqm_identity::sha256sum(malformed_request).unwrap();
        fs::write(
            store.current_path().join(STATE_FILE),
            record.encode().unwrap(),
        )
        .unwrap();

        let mut backend = FakeBackend::new(&cake, &sqm);
        let error = recover_native_apply(
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
        )
        .unwrap_err();

        assert!(error.contains("operation record header mismatch"));
        assert!(backend.events.is_empty());
        assert!(!lock.exists());
        assert_eq!(fs::read(&cake).unwrap(), b"cake-before\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-before\n");
        assert!(store.read_record().unwrap().is_some());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn candidate_phase_rejects_absent_and_premature_existing_evidence() {
        let (root, cake, sqm, recovery, _lock) = transaction_fixture("candidate-phase-binding");
        let existing = plan();
        let existing_manifest = existing.canonical_manifest_bytes().unwrap();
        let store = NativeApplyRecoveryStore::new(&recovery);
        let mut record = store
            .prepare(&existing, &existing_manifest, &cake, &sqm)
            .unwrap();
        let absent_request = absent_plan().request;
        assert!(record
            .validate_candidate_phase(&absent_request)
            .unwrap_err()
            .contains("schema-v4 runtime cannot execute absent-bootstrap authority"));
        let digest = "ab".repeat(32);
        record.candidate_cake_sha256 = Some(digest.clone());
        record.candidate_sqm_sha256 = Some(digest);
        record.candidate_cake_mode = Some(record.cake_mode);
        record.candidate_sqm_mode = Some(record.sqm_mode);
        assert!(record.validate().is_ok());
        assert!(record
            .validate_candidate_phase(&existing.request)
            .unwrap_err()
            .contains("existing-managed"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn production_commit_is_exact_and_a_duplicate_is_a_read_only_success() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("production-commit");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let mut backend = FakeBackend::new(&cake, &sqm);
        let receipt = execute_native_apply_commit(
            &plan,
            &manifest,
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
        )
        .unwrap();
        assert_eq!(receipt.disposition, NativeApplyCommitDisposition::Applied);
        assert!(receipt.recovery_cleared);
        assert_eq!(fs::read(&cake).unwrap(), b"cake-after\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-after\n");
        let first_events = backend.events.clone();

        // A process forked by a parallel test can retain this test's lock fd
        // until exec closes O_CLOEXEC descriptors.  The production call must
        // remain non-blocking, but the duplicate regression may retry only
        // that pre-mutation Busy result during the bounded test fork window.
        let duplicate_deadline = std::time::Instant::now() + Duration::from_secs(2);
        let duplicate = loop {
            match execute_native_apply_commit(
                &plan,
                &manifest,
                NativeApplyTransactionPaths {
                    recovery_root: &recovery,
                    global_lock: &lock,
                    cake_config: &cake,
                    sqm_config: &sqm,
                },
                &mut backend,
            ) {
                Ok(receipt) => break receipt,
                Err(error)
                    if error.contains("is busy")
                        && std::time::Instant::now() < duplicate_deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("native Apply duplicate commit failed: {error}"),
            }
        };
        assert_eq!(
            duplicate.disposition,
            NativeApplyCommitDisposition::AlreadyApplied
        );
        assert_eq!(backend.events, first_events);
        assert!(!recovery.join(CURRENT_DIRECTORY).exists());
        assert!(!recovery.join(COMPLETED_DIRECTORY).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn production_fault_before_commit_marker_rolls_back() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("fault-before-commit");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let mut backend = FakeBackend::new(&cake, &sqm);
        let error = execute_native_apply_commit_with_fault(
            &plan,
            &manifest,
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
            NativeApplyLabFaultInjection::PauseAfterVerifiedBeforeCommit {
                timeout: Duration::from_millis(1),
            },
        )
        .unwrap_err();
        assert!(error.contains("verified-before-commit gate timed out"));
        assert!(error.contains("exact rollback verified"));
        assert_eq!(fs::read(&cake).unwrap(), b"cake-before\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-before\n");
        assert!(!recovery.join(CURRENT_DIRECTORY).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn production_fault_after_commit_marker_preserves_foreign_state_and_commit_intent() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("fault-after-commit");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let mut backend = FakeBackend::new(&cake, &sqm);
        let error = execute_native_apply_commit_with_fault(
            &plan,
            &manifest,
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
            NativeApplyLabFaultInjection::PauseAfterCommitAccepted {
                timeout: Duration::from_millis(1),
            },
        )
        .unwrap_err();
        assert!(error.contains("commit was durably accepted"));
        let store = NativeApplyRecoveryStore::new(&recovery);
        assert_eq!(
            store.read_record().unwrap().unwrap().state,
            NativeApplyRecoveryState::CommitAccepted
        );
        assert_eq!(fs::read(&cake).unwrap(), b"cake-after\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-after\n");

        fs::write(&cake, b"torn-after-marker\n").unwrap();
        let mut recovery_backend = FakeBackend::new(&cake, &sqm);
        let recovery_error = recover_native_apply(
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut recovery_backend,
        )
        .unwrap_err();
        assert!(recovery_error.contains("recovery remains pending"));
        assert!(recovery_error.contains("roll-forward found foreign config state"));
        assert_eq!(fs::read(&cake).unwrap(), b"torn-after-marker\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-after\n");
        assert_eq!(
            store.read_record().unwrap().unwrap().state,
            NativeApplyRecoveryState::CommitAccepted
        );
        assert_eq!(recovery_backend.events, ["emergency-contain"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_rolls_forward_only_after_the_durable_commit_marker() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("roll-forward");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let store = NativeApplyRecoveryStore::new(&recovery);
        store.prepare(&plan, &manifest, &cake, &sqm).unwrap();
        store
            .transition(
                NativeApplyRecoveryState::Prepared,
                NativeApplyRecoveryState::MutationStarted,
            )
            .unwrap();
        fs::write(&cake, b"cake-after\n").unwrap();
        fs::write(&sqm, b"sqm-after\n").unwrap();
        let restarted = store
            .transition(
                NativeApplyRecoveryState::MutationStarted,
                NativeApplyRecoveryState::ServiceRestarted,
            )
            .unwrap();
        let snapshot = store.stage_candidate(&cake, &sqm, &restarted).unwrap();
        store
            .transition(
                NativeApplyRecoveryState::ServiceRestarted,
                NativeApplyRecoveryState::Verified,
            )
            .unwrap();
        let accepted = store.accept_candidate(&cake, &sqm, &snapshot).unwrap();
        assert_eq!(accepted.state, NativeApplyRecoveryState::CommitAccepted);
        fs::write(&cake, b"cake-before\n").unwrap();

        let mut backend = FakeBackend::new(&cake, &sqm);
        let receipt = recover_native_apply(
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
        )
        .unwrap()
        .unwrap();
        assert!(receipt.rolled_forward);
        assert!(receipt.recovery_cleared);
        assert_eq!(fs::read(&cake).unwrap(), b"cake-after\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-after\n");
        assert_eq!(
            backend.events,
            ["restart-applied", "verify-recovered-candidate"]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn candidate_snapshots_without_commit_marker_are_rollback_only() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("pre-marker-crash");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let store = NativeApplyRecoveryStore::new(&recovery);
        store.prepare(&plan, &manifest, &cake, &sqm).unwrap();
        store
            .transition(
                NativeApplyRecoveryState::Prepared,
                NativeApplyRecoveryState::MutationStarted,
            )
            .unwrap();
        let restarted = store
            .transition(
                NativeApplyRecoveryState::MutationStarted,
                NativeApplyRecoveryState::ServiceRestarted,
            )
            .unwrap();
        fs::write(&cake, b"unaccepted-cake\n").unwrap();
        fs::write(&sqm, b"unaccepted-sqm\n").unwrap();
        store.stage_candidate(&cake, &sqm, &restarted).unwrap();
        store
            .transition(
                NativeApplyRecoveryState::ServiceRestarted,
                NativeApplyRecoveryState::Verified,
            )
            .unwrap();

        let mut backend = FakeBackend::new(&cake, &sqm);
        let receipt = recover_native_apply(
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
        )
        .unwrap()
        .unwrap();
        assert!(!receipt.rolled_forward);
        assert_eq!(fs::read(&cake).unwrap(), b"cake-before\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-before\n");
        assert!(!recovery.join(CURRENT_DIRECTORY).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn crash_gate_parks_only_after_durable_service_restart_and_times_out_closed() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("crash-gate-timeout");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let mut backend = FakeBackend::new(&cake, &sqm);
        let error = execute_native_apply_forced_rollback_with_fault(
            &plan,
            &manifest,
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
            NativeApplyLabFaultInjection::PauseAfterServiceRestarted {
                timeout: Duration::from_millis(1),
            },
        )
        .unwrap_err();
        assert!(error.contains("crash-after-service-restarted gate timed out without SIGKILL"));
        assert!(error.contains("exact rollback verified"));
        assert_eq!(
            backend.events,
            [
                "attest-before",
                "uci-batch",
                "attest-before",
                "restart-applied",
                "restart-restored",
                "verify-restored"
            ]
        );
        assert_eq!(fs::read(&cake).unwrap(), b"cake-before\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-before\n");
        assert!(NativeApplyRecoveryStore::new(&recovery)
            .read_record()
            .unwrap()
            .is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn materialization_failure_precedes_journal_and_live_mutation() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("partial-uci");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let mut backend = FakeBackend::new(&cake, &sqm);
        backend.fail_apply_after_partial_write = true;
        let error = execute_native_apply_forced_rollback(
            &plan,
            &manifest,
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
        )
        .unwrap_err();
        assert!(error.contains("candidate materialization failure"));
        assert_eq!(fs::read(&cake).unwrap(), b"cake-before\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-before\n");
        assert!(NativeApplyRecoveryStore::new(&recovery)
            .read_record()
            .unwrap()
            .is_none());
        assert_eq!(backend.events, ["attest-before", "uci-batch"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn write_ahead_mixed_pair_recovers_without_foreign_state_guessing() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("write-ahead-mixed-pair");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let original = NativeApplyConfigPairSnapshot::capture(&cake, &sqm).unwrap();
        let candidate = NativeApplyCandidateMaterialization::new(
            b"cake-after\n".to_vec(),
            b"sqm-after\n".to_vec(),
            canonical_native_uci_batch(&plan).unwrap(),
        )
        .unwrap();
        let store = NativeApplyRecoveryStore::new(&recovery);
        store
            .prepare_write_ahead(&plan, &manifest, &original, &candidate)
            .unwrap();
        let mutation = store.begin_mutation(&cake, &sqm).unwrap();
        atomic_restore(&cake, candidate.cake(), original.cake_mode).unwrap();
        drop(mutation);
        assert_eq!(fs::read(&cake).unwrap(), b"cake-after\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-before\n");

        let mut backend = FakeBackend::new(&cake, &sqm);
        let receipt = recover_native_apply(
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
        )
        .unwrap()
        .unwrap();
        assert!(!receipt.rolled_forward);
        assert!(receipt.recovery_cleared);
        assert_eq!(fs::read(&cake).unwrap(), b"cake-before\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-before\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum WriteAheadCrashBoundary {
        PreparedPublished,
        MutationStarted,
        CakeTemporarySynced,
        CakeRenamed,
        SqmTemporarySynced,
        CandidatePairRenamed,
        ServiceRestarted,
        Verified,
    }

    #[test]
    fn every_precommit_write_ahead_crash_boundary_restores_without_residue() {
        for (label, boundary) in [
            ("prepared", WriteAheadCrashBoundary::PreparedPublished),
            ("mutation-started", WriteAheadCrashBoundary::MutationStarted),
            ("cake-temp", WriteAheadCrashBoundary::CakeTemporarySynced),
            ("cake-renamed", WriteAheadCrashBoundary::CakeRenamed),
            ("sqm-temp", WriteAheadCrashBoundary::SqmTemporarySynced),
            (
                "pair-renamed",
                WriteAheadCrashBoundary::CandidatePairRenamed,
            ),
            (
                "service-restarted",
                WriteAheadCrashBoundary::ServiceRestarted,
            ),
            ("verified", WriteAheadCrashBoundary::Verified),
        ] {
            let (root, cake, sqm, recovery, lock) =
                transaction_fixture(&format!("crash-boundary-{label}"));
            let plan = plan();
            let manifest = plan.canonical_manifest_bytes().unwrap();
            let original = NativeApplyConfigPairSnapshot::capture(&cake, &sqm).unwrap();
            let candidate = NativeApplyCandidateMaterialization::new(
                b"cake-after\n".to_vec(),
                b"sqm-after\n".to_vec(),
                canonical_native_uci_batch(&plan).unwrap(),
            )
            .unwrap();
            let store = NativeApplyRecoveryStore::new(&recovery);
            store
                .prepare_write_ahead(&plan, &manifest, &original, &candidate)
                .unwrap();

            let mut mutation = if boundary == WriteAheadCrashBoundary::PreparedPublished {
                None
            } else {
                Some(store.begin_mutation(&cake, &sqm).unwrap())
            };
            match boundary {
                WriteAheadCrashBoundary::PreparedPublished
                | WriteAheadCrashBoundary::MutationStarted => {}
                WriteAheadCrashBoundary::CakeTemporarySynced => {
                    let (_, temporary) = restore_temporary_path(&cake).unwrap();
                    write_config(&temporary, candidate.cake(), original.cake_mode);
                }
                WriteAheadCrashBoundary::CakeRenamed => {
                    atomic_restore(&cake, candidate.cake(), original.cake_mode).unwrap();
                }
                WriteAheadCrashBoundary::SqmTemporarySynced => {
                    atomic_restore(&cake, candidate.cake(), original.cake_mode).unwrap();
                    let (_, temporary) = restore_temporary_path(&sqm).unwrap();
                    write_config(&temporary, candidate.sqm(), original.sqm_mode);
                }
                WriteAheadCrashBoundary::CandidatePairRenamed
                | WriteAheadCrashBoundary::ServiceRestarted
                | WriteAheadCrashBoundary::Verified => {
                    mutation
                        .as_ref()
                        .unwrap()
                        .install_candidate_files()
                        .unwrap();
                    if matches!(
                        boundary,
                        WriteAheadCrashBoundary::ServiceRestarted
                            | WriteAheadCrashBoundary::Verified
                    ) {
                        store
                            .transition(
                                NativeApplyRecoveryState::MutationStarted,
                                NativeApplyRecoveryState::ServiceRestarted,
                            )
                            .unwrap();
                    }
                    if boundary == WriteAheadCrashBoundary::Verified {
                        store
                            .transition(
                                NativeApplyRecoveryState::ServiceRestarted,
                                NativeApplyRecoveryState::Verified,
                            )
                            .unwrap();
                    }
                }
            }
            drop(mutation.take());

            let mut backend = FakeBackend::new(&cake, &sqm);
            let receipt = recover_native_apply(
                NativeApplyTransactionPaths {
                    recovery_root: &recovery,
                    global_lock: &lock,
                    cake_config: &cake,
                    sqm_config: &sqm,
                },
                &mut backend,
            )
            .unwrap()
            .unwrap();
            assert!(!receipt.rolled_forward, "{label}");
            assert!(receipt.recovery_cleared, "{label}");
            assert_eq!(fs::read(&cake).unwrap(), original.cake(), "{label}");
            assert_eq!(fs::read(&sqm).unwrap(), original.sqm(), "{label}");
            assert!(
                !restore_temporary_path(&cake).unwrap().1.exists(),
                "{label}"
            );
            assert!(!restore_temporary_path(&sqm).unwrap().1.exists(), "{label}");
            assert!(store.read_record().unwrap().is_none(), "{label}");
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn write_ahead_rollback_accepts_all_original_candidate_pair_combinations() {
        for (label, cake_is_candidate, sqm_is_candidate) in [
            ("oo", false, false),
            ("oc", false, true),
            ("co", true, false),
            ("cc", true, true),
        ] {
            let (root, cake, sqm, recovery, lock) =
                transaction_fixture(&format!("write-ahead-{label}"));
            let plan = plan();
            let manifest = plan.canonical_manifest_bytes().unwrap();
            let original = NativeApplyConfigPairSnapshot::capture(&cake, &sqm).unwrap();
            let candidate = NativeApplyCandidateMaterialization::new(
                b"cake-after\n".to_vec(),
                b"sqm-after\n".to_vec(),
                canonical_native_uci_batch(&plan).unwrap(),
            )
            .unwrap();
            let store = NativeApplyRecoveryStore::new(&recovery);
            store
                .prepare_write_ahead(&plan, &manifest, &original, &candidate)
                .unwrap();
            drop(store.begin_mutation(&cake, &sqm).unwrap());
            if cake_is_candidate {
                fs::write(&cake, candidate.cake()).unwrap();
            }
            if sqm_is_candidate {
                fs::write(&sqm, candidate.sqm()).unwrap();
            }

            let mut backend = FakeBackend::new(&cake, &sqm);
            let receipt = recover_native_apply(
                NativeApplyTransactionPaths {
                    recovery_root: &recovery,
                    global_lock: &lock,
                    cake_config: &cake,
                    sqm_config: &sqm,
                },
                &mut backend,
            )
            .unwrap()
            .unwrap();
            assert!(
                !receipt.rolled_forward,
                "unexpected roll-forward for {label}"
            );
            assert_eq!(fs::read(&cake).unwrap(), b"cake-before\n", "{label}");
            assert_eq!(fs::read(&sqm).unwrap(), b"sqm-before\n", "{label}");
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn write_ahead_rollforward_accepts_all_original_candidate_pair_combinations() {
        for (label, cake_is_candidate, sqm_is_candidate) in [
            ("oo", false, false),
            ("oc", false, true),
            ("co", true, false),
            ("cc", true, true),
        ] {
            let (root, cake, sqm, recovery, lock) =
                transaction_fixture(&format!("write-ahead-forward-{label}"));
            let plan = plan();
            let manifest = plan.canonical_manifest_bytes().unwrap();
            let original = NativeApplyConfigPairSnapshot::capture(&cake, &sqm).unwrap();
            let candidate = NativeApplyCandidateMaterialization::new(
                b"cake-after\n".to_vec(),
                b"sqm-after\n".to_vec(),
                canonical_native_uci_batch(&plan).unwrap(),
            )
            .unwrap();
            let store = NativeApplyRecoveryStore::new(&recovery);
            store
                .prepare_write_ahead(&plan, &manifest, &original, &candidate)
                .unwrap();
            let mutation = store.begin_mutation(&cake, &sqm).unwrap();
            mutation.install_candidate_files().unwrap();
            store
                .transition(
                    NativeApplyRecoveryState::MutationStarted,
                    NativeApplyRecoveryState::ServiceRestarted,
                )
                .unwrap();
            drop(mutation);
            store
                .transition(
                    NativeApplyRecoveryState::ServiceRestarted,
                    NativeApplyRecoveryState::Verified,
                )
                .unwrap();
            store
                .transition(
                    NativeApplyRecoveryState::Verified,
                    NativeApplyRecoveryState::CommitAccepted,
                )
                .unwrap();
            fs::write(
                &cake,
                if cake_is_candidate {
                    candidate.cake()
                } else {
                    original.cake()
                },
            )
            .unwrap();
            fs::write(
                &sqm,
                if sqm_is_candidate {
                    candidate.sqm()
                } else {
                    original.sqm()
                },
            )
            .unwrap();

            let mut backend = FakeBackend::new(&cake, &sqm);
            let receipt = recover_native_apply(
                NativeApplyTransactionPaths {
                    recovery_root: &recovery,
                    global_lock: &lock,
                    cake_config: &cake,
                    sqm_config: &sqm,
                },
                &mut backend,
            )
            .unwrap()
            .unwrap();
            assert!(receipt.rolled_forward, "unexpected rollback for {label}");
            assert_eq!(fs::read(&cake).unwrap(), candidate.cake(), "{label}");
            assert_eq!(fs::read(&sqm).unwrap(), candidate.sqm(), "{label}");
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum WriteAheadForeignBoundary {
        MutationStarted,
        ServiceRestarted,
        Verified,
        CommitAccepted,
    }

    #[test]
    fn every_write_ahead_recovery_phase_refuses_foreign_live_bytes() {
        for (label, boundary) in [
            (
                "mutation-started",
                WriteAheadForeignBoundary::MutationStarted,
            ),
            (
                "service-restarted",
                WriteAheadForeignBoundary::ServiceRestarted,
            ),
            ("verified", WriteAheadForeignBoundary::Verified),
            ("commit-accepted", WriteAheadForeignBoundary::CommitAccepted),
        ] {
            let (root, cake, sqm, recovery, lock) =
                transaction_fixture(&format!("foreign-v5-{label}"));
            let plan = plan();
            let manifest = plan.canonical_manifest_bytes().unwrap();
            let original = NativeApplyConfigPairSnapshot::capture(&cake, &sqm).unwrap();
            let candidate = NativeApplyCandidateMaterialization::new(
                b"cake-after\n".to_vec(),
                b"sqm-after\n".to_vec(),
                canonical_native_uci_batch(&plan).unwrap(),
            )
            .unwrap();
            let store = NativeApplyRecoveryStore::new(&recovery);
            store
                .prepare_write_ahead(&plan, &manifest, &original, &candidate)
                .unwrap();
            let mutation = store.begin_mutation(&cake, &sqm).unwrap();
            if boundary != WriteAheadForeignBoundary::MutationStarted {
                mutation.install_candidate_files().unwrap();
                store
                    .transition(
                        NativeApplyRecoveryState::MutationStarted,
                        NativeApplyRecoveryState::ServiceRestarted,
                    )
                    .unwrap();
                if matches!(
                    boundary,
                    WriteAheadForeignBoundary::Verified | WriteAheadForeignBoundary::CommitAccepted
                ) {
                    store
                        .transition(
                            NativeApplyRecoveryState::ServiceRestarted,
                            NativeApplyRecoveryState::Verified,
                        )
                        .unwrap();
                }
                if boundary == WriteAheadForeignBoundary::CommitAccepted {
                    store
                        .transition(
                            NativeApplyRecoveryState::Verified,
                            NativeApplyRecoveryState::CommitAccepted,
                        )
                        .unwrap();
                }
            }
            drop(mutation);
            fs::write(&cake, b"foreign-cake\n").unwrap();
            let sqm_before_recovery = fs::read(&sqm).unwrap();

            let mut backend = FakeBackend::new(&cake, &sqm);
            let error = recover_native_apply(
                NativeApplyTransactionPaths {
                    recovery_root: &recovery,
                    global_lock: &lock,
                    cake_config: &cake,
                    sqm_config: &sqm,
                },
                &mut backend,
            )
            .unwrap_err();
            assert!(error.contains("foreign"), "{label}: {error}");
            assert_eq!(fs::read(&cake).unwrap(), b"foreign-cake\n", "{label}");
            assert_eq!(fs::read(&sqm).unwrap(), sqm_before_recovery, "{label}");
            let retained = store.read_record().unwrap().unwrap();
            assert_eq!(
                retained.state,
                if boundary == WriteAheadForeignBoundary::CommitAccepted {
                    NativeApplyRecoveryState::CommitAccepted
                } else {
                    NativeApplyRecoveryState::RollbackRequired
                },
                "{label}"
            );
            assert!(backend.events.contains(&"emergency-contain"), "{label}");
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn unsafe_stale_restore_temporary_keeps_recovery_pending() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("unsafe-restore-temp");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let original = NativeApplyConfigPairSnapshot::capture(&cake, &sqm).unwrap();
        let candidate = NativeApplyCandidateMaterialization::new(
            b"cake-after\n".to_vec(),
            b"sqm-after\n".to_vec(),
            canonical_native_uci_batch(&plan).unwrap(),
        )
        .unwrap();
        let store = NativeApplyRecoveryStore::new(&recovery);
        store
            .prepare_write_ahead(&plan, &manifest, &original, &candidate)
            .unwrap();
        drop(store.begin_mutation(&cake, &sqm).unwrap());
        let target = root.join("foreign-target");
        write_config(&target, b"foreign\n", 0o600);
        let temporary = restore_temporary_path(&cake).unwrap().1;
        std::os::unix::fs::symlink(&target, &temporary).unwrap();

        let mut backend = FakeBackend::new(&cake, &sqm);
        let error = recover_native_apply(
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
        )
        .unwrap_err();
        assert!(error.contains("stale restore temporary is not a regular file"));
        assert_eq!(fs::read(&target).unwrap(), b"foreign\n");
        assert_eq!(fs::read(&cake).unwrap(), original.cake());
        assert_eq!(fs::read(&sqm).unwrap(), original.sqm());
        assert_eq!(
            store.read_record().unwrap().unwrap().state,
            NativeApplyRecoveryState::RollbackRequired
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_v4_hybrid_pair_is_reconstructed_only_for_rollback() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("legacy-v4-hybrid");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let store = NativeApplyRecoveryStore::new(&recovery);
        store.prepare(&plan, &manifest, &cake, &sqm).unwrap();
        store
            .transition(
                NativeApplyRecoveryState::Prepared,
                NativeApplyRecoveryState::MutationStarted,
            )
            .unwrap();
        store
            .transition(
                NativeApplyRecoveryState::MutationStarted,
                NativeApplyRecoveryState::RollbackRequired,
            )
            .unwrap();
        fs::write(&cake, b"cake-after\n").unwrap();

        let mut backend = FakeBackend::new(&cake, &sqm);
        let receipt = recover_native_apply(
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
        )
        .unwrap()
        .unwrap();
        assert!(!receipt.rolled_forward);
        assert!(receipt.recovery_cleared);
        assert_eq!(fs::read(&cake).unwrap(), b"cake-before\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-before\n");
        assert_eq!(
            backend.events,
            ["reconstruct-legacy", "restart-applied", "verify-restored"]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_manifest_reconstruction_is_exact_and_tamper_evident() {
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let (mutations, authority) =
            legacy_native_apply_materialization(&manifest, &plan.request).unwrap();
        assert_eq!(mutations, plan.uci_mutations);
        assert_eq!(authority, canonical_native_uci_batch(&plan).unwrap());

        let mut foreign_request = plan.request.clone();
        foreign_request.identity.job_id = "12".repeat(16);
        assert!(
            legacy_native_apply_materialization(&manifest, &foreign_request)
                .unwrap_err()
                .contains("job_id binding")
        );
        assert!(require_canonical_manifest_string_field(
            &manifest,
            "worker_run_id",
            &"34".repeat(16),
        )
        .unwrap_err()
        .contains("worker_run_id binding"));
        assert!(
            require_canonical_manifest_string_field(&manifest, "option_id", "foreign_option",)
                .unwrap_err()
                .contains("option_id binding")
        );

        let mut tampered = manifest.clone();
        let needle = b"\"uci_mutations\":[";
        let position = tampered
            .windows(needle.len())
            .position(|window| window == needle)
            .unwrap();
        tampered[position] = b'X';
        assert!(legacy_native_apply_materialization(&tampered, &plan.request).is_err());
    }

    #[test]
    fn failed_rollback_retains_durable_state_and_is_idempotently_resumable() {
        let (root, cake, sqm, recovery, lock_path) = transaction_fixture("resume-rollback");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let mut backend = FakeBackend::new(&cake, &sqm);
        backend.fail_verify_applied = true;
        backend.fail_rollback_restart = true;
        let error = execute_native_apply_forced_rollback(
            &plan,
            &manifest,
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock_path,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
        )
        .unwrap_err();
        assert!(error.contains("rollback remains pending"));
        assert_eq!(fs::read(&cake).unwrap(), b"cake-before\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-before\n");
        let store = NativeApplyRecoveryStore::new(&recovery);
        assert_eq!(
            store.read_record().unwrap().unwrap().state,
            NativeApplyRecoveryState::RollbackRequired
        );
        assert!(backend.events.contains(&"emergency-contain"));

        backend.fail_rollback_restart = false;
        let lock = acquire_test_lock_after_fork_window(&lock_path);
        rollback_native_apply(&plan.request, &store, &lock, &cake, &sqm, &mut backend).unwrap();
        assert!(store.read_record().unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pre_attestation_failure_creates_no_recovery_or_config_change() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("pre-attest");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let mut backend = FakeBackend::new(&cake, &sqm);
        backend.fail_attest = true;
        assert!(execute_native_apply_forced_rollback(
            &plan,
            &manifest,
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
        )
        .unwrap_err()
        .contains("pre-attestation"));
        assert_eq!(fs::read(&cake).unwrap(), b"cake-before\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-before\n");
        assert!(!recovery.join(CURRENT_DIRECTORY).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_native_recovery_is_a_read_only_noop() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("empty-native-recovery");
        let mut backend = FakeBackend::new(&cake, &sqm);

        let receipt = recover_native_apply(
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
        )
        .unwrap();

        assert!(receipt.is_none());
        assert!(backend.events.is_empty());
        assert_eq!(fs::read(&cake).unwrap(), b"cake-before\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-before\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prepared_backup_drift_is_rejected_without_overwriting_the_foreign_change() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("prepared-drift");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let mut backend = FakeBackend::new(&cake, &sqm);
        backend.drift_original_on_second_attest = true;

        let error = execute_native_apply_commit(
            &plan,
            &manifest,
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
        )
        .unwrap_err();

        assert!(error.contains("original cake config digest mismatch"));
        assert_eq!(fs::read(&cake).unwrap(), b"foreign-before-mutation\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-before\n");
        assert_eq!(
            backend.events,
            ["attest-before", "uci-batch", "attest-before"]
        );
        assert_eq!(backend.restart_count, 0);
        assert!(NativeApplyRecoveryStore::new(&recovery)
            .read_record()
            .unwrap()
            .is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prepared_sqm_backup_drift_is_rejected_without_overwriting_the_foreign_change() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("prepared-sqm-drift");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let mut backend = FakeBackend::new(&cake, &sqm);
        backend.drift_sqm_on_second_attest = true;

        let error = execute_native_apply_commit(
            &plan,
            &manifest,
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
        )
        .unwrap_err();

        assert!(error.contains("original SQM config digest mismatch"));
        assert_eq!(fs::read(&cake).unwrap(), b"cake-before\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"foreign-sqm-before-mutation\n");
        assert_eq!(
            backend.events,
            ["attest-before", "uci-batch", "attest-before"]
        );
        assert_eq!(backend.restart_count, 0);
        assert!(NativeApplyRecoveryStore::new(&recovery)
            .read_record()
            .unwrap()
            .is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rollback_keeps_recovery_when_restart_changes_an_original_config() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("rollback-restart-drift");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let mut backend = FakeBackend::new(&cake, &sqm);
        backend.mutate_original_after_rollback_restart = true;
        backend.runtime_only_restored_verification = true;

        let error = execute_native_apply_forced_rollback(
            &plan,
            &manifest,
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
        )
        .unwrap_err();

        assert!(error.contains("rollback failed"));
        assert!(error.contains("original cake config digest mismatch"));
        assert_eq!(fs::read(&cake).unwrap(), b"restart-modified-original\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-before\n");
        assert_eq!(
            NativeApplyRecoveryStore::new(&recovery)
                .read_record()
                .unwrap()
                .unwrap()
                .state,
            NativeApplyRecoveryState::RollbackRequired
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn commit_marker_rejects_live_config_drift_after_candidate_staging() {
        let (root, cake, sqm, recovery, _) = transaction_fixture("candidate-live-drift");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let store = NativeApplyRecoveryStore::new(&recovery);
        store.prepare(&plan, &manifest, &cake, &sqm).unwrap();
        store
            .transition(
                NativeApplyRecoveryState::Prepared,
                NativeApplyRecoveryState::MutationStarted,
            )
            .unwrap();
        fs::write(&cake, b"cake-after\n").unwrap();
        fs::write(&sqm, b"sqm-after\n").unwrap();
        let restarted = store
            .transition(
                NativeApplyRecoveryState::MutationStarted,
                NativeApplyRecoveryState::ServiceRestarted,
            )
            .unwrap();
        let snapshot = store.stage_candidate(&cake, &sqm, &restarted).unwrap();
        store
            .transition(
                NativeApplyRecoveryState::ServiceRestarted,
                NativeApplyRecoveryState::Verified,
            )
            .unwrap();
        fs::write(&cake, b"foreign-after-staging\n").unwrap();

        assert!(store
            .accept_candidate(&cake, &sqm, &snapshot)
            .unwrap_err()
            .contains("staged candidate cake config digest mismatch"));
        assert_eq!(
            store.read_record().unwrap().unwrap().state,
            NativeApplyRecoveryState::Verified
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_retry_resumes_commit_accepted_roll_forward() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("exact-retry-roll-forward");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let store = NativeApplyRecoveryStore::new(&recovery);
        store.prepare(&plan, &manifest, &cake, &sqm).unwrap();
        store
            .transition(
                NativeApplyRecoveryState::Prepared,
                NativeApplyRecoveryState::MutationStarted,
            )
            .unwrap();
        fs::write(&cake, b"cake-after\n").unwrap();
        fs::write(&sqm, b"sqm-after\n").unwrap();
        let restarted = store
            .transition(
                NativeApplyRecoveryState::MutationStarted,
                NativeApplyRecoveryState::ServiceRestarted,
            )
            .unwrap();
        let snapshot = store.stage_candidate(&cake, &sqm, &restarted).unwrap();
        store
            .transition(
                NativeApplyRecoveryState::ServiceRestarted,
                NativeApplyRecoveryState::Verified,
            )
            .unwrap();
        store.accept_candidate(&cake, &sqm, &snapshot).unwrap();
        fs::write(&cake, b"cake-before\n").unwrap();

        let mut backend = FakeBackend::new(&cake, &sqm);
        let receipt = execute_native_apply_commit(
            &plan,
            &manifest,
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
        )
        .unwrap();

        assert_eq!(
            receipt.disposition,
            NativeApplyCommitDisposition::AlreadyApplied
        );
        assert!(receipt.recovery_cleared);
        assert_eq!(fs::read(&cake).unwrap(), b"cake-after\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-after\n");
        assert_eq!(
            backend.events,
            [
                "restart-applied",
                "verify-recovered-candidate",
                "verify-applied"
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn durable_recovery_binds_the_exact_selected_option() {
        let (root, cake, sqm, recovery, _lock) = transaction_fixture("durable-selected-option");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let store = NativeApplyRecoveryStore::new(&recovery);
        let prepared = store.prepare(&plan, &manifest, &cake, &sqm).unwrap();

        assert_eq!(prepared.option_id, "recommended");
        let state = fs::read_to_string(store.current_path().join(STATE_FILE)).unwrap();
        assert!(state.contains("\noption_id=recommended\n"));
        assert_eq!(store.read_record().unwrap().unwrap(), prepared);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retry_rejects_a_different_option_even_with_the_same_request() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("foreign-option");
        let original_plan = plan();
        let original_manifest = original_plan.canonical_manifest_bytes().unwrap();
        let store = NativeApplyRecoveryStore::new(&recovery);
        store
            .prepare(&original_plan, &original_manifest, &cake, &sqm)
            .unwrap();

        let mut different_plan = original_plan.clone();
        different_plan.option_id = "quality_first".to_string();
        let different_manifest = different_plan.canonical_manifest_bytes().unwrap();
        let mut backend = FakeBackend::new(&cake, &sqm);
        let error = execute_native_apply_commit(
            &different_plan,
            &different_manifest,
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
        )
        .unwrap_err();

        assert!(error.contains("foreign native Apply recovery transaction"));
        assert_eq!(
            store.read_record().unwrap().unwrap().option_id,
            "recommended"
        );
        assert_eq!(fs::read(&cake).unwrap(), b"cake-before\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-before\n");
        assert!(backend.events.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_retry_rolls_back_a_precommit_transaction_then_reapplies() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("exact-retry-precommit");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let store = NativeApplyRecoveryStore::new(&recovery);
        store.prepare(&plan, &manifest, &cake, &sqm).unwrap();
        store
            .transition(
                NativeApplyRecoveryState::Prepared,
                NativeApplyRecoveryState::MutationStarted,
            )
            .unwrap();
        fs::write(&cake, b"cake-after\n").unwrap();
        fs::write(&sqm, b"sqm-after\n").unwrap();
        let restarted = store
            .transition(
                NativeApplyRecoveryState::MutationStarted,
                NativeApplyRecoveryState::ServiceRestarted,
            )
            .unwrap();
        store.stage_candidate(&cake, &sqm, &restarted).unwrap();
        store
            .transition(
                NativeApplyRecoveryState::ServiceRestarted,
                NativeApplyRecoveryState::Verified,
            )
            .unwrap();

        let mut backend = FakeBackend::new(&cake, &sqm);
        let receipt = execute_native_apply_commit(
            &plan,
            &manifest,
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
        )
        .unwrap();

        assert_eq!(receipt.disposition, NativeApplyCommitDisposition::Applied);
        assert!(receipt.recovery_cleared);
        assert_eq!(fs::read(&cake).unwrap(), b"cake-after\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-after\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_retry_preserves_unproven_precommit_mutation() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("exact-retry-unproven");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let store = NativeApplyRecoveryStore::new(&recovery);
        store.prepare(&plan, &manifest, &cake, &sqm).unwrap();
        store
            .transition(
                NativeApplyRecoveryState::Prepared,
                NativeApplyRecoveryState::MutationStarted,
            )
            .unwrap();
        fs::write(&cake, b"unproven-precommit\n").unwrap();

        let mut backend = FakeBackend::new(&cake, &sqm);
        let error = execute_native_apply_commit(
            &plan,
            &manifest,
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
        )
        .unwrap_err();

        assert!(error.contains("precommit recovery remains pending"));
        assert!(
            error.contains("foreign or unproven config state"),
            "{error}"
        );
        assert_eq!(fs::read(&cake).unwrap(), b"unproven-precommit\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-before\n");
        assert_eq!(
            store.read_record().unwrap().unwrap().state,
            NativeApplyRecoveryState::RollbackRequired
        );
        assert_eq!(backend.events, ["reconstruct-legacy", "emergency-contain"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn foreign_pending_transaction_is_rejected_without_mutation() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("foreign-pending");
        let original_plan = plan();
        let original_manifest = original_plan.canonical_manifest_bytes().unwrap();
        NativeApplyRecoveryStore::new(&recovery)
            .prepare(&original_plan, &original_manifest, &cake, &sqm)
            .unwrap();
        let mut foreign_plan = original_plan.clone();
        foreign_plan.worker_run_id = "99".repeat(16);
        let foreign_manifest = foreign_plan.canonical_manifest_bytes().unwrap();
        let mut backend = FakeBackend::new(&cake, &sqm);

        let error = execute_native_apply_commit(
            &foreign_plan,
            &foreign_manifest,
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
        )
        .unwrap_err();

        assert!(error.contains("foreign native Apply recovery transaction"));
        assert_eq!(fs::read(&cake).unwrap(), b"cake-before\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-before\n");
        assert!(backend.events.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interrupted_transaction_recovers_from_durable_request_without_a_plan() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("boot-recovery");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let store = NativeApplyRecoveryStore::new(&recovery);
        store.prepare(&plan, &manifest, &cake, &sqm).unwrap();
        store
            .transition(
                NativeApplyRecoveryState::Prepared,
                NativeApplyRecoveryState::MutationStarted,
            )
            .unwrap();
        fs::write(&cake, b"cake-after\n").unwrap();
        fs::write(&sqm, b"sqm-after\n").unwrap();
        let restarted = store
            .transition(
                NativeApplyRecoveryState::MutationStarted,
                NativeApplyRecoveryState::ServiceRestarted,
            )
            .unwrap();
        store.stage_candidate(&cake, &sqm, &restarted).unwrap();

        let mut backend = FakeBackend::new(&cake, &sqm);
        let receipt = recover_native_apply(
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
        )
        .unwrap()
        .unwrap();
        assert_eq!(receipt.job_id, plan.request.identity.job_id);
        assert_eq!(receipt.worker_run_id, plan.worker_run_id);
        assert!(receipt.recovery_cleared);
        assert!(!receipt.rolled_forward);
        assert_eq!(fs::read(&cake).unwrap(), b"cake-before\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-before\n");
        assert_eq!(backend.events, ["restart-applied", "verify-restored"]);
        assert!(store.read_record().unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interrupted_unproven_mutation_is_contained_without_overwrite() {
        let (root, cake, sqm, recovery, lock) = transaction_fixture("boot-unproven");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let store = NativeApplyRecoveryStore::new(&recovery);
        store.prepare(&plan, &manifest, &cake, &sqm).unwrap();
        store
            .transition(
                NativeApplyRecoveryState::Prepared,
                NativeApplyRecoveryState::MutationStarted,
            )
            .unwrap();
        fs::write(&cake, b"unproven-cake\n").unwrap();

        let mut backend = FakeBackend::new(&cake, &sqm);
        let error = recover_native_apply(
            NativeApplyTransactionPaths {
                recovery_root: &recovery,
                global_lock: &lock,
                cake_config: &cake,
                sqm_config: &sqm,
            },
            &mut backend,
        )
        .unwrap_err();

        assert!(error.contains("recovery remains pending"));
        assert!(
            error.contains("foreign or unproven config state"),
            "{error}"
        );
        assert_eq!(fs::read(&cake).unwrap(), b"unproven-cake\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-before\n");
        assert_eq!(
            store.read_record().unwrap().unwrap().state,
            NativeApplyRecoveryState::RollbackRequired
        );
        assert_eq!(backend.events, ["reconstruct-legacy", "emergency-contain"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn canonical_uci_batch_is_single_process_input_from_the_typed_plan() {
        let plan = plan();
        let batch = String::from_utf8(canonical_native_uci_batch(&plan).unwrap()).unwrap();
        let mut expected = String::new();
        for mutation in &plan.uci_mutations {
            match (&mutation.action, mutation.value.as_deref()) {
                (NativeUciMutationAction::Set, Some(value)) => expected.push_str(&format!(
                    "set {}.{}.{}='{}'\n",
                    mutation.package, mutation.section, mutation.option, value
                )),
                (NativeUciMutationAction::Delete, None) => expected.push_str(&format!(
                    "delete {}.{}.{}\n",
                    mutation.package, mutation.section, mutation.option
                )),
                _ => panic!("production plan unexpectedly emitted a section creation"),
            }
        }
        expected.push_str("commit cake-autorate\n");
        assert_eq!(batch, expected);
        assert!(batch.ends_with("commit cake-autorate\n"));
        assert!(batch.contains(&format!(
            "set cake-autorate.wan_sqm.base_dl_shaper_rate_kbps='{}'\n",
            plan.download.base_kbps.unwrap()
        )));
        assert!(batch.contains(&format!(
            "set cake-autorate.wan_sqm.base_ul_shaper_rate_kbps='{}'\n",
            plan.upload.base_kbps.unwrap()
        )));
        assert!(batch.contains("delete cake-autorate.wan_sqm.service_dl_cap_kbps\n"));
        assert!(!batch.contains("sh -c"));
        assert!(!batch.contains("0.8"));
        assert_eq!(
            batch
                .lines()
                .filter(|line| *line == "commit cake-autorate")
                .count(),
            1
        );
    }

    #[test]
    fn rollback_restores_a_section_that_was_absent_before_apply() {
        const CAKE_BEFORE: &[u8] = b"config cake_autorate 'existing'\n\toption enabled '1'\n";
        const CAKE_WITH_BOOTSTRAP: &[u8] = b"config cake_autorate 'existing'\n\toption enabled '1'\n\nconfig cake_autorate 'bootstrap_wan'\n\toption enabled '1'\n";
        let root = temp_root("rollback-absent-section");
        let configs = root.join("configs");
        fs::create_dir_all(&configs).unwrap();
        let cake = configs.join("cake-autorate");
        let sqm = configs.join("sqm");
        write_config(&cake, CAKE_BEFORE, 0o640);
        write_config(&sqm, b"config queue 'existing'\n", 0o644);

        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let store = NativeApplyRecoveryStore::new(&root.join("recovery"));
        store.prepare(&plan, &manifest, &cake, &sqm).unwrap();
        store
            .transition(
                NativeApplyRecoveryState::Prepared,
                NativeApplyRecoveryState::MutationStarted,
            )
            .unwrap();
        fs::write(&cake, CAKE_WITH_BOOTSTRAP).unwrap();
        let restarted = store
            .transition(
                NativeApplyRecoveryState::MutationStarted,
                NativeApplyRecoveryState::ServiceRestarted,
            )
            .unwrap();
        store.stage_candidate(&cake, &sqm, &restarted).unwrap();
        store
            .transition(
                NativeApplyRecoveryState::ServiceRestarted,
                NativeApplyRecoveryState::RollbackRequired,
            )
            .unwrap();
        store.restore_config_files(&cake, &sqm).unwrap();

        assert_eq!(fs::read(&cake).unwrap(), CAKE_BEFORE);
        assert!(!String::from_utf8(fs::read(&cake).unwrap())
            .unwrap()
            .contains("bootstrap_wan"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn global_lock_excludes_a_second_owner_and_is_borrowed_as_exact_fd8() {
        let root = temp_root("global-lock");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("runtime.guard");
        let lock = NativeApplyGlobalLock::acquire(&path).unwrap();
        assert!(NativeApplyGlobalLock::acquire(&path)
            .unwrap_err()
            .contains("busy"));

        let mut child = Command::new("/bin/sh");
        child.arg("-c").arg(concat!(
            "test \"$CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_FD\" = 8 && ",
            "test \"$CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_MODE\" = exclusive && ",
            "test \"$CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_BORROW\" = 1 && ",
            "test -e /proc/self/fd/8"
        ));
        lock.configure_borrowed_restart(&mut child);
        assert!(child.status().unwrap().success());
        drop(lock);
        let replacement = acquire_test_lock_after_fork_window(&path);
        drop(replacement);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn config_pair_lock_refuses_a_busy_cake_or_sqm_file() {
        let (root, cake, sqm, _, _) = transaction_fixture("config-pair-lock");
        let cake_owner = lock_config_file(&cake, "cake-autorate").unwrap();
        let cake_error = match NativeApplyConfigPairLock::acquire(&cake, &sqm) {
            Ok(_) => panic!("busy cake-autorate config lock was not rejected"),
            Err(error) => error,
        };
        assert!(cake_error.contains("cake-autorate config is busy"));
        drop(cake_owner);

        let sqm_owner = lock_config_file(&sqm, "SQM").unwrap();
        let sqm_error = match NativeApplyConfigPairLock::acquire(&cake, &sqm) {
            Ok(_) => panic!("busy SQM config lock was not rejected"),
            Err(error) => error,
        };
        assert!(
            sqm_error.contains("sqm config is busy"),
            "unexpected SQM lock error: {sqm_error}"
        );
        drop(sqm_owner);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_lock_waits_for_the_kernel_owner_release_event() {
        let root = temp_root("recovery-lock-owner-event");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("runtime.guard");
        let owner = NativeApplyGlobalLock::acquire(&path).unwrap();
        let waiter_path = path.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            let lock = NativeApplyGlobalLock::acquire_for_recovery(&waiter_path)?;
            acquired_tx.send(()).unwrap();
            Ok::<_, String>(lock)
        });
        ready_rx.recv().unwrap();
        assert!(matches!(
            acquired_rx.recv_timeout(Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        drop(owner);
        acquired_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let replacement = waiter.join().unwrap().unwrap();
        drop(replacement);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prepare_publishes_one_complete_private_recovery_transaction() {
        let root = temp_root("prepare");
        let configs = root.join("configs");
        fs::create_dir_all(&configs).unwrap();
        let cake = configs.join("cake-autorate");
        let sqm = configs.join("sqm");
        write_config(&cake, b"config cake\n", 0o640);
        write_config(&sqm, b"config queue\n", 0o644);
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let store = NativeApplyRecoveryStore::new(&root.join("recovery"));
        let record = store.prepare(&plan, &manifest, &cake, &sqm).unwrap();
        assert_eq!(record.state, NativeApplyRecoveryState::Prepared);
        assert_eq!(store.read_record().unwrap(), Some(record.clone()));
        store.verify_backups(&record).unwrap();
        assert!(!store.staging_path().exists());
        for name in [
            CAKE_BACKUP_FILE,
            SQM_BACKUP_FILE,
            REQUEST_FILE,
            MANIFEST_FILE,
            STATE_FILE,
        ] {
            let metadata = fs::metadata(store.current_path().join(name)).unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
        assert!(store.prepare(&plan, &manifest, &cake, &sqm).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn write_ahead_prepare_publishes_complete_v5_authority_before_mutation() {
        let (root, cake, sqm, recovery, _lock) = transaction_fixture("prepare-v5");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let original = NativeApplyConfigPairSnapshot::capture(&cake, &sqm).unwrap();
        let candidate = NativeApplyCandidateMaterialization::new(
            b"cake-after\n".to_vec(),
            b"sqm-after\n".to_vec(),
            canonical_native_uci_batch(&plan).unwrap(),
        )
        .unwrap();
        let store = NativeApplyRecoveryStore::new(&recovery);
        let record = store
            .prepare_write_ahead(&plan, &manifest, &original, &candidate)
            .unwrap();
        assert_eq!(record.schema_version, 5);
        assert_eq!(record.state, NativeApplyRecoveryState::Prepared);
        assert!(record.candidate_cake_sha256.is_some());
        assert!(record.candidate_sqm_sha256.is_some());
        assert!(record.materialization_sha256.is_some());
        let state = fs::read(store.current_path().join(STATE_FILE)).unwrap();
        assert!(state.starts_with(format!("{RECOVERY_HEADER_V5}\n").as_bytes()));
        assert_eq!(NativeApplyRecoveryRecord::decode(&state).unwrap(), record);
        for name in [
            CAKE_BACKUP_FILE,
            SQM_BACKUP_FILE,
            CAKE_CANDIDATE_FILE,
            SQM_CANDIDATE_FILE,
            REQUEST_FILE,
            MANIFEST_FILE,
            MATERIALIZATION_FILE,
            STATE_FILE,
        ] {
            let metadata = fs::metadata(store.current_path().join(name)).unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(fs::read(&cake).unwrap(), b"cake-before\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-before\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_rejects_a_retired_public_v4_request() {
        let (root, cake, sqm, recovery, _lock) = transaction_fixture("retired-v4-request");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let store = NativeApplyRecoveryStore::new(&recovery);
        store.prepare(&plan, &manifest, &cake, &sqm).unwrap();

        let retired = plan.request.encode_for_test_schema(4).unwrap();
        let mut record = store.read_record().unwrap().unwrap();
        record.request_sha256 = sqm_identity::sha256sum(retired.as_bytes()).unwrap();
        fs::write(store.current_path().join(REQUEST_FILE), retired).unwrap();
        fs::write(
            store.current_path().join(STATE_FILE),
            record.encode().unwrap(),
        )
        .unwrap();
        assert!(store
            .read_request()
            .unwrap_err()
            .contains("retired wire schema"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn transitions_are_strict_and_tampered_backups_fail_closed() {
        let root = temp_root("transitions");
        let configs = root.join("configs");
        fs::create_dir_all(&configs).unwrap();
        let cake = configs.join("cake-autorate");
        let sqm = configs.join("sqm");
        write_config(&cake, b"cake-before\n", 0o600);
        write_config(&sqm, b"sqm-before\n", 0o600);
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let store = NativeApplyRecoveryStore::new(&root.join("recovery"));
        store.prepare(&plan, &manifest, &cake, &sqm).unwrap();
        assert!(store
            .transition(
                NativeApplyRecoveryState::Prepared,
                NativeApplyRecoveryState::Verified
            )
            .is_err());
        let mut file = OpenOptions::new()
            .append(true)
            .open(store.current_path().join(CAKE_BACKUP_FILE))
            .unwrap();
        file.write_all(b"tampered").unwrap();
        file.sync_all().unwrap();
        assert!(store
            .transition(
                NativeApplyRecoveryState::Prepared,
                NativeApplyRecoveryState::MutationStarted
            )
            .unwrap_err()
            .contains("digest mismatch"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn precommit_candidate_evidence_tamper_fails_closed() {
        let (root, cake, sqm, recovery, _) = transaction_fixture("candidate-evidence-tamper");
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let store = NativeApplyRecoveryStore::new(&recovery);
        store.prepare(&plan, &manifest, &cake, &sqm).unwrap();
        store
            .transition(
                NativeApplyRecoveryState::Prepared,
                NativeApplyRecoveryState::MutationStarted,
            )
            .unwrap();
        fs::write(&cake, b"cake-after\n").unwrap();
        fs::write(&sqm, b"sqm-after\n").unwrap();
        let restarted = store
            .transition(
                NativeApplyRecoveryState::MutationStarted,
                NativeApplyRecoveryState::ServiceRestarted,
            )
            .unwrap();
        store.stage_candidate(&cake, &sqm, &restarted).unwrap();
        let mut file = OpenOptions::new()
            .append(true)
            .open(store.current_path().join(CAKE_CANDIDATE_FILE))
            .unwrap();
        file.write_all(b"tampered").unwrap();
        file.sync_all().unwrap();

        assert!(store
            .transition(
                NativeApplyRecoveryState::ServiceRestarted,
                NativeApplyRecoveryState::RollbackRequired,
            )
            .unwrap_err()
            .contains("candidate snapshot digest mismatch"));
        assert_eq!(
            store.read_record().unwrap().unwrap().state,
            NativeApplyRecoveryState::ServiceRestarted
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rollback_restores_both_files_exactly_and_clears_only_after_attestation() {
        let root = temp_root("rollback");
        let configs = root.join("configs");
        fs::create_dir_all(&configs).unwrap();
        let cake = configs.join("cake-autorate");
        let sqm = configs.join("sqm");
        write_config(&cake, b"cake-before\n", 0o640);
        write_config(&sqm, b"sqm-before\n", 0o644);
        let plan = plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let store = NativeApplyRecoveryStore::new(&root.join("recovery"));
        store.prepare(&plan, &manifest, &cake, &sqm).unwrap();
        store
            .transition(
                NativeApplyRecoveryState::Prepared,
                NativeApplyRecoveryState::MutationStarted,
            )
            .unwrap();
        fs::write(&cake, b"cake-after\n").unwrap();
        fs::write(&sqm, b"sqm-after\n").unwrap();
        let restarted = store
            .transition(
                NativeApplyRecoveryState::MutationStarted,
                NativeApplyRecoveryState::ServiceRestarted,
            )
            .unwrap();
        store.stage_candidate(&cake, &sqm, &restarted).unwrap();
        store
            .transition(
                NativeApplyRecoveryState::ServiceRestarted,
                NativeApplyRecoveryState::RollbackRequired,
            )
            .unwrap();
        store.restore_config_files(&cake, &sqm).unwrap();
        assert_eq!(fs::read(&cake).unwrap(), b"cake-before\n");
        assert_eq!(fs::read(&sqm).unwrap(), b"sqm-before\n");
        assert_eq!(
            fs::metadata(&cake).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(
            fs::metadata(&sqm).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert!(store.clear_restored().is_err());
        store
            .transition(
                NativeApplyRecoveryState::RollbackRequired,
                NativeApplyRecoveryState::Restored,
            )
            .unwrap();
        store.clear_restored().unwrap();
        assert!(store.read_record().unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn incomplete_staging_cleanup_rejects_unknown_or_symlink_entries() {
        let root = temp_root("staging");
        let store = NativeApplyRecoveryStore::new(&root.join("recovery"));
        ensure_private_directory(&store.root).unwrap();
        create_private_directory(&store.staging_path()).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", store.staging_path().join("foreign")).unwrap();
        assert!(store.discard_incomplete_staging().is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
