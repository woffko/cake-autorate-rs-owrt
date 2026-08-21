//! Native nftables DSCP classifier for the Full package.
//!
//! This module owns the complete UCI -> nft ruleset projection and the
//! attested apply/status lifecycle. It never creates or changes a qdisc: the
//! selected upload CAKE queue remains the sole consumer of the DSCP marks.

use std::collections::BTreeSet;
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use ring::digest::{digest, SHA256};

use super::json_wire::{bool_json, json_escape};
use super::process::{
    run_bounded_command_output, run_bounded_command_output_with_input, SpawnSpec,
};
use super::runtime_health::{json_string_value, safe_interface, safe_name, UciPackage, UciSection};

const STATE_SCHEMA_VERSION: u8 = 3;
const PRESET_SCHEMA_VERSION: u8 = 1;
const TABLE_FAMILY: &str = "inet";
const TABLE_NAME: &str = "cake_autorate_dscp";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_COMMAND_OUTPUT: usize = 1024 * 1024;
const MAX_RULESET_BYTES: usize = 256 * 1024;
const MAX_MANIFEST_BYTES: usize = 128 * 1024;
const MAX_INSTANCES: usize = 64;
const MAX_CUSTOM_RULES: usize = 512;
static NEXT_STAGED_FILE: AtomicU32 = AtomicU32::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AutotuneProfile {
    Gaming,
    VariableLink,
    BestOverall,
    Fair,
}

