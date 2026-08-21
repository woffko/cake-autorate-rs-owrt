use super::autotune_apply_openwrt::OpenWrtNativeApplyBackend;
use super::autotune_runtime::{
    speedtest_unshaped_topology, AbsentRuntimeBaseline, AutotuneRuntimePermit, RuntimePermitKind,
};
use super::autotune_runtime_store::RuntimeOverrideStore;
use super::full_autotune::AutotuneRuntimeControl;
use super::identity::{
    monotonic_boot_ms, read_kernel_uuid, ProcessIdentity, DEFAULT_BOOT_ID_PATH, DEFAULT_PROC_ROOT,
};
use super::process::{run_bounded_command_output, SpawnSpec};
use super::protocol::{
    OperationKind, OperationRequest, OperationRouteMode, OperationTargetState, SpeedtestDirection,
};
use super::rating;
use crate::routing::{inspect_route, RouteIdentity, RouteSnapshot, RouteSpec};
use crate::Config;
use std::ffi::OsString;
use std::fmt::Write as FmtWrite;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::IpAddr;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const SPEEDTEST_GO: &str = "/usr/bin/speedtest-go";
const MWAN3: &str = "/usr/sbin/mwan3";
const NFT: &str = "/usr/sbin/nft";
const TC: &str = "/sbin/tc";
const PASSWD: &str = "/etc/passwd";
const SPEEDTEST_USER: &str = "cake-speedtest";
const OUTPUT_LIMIT: usize = 512 * 1024;
const COMMAND_OUTPUT_LIMIT: usize = 256 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const ROUTE_RECHECK_INTERVAL: Duration = Duration::from_secs(1);
const MIN_ROUTE_PROOF_BYTES: u64 = 64 * 1024;
const MAX_UNPROVED_TRAFFIC_ATTEMPTS: u8 = 3;
const TRAFFIC_STOP_RESERVE_WINDOW_MS: u128 = 1_000;
const TRAFFIC_STOP_RESERVE_HEADROOM_PERCENT: u128 = 125;
const TRAFFIC_STOP_RESERVE_FIXED_BYTES: u128 = 1024 * 1024;
const MAX_BACKEND_RUNTIME: Duration = Duration::from_secs(180);
const MAX_SERVER_LIST_RUNTIME: Duration = Duration::from_secs(30);
const MAX_AUTOMATIC_SERVER_ATTEMPTS: usize = 3;
const MAX_RATE_PAYLOAD_TIMING_RATIO_PERCENT: u128 = 135;
pub(crate) const SPEEDTEST_RATE_PAYLOAD_TIMING_MISMATCH: &str =
    "speedtest-rate-payload-timing-mismatch";
#[cfg(test)]
const MAX_UPLOAD_COUNTER_LAG_PERCENT: u128 = 5;
const MIN_QUALIFICATION_COUNTER_CONFIDENCE_PERCENT: u128 = 80;
const MIN_SHAPED_UPLOAD_PAYLOAD_WIRE_PERCENT: u128 = 60;
const MAX_QUALIFICATION_DIAGNOSTIC_BYTES: usize = 1024;
const ACCOUNTING_RX_COUNTER: &str = "rx";
const ACCOUNTING_TX_COUNTER: &str = "tx";
const SIGKILL: i32 = 9;
const PR_SET_PDEATHSIG: i32 = 1;

pub(crate) const SPEEDTEST_TRAFFIC_BUDGET_EXHAUSTED: &str = "speedtest-traffic-budget-exhausted";
pub(crate) const SPEEDTEST_TRAFFIC_LIMIT_REACHED: &str = "speedtest-traffic-limit-reached";

/// Reserve enough aggregate interface traffic for the 100 ms supervisor to
/// observe a limit crossing and SIGKILL the backend without consuming the
/// caller's complete accounting budget.  One full second plus 25% and one MiB
/// deliberately exceeds the normal observation/kill path while remaining
/// usable on wide links.  If the supplied rate authority is too large to fit
/// in u64, the saturated result makes admission fail closed before transfer.
pub(crate) fn traffic_stop_safety_reserve_bytes(
    download_bound_kbps: u64,
    upload_bound_kbps: u64,
) -> u64 {
    let aggregate_kbps =
        u128::from(download_bound_kbps).saturating_add(u128::from(upload_bound_kbps));
    let denominator = 8_u128.saturating_mul(100);
    let variable = aggregate_kbps
        .saturating_mul(TRAFFIC_STOP_RESERVE_WINDOW_MS)
        .saturating_mul(TRAFFIC_STOP_RESERVE_HEADROOM_PERCENT)
        .saturating_add(denominator - 1)
        / denominator;
    u64::try_from(variable.saturating_add(TRAFFIC_STOP_RESERVE_FIXED_BYTES)).unwrap_or(u64::MAX)
}

fn minimum_attempt_budget(direction: SpeedtestDirection, safety_reserve_bytes: u64) -> u64 {
    let route_proof = match direction {
        SpeedtestDirection::Both => 2 * MIN_ROUTE_PROOF_BYTES,
        SpeedtestDirection::Download | SpeedtestDirection::Upload => MIN_ROUTE_PROOF_BYTES,
    };
    safety_reserve_bytes
        .checked_add(route_proof)
        .unwrap_or(u64::MAX)
}

pub(crate) fn minimum_speedtest_traffic_budget_bytes(
    direction: SpeedtestDirection,
    download_bound_kbps: u64,
    upload_bound_kbps: u64,
) -> u64 {
    minimum_attempt_budget(
        direction,
        traffic_stop_safety_reserve_bytes(download_bound_kbps, upload_bound_kbps),
    )
}

pub(crate) fn minimum_full_autotune_traffic_budget_bytes(
    download_bound_kbps: u64,
    upload_bound_kbps: u64,
) -> u64 {
    minimum_attempt_budget(
        SpeedtestDirection::Both,
        traffic_stop_safety_reserve_bytes(download_bound_kbps, upload_bound_kbps),
    )
}

extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
    fn setgroups(size: usize, groups: *const u32) -> i32;
    fn setresgid(real: u32, effective: u32, saved: u32) -> i32;
    fn setresuid(real: u32, effective: u32, saved: u32) -> i32;
    fn getppid() -> i32;
    fn prctl(
        option: i32,
        argument2: usize,
        argument3: usize,
        argument4: usize,
        argument5: usize,
    ) -> i32;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeedtestResult {
    pub direction: SpeedtestDirection,
    pub download_kbps: Option<u64>,
    pub upload_kbps: Option<u64>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub elapsed_ms: u64,
    pub server_id: Option<u64>,
    pub server_name: String,
    pub server_sponsor: String,
}

/// Private measurement detail used only by native Full Auto-Tune.
///
/// `aggregate_*_bytes` comes from the selected physical route interface and is
/// used for the traffic budget. `controlled_*_wire_bytes` comes from the
/// UID-scoped nft counters, while `controlled_*_payload_bytes` comes from
/// speedtest-go's own `Used:` value and is only plausibility/goodput evidence.
/// For a shaped direction, its confidence counter is the corresponding
/// managed CAKE root `Sent` delta. For a raw direction it is the physical
/// route-interface delta. Keeping the sources separate avoids both a guessed
/// overhead multiplier and the pre-CAKE ingress-drop accounting error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpeedtestLoadSample {
    pub direction: SpeedtestDirection,
    pub aggregate_rx_bytes: u64,
    pub aggregate_tx_bytes: u64,
    pub confidence_rx_bytes: u64,
    pub confidence_tx_bytes: u64,
    pub controlled_rx_wire_bytes: u64,
    pub controlled_tx_wire_bytes: u64,
    pub controlled_rx_payload_bytes: u64,
    pub controlled_tx_payload_bytes: u64,
    /// Conservative monotonic window enclosing every route, nft and CAKE
    /// counter snapshot for this backend run. Counter-derived rates must use
    /// this window rather than a backend-owned directional timer.
    pub counter_elapsed_ms: u64,
    pub download_elapsed_ms: Option<u64>,
    pub upload_elapsed_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpeedtestQualificationRejection {
    Direction,
    RateMissing,
    DurationMissing,
    DownloadPayloadProof,
    UploadPayloadProof,
    DownloadWireProof,
    UploadWireProof,
    AggregateRxBelowPayload,
    ConfidenceRxBelowWire,
    RxWireBelowPayload,
    ConfidenceTxWireRatio,
    TxWirePayloadRatio,
    DownloadRatePayloadTiming,
    UploadRatePayloadTiming,
}

impl SpeedtestQualificationRejection {
    fn code(self) -> &'static str {
        match self {
            Self::Direction => "direction",
            Self::RateMissing => "rate-missing",
            Self::DurationMissing => "duration-missing",
            Self::DownloadPayloadProof => "dl-payload-proof",
            Self::UploadPayloadProof => "ul-payload-proof",
            Self::DownloadWireProof => "dl-wire-proof",
            Self::UploadWireProof => "ul-wire-proof",
            Self::AggregateRxBelowPayload => "rx-route-below-payload",
            Self::ConfidenceRxBelowWire => "rx-cake-below-wire",
            Self::RxWireBelowPayload => "rx-wire-below-payload",
            Self::ConfidenceTxWireRatio => "tx-cake-wire-ratio",
            Self::TxWirePayloadRatio => "tx-wire-payload-ratio",
            Self::DownloadRatePayloadTiming => "download-rate-payload",
            Self::UploadRatePayloadTiming => "upload-rate-payload",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CakeCounterKind {
    Cake,
    CakeMq,
}

impl CakeCounterKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Cake => "cake",
            Self::CakeMq => "cake_mq",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CakeCounterDirection {
    Download,
    Upload,
}

impl CakeCounterDirection {
    fn prefix(self) -> &'static str {
        match self {
            Self::Download => "speedtest-download-accounting",
            Self::Upload => "speedtest-upload-accounting",
        }
    }
}

/// An already-bound CAKE counter.  The speed-test supervisor never discovers
/// a replacement qdisc from mutable UCI: it reads exactly this device,
/// ifindex, kind and handle before and after the transfer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CakeCounterTarget {
    device: String,
    ifindex: u32,
    kind: CakeCounterKind,
    handle: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SpeedtestAccountingPlan {
    download: Option<CakeCounterTarget>,
    upload: Option<CakeCounterTarget>,
}

impl SpeedtestAccountingPlan {
    pub(crate) fn route_only() -> Self {
        Self::default()
    }

    pub(crate) fn new(
        download: Option<CakeCounterTarget>,
        upload: Option<CakeCounterTarget>,
    ) -> Self {
        Self { download, upload }
    }

    #[cfg(test)]
    pub(crate) fn download_uses_cake(&self) -> bool {
        self.download.is_some()
    }

