//! Exact selected-instance lifecycle used by native Apply.
//!
//! The durable Apply transaction already owns the global lifecycle lock.  This
//! module keeps every state-dependent decision in Rust and leaves rc.common
//! only the mechanical `procd add` operation which OpenWrt itself defines.

use super::autotune_apply_runtime::NativeApplyGlobalLock;
use super::autotune_uci_materialization::verify_scalar_uci_section;
use super::identity::ProcessIdentity;
use super::json_wire::json_escape;
use super::procd_control::delete_service_or_attest_absent;
use super::process::{run_bounded_command_output_with_input, SpawnSpec};
use super::protocol::OperationRequest;
use super::runtime_health::{safe_interface, safe_name, UciPackage, UciSection};
use super::service_config::{InterfaceResolver, OpenWrtEnvironment};
use super::sqm_projection::{apply_sqm_projection, ProjectionScope, SqmProjectionPlan};
use super::sqm_recovery_openwrt::{
    attest_managed_sqm_after_service_action, error_message as sqm_error_message,
    ManagedSqmAttestationSpec,
};
use super::traffic_classifier::run_traffic_classifier;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const CAKE_PACKAGE: &str = "cake-autorate";
const SQM_PACKAGE: &str = "sqm";
const SERVICE_NAME: &str = "cake-autorate";
const DAEMON_PATH: &str = "/usr/sbin/cake-autorated";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const CONTROLLER_STOP_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_OUTPUT: usize = 64 * 1024;
const MAX_CMDLINE: u64 = 4096;
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
static NEXT_SNAPSHOT: AtomicU32 = AtomicU32::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectedLifecycleAction {
    Restart,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectedProjectionState {
    /// The cake-autorate package already contains the selected direction, but
    /// the managed SQM section may still be the exact previous projection.
    /// Only a rate on the direction being bypassed may therefore be stale.
    Pending,
    /// The SQM projection has completed and both directional rates must match
    /// the selected direction exactly.
    Exact,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SelectedConfig {
    cake: UciPackage,
    sqm: UciPackage,
    instance: String,
    sqm_section: String,
    target_interface: String,
    download_interface: String,
    controller_should_run: bool,
    sqm_should_run: bool,
    download_enabled: bool,
    upload_enabled: bool,
    download_kbps: u64,
    upload_kbps: u64,
}

trait SelectedLifecycleBackend {
    fn project(&mut self, instance: &str) -> Result<SqmProjectionPlan, String>;
    fn snapshot(&mut self, projection: SelectedProjectionState) -> Result<SelectedConfig, String>;
    fn attest_unchanged(&mut self, snapshot: &SelectedConfig) -> Result<(), String>;
    fn freeze_sqm(&mut self, snapshot: &SelectedConfig) -> Result<(), String>;
    fn stop_controller(&mut self, instance: &str) -> Result<(), String>;
    fn stop_sqm(&mut self, snapshot: &SelectedConfig) -> Result<(), String>;
    fn prepare_ingress(&mut self, snapshot: &SelectedConfig) -> Result<(), String>;
    fn start_sqm(&mut self, snapshot: &SelectedConfig) -> Result<(), String>;
    fn apply_classifier(&mut self) -> Result<(), String>;
    fn register_controller(&mut self, instance: &str) -> Result<(), String>;
}

fn execute_selected_lifecycle(
    action: SelectedLifecycleAction,
    backend: &mut impl SelectedLifecycleBackend,
) -> Result<(), String> {
    let snapshot = match action {
        SelectedLifecycleAction::Restart => {
            let initial = backend.snapshot(SelectedProjectionState::Pending)?;
            let projection = backend.project(&initial.instance)?;
            if projection.conflicts().contains(&initial.instance) {
                return Err(format!(
                    "native Apply selected instance {} has a duplicate SQM target",
                    initial.instance
                ));
            }
            let projected = backend.snapshot(SelectedProjectionState::Exact)?;
            if projected.sqm_should_run {
                if !projection.is_managed()
                    || projection.interfaces().len() != 1
                    || !projection
                        .interfaces()
                        .contains(&projected.target_interface)
                {
                    return Err(
                        "native Apply SQM projection did not prove the selected active target"
                            .to_string(),
                    );
                }
                if projected.download_enabled
                    && !projection
                        .ingress_interfaces()
                        .contains(&projected.target_interface)
                {
                    return Err(
                        "native Apply SQM projection omitted the selected ingress target"
                            .to_string(),
                    );
                }
            }
            projected
        }
        SelectedLifecycleAction::Stop => backend.snapshot(SelectedProjectionState::Exact)?,
    };

    backend.attest_unchanged(&snapshot)?;
    backend.freeze_sqm(&snapshot)?;
    backend.attest_unchanged(&snapshot)?;
    backend.stop_controller(&snapshot.instance)?;
    backend.attest_unchanged(&snapshot)?;
    backend.stop_sqm(&snapshot)?;
    backend.attest_unchanged(&snapshot)?;

    if action == SelectedLifecycleAction::Stop {
        return Ok(());
    }
    if snapshot.sqm_should_run {
        if snapshot.download_enabled {
            backend.prepare_ingress(&snapshot)?;
            backend.attest_unchanged(&snapshot)?;
        }
        backend.start_sqm(&snapshot)?;
        backend.attest_unchanged(&snapshot)?;
    }
    backend.apply_classifier()?;
    backend.attest_unchanged(&snapshot)?;
    if snapshot.controller_should_run {
        backend.register_controller(&snapshot.instance)?;
        backend.attest_unchanged(&snapshot)?;
    }
    Ok(())
}

struct OpenWrtSelectedLifecycle<'a> {
    request: &'a OperationRequest,
    expected_target: &'a str,
    lock: &'a NativeApplyGlobalLock,
    environment: OpenWrtEnvironment,
    paths: LifecyclePaths,
    sqm_snapshot: Option<PrivateSqmConfig>,
}

#[derive(Clone, Debug)]
struct LifecyclePaths {
    proc_root: PathBuf,
    sys_class_net: PathBuf,
    ubus: PathBuf,
    init: PathBuf,
    sqm_runner: PathBuf,
    sqm_config: PathBuf,
    tc: PathBuf,
    snapshot_root: PathBuf,
}

impl LifecyclePaths {
    fn production() -> Self {
        Self {
            proc_root: env_path("CAKE_AUTORATE_PROC_ROOT", "/proc"),
            sys_class_net: env_path("CAKE_AUTORATE_SYS_CLASS_NET", "/sys/class/net"),
            ubus: env_path("CAKE_AUTORATE_UBUS_BIN", "/bin/ubus"),
            init: env_path("CAKE_AUTORATE_INIT_BIN", "/etc/init.d/cake-autorate"),
            sqm_runner: env_path("CAKE_AUTORATE_SQM_RUNNER", "/usr/lib/sqm/run.sh"),
            sqm_config: env_path("CAKE_AUTORATE_SQM_CONFIG", "/etc/config/sqm"),
            tc: env_path("CAKE_AUTORATE_TC_BIN", "/sbin/tc"),
            snapshot_root: env_path(
                "CAKE_AUTORATE_LIFECYCLE_SNAPSHOT_ROOT",
                "/tmp/cake-autorate-native-lifecycle",
            ),
        }
    }
}

pub(crate) fn run_selected_instance_lifecycle(
    action: SelectedLifecycleAction,
    request: &OperationRequest,
    expected_target: &str,
    lock: &NativeApplyGlobalLock,
) -> Result<(), String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("native Apply selected lifecycle requires root".to_string());
    }
    validate_request_binding(request, expected_target)?;
    let mut backend = OpenWrtSelectedLifecycle {
        request,
        expected_target,
        lock,
        environment: OpenWrtEnvironment::production(),
        paths: LifecyclePaths::production(),
        sqm_snapshot: None,
    };
    execute_selected_lifecycle(action, &mut backend)
}

