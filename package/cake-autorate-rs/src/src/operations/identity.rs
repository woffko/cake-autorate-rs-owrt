use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_PROC_ROOT: &str = "/proc";
pub const DEFAULT_BOOT_ID_PATH: &str = "/proc/sys/kernel/random/boot_id";
pub const DEFAULT_RANDOM_UUID_PATH: &str = "/proc/sys/kernel/random/uuid";
pub const DEFAULT_UPTIME_PATH: &str = "/proc/uptime";

extern "C" {
    fn getpid() -> i32;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub process_group: u32,
    pub starttime_ticks: u64,
}

impl ProcessIdentity {
    pub fn current() -> Result<Self, String> {
        let raw_pid = unsafe { getpid() };
        let pid = u32::try_from(raw_pid).map_err(|_| "process PID is invalid".to_string())?;
        Self::inspect(Path::new(DEFAULT_PROC_ROOT), pid)
    }

    pub fn inspect(proc_root: &Path, pid: u32) -> Result<Self, String> {
        let stat_path = proc_root.join(pid.to_string()).join("stat");
        let stat = fs::read_to_string(&stat_path)
            .map_err(|error| format!("unable to read {}: {error}", stat_path.display()))?;
        parse_proc_stat(pid, &stat)
    }

    /// Inspect a process only while the kernel still considers it live.
    ///
    /// During task exit procfs can retain an exact command line briefly after
    /// the process has entered Z/X state, while identity fields such as pgrp
    /// are already unavailable.  That is an observed exit transition, not a
    /// malformed live identity.  Live states remain fully fail-closed through
    /// the ordinary identity parser.
    pub fn inspect_live(proc_root: &Path, pid: u32) -> Result<Option<Self>, String> {
        let stat_path = process_stat_path(proc_root, pid);
        let stat = match fs::read_to_string(&stat_path) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("unable to read {}: {error}", stat_path.display())),
        };
        if matches!(parse_proc_stat_state(pid, &stat)?, b'Z' | b'X' | b'x') {
            return Ok(None);
        }
        parse_proc_stat(pid, &stat).map(Some)
    }

    pub fn still_matches(&self, proc_root: &Path) -> Result<bool, String> {
        Ok(Self::inspect_live(proc_root, self.pid)?.as_ref() == Some(self))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoordinatorIdentity {
    pub boot_id: String,
    pub generation: String,
    pub process: ProcessIdentity,
}

impl CoordinatorIdentity {
    pub fn current() -> Result<Self, String> {
        Self::from_paths(
            Path::new(DEFAULT_PROC_ROOT),
            Path::new(DEFAULT_BOOT_ID_PATH),
            Path::new(DEFAULT_RANDOM_UUID_PATH),
        )
    }

    pub fn from_paths(
        proc_root: &Path,
        boot_id_path: &Path,
        random_uuid_path: &Path,
    ) -> Result<Self, String> {
        let process = ProcessIdentity::current()?;
        Ok(Self {
            boot_id: read_kernel_uuid(boot_id_path, "boot ID")?,
            generation: read_kernel_uuid(random_uuid_path, "coordinator generation")?,
            process: if proc_root == Path::new(DEFAULT_PROC_ROOT) {
                process
            } else {
                ProcessIdentity::inspect(proc_root, process.pid)?
            },
        })
    }
}

pub fn read_kernel_uuid(path: &Path, label: &str) -> Result<String, String> {
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("unable to read {label} from {}: {error}", path.display()))?;
    normalize_kernel_uuid(raw.trim()).ok_or_else(|| format!("kernel {label} is malformed"))
}

pub fn normalize_kernel_uuid(value: &str) -> Option<String> {
    let mut normalized = String::with_capacity(32);
    for byte in value.bytes() {
        match byte {
            b'-' => {}
            b'0'..=b'9' | b'a'..=b'f' => normalized.push(char::from(byte)),
            b'A'..=b'F' => normalized.push(char::from(byte.to_ascii_lowercase())),
            _ => return None,
        }
    }
    (normalized.len() == 32).then_some(normalized)
}

pub fn parse_proc_stat(pid: u32, stat: &str) -> Result<ProcessIdentity, String> {
    parse_proc_stat_with_state(pid, stat).map(|(identity, _)| identity)
}

