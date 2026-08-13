use super::protocol::{OperationKind, OperationRequest};
use super::{
    autotune_request::{operation_route_identity, operation_route_matches_config},
    sqm_identity,
};
use crate::routing::{inspect_route, RouteSnapshot, RouteSpec};
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
            "coordinator admission for an externally managed SQM queue is not implemented"
                .to_string(),
        );
    }

    if request.identity.operation == OperationKind::FullAutotune {
        match attest_native_autotune_identity(request, &cfg) {
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
) -> Result<(), (&'static str, String, bool)> {
    operation_route_matches_config(&cfg.route_mode, &cfg.mwan3_member, &request.route).map_err(
        |error| {
            (
                "runtime-route-config-mismatch",
                format!("instance probe-route configuration drifted after launch: {error}"),
                false,
            )
        },
    )?;
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

    attest_openwrt_route_identity(request)
}

pub(crate) fn attest_openwrt_route_identity(
    request: &OperationRequest,
) -> Result<(), (&'static str, String, bool)> {
    let configured_mode = request.route.mode.as_str();
    let member = request.route.mwan3_member.as_deref().unwrap_or("");
    let spec = RouteSpec::new(configured_mode, member, &request.identity.target_interface);
    let snapshot = inspect_route(&spec)
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

fn route_snapshot_matches_request(
    request: &OperationRequest,
    snapshot: &RouteSnapshot,
) -> Result<(), String> {
    let actual = operation_route_identity(&snapshot.identity)?;
    if actual != request.route {
        return Err("selected route identity changed after launch".to_string());
    }
    let fingerprint = sqm_identity::sha256sum(snapshot.stable_key().as_bytes())?;
    if fingerprint != request.identity.route_fingerprint {
        return Err("selected route fingerprint changed after launch".to_string());
    }
    Ok(())
}
