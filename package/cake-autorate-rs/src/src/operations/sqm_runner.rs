//! Isolated adaptation of the inspected upstream SQM runner. Only its UCI
//! package argument changes; a descriptor-lease check precedes the upstream body.
//! Target filtering, global run lock, option mapping
//! and helper invocation remain upstream code. Caller owns runtime authority.
#[cfg(test)]
use super::committed_uci::PreparedConfig;
use super::committed_uci::{file_identity, read_file, Directory, Identity};
use super::process::{run_bounded_command_output_with_input, BoundedCommandOutput, SpawnSpec};
use super::runtime_health::safe_interface;
use crate::config_candidate::digest;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

const SUPPORTED_SHA256: &str = "be3f99376bc1abb824b8dcc411b0484440b4c644f5fe40153a1151113578ce81";
// Reviewed variant adds only RUN_IFACE and section IFACE name validation.
const GUARDED_SHA256: &str = "1fffb1bec2109b1789b6faaef7fcd9753fd9947ca602c955ddaf8c8f0cbecf04";
const MAX_SCRIPT: u64 = 64 * 1024;
const MAGIC: &[u8] = b"#!/bin/sh\n# cake-autorate isolated sqm runner v1\n";
const NEEDLE: &str = "    config_load sqm\n";
pub(crate) const BUSY: &str = "isolated-sqm-runner-busy";
type Result<T> = std::result::Result<T, String>;
fn io<T>(result: std::io::Result<T>) -> Result<T> {
    result.map_err(|_| "isolated-sqm-runner-io".into())
}

#[derive(Clone)]
pub(crate) struct Profile {
    path: PathBuf,
    bytes: Vec<u8>,
    identity: Identity,
}
impl Profile {
    /// Read-only preflight before stopping a healthy service or publishing UCI.
    pub(crate) fn inspect(path: &Path) -> Result<Self> {
        Self::inspect_with_digests(path, &[SUPPORTED_SHA256, GUARDED_SHA256])
    }
    #[cfg(test)]
    pub(super) fn fixture(path: &Path, expected: &str) -> Result<Self> {
        Self::inspect_with_digests(path, &[expected])
    }
    fn inspect_with_digests(path: &Path, expected: &[&str]) -> Result<Self> {
        if !path.is_absolute() {
            return Err("isolated-sqm-runner-path-invalid".into());
        }
        let (bytes, file, identity) = read_file(path, MAX_SCRIPT, false)?;
        if identity.mode & 0o111 == 0
            || io(file.metadata())?.nlink() != 1
            || !expected.contains(&digest(&bytes).as_str())
        {
            return Err("isolated-sqm-runner-unsupported".into());
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| "isolated-sqm-runner-unsupported")?;
        if !text.starts_with("#!/bin/sh\n") || text.matches(NEEDLE).count() != 1 {
            return Err("isolated-sqm-runner-unsupported".into());
        }
        Ok(Self {
            path: path.into(),
            bytes,
            identity,
        })
    }
    fn attest(&self) -> Result<()> {
        let (bytes, file, identity) = read_file(&self.path, MAX_SCRIPT, false)?;
        if bytes != self.bytes || identity != self.identity || io(file.metadata())?.nlink() != 1 {
            return Err("isolated-sqm-runner-source-changed".into());
        }
        Ok(())
    }
    pub(super) fn bind<'a>(
        self,
        config: &'a dyn super::committed_uci::SqmAliasConfig,
        runtime_root: &Path,
    ) -> Result<Runner<'a>> {
        if !runtime_root.is_absolute() {
            return Err("isolated-sqm-runner-path-invalid".into());
        }
        self.attest()?;
        let (directory, alias) = config.sqm_runner_alias()?;
        if alias.len() != 32
            || !alias.starts_with("cu")
            || !alias[2..]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("isolated-sqm-runner-alias-invalid".into());
        }
        // Upstream uci.sh expands UCI_CONFIG_DIR unquoted. Do not permit its
        // whitespace/glob expansion to turn a private directory into argv.
        if !directory.is_absolute()
            || directory
                .as_os_str()
                .as_bytes()
                .iter()
                .any(|b| b.is_ascii_whitespace() || b"*?[]".contains(b))
        {
            return Err("isolated-sqm-runner-config-directory-unsupported".into());
        }
        let parent = Directory::open(runtime_root, false, false)?;
        let workspace = Directory::open(&runtime_root.join(".sqm-start-runner"), true, true)?;
        let lock = io(OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(workspace.path.join("lock")))?;
        if file_identity(&workspace.path.join("lock"), &lock, true)?.len != 0 {
            return Err("isolated-sqm-runner-lock-invalid".into());
        }
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(BUSY.into());
        }
        inspect_workspace(&workspace)?;
        clean_copy(&workspace)?;
        let text =
            std::str::from_utf8(&self.bytes).map_err(|_| "isolated-sqm-runner-unsupported")?;
        let text = text
            .strip_prefix("#!/bin/sh\n")
            .ok_or("isolated-sqm-runner-unsupported")?;
        let mut bytes = MAGIC.to_vec();
        fn quote(path: &Path) -> Result<String> {
            let text = path.to_str().ok_or("isolated-sqm-runner-path-invalid")?;
            Ok(format!("'{}'", text.replace('\'', "'\\''")))
        }
        let alias_lock = directory
            .parent()
            .ok_or("isolated-sqm-runner-path-invalid")?
            .join(".lock");
        bytes.extend(
            format!(
                "[ /proc/$$/fd/6 -ef {} ] && [ /proc/$$/fd/7 -ef {} ] || exit 96\n",
                quote(&alias_lock)?,
                quote(&workspace.path.join("lock"))?
            )
            .as_bytes(),
        );
        bytes.extend(
            text.replacen(
                NEEDLE,
                &format!("    LOAD_STATE= CONFIG_APPEND= config_load '{alias}'\n"),
                1,
            )
            .as_bytes(),
        );
        let mut file = io(OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .custom_flags(libc::O_NOFOLLOW)
            .open(workspace.path.join("current")))?;
        io(file.write_all(&bytes))?;
        io(file.sync_all())?;
        let identity = file_identity(&workspace.path.join("current"), &file, true)?;
        // Linux refuses exec of a file still open for writing (ETXTBSY).
        // Retain a read-only identity handle once materialization is complete.
        drop(file);
        let (_, file, reopened) = read_file(
            &workspace.path.join("current"),
            MAX_SCRIPT + MAGIC.len() as u64,
            true,
        )?;
        if reopened != identity {
            return Err("isolated-sqm-runner-copy-changed".into());
        }
        let runner = Runner {
            profile: self,
            config,
            parent,
            workspace,
            lock,
            file,
            identity,
            sha256: digest(&bytes),
        };
        runner.attest()?;
        Ok(runner)
    }
}

