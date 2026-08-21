//! Canonical private tmpfs records for the instance-side Auto-Tune runtime owner.

use super::autotune_runtime::{
    AbsentRuntimeBaseline, AutotuneRuntimePermit, RuntimeBaseline, RuntimePermitKind,
    RuntimeRateBounds, RuntimeRestoreBaseline, RuntimeSnapshot, TemporaryTopologyIdentity,
    TemporaryTopologyStage,
};
use super::full_autotune::{AutotuneRuntimeAck, AutotuneRuntimeControl, MeasurementTopology};
use super::identity::ProcessIdentity;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

const MAX_RUNTIME_RECORD_BYTES: usize = 4 * 1024;
const RUNTIME_DIR: &str = "autotune-runtime";
const PERMIT_FILE: &str = "permit.record";
const CONTROL_FILE: &str = "control.record";
const RESTORE_FILE: &str = "restore.record";
const ACK_FILE: &str = "ack.record";
const CHECKPOINT_FILE: &str = "checkpoint.record";
const PERMIT_HEADER: &str = "cake-autorate-autotune-runtime-permit\t3";
const VARIANT_PERMIT_HEADER: &str = "cake-autorate-autotune-runtime-permit\t4";
const NAMESPACE_PERMIT_HEADER: &str = "cake-autorate-autotune-runtime-permit\t5";
const CHECKPOINT_HEADER: &str = "cake-autorate-autotune-runtime-checkpoint\t2";
const ABSENT_CHECKPOINT_HEADER: &str = "cake-autorate-autotune-runtime-checkpoint\t3";
const NAMESPACE_CHECKPOINT_HEADER: &str = "cake-autorate-autotune-runtime-checkpoint\t4";
const EXECUTABLE_ABSENT_CHECKPOINT_HEADER: &str = "cake-autorate-autotune-runtime-checkpoint\t5";
static TEMP_SEQUENCE: AtomicU32 = AtomicU32::new(1);

unsafe extern "C" {
    fn geteuid() -> u32;
    fn getpid() -> i32;
}

impl AutotuneRuntimePermit {
    pub fn encode(&self) -> Result<String, String> {
        self.encode_for_schema(match &self.baseline {
            RuntimeBaseline::Managed(_) => 3,
            RuntimeBaseline::Absent(_) => 5,
        })
    }