fn parse_proc_stat_with_state(pid: u32, stat: &str) -> Result<(ProcessIdentity, u8), String> {
    let close = stat
        .rfind(')')
        .ok_or_else(|| "process stat is missing the command terminator".to_string())?;
    let prefix = stat
        .get(..close + 1)
        .ok_or_else(|| "process stat command is malformed".to_string())?;
    let expected_prefix = format!("{pid} (");
    if !prefix.starts_with(&expected_prefix) {
        return Err("process stat PID does not match the requested PID".to_string());
    }
    let fields: Vec<&str> = stat[close + 1..].split_ascii_whitespace().collect();
    // The tail starts at kernel field 3 (state).  pgrp is field 5 and process
    // starttime is field 22, hence tail indices 2 and 19 respectively.
    if fields.len() <= 19 {
        return Err("process stat has too few fields".to_string());
    }
    let state = fields[0].as_bytes();
    if state.len() != 1 || !state[0].is_ascii_alphabetic() {
        return Err("process stat state is invalid".to_string());
    }
    let process_group = fields[2]
        .parse::<u32>()
        .map_err(|_| "process stat process group is invalid".to_string())?;
    let starttime_ticks = fields[19]
        .parse::<u64>()
        .map_err(|_| "process stat starttime is invalid".to_string())?;
    if process_group == 0 || starttime_ticks == 0 {
        return Err("process stat identity fields must be non-zero".to_string());
    }
    Ok((
        ProcessIdentity {
            pid,
            process_group,
            starttime_ticks,
        },
        state[0],
    ))
}

fn parse_proc_stat_state(pid: u32, stat: &str) -> Result<u8, String> {
    let close = stat
        .rfind(')')
        .ok_or_else(|| "process stat is missing the command terminator".to_string())?;
    let prefix = stat
        .get(..close + 1)
        .ok_or_else(|| "process stat command is malformed".to_string())?;
    if !prefix.starts_with(&format!("{pid} (")) {
        return Err("process stat PID does not match the requested PID".to_string());
    }
    let state = stat[close + 1..]
        .split_ascii_whitespace()
        .next()
        .ok_or_else(|| "process stat has too few fields".to_string())?
        .as_bytes();
    if state.len() != 1 || !state[0].is_ascii_alphabetic() {
        return Err("process stat state is invalid".to_string());
    }
    Ok(state[0])
}

pub fn process_stat_path(proc_root: &Path, pid: u32) -> PathBuf {
    proc_root.join(pid.to_string()).join("stat")
}

pub fn monotonic_boot_ms() -> Result<u64, String> {
    monotonic_boot_ms_from(Path::new(DEFAULT_UPTIME_PATH))
}

pub fn monotonic_boot_ms_from(path: &Path) -> Result<u64, String> {
    let input = fs::read_to_string(path)
        .map_err(|error| format!("unable to read {}: {error}", path.display()))?;
    parse_uptime_ms(&input)
}

