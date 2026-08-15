//! Transactional runtime bridge for the native scheduler.
//!
//! This module still performs no wall-clock polling and launches no process.
//! It turns an explicitly supplied, live-attested observation into one atomic
//! per-instance reservation, and later settles cursor plus traffic accounting
//! in the same durable record.  Calendar time can select a due slot, but only
//! caller-supplied state evidence can authorize admission.

use super::protocol::{OperationOrigin, OperationRequest};
use super::rating::RatingRuntimeSnapshot;
use super::scheduler::{
    evaluate_schedule, BudgetLedger, FailedAttemptFence, ScheduleCursor, SchedulerDecision,
    SchedulerInstanceState, SchedulerObservation,
};
use super::scheduler_config::ScheduledInstanceConfig;
use super::scheduler_store::SchedulerStore;
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

const MAX_PROC_ENTRIES: usize = 65_536;
const MAX_CMDLINE_BYTES: usize = 8 * 1024;
pub const SCHEDULER_RUNTIME_FRESHNESS_MS: u64 = 5_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalCalendar {
    pub now_unix_s: u64,
    pub day: String,
    pub month: String,
    pub hour: u8,
    pub window_open: bool,
    pub next_window_open_unix_s: Option<u64>,
}

pub fn local_calendar(
    now_unix_s: u64,
    window_start_hour: u8,
    window_end_hour: u8,
) -> Result<LocalCalendar, String> {
    if now_unix_s == 0 || window_start_hour > 23 || window_end_hour > 23 {
        return Err("scheduler local calendar input is invalid".to_string());
    }
    let raw = libc::time_t::try_from(now_unix_s)
        .map_err(|_| "scheduler wall clock is outside time_t".to_string())?;
    let mut local = unsafe { std::mem::zeroed::<libc::tm>() };
    if unsafe { libc::localtime_r(&raw, &mut local) }.is_null() {
        return Err("scheduler could not resolve the local calendar".to_string());
    }
    if !(0..=23).contains(&local.tm_hour)
        || !(0..=11).contains(&local.tm_mon)
        || !(1..=31).contains(&local.tm_mday)
        || local.tm_year < 70
    {
        return Err("scheduler local calendar fields are invalid".to_string());
    }
    let hour = local.tm_hour as u8;
    let window_open = window_contains_hour(hour, window_start_hour, window_end_hour);
    let next_window_open_unix_s = if window_open || window_start_hour == window_end_hour {
        None
    } else {
        let mut candidate = local;
        candidate.tm_hour = window_start_hour as i32;
        candidate.tm_min = 0;
        candidate.tm_sec = 0;
        candidate.tm_isdst = -1;
        let mut next = unsafe { libc::mktime(&mut candidate) };
        if next < 0 {
            return Err("scheduler could not resolve the next local window".to_string());
        }
        if u64::try_from(next)
            .ok()
            .is_none_or(|value| value <= now_unix_s)
        {
            candidate = local;
            candidate.tm_mday = candidate
                .tm_mday
                .checked_add(1)
                .ok_or_else(|| "scheduler next local day overflow".to_string())?;
            candidate.tm_hour = window_start_hour as i32;
            candidate.tm_min = 0;
            candidate.tm_sec = 0;
            candidate.tm_isdst = -1;
            next = unsafe { libc::mktime(&mut candidate) };
            if next < 0 {
                return Err("scheduler could not resolve the next local day window".to_string());
            }
        }
        let next = u64::try_from(next)
            .map_err(|_| "scheduler next local window is negative".to_string())?;
        if next <= now_unix_s {
            return Err("scheduler next local window did not advance".to_string());
        }
        Some(next)
    };
    let year = local.tm_year + 1900;
    let month_number = local.tm_mon + 1;
    let day = format!("{year:04}{month_number:02}{:02}", local.tm_mday);
    let month = format!("{year:04}{month_number:02}");
    Ok(LocalCalendar {
        now_unix_s,
        day,
        month,
        hour,
        window_open,
        next_window_open_unix_s,
    })
}

fn window_contains_hour(hour: u8, start: u8, end: u8) -> bool {
    if start == end {
        true
    } else if start < end {
        hour >= start && hour < end
    } else {
        hour >= start || hour < end
    }
}

