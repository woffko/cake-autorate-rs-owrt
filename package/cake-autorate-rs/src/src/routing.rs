use std::env;
use std::io;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UplinkState {
    Active,
    Standby,
    Offline,
    Learning,
}

impl UplinkState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "ACTIVE",
            Self::Standby => "STANDBY",
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
    online_since: Option<Instant>,
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
            online_since: None,
            learned: false,
            learning_samples: 0,
            active_route: false,
            reason: "route not checked".to_string(),
        }
    }

    pub fn observe(
        &mut self,
        snapshot: Result<&RouteSnapshot, &str>,
        now: Instant,
        stability: Duration,
    ) -> UplinkTransition {
        let previous_state = self.state;
        let mut identity_changed = false;

        let snapshot = match snapshot {
            Ok(snapshot) if snapshot.online => snapshot,
            Ok(snapshot) => {
                self.state = UplinkState::Offline;
                self.online_since = None;
                self.learned = false;
                self.learning_samples = 0;
                self.reason = if snapshot.reason.is_empty() {
                    format!("member {} is offline", snapshot.identity.member)
                } else {
                    snapshot.reason.clone()
                };
                return self.transition(previous_state, false, false);
            }
            Err(error) => {
                self.state = UplinkState::Offline;
                self.online_since = None;
                self.learned = false;
                self.learning_samples = 0;
                self.reason = error.to_string();
                return self.transition(previous_state, false, false);
            }
        };

        let identity = snapshot.stable_key();
        if self.identity.as_deref() != Some(&identity) {
            self.identity = Some(identity);
            self.online_since = Some(now);
            self.learned = false;
            self.learning_samples = 0;
            identity_changed = true;
        } else if previous_state == UplinkState::Offline || self.online_since.is_none() {
            self.online_since = Some(now);
            self.learned = false;
            self.learning_samples = 0;
        }
        self.active_route = snapshot.active;

        let stable = self
            .online_since
            .map(|since| now.saturating_duration_since(since) >= stability)
            .unwrap_or(false);
        if !stable || !self.learned {
            self.state = UplinkState::Learning;
            self.reason = if stable {
                "learning latency baseline".to_string()
            } else {
                "waiting for route stability".to_string()
            };
        } else {
            self.state = if snapshot.active {
                UplinkState::Active
            } else {
                UplinkState::Standby
            };
            self.reason = snapshot.reason.clone();
        }

        self.transition(previous_state, identity_changed, stable)
    }

    pub fn record_learning_sample(&mut self, required_samples: usize) -> bool {
        if self.state != UplinkState::Learning {
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

    pub fn reason(&self) -> &str {
        &self.reason
    }

    fn transition(
        &self,
        previous_state: UplinkState,
        identity_changed: bool,
        probes_allowed: bool,
    ) -> UplinkTransition {
        UplinkTransition {
            state: self.state,
            reason: self.reason.clone(),
            identity_changed,
            became_offline: self.state == UplinkState::Offline
                && previous_state != UplinkState::Offline,
            reset_learning: identity_changed
                || (self.state == UplinkState::Offline && previous_state != UplinkState::Offline)
                || (previous_state == UplinkState::Offline && self.state != UplinkState::Offline),
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
        RouteMode::Mwan3 => inspect_mwan3(spec),
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
    let source_ip = interface_source_ip(&spec.expected_device).unwrap_or_default();
    let default_device = default_route_device().unwrap_or_default();
    let active = device_online && default_device == spec.expected_device;
    let online = active;
    let reason = if !device_online {
        format!("interface {} is unavailable", spec.expected_device)
    } else if !active {
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

fn inspect_mwan3(spec: &RouteSpec) -> Result<RouteSnapshot, String> {
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
    let member_status = json_string_value(&mwan_json, "status").unwrap_or_default();
    let running = json_bool_value(&mwan_json, "running").unwrap_or(false);
    let member_up = json_bool_value(&mwan_json, "up").unwrap_or(false);
    let enabled = json_bool_value(&mwan_json, "enabled").unwrap_or(false);

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
    let network_up = json_bool_value(&network_json, "up").unwrap_or(false);
    let network_device = json_string_value(&network_json, "l3_device")
        .or_else(|| json_string_value(&network_json, "device"))
        .unwrap_or_default();

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
    let source_ip = env_value(&environment, "SRCIP").unwrap_or_default();
    let wrapper_mask = env_value(&environment, "FWMARK").unwrap_or_default();
    let (fwmark, table) =
        routing_for_device(&device).unwrap_or_else(|| (wrapper_mask, String::new()));

    let device_matches = device == spec.expected_device;
    let online = enabled
        && running
        && member_up
        && network_up
        && member_status == "online"
        && device_matches;
    let default_device = default_route_device().unwrap_or_default();
    let default_policy = mwan3_default_policy();
    let policy_percent = default_policy
        .as_deref()
        .and_then(|policy| json_policy_member_percent(&mwan_json, policy, &spec.member));
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
        match (default_policy.as_deref(), policy_percent) {
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

fn mwan3_default_policy() -> Option<String> {
    let output = run_output("uci", &["-q", "show", "mwan3"]).ok()?;
    if !output.status.success() {
        return None;
    }
    parse_mwan3_default_policy(&String::from_utf8_lossy(&output.stdout))
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

fn default_route_device() -> Option<String> {
    let output = run_output("ip", &["-4", "route", "show", "default"]).ok()?;
    if !output.status.success() {
        return None;
    }
    parse_default_route_device(&String::from_utf8_lossy(&output.stdout))
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

fn interface_source_ip(device: &str) -> Option<String> {
    let output = run_output(
        "ip",
        &["-4", "-o", "addr", "show", "dev", device, "scope", "global"],
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.split_whitespace()
        .skip_while(|word| *word != "inet")
        .nth(1)
        .and_then(|value| value.split('/').next())
        .map(str::to_string)
}

fn routing_for_device(device: &str) -> Option<(String, String)> {
    if device.is_empty() {
        return None;
    }
    let output = run_output("ip", &["-4", "rule", "show"]).ok()?;
    if !output.status.success() {
        return None;
    }
    parse_routing_for_device(&String::from_utf8_lossy(&output.stdout), device)
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
    fn parses_member_metadata() {
        let status = r#"{"status":"online","running":true,"up":true}"#;
        assert_eq!(
            json_string_value(status, "status").as_deref(),
            Some("online")
        );
        assert_eq!(json_bool_value(status, "running"), Some(true));
        assert_eq!(
            env_value("DEVICE=eth0\nSRCIP=10.0.100.101\n", "DEVICE").as_deref(),
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
    fn lifecycle_waits_for_stability_and_learning() {
        let start = Instant::now();
        let route = snapshot(false, "10.0.100.101");
        let mut lifecycle = UplinkLifecycle::new();
        let first = lifecycle.observe(Ok(&route), start, Duration::from_secs(5));
        assert_eq!(first.state, UplinkState::Learning);
        assert!(!first.probes_allowed);
        let stable = lifecycle.observe(
            Ok(&route),
            start + Duration::from_secs(5),
            Duration::from_secs(5),
        );
        assert!(stable.probes_allowed);
        assert!(!lifecycle.record_learning_sample(3));
        assert!(!lifecycle.record_learning_sample(3));
        assert!(lifecycle.record_learning_sample(3));
        assert_eq!(lifecycle.state(), UplinkState::Standby);
    }

    #[test]
    fn lifecycle_resets_after_ip_change_and_offline_recovery() {
        let start = Instant::now();
        let first_route = snapshot(true, "84.1.1.1");
        let second_route = snapshot(true, "84.1.1.2");
        let mut lifecycle = UplinkLifecycle::new();
        lifecycle.observe(Ok(&first_route), start, Duration::ZERO);
        lifecycle.record_learning_sample(1);
        assert_eq!(lifecycle.state(), UplinkState::Active);

        let changed = lifecycle.observe(
            Ok(&second_route),
            start + Duration::from_secs(1),
            Duration::ZERO,
        );
        assert!(changed.identity_changed);
        assert_eq!(changed.state, UplinkState::Learning);

        let offline_route = RouteSnapshot {
            online: false,
            reason: "member offline".to_string(),
            ..second_route.clone()
        };
        let offline = lifecycle.observe(
            Ok(&offline_route),
            start + Duration::from_secs(2),
            Duration::ZERO,
        );
        assert!(offline.became_offline);
        assert!(offline.reset_learning);
        assert_eq!(offline.state, UplinkState::Offline);
        let recovered = lifecycle.observe(
            Ok(&second_route),
            start + Duration::from_secs(3),
            Duration::ZERO,
        );
        assert!(!recovered.identity_changed);
        assert!(recovered.reset_learning);
        assert_eq!(recovered.state, UplinkState::Learning);
    }
}
