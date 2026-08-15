//! Pure UCI materialization for an absent managed configuration.
//!
//! This module deliberately has no process or live-state authority. It expands one already typed
//! [`NativeManagedConfigPlan`] into a bounded, canonical UCI command plan and
//! and can verify the exact typed contents of its target sections in canonical
//! libuci-written package files. In particular, list verification never uses
//! `uci get` or `uci show`, both of which flatten list representation.

use super::autotune_managed_config::{
    NativeManagedConfigPlan, NativeManagedPackage, NativeManagedUciSectionPlan,
    NativeManagedUciValue,
};
use ring::digest::{digest, SHA256};
use std::collections::BTreeMap;

pub(crate) const NATIVE_UCI_MATERIALIZATION_SCHEMA_VERSION: u8 = 1;
const MAX_LOGICAL_ACTIONS: usize = 128;
const MAX_PHYSICAL_COMMANDS: usize = 256;
const MAX_CANONICAL_BYTES: usize = 128 * 1024;
const MAX_UCI_BATCH_BYTES: usize = 128 * 1024;
const MAX_UCI_FILE_BYTES: usize = 1024 * 1024;
const MAX_UCI_FILE_COMMANDS: usize = 8192;
const MAX_UCI_FILE_SECTIONS: usize = 1024;
const MAX_UCI_TARGET_OPTIONS: usize = 128;
const MAX_UCI_TARGET_OPTION_STATEMENTS: usize = 256;
const MAX_UCI_LIST_ITEMS: usize = 64;
const MAX_UCI_VALUE_BYTES: usize = 1024;
const MAX_UCI_TOKEN_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
enum NativeUciMaterializationCommand {
    AddSection {
        package: NativeManagedPackage,
        section: String,
        section_type: String,
    },
    Set {
        package: NativeManagedPackage,
        section: String,
        option: String,
        value: String,
    },
    DeleteOption {
        package: NativeManagedPackage,
        section: String,
        option: String,
    },
    AddList {
        package: NativeManagedPackage,
        section: String,
        option: String,
        value: String,
    },
    CommitPackage {
        package: NativeManagedPackage,
    },
}

impl NativeUciMaterializationCommand {
    fn validate(&self) -> Result<(), String> {
        match self {
            Self::AddSection {
                package,
                section,
                section_type,
            } => {
                validate_package_section(*package, section)?;
                validate_uci_identifier("section type", section_type)
            }
            Self::Set {
                package,
                section,
                option,
                value,
            }
            | Self::AddList {
                package,
                section,
                option,
                value,
            } => {
                validate_package_section(*package, section)?;
                validate_uci_identifier("option", option)?;
                validate_batch_value(option, value)
            }
            Self::DeleteOption {
                package,
                section,
                option,
            } => {
                validate_package_section(*package, section)?;
                validate_uci_identifier("option", option)
            }
            Self::CommitPackage { .. } => Ok(()),
        }
    }
}

/// A canonical physical command plan bound to one typed managed-config plan.
///
/// The logical action count remains the managed-config count: creating one
/// section or assigning one option is one logical action. A list assignment is
/// then expanded into one `delete` plus its ordered `add_list` commands. This
/// keeps the independent 128-action bootstrap wall meaningful without hiding
/// the larger, separately bounded physical command count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeUciMaterializationPlan {
    schema_version: u8,
    managed_config_sha256: String,
    logical_action_count: usize,
    sections: BTreeMap<NativeManagedPackage, NativeManagedUciSectionPlan>,
    commands: Vec<NativeUciMaterializationCommand>,
}

/// Exact candidate package bytes derived only by appending the planned named
/// sections to the attested original package files.  Existing bytes are
/// never normalized or regenerated, so equality with this pair proves that no
/// unrelated UCI section was changed while preparing bootstrap recovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeUciCandidatePair {
    cake: Vec<u8>,
    sqm: Vec<u8>,
}

impl NativeUciCandidatePair {
    pub(crate) fn cake(&self) -> &[u8] {
        &self.cake
    }

    pub(crate) fn sqm(&self) -> &[u8] {
        &self.sqm
    }
}

impl NativeUciMaterializationPlan {
    pub(crate) fn from_managed_config(managed: &NativeManagedConfigPlan) -> Result<Self, String> {
        let logical_action_count = managed.action_count();
        if logical_action_count == 0 || logical_action_count > MAX_LOGICAL_ACTIONS {
            return Err(format!(
                "native UCI materialization exceeds its {MAX_LOGICAL_ACTIONS}-action bound"
            ));
        }

        let sections = sections_from_managed_config(managed)?;

        let commands = materialize_commands(&sections)?;
        let plan = Self {
            schema_version: NATIVE_UCI_MATERIALIZATION_SCHEMA_VERSION,
            managed_config_sha256: managed.canonical_sha256()?,
            logical_action_count,
            sections,
            commands,
        };
        plan.ensure_bound_to(managed)?;
        let _ = plan.canonical_bytes()?;
        let _ = plan.canonical_uci_batch()?;
        Ok(plan)
    }

    #[cfg(test)]
    pub(crate) fn managed_config_sha256(&self) -> &str {
        &self.managed_config_sha256
    }

    #[cfg(test)]
    pub(crate) fn logical_action_count(&self) -> usize {
        self.logical_action_count
    }

    #[cfg(test)]
    pub(crate) fn command_count(&self) -> usize {
        self.commands.len()
    }

    pub(crate) fn ensure_bound_to(&self, managed: &NativeManagedConfigPlan) -> Result<(), String> {
        self.validate()?;
        let actual = managed.canonical_sha256()?;
        let expected_sections = sections_from_managed_config(managed)?;
        if self.managed_config_sha256 != actual || self.sections != expected_sections {
            return Err(
                "native UCI materialization is not bound to this managed-config plan".to_string(),
            );
        }
        Ok(())
    }

    pub(crate) fn canonical_bytes(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let commands = self
            .commands
            .iter()
            .map(command_json)
            .collect::<Vec<_>>()
            .join(",");
        let output = format!(
            concat!(
                "{{\"native_uci_materialization_schema_version\":{},",
                "\"managed_config_sha256\":{},\"logical_action_count\":{},",
                "\"command_count\":{},\"commands\":[{}]}}\n"
            ),
            self.schema_version,
            json_string(&self.managed_config_sha256),
            self.logical_action_count,
            self.commands.len(),
            commands,
        );
        if output.len() > MAX_CANONICAL_BYTES {
            return Err("native UCI materialization exceeds its canonical byte bound".to_string());
        }
        Ok(output.into_bytes())
    }

    pub(crate) fn canonical_sha256(&self) -> Result<String, String> {
        let digest = digest(&SHA256, &self.canonical_bytes()?);
        Ok(hex_lower(digest.as_ref()))
    }

    pub(crate) fn canonical_uci_batch(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let mut output = String::new();
        for command in &self.commands {
            command.validate()?;
            match command {
                NativeUciMaterializationCommand::AddSection {
                    package,
                    section,
                    section_type,
                } => {
                    output.push_str("set ");
                    output.push_str(package_name(*package));
                    output.push('.');
                    output.push_str(section);
                    output.push('=');
                    output.push_str(section_type);
                    output.push('\n');
                }
                NativeUciMaterializationCommand::Set {
                    package,
                    section,
                    option,
                    value,
                } => {
                    output.push_str("set ");
                    push_option_path(&mut output, *package, section, option);
                    output.push_str("='");
                    output.push_str(value);
                    output.push_str("'\n");
                }
                NativeUciMaterializationCommand::DeleteOption {
                    package,
                    section,
                    option,
                } => {
                    output.push_str("delete ");
                    push_option_path(&mut output, *package, section, option);
                    output.push('\n');
                }
                NativeUciMaterializationCommand::AddList {
                    package,
                    section,
                    option,
                    value,
                } => {
                    output.push_str("add_list ");
                    push_option_path(&mut output, *package, section, option);
                    output.push_str("='");
                    output.push_str(value);
                    output.push_str("'\n");
                }
                NativeUciMaterializationCommand::CommitPackage { package } => {
                    output.push_str("commit ");
                    output.push_str(package_name(*package));
                    output.push('\n');
                }
            }
            if output.len() > MAX_UCI_BATCH_BYTES {
                return Err("native UCI materialization batch exceeds its byte bound".to_string());
            }
        }
        Ok(output.into_bytes())
    }

