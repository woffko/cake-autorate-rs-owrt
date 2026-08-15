//! Pure instance-side ownership model for temporary Full Auto-Tune runtime overrides.
//!
//! This module does not execute `tc`, SQM helpers, or touch UCI.  It separates
//! authorization and state transitions from the eventual OpenWrt actuator so
//! every crash and drift path can be proven before runtime mutation is enabled.

use super::full_autotune::{
    validate_topology_rates, AutotuneRuntimeAck, AutotuneRuntimeControl, MeasurementTopology,
    RuntimeAckState, MAX_AUTOTUNE_EVIDENCE_RECORDS,
};
use super::identity::ProcessIdentity;
use super::kernel_topology::{KernelTopologyQuery, PrivateActionIdentity, PrivateNamespaceRecord};
use super::protocol::{OperationRouteIdentity, SpeedtestDirection};
use crate::autotune::{AutotuneProfile, LinkKind};

pub(crate) fn speedtest_unshaped_topology(direction: SpeedtestDirection) -> MeasurementTopology {
    match direction {
        SpeedtestDirection::Download => MeasurementTopology::RawDownload,
        SpeedtestDirection::Upload => MeasurementTopology::RawUpload,
        SpeedtestDirection::Both => MeasurementTopology::RawBoth,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeQdiscKind {
    Cake,
    CakeMq,
}

impl RuntimeQdiscKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cake => "cake",
            Self::CakeMq => "cake_mq",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "cake" => Some(Self::Cake),
            "cake_mq" => Some(Self::CakeMq),
            _ => None,
        }
    }
}

/// Durable lifecycle of the private calibration topology.  The checkpoint is
/// written with `Planned` before SQM or traffic-control state is touched.  An
/// IFB may only carry calibration qdiscs after its exact kernel ifindex has
/// been published in the `LinkOwned` transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TemporaryTopologyStage {
    Planned,
    ManagedSqmSuspended,
    /// The positive UCI/route/kernel absence witness was re-attested at the
    /// mutation boundary.  This stage is used only by a bootstrap owner; it
    /// must never be decoded as a suspended managed SQM instance.
    AbsenceAttested,
    LinkOwned,
    Active,
    TemporaryAbsent,
    BaselineRestored,
}

impl TemporaryTopologyStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::ManagedSqmSuspended => "managed_sqm_suspended",
            Self::AbsenceAttested => "absence_attested",
            Self::LinkOwned => "link_owned",
            Self::Active => "active",
            Self::TemporaryAbsent => "temporary_absent",
            Self::BaselineRestored => "baseline_restored",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "planned" => Some(Self::Planned),
            "managed_sqm_suspended" => Some(Self::ManagedSqmSuspended),
            "absence_attested" => Some(Self::AbsenceAttested),
            "link_owned" => Some(Self::LinkOwned),
            "active" => Some(Self::Active),
            "temporary_absent" => Some(Self::TemporaryAbsent),
            "baseline_restored" => Some(Self::BaselineRestored),
            _ => None,
        }
    }
}

/// Exact, job-scoped ownership identity for the temporary CAKE/IFB topology.
/// All values except `ifb_ifindex` are derived from the random permit id and
/// are therefore durably known before link creation.  Cleanup still requires
/// the observed ifindex once the link has crossed the `LinkOwned` boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemporaryTopologyIdentity {
    pub ifb_name: String,
    pub ifb_alias: String,
    pub ifb_ifindex: Option<u32>,
    pub target_qdisc_handle: String,
    pub ifb_qdisc_handle: String,
    pub ingress_qdisc_handle: String,
    pub redirect_preference: u16,
    pub redirect_filter_handle: u32,
    pub redirect_action_index: u32,
    /// Exact 128-bit tc action cookie, canonically stored as 32 lower-hex
    /// characters so it is stable in the private line protocol and tc argv.
    pub redirect_action_cookie: String,
}

/// The kernel-created root hash table used by a terminal u32 ingress filter.
/// A deterministic node is allocated inside this table; using another hash
/// bucket would require an additional user-created table and would add a
/// second cleanup object without adding ownership strength.
pub(crate) const PRIVATE_U32_ROOT_FILTER_HANDLE: u32 = 0x8000_0000;

impl TemporaryTopologyIdentity {
    pub fn planned_for_permit(permit_id: &str) -> Result<Self, String> {
        require_lower_hex("temporary topology permit id", permit_id, 32)?;
        let interface_seed = &permit_id[..8];
        let handle_seed = &permit_id[..3];
        let preference_seed = u16::from_str_radix(&permit_id[..4], 16)
            .map_err(|_| "temporary topology preference seed is invalid".to_string())?;
        let identity_seed = u32::from_str_radix(&permit_id[..8], 16)
            .map_err(|_| "temporary topology identity seed is invalid".to_string())?;
        let identity = Self {
            ifb_name: format!("catf{interface_seed}"),
            ifb_alias: format!("cake-autotune-{permit_id}"),
            ifb_ifindex: None,
            target_qdisc_handle: format!("a{handle_seed}:"),
            ifb_qdisc_handle: format!("b{handle_seed}:"),
            ingress_qdisc_handle: "ffff:".to_string(),
            redirect_preference: 49_152 + (preference_seed % 1_024),
            redirect_filter_handle: PRIVATE_U32_ROOT_FILTER_HANDLE
                | (identity_seed & 0x0000_0fff).max(1),
            redirect_action_index: 0xca00_0000 | (identity_seed & 0x00ff_ffff),
            redirect_action_cookie: permit_id.to_string(),
        };
        identity.validate()?;
        Ok(identity)
    }

    pub fn validate(&self) -> Result<(), String> {
        require_interface(&self.ifb_name)?;
        if self.ifb_name.len() > 15 {
            return Err("temporary IFB name exceeds the kernel interface limit".to_string());
        }
        require_identifier("temporary IFB alias", &self.ifb_alias)?;
        if self.ifb_ifindex == Some(0) {
            return Err("temporary IFB ifindex is invalid".to_string());
        }
        validate_qdisc_handle("temporary target qdisc handle", &self.target_qdisc_handle)?;
        validate_qdisc_handle("temporary IFB qdisc handle", &self.ifb_qdisc_handle)?;
        if self.target_qdisc_handle == self.ifb_qdisc_handle {
            return Err("temporary CAKE qdisc handles must be distinct".to_string());
        }
        if self.ingress_qdisc_handle != "ffff:" {
            return Err("temporary ingress qdisc handle is unsupported".to_string());
        }
        if !(49_152..=50_175).contains(&self.redirect_preference) {
            return Err("temporary redirect preference is outside its private range".to_string());
        }
        if self.redirect_filter_handle & 0xffff_f000 != PRIVATE_U32_ROOT_FILTER_HANDLE
            || self.redirect_filter_handle & 0x0000_0fff == 0
        {
            return Err(
                "temporary redirect filter handle is not a terminal node in the private u32 root"
                    .to_string(),
            );
        }
        if self.redirect_action_index & 0xff00_0000 != 0xca00_0000 {
            return Err("temporary redirect action index is outside its private range".to_string());
        }
        require_lower_hex(
            "temporary redirect action cookie",
            &self.redirect_action_cookie,
            32,
        )?;
        Ok(())
    }

    pub fn validate_for_permit(&self, permit_id: &str) -> Result<(), String> {
        self.validate()?;
        let expected = Self::planned_for_permit(permit_id)?;
        if self.ifb_name != expected.ifb_name
            || self.ifb_alias != expected.ifb_alias
            || self.target_qdisc_handle != expected.target_qdisc_handle
            || self.ifb_qdisc_handle != expected.ifb_qdisc_handle
            || self.ingress_qdisc_handle != expected.ingress_qdisc_handle
            || self.redirect_preference != expected.redirect_preference
            || self.redirect_filter_handle != expected.redirect_filter_handle
            || self.redirect_action_index != expected.redirect_action_index
            || self.redirect_action_cookie != expected.redirect_action_cookie
        {
            return Err("temporary topology identity does not match its permit".to_string());
        }
        Ok(())
    }

    pub fn private_namespace(&self) -> Result<PrivateNamespaceRecord, String> {
        self.validate()?;
        let namespace = PrivateNamespaceRecord {
            link_names: vec![self.ifb_name.clone()],
            link_aliases: vec![self.ifb_alias.clone()],
            qdisc_handles: vec![
                parse_qdisc_handle(&self.target_qdisc_handle)?,
                parse_qdisc_handle(&self.ifb_qdisc_handle)?,
                parse_qdisc_handle(&self.ingress_qdisc_handle)?,
            ],
            filter_handles: vec![PRIVATE_U32_ROOT_FILTER_HANDLE, self.redirect_filter_handle],
            filter_priorities: vec![self.redirect_preference],
            action_identities: vec![PrivateActionIdentity {
                kind: "mirred".to_string(),
                index: self.redirect_action_index,
                cookie: Some(decode_lower_hex(&self.redirect_action_cookie)?),
            }],
        };
        namespace.validate().map_err(|error| error.to_string())?;
        Ok(namespace)
    }

