use super::protocol::{
    ControlCommand, ControlMessage, ControlRequest, OperationKind, OperationOrigin,
    OperationRequest, OperationRouteMode, OperationState, OperationTargetState,
    MAX_CONTROL_MESSAGE_BYTES, MAX_OPERATION_RECORD_BYTES, OPERATION_PROTOCOL_VERSION,
};
use super::{
    autotune_apply::{
        NativeApplyAcknowledgement, NativeApplyExecutionPlan, MAX_NATIVE_APPLY_ACKNOWLEDGEMENTS,
    },
    autotune_apply_openwrt::{default_native_apply_paths, OpenWrtNativeApplyBackend},
    autotune_apply_runtime::{
        execute_native_apply_commit, execute_native_apply_commit_with_fault,
        execute_native_apply_forced_rollback_with_fault, recover_native_apply,
        NativeApplyCommitDisposition, NativeApplyLabFaultInjection, NativeApplyTransactionBackend,
    },
    autotune_bootstrap_apply::{NativeBootstrapApplyPlan, NativeBootstrapApplyPolicy},
    autotune_bootstrap_apply_recovery::NativeBootstrapApplyRecoveryStore,
    autotune_bootstrap_apply_runtime::{
        execute_native_bootstrap_apply_commit, recover_native_bootstrap_apply,
        NativeBootstrapApplyBackend,
    },
    autotune_request::{
        build_bootstrap_autotune_request, build_live_autotune_request,
        build_live_scheduled_autotune_request, parse_launch_intent,
    },
    autotune_runtime::{
        speedtest_unshaped_topology, AbsentRuntimeBaseline, AutotuneRuntimePermit, RuntimeBaseline,
        RuntimePermitKind, RuntimeQdiscKind, RuntimeRateBounds, TemporaryTopologyStage,
    },
    autotune_runtime_store::RuntimeOverrideStore,
    bootstrap_runtime_owner::read_bootstrap_runtime_owner_claim,
    event_loop::{install_child_signal_handler, CalibrationEventLoop},
    full_autotune::{self, AutotuneTerminal, RuntimeAckState},
    identity::{monotonic_boot_ms, CoordinatorIdentity, ProcessIdentity, DEFAULT_PROC_ROOT},
    journal::{
        JobJournal, JournalDisposition, JournalStore, NativeJobPaths, ScannedJob,
        JOURNAL_RETENTION_TARGET, MAX_JOURNAL_JOBS,
    },
    json_wire::{bool_json, json_escape},
    lease::{requires_heavy_traffic, LeaseAcquireError, LeaseRequest, LeaseTable},
    native_apply_coordinator::{
        NativeApplyAdmission, NativeApplyControlCommand, NativeApplyControlRequest,
        NativeApplyCoordinatorStore, NativeApplyDispatchRecord, NativeApplyDispatchState,
        NativeApplyTerminalOutcome, NativeApplyTerminalRecord, NativeApplyVerifiedDispatchIdentity,
        NativeApplyWorkerClaim,
    },
    process::{signal_adopted_group, ManagedChild, SpawnSpec},
    rating::{self, RatingTerminal},
    rating_request::{build_live_rating_request, parse_rating_launch_intent},
    runtime::{attest_openwrt_runtime, RuntimeAttestation},
    scheduler::{
        validate_scheduler_instance, SchedulerGates, SchedulerGenerations, SchedulerObservation,
    },
    scheduler_config::{
        load_scheduled_instances, scheduled_configuration_committed, ScheduledInstanceConfig,
        SchedulerConfigSnapshot,
    },
    scheduler_owner::{SchedulerOwnerLock, PRODUCTION_SCHEDULER_OWNER_LOCK},
    scheduler_runtime::{
        acknowledge_scheduled_accounting_unknown, attest_no_competing_calibration_processes,
        load_or_initialize_state, local_calendar, mark_scheduled_accounting_unknown,
        reserve_scheduled_request, settle_scheduled_request, QuietEvidence, ScheduledSettlement,
        SCHEDULER_RUNTIME_FRESHNESS_MS,
    },
    scheduler_status::{
        format_native_scheduler_batch, format_native_scheduler_issue,
        native_scheduler_instance_wake_at, native_scheduler_status_response,
        sanitize_scheduler_public_message, NativeSchedulerStatusRows, MAX_SCHEDULER_STATUS_ENTRIES,
        MAX_SCHEDULER_STATUS_RESPONSE_BYTES, SCHEDULER_STATUS_GLOBAL_ERROR,
    },
    scheduler_store::SchedulerStore,
    scheduler_store::PRODUCTION_SCHEDULER_STORE_ROOT as PRODUCTION_SCHEDULER_STORE_DIR,
    service_lifecycle::confirm_controller_service_started,
    speedtest::{self, SpeedtestTerminal},
    speedtest_request::{
        build_bootstrap_speedtest_request, build_live_speedtest_request,
        parse_speedtest_launch_intent,
    },
    state,
};
use crate::Config;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::{c_void, OsString};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[cfg(test)]
use super::bootstrap_runtime_owner::BootstrapRuntimeOwnerClaim;
#[cfg(test)]
use std::thread;

const NATIVE_AUTOTUNE_RUNTIME_PERMIT_MAX_MS: u64 = 30 * 60 * 1_000;
const NATIVE_APPLY_LAB_MODE_ENV: &str = "CAKE_AUTORATE_NATIVE_APPLY_LAB_MODE";
const NATIVE_APPLY_LAB_FORCED_ROLLBACK: &str = "forced-rollback-v1";
const NATIVE_APPLY_LAB_CRASH_AFTER_SERVICE_RESTARTED: &str =
    "forced-rollback-crash-after-service-restarted-v1";
const NATIVE_APPLY_LAB_COMMIT: &str = "commit-v1";
const NATIVE_APPLY_LAB_CRASH_BEFORE_COMMIT: &str = "commit-crash-before-accept-v1";
const NATIVE_APPLY_LAB_CRASH_AFTER_COMMIT: &str = "commit-crash-after-accept-v1";
const NATIVE_APPLY_LAB_CRASH_PAUSE: Duration = Duration::from_secs(30);

fn native_autotune_rate_bounds(
    direction: &str,
    current_kbps: u64,
    bounds: RuntimeRateBounds,
) -> Result<RuntimeRateBounds, String> {
    if current_kbps > bounds.maximum_kbps {
        return Err(format!(
            "current {direction} CAKE rate exceeds the requested service hard cap"
        ));
    }
    Ok(bounds)
}

const DEFAULT_STATE_DIR: &str = "/var/run/cake-autorate-calibration";
pub(crate) const PRODUCTION_CALIBRATION_STATE_DIR: &str = DEFAULT_STATE_DIR;
const NATIVE_OPERATION_STATUS_IDENTITY_VERSION: u32 = 1;
const SCHEDULER_ACK_HEADER: &str = "cake-autorate-scheduler\t1\tacknowledge-accounting";
const SCHEDULER_STATUS_HEADER: &str = "cake-autorate-scheduler\t1\tstatus";
const CONTROL_SOCKET_NAME: &str = "control.sock";
const IO_TIMEOUT: Duration = Duration::from_secs(3);
const NATIVE_APPLY_WATCH_HOLD: Duration = Duration::from_secs(25);
const NATIVE_APPLY_WATCH_CLIENT_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_PENDING_NATIVE_APPLY_WATCHES: usize = 32;
const MAX_RESPONSE_BYTES: usize = 8 * 1024;
const MAX_RESULT_RESPONSE_BYTES: usize = 512 * 1024;
const WORKER_CANCEL_GRACE: Duration = Duration::from_secs(15);
type RoutePinCleaner = fn(&str, &str) -> Result<(), String>;
type BootstrapRuntimeAttestor = fn(&OperationRequest, &AbsentRuntimeBaseline) -> Result<(), String>;
type BootstrapRuntimeBaselineCapturer =
    fn(&OperationRequest) -> Result<AbsentRuntimeBaseline, String>;

fn attest_openwrt_bootstrap_runtime(
    request: &OperationRequest,
    baseline: &AbsentRuntimeBaseline,
) -> Result<(), String> {
    OpenWrtNativeApplyBackend::new().attest_bootstrap_runtime_absence(request, baseline)
}

fn capture_openwrt_bootstrap_runtime(
    request: &OperationRequest,
) -> Result<AbsentRuntimeBaseline, String> {
    OpenWrtNativeApplyBackend::new()
        .capture_bootstrap_runtime_baseline(request, &kernel_request_id()?)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeWorkerKind {
    Rating,
    Speedtest,
    Autotune,
}

impl NativeWorkerKind {
    fn flag(self) -> &'static str {
        match self {
            Self::Rating => "--rating-worker",
            Self::Speedtest => "--speedtest-worker",
            Self::Autotune => "--autotune-worker",
        }
    }

    fn arguments(self, paths: &NativeJobPaths, worker_run_id: &str) -> Vec<OsString> {
        let mut arguments = vec![
            OsString::from(self.flag()),
            OsString::from("--request"),
            paths.request.as_os_str().to_os_string(),
            OsString::from("--terminal"),
            paths.terminal.as_os_str().to_os_string(),
        ];
        if self == Self::Autotune {
            arguments.extend([
                OsString::from("--review"),
                paths.review.as_os_str().to_os_string(),
            ]);
        }
        arguments.extend([
            OsString::from("--permit"),
            paths.permit.as_os_str().to_os_string(),
            OsString::from("--worker-run-id"),
            OsString::from(worker_run_id),
        ]);
        arguments
    }
}

fn bootstrap_runtime_owner_arguments(paths: &NativeJobPaths, worker_run_id: &str) -> Vec<OsString> {
    vec![
        OsString::from("--bootstrap-runtime-owner"),
        OsString::from("--request"),
        paths.request.as_os_str().to_os_string(),
        OsString::from("--runtime-dir"),
        paths.bootstrap_runtime_dir.as_os_str().to_os_string(),
        OsString::from("--worker-run-id"),
        OsString::from(worker_run_id),
    ]
}

enum RuntimePermitReadiness {
    Ready(AutotuneRuntimePermit),
    Waiting { code: String, message: String },
    Unsafe { code: String, message: String },
}

enum NativeRuntimeSnapshotReadiness {
    Ready(rating::RatingRuntimeSnapshot),
    Waiting { code: String, message: String },
    Unsafe { code: String, message: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum BootstrapNoOwnerRecoveryReadiness {
    OwnerRequired,
    OwnerExitPending(ProcessIdentity),
    RestoredFinalizationPending,
    Restored,
}

fn operation_uses_native_route_pin(request: &OperationRequest) -> bool {
    matches!(
        request.identity.operation,
        OperationKind::Speedtest | OperationKind::AutomaticRating | OperationKind::FullAutotune
    )
}

fn request_requires_native_runtime(request: &OperationRequest) -> bool {
    request.identity.operation == OperationKind::FullAutotune
        || (request.identity.operation == OperationKind::Speedtest
            && request.target_state == OperationTargetState::ExistingManaged
            && request.speedtest_topology == Some(super::protocol::SpeedtestTopology::Unshaped))
}

fn cleanup_native_route_pin(
    request: &OperationRequest,
    worker_run_id: &str,
    cleaner: RoutePinCleaner,
) -> Result<(), String> {
    if operation_uses_native_route_pin(request) {
        cleaner(&request.identity.job_id, worker_run_id)?;
    }
    Ok(())
}

fn native_runtime_snapshot_path(request: &OperationRequest) -> PathBuf {
    env::var_os("CAKE_AUTORATE_RUN_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/run/cake-autorate"))
        .join(&request.identity.instance)
        .join("rating-runtime")
}

/// Observe only the instance-owned runtime publication.  This inexpensive
/// state gate runs before the heavyweight UCI/route/tc attestor, so a sleeping
/// or booting instance does not cause repeated shell work.  No elapsed retry
/// count changes the outcome: the current atomic snapshot is authoritative.
fn native_runtime_snapshot_readiness(request: &OperationRequest) -> NativeRuntimeSnapshotReadiness {
    let now_unix_ms = match rating::epoch_ms() {
        Ok(value) => value,
        Err(error) => {
            return NativeRuntimeSnapshotReadiness::Unsafe {
                code: "runtime-clock-unavailable".to_string(),
                message: error,
            }
        }
    };
    native_runtime_snapshot_readiness_at(
        request,
        &native_runtime_snapshot_path(request),
        now_unix_ms,
    )
}

fn native_runtime_snapshot_readiness_at(
    request: &OperationRequest,
    path: &Path,
    now_unix_ms: u64,
) -> NativeRuntimeSnapshotReadiness {
    if now_unix_ms >= request.deadline_unix_ms {
        return NativeRuntimeSnapshotReadiness::Unsafe {
            code: "operation-deadline-expired".to_string(),
            message: "the immutable operation deadline expired while waiting for instance runtime"
                .to_string(),
        };
    }
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return NativeRuntimeSnapshotReadiness::Waiting {
                code: "runtime-snapshot-missing".to_string(),
                message: "the instance has not published its runtime snapshot yet".to_string(),
            }
        }
        Err(error) => {
            return NativeRuntimeSnapshotReadiness::Unsafe {
                code: "runtime-snapshot-inspection-failed".to_string(),
                message: format!("unable to inspect instance runtime snapshot: {error}"),
            }
        }
    }
    let snapshot = match rating::read_rating_snapshot(path) {
        Ok(value) => value,
        Err(error) => {
            return NativeRuntimeSnapshotReadiness::Unsafe {
                code: "runtime-snapshot-invalid".to_string(),
                message: error,
            }
        }
    };
    if snapshot.updated_unix_ms > now_unix_ms.saturating_add(5_000) {
        return NativeRuntimeSnapshotReadiness::Unsafe {
            code: "runtime-snapshot-clock-drift".to_string(),
            message: "the instance runtime snapshot timestamp is in the future".to_string(),
        };
    }
    if now_unix_ms.saturating_sub(snapshot.updated_unix_ms) > 5_000 {
        return NativeRuntimeSnapshotReadiness::Waiting {
            code: "runtime-snapshot-stale".to_string(),
            message: "the instance runtime snapshot is stale".to_string(),
        };
    }
    if !snapshot.route_test_ready {
        return NativeRuntimeSnapshotReadiness::Waiting {
            code: "runtime-route-not-ready".to_string(),
            message: "the selected instance route is not ready".to_string(),
        };
    }
    if !snapshot.sqm_runtime_managed {
        return NativeRuntimeSnapshotReadiness::Waiting {
            code: "runtime-sqm-not-managed".to_string(),
            message: "the instance has not acquired its managed SQM runtime".to_string(),
        };
    }
    if !snapshot.sqm_runtime_healthy {
        return NativeRuntimeSnapshotReadiness::Waiting {
            code: "runtime-sqm-not-healthy".to_string(),
            message: "the instance managed SQM runtime is not healthy".to_string(),
        };
    }
    if snapshot.capture_active {
        return NativeRuntimeSnapshotReadiness::Waiting {
            code: "runtime-capture-active".to_string(),
            message: "another identity-bound runtime capture is active".to_string(),
        };
    }
    if snapshot.cake_dl_kbps < 100.0 && snapshot.cake_ul_kbps < 100.0 {
        return NativeRuntimeSnapshotReadiness::Waiting {
            code: "runtime-rate-not-ready".to_string(),
            message: "the instance has not published any usable managed CAKE rate".to_string(),
        };
    }
    if (snapshot.cake_dl_kbps >= 100.0 && snapshot.download_qdisc_kind.is_none())
        || (snapshot.cake_ul_kbps >= 100.0 && snapshot.upload_qdisc_kind.is_none())
    {
        return NativeRuntimeSnapshotReadiness::Waiting {
            code: "runtime-qdisc-kind-not-ready".to_string(),
            message: "the instance has not published exact managed qdisc kinds yet".to_string(),
        };
    }
    NativeRuntimeSnapshotReadiness::Ready(snapshot)
}

fn native_runtime_qdisc_kinds(
    snapshot: &rating::RatingRuntimeSnapshot,
    baseline_topology: full_autotune::MeasurementTopology,
) -> Result<(RuntimeQdiscKind, RuntimeQdiscKind), String> {
    let download = if baseline_topology.download_is_shaped() {
        snapshot.download_qdisc_kind.ok_or_else(|| {
            "managed download baseline has no exact published qdisc kind".to_string()
        })?
    } else {
        // A direction absent from the persistent baseline has no live qdisc to
        // identify. The private temporary topology deliberately uses CAKE.
        RuntimeQdiscKind::Cake
    };
    let upload = if baseline_topology.upload_is_shaped() {
        snapshot.upload_qdisc_kind.ok_or_else(|| {
            "managed upload baseline has no exact published qdisc kind".to_string()
        })?
    } else {
        RuntimeQdiscKind::Cake
    };
    Ok((download, upload))
}

extern "C" {
    fn geteuid() -> u32;
    fn getsockopt(
        socket: i32,
        level: i32,
        option_name: i32,
        option_value: *mut c_void,
        option_length: *mut u32,
    ) -> i32;
}

const SOL_SOCKET: i32 = 1;
const SO_PEERCRED: i32 = 17;

#[repr(C)]
struct PeerCredentials {
    pid: i32,
    uid: u32,
    gid: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SchedulerAccountingAcknowledgement {
    request_id: String,
    instance: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SchedulerStatusRequest {
    request_id: String,
}

enum CoordinatorControlRequest {
    Operation(ControlMessage),
    NativeApply(NativeApplyControlRequest),
    SchedulerAccountingAcknowledgement(SchedulerAccountingAcknowledgement),
    SchedulerStatus(SchedulerStatusRequest),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ControlEffect {
    #[default]
    ReadOnly,
    StateChanged,
}

impl ControlEffect {
    fn state_changed(self) -> bool {
        self == Self::StateChanged
    }
}

pub fn run_calibrationd<I>(args: I, terminate: &AtomicBool) -> Result<(), String>
where
    I: Iterator<Item = String>,
{
    let options = parse_daemon_args(args)?;
    let _scheduler_owner = options
        .native_scheduler
        .then(|| SchedulerOwnerLock::open(Path::new(PRODUCTION_SCHEDULER_OWNER_LOCK)))
        .transpose()?;
    if options.native_scheduler {
        // procd respawn re-executes calibrationd without rerunning rc.common
        // start_service. Settle any interrupted native Apply transaction on
        // every production coordinator launch before binding the control
        // socket or admitting new work.
        let _ = native_autotune_apply_recovery()?;
    }
    let mut daemon = CalibrationDaemon::bind(&options.state_dir)?;
    if options.native_rating || options.lab_rust_rating {
        daemon.admission_requested = true;
        daemon.native_rating = true;
    }
    if options.native_speedtest || options.lab_rust_speedtest {
        daemon.admission_requested = true;
        daemon.native_speedtest = true;
    }
    if options.native_autotune || options.lab_rust_autotune {
        daemon.admission_requested = true;
        daemon.native_autotune = true;
    }
    if options.native_scheduler || options.lab_rust_scheduler {
        attest_no_competing_calibration_processes(
            Path::new(DEFAULT_PROC_ROOT),
            std::process::id(),
        )?;
        daemon.native_scheduler = Some(NativeSchedulerRuntime {
            store: SchedulerStore::open(
                options
                    .scheduler_store_dir
                    .as_deref()
                    .ok_or_else(|| "native scheduler store path disappeared".to_string())?,
            )?,
            quiet: BTreeMap::new(),
            errors: BTreeMap::new(),
            auto_apply_errors: BTreeMap::new(),
            auto_apply_warnings: BTreeMap::new(),
            waiting: BTreeMap::new(),
            accounting_blocks: BTreeMap::new(),
            status_cache: NativeSchedulerStatusCache::default(),
            lab_mode: options.lab_rust_scheduler,
        });
    }
    if options.native_rating
        || options.native_speedtest
        || options.native_autotune
        || options.lab_rust_rating
        || options.lab_rust_speedtest
        || options.lab_rust_autotune
    {
        daemon.resume_native_workers();
    }
    run_event_driven_calibrationd(&mut daemon, terminate)
}

fn run_event_driven_calibrationd(
    daemon: &mut CalibrationDaemon,
    terminate: &AtomicBool,
) -> Result<(), String> {
    let mut events = CalibrationEventLoop::new()?;
    install_child_signal_handler()?;

    // A control connection may already be queued immediately after bind.  It
    // must become durable before a due background slot is evaluated.
    let _ = daemon.drain_control_requests()?;
    daemon.tick();
    while !terminate.load(Ordering::Relaxed) {
        if daemon.refresh_event_watches(&mut events)? {
            daemon.tick();
            continue;
        }
        let readiness = events.wait(daemon.listener.as_raw_fd(), daemon.next_event_timeout()?)?;
        if terminate.load(Ordering::Relaxed) {
            break;
        }
        let control_effect = if readiness.control {
            daemon.drain_control_requests()?
        } else {
            ControlEffect::ReadOnly
        };
        // Inotify, SIGCHLD, an explicit control transition, or a bounded
        // calendar/safety deadline only asks the state machine to re-attest.
        // The readiness event itself is never authority.
        if coordinator_event_requires_tick(
            control_effect,
            readiness.filesystem,
            readiness.signal,
            readiness.process,
            readiness.deadline,
        ) {
            daemon.tick();
        }
    }
    Ok(())
}

fn coordinator_event_requires_tick(
    control_effect: ControlEffect,
    filesystem: bool,
    signal: bool,
    process: bool,
    deadline: bool,
) -> bool {
    control_effect.state_changed() || filesystem || signal || process || deadline
}

pub fn run_calibrationctl<I>(args: I) -> Result<(), String>
where
    I: Iterator<Item = String>,
{
    let mut args = args.peekable();
    let mut state_dir = state_dir_from_env();
    if args.peek().map(String::as_str) == Some("--state-dir") {
        let _ = args.next();
        state_dir = PathBuf::from(
            args.next()
                .ok_or_else(|| "--state-dir requires a path".to_string())?,
        );
    }
    let command_name = args
        .next()
        .ok_or_else(|| "calibrationctl requires a command".to_string())?;
    if command_name == "autotune-inspect" {
        let intent = parse_launch_intent(args)?;
        let operation = build_live_autotune_request(&intent)?;
        print!("{}", autotune_inspection_response(&operation));
        return Ok(());
    }
    if command_name == "autotune-start" {
        let intent = parse_launch_intent(args)?;
        let operation = build_live_autotune_request(&intent)?;
        print!(
            "{}",
            send_operation_control(&state_dir, ControlCommand::Start, operation)?
        );
        return Ok(());
    }
    if command_name == "autotune-bootstrap-start" {
        let planned_sqm_section = args.next().ok_or_else(|| {
            "calibrationctl autotune-bootstrap-start requires a planned SQM section".to_string()
        })?;
        let intent = parse_launch_intent(args)?;
        let operation = build_bootstrap_autotune_request(&intent, &planned_sqm_section)?;
        print!(
            "{}",
            send_operation_control(&state_dir, ControlCommand::Start, operation)?
        );
        return Ok(());
    }
    if command_name == "rating-start" {
        let intent = parse_rating_launch_intent(args)?;
        let operation = build_live_rating_request(&intent)?;
        print!(
            "{}",
            send_operation_control(&state_dir, ControlCommand::Start, operation)?
        );
        return Ok(());
    }
    if command_name == "speedtest-start" {
        let intent = parse_speedtest_launch_intent(args)?;
        let operation = build_live_speedtest_request(&intent)?;
        print!(
            "{}",
            send_operation_control(&state_dir, ControlCommand::Start, operation)?
        );
        return Ok(());
    }
    if command_name == "speedtest-bootstrap-start" {
        let planned_sqm_section = args.next().ok_or_else(|| {
            "calibrationctl speedtest-bootstrap-start requires a planned SQM section".to_string()
        })?;
        let intent = parse_speedtest_launch_intent(args)?;
        let operation = build_bootstrap_speedtest_request(&intent, &planned_sqm_section)?;
        print!(
            "{}",
            send_operation_control(&state_dir, ControlCommand::Start, operation)?
        );
        return Ok(());
    }
    if command_name == "rating-current" {
        let instance = args
            .next()
            .ok_or_else(|| "calibrationctl rating-current requires an instance".to_string())?;
        if args.next().is_some() {
            return Err("calibrationctl received unexpected arguments".to_string());
        }
        match find_current_rating_operation(&state_dir, &instance)? {
            Some(operation) => print!(
                "{}",
                send_operation_control(&state_dir, ControlCommand::Status, operation)?
            ),
            None => print!(
                "{{\"state\":\"idle\",\"instance\":\"{}\"}}\n",
                json_escape(&instance)
            ),
        }
        return Ok(());
    }
    if command_name == "speedtest-current" {
        let instance = args
            .next()
            .ok_or_else(|| "calibrationctl speedtest-current requires an instance".to_string())?;
        if args.next().is_some() {
            return Err("calibrationctl received unexpected arguments".to_string());
        }
        match find_current_speedtest_operation(&state_dir, &instance)? {
            Some(operation) => print!(
                "{}",
                send_operation_control(&state_dir, ControlCommand::Status, operation)?
            ),
            None => print!(
                "{{\"state\":\"idle\",\"instance\":\"{}\"}}\n",
                json_escape(&instance)
            ),
        }
        return Ok(());
    }
    if command_name == "autotune-current" {
        let instance = args
            .next()
            .ok_or_else(|| "calibrationctl autotune-current requires an instance".to_string())?;
        if args.next().is_some() {
            return Err("calibrationctl received unexpected arguments".to_string());
        }
        match find_current_autotune_operation(&state_dir, &instance)? {
            Some(operation) => print!(
                "{}",
                send_operation_control(&state_dir, ControlCommand::Status, operation)?
            ),
            None => print!(
                "{{\"state\":\"idle\",\"instance\":\"{}\"}}\n",
                json_escape(&instance)
            ),
        }
        return Ok(());
    }
    if command_name == "autotune-apply-check" {
        let job_id = args.next().ok_or_else(|| {
            "calibrationctl autotune-apply-check requires a public job ID".to_string()
        })?;
        let option_id = args.next();
        if args.next().is_some() {
            return Err("calibrationctl received unexpected arguments".to_string());
        }
        print!(
            "{}",
            native_autotune_apply_check(&state_dir, &job_id, option_id.as_deref())?
        );
        return Ok(());
    }
    if command_name == "autotune-apply-start" {
        let job_id = args.next().ok_or_else(|| {
            "calibrationctl autotune-apply-start requires a public job ID".to_string()
        })?;
        let option_id = args.next().ok_or_else(|| {
            "calibrationctl autotune-apply-start requires an option ID".to_string()
        })?;
        let expected_review = args.next().ok_or_else(|| {
            "calibrationctl autotune-apply-start requires the expected Review digest".to_string()
        })?;
        let expected_manifest = args.next().ok_or_else(|| {
            "calibrationctl autotune-apply-start requires the expected manifest digest".to_string()
        })?;
        let acknowledgements = parse_native_apply_acknowledgements(args)?;
        let request = NativeApplyControlRequest::start(
            kernel_request_id()?,
            job_id,
            option_id,
            expected_review,
            expected_manifest,
            acknowledgements,
        );
        print!("{}", send_native_apply_control(&state_dir, &request)?);
        return Ok(());
    }
    if command_name == "autotune-apply-watch" {
        let apply_job_id = args.next().ok_or_else(|| {
            "calibrationctl autotune-apply-watch requires an Apply job ID".to_string()
        })?;
        let apply_job_token = args.next().ok_or_else(|| {
            "calibrationctl autotune-apply-watch requires an Apply job token".to_string()
        })?;
        let observed_generation = args
            .next()
            .ok_or_else(|| {
                "calibrationctl autotune-apply-watch requires an observed generation".to_string()
            })?
            .parse::<u64>()
            .map_err(|_| "calibrationctl autotune-apply-watch generation is invalid".to_string())?;
        if args.next().is_some() {
            return Err("calibrationctl received unexpected arguments".to_string());
        }
        let request = NativeApplyControlRequest::watch(
            kernel_request_id()?,
            apply_job_id,
            apply_job_token,
            observed_generation,
        );
        print!("{}", send_native_apply_control(&state_dir, &request)?);
        return Ok(());
    }
    if command_name == "autotune-apply-status" || command_name == "autotune-apply-result" {
        let apply_job_id = args
            .next()
            .ok_or_else(|| format!("calibrationctl {command_name} requires an Apply job ID"))?;
        let apply_job_token = args
            .next()
            .ok_or_else(|| format!("calibrationctl {command_name} requires an Apply job token"))?;
        if args.next().is_some() {
            return Err("calibrationctl received unexpected arguments".to_string());
        }
        let command = if command_name == "autotune-apply-status" {
            NativeApplyControlCommand::Status
        } else {
            NativeApplyControlCommand::Result
        };
        let request = NativeApplyControlRequest::query(
            kernel_request_id()?,
            command,
            apply_job_id,
            apply_job_token,
        );
        print!("{}", send_native_apply_control(&state_dir, &request)?);
        return Ok(());
    }
    if command_name == "autotune-apply" {
        return Err(
            "synchronous native Apply is retired; use autotune-apply-start/status/result"
                .to_string(),
        );
    }
    if command_name == "autotune-apply-lab-rollback" {
        let fault = require_native_apply_lab_rollback_mode()?;
        let job_id = args.next().ok_or_else(|| {
            "calibrationctl autotune-apply-lab-rollback requires a public job ID".to_string()
        })?;
        let option_id = args.next().ok_or_else(|| {
            "calibrationctl autotune-apply-lab-rollback requires an option ID".to_string()
        })?;
        let expected_manifest = args.next().ok_or_else(|| {
            "calibrationctl autotune-apply-lab-rollback requires the expected manifest digest"
                .to_string()
        })?;
        if args.next().is_some() {
            return Err("calibrationctl received unexpected arguments".to_string());
        }
        print!(
            "{}",
            native_autotune_apply_lab_rollback(
                &state_dir,
                &job_id,
                &option_id,
                &expected_manifest,
                fault,
            )?
        );
        return Ok(());
    }
    if command_name == "autotune-apply-lab-commit" {
        let fault = require_native_apply_lab_commit_mode()?;
        let job_id = args.next().ok_or_else(|| {
            "calibrationctl autotune-apply-lab-commit requires a public job ID".to_string()
        })?;
        let option_id = args.next().ok_or_else(|| {
            "calibrationctl autotune-apply-lab-commit requires an option ID".to_string()
        })?;
        let expected_manifest = args.next().ok_or_else(|| {
            "calibrationctl autotune-apply-lab-commit requires the expected manifest digest"
                .to_string()
        })?;
        if args.next().is_some() {
            return Err("calibrationctl received unexpected arguments".to_string());
        }
        print!(
            "{}",
            native_autotune_apply_lab_commit(
                &state_dir,
                &job_id,
                &option_id,
                &expected_manifest,
                fault,
            )?
        );
        return Ok(());
    }
    if command_name == "autotune-apply-lab-recover" {
        require_native_apply_lab_any_mode()?;
        if args.next().is_some() {
            return Err("calibrationctl received unexpected arguments".to_string());
        }
        print!("{}", native_autotune_apply_lab_recover()?);
        return Ok(());
    }
    if command_name == "scheduler-acknowledge-accounting" {
        let instance = args.next().ok_or_else(|| {
            "calibrationctl scheduler-acknowledge-accounting requires an instance".to_string()
        })?;
        if args.next().is_some() {
            return Err("calibrationctl received unexpected arguments".to_string());
        }
        print!(
            "{}",
            send_scheduler_accounting_acknowledgement(&state_dir, &instance)?
        );
        return Ok(());
    }
    if command_name == "scheduler-status" {
        if args.next().is_some() {
            return Err("calibrationctl received unexpected arguments".to_string());
        }
        print!("{}", send_scheduler_status(&state_dir)?);
        return Ok(());
    }
    if matches!(
        command_name.as_str(),
        "autotune-status" | "autotune-result" | "autotune-cancel"
    ) {
        let job_id = args
            .next()
            .ok_or_else(|| format!("calibrationctl {command_name} requires a public job ID"))?;
        if args.next().is_some() {
            return Err("calibrationctl received unexpected arguments".to_string());
        }
        let operation = read_private_job_operation(&state_dir, &job_id)?;
        if operation.identity.operation != OperationKind::FullAutotune {
            return Err("public Auto-Tune control ID belongs to another operation".to_string());
        }
        let command = match command_name.as_str() {
            "autotune-status" => ControlCommand::Status,
            "autotune-result" => ControlCommand::Result,
            "autotune-cancel" => ControlCommand::Cancel,
            _ => unreachable!(),
        };
        print!(
            "{}",
            send_operation_control(&state_dir, command, operation)?
        );
        return Ok(());
    }
    if matches!(
        command_name.as_str(),
        "rating-status" | "rating-result" | "rating-cancel"
    ) {
        let job_id = args
            .next()
            .ok_or_else(|| format!("calibrationctl {command_name} requires a public job ID"))?;
        if args.next().is_some() {
            return Err("calibrationctl received unexpected arguments".to_string());
        }
        let operation = read_private_job_operation(&state_dir, &job_id)?;
        if !matches!(
            operation.identity.operation,
            OperationKind::AutomaticRating | OperationKind::GuidedRating
        ) {
            return Err("public Rating control ID belongs to another operation".to_string());
        }
        let command = match command_name.as_str() {
            "rating-status" => ControlCommand::Status,
            "rating-result" => ControlCommand::Result,
            "rating-cancel" => ControlCommand::Cancel,
            _ => unreachable!(),
        };
        print!(
            "{}",
            send_operation_control(&state_dir, command, operation)?
        );
        return Ok(());
    }
    if matches!(
        command_name.as_str(),
        "speedtest-status" | "speedtest-result" | "speedtest-cancel"
    ) {
        let job_id = args
            .next()
            .ok_or_else(|| format!("calibrationctl {command_name} requires a public job ID"))?;
        if args.next().is_some() {
            return Err("calibrationctl received unexpected arguments".to_string());
        }
        let operation = read_private_job_operation(&state_dir, &job_id)?;
        if operation.identity.operation != OperationKind::Speedtest {
            return Err("public Speed Test control ID belongs to another operation".to_string());
        }
        let command = match command_name.as_str() {
            "speedtest-status" => ControlCommand::Status,
            "speedtest-result" => ControlCommand::Result,
            "speedtest-cancel" => ControlCommand::Cancel,
            _ => unreachable!(),
        };
        print!(
            "{}",
            send_operation_control(&state_dir, command, operation)?
        );
        return Ok(());
    }
    let (command, operation) = match command_name.as_str() {
        "ping" => (ControlCommand::Ping, None),
        "summary" => (ControlCommand::Summary, None),
        "start" | "status" | "result" | "cancel" => {
            let path = PathBuf::from(args.next().ok_or_else(|| {
                format!("calibrationctl {command_name} requires a secure request file")
            })?);
            let operation = read_operation_request(&path)?;
            let command = match command_name.as_str() {
                "start" => ControlCommand::Start,
                "status" => ControlCommand::Status,
                "result" => ControlCommand::Result,
                "cancel" => ControlCommand::Cancel,
                _ => unreachable!(),
            };
            (command, Some(operation))
        }
        value => return Err(format!("unsupported calibrationctl command: {value}")),
    };
    if args.next().is_some() {
        return Err("calibrationctl received unexpected arguments".to_string());
    }
    match operation {
        Some(operation) => print!(
            "{}",
            send_operation_control(&state_dir, command, operation)?
        ),
        None => {
            let request = ControlMessage {
                control: ControlRequest {
                    request_id: kernel_request_id()?,
                    command,
                    job_id: None,
                    job_token: None,
                },
                operation: None,
            };
            print!("{}", send_control(&state_dir, &request)?);
        }
    }
    Ok(())
}

pub fn run_native_apply_recovery<I>(mut args: I) -> Result<(), String>
where
    I: Iterator<Item = String>,
{
    if args.next().is_some() {
        return Err("native Apply recovery received unexpected arguments".to_string());
    }
    print!("{}", native_autotune_apply_recovery()?);
    Ok(())
}

pub fn run_native_apply_worker<I>(mut args: I) -> Result<(), String>
where
    I: Iterator<Item = String>,
{
    if euid() != 0 {
        return Err("native Apply worker requires root".to_string());
    }
    if args.next().as_deref() != Some("--state-dir") {
        return Err("native Apply worker requires --state-dir".to_string());
    }
    let state_dir = PathBuf::from(
        args.next()
            .ok_or_else(|| "native Apply worker requires a state path".to_string())?,
    );
    if args.next().as_deref() != Some("--apply-job-id") {
        return Err("native Apply worker requires --apply-job-id".to_string());
    }
    let apply_job_id = args
        .next()
        .ok_or_else(|| "native Apply worker requires an Apply job ID".to_string())?;
    if args.next().is_some() {
        return Err("native Apply worker received trailing arguments".to_string());
    }
    validate_state_path(&state_dir)?;
    if apply_job_id.len() != 32
        || !apply_job_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("native Apply worker job ID is invalid".to_string());
    }
    let store = NativeApplyCoordinatorStore::new(default_native_apply_paths().recovery_root);
    let active = store
        .read_active()?
        .ok_or_else(|| "native Apply worker has no durable dispatch".to_string())?;
    if active.apply_job_id != apply_job_id
        || !matches!(
            active.state,
            NativeApplyDispatchState::Validating | NativeApplyDispatchState::Applying
        )
    {
        return Err("native Apply worker dispatch identity is not executable".to_string());
    }
    let claim = NativeApplyWorkerClaim::for_process(&active, ProcessIdentity::current()?)?;
    if let Some(existing) = store.claim_worker(&active, &claim)? {
        if existing == claim {
            // An exec-retry of the exact process may continue.
        } else if existing
            .process
            .still_matches(Path::new(DEFAULT_PROC_ROOT))?
        {
            return Err("native Apply dispatch is already owned by a live worker".to_string());
        } else {
            store.remove_worker_claim(&existing)?;
            if store.claim_worker(&active, &claim)?.is_some() {
                return Err("native Apply worker claim raced with another owner".to_string());
            }
        }
    }

    let result = execute_native_apply_worker(&state_dir, &store, active);
    let cleanup = store.remove_worker_claim(&claim);
    match (result, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn send_operation_control(
    state_dir: &Path,
    command: ControlCommand,
    operation: OperationRequest,
) -> Result<String, String> {
    let request = ControlMessage {
        control: ControlRequest {
            request_id: kernel_request_id()?,
            command,
            job_id: Some(operation.identity.job_id.clone()),
            job_token: Some(operation.identity.job_token.clone()),
        },
        operation: (command == ControlCommand::Start).then_some(operation),
    };
    send_control(state_dir, &request)
}

fn send_scheduler_accounting_acknowledgement(
    state_dir: &Path,
    instance: &str,
) -> Result<String, String> {
    let request = SchedulerAccountingAcknowledgement {
        request_id: kernel_request_id()?,
        instance: instance.to_string(),
    };
    send_encoded_control(state_dir, &request.encode()?, MAX_RESPONSE_BYTES)
}

fn send_scheduler_status(state_dir: &Path) -> Result<String, String> {
    let request = SchedulerStatusRequest {
        request_id: kernel_request_id()?,
    };
    send_encoded_control(
        state_dir,
        &request.encode()?,
        MAX_SCHEDULER_STATUS_RESPONSE_BYTES,
    )
}

fn read_private_job_operation(state_dir: &Path, job_id: &str) -> Result<OperationRequest, String> {
    if !public_job_id_valid(job_id) {
        return Err("public calibration job ID is invalid".to_string());
    }
    for (path, label) in [
        (state_dir.to_path_buf(), "state"),
        (state_dir.join("jobs"), "job root"),
        (state_dir.join("jobs").join(job_id), "job"),
    ] {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("unable to inspect calibration {label} directory: {error}"))?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != euid()
            || metadata.mode() & 0o077 != 0
        {
            return Err(format!(
                "calibration {label} directory must be private, root-owned, and non-symlinked"
            ));
        }
    }
    let request = read_operation_request(&state_dir.join("jobs").join(job_id).join("request"))?;
    if request.identity.job_id != job_id {
        return Err("public calibration job ID does not match its private request".to_string());
    }
    Ok(request)
}

fn public_job_id_valid(job_id: &str) -> bool {
    job_id.len() == 32
        && job_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn read_private_job_journal(state_dir: &Path, job_id: &str) -> Result<JobJournal, String> {
    let _ = read_private_job_operation(state_dir, job_id)?;
    let state_path = state_dir.join("jobs").join(job_id).join("state");
    let journal = JobJournal::decode(&rating::read_private_bounded(
        &state_path,
        MAX_OPERATION_RECORD_BYTES,
    )?)?;
    if journal.job_id != job_id {
        return Err("public calibration job ID does not match its private journal".to_string());
    }
    Ok(journal)
}

fn find_current_rating_operation(
    state_dir: &Path,
    instance: &str,
) -> Result<Option<OperationRequest>, String> {
    let _ = Config::from_uci(instance)?;
    scan_current_rating_operation(state_dir, instance)
}

fn scan_current_rating_operation(
    state_dir: &Path,
    instance: &str,
) -> Result<Option<OperationRequest>, String> {
    let jobs_root = state_dir.join("jobs");
    let entries = match fs::read_dir(&jobs_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "unable to list private calibration jobs for Rating: {error}"
            ))
        }
    };
    let mut selected: Option<(u64, OperationRequest)> = None;
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let Ok(job_id) = entry.file_name().into_string() else {
            continue;
        };
        if !public_job_id_valid(&job_id) {
            continue;
        }
        let Ok(operation) = read_private_job_operation(state_dir, &job_id) else {
            continue;
        };
        if operation.identity.instance != instance
            || !matches!(
                operation.identity.operation,
                OperationKind::AutomaticRating | OperationKind::GuidedRating
            )
        {
            continue;
        }
        let Ok(journal) = read_private_job_journal(state_dir, &job_id) else {
            continue;
        };
        if state::terminal(journal.state) {
            continue;
        }
        let replace = selected
            .as_ref()
            .is_none_or(|(created, _)| operation.created_unix_ms > *created);
        if replace {
            selected = Some((operation.created_unix_ms, operation));
        }
    }
    Ok(selected.map(|(_, operation)| operation))
}

fn find_current_speedtest_operation(
    state_dir: &Path,
    instance: &str,
) -> Result<Option<OperationRequest>, String> {
    let _ = Config::from_uci(instance)?;
    scan_current_speedtest_operation(state_dir, instance)
}

fn scan_current_speedtest_operation(
    state_dir: &Path,
    instance: &str,
) -> Result<Option<OperationRequest>, String> {
    let jobs_root = state_dir.join("jobs");
    let entries = match fs::read_dir(&jobs_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "unable to list private calibration jobs for Speed Test: {error}"
            ))
        }
    };
    let mut selected: Option<(u64, OperationRequest)> = None;
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let Ok(job_id) = entry.file_name().into_string() else {
            continue;
        };
        if !public_job_id_valid(&job_id) {
            continue;
        }
        let Ok(operation) = read_private_job_operation(state_dir, &job_id) else {
            continue;
        };
        if operation.identity.instance != instance
            || operation.identity.operation != OperationKind::Speedtest
        {
            continue;
        }
        let Ok(journal) = read_private_job_journal(state_dir, &job_id) else {
            continue;
        };
        if state::terminal(journal.state) {
            continue;
        }
        let replace = selected
            .as_ref()
            .is_none_or(|(created, _)| operation.created_unix_ms > *created);
        if replace {
            selected = Some((operation.created_unix_ms, operation));
        }
    }
    Ok(selected.map(|(_, operation)| operation))
}

fn find_current_autotune_operation(
    state_dir: &Path,
    instance: &str,
) -> Result<Option<OperationRequest>, String> {
    let _ = Config::from_uci(instance)?;
    scan_current_autotune_operation(state_dir, instance)
}

fn scan_current_autotune_operation(
    state_dir: &Path,
    instance: &str,
) -> Result<Option<OperationRequest>, String> {
    scan_current_autotune_operation_with_review_validator(
        state_dir,
        instance,
        |operation, journal| canonical_current_autotune_review(state_dir, operation, journal),
    )
}

fn canonical_current_autotune_review(
    state_dir: &Path,
    operation: &OperationRequest,
    journal: &JobJournal,
) -> bool {
    verified_native_autotune_public_result(state_dir, operation, journal).is_ok()
}

fn verified_native_autotune_public_result(
    state_dir: &Path,
    operation: &OperationRequest,
    journal: &JobJournal,
) -> Result<String, String> {
    if operation.identity.operation != OperationKind::FullAutotune
        || operation.identity.job_id != journal.job_id
        || !matches!(
            journal.state,
            super::protocol::OperationState::ReviewReady
                | super::protocol::OperationState::Completed
        )
        || journal.runtime_mutated
        || journal.recovery_required
        || journal.process.is_some()
        || journal.runtime_owner_process.is_some()
        || journal.terminal_kind.as_deref() != Some("result")
        || journal.terminal_state.as_deref() != Some("complete")
    {
        return Err("native Full Auto-Tune result is not an inert completed Review".to_string());
    }
    let worker_run_id = journal
        .worker_run_id
        .as_deref()
        .ok_or_else(|| "native Full Auto-Tune Review has no worker identity".to_string())?;
    if !public_job_id_valid(worker_run_id) {
        return Err("native Full Auto-Tune Review worker identity is invalid".to_string());
    }
    let job_dir = state_dir.join("jobs").join(&journal.job_id);
    let request_path = job_dir.join("request");
    let terminal_path = job_dir.join(format!("terminal-{worker_run_id}"));
    let review_path = job_dir.join(format!("review-{worker_run_id}.json"));
    let apply_manifest_path = job_dir.join(format!("apply-manifest-{worker_run_id}.json"));
    let public_result_path = job_dir.join(format!("public-review-{worker_run_id}.json"));
    let terminal = full_autotune::read_terminal_file(&terminal_path)?;
    if terminal.job_id != operation.identity.job_id || terminal.worker_run_id != worker_run_id {
        return Err("native Full Auto-Tune terminal identity changed".to_string());
    }
    let AutotuneTerminal::Complete { review_digest } = terminal.terminal else {
        return Err("native Full Auto-Tune terminal has no complete Review".to_string());
    };
    let (publication_boot_id, publication_generation) = journal.publication_identity()?;
    let canonical = full_autotune::canonical_native_public_result_transaction(
        &request_path,
        &review_path,
        &apply_manifest_path,
        &operation.identity.job_id,
        worker_run_id,
        &review_digest,
        publication_boot_id,
        publication_generation,
    )?;
    let stored = rating::read_private_bounded(&public_result_path, MAX_RESULT_RESPONSE_BYTES)?;
    if stored.as_bytes() != canonical.as_slice() {
        return Err(
            "published native Full Auto-Tune result differs from canonical evidence".to_string(),
        );
    }
    Ok(stored)
}

fn scan_current_autotune_operation_with_review_validator<F>(
    state_dir: &Path,
    instance: &str,
    mut review_valid: F,
) -> Result<Option<OperationRequest>, String>
where
    F: FnMut(&OperationRequest, &JobJournal) -> bool,
{
    let jobs_root = state_dir.join("jobs");
    let entries = match fs::read_dir(&jobs_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "unable to list private calibration jobs for Auto-Tune: {error}"
            ))
        }
    };
    // An in-flight operation always wins over an older Review.  Collect
    // structurally eligible Reviews separately so their potentially expensive
    // evidence replay is never performed while active work exists.
    let mut active: Option<(u64, String, OperationRequest)> = None;
    let mut reviews = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let Ok(job_id) = entry.file_name().into_string() else {
            continue;
        };
        if !public_job_id_valid(&job_id) {
            continue;
        }
        let Ok(operation) = read_private_job_operation(state_dir, &job_id) else {
            continue;
        };
        if operation.identity.instance != instance
            || operation.identity.operation != OperationKind::FullAutotune
        {
            continue;
        }
        let Ok(journal) = read_private_job_journal(state_dir, &job_id) else {
            continue;
        };
        if !state::terminal(journal.state) {
            let candidate = (
                operation.created_unix_ms,
                operation.identity.job_id.clone(),
                operation,
            );
            if active.as_ref().is_none_or(|selected| {
                candidate.0 > selected.0 || candidate.0 == selected.0 && candidate.1 > selected.1
            }) {
                active = Some(candidate);
            }
        } else if journal.state == super::protocol::OperationState::ReviewReady
            && !journal.runtime_mutated
            && !journal.recovery_required
            && journal.process.is_none()
            && journal.runtime_owner_process.is_none()
            && journal.terminal_kind.as_deref() == Some("result")
            && journal.terminal_state.as_deref() == Some("complete")
        {
            reviews.push((
                operation.created_unix_ms,
                operation.identity.job_id.clone(),
                operation,
                journal,
            ));
        }
    }
    if let Some((_, _, operation)) = active {
        return Ok(Some(operation));
    }
    reviews.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| right.1.cmp(&left.1)));
    Ok(reviews.into_iter().find_map(|(_, _, operation, journal)| {
        review_valid(&operation, &journal).then_some(operation)
    }))
}

struct VerifiedNativeApplyContext {
    request: OperationRequest,
    worker_run_id: String,
    bootstrap_runtime_dir: PathBuf,
    option_id: String,
    review_digest: String,
    manifest_digest: String,
    manifest: Vec<u8>,
    plan: NativeApplyExecutionPlan,
}

#[derive(Debug)]
struct VerifiedBootstrapApplyContext {
    plan: NativeBootstrapApplyPlan,
    manifest: Vec<u8>,
    manifest_digest: String,
}

struct VerifiedNativeApplyAuthority {
    source: VerifiedNativeApplyContext,
    bootstrap: Option<VerifiedBootstrapApplyContext>,
}

impl VerifiedNativeApplyAuthority {
    fn effective_manifest_digest(&self) -> &str {
        self.bootstrap
            .as_ref()
            .map(|value| value.manifest_digest.as_str())
            .unwrap_or(self.source.manifest_digest.as_str())
    }

    fn effective_manifest_schema_version(&self) -> u8 {
        match self.bootstrap.as_ref() {
            Some(value) => value.plan.manifest_schema_version(),
            None => self.source.plan.manifest_schema_version(),
        }
    }

    fn target_state_name(&self) -> &'static str {
        match self.source.request.target_state {
            OperationTargetState::ExistingManaged => "existing_managed",
            OperationTargetState::AbsentBootstrap => "absent_bootstrap",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeScheduledAutoApplyOutcome {
    NotRequested,
    ReviewRequired,
    Applied(NativeApplyCommitDisposition),
}

type NativeScheduledAutoApplyExecutor =
    fn(&Path, &str) -> Result<NativeScheduledAutoApplyOutcome, String>;

fn confirm_native_apply_controllers_after_unlock() -> Result<(), String> {
    confirm_native_apply_controllers_after_unlock_with(confirm_controller_service_started)
}

fn confirm_native_apply_controllers_after_unlock_with<F>(confirm: F) -> Result<(), String>
where
    F: FnOnce() -> Result<(), String>,
{
    confirm().map_err(|error| {
        format!(
            "native Apply controller readiness failed after releasing the runtime lock: {error}"
        )
    })
}

fn verified_native_autotune_apply_context(
    state_dir: &Path,
    job_id: &str,
    requested_option_id: Option<&str>,
) -> Result<VerifiedNativeApplyContext, String> {
    let request = read_private_job_operation(state_dir, job_id)?;
    if request.identity.operation != OperationKind::FullAutotune {
        return Err("public Auto-Tune Apply-check ID belongs to another operation".to_string());
    }
    let journal = read_private_job_journal(state_dir, job_id)?;
    if !matches!(
        journal.state,
        super::protocol::OperationState::ReviewReady | super::protocol::OperationState::Completed
    ) || journal.runtime_mutated
        || journal.recovery_required
        || journal.process.is_some()
        || journal.runtime_owner_process.is_some()
        || journal.terminal_kind.as_deref() != Some("result")
        || journal.terminal_state.as_deref() != Some("complete")
    {
        return Err(
            "native Apply check requires a settled Review with no runtime recovery".to_string(),
        );
    }
    let worker_run_id = journal
        .worker_run_id
        .as_deref()
        .ok_or_else(|| "native Apply check has no worker identity".to_string())?;
    let job_directory = state_dir.join("jobs").join(job_id);
    let paths = super::journal::NativeJobPaths {
        request: job_directory.join("request"),
        terminal: job_directory.join(format!("terminal-{worker_run_id}")),
        review: job_directory.join(format!("review-{worker_run_id}.json")),
        apply_manifest: requested_option_id.map_or_else(
            || job_directory.join(format!("apply-manifest-{worker_run_id}.json")),
            |option_id| {
                job_directory.join(format!("apply-manifest-{worker_run_id}-{option_id}.json"))
            },
        ),
        public_result: job_directory.join(format!("public-review-{worker_run_id}.json")),
        permit: job_directory.join(format!("permit-{worker_run_id}")),
        stdout: job_directory.join(format!("stdout-{worker_run_id}.log")),
        stderr: job_directory.join(format!("stderr-{worker_run_id}.log")),
        bootstrap_runtime_dir: job_directory.join(format!("bootstrap-runtime-{worker_run_id}")),
        bootstrap_runtime_stdout: job_directory
            .join(format!("bootstrap-runtime-stdout-{worker_run_id}.log")),
        bootstrap_runtime_stderr: job_directory
            .join(format!("bootstrap-runtime-stderr-{worker_run_id}.log")),
    };
    let terminal = full_autotune::read_terminal_file(&paths.terminal)?;
    if terminal.job_id != job_id || terminal.worker_run_id != worker_run_id {
        return Err("native Apply terminal identity mismatch".to_string());
    }
    let review_digest = match terminal.terminal {
        AutotuneTerminal::Complete { review_digest } => review_digest,
        _ => return Err("native Apply terminal has no complete Review".to_string()),
    };
    let (publication_boot_id, publication_generation) = journal.publication_identity()?;
    let (plan, manifest, manifest_digest) = match requested_option_id {
        Some(option_id) => {
            full_autotune::verified_native_apply_execution_plan_for_option_transaction(
                &paths.request,
                &paths.review,
                &paths.apply_manifest,
                job_id,
                worker_run_id,
                &review_digest,
                publication_boot_id,
                publication_generation,
                option_id,
            )?
        }
        None => full_autotune::verified_native_apply_execution_plan_transaction(
            &paths.request,
            &paths.review,
            &paths.apply_manifest,
            job_id,
            worker_run_id,
            &review_digest,
            publication_boot_id,
            publication_generation,
        )?,
    };
    if plan.request != request {
        return Err("native Apply verified plan changed its private request".to_string());
    }
    Ok(VerifiedNativeApplyContext {
        request,
        worker_run_id: worker_run_id.to_string(),
        bootstrap_runtime_dir: paths.bootstrap_runtime_dir,
        option_id: plan.option_id.clone(),
        review_digest,
        manifest_digest,
        manifest,
        plan,
    })
}

fn verified_native_autotune_apply_authority(
    state_dir: &Path,
    job_id: &str,
    requested_option_id: Option<&str>,
) -> Result<VerifiedNativeApplyAuthority, String> {
    let source = verified_native_autotune_apply_context(state_dir, job_id, requested_option_id)?;
    let bootstrap = if source.request.target_state == OperationTargetState::AbsentBootstrap {
        let claim = read_bootstrap_runtime_owner_claim(
            &source.bootstrap_runtime_dir,
            &source.request,
            &source.worker_run_id,
        )?
        .ok_or_else(|| {
            "native bootstrap Apply has no exact runtime-owner absence witness".to_string()
        })?;
        Some(verified_bootstrap_apply_context(
            &source,
            claim.baseline().clone(),
        )?)
    } else {
        None
    };
    Ok(VerifiedNativeApplyAuthority { source, bootstrap })
}

fn verified_bootstrap_apply_context(
    source: &VerifiedNativeApplyContext,
    baseline: AbsentRuntimeBaseline,
) -> Result<VerifiedBootstrapApplyContext, String> {
    if source.request.target_state != OperationTargetState::AbsentBootstrap {
        return Err("native bootstrap Apply source is not an absent target".to_string());
    }
    let policy = NativeBootstrapApplyPolicy::defaults_v1(&source.request)?;
    let plan =
        NativeBootstrapApplyPlan::from_verified_source(source.plan.clone(), policy, baseline)?;
    if plan.source_manifest_sha256() != source.manifest_digest {
        return Err("native bootstrap Apply source manifest changed after Review".to_string());
    }
    let manifest = plan.canonical_manifest_bytes()?;
    let manifest_digest = super::sqm_identity::sha256sum(&manifest)?;
    Ok(VerifiedBootstrapApplyContext {
        plan,
        manifest,
        manifest_digest,
    })
}

fn execute_native_scheduled_auto_apply(
    state_dir: &Path,
    job_id: &str,
) -> Result<NativeScheduledAutoApplyOutcome, String> {
    let request = read_private_job_operation(state_dir, job_id)?;
    if !request.scheduled_auto_apply_requested {
        return Ok(NativeScheduledAutoApplyOutcome::NotRequested);
    }
    if request.origin != OperationOrigin::Scheduler
        || request.identity.operation != OperationKind::FullAutotune
    {
        return Err("scheduled Auto-Apply request authority is invalid".to_string());
    }
    let context = verified_native_autotune_apply_context(state_dir, job_id, None)?;
    if !context.plan.unattended_scheduler_eligible() {
        return Ok(NativeScheduledAutoApplyOutcome::ReviewRequired);
    }
    if euid() != 0 {
        return Err("scheduled native Auto-Apply requires root".to_string());
    }
    let mut backend = OpenWrtNativeApplyBackend::new();
    let receipt = execute_native_apply_commit(
        &context.plan,
        &context.manifest,
        default_native_apply_paths(),
        &mut backend,
    )?;
    confirm_native_apply_controllers_after_unlock()?;
    Ok(NativeScheduledAutoApplyOutcome::Applied(
        receipt.disposition,
    ))
}

pub(crate) fn native_apply_recovery_markers_present() -> Result<(bool, bool), String> {
    let current = default_native_apply_paths().recovery_root.join("current");
    let existing = match fs::symlink_metadata(&current) {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => Err(format!(
            "unable to inspect native Apply recovery marker: {error}"
        ))?,
    };
    let bootstrap =
        NativeBootstrapApplyRecoveryStore::new(default_native_apply_paths().recovery_root)
            .read_record()?
            .is_some();
    if existing && bootstrap {
        return Err(
            "existing and bootstrap native Apply recovery authorities are both pending".to_string(),
        );
    }
    Ok((existing, bootstrap))
}

pub(crate) fn attest_native_apply_idle_for_service_stop() -> Result<(), String> {
    let store = NativeApplyCoordinatorStore::new(default_native_apply_paths().recovery_root);
    // A recovery marker without an active dispatch/worker must not deadlock an
    // upgrade: the package lifecycle stops this service before invoking the
    // dedicated native recovery command. Active ownership remains fail-closed.
    attest_native_apply_store_idle_for_service_stop(&store)
}

fn attest_native_apply_store_idle_for_service_stop(
    store: &NativeApplyCoordinatorStore,
) -> Result<(), String> {
    if let Some(active) = store.read_active()? {
        return Err(format!(
            "calibration service stop is deferred while native Apply is {} at generation {}",
            active.state.as_str(),
            active.generation
        ));
    }
    if store.read_worker_claim()?.is_some() {
        return Err(
            "calibration service stop is deferred until the native Apply worker claim is settled"
                .to_string(),
        );
    }
    Ok(())
}

fn native_apply_recovery_marker_present() -> Result<bool, String> {
    let (existing, bootstrap) = native_apply_recovery_markers_present()?;
    Ok(existing || bootstrap)
}

fn existing_native_apply_check_result<F>(
    candidate_already_applied: bool,
    attest_runtime: F,
) -> Result<bool, String>
where
    F: FnOnce() -> RuntimeAttestation,
{
    if candidate_already_applied {
        return Ok(true);
    }
    match attest_runtime() {
        RuntimeAttestation::Ready => Ok(false),
        RuntimeAttestation::Waiting { code, message }
        | RuntimeAttestation::Unsafe { code, message } => Err(format!(
            "native Apply live attestation failed ({code}): {message}"
        )),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeApplyLiveState {
    Candidate,
    Original,
}

type NativeApplyLiveStateAttestor =
    fn(&Path, &str, Option<&str>) -> Result<NativeApplyLiveState, String>;

fn attest_verified_native_apply_live_state(
    authority: &VerifiedNativeApplyAuthority,
) -> Result<NativeApplyLiveState, String> {
    if native_apply_recovery_marker_present()? {
        return Err("native Apply recovery remains pending".to_string());
    }
    if let Some(bootstrap) = authority.bootstrap.as_ref() {
        let mut backend = OpenWrtNativeApplyBackend::new();
        let already =
            <OpenWrtNativeApplyBackend as NativeBootstrapApplyBackend>::candidate_already_applied(
                &mut backend,
                &bootstrap.plan,
            )?;
        if already {
            return Ok(NativeApplyLiveState::Candidate);
        }
        <OpenWrtNativeApplyBackend as NativeBootstrapApplyBackend>::attest_absent_before_mutation(
            &mut backend,
            &bootstrap.plan,
        )?;
        return Ok(NativeApplyLiveState::Original);
    }

    let mut backend = OpenWrtNativeApplyBackend::new();
    let candidate_already_applied = NativeApplyTransactionBackend::candidate_already_applied(
        &mut backend,
        &authority.source.plan,
    )?;
    existing_native_apply_check_result(candidate_already_applied, || {
        attest_openwrt_runtime(&authority.source.request)
    })
    .map(|already| {
        if already {
            NativeApplyLiveState::Candidate
        } else {
            NativeApplyLiveState::Original
        }
    })
}

fn native_autotune_apply_live_state(
    state_dir: &Path,
    job_id: &str,
    option_id: Option<&str>,
) -> Result<NativeApplyLiveState, String> {
    let authority = verified_native_autotune_apply_authority(state_dir, job_id, option_id)?;
    attest_verified_native_apply_live_state(&authority)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeApplyTerminalRetry {
    Reuse,
    Rearm,
}

fn native_apply_terminal_retry(
    terminal: &NativeApplyTerminalRecord,
    live: NativeApplyLiveState,
) -> Result<NativeApplyTerminalRetry, String> {
    terminal.validate()?;
    if terminal.outcome.success() && live == NativeApplyLiveState::Candidate {
        Ok(NativeApplyTerminalRetry::Reuse)
    } else {
        Ok(NativeApplyTerminalRetry::Rearm)
    }
}

fn native_autotune_apply_check(
    state_dir: &Path,
    job_id: &str,
    option_id: Option<&str>,
) -> Result<String, String> {
    let authority = verified_native_autotune_apply_authority(state_dir, job_id, option_id)?;
    let already_applied =
        attest_verified_native_apply_live_state(&authority)? == NativeApplyLiveState::Candidate;
    Ok(format!(
        concat!(
            "{{\"state\":\"confirmation_ready\",\"apply_enabled\":true,",
            "\"validation_only\":false,\"job_id\":\"{}\",",
            "\"worker_run_id\":\"{}\",\"option_id\":\"{}\",",
            "\"review_sha256\":\"{}\",",
            "\"source_manifest_sha256\":\"{}\",",
            "\"manifest_sha256\":\"{}\",\"manifest_schema_version\":{},",
            "\"target_state\":\"{}\",\"already_applied\":{},",
            "\"runtime_attested\":true,",
            "\"required_acknowledgements\":{}}}\n"
        ),
        job_id,
        authority.source.worker_run_id,
        authority.source.option_id,
        authority.source.review_digest,
        authority.source.manifest_digest,
        authority.effective_manifest_digest(),
        authority.effective_manifest_schema_version(),
        authority.target_state_name(),
        bool_json(already_applied),
        native_apply_acknowledgements_json(&authority.source.plan.required_acknowledgements),
    ))
}

fn parse_native_apply_acknowledgements<I>(
    mut args: I,
) -> Result<Vec<NativeApplyAcknowledgement>, String>
where
    I: Iterator<Item = String>,
{
    let mut values = BTreeSet::new();
    while let Some(flag) = args.next() {
        if flag != "--ack" {
            return Err(
                "calibrationctl autotune-apply-start received an unexpected argument".to_string(),
            );
        }
        let code = args.next().ok_or_else(|| {
            "calibrationctl autotune-apply-start --ack requires a code".to_string()
        })?;
        let acknowledgement =
            NativeApplyAcknowledgement::from_public_code(&code).ok_or_else(|| {
                "calibrationctl autotune-apply-start acknowledgement code is unknown".to_string()
            })?;
        if !values.insert(acknowledgement) {
            return Err(
                "calibrationctl autotune-apply-start acknowledgement code is duplicated"
                    .to_string(),
            );
        }
        if values.len() > MAX_NATIVE_APPLY_ACKNOWLEDGEMENTS {
            return Err(
                "calibrationctl autotune-apply-start acknowledgement set exceeds its bound"
                    .to_string(),
            );
        }
    }
    Ok(values.into_iter().collect())
}

fn native_apply_acknowledgements_json(values: &[NativeApplyAcknowledgement]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| format!("\"{}\"", value.as_str()))
            .collect::<Vec<_>>()
            .join(",")
    )
}

fn require_exact_native_apply_acknowledgements(
    expected: &[NativeApplyAcknowledgement],
    acknowledged: &[NativeApplyAcknowledgement],
) -> Result<(), String> {
    if expected == acknowledged {
        Ok(())
    } else {
        Err("native Apply requires the exact complete acknowledgement set".to_string())
    }
}

fn bounded_native_apply_diagnostic(error: &str) -> String {
    error
        .bytes()
        .map(|value| {
            if value == b' ' || value.is_ascii_graphic() {
                value as char
            } else {
                ' '
            }
        })
        .take(480)
        .collect::<String>()
        .trim()
        .to_string()
}

fn verified_native_apply_dispatch_identity(
    state_dir: &Path,
    dispatch: &NativeApplyDispatchRecord,
) -> Result<NativeApplyVerifiedDispatchIdentity, String> {
    if !matches!(
        dispatch.state,
        NativeApplyDispatchState::Validating | NativeApplyDispatchState::Applying
    ) {
        return Err("native Apply authority verification requires validating state".to_string());
    }
    let authority = verified_native_autotune_apply_authority(
        state_dir,
        &dispatch.source_job_id,
        Some(&dispatch.option_id),
    )?;
    if authority.source.review_digest != dispatch.review_sha256 {
        return Err("native Apply Review confirmation mismatch".to_string());
    }
    if authority.effective_manifest_digest() != dispatch.manifest_sha256 {
        return Err("native Apply manifest confirmation mismatch".to_string());
    }
    require_exact_native_apply_acknowledgements(
        &authority.source.plan.required_acknowledgements,
        &dispatch.acknowledgements,
    )?;
    let manifest_schema_version = authority.effective_manifest_schema_version();
    let target_state = authority.target_state_name().to_string();
    Ok(NativeApplyVerifiedDispatchIdentity {
        worker_run_id: authority.source.worker_run_id,
        source_manifest_sha256: authority.source.manifest_digest,
        manifest_schema_version,
        target_state,
    })
}

fn execute_native_apply_dispatch(
    state_dir: &Path,
    dispatch: &NativeApplyDispatchRecord,
) -> Result<(NativeApplyTerminalOutcome, bool), String> {
    if euid() != 0 {
        return Err("native Apply requires root".to_string());
    }
    dispatch.validate()?;
    let authority = verified_native_autotune_apply_authority(
        state_dir,
        &dispatch.source_job_id,
        Some(&dispatch.option_id),
    )?;
    if authority.source.worker_run_id != dispatch.worker_run_id
        || authority.source.option_id != dispatch.option_id
        || authority.source.review_digest != dispatch.review_sha256
        || authority.source.manifest_digest != dispatch.source_manifest_sha256
        || authority.effective_manifest_digest() != dispatch.manifest_sha256
        || authority.effective_manifest_schema_version() != dispatch.manifest_schema_version
        || authority.target_state_name() != dispatch.target_state
        || authority.source.plan.required_acknowledgements != dispatch.acknowledgements
    {
        return Err(
            "native Apply durable dispatch no longer matches its verified Review".to_string(),
        );
    }
    let mut backend = OpenWrtNativeApplyBackend::new();
    let receipt = if let Some(bootstrap) = authority.bootstrap.as_ref() {
        execute_native_bootstrap_apply_commit(
            &bootstrap.plan,
            &bootstrap.manifest,
            default_native_apply_paths(),
            &mut backend,
        )?
    } else {
        execute_native_apply_commit(
            &authority.source.plan,
            &authority.source.manifest,
            default_native_apply_paths(),
            &mut backend,
        )?
    };
    let disposition = match receipt.disposition {
        NativeApplyCommitDisposition::Applied => NativeApplyTerminalOutcome::Applied,
        NativeApplyCommitDisposition::AlreadyApplied => NativeApplyTerminalOutcome::AlreadyApplied,
    };
    if receipt.job_id != dispatch.source_job_id
        || receipt.worker_run_id != dispatch.worker_run_id
        || receipt.manifest_sha256 != dispatch.manifest_sha256
    {
        return Err(
            "native Apply transaction receipt changed its durable dispatch identity".to_string(),
        );
    }
    confirm_native_apply_controllers_after_unlock()?;
    Ok((disposition, receipt.recovery_cleared))
}

fn execute_native_apply_worker(
    state_dir: &Path,
    store: &NativeApplyCoordinatorStore,
    dispatch: NativeApplyDispatchRecord,
) -> Result<(), String> {
    let applying = if dispatch.state == NativeApplyDispatchState::Validating {
        let verified = match verified_native_apply_dispatch_identity(state_dir, &dispatch) {
            Ok(value) => value,
            Err(error) => {
                store.complete(&NativeApplyTerminalRecord {
                    dispatch,
                    outcome: NativeApplyTerminalOutcome::Failed,
                    recovery_cleared: true,
                    diagnostic: bounded_native_apply_diagnostic(&error),
                })?;
                return Ok(());
            }
        };
        store.mark_applying(&dispatch, verified)?
    } else if dispatch.state == NativeApplyDispatchState::Applying {
        dispatch
    } else {
        return Err("native Apply worker received a non-executable dispatch".to_string());
    };

    let (existing_pending, bootstrap_pending) = native_apply_recovery_markers_present()?;
    if existing_pending || bootstrap_pending {
        return settle_native_apply_recovery_attempt(
            store,
            &applying,
            None,
            recover_native_apply_outcome(),
        );
    }

    match execute_native_apply_dispatch(state_dir, &applying) {
        Ok((outcome, recovery_cleared)) => store.complete(&NativeApplyTerminalRecord {
            dispatch: applying,
            outcome,
            recovery_cleared,
            diagnostic: String::new(),
        }),
        Err(error) => {
            let (existing_pending, bootstrap_pending) = native_apply_recovery_markers_present()?;
            if existing_pending || bootstrap_pending {
                settle_native_apply_recovery_attempt(
                    store,
                    &applying,
                    Some(&error),
                    recover_native_apply_outcome(),
                )
            } else {
                store.complete(&NativeApplyTerminalRecord {
                    dispatch: applying,
                    outcome: NativeApplyTerminalOutcome::Failed,
                    recovery_cleared: true,
                    diagnostic: bounded_native_apply_diagnostic(&error),
                })
            }
        }
    }
}

fn settle_native_apply_recovery_attempt(
    store: &NativeApplyCoordinatorStore,
    dispatch: &NativeApplyDispatchRecord,
    apply_error: Option<&str>,
    recovery: Result<NativeApplyRecoveryOutcome, String>,
) -> Result<(), String> {
    match recovery {
        Ok(outcome) => match confirm_recovered_native_apply_after_unlock(&outcome) {
            Ok(()) => settle_native_apply_store_recovery(store, &outcome),
            Err(readiness_error) => {
                let diagnostic = apply_error.map_or_else(
                    || readiness_error.clone(),
                    |apply_error| {
                        format!(
                            "native Apply failed: {apply_error}; post-recovery readiness also failed: {readiness_error}"
                        )
                    },
                );
                settle_native_apply_store_readiness_failure(store, &outcome, &diagnostic)
            }
        },
        Err(recovery_error) => {
            let diagnostic = match apply_error {
                Some(apply_error) => format!(
                    "native Apply failed: {apply_error}; recovery remains pending: {recovery_error}"
                ),
                None => format!("native Apply recovery remains pending: {recovery_error}"),
            };
            // The exact recovery authority remains on disk.  Publishing a
            // failed interactive terminal removes only the coordinator's
            // executable dispatch, so an unchanged failure cannot hot-respawn
            // workers.  A later explicit Apply, service recovery, or package
            // lifecycle re-enters recovery from that durable authority.
            store.complete(&NativeApplyTerminalRecord {
                dispatch: dispatch.clone(),
                outcome: NativeApplyTerminalOutcome::Failed,
                recovery_cleared: false,
                diagnostic: bounded_native_apply_diagnostic(&diagnostic),
            })
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum NativeApplyWorkerReadiness {
    Idle,
    Live(ProcessIdentity),
    Launch(NativeApplyDispatchRecord),
}

fn reconcile_native_apply_worker(
    store: &NativeApplyCoordinatorStore,
    proc_root: &Path,
) -> Result<NativeApplyWorkerReadiness, String> {
    let Some(active) = store.read_active()? else {
        if let Some(claim) = store.read_worker_claim()? {
            if claim.process.still_matches(proc_root)? {
                return Ok(NativeApplyWorkerReadiness::Live(claim.process));
            }
            store.remove_worker_claim(&claim)?;
        }
        return Ok(NativeApplyWorkerReadiness::Idle);
    };
    let executable = match active.state {
        NativeApplyDispatchState::Accepted => store.mark_validating(&active)?,
        NativeApplyDispatchState::Validating | NativeApplyDispatchState::Applying => active,
    };
    if let Some(claim) = store.read_worker_claim()? {
        if claim.process.still_matches(proc_root)? {
            if !claim.matches_dispatch(&executable) {
                return Err(
                    "live native Apply worker claim differs from active dispatch".to_string(),
                );
            }
            return Ok(NativeApplyWorkerReadiness::Live(claim.process));
        }
        store.remove_worker_claim(&claim)?;
    }
    Ok(NativeApplyWorkerReadiness::Launch(executable))
}

fn native_apply_accepted_response(record: &NativeApplyDispatchRecord) -> String {
    format!(
        concat!(
            "{{\"state\":\"accepted\",\"apply_job_id\":\"{}\",",
            "\"apply_job_token\":\"{}\",\"job_id\":\"{}\",",
            "\"option_id\":\"{}\",\"generation\":{}}}\n"
        ),
        record.apply_job_id,
        record.apply_job_token,
        record.source_job_id,
        record.option_id,
        record.generation,
    )
}

fn native_apply_status_response(record: &NativeApplyDispatchRecord) -> String {
    format!(
        concat!(
            "{{\"state\":\"{}\",\"terminal\":false,",
            "\"apply_job_id\":\"{}\",\"job_id\":\"{}\",",
            "\"option_id\":\"{}\",\"generation\":{}}}\n"
        ),
        match record.state {
            NativeApplyDispatchState::Accepted => "accepted",
            NativeApplyDispatchState::Validating => "validating",
            NativeApplyDispatchState::Applying => "applying",
        },
        record.apply_job_id,
        record.source_job_id,
        record.option_id,
        record.generation,
    )
}

fn native_apply_terminal_status_response(record: &NativeApplyTerminalRecord) -> String {
    format!(
        concat!(
            "{{\"state\":\"{}\",\"terminal\":true,",
            "\"apply_job_id\":\"{}\",\"job_id\":\"{}\",",
            "\"option_id\":\"{}\",\"generation\":{},",
            "\"recovery_cleared\":{}}}\n"
        ),
        record.outcome.as_str(),
        record.dispatch.apply_job_id,
        record.dispatch.source_job_id,
        record.dispatch.option_id,
        record.dispatch.generation.saturating_add(1),
        bool_json(record.recovery_cleared),
    )
}

fn native_apply_terminal_result_response(record: &NativeApplyTerminalRecord) -> String {
    if !record.outcome.success() {
        return error_response(
            record.outcome.as_str(),
            if record.diagnostic.is_empty() {
                "native Apply did not commit its selected configuration"
            } else {
                &record.diagnostic
            },
        );
    }
    format!(
        concat!(
            "{{\"state\":\"{}\",\"configuration_written\":true,",
            "\"job_id\":\"{}\",\"worker_run_id\":\"{}\",",
            "\"option_id\":\"{}\",\"review_sha256\":\"{}\",",
            "\"source_manifest_sha256\":\"{}\",",
            "\"manifest_sha256\":\"{}\",\"manifest_schema_version\":{},",
            "\"target_state\":\"{}\",",
            "\"acknowledged\":{},\"generation\":{},",
            "\"recovery_cleared\":{}}}\n"
        ),
        record.outcome.as_str(),
        record.dispatch.source_job_id,
        record.dispatch.worker_run_id,
        record.dispatch.option_id,
        record.dispatch.review_sha256,
        record.dispatch.source_manifest_sha256,
        record.dispatch.manifest_sha256,
        record.dispatch.manifest_schema_version,
        record.dispatch.target_state,
        native_apply_acknowledgements_json(&record.dispatch.acknowledgements),
        record.dispatch.generation.saturating_add(1),
        bool_json(record.recovery_cleared),
    )
}

fn require_native_apply_lab_rollback_mode() -> Result<NativeApplyLabFaultInjection, String> {
    let fault = native_apply_lab_fault(env::var(NATIVE_APPLY_LAB_MODE_ENV).ok().as_deref())
        .ok_or_else(|| {
            "native Apply laboratory commands require an exact private forced-rollback gate"
                .to_string()
        })?;
    match fault {
        NativeApplyLabFaultInjection::None
        | NativeApplyLabFaultInjection::PauseAfterServiceRestarted { .. } => Ok(fault),
        _ => Err("native Apply rollback command requires a rollback-only lab mode".to_string()),
    }
}

fn require_native_apply_lab_any_mode() -> Result<NativeApplyLabFaultInjection, String> {
    native_apply_lab_fault(env::var(NATIVE_APPLY_LAB_MODE_ENV).ok().as_deref()).ok_or_else(|| {
        "native Apply recovery command requires an exact private lab gate".to_string()
    })
}

fn require_native_apply_lab_commit_mode() -> Result<NativeApplyLabFaultInjection, String> {
    let fault = native_apply_lab_fault(env::var(NATIVE_APPLY_LAB_MODE_ENV).ok().as_deref())
        .ok_or_else(|| {
            "native Apply commit command requires an exact private commit lab gate".to_string()
        })?;
    match fault {
        NativeApplyLabFaultInjection::PauseAfterVerifiedBeforeCommit { .. }
        | NativeApplyLabFaultInjection::PauseAfterCommitAccepted { .. } => Ok(fault),
        NativeApplyLabFaultInjection::None
            if env::var(NATIVE_APPLY_LAB_MODE_ENV).ok().as_deref()
                == Some(NATIVE_APPLY_LAB_COMMIT) =>
        {
            Ok(fault)
        }
        _ => Err("native Apply commit command requires a commit-only lab mode".to_string()),
    }
}

#[cfg(test)]
fn native_apply_lab_mode_allowed(value: Option<&str>) -> bool {
    native_apply_lab_fault(value).is_some()
}

fn native_apply_lab_fault(value: Option<&str>) -> Option<NativeApplyLabFaultInjection> {
    match value {
        Some(NATIVE_APPLY_LAB_FORCED_ROLLBACK) => Some(NativeApplyLabFaultInjection::None),
        Some(NATIVE_APPLY_LAB_CRASH_AFTER_SERVICE_RESTARTED) => {
            Some(NativeApplyLabFaultInjection::PauseAfterServiceRestarted {
                timeout: NATIVE_APPLY_LAB_CRASH_PAUSE,
            })
        }
        Some(NATIVE_APPLY_LAB_COMMIT) => Some(NativeApplyLabFaultInjection::None),
        Some(NATIVE_APPLY_LAB_CRASH_BEFORE_COMMIT) => Some(
            NativeApplyLabFaultInjection::PauseAfterVerifiedBeforeCommit {
                timeout: NATIVE_APPLY_LAB_CRASH_PAUSE,
            },
        ),
        Some(NATIVE_APPLY_LAB_CRASH_AFTER_COMMIT) => {
            Some(NativeApplyLabFaultInjection::PauseAfterCommitAccepted {
                timeout: NATIVE_APPLY_LAB_CRASH_PAUSE,
            })
        }
        _ => None,
    }
}

fn require_native_apply_digest(label: &str, value: &str) -> Result<(), String> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(format!("native Apply expected {label} digest is invalid"))
    }
}

fn require_native_apply_manifest_digest(value: &str) -> Result<(), String> {
    require_native_apply_digest("manifest", value)
}

fn native_autotune_apply_lab_rollback(
    state_dir: &Path,
    job_id: &str,
    option_id: &str,
    expected_manifest: &str,
    fault: NativeApplyLabFaultInjection,
) -> Result<String, String> {
    require_native_apply_manifest_digest(expected_manifest)?;
    let context = verified_native_autotune_apply_context(state_dir, job_id, Some(option_id))?;
    if context.manifest_digest != expected_manifest {
        return Err("native Apply laboratory manifest confirmation mismatch".to_string());
    }
    let mut backend = OpenWrtNativeApplyBackend::new();
    let receipt = execute_native_apply_forced_rollback_with_fault(
        &context.plan,
        &context.manifest,
        default_native_apply_paths(),
        &mut backend,
        fault,
    )?;
    confirm_native_apply_controllers_after_unlock()?;
    Ok(format!(
        concat!(
            "{{\"state\":\"forced_rollback_verified\",",
            "\"job_id\":\"{}\",\"worker_run_id\":\"{}\",",
            "\"manifest_sha256\":\"{}\",\"apply_verified\":{},",
            "\"rollback_verified\":{},\"recovery_cleared\":{}}}\n"
        ),
        receipt.job_id,
        receipt.worker_run_id,
        receipt.manifest_sha256,
        receipt.apply_verified,
        receipt.rollback_verified,
        receipt.recovery_cleared,
    ))
}

fn native_autotune_apply_lab_commit(
    state_dir: &Path,
    job_id: &str,
    option_id: &str,
    expected_manifest: &str,
    fault: NativeApplyLabFaultInjection,
) -> Result<String, String> {
    require_native_apply_manifest_digest(expected_manifest)?;
    let context = verified_native_autotune_apply_context(state_dir, job_id, Some(option_id))?;
    if context.manifest_digest != expected_manifest {
        return Err("native Apply laboratory manifest confirmation mismatch".to_string());
    }
    let mut backend = OpenWrtNativeApplyBackend::new();
    let receipt = execute_native_apply_commit_with_fault(
        &context.plan,
        &context.manifest,
        default_native_apply_paths(),
        &mut backend,
        fault,
    )?;
    confirm_native_apply_controllers_after_unlock()?;
    let disposition = match receipt.disposition {
        NativeApplyCommitDisposition::Applied => "applied",
        NativeApplyCommitDisposition::AlreadyApplied => "already_applied",
    };
    Ok(format!(
        concat!(
            "{{\"state\":\"{}\",\"job_id\":\"{}\",",
            "\"worker_run_id\":\"{}\",\"manifest_sha256\":\"{}\",",
            "\"recovery_cleared\":{}}}\n"
        ),
        disposition,
        receipt.job_id,
        receipt.worker_run_id,
        receipt.manifest_sha256,
        receipt.recovery_cleared,
    ))
}

fn native_autotune_apply_lab_recover() -> Result<String, String> {
    native_autotune_apply_recovery()
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum NativeApplyRecoveryOutcome {
    None,
    Recovered {
        authority: &'static str,
        job_id: String,
        worker_run_id: String,
        recovery_cleared: bool,
        rolled_forward: bool,
    },
}

impl NativeApplyRecoveryOutcome {
    fn response(&self) -> String {
        match self {
            Self::None => "{\"state\":\"no_pending_recovery\"}\n".to_string(),
            Self::Recovered {
                authority,
                job_id,
                worker_run_id,
                recovery_cleared,
                rolled_forward,
            } => format!(
                concat!(
                    "{{\"state\":\"recovered\",\"authority\":\"{}\",",
                    "\"job_id\":\"{}\",\"worker_run_id\":\"{}\",",
                    "\"recovery_cleared\":{},\"rolled_forward\":{}}}\n"
                ),
                authority,
                job_id,
                worker_run_id,
                bool_json(*recovery_cleared),
                bool_json(*rolled_forward),
            ),
        }
    }
}

fn confirm_recovered_native_apply_after_unlock(
    outcome: &NativeApplyRecoveryOutcome,
) -> Result<(), String> {
    confirm_recovered_native_apply_after_unlock_with(
        outcome,
        confirm_native_apply_controllers_after_unlock,
    )
}

fn confirm_recovered_native_apply_after_unlock_with<F>(
    outcome: &NativeApplyRecoveryOutcome,
    confirm: F,
) -> Result<(), String>
where
    F: FnOnce() -> Result<(), String>,
{
    if matches!(outcome, NativeApplyRecoveryOutcome::Recovered { .. }) {
        confirm()?;
    }
    Ok(())
}

pub(crate) fn native_autotune_apply_recovery() -> Result<String, String> {
    if euid() != 0 {
        return Err("native Apply recovery requires root".to_string());
    }
    let outcome = recover_native_apply_outcome()?;
    if let Err(error) = confirm_recovered_native_apply_after_unlock(&outcome) {
        let store = NativeApplyCoordinatorStore::new(default_native_apply_paths().recovery_root);
        settle_native_apply_store_readiness_failure(&store, &outcome, &error)?;
        return Err(error);
    }
    settle_native_apply_coordinator_recovery(&outcome)?;
    Ok(outcome.response())
}

fn recover_native_apply_outcome() -> Result<NativeApplyRecoveryOutcome, String> {
    let (existing_pending, bootstrap_pending) = native_apply_recovery_markers_present()?;
    let mut backend = OpenWrtNativeApplyBackend::new();
    if existing_pending {
        let receipt = recover_native_apply(default_native_apply_paths(), &mut backend)?
            .ok_or_else(|| "existing native Apply recovery marker disappeared".to_string())?;
        return Ok(NativeApplyRecoveryOutcome::Recovered {
            authority: "existing_v4",
            job_id: receipt.job_id,
            worker_run_id: receipt.worker_run_id,
            recovery_cleared: receipt.recovery_cleared,
            rolled_forward: receipt.rolled_forward,
        });
    }
    if bootstrap_pending {
        let receipt =
            recover_native_bootstrap_apply(default_native_apply_paths(), &mut backend)?
                .ok_or_else(|| "bootstrap native Apply recovery marker disappeared".to_string())?;
        return Ok(NativeApplyRecoveryOutcome::Recovered {
            authority: "bootstrap_v7",
            job_id: receipt.job_id,
            worker_run_id: receipt.worker_run_id,
            recovery_cleared: receipt.recovery_cleared,
            rolled_forward: receipt.rolled_forward,
        });
    }
    let _ = recover_native_apply(default_native_apply_paths(), &mut backend)?;
    let _ = recover_native_bootstrap_apply(default_native_apply_paths(), &mut backend)?;
    Ok(NativeApplyRecoveryOutcome::None)
}

fn settle_native_apply_coordinator_recovery(
    outcome: &NativeApplyRecoveryOutcome,
) -> Result<(), String> {
    let store = NativeApplyCoordinatorStore::new(default_native_apply_paths().recovery_root);
    settle_native_apply_store_recovery(&store, outcome)
}

fn settle_native_apply_store_recovery(
    store: &NativeApplyCoordinatorStore,
    outcome: &NativeApplyRecoveryOutcome,
) -> Result<(), String> {
    let NativeApplyRecoveryOutcome::Recovered {
        recovery_cleared,
        rolled_forward,
        ..
    } = outcome
    else {
        return Ok(());
    };
    let Some(active) = native_apply_recovery_active_dispatch(store, outcome)? else {
        return Ok(());
    };
    let (outcome, diagnostic) = if *rolled_forward {
        (NativeApplyTerminalOutcome::Applied, String::new())
    } else {
        (
            NativeApplyTerminalOutcome::RolledBack,
            "native Apply was rolled back during coordinator recovery".to_string(),
        )
    };
    store.complete(&NativeApplyTerminalRecord {
        dispatch: active,
        outcome,
        recovery_cleared: *recovery_cleared,
        diagnostic,
    })
}

fn native_apply_recovery_active_dispatch(
    store: &NativeApplyCoordinatorStore,
    outcome: &NativeApplyRecoveryOutcome,
) -> Result<Option<NativeApplyDispatchRecord>, String> {
    let NativeApplyRecoveryOutcome::Recovered {
        job_id,
        worker_run_id,
        ..
    } = outcome
    else {
        return Ok(None);
    };
    let Some(active) = store.read_active()? else {
        // Scheduled Auto-Apply and explicit recovery commands legitimately use
        // the same transaction engine without an interactive coordinator job.
        return Ok(None);
    };
    if active.state != NativeApplyDispatchState::Applying {
        return Err("native Apply recovery exists before verified mutation authority".to_string());
    }
    if active.source_job_id != *job_id || active.worker_run_id != *worker_run_id {
        return Err(
            "native Apply recovery identity differs from its coordinator dispatch".to_string(),
        );
    }
    Ok(Some(active))
}

fn settle_native_apply_store_readiness_failure(
    store: &NativeApplyCoordinatorStore,
    outcome: &NativeApplyRecoveryOutcome,
    error: &str,
) -> Result<(), String> {
    let NativeApplyRecoveryOutcome::Recovered {
        recovery_cleared, ..
    } = outcome
    else {
        return Ok(());
    };
    let Some(active) = native_apply_recovery_active_dispatch(store, outcome)? else {
        return Ok(());
    };
    store.complete(&NativeApplyTerminalRecord {
        dispatch: active,
        outcome: NativeApplyTerminalOutcome::Failed,
        recovery_cleared: *recovery_cleared,
        diagnostic: bounded_native_apply_diagnostic(&format!(
            "native Apply recovered its durable configuration, but controller readiness failed: {error}"
        )),
    })
}

fn read_operation_request(path: &Path) -> Result<OperationRequest, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("unable to inspect operation request file: {error}"))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != euid()
        || metadata.mode() & 0o077 != 0
    {
        return Err("operation request file must be a private root-owned regular file".to_string());
    }
    if metadata.len() > MAX_OPERATION_RECORD_BYTES as u64 {
        return Err("operation request file exceeds its size bound".to_string());
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|file| {
            file.take((MAX_OPERATION_RECORD_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
        })
        .map_err(|error| format!("unable to read operation request file: {error}"))?;
    if bytes.len() > MAX_OPERATION_RECORD_BYTES {
        return Err("operation request file exceeds its size bound".to_string());
    }
    let contents =
        String::from_utf8(bytes).map_err(|_| "operation request file is not UTF-8".to_string())?;
    OperationRequest::decode(&contents)
}

struct DaemonOptions {
    state_dir: PathBuf,
    scheduler_store_dir: Option<PathBuf>,
    native_rating: bool,
    native_speedtest: bool,
    native_autotune: bool,
    native_scheduler: bool,
    lab_rust_rating: bool,
    lab_rust_speedtest: bool,
    lab_rust_autotune: bool,
    lab_rust_scheduler: bool,
}

fn parse_daemon_args<I>(mut args: I) -> Result<DaemonOptions, String>
where
    I: Iterator<Item = String>,
{
    let mut state_dir = state_dir_from_env();
    let mut explicit_state_dir = false;
    let mut native_rating = false;
    let mut native_speedtest = false;
    let mut native_autotune = false;
    let mut native_scheduler = false;
    let mut scheduler_store_dir = None;
    let mut lab_rust_rating = false;
    let mut lab_rust_speedtest = false;
    let mut lab_rust_autotune = false;
    let mut lab_rust_scheduler = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--state-dir" => {
                state_dir = PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--state-dir requires a path".to_string())?,
                );
                explicit_state_dir = true;
            }
            "--native-autotune" if !native_autotune => {
                native_autotune = true;
            }
            "--native-rating" if !native_rating => {
                native_rating = true;
            }
            "--native-speedtest" if !native_speedtest => {
                native_speedtest = true;
            }
            "--native-scheduler" if !native_scheduler => {
                native_scheduler = true;
            }
            "--scheduler-store-dir" if scheduler_store_dir.is_none() => {
                scheduler_store_dir =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        "--scheduler-store-dir requires a path".to_string()
                    })?));
            }
            "--lab-rust-rating" if !lab_rust_rating => {
                lab_rust_rating = true;
            }
            "--lab-rust-speedtest" if !lab_rust_speedtest => {
                lab_rust_speedtest = true;
            }
            "--lab-rust-autotune" if !lab_rust_autotune => {
                lab_rust_autotune = true;
            }
            "--lab-rust-scheduler" if !lab_rust_scheduler => {
                lab_rust_scheduler = true;
            }
            _ => return Err(format!("unsupported calibrationd option: {arg}")),
        }
    }
    if lab_rust_rating {
        if !explicit_state_dir {
            return Err("lab Rust rating requires an explicit isolated --state-dir".to_string());
        }
        if env::var("CAKE_AUTORATE_ENABLE_LAB_RUST_RATING").as_deref() != Ok("1") {
            return Err(
                "lab Rust rating requires CAKE_AUTORATE_ENABLE_LAB_RUST_RATING=1".to_string(),
            );
        }
    }
    if lab_rust_speedtest {
        if !explicit_state_dir {
            return Err("lab Rust speedtest requires an explicit isolated --state-dir".to_string());
        }
        if env::var("CAKE_AUTORATE_ENABLE_LAB_RUST_SPEEDTEST").as_deref() != Ok("1") {
            return Err(
                "lab Rust speedtest requires CAKE_AUTORATE_ENABLE_LAB_RUST_SPEEDTEST=1".to_string(),
            );
        }
    }
    if lab_rust_autotune {
        if !explicit_state_dir {
            return Err("lab Rust Auto-Tune requires an explicit isolated --state-dir".to_string());
        }
        if env::var("CAKE_AUTORATE_ENABLE_LAB_RUST_AUTOTUNE").as_deref() != Ok("1") {
            return Err(
                "lab Rust Auto-Tune requires CAKE_AUTORATE_ENABLE_LAB_RUST_AUTOTUNE=1".to_string(),
            );
        }
    }
    if native_scheduler && lab_rust_scheduler {
        return Err(
            "production and laboratory native schedulers require separate coordinators".to_string(),
        );
    }
    if native_scheduler {
        if !native_autotune {
            return Err("production native scheduler requires --native-autotune".to_string());
        }
        let store = scheduler_store_dir.as_ref().ok_or_else(|| {
            "production native scheduler requires an explicit --scheduler-store-dir".to_string()
        })?;
        if store != Path::new(PRODUCTION_SCHEDULER_STORE_DIR) {
            return Err(format!(
                "production native scheduler store must be {PRODUCTION_SCHEDULER_STORE_DIR}"
            ));
        }
    }
    if lab_rust_scheduler {
        if !explicit_state_dir {
            return Err("lab Rust scheduler requires an explicit isolated --state-dir".to_string());
        }
        let store = scheduler_store_dir.as_ref().ok_or_else(|| {
            "lab Rust scheduler requires an explicit --scheduler-store-dir".to_string()
        })?;
        if store == &state_dir {
            return Err(
                "lab Rust scheduler state and coordinator state must be separate".to_string(),
            );
        }
        if !lab_rust_autotune {
            return Err("lab Rust scheduler requires --lab-rust-autotune".to_string());
        }
        if env::var("CAKE_AUTORATE_ENABLE_LAB_RUST_SCHEDULER").as_deref() != Ok("1") {
            return Err(
                "lab Rust scheduler requires CAKE_AUTORATE_ENABLE_LAB_RUST_SCHEDULER=1".to_string(),
            );
        }
    } else if !native_scheduler && scheduler_store_dir.is_some() {
        return Err(
            "--scheduler-store-dir is valid only with --native-scheduler or --lab-rust-scheduler"
                .to_string(),
        );
    }
    if (native_autotune || native_rating || native_speedtest)
        && (lab_rust_rating || lab_rust_speedtest || lab_rust_autotune || lab_rust_scheduler)
    {
        return Err(
            "production native operations cannot be combined with laboratory adapters".to_string(),
        );
    }
    Ok(DaemonOptions {
        state_dir,
        scheduler_store_dir,
        native_rating,
        native_speedtest,
        native_autotune,
        native_scheduler,
        lab_rust_rating,
        lab_rust_speedtest,
        lab_rust_autotune,
        lab_rust_scheduler,
    })
}

fn state_dir_from_env() -> PathBuf {
    env::var_os("CAKE_AUTORATE_CALIBRATION_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_STATE_DIR))
}

fn scheduler_rating_runtime_path(instance: &str) -> PathBuf {
    env::var_os("CAKE_AUTORATE_RUN_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/run/cake-autorate"))
        .join(instance)
        .join("rating-runtime")
}

fn scheduled_runtime_substantively_ready(snapshot: &rating::RatingRuntimeSnapshot) -> bool {
    snapshot.runtime_generation != 0
        && matches!(snapshot.uplink_state.as_str(), "ACTIVE" | "STANDBY")
        && snapshot.route_test_ready
        && (!snapshot.sqm_runtime_managed || snapshot.sqm_runtime_healthy)
        && snapshot.transport_probe_trusted
        && snapshot.baseline_ready
        && snapshot.baseline_samples >= snapshot.baseline_required_samples
        && snapshot.cake_dl_kbps >= 100.0
        && snapshot.cake_ul_kbps >= 100.0
}

fn scheduled_minimum_traffic_budget_bytes(snapshot: &rating::RatingRuntimeSnapshot) -> u64 {
    speedtest::minimum_full_autotune_traffic_budget_bytes(
        snapshot.cake_dl_kbps.ceil() as u64,
        snapshot.cake_ul_kbps.ceil() as u64,
    )
}

fn scheduled_runtime_snapshot_fresh(
    snapshot: &rating::RatingRuntimeSnapshot,
    observed_unix_ms: u64,
) -> bool {
    snapshot.updated_unix_ms <= observed_unix_ms
        && observed_unix_ms - snapshot.updated_unix_ms <= SCHEDULER_RUNTIME_FRESHNESS_MS
}

fn scheduled_runtime_and_quiet_ready(
    quiet: &mut BTreeMap<String, QuietEvidence>,
    instance: &str,
    snapshot: &rating::RatingRuntimeSnapshot,
    active_threshold_kbps: u64,
    idle_window_s: u64,
    observed_unix_ms: u64,
) -> (bool, bool) {
    if !scheduled_runtime_substantively_ready(snapshot) {
        // Route, controller generation, SQM health, transport trust or
        // baseline loss contradicts the accumulated quiet interval.
        quiet.remove(instance);
        return (false, false);
    }
    if !scheduled_runtime_snapshot_fresh(snapshot, observed_unix_ms) {
        // A control wake may see an old publication, while a rating
        // publication can race a clock sampled before config attestation.
        // Neither observation proves that the link was busy.  Keep the
        // evidence fail-closed and let the next authoritative publication
        // either continue it or reset it through its own timestamp gap.
        return (false, false);
    }
    let quiet_window_ready = quiet.entry(instance.to_string()).or_default().observe(
        snapshot,
        active_threshold_kbps,
        idle_window_s,
    );
    (true, quiet_window_ready)
}

fn kernel_request_id() -> Result<String, String> {
    let raw = fs::read_to_string("/proc/sys/kernel/random/uuid")
        .map_err(|error| format!("unable to read kernel random UUID: {error}"))?;
    let id = raw.trim().replace('-', "").to_ascii_lowercase();
    if id.len() != 32
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("kernel random UUID is malformed".to_string());
    }
    Ok(id)
}

struct CalibrationDaemon {
    listener: UnixListener,
    socket_path: PathBuf,
    socket_inode: u64,
    coordinator: CoordinatorIdentity,
    journal_store: JournalStore,
    jobs: Vec<ScannedJob>,
    leases: LeaseTable,
    startup_issues: Vec<String>,
    admission_requested: bool,
    state_dir: PathBuf,
    native_rating: bool,
    native_speedtest: bool,
    native_autotune: bool,
    native_scheduler: Option<NativeSchedulerRuntime>,
    native_children: BTreeMap<String, ManagedChild>,
    bootstrap_runtime_children: BTreeMap<String, ManagedChild>,
    runtime_attestor: Option<fn(&OperationRequest) -> RuntimeAttestation>,
    bootstrap_runtime_attestor: BootstrapRuntimeAttestor,
    bootstrap_runtime_baseline_capturer: BootstrapRuntimeBaselineCapturer,
    route_pin_cleaner: RoutePinCleaner,
    cancellations: BTreeMap<String, PendingWorkerCancellation>,
    job_errors: BTreeMap<String, String>,
    native_apply_store: NativeApplyCoordinatorStore,
    native_apply_live_state_attestor: NativeApplyLiveStateAttestor,
    native_apply_child: Option<ManagedChild>,
    native_apply_worker_identity: Option<ProcessIdentity>,
    native_apply_watches: Vec<PendingNativeApplyWatch>,
}

struct NativeSchedulerRuntime {
    store: SchedulerStore,
    quiet: BTreeMap<String, QuietEvidence>,
    errors: BTreeMap<String, String>,
    auto_apply_errors: BTreeMap<String, String>,
    auto_apply_warnings: BTreeMap<String, String>,
    waiting: BTreeMap<String, &'static str>,
    accounting_blocks: BTreeMap<String, u64>,
    status_cache: NativeSchedulerStatusCache,
    lab_mode: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct NativeSchedulerStatusCache {
    last_good: Option<NativeSchedulerStatusRows>,
    encoded: Option<String>,
    next_scheduler_deadline: Option<Instant>,
}

impl NativeSchedulerStatusCache {
    fn response(&self) -> Option<&str> {
        self.encoded.as_deref()
    }

    fn publish(&mut self, rows: NativeSchedulerStatusRows) -> Result<(), String> {
        let encoded = format_native_scheduler_batch(&rows, false, None)?;
        let next_scheduler_deadline = rows.next_scheduler_wake_at.and_then(|wake_at| {
            rows.observed_monotonic.checked_add(Duration::from_secs(
                wake_at.saturating_sub(rows.observed_at),
            ))
        });
        *self = Self {
            last_good: Some(rows),
            encoded: Some(encoded),
            next_scheduler_deadline,
        };
        Ok(())
    }

    fn next_scheduler_timeout(&self, now: Instant) -> Option<Duration> {
        self.next_scheduler_deadline
            .map(|deadline| deadline.saturating_duration_since(now))
    }

    fn mark_global_failure(&mut self) {
        if self
            .next_scheduler_deadline
            .is_some_and(|deadline| deadline <= Instant::now())
        {
            self.next_scheduler_deadline = None;
        }
        let Some(rows) = self.last_good.clone() else {
            self.encoded = None;
            self.next_scheduler_deadline = None;
            return;
        };
        self.encoded =
            format_native_scheduler_batch(&rows, true, Some(SCHEDULER_STATUS_GLOBAL_ERROR)).ok();
    }
}

enum ScheduledInstancePassError {
    Instance(String),
    Global(String),
}

impl ScheduledInstancePassError {
    fn instance(context: &str, error: String) -> Self {
        Self::Instance(format!("{context}: {error}"))
    }

    fn global(error: String) -> Self {
        Self::Global(error)
    }
}

#[derive(Clone, Debug)]
struct PendingWorkerCancellation {
    process: Option<ProcessIdentity>,
    runtime_owner: Option<ProcessIdentity>,
    kill_after: Instant,
}

struct PendingNativeApplyWatch {
    stream: UnixStream,
    request: NativeApplyControlRequest,
    observed_generation: u64,
    deadline: Instant,
}

impl CalibrationDaemon {
    fn bind(state_dir: &Path) -> Result<Self, String> {
        Self::bind_with_components(state_dir, false, Some(attest_openwrt_runtime))
    }

    #[cfg(test)]
    fn bind_with_admission(state_dir: &Path, admission_requested: bool) -> Result<Self, String> {
        Self::bind_with_components(state_dir, admission_requested, None)
    }

    fn bind_with_components(
        state_dir: &Path,
        admission_requested: bool,
        runtime_attestor: Option<fn(&OperationRequest) -> RuntimeAttestation>,
    ) -> Result<Self, String> {
        secure_state_dir(state_dir)?;
        let coordinator = CoordinatorIdentity::current()?;
        let journal_store = JournalStore::open(state_dir, coordinator.clone())?;
        let scan = journal_store.scan(Path::new(DEFAULT_PROC_ROOT))?;
        let mut jobs = scan.jobs;
        let mut leases = LeaseTable::default();
        let mut startup_issues = scan.unsafe_entries;
        for job in &mut jobs {
            if job.journal.boot_id == coordinator.boot_id
                && job.journal.coordinator_generation != coordinator.generation
            {
                match journal_store.adopt_generation(&job.journal) {
                    Ok(adopted) => job.journal = adopted,
                    Err(error) => startup_issues.push(format!(
                        "unable to adopt job {} into the current coordinator generation: {error}",
                        job.journal.job_id
                    )),
                }
            }
            if job.disposition == JournalDisposition::DeadBeforeMutation
                && !state::terminal(job.journal.state)
                && !job.journal.runtime_mutated
            {
                let mut failed = job.journal.clone();
                match failed
                    .settle_native_terminal("failed", Some("native-worker-disappeared"))
                    .and_then(|_| journal_store.update(&failed))
                {
                    Ok(()) => {
                        job.journal = failed;
                        job.disposition = JournalDisposition::Settled;
                    }
                    Err(error) => startup_issues.push(format!(
                        "unable to settle abandoned native job {}: {error}",
                        job.journal.job_id
                    )),
                }
            }
            if job.disposition == JournalDisposition::RecoveryRequired
                && job.journal.state != super::protocol::OperationState::Recovering
            {
                let mut recovering = job.journal.clone();
                match recovering
                    .require_recovery("startup-reconciliation-required")
                    .and_then(|_| journal_store.update(&recovering))
                {
                    Ok(()) => job.journal = recovering,
                    Err(error) => startup_issues.push(format!(
                        "unable to normalize job {} into recovery after restart: {error}",
                        job.journal.job_id
                    )),
                }
            }
        }
        for job in &jobs {
            if matches!(
                job.disposition,
                JournalDisposition::DeadBeforeMutation | JournalDisposition::Settled
            ) {
                continue;
            }
            match LeaseRequest::from_journalled_operation(
                &job.request,
                job.journal.heavy_lease_acquired,
            )
            .and_then(|request| leases.acquire(request).map_err(|error| error.to_string()))
            {
                Ok(()) => {}
                Err(error) => startup_issues.push(format!(
                    "unable to reconstruct leases for job {}: {error}",
                    job.journal.job_id
                )),
            }
        }
        let socket_path = state_dir.join(CONTROL_SOCKET_NAME);
        if socket_path.as_os_str().len() > 100 {
            return Err("calibration control socket path is too long".to_string());
        }
        remove_stale_socket(&socket_path)?;
        let listener = UnixListener::bind(&socket_path)
            .map_err(|error| format!("unable to bind calibration control socket: {error}"))?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("unable to secure calibration control socket: {error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("unable to configure calibration control socket: {error}"))?;
        let socket_inode = fs::symlink_metadata(&socket_path)
            .map_err(|error| format!("unable to inspect calibration control socket: {error}"))?
            .ino();
        let mut daemon = Self {
            listener,
            socket_path,
            socket_inode,
            coordinator,
            journal_store,
            jobs,
            leases,
            startup_issues,
            admission_requested,
            state_dir: state_dir.to_path_buf(),
            native_rating: false,
            native_speedtest: false,
            native_autotune: false,
            native_scheduler: None,
            native_children: BTreeMap::new(),
            bootstrap_runtime_children: BTreeMap::new(),
            runtime_attestor,
            bootstrap_runtime_attestor: attest_openwrt_bootstrap_runtime,
            bootstrap_runtime_baseline_capturer: capture_openwrt_bootstrap_runtime,
            route_pin_cleaner: speedtest::cleanup_route_pin,
            cancellations: BTreeMap::new(),
            job_errors: BTreeMap::new(),
            native_apply_store: NativeApplyCoordinatorStore::new(
                default_native_apply_paths().recovery_root,
            ),
            native_apply_live_state_attestor: native_autotune_apply_live_state,
            native_apply_child: None,
            native_apply_worker_identity: None,
            native_apply_watches: Vec::new(),
        };
        daemon.resume_journalled_cancellations();
        if daemon.startup_issues.is_empty() {
            if let Err(error) = daemon.prune_settled_history() {
                daemon
                    .startup_issues
                    .push(format!("unable to prune settled journal history: {error}"));
            }
        }
        Ok(daemon)
    }

    fn serve_once(&mut self) -> Result<Option<ControlEffect>, String> {
        let (mut stream, _) = match self.listener.accept() {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
            Err(error) => return Err(format!("calibration control accept failed: {error}")),
        };
        stream
            .set_read_timeout(Some(IO_TIMEOUT))
            .map_err(|error| format!("unable to set control read timeout: {error}"))?;
        stream
            .set_write_timeout(Some(IO_TIMEOUT))
            .map_err(|error| format!("unable to set control write timeout: {error}"))?;
        let (response, effect) = match peer_credentials(&stream) {
            Ok(credentials) if credentials.uid == euid() => match read_request(&mut stream) {
                Ok(CoordinatorControlRequest::NativeApply(request))
                    if request.command == NativeApplyControlCommand::Watch =>
                {
                    return self.serve_native_apply_watch(stream, request).map(Some);
                }
                Ok(request) => self.dispatch_control_request(request),
                Err(error) => (
                    error_response("invalid-request", &error),
                    ControlEffect::ReadOnly,
                ),
            },
            Ok(_) => (
                error_response(
                    "access-denied",
                    "calibration control peer is not root-owned",
                ),
                ControlEffect::ReadOnly,
            ),
            Err(error) => (
                error_response("peer-identity-unavailable", &error),
                ControlEffect::ReadOnly,
            ),
        };
        stream
            .write_all(response.as_bytes())
            .map_err(|error| format!("unable to write calibration control response: {error}"))?;
        Ok(Some(effect))
    }

    fn serve_native_apply_watch(
        &mut self,
        mut stream: UnixStream,
        request: NativeApplyControlRequest,
    ) -> Result<ControlEffect, String> {
        if let Err(error) = request.validate() {
            stream
                .write_all(error_response("native-apply-watch-invalid", &error).as_bytes())
                .map_err(|error| format!("unable to write native Apply watch error: {error}"))?;
            return Ok(ControlEffect::ReadOnly);
        }
        let observed_generation = request
            .observed_generation
            .expect("validated native Apply watch has an observed generation");
        match self.native_apply_watch_response(&request, observed_generation) {
            Ok(Some(response)) => {
                stream.write_all(response.as_bytes()).map_err(|error| {
                    format!("unable to write native Apply watch response: {error}")
                })?;
            }
            Ok(None) if self.native_apply_watches.len() < MAX_PENDING_NATIVE_APPLY_WATCHES => {
                self.native_apply_watches.push(PendingNativeApplyWatch {
                    stream,
                    request,
                    observed_generation,
                    deadline: Instant::now() + NATIVE_APPLY_WATCH_HOLD,
                });
            }
            Ok(None) => {
                stream
                    .write_all(
                        error_response(
                            "native-apply-watch-capacity",
                            "native Apply watch capacity is exhausted",
                        )
                        .as_bytes(),
                    )
                    .map_err(|error| {
                        format!("unable to write native Apply watch capacity error: {error}")
                    })?;
            }
            Err(error) => {
                stream
                    .write_all(error_response("native-apply-watch-invalid", &error).as_bytes())
                    .map_err(|error| {
                        format!("unable to write native Apply watch lookup error: {error}")
                    })?;
            }
        }
        Ok(ControlEffect::ReadOnly)
    }

    fn native_apply_watch_response(
        &self,
        request: &NativeApplyControlRequest,
        observed_generation: u64,
    ) -> Result<Option<String>, String> {
        let (generation, terminal, response) = self.native_apply_current_status(request)?;
        if !terminal && observed_generation > generation {
            return Err(
                "native Apply watch generation is ahead of the durable dispatch".to_string(),
            );
        }
        Ok((terminal || generation > observed_generation).then_some(response))
    }

    fn native_apply_current_status(
        &self,
        request: &NativeApplyControlRequest,
    ) -> Result<(u64, bool, String), String> {
        if let Some(active) = self.native_apply_store.authorized_active(request)? {
            return Ok((
                active.generation,
                false,
                native_apply_status_response(&active),
            ));
        }
        if let Some(terminal) = self.native_apply_store.authorized_terminal(request)? {
            return Ok((
                terminal.dispatch.generation.saturating_add(1),
                true,
                native_apply_terminal_status_response(&terminal),
            ));
        }
        Err("no native Apply matches the supplied watch handle".to_string())
    }

    fn flush_native_apply_watches(&mut self) {
        let now = Instant::now();
        let pending = std::mem::take(&mut self.native_apply_watches);
        for mut watch in pending {
            let response =
                match self.native_apply_watch_response(&watch.request, watch.observed_generation) {
                    Ok(Some(response)) => Some(response),
                    Ok(None) if watch.deadline <= now => self
                        .native_apply_current_status(&watch.request)
                        .map(|(_, _, response)| response)
                        .ok(),
                    Ok(None) => {
                        self.native_apply_watches.push(watch);
                        continue;
                    }
                    Err(error) => Some(error_response("native-apply-watch-invalid", &error)),
                };
            if let Some(response) = response {
                let _ = watch.stream.write_all(response.as_bytes());
            }
        }
    }

    fn dispatch_control_request(
        &mut self,
        request: CoordinatorControlRequest,
    ) -> (String, ControlEffect) {
        match request {
            CoordinatorControlRequest::Operation(request) => self.handle_with_effect(&request),
            CoordinatorControlRequest::NativeApply(request) => {
                self.handle_native_apply_control(&request)
            }
            CoordinatorControlRequest::SchedulerAccountingAcknowledgement(request) => {
                self.handle_scheduler_accounting_acknowledgement_with_effect(&request)
            }
            CoordinatorControlRequest::SchedulerStatus(request) => (
                self.handle_scheduler_status(&request),
                ControlEffect::ReadOnly,
            ),
        }
    }

    fn drain_control_requests(&mut self) -> Result<ControlEffect, String> {
        while let Some(effect) = self.serve_once()? {
            if effect == ControlEffect::StateChanged {
                // Preserve a linearizable control boundary: a read queued
                // behind this transition must observe the post-tick cache,
                // never the pre-transition scheduler snapshot.
                return Ok(ControlEffect::StateChanged);
            }
        }
        Ok(ControlEffect::ReadOnly)
    }

    fn refresh_event_watches(&self, events: &mut CalibrationEventLoop) -> Result<bool, String> {
        events.watch_tree(&self.state_dir, 4, true)?;
        if let Some(scheduler) = self.native_scheduler.as_ref() {
            events.watch_tree(scheduler.store.root(), 0, true)?;
            events.watch_tree(Path::new("/etc/config"), 0, true)?;

            let uci_delta = Path::new("/tmp/.uci");
            if uci_delta.exists() {
                events.watch_tree(uci_delta, 1, true)?;
            } else {
                events.watch_named_entry(Path::new("/tmp"), b".uci")?;
            }
        }
        if self.native_rating
            || self.native_speedtest
            || self.native_autotune
            || self.native_scheduler.is_some()
        {
            let runtime_root = env::var_os("CAKE_AUTORATE_RUN_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/var/run/cake-autorate"));
            if runtime_root.exists() {
                events.watch_tree(&runtime_root, 2, true)?;
            } else if let Some(parent) = runtime_root.parent() {
                let parent = fs::canonicalize(parent).map_err(|error| {
                    format!(
                        "unable to resolve missing scheduler runtime parent {}: {error}",
                        parent.display()
                    )
                })?;
                let name = runtime_root
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| "scheduler runtime root name is not UTF-8".to_string())?;
                events.watch_named_entry(&parent, name.as_bytes())?;
            }
        }
        let processes: Vec<ProcessIdentity> = self
            .jobs
            .iter()
            .filter(|job| !state::terminal(job.journal.state))
            .filter_map(|job| job.journal.process.clone())
            .chain(
                self.jobs
                    .iter()
                    .filter(|job| !state::terminal(job.journal.state))
                    .filter_map(|job| job.journal.runtime_owner_process.clone()),
            )
            .chain(
                self.bootstrap_runtime_children
                    .values()
                    .map(|child| child.identity.clone()),
            )
            .chain(self.cancellations.values().flat_map(|pending| {
                [pending.process.clone(), pending.runtime_owner.clone()]
                    .into_iter()
                    .flatten()
            }))
            .chain(self.native_apply_worker_identity.clone())
            .collect();
        events.refresh_processes(&processes, Path::new(DEFAULT_PROC_ROOT))
    }

    fn next_event_timeout(&self) -> Result<Option<Duration>, String> {
        let now = Instant::now();
        let mut timeout = None;
        for deadline in self
            .cancellations
            .values()
            .map(|pending| pending.kill_after)
            .chain(self.native_apply_watches.iter().map(|watch| watch.deadline))
        {
            if deadline > now {
                reduce_timeout(&mut timeout, deadline.duration_since(now));
            }
        }

        let now_unix_ms = rating::epoch_ms()?;
        for job in &self.jobs {
            if state::terminal(job.journal.state) {
                continue;
            }
            if job.request.deadline_unix_ms > now_unix_ms {
                reduce_timeout(
                    &mut timeout,
                    Duration::from_millis(job.request.deadline_unix_ms - now_unix_ms),
                );
            }
        }
        if let Some(calendar) = self
            .native_scheduler
            .as_ref()
            .and_then(|scheduler| scheduler.status_cache.next_scheduler_timeout(now))
        {
            reduce_timeout(&mut timeout, calendar);
        }
        Ok(timeout)
    }

    fn tick(&mut self) {
        self.drive_native_apply();
        self.poll_native_children();
        self.poll_bootstrap_runtime_owners();
        self.poll_worker_cancellations();
        self.poll_pending_runtime_permits();
        self.monitor_running_workers();
        self.poll_native_runtime_recoveries();
        self.poll_native_scheduler();
        self.dispatch_next_queued();
        self.flush_native_apply_watches();
    }

    fn poll_native_scheduler(&mut self) {
        let Some(mut scheduler) = self.native_scheduler.take() else {
            return;
        };
        if let Err(error) = self.poll_native_scheduler_inner(&mut scheduler) {
            scheduler.errors.insert("_global".to_string(), error);
            scheduler.status_cache.mark_global_failure();
        }
        self.native_scheduler = Some(scheduler);
    }

    fn poll_native_scheduler_inner(
        &mut self,
        scheduler: &mut NativeSchedulerRuntime,
    ) -> Result<(), String> {
        scheduler.errors.clear();
        scheduler.waiting.clear();
        scheduler.accounting_blocks.clear();
        let observed_monotonic = Instant::now();
        let now_unix_ms = rating::epoch_ms()?;
        let now_unix_s = now_unix_ms / 1_000;
        let calendar = local_calendar(now_unix_s, 0, 0)?;

        self.reconcile_native_scheduler_reservations(
            scheduler,
            now_unix_s,
            &calendar.day,
            &calendar.month,
        )?;

        let configuration_committed = scheduled_configuration_committed()?;
        let snapshot = load_scheduled_instances()?;
        let auto_apply_instances = snapshot
            .instances
            .iter()
            .filter(|config| config.enabled() && config.auto_apply)
            .map(|config| config.instance.clone())
            .collect::<BTreeSet<_>>();
        scheduler
            .auto_apply_errors
            .retain(|instance, _| auto_apply_instances.contains(instance));
        scheduler
            .auto_apply_warnings
            .retain(|instance, _| auto_apply_instances.contains(instance));
        for (instance, error) in &scheduler.auto_apply_errors {
            scheduler
                .errors
                .entry(instance.clone())
                .or_insert_with(|| error.clone());
        }
        for issue in &snapshot.issues {
            scheduler
                .errors
                .insert(issue.instance.clone(), issue.message.clone());
        }
        self.poll_native_scheduler_instances(
            scheduler,
            &snapshot.instances,
            configuration_committed,
            now_unix_s,
        )?;
        let rows = build_native_scheduler_status_rows(
            scheduler,
            &snapshot,
            now_unix_s,
            observed_monotonic,
            &calendar.day,
            &calendar.month,
        )?;
        scheduler.status_cache.publish(rows)?;
        Ok(())
    }

    fn poll_native_scheduler_instances(
        &mut self,
        scheduler: &mut NativeSchedulerRuntime,
        instances: &[ScheduledInstanceConfig],
        configuration_committed: bool,
        now_unix_s: u64,
    ) -> Result<(), String> {
        for config in instances {
            let instance = config.instance.clone();
            match self.poll_native_scheduler_instance(
                scheduler,
                config,
                configuration_committed,
                now_unix_s,
            ) {
                Ok(()) => {}
                Err(ScheduledInstancePassError::Instance(error)) => {
                    scheduler.errors.insert(instance.clone(), error);
                    scheduler.quiet.remove(&instance);
                    scheduler.waiting.remove(&instance);
                }
                Err(ScheduledInstancePassError::Global(error)) => return Err(error),
            }
        }
        Ok(())
    }

    fn poll_native_scheduler_instance(
        &mut self,
        scheduler: &mut NativeSchedulerRuntime,
        config: &ScheduledInstanceConfig,
        configuration_committed: bool,
        now_unix_s: u64,
    ) -> Result<(), ScheduledInstancePassError> {
        if !config.enabled() {
            scheduler.quiet.remove(&config.instance);
            return Ok(());
        }
        let instance_calendar =
            local_calendar(now_unix_s, config.window_start_hour, config.window_end_hour).map_err(
                |error| ScheduledInstancePassError::instance("scheduler calendar failed", error),
            )?;
        let mut persisted = load_or_initialize_state(
            &scheduler.store,
            config,
            now_unix_s,
            &instance_calendar.day,
            &instance_calendar.month,
        )
        .map_err(|error| {
            ScheduledInstancePassError::instance(
                "scheduler state load or initialization failed",
                error,
            )
        })?;
        if persisted.budget.reservation.is_some() || persisted.budget.accounting_blocked {
            scheduler.waiting.insert(
                config.instance.clone(),
                if persisted.budget.accounting_blocked {
                    "accounting-unavailable"
                } else {
                    "terminal-settlement-pending"
                },
            );
            return Ok(());
        }
        let due_unix_s = persisted
            .cursor
            .due_unix_s(config.interval_s)
            .map_err(|error| {
                ScheduledInstancePassError::instance("scheduler due time is invalid", error)
            })?;
        if due_unix_s > now_unix_s || !instance_calendar.window_open {
            scheduler.waiting.insert(
                config.instance.clone(),
                if instance_calendar.window_open {
                    "not-due"
                } else {
                    "window-closed"
                },
            );
            return Ok(());
        }
        let manual_job_pending = self.jobs.iter().any(|job| {
            !state::terminal(job.journal.state) && job.request.origin != OperationOrigin::Scheduler
        });
        let scheduled_job_active = self.jobs.iter().any(|job| {
            !state::terminal(job.journal.state)
                && job.request.origin == OperationOrigin::Scheduler
                && job.request.identity.instance == config.instance
        });
        let coordinator_idle = !self
            .jobs
            .iter()
            .any(|job| !state::terminal(job.journal.state));
        let runtime_path = scheduler_rating_runtime_path(&config.instance);
        let runtime = match rating::read_rating_snapshot(&runtime_path) {
            Ok(runtime) => runtime,
            Err(error) => {
                scheduler.errors.insert(
                    config.instance.clone(),
                    format!("scheduled runtime snapshot is unavailable: {error}"),
                );
                scheduler.quiet.remove(&config.instance);
                return Ok(());
            }
        };
        // A scheduled calibration owns generated load and the request's exact
        // test route. A healthy non-default member is intentionally STANDBY,
        // so default-route ownership must not be an admission requirement.
        let route_ready = runtime.route_test_ready;
        // Freshness belongs to the instant at which this snapshot has
        // actually been read, not the earlier calendar sample taken before
        // UCI/config attestation and reservation reconciliation.
        let runtime_observed_unix_ms =
            rating::epoch_ms().map_err(ScheduledInstancePassError::global)?;
        let (runtime_ready, quiet_window_ready) = scheduled_runtime_and_quiet_ready(
            &mut scheduler.quiet,
            &config.instance,
            &runtime,
            config.active_threshold_kbps,
            config.idle_window_s,
            runtime_observed_unix_ms,
        );
        let recovery_clear = self.admission_available();
        let waiting = if !configuration_committed {
            Some("configuration-pending")
        } else if !recovery_clear {
            Some("recovery-pending")
        } else if manual_job_pending {
            Some("manual-job-priority")
        } else if !coordinator_idle {
            Some("coordinator-busy")
        } else if !route_ready {
            Some("route-not-ready")
        } else if !runtime_ready {
            Some("runtime-not-ready")
        } else if !quiet_window_ready {
            Some("quiet-window-pending")
        } else if scheduled_job_active {
            Some("scheduled-job-active")
        } else {
            None
        };
        if let Some(waiting) = waiting {
            scheduler.waiting.insert(config.instance.clone(), waiting);
            return Ok(());
        }
        let available = persisted.budget.available_bytes().map_err(|error| {
            ScheduledInstancePassError::instance("scheduler budget is invalid", error)
        })?;
        if available == 0 {
            scheduler
                .waiting
                .insert(config.instance.clone(), "budget-exhausted");
            return Ok(());
        }
        if available < scheduled_minimum_traffic_budget_bytes(&runtime) {
            scheduler
                .waiting
                .insert(config.instance.clone(), "budget-insufficient");
            return Ok(());
        }
        attest_no_competing_calibration_processes(Path::new(DEFAULT_PROC_ROOT), std::process::id())
            .map_err(ScheduledInstancePassError::global)?;
        let intent = config.launch_intent(available).map_err(|error| {
            ScheduledInstancePassError::instance("scheduled launch intent is invalid", error)
        })?;
        let request =
            build_live_scheduled_autotune_request(&intent, config.auto_apply).map_err(|error| {
                ScheduledInstancePassError::instance("scheduled request attestation failed", error)
            })?;
        if request.identity.target_interface != config.expected_target_interface {
            return Err(ScheduledInstancePassError::instance(
                "scheduled request target changed",
                format!(
                    "expected {}, observed {}",
                    config.expected_target_interface, request.identity.target_interface
                ),
            ));
        }
        let observation = SchedulerObservation {
            now_unix_s,
            window_open: instance_calendar.window_open,
            next_window_open_unix_s: instance_calendar.next_window_open_unix_s,
            generations: SchedulerGenerations {
                config_fingerprint: request.identity.config_fingerprint.clone(),
                route_fingerprint: request.identity.route_fingerprint.clone(),
                runtime_sequence: runtime.runtime_generation,
                coordinator_generation: self.coordinator.generation.clone(),
            },
            explicit_retry_sequence: 0,
            gates: SchedulerGates {
                configuration_committed,
                recovery_clear,
                manual_job_pending,
                coordinator_idle,
                route_ready,
                runtime_ready,
                quiet_window_ready,
                accounting_healthy: !persisted.budget.accounting_blocked,
                scheduled_job_active,
            },
        };
        let reservation_id = kernel_request_id().map_err(ScheduledInstancePassError::global)?;
        let (_, reservation) = reserve_scheduled_request(
            &scheduler.store,
            config,
            &mut persisted,
            &observation,
            &request,
            reservation_id,
            &instance_calendar.day,
            &instance_calendar.month,
        )
        .map_err(|error| {
            ScheduledInstancePassError::instance("durable scheduler reservation failed", error)
        })?;
        let Some(reservation) = reservation else {
            return Ok(());
        };
        let message = ControlMessage {
            control: ControlRequest {
                request_id: kernel_request_id().map_err(ScheduledInstancePassError::global)?,
                command: ControlCommand::Start,
                job_id: Some(request.identity.job_id.clone()),
                job_token: Some(request.identity.job_token.clone()),
            },
            operation: Some(request.clone()),
        };
        let response = self.start_job(&message);
        if !self
            .jobs
            .iter()
            .any(|job| job.journal.job_id == request.identity.job_id)
        {
            settle_scheduled_request(
                &scheduler.store,
                &mut persisted,
                &reservation.reservation_id,
                &reservation.job_id,
                &instance_calendar.day,
                &instance_calendar.month,
                now_unix_s,
                0,
                ScheduledSettlement::Failed,
            )
            .map_err(|error| {
                ScheduledInstancePassError::instance(
                    "durable failed-admission settlement failed",
                    error,
                )
            })?;
            scheduler.errors.insert(
                config.instance.clone(),
                format!(
                    "scheduled coordinator admission failed: {}",
                    response.trim()
                ),
            );
            return Ok(());
        }
        scheduler.quiet.remove(&config.instance);
        scheduler.waiting.remove(&config.instance);
        scheduler.errors.remove(&config.instance);
        Ok(())
    }

    fn reconcile_native_scheduler_reservations(
        &mut self,
        scheduler: &mut NativeSchedulerRuntime,
        now_unix_s: u64,
        day: &str,
        month: &str,
    ) -> Result<(), String> {
        for instance in scheduler.store.instances()? {
            if let Err(error) = self.reconcile_native_scheduler_reservation(
                scheduler, &instance, now_unix_s, day, month,
            ) {
                scheduler.errors.insert(
                    instance,
                    format!("scheduled reservation reconciliation failed: {error}"),
                );
            }
        }
        Ok(())
    }

    fn reconcile_native_scheduler_reservation(
        &mut self,
        scheduler: &mut NativeSchedulerRuntime,
        instance: &str,
        now_unix_s: u64,
        day: &str,
        month: &str,
    ) -> Result<(), String> {
        self.reconcile_native_scheduler_reservation_with(
            scheduler,
            instance,
            now_unix_s,
            day,
            month,
            execute_native_scheduled_auto_apply,
        )
    }

    fn reconcile_native_scheduler_reservation_with(
        &mut self,
        scheduler: &mut NativeSchedulerRuntime,
        instance: &str,
        now_unix_s: u64,
        day: &str,
        month: &str,
        auto_apply: NativeScheduledAutoApplyExecutor,
    ) -> Result<(), String> {
        let Some(mut persisted) = scheduler.store.load_state(instance)? else {
            return Err(format!(
                "native scheduler instance {instance} disappeared during enumeration"
            ));
        };
        let Some(reservation) = persisted.budget.reservation.clone() else {
            return Ok(());
        };
        if persisted.budget.accounting_blocked {
            scheduler
                .accounting_blocks
                .insert(instance.to_string(), reservation.reserved_bytes);
        }
        let Some(index) = self
            .jobs
            .iter()
            .position(|job| job.journal.job_id == reservation.job_id)
        else {
            if !persisted.budget.accounting_blocked {
                mark_scheduled_accounting_unknown(
                    &scheduler.store,
                    &mut persisted,
                    &reservation.reservation_id,
                    &reservation.job_id,
                )?;
                scheduler
                    .accounting_blocks
                    .insert(instance.to_string(), reservation.reserved_bytes);
            }
            scheduler.errors.insert(
                instance.to_string(),
                "reserved scheduled job is absent; full reservation retained".to_string(),
            );
            return Ok(());
        };
        if !state::terminal(self.jobs[index].journal.state) {
            if persisted.budget.accounting_blocked {
                scheduler.errors.insert(
                    instance.to_string(),
                    "scheduled traffic accounting remains blocked until exact terminal evidence is available"
                        .to_string(),
                );
            }
            return Ok(());
        }
        // A prior missing/invalid read keeps the full reservation and blocks
        // new traffic, but it must not make later exact evidence unreachable.
        // Settlement validates the same reservation/job identity before it can
        // clear the conservative block.
        match self.exact_scheduled_terminal_traffic(index, &reservation.job_id) {
            Ok((consumed, settlement)) => {
                let mut auto_apply_error = None;
                persisted.operator_warning = None;
                if settlement == ScheduledSettlement::Success {
                    match auto_apply(&self.state_dir, &reservation.job_id) {
                        Ok(NativeScheduledAutoApplyOutcome::NotRequested) => {
                            scheduler.auto_apply_errors.remove(instance);
                            scheduler.auto_apply_warnings.remove(instance);
                        }
                        Ok(NativeScheduledAutoApplyOutcome::ReviewRequired) => {
                            let message = "scheduled Auto-Apply skipped: the preferred result requires explicit Review or changes SQM direction ownership".to_string();
                            scheduler.auto_apply_errors.remove(instance);
                            scheduler
                                .auto_apply_warnings
                                .insert(instance.to_string(), message.clone());
                            persisted.operator_warning = Some(message);
                        }
                        Ok(NativeScheduledAutoApplyOutcome::Applied(disposition)) => {
                            scheduler.auto_apply_errors.remove(instance);
                            scheduler.auto_apply_warnings.remove(instance);
                            eprintln!(
                                "native scheduled Auto-Apply {} for instance {instance}",
                                match disposition {
                                    NativeApplyCommitDisposition::Applied => "committed",
                                    NativeApplyCommitDisposition::AlreadyApplied => {
                                        "replayed an already committed candidate"
                                    }
                                }
                            );
                        }
                        Err(error) => {
                            let message = format!("scheduled Auto-Apply failed: {error}");
                            scheduler.auto_apply_warnings.remove(instance);
                            scheduler
                                .auto_apply_errors
                                .insert(instance.to_string(), message.clone());
                            auto_apply_error = Some(message);
                            // The immutable measurement terminal owns the
                            // scheduler cursor.  A later Apply failure is a
                            // distinct sticky health/recovery condition and
                            // must not turn a successful calibration into a
                            // failed-attempt fence or schedule an early rerun.
                            match native_apply_recovery_marker_present() {
                                Ok(false) => {}
                                Ok(true) => self.startup_issues.push(format!(
                                    "native Apply recovery remains pending after scheduled job {}",
                                    reservation.job_id
                                )),
                                Err(marker_error) => self.startup_issues.push(marker_error),
                            }
                        }
                    }
                }
                settle_scheduled_request(
                    &scheduler.store,
                    &mut persisted,
                    &reservation.reservation_id,
                    &reservation.job_id,
                    day,
                    month,
                    now_unix_s,
                    consumed,
                    settlement,
                )?;
                if let Some(error) = auto_apply_error {
                    scheduler.errors.insert(instance.to_string(), error);
                } else {
                    scheduler.errors.remove(instance);
                }
                scheduler.accounting_blocks.remove(instance);
            }
            Err(error) => {
                if !persisted.budget.accounting_blocked {
                    mark_scheduled_accounting_unknown(
                        &scheduler.store,
                        &mut persisted,
                        &reservation.reservation_id,
                        &reservation.job_id,
                    )?;
                }
                scheduler.errors.insert(instance.to_string(), error);
            }
        }
        Ok(())
    }

    fn exact_scheduled_terminal_traffic(
        &self,
        index: usize,
        expected_job_id: &str,
    ) -> Result<(u64, ScheduledSettlement), String> {
        let job = &self.jobs[index];
        if job.request.origin != OperationOrigin::Scheduler
            || job.request.identity.job_id != expected_job_id
            || job.request.identity.operation != OperationKind::FullAutotune
        {
            return Err("scheduled terminal job identity is not trustworthy".to_string());
        }
        let worker_run_id = job
            .journal
            .worker_run_id
            .as_deref()
            .ok_or_else(|| "scheduled terminal has no worker run identity".to_string())?;
        let paths = self
            .journal_store
            .native_job_paths(expected_job_id, worker_run_id)?;
        let terminal = full_autotune::read_terminal_file(&paths.terminal)?;
        if terminal.job_id != expected_job_id || terminal.worker_run_id != worker_run_id {
            return Err("scheduled terminal identity changed".to_string());
        }
        let consumed = full_autotune::verify_terminal_consumed_traffic(
            &paths.request,
            expected_job_id,
            worker_run_id,
            terminal.consumed_traffic_bytes,
        )?;
        let settlement = if matches!(terminal.terminal, AutotuneTerminal::Complete { .. })
            && matches!(
                job.journal.state,
                super::protocol::OperationState::ReviewReady
                    | super::protocol::OperationState::Completed
            ) {
            ScheduledSettlement::Success
        } else {
            ScheduledSettlement::Failed
        };
        Ok((consumed, settlement))
    }

    fn poll_native_children(&mut self) {
        let mut errors = Vec::new();
        for (job_id, child) in &mut self.native_children {
            if let Err(error) = child.try_wait() {
                errors.push((job_id.clone(), error));
            }
        }
        for (job_id, error) in errors {
            self.job_errors.insert(
                job_id,
                format!("unable to reap native operation worker: {error}"),
            );
        }
    }

    fn poll_bootstrap_runtime_owners(&mut self) {
        let indices: Vec<usize> = self
            .jobs
            .iter()
            .enumerate()
            .filter_map(|(index, job)| {
                (job.request.target_state == OperationTargetState::AbsentBootstrap
                    && request_requires_native_runtime(&job.request)
                    && !state::terminal(job.journal.state)
                    && matches!(
                        job.journal.state,
                        super::protocol::OperationState::Running
                            | super::protocol::OperationState::Recovering
                    ))
                .then_some(index)
            })
            .collect();
        for index in indices {
            self.advance_bootstrap_runtime_owner(index);
        }
    }

    fn advance_bootstrap_runtime_owner(&mut self, index: usize) {
        let request = self.jobs[index].request.clone();
        let job_id = request.identity.job_id.clone();
        let Some(worker_run_id) = self.jobs[index].journal.worker_run_id.clone() else {
            self.job_errors.insert(
                job_id,
                "bootstrap runtime owner has no worker run identity".to_string(),
            );
            return;
        };
        let paths = match self.journal_store.native_job_paths(&job_id, &worker_run_id) {
            Ok(paths) => paths,
            Err(error) => {
                self.job_errors.insert(job_id, error);
                return;
            }
        };

        if let Some(owner) = self.jobs[index].journal.runtime_owner_process.clone() {
            // A replacement launched during recovery deliberately differs
            // from the dead identity still recorded in the durable journal.
            // The direct-child helper must stay strict for cancellation, but
            // using it against that stale identity here prevents the new
            // exact claim from ever being read.  Re-attest the prior owner
            // directly until it is dead, then durably detach it before the
            // replacement is allowed to attach.
            let replacement_pending = self
                .bootstrap_runtime_children
                .get(&job_id)
                .is_some_and(|child| child.identity != owner);
            let owner_live = if replacement_pending {
                owner.still_matches(Path::new(DEFAULT_PROC_ROOT))
            } else {
                match self.coordinator_owned_bootstrap_runtime_owner_live(&job_id, Some(&owner)) {
                    Ok(Some(live)) => Ok(live),
                    Ok(None) => owner.still_matches(Path::new(DEFAULT_PROC_ROOT)),
                    Err(error) => Err(error),
                }
            };
            match owner_live {
                Ok(true) => {
                    if replacement_pending {
                        self.job_errors.insert(
                            job_id,
                            "replacement bootstrap runtime owner cannot attach while the prior owner is still live"
                                .to_string(),
                        );
                    }
                    return;
                }
                Err(error) => {
                    self.job_errors.insert(
                        job_id,
                        format!("unable to attest bootstrap runtime owner identity: {error}"),
                    );
                    return;
                }
                Ok(false) => {
                    if self.jobs[index].journal.runtime_mutated
                        && self.jobs[index].journal.state
                            != super::protocol::OperationState::Recovering
                    {
                        self.mark_recovery_required_with_code(
                            index,
                            "bootstrap-runtime-owner-disappeared",
                            "bootstrap runtime owner exited before exact restoration",
                        );
                        return;
                    }
                    let mut detached = self.jobs[index].journal.clone();
                    if let Err(error) = detached
                        .clear_bootstrap_runtime_owner()
                        .and_then(|_| self.journal_store.update(&detached))
                    {
                        self.job_errors.insert(job_id, error);
                        return;
                    }
                    self.jobs[index].journal = detached;
                    self.job_errors.remove(&job_id);
                }
            }
        }

        if self.jobs[index].journal.runtime_owner_process.is_none()
            && self.jobs[index].journal.state == super::protocol::OperationState::Recovering
            && self.jobs[index].journal.runtime_mutated
        {
            match self.bootstrap_no_owner_recovery_readiness(index, &paths, &worker_run_id) {
                Ok(BootstrapNoOwnerRecoveryReadiness::OwnerExitPending(owner)) => {
                    let direct_child_reaped =
                        if let Some(child) = self.bootstrap_runtime_children.get_mut(&job_id) {
                            if child.identity != owner {
                                self.job_errors.insert(
                                job_id,
                                "owner-free bootstrap recovery claim differs from its direct child"
                                    .to_string(),
                            );
                                return;
                            }
                            match child.try_wait() {
                                Ok(Some(_)) => true,
                                Ok(None) => false,
                                Err(error) => {
                                    self.job_errors.insert(
                                        job_id,
                                        format!(
                                        "unable to reap restored bootstrap runtime owner: {error}"
                                    ),
                                    );
                                    return;
                                }
                            }
                        } else {
                            false
                        };
                    if direct_child_reaped {
                        self.bootstrap_runtime_children.remove(&job_id);
                        self.job_errors.remove(&job_id);
                        return;
                    }
                    match signal_adopted_group(&owner, Path::new(DEFAULT_PROC_ROOT), 15) {
                        Ok(_) => {
                            self.job_errors.insert(
                                job_id,
                                "bootstrap runtime baseline is restored; waiting for the read-only owner to exit"
                                    .to_string(),
                            );
                        }
                        Err(error) => {
                            self.job_errors.insert(
                                job_id,
                                format!("unable to stop restored bootstrap runtime owner: {error}"),
                            );
                        }
                    }
                    return;
                }
                Ok(
                    BootstrapNoOwnerRecoveryReadiness::Restored
                    | BootstrapNoOwnerRecoveryReadiness::RestoredFinalizationPending,
                ) => {
                    if let Some(child) = self.bootstrap_runtime_children.get_mut(&job_id) {
                        match child.try_wait() {
                            Ok(Some(_)) => {}
                            Ok(None) => {
                                self.job_errors.insert(
                                    job_id,
                                    "restored bootstrap runtime owner still has a live direct child"
                                        .to_string(),
                                );
                                return;
                            }
                            Err(error) => {
                                self.job_errors.insert(
                                    job_id,
                                    format!(
                                        "unable to reap restored bootstrap runtime owner: {error}"
                                    ),
                                );
                                return;
                            }
                        }
                    }
                    self.bootstrap_runtime_children.remove(&job_id);
                    self.job_errors.remove(&job_id);
                    return;
                }
                Ok(BootstrapNoOwnerRecoveryReadiness::OwnerRequired) => {}
                Err(error) => {
                    self.job_errors.insert(
                        job_id,
                        format!("unable to attest owner-free bootstrap recovery: {error}"),
                    );
                    return;
                }
            }
        }

        if let Some(child) = self.bootstrap_runtime_children.get_mut(&job_id) {
            match child.try_wait() {
                Ok(Some(_)) => {
                    self.bootstrap_runtime_children.remove(&job_id);
                    if self.jobs[index].journal.runtime_mutated {
                        self.mark_recovery_required_with_code(
                            index,
                            "bootstrap-runtime-owner-launch-exited",
                            "replacement bootstrap runtime owner exited before attachment",
                        );
                    } else {
                        self.stop_parked_runtime_worker(
                            index,
                            "bootstrap-runtime-owner-launch-exited",
                            "bootstrap runtime owner exited before attachment",
                        );
                    }
                    return;
                }
                Ok(None) => {}
                Err(error) => {
                    self.job_errors.insert(
                        job_id,
                        format!("unable to reap bootstrap runtime owner: {error}"),
                    );
                    return;
                }
            }
            let expected = child.identity.clone();
            match read_bootstrap_runtime_owner_claim(
                &paths.bootstrap_runtime_dir,
                &request,
                &worker_run_id,
            ) {
                Ok(Some(claim)) if claim.process == expected => {
                    self.attach_bootstrap_runtime_owner_identity(index, claim.process);
                }
                Ok(Some(_)) => {
                    self.job_errors.insert(
                        job_id,
                        "bootstrap runtime owner claim differs from the launched process"
                            .to_string(),
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    self.job_errors.insert(job_id, error);
                }
            }
            return;
        }

        match read_bootstrap_runtime_owner_claim(
            &paths.bootstrap_runtime_dir,
            &request,
            &worker_run_id,
        ) {
            Ok(Some(claim)) => match claim.process.still_matches(Path::new(DEFAULT_PROC_ROOT)) {
                Ok(true) => {
                    self.attach_bootstrap_runtime_owner_identity(index, claim.process);
                    return;
                }
                Ok(false) => {}
                Err(error) => {
                    self.job_errors.insert(
                        job_id,
                        format!("unable to inspect claimed bootstrap runtime owner: {error}"),
                    );
                    return;
                }
            },
            Ok(None) => {}
            Err(error) => {
                self.job_errors.insert(job_id, error);
                return;
            }
        }

        if let Err(error) = self.spawn_bootstrap_runtime_owner(&job_id, &paths, &worker_run_id) {
            if self.jobs[index].journal.runtime_mutated {
                self.job_errors.insert(
                    job_id,
                    format!("unable to respawn bootstrap runtime owner: {error}"),
                );
            } else {
                self.stop_parked_runtime_worker(
                    index,
                    "bootstrap-runtime-owner-spawn-failed",
                    &error,
                );
            }
        }
    }

    fn attach_bootstrap_runtime_owner_identity(&mut self, index: usize, process: ProcessIdentity) {
        let job_id = self.jobs[index].journal.job_id.clone();
        let mut attached = self.jobs[index].journal.clone();
        let transition = if attached.runtime_mutated {
            attached.replace_bootstrap_runtime_owner_during_recovery(process)
        } else {
            attached.attach_bootstrap_runtime_owner(process)
        };
        if let Err(error) = transition.and_then(|_| self.journal_store.update(&attached)) {
            self.job_errors.insert(
                job_id,
                format!("unable to durably attach bootstrap runtime owner: {error}"),
            );
            return;
        }
        self.jobs[index].journal = attached;
        self.job_errors.remove(&job_id);
    }

    fn spawn_bootstrap_runtime_owner(
        &mut self,
        job_id: &str,
        paths: &NativeJobPaths,
        worker_run_id: &str,
    ) -> Result<(), String> {
        if paths
            .request
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            != Some(job_id)
        {
            return Err("bootstrap runtime owner paths differ from the exact journal".to_string());
        }
        if self.bootstrap_runtime_children.contains_key(job_id) {
            return Err("bootstrap runtime owner launch is already pending".to_string());
        }
        let program = env::current_exe()
            .map_err(|error| format!("bootstrap runtime owner program is unavailable: {error}"))?;
        if !program.is_absolute() {
            return Err("bootstrap runtime owner program path is not absolute".to_string());
        }
        let stdout = private_runtime_owner_log_file(&paths.bootstrap_runtime_stdout)?;
        let stderr = private_runtime_owner_log_file(&paths.bootstrap_runtime_stderr)?;
        let spec = SpawnSpec {
            program,
            arguments: bootstrap_runtime_owner_arguments(paths, worker_run_id),
            environment: Vec::new(),
        };
        let mut child = ManagedChild::spawn(&spec, stdout, stderr, Path::new(DEFAULT_PROC_ROOT))?;
        child.preserve_on_drop();
        self.bootstrap_runtime_children
            .insert(job_id.to_string(), child);
        Ok(())
    }

    fn poll_native_runtime_recoveries(&mut self) {
        let indices: Vec<usize> = self
            .jobs
            .iter()
            .enumerate()
            .filter_map(|(index, job)| {
                (request_requires_native_runtime(&job.request)
                    && self.supports_native_request(&job.request)
                    && job.journal.runtime_mutated
                    && job.journal.state == super::protocol::OperationState::Recovering)
                    .then_some(index)
            })
            .collect();
        for index in indices {
            let job_id = self.jobs[index].journal.job_id.clone();
            let worker_run_id = match self.jobs[index].journal.worker_run_id.clone() {
                Some(value) => value,
                None => {
                    self.job_errors.insert(
                        job_id,
                        "native runtime recovery has no worker identity".to_string(),
                    );
                    continue;
                }
            };
            if let Err(error) = cleanup_native_route_pin(
                &self.jobs[index].request,
                &worker_run_id,
                self.route_pin_cleaner,
            ) {
                self.job_errors.insert(
                    job_id,
                    format!("unable to clean exact native speedtest route pin: {error}"),
                );
                continue;
            }
            let owner_free_bootstrap_store = if self.jobs[index].request.target_state
                == OperationTargetState::AbsentBootstrap
                && self.jobs[index].journal.runtime_owner_process.is_none()
            {
                let paths = match self.journal_store.native_job_paths(&job_id, &worker_run_id) {
                    Ok(paths) => paths,
                    Err(error) => {
                        self.job_errors.insert(job_id, error);
                        continue;
                    }
                };
                match self.bootstrap_no_owner_recovery_readiness(index, &paths, &worker_run_id) {
                    Ok(
                        readiness @ (BootstrapNoOwnerRecoveryReadiness::Restored
                        | BootstrapNoOwnerRecoveryReadiness::RestoredFinalizationPending),
                    ) => match RuntimeOverrideStore::open(&paths.bootstrap_runtime_dir) {
                        Ok(store) => {
                            if readiness
                                == BootstrapNoOwnerRecoveryReadiness::RestoredFinalizationPending
                            {
                                if let Err(error) = store.finalize_restored() {
                                    self.job_errors.insert(job_id, error);
                                    continue;
                                }
                            }
                            Some(store)
                        }
                        Err(error) => {
                            self.job_errors.insert(job_id, error);
                            continue;
                        }
                    },
                    Ok(BootstrapNoOwnerRecoveryReadiness::OwnerRequired)
                    | Ok(BootstrapNoOwnerRecoveryReadiness::OwnerExitPending(_)) => continue,
                    Err(error) => {
                        self.job_errors.insert(
                            job_id,
                            format!("unable to attest owner-free bootstrap recovery: {error}"),
                        );
                        continue;
                    }
                }
            } else {
                None
            };
            let store = match owner_free_bootstrap_store {
                Some(store) => store,
                None => match self.runtime_store_for_request(&self.jobs[index].request) {
                    Ok(store) => store,
                    Err(error) => {
                        self.job_errors.insert(job_id, error);
                        continue;
                    }
                },
            };
            let permit = match store.read_permit() {
                Ok(value) => value,
                Err(error) => {
                    self.job_errors.insert(job_id, error);
                    continue;
                }
            };
            let control = match store.read_control() {
                Ok(value) => value,
                Err(error) => {
                    self.job_errors.insert(job_id, error);
                    continue;
                }
            };
            let restore_intent = match store.read_restore_intent() {
                Ok(value) => value,
                Err(error) => {
                    self.job_errors.insert(job_id, error);
                    continue;
                }
            };
            let ack = match store.read_ack() {
                Ok(value) => value,
                Err(error) => {
                    self.job_errors.insert(job_id, error);
                    continue;
                }
            };
            let checkpoint = match store.read_checkpoint() {
                Ok(value) => value,
                Err(error) => {
                    self.job_errors.insert(job_id, error);
                    continue;
                }
            };
            if let Some(ack) = ack.as_ref() {
                if ack.job_id != job_id || ack.worker_run_id != worker_run_id {
                    self.job_errors.insert(
                        job_id,
                        "native runtime recovery ACK identity mismatch".to_string(),
                    );
                    continue;
                }
                match ack.state {
                    RuntimeAckState::Restored => {
                        if let Some(permit) = permit.as_ref() {
                            let expected_control = match (control.as_ref(), restore_intent.as_ref())
                            {
                                (Some(control), Some(intent)) if control != intent => {
                                    self.job_errors.insert(
                                        job_id,
                                        "native runtime recovery control and restore intent differ"
                                            .to_string(),
                                    );
                                    continue;
                                }
                                (Some(control), _) => control,
                                (None, Some(intent)) => intent,
                                (None, None) => {
                                    self.job_errors.insert(
                                        job_id,
                                        "native runtime recovery has no exact restored control"
                                            .to_string(),
                                    );
                                    continue;
                                }
                            };
                            let now = match monotonic_boot_ms() {
                                Ok(value) => value,
                                Err(error) => {
                                    self.job_errors.insert(job_id, error);
                                    continue;
                                }
                            };
                            if let Err(error) =
                                store.withdraw_restored_request(permit, expected_control, now)
                            {
                                self.job_errors.insert(job_id, error);
                            }
                            continue;
                        }
                        if control.is_some() {
                            self.job_errors.insert(
                                job_id,
                                "native runtime recovery control outlived its permit".to_string(),
                            );
                            continue;
                        }
                        // Only the instance releases its tracker owner and
                        // removes checkpoint+ACK after request withdrawal.
                        continue;
                    }
                    RuntimeAckState::Rejected if checkpoint.is_none() => {
                        if let Err(error) = store.clear() {
                            self.job_errors.insert(job_id, error);
                            continue;
                        }
                    }
                    RuntimeAckState::Applied => {
                        // The instance daemon owns restoration.  Once the
                        // identity-bound worker has exited, its next runtime
                        // poll observes that exact liveness failure, restores
                        // the checkpointed baseline, and publishes RESTORED.
                        // Withdrawing here would race that attestation and
                        // bypass the instance-owned restore transaction.
                        continue;
                    }
                    RuntimeAckState::Restoring => {
                        match self.maybe_release_heavy_lease_during_recovery(index, ack) {
                            Ok(_) => {}
                            Err(error) => {
                                self.job_errors.insert(job_id, error);
                            }
                        }
                        continue;
                    }
                    RuntimeAckState::Rejected => continue,
                }
            }
            if checkpoint.is_none() && control.is_none() {
                if permit.is_some() {
                    if let Err(error) = store.withdraw_request() {
                        self.job_errors.insert(job_id, error);
                        continue;
                    }
                }
                if let Err(error) = store.clear() {
                    self.job_errors.insert(job_id, error);
                    continue;
                }
            }
            if ensure_runtime_store_idle(&store).is_ok() {
                if self.jobs[index].request.identity.operation == OperationKind::FullAutotune {
                    if let Err(error) = full_autotune::cleanup_restored_capture(
                        store.instance_run_dir(),
                        &job_id,
                        &worker_run_id,
                    ) {
                        self.job_errors.insert(job_id, error);
                        continue;
                    }
                }
                self.settle_native_runtime_recovery(index);
            }
        }
    }

    fn settle_native_runtime_recovery(&mut self, index: usize) {
        let job_id = self.jobs[index].journal.job_id.clone();
        let worker_run_id = self.jobs[index].journal.worker_run_id.clone();
        if let Some(worker_run_id) = worker_run_id.as_deref() {
            if let Err(error) = cleanup_native_route_pin(
                &self.jobs[index].request,
                worker_run_id,
                self.route_pin_cleaner,
            ) {
                self.job_errors.insert(
                    job_id,
                    format!("unable to verify exact native speedtest route-pin cleanup: {error}"),
                );
                return;
            }
        }
        let recovery_diagnostic = self.jobs[index].journal.diagnostic_code.clone();
        let terminal = worker_run_id
            .as_deref()
            .and_then(|worker_run_id| {
                self.journal_store
                    .native_job_paths(&job_id, worker_run_id)
                    .ok()
                    .map(|paths| (worker_run_id, paths))
            })
            .map(
                |(worker_run_id, paths)| match self.jobs[index].request.identity.operation {
                    OperationKind::FullAutotune => native_autotune_terminal_outcome(
                        &paths,
                        &job_id,
                        worker_run_id,
                        &self.jobs[index].journal.boot_id,
                        &self.jobs[index].journal.coordinator_generation,
                    ),
                    OperationKind::Speedtest => {
                        native_speedtest_runtime_terminal_outcome(&paths, &job_id, worker_run_id)
                    }
                    _ => Err("unsupported native runtime terminal operation".to_string()),
                },
            )
            .transpose();
        let terminal = match terminal {
            Ok(Some(Some(terminal))) => terminal,
            Ok(Some(None)) | Ok(None) => {
                let (cancelled, diagnostic) =
                    native_runtime_terminal_diagnostic(recovery_diagnostic.as_deref());
                NativeRuntimeTerminalOutcome {
                    state: if cancelled { "cancelled" } else { "failed" },
                    diagnostic: Some(diagnostic.to_string()),
                }
            }
            Err(error) => {
                self.job_errors.insert(job_id.clone(), error);
                NativeRuntimeTerminalOutcome {
                    state: "failed",
                    diagnostic: Some("native-terminal-inspection-failed".to_string()),
                }
            }
        };
        let mut settled = self.jobs[index].journal.clone();
        let settle_result = match self.jobs[index].request.identity.operation {
            OperationKind::FullAutotune => settled
                .settle_native_runtime_terminal(terminal.state, terminal.diagnostic.as_deref()),
            OperationKind::Speedtest => settled.settle_native_speedtest_runtime_terminal(
                terminal.state,
                terminal.diagnostic.as_deref(),
            ),
            _ => Err("unsupported native runtime settlement operation".to_string()),
        };
        if let Err(error) = settle_result.and_then(|_| self.journal_store.update(&settled)) {
            self.job_errors.insert(job_id, error);
            return;
        }
        self.jobs[index].journal = settled;
        self.jobs[index].disposition = JournalDisposition::Settled;
        self.native_children.remove(&job_id);
        self.cancellations.remove(&job_id);
        if let Some(worker_run_id) = worker_run_id {
            if let Ok(paths) = self.journal_store.native_job_paths(&job_id, &worker_run_id) {
                let _ = fs::remove_file(paths.permit);
            }
        }
        if let Err(error) = self.leases.release(&job_id) {
            self.startup_issues.push(format!(
                "unable to release native runtime operation leases for {job_id}: {error}"
            ));
        }
        if terminal.diagnostic.is_none()
            && matches!(
                self.jobs[index].journal.state,
                super::protocol::OperationState::ReviewReady
                    | super::protocol::OperationState::Completed
                    | super::protocol::OperationState::Cancelled
            )
        {
            self.job_errors.remove(&job_id);
        } else if let Some(diagnostic) = terminal.diagnostic.as_deref() {
            let message =
                format!("native runtime operation failed after exact restoration: {diagnostic}");
            eprintln!("native calibration terminal failure for job {job_id}: {message}");
            self.job_errors.insert(job_id, message);
        } else {
            let message = "native runtime operation reached an unexpected settlement state after exact restoration: native-runtime-settlement-state-unexpected".to_string();
            eprintln!("native calibration terminal failure for job {job_id}: {message}");
            self.job_errors.insert(job_id, message);
        }
    }

    fn maybe_release_heavy_lease_during_recovery(
        &mut self,
        index: usize,
        ack: &super::full_autotune::AutotuneRuntimeAck,
    ) -> Result<bool, String> {
        let journal = &self.jobs[index].journal;
        if !journal.heavy_lease_acquired {
            return Ok(false);
        }
        if ack.state != RuntimeAckState::Restoring
            || ack.job_id != journal.job_id
            || journal.worker_run_id.as_deref() != Some(ack.worker_run_id.as_str())
        {
            return Err("runtime restore ACK does not match the heavy-lease owner".to_string());
        }
        let job_id = journal.job_id.clone();
        let mut staged_leases = self.leases.clone();
        staged_leases.release_heavy(&job_id)?;
        let mut staged_journal = journal.clone();
        staged_journal.release_heavy_lease_during_recovery()?;
        self.journal_store.update(&staged_journal)?;

        // The durable journal is authoritative after a coordinator restart.
        // Commit the matching in-memory lease table only after that write.
        self.leases = staged_leases;
        self.jobs[index].journal = staged_journal;
        self.job_errors.insert(
            job_id,
            "runtime restoration is still pending; global heavy traffic is available to other uplinks while local recovery locks remain held".to_string(),
        );
        Ok(true)
    }

    fn supports_native_request(&self, request: &OperationRequest) -> bool {
        match request.identity.operation {
            OperationKind::GuidedRating | OperationKind::AutomaticRating => {
                self.native_rating && request.target_state == OperationTargetState::ExistingManaged
            }
            OperationKind::Speedtest => {
                self.native_speedtest
                    && request.backend == "speedtest-go"
                    && matches!(
                        request.route.mode,
                        OperationRouteMode::Main | OperationRouteMode::Mwan3
                    )
                    && match request.target_state {
                        OperationTargetState::ExistingManaged => true,
                        OperationTargetState::AbsentBootstrap => {
                            request.validate_admission_policy().is_ok()
                        }
                    }
            }
            OperationKind::FullAutotune => {
                self.native_autotune
                    && request.backend == "speedtest-go"
                    && matches!(
                        request.route.mode,
                        OperationRouteMode::Main | OperationRouteMode::Mwan3
                    )
                    && match request.target_state {
                        OperationTargetState::ExistingManaged => true,
                        OperationTargetState::AbsentBootstrap => {
                            request.validate_admission_policy().is_ok()
                        }
                    }
            }
        }
    }

    fn resume_native_workers(&mut self) {
        let indices: Vec<usize> = self
            .jobs
            .iter()
            .enumerate()
            .filter_map(|(index, job)| {
                (job.disposition == JournalDisposition::LiveProcess
                    && !job.journal.runtime_mutated
                    && self.supports_native_request(&job.request))
                .then_some(index)
            })
            .collect();
        for index in indices {
            if self.jobs[index].journal.state == super::protocol::OperationState::Cancelling {
                self.begin_live_cancellation(index);
                continue;
            }
            let job_id = self.jobs[index].journal.job_id.clone();
            let Some(worker_run_id) = self.jobs[index].journal.worker_run_id.clone() else {
                self.job_errors.insert(
                    job_id,
                    "native worker has no durable run identity".to_string(),
                );
                continue;
            };
            if request_requires_native_runtime(&self.jobs[index].request) {
                // The child is still parked.  Its exact process identity is in
                // the durable journal; the ordinary tick will re-attest current
                // instance state and publish both permits in order.  Restart
                // never grants a generic permit merely because time passed.
                self.job_errors.insert(
                    job_id,
                    "runtime-waiting: re-attesting instance runtime for the adopted worker"
                        .to_string(),
                );
                continue;
            }
            let permit = match rating::encode_permit(&job_id, &worker_run_id) {
                Ok(permit) => permit,
                Err(error) => {
                    self.job_errors.insert(job_id, error);
                    continue;
                }
            };
            if let Err(error) =
                self.journal_store
                    .publish_native_permit(&job_id, &worker_run_id, &permit)
            {
                self.job_errors.insert(
                    job_id.clone(),
                    format!("unable to resume native worker permit: {error}"),
                );
                if let Some(process) = self.jobs[index].journal.process.as_ref() {
                    let _ = signal_adopted_group(process, Path::new(DEFAULT_PROC_ROOT), 15);
                }
            }
        }
    }

    fn dispatch_native_worker(&mut self, index: usize) {
        let request = self.jobs[index].request.clone();
        let job_id = request.identity.job_id.clone();
        let worker_kind = match request.identity.operation {
            OperationKind::GuidedRating | OperationKind::AutomaticRating if self.native_rating => {
                NativeWorkerKind::Rating
            }
            OperationKind::Speedtest if self.native_speedtest => NativeWorkerKind::Speedtest,
            OperationKind::FullAutotune if self.supports_native_request(&request) => {
                NativeWorkerKind::Autotune
            }
            _ => {
                self.fail_queued_before_mutation(
                    index,
                    "native-operation-unsupported",
                    "no native worker is enabled for the requested operation",
                );
                return;
            }
        };
        let worker_run_id = match kernel_request_id() {
            Ok(value) => value,
            Err(error) => {
                self.fail_queued_before_mutation(index, "worker-run-id-unavailable", &error);
                return;
            }
        };
        let paths = match self.journal_store.native_job_paths(&job_id, &worker_run_id) {
            Ok(paths) => paths,
            Err(error) => {
                self.fail_queued_before_mutation(index, "native-path-invalid", &error);
                return;
            }
        };
        let program = match env::current_exe() {
            Ok(program) if program.is_absolute() => program,
            Ok(_) => {
                self.fail_queued_before_mutation(
                    index,
                    "native-program-invalid",
                    "current executable path is not absolute",
                );
                return;
            }
            Err(error) => {
                self.fail_queued_before_mutation(
                    index,
                    "native-program-unavailable",
                    &error.to_string(),
                );
                return;
            }
        };
        let stdout = match private_log_file(&paths.stdout) {
            Ok(file) => file,
            Err(error) => {
                self.fail_queued_before_mutation(index, "native-log-create-failed", &error);
                return;
            }
        };
        let stderr = match private_log_file(&paths.stderr) {
            Ok(file) => file,
            Err(error) => {
                self.fail_queued_before_mutation(index, "native-log-create-failed", &error);
                return;
            }
        };
        let spec = SpawnSpec {
            program,
            arguments: worker_kind.arguments(&paths, &worker_run_id),
            environment: Vec::new(),
        };
        let runtime_owning = request_requires_native_runtime(&request);
        let mut armed = self.jobs[index].journal.clone();
        // Every native worker starts parked and explicitly non-mutating.  A
        // A runtime-owning journal is armed for possible runtime mutation only
        // after its exact attached child and a fresh runtime snapshot have
        // both been attested.
        let arm_result = armed.arm_native_worker(worker_run_id.clone());
        if let Err(error) = arm_result.and_then(|_| self.journal_store.update(&armed)) {
            self.fail_queued_before_mutation(index, "native-dispatch-arm-failed", &error);
            return;
        }
        self.jobs[index].journal = armed;
        self.jobs[index].disposition = JournalDisposition::Launching;

        let mut child =
            match ManagedChild::spawn(&spec, stdout, stderr, Path::new(DEFAULT_PROC_ROOT)) {
                Ok(child) => child,
                Err(error) => {
                    self.settle_native_launch_failure(index, "native-worker-spawn-failed", &error);
                    return;
                }
            };
        let mut running = self.jobs[index].journal.clone();
        let attach_result =
            running.attach_native_running(child.identity.clone(), worker_run_id.clone());
        if let Err(error) = attach_result.and_then(|_| self.journal_store.update(&running)) {
            let _ = child.terminate(Path::new(DEFAULT_PROC_ROOT), Duration::from_secs(1));
            self.settle_native_launch_failure(index, "native-worker-attach-failed", &error);
            return;
        }
        self.jobs[index].journal = running;
        self.jobs[index].disposition = JournalDisposition::LiveProcess;
        if runtime_owning {
            self.native_children.insert(job_id, child);
            return;
        }
        let permit = match rating::encode_permit(&job_id, &worker_run_id) {
            Ok(permit) => permit,
            Err(error) => {
                let _ = child.terminate(Path::new(DEFAULT_PROC_ROOT), Duration::from_secs(1));
                self.settle_native_after_exit(index, Some(("failed", "native-permit-invalid")));
                self.job_errors.insert(job_id, error);
                return;
            }
        };
        if let Err(error) =
            self.journal_store
                .publish_native_permit(&job_id, &worker_run_id, &permit)
        {
            let _ = child.terminate(Path::new(DEFAULT_PROC_ROOT), Duration::from_secs(1));
            self.settle_native_after_exit(index, Some(("failed", "native-permit-publish-failed")));
            self.job_errors.insert(job_id, error);
            return;
        }
        self.native_children.insert(job_id, child);
    }

    fn settle_native_launch_failure(&mut self, index: usize, code: &str, message: &str) {
        let mut failed = self.jobs[index].journal.clone();
        if failed
            .settle_native_terminal("failed", Some(code))
            .and_then(|_| self.journal_store.update(&failed))
            .is_err()
        {
            self.startup_issues.push(format!(
                "unable to settle native launch failure for {}: {message}",
                failed.job_id
            ));
            return;
        }
        self.jobs[index].journal = failed;
        self.jobs[index].disposition = JournalDisposition::Settled;
        self.job_errors
            .insert(self.jobs[index].journal.job_id.clone(), message.to_string());
        if let Err(error) = self.leases.release(&self.jobs[index].journal.job_id) {
            self.startup_issues.push(format!(
                "unable to release native launch failure leases: {error}"
            ));
        }
    }

    fn bootstrap_no_owner_recovery_readiness(
        &self,
        index: usize,
        paths: &NativeJobPaths,
        worker_run_id: &str,
    ) -> Result<BootstrapNoOwnerRecoveryReadiness, String> {
        let job = self
            .jobs
            .get(index)
            .ok_or_else(|| "bootstrap recovery has no coordinator job".to_string())?;
        if job.request.target_state != OperationTargetState::AbsentBootstrap
            || job.journal.state != super::protocol::OperationState::Recovering
            || !job.journal.runtime_mutated
            || !job.journal.recovery_required
            || job.journal.runtime_owner_process.is_some()
            || job.journal.worker_run_id.as_deref() != Some(worker_run_id)
        {
            return Err(
                "owner-free bootstrap recovery requires an unowned recovering runtime journal"
                    .to_string(),
            );
        }
        let store = RuntimeOverrideStore::open(&paths.bootstrap_runtime_dir)?;
        let permit = store.read_permit()?;
        let control = store.read_control()?;
        let restore_intent = store.read_restore_intent()?;
        let ack = store.read_ack()?;
        let checkpoint = store.read_checkpoint()?;
        if permit.is_some() {
            return Ok(BootstrapNoOwnerRecoveryReadiness::OwnerRequired);
        }
        let claim = read_bootstrap_runtime_owner_claim(
            &paths.bootstrap_runtime_dir,
            &job.request,
            worker_run_id,
        )?;
        if control.is_some() || restore_intent.is_some() {
            return Err(
                "bootstrap recovery control state exists without its exact permit".to_string(),
            );
        }
        if ack.is_some() || checkpoint.is_some() {
            let claim = claim.as_ref().ok_or_else(|| {
                "permit-withdrawal recovery residue has no exact bootstrap owner claim".to_string()
            })?;
            let ack = ack.as_ref().ok_or_else(|| {
                "permit-withdrawal recovery checkpoint has no RESTORED acknowledgement".to_string()
            })?;
            ack.validate()?;
            let expected_permit_id = checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.permit_id.as_str())
                .unwrap_or(claim.baseline.kernel_namespace_seed.as_str());
            if ack.state != RuntimeAckState::Restored
                || ack.permit_id != expected_permit_id
                || ack.job_id != job.journal.job_id
                || ack.worker_run_id != worker_run_id
                || ack.target_interface != job.request.identity.target_interface
                || ack.route_fingerprint != job.request.identity.route_fingerprint
                || ack.sqm_fingerprint != job.request.identity.sqm_fingerprint
                || ack.updated_boot_ms > monotonic_boot_ms()?
            {
                return Err(
                    "permit-withdrawal RESTORED acknowledgement identity mismatch".to_string(),
                );
            }
            if let Some(checkpoint) = checkpoint.as_ref() {
                // A checkpoint keeps the generation that issued its permit. A
                // recovering journal is intentionally adopted into each new
                // coordinator generation, so comparing those two generations
                // would reject every otherwise exact restart residue. The
                // immutable job, worker, boot and namespace identities below
                // bind the checkpoint across that adoption boundary.
                if checkpoint.job_id != job.journal.job_id
                    || checkpoint.worker_run_id != worker_run_id
                    || checkpoint.boot_id != job.journal.boot_id
                    || checkpoint.instance_name != job.request.identity.instance
                    || checkpoint.profile
                        != job.request.profile.ok_or_else(|| {
                            "bootstrap recovery request has no profile".to_string()
                        })?
                    || checkpoint.link_kind != full_autotune::detect_native_link_kind(&job.request)?
                    || checkpoint.temporary_stage != TemporaryTopologyStage::BaselineRestored
                    || checkpoint.permit_id != claim.baseline.kernel_namespace_seed
                    || checkpoint.baseline != RuntimeBaseline::Absent(claim.baseline.clone())
                    || ack.updated_boot_ms < checkpoint.created_boot_ms
                {
                    return Err(
                        "permit-withdrawal checkpoint does not attest the exact restored bootstrap runtime"
                            .to_string(),
                    );
                }
            } else if ack.permit_id != claim.baseline.kernel_namespace_seed {
                return Err(
                    "permit-withdrawal acknowledgement lost its bootstrap namespace identity"
                        .to_string(),
                );
            }
            (self.bootstrap_runtime_attestor)(&job.request, &claim.baseline)?;
            return Ok(BootstrapNoOwnerRecoveryReadiness::RestoredFinalizationPending);
        }
        ensure_runtime_store_idle(&store)?;
        match claim {
            Some(claim) => {
                (self.bootstrap_runtime_attestor)(&job.request, &claim.baseline)?;
                match claim.process.still_matches(Path::new(DEFAULT_PROC_ROOT))? {
                    true => Ok(BootstrapNoOwnerRecoveryReadiness::OwnerExitPending(
                        claim.process,
                    )),
                    false => Ok(BootstrapNoOwnerRecoveryReadiness::Restored),
                }
            }
            None => {
                let baseline = (self.bootstrap_runtime_baseline_capturer)(&job.request)?;
                (self.bootstrap_runtime_attestor)(&job.request, &baseline)?;
                Ok(BootstrapNoOwnerRecoveryReadiness::Restored)
            }
        }
    }

    fn runtime_store_for_request(
        &self,
        request: &OperationRequest,
    ) -> Result<RuntimeOverrideStore, String> {
        if request.target_state == OperationTargetState::AbsentBootstrap {
            let job = self
                .jobs
                .iter()
                .find(|job| job.journal.job_id == request.identity.job_id)
                .ok_or_else(|| {
                    "bootstrap runtime store has no exact coordinator job".to_string()
                })?;
            if job.request != *request || job.journal.runtime_owner_process.is_none() {
                return Err(
                    "bootstrap runtime store is not bound to an attached runtime owner".to_string(),
                );
            }
            let worker_run_id =
                job.journal.worker_run_id.as_deref().ok_or_else(|| {
                    "bootstrap runtime store has no worker run identity".to_string()
                })?;
            let paths = self
                .journal_store
                .native_job_paths(&request.identity.job_id, worker_run_id)?;
            return RuntimeOverrideStore::open(&paths.bootstrap_runtime_dir);
        }
        let cfg = Config::from_uci(&request.identity.instance)?;
        if cfg.sqm_interface != request.identity.target_interface {
            return Err("runtime store instance target changed".to_string());
        }
        RuntimeOverrideStore::open(&cfg.run_dir())
    }

    fn assemble_bootstrap_runtime_override_permit(
        &self,
        request: &OperationRequest,
        worker_run_id: &str,
        worker: &ProcessIdentity,
        permit_id: String,
        baseline: AbsentRuntimeBaseline,
    ) -> Result<AutotuneRuntimePermit, String> {
        request.validate()?;
        if request.target_state != OperationTargetState::AbsentBootstrap
            || request.identity.operation != OperationKind::FullAutotune
            || request.origin != OperationOrigin::Luci
            || request.scheduled_auto_apply_requested
        {
            return Err(
                "bootstrap runtime permit requires a manual absent LuCI Full Auto-Tune request"
                    .to_string(),
            );
        }
        let initial_download_kbps = request.service_dl_cap_kbps.ok_or_else(|| {
            "bootstrap runtime permit requires an explicit download service cap".to_string()
        })?;
        let initial_upload_kbps = request.service_ul_cap_kbps.ok_or_else(|| {
            "bootstrap runtime permit requires an explicit upload service cap".to_string()
        })?;
        if baseline.kernel_namespace_seed != permit_id {
            return Err(
                "bootstrap runtime namespace seed differs from its permit identity".to_string(),
            );
        }
        let now_unix_ms = rating::epoch_ms()?;
        let remaining_ms = request.deadline_unix_ms.saturating_sub(now_unix_ms);
        if remaining_ms < 15_000 {
            return Err(
                "operation deadline is too close for a bootstrap runtime probe".to_string(),
            );
        }
        let deadline_boot_ms = monotonic_boot_ms()?
            .checked_add(remaining_ms.min(NATIVE_AUTOTUNE_RUNTIME_PERMIT_MAX_MS))
            .ok_or_else(|| "bootstrap runtime probe deadline overflow".to_string())?;
        let policy = full_autotune::native_autotune_runtime_permit_policy(request)?;
        let permit = AutotuneRuntimePermit {
            kind: RuntimePermitKind::Autotune,
            permit_id,
            job_id: request.identity.job_id.clone(),
            worker_run_id: worker_run_id.to_string(),
            boot_id: self.coordinator.boot_id.clone(),
            coordinator_generation: self.coordinator.generation.clone(),
            worker: worker.clone(),
            instance_name: request.identity.instance.clone(),
            target_interface: request.identity.target_interface.clone(),
            route_identity: runtime_route_identity(request)?,
            route_fingerprint: request.identity.route_fingerprint.clone(),
            sqm_fingerprint: request.identity.sqm_fingerprint.clone(),
            deadline_boot_ms,
            maximum_sequence: policy.maximum_sequence,
            profile: request
                .profile
                .ok_or_else(|| "bootstrap runtime permit requires a profile".to_string())?,
            link_kind: full_autotune::detect_native_link_kind(request)?,
            baseline: RuntimeBaseline::Absent(baseline),
            initial_download_kbps,
            initial_upload_kbps,
            download_qdisc_kind: RuntimeQdiscKind::Cake,
            upload_qdisc_kind: RuntimeQdiscKind::Cake,
            allow_bypass_download: policy.allow_directional_bypass,
            allow_bypass_upload: policy.allow_directional_bypass,
            download_bounds: native_autotune_rate_bounds(
                "download",
                initial_download_kbps,
                policy.download_bounds,
            )?,
            upload_bounds: native_autotune_rate_bounds(
                "upload",
                initial_upload_kbps,
                policy.upload_bounds,
            )?,
        };
        permit.validate()?;
        full_autotune::validate_runtime_permit_admission(
            &permit,
            request,
            worker_run_id,
            worker,
            &self.coordinator.boot_id,
        )?;
        Ok(permit)
    }

    fn build_runtime_override_permit(
        &self,
        request: &OperationRequest,
        worker_run_id: &str,
        worker: &ProcessIdentity,
    ) -> RuntimePermitReadiness {
        if !request_requires_native_runtime(request) {
            return RuntimePermitReadiness::Unsafe {
                code: "runtime-permit-operation-mismatch".to_string(),
                message: "operation does not own a native runtime override".to_string(),
            };
        }
        let snapshot = match native_runtime_snapshot_readiness(request) {
            NativeRuntimeSnapshotReadiness::Ready(snapshot) => snapshot,
            NativeRuntimeSnapshotReadiness::Waiting { code, message } => {
                return RuntimePermitReadiness::Waiting { code, message }
            }
            NativeRuntimeSnapshotReadiness::Unsafe { code, message } => {
                return RuntimePermitReadiness::Unsafe { code, message }
            }
        };
        match attest_openwrt_runtime(request) {
            RuntimeAttestation::Ready => {}
            RuntimeAttestation::Waiting { code, message } => {
                return RuntimePermitReadiness::Waiting { code, message }
            }
            RuntimeAttestation::Unsafe { code, message } => {
                return RuntimePermitReadiness::Unsafe { code, message }
            }
        }
        let built = (|| -> Result<AutotuneRuntimePermit, String> {
            let cfg = Config::from_uci(&request.identity.instance)?;
            if !cfg.manage_sqm
                || !cfg.sqm_enabled
                || (!cfg.download_shaping_enabled() && !cfg.upload_shaping_enabled())
            {
                return Err(
                    "native runtime probes require at least one managed CAKE direction".to_string(),
                );
            }
            let live_sqm = super::sqm_identity::managed_sqm_identity_fingerprint(
                &request.identity.instance,
                &cfg.sqm_section,
                &request.identity.target_interface,
            )?;
            if live_sqm != request.identity.sqm_fingerprint {
                return Err("operation SQM fingerprint is stale".to_string());
            }
            let initial_dl = if cfg.download_shaping_enabled() {
                snapshot.cake_dl_kbps
            } else {
                cfg.base_dl_shaper_rate_kbps
            }
            .round()
            .max(100.0) as u64;
            let initial_ul = if cfg.upload_shaping_enabled() {
                snapshot.cake_ul_kbps
            } else {
                cfg.base_ul_shaper_rate_kbps
            }
            .round()
            .max(100.0) as u64;
            let now_unix_ms = rating::epoch_ms()?;
            let remaining_ms = request.deadline_unix_ms.saturating_sub(now_unix_ms);
            if remaining_ms < 15_000 {
                return Err("operation deadline is too close for a runtime probe".to_string());
            }
            let now_boot_ms = super::identity::monotonic_boot_ms()?;
            let deadline_boot_ms = now_boot_ms
                .checked_add(remaining_ms.min(NATIVE_AUTOTUNE_RUNTIME_PERMIT_MAX_MS))
                .ok_or_else(|| "runtime probe deadline overflow".to_string())?;
            let route_identity = runtime_route_identity(request)?;
            let baseline_topology =
                match (cfg.download_shaping_enabled(), cfg.upload_shaping_enabled()) {
                    (true, true) => full_autotune::MeasurementTopology::ShapedBoth,
                    (true, false) => full_autotune::MeasurementTopology::DownloadOnlyShaped,
                    (false, true) => full_autotune::MeasurementTopology::UploadOnlyShaped,
                    (false, false) => {
                        return Err(
                            "native runtime probes require a shaped baseline direction".to_string()
                        )
                    }
                };
            let (download_qdisc_kind, upload_qdisc_kind) =
                native_runtime_qdisc_kinds(&snapshot, baseline_topology)?;
            let (
                kind,
                maximum_sequence,
                profile,
                link_kind,
                allow_bypass_download,
                allow_bypass_upload,
                download_bounds,
                upload_bounds,
            ) = match request.identity.operation {
                OperationKind::FullAutotune => {
                    let policy = full_autotune::native_autotune_runtime_permit_policy(request)?;
                    let profile = request.profile.ok_or_else(|| {
                        "native runtime permit requires a calibration profile".to_string()
                    })?;
                    let link_kind = full_autotune::detect_native_link_kind(request)?;
                    (
                        super::autotune_runtime::RuntimePermitKind::Autotune,
                        policy.maximum_sequence,
                        profile,
                        link_kind,
                        policy.allow_directional_bypass,
                        policy.allow_directional_bypass,
                        native_autotune_rate_bounds(
                            "download",
                            initial_dl,
                            policy.download_bounds,
                        )?,
                        native_autotune_rate_bounds("upload", initial_ul, policy.upload_bounds)?,
                    )
                }
                OperationKind::Speedtest
                    if request.speedtest_topology
                        == Some(super::protocol::SpeedtestTopology::Unshaped) =>
                {
                    let direction = request.speedtest_direction.ok_or_else(|| {
                        "unshaped Speed Test runtime permit requires a direction".to_string()
                    })?;
                    let topology = speedtest_unshaped_topology(direction);
                    let allow_bypass_download = !topology.download_is_shaped();
                    let allow_bypass_upload = !topology.upload_is_shaped();
                    (
                        super::autotune_runtime::RuntimePermitKind::SpeedtestUnshaped,
                        1,
                        crate::autotune::AutotuneProfile::BestOverall,
                        crate::autotune::LinkKind::Unknown,
                        allow_bypass_download,
                        allow_bypass_upload,
                        super::autotune_runtime::RuntimeRateBounds {
                            minimum_kbps: initial_dl,
                            maximum_kbps: initial_dl,
                        },
                        super::autotune_runtime::RuntimeRateBounds {
                            minimum_kbps: initial_ul,
                            maximum_kbps: initial_ul,
                        },
                    )
                }
                _ => return Err("unsupported native runtime permit operation".to_string()),
            };
            let permit = AutotuneRuntimePermit {
                kind,
                permit_id: kernel_request_id()?,
                job_id: request.identity.job_id.clone(),
                worker_run_id: worker_run_id.to_string(),
                boot_id: self.coordinator.boot_id.clone(),
                coordinator_generation: self.coordinator.generation.clone(),
                worker: worker.clone(),
                instance_name: request.identity.instance.clone(),
                target_interface: request.identity.target_interface.clone(),
                route_identity,
                route_fingerprint: request.identity.route_fingerprint.clone(),
                sqm_fingerprint: request.identity.sqm_fingerprint.clone(),
                deadline_boot_ms,
                maximum_sequence,
                profile,
                link_kind,
                baseline: RuntimeBaseline::Managed(baseline_topology),
                initial_download_kbps: initial_dl,
                initial_upload_kbps: initial_ul,
                download_qdisc_kind,
                upload_qdisc_kind,
                allow_bypass_download,
                allow_bypass_upload,
                download_bounds,
                upload_bounds,
            };
            permit.validate()?;
            Ok(permit)
        })();
        match built {
            Ok(permit) => RuntimePermitReadiness::Ready(permit),
            Err(message) => RuntimePermitReadiness::Unsafe {
                code: "runtime-permit-invalid".to_string(),
                message,
            },
        }
    }

    fn poll_pending_runtime_permits(&mut self) {
        let indices: Vec<usize> = self
            .jobs
            .iter()
            .enumerate()
            .filter_map(|(index, job)| {
                (self.supports_native_request(&job.request)
                    && request_requires_native_runtime(&job.request)
                    && job.disposition == JournalDisposition::LiveProcess
                    && job.journal.state == super::protocol::OperationState::Running
                    && !job.journal.runtime_mutated)
                    .then_some(index)
            })
            .collect();
        for index in indices {
            self.advance_pending_runtime_permit(index);
        }
    }

    fn stop_parked_runtime_worker(&mut self, index: usize, code: &str, message: &str) {
        let job_id = self.jobs[index].journal.job_id.clone();
        let mut stopping = self.jobs[index].journal.clone();
        if let Err(error) = stopping
            .stop_attached_before_runtime_mutation(code)
            .and_then(|_| self.journal_store.update(&stopping))
        {
            self.job_errors.insert(
                job_id,
                format!("unable to persist parked-worker failure {code}: {error}; {message}"),
            );
            return;
        }
        self.jobs[index].journal = stopping;
        self.job_errors.insert(job_id, format!("{code}: {message}"));
        self.begin_live_cancellation(index);
    }

    fn fail_after_runtime_armed(&mut self, index: usize, code: &str, message: &str) {
        if let Some(process) = self.jobs[index].journal.process.as_ref() {
            let _ = signal_adopted_group(process, Path::new(DEFAULT_PROC_ROOT), 15);
        }
        self.mark_recovery_required_with_code(index, code, message);
    }

    fn advance_pending_runtime_permit(&mut self, index: usize) {
        let request = self.jobs[index].request.clone();
        let job_id = request.identity.job_id.clone();
        let Some(worker_run_id) = self.jobs[index].journal.worker_run_id.clone() else {
            self.stop_parked_runtime_worker(
                index,
                "runtime-worker-identity-missing",
                "the parked runtime worker has no durable run identity",
            );
            return;
        };
        let Some(worker) = self.jobs[index].journal.process.clone() else {
            self.stop_parked_runtime_worker(
                index,
                "runtime-worker-process-missing",
                "the parked runtime worker has no durable process identity",
            );
            return;
        };
        match worker.still_matches(Path::new(DEFAULT_PROC_ROOT)) {
            Ok(true) => {}
            Ok(false) => {
                if self.jobs[index].journal.runtime_owner_process.is_some() {
                    self.stop_parked_runtime_worker(
                        index,
                        "runtime-worker-disappeared-before-permit",
                        "the parked worker exited while its bootstrap runtime owner was still live",
                    );
                } else {
                    self.settle_native_after_exit(
                        index,
                        Some(("failed", "runtime-worker-disappeared-before-permit")),
                    );
                }
                return;
            }
            Err(error) => {
                self.job_errors.insert(
                    job_id,
                    format!("unable to attest parked runtime worker identity: {error}"),
                );
                return;
            }
        }
        let bootstrap_claim = if request.target_state == OperationTargetState::AbsentBootstrap {
            let Some(owner) = self.jobs[index].journal.runtime_owner_process.as_ref() else {
                self.job_errors.insert(
                    job_id,
                    "bootstrap-runtime-owner-waiting: the job-scoped runtime owner is not durably attached"
                        .to_string(),
                );
                return;
            };
            match owner.still_matches(Path::new(DEFAULT_PROC_ROOT)) {
                Ok(true) => {}
                Ok(false) => {
                    self.job_errors.insert(
                        job_id,
                        "bootstrap-runtime-owner-waiting: the attached runtime owner disappeared before permit publication"
                            .to_string(),
                    );
                    return;
                }
                Err(error) => {
                    self.job_errors.insert(
                        job_id,
                        format!(
                            "bootstrap-runtime-owner-waiting: unable to attest the runtime owner before permit publication: {error}"
                        ),
                    );
                    return;
                }
            }
            let paths = match self.journal_store.native_job_paths(&job_id, &worker_run_id) {
                Ok(paths) => paths,
                Err(error) => {
                    self.stop_parked_runtime_worker(
                        index,
                        "bootstrap-runtime-path-invalid",
                        &error,
                    );
                    return;
                }
            };
            let claim = match read_bootstrap_runtime_owner_claim(
                &paths.bootstrap_runtime_dir,
                &request,
                &worker_run_id,
            ) {
                Ok(Some(claim)) if claim.process == *owner => claim,
                Ok(Some(_)) => {
                    self.stop_parked_runtime_worker(
                        index,
                        "bootstrap-runtime-owner-claim-mismatch",
                        "the readiness witness belongs to a different runtime owner process",
                    );
                    return;
                }
                Ok(None) => {
                    self.job_errors.insert(
                        job_id,
                        "bootstrap-runtime-owner-waiting: the runtime owner has not published its read-only absence witness"
                            .to_string(),
                    );
                    return;
                }
                Err(error) => {
                    self.stop_parked_runtime_worker(
                        index,
                        "bootstrap-runtime-owner-claim-invalid",
                        &error,
                    );
                    return;
                }
            };
            if !self.acquire_heavy_lease_if_needed(index) {
                return;
            }
            if let Err(error) = (self.bootstrap_runtime_attestor)(&request, &claim.baseline) {
                self.stop_parked_runtime_worker(
                    index,
                    "bootstrap-runtime-readiness-changed",
                    &error,
                );
                return;
            }
            Some(claim)
        } else {
            None
        };
        let readiness = if let Some(claim) = bootstrap_claim {
            match self.assemble_bootstrap_runtime_override_permit(
                &request,
                &worker_run_id,
                &worker,
                claim.baseline.kernel_namespace_seed.clone(),
                claim.baseline,
            ) {
                Ok(permit) => RuntimePermitReadiness::Ready(permit),
                Err(message) => RuntimePermitReadiness::Unsafe {
                    code: "bootstrap-runtime-permit-invalid".to_string(),
                    message,
                },
            }
        } else {
            self.build_runtime_override_permit(&request, &worker_run_id, &worker)
        };
        let runtime_permit = match readiness {
            RuntimePermitReadiness::Ready(permit) => permit,
            RuntimePermitReadiness::Waiting { code, message } => {
                self.job_errors.insert(job_id, format!("{code}: {message}"));
                return;
            }
            RuntimePermitReadiness::Unsafe { code, message } => {
                self.stop_parked_runtime_worker(index, &code, &message);
                return;
            }
        };
        let runtime_store = match self.runtime_store_for_request(&request) {
            Ok(store) => store,
            Err(error) => {
                self.stop_parked_runtime_worker(index, "native-runtime-store-unavailable", &error);
                return;
            }
        };
        if let Err(error) = ensure_runtime_store_idle(&runtime_store) {
            self.stop_parked_runtime_worker(index, "native-runtime-store-not-idle", &error);
            return;
        }

        let mut armed = self.jobs[index].journal.clone();
        let arm_result = if request.target_state == OperationTargetState::AbsentBootstrap {
            armed.arm_attached_bootstrap_runtime_mutation()
        } else {
            armed.arm_attached_runtime_mutation()
        };
        if let Err(error) = arm_result.and_then(|_| self.journal_store.update(&armed)) {
            self.stop_parked_runtime_worker(index, "native-runtime-arm-failed", &error);
            return;
        }
        // This durable bit precedes both permits.  From this point any crash is
        // conservatively reconciled through the instance-owned restore path.
        self.jobs[index].journal = armed;

        if let Err(error) = runtime_store.publish_permit(&runtime_permit) {
            self.fail_after_runtime_armed(
                index,
                "native-runtime-permit-publish-failed",
                &format!("native runtime permit publication failed: {error}"),
            );
            return;
        }
        let operation_permit = match rating::encode_permit(&job_id, &worker_run_id) {
            Ok(permit) => permit,
            Err(error) => {
                self.fail_after_runtime_armed(
                    index,
                    "native-operation-permit-invalid",
                    &format!("native operation permit is invalid: {error}"),
                );
                return;
            }
        };
        if let Err(error) =
            self.journal_store
                .publish_native_permit(&job_id, &worker_run_id, &operation_permit)
        {
            self.fail_after_runtime_armed(
                index,
                "native-operation-permit-publish-failed",
                &format!("native operation permit publication failed: {error}"),
            );
            return;
        }
        self.job_errors.remove(&job_id);
    }

    fn resume_journalled_cancellations(&mut self) {
        let indices: Vec<usize> = self
            .jobs
            .iter()
            .enumerate()
            .filter_map(|(index, job)| {
                (job.disposition == JournalDisposition::LiveProcess
                    && job.journal.state == super::protocol::OperationState::Cancelling)
                    .then_some(index)
            })
            .collect();
        for index in indices {
            self.begin_live_cancellation(index);
        }
    }

    fn begin_live_cancellation(&mut self, index: usize) {
        let job_id = self.jobs[index].journal.job_id.clone();
        if self.cancellations.contains_key(&job_id) {
            return;
        }
        let runtime_mutated = self.jobs[index].journal.runtime_mutated;
        let process = self.jobs[index].journal.process.clone();
        let runtime_owner = self.jobs[index].journal.runtime_owner_process.clone();
        let worker_signal = process
            .as_ref()
            .map(|process| signal_adopted_group(process, Path::new(DEFAULT_PROC_ROOT), 15));
        // Once a bootstrap owner has crossed the durable mutation boundary it
        // is the recovery agent, not a second worker to terminate in parallel.
        // Stopping the operation worker is the event which makes that owner
        // restore its checkpoint.  Signalling both processes can kill the
        // owner in the permit-withdrawal/finalization window and strand an
        // otherwise exact RESTORED checkpoint.
        let owner_signal = (!runtime_mutated)
            .then(|| {
                runtime_owner
                    .as_ref()
                    .map(|owner| signal_adopted_group(owner, Path::new(DEFAULT_PROC_ROOT), 15))
            })
            .flatten();
        let worker_live = worker_signal
            .as_ref()
            .map(|result| result.as_ref().copied().unwrap_or(true))
            .unwrap_or(false);
        let owner_live = if runtime_mutated {
            runtime_owner
                .as_ref()
                .map(|owner| owner.still_matches(Path::new(DEFAULT_PROC_ROOT)))
                .transpose()
                .map(|value| value.unwrap_or(false))
        } else {
            Ok(owner_signal
                .as_ref()
                .map(|result| result.as_ref().copied().unwrap_or(true))
                .unwrap_or(false))
        };
        let mut signal_errors = Vec::new();
        if let Some(Err(error)) = worker_signal {
            signal_errors.push(format!(
                "unable to signal cancelling worker process group: {error}"
            ));
        }
        if let Some(Err(error)) = owner_signal {
            signal_errors.push(format!(
                "unable to signal cancelling bootstrap runtime owner: {error}"
            ));
        }
        let owner_live = match owner_live {
            Ok(value) => value,
            Err(error) => {
                signal_errors.push(format!(
                    "unable to inspect recovering bootstrap runtime owner: {error}"
                ));
                true
            }
        };
        if !signal_errors.is_empty() {
            self.job_errors
                .insert(job_id.clone(), signal_errors.join("; "));
        }
        if worker_live || owner_live {
            self.cancellations.insert(
                job_id,
                PendingWorkerCancellation {
                    process,
                    runtime_owner,
                    kill_after: Instant::now() + WORKER_CANCEL_GRACE,
                },
            );
        } else {
            self.finish_live_cancellation(index);
        }
    }

    fn finish_live_cancellation(&mut self, index: usize) {
        let job_id = self.jobs[index].journal.job_id.clone();
        self.cancellations.remove(&job_id);
        self.bootstrap_runtime_children.remove(&job_id);
        if self.jobs[index].journal.runtime_mutated {
            self.mark_recovery_required(
                index,
                "cancelled worker and runtime owner exited; verifying exact restoration",
            );
            return;
        }
        let diagnostic = self.jobs[index].journal.diagnostic_code.clone();
        if let Some(code) = diagnostic.as_deref() {
            self.settle_native_after_exit(index, Some(("failed", code)));
        } else {
            self.settle_native_after_exit(index, Some(("cancelled", "")));
        }
    }

    fn coordinator_owned_bootstrap_runtime_owner_live(
        &mut self,
        job_id: &str,
        expected: Option<&ProcessIdentity>,
    ) -> Result<Option<bool>, String> {
        let Some(child) = self.bootstrap_runtime_children.get_mut(job_id) else {
            // Runtime owners adopted after a coordinator restart have no
            // Child handle here.  Their exact /proc identity remains the
            // authoritative observation in the caller.
            return Ok(None);
        };
        let expected = expected.ok_or_else(|| {
            "coordinator-owned bootstrap runtime owner has no journal identity".to_string()
        })?;
        if child.identity != *expected {
            return Err(
                "coordinator-owned bootstrap runtime owner differs from cancellation identity"
                    .to_string(),
            );
        }
        let live = child.try_wait()?.is_none();
        if !live {
            // try_wait() has reaped this direct child.  Remove only the exact
            // matching handle; adopted owners continue through /proc below.
            self.bootstrap_runtime_children.remove(job_id);
        }
        Ok(Some(live))
    }

    fn poll_worker_cancellations(&mut self) {
        let job_ids: Vec<String> = self.cancellations.keys().cloned().collect();
        for job_id in job_ids {
            let Some(pending) = self.cancellations.get(&job_id).cloned() else {
                continue;
            };
            let index = match self
                .jobs
                .iter()
                .position(|job| job.journal.job_id == job_id)
            {
                Some(index) => index,
                None => {
                    self.cancellations.remove(&job_id);
                    self.startup_issues.push(format!(
                        "cancelling worker has no coordinator job: {job_id}"
                    ));
                    continue;
                }
            };
            let worker_live = match pending.process.as_ref() {
                Some(process) => match process.still_matches(Path::new(DEFAULT_PROC_ROOT)) {
                    Ok(live) => live,
                    Err(error) => {
                        self.job_errors.insert(
                            job_id.clone(),
                            format!("unable to revalidate cancelling worker identity: {error}"),
                        );
                        continue;
                    }
                },
                None => false,
            };
            let owner_live = match self.coordinator_owned_bootstrap_runtime_owner_live(
                &job_id,
                pending.runtime_owner.as_ref(),
            ) {
                Ok(Some(live)) => live,
                Ok(None) => match pending.runtime_owner.as_ref() {
                    Some(owner) => match owner.still_matches(Path::new(DEFAULT_PROC_ROOT)) {
                        Ok(live) => live,
                        Err(error) => {
                            self.job_errors.insert(
                                job_id.clone(),
                                format!(
                                    "unable to revalidate cancelling bootstrap runtime owner: {error}"
                                ),
                            );
                            continue;
                        }
                    },
                    None => false,
                },
                Err(error) => {
                    self.job_errors.insert(
                        job_id.clone(),
                        format!("unable to reap cancelling bootstrap runtime owner: {error}"),
                    );
                    continue;
                }
            };
            if !worker_live && self.jobs[index].journal.runtime_mutated {
                // Do not wait for the restored owner to exit before entering
                // recovery: it deliberately waits for the coordinator to
                // attest RESTORED and withdraw the permit.  Preserve its exact
                // journal/child identity so the ordinary recovery path can
                // finish the two-stage owner release.
                self.cancellations.remove(&job_id);
                self.mark_recovery_required(
                    index,
                    "cancelled worker exited; bootstrap runtime owner is restoring",
                );
                continue;
            }
            if !worker_live && !owner_live {
                self.finish_live_cancellation(index);
                continue;
            }
            if Instant::now() < pending.kill_after {
                continue;
            }
            let mut reasons = Vec::new();
            if worker_live {
                if let Some(process) = pending.process.as_ref() {
                    reasons.push(
                        match signal_adopted_group(process, Path::new(DEFAULT_PROC_ROOT), 9) {
                            Ok(true) => "worker exceeded cancellation grace and was force-stopped"
                                .to_string(),
                            Ok(false) => "worker exited at the cancellation deadline".to_string(),
                            Err(error) => {
                                format!(
                                    "unable to force-stop worker after cancellation grace: {error}"
                                )
                            }
                        },
                    );
                }
            }
            if owner_live && !self.jobs[index].journal.runtime_mutated {
                if let Some(owner) = pending.runtime_owner.as_ref() {
                    reasons.push(match signal_adopted_group(
                        owner,
                        Path::new(DEFAULT_PROC_ROOT),
                        9,
                    ) {
                        Ok(true) => {
                            "bootstrap runtime owner exceeded cancellation grace and was force-stopped"
                                .to_string()
                        }
                        Ok(false) => {
                            "bootstrap runtime owner exited at the cancellation deadline"
                                .to_string()
                        }
                        Err(error) => format!(
                            "unable to force-stop bootstrap runtime owner after cancellation grace: {error}"
                        ),
                    });
                }
            }
            // SIGKILL delivery is not proof of process exit.  Keep the exact
            // identities pending until pidfd/proc observation reports both
            // groups gone; only that observation may settle or enter recovery.
            self.job_errors.insert(job_id, reasons.join("; "));
        }
    }

    fn monitor_running_workers(&mut self) {
        let mut dead = Vec::new();
        let mut inspection_errors = Vec::new();
        for (index, job) in self.jobs.iter().enumerate() {
            if job.disposition != JournalDisposition::LiveProcess
                || self.cancellations.contains_key(&job.journal.job_id)
            {
                continue;
            }
            match job
                .journal
                .process
                .as_ref()
                .map(|process| process.still_matches(Path::new(DEFAULT_PROC_ROOT)))
            {
                Some(Ok(true)) => {}
                Some(Ok(false)) | None => dead.push(index),
                Some(Err(error)) => {
                    inspection_errors.push((job.journal.job_id.clone(), error));
                }
            }
        }
        for (job_id, error) in inspection_errors {
            self.job_errors.insert(
                job_id,
                format!("unable to inspect operation worker identity: {error}"),
            );
        }
        for index in dead {
            if self.jobs[index].journal.runtime_mutated {
                self.mark_recovery_required(
                    index,
                    "runtime-owning worker disappeared before settlement",
                );
            } else if self.jobs[index].journal.runtime_owner_process.is_some() {
                self.stop_parked_runtime_worker(
                    index,
                    "runtime-worker-disappeared-before-permit",
                    "the parked worker exited while its bootstrap runtime owner was still live",
                );
            } else {
                self.settle_native_after_exit(index, None);
            }
        }
    }

    fn settle_native_after_exit(&mut self, index: usize, fallback: Option<(&str, &str)>) {
        let job_id = self.jobs[index].journal.job_id.clone();
        let Some(worker_run_id) = self.jobs[index].journal.worker_run_id.clone() else {
            self.job_errors.insert(
                job_id,
                "native terminal cannot be matched without worker run identity".to_string(),
            );
            return;
        };
        let paths = match self.journal_store.native_job_paths(&job_id, &worker_run_id) {
            Ok(paths) => paths,
            Err(error) => {
                self.job_errors.insert(job_id, error);
                return;
            }
        };
        if operation_uses_native_route_pin(&self.jobs[index].request) {
            if let Err(error) = cleanup_native_route_pin(
                &self.jobs[index].request,
                &worker_run_id,
                self.route_pin_cleaner,
            ) {
                self.job_errors.insert(
                    job_id,
                    format!("unable to clean exact native speedtest route pin: {error}"),
                );
                return;
            }
        }
        let terminal = match fs::symlink_metadata(&paths.terminal) {
            Ok(_) => match self.jobs[index].request.identity.operation {
                OperationKind::GuidedRating | OperationKind::AutomaticRating => {
                    match rating::read_terminal_file(&paths.terminal) {
                        Ok(record)
                            if record.job_id == job_id && record.worker_run_id == worker_run_id =>
                        {
                            match record.terminal {
                                RatingTerminal::Complete(_) => ("complete".to_string(), None),
                                RatingTerminal::Cancelled => ("cancelled".to_string(), None),
                                RatingTerminal::Incomplete { .. } => (
                                    "incomplete".to_string(),
                                    Some("rating-incomplete".to_string()),
                                ),
                                RatingTerminal::Failed { code } => {
                                    ("failed".to_string(), Some(code))
                                }
                            }
                        }
                        Ok(_) => (
                            "failed".to_string(),
                            Some("native-terminal-identity-mismatch".to_string()),
                        ),
                        Err(_) => (
                            "failed".to_string(),
                            Some("native-terminal-invalid".to_string()),
                        ),
                    }
                }
                OperationKind::Speedtest => match speedtest::read_terminal_file(&paths.terminal) {
                    Ok(record)
                        if record.job_id == job_id && record.worker_run_id == worker_run_id =>
                    {
                        match record.terminal {
                            SpeedtestTerminal::Complete(_)
                                if request_requires_native_runtime(&self.jobs[index].request) =>
                            {
                                (
                                    "failed".to_string(),
                                    Some(
                                        "native-speedtest-complete-before-runtime-mutation"
                                            .to_string(),
                                    ),
                                )
                            }
                            SpeedtestTerminal::Complete(_) => ("complete".to_string(), None),
                            SpeedtestTerminal::Cancelled => ("cancelled".to_string(), None),
                            SpeedtestTerminal::Failed { code } => {
                                ("failed".to_string(), Some(code))
                            }
                        }
                    }
                    Ok(_) => (
                        "failed".to_string(),
                        Some("native-terminal-identity-mismatch".to_string()),
                    ),
                    Err(_) => (
                        "failed".to_string(),
                        Some("native-terminal-invalid".to_string()),
                    ),
                },
                // A runtime-owning Full Auto-Tune terminal is consumed only
                // by settle_native_runtime_recovery(), after exact restore.
                // This direct path is nevertheless required for a clean
                // cancellation or failure before RuntimeMutationArmed.
                OperationKind::FullAutotune => {
                    match full_autotune::read_terminal_file(&paths.terminal) {
                        Ok(record)
                            if record.job_id == job_id && record.worker_run_id == worker_run_id =>
                        {
                            match record.terminal {
                                AutotuneTerminal::Cancelled => ("cancelled".to_string(), None),
                                AutotuneTerminal::Failed { code }
                                | AutotuneTerminal::Inconclusive { code } => {
                                    ("failed".to_string(), Some(code))
                                }
                                AutotuneTerminal::Complete { .. } => (
                                    "failed".to_string(),
                                    Some(
                                        "native-autotune-complete-before-runtime-mutation"
                                            .to_string(),
                                    ),
                                ),
                            }
                        }
                        Ok(_) => (
                            "failed".to_string(),
                            Some("native-terminal-identity-mismatch".to_string()),
                        ),
                        Err(_) => (
                            "failed".to_string(),
                            Some("native-terminal-invalid".to_string()),
                        ),
                    }
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => fallback
                .map(|(state, code)| {
                    (
                        state.to_string(),
                        (!code.is_empty()).then(|| code.to_string()),
                    )
                })
                .unwrap_or_else(|| {
                    (
                        "failed".to_string(),
                        Some("native-terminal-missing".to_string()),
                    )
                }),
            Err(_) => (
                "failed".to_string(),
                Some("native-terminal-inspection-failed".to_string()),
            ),
        };
        if matches!(
            self.jobs[index].request.identity.operation,
            OperationKind::GuidedRating | OperationKind::AutomaticRating
        ) {
            let capture_root = env::var_os("CAKE_AUTORATE_RUN_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/var/run/cake-autorate"));
            let capture_path = capture_root
                .join(&self.jobs[index].request.identity.instance)
                .join("rating-capture");
            if let Err(error) = rating::remove_matching_capture(&capture_path, &job_id) {
                self.job_errors.insert(
                    job_id.clone(),
                    format!("unable to clean exact native rating capture: {error}"),
                );
                return;
            }
        }
        let mut settled = self.jobs[index].journal.clone();
        if let Err(error) = settled
            .settle_native_terminal(&terminal.0, terminal.1.as_deref())
            .and_then(|_| self.journal_store.update(&settled))
        {
            self.job_errors.insert(
                job_id.clone(),
                format!("unable to persist native terminal: {error}"),
            );
            return;
        }
        self.jobs[index].journal = settled;
        self.jobs[index].disposition = JournalDisposition::Settled;
        self.native_children.remove(&job_id);
        self.cancellations.remove(&job_id);
        let _ = fs::remove_file(paths.permit);
        if terminal.0 == "complete" || terminal.0 == "cancelled" {
            self.job_errors.remove(&job_id);
        } else if let Some(code) = terminal.1 {
            self.job_errors.insert(job_id.clone(), code);
        }
        if let Err(error) = self.leases.release(&job_id) {
            self.startup_issues.push(format!(
                "unable to release native operation leases for {job_id}: {error}"
            ));
        }
    }

    fn dispatch_next_queued(&mut self) {
        let queued_indices: Vec<usize> = self
            .jobs
            .iter()
            .enumerate()
            .filter_map(|(index, job)| {
                (job.disposition == JournalDisposition::Queued
                    && self.supports_native_request(&job.request))
                .then_some(index)
            })
            .collect();
        let Some(index) = queued_indices.into_iter().find(|index| {
            self.runtime_preflight_ready(*index)
                && (self.jobs[*index].request.target_state == OperationTargetState::AbsentBootstrap
                    && request_requires_native_runtime(&self.jobs[*index].request)
                    || self.acquire_heavy_lease_if_needed(*index))
        }) else {
            return;
        };
        self.dispatch_native_worker(index);
    }

    fn acquire_heavy_lease_if_needed(&mut self, index: usize) -> bool {
        if !self.jobs[index].journal.heavy_traffic || self.jobs[index].journal.heavy_lease_acquired
        {
            return true;
        }
        let job_id = self.jobs[index].journal.job_id.clone();
        let mut staged_leases = self.leases.clone();
        if let Err(error) = staged_leases.acquire_additional(LeaseRequest::heavy_for(&job_id)) {
            self.job_errors
                .insert(job_id, format!("heavy-traffic-lease-waiting: {error}"));
            return false;
        }
        let mut staged_journal = self.jobs[index].journal.clone();
        let mark_result = if self.jobs[index].request.target_state
            == OperationTargetState::AbsentBootstrap
            && request_requires_native_runtime(&self.jobs[index].request)
        {
            staged_journal.mark_bootstrap_heavy_lease_acquired()
        } else {
            staged_journal.mark_heavy_lease_acquired()
        };
        if let Err(error) = mark_result.and_then(|_| self.journal_store.update(&staged_journal)) {
            self.startup_issues.push(format!(
                "unable to durably acquire heavy-traffic lease for {job_id}: {error}"
            ));
            return false;
        }
        // No runtime mutation can occur until the durable journal says that
        // the global lease is held.  The staged in-memory table is committed
        // only after that write; a crash reconstructs it from the same bit.
        self.leases = staged_leases;
        self.jobs[index].journal = staged_journal;
        self.job_errors.remove(&job_id);
        true
    }

    fn runtime_preflight_ready(&mut self, index: usize) -> bool {
        let Some(attestor) = self.runtime_attestor else {
            return true;
        };
        let request = self.jobs[index].request.clone();
        let job_id = request.identity.job_id.clone();
        let now_unix_ms = match rating::epoch_ms() {
            Ok(value) => value,
            Err(error) => {
                self.fail_queued_before_mutation(index, "runtime-clock-unavailable", &error);
                return false;
            }
        };
        if now_unix_ms >= request.deadline_unix_ms {
            self.fail_queued_before_mutation(
                index,
                "operation-deadline-expired",
                "the immutable operation deadline expired while waiting for runtime",
            );
            return false;
        }
        if request.target_state == OperationTargetState::ExistingManaged
            && request_requires_native_runtime(&request)
            && self.supports_native_request(&request)
        {
            match native_runtime_snapshot_readiness(&request) {
                NativeRuntimeSnapshotReadiness::Ready(_) => {}
                NativeRuntimeSnapshotReadiness::Unsafe { code, message } => {
                    self.fail_queued_before_mutation(index, &code, &message);
                    return false;
                }
                NativeRuntimeSnapshotReadiness::Waiting { code, message } => {
                    self.job_errors.insert(job_id, format!("{code}: {message}"));
                    return false;
                }
            }
        }
        if request.target_state == OperationTargetState::AbsentBootstrap {
            // The independently supervised runtime owner publishes and later
            // re-attests the full UCI/route/kernel absence witness before the
            // heavy lease or either permit.  Calling the existing-instance
            // attestor here would wait forever for UCI/runtime state which is
            // intentionally absent.
            return true;
        }
        match attestor(&request) {
            RuntimeAttestation::Ready => {
                self.job_errors.remove(&job_id);
                true
            }
            RuntimeAttestation::Unsafe { code, message } => {
                self.fail_queued_before_mutation(index, &code, &message);
                false
            }
            RuntimeAttestation::Waiting { code, message } => {
                self.job_errors.insert(job_id, format!("{code}: {message}"));
                false
            }
        }
    }

    fn fail_queued_before_mutation(&mut self, index: usize, code: &str, message: &str) {
        let mut failed = self.jobs[index].journal.clone();
        if failed
            .fail_before_runtime_mutation(code)
            .and_then(|_| self.journal_store.update(&failed))
            .is_err()
        {
            self.startup_issues.push(format!(
                "unable to settle pre-mutation failure for job {}: {code}: {message}",
                failed.job_id
            ));
            return;
        }
        self.jobs[index].journal = failed;
        self.jobs[index].disposition = JournalDisposition::Settled;
        self.job_errors.insert(
            self.jobs[index].journal.job_id.clone(),
            format!("{code}: {message}"),
        );
        if let Err(error) = self.leases.release(&self.jobs[index].journal.job_id) {
            self.startup_issues.push(format!(
                "unable to release pre-mutation failed job leases: {error}"
            ));
        }
    }

    fn mark_recovery_required(&mut self, index: usize, reason: &str) {
        let native_runtime = request_requires_native_runtime(&self.jobs[index].request)
            && self.supports_native_request(&self.jobs[index].request);
        let cancelled =
            self.jobs[index].journal.state == super::protocol::OperationState::Cancelling;
        let diagnostic = if native_runtime && cancelled {
            "native-runtime-cancelled"
        } else if native_runtime {
            "native-runtime-reconciliation-required"
        } else {
            "native-worker-reconciliation-required"
        };
        self.mark_recovery_required_with_code(index, diagnostic, reason);
    }

    fn mark_recovery_required_with_code(&mut self, index: usize, diagnostic: &str, reason: &str) {
        let mut recovering = self.jobs[index].journal.clone();
        if let Err(error) = recovering.require_recovery(diagnostic) {
            self.startup_issues.push(format!(
                "unable to enter recovery for job {}: {reason}: {error}",
                recovering.job_id
            ));
            return;
        }
        if let Err(error) = self.journal_store.update(&recovering) {
            self.startup_issues.push(format!(
                "unable to persist recovery for job {}: {reason}: {error}",
                recovering.job_id
            ));
            return;
        }
        self.jobs[index].journal = recovering;
        self.jobs[index].disposition = JournalDisposition::RecoveryRequired;
        if native_recovery_log_is_error(diagnostic) {
            eprintln!(
                "native calibration recovery required for job {}: {diagnostic}: {reason}",
                self.jobs[index].journal.job_id
            );
        } else if diagnostic == "native-runtime-reconciliation-required" {
            println!(
                "native calibration restoration reconciliation in progress for job {}: {reason}",
                self.jobs[index].journal.job_id
            );
        } else {
            println!(
                "native calibration cancellation restoration in progress for job {}: {reason}",
                self.jobs[index].journal.job_id
            );
        }
        self.job_errors
            .insert(self.jobs[index].journal.job_id.clone(), reason.to_string());
    }

    #[cfg(test)]
    fn handle(&mut self, request: &ControlMessage) -> String {
        self.handle_with_effect(request).0
    }

    fn handle_with_effect(&mut self, request: &ControlMessage) -> (String, ControlEffect) {
        match request.control.command {
            ControlCommand::Ping => (
                format!(
                    "{{\"state\":\"ready\",\"protocol_version\":{},\"coordinator\":\"cake-autorated\",\"generation\":\"{}\",\"admission_enabled\":{},\"capabilities\":[\"passive-journal-v1\",\"strict-peer-identity\",\"queued-admission-v1\"]}}\n",
                    OPERATION_PROTOCOL_VERSION,
                    self.coordinator.generation,
                    bool_json(self.admission_available())
                ),
                ControlEffect::ReadOnly,
            ),
            ControlCommand::Summary => (self.summary_response(), ControlEffect::ReadOnly),
            ControlCommand::Start => {
                let jobs_before = self.jobs.len();
                let leases_before = self.leases.job_count();
                let response = self.start_job(request);
                let effect = if self.jobs.len() != jobs_before
                    || self.leases.job_count() != leases_before
                {
                    ControlEffect::StateChanged
                } else {
                    ControlEffect::ReadOnly
                };
                (response, effect)
            }
            ControlCommand::Status => (self.status_job(request), ControlEffect::ReadOnly),
            ControlCommand::Result => (self.result_job(request), ControlEffect::ReadOnly),
            ControlCommand::Cancel => {
                let before = self.authorized_job_index(request).map(|index| {
                    let job_id = self.jobs[index].journal.job_id.clone();
                    (
                        index,
                        self.jobs[index].journal.clone(),
                        self.jobs[index].disposition,
                        self.cancellations.contains_key(&job_id),
                        self.job_errors.get(&job_id).cloned(),
                        self.startup_issues.len(),
                        self.leases.job_count(),
                    )
                });
                let response = self.cancel_job(request);
                let state_changed = before.is_some_and(
                    |(
                        index,
                        journal,
                        disposition,
                        cancellation_pending,
                        job_error,
                        startup_issue_count,
                        lease_count,
                    )| {
                        self.jobs.get(index).is_some_and(|job| {
                            job.journal != journal || job.disposition != disposition
                        }) || self.cancellations.contains_key(&journal.job_id)
                            != cancellation_pending
                            || self.job_errors.get(&journal.job_id) != job_error.as_ref()
                            || self.startup_issues.len() != startup_issue_count
                            || self.leases.job_count() != lease_count
                    },
                );
                let effect = if state_changed {
                    ControlEffect::StateChanged
                } else {
                    ControlEffect::ReadOnly
                };
                (response, effect)
            }
        }
    }

    fn handle_native_apply_control(
        &mut self,
        request: &NativeApplyControlRequest,
    ) -> (String, ControlEffect) {
        if let Err(error) = request.validate() {
            return (
                error_response("native-apply-control-invalid", &error),
                ControlEffect::ReadOnly,
            );
        }
        match request.command {
            NativeApplyControlCommand::Start => {
                let source_job_id = request
                    .source_job_id
                    .as_deref()
                    .expect("validated native Apply start has a source job ID");
                let option_id = request
                    .option_id
                    .as_deref()
                    .expect("validated native Apply start has an option ID");
                let review_sha256 = request
                    .review_sha256
                    .as_deref()
                    .expect("validated native Apply start has a Review digest");
                let manifest_sha256 = request
                    .manifest_sha256
                    .as_deref()
                    .expect("validated native Apply start has a manifest digest");
                let apply_job_id = match kernel_request_id() {
                    Ok(value) => value,
                    Err(error) => {
                        return (
                            error_response("native-apply-identity-unavailable", &error),
                            ControlEffect::ReadOnly,
                        )
                    }
                };
                let apply_job_token = match kernel_request_id()
                    .and_then(|first| kernel_request_id().map(|second| format!("{first}{second}")))
                {
                    Ok(value) => value,
                    Err(error) => {
                        return (
                            error_response("native-apply-identity-unavailable", &error),
                            ControlEffect::ReadOnly,
                        )
                    }
                };
                let candidate = NativeApplyDispatchRecord {
                    state: NativeApplyDispatchState::Accepted,
                    generation: 1,
                    apply_job_id,
                    apply_job_token,
                    source_job_id: source_job_id.to_string(),
                    worker_run_id: "none".to_string(),
                    option_id: option_id.to_string(),
                    review_sha256: review_sha256.to_string(),
                    source_manifest_sha256: "none".to_string(),
                    manifest_sha256: manifest_sha256.to_string(),
                    manifest_schema_version: 0,
                    target_state: "none".to_string(),
                    acknowledgements: request.acknowledgements.clone(),
                };
                match self.native_apply_store.admit(request, candidate.clone()) {
                    Ok(NativeApplyAdmission::Created(record)) => (
                        native_apply_accepted_response(&record),
                        ControlEffect::StateChanged,
                    ),
                    Ok(NativeApplyAdmission::Existing(record)) => (
                        native_apply_accepted_response(&record),
                        ControlEffect::ReadOnly,
                    ),
                    Ok(NativeApplyAdmission::Terminal(record)) => {
                        let retry = (self.native_apply_live_state_attestor)(
                            &self.state_dir,
                            source_job_id,
                            Some(option_id),
                        )
                        .and_then(|live| native_apply_terminal_retry(&record, live));
                        match retry {
                            Ok(NativeApplyTerminalRetry::Reuse) => (
                                native_apply_accepted_response(&record.dispatch),
                                ControlEffect::ReadOnly,
                            ),
                            Ok(NativeApplyTerminalRetry::Rearm) => {
                                match self
                                    .native_apply_store
                                    .rearm_terminal(request, &record, candidate)
                                {
                                    Ok(rearmed) => (
                                        native_apply_accepted_response(&rearmed),
                                        ControlEffect::StateChanged,
                                    ),
                                    Err(error) => (
                                        error_response(
                                            "native-apply-terminal-rearm-failed",
                                            &error,
                                        ),
                                        ControlEffect::ReadOnly,
                                    ),
                                }
                            }
                            Err(error) => (
                                error_response("native-apply-terminal-revalidation-failed", &error),
                                ControlEffect::ReadOnly,
                            ),
                        }
                    }
                    Err(error) => (
                        error_response("native-apply-admission-failed", &error),
                        ControlEffect::ReadOnly,
                    ),
                }
            }
            NativeApplyControlCommand::Status | NativeApplyControlCommand::Watch => {
                match self.native_apply_store.authorized_active(request) {
                    Ok(Some(record)) => (
                        native_apply_status_response(&record),
                        ControlEffect::ReadOnly,
                    ),
                    Ok(None) => match self.native_apply_store.authorized_terminal(request) {
                        Ok(Some(record)) => (
                            native_apply_terminal_status_response(&record),
                            ControlEffect::ReadOnly,
                        ),
                        Ok(None) => (
                            error_response(
                                "native-apply-not-found",
                                "no native Apply matches the supplied handle",
                            ),
                            ControlEffect::ReadOnly,
                        ),
                        Err(error) => (
                            error_response("native-apply-status-invalid", &error),
                            ControlEffect::ReadOnly,
                        ),
                    },
                    Err(error) => (
                        error_response("native-apply-status-invalid", &error),
                        ControlEffect::ReadOnly,
                    ),
                }
            }
            NativeApplyControlCommand::Result => {
                match self.native_apply_store.authorized_terminal(request) {
                    Ok(Some(record)) => (
                        native_apply_terminal_result_response(&record),
                        ControlEffect::ReadOnly,
                    ),
                    Ok(None) => match self.native_apply_store.authorized_active(request) {
                        Ok(Some(_)) => (
                            error_response(
                                "native-apply-result-not-ready",
                                "native Apply has not reached a terminal state",
                            ),
                            ControlEffect::ReadOnly,
                        ),
                        Ok(None) => (
                            error_response(
                                "native-apply-not-found",
                                "no native Apply matches the supplied handle",
                            ),
                            ControlEffect::ReadOnly,
                        ),
                        Err(error) => (
                            error_response("native-apply-result-invalid", &error),
                            ControlEffect::ReadOnly,
                        ),
                    },
                    Err(error) => (
                        error_response("native-apply-result-invalid", &error),
                        ControlEffect::ReadOnly,
                    ),
                }
            }
        }
    }

    fn drive_native_apply(&mut self) {
        if let Err(error) = self.drive_native_apply_inner() {
            eprintln!("native Apply coordinator drive remains pending: {error}");
        }
    }

    fn drive_native_apply_inner(&mut self) -> Result<(), String> {
        if let Some(child) = self.native_apply_child.as_mut() {
            if child.try_wait()?.is_none() {
                self.native_apply_worker_identity = Some(child.identity.clone());
                return Ok(());
            }
            self.native_apply_child = None;
        }

        match reconcile_native_apply_worker(&self.native_apply_store, Path::new(DEFAULT_PROC_ROOT))?
        {
            NativeApplyWorkerReadiness::Idle => {
                self.native_apply_worker_identity = None;
                Ok(())
            }
            NativeApplyWorkerReadiness::Live(identity) => {
                self.native_apply_worker_identity = Some(identity);
                Ok(())
            }
            NativeApplyWorkerReadiness::Launch(dispatch) => {
                self.native_apply_worker_identity = None;
                self.spawn_native_apply_worker(&dispatch)
            }
        }
    }

    fn spawn_native_apply_worker(
        &mut self,
        dispatch: &NativeApplyDispatchRecord,
    ) -> Result<(), String> {
        if self.native_apply_child.is_some() {
            return Ok(());
        }
        dispatch.validate()?;
        if !matches!(
            dispatch.state,
            NativeApplyDispatchState::Validating | NativeApplyDispatchState::Applying
        ) {
            return Err("native Apply worker launch requires executable state".to_string());
        }
        let program = env::current_exe()
            .map_err(|error| format!("native Apply worker program is unavailable: {error}"))?;
        if !program.is_absolute() {
            return Err("native Apply worker program path is not absolute".to_string());
        }
        let stdout = private_append_log_file(
            &self.native_apply_store.worker_log_path("stdout")?,
            "native Apply worker stdout",
        )?;
        let stderr = private_append_log_file(
            &self.native_apply_store.worker_log_path("stderr")?,
            "native Apply worker stderr",
        )?;
        let spec = SpawnSpec {
            program,
            arguments: vec![
                OsString::from("--native-apply-worker"),
                OsString::from("--state-dir"),
                self.state_dir.clone().into_os_string(),
                OsString::from("--apply-job-id"),
                OsString::from(dispatch.apply_job_id.clone()),
            ],
            environment: Vec::new(),
        };
        let child = ManagedChild::spawn(&spec, stdout, stderr, Path::new(DEFAULT_PROC_ROOT))?;
        self.native_apply_worker_identity = Some(child.identity.clone());
        self.native_apply_child = Some(child);
        Ok(())
    }

    #[cfg(test)]
    fn handle_scheduler_accounting_acknowledgement(
        &mut self,
        request: &SchedulerAccountingAcknowledgement,
    ) -> String {
        self.handle_scheduler_accounting_acknowledgement_with_effect(request)
            .0
    }

    fn handle_scheduler_accounting_acknowledgement_with_effect(
        &mut self,
        request: &SchedulerAccountingAcknowledgement,
    ) -> (String, ControlEffect) {
        let Some(scheduler) = self.native_scheduler.as_ref() else {
            return (
                error_response(
                    "native-scheduler-disabled",
                    "native scheduler accounting is not owned by this coordinator",
                ),
                ControlEffect::ReadOnly,
            );
        };
        let mut state = match scheduler.store.load_state(&request.instance) {
            Ok(Some(state)) => state,
            Ok(None) => {
                return (
                    error_response(
                        "scheduler-instance-not-found",
                        "native scheduler has no durable state for this instance",
                    ),
                    ControlEffect::ReadOnly,
                )
            }
            Err(error) => {
                return (
                    error_response("scheduler-state-invalid", &error),
                    ControlEffect::ReadOnly,
                )
            }
        };
        if !state.budget.accounting_blocked {
            return (
                error_response(
                    "scheduler-accounting-not-blocked",
                    "scheduler traffic accounting is not blocked",
                ),
                ControlEffect::ReadOnly,
            );
        }
        let Some(reservation) = state.budget.reservation.as_ref() else {
            return (
                error_response(
                    "scheduler-state-invalid",
                    "blocked scheduler accounting has no reservation",
                ),
                ControlEffect::ReadOnly,
            );
        };
        let reservation_job_id = reservation.job_id.clone();
        if let Some(index) = self
            .jobs
            .iter()
            .position(|job| job.journal.job_id == reservation_job_id)
        {
            if !state::terminal(self.jobs[index].journal.state) {
                return (
                    error_response(
                        "scheduler-accounting-recoverable",
                        "the reserved scheduled job is still live; refusing to discard its exact accounting path",
                    ),
                    ControlEffect::ReadOnly,
                );
            }
            if self
                .exact_scheduled_terminal_traffic(index, &reservation_job_id)
                .is_ok()
            {
                return (
                    error_response(
                        "scheduler-accounting-recoverable",
                        "exact terminal traffic evidence is available; scheduler settlement must consume it",
                    ),
                    ControlEffect::ReadOnly,
                );
            }
        }
        let scheduler = self
            .native_scheduler
            .as_mut()
            .expect("native scheduler ownership was checked above");
        let charged_bytes =
            match acknowledge_scheduled_accounting_unknown(&scheduler.store, &mut state) {
                Ok(charged_bytes) => charged_bytes,
                Err(error) => {
                    return (
                        error_response("scheduler-accounting-not-blocked", &error),
                        ControlEffect::ReadOnly,
                    )
                }
            };
        scheduler.errors.remove(&request.instance);
        scheduler.waiting.remove(&request.instance);
        scheduler.accounting_blocks.remove(&request.instance);
        (
            format!(
                "{{\"state\":\"acknowledged\",\"instance\":\"{}\",\"charged_bytes\":{charged_bytes},\"reservation_refunded\":false}}\n",
                request.instance
            ),
            ControlEffect::StateChanged,
        )
    }

    fn handle_scheduler_status(&self, _request: &SchedulerStatusRequest) -> String {
        let Some(scheduler) = self.native_scheduler.as_ref() else {
            return error_response(
                "native-scheduler-disabled",
                "native scheduler status is not owned by this coordinator",
            );
        };
        scheduler
            .status_cache
            .response()
            .map(str::to_string)
            .unwrap_or_else(|| {
                error_response(
                    "scheduler-status-unavailable",
                    "native scheduler has not published a complete status snapshot yet",
                )
            })
    }

    fn admission_available(&self) -> bool {
        self.admission_requested
            && self.startup_issues.is_empty()
            && !self.jobs.iter().any(|job| {
                matches!(
                    job.disposition,
                    JournalDisposition::RecoveryRequired | JournalDisposition::StaleBoot
                )
            })
    }

    fn prune_settled_history(&mut self) -> Result<usize, String> {
        let excess = self.jobs.len().saturating_sub(JOURNAL_RETENTION_TARGET);
        if excess == 0 {
            return Ok(0);
        }
        let mut candidates = self
            .jobs
            .iter()
            .filter(|job| {
                job.disposition == JournalDisposition::Settled
                    && !self.leases.contains_job(&job.journal.job_id)
                    && !self.native_children.contains_key(&job.journal.job_id)
                    && !self
                        .bootstrap_runtime_children
                        .contains_key(&job.journal.job_id)
                    && !self.cancellations.contains_key(&job.journal.job_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            left.request
                .created_unix_ms
                .cmp(&right.request.created_unix_ms)
                .then_with(|| left.journal.job_id.cmp(&right.journal.job_id))
        });
        candidates.truncate(excess);
        let mut retired_count = 0usize;
        for job in candidates {
            self.journal_store
                .retire_settled_job(&job, Path::new(DEFAULT_PROC_ROOT))?;
            let job_id = job.journal.job_id;
            self.jobs
                .retain(|retained| retained.journal.job_id != job_id);
            self.job_errors.remove(&job_id);
            retired_count += 1;
        }
        if self.jobs.len() > MAX_JOURNAL_JOBS {
            return Err(format!(
                "{} journals remain after safe retention; maximum is {MAX_JOURNAL_JOBS}",
                self.jobs.len()
            ));
        }
        Ok(retired_count)
    }

    fn start_job(&mut self, message: &ControlMessage) -> String {
        if !self.admission_available() {
            return error_response(
                "admission-disabled",
                "job admission remains disabled until startup recovery is complete",
            );
        }
        let Some(request) = message.operation.as_ref() else {
            return error_response("invalid-request", "start requires an operation request");
        };
        if let Err(error) = request.validate_admission_policy() {
            return error_response("invalid-request-policy", &error);
        }
        if !self.supports_native_request(request) {
            return error_response(
                "operation-unsupported",
                "this coordinator has no enabled native implementation for the requested operation",
            );
        }
        if let Some(existing) = self
            .jobs
            .iter()
            .find(|job| job.request.identity.job_id == request.identity.job_id)
        {
            if existing.request == *request {
                return job_response(existing, true);
            }
            return error_response(
                "job-id-conflict",
                "job_id is already bound to a different immutable request",
            );
        }
        if let Err(error) = self.prune_settled_history() {
            return error_response("journal-retention-failed", &error);
        }

        let lease_request = match LeaseRequest::local_from_operation(request) {
            Ok(request) => request,
            Err(error) => return error_response("invalid-lease-request", &error),
        };
        if let Err(error) = self.leases.acquire(lease_request) {
            return lease_acquire_error_response(&error);
        }
        let journal = match JobJournal::queued(
            request,
            &self.coordinator,
            requires_heavy_traffic(request.identity.operation),
        ) {
            Ok(journal) => journal,
            Err(error) => {
                let _ = self.leases.release(&request.identity.job_id);
                return error_response("invalid-journal", &error);
            }
        };
        if let Err(error) = self.journal_store.create(request, &journal) {
            let _ = self.leases.release(&request.identity.job_id);
            return error_response("journal-create-failed", &error);
        }
        self.jobs.push(ScannedJob {
            request: request.clone(),
            journal,
            disposition: JournalDisposition::Queued,
        });
        let job = self.jobs.last().expect("queued job was just inserted");
        job_response(job, false)
    }

    fn status_job(&self, message: &ControlMessage) -> String {
        let Some(job) = self.authorized_job(message) else {
            return error_response("job-not-found", "no job matches the supplied identity");
        };
        let progress = self.native_autotune_status_progress(job);
        job_status_response_with_progress(
            job,
            true,
            self.job_errors.get(&job.journal.job_id).map(String::as_str),
            progress.as_ref(),
        )
    }

    fn native_autotune_status_progress(
        &self,
        job: &ScannedJob,
    ) -> Option<full_autotune::NativeAutotuneProgress> {
        use super::full_autotune::{NativeAutotuneProgress, NativeAutotuneProgressStep};
        use super::protocol::OperationState;

        if job.request.identity.operation != OperationKind::FullAutotune {
            return None;
        }
        let fixed = |step, percent| NativeAutotuneProgress::coordinator_state(step, percent);
        Some(match job.journal.state {
            OperationState::Queued => fixed(NativeAutotuneProgressStep::WaitingForSlot, 1),
            OperationState::Starting => fixed(NativeAutotuneProgressStep::StartingCalibration, 2),
            OperationState::Running => {
                let Some(worker_run_id) = job.journal.worker_run_id.as_deref() else {
                    return Some(fixed(NativeAutotuneProgressStep::StartingCalibration, 3));
                };
                let Ok(paths) = self
                    .journal_store
                    .native_job_paths(&job.journal.job_id, worker_run_id)
                else {
                    return Some(fixed(NativeAutotuneProgressStep::StartingCalibration, 3));
                };
                let Some(job_directory) = paths.request.parent() else {
                    return Some(fixed(NativeAutotuneProgressStep::StartingCalibration, 3));
                };
                full_autotune::read_native_autotune_progress(
                    job_directory,
                    &job.journal.job_id,
                    worker_run_id,
                )
                .unwrap_or_else(|_| fixed(NativeAutotuneProgressStep::StartingCalibration, 3))
            }
            OperationState::Cancelling | OperationState::Recovering => {
                fixed(NativeAutotuneProgressStep::RestoringSettings, 96)
            }
            OperationState::ReviewReady | OperationState::Completed => {
                fixed(NativeAutotuneProgressStep::ProposalsReady, 100)
            }
            OperationState::Cancelled | OperationState::Failed => return None,
        })
    }

    fn result_job(&self, message: &ControlMessage) -> String {
        let Some(job) = self.authorized_job(message) else {
            return error_response("job-not-found", "no job matches the supplied identity");
        };
        if matches!(
            job.request.identity.operation,
            OperationKind::GuidedRating | OperationKind::AutomaticRating
        ) {
            if job.journal.state != super::protocol::OperationState::Completed {
                return error_response(
                    "result-not-ready",
                    "the native Rating public result is not complete",
                );
            }
            let Some(worker_run_id) = job.journal.worker_run_id.as_deref() else {
                return error_response(
                    "result-identity-missing",
                    "the completed native Rating job has no worker identity",
                );
            };
            let paths = match self
                .journal_store
                .native_job_paths(&job.journal.job_id, worker_run_id)
            {
                Ok(paths) => paths,
                Err(error) => return error_response("result-path-invalid", &error),
            };
            let terminal = match rating::read_terminal_file(&paths.terminal) {
                Ok(terminal) => terminal,
                Err(error) => return error_response("result-terminal-invalid", &error),
            };
            if terminal.job_id != job.journal.job_id || terminal.worker_run_id != worker_run_id {
                return error_response(
                    "result-terminal-identity-mismatch",
                    "the native Rating terminal identity changed",
                );
            }
            return match terminal.terminal {
                RatingTerminal::Complete(result) => {
                    rating_public_result_response(&job.journal.job_id, &result)
                }
                _ => error_response(
                    "result-terminal-incomplete",
                    "the native Rating terminal has no complete result",
                ),
            };
        }
        if job.request.identity.operation == OperationKind::Speedtest {
            if job.journal.state != super::protocol::OperationState::Completed
                || job.journal.runtime_mutated
                || job.journal.recovery_required
            {
                return error_response(
                    "result-not-ready",
                    "the native Speed Test public result is not complete",
                );
            }
            let Some(worker_run_id) = job.journal.worker_run_id.as_deref() else {
                return error_response(
                    "result-identity-missing",
                    "the completed native Speed Test job has no worker identity",
                );
            };
            let paths = match self
                .journal_store
                .native_job_paths(&job.journal.job_id, worker_run_id)
            {
                Ok(paths) => paths,
                Err(error) => return error_response("result-path-invalid", &error),
            };
            let terminal = match speedtest::read_terminal_file(&paths.terminal) {
                Ok(terminal) => terminal,
                Err(error) => return error_response("result-terminal-invalid", &error),
            };
            if terminal.job_id != job.journal.job_id || terminal.worker_run_id != worker_run_id {
                return error_response(
                    "result-terminal-identity-mismatch",
                    "the native Speed Test terminal identity changed",
                );
            }
            return match terminal.terminal {
                SpeedtestTerminal::Complete(result) => {
                    speedtest_public_result_response(&job.request, &result)
                }
                _ => error_response(
                    "result-terminal-incomplete",
                    "the native Speed Test terminal has no complete result",
                ),
            };
        }
        if job.request.identity.operation != OperationKind::FullAutotune
            || !matches!(
                job.journal.state,
                super::protocol::OperationState::ReviewReady
                    | super::protocol::OperationState::Completed
            )
        {
            return error_response(
                "result-not-ready",
                "the native Full Auto-Tune public result is not complete",
            );
        }
        match verified_native_autotune_public_result(&self.state_dir, &job.request, &job.journal) {
            Ok(result) => result,
            Err(error) => error_response("result-verification-failed", &error),
        }
    }

    fn cancel_job(&mut self, message: &ControlMessage) -> String {
        let Some(index) = self.authorized_job_index(message) else {
            return error_response("job-not-found", "no job matches the supplied identity");
        };
        if state::terminal(self.jobs[index].journal.state) {
            return job_response(&self.jobs[index], true);
        }
        use super::protocol::OperationState;
        if self.jobs[index].journal.state == OperationState::Recovering {
            return job_response(&self.jobs[index], true);
        }
        if self.jobs[index].journal.state == OperationState::Cancelling {
            if self.jobs[index].journal.process.is_some()
                || self.jobs[index].journal.runtime_owner_process.is_some()
            {
                self.begin_live_cancellation(index);
            }
            return job_response(&self.jobs[index], true);
        }
        let mut settled = self.jobs[index].journal.clone();
        let next = if settled.state == OperationState::Queued {
            OperationState::Cancelled
        } else {
            OperationState::Cancelling
        };
        if let Err(error) = settled.transition(next) {
            return error_response("invalid-transition", &error);
        }
        if next == OperationState::Cancelled {
            settled.heavy_lease_acquired = false;
        }
        if let Err(error) = self.journal_store.update(&settled) {
            return error_response("journal-update-failed", &error);
        }
        self.jobs[index].journal = settled;
        if next == OperationState::Cancelled {
            self.jobs[index].disposition = JournalDisposition::Settled;
            if let Err(error) = self.leases.release(&self.jobs[index].journal.job_id) {
                self.startup_issues.push(format!(
                    "unable to release leases for cancelled job {}: {error}",
                    self.jobs[index].journal.job_id
                ));
                return error_response("lease-release-failed", &error);
            }
        } else if self.jobs[index].journal.process.is_some()
            || self.jobs[index].journal.runtime_owner_process.is_some()
        {
            self.begin_live_cancellation(index);
        } else if !self.jobs[index].journal.runtime_mutated {
            self.settle_native_after_exit(index, Some(("cancelled", "")));
        } else {
            self.mark_recovery_required(
                index,
                "cancellation began after dispatch was armed but no worker identity is available",
            );
        }
        job_response(&self.jobs[index], false)
    }

    fn authorized_job(&self, message: &ControlMessage) -> Option<&ScannedJob> {
        self.authorized_job_index(message)
            .and_then(|index| self.jobs.get(index))
    }

    fn authorized_job_index(&self, message: &ControlMessage) -> Option<usize> {
        let job_id = message.control.job_id.as_deref()?;
        let job_token = message.control.job_token.as_deref()?;
        self.jobs.iter().position(|job| {
            job.request.identity.job_id == job_id && job.request.identity.job_token == job_token
        })
    }

    fn summary_response(&self) -> String {
        let queued = self
            .jobs
            .iter()
            .filter(|job| job.disposition == JournalDisposition::Queued)
            .count();
        let active = self
            .jobs
            .iter()
            .filter(|job| {
                matches!(
                    job.disposition,
                    JournalDisposition::Launching | JournalDisposition::LiveProcess
                )
            })
            .count();
        let recovery = self
            .jobs
            .iter()
            .filter(|job| {
                matches!(
                    job.disposition,
                    JournalDisposition::RecoveryRequired | JournalDisposition::StaleBoot
                )
            })
            .count()
            + self.startup_issues.len();
        let abandoned = self
            .jobs
            .iter()
            .filter(|job| job.disposition == JournalDisposition::DeadBeforeMutation)
            .count();
        let settled = self
            .jobs
            .iter()
            .filter(|job| job.disposition == JournalDisposition::Settled)
            .count();
        let state = if recovery > 0 {
            "recovery_required"
        } else if active > 0 {
            "running"
        } else if queued > 0 {
            "queued"
        } else {
            "idle"
        };
        let active_operations = self
            .jobs
            .iter()
            .filter(|job| {
                matches!(
                    job.journal.state,
                    OperationState::Queued
                        | OperationState::Starting
                        | OperationState::Running
                        | OperationState::Cancelling
                        | OperationState::Recovering
                )
            })
            .map(|job| {
                format!(
                    "{{\"instance\":\"{}\",\"operation\":\"{}\",\"state\":\"{}\",\"runtime_mutated\":{}}}",
                    json_escape(&job.request.identity.instance),
                    job.request.identity.operation.as_str(),
                    job.journal.state.as_str(),
                    bool_json(job.journal.runtime_mutated)
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let scheduler_errors = self
            .native_scheduler
            .as_ref()
            .map(|scheduler| scheduler.errors.len())
            .unwrap_or(0);
        let scheduler_waiting = self
            .native_scheduler
            .as_ref()
            .map(|scheduler| {
                scheduler
                    .waiting
                    .iter()
                    .map(|(instance, reason)| {
                        format!("{{\"instance\":\"{instance}\",\"reason\":\"{reason}\"}}")
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        let scheduler_accounting_blocks = self
            .native_scheduler
            .as_ref()
            .map(|scheduler| {
                scheduler
                    .accounting_blocks
                    .iter()
                    .map(|(instance, charged_bytes)| {
                        format!("{{\"instance\":\"{instance}\",\"charged_bytes\":{charged_bytes}}}")
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        let native_scheduler = self.native_scheduler.is_some();
        let native_scheduler_lab = self
            .native_scheduler
            .as_ref()
            .is_some_and(|scheduler| scheduler.lab_mode);
        format!(
            "{{\"state\":\"{state}\",\"protocol_version\":{},\"active_job\":{},\"active_jobs\":{active},\"active_operations\":[{active_operations}],\"queued_jobs\":{queued},\"recovery_required_jobs\":{recovery},\"abandoned_safe_jobs\":{abandoned},\"settled_jobs\":{settled},\"leased_jobs\":{},\"admission_enabled\":{},\"native_rating\":{},\"native_speedtest\":{},\"native_bootstrap_speedtest\":true,\"native_speedtest_auto_backend\":true,\"native_full_autotune\":{},\"native_bootstrap_autotune\":{},\"native_autotune_auto_backend\":true,\"native_operation_status_identity_version\":{},\"native_autotune_status_identity_version\":{},\"native_scheduler\":{},\"native_scheduler_lab\":{},\"native_scheduler_errors\":{scheduler_errors},\"native_scheduler_waiting\":[{scheduler_waiting}],\"native_scheduler_accounting_blocks\":[{scheduler_accounting_blocks}],\"native_public_result_version\":{}}}\n",
            OPERATION_PROTOCOL_VERSION,
            if active > 0 { "true" } else { "false" },
            self.leases.job_count(),
            bool_json(self.admission_available()),
            bool_json(self.native_rating),
            bool_json(self.native_speedtest),
            bool_json(self.native_autotune),
            bool_json(self.native_autotune),
            NATIVE_OPERATION_STATUS_IDENTITY_VERSION,
            NATIVE_OPERATION_STATUS_IDENTITY_VERSION,
            bool_json(native_scheduler),
            bool_json(native_scheduler_lab),
            super::autotune_public::NATIVE_PUBLIC_RESULT_MAX_SCHEMA_VERSION
        )
    }
}

fn build_native_scheduler_status_rows(
    scheduler: &NativeSchedulerRuntime,
    snapshot: &SchedulerConfigSnapshot,
    now_unix_s: u64,
    observed_monotonic: Instant,
    day: &str,
    month: &str,
) -> Result<NativeSchedulerStatusRows, String> {
    let durable_instances = scheduler.store.instances()?;
    let configured_instances = snapshot
        .instances
        .iter()
        .map(|config| config.instance.as_str())
        .chain(snapshot.issues.iter().map(|issue| issue.instance.as_str()))
        .collect::<BTreeSet<_>>();
    let orphan_count = durable_instances
        .iter()
        .filter(|instance| !configured_instances.contains(instance.as_str()))
        .count();
    if snapshot
        .instances
        .len()
        .saturating_add(snapshot.issues.len())
        .saturating_add(orphan_count)
        > MAX_SCHEDULER_STATUS_ENTRIES
    {
        return Err("native scheduler status contains too many instance records".to_string());
    }
    let mut instances = Vec::new();
    let mut next_scheduler_wake_at = None;
    let mut issues = snapshot
        .issues
        .iter()
        .map(|issue| {
            format_native_scheduler_issue(
                &issue.instance,
                &sanitize_scheduler_public_message(&issue.message),
                now_unix_s,
            )
        })
        .collect::<Vec<_>>();
    for instance in durable_instances {
        if !configured_instances.contains(instance.as_str()) {
            issues.push(format_native_scheduler_issue(
                &instance,
                "Durable native scheduler state has no current UCI configuration.",
                now_unix_s,
            ));
        }
    }
    for config in &snapshot.instances {
        let persisted = match scheduler.store.load_state(&config.instance) {
            Ok(state) => state,
            Err(_) => {
                issues.push(format_native_scheduler_issue(
                    &config.instance,
                    "Native scheduler state is unavailable or invalid for this instance.",
                    now_unix_s,
                ));
                continue;
            }
        };
        if let Some(wake_at) =
            native_scheduler_instance_wake_at(config, persisted.as_ref(), now_unix_s)
        {
            next_scheduler_wake_at = Some(
                next_scheduler_wake_at
                    .map(|current: u64| current.min(wake_at))
                    .unwrap_or(wake_at),
            );
        }
        let scheduler_error = scheduler
            .errors
            .get(&config.instance)
            .or_else(|| scheduler.errors.get("_global"))
            .map(String::as_str);
        let scheduler_warning = scheduler
            .auto_apply_warnings
            .get(&config.instance)
            .map(String::as_str)
            .or_else(|| {
                persisted
                    .as_ref()
                    .and_then(|state| state.operator_warning.as_deref())
            });
        let waiting = scheduler.waiting.get(&config.instance).copied();
        match native_scheduler_status_response(
            config,
            persisted.as_ref(),
            scheduler_error,
            scheduler_warning,
            waiting,
            now_unix_s,
            day,
            month,
        ) {
            Ok(response) => instances.push(response),
            Err(_) => issues.push(format_native_scheduler_issue(
                &config.instance,
                "Native scheduler state is inconsistent with current configuration.",
                now_unix_s,
            )),
        }
    }
    Ok(NativeSchedulerStatusRows {
        observed_at: now_unix_s,
        observed_monotonic,
        next_scheduler_wake_at,
        instances,
        issues,
    })
}

pub(crate) fn runtime_route_identity(request: &OperationRequest) -> Result<String, String> {
    let source_ip = request
        .route
        .source_ip
        .ok_or_else(|| "native runtime probe requires an explicit source IP".to_string())?;
    match request.route.mode {
        OperationRouteMode::Main => {
            if request.route.mwan3_member.is_some()
                || request.route.fwmark.is_some()
                || request.route.routing_table.is_some()
            {
                return Err("main runtime probe carries policy-routing fields".to_string());
            }
            Ok(format!(
                "main||{}|{}||main",
                request.route.l3_device, source_ip
            ))
        }
        OperationRouteMode::Mwan3 => {
            let member = request
                .route
                .mwan3_member
                .as_deref()
                .ok_or_else(|| "mwan3 runtime probe has no member".to_string())?;
            let fwmark = request
                .route
                .fwmark
                .ok_or_else(|| "mwan3 runtime probe has no fwmark".to_string())?;
            let table = request
                .route
                .routing_table
                .ok_or_else(|| "mwan3 runtime probe has no routing table".to_string())?;
            Ok(format!(
                "mwan3|{}|{}|{}|0x{:x}|{}",
                member, request.route.l3_device, source_ip, fwmark, table
            ))
        }
    }
}

fn native_runtime_terminal_diagnostic(recovery_diagnostic: Option<&str>) -> (bool, &str) {
    if recovery_diagnostic == Some("native-runtime-cancelled") {
        return (true, "native-runtime-cancelled");
    }
    (
        false,
        recovery_diagnostic
            .filter(|code| {
                code.starts_with("native-") && *code != "native-runtime-reconciliation-required"
            })
            .unwrap_or("native-runtime-worker-exited"),
    )
}

fn native_recovery_log_is_error(diagnostic: &str) -> bool {
    !matches!(
        diagnostic,
        "native-runtime-cancelled" | "native-runtime-reconciliation-required"
    )
}

struct NativeRuntimeTerminalOutcome {
    state: &'static str,
    diagnostic: Option<String>,
}

fn native_autotune_terminal_outcome(
    paths: &super::journal::NativeJobPaths,
    expected_job_id: &str,
    expected_worker_run_id: &str,
    coordinator_boot_id: &str,
    coordinator_generation: &str,
) -> Result<Option<NativeRuntimeTerminalOutcome>, String> {
    let record = match fs::symlink_metadata(&paths.terminal) {
        Ok(_) => match full_autotune::read_terminal_file(&paths.terminal) {
            Ok(record) => record,
            Err(_) => {
                return Ok(Some(NativeRuntimeTerminalOutcome {
                    state: "failed",
                    diagnostic: Some("native-terminal-invalid".to_string()),
                }))
            }
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "unable to inspect native Auto-Tune terminal: {error}"
            ))
        }
    };
    if record.job_id != expected_job_id || record.worker_run_id != expected_worker_run_id {
        return Ok(Some(NativeRuntimeTerminalOutcome {
            state: "failed",
            diagnostic: Some("native-terminal-identity-mismatch".to_string()),
        }));
    }
    Ok(Some(match record.terminal {
        AutotuneTerminal::Complete { review_digest } => {
            if full_autotune::verify_native_review_transaction(
                &paths.request,
                &paths.review,
                expected_job_id,
                expected_worker_run_id,
                &review_digest,
            )
            .is_err()
            {
                NativeRuntimeTerminalOutcome {
                    state: "failed",
                    diagnostic: Some("native-review-transaction-invalid".to_string()),
                }
            } else if full_autotune::verify_native_apply_manifest_transaction(
                &paths.request,
                &paths.review,
                &paths.apply_manifest,
                expected_job_id,
                expected_worker_run_id,
                &review_digest,
                coordinator_boot_id,
                coordinator_generation,
            )
            .is_err()
            {
                NativeRuntimeTerminalOutcome {
                    state: "failed",
                    diagnostic: Some("native-apply-manifest-invalid".to_string()),
                }
            } else if full_autotune::publish_native_public_result_transaction(
                &paths.request,
                &paths.review,
                &paths.apply_manifest,
                &paths.public_result,
                expected_job_id,
                expected_worker_run_id,
                &review_digest,
                coordinator_boot_id,
                coordinator_generation,
            )
            .is_err()
            {
                NativeRuntimeTerminalOutcome {
                    state: "failed",
                    diagnostic: Some("native-public-result-publication-invalid".to_string()),
                }
            } else {
                NativeRuntimeTerminalOutcome {
                    state: "complete",
                    diagnostic: None,
                }
            }
        }
        AutotuneTerminal::Cancelled => NativeRuntimeTerminalOutcome {
            state: "cancelled",
            diagnostic: None,
        },
        AutotuneTerminal::Inconclusive { code } => NativeRuntimeTerminalOutcome {
            state: "inconclusive",
            diagnostic: Some(code),
        },
        AutotuneTerminal::Failed { code } => NativeRuntimeTerminalOutcome {
            state: "failed",
            diagnostic: Some(code),
        },
    }))
}

fn native_speedtest_runtime_terminal_outcome(
    paths: &super::journal::NativeJobPaths,
    expected_job_id: &str,
    expected_worker_run_id: &str,
) -> Result<Option<NativeRuntimeTerminalOutcome>, String> {
    let record = match fs::symlink_metadata(&paths.terminal) {
        Ok(_) => match speedtest::read_terminal_file(&paths.terminal) {
            Ok(record) => record,
            Err(_) => {
                return Ok(Some(NativeRuntimeTerminalOutcome {
                    state: "failed",
                    diagnostic: Some("native-speedtest-terminal-invalid".to_string()),
                }))
            }
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "unable to inspect native Speed Test terminal: {error}"
            ))
        }
    };
    if record.job_id != expected_job_id || record.worker_run_id != expected_worker_run_id {
        return Ok(Some(NativeRuntimeTerminalOutcome {
            state: "failed",
            diagnostic: Some("native-speedtest-terminal-identity-mismatch".to_string()),
        }));
    }
    Ok(Some(match record.terminal {
        SpeedtestTerminal::Complete(_) => NativeRuntimeTerminalOutcome {
            state: "complete",
            diagnostic: None,
        },
        SpeedtestTerminal::Cancelled => NativeRuntimeTerminalOutcome {
            state: "cancelled",
            diagnostic: None,
        },
        SpeedtestTerminal::Failed { code } => NativeRuntimeTerminalOutcome {
            state: "failed",
            diagnostic: Some(code),
        },
    }))
}

impl Drop for CalibrationDaemon {
    fn drop(&mut self) {
        let Ok(metadata) = fs::symlink_metadata(&self.socket_path) else {
            return;
        };
        if metadata.file_type().is_socket()
            && metadata.uid() == euid()
            && metadata.ino() == self.socket_inode
        {
            let _ = fs::remove_file(&self.socket_path);
        }
    }
}

fn ensure_runtime_store_idle(store: &RuntimeOverrideStore) -> Result<(), String> {
    if store.read_permit()?.is_some()
        || store.read_control()?.is_some()
        || store.read_restore_intent()?.is_some()
        || store.read_ack()?.is_some()
        || store.read_checkpoint()?.is_some()
    {
        return Err("instance runtime ownership store is not idle".to_string());
    }
    Ok(())
}

fn private_log_file(path: &Path) -> Result<fs::File, String> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("unable to create private native worker log: {error}"))
}

fn private_append_log_file(path: &Path, label: &str) -> Result<fs::File, String> {
    let file = OpenOptions::new()
        .write(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("unable to open {label}: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("unable to inspect {label}: {error}"))?;
    if !metadata.is_file()
        || metadata.uid() != euid()
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.nlink() != 1
    {
        return Err(format!("{label} is unsafe"));
    }
    Ok(file)
}

fn private_runtime_owner_log_file(path: &Path) -> Result<fs::File, String> {
    let file = OpenOptions::new()
        .write(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("unable to open bootstrap runtime owner log: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("unable to inspect bootstrap runtime owner log: {error}"))?;
    if !metadata.is_file()
        || metadata.uid() != euid()
        || metadata.permissions().mode() & 0o777 != 0o600
        || metadata.nlink() != 1
    {
        return Err("bootstrap runtime owner log is unsafe".to_string());
    }
    Ok(file)
}

impl SchedulerAccountingAcknowledgement {
    fn validate(&self) -> Result<(), String> {
        if self.request_id.len() != 32
            || !self
                .request_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("scheduler acknowledgement request ID is invalid".to_string());
        }
        validate_scheduler_instance(&self.instance)
    }

    fn encode(&self) -> Result<String, String> {
        self.validate()?;
        Ok(format!(
            "{SCHEDULER_ACK_HEADER}\nrequest_id={}\ninstance={}\n\n",
            self.request_id, self.instance
        ))
    }

    fn decode(input: &str) -> Result<Self, String> {
        let body = input
            .strip_suffix("\n\n")
            .ok_or_else(|| "scheduler acknowledgement is not canonically terminated".to_string())?;
        if body.contains('\r') {
            return Err("scheduler acknowledgement contains CR".to_string());
        }
        let mut lines = body.split('\n');
        if lines.next() != Some(SCHEDULER_ACK_HEADER) {
            return Err("scheduler acknowledgement header is invalid".to_string());
        }
        let request_id = lines
            .next()
            .and_then(|line| line.strip_prefix("request_id="))
            .ok_or_else(|| "scheduler acknowledgement request ID is missing".to_string())?
            .to_string();
        let instance = lines
            .next()
            .and_then(|line| line.strip_prefix("instance="))
            .ok_or_else(|| "scheduler acknowledgement instance is missing".to_string())?
            .to_string();
        if lines.next().is_some() {
            return Err("scheduler acknowledgement has unexpected fields".to_string());
        }
        let request = Self {
            request_id,
            instance,
        };
        request.validate()?;
        if request.encode()? != input {
            return Err("scheduler acknowledgement is not canonical".to_string());
        }
        Ok(request)
    }
}

impl SchedulerStatusRequest {
    fn validate(&self) -> Result<(), String> {
        if self.request_id.len() != 32
            || !self
                .request_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("scheduler status request ID is invalid".to_string());
        }
        Ok(())
    }

    fn encode(&self) -> Result<String, String> {
        self.validate()?;
        Ok(format!(
            "{SCHEDULER_STATUS_HEADER}\nrequest_id={}\n\n",
            self.request_id
        ))
    }

    fn decode(input: &str) -> Result<Self, String> {
        let body = input
            .strip_suffix("\n\n")
            .ok_or_else(|| "scheduler status request is not canonically terminated".to_string())?;
        if body.contains('\r') {
            return Err("scheduler status request contains CR".to_string());
        }
        let mut lines = body.split('\n');
        if lines.next() != Some(SCHEDULER_STATUS_HEADER) {
            return Err("scheduler status request header is invalid".to_string());
        }
        let request_id = lines
            .next()
            .and_then(|line| line.strip_prefix("request_id="))
            .ok_or_else(|| "scheduler status request ID is missing".to_string())?
            .to_string();
        if lines.next().is_some() {
            return Err("scheduler status request has unexpected fields".to_string());
        }
        let request = Self { request_id };
        request.validate()?;
        if request.encode()? != input {
            return Err("scheduler status request is not canonical".to_string());
        }
        Ok(request)
    }
}

fn send_encoded_control(
    state_dir: &Path,
    encoded: &str,
    response_limit: usize,
) -> Result<String, String> {
    send_encoded_control_with_timeout(state_dir, encoded, response_limit, IO_TIMEOUT)
}

fn send_encoded_control_with_timeout(
    state_dir: &Path,
    encoded: &str,
    response_limit: usize,
    timeout: Duration,
) -> Result<String, String> {
    validate_state_path(state_dir)?;
    let stream = UnixStream::connect(state_dir.join(CONTROL_SOCKET_NAME))
        .map_err(|error| format!("unable to connect to calibrationd: {error}"))?;
    exchange_encoded_control(stream, encoded, response_limit, timeout)
}

fn exchange_encoded_control(
    mut stream: UnixStream,
    encoded: &str,
    response_limit: usize,
    timeout: Duration,
) -> Result<String, String> {
    stream
        .set_read_timeout(Some(timeout))
        .and_then(|_| stream.set_write_timeout(Some(timeout)))
        .map_err(|error| format!("unable to configure calibrationctl socket: {error}"))?;
    stream
        .write_all(encoded.as_bytes())
        .map_err(|error| format!("unable to write calibration control request: {error}"))?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|error| format!("unable to finish calibration control request: {error}"))?;
    let mut bytes = Vec::new();
    (&mut stream)
        .take((response_limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("unable to read calibration control response: {error}"))?;
    if bytes.len() > response_limit {
        return Err("calibration control response exceeds its bound".to_string());
    }
    let response =
        String::from_utf8(bytes).map_err(|_| "control response is not UTF-8".to_string())?;
    if !response.ends_with('\n') || response.lines().count() != 1 {
        return Err("control response is not one bounded JSON line".to_string());
    }
    Ok(response)
}

pub(crate) fn probe_calibration_coordinator_control(
    state_dir: &Path,
    identity: &ProcessIdentity,
) -> Result<bool, String> {
    validate_state_path(state_dir)?;
    let stream = match UnixStream::connect(state_dir.join(CONTROL_SOCKET_NAME)) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound
                    | io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::ConnectionReset
            ) =>
        {
            return Ok(false);
        }
        Err(error) => return Err(format!("unable to connect to calibrationd: {error}")),
    };
    let credentials = peer_credentials(&stream)?;
    if credentials.uid != unsafe { libc::geteuid() }
        || u32::try_from(credentials.pid).ok() != Some(identity.pid)
    {
        return Err(format!(
            "calibration control socket belongs to another process (expected {}, observed {})",
            identity.pid, credentials.pid
        ));
    }
    let request = ControlMessage {
        control: ControlRequest {
            request_id: kernel_request_id()?,
            command: ControlCommand::Ping,
            job_id: None,
            job_token: None,
        },
        operation: None,
    };
    let response =
        exchange_encoded_control(stream, &request.encode()?, MAX_RESPONSE_BYTES, IO_TIMEOUT)?;
    let expected_prefix = format!(
        "{{\"state\":\"ready\",\"protocol_version\":{},\"coordinator\":\"cake-autorated\",",
        OPERATION_PROTOCOL_VERSION
    );
    if !response.starts_with(&expected_prefix)
        || !response.contains("\"capabilities\":[\"passive-journal-v1\",\"strict-peer-identity\",\"queued-admission-v1\"]}")
    {
        return Err("calibration coordinator returned an invalid readiness response".to_string());
    }
    Ok(true)
}

fn send_control(state_dir: &Path, request: &ControlMessage) -> Result<String, String> {
    let response_limit = if request.control.command == ControlCommand::Result {
        MAX_RESULT_RESPONSE_BYTES
    } else {
        MAX_RESPONSE_BYTES
    };
    send_encoded_control(state_dir, &request.encode()?, response_limit)
}

fn send_native_apply_control(
    state_dir: &Path,
    request: &NativeApplyControlRequest,
) -> Result<String, String> {
    let response_limit = if request.command == NativeApplyControlCommand::Result {
        MAX_RESULT_RESPONSE_BYTES
    } else {
        MAX_RESPONSE_BYTES
    };
    if request.command == NativeApplyControlCommand::Watch {
        send_encoded_control_with_timeout(
            state_dir,
            &request.encode()?,
            response_limit,
            NATIVE_APPLY_WATCH_CLIENT_TIMEOUT,
        )
    } else {
        send_encoded_control(state_dir, &request.encode()?, response_limit)
    }
}

fn peer_credentials(stream: &UnixStream) -> Result<PeerCredentials, String> {
    let mut credentials = PeerCredentials {
        pid: 0,
        uid: u32::MAX,
        gid: u32::MAX,
    };
    let mut length = u32::try_from(std::mem::size_of::<PeerCredentials>())
        .map_err(|_| "peer credential structure is too large".to_string())?;
    let result = unsafe {
        getsockopt(
            stream.as_raw_fd(),
            SOL_SOCKET,
            SO_PEERCRED,
            (&mut credentials as *mut PeerCredentials).cast::<c_void>(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(format!(
            "unable to inspect calibration control peer: {}",
            io::Error::last_os_error()
        ));
    }
    if usize::try_from(length).ok() != Some(std::mem::size_of::<PeerCredentials>())
        || credentials.pid <= 0
    {
        return Err("calibration control peer identity is malformed".to_string());
    }
    Ok(credentials)
}

fn read_request(stream: &mut UnixStream) -> Result<CoordinatorControlRequest, String> {
    let mut bytes = Vec::new();
    stream
        .take((MAX_CONTROL_MESSAGE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("unable to read calibration control request: {error}"))?;
    if bytes.len() > MAX_CONTROL_MESSAGE_BYTES {
        return Err("calibration control request exceeds its bound".to_string());
    }
    let payload =
        String::from_utf8(bytes).map_err(|_| "control request is not UTF-8".to_string())?;
    if NativeApplyControlRequest::has_header(&payload) {
        NativeApplyControlRequest::decode(&payload).map(CoordinatorControlRequest::NativeApply)
    } else if payload.starts_with(SCHEDULER_ACK_HEADER) {
        SchedulerAccountingAcknowledgement::decode(&payload)
            .map(CoordinatorControlRequest::SchedulerAccountingAcknowledgement)
    } else if payload.starts_with(SCHEDULER_STATUS_HEADER) {
        SchedulerStatusRequest::decode(&payload).map(CoordinatorControlRequest::SchedulerStatus)
    } else {
        ControlMessage::decode(&payload).map(CoordinatorControlRequest::Operation)
    }
}

fn secure_state_dir(path: &Path) -> Result<(), String> {
    validate_state_path(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_dir() || metadata.file_type().is_symlink() || metadata.uid() != euid() {
                return Err("calibration state directory is unsafe or foreign-owned".to_string());
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let parent = path
                .parent()
                .ok_or_else(|| "calibration state directory has no parent".to_string())?;
            if !fs::metadata(parent)
                .map_err(|error| format!("unable to inspect state parent: {error}"))?
                .is_dir()
            {
                return Err("calibration state parent is not a directory".to_string());
            }
            fs::create_dir(path).map_err(|error| {
                format!("unable to create calibration state directory: {error}")
            })?;
        }
        Err(error) => return Err(format!("unable to inspect calibration state: {error}")),
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("unable to secure calibration state directory: {error}"))
}

fn validate_state_path(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err("calibration state path must be absolute and normalized".to_string());
    }
    if ![Path::new("/tmp"), Path::new("/run"), Path::new("/var/run")]
        .iter()
        .any(|prefix| path.starts_with(prefix))
    {
        return Err("calibration state path must reside in RAM".to_string());
    }
    Ok(())
}

fn remove_stale_socket(path: &Path) -> Result<(), String> {
    match UnixStream::connect(path) {
        Ok(_) => return Err("another calibrationd is already listening".to_string()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(_) => {}
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("unable to inspect stale control socket: {error}")),
    };
    if !metadata.file_type().is_socket() || metadata.uid() != euid() {
        return Err("refusing to replace unsafe calibration control path".to_string());
    }
    fs::remove_file(path).map_err(|error| format!("unable to remove stale control socket: {error}"))
}

fn euid() -> u32 {
    unsafe { geteuid() }
}

fn reduce_timeout(current: &mut Option<Duration>, candidate: Duration) {
    if current.is_none_or(|value| candidate < value) {
        *current = Some(candidate);
    }
}

fn job_response(job: &ScannedJob, idempotent: bool) -> String {
    job_status_response(job, idempotent, None)
}

fn job_status_response(job: &ScannedJob, idempotent: bool, diagnostic: Option<&str>) -> String {
    job_status_response_with_progress(job, idempotent, diagnostic, None)
}

fn job_status_response_with_progress(
    job: &ScannedJob,
    idempotent: bool,
    diagnostic: Option<&str>,
    progress: Option<&full_autotune::NativeAutotuneProgress>,
) -> String {
    let worker_run_id = job.journal.worker_run_id.as_ref().map_or_else(
        || "null".to_string(),
        |worker_run_id| format!("\"{}\"", json_escape(worker_run_id)),
    );
    let terminal_state = job.journal.terminal_state.as_ref().map_or_else(
        || "null".to_string(),
        |state| format!("\"{}\"", json_escape(state)),
    );
    let diagnostic_code = job.journal.diagnostic_code.as_ref().map_or_else(
        || "null".to_string(),
        |code| format!("\"{}\"", json_escape(code)),
    );
    let prefix = format!(
        "{{\"state\":\"{}\",\"job_id\":\"{}\",\"operation\":\"{}\",\"instance\":\"{}\",\"worker_run_id\":{},\"sequence\":{},\"idempotent\":{},\"runtime_mutated\":{},\"recovery_required\":{},\"terminal_state\":{},\"diagnostic_code\":{}",
        job.journal.state.as_str(),
        job.journal.job_id,
        job.request.identity.operation.as_str(),
        json_escape(&job.request.identity.instance),
        worker_run_id,
        job.journal.sequence,
        bool_json(idempotent),
        bool_json(job.journal.runtime_mutated),
        bool_json(job.journal.recovery_required),
        terminal_state,
        diagnostic_code,
    );
    let request_identity = operation_status_request_identity(&job.request);
    let progress = progress.map_or_else(String::new, |progress| {
        format!(
            concat!(
                ",\"progress_schema_version\":1,",
                "\"progress_percent\":{},\"progress_step\":\"{}\",",
                "\"progress_stage_index\":{},\"progress_stage_total\":{},",
                "\"progress_completed_units\":{},\"progress_total_units\":{},",
                "\"progress_direction\":{},\"progress_attempt\":{}"
            ),
            progress.progress_percent,
            progress.step.as_str(),
            progress.stage_index,
            progress.stage_total,
            progress.completed_units,
            progress.total_units,
            progress.direction.map_or_else(
                || "null".to_string(),
                |direction| format!("\"{}\"", direction.as_str())
            ),
            progress
                .attempt
                .map_or_else(|| "null".to_string(), |attempt| attempt.to_string()),
        )
    });
    match diagnostic {
        Some(value) => format!(
            "{prefix}{request_identity}{progress},\"diagnostic\":\"{}\"}}\n",
            json_escape(value)
        ),
        None => format!("{prefix}{request_identity}{progress}}}\n"),
    }
}

fn operation_status_request_identity(request: &OperationRequest) -> String {
    let member = request.route.mwan3_member.as_ref().map_or_else(
        || "null".to_string(),
        |member| format!("\"{}\"", json_escape(member)),
    );
    let direction = request
        .speedtest_direction
        .map_or("null".to_string(), |direction| {
            format!("\"{}\"", direction.as_str())
        });
    let server_id = request
        .speedtest_server_id
        .map_or("null".to_string(), |server_id| format!("\"{server_id}\""));
    let topology = request
        .speedtest_topology
        .map_or("null".to_string(), |topology| {
            format!("\"{}\"", topology.as_str())
        });
    let target_state = match request.target_state {
        OperationTargetState::ExistingManaged => "existing_managed",
        OperationTargetState::AbsentBootstrap => "absent_bootstrap",
    };
    let managed_sqm_section = request.managed_sqm_section.as_ref().map_or_else(
        || "null".to_string(),
        |section| format!("\"{}\"", json_escape(section)),
    );
    let profile = request.profile.map_or("null".to_string(), |profile| {
        format!("\"{}\"", profile.as_str())
    });
    let strategy = request.strategy.map_or("null".to_string(), |strategy| {
        format!("\"{}\"", strategy.as_str())
    });
    let origin = match request.origin {
        OperationOrigin::Luci => "luci",
        OperationOrigin::Scheduler => "scheduler",
        OperationOrigin::Recovery => "recovery",
        OperationOrigin::Internal => "internal",
    };
    format!(
        concat!(
            ",\"request_identity_schema_version\":{}",
            ",\"target_interface\":\"{}\"",
            ",\"backend\":\"{}\"",
            ",\"speedtest_direction\":{}",
            ",\"speedtest_server_id\":{}",
            ",\"speedtest_topology\":{}",
            ",\"route_mode\":\"{}\"",
            ",\"mwan3_member\":{}",
            ",\"target_state\":\"{}\"",
            ",\"managed_sqm_section\":{}",
            ",\"profile\":{}",
            ",\"calibration_strategy\":{}",
            ",\"origin\":\"{}\""
        ),
        NATIVE_OPERATION_STATUS_IDENTITY_VERSION,
        json_escape(&request.identity.target_interface),
        json_escape(&request.backend),
        direction,
        server_id,
        topology,
        request.route.mode.as_str(),
        member,
        target_state,
        managed_sqm_section,
        profile,
        strategy,
        origin,
    )
}

fn error_response(code: &str, message: &str) -> String {
    format!(
        "{{\"state\":\"error\",\"error_code\":\"{}\",\"error\":\"{}\"}}\n",
        json_escape(code),
        json_escape(message)
    )
}

fn lease_acquire_error_response(error: &LeaseAcquireError) -> String {
    let Some(conflict_kind) = error.conflict_kind() else {
        return error_response("lease-conflict", error.user_message());
    };
    let owner_job_id = error
        .owner_job_id()
        .expect("typed lease conflict always carries its owner job ID");
    format!(
        concat!(
            "{{\"state\":\"error\",\"error_code\":\"lease-conflict\",",
            "\"error\":\"{}\",\"conflict_kind\":\"{}\",",
            "\"conflicting_job_id\":\"{}\"}}\n"
        ),
        json_escape(error.user_message()),
        conflict_kind,
        json_escape(owner_job_id),
    )
}

fn rating_public_result_response(job_id: &str, result: &rating::RatingResultSnapshot) -> String {
    if result.partial
        || result.incomplete
        || result.grade.is_empty()
        || result.grade == "LEARNING"
        || result.dl_grade.is_empty()
        || result.ul_grade.is_empty()
        || result.dl_samples == 0
        || result.ul_samples == 0
    {
        return error_response(
            "result-terminal-incomplete",
            "the Rating result is not a complete two-direction measurement",
        );
    }
    format!(
        concat!(
            "{{\"state\":\"complete\",\"job_id\":\"{}\",",
            "\"grade\":\"{}\",\"increase_ms\":{},",
            "\"dl_grade\":\"{}\",\"ul_grade\":\"{}\",",
            "\"dl_samples\":{},\"ul_samples\":{},",
            "\"partial\":false,\"incomplete\":false,",
            "\"rating_method\":\"{}\",",
            "\"evidence_source\":\"worst_of_icmp_and_transport\",",
            "\"icmp_basis\":\"controller_reflector_adaptive_baseline\",",
            "\"transport_basis\":\"endpoint_loaded_p90_minus_idle_p5\",",
            "\"confidence\":\"high\",",
            "\"limits_changed\":false}}\n"
        ),
        json_escape(job_id),
        json_escape(&result.grade),
        result.increase_ms,
        json_escape(&result.dl_grade),
        json_escape(&result.ul_grade),
        result.dl_samples,
        result.ul_samples,
        rating::RATING_EVIDENCE_CONTRACT,
    )
}

fn speedtest_public_result_response(
    request: &OperationRequest,
    result: &speedtest::SpeedtestResult,
) -> String {
    if request.identity.operation != OperationKind::Speedtest
        || request.backend != "speedtest-go"
        || request.speedtest_direction != Some(result.direction)
        || request.allow_sqm_disable
        || request.allow_active_traffic
        || request.managed_sqm_section.is_some()
    {
        return error_response(
            "result-terminal-policy-mismatch",
            "the native Speed Test terminal does not match its request policy",
        );
    }
    let (calibration, shaper_bypassed, runtime_restored) = match request.speedtest_topology {
        Some(super::protocol::SpeedtestTopology::Current) => ("current", false, false),
        Some(super::protocol::SpeedtestTopology::Unshaped) => ("unshaped", true, true),
        None => {
            return error_response(
                "result-terminal-policy-mismatch",
                "the native Speed Test request has no topology",
            )
        }
    };
    let download_kbps = result
        .download_kbps
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string());
    let upload_kbps = result
        .upload_kbps
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string());
    let server_id = result
        .server_id
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string());
    format!(
        concat!(
            "{{\"state\":\"complete\",\"job_id\":\"{}\",",
            "\"direction\":\"{}\",\"download_kbps\":{},\"upload_kbps\":{},",
            "\"download_bytes\":{},\"upload_bytes\":{},\"elapsed_ms\":{},",
            "\"backend\":\"speedtest-go\",\"backend_title\":\"speedtest-go\",",
            "\"server_id\":{},\"server_name\":\"{}\",\"server_sponsor\":\"{}\",",
            "\"calibration\":\"{}\",\"shaper_bypassed\":{},",
            "\"runtime_mutated\":false,\"runtime_restored\":{},",
            "\"limits_changed\":false}}\n"
        ),
        json_escape(&request.identity.job_id),
        result.direction.as_str(),
        download_kbps,
        upload_kbps,
        result.rx_bytes,
        result.tx_bytes,
        result.elapsed_ms,
        server_id,
        json_escape(&result.server_name),
        json_escape(&result.server_sponsor),
        calibration,
        bool_json(shaper_bypassed),
        bool_json(runtime_restored),
    )
}

/// Render the live request attestation without exposing either generated
/// capability field. This is intentionally local and non-mutating: it does not
/// create coordinator state, contact the daemon, or launch a worker.
fn autotune_inspection_response(operation: &OperationRequest) -> String {
    let route = &operation.route;
    let member = route
        .mwan3_member
        .as_deref()
        .map(|value| format!("\"{}\"", json_escape(value)))
        .unwrap_or_else(|| "null".to_string());
    let source_ip = route
        .source_ip
        .map(|value| format!("\"{}\"", value))
        .unwrap_or_else(|| "null".to_string());
    let fwmark = route
        .fwmark
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string());
    let routing_table = route
        .routing_table
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string());
    let profile = operation.profile.map(|value| value.as_str()).unwrap_or("");
    let strategy = operation.strategy.map(|value| value.as_str()).unwrap_or("");
    format!(
        concat!(
            "{{\"state\":\"ready\",\"operation\":\"full_autotune\",",
            "\"instance\":\"{}\",\"target_interface\":\"{}\",",
            "\"backend\":\"{}\",\"profile\":\"{}\",\"strategy\":\"{}\",",
            "\"route\":{{\"mode\":\"{}\",\"mwan3_member\":{},",
            "\"l3_device\":\"{}\",\"source_ip\":{},\"fwmark\":{},",
            "\"routing_table\":{}}},",
            "\"fingerprints\":{{\"route\":\"{}\",\"config\":\"{}\",",
            "\"sqm\":\"{}\"}},\"secrets_exposed\":false}}\n"
        ),
        json_escape(&operation.identity.instance),
        json_escape(&operation.identity.target_interface),
        json_escape(&operation.backend),
        json_escape(profile),
        json_escape(strategy),
        route.mode.as_str(),
        member,
        json_escape(&route.l3_device),
        source_ip,
        fwmark,
        routing_table,
        operation.identity.route_fingerprint,
        operation.identity.config_fingerprint,
        operation.identity.sqm_fingerprint,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autotune::{
        AccessEvidenceSource, AccessMedium, AutotuneProfile, CapacityLearningPolicy,
    };
    use crate::operations::autotune_bootstrap_apply::tests::{
        fixture_plan, fixture_raw_fallback_plan,
    };
    use crate::operations::autotune_bootstrap_apply::NativeBootstrapApplyMode;
    use crate::operations::lease::LeaseKey;
    use crate::operations::protocol::{
        CalibrationStrategy, OperationIdentity, OperationKind, OperationOrigin, OperationRequest,
        OperationRouteIdentity, OperationRouteMode, OperationState, SpeedtestDirection,
        SpeedtestTopology,
    };
    use crate::operations::scheduler::{
        BudgetLedger, FailedAttemptFence, ScheduleCursor, SchedulerInstanceState,
    };
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP_PATH: AtomicU64 = AtomicU64::new(1);

    fn temp_path(suffix: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "catc-{:x}-{:x}-{suffix}",
            std::process::id(),
            NEXT_TEMP_PATH.fetch_add(1, Ordering::Relaxed),
        ))
    }

    #[test]
    fn controller_readiness_is_required_only_after_a_completed_recovery() {
        let calls = std::cell::Cell::new(0u32);
        confirm_recovered_native_apply_after_unlock_with(&NativeApplyRecoveryOutcome::None, || {
            calls.set(calls.get() + 1);
            Ok(())
        })
        .unwrap();
        assert_eq!(calls.get(), 0);

        let recovered = NativeApplyRecoveryOutcome::Recovered {
            authority: "existing_v4",
            job_id: "11".repeat(16),
            worker_run_id: "22".repeat(16),
            recovery_cleared: true,
            rolled_forward: false,
        };
        confirm_recovered_native_apply_after_unlock_with(&recovered, || {
            calls.set(calls.get() + 1);
            Ok(())
        })
        .unwrap();
        assert_eq!(calls.get(), 1);

        let error = confirm_native_apply_controllers_after_unlock_with(|| {
            Err("controller remains WAITING_OPERATION".to_string())
        })
        .unwrap_err();
        assert!(error.contains("after releasing the runtime lock"));
        assert!(error.contains("WAITING_OPERATION"));
    }

    fn test_native_apply_live_candidate(
        _state_dir: &Path,
        _job_id: &str,
        _option_id: Option<&str>,
    ) -> Result<NativeApplyLiveState, String> {
        Ok(NativeApplyLiveState::Candidate)
    }

    fn test_native_apply_live_original(
        _state_dir: &Path,
        _job_id: &str,
        _option_id: Option<&str>,
    ) -> Result<NativeApplyLiveState, String> {
        Ok(NativeApplyLiveState::Original)
    }

    fn test_native_apply_live_foreign(
        _state_dir: &Path,
        _job_id: &str,
        _option_id: Option<&str>,
    ) -> Result<NativeApplyLiveState, String> {
        Err("live configuration is neither the candidate nor the original baseline".to_string())
    }

    fn test_native_apply_recovery_pending(
        _state_dir: &Path,
        _job_id: &str,
        _option_id: Option<&str>,
    ) -> Result<NativeApplyLiveState, String> {
        Err("native Apply recovery remains pending".to_string())
    }

    fn terminalize_native_apply_for_test(
        daemon: &mut CalibrationDaemon,
        start: &NativeApplyControlRequest,
        outcome: NativeApplyTerminalOutcome,
        recovery_cleared: bool,
    ) -> NativeApplyTerminalRecord {
        let (_, effect) = daemon.handle_native_apply_control(start);
        assert_eq!(effect, ControlEffect::StateChanged);
        let accepted = daemon.native_apply_store.read_active().unwrap().unwrap();
        let validating = daemon
            .native_apply_store
            .mark_validating(&accepted)
            .unwrap();
        let applying = daemon
            .native_apply_store
            .mark_applying(
                &validating,
                NativeApplyVerifiedDispatchIdentity {
                    worker_run_id: "77".repeat(16),
                    source_manifest_sha256: "88".repeat(32),
                    manifest_schema_version: 6,
                    target_state: "existing_managed".to_string(),
                },
            )
            .unwrap();
        let terminal = NativeApplyTerminalRecord {
            dispatch: applying,
            outcome,
            recovery_cleared,
            diagnostic: if outcome.success() {
                String::new()
            } else {
                "test terminal failure".to_string()
            },
        };
        daemon.native_apply_store.complete(&terminal).unwrap();
        terminal
    }

    fn write_fake_process(proc_root: &Path, identity: &ProcessIdentity) {
        let directory = proc_root.join(identity.pid.to_string());
        fs::create_dir_all(&directory).unwrap();
        let mut tail = vec![
            "S".to_string(),
            "1".to_string(),
            identity.process_group.to_string(),
            identity.process_group.to_string(),
        ];
        while tail.len() < 19 {
            tail.push("0".to_string());
        }
        tail.push(identity.starttime_ticks.to_string());
        fs::write(
            directory.join("stat"),
            format!(
                "{} (native-apply-worker) {} 0",
                identity.pid,
                tail.join(" ")
            ),
        )
        .unwrap();
    }

    #[test]
    fn native_worker_arguments_are_exact_for_each_worker_kind() {
        let directory = PathBuf::from("/tmp/native-worker-arguments");
        let paths = NativeJobPaths {
            request: directory.join("request"),
            terminal: directory.join("terminal-run"),
            review: directory.join("review-run.json"),
            apply_manifest: directory.join("apply-manifest-run.json"),
            public_result: directory.join("public-review-run.json"),
            permit: directory.join("permit-run"),
            stdout: directory.join("stdout-run.log"),
            stderr: directory.join("stderr-run.log"),
            bootstrap_runtime_dir: directory.join("bootstrap-runtime-run"),
            bootstrap_runtime_stdout: directory.join("bootstrap-runtime-stdout-run.log"),
            bootstrap_runtime_stderr: directory.join("bootstrap-runtime-stderr-run.log"),
        };
        let as_strings = |kind: NativeWorkerKind| {
            kind.arguments(&paths, "abcdef0123456789abcdef0123456789")
                .into_iter()
                .map(|argument| argument.into_string().unwrap())
                .collect::<Vec<_>>()
        };

        let rating = as_strings(NativeWorkerKind::Rating);
        let speedtest = as_strings(NativeWorkerKind::Speedtest);
        let autotune = as_strings(NativeWorkerKind::Autotune);
        assert_eq!(rating[0], "--rating-worker");
        assert_eq!(speedtest[0], "--speedtest-worker");
        assert_eq!(autotune[0], "--autotune-worker");
        assert!(!rating.iter().any(|argument| argument == "--review"));
        assert!(!speedtest.iter().any(|argument| argument == "--review"));
        assert_eq!(
            autotune
                .windows(2)
                .find(|pair| pair[0] == "--review")
                .map(|pair| pair[1].as_str()),
            Some("/tmp/native-worker-arguments/review-run.json")
        );
        for arguments in [&rating, &speedtest, &autotune] {
            for required in ["--request", "--terminal", "--permit", "--worker-run-id"] {
                assert!(arguments.iter().any(|argument| argument == required));
            }
        }
        let owner = bootstrap_runtime_owner_arguments(&paths, "abcdef0123456789abcdef0123456789")
            .into_iter()
            .map(|argument| argument.into_string().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            owner,
            vec![
                "--bootstrap-runtime-owner",
                "--request",
                "/tmp/native-worker-arguments/request",
                "--runtime-dir",
                "/tmp/native-worker-arguments/bootstrap-runtime-run",
                "--worker-run-id",
                "abcdef0123456789abcdef0123456789",
            ]
        );
    }

    #[test]
    fn coordinator_adopts_the_exact_self_claimed_bootstrap_runtime_owner_before_permit() {
        let root = temp_path("bootstrap-owner-adoption");
        let mut daemon = CalibrationDaemon::bind_with_admission(&root, true).unwrap();
        daemon.native_autotune = true;
        daemon.bootstrap_runtime_attestor = allowing_bootstrap_runtime_attestor;
        let operation = bootstrap_operation_request('b', 'c', "wan");
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));

        let spawn_sleep = |name: &str| {
            let spec = SpawnSpec {
                program: PathBuf::from("/bin/bash"),
                arguments: vec![OsString::from("-c"), OsString::from("exec sleep 30")],
                environment: Vec::new(),
            };
            ManagedChild::spawn(
                &spec,
                private_log_file(&root.join(format!("{name}.stdout"))).unwrap(),
                private_log_file(&root.join(format!("{name}.stderr"))).unwrap(),
                Path::new(DEFAULT_PROC_ROOT),
            )
            .unwrap()
        };
        let mut worker = spawn_sleep("worker");
        let mut owner = spawn_sleep("owner");
        let worker_run_id = "d".repeat(32);
        let mut running = daemon.jobs[0].journal.clone();
        running.arm_native_worker(worker_run_id.clone()).unwrap();
        running
            .attach_native_running(worker.identity.clone(), worker_run_id.clone())
            .unwrap();
        daemon.journal_store.update(&running).unwrap();
        daemon.jobs[0].journal = running;
        daemon.jobs[0].disposition = JournalDisposition::LiveProcess;

        let paths = daemon
            .journal_store
            .native_job_paths(&operation.identity.job_id, &worker_run_id)
            .unwrap();
        fs::create_dir(&paths.bootstrap_runtime_dir).unwrap();
        fs::set_permissions(
            &paths.bootstrap_runtime_dir,
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let claim = BootstrapRuntimeOwnerClaim::for_process(
            &daemon.jobs[0].request,
            &worker_run_id,
            owner.identity.clone(),
            bootstrap_runtime_baseline(&daemon.jobs[0].request, '9'),
        )
        .unwrap();
        rating::atomic_private_write(
            &paths
                .bootstrap_runtime_dir
                .join("bootstrap-runtime-owner.claim"),
            claim.encode().unwrap().as_bytes(),
        )
        .unwrap();

        daemon.advance_bootstrap_runtime_owner(0);
        assert_eq!(
            daemon.jobs[0].journal.runtime_owner_process,
            Some(owner.identity.clone())
        );
        assert!(!daemon.jobs[0].journal.runtime_mutated);
        assert!(!daemon.jobs[0].journal.heavy_lease_acquired);
        daemon.advance_pending_runtime_permit(0);
        assert!(daemon.jobs[0].journal.heavy_lease_acquired);
        assert!(daemon.jobs[0].journal.runtime_mutated);
        assert!(daemon.jobs[0].journal.recovery_required);
        assert!(fs::symlink_metadata(&paths.permit).is_ok());
        assert!(daemon.jobs[0]
            .journal
            .encode()
            .unwrap()
            .starts_with("cake-autorate-calibration\t3\tjournal"));

        owner
            .terminate(Path::new(DEFAULT_PROC_ROOT), Duration::from_millis(500))
            .unwrap();
        worker
            .terminate(Path::new(DEFAULT_PROC_ROOT), Duration::from_millis(500))
            .unwrap();
        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn coordinator_restart_reconstructs_bootstrap_owner_and_acquires_heavy_lease_after_readiness() {
        let root = temp_path("bootstrap-owner-restart");
        fs::create_dir_all(&root).unwrap();
        let operation = bootstrap_operation_request('2', '3', "wan");
        let spawn_sleep = |name: &str| {
            ManagedChild::spawn(
                &SpawnSpec {
                    program: PathBuf::from("/bin/sleep"),
                    arguments: vec![OsString::from("30")],
                    environment: Vec::new(),
                },
                private_log_file(&root.join(format!("{name}.stdout"))).unwrap(),
                private_log_file(&root.join(format!("{name}.stderr"))).unwrap(),
                Path::new(DEFAULT_PROC_ROOT),
            )
            .unwrap()
        };
        let mut worker = spawn_sleep("worker");
        let mut owner = spawn_sleep("owner");
        let worker_run_id = "4".repeat(32);

        let paths = {
            let mut daemon = CalibrationDaemon::bind_with_admission(&root, true).unwrap();
            daemon.native_autotune = true;
            assert!(daemon
                .handle(&job_message(ControlCommand::Start, &operation))
                .contains("\"state\":\"queued\""));
            let mut running = daemon.jobs[0].journal.clone();
            running.arm_native_worker(worker_run_id.clone()).unwrap();
            running
                .attach_native_running(worker.identity.clone(), worker_run_id.clone())
                .unwrap();
            running
                .attach_bootstrap_runtime_owner(owner.identity.clone())
                .unwrap();
            daemon.journal_store.update(&running).unwrap();
            daemon.jobs[0].journal = running;
            daemon.jobs[0].disposition = JournalDisposition::LiveProcess;
            let paths = daemon
                .journal_store
                .native_job_paths(&operation.identity.job_id, &worker_run_id)
                .unwrap();
            fs::create_dir(&paths.bootstrap_runtime_dir).unwrap();
            fs::set_permissions(
                &paths.bootstrap_runtime_dir,
                fs::Permissions::from_mode(0o700),
            )
            .unwrap();
            let claim = BootstrapRuntimeOwnerClaim::for_process(
                &daemon.jobs[0].request,
                &worker_run_id,
                owner.identity.clone(),
                bootstrap_runtime_baseline(&daemon.jobs[0].request, '5'),
            )
            .unwrap();
            rating::atomic_private_write(
                &paths
                    .bootstrap_runtime_dir
                    .join("bootstrap-runtime-owner.claim"),
                claim.encode().unwrap().as_bytes(),
            )
            .unwrap();
            assert!(!daemon.jobs[0].journal.heavy_lease_acquired);
            paths
        };

        let mut restarted = CalibrationDaemon::bind_with_admission(&root, true).unwrap();
        restarted.native_autotune = true;
        restarted.bootstrap_runtime_attestor = allowing_bootstrap_runtime_attestor;
        assert_eq!(restarted.jobs.len(), 1);
        assert_eq!(
            restarted.jobs[0].disposition,
            JournalDisposition::LiveProcess
        );
        assert_eq!(
            restarted.jobs[0].journal.process,
            Some(worker.identity.clone())
        );
        assert_eq!(
            restarted.jobs[0].journal.runtime_owner_process,
            Some(owner.identity.clone())
        );
        assert!(!restarted.jobs[0].journal.heavy_lease_acquired);

        restarted.advance_pending_runtime_permit(0);

        assert!(restarted.jobs[0].journal.heavy_lease_acquired);
        assert!(restarted.jobs[0].journal.runtime_mutated);
        assert!(restarted.jobs[0].journal.recovery_required);
        assert!(fs::symlink_metadata(&paths.permit).is_ok());

        owner
            .terminate(Path::new(DEFAULT_PROC_ROOT), Duration::from_millis(500))
            .unwrap();
        worker
            .terminate(Path::new(DEFAULT_PROC_ROOT), Duration::from_millis(500))
            .unwrap();
        drop(restarted);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovering_bootstrap_detaches_a_dead_prior_owner_before_attaching_its_replacement() {
        let root = temp_path("bootstrap-owner-recovery-replacement");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&root, true).unwrap();
        daemon.native_autotune = true;
        let operation = bootstrap_operation_request('6', '7', "wan");
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));

        let spawn_sleep = |name: &str| {
            ManagedChild::spawn(
                &SpawnSpec {
                    program: PathBuf::from("/bin/sleep"),
                    arguments: vec![OsString::from("30")],
                    environment: Vec::new(),
                },
                private_log_file(&root.join(format!("{name}.stdout"))).unwrap(),
                private_log_file(&root.join(format!("{name}.stderr"))).unwrap(),
                Path::new(DEFAULT_PROC_ROOT),
            )
            .unwrap()
        };
        let mut worker = spawn_sleep("worker");
        let mut prior_owner = spawn_sleep("prior-owner");
        let mut replacement = spawn_sleep("replacement-owner");
        replacement.preserve_on_drop();
        let replacement_identity = replacement.identity.clone();
        let worker_run_id = "8".repeat(32);

        let mut recovering = daemon.jobs[0].journal.clone();
        recovering.arm_native_worker(worker_run_id.clone()).unwrap();
        recovering
            .attach_native_running(worker.identity.clone(), worker_run_id.clone())
            .unwrap();
        recovering
            .attach_bootstrap_runtime_owner(prior_owner.identity.clone())
            .unwrap();
        recovering.mark_bootstrap_heavy_lease_acquired().unwrap();
        recovering
            .require_recovery("startup-reconciliation-required")
            .unwrap();
        daemon.journal_store.update(&recovering).unwrap();
        daemon.jobs[0].journal = recovering;
        daemon.jobs[0].disposition = JournalDisposition::RecoveryRequired;

        let paths = daemon
            .journal_store
            .native_job_paths(&operation.identity.job_id, &worker_run_id)
            .unwrap();
        fs::create_dir(&paths.bootstrap_runtime_dir).unwrap();
        fs::set_permissions(
            &paths.bootstrap_runtime_dir,
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let baseline = bootstrap_runtime_baseline(&daemon.jobs[0].request, '9');
        let permit = daemon
            .assemble_bootstrap_runtime_override_permit(
                &daemon.jobs[0].request,
                &worker_run_id,
                &worker.identity,
                baseline.kernel_namespace_seed.clone(),
                baseline.clone(),
            )
            .unwrap();
        RuntimeOverrideStore::open(&paths.bootstrap_runtime_dir)
            .unwrap()
            .publish_permit(&permit)
            .unwrap();
        daemon
            .bootstrap_runtime_children
            .insert(operation.identity.job_id.clone(), replacement);

        // A replacement is not authority while the prior exact process is
        // still live, even though it is already a coordinator-owned child.
        daemon.advance_bootstrap_runtime_owner(0);
        assert_eq!(
            daemon.jobs[0].journal.runtime_owner_process,
            Some(prior_owner.identity.clone())
        );
        assert!(daemon
            .job_errors
            .get(&operation.identity.job_id)
            .unwrap()
            .contains("prior owner is still live"));

        prior_owner
            .terminate(Path::new(DEFAULT_PROC_ROOT), Duration::from_millis(500))
            .unwrap();
        daemon.advance_bootstrap_runtime_owner(0);
        assert!(daemon.jobs[0].journal.runtime_owner_process.is_none());
        assert!(!daemon.job_errors.contains_key(&operation.identity.job_id));
        let persisted = JobJournal::decode(
            &fs::read_to_string(
                root.join("jobs")
                    .join(&operation.identity.job_id)
                    .join("state"),
            )
            .unwrap(),
        )
        .unwrap();
        assert!(persisted.runtime_owner_process.is_none());

        let claim = BootstrapRuntimeOwnerClaim::for_process(
            &daemon.jobs[0].request,
            &worker_run_id,
            replacement_identity.clone(),
            baseline,
        )
        .unwrap();
        rating::atomic_private_write(
            &paths
                .bootstrap_runtime_dir
                .join("bootstrap-runtime-owner.claim"),
            claim.encode().unwrap().as_bytes(),
        )
        .unwrap();
        daemon.advance_bootstrap_runtime_owner(0);
        assert_eq!(
            daemon.jobs[0].journal.runtime_owner_process,
            Some(replacement_identity.clone())
        );
        assert!(!daemon.job_errors.contains_key(&operation.identity.job_id));
        let persisted = JobJournal::decode(
            &fs::read_to_string(
                root.join("jobs")
                    .join(&operation.identity.job_id)
                    .join("state"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(persisted.runtime_owner_process, Some(replacement_identity));

        let mut replacement = daemon
            .bootstrap_runtime_children
            .remove(&operation.identity.job_id)
            .unwrap();
        replacement
            .terminate(Path::new(DEFAULT_PROC_ROOT), Duration::from_millis(500))
            .unwrap();
        worker
            .terminate(Path::new(DEFAULT_PROC_ROOT), Duration::from_millis(500))
            .unwrap();
        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bootstrap_recovery_state_without_permit_never_spawns_an_owner() {
        let root = temp_path("bootstrap-recovery-state-without-permit");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&root, true).unwrap();
        daemon.native_autotune = true;
        let operation = bootstrap_operation_request('e', 'f', "wan");
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));

        let spawn_sleep = |name: &str| {
            ManagedChild::spawn(
                &SpawnSpec {
                    program: PathBuf::from("/bin/sleep"),
                    arguments: vec![OsString::from("30")],
                    environment: Vec::new(),
                },
                private_log_file(&root.join(format!("{name}.stdout"))).unwrap(),
                private_log_file(&root.join(format!("{name}.stderr"))).unwrap(),
                Path::new(DEFAULT_PROC_ROOT),
            )
            .unwrap()
        };
        let mut worker = spawn_sleep("worker");
        let mut owner = spawn_sleep("owner");
        let worker_run_id = "1".repeat(32);
        let mut recovering = daemon.jobs[0].journal.clone();
        recovering.arm_native_worker(worker_run_id.clone()).unwrap();
        recovering
            .attach_native_running(worker.identity.clone(), worker_run_id.clone())
            .unwrap();
        recovering
            .attach_bootstrap_runtime_owner(owner.identity.clone())
            .unwrap();
        recovering.mark_bootstrap_heavy_lease_acquired().unwrap();
        recovering
            .arm_attached_bootstrap_runtime_mutation()
            .unwrap();
        recovering
            .require_recovery("startup-reconciliation-required")
            .unwrap();
        recovering.clear_bootstrap_runtime_owner().unwrap();
        daemon.journal_store.update(&recovering).unwrap();
        daemon.jobs[0].journal = recovering;
        daemon.jobs[0].disposition = JournalDisposition::RecoveryRequired;

        let paths = daemon
            .journal_store
            .native_job_paths(&operation.identity.job_id, &worker_run_id)
            .unwrap();
        fs::create_dir(&paths.bootstrap_runtime_dir).unwrap();
        fs::set_permissions(
            &paths.bootstrap_runtime_dir,
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        RuntimeOverrideStore::open(&paths.bootstrap_runtime_dir)
            .unwrap()
            .publish_restore_intent(&full_autotune::AutotuneRuntimeControl {
                permit_id: "2".repeat(32),
                job_id: operation.identity.job_id.clone(),
                worker_run_id: worker_run_id.clone(),
                boot_id: daemon.coordinator.boot_id.clone(),
                coordinator_generation: daemon.coordinator.generation.clone(),
                worker: worker.identity.clone(),
                sequence: 1,
                deadline_boot_ms: 1,
                target_interface: operation.identity.target_interface.clone(),
                route_fingerprint: operation.identity.route_fingerprint.clone(),
                sqm_fingerprint: operation.identity.sqm_fingerprint.clone(),
                topology: full_autotune::MeasurementTopology::RawBoth,
                download_kbps: None,
                upload_kbps: None,
            })
            .unwrap();

        daemon.advance_bootstrap_runtime_owner(0);

        assert!(daemon.bootstrap_runtime_children.is_empty());
        assert!(daemon.jobs[0].journal.runtime_owner_process.is_none());
        assert!(daemon
            .job_errors
            .get(&operation.identity.job_id)
            .unwrap()
            .contains("control state exists without its exact permit"));

        owner
            .terminate(Path::new(DEFAULT_PROC_ROOT), Duration::from_millis(500))
            .unwrap();
        worker
            .terminate(Path::new(DEFAULT_PROC_ROOT), Duration::from_millis(500))
            .unwrap();
        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_restored_bootstrap_residue_after_permit_withdrawal_survives_restart_and_settles() {
        let root = temp_path("bootstrap-restored-permit-withdrawal-residue");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&root, true).unwrap();
        daemon.native_autotune = true;
        daemon.bootstrap_runtime_attestor = allowing_bootstrap_runtime_attestor;
        daemon.route_pin_cleaner = allowing_route_pin_cleaner;
        let operation = bootstrap_operation_request('2', '3', "wan");
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));

        let spawn_sleep = |name: &str| {
            ManagedChild::spawn(
                &SpawnSpec {
                    program: PathBuf::from("/bin/sleep"),
                    arguments: vec![OsString::from("30")],
                    environment: Vec::new(),
                },
                private_log_file(&root.join(format!("{name}.stdout"))).unwrap(),
                private_log_file(&root.join(format!("{name}.stderr"))).unwrap(),
                Path::new(DEFAULT_PROC_ROOT),
            )
            .unwrap()
        };
        let mut worker = spawn_sleep("worker");
        let mut owner = spawn_sleep("owner");
        let worker_run_id = "4".repeat(32);
        let baseline = bootstrap_runtime_baseline(&operation, '5');
        let permit = daemon
            .assemble_bootstrap_runtime_override_permit(
                &operation,
                &worker_run_id,
                &worker.identity,
                baseline.kernel_namespace_seed.clone(),
                baseline.clone(),
            )
            .unwrap();

        let mut running = daemon.jobs[0].journal.clone();
        running.arm_native_worker(worker_run_id.clone()).unwrap();
        running
            .attach_native_running(worker.identity.clone(), worker_run_id.clone())
            .unwrap();
        running
            .attach_bootstrap_runtime_owner(owner.identity.clone())
            .unwrap();
        daemon.journal_store.update(&running).unwrap();
        daemon.jobs[0].journal = running;
        daemon.jobs[0].disposition = JournalDisposition::LiveProcess;
        assert!(daemon.acquire_heavy_lease_if_needed(0));

        let paths = daemon
            .journal_store
            .native_job_paths(&operation.identity.job_id, &worker_run_id)
            .unwrap();
        fs::create_dir(&paths.bootstrap_runtime_dir).unwrap();
        fs::set_permissions(
            &paths.bootstrap_runtime_dir,
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let store = RuntimeOverrideStore::open(&paths.bootstrap_runtime_dir).unwrap();
        let checkpoint = super::super::autotune_runtime_store::RuntimeOverrideCheckpoint::new(
            &permit,
            monotonic_boot_ms().unwrap(),
            RuntimeBaseline::Absent(baseline.clone()),
        )
        .unwrap()
        .advance_temporary_stage(TemporaryTopologyStage::AbsenceAttested, None)
        .unwrap()
        .advance_temporary_stage(TemporaryTopologyStage::LinkOwned, Some(23))
        .unwrap()
        .advance_temporary_stage(TemporaryTopologyStage::TemporaryAbsent, None)
        .unwrap()
        .advance_temporary_stage(TemporaryTopologyStage::BaselineRestored, None)
        .unwrap();
        store.publish_checkpoint(&checkpoint).unwrap();
        let restored_ack = full_autotune::AutotuneRuntimeAck {
            permit_id: permit.permit_id.clone(),
            job_id: operation.identity.job_id.clone(),
            worker_run_id: worker_run_id.clone(),
            sequence: 1,
            updated_boot_ms: monotonic_boot_ms().unwrap(),
            target_interface: operation.identity.target_interface.clone(),
            route_fingerprint: operation.identity.route_fingerprint.clone(),
            sqm_fingerprint: operation.identity.sqm_fingerprint.clone(),
            state: RuntimeAckState::Restored,
            topology: None,
            download_kbps: None,
            upload_kbps: None,
            diagnostic_code: None,
        };
        store.publish_ack(&restored_ack).unwrap();
        let claim = BootstrapRuntimeOwnerClaim::for_process(
            &operation,
            &worker_run_id,
            owner.identity.clone(),
            baseline,
        )
        .unwrap();
        let claim_path = paths
            .bootstrap_runtime_dir
            .join("bootstrap-runtime-owner.claim");
        rating::atomic_private_write(&claim_path, claim.encode().unwrap().as_bytes()).unwrap();
        full_autotune::publish_terminal_file(
            &paths.terminal,
            &full_autotune::AutotuneTerminalRecord {
                job_id: operation.identity.job_id.clone(),
                worker_run_id: worker_run_id.clone(),
                consumed_traffic_bytes: 0,
                terminal: AutotuneTerminal::Cancelled,
            },
        )
        .unwrap();

        let mut recovering = daemon.jobs[0].journal.clone();
        recovering
            .arm_attached_bootstrap_runtime_mutation()
            .unwrap();
        recovering
            .require_recovery("cancelled-worker-runtime-recovery")
            .unwrap();
        recovering.clear_bootstrap_runtime_owner().unwrap();
        daemon.journal_store.update(&recovering).unwrap();
        daemon.jobs[0].journal = recovering;
        daemon.jobs[0].disposition = JournalDisposition::RecoveryRequired;
        worker
            .terminate(Path::new(DEFAULT_PROC_ROOT), Duration::from_millis(500))
            .unwrap();
        owner
            .terminate(Path::new(DEFAULT_PROC_ROOT), Duration::from_millis(500))
            .unwrap();

        let hidden_claim = paths.bootstrap_runtime_dir.join("owner.claim.hidden");
        fs::rename(&claim_path, &hidden_claim).unwrap();
        assert!(daemon
            .bootstrap_no_owner_recovery_readiness(0, &paths, &worker_run_id)
            .unwrap_err()
            .contains("has no exact bootstrap owner claim"));
        fs::rename(&hidden_claim, &claim_path).unwrap();

        let mut foreign_ack = restored_ack.clone();
        foreign_ack.sqm_fingerprint = "f".repeat(64);
        store.publish_ack(&foreign_ack).unwrap();
        assert!(daemon
            .bootstrap_no_owner_recovery_readiness(0, &paths, &worker_run_id)
            .unwrap_err()
            .contains("acknowledgement identity mismatch"));
        assert!(store.read_checkpoint().unwrap().is_some());
        assert!(store.read_ack().unwrap().is_some());
        store.publish_ack(&restored_ack).unwrap();

        let records = paths.bootstrap_runtime_dir.join("autotune-runtime");
        fs::remove_file(records.join("ack.record")).unwrap();
        assert!(daemon
            .bootstrap_no_owner_recovery_readiness(0, &paths, &worker_run_id)
            .unwrap_err()
            .contains("has no RESTORED acknowledgement"));
        assert!(store.read_checkpoint().unwrap().is_some());
        store.publish_ack(&restored_ack).unwrap();

        fs::remove_file(records.join("checkpoint.record")).unwrap();
        assert_eq!(
            daemon
                .bootstrap_no_owner_recovery_readiness(0, &paths, &worker_run_id)
                .unwrap(),
            BootstrapNoOwnerRecoveryReadiness::RestoredFinalizationPending
        );
        store.publish_checkpoint(&checkpoint).unwrap();
        let issuing_generation = daemon.coordinator.generation.clone();
        assert_eq!(checkpoint.coordinator_generation, issuing_generation);
        drop(daemon);

        let mut daemon = CalibrationDaemon::bind_with_admission(&root, true).unwrap();
        daemon.native_autotune = true;
        daemon.bootstrap_runtime_attestor = allowing_bootstrap_runtime_attestor;
        daemon.route_pin_cleaner = allowing_route_pin_cleaner;
        assert_ne!(daemon.coordinator.generation, issuing_generation);
        assert_eq!(
            daemon.jobs[0].journal.coordinator_generation,
            daemon.coordinator.generation
        );
        assert_eq!(
            store
                .read_checkpoint()
                .unwrap()
                .unwrap()
                .coordinator_generation,
            issuing_generation
        );
        assert_eq!(
            daemon
                .bootstrap_no_owner_recovery_readiness(0, &paths, &worker_run_id)
                .unwrap(),
            BootstrapNoOwnerRecoveryReadiness::RestoredFinalizationPending
        );

        daemon.poll_native_runtime_recoveries();

        assert_eq!(daemon.jobs[0].disposition, JournalDisposition::Settled);
        assert_eq!(
            daemon.jobs[0].journal.state,
            super::super::protocol::OperationState::Cancelled
        );
        assert!(!daemon.jobs[0].journal.runtime_mutated);
        assert!(!daemon.jobs[0].journal.recovery_required);
        assert!(!daemon.jobs[0].journal.heavy_lease_acquired);
        assert_eq!(daemon.leases.job_count(), 0);
        assert!(store.read_permit().unwrap().is_none());
        assert!(store.read_control().unwrap().is_none());
        assert!(store.read_restore_intent().unwrap().is_none());
        assert!(store.read_ack().unwrap().is_none());
        assert!(store.read_checkpoint().unwrap().is_none());
        assert!(!daemon.job_errors.contains_key(&operation.identity.job_id));

        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_bootstrap_recovery_settles_from_exact_absence_without_respawn() {
        let root = temp_path("bootstrap-empty-owner-free-recovery");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&root, true).unwrap();
        daemon.native_autotune = true;
        daemon.bootstrap_runtime_attestor = allowing_bootstrap_runtime_attestor;
        daemon.route_pin_cleaner = allowing_route_pin_cleaner;
        let operation = bootstrap_operation_request('4', '5', "wan");
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));

        let spawn_sleep = |name: &str| {
            ManagedChild::spawn(
                &SpawnSpec {
                    program: PathBuf::from("/bin/sleep"),
                    arguments: vec![OsString::from("30")],
                    environment: Vec::new(),
                },
                private_log_file(&root.join(format!("{name}.stdout"))).unwrap(),
                private_log_file(&root.join(format!("{name}.stderr"))).unwrap(),
                Path::new(DEFAULT_PROC_ROOT),
            )
            .unwrap()
        };
        let mut worker = spawn_sleep("worker");
        let mut owner = spawn_sleep("owner");
        owner.preserve_on_drop();
        let owner_identity = owner.identity.clone();
        let worker_run_id = "6".repeat(32);

        let mut running = daemon.jobs[0].journal.clone();
        running.arm_native_worker(worker_run_id.clone()).unwrap();
        running
            .attach_native_running(worker.identity.clone(), worker_run_id.clone())
            .unwrap();
        running
            .attach_bootstrap_runtime_owner(owner_identity.clone())
            .unwrap();
        daemon.journal_store.update(&running).unwrap();
        daemon.jobs[0].journal = running;
        daemon.jobs[0].disposition = JournalDisposition::LiveProcess;

        let paths = daemon
            .journal_store
            .native_job_paths(&operation.identity.job_id, &worker_run_id)
            .unwrap();
        fs::create_dir(&paths.bootstrap_runtime_dir).unwrap();
        fs::set_permissions(
            &paths.bootstrap_runtime_dir,
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let claim = BootstrapRuntimeOwnerClaim::for_process(
            &daemon.jobs[0].request,
            &worker_run_id,
            owner_identity,
            bootstrap_runtime_baseline(&daemon.jobs[0].request, '7'),
        )
        .unwrap();
        rating::atomic_private_write(
            &paths
                .bootstrap_runtime_dir
                .join("bootstrap-runtime-owner.claim"),
            claim.encode().unwrap().as_bytes(),
        )
        .unwrap();
        assert!(daemon.acquire_heavy_lease_if_needed(0));
        let mut recovering = daemon.jobs[0].journal.clone();
        recovering
            .arm_attached_bootstrap_runtime_mutation()
            .unwrap();
        recovering
            .require_recovery("startup-reconciliation-required")
            .unwrap();
        recovering.clear_bootstrap_runtime_owner().unwrap();
        daemon.journal_store.update(&recovering).unwrap();
        daemon.jobs[0].journal = recovering;
        daemon.jobs[0].disposition = JournalDisposition::RecoveryRequired;
        full_autotune::publish_terminal_file(
            &paths.terminal,
            &full_autotune::AutotuneTerminalRecord {
                job_id: operation.identity.job_id.clone(),
                worker_run_id: worker_run_id.clone(),
                consumed_traffic_bytes: 0,
                terminal: AutotuneTerminal::Cancelled,
            },
        )
        .unwrap();
        daemon
            .bootstrap_runtime_children
            .insert(operation.identity.job_id.clone(), owner);

        worker
            .terminate(Path::new(DEFAULT_PROC_ROOT), Duration::from_millis(500))
            .unwrap();

        daemon.advance_bootstrap_runtime_owner(0);
        assert!(daemon.jobs[0].journal.runtime_owner_process.is_none());
        assert!(daemon
            .bootstrap_runtime_children
            .contains_key(&operation.identity.job_id));
        assert!(daemon.job_errors[&operation.identity.job_id]
            .contains("waiting for the read-only owner to exit"));
        for _ in 0..200 {
            daemon.advance_bootstrap_runtime_owner(0);
            if daemon.bootstrap_runtime_children.is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(daemon.bootstrap_runtime_children.is_empty());
        daemon.advance_bootstrap_runtime_owner(0);
        assert!(!daemon.job_errors.contains_key(&operation.identity.job_id));

        daemon.poll_native_runtime_recoveries();

        assert_eq!(daemon.jobs[0].disposition, JournalDisposition::Settled);
        assert_eq!(
            daemon.jobs[0].journal.state,
            super::super::protocol::OperationState::Cancelled
        );
        assert!(!daemon.jobs[0].journal.runtime_mutated);
        assert!(!daemon.jobs[0].journal.recovery_required);
        assert!(!daemon.jobs[0].journal.heavy_lease_acquired);
        assert_eq!(daemon.leases.job_count(), 0);
        assert!(!daemon.job_errors.contains_key(&operation.identity.job_id));

        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_bootstrap_recovery_without_claim_captures_absence_without_respawn() {
        let root = temp_path("bootstrap-empty-owner-free-recovery-without-claim");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&root, true).unwrap();
        daemon.native_autotune = true;
        daemon.bootstrap_runtime_attestor = allowing_bootstrap_runtime_attestor;
        daemon.bootstrap_runtime_baseline_capturer = allowing_bootstrap_runtime_baseline_capturer;
        daemon.route_pin_cleaner = allowing_route_pin_cleaner;
        let mut operation = bootstrap_operation_request('a', 'c', "wan");
        operation.deadline_unix_ms = operation.created_unix_ms;
        assert!(rating::epoch_ms().unwrap() >= operation.deadline_unix_ms);
        operation.validate_admission_policy().unwrap();
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));

        let spawn_sleep = |name: &str| {
            ManagedChild::spawn(
                &SpawnSpec {
                    program: PathBuf::from("/bin/sleep"),
                    arguments: vec![OsString::from("30")],
                    environment: Vec::new(),
                },
                private_log_file(&root.join(format!("{name}.stdout"))).unwrap(),
                private_log_file(&root.join(format!("{name}.stderr"))).unwrap(),
                Path::new(DEFAULT_PROC_ROOT),
            )
            .unwrap()
        };
        let mut worker = spawn_sleep("worker");
        let mut owner = spawn_sleep("owner");
        let worker_run_id = "d".repeat(32);

        let mut running = daemon.jobs[0].journal.clone();
        running.arm_native_worker(worker_run_id.clone()).unwrap();
        running
            .attach_native_running(worker.identity.clone(), worker_run_id.clone())
            .unwrap();
        running
            .attach_bootstrap_runtime_owner(owner.identity.clone())
            .unwrap();
        daemon.journal_store.update(&running).unwrap();
        daemon.jobs[0].journal = running;
        daemon.jobs[0].disposition = JournalDisposition::LiveProcess;

        let paths = daemon
            .journal_store
            .native_job_paths(&operation.identity.job_id, &worker_run_id)
            .unwrap();
        fs::create_dir(&paths.bootstrap_runtime_dir).unwrap();
        fs::set_permissions(
            &paths.bootstrap_runtime_dir,
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        assert!(daemon.acquire_heavy_lease_if_needed(0));
        let mut recovering = daemon.jobs[0].journal.clone();
        recovering
            .arm_attached_bootstrap_runtime_mutation()
            .unwrap();
        recovering
            .require_recovery("startup-reconciliation-required")
            .unwrap();
        recovering.clear_bootstrap_runtime_owner().unwrap();
        daemon.journal_store.update(&recovering).unwrap();
        daemon.jobs[0].journal = recovering;
        daemon.jobs[0].disposition = JournalDisposition::RecoveryRequired;
        full_autotune::publish_terminal_file(
            &paths.terminal,
            &full_autotune::AutotuneTerminalRecord {
                job_id: operation.identity.job_id.clone(),
                worker_run_id: worker_run_id.clone(),
                consumed_traffic_bytes: 0,
                terminal: AutotuneTerminal::Cancelled,
            },
        )
        .unwrap();

        worker
            .terminate(Path::new(DEFAULT_PROC_ROOT), Duration::from_millis(500))
            .unwrap();
        owner
            .terminate(Path::new(DEFAULT_PROC_ROOT), Duration::from_millis(500))
            .unwrap();

        assert!(fs::symlink_metadata(
            paths
                .bootstrap_runtime_dir
                .join("bootstrap-runtime-owner.claim")
        )
        .is_err());
        daemon.advance_bootstrap_runtime_owner(0);
        assert!(daemon.bootstrap_runtime_children.is_empty());
        assert!(daemon.jobs[0].journal.runtime_owner_process.is_none());
        assert!(!daemon.job_errors.contains_key(&operation.identity.job_id));

        daemon.poll_native_runtime_recoveries();

        assert_eq!(daemon.jobs[0].disposition, JournalDisposition::Settled);
        assert_eq!(
            daemon.jobs[0].journal.state,
            super::super::protocol::OperationState::Cancelled
        );
        assert!(!daemon.jobs[0].journal.runtime_mutated);
        assert!(!daemon.jobs[0].journal.recovery_required);
        assert!(!daemon.jobs[0].journal.heavy_lease_acquired);
        assert_eq!(daemon.leases.job_count(), 0);
        assert!(!daemon.job_errors.contains_key(&operation.identity.job_id));

        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bootstrap_readiness_drift_stops_both_processes_before_runtime_mutation() {
        let root = temp_path("bootstrap-readiness-drift");
        let mut daemon = CalibrationDaemon::bind_with_admission(&root, true).unwrap();
        daemon.native_autotune = true;
        daemon.bootstrap_runtime_attestor = rejecting_bootstrap_runtime_attestor;
        daemon.route_pin_cleaner = allowing_route_pin_cleaner;
        let operation = bootstrap_operation_request('d', 'e', "wan");
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));

        let spawn_sleep = |name: &str| {
            ManagedChild::spawn(
                &SpawnSpec {
                    program: PathBuf::from("/bin/sleep"),
                    arguments: vec![OsString::from("30")],
                    environment: Vec::new(),
                },
                private_log_file(&root.join(format!("{name}.stdout"))).unwrap(),
                private_log_file(&root.join(format!("{name}.stderr"))).unwrap(),
                Path::new(DEFAULT_PROC_ROOT),
            )
            .unwrap()
        };
        let mut worker = spawn_sleep("worker");
        let mut owner = spawn_sleep("owner");
        let worker_run_id = "f".repeat(32);
        let mut running = daemon.jobs[0].journal.clone();
        running.arm_native_worker(worker_run_id.clone()).unwrap();
        running
            .attach_native_running(worker.identity.clone(), worker_run_id.clone())
            .unwrap();
        daemon.journal_store.update(&running).unwrap();
        daemon.jobs[0].journal = running;
        daemon.jobs[0].disposition = JournalDisposition::LiveProcess;

        let paths = daemon
            .journal_store
            .native_job_paths(&operation.identity.job_id, &worker_run_id)
            .unwrap();
        fs::create_dir(&paths.bootstrap_runtime_dir).unwrap();
        fs::set_permissions(
            &paths.bootstrap_runtime_dir,
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let claim = BootstrapRuntimeOwnerClaim::for_process(
            &daemon.jobs[0].request,
            &worker_run_id,
            owner.identity.clone(),
            bootstrap_runtime_baseline(&daemon.jobs[0].request, '1'),
        )
        .unwrap();
        rating::atomic_private_write(
            &paths
                .bootstrap_runtime_dir
                .join("bootstrap-runtime-owner.claim"),
            claim.encode().unwrap().as_bytes(),
        )
        .unwrap();

        daemon.advance_bootstrap_runtime_owner(0);
        assert!(!daemon.jobs[0].journal.heavy_lease_acquired);
        daemon.advance_pending_runtime_permit(0);
        assert!(!daemon.jobs[0].journal.runtime_mutated);
        assert!(daemon.jobs[0].journal.heavy_lease_acquired);
        assert_eq!(
            daemon.jobs[0].journal.state,
            super::super::protocol::OperationState::Cancelling
        );
        assert!(daemon
            .cancellations
            .contains_key(&operation.identity.job_id));
        assert!(fs::symlink_metadata(&paths.permit).is_err());

        assert!(worker
            .wait_for_exit(Duration::from_secs(2))
            .unwrap()
            .is_some());
        assert!(owner
            .wait_for_exit(Duration::from_secs(2))
            .unwrap()
            .is_some());
        daemon.poll_worker_cancellations();

        assert_eq!(daemon.jobs[0].disposition, JournalDisposition::Settled);
        assert_eq!(
            daemon.jobs[0].journal.state,
            super::super::protocol::OperationState::Failed
        );
        assert_eq!(
            daemon.jobs[0].journal.diagnostic_code.as_deref(),
            Some("bootstrap-runtime-readiness-changed")
        );
        assert!(!daemon.jobs[0].journal.runtime_mutated);
        assert!(!daemon.jobs[0].journal.heavy_lease_acquired);
        assert!(daemon.jobs[0].journal.runtime_owner_process.is_none());

        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bootstrap_runtime_permit_waits_for_a_durably_attached_owner() {
        let root = temp_path("bootstrap-owner-permit-wait");
        let mut daemon = CalibrationDaemon::bind_with_admission(&root, true).unwrap();
        daemon.native_autotune = true;
        let operation = bootstrap_operation_request('b', 'c', "wan");
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));

        let spec = SpawnSpec {
            program: PathBuf::from("/bin/bash"),
            arguments: vec![OsString::from("-c"), OsString::from("exec sleep 30")],
            environment: Vec::new(),
        };
        let mut worker = ManagedChild::spawn(
            &spec,
            private_log_file(&root.join("worker-wait.stdout")).unwrap(),
            private_log_file(&root.join("worker-wait.stderr")).unwrap(),
            Path::new(DEFAULT_PROC_ROOT),
        )
        .unwrap();
        let worker_run_id = "d".repeat(32);
        let mut running = daemon.jobs[0].journal.clone();
        running.arm_native_worker(worker_run_id.clone()).unwrap();
        running
            .attach_native_running(worker.identity.clone(), worker_run_id.clone())
            .unwrap();
        daemon.journal_store.update(&running).unwrap();
        daemon.jobs[0].journal = running;
        daemon.jobs[0].disposition = JournalDisposition::LiveProcess;

        daemon.advance_pending_runtime_permit(0);

        assert!(!daemon.jobs[0].journal.runtime_mutated);
        assert!(!daemon.jobs[0].journal.heavy_lease_acquired);
        assert_eq!(
            daemon.jobs[0].journal.state,
            super::super::protocol::OperationState::Running
        );
        assert!(daemon.cancellations.is_empty());
        assert!(daemon.job_errors[&operation.identity.job_id]
            .contains("bootstrap-runtime-owner-waiting"));
        assert!(worker
            .identity
            .still_matches(Path::new(DEFAULT_PROC_ROOT))
            .unwrap());
        let paths = daemon
            .journal_store
            .native_job_paths(&operation.identity.job_id, &worker_run_id)
            .unwrap();
        assert!(!paths.bootstrap_runtime_dir.join("permit").exists());

        worker
            .terminate(Path::new(DEFAULT_PROC_ROOT), Duration::from_millis(500))
            .unwrap();
        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_search_permit_covers_every_bounded_iteration_and_enforces_service_caps() {
        let mut shaped = operation_request('a', 'b', "wan");
        shaped.strategy = Some(CalibrationStrategy::ShapedOnly);
        shaped.allow_sqm_disable = false;
        let shaped_policy = full_autotune::native_autotune_runtime_permit_policy(&shaped).unwrap();
        assert_eq!(
            shaped_policy.maximum_sequence,
            1 + crate::autotune::MAX_PROFILE_REVIEW_OPTIONS as u32
                + (crate::autotune::MAX_PROFILE_SEARCH_OBSERVATIONS * 2) as u32
                + full_autotune::MAX_RUNTIME_ROUTE_REARMS
        );
        assert!(!shaped_policy.allow_directional_bypass);
        assert_eq!(shaped_policy.download_bounds.minimum_kbps, 100);
        assert_eq!(
            shaped_policy.download_bounds.maximum_kbps,
            crate::autotune::MAX_RATE_KBPS
        );

        let mut full_raw = operation_request('a', 'b', "wan");
        full_raw.service_dl_cap_kbps = Some(900_000);
        let raw_policy = full_autotune::native_autotune_runtime_permit_policy(&full_raw).unwrap();
        assert_eq!(
            raw_policy.maximum_sequence,
            // Three raw controls, one explicit-mobile terminal download-bypass
            // control, up to three confirmed-pair controls, and at most two
            // topology-repeat controls per direction surround the bounded
            // independent DL/UL searches.
            8 + crate::autotune::MAX_PROFILE_REVIEW_OPTIONS as u32
                + (crate::autotune::MAX_PROFILE_SEARCH_OBSERVATIONS * 2) as u32
                + full_autotune::MAX_RUNTIME_ROUTE_REARMS
        );
        assert!(raw_policy.allow_directional_bypass);
        assert_eq!(
            native_autotune_rate_bounds("download", 700_000, raw_policy.download_bounds)
                .unwrap()
                .maximum_kbps,
            900_000,
        );
        assert!(
            native_autotune_rate_bounds("download", 950_000, raw_policy.download_bounds)
                .unwrap_err()
                .contains("download CAKE rate exceeds the requested service hard cap")
        );
    }

    #[test]
    fn bootstrap_runtime_permit_uses_explicit_caps_only_as_search_authority() {
        let root = temp_path("bootstrap-runtime-permit");
        let daemon = CalibrationDaemon::bind_with_admission(&root, true).unwrap();
        let mut request = operation_request('e', 'f', "wan");
        request.target_state = OperationTargetState::AbsentBootstrap;
        request.capture_policy =
            Some(crate::operations::autotune_capture_policy::AutotuneCapturePolicyId::StandardV1);
        request.route.source_ip = Some("192.0.2.2".parse().unwrap());
        request.service_dl_cap_kbps = Some(900_000);
        request.service_ul_cap_kbps = Some(100_000);
        request.validate().unwrap();
        let permit_id = "9".repeat(32);
        let baseline = AbsentRuntimeBaseline {
            planned_sqm_section: "wan_sqm".to_string(),
            target_interface: "wan-device".to_string(),
            target_ifindex: 7,
            route_fingerprint: request.identity.route_fingerprint.clone(),
            config_fingerprint: request.identity.config_fingerprint.clone(),
            sqm_fingerprint: request.identity.sqm_fingerprint.clone(),
            kernel_topology_fingerprint: "a".repeat(64),
            kernel_namespace_seed: permit_id.clone(),
        };
        let worker = ProcessIdentity {
            pid: 42,
            process_group: 42,
            starttime_ticks: 1234,
        };
        let permit = daemon
            .assemble_bootstrap_runtime_override_permit(
                &request,
                &"8".repeat(32),
                &worker,
                permit_id,
                baseline.clone(),
            )
            .unwrap();
        assert_eq!(permit.baseline, RuntimeBaseline::Absent(baseline));
        assert_eq!(permit.initial_download_kbps, 900_000);
        assert_eq!(permit.initial_upload_kbps, 100_000);
        assert_eq!(permit.download_qdisc_kind, RuntimeQdiscKind::Cake);
        assert_eq!(permit.upload_qdisc_kind, RuntimeQdiscKind::Cake);
        assert_eq!(permit.download_bounds.maximum_kbps, 900_000);
        assert_eq!(permit.upload_bounds.maximum_kbps, 100_000);

        request.service_ul_cap_kbps = None;
        assert!(daemon
            .assemble_bootstrap_runtime_override_permit(
                &request,
                &"8".repeat(32),
                &worker,
                "9".repeat(32),
                AbsentRuntimeBaseline {
                    planned_sqm_section: "wan_sqm".to_string(),
                    target_interface: "wan-device".to_string(),
                    target_ifindex: 7,
                    route_fingerprint: request.identity.route_fingerprint.clone(),
                    config_fingerprint: request.identity.config_fingerprint.clone(),
                    sqm_fingerprint: request.identity.sqm_fingerprint.clone(),
                    kernel_topology_fingerprint: "a".repeat(64),
                    kernel_namespace_seed: "9".repeat(32),
                },
            )
            .is_err());
        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    fn request(command: ControlCommand) -> ControlMessage {
        ControlMessage {
            control: ControlRequest {
                request_id: "0123456789abcdef0123456789abcdef".to_string(),
                command,
                job_id: None,
                job_token: None,
            },
            operation: None,
        }
    }

    fn operation_request(job_id: char, token: char, instance: &str) -> OperationRequest {
        OperationRequest {
            identity: OperationIdentity {
                job_id: job_id.to_string().repeat(32),
                job_token: token.to_string().repeat(64),
                instance: instance.to_string(),
                operation: OperationKind::FullAutotune,
                target_interface: format!("{instance}-device"),
                route_fingerprint: "5".repeat(64),
                config_fingerprint: "6".repeat(64),
                sqm_fingerprint: format!("{}", if instance == "wan" { '7' } else { '8' })
                    .repeat(64),
            },
            created_unix_ms: 1,
            deadline_unix_ms: 4_102_444_800_000,
            origin: OperationOrigin::Luci,
            backend: "speedtest-go".to_string(),
            speedtest_direction: None,
            speedtest_server_id: None,
            speedtest_topology: None,
            route: OperationRouteIdentity {
                mode: OperationRouteMode::Main,
                mwan3_member: None,
                l3_device: format!("{instance}-device"),
                source_ip: None,
                fwmark: None,
                routing_table: None,
            },
            target_state: OperationTargetState::ExistingManaged,
            capture_policy: None,
            managed_sqm_section: Some(format!("{instance}_sqm")),
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

    fn bootstrap_operation_request(job_id: char, token: char, instance: &str) -> OperationRequest {
        let mut request = operation_request(job_id, token, instance);
        request.target_state = OperationTargetState::AbsentBootstrap;
        request.capture_policy =
            Some(crate::operations::autotune_capture_policy::AutotuneCapturePolicyId::StandardV1);
        request.route.source_ip = Some("192.0.2.2".parse().unwrap());
        request.service_dl_cap_kbps = Some(900_000);
        request.service_ul_cap_kbps = Some(100_000);
        request.validate_admission_policy().unwrap();
        request
    }

    fn bootstrap_runtime_baseline(
        request: &OperationRequest,
        namespace_seed: char,
    ) -> AbsentRuntimeBaseline {
        AbsentRuntimeBaseline {
            planned_sqm_section: request.managed_sqm_section.clone().unwrap(),
            target_interface: request.identity.target_interface.clone(),
            target_ifindex: 7,
            route_fingerprint: request.identity.route_fingerprint.clone(),
            config_fingerprint: request.identity.config_fingerprint.clone(),
            sqm_fingerprint: request.identity.sqm_fingerprint.clone(),
            kernel_topology_fingerprint: "a".repeat(64),
            kernel_namespace_seed: namespace_seed.to_string().repeat(32),
        }
    }

    fn allowing_bootstrap_runtime_attestor(
        _request: &OperationRequest,
        _baseline: &AbsentRuntimeBaseline,
    ) -> Result<(), String> {
        Ok(())
    }

    fn allowing_bootstrap_runtime_baseline_capturer(
        request: &OperationRequest,
    ) -> Result<AbsentRuntimeBaseline, String> {
        Ok(bootstrap_runtime_baseline(request, 'b'))
    }

    fn rejecting_bootstrap_runtime_attestor(
        _request: &OperationRequest,
        _baseline: &AbsentRuntimeBaseline,
    ) -> Result<(), String> {
        Err("injected bootstrap route and topology drift".to_string())
    }

    fn guided_rating_request(job_id: char, token: char, instance: &str) -> OperationRequest {
        let mut request = operation_request(job_id, token, instance);
        request.identity.operation = OperationKind::GuidedRating;
        request.managed_sqm_section = None;
        request.profile = None;
        request.strategy = None;
        request.access_medium = None;
        request.access_source = None;
        request.access_confidence_percent = 0;
        request.capacity_learning_policy = None;
        request.allow_sqm_disable = false;
        request.traffic_budget_bytes = 0;
        request
    }

    #[test]
    fn production_native_autotune_flag_is_explicit_and_not_a_lab_bypass() {
        let state_dir = temp_path("production-native-option");
        let options = parse_daemon_args(
            vec![
                "--state-dir".to_string(),
                state_dir.display().to_string(),
                "--native-autotune".to_string(),
            ]
            .into_iter(),
        )
        .unwrap();
        assert!(options.native_autotune);
        assert!(!options.native_scheduler);
        assert!(!options.lab_rust_rating);
        assert!(!options.lab_rust_speedtest);
        assert!(!options.lab_rust_autotune);
        assert!(!options.lab_rust_scheduler);
        assert!(options.scheduler_store_dir.is_none());
        let duplicate_error = match parse_daemon_args(
            vec![
                "--native-autotune".to_string(),
                "--native-autotune".to_string(),
            ]
            .into_iter(),
        ) {
            Err(error) => error,
            Ok(_) => panic!("duplicate production native Auto-Tune flag was accepted"),
        };
        assert!(duplicate_error.contains("unsupported calibrationd option"));
    }

    #[test]
    fn absent_bootstrap_admission_requires_the_exact_native_policy() {
        let state_dir = temp_path("absent-bootstrap-admission-exact");
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_autotune = true;
        let request = bootstrap_operation_request('a', 'b', "wan");

        assert!(daemon.supports_native_request(&request));
        let response = daemon.handle(&job_message(ControlCommand::Start, &request));
        assert!(response.contains("\"state\":\"queued\""));
        assert!(!daemon.jobs[0].journal.heavy_lease_acquired);

        let mut missing_cap = bootstrap_operation_request('c', 'd', "wanb");
        missing_cap.service_ul_cap_kbps = None;
        assert!(!daemon.supports_native_request(&missing_cap));
        let rejected = daemon.handle(&job_message(ControlCommand::Start, &missing_cap));
        assert!(rejected.contains("\"error_code\":\"invalid-request-policy\""));
        assert!(rejected.contains("explicit download and upload service caps"));

        drop(daemon);
        fs::remove_dir_all(&state_dir).unwrap();
    }

    #[test]
    fn retired_legacy_adapter_flag_is_rejected() {
        let retired_flag = ["--lab-legacy", "-adapter"].concat();
        let error = parse_daemon_args(
            vec![
                "--state-dir".to_string(),
                temp_path("retired-legacy-adapter").display().to_string(),
                retired_flag,
            ]
            .into_iter(),
        )
        .err()
        .expect("retired lab legacy adapter flag was accepted");
        assert!(error.contains("unsupported calibrationd option"));
    }

    #[test]
    fn production_native_rating_flag_is_explicit_and_not_a_lab_bypass() {
        let state_dir = temp_path("production-native-rating-option");
        let options = parse_daemon_args(
            vec![
                "--state-dir".to_string(),
                state_dir.display().to_string(),
                "--native-rating".to_string(),
                "--native-autotune".to_string(),
            ]
            .into_iter(),
        )
        .unwrap();
        assert!(options.native_rating);
        assert!(options.native_autotune);
        assert!(!options.lab_rust_rating);
        assert!(!options.lab_rust_speedtest);
        assert!(!options.lab_rust_autotune);

        let duplicate_error = parse_daemon_args(
            vec!["--native-rating".to_string(), "--native-rating".to_string()].into_iter(),
        )
        .err()
        .unwrap();
        assert!(duplicate_error.contains("unsupported calibrationd option"));
    }

    #[test]
    fn production_native_speedtest_flag_is_explicit_and_not_a_lab_bypass() {
        let state_dir = temp_path("production-native-speedtest-option");
        let options = parse_daemon_args(
            vec![
                "--state-dir".to_string(),
                state_dir.display().to_string(),
                "--native-speedtest".to_string(),
            ]
            .into_iter(),
        )
        .unwrap();
        assert!(options.native_speedtest);
        assert!(!options.lab_rust_speedtest);
        assert!(!options.native_rating);
        assert!(!options.native_autotune);

        let duplicate_error = parse_daemon_args(
            vec![
                "--native-speedtest".to_string(),
                "--native-speedtest".to_string(),
            ]
            .into_iter(),
        )
        .err()
        .unwrap();
        assert!(duplicate_error.contains("unsupported calibrationd option"));

        assert!(parse_daemon_args(
            vec![
                "--native-speedtest".to_string(),
                "--lab-rust-speedtest".to_string(),
            ]
            .into_iter(),
        )
        .is_err());
    }

    #[test]
    fn production_native_scheduler_requires_native_worker_and_exact_persistent_store() {
        let missing_worker = parse_daemon_args(
            vec![
                "--native-scheduler".to_string(),
                "--scheduler-store-dir".to_string(),
                PRODUCTION_SCHEDULER_STORE_DIR.to_string(),
            ]
            .into_iter(),
        )
        .err()
        .unwrap();
        assert!(missing_worker.contains("requires --native-autotune"));

        let wrong_store = parse_daemon_args(
            vec![
                "--native-autotune".to_string(),
                "--native-scheduler".to_string(),
                "--scheduler-store-dir".to_string(),
                "/tmp/not-persistent".to_string(),
            ]
            .into_iter(),
        )
        .err()
        .unwrap();
        assert!(wrong_store.contains(PRODUCTION_SCHEDULER_STORE_DIR));

        let options = parse_daemon_args(
            vec![
                "--native-autotune".to_string(),
                "--native-scheduler".to_string(),
                "--scheduler-store-dir".to_string(),
                PRODUCTION_SCHEDULER_STORE_DIR.to_string(),
            ]
            .into_iter(),
        )
        .unwrap();
        assert!(options.native_autotune);
        assert!(options.native_scheduler);
        assert_eq!(
            options.scheduler_store_dir.as_deref(),
            Some(Path::new(PRODUCTION_SCHEDULER_STORE_DIR))
        );
        assert!(!options.lab_rust_scheduler);
    }

    #[test]
    fn lab_scheduler_requires_isolated_state_store_and_native_worker() {
        let missing_worker = parse_daemon_args(
            vec![
                "--state-dir".to_string(),
                "/tmp/coordinator-lab".to_string(),
                "--scheduler-store-dir".to_string(),
                "/tmp/scheduler-lab".to_string(),
                "--lab-rust-scheduler".to_string(),
            ]
            .into_iter(),
        )
        .err()
        .unwrap();
        assert!(missing_worker.contains("requires --lab-rust-autotune"));

        let unowned_store = parse_daemon_args(
            vec![
                "--state-dir".to_string(),
                "/tmp/coordinator-lab".to_string(),
                "--scheduler-store-dir".to_string(),
                "/tmp/scheduler-lab".to_string(),
            ]
            .into_iter(),
        )
        .err()
        .unwrap();
        assert!(
            unowned_store.contains("valid only with --native-scheduler or --lab-rust-scheduler")
        );

        let same_store = parse_daemon_args(
            vec![
                "--state-dir".to_string(),
                "/tmp/same-lab".to_string(),
                "--scheduler-store-dir".to_string(),
                "/tmp/same-lab".to_string(),
                "--lab-rust-scheduler".to_string(),
            ]
            .into_iter(),
        )
        .err()
        .unwrap();
        assert!(same_store.contains("must be separate"));
    }

    #[test]
    fn production_native_autotune_rejects_unsupported_backend_before_admission() {
        let dir = temp_path("production-native-support");
        let mut daemon = CalibrationDaemon::bind_with_components(&dir, true, None).unwrap();
        daemon.native_autotune = true;

        let supported = operation_request('a', 'b', "wan");
        assert!(daemon.supports_native_request(&supported));

        let mut unsupported = operation_request('c', 'd', "wanb");
        unsupported.backend = "iperf3".to_string();
        assert!(!daemon.supports_native_request(&unsupported));
        let response = daemon.handle(&job_message(ControlCommand::Start, &unsupported));
        assert!(response.contains("\"error_code\":\"operation-unsupported\""));
        assert!(daemon.jobs.is_empty());
        assert_eq!(daemon.leases.job_count(), 0);

        let response = daemon.handle(&job_message(ControlCommand::Start, &supported));
        assert!(response.contains("\"state\":\"queued\""));
        assert_eq!(daemon.jobs.len(), 1);
        assert_eq!(daemon.leases.job_count(), 1);
        drop(daemon);
        fs::remove_dir_all(&dir).unwrap();
    }

    fn ready_runtime_snapshot(updated_unix_ms: u64) -> rating::RatingRuntimeSnapshot {
        rating::RatingRuntimeSnapshot {
            updated_unix_ms,
            capture_observed_unix_ms: updated_unix_ms,
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
            evidence_contract:
                rating::RatingEvidenceContract::WorstOfDirectionBoundIcmpAndTransport,
            dl_samples: 20,
            ul_samples: 20,
            dl_achieved_kbps: 80_000.0,
            ul_achieved_kbps: 9_000.0,
            cake_dl_kbps: 85_000.0,
            cake_ul_kbps: 10_000.0,
            download_qdisc_kind: Some(RuntimeQdiscKind::Cake),
            upload_qdisc_kind: Some(RuntimeQdiscKind::Cake),
            reference_dl_kbps: 90_000.0,
            reference_ul_kbps: 11_000.0,
            capture_active: false,
            capture_job_id: String::new(),
            capture_generation: 0,
            finalized_job_id: String::new(),
            finalized_generation: 0,
            finalized_outcome: String::new(),
            capture_phase: "idle".to_string(),
            capture_contaminated: false,
            current_capture_job_id: String::new(),
            current_capture_generation: 0,
            current: None,
        }
    }

    #[test]
    fn native_runtime_qdisc_authority_preserves_cake_mq_and_defaults_only_missing_directions() {
        let mut snapshot = ready_runtime_snapshot(1_000_000);
        snapshot.download_qdisc_kind = Some(RuntimeQdiscKind::CakeMq);
        snapshot.upload_qdisc_kind = Some(RuntimeQdiscKind::CakeMq);
        assert_eq!(
            native_runtime_qdisc_kinds(&snapshot, full_autotune::MeasurementTopology::ShapedBoth,)
                .unwrap(),
            (RuntimeQdiscKind::CakeMq, RuntimeQdiscKind::CakeMq)
        );

        snapshot.upload_qdisc_kind = None;
        assert_eq!(
            native_runtime_qdisc_kinds(
                &snapshot,
                full_autotune::MeasurementTopology::DownloadOnlyShaped,
            )
            .unwrap(),
            (RuntimeQdiscKind::CakeMq, RuntimeQdiscKind::Cake)
        );
        assert!(native_runtime_qdisc_kinds(
            &snapshot,
            full_autotune::MeasurementTopology::ShapedBoth,
        )
        .is_err());
    }

    #[test]
    fn scheduled_runtime_readiness_separates_freshness_from_substantive_state() {
        let now = 1_000_000;
        let mut snapshot = ready_runtime_snapshot(now - 1_000);
        assert!(scheduled_runtime_substantively_ready(&snapshot));
        assert!(scheduled_runtime_snapshot_fresh(&snapshot, now));
        assert_eq!(
            scheduled_minimum_traffic_budget_bytes(&snapshot),
            16_023_398
        );
        snapshot.updated_unix_ms = now + 1;
        assert!(scheduled_runtime_substantively_ready(&snapshot));
        assert!(!scheduled_runtime_snapshot_fresh(&snapshot, now));
        snapshot.updated_unix_ms = now;
        snapshot.uplink_state = "STANDBY".to_string();
        snapshot.route_active = false;
        assert!(scheduled_runtime_substantively_ready(&snapshot));
        snapshot.route_test_ready = false;
        assert!(!scheduled_runtime_substantively_ready(&snapshot));
        snapshot.route_test_ready = true;
        snapshot.runtime_generation = 0;
        assert!(!scheduled_runtime_substantively_ready(&snapshot));
        snapshot.runtime_generation = 17;
        snapshot.baseline_samples = snapshot.baseline_required_samples - 1;
        assert!(!scheduled_runtime_substantively_ready(&snapshot));
        snapshot.baseline_samples = snapshot.baseline_required_samples;
        snapshot.cake_dl_kbps = 0.0;
        assert!(!scheduled_runtime_substantively_ready(&snapshot));
    }

    #[test]
    fn scheduler_quiet_evidence_survives_snapshot_clock_races_and_stale_control_wakes() {
        let instance = "wan";
        let base = 1_000_000;
        let mut quiet = BTreeMap::new();
        for offset in [0, 4_000, 8_000, 12_000, 16_000, 20_000, 24_000, 28_000] {
            let snapshot = ready_runtime_snapshot(base + offset);
            assert_eq!(
                scheduled_runtime_and_quiet_ready(
                    &mut quiet,
                    instance,
                    &snapshot,
                    100_000,
                    30,
                    snapshot.updated_unix_ms,
                ),
                (true, false)
            );
        }

        let accumulated = quiet.clone();
        let future_publication = ready_runtime_snapshot(base + 32_000);
        assert_eq!(
            scheduled_runtime_and_quiet_ready(
                &mut quiet,
                instance,
                &future_publication,
                100_000,
                30,
                base + 31_950,
            ),
            (false, false)
        );
        assert_eq!(quiet, accumulated);
        assert_eq!(
            scheduled_runtime_and_quiet_ready(
                &mut quiet,
                instance,
                &future_publication,
                100_000,
                30,
                base + 32_000,
            ),
            (true, true)
        );

        let qualified = quiet.clone();
        assert_eq!(
            scheduled_runtime_and_quiet_ready(
                &mut quiet,
                instance,
                &future_publication,
                100_000,
                30,
                base + 38_000,
            ),
            (false, false)
        );
        assert_eq!(quiet, qualified);
    }

    #[test]
    fn scheduler_quiet_evidence_is_destroyed_by_substantive_runtime_loss() {
        let instance = "wan";
        let mut quiet = BTreeMap::new();
        let snapshot = ready_runtime_snapshot(1_000_000);
        assert_eq!(
            scheduled_runtime_and_quiet_ready(
                &mut quiet,
                instance,
                &snapshot,
                100_000,
                30,
                snapshot.updated_unix_ms,
            ),
            (true, false)
        );
        assert!(quiet.contains_key(instance));

        let mut selected_standby = snapshot.clone();
        selected_standby.uplink_state = "STANDBY".to_string();
        selected_standby.route_active = false;
        assert_eq!(
            scheduled_runtime_and_quiet_ready(
                &mut quiet,
                instance,
                &selected_standby,
                100_000,
                30,
                selected_standby.updated_unix_ms,
            ),
            (true, false)
        );
        assert!(quiet.contains_key(instance));

        let mut route_lost = selected_standby;
        route_lost.route_test_ready = false;
        assert_eq!(
            scheduled_runtime_and_quiet_ready(
                &mut quiet,
                instance,
                &route_lost,
                100_000,
                30,
                route_lost.updated_unix_ms,
            ),
            (false, false)
        );
        assert!(!quiet.contains_key(instance));
    }

    #[test]
    fn scheduled_terminal_traffic_is_replayed_before_settlement() {
        let root = temp_path("sched-term");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_autotune = true;
        let mut operation = operation_request('a', 'b', "wan");
        operation.origin = OperationOrigin::Scheduler;
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));
        let worker_run_id = "c".repeat(32);
        let mut armed = daemon.jobs[0].journal.clone();
        armed.arm_native_worker(worker_run_id.clone()).unwrap();
        daemon.journal_store.update(&armed).unwrap();
        daemon.jobs[0].journal = armed;
        let paths = daemon
            .journal_store
            .native_job_paths(&operation.identity.job_id, &worker_run_id)
            .unwrap();
        full_autotune::publish_terminal_file(
            &paths.terminal,
            &full_autotune::AutotuneTerminalRecord {
                job_id: operation.identity.job_id.clone(),
                worker_run_id: worker_run_id.clone(),
                consumed_traffic_bytes: 0,
                terminal: AutotuneTerminal::Cancelled,
            },
        )
        .unwrap();
        assert_eq!(
            daemon
                .exact_scheduled_terminal_traffic(0, &operation.identity.job_id)
                .unwrap(),
            (0, ScheduledSettlement::Failed)
        );

        full_autotune::publish_terminal_file(
            &paths.terminal,
            &full_autotune::AutotuneTerminalRecord {
                job_id: operation.identity.job_id.clone(),
                worker_run_id,
                consumed_traffic_bytes: 1,
                terminal: AutotuneTerminal::Cancelled,
            },
        )
        .unwrap();
        assert!(daemon
            .exact_scheduled_terminal_traffic(0, &operation.identity.job_id)
            .is_err());
        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    fn fail_scheduled_auto_apply(
        _state_dir: &Path,
        _job_id: &str,
    ) -> Result<NativeScheduledAutoApplyOutcome, String> {
        Err("injected Apply failure".to_string())
    }

    fn review_scheduled_auto_apply(
        _state_dir: &Path,
        _job_id: &str,
    ) -> Result<NativeScheduledAutoApplyOutcome, String> {
        Ok(NativeScheduledAutoApplyOutcome::ReviewRequired)
    }

    fn succeed_scheduled_auto_apply_before_settlement(
        state_dir: &Path,
        job_id: &str,
    ) -> Result<NativeScheduledAutoApplyOutcome, String> {
        let scheduler_dir = state_dir
            .parent()
            .ok_or_else(|| "test state directory has no parent".to_string())?
            .join("scheduler");
        let persisted = fs::read_to_string(scheduler_dir.join("instance-wan_sqm.state"))
            .map_err(|error| format!("test scheduler state disappeared before Apply: {error}"))?;
        if !persisted.contains(&format!("reservation_job_id\t{job_id}\n")) {
            return Err("test reservation settled or changed before Apply".to_string());
        }
        fs::write(state_dir.join("apply-before-settlement"), job_id.as_bytes())
            .map_err(|error| format!("unable to publish test Apply marker: {error}"))?;
        Ok(NativeScheduledAutoApplyOutcome::Applied(
            NativeApplyCommitDisposition::Applied,
        ))
    }

    #[test]
    fn scheduled_eligible_apply_runs_before_success_settlement() {
        let root = temp_path("sched-apply-success");
        let state_dir = root.join("state");
        let scheduler_dir = root.join("scheduler");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let store = SchedulerStore::open(&scheduler_dir).unwrap();
        store
            .persist_state(&reserved_scheduler_state("wan_sqm"))
            .unwrap();
        let mut scheduler = NativeSchedulerRuntime {
            store,
            quiet: BTreeMap::new(),
            errors: BTreeMap::new(),
            auto_apply_errors: BTreeMap::new(),
            auto_apply_warnings: BTreeMap::new(),
            waiting: BTreeMap::new(),
            accounting_blocks: BTreeMap::new(),
            status_cache: NativeSchedulerStatusCache::default(),
            lab_mode: false,
        };
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_autotune = true;
        let mut operation = operation_request('2', '3', "wan_sqm");
        operation.origin = OperationOrigin::Scheduler;
        operation.scheduled_auto_apply_requested = true;
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));

        let worker_run_id = "4".repeat(32);
        let mut terminal = daemon.jobs[0].journal.clone();
        terminal.arm_native_worker(worker_run_id.clone()).unwrap();
        terminal.state = super::super::protocol::OperationState::ReviewReady;
        daemon.journal_store.update(&terminal).unwrap();
        daemon.jobs[0].journal = terminal;
        daemon.jobs[0].disposition = JournalDisposition::Settled;
        let paths = daemon
            .journal_store
            .native_job_paths(&operation.identity.job_id, &worker_run_id)
            .unwrap();
        full_autotune::publish_terminal_file(
            &paths.terminal,
            &full_autotune::AutotuneTerminalRecord {
                job_id: operation.identity.job_id.clone(),
                worker_run_id,
                consumed_traffic_bytes: 0,
                terminal: AutotuneTerminal::Complete {
                    review_digest: "5".repeat(64),
                },
            },
        )
        .unwrap();

        daemon
            .reconcile_native_scheduler_reservation_with(
                &mut scheduler,
                "wan_sqm",
                1_100,
                "20260805",
                "202608",
                succeed_scheduled_auto_apply_before_settlement,
            )
            .unwrap();

        assert_eq!(
            fs::read_to_string(state_dir.join("apply-before-settlement")).unwrap(),
            operation.identity.job_id
        );
        let settled = scheduler.store.load_state("wan_sqm").unwrap().unwrap();
        assert!(settled.budget.reservation.is_none());
        assert_eq!(settled.budget.daily_charged_bytes, 0);
        assert_eq!(settled.cursor.last_success_unix_s, Some(1_100));
        assert!(settled.cursor.failed_attempt.is_none());
        assert!(scheduler.auto_apply_errors.is_empty());
        assert!(scheduler.auto_apply_warnings.is_empty());
        assert!(scheduler.errors.is_empty());

        drop(daemon);
        drop(scheduler);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scheduled_apply_failure_preserves_successful_measurement_cursor() {
        let root = temp_path("sched-apply-failure");
        let state_dir = root.join("state");
        let scheduler_dir = root.join("scheduler");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let store = SchedulerStore::open(&scheduler_dir).unwrap();
        store
            .persist_state(&reserved_scheduler_state("wan_sqm"))
            .unwrap();
        let mut scheduler = NativeSchedulerRuntime {
            store,
            quiet: BTreeMap::new(),
            errors: BTreeMap::new(),
            auto_apply_errors: BTreeMap::new(),
            auto_apply_warnings: BTreeMap::new(),
            waiting: BTreeMap::new(),
            accounting_blocks: BTreeMap::new(),
            status_cache: NativeSchedulerStatusCache::default(),
            lab_mode: false,
        };
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_autotune = true;
        let mut operation = operation_request('2', '3', "wan_sqm");
        operation.origin = OperationOrigin::Scheduler;
        operation.scheduled_auto_apply_requested = true;
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));

        let worker_run_id = "4".repeat(32);
        let mut terminal = daemon.jobs[0].journal.clone();
        terminal.arm_native_worker(worker_run_id.clone()).unwrap();
        terminal.state = super::super::protocol::OperationState::ReviewReady;
        daemon.journal_store.update(&terminal).unwrap();
        daemon.jobs[0].journal = terminal;
        daemon.jobs[0].disposition = JournalDisposition::Settled;
        let paths = daemon
            .journal_store
            .native_job_paths(&operation.identity.job_id, &worker_run_id)
            .unwrap();
        full_autotune::publish_terminal_file(
            &paths.terminal,
            &full_autotune::AutotuneTerminalRecord {
                job_id: operation.identity.job_id,
                worker_run_id,
                consumed_traffic_bytes: 0,
                terminal: AutotuneTerminal::Complete {
                    review_digest: "5".repeat(64),
                },
            },
        )
        .unwrap();

        daemon
            .reconcile_native_scheduler_reservation_with(
                &mut scheduler,
                "wan_sqm",
                1_100,
                "20260805",
                "202608",
                fail_scheduled_auto_apply,
            )
            .unwrap();

        let settled = scheduler.store.load_state("wan_sqm").unwrap().unwrap();
        assert!(settled.budget.reservation.is_none());
        assert_eq!(settled.budget.daily_charged_bytes, 0);
        assert_eq!(settled.cursor.last_success_unix_s, Some(1_100));
        assert!(settled.cursor.failed_attempt.is_none());
        assert!(scheduler.auto_apply_errors["wan_sqm"].contains("injected Apply failure"));
        assert!(scheduler.auto_apply_warnings.is_empty());
        assert!(scheduler.errors["wan_sqm"].contains("injected Apply failure"));
        let status = native_scheduler_status_response(
            &scheduled_config("wan_sqm"),
            Some(&settled),
            scheduler.errors.get("wan_sqm").map(String::as_str),
            scheduler
                .auto_apply_warnings
                .get("wan_sqm")
                .map(String::as_str),
            None,
            1_100,
            "20260805",
            "202608",
        )
        .unwrap();
        assert!(status.contains("\"state\":\"error\""));
        assert!(status.contains("\"warning\":null"));

        drop(daemon);
        drop(scheduler);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scheduled_review_required_is_a_non_error_warning() {
        let root = temp_path("sched-review-warning");
        let state_dir = root.join("state");
        let scheduler_dir = root.join("scheduler");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let store = SchedulerStore::open(&scheduler_dir).unwrap();
        store
            .persist_state(&reserved_scheduler_state("wan_sqm"))
            .unwrap();
        let mut scheduler = NativeSchedulerRuntime {
            store,
            quiet: BTreeMap::new(),
            errors: BTreeMap::new(),
            auto_apply_errors: BTreeMap::new(),
            auto_apply_warnings: BTreeMap::new(),
            waiting: BTreeMap::new(),
            accounting_blocks: BTreeMap::new(),
            status_cache: NativeSchedulerStatusCache::default(),
            lab_mode: false,
        };
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_autotune = true;
        let mut operation = operation_request('2', '3', "wan_sqm");
        operation.origin = OperationOrigin::Scheduler;
        operation.scheduled_auto_apply_requested = true;
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));

        let worker_run_id = "4".repeat(32);
        let mut terminal = daemon.jobs[0].journal.clone();
        terminal.arm_native_worker(worker_run_id.clone()).unwrap();
        terminal.state = super::super::protocol::OperationState::ReviewReady;
        daemon.journal_store.update(&terminal).unwrap();
        daemon.jobs[0].journal = terminal;
        daemon.jobs[0].disposition = JournalDisposition::Settled;
        let paths = daemon
            .journal_store
            .native_job_paths(&operation.identity.job_id, &worker_run_id)
            .unwrap();
        full_autotune::publish_terminal_file(
            &paths.terminal,
            &full_autotune::AutotuneTerminalRecord {
                job_id: operation.identity.job_id,
                worker_run_id,
                consumed_traffic_bytes: 0,
                terminal: AutotuneTerminal::Complete {
                    review_digest: "5".repeat(64),
                },
            },
        )
        .unwrap();

        daemon
            .reconcile_native_scheduler_reservation_with(
                &mut scheduler,
                "wan_sqm",
                1_100,
                "20260805",
                "202608",
                review_scheduled_auto_apply,
            )
            .unwrap();

        let settled = scheduler.store.load_state("wan_sqm").unwrap().unwrap();
        assert!(settled.budget.reservation.is_none());
        assert_eq!(settled.cursor.last_success_unix_s, Some(1_100));
        assert!(scheduler.auto_apply_errors.is_empty());
        assert!(scheduler.errors.is_empty());
        let warning = scheduler.auto_apply_warnings["wan_sqm"].as_str();
        assert!(warning.contains("requires explicit Review"));
        assert_eq!(settled.operator_warning.as_deref(), Some(warning));
        let status = native_scheduler_status_response(
            &scheduled_config("wan_sqm"),
            Some(&settled),
            None,
            Some(warning),
            None,
            1_100,
            "20260805",
            "202608",
        )
        .unwrap();
        assert!(status.contains("\"state\":\"idle\""));
        assert!(status.contains("\"accounting_error\":false"));
        assert!(status.contains("\"warning\":\"scheduled Auto-Apply skipped:"));

        drop(daemon);
        drop(scheduler);
        fs::remove_dir_all(root).unwrap();
    }

    fn reserved_scheduler_state(instance: &str) -> SchedulerInstanceState {
        let generations = SchedulerGenerations {
            config_fingerprint: "a".repeat(64),
            route_fingerprint: "b".repeat(64),
            runtime_sequence: 7,
            coordinator_generation: "c".repeat(32),
        };
        let authority = FailedAttemptFence {
            due_unix_s: 1_000,
            interval_s: 3_600,
            generations,
            explicit_retry_sequence: 0,
        };
        let cursor = ScheduleCursor::new(instance.to_string(), 1_000).unwrap();
        let mut budget = BudgetLedger::new(
            instance.to_string(),
            "20260805".to_string(),
            "202608".to_string(),
            10_000,
            50_000,
        )
        .unwrap();
        budget
            .reserve(
                "1".repeat(32),
                "2".repeat(32),
                "20260805",
                "202608",
                authority,
                4_000,
            )
            .unwrap();
        SchedulerInstanceState::new(cursor, budget).unwrap()
    }

    fn scheduled_config(instance: &str) -> ScheduledInstanceConfig {
        ScheduledInstanceConfig {
            instance: instance.to_string(),
            instance_enabled: true,
            scheduled_enabled: true,
            interval_s: 3_600,
            idle_window_s: 60,
            window_start_hour: 0,
            window_end_hour: 0,
            daily_limit_bytes: 10_000,
            monthly_limit_bytes: 50_000,
            auto_apply: false,
            active_threshold_kbps: 2_000,
            expected_target_interface: format!("{instance}-device"),
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

    #[test]
    fn scheduler_status_request_is_batch_only_and_preserves_the_ack_protocol() {
        let request = SchedulerStatusRequest {
            request_id: "0123456789abcdef0123456789abcdef".to_string(),
        };
        let encoded = request.encode().unwrap();
        assert_eq!(
            encoded,
            concat!(
                "cake-autorate-scheduler\t1\tstatus\n",
                "request_id=0123456789abcdef0123456789abcdef\n\n"
            )
        );
        assert_eq!(SchedulerStatusRequest::decode(&encoded).unwrap(), request);
        assert!(
            SchedulerStatusRequest::decode(&format!("{}instance=wan\n\n", encoded.trim_end()))
                .is_err()
        );

        let acknowledgement = SchedulerAccountingAcknowledgement {
            request_id: "fedcba9876543210fedcba9876543210".to_string(),
            instance: "wan".to_string(),
        };
        assert_eq!(
            acknowledgement.encode().unwrap(),
            concat!(
                "cake-autorate-scheduler\t1\tacknowledge-accounting\n",
                "request_id=fedcba9876543210fedcba9876543210\n",
                "instance=wan\n\n"
            )
        );
    }

    #[test]
    fn scheduler_status_cli_rejects_per_instance_and_extra_arguments() {
        let error =
            run_calibrationctl(vec!["scheduler-status".to_string(), "wan".to_string()].into_iter())
                .unwrap_err();
        assert_eq!(error, "calibrationctl received unexpected arguments");
    }

    #[test]
    fn scheduler_status_missing_state_is_explicit_and_conservative() {
        let config = scheduled_config("wan");
        let response = native_scheduler_status_response(
            &config, None, None, None, None, 2_000, "20260809", "202608",
        )
        .unwrap();
        assert!(response.contains("\"state\":\"initializing\""));
        assert!(response.contains("\"enabled\":true"));
        assert!(response.contains("\"initialized\":false"));
        assert!(response.contains("\"budget_authoritative\":false"));
        assert!(response.contains("\"observed_at\":2000,\"updated_at\":0"));
        assert!(response.contains("\"remaining_bytes\":0"));
        assert!(response.contains("\"accounting_error\":false"));
        assert!(response.contains("\"warning\":null"));

        let mut disabled = config.clone();
        disabled.scheduled_enabled = false;
        let response = native_scheduler_status_response(
            &disabled, None, None, None, None, 2_000, "20260809", "202608",
        )
        .unwrap();
        assert!(response.contains("\"state\":\"disabled\""));
        assert!(response.contains("\"enabled\":false"));
        assert!(response.contains("\"initialized\":false"));
        assert!(response.contains("\"budget_authoritative\":true"));
        assert!(response.contains("\"remaining_bytes\":10000"));
        assert!(response.contains("\"remaining_bytes\":50000"));
        assert!(response.contains("\"accounting_error\":false"));
    }

    #[test]
    fn scheduler_status_separates_used_reserved_and_cursor_times_without_mutation() {
        let config = scheduled_config("wan");
        let generations = SchedulerGenerations {
            config_fingerprint: "a".repeat(64),
            route_fingerprint: "b".repeat(64),
            runtime_sequence: 7,
            coordinator_generation: "c".repeat(32),
        };
        let mut cursor = ScheduleCursor::new("wan".to_string(), 1_000).unwrap();
        cursor.record_success(3_600, 1_000, 1_100).unwrap();
        cursor
            .record_failure(FailedAttemptFence {
                due_unix_s: 4_700,
                interval_s: 3_600,
                generations: generations.clone(),
                explicit_retry_sequence: 0,
            })
            .unwrap();
        let mut budget = BudgetLedger::new(
            "wan".to_string(),
            "20260809".to_string(),
            "202608".to_string(),
            10_000,
            50_000,
        )
        .unwrap();
        budget.daily_charged_bytes = 2_000;
        budget.monthly_charged_bytes = 3_000;
        budget
            .reserve(
                "1".repeat(32),
                "2".repeat(32),
                "20260809",
                "202608",
                FailedAttemptFence {
                    due_unix_s: 4_700,
                    interval_s: 3_600,
                    generations,
                    explicit_retry_sequence: 0,
                },
                4_000,
            )
            .unwrap();
        let state = SchedulerInstanceState::new(cursor, budget).unwrap();
        let before = state.clone();
        let response = native_scheduler_status_response(
            &config,
            Some(&state),
            None,
            None,
            None,
            5_000,
            "20260809",
            "202608",
        )
        .unwrap();
        assert_eq!(state, before);
        assert!(response.contains("\"state\":\"running\""));
        assert!(response.contains("\"initialized\":true"));
        assert!(response.contains("\"budget_authoritative\":true"));
        assert!(response.contains(
            "\"daily\":{\"limit_bytes\":10000,\"used_bytes\":2000,\"reserved_bytes\":4000,\"remaining_bytes\":4000}"
        ));
        assert!(response.contains(
            "\"monthly\":{\"limit_bytes\":50000,\"used_bytes\":3000,\"reserved_bytes\":4000,\"remaining_bytes\":43000}"
        ));
        assert!(response.contains("\"updated_at\":1100,\"next_due_at\":4700"));
        assert!(response.contains("\"last_success_at\":1100"));
        assert!(response.contains("\"last_failure_due_at\":4700"));

        let mut changed = config.clone();
        changed.daily_limit_bytes = 11_000;
        assert!(native_scheduler_status_response(
            &changed,
            Some(&state),
            None,
            None,
            None,
            5_000,
            "20260809",
            "202608",
        )
        .unwrap_err()
        .contains("changed during an active reservation"));

        let mut blocked = state.clone();
        blocked
            .budget
            .mark_accounting_unknown(&"1".repeat(32), &"2".repeat(32))
            .unwrap();
        let response = native_scheduler_status_response(
            &config,
            Some(&blocked),
            Some("private-route-and-job-diagnostic"),
            None,
            None,
            5_000,
            "20260809",
            "202608",
        )
        .unwrap();
        assert!(response.contains("\"state\":\"blocked\""));
        assert!(response.contains("\"initialized\":true"));
        assert!(response.contains("\"budget_authoritative\":false"));
        assert!(response.contains("\"accounting_error\":true"));
        assert!(!response.contains("private-route-and-job-diagnostic"));
    }

    #[test]
    fn scheduler_status_batch_fault_isolates_safe_error_entries() {
        let healthy = native_scheduler_status_response(
            &scheduled_config("wan"),
            None,
            None,
            None,
            None,
            7_000,
            "20260809",
            "202608",
        )
        .unwrap();
        let message = sanitize_scheduler_public_message("invalid scheduler option\nvalue");
        let issue = format_native_scheduler_issue("wanb", &message, 7_000);
        let rows = NativeSchedulerStatusRows {
            observed_at: 7_000,
            observed_monotonic: Instant::now(),
            next_scheduler_wake_at: None,
            instances: vec![healthy],
            issues: vec![issue],
        };
        let batch = format_native_scheduler_batch(&rows, false, None).unwrap();
        assert!(batch.starts_with("{\"schema_version\":1,\"owner\":\"native\",\"available\":true"));
        assert!(batch.contains("\"stale\":false,\"global_error\":null"));
        assert!(batch.contains("\"instances\":[{\"instance\":\"wan\""));
        assert!(batch.contains("\"issues\":[{\"instance\":\"wanb\""));
        assert!(batch.contains("invalid scheduler option value"));
        assert!(batch.contains(
            "\"enabled\":false,\"initialized\":false,\"budget_authoritative\":false,\"state\":\"error\""
        ));
        assert!(batch.contains("\"accounting_error\":false"));
        assert_eq!(batch.matches("\"warning\":null").count(), 2);
    }

    #[test]
    fn scheduler_status_cache_is_read_only_byte_stable_and_atomically_replaced() {
        let root = temp_path("scheduler-status-cache");
        let state_dir = root.join("state");
        let scheduler_dir = root.join("scheduler");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let store = SchedulerStore::open(&scheduler_dir).unwrap();
        let mut daemon = CalibrationDaemon::bind(&state_dir).unwrap();
        daemon.native_scheduler = Some(NativeSchedulerRuntime {
            store,
            quiet: BTreeMap::new(),
            errors: BTreeMap::new(),
            auto_apply_errors: BTreeMap::new(),
            auto_apply_warnings: BTreeMap::new(),
            waiting: BTreeMap::new(),
            accounting_blocks: BTreeMap::new(),
            status_cache: NativeSchedulerStatusCache::default(),
            lab_mode: true,
        });
        let status_request = SchedulerStatusRequest {
            request_id: "0123456789abcdef0123456789abcdef".to_string(),
        };
        assert!(daemon
            .handle_scheduler_status(&status_request)
            .contains("\"error_code\":\"scheduler-status-unavailable\""));

        let first_observed_monotonic = Instant::now().checked_sub(Duration::from_secs(4)).unwrap();
        let first_rows = NativeSchedulerStatusRows {
            observed_at: 7_000,
            observed_monotonic: first_observed_monotonic,
            next_scheduler_wake_at: Some(7_010),
            instances: vec![native_scheduler_status_response(
                &scheduled_config("wan"),
                None,
                None,
                None,
                None,
                7_000,
                "20260809",
                "202608",
            )
            .unwrap()],
            issues: Vec::new(),
        };
        daemon
            .native_scheduler
            .as_mut()
            .unwrap()
            .status_cache
            .publish(first_rows)
            .unwrap();

        let cached_before_reads = daemon
            .native_scheduler
            .as_ref()
            .unwrap()
            .status_cache
            .response()
            .unwrap()
            .to_string();
        for read_only in [
            CoordinatorControlRequest::Operation(request(ControlCommand::Summary)),
            CoordinatorControlRequest::SchedulerStatus(status_request.clone()),
            CoordinatorControlRequest::Operation(request(ControlCommand::Summary)),
            CoordinatorControlRequest::SchedulerStatus(status_request.clone()),
        ] {
            let (_, effect) = daemon.dispatch_control_request(read_only);
            assert_eq!(effect, ControlEffect::ReadOnly);
            assert!(!coordinator_event_requires_tick(
                effect, false, false, false, false,
            ));
        }
        assert_eq!(
            daemon
                .native_scheduler
                .as_ref()
                .unwrap()
                .status_cache
                .response(),
            Some(cached_before_reads.as_str())
        );

        let (_, invalid_effect) = daemon.dispatch_control_request(
            CoordinatorControlRequest::Operation(request(ControlCommand::Start)),
        );
        assert_eq!(invalid_effect, ControlEffect::ReadOnly);
        assert!(!coordinator_event_requires_tick(
            invalid_effect,
            false,
            false,
            false,
            false,
        ));

        let mut blocked = reserved_scheduler_state("wan");
        blocked
            .budget
            .mark_accounting_unknown(&"1".repeat(32), &"2".repeat(32))
            .unwrap();
        daemon
            .native_scheduler
            .as_ref()
            .unwrap()
            .store
            .persist_state(&blocked)
            .unwrap();

        // A foreign entry makes a fresh SchedulerStore scan fail. Cached
        // reads must neither inspect the store nor invoke the UCI loader.
        fs::write(scheduler_dir.join("foreign-entry"), b"do not read\n").unwrap();
        let first_timeout = daemon.next_event_timeout().unwrap().unwrap();
        let repeated_timeout = daemon.next_event_timeout().unwrap().unwrap();
        assert!(first_timeout <= Duration::from_secs(6));
        assert!(repeated_timeout <= first_timeout);
        let first = daemon.handle_scheduler_status(&status_request);
        let repeated = daemon.handle_scheduler_status(&status_request);
        assert_eq!(first, repeated);
        assert!(first.contains("\"observed_at\":7000"));

        let acknowledgement = SchedulerAccountingAcknowledgement {
            request_id: "fedcba9876543210fedcba9876543210".to_string(),
            instance: "wan".to_string(),
        };
        let mut acknowledgement_stream =
            UnixStream::connect(state_dir.join(CONTROL_SOCKET_NAME)).unwrap();
        acknowledgement_stream
            .write_all(acknowledgement.encode().unwrap().as_bytes())
            .unwrap();
        acknowledgement_stream
            .shutdown(std::net::Shutdown::Write)
            .unwrap();
        let mut status_stream = UnixStream::connect(state_dir.join(CONTROL_SOCKET_NAME)).unwrap();
        status_stream
            .write_all(status_request.encode().unwrap().as_bytes())
            .unwrap();
        status_stream.shutdown(std::net::Shutdown::Write).unwrap();
        status_stream
            .set_read_timeout(Some(Duration::from_millis(25)))
            .unwrap();

        let control_effect = daemon.drain_control_requests().unwrap();
        assert_eq!(control_effect, ControlEffect::StateChanged);
        assert!(coordinator_event_requires_tick(
            control_effect,
            false,
            false,
            false,
            false,
        ));
        let mut acknowledgement_response = String::new();
        acknowledgement_stream
            .read_to_string(&mut acknowledgement_response)
            .unwrap();
        assert!(acknowledgement_response.contains("\"state\":\"acknowledged\""));
        let mut probe = [0_u8; 1];
        let pending = status_stream.read(&mut probe).unwrap_err();
        assert!(matches!(
            pending.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ));

        daemon.tick();
        assert!(daemon.next_event_timeout().unwrap().is_some());
        let read_effect = daemon.drain_control_requests().unwrap();
        assert_eq!(read_effect, ControlEffect::ReadOnly);
        let mut stale_after_mutation = String::new();
        status_stream
            .read_to_string(&mut stale_after_mutation)
            .unwrap();
        assert!(stale_after_mutation.contains("\"stale\":true"));
        assert_ne!(first, stale_after_mutation);

        let second_rows = NativeSchedulerStatusRows {
            observed_at: 8_000,
            observed_monotonic: Instant::now(),
            next_scheduler_wake_at: None,
            instances: vec![native_scheduler_status_response(
                &scheduled_config("wanb"),
                None,
                None,
                None,
                None,
                8_000,
                "20260809",
                "202608",
            )
            .unwrap()],
            issues: Vec::new(),
        };
        daemon
            .native_scheduler
            .as_mut()
            .unwrap()
            .status_cache
            .publish(second_rows)
            .unwrap();
        assert!(daemon.next_event_timeout().unwrap().is_none());
        let replaced = daemon.handle_scheduler_status(&status_request);
        assert_ne!(first, replaced);
        assert!(replaced.contains("\"observed_at\":8000"));
        assert!(replaced.contains("\"instance\":\"wanb\""));
        assert!(!replaced.contains("\"instance\":\"wan\""));

        daemon
            .native_scheduler
            .as_mut()
            .unwrap()
            .status_cache
            .mark_global_failure();
        let stale = daemon.handle_scheduler_status(&status_request);
        assert!(stale.contains("\"stale\":true"));
        assert!(stale.contains(SCHEDULER_STATUS_GLOBAL_ERROR));
        assert!(stale.contains("\"instance\":\"wanb\""));
        assert_eq!(stale, daemon.handle_scheduler_status(&status_request));

        drop(daemon);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scheduler_status_global_failure_drops_an_expired_wakeup_without_dropping_rows() {
        let mut cache = NativeSchedulerStatusCache::default();
        cache
            .publish(NativeSchedulerStatusRows {
                observed_at: 7_000,
                observed_monotonic: Instant::now().checked_sub(Duration::from_secs(5)).unwrap(),
                next_scheduler_wake_at: Some(7_001),
                instances: Vec::new(),
                issues: Vec::new(),
            })
            .unwrap();
        assert_eq!(
            cache.next_scheduler_timeout(Instant::now()),
            Some(Duration::ZERO)
        );

        cache.mark_global_failure();

        assert_eq!(cache.next_scheduler_timeout(Instant::now()), None);
        let response = cache.response().unwrap();
        assert!(response.contains("\"stale\":true"));
        assert!(response.contains("\"instances\":[]"));
    }

    #[test]
    fn control_effect_matrix_distinguishes_reads_changes_idempotence_and_rejection() {
        let root = temp_path("control-effect-matrix");
        let mut daemon = CalibrationDaemon::bind_with_admission(&root, true).unwrap();
        daemon.native_autotune = true;

        for command in [ControlCommand::Ping, ControlCommand::Summary] {
            let (_, effect) = daemon
                .dispatch_control_request(CoordinatorControlRequest::Operation(request(command)));
            assert_eq!(effect, ControlEffect::ReadOnly);
        }
        let (_, scheduler_status_effect) = daemon.dispatch_control_request(
            CoordinatorControlRequest::SchedulerStatus(SchedulerStatusRequest {
                request_id: "1".repeat(32),
            }),
        );
        assert_eq!(scheduler_status_effect, ControlEffect::ReadOnly);

        let operation = operation_request('a', 'b', "wan");
        let (_, accepted_start) = daemon.dispatch_control_request(
            CoordinatorControlRequest::Operation(job_message(ControlCommand::Start, &operation)),
        );
        assert_eq!(accepted_start, ControlEffect::StateChanged);
        let (_, idempotent_start) = daemon.dispatch_control_request(
            CoordinatorControlRequest::Operation(job_message(ControlCommand::Start, &operation)),
        );
        assert_eq!(idempotent_start, ControlEffect::ReadOnly);
        let mut conflicting = operation.clone();
        conflicting.traffic_budget_bytes += 1;
        let (conflict_response, rejected_start) = daemon.dispatch_control_request(
            CoordinatorControlRequest::Operation(job_message(ControlCommand::Start, &conflicting)),
        );
        assert!(conflict_response.contains("\"error_code\":\"job-id-conflict\""));
        assert_eq!(rejected_start, ControlEffect::ReadOnly);

        for command in [ControlCommand::Status, ControlCommand::Result] {
            let (_, effect) = daemon.dispatch_control_request(
                CoordinatorControlRequest::Operation(job_message(command, &operation)),
            );
            assert_eq!(effect, ControlEffect::ReadOnly);
        }

        let (_, accepted_cancel) = daemon.dispatch_control_request(
            CoordinatorControlRequest::Operation(job_message(ControlCommand::Cancel, &operation)),
        );
        assert_eq!(accepted_cancel, ControlEffect::StateChanged);
        let (_, terminal_cancel) = daemon.dispatch_control_request(
            CoordinatorControlRequest::Operation(job_message(ControlCommand::Cancel, &operation)),
        );
        assert_eq!(terminal_cancel, ControlEffect::ReadOnly);

        let second = operation_request('c', 'd', "wanb");
        let (_, second_start) = daemon.dispatch_control_request(
            CoordinatorControlRequest::Operation(job_message(ControlCommand::Start, &second)),
        );
        assert_eq!(second_start, ControlEffect::StateChanged);
        let second_index = daemon
            .jobs
            .iter()
            .position(|job| job.journal.job_id == second.identity.job_id)
            .unwrap();
        daemon.jobs[second_index].journal.state =
            super::super::protocol::OperationState::Cancelling;
        let (_, repeated_cancelling) = daemon.dispatch_control_request(
            CoordinatorControlRequest::Operation(job_message(ControlCommand::Cancel, &second)),
        );
        assert_eq!(repeated_cancelling, ControlEffect::ReadOnly);

        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn accounting_ack_effect_and_event_tick_gate_are_exact() {
        let root = temp_path("accounting-effect-matrix");
        let state_dir = root.join("state");
        let scheduler_dir = root.join("scheduler");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let store = SchedulerStore::open(&scheduler_dir).unwrap();
        let mut blocked = reserved_scheduler_state("wan");
        blocked
            .budget
            .mark_accounting_unknown(&"1".repeat(32), &"2".repeat(32))
            .unwrap();
        store.persist_state(&blocked).unwrap();
        let mut daemon = CalibrationDaemon::bind(&state_dir).unwrap();
        let acknowledgement = SchedulerAccountingAcknowledgement {
            request_id: "3".repeat(32),
            instance: "wan".to_string(),
        };
        let (_, disabled_ack) = daemon.dispatch_control_request(
            CoordinatorControlRequest::SchedulerAccountingAcknowledgement(acknowledgement.clone()),
        );
        assert_eq!(disabled_ack, ControlEffect::ReadOnly);
        daemon.native_scheduler = Some(NativeSchedulerRuntime {
            store,
            quiet: BTreeMap::new(),
            errors: BTreeMap::new(),
            auto_apply_errors: BTreeMap::new(),
            auto_apply_warnings: BTreeMap::new(),
            waiting: BTreeMap::new(),
            accounting_blocks: BTreeMap::new(),
            status_cache: NativeSchedulerStatusCache::default(),
            lab_mode: true,
        });
        let (_, missing_ack) = daemon.dispatch_control_request(
            CoordinatorControlRequest::SchedulerAccountingAcknowledgement(
                SchedulerAccountingAcknowledgement {
                    request_id: "4".repeat(32),
                    instance: "wanb".to_string(),
                },
            ),
        );
        assert_eq!(missing_ack, ControlEffect::ReadOnly);
        let (_, accepted_ack) = daemon.dispatch_control_request(
            CoordinatorControlRequest::SchedulerAccountingAcknowledgement(acknowledgement.clone()),
        );
        assert_eq!(accepted_ack, ControlEffect::StateChanged);
        let (_, rejected_ack) = daemon.dispatch_control_request(
            CoordinatorControlRequest::SchedulerAccountingAcknowledgement(acknowledgement),
        );
        assert_eq!(rejected_ack, ControlEffect::ReadOnly);

        assert!(!coordinator_event_requires_tick(
            ControlEffect::ReadOnly,
            false,
            false,
            false,
            false,
        ));
        assert!(coordinator_event_requires_tick(
            ControlEffect::StateChanged,
            false,
            false,
            false,
            false,
        ));
        for external in 0..4 {
            let mut readiness = [false; 4];
            readiness[external] = true;
            assert!(coordinator_event_requires_tick(
                ControlEffect::ReadOnly,
                readiness[0],
                readiness[1],
                readiness[2],
                readiness[3],
            ));
        }

        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scheduler_status_socket_fails_closed_when_native_owner_is_disabled() {
        let dir = temp_path("scheduler-status-disabled");
        let thread = serve_one(CalibrationDaemon::bind(&dir).unwrap());
        let response = send_scheduler_status(&dir).unwrap();
        assert!(response.contains("\"error_code\":\"native-scheduler-disabled\""));
        thread.join().unwrap();
        cleanup_state_dir(&dir);
    }

    fn blocked_scheduler_with_terminal_job(
        root: &Path,
        publish_exact_terminal: bool,
    ) -> CalibrationDaemon {
        let state_dir = root.join("state");
        let scheduler_dir = root.join("scheduler");
        fs::create_dir_all(root).unwrap();
        fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
        let store = SchedulerStore::open(&scheduler_dir).unwrap();
        let mut persisted = reserved_scheduler_state("wan_sqm");
        persisted
            .budget
            .mark_accounting_unknown(&"1".repeat(32), &"2".repeat(32))
            .unwrap();
        store.persist_state(&persisted).unwrap();

        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_autotune = true;
        let mut operation = operation_request('2', '3', "wan_sqm");
        operation.origin = OperationOrigin::Scheduler;
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));
        let worker_run_id = "4".repeat(32);
        let mut terminal = daemon.jobs[0].journal.clone();
        terminal.arm_native_worker(worker_run_id.clone()).unwrap();
        terminal
            .settle_native_terminal("cancelled", Some("test-terminal"))
            .unwrap();
        daemon.journal_store.update(&terminal).unwrap();
        daemon.jobs[0].journal = terminal;
        daemon.jobs[0].disposition = JournalDisposition::Settled;
        if publish_exact_terminal {
            let paths = daemon
                .journal_store
                .native_job_paths(&operation.identity.job_id, &worker_run_id)
                .unwrap();
            full_autotune::publish_terminal_file(
                &paths.terminal,
                &full_autotune::AutotuneTerminalRecord {
                    job_id: operation.identity.job_id,
                    worker_run_id,
                    consumed_traffic_bytes: 0,
                    terminal: AutotuneTerminal::Cancelled,
                },
            )
            .unwrap();
        }
        daemon.native_scheduler = Some(NativeSchedulerRuntime {
            store,
            quiet: BTreeMap::new(),
            errors: BTreeMap::new(),
            auto_apply_errors: BTreeMap::new(),
            auto_apply_warnings: BTreeMap::new(),
            waiting: BTreeMap::new(),
            accounting_blocks: BTreeMap::from([("wan_sqm".to_string(), 4_000)]),
            status_cache: NativeSchedulerStatusCache::default(),
            lab_mode: false,
        });
        daemon
    }

    #[test]
    fn reboot_missing_tmpfs_job_blocks_accounting_and_operator_ack_keeps_full_charge() {
        let root = temp_path("sched-reboot");
        let state_dir = root.join("state");
        let scheduler_dir = root.join("scheduler");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let store = SchedulerStore::open(&scheduler_dir).unwrap();
        store
            .persist_state(&reserved_scheduler_state("wan_sqm"))
            .unwrap();
        let mut scheduler = NativeSchedulerRuntime {
            store,
            quiet: BTreeMap::new(),
            errors: BTreeMap::new(),
            auto_apply_errors: BTreeMap::new(),
            auto_apply_warnings: BTreeMap::new(),
            waiting: BTreeMap::new(),
            accounting_blocks: BTreeMap::new(),
            status_cache: NativeSchedulerStatusCache::default(),
            lab_mode: false,
        };
        let mut daemon = CalibrationDaemon::bind(&state_dir).unwrap();

        daemon
            .reconcile_native_scheduler_reservations(&mut scheduler, 1_100, "20260805", "202608")
            .unwrap();
        let blocked = scheduler.store.load_state("wan_sqm").unwrap().unwrap();
        assert!(blocked.budget.accounting_blocked);
        assert!(blocked.budget.reservation.is_some());
        assert_eq!(blocked.budget.daily_charged_bytes, 4_000);
        assert!(blocked.cursor.failed_attempt.is_some());
        assert!(scheduler.errors["wan_sqm"].contains("full reservation retained"));

        daemon.native_scheduler = Some(scheduler);
        assert!(daemon.summary_response().contains(
            "\"native_scheduler_accounting_blocks\":[{\"instance\":\"wan_sqm\",\"charged_bytes\":4000}]"
        ));
        let acknowledgement = SchedulerAccountingAcknowledgement {
            request_id: "3".repeat(32),
            instance: "wan_sqm".to_string(),
        };
        let encoded = acknowledgement.encode().unwrap();
        assert_eq!(
            SchedulerAccountingAcknowledgement::decode(&encoded).unwrap(),
            acknowledgement
        );
        let response = daemon.handle_scheduler_accounting_acknowledgement(&acknowledgement);
        assert!(response.contains("\"state\":\"acknowledged\""));
        assert!(response.contains("\"charged_bytes\":4000"));
        assert!(response.contains("\"reservation_refunded\":false"));
        let acknowledged = daemon
            .native_scheduler
            .as_ref()
            .unwrap()
            .store
            .load_state("wan_sqm")
            .unwrap()
            .unwrap();
        assert!(!acknowledged.budget.accounting_blocked);
        assert!(acknowledged.budget.reservation.is_none());
        assert_eq!(acknowledged.budget.daily_charged_bytes, 4_000);
        assert!(daemon
            .summary_response()
            .contains("\"native_scheduler_accounting_blocks\":[]"));

        drop(daemon);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn corrupt_wan_a_does_not_block_wan_b_poll_or_calendar_wakeup() {
        let root = temp_path("sched-instance-isolation");
        let state_dir = root.join("state");
        let scheduler_dir = root.join("scheduler");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let store = SchedulerStore::open(&scheduler_dir).unwrap();
        let now_unix_s = 1_754_000_000;
        let calendar = local_calendar(now_unix_s, 0, 0).unwrap();
        let wan_b = SchedulerInstanceState::new(
            ScheduleCursor::new("wan_b".to_string(), now_unix_s + 600).unwrap(),
            BudgetLedger::new(
                "wan_b".to_string(),
                calendar.day,
                calendar.month,
                10_000,
                50_000,
            )
            .unwrap(),
        )
        .unwrap();
        store.persist_state(&wan_b).unwrap();
        let mut corrupt = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(scheduler_dir.join("instance-wan_a.state"))
            .unwrap();
        corrupt.write_all(b"truncated\n").unwrap();
        drop(corrupt);

        let mut scheduler = NativeSchedulerRuntime {
            store,
            quiet: BTreeMap::new(),
            errors: BTreeMap::new(),
            auto_apply_errors: BTreeMap::new(),
            auto_apply_warnings: BTreeMap::new(),
            waiting: BTreeMap::new(),
            accounting_blocks: BTreeMap::new(),
            status_cache: NativeSchedulerStatusCache::default(),
            lab_mode: false,
        };
        let mut daemon = CalibrationDaemon::bind(&state_dir).unwrap();
        let instances = vec![scheduled_config("wan_a"), scheduled_config("wan_b")];

        daemon
            .poll_native_scheduler_instances(&mut scheduler, &instances, true, now_unix_s)
            .unwrap();
        assert!(scheduler.errors["wan_a"].contains("state load or initialization failed"));
        assert!(!scheduler.errors.contains_key("_global"));
        assert_eq!(scheduler.waiting.get("wan_b"), Some(&"not-due"));
        let snapshot = SchedulerConfigSnapshot {
            instances,
            issues: Vec::new(),
        };
        assert_eq!(
            build_native_scheduler_status_rows(
                &scheduler,
                &snapshot,
                now_unix_s,
                Instant::now(),
                &local_calendar(now_unix_s, 0, 0).unwrap().day,
                &local_calendar(now_unix_s, 0, 0).unwrap().month,
            )
            .unwrap()
            .next_scheduler_wake_at,
            Some(now_unix_s + 600)
        );

        drop(daemon);
        drop(scheduler);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_wan_a_does_not_block_wan_b_reservation_reconciliation() {
        let root = temp_path("sched-reconcile-isolation");
        let state_dir = root.join("state");
        let scheduler_dir = root.join("scheduler");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let store = SchedulerStore::open(&scheduler_dir).unwrap();
        store
            .persist_state(&reserved_scheduler_state("wan_b"))
            .unwrap();
        let mut corrupt = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(scheduler_dir.join("instance-wan_a.state"))
            .unwrap();
        corrupt.write_all(b"truncated\n").unwrap();
        drop(corrupt);
        let mut scheduler = NativeSchedulerRuntime {
            store,
            quiet: BTreeMap::new(),
            errors: BTreeMap::new(),
            auto_apply_errors: BTreeMap::new(),
            auto_apply_warnings: BTreeMap::new(),
            waiting: BTreeMap::new(),
            accounting_blocks: BTreeMap::new(),
            status_cache: NativeSchedulerStatusCache::default(),
            lab_mode: false,
        };
        let mut daemon = CalibrationDaemon::bind(&state_dir).unwrap();

        daemon
            .reconcile_native_scheduler_reservations(&mut scheduler, 1_100, "20260805", "202608")
            .unwrap();
        assert!(scheduler.errors["wan_a"].contains("reconciliation failed"));
        assert!(scheduler.errors["wan_b"].contains("full reservation retained"));
        assert_eq!(scheduler.accounting_blocks.get("wan_b"), Some(&4_000));
        let wan_b = scheduler.store.load_state("wan_b").unwrap().unwrap();
        assert!(wan_b.budget.accounting_blocked);
        assert!(wan_b.budget.reservation.is_some());
        assert!(wan_b.cursor.failed_attempt.is_some());

        drop(daemon);
        drop(scheduler);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn operator_ack_mutates_only_the_selected_scheduler_instance() {
        let root = temp_path("sched-ack-isolation");
        let state_dir = root.join("state");
        let scheduler_dir = root.join("scheduler");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let store = SchedulerStore::open(&scheduler_dir).unwrap();
        let mut wan_a = reserved_scheduler_state("wan_a");
        let mut wan_b = reserved_scheduler_state("wan_b");
        for state in [&mut wan_a, &mut wan_b] {
            state
                .budget
                .mark_accounting_unknown(&"1".repeat(32), &"2".repeat(32))
                .unwrap();
            store.persist_state(state).unwrap();
        }
        let wan_b_before = store.load_state("wan_b").unwrap().unwrap();
        let scheduler = NativeSchedulerRuntime {
            store,
            quiet: BTreeMap::new(),
            errors: BTreeMap::from([
                ("wan_a".to_string(), "blocked".to_string()),
                ("wan_b".to_string(), "blocked".to_string()),
            ]),
            auto_apply_errors: BTreeMap::new(),
            auto_apply_warnings: BTreeMap::new(),
            waiting: BTreeMap::new(),
            accounting_blocks: BTreeMap::from([
                ("wan_a".to_string(), 4_000),
                ("wan_b".to_string(), 4_000),
            ]),
            status_cache: NativeSchedulerStatusCache::default(),
            lab_mode: false,
        };
        let mut daemon = CalibrationDaemon::bind(&state_dir).unwrap();
        daemon.native_scheduler = Some(scheduler);

        let response = daemon.handle_scheduler_accounting_acknowledgement(
            &SchedulerAccountingAcknowledgement {
                request_id: "7".repeat(32),
                instance: "wan_a".to_string(),
            },
        );
        assert!(response.contains("\"state\":\"acknowledged\""));
        let scheduler = daemon.native_scheduler.as_ref().unwrap();
        assert_eq!(
            scheduler.store.load_state("wan_b").unwrap().unwrap(),
            wan_b_before
        );
        assert!(scheduler.errors.contains_key("wan_b"));
        assert!(scheduler.accounting_blocks.contains_key("wan_b"));
        assert!(!scheduler.errors.contains_key("wan_a"));
        assert!(!scheduler.accounting_blocks.contains_key("wan_a"));

        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn operator_ack_refuses_to_discard_a_live_scheduled_jobs_accounting_path() {
        let root = temp_path("sched-live-ack");
        let state_dir = root.join("state");
        let scheduler_dir = root.join("scheduler");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let store = SchedulerStore::open(&scheduler_dir).unwrap();
        let mut persisted = reserved_scheduler_state("wan_sqm");
        persisted
            .budget
            .mark_accounting_unknown(&"1".repeat(32), &"2".repeat(32))
            .unwrap();
        store.persist_state(&persisted).unwrap();
        let scheduler = NativeSchedulerRuntime {
            store,
            quiet: BTreeMap::new(),
            errors: BTreeMap::new(),
            auto_apply_errors: BTreeMap::new(),
            auto_apply_warnings: BTreeMap::new(),
            waiting: BTreeMap::new(),
            accounting_blocks: BTreeMap::from([("wan_sqm".to_string(), 4_000)]),
            status_cache: NativeSchedulerStatusCache::default(),
            lab_mode: false,
        };
        let mut daemon = CalibrationDaemon::bind(&state_dir).unwrap();
        let mut operation = operation_request('2', '3', "wan_sqm");
        operation.origin = OperationOrigin::Scheduler;
        let journal = JobJournal::queued(&operation, &daemon.coordinator, true).unwrap();
        daemon.jobs.push(ScannedJob {
            request: operation,
            journal,
            disposition: JournalDisposition::Queued,
        });
        daemon.native_scheduler = Some(scheduler);

        let response = daemon.handle_scheduler_accounting_acknowledgement(
            &SchedulerAccountingAcknowledgement {
                request_id: "4".repeat(32),
                instance: "wan_sqm".to_string(),
            },
        );
        assert!(response.contains("\"error_code\":\"scheduler-accounting-recoverable\""));
        let retained = daemon
            .native_scheduler
            .as_ref()
            .unwrap()
            .store
            .load_state("wan_sqm")
            .unwrap()
            .unwrap();
        assert!(retained.budget.accounting_blocked);
        assert!(retained.budget.reservation.is_some());
        assert_eq!(retained.budget.daily_charged_bytes, 4_000);

        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn operator_ack_refuses_while_exact_terminal_traffic_evidence_is_available() {
        let root = temp_path("sched-exact-ack");
        let mut daemon = blocked_scheduler_with_terminal_job(&root, true);
        let response = daemon.handle_scheduler_accounting_acknowledgement(
            &SchedulerAccountingAcknowledgement {
                request_id: "5".repeat(32),
                instance: "wan_sqm".to_string(),
            },
        );
        assert!(response.contains("\"error_code\":\"scheduler-accounting-recoverable\""));
        let retained = daemon
            .native_scheduler
            .as_ref()
            .unwrap()
            .store
            .load_state("wan_sqm")
            .unwrap()
            .unwrap();
        assert!(retained.budget.accounting_blocked);
        assert!(retained.budget.reservation.is_some());
        assert_eq!(retained.budget.daily_charged_bytes, 4_000);
        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn operator_ack_accepts_full_charge_when_terminal_evidence_is_missing() {
        let root = temp_path("sched-missing-terminal-ack");
        let mut daemon = blocked_scheduler_with_terminal_job(&root, false);
        let response = daemon.handle_scheduler_accounting_acknowledgement(
            &SchedulerAccountingAcknowledgement {
                request_id: "6".repeat(32),
                instance: "wan_sqm".to_string(),
            },
        );
        assert!(response.contains("\"state\":\"acknowledged\""));
        assert!(response.contains("\"charged_bytes\":4000"));
        let acknowledged = daemon
            .native_scheduler
            .as_ref()
            .unwrap()
            .store
            .load_state("wan_sqm")
            .unwrap()
            .unwrap();
        assert!(!acknowledged.budget.accounting_blocked);
        assert!(acknowledged.budget.reservation.is_none());
        assert_eq!(acknowledged.budget.daily_charged_bytes, 4_000);
        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_snapshot_wait_is_owned_by_current_state_not_a_retry_timer() {
        let root = temp_path("runtime-snapshot-state");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let snapshot_path = root.join("rating-runtime");
        let mut request = operation_request('a', 'b', "wan");
        request.deadline_unix_ms = 2_000_000;
        let now = 1_000_000;

        assert!(matches!(
            native_runtime_snapshot_readiness_at(&request, &snapshot_path, now),
            NativeRuntimeSnapshotReadiness::Waiting { ref code, .. }
                if code == "runtime-snapshot-missing"
        ));

        let mut snapshot = ready_runtime_snapshot(now - 1_000);
        fs::write(&snapshot_path, snapshot.encode().unwrap()).unwrap();
        fs::set_permissions(&snapshot_path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(matches!(
            native_runtime_snapshot_readiness_at(&request, &snapshot_path, now),
            NativeRuntimeSnapshotReadiness::Ready(_)
        ));

        let mut untyped = snapshot.clone();
        untyped.download_qdisc_kind = None;
        untyped.upload_qdisc_kind = None;
        let current = untyped.encode().unwrap();
        assert!(current.starts_with("cake-autorate-rating-runtime\t5\n"));
        assert!(current.contains(&format!(
            "evidence_contract={}\n",
            rating::RATING_EVIDENCE_CONTRACT
        )));
        let retired_v3 = current
            .replacen(
                "cake-autorate-rating-runtime\t5",
                "cake-autorate-rating-runtime\t3",
                1,
            )
            .replace(
                &format!("evidence_contract={}\n", rating::RATING_EVIDENCE_CONTRACT),
                "",
            )
            .replace("download_qdisc_kind=\n", "")
            .replace("upload_qdisc_kind=\n", "");
        fs::write(&snapshot_path, retired_v3).unwrap();
        assert!(matches!(
            native_runtime_snapshot_readiness_at(&request, &snapshot_path, now),
            NativeRuntimeSnapshotReadiness::Unsafe { ref code, .. }
                if code == "runtime-snapshot-invalid"
        ));
        fs::write(&snapshot_path, snapshot.encode().unwrap()).unwrap();

        snapshot.route_test_ready = false;
        fs::write(&snapshot_path, snapshot.encode().unwrap()).unwrap();
        assert!(matches!(
            native_runtime_snapshot_readiness_at(&request, &snapshot_path, now),
            NativeRuntimeSnapshotReadiness::Waiting { ref code, .. }
                if code == "runtime-route-not-ready"
        ));

        snapshot.route_test_ready = true;
        snapshot.updated_unix_ms = now - 5_001;
        snapshot.capture_observed_unix_ms = now - 5_001;
        fs::write(&snapshot_path, snapshot.encode().unwrap()).unwrap();
        assert!(matches!(
            native_runtime_snapshot_readiness_at(&request, &snapshot_path, now),
            NativeRuntimeSnapshotReadiness::Waiting { ref code, .. }
                if code == "runtime-snapshot-stale"
        ));

        snapshot.updated_unix_ms = now + 5_001;
        fs::write(&snapshot_path, snapshot.encode().unwrap()).unwrap();
        assert!(matches!(
            native_runtime_snapshot_readiness_at(&request, &snapshot_path, now),
            NativeRuntimeSnapshotReadiness::Unsafe { ref code, .. }
                if code == "runtime-snapshot-clock-drift"
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn public_job_id_recovers_private_capability_without_leaking_it() {
        let root = temp_path("public-job-handle");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_autotune = true;
        let operation = operation_request('a', 'b', "wan");
        let started = daemon.handle(&job_message(ControlCommand::Start, &operation));
        assert!(started.contains(&format!("\"job_id\":\"{}\"", "a".repeat(32))));
        assert!(!started.contains(&"b".repeat(64)));

        let loaded = read_private_job_operation(&state_dir, &"a".repeat(32)).unwrap();
        assert_eq!(loaded, operation);
        assert!(read_private_job_operation(&state_dir, &"A".repeat(32)).is_err());
        assert!(read_private_job_operation(&state_dir, &"a".repeat(31)).is_err());

        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn public_rating_controls_reject_an_autotune_job_before_socket_access() {
        let root = temp_path("rating-control-kind");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_autotune = true;
        let operation = operation_request('a', 'b', "wan");
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));

        for command in ["rating-status", "rating-result", "rating-cancel"] {
            let error = run_calibrationctl(
                [
                    "--state-dir".to_string(),
                    state_dir.display().to_string(),
                    command.to_string(),
                    operation.identity.job_id.clone(),
                ]
                .into_iter(),
            )
            .unwrap_err();
            assert!(error.contains("belongs to another operation"));
        }

        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn public_speedtest_controls_reject_an_autotune_job_before_socket_access() {
        let root = temp_path("speedtest-control-kind");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_autotune = true;
        let operation = operation_request('a', 'b', "wan");
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));

        for command in ["speedtest-status", "speedtest-result", "speedtest-cancel"] {
            let error = run_calibrationctl(
                [
                    "--state-dir".to_string(),
                    state_dir.display().to_string(),
                    command.to_string(),
                    operation.identity.job_id.clone(),
                ]
                .into_iter(),
            )
            .unwrap_err();
            assert!(error.contains("belongs to another operation"));
        }

        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_admission_policy_creates_no_job_journal_or_lease() {
        let root = temp_path("invalid-admission-policy");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_autotune = true;
        let mut operation = operation_request('a', 'b', "wan");
        operation.capacity_learning_policy = Some(CapacityLearningPolicy::FixedCap);
        operation.service_dl_cap_kbps = None;
        operation.service_ul_cap_kbps = None;

        let response = daemon.handle(&job_message(ControlCommand::Start, &operation));
        assert!(response.contains("\"error_code\":\"invalid-request-policy\""));
        assert!(response.contains(
            "fixed-cap capacity learning requires download and upload service hard caps"
        ));
        assert!(daemon.jobs.is_empty());
        assert_eq!(daemon.leases.job_count(), 0);
        assert_eq!(fs::read_dir(state_dir.join("jobs")).unwrap().count(), 0);

        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn autotune_inspection_is_non_secret_and_contains_exact_live_identity() {
        let operation = operation_request('a', 'b', "wan");
        let response = autotune_inspection_response(&operation);
        assert!(response.starts_with("{\"state\":\"ready\""));
        assert!(response.contains("\"instance\":\"wan\""));
        assert!(response.contains("\"target_interface\":\"wan-device\""));
        assert!(response.contains("\"route\":\"55555555"));
        assert!(response.contains("\"config\":\"66666666"));
        assert!(response.contains("\"sqm\":\"77777777"));
        assert!(response.contains("\"secrets_exposed\":false"));
        assert!(!response.contains(&operation.identity.job_id));
        assert!(!response.contains(&operation.identity.job_token));
    }

    #[test]
    fn native_rating_public_result_is_complete_non_mutating_and_bounded() {
        let response = rating_public_result_response(
            &"a".repeat(32),
            &rating::RatingResultSnapshot {
                grade: "A+".to_string(),
                increase_ms: 4.25,
                started_unix_ms: 1_000,
                partial: false,
                incomplete: false,
                dl_grade: "A+".to_string(),
                ul_grade: "A".to_string(),
                dl_samples: 24,
                ul_samples: 21,
            },
        );
        assert!(response.starts_with("{\"state\":\"complete\""));
        assert!(response.contains("\"grade\":\"A+\""));
        assert!(response.contains("\"increase_ms\":4.25"));
        assert!(response.contains("\"dl_samples\":24"));
        assert!(response.contains("\"ul_samples\":21"));
        assert!(response.contains("\"partial\":false"));
        assert!(response.contains("\"incomplete\":false"));
        assert!(response.contains(&format!(
            "\"rating_method\":\"{}\"",
            rating::RATING_EVIDENCE_CONTRACT
        )));
        assert!(response.contains("\"evidence_source\":\"worst_of_icmp_and_transport\""));
        assert!(response.contains("\"icmp_basis\":\"controller_reflector_adaptive_baseline\""));
        assert!(response.contains("\"transport_basis\":\"endpoint_loaded_p90_minus_idle_p5\""));
        assert!(response.contains("\"limits_changed\":false"));
        assert!(response.len() < MAX_RESPONSE_BYTES);

        let mut partial = rating::RatingResultSnapshot {
            grade: "A".to_string(),
            increase_ms: 14.308,
            started_unix_ms: 1_000,
            partial: true,
            incomplete: false,
            dl_grade: String::new(),
            ul_grade: "A".to_string(),
            dl_samples: 0,
            ul_samples: 28,
        };
        let rejected = rating_public_result_response(&"b".repeat(32), &partial);
        assert!(rejected.contains("\"error_code\":\"result-terminal-incomplete\""));
        assert!(!rejected.contains("\"state\":\"complete\""));

        partial.partial = false;
        partial.dl_grade = "A".to_string();
        partial.dl_samples = 20;
        partial.incomplete = true;
        let rejected = rating_public_result_response(&"c".repeat(32), &partial);
        assert!(rejected.contains("\"error_code\":\"result-terminal-incomplete\""));
    }

    #[test]
    fn native_speedtest_public_result_is_current_topology_non_mutating_and_bounded() {
        let operation = mwan3_speedtest_operation('a', 'b', "wan");
        let response = speedtest_public_result_response(
            &operation,
            &speedtest::SpeedtestResult {
                direction: super::super::protocol::SpeedtestDirection::Both,
                download_kbps: Some(900_000),
                upload_kbps: Some(850_000),
                rx_bytes: 1_100_000_000,
                tx_bytes: 1_000_000_000,
                elapsed_ms: 22_000,
                server_id: Some(42),
                server_name: "Tallinn \"test\"".to_string(),
                server_sponsor: "Example".to_string(),
            },
        );
        assert!(response.starts_with("{\"state\":\"complete\""));
        assert!(response.contains("\"direction\":\"both\""));
        assert!(response.contains("\"download_kbps\":900000"));
        assert!(response.contains("\"upload_kbps\":850000"));
        assert!(response.contains("\"server_id\":42"));
        assert!(response.contains("Tallinn \\\"test\\\""));
        assert!(response.contains("\"calibration\":\"current\""));
        assert!(response.contains("\"shaper_bypassed\":false"));
        assert!(response.contains("\"runtime_mutated\":false"));
        assert!(response.contains("\"runtime_restored\":false"));
        assert!(response.contains("\"limits_changed\":false"));
        assert!(response.len() < MAX_RESPONSE_BYTES);

        let mut mismatched = operation.clone();
        mismatched.speedtest_direction = Some(super::super::protocol::SpeedtestDirection::Download);
        let rejected = speedtest_public_result_response(
            &mismatched,
            &speedtest::SpeedtestResult {
                direction: super::super::protocol::SpeedtestDirection::Both,
                download_kbps: Some(900_000),
                upload_kbps: Some(850_000),
                rx_bytes: 1_100_000_000,
                tx_bytes: 1_000_000_000,
                elapsed_ms: 22_000,
                server_id: Some(42),
                server_name: "Tallinn".to_string(),
                server_sponsor: "Example".to_string(),
            },
        );
        assert!(rejected.contains("\"error_code\":\"result-terminal-policy-mismatch\""));

        let mut unshaped = operation;
        unshaped.speedtest_topology = Some(super::super::protocol::SpeedtestTopology::Unshaped);
        let response = speedtest_public_result_response(
            &unshaped,
            &speedtest::SpeedtestResult {
                direction: super::super::protocol::SpeedtestDirection::Both,
                download_kbps: Some(900_000),
                upload_kbps: Some(850_000),
                rx_bytes: 1_100_000_000,
                tx_bytes: 1_000_000_000,
                elapsed_ms: 22_000,
                server_id: Some(42),
                server_name: "Tallinn".to_string(),
                server_sponsor: "Example".to_string(),
            },
        );
        assert!(response.contains("\"calibration\":\"unshaped\""));
        assert!(response.contains("\"shaper_bypassed\":true"));
        assert!(response.contains("\"runtime_restored\":true"));
    }

    #[test]
    fn daemon_projects_only_the_identity_bound_completed_speedtest_terminal() {
        let state_dir = temp_path("speedtest-public-terminal");
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_speedtest = true;
        let operation = mwan3_speedtest_operation('a', 'b', "wan");
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));

        let worker_run_id = "c".repeat(32);
        daemon.jobs[0].journal.state = super::super::protocol::OperationState::Completed;
        daemon.jobs[0].journal.worker_run_id = Some(worker_run_id.clone());
        let paths = daemon
            .journal_store
            .native_job_paths(&operation.identity.job_id, &worker_run_id)
            .unwrap();
        fs::write(
            &paths.terminal,
            format!(
                concat!(
                    "cake-autorate-speedtest-terminal\t1\n",
                    "job_id={}\nworker_run_id={}\nstate=complete\ncode=\n",
                    "direction=both\ndownload_kbps=900000\nupload_kbps=850000\n",
                    "rx_bytes=1100000000\ntx_bytes=1000000000\nelapsed_ms=22000\n",
                    "server_id=42\nserver_name_hex=54616c6c696e6e\n",
                    "server_sponsor_hex=4578616d706c65\n\n"
                ),
                operation.identity.job_id, worker_run_id
            ),
        )
        .unwrap();
        fs::set_permissions(&paths.terminal, fs::Permissions::from_mode(0o600)).unwrap();

        let response = daemon.handle(&job_message(ControlCommand::Result, &operation));
        assert!(response.starts_with("{\"state\":\"complete\""));
        assert!(response.contains("\"download_kbps\":900000"));
        assert!(response.contains("\"upload_kbps\":850000"));
        assert!(response.contains("\"server_name\":\"Tallinn\""));
        assert!(response.contains("\"calibration\":\"current\""));
        assert!(response.contains("\"runtime_mutated\":false"));

        fs::write(
            &paths.terminal,
            format!(
                concat!(
                    "cake-autorate-speedtest-terminal\t1\n",
                    "job_id={}\nworker_run_id={}\nstate=complete\ncode=\n",
                    "direction=both\ndownload_kbps=900000\nupload_kbps=850000\n",
                    "rx_bytes=1100000000\ntx_bytes=1000000000\nelapsed_ms=22000\n",
                    "server_id=42\nserver_name_hex=54616c6c696e6e\n",
                    "server_sponsor_hex=4578616d706c65\n\n"
                ),
                "d".repeat(32),
                worker_run_id
            ),
        )
        .unwrap();
        let rejected = daemon.handle(&job_message(ControlCommand::Result, &operation));
        assert!(rejected.contains("\"error_code\":\"result-terminal-identity-mismatch\""));

        drop(daemon);
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn current_rating_scan_is_fault_isolated_active_only_and_kind_exact() {
        let state_dir = temp_path("current-rating-scan");
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_rating = true;

        let mut rating = mwan3_rating_operation('a', 'b', "wan");
        rating.created_unix_ms = 2;
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &rating))
            .contains("\"state\":\"queued\""));

        let mut foreign = operation_request('c', 'd', "wan");
        foreign.created_unix_ms = 999;
        let foreign_journal = JobJournal::queued(
            &foreign,
            &daemon.coordinator,
            requires_heavy_traffic(foreign.identity.operation),
        )
        .unwrap();
        daemon
            .journal_store
            .create(&foreign, &foreign_journal)
            .unwrap();

        let jobs_root = state_dir.join("jobs");
        fs::write(jobs_root.join("not-a-job"), b"ignored\n").unwrap();
        symlink(&state_dir, jobs_root.join("symlink-entry")).unwrap();
        let corrupt = jobs_root.join("e".repeat(32));
        fs::create_dir(&corrupt).unwrap();
        fs::set_permissions(&corrupt, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(corrupt.join("request"), b"malformed\n").unwrap();

        let current = scan_current_rating_operation(&state_dir, "wan")
            .unwrap()
            .expect("active Rating job must remain discoverable");
        assert_eq!(current.identity.job_id, rating.identity.job_id);
        assert_eq!(current.identity.operation, OperationKind::AutomaticRating);

        assert!(daemon
            .handle(&job_message(ControlCommand::Cancel, &rating))
            .contains("\"state\":\"cancelled\""));
        assert!(scan_current_rating_operation(&state_dir, "wan")
            .unwrap()
            .is_none());

        drop(daemon);
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn current_speedtest_scan_is_fault_isolated_active_only_and_kind_exact() {
        let state_dir = temp_path("current-speedtest-scan");
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_speedtest = true;

        let mut speedtest = mwan3_speedtest_operation('a', 'b', "wan");
        speedtest.created_unix_ms = 2;
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &speedtest))
            .contains("\"state\":\"queued\""));

        let mut foreign = mwan3_rating_operation('c', 'd', "wan");
        foreign.created_unix_ms = 999;
        let foreign_journal = JobJournal::queued(
            &foreign,
            &daemon.coordinator,
            requires_heavy_traffic(foreign.identity.operation),
        )
        .unwrap();
        daemon
            .journal_store
            .create(&foreign, &foreign_journal)
            .unwrap();

        let jobs_root = state_dir.join("jobs");
        fs::write(jobs_root.join("not-a-job"), b"ignored\n").unwrap();
        symlink(&state_dir, jobs_root.join("symlink-entry")).unwrap();
        let corrupt = jobs_root.join("e".repeat(32));
        fs::create_dir(&corrupt).unwrap();
        fs::set_permissions(&corrupt, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(corrupt.join("request"), b"malformed\n").unwrap();

        let current = scan_current_speedtest_operation(&state_dir, "wan")
            .unwrap()
            .expect("active Speed Test job must remain discoverable");
        assert_eq!(current.identity.job_id, speedtest.identity.job_id);
        assert_eq!(current.identity.operation, OperationKind::Speedtest);

        assert!(daemon
            .handle(&job_message(ControlCommand::Cancel, &speedtest))
            .contains("\"state\":\"cancelled\""));
        assert!(scan_current_speedtest_operation(&state_dir, "wan")
            .unwrap()
            .is_none());

        drop(daemon);
        fs::remove_dir_all(state_dir).unwrap();
    }

    #[test]
    fn current_autotune_scan_prefers_active_then_exact_inert_review() {
        let state_dir = temp_path("current-autotune-scan");
        let daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();

        let mut review = operation_request('a', 'b', "wan");
        review.created_unix_ms = 2;
        let mut review_journal = JobJournal::queued(&review, &daemon.coordinator, true).unwrap();
        review_journal.state = super::super::protocol::OperationState::ReviewReady;
        review_journal.worker_run_id = Some("1".repeat(32));
        review_journal.terminal_kind = Some("result".to_string());
        review_journal.terminal_state = Some("complete".to_string());
        review_journal.heavy_lease_acquired = false;
        daemon
            .journal_store
            .create(&review, &review_journal)
            .unwrap();

        assert!(
            scan_current_autotune_operation(&state_dir, "wan")
                .unwrap()
                .is_none(),
            "a Review-shaped journal without canonical evidence must stay hidden"
        );
        let current = scan_current_autotune_operation_with_review_validator(
            &state_dir,
            "wan",
            |operation, _| operation.identity.job_id == review.identity.job_id,
        )
        .unwrap()
        .expect("a canonically validated inert Review must remain discoverable");
        assert_eq!(current.identity.job_id, review.identity.job_id);

        let mut active = operation_request('c', 'd', "wan");
        active.created_unix_ms = 3;
        let active_journal = JobJournal::queued(&active, &daemon.coordinator, true).unwrap();
        daemon
            .journal_store
            .create(&active, &active_journal)
            .unwrap();

        let selected =
            scan_current_autotune_operation_with_review_validator(&state_dir, "wan", |_, _| {
                panic!("Review validation must not run while active work exists")
            })
            .unwrap()
            .expect("an in-flight operation must take priority over an older Review");
        assert_eq!(selected.identity.job_id, active.identity.job_id);

        let mut foreign = mwan3_rating_operation('e', 'f', "wan");
        foreign.created_unix_ms = 999;
        let foreign_journal = JobJournal::queued(&foreign, &daemon.coordinator, true).unwrap();
        daemon
            .journal_store
            .create(&foreign, &foreign_journal)
            .unwrap();
        assert_eq!(
            scan_current_autotune_operation_with_review_validator(
                &state_dir,
                "wan",
                |_, _| panic!("foreign work must not force Review validation"),
            )
            .unwrap()
            .unwrap()
            .identity
            .job_id,
            active.identity.job_id,
            "a newer foreign operation kind must never become Auto-Tune authority"
        );

        fs::remove_dir_all(state_dir.join("jobs").join(&active.identity.job_id)).unwrap();
        assert_eq!(
            scan_current_autotune_operation_with_review_validator(
                &state_dir,
                "wan",
                |operation, _| operation.identity.job_id == review.identity.job_id,
            )
            .unwrap()
            .unwrap()
            .identity
            .job_id,
            review.identity.job_id,
            "the safe Review must become current again after the active job disappears"
        );

        let mut stale_review = operation_request('1', '2', "wan");
        stale_review.created_unix_ms = 4;
        let mut stale_journal =
            JobJournal::queued(&stale_review, &daemon.coordinator, true).unwrap();
        stale_journal.state = super::super::protocol::OperationState::ReviewReady;
        stale_journal.worker_run_id = Some("2".repeat(32));
        stale_journal.terminal_kind = Some("result".to_string());
        stale_journal.terminal_state = Some("complete".to_string());
        stale_journal.heavy_lease_acquired = false;
        daemon
            .journal_store
            .create(&stale_review, &stale_journal)
            .unwrap();
        assert_eq!(
            scan_current_autotune_operation_with_review_validator(
                &state_dir,
                "wan",
                |operation, _| operation.identity.job_id == review.identity.job_id,
            )
            .unwrap()
            .unwrap()
            .identity
            .job_id,
            review.identity.job_id,
            "a newer stale Review must not hide an older canonical Review"
        );

        let mut unsafe_review = operation_request('3', '4', "wan");
        unsafe_review.created_unix_ms = 5;
        let mut unsafe_journal =
            JobJournal::queued(&unsafe_review, &daemon.coordinator, true).unwrap();
        unsafe_journal.state = super::super::protocol::OperationState::ReviewReady;
        unsafe_journal.worker_run_id = Some("2".repeat(32));
        unsafe_journal.terminal_kind = Some("result".to_string());
        unsafe_journal.terminal_state = Some("failed".to_string());
        unsafe_journal.heavy_lease_acquired = false;
        daemon
            .journal_store
            .create(&unsafe_review, &unsafe_journal)
            .unwrap();
        assert_eq!(
            scan_current_autotune_operation_with_review_validator(
                &state_dir,
                "wan",
                |operation, _| {
                    assert_ne!(
                        operation.identity.job_id, unsafe_review.identity.job_id,
                        "an incomplete Review must be rejected before evidence replay"
                    );
                    operation.identity.job_id == review.identity.job_id
                },
            )
            .unwrap()
            .unwrap()
            .identity
            .job_id,
            review.identity.job_id,
            "a ReviewReady journal without exact result authority must remain hidden"
        );

        drop(daemon);
        fs::remove_dir_all(state_dir).unwrap();
    }

    fn mwan3_rating_operation(job_id: char, token: char, instance: &str) -> OperationRequest {
        let mut request = operation_request(job_id, token, instance);
        request.identity.operation = OperationKind::AutomaticRating;
        request.route.mode = OperationRouteMode::Mwan3;
        request.route.mwan3_member = Some("wan".to_string());
        request.route.source_ip = Some("192.0.2.1".parse().unwrap());
        request.route.fwmark = Some(0x100);
        request.route.routing_table = Some(100);
        request.profile = None;
        request.strategy = None;
        request.access_medium = None;
        request.access_source = None;
        request.access_confidence_percent = 0;
        request.capacity_learning_policy = None;
        request.service_dl_cap_kbps = None;
        request.service_ul_cap_kbps = None;
        request.managed_sqm_section = None;
        request.allow_sqm_disable = false;
        request
    }

    fn mwan3_speedtest_operation(job_id: char, token: char, instance: &str) -> OperationRequest {
        let mut request = mwan3_rating_operation(job_id, token, instance);
        request.identity.operation = OperationKind::Speedtest;
        request.speedtest_direction = Some(super::super::protocol::SpeedtestDirection::Both);
        request.speedtest_server_id = Some(42);
        request.speedtest_topology = Some(super::super::protocol::SpeedtestTopology::Current);
        request.allow_active_traffic = false;
        request
    }

    fn bootstrap_speedtest_operation(
        job_id: char,
        token: char,
        instance: &str,
    ) -> OperationRequest {
        let mut request = bootstrap_operation_request(job_id, token, instance);
        request.identity.operation = OperationKind::Speedtest;
        request.capture_policy =
            Some(crate::operations::autotune_capture_policy::AutotuneCapturePolicyId::StandardV2);
        request.speedtest_direction = Some(SpeedtestDirection::Both);
        request.speedtest_server_id = None;
        request.speedtest_topology = Some(SpeedtestTopology::Unshaped);
        request.profile = None;
        request.strategy = None;
        request.access_medium = None;
        request.access_source = None;
        request.access_confidence_percent = 0;
        request.capacity_learning_policy = None;
        request.service_dl_cap_kbps = None;
        request.service_ul_cap_kbps = None;
        request.allow_sqm_disable = false;
        request.allow_active_traffic = false;
        request.validate_admission_policy().unwrap();
        request
    }

    fn write_complete_speedtest_terminal(path: &Path, job_id: &str, worker_run_id: &str) {
        fs::write(
            path,
            format!(
                concat!(
                    "cake-autorate-speedtest-terminal\t1\n",
                    "job_id={}\nworker_run_id={}\nstate=complete\ncode=\n",
                    "direction=both\ndownload_kbps=900000\nupload_kbps=850000\n",
                    "rx_bytes=1100000000\ntx_bytes=1000000000\nelapsed_ms=22000\n",
                    "server_id=42\nserver_name_hex=54616c6c696e6e\n",
                    "server_sponsor_hex=4578616d706c65\n\n"
                ),
                job_id, worker_run_id
            ),
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    fn rejecting_route_pin_cleaner(job_id: &str, worker_run_id: &str) -> Result<(), String> {
        assert_eq!(job_id, "a".repeat(32));
        assert_eq!(worker_run_id, "c".repeat(32));
        Err("test-route-pin-cleaner-called".to_string())
    }

    fn forbidden_route_pin_cleaner(_: &str, _: &str) -> Result<(), String> {
        panic!("an operation without native speedtest accounting must not invoke cleanup")
    }

    fn allowing_route_pin_cleaner(_: &str, _: &str) -> Result<(), String> {
        Ok(())
    }

    #[test]
    fn native_route_pin_cleanup_covers_every_backend_using_operation() {
        let route_pinned = [
            OperationKind::Speedtest,
            OperationKind::AutomaticRating,
            OperationKind::FullAutotune,
        ];
        for operation in [
            OperationKind::FullAutotune,
            OperationKind::AutomaticRating,
            OperationKind::GuidedRating,
            OperationKind::Speedtest,
        ] {
            let mut request = operation_request('a', 'b', "wan");
            request.identity.operation = operation;
            for route_mode in [OperationRouteMode::Main, OperationRouteMode::Mwan3] {
                request.route.mode = route_mode;
                let expected = route_pinned.contains(&operation);
                assert_eq!(
                    operation_uses_native_route_pin(&request),
                    expected,
                    "unexpected route-pin cleanup policy for {} on {}",
                    operation.as_str(),
                    route_mode.as_str()
                );
                if expected {
                    assert_eq!(
                        cleanup_native_route_pin(
                            &request,
                            &"c".repeat(32),
                            rejecting_route_pin_cleaner,
                        )
                        .unwrap_err(),
                        "test-route-pin-cleaner-called"
                    );
                } else {
                    cleanup_native_route_pin(
                        &request,
                        &"c".repeat(32),
                        forbidden_route_pin_cleaner,
                    )
                    .unwrap();
                }
            }
        }
    }

    #[test]
    fn only_autotune_and_explicit_unshaped_speedtest_own_native_runtime() {
        let autotune = operation_request('a', 'b', "wan");
        assert!(request_requires_native_runtime(&autotune));
        let bootstrap = bootstrap_operation_request('1', '2', "wan");
        assert!(request_requires_native_runtime(&bootstrap));

        let current = mwan3_speedtest_operation('c', 'd', "wan");
        assert!(!request_requires_native_runtime(&current));

        let mut unshaped = current;
        unshaped.speedtest_topology = Some(super::super::protocol::SpeedtestTopology::Unshaped);
        assert!(request_requires_native_runtime(&unshaped));

        let mut bootstrap_speedtest = unshaped.clone();
        bootstrap_speedtest.target_state = OperationTargetState::AbsentBootstrap;
        assert!(!request_requires_native_runtime(&bootstrap_speedtest));

        let rating = mwan3_rating_operation('e', 'f', "wan");
        assert!(!request_requires_native_runtime(&rating));
    }

    #[test]
    fn bootstrap_speedtest_holds_the_ordinary_heavy_lease_without_runtime_ownership() {
        let root = temp_path("st-bootstrap-heavy");
        fs::create_dir_all(&root).unwrap();
        let operation = bootstrap_speedtest_operation('a', 'b', "wan");

        {
            let mut daemon = CalibrationDaemon::bind_with_admission(&root, true).unwrap();
            daemon.native_speedtest = true;
            assert!(daemon.supports_native_request(&operation));
            assert!(!request_requires_native_runtime(&operation));
            assert!(daemon
                .handle(&job_message(ControlCommand::Start, &operation))
                .contains("\"state\":\"queued\""));
            assert!(!daemon.jobs[0].journal.heavy_lease_acquired);
            assert!(daemon.acquire_heavy_lease_if_needed(0));
            assert!(daemon.jobs[0].journal.heavy_lease_acquired);
            assert!(!daemon.jobs[0].journal.runtime_mutated);
            assert!(!daemon.jobs[0].journal.recovery_required);
            assert!(daemon.bootstrap_runtime_children.is_empty());
        }

        let mut restarted = CalibrationDaemon::bind_with_admission(&root, true).unwrap();
        restarted.native_speedtest = true;
        assert_eq!(restarted.jobs.len(), 1);
        assert!(restarted.jobs[0].journal.heavy_lease_acquired);
        assert!(!restarted.jobs[0].journal.runtime_mutated);
        assert!(!request_requires_native_runtime(&restarted.jobs[0].request));
        assert!(restarted.supports_native_request(&restarted.jobs[0].request));
        assert!(restarted.bootstrap_runtime_children.is_empty());

        drop(restarted);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unshaped_speedtest_complete_before_runtime_mutation_is_rejected() {
        let root = temp_path("st-premature");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_speedtest = true;
        daemon.route_pin_cleaner = allowing_route_pin_cleaner;
        let mut operation = mwan3_speedtest_operation('a', 'b', "wan");
        operation.speedtest_topology = Some(super::super::protocol::SpeedtestTopology::Unshaped);
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));

        let worker_run_id = "c".repeat(32);
        let mut armed = daemon.jobs[0].journal.clone();
        armed.arm_native_worker(worker_run_id.clone()).unwrap();
        daemon.journal_store.update(&armed).unwrap();
        daemon.jobs[0].journal = armed;
        daemon.jobs[0].disposition = JournalDisposition::Launching;
        let paths = daemon
            .journal_store
            .native_job_paths(&operation.identity.job_id, &worker_run_id)
            .unwrap();
        write_complete_speedtest_terminal(
            &paths.terminal,
            &operation.identity.job_id,
            &worker_run_id,
        );

        daemon.settle_native_after_exit(0, None);
        assert_eq!(daemon.jobs[0].disposition, JournalDisposition::Settled);
        assert_eq!(
            daemon.jobs[0].journal.state,
            super::super::protocol::OperationState::Failed
        );
        assert_eq!(
            daemon.jobs[0].journal.diagnostic_code.as_deref(),
            Some("native-speedtest-complete-before-runtime-mutation")
        );
        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restored_unshaped_speedtest_settles_completed_not_review_ready() {
        let root = temp_path("st-restored");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_speedtest = true;
        daemon.route_pin_cleaner = allowing_route_pin_cleaner;
        let mut operation = mwan3_speedtest_operation('a', 'b', "wan");
        operation.speedtest_topology = Some(super::super::protocol::SpeedtestTopology::Unshaped);
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));
        assert!(daemon.acquire_heavy_lease_if_needed(0));

        let worker_run_id = "c".repeat(32);
        let mut recovering = daemon.jobs[0].journal.clone();
        recovering
            .arm_runtime_mutation(worker_run_id.clone())
            .unwrap();
        recovering
            .require_recovery("native-runtime-reconciliation-required")
            .unwrap();
        daemon.journal_store.update(&recovering).unwrap();
        daemon.jobs[0].journal = recovering;
        daemon.jobs[0].disposition = JournalDisposition::RecoveryRequired;
        let paths = daemon
            .journal_store
            .native_job_paths(&operation.identity.job_id, &worker_run_id)
            .unwrap();
        write_complete_speedtest_terminal(
            &paths.terminal,
            &operation.identity.job_id,
            &worker_run_id,
        );

        daemon.settle_native_runtime_recovery(0);
        assert_eq!(daemon.jobs[0].disposition, JournalDisposition::Settled);
        assert_eq!(
            daemon.jobs[0].journal.state,
            super::super::protocol::OperationState::Completed
        );
        assert_eq!(
            daemon.jobs[0].journal.terminal_kind.as_deref(),
            Some("result")
        );
        assert!(!daemon.jobs[0].journal.runtime_mutated);
        assert!(!daemon.jobs[0].journal.recovery_required);
        assert!(!daemon.job_errors.contains_key(&operation.identity.job_id));
        assert!(daemon
            .handle(&job_message(ControlCommand::Result, &operation))
            .contains("\"calibration\":\"unshaped\""));
        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn normal_native_settlement_calls_exact_route_pin_cleaner_before_settlement() {
        let root = temp_path("normal-route-pin-cleanup");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_rating = true;
        daemon.route_pin_cleaner = rejecting_route_pin_cleaner;
        let operation = mwan3_rating_operation('a', 'b', "wan");
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));

        let worker_run_id = "c".repeat(32);
        let mut armed = daemon.jobs[0].journal.clone();
        armed.arm_native_worker(worker_run_id).unwrap();
        daemon.journal_store.update(&armed).unwrap();
        daemon.jobs[0].journal = armed;
        daemon.jobs[0].disposition = JournalDisposition::Launching;

        daemon.settle_native_after_exit(0, Some(("failed", "test-fallback")));
        assert!(
            daemon.job_errors[&operation.identity.job_id].contains("test-route-pin-cleaner-called")
        );
        assert_eq!(daemon.jobs[0].disposition, JournalDisposition::Launching);
        assert!(!state::terminal(daemon.jobs[0].journal.state));
        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_autotune_cancellation_before_runtime_mutation_settles_as_cancelled() {
        let root = temp_path("at-pm-cancel");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_autotune = true;
        daemon.route_pin_cleaner = allowing_route_pin_cleaner;
        let operation = operation_request('a', 'b', "wan");
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));

        let worker_run_id = "c".repeat(32);
        let mut armed = daemon.jobs[0].journal.clone();
        armed.arm_native_worker(worker_run_id.clone()).unwrap();
        daemon.journal_store.update(&armed).unwrap();
        daemon.jobs[0].journal = armed;
        daemon.jobs[0].disposition = JournalDisposition::Launching;
        let paths = daemon
            .journal_store
            .native_job_paths(&operation.identity.job_id, &worker_run_id)
            .unwrap();
        full_autotune::publish_terminal_file(
            &paths.terminal,
            &full_autotune::AutotuneTerminalRecord {
                job_id: operation.identity.job_id.clone(),
                worker_run_id,
                consumed_traffic_bytes: 0,
                terminal: AutotuneTerminal::Cancelled,
            },
        )
        .unwrap();

        daemon.settle_native_after_exit(0, Some(("failed", "fallback-must-not-win")));
        assert_eq!(daemon.jobs[0].disposition, JournalDisposition::Settled);
        assert_eq!(
            daemon.jobs[0].journal.state,
            super::super::protocol::OperationState::Cancelled
        );
        assert_eq!(
            daemon.jobs[0].journal.terminal_state.as_deref(),
            Some("cancelled")
        );
        assert!(!daemon.job_errors.contains_key(&operation.identity.job_id));
        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bootstrap_cancellation_waits_for_both_parked_worker_and_runtime_owner() {
        let root = temp_path("bootstrap-cancel-both-processes");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_autotune = true;
        daemon.route_pin_cleaner = allowing_route_pin_cleaner;
        let operation = bootstrap_operation_request('a', 'b', "wan");
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));

        let spawn_sleep = |name: &str| {
            ManagedChild::spawn(
                &SpawnSpec {
                    program: PathBuf::from("/bin/sleep"),
                    arguments: vec![OsString::from("30")],
                    environment: Vec::new(),
                },
                private_log_file(&root.join(format!("{name}.stdout"))).unwrap(),
                private_log_file(&root.join(format!("{name}.stderr"))).unwrap(),
                Path::new(DEFAULT_PROC_ROOT),
            )
            .unwrap()
        };
        let mut worker = spawn_sleep("worker");
        let mut owner = spawn_sleep("owner");
        let worker_run_id = "c".repeat(32);
        let mut running = daemon.jobs[0].journal.clone();
        running.arm_native_worker(worker_run_id.clone()).unwrap();
        running
            .attach_native_running(worker.identity.clone(), worker_run_id)
            .unwrap();
        running
            .attach_bootstrap_runtime_owner(owner.identity.clone())
            .unwrap();
        daemon.journal_store.update(&running).unwrap();
        daemon.jobs[0].journal = running;
        daemon.jobs[0].disposition = JournalDisposition::LiveProcess;

        let response = daemon.handle(&job_message(ControlCommand::Cancel, &operation));
        assert!(response.contains("\"state\":\"cancelling\""));
        assert!(daemon
            .cancellations
            .get(&operation.identity.job_id)
            .unwrap()
            .runtime_owner
            .is_some());
        assert!(worker
            .wait_for_exit(Duration::from_secs(2))
            .unwrap()
            .is_some());
        assert!(owner
            .wait_for_exit(Duration::from_secs(2))
            .unwrap()
            .is_some());
        daemon.poll_worker_cancellations();

        assert_eq!(daemon.jobs[0].disposition, JournalDisposition::Settled);
        assert_eq!(
            daemon.jobs[0].journal.state,
            super::super::protocol::OperationState::Cancelled
        );
        assert!(daemon.jobs[0].journal.runtime_owner_process.is_none());
        assert!(!daemon.jobs[0].journal.runtime_mutated);
        assert!(!daemon.jobs[0].journal.heavy_lease_acquired);
        assert!(!daemon
            .cancellations
            .contains_key(&operation.identity.job_id));

        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bootstrap_cancellation_tracks_a_live_runtime_owner_after_worker_identity_loss() {
        let root = temp_path("bootstrap-cancel-owner-only");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_autotune = true;
        daemon.route_pin_cleaner = allowing_route_pin_cleaner;
        let operation = bootstrap_operation_request('9', 'a', "wan");
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));

        let spawn_sleep = |name: &str| {
            ManagedChild::spawn(
                &SpawnSpec {
                    program: PathBuf::from("/bin/sleep"),
                    arguments: vec![OsString::from("30")],
                    environment: Vec::new(),
                },
                private_log_file(&root.join(format!("{name}.stdout"))).unwrap(),
                private_log_file(&root.join(format!("{name}.stderr"))).unwrap(),
                Path::new(DEFAULT_PROC_ROOT),
            )
            .unwrap()
        };
        let mut worker = spawn_sleep("worker");
        let mut owner = spawn_sleep("owner");
        let worker_run_id = "b".repeat(32);
        let mut owner_only = daemon.jobs[0].journal.clone();
        owner_only.arm_native_worker(worker_run_id.clone()).unwrap();
        owner_only
            .attach_native_running(worker.identity.clone(), worker_run_id)
            .unwrap();
        owner_only
            .attach_bootstrap_runtime_owner(owner.identity.clone())
            .unwrap();
        worker
            .terminate(Path::new(DEFAULT_PROC_ROOT), Duration::from_millis(500))
            .unwrap();
        owner_only.process = None;
        owner_only.validate().unwrap();
        daemon.journal_store.update(&owner_only).unwrap();
        daemon.jobs[0].journal = owner_only;
        daemon.jobs[0].disposition = JournalDisposition::LiveProcess;

        let response = daemon.handle(&job_message(ControlCommand::Cancel, &operation));
        assert!(response.contains("\"state\":\"cancelling\""));
        let pending = daemon
            .cancellations
            .get(&operation.identity.job_id)
            .unwrap();
        assert!(pending.process.is_none());
        assert_eq!(pending.runtime_owner, Some(owner.identity.clone()));
        assert!(owner
            .wait_for_exit(Duration::from_secs(2))
            .unwrap()
            .is_some());
        daemon.poll_worker_cancellations();

        assert_eq!(daemon.jobs[0].disposition, JournalDisposition::Settled);
        assert_eq!(
            daemon.jobs[0].journal.state,
            super::super::protocol::OperationState::Cancelled
        );
        assert!(daemon.jobs[0].journal.runtime_owner_process.is_none());
        assert!(!daemon
            .cancellations
            .contains_key(&operation.identity.job_id));

        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bootstrap_mutated_cancellation_hands_the_live_owner_to_recovery() {
        let root = temp_path("bootstrap-cancel-owned-owner-zombie");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_autotune = true;
        daemon.route_pin_cleaner = allowing_route_pin_cleaner;
        let operation = bootstrap_operation_request('d', 'e', "wan");
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));

        let spawn_sleep = |name: &str| {
            ManagedChild::spawn(
                &SpawnSpec {
                    program: PathBuf::from("/bin/sleep"),
                    arguments: vec![OsString::from("30")],
                    environment: Vec::new(),
                },
                private_log_file(&root.join(format!("{name}.stdout"))).unwrap(),
                private_log_file(&root.join(format!("{name}.stderr"))).unwrap(),
                Path::new(DEFAULT_PROC_ROOT),
            )
            .unwrap()
        };
        let mut worker = spawn_sleep("worker");
        let mut owner = spawn_sleep("owner");
        owner.preserve_on_drop();
        let worker_run_id = "f".repeat(32);
        let mut running = daemon.jobs[0].journal.clone();
        running.arm_native_worker(worker_run_id.clone()).unwrap();
        running
            .attach_native_running(worker.identity.clone(), worker_run_id)
            .unwrap();
        running
            .attach_bootstrap_runtime_owner(owner.identity.clone())
            .unwrap();
        daemon.journal_store.update(&running).unwrap();
        daemon.jobs[0].journal = running;
        daemon.jobs[0].disposition = JournalDisposition::LiveProcess;
        assert!(daemon.acquire_heavy_lease_if_needed(0));

        let mut running = daemon.jobs[0].journal.clone();
        running.arm_attached_bootstrap_runtime_mutation().unwrap();
        daemon.journal_store.update(&running).unwrap();
        daemon.jobs[0].journal = running;
        daemon
            .bootstrap_runtime_children
            .insert(operation.identity.job_id.clone(), owner);

        let response = daemon.handle(&job_message(ControlCommand::Cancel, &operation));
        assert!(response.contains("\"state\":\"cancelling\""));
        for _ in 0..200 {
            daemon.poll_worker_cancellations();
            if daemon.jobs[0].journal.state != super::super::protocol::OperationState::Cancelling {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        assert_eq!(
            daemon.jobs[0].journal.state,
            super::super::protocol::OperationState::Recovering
        );
        assert_eq!(
            daemon.jobs[0].disposition,
            JournalDisposition::RecoveryRequired
        );
        assert!(!daemon
            .cancellations
            .contains_key(&operation.identity.job_id));
        assert!(daemon
            .bootstrap_runtime_children
            .contains_key(&operation.identity.job_id));
        assert!(worker
            .wait_for_exit(Duration::from_secs(2))
            .unwrap()
            .is_some());
        assert!(daemon.jobs[0].journal.runtime_owner_process.is_some());
        assert!(daemon
            .bootstrap_runtime_children
            .get_mut(&operation.identity.job_id)
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none());

        daemon
            .bootstrap_runtime_children
            .get_mut(&operation.identity.job_id)
            .unwrap()
            .terminate(Path::new(DEFAULT_PROC_ROOT), Duration::from_millis(500))
            .unwrap();
        daemon
            .bootstrap_runtime_children
            .remove(&operation.identity.job_id);

        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bootstrap_mutated_cancellation_survives_restart_and_preserves_owner() {
        let root = temp_path("bootstrap-mutated-cancel-restart");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        let operation = bootstrap_operation_request('4', '5', "wan");
        let spawn_term_resistant = |name: &str| {
            ManagedChild::spawn(
                &SpawnSpec {
                    program: PathBuf::from("/bin/bash"),
                    arguments: vec![
                        OsString::from("-c"),
                        OsString::from("trap '' TERM; exec sleep 30"),
                    ],
                    environment: Vec::new(),
                },
                private_log_file(&root.join(format!("{name}.stdout"))).unwrap(),
                private_log_file(&root.join(format!("{name}.stderr"))).unwrap(),
                Path::new(DEFAULT_PROC_ROOT),
            )
            .unwrap()
        };
        let mut worker = spawn_term_resistant("worker");
        let mut owner = spawn_term_resistant("owner");

        {
            let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
            daemon.native_autotune = true;
            assert!(daemon
                .handle(&job_message(ControlCommand::Start, &operation))
                .contains("\"state\":\"queued\""));
            let worker_run_id = "6".repeat(32);
            let mut running = daemon.jobs[0].journal.clone();
            running.arm_native_worker(worker_run_id.clone()).unwrap();
            running
                .attach_native_running(worker.identity.clone(), worker_run_id)
                .unwrap();
            running
                .attach_bootstrap_runtime_owner(owner.identity.clone())
                .unwrap();
            daemon.journal_store.update(&running).unwrap();
            daemon.jobs[0].journal = running;
            daemon.jobs[0].disposition = JournalDisposition::LiveProcess;
            assert!(daemon.acquire_heavy_lease_if_needed(0));
            let mut running = daemon.jobs[0].journal.clone();
            running.arm_attached_bootstrap_runtime_mutation().unwrap();
            daemon.journal_store.update(&running).unwrap();
            daemon.jobs[0].journal = running;

            let response = daemon.handle(&job_message(ControlCommand::Cancel, &operation));
            assert!(response.contains("\"state\":\"cancelling\""));
            assert!(daemon
                .cancellations
                .get(&operation.identity.job_id)
                .unwrap()
                .runtime_owner
                .is_some());
            assert!(owner
                .identity
                .still_matches(Path::new(DEFAULT_PROC_ROOT))
                .unwrap());
        }

        let mut restarted = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        restarted.native_autotune = true;
        restarted.route_pin_cleaner = allowing_route_pin_cleaner;
        assert_eq!(restarted.jobs.len(), 1);
        assert_eq!(
            restarted.jobs[0].journal.state,
            super::super::protocol::OperationState::Cancelling
        );
        assert!(restarted.jobs[0].journal.runtime_mutated);
        assert!(restarted
            .cancellations
            .get(&operation.identity.job_id)
            .unwrap()
            .runtime_owner
            .is_some());
        assert!(owner
            .identity
            .still_matches(Path::new(DEFAULT_PROC_ROOT))
            .unwrap());

        worker
            .terminate(Path::new(DEFAULT_PROC_ROOT), Duration::from_millis(100))
            .unwrap();
        restarted.poll_worker_cancellations();

        assert_eq!(
            restarted.jobs[0].journal.state,
            super::super::protocol::OperationState::Recovering
        );
        assert_eq!(
            restarted.jobs[0].disposition,
            JournalDisposition::RecoveryRequired
        );
        assert!(restarted.jobs[0].journal.runtime_owner_process.is_some());
        assert!(!restarted
            .cancellations
            .contains_key(&operation.identity.job_id));
        assert!(owner
            .identity
            .still_matches(Path::new(DEFAULT_PROC_ROOT))
            .unwrap());

        owner
            .terminate(Path::new(DEFAULT_PROC_ROOT), Duration::from_millis(100))
            .unwrap();
        drop(restarted);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bootstrap_dual_process_cancellation_survives_coordinator_restart() {
        let root = temp_path("bootstrap-cancel-restart-both");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        let operation = bootstrap_operation_request('6', '7', "wan");
        let spawn_term_resistant = |name: &str| {
            ManagedChild::spawn(
                &SpawnSpec {
                    program: PathBuf::from("/bin/bash"),
                    arguments: vec![
                        OsString::from("-c"),
                        OsString::from("trap '' TERM; exec sleep 30"),
                    ],
                    environment: Vec::new(),
                },
                private_log_file(&root.join(format!("{name}.stdout"))).unwrap(),
                private_log_file(&root.join(format!("{name}.stderr"))).unwrap(),
                Path::new(DEFAULT_PROC_ROOT),
            )
            .unwrap()
        };
        let mut worker = spawn_term_resistant("worker");
        let mut owner = spawn_term_resistant("owner");

        {
            let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
            daemon.native_autotune = true;
            assert!(daemon
                .handle(&job_message(ControlCommand::Start, &operation))
                .contains("\"state\":\"queued\""));
            let worker_run_id = "8".repeat(32);
            let mut running = daemon.jobs[0].journal.clone();
            running.arm_native_worker(worker_run_id.clone()).unwrap();
            running
                .attach_native_running(worker.identity.clone(), worker_run_id)
                .unwrap();
            running
                .attach_bootstrap_runtime_owner(owner.identity.clone())
                .unwrap();
            daemon.journal_store.update(&running).unwrap();
            daemon.jobs[0].journal = running;
            daemon.jobs[0].disposition = JournalDisposition::LiveProcess;

            let response = daemon.handle(&job_message(ControlCommand::Cancel, &operation));
            assert!(response.contains("\"state\":\"cancelling\""));
            assert!(daemon
                .cancellations
                .get(&operation.identity.job_id)
                .unwrap()
                .runtime_owner
                .is_some());
        }

        let mut restarted = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        restarted.native_autotune = true;
        restarted.route_pin_cleaner = allowing_route_pin_cleaner;
        assert_eq!(restarted.jobs.len(), 1);
        assert_eq!(
            restarted.jobs[0].journal.state,
            super::super::protocol::OperationState::Cancelling
        );
        assert_eq!(
            restarted.jobs[0].disposition,
            JournalDisposition::LiveProcess
        );
        assert!(restarted
            .cancellations
            .get(&operation.identity.job_id)
            .unwrap()
            .runtime_owner
            .is_some());

        worker
            .terminate(Path::new(DEFAULT_PROC_ROOT), Duration::from_millis(100))
            .unwrap();
        owner
            .terminate(Path::new(DEFAULT_PROC_ROOT), Duration::from_millis(100))
            .unwrap();
        restarted.poll_worker_cancellations();

        assert_eq!(restarted.jobs[0].disposition, JournalDisposition::Settled);
        assert_eq!(
            restarted.jobs[0].journal.state,
            super::super::protocol::OperationState::Cancelled
        );
        assert!(restarted.jobs[0].journal.runtime_owner_process.is_none());
        assert!(!restarted
            .cancellations
            .contains_key(&operation.identity.job_id));

        drop(restarted);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_cancellation_survives_coordinator_restart_without_retired_adapter() {
        let root = temp_path("native-cancel-restart");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();

        let stdout = private_log_file(&root.join("worker.stdout")).unwrap();
        let stderr = private_log_file(&root.join("worker.stderr")).unwrap();
        let spec = SpawnSpec {
            program: PathBuf::from("/usr/bin/dash"),
            arguments: vec![
                OsString::from("-c"),
                OsString::from("trap '' TERM; exec /bin/sleep 30"),
            ],
            environment: Vec::new(),
        };
        let mut child =
            ManagedChild::spawn(&spec, stdout, stderr, Path::new(DEFAULT_PROC_ROOT)).unwrap();

        let operation = guided_rating_request('a', 'b', "wan");
        let worker_run_id = "c".repeat(32);
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_rating = true;
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));

        let mut running = daemon.jobs[0].journal.clone();
        running.arm_native_worker(worker_run_id.clone()).unwrap();
        running
            .attach_native_running(child.identity.clone(), worker_run_id)
            .unwrap();
        daemon.journal_store.update(&running).unwrap();
        daemon.jobs[0].journal = running;
        daemon.jobs[0].disposition = JournalDisposition::LiveProcess;

        let cancelling = daemon.handle(&job_message(ControlCommand::Cancel, &operation));
        assert!(cancelling.contains("\"state\":\"cancelling\""));
        assert!(child
            .identity
            .still_matches(Path::new(DEFAULT_PROC_ROOT))
            .unwrap());
        drop(daemon);

        let mut restarted = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        assert_eq!(
            restarted.jobs[0].journal.state,
            super::super::protocol::OperationState::Cancelling
        );
        assert!(restarted
            .cancellations
            .contains_key(&operation.identity.job_id));
        restarted
            .cancellations
            .get_mut(&operation.identity.job_id)
            .unwrap()
            .kill_after = Instant::now();
        restarted.poll_worker_cancellations();
        assert!(child
            .wait_for_exit(Duration::from_secs(2))
            .unwrap()
            .is_some());
        restarted.poll_worker_cancellations();

        assert_eq!(
            restarted.jobs[0].journal.state,
            super::super::protocol::OperationState::Cancelled
        );
        assert_eq!(restarted.jobs[0].disposition, JournalDisposition::Settled);
        assert_eq!(restarted.leases.job_count(), 0);
        assert!(restarted.cancellations.is_empty());
        drop(restarted);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_recovery_cleanup_failure_retains_journal_and_heavy_lease() {
        let root = temp_path("runtime-route-pin-cleanup");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_components(&state_dir, true, None).unwrap();
        daemon.native_autotune = true;
        daemon.route_pin_cleaner = rejecting_route_pin_cleaner;
        let mut operation = operation_request('a', 'b', "wan");
        operation.route.mode = OperationRouteMode::Mwan3;
        operation.route.mwan3_member = Some("wan".to_string());
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));
        assert!(daemon.acquire_heavy_lease_if_needed(0));

        let mut recovering = daemon.jobs[0].journal.clone();
        recovering.arm_runtime_mutation("c".repeat(32)).unwrap();
        recovering
            .require_recovery("native-runtime-reconciliation-required")
            .unwrap();
        daemon.journal_store.update(&recovering).unwrap();
        daemon.jobs[0].journal = recovering;
        daemon.jobs[0].disposition = JournalDisposition::RecoveryRequired;

        daemon.poll_native_runtime_recoveries();
        assert!(
            daemon.job_errors[&operation.identity.job_id].contains("test-route-pin-cleaner-called")
        );
        assert_eq!(
            daemon.leases.owner(&LeaseKey::HeavyTraffic),
            Some(operation.identity.job_id.as_str())
        );
        assert_eq!(
            daemon.jobs[0].disposition,
            JournalDisposition::RecoveryRequired
        );

        daemon.job_errors.clear();
        daemon.settle_native_runtime_recovery(0);
        assert!(
            daemon.job_errors[&operation.identity.job_id].contains("test-route-pin-cleaner-called")
        );
        assert_eq!(
            daemon.leases.owner(&LeaseKey::HeavyTraffic),
            Some(operation.identity.job_id.as_str())
        );
        assert_eq!(
            daemon.jobs[0].disposition,
            JournalDisposition::RecoveryRequired
        );
        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_route_identity_is_exact_and_rejects_incomplete_policy_routes() {
        let mut operation = operation_request('a', 'b', "wan");
        operation.route.source_ip = Some("192.0.2.1".parse().unwrap());
        assert_eq!(
            runtime_route_identity(&operation).unwrap(),
            "main||wan-device|192.0.2.1||main"
        );

        operation.route.mode = OperationRouteMode::Mwan3;
        operation.route.mwan3_member = Some("wan".to_string());
        operation.route.fwmark = Some(0x100);
        operation.route.routing_table = Some(1);
        assert_eq!(
            runtime_route_identity(&operation).unwrap(),
            "mwan3|wan|wan-device|192.0.2.1|0x100|1"
        );

        operation.route.mwan3_member = None;
        assert_eq!(
            runtime_route_identity(&operation).unwrap_err(),
            "mwan3 runtime probe has no member"
        );
        operation.route.mwan3_member = Some("wan".to_string());
        operation.route.fwmark = None;
        assert_eq!(
            runtime_route_identity(&operation).unwrap_err(),
            "mwan3 runtime probe has no fwmark"
        );
        operation.route.fwmark = Some(0x100);
        operation.route.routing_table = None;
        assert_eq!(
            runtime_route_identity(&operation).unwrap_err(),
            "mwan3 runtime probe has no routing table"
        );

        operation.route.mode = OperationRouteMode::Main;
        operation.route.mwan3_member = None;
        operation.route.routing_table = Some(1);
        assert_eq!(
            runtime_route_identity(&operation).unwrap_err(),
            "main runtime probe carries policy-routing fields"
        );
    }

    fn job_message(command: ControlCommand, operation: &OperationRequest) -> ControlMessage {
        ControlMessage {
            control: ControlRequest {
                request_id: "9".repeat(32),
                command,
                job_id: Some(operation.identity.job_id.clone()),
                job_token: Some(operation.identity.job_token.clone()),
            },
            operation: (command == ControlCommand::Start).then(|| operation.clone()),
        }
    }

    fn unsafe_runtime(_: &OperationRequest) -> RuntimeAttestation {
        RuntimeAttestation::Unsafe {
            code: "runtime-sqm-mismatch".to_string(),
            message: "foreign qdisc owns the requested interface".to_string(),
        }
    }

    fn waiting_runtime(_: &OperationRequest) -> RuntimeAttestation {
        RuntimeAttestation::Waiting {
            code: "runtime-sqm-settling".to_string(),
            message: "managed IFB is not ready".to_string(),
        }
    }

    fn mismatched_route_runtime(request: &OperationRequest) -> RuntimeAttestation {
        match super::super::autotune_request::operation_route_matches_config(
            "mwan3",
            "wan",
            &request.route,
        ) {
            Ok(()) => RuntimeAttestation::Ready,
            Err(message) => RuntimeAttestation::Unsafe {
                code: "runtime-route-config-mismatch".to_string(),
                message,
            },
        }
    }

    #[test]
    fn native_runtime_terminal_preserves_specific_recovery_diagnostics() {
        assert_eq!(
            native_runtime_terminal_diagnostic(Some("native-runtime-permit-build-failed")),
            (false, "native-runtime-permit-build-failed")
        );
        assert_eq!(
            native_runtime_terminal_diagnostic(Some("native-runtime-reconciliation-required")),
            (false, "native-runtime-worker-exited")
        );
        assert_eq!(
            native_runtime_terminal_diagnostic(Some("native-runtime-cancelled")),
            (true, "native-runtime-cancelled")
        );
        assert_eq!(
            native_runtime_terminal_diagnostic(Some("foreign-reconciliation-required")),
            (false, "native-runtime-worker-exited")
        );
    }

    #[test]
    fn expected_restore_first_reconciliation_is_not_logged_as_recovery_error() {
        assert!(!native_recovery_log_is_error("native-runtime-cancelled"));
        assert!(!native_recovery_log_is_error(
            "native-runtime-reconciliation-required"
        ));
        assert!(native_recovery_log_is_error(
            "native-worker-reconciliation-required"
        ));
    }

    #[test]
    fn unreviewable_pair_terminal_remains_inconclusive_for_restore_first_settlement() {
        let directory = temp_path("native-pair-terminal");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let job_id = "1".repeat(32);
        let worker_run_id = "2".repeat(32);
        let paths = super::super::journal::NativeJobPaths {
            request: directory.join("request"),
            terminal: directory.join(format!("terminal-{worker_run_id}")),
            review: directory.join(format!("review-{worker_run_id}.json")),
            apply_manifest: directory.join(format!("apply-manifest-{worker_run_id}.json")),
            public_result: directory.join(format!("public-review-{worker_run_id}.json")),
            permit: directory.join(format!("permit-{worker_run_id}")),
            stdout: directory.join("stdout.log"),
            stderr: directory.join("stderr.log"),
            bootstrap_runtime_dir: directory.join(format!("bootstrap-runtime-{worker_run_id}")),
            bootstrap_runtime_stdout: directory.join("bootstrap-runtime-stdout.log"),
            bootstrap_runtime_stderr: directory.join("bootstrap-runtime-stderr.log"),
        };
        full_autotune::publish_terminal_file(
            &paths.terminal,
            &full_autotune::AutotuneTerminalRecord {
                job_id: job_id.clone(),
                worker_run_id: worker_run_id.clone(),
                consumed_traffic_bytes: 0,
                terminal: AutotuneTerminal::Inconclusive {
                    code: "pair-options-unreviewable".to_string(),
                },
            },
        )
        .unwrap();

        let outcome = native_autotune_terminal_outcome(
            &paths,
            &job_id,
            &worker_run_id,
            "boot-test",
            &"3".repeat(32),
        )
        .unwrap()
        .unwrap();
        assert_eq!(outcome.state, "inconclusive");
        assert_eq!(
            outcome.diagnostic.as_deref(),
            Some("pair-options-unreviewable")
        );
        assert!(!paths.review.exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn native_autotune_complete_terminal_rejects_arbitrary_self_digested_review() {
        let directory = temp_path("native-review-terminal");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        let job_id = "1".repeat(32);
        let worker_run_id = "2".repeat(32);
        let paths = super::super::journal::NativeJobPaths {
            request: directory.join("request"),
            terminal: directory.join(format!("terminal-{worker_run_id}")),
            review: directory.join(format!("review-{worker_run_id}.json")),
            apply_manifest: directory.join(format!("apply-manifest-{worker_run_id}.json")),
            public_result: directory.join(format!("public-review-{worker_run_id}.json")),
            permit: directory.join(format!("permit-{worker_run_id}")),
            stdout: directory.join("stdout.log"),
            stderr: directory.join("stderr.log"),
            bootstrap_runtime_dir: directory.join(format!("bootstrap-runtime-{worker_run_id}")),
            bootstrap_runtime_stdout: directory.join("bootstrap-runtime-stdout.log"),
            bootstrap_runtime_stderr: directory.join("bootstrap-runtime-stderr.log"),
        };
        let review = b"{\"state\":\"complete\",\"schema_version\":8}\n";
        rating::atomic_private_write(&paths.review, review).unwrap();
        let review_digest = super::super::sqm_identity::sha256sum(review).unwrap();
        full_autotune::publish_terminal_file(
            &paths.terminal,
            &full_autotune::AutotuneTerminalRecord {
                job_id: job_id.clone(),
                worker_run_id: worker_run_id.clone(),
                consumed_traffic_bytes: 0,
                terminal: AutotuneTerminal::Complete {
                    review_digest: review_digest.clone(),
                },
            },
        )
        .unwrap();

        let outcome = native_autotune_terminal_outcome(
            &paths,
            &job_id,
            &worker_run_id,
            "boot-test",
            &"3".repeat(32),
        )
        .unwrap()
        .unwrap();
        assert_eq!(outcome.state, "failed");
        assert_eq!(
            outcome.diagnostic.as_deref(),
            Some("native-review-transaction-invalid")
        );

        fs::remove_file(&paths.review).unwrap();
        rating::atomic_private_write(
            &paths.review,
            b"{\"state\":\"complete\",\"schema_version\":9}\n",
        )
        .unwrap();
        let outcome = native_autotune_terminal_outcome(
            &paths,
            &job_id,
            &worker_run_id,
            "boot-test",
            &"3".repeat(32),
        )
        .unwrap()
        .unwrap();
        assert_eq!(outcome.state, "failed");
        assert_eq!(
            outcome.diagnostic.as_deref(),
            Some("native-review-transaction-invalid")
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn exact_restoring_ack_releases_only_global_heavy_lease_and_survives_restart() {
        let root = temp_path("heavy-recovery-release");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_components(&state_dir, true, None).unwrap();
        daemon.native_autotune = true;
        let operation = operation_request('a', 'b', "wan");
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));
        assert!(daemon.acquire_heavy_lease_if_needed(0));

        let worker_run_id = "8".repeat(32);
        let mut recovering = daemon.jobs[0].journal.clone();
        recovering
            .arm_runtime_mutation(worker_run_id.clone())
            .unwrap();
        recovering
            .require_recovery("native-runtime-reconciliation-required")
            .unwrap();
        daemon.journal_store.update(&recovering).unwrap();
        daemon.jobs[0].journal = recovering;
        daemon.jobs[0].disposition = JournalDisposition::RecoveryRequired;

        let ack = super::super::full_autotune::AutotuneRuntimeAck {
            permit_id: "9".repeat(32),
            job_id: operation.identity.job_id.clone(),
            worker_run_id,
            sequence: 1,
            updated_boot_ms: 1_000_000,
            target_interface: operation.identity.target_interface.clone(),
            route_fingerprint: operation.identity.route_fingerprint.clone(),
            sqm_fingerprint: operation.identity.sqm_fingerprint.clone(),
            state: RuntimeAckState::Restoring,
            topology: None,
            download_kbps: None,
            upload_kbps: None,
            diagnostic_code: Some("worker-exited".to_string()),
        };
        assert!(daemon
            .maybe_release_heavy_lease_during_recovery(0, &ack)
            .unwrap());
        assert_eq!(daemon.leases.owner(&LeaseKey::HeavyTraffic), None);
        assert_eq!(
            daemon.leases.owner(&LeaseKey::Instance("wan".to_string())),
            Some(operation.identity.job_id.as_str())
        );
        assert!(!daemon.jobs[0].journal.heavy_lease_acquired);
        assert!(daemon.jobs[0].journal.recovery_required);
        drop(daemon);

        let restarted = CalibrationDaemon::bind_with_components(&state_dir, true, None).unwrap();
        assert_eq!(restarted.leases.owner(&LeaseKey::HeavyTraffic), None);
        assert_eq!(
            restarted
                .leases
                .owner(&LeaseKey::Instance("wan".to_string())),
            Some(operation.identity.job_id.as_str())
        );
        assert_eq!(restarted.leases.job_count(), 1);
        assert!(restarted.jobs[0].journal.recovery_required);
        drop(restarted);
        fs::remove_dir_all(root).unwrap();
    }

    fn serve_one(mut daemon: CalibrationDaemon) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            for _ in 0..100 {
                if daemon.serve_once().unwrap().is_some() {
                    return;
                }
                thread::sleep(Duration::from_millis(5));
            }
            panic!("calibration daemon received no test request");
        })
    }

    fn cleanup_state_dir(dir: &Path) {
        let _ = fs::remove_dir(dir.join("jobs"));
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn startup_prunes_oldest_settled_history_without_disabling_admission() {
        let dir = temp_path("settled-retention");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let coordinator = CoordinatorIdentity::current().unwrap();
        let store = JournalStore::open(&dir, coordinator.clone()).unwrap();
        for index in 0..=MAX_JOURNAL_JOBS {
            let mut operation = operation_request('a', 'b', "wan");
            operation.identity.job_id = format!("{index:032x}");
            operation.identity.job_token = format!("{index:064x}");
            operation.created_unix_ms = index as u64 + 1;
            let mut journal = JobJournal::queued(&operation, &coordinator, true).unwrap();
            store.create(&operation, &journal).unwrap();
            journal.state = OperationState::Cancelled;
            journal.sequence += 1;
            store.update(&journal).unwrap();
        }
        drop(store);

        let daemon = CalibrationDaemon::bind_with_admission(&dir, true).unwrap();
        assert!(daemon.startup_issues.is_empty());
        assert!(daemon.admission_available());
        assert_eq!(daemon.jobs.len(), JOURNAL_RETENTION_TARGET);
        assert_eq!(
            daemon
                .jobs
                .iter()
                .map(|job| job.request.created_unix_ms)
                .min(),
            Some(18)
        );
        assert_eq!(
            fs::read_dir(dir.join("jobs")).unwrap().count(),
            JOURNAL_RETENTION_TARGET
        );
        drop(daemon);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn partial_retirement_failure_keeps_memory_aligned_and_retries_cleanly() {
        let dir = temp_path("partial-settled-retention");
        let mut daemon = CalibrationDaemon::bind_with_admission(&dir, true).unwrap();
        for index in 0..(JOURNAL_RETENTION_TARGET + 2) {
            let mut operation = operation_request('a', 'b', "wan");
            operation.identity.job_id = format!("{index:032x}");
            operation.identity.job_token = format!("{index:064x}");
            operation.created_unix_ms = index as u64 + 1;
            let mut journal = JobJournal::queued(&operation, &daemon.coordinator, true).unwrap();
            daemon.journal_store.create(&operation, &journal).unwrap();
            journal.state = OperationState::Cancelled;
            journal.sequence += 1;
            daemon.journal_store.update(&journal).unwrap();
            daemon.jobs.push(ScannedJob {
                request: operation,
                journal,
                disposition: JournalDisposition::Settled,
            });
        }
        let first = format!("{:032x}", 0);
        let second = format!("{:032x}", 1);
        let second_dir = dir.join("jobs").join(&second);
        fs::set_permissions(&second_dir, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(daemon.prune_settled_history().is_err());
        assert!(!daemon.jobs.iter().any(|job| job.journal.job_id == first));
        assert!(daemon.jobs.iter().any(|job| job.journal.job_id == second));
        assert!(!dir.join("jobs").join(first).exists());
        assert!(second_dir.exists());

        fs::set_permissions(&second_dir, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(daemon.prune_settled_history().unwrap(), 1);
        assert_eq!(daemon.jobs.len(), JOURNAL_RETENTION_TARGET);
        assert!(daemon.admission_available());
        drop(daemon);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn start_prunes_old_history_before_publishing_the_new_job() {
        let dir = temp_path("start-settled-retention");
        let mut daemon = CalibrationDaemon::bind_with_admission(&dir, true).unwrap();
        daemon.native_autotune = true;
        for index in 0..=JOURNAL_RETENTION_TARGET {
            let mut operation = operation_request('a', 'b', "wan");
            operation.identity.job_id = format!("{index:032x}");
            operation.identity.job_token = format!("{index:064x}");
            operation.created_unix_ms = index as u64 + 1;
            let mut journal = JobJournal::queued(&operation, &daemon.coordinator, true).unwrap();
            daemon.journal_store.create(&operation, &journal).unwrap();
            journal.state = OperationState::Cancelled;
            journal.sequence += 1;
            daemon.journal_store.update(&journal).unwrap();
            daemon.jobs.push(ScannedJob {
                request: operation,
                journal,
                disposition: JournalDisposition::Settled,
            });
        }
        let new_job = operation_request('f', 'e', "wan");
        let response = daemon.handle(&job_message(ControlCommand::Start, &new_job));
        assert!(response.contains("\"state\":\"queued\""));
        assert_eq!(daemon.jobs.len(), JOURNAL_RETENTION_TARGET + 1);
        assert!(!daemon
            .jobs
            .iter()
            .any(|job| job.journal.job_id == format!("{:032x}", 0)));
        assert!(daemon
            .jobs
            .iter()
            .any(|job| job.journal.job_id == new_job.identity.job_id));
        drop(daemon);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn daemon_answers_bounded_ping_and_summary() {
        let dir = temp_path("ping");
        let thread = serve_one(CalibrationDaemon::bind(&dir).unwrap());
        let response = send_control(&dir, &request(ControlCommand::Ping)).unwrap();
        assert!(response.contains("\"state\":\"ready\""));
        thread.join().unwrap();

        let thread = serve_one(CalibrationDaemon::bind(&dir).unwrap());
        let response = send_control(&dir, &request(ControlCommand::Summary)).unwrap();
        assert!(response.contains("\"active_job\":false"));
        assert!(response.contains("\"active_operations\":[]"));
        assert!(response.contains("\"native_scheduler_waiting\":[]"));
        assert!(response.contains("\"native_bootstrap_autotune\":false"));
        assert!(response.contains("\"native_bootstrap_speedtest\":true"));
        assert!(response.contains("\"native_speedtest_auto_backend\":true"));
        assert!(response.contains("\"native_autotune_auto_backend\":true"));
        assert!(response.contains("\"native_operation_status_identity_version\":1"));
        assert!(response.contains("\"native_autotune_status_identity_version\":1"));
        assert!(response.contains(&format!(
            "\"native_public_result_version\":{}",
            crate::operations::autotune_public::NATIVE_PUBLIC_RESULT_MAX_SCHEMA_VERSION
        )));
        thread.join().unwrap();
        cleanup_state_dir(&dir);
    }

    #[test]
    fn readiness_probe_binds_the_control_socket_to_its_exact_peer_pid() {
        let dir = temp_path("readiness-peer");
        let identity = ProcessIdentity::current().unwrap();
        let thread = serve_one(CalibrationDaemon::bind(&dir).unwrap());
        assert!(probe_calibration_coordinator_control(&dir, &identity).unwrap());
        thread.join().unwrap();
        cleanup_state_dir(&dir);
    }

    #[test]
    fn operation_status_identity_is_exact_for_every_native_operation() {
        let mut operation = operation_request('a', 'b', "wan");
        assert_eq!(
            operation_status_request_identity(&operation),
            concat!(
                ",\"request_identity_schema_version\":1",
                ",\"target_interface\":\"wan-device\"",
                ",\"backend\":\"speedtest-go\"",
                ",\"speedtest_direction\":null",
                ",\"speedtest_server_id\":null",
                ",\"speedtest_topology\":null",
                ",\"route_mode\":\"main\"",
                ",\"mwan3_member\":null",
                ",\"target_state\":\"existing_managed\"",
                ",\"managed_sqm_section\":\"wan_sqm\"",
                ",\"profile\":\"variable_link\"",
                ",\"calibration_strategy\":\"full_raw\"",
                ",\"origin\":\"luci\""
            )
        );

        operation.route.mode = OperationRouteMode::Mwan3;
        operation.route.mwan3_member = Some("wanb".to_string());
        operation.profile = Some(AutotuneProfile::GamingExtreme);
        operation.strategy = Some(CalibrationStrategy::ShapedOnly);
        operation.origin = OperationOrigin::Scheduler;
        let identity = operation_status_request_identity(&operation);
        assert!(identity.contains("\"route_mode\":\"mwan3\""));
        assert!(identity.contains("\"mwan3_member\":\"wanb\""));
        assert!(identity.contains("\"profile\":\"gaming_extreme\""));
        assert!(identity.contains("\"calibration_strategy\":\"shaped_only\""));
        assert!(identity.contains("\"origin\":\"scheduler\""));

        operation.identity.operation = OperationKind::AutomaticRating;
        operation.profile = None;
        operation.strategy = None;
        operation.managed_sqm_section = None;
        let rating_identity = operation_status_request_identity(&operation);
        assert!(rating_identity.contains("\"profile\":null"));
        assert!(rating_identity.contains("\"calibration_strategy\":null"));
        assert!(rating_identity.contains("\"managed_sqm_section\":null"));

        operation.identity.operation = OperationKind::Speedtest;
        operation.speedtest_direction = Some(SpeedtestDirection::Both);
        operation.speedtest_server_id = Some(12345);
        operation.speedtest_topology = Some(SpeedtestTopology::Unshaped);
        let speedtest_identity = operation_status_request_identity(&operation);
        assert!(speedtest_identity.contains("\"speedtest_direction\":\"both\""));
        assert!(speedtest_identity.contains("\"speedtest_server_id\":\"12345\""));
        assert!(speedtest_identity.contains("\"speedtest_topology\":\"unshaped\""));
    }

    #[test]
    fn operation_status_exposes_typed_terminal_state_and_diagnostic_code() {
        let operation = operation_request('a', 'b', "wan");
        let coordinator = CoordinatorIdentity::current().unwrap();
        let journal = JobJournal::queued(&operation, &coordinator, true).unwrap();
        let mut job = ScannedJob {
            request: operation,
            journal,
            disposition: JournalDisposition::Queued,
        };
        let active = job_response(&job, false);
        assert!(active.contains("\"terminal_state\":null"));
        assert!(active.contains("\"diagnostic_code\":null"));

        job.journal.state = OperationState::Failed;
        job.journal.terminal_state = Some("inconclusive".to_string());
        job.journal.diagnostic_code = Some("pair-options-unreviewable".to_string());
        let terminal = job_response(&job, true);
        assert!(terminal.contains("\"terminal_state\":\"inconclusive\""));
        assert!(terminal.contains("\"diagnostic_code\":\"pair-options-unreviewable\""));
    }

    #[test]
    fn daemon_summary_exposes_scheduler_wait_reason_without_promoting_it_to_an_error() {
        let root = temp_path("scheduler-wait-summary");
        let state_dir = root.join("state");
        let scheduler_dir = root.join("scheduler");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();

        let mut daemon = CalibrationDaemon::bind(&state_dir).unwrap();
        daemon.native_scheduler = Some(NativeSchedulerRuntime {
            store: SchedulerStore::open(&scheduler_dir).unwrap(),
            quiet: BTreeMap::new(),
            errors: BTreeMap::new(),
            auto_apply_errors: BTreeMap::new(),
            auto_apply_warnings: BTreeMap::new(),
            waiting: BTreeMap::from([("wan".to_string(), "quiet-window-pending")]),
            accounting_blocks: BTreeMap::new(),
            status_cache: NativeSchedulerStatusCache::default(),
            lab_mode: true,
        });

        let response = daemon.summary_response();
        assert!(response.contains("\"native_scheduler_errors\":0"));
        assert!(response.contains(
            "\"native_scheduler_waiting\":[{\"instance\":\"wan\",\"reason\":\"quiet-window-pending\"}]"
        ));

        drop(daemon);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn native_public_result_requires_exact_authorization_and_accepts_review_ready_state() {
        let dir = temp_path("native-result-authorization");
        let mut daemon = CalibrationDaemon::bind_with_components(&dir, true, None).unwrap();
        daemon.native_autotune = true;
        let operation = operation_request('a', 'b', "wan");
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &operation))
            .contains("\"state\":\"queued\""));

        let pending = daemon.handle(&job_message(ControlCommand::Result, &operation));
        assert!(pending.contains("\"error_code\":\"result-not-ready\""));

        let mut wrong_token = job_message(ControlCommand::Result, &operation);
        wrong_token.control.job_token = Some("f".repeat(64));
        let unauthorized = daemon.handle(&wrong_token);
        assert!(unauthorized.contains("\"error_code\":\"job-not-found\""));

        assert!(
            native_autotune_apply_check(&dir, &operation.identity.job_id, None)
                .unwrap_err()
                .contains("requires a settled Review")
        );
        daemon.jobs[0].journal.state = super::super::protocol::OperationState::ReviewReady;
        let review_ready = daemon.handle(&job_message(ControlCommand::Result, &operation));
        assert!(review_ready.contains("\"error_code\":\"result-verification-failed\""));
        assert!(!review_ready.contains("\"error_code\":\"result-not-ready\""));

        drop(daemon);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn native_apply_check_cli_rejects_missing_and_extra_public_arguments() {
        assert!(
            run_calibrationctl(["autotune-apply-check".to_string()].into_iter())
                .unwrap_err()
                .contains("requires a public job ID")
        );
        assert!(run_calibrationctl(
            [
                "autotune-apply-check".to_string(),
                "11".repeat(16),
                "recommended".to_string(),
                "unexpected".to_string(),
            ]
            .into_iter(),
        )
        .unwrap_err()
        .contains("unexpected arguments"));
    }

    #[test]
    fn native_apply_check_recognizes_exact_candidate_before_original_baseline() {
        assert!(existing_native_apply_check_result(true, || {
            panic!("an exact applied candidate must not be compared with its original baseline")
        })
        .unwrap());

        assert!(!existing_native_apply_check_result(false, || RuntimeAttestation::Ready).unwrap());

        let error = existing_native_apply_check_result(false, || RuntimeAttestation::Unsafe {
            code: "runtime-config-changed".to_string(),
            message: "managed configuration changed after launch".to_string(),
        })
        .unwrap_err();
        assert!(error.contains("runtime-config-changed"));
        assert!(error.contains("managed configuration changed after launch"));
    }

    #[test]
    fn native_bootstrap_start_cli_requires_its_explicit_planned_section() {
        assert!(
            run_calibrationctl(["autotune-bootstrap-start".to_string()].into_iter())
                .unwrap_err()
                .contains("requires a planned SQM section")
        );
        assert!(run_calibrationctl(
            [
                "autotune-bootstrap-start".to_string(),
                "cake_wan_sqm".to_string(),
            ]
            .into_iter(),
        )
        .unwrap_err()
        .contains("--instance is required"));
    }

    #[test]
    fn native_bootstrap_speedtest_cli_requires_its_explicit_planned_section() {
        assert!(
            run_calibrationctl(["speedtest-bootstrap-start".to_string()].into_iter())
                .unwrap_err()
                .contains("requires a planned SQM section")
        );
        assert!(run_calibrationctl(
            [
                "speedtest-bootstrap-start".to_string(),
                "cake_wan_sqm".to_string(),
            ]
            .into_iter(),
        )
        .unwrap_err()
        .contains("--instance is required"));
    }

    #[test]
    fn bootstrap_apply_authority_promotes_the_frozen_v4_source_to_exact_v7() {
        let expected = fixture_plan();
        let source_plan = expected.source_apply().clone();
        let source_identity = source_plan.v4_identity().unwrap();
        let source_manifest = source_plan.canonical_manifest_bytes().unwrap();
        let source = VerifiedNativeApplyContext {
            request: source_plan.request.clone(),
            worker_run_id: source_identity.worker_run_id.clone(),
            bootstrap_runtime_dir: PathBuf::from("/not-read-by-pure-promotion"),
            option_id: source_identity.option_id.clone(),
            review_digest: source_identity.source_review_sha256.clone(),
            manifest_digest: source_identity.manifest_sha256.clone(),
            manifest: source_manifest,
            plan: source_plan,
        };
        let promoted =
            verified_bootstrap_apply_context(&source, expected.absent_baseline().clone()).unwrap();
        assert_eq!(
            promoted.manifest,
            expected.canonical_manifest_bytes().unwrap()
        );
        assert_eq!(
            promoted.manifest_digest,
            expected.canonical_manifest_sha256().unwrap()
        );

        let mut tampered = source;
        tampered.manifest_digest = "0".repeat(64);
        assert!(
            verified_bootstrap_apply_context(&tampered, expected.absent_baseline().clone(),)
                .unwrap_err()
                .contains("source manifest changed")
        );
    }

    #[test]
    fn bootstrap_apply_authority_promotes_raw_v5_to_disabled_v8() {
        let expected = fixture_raw_fallback_plan();
        let source_plan = expected.source_apply().clone();
        let source_identity = source_plan.authority_identity().unwrap();
        assert_eq!(source_identity.schema_version(), 5);
        let source_manifest = source_plan.canonical_manifest_bytes().unwrap();
        let source = VerifiedNativeApplyContext {
            request: source_plan.request.clone(),
            worker_run_id: source_identity.worker_run_id().to_string(),
            bootstrap_runtime_dir: PathBuf::from("/not-read-by-pure-raw-promotion"),
            option_id: source_identity.option_id().to_string(),
            review_digest: source_identity.source_review_sha256().to_string(),
            manifest_digest: source_identity.manifest_sha256().to_string(),
            manifest: source_manifest,
            plan: source_plan,
        };
        let promoted =
            verified_bootstrap_apply_context(&source, expected.absent_baseline().clone()).unwrap();

        assert_eq!(promoted.plan.manifest_schema_version(), 8);
        assert_eq!(
            promoted.plan.mode(),
            NativeBootstrapApplyMode::DisabledInactive
        );
        assert_eq!(
            promoted.manifest,
            expected.canonical_manifest_bytes().unwrap()
        );
        assert_eq!(
            promoted.manifest_digest,
            expected.canonical_manifest_sha256().unwrap()
        );
    }

    #[test]
    fn native_apply_cli_acknowledgements_are_bounded_typed_and_exact() {
        assert!(
            run_calibrationctl(["autotune-apply-start".to_string()].into_iter())
                .unwrap_err()
                .contains("requires a public job ID")
        );
        let parsed = parse_native_apply_acknowledgements(
            [
                "--ack".to_string(),
                "upload-capacity-retention".to_string(),
                "--ack".to_string(),
                "download-icmp-latency".to_string(),
            ]
            .into_iter(),
        )
        .unwrap();
        assert_eq!(
            parsed,
            vec![
                NativeApplyAcknowledgement::UploadCapacityRetention,
                NativeApplyAcknowledgement::DownloadIcmpLatency,
            ]
        );
        assert!(parse_native_apply_acknowledgements(
            ["--ack".to_string(), "unknown".to_string()].into_iter()
        )
        .unwrap_err()
        .contains("unknown"));
        assert!(parse_native_apply_acknowledgements(
            [
                "--ack".to_string(),
                "measurement-confidence".to_string(),
                "--ack".to_string(),
                "measurement-confidence".to_string(),
            ]
            .into_iter(),
        )
        .unwrap_err()
        .contains("duplicated"));

        let expected = vec![
            NativeApplyAcknowledgement::DownloadCapacityRetention,
            NativeApplyAcknowledgement::UploadShapingBypassed,
        ];
        assert!(require_exact_native_apply_acknowledgements(
            &expected,
            &[NativeApplyAcknowledgement::UploadShapingBypassed],
        )
        .unwrap_err()
        .contains("exact complete"));
        assert!(require_exact_native_apply_acknowledgements(
            &expected,
            &[
                NativeApplyAcknowledgement::DownloadCapacityRetention,
                NativeApplyAcknowledgement::UploadShapingBypassed,
                NativeApplyAcknowledgement::DownloadIcmpLatency,
            ],
        )
        .unwrap_err()
        .contains("exact complete"));
        require_exact_native_apply_acknowledgements(&expected, &expected).unwrap();
    }

    #[test]
    fn native_apply_start_receipts_before_private_review_replay() {
        let root = temp_path("native-apply-prompt-receipt");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_apply_store = NativeApplyCoordinatorStore::new(&root.join("apply"));
        let request = NativeApplyControlRequest::start(
            "11".repeat(16),
            "22".repeat(16),
            "recommended".to_string(),
            "33".repeat(32),
            "44".repeat(32),
            Vec::new(),
        );

        let (response, effect) = daemon.handle_native_apply_control(&request);
        assert_eq!(effect, ControlEffect::StateChanged);
        assert!(response.contains("\"state\":\"accepted\""));
        assert!(response.contains("\"generation\":1"));
        let accepted = daemon
            .native_apply_store
            .read_active()
            .unwrap()
            .expect("receipt must be durable before the response");
        assert_eq!(accepted.state, NativeApplyDispatchState::Accepted);
        assert_eq!(accepted.worker_run_id, "none");
        assert_eq!(daemon.native_apply_store.read_terminal().unwrap(), None);

        // The source job deliberately does not exist. Start still returns a
        // durable receipt because private Review replay belongs exclusively to
        // the worker phase after this control response.
        assert!(!state_dir.join("jobs").join("22".repeat(16)).exists());

        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_apply_terminal_success_is_reused_only_while_candidate_is_live() {
        let root = temp_path("native-apply-terminal-live-candidate");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_apply_store = NativeApplyCoordinatorStore::new(&root.join("apply"));
        daemon.native_apply_live_state_attestor = test_native_apply_live_candidate;
        let request = NativeApplyControlRequest::start(
            "11".repeat(16),
            "22".repeat(16),
            "recommended".to_string(),
            "33".repeat(32),
            "44".repeat(32),
            Vec::new(),
        );
        let terminal = terminalize_native_apply_for_test(
            &mut daemon,
            &request,
            NativeApplyTerminalOutcome::Applied,
            true,
        );

        let (response, effect) = daemon.handle_native_apply_control(&request);
        assert_eq!(effect, ControlEffect::ReadOnly);
        assert!(response.contains(&terminal.dispatch.apply_job_id));
        assert_eq!(daemon.native_apply_store.read_active().unwrap(), None);
        assert_eq!(
            daemon.native_apply_store.read_terminal().unwrap(),
            Some(terminal)
        );

        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_apply_terminal_is_rearmed_after_exact_external_rollback() {
        let root = temp_path("native-apply-terminal-live-original");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_apply_store = NativeApplyCoordinatorStore::new(&root.join("apply"));
        daemon.native_apply_live_state_attestor = test_native_apply_live_original;
        let request = NativeApplyControlRequest::start(
            "11".repeat(16),
            "22".repeat(16),
            "recommended".to_string(),
            "33".repeat(32),
            "44".repeat(32),
            Vec::new(),
        );
        let terminal = terminalize_native_apply_for_test(
            &mut daemon,
            &request,
            NativeApplyTerminalOutcome::Failed,
            false,
        );

        let (response, effect) = daemon.handle_native_apply_control(&request);
        assert_eq!(effect, ControlEffect::StateChanged);
        assert!(response.contains("\"state\":\"accepted\""));
        let rearmed = daemon.native_apply_store.read_active().unwrap().unwrap();
        assert_eq!(
            rearmed.generation,
            terminal.dispatch.generation.checked_add(1).unwrap()
        );
        assert_ne!(rearmed.apply_job_id, terminal.dispatch.apply_job_id);
        assert_eq!(daemon.native_apply_store.read_terminal().unwrap(), None);

        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_apply_terminal_preserves_evidence_for_foreign_or_pending_recovery_state() {
        let root = temp_path("native-apply-terminal-live-foreign");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_apply_store = NativeApplyCoordinatorStore::new(&root.join("apply"));
        daemon.native_apply_live_state_attestor = test_native_apply_live_foreign;
        let request = NativeApplyControlRequest::start(
            "11".repeat(16),
            "22".repeat(16),
            "recommended".to_string(),
            "33".repeat(32),
            "44".repeat(32),
            Vec::new(),
        );
        let terminal = terminalize_native_apply_for_test(
            &mut daemon,
            &request,
            NativeApplyTerminalOutcome::Applied,
            true,
        );
        let (response, effect) = daemon.handle_native_apply_control(&request);
        assert_eq!(effect, ControlEffect::ReadOnly);
        assert!(response.contains("native-apply-terminal-revalidation-failed"));
        assert_eq!(daemon.native_apply_store.read_active().unwrap(), None);
        assert_eq!(
            daemon.native_apply_store.read_terminal().unwrap(),
            Some(terminal)
        );

        drop(daemon);
        fs::remove_dir_all(root).unwrap();

        let root = temp_path("native-apply-terminal-recovery-pending");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_apply_store = NativeApplyCoordinatorStore::new(&root.join("apply"));
        daemon.native_apply_live_state_attestor = test_native_apply_recovery_pending;
        let terminal = terminalize_native_apply_for_test(
            &mut daemon,
            &request,
            NativeApplyTerminalOutcome::Failed,
            false,
        );
        let (response, effect) = daemon.handle_native_apply_control(&request);
        assert_eq!(effect, ControlEffect::ReadOnly);
        assert!(response.contains("native-apply-terminal-revalidation-failed"));
        assert!(response.contains("recovery remains pending"));
        assert_eq!(daemon.native_apply_store.read_active().unwrap(), None);
        assert_eq!(
            daemon.native_apply_store.read_terminal().unwrap(),
            Some(terminal)
        );

        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_apply_watch_wakes_only_after_generation_changes() {
        let root = temp_path("native-apply-generation-watch");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_apply_store = NativeApplyCoordinatorStore::new(&root.join("apply"));
        let start = NativeApplyControlRequest::start(
            "11".repeat(16),
            "22".repeat(16),
            "recommended".to_string(),
            "33".repeat(32),
            "44".repeat(32),
            Vec::new(),
        );
        let _ = daemon.handle_native_apply_control(&start);
        let accepted = daemon.native_apply_store.read_active().unwrap().unwrap();
        let watch = NativeApplyControlRequest::watch(
            "55".repeat(16),
            accepted.apply_job_id.clone(),
            accepted.apply_job_token.clone(),
            accepted.generation,
        );
        let (server, mut client) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        assert_eq!(
            daemon.serve_native_apply_watch(server, watch).unwrap(),
            ControlEffect::ReadOnly
        );
        assert_eq!(daemon.native_apply_watches.len(), 1);

        let validating = daemon
            .native_apply_store
            .mark_validating(&accepted)
            .unwrap();
        assert_eq!(validating.generation, 2);
        daemon.flush_native_apply_watches();
        assert!(daemon.native_apply_watches.is_empty());
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert!(response.contains("\"state\":\"validating\""));
        assert!(response.contains("\"generation\":2"));

        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_apply_watch_deadline_and_capacity_never_advance_apply_state() {
        let root = temp_path("native-apply-watch-safety-bounds");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_apply_store = NativeApplyCoordinatorStore::new(&root.join("apply"));
        let start_request = NativeApplyControlRequest::start(
            "11".repeat(16),
            "22".repeat(16),
            "recommended".to_string(),
            "33".repeat(32),
            "44".repeat(32),
            Vec::new(),
        );
        let _ = daemon.handle_native_apply_control(&start_request);
        let accepted = daemon.native_apply_store.read_active().unwrap().unwrap();
        let watch_request = NativeApplyControlRequest::watch(
            "55".repeat(16),
            accepted.apply_job_id.clone(),
            accepted.apply_job_token.clone(),
            accepted.generation,
        );

        let (server, mut client) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        daemon
            .serve_native_apply_watch(server, watch_request.clone())
            .unwrap();
        daemon.native_apply_watches[0].deadline = Instant::now() - Duration::from_secs(1);
        daemon.flush_native_apply_watches();
        let mut deadline_response = String::new();
        client.read_to_string(&mut deadline_response).unwrap();
        assert!(deadline_response.contains("\"state\":\"accepted\""));
        assert!(deadline_response.contains("\"generation\":1"));
        assert_eq!(
            daemon.native_apply_store.read_active().unwrap(),
            Some(accepted.clone())
        );

        let mut held_clients = Vec::new();
        for _ in 0..MAX_PENDING_NATIVE_APPLY_WATCHES {
            let (server, client) = UnixStream::pair().unwrap();
            daemon
                .serve_native_apply_watch(server, watch_request.clone())
                .unwrap();
            held_clients.push(client);
        }
        assert_eq!(
            daemon.native_apply_watches.len(),
            MAX_PENDING_NATIVE_APPLY_WATCHES
        );
        let (server, mut overflow_client) = UnixStream::pair().unwrap();
        overflow_client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        daemon
            .serve_native_apply_watch(server, watch_request)
            .unwrap();
        let mut capacity_response = String::new();
        overflow_client
            .read_to_string(&mut capacity_response)
            .unwrap();
        assert!(capacity_response.contains("native-apply-watch-capacity"));
        assert_eq!(
            daemon.native_apply_store.read_active().unwrap(),
            Some(accepted)
        );

        drop(held_clients);
        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_apply_watch_is_reconnectable_and_terminal_wins_over_lost_responses() {
        let root = temp_path("native-apply-watch-reconnect");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_admission(&state_dir, true).unwrap();
        daemon.native_apply_store = NativeApplyCoordinatorStore::new(&root.join("apply"));
        let start = NativeApplyControlRequest::start(
            "11".repeat(16),
            "22".repeat(16),
            "recommended".to_string(),
            "33".repeat(32),
            "44".repeat(32),
            Vec::new(),
        );
        let _ = daemon.handle_native_apply_control(&start);
        let accepted = daemon.native_apply_store.read_active().unwrap().unwrap();

        let future = NativeApplyControlRequest::watch(
            "77".repeat(16),
            accepted.apply_job_id.clone(),
            accepted.apply_job_token.clone(),
            accepted.generation + 1,
        );
        assert!(daemon
            .native_apply_watch_response(&future, accepted.generation + 1)
            .unwrap_err()
            .contains("ahead"));

        let lost = NativeApplyControlRequest::watch(
            "88".repeat(16),
            accepted.apply_job_id.clone(),
            accepted.apply_job_token.clone(),
            accepted.generation,
        );
        let (server, client) = UnixStream::pair().unwrap();
        assert_eq!(
            daemon.serve_native_apply_watch(server, lost).unwrap(),
            ControlEffect::ReadOnly
        );
        drop(client);
        let validating = daemon
            .native_apply_store
            .mark_validating(&accepted)
            .unwrap();
        daemon.flush_native_apply_watches();
        assert!(daemon.native_apply_watches.is_empty());
        assert_eq!(
            daemon.native_apply_store.read_active().unwrap(),
            Some(validating.clone())
        );

        let terminal = NativeApplyTerminalRecord {
            dispatch: validating.clone(),
            outcome: NativeApplyTerminalOutcome::Failed,
            recovery_cleared: true,
            diagnostic: "validation rejected before mutation".to_string(),
        };
        daemon.native_apply_store.complete(&terminal).unwrap();
        let reconnect = NativeApplyControlRequest::watch(
            "99".repeat(16),
            validating.apply_job_id,
            validating.apply_job_token,
            u64::MAX,
        );
        let response = daemon
            .native_apply_watch_response(&reconnect, u64::MAX)
            .unwrap()
            .expect("terminal watch must never wait");
        assert!(response.contains("\"state\":\"failed\""));
        assert!(response.contains("\"generation\":3"));

        drop(daemon);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn calibration_stop_requires_terminal_apply_without_a_worker_claim() {
        let root = temp_path("native-apply-service-stop");
        let store = NativeApplyCoordinatorStore::new(&root);
        attest_native_apply_store_idle_for_service_stop(&store).unwrap();

        let request = NativeApplyControlRequest::start(
            "11".repeat(16),
            "22".repeat(16),
            "recommended".to_string(),
            "33".repeat(32),
            "44".repeat(32),
            Vec::new(),
        );
        let candidate = NativeApplyDispatchRecord {
            state: NativeApplyDispatchState::Accepted,
            generation: 1,
            apply_job_id: "55".repeat(16),
            apply_job_token: "66".repeat(32),
            source_job_id: "22".repeat(16),
            worker_run_id: "none".to_string(),
            option_id: "recommended".to_string(),
            review_sha256: "33".repeat(32),
            source_manifest_sha256: "none".to_string(),
            manifest_sha256: "44".repeat(32),
            manifest_schema_version: 0,
            target_state: "none".to_string(),
            acknowledgements: Vec::new(),
        };
        let accepted = match store.admit(&request, candidate).unwrap() {
            NativeApplyAdmission::Created(record) => record,
            other => panic!("unexpected admission: {other:?}"),
        };
        assert!(attest_native_apply_store_idle_for_service_stop(&store)
            .unwrap_err()
            .contains("accepted at generation 1"));

        let validating = store.mark_validating(&accepted).unwrap();
        let claim = NativeApplyWorkerClaim::for_process(
            &validating,
            ProcessIdentity {
                pid: 4242,
                process_group: 4242,
                starttime_ticks: 7,
            },
        )
        .unwrap();
        assert_eq!(store.claim_worker(&validating, &claim).unwrap(), None);
        let terminal = NativeApplyTerminalRecord {
            outcome: NativeApplyTerminalOutcome::Failed,
            recovery_cleared: true,
            diagnostic: "validation rejected before mutation".to_string(),
            dispatch: validating,
        };
        store.complete(&terminal).unwrap();
        assert!(attest_native_apply_store_idle_for_service_stop(&store)
            .unwrap_err()
            .contains("worker claim"));
        store.remove_worker_claim(&claim).unwrap();
        attest_native_apply_store_idle_for_service_stop(&store).unwrap();

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_recovery_terminalizes_once_without_clearing_authority_or_respawning() {
        let root = temp_path("native-apply-recovery-terminal");
        let store = NativeApplyCoordinatorStore::new(&root.join("apply"));
        let request = NativeApplyControlRequest::start(
            "11".repeat(16),
            "22".repeat(16),
            "recommended".to_string(),
            "33".repeat(32),
            "44".repeat(32),
            Vec::new(),
        );
        let candidate = NativeApplyDispatchRecord {
            state: NativeApplyDispatchState::Accepted,
            generation: 1,
            apply_job_id: "55".repeat(16),
            apply_job_token: "66".repeat(32),
            source_job_id: "22".repeat(16),
            worker_run_id: "none".to_string(),
            option_id: "recommended".to_string(),
            review_sha256: "33".repeat(32),
            source_manifest_sha256: "none".to_string(),
            manifest_sha256: "44".repeat(32),
            manifest_schema_version: 0,
            target_state: "none".to_string(),
            acknowledgements: Vec::new(),
        };
        let accepted = match store.admit(&request, candidate).unwrap() {
            NativeApplyAdmission::Created(record) => record,
            other => panic!("unexpected admission: {other:?}"),
        };
        let validating = store.mark_validating(&accepted).unwrap();
        let applying = store
            .mark_applying(
                &validating,
                NativeApplyVerifiedDispatchIdentity {
                    worker_run_id: "77".repeat(16),
                    source_manifest_sha256: "44".repeat(32),
                    manifest_schema_version: 6,
                    target_state: "existing_managed".to_string(),
                },
            )
            .unwrap();

        settle_native_apply_recovery_attempt(
            &store,
            &applying,
            Some("directional lifecycle failed"),
            Err("exact rollback cannot classify the live files".to_string()),
        )
        .unwrap();

        assert_eq!(store.read_active().unwrap(), None);
        let terminal = store.read_terminal().unwrap().unwrap();
        assert_eq!(terminal.dispatch, applying);
        assert_eq!(terminal.outcome, NativeApplyTerminalOutcome::Failed);
        assert!(!terminal.recovery_cleared);
        assert!(terminal.diagnostic.contains("directional lifecycle failed"));
        assert!(terminal.diagnostic.contains("recovery remains pending"));
        assert_eq!(
            reconcile_native_apply_worker(&store, &root.join("proc")).unwrap(),
            NativeApplyWorkerReadiness::Idle
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_apply_worker_reconciliation_adopts_live_claim_and_relaunches_only_after_exit() {
        let root = temp_path("native-apply-worker-reconcile");
        let proc_root = root.join("proc");
        fs::create_dir_all(&proc_root).unwrap();
        let store = NativeApplyCoordinatorStore::new(&root.join("apply"));
        assert_eq!(
            reconcile_native_apply_worker(&store, &proc_root).unwrap(),
            NativeApplyWorkerReadiness::Idle
        );

        let request = NativeApplyControlRequest::start(
            "11".repeat(16),
            "22".repeat(16),
            "recommended".to_string(),
            "33".repeat(32),
            "44".repeat(32),
            Vec::new(),
        );
        let candidate = NativeApplyDispatchRecord {
            state: NativeApplyDispatchState::Accepted,
            generation: 1,
            apply_job_id: "55".repeat(16),
            apply_job_token: "66".repeat(32),
            source_job_id: "22".repeat(16),
            worker_run_id: "none".to_string(),
            option_id: "recommended".to_string(),
            review_sha256: "33".repeat(32),
            source_manifest_sha256: "none".to_string(),
            manifest_sha256: "44".repeat(32),
            manifest_schema_version: 0,
            target_state: "none".to_string(),
            acknowledgements: Vec::new(),
        };
        let accepted = match store.admit(&request, candidate).unwrap() {
            NativeApplyAdmission::Created(record) => record,
            other => panic!("unexpected admission: {other:?}"),
        };
        let validating = match reconcile_native_apply_worker(&store, &proc_root).unwrap() {
            NativeApplyWorkerReadiness::Launch(record) => record,
            other => panic!("unexpected worker readiness: {other:?}"),
        };
        assert_eq!(validating.state, NativeApplyDispatchState::Validating);
        assert_eq!(validating.generation, accepted.generation + 1);

        let identity = ProcessIdentity {
            pid: 4242,
            process_group: 4242,
            starttime_ticks: 99,
        };
        write_fake_process(&proc_root, &identity);
        let claim = NativeApplyWorkerClaim::for_process(&validating, identity.clone()).unwrap();
        assert_eq!(store.claim_worker(&validating, &claim).unwrap(), None);
        assert_eq!(
            reconcile_native_apply_worker(&store, &proc_root).unwrap(),
            NativeApplyWorkerReadiness::Live(identity.clone())
        );

        fs::remove_dir_all(proc_root.join(identity.pid.to_string())).unwrap();
        assert_eq!(
            reconcile_native_apply_worker(&store, &proc_root).unwrap(),
            NativeApplyWorkerReadiness::Launch(validating.clone())
        );
        assert_eq!(store.read_worker_claim().unwrap(), None);

        let applying = store
            .mark_applying(
                &validating,
                NativeApplyVerifiedDispatchIdentity {
                    worker_run_id: "77".repeat(16),
                    source_manifest_sha256: "88".repeat(32),
                    manifest_schema_version: 4,
                    target_state: "existing_managed".to_string(),
                },
            )
            .unwrap();
        let applying_identity = ProcessIdentity {
            pid: 4343,
            process_group: 4343,
            starttime_ticks: 100,
        };
        write_fake_process(&proc_root, &applying_identity);
        let applying_claim =
            NativeApplyWorkerClaim::for_process(&applying, applying_identity.clone()).unwrap();
        assert_eq!(
            store.claim_worker(&applying, &applying_claim).unwrap(),
            None
        );
        assert_eq!(
            reconcile_native_apply_worker(&store, &proc_root).unwrap(),
            NativeApplyWorkerReadiness::Live(applying_identity.clone())
        );
        fs::remove_dir_all(proc_root.join(applying_identity.pid.to_string())).unwrap();
        assert_eq!(
            reconcile_native_apply_worker(&store, &proc_root).unwrap(),
            NativeApplyWorkerReadiness::Launch(applying)
        );
        assert_eq!(store.read_worker_claim().unwrap(), None);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_apply_worker_validation_failure_is_terminal_before_mutation_authority() {
        let root = temp_path("native-apply-worker-validation-failure");
        let state_dir = root.join("state");
        fs::create_dir_all(&state_dir).unwrap();
        let store = NativeApplyCoordinatorStore::new(&root.join("apply"));
        let request = NativeApplyControlRequest::start(
            "11".repeat(16),
            "22".repeat(16),
            "recommended".to_string(),
            "33".repeat(32),
            "44".repeat(32),
            Vec::new(),
        );
        let candidate = NativeApplyDispatchRecord {
            state: NativeApplyDispatchState::Accepted,
            generation: 1,
            apply_job_id: "55".repeat(16),
            apply_job_token: "66".repeat(32),
            source_job_id: "22".repeat(16),
            worker_run_id: "none".to_string(),
            option_id: "recommended".to_string(),
            review_sha256: "33".repeat(32),
            source_manifest_sha256: "none".to_string(),
            manifest_sha256: "44".repeat(32),
            manifest_schema_version: 0,
            target_state: "none".to_string(),
            acknowledgements: Vec::new(),
        };
        let accepted = match store.admit(&request, candidate).unwrap() {
            NativeApplyAdmission::Created(record) => record,
            other => panic!("unexpected admission: {other:?}"),
        };
        let validating = store.mark_validating(&accepted).unwrap();
        execute_native_apply_worker(&state_dir, &store, validating).unwrap();
        assert_eq!(store.read_active().unwrap(), None);
        let terminal = store.read_terminal().unwrap().unwrap();
        assert_eq!(terminal.outcome, NativeApplyTerminalOutcome::Failed);
        assert!(terminal.recovery_cleared);
        assert!(!terminal.diagnostic.is_empty());
        assert!(!state_dir.join("jobs").exists());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn async_native_apply_recovery_requires_verified_applying_authority() {
        let root = temp_path("native-apply-recovery-unverified");
        let store = NativeApplyCoordinatorStore::new(&root);
        let source_job_id = "11".repeat(16);
        let worker_run_id = "22".repeat(16);
        let review_sha256 = "33".repeat(32);
        let manifest_sha256 = "44".repeat(32);
        let request = NativeApplyControlRequest::start(
            "55".repeat(16),
            source_job_id.clone(),
            "recommended".to_string(),
            review_sha256.clone(),
            manifest_sha256.clone(),
            Vec::new(),
        );
        let candidate = NativeApplyDispatchRecord {
            state: NativeApplyDispatchState::Accepted,
            generation: 1,
            apply_job_id: "66".repeat(16),
            apply_job_token: "77".repeat(32),
            source_job_id: source_job_id.clone(),
            worker_run_id: "none".to_string(),
            option_id: "recommended".to_string(),
            review_sha256,
            source_manifest_sha256: "none".to_string(),
            manifest_sha256,
            manifest_schema_version: 0,
            target_state: "none".to_string(),
            acknowledgements: Vec::new(),
        };
        let accepted = match store.admit(&request, candidate).unwrap() {
            NativeApplyAdmission::Created(record) => record,
            other => panic!("unexpected admission: {other:?}"),
        };
        let recovery = NativeApplyRecoveryOutcome::Recovered {
            authority: "existing_v4",
            job_id: source_job_id,
            worker_run_id,
            recovery_cleared: true,
            rolled_forward: false,
        };
        assert!(settle_native_apply_store_recovery(&store, &recovery)
            .unwrap_err()
            .contains("before verified mutation authority"));
        assert_eq!(store.read_active().unwrap(), Some(accepted));
        assert_eq!(store.read_terminal().unwrap(), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn async_native_apply_recovery_settles_applying_handle_exactly_once() {
        let root = temp_path("native-apply-recovery-applying");
        let store = NativeApplyCoordinatorStore::new(&root);
        let source_job_id = "11".repeat(16);
        let worker_run_id = "22".repeat(16);
        let review_sha256 = "33".repeat(32);
        let manifest_sha256 = "44".repeat(32);
        let request = NativeApplyControlRequest::start(
            "55".repeat(16),
            source_job_id.clone(),
            "recommended".to_string(),
            review_sha256.clone(),
            manifest_sha256.clone(),
            Vec::new(),
        );
        let candidate = NativeApplyDispatchRecord {
            state: NativeApplyDispatchState::Accepted,
            generation: 1,
            apply_job_id: "66".repeat(16),
            apply_job_token: "77".repeat(32),
            source_job_id: source_job_id.clone(),
            worker_run_id: "none".to_string(),
            option_id: "recommended".to_string(),
            review_sha256,
            source_manifest_sha256: "none".to_string(),
            manifest_sha256: manifest_sha256.clone(),
            manifest_schema_version: 0,
            target_state: "none".to_string(),
            acknowledgements: Vec::new(),
        };
        let accepted = match store.admit(&request, candidate).unwrap() {
            NativeApplyAdmission::Created(record) => record,
            other => panic!("unexpected admission: {other:?}"),
        };
        let validating = store.mark_validating(&accepted).unwrap();
        let applying = store
            .mark_applying(
                &validating,
                NativeApplyVerifiedDispatchIdentity {
                    worker_run_id: worker_run_id.clone(),
                    source_manifest_sha256: manifest_sha256,
                    manifest_schema_version: 4,
                    target_state: "existing_managed".to_string(),
                },
            )
            .unwrap();
        assert_eq!(applying.generation, 3);

        let recovery = NativeApplyRecoveryOutcome::Recovered {
            authority: "existing_v4",
            job_id: source_job_id,
            worker_run_id,
            recovery_cleared: true,
            rolled_forward: false,
        };
        settle_native_apply_store_recovery(&store, &recovery).unwrap();
        assert_eq!(store.read_active().unwrap(), None);
        let terminal = store.read_terminal().unwrap().unwrap();
        assert_eq!(terminal.outcome, NativeApplyTerminalOutcome::RolledBack);
        assert!(terminal.recovery_cleared);
        assert!(terminal
            .diagnostic
            .contains("rolled back during coordinator recovery"));

        settle_native_apply_store_recovery(&store, &recovery).unwrap();
        assert_eq!(store.read_terminal().unwrap(), Some(terminal));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn post_recovery_readiness_failure_is_terminal_but_never_successful() {
        let root = temp_path("native-apply-recovery-readiness-failure");
        let store = NativeApplyCoordinatorStore::new(&root);
        let source_job_id = "11".repeat(16);
        let worker_run_id = "22".repeat(16);
        let manifest_sha256 = "44".repeat(32);
        let request = NativeApplyControlRequest::start(
            "55".repeat(16),
            source_job_id.clone(),
            "recommended".to_string(),
            "33".repeat(32),
            manifest_sha256.clone(),
            Vec::new(),
        );
        let candidate = NativeApplyDispatchRecord {
            state: NativeApplyDispatchState::Accepted,
            generation: 1,
            apply_job_id: "66".repeat(16),
            apply_job_token: "77".repeat(32),
            source_job_id: source_job_id.clone(),
            worker_run_id: "none".to_string(),
            option_id: "recommended".to_string(),
            review_sha256: "33".repeat(32),
            source_manifest_sha256: "none".to_string(),
            manifest_sha256: manifest_sha256.clone(),
            manifest_schema_version: 0,
            target_state: "none".to_string(),
            acknowledgements: Vec::new(),
        };
        let accepted = match store.admit(&request, candidate).unwrap() {
            NativeApplyAdmission::Created(record) => record,
            other => panic!("unexpected admission: {other:?}"),
        };
        let validating = store.mark_validating(&accepted).unwrap();
        let applying = store
            .mark_applying(
                &validating,
                NativeApplyVerifiedDispatchIdentity {
                    worker_run_id: worker_run_id.clone(),
                    source_manifest_sha256: manifest_sha256,
                    manifest_schema_version: 5,
                    target_state: "existing_managed".to_string(),
                },
            )
            .unwrap();
        let recovery = NativeApplyRecoveryOutcome::Recovered {
            authority: "existing_v4",
            job_id: source_job_id,
            worker_run_id,
            recovery_cleared: true,
            rolled_forward: false,
        };

        settle_native_apply_store_readiness_failure(
            &store,
            &recovery,
            "controller primary is still WAITING_OPERATION",
        )
        .unwrap();

        assert_eq!(store.read_active().unwrap(), None);
        let terminal = store.read_terminal().unwrap().unwrap();
        assert_eq!(terminal.dispatch, applying);
        assert_eq!(terminal.outcome, NativeApplyTerminalOutcome::Failed);
        assert!(terminal.recovery_cleared);
        assert!(terminal.diagnostic.contains("controller readiness failed"));
        assert!(terminal.diagnostic.contains("WAITING_OPERATION"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_apply_mutation_cli_requires_the_exact_private_lab_gate() {
        assert!(!native_apply_lab_mode_allowed(None));
        assert!(!native_apply_lab_mode_allowed(Some("1")));
        assert!(!native_apply_lab_mode_allowed(Some("forced-rollback")));
        assert!(native_apply_lab_mode_allowed(Some(
            NATIVE_APPLY_LAB_FORCED_ROLLBACK
        )));
        assert_eq!(
            native_apply_lab_fault(Some(NATIVE_APPLY_LAB_CRASH_AFTER_SERVICE_RESTARTED)),
            Some(NativeApplyLabFaultInjection::PauseAfterServiceRestarted {
                timeout: NATIVE_APPLY_LAB_CRASH_PAUSE
            })
        );
        assert_eq!(
            native_apply_lab_fault(Some(NATIVE_APPLY_LAB_COMMIT)),
            Some(NativeApplyLabFaultInjection::None)
        );
        assert_eq!(
            native_apply_lab_fault(Some(NATIVE_APPLY_LAB_CRASH_BEFORE_COMMIT)),
            Some(
                NativeApplyLabFaultInjection::PauseAfterVerifiedBeforeCommit {
                    timeout: NATIVE_APPLY_LAB_CRASH_PAUSE
                }
            )
        );
        assert_eq!(
            native_apply_lab_fault(Some(NATIVE_APPLY_LAB_CRASH_AFTER_COMMIT)),
            Some(NativeApplyLabFaultInjection::PauseAfterCommitAccepted {
                timeout: NATIVE_APPLY_LAB_CRASH_PAUSE
            })
        );
        assert!(!native_apply_lab_mode_allowed(Some(
            "forced-rollback-crash-after-service-restarted"
        )));
    }

    #[test]
    fn daemon_rejects_symlink_and_regular_control_path() {
        let target = temp_path("target");
        let link = temp_path("link");
        fs::create_dir(&target).unwrap();
        symlink(&target, &link).unwrap();
        assert!(CalibrationDaemon::bind(&link).is_err());
        fs::remove_file(&link).unwrap();
        fs::remove_dir(&target).unwrap();

        let dir = temp_path("regular");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join(CONTROL_SOCKET_NAME), b"not a socket").unwrap();
        assert!(CalibrationDaemon::bind(&dir).is_err());
        fs::remove_file(dir.join(CONTROL_SOCKET_NAME)).unwrap();
        cleanup_state_dir(&dir);
    }

    #[test]
    fn daemon_returns_bounded_error_for_invalid_control_record() {
        let dir = temp_path("invalid");
        let thread = serve_one(CalibrationDaemon::bind(&dir).unwrap());
        let mut stream = UnixStream::connect(dir.join(CONTROL_SOCKET_NAME)).unwrap();
        stream.write_all(b"not-canonical\n\n").unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(response.contains("\"error_code\":\"invalid-request\""));
        thread.join().unwrap();
        cleanup_state_dir(&dir);
    }

    #[test]
    fn malformed_startup_journal_keeps_coordinator_visible_but_blocks_admission() {
        let dir = temp_path("unsafe-journal");
        let job_dir = dir.join("jobs").join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        fs::create_dir_all(&job_dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(dir.join("jobs"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&job_dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(job_dir.join("request"), b"malformed\n").unwrap();
        fs::write(job_dir.join("state"), b"malformed\n").unwrap();

        let daemon = CalibrationDaemon::bind(&dir).unwrap();
        let thread = serve_one(daemon);
        let response = send_control(&dir, &request(ControlCommand::Summary)).unwrap();
        assert!(response.contains("\"state\":\"recovery_required\""));
        assert!(response.contains("\"admission_enabled\":false"));
        thread.join().unwrap();

        fs::remove_file(job_dir.join("request")).unwrap();
        fs::remove_file(job_dir.join("state")).unwrap();
        fs::remove_dir(job_dir).unwrap();
        cleanup_state_dir(&dir);
    }

    #[test]
    fn unix_peer_credentials_are_exact_and_local() {
        let (left, _right) = UnixStream::pair().unwrap();
        let credentials = peer_credentials(&left).unwrap();
        assert_eq!(credentials.uid, euid());
        assert!(credentials.pid > 0);
    }

    #[test]
    fn queued_admission_is_idempotent_and_keeps_independent_local_leases() {
        let dir = temp_path("queued-admission");
        let first = operation_request('a', 'b', "wan");
        let second = operation_request('c', 'd', "wanb");
        let mut daemon = CalibrationDaemon::bind_with_admission(&dir, true).unwrap();
        daemon.native_autotune = true;

        let started = daemon.handle(&job_message(ControlCommand::Start, &first));
        assert!(started.contains("\"state\":\"queued\""));
        assert!(started.contains("\"idempotent\":false"));
        assert!(started.contains("\"worker_run_id\":null"));
        assert_eq!(daemon.leases.job_count(), 1);

        let repeated = daemon.handle(&job_message(ControlCommand::Start, &first));
        assert!(repeated.contains("\"idempotent\":true"));
        assert_eq!(daemon.leases.job_count(), 1);

        let mut wrong_token = job_message(ControlCommand::Status, &first);
        wrong_token.control.job_token = Some("e".repeat(64));
        assert!(daemon
            .handle(&wrong_token)
            .contains("\"error_code\":\"job-not-found\""));
        assert!(daemon
            .handle(&job_message(ControlCommand::Status, &first))
            .contains("\"state\":\"queued\""));
        daemon.job_errors.insert(
            first.identity.job_id.clone(),
            "launcher: identity \"mismatch\"".to_string(),
        );
        let diagnosed = daemon.handle(&job_message(ControlCommand::Status, &first));
        assert!(diagnosed.contains("\"diagnostic\":\"launcher: identity \\\"mismatch\\\"\""));

        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &second))
            .contains("\"state\":\"queued\""));
        assert_eq!(daemon.leases.job_count(), 2);
        let cancelled = daemon.handle(&job_message(ControlCommand::Cancel, &first));
        assert!(cancelled.contains("\"state\":\"cancelled\""));
        assert_eq!(daemon.leases.job_count(), 1);
        drop(daemon);

        let restarted = CalibrationDaemon::bind_with_admission(&dir, true).unwrap();
        assert_eq!(restarted.leases.job_count(), 1);
        let summary = restarted.summary_response();
        assert!(summary.contains("\"queued_jobs\":1"));
        assert!(summary.contains("\"settled_jobs\":1"));
        assert!(summary.contains(
            "\"active_operations\":[{\"instance\":\"wanb\",\"operation\":\"full_autotune\",\"state\":\"queued\",\"runtime_mutated\":false}]"
        ));
        drop(restarted);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn runtime_preflight_rejects_unsafe_state_and_waits_without_a_retry_timer() {
        for (suffix, attestor, expect_failed) in [
            (
                "unsafe-runtime",
                unsafe_runtime as fn(&OperationRequest) -> RuntimeAttestation,
                true,
            ),
            (
                "waiting-runtime",
                waiting_runtime as fn(&OperationRequest) -> RuntimeAttestation,
                false,
            ),
        ] {
            let root = temp_path(suffix);
            let state_dir = root.join("state");
            fs::create_dir_all(&root).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            let mut daemon =
                CalibrationDaemon::bind_with_components(&state_dir, true, Some(attestor)).unwrap();
            daemon.native_rating = true;
            let request = guided_rating_request('a', 'b', "wan");
            assert!(daemon
                .handle(&job_message(ControlCommand::Start, &request))
                .contains("\"state\":\"queued\""));
            daemon.tick();
            assert!(!daemon.jobs[0].journal.runtime_mutated);
            if expect_failed {
                assert_eq!(
                    daemon.jobs[0].journal.state,
                    super::super::protocol::OperationState::Failed
                );
                assert_eq!(
                    daemon.jobs[0].journal.diagnostic_code.as_deref(),
                    Some("runtime-sqm-mismatch")
                );
                assert_eq!(daemon.leases.job_count(), 0);
            } else {
                assert_eq!(
                    daemon.jobs[0].journal.state,
                    super::super::protocol::OperationState::Queued
                );
                assert_eq!(daemon.leases.job_count(), 1);
                daemon.tick();
                assert_eq!(
                    daemon.jobs[0].journal.state,
                    super::super::protocol::OperationState::Queued
                );
                assert!(!daemon.jobs[0].journal.heavy_lease_acquired);
            }
            drop(daemon);
            fs::remove_dir_all(&root).unwrap();
        }
    }

    #[test]
    fn route_config_mismatch_settles_before_heavy_lease_or_worker_launch() {
        let root = temp_path("route-mismatch");
        let state_dir = root.join("state");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let mut daemon = CalibrationDaemon::bind_with_components(
            &state_dir,
            true,
            Some(mismatched_route_runtime),
        )
        .unwrap();
        daemon.native_rating = true;
        let request = guided_rating_request('a', 'b', "wan");
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &request))
            .contains("\"state\":\"queued\""));

        daemon.tick();

        assert_eq!(
            daemon.jobs[0].journal.state,
            super::super::protocol::OperationState::Failed
        );
        assert_eq!(
            daemon.jobs[0].journal.diagnostic_code.as_deref(),
            Some("runtime-route-config-mismatch")
        );
        assert!(!daemon.jobs[0].journal.heavy_lease_acquired);
        assert!(!daemon.jobs[0].journal.runtime_mutated);
        assert!(daemon.native_children.is_empty());
        assert_eq!(daemon.leases.job_count(), 0);
        drop(daemon);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn heavy_lease_is_journalled_only_after_preflight_and_reconstructed_on_restart() {
        let dir = temp_path("staged-heavy-lease");
        let first = operation_request('a', 'b', "wan");
        let second = operation_request('c', 'd', "wanb");
        let mut daemon = CalibrationDaemon::bind_with_admission(&dir, true).unwrap();
        daemon.native_autotune = true;
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &first))
            .contains("\"state\":\"queued\""));
        assert!(daemon
            .handle(&job_message(ControlCommand::Start, &second))
            .contains("\"state\":\"queued\""));
        assert!(!daemon.jobs[0].journal.heavy_lease_acquired);
        assert!(!daemon.jobs[1].journal.heavy_lease_acquired);

        assert!(daemon.acquire_heavy_lease_if_needed(0));
        assert!(daemon.jobs[0].journal.heavy_lease_acquired);
        assert!(!daemon.acquire_heavy_lease_if_needed(1));
        assert!(!daemon.jobs[1].journal.heavy_lease_acquired);
        assert_eq!(
            daemon
                .leases
                .owner(&super::super::lease::LeaseKey::HeavyTraffic),
            Some(first.identity.job_id.as_str())
        );
        drop(daemon);

        let restarted = CalibrationDaemon::bind_with_admission(&dir, true).unwrap();
        assert_eq!(restarted.leases.job_count(), 2);
        assert_eq!(
            restarted
                .leases
                .owner(&super::super::lease::LeaseKey::HeavyTraffic),
            Some(first.identity.job_id.as_str())
        );
        drop(restarted);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lease_conflict_response_is_typed_and_keeps_debug_identity_out_of_user_text() {
        let owner_job_id = "a".repeat(32);
        let response = lease_acquire_error_response(&LeaseAcquireError::Conflict {
            key: super::super::lease::LeaseKey::Instance("wwan_adaptive".to_string()),
            owner_job_id: owner_job_id.clone(),
        });
        assert_eq!(
            response,
            format!(
                concat!(
                    "{{\"state\":\"error\",\"error_code\":\"lease-conflict\",",
                    "\"error\":\"another operation is already active for this instance\",",
                    "\"conflict_kind\":\"instance\",",
                    "\"conflicting_job_id\":\"{}\"}}\n"
                ),
                owner_job_id,
            )
        );
        assert!(!response.contains("Instance("));
        assert!(!response.contains("wwan_adaptive"));
    }

    #[test]
    fn second_rating_request_is_rejected_without_mutating_the_active_job() {
        let dir = temp_path("rating-instance-conflict");
        let mut daemon = CalibrationDaemon::bind_with_admission(&dir, true).unwrap();
        daemon.native_rating = true;
        let first = guided_rating_request('a', 'b', "wan");
        let second = guided_rating_request('c', 'd', "wan");
        let accepted = daemon.handle(&job_message(ControlCommand::Start, &first));
        assert!(accepted.contains("\"state\":\"queued\""));
        let journal_before = daemon.jobs[0].journal.clone();

        let rejected = daemon.handle(&job_message(ControlCommand::Start, &second));
        assert!(rejected.contains("\"error_code\":\"lease-conflict\""));
        assert!(rejected.contains("\"conflict_kind\":\"instance\""));
        assert!(rejected.contains(&format!(
            "\"conflicting_job_id\":\"{}\"",
            first.identity.job_id
        )));
        assert!(!rejected.contains("Instance("));
        assert!(!rejected.contains("wan"));
        assert_eq!(daemon.jobs.len(), 1);
        assert_eq!(daemon.jobs[0].journal, journal_before);
        assert_eq!(daemon.leases.job_count(), 1);

        drop(daemon);
        fs::remove_dir_all(&dir).unwrap();
    }
}
