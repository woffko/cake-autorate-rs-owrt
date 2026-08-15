//! Exact, bounded identity for the managed SQM UCI section.
//!
//! The legacy workers define the SQM fingerprint as SHA-256 over byte-sorted
//! `uci -q show sqm.<section>` lines with one terminating newline.  Keep the
//! same contract during migration, but execute fixed binaries directly and
//! never construct a shell command.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::process::{Command, Stdio};

const MAX_UCI_SHOW_BYTES: usize = 64 * 1024;
const BOOTSTRAP_CONFIG_ABSENCE_DOMAIN_V1: &str = "cake-autorate-bootstrap-uci-absence-config-v1";
const BOOTSTRAP_SQM_ABSENCE_DOMAIN_V1: &str = "cake-autorate-bootstrap-uci-absence-sqm-v1";

/// Positive, full-package UCI absence witness for a future bootstrap request.
///
/// This type deliberately says nothing about live qdiscs, ingress filters,
/// IFBs, hotplug ownership, or any other kernel topology.  It is therefore
/// insufficient for operation admission until the runtime bootstrap slice
/// adds and re-attests those independent safety proofs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BootstrapAbsenceIdentity {
    instance: String,
    planned_sqm_section: String,
    target_interface: String,
    route_fingerprint: String,
    config_fingerprint: String,
    sqm_fingerprint: String,
}

impl BootstrapAbsenceIdentity {
    /// Parse two already-successful full-package `uci show` results without
    /// reading UCI or inspecting live network topology.
    pub fn from_raw(
        instance: &str,
        planned_sqm_section: &str,
        target_interface: &str,
        route_fingerprint: &str,
        cake_package_raw: &[u8],
        sqm_package_raw: &[u8],
    ) -> Result<Self, String> {
        validate_bootstrap_identifier("instance", instance)?;
        validate_bootstrap_identifier("planned SQM section", planned_sqm_section)?;
        validate_bootstrap_target(target_interface)?;
        validate_lower_sha256("route fingerprint", route_fingerprint)?;
        let cake_package =
            parse_bootstrap_uci_package("cake-autorate", cake_package_raw, MAX_UCI_SHOW_BYTES)?;
        let sqm_package = parse_bootstrap_uci_package("sqm", sqm_package_raw, MAX_UCI_SHOW_BYTES)?;
        validate_cake_bootstrap_absence(
            &cake_package,
            instance,
            planned_sqm_section,
            target_interface,
        )?;
        validate_sqm_bootstrap_absence(
            &sqm_package,
            instance,
            planned_sqm_section,
            target_interface,
        )?;

        let config_material = bootstrap_absence_fingerprint_material(
            BOOTSTRAP_CONFIG_ABSENCE_DOMAIN_V1,
            instance,
            planned_sqm_section,
            target_interface,
            route_fingerprint,
            "cake-autorate",
            &cake_package.canonical,
        );
        let sqm_material = bootstrap_absence_fingerprint_material(
            BOOTSTRAP_SQM_ABSENCE_DOMAIN_V1,
            instance,
            planned_sqm_section,
            target_interface,
            route_fingerprint,
            "sqm",
            &sqm_package.canonical,
        );
        Ok(Self {
            instance: instance.to_string(),
            planned_sqm_section: planned_sqm_section.to_string(),
            target_interface: target_interface.to_string(),
            route_fingerprint: route_fingerprint.to_string(),
            config_fingerprint: sha256sum(&config_material)?,
            sqm_fingerprint: sha256sum(&sqm_material)?,
        })
    }

    pub fn config_fingerprint(&self) -> &str {
        &self.config_fingerprint
    }

    pub fn sqm_fingerprint(&self) -> &str {
        &self.sqm_fingerprint
    }

    /// Verify every value copied from this witness into a dormant bootstrap
    /// request.  Keeping the fields private prevents callers from relabelling
    /// one UCI snapshot for another instance, target, route, or fingerprint.
    pub(crate) fn ensure_request_binding(
        &self,
        instance: &str,
        planned_sqm_section: &str,
        target_interface: &str,
        route_fingerprint: &str,
        config_fingerprint: &str,
        sqm_fingerprint: &str,
    ) -> Result<(), String> {
        if self.instance != instance
            || self.planned_sqm_section != planned_sqm_section
            || self.target_interface != target_interface
            || self.route_fingerprint != route_fingerprint
            || self.config_fingerprint != config_fingerprint
            || self.sqm_fingerprint != sqm_fingerprint
        {
            return Err(
                "bootstrap UCI absence witness does not match the request binding".to_string(),
            );
        }
        Ok(())
    }

    /// Compare a freshly reconstructed UCI-only witness with the exact
    /// identity captured earlier.  Callers must still attest kernel topology
    /// separately before this can ever authorize runtime mutation.
    pub fn ensure_exact_reattestation(&self, current: &Self) -> Result<(), String> {
        if self == current {
            Ok(())
        } else {
            Err("bootstrap UCI absence identity changed during re-attestation".to_string())
        }
    }
}

