//! Store-backed driver for the pure Auto-Tune runtime ownership state machine.
//!
//! The OpenWrt instance controller supplies the actuator.  This driver owns the
//! ordering: checkpoint before mutation, `RESTORING` ACK before restore, and
//! exact runtime attestation before `APPLIED` or `RESTORED` is published.

use super::autotune_runtime::{
    AutotuneRuntimePermit, RuntimeBaseline, RuntimeIdentityHealth, RuntimeOverrideAction,
    RuntimeOverrideHealth, RuntimeOverridePhase, RuntimeOverrideTracker, RuntimeRestoreBaseline,
    RuntimeRestoreReason, RuntimeRouteRearmHealth, RuntimeSnapshot, TemporaryTopologyStage,
};
use super::autotune_runtime_store::{RuntimeOverrideCheckpoint, RuntimeOverrideStore};
use super::full_autotune::{AutotuneRuntimeAck, AutotuneRuntimeControl, RuntimeAckState};
use super::identity::ProcessIdentity;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeRestoreBlocker {
    TargetUnavailable {
        target_interface: String,
        detail: String,
    },
    SqmRecoveryBusy {
        target_interface: String,
        detail: String,
    },
    TopologySettling {
        target_interface: String,
        detail: String,
    },
    RecoveryInterrupted {
        target_interface: String,
        detail: String,
    },
}

impl RuntimeRestoreBlocker {
    pub fn detail(&self) -> &str {
        match self {
            Self::TargetUnavailable { detail, .. }
            | Self::SqmRecoveryBusy { detail, .. }
            | Self::TopologySettling { detail, .. }
            | Self::RecoveryInterrupted { detail, .. } => detail,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeRestoreObservation {
    Blocked(RuntimeRestoreBlocker),
    Ready,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeActuatorError {
    Blocked(RuntimeRestoreBlocker),
    Unsafe(String),
}

impl std::fmt::Display for RuntimeActuatorError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Blocked(blocker) => formatter.write_str(blocker.detail()),
            Self::Unsafe(message) => formatter.write_str(message),
        }
    }
}

pub trait RuntimeOverrideActuator {
    fn current_boot_ms(&self) -> u64;
    fn capture_baseline(
        &mut self,
        permit: &AutotuneRuntimePermit,
    ) -> Result<RuntimeRestoreBaseline, String>;
    fn current_route_identity(&mut self) -> Result<String, String>;
    /// Prove the exact managed baseline or bootstrap absence immediately
    /// before the first mutation and after exact restoration.
    fn baseline_ready_for_apply(&mut self, permit: &AutotuneRuntimePermit) -> Result<bool, String>;
    /// Re-attest immutable ownership identity while the requested private
    /// topology is active.  Bootstrap actuators must allow their own topology
    /// here while still rejecting UCI, route, ifindex, namespace or foreign
    /// topology drift.
    fn active_identity_matches(&mut self, permit: &AutotuneRuntimePermit) -> Result<bool, String>;
    fn worker_identity_matches(&self, worker: &ProcessIdentity) -> bool;
    fn runtime_matches(
        &mut self,
        expected: &RuntimeSnapshot,
        checkpoint: &RuntimeOverrideCheckpoint,
    ) -> Result<bool, String>;
    fn prepare_baseline(
        &mut self,
        checkpoint: &RuntimeOverrideCheckpoint,
    ) -> Result<(), RuntimeActuatorError>;
    fn create_temporary_ifb(
        &mut self,
        checkpoint: &RuntimeOverrideCheckpoint,
    ) -> Result<u32, RuntimeActuatorError>;
    fn apply_override(
        &mut self,
        control: &AutotuneRuntimeControl,
        expected: &RuntimeSnapshot,
        checkpoint: &RuntimeOverrideCheckpoint,
    ) -> Result<(), RuntimeActuatorError>;
    fn remove_temporary_topology(
        &mut self,
        checkpoint: &RuntimeOverrideCheckpoint,
    ) -> Result<(), RuntimeActuatorError>;
    fn restore_baseline(
        &mut self,
        baseline: &RuntimeRestoreBaseline,
    ) -> Result<(), RuntimeActuatorError>;
    fn observe_restore_blocker(
        &mut self,
        blocker: &RuntimeRestoreBlocker,
        baseline: &RuntimeRestoreBaseline,
    ) -> Result<RuntimeRestoreObservation, String>;
    fn attest_runtime(
        &mut self,
        expected: &RuntimeSnapshot,
        checkpoint: &RuntimeOverrideCheckpoint,
    ) -> Result<RuntimeSnapshot, RuntimeActuatorError>;
    fn attest_restored_baseline(
        &mut self,
        baseline: &RuntimeRestoreBaseline,
        checkpoint: &RuntimeOverrideCheckpoint,
    ) -> Result<RuntimeRestoreBaseline, RuntimeActuatorError>;
    fn finish_restored(&mut self);
    fn recover_safe_configuration(&mut self) -> Result<(), String>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeDriverOutcome {
    Idle,
    PermitAwaitingControl,
    Applying,
    Applied,
    Restoring,
    Restored,
    Rejected,
    UnsafeRecoveryRequired,
}

pub struct RuntimeOverrideDriver {
    instance_name: String,
    store: RuntimeOverrideStore,
    tracker: RuntimeOverrideTracker,
    startup_reconciled: bool,
    unsafe_recovery_reason: Option<String>,
    restore_blocker: Option<RuntimeRestoreBlocker>,
}

impl RuntimeOverrideDriver {
    pub fn open(instance_name: String, instance_run_dir: &Path) -> Result<Self, String> {
        let tracker = RuntimeOverrideTracker::new(instance_name.clone())?;
        Ok(Self {
            instance_name,
            store: RuntimeOverrideStore::open(instance_run_dir)?,
            tracker,
            startup_reconciled: false,
            unsafe_recovery_reason: None,
            restore_blocker: None,
        })
    }

    #[cfg(test)]
    pub fn phase(&self) -> RuntimeOverridePhase {
        self.tracker.phase()
    }

    pub fn unsafe_recovery_reason(&self) -> Option<&str> {
        self.unsafe_recovery_reason.as_deref()
    }

