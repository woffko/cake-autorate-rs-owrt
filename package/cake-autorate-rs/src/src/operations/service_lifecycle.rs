//! Ordinary OpenWrt service lifecycle authority.
//!
//! rc.common remains responsible only for declaring procd instances and for
//! carrying the historical global lock descriptor across its stop callback.
//! Every decision about presets, SQM, ingress preparation and which controller
//! instances are startable is made here from one typed configuration snapshot.

#[cfg(feature = "calibration")]
use super::coordinator::native_apply_recovery_markers_present;
#[cfg(feature = "calibration")]
use super::event_loop::CalibrationEventLoop;
#[cfg(feature = "calibration")]
use super::identity::ProcessIdentity;
#[cfg(feature = "calibration")]
use super::mqtt_publisher::{cleanup_production_service_plans, publish_production_service_plans};
use super::procd_control::delete_service_or_attest_absent;
use super::process::{run_bounded_command_output_with_input, BoundedCommandOutput, SpawnSpec};
#[cfg(feature = "calibration")]
use super::runtime_health::{json_nonnegative_f64_value, json_string_value};
use super::runtime_health::{safe_interface, safe_name, UciPackage, UciSection};
use super::service_config::{run_sync_presets, InterfaceResolver, OpenWrtEnvironment};
#[cfg(feature = "calibration")]
use super::sqm_projection::plan_projection;
use super::sqm_projection::{apply_sqm_projection, ProjectionScope, SqmProjectionPlan};
#[cfg(test)]
use super::sqm_recovery_openwrt::ManagedSqmRatePolicy;
use super::sqm_recovery_openwrt::{
    attest_managed_sqm_after_service_action_or_offline, error_message as sqm_error_message,
    managed_sqm_rate_policy_from_options, stop_managed_sqm_after_service_action,
    ManagedSqmAttestationSpec, ManagedSqmStopSpec, NativeSqmAttestationError, OpenWrtPaths,
};
use super::sqm_start_events::{SqmStartEvents, StartEvents};
#[cfg(feature = "calibration")]
use super::traffic_classifier::run_traffic_classifier;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
#[cfg(feature = "calibration")]
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};
#[cfg(feature = "calibration")]
use std::time::{SystemTime, UNIX_EPOCH};

const CAKE_PACKAGE: &str = "cake-autorate";
const SQM_PACKAGE: &str = "sqm";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const SQM_START_ATTEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONTROLLER_STOP_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(feature = "calibration")]
const CONTROLLER_START_TIMEOUT: Duration = Duration::from_secs(90);
#[cfg(feature = "calibration")]
const CONTROLLER_STATUS_MAX_AGE: f64 = 15.0;
#[cfg(feature = "calibration")]
const CONTROLLER_STATUS_FUTURE_SKEW: f64 = 2.0;
const MAX_OUTPUT: usize = 64 * 1024;
const MAX_INSTANCES: usize = 64;
const MAX_CMDLINE: u64 = 4096;
const MAX_BRIDGER_CONFIG_BYTES: usize = 256 * 1024;
const SERVICE_NAME: &str = "cake-autorate";
const DAEMON_PATH: &str = "/usr/sbin/cake-autorated";
const SERVICE_START_DEFERRED_V1: &str = "service-start-deferred-v1";
static REPLACE_SEQUENCE: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Debug, PartialEq)]
struct ServiceStartPlan {
    instances: Vec<String>,
    managed: Vec<ManagedSqmAttestationSpec>,
}

#[cfg(feature = "calibration")]
#[derive(Clone, Debug, PartialEq, Eq)]
enum ControllerStartReadiness {
    Ready,
    Waiting(String),
}

