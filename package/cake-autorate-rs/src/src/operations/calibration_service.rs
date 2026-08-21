use super::{
    coordinator::{
        attest_native_apply_idle_for_service_stop, native_autotune_apply_recovery,
        probe_calibration_coordinator_control, PRODUCTION_CALIBRATION_STATE_DIR,
    },
    event_loop::CalibrationEventLoop,
    identity::ProcessIdentity,
    scheduler_owner::{
        attest_scheduler_owner_lock_held, SchedulerOwnerLock, PRODUCTION_SCHEDULER_OWNER_LOCK,
    },
    scheduler_runtime::attest_no_competing_calibration_processes,
};
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, Instant};

const CALIBRATION_SERVICE_START_V1: &str = "cake-autorate-calibration-service\t1\tcoordinator";
const CALIBRATION_SERVICE_DEFERRED_V1: &str = "cake-autorate-calibration-service\t1\tdeferred";
const CALIBRATION_SERVICE_STOP_READY_V1: &str = "cake-autorate-calibration-service\t1\tstop-ready";
const CALIBRATION_SERVICE_STARTED_V1: &str = "cake-autorate-calibration-service\t1\tready";
const CALIBRATION_SERVICE_STOPPED_V1: &str = "cake-autorate-calibration-service\t1\tstopped";
const PRODUCTION_PROC_ROOT: &str = "/proc";
const CONTROL_SOCKET_NAME: &[u8] = b"control.sock";
const CALIBRATION_STOP_TIMEOUT: Duration = Duration::from_secs(10);
const CALIBRATION_START_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PROC_ENTRIES: usize = 65_536;
const MAX_CMDLINE_BYTES: usize = 8 * 1024;

trait CalibrationServiceBackend {
    fn prepare_stop(&mut self) -> Result<(), String>;
    fn confirm_started(&mut self) -> Result<(), String>;
    fn confirm_stopped(&mut self) -> Result<(), String>;
    fn acquire_scheduler_owner(&mut self) -> Result<(), String>;
    fn recover_native_apply(&mut self) -> Result<(), String>;
}

struct OpenWrtCalibrationServiceBackend {
    owner_lock: Option<SchedulerOwnerLock>,
}

impl CalibrationServiceBackend for OpenWrtCalibrationServiceBackend {
    fn prepare_stop(&mut self) -> Result<(), String> {
        if self.owner_lock.is_some() {
            return Err("calibration service stop preparation already holds ownership".to_string());
        }
        attest_native_apply_idle_for_service_stop()
    }

    fn confirm_started(&mut self) -> Result<(), String> {
        if self.owner_lock.is_some() {
            return Err(
                "calibration service start confirmation already holds ownership".to_string(),
            );
        }
        confirm_calibration_service_started(
            Path::new(PRODUCTION_PROC_ROOT),
            Path::new(PRODUCTION_CALIBRATION_STATE_DIR),
            Path::new(PRODUCTION_SCHEDULER_OWNER_LOCK),
            CALIBRATION_START_TIMEOUT,
        )
    }

    fn confirm_stopped(&mut self) -> Result<(), String> {
        if self.owner_lock.is_some() {
            return Err(
                "calibration service stop confirmation already holds ownership".to_string(),
            );
        }
        confirm_calibration_service_stopped(
            Path::new(PRODUCTION_PROC_ROOT),
            Path::new(PRODUCTION_SCHEDULER_OWNER_LOCK),
            CALIBRATION_STOP_TIMEOUT,
        )
    }

    fn acquire_scheduler_owner(&mut self) -> Result<(), String> {
        if self.owner_lock.is_some() {
            return Err("calibration service scheduler owner lock is already held".to_string());
        }
        self.owner_lock = Some(SchedulerOwnerLock::open(Path::new(
            PRODUCTION_SCHEDULER_OWNER_LOCK,
        ))?);
        Ok(())
    }

    fn recover_native_apply(&mut self) -> Result<(), String> {
        native_autotune_apply_recovery().map(|_| ())
    }
}

