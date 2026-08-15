//! Schema-v7 Apply authority for an absent Full Auto-Tune target.
//!
//! This binds an already verified schema-v4 evidence selection to an explicit
//! bootstrap policy, the runtime owner's exact absence witness and a complete
//! managed-configuration plan. The schema-v4 mutation vector is source
//! identity only and is never valid bootstrap execution authority.

use super::autotune_apply::{
    NativeApplyAction, NativeApplyAuthorityIdentity, NativeApplyExecutionPlan,
    NATIVE_APPLY_MANIFEST_SCHEMA_VERSION, NATIVE_RAW_FALLBACK_APPLY_MANIFEST_SCHEMA_VERSION,
};
use super::autotune_capture_policy::{AutotuneCapturePolicy, AutotuneCapturePolicyId};
use super::autotune_managed_config::{
    BootstrapApplyInputs, NativeBootstrapPersistPolicy, NativeBootstrapServerPersistence,
    NativeManagedConfigPlan,
};
use super::autotune_runtime::AbsentRuntimeBaseline;
use super::protocol::{OperationKind, OperationOrigin, OperationRequest, OperationTargetState};
use ring::digest::{digest, SHA256};

pub(crate) const NATIVE_BOOTSTRAP_APPLY_MANIFEST_SCHEMA_VERSION: u8 = 7;
pub(crate) const NATIVE_RAW_FALLBACK_BOOTSTRAP_APPLY_MANIFEST_SCHEMA_VERSION: u8 = 8;
pub(crate) const NATIVE_BOOTSTRAP_APPLY_POLICY_SCHEMA_VERSION: u8 = 2;
pub(crate) const MAX_NATIVE_BOOTSTRAP_APPLY_MANIFEST_BYTES: usize = 128 * 1024;
pub(crate) const MAX_NATIVE_BOOTSTRAP_APPLY_UCI_MUTATIONS: usize = 128;
const MAX_NATIVE_BOOTSTRAP_APPLY_POLICY_BYTES: usize = 24 * 1024;
const NATIVE_BOOTSTRAP_REQUEST_SCHEMA_VERSION: u8 = 6;
const BOOTSTRAP_CANDIDATE_DOMAIN_V3: &str = "cake-autorate-native-bootstrap-apply-candidate-v3";
const RAW_FALLBACK_BOOTSTRAP_CANDIDATE_DOMAIN_V4: &str =
    "cake-autorate-native-bootstrap-raw-fallback-candidate-v4";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeBootstrapApplyMode {
    ShapedRuntime,
    DisabledInactive,
}

impl NativeBootstrapApplyMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ShapedRuntime => "shaped_runtime",
            Self::DisabledInactive => "disabled_inactive",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "shaped_runtime" => Some(Self::ShapedRuntime),
            "disabled_inactive" => Some(Self::DisabledInactive),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeBootstrapApplyPolicy {
    capture_policy_id: AutotuneCapturePolicyId,
    capture_policy_sha256: String,
    speedtest_apply_percent: u8,
    server_persistence: NativeBootstrapServerPersistence,
    persist_policy: NativeBootstrapPersistPolicy,
}

impl NativeBootstrapApplyPolicy {
    pub(crate) fn defaults_v1(request: &OperationRequest) -> Result<Self, String> {
        Self::new(
            request,
            90,
            NativeBootstrapServerPersistence::LeaveAutomatic,
        )
    }

    pub(crate) fn new(
        request: &OperationRequest,
        speedtest_apply_percent: u8,
        server_persistence: NativeBootstrapServerPersistence,
    ) -> Result<Self, String> {
        let capture_policy_id = request.capture_policy.ok_or_else(|| {
            "native bootstrap Apply request has no capture policy authority".to_string()
        })?;
        let capture_policy_sha256 = capture_policy_id.canonical_sha256()?;
        let persist_policy =
            NativeBootstrapPersistPolicy::new(speedtest_apply_percent, server_persistence)?;
        let value = Self {
            capture_policy_id,
            capture_policy_sha256,
            speedtest_apply_percent,
            server_persistence,
            persist_policy,
        };
        value.ensure_matches_request(request)?;
        let _ = value.canonical_bytes()?;
        Ok(value)
    }

    fn capture_policy(&self) -> Result<AutotuneCapturePolicy, String> {
        let policy = self.capture_policy_id.expand()?;
        if policy.canonical_sha256()? != self.capture_policy_sha256 {
            return Err("native bootstrap capture policy identity changed".to_string());
        }
        Ok(policy)
    }

    fn ensure_matches_request(&self, request: &OperationRequest) -> Result<(), String> {
        let _ = self.capture_policy()?;
        if request.capture_policy != Some(self.capture_policy_id) {
            return Err(
                "native bootstrap Apply policy differs from the request capture authority"
                    .to_string(),
            );
        }
        Ok(())
    }

    pub(crate) fn canonical_bytes(&self) -> Result<Vec<u8>, String> {
        let mut output = format!(
            concat!(
                "{{\"native_bootstrap_apply_policy_schema_version\":{},",
                "\"capture_policy\":{{\"id\":{},\"sha256\":{}}},",
                "\"persist\":{{\"speedtest_apply_percent\":{},",
                "\"server_persistence\":{}}}}}\n"
            ),
            NATIVE_BOOTSTRAP_APPLY_POLICY_SCHEMA_VERSION,
            json_string(self.capture_policy_id.as_str()),
            json_string(&self.capture_policy_sha256),
            self.speedtest_apply_percent,
            json_string(server_persistence_name(self.server_persistence)),
        );
        if output.len() > MAX_NATIVE_BOOTSTRAP_APPLY_POLICY_BYTES {
            return Err("native bootstrap Apply policy exceeds its size bound".to_string());
        }
        validate_canonical_json_line("native bootstrap Apply policy", output.as_bytes())?;
        Ok(std::mem::take(&mut output).into_bytes())
    }

