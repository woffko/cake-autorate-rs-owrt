//! Bounded read-only projections used by the Full LuCI package.
//!
//! These commands replace small shell helpers. They never mutate UCI, install
//! packages, or drive lifecycle state; mutating settings continue through the
//! ordinary LuCI UCI transaction.

use super::json_wire::{bool_json, json_escape};
use super::process::{run_bounded_command_output, SpawnSpec};
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const COMMAND_OUTPUT_LIMIT: usize = 256 * 1024;
const MAX_HISTORY_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_HISTORY_LINE_BYTES: usize = 16 * 1024;
const MAX_HISTORY_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_HISTORY_OFFSET: u64 = 10_000_000;
const MIN_HISTORY_LIMIT: u64 = 100;
const MAX_HISTORY_LIMIT: u64 = 20_000;

#[derive(Clone, Debug)]
struct Environment {
    apk: PathBuf,
    opkg: PathBuf,
    mwan3: PathBuf,
    nft: PathBuf,
    ubus: PathBuf,
    history_root: PathBuf,
}

impl Environment {
    fn live() -> Self {
        Self {
            apk: env_path("CAKE_AUTORATE_APK_BIN", "/usr/bin/apk"),
            opkg: env_path("CAKE_AUTORATE_OPKG_BIN", "/bin/opkg"),
            mwan3: env_path("CAKE_AUTORATE_MWAN3_BIN", "/usr/sbin/mwan3"),
            nft: env_path("CAKE_AUTORATE_NFT_BIN", "/usr/sbin/nft"),
            ubus: env_path("CAKE_AUTORATE_UBUS_BIN", "/bin/ubus"),
            history_root: env_path("CAKE_AUTORATE_RUN_ROOT", "/var/run/cake-autorate"),
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
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.file_type().is_file()
            && !metadata.file_type().is_symlink()
            && metadata.permissions().mode() & 0o111 != 0
    })
}

fn command_output(program: &Path, arguments: &[&str]) -> Result<Vec<u8>, String> {
    let output = run_bounded_command_output(
        &SpawnSpec {
            program: program.to_path_buf(),
            arguments: arguments.iter().map(OsString::from).collect(),
            environment: Vec::new(),
        },
        COMMAND_TIMEOUT,
        COMMAND_OUTPUT_LIMIT,
        || false,
    )?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!("{} failed with {}", program.display(), output.status)
        } else {
            detail
        });
    }
    Ok(output.stdout)
}

fn optional_command_output(program: &Path, arguments: &[&str]) -> Option<Vec<u8>> {
    executable(program)
        .then(|| command_output(program, arguments).ok())
        .flatten()
}

pub(crate) fn run_package_versions<I>(mut arguments: I) -> Result<String, String>
where
    I: Iterator<Item = String>,
{
    if arguments.next().is_some() {
        return Err("package-versions accepts no options".to_string());
    }
    package_versions(&Environment::live())
}

fn package_versions(environment: &Environment) -> Result<String, String> {
    if !executable(&environment.apk) {
        return Err("apk is unavailable".to_string());
    }
    let output = command_output(
        &environment.apk,
        &[
            "query",
            "--installed",
            "--fields",
            "name,version",
            "--format",
            "json",
            "cake-autorate-rs",
            "luci-app-cake-autorate-rs",
        ],
    )?;
    String::from_utf8(output).map_err(|_| "apk package output is not UTF-8".to_string())
}

pub(crate) fn run_mwan3_info<I>(mut arguments: I) -> Result<String, String>
where
    I: Iterator<Item = String>,
{
    if arguments.next().is_some() {
        return Err("mwan3-info accepts no options".to_string());
    }
    Ok(mwan3_info(&Environment::live()))
}

