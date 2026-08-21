//! Non-secret CLI intent -> live-attested native standalone Speed Test request.
//!
//! Current-topology intent is non-mutating. Unshaped intent is a distinct
//! restore-first runtime request; it never reuses persistent SQM-disable or
//! Auto-Tune Apply authority.

use super::autotune_capture_policy::AutotuneCapturePolicyId;
use super::autotune_request::{
    attest_bootstrap_operation_context, attest_live_operation_context, operation_route_identity,
    BootstrapRequestContext, LiveRequestContext,
};
use super::identity::{read_kernel_uuid, DEFAULT_RANDOM_UUID_PATH};
use super::protocol::{
    OperationIdentity, OperationKind, OperationOrigin, OperationRequest, OperationTargetState,
    SpeedtestDirection, SpeedtestTopology,
};
use super::rating::epoch_ms;
use super::speedtest::minimum_speedtest_traffic_budget_bytes;
use std::path::Path;

const NATIVE_SPEEDTEST_DEADLINE_MS: u64 = 10 * 60 * 1_000;
const NATIVE_SPEEDTEST_BACKEND_RUNTIME_MS: u128 = 180 * 1_000;
const NATIVE_SPEEDTEST_CURRENT_HEADROOM_PERCENT: u128 = 125;
const NATIVE_SPEEDTEST_UNSHAPED_HEADROOM_PERCENT: u128 = 200;
const NATIVE_SPEEDTEST_MAX_TRAFFIC_BUDGET_BYTES: u64 = 256 * 1024 * 1024 * 1024;
const NATIVE_SPEEDTEST_BOOTSTRAP_TRAFFIC_BUDGET_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const _: () = assert!(NATIVE_SPEEDTEST_BACKEND_RUNTIME_MS <= NATIVE_SPEEDTEST_DEADLINE_MS as u128);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeedtestLaunchIntent {
    pub instance: String,
    pub expected_target_interface: String,
    pub backend: String,
    pub direction: SpeedtestDirection,
    pub server_id: Option<u64>,
    pub topology: SpeedtestTopology,
    pub route_mode: String,
    pub mwan3_member: String,
}

/// Parse only user-visible policy fields.  Job identity, fingerprints,
/// deadlines and traffic authority are generated inside the root-owned daemon.
pub fn parse_speedtest_launch_intent<I>(args: I) -> Result<SpeedtestLaunchIntent, String>
where
    I: Iterator<Item = String>,
{
    let mut instance = None;
    let mut expected_target_interface = None;
    let mut backend = None;
    let mut direction = None;
    let mut server_id = None;
    let mut topology = None;
    let mut route_mode = None;
    let mut mwan3_member = None;
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
            "--direction" if direction.is_none() => {
                let raw = value(&mut args)?;
                direction = Some(
                    SpeedtestDirection::parse(&raw)
                        .ok_or_else(|| "unsupported native Speed Test direction".to_string())?,
                );
            }
            "--server-id" if server_id.is_none() => {
                let raw = value(&mut args)?;
                let parsed = raw
                    .parse::<u64>()
                    .map_err(|_| "native Speed Test server ID must be an integer".to_string())?;
                if parsed == 0 {
                    return Err("native Speed Test server ID must be positive".to_string());
                }
                server_id = Some(parsed);
            }
            "--topology" if topology.is_none() => {
                let raw = value(&mut args)?;
                topology = Some(
                    SpeedtestTopology::parse(&raw)
                        .ok_or_else(|| "unsupported native Speed Test topology".to_string())?,
                );
            }
            "--route-mode" if route_mode.is_none() => route_mode = Some(value(&mut args)?),
            "--mwan3-member" if mwan3_member.is_none() => mwan3_member = Some(value(&mut args)?),
            _ => {
                return Err(format!(
                    "unsupported or duplicate native Speed Test option: {flag}"
                ))
            }
        }
    }

    let intent = SpeedtestLaunchIntent {
        instance: instance.ok_or_else(|| "--instance is required".to_string())?,
        expected_target_interface: expected_target_interface
            .ok_or_else(|| "--expected-target is required".to_string())?,
        backend: backend.ok_or_else(|| "--backend is required".to_string())?,
        direction: direction.ok_or_else(|| "--direction is required".to_string())?,
        server_id,
        topology: topology.ok_or_else(|| "--topology is required".to_string())?,
        route_mode: route_mode.ok_or_else(|| "--route-mode is required".to_string())?,
        mwan3_member: mwan3_member.unwrap_or_default(),
    };
    validate_speedtest_intent(&intent)?;
    Ok(intent)
}