impl SelectedLifecycleBackend for OpenWrtSelectedLifecycle<'_> {
    fn project(&mut self, instance: &str) -> Result<SqmProjectionPlan, String> {
        apply_sqm_projection(ProjectionScope::Instance(instance.to_string()))
    }

    fn snapshot(&mut self, projection: SelectedProjectionState) -> Result<SelectedConfig, String> {
        let cake = self.environment.read_package(CAKE_PACKAGE)?;
        let sqm = self.environment.read_package(SQM_PACKAGE)?;
        selected_config(
            &cake,
            &sqm,
            &self.environment,
            self.request,
            self.expected_target,
            projection,
        )
    }

    fn attest_unchanged(&mut self, snapshot: &SelectedConfig) -> Result<(), String> {
        let cake = self.environment.read_package(CAKE_PACKAGE)?;
        let sqm = self.environment.read_package(SQM_PACKAGE)?;
        if cake != snapshot.cake || sqm != snapshot.sqm {
            return Err("native Apply selected lifecycle UCI changed during mutation".to_string());
        }
        if let Some(frozen) = self.sqm_snapshot.as_ref() {
            frozen.attest_source_unchanged(&self.paths)?;
        }
        Ok(())
    }

    fn freeze_sqm(&mut self, snapshot: &SelectedConfig) -> Result<(), String> {
        if self.sqm_snapshot.is_some() {
            return Err("native Apply selected lifecycle SQM config was frozen twice".to_string());
        }
        self.sqm_snapshot = Some(PrivateSqmConfig::capture(&self.paths, snapshot)?);
        Ok(())
    }

    fn stop_controller(&mut self, instance: &str) -> Result<(), String> {
        let observed = find_controller(&self.paths.proc_root, instance)?;
        let pidfd = observed
            .as_ref()
            .map(|identity| open_stable_pidfd(&self.paths.proc_root, identity))
            .transpose()?;
        let request = format!(
            "{{\"name\":\"{}\",\"instance\":\"{}\"}}",
            SERVICE_NAME,
            json_escape(instance)
        );
        delete_service_or_attest_absent(
            &self.paths.ubus,
            &request,
            "delete the selected procd controller",
        )?;
        if let Some(pidfd) = pidfd.as_ref() {
            wait_pidfd(pidfd, CONTROLLER_STOP_TIMEOUT)?;
        }
        if find_controller(&self.paths.proc_root, instance)?.is_some() {
            return Err(format!(
                "native Apply controller {instance} remained after exact procd deletion"
            ));
        }
        Ok(())
    }

    fn stop_sqm(&mut self, snapshot: &SelectedConfig) -> Result<(), String> {
        let config = self.sqm_snapshot.as_ref().ok_or_else(|| {
            "native Apply selected lifecycle has no frozen SQM config".to_string()
        })?;
        let _output = run_command(
            &SpawnSpec {
                program: self.paths.sqm_runner.clone(),
                arguments: vec![
                    OsString::from("stop"),
                    OsString::from(&snapshot.target_interface),
                ],
                environment: vec![(
                    OsString::from("UCI_CONFIG_DIR"),
                    config.directory.as_os_str().to_os_string(),
                )],
            },
            None,
            COMMAND_TIMEOUT,
            |_| {},
        )?;
        // sqm-scripts status is not authoritative in either direction.  Stop
        // succeeds only when the exact selected runtime is absent.
        attest_selected_sqm_absent(&self.paths, snapshot)
    }

    fn prepare_ingress(&mut self, snapshot: &SelectedConfig) -> Result<(), String> {
        remove_empty_clsact(&self.paths, &snapshot.target_interface)
    }

    fn start_sqm(&mut self, snapshot: &SelectedConfig) -> Result<(), String> {
        let config = self.sqm_snapshot.as_ref().ok_or_else(|| {
            "native Apply selected lifecycle has no frozen SQM config".to_string()
        })?;
        let output = run_command(
            &SpawnSpec {
                program: self.paths.sqm_runner.clone(),
                arguments: vec![
                    OsString::from("start"),
                    OsString::from(&snapshot.target_interface),
                ],
                environment: vec![(
                    OsString::from("UCI_CONFIG_DIR"),
                    config.directory.as_os_str().to_os_string(),
                )],
            },
            None,
            COMMAND_TIMEOUT,
            |_| {},
        )?;
        let spec = ManagedSqmAttestationSpec {
            instance: snapshot.instance.clone(),
            sqm_section: snapshot.sqm_section.clone(),
            target_interface: snapshot.target_interface.clone(),
            upload_interface: snapshot.target_interface.clone(),
            download_interface: snapshot.download_interface.clone(),
            direction_mode: if snapshot.download_enabled && snapshot.upload_enabled {
                "both"
            } else if snapshot.download_enabled {
                "download_only"
            } else {
                "upload_only"
            }
            .to_string(),
            minimum_download_kbps: snapshot.download_kbps,
            maximum_download_kbps: snapshot.download_kbps,
            minimum_upload_kbps: snapshot.upload_kbps,
            maximum_upload_kbps: snapshot.upload_kbps,
        };
        match attest_managed_sqm_after_service_action(&spec) {
            Ok(()) => Ok(()),
            Err(postcondition) => {
                let command = if output.status.success() {
                    "SQM start returned success".to_string()
                } else {
                    format!("SQM start failed: {}", stderr(&output.stderr))
                };
                Err(format!(
                    "{command}, but the exact selected runtime postcondition failed: {}",
                    sqm_error_message(&postcondition)
                ))
            }
        }
    }

    fn apply_classifier(&mut self) -> Result<(), String> {
        run_traffic_classifier(["apply".to_string()].into_iter()).map(|_| ())
    }

    fn register_controller(&mut self, instance: &str) -> Result<(), String> {
        run_success(
            &SpawnSpec {
                program: self.paths.init.clone(),
                arguments: vec![
                    OsString::from("native_apply_register_instance"),
                    OsString::from(instance),
                ],
                environment: vec![(
                    OsString::from("CAKE_AUTORATE_NATIVE_APPLY_RECOVERY"),
                    OsString::from("1"),
                )],
            },
            None,
            COMMAND_TIMEOUT,
            |command| self.lock.configure_borrowed_restart(command),
            "register the selected procd controller",
        )
    }
}

