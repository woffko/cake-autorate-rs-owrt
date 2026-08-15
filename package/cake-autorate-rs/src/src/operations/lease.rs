use super::protocol::{OperationKind, OperationRequest, OperationTargetState};
use std::collections::{BTreeMap, BTreeSet};

const MAX_LEASE_KEYS_PER_JOB: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LeaseKey {
    HeavyTraffic,
    Instance(String),
    TargetInterface(String),
    SqmFingerprint(String),
    /// Stable section-name identity for an immutable absent-bootstrap target.
    /// Existing managed requests deliberately retain their historical lease
    /// set; only the v5 target-state authority may introduce this key.
    ManagedSqmSection(String),
}

impl LeaseKey {
    pub(crate) fn managed_sqm_section(section: impl Into<String>) -> Result<Self, String> {
        let section = section.into();
        super::sqm_identity::validate_uci_section(&section)?;
        Ok(Self::ManagedSqmSection(section))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaseRequest {
    pub job_id: String,
    pub keys: BTreeSet<LeaseKey>,
}

impl LeaseRequest {
    pub fn local_from_operation(request: &OperationRequest) -> Result<Self, String> {
        request.validate()?;
        let mut keys = BTreeSet::new();
        keys.insert(LeaseKey::Instance(request.identity.instance.clone()));
        keys.insert(LeaseKey::TargetInterface(
            request.identity.target_interface.clone(),
        ));
        keys.insert(LeaseKey::SqmFingerprint(
            request.identity.sqm_fingerprint.clone(),
        ));
        if request.target_state == OperationTargetState::AbsentBootstrap {
            let section = request.managed_sqm_section.as_deref().ok_or_else(|| {
                "absent bootstrap lease requires a planned managed SQM section".to_string()
            })?;
            keys.insert(LeaseKey::managed_sqm_section(section)?);
        }
        if keys.len() > MAX_LEASE_KEYS_PER_JOB {
            return Err("operation requests too many runtime leases".to_string());
        }
        Ok(Self {
            job_id: request.identity.job_id.clone(),
            keys,
        })
    }

    pub fn from_journalled_operation(
        request: &OperationRequest,
        heavy_lease_acquired: bool,
    ) -> Result<Self, String> {
        let mut lease = Self::local_from_operation(request)?;
        if heavy_lease_acquired {
            if !requires_heavy_traffic(request.identity.operation) {
                return Err("journal claims a heavy lease for a light operation".to_string());
            }
            lease.keys.insert(LeaseKey::HeavyTraffic);
        }
        Ok(lease)
    }

    pub fn heavy_for(job_id: &str) -> Self {
        Self {
            job_id: job_id.to_string(),
            keys: BTreeSet::from([LeaseKey::HeavyTraffic]),
        }
    }
}

pub fn requires_heavy_traffic(operation: OperationKind) -> bool {
    matches!(
        operation,
        OperationKind::FullAutotune
            | OperationKind::AutomaticRating
            | OperationKind::GuidedRating
            | OperationKind::Speedtest
    )
}

#[derive(Clone, Default)]
pub struct LeaseTable {
    owners: BTreeMap<LeaseKey, String>,
    jobs: BTreeMap<String, BTreeSet<LeaseKey>>,
}

impl LeaseTable {
    pub fn acquire(&mut self, request: LeaseRequest) -> Result<(), String> {
        if self.jobs.contains_key(&request.job_id) {
            return Err(format!("job {} already owns leases", request.job_id));
        }
        if let Some((key, owner)) = request
            .keys
            .iter()
            .find_map(|key| self.owners.get(key).map(|owner| (key, owner)))
        {
            return Err(format!(
                "runtime lease {key:?} is already owned by job {owner}"
            ));
        }

        // Conflict validation above covers the entire set, so acquisition is
        // all-or-nothing and cannot leave a partially leased job.
        for key in &request.keys {
            self.owners.insert(key.clone(), request.job_id.clone());
        }
        self.jobs.insert(request.job_id, request.keys);
        Ok(())
    }

    pub fn acquire_additional(&mut self, request: LeaseRequest) -> Result<(), String> {
        let owned = self
            .jobs
            .get(&request.job_id)
            .ok_or_else(|| format!("job {} owns no base leases", request.job_id))?;
        if request.keys.is_empty() || request.keys.iter().any(|key| owned.contains(key)) {
            return Err("additional lease request is empty or duplicates an owned key".to_string());
        }
        if owned.len().saturating_add(request.keys.len()) > MAX_LEASE_KEYS_PER_JOB {
            return Err("operation requests too many runtime leases".to_string());
        }
        if let Some((key, owner)) = request
            .keys
            .iter()
            .find_map(|key| self.owners.get(key).map(|owner| (key, owner)))
        {
            return Err(format!(
                "runtime lease {key:?} is already owned by job {owner}"
            ));
        }

        let owned = self
            .jobs
            .get_mut(&request.job_id)
            .expect("base lease ownership was checked above");
        for key in request.keys {
            self.owners.insert(key.clone(), request.job_id.clone());
            owned.insert(key);
        }
        Ok(())
    }

    pub fn release(&mut self, job_id: &str) -> Result<(), String> {
        let keys = self
            .jobs
            .remove(job_id)
            .ok_or_else(|| format!("job {job_id} owns no runtime leases"))?;
        for key in keys {
            match self.owners.remove(&key) {
                Some(owner) if owner == job_id => {}
                Some(owner) => {
                    self.owners.insert(key, owner);
                    return Err("runtime lease owner table is inconsistent".to_string());
                }
                None => return Err("runtime lease disappeared before release".to_string()),
            }
        }
        Ok(())
    }

    pub fn release_heavy(&mut self, job_id: &str) -> Result<(), String> {
        let keys = self
            .jobs
            .get(job_id)
            .ok_or_else(|| format!("job {job_id} owns no runtime leases"))?;
        if !keys.contains(&LeaseKey::HeavyTraffic)
            || self.owners.get(&LeaseKey::HeavyTraffic).map(String::as_str) != Some(job_id)
        {
            return Err(format!("job {job_id} does not own the heavy-traffic lease"));
        }
        if keys.len() == 1 {
            return Err(
                "heavy-traffic lease cannot be detached without local ownership".to_string(),
            );
        }
        self.owners.remove(&LeaseKey::HeavyTraffic);
        self.jobs
            .get_mut(job_id)
            .expect("lease ownership was checked above")
            .remove(&LeaseKey::HeavyTraffic);
        Ok(())
    }

    pub fn owner(&self, key: &LeaseKey) -> Option<&str> {
        self.owners.get(key).map(String::as_str)
    }

    pub fn job_count(&self) -> usize {
        self.jobs.len()
    }

    pub fn contains_job(&self, job_id: &str) -> bool {
        self.jobs.contains_key(job_id)
    }
}

#[cfg(test)]
mod tests {
    use super::super::protocol::{
        CalibrationStrategy, OperationIdentity, OperationOrigin, OperationRouteIdentity,
        OperationRouteMode,
    };
    use super::*;
    use crate::autotune::{
        AccessEvidenceSource, AccessMedium, AutotuneProfile, CapacityLearningPolicy,
    };

    fn fingerprint(value: char) -> String {
        std::iter::repeat_n(value, 64).collect()
    }

    fn operation_request(
        job_hex: char,
        instance: &str,
        interface: &str,
        sqm_fingerprint: char,
        section: &str,
        target_state: OperationTargetState,
    ) -> OperationRequest {
        OperationRequest {
            identity: OperationIdentity {
                job_id: std::iter::repeat_n(job_hex, 32).collect(),
                job_token: fingerprint(job_hex),
                instance: instance.to_string(),
                operation: OperationKind::FullAutotune,
                target_interface: interface.to_string(),
                route_fingerprint: fingerprint('b'),
                config_fingerprint: fingerprint('c'),
                sqm_fingerprint: fingerprint(sqm_fingerprint),
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
                l3_device: interface.to_string(),
                source_ip: None,
                fwmark: None,
                routing_table: None,
            },
            target_state,
            capture_policy: (target_state == OperationTargetState::AbsentBootstrap).then_some(
                crate::operations::autotune_capture_policy::AutotuneCapturePolicyId::StandardV1,
            ),
            managed_sqm_section: Some(section.to_string()),
            profile: Some(AutotuneProfile::VariableLink),
            strategy: Some(CalibrationStrategy::FullRaw),
            access_medium: Some(AccessMedium::Cellular),
            access_source: Some(AccessEvidenceSource::UserSelected),
            access_confidence_percent: 100,
            capacity_learning_policy: Some(CapacityLearningPolicy::ScheduledActive),
            service_dl_cap_kbps: Some(1_000_000),
            service_ul_cap_kbps: Some(500_000),
            allow_sqm_disable: true,
            allow_active_traffic: false,
            scheduled_auto_apply_requested: false,
            traffic_budget_bytes: 1_000_000,
        }
    }

    fn request(job: &str, instance: &str, interface: &str, sqm: &str) -> LeaseRequest {
        LeaseRequest {
            job_id: job.to_string(),
            keys: BTreeSet::from([
                LeaseKey::HeavyTraffic,
                LeaseKey::Instance(instance.to_string()),
                LeaseKey::TargetInterface(interface.to_string()),
                LeaseKey::SqmFingerprint(sqm.to_string()),
            ]),
        }
    }

    #[test]
    fn acquisition_is_atomic_when_any_key_conflicts() {
        let mut table = LeaseTable::default();
        table
            .acquire(request("job-a", "wan", "pppoe-wan", "sqm-a"))
            .unwrap();
        assert!(table
            .acquire(request("job-b", "wanb", "eth0", "sqm-b"))
            .is_err());
        assert_eq!(table.job_count(), 1);
        assert_eq!(table.owner(&LeaseKey::Instance("wanb".to_string())), None);
    }

    #[test]
    fn per_interface_and_sqm_conflicts_are_independent_of_instance_name() {
        let mut table = LeaseTable::default();
        let mut first = request("job-a", "wan", "pppoe-wan", "sqm-a");
        first.keys.remove(&LeaseKey::HeavyTraffic);
        table.acquire(first).unwrap();

        let mut same_interface = request("job-b", "other", "pppoe-wan", "sqm-b");
        same_interface.keys.remove(&LeaseKey::HeavyTraffic);
        assert!(table.acquire(same_interface).is_err());

        let mut same_sqm = request("job-c", "third", "eth2", "sqm-a");
        same_sqm.keys.remove(&LeaseKey::HeavyTraffic);
        assert!(table.acquire(same_sqm).is_err());
    }

    #[test]
    fn managed_sqm_section_key_uses_the_canonical_uci_name_validation() {
        assert_eq!(
            LeaseKey::managed_sqm_section("cake_wan").unwrap(),
            LeaseKey::ManagedSqmSection("cake_wan".to_string())
        );
        for invalid in ["", "cake-wan", "cake.wan", "cake/wan", "cake wan"] {
            assert!(LeaseKey::managed_sqm_section(invalid).is_err());
        }
        assert!(LeaseKey::managed_sqm_section("a".repeat(65)).is_err());
    }

    #[test]
    fn managed_sqm_section_conflicts_independently_of_mutable_fingerprints() {
        let section = LeaseKey::managed_sqm_section("cake_shared").unwrap();
        let mut first = request("job-a", "wan", "pppoe-wan", "fingerprint-a");
        first.keys.remove(&LeaseKey::HeavyTraffic);
        first.keys.insert(section.clone());

        let mut second = request("job-b", "wanb", "eth0", "fingerprint-b");
        second.keys.remove(&LeaseKey::HeavyTraffic);
        second.keys.insert(section.clone());

        let mut table = LeaseTable::default();
        table.acquire(first).unwrap();
        assert!(table.acquire(second).is_err());
        assert_eq!(table.owner(&section), Some("job-a"));
        assert_eq!(table.job_count(), 1);
        assert_eq!(table.owner(&LeaseKey::Instance("wanb".to_string())), None);
    }

    #[test]
    fn absent_bootstrap_local_lease_includes_exact_planned_section_key() {
        let operation = operation_request(
            '1',
            "bootstrap_wan",
            "eth0",
            'd',
            "cake_bootstrap_wan",
            OperationTargetState::AbsentBootstrap,
        );
        let lease = LeaseRequest::local_from_operation(&operation).unwrap();
        assert_eq!(
            lease.keys.into_iter().collect::<Vec<_>>(),
            vec![
                LeaseKey::Instance("bootstrap_wan".to_string()),
                LeaseKey::TargetInterface("eth0".to_string()),
                LeaseKey::SqmFingerprint(fingerprint('d')),
                LeaseKey::ManagedSqmSection("cake_bootstrap_wan".to_string()),
            ]
        );
    }

    #[test]
    fn absent_bootstrap_restart_reconstructs_same_keys_plus_heavy_at_bound() {
        let operation = operation_request(
            '2',
            "bootstrap_wanb",
            "eth1",
            'e',
            "cake_bootstrap_wanb",
            OperationTargetState::AbsentBootstrap,
        );
        let local = LeaseRequest::local_from_operation(&operation).unwrap();
        let restarted = LeaseRequest::from_journalled_operation(&operation, true).unwrap();
        let mut expected = local.keys;
        expected.insert(LeaseKey::HeavyTraffic);
        assert_eq!(restarted.keys, expected);
        assert_eq!(restarted.keys.len(), MAX_LEASE_KEYS_PER_JOB);
    }

    #[test]
    fn absent_bootstraps_with_one_planned_section_conflict_across_fingerprints() {
        let first = LeaseRequest::local_from_operation(&operation_request(
            '3',
            "bootstrap_wan",
            "eth0",
            'd',
            "cake_shared",
            OperationTargetState::AbsentBootstrap,
        ))
        .unwrap();
        let second = LeaseRequest::local_from_operation(&operation_request(
            '4',
            "bootstrap_wanb",
            "eth1",
            'e',
            "cake_shared",
            OperationTargetState::AbsentBootstrap,
        ))
        .unwrap();
        let section = LeaseKey::ManagedSqmSection("cake_shared".to_string());

        let mut table = LeaseTable::default();
        table.acquire(first).unwrap();
        assert!(table.acquire(second).is_err());
        assert_eq!(
            table.owner(&section),
            Some("33333333333333333333333333333333")
        );
        assert_eq!(
            table.owner(&LeaseKey::Instance("bootstrap_wanb".to_string())),
            None
        );
    }

    #[test]
    fn existing_managed_request_retains_frozen_lease_key_set_and_order() {
        let operation = operation_request(
            '5',
            "wan_sqm",
            "pppoe-wan",
            'f',
            "cake_wan",
            OperationTargetState::ExistingManaged,
        );
        let lease = LeaseRequest::local_from_operation(&operation).unwrap();
        assert_eq!(
            lease.keys.into_iter().collect::<Vec<_>>(),
            vec![
                LeaseKey::Instance("wan_sqm".to_string()),
                LeaseKey::TargetInterface("pppoe-wan".to_string()),
                LeaseKey::SqmFingerprint(fingerprint('f')),
            ]
        );
    }

    #[test]
    fn future_section_key_is_appended_after_the_existing_key_order() {
        let keys = BTreeSet::from([
            LeaseKey::ManagedSqmSection("cake_wan".to_string()),
            LeaseKey::SqmFingerprint("fingerprint".to_string()),
            LeaseKey::TargetInterface("eth0".to_string()),
            LeaseKey::Instance("wan".to_string()),
            LeaseKey::HeavyTraffic,
        ]);
        assert_eq!(
            keys.into_iter().collect::<Vec<_>>(),
            vec![
                LeaseKey::HeavyTraffic,
                LeaseKey::Instance("wan".to_string()),
                LeaseKey::TargetInterface("eth0".to_string()),
                LeaseKey::SqmFingerprint("fingerprint".to_string()),
                LeaseKey::ManagedSqmSection("cake_wan".to_string()),
            ]
        );
    }

    #[test]
    fn release_allows_the_next_job_and_rejects_double_release() {
        let mut table = LeaseTable::default();
        table
            .acquire(request("job-a", "wan", "pppoe-wan", "sqm-a"))
            .unwrap();
        table.release("job-a").unwrap();
        table
            .acquire(request("job-b", "wanb", "eth0", "sqm-b"))
            .unwrap();
        assert!(table.release("job-a").is_err());
    }

    #[test]
    fn heavy_lease_can_be_staged_after_local_ownership() {
        let mut table = LeaseTable::default();
        let mut first = request("job-a", "wan", "pppoe-wan", "sqm-a");
        first.keys.remove(&LeaseKey::HeavyTraffic);
        table.acquire(first).unwrap();
        table
            .acquire_additional(LeaseRequest::heavy_for("job-a"))
            .unwrap();
        assert_eq!(table.owner(&LeaseKey::HeavyTraffic), Some("job-a"));

        let mut second = request("job-b", "wanb", "eth0", "sqm-b");
        second.keys.remove(&LeaseKey::HeavyTraffic);
        table.acquire(second).unwrap();
        assert!(table
            .acquire_additional(LeaseRequest::heavy_for("job-b"))
            .is_err());
        assert_eq!(
            table.owner(&LeaseKey::Instance("wanb".to_string())),
            Some("job-b")
        );
    }

    #[test]
    fn heavy_lease_can_be_released_while_local_recovery_locks_remain() {
        let mut table = LeaseTable::default();
        table
            .acquire(request("job-a", "wan", "pppoe-wan", "sqm-a"))
            .unwrap();
        table.release_heavy("job-a").unwrap();
        assert_eq!(table.owner(&LeaseKey::HeavyTraffic), None);
        assert_eq!(
            table.owner(&LeaseKey::Instance("wan".to_string())),
            Some("job-a")
        );

        let mut second = request("job-b", "wanb", "eth0", "sqm-b");
        second.keys.remove(&LeaseKey::HeavyTraffic);
        table.acquire(second).unwrap();
        table
            .acquire_additional(LeaseRequest::heavy_for("job-b"))
            .unwrap();
        assert_eq!(table.owner(&LeaseKey::HeavyTraffic), Some("job-b"));
        assert_eq!(table.job_count(), 2);
    }
}
