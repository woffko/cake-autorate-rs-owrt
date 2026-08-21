//! Transactional executor for accepted absent-target Apply v7.
//!
//! The orchestration in this module is deliberately backend-generic.  It
//! composes the durable v7 manifest, deterministic two-package UCI
//! materialization, global Apply lock and bootstrap recovery store. The
//! OpenWrt backend positively re-attests UCI, route and kernel topology at
//! every boundary represented by the trait below; elapsed time is never
//! mutation authority.

use super::autotune_apply_runtime::{
    NativeApplyCommitDisposition, NativeApplyCommitReceipt, NativeApplyGlobalLock,
    NativeApplyRecoveryReceipt, NativeApplyTransactionPaths,
};
use super::autotune_bootstrap_apply::{NativeBootstrapApplyMode, NativeBootstrapApplyPlan};
use super::autotune_bootstrap_apply_recovery::{
    NativeBootstrapApplyRecoveryRecord, NativeBootstrapApplyRecoveryState,
    NativeBootstrapApplyRecoveryStore,
};
use super::autotune_runtime::AbsentRuntimeBaseline;
use super::protocol::{OperationKind, OperationOrigin, OperationRequest, OperationTargetState};
use super::sqm_identity;

/// Platform boundary for bootstrap Apply.  Each attestation is a fresh state
/// observation.  Implementations must never turn a timeout, retry count or
/// unchanged elapsed period into authority to mutate or recover state.
pub(crate) trait NativeBootstrapApplyBackend {
    /// Prove that the exact accepted candidate is already installed and live.
    /// This is the only idempotent success path after recovery evidence has
    /// been cleared; a merely existing instance is not sufficient.
    fn candidate_already_applied(
        &mut self,
        plan: &NativeBootstrapApplyPlan,
    ) -> Result<bool, String>;

    /// Re-attest exact UCI absence, selected route, target ifindex and bare
    /// kernel topology against the immutable v6 plan.
    fn attest_absent_before_mutation(
        &mut self,
        plan: &NativeBootstrapApplyPlan,
    ) -> Result<(), String>;

    /// Candidate files are now live but the service has not been restarted.
    /// Require the exact candidate UCI pair plus the original route and bare
    /// kernel topology immediately before the first runtime mutation.
    fn attest_candidate_before_restart(
        &mut self,
        plan: &NativeBootstrapApplyPlan,
    ) -> Result<(), String>;

    fn restart_service(
        &mut self,
        request: &OperationRequest,
        lock: &NativeApplyGlobalLock,
    ) -> Result<(), String>;

    /// Verify exact candidate UCI plus the resulting managed daemon/SQM/CAKE
    /// state for the selected v6 plan.
    fn verify_applied(&mut self, plan: &NativeBootstrapApplyPlan) -> Result<(), String>;

    fn discard_pending_uci_changes(&mut self) -> Result<(), String>;

    /// Re-establish the exact candidate configuration, prove that any live
    /// runtime belongs to that immutable candidate, stop only that selected
    /// owner, and prove its controller/SQM/kernel topology absent.  Rollback
    /// cannot restore the original absent UCI first: doing so would erase the
    /// ownership context required for a safe selected-instance stop.
    fn quiesce_candidate_before_rollback(
        &mut self,
        request: &OperationRequest,
        record: &NativeBootstrapApplyRecoveryRecord,
        lock: &NativeApplyGlobalLock,
    ) -> Result<(), String>;

    /// Before a commit-accepted recovery restarts the selected service, prove
    /// that the exact durable candidate UCI pair is live and that the current
    /// kernel state contains no foreign ownership.
    fn attest_rollforward_before_restart(
        &mut self,
        request: &OperationRequest,
        record: &NativeBootstrapApplyRecoveryRecord,
    ) -> Result<(), String>;

    /// Verify that rollback restored the exact absent UCI, route and kernel
    /// baseline captured before calibration.
    fn verify_absent_restored(
        &mut self,
        request: &OperationRequest,
        baseline: &AbsentRuntimeBaseline,
    ) -> Result<(), String>;

    /// Verify a commit-accepted candidate during restart recovery when the
    /// in-memory selected plan is no longer available.
    fn verify_recovered_candidate(
        &mut self,
        request: &OperationRequest,
        record: &NativeBootstrapApplyRecoveryRecord,
    ) -> Result<(), String>;

    /// Best-effort containment does not clear recovery evidence and is never
    /// reported as successful rollback or commit.
    fn emergency_contain(
        &mut self,
        request: &OperationRequest,
        mode: NativeBootstrapApplyMode,
        lock: &NativeApplyGlobalLock,
    ) -> Result<(), String>;
}