fn validate_request_binding(
    request: &OperationRequest,
    expected_target: &str,
) -> Result<(), String> {
    if !safe_name(&request.identity.instance)
        || !safe_interface(expected_target)
        || match request.managed_sqm_section.as_deref() {
            Some(section) => !safe_name(section),
            None => true,
        }
    {
        return Err("native Apply selected lifecycle identity is unsafe".to_string());
    }
    Ok(())
}

fn selected_config(
    cake: &UciPackage,
    sqm: &UciPackage,
    resolver: &impl InterfaceResolver,
    request: &OperationRequest,
    expected_target: &str,
    projection: SelectedProjectionState,
) -> Result<SelectedConfig, String> {
    let instance = &request.identity.instance;
    let expected_sqm = request
        .managed_sqm_section
        .as_deref()
        .ok_or_else(|| "native Apply request has no managed SQM section".to_string())?;
    let section = cake
        .sections
        .get(instance)
        .filter(|section| section.section_type == "cake_autorate")
        .ok_or_else(|| format!("native Apply instance {instance} is missing"))?;
    let configured_sqm = option(section, "sqm_section")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("cake_{instance}"));
    if configured_sqm != expected_sqm {
        return Err("native Apply instance-to-SQM binding changed".to_string());
    }
    let configured_target = first_option(section, &["sqm_interface", "ul_if", "wan_if"])
        .ok_or_else(|| "native Apply selected instance has no target interface".to_string())?;
    let resolved_target = resolver.resolve(&configured_target)?;
    if configured_target != expected_target && resolved_target != expected_target {
        return Err("native Apply selected target binding changed".to_string());
    }
    let manage_sqm = bool_option(section, "manage_sqm", true);
    let enabled = bool_option(section, "enabled", false);
    let sqm_enabled = bool_option(section, "sqm_enabled", enabled);
    let direction = option(section, "sqm_direction_mode")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "both".to_string());
    if !matches!(
        direction.as_str(),
        "both" | "download_only" | "upload_only" | "off"
    ) {
        return Err("native Apply selected SQM direction is invalid".to_string());
    }
    let sqm_should_run = manage_sqm && enabled && sqm_enabled && direction != "off";
    let controller_should_run = enabled && (!manage_sqm || sqm_should_run);
    let download_enabled = sqm_should_run && direction != "upload_only";
    let upload_enabled = sqm_should_run && direction != "download_only";
    let download_interface = option(section, "dl_if")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("ifb4{expected_target}"));
    let mut download_kbps = 0;
    let mut upload_kbps = 0;

    if let Some(queue) = sqm.sections.get(expected_sqm) {
        if queue.section_type != "queue" {
            return Err("native Apply selected SQM section type changed".to_string());
        }
        if option(queue, "_cake_autorate_managed").as_deref() != Some(instance.as_str()) {
            return Err("native Apply selected SQM owner changed".to_string());
        }
        if let Some(target) = option(queue, "interface").filter(|value| !value.is_empty()) {
            let resolved = resolver.resolve(&target)?;
            if target != expected_target && resolved != expected_target {
                return Err("native Apply selected SQM target changed".to_string());
            }
        } else if sqm_should_run {
            return Err("native Apply active SQM section has no interface".to_string());
        }
        if sqm_should_run {
            if option(queue, "enabled").as_deref() != Some("1") {
                return Err("native Apply active SQM section is disabled".to_string());
            }
            download_kbps = parse_rate(queue, "download")?;
            upload_kbps = parse_rate(queue, "upload")?;
            validate_selected_rates(
                projection,
                download_enabled,
                upload_enabled,
                download_kbps,
                upload_kbps,
            )?;
        }
    } else if sqm_should_run {
        return Err("native Apply active SQM section is missing".to_string());
    }

    Ok(SelectedConfig {
        cake: cake.clone(),
        sqm: sqm.clone(),
        instance: instance.clone(),
        sqm_section: expected_sqm.to_string(),
        target_interface: expected_target.to_string(),
        download_interface,
        controller_should_run,
        sqm_should_run,
        download_enabled,
        upload_enabled,
        download_kbps,
        upload_kbps,
    })
}

