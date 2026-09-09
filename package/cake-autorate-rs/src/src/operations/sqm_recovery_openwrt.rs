//! Native, fail-closed managed-SQM attestation for both Full and Lite builds.
//!
//! This module owns both attestation and exact native recovery: kernel locks,
//! an immutable UCI snapshot, sqm-scripts state identity, bounded start/stop,
//! and exact CAKE topology postconditions.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::rate_limits;

const DEFAULT_LOCK_ROOT: &str = "/tmp/cake-autorate-speedtest";
const DEFAULT_SQM_CONFIG: &str = "/etc/config/sqm";
const DEFAULT_SQM_STATE_ROOT: &str = "/var/run/sqm";
const DEFAULT_SYS_CLASS_NET: &str = "/sys/class/net";
const MAX_COMMAND_OUTPUT: usize = 256 * 1024;
const MAX_UCI_LINES: usize = 512;
const MAX_UCI_VALUE_BYTES: usize = 16 * 1024;
const MAX_UCI_LIST_VALUES: usize = 64;
const MAX_UCI_SECTION_VALUE_BYTES: usize = 64 * 1024;
const MAX_STATE_BYTES: usize = 64 * 1024;
const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const SQM_RUN_TIMEOUT: Duration = Duration::from_secs(30);
const PROCESS_WAIT_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ManagedSqmAttestationSpec {
    pub instance: String,
    pub sqm_section: String,
    pub target_interface: String,
    pub upload_interface: String,
    pub download_interface: String,
    pub direction_mode: String,
    pub minimum_download_kbps: u64,
    pub maximum_download_kbps: u64,
    pub minimum_upload_kbps: u64,
    pub maximum_upload_kbps: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManagedSqmRatePolicy {
    pub minimum_download_kbps: u64,
    pub maximum_download_kbps: u64,
    pub minimum_upload_kbps: u64,
    pub maximum_upload_kbps: u64,
}

impl ManagedSqmRatePolicy {
    fn validate(&self) -> Result<(), NativeSqmAttestationError> {
        for (minimum, maximum, direction) in [
            (
                self.minimum_download_kbps,
                self.maximum_download_kbps,
                "download",
            ),
            (self.minimum_upload_kbps, self.maximum_upload_kbps, "upload"),
        ] {
            if minimum == 0 || minimum > maximum || maximum > rate_limits::MAX_RATE_KBPS {
                return Err(NativeSqmAttestationError::failed(format!(
                    "managed SQM {direction} controller bounds are invalid"
                )));
            }
        }
        Ok(())
    }
}

pub(crate) fn managed_sqm_rate_policy_from_options(
    options: &BTreeMap<String, String>,
) -> Result<ManagedSqmRatePolicy, NativeSqmAttestationError> {
    let minimum_download_kbps = configured_integer_rate(
        options,
        "min_dl_shaper_rate_kbps",
        rate_limits::DEFAULT_MIN_DL_SHAPER_RATE_KBPS,
        false,
    )?;
    let configured_maximum_download_kbps = configured_integer_rate(
        options,
        "max_dl_shaper_rate_kbps",
        rate_limits::DEFAULT_MAX_DL_SHAPER_RATE_KBPS,
        false,
    )?;
    let minimum_upload_kbps = configured_integer_rate(
        options,
        "min_ul_shaper_rate_kbps",
        rate_limits::DEFAULT_MIN_UL_SHAPER_RATE_KBPS,
        false,
    )?;
    let configured_maximum_upload_kbps = configured_integer_rate(
        options,
        "max_ul_shaper_rate_kbps",
        rate_limits::DEFAULT_MAX_UL_SHAPER_RATE_KBPS,
        false,
    )?;
    let adaptive = match options.get("adaptive_ceiling_enabled").map(String::as_str) {
        None | Some("0") => false,
        Some("1") => true,
        Some(_) => {
            return Err(NativeSqmAttestationError::failed(
                "adaptive_ceiling_enabled must be exactly 0 or 1",
            ))
        }
    };
    let download_cap = configured_integer_rate(
        options,
        "adaptive_ceiling_dl_cap_kbps",
        configured_maximum_download_kbps,
        true,
    )?;
    let upload_cap = configured_integer_rate(
        options,
        "adaptive_ceiling_ul_cap_kbps",
        configured_maximum_upload_kbps,
        true,
    )?;
    if minimum_download_kbps > configured_maximum_download_kbps
        || minimum_upload_kbps > configured_maximum_upload_kbps
    {
        return Err(NativeSqmAttestationError::failed(
            "managed SQM controller bounds are not ordered",
        ));
    }
    if adaptive
        && (download_cap < configured_maximum_download_kbps
            || upload_cap < configured_maximum_upload_kbps)
    {
        return Err(NativeSqmAttestationError::failed(
            "adaptive ceiling cap is below the configured controller maximum",
        ));
    }
    let policy = ManagedSqmRatePolicy {
        minimum_download_kbps,
        maximum_download_kbps: if adaptive {
            download_cap.max(configured_maximum_download_kbps)
        } else {
            configured_maximum_download_kbps
        },
        minimum_upload_kbps,
        maximum_upload_kbps: if adaptive {
            upload_cap.max(configured_maximum_upload_kbps)
        } else {
            configured_maximum_upload_kbps
        },
    };
    policy.validate()?;
    Ok(policy)
}

fn configured_integer_rate(
    options: &BTreeMap<String, String>,
    key: &str,
    default: u64,
    allow_zero: bool,
) -> Result<u64, NativeSqmAttestationError> {
    let Some(raw) = options.get(key) else {
        return Ok(default);
    };
    let value = raw.parse::<u64>().map_err(|_| {
        NativeSqmAttestationError::failed(format!(
            "managed SQM option {key} is not an unsigned decimal integer"
        ))
    })?;
    if (!allow_zero && value == 0) || value > rate_limits::MAX_RATE_KBPS {
        return Err(NativeSqmAttestationError::failed(format!(
            "managed SQM option {key} is not a bounded integer kbit/s value"
        )));
    }
    Ok(value)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManagedSqmStopSpec {
    pub instance: String,
    pub sqm_section: String,
    pub target_interface: String,
    pub download_interface: String,
    pub rate_policy: Option<ManagedSqmRatePolicy>,
}

impl ManagedSqmStopSpec {
    fn validate(&self) -> Result<(), NativeSqmAttestationError> {
        validate_uci_name(&self.instance, "stop instance")?;
        validate_uci_name(&self.sqm_section, "stop SQM section")?;
        validate_interface(&self.target_interface, "stop target interface")?;
        validate_interface(&self.download_interface, "stop download interface")?;
        if let Some(policy) = &self.rate_policy {
            policy.validate()?;
        }
        Ok(())
    }
}

impl ManagedSqmAttestationSpec {
    fn validate(&self) -> Result<(), NativeSqmAttestationError> {
        validate_uci_name(&self.instance, "instance")?;
        validate_uci_name(&self.sqm_section, "SQM section")?;
        for (value, label) in [
            (&self.target_interface, "target interface"),
            (&self.upload_interface, "upload interface"),
            (&self.download_interface, "download interface"),
        ] {
            validate_interface(value, label)?;
        }
        if !matches!(
            self.direction_mode.as_str(),
            "both" | "download_only" | "upload_only"
        ) {
            return Err(NativeSqmAttestationError::failed(
                "managed SQM attestation received an invalid direction mode",
            ));
        }
        let download = self.download_enabled();
        let upload = self.upload_enabled();
        if download
            && (self.minimum_download_kbps == 0
                || self.minimum_download_kbps > self.maximum_download_kbps)
        {
            return Err(NativeSqmAttestationError::failed(
                "managed SQM download bounds are invalid",
            ));
        }
        if upload
            && (self.minimum_upload_kbps == 0
                || self.minimum_upload_kbps > self.maximum_upload_kbps)
        {
            return Err(NativeSqmAttestationError::failed(
                "managed SQM upload bounds are invalid",
            ));
        }
        Ok(())
    }

    fn download_enabled(&self) -> bool {
        matches!(self.direction_mode.as_str(), "both" | "download_only")
    }

    fn upload_enabled(&self) -> bool {
        matches!(self.direction_mode.as_str(), "both" | "upload_only")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NativeSqmAttestationError {
    Busy(String),
    Terminated,
    Failed(String),
}

impl NativeSqmAttestationError {
    fn failed(message: impl Into<String>) -> Self {
        Self::Failed(message.into())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct OpenWrtPaths {
    pub(crate) lock_root: PathBuf,
    pub(crate) sqm_config: PathBuf,
    pub(crate) sqm_state_root: PathBuf,
    pub(crate) sys_class_net: PathBuf,
    pub(crate) uci: PathBuf,
    pub(crate) tc: PathBuf,
    pub(crate) sqm_run: PathBuf,
}

impl OpenWrtPaths {
    pub(crate) fn from_environment() -> Self {
        Self {
            lock_root: std::env::var_os("CAKE_AUTORATE_RUNTIME_LOCK_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_LOCK_ROOT)),
            sqm_config: std::env::var_os("CAKE_AUTORATE_SQM_CONFIG_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_SQM_CONFIG)),
            sqm_state_root: std::env::var_os("CAKE_AUTORATE_SQM_STATE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_SQM_STATE_ROOT)),
            sys_class_net: std::env::var_os("CAKE_AUTORATE_SYS_CLASS_NET")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_SYS_CLASS_NET)),
            uci: std::env::var_os("CAKE_AUTORATE_UCI")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("uci")),
            tc: std::env::var_os("CAKE_AUTORATE_TC")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("tc")),
            sqm_run: std::env::var_os("CAKE_AUTORATE_SQM_RUN")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/usr/lib/sqm/run.sh")),
        }
    }
}

struct RuntimeLockClaim {
    global_guard: File,
    interface_guard: File,
    record_path: PathBuf,
    record_body: Vec<u8>,
}

impl Drop for RuntimeLockClaim {
    fn drop(&mut self) {
        if fs::read(&self.record_path).ok().as_deref() == Some(self.record_body.as_slice()) {
            let _ = fs::remove_file(&self.record_path);
        }
        unsafe {
            libc::flock(self.interface_guard.as_raw_fd(), libc::LOCK_UN);
            libc::flock(self.global_guard.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

struct PrivateSqmSnapshot {
    directory: PathBuf,
    live_cake: Vec<u8>,
    live_sqm: Vec<u8>,
    private_sqm: Vec<u8>,
}

struct PrivateSqmStopSnapshot {
    directory: PathBuf,
    source_bytes: Vec<u8>,
    live_cake: Option<Vec<u8>>,
    live_sqm: Vec<u8>,
    private_sqm: Vec<u8>,
}

struct PreparedManagedSqm {
    _locks: RuntimeLockClaim,
    snapshot: PrivateSqmSnapshot,
    sqm: ParsedUciSection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UciListPolicy {
    Allow,
    Reject,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ParsedUciSection {
    scalars: BTreeMap<String, String>,
    lists: BTreeMap<String, Vec<String>>,
}

impl Drop for PrivateSqmSnapshot {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.directory.join("sqm"));
        let _ = fs::remove_dir(&self.directory);
    }
}

impl Drop for PrivateSqmStopSnapshot {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.directory.join("sqm"));
        let _ = fs::remove_dir(&self.directory);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct SqmRuntimeIdentity {
    interface: String,
    qdisc: String,
    script: String,
    upload_kbps: u64,
    download_kbps: u64,
    linklayer: String,
    linklayer_adaptation: String,
    overhead: i64,
    mpu: u64,
    use_mq: bool,
    ingress_cake_opts: String,
    egress_cake_opts: String,
    ingress_qdisc_opts: String,
    egress_qdisc_opts: String,
    squash_dscp: bool,
    squash_ingress: bool,
}

pub(crate) fn attest_managed_sqm(
    spec: &ManagedSqmAttestationSpec,
) -> Result<(), NativeSqmAttestationError> {
    attest_managed_sqm_with_paths(spec, &OpenWrtPaths::from_environment())
}

/// Attest a just-started managed SQM runtime while the caller already owns the
/// global lifecycle mutation lock.  Acquiring that lock again would deadlock;
/// every other proof performed by the ordinary attestor remains mandatory.
#[cfg(feature = "calibration")]
pub(crate) fn attest_managed_sqm_after_service_action(
    spec: &ManagedSqmAttestationSpec,
) -> Result<(), NativeSqmAttestationError> {
    attest_managed_sqm_after_service_action_with_paths(spec, &OpenWrtPaths::from_environment())
}

/// Attest the ordinary service-start postcondition. A configured controller
/// whose exact upload/target interface is currently absent may be deferred to
/// hotplug, but only after its surviving download IFB/state is proven exact or
/// the entire managed runtime is proven absent. The strict online attestor
/// above remains unchanged for every caller that requires a live SQM runtime.
pub(crate) fn attest_managed_sqm_after_service_action_or_offline(
    spec: &ManagedSqmAttestationSpec,
) -> Result<bool, NativeSqmAttestationError> {
    attest_managed_sqm_after_service_action_or_offline_with_paths(
        spec,
        &OpenWrtPaths::from_environment(),
    )
}

/// Stop one exactly cake-owned SQM runtime while the ordinary service
/// lifecycle already owns the global mutation lock.  The helper exit status is
/// diagnostic only: success is the exact absence of the previously attested
/// CAKE/IFB/ingress state and its owner-bound sqm-scripts state file.
pub(crate) fn stop_managed_sqm_after_service_action(
    spec: &ManagedSqmStopSpec,
) -> Result<(), NativeSqmAttestationError> {
    stop_managed_sqm_after_service_action_with_paths(spec, &OpenWrtPaths::from_environment())
}

pub(crate) fn recover_managed_sqm<F>(
    spec: &ManagedSqmAttestationSpec,
    should_cancel: F,
) -> Result<(), NativeSqmAttestationError>
where
    F: Fn() -> bool,
{
    recover_managed_sqm_with_paths(spec, &OpenWrtPaths::from_environment(), should_cancel)
}

fn attest_managed_sqm_with_paths(
    spec: &ManagedSqmAttestationSpec,
    paths: &OpenWrtPaths,
) -> Result<(), NativeSqmAttestationError> {
    let prepared = prepare_managed_sqm(spec, paths, "sqm-attest")?;
    attest_prepared_managed_sqm(spec, paths, &prepared)
}

fn attest_managed_sqm_after_service_action_with_paths(
    spec: &ManagedSqmAttestationSpec,
    paths: &OpenWrtPaths,
) -> Result<(), NativeSqmAttestationError> {
    spec.validate()?;
    let snapshot = freeze_sqm_config(spec, paths)?;
    let cake = parse_uci_show(
        &snapshot.live_cake,
        "cake-autorate",
        &spec.instance,
        "cake_autorate",
        UciListPolicy::Allow,
    )?;
    let sqm = parse_uci_show(
        &snapshot.private_sqm,
        "sqm",
        &spec.sqm_section,
        "queue",
        UciListPolicy::Reject,
    )?;
    validate_live_configuration(spec, &cake, &sqm)?;
    let identity = load_runtime_identity(spec, paths)?;
    validate_state_against_sqm(spec, &identity, &sqm)?;
    validate_kernel_topology(spec, &identity, paths)?;
    validate_configuration_unchanged(spec, paths, &snapshot)
}

fn attest_managed_sqm_after_service_action_or_offline_with_paths(
    spec: &ManagedSqmAttestationSpec,
    paths: &OpenWrtPaths,
) -> Result<bool, NativeSqmAttestationError> {
    if paths.sys_class_net.join(&spec.target_interface).exists() {
        attest_managed_sqm_after_service_action_with_paths(spec, paths)?;
        return Ok(false);
    }
    spec.validate()?;
    if spec.upload_interface != spec.target_interface {
        return Err(NativeSqmAttestationError::failed(
            "offline managed SQM target does not own its upload interface",
        ));
    }
    let snapshot = freeze_sqm_config(spec, paths)?;
    let cake = parse_uci_show(
        &snapshot.live_cake,
        "cake-autorate",
        &spec.instance,
        "cake_autorate",
        UciListPolicy::Allow,
    )?;
    let sqm = parse_uci_show(
        &snapshot.private_sqm,
        "sqm",
        &spec.sqm_section,
        "queue",
        UciListPolicy::Reject,
    )?;
    validate_live_configuration(spec, &cake, &sqm)?;
    match load_optional_runtime_identity(spec, paths)? {
        Some(state) => {
            validate_state_against_sqm(spec, &state, &sqm)?;
            validate_offline_target_topology(spec, &state, paths)?;
        }
        None => {
            attest_managed_sqm_kernel_absent(&stop_spec_from_attestation(spec), paths)?;
        }
    }
    validate_configuration_unchanged(spec, paths, &snapshot)?;
    Ok(true)
}

fn stop_managed_sqm_after_service_action_with_paths(
    source: &ManagedSqmStopSpec,
    paths: &OpenWrtPaths,
) -> Result<(), NativeSqmAttestationError> {
    source.validate()?;
    let snapshot = freeze_stop_sqm_config(source, paths)?;
    let sqm = parse_uci_show(
        &snapshot.private_sqm,
        "sqm",
        &source.sqm_section,
        "queue",
        UciListPolicy::Reject,
    )?;
    if let Some(expected_policy) = &source.rate_policy {
        let cake = parse_uci_show(
            snapshot.live_cake.as_deref().ok_or_else(|| {
                NativeSqmAttestationError::failed(
                    "managed SQM stop has no frozen controller section",
                )
            })?,
            "cake-autorate",
            &source.instance,
            "cake_autorate",
            UciListPolicy::Allow,
        )?;
        if managed_sqm_rate_policy_from_options(&cake.scalars)? != *expected_policy {
            return Err(NativeSqmAttestationError::failed(
                "managed SQM controller rate policy changed before stop",
            ));
        }
    }
    if required_uci_value(&sqm, "_cake_autorate_managed")? != source.instance
        || required_uci_value(&sqm, "interface")? != source.target_interface
    {
        return Err(NativeSqmAttestationError::failed(
            "managed SQM stop ownership changed",
        ));
    }
    let Some(attestation) = stop_attestation_spec(source, &sqm)? else {
        attest_managed_sqm_absent(source, paths)?;
        validate_stop_configuration_unchanged(source, paths, &snapshot)?;
        return Ok(());
    };
    let prior_state = load_optional_runtime_identity(&attestation, paths)?;
    match prior_state.as_ref() {
        Some(state) => {
            validate_state_against_sqm(&attestation, state, &sqm)?;
            if paths
                .sys_class_net
                .join(&attestation.target_interface)
                .exists()
            {
                if let Err(runtime_error) = validate_kernel_topology(&attestation, state, paths) {
                    if attest_managed_sqm_kernel_absent(source, paths).is_err() {
                        return Err(runtime_error);
                    }
                    validate_stop_configuration_unchanged(source, paths, &snapshot)?;
                    remove_runtime_state_if_present(&attestation, paths, &sqm)?;
                    attest_managed_sqm_absent(source, paths)?;
                    return validate_stop_configuration_unchanged(source, paths, &snapshot);
                }
            } else {
                match validate_offline_target_topology(&attestation, state, paths) {
                    Ok(true) => {}
                    Ok(false) => {
                        validate_stop_configuration_unchanged(source, paths, &snapshot)?;
                        remove_runtime_state_if_present(&attestation, paths, &sqm)?;
                        attest_managed_sqm_absent(source, paths)?;
                        return validate_stop_configuration_unchanged(source, paths, &snapshot);
                    }
                    Err(runtime_error) => {
                        if attest_managed_sqm_kernel_absent(source, paths).is_err() {
                            return Err(runtime_error);
                        }
                        validate_stop_configuration_unchanged(source, paths, &snapshot)?;
                        remove_runtime_state_if_present(&attestation, paths, &sqm)?;
                        attest_managed_sqm_absent(source, paths)?;
                        return validate_stop_configuration_unchanged(source, paths, &snapshot);
                    }
                }
            }
        }
        None => {
            attest_managed_sqm_absent(source, paths)?;
            validate_stop_configuration_unchanged(source, paths, &snapshot)?;
            return Ok(());
        }
    }
    validate_stop_configuration_unchanged(source, paths, &snapshot)?;

    let _ = run_sqm_action(
        paths,
        &snapshot.directory,
        "stop",
        &source.target_interface,
        &|| false,
    )?;
    remove_runtime_state_if_present(&attestation, paths, &sqm)?;
    attest_managed_sqm_absent(source, paths)?;
    validate_stop_configuration_unchanged(source, paths, &snapshot)
}

fn stop_spec_from_attestation(spec: &ManagedSqmAttestationSpec) -> ManagedSqmStopSpec {
    ManagedSqmStopSpec {
        instance: spec.instance.clone(),
        sqm_section: spec.sqm_section.clone(),
        target_interface: spec.target_interface.clone(),
        download_interface: spec.download_interface.clone(),
        rate_policy: Some(ManagedSqmRatePolicy {
            minimum_download_kbps: spec.minimum_download_kbps,
            maximum_download_kbps: spec.maximum_download_kbps,
            minimum_upload_kbps: spec.minimum_upload_kbps,
            maximum_upload_kbps: spec.maximum_upload_kbps,
        }),
    }
}

fn validate_offline_target_topology(
    spec: &ManagedSqmAttestationSpec,
    state: &SqmRuntimeIdentity,
    paths: &OpenWrtPaths,
) -> Result<bool, NativeSqmAttestationError> {
    if spec.upload_interface != spec.target_interface
        || paths.sys_class_net.join(&spec.target_interface).exists()
    {
        return Err(NativeSqmAttestationError::failed(
            "managed SQM offline topology has a live or foreign upload interface",
        ));
    }
    let download = paths.sys_class_net.join(&spec.download_interface);
    if !download.exists() {
        return Ok(false);
    }
    let counter = if spec.download_interface.starts_with("ifb")
        || spec.download_interface.starts_with("veth")
    {
        "tx_bytes"
    } else {
        "rx_bytes"
    };
    let counter = download.join("statistics").join(counter);
    if !counter.is_file() {
        return Err(NativeSqmAttestationError::failed(format!(
            "offline managed SQM download counter {} is missing",
            counter.display()
        )));
    }
    validate_cake_direction(
        paths,
        &spec.download_interface,
        spec.download_enabled(),
        spec.minimum_download_kbps,
        spec.maximum_download_kbps,
        state,
        &format!("{} {}", state.ingress_cake_opts, state.ingress_qdisc_opts),
    )?;
    Ok(true)
}

fn stop_attestation_spec(
    source: &ManagedSqmStopSpec,
    sqm: &ParsedUciSection,
) -> Result<Option<ManagedSqmAttestationSpec>, NativeSqmAttestationError> {
    let download = required_uci_value(sqm, "download")?
        .parse::<u64>()
        .map_err(|_| NativeSqmAttestationError::failed("stop SQM download rate is invalid"))?;
    let upload = required_uci_value(sqm, "upload")?
        .parse::<u64>()
        .map_err(|_| NativeSqmAttestationError::failed("stop SQM upload rate is invalid"))?;
    if download == 0 && upload == 0 {
        return Ok(None);
    }
    let direction_mode = match (download > 0, upload > 0) {
        (true, true) => "both",
        (true, false) => "download_only",
        (false, true) => "upload_only",
        // A disabled zero-rate managed section is a valid stop target only
        // when it has no runtime state or topology.  The synthetic positive
        // bounds make any observed zero-rate runtime fail the exact state
        // validation below rather than silently becoming mutation authority.
        (false, false) => "both",
    };
    let mut policy = source.rate_policy.clone().unwrap_or(ManagedSqmRatePolicy {
        minimum_download_kbps: download.max(1),
        maximum_download_kbps: download.max(1),
        minimum_upload_kbps: upload.max(1),
        maximum_upload_kbps: upload.max(1),
    });
    /* LuCI commits the new controller policy before rc.common stops the exact
     * previously managed SQM runtime.  At that boundary the frozen SQM section
     * and its root-owned runtime state remain the deletion authority, while the
     * controller interval may already describe the next start.  Extend this
     * stop-only interval by exactly the positive frozen SQM rates; the later
     * state, qdisc, IFB, ingress, option, owner and config-recheck gates still
     * require byte-for-byte ownership and reject every foreign rate/topology. */
    if download > 0 {
        policy.minimum_download_kbps = policy.minimum_download_kbps.min(download);
        policy.maximum_download_kbps = policy.maximum_download_kbps.max(download);
    }
    if upload > 0 {
        policy.minimum_upload_kbps = policy.minimum_upload_kbps.min(upload);
        policy.maximum_upload_kbps = policy.maximum_upload_kbps.max(upload);
    }
    let spec = ManagedSqmAttestationSpec {
        instance: source.instance.clone(),
        sqm_section: source.sqm_section.clone(),
        target_interface: source.target_interface.clone(),
        upload_interface: source.target_interface.clone(),
        download_interface: source.download_interface.clone(),
        direction_mode: direction_mode.to_string(),
        minimum_download_kbps: policy.minimum_download_kbps,
        maximum_download_kbps: policy.maximum_download_kbps,
        minimum_upload_kbps: policy.minimum_upload_kbps,
        maximum_upload_kbps: policy.maximum_upload_kbps,
    };
    spec.validate()?;
    Ok(Some(spec))
}

fn prepare_managed_sqm(
    spec: &ManagedSqmAttestationSpec,
    paths: &OpenWrtPaths,
    role: &str,
) -> Result<PreparedManagedSqm, NativeSqmAttestationError> {
    spec.validate()?;
    let locks = acquire_runtime_locks(paths, &spec.target_interface, role)?;
    let snapshot = freeze_sqm_config(spec, paths)?;
    let cake = parse_uci_show(
        &snapshot.live_cake,
        "cake-autorate",
        &spec.instance,
        "cake_autorate",
        UciListPolicy::Allow,
    )?;
    let sqm = parse_uci_show(
        &snapshot.private_sqm,
        "sqm",
        &spec.sqm_section,
        "queue",
        UciListPolicy::Reject,
    )?;
    validate_live_configuration(spec, &cake, &sqm)?;
    Ok(PreparedManagedSqm {
        _locks: locks,
        snapshot,
        sqm,
    })
}

fn attest_prepared_managed_sqm(
    spec: &ManagedSqmAttestationSpec,
    paths: &OpenWrtPaths,
    prepared: &PreparedManagedSqm,
) -> Result<(), NativeSqmAttestationError> {
    let identity = load_runtime_identity(spec, paths)?;
    validate_state_against_sqm(spec, &identity, &prepared.sqm)?;
    validate_kernel_topology(spec, &identity, paths)?;
    validate_configuration_unchanged(spec, paths, &prepared.snapshot)
}

fn validate_configuration_unchanged(
    spec: &ManagedSqmAttestationSpec,
    paths: &OpenWrtPaths,
    snapshot: &PrivateSqmSnapshot,
) -> Result<(), NativeSqmAttestationError> {
    let final_cake = uci_show(paths, "cake-autorate", &spec.instance, None)?;
    let final_sqm = uci_show(paths, "sqm", &spec.sqm_section, None)?;
    if final_cake != snapshot.live_cake || final_sqm != snapshot.live_sqm {
        return Err(NativeSqmAttestationError::failed(
            "managed SQM configuration changed during native attestation",
        ));
    }
    Ok(())
}

fn recover_managed_sqm_with_paths<F>(
    spec: &ManagedSqmAttestationSpec,
    paths: &OpenWrtPaths,
    should_cancel: F,
) -> Result<(), NativeSqmAttestationError>
where
    F: Fn() -> bool,
{
    let prepared = prepare_managed_sqm(spec, paths, "sqm-recover")?;
    if attest_prepared_managed_sqm(spec, paths, &prepared).is_ok() {
        return Ok(());
    }

    let prior_state = load_optional_runtime_identity(spec, paths)?;
    if let Some(identity) = prior_state.as_ref() {
        validate_state_against_sqm(spec, identity, &prepared.sqm)?;
    }
    validate_configuration_unchanged(spec, paths, &prepared.snapshot)?;

    if prior_state.is_some() {
        // sqm-scripts stop has historically been best-effort.  It may report
        // a non-zero status after removing the exact state it owned; process
        // timeout/cancellation remains fatal because ownership is then
        // unknown.  The state file is re-attested before any explicit unlink.
        let _ = run_sqm_action(
            paths,
            &prepared.snapshot.directory,
            "stop",
            &spec.target_interface,
            &should_cancel,
        )?;
    }
    remove_runtime_state_if_present(spec, paths, &prepared.sqm)?;
    validate_configuration_unchanged(spec, paths, &prepared.snapshot)?;

    let start = run_sqm_action(
        paths,
        &prepared.snapshot.directory,
        "start",
        &spec.target_interface,
        &should_cancel,
    )?;
    match attest_prepared_managed_sqm(spec, paths, &prepared) {
        Ok(()) => Ok(()),
        Err(postcondition) if !start.status.success() => {
            let detail = String::from_utf8_lossy(&start.stderr).trim().to_string();
            Err(NativeSqmAttestationError::failed(if detail.is_empty() {
                format!(
                    "managed SQM start failed with {}; exact postcondition: {}",
                    start.status,
                    error_message(&postcondition)
                )
            } else {
                format!(
                    "managed SQM start failed: {detail}; exact postcondition: {}",
                    error_message(&postcondition)
                )
            }))
        }
        Err(postcondition) => Err(NativeSqmAttestationError::failed(format!(
            "managed SQM start completed but its exact CAKE/IFB postcondition is not yet observable: {}",
            error_message(&postcondition)
        ))),
    }
}

pub(crate) fn error_message(error: &NativeSqmAttestationError) -> &str {
    match error {
        NativeSqmAttestationError::Busy(message) | NativeSqmAttestationError::Failed(message) => {
            message
        }
        NativeSqmAttestationError::Terminated => "termination requested",
    }
}

fn acquire_runtime_locks(
    paths: &OpenWrtPaths,
    target: &str,
    role: &str,
) -> Result<RuntimeLockClaim, NativeSqmAttestationError> {
    if !safe_token(role) {
        return Err(NativeSqmAttestationError::failed(
            "managed SQM runtime-lock role is unsafe",
        ));
    }
    ensure_private_directory(&paths.lock_root)?;
    let global_path = paths.lock_root.join("runtime.guard");
    let global_guard = open_guard(&global_path)?;
    flock(&global_guard, libc::LOCK_SH | libc::LOCK_NB).map_err(|error| {
        if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
            NativeSqmAttestationError::Busy(
                "managed SQM attestation deferred while another runtime operation owns the global lock"
                    .to_string(),
            )
        } else {
            NativeSqmAttestationError::failed(format!(
                "unable to acquire the global managed-SQM lock: {error}"
            ))
        }
    })?;
    let stem = interface_lock_stem(target)?;
    let record_path = paths.lock_root.join(format!("interface-{stem}.lock"));
    let interface_guard =
        open_guard(&paths.lock_root.join(format!("interface-{stem}.lock.guard")))?;
    flock(&interface_guard, libc::LOCK_EX | libc::LOCK_NB).map_err(|error| {
        if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
            NativeSqmAttestationError::Busy(format!(
                "managed SQM attestation deferred while another operation owns {target}"
            ))
        } else {
            NativeSqmAttestationError::failed(format!(
                "unable to acquire the managed-SQM interface lock: {error}"
            ))
        }
    })?;
    if let Ok(existing) = read_bounded_file(&record_path, 4096) {
        let record = parse_lock_record(&existing)?;
        if !record.recovery_journal.is_empty() && Path::new(&record.recovery_journal).is_file() {
            return Err(NativeSqmAttestationError::Busy(format!(
                "managed SQM attestation deferred for active recovery journal {}",
                record.recovery_journal
            )));
        }
    } else if record_path.exists() {
        return Err(NativeSqmAttestationError::failed(
            "managed-SQM interface lock record is unsafe or malformed",
        ));
    }
    let identity = current_process_identity()?;
    let token = random_token()?;
    let record_body = format!(
        "version=1\npid={}\nproc_starttime={}\nrole={}\ntoken={}\nrecovery_journal=\n",
        identity.0, identity.1, role, token
    )
    .into_bytes();
    atomic_replace_private(&record_path, &record_body)?;
    Ok(RuntimeLockClaim {
        global_guard,
        interface_guard,
        record_path,
        record_body,
    })
}

#[derive(Debug)]
struct LockRecord {
    recovery_journal: String,
}

fn parse_lock_record(bytes: &[u8]) -> Result<LockRecord, NativeSqmAttestationError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| NativeSqmAttestationError::failed("runtime lock record is not UTF-8"))?;
    let mut fields = BTreeMap::new();
    for line in text.lines() {
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| NativeSqmAttestationError::failed("runtime lock record is malformed"))?;
        if fields.insert(key, value).is_some() {
            return Err(NativeSqmAttestationError::failed(
                "runtime lock record contains duplicate fields",
            ));
        }
    }
    let exact = [
        "version",
        "pid",
        "proc_starttime",
        "role",
        "token",
        "recovery_journal",
    ];
    if fields.len() != exact.len() || exact.iter().any(|key| !fields.contains_key(key)) {
        return Err(NativeSqmAttestationError::failed(
            "runtime lock record has an unknown field set",
        ));
    }
    if fields["version"] != "1"
        || fields["pid"]
            .parse::<u32>()
            .ok()
            .filter(|value| *value > 1)
            .is_none()
        || fields["proc_starttime"]
            .parse::<u64>()
            .ok()
            .filter(|value| *value > 0)
            .is_none()
        || !safe_token(fields["role"])
        || !safe_token(fields["token"])
    {
        return Err(NativeSqmAttestationError::failed(
            "runtime lock record identity is invalid",
        ));
    }
    let recovery_journal = fields["recovery_journal"].to_string();
    if !recovery_journal.is_empty() && !safe_ram_path(Path::new(&recovery_journal)) {
        return Err(NativeSqmAttestationError::failed(
            "runtime lock recovery journal path is unsafe",
        ));
    }
    Ok(LockRecord { recovery_journal })
}

fn freeze_sqm_config(
    spec: &ManagedSqmAttestationSpec,
    paths: &OpenWrtPaths,
) -> Result<PrivateSqmSnapshot, NativeSqmAttestationError> {
    let live_cake = uci_show(paths, "cake-autorate", &spec.instance, None)?;
    let live_sqm = uci_show(paths, "sqm", &spec.sqm_section, None)?;
    require_external_regular(&paths.sqm_config, 1024 * 1024)?;
    let token = random_token()?;
    let directory = paths
        .lock_root
        .join(format!("sqm-attest-{}-{token}", std::process::id()));
    fs::create_dir(&directory).map_err(|error| {
        NativeSqmAttestationError::failed(format!("unable to create private SQM snapshot: {error}"))
    })?;
    let mut snapshot = PrivateSqmSnapshot {
        directory,
        live_cake,
        live_sqm,
        private_sqm: Vec::new(),
    };
    fs::set_permissions(&snapshot.directory, fs::Permissions::from_mode(0o700)).map_err(
        |error| {
            NativeSqmAttestationError::failed(format!(
                "unable to protect private SQM snapshot: {error}"
            ))
        },
    )?;
    let source = read_bounded_external_file(&paths.sqm_config, 1024 * 1024)?;
    let target = snapshot.directory.join("sqm");
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&target)
        .map_err(|error| {
            NativeSqmAttestationError::failed(format!(
                "unable to create private SQM config: {error}"
            ))
        })?;
    output.write_all(&source).map_err(|error| {
        NativeSqmAttestationError::failed(format!("unable to freeze private SQM config: {error}"))
    })?;
    output.sync_all().map_err(|error| {
        NativeSqmAttestationError::failed(format!("unable to sync private SQM config: {error}"))
    })?;
    drop(output);
    require_external_regular(&paths.sqm_config, 1024 * 1024)?;
    snapshot.private_sqm = uci_show(paths, "sqm", &spec.sqm_section, Some(&snapshot.directory))?;
    parse_uci_show(
        &snapshot.private_sqm,
        "sqm",
        &spec.sqm_section,
        "queue",
        UciListPolicy::Reject,
    )
    .map_err(|_| {
        NativeSqmAttestationError::failed("private managed SQM snapshot is not canonical")
    })?;
    Ok(snapshot)
}

fn freeze_stop_sqm_config(
    spec: &ManagedSqmStopSpec,
    paths: &OpenWrtPaths,
) -> Result<PrivateSqmStopSnapshot, NativeSqmAttestationError> {
    let live_cake = if spec.rate_policy.is_some() {
        Some(uci_show(paths, "cake-autorate", &spec.instance, None)?)
    } else {
        None
    };
    let live_sqm = uci_show(paths, "sqm", &spec.sqm_section, None)?;
    require_external_regular(&paths.sqm_config, 1024 * 1024)?;
    let source_bytes = read_bounded_external_file(&paths.sqm_config, 1024 * 1024)?;
    let token = random_token()?;
    let directory = paths
        .lock_root
        .join(format!("sqm-stop-{}-{token}", std::process::id()));
    fs::create_dir(&directory).map_err(|error| {
        NativeSqmAttestationError::failed(format!(
            "unable to create private SQM stop snapshot: {error}"
        ))
    })?;
    let mut snapshot = PrivateSqmStopSnapshot {
        directory,
        source_bytes,
        live_cake,
        live_sqm,
        private_sqm: Vec::new(),
    };
    fs::set_permissions(&snapshot.directory, fs::Permissions::from_mode(0o700)).map_err(
        |error| {
            NativeSqmAttestationError::failed(format!(
                "unable to protect private SQM stop snapshot: {error}"
            ))
        },
    )?;
    let target = snapshot.directory.join("sqm");
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&target)
        .map_err(|error| {
            NativeSqmAttestationError::failed(format!(
                "unable to create private SQM stop config: {error}"
            ))
        })?;
    output.write_all(&snapshot.source_bytes).map_err(|error| {
        NativeSqmAttestationError::failed(format!(
            "unable to freeze private SQM stop config: {error}"
        ))
    })?;
    output.sync_all().map_err(|error| {
        NativeSqmAttestationError::failed(format!(
            "unable to sync private SQM stop config: {error}"
        ))
    })?;
    drop(output);
    snapshot.private_sqm = uci_show(paths, "sqm", &spec.sqm_section, Some(&snapshot.directory))?;
    if snapshot.private_sqm != snapshot.live_sqm {
        return Err(NativeSqmAttestationError::failed(
            "managed SQM stop refuses staged or uncommitted section state",
        ));
    }
    Ok(snapshot)
}

fn validate_stop_configuration_unchanged(
    spec: &ManagedSqmStopSpec,
    paths: &OpenWrtPaths,
    snapshot: &PrivateSqmStopSnapshot,
) -> Result<(), NativeSqmAttestationError> {
    let final_cake = if snapshot.live_cake.is_some() {
        Some(uci_show(paths, "cake-autorate", &spec.instance, None)?)
    } else {
        None
    };
    let final_sqm = uci_show(paths, "sqm", &spec.sqm_section, None)?;
    if final_cake != snapshot.live_cake
        || final_sqm != snapshot.live_sqm
        || read_bounded_external_file(&paths.sqm_config, 1024 * 1024)? != snapshot.source_bytes
    {
        return Err(NativeSqmAttestationError::failed(
            "managed SQM configuration changed during native stop",
        ));
    }
    Ok(())
}

fn attest_managed_sqm_absent(
    spec: &ManagedSqmStopSpec,
    paths: &OpenWrtPaths,
) -> Result<(), NativeSqmAttestationError> {
    let state = paths
        .sqm_state_root
        .join(format!("{}.state", spec.target_interface));
    if state.exists() {
        return Err(NativeSqmAttestationError::failed(
            "managed SQM runtime state remains after stop",
        ));
    }
    attest_managed_sqm_kernel_absent(spec, paths)
}

fn attest_managed_sqm_kernel_absent(
    spec: &ManagedSqmStopSpec,
    paths: &OpenWrtPaths,
) -> Result<(), NativeSqmAttestationError> {
    if paths.sys_class_net.join(&spec.target_interface).exists() {
        let qdisc = tc_output(paths, &["qdisc", "show", "dev", &spec.target_interface])?;
        if String::from_utf8_lossy(&qdisc).lines().any(|line| {
            let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
            fields.first() == Some(&"qdisc")
                && matches!(
                    fields.get(1),
                    Some(&"cake") | Some(&"cake_mq") | Some(&"ingress") | Some(&"clsact")
                )
        }) {
            return Err(NativeSqmAttestationError::failed(
                "managed SQM upload or ingress runtime remains after stop",
            ));
        }
    }
    if paths.sys_class_net.join(&spec.download_interface).exists() {
        let qdisc = tc_output(paths, &["qdisc", "show", "dev", &spec.download_interface])?;
        if String::from_utf8_lossy(&qdisc).lines().any(|line| {
            let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
            fields.first() == Some(&"qdisc")
                && matches!(fields.get(1), Some(&"cake") | Some(&"cake_mq"))
        }) {
            return Err(NativeSqmAttestationError::failed(
                "managed SQM download runtime remains after stop",
            ));
        }
    }
    Ok(())
}

fn validate_live_configuration(
    spec: &ManagedSqmAttestationSpec,
    cake: &ParsedUciSection,
    sqm: &ParsedUciSection,
) -> Result<(), NativeSqmAttestationError> {
    for key in ["enabled", "manage_sqm", "sqm_enabled"] {
        if required_uci_value(cake, key)? != "1" {
            return Err(NativeSqmAttestationError::failed(format!(
                "managed SQM option {key} is disabled"
            )));
        }
    }
    let sqm_interface = optional_uci_value(cake, "sqm_interface")?;
    let configured_ul = optional_uci_value(cake, "ul_if")?;
    let cake_target = sqm_interface.or(configured_ul).unwrap_or_default();
    let cake_ul = configured_ul.unwrap_or(cake_target);
    let derived_dl = format!("ifb4{cake_target}");
    let cake_dl = optional_uci_value(cake, "dl_if")?.unwrap_or(&derived_dl);
    let derived_section = format!("cake_{}", spec.instance);
    let cake_section = optional_uci_value(cake, "sqm_section")?.unwrap_or(&derived_section);
    let direction = optional_uci_value(cake, "sqm_direction_mode")?.unwrap_or("both");
    if cake_target != spec.target_interface
        || cake_ul != spec.upload_interface
        || cake_dl != spec.download_interface
        || cake_section != spec.sqm_section
        || direction != spec.direction_mode
        || required_uci_value(sqm, "_cake_autorate_managed")? != spec.instance
        || required_uci_value(sqm, "enabled")? != "1"
        || required_uci_value(sqm, "interface")? != spec.target_interface
    {
        return Err(NativeSqmAttestationError::failed(
            "managed SQM ownership or direction configuration changed",
        ));
    }
    Ok(())
}

fn load_runtime_identity(
    spec: &ManagedSqmAttestationSpec,
    paths: &OpenWrtPaths,
) -> Result<SqmRuntimeIdentity, NativeSqmAttestationError> {
    load_optional_runtime_identity(spec, paths)?
        .ok_or_else(|| NativeSqmAttestationError::failed("managed SQM runtime state is missing"))
}

fn load_optional_runtime_identity(
    spec: &ManagedSqmAttestationSpec,
    paths: &OpenWrtPaths,
) -> Result<Option<SqmRuntimeIdentity>, NativeSqmAttestationError> {
    let path = paths
        .sqm_state_root
        .join(format!("{}.state", spec.target_interface));
    let Some(file) = open_optional_bounded_external_regular(&path, MAX_STATE_BYTES)? else {
        return Ok(None);
    };
    let bytes = read_bounded_open_file(file, &path, MAX_STATE_BYTES)?;
    parse_runtime_identity(&bytes).map(Some)
}

fn remove_runtime_state_if_present(
    spec: &ManagedSqmAttestationSpec,
    paths: &OpenWrtPaths,
    sqm: &ParsedUciSection,
) -> Result<(), NativeSqmAttestationError> {
    let path = paths
        .sqm_state_root
        .join(format!("{}.state", spec.target_interface));
    let Some(file) = open_optional_bounded_external_regular(&path, MAX_STATE_BYTES)? else {
        return Ok(());
    };
    let opened = file.metadata().map_err(|error| {
        NativeSqmAttestationError::failed(format!(
            "unable to inspect opened SQM runtime state {}: {error}",
            path.display()
        ))
    })?;
    let bytes = read_bounded_open_file(file, &path, MAX_STATE_BYTES)?;
    let identity = parse_runtime_identity(&bytes)?;
    validate_state_against_sqm(spec, &identity, sqm)?;
    let current = fs::symlink_metadata(&path).map_err(|error| {
        NativeSqmAttestationError::failed(format!(
            "unable to re-attest SQM runtime state {} before removal: {error}",
            path.display()
        ))
    })?;
    if current.file_type().is_symlink()
        || !current.file_type().is_file()
        || current.dev() != opened.dev()
        || current.ino() != opened.ino()
    {
        return Err(NativeSqmAttestationError::failed(
            "SQM runtime state identity changed before removal",
        ));
    }
    fs::remove_file(&path).map_err(|error| {
        NativeSqmAttestationError::failed(format!(
            "unable to remove exact stale SQM runtime state {}: {error}",
            path.display()
        ))
    })
}

fn parse_runtime_identity(bytes: &[u8]) -> Result<SqmRuntimeIdentity, NativeSqmAttestationError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| NativeSqmAttestationError::failed("SQM runtime state is not UTF-8"))?;
    let mut values = BTreeMap::new();
    for line in text.lines().filter(|line| !line.is_empty()) {
        let (key, quoted) = line
            .split_once('=')
            .ok_or_else(|| NativeSqmAttestationError::failed("SQM runtime state is malformed"))?;
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(NativeSqmAttestationError::failed(
                "SQM runtime state key is unsafe",
            ));
        }
        let value = quoted
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .ok_or_else(|| {
                NativeSqmAttestationError::failed("SQM runtime state value is not quoted")
            })?;
        if value
            .bytes()
            .any(|byte| !(0x20..=0x7e).contains(&byte) || byte == b'"')
        {
            return Err(NativeSqmAttestationError::failed(
                "SQM runtime state value is unsafe",
            ));
        }
        if values.insert(key, value).is_some() {
            return Err(NativeSqmAttestationError::failed(
                "SQM runtime state has duplicate keys",
            ));
        }
    }
    let get = |key: &str| values.get(key).copied().unwrap_or_default().to_string();
    let uint = |key: &str| {
        values
            .get(key)
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| {
                NativeSqmAttestationError::failed(format!("SQM runtime state {key} is invalid"))
            })
    };
    let boolean = |key: &str| match values.get(key).copied() {
        Some("0") => Ok(false),
        Some("1") => Ok(true),
        _ => Err(NativeSqmAttestationError::failed(format!(
            "SQM runtime state {key} is invalid"
        ))),
    };
    Ok(SqmRuntimeIdentity {
        interface: get("IFACE"),
        qdisc: get("QDISC"),
        script: get("SCRIPT"),
        upload_kbps: uint("UPLINK")?,
        download_kbps: uint("DOWNLINK")?,
        linklayer: get("LINKLAYER"),
        linklayer_adaptation: get("LLAM"),
        overhead: values
            .get("OVERHEAD")
            .and_then(|value| value.parse::<i64>().ok())
            .ok_or_else(|| {
                NativeSqmAttestationError::failed("SQM runtime state OVERHEAD is invalid")
            })?,
        mpu: uint("STAB_MPU")?,
        use_mq: boolean("USE_MQ")?,
        ingress_cake_opts: get("INGRESS_CAKE_OPTS"),
        egress_cake_opts: get("EGRESS_CAKE_OPTS"),
        ingress_qdisc_opts: get("IQDISC_OPTS"),
        egress_qdisc_opts: get("EQDISC_OPTS"),
        squash_dscp: boolean("ZERO_DSCP_INGRESS")?,
        squash_ingress: boolean("IGNORE_DSCP_INGRESS")?,
    })
}

