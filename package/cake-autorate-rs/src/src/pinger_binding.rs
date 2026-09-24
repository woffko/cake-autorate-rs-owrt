//! Derive automatic argv from a live route, without rewriting user UCI options.
use super::{safe_extra_args, Config, RouteSnapshot};

#[cfg(feature = "calibration")]
pub(crate) fn bootstrap_arguments(
    route: &crate::operations::protocol::OperationRouteIdentity,
    gid: Option<u32>,
    reflectors: &[String],
) -> Result<Vec<String>, String> {
    use crate::operations::protocol::OperationRouteMode;
    route.validate()?;
    if gid.is_some_and(|gid| matches!(gid, 0 | u32::MAX)) {
        return Err("bootstrap probe group is invalid".into());
    }
    let mut args = vec!["-I".into(), route.l3_device.clone()];
    if route.mode == OperationRouteMode::Explicit {
        if gid.is_none()
            || reflectors.is_empty()
            || reflectors
                .iter()
                .any(|value| value.parse::<std::net::Ipv4Addr>().is_err())
        {
            return Err(
                "explicit bootstrap pinger requires an owned group and literal IPv4 reflectors"
                    .into(),
            );
        }
        args.extend([
            "-S".into(),
            route
                .source_ip
                .ok_or("explicit bootstrap source missing")?
                .to_string(),
        ]);
    }
    Ok(args)
}

pub(crate) fn configured_arguments(cfg: &Config) -> Result<Vec<String>, String> {
    let args = safe_extra_args(&cfg.ping_extra_args);
    if cfg.ping_extra_args.split_whitespace().count() != args.len() {
        return Err("pinger extra arguments contain unsupported tokens".into());
    }
    if args.iter().any(|value| value == "--") {
        return Err("ping_extra_args must not terminate backend option parsing".into());
    }
    let names: &[&str] = match cfg.pinger_method.as_str() {
        "fping" | "fping-ts" => &["-I", "--iface"],
        "ping" => &["-I"],
        "tsping" => &["-i", "--interface"],
        _ => &[],
    };
    if matches!(cfg.pinger_method.as_str(), "tsping" | "irtt")
        && !option_values(&args, &["-I"])?.is_empty()
    {
        return Err("legacy -I is not supported by this pinger; review ping_extra_args".into());
    }
    for value in option_values(&args, names)? {
        let source_pin = cfg.pinger_method == "ping" && value.parse::<std::net::Ipv4Addr>().is_ok();
        if !source_pin && value != cfg.ul_if {
            return Err(
                "ping_extra_args interface pin conflicts with ul_if; review the saved pin".into(),
            );
        }
    }
    Ok(args)
}