pub fn build_live_speedtest_request(
    intent: &SpeedtestLaunchIntent,
) -> Result<OperationRequest, String> {
    validate_speedtest_intent(intent)?;
    let context = attest_live_operation_context(
        &intent.instance,
        &intent.expected_target_interface,
        &intent.route_mode,
        &intent.mwan3_member,
    )?;
    let created_unix_ms = epoch_ms()?;
    let job_id = read_kernel_uuid(
        Path::new(DEFAULT_RANDOM_UUID_PATH),
        "native Speed Test job ID",
    )?;
    let token_a = read_kernel_uuid(
        Path::new(DEFAULT_RANDOM_UUID_PATH),
        "native Speed Test job token",
    )?;
    let token_b = read_kernel_uuid(
        Path::new(DEFAULT_RANDOM_UUID_PATH),
        "native Speed Test job token",
    )?;
    build_speedtest_request(
        intent,
        context,
        job_id,
        format!("{token_a}{token_b}"),
        created_unix_ms,
    )
}

/// Build a read-only unshaped measurement for a UCI-absent instance.  The
/// worker proves that the target remains free of managed UCI and kernel SQM
/// ownership immediately before and after traffic; it never creates an SQM
/// section or runtime override.
pub fn build_bootstrap_speedtest_request(
    intent: &SpeedtestLaunchIntent,
    planned_sqm_section: &str,
) -> Result<OperationRequest, String> {
    validate_speedtest_intent(intent)?;
    if intent.topology != SpeedtestTopology::Unshaped {
        return Err("bootstrap Speed Test requires unshaped topology".to_string());
    }
    let context = attest_bootstrap_operation_context(
        &intent.instance,
        &intent.expected_target_interface,
        &intent.route_mode,
        &intent.mwan3_member,
        planned_sqm_section,
    )?;
    let created_unix_ms = epoch_ms()?;
    let job_id = read_kernel_uuid(
        Path::new(DEFAULT_RANDOM_UUID_PATH),
        "native bootstrap Speed Test job ID",
    )?;
    let token_a = read_kernel_uuid(
        Path::new(DEFAULT_RANDOM_UUID_PATH),
        "native bootstrap Speed Test job token",
    )?;
    let token_b = read_kernel_uuid(
        Path::new(DEFAULT_RANDOM_UUID_PATH),
        "native bootstrap Speed Test job token",
    )?;
    build_bootstrap_speedtest_request_from_context(
        intent,
        context,
        job_id,
        format!("{token_a}{token_b}"),
        created_unix_ms,
    )
}