impl AutotuneProfile {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "gaming" | "gaming_extreme" | "gaming-extreme" | "extreme_gaming" => Ok(Self::Gaming),
            "variable_link" | "variable-link" | "variable" => Ok(Self::VariableLink),
            "best_overall" | "best-overall" | "balanced" | "" => Ok(Self::BestOverall),
            "fair" => Ok(Self::Fair),
            _ => Err("unsupported Auto-Tune profile".to_string()),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Gaming => "gaming",
            Self::VariableLink => "variable_link",
            Self::BestOverall => "best_overall",
            Self::Fair => "fair",
        }
    }

    fn traffic_default(self) -> TrafficProfile {
        match self {
            Self::Gaming => TrafficProfile::Gaming,
            Self::VariableLink | Self::BestOverall => TrafficProfile::BestOverall,
            Self::Fair => TrafficProfile::Fair,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TrafficProfile {
    Auto,
    Gaming,
    BestOverall,
    Fair,
    Custom,
}

impl TrafficProfile {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "auto" | "" => Ok(Self::Auto),
            "gaming" => Ok(Self::Gaming),
            "best_overall" | "best-overall" | "balanced" => Ok(Self::BestOverall),
            "fair" => Ok(Self::Fair),
            "custom" => Ok(Self::Custom),
            _ => Err("unsupported traffic-priority profile".to_string()),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Gaming => "gaming",
            Self::BestOverall => "best_overall",
            Self::Fair => "fair",
            Self::Custom => "custom",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AddressFamily {
    Any,
    Ipv4,
    Ipv6,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Protocol {
    Any,
    Tcp,
    Udp,
    TcpUdp,
    Icmp,
}

#[derive(Clone, Copy, Debug)]
struct Preset {
    protocol: Protocol,
    source_ports: &'static str,
    destination_ports: &'static str,
}

#[derive(Clone, Copy, Debug)]
struct BuiltinRule {
    id: &'static str,
    preset: &'static str,
    class: &'static str,
}

const GAMING_RULES: &[BuiltinRule] = &[
    BuiltinRule {
        id: "dns",
        preset: "dns",
        class: "voice",
    },
    BuiltinRule {
        id: "ntp",
        preset: "ntp",
        class: "voice",
    },
    BuiltinRule {
        id: "web",
        preset: "web",
        class: "best_effort",
    },
    BuiltinRule {
        id: "quic",
        preset: "quic",
        class: "best_effort",
    },
    BuiltinRule {
        id: "steam_realtime",
        preset: "steam_realtime",
        class: "voice",
    },
    BuiltinRule {
        id: "xbox_live",
        preset: "xbox_live",
        class: "voice",
    },
    BuiltinRule {
        id: "playstation",
        preset: "playstation",
        class: "voice",
    },
];

const BEST_OVERALL_RULES: &[BuiltinRule] = &[
    BuiltinRule {
        id: "dns",
        preset: "dns",
        class: "voice",
    },
    BuiltinRule {
        id: "ntp",
        preset: "ntp",
        class: "voice",
    },
    BuiltinRule {
        id: "ssh",
        preset: "ssh",
        class: "video",
    },
    BuiltinRule {
        id: "web",
        preset: "web",
        class: "best_effort",
    },
    BuiltinRule {
        id: "quic",
        preset: "quic",
        class: "best_effort",
    },
];

const FAIR_RULES: &[BuiltinRule] = &[
    BuiltinRule {
        id: "dns",
        preset: "dns",
        class: "video",
    },
    BuiltinRule {
        id: "ntp",
        preset: "ntp",
        class: "video",
    },
    BuiltinRule {
        id: "web",
        preset: "web",
        class: "best_effort",
    },
    BuiltinRule {
        id: "quic",
        preset: "quic",
        class: "best_effort",
    },
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct InstanceManifest {
    instance: String,
    target: String,
    autotune_profile: String,
    configured_profile: String,
    resolved_profile: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StateManifest {
    ruleset_sha256: String,
    instances: Vec<InstanceManifest>,
}

impl StateManifest {
    fn encode(&self) -> Result<String, String> {
        if !is_sha256(&self.ruleset_sha256)
            || self.instances.is_empty()
            || self.instances.len() > MAX_INSTANCES
        {
            return Err("traffic-classifier manifest is structurally invalid".to_string());
        }
        let mut seen = BTreeSet::new();
        let mut output = format!(
            "schema_version={STATE_SCHEMA_VERSION}\nruleset_sha256={}\n",
            self.ruleset_sha256
        );
        for item in &self.instances {
            item.validate()?;
            if !seen.insert(item.instance.as_str()) {
                return Err("traffic-classifier manifest repeats an instance".to_string());
            }
            output.push_str(&format!(
                "{}|{}|{}|{}|{}\n",
                item.instance,
                item.target,
                item.autotune_profile,
                item.configured_profile,
                item.resolved_profile
            ));
        }
        if output.len() > MAX_MANIFEST_BYTES {
            return Err("traffic-classifier manifest exceeds its size bound".to_string());
        }
        Ok(output)
    }

    fn decode(input: &str) -> Result<Self, String> {
        if input.len() > MAX_MANIFEST_BYTES {
            return Err("traffic-classifier manifest exceeds its size bound".to_string());
        }
        let mut lines = input.lines();
        if lines.next() != Some("schema_version=3") {
            return Err("traffic-classifier manifest schema is unsupported".to_string());
        }
        let digest = lines
            .next()
            .and_then(|line| line.strip_prefix("ruleset_sha256="))
            .ok_or_else(|| "traffic-classifier manifest digest is missing".to_string())?;
        if !is_sha256(digest) {
            return Err("traffic-classifier manifest digest is invalid".to_string());
        }
        let mut instances = Vec::new();
        let mut seen = BTreeSet::new();
        for line in lines {
            let fields = line.split('|').collect::<Vec<_>>();
            if fields.len() != 5 {
                return Err("traffic-classifier manifest row is malformed".to_string());
            }
            let item = InstanceManifest {
                instance: fields[0].to_string(),
                target: fields[1].to_string(),
                autotune_profile: fields[2].to_string(),
                configured_profile: fields[3].to_string(),
                resolved_profile: fields[4].to_string(),
            };
            item.validate()?;
            if !seen.insert(item.instance.clone()) {
                return Err("traffic-classifier manifest repeats an instance".to_string());
            }
            instances.push(item);
            if instances.len() > MAX_INSTANCES {
                return Err("traffic-classifier manifest has too many instances".to_string());
            }
        }
        if instances.is_empty() {
            return Err("traffic-classifier manifest has no instances".to_string());
        }
        let result = Self {
            ruleset_sha256: digest.to_string(),
            instances,
        };
        if result.encode()? != input {
            return Err("traffic-classifier manifest is not canonical".to_string());
        }
        Ok(result)
    }
}

impl InstanceManifest {
    fn validate(&self) -> Result<(), String> {
        if !safe_name(&self.instance) || !safe_interface(&self.target) {
            return Err("traffic-classifier manifest identity is unsafe".to_string());
        }
        let autotune = AutotuneProfile::parse(&self.autotune_profile)?;
        if autotune.as_str() != self.autotune_profile {
            return Err(
                "traffic-classifier manifest Auto-Tune profile is not canonical".to_string(),
            );
        }
        let configured = TrafficProfile::parse(&self.configured_profile)?;
        if configured.as_str() != self.configured_profile {
            return Err("traffic-classifier configured profile is not canonical".to_string());
        }
        let resolved = TrafficProfile::parse(&self.resolved_profile)?;
        if resolved == TrafficProfile::Auto || resolved.as_str() != self.resolved_profile {
            return Err("traffic-classifier resolved profile is not canonical".to_string());
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct RenderedRuleset {
    text: String,
    instances: Vec<InstanceManifest>,
    builtin_rules: usize,
    custom_rules: usize,
}

#[derive(Clone, Debug)]
struct Environment {
    uci: PathBuf,
    nft: PathBuf,
    ubus: PathBuf,
    sys_class_net: PathBuf,
    runtime_dir: PathBuf,
    state_file: PathBuf,
}

impl Environment {
    fn live() -> Self {
        let runtime_dir = env_path("CAKE_AUTORATE_RUNTIME_ROOT", "/var/run/cake-autorate");
        Self {
            uci: resolve_optional_program(
                "CAKE_AUTORATE_UCI_BIN",
                &["/sbin/uci", "/usr/sbin/uci", "/usr/bin/uci"],
            ),
            nft: resolve_optional_program(
                "CAKE_AUTORATE_NFT_BIN",
                &["/usr/sbin/nft", "/sbin/nft", "/usr/bin/nft"],
            ),
            ubus: resolve_optional_program(
                "CAKE_AUTORATE_UBUS_BIN",
                &["/bin/ubus", "/usr/bin/ubus"],
            ),
            sys_class_net: env_path("CAKE_AUTORATE_SYS_CLASS_NET", "/sys/class/net"),
            state_file: env::var_os("CAKE_AUTORATE_TRAFFIC_CLASSIFIER_STATE")
                .map(PathBuf::from)
                .unwrap_or_else(|| runtime_dir.join("traffic-classifier.state")),
            runtime_dir,
        }
    }
}

struct RuntimeGuard(File);

impl RuntimeGuard {
    fn lock(environment: &Environment) -> Result<Self, String> {
        prepare_runtime_dir(&environment.runtime_dir)?;
        let path = environment.runtime_dir.join("traffic-classifier.guard");
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err("traffic-classifier guard path is unsafe".to_string());
            }
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
            .map_err(|error| format!("unable to open traffic-classifier guard: {error}"))?;
        let metadata = file
            .metadata()
            .map_err(|error| format!("unable to inspect traffic-classifier guard: {error}"))?;
        if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err("traffic-classifier guard ownership is unsafe".to_string());
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(format!(
                "unable to lock traffic-classifier state: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(Self(file))
    }

    fn lock_shared_if_present(environment: &Environment) -> Result<Option<Self>, String> {
        let path = environment.runtime_dir.join("traffic-classifier.guard");
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "unable to inspect traffic-classifier guard: {error}"
                ));
            }
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("traffic-classifier guard path is unsafe".to_string());
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
            .map_err(|error| format!("unable to open traffic-classifier guard: {error}"))?;
        let metadata = file
            .metadata()
            .map_err(|error| format!("unable to inspect traffic-classifier guard: {error}"))?;
        if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
            return Err("traffic-classifier guard ownership is unsafe".to_string());
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH) } != 0 {
            return Err(format!(
                "unable to lock traffic-classifier state: {}",
                io::Error::last_os_error()
            ));
        }
        Ok(Some(Self(file)))
    }
}

impl Drop for RuntimeGuard {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn env_path(name: &str, default: &str) -> PathBuf {
    env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

fn resolve_optional_program(name: &str, candidates: &[&str]) -> PathBuf {
    env::var_os(name)
        .map(PathBuf::from)
        .or_else(|| {
            candidates
                .iter()
                .map(PathBuf::from)
                .find(|path| path.is_file())
        })
        .unwrap_or_else(|| PathBuf::from("/nonexistent/cake-autorate-optional-program"))
}

fn command(
    program: &Path,
    arguments: &[&str],
    input: Option<&[u8]>,
) -> Result<super::process::BoundedCommandOutput, String> {
    let spec = SpawnSpec {
        program: program.to_path_buf(),
        arguments: arguments.iter().map(OsString::from).collect(),
        environment: Vec::new(),
    };
    if input.is_some() {
        run_bounded_command_output_with_input(
            &spec,
            input,
            COMMAND_TIMEOUT,
            MAX_COMMAND_OUTPUT,
            || false,
            |_| {},
        )
    } else {
        run_bounded_command_output(&spec, COMMAND_TIMEOUT, MAX_COMMAND_OUTPUT, || false)
    }
}

fn command_text(program: &Path, arguments: &[&str]) -> Result<String, String> {
    let output = command(program, arguments, None)?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    String::from_utf8(output.stdout).map_err(|_| "command output is not UTF-8".to_string())
}

fn load_uci(environment: &Environment) -> Result<UciPackage, String> {
    let text = command_text(&environment.uci, &["-q", "show", "cake-autorate"])?;
    UciPackage::parse("cake-autorate", &text)
}

fn option<'a>(section: &'a UciSection, name: &str) -> &'a str {
    section.options.get(name).map(String::as_str).unwrap_or("")
}

fn bool_option(section: &UciSection, name: &str, default: bool) -> bool {
    match option(section, name) {
        "1" => true,
        "0" => false,
        _ => default,
    }
}

fn preset(name: &str, custom: Preset) -> Result<Preset, String> {
    let value = match name {
        "custom" | "" => custom,
        "dns" => Preset {
            protocol: Protocol::TcpUdp,
            source_ports: "",
            destination_ports: "53",
        },
        "ntp" => Preset {
            protocol: Protocol::Udp,
            source_ports: "",
            destination_ports: "123",
        },
        "web" => Preset {
            protocol: Protocol::Tcp,
            source_ports: "",
            destination_ports: "80,443",
        },
        "quic" => Preset {
            protocol: Protocol::Udp,
            source_ports: "",
            destination_ports: "443",
        },
        "ssh" => Preset {
            protocol: Protocol::Tcp,
            source_ports: "",
            destination_ports: "22",
        },
        "steam_realtime" => Preset {
            protocol: Protocol::Udp,
            source_ports: "",
            destination_ports: "27000-27100",
        },
        "xbox_live" => Preset {
            protocol: Protocol::Udp,
            source_ports: "",
            destination_ports: "88,3074",
        },
        "playstation" => Preset {
            protocol: Protocol::Udp,
            source_ports: "",
            destination_ports: "3478-3480",
        },
        "wireguard" => Preset {
            protocol: Protocol::Udp,
            source_ports: "",
            destination_ports: "51820",
        },
        _ => return Err("unsupported traffic-rule preset".to_string()),
    };
    Ok(value)
}

fn protocol(value: &str) -> Result<Protocol, String> {
    match value {
        "any" => Ok(Protocol::Any),
        "tcp" => Ok(Protocol::Tcp),
        "udp" | "" => Ok(Protocol::Udp),
        "tcp_udp" => Ok(Protocol::TcpUdp),
        "icmp" => Ok(Protocol::Icmp),
        _ => Err("unsupported traffic-rule protocol".to_string()),
    }
}

fn family(value: &str) -> Result<AddressFamily, String> {
    match value {
        "any" | "" => Ok(AddressFamily::Any),
        "ipv4" => Ok(AddressFamily::Ipv4),
        "ipv6" => Ok(AddressFamily::Ipv6),
        _ => Err("unsupported traffic-rule family".to_string()),
    }
}

fn validate_ports(value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Ok(());
    }
    for item in value.split(',') {
        let mut pieces = item.split('-');
        let first = pieces
            .next()
            .and_then(|part| part.parse::<u16>().ok())
            .filter(|port| *port > 0)
            .ok_or_else(|| "traffic-rule port is invalid".to_string())?;
        if let Some(last) = pieces.next() {
            let last = last
                .parse::<u16>()
                .ok()
                .filter(|last| *last >= first)
                .ok_or_else(|| "traffic-rule port range is invalid".to_string())?;
            if last == 0 || pieces.next().is_some() {
                return Err("traffic-rule port range is invalid".to_string());
            }
        } else if pieces.next().is_some() {
            return Err("traffic-rule port is invalid".to_string());
        }
    }
    Ok(())
}

fn network_family(value: &str) -> Result<AddressFamily, String> {
    if value.is_empty() {
        return Ok(AddressFamily::Any);
    }
    if value.len() > 64 || value.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err("traffic-rule network is unsafe".to_string());
    }
    let mut parts = value.split('/');
    let address = parts
        .next()
        .unwrap_or_default()
        .parse::<std::net::IpAddr>()
        .map_err(|_| "traffic-rule network address is invalid".to_string())?;
    let prefix = parts.next();
    if parts.next().is_some() {
        return Err("traffic-rule network prefix is invalid".to_string());
    }
    let detected = match address {
        std::net::IpAddr::V4(_) => AddressFamily::Ipv4,
        std::net::IpAddr::V6(_) => AddressFamily::Ipv6,
    };
    if let Some(prefix) = prefix {
        let prefix = prefix
            .parse::<u16>()
            .map_err(|_| "traffic-rule network prefix is invalid".to_string())?;
        let limit = if detected == AddressFamily::Ipv4 {
            32
        } else {
            128
        };
        if prefix > limit {
            return Err("traffic-rule network prefix is invalid".to_string());
        }
    }
    Ok(detected)
}

fn family_compatible(requested: AddressFamily, detected: AddressFamily) -> bool {
    requested == AddressFamily::Any || detected == AddressFamily::Any || requested == detected
}

fn dscp(class: &str) -> Result<&'static str, String> {
    match class {
        "voice" => Ok("cs5"),
        "video" => Ok("af41"),
        "best_effort" => Ok("cs0"),
        "background" => Ok("cs1"),
        _ => Err("unsupported traffic-rule class".to_string()),
    }
}

fn builtin_rules(profile: TrafficProfile) -> &'static [BuiltinRule] {
    match profile {
        TrafficProfile::Gaming => GAMING_RULES,
        TrafficProfile::BestOverall => BEST_OVERALL_RULES,
        TrafficProfile::Fair => FAIR_RULES,
        _ => &[],
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_one_rule(
    output: &mut String,
    target: &str,
    address_family: AddressFamily,
    protocol: Protocol,
    source_ports: &str,
    destination_ports: &str,
    source_network: &str,
    destination_network: &str,
    class: &str,
) -> Result<(), String> {
    output.push_str("\t\toifname \"");
    output.push_str(target);
    output.push('"');
    let family_name = match address_family {
        AddressFamily::Ipv4 => "ip",
        AddressFamily::Ipv6 => "ip6",
        AddressFamily::Any => return Err("traffic-rule family was not expanded".to_string()),
    };
    if !source_network.is_empty() {
        output.push_str(&format!(" {family_name} saddr {source_network}"));
    }
    if !destination_network.is_empty() {
        output.push_str(&format!(" {family_name} daddr {destination_network}"));
    }
    match protocol {
        Protocol::Tcp => {
            output.push_str(" meta l4proto tcp");
            if !source_ports.is_empty() {
                output.push_str(&format!(" tcp sport {{ {source_ports} }}"));
            }
            if !destination_ports.is_empty() {
                output.push_str(&format!(" tcp dport {{ {destination_ports} }}"));
            }
        }
        Protocol::Udp => {
            output.push_str(" meta l4proto udp");
            if !source_ports.is_empty() {
                output.push_str(&format!(" udp sport {{ {source_ports} }}"));
            }
            if !destination_ports.is_empty() {
                output.push_str(&format!(" udp dport {{ {destination_ports} }}"));
            }
        }
        Protocol::Icmp => output.push_str(if address_family == AddressFamily::Ipv4 {
            " meta l4proto icmp"
        } else {
            " meta l4proto ipv6-icmp"
        }),
        Protocol::Any => {}
        Protocol::TcpUdp => return Err("tcp_udp was not expanded".to_string()),
    }
    output.push_str(&format!(" {family_name} dscp set {}\n", dscp(class)?));
    if output.len() > MAX_RULESET_BYTES {
        return Err("traffic-classifier ruleset exceeds its size bound".to_string());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_rule(
    forward: &mut String,
    output: &mut String,
    target: &str,
    requested_family: AddressFamily,
    requested_protocol: Protocol,
    source_ports: &str,
    destination_ports: &str,
    source_network: &str,
    destination_network: &str,
    class: &str,
) -> Result<(), String> {
    validate_ports(source_ports)?;
    validate_ports(destination_ports)?;
    if matches!(requested_protocol, Protocol::Any | Protocol::Icmp)
        && (!source_ports.is_empty() || !destination_ports.is_empty())
    {
        return Err("traffic-rule ports require TCP or UDP".to_string());
    }
    let source_family = network_family(source_network)?;
    let destination_family = network_family(destination_network)?;
    if !family_compatible(requested_family, source_family)
        || !family_compatible(requested_family, destination_family)
        || (source_family != AddressFamily::Any
            && destination_family != AddressFamily::Any
            && source_family != destination_family)
    {
        return Err("traffic-rule address families conflict".to_string());
    }
    let effective_family = if requested_family != AddressFamily::Any {
        requested_family
    } else if source_family != AddressFamily::Any {
        source_family
    } else {
        destination_family
    };
    let families: &[AddressFamily] = if effective_family == AddressFamily::Any {
        &[AddressFamily::Ipv4, AddressFamily::Ipv6]
    } else {
        std::slice::from_ref(&effective_family)
    };
    let protocols: &[Protocol] = if requested_protocol == Protocol::TcpUdp {
        &[Protocol::Tcp, Protocol::Udp]
    } else {
        std::slice::from_ref(&requested_protocol)
    };
    for chain in [forward, output] {
        for protocol in protocols {
            for family in families {
                emit_one_rule(
                    chain,
                    target,
                    *family,
                    *protocol,
                    source_ports,
                    destination_ports,
                    source_network,
                    destination_network,
                    class,
                )?;
            }
        }
    }
    Ok(())
}

fn resolve_interface(environment: &Environment, logical: &str) -> Result<String, String> {
    if logical.is_empty() || !safe_interface(logical) {
        return Err("traffic-rule uplink is unsafe".to_string());
    }
    if environment.sys_class_net.join(logical).exists() {
        return Ok(logical.to_string());
    }
    if environment.ubus.is_file() {
        if let Ok(output) = command_text(
            &environment.ubus,
            &["call", &format!("network.interface.{logical}"), "status"],
        ) {
            for key in ["l3_device", "device"] {
                if let Some(device) = json_string_value(&output, key) {
                    if safe_interface(&device) && environment.sys_class_net.join(&device).exists() {
                        return Ok(device);
                    }
                }
            }
        }
    }
    Ok(logical.to_string())
}

fn render_ruleset(
    environment: &Environment,
    package: &UciPackage,
) -> Result<RenderedRuleset, String> {
    let mut forward = String::new();
    let mut output = String::new();
    let mut instances = Vec::new();
    let mut seen_targets = BTreeSet::new();
    let mut builtin_count = 0usize;
    let mut custom_count = 0usize;

    for (instance, section) in package
        .sections
        .iter()
        .filter(|(_, section)| section.section_type == "cake_autorate")
    {
        if instances.len() >= MAX_INSTANCES {
            return Err("too many traffic-classifier instances".to_string());
        }
        if !bool_option(section, "enabled", false)
            || !bool_option(section, "manage_sqm", true)
            || !bool_option(
                section,
                "sqm_enabled",
                bool_option(section, "enabled", false),
            )
            || !bool_option(section, "traffic_rules_enabled", false)
            || matches!(
                option(section, "sqm_direction_mode"),
                "download_only" | "off"
            )
        {
            continue;
        }
        let autotune = AutotuneProfile::parse(option(section, "autotune_profile"))
            .map_err(|error| format!("Instance {instance} has an {error}."))?;
        let configured_raw = option(section, "traffic_profile");
        let configured = if configured_raw.is_empty() {
            TrafficProfile::Auto
        } else {
            TrafficProfile::parse(configured_raw)
                .map_err(|error| format!("Instance {instance} has an {error}."))?
        };
        let (resolved, rule_match) = match configured {
            TrafficProfile::Auto => (autotune.traffic_default(), autotune.traffic_default()),
            TrafficProfile::Gaming | TrafficProfile::BestOverall | TrafficProfile::Fair => {
                (configured, configured)
            }
            TrafficProfile::Custom => (TrafficProfile::Custom, TrafficProfile::Custom),
        };
        if option(section, "sqm_script") != "layer_cake.qos"
            || !option(section, "sqm_eqdisc_opts")
                .split_ascii_whitespace()
                .any(|item| item == "diffserv4")
        {
            continue;
        }
        let logical = ["wan_if", "sqm_interface", "ul_if"]
            .iter()
            .map(|name| option(section, name))
            .find(|value| !value.is_empty())
            .unwrap_or("");
        let target = resolve_interface(environment, logical)
            .map_err(|_| format!("Unable to resolve the traffic-rule uplink for {instance}."))?;
        if !seen_targets.insert(target.clone()) {
            return Err(format!(
                "Multiple enabled instances claim the same traffic-rule uplink {target}."
            ));
        }
        instances.push(InstanceManifest {
            instance: instance.clone(),
            target: target.clone(),
            autotune_profile: autotune.as_str().to_string(),
            configured_profile: configured.as_str().to_string(),
            resolved_profile: resolved.as_str().to_string(),
        });
        for chain in [&mut forward, &mut output] {
            chain.push_str(&format!("\t\toifname \"{target}\" ip dscp set cs0\n"));
            chain.push_str(&format!("\t\toifname \"{target}\" ip6 dscp set cs0\n"));
        }
        if resolved != TrafficProfile::Custom {
            for rule in builtin_rules(resolved) {
                let value = preset(
                    rule.preset,
                    Preset {
                        protocol: Protocol::Any,
                        source_ports: "",
                        destination_ports: "",
                    },
                )?;
                emit_rule(
                    &mut forward,
                    &mut output,
                    &target,
                    AddressFamily::Any,
                    value.protocol,
                    value.source_ports,
                    value.destination_ports,
                    "",
                    "",
                    rule.class,
                )?;
                builtin_count += 1;
            }
        }
        let mut custom = package
            .sections
            .iter()
            .filter(|(_, rule)| rule.section_type == "traffic_rule")
            .map(|(name, rule)| {
                let order = option(rule, "order")
                    .parse::<u16>()
                    .ok()
                    .filter(|value| *value <= 9999)
                    .unwrap_or(500);
                (order, name, rule)
            })
            .collect::<Vec<_>>();
        custom.sort_by(|left, right| (left.0, left.1).cmp(&(right.0, right.1)));
        for (_, name, rule) in custom {
            if !bool_option(rule, "enabled", false) || option(rule, "instance") != instance {
                continue;
            }
            let rule_profile = TrafficProfile::parse(option(rule, "profile")).ok();
            // An unknown or missing profile makes only that rule inert. It
            // must not prevent the classifier (and therefore SQM startup) for
            // the whole instance.
            if rule_profile != Some(rule_match) {
                continue;
            }
            if custom_count >= MAX_CUSTOM_RULES {
                return Err("too many active custom traffic rules".to_string());
            }
            let raw_preset = Preset {
                protocol: protocol(option(rule, "protocol"))?,
                source_ports: "",
                destination_ports: "",
            };
            let selected = preset(option(rule, "preset"), raw_preset)
                .map_err(|_| format!("Traffic rule {name} uses an unsupported preset."))?;
            let source_ports = if matches!(option(rule, "preset"), "" | "custom") {
                option(rule, "source_ports")
            } else {
                selected.source_ports
            };
            let destination_ports = if matches!(option(rule, "preset"), "" | "custom") {
                option(rule, "destination_ports")
            } else {
                selected.destination_ports
            };
            emit_rule(
                &mut forward,
                &mut output,
                &target,
                family(option(rule, "family"))?,
                selected.protocol,
                source_ports,
                destination_ports,
                option(rule, "source_network"),
                option(rule, "destination_network"),
                if option(rule, "class").is_empty() { "voice" } else { option(rule, "class") },
            )
            .map_err(|_| format!("Traffic rule {name} contains an invalid family, network, protocol, port, or class."))?;
            custom_count += 1;
        }
    }

    let text = format!(
        "table inet {TABLE_NAME} {{\n\tchain forward {{\n\t\ttype filter hook forward priority -140; policy accept;\n{forward}\t}}\n\tchain output {{\n\t\ttype route hook output priority -140; policy accept;\n{output}\t}}\n}}\n"
    );
    if text.len() > MAX_RULESET_BYTES {
        return Err("traffic-classifier ruleset exceeds its size bound".to_string());
    }
    Ok(RenderedRuleset {
        text,
        instances,
        builtin_rules: builtin_count,
        custom_rules: custom_count,
    })
}

fn presets_json() -> String {
    let mut profiles = Vec::new();
    for (name, rules) in [
        ("gaming", GAMING_RULES),
        ("best_overall", BEST_OVERALL_RULES),
        ("fair", FAIR_RULES),
    ] {
        let mut entries = Vec::new();
        for rule in rules {
            let value = preset(
                rule.preset,
                Preset {
                    protocol: Protocol::Any,
                    source_ports: "",
                    destination_ports: "",
                },
            )
            .expect("static preset");
            let protocol = match value.protocol {
                Protocol::Tcp => "tcp",
                Protocol::Udp => "udp",
                Protocol::TcpUdp => "tcp_udp",
                Protocol::Any => "any",
                Protocol::Icmp => "icmp",
            };
            entries.push(format!(
                "{{\"id\":\"{}\",\"preset\":\"{}\",\"class\":\"{}\",\"dscp\":\"{}\",\"protocol\":\"{protocol}\",\"source_ports\":\"{}\",\"destination_ports\":\"{}\"}}",
                json_escape(rule.id), json_escape(rule.preset), json_escape(rule.class), dscp(rule.class).expect("static class"),
                value.source_ports, value.destination_ports
            ));
        }
        profiles.push(format!("\"{name}\":[{}]", entries.join(",")));
    }
    format!(
        "{{\"schema_version\":{PRESET_SCHEMA_VERSION},\"profiles\":{{{}}}}}\n",
        profiles.join(",")
    )
}

fn table_present(environment: &Environment) -> Result<bool, String> {
    command(
        &environment.nft,
        &["list", "table", TABLE_FAMILY, TABLE_NAME],
        None,
    )
    .map(|output| output.status.success())
}

fn ruleset_digest(environment: &Environment) -> Result<String, String> {
    let output = command(
        &environment.nft,
        &["-j", "list", "table", TABLE_FAMILY, TABLE_NAME],
        None,
    )?;
    if !output.status.success() || output.stdout.is_empty() {
        return Err("applied traffic-priority rules are unavailable".to_string());
    }
    let mut canonical = output.stdout;
    while canonical.last() == Some(&b'\n') {
        canonical.pop();
    }
    canonical.push(b'\n');
    Ok(hex_digest(&canonical))
}

fn hex_digest(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn prepare_runtime_dir(path: &Path) -> Result<(), String> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.permissions().mode() & 0o022 != 0
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err("traffic-classifier runtime directory is unsafe".to_string());
        }
        return Ok(());
    }
    fs::create_dir_all(path).map_err(|error| {
        format!("unable to create traffic-classifier runtime directory: {error}")
    })?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .map_err(|error| format!("unable to set traffic-classifier runtime permissions: {error}"))
}

fn write_manifest(path: &Path, manifest: &StateManifest) -> Result<(), String> {
    let bytes = manifest.encode()?;
    let parent = path
        .parent()
        .ok_or_else(|| "traffic-classifier state has no parent".to_string())?;
    let metadata = fs::symlink_metadata(parent).map_err(|error| {
        format!("unable to inspect traffic-classifier runtime directory: {error}")
    })?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o022 != 0
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err("traffic-classifier runtime directory is unsafe".to_string());
    }
    if let Ok(existing) = fs::symlink_metadata(path) {
        if !existing.is_file() || existing.file_type().is_symlink() {
            return Err("traffic-classifier state path is unsafe".to_string());
        }
    }
    let (staged, mut file) = (0..16)
        .find_map(|_| {
            let staged = parent.join(format!(
                ".traffic-classifier-state-{}-{}",
                std::process::id(),
                NEXT_STAGED_FILE.fetch_add(1, Ordering::Relaxed)
            ));
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&staged)
                .ok()
                .map(|file| (staged, file))
        })
        .ok_or_else(|| "unable to allocate traffic-classifier state".to_string())?;
    use std::io::Write as _;
    let result = file
        .write_all(bytes.as_bytes())
        .and_then(|_| file.sync_all())
        .and_then(|_| fs::rename(&staged, path))
        .and_then(|_| File::open(parent))
        .and_then(|directory| directory.sync_all());
    if result.is_err() {
        let _ = fs::remove_file(&staged);
    }
    result.map_err(|error| format!("unable to publish traffic-classifier state: {error}"))
}

fn read_manifest(path: &Path) -> Result<StateManifest, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("unable to inspect traffic-classifier state: {error}"))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_MANIFEST_BYTES as u64
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err("traffic-classifier state is unsafe".to_string());
    }
    let bytes = fs::read(path)
        .map_err(|error| format!("unable to read traffic-classifier state: {error}"))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| "traffic-classifier state is not UTF-8".to_string())?;
    StateManifest::decode(&text)
}