    pub(crate) fn canonical_sha256(&self) -> Result<String, String> {
        Ok(sha256_hex(&self.canonical_bytes()?))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeBootstrapApplyIdentity {
    pub(crate) job_id: String,
    pub(crate) worker_run_id: String,
    pub(crate) option_id: String,
    pub(crate) request_sha256: String,
    pub(crate) source_apply_v4_manifest_sha256: String,
    pub(crate) source_review_sha256: String,
    pub(crate) source_candidate_id: String,
    pub(crate) source_apply_schema_version: u8,
    pub(crate) bootstrap_policy_sha256: String,
    pub(crate) managed_config_schema_version: u8,
    pub(crate) managed_config_sha256: String,
    pub(crate) managed_config_action_count: u16,
    pub(crate) target_ifindex: u32,
    pub(crate) kernel_topology_fingerprint: String,
    pub(crate) kernel_namespace_seed: String,
    pub(crate) composite_candidate_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NativeBootstrapApplyPlan {
    source_apply: NativeApplyExecutionPlan,
    source_authority: NativeApplyAuthorityIdentity,
    policy: NativeBootstrapApplyPolicy,
    managed_config: NativeManagedConfigPlan,
    absent_baseline: AbsentRuntimeBaseline,
    identity: NativeBootstrapApplyIdentity,
}

impl NativeBootstrapApplyPlan {
    pub(crate) fn from_verified_source(
        source_apply: NativeApplyExecutionPlan,
        policy: NativeBootstrapApplyPolicy,
        absent_baseline: AbsentRuntimeBaseline,
    ) -> Result<Self, String> {
        validate_source_apply(&source_apply)?;
        policy.ensure_matches_request(&source_apply.request)?;
        validate_absent_baseline(&source_apply.request, &absent_baseline)?;
        let source_authority = source_apply.authority_identity()?;
        match (&source_authority, source_apply.action) {
            (NativeApplyAuthorityIdentity::ShapedV4(value), NativeApplyAction::ApplySqm)
                if value.schema_version == NATIVE_APPLY_MANIFEST_SCHEMA_VERSION => {}
            (NativeApplyAuthorityIdentity::RawFallbackV5(value), NativeApplyAction::DisableSqm)
                if value.schema_version == NATIVE_RAW_FALLBACK_APPLY_MANIFEST_SCHEMA_VERSION => {}
            _ => {
                return Err(
                    "native bootstrap Apply source schema/action is inconsistent".to_string(),
                )
            }
        }

        let request_bytes = source_apply.request.encode()?.into_bytes();
        if !request_bytes.starts_with(b"cake-autorate-operation\t6\trequest\n") {
            return Err("native bootstrap Apply request is not canonical schema v6".to_string());
        }
        let request_sha256 = sha256_hex(&request_bytes);
        let bootstrap_policy_sha256 = policy.canonical_sha256()?;
        let capture_policy = policy.capture_policy()?;
        let managed_config = NativeManagedConfigPlan::from_bootstrap(BootstrapApplyInputs {
            verified_apply: &source_apply,
            capture_policy: &capture_policy,
            persist_policy: policy.persist_policy,
        })?;
        let managed_config_action_count =
            u16::try_from(managed_config.action_count()).map_err(|_| {
                "native bootstrap Apply action count overflows its wire type".to_string()
            })?;
        if managed_config_action_count == 0
            || usize::from(managed_config_action_count) > MAX_NATIVE_BOOTSTRAP_APPLY_UCI_MUTATIONS
        {
            return Err("native bootstrap Apply action count is outside its bound".to_string());
        }
        let managed_config_sha256 = managed_config.canonical_sha256()?;
        let composite_candidate_id = composite_candidate_id(
            &request_sha256,
            &source_authority,
            &bootstrap_policy_sha256,
            &managed_config_sha256,
            managed_config_action_count,
            &absent_baseline,
        );
        let identity = NativeBootstrapApplyIdentity {
            job_id: source_authority.job_id().to_string(),
            worker_run_id: source_authority.worker_run_id().to_string(),
            option_id: source_authority.option_id().to_string(),
            request_sha256,
            source_apply_v4_manifest_sha256: source_authority.manifest_sha256().to_string(),
            source_review_sha256: source_authority.source_review_sha256().to_string(),
            source_candidate_id: source_authority.candidate_id().to_string(),
            source_apply_schema_version: source_authority.schema_version(),
            bootstrap_policy_sha256,
            managed_config_schema_version: managed_config.schema_version(),
            managed_config_sha256,
            managed_config_action_count,
            target_ifindex: absent_baseline.target_ifindex,
            kernel_topology_fingerprint: absent_baseline.kernel_topology_fingerprint.clone(),
            kernel_namespace_seed: absent_baseline.kernel_namespace_seed.clone(),
            composite_candidate_id,
        };
        let value = Self {
            source_apply,
            source_authority,
            policy,
            managed_config,
            absent_baseline,
            identity,
        };
        value.validate_exact_bindings()?;
        let _ = value.canonical_manifest_bytes()?;
        Ok(value)
    }

    pub(crate) fn request(&self) -> &OperationRequest {
        &self.source_apply.request
    }

    pub(crate) fn source_apply(&self) -> &NativeApplyExecutionPlan {
        &self.source_apply
    }

    pub(crate) fn identity(&self) -> &NativeBootstrapApplyIdentity {
        &self.identity
    }

    pub(crate) fn managed_config(&self) -> &NativeManagedConfigPlan {
        &self.managed_config
    }

    pub(crate) fn absent_baseline(&self) -> &AbsentRuntimeBaseline {
        &self.absent_baseline
    }

    pub(crate) fn manifest_schema_version(&self) -> u8 {
        match &self.source_authority {
            NativeApplyAuthorityIdentity::ShapedV4(_) => {
                NATIVE_BOOTSTRAP_APPLY_MANIFEST_SCHEMA_VERSION
            }
            NativeApplyAuthorityIdentity::RawFallbackV5(_) => {
                NATIVE_RAW_FALLBACK_BOOTSTRAP_APPLY_MANIFEST_SCHEMA_VERSION
            }
            NativeApplyAuthorityIdentity::DirectionalRawFallbackV6(_) => {
                unreachable!("directional raw fallback is rejected by bootstrap construction")
            }
        }
    }

    pub(crate) fn source_manifest_sha256(&self) -> &str {
        &self.identity.source_apply_v4_manifest_sha256
    }

    pub(crate) fn mode(&self) -> NativeBootstrapApplyMode {
        match &self.source_authority {
            NativeApplyAuthorityIdentity::ShapedV4(_) => NativeBootstrapApplyMode::ShapedRuntime,
            NativeApplyAuthorityIdentity::RawFallbackV5(_) => {
                NativeBootstrapApplyMode::DisabledInactive
            }
            NativeApplyAuthorityIdentity::DirectionalRawFallbackV6(_) => {
                unreachable!("directional raw fallback is rejected by bootstrap construction")
            }
        }
    }

    pub(crate) fn canonical_manifest_sha256(&self) -> Result<String, String> {
        Ok(sha256_hex(&self.canonical_manifest_bytes()?))
    }

    pub(crate) fn canonical_manifest_bytes(&self) -> Result<Vec<u8>, String> {
        self.validate_exact_bindings()?;
        if matches!(
            &self.source_authority,
            NativeApplyAuthorityIdentity::RawFallbackV5(_)
        ) {
            return self.canonical_raw_fallback_manifest_bytes();
        }
        if matches!(
            &self.source_authority,
            NativeApplyAuthorityIdentity::DirectionalRawFallbackV6(_)
        ) {
            return Err(
                "directional raw fallback is not yet a bootstrap Apply authority".to_string(),
            );
        }
        let NativeApplyAuthorityIdentity::ShapedV4(source_v4) = &self.source_authority else {
            unreachable!("raw fallback returned above")
        };
        let request = &self.source_apply.request;
        let managed_sqm_section = request.managed_sqm_section.as_deref().ok_or_else(|| {
            "native bootstrap Apply request has no planned SQM section".to_string()
        })?;
        let policy_bytes = self.policy.canonical_bytes()?;
        let policy_json = canonical_json_object("native bootstrap Apply policy", &policy_bytes)?;
        let config_bytes = self.managed_config.canonical_bytes()?;
        let config_json = canonical_json_object("native managed config", &config_bytes)?;
        let mut output = format!(
            concat!(
                "{{\"native_apply_manifest_schema_version\":{},",
                "\"state\":\"confirmation_ready\",\"apply_enabled\":true,",
                "\"auto_apply_enabled\":false,\"manual_apply_enabled\":true,",
                "\"target_state\":\"absent_bootstrap\",",
                "\"option_id\":{},\"job_id\":{},\"worker_run_id\":{},",
                "\"instance\":{},\"target_interface\":{},\"resolved_interface\":{},",
                "\"managed_sqm_section\":{},\"route_fingerprint\":{},",
                "\"config_fingerprint\":{},\"sqm_fingerprint\":{},",
                "\"absence_baseline\":{{\"target_ifindex\":{},",
                "\"kernel_topology_fingerprint\":{},\"kernel_namespace_seed\":{}}},",
                "\"source_request\":{{\"schema_version\":{},\"sha256\":{}}},",
                "\"source_apply_v4\":{{\"schema_version\":{},",
                "\"manifest_sha256\":{},\"source_review_sha256\":{},",
                "\"candidate_id\":{}}},",
                "\"bootstrap_policy\":{{\"schema_version\":{},\"sha256\":{},",
                "\"value\":{}}},",
                "\"managed_config\":{{\"schema_version\":{},\"sha256\":{},",
                "\"action_count\":{},\"value\":{}}},",
                "\"candidate_id\":{}}}\n"
            ),
            NATIVE_BOOTSTRAP_APPLY_MANIFEST_SCHEMA_VERSION,
            json_string(&self.identity.option_id),
            json_string(&self.identity.job_id),
            json_string(&self.identity.worker_run_id),
            json_string(&request.identity.instance),
            json_string(&request.identity.target_interface),
            json_string(&request.route.l3_device),
            json_string(managed_sqm_section),
            json_string(&request.identity.route_fingerprint),
            json_string(&request.identity.config_fingerprint),
            json_string(&request.identity.sqm_fingerprint),
            self.identity.target_ifindex,
            json_string(&self.identity.kernel_topology_fingerprint),
            json_string(&self.identity.kernel_namespace_seed),
            NATIVE_BOOTSTRAP_REQUEST_SCHEMA_VERSION,
            json_string(&self.identity.request_sha256),
            source_v4.schema_version,
            json_string(&self.identity.source_apply_v4_manifest_sha256),
            json_string(&self.identity.source_review_sha256),
            json_string(&self.identity.source_candidate_id),
            NATIVE_BOOTSTRAP_APPLY_POLICY_SCHEMA_VERSION,
            json_string(&self.identity.bootstrap_policy_sha256),
            policy_json,
            self.identity.managed_config_schema_version,
            json_string(&self.identity.managed_config_sha256),
            self.identity.managed_config_action_count,
            config_json,
            json_string(&self.identity.composite_candidate_id),
        );
        if output.len() > MAX_NATIVE_BOOTSTRAP_APPLY_MANIFEST_BYTES {
            return Err("native bootstrap Apply manifest exceeds its size bound".to_string());
        }
        validate_canonical_json_line("native bootstrap Apply manifest", output.as_bytes())?;
        Ok(std::mem::take(&mut output).into_bytes())
    }

    fn canonical_raw_fallback_manifest_bytes(&self) -> Result<Vec<u8>, String> {
        let NativeApplyAuthorityIdentity::RawFallbackV5(source_v5) = &self.source_authority else {
            return Err("native bootstrap raw manifest lacks schema-v5 authority".to_string());
        };
        let request = &self.source_apply.request;
        let managed_sqm_section = request.managed_sqm_section.as_deref().ok_or_else(|| {
            "native bootstrap Apply request has no planned SQM section".to_string()
        })?;
        let policy_bytes = self.policy.canonical_bytes()?;
        let policy_json = canonical_json_object("native bootstrap Apply policy", &policy_bytes)?;
        let config_bytes = self.managed_config.canonical_bytes()?;
        let config_json = canonical_json_object("native managed config", &config_bytes)?;
        let mut output = format!(
            concat!(
                "{{\"native_apply_manifest_schema_version\":{},",
                "\"state\":\"confirmation_ready\",\"apply_enabled\":true,",
                "\"auto_apply_enabled\":false,\"manual_apply_enabled\":true,",
                "\"target_state\":\"absent_bootstrap\",",
                "\"option_id\":{},\"job_id\":{},\"worker_run_id\":{},",
                "\"instance\":{},\"target_interface\":{},\"resolved_interface\":{},",
                "\"managed_sqm_section\":{},\"route_fingerprint\":{},",
                "\"config_fingerprint\":{},\"sqm_fingerprint\":{},",
                "\"absence_baseline\":{{\"target_ifindex\":{},",
                "\"kernel_topology_fingerprint\":{},\"kernel_namespace_seed\":{}}},",
                "\"source_request\":{{\"schema_version\":{},\"sha256\":{}}},",
                "\"source_apply_v5\":{{\"schema_version\":{},",
                "\"manifest_sha256\":{},\"source_review_sha256\":{},",
                "\"candidate_id\":{}}},",
                "\"bootstrap_policy\":{{\"schema_version\":{},\"sha256\":{},",
                "\"value\":{}}},",
                "\"managed_config\":{{\"schema_version\":{},\"sha256\":{},",
                "\"action_count\":{},\"value\":{}}},",
                "\"candidate_id\":{}}}\n"
            ),
            NATIVE_RAW_FALLBACK_BOOTSTRAP_APPLY_MANIFEST_SCHEMA_VERSION,
            json_string(&self.identity.option_id),
            json_string(&self.identity.job_id),
            json_string(&self.identity.worker_run_id),
            json_string(&request.identity.instance),
            json_string(&request.identity.target_interface),
            json_string(&request.route.l3_device),
            json_string(managed_sqm_section),
            json_string(&request.identity.route_fingerprint),
            json_string(&request.identity.config_fingerprint),
            json_string(&request.identity.sqm_fingerprint),
            self.identity.target_ifindex,
            json_string(&self.identity.kernel_topology_fingerprint),
            json_string(&self.identity.kernel_namespace_seed),
            NATIVE_BOOTSTRAP_REQUEST_SCHEMA_VERSION,
            json_string(&self.identity.request_sha256),
            source_v5.schema_version,
            json_string(&self.identity.source_apply_v4_manifest_sha256),
            json_string(&self.identity.source_review_sha256),
            json_string(&self.identity.source_candidate_id),
            NATIVE_BOOTSTRAP_APPLY_POLICY_SCHEMA_VERSION,
            json_string(&self.identity.bootstrap_policy_sha256),
            policy_json,
            self.identity.managed_config_schema_version,
            json_string(&self.identity.managed_config_sha256),
            self.identity.managed_config_action_count,
            config_json,
            json_string(&self.identity.composite_candidate_id),
        );
        if output.len() > MAX_NATIVE_BOOTSTRAP_APPLY_MANIFEST_BYTES {
            return Err("native bootstrap raw Apply manifest exceeds its size bound".to_string());
        }
        validate_canonical_json_line("native bootstrap raw Apply manifest", output.as_bytes())?;
        Ok(std::mem::take(&mut output).into_bytes())
    }

    fn validate_exact_bindings(&self) -> Result<(), String> {
        validate_source_apply(&self.source_apply)?;
        let current_source = self.source_apply.authority_identity()?;
        if current_source != self.source_authority
            || self.identity.job_id != current_source.job_id()
            || self.identity.worker_run_id != current_source.worker_run_id()
            || self.identity.option_id != current_source.option_id()
            || self.identity.source_apply_schema_version != current_source.schema_version()
            || self.identity.source_apply_v4_manifest_sha256 != current_source.manifest_sha256()
            || self.identity.source_review_sha256 != current_source.source_review_sha256()
            || self.identity.source_candidate_id != current_source.candidate_id()
        {
            return Err("native bootstrap Apply source identity changed".to_string());
        }
        let request_bytes = self.source_apply.request.encode()?.into_bytes();
        if self.identity.request_sha256 != sha256_hex(&request_bytes) {
            return Err("native bootstrap Apply request identity changed".to_string());
        }
        if self.identity.bootstrap_policy_sha256 != self.policy.canonical_sha256()? {
            return Err("native bootstrap Apply policy identity changed".to_string());
        }
        self.policy
            .ensure_matches_request(&self.source_apply.request)?;
        if self.identity.managed_config_schema_version != self.managed_config.schema_version()
            || self.identity.managed_config_sha256 != self.managed_config.canonical_sha256()?
            || usize::from(self.identity.managed_config_action_count)
                != self.managed_config.action_count()
            || self.managed_config.action_count() > MAX_NATIVE_BOOTSTRAP_APPLY_UCI_MUTATIONS
        {
            return Err("native bootstrap Apply managed-config identity changed".to_string());
        }
        validate_absent_baseline(&self.source_apply.request, &self.absent_baseline)?;
        if self.identity.target_ifindex != self.absent_baseline.target_ifindex
            || self.identity.kernel_topology_fingerprint
                != self.absent_baseline.kernel_topology_fingerprint
            || self.identity.kernel_namespace_seed != self.absent_baseline.kernel_namespace_seed
        {
            return Err("native bootstrap Apply absence identity changed".to_string());
        }
        let expected_candidate = composite_candidate_id(
            &self.identity.request_sha256,
            &self.source_authority,
            &self.identity.bootstrap_policy_sha256,
            &self.identity.managed_config_sha256,
            self.identity.managed_config_action_count,
            &self.absent_baseline,
        );
        if self.identity.composite_candidate_id != expected_candidate {
            return Err("native bootstrap Apply composite candidate identity changed".to_string());
        }
        Ok(())
    }
}

fn validate_absent_baseline(
    request: &OperationRequest,
    baseline: &AbsentRuntimeBaseline,
) -> Result<(), String> {
    baseline.validate()?;
    if request.target_state != OperationTargetState::AbsentBootstrap
        || request.managed_sqm_section.as_deref() != Some(baseline.planned_sqm_section.as_str())
        || request.identity.target_interface != baseline.target_interface
        || request.identity.route_fingerprint != baseline.route_fingerprint
        || request.identity.config_fingerprint != baseline.config_fingerprint
        || request.identity.sqm_fingerprint != baseline.sqm_fingerprint
    {
        return Err(
            "native bootstrap Apply absence baseline does not match the request".to_string(),
        );
    }
    Ok(())
}

fn validate_source_apply(source: &NativeApplyExecutionPlan) -> Result<(), String> {
    source.validate_exact_invariants()?;
    source.request.validate()?;
    if source.request.target_state != OperationTargetState::AbsentBootstrap
        || source.request.identity.operation != OperationKind::FullAutotune
        || source.request.origin != OperationOrigin::Luci
        || source.request.scheduled_auto_apply_requested
        || !matches!(
            (source.authority_identity()?, source.action),
            (
                NativeApplyAuthorityIdentity::ShapedV4(_),
                NativeApplyAction::ApplySqm,
            ) | (
                NativeApplyAuthorityIdentity::RawFallbackV5(_),
                NativeApplyAction::DisableSqm,
            )
        )
    {
        return Err(
            "native bootstrap Apply requires a matching manual LuCI absent Full Auto-Tune selection"
                .to_string(),
        );
    }
    Ok(())
}

fn composite_candidate_id(
    request_sha256: &str,
    source: &NativeApplyAuthorityIdentity,
    policy_sha256: &str,
    managed_config_sha256: &str,
    action_count: u16,
    absent_baseline: &AbsentRuntimeBaseline,
) -> String {
    let (domain, source_key) = match source {
        NativeApplyAuthorityIdentity::ShapedV4(_) => (
            BOOTSTRAP_CANDIDATE_DOMAIN_V3,
            "source_apply_v4_manifest_sha256",
        ),
        NativeApplyAuthorityIdentity::RawFallbackV5(_) => (
            RAW_FALLBACK_BOOTSTRAP_CANDIDATE_DOMAIN_V4,
            "source_apply_v5_manifest_sha256",
        ),
        NativeApplyAuthorityIdentity::DirectionalRawFallbackV6(_) => {
            unreachable!("directional raw fallback is rejected by bootstrap construction")
        }
    };
    let seed = format!(
        concat!(
            "{{\"domain\":{},\"request_sha256\":{},\"{}\":{},",
            "\"source_review_sha256\":{},\"source_candidate_id\":{},",
            "\"bootstrap_policy_sha256\":{},\"managed_config_sha256\":{},",
            "\"managed_config_action_count\":{},\"target_ifindex\":{},",
            "\"kernel_topology_fingerprint\":{},\"kernel_namespace_seed\":{}}}"
        ),
        json_string(domain),
        json_string(request_sha256),
        source_key,
        json_string(source.manifest_sha256()),
        json_string(source.source_review_sha256()),
        json_string(source.candidate_id()),
        json_string(policy_sha256),
        json_string(managed_config_sha256),
        action_count,
        absent_baseline.target_ifindex,
        json_string(&absent_baseline.kernel_topology_fingerprint),
        json_string(&absent_baseline.kernel_namespace_seed),
    );
    sha256_hex(seed.as_bytes())
}

fn server_persistence_name(value: NativeBootstrapServerPersistence) -> &'static str {
    match value {
        NativeBootstrapServerPersistence::LeaveAutomatic => "leave_automatic",
    }
}

fn canonical_json_object<'a>(label: &str, bytes: &'a [u8]) -> Result<&'a str, String> {
    validate_canonical_json_line(label, bytes)?;
    let text = std::str::from_utf8(bytes).map_err(|_| format!("{label} is not UTF-8"))?;
    text.strip_suffix('\n')
        .ok_or_else(|| format!("{label} has no canonical terminator"))
}

