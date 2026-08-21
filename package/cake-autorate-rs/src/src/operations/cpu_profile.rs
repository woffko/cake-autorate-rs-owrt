//! Bounded, event-driven CPU profiling for the controller and its probe
//! processes. This replaces the shipped shell/awk helper while preserving its
//! operator-facing report.

use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;

const DEFAULT_DURATION_SECONDS: u64 = 30;
const MIN_DURATION_SECONDS: u64 = 5;
const MAX_DURATION_SECONDS: u64 = 300;
const MAX_PROC_BYTES: u64 = 4096;
const MAX_COMMAND_DISPLAY: usize = 512;
const DAEMON_PATH: &[u8] = b"/usr/sbin/cake-autorated";

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProcessCounters {
    total_ticks: u64,
    starttime_ticks: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ProcessSample {
    pid: u32,
    starttime_ticks: u64,
    before_ticks: u64,
    kind: &'static str,
    command: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SystemCounters {
    total: u64,
    busy: u64,
    softirq: u64,
    logical_cpus: u64,
}

pub(crate) fn run_cpu_profile<I>(mut arguments: I) -> Result<String, String>
where
    I: Iterator<Item = String>,
{
    let duration = match arguments.next() {
        Some(value) => value
            .parse::<u64>()
            .map_err(|_| "cpu-profile duration must be an integer".to_string())?,
        None => DEFAULT_DURATION_SECONDS,
    };
    if arguments.next().is_some() {
        return Err("cpu-profile accepts at most one duration".to_string());
    }
    if !(MIN_DURATION_SECONDS..=MAX_DURATION_SECONDS).contains(&duration) {
        return Err(format!(
            "cpu-profile duration must be between {MIN_DURATION_SECONDS} and {MAX_DURATION_SECONDS} seconds"
        ));
    }
    profile(Path::new("/proc"), duration)
}

fn profile(proc_root: &Path, duration: u64) -> Result<String, String> {
    let processes = discover_processes(proc_root)?;
    if processes.is_empty() {
        return Err("no cake-autorate processes found".to_string());
    }
    let before = read_system_counters(&proc_root.join("stat"))?;
    let clock_ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if clock_ticks <= 0 {
        return Err("unable to determine the process clock tick rate".to_string());
    }
    wait_measurement_window(duration)?;
    let after = read_system_counters(&proc_root.join("stat"))?;
    let total_delta = after.total.saturating_sub(before.total);
    if total_delta == 0 {
        return Err("system CPU counters did not advance".to_string());
    }
    let busy_delta = after.busy.saturating_sub(before.busy);
    let softirq_delta = after.softirq.saturating_sub(before.softirq);
    let logical_cpus = before.logical_cpus.max(1);

    let mut output = format!(
        "Window: {duration}s; clock: {clock_ticks} Hz; logical CPUs: {logical_cpus}\nRouter busy: {:.2}%; softirq: {:.2}% of total capacity\n",
        busy_delta as f64 * 100.0 / total_delta as f64,
        softirq_delta as f64 * 100.0 / total_delta as f64,
    );
    for process in processes {
        match read_process_counters(&proc_root.join(process.pid.to_string()).join("stat")) {
            Ok(current) if current.starttime_ticks == process.starttime_ticks => {
                let delta = current.total_ticks.saturating_sub(process.before_ticks);
                let one_cpu = delta as f64 * 100.0 / clock_ticks as f64 / duration as f64;
                let total = one_cpu / logical_cpus as f64;
                output.push_str(&format!(
                    "{:<9} pid {:<6} {:6.2}% of one CPU; {:6.2}% total: {}\n",
                    process.kind, process.pid, one_cpu, total, process.command
                ));
            }
            Ok(_) => output.push_str(&format!(
                "{:<9} pid {:<6} replaced during measurement: {}\n",
                process.kind, process.pid, process.command
            )),
            Err(_) => output.push_str(&format!(
                "{:<9} pid {:<6} exited during measurement: {}\n",
                process.kind, process.pid, process.command
            )),
        }
    }
    Ok(output)
}

fn discover_processes(proc_root: &Path) -> Result<Vec<ProcessSample>, String> {
    let mut pids = Vec::new();
    for entry in fs::read_dir(proc_root)
        .map_err(|error| format!("unable to enumerate processes: {error}"))?
    {
        let entry = entry.map_err(|error| format!("unable to enumerate process: {error}"))?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        if pid != std::process::id() {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    let mut result = Vec::new();
    for pid in pids {
        let root = proc_root.join(pid.to_string());
        let Ok(cmdline) = read_bounded(&root.join("cmdline")) else {
            continue;
        };
        let Some(kind) = classify_command(&cmdline) else {
            continue;
        };
        let counters = read_process_counters(&root.join("stat"))?;
        result.push(ProcessSample {
            pid,
            starttime_ticks: counters.starttime_ticks,
            before_ticks: counters.total_ticks,
            kind,
            command: command_display(&cmdline),
        });
    }
    Ok(result)
}

fn classify_command(cmdline: &[u8]) -> Option<&'static str> {
    let argv0 = cmdline.split(|byte| *byte == 0).next()?;
    if argv0 == DAEMON_PATH {
        return Some("daemon");
    }
    let basename = argv0.rsplit(|byte| *byte == b'/').next().unwrap_or(argv0);
    if matches!(basename, b"fping" | b"fping-ts" | b"tsping" | b"irtt") {
        Some("probe")
    } else {
        None
    }
}

fn command_display(cmdline: &[u8]) -> String {
    let mut output = String::new();
    for byte in cmdline.iter().copied() {
        if output.len() >= MAX_COMMAND_DISPLAY {
            output.push_str("...");
            break;
        }
        match byte {
            0 => output.push(' '),
            b' '..=b'~' => output.push(byte as char),
            _ => output.push('?'),
        }
    }
    output.trim_end().to_string()
}

fn read_process_counters(path: &Path) -> Result<ProcessCounters, String> {
    let bytes = read_bounded(path)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| format!("process stat {} is not UTF-8", path.display()))?;
    let close = text
        .rfind(')')
        .ok_or_else(|| format!("process stat {} has no command boundary", path.display()))?;
    let fields = text[close + 1..]
        .split_ascii_whitespace()
        .collect::<Vec<_>>();
    if fields.len() < 20 {
        return Err(format!("process stat {} is incomplete", path.display()));
    }
    let parse = |index: usize| -> Result<u64, String> {
        fields[index]
            .parse::<u64>()
            .map_err(|_| format!("process stat {} field is invalid", path.display()))
    };
    let total_ticks = [11, 12, 13, 14].into_iter().try_fold(0u64, |sum, index| {
        sum.checked_add(parse(index)?)
            .ok_or_else(|| "process CPU tick total overflowed".to_string())
    })?;
    Ok(ProcessCounters {
        total_ticks,
        starttime_ticks: parse(19)?,
    })
}

fn read_system_counters(path: &Path) -> Result<SystemCounters, String> {
    let bytes = read_bounded(path)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| format!("system stat {} is not UTF-8", path.display()))?;
    let mut aggregate = None;
    let mut logical_cpus = 0u64;
    for line in text.lines() {
        let mut fields = line.split_ascii_whitespace();
        let Some(name) = fields.next() else { continue };
        if name == "cpu" {
            let values = fields
                .take(8)
                .map(|value| {
                    value
                        .parse::<u64>()
                        .map_err(|_| "system CPU counter is invalid".to_string())
                })
                .collect::<Result<Vec<_>, _>>()?;
            if values.len() < 4 {
                return Err("aggregate system CPU counters are incomplete".to_string());
            }
            let user = values[0];
            let nice = values[1];
            let system = values[2];
            let idle = values[3];
            let iowait = values.get(4).copied().unwrap_or(0);
            let irq = values.get(5).copied().unwrap_or(0);
            let softirq = values.get(6).copied().unwrap_or(0);
            let steal = values.get(7).copied().unwrap_or(0);
            let busy = [user, nice, system, irq, softirq, steal]
                .into_iter()
                .try_fold(0u64, |sum, value| sum.checked_add(value))
                .ok_or_else(|| "system CPU busy counter overflowed".to_string())?;
            let total = busy
                .checked_add(idle)
                .and_then(|value| value.checked_add(iowait))
                .ok_or_else(|| "system CPU total overflowed".to_string())?;
            aggregate = Some((total, busy, softirq));
        } else if name.strip_prefix("cpu").is_some_and(|suffix| {
            !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit())
        }) {
            logical_cpus = logical_cpus
                .checked_add(1)
                .ok_or_else(|| "logical CPU count overflowed".to_string())?;
        }
    }
    let (total, busy, softirq) =
        aggregate.ok_or_else(|| "aggregate system CPU counters are missing".to_string())?;
    Ok(SystemCounters {
        total,
        busy,
        softirq,
        logical_cpus: logical_cpus.max(1),
    })
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("unable to inspect {}: {error}", path.display()))?;
    if metadata.len() > MAX_PROC_BYTES {
        return Err(format!("{} exceeds the CPU profiler bound", path.display()));
    }
    let bytes =
        fs::read(path).map_err(|error| format!("unable to read {}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_PROC_BYTES {
        return Err(format!("{} exceeds the CPU profiler bound", path.display()));
    }
    Ok(bytes)
}