    pub fn redirect_filter_tc_handle(&self) -> Result<String, String> {
        self.validate()?;
        Ok(format!(
            "800::{:x}",
            self.redirect_filter_handle & 0x0000_0fff
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeRateBounds {
    pub minimum_kbps: u64,
    pub maximum_kbps: u64,
}

impl RuntimeRateBounds {
    fn validate(self, direction: &str) -> Result<(), String> {
        if self.minimum_kbps < 100
            || self.minimum_kbps > self.maximum_kbps
            || self.maximum_kbps > crate::autotune::MAX_RATE_KBPS
        {
            return Err(format!(
                "{direction} runtime permit rate bounds are invalid"
            ));
        }
        Ok(())
    }

    fn contains(self, value: u64) -> bool {
        (self.minimum_kbps..=self.maximum_kbps).contains(&value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimePermitKind {
    Autotune,
    SpeedtestUnshaped,
}

impl RuntimePermitKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Autotune => "autotune",
            Self::SpeedtestUnshaped => "speedtest_unshaped",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "autotune" => Some(Self::Autotune),
            "speedtest_unshaped" => Some(Self::SpeedtestUnshaped),
            _ => None,
        }
    }
}

/// Durable proof that a future managed SQM instance was absent before a
/// bootstrap calibration acquired the target.  This is an identity witness,
/// not a throughput measurement: rate and qdisc search authority remains in
/// the sibling permit fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AbsentRuntimeBaseline {
    pub planned_sqm_section: String,
    pub target_interface: String,
    pub target_ifindex: u32,
    pub route_fingerprint: String,
    pub config_fingerprint: String,
    pub sqm_fingerprint: String,
    pub kernel_topology_fingerprint: String,
    /// Seed of the exact calibration-private kernel namespace.  It remains
    /// durable after the temporary topology is removed so Apply/recovery can
    /// reconstruct the same read-only query and reject any orphan ownership.
    pub kernel_namespace_seed: String,
}

impl AbsentRuntimeBaseline {
    pub fn validate(&self) -> Result<(), String> {
        super::sqm_identity::validate_uci_section(&self.planned_sqm_section)?;
        require_interface(&self.target_interface)?;
        if self.target_ifindex == 0 || self.target_ifindex > i32::MAX as u32 {
            return Err("absent runtime baseline target ifindex is invalid".to_string());
        }
        require_lower_hex(
            "absent runtime baseline route fingerprint",
            &self.route_fingerprint,
            64,
        )?;
        require_lower_hex(
            "absent runtime baseline config fingerprint",
            &self.config_fingerprint,
            64,
        )?;
        require_lower_hex(
            "absent runtime baseline SQM fingerprint",
            &self.sqm_fingerprint,
            64,
        )?;
        require_lower_hex(
            "absent runtime baseline kernel topology fingerprint",
            &self.kernel_topology_fingerprint,
            64,
        )?;
        require_lower_hex(
            "absent runtime baseline kernel namespace seed",
            &self.kernel_namespace_seed,
            32,
        )?;
        let temporary = TemporaryTopologyIdentity::planned_for_permit(&self.kernel_namespace_seed)?;
        if temporary.ifb_name == self.target_interface
            || temporary.ifb_alias == self.target_interface
        {
            return Err(
                "absent runtime baseline private namespace collides with its target".to_string(),
            );
        }
        let _ = temporary.private_namespace()?;
        Ok(())
    }

    pub fn kernel_topology_query(
        &self,
        route: OperationRouteIdentity,
    ) -> Result<KernelTopologyQuery, String> {
        self.validate()?;
        let query = KernelTopologyQuery {
            target_interface: self.target_interface.clone(),
            route,
            private_namespace: TemporaryTopologyIdentity::planned_for_permit(
                &self.kernel_namespace_seed,
            )?
            .private_namespace()?,
        };
        query.validate().map_err(|error| error.to_string())?;
        Ok(query)
    }
}

/// The lifecycle baseline has two structurally distinct meanings.  `Managed`
/// carries the managed object appropriate to the current protocol layer;
/// `Absent` carries only a positive absence identity and must never be
/// represented by `RawBoth`, zero rates, or an empty managed snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeBaseline<T> {
    Managed(T),
    Absent(AbsentRuntimeBaseline),
}

pub type RuntimePermitBaseline = RuntimeBaseline<MeasurementTopology>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutotuneRuntimePermit {
    pub kind: RuntimePermitKind,
    pub permit_id: String,
    pub job_id: String,
    pub worker_run_id: String,
    pub boot_id: String,
    pub coordinator_generation: String,
    pub worker: ProcessIdentity,
    pub instance_name: String,
    pub target_interface: String,
    pub route_identity: String,
    pub route_fingerprint: String,
    pub sqm_fingerprint: String,
    pub deadline_boot_ms: u64,
    pub maximum_sequence: u32,
    /// Closed policy inputs used to construct and attest the private CAKE
    /// topology.  They are captured in the coordinator permit so the instance
    /// actuator never infers qdisc semantics from mutable UCI during a run.
    pub profile: AutotuneProfile,
    pub link_kind: LinkKind,
    /// Exact managed topology descriptor or an explicit, positive absence
    /// identity.  A directional managed baseline is valid, but it must never
    /// be confused with either `RawBoth` or the both-shaped private topology
    /// which the permit may create.
    pub baseline: RuntimePermitBaseline,
    /// Initial rates are bounded search-authority seeds.  Existing managed
    /// instances re-attest them against live state; absent bootstrap requests
    /// may seed them from service capabilities.  In neither case are these
    /// fields measurement evidence by themselves.
    pub initial_download_kbps: u64,
    pub initial_upload_kbps: u64,
    /// Qdisc kinds authorized for each shaped calibration direction.  These
    /// are explicit because a missing baseline direction has no live qdisc
    /// from which the actuator could safely infer a kind.
    pub download_qdisc_kind: RuntimeQdiscKind,
    pub upload_qdisc_kind: RuntimeQdiscKind,
    pub allow_bypass_download: bool,
    pub allow_bypass_upload: bool,
    pub download_bounds: RuntimeRateBounds,
    pub upload_bounds: RuntimeRateBounds,
}

impl AutotuneRuntimePermit {
    pub fn capture_baseline_topology(&self) -> MeasurementTopology {
        match &self.baseline {
            RuntimeBaseline::Managed(topology) => *topology,
            RuntimeBaseline::Absent(_) => MeasurementTopology::RawBoth,
        }
    }

    pub fn managed_baseline_topology(&self) -> Result<MeasurementTopology, String> {
        match &self.baseline {
            RuntimeBaseline::Managed(topology) => Ok(*topology),
            RuntimeBaseline::Absent(_) => {
                Err("runtime permit does not have a managed baseline topology".to_string())
            }
        }
    }

    /// Authorize the exact live rates which an existing instance will hold
    /// while its idle baseline is captured.  `initial_*` records the rate at
    /// coordinator admission, but the ordinary controller may complete one
    /// already-in-flight update before it observes the permit.  The capture
    /// therefore binds the freshly attested held rate, within the immutable
    /// permit bounds, rather than pretending the historical seed is still
    /// installed.
    pub fn authorizes_idle_baseline(
        &self,
        topology: MeasurementTopology,
        download_kbps: Option<u64>,
        upload_kbps: Option<u64>,
    ) -> Result<(), String> {
        self.validate()?;
        if topology != self.capture_baseline_topology() {
            return Err("idle runtime topology does not match its permit baseline".to_string());
        }
        validate_topology_rates(topology, download_kbps, upload_kbps)?;
        if download_kbps.is_some_and(|rate| !self.download_bounds.contains(rate)) {
            return Err("idle download rate is outside its permitted range".to_string());
        }
        if upload_kbps.is_some_and(|rate| !self.upload_bounds.contains(rate)) {
            return Err("idle upload rate is outside its permitted range".to_string());
        }
        Ok(())
    }

    fn speedtest_unshaped_topology(&self) -> Option<MeasurementTopology> {
        match (self.allow_bypass_download, self.allow_bypass_upload) {
            (true, false) => Some(MeasurementTopology::RawDownload),
            (false, true) => Some(MeasurementTopology::RawUpload),
            (true, true) => Some(MeasurementTopology::RawBoth),
            (false, false) => None,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        require_lower_hex("runtime permit id", &self.permit_id, 32)?;
        require_lower_hex("runtime permit job id", &self.job_id, 32)?;
        require_lower_hex("runtime permit worker run id", &self.worker_run_id, 32)?;
        require_lower_hex("runtime permit boot id", &self.boot_id, 32)?;
        require_lower_hex(
            "runtime permit coordinator generation",
            &self.coordinator_generation,
            32,
        )?;
        require_lower_hex(
            "runtime permit route fingerprint",
            &self.route_fingerprint,
            64,
        )?;
        require_lower_hex("runtime permit SQM fingerprint", &self.sqm_fingerprint, 64)?;
        require_identifier("runtime permit instance", &self.instance_name)?;
        require_interface(&self.target_interface)?;
        require_route_identity(&self.route_identity)?;
        if self.worker.pid == 0
            || self.worker.process_group == 0
            || self.worker.starttime_ticks == 0
        {
            return Err("runtime permit worker identity is incomplete".to_string());
        }
        if self.deadline_boot_ms == 0
            || self.maximum_sequence == 0
            || self.maximum_sequence as usize > MAX_AUTOTUNE_EVIDENCE_RECORDS
        {
            return Err("runtime permit lifetime or sequence bound is invalid".to_string());
        }
        self.download_bounds.validate("download")?;
        self.upload_bounds.validate("upload")?;
        match &self.baseline {
            RuntimeBaseline::Managed(topology) => {
                if !topology.is_managed_baseline() {
                    return Err(
                        "runtime permit baseline must retain at least one managed CAKE direction"
                            .to_string(),
                    );
                }
                if (!topology.download_is_shaped()
                    && self.download_qdisc_kind != RuntimeQdiscKind::Cake)
                    || (!topology.upload_is_shaped()
                        && self.upload_qdisc_kind != RuntimeQdiscKind::Cake)
                {
                    return Err(
                        "a direction missing from the managed baseline must use private CAKE"
                            .to_string(),
                    );
                }
            }
            RuntimeBaseline::Absent(absent) => {
                absent.validate()?;
                if self.kind != RuntimePermitKind::Autotune {
                    return Err(
                        "absent runtime baseline is supported only by Auto-Tune permits"
                            .to_string(),
                    );
                }
                if absent.route_fingerprint != self.route_fingerprint
                    || absent.target_interface != self.target_interface
                    || absent.sqm_fingerprint != self.sqm_fingerprint
                    || absent.kernel_namespace_seed != self.permit_id
                {
                    return Err(
                        "absent runtime baseline does not match its permit fingerprints"
                            .to_string(),
                    );
                }
            }
        }
        if !self.download_bounds.contains(self.initial_download_kbps)
            || !self.upload_bounds.contains(self.initial_upload_kbps)
        {
            return Err(
                "runtime permit initial rates are outside their permitted bounds".to_string(),
            );
        }
        if self.kind == RuntimePermitKind::SpeedtestUnshaped
            && (self.maximum_sequence != 1
                || self.profile != AutotuneProfile::BestOverall
                || self.link_kind != LinkKind::Unknown
                || self.managed_baseline_topology().ok() != Some(MeasurementTopology::ShapedBoth)
                || self.speedtest_unshaped_topology().is_none()
                || self.download_bounds.minimum_kbps != self.initial_download_kbps
                || self.download_bounds.maximum_kbps != self.initial_download_kbps
                || self.upload_bounds.minimum_kbps != self.initial_upload_kbps
                || self.upload_bounds.maximum_kbps != self.initial_upload_kbps)
        {
            return Err("unshaped Speed Test runtime permit has non-canonical policy".to_string());
        }
        Ok(())
    }

    pub fn authorizes(
        &self,
        instance_name: &str,
        control: &AutotuneRuntimeControl,
        current_boot_ms: u64,
    ) -> Result<(), String> {
        self.authorizes_identity(instance_name, control)?;
        if current_boot_ms == 0
            || current_boot_ms >= control.deadline_boot_ms
            || control.deadline_boot_ms > self.deadline_boot_ms
        {
            return Err("runtime control is outside its permitted lifetime".to_string());
        }
        Ok(())
    }

    /// Re-attest the immutable permit/control relationship during recovery.
    /// Restoration can legitimately complete after the original runtime
    /// deadline, so this deliberately checks identity, topology and bounds
    /// without applying the live-control lifetime gate.
    pub(crate) fn attests_recovery_control(
        &self,
        instance_name: &str,
        control: &AutotuneRuntimeControl,
    ) -> Result<(), String> {
        self.authorizes_identity(instance_name, control)
    }

    fn authorizes_identity(
        &self,
        instance_name: &str,
        control: &AutotuneRuntimeControl,
    ) -> Result<(), String> {
        self.validate()?;
        control.validate()?;
        if self.instance_name != instance_name
            || self.permit_id != control.permit_id
            || self.job_id != control.job_id
            || self.worker_run_id != control.worker_run_id
            || self.boot_id != control.boot_id
            || self.coordinator_generation != control.coordinator_generation
            || self.worker != control.worker
            || self.target_interface != control.target_interface
            || self.route_fingerprint != control.route_fingerprint
            || self.sqm_fingerprint != control.sqm_fingerprint
        {
            return Err("runtime control does not match its immutable permit".to_string());
        }
        if control.sequence > self.maximum_sequence {
            return Err("runtime control sequence exceeds its permit".to_string());
        }
        if self.kind == RuntimePermitKind::SpeedtestUnshaped {
            let expected_topology = self.speedtest_unshaped_topology().ok_or_else(|| {
                "unshaped Speed Test permit has no authorized bypass direction".to_string()
            })?;
            let expected_download_kbps = expected_topology
                .download_is_shaped()
                .then_some(self.initial_download_kbps);
            let expected_upload_kbps = expected_topology
                .upload_is_shaped()
                .then_some(self.initial_upload_kbps);
            if control.sequence != 1
                || control.topology != expected_topology
                || control.download_kbps != expected_download_kbps
                || control.upload_kbps != expected_upload_kbps
            {
                return Err(
                    "unshaped Speed Test permit authorizes only its exact directional control"
                        .to_string(),
                );
            }
            return Ok(());
        }
        if !control.topology.download_is_shaped() && !self.allow_bypass_download {
            return Err("runtime permit does not allow download bypass".to_string());
        }
        if !control.topology.upload_is_shaped() && !self.allow_bypass_upload {
            return Err("runtime permit does not allow upload bypass".to_string());
        }
        if control
            .download_kbps
            .is_some_and(|rate| !self.download_bounds.contains(rate))
        {
            return Err("download override is outside its permitted range".to_string());
        }
        if control
            .upload_kbps
            .is_some_and(|rate| !self.upload_bounds.contains(rate))
        {
            return Err("upload override is outside its permitted range".to_string());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeSnapshot {
    pub target_interface: String,
    pub route_fingerprint: String,
    pub sqm_fingerprint: String,
    pub topology: MeasurementTopology,
    pub download_kbps: Option<u64>,
    pub upload_kbps: Option<u64>,
    pub download_qdisc_kind: Option<RuntimeQdiscKind>,
    pub upload_qdisc_kind: Option<RuntimeQdiscKind>,
}

impl RuntimeSnapshot {
    pub fn validate(&self) -> Result<(), String> {
        require_interface(&self.target_interface)?;
        require_lower_hex(
            "runtime snapshot route fingerprint",
            &self.route_fingerprint,
            64,
        )?;
        require_lower_hex(
            "runtime snapshot SQM fingerprint",
            &self.sqm_fingerprint,
            64,
        )?;
        validate_topology_rates(self.topology, self.download_kbps, self.upload_kbps)?;
        if self.download_kbps.is_some() != self.download_qdisc_kind.is_some()
            || self.upload_kbps.is_some() != self.upload_qdisc_kind.is_some()
        {
            return Err("runtime snapshot qdisc kind does not match shaped directions".to_string());
        }
        Ok(())
    }

    fn matches_control(
        &self,
        control: &AutotuneRuntimeControl,
        _permit: &AutotuneRuntimePermit,
    ) -> bool {
        self.target_interface == control.target_interface
            && self.route_fingerprint == control.route_fingerprint
            && self.sqm_fingerprint == control.sqm_fingerprint
            && self.topology == control.topology
            && self.download_kbps == control.download_kbps
            && self.upload_kbps == control.upload_kbps
            && self.download_qdisc_kind == control.download_kbps.map(|_| RuntimeQdiscKind::Cake)
            && self.upload_qdisc_kind == control.upload_kbps.map(|_| RuntimeQdiscKind::Cake)
    }
}

/// Durable restoration authority captured before a private runtime override.
///
/// `Managed` is the historical, fully attested SQM runtime snapshot. `Absent`
/// is only a UCI/kernel identity witness for a future bootstrap actuator; it is
/// deliberately not a measured runtime snapshot and is not executable yet.
pub type RuntimeRestoreBaseline = RuntimeBaseline<RuntimeSnapshot>;

impl RuntimeBaseline<RuntimeSnapshot> {
    pub fn validate_restore_baseline(&self) -> Result<(), String> {
        match self {
            Self::Managed(snapshot) => snapshot.validate(),
            Self::Absent(absent) => absent.validate(),
        }
    }

    pub fn validate_for_permit(&self, permit: &AutotuneRuntimePermit) -> Result<(), String> {
        permit.validate()?;
        self.validate_restore_baseline()?;
        match (&permit.baseline, self) {
            (RuntimeBaseline::Managed(topology), RuntimeBaseline::Managed(snapshot)) => {
                if snapshot.target_interface != permit.target_interface
                    || snapshot.route_fingerprint != permit.route_fingerprint
                    || snapshot.sqm_fingerprint != permit.sqm_fingerprint
                    || snapshot.topology != *topology
                    || (topology.download_is_shaped()
                        && snapshot.download_qdisc_kind != Some(permit.download_qdisc_kind))
                    || (topology.upload_is_shaped()
                        && snapshot.upload_qdisc_kind != Some(permit.upload_qdisc_kind))
                {
                    return Err(
                        "managed runtime restore baseline does not match its permit".to_string()
                    );
                }
            }
            (RuntimeBaseline::Absent(expected), RuntimeBaseline::Absent(actual)) => {
                if actual != expected {
                    return Err(
                        "absent runtime restore baseline does not match its permit".to_string()
                    );
                }
            }
            _ => {
                return Err(
                    "runtime permit and restore checkpoint use different baseline variants"
                        .to_string(),
                )
            }
        }
        Ok(())
    }

    pub fn managed_snapshot(&self) -> Result<&RuntimeSnapshot, String> {
        match self {
            Self::Managed(snapshot) => Ok(snapshot),
            Self::Absent(_) => {
                Err("absent runtime baseline has no managed runtime snapshot".to_string())
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeOverridePhase {
    Idle,
    Applying,
    Applied,
    Restoring,
    Restored,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeRestoreReason {
    ApplyFailed,
    AppliedAttestationMismatch,
    ControlMissing,
    PermitMissing,
    DeadlineExpired,
    WorkerIdentityMismatch,
    RouteDrift,
    BaselineAttestationUnavailable,
    SqmDrift,
    RuntimeDrift,
    SequenceConflict,
    InstanceRestarted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeIdentityHealth {
    Matches,
    Drifted,
    Unavailable,
}

impl RuntimeIdentityHealth {
    pub fn from_attestation(result: Result<bool, String>) -> Self {
        match result {
            Ok(true) => Self::Matches,
            Ok(false) => Self::Drifted,
            Err(_) => Self::Unavailable,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeOverrideAction {
    None,
    MalformedControl,
    Apply(AutotuneRuntimeControl),
    Restore {
        baseline: RuntimeRestoreBaseline,
        reason: RuntimeRestoreReason,
        ack: AutotuneRuntimeAck,
    },
    PublishAck(AutotuneRuntimeAck),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeOverrideHealth<'a> {
    pub current_boot_ms: u64,
    pub permit_present: bool,
    pub control_present: bool,
    pub worker_identity_matches: bool,
    pub route_identity: &'a str,
    pub baseline_identity: RuntimeIdentityHealth,
    pub applied_runtime_matches: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RuntimeRouteRearmHealth<'a> {
    pub current_boot_ms: u64,
    pub restore_intent_present: bool,
    pub worker_identity_matches: bool,
    pub route_identity: &'a str,
    pub baseline_identity: RuntimeIdentityHealth,
    pub baseline_runtime_matches: bool,
}

#[derive(Clone, Debug)]
struct RuntimeOverrideSession {
    permit: AutotuneRuntimePermit,
    baseline: RuntimeRestoreBaseline,
    control: AutotuneRuntimeControl,
    applied_ack: Option<AutotuneRuntimeAck>,
    restore_reason: Option<RuntimeRestoreReason>,
    restore_started_boot_ms: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct RuntimeOverrideTracker {
    instance_name: String,
    phase: RuntimeOverridePhase,
    session: Option<RuntimeOverrideSession>,
}

impl RuntimeOverrideTracker {
    pub fn new(instance_name: String) -> Result<Self, String> {
        require_identifier("runtime tracker instance", &instance_name)?;
        Ok(Self {
            instance_name,
            phase: RuntimeOverridePhase::Idle,
            session: None,
        })
    }

    pub fn phase(&self) -> RuntimeOverridePhase {
        self.phase
    }

    pub(crate) fn owner_permit(&self) -> Option<&AutotuneRuntimePermit> {
        self.session.as_ref().map(|session| &session.permit)
    }

    pub(crate) fn requested_control(&self) -> Option<&AutotuneRuntimeControl> {
        self.session.as_ref().map(|session| &session.control)
    }

    pub(crate) fn restore_reason(&self) -> Option<RuntimeRestoreReason> {
        self.session
            .as_ref()
            .and_then(|session| session.restore_reason)
    }

    pub(crate) fn baseline(&self) -> Option<&RuntimeRestoreBaseline> {
        self.session.as_ref().map(|session| &session.baseline)
    }

    pub fn reject_before_apply(
        &self,
        control: &AutotuneRuntimeControl,
        current_boot_ms: u64,
        diagnostic_code: &str,
    ) -> RuntimeOverrideAction {
        if self.phase != RuntimeOverridePhase::Idle
            || control.validate().is_err()
            || require_identifier("runtime rejection diagnostic", diagnostic_code).is_err()
        {
            return RuntimeOverrideAction::MalformedControl;
        }
        RuntimeOverrideAction::PublishAck(rejected_ack(control, current_boot_ms, diagnostic_code))
    }

    pub fn submit(
        &mut self,
        permit: AutotuneRuntimePermit,
        control: AutotuneRuntimeControl,
        baseline: Option<RuntimeRestoreBaseline>,
        current_boot_ms: u64,
    ) -> RuntimeOverrideAction {
        if control.validate().is_err() {
            return RuntimeOverrideAction::MalformedControl;
        }
        if let Err(error) = permit.authorizes(&self.instance_name, &control, current_boot_ms) {
            return RuntimeOverrideAction::PublishAck(rejected_ack(
                &control,
                current_boot_ms,
                rejection_code(&error),
            ));
        }
        if self.phase == RuntimeOverridePhase::Idle {
            let Some(baseline) = baseline else {
                return RuntimeOverrideAction::PublishAck(rejected_ack(
                    &control,
                    current_boot_ms,
                    "baseline-required",
                ));
            };
            if baseline.validate_for_permit(&permit).is_err() {
                return RuntimeOverrideAction::PublishAck(rejected_ack(
                    &control,
                    current_boot_ms,
                    "baseline-mismatch",
                ));
            }
            self.phase = RuntimeOverridePhase::Applying;
            self.session = Some(RuntimeOverrideSession {
                permit,
                baseline,
                control: control.clone(),
                applied_ack: None,
                restore_reason: None,
                restore_started_boot_ms: None,
            });
            return RuntimeOverrideAction::Apply(control);
        }

        let Some(session) = self.session.as_ref() else {
            return RuntimeOverrideAction::PublishAck(rejected_ack(
                &control,
                current_boot_ms,
                "tracker-state-invalid",
            ));
        };
        if session.permit.permit_id != permit.permit_id
            || session.permit.job_id != permit.job_id
            || session.permit.worker_run_id != permit.worker_run_id
        {
            return RuntimeOverrideAction::PublishAck(rejected_ack(
                &control,
                current_boot_ms,
                "runtime-owner-busy",
            ));
        }
        if session.permit != permit {
            return self.begin_restore(RuntimeRestoreReason::SequenceConflict, current_boot_ms);
        }
        if matches!(
            self.phase,
            RuntimeOverridePhase::Restoring | RuntimeOverridePhase::Restored
        ) {
            return RuntimeOverrideAction::PublishAck(rejected_ack(
                &control,
                current_boot_ms,
                "runtime-restoration-pending",
            ));
        }
        if control.sequence == session.control.sequence {
            if control != session.control {
                return self.begin_restore(RuntimeRestoreReason::SequenceConflict, current_boot_ms);
            }
            return session
                .applied_ack
                .clone()
                .map(RuntimeOverrideAction::PublishAck)
                .unwrap_or(RuntimeOverrideAction::None);
        }
        if control.sequence != session.control.sequence.saturating_add(1) {
            return self.begin_restore(RuntimeRestoreReason::SequenceConflict, current_boot_ms);
        }
        self.phase = RuntimeOverridePhase::Applying;
        if let Some(session) = self.session.as_mut() {
            session.control = control.clone();
            session.applied_ack = None;
        }
        RuntimeOverrideAction::Apply(control)
    }

    /// Re-arm the exact candidate which was interrupted by a transient route
    /// identity loss.  This is deliberately separate from `submit`: ordinary
    /// sequence advancement may change rates/topology, while route recovery is
    /// allowed to change only the sequence number.  The durable baseline stays
    /// owned by the same live worker and is re-attested by the driver before
    /// this transition is called.
    pub fn rearm_after_route_recovery(
        &mut self,
        permit: AutotuneRuntimePermit,
        control: AutotuneRuntimeControl,
        health: RuntimeRouteRearmHealth<'_>,
    ) -> RuntimeOverrideAction {
        if control.validate().is_err() {
            return RuntimeOverrideAction::MalformedControl;
        }
        if let Err(error) = permit.authorizes(&self.instance_name, &control, health.current_boot_ms)
        {
            return RuntimeOverrideAction::PublishAck(rejected_ack(
                &control,
                health.current_boot_ms,
                rejection_code(&error),
            ));
        }
        let Some(session) = self.session.as_ref() else {
            return RuntimeOverrideAction::PublishAck(rejected_ack(
                &control,
                health.current_boot_ms,
                "runtime-rearm-owner-missing",
            ));
        };
        if self.phase != RuntimeOverridePhase::Restored
            || session.restore_reason != Some(RuntimeRestoreReason::RouteDrift)
        {
            return RuntimeOverrideAction::PublishAck(rejected_ack(
                &control,
                health.current_boot_ms,
                "runtime-rearm-not-eligible",
            ));
        }
        if health.restore_intent_present {
            return RuntimeOverrideAction::PublishAck(rejected_ack(
                &control,
                health.current_boot_ms,
                "runtime-rearm-restore-pending",
            ));
        }
        if session.permit != permit {
            return RuntimeOverrideAction::PublishAck(rejected_ack(
                &control,
                health.current_boot_ms,
                "runtime-rearm-owner-mismatch",
            ));
        }
        if session.control.sequence.checked_add(1) != Some(control.sequence) {
            return RuntimeOverrideAction::PublishAck(rejected_ack(
                &control,
                health.current_boot_ms,
                "runtime-rearm-sequence-mismatch",
            ));
        }
        let mut expected = session.control.clone();
        expected.sequence = control.sequence;
        if expected != control {
            return RuntimeOverrideAction::PublishAck(rejected_ack(
                &control,
                health.current_boot_ms,
                "runtime-rearm-control-mismatch",
            ));
        }
        if !health.worker_identity_matches {
            return RuntimeOverrideAction::PublishAck(rejected_ack(
                &control,
                health.current_boot_ms,
                "runtime-rearm-worker-mismatch",
            ));
        }
        if health.route_identity != session.permit.route_identity
            || health.baseline_identity != RuntimeIdentityHealth::Matches
            || !health.baseline_runtime_matches
        {
            // These inputs may still be converging after hotplug recovery.
            // Keep the exact restored baseline and let the operation deadline
            // remain the sole temporal authority; do not invent a retry timer.
            return RuntimeOverrideAction::None;
        }
        self.phase = RuntimeOverridePhase::Applying;
        if let Some(session) = self.session.as_mut() {
            session.control = control.clone();
            session.applied_ack = None;
            session.restore_reason = None;
            session.restore_started_boot_ms = None;
        }
        RuntimeOverrideAction::Apply(control)
    }

    pub fn confirm_applied(
        &mut self,
        attested: &RuntimeSnapshot,
        current_boot_ms: u64,
    ) -> RuntimeOverrideAction {
        if self.phase != RuntimeOverridePhase::Applying {
            return RuntimeOverrideAction::None;
        }
        let Some(session) = self.session.as_ref() else {
            return RuntimeOverrideAction::None;
        };
        if current_boot_ms == 0
            || current_boot_ms >= session.control.deadline_boot_ms
            || attested.validate().is_err()
            || !attested.matches_control(&session.control, &session.permit)
        {
            return self.begin_restore(
                RuntimeRestoreReason::AppliedAttestationMismatch,
                current_boot_ms,
            );
        }
        let ack = applied_ack(&session.control, current_boot_ms);
        self.phase = RuntimeOverridePhase::Applied;
        if let Some(session) = self.session.as_mut() {
            session.applied_ack = Some(ack.clone());
        }
        RuntimeOverrideAction::PublishAck(ack)
    }

    pub fn apply_failed(&mut self, current_boot_ms: u64) -> RuntimeOverrideAction {
        if matches!(
            self.phase,
            RuntimeOverridePhase::Applying | RuntimeOverridePhase::Applied
        ) {
            self.begin_restore(RuntimeRestoreReason::ApplyFailed, current_boot_ms)
        } else {
            RuntimeOverrideAction::None
        }
    }

    pub fn tick(&mut self, health: RuntimeOverrideHealth<'_>) -> RuntimeOverrideAction {
        if !matches!(
            self.phase,
            RuntimeOverridePhase::Applying | RuntimeOverridePhase::Applied
        ) {
            return RuntimeOverrideAction::None;
        }
        let Some(session) = self.session.as_ref() else {
            return RuntimeOverrideAction::None;
        };
        let reason = if !health.permit_present {
            Some(RuntimeRestoreReason::PermitMissing)
        } else if !health.control_present {
            Some(RuntimeRestoreReason::ControlMissing)
        } else if health.current_boot_ms == 0
            || health.current_boot_ms >= session.control.deadline_boot_ms
        {
            Some(RuntimeRestoreReason::DeadlineExpired)
        } else if !health.worker_identity_matches {
            Some(RuntimeRestoreReason::WorkerIdentityMismatch)
        } else if health.route_identity != session.permit.route_identity {
            Some(RuntimeRestoreReason::RouteDrift)
        } else if health.baseline_identity == RuntimeIdentityHealth::Unavailable {
            Some(RuntimeRestoreReason::BaselineAttestationUnavailable)
        } else if health.baseline_identity == RuntimeIdentityHealth::Drifted {
            Some(RuntimeRestoreReason::SqmDrift)
        } else if self.phase == RuntimeOverridePhase::Applied && !health.applied_runtime_matches {
            Some(RuntimeRestoreReason::RuntimeDrift)
        } else {
            None
        };
        reason
            .map(|reason| self.begin_restore(reason, health.current_boot_ms))
            .unwrap_or(RuntimeOverrideAction::None)
    }

    pub fn confirm_restored(
        &mut self,
        attested: &RuntimeRestoreBaseline,
        current_boot_ms: u64,
    ) -> RuntimeOverrideAction {
        if self.phase != RuntimeOverridePhase::Restoring {
            return RuntimeOverrideAction::None;
        }
        let Some(session) = self.session.as_ref() else {
            return RuntimeOverrideAction::None;
        };
        if current_boot_ms == 0
            || attested != &session.baseline
            || attested.validate_restore_baseline().is_err()
        {
            return RuntimeOverrideAction::None;
        }
        let ack = restoration_ack(&session.control, current_boot_ms, RuntimeAckState::Restored);
        self.phase = RuntimeOverridePhase::Restored;
        RuntimeOverrideAction::PublishAck(ack)
    }

    pub fn retry_restore(&self, current_boot_ms: u64) -> RuntimeOverrideAction {
        if self.phase != RuntimeOverridePhase::Restoring {
            return RuntimeOverrideAction::None;
        }
        let Some(session) = self.session.as_ref() else {
            return RuntimeOverrideAction::None;
        };
        let Some(reason) = session.restore_reason else {
            return RuntimeOverrideAction::None;
        };
        RuntimeOverrideAction::Restore {
            baseline: session.baseline.clone(),
            reason,
            ack: restoration_ack(
                &session.control,
                session
                    .restore_started_boot_ms
                    .unwrap_or(current_boot_ms.max(1)),
                RuntimeAckState::Restoring,
            ),
        }
    }

    pub fn restored_ack(&self, current_boot_ms: u64) -> Option<AutotuneRuntimeAck> {
        if self.phase != RuntimeOverridePhase::Restored || current_boot_ms == 0 {
            return None;
        }
        self.session.as_ref().map(|session| {
            restoration_ack(&session.control, current_boot_ms, RuntimeAckState::Restored)
        })
    }

    pub fn release(
        &mut self,
        permit_id: &str,
        job_id: &str,
        worker_run_id: &str,
    ) -> Result<(), String> {
        if self.phase != RuntimeOverridePhase::Restored {
            return Err("runtime override cannot be released before restoration".to_string());
        }
        let session = self
            .session
            .as_ref()
            .ok_or_else(|| "runtime override session is missing".to_string())?;
        if session.permit.permit_id != permit_id
            || session.permit.job_id != job_id
            || session.permit.worker_run_id != worker_run_id
        {
            return Err("runtime override release identity does not match".to_string());
        }
        self.session = None;
        self.phase = RuntimeOverridePhase::Idle;
        Ok(())
    }

    pub fn recover_after_restart(
        instance_name: String,
        permit: AutotuneRuntimePermit,
        control: AutotuneRuntimeControl,
        baseline: RuntimeRestoreBaseline,
        current_boot_ms: u64,
    ) -> Result<(Self, RuntimeOverrideAction), String> {
        permit.validate()?;
        control.validate()?;
        baseline.validate_for_permit(&permit)?;
        permit.authorizes_identity(&instance_name, &control)?;
        let action = RuntimeOverrideAction::Restore {
            baseline: baseline.clone(),
            reason: RuntimeRestoreReason::InstanceRestarted,
            ack: restoration_ack(&control, current_boot_ms.max(1), RuntimeAckState::Restoring),
        };
        Ok((
            Self {
                instance_name,
                phase: RuntimeOverridePhase::Restoring,
                session: Some(RuntimeOverrideSession {
                    permit,
                    baseline,
                    control,
                    applied_ack: None,
                    restore_reason: Some(RuntimeRestoreReason::InstanceRestarted),
                    restore_started_boot_ms: Some(current_boot_ms.max(1)),
                }),
            },
            action,
        ))
    }

    fn begin_restore(
        &mut self,
        reason: RuntimeRestoreReason,
        current_boot_ms: u64,
    ) -> RuntimeOverrideAction {
        let Some(session) = self.session.as_mut() else {
            return RuntimeOverrideAction::None;
        };
        self.phase = RuntimeOverridePhase::Restoring;
        session.restore_reason = Some(reason);
        let restore_started_boot_ms = *session
            .restore_started_boot_ms
            .get_or_insert(current_boot_ms.max(1));
        RuntimeOverrideAction::Restore {
            baseline: session.baseline.clone(),
            reason,
            ack: restoration_ack(
                &session.control,
                restore_started_boot_ms,
                RuntimeAckState::Restoring,
            ),
        }
    }
}

fn applied_ack(control: &AutotuneRuntimeControl, current_boot_ms: u64) -> AutotuneRuntimeAck {
    AutotuneRuntimeAck {
        permit_id: control.permit_id.clone(),
        job_id: control.job_id.clone(),
        worker_run_id: control.worker_run_id.clone(),
        sequence: control.sequence,
        updated_boot_ms: current_boot_ms,
        target_interface: control.target_interface.clone(),
        route_fingerprint: control.route_fingerprint.clone(),
        sqm_fingerprint: control.sqm_fingerprint.clone(),
        state: RuntimeAckState::Applied,
        topology: Some(control.topology),
        download_kbps: control.download_kbps,
        upload_kbps: control.upload_kbps,
        diagnostic_code: None,
    }
}

fn restoration_ack(
    control: &AutotuneRuntimeControl,
    current_boot_ms: u64,
    state: RuntimeAckState,
) -> AutotuneRuntimeAck {
    AutotuneRuntimeAck {
        permit_id: control.permit_id.clone(),
        job_id: control.job_id.clone(),
        worker_run_id: control.worker_run_id.clone(),
        sequence: control.sequence,
        updated_boot_ms: current_boot_ms,
        target_interface: control.target_interface.clone(),
        route_fingerprint: control.route_fingerprint.clone(),
        sqm_fingerprint: control.sqm_fingerprint.clone(),
        state,
        topology: None,
        download_kbps: None,
        upload_kbps: None,
        diagnostic_code: None,
    }
}

fn rejected_ack(
    control: &AutotuneRuntimeControl,
    current_boot_ms: u64,
    diagnostic_code: &str,
) -> AutotuneRuntimeAck {
    AutotuneRuntimeAck {
        permit_id: control.permit_id.clone(),
        job_id: control.job_id.clone(),
        worker_run_id: control.worker_run_id.clone(),
        sequence: control.sequence.max(1),
        updated_boot_ms: current_boot_ms.max(1),
        target_interface: control.target_interface.clone(),
        route_fingerprint: control.route_fingerprint.clone(),
        sqm_fingerprint: control.sqm_fingerprint.clone(),
        state: RuntimeAckState::Rejected,
        topology: None,
        download_kbps: None,
        upload_kbps: None,
        diagnostic_code: Some(diagnostic_code.to_string()),
    }
}

fn rejection_code(error: &str) -> &'static str {
    if error.contains("lifetime") {
        "permit-lifetime-invalid"
    } else if error.contains("bypass") {
        "topology-not-permitted"
    } else if error.contains("range") {
        "rate-not-permitted"
    } else {
        "permit-mismatch"
    }
}

fn require_lower_hex(name: &str, value: &str, length: usize) -> Result<(), String> {
    if value.len() != length
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return Err(format!(
            "{name} must be exactly {length} lowercase hexadecimal characters"
        ));
    }
    Ok(())
}

fn require_identifier(name: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_alphanumeric() && !b"._-".contains(&byte))
    {
        return Err(format!("{name} is invalid"));
    }
    Ok(())
}

fn require_interface(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_alphanumeric() && !b"._:@-".contains(&byte))
    {
        return Err("runtime permit target interface is invalid".to_string());
    }
    Ok(())
}

fn validate_qdisc_handle(name: &str, value: &str) -> Result<(), String> {
    let Some(hex) = value.strip_suffix(':') else {
        return Err(format!("{name} is invalid"));
    };
    if hex.is_empty()
        || hex.len() > 4
        || hex
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return Err(format!("{name} is invalid"));
    }
    Ok(())
}

fn parse_qdisc_handle(value: &str) -> Result<u32, String> {
    validate_qdisc_handle("temporary qdisc handle", value)?;
    let major = u32::from_str_radix(value.trim_end_matches(':'), 16)
        .map_err(|_| "temporary qdisc handle is invalid".to_string())?;
    Ok(major << 16)
}

fn decode_lower_hex(value: &str) -> Result<Vec<u8>, String> {
    if value.len() % 2 != 0 {
        return Err("lower-hex byte string has an odd length".to_string());
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let text = std::str::from_utf8(pair)
                .map_err(|_| "lower-hex byte string is not ASCII".to_string())?;
            u8::from_str_radix(text, 16).map_err(|_| "lower-hex byte string is invalid".to_string())
        })
        .collect()
}

fn require_route_identity(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 512
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b'=')
    {
        return Err("runtime permit route identity is invalid".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_topology_identity_is_deterministic_and_private() {
        let permit_id = "77".repeat(16);
        let identity = TemporaryTopologyIdentity::planned_for_permit(&permit_id).unwrap();
        assert_eq!(identity.ifb_name, "catf77777777");
        assert_eq!(identity.ifb_alias, format!("cake-autotune-{permit_id}"));
        assert_eq!(identity.target_qdisc_handle, "a777:");
        assert_eq!(identity.ifb_qdisc_handle, "b777:");
        assert_eq!(identity.ingress_qdisc_handle, "ffff:");
        assert!(identity.ifb_ifindex.is_none());
        assert_eq!(identity.redirect_preference, 50_039);
        assert_eq!(identity.redirect_filter_handle, 0x8000_0777);
        assert_eq!(identity.redirect_filter_tc_handle().unwrap(), "800::777");
        assert_eq!(identity.redirect_action_index, 0xca77_7777);
        assert_eq!(identity.redirect_action_cookie, permit_id);
        let namespace = identity.private_namespace().unwrap();
        assert_eq!(namespace.link_names, vec![identity.ifb_name.clone()]);
        assert_eq!(namespace.link_aliases, vec![identity.ifb_alias.clone()]);
        assert_eq!(
            namespace.filter_handles,
            vec![PRIVATE_U32_ROOT_FILTER_HANDLE, 0x8000_0777]
        );
        assert_eq!(namespace.filter_priorities, vec![50_039]);
        assert_eq!(namespace.action_identities[0].index, 0xca77_7777);
        assert_eq!(namespace.action_identities[0].cookie, Some(vec![0x77; 16]));
        identity.validate_for_permit(&permit_id).unwrap();

        let other = TemporaryTopologyIdentity::planned_for_permit(&"88".repeat(16)).unwrap();
        assert_ne!(other.ifb_name, identity.ifb_name);
        assert_ne!(
            other.redirect_filter_handle,
            identity.redirect_filter_handle
        );
        assert_ne!(other.redirect_action_index, identity.redirect_action_index);
        assert_ne!(
            other.redirect_action_cookie,
            identity.redirect_action_cookie
        );

        let mut foreign = identity.clone();
        foreign.ifb_alias = "cake-autotune-foreign".to_string();
        assert!(foreign.validate_for_permit(&permit_id).is_err());
        let mut bad_ifindex = identity;
        bad_ifindex.ifb_ifindex = Some(0);
        assert!(bad_ifindex.validate().is_err());
    }

    #[test]
    fn temporary_topology_stage_round_trip_is_closed() {
        for stage in [
            TemporaryTopologyStage::Planned,
            TemporaryTopologyStage::ManagedSqmSuspended,
            TemporaryTopologyStage::AbsenceAttested,
            TemporaryTopologyStage::LinkOwned,
            TemporaryTopologyStage::Active,
            TemporaryTopologyStage::TemporaryAbsent,
            TemporaryTopologyStage::BaselineRestored,
        ] {
            assert_eq!(TemporaryTopologyStage::parse(stage.as_str()), Some(stage));
        }
        assert_eq!(TemporaryTopologyStage::parse("foreign"), None);
    }

    fn worker() -> ProcessIdentity {
        ProcessIdentity {
            pid: 100,
            process_group: 100,
            starttime_ticks: 500,
        }
    }

    fn permit() -> AutotuneRuntimePermit {
        AutotuneRuntimePermit {
            kind: RuntimePermitKind::Autotune,
            permit_id: "77".repeat(16),
            job_id: "11".repeat(16),
            worker_run_id: "22".repeat(16),
            boot_id: "33".repeat(16),
            coordinator_generation: "44".repeat(16),
            worker: worker(),
            instance_name: "wan_sqm".to_string(),
            target_interface: "pppoe-wan".to_string(),
            route_identity: "main||pppoe-wan|192.0.2.1||254".to_string(),
            route_fingerprint: "55".repeat(32),
            sqm_fingerprint: "66".repeat(32),
            deadline_boot_ms: 60_000,
            maximum_sequence: 32,
            profile: AutotuneProfile::BestOverall,
            link_kind: LinkKind::Ethernet,
            baseline: RuntimeBaseline::Managed(MeasurementTopology::ShapedBoth),
            initial_download_kbps: 100_000,
            initial_upload_kbps: 50_000,
            download_qdisc_kind: RuntimeQdiscKind::Cake,
            upload_qdisc_kind: RuntimeQdiscKind::Cake,
            allow_bypass_download: true,
            allow_bypass_upload: true,
            download_bounds: RuntimeRateBounds {
                minimum_kbps: 10_000,
                maximum_kbps: 1_000_000,
            },
            upload_bounds: RuntimeRateBounds {
                minimum_kbps: 5_000,
                maximum_kbps: 500_000,
            },
        }
    }

    fn absent_baseline() -> AbsentRuntimeBaseline {
        AbsentRuntimeBaseline {
            planned_sqm_section: "wan_sqm".to_string(),
            target_interface: "pppoe-wan".to_string(),
            target_ifindex: 7,
            route_fingerprint: "55".repeat(32),
            config_fingerprint: "88".repeat(32),
            sqm_fingerprint: "66".repeat(32),
            kernel_topology_fingerprint: "99".repeat(32),
            kernel_namespace_seed: "77".repeat(16),
        }
    }

    #[test]
    fn absent_permit_baseline_is_explicit_strict_and_target_bound() {
        let mut absent_permit = permit();
        absent_permit.baseline = RuntimeBaseline::Absent(absent_baseline());
        absent_permit.validate().unwrap();
        assert!(absent_permit.managed_baseline_topology().is_err());

        let mut target_drift = absent_permit.clone();
        target_drift.target_interface = "eth9".to_string();
        assert!(target_drift.validate().is_err());
        let mut route_drift = absent_permit.clone();
        route_drift.route_fingerprint = "aa".repeat(32);
        assert!(route_drift.validate().is_err());
        let mut sqm_drift = absent_permit.clone();
        sqm_drift.sqm_fingerprint = "bb".repeat(32);
        assert!(sqm_drift.validate().is_err());
        let mut namespace_drift = absent_permit.clone();
        if let RuntimeBaseline::Absent(absent) = &mut namespace_drift.baseline {
            absent.kernel_namespace_seed = "aa".repeat(16);
        }
        assert!(namespace_drift.validate().is_err());

        let mut invalid_section = absent_permit.clone();
        if let RuntimeBaseline::Absent(absent) = &mut invalid_section.baseline {
            absent.planned_sqm_section = "wan-sqm".to_string();
        }
        assert!(invalid_section.validate().is_err());
        if let RuntimeBaseline::Absent(absent) = &mut invalid_section.baseline {
            absent.planned_sqm_section = "wan.sqm".to_string();
        }
        assert!(invalid_section.validate().is_err());

        if let RuntimeBaseline::Absent(absent) = &mut absent_permit.baseline {
            absent.target_ifindex = 0;
        }
        assert!(absent_permit.validate().is_err());
        if let RuntimeBaseline::Absent(absent) = &mut absent_permit.baseline {
            absent.target_ifindex = i32::MAX as u32 + 1;
        }
        assert!(absent_permit.validate().is_err());
        if let RuntimeBaseline::Absent(absent) = &mut absent_permit.baseline {
            absent.target_ifindex = 7;
            absent.kernel_topology_fingerprint = "not-a-fingerprint".to_string();
        }
        assert!(absent_permit.validate().is_err());

        let mut speedtest = permit();
        speedtest.baseline = RuntimeBaseline::Absent(absent_baseline());
        speedtest.kind = RuntimePermitKind::SpeedtestUnshaped;
        assert!(speedtest.validate().is_err());
    }

    #[test]
    fn absent_baseline_reconstructs_the_exact_route_bound_kernel_query() {
        let baseline = absent_baseline();
        let route = OperationRouteIdentity {
            mode: super::super::protocol::OperationRouteMode::Main,
            mwan3_member: None,
            l3_device: "pppoe-wan".to_string(),
            source_ip: None,
            fwmark: None,
            routing_table: None,
        };
        let query = baseline.kernel_topology_query(route.clone()).unwrap();
        assert_eq!(query.target_interface, baseline.target_interface);
        assert_eq!(query.route, route);
        assert_eq!(
            query.private_namespace,
            TemporaryTopologyIdentity::planned_for_permit(&baseline.kernel_namespace_seed)
                .unwrap()
                .private_namespace()
                .unwrap()
        );

        let mut other = baseline;
        other.kernel_namespace_seed = "88".repeat(16);
        assert_ne!(
            other
                .kernel_topology_query(query.route.clone())
                .unwrap()
                .private_namespace,
            query.private_namespace
        );
    }

    fn control(sequence: u32) -> AutotuneRuntimeControl {
        let permit = permit();
        AutotuneRuntimeControl {
            permit_id: permit.permit_id,
            job_id: permit.job_id,
            worker_run_id: permit.worker_run_id,
            boot_id: permit.boot_id,
            coordinator_generation: permit.coordinator_generation,
            worker: permit.worker,
            sequence,
            deadline_boot_ms: 30_000,
            target_interface: permit.target_interface,
            route_fingerprint: permit.route_fingerprint,
            sqm_fingerprint: permit.sqm_fingerprint,
            topology: MeasurementTopology::ShapedBoth,
            download_kbps: Some(100_000),
            upload_kbps: Some(50_000),
        }
    }

    #[test]
    fn tracker_accepts_an_exact_absent_baseline_as_typed_restore_authority() {
        let mut tracker = RuntimeOverrideTracker::new("wan_sqm".to_string()).unwrap();
        let mut permit = permit();
        permit.baseline = RuntimeBaseline::Absent(absent_baseline());
        let action = tracker.submit(
            permit,
            control(1),
            Some(RuntimeBaseline::Absent(absent_baseline())),
            1_000,
        );
        assert!(matches!(action, RuntimeOverrideAction::Apply(_)));
        assert_eq!(tracker.phase(), RuntimeOverridePhase::Applying);
        assert!(matches!(
            tracker.baseline(),
            Some(RuntimeBaseline::Absent(actual)) if actual == &absent_baseline()
        ));
    }

    #[test]
    fn unshaped_speedtest_permit_authorizes_only_its_exact_directional_control() {
        for direction in [
            SpeedtestDirection::Download,
            SpeedtestDirection::Upload,
            SpeedtestDirection::Both,
        ] {
            let topology = speedtest_unshaped_topology(direction);
            let mut permit = permit();
            permit.kind = RuntimePermitKind::SpeedtestUnshaped;
            permit.maximum_sequence = 1;
            permit.profile = AutotuneProfile::BestOverall;
            permit.link_kind = LinkKind::Unknown;
            permit.allow_bypass_download = !topology.download_is_shaped();
            permit.allow_bypass_upload = !topology.upload_is_shaped();
            permit.download_bounds = RuntimeRateBounds {
                minimum_kbps: permit.initial_download_kbps,
                maximum_kbps: permit.initial_download_kbps,
            };
            permit.upload_bounds = RuntimeRateBounds {
                minimum_kbps: permit.initial_upload_kbps,
                maximum_kbps: permit.initial_upload_kbps,
            };
            permit.validate().unwrap();

            for one_sided in [
                MeasurementTopology::DownloadOnlyShaped,
                MeasurementTopology::UploadOnlyShaped,
            ] {
                let mut unsafe_baseline = permit.clone();
                unsafe_baseline.baseline = RuntimeBaseline::Managed(one_sided);
                assert!(unsafe_baseline.validate().is_err());
            }

            let mut control = control(1);
            control.topology = topology;
            control.download_kbps = topology
                .download_is_shaped()
                .then_some(permit.initial_download_kbps);
            control.upload_kbps = topology
                .upload_is_shaped()
                .then_some(permit.initial_upload_kbps);
            permit.authorizes("wan_sqm", &control, 1_000).unwrap();

            let mut mismatched = control.clone();
            mismatched.topology = MeasurementTopology::ShapedBoth;
            mismatched.download_kbps = Some(permit.initial_download_kbps);
            mismatched.upload_kbps = Some(permit.initial_upload_kbps);
            assert!(permit.authorizes("wan_sqm", &mismatched, 1_000).is_err());

            let mut omitted_retained_rate = control.clone();
            if topology.download_is_shaped() {
                omitted_retained_rate.download_kbps = None;
            } else if topology.upload_is_shaped() {
                omitted_retained_rate.upload_kbps = None;
            }
            if topology != MeasurementTopology::RawBoth {
                assert!(permit
                    .authorizes("wan_sqm", &omitted_retained_rate, 1_000)
                    .is_err());
            }

            let mut repeated = control;
            repeated.sequence = 2;
            assert!(permit.authorizes("wan_sqm", &repeated, 1_000).is_err());
        }

        let mut no_direction = permit();
        no_direction.kind = RuntimePermitKind::SpeedtestUnshaped;
        no_direction.maximum_sequence = 1;
        no_direction.link_kind = LinkKind::Unknown;
        no_direction.allow_bypass_download = false;
        no_direction.allow_bypass_upload = false;
        no_direction.download_bounds = RuntimeRateBounds {
            minimum_kbps: no_direction.initial_download_kbps,
            maximum_kbps: no_direction.initial_download_kbps,
        };
        no_direction.upload_bounds = RuntimeRateBounds {
            minimum_kbps: no_direction.initial_upload_kbps,
            maximum_kbps: no_direction.initial_upload_kbps,
        };
        assert!(no_direction.validate().is_err());
    }

    fn baseline() -> RuntimeSnapshot {
        RuntimeSnapshot {
            target_interface: "pppoe-wan".to_string(),
            route_fingerprint: "55".repeat(32),
            sqm_fingerprint: "66".repeat(32),
            topology: MeasurementTopology::ShapedBoth,
            download_kbps: Some(90_000),
            upload_kbps: Some(45_000),
            download_qdisc_kind: Some(RuntimeQdiscKind::Cake),
            upload_qdisc_kind: Some(RuntimeQdiscKind::Cake),
        }
    }

    #[test]
    fn managed_baseline_qdisc_authority_is_exact_and_missing_directions_use_private_cake() {
        let mut cake_mq_permit = permit();
        cake_mq_permit.download_qdisc_kind = RuntimeQdiscKind::CakeMq;
        cake_mq_permit.upload_qdisc_kind = RuntimeQdiscKind::CakeMq;
        let mut cake_mq_baseline = baseline();
        cake_mq_baseline.download_qdisc_kind = Some(RuntimeQdiscKind::CakeMq);
        cake_mq_baseline.upload_qdisc_kind = Some(RuntimeQdiscKind::CakeMq);
        RuntimeBaseline::Managed(cake_mq_baseline.clone())
            .validate_for_permit(&cake_mq_permit)
            .unwrap();

        cake_mq_baseline.download_qdisc_kind = Some(RuntimeQdiscKind::Cake);
        assert!(RuntimeBaseline::Managed(cake_mq_baseline)
            .validate_for_permit(&cake_mq_permit)
            .is_err());

        let mut download_only = permit();
        download_only.baseline = RuntimeBaseline::Managed(MeasurementTopology::DownloadOnlyShaped);
        download_only.upload_qdisc_kind = RuntimeQdiscKind::CakeMq;
        assert!(download_only.validate().is_err());
        download_only.upload_qdisc_kind = RuntimeQdiscKind::Cake;
        download_only.validate().unwrap();
    }

    fn applied_snapshot(control: &AutotuneRuntimeControl) -> RuntimeSnapshot {
        RuntimeSnapshot {
            target_interface: control.target_interface.clone(),
            route_fingerprint: control.route_fingerprint.clone(),
            sqm_fingerprint: control.sqm_fingerprint.clone(),
            topology: control.topology,
            download_kbps: control.download_kbps,
            upload_kbps: control.upload_kbps,
            download_qdisc_kind: control.download_kbps.map(|_| RuntimeQdiscKind::Cake),
            upload_qdisc_kind: control.upload_kbps.map(|_| RuntimeQdiscKind::Cake),
        }
    }

    fn activate(tracker: &mut RuntimeOverrideTracker) -> AutotuneRuntimeControl {
        let control = control(1);
        assert_eq!(
            tracker.submit(
                permit(),
                control.clone(),
                Some(RuntimeBaseline::Managed(baseline())),
                1_000,
            ),
            RuntimeOverrideAction::Apply(control.clone())
        );
        assert!(matches!(
            tracker.confirm_applied(&applied_snapshot(&control), 2_000),
            RuntimeOverrideAction::PublishAck(AutotuneRuntimeAck {
                state: RuntimeAckState::Applied,
                ..
            })
        ));
        control
    }

    fn restore_with_reason(
        reason: RuntimeRestoreReason,
    ) -> (RuntimeOverrideTracker, AutotuneRuntimeControl) {
        let mut tracker = RuntimeOverrideTracker::new("wan_sqm".to_string()).unwrap();
        let control = activate(&mut tracker);
        assert!(matches!(
            tracker.begin_restore(reason, 3_000),
            RuntimeOverrideAction::Restore { .. }
        ));
        assert!(matches!(
            tracker.confirm_restored(&RuntimeBaseline::Managed(baseline()), 4_000),
            RuntimeOverrideAction::PublishAck(AutotuneRuntimeAck {
                state: RuntimeAckState::Restored,
                ..
            })
        ));
        (tracker, control)
    }

    fn ready_route_rearm_health() -> RuntimeRouteRearmHealth<'static> {
        RuntimeRouteRearmHealth {
            current_boot_ms: 5_000,
            restore_intent_present: false,
            worker_identity_matches: true,
            route_identity: "main||pppoe-wan|192.0.2.1||254",
            baseline_identity: RuntimeIdentityHealth::Matches,
            baseline_runtime_matches: true,
        }
    }

    #[test]
    fn valid_permit_applies_and_duplicate_control_is_idempotent() {
        let mut tracker = RuntimeOverrideTracker::new("wan_sqm".to_string()).unwrap();
        let control = activate(&mut tracker);
        assert_eq!(tracker.phase(), RuntimeOverridePhase::Applied);
        assert!(matches!(
            tracker.submit(permit(), control, None, 2_500),
            RuntimeOverrideAction::PublishAck(AutotuneRuntimeAck {
                state: RuntimeAckState::Applied,
                ..
            })
        ));
    }

    #[test]
    fn permit_bounds_topology_rates_and_lifetime() {
        let permit = permit();
        let mut raw = control(1);
        raw.topology = MeasurementTopology::RawDownload;
        raw.download_kbps = None;
        assert!(permit.authorizes("wan_sqm", &raw, 1_000).is_ok());
        let mut no_bypass = permit.clone();
        no_bypass.allow_bypass_download = false;
        assert!(no_bypass
            .authorizes("wan_sqm", &raw, 1_000)
            .unwrap_err()
            .contains("bypass"));
        let mut excessive = control(1);
        excessive.download_kbps = Some(1_100_000);
        assert!(permit
            .authorizes("wan_sqm", &excessive, 1_000)
            .unwrap_err()
            .contains("range"));
        let mut late = control(1);
        late.deadline_boot_ms = 70_000;
        assert!(permit
            .authorizes("wan_sqm", &late, 1_000)
            .unwrap_err()
            .contains("lifetime"));
    }

    #[test]
    fn foreign_job_is_rejected_without_displacing_live_owner() {
        let mut tracker = RuntimeOverrideTracker::new("wan_sqm".to_string()).unwrap();
        activate(&mut tracker);
        let mut foreign_permit = permit();
        foreign_permit.permit_id = "88".repeat(16);
        foreign_permit.job_id = "99".repeat(16);
        let mut foreign_control = control(1);
        foreign_control.permit_id = foreign_permit.permit_id.clone();
        foreign_control.job_id = foreign_permit.job_id.clone();
        assert!(matches!(
            tracker.submit(
                foreign_permit,
                foreign_control,
                Some(RuntimeBaseline::Managed(baseline())),
                3_000,
            ),
            RuntimeOverrideAction::PublishAck(AutotuneRuntimeAck {
                state: RuntimeAckState::Rejected,
                ..
            })
        ));
        assert_eq!(tracker.phase(), RuntimeOverridePhase::Applied);
    }

    #[test]
    fn rewritten_or_gapped_sequence_forces_restoration() {
        let mut rewritten = RuntimeOverrideTracker::new("wan_sqm".to_string()).unwrap();
        let mut duplicate = activate(&mut rewritten);
        duplicate.download_kbps = Some(95_000);
        assert!(matches!(
            rewritten.submit(permit(), duplicate, None, 3_000),
            RuntimeOverrideAction::Restore {
                reason: RuntimeRestoreReason::SequenceConflict,
                ..
            }
        ));

        let mut gapped = RuntimeOverrideTracker::new("wan_sqm".to_string()).unwrap();
        activate(&mut gapped);
        assert!(matches!(
            gapped.submit(permit(), control(3), None, 3_000),
            RuntimeOverrideAction::Restore {
                reason: RuntimeRestoreReason::SequenceConflict,
                ..
            }
        ));
    }

    #[test]
    fn exact_next_sequence_can_be_applied() {
        let mut tracker = RuntimeOverrideTracker::new("wan_sqm".to_string()).unwrap();
        activate(&mut tracker);
        let next = control(2);
        assert_eq!(
            tracker.submit(permit(), next.clone(), None, 3_000),
            RuntimeOverrideAction::Apply(next.clone())
        );
        assert!(matches!(
            tracker.confirm_applied(&applied_snapshot(&next), 4_000),
            RuntimeOverrideAction::PublishAck(AutotuneRuntimeAck {
                state: RuntimeAckState::Applied,
                sequence: 2,
                ..
            })
        ));
    }

    #[test]
    fn route_drift_rearms_only_the_exact_interrupted_candidate() {
        let (mut tracker, interrupted) = restore_with_reason(RuntimeRestoreReason::RouteDrift);
        let mut rearmed = interrupted.clone();
        rearmed.sequence += 1;
        assert_eq!(
            tracker.rearm_after_route_recovery(
                permit(),
                rearmed.clone(),
                ready_route_rearm_health(),
            ),
            RuntimeOverrideAction::Apply(rearmed.clone())
        );
        assert_eq!(tracker.phase(), RuntimeOverridePhase::Applying);
        assert_eq!(
            tracker.baseline(),
            Some(&RuntimeBaseline::Managed(baseline()))
        );
        assert!(matches!(
            tracker.confirm_applied(&applied_snapshot(&rearmed), 6_000),
            RuntimeOverrideAction::PublishAck(AutotuneRuntimeAck {
                state: RuntimeAckState::Applied,
                sequence: 2,
                ..
            })
        ));
    }

    #[test]
    fn route_rearm_rejects_every_other_restore_reason() {
        for reason in [
            RuntimeRestoreReason::ApplyFailed,
            RuntimeRestoreReason::AppliedAttestationMismatch,
            RuntimeRestoreReason::ControlMissing,
            RuntimeRestoreReason::PermitMissing,
            RuntimeRestoreReason::DeadlineExpired,
            RuntimeRestoreReason::WorkerIdentityMismatch,
            RuntimeRestoreReason::SqmDrift,
            RuntimeRestoreReason::RuntimeDrift,
            RuntimeRestoreReason::SequenceConflict,
            RuntimeRestoreReason::InstanceRestarted,
        ] {
            let (mut tracker, interrupted) = restore_with_reason(reason);
            let mut rearmed = interrupted;
            rearmed.sequence += 1;
            assert!(matches!(
                tracker.rearm_after_route_recovery(permit(), rearmed, ready_route_rearm_health(),),
                RuntimeOverrideAction::PublishAck(AutotuneRuntimeAck {
                    state: RuntimeAckState::Rejected,
                    ..
                })
            ));
            assert_eq!(tracker.phase(), RuntimeOverridePhase::Restored);
        }
    }

    #[test]
    fn route_rearm_rejects_changed_payload_sequence_owner_or_restore_intent() {
        let changed_controls: Vec<AutotuneRuntimeControl> = {
            let base = control(2);
            let mut values = Vec::new();
            let mut value = base.clone();
            value.download_kbps = Some(99_000);
            values.push(value);
            let mut value = base.clone();
            value.topology = MeasurementTopology::RawDownload;
            value.download_kbps = None;
            values.push(value);
            let mut value = base.clone();
            value.deadline_boot_ms -= 1;
            values.push(value);
            let mut value = base.clone();
            value.boot_id = "aa".repeat(16);
            values.push(value);
            let mut value = base.clone();
            value.coordinator_generation = "bb".repeat(16);
            values.push(value);
            let mut value = base.clone();
            value.worker.starttime_ticks += 1;
            values.push(value);
            let mut value = base.clone();
            value.sequence = 1;
            values.push(value);
            let mut value = base;
            value.sequence = 3;
            values.push(value);
            values
        };
        for changed in changed_controls {
            let (mut tracker, _) = restore_with_reason(RuntimeRestoreReason::RouteDrift);
            assert!(matches!(
                tracker.rearm_after_route_recovery(permit(), changed, ready_route_rearm_health(),),
                RuntimeOverrideAction::PublishAck(AutotuneRuntimeAck {
                    state: RuntimeAckState::Rejected,
                    ..
                })
            ));
            assert_eq!(tracker.phase(), RuntimeOverridePhase::Restored);
        }

        let (mut tracker, interrupted) = restore_with_reason(RuntimeRestoreReason::RouteDrift);
        let mut rearmed = interrupted;
        rearmed.sequence += 1;
        let mut health = ready_route_rearm_health();
        health.restore_intent_present = true;
        assert!(matches!(
            tracker.rearm_after_route_recovery(permit(), rearmed, health),
            RuntimeOverrideAction::PublishAck(AutotuneRuntimeAck {
                state: RuntimeAckState::Rejected,
                ..
            })
        ));
        assert_eq!(tracker.phase(), RuntimeOverridePhase::Restored);
    }

    #[test]
    fn route_rearm_waits_for_exact_route_sqm_and_baseline_without_a_private_timer() {
        for mutate in 0..3 {
            let (mut tracker, interrupted) = restore_with_reason(RuntimeRestoreReason::RouteDrift);
            let mut rearmed = interrupted;
            rearmed.sequence += 1;
            let mut health = ready_route_rearm_health();
            match mutate {
                0 => health.route_identity = "mwan3|wan|eth9|192.0.2.1||1001",
                1 => health.baseline_identity = RuntimeIdentityHealth::Drifted,
                _ => health.baseline_runtime_matches = false,
            }
            assert_eq!(
                tracker.rearm_after_route_recovery(permit(), rearmed, health),
                RuntimeOverrideAction::None
            );
            assert_eq!(tracker.phase(), RuntimeOverridePhase::Restored);
        }
    }

    #[test]
    fn every_liveness_or_drift_failure_enters_restore() {
        let cases = [
            (
                false,
                true,
                true,
                "55",
                RuntimeIdentityHealth::Matches,
                true,
                RuntimeRestoreReason::PermitMissing,
            ),
            (
                true,
                false,
                true,
                "55",
                RuntimeIdentityHealth::Matches,
                true,
                RuntimeRestoreReason::ControlMissing,
            ),
            (
                true,
                true,
                false,
                "55",
                RuntimeIdentityHealth::Matches,
                true,
                RuntimeRestoreReason::WorkerIdentityMismatch,
            ),
            (
                true,
                true,
                true,
                "00",
                RuntimeIdentityHealth::Matches,
                true,
                RuntimeRestoreReason::RouteDrift,
            ),
            (
                true,
                true,
                true,
                "55",
                RuntimeIdentityHealth::Drifted,
                true,
                RuntimeRestoreReason::SqmDrift,
            ),
            (
                true,
                true,
                true,
                "55",
                RuntimeIdentityHealth::Unavailable,
                true,
                RuntimeRestoreReason::BaselineAttestationUnavailable,
            ),
            (
                true,
                true,
                true,
                "55",
                RuntimeIdentityHealth::Matches,
                false,
                RuntimeRestoreReason::RuntimeDrift,
            ),
        ];
        for (permit_present, control_present, worker_matches, route, identity, runtime, expected) in
            cases
        {
            let mut tracker = RuntimeOverrideTracker::new("wan_sqm".to_string()).unwrap();
            activate(&mut tracker);
            let route = if route == "55" {
                "main||pppoe-wan|192.0.2.1||254".to_string()
            } else {
                "main||eth0|192.0.2.2||254".to_string()
            };
            assert!(matches!(
                tracker.tick(RuntimeOverrideHealth {
                    current_boot_ms: 5_000,
                    permit_present,
                    control_present,
                    worker_identity_matches: worker_matches,
                    route_identity: &route,
                    baseline_identity: identity,
                    applied_runtime_matches: runtime,
                }),
                RuntimeOverrideAction::Restore { reason, .. } if reason == expected
            ));
        }
    }

    #[test]
    fn deadline_and_pid_reuse_are_fail_closed() {
        let mut expired = RuntimeOverrideTracker::new("wan_sqm".to_string()).unwrap();
        activate(&mut expired);
        assert!(matches!(
            expired.tick(RuntimeOverrideHealth {
                current_boot_ms: 30_000,
                permit_present: true,
                control_present: true,
                worker_identity_matches: true,
                route_identity: "main||pppoe-wan|192.0.2.1||254",
                baseline_identity: RuntimeIdentityHealth::Matches,
                applied_runtime_matches: true,
            }),
            RuntimeOverrideAction::Restore {
                reason: RuntimeRestoreReason::DeadlineExpired,
                ..
            }
        ));

        let mut reused = RuntimeOverrideTracker::new("wan_sqm".to_string()).unwrap();
        activate(&mut reused);
        assert!(matches!(
            reused.tick(RuntimeOverrideHealth {
                current_boot_ms: 5_000,
                permit_present: true,
                control_present: true,
                worker_identity_matches: false,
                route_identity: "main||pppoe-wan|192.0.2.1||254",
                baseline_identity: RuntimeIdentityHealth::Matches,
                applied_runtime_matches: true,
            }),
            RuntimeOverrideAction::Restore {
                reason: RuntimeRestoreReason::WorkerIdentityMismatch,
                ..
            }
        ));
    }

    #[test]
    fn restore_requires_exact_baseline_before_release() {
        let mut tracker = RuntimeOverrideTracker::new("wan_sqm".to_string()).unwrap();
        let control = activate(&mut tracker);
        let restoring = tracker.apply_failed(3_500);
        assert!(matches!(
            restoring,
            RuntimeOverrideAction::Restore {
                ack: AutotuneRuntimeAck {
                    state: RuntimeAckState::Restoring,
                    ..
                },
                ..
            }
        ));
        let mut mismatch = baseline();
        mismatch.download_kbps = Some(80_000);
        assert_eq!(
            tracker.confirm_restored(&RuntimeBaseline::Managed(mismatch), 4_000),
            RuntimeOverrideAction::None
        );
        assert_eq!(tracker.phase(), RuntimeOverridePhase::Restoring);
        assert!(matches!(
            tracker.confirm_restored(&RuntimeBaseline::Managed(baseline()), 4_500),
            RuntimeOverrideAction::PublishAck(AutotuneRuntimeAck {
                state: RuntimeAckState::Restored,
                ..
            })
        ));
        assert!(tracker
            .release("bad", &control.job_id, &control.worker_run_id)
            .is_err());
        tracker
            .release(&control.permit_id, &control.job_id, &control.worker_run_id)
            .unwrap();
        assert_eq!(tracker.phase(), RuntimeOverridePhase::Idle);
    }

    #[test]
    fn restart_never_adopts_an_override_as_the_new_baseline() {
        let (mut tracker, action) = RuntimeOverrideTracker::recover_after_restart(
            "wan_sqm".to_string(),
            permit(),
            control(1),
            RuntimeBaseline::Managed(baseline()),
            1_500,
        )
        .unwrap();
        assert!(matches!(
            action,
            RuntimeOverrideAction::Restore {
                reason: RuntimeRestoreReason::InstanceRestarted,
                ..
            }
        ));
        assert_eq!(tracker.phase(), RuntimeOverridePhase::Restoring);
        assert!(matches!(
            tracker.confirm_restored(&RuntimeBaseline::Managed(baseline()), 2_000),
            RuntimeOverrideAction::PublishAck(AutotuneRuntimeAck {
                state: RuntimeAckState::Restored,
                ..
            })
        ));

        let mut mismatched = control(1);
        mismatched.coordinator_generation = "aa".repeat(16);
        assert!(RuntimeOverrideTracker::recover_after_restart(
            "wan_sqm".to_string(),
            permit(),
            mismatched,
            RuntimeBaseline::Managed(baseline()),
            1_500,
        )
        .is_err());
    }

    #[test]
    fn malformed_control_never_produces_an_unencodable_rejection_ack() {
        let mut tracker = RuntimeOverrideTracker::new("wan_sqm".to_string()).unwrap();
        let mut malformed = control(1);
        malformed.target_interface = "bad interface".to_string();
        assert_eq!(
            tracker.submit(
                permit(),
                malformed,
                Some(RuntimeBaseline::Managed(baseline())),
                1_000,
            ),
            RuntimeOverrideAction::MalformedControl
        );
        assert_eq!(tracker.phase(), RuntimeOverridePhase::Idle);
    }
}
