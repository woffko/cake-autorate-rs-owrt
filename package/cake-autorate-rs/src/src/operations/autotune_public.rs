//! Canonical public projection of a verified native Full Auto-Tune Review.
//!
//! The public projection remains non-mutating, but carries exact confirmation
//! options derived from private Apply plans. LuCI may return only an option ID,
//! immutable digests, and the complete evidence-derived acknowledgement set;
//! it must never return proposal values as Apply authority.

use super::autotune_apply::{NativeApplyAcknowledgement, NativeApplyExecutionPlan};
use super::protocol::{OperationKind, OperationRequest};
use super::sqm_identity;

pub(crate) const NATIVE_PUBLIC_RESULT_SCHEMA_VERSION: u8 = 3;
pub(crate) const NATIVE_RAW_FALLBACK_PUBLIC_RESULT_SCHEMA_VERSION: u8 = 4;
pub(crate) const NATIVE_PUBLIC_RESULT_MAX_SCHEMA_VERSION: u8 =
    NATIVE_RAW_FALLBACK_PUBLIC_RESULT_SCHEMA_VERSION;
const NATIVE_PUBLIC_APPLY_CONTRACT_SCHEMA_VERSION: u8 = 3;
const NATIVE_PUBLIC_RESULT_PRODUCER: &str = "cake-autorated-native-autotune";
const MAX_NATIVE_PUBLIC_RESULT_BYTES: usize = 256 * 1024;
const MAX_NATIVE_ARTIFACT_BYTES: usize = 192 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeArtifactKind {
    Proposal,
    DownloadSearch,
    UploadSearch,
    PairConfirmation,
    TopologyComparison,
    RawFallback,
}

impl NativeArtifactKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Proposal => "proposal",
            Self::DownloadSearch => "download_search",
            Self::UploadSearch => "upload_search",
            Self::PairConfirmation => "pair_confirmation",
            Self::TopologyComparison => "topology_comparison",
            Self::RawFallback => "raw_fallback",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VerifiedNativeArtifact {
    kind: NativeArtifactKind,
    digest: String,
    json: String,
}

impl VerifiedNativeArtifact {
    pub(crate) fn from_canonical_file(
        kind: NativeArtifactKind,
        contents: &[u8],
        expected_digest: &str,
    ) -> Result<Self, String> {
        require_lower_hex("native public artifact digest", expected_digest, 64)?;
        if contents.is_empty() || contents.len() > MAX_NATIVE_ARTIFACT_BYTES {
            return Err(format!(
                "native public {} artifact is empty or exceeds its size bound",
                kind.as_str()
            ));
        }
        if !contents.ends_with(b"\n") || contents.ends_with(b"\n\n") {
            return Err(format!(
                "native public {} artifact is not singly terminated",
                kind.as_str()
            ));
        }
        if sqm_identity::sha256sum(contents)? != expected_digest {
            return Err(format!(
                "native public {} artifact digest mismatch",
                kind.as_str()
            ));
        }
        let json = std::str::from_utf8(&contents[..contents.len() - 1])
            .map_err(|_| format!("native public {} artifact is not UTF-8", kind.as_str()))?;
        validate_single_json_object(json, kind.as_str())?;
        Ok(Self {
            kind,
            digest: expected_digest.to_string(),
            json: json.to_string(),
        })
    }
}

pub(crate) struct NativePublicResultInput<'a> {
    pub request: &'a OperationRequest,
    pub worker_run_id: &'a str,
    pub review_digest: &'a str,
    pub apply_confirmations: &'a [NativePublicApplyConfirmation],
    pub consumed_traffic_bytes: u64,
    pub artifacts: &'a [VerifiedNativeArtifact],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativePublicApplyConfirmation {
    option_id: String,
    preferred: bool,
    job_id: String,
    worker_run_id: String,
    review_digest: String,
    manifest_digest: String,
    selected_topology: String,
    action: &'static str,
    sqm_direction_mode: &'static str,
    auto_apply_evidence_pass: bool,
    manual_review_required: bool,
    required_acknowledgements: Vec<NativeApplyAcknowledgement>,
    download_kbps: Option<u64>,
    upload_kbps: Option<u64>,
}

