//! Pure managed-configuration planning for an absent Auto-Tune target.
//!
//! It turns an already typed native Full Auto-Tune selection into an immutable
//! description of the UCI sections bootstrap Apply creates. A shaped result
//! owns one cake-autorate and one SQM section; an explicit raw/no-SQM result
//! owns only a disabled cake-autorate section. It remains
//! pure: process, filesystem and live-state authority stay in the executor and
//! OpenWrt attestation layers.

#[cfg(test)]
use super::autotune_apply::NativeRawFallbackApplyManifestInput;
use super::autotune_apply::{
    CanonicalDirection, NativeApplyAction, NativeApplyArtifactDigestsOwned,
    NativeApplyDirectionMode, NativeApplyExecutionPlan, NativeSqmDirectionMode,
};
use super::autotune_capture_policy::AutotuneCapturePolicy;
use super::protocol::{
    OperationKind, OperationOrigin, OperationRequest, OperationRouteMode, OperationTargetState,
};
use super::sqm_identity;
use crate::autotune::{
    AutotuneProfile, CapacityLearningPolicy, DirectionProposal, LinkKind, MAX_RATE_KBPS,
};
use ring::digest::{digest, SHA256};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const NATIVE_MANAGED_CONFIG_SCHEMA_VERSION: u8 = 1;
pub(crate) const NATIVE_DISABLED_MANAGED_CONFIG_SCHEMA_VERSION: u8 = 2;
const MAX_NATIVE_MANAGED_CONFIG_BYTES: usize = 64 * 1024;
const MAX_NATIVE_BOOTSTRAP_CONFIG_ACTIONS: usize = 128;
const MAX_NATIVE_UCI_LIST_ITEMS: usize = 64;
const MAX_NATIVE_UCI_LIST_ITEM_BYTES: usize = 1024;
const MAX_NATIVE_UCI_LIST_TOTAL_BYTES: usize = 16 * 1024;
#[cfg(test)]
const SPEEDTEST_APPLY_PERCENT_V1: u8 = 90;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeBootstrapServerPersistence {
    /// The request's server ID is operation authority, not proof that an
    /// automatically selected run used that server.  Until native evidence
    /// carries the observed server identity, omit the UCI pin deliberately.
    LeaveAutomatic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NativeBootstrapPersistPolicy {
    speedtest_apply_percent: u8,
    server: NativeBootstrapServerPersistence,
}

impl NativeBootstrapPersistPolicy {
    pub(crate) fn new(
        speedtest_apply_percent: u8,
        server: NativeBootstrapServerPersistence,
    ) -> Result<Self, String> {
        if !(1..=100).contains(&speedtest_apply_percent) {
            return Err(
                "native bootstrap speed-test apply percentage must be between 1 and 100"
                    .to_string(),
            );
        }
        Ok(Self {
            speedtest_apply_percent,
            server,
        })
    }

    #[cfg(test)]
    pub(crate) fn defaults_v1() -> Self {
        Self {
            speedtest_apply_percent: SPEEDTEST_APPLY_PERCENT_V1,
            server: NativeBootstrapServerPersistence::LeaveAutomatic,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum NativeManagedPackage {
    CakeAutorate,
    Sqm,
}

impl NativeManagedPackage {
    fn as_str(self) -> &'static str {
        match self {
            Self::CakeAutorate => "cake-autorate",
            Self::Sqm => "sqm",
        }
    }

    fn section_type(self) -> &'static str {
        match self {
            Self::CakeAutorate => "cake_autorate",
            Self::Sqm => "queue",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BoundedUciList {
    values: Vec<String>,
}

impl BoundedUciList {
    fn new(option: &str, values: Vec<String>) -> Result<Self, String> {
        if values.is_empty() || values.len() > MAX_NATIVE_UCI_LIST_ITEMS {
            return Err(format!(
                "native bootstrap UCI list for {option} has an invalid item count"
            ));
        }
        let mut unique = BTreeSet::new();
        let mut total_bytes = 0usize;
        for value in &values {
            validate_uci_value(option, value)?;
            if value.is_empty() || value.len() > MAX_NATIVE_UCI_LIST_ITEM_BYTES {
                return Err(format!(
                    "native bootstrap UCI list item for {option} is outside its byte bound"
                ));
            }
            total_bytes = total_bytes.checked_add(value.len()).ok_or_else(|| {
                format!("native bootstrap UCI list for {option} exceeds its byte bound")
            })?;
            if total_bytes > MAX_NATIVE_UCI_LIST_TOTAL_BYTES {
                return Err(format!(
                    "native bootstrap UCI list for {option} exceeds its byte bound"
                ));
            }
            if !unique.insert(value.as_str()) {
                return Err(format!(
                    "native bootstrap UCI list for {option} contains a duplicate item"
                ));
            }
        }
        Ok(Self { values })
    }

    pub(crate) fn values(&self) -> &[String] {
        &self.values
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeBootstrapPingerMethod {
    Fping,
    // Kept in the typed managed-config contract even though the frozen
    // capture policy currently selects only fping.
    #[allow(dead_code)]
    FpingTs,
    #[allow(dead_code)]
    Tsping,
    #[allow(dead_code)]
    Ping,
    Irtt,
}

impl NativeBootstrapPingerMethod {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fping => "fping",
            Self::FpingTs => "fping-ts",
            Self::Tsping => "tsping",
            Self::Ping => "ping",
            Self::Irtt => "irtt",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeBootstrapProbePlan {
    method: NativeBootstrapPingerMethod,
    no_pingers: u8,
    reflectors: BoundedUciList,
}

impl NativeBootstrapProbePlan {
    pub(crate) fn from_capture_policy(policy: &AutotuneCapturePolicy) -> Result<Self, String> {
        let method = match policy.pinger_method() {
            "fping" => NativeBootstrapPingerMethod::Fping,
            _ => {
                return Err(
                    "native bootstrap capture policy uses an unsupported pinger method".to_string(),
                )
            }
        };
        Self::new(
            method,
            policy.active_pingers(),
            policy.reflectors().to_vec(),
        )
    }

    pub(crate) fn new(
        method: NativeBootstrapPingerMethod,
        no_pingers: u8,
        reflectors: Vec<String>,
    ) -> Result<Self, String> {
        if method == NativeBootstrapPingerMethod::Irtt {
            return Err(
                "native bootstrap managed config does not support IRTT until its explicit server and clock authority is native"
                    .to_string(),
            );
        }
        let reflectors = BoundedUciList::new("reflector", reflectors)?;
        for reflector in reflectors.values() {
            if reflector.starts_with('-')
                || !reflector
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b":.-".contains(&byte))
            {
                return Err(
                    "native bootstrap reflector is not a bounded host or address candidate"
                        .to_string(),
                );
            }
        }
        if no_pingers == 0 || usize::from(no_pingers) > reflectors.values().len() {
            return Err(
                "native bootstrap active pinger count must fit the reflector list".to_string(),
            );
        }
        Ok(Self {
            method,
            no_pingers,
            reflectors,
        })
    }
}

fn milliseconds_as_seconds(value: u32) -> String {
    let whole = value / 1_000;
    let remainder = value % 1_000;
    if remainder == 0 {
        return whole.to_string();
    }
    let mut fraction = format!("{remainder:03}");
    while fraction.ends_with('0') {
        fraction.pop();
    }
    format!("{whole}.{fraction}")
}

fn ppm_as_ratio(value: u32) -> String {
    let whole = value / 1_000_000;
    let remainder = value % 1_000_000;
    if remainder == 0 {
        return whole.to_string();
    }
    let mut fraction = format!("{remainder:06}");
    while fraction.ends_with('0') {
        fraction.pop();
    }
    format!("{whole}.{fraction}")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NativeManagedUciValue {
    Scalar(String),
    ReplaceList(BoundedUciList),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeManagedUciSectionPlan {
    package: NativeManagedPackage,
    section: String,
    section_type: &'static str,
    options: BTreeMap<&'static str, NativeManagedUciValue>,
}

impl NativeManagedUciSectionPlan {
    fn new(package: NativeManagedPackage, section: &str) -> Result<Self, String> {
        sqm_identity::validate_uci_section(section)?;
        Ok(Self {
            package,
            section: section.to_string(),
            section_type: package.section_type(),
            options: BTreeMap::new(),
        })
    }

    fn scalar(&mut self, option: &'static str, value: impl Into<String>) -> Result<(), String> {
        validate_uci_option(option)?;
        let value = value.into();
        validate_uci_value(option, &value)?;
        self.insert(option, NativeManagedUciValue::Scalar(value))
    }

    fn replace_list(&mut self, option: &'static str, value: BoundedUciList) -> Result<(), String> {
        validate_uci_option(option)?;
        self.insert(option, NativeManagedUciValue::ReplaceList(value))
    }

    fn insert(&mut self, option: &'static str, value: NativeManagedUciValue) -> Result<(), String> {
        if self.options.insert(option, value).is_some() {
            return Err(format!(
                "native bootstrap managed config assigns {}.{}.{} more than once",
                self.package.as_str(),
                self.section,
                option
            ));
        }
        Ok(())
    }

    pub(crate) fn package(&self) -> NativeManagedPackage {
        self.package
    }

    pub(crate) fn section(&self) -> &str {
        &self.section
    }

    pub(crate) fn section_type(&self) -> &str {
        self.section_type
    }

    pub(crate) fn options(&self) -> &BTreeMap<&'static str, NativeManagedUciValue> {
        &self.options
    }

    #[cfg(test)]
    pub(crate) fn scalar_value(&self, option: &str) -> Option<&str> {
        match self.options.get(option) {
            Some(NativeManagedUciValue::Scalar(value)) => Some(value),
            Some(NativeManagedUciValue::ReplaceList(_)) | None => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn list_value(&self, option: &str) -> Option<&[String]> {
        match self.options.get(option) {
            Some(NativeManagedUciValue::ReplaceList(value)) => Some(value.values()),
            Some(NativeManagedUciValue::Scalar(_)) | None => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NativeManagedDirectionPlan {
    mode: NativeApplyDirectionMode,
    minimum_kbps: u64,
    base_kbps: u64,
    maximum_kbps: u64,
    adaptive_cap_kbps: u64,
    adaptive_safe_kbps: u64,
    adaptive_evidence: &'static str,
    adaptive_cap_source: &'static str,
}

impl NativeManagedDirectionPlan {
    fn from_verified_candidate(
        label: &str,
        candidate: &CanonicalDirection,
        expected: DirectionProposal,
    ) -> Result<Self, String> {
        validate_direction_proposal(label, expected)?;
        match candidate.mode {
            NativeApplyDirectionMode::Shaped => {
                let selected = candidate.base_kbps.ok_or_else(|| {
                    format!("native bootstrap verified {label} candidate has no selected rate")
                })?;
                let minimum = candidate.minimum_kbps.ok_or_else(|| {
                    format!("native bootstrap verified {label} candidate has no minimum")
                })?;
                let shaped_validation = candidate.tested_safe_maximum_kbps == Some(selected)
                    && candidate.adaptive_cap_kbps == Some(expected.absolute_cap_kbps)
                    && candidate.ceiling_evidence == Some("shaped_validation");
                let capacity_only = candidate.tested_safe_maximum_kbps == Some(0)
                    && candidate.adaptive_cap_kbps == Some(selected)
                    && candidate.ceiling_evidence == Some("legacy_unverified");
                if !(100..=MAX_RATE_KBPS).contains(&selected)
                    || minimum < expected.exploration_minimum_kbps
                    || minimum > selected
                    || selected > expected.exploration_cap_kbps
                    || selected > expected.absolute_cap_kbps
                    || expected
                        .service_hard_cap_kbps
                        .is_some_and(|cap| selected > cap)
                    || candidate.maximum_kbps != Some(selected)
                    || candidate.service_hard_cap_kbps != expected.service_hard_cap_kbps
                    || !(shaped_validation || capacity_only)
                    || candidate.cap_source != Some(expected.cap_source.as_str())
                    || match candidate.measured_runtime_minimum_kbps {
                        Some(measured) => measured != minimum,
                        None => minimum != expected.exploration_minimum_kbps,
                    }
                {
                    return Err(format!(
                        "native bootstrap {label} candidate contradicts its verified Apply selection"
                    ));
                }
                Ok(Self {
                    mode: candidate.mode,
                    minimum_kbps: minimum,
                    base_kbps: selected,
                    maximum_kbps: selected,
                    adaptive_cap_kbps: if capacity_only {
                        selected
                    } else {
                        expected.absolute_cap_kbps
                    },
                    adaptive_safe_kbps: if capacity_only { 0 } else { selected },
                    adaptive_evidence: if capacity_only {
                        "legacy_unverified"
                    } else {
                        "shaped_validation"
                    },
                    adaptive_cap_source: expected.cap_source.as_str(),
                })
            }
            NativeApplyDirectionMode::Bypass => {
                if candidate.minimum_kbps.is_some()
                    || candidate.measured_runtime_minimum_kbps.is_some()
                    || candidate.base_kbps.is_some()
                    || candidate.maximum_kbps.is_some()
                    || candidate.tested_safe_maximum_kbps.is_some()
                    || candidate.adaptive_cap_kbps.is_some()
                    || candidate.service_hard_cap_kbps.is_some()
                    || candidate.ceiling_evidence.is_some()
                    || candidate.cap_source.is_some()
                {
                    return Err(format!(
                        "native bootstrap bypassed {label} direction carries shaped rates"
                    ));
                }
                // These are inert controller fields required by Config's
                // cross-direction validation.  They remain proposal-derived;
                // neither service caps nor compiled defaults become evidence.
                Ok(Self {
                    mode: candidate.mode,
                    minimum_kbps: expected.minimum_kbps,
                    base_kbps: expected.base_kbps,
                    maximum_kbps: expected.maximum_kbps,
                    adaptive_cap_kbps: expected.absolute_cap_kbps,
                    adaptive_safe_kbps: 0,
                    adaptive_evidence: "legacy_unverified",
                    adaptive_cap_source: expected.cap_source.as_str(),
                })
            }
        }
    }

    fn shaped(&self) -> bool {
        self.mode == NativeApplyDirectionMode::Shaped
    }

    fn queue_rate_kbps(&self) -> u64 {
        if self.shaped() {
            self.base_kbps
        } else {
            0
        }
    }
}

pub(crate) struct BootstrapApplyInputs<'a> {
    /// The existing native Apply builder is the sole authority for the exact
    /// selected rates and Review/digest transaction.  This dormant projection
    /// does not accept a second, forgeable request/proposal/direction tuple.
    pub(crate) verified_apply: &'a NativeApplyExecutionPlan,
    /// This is the exact request-bound measurement policy.  Probe persistence
    /// is derived from it; callers cannot substitute a second reflector or
    /// transport policy after calibration.
    pub(crate) capture_policy: &'a AutotuneCapturePolicy,
    pub(crate) persist_policy: NativeBootstrapPersistPolicy,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeManagedConfigPlan {
    schema_version: u8,
    instance: String,
    requested_target_interface: String,
    managed_l3_interface: String,
    planned_sqm_section: String,
    cake: NativeManagedUciSectionPlan,
    sqm: Option<NativeManagedUciSectionPlan>,
}

impl NativeManagedConfigPlan {
    pub(crate) fn from_bootstrap(input: BootstrapApplyInputs<'_>) -> Result<Self, String> {
        validate_bootstrap_input(&input)?;
        let verified = input.verified_apply;
        let request = &verified.request;
        let instance = request.identity.instance.as_str();
        let requested_target = request.identity.target_interface.as_str();
        let target = request.route.l3_device.as_str();
        let sqm_section = request
            .managed_sqm_section
            .as_deref()
            .ok_or_else(|| "native bootstrap request has no planned SQM section".to_string())?;
        validate_kernel_interface("native bootstrap route L3 device", target)?;
        let dl_if = format!("ifb4{target}");
        validate_kernel_interface("native bootstrap download IFB", &dl_if)?;

        validate_topology(
            &verified.selected_topology,
            verified.action,
            verified.sqm_direction_mode,
            verified.download.mode,
            verified.upload.mode,
        )?;
        let probe_plan = NativeBootstrapProbePlan::from_capture_policy(input.capture_policy)?;

        let mut cake =
            NativeManagedUciSectionPlan::new(NativeManagedPackage::CakeAutorate, instance)?;
        let (schema_version, sqm) = if verified.action == NativeApplyAction::DisableSqm {
            project_disabled_cake_section(
                &mut cake,
                request,
                &verified.proposal,
                &probe_plan,
                input.capture_policy,
                input.persist_policy,
                &dl_if,
            )?;
            (NATIVE_DISABLED_MANAGED_CONFIG_SCHEMA_VERSION, None)
        } else {
            let download = NativeManagedDirectionPlan::from_verified_candidate(
                "download",
                &verified.download,
                verified.proposal.download,
            )?;
            let upload = NativeManagedDirectionPlan::from_verified_candidate(
                "upload",
                &verified.upload,
                verified.proposal.upload,
            )?;
            if verified.proposal.active_threshold_kbps > download.minimum_kbps
                || verified.proposal.active_threshold_kbps > upload.minimum_kbps
            {
                return Err(
                    "native bootstrap active threshold exceeds a planned direction minimum"
                        .to_string(),
                );
            }
            project_cake_section(
                &mut cake,
                request,
                verified.sqm_direction_mode,
                &download,
                &upload,
                &verified.proposal,
                &probe_plan,
                input.capture_policy,
                input.persist_policy,
                &dl_if,
            )?;
            let mut sqm = NativeManagedUciSectionPlan::new(NativeManagedPackage::Sqm, sqm_section)?;
            project_sqm_section(
                &mut sqm,
                instance,
                target,
                &download,
                &upload,
                &verified.proposal,
            )?;
            (NATIVE_MANAGED_CONFIG_SCHEMA_VERSION, Some(sqm))
        };

        let plan = Self {
            schema_version,
            instance: instance.to_string(),
            requested_target_interface: requested_target.to_string(),
            managed_l3_interface: target.to_string(),
            planned_sqm_section: sqm_section.to_string(),
            cake,
            sqm,
        };
        if plan.action_count() > MAX_NATIVE_BOOTSTRAP_CONFIG_ACTIONS {
            return Err(format!(
                "native bootstrap managed config exceeds its {}-action bound",
                MAX_NATIVE_BOOTSTRAP_CONFIG_ACTIONS
            ));
        }
        let _ = plan.canonical_bytes()?;
        Ok(plan)
    }

    pub(crate) fn cake(&self) -> &NativeManagedUciSectionPlan {
        &self.cake
    }

    pub(crate) fn sqm(&self) -> Option<&NativeManagedUciSectionPlan> {
        self.sqm.as_ref()
    }

    pub(crate) fn schema_version(&self) -> u8 {
        self.schema_version
    }

    pub(crate) fn action_count(&self) -> usize {
        1 + self.cake.options.len()
            + self
                .sqm
                .as_ref()
                .map_or(0, |section| 1 + section.options.len())
    }

    pub(crate) fn canonical_bytes(&self) -> Result<Vec<u8>, String> {
        let sections = match &self.sqm {
            Some(sqm) => format!("{},{}", section_json(&self.cake), section_json(sqm)),
            None => section_json(&self.cake),
        };
        let mut output = format!(
            concat!(
                "{{\"native_managed_config_schema_version\":{},",
                "\"instance\":{},\"requested_target_interface\":{},",
                "\"managed_l3_interface\":{},",
                "\"planned_sqm_section\":{},\"sections\":[{}]}}\n"
            ),
            self.schema_version,
            json_string(&self.instance),
            json_string(&self.requested_target_interface),
            json_string(&self.managed_l3_interface),
            json_string(&self.planned_sqm_section),
            sections,
        );
        if output.len() > MAX_NATIVE_MANAGED_CONFIG_BYTES {
            return Err("native bootstrap managed config exceeds its byte bound".to_string());
        }
        if output.matches('\n').count() != 1 || !output.ends_with('\n') {
            return Err("native bootstrap managed config is not one canonical line".to_string());
        }
        Ok(std::mem::take(&mut output).into_bytes())
    }

    pub(crate) fn canonical_sha256(&self) -> Result<String, String> {
        let digest = digest(&SHA256, &self.canonical_bytes()?);
        Ok(hex_lower(digest.as_ref()))
    }
}

fn validate_bootstrap_input(input: &BootstrapApplyInputs<'_>) -> Result<(), String> {
    let verified = input.verified_apply;
    verified.validate_exact_invariants()?;
    let request = &verified.request;
    request.validate()?;
    if request.target_state != OperationTargetState::AbsentBootstrap
        || request.identity.operation != OperationKind::FullAutotune
        || request.origin != OperationOrigin::Luci
        || request.scheduled_auto_apply_requested
    {
        return Err(
            "native managed-config bootstrap requires a manual LuCI absent Full Auto-Tune request"
                .to_string(),
        );
    }
    if !matches!(
        (&verified.artifacts, verified.action),
        (
            NativeApplyArtifactDigestsOwned::ShapedV4 { .. },
            NativeApplyAction::ApplySqm,
        ) | (
            NativeApplyArtifactDigestsOwned::RawFallbackV5 { .. },
            NativeApplyAction::DisableSqm,
        ) | (
            NativeApplyArtifactDigestsOwned::ShapedCapacityFallbackV9 { .. },
            NativeApplyAction::ApplySqm,
        )
    ) {
        return Err(
            "native managed-config bootstrap source/action schema is inconsistent".to_string(),
        );
    }
    if request.backend != "speedtest-go" {
        return Err("native managed-config bootstrap requires speedtest-go".to_string());
    }
    let proposal = &verified.proposal;
    let profile = input
        .verified_apply
        .request
        .profile
        .ok_or_else(|| "native bootstrap request has no profile".to_string())?;
    if proposal.profile != profile
        || proposal.access_medium
            != request
                .access_medium
                .ok_or_else(|| "native bootstrap request has no access medium".to_string())?
        || proposal.access_source
            != request.access_source.ok_or_else(|| {
                "native bootstrap request has no access evidence source".to_string()
            })?
        || proposal.access_confidence_percent != u64::from(request.access_confidence_percent)
        || proposal.capacity_learning_policy
            != request.capacity_learning_policy.ok_or_else(|| {
                "native bootstrap request has no capacity-learning policy".to_string()
            })?
        || proposal.download.service_hard_cap_kbps != request.service_dl_cap_kbps
        || proposal.upload.service_hard_cap_kbps != request.service_ul_cap_kbps
    {
        return Err(
            "native managed-config proposal contradicts immutable request policy".to_string(),
        );
    }
    validate_proposal_policy(proposal)?;
    validate_topology(
        &verified.selected_topology,
        verified.action,
        verified.sqm_direction_mode,
        verified.download.mode,
        verified.upload.mode,
    )?;
    Ok(())
}

fn validate_topology(
    selected_topology: &str,
    action: NativeApplyAction,
    direction_mode: NativeSqmDirectionMode,
    download: NativeApplyDirectionMode,
    upload: NativeApplyDirectionMode,
) -> Result<(), String> {
    let valid = matches!(
        (selected_topology, action, direction_mode, download, upload),
        (
            "both_shaped",
            NativeApplyAction::ApplySqm,
            NativeSqmDirectionMode::Both,
            NativeApplyDirectionMode::Shaped,
            NativeApplyDirectionMode::Shaped,
        ) | (
            "download_only_shaped",
            NativeApplyAction::ApplySqm,
            NativeSqmDirectionMode::DownloadOnly,
            NativeApplyDirectionMode::Shaped,
            NativeApplyDirectionMode::Bypass,
        ) | (
            "upload_only_shaped",
            NativeApplyAction::ApplySqm,
            NativeSqmDirectionMode::UploadOnly,
            NativeApplyDirectionMode::Bypass,
            NativeApplyDirectionMode::Shaped,
        ) | (
            "no_sqm",
            NativeApplyAction::DisableSqm,
            NativeSqmDirectionMode::Off,
            NativeApplyDirectionMode::Bypass,
            NativeApplyDirectionMode::Bypass,
        )
    );
    if valid {
        Ok(())
    } else {
        Err(
            "native managed-config bootstrap has an unsupported or contradictory topology"
                .to_string(),
        )
    }
}

fn validate_proposal_policy(proposal: &crate::autotune::AutotuneProposal) -> Result<(), String> {
    let profile = proposal.profile;
    if proposal.target_grade != profile.target_grade()
        || proposal.quality_target_required != profile.quality_target_required()
        || proposal.throughput_priority != profile.throughput_priority()
        || proposal.validation_thresholds != profile.validation_thresholds()
        || proposal.confidence > 100
    {
        return Err("native bootstrap proposal profile policy is inconsistent".to_string());
    }
    crate::autotune::validate_capacity_learning_service_caps(
        Some(proposal.capacity_learning_policy),
        proposal.download.service_hard_cap_kbps,
        proposal.upload.service_hard_cap_kbps,
    )?;
    if proposal.active_threshold_kbps == 0
        || proposal.adjust_up_threshold_ms == 0
        || proposal.delay_threshold_ms == 0
        || proposal.adjust_down_threshold_ms == 0
        || proposal.adaptive_hold_s == 0
        || proposal.adaptive_growth_percent == 0
        || proposal.adaptive_growth_percent > 10
        || proposal.adaptive_probe_s == 0
        || proposal.adaptive_cooldown_s == 0
        || proposal.adaptive_failed_bound_ttl_s == 0
    {
        return Err("native bootstrap proposal controller policy is invalid".to_string());
    }
    let expected_adaptive = matches!(
        proposal.capacity_learning_policy,
        CapacityLearningPolicy::PassiveBounded | CapacityLearningPolicy::ScheduledActive
    );
    if proposal.adaptive_ceiling_enabled != expected_adaptive {
        return Err(
            "native bootstrap adaptive state contradicts capacity-learning policy".to_string(),
        );
    }
    let expected_link = match proposal.link_kind {
        LinkKind::Pppoe => ("ethernet", 44, 84),
        LinkKind::Ethernet => ("ethernet", 18, 64),
        LinkKind::Cellular | LinkKind::Unknown => ("none", 0, 0),
    };
    if (proposal.link_layer, proposal.overhead, proposal.mpu) != expected_link {
        return Err("native bootstrap proposal link-layer policy is inconsistent".to_string());
    }
    let gaming = matches!(
        profile,
        AutotuneProfile::Gaming | AutotuneProfile::GamingExtreme
    );
    let expected_iqdisc = if gaming { "diffserv4" } else { "besteffort" };
    if proposal.sqm.qdisc != "cake"
        || proposal.sqm.script != "layer_cake.qos"
        || proposal.sqm.classification != "diffserv4"
        || proposal.sqm.squash_dscp == gaming
        || proposal.sqm.squash_ingress == gaming
        || proposal.sqm.ingress_ecn != "ECN"
        || proposal.sqm.egress_ecn != "NOECN"
        || proposal.sqm.iqdisc_opts != expected_iqdisc
        || proposal.sqm.eqdisc_opts != "diffserv4"
    {
        return Err("native bootstrap proposal SQM policy is inconsistent".to_string());
    }
    Ok(())
}

fn validate_direction_proposal(label: &str, direction: DirectionProposal) -> Result<(), String> {
    for (name, value) in [
        ("minimum", direction.minimum_kbps),
        ("exploration minimum", direction.exploration_minimum_kbps),
        ("base", direction.base_kbps),
        ("maximum", direction.maximum_kbps),
        ("exploration cap", direction.exploration_cap_kbps),
        ("absolute cap", direction.absolute_cap_kbps),
        ("observed low", direction.observed_low_kbps),
        ("observed median", direction.observed_median_kbps),
        ("observed high", direction.observed_high_kbps),
    ] {
        if !(100..=MAX_RATE_KBPS).contains(&value) {
            return Err(format!(
                "native bootstrap {label} proposal {name} is outside the rate bound"
            ));
        }
    }
    if direction.exploration_minimum_kbps > direction.minimum_kbps
        || direction.minimum_kbps > direction.base_kbps
        || direction.base_kbps > direction.maximum_kbps
        || direction.maximum_kbps > direction.exploration_cap_kbps
        || direction.maximum_kbps > direction.absolute_cap_kbps
        || direction.observed_low_kbps > direction.observed_median_kbps
        || direction.observed_median_kbps > direction.observed_high_kbps
        || !direction.variability.is_finite()
        || direction.variability < 0.0
    {
        return Err(format!(
            "native bootstrap {label} proposal rate ordering is invalid"
        ));
    }
    if let Some(runtime_minimum) = direction.runtime_minimum_kbps {
        if runtime_minimum != direction.minimum_kbps
            || runtime_minimum < direction.exploration_minimum_kbps
            || runtime_minimum > direction.base_kbps
        {
            return Err(format!(
                "native bootstrap {label} measured runtime minimum is inconsistent"
            ));
        }
    }
    if let Some(service_cap) = direction.service_hard_cap_kbps {
        if !(100..=MAX_RATE_KBPS).contains(&service_cap)
            || direction.minimum_kbps > service_cap
            || direction.base_kbps > service_cap
            || direction.maximum_kbps > service_cap
        {
            return Err(format!(
                "native bootstrap {label} proposal escapes its service cap"
            ));
        }
    }
    Ok(())
}

fn project_disabled_cake_section(
    cake: &mut NativeManagedUciSectionPlan,
    request: &OperationRequest,
    proposal: &crate::autotune::AutotuneProposal,
    probe: &NativeBootstrapProbePlan,
    capture_policy: &AutotuneCapturePolicy,
    persist_policy: NativeBootstrapPersistPolicy,
    dl_if: &str,
) -> Result<(), String> {
    let target = request.route.l3_device.as_str();
    let sqm_section = request
        .managed_sqm_section
        .as_deref()
        .ok_or_else(|| "native bootstrap request has no planned SQM section".to_string())?;
    let strategy = request
        .strategy
        .ok_or_else(|| "native bootstrap request has no calibration strategy".to_string())?;

    // This is an explicit uncalibrated registration, not a dormant shaper.
    // Keep enough immutable route/profile/capture policy to re-run Auto-Tune,
    // but do not persist a selected rate, runtime minimum, adaptive ceiling or
    // SQM queue projection which the raw fallback never measured.
    for (option, value) in [
        ("traffic_profile", "auto"),
        ("traffic_rules_enabled", "0"),
        ("enabled", "0"),
        ("wan_if", target),
        ("route_mode", request.route.mode.as_str()),
        ("auto_interface_preset", "1"),
        ("sqm_direction_mode", "off"),
        ("adjust_dl_shaper_rate", "0"),
        ("adjust_ul_shaper_rate", "0"),
        ("manage_sqm", "1"),
        ("sqm_section", sqm_section),
        ("sqm_enabled", "0"),
        ("speedtest_backend", "speedtest-go"),
        ("manual_rate_limits", "0"),
        ("advanced_settings", "0"),
        ("sqm_interface", target),
        ("ul_if", target),
        ("dl_if", dl_if),
        ("runtime_learning_mode", "fixed"),
        ("scheduled_autotune_enabled", "0"),
        ("adaptive_ceiling_enabled", "0"),
    ] {
        cake.scalar(option, value)?;
    }
    match request.route.mode {
        OperationRouteMode::Main => {}
        OperationRouteMode::Mwan3 => cake.scalar(
            "mwan3_member",
            request
                .route
                .mwan3_member
                .as_deref()
                .ok_or_else(|| "native bootstrap mwan3 route has no member".to_string())?,
        )?,
    }
    match persist_policy.server {
        NativeBootstrapServerPersistence::LeaveAutomatic => {}
    }
    cake.scalar(
        "speedtest_apply_percent",
        persist_policy.speedtest_apply_percent.to_string(),
    )?;
    cake.scalar(
        "autotune_profile",
        match proposal.profile {
            AutotuneProfile::GamingExtreme => AutotuneProfile::Gaming.as_str(),
            profile => profile.as_str(),
        },
    )?;
    cake.scalar("autotune_calibration_strategy", strategy.as_str())?;
    cake.scalar("pinger_method", probe.method.as_str())?;
    cake.scalar("no_pingers", probe.no_pingers.to_string())?;
    cake.scalar("ping_extra_args", format!("-I {target}"))?;
    cake.replace_list("reflector", probe.reflectors.clone())?;
    cake.scalar(
        "reflector_ping_interval_s",
        milliseconds_as_seconds(capture_policy.reflector_ping_interval_ms()),
    )?;
    cake.scalar(
        "reflector_response_deadline_s",
        milliseconds_as_seconds(capture_policy.reflector_response_deadline_ms()),
    )?;
    cake.scalar(
        "monitor_achieved_rates_interval_ms",
        capture_policy.rate_sample_interval_ms().to_string(),
    )?;
    cake.scalar(
        "rating_capture_ack_ratio",
        ppm_as_ratio(capture_policy.reverse_ack_ratio_ppm()),
    )?;
    cake.scalar(
        "monitor_cpu_usage_interval_ms",
        capture_policy.cpu_sample_interval_ms().to_string(),
    )?;
    cake.scalar(
        "access_medium_selection",
        if proposal.access_source.as_str() == "user_selected" {
            proposal.access_medium.as_str()
        } else {
            "auto"
        },
    )?;
    cake.scalar("access_medium", proposal.access_medium.as_str())?;
    cake.scalar("access_medium_source", proposal.access_source.as_str())?;
    cake.scalar(
        "access_medium_confidence_percent",
        proposal.access_confidence_percent.to_string(),
    )?;
    cake.scalar(
        "capacity_learning_policy",
        proposal.capacity_learning_policy.as_str(),
    )?;
    if let Some(cap) = proposal.download.service_hard_cap_kbps {
        cake.scalar("service_dl_cap_kbps", cap.to_string())?;
    }
    if let Some(cap) = proposal.upload.service_hard_cap_kbps {
        cake.scalar("service_ul_cap_kbps", cap.to_string())?;
    }
    Ok(())
}

fn project_cake_section(
    cake: &mut NativeManagedUciSectionPlan,
    request: &OperationRequest,
    direction_mode: NativeSqmDirectionMode,
    download: &NativeManagedDirectionPlan,
    upload: &NativeManagedDirectionPlan,
    proposal: &crate::autotune::AutotuneProposal,
    probe: &NativeBootstrapProbePlan,
    capture_policy: &AutotuneCapturePolicy,
    persist_policy: NativeBootstrapPersistPolicy,
    dl_if: &str,
) -> Result<(), String> {
    // The requested target is durable operation identity (for example the
    // logical OpenWrt interface `wan`). Every runtime-facing option must use
    // the route-attested L3 device (for example `pppoe-wan`).
    let target = request.route.l3_device.as_str();
    let sqm_section = request
        .managed_sqm_section
        .as_deref()
        .ok_or_else(|| "native bootstrap request has no planned SQM section".to_string())?;
    let strategy = request
        .strategy
        .ok_or_else(|| "native bootstrap request has no calibration strategy".to_string())?;

    for (option, value) in [
        ("traffic_profile", "auto"),
        ("traffic_rules_enabled", "0"),
        ("enabled", "1"),
        ("wan_if", target),
        ("route_mode", request.route.mode.as_str()),
        ("auto_interface_preset", "1"),
        ("sqm_direction_mode", direction_mode.as_str()),
        (
            "adjust_dl_shaper_rate",
            if download.shaped() { "1" } else { "0" },
        ),
        (
            "adjust_ul_shaper_rate",
            if upload.shaped() { "1" } else { "0" },
        ),
        ("manage_sqm", "1"),
        ("sqm_section", sqm_section),
        ("sqm_enabled", "1"),
        ("speedtest_backend", "speedtest-go"),
        ("manual_rate_limits", "1"),
        ("advanced_settings", "0"),
        ("sqm_interface", target),
        ("ul_if", target),
        ("dl_if", dl_if),
    ] {
        cake.scalar(option, value)?;
    }
    match request.route.mode {
        OperationRouteMode::Main => {}
        OperationRouteMode::Mwan3 => cake.scalar(
            "mwan3_member",
            request
                .route
                .mwan3_member
                .as_deref()
                .ok_or_else(|| "native bootstrap mwan3 route has no member".to_string())?,
        )?,
    }
    match persist_policy.server {
        NativeBootstrapServerPersistence::LeaveAutomatic => {}
    }
    cake.scalar(
        "speedtest_apply_percent",
        persist_policy.speedtest_apply_percent.to_string(),
    )?;
    cake.scalar(
        "autotune_profile",
        match proposal.profile {
            AutotuneProfile::GamingExtreme => AutotuneProfile::Gaming.as_str(),
            profile => profile.as_str(),
        },
    )?;
    cake.scalar("autotune_calibration_strategy", strategy.as_str())?;
    cake.scalar("pinger_method", probe.method.as_str())?;
    cake.scalar("no_pingers", probe.no_pingers.to_string())?;
    cake.scalar("ping_extra_args", format!("-I {target}"))?;
    cake.replace_list("reflector", probe.reflectors.clone())?;
    cake.scalar(
        "reflector_ping_interval_s",
        milliseconds_as_seconds(capture_policy.reflector_ping_interval_ms()),
    )?;
    cake.scalar(
        "reflector_response_deadline_s",
        milliseconds_as_seconds(capture_policy.reflector_response_deadline_ms()),
    )?;
    cake.scalar(
        "monitor_achieved_rates_interval_ms",
        capture_policy.rate_sample_interval_ms().to_string(),
    )?;
    cake.scalar(
        "rating_capture_ack_ratio",
        ppm_as_ratio(capture_policy.reverse_ack_ratio_ppm()),
    )?;
    cake.scalar(
        "monitor_cpu_usage_interval_ms",
        capture_policy.cpu_sample_interval_ms().to_string(),
    )?;

    project_direction(cake, "download", download)?;
    project_direction(cake, "upload", upload)?;

    cake.scalar(
        "connection_active_thr_kbps",
        proposal.active_threshold_kbps.to_string(),
    )?;
    for option in [
        "dl_avg_owd_delta_max_adjust_up_thr_ms",
        "ul_avg_owd_delta_max_adjust_up_thr_ms",
    ] {
        cake.scalar(option, proposal.adjust_up_threshold_ms.to_string())?;
    }
    for option in ["dl_owd_delta_delay_thr_ms", "ul_owd_delta_delay_thr_ms"] {
        cake.scalar(option, proposal.delay_threshold_ms.to_string())?;
    }
    for option in [
        "dl_avg_owd_delta_max_adjust_down_thr_ms",
        "ul_avg_owd_delta_max_adjust_down_thr_ms",
    ] {
        cake.scalar(option, proposal.adjust_down_threshold_ms.to_string())?;
    }

    cake.scalar(
        "adaptive_ceiling_enabled",
        if proposal.adaptive_ceiling_enabled {
            "1"
        } else {
            "0"
        },
    )?;
    project_adaptive_direction(cake, "download", download)?;
    project_adaptive_direction(cake, "upload", upload)?;
    for (option, value) in [
        ("adaptive_ceiling_hold_time_s", proposal.adaptive_hold_s),
        (
            "adaptive_ceiling_growth_percent",
            proposal.adaptive_growth_percent,
        ),
        (
            "adaptive_ceiling_probe_duration_s",
            proposal.adaptive_probe_s,
        ),
        ("adaptive_ceiling_cooldown_s", proposal.adaptive_cooldown_s),
        (
            "adaptive_ceiling_failed_bound_ttl_s",
            proposal.adaptive_failed_bound_ttl_s,
        ),
    ] {
        cake.scalar(option, value.to_string())?;
    }

    cake.scalar(
        "access_medium_selection",
        if proposal.access_source.as_str() == "user_selected" {
            proposal.access_medium.as_str()
        } else {
            "auto"
        },
    )?;
    cake.scalar("access_medium", proposal.access_medium.as_str())?;
    cake.scalar("access_medium_source", proposal.access_source.as_str())?;
    cake.scalar(
        "access_medium_confidence_percent",
        proposal.access_confidence_percent.to_string(),
    )?;
    cake.scalar(
        "capacity_learning_policy",
        proposal.capacity_learning_policy.as_str(),
    )?;
    let (learning_mode, scheduled) = match proposal.capacity_learning_policy {
        CapacityLearningPolicy::ScheduledActive => ("periodic_active", "1"),
        CapacityLearningPolicy::PassiveBounded => ("passive", "0"),
        CapacityLearningPolicy::VerifiedOnly | CapacityLearningPolicy::FixedCap => ("fixed", "0"),
    };
    cake.scalar("runtime_learning_mode", learning_mode)?;
    cake.scalar("scheduled_autotune_enabled", scheduled)?;
    if let Some(cap) = request.service_dl_cap_kbps {
        cake.scalar("service_dl_cap_kbps", cap.to_string())?;
    }
    if let Some(cap) = request.service_ul_cap_kbps {
        cake.scalar("service_ul_cap_kbps", cap.to_string())?;
    }

    cake.scalar("transport_latency_enabled", "1")?;
    cake.scalar(
        "transport_probe_backend",
        capture_policy.transport_backend(),
    )?;
    cake.scalar(
        "transport_probe_endpoint",
        capture_policy.transport_endpoint(),
    )?;
    cake.scalar(
        "transport_probe_loaded_interval_s",
        milliseconds_as_seconds(capture_policy.transport_loaded_interval_ms()),
    )?;
    cake.scalar(
        "transport_probe_timeout_s",
        milliseconds_as_seconds(capture_policy.transport_timeout_ms()),
    )?;
    cake.scalar(
        "transport_load_hold_s",
        milliseconds_as_seconds(capture_policy.transport_load_hold_ms()),
    )?;
    cake.scalar("throughput_guard_enabled", "1")?;
    cake.scalar(
        "throughput_guard_retention_percent",
        canonical_decimal(
            "capacity retention minimum",
            proposal
                .validation_thresholds
                .capacity_retention_min_percent,
        )?,
    )?;
    cake.scalar(
        "quality_target_delay_ms",
        canonical_decimal(
            "transport latency target",
            proposal.validation_thresholds.transport_delta_max_ms,
        )?,
    )?;
    for (option, value) in [
        (
            "throughput_reference_dl_p20_kbps",
            proposal.download.observed_low_kbps,
        ),
        (
            "throughput_reference_dl_p50_kbps",
            proposal.download.observed_median_kbps,
        ),
        (
            "throughput_reference_ul_p20_kbps",
            proposal.upload.observed_low_kbps,
        ),
        (
            "throughput_reference_ul_p50_kbps",
            proposal.upload.observed_median_kbps,
        ),
    ] {
        cake.scalar(option, value.to_string())?;
    }
    project_cake_sqm_policy(cake, proposal)?;
    Ok(())
}

fn project_direction(
    cake: &mut NativeManagedUciSectionPlan,
    direction: &str,
    plan: &NativeManagedDirectionPlan,
) -> Result<(), String> {
    let (sqm_rate, minimum, base, maximum) = match direction {
        "download" => (
            "sqm_download",
            "min_dl_shaper_rate_kbps",
            "base_dl_shaper_rate_kbps",
            "max_dl_shaper_rate_kbps",
        ),
        "upload" => (
            "sqm_upload",
            "min_ul_shaper_rate_kbps",
            "base_ul_shaper_rate_kbps",
            "max_ul_shaper_rate_kbps",
        ),
        _ => return Err("native bootstrap direction is unsupported".to_string()),
    };
    cake.scalar(sqm_rate, plan.base_kbps.to_string())?;
    cake.scalar(minimum, plan.minimum_kbps.to_string())?;
    cake.scalar(base, plan.base_kbps.to_string())?;
    cake.scalar(maximum, plan.maximum_kbps.to_string())
}

fn project_adaptive_direction(
    cake: &mut NativeManagedUciSectionPlan,
    direction: &str,
    plan: &NativeManagedDirectionPlan,
) -> Result<(), String> {
    let (cap, safe, evidence, source) = match direction {
        "download" => (
            "adaptive_ceiling_dl_cap_kbps",
            "adaptive_ceiling_dl_safe_kbps",
            "adaptive_ceiling_dl_evidence",
            "adaptive_ceiling_dl_cap_source",
        ),
        "upload" => (
            "adaptive_ceiling_ul_cap_kbps",
            "adaptive_ceiling_ul_safe_kbps",
            "adaptive_ceiling_ul_evidence",
            "adaptive_ceiling_ul_cap_source",
        ),
        _ => return Err("native bootstrap adaptive direction is unsupported".to_string()),
    };
    cake.scalar(cap, plan.adaptive_cap_kbps.to_string())?;
    cake.scalar(safe, plan.adaptive_safe_kbps.to_string())?;
    cake.scalar(evidence, plan.adaptive_evidence)?;
    cake.scalar(source, plan.adaptive_cap_source)
}

fn project_cake_sqm_policy(
    cake: &mut NativeManagedUciSectionPlan,
    proposal: &crate::autotune::AutotuneProposal,
) -> Result<(), String> {
    for (option, value) in [
        ("sqm_debug_logging", "0"),
        ("sqm_verbosity", "5"),
        ("sqm_qdisc", proposal.sqm.qdisc),
        ("sqm_script", proposal.sqm.script),
        ("sqm_qdisc_advanced", "1"),
        (
            "sqm_squash_dscp",
            if proposal.sqm.squash_dscp { "1" } else { "0" },
        ),
        (
            "sqm_squash_ingress",
            if proposal.sqm.squash_ingress {
                "1"
            } else {
                "0"
            },
        ),
        ("sqm_ingress_ecn", proposal.sqm.ingress_ecn),
        ("sqm_egress_ecn", proposal.sqm.egress_ecn),
        ("sqm_qdisc_really_really_advanced", "1"),
        ("sqm_linklayer", proposal.link_layer),
        (
            "sqm_linklayer_advanced",
            if proposal.link_layer == "none" {
                "0"
            } else {
                "1"
            },
        ),
        ("sqm_tcMTU", "2047"),
        ("sqm_tcTSIZE", "128"),
        ("sqm_linklayer_adaptation_mechanism", "default"),
    ] {
        cake.scalar(option, value)?;
    }
    if !proposal.sqm.iqdisc_opts.is_empty() {
        cake.scalar("sqm_iqdisc_opts", proposal.sqm.iqdisc_opts)?;
    }
    if !proposal.sqm.eqdisc_opts.is_empty() {
        cake.scalar("sqm_eqdisc_opts", proposal.sqm.eqdisc_opts)?;
    }
    cake.scalar("sqm_overhead", proposal.overhead.to_string())?;
    cake.scalar("sqm_tcMPU", proposal.mpu.to_string())
}

fn project_sqm_section(
    sqm: &mut NativeManagedUciSectionPlan,
    instance: &str,
    target: &str,
    download: &NativeManagedDirectionPlan,
    upload: &NativeManagedDirectionPlan,
    proposal: &crate::autotune::AutotuneProposal,
) -> Result<(), String> {
    for (option, value) in [
        ("_cake_autorate_managed", instance),
        ("enabled", "1"),
        ("interface", target),
        ("debug_logging", "0"),
        ("verbosity", "5"),
        ("qdisc", proposal.sqm.qdisc),
        ("script", proposal.sqm.script),
        ("qdisc_advanced", "1"),
        (
            "squash_dscp",
            if proposal.sqm.squash_dscp { "1" } else { "0" },
        ),
        (
            "squash_ingress",
            if proposal.sqm.squash_ingress {
                "1"
            } else {
                "0"
            },
        ),
        ("ingress_ecn", proposal.sqm.ingress_ecn),
        ("egress_ecn", proposal.sqm.egress_ecn),
        ("qdisc_really_really_advanced", "1"),
        ("linklayer", proposal.link_layer),
        (
            "linklayer_advanced",
            if proposal.link_layer == "none" {
                "0"
            } else {
                "1"
            },
        ),
        ("tcMTU", "2047"),
        ("tcTSIZE", "128"),
        ("linklayer_adaptation_mechanism", "default"),
    ] {
        sqm.scalar(option, value)?;
    }
    sqm.scalar("download", download.queue_rate_kbps().to_string())?;
    sqm.scalar("upload", upload.queue_rate_kbps().to_string())?;
    if !proposal.sqm.iqdisc_opts.is_empty() {
        sqm.scalar("iqdisc_opts", proposal.sqm.iqdisc_opts)?;
    }
    if !proposal.sqm.eqdisc_opts.is_empty() {
        sqm.scalar("eqdisc_opts", proposal.sqm.eqdisc_opts)?;
    }
    sqm.scalar("overhead", proposal.overhead.to_string())?;
    sqm.scalar("tcMPU", proposal.mpu.to_string())
}

fn validate_uci_option(option: &str) -> Result<(), String> {
    if option.is_empty()
        || option.len() > 64
        || !option
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err("native bootstrap UCI option is not a bounded identifier".to_string());
    }
    Ok(())
}

fn validate_uci_value(option: &str, value: &str) -> Result<(), String> {
    // Libuci permits TAB, LF and CR among C0 bytes. This single-line batch
    // projection is deliberately stricter: only TAB is data; LF, CR and NUL
    // are framing hazards, while an apostrophe would terminate the quoted
    // value.
    let has_forbidden_control = value.bytes().any(|byte| byte < b' ' && byte != b'\t');
    if value.len() > 1024 || value.contains(['\n', '\r', '\0', '\'']) || has_forbidden_control {
        return Err(format!(
            "native bootstrap UCI value for {option} is not safely batch-encodable"
        ));
    }
    Ok(())
}

fn validate_kernel_interface(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 15
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:@-".contains(&byte))
    {
        return Err(format!("{label} is not a valid kernel interface name"));
    }
    Ok(())
}

fn canonical_decimal(label: &str, value: f64) -> Result<String, String> {
    if !value.is_finite() || !(0.0..=10_000.0).contains(&value) {
        return Err(format!(
            "native bootstrap {label} is outside its numeric bound"
        ));
    }
    Ok(format!("{value:.1}"))
}

fn section_json(section: &NativeManagedUciSectionPlan) -> String {
    let options = section
        .options
        .iter()
        .map(|(option, value)| match value {
            NativeManagedUciValue::Scalar(value) => format!(
                "{{\"option\":{},\"kind\":\"scalar\",\"value\":{}}}",
                json_string(option),
                json_string(value),
            ),
            NativeManagedUciValue::ReplaceList(values) => format!(
                "{{\"option\":{},\"kind\":\"replace_list\",\"values\":[{}]}}",
                json_string(option),
                values
                    .values()
                    .iter()
                    .map(|value| json_string(value))
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"package\":{},\"section\":{},\"section_type\":{},\"options\":[{}]}}",
        json_string(section.package.as_str()),
        json_string(&section.section),
        json_string(section.section_type),
        options,
    )
}

fn json_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\u{0c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character.is_control() => {
                output.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => output.push(character),
        }
    }
    output.push('"');
    output
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autotune::{
        build_proposal_for_profile_with_context, AccessEvidenceSource, AccessMedium,
        AutotuneProposal, LatencyBaseline, ProposalContext,
    };
    use crate::operations::autotune_apply::{
        NativeApplyAcknowledgement, NativeApplyArtifactDigests, NativeApplyDirectionInput,
        NativeApplyManifestInput,
    };
    use crate::operations::protocol::{
        CalibrationStrategy, OperationIdentity, OperationRouteIdentity,
    };
    use std::net::{IpAddr, Ipv4Addr};

    const REVIEW_DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn request(
        profile: AutotuneProfile,
        target: &str,
        route_mode: OperationRouteMode,
        service_dl_cap_kbps: Option<u64>,
        service_ul_cap_kbps: Option<u64>,
    ) -> OperationRequest {
        OperationRequest {
            identity: OperationIdentity {
                job_id: "11".repeat(16),
                job_token: "22".repeat(32),
                instance: "wan_sqm".to_string(),
                operation: OperationKind::FullAutotune,
                target_interface: target.to_string(),
                route_fingerprint: "33".repeat(32),
                config_fingerprint: "44".repeat(32),
                sqm_fingerprint: "55".repeat(32),
            },
            created_unix_ms: 1_000,
            deadline_unix_ms: 2_000,
            origin: OperationOrigin::Luci,
            backend: "speedtest-go".to_string(),
            speedtest_direction: None,
            speedtest_server_id: Some(17_372),
            speedtest_topology: None,
            route: OperationRouteIdentity {
                mode: route_mode,
                mwan3_member: (route_mode == OperationRouteMode::Mwan3).then(|| "wanb".to_string()),
                l3_device: target.to_string(),
                source_ip: Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2))),
                fwmark: (route_mode == OperationRouteMode::Mwan3).then_some(0x200),
                routing_table: (route_mode == OperationRouteMode::Mwan3).then_some(200),
            },
            target_state: OperationTargetState::AbsentBootstrap,
            capture_policy: Some(
                crate::operations::autotune_capture_policy::AutotuneCapturePolicyId::StandardV1,
            ),
            managed_sqm_section: Some("cake_wan_sqm".to_string()),
            profile: Some(profile),
            strategy: Some(CalibrationStrategy::FullRaw),
            access_medium: Some(AccessMedium::SharedWired),
            access_source: Some(AccessEvidenceSource::UserSelected),
            access_confidence_percent: 100,
            capacity_learning_policy: Some(CapacityLearningPolicy::VerifiedOnly),
            service_dl_cap_kbps,
            service_ul_cap_kbps,
            allow_sqm_disable: true,
            allow_active_traffic: false,
            scheduled_auto_apply_requested: false,
            traffic_budget_bytes: 1_000_000_000,
        }
    }

    fn proposal(
        profile: AutotuneProfile,
        service_dl_cap_kbps: Option<u64>,
        service_ul_cap_kbps: Option<u64>,
    ) -> AutotuneProposal {
        let mut proposal = build_proposal_for_profile_with_context(
            &[900_000.0, 910_000.0, 920_000.0],
            &[90_000.0, 95_000.0, 100_000.0],
            LatencyBaseline {
                median_ms: 10.0,
                p95_ms: 12.0,
                samples: 10,
            },
            LinkKind::Pppoe,
            profile,
            ProposalContext {
                access_medium: Some(AccessMedium::SharedWired),
                access_source: AccessEvidenceSource::UserSelected,
                access_confidence_percent: 100,
                capacity_learning_policy: Some(CapacityLearningPolicy::VerifiedOnly),
                download_service_cap_kbps: service_dl_cap_kbps,
                upload_service_cap_kbps: service_ul_cap_kbps,
            },
        )
        .unwrap();
        let download = proposal.download.base_kbps;
        let upload = proposal.upload.base_kbps;
        proposal
            .set_tested_safe_maximums(Some(download), Some(upload))
            .unwrap();
        proposal
    }

    #[derive(Clone, Copy)]
    enum Topology {
        Both,
        DownloadOnly,
        UploadOnly,
        Off,
    }

    fn apply_plan(
        request: &OperationRequest,
        proposal: &AutotuneProposal,
        topology: Topology,
    ) -> NativeApplyExecutionPlan {
        if matches!(topology, Topology::Off) {
            return NativeApplyExecutionPlan::from_verified_raw_fallback(
                NativeRawFallbackApplyManifestInput {
                    option_id: "safe_candidate",
                    request,
                    worker_run_id: &"66".repeat(16),
                    review_digest: REVIEW_DIGEST,
                    coordinator_boot_id: "boot-id",
                    coordinator_generation: &"77".repeat(16),
                    proposal,
                    required_acknowledgements: &[
                        NativeApplyAcknowledgement::DownloadShapingBypassed,
                        NativeApplyAcknowledgement::UploadShapingBypassed,
                        NativeApplyAcknowledgement::SqmDisabled,
                    ],
                    proposal_digest: &"88".repeat(32),
                    raw_fallback_digest: &"99".repeat(32),
                },
            )
            .unwrap();
        }
        let (selected_topology, action, direction_mode, download_mode, upload_mode) = match topology
        {
            Topology::Both => (
                "both_shaped",
                NativeApplyAction::ApplySqm,
                NativeSqmDirectionMode::Both,
                NativeApplyDirectionMode::Shaped,
                NativeApplyDirectionMode::Shaped,
            ),
            Topology::DownloadOnly => (
                "download_only_shaped",
                NativeApplyAction::ApplySqm,
                NativeSqmDirectionMode::DownloadOnly,
                NativeApplyDirectionMode::Shaped,
                NativeApplyDirectionMode::Bypass,
            ),
            Topology::UploadOnly => (
                "upload_only_shaped",
                NativeApplyAction::ApplySqm,
                NativeSqmDirectionMode::UploadOnly,
                NativeApplyDirectionMode::Bypass,
                NativeApplyDirectionMode::Shaped,
            ),
            Topology::Off => unreachable!("raw fallback returned above"),
        };
        let shaped_rate = |mode: NativeApplyDirectionMode, value: u64| {
            (mode == NativeApplyDirectionMode::Shaped).then_some(value)
        };
        NativeApplyExecutionPlan::from_verified_input(NativeApplyManifestInput {
            option_id: "safe_candidate",
            request,
            worker_run_id: &"66".repeat(16),
            review_digest: REVIEW_DIGEST,
            coordinator_boot_id: "boot-id",
            coordinator_generation: &"77".repeat(16),
            selected_topology,
            action,
            sqm_direction_mode: direction_mode,
            download: NativeApplyDirectionInput {
                mode: download_mode,
                selected_kbps: shaped_rate(download_mode, proposal.download.base_kbps),
                measured_runtime_minimum_kbps: None,
                proposal: proposal.download,
            },
            upload: NativeApplyDirectionInput {
                mode: upload_mode,
                selected_kbps: shaped_rate(upload_mode, proposal.upload.base_kbps),
                measured_runtime_minimum_kbps: None,
                proposal: proposal.upload,
            },
            proposal,
            auto_apply_evidence_pass: false,
            manual_review_required: true,
            required_acknowledgements: &[NativeApplyAcknowledgement::MeasurementConfidence],
            artifacts: NativeApplyArtifactDigests {
                proposal: &"88".repeat(32),
                download_search: &"99".repeat(32),
                upload_search: &"aa".repeat(32),
                pair_confirmation: &"bb".repeat(32),
                topology_comparison: &"cc".repeat(32),
            },
        })
        .unwrap()
    }

    fn capture_policy() -> AutotuneCapturePolicy {
        crate::operations::autotune_capture_policy::AutotuneCapturePolicyId::StandardV1
            .expand()
            .unwrap()
    }

    fn build(apply: &NativeApplyExecutionPlan) -> NativeManagedConfigPlan {
        let capture_policy = capture_policy();
        NativeManagedConfigPlan::from_bootstrap(BootstrapApplyInputs {
            verified_apply: apply,
            capture_policy: &capture_policy,
            persist_policy: NativeBootstrapPersistPolicy::defaults_v1(),
        })
        .unwrap()
    }

    fn scalar<'a>(section: &'a NativeManagedUciSectionPlan, option: &str) -> &'a str {
        section
            .scalar_value(option)
            .unwrap_or_else(|| panic!("missing scalar option {option}"))
    }

    #[test]
    fn both_projection_has_exact_sections_options_and_golden_digest() {
        let request = request(
            AutotuneProfile::BestOverall,
            "pppoe-wan",
            OperationRouteMode::Main,
            None,
            None,
        );
        let proposal = proposal(AutotuneProfile::BestOverall, None, None);
        let apply = apply_plan(&request, &proposal, Topology::Both);
        let capture_policy = capture_policy();
        let plan = build(&apply);

        assert_eq!(plan.cake().package(), NativeManagedPackage::CakeAutorate);
        assert_eq!(plan.cake().section(), "wan_sqm");
        assert_eq!(plan.cake().section_type(), "cake_autorate");
        assert_eq!(plan.sqm().unwrap().package(), NativeManagedPackage::Sqm);
        assert_eq!(plan.sqm().unwrap().section(), "cake_wan_sqm");
        assert_eq!(plan.sqm().unwrap().section_type(), "queue");
        assert_eq!(scalar(plan.cake(), "wan_if"), "pppoe-wan");
        assert_eq!(scalar(plan.cake(), "dl_if"), "ifb4pppoe-wan");
        assert_eq!(scalar(plan.cake(), "route_mode"), "main");
        assert!(!plan
            .cake()
            .options()
            .contains_key("traffic_profile_migrated"));
        assert!(!plan.cake().options().contains_key("mwan3_member"));
        assert_eq!(scalar(plan.cake(), "speedtest_backend"), "speedtest-go");
        assert!(!plan.cake().options().contains_key("speedtest_go_server_id"));
        assert_eq!(scalar(plan.cake(), "speedtest_apply_percent"), "90");
        assert_eq!(
            scalar(plan.cake(), "sqm_download"),
            proposal.download.base_kbps.to_string()
        );
        assert_eq!(
            scalar(plan.cake(), "sqm_upload"),
            proposal.upload.base_kbps.to_string()
        );
        assert_eq!(
            scalar(plan.sqm().unwrap(), "download"),
            proposal.download.base_kbps.to_string()
        );
        assert_eq!(
            scalar(plan.sqm().unwrap(), "upload"),
            proposal.upload.base_kbps.to_string()
        );
        assert_eq!(
            scalar(plan.sqm().unwrap(), "_cake_autorate_managed"),
            "wan_sqm"
        );
        assert_eq!(scalar(plan.sqm().unwrap(), "interface"), "pppoe-wan");
        assert_eq!(
            scalar(plan.cake(), "sqm_qdisc"),
            scalar(plan.sqm().unwrap(), "qdisc")
        );
        assert_eq!(
            scalar(plan.cake(), "sqm_script"),
            scalar(plan.sqm().unwrap(), "script")
        );
        assert_eq!(
            plan.cake().list_value("reflector").unwrap(),
            capture_policy.reflectors()
        );
        for omitted in [
            "sqm_ilimit",
            "sqm_elimit",
            "sqm_itarget",
            "sqm_etarget",
            "ping_prefix_string",
        ] {
            assert!(!plan.cake().options().contains_key(omitted));
        }
        for omitted in ["ilimit", "elimit", "itarget", "etarget"] {
            assert!(!plan.sqm().unwrap().options().contains_key(omitted));
        }
        assert_eq!(plan.action_count(), 124);
        assert!(plan.action_count() > 96);
        assert!(plan.action_count() <= MAX_NATIVE_BOOTSTRAP_CONFIG_ACTIONS);
        assert_eq!(
            plan.canonical_sha256().unwrap(),
            "78532dec5274f18e4c3380f18855d066585b0bfde7340e117c3ecf6fd826a0fa"
        );
    }

    #[test]
    fn existing_apply_v4_manifest_bytes_are_frozen_by_length_and_digest() {
        let request = request(
            AutotuneProfile::BestOverall,
            "pppoe-wan",
            OperationRouteMode::Main,
            None,
            None,
        );
        let proposal = proposal(AutotuneProfile::BestOverall, None, None);
        let apply = apply_plan(&request, &proposal, Topology::Both);
        let bytes = apply.canonical_manifest_bytes().unwrap();

        assert!(bytes.starts_with(b"{\"native_apply_manifest_schema_version\":4,"));
        assert_eq!(bytes.len(), 11_171);
        assert_eq!(
            hex_lower(digest(&SHA256, &bytes).as_ref()),
            "fa6d15cd64adbca95b6be077b5afd219b88cd7d391c9755dc42c865e45d5d6e6"
        );
    }

    #[test]
    fn one_sided_matrix_keeps_dormant_controller_metadata_and_zeros_only_queue() {
        let request = request(
            AutotuneProfile::BestOverall,
            "pppoe-wan",
            OperationRouteMode::Main,
            None,
            None,
        );
        let proposal = proposal(AutotuneProfile::BestOverall, None, None);
        let dl_apply = apply_plan(&request, &proposal, Topology::DownloadOnly);
        let dl = build(&dl_apply);
        assert_eq!(scalar(dl.cake(), "sqm_direction_mode"), "download_only");
        assert_eq!(scalar(dl.cake(), "adjust_dl_shaper_rate"), "1");
        assert_eq!(scalar(dl.cake(), "adjust_ul_shaper_rate"), "0");
        assert_eq!(
            scalar(dl.sqm().unwrap(), "download"),
            proposal.download.base_kbps.to_string()
        );
        assert_eq!(scalar(dl.sqm().unwrap(), "upload"), "0");
        assert_eq!(
            scalar(dl.cake(), "sqm_upload"),
            proposal.upload.base_kbps.to_string()
        );
        assert_eq!(
            scalar(dl.cake(), "min_ul_shaper_rate_kbps"),
            proposal.upload.minimum_kbps.to_string()
        );
        assert_eq!(scalar(dl.cake(), "adaptive_ceiling_ul_safe_kbps"), "0");
        assert_eq!(
            scalar(dl.cake(), "adaptive_ceiling_ul_evidence"),
            "legacy_unverified"
        );
        assert_eq!(
            scalar(dl.cake(), "adaptive_ceiling_dl_evidence"),
            "shaped_validation"
        );

        let ul_apply = apply_plan(&request, &proposal, Topology::UploadOnly);
        let ul = build(&ul_apply);
        assert_eq!(scalar(ul.cake(), "sqm_direction_mode"), "upload_only");
        assert_eq!(scalar(ul.cake(), "adjust_dl_shaper_rate"), "0");
        assert_eq!(scalar(ul.cake(), "adjust_ul_shaper_rate"), "1");
        assert_eq!(scalar(ul.sqm().unwrap(), "download"), "0");
        assert_eq!(
            scalar(ul.sqm().unwrap(), "upload"),
            proposal.upload.base_kbps.to_string()
        );
        assert_eq!(
            scalar(ul.cake(), "sqm_download"),
            proposal.download.base_kbps.to_string()
        );
        assert_eq!(scalar(ul.cake(), "adaptive_ceiling_dl_safe_kbps"), "0");
        assert_eq!(
            scalar(ul.cake(), "adaptive_ceiling_dl_evidence"),
            "legacy_unverified"
        );
        assert_eq!(
            scalar(ul.cake(), "adaptive_ceiling_ul_evidence"),
            "shaped_validation"
        );
    }

    #[test]
    fn raw_fallback_bootstrap_is_disabled_rate_free_and_tamper_evident() {
        let request = request(
            AutotuneProfile::BestOverall,
            "pppoe-wan",
            OperationRouteMode::Main,
            None,
            None,
        );
        let proposal = proposal(AutotuneProfile::BestOverall, None, None);
        let capture_policy = capture_policy();
        let off = apply_plan(&request, &proposal, Topology::Off);
        let disabled = NativeManagedConfigPlan::from_bootstrap(BootstrapApplyInputs {
            verified_apply: &off,
            capture_policy: &capture_policy,
            persist_policy: NativeBootstrapPersistPolicy::defaults_v1(),
        })
        .unwrap();
        assert_eq!(
            disabled.schema_version(),
            NATIVE_DISABLED_MANAGED_CONFIG_SCHEMA_VERSION
        );
        assert!(disabled.sqm().is_none());
        for (option, expected) in [
            ("enabled", "0"),
            ("manage_sqm", "1"),
            ("sqm_enabled", "0"),
            ("sqm_direction_mode", "off"),
            ("adjust_dl_shaper_rate", "0"),
            ("adjust_ul_shaper_rate", "0"),
            ("manual_rate_limits", "0"),
            ("adaptive_ceiling_enabled", "0"),
            ("scheduled_autotune_enabled", "0"),
        ] {
            assert_eq!(scalar(disabled.cake(), option), expected);
        }
        for forbidden in [
            "sqm_download",
            "sqm_upload",
            "min_dl_shaper_rate_kbps",
            "base_dl_shaper_rate_kbps",
            "max_dl_shaper_rate_kbps",
            "min_ul_shaper_rate_kbps",
            "base_ul_shaper_rate_kbps",
            "max_ul_shaper_rate_kbps",
            "adaptive_ceiling_dl_cap_kbps",
            "adaptive_ceiling_ul_cap_kbps",
            "throughput_reference_dl_p20_kbps",
            "throughput_reference_ul_p20_kbps",
        ] {
            assert!(!disabled.cake().options().contains_key(forbidden));
        }
        assert!(String::from_utf8(disabled.canonical_bytes().unwrap())
            .unwrap()
            .contains("\"sections\":[{\"package\":\"cake-autorate\""));

        let mut arbitrary_rate = apply_plan(&request, &proposal, Topology::Both);
        arbitrary_rate.download.base_kbps = Some(proposal.download.base_kbps - 100);
        assert!(
            NativeManagedConfigPlan::from_bootstrap(BootstrapApplyInputs {
                verified_apply: &arbitrary_rate,
                capture_policy: &capture_policy,
                persist_policy: NativeBootstrapPersistPolicy::defaults_v1(),
            })
            .unwrap_err()
            .contains("changed after exact construction")
        );

        let mut arbitrary_minimum = apply_plan(&request, &proposal, Topology::Both);
        arbitrary_minimum.download.measured_runtime_minimum_kbps =
            Some(proposal.download.exploration_minimum_kbps + 100);
        assert!(
            NativeManagedConfigPlan::from_bootstrap(BootstrapApplyInputs {
                verified_apply: &arbitrary_minimum,
                capture_policy: &capture_policy,
                persist_policy: NativeBootstrapPersistPolicy::defaults_v1(),
            })
            .unwrap_err()
            .contains("changed after exact construction")
        );
    }

    #[test]
    fn service_caps_remain_policy_while_measurements_and_bypass_evidence_stay_distinct() {
        let dl_cap = 750_000;
        let ul_cap = 75_000;
        let request = request(
            AutotuneProfile::BestOverall,
            "pppoe-wan",
            OperationRouteMode::Main,
            Some(dl_cap),
            Some(ul_cap),
        );
        let proposal = proposal(AutotuneProfile::BestOverall, Some(dl_cap), Some(ul_cap));
        let apply = apply_plan(&request, &proposal, Topology::DownloadOnly);
        let plan = build(&apply);

        assert_eq!(scalar(plan.cake(), "service_dl_cap_kbps"), "750000");
        assert_eq!(scalar(plan.cake(), "service_ul_cap_kbps"), "75000");
        assert_eq!(
            scalar(plan.cake(), "throughput_reference_dl_p20_kbps"),
            proposal.download.observed_low_kbps.to_string()
        );
        assert_eq!(
            scalar(plan.cake(), "throughput_reference_ul_p20_kbps"),
            proposal.upload.observed_low_kbps.to_string()
        );
        assert_ne!(proposal.download.observed_low_kbps, dl_cap);
        assert_ne!(proposal.upload.observed_low_kbps, ul_cap);
        assert_eq!(scalar(plan.cake(), "adaptive_ceiling_ul_safe_kbps"), "0");
        assert_eq!(scalar(plan.sqm().unwrap(), "upload"), "0");
        assert_eq!(
            scalar(plan.cake(), "sqm_upload"),
            proposal.upload.base_kbps.to_string()
        );
    }

    #[test]
    fn probe_plan_is_bounded_ordered_and_rejects_irtt() {
        let irtt = NativeBootstrapProbePlan::new(
            NativeBootstrapPingerMethod::Irtt,
            1,
            vec!["irtt.example".to_string()],
        )
        .unwrap_err();
        assert!(irtt.contains("does not support IRTT"));
        assert!(NativeBootstrapProbePlan::new(
            NativeBootstrapPingerMethod::Fping,
            2,
            vec!["1.1.1.1".to_string()],
        )
        .unwrap_err()
        .contains("active pinger count"));
        assert!(NativeBootstrapProbePlan::new(
            NativeBootstrapPingerMethod::Fping,
            1,
            vec!["1.1.1.1".to_string(), "1.1.1.1".to_string()],
        )
        .unwrap_err()
        .contains("duplicate"));
        assert!(NativeBootstrapProbePlan::new(
            NativeBootstrapPingerMethod::Fping,
            1,
            vec!["-unsafe".to_string()],
        )
        .unwrap_err()
        .contains("host or address"));
        assert!(NativeBootstrapProbePlan::new(
            NativeBootstrapPingerMethod::Fping,
            1,
            (0..=MAX_NATIVE_UCI_LIST_ITEMS)
                .map(|index| format!("reflector-{index}.example"))
                .collect(),
        )
        .unwrap_err()
        .contains("invalid item count"));
        assert!(NativeBootstrapProbePlan::new(
            NativeBootstrapPingerMethod::Fping,
            1,
            vec!["a".repeat(MAX_NATIVE_UCI_LIST_ITEM_BYTES + 1)],
        )
        .unwrap_err()
        .contains("safely batch-encodable"));
        assert!(NativeBootstrapProbePlan::new(
            NativeBootstrapPingerMethod::Fping,
            1,
            (0..17)
                .map(|index| format!("{}{}", "a".repeat(999), index))
                .collect(),
        )
        .unwrap_err()
        .contains("byte bound"));
        for method in [
            NativeBootstrapPingerMethod::Fping,
            NativeBootstrapPingerMethod::FpingTs,
            NativeBootstrapPingerMethod::Tsping,
            NativeBootstrapPingerMethod::Ping,
        ] {
            NativeBootstrapProbePlan::new(method, 1, vec!["1.1.1.1".to_string()]).unwrap();
        }
    }

    #[test]
    fn managed_uci_values_match_libuci_text_controls() {
        validate_uci_value("note", "tab\tseparated").unwrap();
        validate_uci_value("note", "delete\x7f").unwrap();
        for value in ["control\x01byte", "bell\x07byte", "escape\x1bbyte"] {
            assert!(validate_uci_value("note", value)
                .unwrap_err()
                .contains("safely batch-encodable"));
        }
        for value in ["line\nbreak", "carriage\rreturn", "nul\0byte", "quote'byte"] {
            assert!(validate_uci_value("note", value)
                .unwrap_err()
                .contains("safely batch-encodable"));
        }
    }

    #[test]
    fn route_profile_ifb_and_capture_policy_bindings_are_exact() {
        let route_request = request(
            AutotuneProfile::GamingExtreme,
            "pppoe-wan",
            OperationRouteMode::Mwan3,
            None,
            None,
        );
        let gaming_proposal = proposal(AutotuneProfile::GamingExtreme, None, None);
        let apply = apply_plan(&route_request, &gaming_proposal, Topology::Both);
        let capture_policy = capture_policy();
        let plan_a = build(&apply);
        let plan_b = build(&apply);
        assert_eq!(scalar(plan_a.cake(), "route_mode"), "mwan3");
        assert_eq!(scalar(plan_a.cake(), "mwan3_member"), "wanb");
        assert_eq!(scalar(plan_a.cake(), "autotune_profile"), "gaming");
        assert_eq!(
            plan_a.canonical_bytes().unwrap(),
            plan_b.canonical_bytes().unwrap()
        );
        assert_eq!(
            plan_a.canonical_sha256().unwrap(),
            plan_b.canonical_sha256().unwrap()
        );
        assert_eq!(
            plan_a.cake().list_value("reflector").unwrap(),
            capture_policy.reflectors()
        );
        assert_eq!(
            plan_a.canonical_bytes().unwrap(),
            plan_a.canonical_bytes().unwrap()
        );

        let long_request = request(
            AutotuneProfile::BestOverall,
            "abcdefghijkl",
            OperationRouteMode::Main,
            None,
            None,
        );
        let ordinary = proposal(AutotuneProfile::BestOverall, None, None);
        let long_apply = apply_plan(&long_request, &ordinary, Topology::Both);
        assert!(
            NativeManagedConfigPlan::from_bootstrap(BootstrapApplyInputs {
                verified_apply: &long_apply,
                capture_policy: &capture_policy,
                persist_policy: NativeBootstrapPersistPolicy::defaults_v1(),
            })
            .unwrap_err()
            .contains("kernel interface")
        );
    }

    #[test]
    fn request_policy_and_daemon_config_invariants_fail_closed() {
        let request = request(
            AutotuneProfile::BestOverall,
            "pppoe-wan",
            OperationRouteMode::Main,
            None,
            None,
        );
        let proposal = proposal(AutotuneProfile::BestOverall, None, None);
        let capture_policy = capture_policy();

        let mut active_proposal = proposal.clone();
        active_proposal.active_threshold_kbps = active_proposal.download.minimum_kbps + 100;
        let active_above_min = apply_plan(&request, &active_proposal, Topology::Both);
        assert!(
            NativeManagedConfigPlan::from_bootstrap(BootstrapApplyInputs {
                verified_apply: &active_above_min,
                capture_policy: &capture_policy,
                persist_policy: NativeBootstrapPersistPolicy::defaults_v1(),
            })
            .unwrap_err()
            .contains("active threshold")
        );

        let mut policy_proposal = proposal.clone();
        policy_proposal.adaptive_ceiling_enabled = true;
        let policy_mismatch = apply_plan(&request, &policy_proposal, Topology::Both);
        assert!(
            NativeManagedConfigPlan::from_bootstrap(BootstrapApplyInputs {
                verified_apply: &policy_mismatch,
                capture_policy: &capture_policy,
                persist_policy: NativeBootstrapPersistPolicy::defaults_v1(),
            })
            .unwrap_err()
            .contains("adaptive state")
        );

        let mut zero_cooldown_proposal = proposal.clone();
        zero_cooldown_proposal.adaptive_cooldown_s = 0;
        let zero_cooldown = apply_plan(&request, &zero_cooldown_proposal, Topology::Both);
        assert!(
            NativeManagedConfigPlan::from_bootstrap(BootstrapApplyInputs {
                verified_apply: &zero_cooldown,
                capture_policy: &capture_policy,
                persist_policy: NativeBootstrapPersistPolicy::defaults_v1(),
            })
            .unwrap_err()
            .contains("controller policy")
        );
    }

    #[test]
    fn logical_target_is_metadata_while_all_runtime_options_use_attested_l3() {
        let mut request = request(
            AutotuneProfile::BestOverall,
            "wan",
            OperationRouteMode::Main,
            None,
            None,
        );
        request.route.l3_device = "pppoe-wan".to_string();
        let proposal = proposal(AutotuneProfile::BestOverall, None, None);
        let apply = apply_plan(&request, &proposal, Topology::Both);
        let plan = build(&apply);

        for option in ["wan_if", "sqm_interface", "ul_if"] {
            assert_eq!(scalar(plan.cake(), option), "pppoe-wan");
        }
        assert_eq!(scalar(plan.cake(), "dl_if"), "ifb4pppoe-wan");
        assert_eq!(scalar(plan.cake(), "ping_extra_args"), "-I pppoe-wan");
        assert_eq!(scalar(plan.sqm().unwrap(), "interface"), "pppoe-wan");
        let canonical = String::from_utf8(plan.canonical_bytes().unwrap()).unwrap();
        assert!(canonical.contains("\"requested_target_interface\":\"wan\""));
        assert!(canonical.contains("\"managed_l3_interface\":\"pppoe-wan\""));
    }

    #[test]
    fn persistence_policy_is_typed_and_never_mislabels_requested_server_as_observed() {
        assert!(NativeBootstrapPersistPolicy::new(
            0,
            NativeBootstrapServerPersistence::LeaveAutomatic,
        )
        .is_err());
        assert!(NativeBootstrapPersistPolicy::new(
            101,
            NativeBootstrapServerPersistence::LeaveAutomatic,
        )
        .is_err());

        let request = request(
            AutotuneProfile::BestOverall,
            "pppoe-wan",
            OperationRouteMode::Main,
            None,
            None,
        );
        assert_eq!(request.speedtest_server_id, Some(17_372));
        let proposal = proposal(AutotuneProfile::BestOverall, None, None);
        let apply = apply_plan(&request, &proposal, Topology::Both);
        let capture_policy = capture_policy();
        let plan = NativeManagedConfigPlan::from_bootstrap(BootstrapApplyInputs {
            verified_apply: &apply,
            capture_policy: &capture_policy,
            persist_policy: NativeBootstrapPersistPolicy::new(
                42,
                NativeBootstrapServerPersistence::LeaveAutomatic,
            )
            .unwrap(),
        })
        .unwrap();

        assert_eq!(scalar(plan.cake(), "speedtest_apply_percent"), "42");
        assert!(!plan.cake().options().contains_key("speedtest_go_server_id"));
    }
}