fn validate_state_against_sqm(
    spec: &ManagedSqmAttestationSpec,
    state: &SqmRuntimeIdentity,
    sqm: &ParsedUciSection,
) -> Result<(), NativeSqmAttestationError> {
    let expected_download = optional_uci_value(sqm, "download")?
        .unwrap_or("")
        .parse::<u64>()
        .unwrap_or(u64::MAX);
    let expected_upload = optional_uci_value(sqm, "upload")?
        .unwrap_or("")
        .parse::<u64>()
        .unwrap_or(u64::MAX);
    let script = optional_uci_value(sqm, "script")?.unwrap_or("");
    let linklayer = optional_uci_value(sqm, "linklayer")?.unwrap_or("none");
    let linklayer_adaptation =
        optional_uci_value(sqm, "linklayer_adaptation_mechanism")?.unwrap_or("default");
    let overhead = optional_uci_value(sqm, "overhead")?.unwrap_or("0");
    let mpu = optional_uci_value(sqm, "tcMPU")?.unwrap_or("0");
    let use_mq = optional_uci_value(sqm, "use_mq")?.unwrap_or("0");
    let ingress_qdisc_opts = optional_uci_value(sqm, "iqdisc_opts")?.unwrap_or("");
    let egress_qdisc_opts = optional_uci_value(sqm, "eqdisc_opts")?.unwrap_or("");
    let squash_dscp = optional_uci_value(sqm, "squash_dscp")?.unwrap_or("0");
    let squash_ingress = optional_uci_value(sqm, "squash_ingress")?.unwrap_or("0");
    if state.interface != spec.target_interface
        || !matches!(state.qdisc.as_str(), "cake" | "cake_mq")
        || state.script != script
        || state.download_kbps != expected_download
        || state.upload_kbps != expected_upload
        || state.linklayer != linklayer
        || state.linklayer_adaptation != linklayer_adaptation
        || state.overhead.to_string() != overhead
        || state.mpu.to_string() != mpu
        || state.use_mq != (use_mq == "1")
        || state.ingress_qdisc_opts != ingress_qdisc_opts
        || state.egress_qdisc_opts != egress_qdisc_opts
        || state.squash_dscp != (squash_dscp == "1")
        || state.squash_ingress != (squash_ingress == "1")
    {
        return Err(NativeSqmAttestationError::failed(
            "SQM runtime state does not match the frozen managed section",
        ));
    }
    if spec.download_enabled() {
        if !(spec.minimum_download_kbps..=spec.maximum_download_kbps).contains(&state.download_kbps)
        {
            return Err(NativeSqmAttestationError::failed(
                "managed SQM download rate is outside configured bounds",
            ));
        }
    } else if state.download_kbps != 0 {
        return Err(NativeSqmAttestationError::failed(
            "disabled managed SQM download direction has a non-zero runtime rate",
        ));
    }
    if spec.upload_enabled() {
        if !(spec.minimum_upload_kbps..=spec.maximum_upload_kbps).contains(&state.upload_kbps) {
            return Err(NativeSqmAttestationError::failed(
                "managed SQM upload rate is outside configured bounds",
            ));
        }
    } else if state.upload_kbps != 0 {
        return Err(NativeSqmAttestationError::failed(
            "disabled managed SQM upload direction has a non-zero runtime rate",
        ));
    }
    Ok(())
}

