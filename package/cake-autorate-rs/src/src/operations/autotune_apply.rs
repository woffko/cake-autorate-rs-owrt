//! Canonical, private native Full Auto-Tune Apply plan.
//!
//! The plan is reconstructed from the immutable request and typed,
//! digest-verified native evidence; it is never assembled from LuCI or
//! public-result fields.  This module remains mutation-free, while the
//! transaction runtime consumes its exact canonical manifest after an explicit
//! confirmation of every evidence-derived acknowledgement.

use super::protocol::{OperationKind, OperationOrigin, OperationRequest};
use super::sqm_identity;
use crate::autotune::{AutotuneProposal, CapacityLearningPolicy, DirectionProposal, MAX_RATE_KBPS};
use ring::digest::{digest, SHA256};
use std::collections::BTreeSet;

pub(crate) const NATIVE_APPLY_MANIFEST_SCHEMA_VERSION: u8 = 4;
pub(crate) const NATIVE_RAW_FALLBACK_APPLY_MANIFEST_SCHEMA_VERSION: u8 = 5;
pub(crate) const MAX_NATIVE_APPLY_MANIFEST_BYTES: usize = 64 * 1024;
pub(crate) const MAX_NATIVE_APPLY_UCI_MUTATIONS: usize = 96;
pub(crate) const MAX_NATIVE_APPLY_ACKNOWLEDGEMENTS: usize = 24;
const NATIVE_APPLY_CONSTRUCTOR_SEAL_DOMAIN_V1: &str =
    "cake-autorate-native-apply-constructor-seal-v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum NativeApplyAcknowledgement {
    DownloadCandidateRealization,
    UploadCandidateRealization,
    DownloadCapacityRetention,
    UploadCapacityRetention,
    DownloadThroughputSafetyFloor,
    UploadThroughputSafetyFloor,
    DownloadPhysicalCapacityLimited,
    UploadPhysicalCapacityLimited,
    DownloadIcmpLatency,
    DownloadTransportLatency,
    UploadIcmpLatency,
    UploadTransportLatency,
    MeasurementContaminated,
    MeasurementConfidence,
    DownloadRawContaminated,
    DownloadRawConfidence,
    DownloadRawQualityTarget,
    UploadRawContaminated,
    UploadRawConfidence,
    UploadRawQualityTarget,
    DownloadShapingBypassed,
    UploadShapingBypassed,
    SqmDisabled,
    TopologyComparisonTrafficBudget,
}