pub(crate) fn run_calibration_service<I>(mut args: I) -> Result<String, String>
where
    I: Iterator<Item = String>,
{
    if unsafe { libc::geteuid() } != 0 {
        return Err("calibration service lifecycle requires root".to_string());
    }
    let operation = args.next();
    if args.next().is_some() {
        return Err("calibration-service received unexpected arguments".to_string());
    }
    run_calibration_service_with_backend(
        operation.as_deref(),
        &mut OpenWrtCalibrationServiceBackend { owner_lock: None },
    )
}

fn run_calibration_service_with_backend(
    operation: Option<&str>,
    backend: &mut impl CalibrationServiceBackend,
) -> Result<String, String> {
    match operation {
        Some("prepare-start") => {
            if calibration_package_upgrade_mode()? {
                return Ok(format!("{CALIBRATION_SERVICE_DEFERRED_V1}\n"));
            }
            prepare_calibration_service(backend)?;
            Ok(format!("{CALIBRATION_SERVICE_START_V1}\n"))
        }
        Some("prepare-stop") => {
            backend.prepare_stop()?;
            Ok(format!("{CALIBRATION_SERVICE_STOP_READY_V1}\n"))
        }
        Some("confirm-started") => {
            backend.confirm_started()?;
            Ok(format!("{CALIBRATION_SERVICE_STARTED_V1}\n"))
        }
        Some("confirm-stopped") => {
            backend.confirm_stopped()?;
            Ok(format!("{CALIBRATION_SERVICE_STOPPED_V1}\n"))
        }
        _ => Err(
            "calibration-service requires prepare-start, prepare-stop, confirm-started or confirm-stopped"
                .to_string(),
        ),
    }
}

fn calibration_package_upgrade_mode() -> Result<bool, String> {
    calibration_package_upgrade_mode_value(std::env::var_os("PKG_UPGRADE").as_deref())
}

fn calibration_package_upgrade_mode_value(value: Option<&std::ffi::OsStr>) -> Result<bool, String> {
    match value {
        None => Ok(false),
        Some(value) if value.is_empty() || value == "0" => Ok(false),
        Some(value) if value == "1" => Ok(true),
        Some(_) => Err("PKG_UPGRADE must be empty, 0, or 1".to_string()),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CalibrationProcessKind {
    NativeCoordinator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CalibrationProcess {
    identity: ProcessIdentity,
    kind: CalibrationProcessKind,
}

enum StartedObservation {
    Ready,
    Waiting(Vec<ProcessIdentity>),
}

fn confirm_calibration_service_started(
    proc_root: &Path,
    state_dir: &Path,
    owner_lock_path: &Path,
    timeout: Duration,
) -> Result<(), String> {
    if timeout.is_zero() {
        return Err("calibration service start confirmation has a zero deadline".to_string());
    }
    let parent = state_dir
        .parent()
        .ok_or_else(|| "calibration state directory has no parent".to_string())?;
    let parent = fs::canonicalize(parent).map_err(|error| {
        format!(
            "unable to resolve calibration state parent {}: {error}",
            parent.display()
        )
    })?;
    let state_name = state_dir
        .file_name()
        .ok_or_else(|| "calibration state directory has no name".to_string())?
        .as_bytes();
    let mut events = CalibrationEventLoop::new()?;
    events.watch_named_entry(&parent, state_name)?;
    let started = Instant::now();
    loop {
        match fs::symlink_metadata(state_dir) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                let canonical = fs::canonicalize(state_dir).map_err(|error| {
                    format!(
                        "unable to resolve calibration state directory {}: {error}",
                        state_dir.display()
                    )
                })?;
                events.watch_named_entry(&canonical, CONTROL_SOCKET_NAME)?;
            }
            Ok(_) => {
                return Err("calibration state path is not a real directory".to_string());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "unable to inspect calibration state directory: {error}"
                ));
            }
        }
        let identities =
            match observe_calibration_service_started(proc_root, state_dir, owner_lock_path)? {
                StartedObservation::Ready => return Ok(()),
                StartedObservation::Waiting(identities) => identities,
            };
        if events.refresh_processes(&identities, proc_root)? {
            continue;
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(
                "calibration service did not become ready before its watchdog deadline".to_string(),
            );
        }
        if events.wait(-1, Some(remaining))?.deadline {
            return Err(
                "calibration service did not become ready before its watchdog deadline".to_string(),
            );
        }
    }
}

