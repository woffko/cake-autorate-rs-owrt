//! Non-secret launch intent -> fully attested native Full Auto-Tune request.
//!
//! LuCI is allowed to choose policy, but it never supplies capability tokens
//! or fingerprints.  Those values are generated from the live router state in
//! this Rust process immediately before the immutable request is submitted to
//! the coordinator.

use super::autotune_capture_policy::AutotuneCapturePolicyId;
use super::identity::{read_kernel_uuid, DEFAULT_RANDOM_UUID_PATH};
use super::launch_route::{
    launch_route_spec, match_launch_route, ExplicitLaunchRoute, LaunchRouteFields,
};
use super::protocol::{
    AutotuneTrafficPlan, CalibrationStrategy, OperationIdentity, OperationKind, OperationOrigin,
    OperationRequest, OperationRouteIdentity, OperationRouteMode, OperationTargetState,
    TrafficPolicy,
};
use super::rating::epoch_ms;
use super::sqm_identity::{
    attest_bootstrap_uci_absence, managed_autotune_config_fingerprint,
    managed_sqm_identity_fingerprint, sha256sum, BootstrapAbsenceIdentity,
};
use crate::autotune::{
    AccessEvidenceSource, AccessMedium, AutotuneProfile, CapacityLearningPolicy,
};
use crate::routing::{
    inspect_route, ExplicitRouteAuthority, RouteIdentity, RouteSnapshot, RouteSpec,
};
use crate::Config;
use std::net::IpAddr;
use std::path::Path;

const NATIVE_AUTOTUNE_DEADLINE_MS: u64 = 45 * 60 * 1_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutotuneLaunchIntent {
    pub(crate) explicit_route: Option<ExplicitLaunchRoute>,
    pub instance: String,
    pub expected_target_interface: String,
    pub backend: String,
    pub route_mode: String,
    pub mwan3_member: String,
    pub profile: AutotuneProfile,
    pub strategy: CalibrationStrategy,
    pub access_medium: AccessMedium,
    pub access_source: AccessEvidenceSource,
    pub access_confidence_percent: u8,
    pub capacity_learning_policy: CapacityLearningPolicy,
    pub service_dl_cap_kbps: Option<u64>,
    pub service_ul_cap_kbps: Option<u64>,
    pub allow_sqm_disable: bool,
    pub allow_active_traffic: bool,
    pub traffic_budget: TrafficPolicy,
    pub traffic_policy_explicit: bool,
    pub traffic_plan: Option<AutotuneTrafficPlan>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LiveRequestContext {
    pub target_interface: String,
    pub managed_sqm_section: String,
    pub configured_speedtest_backend: String,
    pub configured_dl_bound_kbps: Option<u64>,
    pub configured_ul_bound_kbps: Option<u64>,
    pub unshaped_dl_bound_kbps: Option<u64>,
    pub unshaped_ul_bound_kbps: Option<u64>,
    pub route: OperationRouteIdentity,
    pub route_fingerprint: String,
    pub config_fingerprint: String,
    pub sqm_fingerprint: String,
}

/// Route and UCI-only absence authority captured for a dormant new-instance
/// request.  The duplicated binding fields are intentional: the pure builder
/// rechecks every value copied into the immutable request and rejects drift.
/// This context is not an admission or kernel-topology proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BootstrapRequestContext {
    pub explicit_route: Option<ExplicitLaunchRoute>,
    pub target_interface: String,
    pub planned_sqm_section: String,
    pub route_identity: RouteIdentity,
    pub route_fingerprint: String,
    pub config_fingerprint: String,
    pub sqm_fingerprint: String,
    pub absence_identity: BootstrapAbsenceIdentity,
}

/// Parse a deliberately small flag vocabulary. Unknown, duplicate, empty, or
/// secret-looking fields fail closed instead of being ignored.
pub fn parse_launch_intent<I>(args: I) -> Result<AutotuneLaunchIntent, String>
where
    I: Iterator<Item = String>,
{
    let mut instance = None;
    let mut expected_target_interface = None;
    let mut backend = None;
    let mut route_mode = None;
    let mut mwan3_member = None;
    let mut explicit_route = LaunchRouteFields::default();
    let mut profile = None;
    let mut strategy = None;
    let mut access_medium = None;
    let mut access_source = None;
    let mut access_confidence_percent = None;
    let mut capacity_learning_policy = None;
    let mut service_dl_cap_kbps = None;
    let mut service_ul_cap_kbps = None;
    let mut allow_sqm_disable = false;
    let mut allow_active_traffic = false;
    let mut traffic_budget = None;
    let mut traffic_policy = None;
    let mut planning_dl_kbps = None;
    let mut planning_ul_kbps = None;
    let mut args = args.peekable();

    while let Some(flag) = args.next() {
        let value = |args: &mut std::iter::Peekable<I>| {
            args.next()
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("{flag} requires a non-empty value"))
        };
        match flag.as_str() {
            "--instance" if instance.is_none() => instance = Some(value(&mut args)?),
            "--expected-target" if expected_target_interface.is_none() => {
                expected_target_interface = Some(value(&mut args)?)
            }
            "--backend" if backend.is_none() => backend = Some(value(&mut args)?),
            "--route-mode" if route_mode.is_none() => route_mode = Some(value(&mut args)?),
            "--mwan3-member" if mwan3_member.is_none() => mwan3_member = Some(value(&mut args)?),
            "--route-source-ipv4"
            | "--route-table"
            | "--route-fwmark"
            | "--route-fwmark-mask"
            | "--route-dns-ipv4" => {
                explicit_route.set(&flag, value(&mut args)?)?;
            }
            "--profile" if profile.is_none() => {
                let raw = value(&mut args)?;
                profile = Some(
                    AutotuneProfile::parse(&raw)
                        .ok_or_else(|| "unsupported native Auto-Tune profile".to_string())?,
                );
            }
            "--strategy" if strategy.is_none() => {
                let raw = value(&mut args)?;
                strategy = Some(
                    CalibrationStrategy::parse(&raw)
                        .ok_or_else(|| "unsupported native calibration strategy".to_string())?,
                );
            }
            "--access-medium" if access_medium.is_none() => {
                let raw = value(&mut args)?;
                access_medium = Some(
                    AccessMedium::parse(&raw)
                        .ok_or_else(|| "unsupported native access medium".to_string())?,
                );
            }
            "--access-source" if access_source.is_none() => {
                let raw = value(&mut args)?;
                access_source = Some(
                    AccessEvidenceSource::parse(&raw)
                        .ok_or_else(|| "unsupported native access evidence source".to_string())?,
                );
            }
            "--access-confidence-percent" if access_confidence_percent.is_none() => {
                let raw = value(&mut args)?;
                let parsed = raw
                    .parse::<u8>()
                    .map_err(|_| "access confidence must be an integer".to_string())?;
                if parsed > 100 {
                    return Err("access confidence must be between 0 and 100".to_string());
                }
                access_confidence_percent = Some(parsed);
            }
            "--capacity-learning-policy" if capacity_learning_policy.is_none() => {
                let raw = value(&mut args)?;
                capacity_learning_policy = Some(
                    CapacityLearningPolicy::parse(&raw)
                        .ok_or_else(|| "unsupported capacity learning policy".to_string())?,
                );
            }
            "--service-dl-cap-kbps" if service_dl_cap_kbps.is_none() => {
                service_dl_cap_kbps = Some(parse_positive_u64(&value(&mut args)?, &flag)?);
            }
            "--service-ul-cap-kbps" if service_ul_cap_kbps.is_none() => {
                service_ul_cap_kbps = Some(parse_positive_u64(&value(&mut args)?, &flag)?);
            }
            "--planning-dl-kbps" if planning_dl_kbps.is_none() => {
                planning_dl_kbps = Some(parse_positive_u64(&value(&mut args)?, &flag)?);
            }
            "--planning-ul-kbps" if planning_ul_kbps.is_none() => {
                planning_ul_kbps = Some(parse_positive_u64(&value(&mut args)?, &flag)?);
            }
            "--traffic-budget-bytes" if traffic_budget.is_none() => {
                traffic_budget = Some(parse_positive_u64(&value(&mut args)?, &flag)?);
            }
            "--traffic-policy" if traffic_policy.is_none() => {
                let policy = value(&mut args)?;
                if !matches!(policy.as_str(), "capped" | "unlimited") {
                    return Err("--traffic-policy must be capped or unlimited".into());
                }
                traffic_policy = Some(policy);
            }
            "--allow-sqm-disable" if !allow_sqm_disable => allow_sqm_disable = true,
            "--allow-active-traffic" if !allow_active_traffic => allow_active_traffic = true,
            _ => {
                return Err(format!(
                    "unsupported or duplicate native launch option: {flag}"
                ))
            }
        }
    }

    let explicit_policy = traffic_policy.is_some();
    let instance = instance.ok_or_else(|| "--instance is required".to_string())?;
    let expected_target_interface =
        expected_target_interface.ok_or_else(|| "--expected-target is required".to_string())?;
    let backend = backend.ok_or_else(|| "--backend is required".to_string())?;
    let route_mode = route_mode.ok_or_else(|| "--route-mode is required".to_string())?;
    let intent = AutotuneLaunchIntent {
        explicit_route: explicit_route.finish(&route_mode)?,
        instance,
        expected_target_interface,
        backend,
        route_mode,
        mwan3_member: mwan3_member.unwrap_or_default(),
        profile: profile.ok_or_else(|| "--profile is required".to_string())?,
        strategy: strategy.ok_or_else(|| "--strategy is required".to_string())?,
        access_medium: access_medium.ok_or_else(|| "--access-medium is required".to_string())?,
        access_source: access_source.ok_or_else(|| "--access-source is required".to_string())?,
        access_confidence_percent: access_confidence_percent
            .ok_or_else(|| "--access-confidence-percent is required".to_string())?,
        capacity_learning_policy: capacity_learning_policy
            .ok_or_else(|| "--capacity-learning-policy is required".to_string())?,
        service_dl_cap_kbps,
        service_ul_cap_kbps,
        allow_sqm_disable,
        allow_active_traffic,
        traffic_budget: match traffic_policy.as_deref() {
            Some("unlimited") if traffic_budget.is_none() => TrafficPolicy::Unlimited,
            Some("unlimited") => {
                return Err("unlimited policy must not also set --traffic-budget-bytes".into())
            }
            _ => TrafficPolicy::from(
                traffic_budget.ok_or("--traffic-budget-bytes is required for capped traffic")?,
            ),
        },
        traffic_policy_explicit: explicit_policy,
        traffic_plan: match (planning_dl_kbps, planning_ul_kbps) {
            (None, None) => None,
            (Some(download_kbps), Some(upload_kbps)) => Some(AutotuneTrafficPlan {
                download_kbps,
                upload_kbps,
            }),
            _ => return Err("both planning download and upload rates are required".into()),
        },
    };
    validate_intent(&intent)?;
    Ok(intent)
}

