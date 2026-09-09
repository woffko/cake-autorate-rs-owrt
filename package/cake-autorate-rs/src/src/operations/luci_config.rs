//! Small root-owned LuCI configuration transactions that must commit
//! immediately without driving a service Apply lifecycle.

use super::process::{run_bounded_command_output_with_input, BoundedCommandOutput, SpawnSpec};
use super::service_config::PrivateUciSavedir;
use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
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
    ui_config: PathBuf,
}

impl Environment {
    fn live() -> Self {
        Self {
            ui_config: env::var_os("CAKE_AUTORATE_UI_CONFIG_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/etc/config/cake-autorate-ui")),
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
    workspace: &UiWorkspace,
    arguments: &[String],
) -> Result<BoundedCommandOutput, String> {
    run_bounded_command_output_with_input(
        &SpawnSpec {
            program: environment.uci.clone(),
            arguments: workspace.arguments(arguments),
            environment: Vec::new(),
        },
        None,
        COMMAND_TIMEOUT,
        COMMAND_OUTPUT_LIMIT,
        || false,
        |command| unsafe {
            command.pre_exec(|| {
                libc::umask(0o077);
                Ok(())
            });
        },
    )
}

fn require_uci(
    environment: &Environment,
    workspace: &UiWorkspace,
    arguments: &[String],
) -> Result<(), String> {
    let output = uci_output(environment, workspace, arguments)?;
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

// Libuci can read several delta paths even with a private -t savedir. A unique
// package alias prevents deltas for the real UI package from being replayed.
struct UiWorkspace {
    config: PrivateUciSavedir,
    delta: PrivateUciSavedir,
    overrides: PrivateUciSavedir,
    alias: String,
}

impl UiWorkspace {
    fn new(original: &[u8]) -> Result<Self, String> {
        let config = PrivateUciSavedir::create()?;
        let delta = PrivateUciSavedir::create()?;
        let overrides = PrivateUciSavedir::create()?;
        let name = config
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .replace('.', "_");
        let alias = format!("cake_ui_{name}");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(config.path().join(&alias))
            .map_err(|error| error.to_string())?;
        file.write_all(original)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            config,
            delta,
            overrides,
            alias,
        })
    }

    fn arguments(&self, arguments: &[String]) -> Vec<OsString> {
        let mut result = self.delta.arguments([
            OsString::from("-c"),
            self.config.path().as_os_str().to_owned(),
            OsString::from("-C"),
            self.overrides.path().as_os_str().to_owned(),
        ]);
        result.extend(
            arguments
                .iter()
                .map(|arg| OsString::from(arg.replacen("cake-autorate-ui", &self.alias, 1))),
        );
        result
    }
}