    #[cfg(test)]
    pub(crate) fn upload_uses_cake(&self) -> bool {
        self.upload.is_some()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SpeedtestTrafficDebit {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedSpeedtestGo {
    result: SpeedtestResult,
    controlled_rx_payload_bytes: u64,
    controlled_tx_payload_bytes: u64,
    download_elapsed_ms: Option<u64>,
    upload_elapsed_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpeedtestTerminal {
    Complete(SpeedtestResult),
    Cancelled,
    Failed { code: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeedtestTerminalRecord {
    pub job_id: String,
    pub worker_run_id: String,
    pub terminal: SpeedtestTerminal,
}

struct BackendChild {
    child: Child,
    identity: ProcessIdentity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BackendCredentials {
    uid: u32,
    gid: u32,
}

struct NftRoutePin {
    table: Option<String>,
    owner: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CakeCounterSnapshot {
    device: String,
    kind: String,
    handle: String,
    sent_bytes: u64,
}

pub(crate) struct EmbeddedSpeedtestSession {
    route_pin: NftRoutePin,
    credentials: BackendCredentials,
    job_id: String,
    worker_run_id: String,
    route_fingerprint: String,
    route: super::protocol::OperationRouteIdentity,
    backend: String,
    selected_server_id: Option<u64>,
}

impl EmbeddedSpeedtestSession {
    pub(crate) fn open(
        request: &OperationRequest,
        worker_run_id: &str,
        scratch_path: &Path,
        terminate: &AtomicBool,
    ) -> Result<Self, String> {
        if request.backend != "speedtest-go" {
            return Err("speedtest-backend-unsupported".to_string());
        }
        let credentials = backend_credentials()?;
        let route_pin = acquire_route_pin_when_ready(
            || wait_for_route_ready(request, terminate).map(|_| ()),
            || NftRoutePin::acquire(request, worker_run_id, credentials.uid, scratch_path),
            || attest_route(request).map(|_| ()),
        )?;
        Ok(Self {
            route_pin,
            credentials,
            job_id: request.identity.job_id.clone(),
            worker_run_id: worker_run_id.to_string(),
            route_fingerprint: request.identity.route_fingerprint.clone(),
            route: request.route.clone(),
            backend: request.backend.clone(),
            selected_server_id: request.speedtest_server_id,
        })
    }

    pub(crate) fn close(mut self) -> Result<(), String> {
        self.route_pin.release()
    }

    fn matches(&self, request: &OperationRequest, worker_run_id: &str) -> bool {
        self.job_id == request.identity.job_id
            && self.worker_run_id == worker_run_id
            && self.route_fingerprint == request.identity.route_fingerprint
            && self.route == request.route
            && self.backend == request.backend
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SpeedtestTrafficCounters {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

struct BackendOutputGuard {
    output: File,
    stderr: File,
}

impl BackendOutputGuard {
    fn create(terminal_path: &Path) -> Result<(Self, File, File), String> {
        let output_path = terminal_path.with_extension("backend-output");
        let stderr_path = terminal_path.with_extension("backend-stderr");
        let output = private_output_file(&output_path)?;
        let stderr = match private_output_file(&stderr_path) {
            Ok(file) => file,
            Err(error) => {
                let _ = fs::remove_file(&output_path);
                return Err(error);
            }
        };
        let stdout_file = output.try_clone().map_err(|error| {
            cleanup_backend_output(&output_path, &stderr_path);
            format!("speedtest-output-clone-failed: {error}")
        })?;
        let stderr_file = stderr.try_clone().map_err(|error| {
            cleanup_backend_output(&output_path, &stderr_path);
            format!("speedtest-stderr-clone-failed: {error}")
        })?;
        fs::remove_file(&output_path).map_err(|error| {
            cleanup_backend_output(&output_path, &stderr_path);
            format!("speedtest-output-unlink-failed: {error}")
        })?;
        fs::remove_file(&stderr_path).map_err(|error| {
            let _ = fs::remove_file(&stderr_path);
            format!("speedtest-stderr-unlink-failed: {error}")
        })?;
        Ok((Self { output, stderr }, stdout_file, stderr_file))
    }
}

impl NftRoutePin {
    fn acquire(
        request: &OperationRequest,
        worker_run_id: &str,
        backend_uid: u32,
        terminal_path: &Path,
    ) -> Result<Self, String> {
        validate_utility_binary(Path::new(NFT), "nft")?;
        let route_mark = if request.route.mode == OperationRouteMode::Mwan3 {
            validate_utility_binary(Path::new(MWAN3), "mwan3")?;
            let fwmark = request
                .route
                .fwmark
                .ok_or_else(|| "speedtest-mwan3-fwmark-missing".to_string())?;
            let mark_mask = resolve_mwan3_mark_mask(request)?;
            if mark_mask == 0 || fwmark == 0 || fwmark & !mark_mask != 0 {
                return Err("speedtest-mwan3-mark-outside-mask".to_string());
            }
            Some((!mark_mask, fwmark))
        } else {
            None
        };
        let table = route_pin_table_name(&request.identity.job_id, worker_run_id)?;
        let owner = route_pin_owner(&request.identity.job_id, worker_run_id)?;
        cleanup_named_route_pin(&table, &owner)?;

        let batch = nft_route_pin_batch(&table, &owner, backend_uid, route_mark);
        let batch_path = terminal_path.with_extension("nft-batch");
        let mut batch_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&batch_path)
            .map_err(|error| format!("speedtest-route-pin-batch-create-failed: {error}"))?;
        let write_result = batch_file
            .write_all(batch.as_bytes())
            .and_then(|_| batch_file.sync_all())
            .map_err(|error| format!("speedtest-route-pin-batch-write-failed: {error}"));
        drop(batch_file);
        if let Err(error) = write_result {
            let _ = fs::remove_file(&batch_path);
            return Err(error);
        }
        let installed = run_bounded_command(NFT, &["-j", "-f", path_text(&batch_path)?]);
        let _ = fs::remove_file(&batch_path);
        let installed = installed?;
        if !installed.0 {
            return Err("speedtest-route-pin-install-failed".to_string());
        }
        let pin = Self {
            table: Some(table),
            owner: Some(owner),
        };
        attest_named_route_pin(
            pin.table.as_deref().expect("route pin table was set"),
            pin.owner.as_deref().expect("route pin owner was set"),
        )?;
        Ok(pin)
    }

    fn release(&mut self) -> Result<(), String> {
        let Some(table) = self.table.take() else {
            return Ok(());
        };
        let owner = self
            .owner
            .take()
            .ok_or_else(|| "speedtest-route-pin-owner-missing".to_string())?;
        cleanup_named_route_pin(&table, &owner)
    }

    fn traffic_counters(&self) -> Result<SpeedtestTrafficCounters, String> {
        let table = self
            .table
            .as_deref()
            .ok_or_else(|| "speedtest-accounting-table-missing".to_string())?;
        let owner = self
            .owner
            .as_deref()
            .ok_or_else(|| "speedtest-accounting-owner-missing".to_string())?;
        read_named_traffic_counters(table, owner)?
            .ok_or_else(|| "speedtest-accounting-counters-missing-during-measurement".to_string())
    }
}

impl Drop for NftRoutePin {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

impl BackendChild {
    fn spawn(
        arguments: &[String],
        stdout: File,
        stderr: File,
        credentials: BackendCredentials,
    ) -> Result<Self, String> {
        validate_backend_binary(Path::new(SPEEDTEST_GO))?;
        let mut command = Command::new(SPEEDTEST_GO);
        let expected_parent = std::process::id();
        command
            .args(arguments)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .env("HOME", "/tmp")
            .current_dir("/tmp")
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        unsafe {
            command.pre_exec(move || {
                if setgroups(0, std::ptr::null()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if setresgid(credentials.gid, credentials.gid, credentials.gid) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if setresuid(credentials.uid, credentials.uid, credentials.uid) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // Linux clears PDEATHSIG when effective credentials change, so
                // arm it only after the complete privilege drop. The parent
                // check then closes the race in which the worker disappeared
                // before prctl could bind the backend lifecycle to it.
                if prctl(PR_SET_PDEATHSIG, SIGKILL as usize, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if getppid() != i32::try_from(expected_parent).unwrap_or(-1) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "speedtest worker disappeared before backend exec",
                    ));
                }
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("speedtest-backend-spawn-failed: {error}"))?;
        let identity = match ProcessIdentity::inspect(Path::new(DEFAULT_PROC_ROOT), child.id()) {
            Ok(identity) => identity,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("speedtest-backend-identity-unavailable: {error}"));
            }
        };
        Ok(Self { child, identity })
    }

    fn try_wait(&mut self) -> Result<bool, String> {
        self.child
            .try_wait()
            .map(|status| status.is_some())
            .map_err(|error| format!("speedtest-backend-wait-failed: {error}"))
    }

    fn finish(&mut self) -> Result<bool, String> {
        self.child
            .wait()
            .map(|status| status.success())
            .map_err(|error| format!("speedtest-backend-reap-failed: {error}"))
    }

    fn stop_and_reap(&mut self) -> Result<(), String> {
        if self.try_wait()? {
            let _ = self.child.wait();
            return Ok(());
        }
        if self.identity.still_matches(Path::new(DEFAULT_PROC_ROOT))? {
            let pid = i32::try_from(self.identity.pid)
                .map_err(|_| "speedtest-backend-pid-out-of-range".to_string())?;
            if unsafe { kill(pid, SIGKILL) } != 0 {
                return Err("speedtest-backend-kill-failed".to_string());
            }
        }
        self.child
            .wait()
            .map(|_| ())
            .map_err(|error| format!("speedtest-backend-reap-failed: {error}"))
    }
}

impl Drop for BackendChild {
    fn drop(&mut self) {
        let Ok(false) = self.try_wait() else {
            return;
        };
        if self
            .identity
            .still_matches(Path::new(DEFAULT_PROC_ROOT))
            .unwrap_or(false)
        {
            if let Ok(pid) = i32::try_from(self.identity.pid) {
                let _ = unsafe { kill(pid, SIGKILL) };
            }
        }
        let _ = self.child.wait();
    }
}

pub fn run_speedtest_worker<I>(mut args: I, terminate: &AtomicBool) -> Result<(), String>
where
    I: Iterator<Item = String>,
{
    let mut request_path = None;
    let mut terminal_path = None;
    let mut permit_path = None;
    let mut worker_run_id = None;
    while let Some(argument) = args.next() {
        let destination = match argument.as_str() {
            "--request" => &mut request_path,
            "--terminal" => &mut terminal_path,
            "--permit" => &mut permit_path,
            "--worker-run-id" => &mut worker_run_id,
            _ => return Err(format!("unsupported speedtest worker argument: {argument}")),
        };
        if destination.is_some() {
            return Err(format!("duplicate speedtest worker argument: {argument}"));
        }
        *destination = Some(
            args.next()
                .ok_or_else(|| format!("{argument} requires a value"))?,
        );
    }
    let request_path = PathBuf::from(request_path.ok_or("--request is required")?);
    let terminal_path = PathBuf::from(terminal_path.ok_or("--terminal is required")?);
    let permit_path = PathBuf::from(permit_path.ok_or("--permit is required")?);
    let worker_run_id = worker_run_id.ok_or("--worker-run-id is required")?;
    rating::require_exact_hex("worker run id", &worker_run_id, 32)?;
    let request = rating::read_private_request(&request_path)?;
    if request.identity.operation != OperationKind::Speedtest {
        return Err("speedtest worker accepts Speedtest operations only".to_string());
    }

    let terminal = match rating::wait_for_permit(
        &permit_path,
        &request.identity.job_id,
        &worker_run_id,
        request.deadline_unix_ms,
        terminate,
    ) {
        Ok(true) => match run_speedtest(&request, &worker_run_id, terminate, &terminal_path) {
            Ok(terminal) => terminal,
            Err(error) => SpeedtestTerminal::Failed {
                code: rating::bounded_error_code(&error),
            },
        },
        Ok(false) => SpeedtestTerminal::Cancelled,
        Err(error) => SpeedtestTerminal::Failed {
            code: rating::bounded_error_code(&error),
        },
    };
    rating::atomic_private_write(
        &terminal_path,
        terminal
            .encode(&request.identity.job_id, &worker_run_id)?
            .as_bytes(),
    )
}

fn run_speedtest(
    request: &OperationRequest,
    worker_run_id: &str,
    terminate: &AtomicBool,
    terminal_path: &Path,
) -> Result<SpeedtestTerminal, String> {
    if request.backend != "speedtest-go" {
        return Err("speedtest-backend-unsupported".to_string());
    }
    let direction = request
        .speedtest_direction
        .ok_or_else(|| "speedtest-direction-missing".to_string())?;
    match request.speedtest_topology {
        Some(super::protocol::SpeedtestTopology::Current) => {
            run_embedded_speedtest(request, worker_run_id, direction, terminate, terminal_path)
        }
        Some(super::protocol::SpeedtestTopology::Unshaped) => {
            run_unshaped_speedtest(request, worker_run_id, direction, terminate, terminal_path)
        }
        None => Err("speedtest-topology-missing".to_string()),
    }
}

fn run_unshaped_speedtest(
    request: &OperationRequest,
    worker_run_id: &str,
    direction: SpeedtestDirection,
    terminate: &AtomicBool,
    scratch_path: &Path,
) -> Result<SpeedtestTerminal, String> {
    if request.target_state == OperationTargetState::AbsentBootstrap {
        let backend = OpenWrtNativeApplyBackend::new();
        return run_bootstrap_unshaped_with_absence(
            || backend.capture_bootstrap_runtime_baseline(request, worker_run_id),
            || run_embedded_speedtest(request, worker_run_id, direction, terminate, scratch_path),
            |baseline| backend.attest_bootstrap_runtime_absence(request, baseline),
        );
    }
    let cfg = Config::from_uci(&request.identity.instance)?;
    let store = RuntimeOverrideStore::open(&cfg.run_dir())?;
    // The coordinator publishes the runtime permit before the ordinary worker
    // permit.  This function is reached only after the latter was attested, so
    // absence here is ownership loss, not a reason to authorize by waiting.
    let permit = read_unshaped_runtime_permit(&store, request, worker_run_id)?;
    let topology = speedtest_unshaped_topology(direction);
    // The opposite direction is explicitly recreated inside the permit-owned
    // private topology at its captured baseline rate. It remains shaped, but
    // it is not the original managed SQM qdisc until exact restoration.
    let control = AutotuneRuntimeControl {
        permit_id: permit.permit_id.clone(),
        job_id: permit.job_id.clone(),
        worker_run_id: permit.worker_run_id.clone(),
        boot_id: permit.boot_id.clone(),
        coordinator_generation: permit.coordinator_generation.clone(),
        worker: permit.worker.clone(),
        sequence: 1,
        deadline_boot_ms: permit.deadline_boot_ms,
        target_interface: permit.target_interface.clone(),
        route_fingerprint: permit.route_fingerprint.clone(),
        sqm_fingerprint: permit.sqm_fingerprint.clone(),
        topology,
        download_kbps: topology
            .download_is_shaped()
            .then_some(permit.initial_download_kbps),
        upload_kbps: topology
            .upload_is_shaped()
            .then_some(permit.initial_upload_kbps),
    };
    permit.authorizes(&request.identity.instance, &control, monotonic_boot_ms()?)?;
    store.publish_control(&control)?;
    super::full_autotune::wait_for_runtime_applied(&store, &permit, &control, terminate)?;

    let measurement =
        run_embedded_speedtest(request, worker_run_id, direction, terminate, scratch_path);
    let restoration =
        super::full_autotune::restore_runtime_voluntarily(&store, &permit, &control, terminate);
    match (measurement, restoration) {
        (Ok(result), Ok(())) => Ok(result),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(restore_error)) => {
            Err(format!("speedtest-runtime-restore-failed: {restore_error}"))
        }
        (Err(error), Err(restore_error)) => Err(format!(
            "{error}; speedtest-runtime-restore-failed: {restore_error}"
        )),
    }
}

fn run_bootstrap_unshaped_with_absence<T, Capture, Measure, Reattest>(
    capture: Capture,
    measure: Measure,
    reattest: Reattest,
) -> Result<T, String>
where
    Capture: FnOnce() -> Result<AbsentRuntimeBaseline, String>,
    Measure: FnOnce() -> Result<T, String>,
    Reattest: FnOnce(&AbsentRuntimeBaseline) -> Result<(), String>,
{
    let baseline = capture()
        .map_err(|error| format!("speedtest-bootstrap-absence-preflight-failed: {error}"))?;
    let measurement = measure();
    let reattestation = reattest(&baseline)
        .map_err(|error| format!("speedtest-bootstrap-absence-changed: {error}"));
    match (measurement, reattestation) {
        (Ok(result), Ok(())) => Ok(result),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(measurement_error), Err(reattestation_error)) => {
            Err(format!("{measurement_error}; {reattestation_error}"))
        }
    }
}

fn read_unshaped_runtime_permit(
    store: &RuntimeOverrideStore,
    request: &OperationRequest,
    worker_run_id: &str,
) -> Result<AutotuneRuntimePermit, String> {
    let worker = ProcessIdentity::inspect(Path::new(DEFAULT_PROC_ROOT), std::process::id())?;
    let boot_id = read_kernel_uuid(Path::new(DEFAULT_BOOT_ID_PATH), "boot ID")?;
    let permit = store.read_permit()?.ok_or_else(|| {
        "native Speed Test runtime permit is missing after worker admission".to_string()
    })?;
    permit.validate()?;
    if request.speedtest_topology != Some(super::protocol::SpeedtestTopology::Unshaped)
        || permit.kind != RuntimePermitKind::SpeedtestUnshaped
        || permit.job_id != request.identity.job_id
        || permit.worker_run_id != worker_run_id
        || permit.worker != worker
        || permit.boot_id != boot_id
        || permit.instance_name != request.identity.instance
        || permit.target_interface != request.identity.target_interface
        || permit.route_fingerprint != request.identity.route_fingerprint
        || permit.sqm_fingerprint != request.identity.sqm_fingerprint
    {
        return Err("native Speed Test runtime permit identity mismatch".to_string());
    }
    Ok(permit)
}

pub(crate) fn run_embedded_speedtest(
    request: &OperationRequest,
    worker_run_id: &str,
    direction: SpeedtestDirection,
    terminate: &AtomicBool,
    scratch_path: &Path,
) -> Result<SpeedtestTerminal, String> {
    run_embedded_speedtest_with_load_sample(
        request,
        worker_run_id,
        direction,
        terminate,
        scratch_path,
    )
    .map(|(terminal, _)| terminal)
}

pub(crate) fn run_embedded_speedtest_with_load_sample(
    request: &OperationRequest,
    worker_run_id: &str,
    direction: SpeedtestDirection,
    terminate: &AtomicBool,
    scratch_path: &Path,
) -> Result<(SpeedtestTerminal, Option<SpeedtestLoadSample>), String> {
    let mut remaining_traffic_budget = request.traffic_budget_bytes;
    with_embedded_speedtest_session(request, worker_run_id, terminate, scratch_path, |session| {
        run_embedded_speedtest_with_load_sample_in_session_and_debit(
            session,
            request,
            worker_run_id,
            direction,
            0,
            &mut remaining_traffic_budget,
            terminate,
            scratch_path,
            &mut |_| Ok(()),
        )
    })
}

pub(crate) fn with_embedded_speedtest_session<T>(
    request: &OperationRequest,
    worker_run_id: &str,
    terminate: &AtomicBool,
    scratch_path: &Path,
    action: impl FnOnce(&mut EmbeddedSpeedtestSession) -> Result<T, String>,
) -> Result<T, String> {
    let mut session =
        EmbeddedSpeedtestSession::open(request, worker_run_id, scratch_path, terminate)?;
    let result = action(&mut session);
    session.close()?;
    result
}

pub(crate) fn run_embedded_speedtest_with_load_sample_in_session_and_debit(
    session: &mut EmbeddedSpeedtestSession,
    request: &OperationRequest,
    worker_run_id: &str,
    direction: SpeedtestDirection,
    traffic_safety_reserve_bytes: u64,
    remaining_traffic_budget: &mut u64,
    terminate: &AtomicBool,
    scratch_path: &Path,
    on_traffic_debit: &mut dyn FnMut(SpeedtestTrafficDebit) -> Result<(), String>,
) -> Result<(SpeedtestTerminal, Option<SpeedtestLoadSample>), String> {
    let accounting = SpeedtestAccountingPlan::route_only();
    let mut attest_accounting_epoch = || Ok(());
    run_embedded_speedtest_with_accounting_in_session_and_debit(
        session,
        request,
        worker_run_id,
        direction,
        &accounting,
        &mut attest_accounting_epoch,
        traffic_safety_reserve_bytes,
        remaining_traffic_budget,
        terminate,
        scratch_path,
        on_traffic_debit,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_embedded_speedtest_with_accounting_in_session_and_debit(
    session: &mut EmbeddedSpeedtestSession,
    request: &OperationRequest,
    worker_run_id: &str,
    direction: SpeedtestDirection,
    accounting: &SpeedtestAccountingPlan,
    attest_accounting_epoch: &mut dyn FnMut() -> Result<(), String>,
    traffic_safety_reserve_bytes: u64,
    remaining_traffic_budget: &mut u64,
    terminate: &AtomicBool,
    scratch_path: &Path,
    on_traffic_debit: &mut dyn FnMut(SpeedtestTrafficDebit) -> Result<(), String>,
) -> Result<(SpeedtestTerminal, Option<SpeedtestLoadSample>), String> {
    if request.backend != "speedtest-go" {
        return Err("speedtest-backend-unsupported".to_string());
    }
    if !session.matches(request, worker_run_id) {
        return Err("speedtest-session-identity-mismatch".to_string());
    }
    let minimum_attempt_budget = minimum_attempt_budget(direction, traffic_safety_reserve_bytes);
    let result = retry_budgeted_route_measurement_on_loss_with_debit(
        remaining_traffic_budget,
        minimum_attempt_budget,
        || interface_counters(&request.route.l3_device),
        |budget| {
            let mut bounded_request = request.clone();
            bounded_request.traffic_budget_bytes = budget
                .checked_sub(traffic_safety_reserve_bytes)
                .ok_or_else(|| SPEEDTEST_TRAFFIC_BUDGET_EXHAUSTED.to_string())?;
            let initial_route = wait_for_route_ready(request, terminate)?;
            run_speedtest_with_pin(
                &bounded_request,
                direction,
                accounting,
                attest_accounting_epoch,
                terminate,
                scratch_path,
                initial_route,
                session.credentials,
                session.selected_server_id,
                &session.route_pin,
            )
        },
        on_traffic_debit,
    )?;
    if let (SpeedtestTerminal::Complete(measurement), None) =
        (&result.0, session.selected_server_id)
    {
        session.selected_server_id = measurement.server_id;
    }
    Ok(result)
}

/// Select and validate one automatic speedtest-go server for the complete
/// native Auto-Tune job.  A single bidirectional qualification prevents later
/// directional captures from silently switching servers, and rejects the
/// known speedtest-go failure mode where upload payload/rate is reported more
/// than once even though the selected interface counters prove otherwise.
pub(crate) fn qualify_embedded_speedtest_server_in_session(
    session: &mut EmbeddedSpeedtestSession,
    request: &OperationRequest,
    worker_run_id: &str,
    accounting: &SpeedtestAccountingPlan,
    attest_accounting_epoch: &mut dyn FnMut() -> Result<(), String>,
    traffic_safety_reserve_bytes: u64,
    remaining_traffic_budget: &mut u64,
    terminate: &AtomicBool,
    scratch_path: &Path,
    on_traffic_debit: &mut dyn FnMut(SpeedtestTrafficDebit) -> Result<(), String>,
) -> Result<Option<u64>, String> {
    if !session.matches(request, worker_run_id) {
        return Err("speedtest-session-identity-mismatch".to_string());
    }
    if let Some(server_id) = request.speedtest_server_id {
        if server_id == 0 {
            return Err("speedtest-server-id-invalid".to_string());
        }
        session.selected_server_id = Some(server_id);
        return Ok(Some(server_id));
    }
    if session.selected_server_id.is_some() {
        return Ok(session.selected_server_id);
    }

    let candidates = retry_budgeted_route_measurement_on_loss_with_debit(
        remaining_traffic_budget,
        traffic_safety_reserve_bytes
            .checked_add(1)
            .unwrap_or(u64::MAX),
        || interface_counters(&request.route.l3_device),
        |budget| {
            let mut bounded_request = request.clone();
            bounded_request.traffic_budget_bytes = budget
                .checked_sub(traffic_safety_reserve_bytes)
                .ok_or_else(|| SPEEDTEST_TRAFFIC_BUDGET_EXHAUSTED.to_string())?;
            run_speedtest_go_server_list_attempt(
                &bounded_request,
                terminate,
                &scratch_path.with_extension("server-list"),
                session.credentials,
            )
        },
        on_traffic_debit,
    )?;

    let attempts: Vec<Option<u64>> = if candidates.is_empty() {
        vec![None]
    } else {
        candidates
            .into_iter()
            .take(MAX_AUTOMATIC_SERVER_ATTEMPTS)
            .map(Some)
            .collect()
    };
    let mut rejected = 0usize;
    let mut common_rejection = None::<String>;
    let mut mixed_rejections = false;
    let mut record_rejection = |reason: &str| {
        if let Some(common) = common_rejection.as_deref() {
            if common != reason {
                mixed_rejections = true;
            }
        } else {
            common_rejection = Some(reason.to_string());
        }
    };
    for (index, candidate) in attempts.into_iter().enumerate() {
        let attempt = retry_budgeted_route_measurement_on_loss_with_debit(
            remaining_traffic_budget,
            minimum_attempt_budget(SpeedtestDirection::Both, traffic_safety_reserve_bytes),
            || interface_counters(&request.route.l3_device),
            |budget| {
                let mut bounded_request = request.clone();
                bounded_request.traffic_budget_bytes = budget
                    .checked_sub(traffic_safety_reserve_bytes)
                    .ok_or_else(|| SPEEDTEST_TRAFFIC_BUDGET_EXHAUSTED.to_string())?;
                run_speedtest_with_pin(
                    &bounded_request,
                    SpeedtestDirection::Both,
                    accounting,
                    attest_accounting_epoch,
                    terminate,
                    &scratch_path.with_extension(format!("server-attempt-{}", index + 1)),
                    wait_for_route_ready(request, terminate)?,
                    session.credentials,
                    candidate,
                    &session.route_pin,
                )
            },
            on_traffic_debit,
        );

        match attempt {
            Ok((SpeedtestTerminal::Complete(result), Some(sample))) => {
                let Some(selected) = result.server_id.filter(|value| *value > 0) else {
                    eprintln!(
                        "speedtest-qualification-attempt-failed attempt={} code=speedtest-server-id-missing",
                        index + 1
                    );
                    record_rejection("server-id-missing");
                    rejected += 1;
                    continue;
                };
                if let Some(reason) = speedtest_qualification_rejection(&result, &sample) {
                    eprintln!(
                        "{}",
                        qualification_rejection_log_line(index + 1, &result, &sample, reason)
                    );
                    record_rejection(reason.code());
                    rejected += 1;
                } else {
                    session.selected_server_id = Some(selected);
                    return Ok(Some(selected));
                }
            }
            Ok((SpeedtestTerminal::Cancelled, _)) => {
                return Err("speedtest-server-qualification-cancelled".to_string())
            }
            Ok((SpeedtestTerminal::Failed { code }, _)) => {
                if !server_attempt_is_retryable(&code) {
                    return Err(code);
                }
                let reason = retryable_server_rejection_code(&code);
                eprintln!(
                    "speedtest-qualification-attempt-failed attempt={} code={code}",
                    index + 1
                );
                record_rejection(reason);
                rejected += 1;
            }
            Ok((SpeedtestTerminal::Complete(_), None)) => {
                return Err("speedtest-server-qualification-evidence-missing".to_string())
            }
            Err(error) if server_attempt_is_retryable(&error) => {
                let reason = retryable_server_rejection_code(&error);
                eprintln!(
                    "speedtest-qualification-attempt-failed attempt={} code={error}",
                    index + 1
                );
                record_rejection(reason);
                rejected += 1;
            }
            Err(error) => return Err(error),
        }
    }

    let reason = if mixed_rejections {
        "mixed"
    } else {
        common_rejection.as_deref().unwrap_or("unknown")
    };
    Err(format!("speedtest-qualification-{reason}-after-{rejected}"))
}

fn run_speedtest_go_server_list_attempt(
    request: &OperationRequest,
    terminate: &AtomicBool,
    scratch_path: &Path,
    credentials: BackendCredentials,
) -> Result<Vec<u64>, String> {
    let source = request
        .route
        .source_ip
        .filter(|value| matches!(value, IpAddr::V4(_)))
        .ok_or_else(|| "speedtest-source-ipv4-required".to_string())?;
    let arguments = vec![
        "--list".to_string(),
        "--ping-mode".to_string(),
        "http".to_string(),
        "--source".to_string(),
        source.to_string(),
        "--dns-bind-source".to_string(),
    ];
    let initial_route = wait_for_route_ready(request, terminate)?;
    let counters_before = interface_counters(&request.route.l3_device)?;
    let (mut output_guard, stdout, stderr) = BackendOutputGuard::create(scratch_path)?;
    let mut child = BackendChild::spawn(&arguments, stdout, stderr, credentials)?;
    let deadline = Instant::now() + MAX_SERVER_LIST_RUNTIME;
    let mut next_route_check = Instant::now() + ROUTE_RECHECK_INTERVAL;
    loop {
        if terminate.load(Ordering::Relaxed) {
            child.stop_and_reap()?;
            return Err("speedtest-server-list-cancelled".to_string());
        }
        if child.try_wait()? {
            break;
        }
        if Instant::now() >= deadline {
            child.stop_and_reap()?;
            return Err("speedtest-server-list-timeout".to_string());
        }
        let counters = interface_counters(&request.route.l3_device)?;
        let deltas = counter_deltas(counters_before, counters)?;
        if deltas.0.saturating_add(deltas.1) > request.traffic_budget_bytes {
            child.stop_and_reap()?;
            return Err(SPEEDTEST_TRAFFIC_LIMIT_REACHED.to_string());
        }
        if Instant::now() >= next_route_check {
            match attest_route(request) {
                Ok(current) if current.identity == initial_route.identity => {}
                Ok(_) => {
                    child.stop_and_reap()?;
                    return Err("speedtest-route-drift".to_string());
                }
                Err(error) => {
                    child.stop_and_reap()?;
                    return Err(error);
                }
            }
            next_route_check = Instant::now() + ROUTE_RECHECK_INTERVAL;
        }
        thread::sleep(POLL_INTERVAL);
    }
    if !child.finish()? {
        return Ok(Vec::new());
    }
    if attest_route(request)?.identity != initial_route.identity {
        return Err("speedtest-route-drift".to_string());
    }
    let output = read_bounded_file(&mut output_guard.output, OUTPUT_LIMIT)?;
    speedtest_go_server_candidates(&output)
}

/// Server discovery is not measurement evidence.  If the selected route goes
/// temporarily offline while discovery is running, discard that attempt,
/// wait on current route state, and start discovery from scratch.  The wait
/// owns no retry count or private deadline; cancellation and the enclosing
/// operation deadline remain the only terminal conditions.
#[cfg(test)]
fn retry_unmeasured_route_operation<T, Attempt, Wait>(
    mut attempt: Attempt,
    mut wait_until_ready: Wait,
) -> Result<T, String>
where
    Attempt: FnMut() -> Result<T, String>,
    Wait: FnMut() -> Result<(), String>,
{
    loop {
        match attempt() {
            Err(error) if error == "speedtest-route-not-ready" => wait_until_ready()?,
            result => return result,
        }
    }
}

/// A route-interrupted or backend-success-without-traffic transfer is not
/// measurement evidence.  Discard the whole attempt.  Route readiness loss is
/// retried only after the attempt's own state-driven readiness check proves
/// that the immutable selected route is ready again.  A successful backend
/// which did not move even the minimum route-proof bytes is retried at most
/// three times for the same logical measurement; this covers a transient empty
/// speedtest-go run without turning a persistently broken backend or a route
/// counter which cannot prove traffic into an unbounded loop.
///
/// Every attempt is bracketed by interface counters and charged before its
/// outcome is interpreted, including failed and discarded attempts.  Counter
/// resets, static route errors, cancellation, deadline expiry, debit failures,
/// and budget exhaustion stay fail-closed.  No sleep or retry timer is owned by
/// this helper.
#[cfg(test)]
fn retry_budgeted_route_measurement_on_loss<T, Counters, Attempt>(
    remaining_traffic_budget: &mut u64,
    minimum_attempt_budget: u64,
    counters: Counters,
    attempt: Attempt,
) -> Result<T, String>
where
    Counters: FnMut() -> Result<(u64, u64), String>,
    Attempt: FnMut(u64) -> Result<T, String>,
{
    retry_budgeted_route_measurement_on_loss_with_debit(
        remaining_traffic_budget,
        minimum_attempt_budget,
        counters,
        attempt,
        &mut |_| Ok(()),
    )
}

fn retry_budgeted_route_measurement_on_loss_with_debit<T, Counters, Attempt>(
    remaining_traffic_budget: &mut u64,
    minimum_attempt_budget: u64,
    mut counters: Counters,
    mut attempt: Attempt,
    on_traffic_debit: &mut dyn FnMut(SpeedtestTrafficDebit) -> Result<(), String>,
) -> Result<T, String>
where
    Counters: FnMut() -> Result<(u64, u64), String>,
    Attempt: FnMut(u64) -> Result<T, String>,
{
    let mut unproved_traffic_attempts = 0_u8;
    loop {
        if *remaining_traffic_budget < minimum_attempt_budget {
            return Err(SPEEDTEST_TRAFFIC_BUDGET_EXHAUSTED.to_string());
        }
        let before = counters()?;
        let outcome = attempt(*remaining_traffic_budget);
        let after = counters()?;
        let debit = debit_traffic_budget(remaining_traffic_budget, before, after)?;
        on_traffic_debit(debit)?;
        match outcome {
            Err(error) if error == "speedtest-route-not-ready" => continue,
            Err(error) if error == "speedtest-route-traffic-unproved" => {
                unproved_traffic_attempts = unproved_traffic_attempts
                    .checked_add(1)
                    .ok_or_else(|| error.clone())?;
                if unproved_traffic_attempts < MAX_UNPROVED_TRAFFIC_ATTEMPTS {
                    continue;
                }
                return Err(error);
            }
            result => return result,
        }
    }
}

/// Close the readiness-to-route-pin TOCTOU window without turning static nft
/// or mwan3 configuration failures into retries.  A failed `mwan3 use ... env`
/// is retried only when a fresh structured route read proves that the selected
/// route became temporarily unavailable in that exact window.
fn acquire_route_pin_when_ready<T, Wait, Acquire, Attest>(
    mut wait_until_ready: Wait,
    mut acquire: Acquire,
    mut attest_after_failure: Attest,
) -> Result<T, String>
where
    Wait: FnMut() -> Result<(), String>,
    Acquire: FnMut() -> Result<T, String>,
    Attest: FnMut() -> Result<(), String>,
{
    loop {
        wait_until_ready()?;
        match acquire() {
            Ok(value) => return Ok(value),
            Err(error) if error == "speedtest-mwan3-environment-failed" => {
                match attest_after_failure() {
                    Err(route_error) if route_error == "speedtest-route-not-ready" => continue,
                    Ok(()) => return Err(error),
                    Err(route_error) => return Err(route_error),
                }
            }
            Err(error) => return Err(error),
        }
    }
}

fn speedtest_go_server_candidates(output: &str) -> Result<Vec<u64>, String> {
    let mut candidates = Vec::<(u64, u64)>::new();
    for line in output.lines() {
        let Some(after_open) = line.trim_start().strip_prefix('[') else {
            continue;
        };
        let Some((id_text, details)) = after_open.split_once(']') else {
            return Err("speedtest-server-list-invalid".to_string());
        };
        let id = id_text
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| "speedtest-server-list-invalid".to_string())?;
        let latency_micros = details
            .split_ascii_whitespace()
            .find_map(|token| token.strip_suffix("ms"))
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map(|value| (value * 1000.0).round() as u64);
        let Some(latency_micros) = latency_micros else {
            if details
                .split_ascii_whitespace()
                .any(|token| token.eq_ignore_ascii_case("timeout"))
            {
                continue;
            }
            return Err("speedtest-server-list-invalid".to_string());
        };
        candidates.push((latency_micros, id));
    }
    candidates.sort_unstable();
    let mut server_ids = Vec::with_capacity(candidates.len());
    for (_, server_id) in candidates {
        if !server_ids.contains(&server_id) {
            server_ids.push(server_id);
        }
    }
    Ok(server_ids)
}

pub(crate) fn rate_matches_payload_timing(
    reported_kbps: u64,
    payload_bytes: u64,
    elapsed_ms: u64,
) -> bool {
    if reported_kbps == 0 || payload_bytes == 0 || elapsed_ms == 0 {
        return false;
    }
    let Some(reported_work) = u128::from(reported_kbps).checked_mul(u128::from(elapsed_ms)) else {
        return false;
    };
    let Some(payload_work) = u128::from(payload_bytes).checked_mul(8) else {
        return false;
    };
    let (Some(reported_scaled), Some(payload_upper), Some(payload_scaled), Some(reported_upper)) = (
        reported_work.checked_mul(100),
        payload_work.checked_mul(MAX_RATE_PAYLOAD_TIMING_RATIO_PERCENT),
        payload_work.checked_mul(100),
        reported_work.checked_mul(MAX_RATE_PAYLOAD_TIMING_RATIO_PERCENT),
    ) else {
        return false;
    };
    reported_scaled <= payload_upper && payload_scaled <= reported_upper
}

/// Keep speedtest-go's application-rate observation only while its own
/// payload/time evidence is internally plausible, then cap it by independent
/// UID-scoped wire accounting.  In particular, speedtest-go 1.7.10 counts
/// upload bytes as its request body is read; a shaped socket may therefore
/// leave some of those bytes buffered or discarded at the capture boundary.
/// Those backend-only bytes must neither invalidate otherwise isolated shaped
/// traffic nor inflate the rate consumed by native Auto-Tune.
pub(crate) fn bounded_achieved_kbps(
    reported_kbps: u64,
    payload_bytes: u64,
    controlled_wire_bytes: u64,
    elapsed_ms: u64,
) -> Result<u64, String> {
    if !rate_matches_payload_timing(reported_kbps, payload_bytes, elapsed_ms) {
        return Err(SPEEDTEST_RATE_PAYLOAD_TIMING_MISMATCH.to_string());
    }
    let payload_kbps = u64::try_from(
        u128::from(payload_bytes)
            .checked_mul(8)
            .ok_or_else(|| "speedtest-payload-rate-overflow".to_string())?
            / u128::from(elapsed_ms),
    )
    .map_err(|_| "speedtest-payload-rate-overflow".to_string())?;
    let wire_kbps = u64::try_from(
        u128::from(controlled_wire_bytes)
            .checked_mul(8)
            .ok_or_else(|| "speedtest-wire-rate-overflow".to_string())?
            / u128::from(elapsed_ms),
    )
    .map_err(|_| "speedtest-wire-rate-overflow".to_string())?;
    let bounded = reported_kbps.min(payload_kbps).min(wire_kbps);
    if bounded == 0 {
        return Err("speedtest-bounded-rate-invalid".to_string());
    }
    Ok(bounded)
}

#[cfg(test)]
fn upload_counter_lag_is_bounded(interface_bytes: u64, payload_bytes: u64) -> bool {
    if interface_bytes >= payload_bytes {
        return true;
    }
    u128::from(payload_bytes - interface_bytes) * 100
        <= u128::from(payload_bytes) * MAX_UPLOAD_COUNTER_LAG_PERCENT
}

fn counter_ratio_is_at_least(left: u64, right: u64, minimum_percent: u128) -> bool {
    if left == 0 || right == 0 || minimum_percent > 100 {
        return false;
    }
    let smaller = u128::from(left.min(right));
    let larger = u128::from(left.max(right));
    smaller * 100 >= larger * minimum_percent
}

fn speedtest_qualification_rejection(
    result: &SpeedtestResult,
    sample: &SpeedtestLoadSample,
) -> Option<SpeedtestQualificationRejection> {
    if sample.direction != SpeedtestDirection::Both || result.direction != SpeedtestDirection::Both
    {
        return Some(SpeedtestQualificationRejection::Direction);
    }
    let (Some(download_kbps), Some(upload_kbps)) = (result.download_kbps, result.upload_kbps)
    else {
        return Some(SpeedtestQualificationRejection::RateMissing);
    };
    let (Some(download_elapsed_ms), Some(upload_elapsed_ms)) =
        (sample.download_elapsed_ms, sample.upload_elapsed_ms)
    else {
        return Some(SpeedtestQualificationRejection::DurationMissing);
    };
    if sample.controlled_rx_payload_bytes < MIN_ROUTE_PROOF_BYTES {
        return Some(SpeedtestQualificationRejection::DownloadPayloadProof);
    }
    if sample.controlled_tx_payload_bytes < MIN_ROUTE_PROOF_BYTES {
        return Some(SpeedtestQualificationRejection::UploadPayloadProof);
    }
    if sample.controlled_rx_wire_bytes < MIN_ROUTE_PROOF_BYTES {
        return Some(SpeedtestQualificationRejection::DownloadWireProof);
    }
    if sample.controlled_tx_wire_bytes < MIN_ROUTE_PROOF_BYTES {
        return Some(SpeedtestQualificationRejection::UploadWireProof);
    }
    // The route device can account a GRO aggregate once while CAKE with
    // split-gso accounts the repeated protocol headers of every resulting
    // segment.  Therefore route RX is not an upper bound for CAKE Sent.
    // It must still cover the application payload received through the
    // attested route; CAKE and isolated wire accounting are checked below.
    if sample.aggregate_rx_bytes < sample.controlled_rx_payload_bytes {
        return Some(SpeedtestQualificationRejection::AggregateRxBelowPayload);
    }
    if sample.confidence_rx_bytes < sample.controlled_rx_wire_bytes {
        return Some(SpeedtestQualificationRejection::ConfidenceRxBelowWire);
    }
    if sample.controlled_rx_wire_bytes < sample.controlled_rx_payload_bytes {
        return Some(SpeedtestQualificationRejection::RxWireBelowPayload);
    }
    if !counter_ratio_is_at_least(
        sample.confidence_tx_bytes,
        sample.controlled_tx_wire_bytes,
        MIN_QUALIFICATION_COUNTER_CONFIDENCE_PERCENT,
    ) {
        return Some(SpeedtestQualificationRejection::ConfidenceTxWireRatio);
    }
    if !counter_ratio_is_at_least(
        sample.controlled_tx_wire_bytes,
        sample.controlled_tx_payload_bytes,
        MIN_SHAPED_UPLOAD_PAYLOAD_WIRE_PERCENT,
    ) {
        return Some(SpeedtestQualificationRejection::TxWirePayloadRatio);
    }
    if bounded_achieved_kbps(
        download_kbps,
        sample.controlled_rx_payload_bytes,
        sample.controlled_rx_wire_bytes,
        download_elapsed_ms,
    )
    .is_err()
    {
        return Some(SpeedtestQualificationRejection::DownloadRatePayloadTiming);
    }
    if bounded_achieved_kbps(
        upload_kbps,
        sample.controlled_tx_payload_bytes,
        sample.controlled_tx_wire_bytes,
        upload_elapsed_ms,
    )
    .is_err()
    {
        return Some(SpeedtestQualificationRejection::UploadRatePayloadTiming);
    }
    None
}

#[cfg(test)]
fn speedtest_qualification_is_plausible(
    result: &SpeedtestResult,
    sample: &SpeedtestLoadSample,
) -> bool {
    speedtest_qualification_rejection(result, sample).is_none()
}

fn qualification_rejection_log_line(
    attempt: usize,
    result: &SpeedtestResult,
    sample: &SpeedtestLoadSample,
    reason: SpeedtestQualificationRejection,
) -> String {
    let value = |value: Option<u64>| {
        value
            .map(|number| number.to_string())
            .unwrap_or_else(|| "-".to_string())
    };
    let mut line = format!(
        "speedtest-qualification-rejected attempt={attempt} server_id={} reason={} dl_kbps={} ul_kbps={} aggregate_rx={} aggregate_tx={} confidence_rx={} confidence_tx={} controlled_rx_wire={} controlled_tx_wire={} controlled_rx_payload={} controlled_tx_payload={} counter_elapsed_ms={} download_elapsed_ms={} upload_elapsed_ms={}",
        value(result.server_id),
        reason.code(),
        value(result.download_kbps),
        value(result.upload_kbps),
        sample.aggregate_rx_bytes,
        sample.aggregate_tx_bytes,
        sample.confidence_rx_bytes,
        sample.confidence_tx_bytes,
        sample.controlled_rx_wire_bytes,
        sample.controlled_tx_wire_bytes,
        sample.controlled_rx_payload_bytes,
        sample.controlled_tx_payload_bytes,
        sample.counter_elapsed_ms,
        value(sample.download_elapsed_ms),
        value(sample.upload_elapsed_ms),
    );
    if line.len() > MAX_QUALIFICATION_DIAGNOSTIC_BYTES {
        line = format!(
            "speedtest-qualification-rejected attempt={attempt} server_id={} reason={} diagnostic=overflow",
            value(result.server_id),
            reason.code(),
        );
    }
    line
}

fn server_attempt_is_retryable(error: &str) -> bool {
    matches!(
        error,
        "speedtest-backend-failed"
            | "speedtest-download-evidence-missing"
            | "speedtest-upload-evidence-missing"
            | "speedtest-rate-missing"
            | "speedtest-server-id-missing"
            | "speedtest-server-identity-mismatch"
    ) || error.starts_with("speedtest-json-")
}

/// Return true only for evidence-shape errors emitted after speedtest-go has
/// exited successfully and the selected route has already passed its traffic
/// proof.  A Full Auto-Tune caller may treat these as a bounded unmeasurable
/// transfer, but must never extend that policy to backend, route, accounting,
/// identity, budget, cancellation, timeout or output-file failures.
pub(crate) fn completed_speedtest_output_is_unmeasurable(error: &str) -> bool {
    matches!(
        error,
        "speedtest-download-missing"
            | "speedtest-upload-missing"
            | "speedtest-direction-result-missing"
            | "speedtest-duration-missing"
            | "speedtest-download-evidence-missing"
            | "speedtest-upload-evidence-missing"
            | "speedtest-direction-evidence-missing"
            | "speedtest-used-bytes-invalid"
            | "speedtest-used-bytes-duplicate"
            | "speedtest-duration-invalid"
            | "speedtest-rate-invalid"
            | "speedtest-direction-result-unavailable"
    ) || error.starts_with("speedtest-json-")
}

fn backend_output_rejection_log_line(error: &str, output: &str, stderr_bytes: u64) -> String {
    let code = if error.len() <= 64
        && error
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        error
    } else {
        "invalid"
    };
    format!(
        "speedtest-backend-output-rejected code={code} stdout_bytes={} stdout_lines={} json_candidates={} stderr_bytes={stderr_bytes}",
        output.len(),
        output.lines().count(),
        output
            .lines()
            .filter(|line| line.trim_start().starts_with('{'))
            .count(),
    )
}

fn retryable_server_rejection_code(error: &str) -> &'static str {
    match error {
        "speedtest-backend-failed" => "backend",
        "speedtest-download-evidence-missing" => "download-evidence",
        "speedtest-upload-evidence-missing" => "upload-evidence",
        "speedtest-rate-missing" => "rate-missing",
        "speedtest-server-id-missing" => "server-id-missing",
        "speedtest-server-identity-mismatch" => "server-identity",
        _ if error.starts_with("speedtest-json-") => "json",
        _ => "unknown",
    }
}

fn debit_traffic_budget(
    remaining: &mut u64,
    before: (u64, u64),
    after: (u64, u64),
) -> Result<SpeedtestTrafficDebit, String> {
    let deltas = counter_deltas(before, after)?;
    let consumed = deltas
        .0
        .checked_add(deltas.1)
        .ok_or_else(|| "speedtest-traffic-budget-overflow".to_string())?;
    *remaining = remaining
        .checked_sub(consumed)
        .ok_or_else(|| "speedtest-traffic-budget-exceeded".to_string())?;
    Ok(SpeedtestTrafficDebit {
        rx_bytes: deltas.0,
        tx_bytes: deltas.1,
    })
}

fn run_speedtest_with_pin(
    request: &OperationRequest,
    direction: SpeedtestDirection,
    accounting: &SpeedtestAccountingPlan,
    attest_accounting_epoch: &mut dyn FnMut() -> Result<(), String>,
    terminate: &AtomicBool,
    terminal_path: &Path,
    initial_route: RouteSnapshot,
    credentials: BackendCredentials,
    server_id: Option<u64>,
    route_pin: &NftRoutePin,
) -> Result<(SpeedtestTerminal, Option<SpeedtestLoadSample>), String> {
    attest_accounting_epoch()?;
    let (mut output_guard, stdout, stderr) = BackendOutputGuard::create(terminal_path)?;
    let arguments = speedtest_go_arguments(request, direction, server_id)?;
    let accounting_started = Instant::now();
    // Keep the independently attested byte windows strictly nested.  The
    // physical route is the outer traffic-budget window, managed CAKE is the
    // shaped confidence window, and the UID-owned nft rule is the inner
    // controlled-transfer window.  The closing reads below deliberately use
    // the reverse order; no scheduler timing or byte tolerance is needed to
    // prove route >= CAKE >= controlled download bytes.
    let counters_before = interface_counters(&request.route.l3_device)?;
    let cake_before = if direction != SpeedtestDirection::Upload {
        accounting
            .download
            .as_ref()
            .map(|target| cake_counter_snapshot(target, CakeCounterDirection::Download))
            .transpose()?
    } else {
        None
    };
    let upload_cake_before = if direction != SpeedtestDirection::Download {
        accounting
            .upload
            .as_ref()
            .map(|target| cake_counter_snapshot(target, CakeCounterDirection::Upload))
            .transpose()?
    } else {
        None
    };
    let controlled_before = route_pin.traffic_counters()?;
    let started = Instant::now();
    let deadline_ms = request.deadline_unix_ms.saturating_sub(rating::epoch_ms()?);
    if deadline_ms == 0 {
        return Err("speedtest-deadline-expired".to_string());
    }
    let deadline = Instant::now() + Duration::from_millis(deadline_ms).min(MAX_BACKEND_RUNTIME);
    let mut child = BackendChild::spawn(&arguments, stdout, stderr, credentials)?;
    let mut next_route_check = Instant::now() + ROUTE_RECHECK_INTERVAL;
    loop {
        if terminate.load(Ordering::Relaxed) {
            child.stop_and_reap()?;
            return Ok((SpeedtestTerminal::Cancelled, None));
        }
        if child.try_wait()? {
            break;
        }
        if Instant::now() >= deadline {
            child.stop_and_reap()?;
            return Err("speedtest-timeout".to_string());
        }
        let counters = interface_counters(&request.route.l3_device)?;
        let deltas = counter_deltas(counters_before, counters)?;
        if deltas.0.saturating_add(deltas.1) > request.traffic_budget_bytes {
            child.stop_and_reap()?;
            return Err(SPEEDTEST_TRAFFIC_LIMIT_REACHED.to_string());
        }
        if Instant::now() >= next_route_check {
            match attest_route(request) {
                Ok(current) if current.identity == initial_route.identity => {}
                Ok(_) => {
                    child.stop_and_reap()?;
                    return Err("speedtest-route-drift".to_string());
                }
                Err(error) => {
                    child.stop_and_reap()?;
                    return Err(error);
                }
            }
            next_route_check = Instant::now() + ROUTE_RECHECK_INTERVAL;
        }
        thread::sleep(POLL_INTERVAL);
    }
    if !child.finish()? {
        return Err("speedtest-backend-failed".to_string());
    }
    let controlled_after = route_pin.traffic_counters()?;
    let controlled_deltas = counter_deltas(
        (controlled_before.rx_bytes, controlled_before.tx_bytes),
        (controlled_after.rx_bytes, controlled_after.tx_bytes),
    )?;
    let cake_after = cake_before
        .as_ref()
        .map(|_| {
            cake_counter_snapshot(
                accounting.download.as_ref().ok_or_else(|| {
                    "speedtest-download-accounting-qdisc-state-mismatch".to_string()
                })?,
                CakeCounterDirection::Download,
            )
        })
        .transpose()?;
    let upload_cake_after = upload_cake_before
        .as_ref()
        .map(|_| {
            cake_counter_snapshot(
                accounting.upload.as_ref().ok_or_else(|| {
                    "speedtest-upload-accounting-qdisc-state-mismatch".to_string()
                })?,
                CakeCounterDirection::Upload,
            )
        })
        .transpose()?;
    let final_counters = interface_counters(&request.route.l3_device)?;
    attest_accounting_epoch()?;
    let deltas = counter_deltas(counters_before, final_counters)?;
    let counter_elapsed_ms = u64::try_from(accounting_started.elapsed().as_millis())
        .unwrap_or(u64::MAX)
        .max(1);
    let confidence_rx_bytes = match (cake_before.as_ref(), cake_after.as_ref()) {
        (Some(before), Some(after)) => {
            cake_counter_delta(before, after, CakeCounterDirection::Download)?
        }
        (None, None) => deltas.0,
        _ => return Err("speedtest-download-accounting-qdisc-state-mismatch".to_string()),
    };
    let confidence_tx_bytes = match (upload_cake_before.as_ref(), upload_cake_after.as_ref()) {
        (Some(before), Some(after)) => {
            cake_counter_delta(before, after, CakeCounterDirection::Upload)?
        }
        (None, None) => deltas.1,
        _ => return Err("speedtest-upload-accounting-qdisc-state-mismatch".to_string()),
    };
    if deltas.0.saturating_add(deltas.1) > request.traffic_budget_bytes {
        return Err(SPEEDTEST_TRAFFIC_LIMIT_REACHED.to_string());
    }
    let final_route = attest_route(request)?;
    if final_route.identity != initial_route.identity {
        return Err("speedtest-route-drift".to_string());
    }
    prove_route_traffic(direction, deltas)?;
    let output = read_bounded_file(&mut output_guard.output, OUTPUT_LIMIT)?;
    let mut parsed = match parse_speedtest_go_measurement(&output, direction) {
        Ok(parsed) => parsed,
        Err(error) => {
            let stderr_bytes = output_guard
                .stderr
                .metadata()
                .map(|metadata| metadata.len())
                .unwrap_or(u64::MAX);
            eprintln!(
                "{}",
                backend_output_rejection_log_line(&error, &output, stderr_bytes)
            );
            return Err(error);
        }
    };
    if server_id.is_some() && parsed.result.server_id != server_id {
        return Err("speedtest-server-identity-mismatch".to_string());
    }
    parsed.result.rx_bytes = deltas.0;
    parsed.result.tx_bytes = deltas.1;
    parsed.result.elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let load_sample = SpeedtestLoadSample {
        direction,
        aggregate_rx_bytes: deltas.0,
        aggregate_tx_bytes: deltas.1,
        confidence_rx_bytes,
        confidence_tx_bytes,
        controlled_rx_wire_bytes: controlled_deltas.0,
        controlled_tx_wire_bytes: controlled_deltas.1,
        controlled_rx_payload_bytes: parsed.controlled_rx_payload_bytes,
        controlled_tx_payload_bytes: parsed.controlled_tx_payload_bytes,
        counter_elapsed_ms,
        download_elapsed_ms: parsed.download_elapsed_ms,
        upload_elapsed_ms: parsed.upload_elapsed_ms,
    };
    Ok((
        SpeedtestTerminal::Complete(parsed.result),
        Some(load_sample),
    ))
}

fn speedtest_go_arguments(
    request: &OperationRequest,
    direction: SpeedtestDirection,
    server_id: Option<u64>,
) -> Result<Vec<String>, String> {
    let source = request
        .route
        .source_ip
        .ok_or_else(|| "speedtest-source-ip-missing".to_string())?;
    if !matches!(source, IpAddr::V4(_)) {
        return Err("speedtest-source-ipv4-required".to_string());
    }
    let mut arguments = vec![
        "--json".to_string(),
        "--unix".to_string(),
        "--ping-mode".to_string(),
        "http".to_string(),
    ];
    if let Some(server_id) = server_id {
        if server_id == 0 {
            return Err("speedtest-server-id-invalid".to_string());
        }
        arguments.push("--server".to_string());
        arguments.push(server_id.to_string());
    }
    match direction {
        SpeedtestDirection::Download => arguments.push("--no-upload".to_string()),
        SpeedtestDirection::Upload => arguments.push("--no-download".to_string()),
        SpeedtestDirection::Both => {}
    }
    arguments.push("--source".to_string());
    arguments.push(source.to_string());
    arguments.push("--dns-bind-source".to_string());
    Ok(arguments)
}

fn attest_route(request: &OperationRequest) -> Result<RouteSnapshot, String> {
    let mode = request.route.mode.as_str();
    let member = request.route.mwan3_member.as_deref().unwrap_or("");
    let spec = RouteSpec::new(mode, member, &request.route.l3_device);
    let snapshot = inspect_route(&spec).map_err(|_| "speedtest-route-inspection-failed")?;
    if !snapshot.online || (request.route.mode == OperationRouteMode::Main && !snapshot.active) {
        return Err("speedtest-route-not-ready".to_string());
    }
    route_matches_request(request, &snapshot.identity)?;
    Ok(snapshot)
}

/// Wait for a temporarily unavailable selected route only before starting a
/// backend.  Live-transfer and final route checks deliberately continue to use
/// `attest_route()` directly, so a real failover or identity drift stops the
/// measurement immediately.  This wait has no private timeout or delayed
/// action: current route state is re-read until the operation's existing
/// deadline or explicit cancellation.
fn wait_for_route_ready(
    request: &OperationRequest,
    terminate: &AtomicBool,
) -> Result<RouteSnapshot, String> {
    wait_for_route_ready_with(
        || attest_route(request),
        || terminate.load(Ordering::Relaxed),
        || Ok(rating::epoch_ms()? >= request.deadline_unix_ms),
        || thread::sleep(ROUTE_RECHECK_INTERVAL),
    )
}

fn wait_for_route_ready_with<Attest, Cancelled, DeadlineExpired, Pause>(
    mut attest: Attest,
    mut cancelled: Cancelled,
    mut deadline_expired: DeadlineExpired,
    mut pause: Pause,
) -> Result<RouteSnapshot, String>
where
    Attest: FnMut() -> Result<RouteSnapshot, String>,
    Cancelled: FnMut() -> bool,
    DeadlineExpired: FnMut() -> Result<bool, String>,
    Pause: FnMut(),
{
    loop {
        if cancelled() {
            return Err("speedtest-route-wait-cancelled".to_string());
        }
        if deadline_expired()? {
            return Err("speedtest-deadline-expired".to_string());
        }
        match attest() {
            Ok(snapshot) => return Ok(snapshot),
            Err(error) if error == "speedtest-route-not-ready" => pause(),
            Err(error) => return Err(error),
        }
    }
}

fn route_matches_request(request: &OperationRequest, actual: &RouteIdentity) -> Result<(), String> {
    if actual.mode != request.route.mode.as_str()
        || actual.member != request.route.mwan3_member.as_deref().unwrap_or("")
        || actual.device != request.route.l3_device
        || actual.source_ip
            != request
                .route
                .source_ip
                .map(|value| value.to_string())
                .unwrap_or_default()
        || !optional_route_number_matches(request.route.fwmark, &actual.fwmark)
        || !optional_route_number_matches(request.route.routing_table, &actual.table)
    {
        return Err("speedtest-route-identity-mismatch".to_string());
    }
    Ok(())
}

fn optional_route_number_matches(expected: Option<u32>, actual: &str) -> bool {
    match expected {
        None => actual.is_empty() || actual == "main",
        Some(expected) => parse_route_number(actual) == Some(expected),
    }
}

fn parse_route_number(value: &str) -> Option<u32> {
    value
        .strip_prefix("0x")
        .and_then(|value| u32::from_str_radix(value, 16).ok())
        .or_else(|| value.parse().ok())
}

fn backend_credentials() -> Result<BackendCredentials, String> {
    let path = Path::new(PASSWD);
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("speedtest-user-database-unavailable: {error}"))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.len() > 64 * 1024
    {
        return Err("speedtest-user-database-invalid".to_string());
    }
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("speedtest-user-database-read-failed: {error}"))?;
    parse_backend_credentials(&contents)
}

fn parse_backend_credentials(contents: &str) -> Result<BackendCredentials, String> {
    let mut credentials = None;
    for line in contents.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.first().copied() != Some(SPEEDTEST_USER) {
            continue;
        }
        if fields.len() != 7 || credentials.is_some() {
            return Err("speedtest-user-record-invalid".to_string());
        }
        let uid = fields[2]
            .parse::<u32>()
            .map_err(|_| "speedtest-user-id-invalid".to_string())?;
        let gid = fields[3]
            .parse::<u32>()
            .map_err(|_| "speedtest-group-id-invalid".to_string())?;
        if uid == 0 || gid == 0 {
            return Err("speedtest-user-must-be-unprivileged".to_string());
        }
        credentials = Some(BackendCredentials { uid, gid });
    }
    credentials.ok_or_else(|| "speedtest-user-unavailable".to_string())
}

fn resolve_mwan3_mark_mask(request: &OperationRequest) -> Result<u32, String> {
    let member = request
        .route
        .mwan3_member
        .as_deref()
        .ok_or_else(|| "speedtest-mwan3-member-missing".to_string())?;
    if !safe_route_word(member) {
        return Err("speedtest-mwan3-member-invalid".to_string());
    }
    let output = run_bounded_command(MWAN3, &["use", member, "exec", "env"])?;
    if !output.0 {
        return Err("speedtest-mwan3-environment-failed".to_string());
    }
    let environment = String::from_utf8(output.1)
        .map_err(|_| "speedtest-mwan3-environment-invalid".to_string())?;
    let device = unique_environment_value(&environment, "DEVICE")?
        .ok_or_else(|| "speedtest-mwan3-device-missing".to_string())?;
    let source_ip = unique_environment_value(&environment, "SRCIP")?
        .ok_or_else(|| "speedtest-mwan3-source-missing".to_string())?;
    let mark_mask = unique_environment_value(&environment, "FWMARK")?
        .ok_or_else(|| "speedtest-mwan3-mask-missing".to_string())?;
    if device != request.route.l3_device
        || source_ip.parse::<IpAddr>().ok() != request.route.source_ip
    {
        return Err("speedtest-mwan3-environment-drift".to_string());
    }
    parse_hex_u32(mark_mask, "speedtest-mwan3-mask-invalid")
}

fn unique_environment_value<'a>(input: &'a str, key: &str) -> Result<Option<&'a str>, String> {
    let prefix = format!("{key}=");
    let mut result = None;
    for line in input.lines() {
        let Some(value) = line.strip_prefix(&prefix) else {
            continue;
        };
        if value.is_empty() || value.contains(|character: char| character.is_control()) {
            return Err("speedtest-mwan3-environment-invalid".to_string());
        }
        if result.replace(value).is_some() {
            return Err("speedtest-mwan3-environment-duplicate".to_string());
        }
    }
    Ok(result)
}

fn parse_hex_u32(value: &str, code: &str) -> Result<u32, String> {
    let digits = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .ok_or_else(|| code.to_string())?;
    if digits.is_empty() || digits.len() > 8 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(code.to_string());
    }
    u32::from_str_radix(digits, 16).map_err(|_| code.to_string())
}

fn safe_route_word(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'@' | b'-')
        })
}

