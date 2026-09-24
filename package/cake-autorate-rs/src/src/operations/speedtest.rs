use super::autotune_apply_openwrt::OpenWrtNativeApplyBackend;
use super::autotune_runtime::{
    speedtest_unshaped_topology, AbsentRuntimeBaseline, AutotuneRuntimePermit, RuntimePermitKind,
};
use super::autotune_runtime_store::RuntimeOverrideStore;
use super::full_autotune::AutotuneRuntimeControl;
use super::identity::{
    monotonic_boot_ms, read_kernel_uuid, ProcessIdentity, DEFAULT_BOOT_ID_PATH, DEFAULT_PROC_ROOT,
};
use super::process::{
    run_bounded_command_output, run_bounded_command_output_with_input, SpawnSpec,
};
use super::protocol::{
    OperationKind, OperationRequest, OperationRouteMode, OperationTargetState, SpeedtestDirection,
};
use super::rating;
#[cfg(test)]
use crate::owned_route_rules::MAX_ACCOUNTING_FLOWS;
use crate::owned_route_rules::{
    attest_route_pin_snapshot, cleanup_named_route_pin_with, install_owned_route_pin_with,
    nft_table_snapshot_arguments, nft_table_snapshot_proves_absence, NftSocketOwner,
    ACCOUNTING_FAULT_COUNTER, ACCOUNTING_RX_COUNTER, ACCOUNTING_TX_COUNTER,
    FLOW_ACCOUNTING_OWNER_SUFFIX,
};
#[cfg(test)]
use crate::owned_route_rules::{nft_egress_guard_batch, nft_owned_route_pin_batch};
use crate::routing::{inspect_route, RouteIdentity, RouteSnapshot, RouteSpec};
use crate::Config;
use std::ffi::OsString;
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
const ROUTE_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const OWNED_BUDGET_READ_TIMEOUT: Duration = Duration::from_millis(250);
const ROUTE_RECHECK_INTERVAL: Duration = Duration::from_secs(1);
const QUALIFICATION_CPU_PRESSURE_SAMPLES: u8 = 3;
pub(crate) const MIN_ROUTE_PROOF_BYTES: u64 = 64 * 1024;
pub(crate) const MAX_UNPROVED_TRAFFIC_ATTEMPTS: u8 = 3;
const TRAFFIC_STOP_RESERVE_WINDOW_MS: u128 = 1_000;
const TRAFFIC_STOP_RESERVE_HEADROOM_PERCENT: u128 = 125;
const TRAFFIC_STOP_RESERVE_FIXED_BYTES: u128 = 1024 * 1024;
const MAX_BACKEND_RUNTIME: Duration = Duration::from_secs(180);
const MAX_SERVER_LIST_RUNTIME: Duration = Duration::from_secs(30);
const MAX_RATE_PAYLOAD_TIMING_RATIO_PERCENT: u128 = 135;
pub(crate) const SPEEDTEST_RATE_PAYLOAD_TIMING_MISMATCH: &str =
    "speedtest-rate-payload-timing-mismatch";
#[cfg(test)]
const MAX_UPLOAD_COUNTER_LAG_PERCENT: u128 = 5;
const MIN_QUALIFICATION_COUNTER_CONFIDENCE_PERCENT: u128 = 80;
const MIN_SHAPED_UPLOAD_PAYLOAD_WIRE_PERCENT: u128 = 60;
const MAX_QUALIFICATION_DIAGNOSTIC_BYTES: usize = 1024;
const SIGKILL: i32 = 9;
const PR_SET_PDEATHSIG: i32 = 1;

pub(crate) const SPEEDTEST_TRAFFIC_BUDGET_EXHAUSTED: &str = "speedtest-traffic-budget-exhausted";
pub(crate) const SPEEDTEST_TRAFFIC_LIMIT_REACHED: &str = "speedtest-traffic-limit-reached";

/// Reserve observation/termination headroom. Owned supervision polls every 100 ms
/// with a shared 250 ms counter-read deadline; legacy supervision reads the
/// interface. One full second plus 25% and one MiB
/// deliberately exceeds the normal observation/kill path while remaining
/// usable on wide links.  If the supplied rate authority is too large to fit
/// in u64, the saturated result makes admission fail closed before transfer.
/// This is not a guarantee of physical-wire precision or arbitrary kernel-stall timing.
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

/// Stopping reserve for a standalone Speed Test launched with an explicit
/// capped user policy.  Each measured direction needs a rate authority carried
/// in the immutable request: a declared service ceiling, or the managed CAKE
/// ceiling that bounds a still-shaped direction.  Nothing is invented; a
/// missing authority refuses the capped launch instead of hiding a reserve.
pub(crate) fn explicit_speedtest_stop_reserve_bytes(
    direction: SpeedtestDirection,
    download_bound_kbps: Option<u64>,
    upload_bound_kbps: Option<u64>,
) -> Result<u64, String> {
    let bound = |measured: bool, value: Option<u64>| {
        if !measured {
            return Ok(0);
        }
        value
            .filter(|rate| *rate > 0)
            .ok_or_else(|| "traffic-stop-authority-unavailable".to_string())
    };
    Ok(traffic_stop_safety_reserve_bytes(
        bound(direction != SpeedtestDirection::Upload, download_bound_kbps)?,
        bound(direction != SpeedtestDirection::Download, upload_bound_kbps)?,
    ))
}

/// Minimum explicit capped allowance: the stopping reserve plus route proof
/// for every measured direction.  Passing it does not guarantee completion.
pub(crate) fn minimum_explicit_speedtest_traffic_budget_bytes(
    direction: SpeedtestDirection,
    download_bound_kbps: Option<u64>,
    upload_bound_kbps: Option<u64>,
) -> Result<u64, String> {
    Ok(minimum_attempt_budget(
        direction,
        explicit_speedtest_stop_reserve_bytes(direction, download_bound_kbps, upload_bound_kbps)?,
    ))
}

/// Runtime reserve of a standalone request.  Historical requests keep their
/// original zero-reserve semantics; their derived budget already included it.
fn standalone_speedtest_stop_reserve_bytes(
    request: &OperationRequest,
    direction: SpeedtestDirection,
) -> Result<u64, String> {
    if request.identity.operation != OperationKind::Speedtest
        || !request.traffic_policy_explicit
        || request.traffic_budget == super::protocol::TrafficPolicy::Unlimited
    {
        return Ok(0);
    }
    explicit_speedtest_stop_reserve_bytes(
        direction,
        request.service_dl_cap_kbps,
        request.service_ul_cap_kbps,
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
    pub endpoint_host: Option<String>,
    pub endpoint_sha256: Option<String>,
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
    pub lifecycle: bool,
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
    /// Summed route-window debits; `None` for a version 1 terminal.
    pub debited_bytes: Option<u64>,
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
    cleanup_on_drop: bool,
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
    probe_pin: Option<NftRoutePin>,
    credentials: BackendCredentials,
    job_id: String,
    worker_run_id: String,
    route_fingerprint: String,
    route: super::protocol::OperationRouteIdentity,
    backend: String,
    requested_server_id: Option<u64>,
    authorized_traffic_budget: super::protocol::TrafficPolicy,
    traffic_policy_explicit: bool,
    selected_server_id: Option<u64>,
    server_qualified: bool,
    pub(crate) qualified_raw_capacity: Option<super::server_qualification::QualifiedCapacity>,
    selected_endpoint_sha256: Option<String>,
    owned_checkpoint: Option<(PathBuf, String)>,
    owned_ledger: Option<std::cell::Cell<OwnedTrafficLedger>>,
}

impl EmbeddedSpeedtestSession {
    pub(crate) fn owned_budget_watch(
        &self,
        reserve: u64,
    ) -> Result<Option<OwnedBudgetWatch<'_>>, String> {
        let Some(ledger) = &self.owned_ledger else {
            return Ok(None);
        };
        let ledger = ledger.get();
        ledger.remaining()?;
        Ok(Some(OwnedBudgetWatch {
            backend: &self.route_pin,
            probes: self
                .probe_pin
                .as_ref()
                .ok_or("probe accounting table is missing")?,
            limit: ledger
                .authority
                .checked_sub(reserve)
                .ok_or(SPEEDTEST_TRAFFIC_BUDGET_EXHAUSTED)?,
            previous: ledger.committed.counters,
        }))
    }

    fn commit_owned_snapshot(
        &self,
        observed: OwnedTrafficSnapshot,
        lifecycle: bool,
        remaining: &mut super::protocol::TrafficPolicy,
        on_debit: &mut dyn FnMut(SpeedtestTrafficDebit) -> Result<(), String>,
    ) -> Result<bool, String> {
        let slot = self
            .owned_ledger
            .as_ref()
            .ok_or("owned traffic ledger is unavailable")?;
        let (path, request_digest) = self
            .owned_checkpoint
            .as_ref()
            .ok_or("owned traffic checkpoint path is missing")?;
        let mut ledger = slot.get();
        let result = ledger.checkpoint(observed, |point, mut debit| {
            debit.lifecycle = lifecycle;
            point
                .commit_debit_with_intent(
                    path,
                    &self.job_id,
                    &self.worker_run_id,
                    request_digest,
                    || on_debit(debit),
                )
                .map_err(|error| format!("speedtest-owned-persistence-failed: {error}"))
        });
        slot.set(ledger); // Preserve poison on an ambiguous persistence failure.
        let exceeded = result.map_err(|error| {
            if error.starts_with("speedtest-owned-persistence-failed:") {
                error
            } else {
                format!("speedtest-owned-budget-observation-failed: {error}")
            }
        })?;
        *remaining = ledger.remaining()?;
        Ok(exceeded)
    }

    pub(crate) fn checkpoint_owned_lifecycle(
        &self,
        remaining: &mut super::protocol::TrafficPolicy,
        on_debit: &mut dyn FnMut(SpeedtestTrafficDebit) -> Result<(), String>,
    ) -> Result<bool, String> {
        if self.owned_ledger.is_none() {
            return Ok(false);
        }
        self.commit_owned_snapshot(self.owned_traffic_snapshot()?, true, remaining, on_debit)
    }

    pub(crate) fn owned_traffic_snapshot(&self) -> Result<OwnedTrafficSnapshot, String> {
        if self.probe_pin.is_none() {
            return Err("probe accounting table is missing".into());
        }
        read_owned_traffic_counters_with(
            &self.job_id,
            &self.worker_run_id,
            Instant::now() + OWNED_BUDGET_READ_TIMEOUT,
            |table, owner, deadline| {
                let pin = NftRoutePin {
                    table: Some(table.to_string()),
                    owner: Some(owner.to_string()),
                    cleanup_on_drop: false,
                };
                pin.traffic_counters_before(deadline)
            },
        )
    }

    pub(crate) fn publish_probe_owner(
        &mut self,
        store: &RuntimeOverrideStore,
        permit: &AutotuneRuntimePermit,
    ) -> Result<(), String> {
        if !permit.probe_accounting_required {
            return Ok(());
        }
        let directory = self
            .owned_checkpoint
            .as_ref()
            .and_then(|(path, _)| path.parent())
            .ok_or("owned traffic checkpoint directory is missing")?;
        match fs::symlink_metadata(
            directory.join(format!("owned-traffic-unstarted-{}", self.worker_run_id)),
        ) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err("owned producer admission was already closed".into()),
        }
        if self.job_id != permit.job_id
            || self.worker_run_id != permit.worker_run_id
            || self.route_fingerprint != permit.route_fingerprint
        {
            return Err("probe accounting session identity mismatch".into());
        }
        let initial = OwnedTrafficCheckpoint {
            sequence: 0,
            counters: self.owned_traffic_snapshot()?,
        };
        let (path, request_sha256) = self
            .owned_checkpoint
            .as_ref()
            .ok_or("owned traffic checkpoint path is missing")?;
        initial.publish_bound(path, &self.job_id, &self.worker_run_id, request_sha256)?;
        self.owned_ledger = Some(std::cell::Cell::new(
            OwnedTrafficLedger::from_verified_checkpoint(self.authorized_traffic_budget, initial)?,
        ));
        let pin = self
            .probe_pin
            .as_mut()
            .ok_or("probe accounting table is missing")?;
        let owner = super::autotune_runtime_store::ProbeAccountingOwner {
            job_id: self.job_id.clone(),
            worker_run_id: self.worker_run_id.clone(),
            permit_id: permit.permit_id.clone(),
            boot_id: permit.boot_id.clone(),
            route_fingerprint: self.route_fingerprint.clone(),
            backend_uid: self.credentials.uid,
            probe_gid: self.credentials.gid,
        };
        store.publish_probe_accounting_owner(permit, &owner)?;
        // Coordinator retirement/cleanup owns the tables after publication;
        // persistent probe sockets may outlive this worker's stack.
        self.route_pin.cleanup_on_drop = false;
        pin.cleanup_on_drop = false;
        Ok(())
    }

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
            || {
                NftRoutePin::acquire(
                    request,
                    worker_run_id,
                    NftSocketOwner::BackendUid(credentials.uid),
                )
            },
            || attest_route(request).map(|_| ()),
        )?;
        let probe_pin = if request.identity.operation == OperationKind::FullAutotune
            && request.traffic_policy_explicit
        {
            Some(NftRoutePin::acquire(
                request,
                worker_run_id,
                NftSocketOwner::ProbeRootGid(credentials.gid),
            )?)
        } else {
            None
        };
        let owned_checkpoint = if probe_pin.is_some() {
            Some((
                scratch_path
                    .parent()
                    .ok_or("owned traffic checkpoint has no job directory")?
                    .join(format!("owned-traffic-checkpoint-{worker_run_id}")),
                super::autotune_apply::native_apply_sha256_hex(request.encode()?.as_bytes()),
            ))
        } else {
            None
        };
        Ok(Self {
            route_pin,
            probe_pin,
            owned_checkpoint,
            owned_ledger: None,
            credentials,
            job_id: request.identity.job_id.clone(),
            worker_run_id: worker_run_id.to_string(),
            route_fingerprint: request.identity.route_fingerprint.clone(),
            route: request.route.clone(),
            backend: request.backend.clone(),
            requested_server_id: request.speedtest_server_id,
            authorized_traffic_budget: request.traffic_budget,
            traffic_policy_explicit: request.traffic_policy_explicit,
            selected_server_id: request.speedtest_server_id,
            server_qualified: false,
            qualified_raw_capacity: None,
            selected_endpoint_sha256: None,
        })
    }

    pub(crate) fn close(mut self) -> Result<(), String> {
        if self.route_pin.cleanup_on_drop {
            self.route_pin.release()?;
        }
        if let Some(pin) = self.probe_pin.as_mut() {
            if pin.cleanup_on_drop {
                pin.release()?;
            }
        }
        Ok(())
    }

    fn matches(&self, request: &OperationRequest, worker_run_id: &str) -> bool {
        self.job_id == request.identity.job_id
            && self.worker_run_id == worker_run_id
            && self.route_fingerprint == request.identity.route_fingerprint
            && self.route == request.route
            && self.backend == request.backend
            && self.requested_server_id == request.speedtest_server_id
            && self.traffic_policy_explicit == request.traffic_policy_explicit
            && self
                .authorized_traffic_budget
                .permits(request.traffic_budget)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SpeedtestTrafficCounters {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

/// Independent cumulative kernel counters. These are owned IP-packet counts,
/// not a proof of physical-WAN bytes (notably for shared resolver traffic).
/// Do not merge probe counters into backend goodput/measurement evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OwnedTrafficSnapshot {
    pub backend: SpeedtestTrafficCounters,
    pub probes: SpeedtestTrafficCounters,
}

impl OwnedTrafficSnapshot {
    pub fn total_bytes(self) -> Result<u64, String> {
        self.backend
            .rx_bytes
            .checked_add(self.backend.tx_bytes)
            .and_then(|bytes| bytes.checked_add(self.probes.rx_bytes))
            .and_then(|bytes| bytes.checked_add(self.probes.tx_bytes))
            .ok_or_else(|| "owned traffic cumulative counter overflow".into())
    }

    fn delta_since(self, previous: Self) -> Result<SpeedtestTrafficDebit, String> {
        // Validate each component before summing: a growing probe count cannot
        // hide a reset of the backend's counter, or vice versa.
        let backend = counter_deltas(
            (previous.backend.rx_bytes, previous.backend.tx_bytes),
            (self.backend.rx_bytes, self.backend.tx_bytes),
        )?;
        let probes = counter_deltas(
            (previous.probes.rx_bytes, previous.probes.tx_bytes),
            (self.probes.rx_bytes, self.probes.tx_bytes),
        )?;
        let rx_bytes = backend
            .0
            .checked_add(probes.0)
            .ok_or("owned traffic RX delta overflow")?;
        let tx_bytes = backend
            .1
            .checked_add(probes.1)
            .ok_or("owned traffic TX delta overflow")?;
        rx_bytes
            .checked_add(tx_bytes)
            .ok_or("owned traffic total delta overflow")?;
        Ok(SpeedtestTrafficDebit {
            lifecycle: false,
            rx_bytes,
            tx_bytes,
        })
    }
}

pub(crate) fn supervise_owned_traffic(
    directory: &Path,
    request: &OperationRequest,
    worker: &str,
    previous: Option<OwnedTrafficSnapshot>,
) -> Result<OwnedTrafficSnapshot, String> {
    let previous = match previous {
        Some(previous) => previous,
        None => {
            route_pin_table_name(&request.identity.job_id, worker)?;
            let path = directory.join(format!("owned-traffic-checkpoint-{worker}"));
            let bytes = super::autotune_apply_runtime::read_private_recovery_bounded(
                &path,
                4096,
                "owned traffic checkpoint",
            )?;
            let digest =
                super::autotune_apply::native_apply_sha256_hex(request.encode()?.as_bytes());
            OwnedTrafficCheckpoint::decode_bound(
                std::str::from_utf8(&bytes).map_err(|_| "owned checkpoint is not UTF-8")?,
                &request.identity.job_id,
                worker,
                &digest,
            )?
            .counters
        }
    };
    let current = read_owned_traffic_counters_with(
        &request.identity.job_id,
        worker,
        Instant::now() + OWNED_BUDGET_READ_TIMEOUT,
        |table, owner, deadline| {
            NftRoutePin {
                table: Some(table.to_string()),
                owner: Some(owner.to_string()),
                cleanup_on_drop: false,
            }
            .traffic_counters_before(deadline)
        },
    )?;
    current.delta_since(previous)?;
    Ok(current)
}

struct BackendOutputGuard {
    output: File,
    stderr: File,
}

// Durable counter cursor. Debit recovery must also reconcile the evidence
// journal; decoding this file alone does not acknowledge an uncertain append.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OwnedTrafficCheckpoint {
    sequence: u32,
    counters: OwnedTrafficSnapshot,
}

/// A bound checkpoint identifies the accounting method, not final settlement.
pub(crate) fn verify_owned_accounting_checkpoint(
    directory: &Path,
    request: &OperationRequest,
    worker: &str,
) -> Result<(u32, u64, u64), String> {
    if !request.traffic_policy_explicit {
        return Err("owned accounting requires an explicit traffic policy".into());
    }
    if worker.len() != 32
        || !worker
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("owned accounting worker identity is invalid".into());
    }
    super::autotune_apply_runtime::require_private_directory(directory)?;
    let path = directory.join(format!("owned-traffic-checkpoint-{worker}"));
    match fs::symlink_metadata(path.with_extension("owned-intent")) {
        Ok(_) => return Err("owned traffic checkpoint has an unacknowledged debit intent".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("unable to inspect owned debit intent: {error}")),
    }
    match fs::symlink_metadata(path.with_extension("owned-next")) {
        Ok(_) => return Err("owned traffic checkpoint has unresolved staging residue".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("unable to inspect owned traffic staging: {error}")),
    }
    let bytes = super::autotune_apply_runtime::read_private_recovery_bounded(
        &path,
        4096,
        "owned traffic checkpoint",
    )?;
    let digest = super::autotune_apply::native_apply_sha256_hex(request.encode()?.as_bytes());
    let input = std::str::from_utf8(&bytes).map_err(|_| "owned traffic checkpoint is not UTF-8")?;
    let checkpoint =
        OwnedTrafficCheckpoint::decode_bound(input, &request.identity.job_id, worker, &digest)?;
    let counters = checkpoint.counters;
    Ok((
        checkpoint.sequence,
        counters
            .backend
            .rx_bytes
            .checked_add(counters.probes.rx_bytes)
            .ok_or("owned traffic RX overflow")?,
        counters
            .backend
            .tx_bytes
            .checked_add(counters.probes.tx_bytes)
            .ok_or("owned traffic TX overflow")?,
    ))
}

impl OwnedTrafficCheckpoint {
    fn commit_debit_with_intent(
        self,
        path: &Path,
        job: &str,
        worker: &str,
        request_sha256: &str,
        append_debit: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        use super::autotune_apply_runtime::{
            read_private_recovery_bounded, require_private_directory, sync_directory,
            write_new_private_file,
        };
        let directory = path.parent().ok_or("owned checkpoint has no directory")?;
        require_private_directory(directory)?;
        let previous = read_private_recovery_bounded(path, 4096, "owned traffic checkpoint")?;
        let previous = Self::decode_bound(
            std::str::from_utf8(&previous).map_err(|_| "owned checkpoint is not UTF-8")?,
            job,
            worker,
            request_sha256,
        )?;
        if previous.sequence.checked_add(1) != Some(self.sequence) {
            return Err("owned debit intent is not contiguous".into());
        }
        let delta = self.counters.delta_since(previous.counters)?;
        if delta.rx_bytes == 0 && delta.tx_bytes == 0 {
            return Err("owned debit intent is empty".into());
        }
        let intent = path.with_extension("owned-intent");
        let encoded = self.encode_bound(job, worker, request_sha256)?;
        // Never replay an append merely because its intended counters match.
        // An existing intent needs journal reconciliation, not another append.
        write_new_private_file(&intent, encoded.as_bytes())?;
        sync_directory(directory)?;
        append_debit()?;
        self.publish_bound(path, job, worker, request_sha256)?;
        if read_private_recovery_bounded(&intent, 4096, "owned debit intent")? != encoded.as_bytes()
        {
            return Err("owned debit intent changed before acknowledgement".into());
        }
        fs::remove_file(&intent)
            .map_err(|error| format!("unable to acknowledge owned debit intent: {error}"))?;
        sync_directory(directory)
    }

