//! State-driven admission for managed-SQM repair.
//!
//! Poll cadence may observe this gate, but elapsed time never changes its
//! decision.  A failed mutating repair blocks the exact observed topology
//! generation until a real link/topology transition is observed.

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SqmObservedTopologyState {
    Healthy,
    Settling(String),
    Unsafe(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SqmTopologyGeneration {
    pub target_present: bool,
    pub target_ifindex: Option<u64>,
    pub upload_ifindex: Option<u64>,
    pub download_ifindex: Option<u64>,
    pub download_counter_present: bool,
    pub upload_counter_present: bool,
    pub download_qdisc_signature: u64,
    pub upload_qdisc_signature: u64,
    pub ingress_signature: Option<u64>,
    pub topology: SqmObservedTopologyState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SqmRecoveryAdmission {
    Recover,
    WaitForStateChange,
}

#[derive(Clone, Debug, Default)]
pub struct SqmRecoveryGate {
    failed_generation: Option<SqmTopologyGeneration>,
}

impl SqmRecoveryGate {
    pub fn admission(&self, generation: &SqmTopologyGeneration) -> SqmRecoveryAdmission {
        if self.failed_generation.as_ref() == Some(generation) {
            SqmRecoveryAdmission::WaitForStateChange
        } else {
            SqmRecoveryAdmission::Recover
        }
    }

    pub fn record_failed(&mut self, generation: SqmTopologyGeneration) {
        self.failed_generation = Some(generation);
    }

    pub fn observe_healthy(&mut self) {
        self.failed_generation = None;
    }

    pub fn observe_target_missing(&mut self) {
        // A real present -> missing transition is an explicit generation
        // boundary.  Reappearance may therefore admit one new repair even on
        // systems whose synthetic test sysfs has no ifindex file.
        self.failed_generation = None;
    }

    #[cfg(test)]
    fn blocked_generation(&self) -> Option<&SqmTopologyGeneration> {
        self.failed_generation.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generation(target_ifindex: Option<u64>, signature: u64) -> SqmTopologyGeneration {
        SqmTopologyGeneration {
            target_present: true,
            target_ifindex,
            upload_ifindex: target_ifindex,
            download_ifindex: Some(7),
            download_counter_present: true,
            upload_counter_present: true,
            download_qdisc_signature: signature,
            upload_qdisc_signature: 22,
            ingress_signature: Some(33),
            topology: SqmObservedTopologyState::Settling("download-cake-missing".to_string()),
        }
    }

    #[test]
    fn elapsed_polls_never_reauthorize_an_unchanged_failed_generation() {
        let failed = generation(Some(5), 11);
        let mut gate = SqmRecoveryGate::default();
        assert_eq!(gate.admission(&failed), SqmRecoveryAdmission::Recover);
        gate.record_failed(failed.clone());
        for _ in 0..10_000 {
            assert_eq!(
                gate.admission(&failed),
                SqmRecoveryAdmission::WaitForStateChange
            );
        }
        assert_eq!(gate.blocked_generation(), Some(&failed));
    }

    #[test]
    fn topology_or_interface_generation_change_admits_one_new_attempt() {
        let failed = generation(Some(5), 11);
        let mut gate = SqmRecoveryGate::default();
        gate.record_failed(failed.clone());

        let changed_topology = generation(Some(5), 12);
        assert_eq!(
            gate.admission(&changed_topology),
            SqmRecoveryAdmission::Recover
        );
        gate.record_failed(changed_topology.clone());
        assert_eq!(
            gate.admission(&changed_topology),
            SqmRecoveryAdmission::WaitForStateChange
        );

        let recreated = generation(Some(9), 12);
        assert_eq!(gate.admission(&recreated), SqmRecoveryAdmission::Recover);
    }

    #[test]
    fn explicit_link_loss_and_healthy_attestation_clear_the_failure_fence() {
        let failed = generation(None, 11);
        let mut gate = SqmRecoveryGate::default();
        gate.record_failed(failed.clone());
        gate.observe_target_missing();
        assert_eq!(gate.admission(&failed), SqmRecoveryAdmission::Recover);

        gate.record_failed(failed.clone());
        gate.observe_healthy();
        assert_eq!(gate.admission(&failed), SqmRecoveryAdmission::Recover);
    }
}