fn canonical_hex_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn route_pin_table_name(job_id: &str, worker_run_id: &str) -> Result<String, String> {
    if !canonical_hex_id(job_id) || !canonical_hex_id(worker_run_id) {
        return Err("speedtest-route-pin-identity-invalid".to_string());
    }
    Ok(format!(
        "cake_st_{}_{}",
        &job_id[..12],
        &worker_run_id[..12]
    ))
}

fn route_pin_owner(job_id: &str, worker_run_id: &str) -> Result<String, String> {
    if !canonical_hex_id(job_id) || !canonical_hex_id(worker_run_id) {
        return Err("speedtest-route-pin-identity-invalid".to_string());
    }
    Ok(format!("cake-autorate-speedtest:{job_id}:{worker_run_id}"))
}

fn nft_route_pin_batch(
    table: &str,
    owner: &str,
    uid: u32,
    route_mark: Option<(u32, u32)>,
) -> String {
    let mut batch = String::with_capacity(1536);
    batch.push_str("{\"nftables\":[{\"add\":{\"table\":{\"family\":\"inet\",\"name\":\"");
    batch.push_str(table);
    batch.push_str("\",\"comment\":\"");
    batch.push_str(owner);
    batch.push_str("\"}}},{\"add\":{\"counter\":{\"family\":\"inet\",\"table\":\"");
    batch.push_str(table);
    batch.push_str("\",\"name\":\"");
    batch.push_str(ACCOUNTING_RX_COUNTER);
    batch.push_str("\",\"comment\":\"");
    batch.push_str(owner);
    batch.push_str("\"}}},{\"add\":{\"counter\":{\"family\":\"inet\",\"table\":\"");
    batch.push_str(table);
    batch.push_str("\",\"name\":\"");
    batch.push_str(ACCOUNTING_TX_COUNTER);
    batch.push_str("\",\"comment\":\"");
    batch.push_str(owner);
    batch.push_str("\"}}},{\"add\":{\"chain\":{\"family\":\"inet\",\"table\":\"");
    batch.push_str(table);
    batch.push_str("\",\"name\":\"output\",\"type\":\"route\",\"hook\":\"output\",\"prio\":-148,\"policy\":\"accept\"}}},{\"add\":{\"rule\":{\"family\":\"inet\",\"table\":\"");
    batch.push_str(table);
    batch.push_str("\",\"chain\":\"output\",\"expr\":[{\"match\":{\"op\":\"==\",\"left\":{\"meta\":{\"key\":\"skuid\"}},\"right\":");
    let _ = write!(batch, "{uid}");
    batch.push_str("}},{\"counter\":\"");
    batch.push_str(ACCOUNTING_TX_COUNTER);
    batch.push_str("\"}");
    if let Some((clear_mask, fwmark)) = route_mark {
        batch.push_str(",{\"mangle\":{\"key\":{\"meta\":{\"key\":\"mark\"}},\"value\":{\"|\":[{\"&\":[{\"meta\":{\"key\":\"mark\"}},");
        let _ = write!(batch, "{clear_mask}");
        batch.push_str("]},");
        let _ = write!(batch, "{fwmark}");
        batch.push_str("]}}}");
    }
    batch.push_str("]}}},{\"add\":{\"chain\":{\"family\":\"inet\",\"table\":\"");
    batch.push_str(table);
    batch.push_str("\",\"name\":\"input\",\"type\":\"filter\",\"hook\":\"input\",\"prio\":-148,\"policy\":\"accept\"}}},{\"add\":{\"rule\":{\"family\":\"inet\",\"table\":\"");
    batch.push_str(table);
    batch.push_str("\",\"chain\":\"input\",\"expr\":[{\"match\":{\"op\":\"==\",\"left\":{\"meta\":{\"key\":\"skuid\"}},\"right\":");
    let _ = write!(batch, "{uid}");
    batch.push_str("}},{\"counter\":\"");
    batch.push_str(ACCOUNTING_RX_COUNTER);
    batch.push_str("\"}]}}}]}\n");
    batch
}

