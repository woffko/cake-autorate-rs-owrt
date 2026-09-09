//! Bounded, best-effort diagnostic bundle for the Full LuCI package.
//!
//! Every byte emitted by this module passes through one redaction boundary.
//! Files are opened without following the final symlink, subprocesses have a
//! fixed executable/argument vector and timeout, and both individual sources
//! and the complete browser response are bounded.

use super::autotune_uci_materialization::redact_diagnostic_uci;
use super::process::{run_bounded_command_output, BoundedCommandOutput, SpawnSpec};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(8);
const COMMAND_OUTPUT_LIMIT: usize = 512 * 1024;
const SOURCE_FILE_LIMIT: usize = 512 * 1024;
const MAX_BUNDLE_BYTES: usize = 8 * 1024 * 1024;
const MAX_LOG_FILES_PER_INSTANCE: usize = 32;
const MAX_SECTIONS: usize = 64;
const MAX_UCI_VALUE_BYTES: usize = 4096;
const TRUNCATION_MARKER: &[u8] = b"\n[bundle outcome=truncated reason=total-output-limit]\n";

pub(crate) struct LogBundleOutput {
    bytes: Vec<u8>,
    json: bool,
}

impl LogBundleOutput {
    pub(crate) fn write_to<W: Write>(self, writer: &mut W) -> Result<(), String> {
        if self.bytes.len() > MAX_BUNDLE_BYTES {
            return Err("diagnostic bundle exceeds its output limit".to_string());
        }
        if self.json {
            let text = String::from_utf8(self.bytes)
                .map_err(|_| "diagnostic bundle is not valid UTF-8".to_string())?;
            let mut fields = serde_json::Map::new();
            fields.insert("schema_version".to_string(), 1.into());
            fields.insert("format".to_string(), "cake-autorate-log-bundle".into());
            fields.insert("byte_length".to_string(), text.len().into());
            fields.insert("text".to_string(), serde_json::Value::String(text));
            // Serialize to the caller's bounded buffer, not a second large Vec.
            // CGI can return HTTP 200 even if the child dies: an incomplete
            // JSON document must never be accepted as a completed download.
            serde_json::to_writer(&mut *writer, &fields)
                .map_err(|error| format!("unable to write diagnostic JSON: {error}"))?;
            writer
                .write_all(b"\n")
                .map_err(|error| format!("unable to write diagnostic JSON: {error}"))?;
        } else {
            writer
                .write_all(&self.bytes)
                .map_err(|error| format!("unable to write diagnostic bundle: {error}"))?;
        }
        writer
            .flush()
            .map_err(|error| format!("unable to flush diagnostic bundle: {error}"))
    }
}

fn utf8_prefix_len(bytes: &[u8], limit: usize) -> usize {
    let mut retained = limit.min(bytes.len());
    while retained > 0 && retained < bytes.len() && bytes[retained] & 0xc0 == 0x80 {
        retained -= 1;
    }
    retained
}

#[derive(Clone, Debug)]
struct Environment {
    uci: PathBuf,
    date: PathBuf,
    uname: PathBuf,
    apk: PathBuf,
    daemon: PathBuf,
    cake_init: PathBuf,
    nft: PathBuf,
    mwan3: PathBuf,
    ubus: PathBuf,
    ip: PathBuf,
    logread: PathBuf,
    gzip: PathBuf,
    cake_config: PathBuf,
    sqm_config: PathBuf,
    openwrt_release: PathBuf,
    run_root: PathBuf,
    default_log_root: PathBuf,
}

impl Environment {
    fn live() -> Self {
        Self {
            uci: env_path("CAKE_AUTORATE_UCI_BIN", "/sbin/uci"),
            date: env_path("CAKE_AUTORATE_DATE_BIN", "/bin/date"),
            uname: env_path("CAKE_AUTORATE_UNAME_BIN", "/bin/uname"),
            apk: env_path("CAKE_AUTORATE_APK_BIN", "/usr/bin/apk"),
            daemon: env_path("CAKE_AUTORATE_DAEMON", "/usr/sbin/cake-autorated"),
            cake_init: env_path("CAKE_AUTORATE_INIT", "/etc/init.d/cake-autorate"),
            nft: env_path("CAKE_AUTORATE_NFT_BIN", "/usr/sbin/nft"),
            mwan3: env_path("CAKE_AUTORATE_MWAN3_BIN", "/usr/sbin/mwan3"),
            ubus: env_path("CAKE_AUTORATE_UBUS_BIN", "/bin/ubus"),
            ip: env_path("CAKE_AUTORATE_IP_BIN", "/sbin/ip"),
            logread: env_path("CAKE_AUTORATE_LOGREAD_BIN", "/sbin/logread"),
            gzip: env_path("CAKE_AUTORATE_GZIP_BIN", "/bin/gzip"),
            cake_config: env_path("CAKE_AUTORATE_CONFIG_PATH", "/etc/config/cake-autorate"),
            sqm_config: env_path("CAKE_AUTORATE_SQM_CONFIG_PATH", "/etc/config/sqm"),
            openwrt_release: env_path("CAKE_AUTORATE_OPENWRT_RELEASE_PATH", "/etc/openwrt_release"),
            run_root: env_path("CAKE_AUTORATE_RUN_ROOT", "/var/run/cake-autorate"),
            default_log_root: env_path("CAKE_AUTORATE_LOG_ROOT", "/var/log"),
        }
    }
}