fn validate_selected_rates(
    projection: SelectedProjectionState,
    download_enabled: bool,
    upload_enabled: bool,
    download_kbps: u64,
    upload_kbps: u64,
) -> Result<(), String> {
    if (download_enabled && download_kbps == 0) || (upload_enabled && upload_kbps == 0) {
        return Err("native Apply selected active SQM rate is missing".to_string());
    }
    if projection == SelectedProjectionState::Exact
        && ((!download_enabled && download_kbps != 0) || (!upload_enabled && upload_kbps != 0))
    {
        return Err("native Apply projected SQM rates do not match its direction".to_string());
    }
    Ok(())
}

fn find_controller(proc_root: &Path, instance: &str) -> Result<Option<ProcessIdentity>, String> {
    let mut found = None;
    for entry in fs::read_dir(proc_root)
        .map_err(|error| format!("unable to enumerate controller processes: {error}"))?
    {
        let entry = entry.map_err(|error| format!("unable to inspect process entry: {error}"))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        if pid <= 1 || !controller_cmdline_matches(&entry.path().join("cmdline"), instance)? {
            continue;
        }
        let identity = ProcessIdentity::inspect(proc_root, pid)?;
        if found.replace(identity).is_some() {
            return Err(format!(
                "multiple exact controllers are running for native Apply instance {instance}"
            ));
        }
    }
    Ok(found)
}

fn controller_cmdline_matches(path: &Path, instance: &str) -> Result<bool, String> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
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
    let mut expected = Vec::with_capacity(DAEMON_PATH.len() + instance.len() + 13);
    expected.extend_from_slice(DAEMON_PATH.as_bytes());
    expected.push(0);
    expected.extend_from_slice(b"--instance");
    expected.push(0);
    expected.extend_from_slice(instance.as_bytes());
    expected.push(0);
    if bytes.len() > MAX_CMDLINE as usize {
        if bytes.starts_with(&expected) {
            return Err("controller command line exceeds its safety bound".to_string());
        }
        return Ok(false);
    }
    Ok(bytes == expected)
}

