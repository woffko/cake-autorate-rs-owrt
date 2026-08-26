//! Deterministic projection of CAKE Autorate intent into managed SQM sections.
//!
//! The read-only mode is an upgrade oracle.  Apply uses the same pure planner,
//! requires unchanged input snapshots, commits one bounded UCI batch, and then
//! proves the exact same scope is idempotent before publishing runtime state.

use super::runtime_health::{safe_interface, safe_name, UciPackage, UciSection};
use super::service_config::{InterfaceResolver, OpenWrtEnvironment};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const CAKE_PACKAGE: &str = "cake-autorate";
const SQM_PACKAGE: &str = "sqm";
const DEFAULT_CONFLICT_PATH: &str = "/var/run/cake-autorate/sqm-conflicts";
const MAX_ACTIONS: usize = 4096;
const MAX_BATCH_BYTES: usize = 256 * 1024;
const MAX_VALUE_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProjectionScope {
    All,
    Instance(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SqmAction {
    AddSection {
        section: String,
    },
    DeleteSection {
        section: String,
    },
    Set {
        section: String,
        option: String,
        value: String,
    },
    Delete {
        section: String,
        option: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SqmProjectionPlan {
    actions: Vec<SqmAction>,
    managed: bool,
    interfaces: BTreeSet<String>,
    ingress_interfaces: BTreeSet<String>,
    conflicts: BTreeSet<String>,
}

impl SqmProjectionPlan {
    pub(crate) fn is_managed(&self) -> bool {
        self.managed
    }

    #[cfg(feature = "calibration")]
    pub(crate) fn interfaces(&self) -> &BTreeSet<String> {
        &self.interfaces
    }

    pub(crate) fn ingress_interfaces(&self) -> &BTreeSet<String> {
        &self.ingress_interfaces
    }

    pub(crate) fn conflicts(&self) -> &BTreeSet<String> {
        &self.conflicts
    }

    #[cfg(test)]
    pub(crate) fn test_summary(
        managed: bool,
        interfaces: BTreeSet<String>,
        ingress_interfaces: BTreeSet<String>,
        conflicts: BTreeSet<String>,
    ) -> Self {
        Self {
            actions: Vec::new(),
            managed,
            interfaces,
            ingress_interfaces,
            conflicts,
        }
    }
}

pub(crate) fn run_sqm_project<I>(mut arguments: I) -> Result<String, String>
where
    I: Iterator<Item = String>,
{
    let mode = arguments
        .next()
        .ok_or_else(|| "--sqm-project requires dry-run or apply".to_string())?;
    let scope = parse_scope(arguments.next().as_deref().unwrap_or("all"))?;
    if arguments.next().is_some() {
        return Err("--sqm-project received too many arguments".to_string());
    }
    match mode.as_str() {
        "dry-run" => {
            let environment = OpenWrtEnvironment::production();
            let cake = environment.read_package(CAKE_PACKAGE)?;
            let mut sqm = environment.read_package(SQM_PACKAGE)?;
            let plan = plan_projection(&cake, &mut sqm, &environment, &scope)?;
            Ok(render_dry_run(&scope, &plan))
        }
        "apply" => apply_sqm_projection(scope).map(|plan| render_apply_result(&plan)),
        _ => Err("--sqm-project mode must be dry-run or apply".to_string()),
    }
}

/// Apply one exact projection transaction and return its post-verified runtime
/// summary.  Lifecycle code consumes this typed result directly; it must never
/// parse the human-facing CLI line emitted by `run_sqm_project`.
pub(crate) fn apply_sqm_projection(scope: ProjectionScope) -> Result<SqmProjectionPlan, String> {
    let environment = OpenWrtEnvironment::production();
    let cake = environment.read_package(CAKE_PACKAGE)?;
    let mut sqm = environment.read_package(SQM_PACKAGE)?;
    let sqm_original = sqm.clone();
    let plan = plan_projection(&cake, &mut sqm, &environment, &scope)?;
    let cake_attested = environment.read_package(CAKE_PACKAGE)?;
    let sqm_attested = environment.read_package(SQM_PACKAGE)?;
    if cake_attested != cake || sqm_attested != sqm_original {
        return Err("SQM projection inputs changed before commit".to_string());
    }
    if let Some(batch) = canonical_batch(&plan)? {
        environment.run_uci_batch(&batch)?;
    }
    let cake_after = environment.read_package(CAKE_PACKAGE)?;
    let mut sqm_after = environment.read_package(SQM_PACKAGE)?;
    if cake_after != cake {
        return Err("cake-autorate UCI changed during SQM projection".to_string());
    }
    let verified = plan_projection(&cake_after, &mut sqm_after, &environment, &scope)?;
    if !verified.actions.is_empty() {
        return Err("SQM projection postcondition is not idempotent".to_string());
    }
    publish_conflicts(&conflict_path(), &verified.conflicts)?;
    Ok(verified)
}

fn parse_scope(value: &str) -> Result<ProjectionScope, String> {
    if value == "all" {
        Ok(ProjectionScope::All)
    } else if safe_name(value) {
        Ok(ProjectionScope::Instance(value.to_string()))
    } else {
        Err("--sqm-project instance is unsafe".to_string())
    }
}

pub(crate) fn plan_projection(
    cake: &UciPackage,
    sqm: &mut UciPackage,
    resolver: &impl InterfaceResolver,
    scope: &ProjectionScope,
) -> Result<SqmProjectionPlan, String> {
    let mut plan = SqmProjectionPlan::default();
    let mut cache = BTreeMap::new();
    plan.conflicts = detect_conflicts(cake, resolver, &mut cache)?;
    let instances = scoped_instances(cake, scope)?;
    for instance in instances {
        project_instance(cake, sqm, resolver, &mut cache, &mut plan, &instance)?;
    }
    Ok(plan)
}

fn scoped_instances(cake: &UciPackage, scope: &ProjectionScope) -> Result<Vec<String>, String> {
    match scope {
        ProjectionScope::All => Ok(cake
            .sections
            .iter()
            .filter(|(_, section)| section.section_type == "cake_autorate")
            .map(|(name, _)| name.clone())
            .collect()),
        ProjectionScope::Instance(name) => match cake.sections.get(name) {
            Some(section) if section.section_type == "cake_autorate" => Ok(vec![name.clone()]),
            _ => Err(format!("cake-autorate instance {name} does not exist")),
        },
    }
}

fn detect_conflicts(
    cake: &UciPackage,
    resolver: &impl InterfaceResolver,
    cache: &mut BTreeMap<String, String>,
) -> Result<BTreeSet<String>, String> {
    let mut by_target: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (name, section) in &cake.sections {
        if section.section_type != "cake_autorate"
            || !bool_value(section.options.get("enabled").map(String::as_str), false)
            || !bool_value(section.options.get("manage_sqm").map(String::as_str), true)
        {
            continue;
        }
        let sqm_enabled = bool_value(
            section.options.get("sqm_enabled").map(String::as_str),
            bool_value(section.options.get("enabled").map(String::as_str), false),
        );
        if !sqm_enabled
            || section
                .options
                .get("sqm_direction_mode")
                .map(String::as_str)
                .unwrap_or("both")
                == "off"
        {
            continue;
        }
        let Some(target) = first_nonempty(section, &["sqm_interface", "ul_if", "wan_if"]) else {
            continue;
        };
        let target = resolve_cached(resolver, cache, target)?;
        by_target.entry(target).or_default().push(name.clone());
    }
    let mut conflicts = BTreeSet::new();
    for instances in by_target.values().filter(|items| items.len() > 1) {
        conflicts.extend(instances.iter().cloned());
    }
    Ok(conflicts)
}

fn project_instance(
    cake: &UciPackage,
    sqm: &mut UciPackage,
    resolver: &impl InterfaceResolver,
    cache: &mut BTreeMap<String, String>,
    plan: &mut SqmProjectionPlan,
    instance: &str,
) -> Result<(), String> {
    let section = cake
        .sections
        .get(instance)
        .ok_or_else(|| format!("cake-autorate instance {instance} disappeared"))?;
    let manage = bool_value(section.options.get("manage_sqm").map(String::as_str), true);
    let sqm_section = section
        .options
        .get("sqm_section")
        .filter(|value| !value.is_empty())
        .cloned()
        .unwrap_or_else(|| format!("cake_{instance}"));
    if !safe_name(&sqm_section) {
        return Err(format!("instance {instance} has an unsafe SQM section"));
    }
    if !manage {
        cleanup_stale(sqm, plan, instance, None)?;
        return Ok(());
    }

    let enabled = bool_value(section.options.get("enabled").map(String::as_str), false);
    let sqm_enabled = bool_value(
        section.options.get("sqm_enabled").map(String::as_str),
        enabled,
    );
    let direction = section
        .options
        .get("sqm_direction_mode")
        .map(String::as_str)
        .unwrap_or("both");
    if !matches!(direction, "both" | "upload_only" | "download_only" | "off") {
        return Err(format!(
            "instance {instance} has invalid SQM direction mode {direction}"
        ));
    }
    if plan.conflicts.contains(instance) {
        cleanup_stale(sqm, plan, instance, None)?;
        return Ok(());
    }

    cleanup_stale(sqm, plan, instance, Some(&sqm_section))?;
    ensure_queue(sqm, plan, &sqm_section)?;
    set_option(sqm, plan, &sqm_section, "_cake_autorate_managed", instance)?;
    let active = enabled && sqm_enabled && direction != "off";
    set_option(
        sqm,
        plan,
        &sqm_section,
        "enabled",
        if active { "1" } else { "0" },
    )?;

    let configured = first_nonempty(section, &["sqm_interface", "ul_if"])
        .ok_or_else(|| format!("instance {instance} has no SQM interface"))?;
    let target = resolve_cached(resolver, cache, configured)?;
    set_option(sqm, plan, &sqm_section, "interface", &target)?;
    if !active {
        plan.managed = true;
        return Ok(());
    }
    plan.interfaces.insert(target.clone());
    if direction != "upload_only" {
        plan.ingress_interfaces.insert(target.clone());
    }
    disable_unmanaged_conflicts(sqm, resolver, cache, plan, &sqm_section, &target)?;

    let download = first_nonempty(section, &["sqm_download", "base_dl_shaper_rate_kbps"])
        .ok_or_else(|| format!("instance {instance} has no download rate"))?;
    let upload = first_nonempty(section, &["sqm_upload", "base_ul_shaper_rate_kbps"])
        .ok_or_else(|| format!("instance {instance} has no upload rate"))?;
    set_option(
        sqm,
        plan,
        &sqm_section,
        "download",
        if direction == "upload_only" {
            "0"
        } else {
            download
        },
    )?;
    set_option(
        sqm,
        plan,
        &sqm_section,
        "upload",
        if direction == "download_only" {
            "0"
        } else {
            upload
        },
    )?;

    for (source, target, default) in FIXED_OPTIONS {
        let value = section
            .options
            .get(*source)
            .map(String::as_str)
            .unwrap_or(default);
        set_option(sqm, plan, &sqm_section, target, value)?;
    }
    for (source, target) in OPTIONAL_OPTIONS {
        let value = section
            .options
            .get(*source)
            .map(String::as_str)
            .unwrap_or_default();
        if value.is_empty() {
            delete_option(sqm, plan, &sqm_section, target)?;
        } else {
            set_option(sqm, plan, &sqm_section, target, value)?;
        }
    }
    plan.managed = true;
    Ok(())
}

const FIXED_OPTIONS: &[(&str, &str, &str)] = &[
    ("sqm_debug_logging", "debug_logging", "0"),
    ("sqm_verbosity", "verbosity", "5"),
    ("sqm_qdisc", "qdisc", "cake"),
    ("sqm_script", "script", "piece_of_cake.qos"),
    ("sqm_qdisc_advanced", "qdisc_advanced", "0"),
    ("sqm_squash_dscp", "squash_dscp", "1"),
    ("sqm_squash_ingress", "squash_ingress", "1"),
    ("sqm_ingress_ecn", "ingress_ecn", "ECN"),
    ("sqm_egress_ecn", "egress_ecn", "NOECN"),
    (
        "sqm_qdisc_really_really_advanced",
        "qdisc_really_really_advanced",
        "0",
    ),
    ("sqm_linklayer", "linklayer", "none"),
    ("sqm_overhead", "overhead", "0"),
    ("sqm_linklayer_advanced", "linklayer_advanced", "0"),
    ("sqm_tcMTU", "tcMTU", "2047"),
    ("sqm_tcTSIZE", "tcTSIZE", "128"),
    ("sqm_tcMPU", "tcMPU", "0"),
    (
        "sqm_linklayer_adaptation_mechanism",
        "linklayer_adaptation_mechanism",
        "default",
    ),
];

const OPTIONAL_OPTIONS: &[(&str, &str)] = &[
    ("sqm_ilimit", "ilimit"),
    ("sqm_elimit", "elimit"),
    ("sqm_itarget", "itarget"),
    ("sqm_etarget", "etarget"),
    ("sqm_iqdisc_opts", "iqdisc_opts"),
    ("sqm_eqdisc_opts", "eqdisc_opts"),
];

fn disable_unmanaged_conflicts(
    sqm: &mut UciPackage,
    resolver: &impl InterfaceResolver,
    cache: &mut BTreeMap<String, String>,
    plan: &mut SqmProjectionPlan,
    keep: &str,
    target: &str,
) -> Result<(), String> {
    let candidates: Vec<String> = sqm
        .sections
        .iter()
        .filter(|(name, section)| *name != keep && section.section_type == "queue")
        .map(|(name, _)| name.clone())
        .collect();
    for candidate in candidates {
        let section = sqm.sections.get(&candidate).expect("candidate exists");
        if section.options.get("enabled").map(String::as_str) != Some("1") {
            continue;
        }
        let Some(interface) = section
            .options
            .get("interface")
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        if resolve_cached(resolver, cache, interface)? != target {
            continue;
        }
        if section
            .options
            .get("_cake_autorate_managed")
            .is_some_and(|value| !value.is_empty())
        {
            return Err(format!(
                "managed SQM sections {keep} and {candidate} target {target}"
            ));
        }
        set_option(sqm, plan, &candidate, "enabled", "0")?;
        plan.managed = true;
    }
    Ok(())
}

fn cleanup_stale(
    sqm: &mut UciPackage,
    plan: &mut SqmProjectionPlan,
    owner: &str,
    keep: Option<&str>,
) -> Result<(), String> {
    let canonical = format!("cake_{owner}");
    let stale: Vec<String> = sqm
        .sections
        .iter()
        .filter(|(name, section)| {
            Some(name.as_str()) != keep
                && section
                    .options
                    .get("_cake_autorate_managed")
                    .map(String::as_str)
                    == Some(owner)
        })
        .map(|(name, _)| name.clone())
        .collect();
    for section in stale {
        if section == canonical {
            delete_section(sqm, plan, &section)?;
        } else {
            delete_option(sqm, plan, &section, "_cake_autorate_managed")?;
        }
        plan.managed = true;
    }
    Ok(())
}

fn ensure_queue(
    sqm: &mut UciPackage,
    plan: &mut SqmProjectionPlan,
    section: &str,
) -> Result<(), String> {
    match sqm.sections.get(section) {
        Some(value) if value.section_type == "queue" => Ok(()),
        Some(_) => Err(format!("SQM section {section} exists with a foreign type")),
        None => {
            push_action(
                plan,
                SqmAction::AddSection {
                    section: section.to_string(),
                },
            )?;
            sqm.sections.insert(
                section.to_string(),
                UciSection {
                    section_type: "queue".to_string(),
                    options: BTreeMap::new(),
                },
            );
            Ok(())
        }
    }
}

fn set_option(
    sqm: &mut UciPackage,
    plan: &mut SqmProjectionPlan,
    section: &str,
    option: &str,
    value: &str,
) -> Result<(), String> {
    validate_path_value(section, option, Some(value))?;
    let target = sqm
        .sections
        .get_mut(section)
        .ok_or_else(|| format!("SQM section {section} is absent"))?;
    if target.options.get(option).is_some_and(|item| item == value) {
        return Ok(());
    }
    target.options.insert(option.to_string(), value.to_string());
    push_action(
        plan,
        SqmAction::Set {
            section: section.to_string(),
            option: option.to_string(),
            value: value.to_string(),
        },
    )
}

fn delete_option(
    sqm: &mut UciPackage,
    plan: &mut SqmProjectionPlan,
    section: &str,
    option: &str,
) -> Result<(), String> {
    validate_path_value(section, option, None)?;
    let Some(target) = sqm.sections.get_mut(section) else {
        return Ok(());
    };
    if target.options.remove(option).is_none() {
        return Ok(());
    }
    push_action(
        plan,
        SqmAction::Delete {
            section: section.to_string(),
            option: option.to_string(),
        },
    )
}

fn delete_section(
    sqm: &mut UciPackage,
    plan: &mut SqmProjectionPlan,
    section: &str,
) -> Result<(), String> {
    if !safe_name(section) {
        return Err("SQM section deletion contains an unsafe name".to_string());
    }
    if sqm.sections.remove(section).is_none() {
        return Ok(());
    }
    push_action(
        plan,
        SqmAction::DeleteSection {
            section: section.to_string(),
        },
    )
}

fn push_action(plan: &mut SqmProjectionPlan, action: SqmAction) -> Result<(), String> {
    if plan.actions.len() >= MAX_ACTIONS {
        return Err("SQM projection exceeds its action limit".to_string());
    }
    plan.actions.push(action);
    Ok(())
}

fn validate_path_value(section: &str, option: &str, value: Option<&str>) -> Result<(), String> {
    if !safe_name(section) || !safe_name(option) {
        return Err("SQM projection contains an unsafe UCI path".to_string());
    }
    if let Some(value) = value {
        if value.len() > MAX_VALUE_BYTES
            || value.chars().any(char::is_control)
            || value.contains('\'')
            || value.contains('\\')
        {
            return Err("SQM projection contains an unsafe UCI value".to_string());
        }
    }
    Ok(())
}

fn canonical_batch(plan: &SqmProjectionPlan) -> Result<Option<String>, String> {
    canonical_batch_for_package(plan, SQM_PACKAGE)
}

pub(crate) fn canonical_batch_for_package(
    plan: &SqmProjectionPlan,
    package: &str,
) -> Result<Option<String>, String> {
    if !safe_name(package) {
        return Err("SQM projection package alias is unsafe".to_string());
    }
    if plan.actions.is_empty() {
        return Ok(None);
    }
    let mut output = String::new();
    for action in &plan.actions {
        let line = match action {
            SqmAction::AddSection { section } => format!("set {package}.{section}=queue\n"),
            SqmAction::DeleteSection { section } => format!("delete {package}.{section}\n"),
            SqmAction::Set {
                section,
                option,
                value,
            } => format!("set {package}.{section}.{option}='{value}'\n"),
            SqmAction::Delete { section, option } => {
                format!("delete {package}.{section}.{option}\n")
            }
        };
        push_batch(&mut output, &line)?;
    }
    push_batch(&mut output, &format!("commit {package}\n"))?;
    Ok(Some(output))
}

fn push_batch(output: &mut String, line: &str) -> Result<(), String> {
    if output.len().saturating_add(line.len()) > MAX_BATCH_BYTES {
        return Err("SQM projection batch exceeds its byte limit".to_string());
    }
    output.push_str(line);
    Ok(())
}

fn first_nonempty<'a>(section: &'a UciSection, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .filter_map(|name| section.options.get(*name))
        .map(String::as_str)
        .find(|value| !value.is_empty())
}

fn bool_value(value: Option<&str>, default: bool) -> bool {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("1" | "true" | "yes" | "on" | "enabled") => true,
        Some("0" | "false" | "no" | "off" | "disabled") => false,
        _ => default,
    }
}

fn resolve_cached(
    resolver: &impl InterfaceResolver,
    cache: &mut BTreeMap<String, String>,
    name: &str,
) -> Result<String, String> {
    if let Some(value) = cache.get(name) {
        return Ok(value.clone());
    }
    let value = resolver.resolve(name)?;
    if !safe_interface(&value) || value.contains(',') || value.contains(char::is_whitespace) {
        return Err("resolved SQM interface is unsafe".to_string());
    }
    cache.insert(name.to_string(), value.clone());
    Ok(value)
}

fn render_apply_result(plan: &SqmProjectionPlan) -> String {
    format!(
        "sqm-project-v1 {} {} {}\n",
        if plan.managed { 1 } else { 0 },
        csv_or_dash(&plan.interfaces),
        csv_or_dash(&plan.ingress_interfaces)
    )
}

fn csv_or_dash(values: &BTreeSet<String>) -> String {
    if values.is_empty() {
        "-".to_string()
    } else {
        values.iter().cloned().collect::<Vec<_>>().join(",")
    }
}

fn render_dry_run(scope: &ProjectionScope, plan: &SqmProjectionPlan) -> String {
    let actions: Vec<String> = plan
        .actions
        .iter()
        .map(|action| match action {
            SqmAction::AddSection { section } => format!(
                "{{\"kind\":\"add_section\",\"section\":\"{}\"}}",
                crate::json_escape(section)
            ),
            SqmAction::DeleteSection { section } => format!(
                "{{\"kind\":\"delete_section\",\"section\":\"{}\"}}",
                crate::json_escape(section)
            ),
            SqmAction::Set {
                section,
                option,
                value,
            } => format!(
                "{{\"kind\":\"set\",\"section\":\"{}\",\"option\":\"{}\",\"value\":\"{}\"}}",
                crate::json_escape(section),
                crate::json_escape(option),
                crate::json_escape(value)
            ),
            SqmAction::Delete { section, option } => format!(
                "{{\"kind\":\"delete\",\"section\":\"{}\",\"option\":\"{}\"}}",
                crate::json_escape(section),
                crate::json_escape(option)
            ),
        })
        .collect();
    let scope = match scope {
        ProjectionScope::All => "all",
        ProjectionScope::Instance(value) => value,
    };
    format!(
        "{{\"schema_version\":1,\"mode\":\"dry-run\",\"scope\":\"{}\",\"managed\":{},\"interfaces\":\"{}\",\"ingress_interfaces\":\"{}\",\"conflicts\":\"{}\",\"actions\":[{}]}}\n",
        crate::json_escape(scope),
        if plan.managed { "true" } else { "false" },
        crate::json_escape(&csv_or_dash(&plan.interfaces)),
        crate::json_escape(&csv_or_dash(&plan.ingress_interfaces)),
        crate::json_escape(&csv_or_dash(&plan.conflicts)),
        actions.join(",")
    )
}

fn conflict_path() -> PathBuf {
    env::var_os("CAKE_AUTORATE_SQM_CONFLICT_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFLICT_PATH))
}

fn publish_conflicts(path: &Path, conflicts: &BTreeSet<String>) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "SQM conflict path has no parent".to_string())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("unable to create SQM conflict directory: {error}"))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_string())?
        .as_nanos();
    let temp = parent.join(format!(".sqm-conflicts-{}-{nonce}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)
        .map_err(|error| format!("unable to create SQM conflict snapshot: {error}"))?;
    let result = (|| {
        for conflict in conflicts {
            writeln!(file, "{conflict}")
                .map_err(|error| format!("unable to write SQM conflict snapshot: {error}"))?;
        }
        file.sync_all()
            .map_err(|error| format!("unable to sync SQM conflict snapshot: {error}"))?;
        fs::rename(&temp, path)
            .map_err(|error| format!("unable to publish SQM conflict snapshot: {error}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeResolver(BTreeMap<String, String>);

    impl InterfaceResolver for FakeResolver {
        fn resolve(&self, name: &str) -> Result<String, String> {
            Ok(self
                .0
                .get(name)
                .cloned()
                .unwrap_or_else(|| name.to_string()))
        }
    }

    fn parse(package: &str, text: &str) -> UciPackage {
        UciPackage::parse(package, text).unwrap()
    }

    fn base_cake() -> UciPackage {
        parse(
            CAKE_PACKAGE,
            "cake-autorate.wan=cake_autorate\n\
             cake-autorate.wan.enabled='1'\n\
             cake-autorate.wan.sqm_enabled='1'\n\
             cake-autorate.wan.manage_sqm='1'\n\
             cake-autorate.wan.sqm_interface='wan'\n\
             cake-autorate.wan.sqm_download='100000'\n\
             cake-autorate.wan.sqm_upload='20000'\n",
        )
    }

    #[test]
    fn both_projection_is_exact_and_second_plan_is_empty() {
        let cake = base_cake();
        let mut sqm = UciPackage::default();
        let resolver = FakeResolver(BTreeMap::from([("wan".to_string(), "wwan0".to_string())]));
        let plan = plan_projection(&cake, &mut sqm, &resolver, &ProjectionScope::All).unwrap();
        assert!(plan.managed);
        assert_eq!(plan.interfaces, BTreeSet::from(["wwan0".to_string()]));
        assert_eq!(
            plan.ingress_interfaces,
            BTreeSet::from(["wwan0".to_string()])
        );
        let section = sqm.sections.get("cake_wan").unwrap();
        assert_eq!(
            section.options.get("download").map(String::as_str),
            Some("100000")
        );
        assert_eq!(
            section.options.get("upload").map(String::as_str),
            Some("20000")
        );
        assert_eq!(
            section.options.get("qdisc").map(String::as_str),
            Some("cake")
        );
        assert!(
            plan_projection(&cake, &mut sqm, &resolver, &ProjectionScope::All)
                .unwrap()
                .actions
                .is_empty()
        );
    }

    #[test]
    fn directional_modes_zero_only_the_bypassed_direction() {
        for (mode, download, upload, ingress) in [
            ("download_only", "100000", "0", true),
            ("upload_only", "0", "20000", false),
        ] {
            let mut cake = base_cake();
            cake.sections
                .get_mut("wan")
                .unwrap()
                .options
                .insert("sqm_direction_mode".to_string(), mode.to_string());
            let mut sqm = UciPackage::default();
            let plan = plan_projection(
                &cake,
                &mut sqm,
                &FakeResolver::default(),
                &ProjectionScope::All,
            )
            .unwrap();
            let section = sqm.sections.get("cake_wan").unwrap();
            assert_eq!(
                section.options.get("download").map(String::as_str),
                Some(download)
            );
            assert_eq!(
                section.options.get("upload").map(String::as_str),
                Some(upload)
            );
            assert_eq!(!plan.ingress_interfaces.is_empty(), ingress);
        }
    }

    #[test]
    fn disabled_instance_retains_an_exact_inactive_owner_contract() {
        for (option, value) in [
            ("enabled", "0"),
            ("sqm_enabled", "0"),
            ("sqm_direction_mode", "off"),
        ] {
            let mut cake = base_cake();
            cake.sections
                .get_mut("wan")
                .unwrap()
                .options
                .insert(option.to_string(), value.to_string());
            let mut sqm = UciPackage::default();
            let resolver = FakeResolver(BTreeMap::from([("wan".to_string(), "wwan0".to_string())]));
            let plan = plan_projection(&cake, &mut sqm, &resolver, &ProjectionScope::All).unwrap();
            assert!(plan.managed);
            assert!(plan.interfaces.is_empty());
            assert!(plan.ingress_interfaces.is_empty());
            let section = sqm.sections.get("cake_wan").unwrap();
            assert_eq!(
                section
                    .options
                    .get("_cake_autorate_managed")
                    .map(String::as_str),
                Some("wan")
            );
            assert_eq!(
                section.options.get("enabled").map(String::as_str),
                Some("0")
            );
            assert_eq!(
                section.options.get("interface").map(String::as_str),
                Some("wwan0")
            );
            assert!(
                plan_projection(&cake, &mut sqm, &resolver, &ProjectionScope::All)
                    .unwrap()
                    .actions
                    .is_empty()
            );
        }
    }

    #[test]
    fn disabled_noncanonical_owner_replaces_its_canonical_stale_section() {
        let mut cake = base_cake();
        let controller = cake.sections.get_mut("wan").unwrap();
        controller
            .options
            .insert("enabled".to_string(), "0".to_string());
        controller
            .options
            .insert("sqm_section".to_string(), "reserved_wan".to_string());
        let mut sqm = parse(
            SQM_PACKAGE,
            "sqm.cake_wan=queue\n\
             sqm.cake_wan.enabled='1'\n\
             sqm.cake_wan._cake_autorate_managed='wan'\n\
             sqm.reserved_wan=queue\n\
             sqm.reserved_wan.enabled='1'\n\
             sqm.reserved_wan.interface='old0'\n",
        );
        let resolver = FakeResolver(BTreeMap::from([("wan".to_string(), "wwan0".to_string())]));
        plan_projection(&cake, &mut sqm, &resolver, &ProjectionScope::All).unwrap();
        assert!(!sqm.sections.contains_key("cake_wan"));
        let section = sqm.sections.get("reserved_wan").unwrap();
        assert_eq!(
            section
                .options
                .get("_cake_autorate_managed")
                .map(String::as_str),
            Some("wan")
        );
        assert_eq!(
            section.options.get("enabled").map(String::as_str),
            Some("0")
        );
        assert_eq!(
            section.options.get("interface").map(String::as_str),
            Some("wwan0")
        );
    }

    #[test]
    fn duplicate_active_instances_are_conflicted_and_canonical_stale_queues_are_removed() {
        let mut cake = base_cake();
        cake.sections
            .insert("wanb".to_string(), cake.sections["wan"].clone());
        let mut sqm = parse(
            SQM_PACKAGE,
            "sqm.cake_wan=queue\n\
             sqm.cake_wan.enabled='1'\n\
             sqm.cake_wan._cake_autorate_managed='wan'\n\
             sqm.cake_wanb=queue\n\
             sqm.cake_wanb.enabled='1'\n\
             sqm.cake_wanb._cake_autorate_managed='wanb'\n",
        );
        let plan = plan_projection(
            &cake,
            &mut sqm,
            &FakeResolver::default(),
            &ProjectionScope::All,
        )
        .unwrap();
        assert_eq!(
            plan.conflicts,
            BTreeSet::from(["wan".to_string(), "wanb".to_string()])
        );
        // This is the historical cleanup contract: the canonical generated
        // section is removed entirely when its owner cannot safely run.
        assert!(!sqm.sections.contains_key("cake_wan"));
        assert!(!sqm.sections.contains_key("cake_wanb"));
    }

    #[test]
    fn unmanaged_collision_is_disabled_but_foreign_owner_fails_closed() {
        let cake = base_cake();
        let mut sqm = parse(
            SQM_PACKAGE,
            "sqm.manual=queue\n\
             sqm.manual.enabled='1'\n\
             sqm.manual.interface='wan'\n",
        );
        plan_projection(
            &cake,
            &mut sqm,
            &FakeResolver::default(),
            &ProjectionScope::All,
        )
        .unwrap();
        assert_eq!(sqm.sections["manual"].options["enabled"], "0");

        let mut foreign = parse(
            SQM_PACKAGE,
            "sqm.foreign=queue\n\
             sqm.foreign.enabled='1'\n\
             sqm.foreign.interface='wan'\n\
             sqm.foreign._cake_autorate_managed='other'\n",
        );
        assert!(plan_projection(
            &cake,
            &mut foreign,
            &FakeResolver::default(),
            &ProjectionScope::All
        )
        .is_err());
    }

    #[test]
    fn selected_scope_ignores_unrelated_pending_projection() {
        let mut cake = base_cake();
        let mut wanb = cake.sections["wan"].clone();
        wanb.options
            .insert("sqm_interface".to_string(), "wanb".to_string());
        cake.sections.insert("wanb".to_string(), wanb);
        let mut sqm = UciPackage::default();
        let plan = plan_projection(
            &cake,
            &mut sqm,
            &FakeResolver::default(),
            &ProjectionScope::Instance("wan".to_string()),
        )
        .unwrap();
        assert!(plan.actions.iter().all(|action| match action {
            SqmAction::AddSection { section }
            | SqmAction::DeleteSection { section }
            | SqmAction::Set { section, .. }
            | SqmAction::Delete { section, .. } => section != "cake_wanb",
        }));
    }

    #[test]
    fn optional_values_delete_stale_options_and_batch_commits_last() {
        let cake = base_cake();
        let mut sqm = parse(
            SQM_PACKAGE,
            "sqm.cake_wan=queue\n\
             sqm.cake_wan._cake_autorate_managed='wan'\n\
             sqm.cake_wan.iqdisc_opts='stale'\n",
        );
        let plan = plan_projection(
            &cake,
            &mut sqm,
            &FakeResolver::default(),
            &ProjectionScope::All,
        )
        .unwrap();
        assert_eq!(sqm.sections["cake_wan"].options.get("iqdisc_opts"), None);
        let batch = canonical_batch(&plan).unwrap().unwrap();
        assert!(batch.contains("delete sqm.cake_wan.iqdisc_opts\n"));
        assert!(batch.ends_with("commit sqm\n"));
    }

    #[test]
    fn empty_conflict_publication_replaces_stale_content() {
        let root = env::temp_dir().join(format!("cake-sqm-conflict-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("sqm-conflicts");
        fs::write(&path, "stale\n").unwrap();
        publish_conflicts(&path, &BTreeSet::new()).unwrap();
        assert_eq!(fs::read(&path).unwrap(), Vec::<u8>::new());
        fs::remove_dir_all(root).unwrap();
    }
}