#[cfg(feature = "calibration")]
#[derive(Clone, Debug)]
struct ControllerStartAuthority {
    cake: UciPackage,
    sqm: UciPackage,
    instances: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ServiceStopPlan {
    cake: UciPackage,
    sqm: UciPackage,
    managed: Vec<ManagedSqmStopSpec>,
}

#[derive(Clone, Debug)]
struct ServicePaths {
    uci: PathBuf,
    tc: PathBuf,
    ubus: PathBuf,
    proc_root: PathBuf,
    runtime_root: PathBuf,
    runtime_lock_root: PathBuf,
    sqm_init: PathBuf,
    bridger_init: PathBuf,
    bridger_config: PathBuf,
    uci_workspace_root: PathBuf,
}

impl ServicePaths {
    fn production() -> Self {
        Self {
            uci: env_path("CAKE_AUTORATE_UCI_BIN", "/sbin/uci"),
            tc: env_path("CAKE_AUTORATE_TC_BIN", "/sbin/tc"),
            ubus: env_path("CAKE_AUTORATE_UBUS_BIN", "/bin/ubus"),
            proc_root: env_path("CAKE_AUTORATE_PROC_ROOT", "/proc"),
            runtime_root: env_path("CAKE_AUTORATE_RUN_ROOT", "/var/run/cake-autorate"),
            runtime_lock_root: env_path(
                "CAKE_AUTORATE_RUNTIME_LOCK_ROOT",
                "/tmp/cake-autorate-speedtest",
            ),
            sqm_init: env_path("CAKE_AUTORATE_SQM_INIT", "/etc/init.d/sqm"),
            bridger_init: env_path("CAKE_AUTORATE_BRIDGER_INIT", "/etc/init.d/bridger"),
            bridger_config: env_path("CAKE_AUTORATE_BRIDGER_CONFIG", "/etc/config/bridger"),
            uci_workspace_root: env_path(
                "CAKE_AUTORATE_SERVICE_UCI_WORK_ROOT",
                "/tmp/cake-autorate-service-uci",
            ),
        }
    }
}

enum ServiceGlobalLock {
    Borrowed { _guard: File },
    Owned(File),
}

impl Drop for ServiceGlobalLock {
    fn drop(&mut self) {
        if let Self::Owned(file) = self {
            unsafe {
                libc::flock(file.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ServiceProcessKind {
    Controller,
    #[cfg(feature = "calibration")]
    MqttPublisher,
}

impl ServiceProcessKind {
    fn argument(self) -> &'static str {
        match self {
            Self::Controller => "--instance",
            #[cfg(feature = "calibration")]
            Self::MqttPublisher => "--mqtt-publisher",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Controller => "controller",
            #[cfg(feature = "calibration")]
            Self::MqttPublisher => "MQTT publisher",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ControllerIdentity {
    kind: ServiceProcessKind,
    instance: String,
    pid: u32,
    process_group: u32,
    starttime_ticks: u64,
}

struct ControllerPidfd {
    identity: ControllerIdentity,
    pidfd: OwnedFd,
}

trait ServiceStopBackend {
    type Controllers;

    fn snapshot(&mut self) -> Result<ServiceStopPlan, String>;
    fn attest_unchanged(&mut self, plan: &ServiceStopPlan) -> Result<(), String>;
    fn capture_controllers(&mut self) -> Result<Self::Controllers, String>;
    fn disable_service(&mut self) -> Result<(), String>;
    fn wait_controllers(&mut self, controllers: Self::Controllers) -> Result<(), String>;
    fn attest_no_controllers(&mut self) -> Result<(), String>;
    fn clear_classifier(&mut self);
    fn stop_sqm(&mut self, spec: &ManagedSqmStopSpec) -> Result<(), String>;
    fn cleanup_runtime(&mut self) -> Result<(), String>;
    fn cleanup_sidecars(&mut self) -> Result<(), String>;
}

struct OpenWrtServiceStop {
    environment: OpenWrtEnvironment,
    paths: ServicePaths,
}

fn execute_stop(backend: &mut impl ServiceStopBackend) -> Result<(), String> {
    let plan = backend.snapshot()?;
    backend.attest_unchanged(&plan)?;
    let controllers = backend.capture_controllers()?;
    backend.attest_unchanged(&plan)?;
    backend.disable_service()?;
    backend.wait_controllers(controllers)?;
    backend.attest_no_controllers()?;
    backend.attest_unchanged(&plan)?;
    backend.clear_classifier();
    backend.attest_unchanged(&plan)?;
    for spec in &plan.managed {
        backend.stop_sqm(spec)?;
        backend.attest_unchanged(&plan)?;
    }
    backend.attest_no_controllers()?;
    backend.cleanup_runtime()?;
    backend.attest_no_controllers()?;
    backend.cleanup_sidecars()?;
    backend.attest_no_controllers()?;
    backend.attest_unchanged(&plan)
}

#[derive(Debug, PartialEq, Eq)]
struct ExactFileSnapshot {
    bytes: Vec<u8>,
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    gid: u32,
}

struct BridgerWorkspace {
    root: PathBuf,
    config_dir: PathBuf,
    override_dir: PathBuf,
    savedir: PathBuf,
    alias: String,
}

impl BridgerWorkspace {
    fn create(base: &Path) -> Result<Self, String> {
        ensure_owner_directory(base)?;
        for slot in 0..16_u8 {
            let root = base.join(format!("{}.{}", std::process::id(), slot));
            match fs::create_dir(&root) {
                Ok(()) => {
                    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
                        .map_err(|error| format!("unable to secure bridger workspace: {error}"))?;
                    let config_dir = root.join("config");
                    let override_dir = root.join("override");
                    let savedir = root.join("savedir");
                    for directory in [&config_dir, &override_dir, &savedir] {
                        fs::create_dir(directory).map_err(|error| {
                            format!("unable to create bridger workspace: {error}")
                        })?;
                        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(
                            |error| format!("unable to secure bridger workspace: {error}"),
                        )?;
                    }
                    return Ok(Self {
                        root,
                        config_dir,
                        override_dir,
                        savedir,
                        alias: format!("cake_bridger_{}_{}", std::process::id(), slot),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!("unable to create bridger workspace: {error}"));
                }
            }
        }
        Err("unable to allocate an isolated bridger workspace".to_string())
    }

    fn package_path(&self) -> PathBuf {
        self.config_dir.join(&self.alias)
    }

    fn arguments(&self, arguments: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
        let mut result = vec![
            OsString::from("-c"),
            self.config_dir.as_os_str().to_os_string(),
            OsString::from("-C"),
            self.override_dir.as_os_str().to_os_string(),
            OsString::from("-t"),
            self.savedir.as_os_str().to_os_string(),
            OsString::from("-q"),
        ];
        result.extend(arguments);
        result
    }
}

impl Drop for BridgerWorkspace {
    fn drop(&mut self) {
        if owner_directory_is_exact(&self.root) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

pub(crate) fn run_service_lifecycle<I>(mut arguments: I) -> Result<String, String>
where
    I: Iterator<Item = String>,
{
    let command = arguments
        .next()
        .ok_or_else(|| "service-lifecycle requires a command".to_string())?;
    if arguments.next().is_some() {
        return Err("service-lifecycle received unexpected arguments".to_string());
    }
    match command.as_str() {
        "prepare-start" => prepare_start(),
        #[cfg(feature = "calibration")]
        "confirm-started" => confirm_started_openwrt(),
        "execute-stop" => execute_stop_openwrt(),
        _ => Err("service-lifecycle command is unsupported".to_string()),
    }
}

#[cfg(feature = "calibration")]
fn confirm_started_openwrt() -> Result<String, String> {
    confirm_controller_service_started()?;
    Ok("service-started-v1 ready\n".to_string())
}

#[cfg(feature = "calibration")]
pub(crate) fn confirm_controller_service_started() -> Result<(), String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("service lifecycle requires root".to_string());
    }
    let paths = ServicePaths::production();
    let environment = OpenWrtEnvironment::production();
    let authority = controller_start_authority(&environment)?;
    let deadline = Instant::now()
        .checked_add(CONTROLLER_START_TIMEOUT)
        .ok_or_else(|| "controller readiness deadline overflowed".to_string())?;
    let mut events = CalibrationEventLoop::new()?;

    loop {
        arm_controller_start_events(&mut events, &paths)?;
        let last_waiting =
            match observe_controller_start(&paths, &authority.instances, epoch_seconds())? {
                ControllerStartReadiness::Ready => {
                    let lock = acquire_service_global_lock(&paths)?;
                    let cake = environment.read_package(CAKE_PACKAGE)?;
                    let sqm = environment.read_package(SQM_PACKAGE)?;
                    if cake != authority.cake || sqm != authority.sqm {
                        return Err(
                            "controller service configuration changed during readiness".to_string()
                        );
                    }
                    match observe_controller_start(&paths, &authority.instances, epoch_seconds())? {
                        ControllerStartReadiness::Ready => {
                            drop(lock);
                            return Ok(());
                        }
                        ControllerStartReadiness::Waiting(reason) => {
                            drop(lock);
                            reason
                        }
                    }
                }
                ControllerStartReadiness::Waiting(reason) => reason,
            };

        // Re-arm and re-observe before sleeping to close directory-creation
        // and atomic status-replacement races. The deadline only bounds a
        // failed startup; all ordinary progress is driven by filesystem events.
        arm_controller_start_events(&mut events, &paths)?;
        if matches!(
            observe_controller_start(&paths, &authority.instances, epoch_seconds())?,
            ControllerStartReadiness::Ready
        ) {
            continue;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "controller service did not become ready: {last_waiting}"
            ));
        }
        let readiness = events.wait(-1, Some(remaining))?;
        if readiness.signal {
            return Err("controller service readiness was interrupted".to_string());
        }
        if readiness.deadline {
            return Err(format!(
                "controller service did not become ready: {last_waiting}"
            ));
        }
    }
}

#[cfg(feature = "calibration")]
fn controller_start_authority(
    environment: &OpenWrtEnvironment,
) -> Result<ControllerStartAuthority, String> {
    let cake = environment.read_package(CAKE_PACKAGE)?;
    let sqm = environment.read_package(SQM_PACKAGE)?;
    let mut projected_sqm = sqm.clone();
    let projection = plan_projection(
        &cake,
        &mut projected_sqm,
        environment,
        &ProjectionScope::All,
    )?;
    if projected_sqm != sqm {
        return Err("controller readiness found a pending SQM projection".to_string());
    }
    let plan = plan_start(&cake, &sqm, environment, &projection)?;
    Ok(ControllerStartAuthority {
        cake,
        sqm,
        instances: plan.instances,
    })
}

#[cfg(feature = "calibration")]
fn arm_controller_start_events(
    events: &mut CalibrationEventLoop,
    paths: &ServicePaths,
) -> Result<(), String> {
    let parent = paths
        .runtime_root
        .parent()
        .ok_or_else(|| "controller runtime root has no parent".to_string())?;
    let name = paths
        .runtime_root
        .file_name()
        .ok_or_else(|| "controller runtime root has no name".to_string())?;
    events.watch_named_entry(parent, name.as_bytes())?;
    events.watch_tree(&paths.runtime_root, 2, false)?;
    let processes = discover_controllers_allowing_duplicates(&paths.proc_root)?
        .into_iter()
        .map(|controller| ProcessIdentity {
            pid: controller.pid,
            process_group: controller.process_group,
            starttime_ticks: controller.starttime_ticks,
        })
        .collect::<Vec<_>>();
    events
        .refresh_processes(&processes, &paths.proc_root)
        .map(|_| ())
}

#[cfg(feature = "calibration")]
fn observe_controller_start(
    paths: &ServicePaths,
    expected: &[String],
    now_epoch: f64,
) -> Result<ControllerStartReadiness, String> {
    let controllers = discover_controllers_allowing_duplicates(&paths.proc_root)?;
    let actual = controllers
        .iter()
        .filter(|controller| controller.kind == ServiceProcessKind::Controller)
        .collect::<Vec<_>>();
    for controller in &actual {
        if !expected.contains(&controller.instance) {
            return Err(format!(
                "unexpected controller process is running for {}",
                controller.instance
            ));
        }
    }
    if actual.len() != expected.len() {
        return Ok(ControllerStartReadiness::Waiting(format!(
            "expected {} controllers but observed {}",
            expected.len(),
            actual.len()
        )));
    }

    let boot_epoch = proc_boot_epoch(&paths.proc_root)?;
    let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks_per_second <= 0 {
        return Err("unable to determine controller clock tick rate".to_string());
    }
    for instance in expected {
        let controller = actual
            .iter()
            .find(|controller| &controller.instance == instance)
            .ok_or_else(|| format!("controller process disappeared for {instance}"))?;
        let status_path = paths.runtime_root.join(instance).join("status.json");
        let Some(status) = read_exact_file(&status_path)? else {
            return Ok(ControllerStartReadiness::Waiting(format!(
                "controller status is missing for {instance}"
            )));
        };
        if status.uid != unsafe { libc::geteuid() } || status.mode & 0o022 != 0 {
            return Err(format!(
                "controller status ownership is unsafe for {instance}"
            ));
        }
        let json = std::str::from_utf8(&status.bytes)
            .map_err(|_| format!("controller status is not UTF-8 for {instance}"))?;
        if json_string_value(json, "instance").as_deref() != Some(instance) {
            return Err(format!(
                "controller status identity is invalid for {instance}"
            ));
        }
        let state = json_string_value(json, "state")
            .ok_or_else(|| format!("controller status state is missing for {instance}"))?;
        let started_at = json_nonnegative_f64_value(json, "started_at")
            .ok_or_else(|| format!("controller start timestamp is invalid for {instance}"))?;
        let updated_at = json_nonnegative_f64_value(json, "updated_at")
            .ok_or_else(|| format!("controller update timestamp is invalid for {instance}"))?;
        let process_started_at =
            boot_epoch + controller.starttime_ticks as f64 / ticks_per_second as f64;
        if started_at + CONTROLLER_STATUS_FUTURE_SKEW < process_started_at {
            return Ok(ControllerStartReadiness::Waiting(format!(
                "controller status belongs to a prior process for {instance}"
            )));
        }
        if started_at > now_epoch + CONTROLLER_STATUS_FUTURE_SKEW
            || updated_at < started_at
            || updated_at > now_epoch + CONTROLLER_STATUS_FUTURE_SKEW
        {
            return Err(format!("controller status clock is invalid for {instance}"));
        }
        if now_epoch - updated_at > CONTROLLER_STATUS_MAX_AGE {
            return Ok(ControllerStartReadiness::Waiting(format!(
                "controller status is stale for {instance}"
            )));
        }
        match state.as_str() {
            "WAITING_OPERATION" | "RECOVERING" | "STOPPING" => {
                return Ok(ControllerStartReadiness::Waiting(format!(
                    "controller {instance} is still {state}"
                )))
            }
            "WAITING_LINK"
            | "WAITING_SQM"
            | "WAITING_EXTERNAL_SQM"
            | "RUNNING"
            | "IDLE"
            | "STALL"
            | "LEARNING"
            | "ACTIVE"
            | "STANDBY"
            | "OFFLINE" => {}
            "ERROR" => {
                return Err(format!(
                    "controller {instance} entered ERROR during startup"
                ))
            }
            _ => {
                return Err(format!(
                    "controller {instance} published unknown state {state}"
                ))
            }
        }
    }
    Ok(ControllerStartReadiness::Ready)
}

#[cfg(feature = "calibration")]
fn proc_boot_epoch(proc_root: &Path) -> Result<f64, String> {
    let bytes = fs::read(proc_root.join("stat"))
        .map_err(|error| format!("unable to read controller boot time: {error}"))?;
    if bytes.len() > MAX_BRIDGER_CONFIG_BYTES || bytes.contains(&0) {
        return Err("controller boot time input exceeds its bound".to_string());
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| "controller boot time input is not UTF-8".to_string())?;
    let values = text
        .lines()
        .filter_map(|line| line.strip_prefix("btime "))
        .collect::<Vec<_>>();
    if values.len() != 1 {
        return Err("controller boot time is missing or ambiguous".to_string());
    }
    values[0]
        .parse::<u64>()
        .map(|value| value as f64)
        .map_err(|_| "controller boot time is invalid".to_string())
}

#[cfg(feature = "calibration")]
fn epoch_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

fn execute_stop_openwrt() -> Result<String, String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("service lifecycle requires root".to_string());
    }
    let paths = ServicePaths::production();
    let _lock = acquire_service_global_lock(&paths)?;
    let mut backend = OpenWrtServiceStop {
        environment: OpenWrtEnvironment::production(),
        paths,
    };
    execute_stop(&mut backend)?;
    Ok("service-stop-v1 ok\n".to_string())
}

fn prepare_start() -> Result<String, String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("service lifecycle requires root".to_string());
    }
    // OpenWrt's default_postinst invokes service start before the package's
    // own guarded restart hook. A distinct typed deferral prevents both
    // topology mutation and procd registration without conflating package
    // replacement with a genuine empty controller plan; the later
    // PKG_UPGRADE=0 restart owns the complete replacement lifecycle.
    if package_upgrade_mode()? {
        return Ok(format!("{SERVICE_START_DEFERRED_V1}\n"));
    }
    // The start path mutates both UCI packages and the live SQM topology.
    // Acquire the same authority as stop before presets or projection can
    // commit anything. Full rc.common deliberately lends its already-held
    // descriptor; Lite has no calibration peer and acquires it here.
    let paths = ServicePaths::production();
    let _lock = acquire_service_global_lock(&paths)?;
    #[cfg(feature = "calibration")]
    {
        let (existing, bootstrap) = native_apply_recovery_markers_present()?;
        require_no_native_apply_recovery(existing, bootstrap)?;
    }
    require_no_staged_uci(&paths, CAKE_PACKAGE)?;
    require_no_staged_uci(&paths, SQM_PACKAGE)?;
    let environment = OpenWrtEnvironment::production();

    run_sync_presets(std::iter::empty())?;
    let projection = apply_sqm_projection(ProjectionScope::All)?;
    require_no_staged_uci(&paths, CAKE_PACKAGE)?;
    require_no_staged_uci(&paths, SQM_PACKAGE)?;
    let cake = environment.read_package(CAKE_PACKAGE)?;
    let sqm = environment.read_package(SQM_PACKAGE)?;
    let plan = plan_start(&cake, &sqm, &environment, &projection)?;
    attest_start_configuration(&environment, &cake, &sqm, &projection, &plan)?;
    sync_bridger_blacklist(&paths, projection.ingress_interfaces())?;
    attest_start_configuration(&environment, &cake, &sqm, &projection, &plan)?;
    for interface in projection.ingress_interfaces() {
        remove_empty_clsact(&paths, interface)?;
        attest_start_configuration(&environment, &cake, &sqm, &projection, &plan)?;
    }
    start_sqm_backend(&paths, projection.is_managed(), &plan.managed, || {
        require_no_staged_uci(&paths, CAKE_PACKAGE)?;
        require_no_staged_uci(&paths, SQM_PACKAGE)?;
        attest_start_configuration(&environment, &cake, &sqm, &projection, &plan)
    })?;
    attest_start_configuration(&environment, &cake, &sqm, &projection, &plan)?;

    #[cfg(feature = "calibration")]
    if let Err(error) = run_traffic_classifier(["apply".to_string()].into_iter()) {
        eprintln!("WARNING: native traffic classifier is degraded: {error}");
    }

    attest_start_configuration(&environment, &cake, &sqm, &projection, &plan)?;
    #[cfg(feature = "calibration")]
    let mqtt_instances = publish_production_service_plans(&cake)?;
    #[cfg(not(feature = "calibration"))]
    let mqtt_instances = Vec::new();
    attest_start_configuration(&environment, &cake, &sqm, &projection, &plan)?;
    Ok(encode_start_plan(&plan.instances, &mqtt_instances))
}

fn package_upgrade_mode() -> Result<bool, String> {
    package_upgrade_mode_value(std::env::var_os("PKG_UPGRADE").as_deref())
}

fn package_upgrade_mode_value(value: Option<&std::ffi::OsStr>) -> Result<bool, String> {
    match value {
        None => Ok(false),
        Some(value) if value.is_empty() || value == "0" => Ok(false),
        Some(value) if value == "1" => Ok(true),
        Some(_) => Err("PKG_UPGRADE must be empty, 0, or 1".to_string()),
    }
}

#[cfg(feature = "calibration")]
fn require_no_native_apply_recovery(existing: bool, bootstrap: bool) -> Result<(), String> {
    if existing || bootstrap {
        return Err(
            "service start refuses a pending native Apply recovery transaction".to_string(),
        );
    }
    Ok(())
}

fn attest_start_configuration(
    environment: &OpenWrtEnvironment,
    expected_cake: &UciPackage,
    expected_sqm: &UciPackage,
    projection: &SqmProjectionPlan,
    expected_plan: &ServiceStartPlan,
) -> Result<(), String> {
    let cake = environment.read_package(CAKE_PACKAGE)?;
    let sqm = environment.read_package(SQM_PACKAGE)?;
    if expected_cake != &cake || expected_sqm != &sqm {
        return Err("service lifecycle configuration changed during start preparation".to_string());
    }
    let plan = plan_start(&cake, &sqm, environment, projection)?;
    if expected_plan != &plan {
        return Err("service lifecycle start plan changed during preparation".to_string());
    }
    Ok(())
}

fn plan_start(
    cake: &UciPackage,
    sqm: &UciPackage,
    resolver: &impl InterfaceResolver,
    projection: &SqmProjectionPlan,
) -> Result<ServiceStartPlan, String> {
    let mut instances = Vec::new();
    let mut managed = Vec::new();
    for (name, section) in &cake.sections {
        if section.section_type != "cake_autorate" || !bool_option(section, "enabled", false) {
            continue;
        }
        if projection.conflicts().contains(name) {
            eprintln!("WARNING: not starting {name}: duplicate managed SQM target");
            continue;
        }
        if !bool_option(section, "manage_sqm", true) {
            push_instance(&mut instances, name)?;
            continue;
        }
        if !bool_option(section, "sqm_enabled", false) {
            eprintln!("WARNING: not starting {name}: managed SQM is disabled");
            continue;
        }
        let direction = option(section, "sqm_direction_mode")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "both".to_string());
        if direction == "off" {
            eprintln!("WARNING: not starting {name}: managed SQM direction is off");
            continue;
        }
        if !matches!(direction.as_str(), "both" | "download_only" | "upload_only") {
            return Err(format!(
                "service instance {name} has an invalid SQM direction"
            ));
        }
        let target_name = first_option(section, &["sqm_interface", "ul_if", "wan_if"])
            .ok_or_else(|| format!("service instance {name} has no SQM target"))?;
        let target = resolver.resolve(&target_name)?;
        let configured_queue = option(section, "sqm_section").filter(|value| !value.is_empty());
        let Some((queue_name, queue)) =
            find_backing_queue(sqm, configured_queue.as_deref(), &target, resolver)?
        else {
            eprintln!("WARNING: not starting {name}: no enabled SQM backing queue");
            continue;
        };
        if option(queue, "_cake_autorate_managed").as_deref() != Some(name.as_str()) {
            eprintln!("WARNING: not starting {name}: SQM owner is not exact");
            continue;
        }
        let download = parse_rate(queue, "download")?;
        let upload = parse_rate(queue, "upload")?;
        if (direction != "upload_only" && download == 0)
            || (direction == "upload_only" && download != 0)
            || (direction != "download_only" && upload == 0)
            || (direction == "download_only" && upload != 0)
        {
            return Err(format!(
                "service instance {name} SQM rates do not match its direction"
            ));
        }
        let download_interface = option(section, "dl_if")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("ifb4{target}"));
        if !safe_interface(&download_interface) {
            return Err(format!(
                "service instance {name} download interface is unsafe"
            ));
        }
        managed.push(ManagedSqmAttestationSpec {
            instance: name.clone(),
            sqm_section: queue_name,
            target_interface: target.clone(),
            upload_interface: target,
            download_interface,
            direction_mode: direction,
            minimum_download_kbps: download,
            maximum_download_kbps: download,
            minimum_upload_kbps: upload,
            maximum_upload_kbps: upload,
        });
        push_instance(&mut instances, name)?;
    }
    Ok(ServiceStartPlan { instances, managed })
}

fn plan_stop(
    cake: &UciPackage,
    sqm: &UciPackage,
    resolver: &impl InterfaceResolver,
) -> Result<ServiceStopPlan, String> {
    let mut managed = Vec::new();
    let mut targets = BTreeMap::new();
    for (section_name, queue) in &sqm.sections {
        if queue.section_type != "queue" {
            continue;
        }
        let Some(owner) = option(queue, "_cake_autorate_managed").filter(|value| !value.is_empty())
        else {
            continue;
        };
        if !safe_name(&owner) || !safe_name(section_name) {
            return Err("managed SQM stop ownership is unsafe".to_string());
        }
        let target_name = option(queue, "interface")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("managed SQM section {section_name} has no target"))?;
        let target = resolver.resolve(&target_name)?;
        if !safe_interface(&target) {
            return Err(format!(
                "managed SQM section {section_name} resolved to an unsafe target"
            ));
        }
        if let Some(previous) = targets.insert(target.clone(), section_name.clone()) {
            return Err(format!(
                "managed SQM sections {previous} and {section_name} share stop target {target}"
            ));
        }
        let controller = match cake.sections.get(&owner) {
            Some(section) if section.section_type != "cake_autorate" => {
                return Err(format!(
                    "managed SQM owner {owner} is not a cake_autorate section"
                ));
            }
            value => value,
        };
        let rate_policy = controller
            .map(|section| managed_sqm_rate_policy_from_options(&section.options))
            .transpose()
            .map_err(|error| {
                format!(
                    "managed SQM owner {owner} has an invalid rate policy: {}",
                    sqm_error_message(&error)
                )
            })?;
        let download_interface = controller
            .and_then(|section| option(section, "dl_if"))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("ifb4{target}"));
        if !safe_interface(&download_interface) {
            return Err(format!(
                "managed SQM section {section_name} has an unsafe download interface"
            ));
        }
        if managed.len() >= MAX_INSTANCES {
            return Err("service lifecycle exceeds its managed-SQM stop bound".to_string());
        }
        managed.push(ManagedSqmStopSpec {
            instance: owner,
            sqm_section: section_name.clone(),
            target_interface: target,
            download_interface,
            rate_policy,
        });
    }
    Ok(ServiceStopPlan {
        cake: cake.clone(),
        sqm: sqm.clone(),
        managed,
    })
}