fn open_stable_pidfd(proc_root: &Path, identity: &ProcessIdentity) -> Result<OwnedFd, String> {
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, identity.pid, 0) };
    if raw < 0 {
        return Err(format!(
            "unable to open pidfd for selected controller: {}",
            io::Error::last_os_error()
        ));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    if !identity.still_matches(proc_root)? {
        return Err("selected controller identity changed while opening pidfd".to_string());
    }
    Ok(fd)
}

fn wait_pidfd(pidfd: &OwnedFd, timeout: Duration) -> Result<(), String> {
    let started = Instant::now();
    loop {
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(
                "selected controller did not stop before its watchdog deadline".to_string(),
            );
        }
        let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd: pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if result > 0 && descriptor.revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            return Ok(());
        }
        if result == 0 {
            return Err(
                "selected controller did not stop before its watchdog deadline".to_string(),
            );
        }
        if result < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(format!(
            "unable to wait for selected controller exit: {}",
            io::Error::last_os_error()
        ));
    }
}

fn attest_selected_sqm_absent(
    paths: &LifecyclePaths,
    snapshot: &SelectedConfig,
) -> Result<(), String> {
    if netdev_exists(paths, &snapshot.target_interface) {
        let output = tc_output(paths, &["qdisc", "show", "dev", &snapshot.target_interface])?;
        if output.lines().any(|line| {
            let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
            fields.first() == Some(&"qdisc")
                && matches!(
                    fields.get(1),
                    Some(&"cake") | Some(&"cake_mq") | Some(&"ingress") | Some(&"clsact")
                )
        }) {
            return Err("selected SQM upload or ingress runtime remains after stop".to_string());
        }
    }
    if netdev_exists(paths, &snapshot.download_interface) {
        let output = tc_output(
            paths,
            &["qdisc", "show", "dev", &snapshot.download_interface],
        )?;
        if output.lines().any(|line| {
            let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
            fields.first() == Some(&"qdisc")
                && matches!(fields.get(1), Some(&"cake") | Some(&"cake_mq"))
        }) {
            return Err("selected SQM download runtime remains after stop".to_string());
        }
    }
    Ok(())
}

fn remove_empty_clsact(paths: &LifecyclePaths, interface: &str) -> Result<(), String> {
    if !netdev_exists(paths, interface) {
        return Ok(());
    }
    let qdiscs = tc_output(paths, &["qdisc", "show", "dev", interface])?;
    if !qdiscs.lines().any(|line| {
        let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
        fields.first() == Some(&"qdisc") && fields.get(1) == Some(&"clsact")
    }) {
        return Ok(());
    }
    for hook in ["ingress", "egress"] {
        if !tc_output(paths, &["filter", "show", "dev", interface, hook])?
            .trim()
            .is_empty()
        {
            return Err(format!(
                "cannot replace clsact on selected SQM interface {interface}: foreign filters remain"
            ));
        }
    }
    run_success(
        &SpawnSpec {
            program: paths.tc.clone(),
            arguments: ["qdisc", "del", "dev", interface, "clsact"]
                .into_iter()
                .map(OsString::from)
                .collect(),
            environment: Vec::new(),
        },
        None,
        COMMAND_TIMEOUT,
        |_| {},
        "remove an empty selected clsact",
    )
}

fn tc_output(paths: &LifecyclePaths, arguments: &[&str]) -> Result<String, String> {
    let output = run_command(
        &SpawnSpec {
            program: paths.tc.clone(),
            arguments: arguments.iter().map(OsString::from).collect(),
            environment: Vec::new(),
        },
        None,
        COMMAND_TIMEOUT,
        |_| {},
    )?;
    if !output.status.success() {
        return Err(format!("tc inspection failed: {}", stderr(&output.stderr)));
    }
    String::from_utf8(output.stdout).map_err(|_| "tc inspection output is not UTF-8".to_string())
}

fn netdev_exists(paths: &LifecyclePaths, interface: &str) -> bool {
    paths.sys_class_net.join(interface).is_dir()
}

struct PrivateSqmConfig {
    directory: PathBuf,
    source_bytes: Vec<u8>,
}

impl PrivateSqmConfig {
    fn capture(paths: &LifecyclePaths, snapshot: &SelectedConfig) -> Result<Self, String> {
        ensure_private_directory(&paths.snapshot_root)?;
        let id = NEXT_SNAPSHOT.fetch_add(1, Ordering::Relaxed);
        let directory = paths
            .snapshot_root
            .join(format!("{}-{id}", std::process::id()));
        fs::create_dir(&directory)
            .map_err(|error| format!("unable to create private SQM lifecycle snapshot: {error}"))?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(|error| {
            format!("unable to protect private SQM lifecycle snapshot: {error}")
        })?;
        let bytes = read_sqm_config_bytes(paths)?;
        let expected = snapshot
            .sqm
            .sections
            .get(&snapshot.sqm_section)
            .map(|section| (section.section_type.as_str(), &section.options));
        verify_scalar_uci_section(&bytes, &snapshot.sqm_section, expected).map_err(|error| {
            format!("native Apply merged SQM state differs from committed config: {error}")
        })?;
        let target = directory.join("sqm");
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&target)
            .map_err(|error| format!("unable to create private SQM lifecycle config: {error}"))?;
        output
            .write_all(&bytes)
            .and_then(|_| output.sync_all())
            .map_err(|error| format!("unable to publish private SQM lifecycle config: {error}"))?;
        Ok(Self {
            directory,
            source_bytes: bytes,
        })
    }

    fn attest_source_unchanged(&self, paths: &LifecyclePaths) -> Result<(), String> {
        if read_sqm_config_bytes(paths)? != self.source_bytes {
            return Err(
                "native Apply committed SQM config changed during selected lifecycle".to_string(),
            );
        }
        Ok(())
    }
}