/// Read both complete UCI packages through fixed direct commands and build a
/// UCI-only absence witness. A failed command is never interpreted as absence.
/// This function does not inspect qdiscs or ingress and is not an admission
/// authority by itself.
pub fn attest_bootstrap_uci_absence(
    instance: &str,
    planned_sqm_section: &str,
    target_interface: &str,
    route_fingerprint: &str,
) -> Result<BootstrapAbsenceIdentity, String> {
    let cake_package = read_uci_query("cake-autorate", "CAKE Autorate full package")?;
    let sqm_package = read_uci_query("sqm", "SQM full package")?;
    BootstrapAbsenceIdentity::from_raw(
        instance,
        planned_sqm_section,
        target_interface,
        route_fingerprint,
        &cake_package,
        &sqm_package,
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedUciSection {
    section_type: String,
    options: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedUciPackage {
    canonical: Vec<u8>,
    sections: BTreeMap<String, ParsedUciSection>,
}

/// Bind a native calibration request to the complete UCI configuration which
/// can affect its recommendation.  This is the Rust equivalent of the legacy
/// Full Auto-Tune fingerprint: the instance, its owned SQM section, and every
/// traffic rule owned by that instance are sorted byte-for-byte under explicit
/// domain separators before hashing.
pub fn managed_autotune_config_fingerprint(
    instance: &str,
    sqm_section: &str,
) -> Result<String, String> {
    validate_uci_section(instance)?;
    validate_uci_section(sqm_section)?;
    let cake_query = format!("cake-autorate.{instance}");
    let sqm_query = format!("sqm.{sqm_section}");
    let cake = read_uci_query(&cake_query, "CAKE Autorate instance")?;
    let sqm = read_uci_query(&sqm_query, "managed SQM section")?;
    let package = read_uci_query("cake-autorate", "CAKE Autorate package")?;
    let canonical = canonicalize_autotune_config(instance, sqm_section, &cake, &sqm, &package)?;
    sha256sum(&canonical)
}

pub fn managed_sqm_identity_fingerprint(
    instance: &str,
    section: &str,
    target_interface: &str,
) -> Result<String, String> {
    let raw = read_uci_section(section)?;
    let canonical = validate_managed_sqm_identity(&raw, instance, section, target_interface)?;
    sha256sum(&canonical)
}

fn read_uci_section(section: &str) -> Result<Vec<u8>, String> {
    validate_uci_section(section)?;
    let query = format!("sqm.{section}");
    read_uci_query(&query, "SQM identity")
}

fn read_uci_query(query: &str, label: &str) -> Result<Vec<u8>, String> {
    if query.is_empty()
        || query.len() > 160
        || query.bytes().any(|byte| {
            !byte.is_ascii_alphanumeric() && byte != b'_' && byte != b'-' && byte != b'.'
        })
    {
        return Err(format!("{label} UCI query is invalid"));
    }
    let mut child = Command::new("uci")
        .args(["-q", "show", &query])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("failed to execute uci for {label}: {error}"))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "uci SQM identity stdout is unavailable".to_string())?;
    let mut raw = Vec::new();
    stdout
        .by_ref()
        .take((MAX_UCI_SHOW_BYTES + 1) as u64)
        .read_to_end(&mut raw)
        .map_err(|error| format!("failed to read {label}: {error}"))?;
    if raw.len() > MAX_UCI_SHOW_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("{label} UCI output exceeds its size bound"));
    }
    let status = child
        .wait()
        .map_err(|error| format!("failed to reap {label} UCI query: {error}"))?;
    if !status.success() {
        return Err(format!("{label} UCI query {query} is unavailable"));
    }

    Ok(raw)
}

fn validate_bootstrap_identifier(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_alphanumeric() && byte != b'_')
    {
        return Err(format!("bootstrap UCI absence {label} is not canonical"));
    }
    Ok(())
}

fn validate_bootstrap_target(target: &str) -> Result<(), String> {
    if target.is_empty()
        || target.len() > 64
        || target
            .bytes()
            .any(|byte| !byte.is_ascii_alphanumeric() && !b"._:@-".contains(&byte))
    {
        return Err("bootstrap UCI absence target interface is not canonical".to_string());
    }
    Ok(())
}

fn validate_lower_sha256(label: &str, value: &str) -> Result<(), String> {
    if value.len() != 64
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return Err(format!(
            "bootstrap UCI absence {label} is not lowercase SHA-256"
        ));
    }
    Ok(())
}

fn validate_uci_package_section(section: &str) -> Result<(), String> {
    if section.starts_with('@') {
        let Some((section_type, index)) = section[1..].split_once('[') else {
            return Err("bootstrap UCI package has a malformed anonymous section".to_string());
        };
        let Some(index) = index.strip_suffix(']') else {
            return Err("bootstrap UCI package has a malformed anonymous section".to_string());
        };
        if section_type.is_empty()
            || section_type
                .bytes()
                .any(|byte| !byte.is_ascii_alphanumeric() && byte != b'_')
            || index.is_empty()
            || index.bytes().any(|byte| !byte.is_ascii_digit())
        {
            return Err("bootstrap UCI package has a malformed anonymous section".to_string());
        }
        return Ok(());
    }
    validate_bootstrap_identifier("package section", section)
}

