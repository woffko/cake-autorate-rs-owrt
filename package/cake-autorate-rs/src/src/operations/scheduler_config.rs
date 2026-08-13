//! Strict, non-secret UCI policy snapshot for the native scheduler.
//!
//! Parsing is pure and bounded. Live route, SQM ownership and complete config
//! fingerprints are still re-attested by `build_live_scheduled_autotune_request`
//! immediately before admission; this module only translates committed policy
//! into a typed launch intent.

use std::collections::BTreeMap;
use std::io::Read;
use std::process::{Command, Stdio};

use super::autotune_request::AutotuneLaunchIntent;
use super::protocol::CalibrationStrategy;
use super::scheduler::validate_scheduler_instance;
use crate::autotune::{
    AccessEvidenceSource, AccessMedium, AutotuneProfile, CapacityLearningPolicy,
};
use crate::routing::RouteSpec;

const MAX_UCI_BYTES: usize = 256 * 1024;
const MAX_SCHEDULED_INSTANCES: usize = 64;
const MIB: u64 = 1024 * 1024;
const MAX_EXACT_BYTES: u64 = 9_007_199_254_740_991;
const MAX_OPERATION_TRAFFIC_BUDGET_BYTES: u64 = 1 << 40;

const RELEVANT_OPTIONS: &[&str] = &[
    "enabled",
    "scheduled_autotune_enabled",
    "scheduled_autotune_interval_hours",
    "scheduled_autotune_idle_window_s",
    "scheduled_autotune_window_start_hour",
    "scheduled_autotune_window_end_hour",
    "scheduled_autotune_max_traffic_mb_day",
    "scheduled_autotune_max_traffic_mb_month",
    "scheduled_autotune_auto_apply",
    "connection_active_thr_kbps",
    "sqm_interface",
    "wan_if",
    "ul_if",
    "auto_interface_preset",
    "route_mode",
    "mwan3_member",
    "autotune_profile",
    "autotune_calibration_strategy",
    "speedtest_backend",
    "access_medium",
    "access_medium_source",
    "access_medium_confidence_percent",
    "capacity_learning_policy",
    "runtime_learning_mode",
    "adaptive_ceiling_enabled",
    "service_dl_cap_kbps",
    "service_ul_cap_kbps",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduledInstanceConfig {
    pub instance: String,
    pub instance_enabled: bool,
    pub scheduled_enabled: bool,
    pub interval_s: u64,
    pub idle_window_s: u64,
    pub window_start_hour: u8,
    pub window_end_hour: u8,
    pub daily_limit_bytes: u64,
    pub monthly_limit_bytes: u64,
    pub auto_apply: bool,
    pub active_threshold_kbps: u64,
    pub expected_target_interface: String,
    pub backend: String,
    pub route_mode: String,
    pub mwan3_member: String,
    pub profile: AutotuneProfile,
    pub strategy: CalibrationStrategy,
    pub access_medium: AccessMedium,
    pub access_source: AccessEvidenceSource,
    pub access_confidence_percent: u8,
    pub capacity_learning_policy: CapacityLearningPolicy,
    pub service_dl_cap_kbps: Option<u64>,
    pub service_ul_cap_kbps: Option<u64>,
}

impl ScheduledInstanceConfig {
    pub fn enabled(&self) -> bool {
        self.instance_enabled && self.scheduled_enabled
    }

    pub fn launch_intent(&self, traffic_budget_bytes: u64) -> Result<AutotuneLaunchIntent, String> {
        if traffic_budget_bytes == 0 || traffic_budget_bytes > MAX_OPERATION_TRAFFIC_BUDGET_BYTES {
            return Err("scheduled launch traffic budget is outside the exact range".to_string());
        }
        Ok(AutotuneLaunchIntent {
            instance: self.instance.clone(),
            expected_target_interface: self.expected_target_interface.clone(),
            backend: self.backend.clone(),
            route_mode: self.route_mode.clone(),
            mwan3_member: self.mwan3_member.clone(),
            profile: self.profile,
            strategy: self.strategy,
            access_medium: self.access_medium,
            access_source: self.access_source,
            access_confidence_percent: self.access_confidence_percent,
            capacity_learning_policy: self.capacity_learning_policy,
            service_dl_cap_kbps: self.service_dl_cap_kbps,
            service_ul_cap_kbps: self.service_ul_cap_kbps,
            allow_sqm_disable: self.strategy == CalibrationStrategy::FullRaw,
            allow_active_traffic: false,
            traffic_budget_bytes,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchedulerConfigIssue {
    pub instance: String,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SchedulerConfigSnapshot {
    pub instances: Vec<ScheduledInstanceConfig>,
    pub issues: Vec<SchedulerConfigIssue>,
}

#[derive(Default)]
struct RawSection {
    kind: Option<String>,
    options: BTreeMap<String, Vec<String>>,
    unknown_scheduler_options: Vec<String>,
}

pub fn load_scheduled_instances() -> Result<SchedulerConfigSnapshot, String> {
    let raw = run_uci(
        &["-q", "show", "cake-autorate"],
        "CAKE Autorate configuration",
    )?;
    parse_scheduled_instances(&raw)
}

pub fn scheduled_configuration_committed() -> Result<bool, String> {
    for package in ["cake-autorate", "sqm"] {
        let output = run_uci(&["-q", "changes", package], "pending UCI changes")?;
        if !output.trim().is_empty() {
            return Ok(false);
        }
    }
    Ok(true)
}

pub fn parse_scheduled_instances(input: &str) -> Result<SchedulerConfigSnapshot, String> {
    if input.len() > MAX_UCI_BYTES || input.as_bytes().contains(&0) {
        return Err("CAKE Autorate UCI snapshot exceeds its safe bound".to_string());
    }
    let mut sections = BTreeMap::<String, RawSection>::new();
    for (index, line) in input.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let (left, raw_value) = line
            .split_once('=')
            .ok_or_else(|| format!("UCI line {} has no value", index + 1))?;
        let mut path = left.split('.');
        if path.next() != Some("cake-autorate") {
            return Err(format!("UCI line {} belongs to another package", index + 1));
        }
        let instance = path
            .next()
            .ok_or_else(|| format!("UCI line {} has no section", index + 1))?;
        validate_scheduler_instance(instance)?;
        let option = path.next();
        if path.next().is_some() {
            return Err(format!("UCI line {} has an invalid path", index + 1));
        }
        let section = sections.entry(instance.to_string()).or_default();
        match option {
            None => {
                if section.kind.is_some() {
                    return Err(format!("UCI section {instance} has a duplicate type"));
                }
                section.kind = Some(parse_scalar(raw_value)?);
            }
            Some(option) if RELEVANT_OPTIONS.contains(&option) => {
                if !option
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                {
                    return Err(format!("UCI option {option} is invalid"));
                }
                section
                    .options
                    .entry(option.to_string())
                    .or_default()
                    .push(raw_value.to_string());
            }
            Some(option) if option.starts_with("scheduled_autotune_") => {
                section.unknown_scheduler_options.push(option.to_string());
            }
            Some(_) => {}
        }
    }

    let mut snapshot = SchedulerConfigSnapshot::default();
    for (instance, section) in sections {
        if section.kind.as_deref() != Some("cake_autorate") {
            continue;
        }
        if snapshot.instances.len() + snapshot.issues.len() >= MAX_SCHEDULED_INSTANCES {
            return Err("too many CAKE Autorate instances for the native scheduler".to_string());
        }
        match parse_instance(&instance, &section) {
            Ok(config) => snapshot.instances.push(config),
            Err(message) => snapshot
                .issues
                .push(SchedulerConfigIssue { instance, message }),
        }
    }
    Ok(snapshot)
}

fn parse_instance(instance: &str, section: &RawSection) -> Result<ScheduledInstanceConfig, String> {
    if !section.unknown_scheduler_options.is_empty() {
        return Err(format!(
            "unknown scheduled Auto-Tune option(s): {}",
            section.unknown_scheduler_options.join(", ")
        ));
    }
    let values = scalar_options(section)?;
    let instance_enabled = boolean(&values, "enabled", false)?;
    let scheduled_enabled = boolean(&values, "scheduled_autotune_enabled", false)?;
    let interval_hours = bounded_u64(&values, "scheduled_autotune_interval_hours", 24, 1, 8_760)?;
    let interval_s = interval_hours
        .checked_mul(3_600)
        .ok_or_else(|| "scheduled interval overflows seconds".to_string())?;
    let idle_window_s = bounded_u64(&values, "scheduled_autotune_idle_window_s", 60, 30, 3_600)?;
    let window_start_hour =
        bounded_u64(&values, "scheduled_autotune_window_start_hour", 2, 0, 23)? as u8;
    let window_end_hour =
        bounded_u64(&values, "scheduled_autotune_window_end_hour", 5, 0, 23)? as u8;
    let daily_limit_bytes = mb_limit(&values, "scheduled_autotune_max_traffic_mb_day", 4_096)?;
    let monthly_limit_bytes = mb_limit(&values, "scheduled_autotune_max_traffic_mb_month", 16_384)?;
    let auto_apply = boolean(&values, "scheduled_autotune_auto_apply", false)?;
    let active_threshold_kbps =
        bounded_u64(&values, "connection_active_thr_kbps", 2_000, 1, 100_000_000)?;

    let auto_interface = boolean(&values, "auto_interface_preset", true)?;
    let configured_target = text(&values, "sqm_interface", "");
    let wan_if = text(&values, "wan_if", "");
    let ul_if = text(&values, "ul_if", "wan");
    let expected_target_interface = if !configured_target.is_empty() {
        configured_target
    } else if auto_interface && !wan_if.is_empty() {
        wan_if
    } else {
        ul_if
    };
    RouteSpec::new("main", "", &expected_target_interface).validate()?;

    let route_mode = text(&values, "route_mode", "auto");
    let mwan3_member = text(&values, "mwan3_member", "");
    RouteSpec::new(&route_mode, &mwan3_member, &expected_target_interface).validate()?;

    let backend = match text(&values, "speedtest_backend", "auto").as_str() {
        "auto" | "speedtest-go" => "speedtest-go".to_string(),
        other => {
            return Err(format!(
                "native scheduled Auto-Tune does not support backend {other}"
            ))
        }
    };
    let profile = match AutotuneProfile::parse(&text(&values, "autotune_profile", "best_overall"))
        .ok_or_else(|| "scheduled Auto-Tune profile is invalid".to_string())?
    {
        AutotuneProfile::GamingExtreme => AutotuneProfile::Gaming,
        profile => profile,
    };
    let strategy = CalibrationStrategy::parse(&text(
        &values,
        "autotune_calibration_strategy",
        "shaped_only",
    ))
    .ok_or_else(|| "scheduled calibration strategy is invalid".to_string())?;
    let access_medium = AccessMedium::parse(&text(&values, "access_medium", "unknown"))
        .ok_or_else(|| "scheduled access medium is invalid".to_string())?;
    let access_source =
        AccessEvidenceSource::parse(&text(&values, "access_medium_source", "legacy_default"))
            .ok_or_else(|| "scheduled access evidence source is invalid".to_string())?;
    let access_confidence_percent =
        bounded_u64(&values, "access_medium_confidence_percent", 0, 0, 100)? as u8;
    let capacity_learning_policy = resolve_capacity_policy(
        &values,
        scheduled_enabled,
        profile,
        access_medium,
        access_source,
        access_confidence_percent,
    )?;
    if scheduled_enabled && capacity_learning_policy != CapacityLearningPolicy::ScheduledActive {
        return Err(
            "scheduled calibration is enabled but runtime capacity policy is not scheduled_active"
                .to_string(),
        );
    }
    let service_dl_cap_kbps = optional_rate(&values, "service_dl_cap_kbps")?;
    let service_ul_cap_kbps = optional_rate(&values, "service_ul_cap_kbps")?;
    if capacity_learning_policy == CapacityLearningPolicy::FixedCap
        && (service_dl_cap_kbps.is_none() || service_ul_cap_kbps.is_none())
    {
        return Err("fixed_cap requires both service rate caps".to_string());
    }

    let config = ScheduledInstanceConfig {
        instance: instance.to_string(),
        instance_enabled,
        scheduled_enabled,
        interval_s,
        idle_window_s,
        window_start_hour,
        window_end_hour,
        daily_limit_bytes,
        monthly_limit_bytes,
        auto_apply,
        active_threshold_kbps,
        expected_target_interface,
        backend,
        route_mode,
        mwan3_member,
        profile,
        strategy,
        access_medium,
        access_source,
        access_confidence_percent,
        capacity_learning_policy,
        service_dl_cap_kbps,
        service_ul_cap_kbps,
    };
    config.launch_intent(1)?;
    Ok(config)
}

fn scalar_options(section: &RawSection) -> Result<BTreeMap<String, String>, String> {
    let mut values = BTreeMap::new();
    for (name, raw_values) in &section.options {
        if raw_values.len() != 1 {
            return Err(format!("scheduled UCI option {name} is duplicated"));
        }
        values.insert(name.clone(), parse_scalar(&raw_values[0])?);
    }
    Ok(values)
}

fn resolve_capacity_policy(
    values: &BTreeMap<String, String>,
    scheduled_enabled: bool,
    profile: AutotuneProfile,
    medium: AccessMedium,
    source: AccessEvidenceSource,
    confidence: u8,
) -> Result<CapacityLearningPolicy, String> {
    if let Some(value) = values.get("capacity_learning_policy") {
        return CapacityLearningPolicy::parse(value)
            .ok_or_else(|| "scheduled capacity learning policy is invalid".to_string());
    }
    if let Some(value) = values.get("runtime_learning_mode") {
        return match value.as_str() {
            "passive" => Ok(CapacityLearningPolicy::PassiveBounded),
            "periodic_active" => Ok(CapacityLearningPolicy::ScheduledActive),
            "fixed" => Ok(CapacityLearningPolicy::VerifiedOnly),
            _ => Err("legacy runtime learning mode is invalid".to_string()),
        };
    }
    if scheduled_enabled {
        return Ok(CapacityLearningPolicy::ScheduledActive);
    }
    if boolean(values, "adaptive_ceiling_enabled", false)? {
        return Ok(CapacityLearningPolicy::PassiveBounded);
    }
    if profile != AutotuneProfile::VariableLink {
        return Ok(CapacityLearningPolicy::VerifiedOnly);
    }
    let strong_source = matches!(
        source,
        AccessEvidenceSource::UserSelected
            | AccessEvidenceSource::NetworkProtocol
            | AccessEvidenceSource::DeviceType
            | AccessEvidenceSource::InterfaceName
    );
    if strong_source && medium != AccessMedium::Unknown && confidence >= 50 {
        Ok(CapacityLearningPolicy::PassiveBounded)
    } else {
        Ok(CapacityLearningPolicy::VerifiedOnly)
    }
}

fn parse_scalar(raw: &str) -> Result<String, String> {
    let mut value = String::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut started = false;
    let mut ended = false;
    for character in raw.trim().chars() {
        if escaped {
            value.push(character);
            escaped = false;
            started = true;
        } else if character == '\\' {
            escaped = true;
            started = true;
        } else if character == '\'' {
            quoted = !quoted;
            started = true;
        } else if character.is_whitespace() && !quoted {
            if started {
                ended = true;
            }
        } else {
            if ended {
                return Err("scheduled UCI option contains multiple values".to_string());
            }
            value.push(character);
            started = true;
        }
    }
    if !started || quoted || escaped {
        return Err("scheduled UCI scalar is incomplete".to_string());
    }
    Ok(value)
}

fn boolean(values: &BTreeMap<String, String>, name: &str, default: bool) -> Result<bool, String> {
    match values.get(name).map(String::as_str) {
        None => Ok(default),
        Some("0") => Ok(false),
        Some("1") => Ok(true),
        Some(_) => Err(format!("scheduled UCI option {name} is not boolean")),
    }
}

fn text(values: &BTreeMap<String, String>, name: &str, default: &str) -> String {
    values
        .get(name)
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

fn bounded_u64(
    values: &BTreeMap<String, String>,
    name: &str,
    default: u64,
    minimum: u64,
    maximum: u64,
) -> Result<u64, String> {
    let Some(raw) = values.get(name) else {
        return Ok(default);
    };
    if raw.is_empty() || (raw.len() > 1 && raw.starts_with('0')) {
        return Err(format!("scheduled UCI option {name} is not canonical"));
    }
    let value = raw
        .parse::<u64>()
        .map_err(|_| format!("scheduled UCI option {name} is not an unsigned integer"))?;
    if !(minimum..=maximum).contains(&value) {
        return Err(format!("scheduled UCI option {name} is outside its bounds"));
    }
    Ok(value)
}

fn mb_limit(values: &BTreeMap<String, String>, name: &str, default: u64) -> Result<u64, String> {
    bounded_u64(values, name, default, 100, 1_048_576)?
        .checked_mul(MIB)
        .filter(|value| *value <= MAX_EXACT_BYTES)
        .ok_or_else(|| format!("scheduled UCI option {name} overflows bytes"))
}

fn optional_rate(values: &BTreeMap<String, String>, name: &str) -> Result<Option<u64>, String> {
    values
        .get(name)
        .map(|_| bounded_u64(values, name, 0, 100, 100_000_000))
        .transpose()
}

fn run_uci(args: &[&str], label: &str) -> Result<String, String> {
    let mut child = Command::new("uci")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("unable to execute uci for {label}: {error}"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("uci stdout is unavailable for {label}"))?;
    let mut bytes = Vec::new();
    stdout
        .by_ref()
        .take((MAX_UCI_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("unable to read uci output for {label}: {error}"))?;
    if bytes.len() > MAX_UCI_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("uci output for {label} exceeds its safe bound"));
    }
    let status = child
        .wait()
        .map_err(|error| format!("unable to reap uci for {label}: {error}"))?;
    if !status.success() {
        return Err(format!("uci query for {label} failed"));
    }
    String::from_utf8(bytes).map_err(|_| format!("uci output for {label} is not UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn valid_instance(extra: &str) -> String {
        format!(
            "cake-autorate.globals=globals\n\
             cake-autorate.wan_sqm=cake_autorate\n\
             cake-autorate.wan_sqm.enabled='1'\n\
             cake-autorate.wan_sqm.scheduled_autotune_enabled='1'\n\
             cake-autorate.wan_sqm.sqm_interface='pppoe-wan'\n\
             cake-autorate.wan_sqm.route_mode='mwan3'\n\
             cake-autorate.wan_sqm.mwan3_member='wan_member'\n\
             cake-autorate.wan_sqm.autotune_profile='variable_link'\n\
             cake-autorate.wan_sqm.autotune_calibration_strategy='full_raw'\n\
             cake-autorate.wan_sqm.speedtest_backend='auto'\n\
             cake-autorate.wan_sqm.access_medium='cellular'\n\
             cake-autorate.wan_sqm.access_medium_source='user_selected'\n\
             cake-autorate.wan_sqm.access_medium_confidence_percent='100'\n\
             cake-autorate.wan_sqm.capacity_learning_policy='scheduled_active'\n{extra}"
        )
    }

    #[test]
    fn scheduled_instance_parses_into_a_noninteractive_launch_policy() {
        let snapshot = parse_scheduled_instances(&valid_instance("")).unwrap();
        assert!(snapshot.issues.is_empty());
        let config = &snapshot.instances[0];
        assert!(config.enabled());
        assert_eq!(config.interval_s, 86_400);
        assert_eq!(config.daily_limit_bytes, 4_096 * MIB);
        assert_eq!(config.backend, "speedtest-go");
        assert_eq!(config.profile, AutotuneProfile::VariableLink);
        let intent = config.launch_intent(123_456).unwrap();
        assert_eq!(intent.traffic_budget_bytes, 123_456);
        assert!(intent.allow_sqm_disable);
        assert!(!intent.allow_active_traffic);
    }

    #[test]
    fn extreme_profile_is_never_persisted_for_unattended_runs() {
        let input = valid_instance("").replace("variable_link", "gaming_extreme");
        let snapshot = parse_scheduled_instances(&input).unwrap();
        assert_eq!(snapshot.instances[0].profile, AutotuneProfile::Gaming);
    }

    #[test]
    fn one_invalid_instance_does_not_hide_an_independent_valid_instance() {
        let mut input = valid_instance(
            "cake-autorate.bad=cake_autorate\n\
             cake-autorate.bad.enabled='1'\n\
             cake-autorate.bad.scheduled_autotune_enabled='1'\n\
             cake-autorate.bad.sqm_interface='eth0'\n\
             cake-autorate.bad.speedtest_backend='iperf3'\n",
        );
        input.push_str("cake-autorate.rule1=traffic_rule\n");
        let snapshot = parse_scheduled_instances(&input).unwrap();
        assert_eq!(snapshot.instances.len(), 1);
        assert_eq!(snapshot.issues.len(), 1);
        assert_eq!(snapshot.issues[0].instance, "bad");
    }

    #[test]
    fn duplicate_or_malformed_scheduler_scalars_fail_closed() {
        let duplicate = format!(
            "{}cake-autorate.wan_sqm.scheduled_autotune_enabled='1'\n",
            valid_instance("")
        );
        let snapshot = parse_scheduled_instances(&duplicate).unwrap();
        assert_eq!(snapshot.instances.len(), 0);
        assert!(snapshot.issues[0].message.contains("duplicated"));

        let malformed = valid_instance("").replace("'cellular'", "'cellular");
        let snapshot = parse_scheduled_instances(&malformed).unwrap();
        assert!(snapshot.issues[0].message.contains("incomplete"));
    }

    #[test]
    fn inconsistent_active_policy_and_noncanonical_numbers_are_rejected() {
        let inconsistent = valid_instance("")
            .replace("scheduled_active", "verified_only")
            .replace(
                "access_medium_confidence_percent='100'",
                "access_medium_confidence_percent='01'",
            );
        let snapshot = parse_scheduled_instances(&inconsistent).unwrap();
        assert!(snapshot.issues[0].message.contains("not canonical"));

        let inconsistent = valid_instance("").replace("scheduled_active", "verified_only");
        let snapshot = parse_scheduled_instances(&inconsistent).unwrap();
        assert!(snapshot.issues[0].message.contains("not scheduled_active"));
    }

    #[test]
    fn legacy_scheduled_flag_resolves_a_missing_policy_but_disabled_variable_is_bounded() {
        let legacy = valid_instance("")
            .lines()
            .filter(|line| !line.contains("capacity_learning_policy"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let snapshot = parse_scheduled_instances(&legacy).unwrap();
        assert_eq!(
            snapshot.instances[0].capacity_learning_policy,
            CapacityLearningPolicy::ScheduledActive
        );

        let disabled = legacy.replace(
            "scheduled_autotune_enabled='1'",
            "scheduled_autotune_enabled='0'",
        );
        let snapshot = parse_scheduled_instances(&disabled).unwrap();
        assert_eq!(
            snapshot.instances[0].capacity_learning_policy,
            CapacityLearningPolicy::PassiveBounded
        );
    }

    #[test]
    fn only_one_scalar_value_and_bounded_instance_count_are_accepted() {
        assert!(parse_scalar("'one' 'two'").is_err());
        assert_eq!(parse_scalar("'one value'").unwrap(), "one value");
        let mut input = String::new();
        for index in 0..=MAX_SCHEDULED_INSTANCES {
            input.push_str(&format!(
                "cake-autorate.w{index}=cake_autorate\n\
                 cake-autorate.w{index}.sqm_interface='eth0'\n"
            ));
        }
        assert!(parse_scheduled_instances(&input).is_err());
    }

    #[test]
    fn relevant_option_set_has_no_duplicates() {
        let unique = RELEVANT_OPTIONS.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(unique.len(), RELEVANT_OPTIONS.len());
    }

    #[test]
    fn scheduler_option_typos_fail_their_instance_but_unrelated_options_are_ignored() {
        let typo = valid_instance("cake-autorate.wan_sqm.scheduled_autotune_interal_hours='12'\n");
        let snapshot = parse_scheduled_instances(&typo).unwrap();
        assert!(snapshot.instances.is_empty());
        assert_eq!(snapshot.issues.len(), 1);
        assert!(snapshot.issues[0]
            .message
            .contains("scheduled_autotune_interal_hours"));

        let unrelated = valid_instance("").replace(
            "cake-autorate.wan_sqm.enabled='1'",
            "cake-autorate.wan_sqm.enabled='1'\ncake-autorate.wan_sqm.reflector_type='icmp'",
        );
        let snapshot = parse_scheduled_instances(&unrelated).unwrap();
        assert_eq!(snapshot.instances.len(), 1);
        assert!(snapshot.issues.is_empty());
    }
}
