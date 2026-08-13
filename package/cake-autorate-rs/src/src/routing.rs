use std::env;
use std::io;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteMode {
    Main,
    Mwan3,
}

impl RouteMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Mwan3 => "mwan3",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RouteSpec {
    pub configured_mode: String,
    pub member: String,
    pub expected_device: String,
}

impl RouteSpec {
    pub fn new(configured_mode: &str, member: &str, expected_device: &str) -> Self {
        Self {
            configured_mode: configured_mode.to_string(),
            member: member.to_string(),
            expected_device: expected_device.to_string(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
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
    pub mode: String,
    pub member: String,
    pub device: String,
    pub source_ip: String,
    pub fwmark: String,
    pub table: String,
}

impl RouteIdentity {
    pub fn stable_key(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}",
            self.mode, self.member, self.device, self.source_ip, self.fwmark, self.table
        )
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
}

impl RouteInspector {
    pub fn new(spec: RouteSpec) -> Self {
        Self { spec }
    }

    pub fn inspect(&mut self) -> Result<RouteSnapshot, String> {
        self.inspect_fresh()
    }

    pub fn inspect_fresh(&mut self) -> Result<RouteSnapshot, String> {
        match self.spec.effective_mode()? {
            RouteMode::Main => inspect_main(&self.spec),
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
        RouteMode::Mwan3 => {
            let default_policy = mwan3_default_policy()?;
            inspect_mwan3_with_policy(spec, default_policy.as_deref())
        }
    }
}

pub fn external_ipv4(spec: &RouteSpec, timeout_s: u64) -> Result<String, String> {
    let mut command = routed_command(spec, "", "uclient-fetch")?;
    let output = command
        .arg("-q")
        .arg("-4")
        .arg("-T")
        .arg(timeout_s.clamp(1, 30).to_string())
        .arg("-O")
        .arg("-")
        .arg("https://api.ipify.org")
        .output()
        .map_err(|error| format!("failed to query external IP: {error}"))?;
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
        return Ok(RouteSnapshot {
            identity: RouteIdentity {
                mode: RouteMode::Main.as_str().to_string(),
                member: String::new(),
                device: spec.expected_device.clone(),
                source_ip,
                fwmark: String::new(),
                table: "main".to_string(),
            },
            online: false,
            active: false,
            member_status: "connecting".to_string(),
            reason: format!(
                "interface {} has no IPv4 source address; waiting for route identity",
                spec.expected_device
            ),
        });
    }

    let default_device = default_route_device()?.unwrap_or_default();
    let active = device_online && default_device == spec.expected_device;
    let online = active;
    let reason = if !active {
        format!("main default route uses {default_device}")
    } else {
        String::new()
    };

    Ok(RouteSnapshot {
        identity: RouteIdentity {
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
    let device = env_value(&environment, "DEVICE").unwrap_or(network_device);
    if device.is_empty() {
        return Err(format!(
            "mwan3 route {} has no resolved device",
            spec.member
        ));
    }
    let source_ip = env_value(&environment, "SRCIP")
        .filter(|value| valid_ipv4(value))
        .ok_or_else(|| format!("mwan3 route {} has no valid IPv4 source", spec.member))?;
    let wrapper_mask = env_value(&environment, "FWMARK").unwrap_or_default();
    let (fwmark, table) =
        routing_for_device(&device)?.unwrap_or_else(|| (wrapper_mask, String::new()));

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
            mode: RouteMode::Mwan3.as_str().to_string(),
            member: spec.member.clone(),
            device,
            source_ip,
            fwmark,
            table,
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
    environment
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
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

fn parse_proc_default_route_device(routes: &str) -> Option<String> {
    routes.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.get(1) == Some(&"00000000") {
            fields.first().map(|value| (*value).to_string())
        } else {
            None
        }
    })
}

fn parse_default_route_device(routes: &str) -> Option<String> {
    routes.lines().find_map(|line| {
        let words: Vec<&str> = line.split_whitespace().collect();
        words
            .iter()
            .position(|word| *word == "dev")
            .and_then(|index| words.get(index + 1))
            .map(|value| (*value).to_string())
    })
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

fn routing_for_device(device: &str) -> Result<Option<(String, String)>, String> {
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

fn parse_routing_for_device(rules: &str, device: &str) -> Option<(String, String)> {
    let table = rules.lines().find_map(|line| {
        let words: Vec<&str> = line.split_whitespace().collect();
        let rule_device = words
            .iter()
            .position(|word| *word == "iif")
            .and_then(|index| words.get(index + 1))?;
        if *rule_device != device {
            return None;
        }
        words
            .iter()
            .position(|word| *word == "lookup")
            .and_then(|index| words.get(index + 1))
            .map(|value| (*value).to_string())
    })?;

    let mark = rules.lines().find_map(|line| {
        let words: Vec<&str> = line.split_whitespace().collect();
        let lookup_table = words
            .iter()
            .position(|word| *word == "lookup")
            .and_then(|index| words.get(index + 1))?;
        if *lookup_table != table {
            return None;
        }
        words
            .iter()
            .position(|word| *word == "fwmark")
            .and_then(|index| words.get(index + 1))
            .map(|value| value.split('/').next().unwrap_or(value).to_string())
    })?;
    Some((mark, table))
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
            Some(("0x100".to_string(), "1".to_string()))
        );
    }

    #[test]
    fn validates_external_ipv4_without_accepting_trailing_data() {
        assert!(valid_ipv4("84.52.59.166"));
        assert!(!valid_ipv4("84.52.59.166\nwrong"));
        assert!(!valid_ipv4("999.1.1.1"));
    }

    fn snapshot(active: bool, ip: &str) -> RouteSnapshot {
        RouteSnapshot {
            identity: RouteIdentity {
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
