//! Small root-owned LuCI configuration transactions that must commit
//! immediately without driving a service Apply lifecycle.

use super::process::{run_bounded_command_output, BoundedCommandOutput, SpawnSpec};
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const COMMAND_OUTPUT_LIMIT: usize = 64 * 1024;
const STATUS_COLUMNS: &[&str] = &[
    "route",
    "updated",
    "reflector",
    "runtime_reflectors",
    "rtt",
    "dl_achieved",
    "ul_achieved",
    "cake_dl",
    "cake_ul",
    "cpu",
];

struct Environment {
    uci: PathBuf,
    guard: PathBuf,
}

impl Environment {
    fn live() -> Self {
        Self {
            uci: env::var_os("CAKE_AUTORATE_UCI_BIN")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/sbin/uci")),
            guard: env::var_os("CAKE_AUTORATE_LUCI_CONFIG_GUARD")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/var/lock/cake-autorate-luci-config.guard")),
        }
    }
}

struct ExclusiveGuard(File);

impl Drop for ExclusiveGuard {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn acquire_guard(path: &Path) -> Result<ExclusiveGuard, String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err("Status-column guard path is unsafe".to_string());
    }
    let parent = path
        .parent()
        .ok_or_else(|| "Status-column guard parent is missing".to_string())?;
    let parent_metadata = fs::symlink_metadata(parent)
        .map_err(|error| format!("unable to inspect Status-column guard parent: {error}"))?;
    if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
        return Err("Status-column guard parent is unsafe".to_string());
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("unable to open Status-column guard: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("unable to inspect Status-column guard: {error}"))?;
    if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err("Status-column guard is not an owned regular file".to_string());
    }
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("unable to protect Status-column guard: {error}"))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = std::io::Error::last_os_error();
        return if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
            Err("another Status-column update is running".to_string())
        } else {
            Err(format!("unable to lock Status-column transaction: {error}"))
        };
    }
    Ok(ExclusiveGuard(file))
}

fn uci_output(
    environment: &Environment,
    arguments: &[String],
) -> Result<BoundedCommandOutput, String> {
    run_bounded_command_output(
        &SpawnSpec {
            program: environment.uci.clone(),
            arguments: arguments.iter().map(OsString::from).collect(),
            environment: Vec::new(),
        },
        COMMAND_TIMEOUT,
        COMMAND_OUTPUT_LIMIT,
        || false,
    )
}

fn require_uci(environment: &Environment, arguments: &[String]) -> Result<(), String> {
    let output = uci_output(environment, arguments)?;
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if detail.is_empty() {
        format!("uci {} failed with {}", arguments.join(" "), output.status)
    } else {
        detail
    })
}

pub(crate) fn run_status_columns<I>(arguments: I) -> Result<(), String>
where
    I: Iterator<Item = String>,
{
    status_columns(arguments, &Environment::live())
}