fn mwan3_info(environment: &Environment) -> String {
    if !executable(&environment.mwan3) {
        return "{\"available\":false,\"nft\":false,\"scoped_status_api\":false,\"version\":\"\",\"reason\":\"mwan3 is not installed\"}\n".to_string();
    }

    let version = if let Some(output) =
        optional_command_output(&environment.apk, &["list", "--installed", "mwan3"])
    {
        parse_apk_mwan3_version(&output)
    } else if let Some(output) =
        optional_command_output(&environment.opkg, &["list-installed", "mwan3"])
    {
        parse_opkg_mwan3_version(&output)
    } else {
        String::new()
    };
    let nft =
        optional_command_output(&environment.nft, &["list", "table", "inet", "mwan3"]).is_some();
    let scoped_status_api = optional_command_output(&environment.ubus, &["-v", "list", "mwan3"])
        .is_some_and(|output| {
            String::from_utf8_lossy(&output).lines().any(|line| {
                line.find("\"status\"")
                    .zip(line.find("\"interface\""))
                    .is_some_and(|(status, interface)| status < interface)
            })
        });
    let reason = if nft && scoped_status_api {
        "native nftables mwan3 and member-scoped status API detected"
    } else if !nft {
        "mwan3 is installed, but the nftables table inet mwan3 is unavailable"
    } else {
        "mwan3 is installed, but the member-scoped ubus status API is unavailable"
    };
    format!(
        "{{\"available\":true,\"nft\":{},\"scoped_status_api\":{},\"version\":\"{}\",\"reason\":\"{}\"}}\n",
        bool_json(nft),
        bool_json(scoped_status_api),
        json_escape(&version),
        json_escape(reason)
    )
}

fn parse_apk_mwan3_version(output: &[u8]) -> String {
    String::from_utf8_lossy(output)
        .lines()
        .find_map(|line| {
            line.strip_prefix("mwan3-")
                .and_then(|tail| tail.split_ascii_whitespace().next())
        })
        .unwrap_or_default()
        .to_string()
}

fn parse_opkg_mwan3_version(output: &[u8]) -> String {
    String::from_utf8_lossy(output)
        .lines()
        .find_map(|line| line.strip_prefix("mwan3 - "))
        .unwrap_or_default()
        .to_string()
}

pub(crate) fn run_graph_history<I>(mut arguments: I) -> Result<Vec<u8>, String>
where
    I: Iterator<Item = String>,
{
    let section = arguments
        .next()
        .ok_or_else(|| "graph-history requires an instance".to_string())?;
    validate_instance(&section)?;
    let mode = arguments.next().unwrap_or_else(|| "read".to_string());
    let environment = Environment::live();
    match mode.as_str() {
        "read" => {
            let offset = parse_u64(arguments.next().as_deref().unwrap_or("0"), "offset")?;
            let limit = parse_u64(arguments.next().as_deref().unwrap_or("10000"), "limit")?;
            if arguments.next().is_some()
                || offset > MAX_HISTORY_OFFSET
                || !(MIN_HISTORY_LIMIT..=MAX_HISTORY_LIMIT).contains(&limit)
            {
                return Err("graph-history read bounds are invalid".to_string());
            }
            read_history(
                &environment.history_root.join(&section).join("history.csv"),
                offset,
                limit,
            )
        }
        "stats" => {
            if arguments.next().is_some() {
                return Err("graph-history stats accepts no options".to_string());
            }
            history_stats(&environment.history_root.join(&section).join("history.csv"))
        }
        _ => Err("graph-history supports only read or stats".to_string()),
    }
}

fn parse_u64(value: &str, label: &str) -> Result<u64, String> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("graph-history {label} is invalid"));
    }
    value
        .parse::<u64>()
        .map_err(|_| format!("graph-history {label} is invalid"))
}