    pub fn poll(
        &mut self,
        actuator: &mut impl RuntimeOverrideActuator,
    ) -> Result<RuntimeDriverOutcome, String> {
        if self.unsafe_recovery_reason.is_some() {
            return Ok(RuntimeDriverOutcome::UnsafeRecoveryRequired);
        }

        if !self.startup_reconciled {
            self.startup_reconciled = true;
            match self.reconcile_startup(actuator) {
                Ok(Some(RuntimeDriverOutcome::UnsafeRecoveryRequired)) => {
                    self.unsafe_recovery_reason = Some(
                        "an orphaned runtime checkpoint required safe configuration recovery"
                            .to_string(),
                    );
                    return Ok(RuntimeDriverOutcome::UnsafeRecoveryRequired);
                }
                Ok(Some(outcome)) => return Ok(outcome),
                Ok(None) => {}
                Err(error) => {
                    self.unsafe_recovery_reason = Some(error.clone());
                    return Err(error);
                }
            }
        }

        let permit = match self.store.read_permit() {
            Ok(permit) => permit,
            Err(error) => {
                return self.handle_unreadable_active_state(actuator, &error);
            }
        };
        let control = match self.store.read_control() {
            Ok(control) => control,
            Err(error) => {
                return self.handle_unreadable_active_state(actuator, &error);
            }
        };
        let restore_intent = match self.store.read_restore_intent() {
            Ok(intent) => intent,
            Err(error) => {
                return self.handle_unreadable_active_state(actuator, &error);
            }
        };

        if self.tracker.phase() == RuntimeOverridePhase::Restored {
            let requested = self
                .tracker
                .requested_control()
                .cloned()
                .ok_or_else(|| "restored runtime control is missing".to_string())?;
            if self.tracker.restore_reason() == Some(RuntimeRestoreReason::ControlMissing)
                && restore_intent.as_ref() != Some(&requested)
            {
                return self.handle_unreadable_active_state(
                    actuator,
                    "restored runtime intent does not match the exact control",
                );
            }
            if let (Some(permit), Some(control)) = (permit.as_ref(), control.as_ref()) {
                if control != &requested {
                    let owner = self
                        .tracker
                        .owner_permit()
                        .cloned()
                        .ok_or_else(|| "restored runtime owner is missing".to_string())?;
                    let baseline = self
                        .tracker
                        .baseline()
                        .cloned()
                        .ok_or_else(|| "restored runtime baseline is missing".to_string())?;
                    let checkpoint = self.store.read_checkpoint()?.ok_or_else(|| {
                        "route-recovery re-arm has no durable runtime checkpoint".to_string()
                    })?;
                    checkpoint.validate_against(&owner)?;
                    if checkpoint.baseline != baseline {
                        return self.handle_unreadable_active_state(
                            actuator,
                            "route-recovery re-arm checkpoint changed from its exact baseline",
                        );
                    }
                    let now = actuator.current_boot_ms();
                    let route_identity = actuator.current_route_identity().unwrap_or_default();
                    let baseline_identity = RuntimeIdentityHealth::from_attestation(
                        actuator.baseline_ready_for_apply(&owner),
                    );
                    let baseline_runtime_matches = actuator
                        .attest_restored_baseline(&baseline, &checkpoint)
                        .is_ok_and(|attested| attested == baseline);
                    let action = self.tracker.rearm_after_route_recovery(
                        permit.clone(),
                        control.clone(),
                        RuntimeRouteRearmHealth {
                            current_boot_ms: now,
                            restore_intent_present: restore_intent.is_some(),
                            worker_identity_matches: actuator
                                .worker_identity_matches(&owner.worker),
                            route_identity: &route_identity,
                            baseline_identity,
                            baseline_runtime_matches,
                        },
                    );
                    let outcome = self.execute(action, actuator)?;
                    if outcome != RuntimeDriverOutcome::Restored {
                        return Ok(outcome);
                    }
                }
            }
            if permit.is_none() && control.is_none() {
                let owner = self
                    .tracker
                    .owner_permit()
                    .cloned()
                    .ok_or_else(|| "restored runtime owner is missing".to_string())?;
                self.tracker
                    .release(&owner.permit_id, &owner.job_id, &owner.worker_run_id)?;
                self.store.finalize_restored()?;
                return Ok(RuntimeDriverOutcome::Idle);
            }
            let now = actuator.current_boot_ms();
            let durable_ack = self.store.read_ack()?.is_some_and(|ack| {
                ack.state == RuntimeAckState::Restored
                    && ack.attests_restored(&requested, now).is_ok()
            });
            if !durable_ack {
                let ack = self
                    .tracker
                    .restored_ack(now)
                    .ok_or_else(|| "restored runtime ACK could not be reconstructed".to_string())?;
                self.store.publish_ack(&ack)?;
            }
            return Ok(RuntimeDriverOutcome::Restored);
        }

        if self.tracker.phase() == RuntimeOverridePhase::Restoring {
            if let Some(blocker) = self.restore_blocker.clone() {
                let baseline = self
                    .tracker
                    .baseline()
                    .cloned()
                    .ok_or_else(|| "restoring runtime baseline is missing".to_string())?;
                match actuator.observe_restore_blocker(&blocker, &baseline) {
                    Ok(RuntimeRestoreObservation::Blocked(current)) => {
                        self.restore_blocker = Some(current);
                        return Ok(RuntimeDriverOutcome::Restoring);
                    }
                    Ok(RuntimeRestoreObservation::Ready) => {
                        self.restore_blocker = None;
                    }
                    Err(message) => {
                        return self.fail_restore_safely(
                            actuator,
                            format!("runtime restore blocker observation failed: {message}"),
                        )
                    }
                }
            }
            return self.execute(
                self.tracker.retry_restore(actuator.current_boot_ms()),
                actuator,
            );
        }

        if self.tracker.phase() == RuntimeOverridePhase::Idle {
            return match (permit, control) {
                (Some(permit), Some(control)) => {
                    let now = actuator.current_boot_ms();
                    if permit
                        .authorizes(&self.instance_name, &control, now)
                        .is_err()
                    {
                        let action = self.tracker.submit(permit, control, None, now);
                        return self.execute(action, actuator);
                    }
                    let preflight_rejection = if !actuator.worker_identity_matches(&permit.worker) {
                        Some("worker-identity-mismatch")
                    } else if actuator.current_route_identity().ok().as_deref()
                        != Some(permit.route_identity.as_str())
                    {
                        Some("route-drift")
                    } else {
                        match actuator.baseline_ready_for_apply(&permit) {
                            Ok(true) => None,
                            Ok(false) => Some(match &permit.baseline {
                                RuntimeBaseline::Managed(_) => "sqm-drift",
                                RuntimeBaseline::Absent(_) => "absence-drift",
                            }),
                            Err(_) => Some(match &permit.baseline {
                                RuntimeBaseline::Managed(_) => "sqm-attestation-unavailable",
                                RuntimeBaseline::Absent(_) => "absence-attestation-unavailable",
                            }),
                        }
                    };
                    if let Some(code) = preflight_rejection {
                        let action = self.tracker.reject_before_apply(&control, now, code);
                        return self.execute(action, actuator);
                    }
                    let baseline = actuator.capture_baseline(&permit)?;
                    let checkpoint =
                        RuntimeOverrideCheckpoint::new(&permit, now, baseline.clone())?;
                    // The baseline is durable before the tracker can return Apply.
                    self.store.publish_checkpoint(&checkpoint)?;
                    let action = self.tracker.submit(permit, control, Some(baseline), now);
                    self.execute(action, actuator)
                }
                (Some(permit), None) => {
                    let now = actuator.current_boot_ms();
                    if permit.validate().is_ok()
                        && permit.instance_name == self.instance_name
                        && now > 0
                        && now < permit.deadline_boot_ms
                        && actuator.worker_identity_matches(&permit.worker)
                    {
                        Ok(RuntimeDriverOutcome::PermitAwaitingControl)
                    } else {
                        Ok(RuntimeDriverOutcome::Rejected)
                    }
                }
                (None, None) => Ok(RuntimeDriverOutcome::Idle),
                (None, Some(_)) => Ok(RuntimeDriverOutcome::Rejected),
            };
        }

        if let (Some(permit), Some(control)) = (permit.as_ref(), control.as_ref()) {
            let action = self.tracker.submit(
                permit.clone(),
                control.clone(),
                None,
                actuator.current_boot_ms(),
            );
            let outcome = self.execute(action, actuator)?;
            if !matches!(
                outcome,
                RuntimeDriverOutcome::Idle
                    | RuntimeDriverOutcome::Applied
                    | RuntimeDriverOutcome::Rejected
            ) {
                return Ok(outcome);
            }
        }

        if matches!(
            self.tracker.phase(),
            RuntimeOverridePhase::Applying | RuntimeOverridePhase::Applied
        ) {
            let owner = self
                .tracker
                .owner_permit()
                .cloned()
                .ok_or_else(|| "active runtime owner is missing".to_string())?;
            let requested = self
                .tracker
                .requested_control()
                .cloned()
                .ok_or_else(|| "active runtime control is missing".to_string())?;
            let permit_present = permit.as_ref() == Some(&owner);
            let control_present = control.as_ref() == Some(&requested);
            let route_identity = actuator.current_route_identity().unwrap_or_default();
            let baseline = self
                .tracker
                .baseline()
                .cloned()
                .ok_or_else(|| "active runtime baseline is missing".to_string())?;
            let checkpoint = self
                .store
                .read_checkpoint()?
                .ok_or_else(|| "active runtime checkpoint is missing".to_string())?;
            checkpoint.validate_against(&owner)?;
            if checkpoint.baseline != baseline {
                return self.handle_unreadable_active_state(
                    actuator,
                    "active runtime checkpoint changed from its exact baseline",
                );
            }
            let baseline_identity =
                RuntimeIdentityHealth::from_attestation(actuator.active_identity_matches(&owner));
            let applied_runtime_matches = if self.tracker.phase() == RuntimeOverridePhase::Applied {
                let expected = runtime_snapshot_for_control(&owner, &requested);
                actuator
                    .runtime_matches(&expected, &checkpoint)
                    .unwrap_or(false)
            } else {
                true
            };
            let action = self.tracker.tick(RuntimeOverrideHealth {
                current_boot_ms: actuator.current_boot_ms(),
                permit_present,
                control_present,
                worker_identity_matches: actuator.worker_identity_matches(&owner.worker),
                route_identity: &route_identity,
                baseline_identity,
                applied_runtime_matches,
            });
            return self.execute(action, actuator);
        }

        Ok(outcome_for_phase(self.tracker.phase()))
    }

    fn reconcile_startup(
        &mut self,
        actuator: &mut impl RuntimeOverrideActuator,
    ) -> Result<Option<RuntimeDriverOutcome>, String> {
        let checkpoint = match self.store.read_checkpoint() {
            Ok(checkpoint) => checkpoint,
            Err(error) => {
                actuator.recover_safe_configuration()?;
                return Err(format!(
                    "runtime checkpoint is unreadable; safe recovery was requested: {error}"
                ));
            }
        };
        let permit = self.store.read_permit()?;
        let control = self.store.read_control()?;
        let restore_intent = self.store.read_restore_intent()?;
        let ack = self.store.read_ack()?;
        let Some(checkpoint) = checkpoint else {
            if restore_intent.is_some() {
                if permit.is_none() && control.is_none() && ack.is_none() {
                    self.store.finalize_restored()?;
                    return Ok(Some(RuntimeDriverOutcome::Idle));
                }
                actuator.recover_safe_configuration()?;
                return Ok(Some(RuntimeDriverOutcome::UnsafeRecoveryRequired));
            }
            return Ok(None);
        };

        if let Some(restore_control) = restore_intent {
            if control
                .as_ref()
                .is_some_and(|value| value != &restore_control)
            {
                actuator.recover_safe_configuration()?;
                return Ok(Some(RuntimeDriverOutcome::UnsafeRecoveryRequired));
            }
            if let Some(permit) = permit {
                checkpoint.validate_against(&permit)?;
                let (tracker, action) = RuntimeOverrideTracker::recover_after_restart(
                    permit.instance_name.clone(),
                    permit,
                    restore_control,
                    checkpoint.baseline,
                    actuator.current_boot_ms(),
                )?;
                self.tracker = tracker;
                return self.execute(action, actuator).map(Some);
            }
            if control.is_none() {
                if let Some(ack) = ack {
                    let now = actuator.current_boot_ms();
                    if ack.attests_restored(&restore_control, now).is_ok() {
                        let attested = actuator
                            .attest_restored_baseline(&checkpoint.baseline, &checkpoint)
                            .map_err(|error| {
                                format!("restored startup runtime could not be attested: {error}")
                            })?;
                        if attested == checkpoint.baseline {
                            actuator.finish_restored();
                            self.store.finalize_restored()?;
                            return Ok(Some(RuntimeDriverOutcome::Idle));
                        }
                    }
                }
            }
            actuator.recover_safe_configuration()?;
            return Ok(Some(RuntimeDriverOutcome::UnsafeRecoveryRequired));
        }

        let (Some(permit), Some(control)) = (permit, control) else {
            actuator.recover_safe_configuration()?;
            return Ok(Some(RuntimeDriverOutcome::UnsafeRecoveryRequired));
        };
        checkpoint.validate_against(&permit)?;
        let (tracker, action) = RuntimeOverrideTracker::recover_after_restart(
            permit.instance_name.clone(),
            permit,
            control,
            checkpoint.baseline,
            actuator.current_boot_ms(),
        )?;
        self.tracker = tracker;
        self.execute(action, actuator).map(Some)
    }

    fn handle_unreadable_active_state(
        &mut self,
        actuator: &mut impl RuntimeOverrideActuator,
        error: &str,
    ) -> Result<RuntimeDriverOutcome, String> {
        if self.tracker.phase() == RuntimeOverridePhase::Idle {
            return Err(error.to_string());
        }
        actuator.recover_safe_configuration()?;
        let diagnostic = format!(
            "active runtime ownership record is unreadable; safe recovery was requested: {error}"
        );
        self.unsafe_recovery_reason = Some(diagnostic.clone());
        Err(diagnostic)
    }