impl NativePublicApplyConfirmation {
    pub(crate) fn from_verified_plan(
        option_id: &str,
        preferred: bool,
        plan: &NativeApplyExecutionPlan,
        manifest_digest: &str,
    ) -> Result<Self, String> {
        if option_id.is_empty()
            || option_id.len() > 32
            || !option_id
                .bytes()
                .all(|value| value.is_ascii_lowercase() || value == b'_')
        {
            return Err("native public Apply option ID is invalid".to_string());
        }
        if plan.option_id != option_id {
            return Err("native public Apply option ID changed its verified plan".to_string());
        }
        require_lower_hex("native public Apply manifest digest", manifest_digest, 64)?;
        let canonical_manifest = plan.canonical_manifest_bytes()?;
        if sqm_identity::sha256sum(&canonical_manifest)? != manifest_digest {
            return Err("native public Apply manifest digest mismatch".to_string());
        }
        Ok(Self {
            option_id: option_id.to_string(),
            preferred,
            job_id: plan.request.identity.job_id.clone(),
            worker_run_id: plan.worker_run_id.clone(),
            review_digest: plan.review_digest.clone(),
            manifest_digest: manifest_digest.to_string(),
            selected_topology: plan.selected_topology.clone(),
            action: plan.action.as_str(),
            sqm_direction_mode: plan.sqm_direction_mode.as_str(),
            auto_apply_evidence_pass: plan.auto_apply_evidence_pass,
            manual_review_required: plan.manual_review_required,
            required_acknowledgements: plan.required_acknowledgements.clone(),
            download_kbps: plan.download.maximum_kbps,
            upload_kbps: plan.upload.maximum_kbps,
        })
    }
}