fn attest_stop_configuration(
    environment: &OpenWrtEnvironment,
    expected: &ServiceStopPlan,
) -> Result<(), String> {
    let cake = environment.read_package(CAKE_PACKAGE)?;
    let sqm = environment.read_package(SQM_PACKAGE)?;
    if cake != expected.cake || sqm != expected.sqm {
        return Err("service lifecycle configuration changed during stop".to_string());
    }
    if plan_stop(&cake, &sqm, environment)? != *expected {
        return Err("service lifecycle stop plan changed during mutation".to_string());
    }
    Ok(())
}

fn find_backing_queue<'a>(
    sqm: &'a UciPackage,
    configured: Option<&str>,
    expected_target: &str,
    resolver: &impl InterfaceResolver,
) -> Result<Option<(String, &'a UciSection)>, String> {
    if let Some(name) = configured {
        let Some(queue) = sqm.sections.get(name) else {
            return Ok(None);
        };
        return Ok(
            queue_matches(queue, expected_target, resolver)?.then(|| (name.to_string(), queue))
        );
    }
    let mut match_found = None;
    for (name, queue) in &sqm.sections {
        if !queue_matches(queue, expected_target, resolver)? {
            continue;
        }
        if match_found.replace((name.clone(), queue)).is_some() {
            return Err(format!(
                "multiple enabled SQM queues match managed target {expected_target}"
            ));
        }
    }
    Ok(match_found)
}

fn queue_matches(
    queue: &UciSection,
    expected_target: &str,
    resolver: &impl InterfaceResolver,
) -> Result<bool, String> {
    if queue.section_type != "queue" || option(queue, "enabled").as_deref() != Some("1") {
        return Ok(false);
    }
    let Some(interface) = option(queue, "interface").filter(|value| !value.is_empty()) else {
        return Ok(false);
    };
    Ok(resolver.resolve(&interface)? == expected_target)
}

fn push_instance(instances: &mut Vec<String>, name: &str) -> Result<(), String> {
    if instances.len() >= MAX_INSTANCES {
        return Err("service lifecycle exceeds its instance-count bound".to_string());
    }
    if instances.iter().any(|value| value == name) {
        return Err("service lifecycle produced a duplicate instance".to_string());
    }
    instances.push(name.to_string());
    Ok(())
}

fn encode_start_plan(instances: &[String], mqtt_instances: &[String]) -> String {
    let controllers = if instances.is_empty() {
        "-".to_string()
    } else {
        instances.join(",")
    };
    #[cfg(feature = "calibration")]
    {
        let mqtt = if mqtt_instances.is_empty() {
            "-".to_string()
        } else {
            mqtt_instances.join(",")
        };
        format!("service-start-v2 {controllers} {mqtt}\n")
    }
    #[cfg(not(feature = "calibration"))]
    {
        let _ = mqtt_instances;
        format!("service-start-v1 {controllers}\n")
    }
}