fn validate_instance(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || matches!(value, "." | "..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'@' | b'.' | b'-'))
    {
        return Err("invalid instance".to_string());
    }
    Ok(())
}

fn open_history(path: &Path) -> Result<Option<File>, String> {
    match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => {
            let metadata = file
                .metadata()
                .map_err(|error| format!("unable to inspect graph history: {error}"))?;
            if !metadata.is_file() || metadata.len() > MAX_HISTORY_FILE_BYTES {
                return Err("graph history is not a bounded regular file".to_string());
            }
            Ok(Some(file))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("unable to open graph history: {error}")),
    }
}

fn next_bounded_line<R: BufRead>(reader: &mut R, line: &mut Vec<u8>) -> Result<bool, String> {
    line.clear();
    loop {
        let available = reader
            .fill_buf()
            .map_err(|error| format!("unable to read graph history: {error}"))?;
        if available.is_empty() {
            return Ok(!line.is_empty());
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if line.len().saturating_add(take) > MAX_HISTORY_LINE_BYTES {
            return Err("graph history line exceeds its safety bound".to_string());
        }
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if line.last() == Some(&b'\n') {
            return Ok(true);
        }
    }
}

fn history_counts<R: BufRead>(reader: &mut R) -> Result<(u64, u64, u64), String> {
    let mut line = Vec::new();
    let mut records = 0u64;
    let mut newline_samples = 0u64;
    let mut bytes = 0u64;
    while next_bounded_line(reader, &mut line)? {
        records = records
            .checked_add(1)
            .ok_or_else(|| "graph history record count overflowed".to_string())?;
        bytes = bytes
            .checked_add(line.len() as u64)
            .ok_or_else(|| "graph history byte count overflowed".to_string())?;
        if line.last() == Some(&b'\n') {
            newline_samples = newline_samples
                .checked_add(1)
                .ok_or_else(|| "graph history line count overflowed".to_string())?;
        }
    }
    Ok((records, newline_samples, bytes))
}

fn read_history(path: &Path, offset: u64, limit: u64) -> Result<Vec<u8>, String> {
    let Some(file) = open_history(path)? else {
        return Ok(Vec::new());
    };
    let mut reader = BufReader::new(file);
    let (total, _, _) = history_counts(&mut reader)?;
    let requested = offset
        .checked_add(limit)
        .ok_or_else(|| "graph-history read bounds overflowed".to_string())?;
    let skip = total.saturating_sub(requested);
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|error| format!("unable to rewind graph history: {error}"))?;
    let mut line = Vec::new();
    let mut index = 0u64;
    let mut emitted = 0u64;
    let mut output = Vec::new();
    while next_bounded_line(&mut reader, &mut line)? {
        if index >= skip && emitted < limit {
            if output.len().saturating_add(line.len()) > MAX_HISTORY_OUTPUT_BYTES {
                return Err("graph history output exceeds its safety bound".to_string());
            }
            output.extend_from_slice(&line);
            emitted += 1;
        }
        index += 1;
        if emitted == limit {
            break;
        }
    }
    Ok(output)
}