    fn execute(
        &mut self,
        action: RuntimeOverrideAction,
        actuator: &mut impl RuntimeOverrideActuator,
    ) -> Result<RuntimeDriverOutcome, String> {
        match action {
            RuntimeOverrideAction::None => Ok(outcome_for_phase(self.tracker.phase())),
            RuntimeOverrideAction::MalformedControl => Ok(RuntimeDriverOutcome::Rejected),
            RuntimeOverrideAction::PublishAck(ack) => {
                self.store.publish_ack(&ack)?;
                Ok(outcome_for_ack(&ack))
            }
            RuntimeOverrideAction::Apply(control) => {
                self.restore_blocker = None;
                let owner = self
                    .tracker
                    .owner_permit()
                    .cloned()
                    .ok_or_else(|| "applying runtime permit is missing".to_string())?;
                let baseline = self
                    .tracker
                    .baseline()
                    .cloned()
                    .ok_or_else(|| "applying runtime baseline is missing".to_string())?;
                let expected = runtime_snapshot_for_control(&owner, &control);
                let mut checkpoint = self
                    .store
                    .read_checkpoint()?
                    .ok_or_else(|| "applying runtime checkpoint is missing".to_string())?;
                checkpoint.validate_against(&owner)?;
                if checkpoint.baseline != baseline {
                    return self.fail_restore_safely(
                        actuator,
                        "applying runtime checkpoint changed from its exact baseline".to_string(),
                    );
                }
                let apply_result = (|| -> Result<(), RuntimeActuatorError> {
                    if checkpoint.temporary_stage == TemporaryTopologyStage::BaselineRestored {
                        let next = checkpoint
                            .advance_temporary_stage(TemporaryTopologyStage::Planned, None)
                            .map_err(RuntimeActuatorError::Unsafe)?;
                        self.store
                            .replace_checkpoint(&checkpoint, &next)
                            .map_err(RuntimeActuatorError::Unsafe)?;
                        checkpoint = next;
                    }
                    if checkpoint.temporary_stage == TemporaryTopologyStage::Planned {
                        actuator.prepare_baseline(&checkpoint)?;
                        let prepared_stage = match &checkpoint.baseline {
                            RuntimeBaseline::Managed(_) => {
                                TemporaryTopologyStage::ManagedSqmSuspended
                            }
                            RuntimeBaseline::Absent(_) => TemporaryTopologyStage::AbsenceAttested,
                        };
                        let next = checkpoint
                            .advance_temporary_stage(prepared_stage, None)
                            .map_err(RuntimeActuatorError::Unsafe)?;
                        self.store
                            .replace_checkpoint(&checkpoint, &next)
                            .map_err(RuntimeActuatorError::Unsafe)?;
                        checkpoint = next;
                    }
                    if matches!(
                        checkpoint.temporary_stage,
                        TemporaryTopologyStage::ManagedSqmSuspended
                            | TemporaryTopologyStage::AbsenceAttested
                    ) {
                        let ifindex = actuator.create_temporary_ifb(&checkpoint)?;
                        let next = checkpoint
                            .advance_temporary_stage(
                                TemporaryTopologyStage::LinkOwned,
                                Some(ifindex),
                            )
                            .map_err(RuntimeActuatorError::Unsafe)?;
                        self.store
                            .replace_checkpoint(&checkpoint, &next)
                            .map_err(RuntimeActuatorError::Unsafe)?;
                        checkpoint = next;
                    }
                    match checkpoint.temporary_stage {
                        TemporaryTopologyStage::LinkOwned => {
                            actuator.apply_override(&control, &expected, &checkpoint)?;
                            let next = checkpoint
                                .advance_temporary_stage(TemporaryTopologyStage::Active, None)
                                .map_err(RuntimeActuatorError::Unsafe)?;
                            self.store
                                .replace_checkpoint(&checkpoint, &next)
                                .map_err(RuntimeActuatorError::Unsafe)?;
                            checkpoint = next;
                        }
                        TemporaryTopologyStage::Active => {
                            actuator.apply_override(&control, &expected, &checkpoint)?;
                        }
                        _ => {
                            return Err(RuntimeActuatorError::Unsafe(format!(
                                "runtime override cannot apply from temporary stage {}",
                                checkpoint.temporary_stage.as_str()
                            )))
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = apply_result {
                    let restore = self.tracker.apply_failed(actuator.current_boot_ms());
                    let outcome = self.execute(restore, actuator)?;
                    if outcome == RuntimeDriverOutcome::Restoring {
                        return Ok(outcome);
                    }
                    return Err(format!("runtime override application failed: {error}"));
                }
                let attested = match actuator.attest_runtime(&expected, &checkpoint) {
                    Ok(attested) => attested,
                    Err(error) => {
                        let restore = self.tracker.apply_failed(actuator.current_boot_ms());
                        let outcome = self.execute(restore, actuator)?;
                        if outcome == RuntimeDriverOutcome::Restoring {
                            return Ok(outcome);
                        }
                        return Err(format!("applied runtime could not be attested: {error}"));
                    }
                };
                let action = self
                    .tracker
                    .confirm_applied(&attested, actuator.current_boot_ms());
                self.execute(action, actuator)
            }
            RuntimeOverrideAction::Restore { baseline, ack, .. } => {
                // A worker must observe RESTORING before the actuator starts
                // changing the live topology back to its baseline.
                self.store.publish_ack(&ack)?;
                let owner = self
                    .tracker
                    .owner_permit()
                    .cloned()
                    .ok_or_else(|| "restoring runtime permit is missing".to_string())?;
                let mut checkpoint = self
                    .store
                    .read_checkpoint()?
                    .ok_or_else(|| "restoring runtime checkpoint is missing".to_string())?;
                checkpoint.validate_against(&owner)?;
                if checkpoint.baseline != baseline {
                    return self.fail_restore_safely(
                        actuator,
                        "restoring runtime checkpoint changed from its exact baseline".to_string(),
                    );
                }
                let restore_result = (|| -> Result<(), RuntimeActuatorError> {
                    if !matches!(
                        checkpoint.temporary_stage,
                        TemporaryTopologyStage::TemporaryAbsent
                            | TemporaryTopologyStage::BaselineRestored
                    ) {
                        actuator.remove_temporary_topology(&checkpoint)?;
                        let next = checkpoint
                            .advance_temporary_stage(TemporaryTopologyStage::TemporaryAbsent, None)
                            .map_err(RuntimeActuatorError::Unsafe)?;
                        self.store
                            .replace_checkpoint(&checkpoint, &next)
                            .map_err(RuntimeActuatorError::Unsafe)?;
                        checkpoint = next;
                    }
                    if checkpoint.temporary_stage == TemporaryTopologyStage::TemporaryAbsent {
                        actuator.restore_baseline(&baseline)?;
                        let next = checkpoint
                            .advance_temporary_stage(TemporaryTopologyStage::BaselineRestored, None)
                            .map_err(RuntimeActuatorError::Unsafe)?;
                        self.store
                            .replace_checkpoint(&checkpoint, &next)
                            .map_err(RuntimeActuatorError::Unsafe)?;
                        checkpoint = next;
                    }
                    if checkpoint.temporary_stage != TemporaryTopologyStage::BaselineRestored {
                        return Err(RuntimeActuatorError::Unsafe(format!(
                            "runtime baseline cannot attest from temporary stage {}",
                            checkpoint.temporary_stage.as_str()
                        )));
                    }
                    Ok(())
                })();
                if let Err(error) = restore_result {
                    return match error {
                        RuntimeActuatorError::Blocked(blocker) => {
                            self.restore_blocker = Some(blocker);
                            Ok(RuntimeDriverOutcome::Restoring)
                        }
                        RuntimeActuatorError::Unsafe(message) => self.fail_restore_safely(
                            actuator,
                            format!("runtime baseline restore failed: {message}"),
                        ),
                    };
                }
                let attested = match actuator.attest_restored_baseline(&baseline, &checkpoint) {
                    Ok(attested) => attested,
                    Err(RuntimeActuatorError::Blocked(blocker)) => {
                        self.restore_blocker = Some(blocker);
                        return Ok(RuntimeDriverOutcome::Restoring);
                    }
                    Err(RuntimeActuatorError::Unsafe(message)) => {
                        return self.fail_restore_safely(
                            actuator,
                            format!("restored runtime could not be attested: {message}"),
                        )
                    }
                };
                if attested != baseline {
                    return self.fail_restore_safely(
                        actuator,
                        "restored runtime does not exactly match its durable baseline".to_string(),
                    );
                }
                self.restore_blocker = None;
                let action = self
                    .tracker
                    .confirm_restored(&attested, actuator.current_boot_ms());
                if matches!(
                    action,
                    RuntimeOverrideAction::PublishAck(AutotuneRuntimeAck {
                        state: super::full_autotune::RuntimeAckState::Restored,
                        ..
                    })
                ) {
                    actuator.finish_restored();
                }
                self.execute(action, actuator)
            }
        }
    }

    fn fail_restore_safely(
        &mut self,
        actuator: &mut impl RuntimeOverrideActuator,
        diagnostic: String,
    ) -> Result<RuntimeDriverOutcome, String> {
        let recovery = actuator.recover_safe_configuration();
        let diagnostic = match recovery {
            Ok(()) => format!("{diagnostic}; safe configuration recovery was requested"),
            Err(recovery_error) => {
                format!("{diagnostic}; safe configuration recovery also failed: {recovery_error}")
            }
        };
        self.unsafe_recovery_reason = Some(diagnostic.clone());
        Err(diagnostic)
    }
}

pub(crate) fn runtime_snapshot_for_control(
    _permit: &AutotuneRuntimePermit,
    control: &AutotuneRuntimeControl,
) -> RuntimeSnapshot {
    RuntimeSnapshot {
        target_interface: control.target_interface.clone(),
        route_fingerprint: control.route_fingerprint.clone(),
        sqm_fingerprint: control.sqm_fingerprint.clone(),
        topology: control.topology,
        download_kbps: control.download_kbps,
        upload_kbps: control.upload_kbps,
        // Calibration runs on a private topology whose qdisc policy is an
        // explicit permit capability.  The managed baseline kind is restored
        // afterward but must not leak into this temporary topology.
        download_qdisc_kind: control
            .download_kbps
            .map(|_| super::autotune_runtime::RuntimeQdiscKind::Cake),
        upload_qdisc_kind: control
            .upload_kbps
            .map(|_| super::autotune_runtime::RuntimeQdiscKind::Cake),
    }
}

fn outcome_for_ack(ack: &AutotuneRuntimeAck) -> RuntimeDriverOutcome {
    use super::full_autotune::RuntimeAckState;
    match ack.state {
        RuntimeAckState::Applied => RuntimeDriverOutcome::Applied,
        RuntimeAckState::Restoring => RuntimeDriverOutcome::Restoring,
        RuntimeAckState::Restored => RuntimeDriverOutcome::Restored,
        RuntimeAckState::Rejected => RuntimeDriverOutcome::Rejected,
    }
}

fn outcome_for_phase(phase: RuntimeOverridePhase) -> RuntimeDriverOutcome {
    match phase {
        RuntimeOverridePhase::Idle => RuntimeDriverOutcome::Idle,
        RuntimeOverridePhase::Applying => RuntimeDriverOutcome::Applying,
        RuntimeOverridePhase::Applied => RuntimeDriverOutcome::Applied,
        RuntimeOverridePhase::Restoring => RuntimeDriverOutcome::Restoring,
        RuntimeOverridePhase::Restored => RuntimeDriverOutcome::Restored,
    }
}

#[cfg(test)]
mod tests {
    use super::super::autotune_runtime::{
        AbsentRuntimeBaseline, RuntimeBaseline, RuntimeRateBounds,
    };
    use super::super::full_autotune::{MeasurementTopology, RuntimeAckState};
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct FakeActuator {
        now: u64,
        route_identity: String,
        sqm_fingerprint: String,
        runtime: RuntimeSnapshot,
        worker_matches: bool,
        baseline_ready: bool,
        active_identity_matches: bool,
        baseline_attestation_error: bool,
        active_attestation_error: bool,
        fail_suspend: bool,
        fail_create_ifb: bool,
        fail_apply: bool,
        restore_blocker: Option<RuntimeRestoreBlocker>,
        observed_restore_blocker: Option<RuntimeRestoreBlocker>,
        blocker_observations: usize,
        attested_override: Option<RuntimeSnapshot>,
        safe_recoveries: usize,
        suspend_count: usize,
        create_ifb_count: usize,
        remove_temporary_count: usize,
        apply_count: usize,
        restore_count: usize,
    }

    impl RuntimeOverrideActuator for FakeActuator {
        fn current_boot_ms(&self) -> u64 {
            self.now
        }

        fn capture_baseline(
            &mut self,
            permit: &AutotuneRuntimePermit,
        ) -> Result<RuntimeRestoreBaseline, String> {
            Ok(match &permit.baseline {
                RuntimeBaseline::Managed(_) => RuntimeBaseline::Managed(self.runtime.clone()),
                RuntimeBaseline::Absent(absent) => RuntimeBaseline::Absent(absent.clone()),
            })
        }

        fn current_route_identity(&mut self) -> Result<String, String> {
            Ok(self.route_identity.clone())
        }

        fn baseline_ready_for_apply(
            &mut self,
            permit: &AutotuneRuntimePermit,
        ) -> Result<bool, String> {
            if self.baseline_attestation_error {
                return Err("injected baseline attestation failure".to_string());
            }
            Ok(self.baseline_ready && self.sqm_fingerprint == permit.sqm_fingerprint)
        }

        fn active_identity_matches(
            &mut self,
            permit: &AutotuneRuntimePermit,
        ) -> Result<bool, String> {
            if self.active_attestation_error {
                return Err("injected active identity attestation failure".to_string());
            }
            Ok(self.active_identity_matches && self.sqm_fingerprint == permit.sqm_fingerprint)
        }

        fn worker_identity_matches(&self, _worker: &ProcessIdentity) -> bool {
            self.worker_matches
        }

        fn runtime_matches(
            &mut self,
            expected: &RuntimeSnapshot,
            _checkpoint: &RuntimeOverrideCheckpoint,
        ) -> Result<bool, String> {
            Ok(&self.runtime == expected)
        }

        fn prepare_baseline(
            &mut self,
            _checkpoint: &RuntimeOverrideCheckpoint,
        ) -> Result<(), RuntimeActuatorError> {
            self.suspend_count += 1;
            if self.fail_suspend {
                return Err(RuntimeActuatorError::Unsafe(
                    "injected suspension failure".to_string(),
                ));
            }
            Ok(())
        }

        fn create_temporary_ifb(
            &mut self,
            checkpoint: &RuntimeOverrideCheckpoint,
        ) -> Result<u32, RuntimeActuatorError> {
            self.create_ifb_count += 1;
            let expected_stage = match &checkpoint.baseline {
                RuntimeBaseline::Managed(_) => TemporaryTopologyStage::ManagedSqmSuspended,
                RuntimeBaseline::Absent(_) => TemporaryTopologyStage::AbsenceAttested,
            };
            if checkpoint.temporary_stage != expected_stage {
                return Err(RuntimeActuatorError::Unsafe(
                    "temporary IFB creation skipped its baseline preparation stage".to_string(),
                ));
            }
            if self.fail_create_ifb {
                return Err(RuntimeActuatorError::Unsafe(
                    "injected IFB creation failure".to_string(),
                ));
            }
            Ok(42)
        }

        fn apply_override(
            &mut self,
            _control: &AutotuneRuntimeControl,
            expected: &RuntimeSnapshot,
            checkpoint: &RuntimeOverrideCheckpoint,
        ) -> Result<(), RuntimeActuatorError> {
            self.apply_count += 1;
            if self.fail_apply {
                return Err(RuntimeActuatorError::Unsafe(
                    "injected apply failure".to_string(),
                ));
            }
            self.runtime = expected.clone();
            if matches!(&checkpoint.baseline, RuntimeBaseline::Absent(_)) {
                self.baseline_ready = false;
            }
            Ok(())
        }

        fn remove_temporary_topology(
            &mut self,
            checkpoint: &RuntimeOverrideCheckpoint,
        ) -> Result<(), RuntimeActuatorError> {
            self.remove_temporary_count += 1;
            if matches!(&checkpoint.baseline, RuntimeBaseline::Absent(_)) {
                self.baseline_ready = true;
            }
            Ok(())
        }

        fn restore_baseline(
            &mut self,
            baseline: &RuntimeRestoreBaseline,
        ) -> Result<(), RuntimeActuatorError> {
            self.restore_count += 1;
            if let Some(blocker) = self.restore_blocker.clone() {
                return Err(RuntimeActuatorError::Blocked(blocker));
            }
            match baseline {
                RuntimeBaseline::Managed(baseline) => {
                    if self.sqm_fingerprint != baseline.sqm_fingerprint {
                        return Err(RuntimeActuatorError::Unsafe(
                            "injected stale SQM baseline restore".to_string(),
                        ));
                    }
                    self.runtime = baseline.clone();
                }
                RuntimeBaseline::Absent(absent) => {
                    if self.sqm_fingerprint != absent.sqm_fingerprint {
                        return Err(RuntimeActuatorError::Unsafe(
                            "injected stale absence baseline restore".to_string(),
                        ));
                    }
                }
            }
            Ok(())
        }

        fn observe_restore_blocker(
            &mut self,
            _blocker: &RuntimeRestoreBlocker,
            _baseline: &RuntimeRestoreBaseline,
        ) -> Result<RuntimeRestoreObservation, String> {
            self.blocker_observations += 1;
            Ok(self
                .observed_restore_blocker
                .clone()
                .map(RuntimeRestoreObservation::Blocked)
                .unwrap_or(RuntimeRestoreObservation::Ready))
        }

        fn attest_runtime(
            &mut self,
            expected: &RuntimeSnapshot,
            _checkpoint: &RuntimeOverrideCheckpoint,
        ) -> Result<RuntimeSnapshot, RuntimeActuatorError> {
            if self.sqm_fingerprint != expected.sqm_fingerprint {
                return Err(RuntimeActuatorError::Unsafe(
                    "injected SQM fingerprint drift".to_string(),
                ));
            }
            Ok(self
                .attested_override
                .clone()
                .unwrap_or_else(|| self.runtime.clone()))
        }

        fn attest_restored_baseline(
            &mut self,
            baseline: &RuntimeRestoreBaseline,
            _checkpoint: &RuntimeOverrideCheckpoint,
        ) -> Result<RuntimeRestoreBaseline, RuntimeActuatorError> {
            match baseline {
                RuntimeBaseline::Managed(expected) => self
                    .attest_runtime(expected, _checkpoint)
                    .map(RuntimeBaseline::Managed),
                RuntimeBaseline::Absent(absent) => {
                    if self.sqm_fingerprint != absent.sqm_fingerprint {
                        return Err(RuntimeActuatorError::Unsafe(
                            "injected absence fingerprint drift".to_string(),
                        ));
                    }
                    Ok(RuntimeBaseline::Absent(absent.clone()))
                }
            }
        }

        fn finish_restored(&mut self) {}

        fn recover_safe_configuration(&mut self) -> Result<(), String> {
            self.safe_recoveries += 1;
            Ok(())
        }
    }

    fn temp_root(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("runtime-driver-{name}-{nonce}"));
        fs::create_dir(&root).unwrap();
        root
    }

    fn permit() -> AutotuneRuntimePermit {
        AutotuneRuntimePermit {
            kind: super::super::autotune_runtime::RuntimePermitKind::Autotune,
            permit_id: "77".repeat(16),
            job_id: "11".repeat(16),
            worker_run_id: "22".repeat(16),
            boot_id: "33".repeat(16),
            coordinator_generation: "44".repeat(16),
            worker: ProcessIdentity {
                pid: 100,
                process_group: 100,
                starttime_ticks: 500,
            },
            instance_name: "wan_sqm".to_string(),
            target_interface: "pppoe-wan".to_string(),
            route_identity: "main||pppoe-wan|192.0.2.1||254".to_string(),
            route_fingerprint: "55".repeat(32),
            sqm_fingerprint: "66".repeat(32),
            deadline_boot_ms: 60_000,
            maximum_sequence: 32,
            profile: crate::autotune::AutotuneProfile::BestOverall,
            link_kind: crate::autotune::LinkKind::Ethernet,
            baseline: RuntimeBaseline::Managed(MeasurementTopology::ShapedBoth),
            initial_download_kbps: 100_000,
            initial_upload_kbps: 50_000,
            download_qdisc_kind: super::super::autotune_runtime::RuntimeQdiscKind::Cake,
            upload_qdisc_kind: super::super::autotune_runtime::RuntimeQdiscKind::Cake,
            allow_bypass_download: true,
            allow_bypass_upload: true,
            download_bounds: RuntimeRateBounds {
                minimum_kbps: 10_000,
                maximum_kbps: 1_000_000,
            },
            upload_bounds: RuntimeRateBounds {
                minimum_kbps: 5_000,
                maximum_kbps: 500_000,
            },
        }
    }

    fn absent_permit() -> AutotuneRuntimePermit {
        let mut permit = permit();
        let kernel_namespace_seed = permit.permit_id.clone();
        permit.baseline = RuntimeBaseline::Absent(AbsentRuntimeBaseline {
            planned_sqm_section: "wan_sqm".to_string(),
            target_interface: permit.target_interface.clone(),
            target_ifindex: 7,
            route_fingerprint: permit.route_fingerprint.clone(),
            config_fingerprint: "88".repeat(32),
            sqm_fingerprint: permit.sqm_fingerprint.clone(),
            kernel_topology_fingerprint: "99".repeat(32),
            kernel_namespace_seed,
        });
        permit
    }

    fn control(sequence: u32) -> AutotuneRuntimeControl {
        let permit = permit();
        AutotuneRuntimeControl {
            permit_id: permit.permit_id,
            job_id: permit.job_id,
            worker_run_id: permit.worker_run_id,
            boot_id: permit.boot_id,
            coordinator_generation: permit.coordinator_generation,
            worker: permit.worker,
            sequence,
            deadline_boot_ms: 30_000,
            target_interface: permit.target_interface,
            route_fingerprint: permit.route_fingerprint,
            sqm_fingerprint: permit.sqm_fingerprint,
            topology: MeasurementTopology::ShapedBoth,
            download_kbps: Some(100_000),
            upload_kbps: Some(50_000),
        }
    }

    fn baseline() -> RuntimeSnapshot {
        RuntimeSnapshot {
            target_interface: "pppoe-wan".to_string(),
            route_fingerprint: "55".repeat(32),
            sqm_fingerprint: "66".repeat(32),
            topology: MeasurementTopology::ShapedBoth,
            download_kbps: Some(90_000),
            upload_kbps: Some(45_000),
            download_qdisc_kind: Some(super::super::autotune_runtime::RuntimeQdiscKind::Cake),
            upload_qdisc_kind: Some(super::super::autotune_runtime::RuntimeQdiscKind::Cake),
        }
    }

    fn actuator() -> FakeActuator {
        FakeActuator {
            now: 1_000,
            route_identity: "main||pppoe-wan|192.0.2.1||254".to_string(),
            sqm_fingerprint: "66".repeat(32),
            runtime: baseline(),
            worker_matches: true,
            baseline_ready: true,
            active_identity_matches: true,
            baseline_attestation_error: false,
            active_attestation_error: false,
            fail_suspend: false,
            fail_create_ifb: false,
            fail_apply: false,
            restore_blocker: None,
            observed_restore_blocker: None,
            blocker_observations: 0,
            attested_override: None,
            safe_recoveries: 0,
            suspend_count: 0,
            create_ifb_count: 0,
            remove_temporary_count: 0,
            apply_count: 0,
            restore_count: 0,
        }
    }

    fn publish_request(
        root: &Path,
        permit: &AutotuneRuntimePermit,
        control: &AutotuneRuntimeControl,
    ) {
        let store = RuntimeOverrideStore::open(root).unwrap();
        store.publish_permit(permit).unwrap();
        store.publish_control(control).unwrap();
    }

    #[test]
    fn live_permit_waits_for_control_without_checkpoint_or_mutation() {
        let root = temp_root("permit-awaiting-control");
        let permit = permit();
        let store = RuntimeOverrideStore::open(&root).unwrap();
        store.publish_permit(&permit).unwrap();
        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        let mut actuator = actuator();

        for _ in 0..3 {
            assert_eq!(
                driver.poll(&mut actuator).unwrap(),
                RuntimeDriverOutcome::PermitAwaitingControl
            );
            assert_eq!(driver.phase(), RuntimeOverridePhase::Idle);
            assert!(store.read_checkpoint().unwrap().is_none());
            assert!(store.read_ack().unwrap().is_none());
            assert_eq!(actuator.suspend_count, 0);
            assert_eq!(actuator.create_ifb_count, 0);
            assert_eq!(actuator.apply_count, 0);
            assert_eq!(actuator.restore_count, 0);
        }

        store.publish_control(&control(1)).unwrap();
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Applied
        );
        assert_eq!(actuator.apply_count, 1);
        assert!(store.read_checkpoint().unwrap().is_some());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn permit_wait_is_bounded_and_control_without_permit_stays_rejected() {
        let root = temp_root("permit-wait-boundaries");
        let store = RuntimeOverrideStore::open(&root).unwrap();
        let permit = permit();
        store.publish_permit(&permit).unwrap();
        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        let mut actuator = actuator();

        actuator.now = permit.deadline_boot_ms;
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Rejected
        );
        assert!(store.read_checkpoint().unwrap().is_none());
        assert_eq!(actuator.apply_count, 0);

        store.clear().unwrap();
        store.publish_control(&control(1)).unwrap();
        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        actuator.now = 1_000;
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Rejected
        );
        assert!(store.read_checkpoint().unwrap().is_none());
        assert_eq!(actuator.apply_count, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dead_worker_permit_is_rejected_without_checkpoint_or_mutation() {
        let root = temp_root("permit-worker-dead");
        let store = RuntimeOverrideStore::open(&root).unwrap();
        store.publish_permit(&permit()).unwrap();
        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        let mut actuator = actuator();
        actuator.worker_matches = false;

        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Rejected
        );
        assert!(store.read_checkpoint().unwrap().is_none());
        assert_eq!(actuator.suspend_count, 0);
        assert_eq!(actuator.create_ifb_count, 0);
        assert_eq!(actuator.apply_count, 0);
        assert_eq!(actuator.restore_count, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn withdrawing_a_waiting_permit_returns_to_idle() {
        let root = temp_root("permit-withdrawn-before-control");
        let store = RuntimeOverrideStore::open(&root).unwrap();
        store.publish_permit(&permit()).unwrap();
        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        let mut actuator = actuator();

        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::PermitAwaitingControl
        );
        store.clear().unwrap();
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Idle
        );
        assert_eq!(actuator.apply_count, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn checkpoint_precedes_apply_and_exact_ack_is_published() {
        let root = temp_root("apply");
        publish_request(&root, &permit(), &control(1));
        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        let mut actuator = actuator();
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Applied
        );
        assert_eq!(actuator.apply_count, 1);
        assert_eq!(actuator.suspend_count, 1);
        assert_eq!(actuator.create_ifb_count, 1);
        let store = RuntimeOverrideStore::open(&root).unwrap();
        let checkpoint = store.read_checkpoint().unwrap().unwrap();
        assert_eq!(checkpoint.temporary_stage, TemporaryTopologyStage::Active);
        assert_eq!(checkpoint.temporary.ifb_ifindex, Some(42));
        assert_eq!(
            store.read_ack().unwrap().unwrap().state,
            RuntimeAckState::Applied
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn preflight_identity_drift_rejects_without_checkpoint_or_mutation() {
        let root = temp_root("preflight-drift");
        publish_request(&root, &permit(), &control(1));
        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        let mut actuator = actuator();
        actuator.sqm_fingerprint = "aa".repeat(32);
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Rejected
        );
        assert_eq!(actuator.apply_count, 0);
        assert_eq!(actuator.restore_count, 0);
        let store = RuntimeOverrideStore::open(&root).unwrap();
        assert!(store.read_checkpoint().unwrap().is_none());
        let ack = store.read_ack().unwrap().unwrap();
        assert_eq!(ack.state, RuntimeAckState::Rejected);
        assert_eq!(ack.diagnostic_code.as_deref(), Some("sqm-drift"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_absent_request_uses_the_distinct_attestation_stage_and_applies() {
        let root = temp_root("absent-new");
        let permit = absent_permit();
        publish_request(&root, &permit, &control(1));
        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        let mut actuator = actuator();
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Applied
        );
        assert_eq!(actuator.safe_recoveries, 0);
        assert_eq!(actuator.suspend_count, 1);
        assert_eq!(actuator.create_ifb_count, 1);
        assert_eq!(actuator.apply_count, 1);
        assert_eq!(actuator.remove_temporary_count, 0);
        assert_eq!(actuator.restore_count, 0);
        let store = RuntimeOverrideStore::open(&root).unwrap();
        let checkpoint = store.read_checkpoint().unwrap().unwrap();
        assert!(matches!(checkpoint.baseline, RuntimeBaseline::Absent(_)));
        assert_eq!(checkpoint.temporary_stage, TemporaryTopologyStage::Active);
        assert!(!actuator.baseline_ready);
        for _ in 0..3 {
            assert_eq!(
                driver.poll(&mut actuator).unwrap(),
                RuntimeDriverOutcome::Applied
            );
            assert_eq!(
                store.read_ack().unwrap().unwrap().state,
                RuntimeAckState::Applied
            );
            assert_eq!(actuator.remove_temporary_count, 0);
            assert_eq!(actuator.restore_count, 0);
        }
        assert_eq!(actuator.safe_recoveries, 0);
        actuator.worker_matches = false;
        actuator.now = 3_000;
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Restored
        );
        assert_eq!(actuator.remove_temporary_count, 1);
        assert_eq!(actuator.restore_count, 1);
        assert_eq!(
            store.read_checkpoint().unwrap().unwrap().temporary_stage,
            TemporaryTopologyStage::BaselineRestored
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn absent_preflight_requires_bare_state_but_active_identity_drift_restores() {
        let rejected_root = temp_root("absent-preflight-not-bare");
        let permit = absent_permit();
        publish_request(&rejected_root, &permit, &control(1));
        let mut rejected_driver =
            RuntimeOverrideDriver::open("wan_sqm".to_string(), &rejected_root).unwrap();
        let mut rejected_actuator = actuator();
        rejected_actuator.baseline_ready = false;
        assert_eq!(
            rejected_driver.poll(&mut rejected_actuator).unwrap(),
            RuntimeDriverOutcome::Rejected
        );
        let rejected_store = RuntimeOverrideStore::open(&rejected_root).unwrap();
        assert!(rejected_store.read_checkpoint().unwrap().is_none());
        assert_eq!(rejected_actuator.apply_count, 0);
        assert_eq!(
            rejected_store
                .read_ack()
                .unwrap()
                .unwrap()
                .diagnostic_code
                .as_deref(),
            Some("absence-drift")
        );
        fs::remove_dir_all(rejected_root).unwrap();

        let active_root = temp_root("absent-active-identity-drift");
        publish_request(&active_root, &permit, &control(1));
        let mut active_driver =
            RuntimeOverrideDriver::open("wan_sqm".to_string(), &active_root).unwrap();
        let mut active_actuator = actuator();
        assert_eq!(
            active_driver.poll(&mut active_actuator).unwrap(),
            RuntimeDriverOutcome::Applied
        );
        assert!(!active_actuator.baseline_ready);
        active_actuator.active_identity_matches = false;
        active_actuator.now = 2_000;
        assert_eq!(
            active_driver.poll(&mut active_actuator).unwrap(),
            RuntimeDriverOutcome::Restored
        );
        let active_store = RuntimeOverrideStore::open(&active_root).unwrap();
        assert_eq!(
            active_store.read_ack().unwrap().unwrap().state,
            RuntimeAckState::Restored
        );
        assert_eq!(active_actuator.remove_temporary_count, 1);
        assert_eq!(active_actuator.restore_count, 1);
        assert!(active_actuator.baseline_ready);
        fs::remove_dir_all(active_root).unwrap();
    }

    #[test]
    fn attestation_unavailable_is_not_misreported_as_proven_identity_drift() {
        let preflight_root = temp_root("absent-preflight-unavailable");
        let permit = absent_permit();
        publish_request(&preflight_root, &permit, &control(1));
        let mut preflight_driver =
            RuntimeOverrideDriver::open("wan_sqm".to_string(), &preflight_root).unwrap();
        let mut preflight_actuator = actuator();
        preflight_actuator.baseline_attestation_error = true;
        assert_eq!(
            preflight_driver.poll(&mut preflight_actuator).unwrap(),
            RuntimeDriverOutcome::Rejected
        );
        let preflight_store = RuntimeOverrideStore::open(&preflight_root).unwrap();
        assert!(preflight_store.read_checkpoint().unwrap().is_none());
        assert_eq!(preflight_actuator.apply_count, 0);
        assert_eq!(
            preflight_store
                .read_ack()
                .unwrap()
                .unwrap()
                .diagnostic_code
                .as_deref(),
            Some("absence-attestation-unavailable")
        );
        fs::remove_dir_all(preflight_root).unwrap();

        let active_root = temp_root("absent-active-attestation-unavailable");
        publish_request(&active_root, &permit, &control(1));
        let mut active_driver =
            RuntimeOverrideDriver::open("wan_sqm".to_string(), &active_root).unwrap();
        let mut active_actuator = actuator();
        assert_eq!(
            active_driver.poll(&mut active_actuator).unwrap(),
            RuntimeDriverOutcome::Applied
        );
        active_actuator.active_attestation_error = true;
        active_actuator.now = 2_000;
        assert_eq!(
            active_driver.poll(&mut active_actuator).unwrap(),
            RuntimeDriverOutcome::Restored
        );
        assert_eq!(
            active_driver.tracker.restore_reason(),
            Some(RuntimeRestoreReason::BaselineAttestationUnavailable)
        );
        assert_eq!(active_actuator.remove_temporary_count, 1);
        assert_eq!(active_actuator.restore_count, 1);
        fs::remove_dir_all(active_root).unwrap();
    }

    #[test]
    fn canonical_raw_download_snapshot_carries_the_shaped_upload_qdisc_kind() {
        let permit = absent_permit();
        let mut control = control(1);
        control.topology = MeasurementTopology::RawDownload;
        control.download_kbps = None;
        control.upload_kbps = Some(permit.initial_upload_kbps);
        permit.authorizes("wan_sqm", &control, 1_000).unwrap();

        let snapshot = runtime_snapshot_for_control(&permit, &control);
        assert_eq!(snapshot.topology, MeasurementTopology::RawDownload);
        assert_eq!(snapshot.download_kbps, None);
        assert_eq!(snapshot.download_qdisc_kind, None);
        assert_eq!(snapshot.upload_kbps, Some(permit.initial_upload_kbps));
        assert_eq!(
            snapshot.upload_qdisc_kind,
            Some(super::super::autotune_runtime::RuntimeQdiscKind::Cake)
        );
        snapshot.validate().unwrap();
    }

    #[test]
    fn absent_restart_from_planned_restores_without_managed_recovery() {
        let root = temp_root("absent-restart");
        let permit = absent_permit();
        let RuntimeBaseline::Absent(absent) = &permit.baseline else {
            unreachable!()
        };
        let store = RuntimeOverrideStore::open(&root).unwrap();
        store.publish_permit(&permit).unwrap();
        store.publish_control(&control(1)).unwrap();
        store
            .publish_checkpoint(
                &RuntimeOverrideCheckpoint::new(
                    &permit,
                    500,
                    RuntimeBaseline::Absent(absent.clone()),
                )
                .unwrap(),
            )
            .unwrap();

        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        let mut actuator = actuator();
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Restored
        );
        assert_eq!(actuator.safe_recoveries, 0);
        assert_eq!(actuator.suspend_count, 0);
        assert_eq!(actuator.create_ifb_count, 0);
        assert_eq!(actuator.apply_count, 0);
        assert_eq!(actuator.remove_temporary_count, 1);
        assert_eq!(actuator.restore_count, 1);
        assert_eq!(
            store.read_checkpoint().unwrap().unwrap().temporary_stage,
            TemporaryTopologyStage::BaselineRestored
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn worker_death_restores_and_waits_for_owner_release() {
        let root = temp_root("death");
        publish_request(&root, &permit(), &control(1));
        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        let mut actuator = actuator();
        driver.poll(&mut actuator).unwrap();
        actuator.worker_matches = false;
        actuator.now = 3_000;
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Restored
        );
        assert_eq!(actuator.restore_count, 1);
        assert_eq!(actuator.remove_temporary_count, 1);
        assert_eq!(driver.phase(), RuntimeOverridePhase::Restored);
        let store = RuntimeOverrideStore::open(&root).unwrap();
        assert_eq!(
            store.read_ack().unwrap().unwrap().state,
            RuntimeAckState::Restored
        );
        assert_eq!(
            store.read_checkpoint().unwrap().unwrap().temporary_stage,
            TemporaryTopologyStage::BaselineRestored
        );
        store.withdraw_request().unwrap();
        assert!(store.read_ack().unwrap().is_some());
        assert!(store.read_checkpoint().unwrap().is_some());
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Idle
        );
        assert!(store.read_ack().unwrap().is_none());
        assert!(store.read_checkpoint().unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn upload_only_baseline_can_create_both_shaped_private_runtime_and_restore_exactly() {
        let root = temp_root("directional-upload-only");
        let mut permit = permit();
        permit.baseline = RuntimeBaseline::Managed(MeasurementTopology::UploadOnlyShaped);
        let control = control(1);
        publish_request(&root, &permit, &control);

        let mut baseline = baseline();
        baseline.topology = MeasurementTopology::UploadOnlyShaped;
        baseline.download_kbps = None;
        baseline.download_qdisc_kind = None;
        let mut actuator = actuator();
        actuator.runtime = baseline.clone();
        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Applied
        );
        assert_eq!(actuator.runtime.topology, MeasurementTopology::ShapedBoth);
        assert_eq!(actuator.suspend_count, 1);
        assert_eq!(actuator.create_ifb_count, 1);

        actuator.worker_matches = false;
        actuator.now = 3_000;
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Restored
        );
        assert_eq!(actuator.runtime, baseline);
        assert_eq!(actuator.remove_temporary_count, 1);
        assert_eq!(actuator.restore_count, 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn download_only_baseline_can_create_both_shaped_private_runtime_and_restore_exactly() {
        let root = temp_root("directional-download-only");
        let mut permit = permit();
        permit.baseline = RuntimeBaseline::Managed(MeasurementTopology::DownloadOnlyShaped);
        let control = control(1);
        publish_request(&root, &permit, &control);

        let mut baseline = baseline();
        baseline.topology = MeasurementTopology::DownloadOnlyShaped;
        baseline.upload_kbps = None;
        baseline.upload_qdisc_kind = None;
        let mut actuator = actuator();
        actuator.runtime = baseline.clone();
        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Applied
        );
        assert_eq!(actuator.runtime.topology, MeasurementTopology::ShapedBoth);
        assert_eq!(actuator.suspend_count, 1);
        assert_eq!(actuator.create_ifb_count, 1);

        actuator.worker_matches = false;
        actuator.now = 3_000;
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Restored
        );
        assert_eq!(actuator.runtime, baseline);
        assert_eq!(actuator.remove_temporary_count, 1);
        assert_eq!(actuator.restore_count, 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cake_mq_baseline_uses_private_cake_and_restores_the_exact_qdisc_kinds() {
        use super::super::autotune_runtime::RuntimeQdiscKind;

        let root = temp_root("cake-mq-baseline");
        let mut permit = permit();
        permit.download_qdisc_kind = RuntimeQdiscKind::CakeMq;
        permit.upload_qdisc_kind = RuntimeQdiscKind::CakeMq;
        publish_request(&root, &permit, &control(1));

        let mut baseline = baseline();
        baseline.download_qdisc_kind = Some(RuntimeQdiscKind::CakeMq);
        baseline.upload_qdisc_kind = Some(RuntimeQdiscKind::CakeMq);
        let mut actuator = actuator();
        actuator.runtime = baseline.clone();
        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();

        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Applied
        );
        assert_eq!(
            actuator.runtime.download_qdisc_kind,
            Some(RuntimeQdiscKind::Cake)
        );
        assert_eq!(
            actuator.runtime.upload_qdisc_kind,
            Some(RuntimeQdiscKind::Cake)
        );

        actuator.worker_matches = false;
        actuator.now = 3_000;
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Restored
        );
        assert_eq!(actuator.runtime, baseline);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restart_restores_every_persisted_temporary_mutation_boundary() {
        for (name, stage) in [
            ("planned", TemporaryTopologyStage::Planned),
            ("suspended", TemporaryTopologyStage::ManagedSqmSuspended),
            ("link-owned", TemporaryTopologyStage::LinkOwned),
            ("active", TemporaryTopologyStage::Active),
        ] {
            let root = temp_root(name);
            let permit = permit();
            let control = control(1);
            let store = RuntimeOverrideStore::open(&root).unwrap();
            store.publish_permit(&permit).unwrap();
            store.publish_control(&control).unwrap();
            let planned = RuntimeOverrideCheckpoint::new(
                &permit,
                1_000,
                RuntimeBaseline::Managed(baseline()),
            )
            .unwrap();
            let checkpoint = match stage {
                TemporaryTopologyStage::Planned => planned,
                TemporaryTopologyStage::ManagedSqmSuspended => planned
                    .advance_temporary_stage(TemporaryTopologyStage::ManagedSqmSuspended, None)
                    .unwrap(),
                TemporaryTopologyStage::LinkOwned => planned
                    .advance_temporary_stage(TemporaryTopologyStage::ManagedSqmSuspended, None)
                    .unwrap()
                    .advance_temporary_stage(TemporaryTopologyStage::LinkOwned, Some(42))
                    .unwrap(),
                TemporaryTopologyStage::Active => planned
                    .advance_temporary_stage(TemporaryTopologyStage::ManagedSqmSuspended, None)
                    .unwrap()
                    .advance_temporary_stage(TemporaryTopologyStage::LinkOwned, Some(42))
                    .unwrap()
                    .advance_temporary_stage(TemporaryTopologyStage::Active, None)
                    .unwrap(),
                _ => unreachable!(),
            };
            store.publish_checkpoint(&checkpoint).unwrap();

            let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
            let mut actuator = actuator();
            if stage == TemporaryTopologyStage::Active {
                actuator.runtime = runtime_snapshot_for_control(&permit, &control);
            }
            assert_eq!(
                driver.poll(&mut actuator).unwrap(),
                RuntimeDriverOutcome::Restored,
                "stage {}",
                stage.as_str()
            );
            assert_eq!(actuator.remove_temporary_count, 1);
            assert_eq!(actuator.restore_count, 1);
            assert_eq!(actuator.runtime, baseline());
            assert_eq!(
                store.read_checkpoint().unwrap().unwrap().temporary_stage,
                TemporaryTopologyStage::BaselineRestored
            );
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn absent_restart_restores_every_persisted_temporary_mutation_boundary() {
        for (name, stage) in [
            ("absent-planned", TemporaryTopologyStage::Planned),
            ("absent-attested", TemporaryTopologyStage::AbsenceAttested),
            ("absent-link-owned", TemporaryTopologyStage::LinkOwned),
            ("absent-active", TemporaryTopologyStage::Active),
            (
                "absent-temporary-absent",
                TemporaryTopologyStage::TemporaryAbsent,
            ),
            (
                "absent-baseline-restored",
                TemporaryTopologyStage::BaselineRestored,
            ),
        ] {
            let root = temp_root(name);
            let permit = absent_permit();
            let control = control(1);
            let RuntimeBaseline::Absent(absent) = &permit.baseline else {
                unreachable!()
            };
            let store = RuntimeOverrideStore::open(&root).unwrap();
            store.publish_permit(&permit).unwrap();
            store.publish_control(&control).unwrap();
            let planned = RuntimeOverrideCheckpoint::new(
                &permit,
                1_000,
                RuntimeBaseline::Absent(absent.clone()),
            )
            .unwrap();
            let attested = planned
                .advance_temporary_stage(TemporaryTopologyStage::AbsenceAttested, None)
                .unwrap();
            let link_owned = attested
                .advance_temporary_stage(TemporaryTopologyStage::LinkOwned, Some(42))
                .unwrap();
            let active = link_owned
                .advance_temporary_stage(TemporaryTopologyStage::Active, None)
                .unwrap();
            let temporary_absent = active
                .advance_temporary_stage(TemporaryTopologyStage::TemporaryAbsent, None)
                .unwrap();
            let baseline_restored = temporary_absent
                .advance_temporary_stage(TemporaryTopologyStage::BaselineRestored, None)
                .unwrap();
            let checkpoint = match stage {
                TemporaryTopologyStage::Planned => planned,
                TemporaryTopologyStage::AbsenceAttested => attested,
                TemporaryTopologyStage::LinkOwned => link_owned,
                TemporaryTopologyStage::Active => active,
                TemporaryTopologyStage::TemporaryAbsent => temporary_absent,
                TemporaryTopologyStage::BaselineRestored => baseline_restored,
                TemporaryTopologyStage::ManagedSqmSuspended => unreachable!(),
            };
            store.publish_checkpoint(&checkpoint).unwrap();

            let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
            let mut actuator = actuator();
            if stage == TemporaryTopologyStage::Active {
                actuator.runtime = runtime_snapshot_for_control(&permit, &control);
            }
            assert_eq!(
                driver.poll(&mut actuator).unwrap(),
                RuntimeDriverOutcome::Restored,
                "stage {}",
                stage.as_str()
            );
            assert_eq!(
                actuator.remove_temporary_count,
                usize::from(!matches!(
                    stage,
                    TemporaryTopologyStage::TemporaryAbsent
                        | TemporaryTopologyStage::BaselineRestored
                )),
                "stage {}",
                stage.as_str()
            );
            assert_eq!(
                actuator.restore_count,
                usize::from(stage != TemporaryTopologyStage::BaselineRestored),
                "stage {}",
                stage.as_str()
            );
            assert_eq!(
                store.read_checkpoint().unwrap().unwrap().temporary_stage,
                TemporaryTopologyStage::BaselineRestored
            );
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn exact_route_recovery_control_reuses_checkpoint_and_reapplies_before_new_ack() {
        let root = temp_root("route-rearm");
        let permit = permit();
        let interrupted = control(1);
        publish_request(&root, &permit, &interrupted);
        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        let mut actuator = actuator();
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Applied
        );
        let store = RuntimeOverrideStore::open(&root).unwrap();
        let checkpoint = store.read_checkpoint().unwrap().unwrap();

        actuator.route_identity = "mwan3|wan|eth9|192.0.2.1||1001".to_string();
        actuator.now = 3_000;
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Restored
        );
        assert_eq!(actuator.runtime, baseline());
        assert_eq!(actuator.restore_count, 1);

        actuator.route_identity = permit.route_identity.clone();
        actuator.now = 4_000;
        let mut rearmed = interrupted.clone();
        rearmed.sequence += 1;
        store.publish_control(&rearmed).unwrap();
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Applied
        );
        assert_eq!(actuator.apply_count, 2);
        assert_eq!(store.read_checkpoint().unwrap(), Some(checkpoint));
        let ack = store.read_ack().unwrap().unwrap();
        ack.attests_applied(&rearmed, actuator.now).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn voluntary_restore_intent_cannot_be_rearmed() {
        let root = temp_root("voluntary-no-rearm");
        let permit = permit();
        let interrupted = control(1);
        publish_request(&root, &permit, &interrupted);
        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        let mut actuator = actuator();
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Applied
        );
        let store = RuntimeOverrideStore::open(&root).unwrap();
        store.request_restore(&interrupted).unwrap();
        actuator.now = 3_000;
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Restored
        );
        let mut forged_rearm = interrupted;
        forged_rearm.sequence += 1;
        store.publish_control(&forged_rearm).unwrap();
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Rejected
        );
        assert_eq!(actuator.apply_count, 1);
        assert_eq!(actuator.restore_count, 1);
        assert_eq!(driver.phase(), RuntimeOverridePhase::Restored);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn voluntary_restore_survives_the_two_stage_owner_release() {
        let root = temp_root("voluntary");
        let permit = permit();
        let control = control(1);
        publish_request(&root, &permit, &control);
        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        let mut actuator = actuator();
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Applied
        );
        let store = RuntimeOverrideStore::open(&root).unwrap();
        store.request_restore(&control).unwrap();
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Restored
        );
        let ack = store.read_ack().unwrap().unwrap();
        ack.attests_restored(&control, actuator.now).unwrap();
        assert_eq!(store.read_permit().unwrap(), Some(permit.clone()));
        assert_eq!(store.read_restore_intent().unwrap(), Some(control.clone()));
        assert!(store.read_checkpoint().unwrap().is_some());

        store
            .release_restored_request(&permit, &control, actuator.now)
            .unwrap();
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Idle
        );
        assert!(store.read_permit().unwrap().is_none());
        assert!(store.read_control().unwrap().is_none());
        assert!(store.read_restore_intent().unwrap().is_none());
        assert!(store.read_ack().unwrap().is_none());
        assert!(store.read_checkpoint().unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restart_after_voluntary_restore_intent_restores_instead_of_adopting_override() {
        let root = temp_root("voluntary-restart");
        let permit = permit();
        let control = control(1);
        publish_request(&root, &permit, &control);
        let mut first = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        let mut actuator = actuator();
        assert_eq!(
            first.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Applied
        );
        let store = RuntimeOverrideStore::open(&root).unwrap();
        store.request_restore(&control).unwrap();
        drop(first);

        let mut restarted = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        assert_eq!(
            restarted.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Restored
        );
        assert_eq!(actuator.runtime, baseline());
        store
            .release_restored_request(&permit, &control, actuator.now)
            .unwrap();
        assert_eq!(
            restarted.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Idle
        );
        assert!(store.read_restore_intent().unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restored_ack_is_republished_until_the_worker_releases_the_permit() {
        let root = temp_root("restore-ack-retry");
        let permit = permit();
        let control = control(1);
        publish_request(&root, &permit, &control);
        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        let mut actuator = actuator();
        driver.poll(&mut actuator).unwrap();
        let store = RuntimeOverrideStore::open(&root).unwrap();
        store.request_restore(&control).unwrap();
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Restored
        );
        fs::remove_file(root.join("autotune-runtime/ack.record")).unwrap();
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Restored
        );
        store
            .read_ack()
            .unwrap()
            .unwrap()
            .attests_restored(&control, actuator.now)
            .unwrap();
        store
            .release_restored_request(&permit, &control, actuator.now)
            .unwrap();
        driver.poll(&mut actuator).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restart_after_permit_release_finishes_the_attested_restore() {
        let root = temp_root("restore-release-restart");
        let permit = permit();
        let control = control(1);
        publish_request(&root, &permit, &control);
        let mut first = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        let mut actuator = actuator();
        first.poll(&mut actuator).unwrap();
        let store = RuntimeOverrideStore::open(&root).unwrap();
        store.request_restore(&control).unwrap();
        assert_eq!(
            first.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Restored
        );
        store
            .release_restored_request(&permit, &control, actuator.now)
            .unwrap();
        drop(first);

        let mut restarted = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        assert_eq!(
            restarted.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Idle
        );
        assert!(store.read_restore_intent().unwrap().is_none());
        assert!(store.read_ack().unwrap().is_none());
        assert!(store.read_checkpoint().unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restore_waits_for_observed_blocker_clear_without_time_authorized_retry() {
        let root = temp_root("transient-link-restore");
        publish_request(&root, &permit(), &control(1));
        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        let mut actuator = actuator();
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Applied
        );

        actuator.worker_matches = false;
        let unavailable = RuntimeRestoreBlocker::TargetUnavailable {
            target_interface: "pppoe-wan".to_string(),
            detail: "injected link unavailable".to_string(),
        };
        actuator.restore_blocker = Some(unavailable.clone());
        actuator.observed_restore_blocker = Some(unavailable);
        actuator.now = 3_000;
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Restoring
        );
        let store = RuntimeOverrideStore::open(&root).unwrap();
        assert_eq!(
            store.read_ack().unwrap().unwrap().state,
            RuntimeAckState::Restoring
        );
        assert!(store.read_checkpoint().unwrap().is_some());
        assert_eq!(actuator.safe_recoveries, 0);

        actuator.now = 300_000;
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Restoring
        );
        assert_eq!(actuator.restore_count, 1);
        assert_eq!(actuator.blocker_observations, 1);

        actuator.now = 3_001;
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Restoring
        );
        assert_eq!(actuator.restore_count, 1);
        assert_eq!(actuator.blocker_observations, 2);
        assert!(store.read_checkpoint().unwrap().is_some());

        actuator.observed_restore_blocker = Some(RuntimeRestoreBlocker::TopologySettling {
            target_interface: "pppoe-wan".to_string(),
            detail: "injected topology is still changing".to_string(),
        });
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Restoring
        );
        assert_eq!(actuator.restore_count, 1);
        assert_eq!(actuator.blocker_observations, 3);

        actuator.restore_blocker = None;
        actuator.observed_restore_blocker = None;
        actuator.now = 3_002;
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Restored
        );
        assert_eq!(actuator.restore_count, 2);
        assert_eq!(actuator.blocker_observations, 4);
        let restoring_ack = store.read_ack().unwrap().unwrap();
        assert_eq!(restoring_ack.state, RuntimeAckState::Restored);
        assert_eq!(actuator.runtime, baseline());
        assert!(store.read_checkpoint().unwrap().is_some());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restore_attestation_mismatch_is_unsafe_instead_of_an_unbounded_retry_loop() {
        let root = temp_root("restore-attestation-mismatch");
        publish_request(&root, &permit(), &control(1));
        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        let mut actuator = actuator();
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Applied
        );

        let mut mismatched = baseline();
        mismatched.download_kbps = Some(89_999);
        actuator.attested_override = Some(mismatched);
        actuator.worker_matches = false;
        actuator.now = 3_000;
        let error = driver.poll(&mut actuator).unwrap_err();
        assert!(error.contains("does not exactly match its durable baseline"));
        assert_eq!(actuator.restore_count, 1);
        assert_eq!(actuator.safe_recoveries, 1);
        assert_eq!(driver.unsafe_recovery_reason(), Some(error.as_str()));
        let store = RuntimeOverrideStore::open(&root).unwrap();
        assert!(store.read_checkpoint().unwrap().is_some());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_restore_source_has_no_retry_clock_or_backoff_state() {
        let source = include_str!("autotune_runtime_driver.rs");
        for forbidden in [
            concat!("restore_retry_", "after_boot_ms"),
            concat!("restore_retry_", "attempts"),
            concat!("restore_retry_", "delay_ms"),
            concat!("RESTORE_RETRY_", "INITIAL_MS"),
            concat!("RESTORE_RETRY_", "MAX_MS"),
            concat!("RuntimeActuatorError::", "Transient"),
        ] {
            assert!(
                !source.contains(forbidden),
                "runtime restore source still contains timer-owned state: {forbidden}"
            );
        }
    }

    #[test]
    fn replacing_live_owner_records_with_a_foreign_job_forces_restore() {
        let root = temp_root("foreign-replacement");
        publish_request(&root, &permit(), &control(1));
        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        let mut actuator = actuator();
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Applied
        );

        let mut foreign_permit = permit();
        foreign_permit.permit_id = "88".repeat(16);
        foreign_permit.job_id = "99".repeat(16);
        let mut foreign_control = control(1);
        foreign_control.permit_id = foreign_permit.permit_id.clone();
        foreign_control.job_id = foreign_permit.job_id.clone();
        publish_request(&root, &foreign_permit, &foreign_control);

        actuator.now = 3_000;
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Restored
        );
        assert_eq!(actuator.restore_count, 1);
        assert_eq!(actuator.runtime, baseline());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn live_sqm_fingerprint_drift_restores_then_latches_if_baseline_is_unattestable() {
        let root = temp_root("sqm-drift");
        publish_request(&root, &permit(), &control(1));
        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        let mut actuator = actuator();
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Applied
        );

        actuator.sqm_fingerprint = "aa".repeat(32);
        actuator.now = 3_000;
        assert!(driver.poll(&mut actuator).is_err());
        assert_eq!(actuator.restore_count, 1);
        assert_eq!(actuator.safe_recoveries, 1);
        assert_eq!(driver.phase(), RuntimeOverridePhase::Restoring);
        assert!(driver.unsafe_recovery_reason().is_some());
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::UnsafeRecoveryRequired
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_qdisc_kind_drift_is_restored_to_the_exact_baseline() {
        let root = temp_root("qdisc-kind-drift");
        publish_request(&root, &permit(), &control(1));
        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        let mut actuator = actuator();
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Applied
        );

        actuator.runtime.download_qdisc_kind =
            Some(super::super::autotune_runtime::RuntimeQdiscKind::CakeMq);
        actuator.now = 3_000;
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Restored
        );
        assert_eq!(actuator.restore_count, 1);
        assert_eq!(actuator.runtime, baseline());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restart_with_checkpoint_restores_without_reapplying_override() {
        let root = temp_root("restart");
        let permit = permit();
        let control = control(1);
        publish_request(&root, &permit, &control);
        let store = RuntimeOverrideStore::open(&root).unwrap();
        let checkpoint =
            RuntimeOverrideCheckpoint::new(&permit, 500, RuntimeBaseline::Managed(baseline()))
                .unwrap();
        store.publish_checkpoint(&checkpoint).unwrap();
        let mut actuator = actuator();
        actuator.runtime = runtime_snapshot_for_control(&permit, &control);
        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::Restored
        );
        assert_eq!(actuator.apply_count, 0);
        assert_eq!(actuator.restore_count, 1);
        assert_eq!(actuator.runtime, baseline());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn partial_apply_failure_immediately_restores_baseline() {
        let root = temp_root("apply-failure");
        publish_request(&root, &permit(), &control(1));
        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        let mut actuator = actuator();
        actuator.fail_apply = true;
        assert!(driver.poll(&mut actuator).is_err());
        assert_eq!(actuator.apply_count, 1);
        assert_eq!(actuator.restore_count, 1);
        assert_eq!(driver.phase(), RuntimeOverridePhase::Restored);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn suspension_and_ifb_creation_failures_restore_from_their_durable_stage() {
        for (name, fail_suspend, fail_create_ifb) in [
            ("suspend-failure", true, false),
            ("ifb-create-failure", false, true),
        ] {
            let root = temp_root(name);
            publish_request(&root, &permit(), &control(1));
            let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
            let mut actuator = actuator();
            actuator.fail_suspend = fail_suspend;
            actuator.fail_create_ifb = fail_create_ifb;

            assert!(driver.poll(&mut actuator).is_err());
            assert_eq!(actuator.suspend_count, 1);
            assert_eq!(actuator.create_ifb_count, usize::from(!fail_suspend));
            assert_eq!(actuator.remove_temporary_count, 1);
            assert_eq!(actuator.restore_count, 1);
            assert_eq!(actuator.runtime, baseline());
            assert_eq!(driver.phase(), RuntimeOverridePhase::Restored);
            let checkpoint = RuntimeOverrideStore::open(&root)
                .unwrap()
                .read_checkpoint()
                .unwrap()
                .unwrap();
            assert_eq!(
                checkpoint.temporary_stage,
                TemporaryTopologyStage::BaselineRestored
            );
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn orphaned_checkpoint_without_owner_requests_safe_configuration_recovery() {
        let root = temp_root("orphan");
        let owner_permit = permit();
        let store = RuntimeOverrideStore::open(&root).unwrap();
        store
            .publish_checkpoint(
                &RuntimeOverrideCheckpoint::new(
                    &owner_permit,
                    500,
                    RuntimeBaseline::Managed(baseline()),
                )
                .unwrap(),
            )
            .unwrap();
        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        let mut actuator = actuator();
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::UnsafeRecoveryRequired
        );
        assert_eq!(actuator.safe_recoveries, 1);
        assert_eq!(actuator.apply_count, 0);
        assert!(driver.unsafe_recovery_reason().is_some());

        // Safe recovery does not make an orphaned ownership record trusted.
        // The instance remains fail-closed and must not admit a replacement
        // request until an explicit recovery path resolves the checkpoint.
        publish_request(&root, &permit(), &control(1));
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::UnsafeRecoveryRequired
        );
        assert_eq!(actuator.safe_recoveries, 1);
        assert_eq!(actuator.apply_count, 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_checkpoint_recovers_once_then_stays_fail_closed() {
        let root = temp_root("corrupt-checkpoint");
        let store = RuntimeOverrideStore::open(&root).unwrap();
        let checkpoint = root.join("autotune-runtime/checkpoint.record");
        fs::write(&checkpoint, b"not-a-runtime-checkpoint\n").unwrap();
        let mut permissions = fs::metadata(&checkpoint).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o600);
        fs::set_permissions(&checkpoint, permissions).unwrap();
        drop(store);

        let mut driver = RuntimeOverrideDriver::open("wan_sqm".to_string(), &root).unwrap();
        let mut actuator = actuator();
        assert!(driver.poll(&mut actuator).is_err());
        assert_eq!(actuator.safe_recoveries, 1);
        assert!(driver.unsafe_recovery_reason().is_some());

        publish_request(&root, &permit(), &control(1));
        assert_eq!(
            driver.poll(&mut actuator).unwrap(),
            RuntimeDriverOutcome::UnsafeRecoveryRequired
        );
        assert_eq!(actuator.safe_recoveries, 1);
        assert_eq!(actuator.apply_count, 0);
        fs::remove_dir_all(root).unwrap();
    }
}