    fn encode_bound(self, job: &str, worker: &str, request_sha256: &str) -> Result<String, String> {
        for (value, length) in [(job, 32), (worker, 32), (request_sha256, 64)] {
            if value.len() != length
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err("owned traffic checkpoint identity is invalid".into());
            }
        }
        if (self.sequence == 0) != (self.counters.total_bytes()? == 0) {
            return Err("owned traffic checkpoint origin is invalid".into());
        }
        let payload = serde_json::json!({"schema_version":1,"job_id":job,"worker_run_id":worker,
            "request_sha256":request_sha256,"accounting":"owned-ip-system-dns-estimate-v1",
            "sequence":self.sequence,"backend_rx":self.counters.backend.rx_bytes,"backend_tx":self.counters.backend.tx_bytes,
            "probe_rx":self.counters.probes.rx_bytes,"probe_tx":self.counters.probes.tx_bytes});
        let checksum =
            super::autotune_apply::native_apply_sha256_hex(payload.to_string().as_bytes());
        Ok(format!(
            "{}\n",
            serde_json::json!({"payload":payload,"sha256":checksum})
        ))
    }

    fn decode_bound(
        input: &str,
        job: &str,
        worker: &str,
        request_sha256: &str,
    ) -> Result<Self, String> {
        if input.len() > 4096 {
            return Err("owned traffic checkpoint is oversized".into());
        }
        let value: serde_json::Value =
            serde_json::from_str(input).map_err(|_| "owned traffic checkpoint JSON is invalid")?;
        let payload = &value["payload"];
        let number = |key| {
            payload[key]
                .as_u64()
                .ok_or("owned traffic checkpoint counter is invalid")
        };
        let point = Self {
            sequence: number("sequence")?
                .try_into()
                .map_err(|_| "owned traffic sequence is too large")?,
            counters: OwnedTrafficSnapshot {
                backend: SpeedtestTrafficCounters {
                    rx_bytes: number("backend_rx")?,
                    tx_bytes: number("backend_tx")?,
                },
                probes: SpeedtestTrafficCounters {
                    rx_bytes: number("probe_rx")?,
                    tx_bytes: number("probe_tx")?,
                },
            },
        };
        if point.encode_bound(job, worker, request_sha256)? != input {
            return Err("owned traffic checkpoint binding or integrity mismatch".into());
        }
        Ok(point)
    }

    fn publish_bound(
        self,
        path: &Path,
        job: &str,
        worker: &str,
        request_sha256: &str,
    ) -> Result<(), String> {
        use super::autotune_apply_runtime::{
            read_private_recovery_bounded, replace_private_file, require_private_directory,
            sync_directory, write_new_private_file,
        };
        let parent = path
            .parent()
            .ok_or("owned traffic checkpoint has no directory")?;
        require_private_directory(parent)?;
        let encoded = self.encode_bound(job, worker, request_sha256)?;
        let staged = path.with_extension("owned-next");
        match fs::symlink_metadata(&staged) {
            Ok(_) => return Err("owned traffic checkpoint has unresolved staging residue".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("unable to inspect owned traffic staging: {error}")),
        }
        match fs::symlink_metadata(path) {
            Ok(_) => {
                let bytes = read_private_recovery_bounded(path, 4096, "owned traffic checkpoint")?;
                let previous = Self::decode_bound(
                    std::str::from_utf8(&bytes)
                        .map_err(|_| "owned traffic checkpoint is not UTF-8")?,
                    job,
                    worker,
                    request_sha256,
                )?;
                if previous == self {
                    return sync_directory(parent);
                }
                if previous.sequence.checked_add(1) != Some(self.sequence) {
                    return Err("owned traffic checkpoint sequence is not contiguous".into());
                }
                let delta = self.counters.delta_since(previous.counters)?;
                if delta.rx_bytes == 0 && delta.tx_bytes == 0 {
                    return Err("owned traffic checkpoint has no new bytes".into());
                }
                replace_private_file(path, &staged, encoded.as_bytes())?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if self.sequence != 0 {
                    return Err("owned traffic checkpoint origin is missing".into());
                }
                write_new_private_file(path, encoded.as_bytes())?;
            }
            Err(error) => {
                return Err(format!(
                    "unable to inspect owned traffic checkpoint: {error}"
                ))
            }
        }
        sync_directory(parent)
    }
}

#[derive(Clone, Copy)]
struct OwnedTrafficLedger {
    authority: super::protocol::TrafficPolicy,
    committed: OwnedTrafficCheckpoint,
    persistence_uncertain: bool,
}

impl OwnedTrafficLedger {
    fn from_verified_checkpoint(
        authority: super::protocol::TrafficPolicy,
        committed: OwnedTrafficCheckpoint,
    ) -> Result<Self, String> {
        let total = committed.counters.total_bytes()?;
        if committed.sequence == 0 && total != 0 {
            return Err("initial owned traffic checkpoint is not zero".into());
        }
        Ok(Self {
            authority,
            committed,
            persistence_uncertain: false,
        })
    }

    fn remaining(&self) -> Result<super::protocol::TrafficPolicy, String> {
        if self.persistence_uncertain {
            return Err("owned traffic persistence is uncertain".into());
        }
        Ok(self
            .authority
            .checked_sub(self.committed.counters.total_bytes()?)
            .unwrap_or(0_u64.into()))
    }

    fn checkpoint(
        &mut self,
        observed: OwnedTrafficSnapshot,
        persist: impl FnOnce(OwnedTrafficCheckpoint, SpeedtestTrafficDebit) -> Result<(), String>,
    ) -> Result<bool, String> {
        if self.persistence_uncertain {
            return Err("owned traffic persistence is uncertain".into());
        }
        // A reset/overflow or ambiguous append is terminal for this in-memory
        // cursor. Recovery must reload a verified durable checkpoint.
        self.persistence_uncertain = true;
        let total = observed.total_bytes()?;
        let debit = observed.delta_since(self.committed.counters)?;
        let exceeded = self.authority.exceeded(total);
        if debit.rx_bytes != 0 || debit.tx_bytes != 0 {
            let next = OwnedTrafficCheckpoint {
                sequence: self
                    .committed
                    .sequence
                    .checked_add(1)
                    .ok_or("owned traffic sequence overflow")?,
                counters: observed,
            };
            persist(next, debit)?;
            self.committed = next;
        }
        self.persistence_uncertain = false;
        Ok(exceeded)
    }
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

pub(crate) struct OwnedBudgetWatch<'a> {
    backend: &'a NftRoutePin,
    probes: &'a NftRoutePin,
    limit: super::protocol::TrafficPolicy,
    previous: OwnedTrafficSnapshot,
}

impl OwnedBudgetWatch<'_> {
    /// Wait-phase checks do not acknowledge or debit evidence. A failed read
    /// poisons the ledger so a later success cannot hide uncertain accounting.
    pub(crate) fn check_wait(&mut self, session: &EmbeddedSpeedtestSession) -> Result<(), String> {
        match self.poll() {
            Ok(false) => Ok(()),
            Ok(true) => Err(SPEEDTEST_TRAFFIC_BUDGET_EXHAUSTED.to_string()),
            Err(error) => {
                if let Some(slot) = &session.owned_ledger {
                    let mut ledger = slot.get();
                    ledger.persistence_uncertain = true;
                    slot.set(ledger);
                }
                Err(error)
            }
        }
    }

    fn observe(&mut self, observed: OwnedTrafficSnapshot) -> Result<bool, String> {
        observed.delta_since(self.previous)?;
        let consumed = observed.total_bytes()?;
        self.previous = observed;
        Ok(self.limit.exceeded(consumed))
    }

    fn poll(&mut self) -> Result<bool, String> {
        let result = (|| {
            let deadline = Instant::now()
                .checked_add(OWNED_BUDGET_READ_TIMEOUT)
                .ok_or("owned budget deadline overflow")?;
            let observed = OwnedTrafficSnapshot {
                backend: self.backend.traffic_counters_before(deadline)?,
                probes: self.probes.traffic_counters_before(deadline)?,
            };
            self.observe(observed)
        })();
        result
            .map_err(|error: String| format!("speedtest-owned-budget-observation-failed: {error}"))
    }
}

enum BackendExecution<'scope> {
    Direct(BackendChild),
    Watched {
        stop: std::sync::mpsc::Sender<()>,
        worker: Option<std::thread::ScopedJoinHandle<'scope, Result<bool, String>>>,
        outcome: Option<Result<bool, String>>,
    },
}

impl BackendExecution<'_> {
    fn try_wait(&mut self) -> Result<bool, String> {
        match self {
            Self::Direct(child) => child.try_wait(),
            Self::Watched {
                worker, outcome, ..
            } => {
                if worker.as_ref().is_some_and(|worker| worker.is_finished()) {
                    if let Some(finished) = worker.take() {
                        *outcome =
                            Some(finished.join().unwrap_or_else(|_| {
                                Err("speedtest-budget-watcher-panicked".into())
                            }));
                    }
                }
                match outcome {
                    Some(Ok(_)) => Ok(true),
                    Some(Err(error)) => Err(error.clone()),
                    None => Ok(false),
                }
            }
        }
    }

    fn finish(&mut self) -> Result<bool, String> {
        match self {
            Self::Direct(child) => child.finish(),
            Self::Watched {
                worker, outcome, ..
            } => {
                if let Some(worker) = worker.take() {
                    *outcome = Some(
                        worker
                            .join()
                            .unwrap_or_else(|_| Err("speedtest-budget-watcher-panicked".into())),
                    );
                }
                outcome
                    .clone()
                    .ok_or("speedtest budget watcher has no outcome")?
            }
        }
    }

    fn stop_and_reap(&mut self) -> Result<(), String> {
        match self {
            Self::Direct(child) => child.stop_and_reap(),
            Self::Watched { stop, .. } => {
                let _ = stop.send(());
                self.finish().map(|_| ())
            }
        }
    }
}

impl Drop for BackendExecution<'_> {
    fn drop(&mut self) {
        let _ = self.stop_and_reap();
    }
}

fn with_backend_supervision<T, Watch>(
    mut child: BackendChild,
    watch: Option<Watch>,
    terminate: &AtomicBool,
    deadline: Instant,
    timeout_code: &'static str,
    action: impl for<'scope> FnOnce(&mut BackendExecution<'scope>) -> Result<T, String>,
) -> Result<T, String>
where
    Watch: FnMut() -> Result<bool, String> + Send,
{
    thread::scope(|scope| {
        let mut execution = if let Some(mut watch) = watch {
            let (stop, commands) = std::sync::mpsc::channel();
            let worker = thread::Builder::new()
                .name("cake-byte-budget".into())
                .spawn_scoped(scope, move || loop {
                    if terminate.load(Ordering::Relaxed) {
                        child.stop_and_reap()?;
                        return Err("speedtest-cancelled".into());
                    }
                    if child.try_wait()? {
                        return child.finish();
                    }
                    if Instant::now() >= deadline {
                        child.stop_and_reap()?;
                        return Err(timeout_code.into());
                    }
                    if watch()? {
                        child.stop_and_reap()?;
                        if child.finish()? {
                            return Ok(true);
                        }
                        return Err(SPEEDTEST_TRAFFIC_LIMIT_REACHED.into());
                    }
                    match commands.recv_timeout(POLL_INTERVAL) {
                        Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            child.stop_and_reap()?;
                            return Ok(false);
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    }
                })
                .map_err(|error| format!("speedtest-budget-watcher-start-failed: {error}"))?;
            BackendExecution::Watched {
                stop,
                worker: Some(worker),
                outcome: None,
            }
        } else {
            BackendExecution::Direct(child)
        };
        action(&mut execution)
    })
}

impl NftRoutePin {
    fn traffic_counters_before(
        &self,
        deadline: Instant,
    ) -> Result<SpeedtestTrafficCounters, String> {
        let table = self
            .table
            .as_deref()
            .ok_or("speedtest-accounting-table-missing")?;
        let owner = self
            .owner
            .as_deref()
            .ok_or("speedtest-accounting-owner-missing")?;
        let listed = run_bounded_accounting_command(
            &nft_table_snapshot_arguments(table),
            accounting_time_remaining(deadline)?,
            &|| false,
        )?;
        if !listed.0 {
            return Err("speedtest-accounting-counters-missing-during-measurement".into());
        }
        let json = String::from_utf8(listed.1).map_err(|_| "speedtest-accounting-json-invalid")?;
        parse_named_traffic_counters(&json, table, owner)
    }