fn path_text(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| "speedtest-private-path-invalid".to_string())
}

fn run_bounded_command(program: &str, arguments: &[&str]) -> Result<(bool, Vec<u8>), String> {
    let output = Command::new(program)
        .args(arguments)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("speedtest-route-command-failed: {error}"))?;
    if output.stdout.len() > COMMAND_OUTPUT_LIMIT || output.stderr.len() > COMMAND_OUTPUT_LIMIT {
        return Err("speedtest-route-command-output-too-large".to_string());
    }
    Ok((output.status.success(), output.stdout))
}

fn attest_named_route_pin(table: &str, owner: &str) -> Result<(), String> {
    let output = run_bounded_command(NFT, &["-j", "list", "table", "inet", table])?;
    if !output.0 {
        return Err("speedtest-route-pin-missing".to_string());
    }
    let json =
        String::from_utf8(output.1).map_err(|_| "speedtest-route-pin-json-invalid".to_string())?;
    if json_string(&json, "comment")?.as_deref() != Some(owner) {
        return Err("speedtest-route-pin-owner-mismatch".to_string());
    }
    Ok(())
}

fn cleanup_named_route_pin(table: &str, owner: &str) -> Result<(), String> {
    let listed = run_bounded_command(NFT, &["-j", "list", "table", "inet", table])?;
    if !listed.0 {
        let tables = run_bounded_command(NFT, &["-j", "list", "tables"])?;
        if tables.0 {
            return Ok(());
        }
        return Err("speedtest-route-pin-inspection-failed".to_string());
    }
    let json =
        String::from_utf8(listed.1).map_err(|_| "speedtest-route-pin-json-invalid".to_string())?;
    if json_string(&json, "comment")?.as_deref() != Some(owner) {
        return Err("speedtest-route-pin-owner-mismatch".to_string());
    }
    let deleted = run_bounded_command(NFT, &["delete", "table", "inet", table])?;
    if !deleted.0 {
        return Err("speedtest-route-pin-delete-failed".to_string());
    }
    if run_bounded_command(NFT, &["-j", "list", "table", "inet", table])?.0 {
        return Err("speedtest-route-pin-delete-unverified".to_string());
    }
    Ok(())
}

pub(crate) fn cleanup_route_pin(job_id: &str, worker_run_id: &str) -> Result<(), String> {
    validate_utility_binary(Path::new(NFT), "nft")?;
    let table = route_pin_table_name(job_id, worker_run_id)?;
    let owner = route_pin_owner(job_id, worker_run_id)?;
    cleanup_named_route_pin(&table, &owner)
}

pub(crate) fn read_live_traffic_counters_bounded<F>(
    job_id: &str,
    worker_run_id: &str,
    timeout: Duration,
    should_cancel: F,
) -> Result<Option<SpeedtestTrafficCounters>, String>
where
    F: Fn() -> bool,
{
    validate_utility_binary(Path::new(NFT), "nft")?;
    let started = Instant::now();
    let deadline = started.checked_add(timeout).unwrap_or(started);
    let table = route_pin_table_name(job_id, worker_run_id)?;
    let owner = route_pin_owner(job_id, worker_run_id)?;
    let listed = run_bounded_accounting_command(
        &["-j", "list", "counters", "inet", &table],
        accounting_time_remaining(deadline)?,
        &should_cancel,
    )?;
    if !listed.0 {
        if run_bounded_accounting_command(
            &["-j", "list", "tables"],
            accounting_time_remaining(deadline)?,
            &should_cancel,
        )?
        .0
        {
            return Ok(None);
        }
        return Err("speedtest-accounting-inspection-failed".to_string());
    }
    let json =
        String::from_utf8(listed.1).map_err(|_| "speedtest-accounting-json-invalid".to_string())?;
    parse_named_traffic_counters(&json, &table, &owner).map(Some)
}

fn read_named_traffic_counters(
    table: &str,
    owner: &str,
) -> Result<Option<SpeedtestTrafficCounters>, String> {
    let listed = run_bounded_accounting_command(
        &["-j", "list", "counters", "inet", table],
        Duration::from_secs(2),
        &|| false,
    )?;
    if !listed.0 {
        return Ok(None);
    }
    let json =
        String::from_utf8(listed.1).map_err(|_| "speedtest-accounting-json-invalid".to_string())?;
    parse_named_traffic_counters(&json, table, owner).map(Some)
}

fn parse_named_traffic_counters(
    json: &str,
    table: &str,
    owner: &str,
) -> Result<SpeedtestTrafficCounters, String> {
    let rx_bytes = named_counter_bytes(json, table, ACCOUNTING_RX_COUNTER, owner)?
        .ok_or_else(|| "speedtest-accounting-rx-missing".to_string())?;
    let tx_bytes = named_counter_bytes(json, table, ACCOUNTING_TX_COUNTER, owner)?
        .ok_or_else(|| "speedtest-accounting-tx-missing".to_string())?;
    Ok(SpeedtestTrafficCounters { rx_bytes, tx_bytes })
}

fn accounting_time_remaining(deadline: Instant) -> Result<Duration, String> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| "speedtest-accounting-timeout".to_string())
}

fn run_bounded_accounting_command<F>(
    arguments: &[&str],
    timeout: Duration,
    should_cancel: &F,
) -> Result<(bool, Vec<u8>), String>
where
    F: Fn() -> bool,
{
    let spec = SpawnSpec {
        program: PathBuf::from(NFT),
        arguments: arguments.iter().map(OsString::from).collect(),
        environment: Vec::new(),
    };
    let output =
        run_bounded_command_output(&spec, timeout, COMMAND_OUTPUT_LIMIT, || should_cancel())
            .map_err(|error| match error.as_str() {
                "bounded-command-timeout" => "speedtest-accounting-timeout".to_string(),
                "bounded-command-cancelled" => "speedtest-accounting-cancelled".to_string(),
                "bounded-command-output-too-large" => {
                    "speedtest-accounting-output-too-large".to_string()
                }
                _ => format!("speedtest-accounting-command-failed: {error}"),
            })?;
    Ok((output.status.success(), output.stdout))
}

fn named_counter_bytes(
    input: &str,
    expected_table: &str,
    expected_name: &str,
    expected_owner: &str,
) -> Result<Option<u64>, String> {
    let mut remaining = input;
    let mut found = None;
    while let Some((_, after_key)) = remaining.split_once("\"counter\"") {
        let after_colon = after_key
            .trim_start()
            .strip_prefix(':')
            .ok_or_else(|| "speedtest-accounting-json-invalid".to_string())?
            .trim_start();
        let Some(object) = after_colon.strip_prefix('{') else {
            remaining = after_colon.get(1..).unwrap_or_default();
            continue;
        };
        let end = object
            .find('}')
            .ok_or_else(|| "speedtest-accounting-json-invalid".to_string())?;
        let fields = &object[..end];
        remaining = &object[end + 1..];
        if json_string(fields, "name")?.as_deref() != Some(expected_name) {
            continue;
        }
        if json_string(fields, "table")?.as_deref() != Some(expected_table)
            || json_string(fields, "comment")?.as_deref() != Some(expected_owner)
        {
            return Err("speedtest-accounting-identity-mismatch".to_string());
        }
        let bytes = json_u64(fields, "bytes")?
            .ok_or_else(|| "speedtest-accounting-bytes-missing".to_string())?;
        if found.replace(bytes).is_some() {
            return Err("speedtest-accounting-counter-duplicate".to_string());
        }
    }
    Ok(found)
}

fn validate_utility_binary(path: &Path, name: &str) -> Result<PathBuf, String> {
    let entry = fs::symlink_metadata(path).map_err(|_| format!("speedtest-{name}-unavailable"))?;
    if (!entry.is_file() && !entry.file_type().is_symlink())
        || entry.uid() != 0
        || !utility_path_ancestors_are_trusted(path)
    {
        return Err(format!("speedtest-{name}-invalid"));
    }
    let target = fs::canonicalize(path).map_err(|_| format!("speedtest-{name}-invalid"))?;
    let metadata =
        fs::symlink_metadata(&target).map_err(|_| format!("speedtest-{name}-invalid"))?;
    if !utility_target_path_is_trusted(&target)
        || !utility_path_ancestors_are_trusted(&target)
        || !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.mode() & 0o111 == 0
        || metadata.mode() & 0o6000 != 0
    {
        return Err(format!("speedtest-{name}-invalid"));
    }
    Ok(target)
}