pub(crate) fn canonical_native_public_result_bytes(
    input: NativePublicResultInput<'_>,
) -> Result<Vec<u8>, String> {
    input.request.validate()?;
    if input.request.identity.operation != OperationKind::FullAutotune {
        return Err("native public result requires a Full Auto-Tune request".to_string());
    }
    require_lower_hex("native public worker run id", input.worker_run_id, 32)?;
    require_lower_hex("native public Review digest", input.review_digest, 64)?;
    if input.apply_confirmations.is_empty() || input.apply_confirmations.len() > 4 {
        return Err("native public Apply contract has no bounded options".to_string());
    }
    let mut option_ids = Vec::with_capacity(input.apply_confirmations.len());
    for confirmation in input.apply_confirmations {
        if confirmation.job_id != input.request.identity.job_id
            || confirmation.worker_run_id != input.worker_run_id
            || confirmation.review_digest != input.review_digest
        {
            return Err("native public Apply contract changed its private authority".to_string());
        }
        require_lower_hex(
            "native public Apply manifest digest",
            &confirmation.manifest_digest,
            64,
        )?;
        if confirmation.auto_apply_evidence_pass
            != confirmation.required_acknowledgements.is_empty()
            || confirmation.manual_review_required
                != !confirmation.required_acknowledgements.is_empty()
        {
            return Err(
                "native public Apply acknowledgement contract changed its eligibility".to_string(),
            );
        }
        option_ids.push(confirmation.option_id.as_str());
    }
    option_ids.sort_unstable();
    option_ids.dedup();
    if option_ids.len() != input.apply_confirmations.len()
        || input
            .apply_confirmations
            .iter()
            .filter(|confirmation| confirmation.preferred)
            .count()
            != 1
    {
        return Err(
            "native public Apply options are duplicated or have no unique preference".to_string(),
        );
    }
    if input.consumed_traffic_bytes > input.request.traffic_budget_bytes {
        return Err("native public result exceeds its immutable traffic budget".to_string());
    }
    let shaped = [
        NativeArtifactKind::Proposal,
        NativeArtifactKind::DownloadSearch,
        NativeArtifactKind::UploadSearch,
        NativeArtifactKind::PairConfirmation,
        NativeArtifactKind::TopologyComparison,
    ];
    let raw_fallback = [
        NativeArtifactKind::Proposal,
        NativeArtifactKind::RawFallback,
    ];
    let public_schema_version = if input.artifacts.len() == shaped.len()
        && input
            .artifacts
            .iter()
            .zip(shaped)
            .all(|(artifact, kind)| artifact.kind == kind)
    {
        NATIVE_PUBLIC_RESULT_SCHEMA_VERSION
    } else if input.artifacts.len() == raw_fallback.len()
        && input
            .artifacts
            .iter()
            .zip(raw_fallback)
            .all(|(artifact, kind)| artifact.kind == kind)
    {
        if input.apply_confirmations.len() != 1
            || input.apply_confirmations[0].action != "disable_sqm"
            || input.apply_confirmations[0].sqm_direction_mode != "off"
            || input.apply_confirmations[0].auto_apply_evidence_pass
            || !input.apply_confirmations[0].manual_review_required
        {
            return Err(
                "native public raw fallback is not a unique manual SQM-off option".to_string(),
            );
        }
        NATIVE_RAW_FALLBACK_PUBLIC_RESULT_SCHEMA_VERSION
    } else {
        return Err("native public result has an unsupported Review artifact set".to_string());
    };

    let profile = input
        .request
        .profile
        .ok_or_else(|| "native public result has no profile".to_string())?;
    let strategy = input
        .request
        .strategy
        .ok_or_else(|| "native public result has no calibration strategy".to_string())?;
    let source_ip = input.request.route.source_ip.map_or_else(
        || "null".to_string(),
        |value| json_string(&value.to_string()),
    );
    let mwan3_member = input
        .request
        .route
        .mwan3_member
        .as_deref()
        .map_or_else(|| "null".to_string(), json_string);

    let mut output = String::new();
    output.push_str("{\"native_public_schema_version\":");
    output.push_str(&public_schema_version.to_string());
    output.push_str(",\"state\":\"review_ready\"");
    output.push_str(",\"producer\":");
    output.push_str(&json_string(NATIVE_PUBLIC_RESULT_PRODUCER));
    output.push_str(",\"source_review_sha256\":");
    output.push_str(&json_string(input.review_digest));
    output.push_str(",\"public_apply_contract\":{");
    output.push_str("\"schema_version\":");
    output.push_str(&NATIVE_PUBLIC_APPLY_CONTRACT_SCHEMA_VERSION.to_string());
    output.push_str(",\"state\":\"selection_ready\"");
    output.push_str(",\"executor_available\":true");
    output.push_str(",\"explicit_confirmation_required\":true");
    output.push_str(",\"native_job_id\":");
    output.push_str(&json_string(&input.request.identity.job_id));
    output.push_str(",\"worker_run_id\":");
    output.push_str(&json_string(input.worker_run_id));
    output.push_str(",\"source_review_sha256\":");
    output.push_str(&json_string(input.review_digest));
    output.push_str(",\"selection_contract\":\"option_id_plus_review_and_manifest_digests_and_acknowledgements\"");
    output.push_str(",\"options\":[");
    for (index, confirmation) in input.apply_confirmations.iter().enumerate() {
        if index != 0 {
            output.push(',');
        }
        output.push_str("{\"option_id\":");
        output.push_str(&json_string(&confirmation.option_id));
        output.push_str(",\"preferred\":");
        output.push_str(if confirmation.preferred {
            "true"
        } else {
            "false"
        });
        output.push_str(",\"manifest_sha256\":");
        output.push_str(&json_string(&confirmation.manifest_digest));
        output.push_str(",\"selected_topology\":");
        output.push_str(&json_string(&confirmation.selected_topology));
        output.push_str(",\"action\":");
        output.push_str(&json_string(confirmation.action));
        output.push_str(",\"sqm_direction_mode\":");
        output.push_str(&json_string(confirmation.sqm_direction_mode));
        output.push_str(",\"target_rates_kbps\":{\"download\":");
        output.push_str(&optional_u64(confirmation.download_kbps));
        output.push_str(",\"upload\":");
        output.push_str(&optional_u64(confirmation.upload_kbps));
        output.push('}');
        output.push_str(",\"auto_apply_evidence_pass\":");
        output.push_str(if confirmation.auto_apply_evidence_pass {
            "true"
        } else {
            "false"
        });
        output.push_str(",\"manual_review_required\":");
        output.push_str(if confirmation.manual_review_required {
            "true"
        } else {
            "false"
        });
        output.push_str(",\"required_acknowledgements\":[");
        for (ack_index, acknowledgement) in
            confirmation.required_acknowledgements.iter().enumerate()
        {
            if ack_index != 0 {
                output.push(',');
            }
            output.push_str(&json_string(acknowledgement.as_str()));
        }
        output.push(']');
        output.push('}');
    }
    output.push(']');
    output.push('}');
    output.push_str(",\"auto_apply_eligible\":false");
    output.push_str(",\"manual_apply_eligible\":true");
    output.push_str(",\"configuration_written\":false");
    output.push_str(",\"runtime_restored\":true");
    output.push_str(",\"recovery_pending\":false");
    output.push_str(",\"throughput_unit\":\"kbit/s\"");
    output.push_str(",\"proposal_rate_transform\":\"none\"");
    output.push_str(",\"native_job_id\":");
    output.push_str(&json_string(&input.request.identity.job_id));
    output.push_str(",\"job_id\":");
    output.push_str(&json_string(&input.request.identity.instance));
    output.push_str(",\"run_id\":");
    output.push_str(&json_string(input.worker_run_id));
    output.push_str(",\"target_interface\":");
    output.push_str(&json_string(&input.request.identity.target_interface));
    output.push_str(",\"resolved_interface\":");
    output.push_str(&json_string(&input.request.route.l3_device));
    output.push_str(",\"route_mode\":");
    output.push_str(&json_string(input.request.route.mode.as_str()));
    output.push_str(",\"mwan3_member\":");
    output.push_str(&mwan3_member);
    output.push_str(",\"source_ip\":");
    output.push_str(&source_ip);
    output.push_str(",\"route_fingerprint\":");
    output.push_str(&json_string(&format!(
        "sha256:{}",
        input.request.identity.route_fingerprint
    )));
    output.push_str(",\"config_fingerprint\":");
    output.push_str(&json_string(&format!(
        "sha256:{}",
        input.request.identity.config_fingerprint
    )));
    output.push_str(",\"sqm_fingerprint\":");
    output.push_str(&json_string(&format!(
        "sha256:{}",
        input.request.identity.sqm_fingerprint
    )));
    output.push_str(",\"profile\":");
    output.push_str(&json_string(profile.as_str()));
    output.push_str(",\"calibration_strategy\":");
    output.push_str(&json_string(strategy.as_str()));
    output.push_str(",\"consumed_traffic_bytes\":");
    output.push_str(&input.consumed_traffic_bytes.to_string());
    output.push_str(",\"artifacts\":{");
    for (index, artifact) in input.artifacts.iter().enumerate() {
        if index != 0 {
            output.push(',');
        }
        output.push_str(&json_string(artifact.kind.as_str()));
        output.push_str(":{\"sha256\":");
        output.push_str(&json_string(&artifact.digest));
        output.push_str(",\"value\":");
        output.push_str(&artifact.json);
        output.push('}');
    }
    output.push_str("}}\n");

    let bytes = output.into_bytes();
    if bytes.len() > MAX_NATIVE_PUBLIC_RESULT_BYTES {
        return Err("native public result exceeds its size bound".to_string());
    }
    Ok(bytes)
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

fn validate_single_json_object(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > MAX_NATIVE_ARTIFACT_BYTES
        || value.as_bytes().contains(&b'\n')
        || value.as_bytes().contains(&b'\r')
        || value.as_bytes().contains(&0)
        || !value.starts_with('{')
        || !value.ends_with('}')
    {
        return Err(format!(
            "native public {label} artifact is not single-line object JSON"
        ));
    }

    let mut stack = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    for byte in value.bytes() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            } else if byte < 0x20 {
                return Err(format!("native public {label} artifact has a control byte"));
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => stack.push(byte),
            b'}' => {
                if stack.pop() != Some(b'{') {
                    return Err(format!(
                        "native public {label} artifact is structurally invalid"
                    ));
                }
            }
            b']' => {
                if stack.pop() != Some(b'[') {
                    return Err(format!(
                        "native public {label} artifact is structurally invalid"
                    ));
                }
            }
            _ => {}
        }
    }
    if in_string || escaped || !stack.is_empty() {
        return Err(format!(
            "native public {label} artifact is structurally invalid"
        ));
    }
    Ok(())
}