fn observe_calibration_service_started(
    proc_root: &Path,
    state_dir: &Path,
    owner_lock_path: &Path,
) -> Result<StartedObservation, String> {
    let processes = calibration_service_process_records(proc_root, std::process::id())?;
    if processes.is_empty() {
        return Ok(StartedObservation::Waiting(Vec::new()));
    }
    if processes.len() != 1 || processes[0].kind != CalibrationProcessKind::NativeCoordinator {
        return Err(
            "calibration service topology is not exactly one native coordinator".to_string(),
        );
    }
    let identity = processes[0].identity.clone();
    let state_present = match fs::symlink_metadata(state_dir) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => true,
        Ok(_) => return Err("calibration state path is not a real directory".to_string()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(format!(
                "unable to inspect calibration state directory: {error}"
            ))
        }
    };
    if !state_present || !probe_calibration_coordinator_control(state_dir, &identity)? {
        return Ok(StartedObservation::Waiting(vec![identity]));
    }
    attest_scheduler_owner_lock_held(owner_lock_path)?;
    if !identity.still_matches(proc_root)? {
        return Ok(StartedObservation::Waiting(Vec::new()));
    }
    let confirmed = calibration_service_process_records(proc_root, std::process::id())?;
    if confirmed.len() != 1
        || confirmed[0].kind != CalibrationProcessKind::NativeCoordinator
        || confirmed[0].identity != identity
    {
        return Err(
            "calibration service topology changed during readiness attestation".to_string(),
        );
    }
    Ok(StartedObservation::Ready)
}

fn confirm_calibration_service_stopped(
    proc_root: &Path,
    owner_lock_path: &Path,
    timeout: Duration,
) -> Result<(), String> {
    if timeout.is_zero() {
        return Err("calibration service stop confirmation has a zero deadline".to_string());
    }
    let identities = calibration_service_processes(proc_root, std::process::id())?;
    let mut pidfds = Vec::with_capacity(identities.len());
    for identity in &identities {
        if let Some(pidfd) = open_stable_pidfd(proc_root, identity)? {
            pidfds.push((identity, pidfd));
        }
    }
    let started = Instant::now();
    for (identity, pidfd) in pidfds {
        wait_pidfd(&pidfd, timeout.saturating_sub(started.elapsed()))?;
        if identity.still_matches(proc_root)? {
            return Err(format!(
                "calibration service process {} remained live after its pidfd signalled",
                identity.pid
            ));
        }
    }
    if !calibration_service_processes(proc_root, std::process::id())?.is_empty() {
        return Err("calibration service process respawned during stop confirmation".to_string());
    }
    attest_no_competing_calibration_processes(proc_root, std::process::id())?;
    // The process list proves the known old/new service topology is absent;
    // the lock additionally rejects an unknown or foreign ownership holder.
    drop(SchedulerOwnerLock::open(owner_lock_path)?);
    Ok(())
}

fn calibration_service_processes(
    proc_root: &Path,
    own_pid: u32,
) -> Result<Vec<ProcessIdentity>, String> {
    Ok(calibration_service_process_records(proc_root, own_pid)?
        .into_iter()
        .map(|process| process.identity)
        .collect())
}

fn calibration_service_process_records(
    proc_root: &Path,
    own_pid: u32,
) -> Result<Vec<CalibrationProcess>, String> {
    let mut inspected = 0_usize;
    let mut processes = Vec::new();
    for entry in fs::read_dir(proc_root)
        .map_err(|error| format!("unable to enumerate calibration service processes: {error}"))?
    {
        let entry = entry
            .map_err(|error| format!("unable to inspect calibration service process: {error}"))?;
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
            .ok_or_else(|| "calibration service process enumeration overflow".to_string())?;
        if inspected > MAX_PROC_ENTRIES {
            return Err("calibration service process enumeration exceeds its bound".to_string());
        }
        let cmdline_path = entry.path().join("cmdline");
        let Some(cmdline) = read_bounded_cmdline(&cmdline_path)? else {
            continue;
        };
        let Some(kind) = calibration_service_cmdline_kind(&cmdline) else {
            continue;
        };
        let identity = match ProcessIdentity::inspect_live(proc_root, pid) {
            Ok(Some(identity)) => identity,
            Ok(None) => continue,
            Err(_error) if !entry.path().exists() => continue,
            Err(error) => {
                return Err(format!(
                    "unable to attest calibration service process {pid}: {error}"
                ))
            }
        };
        let Some(confirmed_cmdline) = read_bounded_cmdline(&cmdline_path)? else {
            continue;
        };
        if !identity.still_matches(proc_root)? {
            continue;
        }
        if confirmed_cmdline != cmdline {
            return Err(format!(
                "calibration service process {pid} changed during attestation"
            ));
        }
        processes.push(CalibrationProcess { identity, kind });
    }
    processes.sort_by_key(|process| process.identity.pid);
    Ok(processes)
}