fn sync_bridger_blacklist(
    paths: &ServicePaths,
    interfaces: &BTreeSet<String>,
) -> Result<(), String> {
    if interfaces.is_empty() || !executable(&paths.bridger_init) {
        return Ok(());
    }
    let Some(original) = read_exact_file(&paths.bridger_config)? else {
        return Ok(());
    };
    let workspace = BridgerWorkspace::create(&paths.uci_workspace_root)?;
    write_new_file(
        &workspace.package_path(),
        &original.bytes,
        0o600,
        unsafe { libc::geteuid() },
        unsafe { libc::getegid() },
    )?;
    let defaults_target = format!("{}.@defaults[0]", workspace.alias);
    let defaults = command_owned(
        &paths.uci,
        workspace.arguments([
            OsString::from("get"),
            OsString::from(defaults_target.clone()),
        ]),
        None,
    )?;
    if !defaults.status.success() {
        return Ok(());
    }
    let blacklist_target = format!("{}.@defaults[0].blacklist", workspace.alias);
    let current_output = command_owned(
        &paths.uci,
        workspace.arguments([
            OsString::from("get"),
            OsString::from(blacklist_target.clone()),
        ]),
        None,
    )?;
    let current_text = if current_output.status.success() {
        String::from_utf8(current_output.stdout)
            .map_err(|_| "bridger blacklist output is not UTF-8".to_string())?
    } else {
        String::new()
    };
    let current = current_text
        .split_ascii_whitespace()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    if current.iter().any(|value| !safe_interface(value)) {
        return Err("bridger blacklist contains an unsafe interface".to_string());
    }
    let missing = interfaces.difference(&current).cloned().collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    let batch = bridger_batch(&workspace.alias, &missing);
    let applied = command_owned(
        &paths.uci,
        workspace.arguments([OsString::from("batch")]),
        Some(batch.as_bytes()),
    )?;
    require_success(&applied, "update the bridger blacklist")?;
    let final_output = command_owned(
        &paths.uci,
        workspace.arguments([OsString::from("get"), OsString::from(blacklist_target)]),
        None,
    )?;
    require_success(&final_output, "verify the bridger blacklist")?;
    let final_text = String::from_utf8(final_output.stdout)
        .map_err(|_| "bridger blacklist verification is not UTF-8".to_string())?;
    let final_values = final_text.split_ascii_whitespace().collect::<BTreeSet<_>>();
    if !interfaces
        .iter()
        .all(|value| final_values.contains(value.as_str()))
    {
        return Err("bridger blacklist update did not reach its exact postcondition".to_string());
    }
    let candidate = fs::read(workspace.package_path())
        .map_err(|error| format!("unable to read bridger candidate: {error}"))?;
    if candidate.len() > MAX_BRIDGER_CONFIG_BYTES {
        return Err("bridger candidate exceeds its byte bound".to_string());
    }
    replace_exact_file(&paths.bridger_config, &original, &candidate)?;
    let reload = command(paths, &paths.bridger_init, &["reload"], None)?;
    require_success(
        &reload,
        "reload bridger after its isolated configuration update",
    )
}

fn bridger_batch(alias: &str, missing: &[String]) -> String {
    let mut batch = String::new();
    for interface in missing {
        batch.push_str("add_list ");
        batch.push_str(alias);
        batch.push_str(".@defaults[0].blacklist='");
        batch.push_str(interface);
        batch.push_str("'\n");
    }
    batch.push_str("commit ");
    batch.push_str(alias);
    batch.push('\n');
    batch
}

fn remove_empty_clsact(paths: &ServicePaths, interface: &str) -> Result<(), String> {
    let qdiscs = command(paths, &paths.tc, &["qdisc", "show", "dev", interface], None)?;
    if !qdiscs.status.success() {
        return Ok(());
    }
    let qdiscs =
        String::from_utf8(qdiscs.stdout).map_err(|_| "tc qdisc output is not UTF-8".to_string())?;
    if !qdiscs.lines().any(|line| {
        let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
        fields.first() == Some(&"qdisc") && fields.get(1) == Some(&"clsact")
    }) {
        return Ok(());
    }
    for hook in ["ingress", "egress"] {
        let filters = command(
            paths,
            &paths.tc,
            &["filter", "show", "dev", interface, hook],
            None,
        )?;
        require_success(&filters, "inspect clsact filters")?;
        if !filters.stdout.iter().all(u8::is_ascii_whitespace) {
            return Err(format!(
                "cannot replace clsact on SQM interface {interface}: foreign filters remain"
            ));
        }
    }
    let removed = command(
        paths,
        &paths.tc,
        &["qdisc", "del", "dev", interface, "clsact"],
        None,
    )?;
    require_success(&removed, "remove an empty SQM clsact")
}

fn start_sqm_backend(
    paths: &ServicePaths,
    managed: bool,
    specs: &[ManagedSqmAttestationSpec],
    mut attest_configuration: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    if !managed {
        return Ok(());
    }
    if !executable(&paths.sqm_init) {
        return Err("managed SQM is configured but its init script is unavailable".to_string());
    }
    let sqm_paths = OpenWrtPaths::from_environment();
    let config_parent = sqm_paths
        .sqm_config
        .parent()
        .ok_or_else(|| "SQM configuration has no parent directory".to_string())?;
    // Subscribe before the single service action sequence. Upstream iface
    // hotplug can independently stop/start SQM despite our global lock.
    let mut events = SqmStartEvents::subscribe(vec![
        sqm_paths.sqm_state_root.clone(),
        config_parent.to_path_buf(),
    ])?;
    attest_configuration()?;
    let _ = command(paths, &paths.sqm_init, &["enable"], None)?;
    let restart = command(paths, &paths.sqm_init, &["restart"], None)?;
    if !restart.status.success() {
        let _ = command(paths, &paths.sqm_init, &["start"], None)?;
    }
    let offline_instances = await_sqm_start(
        &mut events,
        Instant::now() + SQM_START_ATTEST_TIMEOUT,
        specs,
        attest_configuration,
        attest_managed_sqm_after_service_action_or_offline,
    )?;
    for (spec, offline) in specs.iter().zip(offline_instances) {
        if offline {
            eprintln!(
                "WARNING: managed SQM start for {} is deferred until its target interface returns",
                spec.instance
            );
        }
    }
    Ok(())
}

pub(super) fn await_sqm_start(
    events: &mut impl StartEvents,
    deadline: Instant,
    specs: &[ManagedSqmAttestationSpec],
    mut attest_configuration: impl FnMut() -> Result<(), String>,
    mut attest_runtime: impl FnMut(
        &ManagedSqmAttestationSpec,
    ) -> Result<bool, NativeSqmAttestationError>,
) -> Result<Vec<bool>, String> {
    let mut last_error = "SQM topology kept changing during start attestation".to_string();
    loop {
        if Instant::now() >= deadline {
            return Err(last_error);
        }
        events.drain()?;
        attest_configuration()?;
        let mut offline_instances = Vec::with_capacity(specs.len());
        for spec in specs {
            match attest_runtime(spec) {
                Ok(offline) => offline_instances.push(offline),
                Err(error) => {
                    last_error = format!(
                        "managed SQM did not reach its exact start postcondition for {}: {}",
                        spec.instance,
                        sqm_error_message(&error)
                    );
                    // No string classification and no mutation on a failed
                    // observation. Busy/cancellation are terminal. A failed
                    // topology observation can only be replaced by a fresh
                    // complete exact attestation after a real event.
                    if !matches!(error, NativeSqmAttestationError::Failed(_)) {
                        return Err(last_error);
                    }
                    break;
                }
            }
        }
        // Also check after failures: a changed user configuration must abort,
        // never be adopted as a new baseline while waiting for hotplug.
        attest_configuration()?;
        if Instant::now() >= deadline {
            return Err(last_error);
        }
        if events.drain()? {
            continue;
        }
        if offline_instances.len() == specs.len() {
            return Ok(offline_instances);
        }
        if !events.wait(deadline)? {
            return Err(last_error);
        }
    }
}

impl ServiceStopBackend for OpenWrtServiceStop {
    type Controllers = Vec<ControllerPidfd>;

    fn snapshot(&mut self) -> Result<ServiceStopPlan, String> {
        require_no_staged_uci(&self.paths, CAKE_PACKAGE)?;
        require_no_staged_uci(&self.paths, SQM_PACKAGE)?;
        let cake = self.environment.read_package(CAKE_PACKAGE)?;
        let sqm = self.environment.read_package(SQM_PACKAGE)?;
        plan_stop(&cake, &sqm, &self.environment)
    }

    fn attest_unchanged(&mut self, plan: &ServiceStopPlan) -> Result<(), String> {
        require_no_staged_uci(&self.paths, CAKE_PACKAGE)?;
        require_no_staged_uci(&self.paths, SQM_PACKAGE)?;
        attest_stop_configuration(&self.environment, plan)
    }

    fn capture_controllers(&mut self) -> Result<Self::Controllers, String> {
        capture_controller_pidfds(&self.paths.proc_root)
    }

    fn disable_service(&mut self) -> Result<(), String> {
        let request = format!("{{\"name\":\"{SERVICE_NAME}\"}}");
        delete_service_or_attest_absent(&self.paths.ubus, &request, "delete the procd service")
    }

    fn wait_controllers(&mut self, controllers: Self::Controllers) -> Result<(), String> {
        wait_controller_pidfds(controllers, CONTROLLER_STOP_TIMEOUT)
    }

    fn attest_no_controllers(&mut self) -> Result<(), String> {
        let controllers = discover_controllers(&self.paths.proc_root)?;
        if controllers.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "service stop left exact managed processes: {}",
                controllers
                    .iter()
                    .map(|value| format!("{}:{}", value.kind.label(), value.instance))
                    .collect::<Vec<_>>()
                    .join(",")
            ))
        }
    }

    fn clear_classifier(&mut self) {
        #[cfg(feature = "calibration")]
        if let Err(error) = run_traffic_classifier(["clear".to_string()].into_iter()) {
            eprintln!(
                "WARNING: unable to clear the native traffic classifier during stop: {error}"
            );
        }
    }

    fn stop_sqm(&mut self, spec: &ManagedSqmStopSpec) -> Result<(), String> {
        stop_managed_sqm_after_service_action(spec).map_err(|error| {
            format!(
                "unable to stop managed SQM section {}: {}",
                spec.sqm_section,
                sqm_error_message(&error)
            )
        })
    }

    fn cleanup_runtime(&mut self) -> Result<(), String> {
        cleanup_runtime_files(&self.paths)
    }

    fn cleanup_sidecars(&mut self) -> Result<(), String> {
        #[cfg(feature = "calibration")]
        cleanup_production_service_plans()?;
        Ok(())
    }
}

fn acquire_service_global_lock(paths: &ServicePaths) -> Result<ServiceGlobalLock, String> {
    ensure_owner_directory(&paths.runtime_lock_root)?;
    let guard_path = paths.runtime_lock_root.join("runtime.guard");
    if std::env::var_os("CAKE_AUTORATE_SERVICE_LOCK_BORROW").is_some_and(|value| value == "1") {
        let fd = std::env::var("CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_FD")
            .ok()
            .and_then(|value| value.parse::<i32>().ok())
            .filter(|value| *value == 8)
            .ok_or_else(|| "borrowed service lifecycle lock descriptor is invalid".to_string())?;
        let raw = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        if raw < 0 {
            return Err(format!(
                "unable to duplicate borrowed service lifecycle lock: {}",
                io::Error::last_os_error()
            ));
        }
        let borrowed = unsafe { File::from_raw_fd(raw) };
        attest_borrowed_exclusive_lock(&guard_path, &borrowed)?;
        return Ok(ServiceGlobalLock::Borrowed { _guard: borrowed });
    }

    let guard = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&guard_path)
        .map_err(|error| format!("unable to open service lifecycle lock: {error}"))?;
    attest_guard_identity(&guard_path, &guard)?;
    if unsafe { libc::flock(guard.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = io::Error::last_os_error();
        return Err(if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
            "service lifecycle is busy with another runtime mutation".to_string()
        } else {
            format!("unable to acquire service lifecycle lock: {error}")
        });
    }
    Ok(ServiceGlobalLock::Owned(guard))
}