    fn encode_for_schema(&self, schema: u8) -> Result<String, String> {
        if !(3..=5).contains(&schema) {
            return Err("unsupported runtime permit schema".to_string());
        }
        if schema < 5 && matches!(&self.baseline, RuntimeBaseline::Absent(_)) {
            return Err(
                "runtime permit schema predates deterministic absent namespace authority"
                    .to_string(),
            );
        }
        self.validate()?;
        let mut fields = vec![
            ("permit_id", self.permit_id.clone()),
            ("permit_kind", self.kind.as_str().to_string()),
        ];
        fields.extend([
            ("job_id", self.job_id.clone()),
            ("worker_run_id", self.worker_run_id.clone()),
            ("boot_id", self.boot_id.clone()),
            (
                "coordinator_generation",
                self.coordinator_generation.clone(),
            ),
            ("worker_pid", self.worker.pid.to_string()),
            (
                "worker_process_group",
                self.worker.process_group.to_string(),
            ),
            (
                "worker_starttime_ticks",
                self.worker.starttime_ticks.to_string(),
            ),
            ("instance_name", self.instance_name.clone()),
            ("target_interface", self.target_interface.clone()),
            ("route_identity", self.route_identity.clone()),
            ("route_fingerprint", self.route_fingerprint.clone()),
            ("sqm_fingerprint", self.sqm_fingerprint.clone()),
            ("deadline_boot_ms", self.deadline_boot_ms.to_string()),
            ("maximum_sequence", self.maximum_sequence.to_string()),
            ("profile", self.profile.as_str().to_string()),
            ("link_kind", self.link_kind.as_str().to_string()),
        ]);
        if schema >= 4 {
            let (
                baseline_state,
                baseline_topology,
                absent_planned_sqm_section,
                absent_target_interface,
                absent_target_ifindex,
                absent_route_fingerprint,
                absent_config_fingerprint,
                absent_sqm_fingerprint,
                absent_kernel_topology_fingerprint,
                absent_kernel_namespace_seed,
            ) = match &self.baseline {
                RuntimeBaseline::Managed(topology) => (
                    "managed",
                    topology.as_str().to_string(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                    String::new(),
                ),
                RuntimeBaseline::Absent(absent) => (
                    "absent",
                    String::new(),
                    absent.planned_sqm_section.clone(),
                    absent.target_interface.clone(),
                    absent.target_ifindex.to_string(),
                    absent.route_fingerprint.clone(),
                    absent.config_fingerprint.clone(),
                    absent.sqm_fingerprint.clone(),
                    absent.kernel_topology_fingerprint.clone(),
                    absent.kernel_namespace_seed.clone(),
                ),
            };
            fields.extend([
                ("baseline_state", baseline_state.to_string()),
                ("baseline_topology", baseline_topology),
                ("absent_planned_sqm_section", absent_planned_sqm_section),
                ("absent_target_interface", absent_target_interface),
                ("absent_target_ifindex", absent_target_ifindex),
                ("absent_route_fingerprint", absent_route_fingerprint),
                ("absent_config_fingerprint", absent_config_fingerprint),
                ("absent_sqm_fingerprint", absent_sqm_fingerprint),
                (
                    "absent_kernel_topology_fingerprint",
                    absent_kernel_topology_fingerprint,
                ),
            ]);
            if schema >= 5 {
                fields.push(("absent_kernel_namespace_seed", absent_kernel_namespace_seed));
            }
        } else {
            let RuntimeBaseline::Managed(topology) = &self.baseline else {
                unreachable!("absent baselines were rejected for managed schemas")
            };
            fields.push(("baseline_topology", topology.as_str().to_string()));
        }
        fields.extend([
            (
                "initial_download_kbps",
                self.initial_download_kbps.to_string(),
            ),
            ("initial_upload_kbps", self.initial_upload_kbps.to_string()),
            (
                "download_qdisc_kind",
                self.download_qdisc_kind.as_str().to_string(),
            ),
            (
                "upload_qdisc_kind",
                self.upload_qdisc_kind.as_str().to_string(),
            ),
            (
                "allow_bypass_download",
                bool_text(self.allow_bypass_download).to_string(),
            ),
            (
                "allow_bypass_upload",
                bool_text(self.allow_bypass_upload).to_string(),
            ),
            (
                "download_minimum_kbps",
                self.download_bounds.minimum_kbps.to_string(),
            ),
            (
                "download_maximum_kbps",
                self.download_bounds.maximum_kbps.to_string(),
            ),
            (
                "upload_minimum_kbps",
                self.upload_bounds.minimum_kbps.to_string(),
            ),
            (
                "upload_maximum_kbps",
                self.upload_bounds.maximum_kbps.to_string(),
            ),
        ]);
        encode_record(
            match schema {
                3 => PERMIT_HEADER,
                4 => VARIANT_PERMIT_HEADER,
                5 => NAMESPACE_PERMIT_HEADER,
                _ => unreachable!(),
            },
            &fields,
        )
    }

    pub fn decode(input: &str) -> Result<Self, String> {
        let schema = match input.lines().next() {
            Some(NAMESPACE_PERMIT_HEADER) => 5,
            Some(VARIANT_PERMIT_HEADER) => 4,
            Some(PERMIT_HEADER) => 3,
            _ => return Err("runtime permit record header is invalid".to_string()),
        };
        let mut reader = RecordReader::new(
            input,
            match schema {
                3 => PERMIT_HEADER,
                4 => VARIANT_PERMIT_HEADER,
                5 => NAMESPACE_PERMIT_HEADER,
                _ => unreachable!(),
            },
        )?;
        let permit = Self {
            permit_id: reader.field("permit_id")?,
            kind: RuntimePermitKind::parse(&reader.field("permit_kind")?)
                .ok_or_else(|| "runtime permit kind is unsupported".to_string())?,
            job_id: reader.field("job_id")?,
            worker_run_id: reader.field("worker_run_id")?,
            boot_id: reader.field("boot_id")?,
            coordinator_generation: reader.field("coordinator_generation")?,
            worker: ProcessIdentity {
                pid: parse_u32("worker_pid", &reader.field("worker_pid")?)?,
                process_group: parse_u32(
                    "worker_process_group",
                    &reader.field("worker_process_group")?,
                )?,
                starttime_ticks: parse_u64(
                    "worker_starttime_ticks",
                    &reader.field("worker_starttime_ticks")?,
                )?,
            },
            instance_name: reader.field("instance_name")?,
            target_interface: reader.field("target_interface")?,
            route_identity: reader.field("route_identity")?,
            route_fingerprint: reader.field("route_fingerprint")?,
            sqm_fingerprint: reader.field("sqm_fingerprint")?,
            deadline_boot_ms: parse_u64("deadline_boot_ms", &reader.field("deadline_boot_ms")?)?,
            maximum_sequence: parse_u32("maximum_sequence", &reader.field("maximum_sequence")?)?,
            profile: crate::autotune::AutotuneProfile::parse(&reader.field("profile")?)
                .ok_or_else(|| "runtime permit profile is unsupported".to_string())?,
            link_kind: crate::autotune::LinkKind::parse(&reader.field("link_kind")?)
                .ok_or_else(|| "runtime permit link kind is unsupported".to_string())?,
            baseline: if schema >= 4 {
                let state = reader.field("baseline_state")?;
                let topology = reader.field("baseline_topology")?;
                let planned_sqm_section = reader.field("absent_planned_sqm_section")?;
                let absent_target_interface = reader.field("absent_target_interface")?;
                let target_ifindex = reader.field("absent_target_ifindex")?;
                let absent_route_fingerprint = reader.field("absent_route_fingerprint")?;
                let config_fingerprint = reader.field("absent_config_fingerprint")?;
                let absent_sqm_fingerprint = reader.field("absent_sqm_fingerprint")?;
                let kernel_topology_fingerprint =
                    reader.field("absent_kernel_topology_fingerprint")?;
                let kernel_namespace_seed = if schema >= 5 {
                    reader.field("absent_kernel_namespace_seed")?
                } else {
                    String::new()
                };
                match state.as_str() {
                    "managed" => {
                        if !planned_sqm_section.is_empty()
                            || !absent_target_interface.is_empty()
                            || !target_ifindex.is_empty()
                            || !absent_route_fingerprint.is_empty()
                            || !config_fingerprint.is_empty()
                            || !absent_sqm_fingerprint.is_empty()
                            || !kernel_topology_fingerprint.is_empty()
                            || !kernel_namespace_seed.is_empty()
                        {
                            return Err(
                                "managed runtime permit carries absent baseline fields".to_string()
                            );
                        }
                        RuntimeBaseline::Managed(MeasurementTopology::parse(&topology).ok_or_else(
                            || "runtime permit baseline topology is unsupported".to_string(),
                        )?)
                    }
                    "absent" => {
                        if schema < 5 {
                            return Err(
                                "runtime permit schema 4 absent baseline lacks deterministic namespace authority"
                                    .to_string(),
                            );
                        }
                        if !topology.is_empty() {
                            return Err(
                                "absent runtime permit carries a managed baseline topology"
                                    .to_string(),
                            );
                        }
                        RuntimeBaseline::Absent(AbsentRuntimeBaseline {
                            planned_sqm_section,
                            target_interface: absent_target_interface,
                            target_ifindex: parse_u32("absent_target_ifindex", &target_ifindex)?,
                            route_fingerprint: absent_route_fingerprint,
                            config_fingerprint,
                            sqm_fingerprint: absent_sqm_fingerprint,
                            kernel_topology_fingerprint,
                            kernel_namespace_seed,
                        })
                    }
                    _ => return Err("runtime permit baseline state is unsupported".to_string()),
                }
            } else {
                RuntimeBaseline::Managed(
                    MeasurementTopology::parse(&reader.field("baseline_topology")?).ok_or_else(
                        || "runtime permit baseline topology is unsupported".to_string(),
                    )?,
                )
            },
            initial_download_kbps: parse_u64(
                "initial_download_kbps",
                &reader.field("initial_download_kbps")?,
            )?,
            initial_upload_kbps: parse_u64(
                "initial_upload_kbps",
                &reader.field("initial_upload_kbps")?,
            )?,
            download_qdisc_kind: super::autotune_runtime::RuntimeQdiscKind::parse(
                &reader.field("download_qdisc_kind")?,
            )
            .ok_or_else(|| "runtime permit download qdisc kind is unsupported".to_string())?,
            upload_qdisc_kind: super::autotune_runtime::RuntimeQdiscKind::parse(
                &reader.field("upload_qdisc_kind")?,
            )
            .ok_or_else(|| "runtime permit upload qdisc kind is unsupported".to_string())?,
            allow_bypass_download: parse_bool(
                "allow_bypass_download",
                &reader.field("allow_bypass_download")?,
            )?,
            allow_bypass_upload: parse_bool(
                "allow_bypass_upload",
                &reader.field("allow_bypass_upload")?,
            )?,
            download_bounds: RuntimeRateBounds {
                minimum_kbps: parse_u64(
                    "download_minimum_kbps",
                    &reader.field("download_minimum_kbps")?,
                )?,
                maximum_kbps: parse_u64(
                    "download_maximum_kbps",
                    &reader.field("download_maximum_kbps")?,
                )?,
            },
            upload_bounds: RuntimeRateBounds {
                minimum_kbps: parse_u64(
                    "upload_minimum_kbps",
                    &reader.field("upload_minimum_kbps")?,
                )?,
                maximum_kbps: parse_u64(
                    "upload_maximum_kbps",
                    &reader.field("upload_maximum_kbps")?,
                )?,
            },
        };
        reader.finish()?;
        permit.validate()?;
        if permit.encode_for_schema(schema)? != input {
            return Err("runtime permit record is not canonical".to_string());
        }
        Ok(permit)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeOverrideCheckpoint {
    pub permit_id: String,
    pub job_id: String,
    pub worker_run_id: String,
    pub boot_id: String,
    pub coordinator_generation: String,
    pub instance_name: String,
    pub created_boot_ms: u64,
    pub baseline: RuntimeRestoreBaseline,
    pub profile: crate::autotune::AutotuneProfile,
    pub link_kind: crate::autotune::LinkKind,
    pub managed_sqm_was_active: bool,
    pub temporary_stage: TemporaryTopologyStage,
    pub temporary: TemporaryTopologyIdentity,
}

impl RuntimeOverrideCheckpoint {
    pub fn new(
        permit: &AutotuneRuntimePermit,
        created_boot_ms: u64,
        baseline: RuntimeRestoreBaseline,
    ) -> Result<Self, String> {
        let managed_sqm_was_active = matches!(&baseline, RuntimeBaseline::Managed(_));
        let checkpoint = Self {
            permit_id: permit.permit_id.clone(),
            job_id: permit.job_id.clone(),
            worker_run_id: permit.worker_run_id.clone(),
            boot_id: permit.boot_id.clone(),
            coordinator_generation: permit.coordinator_generation.clone(),
            instance_name: permit.instance_name.clone(),
            created_boot_ms,
            baseline,
            profile: permit.profile,
            link_kind: permit.link_kind,
            managed_sqm_was_active,
            temporary_stage: TemporaryTopologyStage::Planned,
            temporary: TemporaryTopologyIdentity::planned_for_permit(&permit.permit_id)?,
        };
        checkpoint.validate_against(permit)?;
        Ok(checkpoint)
    }

    pub fn managed_baseline(&self) -> Result<&RuntimeSnapshot, String> {
        self.baseline.managed_snapshot()
    }

    pub fn validate_against(&self, permit: &AutotuneRuntimePermit) -> Result<(), String> {
        self.validate()?;
        permit.validate()?;
        if self.created_boot_ms >= permit.deadline_boot_ms
            || self.permit_id != permit.permit_id
            || self.job_id != permit.job_id
            || self.worker_run_id != permit.worker_run_id
            || self.boot_id != permit.boot_id
            || self.coordinator_generation != permit.coordinator_generation
            || self.instance_name != permit.instance_name
            || self.profile != permit.profile
            || self.link_kind != permit.link_kind
        {
            return Err("runtime checkpoint does not match its permit".to_string());
        }
        self.baseline.validate_for_permit(permit)?;
        self.temporary.validate_for_permit(&permit.permit_id)?;
        Ok(())
    }

    pub fn advance_temporary_stage(
        &self,
        next: TemporaryTopologyStage,
        observed_ifindex: Option<u32>,
    ) -> Result<Self, String> {
        let allowed = match &self.baseline {
            RuntimeBaseline::Managed(_) => matches!(
                (self.temporary_stage, next),
                (
                    TemporaryTopologyStage::Planned,
                    TemporaryTopologyStage::ManagedSqmSuspended
                ) | (
                    TemporaryTopologyStage::ManagedSqmSuspended,
                    TemporaryTopologyStage::LinkOwned
                ) | (
                    TemporaryTopologyStage::LinkOwned,
                    TemporaryTopologyStage::Active
                ) | (
                    TemporaryTopologyStage::Planned
                        | TemporaryTopologyStage::ManagedSqmSuspended
                        | TemporaryTopologyStage::LinkOwned
                        | TemporaryTopologyStage::Active,
                    TemporaryTopologyStage::TemporaryAbsent
                ) | (
                    TemporaryTopologyStage::TemporaryAbsent,
                    TemporaryTopologyStage::BaselineRestored
                ) | (
                    TemporaryTopologyStage::BaselineRestored,
                    TemporaryTopologyStage::Planned
                )
            ),
            RuntimeBaseline::Absent(_) => matches!(
                (self.temporary_stage, next),
                (
                    TemporaryTopologyStage::Planned,
                    TemporaryTopologyStage::AbsenceAttested
                ) | (
                    TemporaryTopologyStage::AbsenceAttested,
                    TemporaryTopologyStage::LinkOwned
                ) | (
                    TemporaryTopologyStage::LinkOwned,
                    TemporaryTopologyStage::Active
                ) | (
                    TemporaryTopologyStage::Planned
                        | TemporaryTopologyStage::AbsenceAttested
                        | TemporaryTopologyStage::LinkOwned
                        | TemporaryTopologyStage::Active,
                    TemporaryTopologyStage::TemporaryAbsent
                ) | (
                    TemporaryTopologyStage::TemporaryAbsent,
                    TemporaryTopologyStage::BaselineRestored
                ) | (
                    TemporaryTopologyStage::BaselineRestored,
                    TemporaryTopologyStage::Planned
                )
            ),
        };
        if !allowed {
            return Err(format!(
                "runtime checkpoint temporary stage cannot advance from {} to {}",
                self.temporary_stage.as_str(),
                next.as_str()
            ));
        }
        let mut advanced = self.clone();
        advanced.temporary_stage = next;
        match next {
            TemporaryTopologyStage::LinkOwned => {
                let ifindex = observed_ifindex
                    .filter(|value| *value > 0)
                    .ok_or_else(|| "temporary IFB ownership requires an ifindex".to_string())?;
                if self.temporary.ifb_ifindex.is_some() {
                    return Err("temporary IFB ifindex was already published".to_string());
                }
                advanced.temporary.ifb_ifindex = Some(ifindex);
            }
            TemporaryTopologyStage::Planned
                if self.temporary_stage == TemporaryTopologyStage::BaselineRestored =>
            {
                advanced.temporary.ifb_ifindex = None;
            }
            _ if observed_ifindex.is_some() => {
                return Err("temporary IFB ifindex is only accepted at link ownership".to_string())
            }
            _ => {}
        }
        advanced.validate()?;
        Ok(advanced)
    }

    fn validate(&self) -> Result<(), String> {
        require_lower_hex("runtime checkpoint permit id", &self.permit_id, 32)?;
        require_lower_hex("runtime checkpoint job id", &self.job_id, 32)?;
        require_lower_hex("runtime checkpoint worker run id", &self.worker_run_id, 32)?;
        require_lower_hex("runtime checkpoint boot id", &self.boot_id, 32)?;
        require_lower_hex(
            "runtime checkpoint coordinator generation",
            &self.coordinator_generation,
            32,
        )?;
        require_identifier("runtime checkpoint instance", &self.instance_name)?;
        if self.created_boot_ms == 0 {
            return Err("runtime checkpoint timestamp is missing".to_string());
        }
        self.baseline.validate_restore_baseline()?;
        self.temporary.validate()?;
        match &self.baseline {
            RuntimeBaseline::Managed(_) if !self.managed_sqm_was_active => {
                return Err(
                    "managed runtime checkpoint requires an active managed SQM baseline"
                        .to_string(),
                )
            }
            RuntimeBaseline::Absent(_) if self.managed_sqm_was_active => {
                return Err(
                    "absent runtime checkpoint cannot claim an active managed SQM baseline"
                        .to_string(),
                )
            }
            RuntimeBaseline::Managed(_)
                if self.temporary_stage == TemporaryTopologyStage::AbsenceAttested =>
            {
                return Err(
                    "managed runtime checkpoint cannot claim an absence attestation".to_string(),
                )
            }
            RuntimeBaseline::Absent(_)
                if self.temporary_stage == TemporaryTopologyStage::ManagedSqmSuspended =>
            {
                return Err(
                    "absent runtime checkpoint cannot claim managed SQM suspension".to_string(),
                )
            }
            RuntimeBaseline::Absent(absent) if absent.kernel_namespace_seed != self.permit_id => {
                return Err(
                    "absent runtime checkpoint namespace does not match its permit ID".to_string(),
                )
            }
            _ => {}
        }
        match self.temporary_stage {
            TemporaryTopologyStage::LinkOwned | TemporaryTopologyStage::Active
                if self.temporary.ifb_ifindex.is_none() =>
            {
                return Err(
                    "runtime checkpoint cannot own a temporary topology without an IFB ifindex"
                        .to_string(),
                )
            }
            TemporaryTopologyStage::Planned
            | TemporaryTopologyStage::ManagedSqmSuspended
            | TemporaryTopologyStage::AbsenceAttested
                if self.temporary.ifb_ifindex.is_some() =>
            {
                return Err(
                    "runtime checkpoint publishes an IFB ifindex before link ownership".to_string(),
                )
            }
            _ => {}
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<String, String> {
        self.encode_for_schema(match &self.baseline {
            RuntimeBaseline::Managed(_) => 2,
            RuntimeBaseline::Absent(_) => 5,
        })
    }

    fn encode_for_schema(&self, schema: u8) -> Result<String, String> {
        self.validate()?;
        let header = match (schema, &self.baseline) {
            (2, RuntimeBaseline::Managed(_)) => CHECKPOINT_HEADER,
            (5, RuntimeBaseline::Absent(_)) => EXECUTABLE_ABSENT_CHECKPOINT_HEADER,
            (2, RuntimeBaseline::Absent(_)) => {
                return Err(
                    "legacy runtime checkpoint schema cannot represent an absent baseline"
                        .to_string(),
                )
            }
            (3, _) => {
                return Err(
                    "runtime checkpoint schema 3 predates deterministic namespace authority"
                        .to_string(),
                )
            }
            (4, _) => {
                return Err(
                    "runtime checkpoint schema 4 predates executable absence stages".to_string(),
                )
            }
            (5, RuntimeBaseline::Managed(_)) => {
                return Err(
                    "runtime checkpoint schema 5 is reserved for absent baselines".to_string(),
                )
            }
            _ => return Err("unsupported runtime checkpoint schema".to_string()),
        };
        let mut fields = vec![
            ("permit_id", self.permit_id.clone()),
            ("job_id", self.job_id.clone()),
            ("worker_run_id", self.worker_run_id.clone()),
            ("boot_id", self.boot_id.clone()),
            (
                "coordinator_generation",
                self.coordinator_generation.clone(),
            ),
            ("instance_name", self.instance_name.clone()),
            ("created_boot_ms", self.created_boot_ms.to_string()),
        ];
        match &self.baseline {
            RuntimeBaseline::Managed(baseline) => fields.extend([
                (
                    "baseline_target_interface",
                    baseline.target_interface.clone(),
                ),
                (
                    "baseline_route_fingerprint",
                    baseline.route_fingerprint.clone(),
                ),
                ("baseline_sqm_fingerprint", baseline.sqm_fingerprint.clone()),
                ("baseline_topology", baseline.topology.as_str().to_string()),
                (
                    "baseline_download_kbps",
                    optional_number(baseline.download_kbps),
                ),
                (
                    "baseline_upload_kbps",
                    optional_number(baseline.upload_kbps),
                ),
                (
                    "baseline_download_qdisc_kind",
                    optional_qdisc_kind(baseline.download_qdisc_kind),
                ),
                (
                    "baseline_upload_qdisc_kind",
                    optional_qdisc_kind(baseline.upload_qdisc_kind),
                ),
            ]),
            RuntimeBaseline::Absent(absent) => fields.extend([
                ("baseline_state", "absent".to_string()),
                ("baseline_target_interface", String::new()),
                ("baseline_route_fingerprint", String::new()),
                ("baseline_sqm_fingerprint", String::new()),
                ("baseline_topology", String::new()),
                ("baseline_download_kbps", String::new()),
                ("baseline_upload_kbps", String::new()),
                ("baseline_download_qdisc_kind", String::new()),
                ("baseline_upload_qdisc_kind", String::new()),
                (
                    "absent_planned_sqm_section",
                    absent.planned_sqm_section.clone(),
                ),
                ("absent_target_interface", absent.target_interface.clone()),
                ("absent_target_ifindex", absent.target_ifindex.to_string()),
                ("absent_route_fingerprint", absent.route_fingerprint.clone()),
                (
                    "absent_config_fingerprint",
                    absent.config_fingerprint.clone(),
                ),
                ("absent_sqm_fingerprint", absent.sqm_fingerprint.clone()),
                (
                    "absent_kernel_topology_fingerprint",
                    absent.kernel_topology_fingerprint.clone(),
                ),
                (
                    "absent_kernel_namespace_seed",
                    absent.kernel_namespace_seed.clone(),
                ),
            ]),
        }
        fields.extend([
            ("profile", self.profile.as_str().to_string()),
            ("link_kind", self.link_kind.as_str().to_string()),
            (
                "managed_sqm_was_active",
                bool_text(self.managed_sqm_was_active).to_string(),
            ),
            ("temporary_stage", self.temporary_stage.as_str().to_string()),
            ("temporary_ifb_name", self.temporary.ifb_name.clone()),
            ("temporary_ifb_alias", self.temporary.ifb_alias.clone()),
            (
                "temporary_ifb_ifindex",
                self.temporary
                    .ifb_ifindex
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            (
                "temporary_target_qdisc_handle",
                self.temporary.target_qdisc_handle.clone(),
            ),
            (
                "temporary_ifb_qdisc_handle",
                self.temporary.ifb_qdisc_handle.clone(),
            ),
            (
                "temporary_ingress_qdisc_handle",
                self.temporary.ingress_qdisc_handle.clone(),
            ),
            (
                "temporary_redirect_preference",
                self.temporary.redirect_preference.to_string(),
            ),
        ]);
        if schema >= 4 {
            fields.extend([
                (
                    "temporary_redirect_filter_handle",
                    self.temporary.redirect_filter_handle.to_string(),
                ),
                (
                    "temporary_redirect_action_index",
                    self.temporary.redirect_action_index.to_string(),
                ),
                (
                    "temporary_redirect_action_cookie",
                    self.temporary.redirect_action_cookie.clone(),
                ),
            ]);
        }
        encode_record(header, &fields)
    }

    pub fn decode(input: &str) -> Result<Self, String> {
        let (schema, header) = match input.lines().next() {
            Some(CHECKPOINT_HEADER) => (2, CHECKPOINT_HEADER),
            Some(ABSENT_CHECKPOINT_HEADER) => {
                return Err(
                    "runtime checkpoint schema 3 predates deterministic namespace authority"
                        .to_string(),
                )
            }
            Some(NAMESPACE_CHECKPOINT_HEADER) => {
                return Err(
                    "runtime checkpoint schema 4 predates executable absence stages".to_string(),
                )
            }
            Some(EXECUTABLE_ABSENT_CHECKPOINT_HEADER) => (5, EXECUTABLE_ABSENT_CHECKPOINT_HEADER),
            _ => return Err("runtime record header is invalid".to_string()),
        };
        let mut reader = RecordReader::new(input, header)?;
        let permit_id = reader.field("permit_id")?;
        let job_id = reader.field("job_id")?;
        let worker_run_id = reader.field("worker_run_id")?;
        let boot_id = reader.field("boot_id")?;
        let coordinator_generation = reader.field("coordinator_generation")?;
        let instance_name = reader.field("instance_name")?;
        let created_boot_ms = parse_u64("created_boot_ms", &reader.field("created_boot_ms")?)?;
        let baseline = if schema == 2 {
            let target_interface = reader.field("baseline_target_interface")?;
            let route_fingerprint = reader.field("baseline_route_fingerprint")?;
            let sqm_fingerprint = reader.field("baseline_sqm_fingerprint")?;
            let topology = MeasurementTopology::parse(&reader.field("baseline_topology")?)
                .ok_or_else(|| "runtime checkpoint topology is unsupported".to_string())?;
            let download_kbps = optional_u64(
                "baseline_download_kbps",
                &reader.field("baseline_download_kbps")?,
            )?;
            let upload_kbps = optional_u64(
                "baseline_upload_kbps",
                &reader.field("baseline_upload_kbps")?,
            )?;
            let download_qdisc_kind = optional_qdisc_kind_value(
                "baseline_download_qdisc_kind",
                &reader.field("baseline_download_qdisc_kind")?,
            )?;
            let upload_qdisc_kind = optional_qdisc_kind_value(
                "baseline_upload_qdisc_kind",
                &reader.field("baseline_upload_qdisc_kind")?,
            )?;
            RuntimeBaseline::Managed(RuntimeSnapshot {
                target_interface,
                route_fingerprint,
                sqm_fingerprint,
                topology,
                download_kbps,
                upload_kbps,
                download_qdisc_kind,
                upload_qdisc_kind,
            })
        } else {
            if reader.field("baseline_state")? != "absent" {
                return Err("runtime checkpoint schema 3 requires an absent baseline".to_string());
            }
            for field in [
                "baseline_target_interface",
                "baseline_route_fingerprint",
                "baseline_sqm_fingerprint",
                "baseline_topology",
                "baseline_download_kbps",
                "baseline_upload_kbps",
                "baseline_download_qdisc_kind",
                "baseline_upload_qdisc_kind",
            ] {
                if !reader.field(field)?.is_empty() {
                    return Err(
                        "absent runtime checkpoint contains managed baseline data".to_string()
                    );
                }
            }
            RuntimeBaseline::Absent(AbsentRuntimeBaseline {
                planned_sqm_section: reader.field("absent_planned_sqm_section")?,
                target_interface: reader.field("absent_target_interface")?,
                target_ifindex: parse_u32(
                    "absent_target_ifindex",
                    &reader.field("absent_target_ifindex")?,
                )?,
                route_fingerprint: reader.field("absent_route_fingerprint")?,
                config_fingerprint: reader.field("absent_config_fingerprint")?,
                sqm_fingerprint: reader.field("absent_sqm_fingerprint")?,
                kernel_topology_fingerprint: reader.field("absent_kernel_topology_fingerprint")?,
                kernel_namespace_seed: reader.field("absent_kernel_namespace_seed")?,
            })
        };
        let profile = crate::autotune::AutotuneProfile::parse(&reader.field("profile")?)
            .ok_or_else(|| "runtime checkpoint profile is unsupported".to_string())?;
        let link_kind = crate::autotune::LinkKind::parse(&reader.field("link_kind")?)
            .ok_or_else(|| "runtime checkpoint link kind is unsupported".to_string())?;
        let managed_sqm_was_active = parse_bool(
            "managed_sqm_was_active",
            &reader.field("managed_sqm_was_active")?,
        )?;
        if schema == 2 && !managed_sqm_was_active {
            return Err("legacy runtime checkpoint requires managed_sqm_was_active=1".to_string());
        }
        let temporary_stage = TemporaryTopologyStage::parse(&reader.field("temporary_stage")?)
            .ok_or_else(|| "runtime checkpoint temporary stage is unsupported".to_string())?;
        let mut temporary = TemporaryTopologyIdentity::planned_for_permit(&permit_id)?;
        temporary.ifb_name = reader.field("temporary_ifb_name")?;
        temporary.ifb_alias = reader.field("temporary_ifb_alias")?;
        temporary.ifb_ifindex = optional_u32(
            "temporary_ifb_ifindex",
            &reader.field("temporary_ifb_ifindex")?,
        )?;
        temporary.target_qdisc_handle = reader.field("temporary_target_qdisc_handle")?;
        temporary.ifb_qdisc_handle = reader.field("temporary_ifb_qdisc_handle")?;
        temporary.ingress_qdisc_handle = reader.field("temporary_ingress_qdisc_handle")?;
        temporary.redirect_preference = parse_u16(
            "temporary_redirect_preference",
            &reader.field("temporary_redirect_preference")?,
        )?;
        if schema >= 4 {
            temporary.redirect_filter_handle = parse_u32(
                "temporary_redirect_filter_handle",
                &reader.field("temporary_redirect_filter_handle")?,
            )?;
            temporary.redirect_action_index = parse_u32(
                "temporary_redirect_action_index",
                &reader.field("temporary_redirect_action_index")?,
            )?;
            temporary.redirect_action_cookie = reader.field("temporary_redirect_action_cookie")?;
        }
        reader.finish()?;
        let checkpoint = Self {
            permit_id,
            job_id,
            worker_run_id,
            boot_id,
            coordinator_generation,
            instance_name,
            created_boot_ms,
            baseline,
            profile,
            link_kind,
            managed_sqm_was_active,
            temporary_stage,
            temporary,
        };
        checkpoint.validate()?;
        if checkpoint.encode_for_schema(schema)? != input {
            return Err("runtime checkpoint record is not canonical".to_string());
        }
        Ok(checkpoint)
    }
}

#[derive(Clone, Debug)]
pub struct RuntimeOverrideStore {
    root: PathBuf,
}

impl RuntimeOverrideStore {
    pub fn open(instance_run_dir: &Path) -> Result<Self, String> {
        ensure_existing_directory(instance_run_dir, false)?;
        let root = instance_run_dir.join(RUNTIME_DIR);
        ensure_private_directory(&root)?;
        clean_and_validate_entries(&root)?;
        Ok(Self { root })
    }

    /// Return the private per-instance directory which owns this store.
    /// Callers use it only for exact job-owned companion records after the
    /// runtime ownership records have been proven absent.
    pub fn instance_run_dir(&self) -> &Path {
        self.root
            .parent()
            .expect("runtime override store always has an instance parent")
    }

    pub fn publish_permit(&self, permit: &AutotuneRuntimePermit) -> Result<(), String> {
        atomic_write(&self.root, PERMIT_FILE, &permit.encode()?)
    }

    pub fn publish_control(&self, control: &AutotuneRuntimeControl) -> Result<(), String> {
        atomic_write(&self.root, CONTROL_FILE, &control.encode()?)
    }

    pub fn publish_restore_intent(&self, control: &AutotuneRuntimeControl) -> Result<(), String> {
        atomic_write(&self.root, RESTORE_FILE, &control.encode()?)
    }

    pub fn publish_ack(&self, ack: &AutotuneRuntimeAck) -> Result<(), String> {
        atomic_write(&self.root, ACK_FILE, &ack.encode()?)
    }

    pub fn publish_checkpoint(&self, checkpoint: &RuntimeOverrideCheckpoint) -> Result<(), String> {
        atomic_write(&self.root, CHECKPOINT_FILE, &checkpoint.encode()?)
    }

    pub fn replace_checkpoint(
        &self,
        current: &RuntimeOverrideCheckpoint,
        next: &RuntimeOverrideCheckpoint,
    ) -> Result<(), String> {
        if self.read_checkpoint()?.as_ref() != Some(current) {
            return Err("runtime checkpoint changed before its stage transition".to_string());
        }
        if current.permit_id != next.permit_id
            || current.job_id != next.job_id
            || current.worker_run_id != next.worker_run_id
            || current.boot_id != next.boot_id
            || current.coordinator_generation != next.coordinator_generation
            || current.instance_name != next.instance_name
            || current.created_boot_ms != next.created_boot_ms
            || current.baseline != next.baseline
            || current.profile != next.profile
            || current.link_kind != next.link_kind
            || current.managed_sqm_was_active != next.managed_sqm_was_active
            || current.temporary.ifb_name != next.temporary.ifb_name
            || current.temporary.ifb_alias != next.temporary.ifb_alias
            || current.temporary.target_qdisc_handle != next.temporary.target_qdisc_handle
            || current.temporary.ifb_qdisc_handle != next.temporary.ifb_qdisc_handle
            || current.temporary.ingress_qdisc_handle != next.temporary.ingress_qdisc_handle
            || current.temporary.redirect_preference != next.temporary.redirect_preference
        {
            return Err(
                "runtime checkpoint immutable identity changed during transition".to_string(),
            );
        }
        next.validate()?;
        self.publish_checkpoint(next)
    }

    pub fn read_permit(&self) -> Result<Option<AutotuneRuntimePermit>, String> {
        read_optional(&self.root.join(PERMIT_FILE), AutotuneRuntimePermit::decode)
    }

    pub fn read_control(&self) -> Result<Option<AutotuneRuntimeControl>, String> {
        read_optional(
            &self.root.join(CONTROL_FILE),
            AutotuneRuntimeControl::decode,
        )
    }

    pub fn read_ack(&self) -> Result<Option<AutotuneRuntimeAck>, String> {
        read_optional(&self.root.join(ACK_FILE), AutotuneRuntimeAck::decode)
    }

    pub fn read_restore_intent(&self) -> Result<Option<AutotuneRuntimeControl>, String> {
        read_optional(
            &self.root.join(RESTORE_FILE),
            AutotuneRuntimeControl::decode,
        )
    }

    pub fn read_checkpoint(&self) -> Result<Option<RuntimeOverrideCheckpoint>, String> {
        read_optional(
            &self.root.join(CHECKPOINT_FILE),
            RuntimeOverrideCheckpoint::decode,
        )
    }

    /// Ask the instance-owned runtime driver to restore its checkpoint while
    /// keeping the exact permit visible.  Keeping the permit prevents the
    /// driver from immediately deleting the RESTORED acknowledgement before
    /// the worker has attested it.
    pub fn request_restore(&self, expected: &AutotuneRuntimeControl) -> Result<(), String> {
        let current = self
            .read_control()?
            .ok_or_else(|| "runtime control disappeared before restore intent".to_string())?;
        if current != *expected {
            return Err("runtime control changed before restore intent".to_string());
        }
        if self.read_restore_intent()?.is_some() {
            return Err("runtime restore intent already exists".to_string());
        }
        self.publish_restore_intent(expected)?;
        remove_records(&self.root, &[CONTROL_FILE])
    }

    /// Release the coordinator-owned permit only after the worker has
    /// attested the exact RESTORED acknowledgement.  The instance daemon then
    /// releases its tracker and removes checkpoint plus ACK atomically from
    /// the protocol's point of view.
    pub fn release_restored_request(
        &self,
        expected_permit: &AutotuneRuntimePermit,
        expected_control: &AutotuneRuntimeControl,
        current_boot_ms: u64,
    ) -> Result<(), String> {
        self.attest_restored_owner(expected_permit, expected_control, current_boot_ms, false)?;
        remove_records(&self.root, &[PERMIT_FILE])
    }

    /// Crash-recovery counterpart of `release_restored_request`.  A dead
    /// worker may leave either the live control or the durable restore intent
    /// (and, in the write/remove crash window, both).  Remove coordinator-owned
    /// records only after re-reading and fully binding the permit, control,
    /// checkpoint and RESTORED ACK inside this store operation.
    pub fn withdraw_restored_request(
        &self,
        expected_permit: &AutotuneRuntimePermit,
        expected_control: &AutotuneRuntimeControl,
        current_boot_ms: u64,
    ) -> Result<(), String> {
        self.attest_restored_owner(expected_permit, expected_control, current_boot_ms, true)?;
        remove_records(&self.root, &[CONTROL_FILE, PERMIT_FILE])
    }

    fn attest_restored_owner(
        &self,
        expected_permit: &AutotuneRuntimePermit,
        expected_control: &AutotuneRuntimeControl,
        current_boot_ms: u64,
        allow_live_control: bool,
    ) -> Result<(), String> {
        let current_permit = self
            .read_permit()?
            .ok_or_else(|| "runtime permit disappeared before restored release".to_string())?;
        if current_permit != *expected_permit {
            return Err("runtime permit changed before restored release".to_string());
        }
        expected_permit
            .attests_recovery_control(&expected_permit.instance_name, expected_control)?;

        let current_control = self.read_control()?;
        let restore_intent = self.read_restore_intent()?;
        let control_matches = current_control.as_ref() == Some(expected_control);
        let intent_matches = restore_intent.as_ref() == Some(expected_control);
        if current_control
            .as_ref()
            .is_some_and(|value| value != expected_control)
            || restore_intent
                .as_ref()
                .is_some_and(|value| value != expected_control)
            || (!allow_live_control && (current_control.is_some() || !intent_matches))
            || (allow_live_control && !control_matches && !intent_matches)
        {
            return Err("runtime control identity changed before restored release".to_string());
        }

        let checkpoint = self
            .read_checkpoint()?
            .ok_or_else(|| "runtime checkpoint disappeared before restored release".to_string())?;
        checkpoint.validate_against(expected_permit)?;
        let ack = self
            .read_ack()?
            .ok_or_else(|| "RESTORED runtime ACK disappeared before release".to_string())?;
        ack.attests_restored(expected_control, current_boot_ms)?;
        Ok(())
    }

    /// Withdraw only the coordinator-owned request records after it has
    /// observed a terminal RESTORED acknowledgement.  The instance-owned ACK
    /// and checkpoint deliberately remain visible until the instance daemon
    /// releases its tracker ownership and finalizes the restored session.
    pub fn withdraw_request(&self) -> Result<(), String> {
        remove_records(&self.root, &[CONTROL_FILE, PERMIT_FILE])
    }

    /// Remove the instance-owned completion records after the tracker has
    /// released the exact restored owner.  Removing the checkpoint first is
    /// conservative across a crash: a leftover RESTORED ACK is harmless and
    /// identity-bound, whereas a checkpoint without its owner must trigger
    /// fail-closed recovery on restart.
    pub fn finalize_restored(&self) -> Result<(), String> {
        remove_records(&self.root, &[CHECKPOINT_FILE, ACK_FILE, RESTORE_FILE])
    }

    pub fn clear(&self) -> Result<(), String> {
        remove_records(
            &self.root,
            &[
                CONTROL_FILE,
                PERMIT_FILE,
                CHECKPOINT_FILE,
                ACK_FILE,
                RESTORE_FILE,
            ],
        )
    }
}

fn remove_records(root: &Path, names: &[&str]) -> Result<(), String> {
    for name in names {
        match fs::remove_file(root.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("unable to remove runtime {name}: {error}")),
        }
    }
    sync_directory(root)
}

fn encode_record(header: &str, fields: &[(&str, String)]) -> Result<String, String> {
    let mut output = String::from(header);
    output.push('\n');
    for (name, value) in fields {
        if name.is_empty() || value.contains(['\n', '\r', '\0']) {
            return Err("runtime record contains an invalid field".to_string());
        }
        output.push_str(name);
        output.push('=');
        output.push_str(value);
        output.push('\n');
    }
    if output.len() > MAX_RUNTIME_RECORD_BYTES {
        return Err("runtime record exceeds its bound".to_string());
    }
    Ok(output)
}

struct RecordReader<'a> {
    lines: std::str::Lines<'a>,
}

impl<'a> RecordReader<'a> {
    fn new(input: &'a str, header: &str) -> Result<Self, String> {
        if input.len() > MAX_RUNTIME_RECORD_BYTES || !input.ends_with('\n') {
            return Err("runtime record is unbounded or unterminated".to_string());
        }
        let mut lines = input.lines();
        if lines.next() != Some(header) {
            return Err("runtime record header is invalid".to_string());
        }
        Ok(Self { lines })
    }

    fn field(&mut self, expected: &str) -> Result<String, String> {
        let line = self
            .lines
            .next()
            .ok_or_else(|| format!("runtime record field {expected} is missing"))?;
        let (name, value) = line
            .split_once('=')
            .ok_or_else(|| format!("runtime record field {expected} is malformed"))?;
        if name != expected {
            return Err(format!("runtime record expected {expected}, found {name}"));
        }
        Ok(value.to_string())
    }

    fn finish(&mut self) -> Result<(), String> {
        if self.lines.next().is_some() {
            return Err("runtime record contains extraneous fields".to_string());
        }
        Ok(())
    }
}

fn read_optional<T>(
    path: &Path,
    decoder: impl FnOnce(&str) -> Result<T, String>,
) -> Result<Option<T>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("unable to inspect {}: {error}", path.display())),
    };
    validate_private_file(path, &metadata)?;
    let file =
        File::open(path).map_err(|error| format!("unable to open {}: {error}", path.display()))?;
    let mut bounded = file.take((MAX_RUNTIME_RECORD_BYTES + 1) as u64);
    let mut input = String::new();
    bounded
        .read_to_string(&mut input)
        .map_err(|error| format!("unable to read {}: {error}", path.display()))?;
    if input.len() > MAX_RUNTIME_RECORD_BYTES {
        return Err(format!("{} exceeds its size bound", path.display()));
    }
    decoder(&input).map(Some)
}

fn atomic_write(root: &Path, name: &str, contents: &str) -> Result<(), String> {
    if contents.len() > MAX_RUNTIME_RECORD_BYTES {
        return Err("runtime record exceeds its size bound".to_string());
    }
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let pid = unsafe { getpid() };
    let temp_name = format!(".tmp-{pid}-{sequence}");
    let temp_path = root.join(&temp_name);
    let final_path = root.join(name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp_path)
        .map_err(|error| format!("unable to create {}: {error}", temp_path.display()))?;
    if let Err(error) = file
        .write_all(contents.as_bytes())
        .and_then(|_| file.sync_all())
    {
        let _ = fs::remove_file(&temp_path);
        return Err(format!("unable to publish runtime record: {error}"));
    }
    drop(file);
    if let Err(error) = fs::rename(&temp_path, &final_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(format!("unable to rename runtime record: {error}"));
    }
    sync_directory(root)
}

fn ensure_existing_directory(path: &Path, private: bool) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("unable to inspect {}: {error}", path.display()))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(format!("{} is not a safe directory", path.display()));
    }
    if metadata.uid() != effective_uid() {
        return Err(format!("{} has a foreign owner", path.display()));
    }
    if private && metadata.permissions().mode() & 0o777 != 0o700 {
        return Err(format!("{} is not mode 0700", path.display()));
    }
    Ok(())
}