pub(crate) fn execute_native_bootstrap_apply_commit<B: NativeBootstrapApplyBackend>(
    plan: &NativeBootstrapApplyPlan,
    manifest: &[u8],
    paths: NativeApplyTransactionPaths<'_>,
    backend: &mut B,
) -> Result<NativeApplyCommitReceipt, String> {
    require_exact_manifest(plan, manifest)?;
    let manifest_sha256 = sqm_identity::sha256sum(manifest)?;
    let lock = NativeApplyGlobalLock::acquire(paths.global_lock)?;
    let store = NativeBootstrapApplyRecoveryStore::new(paths.recovery_root);
    store.discard_incomplete_staging()?;
    store.discard_completed()?;

    if let Some(record) = store.read_record()? {
        let request = store.read_request()?;
        if request != *plan.request()
            || record.job_id != plan.identity().job_id
            || record.worker_run_id != plan.identity().worker_run_id
            || record.option_id != plan.identity().option_id
            || record.manifest_sha256 != manifest_sha256
            || record.composite_candidate_id != plan.identity().composite_candidate_id
        {
            return Err(
                "a foreign bootstrap Apply recovery transaction is already pending".to_string(),
            );
        }
        store.verify_exact_plan(plan)?;
        if record.state == NativeBootstrapApplyRecoveryState::CommitAccepted {
            if let Err(recovery_error) = roll_forward_bootstrap_apply(
                &request,
                Some(plan),
                &store,
                &lock,
                paths.cake_config,
                paths.sqm_config,
                backend,
            ) {
                let containment = containment_report(backend, &request, &lock, &record);
                return Err(format!(
                    "bootstrap Apply commit recovery remains pending: {recovery_error}; {}",
                    containment
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
        if let Err(recovery_error) = rollback_bootstrap_apply(
            &request,
            &store,
            &lock,
            paths.cake_config,
            paths.sqm_config,
            backend,
        ) {
            let containment = containment_report(backend, &request, &lock, &record);
            return Err(format!(
                "bootstrap Apply precommit recovery remains pending: {recovery_error}; {}",
                containment
            ));
        }
    }

    if backend.candidate_already_applied(plan)? {
        return Ok(NativeApplyCommitReceipt {
            job_id: plan.identity().job_id.clone(),
            worker_run_id: plan.identity().worker_run_id.clone(),
            manifest_sha256,
            disposition: NativeApplyCommitDisposition::AlreadyApplied,
            recovery_cleared: true,
        });
    }

    backend.attest_absent_before_mutation(plan)?;
    let prepared = store.prepare(plan, manifest, paths.cake_config, paths.sqm_config)?;
    let precommit_result = (|| {
        store.verify_exact_plan(plan)?;
        backend.attest_absent_before_mutation(plan)?;
        let mutation = store.begin_mutation(paths.cake_config, paths.sqm_config)?;
        mutation.install_candidate_files()?;
        store.verify_live_candidate_files(paths.cake_config, paths.sqm_config)?;
        backend.attest_candidate_before_restart(plan)?;
        let verified = match plan.mode() {
            NativeBootstrapApplyMode::ShapedRuntime => {
                backend.restart_service(plan.request(), &lock)?;
                store.transition(
                    NativeBootstrapApplyRecoveryState::MutationStarted,
                    NativeBootstrapApplyRecoveryState::ServiceRestarted,
                )?;
                drop(mutation);
                store.verify_live_candidate_files(paths.cake_config, paths.sqm_config)?;
                backend.verify_applied(plan)?;
                store.transition(
                    NativeBootstrapApplyRecoveryState::ServiceRestarted,
                    NativeBootstrapApplyRecoveryState::Verified,
                )?
            }
            NativeBootstrapApplyMode::DisabledInactive => {
                backend.verify_applied(plan)?;
                let verified = store.transition(
                    NativeBootstrapApplyRecoveryState::MutationStarted,
                    NativeBootstrapApplyRecoveryState::Verified,
                )?;
                drop(mutation);
                store.verify_live_candidate_files(paths.cake_config, paths.sqm_config)?;
                verified
            }
        };
        store.verify_exact_plan(plan)?;
        backend.verify_applied(plan)?;
        let accepted = store.transition(
            NativeBootstrapApplyRecoveryState::Verified,
            NativeBootstrapApplyRecoveryState::CommitAccepted,
        )?;
        if verified.job_id != accepted.job_id {
            return Err("bootstrap Apply recovery identity changed during commit".to_string());
        }
        Ok::<NativeBootstrapApplyRecoveryRecord, String>(accepted)
    })();

    let accepted = match precommit_result {
        Ok(record) => record,
        Err(apply_error) => {
            return match store.read_record() {
                Ok(Some(record))
                    if record.state == NativeBootstrapApplyRecoveryState::CommitAccepted =>
                {
                    Err(format!(
                        "bootstrap Apply commit was durably accepted but completion failed: {apply_error}; roll-forward recovery is required"
                    ))
                }
                Ok(Some(record)) => match rollback_bootstrap_apply(
                    plan.request(),
                    &store,
                    &lock,
                    paths.cake_config,
                    paths.sqm_config,
                    backend,
                ) {
                    Ok(()) => Err(format!(
                        "bootstrap Apply failed before commit acceptance: {apply_error}; exact absent rollback verified"
                    )),
                    Err(rollback_error) => {
                        let containment =
                            containment_report(backend, plan.request(), &lock, &record);
                        Err(format!(
                            "bootstrap Apply failed before commit acceptance: {apply_error}; rollback remains pending: {rollback_error}; {}",
                            containment
                        ))
                    }
                },
                Ok(None) => Err(format!(
                    "bootstrap Apply failed and its durable recovery transaction disappeared: {apply_error}"
                )),
                Err(state_error) => Err(format!(
                    "bootstrap Apply failed and its commit boundary cannot be classified safely: {apply_error}; {state_error}"
                )),
            };
        }
    };

    if accepted.state != NativeBootstrapApplyRecoveryState::CommitAccepted {
        return Err("bootstrap Apply candidate was not durably commit-accepted".to_string());
    }
    if let Err(error) = store
        .verify_live_candidate_files(paths.cake_config, paths.sqm_config)
        .and_then(|_| backend.verify_applied(plan))
        .and_then(|_| store.clear_committed())
    {
        return Err(format!(
            "bootstrap Apply commit was accepted but final attestation/cleanup failed: {error}; roll-forward recovery is required"
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

pub(crate) fn recover_native_bootstrap_apply<B: NativeBootstrapApplyBackend>(
    paths: NativeApplyTransactionPaths<'_>,
    backend: &mut B,
) -> Result<Option<NativeApplyRecoveryReceipt>, String> {
    let store = NativeBootstrapApplyRecoveryStore::new(paths.recovery_root);
    if let Some(record) = store.read_record()? {
        let request = store.read_request()?;
        let _ = absent_baseline_from_record(&request, &record)?;
    }

    let lock = NativeApplyGlobalLock::acquire_for_recovery(paths.global_lock)?;
    let Some(initial_record) = store.read_record()? else {
        store.discard_incomplete_staging()?;
        store.discard_completed()?;
        return Ok(None);
    };
    let initial_request = store.read_request()?;
    let _ = absent_baseline_from_record(&initial_request, &initial_record)?;
    store.discard_incomplete_staging()?;
    store.discard_completed()?;
    let record = store
        .read_record()?
        .ok_or_else(|| "bootstrap Apply recovery disappeared while locked".to_string())?;
    let request = store.read_request()?;
    if record != initial_record || request != initial_request {
        return Err("bootstrap Apply recovery authority changed while locked".to_string());
    }
    let _ = absent_baseline_from_record(&request, &record)?;
    let rolled_forward = record.state == NativeBootstrapApplyRecoveryState::CommitAccepted;
    let recovery_result = if rolled_forward {
        roll_forward_bootstrap_apply(
            &request,
            None,
            &store,
            &lock,
            paths.cake_config,
            paths.sqm_config,
            backend,
        )
    } else {
        rollback_bootstrap_apply(
            &request,
            &store,
            &lock,
            paths.cake_config,
            paths.sqm_config,
            backend,
        )
    };
    if let Err(recovery_error) = recovery_result {
        let containment = containment_report(backend, &request, &lock, &record);
        return Err(format!(
            "bootstrap Apply recovery remains pending: {recovery_error}; {}",
            containment
        ));
    }
    Ok(Some(NativeApplyRecoveryReceipt {
        job_id: record.job_id,
        worker_run_id: record.worker_run_id,
        recovery_cleared: store.read_record()?.is_none(),
        rolled_forward,
    }))
}

fn rollback_bootstrap_apply<B: NativeBootstrapApplyBackend>(
    request: &OperationRequest,
    store: &NativeBootstrapApplyRecoveryStore,
    lock: &NativeApplyGlobalLock,
    cake_config: &std::path::Path,
    sqm_config: &std::path::Path,
    backend: &mut B,
) -> Result<(), String> {
    let mut record = store
        .read_record()?
        .ok_or_else(|| "bootstrap Apply recovery disappeared before rollback".to_string())?;
    let baseline = absent_baseline_from_record(request, &record)?;
    match record.state {
        NativeBootstrapApplyRecoveryState::Prepared => {
            store.verify_live_original_files(cake_config, sqm_config)?;
            backend.verify_absent_restored(request, &baseline)?;
            return store.clear_prepared(cake_config, sqm_config);
        }
        NativeBootstrapApplyRecoveryState::MutationStarted
        | NativeBootstrapApplyRecoveryState::ServiceRestarted
        | NativeBootstrapApplyRecoveryState::Verified => {
            record = store.transition(
                record.state,
                NativeBootstrapApplyRecoveryState::RollbackRequired,
            )?;
        }
        NativeBootstrapApplyRecoveryState::RollbackRequired => {}
        NativeBootstrapApplyRecoveryState::CommitAccepted => {
            return Err(
                "commit-accepted bootstrap Apply cannot cross back into rollback".to_string(),
            )
        }
        NativeBootstrapApplyRecoveryState::Restored => {
            store.verify_live_original_files(cake_config, sqm_config)?;
            backend.verify_absent_restored(request, &baseline)?;
            return store.clear_restored();
        }
    }
    // A selected stop needs the candidate's exact instance/SQM ownership
    // markers.  Reconstruct the complete candidate pair first even if the
    // crash left one package original and the other candidate.  Only after the
    // backend proves that candidate runtime absent may those owner records be
    // removed by restoring the original absent pair.
    store.reconstruct_candidate_files_for_rollback(cake_config, sqm_config)?;
    backend.discard_pending_uci_changes()?;
    backend.quiesce_candidate_before_rollback(request, &record, lock)?;
    store.restore_original_files(cake_config, sqm_config)?;
    backend.discard_pending_uci_changes()?;
    store.verify_live_original_files(cake_config, sqm_config)?;
    backend.verify_absent_restored(request, &baseline)?;
    store.transition(
        NativeBootstrapApplyRecoveryState::RollbackRequired,
        NativeBootstrapApplyRecoveryState::Restored,
    )?;
    store.clear_restored()
}

fn roll_forward_bootstrap_apply<B: NativeBootstrapApplyBackend>(
    request: &OperationRequest,
    plan: Option<&NativeBootstrapApplyPlan>,
    store: &NativeBootstrapApplyRecoveryStore,
    lock: &NativeApplyGlobalLock,
    cake_config: &std::path::Path,
    sqm_config: &std::path::Path,
    backend: &mut B,
) -> Result<(), String> {
    let record = store
        .read_record()?
        .ok_or_else(|| "bootstrap Apply recovery transaction is missing".to_string())?;
    let _ = absent_baseline_from_record(request, &record)?;
    if record.state != NativeBootstrapApplyRecoveryState::CommitAccepted {
        return Err("bootstrap roll-forward requires durable commit acceptance".to_string());
    }
    if let Some(plan) = plan {
        store.verify_exact_plan(plan)?;
    }
    store.restore_candidate_files(cake_config, sqm_config)?;
    backend.discard_pending_uci_changes()?;
    match record.mode {
        NativeBootstrapApplyMode::ShapedRuntime => {
            backend.attest_rollforward_before_restart(request, &record)?;
            backend.restart_service(request, lock)?;
            store.verify_live_candidate_files(cake_config, sqm_config)?;
            backend.verify_recovered_candidate(request, &record)?;
        }
        NativeBootstrapApplyMode::DisabledInactive => {
            // A disabled raw fallback has no selected runtime owner to start.
            // Re-attest the exact inactive candidate while both durable files
            // still match; a restart would create an unnecessary SQM/runtime
            // side effect and would violate this candidate's authority.
            store.verify_live_candidate_files(cake_config, sqm_config)?;
            backend.verify_recovered_candidate(request, &record)?;
        }
    }
    if let Some(plan) = plan {
        backend.verify_applied(plan)?;
    }
    store.clear_committed()
}

fn require_exact_manifest(plan: &NativeBootstrapApplyPlan, manifest: &[u8]) -> Result<(), String> {
    let expected = plan.canonical_manifest_bytes()?;
    if manifest != expected {
        return Err("bootstrap Apply manifest changed after plan construction".to_string());
    }
    Ok(())
}

fn containment_report<B: NativeBootstrapApplyBackend>(
    backend: &mut B,
    request: &OperationRequest,
    lock: &NativeApplyGlobalLock,
    record: &NativeBootstrapApplyRecoveryRecord,
) -> String {
    if !matches!(
        record.state,
        NativeBootstrapApplyRecoveryState::MutationStarted
            | NativeBootstrapApplyRecoveryState::ServiceRestarted
            | NativeBootstrapApplyRecoveryState::Verified
            | NativeBootstrapApplyRecoveryState::CommitAccepted
            | NativeBootstrapApplyRecoveryState::RollbackRequired
    ) {
        return "no bootstrap runtime mutation was authorized; containment was not invoked"
            .to_string();
    }
    backend
        .emergency_contain(request, record.mode, lock)
        .map_or_else(
            |error| format!("emergency containment also failed: {error}"),
            |_| "bootstrap runtime was emergency-contained".to_string(),
        )
}

pub(crate) fn absent_baseline_from_record(
    request: &OperationRequest,
    record: &NativeBootstrapApplyRecoveryRecord,
) -> Result<AbsentRuntimeBaseline, String> {
    request.validate()?;
    if request.target_state != OperationTargetState::AbsentBootstrap
        || request.identity.operation != OperationKind::FullAutotune
        || request.origin != OperationOrigin::Luci
        || request.scheduled_auto_apply_requested
    {
        return Err(
            "bootstrap Apply recovery requires a manual LuCI absent Full Auto-Tune request"
                .to_string(),
        );
    }
    if request.identity.job_id != record.job_id {
        return Err("bootstrap Apply recovery request identity mismatch".to_string());
    }
    let baseline = AbsentRuntimeBaseline {
        planned_sqm_section: request
            .managed_sqm_section
            .clone()
            .ok_or_else(|| "bootstrap Apply recovery has no planned SQM section".to_string())?,
        target_interface: request.identity.target_interface.clone(),
        target_ifindex: record.target_ifindex,
        route_fingerprint: request.identity.route_fingerprint.clone(),
        config_fingerprint: request.identity.config_fingerprint.clone(),
        sqm_fingerprint: request.identity.sqm_fingerprint.clone(),
        kernel_topology_fingerprint: record.kernel_topology_fingerprint.clone(),
        kernel_namespace_seed: record.kernel_namespace_seed.clone(),
    };
    baseline.validate()?;
    Ok(baseline)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::autotune_bootstrap_apply::tests::{
        fixture_plan, fixture_raw_fallback_plan,
    };
    use std::collections::BTreeMap;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST: AtomicU64 = AtomicU64::new(1);

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "cake-bootstrap-apply-runtime-{}-{}-{name}",
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
        lock: PathBuf,
        cake_original: Vec<u8>,
        sqm_original: Vec<u8>,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = TestRoot::new(name);
            let cake = root.path().join("cake-autorate");
            let sqm = root.path().join("sqm");
            let recovery = root.path().join("recovery");
            let lock = root.path().join("apply.lock");
            let cake_original =
                b"# preserve exact cake bytes\nconfig globals 'globals'\n\toption enabled '1'\n"
                    .to_vec();
            let sqm_original = b"config queue 'unrelated'\n\toption interface 'eth9'\n".to_vec();
            write_config(&cake, &cake_original);
            write_config(&sqm, &sqm_original);
            Self {
                _root: root,
                cake,
                sqm,
                recovery,
                lock,
                cake_original,
                sqm_original,
            }
        }

        fn paths(&self) -> NativeApplyTransactionPaths<'_> {
            NativeApplyTransactionPaths {
                recovery_root: &self.recovery,
                global_lock: &self.lock,
                cake_config: &self.cake,
                sqm_config: &self.sqm,
            }
        }

        fn store(&self) -> NativeBootstrapApplyRecoveryStore {
            NativeBootstrapApplyRecoveryStore::new(&self.recovery)
        }
    }

    fn write_config(path: &Path, bytes: &[u8]) {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
    }

    #[derive(Default)]
    struct FakeBackend {
        events: Vec<&'static str>,
        counts: BTreeMap<&'static str, usize>,
        fail: Option<(&'static str, usize)>,
        already_applied: bool,
        candidate_files_at_quiesce: Option<(PathBuf, PathBuf)>,
    }

    impl FakeBackend {
        fn event(&mut self, name: &'static str) -> Result<(), String> {
            self.events.push(name);
            let count = self.counts.entry(name).or_default();
            *count += 1;
            if self.fail == Some((name, *count)) {
                return Err(format!("injected {name} failure"));
            }
            Ok(())
        }
    }

    impl NativeBootstrapApplyBackend for FakeBackend {
        fn candidate_already_applied(
            &mut self,
            _plan: &NativeBootstrapApplyPlan,
        ) -> Result<bool, String> {
            self.event("candidate_already")?;
            Ok(self.already_applied)
        }

        fn attest_absent_before_mutation(
            &mut self,
            _plan: &NativeBootstrapApplyPlan,
        ) -> Result<(), String> {
            self.event("absent")
        }

        fn attest_candidate_before_restart(
            &mut self,
            _plan: &NativeBootstrapApplyPlan,
        ) -> Result<(), String> {
            self.event("candidate_before_restart")
        }

        fn restart_service(
            &mut self,
            _request: &OperationRequest,
            _lock: &NativeApplyGlobalLock,
        ) -> Result<(), String> {
            self.event("restart")
        }

        fn verify_applied(&mut self, _plan: &NativeBootstrapApplyPlan) -> Result<(), String> {
            self.event("verify_applied")
        }

        fn discard_pending_uci_changes(&mut self) -> Result<(), String> {
            self.event("discard_pending")
        }

        fn quiesce_candidate_before_rollback(
            &mut self,
            _request: &OperationRequest,
            record: &NativeBootstrapApplyRecoveryRecord,
            _lock: &NativeApplyGlobalLock,
        ) -> Result<(), String> {
            if let Some((cake, sqm)) = &self.candidate_files_at_quiesce {
                let cake = fs::read_to_string(cake)
                    .map_err(|error| format!("unable to inspect candidate cake file: {error}"))?;
                let sqm = fs::read_to_string(sqm)
                    .map_err(|error| format!("unable to inspect candidate SQM file: {error}"))?;
                match record.mode {
                    NativeBootstrapApplyMode::ShapedRuntime => {
                        if !cake.contains("config cake_autorate 'wan_sqm'")
                            || !sqm.contains("config queue 'cake_wan_sqm'")
                        {
                            return Err(
                                "candidate owner context was removed before runtime quiescence"
                                    .to_string(),
                            );
                        }
                    }
                    NativeBootstrapApplyMode::DisabledInactive => {
                        if !cake.contains("config cake_autorate 'wan_sqm'")
                            || !cake.contains("option enabled '0'")
                            || sqm.contains("_cake_autorate_managed")
                        {
                            return Err(
                                "disabled candidate context changed before rollback".to_string()
                            );
                        }
                    }
                }
            }
            self.event(match record.mode {
                NativeBootstrapApplyMode::ShapedRuntime => "quiesce_candidate",
                NativeBootstrapApplyMode::DisabledInactive => "quiesce_disabled_candidate",
            })
        }

        fn attest_rollforward_before_restart(
            &mut self,
            _request: &OperationRequest,
            record: &NativeBootstrapApplyRecoveryRecord,
        ) -> Result<(), String> {
            if record.mode != NativeBootstrapApplyMode::ShapedRuntime {
                return Err("disabled candidate attempted a recovery restart".to_string());
            }
            self.event("recovery_before_rollforward_restart")
        }

        fn verify_absent_restored(
            &mut self,
            _request: &OperationRequest,
            _baseline: &AbsentRuntimeBaseline,
        ) -> Result<(), String> {
            self.event("verify_absent")
        }

        fn verify_recovered_candidate(
            &mut self,
            _request: &OperationRequest,
            record: &NativeBootstrapApplyRecoveryRecord,
        ) -> Result<(), String> {
            self.event(match record.mode {
                NativeBootstrapApplyMode::ShapedRuntime => "verify_recovered_candidate",
                NativeBootstrapApplyMode::DisabledInactive => "verify_recovered_disabled_candidate",
            })
        }

        fn emergency_contain(
            &mut self,
            _request: &OperationRequest,
            mode: NativeBootstrapApplyMode,
            _lock: &NativeApplyGlobalLock,
        ) -> Result<(), String> {
            self.event(match mode {
                NativeBootstrapApplyMode::ShapedRuntime => "contain",
                NativeBootstrapApplyMode::DisabledInactive => "contain_disabled",
            })
        }
    }

    #[test]
    fn disabled_raw_fallback_commits_without_restart_and_preserves_sqm_file() {
        let fixture = Fixture::new("disabled-commit");
        let plan = fixture_raw_fallback_plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let mut backend = FakeBackend::default();
        let receipt =
            execute_native_bootstrap_apply_commit(&plan, &manifest, fixture.paths(), &mut backend)
                .unwrap();

        assert_eq!(receipt.disposition, NativeApplyCommitDisposition::Applied);
        assert!(receipt.recovery_cleared);
        assert_eq!(fs::read(&fixture.sqm).unwrap(), fixture.sqm_original);
        let cake = String::from_utf8(fs::read(&fixture.cake).unwrap()).unwrap();
        assert!(cake.contains("config cake_autorate 'wan_sqm'"));
        assert!(cake.contains("option enabled '0'"));
        assert!(!backend.events.contains(&"restart"));
        assert_eq!(
            backend.events,
            vec![
                "candidate_already",
                "absent",
                "absent",
                "candidate_before_restart",
                "verify_applied",
                "verify_applied",
                "verify_applied",
            ]
        );
    }

    #[test]
    fn disabled_commit_accepted_recovery_rolls_forward_without_restart() {
        let fixture = Fixture::new("disabled-rollforward");
        let plan = fixture_raw_fallback_plan();
        let store = fixture.store();
        store
            .prepare(
                &plan,
                &plan.canonical_manifest_bytes().unwrap(),
                &fixture.cake,
                &fixture.sqm,
            )
            .unwrap();
        let mutation = store.begin_mutation(&fixture.cake, &fixture.sqm).unwrap();
        mutation.install_candidate_files().unwrap();
        drop(mutation);
        store
            .transition(
                NativeBootstrapApplyRecoveryState::MutationStarted,
                NativeBootstrapApplyRecoveryState::Verified,
            )
            .unwrap();
        store
            .transition(
                NativeBootstrapApplyRecoveryState::Verified,
                NativeBootstrapApplyRecoveryState::CommitAccepted,
            )
            .unwrap();
        let cake_candidate = fs::read(&fixture.cake).unwrap();
        fs::write(&fixture.cake, &fixture.cake_original).unwrap();

        let mut backend = FakeBackend::default();
        let receipt = recover_native_bootstrap_apply(fixture.paths(), &mut backend)
            .unwrap()
            .unwrap();
        assert!(receipt.rolled_forward);
        assert!(receipt.recovery_cleared);
        assert_eq!(fs::read(&fixture.cake).unwrap(), cake_candidate);
        assert_eq!(fs::read(&fixture.sqm).unwrap(), fixture.sqm_original);
        assert_eq!(
            backend.events,
            vec!["discard_pending", "verify_recovered_disabled_candidate",]
        );
        assert!(!backend.events.contains(&"restart"));
        assert!(!backend
            .events
            .contains(&"recovery_before_rollforward_restart"));
    }

    #[test]
    fn disabled_precommit_failure_rolls_back_without_selected_runtime_stop() {
        let fixture = Fixture::new("disabled-rollback");
        let plan = fixture_raw_fallback_plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let mut backend = FakeBackend {
            fail: Some(("verify_applied", 1)),
            candidate_files_at_quiesce: Some((fixture.cake.clone(), fixture.sqm.clone())),
            ..FakeBackend::default()
        };
        let error =
            execute_native_bootstrap_apply_commit(&plan, &manifest, fixture.paths(), &mut backend)
                .unwrap_err();

        assert!(error.contains("exact absent rollback verified"), "{error}");
        assert_eq!(fs::read(&fixture.cake).unwrap(), fixture.cake_original);
        assert_eq!(fs::read(&fixture.sqm).unwrap(), fixture.sqm_original);
        assert!(fixture.store().read_record().unwrap().is_none());
        assert!(backend.events.contains(&"quiesce_disabled_candidate"));
        assert!(!backend.events.contains(&"quiesce_candidate"));
        assert!(!backend.events.contains(&"restart"));
    }

    #[test]
    fn commit_orders_every_attestation_and_clears_durable_recovery() {
        let fixture = Fixture::new("commit");
        let plan = fixture_plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let mut backend = FakeBackend::default();
        let receipt =
            execute_native_bootstrap_apply_commit(&plan, &manifest, fixture.paths(), &mut backend)
                .unwrap();

        assert_eq!(receipt.disposition, NativeApplyCommitDisposition::Applied);
        assert!(receipt.recovery_cleared);
        assert!(fixture.store().read_record().unwrap().is_none());
        assert!(String::from_utf8(fs::read(&fixture.cake).unwrap())
            .unwrap()
            .contains("config cake_autorate 'wan_sqm'"));
        assert!(String::from_utf8(fs::read(&fixture.sqm).unwrap())
            .unwrap()
            .contains("config queue 'cake_wan_sqm'"));
        assert_eq!(
            backend.events,
            vec![
                "candidate_already",
                "absent",
                "absent",
                "candidate_before_restart",
                "restart",
                "verify_applied",
                "verify_applied",
                "verify_applied",
            ]
        );
    }

    #[test]
    fn precommit_failure_restores_exact_absence_and_clears_recovery() {
        let fixture = Fixture::new("rollback");
        let plan = fixture_plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let mut backend = FakeBackend {
            fail: Some(("verify_applied", 1)),
            candidate_files_at_quiesce: Some((fixture.cake.clone(), fixture.sqm.clone())),
            ..FakeBackend::default()
        };
        let error =
            execute_native_bootstrap_apply_commit(&plan, &manifest, fixture.paths(), &mut backend)
                .unwrap_err();

        assert!(error.contains("exact absent rollback verified"), "{error}");
        assert_eq!(fs::read(&fixture.cake).unwrap(), fixture.cake_original);
        assert_eq!(fs::read(&fixture.sqm).unwrap(), fixture.sqm_original);
        assert!(fixture.store().read_record().unwrap().is_none());
        assert!(backend.events.ends_with(&[
            "discard_pending",
            "quiesce_candidate",
            "discard_pending",
            "verify_absent",
        ]));
        assert!(!backend.events.contains(&"contain"));
    }

    #[test]
    fn partial_precommit_pair_is_reconstructed_before_selected_runtime_is_quiesced() {
        let fixture = Fixture::new("partial-precommit-rollback");
        let plan = fixture_plan();
        let store = fixture.store();
        store
            .prepare(
                &plan,
                &plan.canonical_manifest_bytes().unwrap(),
                &fixture.cake,
                &fixture.sqm,
            )
            .unwrap();
        let mutation = store.begin_mutation(&fixture.cake, &fixture.sqm).unwrap();
        mutation.install_candidate_files().unwrap();
        drop(mutation);
        // Model power loss after only one package has survived as candidate.
        fs::write(&fixture.sqm, &fixture.sqm_original).unwrap();

        let mut backend = FakeBackend {
            candidate_files_at_quiesce: Some((fixture.cake.clone(), fixture.sqm.clone())),
            ..FakeBackend::default()
        };
        let receipt = recover_native_bootstrap_apply(fixture.paths(), &mut backend)
            .unwrap()
            .unwrap();

        assert!(!receipt.rolled_forward);
        assert!(receipt.recovery_cleared);
        assert_eq!(fs::read(&fixture.cake).unwrap(), fixture.cake_original);
        assert_eq!(fs::read(&fixture.sqm).unwrap(), fixture.sqm_original);
        assert_eq!(
            backend.events,
            vec![
                "discard_pending",
                "quiesce_candidate",
                "discard_pending",
                "verify_absent",
            ]
        );
    }

    #[test]
    fn failed_second_absence_attestation_never_installs_candidates() {
        let fixture = Fixture::new("absence-drift");
        let plan = fixture_plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let mut backend = FakeBackend {
            fail: Some(("absent", 2)),
            ..FakeBackend::default()
        };
        let error =
            execute_native_bootstrap_apply_commit(&plan, &manifest, fixture.paths(), &mut backend)
                .unwrap_err();

        assert!(error.contains("injected absent failure"));
        assert!(error.contains("exact absent rollback verified"));
        assert_eq!(fs::read(&fixture.cake).unwrap(), fixture.cake_original);
        assert_eq!(fs::read(&fixture.sqm).unwrap(), fixture.sqm_original);
        assert!(!backend.events.contains(&"restart"));
        assert!(fixture.store().read_record().unwrap().is_none());
    }

    #[test]
    fn prepared_recovery_is_read_only_and_does_not_restart() {
        let fixture = Fixture::new("prepared-recovery");
        let plan = fixture_plan();
        fixture
            .store()
            .prepare(
                &plan,
                &plan.canonical_manifest_bytes().unwrap(),
                &fixture.cake,
                &fixture.sqm,
            )
            .unwrap();
        let mut backend = FakeBackend::default();
        let receipt = recover_native_bootstrap_apply(fixture.paths(), &mut backend)
            .unwrap()
            .unwrap();

        assert!(!receipt.rolled_forward);
        assert!(receipt.recovery_cleared);
        assert_eq!(fs::read(&fixture.cake).unwrap(), fixture.cake_original);
        assert_eq!(fs::read(&fixture.sqm).unwrap(), fixture.sqm_original);
        assert_eq!(backend.events, vec!["verify_absent"]);
    }

    #[test]
    fn foreign_prepared_state_is_preserved_without_discard_or_containment() {
        let fixture = Fixture::new("prepared-foreign");
        let plan = fixture_plan();
        fixture
            .store()
            .prepare(
                &plan,
                &plan.canonical_manifest_bytes().unwrap(),
                &fixture.cake,
                &fixture.sqm,
            )
            .unwrap();
        let mut backend = FakeBackend {
            fail: Some(("verify_absent", 1)),
            ..FakeBackend::default()
        };
        let error = recover_native_bootstrap_apply(fixture.paths(), &mut backend).unwrap_err();
        assert!(error.contains("injected verify_absent failure"));
        assert!(error.contains("containment was not invoked"));
        assert_eq!(backend.events, vec!["verify_absent"]);
        assert!(!backend.events.contains(&"discard_pending"));
        assert!(!backend.events.contains(&"contain"));
        assert_eq!(
            fixture.store().read_record().unwrap().unwrap().state,
            NativeBootstrapApplyRecoveryState::Prepared
        );
        assert_eq!(fs::read(&fixture.cake).unwrap(), fixture.cake_original);
        assert_eq!(fs::read(&fixture.sqm).unwrap(), fixture.sqm_original);
    }

    #[test]
    fn commit_accepted_partial_pair_rolls_forward_after_restart() {
        let fixture = Fixture::new("rollforward");
        let plan = fixture_plan();
        let store = fixture.store();
        store
            .prepare(
                &plan,
                &plan.canonical_manifest_bytes().unwrap(),
                &fixture.cake,
                &fixture.sqm,
            )
            .unwrap();
        let mutation = store.begin_mutation(&fixture.cake, &fixture.sqm).unwrap();
        mutation.install_candidate_files().unwrap();
        drop(mutation);
        for (expected, next) in [
            (
                NativeBootstrapApplyRecoveryState::MutationStarted,
                NativeBootstrapApplyRecoveryState::ServiceRestarted,
            ),
            (
                NativeBootstrapApplyRecoveryState::ServiceRestarted,
                NativeBootstrapApplyRecoveryState::Verified,
            ),
            (
                NativeBootstrapApplyRecoveryState::Verified,
                NativeBootstrapApplyRecoveryState::CommitAccepted,
            ),
        ] {
            store.transition(expected, next).unwrap();
        }
        let cake_candidate = fs::read(&fixture.cake).unwrap();
        let sqm_candidate = fs::read(&fixture.sqm).unwrap();
        fs::write(&fixture.sqm, &fixture.sqm_original).unwrap();

        let mut backend = FakeBackend::default();
        let receipt = recover_native_bootstrap_apply(fixture.paths(), &mut backend)
            .unwrap()
            .unwrap();
        assert!(receipt.rolled_forward);
        assert!(receipt.recovery_cleared);
        assert_eq!(fs::read(&fixture.cake).unwrap(), cake_candidate);
        assert_eq!(fs::read(&fixture.sqm).unwrap(), sqm_candidate);
        assert_eq!(
            backend.events,
            vec![
                "discard_pending",
                "recovery_before_rollforward_restart",
                "restart",
                "verify_recovered_candidate",
            ]
        );
    }

    #[test]
    fn failed_final_attestation_retains_commit_for_rollforward_recovery() {
        let fixture = Fixture::new("commit-final-attestation");
        let plan = fixture_plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let mut backend = FakeBackend {
            fail: Some(("verify_applied", 3)),
            ..FakeBackend::default()
        };
        let error =
            execute_native_bootstrap_apply_commit(&plan, &manifest, fixture.paths(), &mut backend)
                .unwrap_err();
        assert!(error.contains("commit was accepted"));
        assert_eq!(
            fixture.store().read_record().unwrap().unwrap().state,
            NativeBootstrapApplyRecoveryState::CommitAccepted
        );

        let mut recovery_backend = FakeBackend::default();
        let receipt = recover_native_bootstrap_apply(fixture.paths(), &mut recovery_backend)
            .unwrap()
            .unwrap();
        assert!(receipt.rolled_forward);
        assert!(receipt.recovery_cleared);
        assert!(recovery_backend
            .events
            .contains(&"verify_recovered_candidate"));
    }

    #[test]
    fn corrupt_durable_request_is_rejected_before_global_lock_or_backend() {
        let fixture = Fixture::new("corrupt-request");
        let plan = fixture_plan();
        fixture
            .store()
            .prepare(
                &plan,
                &plan.canonical_manifest_bytes().unwrap(),
                &fixture.cake,
                &fixture.sqm,
            )
            .unwrap();
        let request_path = fixture
            .recovery
            .join("bootstrap-v6")
            .join("current")
            .join("request");
        fs::write(request_path, b"corrupt\n").unwrap();

        let mut backend = FakeBackend::default();
        let error = recover_native_bootstrap_apply(fixture.paths(), &mut backend).unwrap_err();
        assert!(error.contains("request digest mismatch"));
        assert!(!fixture.lock.exists());
        assert!(backend.events.is_empty());
        assert!(fixture.store().read_record().unwrap().is_some());
    }

    #[test]
    fn exact_already_applied_candidate_is_the_only_journal_free_success() {
        let fixture = Fixture::new("already-applied");
        let plan = fixture_plan();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let mut backend = FakeBackend {
            already_applied: true,
            ..FakeBackend::default()
        };
        let receipt =
            execute_native_bootstrap_apply_commit(&plan, &manifest, fixture.paths(), &mut backend)
                .unwrap();
        assert_eq!(
            receipt.disposition,
            NativeApplyCommitDisposition::AlreadyApplied
        );
        assert_eq!(backend.events, vec!["candidate_already"]);
        assert!(fixture.store().read_record().unwrap().is_none());
        assert_eq!(fs::read(&fixture.cake).unwrap(), fixture.cake_original);
        assert_eq!(fs::read(&fixture.sqm).unwrap(), fixture.sqm_original);
    }

    #[test]
    fn malformed_manifest_is_rejected_before_lock_or_recovery_creation() {
        let fixture = Fixture::new("manifest");
        let plan = fixture_plan();
        let mut backend = FakeBackend::default();
        let error =
            execute_native_bootstrap_apply_commit(&plan, b"{}\n", fixture.paths(), &mut backend)
                .unwrap_err();
        assert!(error.contains("manifest changed"));
        assert!(!fixture.lock.exists());
        assert!(!fixture.recovery.exists());
        assert!(backend.events.is_empty());
    }
}