fn attest_borrowed_exclusive_lock(path: &Path, borrowed: &File) -> Result<(), String> {
    attest_guard_identity(path, borrowed)?;
    // flock() is attached to the open file description. Re-locking a dup of
    // fd 8 would therefore always succeed and could even acquire or upgrade a
    // missing/shared lock. Probe through a separately opened description
    // instead: a real exclusive owner must reject even a shared nonblocking
    // lock with EWOULDBLOCK. This leaves the borrowed OFD untouched.
    let probe = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("unable to open service lifecycle lock probe: {error}"))?;
    attest_guard_identity(path, &probe)?;
    if unsafe { libc::flock(probe.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
        unsafe {
            libc::flock(probe.as_raw_fd(), libc::LOCK_UN);
        }
        return Err("borrowed service lifecycle descriptor has no exclusive lock".to_string());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(libc::EWOULDBLOCK) {
        return Err(format!(
            "unable to attest borrowed service lifecycle lock: {error}"
        ));
    }
    Ok(())
}

fn attest_guard_identity(path: &Path, opened: &File) -> Result<(), String> {
    let current = fs::symlink_metadata(path)
        .map_err(|error| format!("unable to inspect service lifecycle lock: {error}"))?;
    let metadata = opened
        .metadata()
        .map_err(|error| format!("unable to inspect opened service lifecycle lock: {error}"))?;
    if current.file_type().is_symlink()
        || !current.file_type().is_file()
        || current.dev() != metadata.dev()
        || current.ino() != metadata.ino()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.mode() & 0o777 != 0o600
    {
        return Err("service lifecycle lock identity is unsafe".to_string());
    }
    Ok(())
}

fn require_no_staged_uci(paths: &ServicePaths, package: &str) -> Result<(), String> {
    let output = command(paths, &paths.uci, &["-q", "changes", package], None)?;
    require_success(&output, "inspect staged UCI changes")?;
    if !output.stdout.is_empty() {
        return Err(format!(
            "service lifecycle refuses staged or uncommitted {package} changes"
        ));
    }
    Ok(())
}

fn capture_controller_pidfds(proc_root: &Path) -> Result<Vec<ControllerPidfd>, String> {
    let controllers = discover_controllers(proc_root)?;
    let mut result = Vec::with_capacity(controllers.len());
    for identity in controllers {
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, identity.pid, 0) };
        if raw < 0 {
            return Err(format!(
                "unable to open controller pidfd: {}",
                io::Error::last_os_error()
            ));
        }
        let pidfd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
        if !controller_identity_matches(proc_root, &identity)? {
            return Err(format!(
                "{} {} changed while opening pidfd",
                identity.kind.label(),
                identity.instance,
            ));
        }
        result.push(ControllerPidfd { identity, pidfd });
    }
    Ok(result)
}

fn wait_controller_pidfds(
    controllers: Vec<ControllerPidfd>,
    timeout: Duration,
) -> Result<(), String> {
    if controllers.is_empty() {
        return Ok(());
    }
    let started = Instant::now();
    let mut waiting = controllers;
    loop {
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(format!(
                "managed processes did not stop before their watchdog deadline: {}",
                waiting
                    .iter()
                    .map(|value| {
                        format!(
                            "{}:{}",
                            value.identity.kind.label(),
                            value.identity.instance
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
        let mut pollfds = waiting
            .iter()
            .map(|value| libc::pollfd {
                fd: value.pidfd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            })
            .collect::<Vec<_>>();
        let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
        let result = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as _, timeout_ms) };
        if result == 0 {
            return Err(
                "managed processes did not stop before their watchdog deadline".to_string(),
            );
        }
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("unable to wait for controller exits: {error}"));
        }
        let mut index = 0usize;
        waiting.retain(|_| {
            let revents = pollfds[index].revents;
            index += 1;
            revents & (libc::POLLIN | libc::POLLHUP) == 0
        });
        if waiting.is_empty() {
            return Ok(());
        }
    }
}

fn discover_controllers(proc_root: &Path) -> Result<Vec<ControllerIdentity>, String> {
    discover_controllers_with_policy(proc_root, true)
}

#[cfg(feature = "calibration")]
fn discover_controllers_allowing_duplicates(
    proc_root: &Path,
) -> Result<Vec<ControllerIdentity>, String> {
    discover_controllers_with_policy(proc_root, false)
}

fn discover_controllers_with_policy(
    proc_root: &Path,
    reject_duplicates: bool,
) -> Result<Vec<ControllerIdentity>, String> {
    let mut controllers = Vec::new();
    let mut instances = BTreeSet::new();
    for entry in fs::read_dir(proc_root)
        .map_err(|error| format!("unable to enumerate controller processes: {error}"))?
    {
        let entry = entry.map_err(|error| format!("unable to inspect process entry: {error}"))?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        if pid <= 1 {
            continue;
        }
        let Some((kind, instance)) = controller_instance(&entry.path().join("cmdline"))? else {
            continue;
        };
        let mut identity =
            match inspect_controller_identity(&entry.path().join("stat"), pid, kind, &instance) {
                Ok(identity) => identity,
                Err(error) if error == "controller process is a zombie" => continue,
                Err(error) => return Err(error),
            };
        if identity.process_group == 0 || identity.starttime_ticks == 0 {
            return Err("controller process identity contains a zero field".to_string());
        }
        if !instances.insert((kind, instance.clone())) && reject_duplicates {
            return Err(format!(
                "multiple exact {} processes are running for {instance}",
                kind.label()
            ));
        }
        identity.kind = kind;
        identity.instance = instance;
        controllers.push(identity);
    }
    controllers.sort_by(|left, right| {
        (left.kind, left.instance.as_str()).cmp(&(right.kind, right.instance.as_str()))
    });
    Ok(controllers)
}

fn controller_instance(path: &Path) -> Result<Option<(ServiceProcessKind, String)>, String> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "unable to inspect controller command line: {error}"
            ))
        }
    };
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_CMDLINE + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("unable to read controller command line: {error}"))?;
    let controller = format!(
        "{DAEMON_PATH}\0{}\0",
        ServiceProcessKind::Controller.argument()
    );
    let (kind, prefix) = if bytes.starts_with(controller.as_bytes()) {
        (ServiceProcessKind::Controller, controller.len())
    } else {
        #[cfg(feature = "calibration")]
        {
            let mqtt = format!(
                "{DAEMON_PATH}\0{}\0",
                ServiceProcessKind::MqttPublisher.argument()
            );
            if bytes.starts_with(mqtt.as_bytes()) {
                (ServiceProcessKind::MqttPublisher, mqtt.len())
            } else {
                return Ok(None);
            }
        }
        #[cfg(not(feature = "calibration"))]
        {
            return Ok(None);
        }
    };
    if bytes.len() > MAX_CMDLINE as usize {
        return Err("controller command line exceeds its safety bound".to_string());
    }
    parse_service_process_instance(&bytes, kind, prefix)
}

fn parse_service_process_instance(
    bytes: &[u8],
    kind: ServiceProcessKind,
    prefix: usize,
) -> Result<Option<(ServiceProcessKind, String)>, String> {
    let Some(instance_bytes) = bytes[prefix..].strip_suffix(&[0]) else {
        return Ok(None);
    };
    if instance_bytes.is_empty() || instance_bytes.contains(&0) {
        return Ok(None);
    }
    let instance = std::str::from_utf8(instance_bytes)
        .map_err(|_| "controller instance is not UTF-8".to_string())?;
    if !safe_name(instance) {
        return Err("controller instance is unsafe".to_string());
    }
    Ok(Some((kind, instance.to_string())))
}

fn inspect_controller_identity(
    path: &Path,
    pid: u32,
    kind: ServiceProcessKind,
    instance: &str,
) -> Result<ControllerIdentity, String> {
    let stat = match fs::read_to_string(path) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err("controller disappeared during process snapshot".to_string())
        }
        Err(error) => {
            return Err(format!(
                "unable to read controller process identity: {error}"
            ))
        }
    };
    let close = stat
        .rfind(')')
        .ok_or_else(|| "controller process identity is malformed".to_string())?;
    if !stat[..=close].starts_with(&format!("{pid} (")) {
        return Err("controller process PID identity changed".to_string());
    }
    let fields = stat[close + 1..]
        .split_ascii_whitespace()
        .collect::<Vec<_>>();
    if fields.len() <= 19 || fields[0].as_bytes().len() != 1 {
        return Err("controller process identity has too few fields".to_string());
    }
    if fields[0] == "Z" {
        return Err("controller process is a zombie".to_string());
    }
    let process_group = fields[2]
        .parse::<u32>()
        .map_err(|_| "controller process group is invalid".to_string())?;
    let starttime_ticks = fields[19]
        .parse::<u64>()
        .map_err(|_| "controller process start time is invalid".to_string())?;
    Ok(ControllerIdentity {
        kind,
        instance: instance.to_string(),
        pid,
        process_group,
        starttime_ticks,
    })
}

fn controller_identity_matches(
    proc_root: &Path,
    expected: &ControllerIdentity,
) -> Result<bool, String> {
    let root = proc_root.join(expected.pid.to_string());
    if controller_instance(&root.join("cmdline"))?
        != Some((expected.kind, expected.instance.clone()))
    {
        return Ok(false);
    }
    match inspect_controller_identity(
        &root.join("stat"),
        expected.pid,
        expected.kind,
        &expected.instance,
    ) {
        Ok(actual) => Ok(actual == *expected),
        Err(error) if error.contains("disappeared") || error.contains("zombie") => Ok(false),
        Err(error) => Err(error),
    }
}

fn cleanup_runtime_files(paths: &ServicePaths) -> Result<(), String> {
    let entries = match fs::read_dir(&paths.runtime_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("unable to enumerate runtime directories: {error}")),
    };
    for entry in entries {
        let entry = entry.map_err(|error| format!("unable to inspect runtime entry: {error}"))?;
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| format!("unable to inspect runtime directory: {error}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        let Some(instance) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !safe_name(&instance) {
            continue;
        }
        if discover_controllers(&paths.proc_root)?
            .iter()
            .any(|controller| {
                controller.kind == ServiceProcessKind::Controller && controller.instance == instance
            })
        {
            eprintln!("WARNING: preserving runtime files for active replacement {instance}");
            continue;
        }
        for name in ["status.json", "history.csv", "history.csv.tmp"] {
            let path = entry.path().join(name);
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("unable to inspect runtime file: {error}")),
            };
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.nlink() != 1
            {
                return Err(format!("runtime file {} is unsafe", path.display()));
            }
            if discover_controllers(&paths.proc_root)?
                .iter()
                .any(|controller| {
                    controller.kind == ServiceProcessKind::Controller
                        && controller.instance == instance
                })
            {
                eprintln!("WARNING: preserving runtime files for replacement {instance}");
                break;
            }
            fs::remove_file(&path).map_err(|error| {
                format!("unable to remove runtime file {}: {error}", path.display())
            })?;
        }
    }
    Ok(())
}

fn command(
    _paths: &ServicePaths,
    program: &Path,
    arguments: &[&str],
    input: Option<&[u8]>,
) -> Result<BoundedCommandOutput, String> {
    command_owned(
        program,
        arguments.iter().map(OsString::from).collect(),
        input,
    )
}

fn command_owned(
    program: &Path,
    arguments: Vec<OsString>,
    input: Option<&[u8]>,
) -> Result<BoundedCommandOutput, String> {
    run_bounded_command_output_with_input(
        &SpawnSpec {
            program: program.to_path_buf(),
            arguments,
            environment: Vec::new(),
        },
        input,
        COMMAND_TIMEOUT,
        MAX_OUTPUT,
        || false,
        |_| {},
    )
}

fn ensure_owner_directory(path: &Path) -> Result<(), String> {
    match fs::create_dir(path) {
        Ok(()) => fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("unable to secure service owner directory: {error}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!("unable to create service owner directory: {error}"));
        }
    }
    if owner_directory_is_exact(path) {
        Ok(())
    } else {
        Err("service owner directory is not private".to_string())
    }
}

fn owner_directory_is_exact(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.file_type().is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o7777 == 0o700
    })
}