fn ensure_private_directory(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(_) => ensure_existing_directory(path, true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            builder
                .create(path)
                .map_err(|error| format!("unable to create {}: {error}", path.display()))?;
            ensure_existing_directory(path, true)
        }
        Err(error) => Err(format!("unable to inspect {}: {error}", path.display())),
    }
}

fn clean_and_validate_entries(root: &Path) -> Result<(), String> {
    for entry in fs::read_dir(root)
        .map_err(|error| format!("unable to inspect {}: {error}", root.display()))?
    {
        let entry = entry.map_err(|error| format!("unable to inspect runtime entry: {error}"))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "runtime entry name is not UTF-8".to_string())?;
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| format!("unable to inspect runtime entry {name}: {error}"))?;
        if name.starts_with(".tmp-") {
            validate_private_file(&entry.path(), &metadata)?;
            fs::remove_file(entry.path())
                .map_err(|error| format!("unable to remove runtime temp {name}: {error}"))?;
            continue;
        }
        if ![
            PERMIT_FILE,
            CONTROL_FILE,
            RESTORE_FILE,
            ACK_FILE,
            CHECKPOINT_FILE,
        ]
        .contains(&name.as_str())
        {
            return Err(format!("runtime directory contains unknown entry {name}"));
        }
        validate_private_file(&entry.path(), &metadata)?;
    }
    Ok(())
}