    /// Build the only candidate byte pair accepted by bootstrap recovery.
    /// The target sections must be absent from the exact original files and
    /// each candidate is `original + optional newline + canonical section`.
    /// This is intentionally stronger than accepting arbitrary libuci output
    /// after checking only the two target sections.
    pub(crate) fn deterministic_candidate_pair(
        &self,
        cake_original: &[u8],
        sqm_original: &[u8],
    ) -> Result<NativeUciCandidatePair, String> {
        self.validate()?;
        let cake =
            self.deterministic_candidate_file(NativeManagedPackage::CakeAutorate, cake_original)?;
        let sqm = self.deterministic_candidate_file(NativeManagedPackage::Sqm, sqm_original)?;
        let pair = NativeUciCandidatePair { cake, sqm };
        self.verify_deterministic_candidate_pair(cake_original, sqm_original, &pair)?;
        Ok(pair)
    }

    pub(crate) fn verify_deterministic_candidate_pair(
        &self,
        cake_original: &[u8],
        sqm_original: &[u8],
        candidate: &NativeUciCandidatePair,
    ) -> Result<(), String> {
        self.verify_deterministic_candidate_files(
            cake_original,
            sqm_original,
            candidate.cake(),
            candidate.sqm(),
        )
    }

    pub(crate) fn verify_deterministic_candidate_files(
        &self,
        cake_original: &[u8],
        sqm_original: &[u8],
        cake_candidate: &[u8],
        sqm_candidate: &[u8],
    ) -> Result<(), String> {
        self.validate()?;
        let expected_cake =
            self.deterministic_candidate_file(NativeManagedPackage::CakeAutorate, cake_original)?;
        let expected_sqm =
            self.deterministic_candidate_file(NativeManagedPackage::Sqm, sqm_original)?;
        if cake_candidate != expected_cake || sqm_candidate != expected_sqm {
            return Err(
                "native UCI candidate pair is not the deterministic append transition".to_string(),
            );
        }
        Ok(())
    }

    fn deterministic_candidate_file(
        &self,
        package: NativeManagedPackage,
        original: &[u8],
    ) -> Result<Vec<u8>, String> {
        let Some(expected) = self.sections.get(&package) else {
            if package == NativeManagedPackage::Sqm {
                return Ok(original.to_vec());
            }
            return Err(
                "native UCI materialization has no expected cake-autorate section".to_string(),
            );
        };
        verify_target_absent(expected, original)?;
        let append = canonical_section_bytes(expected)?;
        let separator = usize::from(!original.is_empty() && !original.ends_with(b"\n"));
        let candidate_len = original
            .len()
            .checked_add(separator)
            .and_then(|value| value.checked_add(append.len()))
            .ok_or_else(|| "native UCI candidate size overflowed".to_string())?;
        if candidate_len == 0 || candidate_len > MAX_UCI_FILE_BYTES {
            return Err("native UCI candidate file size is outside its bound".to_string());
        }
        let mut candidate = Vec::with_capacity(candidate_len);
        candidate.extend_from_slice(original);
        if separator == 1 {
            candidate.push(b'\n');
        }
        candidate.extend_from_slice(&append);
        verify_expected_section(expected, &candidate)?;
        Ok(candidate)
    }

    /// Verify the exact typed target sections in two raw libuci-written config
    /// files. Unrelated sections are parsed and bounded but are deliberately
    /// outside this target-section comparison; future recovery still binds the
    /// complete raw files by digest.
    pub(crate) fn verify_exact_config_files(
        &self,
        cake_config: &[u8],
        sqm_config: &[u8],
    ) -> Result<(), String> {
        self.validate()?;
        self.verify_exact_package(NativeManagedPackage::CakeAutorate, cake_config)?;
        self.verify_exact_package(NativeManagedPackage::Sqm, sqm_config)
    }

    pub(crate) fn verify_exact_package(
        &self,
        package: NativeManagedPackage,
        config: &[u8],
    ) -> Result<(), String> {
        match self.sections.get(&package) {
            Some(expected) => verify_expected_section(expected, config),
            None if package == NativeManagedPackage::Sqm => Ok(()),
            None => {
                Err("native UCI materialization has no expected cake-autorate section".to_string())
            }
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != NATIVE_UCI_MATERIALIZATION_SCHEMA_VERSION {
            return Err("native UCI materialization schema is unsupported".to_string());
        }
        require_lower_hex_sha256(&self.managed_config_sha256)?;
        if self.logical_action_count == 0 || self.logical_action_count > MAX_LOGICAL_ACTIONS {
            return Err(
                "native UCI materialization logical action count is outside its bound".to_string(),
            );
        }
        if self.commands.is_empty() || self.commands.len() > MAX_PHYSICAL_COMMANDS {
            return Err(
                "native UCI materialization physical command count is outside its bound"
                    .to_string(),
            );
        }
        if self.sections.is_empty()
            || self.sections.len() > 2
            || !self
                .sections
                .contains_key(&NativeManagedPackage::CakeAutorate)
        {
            return Err("native UCI materialization has an invalid section set".to_string());
        }
        let calculated_logical = self
            .sections
            .len()
            .checked_add(
                self.sections
                    .values()
                    .map(|section| section.options().len())
                    .sum::<usize>(),
            )
            .ok_or_else(|| "native UCI materialization action count overflowed".to_string())?;
        if self.logical_action_count != calculated_logical {
            return Err(
                "native UCI materialization logical action count is inconsistent".to_string(),
            );
        }
        let expected_commands = materialize_commands(&self.sections)?;
        if self.commands != expected_commands {
            return Err("native UCI materialization command ordering is not canonical".to_string());
        }
        Ok(())
    }
}

fn sections_from_managed_config(
    managed: &NativeManagedConfigPlan,
) -> Result<BTreeMap<NativeManagedPackage, NativeManagedUciSectionPlan>, String> {
    let mut sections = BTreeMap::new();
    let mut planned = vec![managed.cake()];
    if let Some(sqm) = managed.sqm() {
        planned.push(sqm);
    }
    for section in planned {
        if sections
            .insert(section.package(), section.clone())
            .is_some()
        {
            return Err("native UCI materialization has a duplicate package section".to_string());
        }
    }
    if sections.is_empty()
        || sections.len() > 2
        || !sections.contains_key(&NativeManagedPackage::CakeAutorate)
    {
        return Err("native UCI materialization has an invalid managed section set".to_string());
    }
    Ok(sections)
}

fn materialize_commands(
    sections: &BTreeMap<NativeManagedPackage, NativeManagedUciSectionPlan>,
) -> Result<Vec<NativeUciMaterializationCommand>, String> {
    let mut commands = Vec::new();

    // The complete, sorted section-creation prefix makes the absent target
    // explicit before any option assignment without silently reordering an
    // arbitrary caller-provided vector.
    for (package, section) in sections {
        push_materialization_command(
            &mut commands,
            NativeUciMaterializationCommand::AddSection {
                package: *package,
                section: section.section().to_string(),
                section_type: section.section_type().to_string(),
            },
        )?;
    }

    for (package, section) in sections {
        for (option, value) in section.options() {
            match value {
                NativeManagedUciValue::Scalar(value) => {
                    push_materialization_command(
                        &mut commands,
                        NativeUciMaterializationCommand::Set {
                            package: *package,
                            section: section.section().to_string(),
                            option: (*option).to_string(),
                            value: value.clone(),
                        },
                    )?;
                }
                NativeManagedUciValue::ReplaceList(values) => {
                    push_materialization_command(
                        &mut commands,
                        NativeUciMaterializationCommand::DeleteOption {
                            package: *package,
                            section: section.section().to_string(),
                            option: (*option).to_string(),
                        },
                    )?;
                    for value in values.values() {
                        push_materialization_command(
                            &mut commands,
                            NativeUciMaterializationCommand::AddList {
                                package: *package,
                                section: section.section().to_string(),
                                option: (*option).to_string(),
                                value: value.clone(),
                            },
                        )?;
                    }
                }
            }
        }
    }
    for package in sections.keys() {
        push_materialization_command(
            &mut commands,
            NativeUciMaterializationCommand::CommitPackage { package: *package },
        )?;
    }

    if commands.is_empty() {
        return Err(format!(
            "native UCI materialization exceeds its {MAX_PHYSICAL_COMMANDS}-command bound"
        ));
    }
    for command in &commands {
        command.validate()?;
    }
    Ok(commands)
}

fn push_materialization_command(
    commands: &mut Vec<NativeUciMaterializationCommand>,
    command: NativeUciMaterializationCommand,
) -> Result<(), String> {
    if commands.len() >= MAX_PHYSICAL_COMMANDS {
        return Err(format!(
            "native UCI materialization exceeds its {MAX_PHYSICAL_COMMANDS}-command bound"
        ));
    }
    command.validate()?;
    commands.push(command);
    Ok(())
}

fn push_option_path(
    output: &mut String,
    package: NativeManagedPackage,
    section: &str,
    option: &str,
) {
    output.push_str(package_name(package));
    output.push('.');
    output.push_str(section);
    output.push('.');
    output.push_str(option);
}

fn package_name(package: NativeManagedPackage) -> &'static str {
    match package {
        NativeManagedPackage::CakeAutorate => "cake-autorate",
        NativeManagedPackage::Sqm => "sqm",
    }
}