fn validate_kernel_topology(
    spec: &ManagedSqmAttestationSpec,
    state: &SqmRuntimeIdentity,
    paths: &OpenWrtPaths,
) -> Result<(), NativeSqmAttestationError> {
    let download_counter = if spec.download_enabled() {
        if spec.download_interface.starts_with("ifb") || spec.download_interface.starts_with("veth")
        {
            "tx_bytes"
        } else {
            "rx_bytes"
        }
    } else {
        "rx_bytes"
    };
    let download_device = if spec.download_enabled() {
        &spec.download_interface
    } else {
        &spec.target_interface
    };
    let upload_counter =
        if spec.upload_interface.starts_with("ifb") || spec.upload_interface.starts_with("veth") {
            "rx_bytes"
        } else {
            "tx_bytes"
        };
    for path in [
        paths
            .sys_class_net
            .join(download_device)
            .join("statistics")
            .join(download_counter),
        paths
            .sys_class_net
            .join(&spec.upload_interface)
            .join("statistics")
            .join(upload_counter),
    ] {
        if !path.is_file() {
            return Err(NativeSqmAttestationError::failed(format!(
                "managed SQM counter {} is missing",
                path.display()
            )));
        }
    }
    validate_cake_direction(
        paths,
        &spec.download_interface,
        spec.download_enabled(),
        spec.minimum_download_kbps,
        spec.maximum_download_kbps,
        state,
        &format!("{} {}", state.ingress_cake_opts, state.ingress_qdisc_opts),
    )?;
    validate_cake_direction(
        paths,
        &spec.upload_interface,
        spec.upload_enabled(),
        spec.minimum_upload_kbps,
        spec.maximum_upload_kbps,
        state,
        &format!("{} {}", state.egress_cake_opts, state.egress_qdisc_opts),
    )?;
    if spec.download_interface.starts_with("ifb") {
        let qdisc = tc_output(
            paths,
            &["-details", "qdisc", "show", "dev", &spec.target_interface],
        )?;
        let filters = tc_output(
            paths,
            &["filter", "show", "dev", &spec.target_interface, "ingress"],
        )?;
        let ingress_count = String::from_utf8_lossy(&qdisc)
            .lines()
            .filter(|line| {
                let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
                fields.first() == Some(&"qdisc")
                    && fields.get(1) == Some(&"ingress")
                    && fields.get(2) == Some(&"ffff:")
            })
            .count();
        let redirects = crate::ingress_redirect_targets(&String::from_utf8_lossy(&filters));
        if spec.download_enabled() {
            if ingress_count != 1
                || !redirects
                    .iter()
                    .any(|target| target == &spec.download_interface)
            {
                return Err(NativeSqmAttestationError::failed(
                    "managed SQM ingress redirect is not an exact single match",
                ));
            }
        } else if redirects
            .iter()
            .any(|target| target == &spec.download_interface)
        {
            return Err(NativeSqmAttestationError::failed(
                "disabled managed SQM download redirect remains installed",
            ));
        }
    }
    Ok(())
}