impl Drop for PrivateSqmConfig {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.directory.join("sqm"));
        let _ = fs::remove_dir(&self.directory);
    }
}

fn ensure_private_directory(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path)
        .map_err(|error| format!("unable to create lifecycle runtime directory: {error}"))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("unable to inspect lifecycle runtime directory: {error}"))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err("lifecycle runtime directory is unsafe".to_string());
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("unable to protect lifecycle runtime directory: {error}"))
}

fn read_sqm_config_bytes(paths: &LifecyclePaths) -> Result<Vec<u8>, String> {
    let source = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&paths.sqm_config)
        .map_err(|error| format!("unable to open SQM config for lifecycle snapshot: {error}"))?;
    let metadata = source
        .metadata()
        .map_err(|error| format!("unable to inspect SQM config: {error}"))?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.len() > MAX_CONFIG_BYTES
    {
        return Err("SQM config is unsafe for lifecycle snapshot".to_string());
    }
    let mut bytes = Vec::new();
    source
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("unable to read SQM lifecycle snapshot: {error}"))?;
    if bytes.len() > MAX_CONFIG_BYTES as usize {
        return Err("SQM lifecycle snapshot exceeds its size bound".to_string());
    }
    Ok(bytes)
}

fn run_command<C>(
    spec: &SpawnSpec,
    input: Option<&[u8]>,
    timeout: Duration,
    configure: C,
) -> Result<super::process::BoundedCommandOutput, String>
where
    C: FnOnce(&mut std::process::Command),
{
    run_bounded_command_output_with_input(spec, input, timeout, MAX_OUTPUT, || false, configure)
}

fn run_success<C>(
    spec: &SpawnSpec,
    input: Option<&[u8]>,
    timeout: Duration,
    configure: C,
    operation: &str,
) -> Result<(), String>
where
    C: FnOnce(&mut std::process::Command),
{
    let output = run_command(spec, input, timeout, configure)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("unable to {operation}: {}", stderr(&output.stderr)))
    }
}

fn stderr(bytes: &[u8]) -> String {
    let value = String::from_utf8_lossy(bytes).trim().to_string();
    if value.is_empty() {
        "command exited unsuccessfully".to_string()
    } else {
        value
    }
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
        .ok_or_else(|| format!("native Apply SQM option {key} is missing"))?
        .parse::<u64>()
        .ok()
        .filter(|value| *value <= 10_000_000_000)
        .ok_or_else(|| format!("native Apply SQM option {key} is invalid"))
}