fn validate_package_section(package: NativeManagedPackage, section: &str) -> Result<(), String> {
    let _ = package_name(package);
    validate_uci_identifier("section", section)
}

fn validate_uci_identifier(label: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(format!(
            "native UCI materialization {label} is not a bounded identifier"
        ));
    }
    Ok(())
}

fn validate_batch_value(option: &str, value: &str) -> Result<(), String> {
    // Libuci permits TAB, LF and CR among C0 bytes. This single-line batch
    // grammar is deliberately stricter: only TAB is data, while LF/CR are
    // framing. Keep those bytes and the quote delimiter explicit here.
    let has_forbidden_control = value.bytes().any(|byte| byte < b' ' && byte != b'\t');
    if value.len() > MAX_UCI_VALUE_BYTES
        || value.contains(['\n', '\r', '\0', '\''])
        || has_forbidden_control
    {
        return Err(format!(
            "native UCI materialization value for {option} is not safely batch-encodable"
        ));
    }
    Ok(())
}

fn command_json(command: &NativeUciMaterializationCommand) -> String {
    match command {
        NativeUciMaterializationCommand::AddSection {
            package,
            section,
            section_type,
        } => format!(
            "{{\"action\":\"add_section\",\"package\":{},\"section\":{},\"section_type\":{}}}",
            json_string(package_name(*package)),
            json_string(section),
            json_string(section_type),
        ),
        NativeUciMaterializationCommand::Set {
            package,
            section,
            option,
            value,
        } => format!(
            "{{\"action\":\"set\",\"package\":{},\"section\":{},\"option\":{},\"value\":{}}}",
            json_string(package_name(*package)),
            json_string(section),
            json_string(option),
            json_string(value),
        ),
        NativeUciMaterializationCommand::DeleteOption {
            package,
            section,
            option,
        } => format!(
            "{{\"action\":\"delete_option\",\"package\":{},\"section\":{},\"option\":{}}}",
            json_string(package_name(*package)),
            json_string(section),
            json_string(option),
        ),
        NativeUciMaterializationCommand::AddList {
            package,
            section,
            option,
            value,
        } => format!(
            "{{\"action\":\"add_list\",\"package\":{},\"section\":{},\"option\":{},\"value\":{}}}",
            json_string(package_name(*package)),
            json_string(section),
            json_string(option),
            json_string(value),
        ),
        NativeUciMaterializationCommand::CommitPackage { package } => format!(
            "{{\"action\":\"commit_package\",\"package\":{}}}",
            json_string(package_name(*package)),
        ),
    }
}

fn json_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\u{0c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character.is_control() => {
                output.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => output.push(character),
        }
    }
    output.push('"');
    output
}

fn require_lower_hex_sha256(value: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("native UCI materialization digest is not canonical SHA-256".to_string());
    }
    Ok(())
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ParsedUciValue {
    Scalar(Vec<u8>),
    List(Vec<Vec<u8>>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedUciTargetSection {
    section_type: Vec<u8>,
    options: BTreeMap<Vec<u8>, ParsedUciValue>,
}

fn validate_parsed_uci_name(label: &str, value: &[u8]) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
    {
        return Err(format!("native UCI {label} is not a canonical name"));
    }
    Ok(())
}

fn verify_expected_section(
    expected: &NativeManagedUciSectionPlan,
    config: &[u8],
) -> Result<(), String> {
    let actual = UciConfigParser::new(config, expected.section().as_bytes())?.parse()?;
    if actual.section_type.as_slice() != expected.section_type().as_bytes() {
        return Err("native UCI target section has the wrong type".to_string());
    }
    if actual.options.len() != expected.options().len() {
        return Err("native UCI target section has a missing or extra option".to_string());
    }
    for (option, expected_value) in expected.options() {
        let actual_value = actual
            .options
            .get(option.as_bytes())
            .ok_or_else(|| format!("native UCI target section is missing option {option}"))?;
        match (expected_value, actual_value) {
            (NativeManagedUciValue::Scalar(expected), ParsedUciValue::Scalar(actual))
                if actual.as_slice() == expected.as_bytes() => {}
            (NativeManagedUciValue::ReplaceList(expected), ParsedUciValue::List(actual))
                if actual.len() == expected.values().len()
                    && actual
                        .iter()
                        .zip(expected.values())
                        .all(|(actual, expected)| actual.as_slice() == expected.as_bytes()) => {}
            (NativeManagedUciValue::Scalar(_), ParsedUciValue::List(_))
            | (NativeManagedUciValue::ReplaceList(_), ParsedUciValue::Scalar(_)) => {
                return Err(format!(
                    "native UCI target option {option} has the wrong scalar/list type"
                ));
            }
            _ => {
                return Err(format!(
                    "native UCI target option {option} has a different value or list order"
                ));
            }
        }
    }
    Ok(())
}

fn verify_target_absent(
    expected: &NativeManagedUciSectionPlan,
    config: &[u8],
) -> Result<(), String> {
    if UciConfigParser::new(config, expected.section().as_bytes())?
        .parse_optional()?
        .is_some()
    {
        return Err("native UCI bootstrap target section already exists".to_string());
    }
    Ok(())
}

fn canonical_section_bytes(section: &NativeManagedUciSectionPlan) -> Result<Vec<u8>, String> {
    validate_uci_identifier("section", section.section())?;
    validate_uci_identifier("section type", section.section_type())?;
    let mut output = Vec::new();
    push_bytes_bounded(&mut output, b"config ")?;
    push_bytes_bounded(&mut output, section.section_type().as_bytes())?;
    push_bytes_bounded(&mut output, b" '")?;
    push_bytes_bounded(&mut output, section.section().as_bytes())?;
    push_bytes_bounded(&mut output, b"'\n")?;
    for (option, value) in section.options() {
        validate_uci_identifier("option", option)?;
        match value {
            NativeManagedUciValue::Scalar(value) => {
                validate_batch_value(option, value)?;
                push_config_statement(&mut output, b"option", option, value)?;
            }
            NativeManagedUciValue::ReplaceList(values) => {
                for value in values.values() {
                    validate_batch_value(option, value)?;
                    push_config_statement(&mut output, b"list", option, value)?;
                }
            }
        }
    }
    if output.is_empty() || output.len() > MAX_UCI_FILE_BYTES {
        return Err("native UCI canonical section exceeds its byte bound".to_string());
    }
    Ok(output)
}

fn push_config_statement(
    output: &mut Vec<u8>,
    keyword: &[u8],
    option: &str,
    value: &str,
) -> Result<(), String> {
    push_bytes_bounded(output, b"\t")?;
    push_bytes_bounded(output, keyword)?;
    push_bytes_bounded(output, b" ")?;
    push_bytes_bounded(output, option.as_bytes())?;
    push_bytes_bounded(output, b" '")?;
    push_bytes_bounded(output, value.as_bytes())?;
    push_bytes_bounded(output, b"'\n")
}

fn push_bytes_bounded(output: &mut Vec<u8>, bytes: &[u8]) -> Result<(), String> {
    let next = output
        .len()
        .checked_add(bytes.len())
        .ok_or_else(|| "native UCI canonical section size overflowed".to_string())?;
    if next > MAX_UCI_FILE_BYTES {
        return Err("native UCI canonical section exceeds its byte bound".to_string());
    }
    output.extend_from_slice(bytes);
    Ok(())
}

struct UciConfigParser<'a> {
    input: &'a [u8],
    position: usize,
    target_name: &'a [u8],
    command_count: usize,
    section_count: usize,
    target_option_statements: usize,
    current_is_target: bool,
    target: Option<ParsedUciTargetSection>,
}