fn read_exact_file(path: &Path) -> Result<Option<ExactFileSnapshot>, String> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("unable to open {}: {error}", path.display())),
    };
    let before = file
        .metadata()
        .map_err(|error| format!("unable to inspect {}: {error}", path.display()))?;
    if !before.file_type().is_file() || before.nlink() != 1 {
        return Err(format!(
            "{} is not an exclusive regular configuration file",
            path.display()
        ));
    }
    if before.len() > MAX_BRIDGER_CONFIG_BYTES as u64 {
        return Err(format!("{} exceeds its byte bound", path.display()));
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_BRIDGER_CONFIG_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("unable to read {}: {error}", path.display()))?;
    if bytes.len() > MAX_BRIDGER_CONFIG_BYTES {
        return Err(format!("{} exceeds its byte bound", path.display()));
    }
    let after = file
        .metadata()
        .map_err(|error| format!("unable to reinspect {}: {error}", path.display()))?;
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
    {
        return Err(format!("{} changed while it was read", path.display()));
    }
    Ok(Some(ExactFileSnapshot {
        bytes,
        device: after.dev(),
        inode: after.ino(),
        mode: after.mode(),
        uid: after.uid(),
        gid: after.gid(),
    }))
}

fn write_new_file(path: &Path, bytes: &[u8], mode: u32, uid: u32, gid: u32) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("unable to create {}: {error}", path.display()))?;
    let result = (|| {
        if unsafe { libc::fchown(file.as_raw_fd(), uid, gid) } != 0 {
            return Err(format!(
                "unable to set ownership on {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        file.set_permissions(fs::Permissions::from_mode(mode & 0o7777))
            .map_err(|error| format!("unable to set mode on {}: {error}", path.display()))?;
        file.write_all(bytes)
            .map_err(|error| format!("unable to write {}: {error}", path.display()))?;
        file.sync_all()
            .map_err(|error| format!("unable to sync {}: {error}", path.display()))
    })();
    if result.is_err() {
        drop(file);
        let _ = fs::remove_file(path);
    }
    result
}

fn same_exact_file(left: &ExactFileSnapshot, right: &ExactFileSnapshot) -> bool {
    left.bytes == right.bytes
        && left.device == right.device
        && left.inode == right.inode
        && left.mode == right.mode
        && left.uid == right.uid
        && left.gid == right.gid
}

fn replace_exact_file(
    path: &Path,
    original: &ExactFileSnapshot,
    candidate: &[u8],
) -> Result<(), String> {
    let current = read_exact_file(path)?
        .ok_or_else(|| format!("{} disappeared before replacement", path.display()))?;
    if !same_exact_file(original, &current) {
        return Err(format!("{} changed before replacement", path.display()));
    }
    let parent = path
        .parent()
        .ok_or_else(|| "bridger configuration has no parent directory".to_string())?;
    let temporary = parent.join(format!(
        ".bridger.cake-autorate.{}.{}",
        std::process::id(),
        REPLACE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    write_new_file(
        &temporary,
        candidate,
        original.mode,
        original.uid,
        original.gid,
    )?;
    let current = read_exact_file(path)?
        .ok_or_else(|| format!("{} disappeared before commit", path.display()))?;
    if !same_exact_file(original, &current) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("{} changed before commit", path.display()));
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("unable to commit bridger configuration: {error}"));
    }
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("unable to sync bridger configuration directory: {error}"))?;
    let committed = read_exact_file(path)?
        .ok_or_else(|| "bridger configuration disappeared after commit".to_string())?;
    if committed.bytes != candidate
        || committed.mode != original.mode
        || committed.uid != original.uid
        || committed.gid != original.gid
    {
        return Err("bridger configuration failed its exact commit postcondition".to_string());
    }
    Ok(())
}

fn require_success(output: &BoundedCommandOutput, operation: &str) -> Result<(), String> {
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if detail.is_empty() {
        format!("unable to {operation}: {}", output.status)
    } else {
        format!("unable to {operation}: {detail}")
    })
}

fn executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn option(section: &UciSection, key: &str) -> Option<String> {
    section.options.get(key).cloned()
}

fn first_option(section: &UciSection, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| option(section, key).filter(|value| !value.is_empty()))
}

fn bool_option(section: &UciSection, key: &str, default: bool) -> bool {
    option(section, key).map_or(default, |value| value == "1")
}

fn parse_rate(section: &UciSection, key: &str) -> Result<u64, String> {
    option(section, key)
        .ok_or_else(|| format!("SQM option {key} is missing"))?
        .parse::<u64>()
        .ok()
        .filter(|value| *value <= 10_000_000_000)
        .ok_or_else(|| format!("SQM option {key} is invalid"))
}