impl NativeApplyAcknowledgement {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::DownloadCandidateRealization => "download-candidate-realization",
            Self::UploadCandidateRealization => "upload-candidate-realization",
            Self::DownloadCapacityRetention => "download-capacity-retention",
            Self::UploadCapacityRetention => "upload-capacity-retention",
            Self::DownloadThroughputSafetyFloor => "download-throughput-safety-floor",
            Self::UploadThroughputSafetyFloor => "upload-throughput-safety-floor",
            Self::DownloadPhysicalCapacityLimited => "download-physical-capacity-limited",
            Self::UploadPhysicalCapacityLimited => "upload-physical-capacity-limited",
            Self::DownloadIcmpLatency => "download-icmp-latency",
            Self::DownloadTransportLatency => "download-transport-latency",
            Self::UploadIcmpLatency => "upload-icmp-latency",
            Self::UploadTransportLatency => "upload-transport-latency",
            Self::MeasurementContaminated => "measurement-contaminated",
            Self::MeasurementConfidence => "measurement-confidence",
            Self::DownloadRawContaminated => "download-raw-contaminated",
            Self::DownloadRawConfidence => "download-raw-confidence",
            Self::DownloadRawQualityTarget => "download-raw-quality-target",
            Self::UploadRawContaminated => "upload-raw-contaminated",
            Self::UploadRawConfidence => "upload-raw-confidence",
            Self::UploadRawQualityTarget => "upload-raw-quality-target",
            Self::DownloadShapingBypassed => "download-shaping-bypassed",
            Self::UploadShapingBypassed => "upload-shaping-bypassed",
            Self::SqmDisabled => "sqm-disabled",
            Self::TopologyComparisonTrafficBudget => "topology-comparison-traffic-budget",
        }
    }

    pub(crate) fn from_public_code(value: &str) -> Option<Self> {
        Some(match value {
            "download-candidate-realization" => Self::DownloadCandidateRealization,
            "upload-candidate-realization" => Self::UploadCandidateRealization,
            "download-capacity-retention" => Self::DownloadCapacityRetention,
            "upload-capacity-retention" => Self::UploadCapacityRetention,
            "download-throughput-safety-floor" => Self::DownloadThroughputSafetyFloor,
            "upload-throughput-safety-floor" => Self::UploadThroughputSafetyFloor,
            "download-physical-capacity-limited" => Self::DownloadPhysicalCapacityLimited,
            "upload-physical-capacity-limited" => Self::UploadPhysicalCapacityLimited,
            "download-icmp-latency" => Self::DownloadIcmpLatency,
            "download-transport-latency" => Self::DownloadTransportLatency,
            "upload-icmp-latency" => Self::UploadIcmpLatency,
            "upload-transport-latency" => Self::UploadTransportLatency,
            "measurement-contaminated" => Self::MeasurementContaminated,
            "measurement-confidence" => Self::MeasurementConfidence,
            "download-raw-contaminated" => Self::DownloadRawContaminated,
            "download-raw-confidence" => Self::DownloadRawConfidence,
            "download-raw-quality-target" => Self::DownloadRawQualityTarget,
            "upload-raw-contaminated" => Self::UploadRawContaminated,
            "upload-raw-confidence" => Self::UploadRawConfidence,
            "upload-raw-quality-target" => Self::UploadRawQualityTarget,
            "download-shaping-bypassed" => Self::DownloadShapingBypassed,
            "upload-shaping-bypassed" => Self::UploadShapingBypassed,
            "sqm-disabled" => Self::SqmDisabled,
            "topology-comparison-traffic-budget" => Self::TopologyComparisonTrafficBudget,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeApplyDirectionMode {
    Shaped,
    Bypass,
}

impl NativeApplyDirectionMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Shaped => "shaped",
            Self::Bypass => "bypass",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeApplyAction {
    ApplySqm,
    DisableSqm,
}

impl NativeApplyAction {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ApplySqm => "apply_sqm",
            Self::DisableSqm => "disable_sqm",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeSqmDirectionMode {
    Both,
    DownloadOnly,
    UploadOnly,
    Off,
}

impl NativeSqmDirectionMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::DownloadOnly => "download_only",
            Self::UploadOnly => "upload_only",
            Self::Off => "off",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct NativeApplyDirectionInput {
    pub mode: NativeApplyDirectionMode,
    pub selected_kbps: Option<u64>,
    pub measured_runtime_minimum_kbps: Option<u64>,
    pub proposal: DirectionProposal,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct NativeApplyArtifactDigests<'a> {
    pub proposal: &'a str,
    pub download_search: &'a str,
    pub upload_search: &'a str,
    pub pair_confirmation: &'a str,
    pub topology_comparison: &'a str,
}

pub(crate) struct NativeApplyManifestInput<'a> {
    pub option_id: &'a str,
    pub request: &'a OperationRequest,
    pub worker_run_id: &'a str,
    pub review_digest: &'a str,
    pub coordinator_boot_id: &'a str,
    pub coordinator_generation: &'a str,
    pub selected_topology: &'a str,
    pub action: NativeApplyAction,
    pub sqm_direction_mode: NativeSqmDirectionMode,
    pub download: NativeApplyDirectionInput,
    pub upload: NativeApplyDirectionInput,
    pub proposal: &'a AutotuneProposal,
    pub auto_apply_evidence_pass: bool,
    pub manual_review_required: bool,
    pub required_acknowledgements: &'a [NativeApplyAcknowledgement],
    pub artifacts: NativeApplyArtifactDigests<'a>,
}

pub(crate) struct NativeRawFallbackApplyManifestInput<'a> {
    pub option_id: &'a str,
    pub request: &'a OperationRequest,
    pub worker_run_id: &'a str,
    pub review_digest: &'a str,
    pub coordinator_boot_id: &'a str,
    pub coordinator_generation: &'a str,
    pub proposal: &'a AutotuneProposal,
    pub required_acknowledgements: &'a [NativeApplyAcknowledgement],
    pub proposal_digest: &'a str,
    pub raw_fallback_digest: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CanonicalDirection {
    pub(crate) mode: NativeApplyDirectionMode,
    pub(crate) minimum_kbps: Option<u64>,
    pub(crate) measured_runtime_minimum_kbps: Option<u64>,
    pub(crate) base_kbps: Option<u64>,
    pub(crate) maximum_kbps: Option<u64>,
    pub(crate) tested_safe_maximum_kbps: Option<u64>,
    pub(crate) adaptive_cap_kbps: Option<u64>,
    pub(crate) service_hard_cap_kbps: Option<u64>,
    pub(crate) ceiling_evidence: Option<&'static str>,
    pub(crate) cap_source: Option<&'static str>,
}

impl CanonicalDirection {
    fn from_input(label: &str, input: NativeApplyDirectionInput) -> Result<Self, String> {
        match input.mode {
            NativeApplyDirectionMode::Bypass => {
                if input.selected_kbps.is_some() || input.measured_runtime_minimum_kbps.is_some() {
                    return Err(format!(
                        "native Apply bypassed {label} direction carries shaped rates"
                    ));
                }
                Ok(Self {
                    mode: input.mode,
                    minimum_kbps: None,
                    measured_runtime_minimum_kbps: None,
                    base_kbps: None,
                    maximum_kbps: None,
                    tested_safe_maximum_kbps: None,
                    adaptive_cap_kbps: None,
                    service_hard_cap_kbps: None,
                    ceiling_evidence: None,
                    cap_source: None,
                })
            }
            NativeApplyDirectionMode::Shaped => {
                let selected = input.selected_kbps.ok_or_else(|| {
                    format!("native Apply shaped {label} direction has no selected rate")
                })?;
                if !(100..=MAX_RATE_KBPS).contains(&selected) {
                    return Err(format!(
                        "native Apply selected {label} rate is outside the supported range"
                    ));
                }
                let minimum = input
                    .measured_runtime_minimum_kbps
                    .unwrap_or(input.proposal.exploration_minimum_kbps);
                if minimum < input.proposal.exploration_minimum_kbps
                    || minimum > selected
                    || selected > input.proposal.exploration_cap_kbps
                    || selected > input.proposal.absolute_cap_kbps
                    || input
                        .proposal
                        .service_hard_cap_kbps
                        .is_some_and(|cap| selected > cap)
                {
                    return Err(format!(
                        "native Apply {label} rates escape the verified proposal bounds"
                    ));
                }
                Ok(Self {
                    mode: input.mode,
                    minimum_kbps: Some(minimum),
                    measured_runtime_minimum_kbps: input.measured_runtime_minimum_kbps,
                    base_kbps: Some(selected),
                    maximum_kbps: Some(selected),
                    tested_safe_maximum_kbps: Some(selected),
                    adaptive_cap_kbps: Some(input.proposal.absolute_cap_kbps),
                    service_hard_cap_kbps: input.proposal.service_hard_cap_kbps,
                    ceiling_evidence: Some("shaped_validation"),
                    cap_source: Some(input.proposal.cap_source.as_str()),
                })
            }
        }
    }

    fn to_json(&self) -> String {
        format!(
            concat!(
                "{{\"mode\":{},\"minimum_kbps\":{},",
                "\"measured_runtime_minimum_kbps\":{},\"base_kbps\":{},",
                "\"maximum_kbps\":{},\"tested_safe_maximum_kbps\":{},",
                "\"adaptive_cap_kbps\":{},\"service_hard_cap_kbps\":{},",
                "\"ceiling_evidence\":{},\"cap_source\":{}}}"
            ),
            json_string(self.mode.as_str()),
            optional_u64(self.minimum_kbps),
            optional_u64(self.measured_runtime_minimum_kbps),
            optional_u64(self.base_kbps),
            optional_u64(self.maximum_kbps),
            optional_u64(self.tested_safe_maximum_kbps),
            optional_u64(self.adaptive_cap_kbps),
            optional_u64(self.service_hard_cap_kbps),
            optional_string(self.ceiling_evidence),
            optional_string(self.cap_source),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NativeApplyArtifactDigestsOwned {
    ShapedV4 {
        proposal: String,
        download_search: String,
        upload_search: String,
        pair_confirmation: String,
        topology_comparison: String,
    },
    RawFallbackV5 {
        proposal: String,
        raw_fallback: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NativeUciMutationAction {
    Set,
    Delete,
}

impl NativeUciMutationAction {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Delete => "delete",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeUciMutation {
    pub(crate) action: NativeUciMutationAction,
    pub(crate) package: &'static str,
    pub(crate) section: String,
    pub(crate) option: &'static str,
    pub(crate) value: Option<String>,
}

impl NativeUciMutation {
    fn set(section: &str, option: &'static str, value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        let mutation = Self {
            action: NativeUciMutationAction::Set,
            package: "cake-autorate",
            section: section.to_string(),
            option,
            value: Some(value),
        };
        mutation.validate()?;
        Ok(mutation)
    }

    fn delete(section: &str, option: &'static str) -> Result<Self, String> {
        let mutation = Self {
            action: NativeUciMutationAction::Delete,
            package: "cake-autorate",
            section: section.to_string(),
            option,
            value: None,
        };
        mutation.validate()?;
        Ok(mutation)
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        validate_uci_component("native Apply UCI package", self.package, 64)?;
        if !matches!(self.package, "cake-autorate" | "sqm") {
            return Err(format!(
                "native Apply UCI package {} is outside the transactional backup boundary",
                self.package
            ));
        }
        validate_uci_component("native Apply UCI section", &self.section, 64)?;
        match (&self.action, self.value.as_deref()) {
            (NativeUciMutationAction::Set, Some(value)) => {
                validate_uci_component("native Apply UCI option", self.option, 64)?;
                validate_uci_value(self.option, value)?;
            }
            (NativeUciMutationAction::Delete, None) => {
                validate_uci_component("native Apply UCI option", self.option, 64)?;
            }
            _ => return Err("native Apply UCI action/value pair is inconsistent".to_string()),
        }
        Ok(())
    }

    fn to_json(&self) -> String {
        format!(
            "{{\"action\":{},\"package\":{},\"section\":{},\"option\":{},\"value\":{}}}",
            json_string(self.action.as_str()),
            json_string(self.package),
            json_string(&self.section),
            json_string(self.option),
            optional_string(self.value.as_deref()),
        )
    }
}

/// Immutable, typed Apply authority reconstructed only from the private
/// request, Review, and digest-verified native artifacts.  The canonical
/// manifest and the transactional executor consume this same value;
/// neither path reparses public JSON or recalculates proposal rates.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NativeApplyExecutionPlan {
    pub(crate) option_id: String,
    pub(crate) request: OperationRequest,
    pub(crate) worker_run_id: String,
    pub(crate) review_digest: String,
    pub(crate) coordinator_boot_id: String,
    pub(crate) coordinator_generation: String,
    pub(crate) selected_topology: String,
    pub(crate) action: NativeApplyAction,
    pub(crate) sqm_direction_mode: NativeSqmDirectionMode,
    pub(crate) download: CanonicalDirection,
    pub(crate) upload: CanonicalDirection,
    pub(crate) proposal: AutotuneProposal,
    pub(crate) auto_apply_evidence_pass: bool,
    pub(crate) manual_review_required: bool,
    pub(crate) required_acknowledgements: Vec<NativeApplyAcknowledgement>,
    pub(crate) artifacts: NativeApplyArtifactDigestsOwned,
    pub(crate) uci_mutations: Vec<NativeUciMutation>,
    constructor_seal_sha256: String,
}

/// Frozen identity of the existing schema-v4 Apply artifact.
///
/// An absent-bootstrap manifest may bind this value as evidence-selection
/// authority, but it must never execute the schema-v4 mutation vector. The
/// request digest which distinguishes an absent target belongs to the outer
/// schema-v5 bootstrap envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeApplyV4Identity {
    pub(crate) schema_version: u8,
    pub(crate) job_id: String,
    pub(crate) worker_run_id: String,
    pub(crate) option_id: String,
    pub(crate) source_review_sha256: String,
    pub(crate) candidate_id: String,
    pub(crate) manifest_sha256: String,
}

/// Frozen identity of the manual raw-fallback schema-v5 Apply artifact.
///
/// This is deliberately a distinct type: a raw/no-SQM decision is not a
/// shaped schema-v4 candidate and must never be accepted by the existing
/// managed-instance executor through a version downgrade.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeApplyV5Identity {
    pub(crate) schema_version: u8,
    pub(crate) job_id: String,
    pub(crate) worker_run_id: String,
    pub(crate) option_id: String,
    pub(crate) source_review_sha256: String,
    pub(crate) candidate_id: String,
    pub(crate) manifest_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NativeApplyAuthorityIdentity {
    ShapedV4(NativeApplyV4Identity),
    RawFallbackV5(NativeApplyV5Identity),
}

impl NativeApplyAuthorityIdentity {
    pub(crate) fn schema_version(&self) -> u8 {
        match self {
            Self::ShapedV4(value) => value.schema_version,
            Self::RawFallbackV5(value) => value.schema_version,
        }
    }

    pub(crate) fn job_id(&self) -> &str {
        match self {
            Self::ShapedV4(value) => &value.job_id,
            Self::RawFallbackV5(value) => &value.job_id,
        }
    }

    pub(crate) fn worker_run_id(&self) -> &str {
        match self {
            Self::ShapedV4(value) => &value.worker_run_id,
            Self::RawFallbackV5(value) => &value.worker_run_id,
        }
    }

    pub(crate) fn option_id(&self) -> &str {
        match self {
            Self::ShapedV4(value) => &value.option_id,
            Self::RawFallbackV5(value) => &value.option_id,
        }
    }

    pub(crate) fn source_review_sha256(&self) -> &str {
        match self {
            Self::ShapedV4(value) => &value.source_review_sha256,
            Self::RawFallbackV5(value) => &value.source_review_sha256,
        }
    }

    pub(crate) fn candidate_id(&self) -> &str {
        match self {
            Self::ShapedV4(value) => &value.candidate_id,
            Self::RawFallbackV5(value) => &value.candidate_id,
        }
    }

    pub(crate) fn manifest_sha256(&self) -> &str {
        match self {
            Self::ShapedV4(value) => &value.manifest_sha256,
            Self::RawFallbackV5(value) => &value.manifest_sha256,
        }
    }
}

impl NativeApplyExecutionPlan {
    pub(crate) fn manifest_schema_version(&self) -> u8 {
        match &self.artifacts {
            NativeApplyArtifactDigestsOwned::ShapedV4 { .. } => {
                NATIVE_APPLY_MANIFEST_SCHEMA_VERSION
            }
            NativeApplyArtifactDigestsOwned::RawFallbackV5 { .. } => {
                NATIVE_RAW_FALLBACK_APPLY_MANIFEST_SCHEMA_VERSION
            }
        }
    }

    /// A scheduled job may mutate configuration without an operator only at
    /// the narrowest already-proven boundary. Directional bypass and SQM-off
    /// plans always require explicit acknowledgements and remain Review-only.
    pub(crate) fn unattended_scheduler_eligible(&self) -> bool {
        self.request.origin == OperationOrigin::Scheduler
            && self.request.scheduled_auto_apply_requested
            && self.action == NativeApplyAction::ApplySqm
            && self.sqm_direction_mode == NativeSqmDirectionMode::Both
            && self.download.mode == NativeApplyDirectionMode::Shaped
            && self.upload.mode == NativeApplyDirectionMode::Shaped
            && self.auto_apply_evidence_pass
            && !self.manual_review_required
            && self.required_acknowledgements.is_empty()
    }

    pub(crate) fn from_verified_input(input: NativeApplyManifestInput<'_>) -> Result<Self, String> {
        input.request.validate()?;
        if input.request.identity.operation != OperationKind::FullAutotune {
            return Err("native Apply manifest requires a Full Auto-Tune request".to_string());
        }
        require_lower_hex("native Apply worker run id", input.worker_run_id, 32)?;
        require_lower_hex("native Apply Review digest", input.review_digest, 64)?;
        validate_native_apply_option_id(input.option_id)?;
        require_safe_identity("coordinator boot id", input.coordinator_boot_id)?;
        require_lower_hex("coordinator generation", input.coordinator_generation, 32)?;
        for (label, digest) in [
            ("proposal", input.artifacts.proposal),
            ("download search", input.artifacts.download_search),
            ("upload search", input.artifacts.upload_search),
            ("pair confirmation", input.artifacts.pair_confirmation),
            ("topology comparison", input.artifacts.topology_comparison),
        ] {
            require_lower_hex(&format!("native Apply {label} digest"), digest, 64)?;
        }
        let profile = input
            .request
            .profile
            .ok_or_else(|| "native Apply request has no profile".to_string())?;
        let access_medium = input
            .request
            .access_medium
            .ok_or_else(|| "native Apply request has no access medium".to_string())?;
        let access_source = input
            .request
            .access_source
            .ok_or_else(|| "native Apply request has no access source".to_string())?;
        let capacity_learning_policy = input
            .request
            .capacity_learning_policy
            .ok_or_else(|| "native Apply request has no capacity-learning policy".to_string())?;
        if input.request.strategy.is_none() {
            return Err("native Apply request has no strategy".to_string());
        }
        if input.request.managed_sqm_section.is_none() {
            return Err("native Apply request has no managed SQM section".to_string());
        }
        if input.proposal.profile != profile
            || input.proposal.access_medium != access_medium
            || input.proposal.access_source != access_source
            || input.proposal.access_confidence_percent
                != u64::from(input.request.access_confidence_percent)
            || input.proposal.capacity_learning_policy != capacity_learning_policy
            || input.proposal.download.service_hard_cap_kbps != input.request.service_dl_cap_kbps
            || input.proposal.upload.service_hard_cap_kbps != input.request.service_ul_cap_kbps
        {
            return Err("native Apply proposal contradicts immutable request policy".to_string());
        }

        validate_native_apply_topology(
            input.selected_topology,
            input.action,
            input.sqm_direction_mode,
            input.download.mode,
            input.upload.mode,
        )?;
        let download = CanonicalDirection::from_input("download", input.download)?;
        let upload = CanonicalDirection::from_input("upload", input.upload)?;
        let required_acknowledgements =
            canonical_acknowledgements(input.required_acknowledgements)?;
        if input.auto_apply_evidence_pass == input.manual_review_required
            || input.manual_review_required != !required_acknowledgements.is_empty()
        {
            return Err(
                "native Apply acknowledgement set contradicts its evidence eligibility".to_string(),
            );
        }
        let uci_mutations = native_apply_uci_mutations(
            input.request,
            input.action,
            input.sqm_direction_mode,
            &download,
            &upload,
            input.proposal,
        )?;
        let mut value = Self {
            option_id: input.option_id.to_string(),
            request: input.request.clone(),
            worker_run_id: input.worker_run_id.to_string(),
            review_digest: input.review_digest.to_string(),
            coordinator_boot_id: input.coordinator_boot_id.to_string(),
            coordinator_generation: input.coordinator_generation.to_string(),
            selected_topology: input.selected_topology.to_string(),
            action: input.action,
            sqm_direction_mode: input.sqm_direction_mode,
            download,
            upload,
            proposal: input.proposal.clone(),
            auto_apply_evidence_pass: input.auto_apply_evidence_pass,
            manual_review_required: input.manual_review_required,
            required_acknowledgements,
            artifacts: NativeApplyArtifactDigestsOwned::ShapedV4 {
                proposal: input.artifacts.proposal.to_string(),
                download_search: input.artifacts.download_search.to_string(),
                upload_search: input.artifacts.upload_search.to_string(),
                pair_confirmation: input.artifacts.pair_confirmation.to_string(),
                topology_comparison: input.artifacts.topology_comparison.to_string(),
            },
            uci_mutations,
            constructor_seal_sha256: String::new(),
        };
        value.constructor_seal_sha256 = value.current_constructor_seal_sha256()?;
        Ok(value)
    }

    pub(crate) fn from_verified_raw_fallback(
        input: NativeRawFallbackApplyManifestInput<'_>,
    ) -> Result<Self, String> {
        for (label, digest) in [
            ("proposal", input.proposal_digest),
            ("raw fallback", input.raw_fallback_digest),
        ] {
            require_lower_hex(&format!("native Apply {label} digest"), digest, 64)?;
        }
        let mut acknowledgements = input.required_acknowledgements.to_vec();
        acknowledgements.sort_unstable();
        acknowledgements.dedup();
        for required in [
            NativeApplyAcknowledgement::DownloadShapingBypassed,
            NativeApplyAcknowledgement::UploadShapingBypassed,
            NativeApplyAcknowledgement::SqmDisabled,
        ] {
            if !acknowledgements.contains(&required) {
                return Err(format!(
                    "native raw-fallback Apply lacks acknowledgement {}",
                    required.as_str()
                ));
            }
        }
        if acknowledgements != input.required_acknowledgements {
            return Err("native raw-fallback Apply acknowledgements are not canonical".to_string());
        }
        let placeholder = NativeApplyArtifactDigests {
            proposal: input.proposal_digest,
            download_search: input.raw_fallback_digest,
            upload_search: input.raw_fallback_digest,
            pair_confirmation: input.raw_fallback_digest,
            topology_comparison: input.raw_fallback_digest,
        };
        let mut value = Self::from_verified_input(NativeApplyManifestInput {
            option_id: input.option_id,
            request: input.request,
            worker_run_id: input.worker_run_id,
            review_digest: input.review_digest,
            coordinator_boot_id: input.coordinator_boot_id,
            coordinator_generation: input.coordinator_generation,
            selected_topology: "no_sqm",
            action: NativeApplyAction::DisableSqm,
            sqm_direction_mode: NativeSqmDirectionMode::Off,
            download: NativeApplyDirectionInput {
                mode: NativeApplyDirectionMode::Bypass,
                selected_kbps: None,
                measured_runtime_minimum_kbps: None,
                proposal: input.proposal.download,
            },
            upload: NativeApplyDirectionInput {
                mode: NativeApplyDirectionMode::Bypass,
                selected_kbps: None,
                measured_runtime_minimum_kbps: None,
                proposal: input.proposal.upload,
            },
            proposal: input.proposal,
            auto_apply_evidence_pass: false,
            manual_review_required: true,
            required_acknowledgements: input.required_acknowledgements,
            artifacts: placeholder,
        })?;
        value.artifacts = NativeApplyArtifactDigestsOwned::RawFallbackV5 {
            proposal: input.proposal_digest.to_string(),
            raw_fallback: input.raw_fallback_digest.to_string(),
        };
        value.constructor_seal_sha256 = value.current_constructor_seal_sha256()?;
        Ok(value)
    }

    /// Re-run the sole typed constructor over the currently held fields and
    /// require its complete projection to remain byte-for-byte equivalent at
    /// the type level. `NativeApplyExecutionPlan` is still crate-visible during
    /// the migration, so a caller must not be able to turn a once-verified plan
    /// into new authority by mutating a digest, acknowledgement, direction, or
    /// UCI action after construction.
    pub(crate) fn validate_exact_invariants(&self) -> Result<(), String> {
        require_lower_hex(
            "native Apply constructor seal",
            &self.constructor_seal_sha256,
            64,
        )?;
        if self.current_constructor_seal_sha256()? != self.constructor_seal_sha256 {
            return Err("native Apply execution plan changed after exact construction".to_string());
        }
        let reconstructed = match &self.artifacts {
            NativeApplyArtifactDigestsOwned::ShapedV4 {
                proposal,
                download_search,
                upload_search,
                pair_confirmation,
                topology_comparison,
            } => Self::from_verified_input(NativeApplyManifestInput {
                option_id: &self.option_id,
                request: &self.request,
                worker_run_id: &self.worker_run_id,
                review_digest: &self.review_digest,
                coordinator_boot_id: &self.coordinator_boot_id,
                coordinator_generation: &self.coordinator_generation,
                selected_topology: &self.selected_topology,
                action: self.action,
                sqm_direction_mode: self.sqm_direction_mode,
                download: NativeApplyDirectionInput {
                    mode: self.download.mode,
                    selected_kbps: self.download.base_kbps,
                    measured_runtime_minimum_kbps: self.download.measured_runtime_minimum_kbps,
                    proposal: self.proposal.download,
                },
                upload: NativeApplyDirectionInput {
                    mode: self.upload.mode,
                    selected_kbps: self.upload.base_kbps,
                    measured_runtime_minimum_kbps: self.upload.measured_runtime_minimum_kbps,
                    proposal: self.proposal.upload,
                },
                proposal: &self.proposal,
                auto_apply_evidence_pass: self.auto_apply_evidence_pass,
                manual_review_required: self.manual_review_required,
                required_acknowledgements: &self.required_acknowledgements,
                artifacts: NativeApplyArtifactDigests {
                    proposal,
                    download_search,
                    upload_search,
                    pair_confirmation,
                    topology_comparison,
                },
            })?,
            NativeApplyArtifactDigestsOwned::RawFallbackV5 {
                proposal,
                raw_fallback,
            } => Self::from_verified_raw_fallback(NativeRawFallbackApplyManifestInput {
                option_id: &self.option_id,
                request: &self.request,
                worker_run_id: &self.worker_run_id,
                review_digest: &self.review_digest,
                coordinator_boot_id: &self.coordinator_boot_id,
                coordinator_generation: &self.coordinator_generation,
                proposal: &self.proposal,
                required_acknowledgements: &self.required_acknowledgements,
                proposal_digest: proposal,
                raw_fallback_digest: raw_fallback,
            })?,
        };
        if reconstructed != *self {
            return Err(
                "native Apply execution plan differs from its exact constructor projection"
                    .to_string(),
            );
        }
        Ok(())
    }

    fn current_constructor_seal_sha256(&self) -> Result<String, String> {
        let request_sha256 = native_apply_sha256_hex(self.request.encode()?.as_bytes());
        let manifest_sha256 = native_apply_sha256_hex(&self.canonical_manifest_bytes()?);
        let seed = format!(
            "domain={NATIVE_APPLY_CONSTRUCTOR_SEAL_DOMAIN_V1}\nrequest_sha256={request_sha256}\nmanifest_sha256={manifest_sha256}\n"
        );
        Ok(native_apply_sha256_hex(seed.as_bytes()))
    }

    fn canonical_candidate_json(&self) -> String {
        canonical_candidate_json(
            &self.selected_topology,
            self.action,
            self.sqm_direction_mode,
            &self.download,
            &self.upload,
            &self.proposal,
            &self.uci_mutations,
        )
    }

    fn candidate_id_for_json(&self, candidate: &str) -> Result<String, String> {
        let required_acknowledgements = acknowledgement_json(&self.required_acknowledgements);
        let artifacts = match &self.artifacts {
            NativeApplyArtifactDigestsOwned::ShapedV4 {
                proposal,
                download_search,
                upload_search,
                pair_confirmation,
                topology_comparison,
            } => format!(
                concat!(
                    "{{\"proposal\":{},\"download_search\":{},\"upload_search\":{},",
                    "\"pair_confirmation\":{},\"topology_comparison\":{}}}"
                ),
                json_string(proposal),
                json_string(download_search),
                json_string(upload_search),
                json_string(pair_confirmation),
                json_string(topology_comparison),
            ),
            NativeApplyArtifactDigestsOwned::RawFallbackV5 {
                proposal,
                raw_fallback,
            } => format!(
                "{{\"proposal\":{},\"raw_fallback\":{}}}",
                json_string(proposal),
                json_string(raw_fallback),
            ),
        };
        let candidate_seed = format!(
            "{{\"option_id\":{},\"source_review_sha256\":{},\"artifacts\":{},\"required_acknowledgements\":{},\"candidate\":{}}}",
            json_string(&self.option_id),
            json_string(&self.review_digest),
            artifacts,
            required_acknowledgements,
            candidate,
        );
        sqm_identity::sha256sum(candidate_seed.as_bytes())
    }

    pub(crate) fn v4_candidate_id(&self) -> Result<String, String> {
        self.validate_exact_invariants()?;
        self.candidate_id_for_json(&self.canonical_candidate_json())
    }

    pub(crate) fn v4_identity(&self) -> Result<NativeApplyV4Identity, String> {
        if !matches!(
            self.artifacts,
            NativeApplyArtifactDigestsOwned::ShapedV4 { .. }
        ) {
            return Err("native raw-fallback Apply plan has no schema-v4 identity".to_string());
        }
        let candidate_id = self.v4_candidate_id()?;
        let manifest = self.canonical_manifest_bytes()?;
        Ok(NativeApplyV4Identity {
            schema_version: NATIVE_APPLY_MANIFEST_SCHEMA_VERSION,
            job_id: self.request.identity.job_id.clone(),
            worker_run_id: self.worker_run_id.clone(),
            option_id: self.option_id.clone(),
            source_review_sha256: self.review_digest.clone(),
            candidate_id,
            manifest_sha256: sqm_identity::sha256sum(&manifest)?,
        })
    }

    pub(crate) fn v5_identity(&self) -> Result<NativeApplyV5Identity, String> {
        if !matches!(
            self.artifacts,
            NativeApplyArtifactDigestsOwned::RawFallbackV5 { .. }
        ) {
            return Err("native shaped Apply plan has no schema-v5 identity".to_string());
        }
        let candidate_id = self.v4_candidate_id()?;
        let manifest = self.canonical_manifest_bytes()?;
        Ok(NativeApplyV5Identity {
            schema_version: NATIVE_RAW_FALLBACK_APPLY_MANIFEST_SCHEMA_VERSION,
            job_id: self.request.identity.job_id.clone(),
            worker_run_id: self.worker_run_id.clone(),
            option_id: self.option_id.clone(),
            source_review_sha256: self.review_digest.clone(),
            candidate_id,
            manifest_sha256: sqm_identity::sha256sum(&manifest)?,
        })
    }

    pub(crate) fn authority_identity(&self) -> Result<NativeApplyAuthorityIdentity, String> {
        match &self.artifacts {
            NativeApplyArtifactDigestsOwned::ShapedV4 { .. } => {
                Ok(NativeApplyAuthorityIdentity::ShapedV4(self.v4_identity()?))
            }
            NativeApplyArtifactDigestsOwned::RawFallbackV5 { .. } => Ok(
                NativeApplyAuthorityIdentity::RawFallbackV5(self.v5_identity()?),
            ),
        }
    }

    pub(crate) fn canonical_manifest_bytes(&self) -> Result<Vec<u8>, String> {
        if let NativeApplyArtifactDigestsOwned::RawFallbackV5 {
            proposal,
            raw_fallback,
        } = &self.artifacts
        {
            return self.canonical_raw_fallback_manifest_bytes(proposal, raw_fallback);
        }
        let NativeApplyArtifactDigestsOwned::ShapedV4 {
            proposal: proposal_digest,
            download_search,
            upload_search,
            pair_confirmation,
            topology_comparison,
        } = &self.artifacts
        else {
            unreachable!("raw fallback returned above")
        };
        let profile = self
            .request
            .profile
            .ok_or_else(|| "native Apply request has no profile".to_string())?;
        let strategy = self
            .request
            .strategy
            .ok_or_else(|| "native Apply request has no strategy".to_string())?;
        let access_medium = self
            .request
            .access_medium
            .ok_or_else(|| "native Apply request has no access medium".to_string())?;
        let access_source = self
            .request
            .access_source
            .ok_or_else(|| "native Apply request has no access source".to_string())?;
        let capacity_learning_policy = self
            .request
            .capacity_learning_policy
            .ok_or_else(|| "native Apply request has no capacity-learning policy".to_string())?;
        let managed_sqm_section = self
            .request
            .managed_sqm_section
            .as_deref()
            .ok_or_else(|| "native Apply request has no managed SQM section".to_string())?;
        let candidate = self.canonical_candidate_json();
        let required_acknowledgements = acknowledgement_json(&self.required_acknowledgements);
        let candidate_id = self.candidate_id_for_json(&candidate)?;
        let mwan3_member = self
            .request
            .route
            .mwan3_member
            .as_deref()
            .map_or_else(|| "null".to_string(), json_string);
        let source_ip = self.request.route.source_ip.map_or_else(
            || "null".to_string(),
            |value| json_string(&value.to_string()),
        );
        let server_id = self
            .request
            .speedtest_server_id
            .map_or_else(|| "null".to_string(), |value| value.to_string());
        let mut output = format!(
            concat!(
                "{{\"native_apply_manifest_schema_version\":{},",
                "\"state\":\"confirmation_ready\",\"apply_enabled\":true,",
                "\"auto_apply_enabled\":false,\"manual_apply_enabled\":true,",
                "\"option_id\":{},",
                "\"job_id\":{},\"worker_run_id\":{},\"source_review_sha256\":{},",
                "\"coordinator_boot_id\":{},\"coordinator_generation\":{},",
                "\"instance\":{},\"target_interface\":{},",
                "\"managed_sqm_section\":{},",
                "\"route_mode\":{},\"mwan3_member\":{},\"resolved_interface\":{},",
                "\"source_ip\":{},\"route_fingerprint\":{},",
                "\"config_fingerprint\":{},\"sqm_fingerprint\":{},",
                "\"profile\":{},\"calibration_strategy\":{},",
                "\"backend\":{},\"server_id\":{},\"allow_active_traffic\":{},",
                "\"allow_sqm_disable\":{},\"traffic_budget_bytes\":{},",
                "\"request_deadline_unix_ms\":{},",
                "\"access_medium\":{},\"access_source\":{},",
                "\"access_confidence_percent\":{},\"capacity_learning_policy\":{},",
                "\"service_dl_cap_kbps\":{},\"service_ul_cap_kbps\":{},",
                "\"auto_apply_evidence_pass\":{},\"manual_review_required\":{},",
                "\"required_acknowledgements\":{},",
                "\"proposal_rate_transform\":\"none\",",
                "\"artifacts\":{{\"proposal\":{},\"download_search\":{},",
                "\"upload_search\":{},\"pair_confirmation\":{},",
                "\"topology_comparison\":{}}},",
                "\"candidate_id\":{},\"candidate\":{}}}\n"
            ),
            NATIVE_APPLY_MANIFEST_SCHEMA_VERSION,
            json_string(&self.option_id),
            json_string(&self.request.identity.job_id),
            json_string(&self.worker_run_id),
            json_string(&self.review_digest),
            json_string(&self.coordinator_boot_id),
            json_string(&self.coordinator_generation),
            json_string(&self.request.identity.instance),
            json_string(&self.request.identity.target_interface),
            json_string(managed_sqm_section),
            json_string(self.request.route.mode.as_str()),
            mwan3_member,
            json_string(&self.request.route.l3_device),
            source_ip,
            json_string(&self.request.identity.route_fingerprint),
            json_string(&self.request.identity.config_fingerprint),
            json_string(&self.request.identity.sqm_fingerprint),
            json_string(profile.as_str()),
            json_string(strategy.as_str()),
            json_string(&self.request.backend),
            server_id,
            self.request.allow_active_traffic,
            self.request.allow_sqm_disable,
            self.request.traffic_budget_bytes,
            self.request.deadline_unix_ms,
            json_string(access_medium.as_str()),
            json_string(access_source.as_str()),
            self.request.access_confidence_percent,
            json_string(capacity_learning_policy.as_str()),
            optional_u64(self.request.service_dl_cap_kbps),
            optional_u64(self.request.service_ul_cap_kbps),
            self.auto_apply_evidence_pass,
            self.manual_review_required,
            required_acknowledgements,
            json_string(proposal_digest),
            json_string(download_search),
            json_string(upload_search),
            json_string(pair_confirmation),
            json_string(topology_comparison),
            json_string(&candidate_id),
            candidate,
        );
        if output.len() > MAX_NATIVE_APPLY_MANIFEST_BYTES {
            return Err("native Apply manifest exceeds its size bound".to_string());
        }
        if output.matches('\n').count() != 1 || !output.ends_with('\n') {
            return Err("native Apply manifest is not one canonical line".to_string());
        }
        Ok(std::mem::take(&mut output).into_bytes())
    }

    fn canonical_raw_fallback_manifest_bytes(
        &self,
        proposal_digest: &str,
        raw_fallback_digest: &str,
    ) -> Result<Vec<u8>, String> {
        if self.action != NativeApplyAction::DisableSqm
            || self.sqm_direction_mode != NativeSqmDirectionMode::Off
            || self.download.mode != NativeApplyDirectionMode::Bypass
            || self.upload.mode != NativeApplyDirectionMode::Bypass
            || self.auto_apply_evidence_pass
            || !self.manual_review_required
        {
            return Err("native raw-fallback Apply plan is not manual SQM-off".to_string());
        }
        let candidate = self.canonical_candidate_json();
        let candidate_id = self.candidate_id_for_json(&candidate)?;
        let required_acknowledgements = acknowledgement_json(&self.required_acknowledgements);
        let managed_sqm_section = self
            .request
            .managed_sqm_section
            .as_deref()
            .ok_or_else(|| "native Apply request has no managed SQM section".to_string())?;
        let mut output = format!(
            concat!(
                "{{\"native_apply_manifest_schema_version\":{},",
                "\"state\":\"confirmation_ready\",\"apply_enabled\":true,",
                "\"auto_apply_enabled\":false,\"manual_apply_enabled\":true,",
                "\"option_id\":{},\"job_id\":{},\"worker_run_id\":{},",
                "\"source_review_sha256\":{},\"coordinator_boot_id\":{},",
                "\"coordinator_generation\":{},\"instance\":{},",
                "\"target_interface\":{},\"managed_sqm_section\":{},",
                "\"route_fingerprint\":{},\"config_fingerprint\":{},",
                "\"sqm_fingerprint\":{},\"auto_apply_evidence_pass\":false,",
                "\"manual_review_required\":true,",
                "\"required_acknowledgements\":{},",
                "\"proposal_rate_transform\":\"none\",",
                "\"artifacts\":{{\"proposal\":{},\"raw_fallback\":{}}},",
                "\"candidate_id\":{},\"candidate\":{}}}\n"
            ),
            NATIVE_RAW_FALLBACK_APPLY_MANIFEST_SCHEMA_VERSION,
            json_string(&self.option_id),
            json_string(&self.request.identity.job_id),
            json_string(&self.worker_run_id),
            json_string(&self.review_digest),
            json_string(&self.coordinator_boot_id),
            json_string(&self.coordinator_generation),
            json_string(&self.request.identity.instance),
            json_string(&self.request.identity.target_interface),
            json_string(managed_sqm_section),
            json_string(&self.request.identity.route_fingerprint),
            json_string(&self.request.identity.config_fingerprint),
            json_string(&self.request.identity.sqm_fingerprint),
            required_acknowledgements,
            json_string(proposal_digest),
            json_string(raw_fallback_digest),
            json_string(&candidate_id),
            candidate,
        );
        if output.len() > MAX_NATIVE_APPLY_MANIFEST_BYTES {
            return Err("native raw-fallback Apply manifest exceeds its size bound".to_string());
        }
        if output.matches('\n').count() != 1 || !output.ends_with('\n') {
            return Err("native raw-fallback Apply manifest is not one canonical line".to_string());
        }
        Ok(std::mem::take(&mut output).into_bytes())
    }
}

fn canonical_acknowledgements(
    values: &[NativeApplyAcknowledgement],
) -> Result<Vec<NativeApplyAcknowledgement>, String> {
    if values.len() > MAX_NATIVE_APPLY_ACKNOWLEDGEMENTS {
        return Err("native Apply acknowledgement set exceeds its bound".to_string());
    }
    let unique = values.iter().copied().collect::<BTreeSet<_>>();
    if unique.len() != values.len() {
        return Err("native Apply acknowledgement set contains duplicates".to_string());
    }
    Ok(unique.into_iter().collect())
}

fn acknowledgement_json(values: &[NativeApplyAcknowledgement]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| json_string(value.as_str()))
            .collect::<Vec<_>>()
            .join(",")
    )
}

pub(crate) fn validate_native_apply_option_id(option_id: &str) -> Result<(), String> {
    if option_id.is_empty()
        || option_id.len() > 32
        || !option_id
            .bytes()
            .all(|value| value.is_ascii_lowercase() || value == b'_')
    {
        return Err("native Apply option ID is invalid".to_string());
    }
    Ok(())
}

pub(crate) fn canonical_native_apply_manifest_bytes(
    input: NativeApplyManifestInput<'_>,
) -> Result<Vec<u8>, String> {
    NativeApplyExecutionPlan::from_verified_input(input)?.canonical_manifest_bytes()
}

fn native_apply_uci_mutations(
    request: &OperationRequest,
    action: NativeApplyAction,
    direction_mode: NativeSqmDirectionMode,
    download: &CanonicalDirection,
    upload: &CanonicalDirection,
    proposal: &AutotuneProposal,
) -> Result<Vec<NativeUciMutation>, String> {
    let section = &request.identity.instance;
    let sqm_section = request
        .managed_sqm_section
        .as_deref()
        .ok_or_else(|| "native Apply request has no managed SQM section".to_string())?;
    let strategy = request
        .strategy
        .ok_or_else(|| "native Apply request has no strategy".to_string())?;
    let mut mutations = Vec::new();
    let mut set = |option: &'static str, value: String| -> Result<(), String> {
        mutations.push(NativeUciMutation::set(section, option, value)?);
        Ok(())
    };

    set(
        "enabled",
        if action == NativeApplyAction::DisableSqm {
            "0"
        } else {
            "1"
        }
        .to_string(),
    )?;
    set(
        "sqm_enabled",
        if action == NativeApplyAction::DisableSqm {
            "0"
        } else {
            "1"
        }
        .to_string(),
    )?;
    set("sqm_direction_mode", direction_mode.as_str().to_string())?;
    if action == NativeApplyAction::DisableSqm {
        validate_native_uci_mutations(&mutations)?;
        return Ok(mutations);
    }

    set(
        "adjust_dl_shaper_rate",
        if direction_mode == NativeSqmDirectionMode::UploadOnly {
            "0"
        } else {
            "1"
        }
        .to_string(),
    )?;
    set(
        "adjust_ul_shaper_rate",
        if direction_mode == NativeSqmDirectionMode::DownloadOnly {
            "0"
        } else {
            "1"
        }
        .to_string(),
    )?;
    set("manage_sqm", "1".to_string())?;
    set("sqm_section", sqm_section.to_string())?;
    set("manual_rate_limits", "1".to_string())?;
    set("autotune_profile", proposal.profile.as_str().to_string())?;
    set(
        "autotune_calibration_strategy",
        strategy.as_str().to_string(),
    )?;

    drop(set);
    append_directional_rate_mutations(&mut mutations, section, "download", download)?;
    append_directional_rate_mutations(&mut mutations, section, "upload", upload)?;
    let mut set = |option: &'static str, value: String| -> Result<(), String> {
        mutations.push(NativeUciMutation::set(section, option, value)?);
        Ok(())
    };

    set(
        "connection_active_thr_kbps",
        proposal.active_threshold_kbps.to_string(),
    )?;
    for option in [
        "dl_avg_owd_delta_max_adjust_up_thr_ms",
        "ul_avg_owd_delta_max_adjust_up_thr_ms",
    ] {
        set(option, proposal.adjust_up_threshold_ms.to_string())?;
    }
    for option in ["dl_owd_delta_delay_thr_ms", "ul_owd_delta_delay_thr_ms"] {
        set(option, proposal.delay_threshold_ms.to_string())?;
    }
    for option in [
        "dl_avg_owd_delta_max_adjust_down_thr_ms",
        "ul_avg_owd_delta_max_adjust_down_thr_ms",
    ] {
        set(option, proposal.adjust_down_threshold_ms.to_string())?;
    }

    set(
        "adaptive_ceiling_enabled",
        if proposal.adaptive_ceiling_enabled {
            "1"
        } else {
            "0"
        }
        .to_string(),
    )?;
    drop(set);
    append_directional_adaptive_mutations(&mut mutations, section, "download", download)?;
    append_directional_adaptive_mutations(&mut mutations, section, "upload", upload)?;
    let mut set = |option: &'static str, value: String| -> Result<(), String> {
        mutations.push(NativeUciMutation::set(section, option, value)?);
        Ok(())
    };
    set(
        "adaptive_ceiling_hold_time_s",
        proposal.adaptive_hold_s.to_string(),
    )?;
    set(
        "adaptive_ceiling_growth_percent",
        proposal.adaptive_growth_percent.to_string(),
    )?;
    set(
        "adaptive_ceiling_probe_duration_s",
        proposal.adaptive_probe_s.to_string(),
    )?;
    set(
        "adaptive_ceiling_cooldown_s",
        proposal.adaptive_cooldown_s.to_string(),
    )?;
    set(
        "adaptive_ceiling_failed_bound_ttl_s",
        proposal.adaptive_failed_bound_ttl_s.to_string(),
    )?;

    set(
        "access_medium_selection",
        if proposal.access_source.as_str() == "user_selected" {
            proposal.access_medium.as_str()
        } else {
            "auto"
        }
        .to_string(),
    )?;
    set("access_medium", proposal.access_medium.as_str().to_string())?;
    set(
        "access_medium_source",
        proposal.access_source.as_str().to_string(),
    )?;
    set(
        "access_medium_confidence_percent",
        proposal.access_confidence_percent.to_string(),
    )?;
    set(
        "capacity_learning_policy",
        proposal.capacity_learning_policy.as_str().to_string(),
    )?;
    let (runtime_learning_mode, scheduled_autotune_enabled) = match proposal
        .capacity_learning_policy
    {
        CapacityLearningPolicy::ScheduledActive => ("periodic_active", "1"),
        CapacityLearningPolicy::PassiveBounded => ("passive", "0"),
        CapacityLearningPolicy::VerifiedOnly | CapacityLearningPolicy::FixedCap => ("fixed", "0"),
    };
    set("runtime_learning_mode", runtime_learning_mode.to_string())?;
    set(
        "scheduled_autotune_enabled",
        scheduled_autotune_enabled.to_string(),
    )?;

    drop(set);
    append_optional_u64_mutation(
        &mut mutations,
        section,
        "service_dl_cap_kbps",
        request.service_dl_cap_kbps,
    )?;
    append_optional_u64_mutation(
        &mut mutations,
        section,
        "service_ul_cap_kbps",
        request.service_ul_cap_kbps,
    )?;
    let mut set = |option: &'static str, value: String| -> Result<(), String> {
        mutations.push(NativeUciMutation::set(section, option, value)?);
        Ok(())
    };
    set("transport_latency_enabled", "1".to_string())?;
    set("throughput_guard_enabled", "1".to_string())?;
    set(
        "throughput_guard_retention_percent",
        canonical_decimal(
            "capacity retention minimum",
            proposal
                .validation_thresholds
                .capacity_retention_min_percent,
        )?,
    )?;
    set(
        "quality_target_delay_ms",
        canonical_decimal(
            "transport latency target",
            proposal.validation_thresholds.transport_delta_max_ms,
        )?,
    )?;
    set(
        "throughput_reference_dl_p20_kbps",
        proposal.download.observed_low_kbps.to_string(),
    )?;
    set(
        "throughput_reference_dl_p50_kbps",
        proposal.download.observed_median_kbps.to_string(),
    )?;
    set(
        "throughput_reference_ul_p20_kbps",
        proposal.upload.observed_low_kbps.to_string(),
    )?;
    set(
        "throughput_reference_ul_p50_kbps",
        proposal.upload.observed_median_kbps.to_string(),
    )?;

    set("sqm_qdisc", proposal.sqm.qdisc.to_string())?;
    set("sqm_script", proposal.sqm.script.to_string())?;
    set("sqm_qdisc_advanced", "1".to_string())?;
    set("sqm_qdisc_really_really_advanced", "1".to_string())?;
    set(
        "sqm_squash_dscp",
        if proposal.sqm.squash_dscp { "1" } else { "0" }.to_string(),
    )?;
    set(
        "sqm_squash_ingress",
        if proposal.sqm.squash_ingress {
            "1"
        } else {
            "0"
        }
        .to_string(),
    )?;
    set("sqm_ingress_ecn", proposal.sqm.ingress_ecn.to_string())?;
    set("sqm_egress_ecn", proposal.sqm.egress_ecn.to_string())?;
    drop(set);
    append_optional_text_mutation(
        &mut mutations,
        section,
        "sqm_iqdisc_opts",
        proposal.sqm.iqdisc_opts,
    )?;
    append_optional_text_mutation(
        &mut mutations,
        section,
        "sqm_eqdisc_opts",
        proposal.sqm.eqdisc_opts,
    )?;
    mutations.push(NativeUciMutation::set(
        section,
        "sqm_linklayer",
        proposal.link_layer,
    )?);
    mutations.push(NativeUciMutation::set(
        section,
        "sqm_overhead",
        proposal.overhead.to_string(),
    )?);
    mutations.push(NativeUciMutation::set(
        section,
        "sqm_tcMPU",
        proposal.mpu.to_string(),
    )?);
    mutations.push(NativeUciMutation::set(
        section,
        "sqm_linklayer_advanced",
        if proposal.link_layer == "none" {
            "0"
        } else {
            "1"
        },
    )?);
    validate_native_uci_mutations(&mutations)?;
    Ok(mutations)
}