impl<'a> UciConfigParser<'a> {
    fn new(input: &'a [u8], target_name: &'a [u8]) -> Result<Self, String> {
        if input.len() > MAX_UCI_FILE_BYTES {
            return Err("native UCI config file size is outside its bound".to_string());
        }
        if input.contains(&0) || input.contains(&b'\r') {
            return Err("native UCI config contains NUL or carriage return".to_string());
        }
        validate_parsed_uci_name("target section name", target_name)?;
        Ok(Self {
            input,
            position: 0,
            target_name,
            command_count: 0,
            section_count: 0,
            target_option_statements: 0,
            current_is_target: false,
            target: None,
        })
    }

    fn parse(self) -> Result<ParsedUciTargetSection, String> {
        self.parse_optional()?
            .ok_or_else(|| "native UCI config is missing its target section".to_string())
    }

    fn parse_optional(mut self) -> Result<Option<ParsedUciTargetSection>, String> {
        while self.skip_layout()? {
            self.command_count = self
                .command_count
                .checked_add(1)
                .ok_or_else(|| "native UCI config command count overflowed".to_string())?;
            if self.command_count > MAX_UCI_FILE_COMMANDS {
                return Err("native UCI config exceeds its command-count bound".to_string());
            }

            let keyword = self.parse_uci_name_token("command")?;
            self.require_horizontal_space()?;
            match keyword.as_slice() {
                b"config" => self.parse_config_command()?,
                b"option" => self.parse_option_command(false)?,
                b"list" => self.parse_option_command(true)?,
                _ => return Err("native UCI config contains an unsupported command".to_string()),
            }
        }
        Ok(self.target)
    }

    fn parse_config_command(&mut self) -> Result<(), String> {
        self.section_count = self
            .section_count
            .checked_add(1)
            .ok_or_else(|| "native UCI config section count overflowed".to_string())?;
        if self.section_count > MAX_UCI_FILE_SECTIONS {
            return Err("native UCI config exceeds its section-count bound".to_string());
        }

        let section_type = self.parse_section_type_token()?;
        let had_space = self.skip_horizontal_space();
        let section_name = if self.at_line_end_or_comment() {
            None
        } else {
            if !had_space {
                return Err("native UCI config tokens are not separated".to_string());
            }
            let section_name = self.parse_quoted_token()?;
            validate_parsed_uci_name("section name", &section_name)?;
            Some(section_name)
        };
        self.finish_command()?;

        self.current_is_target = section_name
            .as_deref()
            .is_some_and(|name| name == self.target_name);
        if self.current_is_target {
            if self.target.is_some() {
                return Err(
                    "native UCI config contains the target section more than once".to_string(),
                );
            }
            self.target = Some(ParsedUciTargetSection {
                section_type,
                options: BTreeMap::new(),
            });
        }
        Ok(())
    }