fn read_bounded_cmdline(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "unable to open calibration service command line: {error}"
            ))
        }
    };
    let mut bytes = Vec::new();
    file.by_ref()
        .take((MAX_CMDLINE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("unable to read calibration service command line: {error}"))?;
    if bytes.len() > MAX_CMDLINE_BYTES {
        if calibration_service_owned_prefix(&bytes) {
            return Err("calibration service command line exceeds its bound".to_string());
        }
        return Ok(None);
    }
    Ok(Some(bytes))
}

fn calibration_service_owned_prefix(bytes: &[u8]) -> bool {
    bytes.starts_with(b"/usr/sbin/cake-autorated\0--calibrationd\0")
}

#[cfg(test)]
fn calibration_service_cmdline_matches(bytes: &[u8]) -> bool {
    calibration_service_cmdline_kind(bytes).is_some()
}

fn calibration_service_cmdline_kind(bytes: &[u8]) -> Option<CalibrationProcessKind> {
    let arguments = bytes
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .collect::<Vec<_>>();
    let expected: &[&[u8]] = &[
        b"/usr/sbin/cake-autorated",
        b"--calibrationd",
        b"--native-rating",
        b"--native-speedtest",
        b"--native-autotune",
        b"--native-scheduler",
        b"--scheduler-store-dir",
        b"/etc/cake-autorate-rs-scheduler",
    ];
    (arguments == expected).then_some(CalibrationProcessKind::NativeCoordinator)
}

fn open_stable_pidfd(
    proc_root: &Path,
    identity: &ProcessIdentity,
) -> Result<Option<OwnedFd>, String> {
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, identity.pid, 0) };
    if raw < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) && !identity.still_matches(proc_root)? {
            return Ok(None);
        }
        return Err(format!(
            "unable to open pidfd for calibration service process {}: {error}",
            identity.pid
        ));
    }
    let pidfd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    if !identity.still_matches(proc_root)? {
        return Ok(None);
    }
    Ok(Some(pidfd))
}

fn wait_pidfd(pidfd: &OwnedFd, timeout: Duration) -> Result<(), String> {
    if timeout.is_zero() {
        return Err("calibration service did not stop before its watchdog deadline".to_string());
    }
    let started = Instant::now();
    loop {
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(
                "calibration service did not stop before its watchdog deadline".to_string(),
            );
        }
        let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd: pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let status = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if status > 0 && descriptor.revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            return Ok(());
        }
        if status == 0 {
            return Err(
                "calibration service did not stop before its watchdog deadline".to_string(),
            );
        }
        if status < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(format!(
            "unable to wait for calibration service process exit: {}",
            io::Error::last_os_error()
        ));
    }
}