fn validate_cake_direction(
    paths: &OpenWrtPaths,
    interface: &str,
    enabled: bool,
    minimum_kbps: u64,
    maximum_kbps: u64,
    state: &SqmRuntimeIdentity,
    expected_options: &str,
) -> Result<(), NativeSqmAttestationError> {
    let output = if paths.sys_class_net.join(interface).exists() {
        tc_output(paths, &["-details", "qdisc", "show", "dev", interface])?
    } else if enabled {
        return Err(NativeSqmAttestationError::failed(format!(
            "managed SQM shaping device {interface} is missing"
        )));
    } else {
        Vec::new()
    };
    let text = String::from_utf8_lossy(&output);
    let roots = text
        .lines()
        .filter(|line| {
            let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
            fields.first() == Some(&"qdisc")
                && matches!(fields.get(1), Some(&"cake") | Some(&"cake_mq"))
                && fields.get(3) == Some(&"root")
        })
        .collect::<Vec<_>>();
    if !enabled {
        return if roots.is_empty() {
            Ok(())
        } else {
            Err(NativeSqmAttestationError::failed(format!(
                "CAKE remains on disabled managed SQM direction {interface}"
            )))
        };
    }
    let [line] = roots.as_slice() else {
        return Err(NativeSqmAttestationError::failed(format!(
            "managed SQM direction {interface} does not have exactly one root CAKE qdisc"
        )));
    };
    let (kind, rate) = crate::root_cake_qdisc(line).map_err(|error| {
        NativeSqmAttestationError::failed(format!("managed SQM CAKE identity is invalid: {error}"))
    })?;
    if !(minimum_kbps..=maximum_kbps).contains(&rate)
        || (matches!(kind, crate::CakeQdiscKind::CakeMq) && !state.use_mq)
    {
        return Err(NativeSqmAttestationError::failed(format!(
            "managed SQM CAKE rate or qdisc kind is outside the frozen policy on {interface}"
        )));
    }
    validate_cake_options(line, expected_options, state)?;
    Ok(())
}