fn validate_private_file(path: &Path, metadata: &fs::Metadata) -> Result<(), String> {
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != effective_uid()
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(format!("{} is not a private regular file", path.display()));
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("unable to sync {}: {error}", path.display()))
}

fn effective_uid() -> u32 {
    unsafe { geteuid() }
}

fn parse_u64(name: &str, value: &str) -> Result<u64, String> {
    if value.is_empty() || (value.len() > 1 && value.starts_with('0')) {
        return Err(format!("runtime record field {name} is not canonical"));
    }
    value
        .parse::<u64>()
        .map_err(|_| format!("runtime record field {name} is invalid"))
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

fn parse_u32(name: &str, value: &str) -> Result<u32, String> {
    let value = parse_u64(name, value)?;
    u32::try_from(value).map_err(|_| format!("runtime record field {name} is too large"))
}

fn parse_u16(name: &str, value: &str) -> Result<u16, String> {
    let value = parse_u64(name, value)?;
    u16::try_from(value).map_err(|_| format!("runtime record field {name} is too large"))
}

fn parse_bool(name: &str, value: &str) -> Result<bool, String> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err(format!("runtime record field {name} is not a boolean")),
    }
}

fn bool_text(value: bool) -> &'static str {
    if value {
        "1"
    } else {
        "0"
    }
}

