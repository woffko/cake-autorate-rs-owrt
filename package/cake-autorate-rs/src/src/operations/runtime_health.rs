//! Read-only reconciliation of configured CAKE Autorate intent with live state.
//!
//! This replaces the historical LuCI shell helper.  It deliberately performs
//! no repair, service action, UCI write, or qdisc mutation: Status must expose
//! partial, waiting, conflicting, and orphaned states instead of hiding them.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const SCHEMA_VERSION: u8 = 1;
const MAX_COMMAND_BYTES: usize = 1024 * 1024;
const MAX_STATUS_BYTES: u64 = 1024 * 1024;
const CONTROLLER_FRESH_SECONDS: u64 = 90;
const CONTROLLER_FUTURE_SKEW_SECONDS: u64 = 10;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct UciSection {
    pub(crate) section_type: String,
    pub(crate) options: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct UciPackage {
    pub(crate) sections: BTreeMap<String, UciSection>,
}

impl UciPackage {
    pub(crate) fn parse(package: &str, input: &str) -> Result<Self, String> {
        let prefix = format!("{package}.");
        let mut result = Self::default();
        for line in input.lines().filter(|line| !line.is_empty()) {
            if line.chars().any(char::is_control) {
                return Err(format!(
                    "UCI package {package} contains a control character"
                ));
            }
            let (key, raw) = line
                .split_once('=')
                .ok_or_else(|| format!("UCI package {package} contains a malformed line"))?;
            let path = key
                .strip_prefix(&prefix)
                .ok_or_else(|| format!("UCI line is outside package {package}"))?;
            let mut parts = path.split('.');
            let section = parts.next().unwrap_or_default();
            let option = parts.next();
            if !safe_name(section) || parts.next().is_some() {
                return Err(format!("UCI package {package} contains an unsafe key"));
            }
            let values = crate::parse_uci_values(raw);
            let value = values.first().cloned().unwrap_or_default();
            let entry = result.sections.entry(section.to_string()).or_default();
            if let Some(option) = option {
                if !safe_name(option) {
                    return Err(format!("UCI package {package} contains an unsafe option"));
                }
                if entry.options.insert(option.to_string(), value).is_some() {
                    return Err(format!("UCI package {package} contains a duplicate option"));
                }
            } else {
                if value.is_empty() || entry.section_type.len() > 0 {
                    return Err(format!(
                        "UCI package {package} contains a duplicate section"
                    ));
                }
                entry.section_type = value;
            }
        }
        if result
            .sections
            .values()
            .any(|section| section.section_type.is_empty())
        {
            return Err(format!(
                "UCI package {package} has options without a section"
            ));
        }
        Ok(result)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CakeInfo {
    count: usize,
    kind: String,
    mode: String,
    rate_kbps: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ControllerStatus {
    state: String,
    reason: String,
    fresh: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ClassifierStatus {
    state: String,
    attested_instances: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Environment {
    uci: String,
    tc: String,
    pgrep: String,
    ubus: String,
    daemon: String,
    classifier: String,
    sys_class_net: PathBuf,
    runtime_root: PathBuf,
}

impl Environment {
    fn live() -> Self {
        Self {
            uci: env_string("CAKE_AUTORATE_UCI_BIN", "uci"),
            tc: env_string("CAKE_AUTORATE_TC_BIN", "tc"),
            pgrep: env_string("CAKE_AUTORATE_PGREP_BIN", "pgrep"),
            ubus: env_string("CAKE_AUTORATE_UBUS_BIN", "ubus"),
            daemon: env_string("CAKE_AUTORATE_DAEMON_BIN", "/usr/sbin/cake-autorated"),
            classifier: env_string(
                "CAKE_AUTORATE_TRAFFIC_CLASSIFIER",
                "/usr/sbin/cake-autorated",
            ),
            sys_class_net: env_path("CAKE_AUTORATE_SYS_CLASS_NET", "/sys/class/net"),
            runtime_root: env_path("CAKE_AUTORATE_RUNTIME_ROOT", "/var/run/cake-autorate"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InstanceHealth {
    overall_state: String,
    autorate_state: String,
    autorate_processes: usize,
    controller_state: String,
    controller_status_fresh: bool,
    controller_reason: String,
    sqm_config_state: String,
    sqm_section: String,
    sqm_owner: String,
    sqm_target: String,
    sqm_direction_mode: String,
    target_interface: String,
    target_state: String,
    ul_interface: String,
    dl_interface: String,
    cake_ul_state: String,
    cake_ul_kind: String,
    cake_ul_mode: String,
    cake_ul_count: usize,
    cake_ul_rate_kbps: u64,
    cake_dl_state: String,
    cake_dl_kind: String,
    cake_dl_mode: String,
    cake_dl_count: usize,
    cake_dl_rate_kbps: u64,
    classifier_state: String,
    classifier_profile: String,
    classifier_global_state: String,
    autotune_profile: String,
    traffic_profile_mode: String,
    traffic_profile_resolved: String,
    access_medium: String,
    access_medium_source: String,
    access_medium_confidence_percent: u64,
    capacity_learning_policy: String,
    classifier_target: String,
    classifier_applied_profile: String,
    classifier_applied_autotune_profile: String,
    classifier_applied_configured_profile: String,
    classifier_applied_resolved_profile: String,
    ifb_state: String,
    ingress_state: String,
    apply_state: String,
    operation_state: String,
    issues: String,
    observed_at: u64,
}

impl InstanceHealth {
    fn encode_json(&self) -> String {
        use super::json_wire::{bool_json, json_escape};
        format!(
            concat!(
                "{{\"overall_state\":\"{}\",",
                "\"autorate_state\":\"{}\",\"autorate_processes\":{},",
                "\"controller_state\":\"{}\",\"controller_status_fresh\":{},\"controller_reason\":\"{}\",",
                "\"sqm_config_state\":\"{}\",\"sqm_section\":\"{}\",\"sqm_owner\":\"{}\",\"sqm_target\":\"{}\",",
                "\"sqm_direction_mode\":\"{}\",",
                "\"target_interface\":\"{}\",\"target_state\":\"{}\",\"ul_interface\":\"{}\",\"dl_interface\":\"{}\",",
                "\"cake_ul_state\":\"{}\",\"cake_ul_kind\":\"{}\",\"cake_ul_mode\":\"{}\",\"cake_ul_count\":{},\"cake_ul_rate_kbps\":{},",
                "\"cake_dl_state\":\"{}\",\"cake_dl_kind\":\"{}\",\"cake_dl_mode\":\"{}\",\"cake_dl_count\":{},\"cake_dl_rate_kbps\":{},",
                "\"classifier_state\":\"{}\",\"classifier_profile\":\"{}\",\"classifier_global_state\":\"{}\",",
                "\"autotune_profile\":\"{}\",\"traffic_profile_mode\":\"{}\",\"traffic_profile_resolved\":\"{}\",",
                "\"access_medium\":\"{}\",\"access_medium_source\":\"{}\",\"access_medium_confidence_percent\":{},",
                "\"capacity_learning_policy\":\"{}\",",
                "\"classifier_target\":\"{}\",\"classifier_applied_profile\":\"{}\",",
                "\"classifier_applied_autotune_profile\":\"{}\",\"classifier_applied_configured_profile\":\"{}\",\"classifier_applied_resolved_profile\":\"{}\",",
                "\"ifb_state\":\"{}\",\"ingress_state\":\"{}\",\"apply_state\":\"{}\",\"operation_state\":\"{}\",",
                "\"issues\":\"{}\",\"observed_at\":{}}}"
            ),
            json_escape(&self.overall_state),
            json_escape(&self.autorate_state),
            self.autorate_processes,
            json_escape(&self.controller_state),
            bool_json(self.controller_status_fresh),
            json_escape(&self.controller_reason),
            json_escape(&self.sqm_config_state),
            json_escape(&self.sqm_section),
            json_escape(&self.sqm_owner),
            json_escape(&self.sqm_target),
            json_escape(&self.sqm_direction_mode),
            json_escape(&self.target_interface),
            json_escape(&self.target_state),
            json_escape(&self.ul_interface),
            json_escape(&self.dl_interface),
            json_escape(&self.cake_ul_state),
            json_escape(&self.cake_ul_kind),
            json_escape(&self.cake_ul_mode),
            self.cake_ul_count,
            self.cake_ul_rate_kbps,
            json_escape(&self.cake_dl_state),
            json_escape(&self.cake_dl_kind),
            json_escape(&self.cake_dl_mode),
            self.cake_dl_count,
            self.cake_dl_rate_kbps,
            json_escape(&self.classifier_state),
            json_escape(&self.classifier_profile),
            json_escape(&self.classifier_global_state),
            json_escape(&self.autotune_profile),
            json_escape(&self.traffic_profile_mode),
            json_escape(&self.traffic_profile_resolved),
            json_escape(&self.access_medium),
            json_escape(&self.access_medium_source),
            self.access_medium_confidence_percent,
            json_escape(&self.capacity_learning_policy),
            json_escape(&self.classifier_target),
            json_escape(&self.classifier_applied_profile),
            json_escape(&self.classifier_applied_autotune_profile),
            json_escape(&self.classifier_applied_configured_profile),
            json_escape(&self.classifier_applied_resolved_profile),
            json_escape(&self.ifb_state),
            json_escape(&self.ingress_state),
            json_escape(&self.apply_state),
            json_escape(&self.operation_state),
            json_escape(&self.issues),
            self.observed_at,
        )
    }
}

fn env_string(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_string())
}

fn env_path(name: &str, default: &str) -> PathBuf {
    env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

pub(crate) fn safe_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

pub(crate) fn safe_interface(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.:@-".contains(&byte))
}

fn run_bounded(command: &str, args: &[&str]) -> Result<(bool, String), String> {
    let mut child = Command::new(command)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("failed to execute {command}: {error}"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{command} stdout is unavailable"))?;
    let mut bytes = Vec::new();
    stdout
        .by_ref()
        .take((MAX_COMMAND_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("failed to read {command}: {error}"))?;
    if bytes.len() > MAX_COMMAND_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("{command} output exceeds its size bound"));
    }
    let status = child
        .wait()
        .map_err(|error| format!("failed to reap {command}: {error}"))?;
    let text =
        String::from_utf8(bytes).map_err(|_| format!("{command} output is not valid UTF-8"))?;
    Ok((status.success(), text))
}

fn uci_package(environment: &Environment, package: &str) -> Result<UciPackage, String> {
    let (success, output) = run_bounded(&environment.uci, &["-q", "show", package])?;
    if !success {
        return Err(format!("UCI package {package} is unavailable"));
    }
    UciPackage::parse(package, &output)
}

fn option(section: &UciSection, key: &str) -> String {
    section.options.get(key).cloned().unwrap_or_default()
}

fn bool_option(section: &UciSection, key: &str, default: bool) -> bool {
    match section.options.get(key).map(String::as_str) {
        Some("1") => true,
        Some("0") => false,
        _ => default,
    }
}

fn normalize_autotune_profile(value: &str) -> &'static str {
    match value {
        "gaming" | "gaming_extreme" | "gaming-extreme" | "extreme_gaming" => "gaming",
        "best-overall" | "balanced" | "" | "best_overall" => "best_overall",
        "variable-link" | "variable" | "variable_link" => "variable_link",
        "fair" => "fair",
        _ => "unknown",
    }
}

fn access_medium(value: &str) -> String {
    match value {
        "cellular" | "leo_satellite" | "geo_satellite" | "fixed_wireless" | "shared_wired" => {
            value.to_string()
        }
        _ => "unknown".to_string(),
    }
}

fn access_source(value: &str) -> String {
    match value {
        "user_selected" | "network_protocol" | "device_type" | "interface_name"
        | "auto_inconclusive" | "legacy_default" => value.to_string(),
        _ => "legacy_default".to_string(),
    }
}

fn capacity_policy(section: &UciSection) -> String {
    let configured = option(section, "capacity_learning_policy");
    if matches!(
        configured.as_str(),
        "verified_only" | "passive_bounded" | "scheduled_active" | "fixed_cap"
    ) {
        configured
    } else if bool_option(section, "scheduled_autotune_enabled", false) {
        "scheduled_active".to_string()
    } else if bool_option(section, "adaptive_ceiling_enabled", false) {
        "passive_bounded".to_string()
    } else {
        "verified_only".to_string()
    }
}

fn resolve_interface(environment: &Environment, name: &str) -> String {
    if !safe_interface(name) {
        return name.to_string();
    }
    if environment.sys_class_net.join(name).exists() {
        return name.to_string();
    }
    let object = format!("network.interface.{name}");
    if let Ok((true, status)) = run_bounded(&environment.ubus, &["call", &object, "status"]) {
        if let Some(device) =
            json_string_value(&status, "l3_device").or_else(|| json_string_value(&status, "device"))
        {
            if safe_interface(&device) && environment.sys_class_net.join(&device).exists() {
                return device;
            }
        }
    }
    name.to_string()
}

fn json_key_tail<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let marker = format!("\"{key}\"");
    let mut remaining = json;
    while let Some(offset) = remaining.find(&marker) {
        let after_marker = &remaining[offset + marker.len()..];
        if let Some(value) = after_marker.trim_start().strip_prefix(':') {
            return Some(value.trim_start());
        }
        remaining = after_marker;
    }
    None
}

pub(crate) fn json_string_value(json: &str, key: &str) -> Option<String> {
    let mut chars = json_key_tail(json, key)?.strip_prefix('"')?.chars();
    let mut output = String::new();
    while let Some(character) = chars.next() {
        match character {
            '"' => return Some(output),
            '\\' => match chars.next()? {
                '"' => output.push('"'),
                '\\' => output.push('\\'),
                '/' => output.push('/'),
                'b' => output.push('\u{0008}'),
                'f' => output.push('\u{000c}'),
                'n' => output.push('\n'),
                'r' => output.push('\r'),
                't' => output.push('\t'),
                _ => return None,
            },
            character if character.is_control() => return None,
            character => output.push(character),
        }
        if output.len() > 4096 {
            return None;
        }
    }
    None
}

fn json_u64_value(json: &str, key: &str) -> Option<u64> {
    let tail = json_key_tail(json, key)?;
    let digits = tail
        .bytes()
        .take_while(u8::is_ascii_digit)
        .collect::<Vec<_>>();
    (!digits.is_empty())
        .then(|| std::str::from_utf8(&digits).ok()?.parse::<u64>().ok())
        .flatten()
}

#[cfg(feature = "calibration")]
pub(crate) fn json_nonnegative_f64_value(json: &str, key: &str) -> Option<f64> {
    let tail = json_key_tail(json, key)?;
    let token = tail
        .bytes()
        .take_while(|byte| {
            byte.is_ascii_digit() || matches!(*byte, b'.' | b'e' | b'E' | b'+' | b'-')
        })
        .collect::<Vec<_>>();
    if token.is_empty() {
        return None;
    }
    let value = std::str::from_utf8(&token).ok()?.parse::<f64>().ok()?;
    (value.is_finite() && value >= 0.0).then_some(value)
}

fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn controller_status(
    environment: &Environment,
    instance: &str,
    process_count: usize,
    now: u64,
) -> ControllerStatus {
    let mut result = ControllerStatus {
        state: "UNKNOWN".to_string(),
        ..ControllerStatus::default()
    };
    if process_count != 1 {
        return result;
    }
    let path = environment.runtime_root.join(instance).join("status.json");
    let Ok(metadata) = fs::symlink_metadata(&path) else {
        return result;
    };
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_STATUS_BYTES
    {
        return result;
    }
    let Ok(json) = fs::read_to_string(path) else {
        return result;
    };
    let state = json_string_value(&json, "state").unwrap_or_default();
    if !matches!(
        state.as_str(),
        "WAITING_LINK"
            | "WAITING_SQM"
            | "WAITING_EXTERNAL_SQM"
            | "WAITING_OPERATION"
            | "RECOVERING"
            | "RUNNING"
            | "IDLE"
            | "STALL"
            | "LEARNING"
            | "ACTIVE"
            | "STANDBY"
            | "OFFLINE"
            | "ERROR"
            | "STOPPING"
    ) {
        return result;
    }
    result.state = state;
    result.reason = json_string_value(&json, "sqm_runtime_reason")
        .or_else(|| json_string_value(&json, "uplink_reason"))
        .unwrap_or_default();
    result.fresh = json_u64_value(&json, "updated_at").is_some_and(|updated| {
        updated <= now.saturating_add(CONTROLLER_FUTURE_SKEW_SECONDS)
            && now.saturating_sub(updated) <= CONTROLLER_FRESH_SECONDS
    });
    result
}

fn process_count(environment: &Environment, instance: &str) -> usize {
    let pattern = format!("^{} --instance {}$", environment.daemon, instance);
    let Ok((_success, output)) = run_bounded(&environment.pgrep, &["-f", &pattern]) else {
        return 0;
    };
    output
        .lines()
        .filter(|line| !line.is_empty() && line.bytes().all(|byte| byte.is_ascii_digit()))
        .count()
}

fn cake_info(environment: &Environment, interface: &str) -> CakeInfo {
    if !safe_interface(interface) || !environment.sys_class_net.join(interface).exists() {
        return CakeInfo::default();
    }
    let Ok((true, output)) = run_bounded(&environment.tc, &["qdisc", "show", "dev", interface])
    else {
        return CakeInfo::default();
    };
    let mut result = CakeInfo::default();
    for line in output.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.first() != Some(&"qdisc")
            || !matches!(fields.get(1), Some(&"cake") | Some(&"cake_mq"))
            || !fields.contains(&"root")
        {
            continue;
        }
        result.count = result.count.saturating_add(1);
        result.kind = fields[1].to_string();
        result.rate_kbps = fields
            .windows(2)
            .find_map(|pair| (pair[0] == "bandwidth").then_some(pair[1]))
            .and_then(|value| crate::parse_tc_bandwidth_kbps(value).ok())
            .unwrap_or(0);
        result.mode = fields
            .iter()
            .find(|value| {
                matches!(
                    **value,
                    "besteffort" | "diffserv3" | "diffserv4" | "diffserv8" | "precedence"
                )
            })
            .copied()
            .unwrap_or_default()
            .to_string();
    }
    result
}

fn target_topology(environment: &Environment, target: &str, ifb: &str) -> (bool, bool) {
    if !safe_interface(target)
        || !safe_interface(ifb)
        || !environment.sys_class_net.join(target).exists()
    {
        return (false, false);
    }
    let ingress = run_bounded(&environment.tc, &["qdisc", "show", "dev", target])
        .ok()
        .filter(|(success, _)| *success)
        .is_some_and(|(_, output)| {
            output.lines().any(|line| {
                let fields = line.split_whitespace().collect::<Vec<_>>();
                fields.first() == Some(&"qdisc")
                    && matches!(fields.get(1), Some(&"ingress") | Some(&"clsact"))
            })
        });
    let redirect = run_bounded(
        &environment.tc,
        &["filter", "show", "dev", target, "ingress"],
    )
    .ok()
    .filter(|(success, _)| *success)
    .is_some_and(|(_, output)| {
        crate::ingress_redirect_targets(&output)
            .iter()
            .any(|value| value == ifb)
    });
    (ingress, redirect)
}

fn classifier_status(environment: &Environment) -> ClassifierStatus {
    let Ok((true, output)) =
        run_bounded(&environment.classifier, &["--traffic-classifier", "status"])
    else {
        return ClassifierStatus {
            state: "UNAVAILABLE".to_string(),
            ..ClassifierStatus::default()
        };
    };
    let state = match json_string_value(&output, "state").as_deref() {
        Some("active") => "ACTIVE",
        Some("inactive") => "INACTIVE",
        Some("drifted") => "DRIFTED",
        Some("untracked") => "UNTRACKED",
        Some("unavailable") => "UNAVAILABLE",
        _ => "ERROR",
    };
    ClassifierStatus {
        state: state.to_string(),
        attested_instances: json_string_value(&output, "attested_instances").unwrap_or_default(),
    }
}

fn owned_runtime_state(manage_sqm: bool, expected: bool, count: usize) -> &'static str {
    if !manage_sqm {
        if count > 0 {
            "EXTERNAL"
        } else {
            "ABSENT"
        }
    } else if expected {
        match count {
            0 => "MISSING",
            1 => "ACTIVE",
            _ => "DUPLICATE",
        }
    } else if count > 0 {
        "ORPHANED"
    } else {
        "ABSENT"
    }
}

fn add_issue(issues: &mut Vec<String>, issue: impl Into<String>) {
    issues.push(issue.into());
}

fn classify_instance(
    environment: &Environment,
    instance: &str,
    config: &UciSection,
    sqm: &UciPackage,
    classifier: &ClassifierStatus,
    now: u64,
) -> InstanceHealth {
    let mut issues = Vec::new();
    let enabled = bool_option(config, "enabled", false);
    let manage_sqm = bool_option(config, "manage_sqm", true);
    let sqm_enabled = bool_option(config, "sqm_enabled", enabled);
    let direction_mode = match option(config, "sqm_direction_mode").as_str() {
        "" | "both" => "both",
        "upload_only" => "upload_only",
        "download_only" => "download_only",
        "off" => "off",
        _ => {
            add_issue(&mut issues, "The SQM direction mode is invalid.");
            "invalid"
        }
    };
    let shape_download = matches!(direction_mode, "both" | "download_only");
    let shape_upload = matches!(direction_mode, "both" | "upload_only");
    let traffic_rules_enabled = bool_option(config, "traffic_rules_enabled", false);
    let autotune_profile = normalize_autotune_profile(&option(config, "autotune_profile"));
    let medium = access_medium(&option(config, "access_medium"));
    let medium_source = access_source(&option(config, "access_medium_source"));
    let medium_confidence = option(config, "access_medium_confidence_percent")
        .parse::<u64>()
        .ok()
        .filter(|value| *value <= 100)
        .unwrap_or(0);
    let learning_policy = capacity_policy(config);
    let mut profile_mode = option(config, "traffic_profile");
    if profile_mode.is_empty() {
        profile_mode = "auto".to_string();
    }
    let profile_resolved = match profile_mode.as_str() {
        "auto" if autotune_profile == "variable_link" => "best_overall".to_string(),
        "auto" => autotune_profile.to_string(),
        "gaming" | "best_overall" | "fair" | "custom" => profile_mode.clone(),
        _ => {
            profile_mode = "unknown".to_string();
            "unknown".to_string()
        }
    };

    let target_configured = ["wan_if", "sqm_interface", "ul_if"]
        .iter()
        .find_map(|key| config.options.get(*key).filter(|value| !value.is_empty()))
        .cloned()
        .unwrap_or_default();
    let target = resolve_interface(environment, &target_configured);
    let upload_configured = config
        .options
        .get("ul_if")
        .filter(|value| !value.is_empty())
        .cloned()
        .unwrap_or_else(|| target.clone());
    let upload = resolve_interface(environment, &upload_configured);
    let download = config
        .options
        .get("dl_if")
        .filter(|value| !value.is_empty())
        .cloned()
        .unwrap_or_else(|| format!("ifb4{target}"));
    let sqm_section = config
        .options
        .get("sqm_section")
        .filter(|value| !value.is_empty())
        .cloned()
        .unwrap_or_else(|| format!("cake_{instance}"));

    let daemon_count = process_count(environment, instance);
    let autorate_state = match (enabled, daemon_count) {
        (true, 0) => {
            add_issue(
                &mut issues,
                "Autorate is enabled but its process is absent.",
            );
            "STOPPED"
        }
        (true, 1) => "RUNNING",
        (true, _) => {
            add_issue(
                &mut issues,
                "More than one autorate process owns this instance.",
            );
            "DUPLICATE"
        }
        (false, 0) => "DISABLED",
        (false, _) => {
            add_issue(
                &mut issues,
                "Autorate is disabled but its process is still running.",
            );
            "UNEXPECTED"
        }
    };
    let controller = controller_status(environment, instance, daemon_count, now);

    let mut sqm_owner = String::new();
    let mut sqm_target = String::new();
    let sqm_config_state = if !manage_sqm {
        "UNMANAGED"
    } else if !safe_name(&sqm_section) || !sqm.sections.contains_key(&sqm_section) {
        if enabled {
            add_issue(&mut issues, "The managed SQM queue is missing.");
        }
        "MISSING"
    } else {
        let queue = &sqm.sections[&sqm_section];
        sqm_owner = option(queue, "_cake_autorate_managed");
        sqm_target = resolve_interface(environment, &option(queue, "interface"));
        let queue_enabled = bool_option(queue, "enabled", false);
        if sqm_owner != instance {
            add_issue(
                &mut issues,
                "The SQM queue ownership marker belongs to another instance.",
            );
            "CONFLICT"
        } else if target.is_empty() || sqm_target != target {
            add_issue(
                &mut issues,
                "The managed SQM queue targets a different interface.",
            );
            "CONFLICT"
        } else if queue_enabled {
            "ENABLED"
        } else {
            "DISABLED"
        }
    };
    let expected_runtime = enabled && manage_sqm && sqm_enabled && sqm_config_state == "ENABLED";
    if enabled != sqm_enabled && manage_sqm {
        add_issue(
            &mut issues,
            "Autorate and managed SQM enable flags disagree.",
        );
    }
    let target_present =
        safe_interface(&target) && environment.sys_class_net.join(&target).exists();
    let controller_transient = controller.fresh
        && matches!(
            controller.state.as_str(),
            "WAITING_LINK"
                | "WAITING_SQM"
                | "WAITING_EXTERNAL_SQM"
                | "WAITING_OPERATION"
                | "RECOVERING"
        );
    let runtime_link_wait = expected_runtime && autorate_state == "RUNNING" && !target_present;
    let suppress_runtime_issues = controller_transient || runtime_link_wait;

    let upload_cake = cake_info(environment, &upload);
    let download_cake = cake_info(environment, &download);
    let upload_expected = expected_runtime && shape_upload;
    let download_expected = expected_runtime && shape_download;
    let cake_ul_state = owned_runtime_state(manage_sqm, upload_expected, upload_cake.count);
    let cake_dl_state = owned_runtime_state(manage_sqm, download_expected, download_cake.count);
    let download_device_present = environment.sys_class_net.join(&download).exists();
    let ifb_state = if download_device_present {
        if !manage_sqm {
            "EXTERNAL"
        } else if download_expected {
            "PRESENT"
        } else if expected_runtime {
            "IDLE"
        } else {
            "ORPHANED"
        }
    } else if download_expected {
        "MISSING"
    } else {
        "ABSENT"
    };
    let (ingress_present, redirect_present) = target_topology(environment, &target, &download);
    let ingress_state = if !manage_sqm {
        if ingress_present || redirect_present {
            "EXTERNAL"
        } else {
            "ABSENT"
        }
    } else if download_expected {
        if ingress_present && redirect_present {
            "ACTIVE"
        } else {
            "MISSING"
        }
    } else if expected_runtime && !redirect_present {
        if ingress_present {
            "IDLE"
        } else {
            "ABSENT"
        }
    } else if ingress_present || redirect_present {
        "ORPHANED"
    } else {
        "ABSENT"
    };

    match cake_ul_state {
        "MISSING" if shape_upload && !suppress_runtime_issues => {
            add_issue(&mut issues, "Upload CAKE qdisc is missing.")
        }
        "DUPLICATE" => add_issue(
            &mut issues,
            "Multiple upload CAKE root qdiscs were detected.",
        ),
        "ORPHANED" => add_issue(
            &mut issues,
            "Upload CAKE still limits traffic although the instance is disabled.",
        ),
        _ => {}
    }
    match cake_dl_state {
        "MISSING" if shape_download && !suppress_runtime_issues => {
            add_issue(&mut issues, "Download CAKE qdisc is missing.")
        }
        "DUPLICATE" => add_issue(
            &mut issues,
            "Multiple download CAKE root qdiscs were detected.",
        ),
        "ORPHANED" => add_issue(
            &mut issues,
            "Download CAKE still limits traffic although the instance is disabled.",
        ),
        _ => {}
    }
    match ifb_state {
        "MISSING" if !suppress_runtime_issues => {
            add_issue(&mut issues, "The managed download IFB is missing.")
        }
        "ORPHANED" => add_issue(
            &mut issues,
            "The managed download IFB remains after SQM was disabled.",
        ),
        _ => {}
    }
    match ingress_state {
        "MISSING" if !suppress_runtime_issues => add_issue(
            &mut issues,
            "The ingress redirect to the managed IFB is missing.",
        ),
        "ORPHANED" => add_issue(
            &mut issues,
            "The ingress redirect remains after SQM was disabled.",
        ),
        _ => {}
    }

    let classifier_expected =
        enabled && manage_sqm && sqm_enabled && traffic_rules_enabled && shape_upload;
    let mut classifier_attestation = classifier.state.clone();
    let mut classifier_target = String::new();
    let mut classifier_applied_autotune = String::new();
    let mut classifier_applied_configured = String::new();
    let mut classifier_applied_resolved = String::new();
    if classifier.state == "ACTIVE" {
        classifier_attestation = "MISSING".to_string();
        for record in classifier.attested_instances.split(';') {
            let fields = record.split('|').collect::<Vec<_>>();
            if fields.len() == 5 && fields[0] == instance {
                classifier_attestation = "ACTIVE".to_string();
                classifier_target = fields[1].to_string();
                classifier_applied_autotune = fields[2].to_string();
                classifier_applied_configured = fields[3].to_string();
                classifier_applied_resolved = fields[4].to_string();
                break;
            }
        }
    }
    let classifier_state = if !classifier_expected {
        if classifier_attestation == "ACTIVE" {
            add_issue(
                &mut issues,
                "Traffic-priority rules remain loaded for this disabled instance.",
            );
            "ORPHANED"
        } else {
            "DISABLED"
        }
    } else if suppress_runtime_issues {
        "WAITING"
    } else if cake_ul_state == "ACTIVE" && upload_cake.mode != "diffserv4" {
        add_issue(&mut issues, "Traffic-priority rules require upload CAKE diffserv4; re-run Auto-Tune or update the SQM profile.");
        "INEFFECTIVE"
    } else if classifier_attestation == "ACTIVE"
        && (classifier_target != target
            || classifier_applied_autotune != autotune_profile
            || classifier_applied_configured != profile_mode
            || classifier_applied_resolved != profile_resolved)
    {
        add_issue(
            &mut issues,
            "Loaded traffic-priority rules target a stale interface or profile.",
        );
        "DRIFTED"
    } else if classifier_attestation == "ACTIVE" && cake_ul_state == "ACTIVE" {
        "ACTIVE"
    } else if classifier_attestation == "ACTIVE" {
        add_issue(
            &mut issues,
            "Traffic-priority rules are loaded but upload CAKE is not using diffserv4.",
        );
        "INEFFECTIVE"
    } else if matches!(classifier_attestation.as_str(), "DRIFTED" | "UNTRACKED") {
        add_issue(
            &mut issues,
            "The native traffic-priority table no longer matches its attested state.",
        );
        "DRIFTED"
    } else if matches!(classifier_attestation.as_str(), "MISSING" | "INACTIVE") {
        add_issue(
            &mut issues,
            "Traffic-priority rules are enabled but no attested rules exist for this instance.",
        );
        "MISSING"
    } else {
        add_issue(
            &mut issues,
            "Traffic-priority classifier status is unavailable or invalid.",
        );
        classifier_attestation.as_str()
    };

    // Calibration and Apply ownership now lives exclusively in the native
    // coordinator. Status.js overlays its authenticated active-operation
    // snapshot; runtime-health never guesses from retired tmpfs PID/marker
    // files.
    let apply_state = "IDLE";
    let operation = "IDLE";

    let mut overall = "HEALTHY";
    if !enabled {
        overall = if autorate_state == "DISABLED"
            && cake_ul_state != "ORPHANED"
            && cake_dl_state != "ORPHANED"
            && ifb_state != "ORPHANED"
            && ingress_state != "ORPHANED"
        {
            "DISABLED"
        } else {
            "ORPHANED"
        };
    } else if controller.fresh
        && autorate_state == "RUNNING"
        && (!manage_sqm || sqm_config_state == "ENABLED")
    {
        match controller.state.as_str() {
            "RECOVERING" => overall = "RECOVERING",
            "WAITING_LINK" | "WAITING_SQM" | "WAITING_EXTERNAL_SQM" | "WAITING_OPERATION" => {
                overall = "WAITING";
                if !controller.reason.is_empty() {
                    add_issue(
                        &mut issues,
                        format!("Controller is waiting: {}", controller.reason),
                    );
                }
            }
            _ => {}
        }
    }
    if overall == "HEALTHY" && runtime_link_wait {
        overall = "WAITING";
        add_issue(
            &mut issues,
            format!("Target interface {target} is unavailable; waiting for link and managed SQM."),
        );
    }
    if overall == "HEALTHY" && !manage_sqm {
        overall = if autorate_state == "RUNNING" {
            "UNMANAGED"
        } else {
            "DEGRADED"
        };
    } else if overall == "HEALTHY"
        && (autorate_state != "RUNNING"
            || sqm_config_state != "ENABLED"
            || cake_ul_state == "ORPHANED"
            || cake_dl_state == "ORPHANED"
            || ifb_state == "ORPHANED"
            || ingress_state == "ORPHANED"
            || (shape_upload && cake_ul_state != "ACTIVE")
            || (shape_download && cake_dl_state != "ACTIVE")
            || (shape_download && ifb_state != "PRESENT")
            || (shape_download && ingress_state != "ACTIVE")
            || classifier_state == "ORPHANED"
            || (classifier_expected && classifier_state != "ACTIVE"))
    {
        overall = if [
            autorate_state,
            sqm_config_state,
            cake_ul_state,
            cake_dl_state,
            ifb_state,
            ingress_state,
            classifier_state,
        ]
        .iter()
        .any(|state| matches!(*state, "DUPLICATE" | "CONFLICT" | "ORPHANED"))
        {
            "ORPHANED"
        } else {
            "DEGRADED"
        };
    }

    InstanceHealth {
        overall_state: overall.to_string(),
        autorate_state: autorate_state.to_string(),
        autorate_processes: daemon_count,
        controller_state: controller.state,
        controller_status_fresh: controller.fresh,
        controller_reason: controller.reason,
        sqm_config_state: sqm_config_state.to_string(),
        sqm_section,
        sqm_owner,
        sqm_target,
        sqm_direction_mode: direction_mode.to_string(),
        target_interface: target,
        target_state: if target_present { "PRESENT" } else { "MISSING" }.to_string(),
        ul_interface: upload,
        dl_interface: download,
        cake_ul_state: cake_ul_state.to_string(),
        cake_ul_kind: upload_cake.kind,
        cake_ul_mode: upload_cake.mode,
        cake_ul_count: upload_cake.count,
        cake_ul_rate_kbps: upload_cake.rate_kbps,
        cake_dl_state: cake_dl_state.to_string(),
        cake_dl_kind: download_cake.kind,
        cake_dl_mode: download_cake.mode,
        cake_dl_count: download_cake.count,
        cake_dl_rate_kbps: download_cake.rate_kbps,
        classifier_state: classifier_state.to_string(),
        classifier_profile: profile_resolved.clone(),
        classifier_global_state: classifier.state.clone(),
        autotune_profile: autotune_profile.to_string(),
        traffic_profile_mode: profile_mode,
        traffic_profile_resolved: profile_resolved,
        access_medium: medium,
        access_medium_source: medium_source,
        access_medium_confidence_percent: medium_confidence,
        capacity_learning_policy: learning_policy,
        classifier_target,
        classifier_applied_profile: classifier_applied_resolved.clone(),
        classifier_applied_autotune_profile: classifier_applied_autotune,
        classifier_applied_configured_profile: classifier_applied_configured,
        classifier_applied_resolved_profile: classifier_applied_resolved,
        ifb_state: ifb_state.to_string(),
        ingress_state: ingress_state.to_string(),
        apply_state: apply_state.to_string(),
        operation_state: operation.to_string(),
        issues: issues.join("; "),
        observed_at: now,
    }
}

pub fn run_runtime_health<I>(mut args: I) -> Result<String, String>
where
    I: Iterator<Item = String>,
{
    if args.next().is_some() {
        return Err("runtime-health accepts no options".to_string());
    }
    let environment = Environment::live();
    let cake = uci_package(&environment, "cake-autorate")?;
    let sqm = uci_package(&environment, "sqm").unwrap_or_default();
    let classifier = classifier_status(&environment);
    let now = now_epoch();
    let mut encoded = Vec::new();
    for (instance, section) in &cake.sections {
        if section.section_type != "cake_autorate" || !safe_name(instance) {
            continue;
        }
        encoded.push((
            instance.clone(),
            classify_instance(&environment, instance, section, &sqm, &classifier, now),
        ));
    }
    let instances = encoded
        .into_iter()
        .map(|(instance, health)| {
            format!(
                "\"{}\":{}",
                super::json_wire::json_escape(&instance),
                health.encode_json()
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    Ok(format!(
        "{{\"schema_version\":{SCHEMA_VERSION},\"instances\":{{{instances}}}}}\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

    struct Fixture {
        root: PathBuf,
        environment: Environment,
    }

    impl Fixture {
        fn new(tc_body: &str, classifier_json: &str, daemon_count: usize) -> Self {
            let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "runtime-health-fixture-{}-{id}",
                std::process::id()
            ));
            fs::create_dir_all(root.join("sys/eth0")).unwrap();
            fs::create_dir_all(root.join("sys/ifb4eth0")).unwrap();
            for path in ["proc", "run", "autotune", "quality", "speedtest", "apply"] {
                fs::create_dir_all(root.join(path)).unwrap();
            }
            let tc = root.join("tc");
            let pgrep = root.join("pgrep");
            let classifier = root.join("classifier");
            write_executable(&tc, tc_body);
            write_executable(
                &pgrep,
                &format!(
                    "#!/bin/sh\nindex=0\nwhile [ \"$index\" -lt {daemon_count} ]; do echo $((1000 + index)); index=$((index + 1)); done\n"
                ),
            );
            write_executable(
                &classifier,
                &format!(
                    "#!/bin/sh\nprintf '%s\\n' '{}'\n",
                    classifier_json.replace('\u{27}', "'\\''")
                ),
            );
            let environment = Environment {
                uci: "/bin/false".into(),
                tc: tc.to_string_lossy().into_owned(),
                pgrep: pgrep.to_string_lossy().into_owned(),
                ubus: "/bin/false".into(),
                daemon: "/usr/sbin/cake-autorated".into(),
                classifier: classifier.to_string_lossy().into_owned(),
                sys_class_net: root.join("sys"),
                runtime_root: root.join("run"),
            };
            Self { root, environment }
        }

        fn remove_ifb(&self) {
            fs::remove_dir_all(self.root.join("sys/ifb4eth0")).unwrap();
        }

        fn controller(&self, instance: &str, json: &str) {
            let directory = self.root.join("run").join(instance);
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("status.json"), json).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn write_executable(path: &Path, content: &str) {
        fs::write(path, content).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(path).unwrap().permissions();
            permissions.set_mode(0o700);
            fs::set_permissions(path, permissions).unwrap();
        }
    }

    fn base_config(enabled: bool) -> UciSection {
        let mut options = BTreeMap::from([
            ("enabled".into(), if enabled { "1" } else { "0" }.into()),
            ("manage_sqm".into(), "1".into()),
            ("sqm_enabled".into(), if enabled { "1" } else { "0" }.into()),
            ("sqm_direction_mode".into(), "both".into()),
            ("traffic_rules_enabled".into(), "1".into()),
            ("autotune_profile".into(), "best_overall".into()),
            ("access_medium".into(), "unknown".into()),
            ("access_medium_source".into(), "auto_inconclusive".into()),
            ("access_medium_confidence_percent".into(), "20".into()),
            ("capacity_learning_policy".into(), "verified_only".into()),
            ("traffic_profile".into(), "auto".into()),
            ("wan_if".into(), "eth0".into()),
            ("sqm_interface".into(), "eth0".into()),
            ("ul_if".into(), "eth0".into()),
            ("dl_if".into(), "ifb4eth0".into()),
            ("sqm_section".into(), "cake_wan_sqm".into()),
        ]);
        if !enabled {
            options.insert("traffic_rules_enabled".into(), "0".into());
        }
        UciSection {
            section_type: "cake_autorate".into(),
            options,
        }
    }

    fn base_sqm(enabled: bool) -> UciPackage {
        UciPackage {
            sections: BTreeMap::from([(
                "cake_wan_sqm".into(),
                UciSection {
                    section_type: "queue".into(),
                    options: BTreeMap::from([
                        ("_cake_autorate_managed".into(), "wan_sqm".into()),
                        ("interface".into(), "eth0".into()),
                        ("enabled".into(), if enabled { "1" } else { "0" }.into()),
                    ]),
                },
            )]),
        }
    }

    const HEALTHY_TC: &str = r#"#!/bin/sh
case "$*" in
  "qdisc show dev eth0")
    echo 'qdisc cake 8001: root bandwidth 100Mbit diffserv4'
    echo 'qdisc ingress ffff: parent ffff:fff1' ;;
  "qdisc show dev ifb4eth0") echo 'qdisc cake_mq 8002: root bandwidth 500Mbit besteffort' ;;
  "filter show dev eth0 ingress") echo 'action order 1: mirred (Egress Redirect to device ifb4eth0) stolen' ;;
esac
"#;

    const UPLOAD_ONLY_TC: &str = r#"#!/bin/sh
case "$*" in
  "qdisc show dev eth0") echo 'qdisc cake 8001: root bandwidth 100Mbit diffserv4' ;;
esac
"#;

    fn active_classifier() -> &'static str {
        r#"{"state":"active","schema_version":3,"table_present":true,"attested_instances":"wan_sqm|eth0|best_overall|auto|best_overall"}"#
    }

    #[test]
    fn uci_package_parser_is_bounded_and_exact() {
        let package = UciPackage::parse(
            "cake-autorate",
            "cake-autorate.wan=cake_autorate\ncake-autorate.wan.enabled='1'\n",
        )
        .unwrap();
        assert_eq!(package.sections["wan"].section_type, "cake_autorate");
        assert_eq!(package.sections["wan"].options["enabled"], "1");
        let list = UciPackage::parse(
            "cake-autorate",
            "cake-autorate.wan=cake_autorate\ncake-autorate.wan.reflector='1.1.1.1' '1.0.0.1'\n",
        )
        .unwrap();
        assert_eq!(list.sections["wan"].options["reflector"], "1.1.1.1");
        assert!(UciPackage::parse(
            "cake-autorate",
            "cake-autorate.wan=cake_autorate\ncake-autorate.wan.enabled='1'\ncake-autorate.wan.enabled='0'\n",
        )
        .is_err());
    }

    #[test]
    #[cfg(feature = "calibration")]
    fn nonnegative_json_float_parser_is_finite_and_exact() {
        let json = r#"{"started_at":123.500,"scientific":1.25e2,"negative":-1.0,"nan":"NaN"}"#;
        assert_eq!(json_nonnegative_f64_value(json, "started_at"), Some(123.5));
        assert_eq!(json_nonnegative_f64_value(json, "scientific"), Some(125.0));
        assert_eq!(json_nonnegative_f64_value(json, "negative"), None);
        assert_eq!(json_nonnegative_f64_value(json, "nan"), None);
        assert_eq!(json_nonnegative_f64_value(json, "missing"), None);
    }

    #[test]
    fn controller_parser_uses_the_first_top_level_state_contract() {
        let json = r#"{"state":"RUNNING","updated_at":42,"adaptive_capacity":{"download":{"confidence":{"state":"passive_transport"}}}}"#;
        assert_eq!(json_string_value(json, "state").as_deref(), Some("RUNNING"));
        assert_eq!(json_u64_value(json, "updated_at"), Some(42));
        let quoted_key =
            r#"{"sqm_runtime_reason":"waiting for \"state\" change","state":"WAITING_SQM"}"#;
        assert_eq!(
            json_string_value(quoted_key, "state").as_deref(),
            Some("WAITING_SQM")
        );
    }

    #[test]
    fn root_cake_parser_preserves_kind_mode_and_decimal_rate() {
        let root = std::env::temp_dir().join(format!("runtime-health-cake-{}", std::process::id()));
        let interface = root.join("sys/eth0");
        fs::create_dir_all(&interface).unwrap();
        let tc = root.join("tc");
        fs::write(
            &tc,
            "#!/bin/sh\nprintf '%s\\n' 'qdisc cake_mq 8001: root bandwidth 723.4Mbit diffserv4'\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&tc).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            permissions.set_mode(0o700);
            fs::set_permissions(&tc, permissions).unwrap();
        }
        let environment = Environment {
            uci: String::new(),
            tc: tc.to_string_lossy().into_owned(),
            pgrep: String::new(),
            ubus: String::new(),
            daemon: String::new(),
            classifier: String::new(),
            sys_class_net: root.join("sys"),
            runtime_root: root.join("run"),
        };
        assert_eq!(
            cake_info(&environment, "eth0"),
            CakeInfo {
                count: 1,
                kind: "cake_mq".into(),
                mode: "diffserv4".into(),
                rate_kbps: 723_400
            }
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn owned_runtime_state_distinguishes_external_missing_and_orphaned() {
        assert_eq!(owned_runtime_state(false, false, 1), "EXTERNAL");
        assert_eq!(owned_runtime_state(true, true, 0), "MISSING");
        assert_eq!(owned_runtime_state(true, true, 1), "ACTIVE");
        assert_eq!(owned_runtime_state(true, false, 1), "ORPHANED");
    }

    #[test]
    fn json_contract_escapes_diagnostics() {
        let health = InstanceHealth {
            overall_state: "WAITING".into(),
            autorate_state: "RUNNING".into(),
            autorate_processes: 1,
            controller_state: "WAITING_SQM".into(),
            controller_status_fresh: true,
            controller_reason: "waiting \"for\" IFB".into(),
            sqm_config_state: "ENABLED".into(),
            sqm_section: "cake_wan".into(),
            sqm_owner: "wan".into(),
            sqm_target: "eth0".into(),
            sqm_direction_mode: "both".into(),
            target_interface: "eth0".into(),
            target_state: "PRESENT".into(),
            ul_interface: "eth0".into(),
            dl_interface: "ifb4eth0".into(),
            cake_ul_state: "ACTIVE".into(),
            cake_ul_kind: "cake".into(),
            cake_ul_mode: "diffserv4".into(),
            cake_ul_count: 1,
            cake_ul_rate_kbps: 100_000,
            cake_dl_state: "ACTIVE".into(),
            cake_dl_kind: "cake".into(),
            cake_dl_mode: "besteffort".into(),
            cake_dl_count: 1,
            cake_dl_rate_kbps: 500_000,
            classifier_state: "ACTIVE".into(),
            classifier_profile: "best_overall".into(),
            classifier_global_state: "ACTIVE".into(),
            autotune_profile: "best_overall".into(),
            traffic_profile_mode: "auto".into(),
            traffic_profile_resolved: "best_overall".into(),
            access_medium: "unknown".into(),
            access_medium_source: "auto_inconclusive".into(),
            access_medium_confidence_percent: 20,
            capacity_learning_policy: "verified_only".into(),
            classifier_target: "eth0".into(),
            classifier_applied_profile: "best_overall".into(),
            classifier_applied_autotune_profile: "best_overall".into(),
            classifier_applied_configured_profile: "auto".into(),
            classifier_applied_resolved_profile: "best_overall".into(),
            ifb_state: "PRESENT".into(),
            ingress_state: "ACTIVE".into(),
            apply_state: "IDLE".into(),
            operation_state: "IDLE".into(),
            issues: "Controller is waiting: waiting \"for\" IFB".into(),
            observed_at: 42,
        };
        let encoded = health.encode_json();
        assert!(encoded.contains("\\\"for\\\""));
        assert!(encoded.ends_with("\"observed_at\":42}"));
    }

    #[test]
    fn healthy_both_directions_match_the_retired_contract() {
        let fixture = Fixture::new(HEALTHY_TC, active_classifier(), 1);
        let health = classify_instance(
            &fixture.environment,
            "wan_sqm",
            &base_config(true),
            &base_sqm(true),
            &classifier_status(&fixture.environment),
            100,
        );
        assert_eq!(health.overall_state, "HEALTHY");
        assert_eq!(health.autorate_state, "RUNNING");
        assert_eq!(health.sqm_config_state, "ENABLED");
        assert_eq!(health.cake_ul_state, "ACTIVE");
        assert_eq!(health.cake_ul_rate_kbps, 100_000);
        assert_eq!(health.cake_dl_state, "ACTIVE");
        assert_eq!(health.cake_dl_kind, "cake_mq");
        assert_eq!(health.cake_dl_rate_kbps, 500_000);
        assert_eq!(health.ingress_state, "ACTIVE");
        assert_eq!(health.classifier_state, "ACTIVE");
        assert!(health.issues.is_empty());
    }

    #[test]
    fn upload_only_treats_an_empty_ifb_as_idle_without_inventing_a_redirect() {
        let fixture = Fixture::new(UPLOAD_ONLY_TC, active_classifier(), 1);
        let mut config = base_config(true);
        config
            .options
            .insert("sqm_direction_mode".into(), "upload_only".into());
        let health = classify_instance(
            &fixture.environment,
            "wan_sqm",
            &config,
            &base_sqm(true),
            &classifier_status(&fixture.environment),
            100,
        );
        assert_eq!(health.overall_state, "HEALTHY");
        assert_eq!(health.cake_ul_state, "ACTIVE");
        assert_eq!(health.cake_dl_state, "ABSENT");
        assert_eq!(health.ifb_state, "IDLE");
        assert_eq!(health.ingress_state, "ABSENT");
        assert!(health.issues.is_empty());
    }

    #[test]
    fn disabled_instance_with_live_topology_is_orphaned() {
        let fixture = Fixture::new(HEALTHY_TC, active_classifier(), 0);
        let health = classify_instance(
            &fixture.environment,
            "wan_sqm",
            &base_config(false),
            &base_sqm(false),
            &classifier_status(&fixture.environment),
            100,
        );
        assert_eq!(health.overall_state, "ORPHANED");
        assert_eq!(health.cake_ul_state, "ORPHANED");
        assert_eq!(health.cake_dl_state, "ORPHANED");
        assert_eq!(health.ifb_state, "ORPHANED");
        assert_eq!(health.ingress_state, "ORPHANED");
        assert_eq!(health.classifier_state, "ORPHANED");
    }

    #[test]
    fn fresh_waiting_controller_suppresses_false_missing_topology_failures() {
        let fixture = Fixture::new("#!/bin/sh\nexit 0\n", r#"{"state":"inactive"}"#, 1);
        fixture.remove_ifb();
        fixture.controller(
            "wan_sqm",
            r#"{"state":"WAITING_SQM","sqm_runtime_reason":"waiting for IFB","updated_at":100}"#,
        );
        let health = classify_instance(
            &fixture.environment,
            "wan_sqm",
            &base_config(true),
            &base_sqm(true),
            &classifier_status(&fixture.environment),
            100,
        );
        assert_eq!(health.overall_state, "WAITING");
        assert_eq!(health.controller_state, "WAITING_SQM");
        assert!(health.controller_status_fresh);
        assert_eq!(health.classifier_state, "WAITING");
        assert_eq!(health.issues, "Controller is waiting: waiting for IFB");
    }
}