fn append_directional_rate_mutations(
    mutations: &mut Vec<NativeUciMutation>,
    section: &str,
    direction: &str,
    value: &CanonicalDirection,
) -> Result<(), String> {
    if value.mode == NativeApplyDirectionMode::Bypass {
        return Ok(());
    }
    let minimum = value
        .minimum_kbps
        .ok_or_else(|| format!("native Apply {direction} minimum is missing"))?;
    let base = value
        .base_kbps
        .ok_or_else(|| format!("native Apply {direction} base is missing"))?;
    let maximum = value
        .maximum_kbps
        .ok_or_else(|| format!("native Apply {direction} maximum is missing"))?;
    let options = match direction {
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
        _ => return Err("native Apply has an unsupported direction".to_string()),
    };
    mutations.push(NativeUciMutation::set(
        section,
        options.0,
        base.to_string(),
    )?);
    mutations.push(NativeUciMutation::set(
        section,
        options.1,
        minimum.to_string(),
    )?);
    mutations.push(NativeUciMutation::set(
        section,
        options.2,
        base.to_string(),
    )?);
    mutations.push(NativeUciMutation::set(
        section,
        options.3,
        maximum.to_string(),
    )?);
    Ok(())
}

fn append_directional_adaptive_mutations(
    mutations: &mut Vec<NativeUciMutation>,
    section: &str,
    direction: &str,
    value: &CanonicalDirection,
) -> Result<(), String> {
    if value.mode == NativeApplyDirectionMode::Bypass {
        return Ok(());
    }
    let (cap_option, safe_option, evidence_option, source_option) = match direction {
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
        _ => return Err("native Apply has an unsupported direction".to_string()),
    };
    mutations.push(NativeUciMutation::set(
        section,
        cap_option,
        value
            .adaptive_cap_kbps
            .ok_or_else(|| format!("native Apply {direction} adaptive cap is missing"))?
            .to_string(),
    )?);
    mutations.push(NativeUciMutation::set(
        section,
        safe_option,
        value
            .tested_safe_maximum_kbps
            .ok_or_else(|| format!("native Apply {direction} safe rate is missing"))?
            .to_string(),
    )?);
    mutations.push(NativeUciMutation::set(
        section,
        evidence_option,
        value
            .ceiling_evidence
            .ok_or_else(|| format!("native Apply {direction} ceiling evidence is missing"))?,
    )?);
    mutations.push(NativeUciMutation::set(
        section,
        source_option,
        value
            .cap_source
            .ok_or_else(|| format!("native Apply {direction} cap source is missing"))?,
    )?);
    Ok(())
}