fn optional_number(value: Option<u64>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
}

fn optional_qdisc_kind(value: Option<super::autotune_runtime::RuntimeQdiscKind>) -> String {
    value
        .map(|kind| kind.as_str().to_string())
        .unwrap_or_default()
}

fn optional_qdisc_kind_value(
    name: &str,
    value: &str,
) -> Result<Option<super::autotune_runtime::RuntimeQdiscKind>, String> {
    if value.is_empty() {
        return Ok(None);
    }
    super::autotune_runtime::RuntimeQdiscKind::parse(value)
        .map(Some)
        .ok_or_else(|| format!("runtime record field {name} is invalid"))
}

fn optional_u64(name: &str, value: &str) -> Result<Option<u64>, String> {
    if value.is_empty() {
        Ok(None)
    } else {
        parse_u64(name, value).map(Some)
    }
}

fn optional_u32(name: &str, value: &str) -> Result<Option<u32>, String> {
    if value.is_empty() {
        Ok(None)
    } else {
        parse_u32(name, value).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::super::full_autotune::MAX_AUTOTUNE_EVIDENCE_RECORDS;
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("runtime-store-{name}-{nonce}"));
        fs::create_dir(&root).unwrap();
        root
    }

    fn permit() -> AutotuneRuntimePermit {
        AutotuneRuntimePermit {
            kind: RuntimePermitKind::Autotune,
            permit_id: "77".repeat(16),
            job_id: "11".repeat(16),
            worker_run_id: "22".repeat(16),
            boot_id: "33".repeat(16),
            coordinator_generation: "44".repeat(16),
            worker: ProcessIdentity {
                pid: 100,
                process_group: 100,
                starttime_ticks: 500,
            },
            instance_name: "wan_sqm".to_string(),
            target_interface: "pppoe-wan".to_string(),
            route_identity: "main||pppoe-wan|192.0.2.1||254".to_string(),
            route_fingerprint: "55".repeat(32),
            sqm_fingerprint: "66".repeat(32),
            deadline_boot_ms: 60_000,
            maximum_sequence: MAX_AUTOTUNE_EVIDENCE_RECORDS as u32,
            profile: crate::autotune::AutotuneProfile::BestOverall,
            link_kind: crate::autotune::LinkKind::Ethernet,
            baseline: RuntimeBaseline::Managed(MeasurementTopology::ShapedBoth),
            initial_download_kbps: 100_000,
            initial_upload_kbps: 50_000,
            download_qdisc_kind: super::super::autotune_runtime::RuntimeQdiscKind::Cake,
            upload_qdisc_kind: super::super::autotune_runtime::RuntimeQdiscKind::Cake,
            allow_bypass_download: true,
            allow_bypass_upload: false,
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

    fn absent_permit() -> AutotuneRuntimePermit {
        let mut permit = permit();
        let kernel_namespace_seed = permit.permit_id.clone();
        permit.baseline = RuntimeBaseline::Absent(AbsentRuntimeBaseline {
            planned_sqm_section: "wan_sqm".to_string(),
            target_interface: "pppoe-wan".to_string(),
            target_ifindex: 7,
            route_fingerprint: "55".repeat(32),
            config_fingerprint: "88".repeat(32),
            sqm_fingerprint: "66".repeat(32),
            kernel_topology_fingerprint: "99".repeat(32),
            kernel_namespace_seed,
        });
        permit
    }

    fn frozen_managed_permit(schema: u8) -> String {
        let (header, permit_kind) = match schema {
            3 => (PERMIT_HEADER, "permit_kind=autotune\n"),
            _ => panic!("unsupported frozen permit schema"),
        };
        format!(
            "{header}\n\
permit_id=77777777777777777777777777777777\n\
{permit_kind}\
job_id=11111111111111111111111111111111\n\
worker_run_id=22222222222222222222222222222222\n\
boot_id=33333333333333333333333333333333\n\
coordinator_generation=44444444444444444444444444444444\n\
worker_pid=100\n\
worker_process_group=100\n\
worker_starttime_ticks=500\n\
instance_name=wan_sqm\n\
target_interface=pppoe-wan\n\
route_identity=main||pppoe-wan|192.0.2.1||254\n\
route_fingerprint=5555555555555555555555555555555555555555555555555555555555555555\n\
sqm_fingerprint=6666666666666666666666666666666666666666666666666666666666666666\n\
deadline_boot_ms=60000\n\
maximum_sequence=256\n\
profile=best_overall\n\
link_kind=ethernet\n\
baseline_topology=both_shaped\n\
initial_download_kbps=100000\n\
initial_upload_kbps=50000\n\
download_qdisc_kind=cake\n\
upload_qdisc_kind=cake\n\
allow_bypass_download=1\n\
allow_bypass_upload=0\n\
download_minimum_kbps=10000\n\
download_maximum_kbps=1000000\n\
upload_minimum_kbps=5000\n\
upload_maximum_kbps=500000\n"
        )
    }

    fn frozen_managed_checkpoint() -> String {
        format!(
            "{CHECKPOINT_HEADER}\n\
permit_id=77777777777777777777777777777777\n\
job_id=11111111111111111111111111111111\n\
worker_run_id=22222222222222222222222222222222\n\
boot_id=33333333333333333333333333333333\n\
coordinator_generation=44444444444444444444444444444444\n\
instance_name=wan_sqm\n\
created_boot_ms=1000\n\
baseline_target_interface=pppoe-wan\n\
baseline_route_fingerprint=5555555555555555555555555555555555555555555555555555555555555555\n\
baseline_sqm_fingerprint=6666666666666666666666666666666666666666666666666666666666666666\n\
baseline_topology=both_shaped\n\
baseline_download_kbps=90000\n\
baseline_upload_kbps=45000\n\
baseline_download_qdisc_kind=cake\n\
baseline_upload_qdisc_kind=cake\n\
profile=best_overall\n\
link_kind=ethernet\n\
managed_sqm_was_active=1\n\
temporary_stage=planned\n\
temporary_ifb_name=catf77777777\n\
temporary_ifb_alias=cake-autotune-77777777777777777777777777777777\n\
temporary_ifb_ifindex=\n\
temporary_target_qdisc_handle=a777:\n\
temporary_ifb_qdisc_handle=b777:\n\
temporary_ingress_qdisc_handle=ffff:\n\
temporary_redirect_preference=50039\n"
        )
    }

    fn control() -> AutotuneRuntimeControl {
        let permit = permit();
        AutotuneRuntimeControl {
            permit_id: permit.permit_id,
            job_id: permit.job_id,
            worker_run_id: permit.worker_run_id,
            boot_id: permit.boot_id,
            coordinator_generation: permit.coordinator_generation,
            worker: permit.worker,
            sequence: 1,
            deadline_boot_ms: 30_000,
            target_interface: permit.target_interface,
            route_fingerprint: permit.route_fingerprint,
            sqm_fingerprint: permit.sqm_fingerprint,
            topology: MeasurementTopology::ShapedBoth,
            download_kbps: Some(100_000),
            upload_kbps: Some(50_000),
        }
    }

    fn baseline() -> RuntimeSnapshot {
        RuntimeSnapshot {
            target_interface: "pppoe-wan".to_string(),
            route_fingerprint: "55".repeat(32),
            sqm_fingerprint: "66".repeat(32),
            topology: MeasurementTopology::ShapedBoth,
            download_kbps: Some(90_000),
            upload_kbps: Some(45_000),
            download_qdisc_kind: Some(super::super::autotune_runtime::RuntimeQdiscKind::Cake),
            upload_qdisc_kind: Some(super::super::autotune_runtime::RuntimeQdiscKind::Cake),
        }
    }

    fn restored_ack(control: &AutotuneRuntimeControl, updated_boot_ms: u64) -> AutotuneRuntimeAck {
        AutotuneRuntimeAck {
            permit_id: control.permit_id.clone(),
            job_id: control.job_id.clone(),
            worker_run_id: control.worker_run_id.clone(),
            sequence: control.sequence,
            updated_boot_ms,
            target_interface: control.target_interface.clone(),
            route_fingerprint: control.route_fingerprint.clone(),
            sqm_fingerprint: control.sqm_fingerprint.clone(),
            state: super::super::full_autotune::RuntimeAckState::Restored,
            topology: None,
            download_kbps: None,
            upload_kbps: None,
            diagnostic_code: None,
        }
    }

    #[test]
    fn permit_and_checkpoint_are_canonical_and_strict() {
        let permit = permit();
        let encoded = permit.encode().unwrap();
        assert_eq!(AutotuneRuntimePermit::decode(&encoded).unwrap(), permit);
        let reordered = encoded.replacen(
            "job_id=11111111111111111111111111111111\nworker_run_id=",
            "worker_run_id=",
            1,
        );
        assert!(AutotuneRuntimePermit::decode(&reordered).is_err());

        let checkpoint =
            RuntimeOverrideCheckpoint::new(&permit, 1_000, RuntimeBaseline::Managed(baseline()))
                .unwrap();
        let encoded = checkpoint.encode().unwrap();
        assert_eq!(
            RuntimeOverrideCheckpoint::decode(&encoded).unwrap(),
            checkpoint
        );
        let unknown = encoded.replace(
            "baseline_upload_kbps=45000\n",
            "baseline_upload_kbps=45000\nextra=1\n",
        );
        assert!(RuntimeOverrideCheckpoint::decode(&unknown).is_err());
        let invalid_identity = encoded.replacen(
            "permit_id=77777777777777777777777777777777",
            "permit_id=not-hex-------------------------",
            1,
        );
        assert!(RuntimeOverrideCheckpoint::decode(&invalid_identity).is_err());

        let mut link_owned = checkpoint.clone();
        link_owned.temporary_stage = TemporaryTopologyStage::LinkOwned;
        assert!(link_owned.encode().is_err());
        link_owned.temporary.ifb_ifindex = Some(42);
        let link_owned_encoded = link_owned.encode().unwrap();
        assert_eq!(
            RuntimeOverrideCheckpoint::decode(&link_owned_encoded).unwrap(),
            link_owned
        );

        let mut foreign = checkpoint.clone();
        foreign.temporary.ifb_alias = "cake-autotune-foreign".to_string();
        assert!(foreign.validate_against(&permit).is_err());
    }

    #[test]
    fn managed_v3_permit_bytes_decode_and_reencode_exactly() {
        let frozen = frozen_managed_permit(3);
        let decoded = AutotuneRuntimePermit::decode(&frozen).unwrap();
        assert_eq!(
            decoded.baseline,
            RuntimeBaseline::Managed(MeasurementTopology::ShapedBoth)
        );
        assert_eq!(decoded.encode_for_schema(3).unwrap(), frozen);
        assert_eq!(decoded.encode().unwrap(), frozen);
        assert_eq!(permit().encode().unwrap(), frozen_managed_permit(3));
        assert!(permit().encode_for_schema(2).is_err());
        let retired_v2 = frozen
            .replacen(PERMIT_HEADER, "cake-autorate-autotune-runtime-permit\t2", 1)
            .replace("permit_kind=autotune\n", "");
        assert!(AutotuneRuntimePermit::decode(&retired_v2).is_err());
    }

    #[test]
    fn namespace_permit_schema_is_canonical_and_absent_never_downgrades() {
        let managed = permit();
        let encoded_managed = managed.encode_for_schema(4).unwrap();
        assert!(encoded_managed.starts_with(VARIANT_PERMIT_HEADER));
        assert!(encoded_managed.contains("baseline_state=managed\n"));
        assert!(encoded_managed.contains("absent_target_interface=\n"));
        assert_eq!(
            AutotuneRuntimePermit::decode(&encoded_managed).unwrap(),
            managed
        );

        let absent = absent_permit();
        let encoded_absent = absent.encode().unwrap();
        assert!(encoded_absent.starts_with(NAMESPACE_PERMIT_HEADER));
        assert!(encoded_absent.contains("baseline_state=absent\n"));
        assert!(encoded_absent.contains("baseline_topology=\n"));
        assert!(encoded_absent.contains("absent_target_interface=pppoe-wan\n"));
        assert!(encoded_absent.contains(&format!(
            "absent_kernel_namespace_seed={}\n",
            absent.permit_id
        )));
        assert_eq!(
            AutotuneRuntimePermit::decode(&encoded_absent).unwrap(),
            absent
        );
        assert!(absent.encode_for_schema(3).is_err());
        assert!(absent.encode_for_schema(4).is_err());

        let old_absent = encoded_absent
            .replacen(NAMESPACE_PERMIT_HEADER, VARIANT_PERMIT_HEADER, 1)
            .replace(
                &format!("absent_kernel_namespace_seed={}\n", absent.permit_id),
                "",
            );
        assert!(AutotuneRuntimePermit::decode(&old_absent)
            .unwrap_err()
            .contains("lacks deterministic namespace authority"));

        let managed_with_absent_data = encoded_managed.replacen(
            "absent_target_interface=\n",
            "absent_target_interface=pppoe-wan\n",
            1,
        );
        assert!(AutotuneRuntimePermit::decode(&managed_with_absent_data).is_err());
        let absent_with_topology =
            encoded_absent.replacen("baseline_topology=\n", "baseline_topology=both_shaped\n", 1);
        assert!(AutotuneRuntimePermit::decode(&absent_with_topology).is_err());
    }

    #[test]
    fn frozen_v2_checkpoint_is_managed_and_byte_exact() {
        let permit = permit();
        let checkpoint =
            RuntimeOverrideCheckpoint::new(&permit, 1_000, RuntimeBaseline::Managed(baseline()))
                .unwrap();
        let frozen = frozen_managed_checkpoint();
        assert_eq!(checkpoint.encode().unwrap(), frozen);
        let decoded = RuntimeOverrideCheckpoint::decode(&frozen).unwrap();
        assert_eq!(decoded, checkpoint);
        assert!(matches!(decoded.baseline, RuntimeBaseline::Managed(_)));
        assert_eq!(decoded.encode_for_schema(2).unwrap(), frozen);

        let inactive = frozen.replacen(
            "managed_sqm_was_active=1\n",
            "managed_sqm_was_active=0\n",
            1,
        );
        assert!(RuntimeOverrideCheckpoint::decode(&inactive).is_err());
    }

    #[test]
    fn absent_checkpoint_is_canonical_v5_and_older_absent_schemas_are_rejected() {
        let absent_permit_value = absent_permit();
        let RuntimeBaseline::Absent(absent) = &absent_permit_value.baseline else {
            unreachable!()
        };
        let checkpoint = RuntimeOverrideCheckpoint::new(
            &absent_permit_value,
            1_000,
            RuntimeBaseline::Absent(absent.clone()),
        )
        .unwrap();
        assert!(!checkpoint.managed_sqm_was_active);
        let encoded = checkpoint.encode().unwrap();
        assert!(encoded.starts_with(EXECUTABLE_ABSENT_CHECKPOINT_HEADER));
        assert!(encoded.contains("baseline_state=absent\n"));
        assert!(encoded.contains("baseline_target_interface=\n"));
        assert!(encoded.contains("absent_target_interface=pppoe-wan\n"));
        assert!(encoded.contains(&format!(
            "absent_kernel_namespace_seed={}\n",
            checkpoint.permit_id
        )));
        assert!(encoded.contains("temporary_redirect_filter_handle="));
        assert!(encoded.contains("temporary_redirect_action_index="));
        assert!(encoded.contains(&format!(
            "temporary_redirect_action_cookie={}\n",
            checkpoint.permit_id
        )));
        assert_eq!(
            RuntimeOverrideCheckpoint::decode(&encoded).unwrap(),
            checkpoint
        );
        assert!(checkpoint.encode_for_schema(2).is_err());
        assert!(checkpoint.encode_for_schema(3).is_err());
        assert!(checkpoint.encode_for_schema(4).is_err());

        let dormant_v4 = encoded.replacen(
            EXECUTABLE_ABSENT_CHECKPOINT_HEADER,
            NAMESPACE_CHECKPOINT_HEADER,
            1,
        );
        assert!(RuntimeOverrideCheckpoint::decode(&dormant_v4)
            .unwrap_err()
            .contains("predates executable absence stages"));

        let old_absent = encoded
            .replacen(
                EXECUTABLE_ABSENT_CHECKPOINT_HEADER,
                ABSENT_CHECKPOINT_HEADER,
                1,
            )
            .replace(
                &format!("absent_kernel_namespace_seed={}\n", checkpoint.permit_id),
                "",
            )
            .replace(
                &format!(
                    "temporary_redirect_filter_handle={}\n",
                    checkpoint.temporary.redirect_filter_handle
                ),
                "",
            )
            .replace(
                &format!(
                    "temporary_redirect_action_index={}\n",
                    checkpoint.temporary.redirect_action_index
                ),
                "",
            )
            .replace(
                &format!(
                    "temporary_redirect_action_cookie={}\n",
                    checkpoint.temporary.redirect_action_cookie
                ),
                "",
            );
        assert!(RuntimeOverrideCheckpoint::decode(&old_absent)
            .unwrap_err()
            .contains("predates deterministic namespace authority"));

        let managed =
            RuntimeOverrideCheckpoint::new(&permit(), 1_000, RuntimeBaseline::Managed(baseline()))
                .unwrap();
        assert!(managed.validate_against(&absent_permit_value).is_err());
        assert!(checkpoint.validate_against(&permit()).is_err());

        let false_managed_claim = encoded.replacen(
            "managed_sqm_was_active=0\n",
            "managed_sqm_was_active=1\n",
            1,
        );
        assert!(RuntimeOverrideCheckpoint::decode(&false_managed_claim).is_err());
    }

    #[test]
    fn absent_permit_target_binding_is_strict_and_seeds_are_not_identity_evidence() {
        let permit = absent_permit();
        permit.validate().unwrap();

        let mut target_drift = permit.clone();
        target_drift.target_interface = "eth9".to_string();
        assert!(target_drift.validate().is_err());
        let mut embedded_target_drift = permit.clone();
        let RuntimeBaseline::Absent(absent) = &mut embedded_target_drift.baseline else {
            unreachable!()
        };
        absent.target_interface = "eth9".to_string();
        assert!(embedded_target_drift.validate().is_err());

        let baseline_identity = permit.baseline.clone();
        let mut different_search_authority = permit.clone();
        different_search_authority.initial_download_kbps = 200_000;
        different_search_authority.initial_upload_kbps = 100_000;
        different_search_authority.download_qdisc_kind =
            super::super::autotune_runtime::RuntimeQdiscKind::CakeMq;
        different_search_authority.upload_qdisc_kind =
            super::super::autotune_runtime::RuntimeQdiscKind::CakeMq;
        different_search_authority.validate().unwrap();
        assert_eq!(different_search_authority.baseline, baseline_identity);
    }

    #[test]
    fn directional_baseline_retains_a_full_private_topology_plan() {
        let mut permit = permit();
        permit.baseline = RuntimeBaseline::Managed(MeasurementTopology::UploadOnlyShaped);
        let mut baseline = baseline();
        baseline.topology = MeasurementTopology::UploadOnlyShaped;
        baseline.download_kbps = None;
        baseline.download_qdisc_kind = None;
        let checkpoint =
            RuntimeOverrideCheckpoint::new(&permit, 1_000, RuntimeBaseline::Managed(baseline))
                .unwrap();
        assert_eq!(checkpoint.temporary_stage, TemporaryTopologyStage::Planned);
        assert!(checkpoint.temporary.ifb_ifindex.is_none());
        assert!(checkpoint.managed_sqm_was_active);
        checkpoint.validate_against(&permit).unwrap();
    }

    #[test]
    fn checkpoint_stage_transitions_are_monotonic_and_compare_before_replace() {
        let root = temp_root("checkpoint-transition");
        let store = RuntimeOverrideStore::open(&root).unwrap();
        let permit = permit();
        let planned =
            RuntimeOverrideCheckpoint::new(&permit, 1_000, RuntimeBaseline::Managed(baseline()))
                .unwrap();
        store.publish_checkpoint(&planned).unwrap();

        let suspended = planned
            .advance_temporary_stage(TemporaryTopologyStage::ManagedSqmSuspended, None)
            .unwrap();
        store.replace_checkpoint(&planned, &suspended).unwrap();
        let owned = suspended
            .advance_temporary_stage(TemporaryTopologyStage::LinkOwned, Some(42))
            .unwrap();
        store.replace_checkpoint(&suspended, &owned).unwrap();
        assert!(store.replace_checkpoint(&suspended, &owned).is_err());
        assert!(owned
            .advance_temporary_stage(TemporaryTopologyStage::Planned, None)
            .is_err());

        let active = owned
            .advance_temporary_stage(TemporaryTopologyStage::Active, None)
            .unwrap();
        store.replace_checkpoint(&owned, &active).unwrap();
        let absent = active
            .advance_temporary_stage(TemporaryTopologyStage::TemporaryAbsent, None)
            .unwrap();
        store.replace_checkpoint(&active, &absent).unwrap();
        let restored = absent
            .advance_temporary_stage(TemporaryTopologyStage::BaselineRestored, None)
            .unwrap();
        store.replace_checkpoint(&absent, &restored).unwrap();
        assert_eq!(store.read_checkpoint().unwrap(), Some(restored));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn private_store_separates_request_withdrawal_from_restored_finalization() {
        let root = temp_root("roundtrip");
        let store = RuntimeOverrideStore::open(&root).unwrap();
        let permit = permit();
        let control = control();
        let checkpoint =
            RuntimeOverrideCheckpoint::new(&permit, 1_000, RuntimeBaseline::Managed(baseline()))
                .unwrap();
        let ack = AutotuneRuntimeAck {
            permit_id: control.permit_id.clone(),
            job_id: control.job_id.clone(),
            worker_run_id: control.worker_run_id.clone(),
            sequence: control.sequence,
            updated_boot_ms: 2_000,
            target_interface: control.target_interface.clone(),
            route_fingerprint: control.route_fingerprint.clone(),
            sqm_fingerprint: control.sqm_fingerprint.clone(),
            state: super::super::full_autotune::RuntimeAckState::Applied,
            topology: Some(control.topology),
            download_kbps: control.download_kbps,
            upload_kbps: control.upload_kbps,
            diagnostic_code: None,
        };
        store.publish_permit(&permit).unwrap();
        store.publish_checkpoint(&checkpoint).unwrap();
        store.publish_control(&control).unwrap();
        store.publish_ack(&ack).unwrap();
        assert_eq!(store.read_permit().unwrap(), Some(permit));
        assert_eq!(store.read_checkpoint().unwrap(), Some(checkpoint));
        assert_eq!(store.read_control().unwrap(), Some(control));
        assert_eq!(store.read_ack().unwrap(), Some(ack));
        store.withdraw_request().unwrap();
        assert!(store.read_permit().unwrap().is_none());
        assert!(store.read_control().unwrap().is_none());
        assert!(store.read_checkpoint().unwrap().is_some());
        assert!(store.read_ack().unwrap().is_some());
        store.finalize_restored().unwrap();
        assert!(store.read_checkpoint().unwrap().is_none());
        assert!(store.read_ack().unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn voluntary_restore_keeps_exact_intent_until_instance_finalization() {
        let root = temp_root("voluntary-restore");
        let store = RuntimeOverrideStore::open(&root).unwrap();
        let permit = permit();
        let control = control();
        let checkpoint =
            RuntimeOverrideCheckpoint::new(&permit, 1_000, RuntimeBaseline::Managed(baseline()))
                .unwrap();
        store.publish_permit(&permit).unwrap();
        store.publish_checkpoint(&checkpoint).unwrap();
        store.publish_control(&control).unwrap();

        let mut foreign = control.clone();
        foreign.sequence += 1;
        assert!(store.request_restore(&foreign).is_err());
        assert_eq!(store.read_control().unwrap(), Some(control.clone()));
        assert!(store.read_restore_intent().unwrap().is_none());

        store.request_restore(&control).unwrap();
        assert_eq!(store.read_permit().unwrap(), Some(permit.clone()));
        assert!(store.read_control().unwrap().is_none());
        assert_eq!(store.read_restore_intent().unwrap(), Some(control.clone()));
        assert!(store.read_checkpoint().unwrap().is_some());

        let mut wrong_ack = restored_ack(&control, 2_000);
        wrong_ack.sequence += 1;
        store.publish_ack(&wrong_ack).unwrap();
        assert!(store
            .release_restored_request(&permit, &control, 2_000)
            .is_err());
        assert_eq!(store.read_permit().unwrap(), Some(permit.clone()));

        store.publish_ack(&restored_ack(&control, 2_000)).unwrap();
        store
            .release_restored_request(&permit, &control, 2_000)
            .unwrap();
        assert!(store.read_permit().unwrap().is_none());
        assert!(store.read_restore_intent().unwrap().is_some());
        store.finalize_restored().unwrap();
        assert!(store.read_restore_intent().unwrap().is_none());
        assert!(store.read_checkpoint().unwrap().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn crash_recovery_withdraws_only_an_exact_restored_owner() {
        let root = temp_root("crash-restored-release");
        let store = RuntimeOverrideStore::open(&root).unwrap();
        let permit = permit();
        let control = control();
        let checkpoint =
            RuntimeOverrideCheckpoint::new(&permit, 1_000, RuntimeBaseline::Managed(baseline()))
                .unwrap();
        store.publish_permit(&permit).unwrap();
        store.publish_checkpoint(&checkpoint).unwrap();
        store.publish_control(&control).unwrap();

        let mut wrong_ack = restored_ack(&control, 2_000);
        wrong_ack.route_fingerprint = "aa".repeat(32);
        store.publish_ack(&wrong_ack).unwrap();
        assert!(store
            .withdraw_restored_request(&permit, &control, 2_000)
            .is_err());
        assert_eq!(store.read_permit().unwrap(), Some(permit.clone()));
        assert_eq!(store.read_control().unwrap(), Some(control.clone()));

        store.publish_ack(&restored_ack(&control, 2_000)).unwrap();
        store
            .withdraw_restored_request(&permit, &control, 2_000)
            .unwrap();
        assert!(store.read_permit().unwrap().is_none());
        assert!(store.read_control().unwrap().is_none());
        assert_eq!(store.read_checkpoint().unwrap(), Some(checkpoint));
        assert!(store.read_ack().unwrap().is_some());
        store.finalize_restored().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn store_discards_only_safe_temps_and_rejects_unknown_or_symlink_entries() {
        let root = temp_root("entries");
        let store = RuntimeOverrideStore::open(&root).unwrap();
        let runtime_root = root.join(RUNTIME_DIR);
        let temp = runtime_root.join(".tmp-safe");
        File::create(&temp).unwrap();
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o600)).unwrap();
        drop(store);
        RuntimeOverrideStore::open(&root).unwrap();
        assert!(!temp.exists());

        File::create(runtime_root.join("unknown")).unwrap();
        assert!(RuntimeOverrideStore::open(&root).is_err());
        fs::remove_file(runtime_root.join("unknown")).unwrap();
        symlink("/tmp", runtime_root.join(CONTROL_FILE)).unwrap();
        assert!(RuntimeOverrideStore::open(&root).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