fn env_path(name: &str, fallback: &str) -> PathBuf {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(fallback))
}

fn executable(path: &Path) -> bool {
    // OpenWrt utilities (including gzip) are commonly BusyBox symlinks.
    // SpawnSpec also validates the resolved executable before launching it.
    fs::metadata(path).is_ok_and(|metadata| {
        metadata.file_type().is_file()
            && metadata.permissions().mode() & 0o111 != 0
            && metadata.permissions().mode() & 0o022 == 0
    })
}

fn validate_section(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || matches!(value, "." | "..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-'))
    {
        return Err("invalid diagnostic section".to_string());
    }
    Ok(())
}

fn validate_member(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'@' | b'-')
        })
}

fn command_output(program: &Path, arguments: &[String]) -> Result<BoundedCommandOutput, String> {
    run_bounded_command_output(
        &SpawnSpec {
            program: program.to_path_buf(),
            arguments: arguments.iter().map(OsString::from).collect(),
            environment: Vec::new(),
        },
        COMMAND_TIMEOUT,
        COMMAND_OUTPUT_LIMIT,
        || false,
    )
}

struct Bundle {
    bytes: Vec<u8>,
    sealed: bool,
}

impl Bundle {
    fn new() -> Self {
        Self {
            bytes: Vec::with_capacity(64 * 1024),
            sealed: false,
        }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }

    fn append_raw(&mut self, value: &[u8]) {
        if self.sealed {
            return;
        }
        let remaining = MAX_BUNDLE_BYTES.saturating_sub(self.bytes.len());
        if value.len() <= remaining {
            self.bytes.extend_from_slice(value);
            return;
        }
        // Make room for the whole marker, even if a preceding source filled
        // almost all available space. Both stored and incoming text stay UTF-8.
        let content_limit = MAX_BUNDLE_BYTES.saturating_sub(TRUNCATION_MARKER.len());
        self.bytes
            .truncate(utf8_prefix_len(&self.bytes, content_limit));
        let retained = utf8_prefix_len(value, content_limit.saturating_sub(self.bytes.len()));
        self.bytes.extend_from_slice(&value[..retained]);
        self.bytes.extend_from_slice(TRUNCATION_MARKER);
        self.sealed = true;
    }

    fn append_text(&mut self, value: &str) {
        self.append_raw(value.as_bytes());
    }

    fn append_formatted(&mut self, value: &[u8], format: DiagnosticFormat) -> bool {
        match std::str::from_utf8(value) {
            Ok(value) => match sanitize_diagnostic(value, format) {
                Ok(clean) => {
                    self.append_text(&clean);
                    true
                }
                Err(_) => {
                    self.outcome("omitted", "malformed-or-unsupported-structured-source");
                    false
                }
            },
            Err(_) => {
                self.append_text("[source outcome=omitted reason=non-utf8]\n");
                false
            }
        }
    }

    fn title(&mut self, value: &str) {
        self.append_text(&format!("\n===== {value} =====\n"));
    }

    fn outcome(&mut self, state: &str, detail: &str) {
        self.append_text(&format!(
            "[source outcome={state} detail={}]\n",
            safe_marker(detail)
        ));
    }

    fn command(&mut self, title: &str, program: &Path, arguments: &[&str]) {
        self.title(title);
        if !executable(program) {
            self.outcome("skipped", "executable-unavailable");
            return;
        }
        let arguments = arguments
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>();
        match command_output(program, &arguments) {
            Ok(output) => {
                if output.status.success() {
                    if self.append_formatted(&output.stdout, DiagnosticFormat::Text) {
                        self.outcome("ok", "exit-0");
                    }
                } else {
                    // Parser/tool errors can quote raw secret-bearing input.
                    // Report the exit status, never echo that unparsed source.
                    self.outcome("failed", &format!("exit-{}", output.status));
                }
            }
            Err(error) => self.outcome("failed", &error),
        }
    }

    fn file(&mut self, title: &str, path: &Path, gzip: &Path) {
        let format = if path.extension() == Some(OsStr::new("json")) {
            DiagnosticFormat::Json
        } else {
            DiagnosticFormat::Text
        };
        self.file_formatted(title, path, gzip, format);
    }

    fn file_formatted(&mut self, title: &str, path: &Path, gzip: &Path, format: DiagnosticFormat) {
        self.title(title);
        match read_regular_file(path, SOURCE_FILE_LIMIT) {
            Ok(Some(contents)) => {
                if contents.truncated {
                    self.outcome("omitted", "source-file-limit");
                    return;
                }
                if contents.bytes.starts_with(&[0x1f, 0x8b]) {
                    if !executable(gzip) {
                        self.outcome("skipped", "gzip-unavailable");
                        return;
                    }
                    let argument = path.to_string_lossy().into_owned();
                    match command_output(gzip, &["-dc".to_string(), argument]) {
                        Ok(output) if output.status.success() => {
                            if self.append_formatted(&output.stdout, format) {
                                self.outcome("ok", "gzip");
                            }
                        }
                        Ok(output) => {
                            self.outcome("failed", &format!("gzip-{}", output.status));
                        }
                        Err(error) => self.outcome("failed", &error),
                    }
                } else if path.extension() == Some(OsStr::new("gz")) {
                    self.outcome("failed", "gzip-magic-mismatch");
                } else {
                    if !self.append_formatted(&contents.bytes, format) {
                        return;
                    }
                    self.outcome(
                        if contents.truncated {
                            "truncated"
                        } else {
                            "ok"
                        },
                        if contents.truncated {
                            "source-file-limit"
                        } else {
                            "plain-text"
                        },
                    );
                }
            }
            Ok(None) => self.outcome("skipped", "file-unavailable-or-unsafe"),
            Err(error) => self.outcome("failed", &error),
        }
    }
}