fn validate_cake_options(
    line: &str,
    expected: &str,
    state: &SqmRuntimeIdentity,
) -> Result<(), NativeSqmAttestationError> {
    let actual = line.split_ascii_whitespace().collect::<BTreeSet<_>>();
    let expected = expected.split_ascii_whitespace().collect::<Vec<_>>();
    for (group, default) in [
        (
            &[
                "besteffort",
                "diffserv3",
                "diffserv4",
                "diffserv8",
                "precedence",
            ][..],
            None,
        ),
        (
            &[
                "triple-isolate",
                "dual-srchost",
                "dual-dsthost",
                "srchost",
                "dsthost",
                "hosts",
                "flows",
                "flowblind",
            ][..],
            Some("triple-isolate"),
        ),
        (&["nat", "nonat"][..], Some("nonat")),
        (&["wash", "nowash"][..], Some("nowash")),
        (
            &["ack-filter", "ack-filter-aggressive", "no-ack-filter"][..],
            Some("no-ack-filter"),
        ),
        (&["split-gso", "no-split-gso"][..], Some("split-gso")),
    ] {
        let option = expected
            .iter()
            .rev()
            .find(|option| group.contains(option))
            .copied()
            .or(default);
        if let Some(option) = option {
            if !actual.contains(option) {
                return Err(NativeSqmAttestationError::failed(format!(
                    "managed SQM CAKE option {option} is missing"
                )));
            }
        }
    }
    if matches!(state.linklayer_adaptation.as_str(), "cake" | "default") {
        let expected_mode = if state.linklayer == "none" {
            "raw"
        } else if state.linklayer == "atm" {
            "atm"
        } else {
            "noatm"
        };
        if !actual.contains(expected_mode)
            || value_after(line, "overhead") != Some(state.overhead.to_string())
            || (state.mpu != 0 && value_after(line, "mpu") != Some(state.mpu.to_string()))
        {
            return Err(NativeSqmAttestationError::failed(
                "managed SQM CAKE link-layer identity changed",
            ));
        }
    }
    Ok(())
}

fn value_after(line: &str, key: &str) -> Option<String> {
    let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
    fields
        .windows(2)
        .find_map(|pair| (pair[0] == key).then(|| pair[1].to_string()))
}

fn uci_show(
    paths: &OpenWrtPaths,
    package: &str,
    section: &str,
    config_dir: Option<&Path>,
) -> Result<Vec<u8>, NativeSqmAttestationError> {
    let mut args = vec![OsString::from("-q")];
    if let Some(config_dir) = config_dir {
        args.push(OsString::from("-c"));
        args.push(config_dir.as_os_str().to_os_string());
    }
    args.push(OsString::from("show"));
    args.push(OsString::from(format!("{package}.{section}")));
    run_output(&paths.uci, &args)
}

fn tc_output(paths: &OpenWrtPaths, args: &[&str]) -> Result<Vec<u8>, NativeSqmAttestationError> {
    run_output(
        &paths.tc,
        &args.iter().map(OsString::from).collect::<Vec<_>>(),
    )
}

struct ManagedCommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn run_output(program: &Path, args: &[OsString]) -> Result<Vec<u8>, NativeSqmAttestationError> {
    let output = run_bounded_command(
        program,
        args,
        &[],
        QUERY_TIMEOUT,
        MAX_COMMAND_OUTPUT,
        16 * 1024,
        &|| false,
    )?;
    require_success(program, output.status, &output.stderr)?;
    Ok(output.stdout)
}

fn run_sqm_action<F>(
    paths: &OpenWrtPaths,
    config_directory: &Path,
    action: &str,
    target: &str,
    should_cancel: &F,
) -> Result<ManagedCommandOutput, NativeSqmAttestationError>
where
    F: Fn() -> bool,
{
    if !matches!(action, "start" | "stop") {
        return Err(NativeSqmAttestationError::failed(
            "managed SQM action is unsupported",
        ));
    }
    run_bounded_command(
        &paths.sqm_run,
        &[OsString::from(action), OsString::from(target)],
        &[(
            OsString::from("UCI_CONFIG_DIR"),
            config_directory.as_os_str().to_os_string(),
        )],
        SQM_RUN_TIMEOUT,
        16 * 1024,
        16 * 1024,
        should_cancel,
    )
}

fn run_bounded_command<F>(
    program: &Path,
    args: &[OsString],
    environment: &[(OsString, OsString)],
    timeout: Duration,
    stdout_limit: usize,
    stderr_limit: usize,
    should_cancel: &F,
) -> Result<ManagedCommandOutput, NativeSqmAttestationError>
where
    F: Fn() -> bool,
{
    if timeout.is_zero() || args.len() > 16 || environment.len() > 4 {
        return Err(NativeSqmAttestationError::failed(
            "managed SQM command contract is invalid",
        ));
    }
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .envs(environment.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().map_err(|error| {
        NativeSqmAttestationError::failed(format!(
            "unable to execute {}: {error}",
            program.display()
        ))
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        NativeSqmAttestationError::failed("managed SQM command stdout is unavailable")
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        NativeSqmAttestationError::failed("managed SQM command stderr is unavailable")
    })?;
    let stdout_reader = thread::spawn(move || read_bounded_pipe(stdout, stdout_limit));
    let stderr_reader = thread::spawn(move || read_bounded_pipe(stderr, stderr_limit));
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|error| {
            NativeSqmAttestationError::failed(format!(
                "unable to inspect {}: {error}",
                program.display()
            ))
        })? {
            break status;
        }
        if should_cancel() {
            kill_command_group(&mut child);
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(NativeSqmAttestationError::Terminated);
        }
        let now = Instant::now();
        if now >= deadline {
            kill_command_group(&mut child);
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(NativeSqmAttestationError::failed(format!(
                "managed SQM command {} exceeded its watchdog deadline",
                program.display()
            )));
        }
        thread::sleep(PROCESS_WAIT_INTERVAL.min(deadline.saturating_duration_since(now)));
    };
    let stdout = stdout_reader
        .join()
        .map_err(|_| NativeSqmAttestationError::failed("managed SQM stdout reader panicked"))??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| NativeSqmAttestationError::failed("managed SQM stderr reader panicked"))??;
    Ok(ManagedCommandOutput {
        status,
        stdout,
        stderr,
    })
}

fn kill_command_group(child: &mut std::process::Child) {
    if let Ok(group) = i32::try_from(child.id()) {
        if group > 1 {
            let _ = unsafe { libc::kill(-group, libc::SIGKILL) };
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn read_bounded_pipe(
    mut pipe: impl Read,
    limit: usize,
) -> Result<Vec<u8>, NativeSqmAttestationError> {
    let mut bytes = Vec::new();
    pipe.by_ref()
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            NativeSqmAttestationError::failed(format!("unable to read command output: {error}"))
        })?;
    if bytes.len() > limit {
        return Err(NativeSqmAttestationError::failed(
            "managed SQM command output exceeds its safety bound",
        ));
    }
    Ok(bytes)
}

fn require_success(
    program: &Path,
    status: ExitStatus,
    stderr: &[u8],
) -> Result<(), NativeSqmAttestationError> {
    if status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(stderr).trim().to_string();
    Err(NativeSqmAttestationError::failed(if detail.is_empty() {
        format!("{} failed with {status}", program.display())
    } else {
        detail
    }))
}

fn parse_uci_show(
    bytes: &[u8],
    package: &str,
    section: &str,
    expected_section_type: &str,
    list_policy: UciListPolicy,
) -> Result<ParsedUciSection, NativeSqmAttestationError> {
    if bytes.len() > MAX_COMMAND_OUTPUT {
        return Err(NativeSqmAttestationError::failed(
            "UCI show output exceeds its safety bound",
        ));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|_| NativeSqmAttestationError::failed("UCI show output is not UTF-8"))?;
    let prefix = format!("{package}.{section}");
    let mut section_type = None;
    let mut values = ParsedUciSection::default();
    let mut total_value_bytes = 0usize;
    for (index, line) in text.lines().enumerate() {
        if index >= MAX_UCI_LINES {
            return Err(NativeSqmAttestationError::failed(
                "UCI show output has too many lines",
            ));
        }
        let (left, raw) = line
            .split_once('=')
            .ok_or_else(|| NativeSqmAttestationError::failed("UCI show output is malformed"))?;
        if left == prefix {
            if section_type.replace(raw.to_string()).is_some() {
                return Err(NativeSqmAttestationError::failed(
                    "UCI section type is duplicated",
                ));
            }
            continue;
        }
        let option_prefix = format!("{prefix}.");
        let option = left.strip_prefix(&option_prefix).ok_or_else(|| {
            NativeSqmAttestationError::failed("UCI show output crossed its section boundary")
        })?;
        validate_uci_name(option, "UCI option")?;
        if values.scalars.contains_key(option) || values.lists.contains_key(option) {
            return Err(NativeSqmAttestationError::failed(format!(
                "managed SQM UCI option {option} is duplicated"
            )));
        }
        let parsed = parse_uci_show_values(raw)?;
        if parsed.is_empty() {
            return Err(NativeSqmAttestationError::failed(format!(
                "managed SQM UCI option {option} has no value"
            )));
        }
        if parsed.len() > MAX_UCI_LIST_VALUES {
            return Err(NativeSqmAttestationError::failed(
                "managed SQM UCI list has too many values",
            ));
        }
        for value in &parsed {
            if value.len() > MAX_UCI_VALUE_BYTES {
                return Err(NativeSqmAttestationError::failed(format!(
                    "managed SQM UCI option {option} exceeds its value size bound"
                )));
            }
            total_value_bytes = total_value_bytes.checked_add(value.len()).ok_or_else(|| {
                NativeSqmAttestationError::failed("managed SQM UCI values overflow their bound")
            })?;
            if total_value_bytes > MAX_UCI_SECTION_VALUE_BYTES {
                return Err(NativeSqmAttestationError::failed(
                    "managed SQM UCI section exceeds its value size bound",
                ));
            }
        }
        if parsed.len() == 1 {
            values.scalars.insert(option.to_string(), parsed[0].clone());
        } else {
            if list_policy == UciListPolicy::Reject {
                return Err(NativeSqmAttestationError::failed(format!(
                    "managed SQM UCI option {option} must be scalar"
                )));
            }
            values.lists.insert(option.to_string(), parsed);
        }
    }
    match section_type.as_deref() {
        None => {
            return Err(NativeSqmAttestationError::failed(
                "managed SQM UCI section is missing",
            ));
        }
        Some(actual) if actual != expected_section_type => {
            return Err(NativeSqmAttestationError::failed(format!(
                "managed SQM UCI section has type {actual}, expected {expected_section_type}"
            )));
        }
        Some(_) => {}
    }
    Ok(values)
}

fn parse_uci_show_values(raw: &str) -> Result<Vec<String>, NativeSqmAttestationError> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    let mut token_started = false;
    let mut chars = raw.trim().chars();

    while let Some(ch) = chars.next() {
        if in_quote {
            if ch == '\'' {
                in_quote = false;
            } else {
                current.push(ch);
            }
            token_started = true;
            continue;
        }
        match ch {
            '\'' => {
                in_quote = true;
                token_started = true;
            }
            '\\' => {
                let next = chars.next().ok_or_else(|| {
                    NativeSqmAttestationError::failed("UCI show value has a dangling escape")
                })?;
                current.push(next);
                token_started = true;
            }
            value if value.is_whitespace() => {
                if token_started {
                    values.push(std::mem::take(&mut current));
                    token_started = false;
                }
            }
            value => {
                current.push(value);
                token_started = true;
            }
        }
    }
    if in_quote {
        return Err(NativeSqmAttestationError::failed(
            "UCI show value has an unterminated quote",
        ));
    }
    if token_started {
        values.push(current);
    }
    Ok(values)
}

fn optional_uci_value<'a>(
    values: &'a ParsedUciSection,
    key: &str,
) -> Result<Option<&'a str>, NativeSqmAttestationError> {
    if values.lists.contains_key(key) {
        return Err(NativeSqmAttestationError::failed(format!(
            "managed SQM option {key} must be scalar"
        )));
    }
    Ok(values.scalars.get(key).map(String::as_str))
}

fn required_uci_value<'a>(
    values: &'a ParsedUciSection,
    key: &str,
) -> Result<&'a str, NativeSqmAttestationError> {
    optional_uci_value(values, key)?.ok_or_else(|| {
        NativeSqmAttestationError::failed(format!("managed SQM option {key} is missing"))
    })
}

fn current_process_identity() -> Result<(u32, u64), NativeSqmAttestationError> {
    let pid = std::process::id();
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|error| {
        NativeSqmAttestationError::failed(format!(
            "unable to read current process identity: {error}"
        ))
    })?;
    let close = stat.rfind(')').ok_or_else(|| {
        NativeSqmAttestationError::failed("current process identity is malformed")
    })?;
    let fields = stat[close + 1..]
        .split_ascii_whitespace()
        .collect::<Vec<_>>();
    let starttime = fields
        .get(19)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| NativeSqmAttestationError::failed("current process starttime is invalid"))?;
    Ok((pid, starttime))
}