fn prepare_calibration_service(backend: &mut impl CalibrationServiceBackend) -> Result<(), String> {
    // Serialize the complete preparation against a live/parallel coordinator.
    // This lock is separate from runtime.guard. The recovery implementations
    // retain their established runtime lock order; an outer runtime flock here
    // would conflict with those nested locks.
    backend.acquire_scheduler_owner()?;
    backend.recover_native_apply()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQUENCE: AtomicU64 = AtomicU64::new(1);

    fn stop_test_root() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "cake-calibration-stop-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[derive(Default)]
    struct FakeBackend {
        events: Vec<&'static str>,
        fail_at: Option<&'static str>,
    }

    impl FakeBackend {
        fn new() -> Self {
            Self::default()
        }

        fn event(&mut self, event: &'static str) -> Result<(), String> {
            self.events.push(event);
            if self.fail_at == Some(event) {
                return Err(format!("injected failure at {event}"));
            }
            Ok(())
        }
    }

    impl CalibrationServiceBackend for FakeBackend {
        fn prepare_stop(&mut self) -> Result<(), String> {
            self.event("prepare-stop")
        }

        fn confirm_started(&mut self) -> Result<(), String> {
            self.event("confirm-started")
        }

        fn confirm_stopped(&mut self) -> Result<(), String> {
            self.event("confirm-stopped")
        }

        fn acquire_scheduler_owner(&mut self) -> Result<(), String> {
            self.event("scheduler-lock")
        }

        fn recover_native_apply(&mut self) -> Result<(), String> {
            self.event("native-apply-recovery")
        }
    }

    #[test]
    fn prepare_start_serializes_ownership_before_native_recovery() {
        let mut backend = FakeBackend::new();
        prepare_calibration_service(&mut backend).unwrap();
        assert_eq!(backend.events, ["scheduler-lock", "native-apply-recovery"]);
    }

    #[test]
    fn every_failure_stops_before_later_authority() {
        for failure in ["scheduler-lock", "native-apply-recovery"] {
            let mut backend = FakeBackend::new();
            backend.fail_at = Some(failure);
            assert!(prepare_calibration_service(&mut backend).is_err());
            assert_eq!(backend.events.last(), Some(&failure));
        }
    }

    #[test]
    fn cli_contract_is_exact_and_rejects_extra_arguments() {
        assert!(run_calibration_service(std::iter::empty()).is_err());
        assert!(run_calibration_service(["other".to_string()].into_iter()).is_err());
        assert!(run_calibration_service(
            ["prepare-start".to_string(), "extra".to_string()].into_iter()
        )
        .is_err());
        assert!(run_calibration_service(
            ["prepare-stop".to_string(), "extra".to_string()].into_iter()
        )
        .is_err());
        assert!(run_calibration_service(
            ["confirm-stopped".to_string(), "extra".to_string()].into_iter()
        )
        .is_err());
        assert!(run_calibration_service(
            ["confirm-started".to_string(), "extra".to_string()].into_iter()
        )
        .is_err());
    }

    #[test]
    fn package_upgrade_deferral_flag_is_exact() {
        use std::ffi::OsStr;

        assert!(!calibration_package_upgrade_mode_value(None).unwrap());
        assert!(!calibration_package_upgrade_mode_value(Some(OsStr::new(""))).unwrap());
        assert!(!calibration_package_upgrade_mode_value(Some(OsStr::new("0"))).unwrap());
        assert!(calibration_package_upgrade_mode_value(Some(OsStr::new("1"))).unwrap());
        assert!(calibration_package_upgrade_mode_value(Some(OsStr::new("yes"))).is_err());
    }

    #[test]
    fn start_and_stop_confirmations_are_separate_exact_authorities() {
        let mut stop_ready = FakeBackend::new();
        assert_eq!(
            run_calibration_service_with_backend(Some("prepare-stop"), &mut stop_ready).unwrap(),
            format!("{CALIBRATION_SERVICE_STOP_READY_V1}\n")
        );
        assert_eq!(stop_ready.events, ["prepare-stop"]);

        let mut started = FakeBackend::new();
        assert_eq!(
            run_calibration_service_with_backend(Some("confirm-started"), &mut started).unwrap(),
            format!("{CALIBRATION_SERVICE_STARTED_V1}\n")
        );
        assert_eq!(started.events, ["confirm-started"]);

        let mut stopped = FakeBackend::new();
        assert_eq!(
            run_calibration_service_with_backend(Some("confirm-stopped"), &mut stopped).unwrap(),
            format!("{CALIBRATION_SERVICE_STOPPED_V1}\n")
        );
        assert_eq!(stopped.events, ["confirm-stopped"]);

        let mut failed = FakeBackend::new();
        failed.fail_at = Some("confirm-stopped");
        assert!(
            run_calibration_service_with_backend(Some("confirm-stopped"), &mut failed).is_err()
        );
        assert_eq!(failed.events, ["confirm-stopped"]);
    }

    #[test]
    fn native_calibration_service_process_vector_is_exact() {
        let arguments = [
            "/usr/sbin/cake-autorated",
            "--calibrationd",
            "--native-rating",
            "--native-speedtest",
            "--native-autotune",
            "--native-scheduler",
            "--scheduler-store-dir",
            "/etc/cake-autorate-rs-scheduler",
        ];
        let mut cmdline = arguments.join("\0").into_bytes();
        cmdline.push(0);
        assert!(calibration_service_cmdline_matches(&cmdline));
        assert_eq!(
            calibration_service_cmdline_kind(&cmdline),
            Some(CalibrationProcessKind::NativeCoordinator)
        );
        cmdline.extend_from_slice(b"extra\0");
        assert!(!calibration_service_cmdline_matches(&cmdline));

        assert!(!calibration_service_cmdline_matches(
            b"/usr/sbin/cake-autorated\0--calibrationd\0--native-autotune\0"
        ));
    }

    #[test]
    fn bounded_calibration_process_reader_ignores_only_unrelated_long_argv() {
        let root = stop_test_root();
        fs::create_dir_all(&root).unwrap();
        let path = root.join("cmdline");
        let mut unrelated = b"/usr/bin/apcontroller\0".to_vec();
        unrelated.extend(std::iter::repeat_n(b'x', MAX_CMDLINE_BYTES + 32));
        unrelated.push(0);
        fs::write(&path, unrelated).unwrap();
        assert_eq!(read_bounded_cmdline(&path).unwrap(), None);

        let mut owned = b"/usr/sbin/cake-autorated\0--calibrationd\0".to_vec();
        owned.extend(std::iter::repeat_n(b'y', MAX_CMDLINE_BYTES + 32));
        owned.push(0);
        fs::write(&path, owned).unwrap();
        assert!(read_bounded_cmdline(&path)
            .unwrap_err()
            .contains("exceeds its bound"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stop_confirmation_requires_both_empty_process_state_and_free_owner_lock() {
        let root = stop_test_root();
        let proc_root = root.join("proc");
        let lock_path = root.join("scheduler-owner.lock");
        fs::create_dir_all(&proc_root).unwrap();

        confirm_calibration_service_stopped(&proc_root, &lock_path, Duration::from_secs(1))
            .unwrap();

        let held = SchedulerOwnerLock::open(&lock_path).unwrap();
        assert!(confirm_calibration_service_stopped(
            &proc_root,
            &lock_path,
            Duration::from_secs(1),
        )
        .unwrap_err()
        .contains("already held"));
        drop(held);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stop_confirmation_accepts_only_a_kernel_attested_exit_transition() {
        let root = stop_test_root();
        let proc_root = root.join("proc");
        let proc_dir = proc_root.join("42");
        let lock_path = root.join("scheduler-owner.lock");
        fs::create_dir_all(&proc_dir).unwrap();
        fs::write(
            proc_dir.join("cmdline"),
            b"/usr/sbin/cake-autorated\0--calibrationd\0--native-rating\0--native-speedtest\0--native-autotune\0--native-scheduler\0--scheduler-store-dir\0/etc/cake-autorate-rs-scheduler\0",
        )
        .unwrap();
        let mut fields = vec!["X".to_string(), "1".to_string(), "-1".to_string()];
        while fields.len() <= 19 {
            fields.push("0".to_string());
        }
        fs::write(
            proc_dir.join("stat"),
            format!("42 (cake-autorated) {} 0", fields.join(" ")),
        )
        .unwrap();

        confirm_calibration_service_stopped(&proc_root, &lock_path, Duration::from_secs(1))
            .unwrap();

        fields[0] = "S".to_string();
        fs::write(
            proc_dir.join("stat"),
            format!("42 (cake-autorated) {} 0", fields.join(" ")),
        )
        .unwrap();
        assert!(confirm_calibration_service_stopped(
            &proc_root,
            &lock_path,
            Duration::from_secs(1),
        )
        .unwrap_err()
        .contains("process group is invalid"));

        fs::remove_dir_all(root).unwrap();
    }
}