fn safe_marker(value: &str) -> String {
    let mut output = String::with_capacity(value.len().min(160));
    for character in value.chars().take(160) {
        output.push(
            if character.is_ascii_alphanumeric() || "._:/-".contains(character) {
                character
            } else {
                '_'
            },
        );
    }
    output
}

#[derive(Clone, Copy)]
enum DiagnosticFormat {
    Text,
    Uci,
    Json,
}

fn sensitive_field(name: &str) -> bool {
    let name = name.to_ascii_lowercase().replace(['_', '-'], "");
    [
        "password",
        "passwd",
        "username",
        "privatekey",
        "apikey",
        "authorization",
        "secret",
        "token",
        "cookie",
        "psk",
    ]
    .iter()
    .any(|key| name.ends_with(key))
}

fn sanitize_diagnostic(value: &str, format: DiagnosticFormat) -> Result<String, ()> {
    if matches!(format, DiagnosticFormat::Uci) {
        return redact_diagnostic_uci(value, sensitive_field).map_err(|_| ());
    }
    if matches!(format, DiagnosticFormat::Json) || value.trim_start().starts_with(['{', '[']) {
        // Default serde_json recursion limits remain enabled. Input/output is
        // already bounded at the diagnostic source and bundle boundaries.
        let mut parsed: serde_json::Value = serde_json::from_str(value).map_err(|_| ())?;
        fn clean(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::Object(fields) => {
                    for (key, value) in fields {
                        if sensitive_field(key) {
                            *value = "<redacted>".into();
                        } else {
                            clean(value);
                        }
                    }
                }
                serde_json::Value::Array(items) => items.iter_mut().for_each(clean),
                serde_json::Value::String(text) => *text = redact_text(text),
                _ => {}
            }
        }
        clean(&mut parsed);
        return serde_json::to_string(&parsed)
            .map(|text| text + "\n")
            .map_err(|_| ());
    }
    Ok(redact_text(value))
}

fn redact_text(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut quote = None;
    let mut pending_value = false;
    let mut escaped = false;
    for line in value.split_inclusive('\n') {
        if quote.is_some() || pending_value || escaped {
            let continuation = line.trim_end_matches(['\r', '\n']).trim_start();
            if !continuation.is_empty() {
                pending_value = false;
                scan_secret_quotes(continuation, &mut quote, &mut escaped);
            }
            if line.ends_with('\n') {
                output.push('\n');
            }
            continue;
        }
        let has_newline = line.ends_with('\n');
        let body = line
            .strip_suffix('\n')
            .unwrap_or(line)
            .strip_suffix('\r')
            .unwrap_or_else(|| line.strip_suffix('\n').unwrap_or(line));
        let lower = body.to_ascii_lowercase();
        let sensitive_key = [
            "mqtt_password",
            "mqtt_username",
            "password",
            "passwd",
            "private_key",
            "api_key",
            "apikey",
            "authorization",
            "secret",
            "token",
            "cookie",
            "psk",
        ]
        .iter()
        .filter_map(|needle| find_sensitive_key(&lower, needle))
        .min_by_key(|(offset, _)| *offset);
        let password_flag = body.find(" -P ").map(|offset| (offset + 1, 2));
        let sensitive = match (sensitive_key, password_flag) {
            (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
            (Some(value), None) | (None, Some(value)) => Some(value),
            (None, None) => None,
        };
        if let Some((offset, length)) = sensitive {
            output.push_str(&body[..offset + length]);
            output.push_str("=<redacted>");
            let tail = body[offset + length..].trim_start();
            pending_value = tail.trim_matches([' ', '\t', '=', ':']).is_empty();
            scan_secret_quotes(tail, &mut quote, &mut escaped);
        } else {
            output.push_str(body);
        }
        if has_newline {
            output.push('\n');
        }
    }
    output
}

fn scan_secret_quotes(value: &str, quote: &mut Option<char>, escaped: &mut bool) {
    for ch in value.chars() {
        if *escaped {
            *escaped = false;
            continue;
        }
        if ch == '\\' && *quote != Some('\'') {
            *escaped = true;
            continue;
        }
        if let Some(open) = *quote {
            if ch == open {
                *quote = None;
            }
        } else if matches!(ch, '\'' | '"') {
            *quote = Some(ch);
        }
    }
}

fn find_sensitive_key(line: &str, needle: &str) -> Option<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut cursor = 0usize;
    while let Some(relative) = line[cursor..].find(needle) {
        let offset = cursor + relative;
        let before_is_boundary =
            offset == 0 || !bytes[offset - 1].is_ascii_alphanumeric() && bytes[offset - 1] != b'_';
        let after = offset + needle.len();
        let after_is_separator = bytes.get(after).is_some_and(|byte| {
            byte.is_ascii_whitespace() || matches!(byte, b'=' | b':' | b'\'' | b'"')
        });
        if before_is_boundary && after_is_separator {
            return Some((offset, needle.len()));
        }
        cursor = after;
        if cursor >= line.len() {
            break;
        }
    }
    None
}