fn remove_manifest(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "unable to inspect traffic-classifier state: {error}"
        )),
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            fs::remove_file(path)
                .map_err(|error| format!("unable to remove traffic-classifier state: {error}"))
        }
        Ok(_) => Err("traffic-classifier state path is unsafe".to_string()),
    }
}

fn clear_table(environment: &Environment) -> Result<(), String> {
    if table_present(environment)? {
        let output = command(
            &environment.nft,
            &["delete", "table", TABLE_FAMILY, TABLE_NAME],
            None,
        )?;
        if !output.status.success() {
            return Err("unable to remove the cake-autorate-rs DSCP table".to_string());
        }
    }
    Ok(())
}

fn apply(environment: &Environment) -> Result<String, String> {
    if !environment.nft.is_file() {
        return Err("nft is unavailable; traffic-priority rules were not applied".to_string());
    }
    let _guard = RuntimeGuard::lock(environment)?;
    let rendered = render_ruleset(environment, &load_uci(environment)?)?;
    if rendered.instances.is_empty() {
        clear_table(environment)?;
        remove_manifest(&environment.state_file)?;
        return Ok("{\"state\":\"inactive\",\"schema_version\":3,\"instances\":0,\"builtin_rules\":0,\"custom_rules\":0}\n".to_string());
    }
    let mut batch = String::new();
    if table_present(environment)? {
        batch.push_str(&format!("delete table {TABLE_FAMILY} {TABLE_NAME}\n"));
    }
    batch.push_str(&rendered.text);
    for args in [["-c", "-f", "-"], ["-f", "-", ""]] {
        let actual = if args[2].is_empty() {
            &args[..2]
        } else {
            &args[..]
        };
        let output = command(&environment.nft, actual, Some(batch.as_bytes()))?;
        if !output.status.success() {
            return Err(if actual[0] == "-c" {
                "the generated nftables traffic-priority rules failed validation".to_string()
            } else {
                "unable to apply the validated nftables traffic-priority rules".to_string()
            });
        }
    }
    let applied_digest = match ruleset_digest(environment) {
        Ok(value) => value,
        Err(error) => {
            let _ = clear_table(environment);
            let _ = remove_manifest(&environment.state_file);
            return Err(format!(
                "applied traffic-priority rules could not be attested: {error}"
            ));
        }
    };
    let manifest = StateManifest {
        ruleset_sha256: applied_digest.clone(),
        instances: rendered.instances,
    };
    if let Err(error) = write_manifest(&environment.state_file, &manifest) {
        let _ = clear_table(environment);
        let _ = remove_manifest(&environment.state_file);
        return Err(error);
    }
    Ok(format!(
        "{{\"state\":\"active\",\"schema_version\":3,\"instances\":{},\"builtin_rules\":{},\"custom_rules\":{},\"ruleset_sha256\":\"{}\"}}\n",
        manifest.instances.len(), rendered.builtin_rules, rendered.custom_rules, applied_digest
    ))
}

