//! User-selected route authority, not permission to emit packets.

use super::autotune_request::{explicit_operation_authority, operation_route_matches_config};
use super::protocol::OperationRouteIdentity;
use crate::routing::{explicit_dns_server, ExplicitRouteAuthority, RouteSpec};
use std::net::Ipv4Addr;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExplicitLaunchRoute {
    pub(crate) authority: ExplicitRouteAuthority,
    pub(crate) dns_server: Ipv4Addr,
}

#[derive(Default)]
pub(crate) struct LaunchRouteFields {
    values: [Option<String>; 5],
}

impl LaunchRouteFields {
    pub(crate) fn set(&mut self, flag: &str, value: String) -> Result<(), String> {
        let index = match flag {
            "--route-source-ipv4" => 0,
            "--route-table" => 1,
            "--route-fwmark" => 2,
            "--route-fwmark-mask" => 3,
            "--route-dns-ipv4" => 4,
            _ => return Err(format!("unsupported route option: {flag}")),
        };
        if value.is_empty() || self.values[index].is_some() {
            return Err(format!("empty or duplicate route option: {flag}"));
        }
        self.values[index] = Some(value);
        Ok(())
    }

    pub(crate) fn finish(self, mode: &str) -> Result<Option<ExplicitLaunchRoute>, String> {
        let fields = self
            .values
            .each_ref()
            .map(|v| v.as_deref().unwrap_or_default());
        let authority = ExplicitRouteAuthority::from_fields(
            mode,
            [fields[0], fields[1], fields[2], fields[3]],
        )?;
        let dns = explicit_dns_server(mode, fields[4])?;
        match authority {
            Some(authority) => Ok(Some(ExplicitLaunchRoute {
                authority,
                dns_server: dns.ok_or("explicit launch requires --route-dns-ipv4")?,
            })),
            None => Ok(None),
        }
    }
}

/// Validate launch structure without relaxing RouteSpec's runtime admission gate.
pub(crate) fn launch_route_spec(
    mode: &str,
    member: &str,
    target: &str,
    explicit: Option<&ExplicitLaunchRoute>,
) -> Result<RouteSpec, String> {
    let mut spec = RouteSpec::new(mode, member, target);
    if mode == "explicit" {
        if !member.is_empty() {
            return Err("explicit launch must not carry an mwan3 member".into());
        }
        let selected = explicit.ok_or("explicit launch authority is incomplete")?;
        // Reuse the established device validator, without claiming main routing.
        RouteSpec::new("main", "", target).validate()?;
        explicit_dns_server(mode, &selected.dns_server.to_string())?
            .ok_or("explicit launch DNS is missing")?;
        spec.explicit_authority = Some(selected.authority.clone());
    } else {
        if explicit.is_some() {
            return Err("explicit launch authority conflicts with route mode".into());
        }
        spec.validate()?;
    }
    Ok(spec)
}

pub(crate) fn match_launch_route(
    mode: &str,
    member: &str,
    target: &str,
    explicit: Option<&ExplicitLaunchRoute>,
    route: &OperationRouteIdentity,
) -> Result<(), String> {
    launch_route_spec(mode, member, target, explicit)?;
    if route.l3_device != target {
        return Err("launch route target differs from attested route".into());
    }
    if let Some(selected) = explicit {
        if explicit_operation_authority(route)? != selected.authority
            || route.dns_server != Some(selected.dns_server)
        {
            return Err("launch route authority or DNS differs from attested route".into());
        }
    } else {
        if route.dns_server.is_some() {
            return Err("legacy launch must not carry explicit DNS".into());
        }
        operation_route_matches_config(mode, member, route)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::protocol::OperationRouteMode;
    use super::*;

    fn fields() -> LaunchRouteFields {
        let mut fields = LaunchRouteFields::default();
        for (flag, value) in [
            ("--route-source-ipv4", "192.0.2.2"),
            ("--route-table", "101"),
            ("--route-fwmark", "0x100"),
            ("--route-fwmark-mask", "0x3f00"),
            ("--route-dns-ipv4", "192.0.2.53"),
        ] {
            fields.set(flag, value.into()).unwrap();
        }
        fields
    }

    #[test]
    fn r6_launch_fields_require_complete_explicit_selection() {
        assert!(LaunchRouteFields::default()
            .finish("main")
            .unwrap()
            .is_none());
        for mode in ["main", "auto", "mwan3", "invalid"] {
            assert!(fields().finish(mode).is_err());
        }
        for index in 0..5 {
            let mut missing = fields();
            missing.values[index] = None;
            assert!(missing.finish("explicit").is_err(), "missing {index}");
        }
        for (index, bad) in [
            (0, "::1"),
            (1, "main"),
            (2, "0"),
            (3, "0xff"),
            (4, "127.0.0.1"),
            (4, "dns.example"),
            (4, "169.254.1.1"),
        ] {
            let mut invalid = fields();
            invalid.values[index] = Some(bad.into());
            assert!(invalid.finish("explicit").is_err(), "invalid {index} {bad}");
        }
        for flag in [
            "--route-source-ipv4",
            "--route-table",
            "--route-fwmark",
            "--route-fwmark-mask",
            "--route-dns-ipv4",
        ] {
            assert!(fields()
                .set(flag, "1".into())
                .unwrap_err()
                .contains("duplicate"));
        }
        let selected = fields().finish("explicit").unwrap().unwrap();
        let spec = launch_route_spec("explicit", "", "eth1", Some(&selected)).unwrap();
        // Route authority is valid for the Full controller; operation requests
        // remain closed at protocol admission (link-qualified execution).
        spec.validate().unwrap();
        for (mode, member, target) in [
            ("main", "", "eth1"),
            ("explicit", "wan", "eth1"),
            ("explicit", "", "bad/device"),
        ] {
            assert!(launch_route_spec(mode, member, target, Some(&selected)).is_err());
        }
    }

    #[test]
    fn r6_launch_binding_rejects_each_changed_authority_field() {
        let selected = fields().finish("explicit").unwrap().unwrap();
        let route = OperationRouteIdentity {
            mode: OperationRouteMode::Explicit,
            mwan3_member: None,
            l3_device: "eth1".into(),
            device_ifindex: Some(7),
            source_ip: Some("192.0.2.2".parse().unwrap()),
            routing_table: Some(101),
            fwmark: Some(0x100),
            fwmark_mask: Some(0x3f00),
            dns_server: Some(selected.dns_server),
        };
        match_launch_route("explicit", "", "eth1", Some(&selected), &route).unwrap();
        for index in 0..10 {
            let mut changed = route.clone();
            match index {
                0 => changed.source_ip = Some("192.0.2.3".parse().unwrap()),
                1 => changed.routing_table = Some(102),
                2 => changed.fwmark = Some(0x200),
                3 => changed.fwmark_mask = Some(0xff00),
                4 => changed.dns_server = Some("192.0.2.54".parse().unwrap()),
                5 => changed.dns_server = None,
                6 => changed.device_ifindex = None,
                7 => changed.l3_device = "eth2".into(),
                8 => changed.mwan3_member = Some("wan".into()),
                _ => changed.mode = OperationRouteMode::Main,
            }
            assert!(
                match_launch_route("explicit", "", "eth1", Some(&selected), &changed).is_err(),
                "changed {index}"
            );
        }
    }
}