fn wait_measurement_window(seconds: u64) -> Result<(), String> {
    let raw = unsafe { libc::timerfd_create(libc::CLOCK_MONOTONIC, libc::TFD_CLOEXEC) };
    if raw < 0 {
        return Err(format!(
            "unable to create the CPU profiler timer: {}",
            io::Error::last_os_error()
        ));
    }
    let timer = unsafe { OwnedFd::from_raw_fd(raw) };
    let specification = libc::itimerspec {
        it_interval: libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        },
        it_value: libc::timespec {
            tv_sec: seconds as libc::time_t,
            tv_nsec: 0,
        },
    };
    if unsafe { libc::timerfd_settime(timer.as_raw_fd(), 0, &specification, std::ptr::null_mut()) }
        != 0
    {
        return Err(format!(
            "unable to arm the CPU profiler timer: {}",
            io::Error::last_os_error()
        ));
    }
    let mut descriptor = libc::pollfd {
        fd: timer.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        let ready = unsafe { libc::poll(&mut descriptor, 1, -1) };
        if ready > 0 && descriptor.revents & libc::POLLIN != 0 {
            break;
        }
        if ready < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err("CPU profiler timer wait failed".to_string());
    }
    let mut expirations = 0u64;
    let read = unsafe {
        libc::read(
            timer.as_raw_fd(),
            (&mut expirations as *mut u64).cast(),
            std::mem::size_of::<u64>(),
        )
    };
    if read != std::mem::size_of::<u64>() as isize || expirations == 0 {
        return Err("CPU profiler timer completion is invalid".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn root(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "cake-cpu-profile-{label}-{}-{}",
            std::process::id(),
            TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn proc_stat_parser_handles_spaces_parentheses_and_pid_identity() {
        let root = root("stat");
        let path = root.join("stat");
        fs::write(
            &path,
            "42 (probe name (worker)) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22\n",
        )
        .unwrap();
        let counters = read_process_counters(&path).unwrap();
        assert_eq!(counters.total_ticks, 50);
        assert_eq!(counters.starttime_ticks, 19);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn system_parser_uses_busy_and_softirq_without_double_counting_idle() {
        let root = root("system");
        let path = root.join("stat");
        fs::write(
            &path,
            "cpu 10 2 3 20 5 4 6 7 0 0\ncpu0 1 0 0 1\ncpu1 1 0 0 1\nintr 0\n",
        )
        .unwrap();
        let counters = read_system_counters(&path).unwrap();
        assert_eq!(counters.busy, 32);
        assert_eq!(counters.total, 57);
        assert_eq!(counters.softirq, 6);
        assert_eq!(counters.logical_cpus, 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discovery_is_exact_bounded_and_ignores_the_profiler_itself() {
        let root = root("discovery");
        fs::write(root.join("stat"), "cpu 1 0 0 1\ncpu0 1 0 0 1\n").unwrap();
        for (pid, command) in [
            (
                101u32,
                b"/usr/sbin/cake-autorated\0--instance\0wan\0".as_slice(),
            ),
            (102, b"/usr/bin/fping\0-A\0".as_slice()),
            (103, b"/usr/sbin/cake-autorated-old\0".as_slice()),
        ] {
            let process = root.join(pid.to_string());
            fs::create_dir(&process).unwrap();
            fs::write(process.join("cmdline"), command).unwrap();
            fs::write(
                process.join("stat"),
                format!("{pid} (worker) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20\n"),
            )
            .unwrap();
        }
        let samples = discover_processes(&root).unwrap();
        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].kind, "daemon");
        assert_eq!(samples[1].kind, "probe");
        assert!(samples[0].command.contains("--instance wan"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unsafe_display_bytes_are_never_written_to_the_terminal() {
        assert_eq!(
            command_display(b"fping\0ok\0bad\x1b[31m\0"),
            "fping ok bad?[31m"
        );
        let root = root("bound");
        let path = root.join("oversize");
        fs::write(&path, vec![b'x'; MAX_PROC_BYTES as usize + 1]).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(read_bounded(&path).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