fn env_path(name: &str, default: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeBackend {
        events: Vec<&'static str>,
        snapshots: Vec<SelectedConfig>,
        projection: SqmProjectionPlan,
        fail_attestation_at: usize,
        attestations: usize,
    }

    impl SelectedLifecycleBackend for FakeBackend {
        fn project(&mut self, _: &str) -> Result<SqmProjectionPlan, String> {
            self.events.push("project");
            Ok(self.projection.clone())
        }
        fn snapshot(
            &mut self,
            projection: SelectedProjectionState,
        ) -> Result<SelectedConfig, String> {
            self.events.push(match projection {
                SelectedProjectionState::Pending => "snapshot-pending",
                SelectedProjectionState::Exact => "snapshot-exact",
            });
            let snapshot = if self.snapshots.len() > 1 {
                self.snapshots.remove(0)
            } else {
                self.snapshots[0].clone()
            };
            if snapshot.sqm_should_run {
                validate_selected_rates(
                    projection,
                    snapshot.download_enabled,
                    snapshot.upload_enabled,
                    snapshot.download_kbps,
                    snapshot.upload_kbps,
                )?;
            }
            Ok(snapshot)
        }
        fn attest_unchanged(&mut self, _: &SelectedConfig) -> Result<(), String> {
            self.events.push("attest");
            self.attestations += 1;
            if self.fail_attestation_at == self.attestations {
                Err("drift".to_string())
            } else {
                Ok(())
            }
        }
        fn freeze_sqm(&mut self, _: &SelectedConfig) -> Result<(), String> {
            self.events.push("freeze-sqm");
            Ok(())
        }
        fn stop_controller(&mut self, _: &str) -> Result<(), String> {
            self.events.push("stop-controller");
            Ok(())
        }
        fn stop_sqm(&mut self, _: &SelectedConfig) -> Result<(), String> {
            self.events.push("stop-sqm");
            Ok(())
        }
        fn prepare_ingress(&mut self, _: &SelectedConfig) -> Result<(), String> {
            self.events.push("prepare-ingress");
            Ok(())
        }
        fn start_sqm(&mut self, _: &SelectedConfig) -> Result<(), String> {
            self.events.push("start-sqm");
            Ok(())
        }
        fn apply_classifier(&mut self) -> Result<(), String> {
            self.events.push("classifier");
            Ok(())
        }
        fn register_controller(&mut self, _: &str) -> Result<(), String> {
            self.events.push("register");
            Ok(())
        }
    }

    fn config(active: bool, download: bool, controller: bool) -> SelectedConfig {
        SelectedConfig {
            cake: UciPackage::default(),
            sqm: UciPackage::default(),
            instance: "wan_sqm".to_string(),
            sqm_section: "cake_wan_sqm".to_string(),
            target_interface: "pppoe-wan".to_string(),
            download_interface: "ifb4pppoe-wan".to_string(),
            controller_should_run: controller,
            sqm_should_run: active,
            download_enabled: download,
            upload_enabled: active,
            download_kbps: if active && download { 80_000 } else { 0 },
            upload_kbps: if active { 20_000 } else { 0 },
        }
    }

    fn projection(active: bool, download: bool) -> SqmProjectionPlan {
        let mut interfaces = std::collections::BTreeSet::new();
        let mut ingress = std::collections::BTreeSet::new();
        if active {
            interfaces.insert("pppoe-wan".to_string());
        }
        if download {
            ingress.insert("pppoe-wan".to_string());
        }
        SqmProjectionPlan::test_summary(
            active,
            interfaces,
            ingress,
            std::collections::BTreeSet::new(),
        )
    }

    #[test]
    fn restart_order_is_state_driven_and_fenced_between_mutations() {
        let cfg = config(true, true, true);
        let mut backend = FakeBackend {
            snapshots: vec![cfg.clone(), cfg],
            projection: projection(true, true),
            ..FakeBackend::default()
        };
        execute_selected_lifecycle(SelectedLifecycleAction::Restart, &mut backend).unwrap();
        assert_eq!(
            backend.events,
            [
                "snapshot-pending",
                "project",
                "snapshot-exact",
                "attest",
                "freeze-sqm",
                "attest",
                "stop-controller",
                "attest",
                "stop-sqm",
                "attest",
                "prepare-ingress",
                "attest",
                "start-sqm",
                "attest",
                "classifier",
                "attest",
                "register",
                "attest"
            ]
        );
    }

    #[test]
    fn disabled_candidate_stops_runtime_without_restarting_sqm_or_controller() {
        let cfg = config(false, false, false);
        let mut backend = FakeBackend {
            snapshots: vec![cfg.clone(), cfg],
            projection: projection(false, false),
            ..FakeBackend::default()
        };
        execute_selected_lifecycle(SelectedLifecycleAction::Restart, &mut backend).unwrap();
        assert!(!backend.events.contains(&"start-sqm"));
        assert!(!backend.events.contains(&"register"));
        assert!(backend.events.contains(&"classifier"));
    }

    #[test]
    fn upload_only_candidate_projects_stale_download_before_touching_runtime() {
        let exact = config(true, false, true);
        let mut pending = exact.clone();
        pending.download_kbps = 80_000;
        let mut backend = FakeBackend {
            snapshots: vec![pending, exact],
            projection: projection(true, false),
            ..FakeBackend::default()
        };
        execute_selected_lifecycle(SelectedLifecycleAction::Restart, &mut backend).unwrap();
        assert_eq!(
            &backend.events[..3],
            ["snapshot-pending", "project", "snapshot-exact"]
        );
        assert!(!backend.events.contains(&"prepare-ingress"));
        assert!(backend.events.contains(&"start-sqm"));
    }

    #[test]
    fn pending_directional_projection_allows_only_the_stale_bypassed_rate() {
        validate_selected_rates(
            SelectedProjectionState::Pending,
            false,
            true,
            80_000,
            20_000,
        )
        .unwrap();
        assert!(validate_selected_rates(
            SelectedProjectionState::Exact,
            false,
            true,
            80_000,
            20_000,
        )
        .unwrap_err()
        .contains("projected SQM rates"));
        validate_selected_rates(SelectedProjectionState::Exact, false, true, 0, 20_000).unwrap();

        validate_selected_rates(
            SelectedProjectionState::Pending,
            true,
            false,
            80_000,
            20_000,
        )
        .unwrap();
        assert!(validate_selected_rates(
            SelectedProjectionState::Exact,
            true,
            false,
            80_000,
            20_000,
        )
        .unwrap_err()
        .contains("projected SQM rates"));
        validate_selected_rates(SelectedProjectionState::Exact, true, false, 80_000, 0).unwrap();

        assert!(
            validate_selected_rates(SelectedProjectionState::Pending, false, true, 80_000, 0,)
                .unwrap_err()
                .contains("active SQM rate")
        );
        assert!(
            validate_selected_rates(SelectedProjectionState::Pending, true, false, 0, 20_000,)
                .unwrap_err()
                .contains("active SQM rate")
        );
    }

    #[test]
    fn configuration_drift_stops_before_the_next_mutation() {
        let cfg = config(true, true, true);
        let mut backend = FakeBackend {
            snapshots: vec![cfg.clone(), cfg],
            projection: projection(true, true),
            fail_attestation_at: 3,
            ..FakeBackend::default()
        };
        assert!(
            execute_selected_lifecycle(SelectedLifecycleAction::Restart, &mut backend).is_err()
        );
        assert!(backend.events.contains(&"stop-controller"));
        assert!(!backend.events.contains(&"stop-sqm"));
    }

    #[test]
    fn stop_action_never_projects_or_starts_any_runtime() {
        let cfg = config(true, true, true);
        let mut backend = FakeBackend {
            snapshots: vec![cfg],
            ..FakeBackend::default()
        };
        execute_selected_lifecycle(SelectedLifecycleAction::Stop, &mut backend).unwrap();
        assert_eq!(
            backend.events,
            [
                "snapshot-exact",
                "attest",
                "freeze-sqm",
                "attest",
                "stop-controller",
                "attest",
                "stop-sqm",
                "attest"
            ]
        );
    }

    #[test]
    fn exact_controller_cmdline_rejects_prefixes_and_extra_arguments() {
        let root = std::env::temp_dir().join(format!(
            "cake-selected-lifecycle-cmdline-{}-{}",
            std::process::id(),
            NEXT_SNAPSHOT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        for (name, bytes, expected) in [
            (
                "exact",
                b"/usr/sbin/cake-autorated\0--instance\0wan_sqm\0".as_slice(),
                true,
            ),
            (
                "prefix",
                b"/usr/sbin/cake-autorated-helper\0--instance\0wan_sqm\0".as_slice(),
                false,
            ),
            (
                "extra",
                b"/usr/sbin/cake-autorated\0--instance\0wan_sqm\0--once\0".as_slice(),
                false,
            ),
            (
                "extra-empty",
                b"/usr/sbin/cake-autorated\0--instance\0wan_sqm\0\0".as_slice(),
                false,
            ),
        ] {
            let path = root.join(name);
            fs::write(&path, bytes).unwrap();
            assert_eq!(
                controller_cmdline_matches(&path, "wan_sqm").unwrap(),
                expected
            );
        }
        let unrelated = root.join("unrelated-long");
        let mut unrelated_cmdline = b"/usr/bin/apcontroller\0".to_vec();
        unrelated_cmdline.extend(std::iter::repeat_n(b'x', MAX_CMDLINE as usize + 32));
        unrelated_cmdline.push(0);
        fs::write(&unrelated, unrelated_cmdline).unwrap();
        assert!(!controller_cmdline_matches(&unrelated, "wan_sqm").unwrap());

        let oversized = root.join("owned-long");
        let mut oversized_cmdline = b"/usr/sbin/cake-autorated\0--instance\0wan_sqm\0".to_vec();
        oversized_cmdline.extend(std::iter::repeat_n(b'y', MAX_CMDLINE as usize + 32));
        fs::write(&oversized, oversized_cmdline).unwrap();
        assert!(controller_cmdline_matches(&oversized, "wan_sqm")
            .unwrap_err()
            .contains("exceeds its safety bound"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn private_sqm_snapshot_is_single_source_and_rejects_merged_view_drift() {
        let root = std::env::temp_dir().join(format!(
            "cake-selected-lifecycle-sqm-{}-{}",
            std::process::id(),
            NEXT_SNAPSHOT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let sqm_config = root.join("sqm");
        fs::write(
            &sqm_config,
            b"config queue 'cake_wan_sqm'\n\toption enabled '1'\n\toption interface 'pppoe-wan'\n",
        )
        .unwrap();
        let paths = LifecyclePaths {
            proc_root: root.join("proc"),
            sys_class_net: root.join("sys"),
            ubus: root.join("ubus"),
            init: root.join("init"),
            sqm_runner: root.join("run.sh"),
            sqm_config: sqm_config.clone(),
            tc: root.join("tc"),
            snapshot_root: root.join("snapshots"),
        };
        let mut snapshot = config(false, false, false);
        snapshot.sqm.sections.insert(
            "cake_wan_sqm".to_string(),
            UciSection {
                section_type: "queue".to_string(),
                options: std::collections::BTreeMap::from([
                    ("enabled".to_string(), "1".to_string()),
                    ("interface".to_string(), "pppoe-wan".to_string()),
                ]),
            },
        );

        let frozen = PrivateSqmConfig::capture(&paths, &snapshot).unwrap();
        frozen.attest_source_unchanged(&paths).unwrap();
        fs::write(
            &sqm_config,
            b"config queue 'cake_wan_sqm'\n\toption enabled '0'\n\toption interface 'pppoe-wan'\n",
        )
        .unwrap();
        assert!(frozen.attest_source_unchanged(&paths).is_err());
        drop(frozen);

        assert!(PrivateSqmConfig::capture(&paths, &snapshot).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
