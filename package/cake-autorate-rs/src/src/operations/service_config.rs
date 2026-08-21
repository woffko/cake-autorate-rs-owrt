//! Idempotent normalization of service presets before SQM projection.
//!
//! This is deliberately a small UCI transaction: first build and validate the
//! complete mutation plan in memory, then execute exact argv-only UCI commands
//! and commit once. Both package variants share the same current
//! route/interface/rate preset contract.

use super::process::{
    run_bounded_command_output, run_bounded_command_output_with_input, SpawnSpec,
};
use super::runtime_health::{json_string_value, safe_interface, safe_name, UciPackage};
use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

const PACKAGE: &str = "cake-autorate";
const UCI_BIN: &str = "/sbin/uci";
const UBUS_BIN: &str = "/bin/ubus";
const SYS_CLASS_NET: &str = "/sys/class/net";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_UCI_OUTPUT: usize = 1024 * 1024;
const MAX_UBUS_OUTPUT: usize = 64 * 1024;
const MAX_ACTIONS: usize = 4096;
const MAX_VALUE_BYTES: usize = 4096;
const MAX_BATCH_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
enum PresetAction {
    Set {
        section: String,
        option: String,
        value: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PresetPlan {
    actions: Vec<PresetAction>,
}

pub(crate) trait InterfaceResolver {
    fn resolve(&self, name: &str) -> Result<String, String>;
}

#[derive(Clone, Debug)]
pub(crate) struct OpenWrtEnvironment {
    uci: PathBuf,
    ubus: PathBuf,
    sys_class_net: PathBuf,
}

impl OpenWrtEnvironment {
    pub(crate) fn production() -> Self {
        Self {
            uci: env_path("CAKE_AUTORATE_UCI_BIN", UCI_BIN),
            ubus: env_path("CAKE_AUTORATE_UBUS_BIN", UBUS_BIN),
            sys_class_net: env_path("CAKE_AUTORATE_SYS_CLASS_NET", SYS_CLASS_NET),
        }
    }

    pub(crate) fn read_package(&self, package: &str) -> Result<UciPackage, String> {
        let output = run_command(&self.uci, &["-q", "-X", "show", package], MAX_UCI_OUTPUT)?;
        let text =
            String::from_utf8(output).map_err(|_| format!("{package} UCI output is not UTF-8"))?;
        UciPackage::parse(package, &text)
    }

    fn apply(&self, plan: &PresetPlan) -> Result<(), String> {
        let Some(batch) = canonical_batch(plan)? else {
            return Ok(());
        };
        self.run_uci_batch(&batch)
    }

    pub(crate) fn run_uci_batch(&self, batch: &str) -> Result<(), String> {
        let spec = SpawnSpec {
            program: self.uci.clone(),
            arguments: vec![OsString::from("-q"), OsString::from("batch")],
            environment: Vec::new(),
        };
        let output = run_bounded_command_output_with_input(
            &spec,
            Some(batch.as_bytes()),
            COMMAND_TIMEOUT,
            16 * 1024,
            || false,
            |_| {},
        )?;
        if !output.status.success() {
            return Err(format!(
                "{} batch exited unsuccessfully: {}",
                self.uci.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    }
}

fn canonical_batch(plan: &PresetPlan) -> Result<Option<String>, String> {
    if plan.actions.is_empty() {
        return Ok(None);
    }
    let mut batch = String::new();
    for action in &plan.actions {
        match action {
            PresetAction::Set {
                section,
                option,
                value,
            } => {
                push_batch_line(
                    &mut batch,
                    &format!("set {PACKAGE}.{section}.{option}='{value}'\n"),
                )?;
            }
        }
    }
    push_batch_line(&mut batch, &format!("commit {PACKAGE}\n"))?;
    Ok(Some(batch))
}

impl InterfaceResolver for OpenWrtEnvironment {
    fn resolve(&self, name: &str) -> Result<String, String> {
        if !safe_interface(name) {
            return Err("configured interface name is unsafe".to_string());
        }
        if self.sys_class_net.join(name).exists() {
            return Ok(name.to_string());
        }
        let object = format!("network.interface.{name}");
        let output = match run_command(&self.ubus, &["call", &object, "status"], MAX_UBUS_OUTPUT) {
            Ok(output) => output,
            Err(_) => return Ok(name.to_string()),
        };
        let status = match String::from_utf8(output) {
            Ok(status) => status,
            Err(_) => return Ok(name.to_string()),
        };
        for key in ["l3_device", "device"] {
            let Some(device) = json_string_value(&status, key) else {
                continue;
            };
            if safe_interface(&device) && self.sys_class_net.join(&device).exists() {
                return Ok(device);
            }
        }
        Ok(name.to_string())
    }
}

pub(crate) fn run_sync_presets<I>(mut arguments: I) -> Result<String, String>
where
    I: Iterator<Item = String>,
{
    if arguments.next().is_some() {
        return Err("--sync-presets does not accept arguments".to_string());
    }
    let environment = OpenWrtEnvironment::production();
    let mut package = environment.read_package(PACKAGE)?;
    let plan = plan_presets(&mut package, &environment)?;
    environment.apply(&plan)?;
    Ok(format!(
        "{{\"schema_version\":1,\"changed\":{},\"mutations\":{}}}\n",
        if plan.actions.is_empty() {
            "false"
        } else {
            "true"
        },
        plan.actions.len()
    ))
}

fn plan_presets(
    package: &mut UciPackage,
    resolver: &impl InterfaceResolver,
) -> Result<PresetPlan, String> {
    let mut plan = PresetPlan::default();
    let instances = instance_names(package);

    for section in &instances {
        sync_interface_preset(package, &mut plan, resolver, section)?;
    }
    for section in &instances {
        sync_rate_preset(package, &mut plan, section)?;
    }
    Ok(plan)
}

fn instance_names(package: &UciPackage) -> Vec<String> {
    package
        .sections
        .iter()
        .filter(|(_, section)| section.section_type == "cake_autorate")
        .map(|(name, _)| name.clone())
        .collect()
}

fn sync_interface_preset(
    package: &mut UciPackage,
    plan: &mut PresetPlan,
    resolver: &impl InterfaceResolver,
    section: &str,
) -> Result<(), String> {
    if !bool_option(option(package, section, "auto_interface_preset"), true) {
        return Ok(());
    }
    let Some(configured) = ["wan_if", "sqm_interface", "ul_if"]
        .iter()
        .filter_map(|key| option(package, section, key))
        .find(|value| !value.is_empty())
        .map(str::to_string)
    else {
        return Ok(());
    };
    let resolved = resolver.resolve(&configured)?;
    if !safe_interface(&resolved) {
        return Err(format!("resolved interface for {section} is unsafe"));
    }
    let download = format!("ifb4{resolved}");
    if !safe_interface(&download) {
        return Err(format!(
            "derived download interface for {section} is unsafe"
        ));
    }
    set_option(package, plan, section, "wan_if", &resolved)?;
    set_option(package, plan, section, "sqm_interface", &resolved)?;
    set_option(package, plan, section, "ul_if", &resolved)?;
    set_option(package, plan, section, "dl_if", &download)
}

fn sync_rate_preset(
    package: &mut UciPackage,
    plan: &mut PresetPlan,
    section: &str,
) -> Result<(), String> {
    if bool_option(option(package, section, "manual_rate_limits"), false) {
        return Ok(());
    }
    sync_direction_rates(package, plan, section, "dl")?;
    sync_direction_rates(package, plan, section, "ul")
}

fn sync_direction_rates(
    package: &mut UciPackage,
    plan: &mut PresetPlan,
    section: &str,
    direction: &str,
) -> Result<(), String> {
    let sqm = format!(
        "sqm_{}",
        if direction == "dl" {
            "download"
        } else {
            "upload"
        }
    );
    let base = format!("base_{direction}_shaper_rate_kbps");
    let maximum = format!("max_{direction}_shaper_rate_kbps");
    let minimum = format!("min_{direction}_shaper_rate_kbps");
    let rate = option(package, section, &sqm)
        .filter(|value| !value.is_empty())
        .or_else(|| option(package, section, &base).filter(|value| !value.is_empty()))
        .map(str::to_string);
    let Some(rate) = rate else {
        return Ok(());
    };
    set_if_empty(package, plan, section, &base, &rate)?;
    set_if_empty(package, plan, section, &maximum, &rate)?;
    set_if_empty(package, plan, section, &minimum, &half_rate(&rate))
}

fn half_rate(value: &str) -> String {
    value
        .parse::<u64>()
        .map(|number| (number / 2 + number % 2).to_string())
        .unwrap_or_else(|_| value.to_string())
}

fn bool_option(value: Option<&str>, default: bool) -> bool {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("1" | "true" | "yes" | "on" | "enabled") => true,
        Some("0" | "false" | "no" | "off" | "disabled") => false,
        _ => default,
    }
}

fn option<'a>(package: &'a UciPackage, section: &str, option: &str) -> Option<&'a str> {
    package
        .sections
        .get(section)
        .and_then(|value| value.options.get(option))
        .map(String::as_str)
}

fn set_if_empty(
    package: &mut UciPackage,
    plan: &mut PresetPlan,
    section: &str,
    option_name: &str,
    value: &str,
) -> Result<(), String> {
    if value.is_empty()
        || option(package, section, option_name).is_some_and(|item| !item.is_empty())
    {
        return Ok(());
    }
    set_option(package, plan, section, option_name, value)
}

fn set_option(
    package: &mut UciPackage,
    plan: &mut PresetPlan,
    section: &str,
    option_name: &str,
    value: &str,
) -> Result<(), String> {
    validate_mutation(section, option_name, Some(value))?;
    let current = package
        .sections
        .get_mut(section)
        .ok_or_else(|| format!("UCI section {section} disappeared while planning"))?;
    if current
        .options
        .get(option_name)
        .is_some_and(|item| item == value)
    {
        return Ok(());
    }
    current
        .options
        .insert(option_name.to_string(), value.to_string());
    push_action(
        plan,
        PresetAction::Set {
            section: section.to_string(),
            option: option_name.to_string(),
            value: value.to_string(),
        },
    )
}

fn validate_mutation(section: &str, option_name: &str, value: Option<&str>) -> Result<(), String> {
    if !safe_name(section) || !safe_name(option_name) {
        return Err("preset mutation contains an unsafe UCI path".to_string());
    }
    if let Some(value) = value {
        if value.len() > MAX_VALUE_BYTES
            || value.chars().any(char::is_control)
            || value.contains('\'')
            || value.contains('\\')
        {
            return Err("preset mutation contains an unsafe UCI value".to_string());
        }
    }
    Ok(())
}

fn push_action(plan: &mut PresetPlan, action: PresetAction) -> Result<(), String> {
    if plan.actions.len() >= MAX_ACTIONS {
        return Err("preset mutation plan exceeds its action limit".to_string());
    }
    plan.actions.push(action);
    Ok(())
}

fn push_batch_line(batch: &mut String, line: &str) -> Result<(), String> {
    if batch.len().saturating_add(line.len()) > MAX_BATCH_BYTES {
        return Err("preset UCI batch exceeds its byte limit".to_string());
    }
    batch.push_str(line);
    Ok(())
}

fn run_command(program: &Path, arguments: &[&str], limit: usize) -> Result<Vec<u8>, String> {
    let spec = SpawnSpec {
        program: program.to_path_buf(),
        arguments: arguments.iter().map(OsString::from).collect(),
        environment: Vec::new(),
    };
    let output = run_bounded_command_output(&spec, COMMAND_TIMEOUT, limit, || false)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "{} exited unsuccessfully: {}",
            program.display(),
            stderr.trim()
        ));
    }
    Ok(output.stdout)
}