struct BoundedFile {
    bytes: Vec<u8>,
    truncated: bool,
}

fn read_regular_file(path: &Path, limit: usize) -> Result<Option<BoundedFile>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("unable-to-inspect-file-{error}")),
    };
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Ok(None);
    }
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) => return Err(format!("unable-to-open-file-{error}")),
    };
    if !file
        .metadata()
        .map_err(|error| format!("unable-to-attest-file-{error}"))?
        .is_file()
    {
        return Ok(None);
    }
    let take_limit = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    let mut bytes = Vec::with_capacity(limit.min(16 * 1024));
    Read::by_ref(&mut file)
        .take(take_limit)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("unable-to-read-file-{error}"))?;
    let truncated = bytes.len() > limit;
    bytes.truncate(limit);
    Ok(Some(BoundedFile { bytes, truncated }))
}

fn uci_output(
    environment: &Environment,
    arguments: &[&str],
) -> Result<BoundedCommandOutput, String> {
    if !executable(&environment.uci) {
        return Err("uci-unavailable".to_string());
    }
    command_output(
        &environment.uci,
        &arguments
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>(),
    )
}

fn uci_get(
    environment: &Environment,
    section: &str,
    option: Option<&str>,
) -> Result<Option<String>, String> {
    let key = option.map_or_else(
        || format!("cake-autorate.{section}"),
        |option| format!("cake-autorate.{section}.{option}"),
    );
    let output = uci_output(environment, &["-q", "get", &key])?;
    if !output.status.success() {
        return Ok(None);
    }
    let value = std::str::from_utf8(&output.stdout)
        .map_err(|_| "uci-value-non-utf8".to_string())?
        .trim_end_matches(['\r', '\n']);
    if value.len() > MAX_UCI_VALUE_BYTES || value.chars().any(char::is_control) {
        return Err("uci-value-unsafe-or-oversized".to_string());
    }
    Ok(Some(value.to_string()))
}

fn configured_sections(environment: &Environment, requested: &str) -> Result<Vec<String>, String> {
    if requested != "all" {
        validate_section(requested)?;
        return match uci_get(environment, requested, None)? {
            Some(section_type) if section_type == "cake_autorate" => {
                Ok(vec![requested.to_string()])
            }
            _ => Err(format!("unknown cake-autorate section: {requested}")),
        };
    }
    // -X disables extended @type[index] aliases so anonymous sections retain
    // the stable cfgXXXXXX identifiers used by runtime/log paths.
    let output = uci_output(environment, &["-q", "-X", "show", "cake-autorate"])?;
    if !output.status.success() {
        return Ok(Vec::new());
    }
    let text = std::str::from_utf8(&output.stdout)
        .map_err(|_| "cake-autorate-section-list-non-utf8".to_string())?;
    let mut sections = Vec::new();
    for line in text.lines() {
        let Some(section) = line
            .strip_prefix("cake-autorate.")
            .and_then(|line| line.strip_suffix("=cake_autorate"))
        else {
            continue;
        };
        validate_section(section)?;
        if !sections.iter().any(|existing| existing == section) {
            if sections.len() >= MAX_SECTIONS {
                return Err("too-many-cake-autorate-sections".to_string());
            }
            sections.push(section.to_string());
        }
    }
    Ok(sections)
}

fn validated_log_directory(path: &Path, allow_platform_symlinks: bool) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("log-directory-not-absolute".to_string());
    }
    if allow_platform_symlinks {
        let canonical =
            fs::canonicalize(path).map_err(|error| format!("log-directory-unavailable-{error}"))?;
        if !canonical.is_absolute()
            || !fs::metadata(&canonical)
                .map_err(|error| format!("log-directory-unavailable-{error}"))?
                .is_dir()
        {
            return Err("log-directory-unsafe".to_string());
        }
        return Ok(canonical);
    }
    let mut current = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir => continue,
            Component::Normal(value) => current.push(value),
            _ => return Err("log-directory-has-unsafe-component".to_string()),
        }
        let metadata = fs::symlink_metadata(&current)
            .map_err(|error| format!("log-directory-component-unavailable-{error}"))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("log-directory-component-unsafe".to_string());
        }
    }
    Ok(path.to_path_buf())
}

fn log_paths(
    directory: &Path,
    section: &str,
    allow_platform_symlinks: bool,
) -> Result<Vec<PathBuf>, String> {
    let directory = validated_log_directory(directory, allow_platform_symlinks)?;
    let basename = format!("cake-autorate.{section}.log");
    let mut paths = Vec::new();
    for entry in
        fs::read_dir(&directory).map_err(|error| format!("unable-to-list-log-directory-{error}"))?
    {
        let entry = entry.map_err(|error| format!("unable-to-read-log-entry-{error}"))?;
        let name = entry.file_name();
        let bytes = name.as_bytes();
        if bytes == basename.as_bytes()
            || (bytes.starts_with(basename.as_bytes()) && bytes.get(basename.len()) == Some(&b'.'))
        {
            if paths.len() >= MAX_LOG_FILES_PER_INSTANCE {
                return Err("too-many-log-rotations".to_string());
            }
            paths.push(entry.path());
        }
    }
    paths.sort_by(|left, right| {
        left.as_os_str()
            .as_bytes()
            .cmp(right.as_os_str().as_bytes())
    });
    Ok(paths)
}

