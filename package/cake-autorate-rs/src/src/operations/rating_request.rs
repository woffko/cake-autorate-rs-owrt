//! Non-secret LuCI intent -> live-attested native Rating request.
//!
//! The browser selects only the user-visible mode, instance and routing
//! policy. Capability tokens and all route/configuration/SQM fingerprints are
//! generated from current router state inside this root-owned Rust process.

use super::autotune_request::{attest_live_operation_context, LiveRequestContext};
use super::identity::{read_kernel_uuid, DEFAULT_RANDOM_UUID_PATH};
use super::protocol::{
    OperationIdentity, OperationKind, OperationOrigin, OperationRequest, OperationTargetState,
};
use super::rating::epoch_ms;
use std::path::Path;

const NATIVE_AUTOMATIC_RATING_DEADLINE_MS: u64 = 20 * 60 * 1_000;
const NATIVE_GUIDED_RATING_DEADLINE_MS: u64 = 30 * 60 * 1_000;
const NATIVE_AUTOMATIC_RATING_TRAFFIC_BUDGET_BYTES: u64 = 12_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RatingLaunchMode {
    Automatic,
    Client,
}

impl RatingLaunchMode {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "automatic" => Some(Self::Automatic),
            "client" => Some(Self::Client),
            _ => None,
        }
    }

    fn operation(self) -> OperationKind {
        match self {
            Self::Automatic => OperationKind::AutomaticRating,
            Self::Client => OperationKind::GuidedRating,
        }
    }

    fn deadline_ms(self) -> u64 {
        match self {
            Self::Automatic => NATIVE_AUTOMATIC_RATING_DEADLINE_MS,
            Self::Client => NATIVE_GUIDED_RATING_DEADLINE_MS,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RatingLaunchIntent {
    pub instance: String,
    pub expected_target_interface: String,
    pub mode: RatingLaunchMode,
    pub backend: String,
    pub route_mode: String,
    pub mwan3_member: String,
}

/// Parse only policy fields which are safe to expose through LuCI exec ACLs.
/// Unknown, duplicate, empty, or secret-bearing fields are rejected.
pub fn parse_rating_launch_intent<I>(args: I) -> Result<RatingLaunchIntent, String>
where
    I: Iterator<Item = String>,
{
    let mut instance = None;
    let mut expected_target_interface = None;
    let mut mode = None;
    let mut backend = None;
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
            "--mode" if mode.is_none() => {
                let raw = value(&mut args)?;
                mode = Some(
                    RatingLaunchMode::parse(&raw)
                        .ok_or_else(|| "unsupported native Rating mode".to_string())?,
                );
            }
            "--backend" if backend.is_none() => backend = Some(value(&mut args)?),
            "--route-mode" if route_mode.is_none() => route_mode = Some(value(&mut args)?),
            "--mwan3-member" if mwan3_member.is_none() => mwan3_member = Some(value(&mut args)?),
            _ => {
                return Err(format!(
                    "unsupported or duplicate native Rating option: {flag}"
                ))
            }
        }
    }

    let intent = RatingLaunchIntent {
        instance: instance.ok_or_else(|| "--instance is required".to_string())?,
        expected_target_interface: expected_target_interface
            .ok_or_else(|| "--expected-target is required".to_string())?,
        mode: mode.ok_or_else(|| "--mode is required".to_string())?,
        backend: backend.ok_or_else(|| "--backend is required".to_string())?,
        route_mode: route_mode.ok_or_else(|| "--route-mode is required".to_string())?,
        mwan3_member: mwan3_member.unwrap_or_default(),
    };
    validate_rating_intent(&intent)?;
    Ok(intent)
}

pub fn build_live_rating_request(intent: &RatingLaunchIntent) -> Result<OperationRequest, String> {
    validate_rating_intent(intent)?;
    let context = attest_live_operation_context(
        &intent.instance,
        &intent.expected_target_interface,
        &intent.route_mode,
        &intent.mwan3_member,
    )?;
    let created_unix_ms = epoch_ms()?;
    let job_id = read_kernel_uuid(Path::new(DEFAULT_RANDOM_UUID_PATH), "native Rating job ID")?;
    let token_a = read_kernel_uuid(
        Path::new(DEFAULT_RANDOM_UUID_PATH),
        "native Rating job token",
    )?;
    let token_b = read_kernel_uuid(
        Path::new(DEFAULT_RANDOM_UUID_PATH),
        "native Rating job token",
    )?;
    build_rating_request(
        intent,
        context,
        job_id,
        format!("{token_a}{token_b}"),
        created_unix_ms,
    )
}