    fn parse_option_command(&mut self, list: bool) -> Result<(), String> {
        if self.section_count == 0 {
            return Err("native UCI option appears before any config section".to_string());
        }
        let option = self.parse_uci_name_token("option")?;
        self.require_horizontal_space()?;
        let value = self.parse_quoted_token()?;
        self.finish_command()?;

        if !self.current_is_target {
            return Ok(());
        }
        self.target_option_statements = self
            .target_option_statements
            .checked_add(1)
            .ok_or_else(|| "native UCI target option count overflowed".to_string())?;
        if self.target_option_statements > MAX_UCI_TARGET_OPTION_STATEMENTS {
            return Err("native UCI target exceeds its option-statement bound".to_string());
        }
        let target = self
            .target
            .as_mut()
            .ok_or_else(|| "native UCI target parser state is inconsistent".to_string())?;
        if !target.options.contains_key(&option) && target.options.len() >= MAX_UCI_TARGET_OPTIONS {
            return Err("native UCI target exceeds its distinct-option bound".to_string());
        }

        if list {
            match target.options.entry(option) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(ParsedUciValue::List(vec![value]));
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => match entry.get_mut() {
                    ParsedUciValue::Scalar(_) => {
                        return Err(
                            "native UCI target mixes scalar and list option types".to_string()
                        );
                    }
                    ParsedUciValue::List(values) => {
                        if values.len() >= MAX_UCI_LIST_ITEMS {
                            return Err(
                                "native UCI target list exceeds its item-count bound".to_string()
                            );
                        }
                        values.push(value);
                    }
                },
            }
        } else if target
            .options
            .insert(option, ParsedUciValue::Scalar(value))
            .is_some()
        {
            return Err(
                "native UCI target contains a duplicate or mixed scalar option".to_string(),
            );
        }
        Ok(())
    }

    fn skip_layout(&mut self) -> Result<bool, String> {
        loop {
            self.skip_horizontal_space();
            if self.position >= self.input.len() {
                return Ok(false);
            }
            match self.input[self.position] {
                b'\n' => self.position += 1,
                b'#' => self.skip_comment(),
                _ => return Ok(true),
            }
        }
    }

    fn skip_comment(&mut self) {
        while self.position < self.input.len() && self.input[self.position] != b'\n' {
            self.position += 1;
        }
        if self.position < self.input.len() {
            self.position += 1;
        }
    }

    fn skip_horizontal_space(&mut self) -> bool {
        let start = self.position;
        while self.position < self.input.len() && matches!(self.input[self.position], b' ' | b'\t')
        {
            self.position += 1;
        }
        self.position != start
    }

    fn require_horizontal_space(&mut self) -> Result<(), String> {
        if self.skip_horizontal_space() {
            Ok(())
        } else {
            Err("native UCI config tokens are not separated".to_string())
        }
    }

    fn at_line_end_or_comment(&self) -> bool {
        self.position >= self.input.len() || matches!(self.input[self.position], b'\n' | b'#')
    }

    fn finish_command(&mut self) -> Result<(), String> {
        self.skip_horizontal_space();
        if self.position < self.input.len() && self.input[self.position] == b'#' {
            self.skip_comment();
            return Ok(());
        }
        if self.position >= self.input.len() {
            return Ok(());
        }
        if self.input[self.position] != b'\n' {
            return Err("native UCI config has trailing tokens on a command".to_string());
        }
        self.position += 1;
        Ok(())
    }

    fn parse_uci_name_token(&mut self, label: &str) -> Result<Vec<u8>, String> {
        let start = self.position;
        while self.position < self.input.len()
            && !matches!(self.input[self.position], b' ' | b'\t' | b'\n' | b'#')
        {
            let byte = self.input[self.position];
            if !byte.is_ascii_alphanumeric() && byte != b'_' {
                return Err(format!("native UCI {label} is not a canonical name"));
            }
            self.position += 1;
            if self.position - start > 64 {
                return Err(format!("native UCI {label} exceeds its byte bound"));
            }
        }
        if self.position == start {
            return Err(format!("native UCI {label} is empty"));
        }
        Ok(self.input[start..self.position].to_vec())
    }

    fn parse_section_type_token(&mut self) -> Result<Vec<u8>, String> {
        let mut output = Vec::new();
        loop {
            let Some(&byte) = self.input.get(self.position) else {
                break;
            };
            if matches!(byte, b' ' | b'\t' | b'\n' | b'#') {
                break;
            }
            if self.input.get(self.position..self.position + 4) == Some(b"'\\''") {
                output.push(b'\'');
                self.position += 4;
            } else {
                // Libuci permits printable ASCII in section types.  Its
                // canonical writer only escapes apostrophes; the remaining
                // grammar delimiters cannot be represented losslessly and are
                // therefore rejected rather than normalized here.
                if !byte.is_ascii_graphic() || matches!(byte, b'\'' | b'"' | b'\\' | b';') {
                    return Err("native UCI section type is not canonically encoded".to_string());
                }
                output.push(byte);
                self.position += 1;
            }
            if output.len() > 64 {
                return Err("native UCI section type exceeds its byte bound".to_string());
            }
        }
        if output.is_empty() {
            return Err("native UCI section type is empty".to_string());
        }
        Ok(output)
    }

    fn parse_quoted_token(&mut self) -> Result<Vec<u8>, String> {
        if self.position >= self.input.len() || self.input[self.position] != b'\'' {
            return Err("native UCI value is not a canonical single-quoted token".to_string());
        }
        self.position += 1;
        let mut output = Vec::new();
        loop {
            let byte = *self
                .input
                .get(self.position)
                .ok_or_else(|| "native UCI value has an unterminated quote".to_string())?;
            if byte == b'\'' {
                if self.input.get(self.position..self.position + 4) == Some(b"'\\''") {
                    if output.len() >= MAX_UCI_TOKEN_BYTES {
                        return Err("native UCI token exceeds its byte bound".to_string());
                    }
                    output.push(b'\'');
                    self.position += 4;
                    continue;
                }
                self.position += 1;
                break;
            }
            if byte == 0 || byte == b'\r' {
                return Err("native UCI quoted token contains a forbidden byte".to_string());
            }
            if output.len() >= MAX_UCI_TOKEN_BYTES {
                return Err("native UCI token exceeds its byte bound".to_string());
            }
            output.push(byte);
            self.position += 1;
        }
        if self.position < self.input.len()
            && !matches!(self.input[self.position], b' ' | b'\t' | b'\n' | b'#')
        {
            return Err("native UCI quoted token has a non-canonical suffix".to_string());
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::autotune::{
        build_proposal_for_profile_with_context, AccessEvidenceSource, AccessMedium,
        AutotuneProfile, CapacityLearningPolicy, LatencyBaseline, LinkKind, ProposalContext,
    };
    use crate::operations::autotune_apply::{
        NativeApplyAcknowledgement, NativeApplyAction, NativeApplyArtifactDigests,
        NativeApplyDirectionInput, NativeApplyDirectionMode, NativeApplyExecutionPlan,
        NativeApplyManifestInput, NativeSqmDirectionMode,
    };
    use crate::operations::autotune_managed_config::{
        BootstrapApplyInputs, NativeBootstrapPersistPolicy,
    };
    use crate::operations::protocol::{
        CalibrationStrategy, OperationIdentity, OperationKind, OperationOrigin, OperationRequest,
        OperationRouteIdentity, OperationRouteMode, OperationTargetState,
    };
    use std::net::{IpAddr, Ipv4Addr};

    const REVIEW_DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn request(profile: AutotuneProfile) -> OperationRequest {
        OperationRequest {
            identity: OperationIdentity {
                job_id: "11".repeat(16),
                job_token: "22".repeat(32),
                instance: "wan_sqm".to_string(),
                operation: OperationKind::FullAutotune,
                target_interface: "pppoe-wan".to_string(),
                route_fingerprint: "33".repeat(32),
                config_fingerprint: "44".repeat(32),
                sqm_fingerprint: "55".repeat(32),
            },
            created_unix_ms: 1_000,
            deadline_unix_ms: 2_000,
            origin: OperationOrigin::Luci,
            backend: "speedtest-go".to_string(),
            speedtest_direction: None,
            speedtest_server_id: Some(17_372),
            speedtest_topology: None,
            route: OperationRouteIdentity {
                mode: OperationRouteMode::Main,
                mwan3_member: None,
                l3_device: "pppoe-wan".to_string(),
                source_ip: Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2))),
                fwmark: None,
                routing_table: None,
            },
            target_state: OperationTargetState::AbsentBootstrap,
            capture_policy: Some(
                crate::operations::autotune_capture_policy::AutotuneCapturePolicyId::StandardV1,
            ),
            managed_sqm_section: Some("cake_wan_sqm".to_string()),
            profile: Some(profile),
            strategy: Some(CalibrationStrategy::FullRaw),
            access_medium: Some(AccessMedium::SharedWired),
            access_source: Some(AccessEvidenceSource::UserSelected),
            access_confidence_percent: 100,
            capacity_learning_policy: Some(CapacityLearningPolicy::VerifiedOnly),
            service_dl_cap_kbps: None,
            service_ul_cap_kbps: None,
            allow_sqm_disable: true,
            allow_active_traffic: false,
            scheduled_auto_apply_requested: false,
            traffic_budget_bytes: 1_000_000_000,
        }
    }

    fn managed_config(profile: AutotuneProfile) -> NativeManagedConfigPlan {
        let request = request(profile);
        let mut proposal = build_proposal_for_profile_with_context(
            &[900_000.0, 910_000.0, 920_000.0],
            &[90_000.0, 95_000.0, 100_000.0],
            LatencyBaseline {
                median_ms: 10.0,
                p95_ms: 12.0,
                samples: 10,
            },
            LinkKind::Pppoe,
            profile,
            ProposalContext {
                access_medium: Some(AccessMedium::SharedWired),
                access_source: AccessEvidenceSource::UserSelected,
                access_confidence_percent: 100,
                capacity_learning_policy: Some(CapacityLearningPolicy::VerifiedOnly),
                download_service_cap_kbps: None,
                upload_service_cap_kbps: None,
            },
        )
        .unwrap();
        proposal
            .set_tested_safe_maximums(
                Some(proposal.download.base_kbps),
                Some(proposal.upload.base_kbps),
            )
            .unwrap();

        let apply = NativeApplyExecutionPlan::from_verified_input(NativeApplyManifestInput {
            option_id: "safe_candidate",
            request: &request,
            worker_run_id: &"66".repeat(16),
            review_digest: REVIEW_DIGEST,
            coordinator_boot_id: "boot-id",
            coordinator_generation: &"77".repeat(16),
            selected_topology: "both_shaped",
            action: NativeApplyAction::ApplySqm,
            sqm_direction_mode: NativeSqmDirectionMode::Both,
            download: NativeApplyDirectionInput {
                mode: NativeApplyDirectionMode::Shaped,
                selected_kbps: Some(proposal.download.base_kbps),
                measured_runtime_minimum_kbps: None,
                proposal: proposal.download,
            },
            upload: NativeApplyDirectionInput {
                mode: NativeApplyDirectionMode::Shaped,
                selected_kbps: Some(proposal.upload.base_kbps),
                measured_runtime_minimum_kbps: None,
                proposal: proposal.upload,
            },
            proposal: &proposal,
            auto_apply_evidence_pass: false,
            manual_review_required: true,
            required_acknowledgements: &[NativeApplyAcknowledgement::MeasurementConfidence],
            artifacts: NativeApplyArtifactDigests {
                proposal: &"88".repeat(32),
                download_search: &"99".repeat(32),
                upload_search: &"aa".repeat(32),
                pair_confirmation: &"bb".repeat(32),
                topology_comparison: &"cc".repeat(32),
            },
        })
        .unwrap();
        let capture_policy =
            crate::operations::autotune_capture_policy::AutotuneCapturePolicyId::StandardV1
                .expand()
                .unwrap();
        NativeManagedConfigPlan::from_bootstrap(BootstrapApplyInputs {
            verified_apply: &apply,
            capture_policy: &capture_policy,
            persist_policy: NativeBootstrapPersistPolicy::defaults_v1(),
        })
        .unwrap()
    }

    fn render_section(section: &NativeManagedUciSectionPlan) -> Vec<u8> {
        let mut output = format!(
            "config {} '{}'\n",
            section.section_type(),
            section.section()
        );
        for (option, value) in section.options() {
            match value {
                NativeManagedUciValue::Scalar(value) => {
                    assert!(!value.contains('\''));
                    output.push_str(&format!("\toption {option} '{value}'\n"));
                }
                NativeManagedUciValue::ReplaceList(values) => {
                    for value in values.values() {
                        assert!(!value.contains('\''));
                        output.push_str(&format!("\tlist {option} '{value}'\n"));
                    }
                }
            }
        }
        output.into_bytes()
    }

    fn parsed_list<'a>(section: &'a ParsedUciTargetSection, option: &[u8]) -> &'a [Vec<u8>] {
        match section.options.get(option).unwrap() {
            ParsedUciValue::List(values) => values,
            ParsedUciValue::Scalar(_) => panic!("expected list"),
        }
    }

    #[test]
    fn golden_125_action_plan_is_bound_ordered_and_deterministic() {
        let managed = managed_config(AutotuneProfile::BestOverall);
        assert_eq!(managed.action_count(), 125);
        assert_eq!(
            managed.canonical_sha256().unwrap(),
            "3d146bba907d91f8900825066ac72db4383047b0bd562a181c75cd769b502474"
        );

        let plan = NativeUciMaterializationPlan::from_managed_config(&managed).unwrap();
        assert_eq!(plan.logical_action_count(), 125);
        assert_eq!(plan.command_count(), 157);
        assert_eq!(
            plan.managed_config_sha256(),
            managed.canonical_sha256().unwrap()
        );
        assert!(matches!(
            &plan.commands[0],
            NativeUciMaterializationCommand::AddSection {
                package: NativeManagedPackage::CakeAutorate,
                section,
                section_type,
            } if section == "wan_sqm" && section_type == "cake_autorate"
        ));
        assert!(matches!(
            &plan.commands[1],
            NativeUciMaterializationCommand::AddSection {
                package: NativeManagedPackage::Sqm,
                section,
                section_type,
            } if section == "cake_wan_sqm" && section_type == "queue"
        ));
        assert!(matches!(
            &plan.commands[plan.commands.len() - 2..],
            [
                NativeUciMaterializationCommand::CommitPackage {
                    package: NativeManagedPackage::CakeAutorate,
                },
                NativeUciMaterializationCommand::CommitPackage {
                    package: NativeManagedPackage::Sqm,
                }
            ]
        ));
        let second = NativeUciMaterializationPlan::from_managed_config(&managed).unwrap();
        assert_eq!(
            plan.canonical_bytes().unwrap(),
            second.canonical_bytes().unwrap()
        );
        assert_eq!(
            plan.canonical_sha256().unwrap(),
            second.canonical_sha256().unwrap()
        );
        assert_eq!(
            plan.canonical_sha256().unwrap(),
            "820b1bd627e767307819e23e2fd2bebe72c061f148b42311cfd5f0aa867a217f"
        );
    }

    #[test]
    fn raw_fallback_materializes_only_disabled_cake_and_preserves_sqm_bytes() {
        let bootstrap =
            crate::operations::autotune_bootstrap_apply::tests::fixture_raw_fallback_plan();
        let managed = bootstrap.managed_config();
        assert!(managed.sqm().is_none());
        let plan = NativeUciMaterializationPlan::from_managed_config(managed).unwrap();
        assert_eq!(plan.logical_action_count(), managed.action_count());
        assert!(plan
            .sections
            .contains_key(&NativeManagedPackage::CakeAutorate));
        assert!(!plan.sections.contains_key(&NativeManagedPackage::Sqm));
        assert!(matches!(
            &plan.commands[0],
            NativeUciMaterializationCommand::AddSection {
                package: NativeManagedPackage::CakeAutorate,
                section,
                section_type,
            } if section == "wan_sqm" && section_type == "cake_autorate"
        ));
        assert!(matches!(
            plan.commands.last(),
            Some(NativeUciMaterializationCommand::CommitPackage {
                package: NativeManagedPackage::CakeAutorate,
            })
        ));
        assert!(!plan.commands.iter().any(|command| matches!(
            command,
            NativeUciMaterializationCommand::AddSection {
                package: NativeManagedPackage::Sqm,
                ..
            } | NativeUciMaterializationCommand::CommitPackage {
                package: NativeManagedPackage::Sqm,
            }
        )));

        let cake_original = b"config defaults 'globals'\n\toption enabled '1'\n";
        let sqm_original = b"config queue 'foreign'\n\toption enabled '1'\n";
        let pair = plan
            .deterministic_candidate_pair(cake_original, sqm_original)
            .unwrap();
        assert_ne!(pair.cake(), cake_original);
        assert_eq!(pair.sqm(), sqm_original);
        plan.verify_deterministic_candidate_pair(cake_original, sqm_original, &pair)
            .unwrap();
    }

    #[test]
    fn batch_expands_list_in_source_order_after_sorted_section_prefix() {
        let managed = managed_config(AutotuneProfile::BestOverall);
        let plan = NativeUciMaterializationPlan::from_managed_config(&managed).unwrap();
        let batch = String::from_utf8(plan.canonical_uci_batch().unwrap()).unwrap();
        assert!(batch
            .starts_with("set cake-autorate.wan_sqm=cake_autorate\nset sqm.cake_wan_sqm=queue\n"));
        let positions = [
            "add_list cake-autorate.wan_sqm.reflector='1.1.1.1'",
            "add_list cake-autorate.wan_sqm.reflector='1.0.0.1'",
            "add_list cake-autorate.wan_sqm.reflector='8.8.8.8'",
            "add_list cake-autorate.wan_sqm.reflector='8.8.4.4'",
            "add_list cake-autorate.wan_sqm.reflector='9.9.9.9'",
            "add_list cake-autorate.wan_sqm.reflector='9.9.9.10'",
        ]
        .map(|needle| batch.find(needle).unwrap());
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(batch.contains("delete cake-autorate.wan_sqm.reflector\n"));
        assert!(batch.ends_with("commit cake-autorate\ncommit sqm\n"));
    }

    #[test]
    fn exact_verifier_accepts_generated_sections_and_rejects_key_or_type_drift() {
        let managed = managed_config(AutotuneProfile::BestOverall);
        let plan = NativeUciMaterializationPlan::from_managed_config(&managed).unwrap();
        let cake = render_section(managed.cake());
        let sqm = render_section(managed.sqm().unwrap());
        plan.verify_exact_config_files(&cake, &sqm).unwrap();

        let wrong_type = String::from_utf8(cake.clone()).unwrap().replacen(
            "config cake_autorate",
            "config queue",
            1,
        );
        assert!(plan
            .verify_exact_package(NativeManagedPackage::CakeAutorate, wrong_type.as_bytes())
            .unwrap_err()
            .contains("wrong type"));

        let missing = String::from_utf8(cake.clone())
            .unwrap()
            .lines()
            .filter(|line| !line.starts_with("\toption connection_active_thr_kbps "))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        assert!(plan
            .verify_exact_package(NativeManagedPackage::CakeAutorate, missing.as_bytes())
            .unwrap_err()
            .contains("missing or extra"));

        let mut extra = cake;
        extra.extend_from_slice(b"\toption unexpected '1'\n");
        assert!(plan
            .verify_exact_package(NativeManagedPackage::CakeAutorate, &extra)
            .unwrap_err()
            .contains("missing or extra"));
    }

    #[test]
    fn deterministic_candidate_pair_preserves_every_original_byte_and_appends_only_targets() {
        let managed = managed_config(AutotuneProfile::BestOverall);
        let plan = NativeUciMaterializationPlan::from_managed_config(&managed).unwrap();
        let cake_original = b"# exact original\nconfig other 'keep'\n\toption value 'a#b'\n";
        let sqm_original = b"config other_queue 'keep'\n\toption enabled '1'";
        let pair = plan
            .deterministic_candidate_pair(cake_original, sqm_original)
            .unwrap();

        let mut expected_cake = cake_original.to_vec();
        expected_cake.extend_from_slice(&render_section(managed.cake()));
        let mut expected_sqm = sqm_original.to_vec();
        expected_sqm.push(b'\n');
        expected_sqm.extend_from_slice(&render_section(managed.sqm().unwrap()));
        assert_eq!(pair.cake(), expected_cake);
        assert_eq!(pair.sqm(), expected_sqm);
        assert!(pair.cake().starts_with(cake_original));
        assert!(pair.sqm().starts_with(sqm_original));
        plan.verify_exact_config_files(pair.cake(), pair.sqm())
            .unwrap();
        plan.verify_deterministic_candidate_pair(cake_original, sqm_original, &pair)
            .unwrap();
    }

    #[test]
    fn deterministic_candidate_rejects_existing_target_and_arbitrary_candidate_bytes() {
        let managed = managed_config(AutotuneProfile::BestOverall);
        let plan = NativeUciMaterializationPlan::from_managed_config(&managed).unwrap();
        let cake_original = render_section(managed.cake());
        assert!(plan
            .deterministic_candidate_pair(&cake_original, b"")
            .unwrap_err()
            .contains("already exists"));

        let original_cake = b"config other 'keep'\n";
        let original_sqm = b"";
        let pair = plan
            .deterministic_candidate_pair(original_cake, original_sqm)
            .unwrap();
        let mut foreign = pair.clone();
        foreign.cake.splice(
            original_cake.len()..original_cake.len(),
            b"config injected 'foreign'\n".iter().copied(),
        );
        assert!(plan
            .verify_deterministic_candidate_pair(original_cake, original_sqm, &foreign)
            .unwrap_err()
            .contains("deterministic append transition"));
    }

    #[test]
    fn one_item_scalar_and_list_are_distinct_and_exact_type_is_enforced() {
        let scalar = UciConfigParser::new(
            b"config cake_autorate 'wan_sqm'\n\toption reflector '1.1.1.1'\n",
            b"wan_sqm",
        )
        .unwrap()
        .parse()
        .unwrap();
        assert!(matches!(
            scalar.options.get(b"reflector".as_slice()),
            Some(ParsedUciValue::Scalar(value)) if value == b"1.1.1.1"
        ));
        let list = UciConfigParser::new(
            b"config cake_autorate 'wan_sqm'\n\tlist reflector '1.1.1.1'\n",
            b"wan_sqm",
        )
        .unwrap()
        .parse()
        .unwrap();
        assert_eq!(parsed_list(&list, b"reflector"), &[b"1.1.1.1".to_vec()]);

        let managed = managed_config(AutotuneProfile::BestOverall);
        let plan = NativeUciMaterializationPlan::from_managed_config(&managed).unwrap();
        let cake = String::from_utf8(render_section(managed.cake())).unwrap();
        let mut replaced = false;
        let scalar_instead = cake
            .lines()
            .filter_map(|line| {
                if line.starts_with("\tlist reflector ") {
                    if replaced {
                        None
                    } else {
                        replaced = true;
                        Some("\toption reflector '1.1.1.1'")
                    }
                } else {
                    Some(line)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        assert!(plan
            .verify_exact_package(
                NativeManagedPackage::CakeAutorate,
                scalar_instead.as_bytes(),
            )
            .unwrap_err()
            .contains("wrong scalar/list type"));
    }

    #[test]
    fn parser_preserves_list_order_and_duplicates_without_normalization() {
        let ordered = UciConfigParser::new(
            concat!(
                "config cake_autorate 'wan_sqm'\n",
                "\tlist reflector 'first'\n",
                "\tlist reflector 'second'\n"
            )
            .as_bytes(),
            b"wan_sqm",
        )
        .unwrap()
        .parse()
        .unwrap();
        assert_eq!(
            parsed_list(&ordered, b"reflector"),
            &[b"first".to_vec(), b"second".to_vec()]
        );
        let duplicate = UciConfigParser::new(
            concat!(
                "config cake_autorate 'wan_sqm'\n",
                "\tlist reflector 'same'\n",
                "\tlist reflector 'same'\n"
            )
            .as_bytes(),
            b"wan_sqm",
        )
        .unwrap()
        .parse()
        .unwrap();
        assert_eq!(
            parsed_list(&duplicate, b"reflector"),
            &[b"same".to_vec(), b"same".to_vec()]
        );

        let managed = managed_config(AutotuneProfile::BestOverall);
        let plan = NativeUciMaterializationPlan::from_managed_config(&managed).unwrap();
        let cake = String::from_utf8(render_section(managed.cake())).unwrap();
        let first = "\tlist reflector '1.1.1.1'\n";
        let second = "\tlist reflector '1.0.0.1'\n";
        let reversed = cake.replacen(&format!("{first}{second}"), &format!("{second}{first}"), 1);
        assert!(plan
            .verify_exact_package(NativeManagedPackage::CakeAutorate, reversed.as_bytes())
            .unwrap_err()
            .contains("different value or list order"));
        let duplicated = cake.replacen(second, first, 1);
        assert!(plan
            .verify_exact_package(NativeManagedPackage::CakeAutorate, duplicated.as_bytes())
            .unwrap_err()
            .contains("different value or list order"));
    }

    #[test]
    fn quoted_parser_handles_canonical_apostrophe_newline_and_non_utf8_bytes() {
        let parsed = UciConfigParser::new(
            b"config cake_autorate 'wan_sqm'\n\toption note 'can'\\''t'\n",
            b"wan_sqm",
        )
        .unwrap()
        .parse()
        .unwrap();
        assert!(matches!(
            parsed.options.get(b"note".as_slice()),
            Some(ParsedUciValue::Scalar(value)) if value == b"can't"
        ));

        let mut bytes = b"config other 'other'\n\toption blob 'first\n".to_vec();
        bytes.push(0xff);
        bytes.extend_from_slice(b"second'\nconfig cake_autorate 'wan_sqm'\n\toption enabled '1'\n");
        let target = UciConfigParser::new(&bytes, b"wan_sqm")
            .unwrap()
            .parse()
            .unwrap();
        assert!(matches!(
            target.options.get(b"enabled".as_slice()),
            Some(ParsedUciValue::Scalar(value)) if value == b"1"
        ));
    }

    #[test]
    fn parser_handles_comments_hash_values_and_explicit_name_grammars() {
        let parsed = UciConfigParser::new(
            concat!(
                "# package comment\n",
                "  # indented comment\n",
                "config service-type.v1 'other' # unrelated section\n",
                "\toption ignored 'value#inside' # unrelated option\n",
                "config cake_autorate 'wan_sqm' # target section\n",
                "\toption note 'hash#inside' # scalar comment\n",
                "\tlist reflector 'first#one' # list comment\n",
                "\tlist reflector 'second'\n",
            )
            .as_bytes(),
            b"wan_sqm",
        )
        .unwrap()
        .parse()
        .unwrap();
        assert_eq!(parsed.section_type, b"cake_autorate");
        assert!(matches!(
            parsed.options.get(b"note".as_slice()),
            Some(ParsedUciValue::Scalar(value)) if value == b"hash#inside"
        ));
        assert_eq!(
            parsed_list(&parsed, b"reflector"),
            &[b"first#one".to_vec(), b"second".to_vec()]
        );

        for bytes in [
            b"config cake_autorate 'bad-name'\n".as_slice(),
            b"config cake_autorate 'wan_sqm'\n\toption bad-name '1'\n".as_slice(),
        ] {
            assert!(UciConfigParser::new(bytes, b"wan_sqm")
                .unwrap()
                .parse()
                .is_err());
        }
    }

    #[test]
    fn malformed_grammar_and_mixed_or_duplicate_types_fail_closed() {
        for bytes in [
            b"config cake_autorate 'wan_sqm\n".as_slice(),
            b"config cake_autorate 'wan_sqm'junk\n".as_slice(),
            b"commit cake-autorate\n".as_slice(),
            b"option enabled '1'\nconfig cake_autorate 'wan_sqm'\n".as_slice(),
        ] {
            assert!(UciConfigParser::new(bytes, b"wan_sqm")
                .unwrap()
                .parse()
                .is_err());
        }
        for bytes in [
            concat!(
                "config cake_autorate 'wan_sqm'\n",
                "\toption reflector 'first'\n",
                "\tlist reflector 'second'\n"
            )
            .as_bytes(),
            concat!(
                "config cake_autorate 'wan_sqm'\n",
                "\toption enabled '1'\n",
                "\toption enabled '1'\n"
            )
            .as_bytes(),
            concat!(
                "config cake_autorate 'wan_sqm'\n",
                "config queue 'wan_sqm'\n"
            )
            .as_bytes(),
        ] {
            assert!(UciConfigParser::new(bytes, b"wan_sqm")
                .unwrap()
                .parse()
                .is_err());
        }
    }

    #[test]
    fn all_parser_and_batch_bounds_are_enforced_before_unbounded_growth() {
        assert!(UciConfigParser::new(&vec![b'x'; MAX_UCI_FILE_BYTES + 1], b"wan_sqm").is_err());

        let mut oversized_token = b"config cake_autorate 'wan_sqm'\n\toption note '".to_vec();
        oversized_token.extend(std::iter::repeat(b'x').take(MAX_UCI_TOKEN_BYTES + 1));
        oversized_token.extend_from_slice(b"'\n");
        assert!(UciConfigParser::new(&oversized_token, b"wan_sqm")
            .unwrap()
            .parse()
            .unwrap_err()
            .contains("token exceeds"));

        let mut too_many_list_items = b"config cake_autorate 'wan_sqm'\n".to_vec();
        for _ in 0..=MAX_UCI_LIST_ITEMS {
            too_many_list_items.extend_from_slice(b"\tlist reflector 'same'\n");
        }
        assert!(UciConfigParser::new(&too_many_list_items, b"wan_sqm")
            .unwrap()
            .parse()
            .unwrap_err()
            .contains("item-count bound"));

        let mut too_many_options = b"config cake_autorate 'wan_sqm'\n".to_vec();
        for index in 0..=MAX_UCI_TARGET_OPTIONS {
            too_many_options.extend_from_slice(format!("\toption o{index} '1'\n").as_bytes());
        }
        assert!(UciConfigParser::new(&too_many_options, b"wan_sqm")
            .unwrap()
            .parse()
            .unwrap_err()
            .contains("distinct-option bound"));

        let unsafe_commands = [
            NativeUciMaterializationCommand::Set {
                package: NativeManagedPackage::Sqm,
                section: "safe".to_string(),
                option: "enabled".to_string(),
                value: "x'\ncommit sqm".to_string(),
            },
            NativeUciMaterializationCommand::Set {
                package: NativeManagedPackage::Sqm,
                section: "safe".to_string(),
                option: "enabled".to_string(),
                value: "x\0y".to_string(),
            },
            NativeUciMaterializationCommand::Set {
                package: NativeManagedPackage::Sqm,
                section: "safe".to_string(),
                option: "enabled".to_string(),
                value: "x".repeat(MAX_UCI_VALUE_BYTES + 1),
            },
            NativeUciMaterializationCommand::Set {
                package: NativeManagedPackage::Sqm,
                section: "safe".to_string(),
                option: "enabled".to_string(),
                value: "c0\x01byte".to_string(),
            },
            NativeUciMaterializationCommand::Set {
                package: NativeManagedPackage::Sqm,
                section: "safe".to_string(),
                option: "enabled".to_string(),
                value: "escape\x1bbyte".to_string(),
            },
        ];
        assert!(unsafe_commands
            .iter()
            .all(|command| command.validate().is_err()));
        for value in ["tab\tseparated", "delete\x7f"] {
            NativeUciMaterializationCommand::Set {
                package: NativeManagedPackage::Sqm,
                section: "safe".to_string(),
                option: "enabled".to_string(),
                value: value.to_string(),
            }
            .validate()
            .unwrap();
        }

        let managed = managed_config(AutotuneProfile::BestOverall);
        let mut plan = NativeUciMaterializationPlan::from_managed_config(&managed).unwrap();
        plan.logical_action_count = MAX_LOGICAL_ACTIONS + 1;
        assert!(plan
            .validate()
            .unwrap_err()
            .contains("logical action count"));
        let mut plan = NativeUciMaterializationPlan::from_managed_config(&managed).unwrap();
        plan.commands
            .resize(MAX_PHYSICAL_COMMANDS + 1, plan.commands[0].clone());
        assert!(plan
            .validate()
            .unwrap_err()
            .contains("physical command count"));

        let mut commands = vec![
            NativeUciMaterializationCommand::CommitPackage {
                package: NativeManagedPackage::Sqm,
            };
            MAX_PHYSICAL_COMMANDS
        ];
        assert!(push_materialization_command(
            &mut commands,
            NativeUciMaterializationCommand::CommitPackage {
                package: NativeManagedPackage::CakeAutorate,
            },
        )
        .unwrap_err()
        .contains("command bound"));
        assert_eq!(commands.len(), MAX_PHYSICAL_COMMANDS);
    }

    #[test]
    fn materialization_binding_changes_with_managed_config_and_rejects_foreign_plan() {
        let best = managed_config(AutotuneProfile::BestOverall);
        let fair = managed_config(AutotuneProfile::Fair);
        let plan = NativeUciMaterializationPlan::from_managed_config(&best).unwrap();
        plan.ensure_bound_to(&best).unwrap();
        assert!(plan.ensure_bound_to(&fair).is_err());
        let fair_plan = NativeUciMaterializationPlan::from_managed_config(&fair).unwrap();
        assert_ne!(
            plan.managed_config_sha256(),
            fair_plan.managed_config_sha256()
        );
        assert_ne!(
            plan.canonical_sha256().unwrap(),
            fair_plan.canonical_sha256().unwrap()
        );

        let mut internally_consistent_tamper = plan.clone();
        internally_consistent_tamper
            .sections
            .insert(NativeManagedPackage::CakeAutorate, fair.cake().clone());
        internally_consistent_tamper.commands =
            materialize_commands(&internally_consistent_tamper.sections).unwrap();
        internally_consistent_tamper.validate().unwrap();
        assert!(internally_consistent_tamper
            .ensure_bound_to(&best)
            .unwrap_err()
            .contains("not bound"));
    }
}