fn env_path(name: &str, default: &str) -> PathBuf {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    static TEST_SEQUENCE: AtomicU32 = AtomicU32::new(0);

    #[derive(Default)]
    struct Resolver(BTreeMap<String, String>);

    impl InterfaceResolver for Resolver {
        fn resolve(&self, name: &str) -> Result<String, String> {
            Ok(self
                .0
                .get(name)
                .cloned()
                .unwrap_or_else(|| name.to_string()))
        }
    }

    fn parse(package: &str, body: &str) -> UciPackage {
        UciPackage::parse(package, body).unwrap()
    }

    fn projection(conflicts: &[&str]) -> SqmProjectionPlan {
        SqmProjectionPlan::test_summary(
            true,
            BTreeSet::from(["wwan0".to_string()]),
            BTreeSet::from(["wwan0".to_string()]),
            conflicts.iter().map(|value| value.to_string()).collect(),
        )
    }

    fn test_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "cake-service-lifecycle-{label}-{}-{}",
            std::process::id(),
            TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        root
    }

    fn write_executable(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    struct ScriptedStartEvents {
        drains: std::collections::VecDeque<bool>,
        waits: std::collections::VecDeque<bool>,
    }

    impl StartEvents for ScriptedStartEvents {
        fn drain(&mut self) -> Result<bool, String> {
            Ok(self.drains.pop_front().expect("unexpected event drain"))
        }
        fn wait(&mut self, _: Instant) -> Result<bool, String> {
            Ok(self.waits.pop_front().expect("unexpected readiness wait"))
        }
    }

    fn start_spec(instance: &str) -> ManagedSqmAttestationSpec {
        ManagedSqmAttestationSpec {
            instance: instance.to_string(),
            sqm_section: format!("cake_{instance}"),
            target_interface: "eth1".to_string(),
            upload_interface: "eth1".to_string(),
            download_interface: "ifb4eth1".to_string(),
            direction_mode: "both".to_string(),
            minimum_download_kbps: 20_000,
            maximum_download_kbps: 20_000,
            minimum_upload_kbps: 21_202,
            maximum_upload_kbps: 21_202,
        }
    }

    #[test]
    fn sqm_start_hotplug_gap_requires_event_then_rechecks_every_instance() {
        let mut events = ScriptedStartEvents {
            drains: [false, false, true, false].into(),
            waits: [true].into(),
        };
        let specs = [start_spec("first"), start_spec("second")];
        let mut calls = Vec::new();
        let mut checks = 0;
        let result = await_sqm_start(
            &mut events,
            Instant::now() + Duration::from_secs(10),
            &specs,
            || {
                checks += 1;
                Ok(())
            },
            |spec| {
                calls.push(spec.instance.clone());
                if calls.len() == 2 {
                    Err(NativeSqmAttestationError::Failed(
                        "IFB counter missing in hotplug stop/start gap".into(),
                    ))
                } else {
                    Ok(false)
                }
            },
        )
        .unwrap();
        assert_eq!(result, [false, false]);
        assert_eq!(calls, ["first", "second", "first", "second"]);
        assert_eq!(checks, 4);
        assert!(events.waits.is_empty());
    }

    #[test]
    fn sqm_start_event_during_success_invalidates_the_whole_observation() {
        let mut events = ScriptedStartEvents {
            drains: [false, true, false, false].into(),
            waits: [].into(),
        };
        let mut calls = 0;
        let result = await_sqm_start(
            &mut events,
            Instant::now() + Duration::from_secs(10),
            &[start_spec("wan")],
            || Ok(()),
            |_| {
                calls += 1;
                Ok(calls == 2)
            },
        )
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(result, [true]); // Only the existing strict offline proof can return true.
    }

    #[test]
    fn sqm_start_timeout_never_accepts_foreign_or_missing_runtime() {
        let mut events = ScriptedStartEvents {
            drains: [false, false].into(),
            waits: [false].into(),
        };
        let mut calls = 0;
        let error = await_sqm_start(
            &mut events,
            Instant::now() + Duration::from_secs(10),
            &[start_spec("wan")],
            || Ok(()),
            |_| {
                calls += 1;
                Err(NativeSqmAttestationError::Failed("foreign runtime".into()))
            },
        )
        .unwrap_err();
        assert!(error.contains("foreign runtime"));
        assert_eq!(calls, 1); // No timer polling and no repeated service actions.
    }

    #[test]
    fn sqm_start_configuration_change_after_failed_observation_aborts_before_wait() {
        let mut events = ScriptedStartEvents {
            drains: [false].into(),
            waits: [].into(),
        };
        let mut checks = 0;
        let error = await_sqm_start(
            &mut events,
            Instant::now() + Duration::from_secs(10),
            &[start_spec("wan")],
            || {
                checks += 1;
                if checks == 2 {
                    Err("configuration changed".into())
                } else {
                    Ok(())
                }
            },
            |_| {
                Err(NativeSqmAttestationError::Failed(
                    "state is being replaced".into(),
                ))
            },
        )
        .unwrap_err();
        assert_eq!(error, "configuration changed");
        assert_eq!(checks, 2);
    }

    #[test]
    fn sqm_start_cancellation_and_busy_are_terminal_without_wait() {
        for error in [
            NativeSqmAttestationError::Terminated,
            NativeSqmAttestationError::Busy("owner busy".into()),
        ] {
            let mut events = ScriptedStartEvents {
                drains: [false].into(),
                waits: [].into(),
            };
            let mut calls = 0;
            assert!(await_sqm_start(
                &mut events,
                Instant::now() + Duration::from_secs(10),
                &[start_spec("wan")],
                || Ok(()),
                |_| {
                    calls += 1;
                    Err(error.clone())
                }
            )
            .is_err());
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn start_plan_preserves_manual_instances_and_requires_exact_managed_backing() {
        let cake = parse(
            CAKE_PACKAGE,
            "cake-autorate.manual=cake_autorate\ncake-autorate.manual.enabled='1'\ncake-autorate.manual.manage_sqm='0'\ncake-autorate.wan=cake_autorate\ncake-autorate.wan.enabled='1'\ncake-autorate.wan.manage_sqm='1'\ncake-autorate.wan.sqm_enabled='1'\ncake-autorate.wan.sqm_direction_mode='both'\ncake-autorate.wan.sqm_interface='wan'\ncake-autorate.wan.dl_if='ifb4wwan0'\ncake-autorate.wan.sqm_section='cake_wan'\n",
        );
        let sqm = parse(
            SQM_PACKAGE,
            "sqm.cake_wan=queue\nsqm.cake_wan.enabled='1'\nsqm.cake_wan.interface='wwan0'\nsqm.cake_wan._cake_autorate_managed='wan'\nsqm.cake_wan.download='500000'\nsqm.cake_wan.upload='100000'\n",
        );
        let resolver = Resolver(BTreeMap::from([("wan".to_string(), "wwan0".to_string())]));
        let plan = plan_start(&cake, &sqm, &resolver, &projection(&[])).unwrap();
        assert_eq!(plan.instances, ["manual", "wan"]);
        assert_eq!(plan.managed.len(), 1);
        assert_eq!(plan.managed[0].target_interface, "wwan0");
        assert_eq!(plan.managed[0].minimum_download_kbps, 500_000);
        #[cfg(feature = "calibration")]
        assert_eq!(
            encode_start_plan(&plan.instances, &[]),
            "service-start-v2 manual,wan -\n"
        );
        #[cfg(not(feature = "calibration"))]
        assert_eq!(
            encode_start_plan(&plan.instances, &[]),
            "service-start-v1 manual,wan\n"
        );
    }

    #[test]
    fn disabled_missing_foreign_and_conflicting_managed_queues_never_start() {
        let cake = parse(
            CAKE_PACKAGE,
            "cake-autorate.disabled=cake_autorate\ncake-autorate.disabled.enabled='1'\ncake-autorate.disabled.sqm_enabled='0'\ncake-autorate.missing=cake_autorate\ncake-autorate.missing.enabled='1'\ncake-autorate.missing.sqm_enabled='1'\ncake-autorate.missing.sqm_interface='wwan0'\ncake-autorate.missing.sqm_section='none'\ncake-autorate.conflict=cake_autorate\ncake-autorate.conflict.enabled='1'\ncake-autorate.conflict.manage_sqm='0'\n",
        );
        let plan = plan_start(
            &cake,
            &UciPackage::default(),
            &Resolver::default(),
            &projection(&["conflict"]),
        )
        .unwrap();
        assert!(plan.instances.is_empty());
        assert!(plan.managed.is_empty());
    }

    #[test]
    fn invalid_direction_and_rate_shape_fail_closed() {
        let cake = parse(
            CAKE_PACKAGE,
            "cake-autorate.wan=cake_autorate\ncake-autorate.wan.enabled='1'\ncake-autorate.wan.sqm_enabled='1'\ncake-autorate.wan.sqm_interface='wwan0'\ncake-autorate.wan.sqm_section='cake_wan'\ncake-autorate.wan.sqm_direction_mode='upload_only'\n",
        );
        let sqm = parse(
            SQM_PACKAGE,
            "sqm.cake_wan=queue\nsqm.cake_wan.enabled='1'\nsqm.cake_wan.interface='wwan0'\nsqm.cake_wan._cake_autorate_managed='wan'\nsqm.cake_wan.download='1'\nsqm.cake_wan.upload='100000'\n",
        );
        assert!(plan_start(&cake, &sqm, &Resolver::default(), &projection(&[]),).is_err());
    }

    #[test]
    fn output_protocol_is_bounded_and_unambiguous() {
        #[cfg(feature = "calibration")]
        assert_eq!(encode_start_plan(&[], &[]), "service-start-v2 - -\n");
        #[cfg(not(feature = "calibration"))]
        assert_eq!(encode_start_plan(&[], &[]), "service-start-v1 -\n");
        let instances = (0..MAX_INSTANCES)
            .map(|index| format!("wan_{index}"))
            .collect::<Vec<_>>();
        #[cfg(feature = "calibration")]
        assert!(encode_start_plan(&instances, &["wan_0".to_string()])
            .starts_with("service-start-v2 wan_0,"));
        #[cfg(not(feature = "calibration"))]
        assert!(encode_start_plan(&instances, &[]).starts_with("service-start-v1 wan_0,"));
        let mut full = instances;
        assert!(push_instance(&mut full, "overflow").is_err());
        assert_eq!(
            bridger_batch("cake_bridger_7_0", &["ifb4wwan0".to_string()]),
            "add_list cake_bridger_7_0.@defaults[0].blacklist='ifb4wwan0'\ncommit cake_bridger_7_0\n"
        );
    }

    #[test]
    fn package_upgrade_has_a_distinct_typed_deferral() {
        assert!(!package_upgrade_mode_value(None).unwrap());
        assert!(!package_upgrade_mode_value(Some(std::ffi::OsStr::new(""))).unwrap());
        assert!(!package_upgrade_mode_value(Some(std::ffi::OsStr::new("0"))).unwrap());
        assert!(package_upgrade_mode_value(Some(std::ffi::OsStr::new("1"))).unwrap());
        for invalid in ["true", "01", "2", "-1"] {
            assert!(package_upgrade_mode_value(Some(std::ffi::OsStr::new(invalid))).is_err());
        }
        assert_eq!(
            format!("{SERVICE_START_DEFERRED_V1}\n"),
            "service-start-deferred-v1\n"
        );
    }

    #[cfg(feature = "calibration")]
    #[test]
    fn pending_native_apply_recovery_cannot_enter_ordinary_start() {
        assert!(require_no_native_apply_recovery(false, false).is_ok());
        assert!(require_no_native_apply_recovery(true, false).is_err());
        assert!(require_no_native_apply_recovery(false, true).is_err());
        assert!(require_no_native_apply_recovery(true, true).is_err());
    }

    #[test]
    fn borrowed_lock_requires_a_preexisting_exclusive_owner_without_upgrading_it() {
        let root = test_root("borrowed-lock");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("runtime.guard");
        let owner = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();

        assert!(attest_borrowed_exclusive_lock(&path, &owner)
            .unwrap_err()
            .contains("has no exclusive lock"));

        assert_eq!(
            unsafe { libc::flock(owner.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) },
            0
        );
        assert!(attest_borrowed_exclusive_lock(&path, &owner)
            .unwrap_err()
            .contains("has no exclusive lock"));
        let shared_probe = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(shared_probe.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB,) },
            0,
            "borrow attestation silently upgraded the shared lock"
        );
        unsafe {
            libc::flock(shared_probe.as_raw_fd(), libc::LOCK_UN);
            libc::flock(owner.as_raw_fd(), libc::LOCK_UN);
        }

        assert_eq!(
            unsafe { libc::flock(owner.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        attest_borrowed_exclusive_lock(&path, &owner).unwrap();
        let exclusive_probe = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert_ne!(
            unsafe { libc::flock(exclusive_probe.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB,) },
            0,
            "borrow attestation released the parent's exclusive lock"
        );
        unsafe {
            libc::flock(owner.as_raw_fd(), libc::LOCK_UN);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bridger_update_is_materialized_under_an_alias_and_committed_atomically() {
        let root = test_root("bridger");
        let config = root.join("bridger");
        let uci = root.join("uci");
        let bridger_init = root.join("bridger-init");
        let log = root.join("calls");
        fs::write(
            &config,
            b"config defaults\n\toption bridge_local_tx '1'\n\tlist blacklist 'eth0'\n",
        )
        .unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o640)).unwrap();
        write_executable(
            &uci,
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
config_dir=
while [ "$#" -gt 0 ]; do
    case "$1" in
        -c) config_dir="$2"; shift 2 ;;
        -C|-t) shift 2 ;;
        -q) shift ;;
        *) break ;;
    esac
done
command="$1"
shift
case "$command" in
    get)
        target="$1"
        alias="${{target%%.*}}"
        case "$target" in
            *.@defaults\[0\].blacklist)
                sed -n "s/^[[:space:]]*list[[:space:]]*blacklist[[:space:]]*'\\([^']*\\)'.*/\\1/p" "$config_dir/$alias"
                ;;
            *.@defaults\[0\]) printf '%s\n' defaults ;;
            *) exit 1 ;;
        esac
        ;;
    batch)
        while IFS=' ' read -r operation target; do
            case "$operation" in
                add_list)
                    alias="${{target%%.*}}"
                    value="${{target#*=}}"
                    value="${{value#\'}}"
                    value="${{value%\'}}"
                    printf "\tlist blacklist '%s'\n" "$value" >> "$config_dir/$alias"
                    ;;
                commit) : ;;
                '') : ;;
                *) exit 1 ;;
            esac
        done
        ;;
    *) exit 1 ;;