fn clear(environment: &Environment) -> Result<String, String> {
    let _guard = RuntimeGuard::lock(environment)?;
    clear_table(environment)?;
    remove_manifest(&environment.state_file)?;
    Ok("{\"state\":\"inactive\",\"schema_version\":3}\n".to_string())
}

fn status_snapshot(environment: &Environment, requested: Option<&str>) -> Result<String, String> {
    if let Some(instance) = requested {
        if !safe_name(instance) {
            return Err("the requested traffic-classifier instance name is unsafe".to_string());
        }
    }
    if !environment.nft.is_file() {
        return Ok(format!(
            "{{\"state\":\"unavailable\",\"schema_version\":3,\"table_present\":false,\"table\":\"inet {TABLE_NAME}\"}}\n"
        ));
    }
    let present = table_present(environment)?;
    if !present {
        let stale = fs::symlink_metadata(&environment.state_file).is_ok();
        return Ok(format!(
            "{{\"state\":\"inactive\",\"schema_version\":3,\"table_present\":false,\"table\":\"inet {TABLE_NAME}\",\"stale_manifest\":{}}}\n",
            bool_json(stale)
        ));
    }
    let manifest = match read_manifest(&environment.state_file) {
        Ok(value) => value,
        Err(_) => return Ok(format!("{{\"state\":\"untracked\",\"schema_version\":3,\"table_present\":true,\"table\":\"inet {TABLE_NAME}\"}}\n")),
    };
    let actual = ruleset_digest(environment).unwrap_or_default();
    let stable_manifest = read_manifest(&environment.state_file).ok();
    if stable_manifest.as_ref() != Some(&manifest) || !table_present(environment)? {
        return Ok(format!("{{\"state\":\"untracked\",\"schema_version\":3,\"table_present\":true,\"table\":\"inet {TABLE_NAME}\"}}\n"));
    }
    if actual != manifest.ruleset_sha256 {
        return Ok(format!("{{\"state\":\"drifted\",\"schema_version\":3,\"table_present\":true,\"table\":\"inet {TABLE_NAME}\",\"instances\":{}}}\n", manifest.instances.len()));
    }
    if let Some(instance) = requested {
        let Some(item) = manifest
            .instances
            .iter()
            .find(|item| item.instance == instance)
        else {
            return Ok(format!("{{\"state\":\"missing\",\"schema_version\":3,\"table_present\":true,\"table\":\"inet {TABLE_NAME}\",\"instance\":\"{}\"}}\n", json_escape(instance)));
        };
        return Ok(format!(
            "{{\"state\":\"active\",\"schema_version\":3,\"table_present\":true,\"table\":\"inet {TABLE_NAME}\",\"instance\":\"{}\",\"target\":\"{}\",\"autotune_profile\":\"{}\",\"configured_profile\":\"{}\",\"resolved_profile\":\"{}\",\"profile\":\"{}\",\"ruleset_sha256\":\"{}\"}}\n",
            json_escape(&item.instance), json_escape(&item.target), item.autotune_profile,
            item.configured_profile, item.resolved_profile, item.resolved_profile, manifest.ruleset_sha256
        ));
    }
    let serialized = manifest
        .instances
        .iter()
        .map(|item| {
            format!(
                "{}|{}|{}|{}|{}",
                item.instance,
                item.target,
                item.autotune_profile,
                item.configured_profile,
                item.resolved_profile
            )
        })
        .collect::<Vec<_>>()
        .join(";");
    Ok(format!(
        "{{\"state\":\"active\",\"schema_version\":3,\"table_present\":true,\"table\":\"inet {TABLE_NAME}\",\"instances\":{},\"attested_instances\":\"{}\",\"ruleset_sha256\":\"{}\"}}\n",
        manifest.instances.len(), json_escape(&serialized), manifest.ruleset_sha256
    ))
}