fn utility_target_path_is_trusted(path: &Path) -> bool {
    ["/bin", "/sbin", "/usr/bin", "/usr/sbin", "/usr/libexec"]
        .into_iter()
        .map(Path::new)
        .any(|prefix| path.starts_with(prefix) && path != prefix)
}

fn utility_path_ancestors_are_trusted(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let mut parent = path.parent();
    while let Some(directory) = parent {
        let Ok(metadata) = fs::symlink_metadata(directory) else {
            return false;
        };
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return false;
        }
        parent = directory.parent();
    }
    true
}

fn validate_backend_binary(path: &Path) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "speedtest-backend-unavailable".to_string())?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.mode() & 0o111 == 0
        || metadata.mode() & 0o6000 != 0
    {
        return Err("speedtest-backend-invalid".to_string());
    }
    Ok(())
}

fn private_output_file(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("speedtest-output-create-failed: {error}"))
}

fn cleanup_backend_output(output: &Path, stderr: &Path) {
    let _ = fs::remove_file(output);
    let _ = fs::remove_file(stderr);
}

impl CakeCounterTarget {
    #[cfg(test)]
    pub(crate) fn for_test(
        device: &str,
        ifindex: u32,
        kind: CakeCounterKind,
        handle: &str,
    ) -> Self {
        Self {
            device: device.to_string(),
            ifindex,
            kind,
            handle: handle.to_string(),
        }
    }

    pub(crate) fn bind_current(
        device: &str,
        kind: CakeCounterKind,
        direction: CakeCounterDirection,
    ) -> Result<Self, String> {
        Self::bind(device, kind, None, None, direction)
    }

    pub(crate) fn bind_exact(
        device: &str,
        kind: CakeCounterKind,
        handle: &str,
        expected_ifindex: Option<u32>,
        direction: CakeCounterDirection,
    ) -> Result<Self, String> {
        Self::bind(device, kind, Some(handle), expected_ifindex, direction)
    }

    fn bind(
        device: &str,
        kind: CakeCounterKind,
        expected_handle: Option<&str>,
        expected_ifindex: Option<u32>,
        direction: CakeCounterDirection,
    ) -> Result<Self, String> {
        validate_counter_device(device)?;
        if let Some(handle) = expected_handle {
            validate_counter_handle(handle, direction)?;
        }
        let prefix = direction.prefix();
        let ifindex_before = interface_ifindex(device, direction)?;
        if expected_ifindex.is_some_and(|expected| expected != ifindex_before) {
            return Err(format!("{prefix}-device-ifindex-mismatch"));
        }
        let output = cake_qdisc_output(device, direction)?;
        let (handle, _) =
            parse_exact_root_cake_sent_bytes(&output, kind, expected_handle, direction)?;
        let ifindex_after = interface_ifindex(device, direction)?;
        if ifindex_after != ifindex_before {
            return Err(format!("{prefix}-device-ifindex-changed"));
        }
        Ok(Self {
            device: device.to_string(),
            ifindex: ifindex_before,
            kind,
            handle,
        })
    }
}

fn cake_counter_snapshot(
    target: &CakeCounterTarget,
    direction: CakeCounterDirection,
) -> Result<CakeCounterSnapshot, String> {
    validate_counter_device(&target.device)?;
    validate_counter_handle(&target.handle, direction)?;
    let prefix = direction.prefix();
    if interface_ifindex(&target.device, direction)? != target.ifindex {
        return Err(format!("{prefix}-device-ifindex-changed"));
    }
    let output = cake_qdisc_output(&target.device, direction)?;
    let (handle, sent_bytes) =
        parse_exact_root_cake_sent_bytes(&output, target.kind, Some(&target.handle), direction)?;
    if interface_ifindex(&target.device, direction)? != target.ifindex {
        return Err(format!("{prefix}-device-ifindex-changed"));
    }
    Ok(CakeCounterSnapshot {
        device: target.device.clone(),
        kind: target.kind.as_str().to_string(),
        handle,
        sent_bytes,
    })
}

fn cake_qdisc_output(device: &str, direction: CakeCounterDirection) -> Result<String, String> {
    let prefix = direction.prefix();
    let tc = validate_utility_binary(Path::new(TC), "tc")?;
    let spec = SpawnSpec {
        program: tc,
        arguments: ["-s", "qdisc", "show", "dev", device]
            .into_iter()
            .map(OsString::from)
            .collect(),
        environment: Vec::new(),
    };
    let output =
        run_bounded_command_output(&spec, Duration::from_secs(2), COMMAND_OUTPUT_LIMIT, || {
            false
        })
        .map_err(|error| format!("{prefix}-qdisc-command-{error}"))?;
    if !output.status.success() {
        return Err(format!("{prefix}-qdisc-inspection-failed"));
    }
    String::from_utf8(output.stdout).map_err(|_| format!("{prefix}-qdisc-output-invalid"))
}

fn parse_exact_root_cake_sent_bytes(
    output: &str,
    expected_kind: CakeCounterKind,
    expected_handle: Option<&str>,
    direction: CakeCounterDirection,
) -> Result<(String, u64), String> {
    let prefix = direction.prefix();
    let lines = output.lines().collect::<Vec<_>>();
    let mut found = None;
    for (index, line) in lines.iter().enumerate() {
        let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
        if fields.first() != Some(&"qdisc") || !fields.contains(&"root") {
            continue;
        }
        let Some(kind @ ("cake" | "cake_mq")) = fields.get(1).copied() else {
            continue;
        };
        let handle = fields
            .get(2)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("{prefix}-qdisc-handle-missing"))?;
        if kind != expected_kind.as_str()
            || expected_handle.is_some_and(|expected| expected != *handle)
        {
            return Err(format!("{prefix}-qdisc-identity-mismatch"));
        }
        let sent_bytes = lines[index + 1..]
            .iter()
            .take_while(|candidate| {
                candidate
                    .split_ascii_whitespace()
                    .next()
                    .is_none_or(|first| first != "qdisc")
            })
            .find_map(|candidate| {
                let fields = candidate.split_ascii_whitespace().collect::<Vec<_>>();
                (fields.first() == Some(&"Sent") && fields.get(2) == Some(&"bytes"))
                    .then(|| fields.get(1).copied())
                    .flatten()
            })
            .ok_or_else(|| format!("{prefix}-qdisc-bytes-missing"))?
            .parse::<u64>()
            .map_err(|_| format!("{prefix}-qdisc-bytes-invalid"))?;
        if found.replace((handle.to_string(), sent_bytes)).is_some() {
            return Err(format!("{prefix}-qdisc-duplicate"));
        }
    }
    found.ok_or_else(|| format!("{prefix}-qdisc-missing"))
}

fn cake_counter_delta(
    before: &CakeCounterSnapshot,
    after: &CakeCounterSnapshot,
    direction: CakeCounterDirection,
) -> Result<u64, String> {
    let prefix = direction.prefix();
    if before.device != after.device || before.kind != after.kind || before.handle != after.handle {
        return Err(format!("{prefix}-qdisc-changed"));
    }
    after
        .sent_bytes
        .checked_sub(before.sent_bytes)
        .ok_or_else(|| format!("{prefix}-qdisc-counter-reset"))
}

fn validate_counter_device(device: &str) -> Result<(), String> {
    if device.is_empty()
        || !device.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'@' | b'-')
        })
    {
        return Err("speedtest-counter-device-invalid".to_string());
    }
    Ok(())
}

fn validate_counter_handle(handle: &str, direction: CakeCounterDirection) -> Result<(), String> {
    let valid = handle.strip_suffix(':').is_some_and(|prefix| {
        !prefix.is_empty() && prefix.bytes().all(|byte| byte.is_ascii_hexdigit())
    });
    if !valid {
        return Err(format!("{}-qdisc-handle-invalid", direction.prefix()));
    }
    Ok(())
}

fn interface_ifindex(device: &str, direction: CakeCounterDirection) -> Result<u32, String> {
    validate_counter_device(device)?;
    let root = std::env::var_os("CAKE_AUTORATE_SYS_CLASS_NET")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/sys/class/net"));
    let prefix = direction.prefix();
    let value = fs::read_to_string(root.join(device).join("ifindex"))
        .map_err(|_| format!("{prefix}-device-ifindex-unavailable"))?;
    value
        .trim()
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("{prefix}-device-ifindex-invalid"))
}

fn interface_counters(device: &str) -> Result<(u64, u64), String> {
    validate_counter_device(device)?;
    let root = std::env::var_os("CAKE_AUTORATE_SYS_CLASS_NET")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/sys/class/net"));
    Ok((
        read_counter(&root.join(device).join("statistics/rx_bytes"))?,
        read_counter(&root.join(device).join("statistics/tx_bytes"))?,
    ))
}

fn read_counter(path: &Path) -> Result<u64, String> {
    let value =
        fs::read_to_string(path).map_err(|_| "speedtest-counter-read-failed".to_string())?;
    value
        .trim()
        .parse()
        .map_err(|_| "speedtest-counter-invalid".to_string())
}

fn counter_deltas(before: (u64, u64), after: (u64, u64)) -> Result<(u64, u64), String> {
    if after.0 < before.0 || after.1 < before.1 {
        return Err("speedtest-counter-reset".to_string());
    }
    Ok((after.0 - before.0, after.1 - before.1))
}

fn prove_route_traffic(direction: SpeedtestDirection, deltas: (u64, u64)) -> Result<(), String> {
    let proved = match direction {
        SpeedtestDirection::Download => deltas.0 >= MIN_ROUTE_PROOF_BYTES,
        SpeedtestDirection::Upload => deltas.1 >= MIN_ROUTE_PROOF_BYTES,
        SpeedtestDirection::Both => {
            deltas.0 >= MIN_ROUTE_PROOF_BYTES && deltas.1 >= MIN_ROUTE_PROOF_BYTES
        }
    };
    proved
        .then_some(())
        .ok_or_else(|| "speedtest-route-traffic-unproved".to_string())
}

fn read_bounded(path: &Path, limit: usize) -> Result<String, String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "speedtest-output-missing".to_string())?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err("speedtest-output-invalid".to_string());
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take((limit + 1) as u64).read_to_end(&mut bytes))
        .map_err(|_| "speedtest-output-read-failed".to_string())?;
    if bytes.len() > limit {
        return Err("speedtest-output-oversized".to_string());
    }
    String::from_utf8(bytes).map_err(|_| "speedtest-output-not-utf8".to_string())
}

fn read_bounded_file(file: &mut File, limit: usize) -> Result<String, String> {
    let metadata = file
        .metadata()
        .map_err(|_| "speedtest-output-missing".to_string())?;
    if !metadata.is_file() || metadata.len() > limit as u64 {
        return Err("speedtest-output-invalid".to_string());
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|_| "speedtest-output-read-failed".to_string())?;
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "speedtest-output-read-failed".to_string())?;
    if bytes.len() > limit {
        return Err("speedtest-output-oversized".to_string());
    }
    String::from_utf8(bytes).map_err(|_| "speedtest-output-not-utf8".to_string())
}

#[cfg(test)]
fn parse_speedtest_go(
    output: &str,
    direction: SpeedtestDirection,
) -> Result<SpeedtestResult, String> {
    parse_speedtest_go_measurement(output, direction).map(|parsed| parsed.result)
}

fn parse_speedtest_go_measurement(
    output: &str,
    direction: SpeedtestDirection,
) -> Result<ParsedSpeedtestGo, String> {
    let json = output
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with('{'))
        .ok_or_else(|| "speedtest-json-missing".to_string())?;
    let download_rate = json_number(json, "dl_speed")?;
    let upload_rate = json_number(json, "ul_speed")?;
    let download_kbps = match direction {
        SpeedtestDirection::Upload => optional_unused_rate(download_rate)?,
        SpeedtestDirection::Download | SpeedtestDirection::Both => {
            download_rate.map(bytes_per_second_to_kbps).transpose()?
        }
    };
    let upload_kbps = match direction {
        SpeedtestDirection::Download => optional_unused_rate(upload_rate)?,
        SpeedtestDirection::Upload | SpeedtestDirection::Both => {
            upload_rate.map(bytes_per_second_to_kbps).transpose()?
        }
    };
    match direction {
        SpeedtestDirection::Download if download_kbps.is_none() => {
            return Err("speedtest-download-missing".to_string())
        }
        SpeedtestDirection::Upload if upload_kbps.is_none() => {
            return Err("speedtest-upload-missing".to_string())
        }
        SpeedtestDirection::Both if download_kbps.is_none() || upload_kbps.is_none() => {
            return Err("speedtest-direction-result-missing".to_string())
        }
        _ => {}
    }
    let duration = json_object(json, "test_duration")?
        .ok_or_else(|| "speedtest-duration-missing".to_string())?;
    let download_elapsed_ms = duration_ms(json_u64(duration, "download")?)?;
    let upload_elapsed_ms = duration_ms(json_u64(duration, "upload")?)?;
    let download_payload_bytes = used_payload_bytes(output, "Download")?;
    let upload_payload_bytes = used_payload_bytes(output, "Upload")?;
    match direction {
        SpeedtestDirection::Download
            if download_payload_bytes.is_none() || download_elapsed_ms.is_none() =>
        {
            return Err("speedtest-download-evidence-missing".to_string())
        }
        SpeedtestDirection::Upload
            if upload_payload_bytes.is_none() || upload_elapsed_ms.is_none() =>
        {
            return Err("speedtest-upload-evidence-missing".to_string())
        }
        SpeedtestDirection::Both
            if download_payload_bytes.is_none()
                || upload_payload_bytes.is_none()
                || download_elapsed_ms.is_none()
                || upload_elapsed_ms.is_none() =>
        {
            return Err("speedtest-direction-evidence-missing".to_string())
        }
        _ => {}
    }
    let result = SpeedtestResult {
        direction,
        download_kbps,
        upload_kbps,
        rx_bytes: 0,
        tx_bytes: 0,
        elapsed_ms: 0,
        server_id: json_number(json, "id")?.map(|value| value.round() as u64),
        server_name: json_string(json, "name")?.unwrap_or_default(),
        server_sponsor: json_string(json, "sponsor")?.unwrap_or_default(),
    };
    Ok(ParsedSpeedtestGo {
        result,
        controlled_rx_payload_bytes: download_payload_bytes.unwrap_or(0),
        controlled_tx_payload_bytes: upload_payload_bytes.unwrap_or(0),
        download_elapsed_ms,
        upload_elapsed_ms,
    })
}

fn used_payload_bytes(output: &str, label: &str) -> Result<Option<u64>, String> {
    let prefix = format!("{label}:");
    let mut value = None;
    for line in output.lines() {
        let line = line.trim_start_matches(|character: char| character.is_whitespace());
        if !line.starts_with(&prefix) {
            continue;
        }
        let (_, tail) = line
            .split_once("(Used: ")
            .ok_or_else(|| "speedtest-used-bytes-invalid".to_string())?;
        let (number, _) = tail
            .split_once("MB)")
            .ok_or_else(|| "speedtest-used-bytes-invalid".to_string())?;
        let megabytes = number
            .parse::<f64>()
            .map_err(|_| "speedtest-used-bytes-invalid".to_string())?;
        if !megabytes.is_finite() || megabytes <= 0.0 || megabytes > 1_000_000.0 {
            return Err("speedtest-used-bytes-invalid".to_string());
        }
        let bytes = (megabytes * 1_000_000.0).round() as u64;
        if value.replace(bytes).is_some() {
            return Err("speedtest-used-bytes-duplicate".to_string());
        }
    }
    Ok(value)
}

fn json_object<'a>(input: &'a str, key: &str) -> Result<Option<&'a str>, String> {
    let Some(tail) = json_value_tail(input, key) else {
        return Ok(None);
    };
    let tail = tail
        .strip_prefix('{')
        .ok_or_else(|| "speedtest-json-object-invalid".to_string())?;
    let end = tail
        .find('}')
        .ok_or_else(|| "speedtest-json-object-invalid".to_string())?;
    Ok(Some(&tail[..end]))
}

fn json_u64(input: &str, key: &str) -> Result<Option<u64>, String> {
    let Some(tail) = json_value_tail(input, key) else {
        return Ok(None);
    };
    if let Some(suffix) = tail.strip_prefix("null") {
        if suffix.is_empty()
            || suffix.chars().next().is_some_and(|character| {
                matches!(character, ',' | '}' | ']') || character.is_ascii_whitespace()
            })
        {
            return Ok(None);
        }
        return Err("speedtest-json-integer-invalid".to_string());
    }
    let (digits, suffix) = if let Some(quoted) = tail.strip_prefix('"') {
        let end = quoted
            .find('"')
            .ok_or_else(|| "speedtest-json-integer-invalid".to_string())?;
        (&quoted[..end], &quoted[end + 1..])
    } else {
        let end = tail
            .find(|character: char| {
                matches!(character, ',' | '}' | ']') || character.is_ascii_whitespace()
            })
            .unwrap_or(tail.len());
        (&tail[..end], &tail[end..])
    };
    if digits.is_empty()
        || digits.bytes().any(|byte| !byte.is_ascii_digit())
        || (!suffix.is_empty()
            && !suffix.chars().next().is_some_and(|character| {
                matches!(character, ',' | '}' | ']') || character.is_ascii_whitespace()
            }))
    {
        return Err("speedtest-json-integer-invalid".to_string());
    }
    digits
        .parse::<u64>()
        .map(Some)
        .map_err(|_| "speedtest-json-integer-invalid".to_string())
}

fn duration_ms(nanoseconds: Option<u64>) -> Result<Option<u64>, String> {
    nanoseconds
        .map(|value| {
            if value == 0 {
                return Err("speedtest-duration-invalid".to_string());
            }
            value
                .checked_add(999_999)
                .map(|rounded| rounded / 1_000_000)
                .filter(|milliseconds| *milliseconds > 0 && *milliseconds <= 180_000)
                .ok_or_else(|| "speedtest-duration-invalid".to_string())
        })
        .transpose()
}

fn optional_unused_rate(value: Option<f64>) -> Result<Option<u64>, String> {
    match value {
        None | Some(0.0) => Ok(None),
        Some(value) => bytes_per_second_to_kbps(value).map(Some),
    }
}

fn bytes_per_second_to_kbps(value: f64) -> Result<u64, String> {
    if !value.is_finite() || value <= 0.0 || value > 125_000_000_000.0 {
        return Err("speedtest-rate-invalid".to_string());
    }
    let rounded = (value * 8.0 / 1000.0).round() as u64;
    if rounded == 0 {
        return Err("speedtest-direction-result-unavailable".to_string());
    }
    Ok(rounded)
}

fn json_number(input: &str, key: &str) -> Result<Option<f64>, String> {
    let Some(tail) = json_value_tail(input, key) else {
        return Ok(None);
    };
    if let Some(quoted) = tail.strip_prefix('"') {
        let end = quoted
            .find('"')
            .ok_or_else(|| "speedtest-json-number-invalid".to_string())?;
        return quoted[..end]
            .parse::<f64>()
            .map(Some)
            .map_err(|_| "speedtest-json-number-invalid".to_string());
    }
    let end = tail
        .find(|character: char| !matches!(character, '0'..='9' | '.' | '-' | '+' | 'e' | 'E'))
        .unwrap_or(tail.len());
    if end == 0 {
        return Err("speedtest-json-number-invalid".to_string());
    }
    tail[..end]
        .parse::<f64>()
        .map(Some)
        .map_err(|_| "speedtest-json-number-invalid".to_string())
}

fn json_string(input: &str, key: &str) -> Result<Option<String>, String> {
    let Some(tail) = json_value_tail(input, key) else {
        return Ok(None);
    };
    let Some(mut tail) = tail.strip_prefix('"') else {
        return Err("speedtest-json-string-invalid".to_string());
    };
    let mut value = String::new();
    while !tail.is_empty() && value.len() <= 256 {
        let character = tail.chars().next().expect("non-empty tail");
        tail = &tail[character.len_utf8()..];
        match character {
            '"' => return Ok(Some(value)),
            '\\' => {
                let escaped = tail.chars().next().ok_or("speedtest-json-escape-invalid")?;
                tail = &tail[escaped.len_utf8()..];
                value.push(match escaped {
                    '"' => '"',
                    '\\' => '\\',
                    '/' => '/',
                    'b' => '\u{0008}',
                    'f' => '\u{000c}',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    _ => return Err("speedtest-json-escape-invalid".to_string()),
                });
            }
            value_character if value_character.is_control() => {
                return Err("speedtest-json-string-invalid".to_string())
            }
            value_character => value.push(value_character),
        }
    }
    Err("speedtest-json-string-invalid".to_string())
}

fn json_value_tail<'a>(input: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let tail = input.split_once(&needle)?.1;
    let tail = tail.trim_start();
    tail.strip_prefix(':').map(str::trim_start)
}

impl SpeedtestTerminal {
    fn encode(&self, job_id: &str, worker_run_id: &str) -> Result<String, String> {
        let (state, code, result) = match self {
            Self::Complete(result) => ("complete", "", Some(result)),
            Self::Cancelled => ("cancelled", "", None),
            Self::Failed { code } => ("failed", code.as_str(), None),
        };
        Ok(format!(
            "cake-autorate-speedtest-terminal\t1\njob_id={job_id}\nworker_run_id={worker_run_id}\nstate={state}\ncode={code}\ndirection={}\ndownload_kbps={}\nupload_kbps={}\nrx_bytes={}\ntx_bytes={}\nelapsed_ms={}\nserver_id={}\nserver_name_hex={}\nserver_sponsor_hex={}\n\n",
            result.map(|value| value.direction.as_str()).unwrap_or(""),
            optional_u64(result.and_then(|value| value.download_kbps)),
            optional_u64(result.and_then(|value| value.upload_kbps)),
            result.map(|value| value.rx_bytes).unwrap_or(0),
            result.map(|value| value.tx_bytes).unwrap_or(0),
            result.map(|value| value.elapsed_ms).unwrap_or(0),
            optional_u64(result.and_then(|value| value.server_id)),
            hex_encode(result.map(|value| value.server_name.as_str()).unwrap_or("")),
            hex_encode(result.map(|value| value.server_sponsor.as_str()).unwrap_or("")),
        ))
    }
}

