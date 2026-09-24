use std::env;
use std::io;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::OnceLock;

/// Optional configured recursive DNS endpoint; never infer a public default or
/// accept loopback, whose upstream socket would belong to another process.
pub(crate) fn explicit_dns_server(
    mode: &str,
    value: &str,
) -> Result<Option<std::net::Ipv4Addr>, String> {
    if value.is_empty() {
        return Ok(None);
    }
    if mode != "explicit" {
        return Err("route_dns_ipv4 requires explicit routing".into());
    }
    let ip: std::net::Ipv4Addr = value
        .parse()
        .map_err(|_| "route_dns_ipv4 must be an IPv4 address")?;
    if ip.octets()[0] == 0 || ip.octets()[0] >= 224 || ip.is_loopback() || ip.is_link_local() {
        return Err("route_dns_ipv4 requires a non-loopback unicast server".into());
    }
    Ok(Some(ip))
}

/// Configured authority only; assignment, rules and packet enforcement require
/// separate live admission. This type does not itself enable explicit PBR.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExplicitRouteAuthority {
    source: std::net::Ipv4Addr,
    table: u32,
    mark: u32,
    mask: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExplicitRouteObservation {
    pub(crate) identity: RouteIdentity,
    pub(crate) ifindex: u32,
    pub(crate) rule_priority: u32,
}

impl ExplicitRouteAuthority {
    /// Compare an observed identity with configured policy; callers must still
    /// establish freshness, device ownership and packet enforcement separately.
    pub(crate) fn attest_identity(&self, identity: &RouteIdentity) -> Result<(), String> {
        if identity.mode != "explicit"
            || !identity.member.is_empty()
            || !identity.device_ifindex.is_some_and(|index| index != 0)
            || !is_safe_identifier(&identity.device)
        {
            return Err("explicit route identity lacks mode/link authority".into());
        }
        let mask = identity
            .fwmark_mask
            .ok_or("explicit identity mask missing")?
            .to_string();
        let observed = Self::from_fields(
            "explicit",
            [
                &identity.source_ip,
                &identity.table,
                &identity.fwmark,
                &mask,
            ],
        )?
        .ok_or("explicit identity authority missing")?;
        if observed != *self {
            return Err("explicit route identity differs from configured authority".into());
        }
        Ok(())
    }

    /// Bounded read-only system collector. Never uses PATH or a shell, and
    /// never falls back to the main-route inspector after a failed witness.
    pub(crate) fn observe_system(&self, device: &str) -> Result<ExplicitRouteObservation, String> {
        use crate::operations::process::{run_bounded_command_output_with_input, SpawnSpec};
        use std::os::unix::fs::MetadataExt;
        let program = ["/sbin/ip", "/usr/sbin/ip"]
            .into_iter()
            .find(|path| Path::new(path).exists())
            .ok_or("explicit-route-ip-unavailable")?;
        let metadata = std::fs::metadata(program).map_err(|_| "explicit-route-ip-unavailable")?;
        if !metadata.is_file() || metadata.uid() != 0 {
            return Err("explicit-route-ip-owner-invalid".into());
        }
        self.observe_with(device, |args| {
            let spec = SpawnSpec {
                program: program.into(),
                arguments: args.iter().map(std::ffi::OsString::from).collect(),
                environment: vec![("LC_ALL".into(), "C".into())],
            };
            let output = run_bounded_command_output_with_input(
                &spec,
                None,
                std::time::Duration::from_secs(2),
                256 * 1024,
                || false,
                |_| {},
            )?;
            if !output.status.success() {
                return Err("explicit-route-ip-inspection-failed".into());
            }
            Ok(output.stdout)
        })
    }

    /// Read-only candidate observation, not permission to spawn traffic.
    /// Two matching inventories detect observed concurrent changes; they do
    /// not replace a link-generation/event witness or packet enforcement.
    pub(crate) fn observe_with(
        &self,
        device: &str,
        mut read_ip: impl FnMut(&[&str]) -> Result<Vec<u8>, String>,
    ) -> Result<ExplicitRouteObservation, String> {
        if !is_safe_identifier(device) {
            return Err("explicit-route-device-invalid".into());
        }
        let table = self.table.to_string();
        let mut read = || -> Result<(u32, u32, Vec<u8>, Vec<u8>), String> {
            let addresses = read_ip(&["-j", "-4", "addr", "show", "dev", device])?;
            let index = self.attest_source_address(device, &addresses)?;
            let rules = read_ip(&["-4", "rule", "show"])?;
            let priority = self.attest_rules(
                std::str::from_utf8(&rules).map_err(|_| "explicit-rule-encoding-invalid")?,
            )?;
            let routes = read_ip(&["-j", "-4", "route", "show", "table", &table])?;
            self.attest_table_routes(device, &routes)?;
            Ok((index, priority, rules, routes))
        };
        let before = read()?;
        let after = read()?;
        if before != after {
            return Err("explicit-route-observation-changed".into());
        }
        Ok(ExplicitRouteObservation {
            identity: RouteIdentity {
                device_ifindex: Some(before.0),
                mode: "explicit".into(),
                member: String::new(),
                device: device.into(),
                source_ip: self.source.to_string(),
                fwmark: format!("0x{:x}", self.mark),
                table,
                fwmark_mask: Some(self.mask),
            },
            ifindex: before.0,
            rule_priority: before.1,
        })
    }

    /// Verify the configured source, not merely the first address on a device.
    /// Return the observed link index for the caller's route-generation witness.
    pub(crate) fn attest_source_address(
        &self,
        device: &str,
        input: &[u8],
    ) -> Result<u32, &'static str> {
        if !is_safe_identifier(device) || input.len() > 256 * 1024 {
            return Err("explicit-address-inventory-invalid");
        }
        let value: serde_json::Value =
            serde_json::from_slice(input).map_err(|_| "explicit-address-inventory-invalid")?;
        let [link] = value
            .as_array()
            .ok_or("explicit-address-inventory-invalid")?
            .as_slice()
        else {
            return Err("explicit-address-device-ambiguous");
        };
        if link.get("ifname").and_then(|name| name.as_str()) != Some(device) {
            return Err("explicit-address-device-mismatch");
        }
        let index = link
            .get("ifindex")
            .and_then(|index| index.as_u64())
            .and_then(|index| u32::try_from(index).ok())
            .filter(|index| *index != 0)
            .ok_or("explicit-address-ifindex-invalid")?;
        let flags = link
            .get("flags")
            .and_then(|flags| flags.as_array())
            .ok_or("explicit-address-link-flags-invalid")?;
        if !flags.iter().any(|flag| flag.as_str() == Some("UP")) {
            return Err("explicit-address-device-down");
        }
        let addresses = link
            .get("addr_info")
            .and_then(|addresses| addresses.as_array())
            .ok_or("explicit-address-inventory-invalid")?;
        let mut found = false;
        for address in addresses {
            if address.get("family").and_then(|family| family.as_str()) != Some("inet") {
                continue;
            }
            let local = address
                .get("local")
                .and_then(|local| local.as_str())
                .and_then(|local| local.parse::<std::net::Ipv4Addr>().ok())
                .ok_or("explicit-address-entry-invalid")?;
            if local != self.source {
                continue;
            }
            if found {
                return Err("explicit-address-source-ambiguous");
            }
            if address.get("scope").and_then(|scope| scope.as_str()) != Some("global")
                || !address
                    .get("prefixlen")
                    .and_then(|prefix| prefix.as_u64())
                    .is_some_and(|prefix| prefix <= 32)
                || !address
                    .get("valid_life_time")
                    .and_then(|life| life.as_u64())
                    .is_some_and(|life| life > 0)
                || ["tentative", "dadfailed", "deprecated"].iter().any(|flag| {
                    address
                        .get(*flag)
                        .is_some_and(|value| value.as_bool() != Some(false))
                })
            {
                return Err("explicit-address-source-unusable");
            }
            found = true;
        }
        if !found {
            return Err("explicit-address-source-unassigned");
        }
        Ok(index)
    }

    /// A rule lookup must not fall through into main. Require a usable default
    /// and ensure every more-specific entry stays on the selected device.
    /// Input is the JSON dump of this authority's table, not `route get` for a
    /// single sample destination. This does not attest address assignment.
    pub(crate) fn attest_table_routes(
        &self,
        device: &str,
        input: &[u8],
    ) -> Result<(), &'static str> {
        if !is_safe_identifier(device) || input.len() > 256 * 1024 {
            return Err("explicit-route-inventory-invalid");
        }
        let value: serde_json::Value =
            serde_json::from_slice(input).map_err(|_| "explicit-route-inventory-invalid")?;
        let routes = value.as_array().ok_or("explicit-route-inventory-invalid")?;
        if routes.len() > 4096 {
            return Err("explicit-route-inventory-invalid");
        }
        let mut default = false;
        for route in routes {
            let entry = route.as_object().ok_or("explicit-route-entry-invalid")?;
            // Unknown forwarding extensions (encap, nhid, multipath, etc.)
            // require explicit support rather than treating their outer dev
            // field as proof of the actual egress path.
            if entry.keys().any(|key| {
                !matches!(
                    key.as_str(),
                    "type"
                        | "dst"
                        | "gateway"
                        | "dev"
                        | "protocol"
                        | "scope"
                        | "prefsrc"
                        | "metric"
                        | "flags"
                        | "table"
                )
            }) {
                return Err("explicit-route-extension-unsupported");
            }
            if entry
                .get("type")
                .is_some_and(|kind| kind.as_str() != Some("unicast"))
                || entry.get("dev").and_then(|dev| dev.as_str()) != Some(device)
            {
                return Err("explicit-route-egress-unverified");
            }
            if let Some(table) = entry.get("table") {
                let table = table
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                    .or_else(|| table.as_str().and_then(|value| value.parse::<u32>().ok()));
                if table != Some(self.table) {
                    return Err("explicit-route-table-mismatch");
                }
            }
            if let Some(flags) = entry.get("flags") {
                let flags = flags.as_array().ok_or("explicit-route-entry-invalid")?;
                if flags.iter().any(|flag| flag.as_str() != Some("onlink")) {
                    return Err("explicit-route-flags-unsupported");
                }
            }
            if entry.get("gateway").is_some_and(|gateway| {
                gateway
                    .as_str()
                    .and_then(|address| address.parse::<std::net::Ipv4Addr>().ok())
                    .is_none()
            }) {
                return Err("explicit-route-gateway-invalid");
            }
            let destination = entry
                .get("dst")
                .and_then(|dst| dst.as_str())
                .ok_or("explicit-route-destination-invalid")?;
            if matches!(destination, "default" | "0.0.0.0/0") {
                default = true;
            } else {
                let (address, prefix) = destination.split_once('/').unwrap_or((destination, "32"));
                if address.parse::<std::net::Ipv4Addr>().is_err()
                    || !prefix.parse::<u8>().is_ok_and(|prefix| prefix <= 32)
                {
                    return Err("explicit-route-destination-invalid");
                }
            }
        }
        if !default {
            return Err("explicit-route-default-missing");
        }
        Ok(())
    }

    /// Prove the configured mark reaches an unconditional/source-bound table
    /// rule before any potentially matching nonlocal policy. This is only the
    /// rule witness: assignment and table routes must be attested separately.
    pub(crate) fn attest_rules(&self, rules: &str) -> Result<u32, &'static str> {
        if rules.len() > 256 * 1024 {
            return Err("explicit-rule-inventory-too-large");
        }
        let mut rows = Vec::new();
        for line in rules.lines().filter(|line| !line.trim().is_empty()) {
            if rows.len() == 4096 {
                return Err("explicit-rule-inventory-too-large");
            }
            let (priority, body) = line.split_once(':').ok_or("explicit-rule-invalid")?;
            let priority = priority
                .trim()
                .parse::<u32>()
                .map_err(|_| "explicit-rule-invalid")?;
            rows.push((priority, body.split_ascii_whitespace().collect::<Vec<_>>()));
        }
        rows.sort_unstable_by_key(|row| row.0);
        if rows.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err("explicit-rule-priority-ambiguous");
        }
        if !rows.first().is_some_and(|(priority, body)| {
            *priority == 0 && matches!(body.as_slice(), ["from", "all", "lookup", "local" | "255"])
        }) {
            return Err("explicit-local-rule-unverified");
        }
        for (priority, body) in rows.iter().skip(1) {
            let ["from", source, "fwmark", selector, "lookup", table] = body.as_slice() else {
                return Err("explicit-earlier-rule-unverified");
            };
            let (mark, mask) = selector.split_once('/').unwrap_or((selector, "0xffffffff"));
            let mark = parse_route_u32(mark).ok_or("explicit-rule-mark-invalid")?;
            let mask = parse_route_u32(mask).ok_or("explicit-rule-mark-invalid")?;
            if mask == 0 || mark & !mask != 0 {
                return Err("explicit-rule-mark-invalid");
            }
            // Owned marking preserves bits outside self.mask. Disjointness
            // must follow from guaranteed bits, not from assuming those bits 0.
            if (mark ^ self.mark) & mask & self.mask != 0 {
                continue;
            }
            let source_matches = *source == "all"
                || source
                    .strip_suffix("/32")
                    .unwrap_or(source)
                    .parse::<std::net::Ipv4Addr>()
                    .is_ok_and(|source| source == self.source);
            if source_matches
                && mark == self.mark
                && mask == self.mask
                && table.parse::<u32>().ok() == Some(self.table)
            {
                return Ok(*priority);
            }
            return Err("explicit-earlier-rule-unverified");
        }
        Err("explicit-mark-table-rule-missing")
    }

    pub(crate) fn from_fields(mode: &str, fields: [&str; 4]) -> Result<Option<Self>, String> {
        if mode != "explicit" {
            return if fields.iter().all(|value| value.is_empty()) {
                Ok(None)
            } else {
                Err("explicit route authority fields require route_mode=explicit".into())
            };
        }
        let [source, table, mark, mask] = fields;
        let source = source
            .parse::<std::net::Ipv4Addr>()
            .map_err(|_| "route_source_ipv4 must be an IPv4 address")?;
        if source.octets()[0] == 0 || source.is_loopback() || source.octets()[0] >= 224 {
            return Err("route_source_ipv4 must be a unicast source".into());
        }
        if table.is_empty() || !table.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("route_table must be a nonzero numeric table".into());
        }
        let table = table
            .parse::<u32>()
            .ok()
            .filter(|value| *value != 0)
            .ok_or("route_table must be a nonzero numeric table")?;
        let mark =
            parse_route_u32(mark).ok_or("route_fwmark must be a u32 decimal or hex value")?;
        let mask =
            parse_route_u32(mask).ok_or("route_fwmark_mask must be a u32 decimal or hex value")?;
        if mark == 0 || mask == 0 || mark & !mask != 0 {
            return Err(
                "explicit route mark must be nonzero and contained in its nonzero mask".into(),
            );
        }
        Ok(Some(Self {
            source,
            table,
            mark,
            mask,
        }))
    }
}