fn build_rating_request(
    intent: &RatingLaunchIntent,
    context: LiveRequestContext,
    job_id: String,
    job_token: String,
    created_unix_ms: u64,
) -> Result<OperationRequest, String> {
    let deadline_unix_ms = created_unix_ms
        .checked_add(intent.mode.deadline_ms())
        .ok_or_else(|| "native Rating deadline overflow".to_string())?;
    let request = OperationRequest {
        identity: OperationIdentity {
            job_id,
            job_token,
            instance: intent.instance.clone(),
            operation: intent.mode.operation(),
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
        traffic_budget_bytes: match intent.mode {
            RatingLaunchMode::Automatic => NATIVE_AUTOMATIC_RATING_TRAFFIC_BUDGET_BYTES,
            RatingLaunchMode::Client => 0,
        },
    };
    request.validate()?;
    Ok(request)
}

fn validate_rating_intent(intent: &RatingLaunchIntent) -> Result<(), String> {
    match intent.mode {
        RatingLaunchMode::Automatic => {
            if intent.backend != "speedtest-go" {
                return Err("native automatic Rating currently requires speedtest-go".to_string());
            }
        }
        RatingLaunchMode::Client => {
            if intent.backend != "client" {
                return Err("native guided Rating requires the client backend".to_string());
            }
        }
    }
    match intent.route_mode.as_str() {
        "main" if intent.mwan3_member.is_empty() => {}
        "mwan3" if !intent.mwan3_member.is_empty() => {}
        "main" => return Err("main Rating route must not carry an mwan3 member".to_string()),
        "mwan3" => return Err("mwan3 Rating route requires a member".to_string()),
        _ => return Err("native Rating route mode must be main or mwan3".to_string()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::protocol::{OperationRouteIdentity, OperationRouteMode};
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn intent(mode: RatingLaunchMode) -> RatingLaunchIntent {
        RatingLaunchIntent {
            instance: "wan_sqm".to_string(),
            expected_target_interface: "pppoe-wan".to_string(),
            mode,
            backend: if mode == RatingLaunchMode::Automatic {
                "speedtest-go".to_string()
            } else {
                "client".to_string()
            },
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
            unshaped_dl_bound_kbps: Some(1_000_000),
            unshaped_ul_bound_kbps: Some(500_000),
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

    #[test]
    fn launch_parser_accepts_only_non_secret_policy_fields() {
        let parsed = parse_rating_launch_intent(
            [
                "--instance",
                "wan_sqm",
                "--expected-target",
                "pppoe-wan",
                "--mode",
                "automatic",
                "--backend",
                "speedtest-go",
                "--route-mode",
                "main",
            ]
            .into_iter()
            .map(str::to_string),
        )
        .unwrap();
        assert_eq!(parsed, intent(RatingLaunchMode::Automatic));
        for forbidden in [
            "--job-token",
            "--route-fingerprint",
            "--config-fingerprint",
            "--traffic-budget-bytes",
        ] {
            let mut args = vec![forbidden.to_string(), "secret".to_string()];
            assert!(parse_rating_launch_intent(args.drain(..)).is_err());
        }
    }

    #[test]
    fn launch_parser_rejects_duplicate_and_cross_mode_authority() {
        let mut duplicate = intent(RatingLaunchMode::Automatic);
        duplicate.backend = "client".to_string();
        assert!(validate_rating_intent(&duplicate).is_err());
        let duplicate_args = [
            "--instance",
            "wan_sqm",
            "--instance",
            "wanb_sqm",
            "--expected-target",
            "pppoe-wan",
            "--mode",
            "client",
            "--backend",
            "client",
            "--route-mode",
            "main",
        ];
        assert!(
            parse_rating_launch_intent(duplicate_args.into_iter().map(str::to_string)).is_err()
        );
    }

    #[test]
    fn pure_builder_preserves_rating_mode_and_keeps_capability_private() {
        let automatic = build_rating_request(
            &intent(RatingLaunchMode::Automatic),
            context(),
            "d".repeat(32),
            "e".repeat(64),
            1_000,
        )
        .unwrap();
        assert_eq!(automatic.identity.operation, OperationKind::AutomaticRating);
        assert_eq!(
            automatic.target_state,
            OperationTargetState::ExistingManaged
        );
        assert_eq!(automatic.backend, "speedtest-go");
        assert_eq!(
            automatic.traffic_budget_bytes,
            NATIVE_AUTOMATIC_RATING_TRAFFIC_BUDGET_BYTES
        );
        assert!(automatic.managed_sqm_section.is_none());
        assert_eq!(
            automatic.deadline_unix_ms,
            1_000 + NATIVE_AUTOMATIC_RATING_DEADLINE_MS
        );

        let guided = build_rating_request(
            &intent(RatingLaunchMode::Client),
            context(),
            "f".repeat(32),
            "1".repeat(64),
            2_000,
        )
        .unwrap();
        assert_eq!(guided.identity.operation, OperationKind::GuidedRating);
        assert_eq!(guided.target_state, OperationTargetState::ExistingManaged);
        assert_eq!(guided.backend, "client");
        assert_eq!(guided.traffic_budget_bytes, 0);
        assert_eq!(
            guided.deadline_unix_ms,
            2_000 + NATIVE_GUIDED_RATING_DEADLINE_MS
        );
    }

    #[test]
    fn rating_request_does_not_depend_on_speedtest_rate_authority() {
        let mut live = context();
        live.configured_dl_bound_kbps = None;
        live.configured_ul_bound_kbps = None;
        live.unshaped_dl_bound_kbps = None;
        live.unshaped_ul_bound_kbps = None;
        assert!(build_rating_request(
            &intent(RatingLaunchMode::Client),
            live,
            "d".repeat(32),
            "e".repeat(64),
            1_000,
        )
        .is_ok());
    }
}