fn append_instance(bundle: &mut Bundle, environment: &Environment, section: &str) {
    let status = environment.run_root.join(section).join("status.json");
    bundle.file(
        &format!("instance {section} status"),
        &status,
        &environment.gzip,
    );

    let (log_directory, allow_platform_symlinks) =
        match uci_get(environment, section, Some("log_file_path_override")) {
            Ok(Some(value)) if !value.is_empty() => (PathBuf::from(value), false),
            Ok(_) => (environment.default_log_root.clone(), true),
            Err(error) => {
                bundle.title(&format!("instance {section} logs"));
                bundle.outcome("failed", &error);
                return;
            }
        };
    match log_paths(&log_directory, section, allow_platform_symlinks) {
        Ok(paths) if paths.is_empty() => {
            bundle.title(&format!("instance {section} logs"));
            bundle.outcome("skipped", "no-log-files");
        }
        Ok(paths) => {
            for path in paths {
                bundle.file(&path.to_string_lossy(), &path, &environment.gzip);
            }
        }
        Err(error) => {
            bundle.title(&format!("instance {section} logs"));
            bundle.outcome("skipped", &error);
        }
    }

    let member = match uci_get(environment, section, Some("mwan3_member")) {
        Ok(Some(member)) if validate_member(&member) => member,
        Ok(Some(_)) => {
            bundle.title(&format!("instance {section} route topology"));
            bundle.outcome("skipped", "invalid-mwan3-member");
            return;
        }
        _ => return,
    };
    let body = format!("{{\"interface\":\"{member}\"}}");
    bundle.command(
        &format!("instance {section} mwan3 member"),
        &environment.ubus,
        &["call", "mwan3", "status", &body],
    );
    let object = format!("network.interface.{member}");
    bundle.command(
        &format!("instance {section} network interface"),
        &environment.ubus,
        &["call", &object, "status"],
    );
}

pub(crate) fn run_log_bundle<I>(mut arguments: I) -> Result<LogBundleOutput, String>
where
    I: Iterator<Item = String>,
{
    let first = arguments.next().unwrap_or_else(|| "all".to_string());
    let json = first == "--json";
    let requested = if json {
        arguments.next().unwrap_or_else(|| "all".to_string())
    } else {
        first
    };
    if arguments.next().is_some() {
        return Err("log-bundle accepts [--json] and one optional section".to_string());
    }
    if matches!(requested.as_str(), "--help" | "-h") {
        return Ok(LogBundleOutput {
            bytes: b"usage: cake-autorated --log-bundle [--json] [all|section]\n".to_vec(),
            json: false,
        });
    }
    Ok(LogBundleOutput {
        bytes: log_bundle(&requested, &Environment::live())?,
        json,
    })
}