fn random_token() -> Result<String, NativeSqmAttestationError> {
    let raw = fs::read_to_string("/proc/sys/kernel/random/uuid").map_err(|error| {
        NativeSqmAttestationError::failed(format!("unable to read a kernel token: {error}"))
    })?;
    let token = raw.trim().replace('-', "");
    if token.len() != 32 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(NativeSqmAttestationError::failed(
            "kernel token is malformed",
        ));
    }
    Ok(token.to_ascii_lowercase())
}

fn ensure_private_directory(path: &Path) -> Result<(), NativeSqmAttestationError> {
    if !safe_ram_path(path) {
        return Err(NativeSqmAttestationError::failed(
            "runtime lock root is unsafe",
        ));
    }
    if !path.exists() {
        fs::create_dir(path).map_err(|error| {
            NativeSqmAttestationError::failed(format!(
                "unable to create runtime lock root: {error}"
            ))
        })?;
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        NativeSqmAttestationError::failed(format!("unable to inspect runtime lock root: {error}"))
    })?;
    if !metadata.file_type().is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(NativeSqmAttestationError::failed(
            "runtime lock root is not a private owned directory",
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        NativeSqmAttestationError::failed(format!("unable to protect runtime lock root: {error}"))
    })
}

fn open_guard(path: &Path) -> Result<File, NativeSqmAttestationError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| {
            NativeSqmAttestationError::failed(format!("unable to open {}: {error}", path.display()))
        })?;
    let metadata = file.metadata().map_err(|error| {
        NativeSqmAttestationError::failed(format!("unable to inspect {}: {error}", path.display()))
    })?;
    if !metadata.file_type().is_file() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(NativeSqmAttestationError::failed(
            "runtime guard is not an owned regular file",
        ));
    }
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| {
            NativeSqmAttestationError::failed(format!("unable to protect runtime guard: {error}"))
        })?;
    Ok(file)
}

fn flock(file: &File, operation: i32) -> std::io::Result<()> {
    let result = unsafe { libc::flock(file.as_raw_fd(), operation) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn atomic_replace_private(path: &Path, bytes: &[u8]) -> Result<(), NativeSqmAttestationError> {
    let token = random_token()?;
    let temp = path.with_extension(format!("tmp.{token}"));
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temp)
        .map_err(|error| {
            NativeSqmAttestationError::failed(format!("unable to create runtime record: {error}"))
        })?;
    output.write_all(bytes).map_err(|error| {
        NativeSqmAttestationError::failed(format!("unable to write runtime record: {error}"))
    })?;
    output.sync_all().map_err(|error| {
        NativeSqmAttestationError::failed(format!("unable to sync runtime record: {error}"))
    })?;
    drop(output);
    fs::rename(&temp, path).map_err(|error| {
        let _ = fs::remove_file(&temp);
        NativeSqmAttestationError::failed(format!("unable to publish runtime record: {error}"))
    })
}

fn read_bounded_file(path: &Path, limit: usize) -> Result<Vec<u8>, NativeSqmAttestationError> {
    let file = open_bounded_owned_regular(path, limit)?;
    read_bounded_open_file(file, path, limit)
}

fn read_bounded_external_file(
    path: &Path,
    limit: usize,
) -> Result<Vec<u8>, NativeSqmAttestationError> {
    let file = open_optional_bounded_external_regular(path, limit)?.ok_or_else(|| {
        NativeSqmAttestationError::failed(format!("{} is missing", path.display()))
    })?;
    read_bounded_open_file(file, path, limit)
}

fn read_bounded_open_file(
    file: File,
    path: &Path,
    limit: usize,
) -> Result<Vec<u8>, NativeSqmAttestationError> {
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            NativeSqmAttestationError::failed(format!("unable to read {}: {error}", path.display()))
        })?;
    if bytes.len() > limit {
        return Err(NativeSqmAttestationError::failed(format!(
            "{} exceeds its size bound",
            path.display()
        )));
    }
    Ok(bytes)
}

fn require_external_regular(path: &Path, limit: usize) -> Result<(), NativeSqmAttestationError> {
    drop(
        open_optional_bounded_external_regular(path, limit)?.ok_or_else(|| {
            NativeSqmAttestationError::failed(format!("{} is missing", path.display()))
        })?,
    );
    Ok(())
}

fn open_bounded_owned_regular(
    path: &Path,
    limit: usize,
) -> Result<File, NativeSqmAttestationError> {
    open_optional_bounded_owned_regular(path, limit)?
        .ok_or_else(|| NativeSqmAttestationError::failed(format!("{} is missing", path.display())))
}

fn open_optional_bounded_owned_regular(
    path: &Path,
    limit: usize,
) -> Result<Option<File>, NativeSqmAttestationError> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(NativeSqmAttestationError::failed(format!(
                "unable to securely open {}: {error}",
                path.display()
            )));
        }
    };
    let metadata = file.metadata().map_err(|error| {
        NativeSqmAttestationError::failed(format!(
            "unable to inspect opened {}: {error}",
            path.display()
        ))
    })?;
    if !metadata.file_type().is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.mode() & 0o077 != 0
        || metadata.len() > limit as u64
    {
        return Err(NativeSqmAttestationError::failed(format!(
            "{} is not a bounded owned regular file",
            path.display()
        )));
    }
    Ok(Some(file))
}

fn open_optional_bounded_external_regular(
    path: &Path,
    limit: usize,
) -> Result<Option<File>, NativeSqmAttestationError> {
    let parent = path.parent().ok_or_else(|| {
        NativeSqmAttestationError::failed(format!(
            "external file {} has no parent directory",
            path.display()
        ))
    })?;
    let parent_metadata = fs::symlink_metadata(parent).map_err(|error| {
        NativeSqmAttestationError::failed(format!(
            "unable to inspect external file directory {}: {error}",
            parent.display()
        ))
    })?;
    if parent_metadata.file_type().is_symlink()
        || !parent_metadata.file_type().is_dir()
        || parent_metadata.uid() != unsafe { libc::geteuid() }
        || parent_metadata.mode() & 0o022 != 0
    {
        return Err(NativeSqmAttestationError::failed(format!(
            "external file directory {} is not owner-controlled",
            parent.display()
        )));
    }

    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(NativeSqmAttestationError::failed(format!(
                "unable to securely open external file {}: {error}",
                path.display()
            )));
        }
    };
    let metadata = file.metadata().map_err(|error| {
        NativeSqmAttestationError::failed(format!(
            "unable to inspect opened external file {}: {error}",
            path.display()
        ))
    })?;
    if !metadata.file_type().is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.mode() & 0o022 != 0
        || metadata.len() > limit as u64
    {
        return Err(NativeSqmAttestationError::failed(format!(
            "{} is not a bounded owner-controlled regular file",
            path.display()
        )));
    }
    Ok(Some(file))
}

fn interface_lock_stem(interface: &str) -> Result<String, NativeSqmAttestationError> {
    validate_interface(interface, "lock interface")?;
    Ok(interface
        .chars()
        .map(|value| {
            if matches!(value, ':' | '@' | '.') {
                '_'
            } else {
                value
            }
        })
        .collect())
}

fn validate_interface(value: &str, label: &str) -> Result<(), NativeSqmAttestationError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.:@-".contains(&byte))
    {
        return Err(NativeSqmAttestationError::failed(format!(
            "{label} is unsafe"
        )));
    }
    Ok(())
}

fn validate_uci_name(value: &str, label: &str) -> Result<(), NativeSqmAttestationError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(NativeSqmAttestationError::failed(format!(
            "{label} is unsafe"
        )));
    }
    Ok(())
}