fn status_columns<I>(mut arguments: I, environment: &Environment) -> Result<(), String>
where
    I: Iterator<Item = String>,
{
    let mode = arguments
        .next()
        .ok_or_else(|| "status-columns requires set or reset".to_string())?;
    if !matches!(mode.as_str(), "set" | "reset") {
        return Err("status-columns requires set or reset".to_string());
    }
    let mut selected = Vec::new();
    for key in arguments {
        if mode == "reset" {
            return Err("status-columns reset accepts no columns".to_string());
        }
        if !STATUS_COLUMNS.contains(&key.as_str()) {
            return Err(format!("invalid Status column: {key}"));
        }
        if !selected.contains(&key) {
            selected.push(key);
        }
    }

    let _guard = acquire_guard(&environment.guard)?;
    let get_section = vec![
        "-q".to_string(),
        "get".to_string(),
        "cake-autorate.globals".to_string(),
    ];
    if !uci_output(environment, &get_section)?.status.success() {
        require_uci(
            environment,
            &[
                "set".to_string(),
                "cake-autorate.globals=globals".to_string(),
            ],
        )?;
    }
    let _ = uci_output(
        environment,
        &[
            "-q".to_string(),
            "delete".to_string(),
            "cake-autorate.globals.status_columns".to_string(),
        ],
    )?;
    if mode == "set" {
        for key in selected {
            require_uci(
                environment,
                &[
                    "add_list".to_string(),
                    format!("cake-autorate.globals.status_columns={key}"),
                ],
            )?;
        }
    }
    require_uci(
        environment,
        &["commit".to_string(), "cake-autorate".to_string()],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

    struct Fixture {
        root: PathBuf,
        environment: Environment,
        log: PathBuf,
    }

    impl Fixture {
        fn new(section_exists: bool) -> Self {
            let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root =
                env::temp_dir().join(format!("cake-luci-config-{}-{id}", std::process::id()));
            fs::create_dir(&root).unwrap();
            let uci = root.join("uci");
            let log = root.join("uci.log");
            fs::write(
                &uci,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n'-q get cake-autorate.globals') exit {} ;;\nesac\nexit 0\n",
                    log.display(),
                    if section_exists { 0 } else { 1 }
                ),
            )
            .unwrap();
            fs::set_permissions(&uci, fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                environment: Environment {
                    uci,
                    guard: root.join("guard"),
                },
                root,
                log,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn invalid_columns_fail_before_lock_or_uci() {
        let fixture = Fixture::new(true);
        assert!(status_columns(
            ["set".to_string(), "not-a-column".to_string()].into_iter(),
            &fixture.environment,
        )
        .is_err());
        assert!(!fixture.environment.guard.exists());
        assert!(!fixture.log.exists());
    }

    #[test]
    fn set_deduplicates_and_commits_the_exact_whitelist() {
        let fixture = Fixture::new(false);
        status_columns(
            [
                "set".to_string(),
                "route".to_string(),
                "cpu".to_string(),
                "route".to_string(),
            ]
            .into_iter(),
            &fixture.environment,
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(&fixture.log).unwrap(),
            concat!(
                "-q get cake-autorate.globals\n",
                "set cake-autorate.globals=globals\n",
                "-q delete cake-autorate.globals.status_columns\n",
                "add_list cake-autorate.globals.status_columns=route\n",
                "add_list cake-autorate.globals.status_columns=cpu\n",
                "commit cake-autorate\n",
            )
        );
    }

    #[test]
    fn reset_removes_the_option_and_commits_without_recreating_globals() {
        let fixture = Fixture::new(true);
        status_columns(["reset".to_string()].into_iter(), &fixture.environment).unwrap();
        assert_eq!(
            fs::read_to_string(&fixture.log).unwrap(),
            concat!(
                "-q get cake-autorate.globals\n",
                "-q delete cake-autorate.globals.status_columns\n",
                "commit cake-autorate\n",
            )
        );
    }

    #[test]
    fn competing_update_is_busy_before_uci() {
        let fixture = Fixture::new(true);
        let owner = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .open(&fixture.environment.guard)
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(owner.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        assert!(status_columns(
            ["set".to_string(), "route".to_string()].into_iter(),
            &fixture.environment,
        )
        .unwrap_err()
        .contains("another Status-column update"));
        assert!(!fixture.log.exists());
    }

    #[test]
    fn failed_option_write_never_reaches_commit() {
        let fixture = Fixture::new(true);
        fs::write(
            &fixture.environment.uci,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n'add_list '*) exit 9 ;;\nesac\nexit 0\n",
                fixture.log.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&fixture.environment.uci, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(status_columns(
            ["set".to_string(), "route".to_string()].into_iter(),
            &fixture.environment,
        )
        .is_err());
        assert!(!fs::read_to_string(&fixture.log)
            .unwrap()
            .contains("commit cake-autorate"));
    }
}