fn history_stats(path: &Path) -> Result<Vec<u8>, String> {
    let Some(file) = open_history(path)? else {
        return Ok(b"{\"bytes\":0,\"samples\":0}\n".to_vec());
    };
    let mut reader = BufReader::new(file);
    let (_, samples, bytes) = history_counts(&mut reader)?;
    Ok(format!("{{\"bytes\":{bytes},\"samples\":{samples}}}\n").into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

    struct Fixture {
        root: PathBuf,
        environment: Environment,
    }

    impl Fixture {
        fn new() -> Self {
            let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root =
                env::temp_dir().join(format!("cake-luci-readouts-{}-{id}", std::process::id()));
            fs::create_dir(&root).unwrap();
            let environment = Environment {
                apk: root.join("apk"),
                opkg: root.join("opkg"),
                mwan3: root.join("mwan3"),
                nft: root.join("nft"),
                ubus: root.join("ubus"),
                history_root: root.join("history"),
            };
            fs::create_dir(&environment.history_root).unwrap();
            Self { root, environment }
        }

        fn executable(&self, path: &Path, body: &str) {
            fs::write(path, body).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn package_versions_preserves_exact_apk_json_and_fails_without_apk() {
        let fixture = Fixture::new();
        assert_eq!(
            package_versions(&fixture.environment).unwrap_err(),
            "apk is unavailable"
        );
        fixture.executable(
            &fixture.environment.apk,
            "#!/bin/sh\nprintf '%s\\n' '[{\"name\":\"cake-autorate-rs\",\"version\":\"1\"}]'\n",
        );
        assert_eq!(
            package_versions(&fixture.environment).unwrap(),
            "[{\"name\":\"cake-autorate-rs\",\"version\":\"1\"}]\n"
        );
    }

    #[test]
    fn mwan3_projection_matches_available_and_missing_contracts() {
        let fixture = Fixture::new();
        assert!(mwan3_info(&fixture.environment).contains("\"available\":false"));
        fixture.executable(&fixture.environment.mwan3, "#!/bin/sh\nexit 0\n");
        fixture.executable(
            &fixture.environment.apk,
            "#!/bin/sh\nprintf '%s\\n' 'mwan3-2.11.20-r1 description'\n",
        );
        fixture.executable(&fixture.environment.nft, "#!/bin/sh\nexit 0\n");
        fixture.executable(
            &fixture.environment.ubus,
            "#!/bin/sh\nprintf '%s\\n' '\"status\": { \"interface\": \"String\" }'\n",
        );
        assert_eq!(
            mwan3_info(&fixture.environment),
            "{\"available\":true,\"nft\":true,\"scoped_status_api\":true,\"version\":\"2.11.20-r1\",\"reason\":\"native nftables mwan3 and member-scoped status API detected\"}\n"
        );
        fs::remove_file(&fixture.environment.nft).unwrap();
        assert!(
            mwan3_info(&fixture.environment).contains("nftables table inet mwan3 is unavailable")
        );
    }

    #[test]
    fn graph_history_read_and_stats_match_tail_head_semantics() {
        let fixture = Fixture::new();
        let directory = fixture.environment.history_root.join("wan");
        fs::create_dir(&directory).unwrap();
        let path = directory.join("history.csv");
        let mut source = String::new();
        for index in 0..250 {
            source.push_str(&format!("{index},line\n"));
        }
        fs::write(&path, &source).unwrap();
        let output = read_history(&path, 10, 100).unwrap();
        let expected = (140..240)
            .map(|index| format!("{index},line\n"))
            .collect::<String>();
        assert_eq!(output, expected.as_bytes());
        assert_eq!(
            history_stats(&path).unwrap(),
            format!("{{\"bytes\":{},\"samples\":250}}\n", source.len()).as_bytes()
        );
        fs::write(&path, b"a\nb").unwrap();
        assert_eq!(read_history(&path, 0, 100).unwrap(), b"a\nb");
        assert_eq!(
            history_stats(&path).unwrap(),
            b"{\"bytes\":3,\"samples\":1}\n"
        );
    }

    #[test]
    fn graph_history_rejects_traversal_symlinks_and_oversized_lines() {
        assert!(validate_instance("..").is_err());
        assert!(validate_instance("wan/other").is_err());
        let fixture = Fixture::new();
        let directory = fixture.environment.history_root.join("wan");
        fs::create_dir(&directory).unwrap();
        let target = fixture.root.join("foreign");
        fs::write(&target, b"secret\n").unwrap();
        std::os::unix::fs::symlink(&target, directory.join("history.csv")).unwrap();
        assert!(read_history(&directory.join("history.csv"), 0, 100).is_err());
        fs::remove_file(directory.join("history.csv")).unwrap();
        fs::write(
            directory.join("history.csv"),
            vec![b'x'; MAX_HISTORY_LINE_BYTES + 1],
        )
        .unwrap();
        assert!(history_stats(&directory.join("history.csv")).is_err());
    }
}