pub fn attest_no_competing_calibration_processes(
    proc_root: &Path,
    own_pid: u32,
) -> Result<(), String> {
    let mut inspected = 0_usize;
    for entry in fs::read_dir(proc_root).map_err(|error| {
        format!("unable to enumerate processes for scheduler ownership: {error}")
    })? {
        let entry = entry.map_err(|error| {
            format!("unable to inspect process for scheduler ownership: {error}")
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        if pid == 0 || pid == own_pid {
            continue;
        }
        inspected = inspected
            .checked_add(1)
            .ok_or_else(|| "scheduler process enumeration overflow".to_string())?;
        if inspected > MAX_PROC_ENTRIES {
            return Err("scheduler process enumeration exceeds its bound".to_string());
        }
        let path = entry.path().join("cmdline");
        let mut bytes = Vec::new();
        match File::open(&path).and_then(|file| {
            file.take((MAX_CMDLINE_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
        }) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(format!(
                    "unable to inspect process {pid} for scheduler ownership: {error}"
                ))
            }
        }
        if bytes.len() > MAX_CMDLINE_BYTES {
            return Err(format!(
                "process {pid} command line exceeds the scheduler ownership bound"
            ));
        }
        let arguments: Vec<&[u8]> = bytes
            .split(|byte| *byte == 0)
            .filter(|argument| !argument.is_empty())
            .collect();
        if arguments.is_empty() {
            continue;
        }
        let legacy_scheduler = arguments
            .windows(2)
            .any(|pair| pair[0].ends_with(b"/autotune-scheduler") && pair[1] == b"run");
        let native_coordinator = arguments
            .iter()
            .any(|argument| *argument == b"--calibrationd")
            && arguments
                .iter()
                .any(|argument| argument.ends_with(b"/cake-autorated"));
        if legacy_scheduler || native_coordinator {
            return Err(format!(
                "competing calibration scheduler/coordinator process {pid} is active"
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledReservation {
    pub reservation_id: String,
    pub job_id: String,
    pub due_unix_s: u64,
    pub traffic_budget_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScheduledSettlement {
    Success,
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuietEvidence {
    runtime_generation: Option<u64>,
    first_snapshot_unix_ms: Option<u64>,
    last_snapshot_unix_ms: Option<u64>,
}

impl QuietEvidence {
    pub fn observe(
        &mut self,
        snapshot: &RatingRuntimeSnapshot,
        active_threshold_kbps: u64,
        required_window_s: u64,
    ) -> bool {
        let rate = snapshot.dl_achieved_kbps + snapshot.ul_achieved_kbps;
        let eligible = snapshot.runtime_generation != 0
            && snapshot.updated_unix_ms != 0
            && rate.is_finite()
            && rate >= 0.0
            && rate <= active_threshold_kbps as f64;
        if !eligible || self.runtime_generation != Some(snapshot.runtime_generation) {
            self.runtime_generation = eligible.then_some(snapshot.runtime_generation);
            self.first_snapshot_unix_ms = eligible.then_some(snapshot.updated_unix_ms);
            self.last_snapshot_unix_ms = eligible.then_some(snapshot.updated_unix_ms);
            return false;
        }
        if self
            .last_snapshot_unix_ms
            .is_some_and(|last| snapshot.updated_unix_ms < last)
        {
            // A timestamp regression cannot be joined to either side of the
            // prior quiet interval.  Ignore the regressed sample and require a
            // later authoritative snapshot to start a completely new window.
            self.reset();
            return false;
        }
        if self.last_snapshot_unix_ms.is_some_and(|last| {
            snapshot.updated_unix_ms.saturating_sub(last) > SCHEDULER_RUNTIME_FRESHNESS_MS
        }) {
            // Two quiet samples separated by a publication stall do not prove
            // that the missing interval was quiet.  The first post-stall
            // authoritative sample starts a new evidence window.
            self.runtime_generation = Some(snapshot.runtime_generation);
            self.first_snapshot_unix_ms = Some(snapshot.updated_unix_ms);
            self.last_snapshot_unix_ms = Some(snapshot.updated_unix_ms);
            return false;
        }
        let required_ms = required_window_s.saturating_mul(1_000);
        if self
            .last_snapshot_unix_ms
            .is_some_and(|last| snapshot.updated_unix_ms == last)
        {
            // Reactor wakes are broader than rating publication: control,
            // config and unrelated runtime-tree events may all re-read the
            // same atomic snapshot.  A duplicate supplies no new evidence,
            // but it must not erase already accumulated distinct evidence.
            return self.first_snapshot_unix_ms.is_some_and(|first| {
                snapshot.updated_unix_ms.saturating_sub(first) >= required_ms
            });
        }
        self.last_snapshot_unix_ms = Some(snapshot.updated_unix_ms);
        self.first_snapshot_unix_ms
            .is_some_and(|first| snapshot.updated_unix_ms.saturating_sub(first) >= required_ms)
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

pub fn load_or_initialize_state(
    store: &SchedulerStore,
    config: &ScheduledInstanceConfig,
    now_unix_s: u64,
    day: &str,
    month: &str,
) -> Result<SchedulerInstanceState, String> {
    if now_unix_s == 0 {
        return Err("scheduler cannot initialize from a zero wall clock".to_string());
    }
    let mut state = match store.load_state(&config.instance)? {
        Some(state) => state,
        None => SchedulerInstanceState::new(
            ScheduleCursor::new(config.instance.clone(), now_unix_s)?,
            BudgetLedger::new(
                config.instance.clone(),
                day.to_string(),
                month.to_string(),
                config.daily_limit_bytes,
                config.monthly_limit_bytes,
            )?,
        )?,
    };
    let before = state.clone();
    state.budget.reconfigure_limits(
        day,
        month,
        config.daily_limit_bytes,
        config.monthly_limit_bytes,
    )?;
    if state != before || store.load_state(&config.instance)?.is_none() {
        store.persist_state(&state)?;
    }
    Ok(state)
}

#[allow(clippy::too_many_arguments)]
pub fn reserve_scheduled_request(
    store: &SchedulerStore,
    config: &ScheduledInstanceConfig,
    state: &mut SchedulerInstanceState,
    observation: &SchedulerObservation,
    request: &OperationRequest,
    reservation_id: String,
    day: &str,
    month: &str,
) -> Result<(SchedulerDecision, Option<ScheduledReservation>), String> {
    state.validate()?;
    request.validate()?;
    if request.origin != OperationOrigin::Scheduler {
        return Err("native scheduler request has an untrusted origin".to_string());
    }
    if request.scheduled_auto_apply_requested != config.auto_apply {
        return Err(
            "native scheduler request Auto-Apply authority differs from committed policy"
                .to_string(),
        );
    }
    if request.identity.instance != config.instance
        || state.cursor.instance != config.instance
        || request.identity.config_fingerprint != observation.generations.config_fingerprint
        || request.identity.route_fingerprint != observation.generations.route_fingerprint
    {
        return Err("native scheduler request and observed authority differ".to_string());
    }
    let available = state.budget.available_bytes()?;
    let decision = evaluate_schedule(
        config.enabled(),
        config.interval_s,
        request.traffic_budget_bytes,
        available,
        &state.cursor,
        observation,
    )?;
    let SchedulerDecision::Admit {
        due_unix_s,
        traffic_budget_bytes,
    } = decision
    else {
        return Ok((decision, None));
    };
    if traffic_budget_bytes != request.traffic_budget_bytes {
        return Err("live scheduled request exceeds the admitted traffic budget".to_string());
    }
    let authority = FailedAttemptFence {
        due_unix_s,
        interval_s: config.interval_s,
        generations: observation.generations.clone(),
        explicit_retry_sequence: observation.explicit_retry_sequence,
    };
    state.budget.reserve(
        reservation_id.clone(),
        request.identity.job_id.clone(),
        day,
        month,
        authority,
        traffic_budget_bytes,
    )?;
    store.persist_state(state)?;
    Ok((
        SchedulerDecision::Admit {
            due_unix_s,
            traffic_budget_bytes,
        },
        Some(ScheduledReservation {
            reservation_id,
            job_id: request.identity.job_id.clone(),
            due_unix_s,
            traffic_budget_bytes,
        }),
    ))
}

#[allow(clippy::too_many_arguments)]
pub fn settle_scheduled_request(
    store: &SchedulerStore,
    state: &mut SchedulerInstanceState,
    reservation_id: &str,
    job_id: &str,
    day: &str,
    month: &str,
    completed_unix_s: u64,
    consumed_traffic_bytes: u64,
    settlement: ScheduledSettlement,
) -> Result<(), String> {
    let authority = state
        .budget
        .reservation
        .as_ref()
        .ok_or_else(|| "scheduled settlement has no durable reservation".to_string())?
        .authority
        .clone();
    state
        .budget
        .settle(reservation_id, job_id, day, month, consumed_traffic_bytes)?;
    match settlement {
        ScheduledSettlement::Success => state.cursor.record_success(
            authority.interval_s,
            authority.due_unix_s,
            completed_unix_s,
        )?,
        ScheduledSettlement::Failed => state.cursor.record_failure(authority)?,
    }
    state.validate()?;
    store.persist_state(state)
}

pub fn mark_scheduled_accounting_unknown(
    store: &SchedulerStore,
    state: &mut SchedulerInstanceState,
    reservation_id: &str,
    job_id: &str,
) -> Result<(), String> {
    let authority = state
        .budget
        .reservation
        .as_ref()
        .ok_or_else(|| "unknown scheduled accounting has no durable reservation".to_string())?
        .authority
        .clone();
    state
        .budget
        .mark_accounting_unknown(reservation_id, job_id)?;
    state.cursor.record_failure(authority)?;
    state.validate()?;
    store.persist_state(state)
}

pub fn acknowledge_scheduled_accounting_unknown(
    store: &SchedulerStore,
    state: &mut SchedulerInstanceState,
) -> Result<u64, String> {
    let charged_bytes = state.budget.acknowledge_accounting_unknown()?;
    state.validate()?;
    store.persist_state(state)?;
    Ok(charged_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autotune::{
        AccessEvidenceSource, AccessMedium, AutotuneProfile, CapacityLearningPolicy,
    };
    use crate::operations::protocol::{
        CalibrationStrategy, OperationIdentity, OperationKind, OperationRouteIdentity,
        OperationRouteMode, OperationTargetState,
    };
    use crate::operations::scheduler::{SchedulerGates, SchedulerGenerations};
    use std::net::{IpAddr, Ipv4Addr};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

    fn root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "cake-autorate-scheduler-runtime-{name}-{}-{}",
            std::process::id(),
            TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn config() -> ScheduledInstanceConfig {
        ScheduledInstanceConfig {
            instance: "wan_sqm".to_string(),
            instance_enabled: true,
            scheduled_enabled: true,
            interval_s: 3_600,
            idle_window_s: 60,
            window_start_hour: 2,
            window_end_hour: 5,
            daily_limit_bytes: 10_000,
            monthly_limit_bytes: 50_000,
            auto_apply: false,
            active_threshold_kbps: 2_000,
            expected_target_interface: "pppoe-wan".to_string(),
            backend: "speedtest-go".to_string(),
            route_mode: "main".to_string(),
            mwan3_member: String::new(),
            profile: AutotuneProfile::VariableLink,
            strategy: CalibrationStrategy::FullRaw,
            access_medium: AccessMedium::Cellular,
            access_source: AccessEvidenceSource::UserSelected,
            access_confidence_percent: 100,
            capacity_learning_policy: CapacityLearningPolicy::ScheduledActive,
            service_dl_cap_kbps: None,
            service_ul_cap_kbps: None,
        }
    }

    fn generations() -> SchedulerGenerations {
        SchedulerGenerations {
            config_fingerprint: "a".repeat(64),
            route_fingerprint: "b".repeat(64),
            runtime_sequence: 7,
            coordinator_generation: "c".repeat(32),
        }
    }

    fn observation(now_unix_s: u64) -> SchedulerObservation {
        SchedulerObservation {
            now_unix_s,
            window_open: true,
            next_window_open_unix_s: None,
            generations: generations(),
            explicit_retry_sequence: 0,
            gates: SchedulerGates {
                configuration_committed: true,
                recovery_clear: true,
                manual_job_pending: false,
                coordinator_idle: true,
                route_ready: true,
                runtime_ready: true,
                quiet_window_ready: true,
                accounting_healthy: true,
                scheduled_job_active: false,
            },
        }
    }

    fn request(budget: u64) -> OperationRequest {
        OperationRequest {
            identity: OperationIdentity {
                job_id: "1".repeat(32),
                job_token: "2".repeat(64),
                instance: "wan_sqm".to_string(),
                operation: OperationKind::FullAutotune,
                target_interface: "pppoe-wan".to_string(),
                route_fingerprint: "b".repeat(64),
                config_fingerprint: "a".repeat(64),
                sqm_fingerprint: "d".repeat(64),
            },
            created_unix_ms: 1,
            deadline_unix_ms: 600_001,
            origin: OperationOrigin::Scheduler,
            backend: "speedtest-go".to_string(),
            speedtest_direction: None,
            speedtest_server_id: None,
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
            profile: Some(AutotuneProfile::VariableLink),
            strategy: Some(CalibrationStrategy::FullRaw),
            access_medium: Some(AccessMedium::Cellular),
            access_source: Some(AccessEvidenceSource::UserSelected),
            access_confidence_percent: 100,
            capacity_learning_policy: Some(CapacityLearningPolicy::ScheduledActive),
            service_dl_cap_kbps: None,
            service_ul_cap_kbps: None,
            allow_sqm_disable: true,
            allow_active_traffic: false,
            scheduled_auto_apply_requested: false,
            traffic_budget_bytes: budget,
        }
    }

    fn snapshot(generation: u64, updated_unix_ms: u64, rate: f64) -> RatingRuntimeSnapshot {
        RatingRuntimeSnapshot {
            updated_unix_ms,
            capture_observed_unix_ms: updated_unix_ms,
            runtime_generation: generation,
            uplink_state: "ACTIVE".to_string(),
            route_active: true,
            route_test_ready: true,
            sqm_runtime_managed: true,
            sqm_runtime_healthy: true,
            transport_probe_trusted: true,
            baseline_ready: true,
            baseline_samples: 4,
            baseline_required_samples: 4,
            required_samples: 4,
            evidence_contract:
                crate::operations::rating::RatingEvidenceContract::WorstOfDirectionBoundIcmpAndTransport,
            dl_samples: 4,
            ul_samples: 4,
            dl_achieved_kbps: rate,
            ul_achieved_kbps: 0.0,
            cake_dl_kbps: 10_000.0,
            cake_ul_kbps: 1_000.0,
            download_qdisc_kind: Some(crate::operations::autotune_runtime::RuntimeQdiscKind::Cake),
            upload_qdisc_kind: Some(crate::operations::autotune_runtime::RuntimeQdiscKind::Cake),
            reference_dl_kbps: 10_000.0,
            reference_ul_kbps: 1_000.0,
            capture_active: false,
            capture_job_id: String::new(),
            capture_generation: 0,
            finalized_job_id: String::new(),
            finalized_generation: 0,
            finalized_outcome: String::new(),
            capture_phase: "IDLE".to_string(),
            capture_contaminated: false,
            current_capture_job_id: String::new(),
            current_capture_generation: 0,
            current: None,
        }
    }

    #[test]
    fn reservation_and_success_settle_cursor_and_budget_in_one_record() {
        let root = root("success");
        let store = SchedulerStore::open(&root).unwrap();
        let config = config();
        let mut state =
            load_or_initialize_state(&store, &config, 1_000, "20260805", "202608").unwrap();
        let (decision, reserved) = reserve_scheduled_request(
            &store,
            &config,
            &mut state,
            &observation(1_000),
            &request(10_000),
            "3".repeat(32),
            "20260805",
            "202608",
        )
        .unwrap();
        assert!(matches!(decision, SchedulerDecision::Admit { .. }));
        assert_eq!(reserved.unwrap().traffic_budget_bytes, 10_000);
        settle_scheduled_request(
            &store,
            &mut state,
            &"3".repeat(32),
            &"1".repeat(32),
            "20260805",
            "202608",
            1_100,
            2_500,
            ScheduledSettlement::Success,
        )
        .unwrap();
        let restored = store.load_state("wan_sqm").unwrap().unwrap();
        assert_eq!(restored.cursor.last_success_unix_s, Some(1_100));
        assert_eq!(restored.budget.daily_charged_bytes, 2_500);
        assert!(restored.budget.reservation.is_none());
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reservation_binds_immutable_auto_apply_authority_to_committed_policy() {
        let root = root("auto-apply-authority");
        let store = SchedulerStore::open(&root).unwrap();
        let mut config = config();
        let mut state =
            load_or_initialize_state(&store, &config, 1_000, "20260805", "202608").unwrap();
        let mut operation = request(10_000);
        operation.scheduled_auto_apply_requested = true;
        assert!(reserve_scheduled_request(
            &store,
            &config,
            &mut state,
            &observation(1_000),
            &operation,
            "3".repeat(32),
            "20260805",
            "202608",
        )
        .unwrap_err()
        .contains("differs from committed policy"));

        config.auto_apply = true;
        let (_, reservation) = reserve_scheduled_request(
            &store,
            &config,
            &mut state,
            &observation(1_000),
            &operation,
            "3".repeat(32),
            "20260805",
            "202608",
        )
        .unwrap();
        assert!(reservation.is_some());
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unchanged_limits_persist_exhausted_budget_rollover() {
        let root = root("persist-period-rollover");
        let store = SchedulerStore::open(&root).unwrap();
        let config = config();
        let mut state =
            load_or_initialize_state(&store, &config, 1_000, "20260805", "202608").unwrap();
        reserve_scheduled_request(
            &store,
            &config,
            &mut state,
            &observation(1_000),
            &request(10_000),
            "3".repeat(32),
            "20260805",
            "202608",
        )
        .unwrap();
        settle_scheduled_request(
            &store,
            &mut state,
            &"3".repeat(32),
            &"1".repeat(32),
            "20260805",
            "202608",
            1_100,
            10_000,
            ScheduledSettlement::Success,
        )
        .unwrap();
        assert_eq!(state.budget.available_bytes().unwrap(), 0);

        let rolled =
            load_or_initialize_state(&store, &config, 2_000, "20260806", "202608").unwrap();
        assert_eq!(rolled.budget.day, "20260806");
        assert_eq!(rolled.budget.daily_charged_bytes, 0);
        assert_eq!(rolled.budget.monthly_charged_bytes, 10_000);
        assert_eq!(rolled.budget.available_bytes().unwrap(), 10_000);

        let restored = store.load_state("wan_sqm").unwrap().unwrap();
        assert_eq!(restored, rolled);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unknown_accounting_atomically_fences_the_slot_and_budget() {
        let root = root("unknown");
        let store = SchedulerStore::open(&root).unwrap();
        let config = config();
        let mut state =
            load_or_initialize_state(&store, &config, 1_000, "20260805", "202608").unwrap();
        reserve_scheduled_request(
            &store,
            &config,
            &mut state,
            &observation(1_000),
            &request(10_000),
            "3".repeat(32),
            "20260805",
            "202608",
        )
        .unwrap();
        mark_scheduled_accounting_unknown(&store, &mut state, &"3".repeat(32), &"1".repeat(32))
            .unwrap();
        let restored = store.load_state("wan_sqm").unwrap().unwrap();
        assert!(restored.budget.accounting_blocked);
        assert!(restored.cursor.failed_attempt.is_some());
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn operator_acknowledgement_is_durable_and_does_not_refund_the_reservation() {
        let root = root("acknowledge-unknown");
        let store = SchedulerStore::open(&root).unwrap();
        let config = config();
        let mut state =
            load_or_initialize_state(&store, &config, 1_000, "20260805", "202608").unwrap();
        reserve_scheduled_request(
            &store,
            &config,
            &mut state,
            &observation(1_000),
            &request(10_000),
            "3".repeat(32),
            "20260805",
            "202608",
        )
        .unwrap();
        mark_scheduled_accounting_unknown(&store, &mut state, &"3".repeat(32), &"1".repeat(32))
            .unwrap();

        assert_eq!(
            acknowledge_scheduled_accounting_unknown(&store, &mut state).unwrap(),
            10_000
        );
        let restored = store.load_state("wan_sqm").unwrap().unwrap();
        assert!(!restored.budget.accounting_blocked);
        assert!(restored.budget.reservation.is_none());
        assert_eq!(restored.budget.daily_charged_bytes, 10_000);
        assert_eq!(restored.budget.monthly_charged_bytes, 10_000);
        assert!(restored.cursor.failed_attempt.is_some());
        let acknowledged_sequence = restored.budget.sequence;
        drop(store);

        let reopened = SchedulerStore::open(&root).unwrap();
        let mut replay = reopened.load_state("wan_sqm").unwrap().unwrap();
        let before_replay = replay.clone();
        let error = acknowledge_scheduled_accounting_unknown(&reopened, &mut replay).unwrap_err();
        assert!(error.contains("scheduler traffic accounting is not blocked"));
        assert_eq!(replay, before_replay);
        assert_eq!(replay.budget.sequence, acknowledged_sequence);
        assert_eq!(
            reopened.load_state("wan_sqm").unwrap().unwrap(),
            before_replay
        );
        drop(reopened);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn quiet_window_advances_only_on_distinct_same_generation_samples() {
        let mut quiet = QuietEvidence::default();
        for timestamp in (1_000..=61_000).step_by(5_000) {
            let qualified = timestamp == 61_000;
            assert_eq!(
                quiet.observe(&snapshot(7, timestamp, 100.0), 2_000, 60),
                qualified
            );
            for _ in 0..3 {
                assert_eq!(
                    quiet.observe(&snapshot(7, timestamp, 100.0), 2_000, 60),
                    qualified
                );
            }
        }
        assert!(!quiet.observe(&snapshot(8, 66_000, 100.0), 2_000, 60));
        assert!(!quiet.observe(&snapshot(8, 71_000, 3_000.0), 2_000, 60));
    }

    #[test]
    fn quiet_window_timestamp_regression_requires_fresh_evidence() {
        let mut quiet = QuietEvidence::default();
        assert!(!quiet.observe(&snapshot(7, 1_000, 100.0), 2_000, 30));
        assert!(!quiet.observe(&snapshot(7, 6_000, 100.0), 2_000, 30));
        assert!(!quiet.observe(&snapshot(7, 2_000, 100.0), 2_000, 30));
        for timestamp in (7_000..=37_000).step_by(5_000) {
            assert_eq!(
                quiet.observe(&snapshot(7, timestamp, 100.0), 2_000, 30),
                timestamp == 37_000
            );
        }
    }

    #[test]
    fn quiet_window_publication_gap_starts_a_new_window() {
        let mut quiet = QuietEvidence::default();
        assert!(!quiet.observe(&snapshot(7, 1_000, 100.0), 2_000, 30));
        assert!(!quiet.observe(&snapshot(7, 6_000, 100.0), 2_000, 30));
        assert!(!quiet.observe(&snapshot(7, 12_000, 100.0), 2_000, 30));
        for timestamp in (17_000..=42_000).step_by(5_000) {
            assert_eq!(
                quiet.observe(&snapshot(7, timestamp, 100.0), 2_000, 30),
                timestamp == 42_000
            );
        }
    }

    #[test]
    fn local_windows_handle_daytime_overnight_and_always_open_shapes() {
        assert!(window_contains_hour(3, 2, 5));
        assert!(!window_contains_hour(5, 2, 5));
        assert!(window_contains_hour(23, 22, 4));
        assert!(window_contains_hour(2, 22, 4));
        assert!(!window_contains_hour(12, 22, 4));
        assert!(window_contains_hour(12, 7, 7));

        let calendar = local_calendar(1_786_000_000, 0, 0).unwrap();
        assert_eq!(calendar.day.len(), 8);
        assert_eq!(calendar.month.len(), 6);
        assert!(calendar.window_open);
        assert_eq!(calendar.next_window_open_unix_s, None);
    }

    #[test]
    fn process_attestation_rejects_legacy_scheduler_and_another_coordinator() {
        let root = root("proc-attestation");
        std::fs::create_dir_all(root.join("101")).unwrap();
        std::fs::write(
            root.join("101/cmdline"),
            b"/bin/sh\0/usr/libexec/cake-autorate-rs/autotune-scheduler\0run\0",
        )
        .unwrap();
        assert!(attest_no_competing_calibration_processes(&root, 999).is_err());

        std::fs::remove_dir_all(root.join("101")).unwrap();
        std::fs::create_dir_all(root.join("102")).unwrap();
        std::fs::write(
            root.join("102/cmdline"),
            b"/usr/sbin/cake-autorated\0--calibrationd\0--native-autotune\0",
        )
        .unwrap();
        assert!(attest_no_competing_calibration_processes(&root, 999).is_err());
        assert!(attest_no_competing_calibration_processes(&root, 102).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }
}