fn parse_uptime_ms(input: &str) -> Result<u64, String> {
    if input.len() > 256 {
        return Err("kernel uptime record is malformed".to_string());
    }
    let input = input.strip_suffix('\n').unwrap_or(input);
    if input.contains(['\n', '\r', '\0']) {
        return Err("kernel uptime record is malformed".to_string());
    }
    let value = input
        .split_ascii_whitespace()
        .next()
        .ok_or_else(|| "kernel uptime is missing".to_string())?;
    let (seconds, fraction) = value.split_once('.').unwrap_or((value, ""));
    if seconds.is_empty()
        || seconds.bytes().any(|byte| !byte.is_ascii_digit())
        || fraction.bytes().any(|byte| !byte.is_ascii_digit())
    {
        return Err("kernel uptime is not numeric".to_string());
    }
    let seconds = seconds
        .parse::<u64>()
        .map_err(|_| "kernel uptime seconds overflow".to_string())?;
    let mut milliseconds = 0u64;
    let mut scale = 100u64;
    for digit in fraction.bytes().take(3) {
        milliseconds = milliseconds
            .checked_add(u64::from(digit - b'0') * scale)
            .ok_or_else(|| "kernel uptime milliseconds overflow".to_string())?;
        scale /= 10;
    }
    seconds
        .checked_mul(1_000)
        .and_then(|value| value.checked_add(milliseconds))
        .ok_or_else(|| "kernel uptime milliseconds overflow".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_stat(pid: u32, command: &str, process_group: u32, starttime: u64) -> String {
        fake_stat_with_state(pid, command, "S", process_group, starttime)
    }

    fn fake_stat_with_state(
        pid: u32,
        command: &str,
        state: &str,
        process_group: u32,
        starttime: u64,
    ) -> String {
        let mut tail = vec![
            state.to_string(),
            "1".to_string(),
            process_group.to_string(),
            process_group.to_string(),
        ];
        while tail.len() < 19 {
            tail.push("0".to_string());
        }
        tail.push(starttime.to_string());
        format!("{pid} ({command}) {} 0", tail.join(" "))
    }

    #[test]
    fn parses_starttime_after_a_command_with_spaces_and_parentheses() {
        let stat = fake_stat(42, "worker ) with spaces", 41, 987654);
        let identity = parse_proc_stat(42, &stat).unwrap();
        assert_eq!(identity.pid, 42);
        assert_eq!(identity.process_group, 41);
        assert_eq!(identity.starttime_ticks, 987654);
    }

    #[test]
    fn rejects_pid_mismatch_short_and_zero_identity_records() {
        assert!(parse_proc_stat(9, "8 (worker) S 1 8 8").is_err());
        assert!(parse_proc_stat(8, "8 (worker) S 1 8 8").is_err());
        let zero = fake_stat(8, "worker", 0, 8);
        assert!(parse_proc_stat(8, &zero).is_err());
    }

    #[test]
    fn kernel_uuid_is_canonical_lower_hex() {
        assert_eq!(
            normalize_kernel_uuid("12345678-90AB-CDEF-1234-567890ABCDEF"),
            Some("1234567890abcdef1234567890abcdef".to_string())
        );
        assert_eq!(normalize_kernel_uuid("1234"), None);
        assert_eq!(
            normalize_kernel_uuid("12345678-90ab-cdef-1234-567890abcdeg"),
            None
        );
    }

    #[test]
    fn exact_process_identity_detects_pid_reuse() {
        let root =
            std::env::temp_dir().join(format!("cake-process-identity-{}", unsafe { getpid() }));
        let proc_dir = root.join("42");
        fs::create_dir_all(&proc_dir).unwrap();
        let first = fake_stat(42, "worker", 41, 123);
        fs::write(proc_dir.join("stat"), &first).unwrap();
        let identity = ProcessIdentity::inspect(&root, 42).unwrap();
        assert!(identity.still_matches(&root).unwrap());
        let reused = fake_stat(42, "worker", 41, 124);
        fs::write(proc_dir.join("stat"), reused).unwrap();
        assert!(!identity.still_matches(&root).unwrap());
        fs::remove_file(proc_dir.join("stat")).unwrap();
        fs::remove_dir(proc_dir).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn exact_process_identity_treats_a_matching_zombie_as_not_live() {
        let root = std::env::temp_dir().join(format!("cake-process-zombie-identity-{}", unsafe {
            getpid()
        }));
        let proc_dir = root.join("42");
        fs::create_dir_all(&proc_dir).unwrap();
        fs::write(proc_dir.join("stat"), fake_stat(42, "worker", 41, 123)).unwrap();
        let identity = ProcessIdentity::inspect(&root, 42).unwrap();
        assert!(identity.still_matches(&root).unwrap());

        fs::write(
            proc_dir.join("stat"),
            fake_stat_with_state(42, "worker", "Z", 41, 123),
        )
        .unwrap();
        assert!(!identity.still_matches(&root).unwrap());

        fs::remove_file(proc_dir.join("stat")).unwrap();
        fs::remove_dir(proc_dir).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn exiting_process_does_not_require_identity_fields_that_the_kernel_released() {
        let root = std::env::temp_dir().join(format!("cake-process-exiting-identity-{}", unsafe {
            getpid()
        }));
        let proc_dir = root.join("42");
        fs::create_dir_all(&proc_dir).unwrap();
        fs::write(proc_dir.join("stat"), fake_stat(42, "worker", 41, 123)).unwrap();
        let identity = ProcessIdentity::inspect(&root, 42).unwrap();

        let mut fields = vec!["X".to_string(), "1".to_string(), "-1".to_string()];
        while fields.len() <= 19 {
            fields.push("0".to_string());
        }
        for state in ["Z", "X", "x"] {
            fields[0] = state.to_string();
            fs::write(
                proc_dir.join("stat"),
                format!("42 (worker) {} 0", fields.join(" ")),
            )
            .unwrap();
            assert_eq!(ProcessIdentity::inspect_live(&root, 42).unwrap(), None);
            assert!(!identity.still_matches(&root).unwrap());
        }
        for state in ["S", "R"] {
            fields[0] = state.to_string();
            fs::write(
                proc_dir.join("stat"),
                format!("42 (worker) {} 0", fields.join(" ")),
            )
            .unwrap();
            assert!(ProcessIdentity::inspect_live(&root, 42).is_err());
        }

        fs::remove_file(proc_dir.join("stat")).unwrap();
        fs::remove_dir(proc_dir).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn kernel_uptime_is_parsed_without_floating_point_or_wall_clock() {
        assert_eq!(parse_uptime_ms("123.45 99.00").unwrap(), 123_450);
        assert_eq!(parse_uptime_ms("7.001 1.0").unwrap(), 7_001);
        assert_eq!(parse_uptime_ms("9 1").unwrap(), 9_000);
        assert_eq!(parse_uptime_ms("10.25 4.0\n").unwrap(), 10_250);
        assert!(parse_uptime_ms("1e3 1").is_err());
        assert!(parse_uptime_ms("\n").is_err());
    }
}