pub fn build_live_autotune_request(
    intent: &AutotuneLaunchIntent,
) -> Result<OperationRequest, String> {
    build_live_autotune_request_for_origin(intent, OperationOrigin::Luci, false)
}

/// Build a live-attested Full Auto-Tune request for a UCI-absent instance.
/// Service caps are copied only as user/policy authority seeds; this request
/// does not synthesize measured throughput evidence.  Coordinator admission,
/// runtime mutation and Apply each independently re-attest the same bound
/// UCI, route and kernel absence before crossing their mutation boundary.
pub fn build_bootstrap_autotune_request(
    intent: &AutotuneLaunchIntent,
    planned_sqm_section: &str,
) -> Result<OperationRequest, String> {
    validate_intent(intent)?;
    let context = attest_bootstrap_launch_context(intent, planned_sqm_section)?;
    let created_unix_ms = epoch_ms()?;
    let job_id = read_kernel_uuid(Path::new(DEFAULT_RANDOM_UUID_PATH), "native job ID")?;
    let token_a = read_kernel_uuid(Path::new(DEFAULT_RANDOM_UUID_PATH), "native job token")?;
    let token_b = read_kernel_uuid(Path::new(DEFAULT_RANDOM_UUID_PATH), "native job token")?;
    build_bootstrap_request(
        intent,
        context,
        job_id,
        format!("{token_a}{token_b}"),
        created_unix_ms,
    )
}

/// Build the same live-attested request for the trusted native scheduler.
///
/// The origin is deliberately selected by a separate Rust entry point rather
/// than parsed from LuCI or shell arguments.  This prevents an untrusted caller
/// from relabelling a manual request as scheduled work while allowing the
/// coordinator to apply its manual-over-scheduled priority policy.
pub fn build_live_scheduled_autotune_request(
    intent: &AutotuneLaunchIntent,
    auto_apply_requested: bool,
) -> Result<OperationRequest, String> {
    build_live_autotune_request_for_origin(intent, OperationOrigin::Scheduler, auto_apply_requested)
}

fn build_live_autotune_request_for_origin(
    intent: &AutotuneLaunchIntent,
    origin: OperationOrigin,
    scheduled_auto_apply_requested: bool,
) -> Result<OperationRequest, String> {
    validate_intent(intent)?;
    let context = attest_launch_context(intent)?;
    let created_unix_ms = epoch_ms()?;
    let job_id = read_kernel_uuid(Path::new(DEFAULT_RANDOM_UUID_PATH), "native job ID")?;
    let token_a = read_kernel_uuid(Path::new(DEFAULT_RANDOM_UUID_PATH), "native job token")?;
    let token_b = read_kernel_uuid(Path::new(DEFAULT_RANDOM_UUID_PATH), "native job token")?;
    build_request(
        intent,
        context,
        job_id,
        format!("{token_a}{token_b}"),
        created_unix_ms,
        origin,
        scheduled_auto_apply_requested,
    )
}

fn build_request(
    intent: &AutotuneLaunchIntent,
    context: LiveRequestContext,
    job_id: String,
    job_token: String,
    created_unix_ms: u64,
    origin: OperationOrigin,
    scheduled_auto_apply_requested: bool,
) -> Result<OperationRequest, String> {
    let traffic_plan = launch_traffic_plan(
        intent,
        origin,
        context.configured_dl_bound_kbps,
        context.configured_ul_bound_kbps,
    )?;
    match_launch_route(
        &intent.route_mode,
        &intent.mwan3_member,
        &intent.expected_target_interface,
        intent.explicit_route.as_ref(),
        &context.route,
    )?;
    let deadline_unix_ms = created_unix_ms
        .checked_add(NATIVE_AUTOTUNE_DEADLINE_MS)
        .ok_or_else(|| "native Auto-Tune deadline overflow".to_string())?;
    let request = OperationRequest {
        identity: OperationIdentity {
            job_id,
            job_token,
            instance: intent.instance.clone(),
            operation: OperationKind::FullAutotune,
            target_interface: context.target_interface,
            route_fingerprint: context.route_fingerprint,
            config_fingerprint: context.config_fingerprint,
            sqm_fingerprint: context.sqm_fingerprint,
        },
        created_unix_ms,
        deadline_unix_ms,
        origin,
        backend: intent.backend.clone(),
        speedtest_direction: None,
        speedtest_server_id: None,
        speedtest_topology: None,
        route: context.route,
        target_state: OperationTargetState::ExistingManaged,
        capture_policy: None,
        managed_sqm_section: Some(context.managed_sqm_section),
        profile: Some(intent.profile),
        strategy: Some(intent.strategy),
        access_medium: Some(intent.access_medium),
        access_source: Some(intent.access_source),
        access_confidence_percent: intent.access_confidence_percent,
        capacity_learning_policy: Some(intent.capacity_learning_policy),
        service_dl_cap_kbps: intent.service_dl_cap_kbps,
        service_ul_cap_kbps: intent.service_ul_cap_kbps,
        allow_sqm_disable: intent.allow_sqm_disable,
        allow_active_traffic: intent.allow_active_traffic,
        scheduled_auto_apply_requested,
        traffic_budget: intent.traffic_budget,
        traffic_policy_explicit: intent.traffic_policy_explicit,
        traffic_plan,
    };
    request.validate()?;
    Ok(request)
}

