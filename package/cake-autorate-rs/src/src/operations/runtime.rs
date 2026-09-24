use super::protocol::{
    OperationKind, OperationRequest, OperationRouteIdentity, OperationRouteMode,
};
use super::{
    autotune_request::{
        explicit_operation_authority, operation_route_identity, operation_route_matches_instance,
    },
    sqm_identity,
};
use crate::routing::{
    inspect_route, ExplicitRouteAuthority, ExplicitRouteObservation, RouteSnapshot, RouteSpec,
};
use crate::{inspect_sqm_topology, managed_sqm_target_ready, Config, SqmTopologyErrorKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeAttestation {
    Ready,
    Waiting { code: String, message: String },
    Unsafe { code: String, message: String },
}

impl RuntimeAttestation {
    pub(crate) fn waiting(code: &str, message: impl Into<String>) -> Self {
        Self::Waiting {
            code: code.to_string(),
            message: message.into(),
        }
    }

    pub(crate) fn unsafe_state(code: &str, message: impl Into<String>) -> Self {
        Self::Unsafe {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

pub fn attest_openwrt_runtime(request: &OperationRequest) -> RuntimeAttestation {
    attest_runtime_for(request, RuntimePurpose::OperationTraffic)
}

/// Only native rollback verification uses this lane. Its caller already owns
/// the recovery record/global lock and has restored the exact original files.
/// Batch acceptance happens after unlocking; configuration/route/qdisc checks
/// still apply here, but requiring an already-settled batch would deadlock it.
pub(crate) fn attest_openwrt_runtime_during_native_restore(
    request: &OperationRequest,
) -> RuntimeAttestation {
    attest_runtime_for(request, RuntimePurpose::NativeRestore)
}

#[derive(Clone, Copy)]
enum RuntimePurpose {
    OperationTraffic,
    NativeRestore,
}

impl RuntimePurpose {
    fn attest_generation(self, proof: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
        match self {
            Self::OperationTraffic => proof(),
            Self::NativeRestore => Ok(()),
        }
    }
}

fn attest_runtime_for(request: &OperationRequest, purpose: RuntimePurpose) -> RuntimeAttestation {
    let cfg = match Config::from_uci(&request.identity.instance) {
        Ok(cfg) => cfg,
        Err(error) => {
            return RuntimeAttestation::unsafe_state("runtime-config-invalid", error);
        }
    };

    if cfg.sqm_interface != request.identity.target_interface
        || cfg.sqm_interface != request.route.l3_device
    {
        return RuntimeAttestation::unsafe_state(
            "runtime-target-mismatch",
            format!(
                "configured SQM interface {} does not match requested target {} and route device {}",
                cfg.sqm_interface, request.identity.target_interface, request.route.l3_device
            ),
        );
    }
    if request.identity.operation == OperationKind::Speedtest
        && (!cfg.manage_sqm || !cfg.sqm_enabled)
    {
        return if managed_sqm_target_ready(&cfg) {
            RuntimeAttestation::Ready
        } else {
            RuntimeAttestation::waiting(
                "runtime-target-not-ready",
                format!(
                    "target interface {} and its counters are still settling",
                    cfg.sqm_interface
                ),
            )
        };
    }
    if !cfg.manage_sqm {
        return RuntimeAttestation::unsafe_state(
            "external-sqm-unmodelled",
            "bandwidth-only external CAKE control does not provide the managed topology/restore authority required by this native operation"
                .to_string(),
        );
    }

    if request.identity.operation == OperationKind::FullAutotune {
        match attest_native_autotune_identity(request, &cfg, purpose) {
            Ok(()) => {}
            Err((code, message, true)) => {
                return RuntimeAttestation::waiting(code, message);
            }
            Err((code, message, false)) => {
                return RuntimeAttestation::unsafe_state(code, message);
            }
        }
    }
    if !managed_sqm_target_ready(&cfg) {
        return RuntimeAttestation::waiting(
            "runtime-target-not-ready",
            format!(
                "target interface {} and its counters are still settling",
                cfg.sqm_interface
            ),
        );
    }

    match inspect_sqm_topology(&cfg) {
        Ok(()) => RuntimeAttestation::Ready,
        Err(error) => match error.kind {
            SqmTopologyErrorKind::Settling => {
                RuntimeAttestation::waiting(error.code, error.message)
            }
            SqmTopologyErrorKind::Unsafe => {
                RuntimeAttestation::unsafe_state(error.code, error.message)
            }
        },
    }
}

fn attest_native_autotune_identity(
    request: &OperationRequest,
    cfg: &Config,
    purpose: RuntimePurpose,
) -> Result<(), (&'static str, String, bool)> {
    operation_route_matches_instance(cfg, &request.route).map_err(|error| {
        (
            "runtime-route-config-mismatch",
            format!("instance probe-route configuration drifted after launch: {error}"),
            false,
        )
    })?;
    let expected_sqm_section = request.managed_sqm_section.as_deref().ok_or_else(|| {
        (
            "runtime-sqm-section-missing",
            "immutable Full Auto-Tune request has no managed SQM section".to_string(),
            false,
        )
    })?;
    if cfg.sqm_section != expected_sqm_section {
        return Err((
            "runtime-sqm-section-changed",
            format!(
                "managed SQM section changed from {expected_sqm_section} to {} after launch",
                cfg.sqm_section
            ),
            false,
        ));
    }
    let live_config = sqm_identity::managed_autotune_config_fingerprint(
        &request.identity.instance,
        expected_sqm_section,
    )
    .map_err(|error| ("runtime-config-identity-unavailable", error, false))?;
    if live_config != request.identity.config_fingerprint {
        return Err((
            "runtime-config-changed",
            "managed CAKE Autorate, SQM, or traffic-priority configuration changed after launch"
                .to_string(),
            false,
        ));
    }

    let live_sqm = sqm_identity::managed_sqm_identity_fingerprint(
        &request.identity.instance,
        expected_sqm_section,
        &request.identity.target_interface,
    )
    .map_err(|error| ("runtime-sqm-identity-unavailable", error, false))?;
    if live_sqm != request.identity.sqm_fingerprint {
        return Err((
            "runtime-sqm-changed",
            "managed SQM identity changed after launch".to_string(),
            false,
        ));
    }

    purpose
        .attest_generation(|| {
            super::service_lifecycle::attest_operation_applied_sqm(
                &request.identity.instance,
                cfg,
                &live_sqm,
            )
        })
        .map_err(|error| ("runtime-applied-source-mismatch", error, false))?;

    attest_openwrt_route_identity(request)
}

#[cfg(test)]
mod source_authority_tests {
    use super::*;

    #[test]
    fn r4_new_traffic_requires_settled_generation_but_owner_restore_can_finish_it() {
        let mut calls = 0;
        assert_eq!(
            RuntimePurpose::OperationTraffic
                .attest_generation(|| {
                    calls += 1;
                    Err("generation-pending".into())
                })
                .unwrap_err(),
            "generation-pending"
        );
        assert_eq!(calls, 1);
        RuntimePurpose::NativeRestore
            .attest_generation(|| {
                panic!("owner restore cannot wait for post-unlock batch acceptance")
            })
            .unwrap();
    }
}

pub(crate) fn attest_openwrt_route_identity(
    request: &OperationRequest,
) -> Result<(), (&'static str, String, bool)> {
    let snapshot = inspect_operation_route(
        &request.route,
        &request.identity.target_interface,
        ExplicitRouteAuthority::observe_system,
    )
    .map_err(|error| ("runtime-route-identity-unavailable", error, false))?;
    if !snapshot.online {
        return Err((
            "runtime-route-not-ready",
            if snapshot.reason.is_empty() {
                "selected route is offline".to_string()
            } else {
                snapshot.reason.clone()
            },
            true,
        ));
    }
    route_snapshot_matches_request(request, &snapshot)
        .map_err(|error| ("runtime-route-changed", error, false))
}

/// Read-only revalidation, not execution admission. Explicit requests must not
/// fall through to the legacy main/mwan3 inspector, even after observation fails.
pub(crate) fn inspect_operation_route(
    route: &OperationRouteIdentity,
    target: &str,
    observe: impl FnOnce(&ExplicitRouteAuthority, &str) -> Result<ExplicitRouteObservation, String>,
) -> Result<RouteSnapshot, String> {
    if route.mode != OperationRouteMode::Explicit {
        let spec = RouteSpec::new(
            route.mode.as_str(),
            route.mwan3_member.as_deref().unwrap_or(""),
            target,
        );
        return inspect_route(&spec);
    }
    let authority = explicit_operation_authority(route)?;
    if route.l3_device != target {
        return Err("explicit route device differs from the operation target".into());
    }
    let observation = observe(&authority, target)?;
    let mut actual = operation_route_identity(&observation.identity)?;
    // Kernel observation attests forwarding, not recursive-server selection.
    // DNS is separately compared against immutable/applied instance config.
    actual.dns_server = route.dns_server;
    if actual != *route {
        return Err("selected explicit route identity changed after launch".into());
    }
    Ok(RouteSnapshot {
        identity: observation.identity,
        online: true,
        active: true,
        member_status: String::new(),
        reason: String::new(),
    })
}

fn route_snapshot_matches_request(
    request: &OperationRequest,
    snapshot: &RouteSnapshot,
) -> Result<(), String> {
    request.route.validate()?;
    let mut actual = operation_route_identity(&snapshot.identity)?;
    actual.dns_server = request.route.dns_server;
    if actual != request.route {
        return Err("selected route identity changed after launch".to_string());
    }
    let fingerprint = sqm_identity::sha256sum(snapshot.stable_key().as_bytes())?;
    if fingerprint != request.identity.route_fingerprint {
        return Err("selected route fingerprint changed after launch".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::RouteIdentity;

    fn explicit_observation() -> ExplicitRouteObservation {
        ExplicitRouteObservation {
            identity: RouteIdentity {
                device_ifindex: Some(42),
                mode: "explicit".into(),
                member: String::new(),
                device: "eth1".into(),
                source_ip: "192.0.2.2".into(),
                fwmark: "0x100".into(),
                table: "101".into(),
                fwmark_mask: Some(0x3f00),
            },
            ifindex: 42,
            rule_priority: 100,
        }
    }

    #[test]
    fn r6_explicit_runtime_reobserves_exact_authority_without_fallback() {
        let observation = explicit_observation();
        let route = operation_route_identity(&observation.identity).unwrap();
        let snapshot = inspect_operation_route(&route, "eth1", |authority, device| {
            assert_eq!(device, "eth1");
            assert_eq!(
                authority,
                &ExplicitRouteAuthority::from_fields(
                    "explicit",
                    ["192.0.2.2", "101", "256", "16128"]
                )
                .unwrap()
                .unwrap()
            );
            Ok(observation.clone())
        })
        .unwrap();
        assert_eq!(snapshot.identity, observation.identity);
        assert!(snapshot.online);
        let error = inspect_operation_route(&route, "eth1", |_, _| {
            Err("explicit-earlier-rule-unverified".into())
        })
        .unwrap_err();
        assert_eq!(error, "explicit-earlier-rule-unverified");
        assert!(inspect_operation_route(&route, "eth2", |_, _| {
            panic!("target mismatch must be rejected before collecting")
        })
        .is_err());
        let mut invalid = route.clone();
        invalid.device_ifindex = None;
        assert!(inspect_operation_route(&invalid, "eth1", |_, _| {
            panic!("incomplete authority must be rejected before collecting")
        })
        .is_err());
    }

    #[test]
    fn r6_explicit_runtime_rejects_each_changed_identity_component() {
        let original = explicit_observation();
        let route = operation_route_identity(&original.identity).unwrap();
        for variant in 0..8 {
            let mut changed = original.clone();
            match variant {
                0 => changed.identity.device_ifindex = Some(43),
                1 => changed.identity.source_ip = "192.0.2.3".into(),
                2 => changed.identity.device = "eth2".into(),
                3 => changed.identity.fwmark = "0x200".into(),
                4 => changed.identity.fwmark_mask = Some(0xff00),
                5 => changed.identity.table = "102".into(),
                6 => changed.identity.mode = "main".into(),
                _ => changed.identity.member = "wan".into(),
            }
            assert!(inspect_operation_route(&route, "eth1", |_, _| Ok(changed)).is_err());
        }
    }
}