fn read_ui_config(file: &mut File) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    file.take((COMMAND_OUTPUT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > COMMAND_OUTPUT_LIMIT {
        return Err("UI config exceeds its size limit".to_string());
    }
    Ok(bytes)
}

fn publish_ui_config(
    path: &Path,
    original: &[u8],
    metadata: &fs::Metadata,
    candidate: &[u8],
    alias: &str,
) -> Result<(), String> {
    let temp = path.with_file_name(format!(".{alias}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(metadata.mode() & 0o777)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temp)
        .map_err(|error| error.to_string())?;
    let result = (|| {
        file.set_permissions(fs::Permissions::from_mode(metadata.mode() & 0o777))
            .map_err(|error| error.to_string())?;
        file.write_all(candidate)
            .map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        let live = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if !live.is_file()
            || live.len() > COMMAND_OUTPUT_LIMIT as u64
            || live.dev() != metadata.dev()
            || live.ino() != metadata.ino()
            || fs::read(path).map_err(|error| error.to_string())? != original
        {
            return Err("UI config changed during preference save".to_string());
        }
        fs::rename(&temp, path).map_err(|error| error.to_string())?;
        File::open(path.parent().ok_or("UI config has no parent")?)
            .and_then(|parent| parent.sync_all())
            .map_err(|error| error.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
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
    let mut original_file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&environment.ui_config)
        .map_err(|error| format!("unable to open UI preferences: {error}"))?;
    let metadata = original_file
        .metadata()
        .map_err(|error| error.to_string())?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o022 != 0
    {
        return Err("UI config must be an owner-controlled regular file".to_string());
    }
    if unsafe { libc::flock(original_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("UI config is busy with another writer".to_string());
    }
    let original = read_ui_config(&mut original_file)?;
    let workspace = UiWorkspace::new(&original)?;
    // Only UI preferences belong to this transaction. In particular, neither
    // runtime configuration nor any caller's staged deltas may be committed.
    let columns = if mode == "set" {
        selected.join(" ")
    } else {
        String::new()
    };
    for assignment in [
        "cake-autorate-ui.globals=globals".to_string(),
        format!("cake-autorate-ui.globals.status_columns={columns}"),
        "cake-autorate-ui.globals.status_columns_set=1".to_string(),
    ] {
        require_uci(environment, &workspace, &["set".to_string(), assignment])?;
    }
    require_uci(
        environment,
        &workspace,
        &["commit".to_string(), "cake-autorate-ui".to_string()],
    )?;
    workspace.delta.require_clean()?;
    let mut file = File::open(workspace.config.path().join(&workspace.alias))
        .map_err(|error| error.to_string())?;
    let candidate = read_ui_config(&mut file)?;
    publish_ui_config(
        &environment.ui_config,
        &original,
        &metadata,
        &candidate,
        &workspace.alias,
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
        fn new(_section_exists: bool) -> Self {
            let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root =
                env::temp_dir().join(format!("cake-luci-config-{}-{id}", std::process::id()));
            fs::create_dir(&root).unwrap();
            let uci = root.join("uci");
            let log = root.join("uci.log");
            let ui_config = root.join("ui-config");
            fs::write(&ui_config, "config globals 'globals'\n").unwrap();
            fs::write(
                &uci,
                format!(
                    "#!/bin/sh\n[ \"$1\" = -t ] || exit 8\n[ -d \"$2\" ] || exit 8\nshift 7\nprintf '%s\\n' \"$*\" | sed 's/cake_ui_[0-9_]*\\./cake-autorate-ui./g;s/cake_ui_[0-9_]*$/cake-autorate-ui/' >> '{}'\nexit 0\n",
                    log.display(),
                ),
            )
            .unwrap();
            fs::set_permissions(&uci, fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                environment: Environment {
                    uci,
                    guard: root.join("guard"),
                    ui_config,
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
    fn status_preferences_never_commit_the_runtime_package() {
        let fixture = Fixture::new(true);
        status_columns(
            ["set".to_string(), "cpu".to_string()].into_iter(),
            &fixture.environment,
        )
        .unwrap();
        let log = fs::read_to_string(&fixture.log).unwrap();
        assert!(!log.lines().any(|line| line == "commit cake-autorate"));
        assert!(log.contains("commit cake-autorate-ui"));
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
                "set cake-autorate-ui.globals=globals\n",
                "set cake-autorate-ui.globals.status_columns=route cpu\n",
                "set cake-autorate-ui.globals.status_columns_set=1\n",
                "commit cake-autorate-ui\n",
            )
        );
    }

    #[test]
    fn reset_records_explicit_defaults_without_touching_runtime_globals() {
        let fixture = Fixture::new(true);
        status_columns(["reset".to_string()].into_iter(), &fixture.environment).unwrap();
        assert_eq!(
            fs::read_to_string(&fixture.log).unwrap(),
            concat!(
                "set cake-autorate-ui.globals=globals\n",
                "set cake-autorate-ui.globals.status_columns=\n",
                "set cake-autorate-ui.globals.status_columns_set=1\n",
                "commit cake-autorate-ui\n",
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
                "#!/bin/sh\nshift 7\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\nset*'.globals.status_columns='*) exit 9 ;;\nesac\nexit 0\n",
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