impl SpeedtestTerminalRecord {
    fn decode(input: &str) -> Result<Self, String> {
        let mut lines = input.split('\n');
        if lines.next() != Some("cake-autorate-speedtest-terminal\t1") {
            return Err("speedtest-terminal-header-invalid".to_string());
        }
        let job_id = terminal_field(&mut lines, "job_id")?;
        let worker_run_id = terminal_field(&mut lines, "worker_run_id")?;
        let state = terminal_field(&mut lines, "state")?;
        let code = terminal_field(&mut lines, "code")?;
        let direction = terminal_field(&mut lines, "direction")?;
        let download_kbps = terminal_optional_u64(&mut lines, "download_kbps")?;
        let upload_kbps = terminal_optional_u64(&mut lines, "upload_kbps")?;
        let rx_bytes = terminal_u64(&mut lines, "rx_bytes")?;
        let tx_bytes = terminal_u64(&mut lines, "tx_bytes")?;
        let elapsed_ms = terminal_u64(&mut lines, "elapsed_ms")?;
        let server_id = terminal_optional_u64(&mut lines, "server_id")?;
        let server_name = hex_decode(&terminal_field(&mut lines, "server_name_hex")?)?;
        let server_sponsor = hex_decode(&terminal_field(&mut lines, "server_sponsor_hex")?)?;
        if lines.next() != Some("") || lines.next() != Some("") || lines.next().is_some() {
            return Err("speedtest-terminal-trailing-data".to_string());
        }
        let terminal = match state.as_str() {
            "complete" if code.is_empty() => {
                let direction = SpeedtestDirection::parse(&direction)
                    .ok_or_else(|| "speedtest-terminal-direction-invalid".to_string())?;
                SpeedtestTerminal::Complete(SpeedtestResult {
                    direction,
                    download_kbps,
                    upload_kbps,
                    rx_bytes,
                    tx_bytes,
                    elapsed_ms,
                    server_id,
                    server_name,
                    server_sponsor,
                })
            }
            "cancelled" if code.is_empty() && direction.is_empty() => SpeedtestTerminal::Cancelled,
            "failed" if !code.is_empty() && direction.is_empty() => {
                SpeedtestTerminal::Failed { code }
            }
            _ => return Err("speedtest-terminal-state-invalid".to_string()),
        };
        Ok(Self {
            job_id,
            worker_run_id,
            terminal,
        })
    }
}

pub fn read_terminal_file(path: &Path) -> Result<SpeedtestTerminalRecord, String> {
    SpeedtestTerminalRecord::decode(&read_bounded(path, 8 * 1024)?)
}

fn terminal_field<'a>(
    lines: &mut impl Iterator<Item = &'a str>,
    name: &str,
) -> Result<String, String> {
    let line = lines
        .next()
        .ok_or_else(|| "speedtest-terminal-field-missing".to_string())?;
    line.strip_prefix(&format!("{name}="))
        .map(ToString::to_string)
        .ok_or_else(|| "speedtest-terminal-field-order-invalid".to_string())
}

fn terminal_optional_u64<'a>(
    lines: &mut impl Iterator<Item = &'a str>,
    name: &str,
) -> Result<Option<u64>, String> {
    let value = terminal_field(lines, name)?;
    if value.is_empty() {
        Ok(None)
    } else {
        value
            .parse()
            .map(Some)
            .map_err(|_| "speedtest-terminal-number-invalid".to_string())
    }
}

fn terminal_u64<'a>(lines: &mut impl Iterator<Item = &'a str>, name: &str) -> Result<u64, String> {
    terminal_field(lines, name)?
        .parse()
        .map_err(|_| "speedtest-terminal-number-invalid".to_string())
}