fn validate_canonical_json_line(label: &str, bytes: &[u8]) -> Result<(), String> {
    let text = std::str::from_utf8(bytes).map_err(|_| format!("{label} is not UTF-8"))?;
    if !text.starts_with('{')
        || !text.ends_with("}\n")
        || text.matches('\n').count() != 1
        || text.contains(['\r', '\0'])
    {
        return Err(format!("{label} is not one canonical JSON line"));
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex_lower(digest(&SHA256, bytes).as_ref())
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::autotune::{
        build_proposal_for_profile_with_context, AccessEvidenceSource, AccessMedium,
        AutotuneProfile, AutotuneProposal, CapacityLearningPolicy, LatencyBaseline, LinkKind,
        ProposalContext,
    };
    use crate::operations::autotune_apply::{
        NativeApplyAcknowledgement, NativeApplyArtifactDigests, NativeApplyDirectionInput,
        NativeApplyDirectionMode, NativeApplyManifestInput, NativeRawFallbackApplyManifestInput,
        NativeSqmDirectionMode,
    };
    use crate::operations::protocol::{
        CalibrationStrategy, OperationIdentity, OperationRouteIdentity, OperationRouteMode,
    };
    use std::net::{IpAddr, Ipv4Addr};

    const REVIEW_DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[derive(Clone, Copy)]
    enum Topology {
        Both,
        DownloadOnly,
        UploadOnly,
        Off,
    }

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
            created_unix_ms: 1_000,
            deadline_unix_ms: 2_000,
            origin: OperationOrigin::Luci,
            backend: "speedtest-go".to_string(),
            speedtest_direction: None,
            speedtest_server_id: Some(17_372),
            speedtest_topology: None,
            route: OperationRouteIdentity {
                mode: OperationRouteMode::Main,
                mwan3_member: None,
                l3_device: "pppoe-wan".to_string(),
                source_ip: Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2))),
                fwmark: None,
                routing_table: None,
            },
            target_state: OperationTargetState::AbsentBootstrap,
            capture_policy: Some(
                crate::operations::autotune_capture_policy::AutotuneCapturePolicyId::StandardV1,
            ),
            managed_sqm_section: Some("cake_wan_sqm".to_string()),
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
        let mut value = build_proposal_for_profile_with_context(
            &[900_000.0, 910_000.0, 920_000.0],
            &[90_000.0, 95_000.0, 100_000.0],
            LatencyBaseline {
                median_ms: 10.0,
                p95_ms: 12.0,
                samples: 10,
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
        .unwrap();
        value
            .set_tested_safe_maximums(Some(value.download.base_kbps), Some(value.upload.base_kbps))
            .unwrap();
        value
    }

    fn source_apply(request: &OperationRequest, topology: Topology) -> NativeApplyExecutionPlan {
        let proposal = proposal();
        if matches!(topology, Topology::Off) {
            return NativeApplyExecutionPlan::from_verified_raw_fallback(
                NativeRawFallbackApplyManifestInput {
                    option_id: "safe_candidate",
                    request,
                    worker_run_id: &"66".repeat(16),
                    review_digest: REVIEW_DIGEST,
                    coordinator_boot_id: "boot-id",
                    coordinator_generation: &"77".repeat(16),
                    proposal: &proposal,
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
        let direction = |mode: NativeApplyDirectionMode,
                         proposal: crate::autotune::DirectionProposal| {
            NativeApplyDirectionInput {
                mode,
                selected_kbps: (mode == NativeApplyDirectionMode::Shaped)
                    .then_some(proposal.base_kbps),
                measured_runtime_minimum_kbps: None,
                proposal,
            }
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
            download: direction(download_mode, proposal.download),
            upload: direction(upload_mode, proposal.upload),
            proposal: &proposal,
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

    fn policy(percent: u8) -> NativeBootstrapApplyPolicy {
        let request = request();
        NativeBootstrapApplyPolicy::new(
            &request,
            percent,
            NativeBootstrapServerPersistence::LeaveAutomatic,
        )
        .unwrap()
    }

    fn absent_baseline(request: &OperationRequest) -> AbsentRuntimeBaseline {
        AbsentRuntimeBaseline {
            planned_sqm_section: request.managed_sqm_section.clone().unwrap(),
            target_interface: request.identity.target_interface.clone(),
            target_ifindex: 17,
            route_fingerprint: request.identity.route_fingerprint.clone(),
            config_fingerprint: request.identity.config_fingerprint.clone(),
            sqm_fingerprint: request.identity.sqm_fingerprint.clone(),
            kernel_topology_fingerprint: "dd".repeat(32),
            kernel_namespace_seed: "ee".repeat(16),
        }
    }

    fn bootstrap_plan(
        source: NativeApplyExecutionPlan,
        policy: NativeBootstrapApplyPolicy,
    ) -> Result<NativeBootstrapApplyPlan, String> {
        let baseline = absent_baseline(&source.request);
        NativeBootstrapApplyPlan::from_verified_source(source, policy, baseline)
    }

    pub(crate) fn fixture_plan() -> NativeBootstrapApplyPlan {
        bootstrap_plan(source_apply(&request(), Topology::Both), policy(90)).unwrap()
    }

    pub(crate) fn fixture_raw_fallback_plan() -> NativeBootstrapApplyPlan {
        bootstrap_plan(source_apply(&request(), Topology::Off), policy(90)).unwrap()
    }

    #[test]
    fn v7_manifest_binds_capture_namespace_policy_config_and_frozen_v4_identity() {
        let source = source_apply(&request(), Topology::Both);
        let source_v4 = source.canonical_manifest_bytes().unwrap();
        assert_eq!(source_v4.len(), 11_171);
        assert_eq!(
            sha256_hex(&source_v4),
            "fa6d15cd64adbca95b6be077b5afd219b88cd7d391c9755dc42c865e45d5d6e6"
        );
        let mut existing_request = request();
        existing_request.target_state = OperationTargetState::ExistingManaged;
        existing_request.capture_policy = None;
        assert_eq!(
            source_apply(&existing_request, Topology::Both)
                .canonical_manifest_bytes()
                .unwrap(),
            source_v4,
            "the frozen schema-v4 artifact must remain identical for its existing-managed authority"
        );
        let plan = bootstrap_plan(source, policy(42)).unwrap();
        let bytes = plan.canonical_manifest_bytes().unwrap();
        let repeated = plan.canonical_manifest_bytes().unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert_eq!(bytes.len(), 10_757);
        assert_eq!(
            sha256_hex(&bytes),
            "4921f4393188c528ab3dfa581919f99b0b0c3bfe1356727d9f6fb2d6250d2e1f"
        );

        assert_eq!(bytes, repeated);
        assert!(bytes.len() < MAX_NATIVE_BOOTSTRAP_APPLY_MANIFEST_BYTES);
        assert!(text.starts_with("{\"native_apply_manifest_schema_version\":7,"));
        assert!(text.contains("\"target_state\":\"absent_bootstrap\""));
        assert!(text.contains("\"source_request\":{\"schema_version\":6"));
        assert!(text.contains("\"source_apply_v4\":{\"schema_version\":4"));
        assert!(text.contains("\"native_bootstrap_apply_policy_schema_version\":2"));
        assert!(text.contains("\"speedtest_apply_percent\":42"));
        assert!(text.contains("\"capture_policy\":{\"id\":\"standard_v1\""));
        assert!(text.contains(
            "\"option\":\"reflector\",\"kind\":\"replace_list\",\"values\":[\"1.1.1.1\",\"1.0.0.1\""
        ));
        assert!(text.contains("\"native_managed_config_schema_version\":1"));
        assert!(text.contains("\"absence_baseline\":{\"target_ifindex\":17"));
        assert!(text.contains(&format!(
            "\"kernel_topology_fingerprint\":\"{}\"",
            "dd".repeat(32)
        )));
        assert!(text.contains(&format!(
            "\"kernel_namespace_seed\":\"{}\"",
            "ee".repeat(16)
        )));
        assert_eq!(plan.identity.managed_config_action_count, 125);
        assert!(usize::from(plan.identity.managed_config_action_count) > 96);
        assert!(
            usize::from(plan.identity.managed_config_action_count)
                <= MAX_NATIVE_BOOTSTRAP_APPLY_UCI_MUTATIONS
        );
        assert_eq!(
            plan.identity.source_apply_v4_manifest_sha256,
            "fa6d15cd64adbca95b6be077b5afd219b88cd7d391c9755dc42c865e45d5d6e6"
        );
        assert_eq!(
            plan.canonical_manifest_sha256().unwrap(),
            sha256_hex(&bytes)
        );
    }

    #[test]
    fn policy_request_and_config_identity_drift_are_all_distinct() {
        let source = source_apply(&request(), Topology::Both);
        let a = bootstrap_plan(source.clone(), policy(90)).unwrap();
        let percent = bootstrap_plan(source, policy(42)).unwrap();
        let mut changed_request = request();
        changed_request.identity.config_fingerprint = "ab".repeat(32);
        let request_drift =
            bootstrap_plan(source_apply(&changed_request, Topology::Both), policy(90)).unwrap();
        let namespace_source = source_apply(&request(), Topology::Both);
        let mut namespace_baseline = absent_baseline(&namespace_source.request);
        namespace_baseline.kernel_namespace_seed = "ff".repeat(16);
        let namespace_drift = NativeBootstrapApplyPlan::from_verified_source(
            namespace_source,
            policy(90),
            namespace_baseline,
        )
        .unwrap();

        for other in [&percent, &request_drift, &namespace_drift] {
            assert_ne!(
                a.identity.composite_candidate_id,
                other.identity.composite_candidate_id
            );
            assert_ne!(
                a.canonical_manifest_bytes().unwrap(),
                other.canonical_manifest_bytes().unwrap()
            );
        }
        assert_ne!(
            a.identity.bootstrap_policy_sha256,
            percent.identity.bootstrap_policy_sha256
        );
        assert_ne!(
            a.identity.request_sha256,
            request_drift.identity.request_sha256
        );

        let mut forged_policy = policy(90);
        forged_policy.capture_policy_sha256 = "ff".repeat(32);
        assert!(
            bootstrap_plan(source_apply(&request(), Topology::Both), forged_policy)
                .unwrap_err()
                .contains("capture policy identity")
        );
    }

    #[test]
    fn topology_matrix_is_bounded_and_non_sqm_or_non_absent_sources_fail_closed() {
        for topology in [Topology::Both, Topology::DownloadOnly, Topology::UploadOnly] {
            let plan = bootstrap_plan(source_apply(&request(), topology), policy(90)).unwrap();
            assert!(plan.managed_config.action_count() <= MAX_NATIVE_BOOTSTRAP_APPLY_UCI_MUTATIONS);
            assert!(
                plan.canonical_manifest_bytes().unwrap().len()
                    <= MAX_NATIVE_BOOTSTRAP_APPLY_MANIFEST_BYTES
            );
        }

        let off = bootstrap_plan(source_apply(&request(), Topology::Off), policy(90)).unwrap();
        assert_eq!(
            off.manifest_schema_version(),
            NATIVE_RAW_FALLBACK_BOOTSTRAP_APPLY_MANIFEST_SCHEMA_VERSION
        );
        assert!(off.managed_config().sqm().is_none());
        let off_manifest = String::from_utf8(off.canonical_manifest_bytes().unwrap()).unwrap();
        assert!(off_manifest.starts_with("{\"native_apply_manifest_schema_version\":8,"));
        assert!(off_manifest.contains("\"source_apply_v5\":{\"schema_version\":5"));
        assert!(off_manifest.contains("\"native_managed_config_schema_version\":2"));
        assert!(!off_manifest.contains("\"package\":\"sqm\""));

        let mut existing = request();
        existing.target_state = OperationTargetState::ExistingManaged;
        existing.capture_policy = None;
        let existing =
            bootstrap_plan(source_apply(&existing, Topology::Both), policy(90)).unwrap_err();
        assert!(existing.contains("matching manual LuCI absent Full Auto-Tune selection"));
    }

    #[test]
    fn tampered_cached_identity_never_serializes() {
        let mut plan =
            bootstrap_plan(source_apply(&request(), Topology::Both), policy(90)).unwrap();
        plan.identity.managed_config_action_count = 129;
        assert!(plan
            .canonical_manifest_bytes()
            .unwrap_err()
            .contains("managed-config identity changed"));

        let mut plan =
            bootstrap_plan(source_apply(&request(), Topology::Both), policy(90)).unwrap();
        plan.identity.source_review_sha256 = "de".repeat(32);
        assert!(plan
            .canonical_manifest_bytes()
            .unwrap_err()
            .contains("source identity changed"));

        let mut plan =
            bootstrap_plan(source_apply(&request(), Topology::Both), policy(90)).unwrap();
        plan.absent_baseline.kernel_topology_fingerprint = "ef".repeat(32);
        assert!(plan
            .canonical_manifest_bytes()
            .unwrap_err()
            .contains("absence identity changed"));

        let mut plan =
            bootstrap_plan(source_apply(&request(), Topology::Both), policy(90)).unwrap();
        plan.absent_baseline.kernel_namespace_seed = "ff".repeat(16);
        assert!(plan
            .canonical_manifest_bytes()
            .unwrap_err()
            .contains("absence identity changed"));
    }

    #[test]
    fn absence_baseline_must_match_every_request_binding() {
        let source = source_apply(&request(), Topology::Both);
        let mut baseline = absent_baseline(&source.request);
        baseline.planned_sqm_section = "foreign_sqm".to_string();
        assert!(NativeBootstrapApplyPlan::from_verified_source(
            source.clone(),
            policy(90),
            baseline,
        )
        .unwrap_err()
        .contains("does not match the request"));

        let mut baseline = absent_baseline(&source.request);
        baseline.kernel_topology_fingerprint = "not-a-digest".to_string();
        assert!(
            NativeBootstrapApplyPlan::from_verified_source(source, policy(90), baseline,)
                .unwrap_err()
                .contains("kernel topology fingerprint")
        );

        let source = source_apply(&request(), Topology::Both);
        let mut baseline = absent_baseline(&source.request);
        baseline.kernel_namespace_seed = "not-a-seed".to_string();
        assert!(
            NativeBootstrapApplyPlan::from_verified_source(source, policy(90), baseline,)
                .unwrap_err()
                .contains("kernel namespace seed")
        );
    }

    #[test]
    fn mutated_schema_v4_source_is_never_promoted_to_bootstrap_authority() {
        let bootstrap_policy = || policy(90);

        let mut changed_after_construction = source_apply(&request(), Topology::Both);
        changed_after_construction
            .request
            .identity
            .config_fingerprint = "de".repeat(32);
        assert!(
            bootstrap_plan(changed_after_construction, bootstrap_policy(),)
                .unwrap_err()
                .contains("changed after exact construction")
        );

        let mut invalid_review = source_apply(&request(), Topology::Both);
        invalid_review.review_digest = "not-a-sha256".to_string();
        assert!(bootstrap_plan(invalid_review, bootstrap_policy(),)
            .unwrap_err()
            .contains("changed after exact construction"));

        let mut contradictory_acknowledgements = source_apply(&request(), Topology::Both);
        contradictory_acknowledgements
            .required_acknowledgements
            .clear();
        assert!(
            bootstrap_plan(contradictory_acknowledgements, bootstrap_policy(),)
                .unwrap_err()
                .contains("changed after exact construction")
        );

        let mut changed_uci_projection = source_apply(&request(), Topology::Both);
        changed_uci_projection.uci_mutations.pop().unwrap();
        assert!(bootstrap_plan(changed_uci_projection, bootstrap_policy(),)
            .unwrap_err()
            .contains("changed after exact construction"));
    }
}