fn log_bundle(requested: &str, environment: &Environment) -> Result<Vec<u8>, String> {
    let sections = configured_sections(environment, requested)?;
    let mut bundle = Bundle::new();
    bundle.title("cake-autorate-rs diagnostic bundle");
    let generated = if executable(&environment.date) {
        command_output(
            &environment.date,
            &["-u".to_string(), "+%Y-%m-%dT%H:%M:%SZ".to_string()],
        )
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
    } else {
        None
    }
    .unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .to_string()
    });
    bundle.append_text(&format!("generated_at_utc={generated}\n"));
    bundle.append_text(&format!("requested_section={requested}\n"));
    bundle.append_text(&format!(
        "included_sections={}\n",
        if sections.is_empty() {
            "none".to_string()
        } else {
            sections.join(" ")
        }
    ));

    bundle.command("system", &environment.uname, &["-a"]);
    bundle.file(
        &environment.openwrt_release.to_string_lossy(),
        &environment.openwrt_release,
        &environment.gzip,
    );
    bundle.command(
        "apk cake-autorate packages",
        &environment.apk,
        &["info", "cake-autorate-rs", "luci-app-cake-autorate-rs"],
    );
    bundle.command(
        "cake-autorate service status",
        &environment.cake_init,
        &["status"],
    );
    bundle.command(
        "per-instance runtime reconciliation",
        &environment.daemon,
        &["--runtime-health"],
    );
    bundle.command(
        "native traffic-priority classifier",
        &environment.daemon,
        &["--traffic-classifier", "status"],
    );
    bundle.command(
        "native traffic-priority nftables table",
        &environment.nft,
        &["list", "table", "inet", "cake_autorate_dscp"],
    );
    bundle.file_formatted(
        &environment.cake_config.to_string_lossy(),
        &environment.cake_config,
        &environment.gzip,
        DiagnosticFormat::Uci,
    );
    bundle.file_formatted(
        &environment.sqm_config.to_string_lossy(),
        &environment.sqm_config,
        &environment.gzip,
        DiagnosticFormat::Uci,
    );
    if executable(&environment.mwan3) {
        bundle.command("mwan3 version", &environment.mwan3, &["--version"]);
        bundle.command(
            "mwan3 status",
            &environment.ubus,
            &["call", "mwan3", "status"],
        );
        bundle.command(
            "IPv4 policy rules",
            &environment.ip,
            &["-4", "rule", "show"],
        );
        bundle.command(
            "IPv4 routing tables",
            &environment.ip,
            &["-4", "route", "show", "table", "all"],
        );
        bundle.file_formatted(
            "mwan3 config",
            &environment.cake_config.with_file_name("mwan3"),
            &environment.gzip,
            DiagnosticFormat::Uci,
        );
    }
    for section in &sections {
        append_instance(&mut bundle, environment, section);
    }
    bundle.command(
        "logread cake-autorate tail",
        &environment.logread,
        &["-e", "cake-autorate", "-l", "200"],
    );
    Ok(bundle.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

    struct Fixture {
        root: PathBuf,
        environment: Environment,
    }

    impl Fixture {
        fn new() -> Self {
            let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root = env::temp_dir().join(format!("cake-log-bundle-{}-{id}", std::process::id()));
            fs::create_dir(&root).unwrap();
            let bin = root.join("bin");
            let run = root.join("run");
            let logs = root.join("logs");
            fs::create_dir(&bin).unwrap();
            fs::create_dir(&run).unwrap();
            fs::create_dir(&logs).unwrap();
            let missing = bin.join("missing");
            let environment = Environment {
                uci: bin.join("uci"),
                date: bin.join("date"),
                uname: missing.clone(),
                apk: missing.clone(),
                daemon: missing.clone(),
                cake_init: missing.clone(),
                nft: missing.clone(),
                mwan3: missing.clone(),
                ubus: missing.clone(),
                ip: missing.clone(),
                logread: missing.clone(),
                gzip: missing,
                cake_config: root.join("cake-config"),
                sqm_config: root.join("sqm-config"),
                openwrt_release: root.join("openwrt-release"),
                run_root: run,
                default_log_root: logs,
            };
            let fixture = Self { root, environment };
            fixture.executable(
                &fixture.environment.date,
                "#!/bin/sh\nprintf '2026-08-16T00:00:00Z\\n'\n",
            );
            fixture
        }

        fn executable(&self, path: &Path, body: &str) {
            fs::write(path, body).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }

        fn uci(&self, body: &str) {
            self.executable(&self.environment.uci, body);
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn multiline_secret_is_absent_from_downloaded_config_bytes() {
        let fixture = Fixture::new();
        fixture.uci("#!/bin/sh\ncase \"$*\" in\n'-q get cake-autorate.wan') printf 'cake_autorate\\n' ;;\n*) exit 1 ;;\nesac\n");
        fs::write(&fixture.environment.cake_config,
            "config cake_autorate 'wan'\n option mqtt_password 'first_marker\nsecond_marker'\n option enabled '1'\n").unwrap();
        let output = String::from_utf8(log_bundle("wan", &fixture.environment).unwrap()).unwrap();
        for marker in ["first_marker", "second_marker"] {
            assert!(!output.contains(marker), "secret component was exported");
        }
    }

    #[test]
    fn structured_redaction_handles_nested_unicode_keys_and_rejects_bad_input() {
        let input = r#"{"pass\u0077ord":"first_marker\nsecond_marker","rows":[{"api-key":"third_marker"}],"ok":true}"#;
        let result = sanitize_diagnostic(input, DiagnosticFormat::Json).unwrap();
        for value in ["first_marker", "second_marker", "third_marker"] {
            assert!(!result.contains(value));
        }
        assert!(result.contains("\"ok\":true"));
        for input in ["{\"password\":\"unterminated", "{bad-json}", "[1,", "NaN"] {
            assert!(sanitize_diagnostic(input, DiagnosticFormat::Json).is_err());
        }
        for input in [
            "config cake_autorate 'wan'\n option password 'first\nsecond",
            "option password 'first\nsecond'\n",
            "config cake_autorate 'wan'\n option password\n 'first\nsecond'\n",
        ] {
            assert!(sanitize_diagnostic(input, DiagnosticFormat::Uci).is_err());
        }
        let crlf = "config cake_autorate 'wan'\r\n option password 'first\r\nsecond'\r\n option enabled '1'\r\n";
        let result = sanitize_diagnostic(crlf, DiagnosticFormat::Uci).unwrap();
        assert!(!result.contains("first") && !result.contains("second"));
        assert!(result.contains("enabled '1'"));
    }

    #[test]
    fn text_redaction_covers_quoted_and_next_line_secret_values() {
        for input in [
            "password='first_marker\nsecond_marker'\nINFO healthy\n",
            "password=\n'first_marker\nsecond_marker'\nINFO healthy\n",
            "password='first_marker'\\''more\nsecond_marker'\nINFO healthy\n",
            "password=first_marker\\\nsecond_marker\nINFO healthy\n",
            "password=\"first_marker\\\n\"more\nsecond_marker\"\nINFO healthy\n",
        ] {
            let result = redact_text(input);
            assert!(!result.contains("first_marker") && !result.contains("second_marker"));
            assert!(result.contains("INFO healthy"));
        }
    }

    #[test]
    fn gzip_and_parser_failures_do_not_bypass_redaction() {
        let fixture = Fixture::new();
        let path = fixture.root.join("config.gz");
        let input = b"config cake_autorate 'wan'\n option password 'first_marker\nsecond_marker'\n";
        let gzip = Path::new("/bin/gzip");
        let packed = super::super::process::run_bounded_command_output_with_input(
            &SpawnSpec {
                program: gzip.to_path_buf(),
                arguments: vec!["-c".into()],
                environment: vec![],
            },
            Some(input),
            Duration::from_secs(2),
            SOURCE_FILE_LIMIT,
            || false,
            |_| {},
        )
        .unwrap();
        assert!(packed.status.success());
        fs::write(&path, packed.stdout).unwrap();
        let alias = fixture.root.join("gzip-alias");
        std::os::unix::fs::symlink(gzip, &alias).unwrap();
        let mut bundle = Bundle::new();
        bundle.file_formatted("gzip config", &path, &alias, DiagnosticFormat::Uci);
        let text = String::from_utf8(bundle.finish()).unwrap();
        assert!(text.contains("<redacted>") && text.contains("detail=gzip"));
        assert!(!text.contains("first_marker") && !text.contains("second_marker"));
        let helper = fixture.root.join("failing-parser");
        fixture.executable(
            &helper,
            "#!/bin/sh\nprintf 'unlabeled_sensitive_marker' >&2\nexit 1\n",
        );
        let mut bundle = Bundle::new();
        bundle.command("failing parser", &helper, &[]);
        assert!(!String::from_utf8(bundle.finish())
            .unwrap()
            .contains("unlabeled_sensitive_marker"));
        fs::write(&path, b"config broken 'unterminated").unwrap();
        let mut bundle = Bundle::new();
        bundle.file_formatted("malformed config", &path, &alias, DiagnosticFormat::Uci);
        let text = String::from_utf8(bundle.finish()).unwrap();
        assert!(!text.contains("source outcome=ok"));
    }

    #[test]
    fn every_emitted_surface_uses_the_fail_closed_redactor() {
        let input = concat!(
            "cake-autorate.wan.mqtt_password='ab'\\''tail'\n",
            "option password 'clear'\n",
            "mqtt_username=bob\n",
            "mosquitto_pub -P broker-secret -t topic\n",
            "ordinary latency=12\n"
        );
        let redacted = redact_text(input);
        for secret in ["tail", "clear", "bob", "broker-secret"] {
            assert!(!redacted.contains(secret));
        }
        assert!(redacted.contains("ordinary latency=12"));
        assert_eq!(redacted.matches("<redacted>").count(), 4);
    }

    #[test]
    fn all_enumerates_only_typed_sections_and_specific_requests_recheck_type() {
        let fixture = Fixture::new();
        fixture.uci(
            "#!/bin/sh\ncase \"$*\" in\n'-q -X show cake-autorate') printf 'cake-autorate.wan=cake_autorate\\ncake-autorate.wan.enabled=1\\ncake-autorate.cfg01=cake_autorate\\n' ;;\n'-q get cake-autorate.wan') printf 'cake_autorate\\n' ;;\n*) exit 1 ;;\nesac\n",
        );
        assert_eq!(
            configured_sections(&fixture.environment, "all").unwrap(),
            ["wan", "cfg01"]
        );
        assert_eq!(
            configured_sections(&fixture.environment, "wan").unwrap(),
            ["wan"]
        );
        assert!(configured_sections(&fixture.environment, "..").is_err());
        assert!(configured_sections(&fixture.environment, "missing").is_err());
    }

    #[test]
    fn unsafe_log_paths_and_symlinked_files_are_not_read() {
        let fixture = Fixture::new();
        let outside = fixture.root.join("outside");
        fs::create_dir(&outside).unwrap();
        let link = fixture.root.join("linked");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(validated_log_directory(&link, false).is_err());
        assert_eq!(
            validated_log_directory(&link, true).unwrap(),
            fs::canonicalize(&outside).unwrap()
        );
        assert!(validated_log_directory(Path::new("../logs"), false).is_err());

        let secret = outside.join("secret");
        fs::write(&secret, "must-not-leak").unwrap();
        let log = fixture
            .environment
            .default_log_root
            .join("cake-autorate.wan.log");
        std::os::unix::fs::symlink(&secret, &log).unwrap();
        assert!(read_regular_file(&log, SOURCE_FILE_LIMIT)
            .unwrap()
            .is_none());
    }

    #[test]
    fn file_and_total_output_limits_are_explicit() {
        let fixture = Fixture::new();
        let path = fixture.root.join("large.log");
        fs::write(&path, vec![b'x'; SOURCE_FILE_LIMIT + 17]).unwrap();
        let value = read_regular_file(&path, SOURCE_FILE_LIMIT)
            .unwrap()
            .unwrap();
        assert_eq!(value.bytes.len(), SOURCE_FILE_LIMIT);
        assert!(value.truncated);

        let mut bundle = Bundle::new();
        bundle.append_raw(&vec![b'y'; MAX_BUNDLE_BYTES + 1]);
        assert!(bundle.sealed);
        assert_eq!(bundle.bytes.len(), MAX_BUNDLE_BYTES);
        assert!(bundle.bytes.ends_with(TRUNCATION_MARKER));
    }

    #[test]
    fn truncation_keeps_utf8_and_a_complete_marker_even_after_nearly_full_source() {
        let mut bundle = Bundle::new();
        bundle.append_text(&"✓".repeat(MAX_BUNDLE_BYTES / 3 + 10));
        assert!(bundle.sealed);
        assert!(bundle.bytes.len() <= MAX_BUNDLE_BYTES);
        assert!(std::str::from_utf8(&bundle.bytes).is_ok());
        assert!(bundle.bytes.ends_with(TRUNCATION_MARKER));

        let mut bundle = Bundle::new();
        bundle.append_text(&"x".repeat(MAX_BUNDLE_BYTES - 2));
        bundle.append_text("🌍more text");
        assert_eq!(bundle.bytes.len(), MAX_BUNDLE_BYTES);
        assert!(std::str::from_utf8(&bundle.bytes).is_ok());
        assert!(bundle.bytes.ends_with(TRUNCATION_MARKER));
    }

    #[test]
    fn json_envelope_preserves_sanitized_text_bytes_and_rejects_partial_output() {
        let text = redact_text("password='synthetic_json_marker'\nlatency=✓\n");
        let mut output = Vec::new();
        LogBundleOutput {
            bytes: text.as_bytes().to_vec(),
            json: true,
        }
        .write_to(&mut output)
        .unwrap();
        let envelope: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(envelope["schema_version"], 1);
        assert_eq!(envelope["format"], "cake-autorate-log-bundle");
        assert_eq!(envelope["byte_length"].as_u64(), Some(text.len() as u64));
        assert_eq!(envelope["text"].as_str(), Some(text.as_str()));
        assert!(!String::from_utf8_lossy(&output).contains("synthetic_json_marker"));
        assert!(serde_json::from_slice::<serde_json::Value>(&output[..output.len() - 2]).is_err());

        let mut plain = Vec::new();
        LogBundleOutput {
            bytes: text.as_bytes().to_vec(),
            json: false,
        }
        .write_to(&mut plain)
        .unwrap();
        assert_eq!(plain, text.as_bytes());
    }

    #[test]
    fn json_wire_growth_does_not_truncate_the_envelope_or_expand_decoded_limit() {
        let bytes = vec![b'\n'; 300 * 1024];
        let mut output = Vec::new();
        LogBundleOutput { bytes, json: true }
            .write_to(&mut output)
            .unwrap();
        assert!(output.len() > 600 * 1024);
        let envelope: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(envelope["text"].as_str().unwrap().len(), 300 * 1024);
        let mut rejected = Vec::new();
        assert!(LogBundleOutput {
            bytes: vec![b'x'; MAX_BUNDLE_BYTES + 1],
            json: true,
        }
        .write_to(&mut rejected)
        .is_err());
        assert!(rejected.is_empty());
    }

    #[test]
    fn interrupted_json_writer_never_completes_a_success_envelope() {
        struct Interrupted {
            bytes: Vec<u8>,
        }
        impl Write for Interrupted {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                let count = (128usize.saturating_sub(self.bytes.len())).min(bytes.len());
                if count == 0 {
                    return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
                }
                self.bytes.extend_from_slice(&bytes[..count]);
                Ok(count)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut output = Interrupted { bytes: Vec::new() };
        assert!(LogBundleOutput {
            bytes: vec![b'x'; 300 * 1024],
            json: true,
        }
        .write_to(&mut output)
        .is_err());
        assert_eq!(output.bytes.len(), 128);
        assert!(serde_json::from_slice::<serde_json::Value>(&output.bytes).is_err());
    }

    #[test]
    fn log_bundle_cli_rejects_extra_arguments_before_collecting_and_keeps_help_plain() {
        assert!(
            run_log_bundle(["--json", "all", "extra"].into_iter().map(str::to_string)).is_err()
        );
        let result = run_log_bundle(["--help"].into_iter().map(str::to_string)).unwrap();
        assert!(!result.json);
        assert!(String::from_utf8(result.bytes)
            .unwrap()
            .contains("[--json]"));
    }

    #[test]
    fn degraded_bundle_is_still_downloadable_and_deterministic() {
        let fixture = Fixture::new();
        fixture.uci(
            "#!/bin/sh\ncase \"$*\" in\n'-q -X show cake-autorate') printf 'cake-autorate.wan=cake_autorate\\n' ;;\n*log_file_path_override) exit 1 ;;\n*mwan3_member) exit 1 ;;\n*) exit 1 ;;\nesac\n",
        );
        fs::create_dir(fixture.environment.run_root.join("wan")).unwrap();
        fs::write(
            fixture.environment.run_root.join("wan/status.json"),
            "{\"password\":\"runtime-secret\",\"ok\":true}\n",
        )
        .unwrap();
        fs::write(
            fixture
                .environment
                .default_log_root
                .join("cake-autorate.wan.log"),
            "token=log-secret\nINFO healthy\n",
        )
        .unwrap();
        let first = String::from_utf8(log_bundle("all", &fixture.environment).unwrap()).unwrap();
        let second = String::from_utf8(log_bundle("all", &fixture.environment).unwrap()).unwrap();
        assert_eq!(first, second);
        assert!(!first.contains("runtime-secret"));
        assert!(!first.contains("log-secret"));
        assert!(first.contains("INFO healthy"));
        assert!(first.contains("source outcome=skipped"));
    }
}