fn optional_u64(value: Option<u64>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn hex_encode(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn hex_decode(value: &str) -> Result<String, String> {
    if value.len() > 512 || value.len() % 2 != 0 {
        return Err("speedtest-terminal-text-invalid".to_string());
    }
    let bytes = value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair)
                .map_err(|_| "speedtest-terminal-text-invalid".to_string())?;
            u8::from_str_radix(text, 16).map_err(|_| "speedtest-terminal-text-invalid".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    String::from_utf8(bytes).map_err(|_| "speedtest-terminal-text-invalid".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::protocol::{
        OperationIdentity, OperationOrigin, OperationRouteIdentity, OperationTargetState,
    };
    use std::cell::{Cell, RefCell};
    use std::net::{IpAddr, Ipv4Addr};

    fn request(direction: SpeedtestDirection) -> OperationRequest {
        OperationRequest {
            identity: OperationIdentity {
                job_id: "1".repeat(32),
                job_token: "2".repeat(64),
                instance: "wan_sqm".to_string(),
                operation: OperationKind::Speedtest,
                target_interface: "pppoe-wan".to_string(),
                route_fingerprint: "3".repeat(64),
                config_fingerprint: "4".repeat(64),
                sqm_fingerprint: "5".repeat(64),
            },
            created_unix_ms: 1,
            deadline_unix_ms: u64::MAX,
            origin: OperationOrigin::Internal,
            backend: "speedtest-go".to_string(),
            speedtest_direction: Some(direction),
            speedtest_server_id: Some(17372),
            speedtest_topology: Some(super::super::protocol::SpeedtestTopology::Current),
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
            managed_sqm_section: None,
            profile: None,
            strategy: None,
            access_medium: None,
            access_source: None,
            access_confidence_percent: 0,
            capacity_learning_policy: None,
            service_dl_cap_kbps: None,
            service_ul_cap_kbps: None,
            allow_sqm_disable: false,
            allow_active_traffic: false,
            scheduled_auto_apply_requested: false,
            traffic_budget_bytes: 4_000_000_000,
        }
    }

    #[test]
    fn speedtest_worker_rejects_autotune_path_and_malformed_run_identity() {
        let terminate = AtomicBool::new(false);
        let error = run_speedtest_worker(
            ["--review".to_string(), "/tmp/review.json".to_string()].into_iter(),
            &terminate,
        )
        .unwrap_err();
        assert_eq!(error, "unsupported speedtest worker argument: --review");

        let error = run_speedtest_worker(
            [
                "--request".to_string(),
                "/tmp/request".to_string(),
                "--terminal".to_string(),
                "/tmp/terminal".to_string(),
                "--permit".to_string(),
                "/tmp/permit".to_string(),
                "--worker-run-id".to_string(),
                "not-lower-hex".to_string(),
            ]
            .into_iter(),
            &terminate,
        )
        .unwrap_err();
        assert_eq!(
            error,
            "worker run id must be exactly 32 lowercase hexadecimal characters"
        );
    }

    #[test]
    fn bootstrap_speedtest_rechecks_absence_after_every_measurement_outcome() {
        let baseline = || AbsentRuntimeBaseline {
            planned_sqm_section: "wan_sqm".to_string(),
            target_interface: "eth1".to_string(),
            target_ifindex: 7,
            route_fingerprint: "1".repeat(64),
            config_fingerprint: "2".repeat(64),
            sqm_fingerprint: "3".repeat(64),
            kernel_topology_fingerprint: "4".repeat(64),
            kernel_namespace_seed: "5".repeat(32),
        };
        let events = RefCell::new(Vec::new());
        let result = run_bootstrap_unshaped_with_absence(
            || {
                events.borrow_mut().push("capture");
                Ok(baseline())
            },
            || {
                events.borrow_mut().push("measure");
                Ok(7_u8)
            },
            |_| {
                events.borrow_mut().push("reattest");
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(result, 7);
        assert_eq!(*events.borrow(), ["capture", "measure", "reattest"]);

        let measured = Cell::new(false);
        let error = run_bootstrap_unshaped_with_absence::<u8, _, _, _>(
            || Err("uci-is-not-absent".to_string()),
            || {
                measured.set(true);
                Ok(1)
            },
            |_| Ok(()),
        )
        .unwrap_err();
        assert_eq!(
            error,
            "speedtest-bootstrap-absence-preflight-failed: uci-is-not-absent"
        );
        assert!(
            !measured.get(),
            "traffic must not start after a failed absence witness"
        );

        let error = run_bootstrap_unshaped_with_absence(
            || Ok(baseline()),
            || Ok(9_u8),
            |_| Err("route-drift".to_string()),
        )
        .unwrap_err();
        assert_eq!(error, "speedtest-bootstrap-absence-changed: route-drift");

        let error = run_bootstrap_unshaped_with_absence::<u8, _, _, _>(
            || Ok(baseline()),
            || Err("backend-failed".to_string()),
            |_| Err("kernel-drift".to_string()),
        )
        .unwrap_err();
        assert_eq!(
            error,
            "backend-failed; speedtest-bootstrap-absence-changed: kernel-drift"
        );
    }

    #[test]
    fn unshaped_worker_accepts_only_its_exact_runtime_permit() {
        use crate::autotune::{AutotuneProfile, LinkKind};
        use crate::operations::autotune_runtime::{
            RuntimeBaseline, RuntimeQdiscKind, RuntimeRateBounds,
        };
        use crate::operations::full_autotune::MeasurementTopology;
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "cake-speedtest-runtime-permit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let store = RuntimeOverrideStore::open(&root).unwrap();
        let mut operation = request(SpeedtestDirection::Both);
        operation.speedtest_topology = Some(super::super::protocol::SpeedtestTopology::Unshaped);
        let worker_run_id = "6".repeat(32);
        let permit = AutotuneRuntimePermit {
            kind: RuntimePermitKind::SpeedtestUnshaped,
            permit_id: "7".repeat(32),
            job_id: operation.identity.job_id.clone(),
            worker_run_id: worker_run_id.clone(),
            boot_id: read_kernel_uuid(Path::new(DEFAULT_BOOT_ID_PATH), "boot ID").unwrap(),
            coordinator_generation: "8".repeat(32),
            worker: ProcessIdentity::inspect(Path::new(DEFAULT_PROC_ROOT), std::process::id())
                .unwrap(),
            instance_name: operation.identity.instance.clone(),
            target_interface: operation.identity.target_interface.clone(),
            route_identity: "main||pppoe-wan|192.0.2.1||254".to_string(),
            route_fingerprint: operation.identity.route_fingerprint.clone(),
            sqm_fingerprint: operation.identity.sqm_fingerprint.clone(),
            deadline_boot_ms: monotonic_boot_ms().unwrap() + 60_000,
            maximum_sequence: 1,
            profile: AutotuneProfile::BestOverall,
            link_kind: LinkKind::Unknown,
            baseline: RuntimeBaseline::Managed(MeasurementTopology::ShapedBoth),
            initial_download_kbps: 100_000,
            initial_upload_kbps: 50_000,
            download_qdisc_kind: RuntimeQdiscKind::Cake,
            upload_qdisc_kind: RuntimeQdiscKind::Cake,
            allow_bypass_download: true,
            allow_bypass_upload: true,
            download_bounds: RuntimeRateBounds {
                minimum_kbps: 100_000,
                maximum_kbps: 100_000,
            },
            upload_bounds: RuntimeRateBounds {
                minimum_kbps: 50_000,
                maximum_kbps: 50_000,
            },
        };
        store.publish_permit(&permit).unwrap();
        assert_eq!(
            read_unshaped_runtime_permit(&store, &operation, &worker_run_id).unwrap(),
            permit
        );
        store.clear().unwrap();
        assert!(
            read_unshaped_runtime_permit(&store, &operation, &worker_run_id)
                .unwrap_err()
                .contains("missing after worker admission")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn traffic_stop_reserve_is_rate_aware_and_saturating() {
        assert_eq!(
            traffic_stop_safety_reserve_bytes(85_000, 10_000),
            15_892_326
        );
        assert_eq!(
            minimum_full_autotune_traffic_budget_bytes(85_000, 10_000),
            16_023_398
        );
        assert_eq!(traffic_stop_safety_reserve_bytes(0, 0), 1024 * 1024);
        assert_eq!(
            traffic_stop_safety_reserve_bytes(u64::MAX, u64::MAX),
            u64::MAX
        );
        assert_eq!(
            minimum_attempt_budget(SpeedtestDirection::Download, u64::MAX),
            u64::MAX
        );
    }

    #[test]
    fn utility_target_allowlist_is_absolute_and_component_bounded() {
        assert!(utility_target_path_is_trusted(Path::new(
            "/usr/libexec/tc-tiny"
        )));
        assert!(utility_target_path_is_trusted(Path::new("/sbin/tc")));
        assert!(!utility_target_path_is_trusted(Path::new("/usr/libexec")));
        assert!(!utility_target_path_is_trusted(Path::new(
            "/usr/libexec-untrusted/tc"
        )));
        assert!(!utility_target_path_is_trusted(Path::new("/tmp/tc")));
        assert!(!utility_target_path_is_trusted(Path::new("usr/bin/tc")));
    }

    #[test]
    fn speedtest_arguments_are_direct_and_directional() {
        assert_eq!(
            speedtest_go_arguments(
                &request(SpeedtestDirection::Download),
                SpeedtestDirection::Download,
                Some(17372),
            )
            .unwrap(),
            vec![
                "--json",
                "--unix",
                "--ping-mode",
                "http",
                "--server",
                "17372",
                "--no-upload",
                "--source",
                "192.0.2.1",
                "--dns-bind-source",
            ]
        );
    }

    #[test]
    fn automatic_server_candidates_are_sorted_bounded_and_shell_free() {
        let output = concat!(
            "Found 4 Public Servers\n",
            "[ 35793] 1.12km 3ms Tallinn by Tele2 Eesti\n",
            "[13397] 7864.13km Timeout Salo by Lounea\n",
            "[17372] 1.20km 2.5ms Tallinn by Telia Eesti AS\n",
            "[29062] 1.10km 4ms Tallinn by STV AS\n",
        );
        assert_eq!(
            speedtest_go_server_candidates(output).unwrap(),
            vec![17372, 35793, 29062]
        );
        assert!(speedtest_go_server_candidates("[oops] 1km 2ms invalid").is_err());
        assert!(speedtest_go_server_candidates("[13397] 1km unavailable").is_err());
    }

    #[test]
    fn automatic_server_validation_rejects_the_known_duplicated_upload_signature() {
        let impossible = SpeedtestResult {
            direction: SpeedtestDirection::Both,
            download_kbps: Some(900_668),
            upload_kbps: Some(2_202_981),
            rx_bytes: 1_200_000_000,
            tx_bytes: 388_653_768,
            elapsed_ms: 23_350,
            server_id: Some(35793),
            server_name: "Tallinn".to_string(),
            server_sponsor: "Tele2 Eesti".to_string(),
        };
        let impossible_sample = SpeedtestLoadSample {
            direction: SpeedtestDirection::Both,
            aggregate_rx_bytes: 1_200_000_000,
            aggregate_tx_bytes: 388_653_768,
            confidence_rx_bytes: 1_200_000_000,
            confidence_tx_bytes: 388_653_768,
            controlled_rx_wire_bytes: 1_141_930_000,
            controlled_tx_wire_bytes: 2_278_610_000,
            controlled_rx_payload_bytes: 1_141_930_000,
            controlled_tx_payload_bytes: 2_278_610_000,
            counter_elapsed_ms: 20_100,
            download_elapsed_ms: Some(10_050),
            upload_elapsed_ms: Some(10_050),
        };
        assert!(!speedtest_qualification_is_plausible(
            &impossible,
            &impossible_sample
        ));

        let mut valid = impossible;
        valid.upload_kbps = Some(913_033);
        let mut valid_sample = impossible_sample;
        valid_sample.aggregate_tx_bytes = 1_150_000_000;
        valid_sample.confidence_tx_bytes = 1_150_000_000;
        valid_sample.controlled_tx_wire_bytes = 1_100_000_000;
        valid_sample.controlled_tx_payload_bytes = 1_100_000_000;
        assert!(speedtest_qualification_is_plausible(&valid, &valid_sample));
    }

    #[test]
    fn automatic_server_validation_accepts_consistent_reverse_asymmetry() {
        let result = SpeedtestResult {
            direction: SpeedtestDirection::Both,
            download_kbps: Some(1_000),
            upload_kbps: Some(3_000),
            rx_bytes: 0,
            tx_bytes: 0,
            elapsed_ms: 0,
            server_id: Some(17_372),
            server_name: "Tallinn".to_string(),
            server_sponsor: "Example".to_string(),
        };
        let sample = SpeedtestLoadSample {
            direction: SpeedtestDirection::Both,
            aggregate_rx_bytes: 1_400_000,
            aggregate_tx_bytes: 3_900_000,
            confidence_rx_bytes: 1_350_000,
            confidence_tx_bytes: 3_850_000,
            controlled_rx_wire_bytes: 1_300_000,
            controlled_tx_wire_bytes: 3_800_000,
            controlled_rx_payload_bytes: 1_250_000,
            controlled_tx_payload_bytes: 3_750_000,
            counter_elapsed_ms: 20_000,
            download_elapsed_ms: Some(10_000),
            upload_elapsed_ms: Some(10_000),
        };
        assert!(speedtest_qualification_is_plausible(&result, &sample));
    }

    #[test]
    fn automatic_server_rejection_is_typed_and_private_diagnostic_is_bounded() {
        let result = SpeedtestResult {
            direction: SpeedtestDirection::Both,
            download_kbps: Some(1_000),
            upload_kbps: Some(1_000),
            rx_bytes: 0,
            tx_bytes: 0,
            elapsed_ms: 0,
            server_id: Some(17_372),
            server_name: "must-not-be-logged-name".to_string(),
            server_sponsor: "must-not-be-logged-sponsor".to_string(),
        };
        let sample = SpeedtestLoadSample {
            direction: SpeedtestDirection::Both,
            aggregate_rx_bytes: 1_400_000,
            aggregate_tx_bytes: 1_400_000,
            confidence_rx_bytes: 1_350_000,
            confidence_tx_bytes: 1_350_000,
            controlled_rx_wire_bytes: 1_300_000,
            controlled_tx_wire_bytes: 1_300_000,
            controlled_rx_payload_bytes: 1_250_000,
            controlled_tx_payload_bytes: 1_250_000,
            counter_elapsed_ms: 20_000,
            download_elapsed_ms: Some(10_000),
            upload_elapsed_ms: Some(10_000),
        };
        assert_eq!(speedtest_qualification_rejection(&result, &sample), None);

        let mut changed_result = result.clone();
        changed_result.direction = SpeedtestDirection::Download;
        assert_eq!(
            speedtest_qualification_rejection(&changed_result, &sample),
            Some(SpeedtestQualificationRejection::Direction)
        );
        changed_result = result.clone();
        changed_result.download_kbps = None;
        assert_eq!(
            speedtest_qualification_rejection(&changed_result, &sample),
            Some(SpeedtestQualificationRejection::RateMissing)
        );

        let mut changed_sample = sample.clone();
        changed_sample.download_elapsed_ms = None;
        assert_eq!(
            speedtest_qualification_rejection(&result, &changed_sample),
            Some(SpeedtestQualificationRejection::DurationMissing)
        );
        changed_sample = sample.clone();
        changed_sample.controlled_rx_payload_bytes = 0;
        assert_eq!(
            speedtest_qualification_rejection(&result, &changed_sample),
            Some(SpeedtestQualificationRejection::DownloadPayloadProof)
        );
        changed_sample = sample.clone();
        changed_sample.controlled_tx_payload_bytes = 0;
        assert_eq!(
            speedtest_qualification_rejection(&result, &changed_sample),
            Some(SpeedtestQualificationRejection::UploadPayloadProof)
        );
        changed_sample = sample.clone();
        changed_sample.controlled_rx_wire_bytes = 0;
        assert_eq!(
            speedtest_qualification_rejection(&result, &changed_sample),
            Some(SpeedtestQualificationRejection::DownloadWireProof)
        );
        changed_sample = sample.clone();
        changed_sample.controlled_tx_wire_bytes = 0;
        assert_eq!(
            speedtest_qualification_rejection(&result, &changed_sample),
            Some(SpeedtestQualificationRejection::UploadWireProof)
        );
        changed_sample = sample.clone();
        changed_sample.aggregate_rx_bytes = changed_sample.confidence_rx_bytes - 1;
        assert_eq!(
            speedtest_qualification_rejection(&result, &changed_sample),
            None
        );
        changed_sample.aggregate_rx_bytes = changed_sample.controlled_rx_payload_bytes - 1;
        assert_eq!(
            speedtest_qualification_rejection(&result, &changed_sample),
            Some(SpeedtestQualificationRejection::AggregateRxBelowPayload)
        );
        changed_sample = sample.clone();
        changed_sample.confidence_rx_bytes = changed_sample.controlled_rx_wire_bytes - 1;
        assert_eq!(
            speedtest_qualification_rejection(&result, &changed_sample),
            Some(SpeedtestQualificationRejection::ConfidenceRxBelowWire)
        );
        changed_sample = sample.clone();
        changed_sample.controlled_rx_wire_bytes = changed_sample.controlled_rx_payload_bytes - 1;
        assert_eq!(
            speedtest_qualification_rejection(&result, &changed_sample),
            Some(SpeedtestQualificationRejection::RxWireBelowPayload)
        );
        changed_sample = sample.clone();
        changed_sample.confidence_tx_bytes = changed_sample.controlled_tx_wire_bytes / 2;
        assert_eq!(
            speedtest_qualification_rejection(&result, &changed_sample),
            Some(SpeedtestQualificationRejection::ConfidenceTxWireRatio)
        );
        changed_sample = sample.clone();
        changed_sample.controlled_tx_payload_bytes =
            changed_sample.controlled_tx_wire_bytes.saturating_mul(2);
        assert_eq!(
            speedtest_qualification_rejection(&result, &changed_sample),
            Some(SpeedtestQualificationRejection::TxWirePayloadRatio)
        );
        changed_result = result.clone();
        changed_result.download_kbps = Some(10_000);
        assert_eq!(
            speedtest_qualification_rejection(&changed_result, &sample),
            Some(SpeedtestQualificationRejection::DownloadRatePayloadTiming)
        );
        changed_result = result.clone();
        changed_result.upload_kbps = Some(10_000);
        assert_eq!(
            speedtest_qualification_rejection(&changed_result, &sample),
            Some(SpeedtestQualificationRejection::UploadRatePayloadTiming)
        );

        let diagnostic = qualification_rejection_log_line(
            3,
            &result,
            &sample,
            SpeedtestQualificationRejection::RxWireBelowPayload,
        );
        assert!(diagnostic.len() <= MAX_QUALIFICATION_DIAGNOSTIC_BYTES);
        assert!(diagnostic.contains("attempt=3"));
        assert!(diagnostic.contains("server_id=17372"));
        assert!(diagnostic.contains("reason=rx-wire-below-payload"));
        assert!(diagnostic.contains("controlled_rx_payload=1250000"));
        assert!(!diagnostic.contains(&result.server_name));
        assert!(!diagnostic.contains(&result.server_sponsor));
    }

    #[test]
    fn retryable_server_failures_have_bounded_terminal_reason_classes() {
        assert_eq!(
            retryable_server_rejection_code("speedtest-backend-failed"),
            "backend"
        );
        assert_eq!(
            retryable_server_rejection_code("speedtest-server-identity-mismatch"),
            "server-identity"
        );
        assert_eq!(
            retryable_server_rejection_code("speedtest-json-invalid"),
            "json"
        );
    }

    #[test]
    fn completed_output_unmeasurable_classification_is_parse_only() {
        for error in [
            "speedtest-json-missing",
            "speedtest-json-number-invalid",
            "speedtest-download-missing",
            "speedtest-upload-missing",
            "speedtest-direction-result-missing",
            "speedtest-duration-missing",
            "speedtest-download-evidence-missing",
            "speedtest-upload-evidence-missing",
            "speedtest-direction-evidence-missing",
            "speedtest-used-bytes-invalid",
            "speedtest-used-bytes-duplicate",
            "speedtest-duration-invalid",
            "speedtest-rate-invalid",
            "speedtest-direction-result-unavailable",
        ] {
            assert!(
                completed_speedtest_output_is_unmeasurable(error),
                "expected completed-output rejection for {error}"
            );
        }
        for error in [
            "speedtest-backend-failed",
            "speedtest-timeout",
            "speedtest-route-drift",
            "speedtest-server-identity-mismatch",
            "speedtest-output-not-utf8",
            "speedtest-output-oversized",
            "speedtest-download-accounting-qdisc-state-mismatch",
            SPEEDTEST_TRAFFIC_BUDGET_EXHAUSTED,
            SPEEDTEST_TRAFFIC_LIMIT_REACHED,
        ] {
            assert!(
                !completed_speedtest_output_is_unmeasurable(error),
                "unsafe retry classification for {error}"
            );
        }
    }

    #[test]
    fn backend_output_rejection_diagnostic_never_copies_backend_text() {
        let output = "PRIVATE-MARKER\n  {malformed PRIVATE-JSON}\n";
        let line = backend_output_rejection_log_line("speedtest-json-number-invalid", output, 37);
        assert_eq!(
            line,
            "speedtest-backend-output-rejected code=speedtest-json-number-invalid stdout_bytes=42 stdout_lines=2 json_candidates=1 stderr_bytes=37"
        );
        assert!(!line.contains("PRIVATE-MARKER"));
        assert!(!line.contains("PRIVATE-JSON"));

        let invalid_code = backend_output_rejection_log_line("unsafe value", output, 0);
        assert!(invalid_code.contains("code=invalid"));
        assert!(!invalid_code.contains("unsafe value"));
    }

    #[test]
    fn shaped_upload_qualification_uses_a_bounded_counter_ratio() {
        assert!(counter_ratio_is_at_least(92, 100, 80));
        assert!(counter_ratio_is_at_least(80, 100, 80));
        assert!(!counter_ratio_is_at_least(79, 100, 80));
        assert!(!counter_ratio_is_at_least(0, 100, 80));
        assert!(!counter_ratio_is_at_least(u64::MAX, 1, 101));
    }

    #[test]
    fn qualification_accepts_isolated_shaped_upload_boundary_lead() {
        let result = SpeedtestResult {
            direction: SpeedtestDirection::Both,
            download_kbps: Some(720),
            upload_kbps: Some(976),
            rx_bytes: 1_200_000,
            tx_bytes: 1_717_093,
            elapsed_ms: 25_000,
            server_id: Some(17_372),
            server_name: "test".to_string(),
            server_sponsor: "test".to_string(),
        };
        let sample = SpeedtestLoadSample {
            direction: SpeedtestDirection::Both,
            aggregate_rx_bytes: 1_200_000,
            aggregate_tx_bytes: 1_717_093,
            confidence_rx_bytes: 1_100_000,
            confidence_tx_bytes: 1_716_219,
            controlled_rx_wire_bytes: 1_000_000,
            controlled_tx_wire_bytes: 1_612_390,
            controlled_rx_payload_bytes: 900_000,
            controlled_tx_payload_bytes: 1_830_000,
            counter_elapsed_ms: 25_000,
            download_elapsed_ms: Some(10_000),
            upload_elapsed_ms: Some(15_000),
        };
        assert!(!upload_counter_lag_is_bounded(
            sample.controlled_tx_wire_bytes,
            sample.controlled_tx_payload_bytes
        ));
        assert!(speedtest_qualification_is_plausible(&result, &sample));
        assert_eq!(
            bounded_achieved_kbps(
                result.upload_kbps.unwrap(),
                sample.controlled_tx_payload_bytes,
                sample.controlled_tx_wire_bytes,
                sample.upload_elapsed_ms.unwrap(),
            )
            .unwrap(),
            859
        );

        let mut duplicated = sample;
        duplicated.controlled_tx_payload_bytes =
            duplicated.controlled_tx_wire_bytes.saturating_mul(2);
        let mut duplicated_result = result;
        duplicated_result.upload_kbps = Some(
            duplicated.controlled_tx_payload_bytes.saturating_mul(8)
                / duplicated.upload_elapsed_ms.unwrap(),
        );
        assert!(!speedtest_qualification_is_plausible(
            &duplicated_result,
            &duplicated
        ));
    }

    #[test]
    fn automatic_server_validation_rejects_rate_timing_mismatch_and_missing_duration() {
        let result = SpeedtestResult {
            direction: SpeedtestDirection::Both,
            download_kbps: Some(1_000),
            upload_kbps: Some(1_000),
            rx_bytes: 0,
            tx_bytes: 0,
            elapsed_ms: 0,
            server_id: Some(17_372),
            server_name: "Tallinn".to_string(),
            server_sponsor: "Example".to_string(),
        };
        let mut sample = SpeedtestLoadSample {
            direction: SpeedtestDirection::Both,
            aggregate_rx_bytes: 1_400_000,
            aggregate_tx_bytes: 1_400_000,
            confidence_rx_bytes: 1_350_000,
            confidence_tx_bytes: 1_350_000,
            controlled_rx_wire_bytes: 1_300_000,
            controlled_tx_wire_bytes: 1_300_000,
            controlled_rx_payload_bytes: 1_250_000,
            controlled_tx_payload_bytes: 1_250_000,
            counter_elapsed_ms: 20_000,
            download_elapsed_ms: Some(10_000),
            upload_elapsed_ms: Some(10_000),
        };
        assert!(speedtest_qualification_is_plausible(&result, &sample));

        let mut mismatched = result.clone();
        mismatched.upload_kbps = Some(10_000);
        assert!(!speedtest_qualification_is_plausible(&mismatched, &sample));
        sample.download_elapsed_ms = None;
        assert!(!speedtest_qualification_is_plausible(&result, &sample));
        sample.download_elapsed_ms = Some(0);
        assert!(!speedtest_qualification_is_plausible(&result, &sample));
    }

    #[test]
    fn rate_payload_timing_tolerance_is_bounded_and_overflow_fails_closed() {
        assert!(rate_matches_payload_timing(1_350, 1_250_000, 10_000));
        assert!(!rate_matches_payload_timing(1_351, 1_250_000, 10_000));
        assert!(rate_matches_payload_timing(1_000, 1_687_500, 10_000));
        assert!(!rate_matches_payload_timing(999, 1_687_500, 10_000));
        assert!(!rate_matches_payload_timing(u64::MAX, u64::MAX, u64::MAX));
    }

    #[test]
    fn backend_rate_is_capped_by_independent_controlled_wire_evidence() {
        // Reproduced r131 shaped-upload boundary: speedtest-go counted bytes
        // read from its request body which had not all reached the UID-scoped
        // nft counter when the shaped transfer stopped.
        assert_eq!(
            bounded_achieved_kbps(976, 1_830_000, 1_612_390, 15_000).unwrap(),
            859
        );

        // A backend that doubles both its rate and Used value remains
        // internally self-consistent, but cannot inflate the rate admitted to
        // native Auto-Tune beyond independently observed wire traffic.
        assert_eq!(
            bounded_achieved_kbps(1_600, 3_000_000, 1_500_000, 15_000).unwrap(),
            800
        );
        assert!(bounded_achieved_kbps(1_600, 1_000_000, 1_500_000, 15_000).is_err());
        assert!(bounded_achieved_kbps(1, 1, 1, u64::MAX).is_err());
    }

    #[test]
    fn root_cake_sent_counter_is_exact_and_handle_bound() {
        let output = concat!(
            "qdisc cake 801e: root refcnt 2 bandwidth 10Mbit besteffort\n",
            " Sent 16969492 bytes 11779 pkt (dropped 3947, overlimits 26537 requeues 0)\n",
            " backlog 0b 0p requeues 0\n",
        );
        assert_eq!(
            parse_exact_root_cake_sent_bytes(
                output,
                CakeCounterKind::Cake,
                Some("801e:"),
                CakeCounterDirection::Download,
            )
            .unwrap(),
            ("801e:".to_string(), 16_969_492)
        );
        assert_eq!(
            parse_exact_root_cake_sent_bytes(
                output,
                CakeCounterKind::Cake,
                Some("801f:"),
                CakeCounterDirection::Download,
            )
            .unwrap_err(),
            "speedtest-download-accounting-qdisc-identity-mismatch"
        );
        assert_eq!(
            parse_exact_root_cake_sent_bytes(
                output,
                CakeCounterKind::CakeMq,
                None,
                CakeCounterDirection::Upload,
            )
            .unwrap_err(),
            "speedtest-upload-accounting-qdisc-identity-mismatch"
        );
        let duplicate = format!("{output}{output}");
        assert_eq!(
            parse_exact_root_cake_sent_bytes(
                &duplicate,
                CakeCounterKind::Cake,
                Some("801e:"),
                CakeCounterDirection::Upload,
            )
            .unwrap_err(),
            "speedtest-upload-accounting-qdisc-duplicate"
        );
        let before = CakeCounterSnapshot {
            device: "ifb4eth1".to_string(),
            kind: "cake".to_string(),
            handle: "801e:".to_string(),
            sent_bytes: 10_000,
        };
        let mut after = before.clone();
        after.sent_bytes = 20_000;
        assert_eq!(
            cake_counter_delta(&before, &after, CakeCounterDirection::Download).unwrap(),
            10_000
        );
        after.handle = "801f:".to_string();
        assert_eq!(
            cake_counter_delta(&before, &after, CakeCounterDirection::Download).unwrap_err(),
            "speedtest-download-accounting-qdisc-changed"
        );
        assert_eq!(
            cake_counter_delta(&before, &after, CakeCounterDirection::Upload).unwrap_err(),
            "speedtest-upload-accounting-qdisc-changed"
        );
        let mut reset = before.clone();
        reset.sent_bytes = 9_999;
        assert_eq!(
            cake_counter_delta(&before, &reset, CakeCounterDirection::Download).unwrap_err(),
            "speedtest-download-accounting-qdisc-counter-reset"
        );
        assert_eq!(
            cake_counter_delta(&before, &reset, CakeCounterDirection::Upload).unwrap_err(),
            "speedtest-upload-accounting-qdisc-counter-reset"
        );
        assert_eq!(
            parse_exact_root_cake_sent_bytes(
                "qdisc fq_codel 0: root refcnt 2 limit 10240p\n Sent 100 bytes 1 pkt\n",
                CakeCounterKind::Cake,
                None,
                CakeCounterDirection::Upload,
            )
            .unwrap_err(),
            "speedtest-upload-accounting-qdisc-missing"
        );
    }

    #[test]
    fn backend_output_is_unlinked_immediately_and_same_path_is_reusable() {
        let scratch = std::env::temp_dir().join(format!(
            "cake-speedtest-output-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let output_path = scratch.with_extension("backend-output");
        let stderr_path = scratch.with_extension("backend-stderr");

        for payload in [b"first".as_slice(), b"second".as_slice()] {
            let (mut guard, mut stdout, stderr) = BackendOutputGuard::create(&scratch).unwrap();
            assert!(!output_path.exists());
            assert!(!stderr_path.exists());
            stdout.write_all(payload).unwrap();
            stdout.flush().unwrap();
            drop(stdout);
            drop(stderr);
            assert_eq!(
                read_bounded_file(&mut guard.output, 64).unwrap().as_bytes(),
                payload
            );
            drop(guard);
            assert!(!output_path.exists());
            assert!(!stderr_path.exists());
        }
    }

    #[test]
    fn live_backend_output_stays_anonymous_and_survives_child_kill_for_bounded_read() {
        let scratch = std::env::temp_dir().join(format!(
            "cake-speedtest-live-output-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let output_path = scratch.with_extension("backend-output");
        let stderr_path = scratch.with_extension("backend-stderr");
        let (mut guard, stdout, stderr) = BackendOutputGuard::create(&scratch).unwrap();
        let mut child = Command::new("sh")
            .args(["-c", "printf partial-output; sleep 30"])
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .unwrap();

        let paths_were_absent_while_live = !output_path.exists() && !stderr_path.exists();
        for _ in 0..100 {
            if guard.output.metadata().unwrap().len() > 0 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let partial_output_was_written = guard.output.metadata().unwrap().len() > 0;
        child.kill().unwrap();
        child.wait().unwrap();

        assert!(paths_were_absent_while_live);
        assert!(partial_output_was_written);
        assert_eq!(
            read_bounded_file(&mut guard.output, 64).unwrap(),
            "partial-output"
        );
        assert!(!output_path.exists());
        assert!(!stderr_path.exists());
    }

    #[test]
    fn parser_uses_bytes_per_second_once() {
        let output = r#"Download: 900 Mbps (Used: 1133.87MB)
Upload: 766.6 Mbps (Used: 975.34MB)
{"servers":[{"id":"17372","name":"Tallinn","sponsor":"Telia Eesti AS","dl_speed":112500000.0,"ul_speed":95825000.0,"test_duration":{"download":10050243168,"upload":10051665912}}]}"#;
        let result = parse_speedtest_go(output, SpeedtestDirection::Both).unwrap();
        assert_eq!(result.download_kbps, Some(900_000));
        assert_eq!(result.upload_kbps, Some(766_600));
        assert_eq!(result.server_id, Some(17372));
        assert_eq!(result.server_sponsor, "Telia Eesti AS");
        let parsed = parse_speedtest_go_measurement(output, SpeedtestDirection::Both).unwrap();
        assert_eq!(parsed.controlled_rx_payload_bytes, 1_133_870_000);
        assert_eq!(parsed.controlled_tx_payload_bytes, 975_340_000);
        assert_eq!(parsed.download_elapsed_ms, Some(10_051));
        assert_eq!(parsed.upload_elapsed_ms, Some(10_052));
    }

    #[test]
    fn parser_accepts_zero_for_the_direction_explicitly_skipped_by_backend() {
        let download = parse_speedtest_go(
            "Download: 900 Mbps (Used: 1133.87MB)\n{\"servers\":[{\"id\":\"17372\",\"name\":\"Tallinn\",\"sponsor\":\"Example\",\"dl_speed\":112500000.0,\"ul_speed\":0,\"test_duration\":{\"download\":10050243168,\"upload\":null}}]}",
            SpeedtestDirection::Download,
        )
        .unwrap();
        assert_eq!(download.download_kbps, Some(900_000));
        assert_eq!(download.upload_kbps, None);

        let upload = parse_speedtest_go(
            "Upload: 200 Mbps (Used: 251.25MB)\n{\"servers\":[{\"id\":\"17372\",\"name\":\"Tallinn\",\"sponsor\":\"Example\",\"dl_speed\":0,\"ul_speed\":25000000.0,\"test_duration\":{\"download\":null,\"upload\":10050000000}}]}",
            SpeedtestDirection::Upload,
        )
        .unwrap();
        assert_eq!(upload.download_kbps, None);
        assert_eq!(upload.upload_kbps, Some(200_000));
    }

    #[test]
    fn parser_types_a_selected_direction_below_rate_resolution() {
        let error = parse_speedtest_go(
            "{\"servers\":[{\"id\":17372,\"name\":\"Example\",\"sponsor\":\"Example\",\"dl_speed\":0.3001399059302496,\"ul_speed\":0,\"test_duration\":{\"download\":10000000000,\"upload\":null}}]}",
            SpeedtestDirection::Download,
        )
        .unwrap_err();
        assert_eq!(error, "speedtest-direction-result-unavailable");
    }

    #[test]
    fn json_integer_accepts_only_null_or_a_complete_integer_token() {
        assert_eq!(
            json_u64("{\"value\":null,\"next\":1}", "value").unwrap(),
            None
        );
        assert_eq!(
            json_u64("{\"value\":\"42\",\"next\":1}", "value").unwrap(),
            Some(42)
        );
        assert_eq!(
            json_u64("{\"value\":42,\"next\":1}", "value").unwrap(),
            Some(42)
        );
        assert!(json_u64("{\"value\":nullified}", "value").is_err());
        assert!(json_u64("{\"value\":42oops}", "value").is_err());
        assert!(json_u64("{\"value\":\"42oops\"}", "value").is_err());
    }

    #[test]
    fn parser_fails_closed_without_backend_owned_byte_evidence() {
        let output = r#"{"servers":[{"id":"17372","name":"Tallinn","sponsor":"Example","dl_speed":112500000.0,"ul_speed":0,"test_duration":{"download":10050243168}}]}"#;
        assert_eq!(
            parse_speedtest_go(output, SpeedtestDirection::Download).unwrap_err(),
            "speedtest-download-evidence-missing"
        );
        assert!(used_payload_bytes(
            "Download: 900 Mbps (Used: 1.00MB)\nDownload: 901 Mbps (Used: 1.01MB)",
            "Download"
        )
        .is_err());
    }

    #[test]
    fn budget_and_counter_reset_fail_closed() {
        assert_eq!(counter_deltas((100, 200), (150, 260)).unwrap(), (50, 60));
        assert!(counter_deltas((100, 200), (99, 260)).is_err());
        assert!(prove_route_traffic(SpeedtestDirection::Both, (100, 100)).is_err());
    }

    #[test]
    fn package_user_is_unique_and_unprivileged() {
        let credentials = parse_backend_credentials(
            "root:x:0:0:root:/root:/bin/ash\ncake-speedtest:x:32769:32770::/tmp:/bin/false\n",
        )
        .unwrap();
        assert_eq!(
            credentials,
            BackendCredentials {
                uid: 32769,
                gid: 32770
            }
        );
        assert!(parse_backend_credentials("cake-speedtest:x:0:0::/tmp:/bin/false\n").is_err());
        assert!(parse_backend_credentials(
            "cake-speedtest:x:1:1::/tmp:/bin/false\ncake-speedtest:x:2:2::/tmp:/bin/false\n"
        )
        .is_err());
    }

    #[test]
    fn embedded_session_identity_allows_only_bounded_runs_from_one_phase() {
        let mut operation = request(SpeedtestDirection::Download);
        operation.identity.operation = OperationKind::FullAutotune;
        operation.speedtest_direction = None;
        operation.speedtest_server_id = None;
        let worker_run_id = "6".repeat(32);
        let session = EmbeddedSpeedtestSession {
            route_pin: NftRoutePin {
                table: None,
                owner: None,
            },
            credentials: BackendCredentials {
                uid: 32769,
                gid: 32770,
            },
            job_id: operation.identity.job_id.clone(),
            worker_run_id: worker_run_id.clone(),
            route_fingerprint: operation.identity.route_fingerprint.clone(),
            route: operation.route.clone(),
            backend: operation.backend.clone(),
            selected_server_id: None,
        };
        assert!(session.matches(&operation, &worker_run_id));

        let mut next_bounded_run = operation.clone();
        next_bounded_run.traffic_budget_bytes /= 2;
        assert!(session.matches(&next_bounded_run, &worker_run_id));

        let mut changed_route = operation.clone();
        changed_route.identity.route_fingerprint = "7".repeat(64);
        assert!(!session.matches(&changed_route, &worker_run_id));
        changed_route = operation.clone();
        changed_route.route.l3_device = "eth0".to_string();
        assert!(!session.matches(&changed_route, &worker_run_id));
        assert!(!session.matches(&operation, &"8".repeat(32)));
    }

    #[test]
    fn mwan3_environment_and_route_pin_are_strict_and_identity_owned() {
        let environment = "Running exec\nDEVICE=eth0\nSRCIP=192.0.2.1\nFWMARK=0x3f00\n";
        assert_eq!(
            unique_environment_value(environment, "FWMARK").unwrap(),
            Some("0x3f00")
        );
        assert_eq!(parse_hex_u32("0x3f00", "invalid").unwrap(), 0x3f00);
        assert!(unique_environment_value("FWMARK=0x100\nFWMARK=0x200\n", "FWMARK").is_err());
        assert!(parse_hex_u32("3f00", "invalid").is_err());

        let job_id = "a".repeat(32);
        let run_id = "b".repeat(32);
        let table = route_pin_table_name(&job_id, &run_id).unwrap();
        let owner = route_pin_owner(&job_id, &run_id).unwrap();
        assert_eq!(table, "cake_st_aaaaaaaaaaaa_bbbbbbbbbbbb");
        let batch = nft_route_pin_batch(&table, &owner, 32769, Some((!0x3f00, 0x200)));
        assert!(batch.starts_with("{\"nftables\":["));
        assert!(batch.contains("\"type\":\"route\",\"hook\":\"output\",\"prio\":-148"));
        assert!(batch.contains("\"type\":\"filter\",\"hook\":\"input\",\"prio\":-148"));
        assert!(batch.contains("\"name\":\"rx\""));
        assert!(batch.contains("\"name\":\"tx\""));
        assert!(batch.contains("{\"counter\":\"rx\"}"));
        assert!(batch.contains("{\"counter\":\"tx\"}"));
        assert!(batch.contains("\"right\":32769"));
        assert!(batch.contains("4294951167"));
        assert!(batch.contains("]},512]"));
        assert!(batch.ends_with("{\"counter\":\"rx\"}]}}}]}\n"));
        assert_eq!(batch.matches(&owner).count(), 3);
        let accounting_only = nft_route_pin_batch(&table, &owner, 32769, None);
        assert!(!accounting_only.contains("\"mangle\""));
        assert!(accounting_only.contains("{\"counter\":\"rx\"}"));
        assert!(accounting_only.contains("{\"counter\":\"tx\"}"));
        assert!(route_pin_table_name("not-hex", &run_id).is_err());
    }

    #[test]
    fn named_speedtest_counters_are_identity_bound_and_exact() {
        let owner = "cake-autorate-speedtest:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let json = concat!(
            "{\"nftables\":[",
            "{\"counter\":{\"family\":\"inet\",\"name\":\"rx\",\"table\":\"cake_st_aaaaaaaaaaaa_bbbbbbbbbbbb\",\"comment\":\"__OWNER__\",\"packets\":7,\"bytes\":12345}},",
            "{\"counter\":{\"family\":\"inet\",\"name\":\"tx\",\"table\":\"cake_st_aaaaaaaaaaaa_bbbbbbbbbbbb\",\"comment\":\"__OWNER__\",\"packets\":8,\"bytes\":67890}}]}"
        )
        .replace("__OWNER__", owner);
        assert_eq!(
            named_counter_bytes(
                &json,
                "cake_st_aaaaaaaaaaaa_bbbbbbbbbbbb",
                ACCOUNTING_RX_COUNTER,
                owner
            )
            .unwrap(),
            Some(12_345)
        );
        assert_eq!(
            named_counter_bytes(
                &json,
                "cake_st_aaaaaaaaaaaa_bbbbbbbbbbbb",
                ACCOUNTING_TX_COUNTER,
                owner
            )
            .unwrap(),
            Some(67_890)
        );
        assert!(named_counter_bytes(&json, "cake_st_wrong", ACCOUNTING_RX_COUNTER, owner).is_err());
    }

    #[test]
    fn route_identity_must_match_structured_request() {
        let request = request(SpeedtestDirection::Both);
        let actual = RouteIdentity {
            mode: "main".to_string(),
            member: String::new(),
            device: "pppoe-wan".to_string(),
            source_ip: "192.0.2.1".to_string(),
            fwmark: String::new(),
            table: "main".to_string(),
        };
        assert!(route_matches_request(&request, &actual).is_ok());
        let mut drifted = actual;
        drifted.source_ip = "192.0.2.2".to_string();
        assert!(route_matches_request(&request, &drifted).is_err());
    }

    #[test]
    fn route_ready_wait_retries_only_transient_prelaunch_unavailability() {
        let attempts = Cell::new(0usize);
        let pauses = Cell::new(0usize);
        let expected = RouteSnapshot {
            identity: RouteIdentity {
                mode: "mwan3".to_string(),
                member: "wan".to_string(),
                device: "eth0".to_string(),
                source_ip: "192.0.2.1".to_string(),
                fwmark: "0x100".to_string(),
                table: "1".to_string(),
            },
            online: true,
            active: true,
            member_status: "online".to_string(),
            reason: String::new(),
        };
        let actual = wait_for_route_ready_with(
            || {
                attempts.set(attempts.get() + 1);
                if attempts.get() < 3 {
                    Err("speedtest-route-not-ready".to_string())
                } else {
                    Ok(expected.clone())
                }
            },
            || false,
            || Ok(false),
            || pauses.set(pauses.get() + 1),
        )
        .unwrap();
        assert_eq!(actual, expected);
        assert_eq!(attempts.get(), 3);
        assert_eq!(pauses.get(), 2);

        let attempts = Cell::new(0usize);
        let pauses = Cell::new(0usize);
        let error = wait_for_route_ready_with(
            || {
                attempts.set(attempts.get() + 1);
                Err("speedtest-route-not-ready".to_string())
            },
            || false,
            || Ok(attempts.get() >= 3),
            || pauses.set(pauses.get() + 1),
        )
        .unwrap_err();
        assert_eq!(error, "speedtest-deadline-expired");
        assert_eq!(attempts.get(), 3);
        assert_eq!(pauses.get(), 3);

        let attempts = Cell::new(0usize);
        let error = wait_for_route_ready_with(
            || {
                attempts.set(attempts.get() + 1);
                Err("speedtest-route-identity-mismatch".to_string())
            },
            || false,
            || Ok(false),
            || panic!("identity mismatch must not be retried"),
        )
        .unwrap_err();
        assert_eq!(error, "speedtest-route-identity-mismatch");
        assert_eq!(attempts.get(), 1);
    }

    #[test]
    fn route_ready_wait_honours_cancellation_and_operation_deadline() {
        let error = wait_for_route_ready_with(
            || panic!("cancelled wait must not inspect routing"),
            || true,
            || Ok(false),
            || panic!("cancelled wait must not sleep"),
        )
        .unwrap_err();
        assert_eq!(error, "speedtest-route-wait-cancelled");

        let error = wait_for_route_ready_with(
            || panic!("expired wait must not inspect routing"),
            || false,
            || Ok(true),
            || panic!("expired wait must not sleep"),
        )
        .unwrap_err();
        assert_eq!(error, "speedtest-deadline-expired");
    }

    #[test]
    fn route_ready_wait_has_no_private_retry_or_delay_deadline() {
        let attempts = Cell::new(0usize);
        let pauses = Cell::new(0usize);
        let expected = RouteSnapshot {
            identity: RouteIdentity {
                mode: "mwan3".to_string(),
                member: "wan".to_string(),
                device: "eth0".to_string(),
                source_ip: "192.0.2.1".to_string(),
                fwmark: "0x100".to_string(),
                table: "1".to_string(),
            },
            online: true,
            active: false,
            member_status: "online".to_string(),
            reason: "standby".to_string(),
        };
        let actual = wait_for_route_ready_with(
            || {
                attempts.set(attempts.get() + 1);
                if attempts.get() <= 64 {
                    Err("speedtest-route-not-ready".to_string())
                } else {
                    Ok(expected.clone())
                }
            },
            || false,
            || Ok(false),
            || pauses.set(pauses.get() + 1),
        )
        .unwrap();
        assert_eq!(actual, expected);
        assert_eq!(attempts.get(), 65);
        assert_eq!(pauses.get(), 64);
    }

    #[test]
    fn unmeasured_route_operation_restarts_without_a_private_retry_limit() {
        let attempts = Cell::new(0_u32);
        let waits = Cell::new(0_u32);
        let result = retry_unmeasured_route_operation(
            || {
                let attempt = attempts.get() + 1;
                attempts.set(attempt);
                if attempt <= 64 {
                    Err("speedtest-route-not-ready".to_string())
                } else {
                    Ok("server-list-ready")
                }
            },
            || {
                waits.set(waits.get() + 1);
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(result, "server-list-ready");
        assert_eq!(attempts.get(), 65);
        assert_eq!(waits.get(), 64);
    }

    #[test]
    fn unmeasured_route_operation_never_retries_identity_or_wait_errors() {
        let attempts = Cell::new(0_u32);
        let waits = Cell::new(0_u32);
        let identity_error = retry_unmeasured_route_operation::<(), _, _>(
            || {
                attempts.set(attempts.get() + 1);
                Err("speedtest-route-identity-mismatch".to_string())
            },
            || {
                waits.set(waits.get() + 1);
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(identity_error, "speedtest-route-identity-mismatch");
        assert_eq!(attempts.get(), 1);
        assert_eq!(waits.get(), 0);

        let wait_error = retry_unmeasured_route_operation::<(), _, _>(
            || Err("speedtest-route-not-ready".to_string()),
            || Err("speedtest-route-wait-cancelled".to_string()),
        )
        .unwrap_err();
        assert_eq!(wait_error, "speedtest-route-wait-cancelled");
    }

    #[test]
    fn route_loss_retries_same_measurement_and_debits_every_run_once() {
        let attempts = Cell::new(0_u32);
        let counters = Cell::new((1_000_u64, 2_000_u64));
        let debit_count = Cell::new(0_u32);
        let debited_rx = Cell::new(0_u64);
        let debited_tx = Cell::new(0_u64);
        let mut remaining = 10_000_u64;
        let result = retry_budgeted_route_measurement_on_loss_with_debit(
            &mut remaining,
            1,
            || Ok(counters.get()),
            |budget| {
                let attempt = attempts.get() + 1;
                attempts.set(attempt);
                let current = counters.get();
                counters.set((current.0 + 10, current.1 + 20));
                if attempt <= 64 {
                    Err("speedtest-route-not-ready".to_string())
                } else {
                    Ok(budget)
                }
            },
            &mut |debit| {
                debit_count.set(debit_count.get() + 1);
                debited_rx.set(debited_rx.get() + debit.rx_bytes);
                debited_tx.set(debited_tx.get() + debit.tx_bytes);
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(attempts.get(), 65);
        assert_eq!(result, 8_080);
        assert_eq!(remaining, 8_050);
        assert_eq!(debit_count.get(), 65);
        assert_eq!(debited_rx.get(), 650);
        assert_eq!(debited_tx.get(), 1_300);
    }

    #[test]
    fn unproved_traffic_retries_the_same_measurement_and_debits_every_attempt() {
        let attempts = Cell::new(0_u32);
        let counters = Cell::new((1_000_u64, 2_000_u64));
        let debit_count = Cell::new(0_u32);
        let debited = Cell::new((0_u64, 0_u64));
        let mut remaining = 10_000_u64;
        let result = retry_budgeted_route_measurement_on_loss_with_debit(
            &mut remaining,
            1,
            || Ok(counters.get()),
            |budget| {
                let attempt = attempts.get() + 1;
                attempts.set(attempt);
                let current = counters.get();
                counters.set((current.0 + 100, current.1 + 200));
                if attempt == 1 {
                    Err("speedtest-route-traffic-unproved".to_string())
                } else {
                    Ok(budget)
                }
            },
            &mut |debit| {
                debit_count.set(debit_count.get() + 1);
                let total = debited.get();
                debited.set((total.0 + debit.rx_bytes, total.1 + debit.tx_bytes));
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(attempts.get(), 2);
        assert_eq!(result, 9_700);
        assert_eq!(remaining, 9_400);
        assert_eq!(debit_count.get(), 2);
        assert_eq!(debited.get(), (200, 400));
    }

    #[test]
    fn persistently_unproved_traffic_stops_after_three_charged_attempts() {
        let attempts = Cell::new(0_u32);
        let counters = Cell::new((1_000_u64, 2_000_u64));
        let debit_count = Cell::new(0_u32);
        let mut remaining = 10_000_u64;
        let error = retry_budgeted_route_measurement_on_loss_with_debit::<(), _, _>(
            &mut remaining,
            1,
            || Ok(counters.get()),
            |_| {
                attempts.set(attempts.get() + 1);
                let current = counters.get();
                counters.set((current.0 + 100, current.1 + 200));
                Err("speedtest-route-traffic-unproved".to_string())
            },
            &mut |_| {
                debit_count.set(debit_count.get() + 1);
                Ok(())
            },
        )
        .unwrap_err();

        assert_eq!(error, "speedtest-route-traffic-unproved");
        assert_eq!(attempts.get(), u32::from(MAX_UNPROVED_TRAFFIC_ATTEMPTS));
        assert_eq!(remaining, 9_100);
        assert_eq!(debit_count.get(), u32::from(MAX_UNPROVED_TRAFFIC_ATTEMPTS));
    }

    #[test]
    fn readiness_loss_does_not_spend_the_unproved_traffic_retry_limit() {
        let attempts = Cell::new(0_u32);
        let counters = Cell::new((1_000_u64, 2_000_u64));
        let mut remaining = 10_000_u64;
        let error = retry_budgeted_route_measurement_on_loss::<(), _, _>(
            &mut remaining,
            1,
            || Ok(counters.get()),
            |_| {
                let attempt = attempts.get() + 1;
                attempts.set(attempt);
                let current = counters.get();
                counters.set((current.0 + 10, current.1 + 20));
                if attempt % 2 == 0 {
                    Err("speedtest-route-not-ready".to_string())
                } else {
                    Err("speedtest-route-traffic-unproved".to_string())
                }
            },
        )
        .unwrap_err();

        assert_eq!(error, "speedtest-route-traffic-unproved");
        assert_eq!(attempts.get(), 5);
        assert_eq!(remaining, 9_850);
    }

    #[test]
    fn insufficient_attempt_budget_stops_before_counters_or_transfer() {
        let counter_reads = Cell::new(0_u32);
        let attempts = Cell::new(0_u32);
        let debit_count = Cell::new(0_u32);
        let mut remaining = 999_u64;
        let error = retry_budgeted_route_measurement_on_loss_with_debit::<(), _, _>(
            &mut remaining,
            1_000,
            || {
                counter_reads.set(counter_reads.get() + 1);
                Ok((0, 0))
            },
            |_| {
                attempts.set(attempts.get() + 1);
                Ok(())
            },
            &mut |_| {
                debit_count.set(debit_count.get() + 1);
                Ok(())
            },
        )
        .unwrap_err();

        assert_eq!(error, SPEEDTEST_TRAFFIC_BUDGET_EXHAUSTED);
        assert_eq!(remaining, 999);
        assert_eq!(counter_reads.get(), 0);
        assert_eq!(attempts.get(), 0);
        assert_eq!(debit_count.get(), 0);
    }

    #[test]
    fn supervisor_limit_is_debited_exactly_before_it_is_returned() {
        let counters = Cell::new((10_000_u64, 20_000_u64));
        let debit_count = Cell::new(0_u32);
        let debited = Cell::new((0_u64, 0_u64));
        let mut remaining = 10_000_u64;
        let error = retry_budgeted_route_measurement_on_loss_with_debit::<(), _, _>(
            &mut remaining,
            1,
            || Ok(counters.get()),
            |_| {
                counters.set((11_500, 20_500));
                Err(SPEEDTEST_TRAFFIC_LIMIT_REACHED.to_string())
            },
            &mut |debit| {
                debit_count.set(debit_count.get() + 1);
                debited.set((debit.rx_bytes, debit.tx_bytes));
                Ok(())
            },
        )
        .unwrap_err();

        assert_eq!(error, SPEEDTEST_TRAFFIC_LIMIT_REACHED);
        assert_eq!(remaining, 8_000);
        assert_eq!(debit_count.get(), 1);
        assert_eq!(debited.get(), (1_500, 500));
    }

    #[test]
    fn counter_overrun_keeps_accounting_fail_closed_without_callback() {
        let counters = Cell::new((100_u64, 200_u64));
        let debit_count = Cell::new(0_u32);
        let mut remaining = 1_000_u64;
        let error = retry_budgeted_route_measurement_on_loss_with_debit::<(), _, _>(
            &mut remaining,
            1,
            || Ok(counters.get()),
            |_| {
                counters.set((900, 600));
                Err(SPEEDTEST_TRAFFIC_LIMIT_REACHED.to_string())
            },
            &mut |_| {
                debit_count.set(debit_count.get() + 1);
                Ok(())
            },
        )
        .unwrap_err();

        assert_eq!(error, "speedtest-traffic-budget-exceeded");
        assert_eq!(remaining, 1_000);
        assert_eq!(debit_count.get(), 0);
    }

    #[test]
    fn traffic_debit_persistence_failure_stops_before_any_retry() {
        let attempts = Cell::new(0_u32);
        let counters = Cell::new((100_u64, 200_u64));
        let mut remaining = 1_000_u64;
        let error = retry_budgeted_route_measurement_on_loss_with_debit(
            &mut remaining,
            1,
            || Ok(counters.get()),
            |_| {
                attempts.set(attempts.get() + 1);
                counters.set((110, 220));
                Err::<(), _>("speedtest-route-not-ready".to_string())
            },
            &mut |_| Err("traffic-debit-persistence-failed".to_string()),
        )
        .unwrap_err();
        assert_eq!(error, "traffic-debit-persistence-failed");
        assert_eq!(attempts.get(), 1);
        assert_eq!(remaining, 970);
    }

    #[test]
    fn route_measurement_retry_keeps_static_errors_and_accounting_fail_closed() {
        let attempts = Cell::new(0_u32);
        let counters = Cell::new((100_u64, 200_u64));
        let mut remaining = 1_000_u64;
        let error = retry_budgeted_route_measurement_on_loss::<(), _, _>(
            &mut remaining,
            1,
            || Ok(counters.get()),
            |_| {
                attempts.set(attempts.get() + 1);
                counters.set((110, 210));
                Err("speedtest-route-identity-mismatch".to_string())
            },
        )
        .unwrap_err();
        assert_eq!(error, "speedtest-route-identity-mismatch");
        assert_eq!(attempts.get(), 1);
        assert_eq!(remaining, 980);

        let reads = Cell::new(0_u32);
        let mut remaining = 1_000_u64;
        let error = retry_budgeted_route_measurement_on_loss::<(), _, _>(
            &mut remaining,
            1,
            || {
                let read = reads.get() + 1;
                reads.set(read);
                Ok(if read == 1 { (100, 100) } else { (1, 1) })
            },
            |_| Err("speedtest-route-not-ready".to_string()),
        )
        .unwrap_err();
        assert_eq!(error, "speedtest-counter-reset");
        assert_eq!(reads.get(), 2);
        assert_eq!(remaining, 1_000);
    }

    #[test]
    fn route_pin_acquire_retries_only_a_proven_readiness_race() {
        let waits = Cell::new(0_u32);
        let acquires = Cell::new(0_u32);
        let attestations = Cell::new(0_u32);
        let pin = acquire_route_pin_when_ready(
            || {
                waits.set(waits.get() + 1);
                Ok(())
            },
            || {
                let attempt = acquires.get() + 1;
                acquires.set(attempt);
                if attempt == 1 {
                    Err("speedtest-mwan3-environment-failed".to_string())
                } else {
                    Ok("route-pin")
                }
            },
            || {
                attestations.set(attestations.get() + 1);
                Err("speedtest-route-not-ready".to_string())
            },
        )
        .unwrap();
        assert_eq!(pin, "route-pin");
        assert_eq!(waits.get(), 2);
        assert_eq!(acquires.get(), 2);
        assert_eq!(attestations.get(), 1);
    }

    #[test]
    fn route_pin_acquire_keeps_static_and_identity_failures_fatal() {
        let attestations = Cell::new(0_u32);
        let static_error = acquire_route_pin_when_ready::<(), _, _, _>(
            || Ok(()),
            || Err("speedtest-route-pin-install-failed".to_string()),
            || {
                attestations.set(attestations.get() + 1);
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(static_error, "speedtest-route-pin-install-failed");
        assert_eq!(attestations.get(), 0);

        let still_online = acquire_route_pin_when_ready::<(), _, _, _>(
            || Ok(()),
            || Err("speedtest-mwan3-environment-failed".to_string()),
            || Ok(()),
        )
        .unwrap_err();
        assert_eq!(still_online, "speedtest-mwan3-environment-failed");

        let identity_error = acquire_route_pin_when_ready::<(), _, _, _>(
            || Ok(()),
            || Err("speedtest-mwan3-environment-failed".to_string()),
            || Err("speedtest-route-identity-mismatch".to_string()),
        )
        .unwrap_err();
        assert_eq!(identity_error, "speedtest-route-identity-mismatch");
    }

    #[test]
    fn terminal_round_trip_preserves_result_and_identity() {
        let result = SpeedtestResult {
            direction: SpeedtestDirection::Both,
            download_kbps: Some(900_000),
            upload_kbps: Some(766_600),
            rx_bytes: 1_000_000,
            tx_bytes: 2_000_000,
            elapsed_ms: 22_000,
            server_id: Some(17372),
            server_name: "Tallinn".to_string(),
            server_sponsor: "Telia Eesti AS".to_string(),
        };
        let terminal = SpeedtestTerminal::Complete(result);
        let encoded = terminal.encode(&"a".repeat(32), &"b".repeat(32)).unwrap();
        let decoded = SpeedtestTerminalRecord::decode(&encoded).unwrap();
        assert_eq!(decoded.job_id, "a".repeat(32));
        assert_eq!(decoded.worker_run_id, "b".repeat(32));
        assert_eq!(decoded.terminal, terminal);
    }
}