fn inspect_workspace(directory: &Directory) -> Result<()> {
    for name in directory.names()? {
        if name != "lock" && name != "current" {
            return Err("isolated-sqm-runner-foreign-entry".into());
        }
    }
    directory.attest()
}
fn clean_copy(directory: &Directory) -> Result<()> {
    let path = directory.path.join("current");
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("isolated-sqm-runner-io".into()),
        Ok(_) => {}
    }
    let (bytes, file, identity) = read_file(&path, MAX_SCRIPT + MAGIC.len() as u64, true)?;
    if !bytes.starts_with(MAGIC) && !MAGIC.starts_with(&bytes) {
        return Err("isolated-sqm-runner-unowned-artifact".into());
    }
    directory.attest()?;
    if file_identity(&path, &file, true)? != identity {
        return Err("isolated-sqm-runner-copy-changed".into());
    }
    io(fs::remove_file(path))
}

/// Read-only fence for a lifecycle coordinator BEFORE file-transaction recovery.
/// A helper can outlive its supervisor while still owning the inherited lease.
pub(crate) fn ensure_idle(runtime_root: &Path) -> Result<()> {
    if !runtime_root.is_absolute() {
        return Err("isolated-sqm-runner-path-invalid".into());
    }
    let path = runtime_root.join(".sqm-start-runner");
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("isolated-sqm-runner-io".into()),
        Ok(_) => {}
    }
    let directory = Directory::open(&path, true, false)?;
    inspect_workspace(&directory)?;
    if directory.names()?.is_empty() {
        return Ok(());
    }
    let (_, lock, _) = read_file(&path.join("lock"), 0, true)?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(BUSY.into());
    }
    if directory.names()?.iter().any(|name| name == "current") {
        let (bytes, _, _) =
            read_file(&path.join("current"), MAX_SCRIPT + MAGIC.len() as u64, true)?;
        if !bytes.starts_with(MAGIC) && !MAGIC.starts_with(&bytes) {
            return Err("isolated-sqm-runner-unowned-artifact".into());
        }
    }
    directory.attest()
}