pub(crate) fn parse_route_u32(value: &str) -> Option<u32> {
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        if hex.is_empty() || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        u32::from_str_radix(hex, 16).ok()
    } else if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        value.parse::<u32>().ok()
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteMode {
    Main,
    Mwan3,
    Explicit,
}

impl RouteMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Mwan3 => "mwan3",
            Self::Explicit => "explicit",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteSpec {
    pub configured_mode: String,
    pub member: String,
    pub expected_device: String,
    pub(crate) explicit_authority: Option<ExplicitRouteAuthority>,
}

impl RouteSpec {
    pub fn new(configured_mode: &str, member: &str, expected_device: &str) -> Self {
        Self {
            configured_mode: configured_mode.to_string(),
            member: member.to_string(),
            expected_device: expected_device.to_string(),
            explicit_authority: None,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        // Keep runtime admission closed until every producer, request and
        // recovery consumer shares the explicit authority contract.
        if self.configured_mode == "explicit" {
            if !self.member.is_empty() {
                return Err("route_mode=explicit must not define mwan3_member".into());
            }
            if self.explicit_authority.is_none() {
                return Err("explicit route authority is incomplete".into());
            }
            return Err("explicit PBR runtime enforcement is not available in this build".into());
        }
        if self.explicit_authority.is_some() {
            return Err("explicit route authority fields require route_mode=explicit".into());
        }
        if !matches!(self.configured_mode.as_str(), "auto" | "main" | "mwan3") {
            return Err("route_mode must be auto, main, or mwan3".to_string());
        }
        if !self.member.is_empty() && !is_safe_identifier(&self.member) {
            return Err("mwan3_member contains unsupported characters".to_string());
        }
        if self.configured_mode == "mwan3" && self.member.is_empty() {
            return Err("route_mode=mwan3 requires mwan3_member".to_string());
        }
        if self.configured_mode == "main" && !self.member.is_empty() {
            return Err("route_mode=main must not define mwan3_member".to_string());
        }
        if self.expected_device.is_empty() || !is_safe_identifier(&self.expected_device) {
            return Err("route device contains unsupported characters".to_string());
        }
        Ok(())
    }

    pub fn effective_mode(&self) -> Result<RouteMode, String> {
        self.validate()?;
        match self.configured_mode.as_str() {
            "main" => Ok(RouteMode::Main),
            "mwan3" => Ok(RouteMode::Mwan3),
            "explicit" => Ok(RouteMode::Explicit),
            "auto" if self.member.is_empty() => Ok(RouteMode::Main),
            "auto" => {
                if command_available("mwan3") {
                    Ok(RouteMode::Mwan3)
                } else {
                    Err(format!(
                        "mwan3_member={} is configured, but mwan3 is unavailable",
                        self.member
                    ))
                }
            }
            _ => unreachable!(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteIdentity {
    /// None preserves historical main/mwan3 fingerprints. Explicit observation
    /// carries the current link index; this alone is not an ABA-proof epoch.
    pub device_ifindex: Option<u32>,
    pub fwmark_mask: Option<u32>,
    pub mode: String,
    pub member: String,
    pub device: String,
    pub source_ip: String,
    pub fwmark: String,
    pub table: String,
}

impl RouteIdentity {
    pub fn stable_key(&self) -> String {
        let key = format!(
            "{}|{}|{}|{}|{}|{}",
            self.mode, self.member, self.device, self.source_ip, self.fwmark, self.table
        );
        let key = match self.fwmark_mask {
            Some(mask) => format!("{key}|mask={mask}"),
            None => key,
        };
        match self.device_ifindex {
            Some(index) => format!("{key}|ifindex={index}"),
            None => key,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteSnapshot {
    pub identity: RouteIdentity,
    pub online: bool,
    pub active: bool,
    pub member_status: String,
    pub reason: String,
}

pub struct RouteInspector {
    spec: RouteSpec,
    owned_explicit_probe: bool,
}

impl RouteInspector {
    pub fn new(spec: RouteSpec) -> Self {
        Self {
            spec,
            owned_explicit_probe: false,
        }
    }

    /// Restricted worker lane. The caller must retain the producer until the
    /// worker is joined; ordinary inspectors still obey product admission.
    #[cfg(feature = "transport-probes")]
    pub(crate) fn for_owned_probe(
        spec: RouteSpec,
        producer: &crate::permanent_probe_owner::PermanentProbeProducer,
    ) -> Result<Self, String> {
        if spec.configured_mode != "explicit"
            || !spec.member.is_empty()
            || spec.expected_device != producer.route().device
        {
            return Err("owned route inspector configuration differs from probe owner".into());
        }
        spec.explicit_authority
            .as_ref()
            .ok_or("owned route inspector authority missing")?
            .attest_identity(producer.route())?;
        Ok(Self {
            spec,
            owned_explicit_probe: true,
        })
    }

    pub fn inspect(&mut self) -> Result<RouteSnapshot, String> {
        self.inspect_fresh()
    }

    pub fn inspect_fresh(&mut self) -> Result<RouteSnapshot, String> {
        if self.owned_explicit_probe {
            return inspect_explicit(&self.spec);
        }
        match self.spec.effective_mode()? {
            RouteMode::Main => inspect_main(&self.spec),
            RouteMode::Explicit => inspect_explicit(&self.spec),
            RouteMode::Mwan3 => {
                let default_policy = mwan3_default_policy()?;
                inspect_mwan3_with_policy(&self.spec, default_policy.as_deref())
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UplinkState {
    Active,
    Standby,
    Rechecking,
    Offline,
    Learning,
}

impl UplinkState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "ACTIVE",
            Self::Standby => "STANDBY",
            Self::Rechecking => "RECHECKING",
            Self::Offline => "OFFLINE",
            Self::Learning => "LEARNING",
        }
    }
}

#[derive(Clone, Debug)]
pub struct UplinkTransition {
    pub state: UplinkState,
    pub reason: String,
    pub identity_changed: bool,
    pub became_offline: bool,
    pub reset_learning: bool,
    pub probes_allowed: bool,
}

impl UplinkTransition {
    /// Every route observation path must retire old producers on the same
    /// authority boundary, including observations triggered by pinger failure.
    pub(crate) fn retires_probes(&self, previously_allowed: bool) -> bool {
        self.became_offline || self.identity_changed || (previously_allowed && !self.probes_allowed)
    }
}

#[derive(Clone, Debug)]
pub struct UplinkLifecycle {
    state: UplinkState,
    identity: Option<String>,
    confirmation_candidate: Option<String>,
    route_confirmed: bool,
    learned: bool,
    learning_samples: usize,
    active_route: bool,
    reason: String,
}

impl UplinkLifecycle {
    pub fn new() -> Self {
        Self {
            state: UplinkState::Offline,
            identity: None,
            confirmation_candidate: None,
            route_confirmed: false,
            learned: false,
            learning_samples: 0,
            active_route: false,
            reason: "route not checked".to_string(),
        }
    }

    pub fn observe(&mut self, snapshot: Result<&RouteSnapshot, &str>) -> UplinkTransition {
        let previous_state = self.state;

        let snapshot = match snapshot {
            Ok(snapshot) if snapshot.online => snapshot,
            Ok(snapshot) if Self::transient_member_state(snapshot) => {
                // A connecting/disconnecting status is not proof that the
                // route is offline.  Revoke probe admission until a fresh,
                // exact route snapshot arrives, but preserve the learned
                // baseline and the last confirmed identity.
                self.confirmation_candidate = None;
                self.state = UplinkState::Rechecking;
                self.reason = if snapshot.reason.is_empty() {
                    format!(
                        "member {} is {}; waiting for an exact route observation",
                        snapshot.identity.member, snapshot.member_status
                    )
                } else {
                    snapshot.reason.clone()
                };
                return self.transition(previous_state, false, false, false);
            }
            Ok(snapshot) => {
                // Only an explicit, successfully inspected offline snapshot
                // may assert OFFLINE.  Elapsed time and repeated inspection
                // failures must never manufacture this state.
                let reset_learning = self.route_confirmed
                    || self.learned
                    || self.confirmation_candidate.is_some()
                    || previous_state != UplinkState::Offline;
                self.state = UplinkState::Offline;
                self.confirmation_candidate = None;
                self.route_confirmed = false;
                self.learned = false;
                self.learning_samples = 0;
                self.active_route = false;
                self.reason = if snapshot.reason.is_empty() {
                    format!("member {} is offline", snapshot.identity.member)
                } else {
                    snapshot.reason.clone()
                };
                return self.transition(previous_state, false, reset_learning, false);
            }
            Err(error) => {
                // An inspection error proves only that the route is unknown.
                // It immediately closes probe admission, but it neither
                // destroys a learned baseline nor becomes OFFLINE after an
                // arbitrary number of observations.
                self.confirmation_candidate = None;
                self.state = UplinkState::Rechecking;
                self.reason = format!("route status unavailable: {error}");
                return self.transition(previous_state, false, false, false);
            }
        };

        let identity = snapshot.stable_key();
        let identity_changed = self.identity.as_deref() != Some(identity.as_str());
        self.active_route = snapshot.active;

        if identity_changed {
            self.identity = Some(identity.clone());
            self.confirmation_candidate = Some(identity);
            self.route_confirmed = false;
            self.learned = false;
            self.learning_samples = 0;
            self.state = UplinkState::Learning;
            self.reason = "waiting for repeated matching route identity".to_string();
            return self.transition(previous_state, true, true, false);
        }

        if !self.route_confirmed {
            if self.confirmation_candidate.as_deref() != Some(identity.as_str()) {
                self.confirmation_candidate = Some(identity);
                self.state = UplinkState::Learning;
                self.reason = "waiting for repeated matching route identity".to_string();
                return self.transition(
                    previous_state,
                    false,
                    previous_state == UplinkState::Offline,
                    false,
                );
            }
            self.confirmation_candidate = None;
            self.route_confirmed = true;
        }

        if !self.learned {
            self.state = UplinkState::Learning;
            self.reason = "learning latency baseline".to_string();
        } else {
            self.state = if snapshot.active {
                UplinkState::Active
            } else {
                UplinkState::Standby
            };
            self.reason = snapshot.reason.clone();
        }

        self.transition(previous_state, false, false, true)
    }

    pub fn record_learning_sample(&mut self, required_samples: usize) -> bool {
        if self.state != UplinkState::Learning || !self.route_confirmed {
            return false;
        }
        self.learning_samples = self.learning_samples.saturating_add(1);
        if self.learning_samples < required_samples.max(1) {
            return false;
        }
        self.learned = true;
        self.state = if self.active_route {
            UplinkState::Active
        } else {
            UplinkState::Standby
        };
        if self.state == UplinkState::Active {
            self.reason.clear();
        } else {
            self.reason = "standby: forced probes remain isolated to this uplink".to_string();
        }
        true
    }

    pub fn state(&self) -> UplinkState {
        self.state
    }

    fn transient_member_state(snapshot: &RouteSnapshot) -> bool {
        matches!(
            snapshot.member_status.as_str(),
            "connecting" | "disconnecting"
        )
    }

    pub fn reason(&self) -> &str {
        &self.reason
    }

    fn transition(
        &self,
        previous_state: UplinkState,
        identity_changed: bool,
        reset_learning: bool,
        probes_allowed: bool,
    ) -> UplinkTransition {
        UplinkTransition {
            state: self.state,
            reason: self.reason.clone(),
            identity_changed,
            became_offline: self.state == UplinkState::Offline
                && previous_state != UplinkState::Offline,
            reset_learning,
            probes_allowed,
        }
    }
}

impl RouteSnapshot {
    pub fn stable_key(&self) -> String {
        self.identity.stable_key()
    }
}

pub fn routed_command(
    spec: &RouteSpec,
    legacy_prefix: &str,
    binary: &str,
) -> Result<Command, String> {
    if binary.is_empty() || binary.chars().any(char::is_whitespace) {
        return Err("routed command binary is invalid".to_string());
    }

    match spec.effective_mode()? {
        RouteMode::Explicit => {
            Err("explicit routed commands require an admitted probe owner".into())
        }
        RouteMode::Mwan3 => {
            if !legacy_prefix.trim().is_empty() {
                return Err(
                    "ping_prefix_string must be empty when structured mwan3 routing is used"
                        .to_string(),
                );
            }
            let mut command = Command::new("mwan3");
            command.arg("use").arg(&spec.member).arg("exec").arg(binary);
            Ok(command)
        }
        RouteMode::Main => {
            let prefix = safe_command_words(legacy_prefix)?;
            if prefix.is_empty() {
                Ok(Command::new(binary))
            } else {
                let mut command = Command::new(&prefix[0]);
                command.args(&prefix[1..]).arg(binary);
                Ok(command)
            }
        }
    }
}

pub fn inspect_route(spec: &RouteSpec) -> Result<RouteSnapshot, String> {
    match spec.effective_mode()? {
        RouteMode::Main => inspect_main(spec),
        RouteMode::Explicit => inspect_explicit(spec),
        RouteMode::Mwan3 => {
            let default_policy = mwan3_default_policy()?;
            inspect_mwan3_with_policy(spec, default_policy.as_deref())
        }
    }
}

fn inspect_explicit(spec: &RouteSpec) -> Result<RouteSnapshot, String> {
    if spec.configured_mode != "explicit" || !spec.member.is_empty() {
        return Err("explicit inspector requires independent configured authority".into());
    }
    let observation = spec
        .explicit_authority
        .as_ref()
        .ok_or("explicit route authority is incomplete")?
        .observe_system(&spec.expected_device)?;
    Ok(RouteSnapshot {
        identity: observation.identity,
        online: true,
        active: true,
        member_status: String::new(),
        reason: String::new(),
    })
}

pub(crate) fn validate_external_ip_url(value: &str) -> Result<(), String> {
    let invalid = || {
        "external_ip_check_url must be an unauthenticated HTTPS host/path (no query or fragment)"
            .to_string()
    };
    if value.len() > 1024
        || !value.is_ascii()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err(invalid());
    }
    let remainder = value.strip_prefix("https://").ok_or_else(invalid)?;
    if remainder.contains(['@', '?', '#', '\\']) {
        return Err(invalid());
    }
    let (authority, path) = remainder.split_once('/').unwrap_or((remainder, ""));
    let host = if let Some((host, port)) = authority.split_once(':') {
        if port.parse::<u16>().ok().is_none_or(|port| port == 0) {
            return Err(invalid());
        }
        host
    } else {
        authority
    };
    if host.is_empty()
        || host.len() > 253
        || host.split('.').any(|label| {
            label.is_empty()
                || label.len() > 63
                || label.starts_with('-')
                || label.ends_with('-')
                || !label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
    {
        return Err(invalid());
    }
    if !path
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"/-._~%".contains(&byte))
    {
        return Err(invalid());
    }
    let mut bytes = path.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%'
            && !(bytes.next().is_some_and(|value| value.is_ascii_hexdigit())
                && bytes.next().is_some_and(|value| value.is_ascii_hexdigit()))
        {
            return Err(invalid());
        }
    }
    Ok(())
}

fn external_ip_command(
    spec: &RouteSpec,
    timeout_s: u64,
    endpoint: &str,
) -> Result<crate::operations::process::SpawnSpec, String> {
    validate_external_ip_url(endpoint)?;
    let mut command = routed_command(spec, "", "/bin/uclient-fetch")?;
    command
        .arg("-q")
        .arg("-4")
        .arg("-T")
        .arg(timeout_s.clamp(1, 30).to_string())
        .arg("-O")
        .arg("-")
        .arg(endpoint);
    Ok(crate::operations::process::SpawnSpec {
        program: if command.get_program() == "mwan3" {
            "/usr/sbin/mwan3".into()
        } else {
            command.get_program().into()
        },
        arguments: command.get_args().map(ToOwned::to_owned).collect(),
        environment: vec![("LC_ALL".into(), "C".into())],
    })
}

pub fn external_ipv4(spec: &RouteSpec, timeout_s: u64, endpoint: &str) -> Result<String, String> {
    let command = external_ip_command(spec, timeout_s, endpoint)?;
    // A configured endpoint must not hold a worker or grow its capture buffer
    // indefinitely. The client timeout alone is not an absolute output bound.
    let output = crate::operations::process::run_bounded_command_output(
        &command,
        std::time::Duration::from_secs(timeout_s.clamp(1, 30)),
        1024,
        || crate::TERMINATE.load(std::sync::atomic::Ordering::SeqCst),
    )?;
    if !output.status.success() {
        return Err(format!("external IP query exited with {}", output.status));
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if valid_ipv4(&value) {
        Ok(value)
    } else {
        Err("external IP query returned an invalid IPv4 address".to_string())
    }
}

fn inspect_main(spec: &RouteSpec) -> Result<RouteSnapshot, String> {
    let device_path = format!("/sys/class/net/{}", spec.expected_device);
    let device_online = Path::new(&device_path).exists();
    if !device_online {
        return Ok(RouteSnapshot {
            identity: RouteIdentity {
                device_ifindex: None,
                fwmark_mask: None,
                mode: RouteMode::Main.as_str().to_string(),
                member: String::new(),
                device: spec.expected_device.clone(),
                source_ip: String::new(),
                fwmark: String::new(),
                table: "main".to_string(),
            },
            online: false,
            active: false,
            member_status: "offline".to_string(),
            reason: format!("interface {} is unavailable", spec.expected_device),
        });
    }

    let source_ip = interface_source_ip(&spec.expected_device)?.unwrap_or_default();
    if source_ip.is_empty() {
        let ipv6_only = interface_has_global_ipv6(&spec.expected_device)?;
        return Ok(RouteSnapshot {
            identity: RouteIdentity {
                device_ifindex: None,
                fwmark_mask: None,
                mode: RouteMode::Main.as_str().to_string(),
                member: String::new(),
                device: spec.expected_device.clone(),
                source_ip,
                fwmark: String::new(),
                table: "main".to_string(),
            },
            online: false,
            active: false,
            member_status: if ipv6_only {
                "unsupported_family"
            } else {
                "connecting"
            }
            .to_string(),
            reason: missing_ipv4_reason(&spec.expected_device, ipv6_only),
        });
    }

    let default_device = default_route_device()?.unwrap_or_default();
    let default_matches = device_online && default_device == spec.expected_device;
    let policy_problem = if default_matches {
        let rules = checked_output(
            "ip",
            &["-4", "rule", "show"],
            "failed to inspect main IPv4 policy rules",
        )?;
        let routes = checked_output(
            "ip",
            &["-4", "route", "show", "table", "main"],
            "failed to inspect main IPv4 route overrides",
        )?;
        main_policy_problem(
            &String::from_utf8_lossy(&rules.stdout),
            &String::from_utf8_lossy(&routes.stdout),
            &spec.expected_device,
        )
    } else {
        None
    };
    let active = default_matches && policy_problem.is_none();
    let online = active;
    let reason = if let Some(problem) = policy_problem {
        problem.to_string()
    } else if !active {
        if default_device.is_empty() {
            "main IPv4 default route is missing or ambiguous; uplink probing is not authorized"
                .to_string()
        } else {
            format!("main default route uses {default_device}, expected {}; VPN/PBR paths require explicit route binding", spec.expected_device)
        }
    } else {
        String::new()
    };

    Ok(RouteSnapshot {
        identity: RouteIdentity {
            device_ifindex: None,
            fwmark_mask: None,
            mode: RouteMode::Main.as_str().to_string(),
            member: String::new(),
            device: spec.expected_device.clone(),
            source_ip,
            fwmark: String::new(),
            table: "main".to_string(),
        },
        online,
        active,
        member_status: if online { "online" } else { "route_mismatch" }.to_string(),
        reason,
    })
}

fn main_policy_problem(rules: &str, routes: &str, expected_device: &str) -> Option<&'static str> {
    // main carries neither a policy mark nor an admitted table. Do not infer
    // policy authority from a /0 label or silently ignore unknown selectors.
    let mut seen = [false; 3];
    for line in rules.lines().filter(|line| !line.trim().is_empty()) {
        let words: Vec<_> = line.split_whitespace().collect();
        let index = match words.as_slice() {
            ["0:", "from", "all", "lookup", "local" | "255"] => 0,
            ["32766:", "from", "all", "lookup", "main" | "254"] => 1,
            ["32767:", "from", "all", "lookup", "default" | "253"] => 2,
            _ => return Some("main route has unsupported IPv4 policy rules; explicit route authority is required before probes or tests"),
        };
        if seen[index] {
            return Some(
                "main IPv4 policy rules are ambiguous; probes and tests are not authorized",
            );
        }
        seen[index] = true;
    }
    if !seen.into_iter().all(|present| present) {
        return Some("main IPv4 policy rules are incomplete; probes and tests are not authorized");
    }
    for line in routes.lines() {
        let words: Vec<_> = line.split_whitespace().collect();
        if matches!(words.first(), Some(&"0.0.0.0/1" | &"128.0.0.0/1")) {
            let devices: Vec<_> = words
                .windows(2)
                .filter(|pair| pair[0] == "dev")
                .map(|pair| pair[1])
                .collect();
            if devices.as_slice() != [expected_device] {
                return Some("main route is overridden by split-default routing; explicit route authority is required before probes or tests");
            }
        }
    }
    None
}

fn inspect_mwan3_with_policy(
    spec: &RouteSpec,
    default_policy: Option<&str>,
) -> Result<RouteSnapshot, String> {
    ensure_nft_mwan3()?;
    let request = format!(r#"{{"interface":"{}"}}"#, spec.member);
    let mwan_status = run_output("ubus", &["call", "mwan3", "status", &request])
        .map_err(|error| format!("failed to inspect mwan3 member {}: {error}", spec.member))?;
    if !mwan_status.status.success() {
        return Err(format!(
            "mwan3 status failed for {}: {}",
            spec.member,
            output_error(&mwan_status)
        ));
    }
    let mwan_json = String::from_utf8_lossy(&mwan_status.stdout);
    let member_status = json_string_value(&mwan_json, "status")
        .ok_or_else(|| format!("mwan3 status for {} has no status field", spec.member))?;
    let running = json_bool_value(&mwan_json, "running")
        .ok_or_else(|| format!("mwan3 status for {} has no running field", spec.member))?;
    let member_up = json_bool_value(&mwan_json, "up")
        .ok_or_else(|| format!("mwan3 status for {} has no up field", spec.member))?;
    let enabled = json_bool_value(&mwan_json, "enabled")
        .ok_or_else(|| format!("mwan3 status for {} has no enabled field", spec.member))?;

    let network_object = format!("network.interface.{}", spec.member);
    let network_status = run_output("ubus", &["call", &network_object, "status"])
        .map_err(|error| format!("failed to inspect {network_object}: {error}"))?;
    if !network_status.status.success() {
        return Err(format!(
            "network status failed for {}: {}",
            spec.member,
            output_error(&network_status)
        ));
    }
    let network_json = String::from_utf8_lossy(&network_status.stdout);
    let network_up = json_bool_value(&network_json, "up")
        .ok_or_else(|| format!("network status for {} has no up field", spec.member))?;
    let network_device = json_string_value(&network_json, "l3_device")
        .or_else(|| json_string_value(&network_json, "device"))
        .unwrap_or_default();

    if !(enabled && running && member_up && network_up && member_status == "online") {
        let reason = if !enabled {
            format!("member {} is disabled", spec.member)
        } else if matches!(member_status.as_str(), "connecting" | "disconnecting") {
            format!("member {} is {member_status}", spec.member)
        } else if !running || !network_up {
            format!("member {} interface is down", spec.member)
        } else {
            format!("member {} is {member_status}", spec.member)
        };
        return Ok(RouteSnapshot {
            identity: RouteIdentity {
                device_ifindex: None,
                fwmark_mask: None,
                mode: RouteMode::Mwan3.as_str().to_string(),
                member: spec.member.clone(),
                device: if network_device.is_empty() {
                    spec.expected_device.clone()
                } else {
                    network_device
                },
                source_ip: String::new(),
                fwmark: String::new(),
                table: String::new(),
            },
            online: false,
            active: false,
            member_status,
            reason,
        });
    }

    let environment = run_output("mwan3", &["use", &spec.member, "exec", "env"])
        .map_err(|error| format!("failed to resolve mwan3 route {}: {error}", spec.member))?;
    if !environment.status.success() {
        return Err(format!(
            "mwan3 use {} failed: {}",
            spec.member,
            output_error(&environment)
        ));
    }
    let environment = String::from_utf8_lossy(&environment.stdout);
    let device = env_value(&environment, "DEVICE")
        .ok_or_else(|| format!("mwan3 route {} has no unique device", spec.member))?;
    if device.is_empty() {
        return Err(format!(
            "mwan3 route {} has no resolved device",
            spec.member
        ));
    }
    let source_ip = env_value(&environment, "SRCIP")
        .filter(|value| valid_ipv4(value))
        .ok_or_else(|| format!("mwan3 route {} has no valid IPv4 source", spec.member))?;
    let (fwmark, table, mask) = routing_for_device(&device)?.ok_or_else(|| {
        format!(
            "mwan3 route {} has no unambiguous unconditional mark/table authority",
            spec.member
        )
    })?;
    attest_mwan3_environment_mask(&environment, mask)?;

    let device_matches = device == spec.expected_device;
    let online = enabled
        && running
        && member_up
        && network_up
        && member_status == "online"
        && device_matches;
    let policy_percent = default_policy
        .and_then(|policy| json_policy_member_percent(&mwan_json, policy, &spec.member));
    let default_device = if policy_percent.is_none() {
        default_route_device()?.unwrap_or_default()
    } else {
        String::new()
    };
    let active = online
        && policy_percent
            .map(|percent| percent > 0)
            .unwrap_or(default_device == device);
    let reason = if !device_matches {
        format!(
            "route mismatch: member {} uses {}, expected {}",
            spec.member, device, spec.expected_device
        )
    } else if !enabled {
        format!("member {} is disabled", spec.member)
    } else if !running || !network_up {
        format!("member {} interface is down", spec.member)
    } else if !member_up || member_status != "online" {
        format!("member {} is {member_status}", spec.member)
    } else if !active {
        match (default_policy, policy_percent) {
            (Some(policy), Some(percent)) => {
                format!("standby: mwan3 policy {policy} assigns {percent}%")
            }
            _ => format!("standby: main default route uses {default_device}"),
        }
    } else {
        String::new()
    };

    Ok(RouteSnapshot {
        identity: RouteIdentity {
            device_ifindex: None,
            mode: RouteMode::Mwan3.as_str().to_string(),
            member: spec.member.clone(),
            device,
            source_ip,
            fwmark,
            table,
            fwmark_mask: Some(mask),
        },
        online,
        active,
        member_status,
        reason,
    })
}

fn ensure_nft_mwan3() -> Result<(), String> {
    static NFT_MWAN3_READY: OnceLock<()> = OnceLock::new();
    if NFT_MWAN3_READY.get().is_some() {
        return Ok(());
    }
    let output = run_output("nft", &["list", "table", "inet", "mwan3"])
        .map_err(|error| format!("nftables mwan3 backend is unavailable: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "nftables table inet mwan3 is unavailable: {}",
            output_error(&output)
        ));
    }
    let _ = NFT_MWAN3_READY.set(());
    Ok(())
}

pub fn is_safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':' | '@'))
}

fn safe_command_words(value: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    for word in value.split_whitespace() {
        if word.is_empty() {
            continue;
        }
        let safe = word.len() <= 128
            && word.chars().all(|ch| {
                ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':' | '/' | '=')
            });
        if !safe {
            return Err(format!(
                "ping_prefix_string contains unsupported argument token: {word}"
            ));
        }
        words.push(word.to_string());
    }
    Ok(words)
}

fn command_available(binary: &str) -> bool {
    if binary.contains('/') {
        return Path::new(binary).is_file();
    }
    env::var_os("PATH")
        .map(|paths| env::split_paths(&paths).any(|path| path.join(binary).is_file()))
        .unwrap_or(false)
}

fn run_output(binary: &str, args: &[&str]) -> io::Result<Output> {
    Command::new(binary).args(args).output()
}

fn checked_output(binary: &str, args: &[&str], purpose: &str) -> Result<Output, String> {
    let output = run_output(binary, args)
        .map_err(|error| format!("{purpose}: failed to execute {binary}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{purpose}: {binary} failed: {}",
            output_error(&output)
        ));
    }
    Ok(output)
}

fn output_error(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        output.status.to_string()
    } else {
        stderr
    }
}

fn json_key_tail<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let marker = format!("\"{key}\"");
    let start = json.find(&marker)? + marker.len();
    let tail = json[start..].trim_start();
    tail.strip_prefix(':').map(str::trim_start)
}

fn json_string_value(json: &str, key: &str) -> Option<String> {
    let tail = json_key_tail(json, key)?.strip_prefix('"')?;
    let end = tail.find('"')?;
    Some(tail[..end].to_string())
}

fn json_bool_value(json: &str, key: &str) -> Option<bool> {
    let tail = json_key_tail(json, key)?;
    if tail.starts_with("true") {
        Some(true)
    } else if tail.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

fn json_u64_value(json: &str, key: &str) -> Option<u64> {
    let tail = json_key_tail(json, key)?;
    let end = tail
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(tail.len());
    tail.get(..end)?.parse().ok()
}

fn json_policy_member_percent(json: &str, policy: &str, member: &str) -> Option<u64> {
    if !is_safe_identifier(policy) || !is_safe_identifier(member) {
        return None;
    }
    let marker = format!("\"{policy}\"");
    let policy_tail = json.get(json.find(&marker)? + marker.len()..)?;
    let array = policy_tail.get(policy_tail.find('[')? + 1..)?;
    let array = array.get(..array.find(']')?)?;

    array.split('{').skip(1).find_map(|tail| {
        let object = tail.get(..tail.find('}')?)?;
        if json_string_value(object, "interface").as_deref() == Some(member) {
            json_u64_value(object, "percent")
        } else {
            None
        }
    })
}

fn mwan3_default_policy() -> Result<Option<String>, String> {
    let output = checked_output(
        "uci",
        &["-q", "show", "mwan3"],
        "failed to resolve the default mwan3 policy",
    )?;
    Ok(parse_mwan3_default_policy(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn parse_mwan3_default_policy(config: &str) -> Option<String> {
    let sections: Vec<&str> = config
        .lines()
        .filter_map(|line| {
            let section = line.strip_prefix("mwan3.")?.strip_suffix("=rule")?;
            is_safe_identifier(section).then_some(section)
        })
        .collect();

    sections.into_iter().find_map(|section| {
        let enabled = uci_option(config, section, "enabled").unwrap_or_else(|| "1".to_string());
        let family = uci_option(config, section, "family").unwrap_or_else(|| "ipv4".to_string());
        let destination = uci_option(config, section, "dest_ip").unwrap_or_default();
        let broad = ["src_ip", "src_port", "dest_port", "proto", "ipset"]
            .iter()
            .all(|option| uci_option(config, section, option).is_none());
        let policy = uci_option(config, section, "use_policy")?;
        (enabled != "0"
            && family != "ipv6"
            && (destination.is_empty() || destination == "0.0.0.0/0")
            && broad
            && is_safe_identifier(&policy))
        .then_some(policy)
    })
}

fn uci_option(config: &str, section: &str, option: &str) -> Option<String> {
    let prefix = format!("mwan3.{section}.{option}=");
    config.lines().find_map(|line| {
        line.strip_prefix(&prefix).map(|value| {
            value
                .trim()
                .trim_matches(|ch| ch == '\'' || ch == '"')
                .to_string()
        })
    })
}

fn env_value(environment: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    let mut values = environment
        .lines()
        .filter_map(|line| line.strip_prefix(&prefix));
    let value = values.next()?;
    if values.next().is_some() || value.is_empty() || value.chars().any(char::is_whitespace) {
        return None;
    }
    Some(value.to_string())
}

fn attest_mwan3_environment_mask(environment: &str, rule_mask: u32) -> Result<(), String> {
    let mask = env_value(environment, "FWMARK").and_then(|value| parse_route_u32(&value));
    if rule_mask == 0 || mask != Some(rule_mask) {
        return Err("mwan3 wrapper mask does not match routing-rule authority".to_string());
    }
    Ok(())
}

fn default_route_device() -> Result<Option<String>, String> {
    if let Ok(routes) = std::fs::read_to_string("/proc/net/route") {
        if let Some(device) = parse_proc_default_route_device(&routes) {
            return Ok(Some(device));
        }
    }
    let output = checked_output(
        "ip",
        &["-4", "route", "show", "default"],
        "failed to inspect the IPv4 default route",
    )?;
    Ok(parse_default_route_device(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

#[derive(Default)]
struct DefaultRoutePick<'a> {
    metric: Option<u32>,
    device: Option<&'a str>,
}
impl<'a> DefaultRoutePick<'a> {
    fn consider(&mut self, metric: u32, device: Option<&'a str>) {
        match self.metric {
            None => {
                self.metric = Some(metric);
                self.device = device;
            }
            Some(best) if metric < best => {
                self.metric = Some(metric);
                self.device = device;
            }
            Some(best) if metric == best && self.device != device => self.device = None,
            _ => {}
        }
    }
}

fn parse_proc_default_route_device(routes: &str) -> Option<String> {
    let mut pick = DefaultRoutePick::default();
    for line in routes.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.get(1) != Some(&"00000000") {
            continue;
        }
        // Destination zero alone also describes 0.0.0.0/1 (split VPN routes).
        if u32::from_str_radix(fields.get(7)?, 16).ok()? != 0 {
            continue;
        }
        let flags = u32::from_str_radix(fields.get(3)?, 16).ok()?;
        if flags & 0x0001 == 0 {
            continue;
        } // RTF_UP: usable route.
        let metric = fields.get(6)?.parse::<u32>().ok()?;
        let device = *fields.first()?;
        if !is_safe_identifier(device) {
            return None;
        }
        // A lower-metric reject default blocks a higher-metric unicast route.
        pick.consider(metric, (flags & 0x0200 == 0).then_some(device)); // RTF_REJECT.
    }
    pick.device.map(str::to_owned)
}

struct TextDefaultRoute<'a> {
    metric: u32,
    device: Option<&'a str>,
    unusable: bool,
}
impl<'a> TextDefaultRoute<'a> {
    fn observe_devices(&mut self, words: &[&'a str]) {
        for (index, word) in words.iter().enumerate() {
            if matches!(*word, "dead" | "linkdown" | "nhid") {
                self.unusable = true;
            }
            if *word != "dev" {
                continue;
            }
            let Some(device) = words
                .get(index + 1)
                .copied()
                .filter(|value| is_safe_identifier(value))
            else {
                self.unusable = true;
                continue;
            };
            if self.device.is_some_and(|previous| previous != device) {
                self.unusable = true;
            }
            self.device = Some(device);
        }
    }
    fn finish(self, pick: &mut DefaultRoutePick<'a>) {
        pick.consider(self.metric, if self.unusable { None } else { self.device });
    }
}

fn parse_default_route_device(routes: &str) -> Option<String> {
    let mut pick = DefaultRoutePick::default();
    let mut current: Option<TextDefaultRoute<'_>> = None;
    for line in routes.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.first() == Some(&"nexthop") {
            current.as_mut()?.observe_devices(&words);
            continue;
        }
        if let Some(previous) = current.take() {
            previous.finish(&mut pick);
        }
        let default = words.first() == Some(&"default") || words.get(1) == Some(&"default");
        if !default {
            continue;
        }
        let mut metrics = words
            .iter()
            .enumerate()
            .filter(|(_, word)| **word == "metric");
        let metric = match metrics.next() {
            Some((index, _)) => words.get(index + 1)?.parse::<u32>().ok()?,
            None => 0,
        };
        if metrics.next().is_some() {
            return None;
        }
        let mut route = TextDefaultRoute {
            metric,
            device: None,
            unusable: !matches!(words.first(), Some(&"default" | &"unicast")),
        };
        route.observe_devices(&words);
        current = Some(route);
    }
    if let Some(last) = current {
        last.finish(&mut pick);
    }
    pick.device.map(str::to_owned)
}

fn interface_source_ip(device: &str) -> Result<Option<String>, String> {
    let output = checked_output(
        "ip",
        &["-4", "-o", "addr", "show", "dev", device, "scope", "global"],
        &format!("failed to inspect IPv4 addresses on {device}"),
    )?;
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .split_whitespace()
        .skip_while(|word| *word != "inet")
        .nth(1)
        .and_then(|value| value.split('/').next())
        .filter(|value| valid_ipv4(value))
        .map(str::to_string))
}

fn missing_ipv4_reason(device: &str, has_ipv6: bool) -> String {
    if has_ipv6 {
        format!("interface {device} has IPv6 but no IPv4 source; IPv6-only route/probe support is not implemented")
    } else {
        format!("interface {device} has no IPv4 source address; waiting for route identity")
    }
}

fn interface_has_global_ipv6(device: &str) -> Result<bool, String> {
    let output = checked_output(
        "ip",
        &["-6", "-o", "addr", "show", "dev", device, "scope", "global"],
        "failed to inspect IPv6-only route capability",
    )?;
    Ok(has_global_ipv6_address(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn has_global_ipv6_address(output: &str) -> bool {
    output.lines().any(|line| {
        let mut words = line.split_whitespace();
        while let Some(word) = words.next() {
            if word != "inet6" {
                continue;
            }
            let Some(address) = words
                .next()
                .and_then(|value| value.split('/').next())
                .and_then(|value| value.parse::<std::net::Ipv6Addr>().ok())
            else {
                return false;
            };
            return !address.is_unspecified()
                && !address.is_loopback()
                && !address.is_multicast()
                && address.segments()[0] & 0xffc0 != 0xfe80;
        }
        false
    })
}

fn routing_for_device(device: &str) -> Result<Option<(String, String, u32)>, String> {
    if device.is_empty() {
        return Ok(None);
    }
    let output = checked_output(
        "ip",
        &["-4", "rule", "show"],
        &format!("failed to inspect IPv4 routing rules for {device}"),
    )?;
    Ok(parse_routing_for_device(
        &String::from_utf8_lossy(&output.stdout),
        device,
    ))
}

fn parse_routing_for_device(rules: &str, device: &str) -> Option<(String, String, u32)> {
    // These are mwan3's ingress-table and locally generated mark rules, not
    // arbitrary PBR selectors. A first match alone cannot establish authority.
    let mut table = None;
    for line in rules.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        if !words.windows(2).any(|pair| pair == ["iif", device]) {
            continue;
        }
        let [priority, "from", "all", "iif", _, "lookup", candidate] = words.as_slice() else {
            return None;
        };
        priority.strip_suffix(':')?.parse::<u32>().ok()?;
        if !is_safe_identifier(candidate) || table.is_some_and(|value| value != *candidate) {
            return None;
        }
        table = Some(*candidate);
    }
    let table = table?;

    let mut authority = None;
    for line in rules.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        if !words.contains(&"fwmark") || !words.windows(2).any(|pair| pair == ["lookup", table]) {
            continue;
        }
        let [priority, "from", "all", "fwmark", selector, "lookup", _] = words.as_slice() else {
            return None;
        };
        priority.strip_suffix(':')?.parse::<u32>().ok()?;
        let (mark, mask) = selector.split_once('/').unwrap_or((selector, "0xffffffff"));
        let candidate = (parse_route_u32(mark)?, parse_route_u32(mask)?);
        if candidate.0 == 0
            || candidate.1 == 0
            || candidate.0 & !candidate.1 != 0
            || authority.is_some_and(|value| value != candidate)
        {
            return None;
        }
        authority = Some(candidate);
    }
    let (mark, mask) = authority?;
    Some((format!("0x{mark:x}"), table.to_string(), mask))
}

fn valid_ipv4(value: &str) -> bool {
    let octets: Vec<&str> = value.split('.').collect();
    octets.len() == 4
        && octets.iter().all(|octet| {
            !octet.is_empty()
                && octet.len() <= 3
                && octet.chars().all(|ch| ch.is_ascii_digit())
                && octet.parse::<u8>().is_ok()
        })
}

#[cfg(test)]
mod tests {
    #[test]
    fn r6_legacy_route_key_bytes_do_not_acquire_a_link_suffix() {
        let mut identity = RouteIdentity {
            device_ifindex: None,
            fwmark_mask: None,
            mode: "main".into(),
            member: String::new(),
            device: "eth1".into(),
            source_ip: "192.0.2.2".into(),
            fwmark: String::new(),
            table: "main".into(),
        };
        assert_eq!(identity.stable_key(), "main||eth1|192.0.2.2||main");
        identity.mode = "mwan3".into();
        identity.member = "wan".into();
        identity.fwmark = "0x100".into();
        identity.table = "101".into();
        identity.fwmark_mask = Some(0x3f00);
        assert_eq!(
            identity.stable_key(),
            "mwan3|wan|eth1|192.0.2.2|0x100|101|mask=16128"
        );
    }

    #[test]
    fn r6_explicit_observation_binds_inventory_and_rejects_changes() {
        use serde_json::json;
        let authority = ExplicitRouteAuthority::from_fields(
            "explicit",
            ["192.0.2.2", "101", "0x100", "0x3f00"],
        )
        .unwrap()
        .unwrap();
        for mutation in 0..5 {
            let mut calls = 0;
            let observed = authority.observe_with("eth1", |args| {
                calls += 1;
                if mutation == 4 && calls == 2 {
                    return Err("inspection-failed".into());
                }
                match calls % 3 {
                    1 => {
                        assert_eq!(args, ["-j", "-4", "addr", "show", "dev", "eth1"]);
                        Ok(serde_json::to_vec(&json!([{"ifindex":if mutation == 1 && calls == 4 {43} else {42},
                            "ifname":"eth1","flags":["UP"],"addr_info":[{"family":"inet","local":"192.0.2.2",
                            "scope":"global","prefixlen":24,"valid_life_time":1000-calls}]}])).unwrap())
                    }
                    2 => {
                        assert_eq!(args, ["-4", "rule", "show"]);
                        let priority = if mutation == 2 && calls == 5 {101} else {100};
                        Ok(format!("0: from all lookup local\n{priority}: from all fwmark 0x100/0x3f00 lookup 101\n").into_bytes())
                    }
                    _ => {
                        assert_eq!(args, ["-j", "-4", "route", "show", "table", "101"]);
                        Ok(serde_json::to_vec(&json!([{"dst":"default","dev":"eth1",
                            "metric":if mutation == 3 && calls == 6 {20} else {10}}])).unwrap())
                    }
                }
            });
            if mutation == 0 {
                let observed = observed.unwrap();
                assert_eq!(observed.ifindex, 42);
                assert_eq!(observed.rule_priority, 100);
                assert_eq!(observed.identity.mode, "explicit");
                assert_eq!(observed.identity.fwmark_mask, Some(0x3f00));
                assert_eq!(observed.identity.device_ifindex, Some(42));
                let mut replacement = observed.identity.clone();
                replacement.device_ifindex = Some(43);
                assert_ne!(replacement, observed.identity);
                assert_ne!(replacement.stable_key(), observed.identity.stable_key());
            } else if mutation == 4 {
                assert_eq!(observed.unwrap_err(), "inspection-failed");
            } else {
                assert_eq!(observed.unwrap_err(), "explicit-route-observation-changed");
            }
        }
    }

    #[test]
    fn r6_explicit_source_witness_requires_exact_usable_assignment() {
        use serde_json::json;
        let authority = ExplicitRouteAuthority::from_fields(
            "explicit",
            ["192.0.2.2", "101", "0x100", "0x3f00"],
        )
        .unwrap()
        .unwrap();
        let source = json!({"family":"inet","local":"192.0.2.2","scope":"global",
            "prefixlen":24,"valid_life_time":4294967295_u64});
        let original = json!([{"ifindex":42,"ifname":"eth1","flags":["UP","LOWER_UP"],
            "addr_info":[source]}]);
        let check =
            |value| authority.attest_source_address("eth1", &serde_json::to_vec(&value).unwrap());
        assert_eq!(check(original.clone()), Ok(42));
        for (key, value) in [
            ("local", json!("192.0.2.3")),
            ("scope", json!("host")),
            ("prefixlen", json!(33)),
            ("valid_life_time", json!(0)),
            ("tentative", json!(true)),
            ("dadfailed", json!(true)),
            ("deprecated", json!(true)),
        ] {
            let mut changed = original.clone();
            changed[0]["addr_info"][0][key] = value;
            assert!(check(changed).is_err(), "{key}");
        }
        for (key, value) in [
            ("ifname", json!("eth2")),
            ("ifindex", json!(0)),
            ("flags", json!(["LOWER_UP"])),
            ("addr_info", json!([])),
        ] {
            let mut changed = original.clone();
            changed[0][key] = value;
            assert!(check(changed).is_err(), "{key}");
        }
        let mut multiple = original.clone();
        multiple[0]["addr_info"] = json!([
            {"family":"inet","local":"192.0.2.3"}, source]);
        assert_eq!(check(multiple.clone()), Ok(42));
        multiple[0]["addr_info"] = json!([source, source]);
        assert!(check(multiple).is_err());
    }

    #[test]
    #[ignore = "requires a fresh private user/network namespace"]
    fn r6_explicit_policy_kernel_fixture() {
        let parent = std::env::var("CAKE_R6_PARENT_NETNS").unwrap();
        assert_ne!(
            std::fs::read_link("/proc/self/ns/net")
                .unwrap()
                .to_string_lossy(),
            parent
        );
        fn ip(args: &[&str]) -> Vec<u8> {
            let output = Command::new("ip").args(args).output().unwrap();
            assert!(
                output.status.success(),
                "ip {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            output.stdout
        }
        let links: serde_json::Value =
            serde_json::from_slice(&ip(&["-j", "link", "show"])).unwrap();
        assert_eq!(links.as_array().unwrap().len(), 1);
        assert_eq!(links[0]["ifname"], "lo");
        ip(&[
            "link", "add", "cake-wan", "type", "veth", "peer", "name", "cake-vpn",
        ]);
        for device in ["cake-wan", "cake-vpn"] {
            ip(&["link", "set", device, "up"]);
        }
        ip(&["addr", "add", "192.0.2.2/24", "dev", "cake-wan"]);
        ip(&["route", "add", "default", "dev", "cake-vpn"]);
        ip(&["route", "add", "table", "101", "default", "dev", "cake-wan"]);
        ip(&["route", "add", "table", "102", "default", "dev", "cake-vpn"]);
        ip(&[
            "rule",
            "add",
            "pref",
            "100",
            "fwmark",
            "0x100/0x3f00",
            "lookup",
            "101",
        ]);
        let authority = ExplicitRouteAuthority::from_fields(
            "explicit",
            ["192.0.2.2", "101", "0x100", "0x3f00"],
        )
        .unwrap()
        .unwrap();
        let rules = || String::from_utf8(ip(&["-4", "rule", "show"])).unwrap();
        let addresses = || ip(&["-j", "-4", "addr", "show", "dev", "cake-wan"]);
        let index = authority
            .attest_source_address("cake-wan", &addresses())
            .unwrap();
        let routes = || ip(&["-j", "-4", "route", "show", "table", "101"]);
        let selected_device = || {
            let route: serde_json::Value = serde_json::from_slice(&ip(&[
                "-j",
                "-4",
                "route",
                "get",
                "198.51.100.1",
                "from",
                "192.0.2.2",
                "mark",
                "0x100",
            ]))
            .unwrap();
            route[0]["dev"].as_str().unwrap().to_string()
        };
        assert_eq!(authority.attest_rules(&rules()), Ok(100));
        assert_eq!(authority.attest_table_routes("cake-wan", &routes()), Ok(()));
        let observation = authority
            .observe_with("cake-wan", |args| Ok(ip(args)))
            .unwrap();
        assert_eq!(observation.ifindex, index);
        assert_eq!(observation.rule_priority, 100);
        #[cfg(feature = "calibration")]
        let request_route =
            crate::operations::autotune_request::operation_route_identity(&observation.identity)
                .unwrap();
        #[cfg(feature = "calibration")]
        let reobserve = || {
            crate::operations::runtime::inspect_operation_route(
                &request_route,
                "cake-wan",
                |authority, device| {
                    if std::env::var("CAKE_R6_SYSTEM_IP").as_deref() == Ok("1") {
                        authority.observe_system(device)
                    } else {
                        authority.observe_with(device, |args| Ok(ip(args)))
                    }
                },
            )
        };
        #[cfg(feature = "calibration")]
        assert_eq!(reobserve().unwrap().identity, observation.identity);
        if std::env::var("CAKE_R6_SYSTEM_IP").as_deref() == Ok("1") {
            assert_eq!(authority.observe_system("cake-wan").unwrap(), observation);
            let mut spec = RouteSpec::new("explicit", "", "cake-wan");
            spec.explicit_authority = Some(authority.clone());
            assert_eq!(
                inspect_explicit(&spec).unwrap().identity,
                observation.identity
            );
            // The candidate inspector is ready; product activation still waits
            // for all producer/planner and recovery acceptance requirements.
            assert!(inspect_route(&spec).is_err());
            assert!(routed_command(&spec, "", "ping").is_err());
        }
        assert_eq!(selected_device(), "cake-wan");
        ip(&[
            "rule",
            "add",
            "pref",
            "90",
            "fwmark",
            "0x100/0x3f00",
            "lookup",
            "102",
        ]);
        assert!(authority.attest_rules(&rules()).is_err());
        #[cfg(feature = "calibration")]
        assert!(reobserve().is_err());
        assert_eq!(selected_device(), "cake-vpn");
        ip(&["rule", "del", "pref", "90"]);
        ip(&["route", "add", "table", "101", "throw", "198.51.100.0/24"]);
        assert!(authority
            .attest_table_routes("cake-wan", &routes())
            .is_err());
        #[cfg(feature = "calibration")]
        assert!(reobserve().is_err());
        assert_eq!(selected_device(), "cake-vpn");
        ip(&["route", "del", "table", "101", "throw", "198.51.100.0/24"]);
        assert_eq!(authority.attest_table_routes("cake-wan", &routes()), Ok(()));
        assert_eq!(selected_device(), "cake-wan");
        ip(&["addr", "del", "192.0.2.2/24", "dev", "cake-wan"]);
        #[cfg(feature = "calibration")]
        assert!(reobserve().is_err());
        assert!(authority
            .attest_source_address("cake-wan", &addresses())
            .is_err());
        ip(&["link", "del", "cake-wan"]);
        ip(&[
            "link", "add", "cake-wan", "type", "veth", "peer", "name", "cake-vpn",
        ]);
        ip(&["link", "set", "cake-wan", "up"]);
        ip(&["addr", "add", "192.0.2.2/24", "dev", "cake-wan"]);
        assert_ne!(
            authority
                .attest_source_address("cake-wan", &addresses())
                .unwrap(),
            index
        );
        ip(&["link", "set", "cake-vpn", "up"]);
        ip(&["route", "add", "table", "101", "default", "dev", "cake-wan"]);
        let recreated = authority
            .observe_with("cake-wan", |args| Ok(ip(args)))
            .unwrap();
        assert_ne!(
            recreated.identity.device_ifindex,
            observation.identity.device_ifindex
        );
        assert_eq!(recreated.identity.device, observation.identity.device);
        assert_eq!(recreated.identity.source_ip, observation.identity.source_ip);
        // An otherwise valid replacement link cannot resume an old request.
        #[cfg(feature = "calibration")]
        assert!(reobserve().unwrap_err().contains("identity changed"));
    }

    #[test]
    fn r6_explicit_table_witness_refuses_fallthrough_and_foreign_specific_routes() {
        use serde_json::json;
        let authority = ExplicitRouteAuthority::from_fields(
            "explicit",
            ["192.0.2.2", "101", "0x100", "0x3f00"],
        )
        .unwrap()
        .unwrap();
        let default = json!({"dst":"default","gateway":"192.0.2.1","dev":"eth1","protocol":"static","flags":[]});
        let connected = json!({"dst":"192.0.2.0/24","dev":"eth1","protocol":"kernel","scope":"link","prefsrc":"192.0.2.2","flags":[]});
        let check =
            |rows| authority.attest_table_routes("eth1", &serde_json::to_vec(&rows).unwrap());
        assert_eq!(check(json!([default, connected])), Ok(()));
        assert_eq!(
            check(json!([connected])),
            Err("explicit-route-default-missing")
        );
        assert!(check(json!([])).is_err());
        for extra in [
            json!({"dst":"198.51.100.0/24","dev":"vpn0"}),
            json!({"dst":"198.51.100.0/24","dev":"eth1","type":"throw"}),
            json!({"dst":"198.51.100.0/24","dev":"eth1","type":"unreachable"}),
            json!({"dst":"198.51.100.0/24","dev":"eth1","flags":["linkdown"]}),
            json!({"dst":"198.51.100.0/24","dev":"eth1","table":102}),
            json!({"dst":"198.51.100.0/24","dev":"eth1","nhid":42}),
            json!({"dst":"198.51.100.0/24","dev":"eth1","nexthops":[{"dev":"vpn0"}]}),
            json!({"dst":"2001:db8::/32","dev":"eth1"}),
            json!({"dst":"198.51.100.0/99","dev":"eth1"}),
        ] {
            assert!(check(json!([default, extra])).is_err(), "{extra}");
        }
        let mut typed = default.clone();
        typed["type"] = json!("unicast");
        typed["table"] = json!(101);
        typed["flags"] = json!(["onlink"]);
        assert_eq!(check(json!([typed])), Ok(()));
    }

    #[test]
    fn r6_explicit_rule_witness_accounts_for_priority_and_preserved_mark_bits() {
        let authority = ExplicitRouteAuthority::from_fields(
            "explicit",
            ["192.0.2.2", "101", "0x100", "0x3f00"],
        )
        .unwrap()
        .unwrap();
        let local = "0: from all lookup local\n";
        let selected = "100: from all fwmark 0x100/0x3f00 lookup 101\n";
        let tail = "32766: from all lookup main\n32767: from all lookup default\n";
        assert_eq!(
            authority.attest_rules(&format!("{local}{selected}{tail}")),
            Ok(100)
        );
        assert_eq!(
            authority.attest_rules(&format!("{tail}{selected}{local}")),
            Ok(100)
        );
        assert_eq!(
            authority.attest_rules(&format!(
                "{local}100: from 192.0.2.2/32 fwmark 256/16128 lookup 101\n{tail}"
            )),
            Ok(100)
        );
        assert_eq!(
            authority.attest_rules(&format!(
                "{local}90: from all fwmark 0x200/0x3f00 lookup 102\n{selected}{tail}"
            )),
            Ok(100)
        );
        for earlier in [
            "90: from all lookup 102\n",
            "90: from all fwmark 0x100/0x3f00 lookup 102\n",
            // High mark bits are retained, so this CAN match a probe socket.
            "90: from all fwmark 0x10100/0x13f00 lookup 102\n",
            "90: from all fwmark 0x100/0xff00 lookup 102\n",
            "90: from 192.0.2.2 to 198.51.100.0/24 lookup 102\n",
            "90: not from all fwmark 0x200/0x3f00 lookup 102\n",
            "90: from all fwmark 0x100/0x3f00 lookup 101 suppress_prefixlength 0\n",
            "100: from all lookup 102\n",
        ] {
            assert!(
                authority
                    .attest_rules(&format!("{local}{earlier}{selected}{tail}"))
                    .is_err(),
                "{earlier}"
            );
        }
        assert!(authority.attest_rules(selected).is_err());
        assert!(authority.attest_rules(local).is_err());
        assert!(authority
            .attest_rules(&format!(
                "{local}100: from 192.0.2.3 fwmark 0x100/0x3f00 lookup 101\n"
            ))
            .is_err());
    }

    #[test]
    fn r6_all_route_observations_share_probe_retirement_boundary() {
        use super::{UplinkLifecycle, UplinkState};
        let first = snapshot(true, "192.0.2.10");
        let changed = snapshot(true, "192.0.2.11");
        let mut lifecycle = UplinkLifecycle::new();
        lifecycle.observe(Ok(&first));
        let admitted = lifecycle.observe(Ok(&first));
        assert!(admitted.probes_allowed);
        assert!(!admitted.retires_probes(false));
        let stable = lifecycle.observe(Ok(&first));
        assert!(!stable.retires_probes(true));
        let recheck = lifecycle.observe(Err("inspection failed"));
        assert_eq!(recheck.state, UplinkState::Rechecking);
        assert!(recheck.retires_probes(true));
        assert!(!recheck.retires_probes(false));
        let new_route = lifecycle.observe(Ok(&changed));
        assert!(new_route.identity_changed);
        assert!(new_route.retires_probes(false));
        lifecycle.observe(Ok(&changed));
        let mut offline = changed.clone();
        offline.online = false;
        let lost = lifecycle.observe(Ok(&offline));
        assert!(lost.became_offline);
        assert!(lost.retires_probes(false));
    }

    #[test]
    fn r6_explicit_authority_is_typed_complete_and_mask_sensitive() {
        use super::ExplicitRouteAuthority as Authority;
        let a =
            Authority::from_fields("explicit", ["192.0.2.1", "101", "0x200", "0x3f00"]).unwrap();
        let b = Authority::from_fields("explicit", ["192.0.2.1", "101", "512", "16128"]).unwrap();
        assert_eq!(a, b);
        assert_ne!(
            a,
            Authority::from_fields("explicit", ["192.0.2.1", "101", "512", "0xff00"]).unwrap()
        );
        for mode in ["auto", "main", "mwan3"] {
            assert_eq!(Authority::from_fields(mode, [""; 4]).unwrap(), None);
            assert!(Authority::from_fields(mode, ["192.0.2.1", "101", "512", "16128"]).is_err());
        }
        for fields in [
            ["::1", "101", "512", "16128"],
            ["127.0.0.1", "101", "512", "16128"],
            ["0.1.2.3", "101", "512", "16128"],
            ["224.0.0.1", "101", "512", "16128"],
            ["255.255.255.255", "101", "512", "16128"],
            ["192.0.2.1", "0", "512", "16128"],
            ["192.0.2.1", "main", "512", "16128"],
            ["192.0.2.1", "4294967296", "512", "16128"],
            ["192.0.2.1", "101", "0", "16128"],
            ["192.0.2.1", "101", "513", "16128"],
            ["192.0.2.1", "101", "512", "0"],
            ["192.0.2.1", "101", "512", ""],
            ["192.0.2.1", "101", "+512", "16128"],
            ["192.0.2.1", "101", "0x+200", "16128"],
            ["192.0.2.1", "101", "0x100000000", "16128"],
        ] {
            assert!(
                Authority::from_fields("explicit", fields).is_err(),
                "{fields:?}"
            );
        }
    }

    #[test]
    fn r6_explicit_config_keeps_runtime_admission_closed_until_enforced() {
        let text = "cake-autorate.primary.route_mode='explicit'\ncake-autorate.primary.route_source_ipv4='192.0.2.1'\ncake-autorate.primary.route_table='101'\ncake-autorate.primary.route_fwmark='512'\ncake-autorate.primary.route_fwmark_mask='16128'\n";
        let cfg = crate::Config::from_uci_text("primary", text).unwrap();
        assert!(cfg.explicit_route_authority.is_some());
        assert!(cfg
            .route_spec()
            .validate()
            .unwrap_err()
            .contains("enforcement is not available"));
        assert!(cfg.validate().is_err());
        let conflict = text.replace("='explicit'", "='main'");
        assert!(crate::Config::from_uci_text("primary", &conflict).is_err());
    }
    #[test]
    fn r6_main_refuses_policy_and_split_overrides_before_network_work() {
        let ordinary = "0: from all lookup local\n32766: from all lookup main\n32767: from all lookup default\n";
        let plain = "default via 192.0.2.254 dev wan\n192.0.2.0/24 dev wan scope link\n10.0.0.0/24 dev lan scope link\n";
        assert!(super::main_policy_problem(ordinary, plain, "wan").is_none());
        for extra in [
            "100: from 192.0.2.1 lookup 100\n",
            "100: from all fwmark 0x100 lookup 100\n",
            "100: from all uidrange 32769-32769 lookup 100\n",
            "100: from all to 203.0.113.0/24 lookup 100\n",
        ] {
            assert!(
                super::main_policy_problem(&format!("{ordinary}{extra}"), plain, "wan").is_some()
            );
        }
        for prefix in ["0.0.0.0/1", "128.0.0.0/1"] {
            assert!(super::main_policy_problem(
                ordinary,
                &format!("{plain}{prefix} dev vpn\n"),
                "wan"
            )
            .is_some());
            assert!(super::main_policy_problem(
                ordinary,
                &format!("{plain}{prefix} dev wan\n"),
                "wan"
            )
            .is_none());
        }
        assert!(super::main_policy_problem("", plain, "wan").is_some());
        assert!(super::main_policy_problem(
            &format!("{ordinary}0: from all lookup local\n"),
            plain,
            "wan"
        )
        .is_some());
    }

    use super::*;
    use std::ffi::OsStr;

    fn args(command: &Command) -> Vec<String> {
        command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn main_command_is_direct() {
        let spec = RouteSpec::new("main", "", "eth0");
        let command = routed_command(&spec, "", "fping").unwrap();
        assert_eq!(command.get_program(), OsStr::new("fping"));
        assert!(args(&command).is_empty());
    }

    #[test]
    fn mwan3_command_is_structured_without_shell() {
        let spec = RouteSpec::new("mwan3", "wanb", "eth0");
        let command = routed_command(&spec, "", "uclient-fetch").unwrap();
        assert_eq!(command.get_program(), OsStr::new("mwan3"));
        assert_eq!(args(&command), ["use", "wanb", "exec", "uclient-fetch"]);
    }

    #[test]
    fn rejects_member_shell_injection() {
        let spec = RouteSpec::new("mwan3", "wanb;reboot", "eth0");
        assert!(routed_command(&spec, "", "fping").is_err());
    }

    #[test]
    fn refuses_legacy_prefix_with_structured_mwan3() {
        let spec = RouteSpec::new("mwan3", "wanb", "eth0");
        assert!(routed_command(&spec, "mwan3 use wanb exec", "fping").is_err());
    }

    #[test]
    fn checked_output_propagates_execution_failure() {
        let error = checked_output(
            "/definitely-not-a-cake-autorate-command",
            &[],
            "route inspection",
        )
        .unwrap_err();
        assert!(error.contains("route inspection"));
        assert!(error.contains("failed to execute"));
    }

    #[test]
    fn r6_mwan3_wrapper_mask_must_match_rule_authority() {
        for value in ["0x3f00", "16128", "0X3F00"] {
            assert!(
                super::attest_mwan3_environment_mask(&format!("FWMARK={value}\n"), 0x3f00).is_ok()
            );
        }
        for environment in [
            "",
            "FWMARK=\n",
            "FWMARK=0\n",
            "FWMARK=0xff00\n",
            "FWMARK=0x+3f00\n",
            "FWMARK= 0x3f00\n",
            "FWMARK=0x3f00\nFWMARK=0x3f00\n",
        ] {
            assert!(super::attest_mwan3_environment_mask(environment, 0x3f00).is_err());
        }
        assert!(env_value("DEVICE=eth0\nDEVICE=eth1\n", "DEVICE").is_none());
        assert!(env_value("SRCIP=192.0.2.1 \n", "SRCIP").is_none());
    }

    #[test]
    fn parses_member_metadata() {
        let status = r#"{"status":"online","running":true,"up":true}"#;
        assert_eq!(
            json_string_value(status, "status").as_deref(),
            Some("online")
        );
        assert_eq!(json_bool_value(status, "running"), Some(true));
        assert_eq!(
            env_value("DEVICE=eth0\nSRCIP=192.0.2.101\n", "DEVICE").as_deref(),
            Some("eth0")
        );
    }

    #[test]
    fn resolves_active_member_from_default_mwan3_policy() {
        let config = "mwan3.specific=rule\n\
mwan3.specific.src_ip='192.0.2.1'\n\
mwan3.specific.use_policy='wan_only'\n\
mwan3.default_rule_v4=rule\n\
mwan3.default_rule_v4.dest_ip='0.0.0.0/0'\n\
mwan3.default_rule_v4.family='ipv4'\n\
mwan3.default_rule_v4.use_policy='wan_then_wan2'\n";
        assert_eq!(
            parse_mwan3_default_policy(config).as_deref(),
            Some("wan_then_wan2")
        );

        let status = r#"{"policies":{"ipv4":{"wan_then_wan2":[
            {"interface":"wan","percent":0,"status":"offline"},
            {"interface":"wan2","percent":100,"status":"online"}
        ]}}}"#;
        assert_eq!(
            json_policy_member_percent(status, "wan_then_wan2", "wan"),
            Some(0)
        );
        assert_eq!(
            json_policy_member_percent(status, "wan_then_wan2", "wan2"),
            Some(100)
        );
    }

    #[test]
    fn parses_default_device_and_fwmark_table() {
        assert_eq!(
            parse_default_route_device("default via 10.0.0.1 dev eth0 metric 20\n").as_deref(),
            Some("eth0")
        );
        assert_eq!(
            parse_proc_default_route_device(
                "Iface Destination Gateway Flags RefCnt Use Metric Mask MTU Window IRTT\n\
                 eth2 00000000 0100000A 0003 0 0 0 00000000 0 0 0\n"
            )
            .as_deref(),
            Some("eth2")
        );
        let rules =
            "1001: from all iif pppoe-wan lookup 1\n2001: from all fwmark 0x100/0x3f00 lookup 1\n";
        assert_eq!(
            parse_routing_for_device(rules, "pppoe-wan"),
            Some(("0x100".to_string(), "1".to_string(), 0x3f00))
        );
    }

    #[test]
    fn r6_mwan3_rule_authority_refuses_ambiguous_or_conditional_pairs() {
        let ingress = "1001: from all iif wan lookup 1\n";
        let marked = "2001: from all fwmark 0x100/0x3f00 lookup 1\n";
        for rules in [
            format!("{ingress}1002: from all iif wan lookup 2\n{marked}"),
            format!("{ingress}{marked}2002: from all fwmark 0x200/0x3f00 lookup 1\n"),
            format!("{ingress}2001: from 192.0.2.1 fwmark 0x100/0x3f00 lookup 1\n"),
            format!("{ingress}2001: not from all fwmark 0x100/0x3f00 lookup 1\n"),
            format!("{ingress}2001: from all fwmark 0x100/0x3f00 to 203.0.113.0/24 lookup 1\n"),
            format!(
                "{ingress}2001: from all fwmark 0x100/0x3f00 lookup 1 suppress_prefixlength 0\n"
            ),
            format!("1001: from 192.0.2.0/24 iif wan lookup 1\n{marked}"),
            format!("{ingress}2001: from all fwmark 0x100/0x0 lookup 1\n"),
            format!("{ingress}2001: from all fwmark 0x101/0x3f00 lookup 1\n"),
            format!("{ingress}2001: from all fwmark 0x0/0x3f00 lookup 1\n"),
            format!("{ingress}{marked}2002: from all fwmark 0x100/0xff00 lookup 1\n"),
            format!("{ingress}2001: from all fwmark 0x100/not-a-mask lookup 1\n"),
            format!("{ingress}2001: from all fwmark 0x100000000 lookup 1\n"),
        ] {
            assert_eq!(parse_routing_for_device(&rules, "wan"), None, "{rules}");
            let reversed = rules.lines().rev().collect::<Vec<_>>().join("\n");
            assert_eq!(parse_routing_for_device(&reversed, "wan"), None);
        }
        // Unrelated member rules and equivalent duplicates do not create a
        // second authority; textual mark spelling must not change identity.
        let rules = format!("{ingress}{marked}1002: from all iif other lookup 2\n2002: from all fwmark 0x200/0x3f00 lookup 2\n3001: from all fwmark 256/16128 lookup 1\n");
        assert_eq!(
            parse_routing_for_device(&rules, "wan"),
            Some(("0x100".into(), "1".into(), 0x3f00))
        );
        let reversed = rules.lines().rev().collect::<Vec<_>>().join("\n");
        assert_eq!(
            parse_routing_for_device(&reversed, "wan"),
            parse_routing_for_device(&rules, "wan")
        );
        assert_eq!(parse_routing_for_device(ingress, "wan"), None);
        assert_eq!(parse_routing_for_device(marked, "wan"), None);
        assert_eq!(parse_routing_for_device(&rules, "missing"), None);
        assert_eq!(
            parse_routing_for_device(
                &format!("{ingress}2001: from all fwmark 256 lookup 1\n"),
                "wan"
            ),
            Some(("0x100".into(), "1".into(), u32::MAX))
        );
    }

    #[test]
    fn r6_proc_default_uses_mask_flags_and_lowest_metric_not_row_order() {
        let header = "Iface Destination Gateway Flags RefCnt Use Metric Mask MTU Window IRTT\n";
        let slow = "slow 00000000 0100000A 0003 0 0 100 00000000 0 0 0\n";
        let fast = "fast 00000000 0100000A 0003 0 0 20 00000000 0 0 0\n";
        let split = "vpn 00000000 00000000 0001 0 0 0 00000080 0 0 0\n";
        let down = "down 00000000 0100000A 0000 0 0 0 00000000 0 0 0\n";
        for body in [
            format!("{split}{down}{slow}{fast}"),
            format!("{fast}{slow}{down}{split}"),
        ] {
            assert_eq!(
                parse_proc_default_route_device(&format!("{header}{body}")).as_deref(),
                Some("fast")
            );
        }
        assert!(parse_proc_default_route_device(&format!("{header}{split}{down}")).is_none());
        let equal = fast.replace("fast ", "other ");
        assert!(parse_proc_default_route_device(&format!("{header}{fast}{equal}")).is_none());
        assert!(parse_proc_default_route_device(&format!("{header}{equal}{fast}")).is_none());
        assert_eq!(
            parse_proc_default_route_device(&format!("{header}{fast}{fast}")).as_deref(),
            Some("fast")
        );
        let rejected = fast.replace("0003", "0203");
        assert!(parse_proc_default_route_device(&format!("{header}{slow}{rejected}")).is_none());
        for invalid in [
            fast.replace(" 20 ", " invalid "),
            fast.replace("00000000 0 0 0", "invalid 0 0 0"),
        ] {
            assert!(parse_proc_default_route_device(&format!("{header}{invalid}")).is_none());
        }
    }

    #[test]
    fn r6_text_default_metric_and_ambiguity_match_proc_semantics() {
        let slow = "default via 192.0.2.1 dev slow metric 100\n";
        let fast = "default via 198.51.100.1 dev fast metric 20\n";
        for routes in [format!("{slow}{fast}"), format!("{fast}{slow}")] {
            assert_eq!(parse_default_route_device(&routes).as_deref(), Some("fast"));
        }
        assert_eq!(
            parse_default_route_device(&format!("{slow}default dev direct\n")).as_deref(),
            Some("direct")
        );
        assert!(parse_default_route_device("0.0.0.0/1 dev vpn\n128.0.0.0/1 dev vpn\n").is_none());
        let equal = fast.replace("dev fast", "dev other");
        assert!(parse_default_route_device(&format!("{fast}{equal}")).is_none());
        assert_eq!(
            parse_default_route_device(&format!("{fast}{fast}")).as_deref(),
            Some("fast")
        );
        for blocked in [
            "blackhole default metric 10\n",
            "unreachable default metric 10\n",
            "default nhid 7 metric 10\n",
            "default dev deadwan linkdown metric 10\n",
        ] {
            assert!(parse_default_route_device(&format!("{fast}{blocked}")).is_none());
        }
        for malformed in [
            "default dev fast metric bad",
            "default dev fast metric",
            "default dev fast metric 2 metric 3",
        ] {
            assert!(parse_default_route_device(malformed).is_none());
        }
    }

    #[test]
    fn r6_multipath_does_not_silently_pick_the_first_nexthop_device() {
        for routes in [
            "default metric 20 nexthop via 192.0.2.1 dev first weight 1 nexthop via 198.51.100.1 dev second weight 1\n",
            "default metric 20\n  nexthop via 192.0.2.1 dev first weight 1\n  nexthop via 198.51.100.1 dev second weight 1\n",
        ] {
            assert!(parse_default_route_device(routes).is_none());
            assert_eq!(parse_default_route_device(&format!("{routes}default dev preferred metric 10\n")).as_deref(), Some("preferred"));
        }
        assert_eq!(parse_default_route_device("default metric 20\n nexthop via 192.0.2.1 dev same weight 1\n nexthop via 198.51.100.1 dev same weight 1\n").as_deref(), Some("same"));
    }

    #[test]
    fn r6_ipv6_only_is_an_explicit_capability_refusal_not_endless_connecting() {
        for address in ["2001:db8::1", "fd00::1"] {
            assert!(has_global_ipv6_address(&format!(
                "2: fixture inet6 {address}/64 scope global"
            )));
        }
        for address in ["::", "::1", "fe80::1", "ff02::1", "invalid"] {
            assert!(!has_global_ipv6_address(&format!(
                "2: fixture inet6 {address}/64 scope global"
            )));
        }
        assert!(!has_global_ipv6_address(
            "2: fixture inet 192.0.2.1/24 scope global"
        ));
        assert!(missing_ipv4_reason("fixture", true)
            .contains("IPv6-only route/probe support is not implemented"));
        assert!(missing_ipv4_reason("fixture", false).contains("waiting for route identity"));
        let snapshot = RouteSnapshot {
            identity: RouteIdentity {
                device_ifindex: None,
                fwmark_mask: None,
                mode: "main".into(),
                member: String::new(),
                device: "fixture".into(),
                source_ip: String::new(),
                fwmark: String::new(),
                table: "main".into(),
            },
            online: false,
            active: false,
            member_status: "unsupported_family".into(),
            reason: missing_ipv4_reason("fixture", true),
        };
        let mut lifecycle = UplinkLifecycle::new();
        let transition = lifecycle.observe(Ok(&snapshot));
        assert_eq!(transition.state, UplinkState::Offline);
        assert!(!transition.probes_allowed);
        assert!(transition.reason.contains("IPv6-only"));
    }

    #[test]
    fn validates_external_ipv4_without_accepting_trailing_data() {
        assert!(valid_ipv4("84.52.59.166"));
        assert!(!valid_ipv4("84.52.59.166\nwrong"));
        assert!(!valid_ipv4("999.1.1.1"));
    }

    #[test]
    fn r7_external_ip_endpoint_rejects_unsafe_or_unbounded_urls_without_echoing_them() {
        for value in [
            "https://api.ipify.org",
            "https://example.invalid:8443/plain/ip",
            "https://example.invalid/ip%20v4",
        ] {
            assert!(validate_external_ip_url(value).is_ok(), "{value}");
        }
        for value in [
            "http://example.invalid",
            "https://user:secret@example.invalid/",
            "https://example.invalid/?secret=x",
            "https://example.invalid/#x",
            "https://example.invalid/\n",
            "https://-bad.invalid",
            "https://example.invalid:0",
            "https://example.invalid:65536",
            "https://example.invalid/%",
            "https://example.invalid/%GG",
            "https://example.invalid/\\x",
            "https://",
            "https://é.invalid",
        ] {
            let error = validate_external_ip_url(value).unwrap_err();
            assert!(!error.contains(value));
            assert!(!error.contains("secret"));
        }
        assert!(
            validate_external_ip_url(&format!("https://example.invalid/{}", "a".repeat(1024)))
                .is_err()
        );
    }

    #[test]
    fn r7_external_ip_command_preserves_route_and_uses_absolute_programs() {
        let endpoint = "https://never-contact.invalid/ip";
        for mode in ["main", "mwan3"] {
            let member = if mode == "mwan3" { "lab" } else { "" };
            let spec = RouteSpec::new(mode, member, "eth0");
            let command = external_ip_command(&spec, 5, endpoint).unwrap();
            let mut expected = Vec::new();
            if mode == "mwan3" {
                assert_eq!(command.program.to_str(), Some("/usr/sbin/mwan3"));
                expected.extend(["use", "lab", "exec", "/bin/uclient-fetch"]);
            } else {
                assert_eq!(command.program.to_str(), Some("/bin/uclient-fetch"));
            }
            expected.extend(["-q", "-4", "-T", "5", "-O", "-", endpoint]);
            assert_eq!(
                command.arguments,
                expected
                    .iter()
                    .map(std::ffi::OsString::from)
                    .collect::<Vec<_>>()
            );
        }
    }

    fn snapshot(active: bool, ip: &str) -> RouteSnapshot {
        RouteSnapshot {
            identity: RouteIdentity {
                device_ifindex: None,
                fwmark_mask: None,
                mode: "mwan3".to_string(),
                member: "wanb".to_string(),
                device: "eth0".to_string(),
                source_ip: ip.to_string(),
                fwmark: "0x100".to_string(),
                table: "1".to_string(),
            },
            online: true,
            active,
            member_status: "online".to_string(),
            reason: String::new(),
        }
    }

    #[test]
    fn lifecycle_requires_repeated_matching_identity_before_learning() {
        let route = snapshot(false, "192.0.2.101");
        let mut lifecycle = UplinkLifecycle::new();
        let first = lifecycle.observe(Ok(&route));
        assert_eq!(first.state, UplinkState::Learning);
        assert!(!first.probes_allowed);
        assert!(!lifecycle.record_learning_sample(1));

        let confirmed = lifecycle.observe(Ok(&route));
        assert_eq!(confirmed.state, UplinkState::Learning);
        assert!(confirmed.probes_allowed);
        assert!(!lifecycle.record_learning_sample(3));
        assert!(!lifecycle.record_learning_sample(3));
        assert!(lifecycle.record_learning_sample(3));
        assert_eq!(lifecycle.state(), UplinkState::Standby);
    }

    #[test]
    fn lifecycle_resets_after_ip_change_and_offline_recovery() {
        let first_route = snapshot(true, "198.51.100.1");
        let second_route = snapshot(true, "198.51.100.2");
        let mut lifecycle = UplinkLifecycle::new();
        lifecycle.observe(Ok(&first_route));
        lifecycle.observe(Ok(&first_route));
        lifecycle.record_learning_sample(1);
        assert_eq!(lifecycle.state(), UplinkState::Active);

        let changed = lifecycle.observe(Ok(&second_route));
        assert!(changed.identity_changed);
        assert_eq!(changed.state, UplinkState::Learning);
        assert!(!changed.probes_allowed);
        assert!(lifecycle.observe(Ok(&second_route)).probes_allowed);

        let offline_route = RouteSnapshot {
            online: false,
            reason: "member offline".to_string(),
            ..second_route.clone()
        };
        let offline = lifecycle.observe(Ok(&offline_route));
        assert!(offline.became_offline);
        assert!(offline.reset_learning);
        assert_eq!(offline.state, UplinkState::Offline);

        let recovered = lifecycle.observe(Ok(&second_route));
        assert!(!recovered.identity_changed);
        assert!(recovered.reset_learning);
        assert_eq!(recovered.state, UplinkState::Learning);
        assert!(!recovered.probes_allowed);
        assert!(lifecycle.observe(Ok(&second_route)).probes_allowed);
    }

    #[test]
    fn lifecycle_never_turns_inspection_errors_into_offline() {
        let route = snapshot(false, "192.0.2.101");
        let mut lifecycle = UplinkLifecycle::new();
        lifecycle.observe(Ok(&route));
        lifecycle.observe(Ok(&route));
        lifecycle.record_learning_sample(1);
        assert_eq!(lifecycle.state(), UplinkState::Standby);

        for error in [
            "mwan3 status failed",
            "ubus still unavailable",
            "route inspector remains unavailable",
            "another failure",
        ] {
            let unknown = lifecycle.observe(Err(error));
            assert_eq!(unknown.state, UplinkState::Rechecking);
            assert!(!unknown.became_offline);
            assert!(!unknown.reset_learning);
            assert!(!unknown.probes_allowed);
        }

        let recovered = lifecycle.observe(Ok(&route));
        assert_eq!(recovered.state, UplinkState::Standby);
        assert!(!recovered.reset_learning);
        assert!(recovered.probes_allowed);
    }

    #[test]
    fn lifecycle_requires_consecutive_identity_evidence_after_unknown_start() {
        let route = snapshot(true, "198.51.100.1");
        let mut lifecycle = UplinkLifecycle::new();
        assert!(!lifecycle.observe(Ok(&route)).probes_allowed);
        assert_eq!(
            lifecycle.observe(Err("ubus unavailable")).state,
            UplinkState::Rechecking
        );
        assert!(!lifecycle.observe(Ok(&route)).probes_allowed);
        assert!(lifecycle.observe(Ok(&route)).probes_allowed);
    }

    #[test]
    fn lifecycle_rechecks_a_new_identity_after_an_inspection_error() {
        let first_route = snapshot(true, "198.51.100.1");
        let second_route = snapshot(true, "198.51.100.2");
        let mut lifecycle = UplinkLifecycle::new();
        lifecycle.observe(Ok(&first_route));
        lifecycle.observe(Ok(&first_route));
        assert!(lifecycle.record_learning_sample(1));
        assert_eq!(lifecycle.state(), UplinkState::Active);

        assert_eq!(
            lifecycle.observe(Err("route inspector unavailable")).state,
            UplinkState::Rechecking
        );
        let changed = lifecycle.observe(Ok(&second_route));
        assert!(changed.identity_changed);
        assert!(changed.reset_learning);
        assert!(!changed.probes_allowed);
        assert!(!lifecycle.record_learning_sample(1));

        let confirmed = lifecycle.observe(Ok(&second_route));
        assert!(!confirmed.identity_changed);
        assert!(confirmed.probes_allowed);
        assert!(lifecycle.record_learning_sample(1));
        assert_eq!(lifecycle.state(), UplinkState::Active);
    }

    #[test]
    fn lifecycle_preserves_learning_across_persistent_mwan3_transition_state() {
        let route = snapshot(true, "198.51.100.1");
        let mut lifecycle = UplinkLifecycle::new();
        lifecycle.observe(Ok(&route));
        lifecycle.observe(Ok(&route));
        lifecycle.record_learning_sample(1);

        let disconnecting = RouteSnapshot {
            online: false,
            member_status: "disconnecting".to_string(),
            reason: "member wanb is disconnecting".to_string(),
            ..route.clone()
        };
        for _ in 0..8 {
            let transition = lifecycle.observe(Ok(&disconnecting));
            assert_eq!(transition.state, UplinkState::Rechecking);
            assert!(!transition.became_offline);
            assert!(!transition.reset_learning);
            assert!(!transition.probes_allowed);
        }

        let recovered = lifecycle.observe(Ok(&route));
        assert_eq!(recovered.state, UplinkState::Active);
        assert!(!recovered.reset_learning);
        assert!(recovered.probes_allowed);
    }

    #[test]
    fn lifecycle_accepts_offline_only_from_an_explicit_snapshot() {
        let route = snapshot(true, "198.51.100.1");
        let mut lifecycle = UplinkLifecycle::new();
        lifecycle.observe(Ok(&route));
        lifecycle.observe(Ok(&route));
        lifecycle.record_learning_sample(1);

        let offline_route = RouteSnapshot {
            online: false,
            member_status: "offline".to_string(),
            reason: "member wanb is offline".to_string(),
            ..route
        };
        let offline = lifecycle.observe(Ok(&offline_route));
        assert_eq!(offline.state, UplinkState::Offline);
        assert!(offline.became_offline);
        assert!(offline.reset_learning);
    }
}