fn build_bootstrap_speedtest_request_from_context(
    intent: &SpeedtestLaunchIntent,
    context: BootstrapRequestContext,
    job_id: String,
    job_token: String,
    created_unix_ms: u64,
) -> Result<OperationRequest, String> {
    validate_speedtest_intent(intent)?;
    if intent.topology != SpeedtestTopology::Unshaped {
        return Err("bootstrap Speed Test requires unshaped topology".to_string());
    }
    let route = operation_route_identity(&context.route_identity)?;
    let deadline_unix_ms = created_unix_ms
        .checked_add(NATIVE_SPEEDTEST_DEADLINE_MS)
        .ok_or_else(|| "native bootstrap Speed Test deadline overflow".to_string())?;
    let request = OperationRequest {
        identity: OperationIdentity {
            job_id,
            job_token,
            instance: intent.instance.clone(),
            operation: OperationKind::Speedtest,
            target_interface: context.target_interface,
            route_fingerprint: context.route_fingerprint,
            config_fingerprint: context.config_fingerprint,
            sqm_fingerprint: context.sqm_fingerprint,
        },
        created_unix_ms,
        deadline_unix_ms,
        origin: OperationOrigin::Luci,
        backend: intent.backend.clone(),
        speedtest_direction: Some(intent.direction),
        speedtest_server_id: intent.server_id,
        speedtest_topology: Some(intent.topology),
        route,
        target_state: OperationTargetState::AbsentBootstrap,
        capture_policy: Some(AutotuneCapturePolicyId::StandardV2),
        managed_sqm_section: Some(context.planned_sqm_section),
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
        traffic_budget_bytes: NATIVE_SPEEDTEST_BOOTSTRAP_TRAFFIC_BUDGET_BYTES,
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

fn build_speedtest_request(
    intent: &SpeedtestLaunchIntent,
    context: LiveRequestContext,
    job_id: String,
    job_token: String,
    created_unix_ms: u64,
) -> Result<OperationRequest, String> {
    if resolve_native_speedtest_backend(&context.configured_speedtest_backend)
        != Some(intent.backend.as_str())
    {
        return Err(
            "native Speed Test backend does not match the instance configuration".to_string(),
        );
    }
    let deadline_unix_ms = created_unix_ms
        .checked_add(NATIVE_SPEEDTEST_DEADLINE_MS)
        .ok_or_else(|| "native Speed Test deadline overflow".to_string())?;
    let traffic_budget_bytes = native_speedtest_traffic_budget_bytes(intent, &context)?;
    let request = OperationRequest {
        identity: OperationIdentity {
            job_id,
            job_token,
            instance: intent.instance.clone(),
            operation: OperationKind::Speedtest,
            target_interface: context.target_interface,
            route_fingerprint: context.route_fingerprint,
            config_fingerprint: context.config_fingerprint,
            sqm_fingerprint: context.sqm_fingerprint,
        },
        created_unix_ms,
        deadline_unix_ms,
        // The public CLI is the local LuCI RPC boundary, matching Rating and
        // Auto-Tune; it is not a separate remote or scheduler origin.
        origin: OperationOrigin::Luci,
        backend: intent.backend.clone(),
        speedtest_direction: Some(intent.direction),
        speedtest_server_id: intent.server_id,
        speedtest_topology: Some(intent.topology),
        route: context.route,
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
        traffic_budget_bytes,
    };
    request.validate()?;
    Ok(request)
}

/// Resolve the user-facing automatic backend policy before it enters the
/// immutable native request.  Recovery and status identities therefore name
/// the concrete worker implementation, never the mutable selection policy.
pub(crate) fn resolve_native_speedtest_backend(backend: &str) -> Option<&'static str> {
    match backend {
        "auto" | "speedtest-go" => Some("speedtest-go"),
        _ => None,
    }
}

fn native_speedtest_traffic_budget_bytes(
    intent: &SpeedtestLaunchIntent,
    context: &LiveRequestContext,
) -> Result<u64, String> {
    let (download_bound_kbps, upload_bound_kbps, headroom_percent) = match intent.topology {
        SpeedtestTopology::Current => (
            context.configured_dl_bound_kbps,
            context.configured_ul_bound_kbps,
            NATIVE_SPEEDTEST_CURRENT_HEADROOM_PERCENT,
        ),
        SpeedtestTopology::Unshaped => (
            context.unshaped_dl_bound_kbps,
            context.unshaped_ul_bound_kbps,
            NATIVE_SPEEDTEST_UNSHAPED_HEADROOM_PERCENT,
        ),
    };
    let selected_dl = if intent.direction == SpeedtestDirection::Upload {
        0
    } else {
        download_bound_kbps.ok_or_else(|| {
            "native Speed Test has no valid configured download rate authority".to_string()
        })?
    };
    let selected_ul = if intent.direction == SpeedtestDirection::Download {
        0
    } else {
        upload_bound_kbps.ok_or_else(|| {
            "native Speed Test has no valid configured upload rate authority".to_string()
        })?
    };
    let aggregate_kbps = u128::from(selected_dl)
        .checked_add(u128::from(selected_ul))
        .ok_or_else(|| "native Speed Test rate authority overflow".to_string())?;
    let denominator = 8_u128 * 100;
    let transfer_envelope = aggregate_kbps
        .checked_mul(NATIVE_SPEEDTEST_BACKEND_RUNTIME_MS)
        .and_then(|value| value.checked_mul(headroom_percent))
        .and_then(|value| value.checked_add(denominator - 1))
        .map(|value| value / denominator)
        .ok_or_else(|| "native Speed Test traffic budget overflow".to_string())?;
    let minimum =
        minimum_speedtest_traffic_budget_bytes(intent.direction, selected_dl, selected_ul);
    if minimum > NATIVE_SPEEDTEST_MAX_TRAFFIC_BUDGET_BYTES {
        return Err(
            "native Speed Test safety reserve exceeds its absolute traffic cap".to_string(),
        );
    }
    let derived = transfer_envelope.max(u128::from(minimum));
    let bounded = derived.min(u128::from(NATIVE_SPEEDTEST_MAX_TRAFFIC_BUDGET_BYTES));
    u64::try_from(bounded).map_err(|_| "native Speed Test traffic budget overflow".to_string())
}

fn validate_speedtest_intent(intent: &SpeedtestLaunchIntent) -> Result<(), String> {
    if intent.backend != "speedtest-go" {
        return Err("native standalone Speed Test currently requires speedtest-go".to_string());
    }
    match intent.route_mode.as_str() {
        "main" if intent.mwan3_member.is_empty() => {}
        "mwan3" if !intent.mwan3_member.is_empty() => {}
        "main" => return Err("main Speed Test route must not carry an mwan3 member".to_string()),
        "mwan3" => return Err("mwan3 Speed Test route requires a member".to_string()),
        _ => return Err("native Speed Test route mode must be main or mwan3".to_string()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::protocol::{OperationRouteIdentity, OperationRouteMode};
    use super::super::sqm_identity::{sha256sum, BootstrapAbsenceIdentity};
    use super::*;
    use crate::routing::RouteIdentity;
    use std::net::{IpAddr, Ipv4Addr};

    fn intent() -> SpeedtestLaunchIntent {
        SpeedtestLaunchIntent {
            instance: "wan_sqm".to_string(),
            expected_target_interface: "pppoe-wan".to_string(),
            backend: "speedtest-go".to_string(),
            direction: SpeedtestDirection::Both,
            server_id: Some(42),
            topology: SpeedtestTopology::Current,
            route_mode: "main".to_string(),
            mwan3_member: String::new(),
        }
    }

    fn context() -> LiveRequestContext {
        LiveRequestContext {
            target_interface: "pppoe-wan".to_string(),
            managed_sqm_section: "cake_wan_sqm".to_string(),
            configured_speedtest_backend: "speedtest-go".to_string(),
            configured_dl_bound_kbps: Some(1_000_000),
            configured_ul_bound_kbps: Some(500_000),
            unshaped_dl_bound_kbps: Some(1_250_000),
            unshaped_ul_bound_kbps: Some(625_000),
            route: OperationRouteIdentity {
                mode: OperationRouteMode::Main,
                mwan3_member: None,
                l3_device: "pppoe-wan".to_string(),
                source_ip: Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2))),
                fwmark: None,
                routing_table: None,
            },
            route_fingerprint: "a".repeat(64),
            config_fingerprint: "b".repeat(64),
            sqm_fingerprint: "c".repeat(64),
        }
    }

    fn bootstrap_context(intent: &SpeedtestLaunchIntent) -> BootstrapRequestContext {
        let route_identity = RouteIdentity {
            mode: "main".to_string(),
            member: String::new(),
            device: intent.expected_target_interface.clone(),
            source_ip: "192.0.2.2".to_string(),
            fwmark: String::new(),
            table: "main".to_string(),
        };
        let route_fingerprint = sha256sum(route_identity.stable_key().as_bytes()).unwrap();
        let absence_identity = BootstrapAbsenceIdentity::from_raw(
            &intent.instance,
            "cake_wan_sqm",
            &intent.expected_target_interface,
            &route_fingerprint,
            b"cake-autorate.globals=globals\n",
            b"",
        )
        .unwrap();
        BootstrapRequestContext {
            target_interface: intent.expected_target_interface.clone(),
            planned_sqm_section: "cake_wan_sqm".to_string(),
            route_identity,
            route_fingerprint,
            config_fingerprint: absence_identity.config_fingerprint().to_string(),
            sqm_fingerprint: absence_identity.sqm_fingerprint().to_string(),
            absence_identity,
        }
    }

    #[test]
    fn parser_accepts_only_non_secret_current_topology_policy() {
        let parsed = parse_speedtest_launch_intent(
            [
                "--instance",
                "wan_sqm",
                "--expected-target",
                "pppoe-wan",
                "--backend",
                "speedtest-go",
                "--direction",
                "both",
                "--server-id",
                "42",
                "--topology",
                "current",
                "--route-mode",
                "main",
            ]
            .into_iter()
            .map(str::to_string),
        )
        .unwrap();
        assert_eq!(parsed, intent());
        for forbidden in [
            "--job-token",
            "--route-fingerprint",
            "--traffic-budget-bytes",
            "--sqm-bypass",
        ] {
            assert!(parse_speedtest_launch_intent(
                [forbidden, "secret"].into_iter().map(str::to_string)
            )
            .is_err());
        }
    }

    #[test]
    fn parser_rejects_duplicates_unsupported_backend_and_route_ambiguity() {
        let duplicate = [
            "--instance",
            "wan_sqm",
            "--instance",
            "wanb_sqm",
            "--expected-target",
            "pppoe-wan",
            "--backend",
            "speedtest-go",
            "--direction",
            "both",
            "--topology",
            "current",
            "--route-mode",
            "main",
        ];
        assert!(parse_speedtest_launch_intent(duplicate.into_iter().map(str::to_string)).is_err());

        let mut invalid = intent();
        invalid.backend = "auto".to_string();
        assert!(validate_speedtest_intent(&invalid).is_err());
        invalid.backend = "speedtest-go".to_string();
        invalid.route_mode = "mwan3".to_string();
        assert!(validate_speedtest_intent(&invalid).is_err());

        for invalid_args in [
            vec!["--server-id", "0"],
            vec!["--topology", "unshaped"],
            vec!["--unexpected", "value"],
        ] {
            let mut args = vec![
                "--instance",
                "wan_sqm",
                "--expected-target",
                "pppoe-wan",
                "--backend",
                "speedtest-go",
                "--direction",
                "both",
                "--topology",
                "current",
                "--route-mode",
                "main",
            ];
            args.extend(invalid_args);
            assert!(parse_speedtest_launch_intent(args.into_iter().map(str::to_string)).is_err());
        }
    }

    #[test]
    fn parser_preserves_every_direction_and_builder_rejects_deadline_overflow() {
        for direction in ["both", "download", "upload"] {
            let parsed = parse_speedtest_launch_intent(
                [
                    "--instance",
                    "wan_sqm",
                    "--expected-target",
                    "pppoe-wan",
                    "--backend",
                    "speedtest-go",
                    "--direction",
                    direction,
                    "--topology",
                    "current",
                    "--route-mode",
                    "main",
                ]
                .into_iter()
                .map(str::to_string),
            )
            .unwrap();
            let request =
                build_speedtest_request(&parsed, context(), "d".repeat(32), "e".repeat(64), 1_000)
                    .unwrap();
            assert_eq!(request.speedtest_direction, Some(parsed.direction));
        }

        assert!(build_speedtest_request(
            &intent(),
            context(),
            "d".repeat(32),
            "e".repeat(64),
            u64::MAX,
        )
        .unwrap_err()
        .contains("deadline overflow"));
    }

    #[test]
    fn pure_builder_is_current_topology_non_mutating_and_server_bounded() {
        let request =
            build_speedtest_request(&intent(), context(), "d".repeat(32), "e".repeat(64), 1_000)
                .unwrap();
        assert_eq!(request.identity.operation, OperationKind::Speedtest);
        assert_eq!(request.target_state, OperationTargetState::ExistingManaged);
        assert_eq!(request.speedtest_direction, Some(SpeedtestDirection::Both));
        assert_eq!(request.speedtest_topology, Some(SpeedtestTopology::Current));
        assert_eq!(request.speedtest_server_id, Some(42));
        assert!(request.traffic_budget_bytes > 12_000_000_000);
        assert_eq!(
            request.deadline_unix_ms,
            1_000 + NATIVE_SPEEDTEST_DEADLINE_MS
        );
        assert!(request.managed_sqm_section.is_none());
        assert!(!request.allow_sqm_disable);

        let mut unshaped_intent = intent();
        unshaped_intent.topology = SpeedtestTopology::Unshaped;
        let unshaped = build_speedtest_request(
            &unshaped_intent,
            context(),
            "f".repeat(32),
            "0".repeat(64),
            2_000,
        )
        .unwrap();
        assert_eq!(
            unshaped.speedtest_topology,
            Some(SpeedtestTopology::Unshaped)
        );
        assert_eq!(unshaped.target_state, OperationTargetState::ExistingManaged);
        assert!(!unshaped.allow_sqm_disable);
        assert!(unshaped.managed_sqm_section.is_none());
        assert!(unshaped.traffic_budget_bytes > request.traffic_budget_bytes);
    }

    #[test]
    fn traffic_budget_is_directional_monotone_topology_aware_and_absolutely_bounded() {
        let mut launch = intent();
        launch.direction = SpeedtestDirection::Download;
        let download = native_speedtest_traffic_budget_bytes(&launch, &context()).unwrap();
        launch.direction = SpeedtestDirection::Upload;
        let upload = native_speedtest_traffic_budget_bytes(&launch, &context()).unwrap();
        launch.direction = SpeedtestDirection::Both;
        let both = native_speedtest_traffic_budget_bytes(&launch, &context()).unwrap();
        assert!(both > download);
        assert!(both > upload);

        launch.topology = SpeedtestTopology::Unshaped;
        let unshaped = native_speedtest_traffic_budget_bytes(&launch, &context()).unwrap();
        assert!(unshaped > both);

        let mut extreme = context();
        extreme.unshaped_dl_bound_kbps = Some(u64::MAX / 4);
        extreme.unshaped_ul_bound_kbps = Some(u64::MAX / 4);
        assert_eq!(
            native_speedtest_traffic_budget_bytes(&launch, &extreme).unwrap_err(),
            "native Speed Test safety reserve exceeds its absolute traffic cap"
        );
    }

    #[test]
    fn builder_resolves_auto_but_rejects_an_incompatible_live_backend() {
        let mut automatic = context();
        automatic.configured_speedtest_backend = "auto".to_string();
        let request =
            build_speedtest_request(&intent(), automatic, "d".repeat(32), "e".repeat(64), 1_000)
                .unwrap();
        assert_eq!(request.backend, "speedtest-go");

        let mut mismatched = context();
        mismatched.configured_speedtest_backend = "librespeed-cli".to_string();
        assert_eq!(
            build_speedtest_request(&intent(), mismatched, "d".repeat(32), "e".repeat(64), 1_000,)
                .unwrap_err(),
            "native Speed Test backend does not match the instance configuration"
        );
        assert_eq!(
            resolve_native_speedtest_backend("auto"),
            Some("speedtest-go")
        );
        assert_eq!(
            resolve_native_speedtest_backend("speedtest-go"),
            Some("speedtest-go")
        );
        assert_eq!(resolve_native_speedtest_backend("iperf3"), None);
    }

    #[test]
    fn bootstrap_builder_is_read_only_unshaped_and_binds_the_absence_witness() {
        let mut launch = intent();
        launch.topology = SpeedtestTopology::Unshaped;
        let request = build_bootstrap_speedtest_request_from_context(
            &launch,
            bootstrap_context(&launch),
            "a".repeat(32),
            "b".repeat(64),
            1_000,
        )
        .unwrap();
        assert_eq!(request.target_state, OperationTargetState::AbsentBootstrap);
        assert_eq!(request.managed_sqm_section.as_deref(), Some("cake_wan_sqm"));
        assert_eq!(
            request.speedtest_topology,
            Some(SpeedtestTopology::Unshaped)
        );
        assert!(!request.allow_sqm_disable);
        assert!(!request.allow_active_traffic);
        assert_eq!(
            request.traffic_budget_bytes,
            NATIVE_SPEEDTEST_BOOTSTRAP_TRAFFIC_BUDGET_BYTES
        );
        request.validate_admission_policy().unwrap();

        launch.topology = SpeedtestTopology::Current;
        assert!(build_bootstrap_speedtest_request_from_context(
            &launch,
            bootstrap_context(&launch),
            "c".repeat(32),
            "d".repeat(64),
            1_000,
        )
        .unwrap_err()
        .contains("requires unshaped topology"));
    }

    #[test]
    fn directional_budget_requires_only_the_selected_rate_authority() {
        let mut download_only = intent();
        download_only.direction = SpeedtestDirection::Download;
        let mut live = context();
        live.configured_ul_bound_kbps = None;
        assert!(native_speedtest_traffic_budget_bytes(&download_only, &live).is_ok());

        download_only.direction = SpeedtestDirection::Both;
        assert_eq!(
            native_speedtest_traffic_budget_bytes(&download_only, &live).unwrap_err(),
            "native Speed Test has no valid configured upload rate authority"
        );
    }
}