pub(crate) fn arguments(cfg: &Config, route: &RouteSnapshot) -> Result<Vec<String>, String> {
    let spec = cfg.route_spec();
    let identity = &route.identity;
    let mode = if cfg.route_mode == "explicit" {
        if !cfg.mwan3_member.is_empty() || !cfg.ping_prefix_string.trim().is_empty() {
            return Err("explicit pinger cannot use mwan3 member or legacy prefix".into());
        }
        cfg.explicit_route_authority
            .as_ref()
            .ok_or("explicit pinger authority missing")?
            .attest_identity(identity)?;
        "explicit"
    } else {
        spec.effective_mode()?.as_str()
    };
    let source = identity.source_ip.parse::<std::net::Ipv4Addr>().ok();
    if !route.online
        || identity.device != spec.expected_device
        || identity.mode != mode
        || identity.member != spec.member
        || source.is_none_or(|address| {
            address.is_unspecified()
                || address.is_loopback()
                || address.is_multicast()
                || address.octets() == [255; 4]
        })
    {
        return Err("pinger binding requires the current matching IPv4 route identity".into());
    }
    if identity.mode == "main" {
        if !matches!(identity.table.as_str(), "main" | "254")
            || (!identity.fwmark.is_empty() && parse_mark(&identity.fwmark) != Some(0))
        {
            return Err("main pinger route has an unexpected table or mark".into());
        }
    } else if identity.mode == "mwan3"
        && (parse_mark(&identity.fwmark).is_none_or(|mark| mark == 0)
            || !super::routing::is_safe_identifier(&identity.table))
    {
        return Err("mwan3 pinger route has no exact mark/table authority".into());
    }
    let mut args = configured_arguments(cfg)?;
    match cfg.pinger_method.as_str() {
        "fping" | "fping-ts" => {
            ensure_option(&mut args, &["-I", "--iface"], "-I", &identity.device, false)?;
            ensure_option(
                &mut args,
                &["-S", "--src"],
                "-S",
                &identity.source_ip,
                false,
            )?;
        }
        "ping" => {
            // ping accepts either an interface or an address for -I. Preserve
            // a matching manual source pin; never silently reinterpret it.
            let pins = option_values(&args, &["-I"])?;
            if pins.is_empty() {
                args.extend([
                    "-I".into(),
                    if mode == "explicit" {
                        identity.source_ip.clone()
                    } else {
                        identity.device.clone()
                    },
                ]);
            } else if pins.iter().any(|value| {
                (*value != identity.device && *value != identity.source_ip)
                    || (mode == "explicit" && *value != identity.source_ip)
            }) {
                return Err(
                    "explicit ping -I conflicts with current route; review the saved pin".into(),
                );
            }
        }
        "tsping" => {
            if mode == "explicit" {
                return Err("explicit tsping source-address binding is not supported".into());
            }
            if !option_values(&args, &["-I"])?.is_empty() {
                return Err("tsping uses --interface, not legacy -I; review the saved pin".into());
            }
            ensure_option(
                &mut args,
                &["-i", "--interface"],
                "--interface",
                &identity.device,
                false,
            )?;
            if !identity.fwmark.is_empty() {
                ensure_option(
                    &mut args,
                    &["-f", "--fw-mark"],
                    "--fw-mark",
                    &identity.fwmark,
                    true,
                )?;
            } else if !option_values(&args, &["-f", "--fw-mark"])?.is_empty() {
                return Err("explicit tsping fwmark has no matching route authority".into());
            }
        }
        "irtt" => {
            if !option_values(&args, &["-I"])?.is_empty() {
                return Err("irtt does not support legacy -I; review the saved pin".into());
            }
            let locals = option_values(&args, &["--local"])?;
            if locals.is_empty() {
                args.push(format!("--local={}", identity.source_ip));
            } else if locals.iter().any(|value| {
                let address = value.split(':').next().unwrap_or_default();
                address != identity.source_ip
            }) {
                return Err("explicit irtt local address conflicts with current route".into());
            }
            // IRTT's local option binds an address, not SO_BINDTODEVICE. Its
            // existing main/mwan3 route contract remains necessary.
        }
        _ => return Err("unsupported pinger binding backend".into()),
    }
    Ok(args)
}

fn option_values<'a>(args: &'a [String], names: &[&str]) -> Result<Vec<&'a str>, String> {
    let mut values = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let token = &args[index];
        let mut found = None;
        for name in names {
            if token == name {
                index += 1;
                found = Some(
                    args.get(index)
                        .map(String::as_str)
                        .ok_or("pinger binding option has no value")?,
                );
                break;
            }
            if name.starts_with("--") {
                if let Some(value) = token.strip_prefix(&format!("{name}=")) {
                    found = Some(value);
                    break;
                }
            }
            if name.len() == 2 {
                if let Some(value) = token.strip_prefix(*name).filter(|value| !value.is_empty()) {
                    found = Some(value);
                    break;
                }
            }
        }
        if let Some(value) = found {
            if value.is_empty() || value.starts_with('-') {
                return Err("pinger binding option has no value".into());
            }
            values.push(value);
        }
        index += 1;
    }
    Ok(values)
}

fn ensure_option(
    args: &mut Vec<String>,
    names: &[&str],
    option: &str,
    expected: &str,
    mark: bool,
) -> Result<(), String> {
    let existing = option_values(args, names)?;
    if existing.is_empty() {
        let value = if mark {
            parse_mark(expected)
                .ok_or("invalid route mark")?
                .to_string()
        } else {
            expected.into()
        };
        args.extend([option.into(), value]);
    } else if existing.iter().any(|value| {
        if mark {
            parse_mark(value) != parse_mark(expected) || parse_mark(expected).is_none()
        } else {
            *value != expected
        }
    }) {
        return Err(
            "explicit pinger binding conflicts with current route; review the saved pin".into(),
        );
    }
    Ok(())
}