pub(crate) struct Runner<'a> {
    profile: Profile,
    config: &'a dyn super::committed_uci::SqmAliasConfig,
    parent: Directory,
    workspace: Directory,
    lock: File,
    file: File,
    identity: Identity,
    sha256: String,
}
impl Runner<'_> {
    pub(crate) fn attest(&self) -> Result<()> {
        self.profile.attest()?;
        self.config.sqm_runner_alias()?;
        self.parent.attest()?;
        inspect_workspace(&self.workspace)?;
        file_identity(&self.workspace.path.join("lock"), &self.lock, true)?;
        let (bytes, _, identity) = read_file(
            &self.workspace.path.join("current"),
            MAX_SCRIPT + MAGIC.len() as u64,
            true,
        )?;
        if identity != self.identity
            || digest(&bytes) != self.sha256
            || file_identity(&self.workspace.path.join("current"), &self.file, true)?
                != self.identity
        {
            return Err("isolated-sqm-runner-copy-changed".into());
        }
        Ok(())
    }
    fn command(&self, action: &str, target: &str) -> Result<SpawnSpec> {
        if !matches!(action, "start" | "stop") || !safe_interface(target) {
            return Err("isolated-sqm-runner-target-invalid".into());
        }
        self.attest()?;
        let (directory, _) = self.config.sqm_runner_alias()?;
        Ok(SpawnSpec {
            program: self.workspace.path.join("current"),
            arguments: vec![action.into(), target.into()],
            environment: vec![
                ("UCI_CONFIG_DIR".into(), directory.as_os_str().into()),
                ("LOAD_STATE".into(), "".into()),
                ("CONFIG_APPEND".into(), "".into()),
            ],
        })
    }
    /// Scoped actions only. Caller must validate owner/runtime state
    /// before invocation and check the exact kernel/state postcondition after it.
    pub(crate) fn start(
        &self,
        target: &str,
        attest_authority: impl FnMut() -> Result<()>,
        should_cancel: impl Fn() -> bool,
    ) -> Result<BoundedCommandOutput> {
        self.action("start", target, attest_authority, should_cancel)
    }
    pub(crate) fn stop(
        &self,
        target: &str,
        attest_authority: impl FnMut() -> Result<()>,
        should_cancel: impl Fn() -> bool,
    ) -> Result<BoundedCommandOutput> {
        self.action("stop", target, attest_authority, should_cancel)
    }
    fn action(
        &self,
        action: &str,
        target: &str,
        mut attest_authority: impl FnMut() -> Result<()>,
        should_cancel: impl Fn() -> bool,
    ) -> Result<BoundedCommandOutput> {
        attest_authority()?;
        let command = self.command(action, target)?;
        // Keep BOTH private namespaces locked if the supervising Rust process
        // exits before the SQM process tree. No live helper may lose its alias
        // or executable to a later operation's crash-artifact cleanup.
        let alias_lock = self.config.retain_alias_lease()?;
        fn high_fd(file: &File) -> Result<File> {
            let fd = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 64) };
            if fd < 0 {
                return Err("isolated-sqm-runner-lease-dup-failed".into());
            }
            Ok(unsafe { File::from_raw_fd(fd) })
        }
        let alias_lock = high_fd(&alias_lock)?;
        let runner_lock = high_fd(&self.lock)?;
        let result = run_bounded_command_output_with_input(
            &command,
            None,
            Duration::from_secs(30),
            16 * 1024,
            &should_cancel,
            |command| unsafe {
                command.pre_exec(move || {
                    if libc::dup2(alias_lock.as_raw_fd(), 6) < 0
                        || libc::dup2(runner_lock.as_raw_fd(), 7) < 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            },
        )
        .map_err(|error| {
            if error == "bounded-command-cancelled" {
                "isolated-sqm-runner-cancelled"
            } else {
                "isolated-sqm-runner-command-failed"
            }
        })?;
        self.attest()?;
        attest_authority()?;
        Ok(result)
    }
}
impl Drop for Runner<'_> {
    fn drop(&mut self) {
        let path = self.workspace.path.join("current");
        if self.workspace.attest().is_ok()
            && file_identity(&path, &self.file, true).ok().as_ref() == Some(&self.identity)
        {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::committed_uci::CommittedSnapshot;
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::fs::{symlink, DirBuilderExt, PermissionsExt};
    use std::time::{SystemTime, UNIX_EPOCH};
    const FIXTURE_RUNNER: &[u8] = b"#!/bin/sh\nconfig_load() { [ \"$1\" != sqm ] || exit 91; [ -f \"$UCI_CONFIG_DIR/$1\" ] || exit 92; printf '%s\\n' \"$1\"; }\n    config_load sqm\nprintf '%s|%s\\n' \"$1\" \"$2\"\n";
    struct Fixture {
        root: PathBuf,
        config: PathBuf,
        run: PathBuf,
        source: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root =
                std::env::temp_dir().join(format!("cake-r4-runner-{}-{nonce}", std::process::id()));
            fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            let config = root.join("config");
            let run = root.join("run");
            for path in [&config, &run] {
                fs::create_dir(path).unwrap();
            }
            for name in ["cake-autorate", "sqm"] {
                fs::write(config.join(name), "config queue 'lab'\n").unwrap();
            }
            let source = root.join("upstream-runner");
            fs::write(&source, FIXTURE_RUNNER).unwrap();
            fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                root,
                config,
                run,
                source,
            }
        }
        fn config(&self) -> PreparedConfig {
            fn query(args: Vec<OsString>) -> Result<Vec<u8>> {
                Ok(format!("{}.lab=queue\n", args[9].to_str().unwrap()).into_bytes())
            }
            CommittedSnapshot::capture_fixture(&self.config, &self.run, query)
                .unwrap()
                .prepare_fixture([&[], &[]], query)
                .unwrap()
        }
        fn profile(&self) -> Profile {
            Profile::inspect_with_digests(&self.source, &[&digest(FIXTURE_RUNNER)]).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn r4_sqm_runner_uses_frozen_alias_scoped_target_and_inherited_namespace_leases() {
        let fixture = Fixture::new();
        let config = fixture.config();
        let runner = fixture.profile().bind(&config, &fixture.run).unwrap();
        let mut checks = 0;
        let result = runner
            .start(
                "fixture0",
                || {
                    checks += 1;
                    Ok(())
                },
                || false,
            )
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(checks, 2);
        let (_, alias) = config.sqm_runner_alias().unwrap();
        assert_eq!(
            result.stdout,
            format!("{alias}\nstart|fixture0\n").as_bytes()
        );
        assert_eq!(fs::read(&fixture.source).unwrap(), FIXTURE_RUNNER);
        assert!(runner.command("start", "../other").is_err());
        assert!(runner.command("restart", "eth0").is_err());
        let stopped = runner.stop("fixture0", || Ok(()), || false).unwrap();
        assert!(stopped.status.success());
        assert_eq!(
            stopped.stdout,
            format!("{alias}\nstop|fixture0\n").as_bytes()
        );
        drop(runner);
        assert!(!fixture.run.join(".sqm-start-runner/current").exists());
        config.attest().unwrap();
    }

    #[test]
    fn r4_sqm_runner_rejects_unknown_profile_or_changed_source_before_execution() {
        let fixture = Fixture::new();
        assert_eq!(
            Profile::inspect(&fixture.source).err().unwrap(),
            "isolated-sqm-runner-unsupported"
        );
        let config = fixture.config();
        let profile = fixture.profile();
        OpenOptions::new()
            .append(true)
            .open(&fixture.source)
            .unwrap()
            .write_all(b"# changed\n")
            .unwrap();
        assert!(profile.bind(&config, &fixture.run).is_err());
        assert!(!fixture.run.join(".sqm-start-runner").exists());
    }

    #[test]
    fn r4_sqm_runner_refuses_busy_workspace_cancellation_and_authority_drift() {
        let fixture = Fixture::new();
        let config = fixture.config();
        let runner = fixture.profile().bind(&config, &fixture.run).unwrap();
        assert_eq!(
            fixture.profile().bind(&config, &fixture.run).err().unwrap(),
            BUSY
        );
        assert_eq!(ensure_idle(&fixture.run).err().unwrap(), BUSY);
        assert_eq!(
            runner.start("fixture0", || Ok(()), || true).err().unwrap(),
            "isolated-sqm-runner-cancelled"
        );
        assert_eq!(
            runner
                .start("fixture0", || Err("source-drift".into()), || false)
                .err()
                .unwrap(),
            "source-drift"
        );
        runner.attest().unwrap();
    }

    #[test]
    fn r4_sqm_runner_child_retains_leases_after_supervisor_exit_until_it_finishes() {
        use std::os::unix::process::ExitStatusExt;
        let fixture = Fixture::new();
        let fifo = fixture.root.join("continue");
        let c_path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);
        let mut gate = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fifo)
            .unwrap();
        let script = format!("#!/bin/sh\nconfig_load() {{ :; }}\n    config_load sqm\nprintf '%s\\n' \"$$\" > '{}/child-pid'\nkill -KILL \"$PPID\"\nread -r finish < '{}'\n", fixture.root.display(), fifo.display());
        fs::write(&fixture.source, script).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "operations::sqm_runner::tests::lease_parent_exit_fixture",
                "--ignored",
                "--test-threads=1",
            ])
            .env("CAKE_SQM_LEASE_FIXTURE", &fixture.root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        let pid = fs::read_to_string(fixture.root.join("child-pid"))
            .unwrap()
            .trim()
            .parse::<libc::pid_t>()
            .unwrap();
        let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        assert!(pidfd >= 0);
        let pidfd = unsafe { File::from_raw_fd(pidfd as i32) };
        let busy = ensure_idle(&fixture.run);
        let aliases = CommittedSnapshot::capture_fixture(&fixture.config, &fixture.run, |_| {
            panic!("live alias lease must reject before query")
        });
        // Always release the controlled helper before asserting failures.
        gate.write_all(b"finish\n").unwrap();
        let mut poll = libc::pollfd {
            fd: pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut poll, 1, 2000) }, 1);
        assert_eq!(busy.err().unwrap(), BUSY);
        assert_eq!(aliases.err().unwrap(), "committed-uci-workspace-busy");
        ensure_idle(&fixture.run).unwrap();
        let config = fixture.config();
        let profile = Profile::fixture(
            &fixture.source,
            &digest(&fs::read(&fixture.source).unwrap()),
        )
        .unwrap();
        drop(profile.bind(&config, &fixture.run).unwrap());
        assert!(!fixture.run.join(".sqm-start-runner/current").exists());
    }

    #[test]
    #[ignore = "private subprocess fixture invoked only by its bounded parent"]
    fn lease_parent_exit_fixture() {
        let root = PathBuf::from(
            std::env::var_os("CAKE_SQM_LEASE_FIXTURE")
                .expect("explicit controlled fixture required"),
        );
        assert!(root.starts_with(std::env::temp_dir()));
        assert!(root
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("cake-r4-runner-"));
        let fixture = Fixture {
            config: root.join("config"),
            run: root.join("run"),
            source: root.join("upstream-runner"),
            root,
        };
        let config = fixture.config();
        let profile = Profile::fixture(
            &fixture.source,
            &digest(&fs::read(&fixture.source).unwrap()),
        )
        .unwrap();
        let runner = profile.bind(&config, &fixture.run).unwrap();
        let _ = runner.start("fixture0", || Ok(()), || false);
        panic!("fixture supervisor should have been killed by its controlled helper");
    }

    #[test]
    fn r4_sqm_runner_never_unlinks_foreign_or_linked_artifacts() {
        for case in 0..3 {
            let fixture = Fixture::new();
            let config = fixture.config();
            let runner = fixture.profile().bind(&config, &fixture.run).unwrap();
            let current = runner.workspace.path.join("current");
            let foreign = fixture.root.join("foreign");
            fs::write(&foreign, b"foreign data").unwrap();
            match case {
                0 => {
                    fs::remove_file(&current).unwrap();
                    symlink(&foreign, &current).unwrap();
                }
                1 => {
                    fs::hard_link(&current, fixture.root.join("linked")).unwrap();
                }
                _ => {
                    fs::write(&current, b"foreign data").unwrap();
                }
            }
            assert!(runner.attest().is_err());
            drop(runner);
            assert!(current.symlink_metadata().is_ok());
            assert!(fixture.profile().bind(&config, &fixture.run).is_err());
            assert_eq!(fs::read(foreign).unwrap(), b"foreign data");
        }
    }

    #[test]
    #[ignore = "requires explicitly inspected SDK runner, uci.sh, UCI and loader paths"]
    fn r4_sqm_runner_real_sdk_loads_alias_ignoring_original_deltas_and_keeps_upstream_scoping() {
        real_sdk_runner(false);
    }

    #[test]
    #[ignore = "requires explicitly inspected SDK runner, uci.sh, UCI and loader paths"]
    fn r4_sqm_runner_real_sdk_guarded_variant_loads_alias_and_keeps_upstream_scoping() {
        real_sdk_runner(true);
    }

    fn real_sdk_runner(guarded: bool) {
        let fixture = Fixture::new();
        let original = PathBuf::from(
            std::env::var_os("CAKE_TEST_SQM_RUNNER").expect("explicit SDK runner required"),
        );
        let mut known = Profile::inspect(&original).unwrap();
        if guarded {
            let text = String::from_utf8(known.bytes.clone()).unwrap();
            assert_eq!(digest(text.as_bytes()), SUPPORTED_SHA256);
            let text = text.replace(
                "stop_statefile() {",
                "if [ -n \"$RUN_IFACE\" ] && ! valid_interface_name \"$RUN_IFACE\"; then\n    sqm_error \"Invalid SQM interface name: $RUN_IFACE\"\n    exit 1\nfi\n\nstop_statefile() {",
            ).replace(
                "    [ -z \"$RUN_IFACE\" -o \"$RUN_IFACE\" = \"$IFACE\" ] || return",
                "    if ! valid_interface_name \"$IFACE\"; then\n        sqm_error \"Invalid SQM interface name in section $section: $IFACE\"\n        return 1\n    fi\n\n    [ -z \"$RUN_IFACE\" -o \"$RUN_IFACE\" = \"$IFACE\" ] || return",
            );
            assert_eq!(digest(text.as_bytes()), GUARDED_SHA256);
            let guarded_path = fixture.root.join("guarded-runner");
            fs::write(&guarded_path, text).unwrap();
            fs::set_permissions(&guarded_path, fs::Permissions::from_mode(0o700)).unwrap();
            known = Profile::inspect(&guarded_path).unwrap();
        }
        let uci_sh = PathBuf::from(
            std::env::var_os("CAKE_TEST_UCI_SH").expect("explicit SDK uci.sh required"),
        );
        let global = fixture.root.join("global");
        let rpcd = fixture.root.join("rpcd");
        let lib = fixture.root.join("lib");
        let state = fixture.root.join("state");
        let available = fixture.root.join("available");
        for path in [&global, &rpcd, &lib, &state, &available] {
            fs::create_dir(path).unwrap();
        }
        for path in [&global, &rpcd] {
            fs::write(
                path.join("sqm"),
                "sqm.lab.interface='poison0'\nsqm.lab.download='1'\n",
            )
            .unwrap();
        }
        fs::write(fixture.config.join("sqm"), "config queue 'lab'\n option enabled '1'\n option interface 'fixture0'\n option upload '12345'\n option download '45678'\n option qdisc 'cake'\n option script 'piece_of_cake.qos'\n option use_mq '0'\nconfig queue 'foreign'\n option enabled '1'\n option interface 'foreign0'\n option upload '99'\n option download '88'\n").unwrap();
        let binary = std::env::var_os("CAKE_TEST_UCI").expect("explicit SDK UCI required");
        let loader = std::env::var_os("CAKE_TEST_MUSL_LOADER");
        let mut command = SpawnSpec {
            program: loader.clone().unwrap_or_else(|| binary.clone()).into(),
            arguments: vec![],
            environment: vec![],
        };
        if loader.is_some() {
            command.arguments.extend([
                "--library-path".into(),
                std::env::var_os("CAKE_TEST_LIB_DIR").expect("explicit libraries required"),
                binary,
            ]);
        }
        command.arguments.extend([
            "-p".into(),
            global.as_os_str().into(),
            "-p".into(),
            rpcd.as_os_str().into(),
        ]);
        let query = |args: Vec<OsString>| -> Result<Vec<u8>> {
            let mut query = command.clone();
            query.arguments.extend(args);
            let output = super::super::process::run_bounded_command_output(
                &query,
                Duration::from_secs(10),
                1024 * 1024,
                || false,
            )?;
            if !output.status.success() || !output.stderr.is_empty() {
                return Err("SDK UCI fixture query failed".into());
            }
            Ok(output.stdout)
        };
        let config = CommittedSnapshot::capture_fixture(&fixture.config, &fixture.run, &query)
            .unwrap()
            .prepare_fixture([&[], &[]], &query)
            .unwrap();
        fn quote(path: &std::ffi::OsStr) -> String {
            format!("'{}'", path.to_str().unwrap().replace('\'', "'\\''"))
        }
        let uci = fixture.root.join("sdk-uci");
        let argv = std::iter::once(command.program.as_os_str())
            .chain(command.arguments.iter().map(|s| s.as_os_str()))
            .map(quote)
            .collect::<Vec<_>>()
            .join(" ");
        fs::write(&uci, format!("#!/bin/sh\nexec {argv} \"$@\"\n")).unwrap();
        fs::set_permissions(&uci, fs::Permissions::from_mode(0o700)).unwrap();
        let adapter = fixture.root.join("uci.sh");
        fs::write(
            &adapter,
            fs::read_to_string(&uci_sh)
                .unwrap()
                .replace("/sbin/uci", &quote(uci.as_os_str())),
        )
        .unwrap();
        // Execute the actual OpenWrt callbacks, SDK runner/uci.sh and libuci.
        // Only library locations, host-dash's export -n incompatibility, logging
        // and the final kernel-writing helper are adapted in this host fixture.
        let base = fixture.root.join("base.sh");
        let base_source = PathBuf::from(
            std::env::var_os("CAKE_TEST_BASE_FUNCTIONS")
                .expect("explicit base-files functions required"),
        );
        let callbacks = fs::read_to_string(&base_source)
            .unwrap()
            .replace("/lib/config/uci.sh", &quote(adapter.as_os_str()))
            .replace("NO_EXPORT=1\n", "NO_EXPORT=\n");
        fs::write(&base, callbacks).unwrap();
        fs::write(
            lib.join("functions.sh"),
            "check_state_dir() { :; }\nsqm_trace() { :; }\nsqm_warn() { :; }\nsqm_error() { :; }\nvalid_interface_name() { case \"$1\" in ''|*[!A-Za-z0-9_.-]*) return 1 ;; esac; [ \"${#1}\" -le 15 ]; }\n",
        )
        .unwrap();
        let conf = fixture.root.join("sqm.conf");
        fs::write(
            &conf,
            format!(
                "SQM_STATE_DIR={}\nSQM_QDISC_STATE_DIR={}\nSQM_LIB_DIR={}\n",
                quote(state.as_os_str()),
                quote(available.as_os_str()),
                quote(lib.as_os_str())
            ),
        )
        .unwrap();
        let result_path = fixture.root.join("started");
        fs::write(lib.join("start-sqm"), format!("#!/bin/sh\nprintf '%s|%s|%s|%s|%s|%s\\n' \"$IFACE\" \"$UPLINK\" \"$DOWNLINK\" \"$QDISC\" \"$SCRIPT\" \"$USE_MQ\" >> {}\n", quote(result_path.as_os_str()))).unwrap();
        fs::set_permissions(lib.join("start-sqm"), fs::Permissions::from_mode(0o700)).unwrap();
        let source = fixture.root.join("sdk-runner-rebased");
        let body = String::from_utf8(known.bytes)
            .unwrap()
            .replace(
                ". /lib/functions.sh",
                &format!(". {}", quote(base.as_os_str())),
            )
            .replace(
                ". /etc/sqm/sqm.conf",
                &format!(". {}", quote(conf.as_os_str())),
            );
        fs::write(&source, &body).unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
        let runner = Profile::fixture(&source, &digest(body.as_bytes()))
            .unwrap()
            .bind(&config, &fixture.run)
            .unwrap();
        let result = runner
            .start("fixture0", || config.attest(), || false)
            .unwrap();
        assert_eq!(
            result.status.code(),
            Some(1),
            "upstream last foreign callback is skipped"
        );
        assert_eq!(
            fs::read_to_string(&result_path).expect("selected queue must actually start"),
            "fixture0|12345|45678|cake|piece_of_cake.qos|0\n"
        );
        assert!(!state.join("sqm-run.lock").exists());
        for path in [&global, &rpcd] {
            assert_eq!(
                fs::read_to_string(path.join("sqm")).unwrap(),
                "sqm.lab.interface='poison0'\nsqm.lab.download='1'\n"
            );
        }
        config.attest().unwrap();
    }
}
