//! Bounded, lossless preparation of scalar edits to named managed UCI sections.
//! Untouched statements/comments, including anonymous foreign sections, retain
//! their original bytes. This is a renderer, not publication or authorization.
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

const MAX_BYTES: usize = 1024 * 1024;
const MAX_STATEMENTS: usize = 32 * 1024;
const MAX_EDITS: usize = 4096;
type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Edit {
    AddSection {
        section: String,
        kind: String,
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

#[derive(Default)]
struct SectionPatch {
    add: Option<String>,
    delete: bool,
    options: BTreeMap<String, Option<String>>,
}
struct Statement {
    span: Range<usize>,
    token_end: usize,
    words: Vec<Vec<u8>>,
    section: Option<String>,
}

fn name(value: &[u8]) -> Result<String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_')
    {
        return Err("uci-edit-name-invalid".into());
    }
    Ok(String::from_utf8(value.to_vec()).expect("ASCII name"))
}
fn quote(value: &str) -> Result<String> {
    if value.len() > 4096 || value.chars().any(char::is_control) {
        return Err("uci-edit-value-invalid".into());
    }
    Ok(format!("'{}'", value.replace('\'', "'\\''")))
}

/// UCI quoting, not shell execution: quote fragments concatenate and backslash
/// escapes/line continuations apply outside single quotes (including CRLF).
fn token(input: &[u8], position: &mut usize) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut quoted = None;
    let start = *position;
    while let Some(&byte) = input.get(*position) {
        match quoted {
            Some(b'\'') => {
                *position += 1;
                if byte == b'\'' {
                    quoted = None;
                } else {
                    output.push(byte);
                }
            }
            _ => {
                if quoted == Some(b'"') && byte == b'"' {
                    *position += 1;
                    quoted = None;
                    continue;
                }
                if quoted.is_none() {
                    if byte.is_ascii_whitespace() || matches!(byte, b';' | b'#') {
                        break;
                    }
                    if matches!(byte, b'\'' | b'"') {
                        quoted = Some(byte);
                        *position += 1;
                        continue;
                    }
                }
                *position += 1;
                if byte == b'\\' {
                    let next = *input.get(*position).ok_or("uci-edit-dangling-escape")?;
                    *position += 1;
                    if next == b'\n' {
                        continue;
                    }
                    if next == b'\r' && input.get(*position) == Some(&b'\n') {
                        *position += 1;
                        continue;
                    }
                    output.push(next);
                } else {
                    output.push(byte);
                }
            }
        }
    }
    if quoted.is_some() || *position == start {
        return Err("uci-edit-token-invalid".into());
    }
    Ok(output)
}

fn statements(input: &[u8]) -> Result<(Vec<Statement>, BTreeMap<String, String>)> {
    if input.len() > MAX_BYTES || input.contains(&0) {
        return Err("uci-edit-input-invalid".into());
    }
    let mut position = 0;
    let mut result = Vec::new();
    let mut sections = BTreeMap::new();
    let mut current = None;
    while position < input.len() {
        if input[position].is_ascii_whitespace() || input[position] == b';' {
            position += 1;
            continue;
        }
        if input[position] == b'#' {
            while position < input.len() && input[position] != b'\n' {
                position += 1;
            }
            continue;
        }
        if result.len() == MAX_STATEMENTS {
            return Err("uci-edit-statement-limit".into());
        }
        let start = position;
        let mut end;
        let mut words = Vec::new();
        loop {
            if words.len() == 4 {
                return Err("uci-edit-too-many-tokens".into());
            }
            words.push(token(input, &mut position)?);
            end = position;
            while position < input.len()
                && input[position].is_ascii_whitespace()
                && input[position] != b'\n'
            {
                position += 1;
            }
            match input.get(position) {
                Some(b'#') => {
                    while position < input.len() && input[position] != b'\n' {
                        position += 1;
                    }
                }
                Some(b';' | b'\n') | None => {}
                _ => continue,
            }
            if position < input.len() {
                position += 1;
            }
            break;
        }
        // libuci also accepts these one-letter command spellings. Preserve
        // their original bytes; normalize only the internal statement kind.
        words[0] = match words[0].as_slice() {
            b"p" => b"package".to_vec(),
            b"c" => b"config".to_vec(),
            b"o" => b"option".to_vec(),
            b"l" => b"list".to_vec(),
            _ => words[0].clone(),
        };
        match words[0].as_slice() {
            b"package" if words.len() == 2 => {} // libuci file load is single-package.
            b"config" if (2..=3).contains(&words.len()) => {
                current = if words.len() == 3 && !words[2].is_empty() {
                    Some(name(&words[2])?)
                } else {
                    None
                };
                if let Some(section) = &current {
                    let kind = std::str::from_utf8(&words[1])
                        .map_err(|_| "uci-edit-kind-invalid")?
                        .to_string();
                    if sections
                        .insert(section.clone(), kind.clone())
                        .is_some_and(|previous| previous != kind)
                    {
                        return Err("uci-edit-section-type-ambiguous".into());
                    }
                }
            }
            b"option" | b"list" if (2..=3).contains(&words.len()) => {
                name(&words[1])?;
            }
            _ => return Err("uci-edit-command-invalid".into()),
        }
        result.push(Statement {
            span: start..position,
            token_end: end,
            words,
            section: current.clone(),
        });
    }
    Ok((result, sections))
}