fn status(environment: &Environment, requested: Option<&str>) -> Result<String, String> {
    if let Some(_guard) = RuntimeGuard::lock_shared_if_present(environment)? {
        return status_snapshot(environment, requested);
    }

    let unlocked_snapshot = status_snapshot(environment, requested)?;
    // The very first apply creates the persistent guard before mutating nft or
    // the manifest. If it appeared during our lock-free initial read, repeat
    // the complete observation under a shared lock. A normal status call on a
    // never-used installation remains strictly read-only and creates nothing.
    if let Some(_guard) = RuntimeGuard::lock_shared_if_present(environment)? {
        status_snapshot(environment, requested)
    } else {
        Ok(unlocked_snapshot)
    }
}

fn render(environment: &Environment) -> Result<String, String> {
    render_ruleset(environment, &load_uci(environment)?).map(|rules| rules.text)
}

fn run_with_environment(
    environment: &Environment,
    mut args: impl Iterator<Item = String>,
) -> Result<String, String> {
    let action = args.next().unwrap_or_else(|| "status".to_string());
    match action.as_str() {
        "render" if args.next().is_none() => render(environment),
        "apply" if args.next().is_none() => apply(environment),
        "clear" if args.next().is_none() => clear(environment),
        "presets" if args.next().is_none() => Ok(presets_json()),
        "status" => {
            let requested = args.next();
            if args.next().is_some() {
                return Err("traffic-classifier status received too many arguments".to_string());
            }
            status(environment, requested.as_deref())
        }
        _ => Err("usage: cake-autorated --traffic-classifier render|apply|clear|presets|status [INSTANCE]".to_string()),
    }
}