fn parse_bootstrap_uci_package(
    package: &str,
    raw: &[u8],
    maximum_bytes: usize,
) -> Result<ParsedUciPackage, String> {
    if raw.len() > maximum_bytes {
        return Err(format!(
            "bootstrap UCI absence {package} package exceeds its size bound"
        ));
    }
    let text = std::str::from_utf8(raw)
        .map_err(|_| format!("bootstrap UCI absence {package} package is not valid UTF-8"))?;
    let package_prefix = format!("{package}.");
    let mut canonical_lines = Vec::new();
    let mut declarations = BTreeMap::<String, String>::new();
    let mut option_sets = BTreeMap::<String, BTreeMap<String, String>>::new();
    let mut seen_keys = BTreeMap::<String, String>::new();

    for line in text.lines() {
        if line.is_empty() || line.chars().any(char::is_control) {
            return Err(format!(
                "bootstrap UCI absence {package} package contains a malformed line"
            ));
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("bootstrap UCI absence {package} package line has no value"))?;
        if value.is_empty() {
            return Err(format!(
                "bootstrap UCI absence {package} package has an empty raw value"
            ));
        }
        if let Some(previous) = seen_keys.insert(key.to_string(), value.to_string()) {
            let relation = if previous == value {
                "duplicate"
            } else {
                "conflicting duplicate"
            };
            return Err(format!(
                "bootstrap UCI absence {package} package has a {relation} key {key}"
            ));
        }
        let path = key
            .strip_prefix(&package_prefix)
            .ok_or_else(|| format!("bootstrap UCI absence package line is outside {package}"))?;
        let mut parts = path.split('.');
        let section = parts.next().unwrap_or_default();
        validate_uci_package_section(section)?;
        let option = parts.next();
        if parts.next().is_some() {
            return Err(format!(
                "bootstrap UCI absence {package} package key is malformed"
            ));
        }
        if let Some(option) = option {
            validate_bootstrap_identifier("package option", option)?;
            option_sets
                .entry(section.to_string())
                .or_default()
                .insert(option.to_string(), value.to_string());
        } else {
            if value
                .bytes()
                .any(|byte| !byte.is_ascii_alphanumeric() && byte != b'_')
            {
                return Err(format!(
                    "bootstrap UCI absence {package} section type is malformed"
                ));
            }
            declarations.insert(section.to_string(), value.to_string());
        }
        canonical_lines.push(line);
    }

    for section in option_sets.keys() {
        if !declarations.contains_key(section) {
            return Err(format!(
                "bootstrap UCI absence {package} package has options for an undeclared section"
            ));
        }
    }
    let sections = declarations
        .into_iter()
        .map(|(section, section_type)| {
            let options = option_sets.remove(&section).unwrap_or_default();
            (
                section,
                ParsedUciSection {
                    section_type,
                    options,
                },
            )
        })
        .collect();
    canonical_lines.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    let mut canonical = canonical_lines.join("\n").into_bytes();
    if !canonical.is_empty() {
        canonical.push(b'\n');
    }
    Ok(ParsedUciPackage {
        canonical,
        sections,
    })
}

fn bootstrap_uci_scalar<'a>(
    section: &'a ParsedUciSection,
    option: &str,
    label: &str,
) -> Result<Option<&'a str>, String> {
    let Some(raw) = section.options.get(option) else {
        return Ok(None);
    };
    let value = if raw.len() >= 2
        && ((raw.starts_with('\'') && raw.ends_with('\''))
            || (raw.starts_with('"') && raw.ends_with('"')))
    {
        &raw[1..raw.len() - 1]
    } else if raw.starts_with(['\'', '"']) || raw.ends_with(['\'', '"']) {
        return Err(format!(
            "bootstrap UCI absence {label} has malformed quoting"
        ));
    } else {
        raw.as_str()
    };
    if value
        .bytes()
        .any(|byte| !byte.is_ascii_alphanumeric() && !b"_-.@:".contains(&byte))
    {
        return Err(format!(
            "bootstrap UCI absence {label} has an unsafe scalar"
        ));
    }
    Ok(Some(value))
}

fn effective_cake_target<'a>(section: &'a ParsedUciSection) -> Result<&'a str, String> {
    let sqm_interface = bootstrap_uci_scalar(section, "sqm_interface", "SQM target")?
        .filter(|value| !value.is_empty());
    let wan_interface =
        bootstrap_uci_scalar(section, "wan_if", "WAN target")?.filter(|value| !value.is_empty());
    let mut upload_interface = bootstrap_uci_scalar(section, "ul_if", "upload target")?
        .filter(|value| !value.is_empty())
        .unwrap_or("wan");
    let automatic = bootstrap_uci_scalar(
        section,
        "auto_interface_preset",
        "automatic interface preset",
    )?
    .unwrap_or("1");
    if automatic != "0" && automatic != "1" {
        return Err("bootstrap UCI absence automatic interface preset is not boolean".to_string());
    }
    if automatic == "1" {
        upload_interface = wan_interface.or(sqm_interface).unwrap_or(upload_interface);
    }
    Ok(sqm_interface.unwrap_or(upload_interface))
}