pub(crate) fn render(original: &[u8], edits: &[Edit]) -> Result<Vec<u8>> {
    if edits.len() > MAX_EDITS {
        return Err("uci-edit-count-limit".into());
    }
    let mut edit_bytes = 0usize;
    for edit in edits {
        let fields: &[&str] = match edit {
            Edit::AddSection { section, kind } => &[section, kind],
            Edit::DeleteSection { section } => &[section],
            Edit::Set {
                section,
                option,
                value,
            } => &[section, option, value],
            Edit::Delete { section, option } => &[section, option],
        };
        for field in fields {
            edit_bytes = edit_bytes
                .checked_add(field.len())
                .ok_or("uci-edit-payload-limit")?;
            if edit_bytes > MAX_BYTES {
                return Err("uci-edit-payload-limit".into());
            }
        }
    }
    let (statements, sections) = statements(original)?;
    let mut patches = BTreeMap::<String, SectionPatch>::new();
    for edit in edits {
        let section = match edit {
            Edit::AddSection { section, .. }
            | Edit::DeleteSection { section }
            | Edit::Set { section, .. }
            | Edit::Delete { section, .. } => section,
        };
        name(section.as_bytes())?;
        let patch = patches.entry(section.clone()).or_default();
        match edit {
            Edit::AddSection { kind, .. } => {
                name(kind.as_bytes())?;
                if (sections.contains_key(section) && !patch.delete) || patch.add.is_some() {
                    return Err("uci-edit-add-section-already-exists".into());
                }
                patch.add = Some(kind.clone());
                patch.delete = false;
                patch.options.clear();
            }
            Edit::DeleteSection { .. } => {
                if !sections.contains_key(section) && patch.add.is_none() && !patch.delete {
                    return Err("uci-edit-target-requires-a-named-section".into());
                }
                patch.delete = true;
                patch.add = None;
                patch.options.clear();
            }
            Edit::Set { option, .. } | Edit::Delete { option, .. } => {
                name(option.as_bytes())?;
                if patch.delete || (!sections.contains_key(section) && patch.add.is_none()) {
                    return Err("uci-edit-target-requires-a-named-section".into());
                }
                let value = match edit {
                    Edit::Set { value, .. } => {
                        quote(value)?;
                        Some(value.clone())
                    }
                    _ => None,
                };
                patch.options.insert(option.clone(), value);
            }
        }
    }
    let mut replacements = Vec::<(Range<usize>, Vec<u8>)>::new();
    let mut insertions = BTreeMap::<usize, Vec<u8>>::new();
    for (section, patch) in &patches {
        let mut seen = BTreeSet::new();
        let mut last = None;
        for statement in statements
            .iter()
            .filter(|statement| statement.section.as_ref() == Some(section))
        {
            // package declarations do not belong to (or delete) a section.
            if statement.words[0] == b"package" {
                continue;
            }
            last = Some(statement.span.end);
            if patch.delete || patch.add.is_some() {
                replacements.push((statement.span.clone(), Vec::new()));
                continue;
            }
            if !matches!(statement.words[0].as_slice(), b"option" | b"list") {
                continue;
            }
            let key = name(&statement.words[1])?;
            if let Some(value) = patch.options.get(&key) {
                if let Some(value) = value.as_ref().filter(|_| seen.insert(key.clone())) {
                    replacements.push((
                        statement.span.start..statement.token_end,
                        format!("option {key} {}", quote(value)?).into_bytes(),
                    ));
                } else {
                    replacements.push((statement.span.clone(), Vec::new()));
                }
            }
        }
        if !patch.delete && patch.add.is_none() {
            let missing = patch
                .options
                .iter()
                .filter(|(key, value)| !seen.contains(*key) && value.is_some());
            for (key, value) in missing {
                let at = last.ok_or("uci-edit-target-requires-a-named-section")?;
                let bytes = insertions.entry(at).or_default();
                if bytes.is_empty() && at != 0 && original[at - 1] != b'\n' {
                    bytes.push(b'\n');
                }
                bytes.extend(
                    format!("\toption {key} {}\n", quote(value.as_ref().unwrap())?).as_bytes(),
                );
            }
        }
    }
    // New named sections are appended after any additions to the existing last
    // section. All original anonymous/foreign blocks remain untouched.
    for (section, patch) in &patches {
        if let Some(kind) = &patch.add {
            let at = original.len();
            let bytes = insertions.entry(at).or_default();
            if bytes.is_empty() && at != 0 && original[at - 1] != b'\n' {
                bytes.push(b'\n');
            }
            bytes.extend(format!("config {kind} '{section}'\n").as_bytes());
            for (option, value) in &patch.options {
                if let Some(value) = value {
                    bytes.extend(format!("\toption {option} {}\n", quote(value)?).as_bytes());
                }
            }
        }
    }
    replacements.extend(insertions.into_iter().map(|(at, bytes)| (at..at, bytes)));
    replacements.sort_by_key(|(range, _)| (range.start, range.end));
    let mut output = Vec::new();
    let mut position = 0;
    for (range, replacement) in replacements {
        if range.start < position {
            return Err("uci-edit-overlapping-ranges".into());
        }
        output.extend_from_slice(&original[position..range.start]);
        output.extend(replacement);
        position = range.end;
        if output.len() > MAX_BYTES {
            return Err("uci-edit-output-too-large".into());
        }
    }
    output.extend_from_slice(&original[position..]);
    if output.len() > MAX_BYTES {
        return Err("uci-edit-output-too-large".into());
    }
    Ok(output)
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum SectionKey {
    Named(String),
    // libuci-generated anonymous IDs change when earlier named sections are
    // deleted. Match the ordered anonymous subsequence, not unstable IDs.
    Anonymous(usize),
}
pub(super) type ShowView = BTreeMap<(SectionKey, Option<String>), String>;

/// Native `uci -X show` values remain verbatim here, including every list item.
/// The lossless renderer never substitutes its token values for native values.
pub(super) fn show_view(input: &[u8], alias: &str, show: &str) -> Result<ShowView> {
    let (statements, _) = statements(input)?;
    let mut sequence = Vec::new();
    let mut named = BTreeSet::new();
    let mut anonymous = 0;
    for statement in statements.iter().filter(|s| s.words[0] == b"config") {
        let key = match &statement.section {
            Some(name) if named.insert(name.clone()) => SectionKey::Named(name.clone()),
            Some(_) => continue,
            None => {
                let key = SectionKey::Anonymous(anonymous);
                anonymous += 1;
                key
            }
        };
        sequence.push((key, statement.words[1].clone()));
    }
    let mut sequence = sequence.into_iter();
    let mut identities = BTreeMap::new();
    let mut result = ShowView::new();
    let prefix = format!("{alias}.");
    for line in show.lines().filter(|line| !line.is_empty()) {
        if line.chars().any(char::is_control) {
            return Err("uci-edit-show-invalid".into());
        }
        let (path, value) = line.split_once('=').ok_or("uci-edit-show-invalid")?;
        let path = path.strip_prefix(&prefix).ok_or("uci-edit-show-invalid")?;
        let (section, option) = match path.split_once('.') {
            Some((section, option)) => (section, Some(name(option.as_bytes())?)),
            None => (path, None),
        };
        name(section.as_bytes())?;
        let key = if option.is_none() {
            let (key, kind) = sequence.next().ok_or("uci-edit-show-section-mismatch")?;
            if matches!(&key, SectionKey::Named(expected) if expected != section)
                || kind != value.as_bytes()
                || identities
                    .insert(section.to_string(), key.clone())
                    .is_some()
            {
                return Err("uci-edit-show-section-mismatch".into());
            }
            key
        } else {
            identities
                .get(section)
                .ok_or("uci-edit-show-section-mismatch")?
                .clone()
        };
        if result.insert((key, option), value.into()).is_some() {
            return Err("uci-edit-show-duplicate".into());
        }
    }
    if sequence.next().is_some() {
        return Err("uci-edit-show-section-mismatch".into());
    }
    Ok(result)
}

pub(super) fn expected_view(mut view: ShowView, edits: &[Edit]) -> Result<ShowView> {
    for edit in edits {
        let section = match edit {
            Edit::AddSection { section, .. }
            | Edit::DeleteSection { section }
            | Edit::Set { section, .. }
            | Edit::Delete { section, .. } => section,
        };
        let section = SectionKey::Named(name(section.as_bytes())?);
        let header = (section.clone(), None);
        match edit {
            Edit::AddSection { kind, .. } => {
                if view.insert(header, name(kind.as_bytes())?).is_some() {
                    return Err("uci-edit-show-add-existing".into());
                }
            }
            Edit::DeleteSection { .. } => {
                if view.remove(&header).is_none() {
                    return Err("uci-edit-show-target-missing".into());
                }
                view.retain(|(key, _), _| key != &section);
            }
            Edit::Set { option, value, .. } => {
                if !view.contains_key(&header) {
                    return Err("uci-edit-show-target-missing".into());
                }
                let key = (section, Some(name(option.as_bytes())?));
                if value.is_empty() {
                    // uci_set with an empty scalar deletes the option.
                    view.remove(&key);
                } else {
                    view.insert(key, quote(value)?);
                }
            }
            Edit::Delete { option, .. } => {
                if !view.contains_key(&header) {
                    return Err("uci-edit-show-target-missing".into());
                }
                view.remove(&(section, Some(name(option.as_bytes())?)));
            }
        }
    }
    Ok(view)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(section: &str, option: &str, value: &str) -> Edit {
        Edit::Set {
            section: section.into(),
            option: option.into(),
            value: value.into(),
        }
    }
    #[test]
    fn r4_uci_edits_preserve_foreign_bytes_and_unmodified_owned_fields() {
        let foreign = "# foreign comment\nconfig queue\n option interface \"foreign0\"\n list tags 'one'\n list tags 'two'\n";
        let original = format!("# heading\nconfig queue 'owned'\n option rate '10' # rate note\n option retained 'private unchanged'\n{foreign}");
        let output =
            String::from_utf8(render(original.as_bytes(), &[set("owned", "rate", "20")]).unwrap())
                .unwrap();
        assert!(output.ends_with(foreign));
        assert!(output.contains("option retained 'private unchanged'\n"));
        assert!(output.contains("option rate '20' # rate note\n"));
        assert!(output.starts_with("# heading\n"));
    }
    #[test]
    fn r4_uci_edits_handle_quote_fragments_semicolons_comments_and_continuations() {
        let input = b"package 'sqm'\r\nconfig \"queue\" 'owned'; option r\\\nate \"1\\0\";option keep 'a'\\''b#;c' # note\n";
        let output = render(
            input,
            &[set("owned", "rate", "2'0"), set("owned", "new", "yes")],
        )
        .unwrap();
        let text = String::from_utf8(output.clone()).unwrap();
        assert!(text.contains("option rate '2'\\''0';option keep 'a'\\''b#;c' # note\n"));
        assert!(text.ends_with("\toption new 'yes'\n"));
        let (parsed, _) = statements(&output).unwrap();
        assert!(parsed
            .iter()
            .any(|s| s.words == [b"option".to_vec(), b"rate".to_vec(), b"2'0".to_vec()]));
    }
    #[test]
    fn r4_uci_edits_replace_all_shadowed_scalar_and_list_definitions() {
        let input = b"config queue 'owned'\n list value 'old1'\n list value 'old2'\nconfig queue 'owned'\n option value 'old3'\n option other 'keep'\n";
        let output = render(input, &[set("owned", "value", "new")]).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert_eq!(text.matches("option value 'new'").count(), 1);
        for old in ["old1", "old2", "old3"] {
            assert!(!text.contains(old));
        }
        assert!(text.contains("option other 'keep'"));
    }
    #[test]
    fn r4_uci_edits_delete_owned_fields_and_sections_without_removing_foreign_blocks() {
        let foreign = "# foreign\nconfig queue 'other'\n option x 'keep'\n";
        let input =
            format!("config queue 'owned'\n option remove 'old'\n option keep 'yes'\n{foreign}");
        let output = render(
            input.as_bytes(),
            &[Edit::Delete {
                section: "owned".into(),
                option: "remove".into(),
            }],
        )
        .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("option keep 'yes'"));
        assert!(!output.contains("option remove"));
        assert!(output.ends_with(foreign));
        let output = String::from_utf8(
            render(
                input.as_bytes(),
                &[Edit::DeleteSection {
                    section: "owned".into(),
                }],
            )
            .unwrap(),
        )
        .unwrap();
        assert!(output.ends_with(foreign));
        assert!(!output.contains("owned"));
        assert!(!output.contains("option keep"));
    }
    #[test]
    fn r4_uci_edits_add_and_recreate_named_sections_in_action_order() {
        let input = b"config queue 'owned'\n option stale 'discard'";
        let edits = [
            Edit::DeleteSection {
                section: "owned".into(),
            },
            Edit::AddSection {
                section: "owned".into(),
                kind: "queue".into(),
            },
            set("owned", "fresh", "1"),
            Edit::AddSection {
                section: "added".into(),
                kind: "queue".into(),
            },
            set("added", "value", "2"),
        ];
        let output = String::from_utf8(render(input, &edits).unwrap()).unwrap();
        assert!(!output.contains("stale"));
        assert_eq!(output.matches("config queue 'owned'").count(), 1);
        assert!(output.contains("option fresh '1'"));
        assert!(output.contains("option value '2'"));
        statements(output.as_bytes()).unwrap();
    }
    #[test]
    fn r4_uci_edits_eof_insertions_do_not_attach_fields_to_the_next_section() {
        let input = b"config queue 'owned';option old '1' # trailing";
        let edits = [
            set("owned", "new", "2"),
            Edit::AddSection {
                section: "added".into(),
                kind: "queue".into(),
            },
            set("added", "own", "3"),
        ];
        let output = render(input, &edits).unwrap();
        let (parsed, _) = statements(&output).unwrap();
        assert!(parsed
            .iter()
            .any(|s| s.section.as_deref() == Some("owned")
                && s.words.get(1) == Some(&b"new".to_vec())));
        assert!(parsed
            .iter()
            .any(|s| s.section.as_deref() == Some("added")
                && s.words.get(1) == Some(&b"own".to_vec())));
    }
    #[test]
    fn r4_uci_edits_reject_ambiguous_unsafe_or_oversized_requests_without_echo() {
        for input in [
            b"config queue 'unterminated".as_slice(),
            b"unknown command\n",
            b"config queue 'owned'\0",
            b"config queue 'owned'\nconfig other 'owned'\n",
        ] {
            assert!(render(input, &[]).is_err());
        }
        let input = b"config queue 'owned'\n";
        for edit in [
            set("missing", "x", "1"),
            set("owned", "../path", "private-value"),
            set("owned", "x", "private\nvalue"),
        ] {
            let error = render(input, &[edit]).err().unwrap();
            assert!(!error.contains("private"));
        }
        assert!(render(input, &vec![set("owned", "x", "1"); MAX_EDITS + 1]).is_err());
        assert_eq!(
            render(input, &vec![set("owned", "x", &"v".repeat(4096)); 257])
                .err()
                .unwrap(),
            "uci-edit-payload-limit"
        );
        assert_eq!(render(input, &[]).unwrap(), input);
    }

    #[test]
    #[ignore = "requires explicit inspected SDK UCI and loader paths"]
    fn r4_uci_edits_real_uci_preserves_foreign_semantics_and_expected_owned_values() {
        use std::fs;
        use std::os::unix::fs::DirBuilderExt;
        use std::path::PathBuf;
        use std::process::Command;
        use std::time::{SystemTime, UNIX_EPOCH};
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("cake-r4-edits-{}-{nonce}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let config = root.join("config");
        let overrides = root.join("overrides");
        let delta = root.join("delta");
        for path in [&config, &overrides, &delta] {
            fs::create_dir(path).unwrap();
        }
        let package = format!("r4edit_{nonce:x}");
        let uci = std::env::var_os("CAKE_TEST_UCI").expect("explicit UCI path required");
        let loader = std::env::var_os("CAKE_TEST_MUSL_LOADER");
        let query = |path: &str| -> String {
            let mut command = Command::new(loader.as_ref().unwrap_or(&uci));
            if loader.is_some() {
                command.args([
                    "--library-path".into(),
                    std::env::var_os("CAKE_TEST_LIB_DIR").expect("explicit libraries required"),
                    uci.clone(),
                ]);
            }
            let result = command
                .args([
                    "-c",
                    config.to_str().unwrap(),
                    "-C",
                    overrides.to_str().unwrap(),
                    "-t",
                    delta.to_str().unwrap(),
                    "-d",
                    "|",
                    "get",
                    &format!("{package}.{path}"),
                ])
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "real UCI could not load fixture query {path}: {}; fixture: {}",
                String::from_utf8_lossy(&result.stderr),
                String::from_utf8_lossy(&fs::read(config.join(&package)).unwrap())
            );
            String::from_utf8(result.stdout)
                .unwrap()
                .trim_end_matches('\n')
                .to_string()
        };
        let foreign = "# untouched foreign block\nconfig queue 'foreign'\n option marker 'stay'\n list tags 'one'\n list tags 'two'\n";
        for owned in [
            "config queue 'owned'\n option rate '10' # retain comment\n",
            // Quote the argument before `;`: libuci's in-place unquoted
            // token terminator otherwise overwrites the separator and the
            // following command is lost (not a valid continuation fixture).
            "config \"queue\" 'owned';option r\\\nate \"1\\0\"\n",
            "config queue owned\r\n list rate 'old'\r\n option rate '10'\r\n",
            "c queue 'owned'\n o rate '10'\n",
        ] {
            let input = format!("package '{package}'\n{owned}{foreign}");
            fs::write(config.join(&package), input.as_bytes()).unwrap();
            assert_eq!(query("owned.rate"), "10");
            let rendered = render(
                input.as_bytes(),
                &[
                    set("owned", "rate", "20'0"),
                    set("owned", "extra", "new"),
                    Edit::AddSection {
                        section: "added".into(),
                        kind: "queue".into(),
                    },
                    set("added", "value", "3"),
                ],
            )
            .unwrap();
            assert!(rendered
                .windows(foreign.len())
                .any(|window| window == foreign.as_bytes()));
            fs::write(config.join(&package), rendered).unwrap();
            assert_eq!(query("owned.rate"), "20'0");
            assert_eq!(query("owned.extra"), "new");
            assert_eq!(query("added.value"), "3");
            assert_eq!(query("foreign.marker"), "stay");
            assert_eq!(query("foreign.tags"), "one|two");
            assert_eq!(fs::read_dir(&delta).unwrap().count(), 0);
        }
    }
}