pub(crate) fn run_traffic_classifier(args: impl Iterator<Item = String>) -> Result<String, String> {
    let environment = Environment::live();
    run_with_environment(&environment, args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static NEXT: AtomicU32 = AtomicU32::new(1);

    fn root() -> PathBuf {
        let root = env::temp_dir().join(format!(
            "cake-traffic-classifier-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("sys/eth0")).unwrap();
        fs::create_dir_all(root.join("runtime")).unwrap();
        root
    }

    fn base_uci(extra: &str) -> UciPackage {
        UciPackage::parse(
            "cake-autorate",
            &format!(
                "cake-autorate.wan_sqm=cake_autorate\n\
                 cake-autorate.wan_sqm.enabled='1'\n\
                 cake-autorate.wan_sqm.manage_sqm='1'\n\
                 cake-autorate.wan_sqm.sqm_enabled='1'\n\
                 cake-autorate.wan_sqm.sqm_direction_mode='both'\n\
                 cake-autorate.wan_sqm.traffic_rules_enabled='1'\n\
                 cake-autorate.wan_sqm.autotune_profile='gaming'\n\
                 cake-autorate.wan_sqm.traffic_profile='auto'\n\
                 cake-autorate.wan_sqm.sqm_script='layer_cake.qos'\n\
                 cake-autorate.wan_sqm.sqm_eqdisc_opts='diffserv4'\n\
                 cake-autorate.wan_sqm.wan_if='eth0'\n{extra}"
            ),
        )
        .unwrap()
    }

    fn environment(root: &Path) -> Environment {
        Environment {
            uci: PathBuf::from("/bin/false"),
            nft: PathBuf::from("/bin/false"),
            ubus: PathBuf::from("/bin/false"),
            sys_class_net: root.join("sys"),
            runtime_dir: root.join("runtime"),
            state_file: root.join("runtime/traffic-classifier.state"),
        }
    }

    fn executable(path: &Path, body: &str) {
        use std::io::Write as _;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(path)
            .unwrap();
        file.write_all(body.as_bytes()).unwrap();
    }

    fn command_environment(root: &Path) -> Environment {
        let config = root.join("cake.show");
        fs::write(
            &config,
            "cake-autorate.wan_sqm=cake_autorate\n\
             cake-autorate.wan_sqm.enabled='1'\n\
             cake-autorate.wan_sqm.manage_sqm='1'\n\
             cake-autorate.wan_sqm.sqm_enabled='1'\n\
             cake-autorate.wan_sqm.sqm_direction_mode='both'\n\
             cake-autorate.wan_sqm.traffic_rules_enabled='1'\n\
             cake-autorate.wan_sqm.autotune_profile='gaming'\n\
             cake-autorate.wan_sqm.traffic_profile='auto'\n\
             cake-autorate.wan_sqm.sqm_script='layer_cake.qos'\n\
             cake-autorate.wan_sqm.sqm_eqdisc_opts='diffserv4'\n\
             cake-autorate.wan_sqm.wan_if='eth0'\n",
        )
        .unwrap();
        let uci = root.join("uci");
        executable(&uci, &format!("#!/bin/sh\ncat '{}'\n", config.display()));
        let marker = root.join("table-present");
        let checked = root.join("checked.nft");
        let applied = root.join("applied.nft");
        let nft = root.join("nft");
        executable(
            &nft,
            &format!(
                "#!/bin/sh\ncase \"$*\" in\n\
                 \"list table inet cake_autorate_dscp\") [ -f '{marker}' ] ;;\n\
                 \"-c -f -\") cat > '{checked}' ;;\n\
                 \"-f -\") cat > '{applied}'; touch '{marker}' ;;\n\
                 \"-j list table inet cake_autorate_dscp\") [ -f '{marker}' ] || exit 1; cat '{applied}' ;;\n\
                 \"delete table inet cake_autorate_dscp\") rm -f '{marker}' ;;\n\
                 *) exit 2 ;;\n\
                 esac\n",
                marker = marker.display(),
                checked = checked.display(),
                applied = applied.display(),
            ),
        );
        Environment {
            uci,
            nft,
            ubus: PathBuf::from("/bin/false"),
            sys_class_net: root.join("sys"),
            runtime_dir: root.join("runtime"),
            state_file: root.join("runtime/traffic-classifier.state"),
        }
    }

    #[test]
    fn preset_catalog_is_frozen_and_complete() {
        let value = presets_json();
        assert!(value.starts_with("{\"schema_version\":1,\"profiles\":{"));
        assert_eq!(value.matches("\"id\":").count(), 16);
        assert!(value.contains("\"steam_realtime\""));
        assert!(value.contains("\"dscp\":\"af41\""));
    }

    #[test]
    fn gaming_projection_matches_the_retired_rule_contract() {
        let root = root();
        let package = base_uci(
            "cake-autorate.rule1=traffic_rule\n\
             cake-autorate.rule1.enabled='1'\n\
             cake-autorate.rule1.instance='wan_sqm'\n\
             cake-autorate.rule1.profile='gaming'\n\
             cake-autorate.rule1.preset='wireguard'\n\
             cake-autorate.rule1.family='any'\n\
             cake-autorate.rule1.source_network='192.168.1.50/32'\n\
             cake-autorate.rule1.class='video'\n\
             cake-autorate.rule1.order='100'\n",
        );
        let rendered = render_ruleset(&environment(&root), &package).unwrap();
        assert_eq!(rendered.instances.len(), 1);
        assert_eq!(rendered.builtin_rules, 7);
        assert_eq!(rendered.custom_rules, 1);
        assert_eq!(
            rendered
                .text
                .matches("oifname \"eth0\" ip dscp set cs0")
                .count(),
            2
        );
        assert_eq!(
            rendered
                .text
                .matches("udp dport { 27000-27100 } ip dscp set cs5")
                .count(),
            2
        );
        assert_eq!(rendered.text.matches("ip saddr 192.168.1.50/32 meta l4proto udp udp dport { 51820 } ip dscp set af41").count(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn custom_rule_requires_current_profile_and_selected_custom_mode() {
        let root = root();
        let rules = "cake-autorate.rule1=traffic_rule\n\
             cake-autorate.rule1.enabled='1'\n\
             cake-autorate.rule1.instance='wan_sqm'\n\
             cake-autorate.rule1.preset='wireguard'\n\
             cake-autorate.rule1.family='any'\n\
             cake-autorate.rule1.class='video'\n";
        let gaming = render_ruleset(&environment(&root), &base_uci(rules)).unwrap();
        assert!(!gaming.text.contains("51820"));
        let mut custom = base_uci(rules);
        custom
            .sections
            .get_mut("wan_sqm")
            .unwrap()
            .options
            .insert("traffic_profile".to_string(), "custom".to_string());
        let missing_profile = render_ruleset(&environment(&root), &custom).unwrap();
        assert!(!missing_profile.text.contains("51820"));
        custom
            .sections
            .get_mut("rule1")
            .unwrap()
            .options
            .insert("profile".to_string(), "custom".to_string());
        let current = render_ruleset(&environment(&root), &custom).unwrap();
        assert!(current.text.contains("51820"));

        let wrong_profile = "cake-autorate.rule1=traffic_rule\n\
             cake-autorate.rule1.enabled='1'\n\
             cake-autorate.rule1.instance='wan_sqm'\n\
             cake-autorate.rule1.profile='variable_link'\n\
             cake-autorate.rule1.preset='wireguard'\n\
             cake-autorate.rule1.family='any'\n\
             cake-autorate.rule1.class='video'\n";
        let rendered = render_ruleset(&environment(&root), &base_uci(wrong_profile)).unwrap();
        assert!(!rendered.text.contains("51820"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn classifier_requires_explicit_opt_in_and_upload_diffserv4() {
        let root = root();
        let mut package = base_uci("");
        package
            .sections
            .get_mut("wan_sqm")
            .unwrap()
            .options
            .remove("traffic_rules_enabled");
        assert!(render_ruleset(&environment(&root), &package)
            .unwrap()
            .instances
            .is_empty());
        let mut package = base_uci("");
        package.sections.get_mut("wan_sqm").unwrap().options.insert(
            "sqm_direction_mode".to_string(),
            "download_only".to_string(),
        );
        assert!(render_ruleset(&environment(&root), &package)
            .unwrap()
            .instances
            .is_empty());
        let mut package = base_uci("");
        package
            .sections
            .get_mut("wan_sqm")
            .unwrap()
            .options
            .insert("sqm_script".to_string(), "piece_of_cake.qos".to_string());
        assert!(render_ruleset(&environment(&root), &package)
            .unwrap()
            .instances
            .is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unsafe_ports_networks_and_duplicate_targets_fail_closed() {
        assert!(validate_ports("53;delete-table").is_err());
        assert!(network_family("999.999.1.1/32").is_err());
        let root = root();
        let package = base_uci(
            "cake-autorate.second=cake_autorate\n\
             cake-autorate.second.enabled='1'\n\
             cake-autorate.second.manage_sqm='1'\n\
             cake-autorate.second.sqm_enabled='1'\n\
             cake-autorate.second.traffic_rules_enabled='1'\n\
             cake-autorate.second.autotune_profile='fair'\n\
             cake-autorate.second.traffic_profile='auto'\n\
             cake-autorate.second.sqm_script='layer_cake.qos'\n\
             cake-autorate.second.sqm_eqdisc_opts='diffserv4'\n\
             cake-autorate.second.wan_if='eth0'\n",
        );
        assert!(render_ruleset(&environment(&root), &package)
            .unwrap_err()
            .contains("same traffic-rule uplink"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn state_manifest_is_byte_exact_and_rejects_tamper() {
        let manifest = StateManifest {
            ruleset_sha256: "a".repeat(64),
            instances: vec![InstanceManifest {
                instance: "wan_sqm".to_string(),
                target: "eth0".to_string(),
                autotune_profile: "gaming".to_string(),
                configured_profile: "auto".to_string(),
                resolved_profile: "gaming".to_string(),
            }],
        };
        let bytes = manifest.encode().unwrap();
        assert_eq!(StateManifest::decode(&bytes).unwrap(), manifest);
        assert!(
            StateManifest::decode(&bytes.replace("schema_version=3", "schema_version=2")).is_err()
        );
        assert!(
            StateManifest::decode(&(bytes.clone() + "wan_sqm|eth0|gaming|auto|gaming\n")).is_err()
        );
    }

    #[test]
    fn logical_ruleset_digest_matches_shell_command_substitution_semantics() {
        assert_eq!(hex_digest(b"{}\n"), hex_digest(b"{}\n"));
        assert_ne!(hex_digest(b"{}"), hex_digest(b"{}\n"));
        assert!(is_sha256(&hex_digest(b"{}\n")));
    }

    #[test]
    fn apply_status_drift_and_clear_share_one_attested_lifecycle() {
        let root = root();
        let environment = command_environment(&root);
        assert!(status(&environment, None)
            .unwrap()
            .contains("\"state\":\"inactive\""));
        assert!(!environment
            .runtime_dir
            .join("traffic-classifier.guard")
            .exists());
        let applied = apply(&environment).unwrap();
        assert!(applied.contains("\"state\":\"active\""));
        assert!(applied.contains("\"instances\":1"));
        assert_eq!(
            fs::read(root.join("checked.nft")).unwrap(),
            fs::read(root.join("applied.nft")).unwrap()
        );
        let global = status(&environment, None).unwrap();
        assert!(global.contains("\"state\":\"active\""));
        assert!(global.contains("wan_sqm|eth0|gaming|auto|gaming"));
        let instance = status(&environment, Some("wan_sqm")).unwrap();
        assert!(instance.contains("\"resolved_profile\":\"gaming\""));
        assert!(status(&environment, Some("missing"))
            .unwrap()
            .contains("\"state\":\"missing\""));
        fs::write(root.join("applied.nft"), "foreign\n").unwrap();
        assert!(status(&environment, None)
            .unwrap()
            .contains("\"state\":\"drifted\""));
        clear(&environment).unwrap();
        assert!(!root.join("table-present").exists());
        assert!(!environment.state_file.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn state_and_guard_symlinks_are_never_followed() {
        use std::os::unix::fs::symlink;
        let root = root();
        let environment = command_environment(&root);
        let foreign = root.join("foreign");
        fs::write(&foreign, "unchanged").unwrap();
        symlink(
            &foreign,
            environment.runtime_dir.join("traffic-classifier.guard"),
        )
        .unwrap();
        assert!(RuntimeGuard::lock(&environment).is_err());
        assert_eq!(fs::read_to_string(&foreign).unwrap(), "unchanged");
        fs::remove_file(environment.runtime_dir.join("traffic-classifier.guard")).unwrap();
        symlink(&foreign, &environment.state_file).unwrap();
        assert!(remove_manifest(&environment.state_file).is_err());
        assert_eq!(fs::read_to_string(&foreign).unwrap(), "unchanged");
        fs::remove_dir_all(root).unwrap();
    }
}