fn optional_u64(value: Option<u64>) -> String {
    value.map_or_else(|| "null".to_string(), |value| value.to_string())
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
            value if value.is_control() => {
                use std::fmt::Write as _;
                let _ = write!(output, "\\u{:04x}", value as u32);
            }
            value => output.push(value),
        }
    }
    output.push('"');
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autotune::{
        AccessEvidenceSource, AccessMedium, AutotuneProfile, CapacityLearningPolicy,
    };
    use crate::operations::protocol::{
        CalibrationStrategy, OperationIdentity, OperationOrigin, OperationRouteIdentity,
        OperationRouteMode, OperationTargetState,
    };
    use std::net::{IpAddr, Ipv4Addr};

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
            deadline_unix_ms: 2,
            origin: OperationOrigin::Luci,
            backend: "speedtest-go".to_string(),
            speedtest_direction: None,
            speedtest_server_id: Some(17372),
            speedtest_topology: None,
            route: OperationRouteIdentity {
                mode: OperationRouteMode::Main,
                mwan3_member: None,
                l3_device: "pppoe-wan".to_string(),
                source_ip: Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10))),
                fwmark: None,
                routing_table: None,
            },
            target_state: OperationTargetState::ExistingManaged,
            capture_policy: None,
            managed_sqm_section: Some("wan_sqm".to_string()),
            profile: Some(AutotuneProfile::VariableLink),
            strategy: Some(CalibrationStrategy::FullRaw),
            access_medium: Some(AccessMedium::Cellular),
            access_source: Some(AccessEvidenceSource::UserSelected),
            access_confidence_percent: 100,
            capacity_learning_policy: Some(CapacityLearningPolicy::VerifiedOnly),
            service_dl_cap_kbps: None,
            service_ul_cap_kbps: None,
            allow_sqm_disable: true,
            allow_active_traffic: false,
            scheduled_auto_apply_requested: false,
            traffic_budget_bytes: 25_000_000_000,
        }
    }

    fn artifact(kind: NativeArtifactKind, json: &str) -> VerifiedNativeArtifact {
        let bytes = format!("{json}\n").into_bytes();
        let digest = sqm_identity::sha256sum(&bytes).unwrap();
        VerifiedNativeArtifact::from_canonical_file(kind, &bytes, &digest).unwrap()
    }

    fn artifacts() -> Vec<VerifiedNativeArtifact> {
        vec![
            artifact(
                NativeArtifactKind::Proposal,
                r#"{"schema_version":4,"download":{"base_kbps":904900},"upload":{"base_kbps":915600}}"#,
            ),
            artifact(
                NativeArtifactKind::DownloadSearch,
                r#"{"schema_version":4,"transport_censored":true}"#,
            ),
            artifact(
                NativeArtifactKind::UploadSearch,
                r#"{"schema_version":4,"transport_censored":false}"#,
            ),
            artifact(
                NativeArtifactKind::PairConfirmation,
                r#"{"schema_version":4,"transport_censored":true,"transport_timeout_count":3,"transport_timeout_total_us":15000000}"#,
            ),
            artifact(
                NativeArtifactKind::TopologyComparison,
                r#"{"schema_version":2,"transport_censored":true}"#,
            ),
        ]
    }

    fn confirmation() -> NativePublicApplyConfirmation {
        NativePublicApplyConfirmation {
            option_id: "recommended".to_string(),
            preferred: true,
            job_id: "11".repeat(16),
            worker_run_id: "66".repeat(16),
            review_digest: "77".repeat(32),
            manifest_digest: "88".repeat(32),
            selected_topology: "both_shaped".to_string(),
            action: "apply_sqm",
            sqm_direction_mode: "both",
            auto_apply_evidence_pass: false,
            manual_review_required: true,
            required_acknowledgements: vec![NativeApplyAcknowledgement::MeasurementConfidence],
            download_kbps: Some(904_900),
            upload_kbps: Some(915_600),
        }
    }

    #[test]
    fn public_projection_is_confirmation_ready_and_preserves_exact_rates() {
        let request = request();
        let artifacts = artifacts();
        let confirmation = confirmation();
        let bytes = canonical_native_public_result_bytes(NativePublicResultInput {
            request: &request,
            worker_run_id: &"66".repeat(16),
            review_digest: &"77".repeat(32),
            apply_confirmations: std::slice::from_ref(&confirmation),
            consumed_traffic_bytes: 17_741_550_972,
            artifacts: &artifacts,
        })
        .unwrap();
        let output = String::from_utf8(bytes).unwrap();
        assert!(output.ends_with('\n'));
        assert!(output.contains("\"native_public_schema_version\":3"));
        assert!(output.contains("\"public_apply_contract\":{\"schema_version\":3"));
        assert!(output.contains("\"state\":\"selection_ready\""));
        assert!(output.contains("\"executor_available\":true"));
        assert!(output.contains("\"explicit_confirmation_required\":true"));
        assert!(output.contains(concat!(
            "\"selection_contract\":",
            "\"option_id_plus_review_and_manifest_digests_and_acknowledgements\""
        )));
        assert!(output.contains("\"option_id\":\"recommended\""));
        assert!(output.contains(&format!("\"manifest_sha256\":\"{}\"", "88".repeat(32))));
        assert!(output.contains("\"selected_topology\":\"both_shaped\""));
        assert!(output.contains("\"action\":\"apply_sqm\""));
        assert!(output.contains("\"sqm_direction_mode\":\"both\""));
        assert!(output.contains("\"required_acknowledgements\":[\"measurement-confidence\"]"));
        assert!(output.contains("\"auto_apply_eligible\":false"));
        assert!(output.contains("\"manual_apply_eligible\":true"));
        assert!(output.contains("\"proposal_rate_transform\":\"none\""));
        assert!(output.contains("\"base_kbps\":904900"));
        assert!(output.contains("\"base_kbps\":915600"));
        assert!(output.contains("\"transport_timeout_count\":3"));
        assert!(output.contains("\"transport_timeout_total_us\":15000000"));
        assert!(!output.contains("723920"));
        assert!(!output.contains("732480"));
    }

    #[test]
    fn public_projection_exposes_multiple_digest_bound_options_without_rate_authority() {
        let request = request();
        let artifacts = artifacts();
        let recommended = confirmation();
        let mut throughput = confirmation();
        throughput.option_id = "throughput_first".to_string();
        throughput.preferred = false;
        throughput.manifest_digest = "99".repeat(32);
        throughput.download_kbps = Some(950_000);
        throughput.upload_kbps = Some(940_000);
        let confirmations = vec![recommended, throughput];
        let output = canonical_native_public_result_bytes(NativePublicResultInput {
            request: &request,
            worker_run_id: &"66".repeat(16),
            review_digest: &"77".repeat(32),
            apply_confirmations: &confirmations,
            consumed_traffic_bytes: 1,
            artifacts: &artifacts,
        })
        .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert_eq!(output.matches("\"option_id\":").count(), 2);
        assert!(output.contains("\"option_id\":\"recommended\""));
        assert!(output.contains("\"option_id\":\"throughput_first\""));
        assert!(output.contains("\"download\":950000"));

        let mut duplicate = confirmations.clone();
        duplicate[1].option_id = "recommended".to_string();
        let error = canonical_native_public_result_bytes(NativePublicResultInput {
            request: &request,
            worker_run_id: &"66".repeat(16),
            review_digest: &"77".repeat(32),
            apply_confirmations: &duplicate,
            consumed_traffic_bytes: 1,
            artifacts: &artifacts,
        })
        .unwrap_err();
        assert!(error.contains("duplicated"));
    }

    #[test]
    fn artifact_digest_mismatch_is_rejected() {
        let error = VerifiedNativeArtifact::from_canonical_file(
            NativeArtifactKind::Proposal,
            b"{}\n",
            &"88".repeat(32),
        )
        .unwrap_err();
        assert!(error.contains("digest mismatch"));
    }

    #[test]
    fn missing_or_reordered_artifact_is_rejected() {
        let request = request();
        let mut artifacts = artifacts();
        artifacts.swap(0, 1);
        let confirmation = confirmation();
        let error = canonical_native_public_result_bytes(NativePublicResultInput {
            request: &request,
            worker_run_id: &"66".repeat(16),
            review_digest: &"77".repeat(32),
            apply_confirmations: std::slice::from_ref(&confirmation),
            consumed_traffic_bytes: 1,
            artifacts: &artifacts,
        })
        .unwrap_err();
        assert!(error.contains("unsupported Review artifact set"));
    }

    #[test]
    fn confirmation_identity_cannot_be_rebound_to_another_public_result() {
        let request = request();
        let artifacts = artifacts();
        let mut confirmation = confirmation();
        confirmation.job_id = "99".repeat(16);
        let error = canonical_native_public_result_bytes(NativePublicResultInput {
            request: &request,
            worker_run_id: &"66".repeat(16),
            review_digest: &"77".repeat(32),
            apply_confirmations: std::slice::from_ref(&confirmation),
            consumed_traffic_bytes: 1,
            artifacts: &artifacts,
        })
        .unwrap_err();
        assert!(error.contains("changed its private authority"));
    }

    #[test]
    fn acknowledgement_eligibility_cannot_be_rebound_in_the_public_projection() {
        let request = request();
        let artifacts = artifacts();
        let mut confirmation = confirmation();
        confirmation.required_acknowledgements.clear();
        let error = canonical_native_public_result_bytes(NativePublicResultInput {
            request: &request,
            worker_run_id: &"66".repeat(16),
            review_digest: &"77".repeat(32),
            apply_confirmations: std::slice::from_ref(&confirmation),
            consumed_traffic_bytes: 1,
            artifacts: &artifacts,
        })
        .unwrap_err();
        assert!(error.contains("acknowledgement contract"));
    }

    #[test]
    fn malformed_embedded_json_is_rejected_even_with_matching_digest() {
        let bytes = b"{\"value\":[}}\n";
        let digest = sqm_identity::sha256sum(bytes).unwrap();
        let error = VerifiedNativeArtifact::from_canonical_file(
            NativeArtifactKind::Proposal,
            bytes,
            &digest,
        )
        .unwrap_err();
        assert!(error.contains("structurally invalid"));
    }

    #[test]
    fn traffic_budget_is_enforced_by_the_projection() {
        let mut request = request();
        request.traffic_budget_bytes = 1_000;
        let artifacts = artifacts();
        let confirmation = confirmation();
        let error = canonical_native_public_result_bytes(NativePublicResultInput {
            request: &request,
            worker_run_id: &"66".repeat(16),
            review_digest: &"77".repeat(32),
            apply_confirmations: std::slice::from_ref(&confirmation),
            consumed_traffic_bytes: 1_001,
            artifacts: &artifacts,
        })
        .unwrap_err();
        assert!(error.contains("traffic budget"));
    }
}