    fn acquire(
        request: &OperationRequest,
        worker_run_id: &str,
        socket_owner: NftSocketOwner,
    ) -> Result<Self, String> {
        if matches!(socket_owner, NftSocketOwner::ProbeRootGid(0 | u32::MAX)) {
            return Err("speedtest-probe-group-invalid".into());
        }
        if matches!(socket_owner, NftSocketOwner::ProbeRootGid(_))
            && super::autotune_runtime_store::effective_uid() != 0
        {
            return Err("probe accounting requires a root worker".into());
        }
        validate_utility_binary(Path::new(NFT), "nft")?;
        let route_mark = selected_route_mark(request, || {
            validate_utility_binary(Path::new(MWAN3), "mwan3")?;
            resolve_mwan3_mark_mask(request)
        })?;
        let (table, owner) = match socket_owner {
            NftSocketOwner::BackendUid(_) => (
                route_pin_table_name(&request.identity.job_id, worker_run_id)?,
                route_pin_owner(&request.identity.job_id, worker_run_id)?,
            ),
            NftSocketOwner::ProbeRootGid(_) => {
                probe_pin_identity(&request.identity.job_id, worker_run_id)?
            }
        };
        cleanup_named_route_pin(&table, &owner)?;

        install_owned_route_pin_with(
            &table,
            &owner,
            socket_owner,
            route_mark,
            &request.route.l3_device,
            |arguments, input| {
                let spec = SpawnSpec {
                    program: PathBuf::from(NFT),
                    arguments: arguments.iter().map(OsString::from).collect(),
                    environment: Vec::new(),
                };
                let output = run_bounded_command_output_with_input(
                    &spec,
                    Some(input),
                    ROUTE_COMMAND_TIMEOUT,
                    COMMAND_OUTPUT_LIMIT,
                    || false,
                    |_| {},
                )
                .map_err(route_command_error)?;
                Ok(output.status.success())
            },
        )?;
        let pin = Self {
            cleanup_on_drop: true,
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
        if self.cleanup_on_drop {
            let _ = self.release();
        }
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

    // Sum of the route-window debits enforced against the budget, reported
    // for every terminal state including byte-limit and other failures.
    let debited = std::cell::Cell::new(0_u64);
    let terminal = match rating::wait_for_permit(
        &permit_path,
        &request.identity.job_id,
        &worker_run_id,
        request.deadline_unix_ms,
        terminate,
    ) {
        Ok(true) => match run_speedtest(
            &request,
            &worker_run_id,
            terminate,
            &terminal_path,
            &debited,
        ) {
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
            .encode_debited(&request.identity.job_id, &worker_run_id, debited.get())?
            .as_bytes(),
    )
}

fn run_speedtest(
    request: &OperationRequest,
    worker_run_id: &str,
    terminate: &AtomicBool,
    terminal_path: &Path,
    debited: &std::cell::Cell<u64>,
) -> Result<SpeedtestTerminal, String> {
    if request.backend != "speedtest-go" {
        return Err("speedtest-backend-unsupported".to_string());
    }
    let direction = request
        .speedtest_direction
        .ok_or_else(|| "speedtest-direction-missing".to_string())?;
    match request.speedtest_topology {
        Some(super::protocol::SpeedtestTopology::Current) => run_embedded_speedtest_debited(
            request,
            worker_run_id,
            direction,
            terminate,
            terminal_path,
            debited,
        ),
        Some(super::protocol::SpeedtestTopology::Unshaped) => run_unshaped_speedtest(
            request,
            worker_run_id,
            direction,
            terminate,
            terminal_path,
            debited,
        ),
        None => Err("speedtest-topology-missing".to_string()),
    }
}

fn run_unshaped_speedtest(
    request: &OperationRequest,
    worker_run_id: &str,
    direction: SpeedtestDirection,
    terminate: &AtomicBool,
    scratch_path: &Path,
    debited: &std::cell::Cell<u64>,
) -> Result<SpeedtestTerminal, String> {
    if request.target_state == OperationTargetState::AbsentBootstrap {
        let backend = OpenWrtNativeApplyBackend::new();
        return run_bootstrap_unshaped_with_absence(
            || backend.capture_bootstrap_runtime_baseline(request, worker_run_id),
            || {
                run_embedded_speedtest_debited(
                    request,
                    worker_run_id,
                    direction,
                    terminate,
                    scratch_path,
                    debited,
                )
            },
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
    super::full_autotune::wait_for_runtime_applied(None, &store, &permit, &control, terminate)?;

    let measurement = run_embedded_speedtest_debited(
        request,
        worker_run_id,
        direction,
        terminate,
        scratch_path,
        debited,
    );
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

fn run_embedded_speedtest_debited(
    request: &OperationRequest,
    worker_run_id: &str,
    direction: SpeedtestDirection,
    terminate: &AtomicBool,
    scratch_path: &Path,
    debited: &std::cell::Cell<u64>,
) -> Result<SpeedtestTerminal, String> {
    run_embedded_speedtest_with_load_sample_and_debit(
        request,
        worker_run_id,
        direction,
        terminate,
        scratch_path,
        &mut |debit| {
            debited.set(
                debited
                    .get()
                    .saturating_add(debit.rx_bytes)
                    .saturating_add(debit.tx_bytes),
            );
            Ok(())
        },
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
    run_embedded_speedtest_with_load_sample_and_debit(
        request,
        worker_run_id,
        direction,
        terminate,
        scratch_path,
        &mut |_| Ok(()),
    )
}

fn run_embedded_speedtest_with_load_sample_and_debit(
    request: &OperationRequest,
    worker_run_id: &str,
    direction: SpeedtestDirection,
    terminate: &AtomicBool,
    scratch_path: &Path,
    on_traffic_debit: &mut dyn FnMut(SpeedtestTrafficDebit) -> Result<(), String>,
) -> Result<(SpeedtestTerminal, Option<SpeedtestLoadSample>), String> {
    let mut remaining_traffic_budget = request.traffic_budget;
    let traffic_safety_reserve_bytes = standalone_speedtest_stop_reserve_bytes(request, direction)?;
    with_embedded_speedtest_session(request, worker_run_id, terminate, scratch_path, |session| {
        run_embedded_speedtest_with_load_sample_in_session_and_debit(
            session,
            request,
            worker_run_id,
            direction,
            traffic_safety_reserve_bytes,
            &mut remaining_traffic_budget,
            terminate,
            scratch_path,
            on_traffic_debit,
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
    remaining_traffic_budget: &mut super::protocol::TrafficPolicy,
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
    remaining_traffic_budget: &mut super::protocol::TrafficPolicy,
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
    let result = retry_budgeted_session_measurement_with_debit(
        session,
        request,
        remaining_traffic_budget,
        minimum_attempt_budget,
        || session.owned_traffic_snapshot(),
        |budget| {
            let mut bounded_request = request.clone();
            bounded_request.traffic_budget = budget
                .checked_sub(traffic_safety_reserve_bytes)
                .ok_or_else(|| SPEEDTEST_TRAFFIC_BUDGET_EXHAUSTED.to_string())?;
            let initial_route = wait_for_route_ready(request, terminate)?;
            let outcome = run_speedtest_with_pin(
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
                false,
                session.owned_budget_watch(traffic_safety_reserve_bytes)?,
            );
            // Preserve the aggregate debit in the enclosing retry transaction,
            // but never classify an accounting fault as a slow/broken server.
            if outcome.is_err() {
                session.route_pin.traffic_counters()?;
            }
            if let Some(pin) = session.probe_pin.as_ref() {
                pin.traffic_counters()?;
            }
            if let (Some(expected), Ok((SpeedtestTerminal::Complete(_), sample))) =
                (session.selected_endpoint_sha256.as_deref(), &outcome)
            {
                attest_selected_endpoint(
                    expected,
                    sample
                        .as_ref()
                        .and_then(|sample| sample.endpoint_sha256.as_deref()),
                )?;
            }
            outcome
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

/// Compare and validate speedtest-go servers for the complete
/// native Auto-Tune job. Repeated interleaved comparisons prevent the first
/// plausible slow server from winning by list order. They are preliminary
/// under the current topology, not proof of unshaped capacity. Validation rejects the
/// known speedtest-go failure mode where upload payload/rate is reported more
/// than once even though the selected interface counters prove otherwise.
pub(crate) fn qualify_embedded_speedtest_server_in_session(
    session: &mut EmbeddedSpeedtestSession,
    request: &OperationRequest,
    worker_run_id: &str,
    accounting: &SpeedtestAccountingPlan,
    attest_accounting_epoch: &mut dyn FnMut() -> Result<(), String>,
    traffic_safety_reserve_bytes: u64,
    remaining_traffic_budget: &mut super::protocol::TrafficPolicy,
    terminate: &AtomicBool,
    scratch_path: &Path,
    on_traffic_debit: &mut dyn FnMut(SpeedtestTrafficDebit) -> Result<(), String>,
    on_comparison: &mut dyn FnMut(super::server_qualification::Comparison) -> Result<(), String>,
) -> Result<Option<u64>, String> {
    if !session.matches(request, worker_run_id) {
        return Err("speedtest-session-identity-mismatch".to_string());
    }
    if session.server_qualified {
        return Ok(session.selected_server_id);
    }
    let debit_count = std::cell::Cell::new(0u32);
    let mut record_debit = |debit: SpeedtestTrafficDebit| {
        let lifecycle = debit.lifecycle;
        on_traffic_debit(debit)?;
        if lifecycle {
            return Ok(());
        }
        debit_count.set(
            debit_count
                .get()
                .checked_add(1)
                .ok_or("qualification debit count overflow")?,
        );
        Ok(())
    };
    let started_boot_ms = monotonic_boot_ms()?;
    let started = Instant::now();
    let mut comparisons = Vec::with_capacity(super::server_qualification::MAX_COMPARISONS);
    let listed = retry_budgeted_session_measurement_with_debit(
        session,
        request,
        remaining_traffic_budget,
        traffic_safety_reserve_bytes
            .checked_add(1)
            .unwrap_or(u64::MAX),
        || session.owned_traffic_snapshot(),
        |budget| {
            let mut bounded_request = request.clone();
            bounded_request.traffic_budget = budget
                .checked_sub(traffic_safety_reserve_bytes)
                .ok_or_else(|| SPEEDTEST_TRAFFIC_BUDGET_EXHAUSTED.to_string())?;
            let outcome = run_speedtest_go_server_list_attempt(
                &bounded_request,
                terminate,
                &scratch_path.with_extension("server-list"),
                session.credentials,
                session.owned_budget_watch(traffic_safety_reserve_bytes)?,
            );
            session.route_pin.traffic_counters()?;
            if let Some(pin) = session.probe_pin.as_ref() {
                pin.traffic_counters()?;
            }
            outcome
        },
        &mut record_debit,
    );
    let discovery = super::server_qualification::Comparison {
        index: 0,
        candidate_id: None,
        server_id: None,
        server_name: String::new(),
        server_sponsor: String::new(),
        endpoint_host: None,
        endpoint_sha256: None,
        display_metadata_truncated: false,
        started_boot_ms,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        debit_offset: 0,
        debit_count: debit_count.get(),
        download_kbps: None,
        upload_kbps: None,
        valid: false,
        code: if listed.is_ok() {
            "server-list-complete"
        } else {
            "server-list-failed"
        }
        .into(),
    };
    comparisons.push(discovery.clone());
    on_comparison(discovery)?;
    let candidates = listed?;

    let mut attempts = Vec::with_capacity(super::server_qualification::MAX_SERVERS);
    if let Some(id) = request.speedtest_server_id {
        attempts.push(Some(id));
    }
    for id in candidates {
        if !attempts.contains(&Some(id)) {
            attempts.push(Some(id));
        }
        if attempts.len() == super::server_qualification::MAX_SERVERS {
            break;
        }
    }
    let selected = qualify_server_candidates(
        request.speedtest_server_id,
        accounting.download.is_none() && accounting.upload.is_none(),
        &attempts,
        &mut comparisons,
        |index, candidate| {
            let started_boot_ms = monotonic_boot_ms()?;
            let started = Instant::now();
            let debit_offset = debit_count.get();
            let outcome = retry_budgeted_session_measurement_with_debit(
                session,
                request,
                remaining_traffic_budget,
                minimum_attempt_budget(SpeedtestDirection::Both, traffic_safety_reserve_bytes),
                || session.owned_traffic_snapshot(),
                |budget| {
                    let mut bounded_request = request.clone();
                    bounded_request.traffic_budget = budget
                        .checked_sub(traffic_safety_reserve_bytes)
                        .ok_or_else(|| SPEEDTEST_TRAFFIC_BUDGET_EXHAUSTED.to_string())?;
                    let outcome = run_speedtest_with_pin(
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
                        true,
                        session.owned_budget_watch(traffic_safety_reserve_bytes)?,
                    );
                    if outcome.is_err() {
                        session.route_pin.traffic_counters()?;
                    }
                    if let Some(pin) = session.probe_pin.as_ref() {
                        pin.traffic_counters()?;
                    }
                    outcome
                },
                &mut record_debit,
            );
            Ok(ServerQualificationAttempt {
                outcome,
                started_boot_ms,
                elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                debit_offset,
                debit_count: debit_count.get() - debit_offset,
            })
        },
        on_comparison,
    )?;
    session.selected_server_id = Some(selected.server_id);
    session.selected_endpoint_sha256 = Some(selected.endpoint_sha256);
    session.qualified_raw_capacity = selected.raw_capacity;
    session.server_qualified = true;
    Ok(Some(selected.server_id))
}

struct ServerQualificationAttempt {
    outcome: Result<(SpeedtestTerminal, Option<SpeedtestLoadSample>), String>,
    started_boot_ms: u64,
    elapsed_ms: u64,
    debit_offset: u32,
    debit_count: u32,
}

struct ServerQualificationSelection {
    server_id: u64,
    endpoint_sha256: String,
    raw_capacity: Option<super::server_qualification::QualifiedCapacity>,
}

// The scheduler and evidence checks are shared with deterministic fixtures.
// Production's callback still owns route attestation, retry debits and stop authority.
fn qualify_server_candidates(
    requested_server_id: Option<u64>,
    raw: bool,
    attempts: &[Option<u64>],
    comparisons: &mut Vec<super::server_qualification::Comparison>,
    mut measure: impl FnMut(usize, Option<u64>) -> Result<ServerQualificationAttempt, String>,
    on_comparison: &mut dyn FnMut(super::server_qualification::Comparison) -> Result<(), String>,
) -> Result<ServerQualificationSelection, String> {
    if attempts.len() < 2 {
        return Err("server-comparison-insufficient-listed-candidates".into());
    }
    if attempts.len() > super::server_qualification::MAX_SERVERS {
        return Err("server-comparison-candidate-bound".into());
    }
    let mut observations =
        Vec::with_capacity(attempts.len() * super::server_qualification::REPEATS);
    let mut rejected_candidates = [false; super::server_qualification::MAX_SERVERS];
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
    for (batch_number, batch) in attempts
        .chunks(super::server_qualification::SERVERS_PER_BATCH)
        .enumerate()
    {
        // Healthy first-batch sources do not incur backup traffic. A failed
        // comparison may use one bounded backup batch under the same ledger.
        if batch_number > 0
            && super::server_qualification::select_independent(comparisons, requested_server_id)
                .is_ok()
        {
            break;
        }
        let batch_offset = batch_number * super::server_qualification::SERVERS_PER_BATCH;
        for round_index in 0..batch.len() * super::server_qualification::REPEATS {
            let index = batch_offset * super::server_qualification::REPEATS + round_index;
            let slot = batch_offset + round_index % batch.len();
            if rejected_candidates[slot] {
                continue;
            }
            let candidate = attempts[slot];
            let measured = measure(index, candidate)?;
            let attempt = measured.outcome;
            let mut comparison = super::server_qualification::Comparison {
                index: index + 1,
                candidate_id: candidate,
                server_id: None,
                server_name: String::new(),
                server_sponsor: String::new(),
                endpoint_host: None,
                endpoint_sha256: None,
                display_metadata_truncated: false,
                started_boot_ms: measured.started_boot_ms,
                elapsed_ms: measured.elapsed_ms,
                debit_offset: measured.debit_offset,
                debit_count: measured.debit_count,
                download_kbps: None,
                upload_kbps: None,
                valid: false,
                code: "attempt-failed".into(),
            };
            match &attempt {
                Ok((SpeedtestTerminal::Complete(result), sample)) => {
                    comparison.server_id = result.server_id;
                    comparison.server_name = result.server_name.clone();
                    comparison.server_sponsor = result.server_sponsor.clone();
                    comparison.endpoint_host = sample
                        .as_ref()
                        .and_then(|sample| sample.endpoint_host.clone());
                    comparison.endpoint_sha256 = sample
                        .as_ref()
                        .and_then(|sample| sample.endpoint_sha256.clone());
                    comparison.download_kbps = result.download_kbps;
                    comparison.upload_kbps = result.upload_kbps;
                    comparison.code = if let Some(sample) = sample {
                        if let Some(reason) = speedtest_qualification_rejection(result, sample) {
                            reason.code()
                        } else {
                            let (download, upload) = qualification_bounded_goodput(result, sample)?;
                            comparison.download_kbps = Some(download);
                            comparison.upload_kbps = Some(upload);
                            comparison.valid = true;
                            "valid-observation"
                        }
                    } else {
                        "measurement-evidence-missing"
                    }
                    .into();
                    if result.server_id.is_none_or(|id| id == 0) {
                        comparison.valid = false;
                        comparison.code = "server-id-missing".into();
                    }
                    if comparison.valid
                        && (comparison.endpoint_host.is_none()
                            || comparison.endpoint_sha256.is_none())
                    {
                        comparison.valid = false;
                        comparison.code = "server-comparison-endpoint-missing".into();
                    }
                    let provider = super::server_qualification::normalized_provider(
                        &comparison.server_sponsor,
                    );
                    if comparison.valid && provider.is_none() {
                        comparison.valid = false;
                        comparison.code = "server-comparison-provider-missing".into();
                    }
                    if comparison.valid
                        && comparisons.iter().any(|old| {
                            old.valid
                                && old.server_id == comparison.server_id
                                && (old.endpoint_host != comparison.endpoint_host
                                    || old.endpoint_sha256 != comparison.endpoint_sha256
                                    || super::server_qualification::normalized_provider(
                                        &old.server_sponsor,
                                    ) != provider)
                        })
                    {
                        comparison.valid = false;
                        comparison.code = "server-comparison-source-identity-changed".into();
                    }
                }
                Ok((SpeedtestTerminal::Cancelled, _)) => comparison.code = "cancelled".into(),
                Ok((SpeedtestTerminal::Failed { code }, _)) | Err(code) => {
                    comparison.code = if code.len() <= 128
                        && code.bytes().all(|byte| {
                            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                        }) {
                        code.clone()
                    } else {
                        retryable_server_rejection_code(code).into()
                    };
                }
            }
            let accepted = comparison.valid;
            let rejection_code = comparison.code.clone();
            let accepted_rates = (comparison.download_kbps, comparison.upload_kbps);
            comparisons.push(comparison.clone());
            on_comparison(comparison)?;
            match attempt {
                Ok((SpeedtestTerminal::Complete(result), Some(sample))) => {
                    let Some(selected) = result.server_id.filter(|value| *value > 0) else {
                        eprintln!(
                        "speedtest-qualification-attempt-failed attempt={} code=speedtest-server-id-missing",
                        index + 1
                    );
                        record_rejection("server-id-missing");
                        rejected_candidates[slot] = true;
                        rejected += 1;
                        continue;
                    };
                    if let Some(reason) = speedtest_qualification_rejection(&result, &sample) {
                        eprintln!(
                            "{}",
                            qualification_rejection_log_line(index + 1, &result, &sample, reason)
                        );
                        record_rejection(reason.code());
                        rejected_candidates[slot] = true;
                        rejected += 1;
                    } else if !accepted {
                        record_rejection(&rejection_code);
                        rejected_candidates[slot] = true;
                        rejected += 1;
                    } else {
                        let observation = super::server_qualification::Observation {
                            server_id: selected,
                            download_kbps: accepted_rates
                                .0
                                .ok_or("qualification download missing")?,
                            upload_kbps: accepted_rates.1.ok_or("qualification upload missing")?,
                        };
                        eprintln!("speedtest-qualification-observed attempt={} server_id={} dl_kbps={} ul_kbps={}",
                        index + 1, selected, observation.download_kbps, observation.upload_kbps);
                        observations.push(observation);
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
                    rejected_candidates[slot] = true;
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
                    rejected_candidates[slot] = true;
                    rejected += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }
    if !observations.is_empty() {
        let selected =
            super::server_qualification::select_independent(comparisons, requested_server_id)?;
        let endpoint_sha256 = comparisons
            .iter()
            .find(|row| row.valid && row.server_id == Some(selected))
            .and_then(|row| row.endpoint_sha256.clone())
            .ok_or("server-comparison-endpoint-missing")?;
        let raw_capacity = if raw {
            Some(
                super::server_qualification::QualifiedCapacity::from_selected(
                    &observations,
                    selected,
                )?,
            )
        } else {
            None
        };
        eprintln!(
            "speedtest-qualification-selected server_id={selected} policy={}",
            super::server_qualification::SOURCE_POLICY
        );
        return Ok(ServerQualificationSelection {
            server_id: selected,
            endpoint_sha256,
            raw_capacity,
        });
    }
    let reason = if mixed_rejections {
        "mixed"
    } else {
        common_rejection.as_deref().unwrap_or("unknown")
    };
    Err(format!("speedtest-qualification-{reason}-after-{rejected}"))
}

fn speedtest_go_server_list_arguments(request: &OperationRequest) -> Result<Vec<String>, String> {
    let source = request
        .route
        .source_ip
        .filter(|value| matches!(value, IpAddr::V4(_)))
        .ok_or_else(|| "speedtest-source-ipv4-required".to_string())?;
    let mut arguments = vec![
        "--list".to_string(),
        "--ping-mode".to_string(),
        "http".to_string(),
        "--source".to_string(),
        source.to_string(),
    ];
    append_backend_dns_arguments(request, &mut arguments)?;
    Ok(arguments)
}

fn run_speedtest_go_server_list_attempt(
    request: &OperationRequest,
    terminate: &AtomicBool,
    scratch_path: &Path,
    credentials: BackendCredentials,
    mut budget_watch: Option<OwnedBudgetWatch<'_>>,
) -> Result<Vec<u64>, String> {
    let arguments = speedtest_go_server_list_arguments(request)?;
    let initial_route = wait_for_route_ready(request, terminate)?;
    let counters_before = interface_counters(&request.route.l3_device)?;
    let (mut output_guard, stdout, stderr) = BackendOutputGuard::create(scratch_path)?;
    if budget_watch
        .as_mut()
        .map(OwnedBudgetWatch::poll)
        .transpose()?
        .unwrap_or(false)
    {
        return Err(SPEEDTEST_TRAFFIC_LIMIT_REACHED.into());
    }
    let owned_budget = budget_watch.is_some();
    let child = BackendChild::spawn(&arguments, stdout, stderr, credentials)?;
    let deadline = Instant::now() + MAX_SERVER_LIST_RUNTIME;
    let success = with_backend_supervision(
        child,
        budget_watch.map(|mut watch| move || watch.poll()),
        terminate,
        deadline,
        "speedtest-server-list-timeout",
        |child| {
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
                if !owned_budget {
                    let counters = interface_counters(&request.route.l3_device)?;
                    let deltas = counter_deltas(counters_before, counters)?;
                    if request
                        .traffic_budget
                        .exceeded(deltas.0.saturating_add(deltas.1))
                    {
                        child.stop_and_reap()?;
                        return Err(SPEEDTEST_TRAFFIC_LIMIT_REACHED.to_string());
                    }
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
            child.finish()
        },
    )?;
    if !success {
        return Ok(Vec::new());
    }
    if attest_route(request)?.identity != initial_route.identity {
        return Err("speedtest-route-drift".to_string());
    }
    let output = read_bounded_file(&mut output_guard.output, OUTPUT_LIMIT)?;
    speedtest_go_server_candidates(&output, request.speedtest_server_id)
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
    remaining_traffic_budget: &mut super::protocol::TrafficPolicy,
    minimum_attempt_budget: u64,
    counters: Counters,
    attempt: Attempt,
) -> Result<T, String>
where
    Counters: FnMut() -> Result<(u64, u64), String>,
    Attempt: FnMut(super::protocol::TrafficPolicy) -> Result<T, String>,
{
    retry_budgeted_route_measurement_on_loss_with_debit(
        remaining_traffic_budget,
        minimum_attempt_budget,
        counters,
        attempt,
        &mut |_| Ok(()),
    )
}

fn retry_budgeted_session_measurement_with_debit<T>(
    session: &EmbeddedSpeedtestSession,
    request: &OperationRequest,
    remaining: &mut super::protocol::TrafficPolicy,
    minimum: u64,
    mut owned_counters: impl FnMut() -> Result<OwnedTrafficSnapshot, String>,
    mut attempt: impl FnMut(super::protocol::TrafficPolicy) -> Result<T, String>,
    on_debit: &mut dyn FnMut(SpeedtestTrafficDebit) -> Result<(), String>,
) -> Result<T, String> {
    let Some(slot) = &session.owned_ledger else {
        return retry_budgeted_route_measurement_on_loss_with_debit(
            remaining,
            minimum,
            || interface_counters(&request.route.l3_device),
            attempt,
            on_debit,
        );
    };
    let mut unproved = 0_u8;
    let mut read_owned = |previous: OwnedTrafficSnapshot| {
        let result = owned_counters().and_then(|observed| {
            observed.total_bytes()?;
            observed.delta_since(previous)?;
            Ok(observed)
        });
        if result.is_err() {
            let mut failed = slot.get();
            failed.persistence_uncertain = true;
            slot.set(failed);
        }
        result
    };
    loop {
        let ledger = slot.get();
        ledger.remaining()?;
        let before = read_owned(ledger.committed.counters)?;
        let available = ledger
            .authority
            .checked_sub(before.total_bytes()?)
            .unwrap_or(0_u64.into());
        if !available.allows(minimum) {
            let overrun = session.commit_owned_snapshot(before, true, remaining, on_debit)?;
            return Err(if overrun {
                "speedtest-traffic-budget-exceeded"
            } else {
                SPEEDTEST_TRAFFIC_BUDGET_EXHAUSTED
            }
            .into());
        }
        let outcome = attempt(available);
        if outcome.as_ref().err().is_some_and(|error| {
            error.starts_with("speedtest-owned-budget-observation-failed:")
                || error == "speedtest-budget-watcher-panicked"
        }) {
            let mut failed = slot.get();
            failed.persistence_uncertain = true;
            slot.set(failed);
            return outcome;
        }
        let after = read_owned(before)?;
        if session.commit_owned_snapshot(after, false, remaining, on_debit)? {
            return Err("speedtest-traffic-budget-exceeded".into());
        }
        match outcome {
            Err(error) if error == "speedtest-route-not-ready" => continue,
            Err(error) if error == "speedtest-route-traffic-unproved" => {
                unproved = unproved.checked_add(1).ok_or_else(|| error.clone())?;
                if unproved < MAX_UNPROVED_TRAFFIC_ATTEMPTS {
                    continue;
                }
                return Err(error);
            }
            result => return result,
        }
    }
}

fn retry_budgeted_route_measurement_on_loss_with_debit<T, Counters, Attempt>(
    remaining_traffic_budget: &mut super::protocol::TrafficPolicy,
    minimum_attempt_budget: u64,
    mut counters: Counters,
    mut attempt: Attempt,
    on_traffic_debit: &mut dyn FnMut(SpeedtestTrafficDebit) -> Result<(), String>,
) -> Result<T, String>
where
    Counters: FnMut() -> Result<(u64, u64), String>,
    Attempt: FnMut(super::protocol::TrafficPolicy) -> Result<T, String>,
{
    let mut unproved_traffic_attempts = 0_u8;
    loop {
        if !remaining_traffic_budget.allows(minimum_attempt_budget) {
            return Err(SPEEDTEST_TRAFFIC_BUDGET_EXHAUSTED.to_string());
        }
        let before = counters()?;
        let outcome = attempt(*remaining_traffic_budget);
        let after = counters()?;
        let (debit, overrun) = debit_traffic_budget(remaining_traffic_budget, before, after)?;
        // A refused first packet can fail without spending any bytes. Keep
        // its actual error; an empty debit is not measurement evidence.
        if debit.rx_bytes != 0 || debit.tx_bytes != 0 {
            on_traffic_debit(debit)?;
        }
        if overrun {
            return Err("speedtest-traffic-budget-exceeded".to_string());
        }
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

fn speedtest_go_server_candidates(
    output: &str,
    requested: Option<u64>,
) -> Result<Vec<u64>, String> {
    // Listing RTT is only a discovery ordering hint. Prefer different named
    // providers before spending the bounded comparison budget on aliases of
    // one provider. Names do not prove endpoint/AS independence or capacity.
    let mut candidates = Vec::<(u64, u64, Option<String>)>::new();
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
        let provider = details.rsplit_once(" by ").and_then(|(_, provider)| {
            let normalized = provider
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
            (!normalized.is_empty() && normalized.len() <= 256).then_some(normalized)
        });
        candidates.push((latency_micros, id, provider));
    }
    candidates.sort_unstable();
    if let Some(index) = candidates
        .iter()
        .position(|(_, id, _)| Some(*id) == requested)
    {
        // The explicit server represents its provider first; otherwise another
        // alias can consume one of the two remaining comparison slots.
        let explicit = candidates.remove(index);
        candidates.insert(0, explicit);
    }
    let mut server_ids = Vec::with_capacity(candidates.len());
    let mut providers = Vec::with_capacity(super::server_qualification::MAX_SERVERS);
    for (_, server_id, provider) in &candidates {
        if let Some(provider) = provider {
            if !server_ids.contains(server_id) && !providers.contains(&provider.as_str()) {
                server_ids.push(*server_id);
                providers.push(provider.as_str());
                if providers.len() == super::server_qualification::MAX_SERVERS {
                    break;
                }
            }
        }
    }
    // Retain unknown/duplicate-provider candidates as fallbacks, not as a
    // fabricated independence guarantee. Qualification still measures them.
    for (_, server_id, _) in &candidates {
        if !server_ids.contains(server_id) {
            server_ids.push(*server_id);
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

fn qualification_bounded_goodput(
    result: &SpeedtestResult,
    sample: &SpeedtestLoadSample,
) -> Result<(u64, u64), String> {
    Ok((
        bounded_achieved_kbps(
            result
                .download_kbps
                .ok_or("qualification download missing")?,
            sample.controlled_rx_payload_bytes,
            sample.controlled_rx_wire_bytes,
            sample
                .download_elapsed_ms
                .ok_or("qualification download duration missing")?,
        )?,
        bounded_achieved_kbps(
            result.upload_kbps.ok_or("qualification upload missing")?,
            sample.controlled_tx_payload_bytes,
            sample.controlled_tx_wire_bytes,
            sample
                .upload_elapsed_ms
                .ok_or("qualification upload duration missing")?,
        )?,
    ))
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
    remaining: &mut super::protocol::TrafficPolicy,
    before: (u64, u64),
    after: (u64, u64),
) -> Result<(SpeedtestTrafficDebit, bool), String> {
    let deltas = counter_deltas(before, after)?;
    let consumed = deltas
        .0
        .checked_add(deltas.1)
        .ok_or_else(|| "speedtest-traffic-budget-overflow".to_string())?;
    let overrun = remaining.exceeded(consumed);
    // The remaining authority is exhausted, but the observed debit must not
    // be clipped to that authority: persistence records the actual overrun.
    *remaining = remaining.checked_sub(consumed).unwrap_or(0_u64.into());
    Ok((
        SpeedtestTrafficDebit {
            lifecycle: false,
            rx_bytes: deltas.0,
            tx_bytes: deltas.1,
        },
        overrun,
    ))
}

struct QualificationCpuObserver {
    previous: crate::CpuSnapshot,
    busy_streak: Vec<u8>,
    maximum_percent: f64,
}

impl QualificationCpuObserver {
    fn new(maximum_percent: f64) -> Result<Self, String> {
        let previous =
            crate::read_cpu_snapshot().map_err(|_| "speedtest-cpu-evidence-unavailable")?;
        if previous.counters.len() < 2 || previous.counters.len() != previous.raw_lines.len() {
            return Err("speedtest-cpu-evidence-unavailable".into());
        }
        let busy_streak = vec![0; previous.counters.len() - 1];
        Ok(Self {
            previous,
            busy_streak,
            maximum_percent,
        })
    }

    fn observe(&mut self, current: crate::CpuSnapshot) -> Result<bool, String> {
        if current.counters.len() != self.previous.counters.len()
            || current.raw_lines.len() != self.previous.raw_lines.len()
            || current
                .raw_lines
                .iter()
                .zip(&self.previous.raw_lines)
                .any(|(now, old)| now.split_whitespace().next() != old.split_whitespace().next())
        {
            return Err("speedtest-cpu-evidence-changed".into());
        }
        let mut pressure = false;
        for (index, (now, old)) in current
            .counters
            .iter()
            .zip(&self.previous.counters)
            .enumerate()
            .skip(1)
        {
            let total = now
                .total
                .checked_sub(old.total)
                .ok_or("speedtest-cpu-evidence-reset")?;
            let idle = now
                .idle
                .checked_sub(old.idle)
                .filter(|idle| *idle <= total)
                .ok_or("speedtest-cpu-evidence-reset")?;
            if total == 0 {
                continue;
            } // No ticks do not prove an idle core.
            let busy = (total - idle) as f64 * 100.0 / total as f64;
            self.busy_streak[index - 1] = if busy > self.maximum_percent {
                self.busy_streak[index - 1].saturating_add(1)
            } else {
                0
            };
            pressure |= self.busy_streak[index - 1] >= QUALIFICATION_CPU_PRESSURE_SAMPLES;
        }
        self.previous = current;
        Ok(pressure)
    }
}

fn qualification_cpu_warning(
    observer: &mut Option<QualificationCpuObserver>,
    snapshot: Result<crate::CpuSnapshot, String>,
) -> Option<String> {
    let cpu = observer.as_mut()?;
    let result = snapshot.and_then(|snapshot| cpu.observe(snapshot));
    let warning = match result {
        Ok(false) => return None,
        Ok(true) => "sustained-core-pressure".to_string(),
        Err(error) => error,
    };
    // CPU is advisory. One bounded diagnostic cannot invalidate the source,
    // retry it as a bad server, or affect candidate/Apply eligibility.
    *observer = None;
    Some(warning)
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
    qualifying: bool,
    mut budget_watch: Option<OwnedBudgetWatch<'_>>,
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
    let mut cpu = if qualifying {
        let limit = request
            .profile
            .unwrap_or(crate::autotune::AutotuneProfile::BestOverall)
            .validation_thresholds()
            .cpu_max_percent;
        match QualificationCpuObserver::new(limit) {
            Ok(observer) => Some(observer),
            Err(error) => {
                eprintln!("speedtest-qualification-cpu advisory=true code={error}");
                None
            }
        }
    } else {
        None
    };
    if budget_watch
        .as_mut()
        .map(OwnedBudgetWatch::poll)
        .transpose()?
        .unwrap_or(false)
    {
        return Err(SPEEDTEST_TRAFFIC_LIMIT_REACHED.into());
    }
    let owned_budget = budget_watch.is_some();
    let child = BackendChild::spawn(&arguments, stdout, stderr, credentials)?;
    let success = with_backend_supervision(
        child,
        budget_watch.map(|mut watch| move || watch.poll()),
        terminate,
        deadline,
        "speedtest-timeout",
        |child| {
            let mut next_route_check = Instant::now() + ROUTE_RECHECK_INTERVAL;
            loop {
                if terminate.load(Ordering::Relaxed) {
                    child.stop_and_reap()?;
                    return Ok(false);
                }
                if child.try_wait()? {
                    break;
                }
                if Instant::now() >= deadline {
                    child.stop_and_reap()?;
                    return Err("speedtest-timeout".to_string());
                }
                if !owned_budget {
                    let counters = interface_counters(&request.route.l3_device)?;
                    let deltas = counter_deltas(counters_before, counters)?;
                    if request
                        .traffic_budget
                        .exceeded(deltas.0.saturating_add(deltas.1))
                    {
                        child.stop_and_reap()?;
                        return Err(SPEEDTEST_TRAFFIC_LIMIT_REACHED.to_string());
                    }
                }
                if cpu.is_some() {
                    let snapshot = crate::read_cpu_snapshot()
                        .map_err(|_| "speedtest-cpu-evidence-unavailable".to_string());
                    if let Some(warning) = qualification_cpu_warning(&mut cpu, snapshot) {
                        eprintln!("speedtest-qualification-cpu advisory=true code={warning}");
                    }
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
            child.finish()
        },
    )?;
    if terminate.load(Ordering::Relaxed) {
        return Ok((SpeedtestTerminal::Cancelled, None));
    }
    if !success {
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
    if request
        .traffic_budget
        .exceeded(deltas.0.saturating_add(deltas.1))
    {
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
    let endpoint = speedtest_server_endpoint(
        &output,
        parsed.result.server_id,
        &parsed.result.server_sponsor,
    )?;
    let load_sample = SpeedtestLoadSample {
        endpoint_host: endpoint.as_ref().map(|value| value.0.clone()),
        endpoint_sha256: endpoint.map(|value| value.1),
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

fn speedtest_server_endpoint(
    output: &str,
    expected_id: Option<u64>,
    expected_provider: &str,
) -> Result<Option<(String, String)>, String> {
    let line = output
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with('{'))
        .ok_or("speedtest-json-missing")?;
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|_| "speedtest-json-invalid")?;
    let servers = value["servers"]
        .as_array()
        .filter(|servers| servers.len() == 1)
        .ok_or("speedtest-server-result-ambiguous")?;
    let server = &servers[0];
    let id = server["id"]
        .as_u64()
        .or_else(|| server["id"].as_str().and_then(|id| id.parse::<u64>().ok()));
    if id.is_none_or(|id| id == 0)
        || id != expected_id
        || server["sponsor"].as_str().unwrap_or_default() != expected_provider
    {
        return Err("speedtest-server-identity-mismatch".into());
    }
    match server.get("url") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => super::server_qualification::endpoint_identity(
            value.as_str().ok_or("speedtest-server-endpoint-invalid")?,
        )
        .map(Some),
    }
}

fn attest_selected_endpoint(expected: &str, actual: Option<&str>) -> Result<(), String> {
    if actual != Some(expected) {
        return Err("speedtest-server-endpoint-changed".into());
    }
    Ok(())
}

fn append_backend_dns_arguments(
    request: &OperationRequest,
    arguments: &mut Vec<String>,
) -> Result<(), String> {
    if request.route.mode == OperationRouteMode::Explicit {
        request.route.validate()?;
        let server = request
            .route
            .dns_server
            .ok_or("speedtest-explicit-dns-required")?;
        // An old backend rejects this unknown option before starting discovery.
        // Never retry explicit operations using the system DNS endpoint.
        arguments.push("--route-dns-ipv4".into());
        arguments.push(server.to_string());
    } else {
        if request.route.dns_server.is_some() {
            return Err("speedtest-explicit-dns-on-legacy-route".into());
        }
        arguments.push("--dns-bind-source".into());
    }
    Ok(())
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
    append_backend_dns_arguments(request, &mut arguments)?;
    Ok(arguments)
}

fn selected_route_mark(
    request: &OperationRequest,
    resolve_mwan3: impl FnOnce() -> Result<u32, String>,
) -> Result<Option<(u32, u32)>, String> {
    match request.route.mode {
        OperationRouteMode::Main => Ok(None),
        OperationRouteMode::Mwan3 => {
            let fwmark = request
                .route
                .fwmark
                .ok_or("speedtest-mwan3-fwmark-missing")?;
            let mask = resolve_mwan3()?;
            if mask == 0 || fwmark == 0 || fwmark & !mask != 0 {
                return Err("speedtest-mwan3-mark-outside-mask".into());
            }
            Ok(Some((!mask, fwmark)))
        }
        OperationRouteMode::Explicit => {
            super::autotune_request::explicit_operation_authority(&request.route)?;
            let mask = request
                .route
                .fwmark_mask
                .ok_or("explicit route mask missing")?;
            let mark = request.route.fwmark.ok_or("explicit route mark missing")?;
            Ok(Some((!mask, mark)))
        }
    }
}

fn attest_route(request: &OperationRequest) -> Result<RouteSnapshot, String> {
    if request.route.mode == OperationRouteMode::Explicit {
        let snapshot = super::runtime::inspect_operation_route(
            &request.route,
            &request.identity.target_interface,
            crate::routing::ExplicitRouteAuthority::observe_system,
        )?;
        route_matches_request(request, &snapshot.identity)?;
        return Ok(snapshot);
    }
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
        || actual.device_ifindex != request.route.device_ifindex
        || actual.member != request.route.mwan3_member.as_deref().unwrap_or("")
        || actual.device != request.route.l3_device
        || actual.source_ip
            != request
                .route
                .source_ip
                .map(|value| value.to_string())
                .unwrap_or_default()
        || !optional_route_number_matches(request.route.fwmark, &actual.fwmark)
        || request
            .route
            .fwmark_mask
            .is_some_and(|mask| actual.fwmark_mask != Some(mask))
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
    validate_mwan3_environment(request, &environment)
}

fn validate_mwan3_environment(
    request: &OperationRequest,
    environment: &str,
) -> Result<u32, String> {
    let device = unique_environment_value(environment, "DEVICE")?
        .ok_or_else(|| "speedtest-mwan3-device-missing".to_string())?;
    let source_ip = unique_environment_value(environment, "SRCIP")?
        .ok_or_else(|| "speedtest-mwan3-source-missing".to_string())?;
    let mark_mask = unique_environment_value(environment, "FWMARK")?
        .ok_or_else(|| "speedtest-mwan3-mask-missing".to_string())?;
    if device != request.route.l3_device
        || source_ip.parse::<IpAddr>().ok() != request.route.source_ip
    {
        return Err("speedtest-mwan3-environment-drift".to_string());
    }
    let mask = parse_hex_u32(mark_mask, "speedtest-mwan3-mask-invalid")?;
    if mask == 0
        || request
            .route
            .fwmark_mask
            .is_some_and(|expected| expected != mask)
    {
        return Err("speedtest-mwan3-environment-drift".to_string());
    }
    Ok(mask)
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

fn probe_pin_identity(job_id: &str, worker_run_id: &str) -> Result<(String, String), String> {
    if !canonical_hex_id(job_id) || !canonical_hex_id(worker_run_id) {
        return Err("speedtest-route-pin-identity-invalid".into());
    }
    Ok((
        format!("cake_pt_{}_{}", &job_id[..12], &worker_run_id[..12]),
        format!("cake-autorate-probes:{job_id}:{worker_run_id}"),
    ))
}

#[cfg(test)]
fn nft_route_pin_batch(
    table: &str,
    owner: &str,
    uid: u32,
    route_mark: Option<(u32, u32)>,
) -> String {
    nft_owned_route_pin_batch(table, owner, NftSocketOwner::BackendUid(uid), route_mark)
}

fn path_text(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| "speedtest-private-path-invalid".to_string())
}

fn run_bounded_command(program: &str, arguments: &[&str]) -> Result<(bool, Vec<u8>), String> {
    let spec = SpawnSpec {
        program: PathBuf::from(program),
        arguments: arguments.iter().map(OsString::from).collect(),
        environment: Vec::new(),
    };
    let output =
        run_bounded_command_output(&spec, ROUTE_COMMAND_TIMEOUT, COMMAND_OUTPUT_LIMIT, || false)
            .map_err(route_command_error)?;
    Ok((output.status.success(), output.stdout))
}

fn route_command_error(error: String) -> String {
    match error.as_str() {
        "bounded-command-timeout" => "speedtest-route-command-timeout".to_string(),
        "bounded-command-output-too-large" => {
            "speedtest-route-command-output-too-large".to_string()
        }
        _ => format!("speedtest-route-command-failed: {error}"),
    }
}

fn attest_named_route_pin(table: &str, owner: &str) -> Result<(), String> {
    let output = run_bounded_command(NFT, &nft_table_snapshot_arguments(table))?;
    if !output.0 {
        return Err("speedtest-route-pin-missing".to_string());
    }
    attest_route_pin_snapshot(&output.1, table, owner).map(|_| ())
}

fn cleanup_named_route_pin(table: &str, owner: &str) -> Result<(), String> {
    cleanup_named_route_pin_with(table, owner, |arguments| {
        run_bounded_command(NFT, arguments)
    })
}

pub(crate) struct OwnedCleanupContext<'a> {
    pub directory: &'a Path,
    pub request: &'a OperationRequest,
}

fn nft_owned_cutoff_batch(job: &str, worker: &str) -> Result<String, String> {
    let backend = route_pin_table_name(job, worker)?;
    let (probes, _) = probe_pin_identity(job, worker)?;
    let mut commands = Vec::with_capacity(4);
    for table in [&backend, &probes] {
        for chain in ["output", "input"] {
            commands.push(serde_json::json!({"flush":{"chain":{
                "family":"inet", "table":table, "name":chain
            }}}));
        }
    }
    Ok(serde_json::json!({"nftables":commands}).to_string())
}

fn freeze_owned_counters(directory: &Path, job: &str, worker: &str) -> Result<(), String> {
    use super::autotune_apply_runtime::{
        read_private_recovery_bounded, require_private_directory, sync_directory,
        write_new_private_file,
    };
    let batch = nft_owned_cutoff_batch(job, worker)?;
    require_private_directory(directory)?;
    let table = route_pin_table_name(job, worker)?;
    let owner = route_pin_owner(job, worker)?;
    let (probe_table, probe_owner) = probe_pin_identity(job, worker)?;
    attest_named_route_pin(&table, &owner)?;
    attest_named_route_pin(&probe_table, &probe_owner)?;
    let path = directory.join(format!("owned-traffic-cutoff-batch-{worker}"));
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            if read_private_recovery_bounded(&path, 4096, "owned cutoff batch")? != batch.as_bytes()
            {
                return Err("owned cutoff batch differs from exact reconstruction".into());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_new_private_file(&path, batch.as_bytes())?;
        }
        Err(error) => return Err(format!("unable to inspect owned cutoff batch: {error}")),
    }
    sync_directory(directory)?;
    // One nft transaction detaches accounting rules, retaining named counters.
    // Only private test chains are touched, after producer retirement. Packets
    // arriving after this boundary are outside the recorded test interval.
    if !run_bounded_command(NFT, &["-j", "-f", path_text(&path)?])?.0 {
        return Err("owned counter cutoff transaction failed".into());
    }
    Ok(())
}

pub(crate) fn cleanup_route_pin(
    job_id: &str,
    worker_run_id: &str,
    owned: Option<OwnedCleanupContext<'_>>,
) -> Result<(), String> {
    validate_utility_binary(Path::new(NFT), "nft")?;
    let table = route_pin_table_name(job_id, worker_run_id)?;
    let owner = route_pin_owner(job_id, worker_run_id)?;
    if let Some(context) = owned {
        if context.request.identity.job_id != job_id {
            return Err("owned cleanup request identity mismatch".into());
        }
        let directory = context.directory;
        preserve_cutoff_observation(
            context,
            worker_run_id,
            || freeze_owned_counters(directory, job_id, worker_run_id),
            || {
                read_owned_traffic_counters_with(
                    job_id,
                    worker_run_id,
                    Instant::now() + OWNED_BUDGET_READ_TIMEOUT,
                    |table, owner, deadline| {
                        NftRoutePin {
                            table: Some(table.to_string()),
                            owner: Some(owner.to_string()),
                            cleanup_on_drop: false,
                        }
                        .traffic_counters_before(deadline)
                    },
                )
            },
        )?;
    }
    cleanup_named_route_pin(&table, &owner)?;
    let (probe_table, probe_owner) = probe_pin_identity(job_id, worker_run_id)?;
    cleanup_named_route_pin(&probe_table, &probe_owner)
}

// This version is written only after the producer-retirement cutoff succeeds.
// An unavailable observation is explicit and must never be interpreted as zero.
fn encode_cutoff_observation(
    request: &OperationRequest,
    worker: &str,
    snapshot: Option<OwnedTrafficSnapshot>,
) -> Result<String, String> {
    route_pin_table_name(&request.identity.job_id, worker)?;
    if let Some(snapshot) = snapshot {
        snapshot.total_bytes()?;
    }
    let digest = super::autotune_apply::native_apply_sha256_hex(request.encode()?.as_bytes());
    let counters = snapshot.map(|s| {
        [
            s.backend.rx_bytes,
            s.backend.tx_bytes,
            s.probes.rx_bytes,
            s.probes.tx_bytes,
        ]
    });
    let payload = serde_json::json!({"schema_version":2, "purpose":"post-retirement-rule-cutoff-v1",
        "job_id":request.identity.job_id, "worker_run_id":worker,
        "request_sha256":digest, "counters":counters});
    let sha256 = super::autotune_apply::native_apply_sha256_hex(payload.to_string().as_bytes());
    Ok(format!(
        "{}\n",
        serde_json::json!({"payload":payload,"sha256":sha256})
    ))
}

fn read_cutoff_observation(
    directory: &Path,
    request: &OperationRequest,
    worker: &str,
) -> Result<Option<OwnedTrafficSnapshot>, String> {
    route_pin_table_name(&request.identity.job_id, worker)?;
    super::autotune_apply_runtime::require_private_directory(directory)?;
    let bytes = super::autotune_apply_runtime::read_private_recovery_bounded(
        &directory.join(format!("owned-traffic-cutoff-{worker}")),
        4096,
        "owned cutoff observation",
    )?;
    decode_cutoff_observation(&bytes, request, worker)
}

fn decode_cutoff_observation(
    bytes: &[u8],
    request: &OperationRequest,
    worker: &str,
) -> Result<Option<OwnedTrafficSnapshot>, String> {
    if bytes.len() > 4096 {
        return Err("owned cutoff observation is oversized".into());
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| "invalid owned cutoff observation")?;
    let counters = &value["payload"]["counters"];
    let snapshot = if counters.is_null() {
        None
    } else {
        let list = counters
            .as_array()
            .filter(|v| v.len() == 4)
            .ok_or("invalid owned cutoff counters")?;
        let mut values = [0_u64; 4];
        for (index, value) in list.iter().enumerate() {
            values[index] = value.as_u64().ok_or("invalid owned cutoff counter")?;
        }
        Some(OwnedTrafficSnapshot {
            backend: SpeedtestTrafficCounters {
                rx_bytes: values[0],
                tx_bytes: values[1],
            },
            probes: SpeedtestTrafficCounters {
                rx_bytes: values[2],
                tx_bytes: values[3],
            },
        })
    };
    if encode_cutoff_observation(request, worker, snapshot)?.as_bytes() != bytes {
        return Err("owned cutoff observation binding mismatch".into());
    }
    Ok(snapshot)
}

#[cfg(test)]
fn verify_owned_cutoff_consistency(
    directory: &Path,
    request: &OperationRequest,
    worker: &str,
) -> Result<u64, String> {
    // The journal owns the worker prefix; this immutable cutoff owns the tail.
    // Check every component before returning the cumulative (not additive) total.
    let observed = read_cutoff_observation(directory, request, worker)?
        .ok_or("owned cutoff traffic is unavailable")?;
    let bytes = super::autotune_apply_runtime::read_private_recovery_bounded(
        &directory.join(format!("owned-traffic-checkpoint-{worker}")),
        4096,
        "owned traffic checkpoint",
    )?;
    let input = std::str::from_utf8(&bytes).map_err(|_| "owned traffic checkpoint is not UTF-8")?;
    let digest = super::autotune_apply::native_apply_sha256_hex(request.encode()?.as_bytes());
    let checkpoint =
        OwnedTrafficCheckpoint::decode_bound(input, &request.identity.job_id, worker, &digest)?;
    observed.delta_since(checkpoint.counters)?;
    observed.total_bytes()
}

pub(crate) fn reconcile_owned_cutoff(
    directory: &Path,
    request: &OperationRequest,
    worker: &str,
    journal: (u32, u64, u64),
) -> Result<u64, String> {
    use super::autotune_apply_runtime::{read_private_recovery_bounded, require_private_directory};
    route_pin_table_name(&request.identity.job_id, worker)?;
    require_private_directory(directory)?;
    let path = directory.join(format!("owned-traffic-checkpoint-{worker}"));
    let digest = super::autotune_apply::native_apply_sha256_hex(request.encode()?.as_bytes());
    let read = |path: &Path| -> Result<OwnedTrafficCheckpoint, String> {
        let bytes = read_private_recovery_bounded(path, 4096, "owned reconciliation checkpoint")?;
        OwnedTrafficCheckpoint::decode_bound(
            std::str::from_utf8(&bytes).map_err(|_| "owned checkpoint is not UTF-8")?,
            &request.identity.job_id,
            worker,
            &digest,
        )
    };
    let current = read(&path)?;
    let mut pending = None;
    for extension in ["owned-intent", "owned-next"] {
        let staged = path.with_extension(extension);
        match fs::symlink_metadata(&staged) {
            Ok(_) => {
                let candidate = read(&staged)?;
                if pending.is_some_and(|previous| previous != candidate) {
                    return Err("owned pending checkpoints disagree".into());
                }
                if candidate != current {
                    if current.sequence.checked_add(1) != Some(candidate.sequence) {
                        return Err("owned pending checkpoint is not contiguous".into());
                    }
                    let delta = candidate.counters.delta_since(current.counters)?;
                    if delta.rx_bytes == 0 && delta.tx_bytes == 0 {
                        return Err("owned pending checkpoint has no new traffic".into());
                    }
                }
                pending = Some(candidate);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "unable to inspect owned reconciliation input: {error}"
                ))
            }
        }
    }
    let totals = |point: OwnedTrafficCheckpoint| -> Result<(u32, u64, u64), String> {
        Ok((
            point.sequence,
            point
                .counters
                .backend
                .rx_bytes
                .checked_add(point.counters.probes.rx_bytes)
                .ok_or("owned reconciliation RX overflow")?,
            point
                .counters
                .backend
                .tx_bytes
                .checked_add(point.counters.probes.tx_bytes)
                .ok_or("owned reconciliation TX overflow")?,
        ))
    };
    // The append either did not land, or landed exactly once. Neither state
    // authorizes replaying the append or modifying measurement evidence.
    if totals(current)? != journal && pending.map(totals).transpose()? != Some(journal) {
        return Err("owned checkpoint transaction does not match the durable debit journal".into());
    }
    let observed = read_cutoff_observation(directory, request, worker)?
        .ok_or("owned cutoff traffic is unavailable")?;
    observed.delta_since(pending.unwrap_or(current).counters)?;
    observed.total_bytes()
}

fn preserve_cutoff_observation(
    context: OwnedCleanupContext<'_>,
    worker: &str,
    freeze: impl FnOnce() -> Result<(), String>,
    observe: impl FnOnce() -> Result<OwnedTrafficSnapshot, String>,
) -> Result<(), String> {
    use super::autotune_apply_runtime::{
        read_private_recovery_bounded, require_private_directory, sync_directory,
        write_new_private_file,
    };
    route_pin_table_name(&context.request.identity.job_id, worker)?;
    require_private_directory(context.directory)?;
    let path = context
        .directory
        .join(format!("owned-traffic-cutoff-{worker}"));
    let staged = path.with_extension("cutoff-next");
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            read_cutoff_observation(context.directory, context.request, worker)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut recovered = false;
            match fs::symlink_metadata(&staged) {
                Ok(_) => {
                    let bytes =
                        read_private_recovery_bounded(&staged, 4096, "staged owned cutoff")?;
                    match serde_json::from_slice::<serde_json::Value>(&bytes) {
                        Err(error) if error.is_eof() => {
                            // Preserve a single demonstrably truncated write.
                            // Re-freezing the still-retained tables is idempotent.
                            let incomplete = path.with_extension("cutoff-incomplete");
                            match fs::symlink_metadata(&incomplete) {
                                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                                _ => {
                                    return Err(
                                        "owned cutoff already has an incomplete write to inspect"
                                            .into(),
                                    )
                                }
                            }
                            fs::rename(&staged, &incomplete).map_err(|error| {
                                format!("unable to retain partial cutoff: {error}")
                            })?;
                            sync_directory(context.directory)?;
                        }
                        _ => {
                            decode_cutoff_observation(&bytes, context.request, worker)?;
                            recovered = true;
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("unable to inspect staged cutoff: {error}")),
            }
            // Failure to prepare/execute the cutoff must retain live counters.
            // It must not become a nullable observation followed by deletion.
            if !recovered {
                freeze()?;
                let snapshot = observe().ok();
                write_new_private_file(
                    &staged,
                    encode_cutoff_observation(context.request, worker, snapshot)?.as_bytes(),
                )?;
            }
            // A recovered complete write may have crashed before fsync.
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&staged)
                .and_then(|file| file.sync_all())
                .map_err(|error| format!("unable to sync staged cutoff: {error}"))?;
            fs::rename(&staged, &path)
                .map_err(|error| format!("unable to publish owned cutoff: {error}"))?;
        }
        Err(error) => {
            return Err(format!(
                "unable to inspect owned cutoff observation: {error}"
            ))
        }
    }
    sync_directory(context.directory)
}

// Both identities are validated before I/O. Absence of either table is an
// error, never a zero measurement; all four components share one deadline.
fn read_owned_traffic_counters_with(
    job_id: &str,
    worker_run_id: &str,
    deadline: Instant,
    mut read: impl FnMut(&str, &str, Instant) -> Result<SpeedtestTrafficCounters, String>,
) -> Result<OwnedTrafficSnapshot, String> {
    let table = route_pin_table_name(job_id, worker_run_id)?;
    let owner = route_pin_owner(job_id, worker_run_id)?;
    let (probe_table, probe_owner) = probe_pin_identity(job_id, worker_run_id)?;
    let snapshot = OwnedTrafficSnapshot {
        backend: read(&table, &owner, deadline)?,
        probes: read(&probe_table, &probe_owner, deadline)?,
    };
    snapshot.total_bytes()?;
    Ok(snapshot)
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
    read_live_named_traffic_counters_with(&table, &owner, |arguments| {
        run_bounded_accounting_command(
            arguments,
            accounting_time_remaining(deadline)?,
            &should_cancel,
        )
    })
}

fn read_live_named_traffic_counters_with(
    table: &str,
    owner: &str,
    mut run: impl FnMut(&[&str]) -> Result<(bool, Vec<u8>), String>,
) -> Result<Option<SpeedtestTrafficCounters>, String> {
    let listed = run(&nft_table_snapshot_arguments(table))?;
    if !listed.0 {
        let tables = run(&["-j", "list", "tables"])?;
        if tables.0 && nft_table_snapshot_proves_absence(&tables.1, table)? {
            return Ok(None);
        }
        return Err("speedtest-accounting-inspection-failed".to_string());
    }
    let json =
        String::from_utf8(listed.1).map_err(|_| "speedtest-accounting-json-invalid".to_string())?;
    parse_named_traffic_counters(&json, table, owner).map(Some)
}

fn read_named_traffic_counters(
    table: &str,
    owner: &str,
) -> Result<Option<SpeedtestTrafficCounters>, String> {
    let listed = run_bounded_accounting_command(
        &nft_table_snapshot_arguments(table),
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
    let flow_owner = format!("{owner}{FLOW_ACCOUNTING_OWNER_SUFFIX}");
    let (rx_bytes, counter_owner) =
        match named_counter_bytes(json, table, ACCOUNTING_RX_COUNTER, &flow_owner) {
            Ok(Some(bytes)) => (bytes, flow_owner.as_str()),
            Err(error) if error == "speedtest-accounting-identity-mismatch" => (
                named_counter_bytes(json, table, ACCOUNTING_RX_COUNTER, owner)?
                    .ok_or_else(|| "speedtest-accounting-rx-missing".to_string())?,
                owner,
            ),
            Ok(None) => return Err("speedtest-accounting-rx-missing".into()),
            Err(error) => return Err(error),
        };
    let tx_bytes = named_counter_bytes(json, table, ACCOUNTING_TX_COUNTER, counter_owner)?
        .ok_or_else(|| "speedtest-accounting-tx-missing".to_string())?;
    if counter_owner == flow_owner {
        let faults = named_counter_bytes(json, table, ACCOUNTING_FAULT_COUNTER, counter_owner)?
            .ok_or_else(|| "speedtest-accounting-flow-fault-counter-missing".to_string())?;
        if faults != 0 {
            return Err("speedtest-accounting-flow-registration-failed".into());
        }
    }
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
    // Go JSON escapes '&' as \u0026 and may emit surrogate pairs. Decode a
    // bounded string value with the existing JSON parser, retaining the old
    // 256-byte decoded limit (worst-case ASCII escaping needs 6 chars/byte).
    let mut end = tail.len().min(6 * 256 + 2);
    while !tail.is_char_boundary(end) {
        end -= 1;
    }
    let value = serde_json::Deserializer::from_str(&tail[..end])
        .into_iter::<String>()
        .next()
        .ok_or("speedtest-json-string-invalid")?
        .map_err(|_| "speedtest-json-string-invalid")?;
    if value.len() > 256 {
        return Err("speedtest-json-string-invalid".into());
    }
    Ok(Some(value))
}

fn json_value_tail<'a>(input: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let tail = input.split_once(&needle)?.1;
    let tail = tail.trim_start();
    tail.strip_prefix(':').map(str::trim_start)
}

impl SpeedtestTerminal {
    /// Version 2 appends the summed route-window debits of this worker, the
    /// same measure the traffic budget enforces. Version 1 remains readable.
    pub(crate) fn encode_debited(
        &self,
        job_id: &str,
        worker_run_id: &str,
        debited_bytes: u64,
    ) -> Result<String, String> {
        let v1 = self.encode(job_id, worker_run_id)?;
        let body = v1
            .strip_prefix("cake-autorate-speedtest-terminal\t1\n")
            .and_then(|body| body.strip_suffix("\n\n"))
            .ok_or_else(|| "speedtest-terminal-encode-invalid".to_string())?;
        Ok(format!(
            "cake-autorate-speedtest-terminal\t2\n{body}\ndebited_bytes={debited_bytes}\n\n"
        ))
    }

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
        let version = match lines.next() {
            Some("cake-autorate-speedtest-terminal\t1") => 1,
            Some("cake-autorate-speedtest-terminal\t2") => 2,
            _ => return Err("speedtest-terminal-header-invalid".to_string()),
        };
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
        let debited_bytes = if version == 2 {
            Some(terminal_u64(&mut lines, "debited_bytes")?)
        } else {
            None
        };
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
            debited_bytes,
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

    #[test]
    fn r6_export_egress_guard_for_isolated_kernel_test() {
        use super::*;
        let Some(path) = std::env::var_os("CAKE_R6_EGRESS_EXPORT") else {
            return;
        };
        let socket_owner = match std::env::var("CAKE_R6_EGRESS_OWNER").as_deref() {
            Ok("backend") => NftSocketOwner::BackendUid(1234),
            Ok("probe") => NftSocketOwner::ProbeRootGid(42),
            Err(std::env::VarError::NotPresent) => NftSocketOwner::BackendUid(0),
            other => panic!("unsupported isolated egress fixture owner: {other:?}"),
        };
        let route_mark = match std::env::var("CAKE_R6_EGRESS_MARK").as_deref() {
            Ok("0x200/0x3f00") => Some((!0x3f00, 0x200)),
            Err(std::env::VarError::NotPresent) => None,
            other => panic!("unsupported isolated egress fixture mark: {other:?}"),
        };
        let base =
            nft_owned_route_pin_batch("cake_r6_guard", "isolated-test", socket_owner, route_mark);
        let batch =
            nft_egress_guard_batch(&base, "cake_r6_guard", socket_owner, "cake_test").unwrap();
        super::super::autotune_apply_runtime::write_new_private_file(
            Path::new(&path),
            batch.as_bytes(),
        )
        .unwrap();
    }

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
                dns_server: None,
                device_ifindex: None,
                mode: OperationRouteMode::Main,
                mwan3_member: None,
                l3_device: "pppoe-wan".to_string(),
                source_ip: Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))),
                fwmark: None,
                routing_table: None,
                fwmark_mask: None,
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
            traffic_budget: crate::operations::protocol::TrafficPolicy::Capped {
                max_bytes: 4_000_000_000,
            },
            traffic_policy_explicit: false,
            traffic_plan: None,
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
            dns_server: None,
            probe_accounting_required: false,
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
    fn t2_standalone_reserve_applies_only_to_explicit_capped_speedtest() {
        let mut operation = request(SpeedtestDirection::Both);
        operation.traffic_policy_explicit = false;
        operation.service_dl_cap_kbps = None;
        operation.service_ul_cap_kbps = None;
        // Historical derived budgets already included their reserve.
        assert_eq!(
            standalone_speedtest_stop_reserve_bytes(&operation, SpeedtestDirection::Both),
            Ok(0)
        );
        operation.traffic_policy_explicit = true;
        operation.traffic_budget = super::super::protocol::TrafficPolicy::Unlimited;
        assert_eq!(
            standalone_speedtest_stop_reserve_bytes(&operation, SpeedtestDirection::Both),
            Ok(0)
        );
        operation.traffic_budget = 10_000_000_000_u64.into();
        assert_eq!(
            standalone_speedtest_stop_reserve_bytes(&operation, SpeedtestDirection::Both)
                .unwrap_err(),
            "traffic-stop-authority-unavailable"
        );
        operation.service_dl_cap_kbps = Some(85_000);
        assert_eq!(
            standalone_speedtest_stop_reserve_bytes(&operation, SpeedtestDirection::Download),
            Ok(traffic_stop_safety_reserve_bytes(85_000, 0))
        );
        assert!(
            standalone_speedtest_stop_reserve_bytes(&operation, SpeedtestDirection::Both).is_err()
        );
        operation.service_ul_cap_kbps = Some(10_000);
        assert_eq!(
            standalone_speedtest_stop_reserve_bytes(&operation, SpeedtestDirection::Both),
            Ok(traffic_stop_safety_reserve_bytes(85_000, 10_000))
        );
        assert_eq!(
            minimum_explicit_speedtest_traffic_budget_bytes(
                SpeedtestDirection::Both,
                Some(85_000),
                Some(10_000)
            ),
            Ok(minimum_speedtest_traffic_budget_bytes(
                SpeedtestDirection::Both,
                85_000,
                10_000
            ))
        );
        operation.identity.operation = OperationKind::AutomaticRating;
        assert_eq!(
            standalone_speedtest_stop_reserve_bytes(&operation, SpeedtestDirection::Both),
            Ok(0)
        );
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
    fn r6_explicit_backend_dns_is_required_for_discovery_and_load() {
        let mut operation = request(SpeedtestDirection::Both);
        assert_eq!(
            speedtest_go_server_list_arguments(&operation).unwrap(),
            [
                "--list",
                "--ping-mode",
                "http",
                "--source",
                "192.0.2.1",
                "--dns-bind-source"
            ]
        );
        operation.route.mode = OperationRouteMode::Explicit;
        operation.route.device_ifindex = Some(42);
        operation.route.routing_table = Some(101);
        operation.route.fwmark = Some(0x100);
        operation.route.fwmark_mask = Some(0x3f00);
        assert!(speedtest_go_server_list_arguments(&operation).is_err());
        assert!(speedtest_go_arguments(&operation, SpeedtestDirection::Both, None).is_err());
        operation.route.dns_server = Some("192.0.2.53".parse().unwrap());
        for args in [
            speedtest_go_server_list_arguments(&operation).unwrap(),
            speedtest_go_arguments(&operation, SpeedtestDirection::Both, None).unwrap(),
            speedtest_go_arguments(&operation, SpeedtestDirection::Download, Some(123)).unwrap(),
            speedtest_go_arguments(&operation, SpeedtestDirection::Upload, Some(123)).unwrap(),
        ] {
            assert!(args.ends_with(&["--route-dns-ipv4".into(), "192.0.2.53".into()]));
            assert!(!args.iter().any(|arg| arg == "--dns-bind-source"));
        }
        for invalid in ["0.0.0.0", "127.0.0.1", "169.254.1.1", "224.0.0.1"] {
            operation.route.dns_server = Some(invalid.parse().unwrap());
            assert!(speedtest_go_server_list_arguments(&operation).is_err());
            assert!(speedtest_go_arguments(&operation, SpeedtestDirection::Both, None).is_err());
        }
        operation.route.dns_server = Some("192.0.2.53".parse().unwrap());
        operation.route.mode = OperationRouteMode::Main;
        assert!(speedtest_go_server_list_arguments(&operation).is_err());
        assert!(speedtest_go_arguments(&operation, SpeedtestDirection::Both, None).is_err());
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
            speedtest_go_server_candidates(output, None).unwrap(),
            vec![17372, 35793, 29062]
        );
        assert!(speedtest_go_server_candidates("[oops] 1km 2ms invalid", None).is_err());
        assert!(speedtest_go_server_candidates("[13397] 1km unavailable", None).is_err());
    }

    #[test]
    fn t1_backend_provider_unicode_escapes_are_decoded_with_the_original_bound() {
        assert_eq!(
            json_string(r#"{"sponsor":"AT\u0026T \ud83c\udf10"}"#, "sponsor").unwrap(),
            Some("AT&T 🌐".into())
        );
        assert_eq!(
            json_string(
                &format!("{{\"sponsor\":\"{}\"}}", "\\u0061".repeat(256)),
                "sponsor"
            )
            .unwrap(),
            Some("a".repeat(256))
        );
        for invalid in [
            format!("{{\"sponsor\":\"{}\"}}", "a".repeat(257)),
            r#"{"sponsor":"\ud800"}"#.into(),
            r#"{"sponsor":null}"#.into(),
        ] {
            assert!(json_string(&invalid, "sponsor").is_err());
        }
    }

    #[test]
    fn t1_endpoint_metadata_is_bound_to_the_only_returned_server_and_never_echoed() {
        let output = "progress\n{\"servers\":[{\"id\":\"11\",\"sponsor\":\"Provider\",\"url\":\"https://Server.Example.:8080/upload.php?secret=value\"}]}";
        assert_eq!(
            speedtest_server_endpoint(output, Some(11), "Provider")
                .unwrap()
                .map(|value| value.0),
            Some("server.example".into())
        );
        assert!(speedtest_server_endpoint(output, Some(12), "Provider").is_err());
        assert!(speedtest_server_endpoint(output, Some(11), "Other").is_err());
        assert!(
            speedtest_server_endpoint("{\"servers\":[{\"id\":11},{\"id\":12}]}", Some(11), "")
                .is_err()
        );
        assert_eq!(
            speedtest_server_endpoint("{\"servers\":[{\"id\":11}]}", Some(11), "").unwrap(),
            None
        );
        let error = speedtest_server_endpoint(
            "{\"servers\":[{\"id\":11,\"url\":\"http://private:secret@example.test/\"}]}",
            Some(11),
            "",
        )
        .unwrap_err();
        assert_eq!(error, "speedtest-server-endpoint-invalid");
        assert!(!error.contains("secret"));
        assert!(attest_selected_endpoint("one.example", Some("one.example")).is_ok());
        assert_eq!(
            attest_selected_endpoint("one.example", Some("two.example")).unwrap_err(),
            "speedtest-server-endpoint-changed"
        );
        assert!(attest_selected_endpoint("one.example", None).is_err());
    }

    #[test]
    fn t1_discovery_prioritizes_distinct_providers_without_fabricating_independence() {
        let output = concat!(
            "[11] 1km 1ms City by Example ISP\n",
            "[12] 1km 2ms City by   EXAMPLE   ISP  \n",
            "[13] 1km 3ms City by Example ISP\n",
            "[14] 2km 4ms City by Second ISP\n",
            "[15] 3km 5ms City by Third ISP\n",
            "[16] 4km 6ms City by Fourth ISP\n",
        );
        let candidates = speedtest_go_server_candidates(output, None).unwrap();
        assert_eq!(&candidates[..3], &[11, 14, 15]);
        assert_eq!(candidates, vec![11, 14, 15, 16, 12, 13]);
        assert_eq!(
            &speedtest_go_server_candidates(output, Some(13)).unwrap()[..3],
            &[13, 14, 15]
        );
        assert_eq!(
            speedtest_go_server_candidates(output, Some(99)).unwrap(),
            candidates
        );
        assert_eq!(
            speedtest_go_server_candidates("[11] 1km 1ms City\n[12] 1km 2ms City\n", None).unwrap(),
            vec![11, 12]
        );
        assert_eq!(
            speedtest_go_server_candidates(
                "[11] 1km 1ms City by Same\n[12] 1km 2ms City by Same\n",
                None
            )
            .unwrap(),
            vec![11, 12]
        );
    }

    #[test]
    fn t2_unlimited_retry_accounts_more_than_32gb_without_bypassing_cancel_or_deadline() {
        use super::super::protocol::TrafficPolicy;
        for terminal in ["speedtest-cancelled", "speedtest-timeout"] {
            let counters = Cell::new((0u64, 0u64));
            let calls = Cell::new(0u32);
            let charged = Cell::new(0u64);
            let mut budget = TrafficPolicy::Unlimited;
            let result = retry_budgeted_route_measurement_on_loss_with_debit::<(), _, _>(
                &mut budget,
                32_000_000_000,
                || Ok(counters.get()),
                |grant| {
                    assert_eq!(grant, TrafficPolicy::Unlimited);
                    calls.set(calls.get() + 1);
                    let (rx, tx) = counters.get();
                    counters.set((rx + 10_000_000_000, tx + 10_000_000_000));
                    Err(if calls.get() == 1 {
                        "speedtest-route-not-ready"
                    } else {
                        terminal
                    }
                    .into())
                },
                &mut |debit| {
                    charged.set(charged.get() + debit.rx_bytes + debit.tx_bytes);
                    Ok(())
                },
            );
            assert_eq!(result.unwrap_err(), terminal);
            assert_eq!(charged.get(), 40_000_000_000);
            assert_eq!(calls.get(), 2);
            assert_eq!(budget, TrafficPolicy::Unlimited);
        }
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
            endpoint_host: None,
            endpoint_sha256: None,
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
            endpoint_host: None,
            endpoint_sha256: None,
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
    fn t1_qualification_ranks_bounded_goodput_not_allowed_reported_inflation() {
        let result = SpeedtestResult {
            direction: SpeedtestDirection::Both,
            download_kbps: Some(1350),
            upload_kbps: Some(1350),
            rx_bytes: 0,
            tx_bytes: 0,
            elapsed_ms: 0,
            server_id: Some(1),
            server_name: "fixture".into(),
            server_sponsor: "fixture".into(),
        };
        let sample = SpeedtestLoadSample {
            endpoint_host: None,
            endpoint_sha256: None,
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
        assert!(speedtest_qualification_rejection(&result, &sample).is_none());
        let rates = qualification_bounded_goodput(&result, &sample).unwrap();
        assert_eq!(rates, (1000, 1000));
        let mut observations = Vec::new();
        for _ in 0..super::super::server_qualification::REPEATS {
            observations.push(super::super::server_qualification::Observation {
                server_id: 1,
                download_kbps: rates.0,
                upload_kbps: rates.1,
            });
            observations.push(super::super::server_qualification::Observation {
                server_id: 2,
                download_kbps: 1250,
                upload_kbps: 1250,
            });
        }
        assert_eq!(
            super::super::server_qualification::select(&observations, None).unwrap(),
            2
        );
        let bounded =
            super::super::server_qualification::QualifiedCapacity::from_selected(&observations, 1)
                .unwrap();
        bounded
            .attest_raw_rate(SpeedtestDirection::Download, 1000)
            .unwrap();
        let old_basis = super::super::server_qualification::QualifiedCapacity {
            download_kbps: 1350,
            upload_kbps: 1350,
        };
        assert!(old_basis
            .attest_raw_rate(SpeedtestDirection::Download, 1000)
            .is_err());
        let mut wire_limited = sample.clone();
        wire_limited.controlled_tx_wire_bytes = 1_100_000;
        assert_eq!(
            qualification_bounded_goodput(&result, &wire_limited)
                .unwrap()
                .1,
            880
        );
    }

    fn qualification_fixture_attempt(id: u64, debit_offset: u32) -> ServerQualificationAttempt {
        ServerQualificationAttempt {
            outcome: Ok((
                SpeedtestTerminal::Complete(SpeedtestResult {
                    direction: SpeedtestDirection::Both,
                    download_kbps: Some(1000),
                    upload_kbps: Some(1000),
                    rx_bytes: 1_400_000,
                    tx_bytes: 1_400_000,
                    elapsed_ms: 20_000,
                    server_id: Some(id),
                    server_name: format!("fixture-{id}"),
                    server_sponsor: format!("provider-{id}"),
                }),
                Some(SpeedtestLoadSample {
                    endpoint_host: Some(format!("server-{id}.example")),
                    endpoint_sha256: Some(format!("{id:064x}")),
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
                }),
            )),
            started_boot_ms: 1000 + u64::from(debit_offset) * 20_000,
            elapsed_ms: 20_000,
            debit_offset,
            debit_count: 1,
        }
    }

    #[test]
    fn t1_scheduler_uses_backup_only_after_primary_failure_and_skips_rejected_slots() {
        for (fallback, count) in [(false, 6), (true, 6), (true, 5)] {
            let attempts: Vec<_> = (1..=count).map(Some).collect();
            let mut comparisons = Vec::new();
            let mut calls = Vec::new();
            let mut published = Vec::new();
            let selected = qualify_server_candidates(
                None,
                true,
                &attempts,
                &mut comparisons,
                |index, candidate| {
                    let id = candidate.unwrap();
                    let mut measured = qualification_fixture_attempt(id, calls.len() as u32 + 1);
                    calls.push((index + 1, id));
                    if fallback && id <= 3 {
                        measured.outcome = Ok((
                            SpeedtestTerminal::Failed {
                                code: "speedtest-backend-failed".into(),
                            },
                            None,
                        ));
                    }
                    Ok(measured)
                },
                &mut |comparison| {
                    published.push(comparison.index);
                    Ok(())
                },
            )
            .unwrap();
            let expected_ids = if !fallback {
                vec![1, 2, 3, 1, 2, 3, 1, 2, 3]
            } else if count == 6 {
                vec![1, 2, 3, 4, 5, 6, 4, 5, 6, 4, 5, 6]
            } else {
                vec![1, 2, 3, 4, 5, 4, 5, 4, 5]
            };
            assert_eq!(
                calls.iter().map(|row| row.1).collect::<Vec<_>>(),
                expected_ids
            );
            assert_eq!(published, calls.iter().map(|row| row.0).collect::<Vec<_>>());
            if fallback {
                assert_eq!(
                    calls[3].0, 10,
                    "skipped primary slots retain stable evidence indices"
                );
                assert!(selected.server_id >= 4);
            } else {
                assert!(selected.server_id <= 3);
            }
            assert_eq!(
                selected.endpoint_sha256,
                format!("{:064x}", selected.server_id)
            );
            assert_eq!(selected.raw_capacity.unwrap().download_kbps, 1000);
            for (offset, row) in comparisons.iter().enumerate() {
                assert_eq!(row.debit_offset, offset as u32 + 1);
                assert_eq!(row.debit_count, 1);
                assert_eq!(row.valid, !fallback || row.candidate_id.unwrap() > 3);
            }
        }
    }

    #[test]
    fn t1_scheduler_does_not_replace_failed_pinned_server_or_promote_shaped_capacity() {
        for pinned in [None, Some(1)] {
            let mut comparisons = Vec::new();
            let mut calls = Vec::new();
            let outcome = qualify_server_candidates(
                pinned,
                false,
                &[Some(1), Some(2), Some(3), Some(4), Some(5), Some(6)],
                &mut comparisons,
                |_, candidate| {
                    let id = candidate.unwrap();
                    let mut attempt = qualification_fixture_attempt(id, calls.len() as u32);
                    calls.push(id);
                    if id == 1 {
                        attempt.outcome = Err("speedtest-backend-failed".into());
                    }
                    Ok(attempt)
                },
                &mut |_| Ok(()),
            );
            assert_eq!(calls.iter().filter(|id| **id == 1).count(), 1);
            if pinned.is_some() {
                assert!(
                    outcome.is_err(),
                    "healthy backups cannot replace explicit pin"
                );
            } else {
                let selected = outcome.unwrap();
                assert!(selected.server_id == 2 || selected.server_id == 3);
                assert!(selected.raw_capacity.is_none());
                assert!(calls.iter().all(|id| *id <= 3));
            }
        }
    }

    #[test]
    fn t1_scheduler_stops_on_budget_cancel_and_evidence_publication_failure() {
        for failure in ["budget", "cancel", "publication", "epoch"] {
            let mut calls = Vec::new();
            let mut comparisons = Vec::new();
            let mut published = 0;
            let error = qualify_server_candidates(
                None,
                true,
                &[Some(1), Some(2), Some(3), Some(4), Some(5), Some(6)],
                &mut comparisons,
                |_, candidate| {
                    let id = candidate.unwrap();
                    let mut attempt = qualification_fixture_attempt(id, calls.len() as u32);
                    calls.push(id);
                    if id <= 3 {
                        attempt.outcome = Err("speedtest-backend-failed".into());
                    }
                    if id == 4 {
                        match failure {
                            "budget" => {
                                attempt.outcome = Err(SPEEDTEST_TRAFFIC_BUDGET_EXHAUSTED.into())
                            }
                            "cancel" => attempt.outcome = Ok((SpeedtestTerminal::Cancelled, None)),
                            "epoch" => return Err("fixture-accounting-epoch-changed".into()),
                            _ => (),
                        }
                    }
                    Ok(attempt)
                },
                &mut |row| {
                    published += 1;
                    if failure == "publication" && row.candidate_id == Some(4) {
                        Err("fixture-report-write-failed".into())
                    } else {
                        Ok(())
                    }
                },
            )
            .err()
            .unwrap();
            assert_eq!(calls, [1, 2, 3, 4]);
            assert_eq!(published, if failure == "epoch" { 3 } else { 4 });
            assert_eq!(
                error,
                match failure {
                    "budget" => SPEEDTEST_TRAFFIC_BUDGET_EXHAUSTED,
                    "cancel" => "speedtest-server-qualification-cancelled",
                    "epoch" => "fixture-accounting-epoch-changed",
                    _ => "fixture-report-write-failed",
                }
            );
        }
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
            endpoint_host: None,
            endpoint_sha256: None,
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
            endpoint_host: None,
            endpoint_sha256: None,
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
            endpoint_host: None,
            endpoint_sha256: None,
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
            probe_pin: None,
            owned_checkpoint: None,
            owned_ledger: None,
            route_pin: NftRoutePin {
                cleanup_on_drop: true,
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
            requested_server_id: operation.speedtest_server_id,
            authorized_traffic_budget: operation.traffic_budget,
            traffic_policy_explicit: operation.traffic_policy_explicit,
            selected_server_id: None,
            server_qualified: false,
            qualified_raw_capacity: None,
            selected_endpoint_sha256: None,
        };
        assert!(session.matches(&operation, &worker_run_id));

        let mut next_bounded_run = operation.clone();
        next_bounded_run.traffic_budget =
            (next_bounded_run.traffic_budget.limit_bytes().unwrap() / 2).into();
        assert!(session.matches(&next_bounded_run, &worker_run_id));
        let mut enlarged = operation.clone();
        enlarged.traffic_budget = (operation.traffic_budget.limit_bytes().unwrap() + 1).into();
        assert!(!session.matches(&enlarged, &worker_run_id));
        enlarged.traffic_budget = super::super::protocol::TrafficPolicy::Unlimited;
        enlarged.traffic_policy_explicit = true;
        assert!(!session.matches(&enlarged, &worker_run_id));
        let mut changed_server = operation.clone();
        changed_server.speedtest_server_id = Some(999);
        assert!(!session.matches(&changed_server, &worker_run_id));

        let mut changed_route = operation.clone();
        changed_route.identity.route_fingerprint = "7".repeat(64);
        assert!(!session.matches(&changed_route, &worker_run_id));
        changed_route = operation.clone();
        changed_route.route.l3_device = "eth0".to_string();
        assert!(!session.matches(&changed_route, &worker_run_id));
        assert!(!session.matches(&operation, &"8".repeat(32)));
    }

    #[test]
    fn t1_cpu_warning_preserves_measurement_result_and_exact_debit() {
        let snapshot = |total| crate::CpuSnapshot {
            counters: vec![crate::CpuCounters { total, idle: 0 }; 2],
            raw_lines: vec!["cpu".into(), "cpu0".into()],
        };
        let observer = || {
            Some(QualificationCpuObserver {
                previous: snapshot(0),
                busy_streak: vec![0],
                maximum_percent: crate::autotune::AutotuneProfile::BestOverall
                    .validation_thresholds()
                    .cpu_max_percent,
            })
        };
        let mut remaining = super::super::protocol::TrafficPolicy::Capped { max_bytes: 10_000 };
        let reads = Cell::new(0);
        let attempts = Cell::new(0);
        let mut debits = Vec::new();
        let result = retry_budgeted_route_measurement_on_loss_with_debit(
            &mut remaining,
            1,
            || {
                let n = reads.get();
                reads.set(n + 1);
                Ok(if n == 0 { (100, 200) } else { (3100, 2200) })
            },
            |_| {
                attempts.set(attempts.get() + 1);
                let mut cpu = observer();
                assert!(qualification_cpu_warning(&mut cpu, Ok(snapshot(100))).is_none());
                assert!(qualification_cpu_warning(&mut cpu, Ok(snapshot(200))).is_none());
                assert_eq!(
                    qualification_cpu_warning(&mut cpu, Ok(snapshot(300))).as_deref(),
                    Some("sustained-core-pressure")
                );
                assert!(cpu.is_none());
                assert!(qualification_cpu_warning(&mut cpu, Ok(snapshot(400))).is_none());
                Ok(9)
            },
            &mut |debit| {
                debits.push(debit);
                Ok(())
            },
        );
        assert_eq!(result.unwrap(), 9);
        assert_eq!(attempts.get(), 1);
        assert_eq!(reads.get(), 2);
        assert_eq!(
            debits,
            vec![SpeedtestTrafficDebit {
                lifecycle: false,
                rx_bytes: 3000,
                tx_bytes: 2000
            }]
        );
        assert_eq!(remaining.limit_bytes(), Some(5000));
        let mut cpu = observer();
        assert_eq!(
            qualification_cpu_warning(&mut cpu, Err("speedtest-cpu-evidence-unavailable".into()))
                .as_deref(),
            Some("speedtest-cpu-evidence-unavailable")
        );
        assert!(cpu.is_none());
    }

    #[test]
    fn t1_cpu_pressure_is_sustained_per_core_not_host_average_or_one_spike() {
        fn snapshot(total: u64, idle0: u64, idle1: u64) -> crate::CpuSnapshot {
            crate::CpuSnapshot {
                counters: vec![
                    crate::CpuCounters {
                        total: total * 2,
                        idle: idle0 + idle1,
                    },
                    crate::CpuCounters { total, idle: idle0 },
                    crate::CpuCounters { total, idle: idle1 },
                ],
                raw_lines: vec!["cpu".into(), "cpu0".into(), "cpu1".into()],
            }
        }
        let fresh = || QualificationCpuObserver {
            previous: snapshot(0, 0, 0),
            busy_streak: vec![0; 2],
            maximum_percent: crate::autotune::AutotuneProfile::BestOverall
                .validation_thresholds()
                .cpu_max_percent,
        };
        let mut guard = fresh();
        assert!(!guard.observe(snapshot(100, 0, 100)).unwrap());
        assert!(!guard.observe(snapshot(200, 0, 200)).unwrap());
        assert!(
            guard.observe(snapshot(300, 0, 300)).unwrap(),
            "one saturated core is hidden by 50% host average"
        );
        let mut guard = fresh();
        for (total, idle) in [(100, 0), (200, 100), (300, 100), (400, 200)] {
            assert!(
                !guard.observe(snapshot(total, idle, total)).unwrap(),
                "isolated spikes must not reject a source"
            );
        }
        let mut guard = fresh();
        for n in 1..=4 {
            assert!(
                !guard.observe(snapshot(n * 100, n * 15, n * 100)).unwrap(),
                "exact 85% remains within the profile advisory threshold"
            );
        }
        let mut guard = fresh();
        guard.observe(snapshot(100, 0, 100)).unwrap();
        let prior = guard.busy_streak.clone();
        assert!(!guard.observe(snapshot(100, 0, 100)).unwrap());
        assert_eq!(
            guard.busy_streak, prior,
            "missing ticks are not idle evidence"
        );
        assert!(guard.observe(snapshot(99, 0, 100)).is_err());
        let mut changed = snapshot(200, 0, 200);
        changed.raw_lines[2] = "cpu2".into();
        assert!(
            guard.observe(changed).is_err(),
            "CPU identity changes cannot inherit a streak"
        );
    }

    #[test]
    fn t2_probe_counter_table_is_disjoint_from_backend_measurement_counters() {
        let job = "a".repeat(32);
        let run = "b".repeat(32);
        let (table, owner) = probe_pin_identity(&job, &run).unwrap();
        assert_eq!(table, "cake_pt_aaaaaaaaaaaa_bbbbbbbbbbbb");
        assert_ne!(table, route_pin_table_name(&job, &run).unwrap());
        assert_ne!(owner, route_pin_owner(&job, &run).unwrap());
        assert!(probe_pin_identity("bad", &run).is_err());
        let gid = 32770;
        let batch: serde_json::Value = serde_json::from_str(&nft_owned_route_pin_batch(
            &table,
            &owner,
            NftSocketOwner::ProbeRootGid(gid),
            Some((!0x3f00, 0x200)),
        ))
        .unwrap();
        let commands = batch["nftables"].as_array().unwrap();
        assert_eq!(commands[0]["create"]["table"]["comment"], owner);
        assert_eq!(commands[1]["add"]["set"]["size"], MAX_ACCOUNTING_FLOWS);
        let rules: Vec<_> = commands
            .iter()
            .filter_map(|command| command.get("add")?.get("rule"))
            .filter(|rule| !rule.to_string().contains("owned_dns6"))
            .collect();
        assert_eq!(rules.len(), 6);
        let matches_credentials =
            |rule: &serde_json::Value, uid: Option<u32>, group: Option<u32>| {
                rule["expr"].as_array().unwrap().iter().all(|expression| {
                    let condition = &expression["match"];
                    let value = match condition["left"]["meta"]["key"].as_str() {
                        Some("skuid") => uid,
                        Some("skgid") => group,
                        _ => return true,
                    };
                    let Some(value) = value else {
                        return false;
                    };
                    let expected = condition["right"].as_u64().unwrap();
                    match condition["op"].as_str().unwrap() {
                        "==" => u64::from(value) == expected,
                        "!=" => u64::from(value) != expected,
                        _ => panic!("unexpected credential matcher"),
                    }
                })
            };
        for (uid, group, owned) in [
            (0, gid, true),
            (32769, gid, false),
            (0, 0, false),
            (32769, 0, false),
        ] {
            assert_eq!(matches_credentials(rules[0], Some(uid), Some(group)), owned);
            assert_eq!(matches_credentials(rules[4], Some(uid), Some(group)), owned);
            let clears = matches_credentials(rules[1], Some(uid), Some(group))
                || matches_credentials(rules[2], Some(uid), Some(group));
            assert_eq!(
                clears, !owned,
                "foreign tuple must be cleared before transmit counting"
            );
        }
        for rule in &rules[..3] {
            assert!(
                !matches_credentials(rule, None, None),
                "late socketless packets must retain conntrack ownership"
            );
        }
        assert_eq!(
            rules[3]["expr"].as_array().unwrap().last().unwrap(),
            &serde_json::json!({"return":null})
        );
        assert_eq!(
            rules[4]["expr"].as_array().unwrap().last().unwrap(),
            &serde_json::json!({"drop":null})
        );
        assert!(
            rules[4]["expr"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["match"]["left"].get("ct").is_none()),
            "untracked/unsupported owned traffic must hit the fault counter"
        );
        assert!(
            rules[5]["expr"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["match"]["left"]
                    .get("meta")
                    .is_none_or(|m| m["key"] == "nfproto")),
            "reply accounting must not depend on a receive socket's UID/GID"
        );
    }

    #[test]
    fn t2_route_commands_have_live_output_and_runtime_bounds() {
        assert_eq!(
            run_bounded_command("/usr/bin/head", &["-c", "262145", "/dev/zero"]).unwrap_err(),
            "speedtest-route-command-output-too-large"
        );
        assert_eq!(
            run_bounded_command("/bin/sleep", &["30"]).unwrap_err(),
            "speedtest-route-command-timeout"
        );
    }

    #[test]
    fn t2_budget_watcher_stops_owned_pid_while_parent_does_not_poll_it() {
        fn sleeper() -> BackendChild {
            let child = Command::new("/bin/sleep")
                .arg("30")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let identity =
                ProcessIdentity::inspect(Path::new(DEFAULT_PROC_ROOT), child.id()).unwrap();
            BackendChild { child, identity }
        }
        let mut peer = sleeper();
        let child = sleeper();
        let identity = child.identity.clone();
        assert_eq!(peer.identity.process_group, identity.process_group);
        let (observed, notifications) = std::sync::mpsc::channel();
        let terminate = AtomicBool::new(false);
        let result = with_backend_supervision(
            child,
            Some(move || {
                let _ = observed.send(());
                Ok(true)
            }),
            &terminate,
            Instant::now() + Duration::from_secs(5),
            "fixture-timeout",
            |backend| {
                notifications.recv_timeout(Duration::from_secs(2)).unwrap();
                let deadline = Instant::now() + Duration::from_secs(2);
                while identity
                    .still_matches(Path::new(DEFAULT_PROC_ROOT))
                    .unwrap_or(false)
                    && Instant::now() < deadline
                {
                    thread::sleep(Duration::from_millis(10));
                }
                assert!(
                    !identity
                        .still_matches(Path::new(DEFAULT_PROC_ROOT))
                        .unwrap_or(false),
                    "watcher must stop and reap without a parent try_wait call"
                );
                assert!(
                    !peer.try_wait().unwrap(),
                    "shared process-group peer must survive"
                );
                backend.finish()
            },
        );
        assert_eq!(result.unwrap_err(), SPEEDTEST_TRAFFIC_LIMIT_REACHED);
        peer.stop_and_reap().unwrap();
        let failure = with_backend_supervision(
            sleeper(),
            Some(|| Err("fixture-counter-fault".into())),
            &terminate,
            Instant::now() + Duration::from_secs(5),
            "fixture-timeout",
            |backend| backend.finish(),
        )
        .unwrap_err();
        assert_eq!(failure, "fixture-counter-fault");
        let cancelled = AtomicBool::new(true);
        let cancellation = with_backend_supervision(
            sleeper(),
            Some(|| panic!("cancelled child must not poll")),
            &cancelled,
            Instant::now() + Duration::from_secs(5),
            "fixture-timeout",
            |backend| backend.finish(),
        )
        .unwrap_err();
        assert_eq!(cancellation, "speedtest-cancelled");
        let abandoned = sleeper();
        let abandoned_identity = abandoned.identity.clone();
        let error = with_backend_supervision::<(), _>(
            abandoned,
            Some(|| Ok(false)),
            &terminate,
            Instant::now() + Duration::from_secs(5),
            "fixture-timeout",
            |_| Err("parent-route-failure".into()),
        )
        .unwrap_err();
        assert_eq!(error, "parent-route-failure");
        assert!(!abandoned_identity
            .still_matches(Path::new(DEFAULT_PROC_ROOT))
            .unwrap_or(false));
        let timeout = with_backend_supervision(
            sleeper(),
            Some(|| panic!("expired backend must not poll")),
            &terminate,
            Instant::now(),
            "fixture-timeout",
            |backend| backend.finish(),
        )
        .unwrap_err();
        assert_eq!(timeout, "fixture-timeout");
    }

    #[test]
    fn t2_cutoff_staged_recovery_preserves_partial_and_rejects_foreign_bytes() {
        use super::super::autotune_apply_runtime::write_new_private_file;
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("cake-cutoff-stage-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let operation = request(SpeedtestDirection::Both);
        let context = || OwnedCleanupContext {
            directory: &root,
            request: &operation,
        };
        let snapshot = OwnedTrafficSnapshot {
            backend: SpeedtestTrafficCounters {
                rx_bytes: 70,
                tx_bytes: 30,
            },
            probes: SpeedtestTrafficCounters {
                rx_bytes: 7,
                tx_bytes: 3,
            },
        };
        let worker = "a".repeat(32);
        let path = root.join(format!("owned-traffic-cutoff-{worker}"));
        let staged = path.with_extension("cutoff-next");
        let bytes = encode_cutoff_observation(&operation, &worker, Some(snapshot)).unwrap();
        write_new_private_file(&staged, bytes.as_bytes()).unwrap();
        preserve_cutoff_observation(
            context(),
            &worker,
            || panic!("complete staged receipt must not refreeze"),
            || panic!("complete staged receipt must not resample"),
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), bytes);
        assert!(!staged.exists());
        assert_eq!(
            read_cutoff_observation(&root, &operation, &worker).unwrap(),
            Some(snapshot)
        );

        fs::remove_file(&path).unwrap();
        write_new_private_file(&staged, b"{\"payload\":").unwrap();
        let frozen = std::cell::Cell::new(false);
        preserve_cutoff_observation(
            context(),
            &worker,
            || {
                frozen.set(true);
                Ok(())
            },
            || {
                assert!(frozen.get());
                Ok(snapshot)
            },
        )
        .unwrap();
        assert_eq!(
            fs::read(path.with_extension("cutoff-incomplete")).unwrap(),
            b"{\"payload\":"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), bytes);

        fs::remove_file(&path).unwrap();
        let foreign =
            encode_cutoff_observation(&operation, &"b".repeat(32), Some(snapshot)).unwrap();
        write_new_private_file(&staged, foreign.as_bytes()).unwrap();
        assert!(preserve_cutoff_observation(
            context(),
            &worker,
            || panic!("foreign receipt cannot refreeze"),
            || panic!("foreign receipt cannot resample")
        )
        .is_err());
        assert_eq!(fs::read_to_string(&staged).unwrap(), foreign);
        assert!(!path.exists());
        fs::remove_file(&staged).unwrap();
        write_new_private_file(&staged, b"{").unwrap();
        assert!(preserve_cutoff_observation(
            context(),
            &worker,
            || panic!("second partial write must remain bounded"),
            || panic!()
        )
        .is_err());
        assert_eq!(fs::read(&staged).unwrap(), b"{");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn t2_cutoff_observation_is_bound_immutable_and_never_implies_final_zero() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("cake-cutoff-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let mut operation = request(SpeedtestDirection::Both);
        let worker = "a".repeat(32);
        let context = || OwnedCleanupContext {
            directory: &root,
            request: &operation,
        };
        let snapshot = OwnedTrafficSnapshot {
            backend: SpeedtestTrafficCounters {
                rx_bytes: 70,
                tx_bytes: 30,
            },
            probes: SpeedtestTrafficCounters {
                rx_bytes: 7,
                tx_bytes: 3,
            },
        };
        let failure = preserve_cutoff_observation(
            context(),
            &worker,
            || Err("cutoff-batch-write-failed".into()),
            || panic!("failed cutoff must not sample or delete counters"),
        )
        .unwrap_err();
        assert_eq!(failure, "cutoff-batch-write-failed");
        assert!(!root.join(format!("owned-traffic-cutoff-{worker}")).exists());
        preserve_cutoff_observation(context(), &worker, || Ok(()), || Ok(snapshot)).unwrap();
        let path = root.join(format!("owned-traffic-cutoff-{worker}"));
        let original = fs::read_to_string(&path).unwrap();
        assert!(original.contains("post-retirement-rule-cutoff-v1"));
        assert!(original.contains("\"schema_version\":2"));
        assert!(original.contains("[70,30,7,3]"));
        assert!(verify_owned_cutoff_consistency(&root, &operation, &worker).is_err());
        let digest = super::super::autotune_apply::native_apply_sha256_hex(
            operation.encode().unwrap().as_bytes(),
        );
        let checkpoint_path = root.join(format!("owned-traffic-checkpoint-{worker}"));
        let zero = SpeedtestTrafficCounters {
            rx_bytes: 0,
            tx_bytes: 0,
        };
        OwnedTrafficCheckpoint {
            sequence: 0,
            counters: OwnedTrafficSnapshot {
                backend: zero,
                probes: zero,
            },
        }
        .publish_bound(
            &checkpoint_path,
            &operation.identity.job_id,
            &worker,
            &digest,
        )
        .unwrap();
        assert_eq!(
            verify_owned_cutoff_consistency(&root, &operation, &worker).unwrap(),
            110
        );
        OwnedTrafficCheckpoint {
            sequence: 1,
            counters: snapshot,
        }
        .publish_bound(
            &checkpoint_path,
            &operation.identity.job_id,
            &worker,
            &digest,
        )
        .unwrap();
        assert_eq!(
            verify_owned_cutoff_consistency(&root, &operation, &worker).unwrap(),
            110
        );
        preserve_cutoff_observation(
            context(),
            &worker,
            || panic!("durable receipt must not refreeze"),
            || panic!("retry must preserve first observation"),
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        let missing = "b".repeat(32);
        preserve_cutoff_observation(
            context(),
            &missing,
            || Ok(()),
            || Err("missing table".into()),
        )
        .unwrap();
        let unavailable =
            fs::read_to_string(root.join(format!("owned-traffic-cutoff-{missing}"))).unwrap();
        assert!(unavailable.contains("\"counters\":null"));
        assert!(verify_owned_cutoff_consistency(&root, &operation, &missing).is_err());
        let mut swapped = snapshot;
        swapped.backend.rx_bytes -= 1;
        swapped.probes.rx_bytes += 1;
        assert_eq!(
            swapped.total_bytes().unwrap(),
            snapshot.total_bytes().unwrap()
        );
        fs::remove_file(&path).unwrap();
        preserve_cutoff_observation(context(), &worker, || Ok(()), || Ok(swapped)).unwrap();
        assert!(verify_owned_cutoff_consistency(&root, &operation, &worker).is_err());
        fs::remove_file(&path).unwrap();
        let mut tail = snapshot;
        tail.backend.rx_bytes += 11;
        tail.backend.tx_bytes += 5;
        tail.probes.rx_bytes += 2;
        tail.probes.tx_bytes += 1;
        preserve_cutoff_observation(context(), &worker, || Ok(()), || Ok(tail)).unwrap();
        for _ in 0..2 {
            assert_eq!(
                verify_owned_cutoff_consistency(&root, &operation, &worker).unwrap(),
                129,
                "cutoff is cumulative; retries must not add the tail twice"
            );
        }
        fs::remove_file(&path).unwrap();
        assert!(read_cutoff_observation(&root, &operation, &worker).is_err());
        assert!(
            !path.exists(),
            "verification must not create a missing observation"
        );
        preserve_cutoff_observation(context(), &worker, || Ok(()), || Ok(snapshot)).unwrap();
        operation.traffic_budget = 1_000_000_u64.into();
        assert!(preserve_cutoff_observation(
            OwnedCleanupContext {
                directory: &root,
                request: &operation
            },
            &worker,
            || panic!("wrong request must not refreeze"),
            || panic!("wrong request must not reobserve")
        )
        .is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn t2_owned_snapshot_reads_both_exact_owners_under_one_deadline() {
        let job = "a".repeat(32);
        let worker = "b".repeat(32);
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut calls = Vec::new();
        let snapshot =
            read_owned_traffic_counters_with(&job, &worker, deadline, |table, owner, until| {
                assert_eq!(until, deadline);
                calls.push((table.to_string(), owner.to_string()));
                Ok(SpeedtestTrafficCounters {
                    rx_bytes: 7,
                    tx_bytes: 3,
                })
            })
            .unwrap();
        assert_eq!(snapshot.total_bytes().unwrap(), 20);
        assert_eq!(
            calls,
            vec![
                (
                    route_pin_table_name(&job, &worker).unwrap(),
                    route_pin_owner(&job, &worker).unwrap()
                ),
                probe_pin_identity(&job, &worker).unwrap(),
            ]
        );
        for fail_at in [1, 2] {
            let mut count = 0;
            let error = read_owned_traffic_counters_with(&job, &worker, deadline, |_, _, _| {
                count += 1;
                if count == fail_at {
                    Err("missing-or-unowned-table".into())
                } else {
                    Ok(SpeedtestTrafficCounters {
                        rx_bytes: 7,
                        tx_bytes: 3,
                    })
                }
            })
            .unwrap_err();
            assert_eq!(error, "missing-or-unowned-table");
            assert_eq!(count, fail_at);
        }
        assert!(
            read_owned_traffic_counters_with("../bad", &worker, deadline, |_, _, _| panic!(
                "invalid identity reached I/O"
            ))
            .is_err()
        );
        assert!(
            read_owned_traffic_counters_with(&job, &worker, deadline, |_, _, _| Ok(
                SpeedtestTrafficCounters {
                    rx_bytes: u64::MAX,
                    tx_bytes: 1
                }
            ))
            .is_err()
        );
    }

    #[test]
    fn t2_owned_budget_watch_uses_cumulative_not_window_consumption() {
        let pin = NftRoutePin {
            table: None,
            owner: None,
            cleanup_on_drop: false,
        };
        let point = |rx, tx, prx, ptx| OwnedTrafficSnapshot {
            backend: SpeedtestTrafficCounters {
                rx_bytes: rx,
                tx_bytes: tx,
            },
            probes: SpeedtestTrafficCounters {
                rx_bytes: prx,
                tx_bytes: ptx,
            },
        };
        let mut watch = OwnedBudgetWatch {
            backend: &pin,
            probes: &pin,
            limit: 100_u64.into(),
            previous: point(50, 10, 5, 5),
        };
        assert!(!watch.observe(point(60, 15, 10, 15)).unwrap());
        assert!(watch.observe(point(61, 15, 10, 15)).unwrap());
        assert!(watch.observe(point(60, 15, 100, 100)).is_err());
        watch.limit = super::super::protocol::TrafficPolicy::Unlimited;
        assert!(!watch.observe(point(70_000_000_000, 20, 100, 100)).unwrap());
    }

    #[test]
    fn t2_scheduler_backup_keeps_one_owned_rx_tx_budget_and_durable_debits() {
        use std::cell::Cell;
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("cake-backup-budget-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let mut operation = request(SpeedtestDirection::Both);
        operation.traffic_budget = 100_u64.into();
        let worker = "a".repeat(32);
        let digest = super::super::autotune_apply::native_apply_sha256_hex(
            operation.encode().unwrap().as_bytes(),
        );
        let snapshot = |attempts| OwnedTrafficSnapshot {
            backend: SpeedtestTrafficCounters {
                rx_bytes: attempts * 10,
                tx_bytes: attempts * 5,
            },
            probes: SpeedtestTrafficCounters {
                rx_bytes: 0,
                tx_bytes: 0,
            },
        };
        let origin = OwnedTrafficCheckpoint {
            sequence: 0,
            counters: snapshot(0),
        };
        let path = root.join("checkpoint");
        origin
            .publish_bound(&path, &operation.identity.job_id, &worker, &digest)
            .unwrap();
        let session = EmbeddedSpeedtestSession {
            route_pin: NftRoutePin {
                table: None,
                owner: None,
                cleanup_on_drop: false,
            },
            probe_pin: None,
            credentials: BackendCredentials {
                uid: 32769,
                gid: 32770,
            },
            job_id: operation.identity.job_id.clone(),
            worker_run_id: worker,
            route_fingerprint: operation.identity.route_fingerprint.clone(),
            route: operation.route.clone(),
            backend: operation.backend.clone(),
            requested_server_id: None,
            authorized_traffic_budget: 100_u64.into(),
            traffic_policy_explicit: true,
            selected_server_id: None,
            server_qualified: false,
            qualified_raw_capacity: None,
            selected_endpoint_sha256: None,
            owned_checkpoint: Some((path, digest)),
            owned_ledger: Some(Cell::new(
                OwnedTrafficLedger::from_verified_checkpoint(100_u64.into(), origin).unwrap(),
            )),
        };
        let completed = Cell::new(0u64);
        let mut remaining = 100_u64.into();
        let mut backend_calls = Vec::new();
        let mut comparisons = Vec::new();
        let mut debits = Vec::new();
        let mut debit_count = 0u32;
        let error = qualify_server_candidates(
            None,
            true,
            &[Some(1), Some(2), Some(3), Some(4), Some(5), Some(6)],
            &mut comparisons,
            |_, candidate| {
                let id = candidate.unwrap();
                let mut measured = qualification_fixture_attempt(id, debit_count);
                measured.outcome = retry_budgeted_session_measurement_with_debit(
                    &session,
                    &operation,
                    &mut remaining,
                    20,
                    || Ok(snapshot(completed.get())),
                    |available| {
                        assert_eq!(available.limit_bytes(), Some(100 - completed.get() * 15));
                        backend_calls.push(id);
                        completed.set(completed.get() + 1);
                        if id <= 3 {
                            Err("speedtest-backend-failed".into())
                        } else {
                            qualification_fixture_attempt(id, 0).outcome
                        }
                    },
                    &mut |debit| {
                        if !debit.lifecycle {
                            debit_count += 1;
                        }
                        debits.push(debit);
                        Ok(())
                    },
                );
                measured.debit_count = debit_count - measured.debit_offset;
                Ok(measured)
            },
            &mut |_| Ok(()),
        )
        .err()
        .unwrap();
        assert_eq!(error, SPEEDTEST_TRAFFIC_BUDGET_EXHAUSTED);
        assert_eq!(backend_calls, [1, 2, 3, 4, 5, 6]);
        assert_eq!(comparisons.last().unwrap().candidate_id, Some(4));
        assert_eq!(comparisons.last().unwrap().debit_count, 0);
        assert_eq!(remaining.limit_bytes(), Some(10));
        assert_eq!(debits.iter().map(|row| row.rx_bytes).sum::<u64>(), 60);
        assert_eq!(debits.iter().map(|row| row.tx_bytes).sum::<u64>(), 30);
        assert_eq!(debit_count, 6);
        let ledger = session.owned_ledger.as_ref().unwrap().get();
        assert_eq!(ledger.committed.counters, snapshot(6));
        assert_eq!(ledger.authority.limit_bytes(), Some(100));
        // Re-read the durable owned checkpoint through the same path used by
        // subsequent attempts; callback accounting cannot silently reset it.
        let (checkpoint_path, request_digest) = session.owned_checkpoint.as_ref().unwrap();
        let stored = OwnedTrafficCheckpoint::decode_bound(
            &fs::read_to_string(checkpoint_path).unwrap(),
            &session.job_id,
            &session.worker_run_id,
            request_digest,
        )
        .unwrap();
        assert_eq!(stored, ledger.committed);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn t2_owned_retry_uses_original_allowance_and_preserves_between_attempt_bytes() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("cake-owned-retry-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let mut operation = request(SpeedtestDirection::Both);
        operation.traffic_budget = 100_u64.into();
        let worker = "a".repeat(32);
        let digest = super::super::autotune_apply::native_apply_sha256_hex(
            operation.encode().unwrap().as_bytes(),
        );
        let snapshot = |bytes| OwnedTrafficSnapshot {
            backend: SpeedtestTrafficCounters {
                rx_bytes: bytes,
                tx_bytes: 0,
            },
            probes: SpeedtestTrafficCounters {
                rx_bytes: 0,
                tx_bytes: 0,
            },
        };
        let origin = OwnedTrafficCheckpoint {
            sequence: 0,
            counters: snapshot(0),
        };
        let path = root.join("checkpoint");
        origin
            .publish_bound(&path, &operation.identity.job_id, &worker, &digest)
            .unwrap();
        let session = EmbeddedSpeedtestSession {
            route_pin: NftRoutePin {
                table: None,
                owner: None,
                cleanup_on_drop: false,
            },
            probe_pin: None,
            credentials: BackendCredentials {
                uid: 32769,
                gid: 32770,
            },
            job_id: operation.identity.job_id.clone(),
            worker_run_id: worker,
            route_fingerprint: operation.identity.route_fingerprint.clone(),
            route: operation.route.clone(),
            backend: operation.backend.clone(),
            requested_server_id: operation.speedtest_server_id,
            authorized_traffic_budget: 100_u64.into(),
            traffic_policy_explicit: true,
            selected_server_id: None,
            server_qualified: false,
            qualified_raw_capacity: None,
            selected_endpoint_sha256: None,
            owned_checkpoint: Some((path, digest)),
            owned_ledger: Some(std::cell::Cell::new(
                OwnedTrafficLedger::from_verified_checkpoint(100_u64.into(), origin).unwrap(),
            )),
        };
        let mut readings = [snapshot(10), snapshot(40), snapshot(50), snapshot(70)].into_iter();
        let mut calls = 0;
        let mut remaining = 100_u64.into();
        let mut debits = Vec::with_capacity(3);
        let outcome = retry_budgeted_session_measurement_with_debit(
            &session,
            &operation,
            &mut remaining,
            10,
            || Ok(readings.next().unwrap()),
            |available| {
                calls += 1;
                assert_eq!(
                    available.limit_bytes(),
                    Some(if calls == 1 { 90 } else { 50 })
                );
                if calls == 1 {
                    Err("speedtest-route-not-ready".into())
                } else {
                    Ok(())
                }
            },
            &mut |debit| {
                debits.push(debit);
                Ok(())
            },
        );
        outcome.unwrap();
        assert_eq!(remaining.limit_bytes(), Some(30));
        assert_eq!(
            debits.iter().map(|d| d.rx_bytes + d.tx_bytes).sum::<u64>(),
            70
        );
        assert!(debits.iter().all(|d| !d.lifecycle));
        let error = retry_budgeted_session_measurement_with_debit::<()>(
            &session,
            &operation,
            &mut remaining,
            10,
            || Ok(snapshot(95)),
            |_| panic!("insufficient observed remainder must refuse load"),
            &mut |debit| {
                debits.push(debit);
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(error, SPEEDTEST_TRAFFIC_BUDGET_EXHAUSTED);
        assert_eq!(remaining.limit_bytes(), Some(5));
        assert!(debits.last().unwrap().lifecycle);
        assert_eq!(debits.last().unwrap().rx_bytes, 25);
        let mut reset_readings = [snapshot(99), snapshot(98)].into_iter();
        assert!(retry_budgeted_session_measurement_with_debit(
            &session,
            &operation,
            &mut remaining,
            1,
            || Ok(reset_readings.next().unwrap()),
            |_| Ok(()),
            &mut |_| panic!("within-attempt reset must not be committed")
        )
        .is_err());
        let poisoned = session.owned_ledger.as_ref().unwrap().get();
        assert!(poisoned.remaining().is_err());
        assert_eq!(poisoned.committed.counters.total_bytes().unwrap(), 95);
        // A capture wait has no backend retry transaction to poison the
        // ledger on its behalf. Missing counters must still invalidate it.
        session.owned_ledger.as_ref().unwrap().set(
            OwnedTrafficLedger::from_verified_checkpoint(100_u64.into(), poisoned.committed)
                .unwrap(),
        );
        let mut wait_watch = OwnedBudgetWatch {
            backend: &session.route_pin,
            probes: &session.route_pin,
            limit: 100_u64.into(),
            previous: poisoned.committed.counters,
        };
        assert!(wait_watch
            .check_wait(&session)
            .unwrap_err()
            .starts_with("speedtest-owned-budget-observation-failed:"));
        assert!(session
            .owned_ledger
            .as_ref()
            .unwrap()
            .get()
            .remaining()
            .is_err());
        // The real backend may exit zero after a route switch (server list
        // with timeout entries). A final accounting fault must override that
        // success even when the byte allowance is unlimited.
        for authority in [
            100_u64.into(),
            super::super::protocol::TrafficPolicy::Unlimited,
        ] {
            session.owned_ledger.as_ref().unwrap().set(
                OwnedTrafficLedger::from_verified_checkpoint(authority, poisoned.committed)
                    .unwrap(),
            );
            let mut remaining = authority;
            let reads = Cell::new(0);
            let attempts = Cell::new(0);
            let error = retry_budgeted_session_measurement_with_debit(
                &session,
                &operation,
                &mut remaining,
                1,
                || {
                    reads.set(reads.get() + 1);
                    if reads.get() == 1 {
                        Ok(poisoned.committed.counters)
                    } else {
                        Err("speedtest-accounting-flow-registration-failed".into())
                    }
                },
                |_| {
                    attempts.set(attempts.get() + 1);
                    Ok(())
                },
                &mut |_| panic!("faulted successful backend must not publish a debit"),
            )
            .unwrap_err();
            assert_eq!(error, "speedtest-accounting-flow-registration-failed");
            assert_eq!(reads.get(), 2);
            assert_eq!(attempts.get(), 1);
            let ledger = session.owned_ledger.as_ref().unwrap().get();
            assert!(ledger.remaining().is_err());
            assert_eq!(ledger.committed, poisoned.committed);
        }
        drop(session);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn t2_public_accounting_method_requires_a_bound_private_checkpoint() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("cake-owned-label-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let mut operation = request(SpeedtestDirection::Both);
        operation.traffic_policy_explicit = true;
        let worker = "a".repeat(32);
        assert!(verify_owned_accounting_checkpoint(&root, &operation, &worker).is_err());
        let digest = super::super::autotune_apply::native_apply_sha256_hex(
            operation.encode().unwrap().as_bytes(),
        );
        let zero = SpeedtestTrafficCounters {
            rx_bytes: 0,
            tx_bytes: 0,
        };
        let point = OwnedTrafficCheckpoint {
            sequence: 0,
            counters: OwnedTrafficSnapshot {
                backend: zero,
                probes: zero,
            },
        };
        let path = root.join(format!("owned-traffic-checkpoint-{worker}"));
        point
            .publish_bound(&path, &operation.identity.job_id, &worker, &digest)
            .unwrap();
        assert_eq!(
            verify_owned_accounting_checkpoint(&root, &operation, &worker).unwrap(),
            (0, 0, 0)
        );
        let next = OwnedTrafficCheckpoint {
            sequence: 1,
            counters: OwnedTrafficSnapshot {
                backend: SpeedtestTrafficCounters {
                    rx_bytes: 7,
                    tx_bytes: 2,
                },
                probes: SpeedtestTrafficCounters {
                    rx_bytes: 2,
                    tx_bytes: 1,
                },
            },
        };
        next.publish_bound(&path, &operation.identity.job_id, &worker, &digest)
            .unwrap();
        assert_eq!(
            verify_owned_accounting_checkpoint(&root, &operation, &worker).unwrap(),
            (1, 9, 3)
        );
        let staged = path.with_extension("owned-next");
        super::super::autotune_apply_runtime::write_new_private_file(&staged, b"ambiguous")
            .unwrap();
        assert!(verify_owned_accounting_checkpoint(&root, &operation, &worker).is_err());
        assert!(staged.exists());
        fs::remove_file(staged).unwrap();
        operation.traffic_budget = 1_000_000_u64.into();
        assert!(verify_owned_accounting_checkpoint(&root, &operation, &worker).is_err());
        assert!(verify_owned_accounting_checkpoint(&root, &operation, "../outside").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn t2_owned_debit_intent_precedes_append_and_refuses_ambiguous_retry() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("cake-debit-intent-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let job = "a".repeat(32);
        let worker = "b".repeat(32);
        let digest = "c".repeat(64);
        let zero = SpeedtestTrafficCounters {
            rx_bytes: 0,
            tx_bytes: 0,
        };
        let origin = OwnedTrafficCheckpoint {
            sequence: 0,
            counters: OwnedTrafficSnapshot {
                backend: zero,
                probes: zero,
            },
        };
        let next = OwnedTrafficCheckpoint {
            sequence: 1,
            counters: OwnedTrafficSnapshot {
                backend: SpeedtestTrafficCounters {
                    rx_bytes: 70,
                    tx_bytes: 30,
                },
                probes: SpeedtestTrafficCounters {
                    rx_bytes: 7,
                    tx_bytes: 3,
                },
            },
        };
        for fail in [false, true] {
            let path = root.join(if fail { "failed" } else { "complete" });
            origin.publish_bound(&path, &job, &worker, &digest).unwrap();
            let intent = path.with_extension("owned-intent");
            let result = next.commit_debit_with_intent(&path, &job, &worker, &digest, || {
                let saved = fs::read_to_string(&intent).unwrap();
                assert_eq!(
                    OwnedTrafficCheckpoint::decode_bound(&saved, &job, &worker, &digest).unwrap(),
                    next
                );
                if fail {
                    Err("append-outcome-uncertain".into())
                } else {
                    Ok(())
                }
            });
            let saved = OwnedTrafficCheckpoint::decode_bound(
                &fs::read_to_string(&path).unwrap(),
                &job,
                &worker,
                &digest,
            )
            .unwrap();
            if fail {
                assert_eq!(result.unwrap_err(), "append-outcome-uncertain");
                assert_eq!(saved, origin);
                assert!(intent.exists());
                assert!(next
                    .commit_debit_with_intent(&path, &job, &worker, &digest, || panic!(
                        "uncertain debit must never be appended twice"
                    ))
                    .is_err());
                assert!(intent.exists());
            } else {
                result.unwrap();
                assert_eq!(saved, next);
                assert!(!intent.exists());
            }
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn t2_owned_traffic_checkpoint_is_identity_bound_and_refuses_lost_origin() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("cake-owned-point-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("checkpoint");
        let (job, worker, request) = ("a".repeat(32), "b".repeat(32), "c".repeat(64));
        let zero = OwnedTrafficCheckpoint {
            sequence: 0,
            counters: OwnedTrafficSnapshot {
                backend: SpeedtestTrafficCounters {
                    rx_bytes: 0,
                    tx_bytes: 0,
                },
                probes: SpeedtestTrafficCounters {
                    rx_bytes: 0,
                    tx_bytes: 0,
                },
            },
        };
        let mut first = zero;
        first.sequence = 1;
        first.counters.backend.rx_bytes = 100;
        assert!(first.publish_bound(&path, &job, &worker, &request).is_err());
        assert!(!path.exists());
        zero.publish_bound(&path, &job, &worker, &request).unwrap();
        first.publish_bound(&path, &job, &worker, &request).unwrap();
        first.publish_bound(&path, &job, &worker, &request).unwrap();
        let bytes = fs::read_to_string(&path).unwrap();
        assert_eq!(
            OwnedTrafficCheckpoint::decode_bound(&bytes, &job, &worker, &request).unwrap(),
            first
        );
        assert!(
            OwnedTrafficCheckpoint::decode_bound(&bytes, &"d".repeat(32), &worker, &request)
                .is_err()
        );
        assert!(
            OwnedTrafficCheckpoint::decode_bound(&bytes, &job, &"d".repeat(32), &request).is_err()
        );
        assert!(
            OwnedTrafficCheckpoint::decode_bound(&bytes, &job, &worker, &"d".repeat(64)).is_err()
        );
        assert!(OwnedTrafficCheckpoint::decode_bound(
            &bytes.replace("100", "101"),
            &job,
            &worker,
            &request
        )
        .is_err());
        let mut skipped = first;
        skipped.sequence = 3;
        skipped.counters.backend.rx_bytes = 200;
        assert!(skipped
            .publish_bound(&path, &job, &worker, &request)
            .is_err());
        let mut reset = first;
        reset.sequence = 2;
        reset.counters.backend.rx_bytes = 99;
        reset.counters.probes.rx_bytes = 1000;
        assert!(reset.publish_bound(&path, &job, &worker, &request).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), bytes);
        let staged = path.with_extension("owned-next");
        super::super::autotune_apply_runtime::write_new_private_file(&staged, bytes.as_bytes())
            .unwrap();
        assert!(first.publish_bound(&path, &job, &worker, &request).is_err());
        assert_eq!(
            fs::read_to_string(&staged).unwrap(),
            bytes,
            "an ambiguous staged write must not be silently discarded"
        );
        fs::remove_file(staged).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(first.publish_bound(&path, &job, &worker, &request).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn t2_whole_job_ledger_preserves_idle_retries_tail_and_uncertain_writes() {
        let counters = |rx, tx, probe_rx, probe_tx| OwnedTrafficSnapshot {
            backend: SpeedtestTrafficCounters {
                rx_bytes: rx,
                tx_bytes: tx,
            },
            probes: SpeedtestTrafficCounters {
                rx_bytes: probe_rx,
                tx_bytes: probe_tx,
            },
        };
        let zero = OwnedTrafficCheckpoint {
            sequence: 0,
            counters: counters(0, 0, 0, 0),
        };
        let mut ledger =
            OwnedTrafficLedger::from_verified_checkpoint(100_u64.into(), zero).unwrap();
        let mut persisted = Vec::with_capacity(4);
        for snapshot in [
            counters(0, 0, 10, 5),
            counters(50, 20, 12, 8),
            counters(80, 40, 20, 10),
            counters(90, 40, 25, 15),
        ] {
            let exceeded = ledger
                .checkpoint(snapshot, |point, debit| {
                    persisted.push((point, debit));
                    Ok(())
                })
                .unwrap();
            assert_eq!(exceeded, snapshot.total_bytes().unwrap() > 100);
        }
        assert_eq!(
            persisted
                .iter()
                .map(|(_, d)| d.rx_bytes + d.tx_bytes)
                .sum::<u64>(),
            170
        );
        assert_eq!(ledger.remaining().unwrap(), 0_u64.into());
        assert_eq!(persisted.last().unwrap().0.sequence, 4);
        assert!(ledger
            .checkpoint(ledger.committed.counters, |_, _| panic!(
                "identical snapshot must not charge twice"
            ))
            .unwrap());
        let recovered =
            OwnedTrafficLedger::from_verified_checkpoint(100_u64.into(), ledger.committed).unwrap();
        assert_eq!(recovered.remaining().unwrap(), ledger.remaining().unwrap());

        let mut unlimited = OwnedTrafficLedger::from_verified_checkpoint(
            super::super::protocol::TrafficPolicy::Unlimited,
            zero,
        )
        .unwrap();
        let large = counters(40_000_000_000, 30_000_000_000, 100, 50);
        assert!(!unlimited.checkpoint(large, |_, _| Ok(())).unwrap());
        assert_eq!(
            unlimited.remaining().unwrap(),
            super::super::protocol::TrafficPolicy::Unlimited
        );
        assert_eq!(
            unlimited.committed.counters.total_bytes().unwrap(),
            70_000_000_150
        );

        let mut uncertain =
            OwnedTrafficLedger::from_verified_checkpoint(100_u64.into(), zero).unwrap();
        let mut durable = None;
        assert!(uncertain
            .checkpoint(counters(20, 0, 0, 0), |point, _| {
                durable = Some(point);
                Err("append acknowledgement lost".into())
            })
            .is_err());
        assert!(uncertain.remaining().is_err());
        assert!(uncertain
            .checkpoint(counters(20, 0, 0, 0), |_, _| panic!(
                "ambiguous append must not be duplicated"
            ))
            .is_err());
        let mut restored =
            OwnedTrafficLedger::from_verified_checkpoint(100_u64.into(), durable.unwrap()).unwrap();
        restored
            .checkpoint(counters(25, 0, 0, 0), |_, debit| {
                assert_eq!(debit.rx_bytes, 5);
                Ok(())
            })
            .unwrap();
        assert_eq!(restored.remaining().unwrap(), 75_u64.into());
        assert!(restored
            .checkpoint(counters(24, 0, 50, 0), |_, _| panic!(
                "masked reset must not persist"
            ))
            .is_err());
        assert!(restored.remaining().is_err());
    }

    #[test]
    fn t2_owned_traffic_snapshots_keep_all_four_counters_independent() {
        let snapshot = |rx, tx, probe_rx, probe_tx| OwnedTrafficSnapshot {
            backend: SpeedtestTrafficCounters {
                rx_bytes: rx,
                tx_bytes: tx,
            },
            probes: SpeedtestTrafficCounters {
                rx_bytes: probe_rx,
                tx_bytes: probe_tx,
            },
        };
        let zero = snapshot(0, 0, 0, 0);
        let idle = snapshot(0, 0, 100, 50);
        let load = snapshot(10000, 2000, 200, 100);
        let tail = snapshot(10020, 2030, 220, 110);
        let mut charged = 0;
        for (previous, current) in [(zero, idle), (idle, load), (load, tail)] {
            let debit = current.delta_since(previous).unwrap();
            charged += debit.rx_bytes + debit.tx_bytes;
        }
        assert_eq!(charged, tail.total_bytes().unwrap());
        assert_eq!(charged, 12380);
        assert_eq!(
            tail.delta_since(tail).unwrap(),
            SpeedtestTrafficDebit {
                lifecycle: false,
                rx_bytes: 0,
                tx_bytes: 0
            }
        );
        for reset in [
            snapshot(9999, 3000, 5000, 5000),
            snapshot(20000, 1999, 5000, 5000),
            snapshot(20000, 3000, 199, 5000),
            snapshot(20000, 3000, 5000, 99),
        ] {
            assert!(reset.total_bytes().unwrap() > load.total_bytes().unwrap());
            assert!(reset.delta_since(load).is_err());
        }
        let overflow = snapshot(u64::MAX, 1, 0, 0);
        assert!(overflow.total_bytes().is_err());
        assert!(overflow.delta_since(zero).is_err());
        assert_eq!(
            load.backend.rx_bytes, 10000,
            "probe debit must not become backend goodput"
        );
    }

    #[test]
    fn t2_live_counter_read_requires_positive_absence_evidence() {
        let table = "cake_st_aaaaaaaaaaaa_bbbbbbbbbbbb";
        let owner = "cake-autorate-speedtest:test-owner";
        let present =
            format!(r#"{{"nftables":[{{"table":{{"family":"inet","name":"{table}"}}}}]}}"#)
                .into_bytes();
        for (success, snapshot, absent) in [
            (true, present, false),
            (true, br#"{"nftables":[]}"#.to_vec(), true),
            (false, br#"{"nftables":[]}"#.to_vec(), false),
            (true, b"{}".to_vec(), false),
            (true, br#"{"nftables":[],"nftables":[]}"#.to_vec(), false),
        ] {
            let mut calls = 0;
            let result = read_live_named_traffic_counters_with(table, owner, |arguments| {
                calls += 1;
                if calls == 1 {
                    assert_eq!(arguments, nft_table_snapshot_arguments(table));
                    Ok((false, Vec::new()))
                } else {
                    assert_eq!(arguments, ["-j", "list", "tables"]);
                    Ok((success, snapshot.clone()))
                }
            });
            assert_eq!(calls, 2);
            if absent {
                assert!(result.unwrap().is_none());
            } else {
                assert!(result.is_err());
            }
        }
        let mut calls = 0;
        let error = read_live_named_traffic_counters_with(table, owner, |_| {
            calls += 1;
            Err("cancelled".to_string())
        })
        .unwrap_err();
        assert_eq!(error, "cancelled");
        assert_eq!(calls, 1);
    }

    #[test]
    fn t2_mwan3_environment_and_route_pin_are_strict_and_identity_owned() {
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
        let parsed: serde_json::Value = serde_json::from_str(&batch).unwrap();
        let commands = parsed["nftables"].as_array().unwrap();
        assert_eq!(commands[0]["create"]["table"]["comment"], owner);
        assert_eq!(commands[1]["add"]["set"]["size"], MAX_ACCOUNTING_FLOWS);
        assert!(commands[1]["add"]["set"].get("timeout").is_none());
        for (entry, name) in commands[2..5].iter().zip(["rx", "tx", "flow_fault"]) {
            assert_eq!(entry["add"]["counter"]["name"], name);
            assert_eq!(
                entry["add"]["counter"]["comment"],
                format!("{owner}{FLOW_ACCOUNTING_OWNER_SUFFIX}")
            );
        }
        for (entry, hook, kind) in [
            (&commands[5], "output", "route"),
            (&commands[6], "input", "filter"),
        ] {
            assert_eq!(entry["add"]["chain"]["hook"], hook);
            assert_eq!(entry["add"]["chain"]["type"], kind);
            assert_eq!(entry["add"]["chain"]["prio"], -148);
        }
        let rules: Vec<_> = commands
            .iter()
            .filter(|command| {
                command["add"].get("rule").is_some() && !command.to_string().contains("owned_dns6")
            })
            .collect();
        assert_eq!(rules.len(), 5);
        assert_eq!(
            rules[0]["add"]["rule"]["expr"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["set"]["op"],
            "update"
        );
        assert_eq!(
            rules[1]["add"]["rule"]["expr"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()["set"]["op"],
            "delete"
        );
        let transmit = rules[2]["add"]["rule"]["expr"].as_array().unwrap();
        let mark = transmit
            .iter()
            .find(|value| value.get("mangle").is_some())
            .unwrap();
        assert_eq!(mark["mangle"]["value"]["|"][0]["&"][1], !0x3f00_u32);
        assert_eq!(mark["mangle"]["value"]["|"][1], 0x200);
        let fallback = rules[3]["add"]["rule"]["expr"].as_array().unwrap();
        assert_eq!(fallback.len(), 3);
        assert_eq!(fallback[0]["match"]["left"]["meta"]["key"], "skuid");
        assert_eq!(fallback[0]["match"]["right"], 32769);
        assert_eq!(fallback[1]["counter"], "flow_fault");
        assert!(fallback[2].get("drop").is_some());
        assert_eq!(rules[4]["add"]["rule"]["chain"], "input");
        assert!(!rules[4].to_string().contains("skuid"));
        let accounting_only = nft_route_pin_batch(&table, &owner, 32769, None);
        assert!(!accounting_only.contains("\"mangle\""));
        assert!(accounting_only.contains("{\"counter\":\"rx\"}"));
        assert!(accounting_only.contains("{\"counter\":\"tx\"}"));
        assert!(route_pin_table_name("not-hex", &run_id).is_err());
    }

    #[test]
    fn t2_cutoff_batch_detaches_only_exact_private_chains_without_resetting_counters() {
        let job = "a".repeat(32);
        let worker = "b".repeat(32);
        let batch = nft_owned_cutoff_batch(&job, &worker).unwrap();
        let value: serde_json::Value = serde_json::from_str(&batch).unwrap();
        let commands = value["nftables"].as_array().unwrap();
        assert_eq!(commands.len(), 4);
        for (index, command) in commands.iter().enumerate() {
            let table = if index < 2 {
                route_pin_table_name(&job, &worker).unwrap()
            } else {
                probe_pin_identity(&job, &worker).unwrap().0
            };
            assert_eq!(
                *command,
                serde_json::json!({"flush":{"chain":{
                    "family":"inet", "table":table,
                    "name":if index % 2 == 0 { "output" } else { "input" }
                }}})
            );
        }
        assert!(!batch.contains("counter"));
        assert!(!batch.contains("delete"));
        assert!(nft_owned_cutoff_batch("bad", &worker).is_err());
    }

    #[test]
    fn t2_flow_accounting_export_exact_production_rules_for_kernel_fixture() {
        use std::os::unix::fs::PermissionsExt;
        let Some(directory) = std::env::var_os("CAKE_T2_NFT_BATCH_DIR") else {
            return;
        };
        let directory = PathBuf::from(directory);
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        super::super::autotune_apply_runtime::write_new_private_file(
            &directory.join("cutoff.json"),
            nft_owned_cutoff_batch(&"a".repeat(32), &"b".repeat(32))
                .unwrap()
                .as_bytes(),
        )
        .unwrap();
        let table = route_pin_table_name(&"a".repeat(32), &"b".repeat(32)).unwrap();
        let owner = route_pin_owner(&"a".repeat(32), &"b".repeat(32)).unwrap();
        let mut query = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.join("query.json"))
            .unwrap();
        query
            .write_all(
                serde_json::to_string(&nft_table_snapshot_arguments(&table))
                    .unwrap()
                    .as_bytes(),
            )
            .unwrap();
        // UID0 is solely for the one-ID isolated user namespace fixture.
        // Production credentials already reject UID0 before this renderer.
        for (name, mark) in [("main.json", None), ("mwan3.json", Some((!0x3f00, 0x200)))] {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(directory.join(name))
                .unwrap();
            file.write_all(nft_route_pin_batch(&table, &owner, 0, mark).as_bytes())
                .unwrap();
        }
        let (probe_table, probe_owner) =
            probe_pin_identity(&"a".repeat(32), &"b".repeat(32)).unwrap();
        // A separate kernel fixture can map its sole GID to42. These are the
        // exact production root+GID predicates, not an allocated mark bit.
        for (name, mark) in [
            ("probe-main.json", None),
            ("probe-mwan3.json", Some((!0x3f00, 0x200))),
        ] {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(directory.join(name))
                .unwrap();
            file.write_all(
                nft_owned_route_pin_batch(
                    &probe_table,
                    &probe_owner,
                    NftSocketOwner::ProbeRootGid(42),
                    mark,
                )
                .as_bytes(),
            )
            .unwrap();
        }
    }

    #[test]
    fn t2_flow_counter_version_requires_fault_evidence_and_keeps_legacy_readable() {
        use serde_json::json;
        let table = route_pin_table_name(&"a".repeat(32), &"b".repeat(32)).unwrap();
        let owner = route_pin_owner(&"a".repeat(32), &"b".repeat(32)).unwrap();
        let counters = |counter_owner: &str| {
            json!({"nftables":[
                {"counter":{"family":"inet","table":table,"name":"rx","comment":counter_owner,"bytes":1200}},
                {"counter":{"family":"inet","table":table,"name":"tx","comment":counter_owner,"bytes":300}}
            ]})
        };
        let expected = SpeedtestTrafficCounters {
            rx_bytes: 1200,
            tx_bytes: 300,
        };
        assert_eq!(
            parse_named_traffic_counters(&counters(&owner).to_string(), &table, &owner).unwrap(),
            expected
        );
        let flow_owner = format!("{owner}{FLOW_ACCOUNTING_OWNER_SUFFIX}");
        let mut current = counters(&flow_owner);
        assert_eq!(
            parse_named_traffic_counters(&current.to_string(), &table, &owner).unwrap_err(),
            "speedtest-accounting-flow-fault-counter-missing"
        );
        current["nftables"]
            .as_array_mut()
            .unwrap()
            .push(json!({"counter":{
            "family":"inet","table":table,"name":"flow_fault","comment":flow_owner,"bytes":0}}));
        assert_eq!(
            parse_named_traffic_counters(&current.to_string(), &table, &owner).unwrap(),
            expected
        );
        current["nftables"][2]["counter"]["bytes"] = json!(1);
        let error = parse_named_traffic_counters(&current.to_string(), &table, &owner).unwrap_err();
        assert_eq!(error, "speedtest-accounting-flow-registration-failed");
        assert!(!server_attempt_is_retryable(&error));
        assert!(!completed_speedtest_output_is_unmeasurable(&error));
        current["nftables"][2]["counter"]["bytes"] = json!(0);
        current["nftables"][2]["counter"]["comment"] = json!(owner);
        assert!(parse_named_traffic_counters(&current.to_string(), &table, &owner).is_err());
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
    fn r6_route_mask_drift_is_rejected_before_traffic() {
        let mut request = request(SpeedtestDirection::Both);
        request.route.mode = OperationRouteMode::Mwan3;
        request.route.mwan3_member = Some("wan".to_string());
        request.route.fwmark = Some(0x100);
        request.route.routing_table = Some(1);
        request.route.fwmark_mask = Some(0x3f00);
        let environment = format!(
            "DEVICE={}\nSRCIP={}\nFWMARK=0x3f00\n",
            request.route.l3_device,
            request.route.source_ip.unwrap()
        );
        assert_eq!(
            validate_mwan3_environment(&request, &environment).unwrap(),
            0x3f00
        );
        for changed in [
            environment.replace("0x3f00", "0xff00"),
            environment.replace("0x3f00", "0x0"),
            environment.replace("0x3f00", "garbage"),
            format!("{environment}FWMARK=0x3f00\n"),
            environment.replace("FWMARK=0x3f00\n", ""),
        ] {
            assert!(validate_mwan3_environment(&request, &changed).is_err());
        }
        let mut actual = RouteIdentity {
            device_ifindex: None,
            mode: "mwan3".to_string(),
            member: "wan".to_string(),
            device: request.route.l3_device.clone(),
            source_ip: request.route.source_ip.unwrap().to_string(),
            fwmark: "0x100".to_string(),
            table: "1".to_string(),
            fwmark_mask: Some(0x3f00),
        };
        assert!(route_matches_request(&request, &actual).is_ok());
        let original_key = actual.stable_key();
        actual.fwmark_mask = Some(0xff00);
        assert_ne!(original_key, actual.stable_key());
        assert!(route_matches_request(&request, &actual).is_err());
        actual.fwmark_mask = None;
        assert!(route_matches_request(&request, &actual).is_err());
        // Legacy requests did not carry a mask; preserve their wire contract.
        request.route.fwmark_mask = None;
        assert!(route_matches_request(&request, &actual).is_ok());
    }

    #[test]
    fn r6_explicit_backend_mark_and_link_never_use_mwan3_or_unmarked_fallback() {
        let mut request = request(SpeedtestDirection::Both);
        let live = RouteIdentity {
            device_ifindex: Some(42),
            fwmark_mask: Some(0x3f00),
            mode: "explicit".into(),
            member: String::new(),
            device: "pppoe-wan".into(),
            source_ip: "192.0.2.1".into(),
            fwmark: "0x100".into(),
            table: "101".into(),
        };
        request.route = super::super::autotune_request::operation_route_identity(&live).unwrap();
        assert_eq!(
            selected_route_mark(&request, || panic!("explicit cannot consult mwan3")).unwrap(),
            Some((!0x3f00, 0x100))
        );
        assert!(route_matches_request(&request, &live).is_ok());
        let mut changed = live.clone();
        changed.device_ifindex = Some(43);
        assert!(route_matches_request(&request, &changed).is_err());
        changed = live;
        changed.fwmark_mask = Some(0xff00);
        assert!(route_matches_request(&request, &changed).is_err());
        request.route.fwmark_mask = None;
        assert!(selected_route_mark(&request, || panic!("no fallback")).is_err());
        request.route.fwmark_mask = Some(0x3f00);
        request.route.fwmark = Some(0x4000);
        assert!(selected_route_mark(&request, || panic!("no fallback")).is_err());
    }

    #[test]
    fn route_identity_must_match_structured_request() {
        let request = request(SpeedtestDirection::Both);
        let actual = RouteIdentity {
            device_ifindex: None,
            fwmark_mask: None,
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
                device_ifindex: None,
                fwmark_mask: None,
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
                device_ifindex: None,
                fwmark_mask: None,
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
        let mut remaining = 10_000_u64.into();
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
        assert_eq!(result.limit_bytes(), Some(8_080));
        assert_eq!(remaining.limit_bytes(), Some(8_050));
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
        let mut remaining = 10_000_u64.into();
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
        assert_eq!(result.limit_bytes(), Some(9_700));
        assert_eq!(remaining.limit_bytes(), Some(9_400));
        assert_eq!(debit_count.get(), 2);
        assert_eq!(debited.get(), (200, 400));
    }

    #[test]
    fn persistently_unproved_traffic_stops_after_three_charged_attempts() {
        let attempts = Cell::new(0_u32);
        let counters = Cell::new((1_000_u64, 2_000_u64));
        let debit_count = Cell::new(0_u32);
        let mut remaining = 10_000_u64.into();
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
        assert_eq!(remaining.limit_bytes(), Some(9_100));
        assert_eq!(debit_count.get(), u32::from(MAX_UNPROVED_TRAFFIC_ATTEMPTS));
    }

    #[test]
    fn readiness_loss_does_not_spend_the_unproved_traffic_retry_limit() {
        let attempts = Cell::new(0_u32);
        let counters = Cell::new((1_000_u64, 2_000_u64));
        let mut remaining = 10_000_u64.into();
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
        assert_eq!(remaining.limit_bytes(), Some(9_850));
    }

    #[test]
    fn insufficient_attempt_budget_stops_before_counters_or_transfer() {
        let counter_reads = Cell::new(0_u32);
        let attempts = Cell::new(0_u32);
        let debit_count = Cell::new(0_u32);
        let mut remaining = 999_u64.into();
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
        assert_eq!(remaining.limit_bytes(), Some(999));
        assert_eq!(counter_reads.get(), 0);
        assert_eq!(attempts.get(), 0);
        assert_eq!(debit_count.get(), 0);
    }

    #[test]
    fn supervisor_limit_is_debited_exactly_before_it_is_returned() {
        let counters = Cell::new((10_000_u64, 20_000_u64));
        let debit_count = Cell::new(0_u32);
        let debited = Cell::new((0_u64, 0_u64));
        let mut remaining = 10_000_u64.into();
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
        assert_eq!(remaining.limit_bytes(), Some(8_000));
        assert_eq!(debit_count.get(), 1);
        assert_eq!(debited.get(), (1_500, 500));
    }

    #[test]
    fn t2_zero_byte_fault_keeps_its_cause_without_empty_measurement_credit() {
        let mut remaining = 1_000_u64.into();
        let attempts = Cell::new(0_u32);
        let error = retry_budgeted_route_measurement_on_loss_with_debit::<(), _, _>(
            &mut remaining,
            1,
            || Ok((100, 200)),
            |_| {
                attempts.set(attempts.get() + 1);
                Err("speedtest-accounting-flow-registration-failed".into())
            },
            &mut |_| panic!("zero usage must not create a measurement debit"),
        )
        .unwrap_err();
        assert_eq!(error, "speedtest-accounting-flow-registration-failed");
        assert_eq!(attempts.get(), 1);
        assert_eq!(remaining.limit_bytes(), Some(1_000));
    }

    #[test]
    fn t2_counter_overrun_is_debited_exactly_and_cannot_retry() {
        for outcome in [
            Ok(()),
            Err(SPEEDTEST_TRAFFIC_LIMIT_REACHED.to_string()),
            Err("speedtest-route-not-ready".to_string()),
            Err("speedtest-route-traffic-unproved".to_string()),
            Err("speedtest-cancelled".to_string()),
            Err("speedtest-timeout".to_string()),
        ] {
            let counters = Cell::new((100_u64, 200_u64));
            let attempts = Cell::new(0_u32);
            let debit_count = Cell::new(0_u32);
            let debited = Cell::new((0_u64, 0_u64));
            let mut remaining = 1_000_u64.into();
            let error = retry_budgeted_route_measurement_on_loss_with_debit::<(), _, _>(
                &mut remaining,
                1,
                || Ok(counters.get()),
                |_| {
                    attempts.set(attempts.get() + 1);
                    counters.set((900, 600));
                    outcome.clone()
                },
                &mut |debit| {
                    debit_count.set(debit_count.get() + 1);
                    debited.set((debit.rx_bytes, debit.tx_bytes));
                    Ok(())
                },
            )
            .unwrap_err();

            assert_eq!(error, "speedtest-traffic-budget-exceeded");
            assert_eq!(remaining.limit_bytes(), Some(0));
            assert_eq!(debit_count.get(), 1);
            assert_eq!(attempts.get(), 1);
            assert_eq!(debited.get(), (800, 400));
        }
    }

    #[test]
    fn traffic_debit_persistence_failure_stops_before_any_retry() {
        let attempts = Cell::new(0_u32);
        let counters = Cell::new((100_u64, 200_u64));
        let mut remaining = 1_000_u64.into();
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
        assert_eq!(remaining.limit_bytes(), Some(970));
    }

    #[test]
    fn route_measurement_retry_keeps_static_errors_and_accounting_fail_closed() {
        let attempts = Cell::new(0_u32);
        let counters = Cell::new((100_u64, 200_u64));
        let mut remaining = 1_000_u64.into();
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
        assert_eq!(remaining.limit_bytes(), Some(980));

        let reads = Cell::new(0_u32);
        let mut remaining = 1_000_u64.into();
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
        assert_eq!(remaining.limit_bytes(), Some(1_000));
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
        assert_eq!(
            decoded.debited_bytes, None,
            "version 1 carries no debit total"
        );
    }

    #[test]
    fn t2_terminal_v2_reports_debits_for_every_state() {
        for terminal in [
            SpeedtestTerminal::Failed {
                code: SPEEDTEST_TRAFFIC_LIMIT_REACHED.to_string(),
            },
            SpeedtestTerminal::Cancelled,
            SpeedtestTerminal::Complete(SpeedtestResult {
                direction: SpeedtestDirection::Download,
                download_kbps: Some(20_000),
                upload_kbps: None,
                rx_bytes: 30_000_000,
                tx_bytes: 1_000_000,
                elapsed_ms: 12_000,
                server_id: None,
                server_name: String::new(),
                server_sponsor: String::new(),
            }),
        ] {
            let encoded = terminal
                .encode_debited(&"a".repeat(32), &"b".repeat(32), 33_070_284)
                .unwrap();
            assert!(encoded.starts_with("cake-autorate-speedtest-terminal\t2\n"));
            let decoded = SpeedtestTerminalRecord::decode(&encoded).unwrap();
            assert_eq!(decoded.terminal, terminal);
            assert_eq!(decoded.debited_bytes, Some(33_070_284));
            for corrupt in [
                encoded.replace("debited_bytes=33070284\n", ""),
                encoded.replace("debited_bytes=33070284", "debited_bytes="),
                encoded.replace("\t2\n", "\t3\n"),
                encoded.replace("\t2\n", "\t1\n"),
            ] {
                assert!(SpeedtestTerminalRecord::decode(&corrupt).is_err());
            }
        }
    }
}
