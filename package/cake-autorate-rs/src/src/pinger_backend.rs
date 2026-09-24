//! Runtime probe capabilities and timing, separate from immutable UCI intent.
use super::{monitor_tick_timeout, pinger_command, Config};
use crate::operations::process::{run_bounded_command_output, SpawnSpec};
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PingerTiming {
    pub interval_s: f64,
    pub stall_timeout: Duration,
    pub response_deadline: Duration,
}

impl PingerTiming {
    pub fn configured(cfg: &Config) -> Self {
        // Until ping is identified, assume its portable whole-second cadence.
        let interval = if cfg.pinger_method == "ping" {
            cfg.reflector_ping_interval_s.ceil().max(1.0)
        } else if cfg.pinger_method == "irtt" {
            cfg.reflector_ping_interval_s
        } else {
            (cfg.reflector_ping_interval_s * 1000.0).round().max(1.0) / 1000.0
        };
        Self::for_interval(cfg, interval)
    }

    fn for_interval(cfg: &Config, interval_s: f64) -> Self {
        // Independent ping children can reply in a cluster: interval/N is an
        // average, not a bound on the next gap. Allow one cycle plus quarter-
        // cycle jitter (at least one controller monitoring tick) before STALL.
        let jitter_s = (interval_s / 4.0).max(monitor_tick_timeout(cfg).as_secs_f64());
        let response_average = interval_s / cfg.no_pingers.max(1) as f64;
        Self {
            interval_s,
            stall_timeout: Duration::from_secs_f64(
                (cfg.stall_detection_thr as f64 * response_average).max(interval_s) + jitter_s,
            ),
            // Equality with the cadence is not a useful health deadline. One
            // missing reply plus phase/jitter must not repeatedly replace a peer.
            response_deadline: Duration::from_secs_f64(
                cfg.reflector_response_deadline_s
                    .max(2.0 * interval_s + jitter_s),
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PingBackend {
    Iputils,
    BusyBox,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PingerPlan {
    backend: Option<PingBackend>,
    pub timing: PingerTiming,
}

impl PingerPlan {
    pub fn configured(cfg: &Config) -> Self {
        Self {
            backend: None,
            timing: PingerTiming::configured(cfg),
        }
    }
    pub fn detect(cfg: &Config) -> Result<Self, String> {
        if !matches!(cfg.pinger_method.as_str(), "ping" | "tsping") {
            return Ok(Self::configured(cfg));
        }
        let command = capability_command(cfg)?;
        let spec = SpawnSpec {
            program: capability_program(
                command.get_program(),
                std::env::var_os("PATH").as_deref(),
            )?,
            arguments: command.get_args().map(ToOwned::to_owned).collect(),
            environment: vec![("LC_ALL".into(), "C".into())],
        };
        let output = run_bounded_command_output(&spec, Duration::from_secs(2), 8192, || {
            super::TERMINATE.load(std::sync::atomic::Ordering::SeqCst)
        })?;
        if cfg.pinger_method == "tsping" {
            if !output.status.success()
                || !tsping_help_supports_binding(&output.stdout, &output.stderr)
            {
                return Err("tsping lacks the required --interface/capture options".into());
            }
            return Ok(Self::configured(cfg));
        }
        Self::from_version(cfg, output.status.success(), &output.stdout, &output.stderr)
    }

    pub(super) fn from_version(
        cfg: &Config,
        success: bool,
        stdout: &[u8],
        stderr: &[u8],
    ) -> Result<Self, String> {
        let stdout = std::str::from_utf8(stdout).map_err(|_| "invalid ping capability output")?;
        let stderr = std::str::from_utf8(stderr).map_err(|_| "invalid ping capability output")?;
        let iputils = success
            && stdout
                .lines()
                .any(|line| line.starts_with("ping from iputils "));
        let busybox = [stdout, stderr].iter().any(|text| {
            text.lines().any(|line| line.starts_with("BusyBox v"))
                && text
                    .lines()
                    .any(|line| line.trim_start().starts_with("-i "))
                && text
                    .lines()
                    .any(|line| line.trim_start().starts_with("-W "))
                && text
                    .lines()
                    .any(|line| line.trim_start().starts_with("-I "))
        });
        let backend = match (iputils, busybox) {
            (true, false) => PingBackend::Iputils,
            (false, true) => PingBackend::BusyBox,
            _ => return Err("ping backend capabilities are unknown or unsupported".into()),
        };
        let interval_s = match backend {
            PingBackend::Iputils => cfg.reflector_ping_interval_s,
            // BusyBox help does not expose FLOAT_DURATION. Integer intervals
            // work on both builds; this is a compatibility policy, not a claim
            // that all BusyBox builds lack fractional interval support.
            PingBackend::BusyBox => cfg.reflector_ping_interval_s.ceil().max(1.0),
        };
        Ok(Self {
            backend: Some(backend),
            timing: PingerTiming::for_interval(cfg, interval_s),
        })
    }

    pub fn name(&self) -> &'static str {
        match self.backend {
            Some(PingBackend::Iputils) => "iputils-timestamped",
            Some(PingBackend::BusyBox) => "busybox-integer-compatible",
            None => "configured-native",
        }
    }

    pub fn append_ping_arguments(&self, command: &mut Command) {
        command
            .arg("-n")
            .arg("-i")
            .arg(self.timing.interval_s.to_string())
            .args(["-W", "10"]);
        if self.backend == Some(PingBackend::Iputils) {
            command.arg("-D");
        }
    }
}

// This lane is intentionally restricted to fixed no-destination capability
// arguments. Actual explicit probes must still have an admitted owner.
fn capability_command(cfg: &Config) -> Result<Command, String> {
    let argument = match cfg.pinger_method.as_str() {
        "ping" => "-V",
        "tsping" => "--help",
        _ => return Err("unsupported pinger capability command".into()),
    };
    let mut command = if cfg.route_mode == "explicit" {
        if !cfg.ping_prefix_string.trim().is_empty()
            || !cfg.mwan3_member.is_empty()
            || cfg.explicit_route_authority.is_none()
            || !crate::routing::is_safe_identifier(&cfg.ul_if)
        {
            return Err(
                "explicit capability check requires unprefixed configured authority".into(),
            );
        }
        Command::new(&cfg.pinger_method)
    } else {
        pinger_command(cfg, &cfg.pinger_method, None)?
    };
    command.arg(argument);
    Ok(command)
}

fn capability_program(program: &OsStr, search_path: Option<&OsStr>) -> Result<PathBuf, String> {
    let program = Path::new(program);
    if program.is_absolute() {
        return Ok(program.to_path_buf());
    }
    if program.components().count() != 1 {
        return Err("relative pinger capability program paths are unsupported".into());
    }
    let paths = search_path.unwrap_or_else(|| OsStr::new("/usr/sbin:/usr/bin:/sbin:/bin"));
    for directory in std::env::split_paths(paths) {
        let directory = if directory.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            directory
        };
        let Ok(directory) = directory.canonicalize() else {
            continue;
        };
        let candidate = directory.join(program);
        if candidate
            .metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        {
            // Preserve the final symlink name: BusyBox dispatches via argv[0].
            // SpawnSpec still validates ownership/mode of its resolved target.
            return Ok(candidate);
        }
    }
    Err("pinger capability program not found in PATH".into())
}

fn tsping_help_supports_binding(stdout: &[u8], stderr: &[u8]) -> bool {
    [stdout, stderr].iter().any(|bytes| {
        std::str::from_utf8(bytes).is_ok_and(|text| {
            [
                "--interface",
                "--machine-readable",
                "--print-timestamps",
                "--target-spacing",
                "--sleep-time",
                "--fw-mark",
            ]
            .iter()
            .all(|option| text.contains(option))
        })
    })
}

#[cfg(test)]
mod binding_tests {
    #[test]
    fn r6_explicit_capability_is_fixed_and_does_not_admit_network_probes() {
        let mut cfg = super::Config::defaults("capability".into());
        cfg.route_mode = "explicit".into();
        cfg.ul_if = "wan".into();
        cfg.mwan3_member.clear();
        cfg.ping_prefix_string.clear();
        cfg.explicit_route_authority = crate::routing::ExplicitRouteAuthority::from_fields(
            "explicit",
            ["192.0.2.2", "101", "0x100", "0x3f00"],
        )
        .unwrap();
        for (method, argument) in [("ping", "-V"), ("tsping", "--help")] {
            cfg.pinger_method = method.into();
            let command = super::capability_command(&cfg).unwrap();
            assert_eq!(command.get_program(), method);
            assert_eq!(command.get_args().collect::<Vec<_>>(), [argument]);
            assert!(super::pinger_command(&cfg, method, None).is_err());
        }
        cfg.pinger_method = "ping".into();
        cfg.ping_prefix_string = "env".into();
        assert!(super::capability_command(&cfg).is_err());
        cfg.ping_prefix_string.clear();
        cfg.mwan3_member = "other".into();
        assert!(super::capability_command(&cfg).is_err());
        cfg.mwan3_member.clear();
        cfg.explicit_route_authority = None;
        assert!(super::capability_command(&cfg).is_err());
    }
    use super::*;
    #[test]
    fn capability_path_resolves_ping_tsping_and_route_wrapper_without_losing_applet_name() {
        let root = std::env::temp_dir().join(format!("cake-pinger-path-{}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let executable = std::fs::canonicalize("/bin/sh").unwrap();
        for name in ["ping", "tsping", "mwan3"] {
            let alias = root.join(name);
            std::os::unix::fs::symlink(&executable, &alias).unwrap();
            let resolved = capability_program(OsStr::new(name), Some(root.as_os_str())).unwrap();
            assert_eq!(resolved, alias);
            let spec = SpawnSpec {
                program: resolved,
                arguments: vec!["-c".into(), "printf capability-ok".into()],
                environment: vec![],
            };
            spec.validate().unwrap();
            let output =
                run_bounded_command_output(&spec, Duration::from_secs(2), 8192, || false).unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, b"capability-ok");
        }
        assert!(capability_program(OsStr::new("missing"), Some(root.as_os_str())).is_err());
        assert!(capability_program(OsStr::new("../ping"), Some(root.as_os_str())).is_err());
        assert_eq!(
            capability_program(OsStr::new("/bin/sh"), None).unwrap(),
            Path::new("/bin/sh")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn r6_tsping_requires_its_own_documented_interface_option() {
        let help = b"--interface=INTERFACE --machine-readable --print-timestamps --target-spacing --sleep-time --fw-mark";
        assert!(tsping_help_supports_binding(help, b""));
        assert!(!tsping_help_supports_binding(
            b"-I INTERFACE --print-timestamps",
            b""
        ));
        assert!(!tsping_help_supports_binding(b"\xff", b""));
    }
}