fn safe_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn safe_ram_path(path: &Path) -> bool {
    let text = path.to_string_lossy();
    (text.starts_with("/tmp/") || text.starts_with("/run/") || text.starts_with("/var/run/"))
        && !text.contains("/../")
        && !text.ends_with("/..")
        && !text.contains("//")
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_./:@-".contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn rate_policy_options(values: &[(&str, &str)]) -> BTreeMap<String, String> {
        values
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn controller_rate_policy_is_bounded_and_adaptive_caps_are_conditional() {
        assert_eq!(
            managed_sqm_rate_policy_from_options(&BTreeMap::new()).unwrap(),
            ManagedSqmRatePolicy {
                minimum_download_kbps: rate_limits::DEFAULT_MIN_DL_SHAPER_RATE_KBPS,
                maximum_download_kbps: rate_limits::DEFAULT_MAX_DL_SHAPER_RATE_KBPS,
                minimum_upload_kbps: rate_limits::DEFAULT_MIN_UL_SHAPER_RATE_KBPS,
                maximum_upload_kbps: rate_limits::DEFAULT_MAX_UL_SHAPER_RATE_KBPS,
            }
        );

        let disabled = rate_policy_options(&[
            ("adaptive_ceiling_enabled", "0"),
            ("adaptive_ceiling_dl_cap_kbps", "1600000"),
            ("adaptive_ceiling_ul_cap_kbps", "900000"),
        ]);
        assert_eq!(
            managed_sqm_rate_policy_from_options(&disabled).unwrap(),
            ManagedSqmRatePolicy {
                minimum_download_kbps: 5_000,
                maximum_download_kbps: 80_000,
                minimum_upload_kbps: 5_000,
                maximum_upload_kbps: 35_000,
            }
        );

        let enabled = rate_policy_options(&[
            ("adaptive_ceiling_enabled", "1"),
            ("adaptive_ceiling_dl_cap_kbps", "1600000"),
            ("adaptive_ceiling_ul_cap_kbps", "900000"),
        ]);
        assert_eq!(
            managed_sqm_rate_policy_from_options(&enabled).unwrap(),
            ManagedSqmRatePolicy {
                minimum_download_kbps: 5_000,
                maximum_download_kbps: 1_600_000,
                minimum_upload_kbps: 5_000,
                maximum_upload_kbps: 900_000,
            }
        );

        for invalid in [
            rate_policy_options(&[("adaptive_ceiling_enabled", "true")]),
            rate_policy_options(&[("min_dl_shaper_rate_kbps", "0")]),
            rate_policy_options(&[("min_dl_shaper_rate_kbps", "5000.5")]),
            rate_policy_options(&[("min_dl_shaper_rate_kbps", "5e3")]),
            rate_policy_options(&[("min_dl_shaper_rate_kbps", "NaN")]),
            rate_policy_options(&[
                ("min_dl_shaper_rate_kbps", "90000"),
                ("max_dl_shaper_rate_kbps", "80000"),
            ]),
            rate_policy_options(&[
                ("adaptive_ceiling_enabled", "1"),
                ("adaptive_ceiling_dl_cap_kbps", "79999"),
            ]),
        ] {
            assert!(managed_sqm_rate_policy_from_options(&invalid).is_err());
        }
    }

    #[test]
    fn stop_policy_preserves_directional_absence_and_stale_exact_fallback() {
        let dynamic = ManagedSqmStopSpec {
            instance: "wan".to_string(),
            sqm_section: "cake_wan".to_string(),
            target_interface: "eth0".to_string(),
            download_interface: "ifb4eth0".to_string(),
            rate_policy: Some(ManagedSqmRatePolicy {
                minimum_download_kbps: 5_000,
                maximum_download_kbps: 80_000,
                minimum_upload_kbps: 5_000,
                maximum_upload_kbps: 35_000,
            }),
        };
        let mut sqm = ParsedUciSection::default();
        sqm.scalars.insert("download".to_string(), "0".to_string());
        sqm.scalars.insert("upload".to_string(), "0".to_string());
        assert_eq!(stop_attestation_spec(&dynamic, &sqm).unwrap(), None);

        sqm.scalars
            .insert("upload".to_string(), "20000".to_string());
        let upload_only = stop_attestation_spec(&dynamic, &sqm).unwrap().unwrap();
        assert_eq!(upload_only.direction_mode, "upload_only");
        assert_eq!(upload_only.maximum_download_kbps, 80_000);
        assert_eq!(upload_only.maximum_upload_kbps, 35_000);

        sqm.scalars
            .insert("download".to_string(), "4000".to_string());
        sqm.scalars
            .insert("upload".to_string(), "40000".to_string());
        let transition = stop_attestation_spec(&dynamic, &sqm).unwrap().unwrap();
        assert_eq!(transition.direction_mode, "both");
        assert_eq!(transition.minimum_download_kbps, 4_000);
        assert_eq!(transition.maximum_download_kbps, 80_000);
        assert_eq!(transition.minimum_upload_kbps, 5_000);
        assert_eq!(transition.maximum_upload_kbps, 40_000);

        sqm.scalars
            .insert("download".to_string(), "90000".to_string());
        sqm.scalars.insert("upload".to_string(), "0".to_string());
        let download_only = stop_attestation_spec(&dynamic, &sqm).unwrap().unwrap();
        assert_eq!(download_only.direction_mode, "download_only");
        assert_eq!(download_only.minimum_download_kbps, 5_000);
        assert_eq!(download_only.maximum_download_kbps, 90_000);
        assert_eq!(download_only.minimum_upload_kbps, 5_000);
        assert_eq!(download_only.maximum_upload_kbps, 35_000);

        sqm.scalars.insert("download".to_string(), "0".to_string());
        sqm.scalars
            .insert("upload".to_string(), "20000".to_string());
        let stale = ManagedSqmStopSpec {
            rate_policy: None,
            ..dynamic
        };
        let exact = stop_attestation_spec(&stale, &sqm).unwrap().unwrap();
        assert_eq!(exact.direction_mode, "upload_only");
        assert_eq!(exact.minimum_download_kbps, 1);
        assert_eq!(exact.maximum_download_kbps, 1);
        assert_eq!(exact.minimum_upload_kbps, 20_000);
        assert_eq!(exact.maximum_upload_kbps, 20_000);
    }

    #[test]
    fn live_rate_above_static_max_requires_an_enabled_adaptive_cap() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cake-native-sqm-rate-bound-{}-{unique}",
            std::process::id()
        ));
        let sys = root.join("sys");
        fs::create_dir_all(sys.join("eth0")).unwrap();
        let tc = root.join("tc");
        executable(
            &tc,
            "#!/bin/sh\nprintf '%s\\n' 'qdisc cake 8001: root bandwidth 90Mbit besteffort triple-isolate nonat nowash no-ack-filter split-gso'\n",
        );
        let paths = OpenWrtPaths {
            lock_root: root.join("locks"),
            sqm_config: root.join("sqm"),
            sqm_state_root: root.join("state"),
            sys_class_net: sys,
            uci: root.join("uci"),
            tc,
            sqm_run: root.join("sqm-run"),
        };
        let state = SqmRuntimeIdentity::default();
        assert!(validate_cake_direction(&paths, "eth0", true, 5_000, 80_000, &state, "").is_err());
        validate_cake_direction(&paths, "eth0", true, 5_000, 100_000, &state, "").unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    fn executable(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn runtime_identity_rejects_duplicates_and_decodes_exact_state() {
        let source = b"IFACE=\"wan\"\nQDISC=\"cake\"\nSCRIPT=\"piece_of_cake.qos\"\nUPLINK=\"20000\"\nDOWNLINK=\"50000\"\nLINKLAYER=\"none\"\nLLAM=\"default\"\nOVERHEAD=\"0\"\nSTAB_MPU=\"0\"\nUSE_MQ=\"0\"\nINGRESS_CAKE_OPTS=\"besteffort nat wash\"\nEGRESS_CAKE_OPTS=\"diffserv4 nat\"\nIQDISC_OPTS=\"\"\nEQDISC_OPTS=\"\"\nZERO_DSCP_INGRESS=\"0\"\nIGNORE_DSCP_INGRESS=\"0\"\n";
        let state = parse_runtime_identity(source).unwrap();
        assert_eq!(state.interface, "wan");
        assert_eq!(state.download_kbps, 50_000);
        assert_eq!(state.upload_kbps, 20_000);
        let mut duplicate = source.to_vec();
        duplicate.extend_from_slice(b"IFACE=\"wan\"\n");
        assert!(parse_runtime_identity(&duplicate).is_err());
    }

    #[test]
    fn lock_record_is_exact_and_future_fields_fail_closed() {
        let source = b"version=1\npid=10\nproc_starttime=20\nrole=sqm-attest\ntoken=abc\nrecovery_journal=\n";
        assert_eq!(parse_lock_record(source).unwrap().recovery_journal, "");
        let mut future = source.to_vec();
        future.extend_from_slice(b"future=1\n");
        assert!(parse_lock_record(&future).is_err());
    }

    #[test]
    fn uci_parser_stays_inside_one_section_and_rejects_scalar_ambiguity() {
        let parsed = parse_uci_show(
            b"sqm.cake_wan=queue\nsqm.cake_wan.enabled='1'\nsqm.cake_wan.interface='wan'\n",
            "sqm",
            "cake_wan",
            "queue",
            UciListPolicy::Reject,
        )
        .unwrap();
        assert_eq!(parsed.scalars["enabled"], "1");
        assert!(parse_uci_show(
            b"sqm.cake_wan=queue\nsqm.other.enabled='1'\n",
            "sqm",
            "cake_wan",
            "queue",
            UciListPolicy::Reject,
        )
        .is_err());
        assert!(parse_uci_show(
            b"sqm.cake_wan=interface\nsqm.cake_wan.enabled='1'\n",
            "sqm",
            "cake_wan",
            "queue",
            UciListPolicy::Reject,
        )
        .is_err());
    }

    #[test]
    fn stock_openwrt_managed_sqm_projection_is_all_scalar() {
        let source = b"sqm.cake_primary=queue\nsqm.cake_primary._cake_autorate_managed='primary'\nsqm.cake_primary.enabled='1'\nsqm.cake_primary.interface='eth1'\nsqm.cake_primary.download='20000'\nsqm.cake_primary.upload='20000'\nsqm.cake_primary.debug_logging='0'\nsqm.cake_primary.verbosity='5'\nsqm.cake_primary.qdisc='cake'\nsqm.cake_primary.script='piece_of_cake.qos'\nsqm.cake_primary.qdisc_advanced='0'\nsqm.cake_primary.squash_dscp='1'\nsqm.cake_primary.squash_ingress='1'\nsqm.cake_primary.ingress_ecn='ECN'\nsqm.cake_primary.egress_ecn='NOECN'\nsqm.cake_primary.qdisc_really_really_advanced='0'\nsqm.cake_primary.linklayer='none'\nsqm.cake_primary.overhead='0'\nsqm.cake_primary.linklayer_advanced='0'\nsqm.cake_primary.tcMTU='2047'\nsqm.cake_primary.tcTSIZE='128'\nsqm.cake_primary.tcMPU='0'\nsqm.cake_primary.linklayer_adaptation_mechanism='default'\n";
        let parsed = parse_uci_show(
            source,
            "sqm",
            "cake_primary",
            "queue",
            UciListPolicy::Reject,
        )
        .unwrap();
        assert_eq!(
            parsed.scalars.get("interface").map(String::as_str),
            Some("eth1")
        );
        assert_eq!(
            parsed.scalars.get("script").map(String::as_str),
            Some("piece_of_cake.qos")
        );
        assert_eq!(parsed.scalars.len(), 22);
        assert!(parsed.lists.is_empty());
    }

    #[test]
    fn cake_reflector_list_is_preserved_without_weakening_scalar_ownership() {
        let cake_source = b"cake-autorate.primary=cake_autorate\ncake-autorate.primary.enabled='1'\ncake-autorate.primary.manage_sqm='1'\ncake-autorate.primary.sqm_enabled='1'\ncake-autorate.primary.sqm_interface='eth1'\ncake-autorate.primary.ul_if='eth1'\ncake-autorate.primary.dl_if='ifb4eth1'\ncake-autorate.primary.sqm_section='cake_primary'\ncake-autorate.primary.sqm_direction_mode='both'\ncake-autorate.primary.reflector='1.1.1.1' '1.0.0.1' '8.8.8.8' '8.8.4.4' '9.9.9.9' '149.112.112.112'\n";
        let sqm_source = b"sqm.cake_primary=queue\nsqm.cake_primary._cake_autorate_managed='primary'\nsqm.cake_primary.enabled='1'\nsqm.cake_primary.interface='eth1'\n";
        let cake = parse_uci_show(
            cake_source,
            "cake-autorate",
            "primary",
            "cake_autorate",
            UciListPolicy::Allow,
        )
        .unwrap();
        let sqm = parse_uci_show(
            sqm_source,
            "sqm",
            "cake_primary",
            "queue",
            UciListPolicy::Reject,
        )
        .unwrap();
        assert_eq!(
            cake.lists["reflector"],
            [
                "1.1.1.1",
                "1.0.0.1",
                "8.8.8.8",
                "8.8.4.4",
                "9.9.9.9",
                "149.112.112.112"
            ]
        );
        let spec = ManagedSqmAttestationSpec {
            instance: "primary".to_string(),
            sqm_section: "cake_primary".to_string(),
            target_interface: "eth1".to_string(),
            upload_interface: "eth1".to_string(),
            download_interface: "ifb4eth1".to_string(),
            direction_mode: "both".to_string(),
            minimum_download_kbps: 10_000,
            maximum_download_kbps: 20_000,
            minimum_upload_kbps: 10_000,
            maximum_upload_kbps: 20_000,
        };
        validate_live_configuration(&spec, &cake, &sqm).unwrap();
    }

    #[test]
    fn list_valued_ownership_keys_never_fall_through_to_defaults() {
        let base = "cake-autorate.primary=cake_autorate\ncake-autorate.primary.enabled='1'\ncake-autorate.primary.manage_sqm='1'\ncake-autorate.primary.sqm_enabled='1'\ncake-autorate.primary.sqm_interface='eth1'\ncake-autorate.primary.ul_if='eth1'\ncake-autorate.primary.dl_if='ifb4eth1'\ncake-autorate.primary.sqm_section='cake_primary'\ncake-autorate.primary.sqm_direction_mode='both'\n";
        let sqm = parse_uci_show(
            b"sqm.cake_primary=queue\nsqm.cake_primary._cake_autorate_managed='primary'\nsqm.cake_primary.enabled='1'\nsqm.cake_primary.interface='eth1'\n",
            "sqm",
            "cake_primary",
            "queue",
            UciListPolicy::Reject,
        )
        .unwrap();
        let spec = ManagedSqmAttestationSpec {
            instance: "primary".to_string(),
            sqm_section: "cake_primary".to_string(),
            target_interface: "eth1".to_string(),
            upload_interface: "eth1".to_string(),
            download_interface: "ifb4eth1".to_string(),
            direction_mode: "both".to_string(),
            minimum_download_kbps: 10_000,
            maximum_download_kbps: 20_000,
            minimum_upload_kbps: 10_000,
            maximum_upload_kbps: 20_000,
        };
        for (key, scalar) in [
            ("enabled", "'1'"),
            ("sqm_interface", "'eth1'"),
            ("ul_if", "'eth1'"),
            ("dl_if", "'ifb4eth1'"),
            ("sqm_section", "'cake_primary'"),
            ("sqm_direction_mode", "'both'"),
        ] {
            let source = base.replace(
                &format!("cake-autorate.primary.{key}={scalar}"),
                &format!("cake-autorate.primary.{key}='first' 'second'"),
            );
            let parsed = parse_uci_show(
                source.as_bytes(),
                "cake-autorate",
                "primary",
                "cake_autorate",
                UciListPolicy::Allow,
            )
            .unwrap();
            assert!(
                validate_live_configuration(&spec, &parsed, &sqm).is_err(),
                "list-valued {key} must not fall through"
            );
        }
    }

    #[test]
    fn typed_uci_parser_rejects_empty_duplicate_unbounded_and_queue_lists() {
        assert!(parse_uci_show(
            b"sqm.cake_wan=queue\nsqm.cake_wan.enabled=\n",
            "sqm",
            "cake_wan",
            "queue",
            UciListPolicy::Reject,
        )
        .is_err());
        let empty_scalar = parse_uci_show(
            b"cake-autorate.primary=cake_autorate\ncake-autorate.primary.label=''\n",
            "cake-autorate",
            "primary",
            "cake_autorate",
            UciListPolicy::Allow,
        )
        .unwrap();
        assert_eq!(empty_scalar.scalars["label"], "");
        assert!(parse_uci_show(
            b"sqm.cake_wan=queue\nsqm.cake_wan.reflector='1.1.1.1' '8.8.8.8'\n",
            "sqm",
            "cake_wan",
            "queue",
            UciListPolicy::Reject,
        )
        .is_err());
        for duplicate in [
            "cake-autorate.primary=cake_autorate\ncake-autorate.primary.value='one'\ncake-autorate.primary.value='two' 'three'\n",
            "cake-autorate.primary=cake_autorate\ncake-autorate.primary.value='one' 'two'\ncake-autorate.primary.value='three'\n",
            "cake-autorate.primary=cake_autorate\ncake-autorate.primary.value='one' 'two'\ncake-autorate.primary.value='three' 'four'\n",
        ] {
            assert!(parse_uci_show(
                duplicate.as_bytes(),
                "cake-autorate",
                "primary",
                "cake_autorate",
                UciListPolicy::Allow,
            )
            .is_err());
        }
        let too_many = std::iter::repeat_n("'1'", MAX_UCI_LIST_VALUES + 1)
            .collect::<Vec<_>>()
            .join(" ");
        let source = format!(
            "cake-autorate.primary=cake_autorate\ncake-autorate.primary.reflector={too_many}\n"
        );
        assert!(parse_uci_show(
            source.as_bytes(),
            "cake-autorate",
            "primary",
            "cake_autorate",
            UciListPolicy::Allow,
        )
        .is_err());
        let long_value = "x".repeat(MAX_UCI_VALUE_BYTES + 1);
        let source = format!(
            "cake-autorate.primary=cake_autorate\ncake-autorate.primary.value='{long_value}'\n"
        );
        assert!(parse_uci_show(
            source.as_bytes(),
            "cake-autorate",
            "primary",
            "cake_autorate",
            UciListPolicy::Allow,
        )
        .is_err());
    }

    #[test]
    fn uci_show_value_parser_handles_shell_escaped_apostrophes_and_rejects_truncation() {
        assert_eq!(
            parse_uci_show_values("'one' 'two words' 'it'\\''s'").unwrap(),
            ["one", "two words", "it's"]
        );
        assert!(parse_uci_show_values("'unterminated").is_err());
        assert!(parse_uci_show_values("value\\").is_err());
    }

    #[test]
    fn cake_option_groups_and_link_layer_are_exact() {
        let state = SqmRuntimeIdentity {
            linklayer: "ethernet".to_string(),
            linklayer_adaptation: "default".to_string(),
            overhead: 44,
            mpu: 84,
            ..SqmRuntimeIdentity::default()
        };
        let line = "qdisc cake 8010: root bandwidth 100Mbit besteffort triple-isolate nat wash no-ack-filter split-gso noatm overhead 44 mpu 84";
        assert!(validate_cake_options(line, "besteffort nat wash", &state).is_ok());
        assert!(validate_cake_options(line, "diffserv4 nat wash", &state).is_err());
    }

    #[test]
    fn interface_lock_names_match_the_historical_shared_inode_contract() {
        assert_eq!(
            interface_lock_stem("wan.10@eth0:1").unwrap(),
            "wan_10_eth0_1"
        );
        assert!(interface_lock_stem("../wan").is_err());
    }

    #[test]
    fn external_sqm_files_accept_read_bits_without_weakening_private_records() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cake-native-sqm-external-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();

        let state = root.join("eth0.state");
        fs::write(&state, b"IFACE=\"eth0\"\n").unwrap();
        fs::set_permissions(&state, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            open_optional_bounded_external_regular(&state, MAX_STATE_BYTES)
                .unwrap()
                .is_some()
        );
        assert!(open_optional_bounded_owned_regular(&state, MAX_STATE_BYTES).is_err());

        let linked = root.join("eth0-linked.state");
        fs::hard_link(&state, &linked).unwrap();
        assert!(open_optional_bounded_external_regular(&state, MAX_STATE_BYTES).is_err());
        fs::remove_file(&linked).unwrap();

        fs::set_permissions(&root, fs::Permissions::from_mode(0o720)).unwrap();
        assert!(open_optional_bounded_external_regular(&state, MAX_STATE_BYTES).is_err());
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn command_watchdog_terminates_the_owned_process_group() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cake-native-sqm-watchdog-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        let command = root.join("wait-forever");
        executable(&command, "#!/bin/sh\nexec sleep 30\n");

        let started = Instant::now();
        let error = match run_bounded_command(
            &command,
            &[],
            &[],
            Duration::from_millis(40),
            1024,
            1024,
            &|| false,
        ) {
            Err(error) => error,
            Ok(_) => panic!("watchdog command unexpectedly completed"),
        };
        assert!(matches!(
            error,
            NativeSqmAttestationError::Failed(ref message)
                if message.contains("watchdog deadline")
        ));
        assert!(started.elapsed() < Duration::from_secs(2));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn complete_fake_openwrt_attestation_freezes_uci_and_releases_its_record() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cake-native-sqm-attest-{}-{unique}",
            std::process::id()
        ));
        let lock_root = root.join("locks");
        let state_root = root.join("state");
        let sys = root.join("sys");
        fs::create_dir_all(&lock_root).unwrap();
        fs::create_dir_all(&state_root).unwrap();
        fs::create_dir_all(sys.join("eth0/statistics")).unwrap();
        fs::create_dir_all(sys.join("ifb4eth0/statistics")).unwrap();
        fs::write(sys.join("eth0/statistics/tx_bytes"), b"0\n").unwrap();
        fs::write(sys.join("ifb4eth0/statistics/tx_bytes"), b"0\n").unwrap();
        fs::set_permissions(&lock_root, fs::Permissions::from_mode(0o700)).unwrap();

        let sqm_config = root.join("sqm");
        fs::write(&sqm_config, b"config queue 'cake_wan'\n").unwrap();
        fs::set_permissions(&sqm_config, fs::Permissions::from_mode(0o644)).unwrap();
        let state_body = b"IFACE=\"eth0\"\nQDISC=\"cake\"\nSCRIPT=\"piece_of_cake.qos\"\nUPLINK=\"20000\"\nDOWNLINK=\"20000\"\nLINKLAYER=\"none\"\nLLAM=\"default\"\nOVERHEAD=\"0\"\nSTAB_MPU=\"0\"\nUSE_MQ=\"0\"\nINGRESS_CAKE_OPTS=\"besteffort triple-isolate nat wash no-ack-filter split-gso\"\nEGRESS_CAKE_OPTS=\"diffserv4 triple-isolate nat nowash no-ack-filter split-gso\"\nIQDISC_OPTS=\"\"\nEQDISC_OPTS=\"\"\nZERO_DSCP_INGRESS=\"0\"\nIGNORE_DSCP_INGRESS=\"0\"\n";
        fs::write(state_root.join("eth0.state"), state_body).unwrap();
        fs::set_permissions(
            state_root.join("eth0.state"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();

        let uci = root.join("uci");
        executable(
            &uci,
            "#!/bin/sh\ncase \"$*\" in\n*'show cake-autorate.wan') cat <<'EOF'\ncake-autorate.wan=cake_autorate\ncake-autorate.wan.enabled='1'\ncake-autorate.wan.manage_sqm='1'\ncake-autorate.wan.sqm_enabled='1'\ncake-autorate.wan.sqm_interface='eth0'\ncake-autorate.wan.ul_if='eth0'\ncake-autorate.wan.dl_if='ifb4eth0'\ncake-autorate.wan.sqm_section='cake_wan'\ncake-autorate.wan.sqm_direction_mode='both'\ncake-autorate.wan.min_dl_shaper_rate_kbps='5000'\ncake-autorate.wan.max_dl_shaper_rate_kbps='80000'\ncake-autorate.wan.min_ul_shaper_rate_kbps='5000'\ncake-autorate.wan.max_ul_shaper_rate_kbps='35000'\ncake-autorate.wan.adaptive_ceiling_enabled='0'\nEOF\n;;\n*'show sqm.cake_wan') cat <<'EOF'\nsqm.cake_wan=queue\nsqm.cake_wan._cake_autorate_managed='wan'\nsqm.cake_wan.enabled='1'\nsqm.cake_wan.interface='eth0'\nsqm.cake_wan.qdisc='cake'\nsqm.cake_wan.script='piece_of_cake.qos'\nsqm.cake_wan.download='20000'\nsqm.cake_wan.upload='20000'\nsqm.cake_wan.linklayer='none'\nsqm.cake_wan.linklayer_adaptation_mechanism='default'\nsqm.cake_wan.overhead='0'\nsqm.cake_wan.tcMPU='0'\nsqm.cake_wan.use_mq='0'\nsqm.cake_wan.squash_dscp='0'\nsqm.cake_wan.squash_ingress='0'\nEOF\n;;\n*) exit 64 ;;\nesac\n",
        );
        let tc = root.join("tc");
        executable(
            &tc,
            &format!(
                "#!/bin/sh\n[ -f '{}' ] || exit 0\ncase \"$*\" in\n'-details qdisc show dev eth0') printf '%s\n' 'qdisc cake 8001: root bandwidth 20Mbit diffserv4 triple-isolate nat nowash no-ack-filter split-gso raw overhead 0' 'qdisc ingress ffff: parent ffff:fff1' ;;\n'-details qdisc show dev ifb4eth0') printf '%s\n' 'qdisc cake 8002: root bandwidth 29137Kbit besteffort triple-isolate nat wash no-ack-filter split-gso raw overhead 0' ;;\n'filter show dev eth0 ingress') printf '%s\n' 'action order 1: mirred (Egress Redirect to device ifb4eth0)' ;;\n'qdisc show dev eth0') printf '%s\n' 'qdisc cake 8001: root bandwidth 20Mbit' 'qdisc ingress ffff: parent ffff:fff1' ;;\n'qdisc show dev ifb4eth0') printf '%s\n' 'qdisc cake 8002: root bandwidth 29137Kbit' ;;\n*) exit 64 ;;\nesac\n",
                state_root.join("eth0.state").display()
            ),
        );
        let state_template = root.join("state-template");
        fs::write(&state_template, state_body).unwrap();
        fs::set_permissions(&state_template, fs::Permissions::from_mode(0o600)).unwrap();
        let sqm_run_log = root.join("sqm-run.log");
        let sqm_run = root.join("sqm-run");
        executable(
            &sqm_run,
            &format!(
                "#!/bin/sh\n[ -r \"$UCI_CONFIG_DIR/sqm\" ] || exit 70\nprintf '%s\\n' \"$1 $2\" >> '{}'\ncase \"$1\" in\nstop) rm -f '{}'; exit 9 ;;\nstart) cp '{}' '{}'; chmod 644 '{}' ;;\n*) exit 64 ;;\nesac\n",
                sqm_run_log.display(),
                state_root.join("eth0.state").display(),
                state_template.display(),
                state_root.join("eth0.state").display(),
                state_root.join("eth0.state").display(),
            ),
        );

        let paths = OpenWrtPaths {
            lock_root: lock_root.clone(),
            sqm_config,
            sqm_state_root: state_root,
            sys_class_net: sys,
            uci,
            sqm_run,
            tc,
        };
        let spec = ManagedSqmAttestationSpec {
            instance: "wan".to_string(),
            sqm_section: "cake_wan".to_string(),
            target_interface: "eth0".to_string(),
            upload_interface: "eth0".to_string(),
            download_interface: "ifb4eth0".to_string(),
            direction_mode: "both".to_string(),
            minimum_download_kbps: 5_000,
            maximum_download_kbps: 80_000,
            minimum_upload_kbps: 5_000,
            maximum_upload_kbps: 35_000,
        };
        attest_managed_sqm_with_paths(&spec, &paths).unwrap();
        fs::remove_file(lock_root.join("runtime.guard")).unwrap();
        fs::remove_file(lock_root.join("interface-eth0.lock.guard")).unwrap();
        attest_managed_sqm_after_service_action_with_paths(&spec, &paths).unwrap();
        assert!(!lock_root.join("runtime.guard").exists());
        assert!(!lock_root.join("interface-eth0.lock.guard").exists());

        // Reproduce the observed boot ordering using the real strict
        // attestor and inotify: the old state still exists while hotplug has
        // removed the IFB counter, then hotplug restores it and writes state.
        // The readiness loop itself must never invoke the SQM helper.
        let mut events = super::super::sqm_start_events::SqmStartEvents::subscribe(vec![paths
            .sqm_state_root
            .clone()])
        .unwrap();
        let counter = paths.sys_class_net.join("ifb4eth0/statistics/tx_bytes");
        fs::remove_file(&counter).unwrap();
        let mut observations = 0;
        let ready = super::super::service_lifecycle::await_sqm_start(
            &mut events,
            std::time::Instant::now() + std::time::Duration::from_secs(10),
            std::slice::from_ref(&spec),
            || Ok(()),
            |spec| {
                observations += 1;
                let observed = attest_managed_sqm_after_service_action_or_offline_with_paths(spec, &paths);
                if observations == 1 {
                    assert!(matches!(&observed, Err(NativeSqmAttestationError::Failed(message)) if message.contains("counter") && message.contains("missing")));
                    fs::write(&counter, b"0\n").unwrap();
                    fs::write(paths.sqm_state_root.join("eth0.state"), state_body).unwrap();
                }
                observed
            },
        ).unwrap();
        assert_eq!(ready, [false]);
        assert_eq!(observations, 2);
        assert!(!sqm_run_log.exists());

        let foreign_state = b"IFACE=\"foreign\"\n";
        fs::write(paths.sqm_state_root.join("eth0.state"), foreign_state).unwrap();
        fs::set_permissions(
            paths.sqm_state_root.join("eth0.state"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(recover_managed_sqm_with_paths(&spec, &paths, || false).is_err());
        assert_eq!(
            fs::read(paths.sqm_state_root.join("eth0.state")).unwrap(),
            foreign_state
        );
        assert!(!sqm_run_log.exists());

        fs::remove_file(paths.sqm_state_root.join("eth0.state")).unwrap();
        recover_managed_sqm_with_paths(&spec, &paths, || false).unwrap();
        assert_eq!(fs::read_to_string(&sqm_run_log).unwrap(), "start eth0\n");
        assert_eq!(
            fs::read(paths.sqm_state_root.join("eth0.state")).unwrap(),
            state_body
        );
        let stop = ManagedSqmStopSpec {
            instance: "wan".to_string(),
            sqm_section: "cake_wan".to_string(),
            target_interface: "eth0".to_string(),
            download_interface: "ifb4eth0".to_string(),
            rate_policy: Some(ManagedSqmRatePolicy {
                minimum_download_kbps: 5_000,
                maximum_download_kbps: 80_000,
                minimum_upload_kbps: 5_000,
                maximum_upload_kbps: 35_000,
            }),
        };
        stop_managed_sqm_after_service_action_with_paths(&stop, &paths).unwrap();
        assert!(!paths.sqm_state_root.join("eth0.state").exists());
        assert_eq!(
            fs::read_to_string(&sqm_run_log).unwrap(),
            "start eth0\nstop eth0\n"
        );
        fs::write(paths.sqm_state_root.join("eth0.state"), state_body).unwrap();
        fs::set_permissions(
            paths.sqm_state_root.join("eth0.state"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        fs::remove_dir_all(paths.sys_class_net.join("eth0")).unwrap();
        assert!(
            attest_managed_sqm_after_service_action_or_offline_with_paths(&spec, &paths).unwrap()
        );
        fs::remove_file(paths.sys_class_net.join("ifb4eth0/statistics/tx_bytes")).unwrap();
        assert!(
            attest_managed_sqm_after_service_action_or_offline_with_paths(&spec, &paths).is_err()
        );
        fs::write(
            paths.sys_class_net.join("ifb4eth0/statistics/tx_bytes"),
            b"0\n",
        )
        .unwrap();
        stop_managed_sqm_after_service_action_with_paths(&stop, &paths).unwrap();
        assert!(!paths.sqm_state_root.join("eth0.state").exists());
        assert_eq!(
            fs::read_to_string(&sqm_run_log).unwrap(),
            "start eth0\nstop eth0\nstop eth0\n"
        );
        assert!(
            attest_managed_sqm_after_service_action_or_offline_with_paths(&spec, &paths).unwrap()
        );

        fs::write(paths.sqm_state_root.join("eth0.state"), state_body).unwrap();
        fs::set_permissions(
            paths.sqm_state_root.join("eth0.state"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        fs::remove_dir_all(paths.sys_class_net.join("ifb4eth0")).unwrap();
        stop_managed_sqm_after_service_action_with_paths(&stop, &paths).unwrap();
        assert!(!paths.sqm_state_root.join("eth0.state").exists());
        assert_eq!(
            fs::read_to_string(&sqm_run_log).unwrap(),
            "start eth0\nstop eth0\nstop eth0\n"
        );
        assert!(!lock_root.join("interface-eth0.lock").exists());
        assert!(lock_root.join("runtime.guard").is_file());
        assert!(lock_root.join("interface-eth0.lock.guard").is_file());
        assert_eq!(
            fs::read_dir(&lock_root)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("sqm-attest-"))
                .count(),
            0
        );
        fs::remove_dir_all(root).unwrap();
    }
}
