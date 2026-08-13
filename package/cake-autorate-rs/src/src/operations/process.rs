use super::identity::ProcessIdentity;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const MAX_ARGUMENTS: usize = 64;
const MAX_ARGUMENT_BYTES: usize = 4 * 1024;
const MAX_TOTAL_ARGUMENT_BYTES: usize = 16 * 1024;
const MAX_ENVIRONMENT_ENTRIES: usize = 32;
const MAX_TOTAL_ENVIRONMENT_BYTES: usize = 8 * 1024;
const MAX_STDIN_BYTES: usize = 256 * 1024;
const WAIT_INTERVAL: Duration = Duration::from_millis(20);
const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;

extern "C" {
    fn setsid() -> i32;
    fn kill(pid: i32, signal: i32) -> i32;
}

#[derive(Clone, Debug)]
pub struct SpawnSpec {
    pub program: PathBuf,
    pub arguments: Vec<OsString>,
    pub environment: Vec<(OsString, OsString)>,
}

impl SpawnSpec {
    pub fn validate(&self) -> Result<(), String> {
        validate_program(&self.program)?;
        if self.arguments.len() > MAX_ARGUMENTS {
            return Err("operation worker has too many arguments".to_string());
        }
        let mut argument_bytes = 0usize;
        for argument in &self.arguments {
            let bytes = argument.as_bytes();
            if bytes.len() > MAX_ARGUMENT_BYTES || bytes.contains(&0) {
                return Err("operation worker argument is unsafe or oversized".to_string());
            }
            argument_bytes = argument_bytes.saturating_add(bytes.len());
        }
        if argument_bytes > MAX_TOTAL_ARGUMENT_BYTES {
            return Err("operation worker argument vector exceeds its bound".to_string());
        }
        if self.environment.len() > MAX_ENVIRONMENT_ENTRIES {
            return Err("operation worker has too many environment entries".to_string());
        }
        let mut environment_bytes = 0usize;
        for (name, value) in &self.environment {
            let name = name.as_bytes();
            let value = value.as_bytes();
            if name.is_empty()
                || !name
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                || value.contains(&0)
            {
                return Err("operation worker environment is unsafe".to_string());
            }
            environment_bytes = environment_bytes
                .saturating_add(name.len())
                .saturating_add(value.len());
        }
        if environment_bytes > MAX_TOTAL_ENVIRONMENT_BYTES {
            return Err("operation worker environment exceeds its bound".to_string());
        }
        Ok(())
    }
}

pub struct ManagedChild {
    child: Child,
    pub identity: ProcessIdentity,
    terminate_on_drop: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminationOutcome {
    Exited,
    Terminated,
    Killed,
    IdentityMismatch,
}

#[derive(Debug)]
pub struct BoundedCommandOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug)]
struct BoundedRead {
    bytes: Vec<u8>,
    exceeded_limit: bool,
}

struct OwnedProcessGroupChild {
    child: Child,
    process_group: u32,
    reaped: bool,
}

impl OwnedProcessGroupChild {
    fn try_wait(&mut self) -> Result<Option<ExitStatus>, String> {
        let status = self
            .child
            .try_wait()
            .map_err(|error| format!("unable to inspect bounded command: {error}"))?;
        if status.is_some() {
            self.reaped = true;
        }
        Ok(status)
    }

    fn kill_and_reap(&mut self) -> Result<(), String> {
        if self.reaped {
            return Ok(());
        }
        match self.child.try_wait() {
            Ok(Some(_)) => {
                self.reaped = true;
                return Ok(());
            }
            Ok(None) | Err(_) => {}
        }
        let group = i32::try_from(self.process_group)
            .map_err(|_| "bounded command process group is out of range".to_string())?;
        if group <= 1 {
            return Err("refusing to signal an unsafe bounded command process group".to_string());
        }
        let group_killed = unsafe { kill(-group, SIGKILL) } == 0;
        if !group_killed {
            let _ = self.child.kill();
        }
        self.child
            .wait()
            .map_err(|error| format!("unable to reap bounded command: {error}"))?;
        self.reaped = true;
        Ok(())
    }
}

impl Drop for OwnedProcessGroupChild {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        match self.child.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => {
                if let Ok(group) = i32::try_from(self.process_group) {
                    if group > 1 {
                        let _ = unsafe { kill(-group, SIGKILL) };
                    }
                }
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
        self.reaped = true;
    }
}