fn build_bootstrap_request(
    intent: &AutotuneLaunchIntent,
    context: BootstrapRequestContext,
    job_id: String,
    job_token: String,
    created_unix_ms: u64,
) -> Result<OperationRequest, String> {
    validate_intent(intent)?;
    let traffic_plan = launch_traffic_plan(intent, OperationOrigin::Luci, None, None)?;
    let route = validate_bootstrap_request_context(intent, &context)?;
    let deadline_unix_ms = created_unix_ms
        .checked_add(NATIVE_AUTOTUNE_DEADLINE_MS)
        .ok_or_else(|| "native bootstrap Auto-Tune deadline overflow".to_string())?;
    let request = OperationRequest {
        identity: OperationIdentity {
            job_id,
            job_token,
            instance: intent.instance.clone(),
            operation: OperationKind::FullAutotune,
            target_interface: context.target_interface,
            route_fingerprint: context.route_fingerprint,
            config_fingerprint: context.config_fingerprint,
            sqm_fingerprint: context.sqm_fingerprint,
        },
        created_unix_ms,
        deadline_unix_ms,
        origin: OperationOrigin::Luci,
        backend: intent.backend.clone(),
        speedtest_direction: None,
        speedtest_server_id: None,
        speedtest_topology: None,
        route,
        target_state: OperationTargetState::AbsentBootstrap,
        capture_policy: Some(AutotuneCapturePolicyId::StandardV2),
        managed_sqm_section: Some(context.planned_sqm_section),
        profile: Some(intent.profile),
        strategy: Some(intent.strategy),
        access_medium: Some(intent.access_medium),
        access_source: Some(intent.access_source),
        access_confidence_percent: intent.access_confidence_percent,
        capacity_learning_policy: Some(intent.capacity_learning_policy),
        service_dl_cap_kbps: intent.service_dl_cap_kbps,
        service_ul_cap_kbps: intent.service_ul_cap_kbps,
        allow_sqm_disable: intent.allow_sqm_disable,
        allow_active_traffic: intent.allow_active_traffic,
        scheduled_auto_apply_requested: false,
        traffic_budget: intent.traffic_budget,
        traffic_policy_explicit: intent.traffic_policy_explicit,
        traffic_plan,
    };
    request.validate()?;
    context.absence_identity.ensure_request_binding(
        &request.identity.instance,
        request.managed_sqm_section.as_deref().unwrap_or_default(),
        &request.identity.target_interface,
        &request.identity.route_fingerprint,
        &request.identity.config_fingerprint,
        &request.identity.sqm_fingerprint,
    )?;
    Ok(request)
}

fn attest_bootstrap_launch_context(
    intent: &AutotuneLaunchIntent,
    planned_sqm_section: &str,
) -> Result<BootstrapRequestContext, String> {
    attest_bootstrap_operation_context(
        &intent.instance,
        &intent.expected_target_interface,
        &intent.route_mode,
        &intent.mwan3_member,
        planned_sqm_section,
        intent.explicit_route.as_ref(),
    )
}

pub(crate) fn attest_bootstrap_operation_context(
    instance: &str,
    expected_target_interface: &str,
    route_mode: &str,
    mwan3_member: &str,
    planned_sqm_section: &str,
    explicit_route: Option<&ExplicitLaunchRoute>,
) -> Result<BootstrapRequestContext, String> {
    let route_spec = launch_route_spec(
        route_mode,
        mwan3_member,
        expected_target_interface,
        explicit_route,
    )?;
    let snapshot = if let Some(selected) = explicit_route {
        // Read-only attestation does not grant producer or coordinator admission.
        let observation = selected
            .authority
            .observe_system(expected_target_interface)?;
        RouteSnapshot {
            identity: observation.identity,
            online: true,
            active: true,
            member_status: String::new(),
            reason: String::new(),
        }
    } else {
        inspect_route(&route_spec)?
    };
    if !snapshot.online {
        return Err(format!("selected route is not online: {}", snapshot.reason));
    }
    let route_fingerprint = sha256sum(snapshot.stable_key().as_bytes())?;
    let absence_identity = attest_bootstrap_uci_absence(
        instance,
        planned_sqm_section,
        expected_target_interface,
        &route_fingerprint,
    );
    bootstrap_context_from_operation_attestation(
        instance,
        expected_target_interface,
        route_mode,
        mwan3_member,
        planned_sqm_section,
        snapshot.identity,
        route_fingerprint,
        absence_identity,
        explicit_route,
    )
}

#[cfg(test)]
fn bootstrap_context_from_attestation(
    intent: &AutotuneLaunchIntent,
    planned_sqm_section: &str,
    route_identity: RouteIdentity,
    route_fingerprint: String,
    absence_identity: Result<BootstrapAbsenceIdentity, String>,
) -> Result<BootstrapRequestContext, String> {
    bootstrap_context_from_operation_attestation(
        &intent.instance,
        &intent.expected_target_interface,
        &intent.route_mode,
        &intent.mwan3_member,
        planned_sqm_section,
        route_identity,
        route_fingerprint,
        absence_identity,
        intent.explicit_route.as_ref(),
    )
}

#[allow(clippy::too_many_arguments)]
fn bootstrap_context_from_operation_attestation(
    instance: &str,
    expected_target_interface: &str,
    route_mode: &str,
    mwan3_member: &str,
    planned_sqm_section: &str,
    route_identity: RouteIdentity,
    route_fingerprint: String,
    absence_identity: Result<BootstrapAbsenceIdentity, String>,
    explicit_route: Option<&ExplicitLaunchRoute>,
) -> Result<BootstrapRequestContext, String> {
    let absence_identity = absence_identity
        .map_err(|error| format!("bootstrap UCI absence witness is unavailable: {error}"))?;
    let context = BootstrapRequestContext {
        explicit_route: explicit_route.cloned(),
        target_interface: expected_target_interface.to_string(),
        planned_sqm_section: planned_sqm_section.to_string(),
        route_identity,
        route_fingerprint,
        config_fingerprint: absence_identity.config_fingerprint().to_string(),
        sqm_fingerprint: absence_identity.sqm_fingerprint().to_string(),
        absence_identity,
    };
    validate_bootstrap_operation_context(
        instance,
        expected_target_interface,
        route_mode,
        mwan3_member,
        explicit_route,
        &context,
    )?;
    Ok(context)
}

fn validate_bootstrap_request_context(
    intent: &AutotuneLaunchIntent,
    context: &BootstrapRequestContext,
) -> Result<OperationRouteIdentity, String> {
    validate_bootstrap_operation_context(
        &intent.instance,
        &intent.expected_target_interface,
        &intent.route_mode,
        &intent.mwan3_member,
        intent.explicit_route.as_ref(),
        context,
    )
}

pub(crate) fn validate_bootstrap_operation_context(
    instance: &str,
    expected_target_interface: &str,
    route_mode: &str,
    mwan3_member: &str,
    explicit_route: Option<&ExplicitLaunchRoute>,
    context: &BootstrapRequestContext,
) -> Result<OperationRouteIdentity, String> {
    if context.target_interface != expected_target_interface {
        return Err("bootstrap target changed after absence attestation".to_string());
    }
    if context.route_identity.device != context.target_interface {
        return Err("bootstrap route no longer resolves to the target interface".to_string());
    }
    if explicit_route != context.explicit_route.as_ref() {
        return Err("bootstrap route selection changed after attestation".into());
    }
    let mut route = operation_route_identity(&context.route_identity)?;
    route.dns_server = context
        .explicit_route
        .as_ref()
        .map(|selected| selected.dns_server);
    match_launch_route(
        route_mode,
        mwan3_member,
        expected_target_interface,
        explicit_route,
        &route,
    )
    .map_err(|error| format!("selected bootstrap route changed: {error}"))?;
    let expected_route_fingerprint = sha256sum(context.route_identity.stable_key().as_bytes())?;
    if expected_route_fingerprint != context.route_fingerprint {
        return Err("bootstrap route fingerprint changed after attestation".to_string());
    }
    context.absence_identity.ensure_request_binding(
        instance,
        &context.planned_sqm_section,
        &context.target_interface,
        &context.route_fingerprint,
        &context.config_fingerprint,
        &context.sqm_fingerprint,
    )?;
    Ok(route)
}

fn attest_launch_context(intent: &AutotuneLaunchIntent) -> Result<LiveRequestContext, String> {
    attest_live_operation_context(
        &intent.instance,
        &intent.expected_target_interface,
        &intent.route_mode,
        &intent.mwan3_member,
    )
}