fn append_optional_u64_mutation(
    mutations: &mut Vec<NativeUciMutation>,
    section: &str,
    option: &'static str,
    value: Option<u64>,
) -> Result<(), String> {
    mutations.push(match value {
        Some(value) => NativeUciMutation::set(section, option, value.to_string())?,
        None => NativeUciMutation::delete(section, option)?,
    });
    Ok(())
}

fn append_optional_text_mutation(
    mutations: &mut Vec<NativeUciMutation>,
    section: &str,
    option: &'static str,
    value: &str,
) -> Result<(), String> {
    mutations.push(if value.is_empty() {
        NativeUciMutation::delete(section, option)?
    } else {
        NativeUciMutation::set(section, option, value)?
    });
    Ok(())
}

pub(crate) fn validate_native_uci_mutations(mutations: &[NativeUciMutation]) -> Result<(), String> {
    if mutations.is_empty() || mutations.len() > MAX_NATIVE_APPLY_UCI_MUTATIONS {
        return Err("native Apply UCI mutation count is outside its bound".to_string());
    }
    let mut options = BTreeSet::new();
    for mutation in mutations {
        mutation.validate()?;
        let key = (mutation.package, mutation.section.as_str(), mutation.option);
        if !options.insert(key) {
            return Err(format!(
                "native Apply UCI plan mutates {}.{}.{} more than once",
                mutation.package, mutation.section, mutation.option
            ));
        }
    }
    Ok(())
}