impl ManagedChild {
    pub fn spawn(
        spec: &SpawnSpec,
        stdout: File,
        stderr: File,
        proc_root: &Path,
    ) -> Result<Self, String> {
        spec.validate()?;
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.arguments)
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .envs(spec.environment.iter().cloned())
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        unsafe {
            command.pre_exec(|| {
                if setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("unable to launch operation worker: {error}"))?;
        let pid = child.id();
        let identity = match ProcessIdentity::inspect(proc_root, pid) {
            Ok(identity) if identity.process_group == pid => identity,
            Ok(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("operation worker did not enter its own process group".to_string());
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "unable to attest operation worker identity: {error}"
                ));
            }
        };
        Ok(Self {
            child,
            identity,
            terminate_on_drop: true,
        })
    }

    /// Keep an independently restoring child alive if its coordinator exits
    /// unexpectedly.  The caller remains responsible for reaping it while
    /// alive and for an explicit TERM/restore handshake during orderly
    /// shutdown.
    pub fn preserve_on_drop(&mut self) {
        self.terminate_on_drop = false;
    }

    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>, String> {
        self.child
            .try_wait()
            .map_err(|error| format!("unable to inspect operation worker: {error}"))
    }

    pub fn wait_for_exit(&mut self, timeout: Duration) -> Result<Option<ExitStatus>, String> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            thread::sleep(WAIT_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
        }
    }

    pub fn terminate(
        &mut self,
        proc_root: &Path,
        grace: Duration,
    ) -> Result<TerminationOutcome, String> {
        if let Some(_) = self.try_wait()? {
            return Ok(TerminationOutcome::Exited);
        }
        if !self.identity.still_matches(proc_root)? {
            return Ok(TerminationOutcome::IdentityMismatch);
        }
        signal_group(&self.identity, SIGTERM)?;
        if self.wait_until(grace)? {
            return Ok(TerminationOutcome::Terminated);
        }
        if !self.identity.still_matches(proc_root)? {
            let _ = self.child.wait();
            return Ok(TerminationOutcome::Terminated);
        }
        signal_group(&self.identity, SIGKILL)?;
        self.child
            .wait()
            .map_err(|error| format!("unable to reap killed operation worker: {error}"))?;
        Ok(TerminationOutcome::Killed)
    }

    fn wait_until(&mut self, timeout: Duration) -> Result<bool, String> {
        self.wait_for_exit(timeout).map(|status| status.is_some())
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        if !self.terminate_on_drop {
            return;
        }
        let Ok(None) = self.child.try_wait() else {
            return;
        };
        let _ = signal_group(&self.identity, SIGKILL);
        let _ = self.child.wait();
    }
}

pub fn run_bounded_command_output<F>(
    spec: &SpawnSpec,
    timeout: Duration,
    output_limit: usize,
    should_cancel: F,
) -> Result<BoundedCommandOutput, String>
where
    F: Fn() -> bool,
{
    run_bounded_command_output_with_input(spec, None, timeout, output_limit, should_cancel, |_| {})
}