/// Build the immutable live interface, route, configuration and SQM identity
/// shared by native operations. Policy fields and operation capabilities are
/// deliberately added by each operation-specific request builder afterwards.
pub(crate) fn attest_live_operation_context(
    instance: &str,
    expected_target_interface: &str,
    route_mode: &str,
    mwan3_member: &str,
) -> Result<LiveRequestContext, String> {
    let cfg = Config::from_uci(instance)?;
    if cfg.sqm_interface != expected_target_interface {
        return Err("configured SQM target changed after the launch dialog opened".to_string());
    }
    if !cfg.enabled
        || !cfg.manage_sqm
        || !cfg.sqm_enabled
        || (!cfg.download_shaping_enabled() && !cfg.upload_shaping_enabled())
    {
        return Err(
            "native operation requires a running instance with at least one managed CAKE direction"
                .to_string(),
        );
    }
    let snapshot = if route_mode == "explicit" {
        if cfg.route_mode != "explicit" || !mwan3_member.is_empty() || !cfg.mwan3_member.is_empty()
        {
            return Err("explicit launch selection differs from configured route".into());
        }
        let authority = cfg
            .explicit_route_authority
            .as_ref()
            .ok_or("configured explicit route authority missing")?;
        let observation = authority.observe_system(&cfg.sqm_interface)?;
        RouteSnapshot {
            identity: observation.identity,
            online: true,
            active: true,
            member_status: String::new(),
            reason: String::new(),
        }
    } else {
        let route_spec = RouteSpec::new(route_mode, mwan3_member, &cfg.sqm_interface);
        inspect_route(&route_spec)?
    };
    if !snapshot.online {
        return Err(format!("selected route is not online: {}", snapshot.reason));
    }
    let mut route = operation_route_identity(&snapshot.identity)?;
    route.dns_server = cfg.explicit_dns_server;
    operation_route_matches_instance(&cfg, &route).map_err(|error| {
        format!("selected route does not match the instance probe route: {error}")
    })?;
    let route_fingerprint = sha256sum(snapshot.stable_key().as_bytes())?;
    let config_fingerprint = managed_autotune_config_fingerprint(instance, &cfg.sqm_section)?;
    let sqm_fingerprint =
        managed_sqm_identity_fingerprint(instance, &cfg.sqm_section, &cfg.sqm_interface)?;
    super::service_lifecycle::attest_operation_applied_sqm(instance, &cfg, &sqm_fingerprint)?;
    let rate_bound = |value: f64| {
        if value.is_finite() && value > 0.0 && value <= u64::MAX as f64 {
            Some(value.ceil() as u64)
        } else {
            None
        }
    };
    let configured_dl_bound_kbps = rate_bound(cfg.max_dl_shaper_rate_kbps);
    let configured_ul_bound_kbps = rate_bound(cfg.max_ul_shaper_rate_kbps);
    let unshaped_dl_bound_kbps = rate_bound(
        cfg.max_dl_shaper_rate_kbps
            .max(cfg.adaptive_ceiling_dl_cap_kbps),
    );
    let unshaped_ul_bound_kbps = rate_bound(
        cfg.max_ul_shaper_rate_kbps
            .max(cfg.adaptive_ceiling_ul_cap_kbps),
    );
    Ok(LiveRequestContext {
        target_interface: cfg.sqm_interface,
        managed_sqm_section: cfg.sqm_section,
        configured_speedtest_backend: cfg.speedtest_backend,
        configured_dl_bound_kbps,
        configured_ul_bound_kbps,
        unshaped_dl_bound_kbps,
        unshaped_ul_bound_kbps,
        route,
        route_fingerprint,
        config_fingerprint,
        sqm_fingerprint,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ConfiguredRouteSelection {
    mode: OperationRouteMode,
    mwan3_member: Option<String>,
}

fn configured_route_selection(
    configured_mode: &str,
    configured_member: &str,
) -> Result<ConfiguredRouteSelection, String> {
    RouteSpec::new(configured_mode, configured_member, "route-check").validate()?;
    match configured_mode {
        "main" => Ok(ConfiguredRouteSelection {
            mode: OperationRouteMode::Main,
            mwan3_member: None,
        }),
        "mwan3" => Ok(ConfiguredRouteSelection {
            mode: OperationRouteMode::Mwan3,
            mwan3_member: Some(configured_member.to_string()),
        }),
        "auto" if configured_member.is_empty() => Ok(ConfiguredRouteSelection {
            mode: OperationRouteMode::Main,
            mwan3_member: None,
        }),
        "auto" => Ok(ConfiguredRouteSelection {
            mode: OperationRouteMode::Mwan3,
            mwan3_member: Some(configured_member.to_string()),
        }),
        _ => unreachable!("RouteSpec validation rejected unsupported route mode"),
    }
}

/// Complete configured authority check for existing instances. The legacy
/// mode/member-only check intentionally remains insufficient for explicit PBR.
pub(crate) fn operation_route_matches_instance(
    cfg: &Config,
    route: &OperationRouteIdentity,
) -> Result<(), String> {
    if cfg.route_mode != "explicit" {
        if cfg.explicit_route_authority.is_some()
            || cfg.explicit_dns_server.is_some()
            || route.dns_server.is_some()
        {
            return Err("explicit authority conflicts with configured route mode".into());
        }
        return operation_route_matches_config(&cfg.route_mode, &cfg.mwan3_member, route);
    }
    if !cfg.mwan3_member.is_empty()
        || route.l3_device != cfg.sqm_interface
        || route.l3_device != cfg.ul_if
    {
        return Err("configured explicit route target or member differs".into());
    }
    let actual = explicit_operation_authority(route)?;
    if cfg.explicit_route_authority.as_ref() != Some(&actual)
        || cfg.explicit_dns_server != route.dns_server
    {
        return Err("configured explicit route authority differs from requested route".into());
    }
    Ok(())
}

pub(crate) fn explicit_operation_authority(
    route: &OperationRouteIdentity,
) -> Result<ExplicitRouteAuthority, String> {
    route.validate()?;
    if route.mode != OperationRouteMode::Explicit {
        return Err("explicit route authority requires explicit mode".into());
    }
    let source = route
        .source_ip
        .ok_or("explicit route source missing")?
        .to_string();
    let table = route
        .routing_table
        .ok_or("explicit route table missing")?
        .to_string();
    let mark = route
        .fwmark
        .ok_or("explicit route mark missing")?
        .to_string();
    let mask = route
        .fwmark_mask
        .ok_or("explicit route mask missing")?
        .to_string();
    ExplicitRouteAuthority::from_fields("explicit", [&source, &table, &mark, &mask])?
        .ok_or_else(|| "explicit route authority missing".into())
}

pub(crate) fn operation_route_matches_config(
    configured_mode: &str,
    configured_member: &str,
    route: &OperationRouteIdentity,
) -> Result<(), String> {
    let configured = configured_route_selection(configured_mode, configured_member)?;
    if route.mode != configured.mode || route.mwan3_member != configured.mwan3_member {
        return Err(format!(
            "configured {} route differs from requested {} route",
            configured.mode.as_str(),
            route.mode.as_str()
        ));
    }
    Ok(())
}

pub(crate) fn operation_route_identity(
    identity: &RouteIdentity,
) -> Result<OperationRouteIdentity, String> {
    // Do not silently discard a new link witness into the legacy wire schemas.
    // Explicit request support must deliberately propagate/attest it first.
    if identity.device_ifindex.is_some() && identity.mode != "explicit" {
        return Err("link-qualified route requests are not available in this build".into());
    }
    let source_ip = identity
        .source_ip
        .parse::<IpAddr>()
        .map_err(|_| "live route has no valid source IP".to_string())?;
    match identity.mode.as_str() {
        "explicit" => {
            if !identity.member.is_empty() {
                return Err("explicit route must not carry an mwan3 member".into());
            }
            let route = OperationRouteIdentity {
                dns_server: None,
                device_ifindex: identity.device_ifindex,
                mode: OperationRouteMode::Explicit,
                mwan3_member: None,
                l3_device: identity.device.clone(),
                source_ip: Some(source_ip),
                fwmark: Some(parse_u32_auto(&identity.fwmark, "explicit fwmark")?),
                routing_table: Some(parse_u32_auto(&identity.table, "explicit routing table")?),
                fwmark_mask: identity.fwmark_mask,
            };
            route.validate()?;
            Ok(route)
        }
        "main" => {
            if !identity.member.is_empty()
                || !identity.fwmark.is_empty()
                || identity.fwmark_mask.is_some()
                || identity.table != "main"
            {
                return Err("main route identity carries policy-routing fields".to_string());
            }
            Ok(OperationRouteIdentity {
                dns_server: None,
                device_ifindex: None,
                mode: OperationRouteMode::Main,
                mwan3_member: None,
                l3_device: identity.device.clone(),
                source_ip: Some(source_ip),
                fwmark: None,
                routing_table: None,
                fwmark_mask: None,
            })
        }
        "mwan3" => Ok(OperationRouteIdentity {
            dns_server: None,
            device_ifindex: None,
            mode: OperationRouteMode::Mwan3,
            mwan3_member: Some(identity.member.clone()),
            l3_device: identity.device.clone(),
            source_ip: Some(source_ip),
            fwmark: Some(parse_u32_auto(&identity.fwmark, "mwan3 fwmark")?),
            routing_table: Some(parse_u32_auto(&identity.table, "mwan3 routing table")?),
            fwmark_mask: identity.fwmark_mask,
        }),
        _ => Err("live route mode is unsupported".to_string()),
    }
}

/// Fill a planning assumption for new interactive capped launches only. These
/// configured hints are not a capacity measurement and never change a shaper.
fn launch_traffic_plan(
    intent: &AutotuneLaunchIntent,
    origin: OperationOrigin,
    configured_dl_kbps: Option<u64>,
    configured_ul_kbps: Option<u64>,
) -> Result<Option<AutotuneTrafficPlan>, String> {
    let mut plan = intent.traffic_plan;
    if plan.is_none()
        && intent.traffic_policy_explicit
        && intent.traffic_budget != TrafficPolicy::Unlimited
        && origin != OperationOrigin::Scheduler
    {
        if let (Some(download_kbps), Some(upload_kbps)) = (
            intent.service_dl_cap_kbps.max(configured_dl_kbps),
            intent.service_ul_cap_kbps.max(configured_ul_kbps),
        ) {
            plan = Some(AutotuneTrafficPlan {
                download_kbps,
                upload_kbps,
            });
        }
    }
    if let Some(plan) = plan {
        super::full_autotune::validate_autotune_planning_budget(intent.traffic_budget, plan)
            .map_err(|error| error.message)?;
    }
    Ok(plan)
}

fn validate_intent(intent: &AutotuneLaunchIntent) -> Result<(), String> {
    if intent.backend != "speedtest-go" {
        return Err("native Full Auto-Tune currently requires speedtest-go".to_string());
    }
    launch_route_spec(
        &intent.route_mode,
        &intent.mwan3_member,
        &intent.expected_target_interface,
        intent.explicit_route.as_ref(),
    )?;
    intent.traffic_budget.validate()?;
    if let Some(plan) = intent.traffic_plan {
        plan.validate()?;
        if !intent.traffic_policy_explicit {
            return Err("planning rates require explicit traffic policy".into());
        }
        super::full_autotune::validate_autotune_planning_budget(intent.traffic_budget, plan)
            .map_err(|error| error.message)?;
    }
    if intent.traffic_budget.is_empty() {
        return Err("native Full Auto-Tune requires a non-zero traffic budget".to_string());
    }
    if intent.traffic_budget == TrafficPolicy::Unlimited && !intent.traffic_policy_explicit {
        return Err("unlimited launch requires explicit policy".into());
    }
    crate::autotune::validate_capacity_learning_service_caps(
        Some(intent.capacity_learning_policy),
        intent.service_dl_cap_kbps,
        intent.service_ul_cap_kbps,
    )?;
    super::full_autotune::validate_autotune_traffic_admission(
        intent.traffic_budget,
        Some(intent.strategy),
        intent.allow_sqm_disable,
        intent.service_dl_cap_kbps,
        intent.service_ul_cap_kbps,
    )
    .map_err(|error| error.message)?;
    Ok(())
}

fn parse_positive_u64(value: &str, label: &str) -> Result<u64, String> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| format!("{label} must be an unsigned integer"))?;
    if parsed == 0 {
        return Err(format!("{label} must be positive"));
    }
    Ok(parsed)
}

fn parse_u32_auto(value: &str, label: &str) -> Result<u32, String> {
    if let Some(hex) = value.strip_prefix("0x") {
        u32::from_str_radix(hex, 16).map_err(|_| format!("{label} is invalid"))
    } else {
        value
            .parse::<u32>()
            .map_err(|_| format!("{label} is invalid"))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn r6_explicit_instance_requires_full_configured_authority() {
        let route = super::OperationRouteIdentity {
            dns_server: None,
            device_ifindex: Some(42),
            mode: super::OperationRouteMode::Explicit,
            mwan3_member: None,
            l3_device: "eth1".into(),
            source_ip: Some("192.0.2.2".parse().unwrap()),
            fwmark: Some(0x100),
            routing_table: Some(101),
            fwmark_mask: Some(0x3f00),
        };
        let mut cfg = crate::Config::defaults("r6-explicit".into());
        cfg.route_mode = "explicit".into();
        cfg.mwan3_member.clear();
        cfg.sqm_interface = "eth1".into();
        cfg.ul_if = "eth1".into();
        cfg.explicit_route_authority = Some(super::explicit_operation_authority(&route).unwrap());
        assert!(super::operation_route_matches_instance(&cfg, &route).is_ok());
        // Mode/member-only and bootstrap callers must not grant explicit authority.
        assert!(super::operation_route_matches_config("explicit", "", &route).is_err());
        for variant in 0..9 {
            let mut changed = cfg.clone();
            match variant {
                0 => changed.route_mode = "main".into(),
                1 => changed.mwan3_member = "wan".into(),
                2 => changed.sqm_interface = "eth2".into(),
                3 => changed.ul_if = "eth2".into(),
                4 => changed.explicit_route_authority = None,
                _ => {
                    let mut fields = ["192.0.2.2", "101", "256", "16128"];
                    fields[variant - 5] = ["192.0.2.3", "102", "512", "65280"][variant - 5];
                    changed.explicit_route_authority =
                        super::ExplicitRouteAuthority::from_fields("explicit", fields).unwrap();
                }
            }
            assert!(super::operation_route_matches_instance(&changed, &route).is_err());
        }
        let mut legacy = cfg;
        legacy.route_mode = "main".into();
        legacy.explicit_route_authority = None;
        assert!(super::operation_route_matches_instance(&legacy, &route).is_err());
    }

    use super::*;

    fn args<'a>(values: &'a [&'a str]) -> impl Iterator<Item = String> + 'a {
        values.iter().map(|value| (*value).to_string())
    }

    fn intent_args() -> Vec<String> {
        args(&[
            "--instance",
            "wan_sqm",
            "--expected-target",
            "pppoe-wan",
            "--backend",
            "speedtest-go",
            "--route-mode",
            "mwan3",
            "--mwan3-member",
            "wan",
            "--profile",
            "variable_link",
            "--strategy",
            "full_raw",
            "--access-medium",
            "cellular",
            "--access-source",
            "user_selected",
            "--access-confidence-percent",
            "100",
            "--capacity-learning-policy",
            "scheduled_active",
            "--service-dl-cap-kbps",
            "1000000",
            "--service-ul-cap-kbps",
            "500000",
            "--traffic-budget-bytes",
            "25000000000",
            "--allow-sqm-disable",
            "--allow-active-traffic",
        ])
        .collect()
    }

    fn intent() -> AutotuneLaunchIntent {
        parse_launch_intent(intent_args().into_iter()).unwrap()
    }

    #[test]
    fn t2_planning_defaults_are_bound_only_to_new_interactive_capped_requests() {
        let mut value = intent();
        assert_eq!(
            launch_traffic_plan(&value, OperationOrigin::Luci, Some(100), Some(100)).unwrap(),
            None
        );
        value.traffic_policy_explicit = true;
        let plan = launch_traffic_plan(&value, OperationOrigin::Luci, Some(100), Some(100))
            .unwrap()
            .unwrap();
        assert_eq!(plan.download_kbps, 1_000_000);
        assert_eq!(plan.upload_kbps, 500_000);
        value.service_dl_cap_kbps = None;
        value.service_ul_cap_kbps = None;
        assert_eq!(
            launch_traffic_plan(&value, OperationOrigin::Luci, None, Some(100)).unwrap(),
            None
        );
        assert_eq!(
            launch_traffic_plan(&value, OperationOrigin::Scheduler, Some(100), Some(100)).unwrap(),
            None
        );
        value.traffic_budget = 1_000_000_000_u64.into();
        assert!(launch_traffic_plan(
            &value,
            OperationOrigin::Luci,
            Some(1_000_000),
            Some(100_000)
        )
        .is_err());
        value.traffic_plan = Some(AutotuneTrafficPlan {
            download_kbps: 1000,
            upload_kbps: 1000,
        });
        assert_eq!(
            launch_traffic_plan(
                &value,
                OperationOrigin::Luci,
                Some(1_000_000),
                Some(100_000)
            )
            .unwrap(),
            value.traffic_plan
        );
        value.traffic_plan = None;
        value.traffic_budget = TrafficPolicy::Unlimited;
        assert_eq!(
            launch_traffic_plan(
                &value,
                OperationOrigin::Luci,
                Some(1_000_000),
                Some(100_000)
            )
            .unwrap(),
            None
        );
    }

    #[test]
    fn t2_planning_flags_are_paired_explicit_and_checked_before_attestation() {
        let mut values = intent_args();
        values.extend(args(&[
            "--traffic-policy",
            "capped",
            "--planning-dl-kbps",
            "1000000",
            "--planning-ul-kbps",
            "100000",
        ]));
        let planned = parse_launch_intent(values.clone().into_iter()).unwrap();
        assert_eq!(
            planned.traffic_plan,
            Some(AutotuneTrafficPlan {
                download_kbps: 1_000_000,
                upload_kbps: 100_000
            })
        );
        let budget = values
            .iter()
            .position(|value| value == "--traffic-budget-bytes")
            .unwrap()
            + 1;
        values[budget] = "1000000000".into();
        assert!(parse_launch_intent(values.clone().into_iter())
            .unwrap_err()
            .contains("initial-stage planning allowance"));
        values[budget] = "25000000000".into();
        values.truncate(values.len() - 2);
        assert!(parse_launch_intent(values.into_iter())
            .unwrap_err()
            .contains("both planning"));
        let mut legacy = intent();
        legacy.traffic_plan = planned.traffic_plan;
        assert!(validate_intent(&legacy)
            .unwrap_err()
            .contains("explicit traffic policy"));
    }

    #[test]
    fn t2_raw_launch_requires_stop_authority_before_live_attestation() {
        let mut intent = intent();
        intent.capacity_learning_policy = CapacityLearningPolicy::VerifiedOnly;
        intent.service_dl_cap_kbps = None;
        intent.service_ul_cap_kbps = None;
        for explicit in [false, true] {
            intent.traffic_policy_explicit = explicit;
            assert!(validate_intent(&intent)
                .unwrap_err()
                .starts_with("traffic-stop-authority-unavailable:"));
        }
        intent.traffic_budget = TrafficPolicy::Unlimited;
        validate_intent(&intent).unwrap();
        intent.traffic_budget = 25_000_000_000_u64.into();
        intent.strategy = CalibrationStrategy::ShapedOnly;
        intent.allow_sqm_disable = false;
        validate_intent(&intent).unwrap();
    }

    #[test]
    fn t2_raw_launch_reserve_is_part_of_total_before_live_attestation() {
        let mut intent = intent();
        intent.traffic_budget = 100_000_000_u64.into();
        let error = validate_intent(&intent).unwrap_err();
        assert!(error.contains("236210008 bytes total DL+UL"));
        assert!(error.contains("235423576 bytes reserved for stopping"));
        intent.traffic_budget = 236_210_008_u64.into();
        validate_intent(&intent).unwrap();
    }

    fn bootstrap_route_identity() -> RouteIdentity {
        RouteIdentity {
            device_ifindex: None,
            fwmark_mask: None,
            mode: "mwan3".to_string(),
            member: "wan".to_string(),
            device: "pppoe-wan".to_string(),
            source_ip: "192.0.2.2".to_string(),
            fwmark: "0x100".to_string(),
            table: "1".to_string(),
        }
    }

    fn bootstrap_context(
        intent: &AutotuneLaunchIntent,
        planned_sqm_section: &str,
    ) -> BootstrapRequestContext {
        let mut route_identity = bootstrap_route_identity();
        if intent.explicit_route.is_some() {
            route_identity.mode = "explicit".into();
            route_identity.member.clear();
            route_identity.table = "101".into();
            route_identity.device_ifindex = Some(7);
            route_identity.fwmark_mask = Some(0x3f00);
        }
        let route_fingerprint = sha256sum(route_identity.stable_key().as_bytes()).unwrap();
        let absence_identity = BootstrapAbsenceIdentity::from_raw(
            &intent.instance,
            planned_sqm_section,
            &intent.expected_target_interface,
            &route_fingerprint,
            b"cake-autorate.globals=globals\n",
            b"",
        );
        bootstrap_context_from_attestation(
            intent,
            planned_sqm_section,
            route_identity,
            route_fingerprint,
            absence_identity,
        )
        .unwrap()
    }

    fn build_test_bootstrap_request(
        intent: &AutotuneLaunchIntent,
        context: BootstrapRequestContext,
    ) -> Result<OperationRequest, String> {
        build_bootstrap_request(intent, context, "4".repeat(32), "5".repeat(64), 1_000)
    }

    #[test]
    fn r6_autotune_launch_propagates_bootstrap_authority_without_admission() {
        let mut argv = intent_args();
        let mode = argv.iter().position(|arg| arg == "--route-mode").unwrap();
        argv[mode + 1] = "explicit".into();
        let member = argv.iter().position(|arg| arg == "--mwan3-member").unwrap();
        argv.drain(member..member + 2);
        argv.extend(args(&[
            "--route-source-ipv4",
            "192.0.2.2",
            "--route-table",
            "101",
            "--route-fwmark",
            "0x100",
            "--route-fwmark-mask",
            "0x3f00",
            "--route-dns-ipv4",
            "192.0.2.53",
        ]));
        let intent = parse_launch_intent(argv.clone().into_iter()).unwrap();
        let context = bootstrap_context(&intent, "cake_wan_sqm");
        let request = build_test_bootstrap_request(&intent, context.clone()).unwrap();
        assert_eq!(
            request.route.dns_server,
            Some("192.0.2.53".parse().unwrap())
        );
        assert_eq!(request.route.device_ifindex, Some(7));
        assert_eq!(request.route.routing_table, Some(101));
        assert!(request
            .validate_admission_policy()
            .unwrap_err()
            .contains("not available"));
        for index in 0..3 {
            let mut changed = context.clone();
            match index {
                0 => {
                    changed.explicit_route.as_mut().unwrap().dns_server =
                        "192.0.2.54".parse().unwrap()
                }
                1 => changed.route_identity.table = "102".into(),
                _ => changed.explicit_route = None,
            }
            assert!(build_test_bootstrap_request(&intent, changed).is_err());
        }
        argv.extend(args(&["--route-dns-ipv4", "192.0.2.54"]));
        assert!(parse_launch_intent(argv.into_iter())
            .unwrap_err()
            .contains("duplicate"));
    }

    #[test]
    fn t2_launch_intent_preserves_explicit_policy_and_rejects_conflicting_limits() {
        let mut capped_args = intent_args();
        capped_args.extend(["--traffic-policy".into(), "capped".into()]);
        let capped = parse_launch_intent(capped_args.clone().into_iter()).unwrap();
        assert!(capped.traffic_policy_explicit);
        assert_eq!(capped.traffic_budget.limit_bytes(), Some(25_000_000_000));
        let mut insufficient = capped_args.clone();
        let budget_index = insufficient
            .iter()
            .position(|value| value == "--traffic-budget-bytes")
            .unwrap()
            + 1;
        insufficient[budget_index] = "1".into();
        let error = parse_launch_intent(insufficient.into_iter()).unwrap_err();
        assert!(error.contains("mandatory server comparison and initial control evidence"));
        assert!(error.contains("1048576"));
        let mut unlimited_args = intent_args();
        let index = unlimited_args
            .iter()
            .position(|value| value == "--traffic-budget-bytes")
            .unwrap();
        unlimited_args.drain(index..index + 2);
        let mut missing_cap = unlimited_args.clone();
        missing_cap.extend(["--traffic-policy".into(), "capped".into()]);
        assert_eq!(
            parse_launch_intent(missing_cap.into_iter()).unwrap_err(),
            "--traffic-budget-bytes is required for capped traffic"
        );
        unlimited_args.extend(["--traffic-policy".into(), "unlimited".into()]);
        let unlimited = parse_launch_intent(unlimited_args.clone().into_iter()).unwrap();
        assert_eq!(unlimited.traffic_budget, TrafficPolicy::Unlimited);
        assert!(unlimited.traffic_policy_explicit);
        let request =
            build_test_bootstrap_request(&unlimited, bootstrap_context(&unlimited, "cake_wan_sqm"))
                .unwrap();
        assert_eq!(request.traffic_budget, TrafficPolicy::Unlimited);
        assert!(request.traffic_policy_explicit);
        assert!(request
            .encode()
            .unwrap()
            .starts_with("cake-autorate-operation\t8\trequest\n"));
        assert_eq!(
            request.deadline_unix_ms - request.created_unix_ms,
            45 * 60 * 1000
        );
        capped_args.extend(["--traffic-policy".into(), "capped".into()]);
        assert!(parse_launch_intent(capped_args.into_iter()).is_err());
        unlimited_args.extend(["--traffic-budget-bytes".into(), "1000".into()]);
        assert!(parse_launch_intent(unlimited_args.into_iter()).is_err());
        let mut unknown = intent_args();
        unknown.extend(["--traffic-policy".into(), "auto".into()]);
        assert!(parse_launch_intent(unknown.into_iter()).is_err());
        assert!(
            !intent().traffic_policy_explicit,
            "old finite CLI stays legacy-compatible"
        );
    }

    #[test]
    fn launch_parser_accepts_policy_only_and_has_no_secret_fields() {
        let parsed = intent();
        assert_eq!(parsed.instance, "wan_sqm");
        assert_eq!(parsed.profile, AutotuneProfile::VariableLink);
        assert_eq!(parsed.strategy, CalibrationStrategy::FullRaw);
        assert!(parsed.allow_sqm_disable);
        assert!(parsed.allow_active_traffic);
    }

    #[test]
    fn launch_parser_rejects_unknown_duplicate_and_secret_injection() {
        for suffix in [
            vec!["--instance", "other"],
            vec!["--job-token", "a"],
            vec!["--route-fingerprint", "a"],
            vec!["--allow-sqm-disable", "--allow-sqm-disable"],
        ] {
            let mut base: Vec<&str> = vec![
                "--instance",
                "wan_sqm",
                "--expected-target",
                "pppoe-wan",
                "--backend",
                "speedtest-go",
                "--route-mode",
                "main",
                "--profile",
                "best_overall",
                "--strategy",
                "shaped_only",
                "--access-medium",
                "shared_wired",
                "--access-source",
                "user_selected",
                "--access-confidence-percent",
                "100",
                "--capacity-learning-policy",
                "verified_only",
                "--traffic-budget-bytes",
                "1000000",
            ];
            base.extend(suffix);
            assert!(parse_launch_intent(args(&base)).is_err());
        }
    }

    #[test]
    fn fixed_cap_launch_requires_both_service_caps_before_live_attestation() {
        let base = [
            "--instance",
            "wan_sqm",
            "--expected-target",
            "pppoe-wan",
            "--backend",
            "speedtest-go",
            "--route-mode",
            "main",
            "--profile",
            "best_overall",
            "--strategy",
            "shaped_only",
            "--access-medium",
            "shared_wired",
            "--access-source",
            "user_selected",
            "--access-confidence-percent",
            "100",
            "--capacity-learning-policy",
            "fixed_cap",
            "--traffic-budget-bytes",
            "1000000",
        ];
        let parse_with = |extra: &[&str]| {
            parse_launch_intent(
                base.iter()
                    .chain(extra.iter())
                    .map(|value| (*value).to_string()),
            )
        };
        let expected = "fixed-cap capacity learning requires download and upload service hard caps";

        assert_eq!(parse_with(&[]).unwrap_err(), expected);
        assert_eq!(
            parse_with(&["--service-dl-cap-kbps", "1000000"]).unwrap_err(),
            expected
        );
        assert_eq!(
            parse_with(&["--service-ul-cap-kbps", "500000"]).unwrap_err(),
            expected
        );
        let caps = [
            "--service-dl-cap-kbps",
            "1000000",
            "--service-ul-cap-kbps",
            "500000",
        ];
        assert!(parse_with(&caps)
            .unwrap_err()
            .contains("traffic allowance is insufficient"));
        let mut sufficient: Vec<String> = base.iter().map(|value| (*value).to_string()).collect();
        let budget = sufficient
            .iter()
            .position(|value| value == "--traffic-budget-bytes")
            .unwrap()
            + 1;
        sufficient[budget] = "2000000".into();
        sufficient.extend(caps.iter().map(|value| (*value).to_string()));
        let valid = parse_launch_intent(sufficient.into_iter()).unwrap();
        assert_eq!(valid.service_dl_cap_kbps, Some(1_000_000));
        assert_eq!(valid.service_ul_cap_kbps, Some(500_000));
    }

    #[test]
    fn route_identity_conversion_is_exact_and_rejects_policy_on_main() {
        let main = RouteIdentity {
            device_ifindex: None,
            fwmark_mask: None,
            mode: "main".to_string(),
            member: String::new(),
            device: "eth0".to_string(),
            source_ip: "192.0.2.2".to_string(),
            fwmark: String::new(),
            table: "main".to_string(),
        };
        let converted = operation_route_identity(&main).unwrap();
        assert_eq!(converted.mode, OperationRouteMode::Main);
        assert_eq!(converted.source_ip.unwrap().to_string(), "192.0.2.2");
        let mut link_qualified = main.clone();
        link_qualified.device_ifindex = Some(42);
        assert!(operation_route_identity(&link_qualified).is_err());
        link_qualified.mode = "explicit".into();
        link_qualified.fwmark = "0x100".into();
        link_qualified.fwmark_mask = Some(0x3f00);
        link_qualified.table = "101".into();
        let explicit = operation_route_identity(&link_qualified).unwrap();
        assert_eq!(explicit.mode, OperationRouteMode::Explicit);
        assert_eq!(explicit.device_ifindex, Some(42));
        assert_eq!(explicit.fwmark_mask, Some(0x3f00));
        assert_eq!(explicit.routing_table, Some(101));
        assert_eq!(explicit.mwan3_member, None);
        let mut poisoned = main;
        poisoned.fwmark = "0x100".to_string();
        assert!(operation_route_identity(&poisoned).is_err());

        let mwan3 = RouteIdentity {
            device_ifindex: None,
            fwmark_mask: Some(0x3f00),
            mode: "mwan3".to_string(),
            member: "wanb".to_string(),
            device: "eth1".to_string(),
            source_ip: "198.51.100.2".to_string(),
            fwmark: "0x200".to_string(),
            table: "2".to_string(),
        };
        let converted = operation_route_identity(&mwan3).unwrap();
        assert_eq!(converted.fwmark, Some(0x200));
        assert_eq!(converted.routing_table, Some(2));
        assert_eq!(converted.fwmark_mask, Some(0x3f00));
    }

    #[test]
    fn configured_route_matching_accepts_equivalent_auto_forms_only() {
        let main = OperationRouteIdentity {
            dns_server: None,
            device_ifindex: None,
            mode: OperationRouteMode::Main,
            mwan3_member: None,
            l3_device: "eth0".to_string(),
            source_ip: Some("192.0.2.2".parse().unwrap()),
            fwmark: None,
            routing_table: None,
            fwmark_mask: None,
        };
        assert!(operation_route_matches_config("main", "", &main).is_ok());
        assert!(operation_route_matches_config("auto", "", &main).is_ok());
        assert!(operation_route_matches_config("mwan3", "", &main).is_err());

        let mwan3 = OperationRouteIdentity {
            dns_server: None,
            device_ifindex: None,
            mode: OperationRouteMode::Mwan3,
            mwan3_member: Some("wanb".to_string()),
            l3_device: "eth1".to_string(),
            source_ip: Some("198.51.100.2".parse().unwrap()),
            fwmark: Some(0x200),
            routing_table: Some(2),
            fwmark_mask: None,
        };
        assert!(operation_route_matches_config("mwan3", "wanb", &mwan3).is_ok());
        assert!(operation_route_matches_config("auto", "wanb", &mwan3).is_ok());
        assert!(operation_route_matches_config("main", "", &mwan3).is_err());
        assert!(operation_route_matches_config("mwan3", "wan", &mwan3).is_err());
        assert!(operation_route_matches_config("auto", "wan", &mwan3).is_err());
    }

    #[test]
    fn pure_builder_preserves_rates_and_keeps_generated_identity_private() {
        let intent = intent();
        let request = build_request(
            &intent,
            LiveRequestContext {
                target_interface: "pppoe-wan".to_string(),
                managed_sqm_section: "wan_sqm".to_string(),
                configured_speedtest_backend: "speedtest-go".to_string(),
                configured_dl_bound_kbps: Some(1_000_000),
                configured_ul_bound_kbps: Some(500_000),
                unshaped_dl_bound_kbps: Some(1_000_000),
                unshaped_ul_bound_kbps: Some(500_000),
                route: OperationRouteIdentity {
                    dns_server: None,
                    device_ifindex: None,
                    mode: OperationRouteMode::Mwan3,
                    mwan3_member: Some("wan".to_string()),
                    l3_device: "pppoe-wan".to_string(),
                    source_ip: Some("192.0.2.2".parse().unwrap()),
                    fwmark: Some(0x100),
                    routing_table: Some(1),
                    fwmark_mask: None,
                },
                route_fingerprint: "1".repeat(64),
                config_fingerprint: "2".repeat(64),
                sqm_fingerprint: "3".repeat(64),
            },
            "4".repeat(32),
            "5".repeat(64),
            1_000,
            OperationOrigin::Luci,
            false,
        )
        .unwrap();
        assert_eq!(request.service_dl_cap_kbps, Some(1_000_000));
        assert_eq!(request.service_ul_cap_kbps, Some(500_000));
        assert_eq!(request.target_state, OperationTargetState::ExistingManaged);
        assert_eq!(request.managed_sqm_section.as_deref(), Some("wan_sqm"));
        assert_eq!(
            request.deadline_unix_ms,
            1_000 + NATIVE_AUTOTUNE_DEADLINE_MS
        );
        let debug = format!("{request:?}");
        assert!(!debug.contains(&"5".repeat(64)));
    }

    #[test]
    fn trusted_builder_origin_is_explicit_and_not_part_of_launch_intent() {
        let intent = intent();
        let context = || LiveRequestContext {
            target_interface: "pppoe-wan".to_string(),
            managed_sqm_section: "wan_sqm".to_string(),
            configured_speedtest_backend: "speedtest-go".to_string(),
            configured_dl_bound_kbps: Some(1_000_000),
            configured_ul_bound_kbps: Some(500_000),
            unshaped_dl_bound_kbps: Some(1_000_000),
            unshaped_ul_bound_kbps: Some(500_000),
            route: OperationRouteIdentity {
                dns_server: None,
                device_ifindex: None,
                mode: OperationRouteMode::Mwan3,
                mwan3_member: Some("wan".to_string()),
                l3_device: "pppoe-wan".to_string(),
                source_ip: Some("192.0.2.2".parse().unwrap()),
                fwmark: Some(0x100),
                routing_table: Some(1),
                fwmark_mask: Some(0x3f00),
            },
            route_fingerprint: "1".repeat(64),
            config_fingerprint: "2".repeat(64),
            sqm_fingerprint: "3".repeat(64),
        };
        let manual = build_request(
            &intent,
            context(),
            "4".repeat(32),
            "5".repeat(64),
            1_000,
            OperationOrigin::Luci,
            false,
        )
        .unwrap();
        let scheduled = build_request(
            &intent,
            context(),
            "6".repeat(32),
            "7".repeat(64),
            1_000,
            OperationOrigin::Scheduler,
            true,
        )
        .unwrap();
        assert_eq!(manual.origin, OperationOrigin::Luci);
        assert_eq!(scheduled.origin, OperationOrigin::Scheduler);
        assert!(!manual.scheduled_auto_apply_requested);
        assert!(scheduled.scheduled_auto_apply_requested);
        for request in [manual, scheduled] {
            assert_eq!(request.route.fwmark_mask, Some(0x3f00));
            assert!(!request.traffic_policy_explicit);
            assert_eq!(
                OperationRequest::decode(&request.encode().unwrap()).unwrap(),
                request
            );
        }
    }

    #[test]
    fn bootstrap_builder_is_luci_only_and_preserves_explicit_authority_seeds() {
        let mut intent = intent();
        intent.capacity_learning_policy = CapacityLearningPolicy::FixedCap;
        let request =
            build_test_bootstrap_request(&intent, bootstrap_context(&intent, "cake_wan_sqm"))
                .unwrap();

        assert_eq!(request.target_state, OperationTargetState::AbsentBootstrap);
        assert_eq!(
            request.capture_policy,
            Some(AutotuneCapturePolicyId::StandardV2)
        );
        assert_eq!(request.origin, OperationOrigin::Luci);
        assert_eq!(request.managed_sqm_section.as_deref(), Some("cake_wan_sqm"));
        assert!(!request.scheduled_auto_apply_requested);
        assert_eq!(request.service_dl_cap_kbps, Some(1_000_000));
        assert_eq!(request.service_ul_cap_kbps, Some(500_000));
        assert_eq!(
            request.capacity_learning_policy,
            Some(CapacityLearningPolicy::FixedCap)
        );
        request.validate().unwrap();
        request.validate_admission_policy().unwrap();
        let mut missing_cap = request.clone();
        missing_cap.service_ul_cap_kbps = None;
        assert!(missing_cap
            .validate_admission_policy()
            .unwrap_err()
            .contains("download and upload service hard caps"));

        let encoded = request.encode().unwrap();
        assert!(encoded.starts_with("cake-autorate-operation\t6\trequest\n"));
        assert!(encoded.contains("target_state=absent_bootstrap\n"));
        assert!(encoded.contains("capture_policy=standard_v2\n"));
        let policy_digest = AutotuneCapturePolicyId::StandardV2
            .canonical_sha256()
            .unwrap();
        assert!(encoded.contains(&format!("capture_policy_sha256={policy_digest}\n")));
        assert!(encoded.contains("service_dl_cap_kbps=1000000\n"));
        assert!(encoded.contains("service_ul_cap_kbps=500000\n"));
        for forbidden_measurement_claim in [
            "measured_capacity",
            "measured_throughput",
            "throughput_reference",
            "observed_rate",
        ] {
            assert!(!encoded.contains(forbidden_measurement_claim));
        }
    }

    #[test]
    fn bootstrap_context_requires_an_available_valid_absence_witness() {
        let intent = intent();
        let route_identity = bootstrap_route_identity();
        let route_fingerprint = sha256sum(route_identity.stable_key().as_bytes()).unwrap();
        let unavailable = bootstrap_context_from_attestation(
            &intent,
            "cake_wan_sqm",
            route_identity.clone(),
            route_fingerprint.clone(),
            Err("uci query failed".to_string()),
        )
        .unwrap_err();
        assert!(unavailable.contains("witness is unavailable"));

        let conflicting = BootstrapAbsenceIdentity::from_raw(
            &intent.instance,
            "cake_wan_sqm",
            &intent.expected_target_interface,
            &route_fingerprint,
            b"cake-autorate.wan_sqm=cake_autorate\n",
            b"",
        );
        assert!(conflicting.is_err());
        assert!(bootstrap_context_from_attestation(
            &intent,
            "cake_wan_sqm",
            route_identity,
            route_fingerprint,
            conflicting,
        )
        .is_err());
    }

    #[test]
    fn bootstrap_builder_rejects_all_identity_route_and_fingerprint_drift() {
        let intent = intent();
        let baseline = bootstrap_context(&intent, "cake_wan_sqm");

        let mut changed_instance = intent.clone();
        changed_instance.instance = "wanb_sqm".to_string();
        assert!(build_test_bootstrap_request(&changed_instance, baseline.clone()).is_err());

        let mut changed_section = baseline.clone();
        changed_section.planned_sqm_section = "cake_wanb_sqm".to_string();
        assert!(build_test_bootstrap_request(&intent, changed_section).is_err());

        let mut changed_target = baseline.clone();
        changed_target.target_interface = "eth2".to_string();
        assert!(build_test_bootstrap_request(&intent, changed_target).is_err());

        let mut changed_route = baseline.clone();
        changed_route.route_identity.source_ip = "192.0.2.3".to_string();
        assert!(build_test_bootstrap_request(&intent, changed_route).is_err());

        let mut changed_route_fingerprint = baseline.clone();
        changed_route_fingerprint.route_fingerprint = "a".repeat(64);
        assert!(build_test_bootstrap_request(&intent, changed_route_fingerprint).is_err());

        let mut changed_config_fingerprint = baseline.clone();
        changed_config_fingerprint.config_fingerprint = "b".repeat(64);
        assert!(build_test_bootstrap_request(&intent, changed_config_fingerprint).is_err());

        let mut changed_sqm_fingerprint = baseline;
        changed_sqm_fingerprint.sqm_fingerprint = "c".repeat(64);
        assert!(build_test_bootstrap_request(&intent, changed_sqm_fingerprint).is_err());
    }

    #[test]
    fn existing_builder_v4_public_wire_and_semantics_remain_stable() {
        let intent = intent();
        let request = build_request(
            &intent,
            LiveRequestContext {
                target_interface: "pppoe-wan".to_string(),
                managed_sqm_section: "wan_sqm".to_string(),
                configured_speedtest_backend: "speedtest-go".to_string(),
                configured_dl_bound_kbps: Some(1_000_000),
                configured_ul_bound_kbps: Some(500_000),
                unshaped_dl_bound_kbps: Some(1_000_000),
                unshaped_ul_bound_kbps: Some(500_000),
                route: OperationRouteIdentity {
                    dns_server: None,
                    device_ifindex: None,
                    mode: OperationRouteMode::Mwan3,
                    mwan3_member: Some("wan".to_string()),
                    l3_device: "pppoe-wan".to_string(),
                    source_ip: Some("192.0.2.2".parse().unwrap()),
                    fwmark: Some(0x100),
                    routing_table: Some(1),
                    fwmark_mask: None,
                },
                route_fingerprint: "1".repeat(64),
                config_fingerprint: "2".repeat(64),
                sqm_fingerprint: "3".repeat(64),
            },
            "4".repeat(32),
            "5".repeat(64),
            1_000,
            OperationOrigin::Luci,
            false,
        )
        .unwrap();

        assert_eq!(request.target_state, OperationTargetState::ExistingManaged);
        let v4 = request.encode_for_test_schema(4).unwrap();
        assert!(v4.starts_with("cake-autorate-operation\t4\trequest\n"));
        assert!(!v4.contains("target_state="));
        assert_eq!(OperationRequest::decode(&v4).unwrap(), request);
    }
}