esac
"#,
                log.display()
            ),
        );
        write_executable(
            &bridger_init,
            &format!(
                "#!/bin/sh\nprintf 'bridger:%s\\n' \"$*\" >> '{}'\n",
                log.display()
            ),
        );
        let paths = ServicePaths {
            uci,
            tc: root.join("tc"),
            ubus: root.join("ubus"),
            proc_root: root.join("proc"),
            runtime_root: root.join("runtime"),
            runtime_lock_root: root.join("locks"),
            sqm_init: root.join("sqm-init"),
            bridger_init,
            bridger_config: config.clone(),
            uci_workspace_root: root.join("workspace"),
        };
        sync_bridger_blacklist(&paths, &BTreeSet::from(["ifb4wwan0".to_string()])).unwrap();

        let committed = fs::read_to_string(&config).unwrap();
        assert!(committed.contains("option bridge_local_tx '1'"));
        assert!(committed.contains("list blacklist 'eth0'"));
        assert!(committed.contains("list blacklist 'ifb4wwan0'"));
        assert_eq!(fs::metadata(&config).unwrap().mode() & 0o7777, 0o640);
        let calls = fs::read_to_string(&log).unwrap();
        assert!(calls
            .lines()
            .filter(|line| line.starts_with("-c "))
            .all(|line| line.contains(" -C ") && line.contains(" -t ")));
        assert!(calls
            .lines()
            .filter(|line| line.contains(" get "))
            .all(|line| line.contains("cake_bridger_") && !line.contains(" bridger.@")));
        assert!(calls.lines().any(|line| line == "bridger:reload"));
        assert!(fs::read_dir(root.join("workspace"))
            .unwrap()
            .next()
            .is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_file_replacement_rejects_concurrent_drift() {
        let root = test_root("replace-drift");
        let config = root.join("bridger");
        fs::write(&config, b"original\n").unwrap();
        let snapshot = read_exact_file(&config).unwrap().unwrap();
        fs::write(&config, b"foreign\n").unwrap();
        assert!(replace_exact_file(&config, &snapshot, b"candidate\n").is_err());
        assert_eq!(fs::read(&config).unwrap(), b"foreign\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[derive(Default)]
    struct FakeStopBackend {
        events: Vec<String>,
        plan: ServiceStopPlan,
        residual_controller: bool,
    }

    impl ServiceStopBackend for FakeStopBackend {
        type Controllers = ();

        fn snapshot(&mut self) -> Result<ServiceStopPlan, String> {
            self.events.push("snapshot".to_string());
            Ok(self.plan.clone())
        }

        fn attest_unchanged(&mut self, _plan: &ServiceStopPlan) -> Result<(), String> {
            self.events.push("attest".to_string());
            Ok(())
        }

        fn capture_controllers(&mut self) -> Result<Self::Controllers, String> {
            self.events.push("pidfd-open".to_string());
            Ok(())
        }

        fn disable_service(&mut self) -> Result<(), String> {
            self.events.push("service-delete".to_string());
            Ok(())
        }

        fn wait_controllers(&mut self, _controllers: Self::Controllers) -> Result<(), String> {
            self.events.push("pidfd-wait".to_string());
            Ok(())
        }

        fn attest_no_controllers(&mut self) -> Result<(), String> {
            self.events.push("process-rescan".to_string());
            if self.residual_controller {
                Err("residual controller".to_string())
            } else {
                Ok(())
            }
        }

        fn clear_classifier(&mut self) {
            self.events.push("classifier-clear".to_string());
        }

        fn stop_sqm(&mut self, spec: &ManagedSqmStopSpec) -> Result<(), String> {
            self.events.push(format!("sqm-stop:{}", spec.sqm_section));
            Ok(())
        }

        fn cleanup_runtime(&mut self) -> Result<(), String> {
            self.events.push("runtime-cleanup".to_string());
            Ok(())
        }

        fn cleanup_sidecars(&mut self) -> Result<(), String> {
            self.events.push("sidecar-cleanup".to_string());
            Ok(())
        }
    }

    fn stop_spec(section: &str, target: &str) -> ManagedSqmStopSpec {
        ManagedSqmStopSpec {
            instance: section.trim_start_matches("cake_").to_string(),
            sqm_section: section.to_string(),
            target_interface: target.to_string(),
            download_interface: format!("ifb4{target}"),
            rate_policy: None,
        }
    }

    #[test]
    fn stop_state_machine_disables_respawn_before_sqm_and_cleans_last() {
        let mut backend = FakeStopBackend {
            plan: ServiceStopPlan {
                managed: vec![stop_spec("cake_wan", "wwan0")],
                ..ServiceStopPlan::default()
            },
            ..FakeStopBackend::default()
        };
        execute_stop(&mut backend).unwrap();
        let joined = backend.events.join("|");
        assert!(joined.find("pidfd-open").unwrap() < joined.find("service-delete").unwrap());
        assert!(joined.find("pidfd-wait").unwrap() < joined.find("sqm-stop:cake_wan").unwrap());
        assert!(
            joined.find("sqm-stop:cake_wan").unwrap() < joined.find("runtime-cleanup").unwrap()
        );
        assert_eq!(
            backend
                .events
                .iter()
                .filter(|event| *event == "process-rescan")
                .count(),
            4
        );
        assert!(joined.find("runtime-cleanup").unwrap() < joined.find("sidecar-cleanup").unwrap());
    }

    #[test]
    fn residual_controller_blocks_every_runtime_mutation() {
        let mut backend = FakeStopBackend {
            residual_controller: true,
            ..FakeStopBackend::default()
        };
        assert!(execute_stop(&mut backend).is_err());
        assert_eq!(
            backend.events,
            [
                "snapshot",
                "attest",
                "pidfd-open",
                "attest",
                "service-delete",
                "pidfd-wait",
                "process-rescan"
            ]
        );
    }

    #[test]
    fn stop_plan_includes_disabled_and_stale_owned_queues_but_rejects_shared_target() {
        let cake = parse(
            CAKE_PACKAGE,
            "cake-autorate.wan=cake_autorate\ncake-autorate.wan.dl_if='ifb4wwan0'\n",
        );
        let sqm = parse(
            SQM_PACKAGE,
            "sqm.cake_wan=queue\nsqm.cake_wan.enabled='0'\nsqm.cake_wan.interface='wwan0'\nsqm.cake_wan._cake_autorate_managed='wan'\nsqm.stale=queue\nsqm.stale.enabled='0'\nsqm.stale.interface='wwan1'\nsqm.stale._cake_autorate_managed='removed'\n",
        );
        let plan = plan_stop(&cake, &sqm, &Resolver::default()).unwrap();
        assert_eq!(
            plan.managed
                .iter()
                .map(|spec| spec.sqm_section.as_str())
                .collect::<Vec<_>>(),
            ["cake_wan", "stale"]
        );
        assert_eq!(plan.managed[1].download_interface, "ifb4wwan1");
        assert_eq!(
            plan.managed[0].rate_policy,
            Some(ManagedSqmRatePolicy {
                minimum_download_kbps: crate::rate_limits::DEFAULT_MIN_DL_SHAPER_RATE_KBPS,
                maximum_download_kbps: crate::rate_limits::DEFAULT_MAX_DL_SHAPER_RATE_KBPS,
                minimum_upload_kbps: crate::rate_limits::DEFAULT_MIN_UL_SHAPER_RATE_KBPS,
                maximum_upload_kbps: crate::rate_limits::DEFAULT_MAX_UL_SHAPER_RATE_KBPS,
            })
        );
        assert_eq!(plan.managed[1].rate_policy, None);

        let wrong_owner_type = parse(
            CAKE_PACKAGE,
            "cake-autorate.wan=system\ncake-autorate.wan.dl_if='ifb4wwan0'\n",
        );
        assert!(plan_stop(&wrong_owner_type, &sqm, &Resolver::default()).is_err());

        let duplicate = parse(
            SQM_PACKAGE,
            "sqm.one=queue\nsqm.one.interface='wwan0'\nsqm.one._cake_autorate_managed='wan'\nsqm.two=queue\nsqm.two.interface='wwan0'\nsqm.two._cake_autorate_managed='wanb'\n",
        );
        assert!(plan_stop(&cake, &duplicate, &Resolver::default()).is_err());
    }

    #[test]
    fn controller_match_is_exact_and_rejects_unsafe_identity() {
        let root = test_root("controller-cmdline");
        let path = root.join("cmdline");
        fs::write(&path, b"/usr/sbin/cake-autorated\0--instance\0wan\0").unwrap();
        assert_eq!(
            controller_instance(&path).unwrap(),
            Some((ServiceProcessKind::Controller, "wan".to_string()))
        );
        #[cfg(feature = "calibration")]
        {
            fs::write(&path, b"/usr/sbin/cake-autorated\0--mqtt-publisher\0wan\0").unwrap();
            assert_eq!(
                controller_instance(&path).unwrap(),
                Some((ServiceProcessKind::MqttPublisher, "wan".to_string()))
            );
        }
        fs::write(
            &path,
            b"/usr/sbin/cake-autorated\0--instance\0wan\0--extra\0",
        )
        .unwrap();
        assert_eq!(controller_instance(&path).unwrap(), None);
        fs::write(&path, b"/usr/sbin/cake-autorated\0--instance\0bad-name\0").unwrap();
        assert!(controller_instance(&path).is_err());
        let mut unrelated = b"/usr/bin/apcontroller\0".to_vec();
        unrelated.extend(std::iter::repeat_n(b'x', MAX_CMDLINE as usize + 32));
        unrelated.push(0);
        fs::write(&path, unrelated).unwrap();
        assert_eq!(controller_instance(&path).unwrap(), None);
        let mut oversized_owned = b"/usr/sbin/cake-autorated\0--instance\0wan\0".to_vec();
        oversized_owned.extend(std::iter::repeat_n(b'y', MAX_CMDLINE as usize + 32));
        fs::write(&path, oversized_owned).unwrap();
        assert!(controller_instance(&path)
            .unwrap_err()
            .contains("exceeds its safety bound"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(feature = "calibration")]
    fn controller_readiness_binds_process_status_state_and_freshness() {
        let root = test_root("controller-readiness");
        let proc_root = root.join("proc");
        let runtime_root = root.join("run");
        fs::create_dir(&proc_root).unwrap();
        fs::create_dir(&runtime_root).unwrap();
        fs::write(proc_root.join("stat"), b"cpu 1 2 3 4\nbtime 100\n").unwrap();
        let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
        assert!(ticks_per_second > 0);
        let process = proc_root.join("100");
        fs::create_dir(&process).unwrap();
        fs::write(
            process.join("cmdline"),
            b"/usr/sbin/cake-autorated\0--instance\0wan\0",
        )
        .unwrap();
        fs::write(
            process.join("stat"),
            format!(
                "100 (cake) S 1 100 100 0 0 0 0 0 0 0 0 0 0 0 0 0 1 0 {}\n",
                ticks_per_second * 10
            ),
        )
        .unwrap();
        let status_dir = runtime_root.join("wan");
        fs::create_dir(&status_dir).unwrap();
        let status = status_dir.join("status.json");
        let write_status = |state: &str, started_at: f64, updated_at: f64| {
            fs::write(
                &status,
                format!(
                    "{{\"instance\":\"wan\",\"state\":\"{state}\",\"started_at\":{started_at},\"updated_at\":{updated_at}}}"
                ),
            )
            .unwrap();
            fs::set_permissions(&status, fs::Permissions::from_mode(0o644)).unwrap();
        };
        let paths = ServicePaths {
            uci: root.join("uci"),
            tc: root.join("tc"),
            ubus: root.join("ubus"),
            proc_root: proc_root.clone(),
            runtime_root: runtime_root.clone(),
            runtime_lock_root: root.join("locks"),
            sqm_init: root.join("sqm"),
            bridger_init: root.join("bridger"),
            bridger_config: root.join("bridger-config"),
            uci_workspace_root: root.join("uci-work"),
        };
        let expected = vec!["wan".to_string()];

        write_status("WAITING_OPERATION", 111.0, 121.0);
        assert!(matches!(
            observe_controller_start(&paths, &expected, 121.0).unwrap(),
            ControllerStartReadiness::Waiting(reason) if reason.contains("WAITING_OPERATION")
        ));

        write_status("RUNNING", 111.0, 121.0);
        assert_eq!(
            observe_controller_start(&paths, &expected, 121.0).unwrap(),
            ControllerStartReadiness::Ready
        );

        let replacement = proc_root.join("101");
        fs::create_dir(&replacement).unwrap();
        fs::write(
            replacement.join("cmdline"),
            b"/usr/sbin/cake-autorated\0--instance\0wan\0",
        )
        .unwrap();
        fs::write(
            replacement.join("stat"),
            format!(
                "101 (cake) S 1 101 101 0 0 0 0 0 0 0 0 0 0 0 0 0 1 0 {}\n",
                ticks_per_second * 11
            ),
        )
        .unwrap();
        assert!(matches!(
            observe_controller_start(&paths, &expected, 121.0).unwrap(),
            ControllerStartReadiness::Waiting(reason) if reason.contains("observed 2")
        ));
        fs::remove_dir_all(replacement).unwrap();

        write_status("RUNNING", 100.0, 121.0);
        assert!(matches!(
            observe_controller_start(&paths, &expected, 121.0).unwrap(),
            ControllerStartReadiness::Waiting(reason) if reason.contains("prior process")
        ));

        write_status("RUNNING", 111.0, 121.0);
        assert!(matches!(
            observe_controller_start(&paths, &expected, 200.0).unwrap(),
            ControllerStartReadiness::Waiting(reason) if reason.contains("stale")
        ));

        write_status("ERROR", 111.0, 121.0);
        assert!(observe_controller_start(&paths, &expected, 121.0).is_err());
        fs::set_permissions(&status, fs::Permissions::from_mode(0o666)).unwrap();
        write_status("RUNNING", 111.0, 121.0);
        fs::set_permissions(&status, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(observe_controller_start(&paths, &expected, 121.0).is_err());

        fs::remove_dir_all(&process).unwrap();
        assert_eq!(
            observe_controller_start(&paths, &[], 121.0).unwrap(),
            ControllerStartReadiness::Ready
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(feature = "calibration")]
    fn process_snapshot_includes_controller_and_mqtt_with_the_same_instance() {
        let root = test_root("managed-processes");
        for (pid, argument) in [(100u32, "--instance"), (101u32, "--mqtt-publisher")] {
            let process = root.join(pid.to_string());
            fs::create_dir(&process).unwrap();
            fs::write(
                process.join("cmdline"),
                format!("/usr/sbin/cake-autorated\0{argument}\0wan\0"),
            )
            .unwrap();
            fs::write(
                process.join("stat"),
                format!(
                    "{pid} (cake) S 1 {pid} {pid} 0 0 0 0 0 0 0 0 0 0 0 0 0 1 0 {}\n",
                    pid + 1000
                ),
            )
            .unwrap();
        }
        let processes = discover_controllers(&root).unwrap();
        assert_eq!(processes.len(), 2);
        assert_eq!(processes[0].kind, ServiceProcessKind::Controller);
        assert_eq!(processes[1].kind, ServiceProcessKind::MqttPublisher);
        assert!(processes.iter().all(|value| value.instance == "wan"));
        fs::remove_dir_all(root).unwrap();
    }
}