fn validate_cake_bootstrap_absence(
    package: &ParsedUciPackage,
    instance: &str,
    planned_sqm_section: &str,
    target_interface: &str,
) -> Result<(), String> {
    if package.sections.contains_key(instance) {
        return Err("bootstrap CAKE Autorate instance already exists in UCI".to_string());
    }
    for (section_name, section) in &package.sections {
        match section.section_type.as_str() {
            "traffic_rule" => {
                if bootstrap_uci_scalar(section, "instance", "traffic-rule owner")?
                    == Some(instance)
                {
                    return Err(
                        "bootstrap CAKE Autorate traffic rule is already owned by the instance"
                            .to_string(),
                    );
                }
            }
            "cake_autorate" => {
                let owned_sqm =
                    match bootstrap_uci_scalar(section, "sqm_section", "managed SQM section")? {
                        Some("") => {
                            return Err(
                                "bootstrap CAKE Autorate owner has an empty managed SQM section"
                                    .to_string(),
                            )
                        }
                        Some(value) => value.to_string(),
                        None => format!("cake_{section_name}"),
                    };
                if owned_sqm == planned_sqm_section {
                    return Err(
                        "bootstrap planned SQM section is already owned by another autorate instance"
                            .to_string(),
                    );
                }
                if effective_cake_target(section)? == target_interface {
                    return Err(
                        "bootstrap target interface is already owned by another autorate instance"
                            .to_string(),
                    );
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_sqm_bootstrap_absence(
    package: &ParsedUciPackage,
    instance: &str,
    planned_sqm_section: &str,
    target_interface: &str,
) -> Result<(), String> {
    if package.sections.contains_key(planned_sqm_section) {
        return Err("bootstrap planned SQM section already exists in UCI".to_string());
    }
    for section in package.sections.values() {
        // A positive absence witness must be conservative about locally
        // extended SQM section types.  The standard type is `queue`, but an
        // owner marker or the target-interface option is conflicting evidence
        // regardless of the declaration label used by a local extension.
        if let Some(owner) = bootstrap_uci_scalar(
            section,
            "_cake_autorate_managed",
            "SQM autorate owner marker",
        )? {
            if owner == instance || owner == planned_sqm_section {
                return Err(
                    "bootstrap SQM queue already carries the requested owner marker".to_string(),
                );
            }
        }
        if bootstrap_uci_scalar(section, "interface", "SQM queue target")? == Some(target_interface)
        {
            return Err("bootstrap target interface already has an SQM queue owner".to_string());
        }
    }
    Ok(())
}

fn bootstrap_absence_fingerprint_material(
    domain: &str,
    instance: &str,
    planned_sqm_section: &str,
    target_interface: &str,
    route_fingerprint: &str,
    package: &str,
    canonical_snapshot: &[u8],
) -> Vec<u8> {
    let mut material = format!(
        "{domain}\ninstance={instance}\nplanned_sqm_section={planned_sqm_section}\ntarget_interface={target_interface}\nroute_fingerprint={route_fingerprint}\npackage={package}\nsnapshot_bytes={}\n",
        canonical_snapshot.len()
    )
    .into_bytes();
    material.extend_from_slice(canonical_snapshot);
    material
}

fn canonicalize_autotune_config(
    instance: &str,
    sqm_section: &str,
    cake_raw: &[u8],
    sqm_raw: &[u8],
    package_raw: &[u8],
) -> Result<Vec<u8>, String> {
    validate_uci_section(instance)?;
    validate_uci_section(sqm_section)?;
    let cake = canonicalize_uci_show(cake_raw)?;
    let sqm = canonicalize_uci_show(sqm_raw)?;
    let package = std::str::from_utf8(package_raw)
        .map_err(|_| "CAKE Autorate UCI package is not valid UTF-8".to_string())?;

    let mut rule_sections = Vec::new();
    for line in package.lines() {
        let Some(rest) = line.strip_prefix("cake-autorate.") else {
            continue;
        };
        let Some(section) = rest.strip_suffix("=traffic_rule") else {
            continue;
        };
        validate_uci_section(section)?;
        if !rule_sections.iter().any(|known| known == section) {
            rule_sections.push(section.to_string());
        }
    }
    rule_sections.sort_unstable();

    let mut rule_lines = Vec::new();
    for section in rule_sections {
        let owner_prefix = format!("cake-autorate.{section}.instance=");
        let owner = package
            .lines()
            .find_map(|line| line.strip_prefix(&owner_prefix))
            .map(unquote_uci_value);
        if owner != Some(instance) {
            continue;
        }
        let prefix = format!("cake-autorate.{section}");
        let declaration = format!("{prefix}=traffic_rule");
        for line in package
            .lines()
            .filter(|line| *line == declaration || line.starts_with(&format!("{prefix}.")))
        {
            if line.is_empty() || line.contains('\0') {
                return Err("traffic-rule UCI identity is malformed".to_string());
            }
            rule_lines.push(line);
        }
    }
    rule_lines.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));

    let mut canonical = Vec::new();
    canonical.extend_from_slice(format!("cake-autorate:{instance}\n").as_bytes());
    canonical.extend_from_slice(&cake);
    canonical.extend_from_slice(format!("sqm:{sqm_section}\n").as_bytes());
    canonical.extend_from_slice(&sqm);
    canonical.extend_from_slice(format!("traffic-rules:{instance}\n").as_bytes());
    if rule_lines.is_empty() {
        canonical.extend_from_slice(b"<none>\n");
    } else {
        canonical.extend_from_slice(rule_lines.join("\n").as_bytes());
        canonical.push(b'\n');
    }
    Ok(canonical)
}

fn unquote_uci_value(value: &str) -> &str {
    if value.len() >= 2
        && ((value.starts_with('\'') && value.ends_with('\''))
            || (value.starts_with('"') && value.ends_with('"')))
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

pub(crate) fn validate_uci_section(section: &str) -> Result<(), String> {
    if section.is_empty()
        || section.len() > 64
        || section
            .bytes()
            .any(|byte| !byte.is_ascii_alphanumeric() && byte != b'_')
    {
        return Err("managed SQM section name is invalid".to_string());
    }
    Ok(())
}

fn canonicalize_uci_show(raw: &[u8]) -> Result<Vec<u8>, String> {
    let text =
        std::str::from_utf8(raw).map_err(|_| "SQM UCI identity is not valid UTF-8".to_string())?;
    let mut lines: Vec<&str> = text.lines().collect();
    if lines.is_empty()
        || lines
            .iter()
            .any(|line| line.is_empty() || line.contains('\0'))
    {
        return Err("SQM UCI identity is empty or malformed".to_string());
    }
    lines.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    let mut canonical = lines.join("\n").into_bytes();
    canonical.push(b'\n');
    Ok(canonical)
}

fn validate_managed_sqm_identity(
    raw: &[u8],
    instance: &str,
    section: &str,
    target_interface: &str,
) -> Result<Vec<u8>, String> {
    validate_uci_section(section)?;
    if instance.is_empty() || target_interface.is_empty() {
        return Err("managed SQM owner or target is empty".to_string());
    }
    let text =
        std::str::from_utf8(raw).map_err(|_| "SQM UCI identity is not valid UTF-8".to_string())?;
    let prefix = format!("sqm.{section}");
    let owner = uci_scalar(text, &format!("{prefix}._cake_autorate_managed"))?;
    let enabled = uci_scalar(text, &format!("{prefix}.enabled"))?;
    let interface = uci_scalar(text, &format!("{prefix}.interface"))?;
    if owner != instance {
        return Err("managed SQM section owner does not match the autorate instance".to_string());
    }
    if enabled != "1" {
        return Err("managed SQM section is disabled".to_string());
    }
    if interface != target_interface {
        return Err("managed SQM section target does not match the autorate instance".to_string());
    }
    canonicalize_uci_show(raw)
}

fn uci_scalar<'a>(text: &'a str, key: &str) -> Result<&'a str, String> {
    let prefix = format!("{key}=");
    let mut values = text.lines().filter_map(|line| line.strip_prefix(&prefix));
    let raw = values
        .next()
        .ok_or_else(|| format!("required UCI option {key} is unavailable"))?;
    if values.next().is_some() {
        return Err(format!("required UCI option {key} is ambiguous"));
    }
    let value = if raw.len() >= 2
        && ((raw.starts_with('\'') && raw.ends_with('\''))
            || (raw.starts_with('"') && raw.ends_with('"')))
    {
        &raw[1..raw.len() - 1]
    } else {
        raw
    };
    if value.is_empty()
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_alphanumeric() && !b"_-.@:".contains(&byte))
    {
        return Err(format!("required UCI option {key} has an unsafe value"));
    }
    Ok(value)
}

pub(crate) fn sha256sum(input: &[u8]) -> Result<String, String> {
    let mut child = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("failed to execute sha256sum: {error}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| "sha256sum stdin is unavailable".to_string())?
        .write_all(input)
        .map_err(|error| format!("failed to write SQM identity to sha256sum: {error}"))?;
    let output = child
        .wait_with_output()
        .map_err(|error| format!("failed to reap sha256sum: {error}"))?;
    if !output.status.success() || output.stdout.len() > 256 {
        return Err("sha256sum failed while computing SQM identity".to_string());
    }
    parse_sha256sum(&output.stdout)
}

fn parse_sha256sum(output: &[u8]) -> Result<String, String> {
    let text = std::str::from_utf8(output)
        .map_err(|_| "sha256sum output is not valid UTF-8".to_string())?;
    let digest = text
        .split_ascii_whitespace()
        .next()
        .ok_or_else(|| "sha256sum output is empty".to_string())?;
    if digest.len() != 64
        || digest
            .bytes()
            .any(|byte| !byte.is_ascii_digit() && !(b'a'..=b'f').contains(&byte))
    {
        return Err("sha256sum returned an invalid digest".to_string());
    }
    Ok(digest.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_uci_identity_matches_legacy_sorted_lines() {
        assert_eq!(
            canonicalize_uci_show(b"sqm.wan.upload='20'\nsqm.wan=queue\nsqm.wan.download='100'\n")
                .unwrap(),
            b"sqm.wan.download='100'\nsqm.wan.upload='20'\nsqm.wan=queue\n"
        );
    }

    #[test]
    fn identity_inputs_and_digest_are_strict_and_bounded() {
        assert!(validate_uci_section("wan_sqm").is_ok());
        assert!(validate_uci_section("wan.sqm").is_err());
        assert!(canonicalize_uci_show(b"").is_err());
        assert!(canonicalize_uci_show(b"sqm.wan=x\0bad\n").is_err());
        assert_eq!(
            parse_sha256sum(
                b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  -\n"
            )
            .unwrap(),
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
        assert!(parse_sha256sum(b"ABC\n").is_err());
    }

    #[test]
    fn managed_identity_requires_exact_owner_enabled_state_and_target() {
        let valid = b"sqm.cake_wan=queue\nsqm.cake_wan.interface='pppoe-wan'\nsqm.cake_wan.enabled='1'\nsqm.cake_wan._cake_autorate_managed='wan_sqm'\n";
        assert!(validate_managed_sqm_identity(valid, "wan_sqm", "cake_wan", "pppoe-wan").is_ok());
        assert!(
            validate_managed_sqm_identity(valid, "wanb_sqm", "cake_wan", "pppoe-wan")
                .unwrap_err()
                .contains("owner")
        );
        assert!(
            validate_managed_sqm_identity(valid, "wan_sqm", "cake_wan", "eth0")
                .unwrap_err()
                .contains("target")
        );
        let disabled = valid
            .windows(b"enabled='1'".len())
            .position(|window| window == b"enabled='1'")
            .map(|offset| {
                let mut bytes = valid.to_vec();
                bytes[offset + b"enabled='".len()] = b'0';
                bytes
            })
            .unwrap();
        assert!(
            validate_managed_sqm_identity(&disabled, "wan_sqm", "cake_wan", "pppoe-wan")
                .unwrap_err()
                .contains("disabled")
        );
    }

    #[test]
    fn autotune_config_identity_matches_legacy_domains_and_owned_rules() {
        let cake = b"cake-autorate.wan_sqm.max_dl_shaper_rate_kbps='900000'\ncake-autorate.wan_sqm=cake_autorate\ncake-autorate.wan_sqm.sqm_section='cake_wan_sqm'\n";
        let sqm = b"sqm.cake_wan_sqm.upload='800000'\nsqm.cake_wan_sqm=queue\nsqm.cake_wan_sqm.download='900000'\n";
        let package = b"cake-autorate.other_rule=traffic_rule\ncake-autorate.other_rule.instance='wanb_sqm'\ncake-autorate.z_rule.proto='udp'\ncake-autorate.z_rule.instance='wan_sqm'\ncake-autorate.z_rule=traffic_rule\ncake-autorate.a_rule=traffic_rule\ncake-autorate.a_rule.instance='wan_sqm'\ncake-autorate.a_rule.dest_port='443'\n";
        let canonical =
            canonicalize_autotune_config("wan_sqm", "cake_wan_sqm", cake, sqm, package).unwrap();
        assert_eq!(
            String::from_utf8(canonical).unwrap(),
            "cake-autorate:wan_sqm\n\
cake-autorate.wan_sqm.max_dl_shaper_rate_kbps='900000'\n\
cake-autorate.wan_sqm.sqm_section='cake_wan_sqm'\n\
cake-autorate.wan_sqm=cake_autorate\n\
sqm:cake_wan_sqm\n\
sqm.cake_wan_sqm.download='900000'\n\
sqm.cake_wan_sqm.upload='800000'\n\
sqm.cake_wan_sqm=queue\n\
traffic-rules:wan_sqm\n\
cake-autorate.a_rule.dest_port='443'\n\
cake-autorate.a_rule.instance='wan_sqm'\n\
cake-autorate.a_rule=traffic_rule\n\
cake-autorate.z_rule.instance='wan_sqm'\n\
cake-autorate.z_rule.proto='udp'\n\
cake-autorate.z_rule=traffic_rule\n"
        );
    }

    #[test]
    fn autotune_config_identity_uses_explicit_empty_rule_marker() {
        let canonical = canonicalize_autotune_config(
            "wan_sqm",
            "cake_wan_sqm",
            b"cake-autorate.wan_sqm=cake_autorate\n",
            b"sqm.cake_wan_sqm=queue\n",
            b"cake-autorate.wan_sqm=cake_autorate\n",
        )
        .unwrap();
        assert!(canonical.ends_with(b"traffic-rules:wan_sqm\n<none>\n"));
    }

    fn route_fingerprint() -> String {
        "a".repeat(64)
    }

    fn bootstrap_identity(cake: &[u8], sqm: &[u8]) -> Result<BootstrapAbsenceIdentity, String> {
        BootstrapAbsenceIdentity::from_raw(
            "wan_sqm",
            "cake_wan_sqm",
            "pppoe-wan",
            &route_fingerprint(),
            cake,
            sqm,
        )
    }

    #[test]
    fn bootstrap_absence_identity_is_canonical_domain_separated_and_exact() {
        let cake = b"cake-autorate.other_rule.instance='other'\ncake-autorate.globals=globals\ncake-autorate.other=cake_autorate\ncake-autorate.other_rule=traffic_rule\ncake-autorate.other.sqm_interface='eth1'\ncake-autorate.other.sqm_section='cake_other'\n";
        let cake_reordered = b"cake-autorate.other.sqm_section='cake_other'\ncake-autorate.other_rule=traffic_rule\ncake-autorate.other.sqm_interface='eth1'\ncake-autorate.other=cake_autorate\ncake-autorate.globals=globals\ncake-autorate.other_rule.instance='other'\n";
        let sqm = b"sqm.other.interface='eth1'\nsqm.other=queue\nsqm.other._cake_autorate_managed='other'\n";
        let first = bootstrap_identity(cake, sqm).unwrap();
        let reordered = bootstrap_identity(cake_reordered, sqm).unwrap();
        assert_eq!(first, reordered);
        assert_ne!(first.config_fingerprint(), first.sqm_fingerprint());
        validate_lower_sha256("config", first.config_fingerprint()).unwrap();
        validate_lower_sha256("sqm", first.sqm_fingerprint()).unwrap();
        first.ensure_exact_reattestation(&reordered).unwrap();

        let route_drift = BootstrapAbsenceIdentity::from_raw(
            "wan_sqm",
            "cake_wan_sqm",
            "pppoe-wan",
            &"b".repeat(64),
            cake,
            sqm,
        )
        .unwrap();
        assert_ne!(first.config_fingerprint(), route_drift.config_fingerprint());
        assert_ne!(first.sqm_fingerprint(), route_drift.sqm_fingerprint());
        assert!(first.ensure_exact_reattestation(&route_drift).is_err());
    }

    #[test]
    fn bootstrap_absence_fingerprints_cover_unrelated_full_package_drift() {
        let cake = b"cake-autorate.globals=globals\ncake-autorate.globals.graph_history_ram_budget_kib='auto'\n";
        let sqm = b"sqm.other=queue\nsqm.other.interface='eth1'\n";
        let baseline = bootstrap_identity(cake, sqm).unwrap();
        let cake_drift = bootstrap_identity(
            b"cake-autorate.globals=globals\ncake-autorate.globals.graph_history_ram_budget_kib='256'\n",
            sqm,
        )
        .unwrap();
        assert_ne!(
            baseline.config_fingerprint(),
            cake_drift.config_fingerprint()
        );
        assert_eq!(baseline.sqm_fingerprint(), cake_drift.sqm_fingerprint());

        let sqm_drift =
            bootstrap_identity(cake, b"sqm.other=queue\nsqm.other.interface='eth2'\n").unwrap();
        assert_eq!(
            baseline.config_fingerprint(),
            sqm_drift.config_fingerprint()
        );
        assert_ne!(baseline.sqm_fingerprint(), sqm_drift.sqm_fingerprint());
    }

    #[test]
    fn bootstrap_absence_fingerprints_bind_every_requested_identity_field() {
        let cake = b"cake-autorate.globals=globals\n";
        let sqm = b"sqm.other=queue\nsqm.other.interface='eth1'\n";
        let baseline = bootstrap_identity(cake, sqm).unwrap();
        for changed in [
            BootstrapAbsenceIdentity::from_raw(
                "wanb_sqm",
                "cake_wan_sqm",
                "pppoe-wan",
                &route_fingerprint(),
                cake,
                sqm,
            )
            .unwrap(),
            BootstrapAbsenceIdentity::from_raw(
                "wan_sqm",
                "cake_wanb_sqm",
                "pppoe-wan",
                &route_fingerprint(),
                cake,
                sqm,
            )
            .unwrap(),
            BootstrapAbsenceIdentity::from_raw(
                "wan_sqm",
                "cake_wan_sqm",
                "eth2",
                &route_fingerprint(),
                cake,
                sqm,
            )
            .unwrap(),
        ] {
            assert_ne!(baseline.config_fingerprint(), changed.config_fingerprint());
            assert_ne!(baseline.sqm_fingerprint(), changed.sqm_fingerprint());
            assert!(baseline.ensure_exact_reattestation(&changed).is_err());
        }
    }

    #[test]
    fn bootstrap_absence_rejects_exact_sections_and_owned_traffic_rules() {
        let exact_instance =
            b"cake-autorate.wan_sqm=cake_autorate\ncake-autorate.wan_sqm.sqm_interface='eth1'\n";
        assert!(bootstrap_identity(exact_instance, b"")
            .unwrap_err()
            .contains("instance already exists"));

        let exact_sqm = b"sqm.cake_wan_sqm=queue\nsqm.cake_wan_sqm.interface='eth1'\n";
        assert!(bootstrap_identity(b"", exact_sqm)
            .unwrap_err()
            .contains("planned SQM section already exists"));

        let owned_rule =
            b"cake-autorate.rule=traffic_rule\ncake-autorate.rule.instance='wan_sqm'\n";
        assert!(bootstrap_identity(owned_rule, b"")
            .unwrap_err()
            .contains("traffic rule"));
    }

    #[test]
    fn bootstrap_absence_rejects_autorate_section_and_target_owners() {
        let section_owner = b"cake-autorate.other=cake_autorate\ncake-autorate.other.sqm_section='cake_wan_sqm'\ncake-autorate.other.sqm_interface='eth1'\n";
        assert!(bootstrap_identity(section_owner, b"")
            .unwrap_err()
            .contains("planned SQM section"));

        let direct_target = b"cake-autorate.other=cake_autorate\ncake-autorate.other.sqm_section='cake_other'\ncake-autorate.other.sqm_interface='pppoe-wan'\n";
        assert!(bootstrap_identity(direct_target, b"")
            .unwrap_err()
            .contains("target interface"));

        let fallback_target = b"cake-autorate.other=cake_autorate\ncake-autorate.other.sqm_section='cake_other'\ncake-autorate.other.wan_if='pppoe-wan'\n";
        assert!(bootstrap_identity(fallback_target, b"")
            .unwrap_err()
            .contains("target interface"));
    }

    #[test]
    fn bootstrap_absence_rejects_sqm_owner_markers_and_target_queues() {
        for owner in ["wan_sqm", "cake_wan_sqm"] {
            let sqm = format!(
                "sqm.other=queue\nsqm.other.interface='eth1'\nsqm.other._cake_autorate_managed='{owner}'\n"
            );
            assert!(bootstrap_identity(b"", sqm.as_bytes())
                .unwrap_err()
                .contains("owner marker"));
        }

        let target = b"sqm.other=queue\nsqm.other.interface='pppoe-wan'\n";
        assert!(bootstrap_identity(b"", target)
            .unwrap_err()
            .contains("target interface"));

        let extended_type = b"sqm.other=local_queue\nsqm.other.interface='pppoe-wan'\n";
        assert!(bootstrap_identity(b"", extended_type)
            .unwrap_err()
            .contains("target interface"));
    }

    #[test]
    fn bootstrap_absence_parser_fails_closed_on_malformed_and_duplicate_input() {
        let duplicate = b"cake-autorate.globals=globals\ncake-autorate.globals=globals\n";
        assert!(bootstrap_identity(duplicate, b"")
            .unwrap_err()
            .contains("duplicate"));
        let conflicting = b"cake-autorate.globals=globals\ncake-autorate.globals=cake_autorate\n";
        assert!(bootstrap_identity(conflicting, b"")
            .unwrap_err()
            .contains("conflicting duplicate"));
        let undeclared = b"cake-autorate.ghost.sqm_section='cake_other'\n";
        assert!(bootstrap_identity(undeclared, b"")
            .unwrap_err()
            .contains("undeclared section"));
        let malformed_owner =
            b"cake-autorate.rule=traffic_rule\ncake-autorate.rule.instance='wan_sqm\n";
        assert!(bootstrap_identity(malformed_owner, b"")
            .unwrap_err()
            .contains("malformed quoting"));
        let wrong_package = b"sqm.other=queue\n";
        assert!(bootstrap_identity(wrong_package, b"").is_err());
    }

    #[test]
    fn bootstrap_absence_inputs_are_strict_and_bounded() {
        assert!(BootstrapAbsenceIdentity::from_raw(
            "wan-sqm",
            "cake_wan_sqm",
            "pppoe-wan",
            &route_fingerprint(),
            b"",
            b"",
        )
        .is_err());
        assert!(BootstrapAbsenceIdentity::from_raw(
            "wan_sqm",
            "cake_wan_sqm",
            "bad interface",
            &route_fingerprint(),
            b"",
            b"",
        )
        .is_err());
        assert!(BootstrapAbsenceIdentity::from_raw(
            "wan_sqm",
            "cake_wan_sqm",
            &"x".repeat(64),
            &"A".repeat(64),
            b"",
            b"",
        )
        .is_err());
        let oversized = vec![b'x'; MAX_UCI_SHOW_BYTES + 1];
        assert!(bootstrap_identity(&oversized, b"")
            .unwrap_err()
            .contains("size bound"));
    }
}