fn parse_mark(value: &str) -> Option<u32> {
    if let Some(value) = value.strip_prefix("0x") {
        u32::from_str_radix(value, 16).ok()
    } else {
        value.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "calibration")]
    #[test]
    fn r6_bootstrap_fping_pins_source_device_and_requires_owned_literals() {
        use crate::operations::protocol::{OperationRouteIdentity, OperationRouteMode};
        let mut route = OperationRouteIdentity {
            dns_server: Some("192.0.2.53".parse().unwrap()),
            device_ifindex: Some(42),
            mode: OperationRouteMode::Explicit,
            mwan3_member: None,
            l3_device: "wan".into(),
            source_ip: Some("192.0.2.2".parse().unwrap()),
            fwmark: Some(0x100),
            routing_table: Some(101),
            fwmark_mask: Some(0x3f00),
        };
        let reflectors = vec!["198.51.100.1".into()];
        assert_eq!(
            super::bootstrap_arguments(&route, Some(32770), &reflectors).unwrap(),
            ["-I", "wan", "-S", "192.0.2.2"]
        );
        for gid in [None, Some(0), Some(u32::MAX)] {
            assert!(super::bootstrap_arguments(&route, gid, &reflectors).is_err());
        }
        for targets in [
            Vec::new(),
            vec!["dns.example".into()],
            vec!["::1".into()],
            vec!["--help".into()],
        ] {
            assert!(super::bootstrap_arguments(&route, Some(32770), &targets).is_err());
        }
        route.source_ip = None;
        assert!(super::bootstrap_arguments(&route, Some(32770), &reflectors).is_err());
        route.mode = OperationRouteMode::Main;
        route.dns_server = None;
        route.device_ifindex = None;
        route.fwmark_mask = None;
        route.fwmark = None;
        route.routing_table = None;
        assert_eq!(
            super::bootstrap_arguments(&route, None, &reflectors).unwrap(),
            ["-I", "wan"]
        );
    }
    use super::*;
    use crate::routing;
    fn fixture(method: &str) -> (Config, RouteSnapshot) {
        let mut cfg = Config::defaults("binding".into());
        cfg.pinger_method = method.into();
        cfg.ul_if = "fixture0".into();
        cfg.route_mode = "main".into();
        let route = RouteSnapshot {
            identity: routing::RouteIdentity {
                device_ifindex: None,
                fwmark_mask: None,
                mode: "main".into(),
                member: String::new(),
                device: "fixture0".into(),
                source_ip: "192.0.2.1".into(),
                fwmark: String::new(),
                table: "main".into(),
            },
            online: true,
            active: true,
            member_status: "online".into(),
            reason: String::new(),
        };
        (cfg, route)
    }
    #[test]
    fn r6_explicit_pinger_uses_complete_authority_and_exact_source() {
        let (mut cfg, mut route) = fixture("fping");
        cfg.route_mode = "explicit".into();
        cfg.explicit_route_authority = routing::ExplicitRouteAuthority::from_fields(
            "explicit",
            ["192.0.2.1", "101", "0x100", "0x3f00"],
        )
        .unwrap();
        route.identity.mode = "explicit".into();
        route.identity.device_ifindex = Some(42);
        route.identity.table = "101".into();
        route.identity.fwmark = "0x100".into();
        route.identity.fwmark_mask = Some(0x3f00);
        for (method, expected) in [
            ("fping", vec!["-I", "fixture0", "-S", "192.0.2.1"]),
            ("fping-ts", vec!["-I", "fixture0", "-S", "192.0.2.1"]),
            ("ping", vec!["-I", "192.0.2.1"]),
            ("irtt", vec!["--local=192.0.2.1"]),
        ] {
            cfg.pinger_method = method.into();
            assert_eq!(arguments(&cfg, &route).unwrap(), expected);
        }
        cfg.pinger_method = "tsping".into();
        assert!(arguments(&cfg, &route)
            .unwrap_err()
            .contains("source-address"));
        cfg.pinger_method = "ping".into();
        cfg.ping_extra_args = "-I fixture0".into();
        assert!(arguments(&cfg, &route).is_err());
        cfg.ping_extra_args.clear();
        for variant in 0..6 {
            let mut changed = route.clone();
            match variant {
                0 => changed.identity.fwmark_mask = Some(0xff00),
                1 => changed.identity.table = "102".into(),
                2 => changed.identity.source_ip = "192.0.2.2".into(),
                3 => changed.identity.device_ifindex = None,
                4 => changed.identity.fwmark = "0x200".into(),
                _ => changed.identity.member = "wan".into(),
            }
            assert!(arguments(&cfg, &changed).is_err());
        }
        assert!(super::super::pinger_command(&cfg, "ping", None).is_err());
        let command = super::super::pinger_command(&cfg, "ping", Some(50000)).unwrap();
        assert_eq!(command.get_program(), "ping");
        cfg.ping_prefix_string = "mwan3 use wan exec".into();
        assert!(super::super::pinger_command(&cfg, "ping", Some(50000)).is_err());
    }

    #[test]
    fn r6_runtime_binding_is_backend_specific_and_never_persisted() {
        for (method, expected) in [
            ("fping", vec!["-I", "fixture0", "-S", "192.0.2.1"]),
            ("fping-ts", vec!["-I", "fixture0", "-S", "192.0.2.1"]),
            ("ping", vec!["-I", "fixture0"]),
            ("tsping", vec!["--interface", "fixture0"]),
            ("irtt", vec!["--local=192.0.2.1"]),
        ] {
            let (cfg, route) = fixture(method);
            assert_eq!(arguments(&cfg, &route).unwrap(), expected);
            assert!(cfg.ping_extra_args.is_empty());
        }
    }
    #[test]
    fn r6_manual_arguments_survive_but_stale_conflicting_pins_are_rejected() {
        let (mut cfg, mut route) = fixture("fping");
        cfg.ping_extra_args = "-I fixture0 -S192.0.2.1 -Q 5".into();
        assert_eq!(
            arguments(&cfg, &route).unwrap(),
            ["-I", "fixture0", "-S192.0.2.1", "-Q", "5"]
        );
        let original = cfg.ping_extra_args.clone();
        route.identity.source_ip = "192.0.2.2".into();
        assert!(arguments(&cfg, &route).is_err());
        assert_eq!(cfg.ping_extra_args, original);
        for invalid in [
            "-I oldwan",
            "--iface=oldwan",
            "-I",
            "-I fixture0 -I oldwan",
            "-I fixture0 ;",
        ] {
            cfg.ping_extra_args = invalid.into();
            assert!(arguments(&cfg, &route).is_err());
        }
        cfg.ping_extra_args.clear();
        route.online = false;
        assert!(arguments(&cfg, &route).is_err());
    }

    #[test]
    fn r6_conflicting_device_pins_fail_pure_candidate_validation_before_start() {
        let (mut cfg, _) = fixture("fping");
        cfg.ping_extra_args = "-I oldwan".into();
        assert!(cfg.validate().unwrap_err().contains("pin conflicts"));
        cfg.ping_extra_args = "-I fixture0 -Q 5".into();
        cfg.validate().unwrap();
        cfg.ping_extra_args = "--".into();
        assert!(cfg.validate().unwrap_err().contains("option parsing"));
        cfg.ping_extra_args = "-I=fixture0".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn r6_route_changes_rederive_auto_binding_and_require_exact_mwan3_authority() {
        let (mut cfg, mut route) = fixture("tsping");
        cfg.route_mode = "mwan3".into();
        cfg.mwan3_member = "member".into();
        route.identity.mode = "mwan3".into();
        route.identity.member = "member".into();
        assert!(arguments(&cfg, &route).is_err());
        route.identity.fwmark = "0x100".into();
        route.identity.table = "1".into();
        assert_eq!(
            arguments(&cfg, &route).unwrap(),
            ["--interface", "fixture0", "--fw-mark", "256"]
        );
        cfg.ping_extra_args = "--fw-mark=256".into();
        assert!(arguments(&cfg, &route).is_ok());
        cfg.ping_extra_args = "--fw-mark=512".into();
        assert!(arguments(&cfg, &route).is_err());
        cfg.ping_extra_args.clear();
        cfg.ul_if = "fixture1".into();
        route.identity.device = "fixture1".into();
        assert_eq!(arguments(&cfg, &route).unwrap()[1], "fixture1");
        route.identity.member = "wrong".into();
        assert!(arguments(&cfg, &route).is_err());
    }
}