fn env_path(name: &str, default: &str) -> PathBuf {
    env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs;

    #[derive(Default)]
    struct FakeResolver {
        devices: BTreeMap<String, String>,
    }

    impl InterfaceResolver for FakeResolver {
        fn resolve(&self, name: &str) -> Result<String, String> {
            Ok(self
                .devices
                .get(name)
                .cloned()
                .unwrap_or_else(|| name.to_string()))
        }
    }

    fn parse(input: &str) -> UciPackage {
        UciPackage::parse(PACKAGE, input).unwrap()
    }

    #[test]
    fn interface_and_rate_presets_are_exact_and_idempotent() {
        let mut package = parse(
            "cake-autorate.wan=cake_autorate\n\
             cake-autorate.wan.wan_if='wan'\n\
             cake-autorate.wan.sqm_download='1001'\n\
             cake-autorate.wan.sqm_upload='200'\n",
        );
        let resolver = FakeResolver {
            devices: BTreeMap::from([("wan".to_string(), "wwan0".to_string())]),
        };
        let plan = plan_presets(&mut package, &resolver).unwrap();
        assert_eq!(option(&package, "wan", "wan_if"), Some("wwan0"));
        assert_eq!(option(&package, "wan", "sqm_interface"), Some("wwan0"));
        assert_eq!(option(&package, "wan", "ul_if"), Some("wwan0"));
        assert_eq!(option(&package, "wan", "dl_if"), Some("ifb4wwan0"));
        assert_eq!(
            option(&package, "wan", "base_dl_shaper_rate_kbps"),
            Some("1001")
        );
        assert_eq!(
            option(&package, "wan", "min_dl_shaper_rate_kbps"),
            Some("501")
        );
        assert_eq!(
            option(&package, "wan", "max_ul_shaper_rate_kbps"),
            Some("200")
        );
        assert!(!plan.actions.is_empty());
        assert!(plan_presets(&mut package, &resolver)
            .unwrap()
            .actions
            .is_empty());
    }

    #[test]
    fn manual_rates_are_not_rewritten() {
        let mut package = parse(
            "cake-autorate.wan=cake_autorate\n\
             cake-autorate.wan.manual_rate_limits='1'\n\
             cake-autorate.wan.sqm_download='1000'\n",
        );
        let plan = plan_presets(&mut package, &FakeResolver::default()).unwrap();
        assert_eq!(plan.actions.len(), 0);
        assert_eq!(option(&package, "wan", "base_dl_shaper_rate_kbps"), None);
    }

    #[test]
    fn existing_rate_boundaries_are_preserved() {
        let mut package = parse(
            "cake-autorate.wan=cake_autorate\n\
             cake-autorate.wan.sqm_download='1000'\n\
             cake-autorate.wan.base_dl_shaper_rate_kbps='900'\n\
             cake-autorate.wan.min_dl_shaper_rate_kbps='300'\n",
        );
        plan_presets(&mut package, &FakeResolver::default()).unwrap();
        assert_eq!(
            option(&package, "wan", "base_dl_shaper_rate_kbps"),
            Some("900")
        );
        assert_eq!(
            option(&package, "wan", "min_dl_shaper_rate_kbps"),
            Some("300")
        );
        assert_eq!(
            option(&package, "wan", "max_dl_shaper_rate_kbps"),
            Some("1000")
        );
    }

    #[test]
    fn cli_rejects_extra_arguments_before_touching_openwrt() {
        assert!(run_sync_presets(vec!["extra".to_string()].into_iter()).is_err());
    }

    #[test]
    fn bool_and_half_rate_match_the_stable_preset_contract() {
        assert!(bool_option(Some("enabled"), false));
        assert!(!bool_option(Some("garbage"), false));
        assert!(bool_option(Some("garbage"), true));
        assert_eq!(half_rate("1"), "1");
        assert_eq!(half_rate("1001"), "501");
        assert_eq!(half_rate("custom"), "custom");
    }

    #[test]
    fn mutations_are_one_canonical_commit_last_uci_batch() {
        let plan = PresetPlan {
            actions: vec![PresetAction::Set {
                section: "wan".to_string(),
                option: "wan_if".to_string(),
                value: "wwan0".to_string(),
            }],
        };
        assert_eq!(
            canonical_batch(&plan).unwrap().unwrap(),
            "set cake-autorate.wan.wan_if='wwan0'\n\
             commit cake-autorate\n"
        );
        assert_eq!(canonical_batch(&PresetPlan::default()).unwrap(), None);
    }

    #[test]
    fn resolver_rejects_unsafe_names_before_any_lookup() {
        let environment = OpenWrtEnvironment {
            uci: PathBuf::from(UCI_BIN),
            ubus: PathBuf::from(UBUS_BIN),
            sys_class_net: PathBuf::from(SYS_CLASS_NET),
        };
        assert!(environment.resolve("../wan").is_err());
    }

    #[test]
    fn fixed_system_paths_are_absolute() {
        for path in [UCI_BIN, UBUS_BIN, SYS_CLASS_NET] {
            assert!(Path::new(path).is_absolute());
        }
        assert!(fs::metadata("/").is_ok());
    }
}