fn validate_uci_component(label: &str, value: &str, maximum: usize) -> Result<(), String> {
    if value.is_empty()
        || value.len() > maximum
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
    {
        return Err(format!("{label} is not a bounded safe identifier"));
    }
    Ok(())
}

fn validate_uci_value(option: &str, value: &str) -> Result<(), String> {
    if value.len() > 1024 || value.contains(['\n', '\r', '\0', '\'']) {
        return Err(format!(
            "native Apply UCI value for {option} is not safely batch-encodable"
        ));
    }
    Ok(())
}

fn canonical_decimal(label: &str, value: f64) -> Result<String, String> {
    if !value.is_finite() || !(0.0..=10_000.0).contains(&value) {
        return Err(format!("native Apply {label} is outside its numeric bound"));
    }
    Ok(format!("{value:.1}"))
}

fn canonical_candidate_json(
    selected_topology: &str,
    action: NativeApplyAction,
    direction_mode: NativeSqmDirectionMode,
    download: &CanonicalDirection,
    upload: &CanonicalDirection,
    proposal: &AutotuneProposal,
    uci_mutations: &[NativeUciMutation],
) -> String {
    let uci_mutations = uci_mutations
        .iter()
        .map(NativeUciMutation::to_json)
        .collect::<Vec<_>>()
        .join(",");
    format!(
        concat!(
            "{{\"action\":{},\"sqm_direction_mode\":{},\"selected_topology\":{},",
            "\"download\":{},\"upload\":{},",
            "\"controller\":{{\"active_threshold_kbps\":{},",
            "\"adjust_up_threshold_ms\":{},\"delay_threshold_ms\":{},",
            "\"adjust_down_threshold_ms\":{}}},",
            "\"adaptive_ceiling\":{{\"enabled\":{},\"hold_s\":{},",
            "\"growth_percent\":{},\"probe_s\":{},\"cooldown_s\":{},",
            "\"failed_bound_ttl_s\":{}}},",
            "\"sqm\":{{\"qdisc\":{},\"script\":{},\"classification\":{},",
            "\"squash_dscp\":{},\"squash_ingress\":{},",
            "\"ingress_ecn\":{},\"egress_ecn\":{},",
            "\"iqdisc_opts\":{},\"eqdisc_opts\":{}}},",
            "\"link\":{{\"kind\":{},\"layer\":{},\"overhead\":{},\"mpu\":{}}},",
            "\"uci_mutations\":[{}]}}"
        ),
        json_string(action.as_str()),
        json_string(direction_mode.as_str()),
        json_string(selected_topology),
        download.to_json(),
        upload.to_json(),
        proposal.active_threshold_kbps,
        proposal.adjust_up_threshold_ms,
        proposal.delay_threshold_ms,
        proposal.adjust_down_threshold_ms,
        proposal.adaptive_ceiling_enabled,
        proposal.adaptive_hold_s,
        proposal.adaptive_growth_percent,
        proposal.adaptive_probe_s,
        proposal.adaptive_cooldown_s,
        proposal.adaptive_failed_bound_ttl_s,
        json_string(proposal.sqm.qdisc),
        json_string(proposal.sqm.script),
        json_string(proposal.sqm.classification),
        proposal.sqm.squash_dscp,
        proposal.sqm.squash_ingress,
        json_string(proposal.sqm.ingress_ecn),
        json_string(proposal.sqm.egress_ecn),
        json_string(proposal.sqm.iqdisc_opts),
        json_string(proposal.sqm.eqdisc_opts),
        json_string(proposal.link_kind.as_str()),
        json_string(proposal.link_layer),
        proposal.overhead,
        proposal.mpu,
        uci_mutations,
    )
}