pub fn run_bounded_command_output_with_input<F, C>(
    spec: &SpawnSpec,
    input: Option<&[u8]>,
    timeout: Duration,
    output_limit: usize,
    should_cancel: F,
    configure: C,
) -> Result<BoundedCommandOutput, String>
where
    F: Fn() -> bool,
    C: FnOnce(&mut Command),
{
    spec.validate()?;
    if timeout.is_zero() {
        return Err("bounded command timeout is invalid".to_string());
    }
    if input.is_some_and(|bytes| bytes.len() > MAX_STDIN_BYTES) {
        return Err("bounded command stdin exceeds its size limit".to_string());
    }
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.arguments)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .envs(spec.environment.iter().cloned())
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure(&mut command);
    unsafe {
        command.pre_exec(|| {
            if setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command
        .spawn()
        .map_err(|error| format!("unable to launch bounded command: {error}"))?;
    let process_group = child.id();
    let mut child = OwnedProcessGroupChild {
        child,
        process_group,
        reaped: false,
    };
    let stdin_writer = if let Some(bytes) = input {
        let mut stdin = child
            .child
            .stdin
            .take()
            .ok_or_else(|| "bounded command stdin pipe is missing".to_string())?;
        let bytes = bytes.to_vec();
        Some(
            thread::Builder::new()
                .name("cake-bounded-stdin".to_string())
                .spawn(move || {
                    stdin.write_all(&bytes)?;
                    stdin.flush()
                })
                .map_err(|error| {
                    format!("unable to start bounded command stdin writer: {error}")
                })?,
        )
    } else {
        None
    };
    let stdout = child
        .child
        .stdout
        .take()
        .ok_or_else(|| "bounded command stdout pipe is missing".to_string())?;
    let stderr = child
        .child
        .stderr
        .take()
        .ok_or_else(|| "bounded command stderr pipe is missing".to_string())?;
    let stdout_reader = spawn_bounded_reader("cake-bounded-stdout", stdout, output_limit)?;
    let stderr_reader = match spawn_bounded_reader("cake-bounded-stderr", stderr, output_limit) {
        Ok(reader) => reader,
        Err(error) => {
            let _ = child.kill_and_reap();
            let _ = stdout_reader.join();
            return Err(error);
        }
    };

    let started = Instant::now();
    let deadline = started.checked_add(timeout).unwrap_or(started);
    let mut status = None;
    let mut terminal_error = None;
    loop {
        if should_cancel() {
            terminal_error = Some("bounded-command-cancelled".to_string());
            if let Err(error) = child.kill_and_reap() {
                terminal_error = Some(error);
            }
            break;
        }
        match child.try_wait() {
            Ok(Some(value)) => {
                status = Some(value);
                break;
            }
            Ok(None) => {}
            Err(error) => {
                terminal_error = Some(error);
                if let Err(kill_error) = child.kill_and_reap() {
                    terminal_error = Some(kill_error);
                }
                break;
            }
        }
        let now = Instant::now();
        if now >= deadline {
            terminal_error = Some("bounded-command-timeout".to_string());
            if let Err(error) = child.kill_and_reap() {
                terminal_error = Some(error);
            }
            break;
        }
        thread::sleep(WAIT_INTERVAL.min(deadline.saturating_duration_since(now)));
    }

    let stdout = join_bounded_reader(stdout_reader, "stdout")?;
    let stderr = join_bounded_reader(stderr_reader, "stderr")?;
    let stdin_result = stdin_writer
        .map(|writer| {
            writer
                .join()
                .map_err(|_| "bounded command stdin writer panicked".to_string())?
                .map_err(|error| format!("unable to write bounded command stdin: {error}"))
        })
        .transpose();
    if let Some(error) = terminal_error {
        return Err(error);
    }
    if let Err(error) = stdin_result {
        return Err(error);
    }
    if stdout.exceeded_limit || stderr.exceeded_limit {
        return Err("bounded-command-output-too-large".to_string());
    }
    Ok(BoundedCommandOutput {
        status: status.ok_or_else(|| "bounded command status is missing".to_string())?,
        stdout: stdout.bytes,
        stderr: stderr.bytes,
    })
}

fn spawn_bounded_reader<R>(
    name: &str,
    reader: R,
    output_limit: usize,
) -> Result<thread::JoinHandle<io::Result<BoundedRead>>, String>
where
    R: Read + Send + 'static,
{
    thread::Builder::new()
        .name(name.to_string())
        .spawn(move || drain_bounded(reader, output_limit))
        .map_err(|error| format!("unable to start bounded command reader: {error}"))
}

fn drain_bounded<R: Read>(mut reader: R, output_limit: usize) -> io::Result<BoundedRead> {
    let mut bytes = Vec::with_capacity(output_limit.min(16 * 1024));
    let mut exceeded_limit = false;
    let mut chunk = [0u8; 8192];
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        let remaining = output_limit.saturating_sub(bytes.len());
        let retained = remaining.min(read);
        bytes.extend_from_slice(&chunk[..retained]);
        if retained < read {
            exceeded_limit = true;
        }
    }
    Ok(BoundedRead {
        bytes,
        exceeded_limit,
    })
}

fn join_bounded_reader(
    reader: thread::JoinHandle<io::Result<BoundedRead>>,
    stream: &str,
) -> Result<BoundedRead, String> {
    reader
        .join()
        .map_err(|_| format!("bounded command {stream} reader panicked"))?
        .map_err(|error| format!("unable to read bounded command {stream}: {error}"))
}

pub fn signal_adopted_group(
    identity: &ProcessIdentity,
    proc_root: &Path,
    signal: i32,
) -> Result<bool, String> {
    if signal != SIGTERM && signal != SIGKILL {
        return Err("unsupported operation worker signal".to_string());
    }
    if !identity.still_matches(proc_root)? {
        return Ok(false);
    }
    signal_group(identity, signal)?;
    Ok(true)
}

fn signal_group(identity: &ProcessIdentity, signal: i32) -> Result<(), String> {
    let process_group = i32::try_from(identity.process_group)
        .map_err(|_| "operation process group is out of range".to_string())?;
    if process_group <= 1 {
        return Err("refusing to signal an unsafe process group".to_string());
    }
    let result = unsafe { kill(-process_group, signal) };
    if result == 0 {
        Ok(())
    } else {
        Err(format!(
            "unable to signal operation process group: {}",
            io::Error::last_os_error()
        ))
    }
}

fn validate_program(program: &Path) -> Result<(), String> {
    if !program.is_absolute()
        || program
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err("operation worker program must be absolute and normalized".to_string());
    }
    let metadata = fs::symlink_metadata(program)
        .map_err(|error| format!("unable to inspect operation worker program: {error}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("operation worker program is not a regular file".to_string());
    }
    if program.as_os_str().as_bytes().contains(&0) {
        return Err("operation worker program path contains NUL".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::Cursor;
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    fn log_file(name: &str) -> File {
        let path =
            std::env::temp_dir().join(format!("cake-process-{}-{name}.log", std::process::id()));
        let _ = fs::remove_file(&path);
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap()
    }

    #[test]
    fn unsafe_program_and_argument_vectors_are_rejected() {
        let relative = SpawnSpec {
            program: PathBuf::from("bin/echo"),
            arguments: vec![],
            environment: vec![],
        };
        assert!(relative.validate().is_err());

        let oversized = SpawnSpec {
            program: PathBuf::from("/bin/echo"),
            arguments: vec![OsString::from("x".repeat(MAX_ARGUMENT_BYTES + 1))],
            environment: vec![],
        };
        assert!(oversized.validate().is_err());
    }

    #[test]
    fn spawned_worker_has_an_attested_private_group_and_is_terminated() {
        let spec = SpawnSpec {
            program: PathBuf::from("/bin/bash"),
            arguments: vec![OsString::from("-c"), OsString::from("sleep 30 & wait")],
            environment: vec![],
        };
        let stdout = log_file("stdout");
        let stderr = log_file("stderr");
        let mut child = ManagedChild::spawn(&spec, stdout, stderr, Path::new("/proc")).unwrap();
        assert_eq!(child.identity.pid, child.identity.process_group);
        let outcome = child
            .terminate(Path::new("/proc"), Duration::from_millis(500))
            .unwrap();
        assert!(matches!(
            outcome,
            TerminationOutcome::Terminated | TerminationOutcome::Killed
        ));
        assert!(!child.identity.still_matches(Path::new("/proc")).unwrap());
    }

    #[test]
    fn independently_restoring_child_survives_handle_drop_until_explicit_signal() {
        let spec = SpawnSpec {
            program: PathBuf::from("/bin/bash"),
            arguments: vec![OsString::from("-c"), OsString::from("exec sleep 30")],
            environment: vec![],
        };
        let stdout = log_file("preserved-stdout");
        let stderr = log_file("preserved-stderr");
        let identity = {
            let mut child = ManagedChild::spawn(&spec, stdout, stderr, Path::new("/proc")).unwrap();
            child.preserve_on_drop();
            child.identity.clone()
        };
        assert!(identity.still_matches(Path::new("/proc")).unwrap());
        assert!(signal_adopted_group(&identity, Path::new("/proc"), SIGTERM).unwrap());
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(identity.pid as i32, &mut status, 0) },
            identity.pid as i32
        );
        assert!(!identity.still_matches(Path::new("/proc")).unwrap());
    }

    #[test]
    fn adopted_group_is_not_signalled_after_pid_identity_changes() {
        let identity = ProcessIdentity {
            pid: u32::MAX - 1,
            process_group: u32::MAX - 1,
            starttime_ticks: 1,
        };
        assert!(!signal_adopted_group(&identity, Path::new("/proc"), SIGTERM).unwrap());
    }

    #[test]
    fn bounded_reader_retains_only_its_limit_while_draining_to_eof() {
        let input = vec![b'x'; 64 * 1024];
        let drained = drain_bounded(Cursor::new(input), 1024).unwrap();
        assert_eq!(drained.bytes.len(), 1024);
        assert!(drained.exceeded_limit);
    }

    #[test]
    fn bounded_command_delivers_exact_bounded_stdin() {
        let spec = SpawnSpec {
            program: PathBuf::from("/bin/bash"),
            arguments: vec![
                OsString::from("-c"),
                OsString::from("IFS= read -r line; printf '%s' \"$line\""),
            ],
            environment: vec![],
        };
        let output = run_bounded_command_output_with_input(
            &spec,
            Some(b"one fixed batch\n"),
            Duration::from_secs(2),
            1024,
            || false,
            |_| {},
        )
        .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"one fixed batch");
        assert!(output.stderr.is_empty());

        assert!(run_bounded_command_output_with_input(
            &spec,
            Some(&vec![b'x'; MAX_STDIN_BYTES + 1]),
            Duration::from_secs(2),
            1024,
            || false,
            |_| {},
        )
        .unwrap_err()
        .contains("stdin exceeds"));
    }

    #[test]
    fn bounded_command_times_out_kills_and_reaps_its_process_group() {
        let pid_path =
            std::env::temp_dir().join(format!("cake-bounded-command-pid-{}", std::process::id()));
        let _ = fs::remove_file(&pid_path);
        let script = format!(
            "printf '%s\\n' \"$$\" > {}; exec sleep 60",
            pid_path.display()
        );
        let spec = SpawnSpec {
            program: PathBuf::from("/bin/bash"),
            arguments: vec![OsString::from("-c"), OsString::from(script)],
            environment: vec![],
        };
        let started = Instant::now();
        let error = run_bounded_command_output(&spec, Duration::from_millis(150), 1024, || false)
            .unwrap_err();
        assert_eq!(error, "bounded-command-timeout");
        assert!(started.elapsed() < Duration::from_secs(2));
        let pid = fs::read_to_string(&pid_path)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        assert!(
            !Path::new("/proc").join(pid.to_string()).exists(),
            "timed-out child must be reaped"
        );
        fs::remove_file(pid_path).unwrap();
    }

    #[test]
    fn infinite_output_cannot_deadlock_or_exceed_retained_memory_bound() {
        let spec = SpawnSpec {
            program: PathBuf::from("/bin/bash"),
            arguments: vec![
                OsString::from("-c"),
                OsString::from("while :; do printf '0123456789abcdef'; done"),
            ],
            environment: vec![],
        };
        let started = Instant::now();
        let error = run_bounded_command_output(&spec, Duration::from_millis(150), 1024, || false)
            .unwrap_err();
        assert_eq!(error, "bounded-command-timeout");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn finite_oversized_output_is_rejected_after_the_pipe_is_drained() {
        let spec = SpawnSpec {
            program: PathBuf::from("/bin/bash"),
            arguments: vec![
                OsString::from("-c"),
                OsString::from("for i in {1..1024}; do printf '0123456789abcdef'; done"),
            ],
            environment: vec![],
        };
        let error =
            run_bounded_command_output(&spec, Duration::from_secs(1), 1024, || false).unwrap_err();
        assert_eq!(error, "bounded-command-output-too-large");
    }

    #[test]
    fn cancellation_kills_and_reaps_before_the_timeout() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let trigger = Arc::clone(&cancelled);
        let trigger_thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            trigger.store(true, Ordering::Release);
        });
        let spec = SpawnSpec {
            program: PathBuf::from("/bin/sleep"),
            arguments: vec![OsString::from("60")],
            environment: vec![],
        };
        let started = Instant::now();
        let error = run_bounded_command_output(&spec, Duration::from_secs(2), 1024, || {
            cancelled.load(Ordering::Acquire)
        })
        .unwrap_err();
        trigger_thread.join().unwrap();
        assert_eq!(error, "bounded-command-cancelled");
        assert!(started.elapsed() < Duration::from_millis(500));
    }
}