pub(crate) fn validate_native_apply_topology(
    topology: &str,
    action: NativeApplyAction,
    direction_mode: NativeSqmDirectionMode,
    download: NativeApplyDirectionMode,
    upload: NativeApplyDirectionMode,
) -> Result<(), String> {
    let expected = match topology {
        "both_shaped" => (
            NativeApplyAction::ApplySqm,
            NativeSqmDirectionMode::Both,
            NativeApplyDirectionMode::Shaped,
            NativeApplyDirectionMode::Shaped,
        ),
        "download_only_shaped" => (
            NativeApplyAction::ApplySqm,
            NativeSqmDirectionMode::DownloadOnly,
            NativeApplyDirectionMode::Shaped,
            NativeApplyDirectionMode::Bypass,
        ),
        "upload_only_shaped" => (
            NativeApplyAction::ApplySqm,
            NativeSqmDirectionMode::UploadOnly,
            NativeApplyDirectionMode::Bypass,
            NativeApplyDirectionMode::Shaped,
        ),
        "no_sqm" => (
            NativeApplyAction::DisableSqm,
            NativeSqmDirectionMode::Off,
            NativeApplyDirectionMode::Bypass,
            NativeApplyDirectionMode::Bypass,
        ),
        _ => return Err("native Apply selected topology is unsupported".to_string()),
    };
    if expected != (action, direction_mode, download, upload) {
        return Err(
            "native Apply topology contradicts its exact action or direction mode".to_string(),
        );
    }
    Ok(())
}

fn optional_u64(value: Option<u64>) -> String {
    value.map_or_else(|| "null".to_string(), |value| value.to_string())
}

fn optional_string(value: Option<&str>) -> String {
    value.map_or_else(|| "null".to_string(), json_string)
}

fn json_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            value if value.is_control() => output.push_str(&format!("\\u{:04x}", value as u32)),
            value => output.push(value),
        }
    }
    output.push('"');
    output
}

fn require_lower_hex(label: &str, value: &str, length: usize) -> Result<(), String> {
    if value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(format!(
            "{label} must be exactly {length} lowercase hex characters"
        ))
    }
}

fn require_safe_identity(label: &str, value: &str) -> Result<(), String> {
    if !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
    {
        Ok(())
    } else {
        Err(format!("{label} is not a bounded safe identifier"))
    }
}

fn native_apply_sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let hash = digest(&SHA256, bytes);
    let mut output = String::with_capacity(64);
    for byte in hash.as_ref() {
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
        AutotuneProfile, CapacityLearningPolicy, LatencyBaseline, LinkKind, ProposalContext,
    };
    use crate::operations::protocol::{
        CalibrationStrategy, OperationIdentity, OperationOrigin, OperationRouteIdentity,
        OperationRouteMode, OperationTargetState,
    };
    use std::net::{IpAddr, Ipv4Addr};

    const REVIEW_DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const PROPOSAL_DIGEST: &str =
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const DOWNLOAD_DIGEST: &str =
        "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const UPLOAD_DIGEST: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    const PAIR_DIGEST: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
    const TOPOLOGY_DIGEST: &str =
        "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

    fn request() -> OperationRequest {
        OperationRequest {
            identity: OperationIdentity {
                job_id: "11".repeat(16),
                job_token: "22".repeat(32),
                instance: "wan_sqm".to_string(),
                operation: OperationKind::FullAutotune,
                target_interface: "pppoe-wan".to_string(),
                route_fingerprint: "33".repeat(32),
                config_fingerprint: "44".repeat(32),
                sqm_fingerprint: "55".repeat(32),
            },
            created_unix_ms: 1,
            deadline_unix_ms: 600_001,
            origin: OperationOrigin::Luci,
            backend: "speedtest-go".to_string(),
            speedtest_direction: None,
            speedtest_server_id: Some(17_372),
            speedtest_topology: None,
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
            managed_sqm_section: Some("wan_sqm".to_string()),
            profile: Some(AutotuneProfile::BestOverall),
            strategy: Some(CalibrationStrategy::FullRaw),
            access_medium: Some(AccessMedium::SharedWired),
            access_source: Some(AccessEvidenceSource::UserSelected),
            access_confidence_percent: 100,
            capacity_learning_policy: Some(CapacityLearningPolicy::VerifiedOnly),
            service_dl_cap_kbps: None,
            service_ul_cap_kbps: None,
            allow_sqm_disable: true,
            allow_active_traffic: false,
            scheduled_auto_apply_requested: false,
            traffic_budget_bytes: 1_000_000_000,
        }
    }

    fn proposal() -> AutotuneProposal {
        build_proposal_for_profile_with_context(
            &[100_000.0, 101_000.0],
            &[50_000.0, 51_000.0],
            LatencyBaseline {
                median_ms: 5.0,
                p95_ms: 8.0,
                samples: 20,
            },
            LinkKind::Pppoe,
            AutotuneProfile::BestOverall,
            ProposalContext {
                access_medium: Some(AccessMedium::SharedWired),
                access_source: AccessEvidenceSource::UserSelected,
                access_confidence_percent: 100,
                capacity_learning_policy: Some(CapacityLearningPolicy::VerifiedOnly),
                download_service_cap_kbps: None,
                upload_service_cap_kbps: None,
            },
        )
        .unwrap()
    }

    fn direction(
        mode: NativeApplyDirectionMode,
        proposal: DirectionProposal,
    ) -> NativeApplyDirectionInput {
        NativeApplyDirectionInput {
            mode,
            selected_kbps: (mode == NativeApplyDirectionMode::Shaped).then_some(proposal.base_kbps),
            measured_runtime_minimum_kbps: None,
            proposal,
        }
    }

    fn execution_plan(
        request: &OperationRequest,
        proposal: &AutotuneProposal,
        selected_topology: &str,
        action: NativeApplyAction,
        sqm_direction_mode: NativeSqmDirectionMode,
        download_mode: NativeApplyDirectionMode,
        upload_mode: NativeApplyDirectionMode,
        review_digest: &str,
    ) -> Result<NativeApplyExecutionPlan, String> {
        NativeApplyExecutionPlan::from_verified_input(NativeApplyManifestInput {
            option_id: "recommended",
            request,
            worker_run_id: &"66".repeat(16),
            review_digest,
            coordinator_boot_id: "boot-id",
            coordinator_generation: &"77".repeat(16),
            selected_topology,
            action,
            sqm_direction_mode,
            download: direction(download_mode, proposal.download),
            upload: direction(upload_mode, proposal.upload),
            proposal,
            auto_apply_evidence_pass: false,
            manual_review_required: true,
            required_acknowledgements: &[NativeApplyAcknowledgement::MeasurementConfidence],
            artifacts: NativeApplyArtifactDigests {
                proposal: PROPOSAL_DIGEST,
                download_search: DOWNLOAD_DIGEST,
                upload_search: UPLOAD_DIGEST,
                pair_confirmation: PAIR_DIGEST,
                topology_comparison: TOPOLOGY_DIGEST,
            },
        })
    }

    fn manifest(
        request: &OperationRequest,
        proposal: &AutotuneProposal,
        selected_topology: &str,
        action: NativeApplyAction,
        sqm_direction_mode: NativeSqmDirectionMode,
        download_mode: NativeApplyDirectionMode,
        upload_mode: NativeApplyDirectionMode,
        review_digest: &str,
    ) -> Result<Vec<u8>, String> {
        execution_plan(
            request,
            proposal,
            selected_topology,
            action,
            sqm_direction_mode,
            download_mode,
            upload_mode,
            review_digest,
        )?
        .canonical_manifest_bytes()
    }

    fn mutation_value<'a>(plan: &'a NativeApplyExecutionPlan, option: &str) -> Option<&'a str> {
        plan.uci_mutations
            .iter()
            .find(|mutation| mutation.option == option)
            .and_then(|mutation| mutation.value.as_deref())
    }

    #[test]
    fn raw_fallback_manifest_is_separate_manual_v5_authority() {
        let operation = request();
        let proposal = proposal();
        let acknowledgements = [
            NativeApplyAcknowledgement::DownloadShapingBypassed,
            NativeApplyAcknowledgement::UploadShapingBypassed,
            NativeApplyAcknowledgement::SqmDisabled,
        ];
        let plan = NativeApplyExecutionPlan::from_verified_raw_fallback(
            NativeRawFallbackApplyManifestInput {
                option_id: "no_sqm",
                request: &operation,
                worker_run_id: &"66".repeat(16),
                review_digest: REVIEW_DIGEST,
                coordinator_boot_id: "boot-id",
                coordinator_generation: &"77".repeat(16),
                proposal: &proposal,
                required_acknowledgements: &acknowledgements,
                proposal_digest: PROPOSAL_DIGEST,
                raw_fallback_digest: DOWNLOAD_DIGEST,
            },
        )
        .unwrap();
        plan.validate_exact_invariants().unwrap();
        assert!(!plan.unattended_scheduler_eligible());
        assert_eq!(plan.action, NativeApplyAction::DisableSqm);
        assert_eq!(plan.sqm_direction_mode, NativeSqmDirectionMode::Off);
        assert_eq!(plan.uci_mutations.len(), 3);
        let json = String::from_utf8(plan.canonical_manifest_bytes().unwrap()).unwrap();
        assert!(json.starts_with("{\"native_apply_manifest_schema_version\":5,"));
        assert!(json.contains("\"artifacts\":{\"proposal\":"));
        assert!(json.contains("\"raw_fallback\":"));
        assert!(!json.contains("download_search"));
        assert!(!json.contains("pair_confirmation"));
        assert!(plan.v4_identity().is_err());

        let missing_disable_ack = NativeApplyExecutionPlan::from_verified_raw_fallback(
            NativeRawFallbackApplyManifestInput {
                required_acknowledgements: &acknowledgements[..2],
                option_id: "no_sqm",
                request: &operation,
                worker_run_id: &"66".repeat(16),
                review_digest: REVIEW_DIGEST,
                coordinator_boot_id: "boot-id",
                coordinator_generation: &"77".repeat(16),
                proposal: &proposal,
                proposal_digest: PROPOSAL_DIGEST,
                raw_fallback_digest: DOWNLOAD_DIGEST,
            },
        );
        assert!(missing_disable_ack.is_err());
    }

    #[test]
    fn legacy_set_delete_json_bytes_are_frozen() {
        assert_eq!(
            NativeUciMutation::set("wan_sqm", "enabled", "1")
                .unwrap()
                .to_json(),
            "{\"action\":\"set\",\"package\":\"cake-autorate\",\"section\":\"wan_sqm\",\"option\":\"enabled\",\"value\":\"1\"}"
        );
        assert_eq!(
            NativeUciMutation::delete("wan_sqm", "service_dl_cap_kbps")
                .unwrap()
                .to_json(),
            "{\"action\":\"delete\",\"package\":\"cake-autorate\",\"section\":\"wan_sqm\",\"option\":\"service_dl_cap_kbps\",\"value\":null}"
        );
    }

    #[test]
    fn production_plans_still_emit_only_the_legacy_set_delete_contract() {
        let request = request();
        let proposal = proposal();
        let plan = execution_plan(
            &request,
            &proposal,
            "both_shaped",
            NativeApplyAction::ApplySqm,
            NativeSqmDirectionMode::Both,
            NativeApplyDirectionMode::Shaped,
            NativeApplyDirectionMode::Shaped,
            REVIEW_DIGEST,
        )
        .unwrap();
        assert!(plan.uci_mutations.iter().all(|mutation| matches!(
            &mutation.action,
            NativeUciMutationAction::Set | NativeUciMutationAction::Delete
        )));
        let manifest = String::from_utf8(plan.canonical_manifest_bytes().unwrap()).unwrap();
        assert!(!manifest.contains("\"add_section\""));
        assert!(!manifest.contains("\"section_type\""));
    }

    #[test]
    fn every_supported_topology_has_one_exact_action_and_direction_mapping() {
        let request = request();
        let proposal = proposal();
        for (topology, action, direction_mode, download, upload, expected) in [
            (
                "both_shaped",
                NativeApplyAction::ApplySqm,
                NativeSqmDirectionMode::Both,
                NativeApplyDirectionMode::Shaped,
                NativeApplyDirectionMode::Shaped,
                "\"action\":\"apply_sqm\",\"sqm_direction_mode\":\"both\"",
            ),
            (
                "download_only_shaped",
                NativeApplyAction::ApplySqm,
                NativeSqmDirectionMode::DownloadOnly,
                NativeApplyDirectionMode::Shaped,
                NativeApplyDirectionMode::Bypass,
                "\"action\":\"apply_sqm\",\"sqm_direction_mode\":\"download_only\"",
            ),
            (
                "upload_only_shaped",
                NativeApplyAction::ApplySqm,
                NativeSqmDirectionMode::UploadOnly,
                NativeApplyDirectionMode::Bypass,
                NativeApplyDirectionMode::Shaped,
                "\"action\":\"apply_sqm\",\"sqm_direction_mode\":\"upload_only\"",
            ),
            (
                "no_sqm",
                NativeApplyAction::DisableSqm,
                NativeSqmDirectionMode::Off,
                NativeApplyDirectionMode::Bypass,
                NativeApplyDirectionMode::Bypass,
                "\"action\":\"disable_sqm\",\"sqm_direction_mode\":\"off\"",
            ),
        ] {
            let bytes = manifest(
                &request,
                &proposal,
                topology,
                action,
                direction_mode,
                download,
                upload,
                REVIEW_DIGEST,
            )
            .unwrap();
            let text = String::from_utf8(bytes).unwrap();
            assert!(text.contains(expected));
            assert!(text.contains(&format!("\"selected_topology\":\"{topology}\"")));
            assert!(text.contains("\"state\":\"confirmation_ready\""));
            assert!(text.contains("\"apply_enabled\":true"));
            assert!(text.contains("\"auto_apply_enabled\":false"));
            assert!(text.contains("\"manual_apply_enabled\":true"));
            assert!(text.contains("\"required_acknowledgements\":[\"measurement-confidence\"]"));
            assert!(text.contains("\"managed_sqm_section\":\"wan_sqm\""));
            assert!(text.contains("\"native_apply_manifest_schema_version\":4"));
        }
    }

    #[test]
    fn typed_plan_is_the_single_exact_source_for_manifest_and_uci_rates() {
        let request = request();
        let proposal = proposal();
        let plan = execution_plan(
            &request,
            &proposal,
            "both_shaped",
            NativeApplyAction::ApplySqm,
            NativeSqmDirectionMode::Both,
            NativeApplyDirectionMode::Shaped,
            NativeApplyDirectionMode::Shaped,
            REVIEW_DIGEST,
        )
        .unwrap();
        let expected_dl = proposal.download.base_kbps.to_string();
        let expected_ul = proposal.upload.base_kbps.to_string();
        assert_eq!(
            mutation_value(&plan, "base_dl_shaper_rate_kbps"),
            Some(expected_dl.as_str())
        );
        assert_eq!(
            mutation_value(&plan, "sqm_download"),
            mutation_value(&plan, "base_dl_shaper_rate_kbps")
        );
        assert_eq!(
            mutation_value(&plan, "base_ul_shaper_rate_kbps"),
            Some(expected_ul.as_str())
        );
        assert_eq!(
            mutation_value(&plan, "sqm_upload"),
            mutation_value(&plan, "base_ul_shaper_rate_kbps")
        );
        let direct = canonical_native_apply_manifest_bytes(NativeApplyManifestInput {
            option_id: "recommended",
            request: &request,
            worker_run_id: &"66".repeat(16),
            review_digest: REVIEW_DIGEST,
            coordinator_boot_id: "boot-id",
            coordinator_generation: &"77".repeat(16),
            selected_topology: "both_shaped",
            action: NativeApplyAction::ApplySqm,
            sqm_direction_mode: NativeSqmDirectionMode::Both,
            download: direction(NativeApplyDirectionMode::Shaped, proposal.download),
            upload: direction(NativeApplyDirectionMode::Shaped, proposal.upload),
            proposal: &proposal,
            auto_apply_evidence_pass: false,
            manual_review_required: true,
            required_acknowledgements: &[NativeApplyAcknowledgement::MeasurementConfidence],
            artifacts: NativeApplyArtifactDigests {
                proposal: PROPOSAL_DIGEST,
                download_search: DOWNLOAD_DIGEST,
                upload_search: UPLOAD_DIGEST,
                pair_confirmation: PAIR_DIGEST,
                topology_comparison: TOPOLOGY_DIGEST,
            },
        })
        .unwrap();
        assert_eq!(plan.canonical_manifest_bytes().unwrap(), direct);
    }

    #[test]
    fn bypassed_direction_has_no_rate_or_adaptive_mutation_and_disable_is_minimal() {
        let request = request();
        let proposal = proposal();
        let download_only = execution_plan(
            &request,
            &proposal,
            "download_only_shaped",
            NativeApplyAction::ApplySqm,
            NativeSqmDirectionMode::DownloadOnly,
            NativeApplyDirectionMode::Shaped,
            NativeApplyDirectionMode::Bypass,
            REVIEW_DIGEST,
        )
        .unwrap();
        for option in [
            "sqm_upload",
            "min_ul_shaper_rate_kbps",
            "base_ul_shaper_rate_kbps",
            "max_ul_shaper_rate_kbps",
            "adaptive_ceiling_ul_cap_kbps",
            "adaptive_ceiling_ul_safe_kbps",
            "adaptive_ceiling_ul_evidence",
            "adaptive_ceiling_ul_cap_source",
        ] {
            assert!(
                download_only
                    .uci_mutations
                    .iter()
                    .all(|mutation| mutation.option != option),
                "bypassed upload unexpectedly mutates {option}"
            );
        }

        let disabled = execution_plan(
            &request,
            &proposal,
            "no_sqm",
            NativeApplyAction::DisableSqm,
            NativeSqmDirectionMode::Off,
            NativeApplyDirectionMode::Bypass,
            NativeApplyDirectionMode::Bypass,
            REVIEW_DIGEST,
        )
        .unwrap();
        assert_eq!(disabled.uci_mutations.len(), 3);
        assert_eq!(mutation_value(&disabled, "enabled"), Some("0"));
        assert_eq!(mutation_value(&disabled, "sqm_enabled"), Some("0"));
        assert_eq!(mutation_value(&disabled, "sqm_direction_mode"), Some("off"));
    }

    #[test]
    fn bypassed_directions_are_rate_free_and_require_explicit_confirmation() {
        let request = request();
        let proposal = proposal();
        let text = String::from_utf8(
            manifest(
                &request,
                &proposal,
                "download_only_shaped",
                NativeApplyAction::ApplySqm,
                NativeSqmDirectionMode::DownloadOnly,
                NativeApplyDirectionMode::Shaped,
                NativeApplyDirectionMode::Bypass,
                REVIEW_DIGEST,
            )
            .unwrap(),
        )
        .unwrap();
        assert!(text.contains("\"state\":\"confirmation_ready\""));
        assert!(text.contains("\"apply_enabled\":true"));
        assert!(text.contains("\"auto_apply_enabled\":false"));
        assert!(text.contains("\"manual_apply_enabled\":true"));
        assert!(text.contains("\"proposal_rate_transform\":\"none\""));
        assert!(text.contains(concat!(
            "\"upload\":{\"mode\":\"bypass\",\"minimum_kbps\":null,",
            "\"measured_runtime_minimum_kbps\":null,\"base_kbps\":null,",
            "\"maximum_kbps\":null,\"tested_safe_maximum_kbps\":null,",
            "\"adaptive_cap_kbps\":null,\"service_hard_cap_kbps\":null,",
            "\"ceiling_evidence\":null,\"cap_source\":null}"
        )));
    }

    #[test]
    fn contradictory_topology_and_bypass_rates_fail_closed() {
        let request = request();
        let proposal = proposal();
        assert!(manifest(
            &request,
            &proposal,
            "both_shaped",
            NativeApplyAction::ApplySqm,
            NativeSqmDirectionMode::DownloadOnly,
            NativeApplyDirectionMode::Shaped,
            NativeApplyDirectionMode::Bypass,
            REVIEW_DIGEST,
        )
        .unwrap_err()
        .contains("topology contradicts"));

        let mut upload = direction(NativeApplyDirectionMode::Bypass, proposal.upload);
        upload.selected_kbps = Some(proposal.upload.base_kbps);
        let error = canonical_native_apply_manifest_bytes(NativeApplyManifestInput {
            option_id: "recommended",
            request: &request,
            worker_run_id: &"66".repeat(16),
            review_digest: REVIEW_DIGEST,
            coordinator_boot_id: "boot-id",
            coordinator_generation: &"77".repeat(16),
            selected_topology: "download_only_shaped",
            action: NativeApplyAction::ApplySqm,
            sqm_direction_mode: NativeSqmDirectionMode::DownloadOnly,
            download: direction(NativeApplyDirectionMode::Shaped, proposal.download),
            upload,
            proposal: &proposal,
            auto_apply_evidence_pass: false,
            manual_review_required: true,
            required_acknowledgements: &[NativeApplyAcknowledgement::MeasurementConfidence],
            artifacts: NativeApplyArtifactDigests {
                proposal: PROPOSAL_DIGEST,
                download_search: DOWNLOAD_DIGEST,
                upload_search: UPLOAD_DIGEST,
                pair_confirmation: PAIR_DIGEST,
                topology_comparison: TOPOLOGY_DIGEST,
            },
        })
        .unwrap_err();
        assert!(error.contains("bypassed upload direction carries shaped rates"));

        let mut missing_rate = direction(NativeApplyDirectionMode::Shaped, proposal.download);
        missing_rate.selected_kbps = None;
        let error = canonical_native_apply_manifest_bytes(NativeApplyManifestInput {
            option_id: "recommended",
            request: &request,
            worker_run_id: &"66".repeat(16),
            review_digest: REVIEW_DIGEST,
            coordinator_boot_id: "boot-id",
            coordinator_generation: &"77".repeat(16),
            selected_topology: "both_shaped",
            action: NativeApplyAction::ApplySqm,
            sqm_direction_mode: NativeSqmDirectionMode::Both,
            download: missing_rate,
            upload: direction(NativeApplyDirectionMode::Shaped, proposal.upload),
            proposal: &proposal,
            auto_apply_evidence_pass: false,
            manual_review_required: true,
            required_acknowledgements: &[NativeApplyAcknowledgement::MeasurementConfidence],
            artifacts: NativeApplyArtifactDigests {
                proposal: PROPOSAL_DIGEST,
                download_search: DOWNLOAD_DIGEST,
                upload_search: UPLOAD_DIGEST,
                pair_confirmation: PAIR_DIGEST,
                topology_comparison: TOPOLOGY_DIGEST,
            },
        })
        .unwrap_err();
        assert!(error.contains("shaped download direction has no selected rate"));
    }

    #[test]
    fn review_and_request_binding_is_deterministic_and_tamper_evident() {
        let request = request();
        let proposal = proposal();
        let original = manifest(
            &request,
            &proposal,
            "both_shaped",
            NativeApplyAction::ApplySqm,
            NativeSqmDirectionMode::Both,
            NativeApplyDirectionMode::Shaped,
            NativeApplyDirectionMode::Shaped,
            REVIEW_DIGEST,
        )
        .unwrap();
        let repeated = manifest(
            &request,
            &proposal,
            "both_shaped",
            NativeApplyAction::ApplySqm,
            NativeSqmDirectionMode::Both,
            NativeApplyDirectionMode::Shaped,
            NativeApplyDirectionMode::Shaped,
            REVIEW_DIGEST,
        )
        .unwrap();
        assert_eq!(original, repeated);

        let changed_review = manifest(
            &request,
            &proposal,
            "both_shaped",
            NativeApplyAction::ApplySqm,
            NativeSqmDirectionMode::Both,
            NativeApplyDirectionMode::Shaped,
            NativeApplyDirectionMode::Shaped,
            &"99".repeat(32),
        )
        .unwrap();
        assert_ne!(original, changed_review);

        let mut changed_request = request.clone();
        changed_request.identity.config_fingerprint = "88".repeat(32);
        let changed_manifest = manifest(
            &changed_request,
            &proposal,
            "both_shaped",
            NativeApplyAction::ApplySqm,
            NativeSqmDirectionMode::Both,
            NativeApplyDirectionMode::Shaped,
            NativeApplyDirectionMode::Shaped,
            REVIEW_DIGEST,
        )
        .unwrap();
        assert_ne!(original, changed_manifest);

        changed_request.profile = Some(AutotuneProfile::Fair);
        assert!(manifest(
            &changed_request,
            &proposal,
            "both_shaped",
            NativeApplyAction::ApplySqm,
            NativeSqmDirectionMode::Both,
            NativeApplyDirectionMode::Shaped,
            NativeApplyDirectionMode::Shaped,
            REVIEW_DIGEST,
        )
        .unwrap_err()
        .contains("contradicts immutable request policy"));

        let mut sqm_changed_request = request.clone();
        sqm_changed_request.managed_sqm_section = Some("wanb_sqm".to_string());
        let changed_manifest = manifest(
            &sqm_changed_request,
            &proposal,
            "both_shaped",
            NativeApplyAction::ApplySqm,
            NativeSqmDirectionMode::Both,
            NativeApplyDirectionMode::Shaped,
            NativeApplyDirectionMode::Shaped,
            REVIEW_DIGEST,
        )
        .unwrap();
        assert_ne!(original, changed_manifest);
    }

    #[test]
    fn v4_identity_is_derived_from_the_exact_frozen_manifest_candidate() {
        let request = request();
        let proposal = proposal();
        let plan = execution_plan(
            &request,
            &proposal,
            "both_shaped",
            NativeApplyAction::ApplySqm,
            NativeSqmDirectionMode::Both,
            NativeApplyDirectionMode::Shaped,
            NativeApplyDirectionMode::Shaped,
            REVIEW_DIGEST,
        )
        .unwrap();
        let manifest = plan.canonical_manifest_bytes().unwrap();
        let identity = plan.v4_identity().unwrap();

        assert_eq!(
            identity.schema_version,
            NATIVE_APPLY_MANIFEST_SCHEMA_VERSION
        );
        assert_eq!(identity.job_id, request.identity.job_id);
        assert_eq!(identity.worker_run_id, plan.worker_run_id);
        assert_eq!(identity.option_id, plan.option_id);
        assert_eq!(identity.source_review_sha256, REVIEW_DIGEST);
        assert_eq!(identity.candidate_id, plan.v4_candidate_id().unwrap());
        assert_eq!(
            identity.manifest_sha256,
            sqm_identity::sha256sum(&manifest).unwrap()
        );
        let text = String::from_utf8(manifest).unwrap();
        assert!(text.contains(&format!(
            "\"candidate_id\":{}",
            json_string(&identity.candidate_id)
        )));
    }

    #[test]
    fn unattended_scheduler_apply_accepts_only_ack_free_shaped_both() {
        let mut request = request();
        request.origin = OperationOrigin::Scheduler;
        request.scheduled_auto_apply_requested = true;
        let proposal = proposal();
        let plan = NativeApplyExecutionPlan::from_verified_input(NativeApplyManifestInput {
            option_id: "recommended",
            request: &request,
            worker_run_id: &"66".repeat(16),
            review_digest: REVIEW_DIGEST,
            coordinator_boot_id: "boot-id",
            coordinator_generation: &"77".repeat(16),
            selected_topology: "both_shaped",
            action: NativeApplyAction::ApplySqm,
            sqm_direction_mode: NativeSqmDirectionMode::Both,
            download: direction(NativeApplyDirectionMode::Shaped, proposal.download),
            upload: direction(NativeApplyDirectionMode::Shaped, proposal.upload),
            proposal: &proposal,
            auto_apply_evidence_pass: true,
            manual_review_required: false,
            required_acknowledgements: &[],
            artifacts: NativeApplyArtifactDigests {
                proposal: PROPOSAL_DIGEST,
                download_search: DOWNLOAD_DIGEST,
                upload_search: UPLOAD_DIGEST,
                pair_confirmation: PAIR_DIGEST,
                topology_comparison: TOPOLOGY_DIGEST,
            },
        })
        .unwrap();
        assert!(plan.unattended_scheduler_eligible());

        let mut rejected = plan.clone();
        rejected.request.scheduled_auto_apply_requested = false;
        assert!(!rejected.unattended_scheduler_eligible());
        rejected = plan.clone();
        rejected.request.origin = OperationOrigin::Luci;
        assert!(!rejected.unattended_scheduler_eligible());
        rejected = plan.clone();
        rejected.sqm_direction_mode = NativeSqmDirectionMode::DownloadOnly;
        assert!(!rejected.unattended_scheduler_eligible());
        rejected = plan.clone();
        rejected.download.mode = NativeApplyDirectionMode::Bypass;
        assert!(!rejected.unattended_scheduler_eligible());
        rejected = plan.clone();
        rejected.upload.mode = NativeApplyDirectionMode::Bypass;
        assert!(!rejected.unattended_scheduler_eligible());
        rejected = plan.clone();
        rejected.action = NativeApplyAction::DisableSqm;
        assert!(!rejected.unattended_scheduler_eligible());
        rejected = plan.clone();
        rejected.auto_apply_evidence_pass = false;
        assert!(!rejected.unattended_scheduler_eligible());
        rejected = plan.clone();
        rejected.manual_review_required = true;
        assert!(!rejected.unattended_scheduler_eligible());
        rejected = plan.clone();
        rejected.required_acknowledgements =
            vec![NativeApplyAcknowledgement::MeasurementConfidence];
        assert!(!rejected.unattended_scheduler_eligible());
    }
}
