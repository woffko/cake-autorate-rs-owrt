//! Linux event reactor for the calibration coordinator.
//!
//! Readiness notifications are hints only.  The coordinator always re-reads
//! and re-attests its durable authority after a wakeup.  A poll timeout may
//! expose a calendar or safety deadline, but it never manufactures route,
//! runtime, quiet-window, recovery, or accounting evidence.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use super::identity::ProcessIdentity;

const NO_WAKE_FD: i32 = -1;
const MAX_WATCH_DIRECTORIES: usize = 2_048;
const INOTIFY_BUFFER_BYTES: usize = 64 * 1024;
const WATCH_MASK: u32 = libc::IN_ATTRIB
    | libc::IN_CLOSE_WRITE
    | libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_DELETE_SELF
    | libc::IN_MOVE_SELF
    | libc::IN_MOVED_FROM
    | libc::IN_MOVED_TO;

static ACTIVE_WAKE_FD: AtomicI32 = AtomicI32::new(NO_WAKE_FD);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EventReadiness {
    pub control: bool,
    pub filesystem: bool,
    pub signal: bool,
    pub process: bool,
    pub deadline: bool,
}

pub struct CalibrationEventLoop {
    inotify: OwnedFd,
    wake: OwnedFd,
    watch_descriptors: BTreeMap<i32, WatchInterest>,
    processes: BTreeMap<u32, WatchedProcess>,
}

struct WatchInterest {
    directory: PathBuf,
    names: Option<BTreeSet<Vec<u8>>>,
}

struct WatchedProcess {
    identity: ProcessIdentity,
    pidfd: OwnedFd,
}

impl CalibrationEventLoop {
    pub fn new() -> Result<Self, String> {
        let inotify = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if inotify < 0 {
            return Err(format!(
                "unable to create calibration inotify descriptor: {}",
                io::Error::last_os_error()
            ));
        }
        let inotify = unsafe { OwnedFd::from_raw_fd(inotify) };
        let wake = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        if wake < 0 {
            return Err(format!(
                "unable to create calibration wake descriptor: {}",
                io::Error::last_os_error()
            ));
        }
        let wake = unsafe { OwnedFd::from_raw_fd(wake) };
        ACTIVE_WAKE_FD
            .compare_exchange(
                NO_WAKE_FD,
                wake.as_raw_fd(),
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .map_err(|_| "another calibration event loop already owns signal wakeup".to_string())?;
        Ok(Self {
            inotify,
            wake,
            watch_descriptors: BTreeMap::new(),
            processes: BTreeMap::new(),
        })
    }

    pub fn watch_tree(
        &mut self,
        root: &Path,
        maximum_depth: usize,
        required: bool,
    ) -> Result<(), String> {
        let metadata = match fs::symlink_metadata(root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound && !required => return Ok(()),
            Err(error) => {
                return Err(format!(
                    "unable to inspect calibration event root {}: {error}",
                    root.display()
                ))
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(format!(
                "calibration event root is not a real directory: {}",
                root.display()
            ));
        }
        let mut queue = VecDeque::from([(root.to_path_buf(), 0_usize)]);
        let mut seen = BTreeSet::new();
        while let Some((directory, depth)) = queue.pop_front() {
            if !seen.insert(directory.clone()) {
                continue;
            }
            if seen.len() > MAX_WATCH_DIRECTORIES {
                return Err("calibration event directory tree exceeds its bound".to_string());
            }
            self.watch_directory(&directory)?;
            if depth >= maximum_depth {
                continue;
            }
            for entry in fs::read_dir(&directory).map_err(|error| {
                format!(
                    "unable to enumerate calibration event root {}: {error}",
                    directory.display()
                )
            })? {
                let entry = entry.map_err(|error| {
                    format!(
                        "unable to inspect calibration event root {}: {error}",
                        directory.display()
                    )
                })?;
                let file_type = entry.file_type().map_err(|error| {
                    format!(
                        "unable to inspect calibration event entry {}: {error}",
                        entry.path().display()
                    )
                })?;
                if file_type.is_dir() && !file_type.is_symlink() {
                    queue.push_back((entry.path(), depth + 1));
                }
            }
        }
        Ok(())
    }

    pub fn watch_named_entry(&mut self, directory: &Path, name: &[u8]) -> Result<(), String> {
        if name.is_empty() || name.contains(&0) || name.contains(&b'/') {
            return Err("calibration event entry name is unsafe".to_string());
        }
        let descriptor = self.add_watch(directory)?;
        match self.watch_descriptors.entry(descriptor) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(WatchInterest {
                    directory: directory.to_path_buf(),
                    names: Some(BTreeSet::from([name.to_vec()])),
                });
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                if entry.get().directory != directory {
                    return Err("calibration inotify descriptor changed directory".to_string());
                }
                if let Some(names) = entry.get_mut().names.as_mut() {
                    names.insert(name.to_vec());
                }
            }
        }
        Ok(())
    }

    pub fn refresh_processes(
        &mut self,
        identities: &[ProcessIdentity],
        proc_root: &Path,
    ) -> Result<bool, String> {
        let mut desired = BTreeMap::new();
        for identity in identities {
            match desired.insert(identity.pid, identity.clone()) {
                Some(previous) if previous != *identity => {
                    return Err("one PID has conflicting calibration process identities".to_string())
                }
                _ => {}
            }
        }
        self.processes.retain(|pid, process| {
            desired
                .get(pid)
                .is_some_and(|identity| identity == &process.identity)
        });

        let mut stale = false;
        for (pid, identity) in desired {
            if self.processes.contains_key(&pid) {
                continue;
            }
            if !identity.still_matches(proc_root)? {
                stale = true;
                continue;
            }
            let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0_u32) };
            if raw < 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    stale = true;
                    continue;
                }
                return Err(format!(
                    "unable to open calibration pidfd for process {pid}: {error}"
                ));
            }
            let pidfd = unsafe { OwnedFd::from_raw_fd(raw as RawFd) };
            if !identity.still_matches(proc_root)? {
                stale = true;
                continue;
            }
            self.processes
                .insert(pid, WatchedProcess { identity, pidfd });
        }
        Ok(stale)
    }

    pub fn wait(
        &mut self,
        listener_fd: RawFd,
        timeout: Option<Duration>,
    ) -> Result<EventReadiness, String> {
        let mut descriptors = vec![
            libc::pollfd {
                fd: listener_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.inotify.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        descriptors.extend(self.processes.values().map(|process| libc::pollfd {
            fd: process.pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }));
        let timeout_ms = poll_timeout_ms(timeout);
        let status = unsafe {
            libc::poll(
                descriptors.as_mut_ptr(),
                descriptors.len() as libc::nfds_t,
                timeout_ms,
            )
        };
        if status < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                return Ok(EventReadiness {
                    signal: true,
                    ..EventReadiness::default()
                });
            }
            return Err(format!("calibration event poll failed: {error}"));
        }
        if status == 0 {
            return Ok(EventReadiness {
                deadline: true,
                ..EventReadiness::default()
            });
        }
        for descriptor in &descriptors[..3] {
            let fatal = descriptor.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL);
            if fatal != 0 {
                return Err(format!(
                    "calibration event descriptor {} failed with poll flags {fatal}",
                    descriptor.fd
                ));
            }
        }
        for descriptor in &descriptors[3..] {
            let fatal = descriptor.revents & (libc::POLLERR | libc::POLLNVAL);
            if fatal != 0 {
                return Err(format!(
                    "calibration process descriptor {} failed with poll flags {fatal}",
                    descriptor.fd
                ));
            }
        }
        let filesystem = descriptors[1].revents & libc::POLLIN != 0;
        let filesystem = if filesystem {
            self.drain_inotify()?
        } else {
            false
        };
        let signal = descriptors[2].revents & libc::POLLIN != 0;
        if signal {
            drain_eventfd(self.wake.as_raw_fd())?;
        }
        Ok(EventReadiness {
            control: descriptors[0].revents & libc::POLLIN != 0,
            filesystem,
            signal,
            process: descriptors[3..]
                .iter()
                .any(|descriptor| descriptor.revents & (libc::POLLIN | libc::POLLHUP) != 0),
            deadline: false,
        })
    }

    fn watch_directory(&mut self, directory: &Path) -> Result<(), String> {
        let descriptor = self.add_watch(directory)?;
        match self.watch_descriptors.entry(descriptor) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(WatchInterest {
                    directory: directory.to_path_buf(),
                    names: None,
                });
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                if entry.get().directory != directory {
                    return Err("calibration inotify descriptor changed directory".to_string());
                }
                entry.get_mut().names = None;
            }
        }
        Ok(())
    }

    fn add_watch(&self, directory: &Path) -> Result<i32, String> {
        let metadata = fs::symlink_metadata(directory).map_err(|error| {
            format!(
                "unable to inspect calibration event directory {}: {error}",
                directory.display()
            )
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(format!(
                "calibration event directory is not a real directory: {}",
                directory.display()
            ));
        }
        let bytes = std::ffi::CString::new(directory.as_os_str().as_encoded_bytes())
            .map_err(|_| "calibration event path contains NUL".to_string())?;
        let descriptor = unsafe {
            libc::inotify_add_watch(
                self.inotify.as_raw_fd(),
                bytes.as_ptr(),
                WATCH_MASK | libc::IN_DONT_FOLLOW | libc::IN_ONLYDIR,
            )
        };
        if descriptor < 0 {
            return Err(format!(
                "unable to watch calibration event directory {}: {}",
                directory.display(),
                io::Error::last_os_error()
            ));
        }
        Ok(descriptor)
    }

    fn drain_inotify(&mut self) -> Result<bool, String> {
        let mut buffer = vec![0_u8; INOTIFY_BUFFER_BYTES];
        let mut relevant = false;
        loop {
            let count = unsafe {
                libc::read(
                    self.inotify.as_raw_fd(),
                    buffer.as_mut_ptr().cast::<libc::c_void>(),
                    buffer.len(),
                )
            };
            if count < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::WouldBlock {
                    return Ok(relevant);
                }
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(format!(
                    "unable to drain calibration filesystem events: {error}"
                ));
            }
            if count == 0 {
                return Err("calibration inotify descriptor reached unexpected EOF".to_string());
            }
            let count = usize::try_from(count)
                .map_err(|_| "calibration filesystem event count is invalid".to_string())?;
            let mut offset = 0_usize;
            while offset + std::mem::size_of::<libc::inotify_event>() <= count {
                let event = unsafe {
                    std::ptr::read_unaligned(
                        buffer.as_ptr().add(offset).cast::<libc::inotify_event>(),
                    )
                };
                let record_len = std::mem::size_of::<libc::inotify_event>()
                    .checked_add(event.len as usize)
                    .ok_or_else(|| "calibration filesystem event length overflow".to_string())?;
                if record_len == 0 || offset.saturating_add(record_len) > count {
                    return Err("calibration filesystem event is truncated".to_string());
                }
                if event.mask & libc::IN_IGNORED != 0 {
                    self.watch_descriptors.remove(&event.wd);
                    relevant = true;
                } else if event.mask & libc::IN_Q_OVERFLOW != 0 {
                    relevant = true;
                } else if let Some(interest) = self.watch_descriptors.get(&event.wd) {
                    let name_start = offset + std::mem::size_of::<libc::inotify_event>();
                    let name_end = name_start + event.len as usize;
                    let raw_name = &buffer[name_start..name_end];
                    let name_len = raw_name
                        .iter()
                        .position(|byte| *byte == 0)
                        .unwrap_or(raw_name.len());
                    let name = &raw_name[..name_len];
                    if name.is_empty()
                        || interest
                            .names
                            .as_ref()
                            .is_none_or(|names| names.contains(name))
                    {
                        relevant = true;
                    }
                }
                offset += record_len;
            }
            if offset != count {
                return Err("calibration filesystem event stream is misaligned".to_string());
            }
        }
    }
}

impl Drop for CalibrationEventLoop {
    fn drop(&mut self) {
        let _ = ACTIVE_WAKE_FD.compare_exchange(
            self.wake.as_raw_fd(),
            NO_WAKE_FD,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }
}

pub fn wake_from_signal() {
    let descriptor = ACTIVE_WAKE_FD.load(Ordering::Relaxed);
    if descriptor == NO_WAKE_FD {
        return;
    }
    let value = 1_u64;
    unsafe {
        libc::write(
            descriptor,
            (&value as *const u64).cast::<libc::c_void>(),
            std::mem::size_of::<u64>(),
        );
    }
}

extern "C" fn handle_child_signal(_: libc::c_int) {
    // Synchronous helpers such as `uci` are spawned and reaped inside one
    // state-machine pass.  Feeding their SIGCHLD into the reactor would make
    // that pass self-triggering.  Every asynchronous coordinator-owned child
    // is instead registered by exact ProcessIdentity and watched through a
    // level-triggered pidfd.
}

pub fn install_child_signal_handler() -> Result<(), String> {
    let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
    action.sa_sigaction = handle_child_signal as *const () as usize;
    action.sa_flags = libc::SA_RESTART | libc::SA_NOCLDSTOP;
    if unsafe { libc::sigemptyset(&mut action.sa_mask) } != 0 {
        return Err(format!(
            "unable to initialize SIGCHLD mask: {}",
            io::Error::last_os_error()
        ));
    }
    if unsafe { libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut()) } != 0 {
        return Err(format!(
            "unable to install calibration SIGCHLD handler: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn poll_timeout_ms(timeout: Option<Duration>) -> libc::c_int {
    let Some(timeout) = timeout else {
        return -1;
    };
    if timeout.is_zero() {
        return 0;
    }
    let millis = timeout.as_nanos().saturating_add(999_999) / 1_000_000;
    i32::try_from(millis).unwrap_or(i32::MAX)
}

fn drain_eventfd(descriptor: RawFd) -> Result<(), String> {
    loop {
        let mut value = 0_u64;
        let count = unsafe {
            libc::read(
                descriptor,
                (&mut value as *mut u64).cast::<libc::c_void>(),
                std::mem::size_of::<u64>(),
            )
        };
        if count == std::mem::size_of::<u64>() as isize {
            continue;
        }
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                return Ok(());
            }
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("unable to drain calibration wake events: {error}"));
        }
        return Err("calibration wake descriptor returned an invalid record".to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, MutexGuard, OnceLock};

    fn event_loop_guard() -> MutexGuard<'static, ()> {
        static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
        GUARD
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("event-loop test mutex is poisoned")
    }

    fn temp_path(name: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        std::env::temp_dir().join(format!(
            "cake-event-loop-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn filesystem_and_control_events_wake_without_periodic_polling() {
        let _guard = event_loop_guard();
        let root = temp_path("events");
        fs::create_dir(&root).unwrap();
        let socket = root.join("control.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut events = CalibrationEventLoop::new().unwrap();
        events.watch_tree(&root, 1, true).unwrap();

        fs::write(root.join("runtime.tmp"), b"one").unwrap();
        let ready = events
            .wait(listener.as_raw_fd(), Some(Duration::from_secs(1)))
            .unwrap();
        assert!(ready.filesystem);
        assert!(!ready.deadline);

        let _client = std::os::unix::net::UnixStream::connect(&socket).unwrap();
        let ready = events
            .wait(listener.as_raw_fd(), Some(Duration::from_secs(1)))
            .unwrap();
        assert!(ready.control);
        drop(events);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn timeout_is_only_reported_as_a_deadline_event() {
        let _guard = event_loop_guard();
        let root = temp_path("deadline");
        fs::create_dir(&root).unwrap();
        let socket = root.join("control.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut events = CalibrationEventLoop::new().unwrap();
        let ready = events
            .wait(listener.as_raw_fd(), Some(Duration::from_millis(2)))
            .unwrap();
        assert_eq!(
            ready,
            EventReadiness {
                deadline: true,
                ..EventReadiness::default()
            }
        );
        drop(events);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn named_parent_watch_ignores_unrelated_files() {
        let _guard = event_loop_guard();
        let root = temp_path("filtered");
        fs::create_dir(&root).unwrap();
        let socket = root.join("control.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut events = CalibrationEventLoop::new().unwrap();
        events.watch_named_entry(&root, b"wanted").unwrap();

        fs::write(root.join("unrelated"), b"one").unwrap();
        let unrelated = events
            .wait(listener.as_raw_fd(), Some(Duration::from_secs(1)))
            .unwrap();
        assert!(!unrelated.filesystem);

        fs::write(root.join("wanted"), b"two").unwrap();
        let wanted = events
            .wait(listener.as_raw_fd(), Some(Duration::from_secs(1)))
            .unwrap();
        assert!(wanted.filesystem);
        drop(events);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn signal_wakeup_interrupts_an_unbounded_wait() {
        let _guard = event_loop_guard();
        let root = temp_path("signal");
        fs::create_dir(&root).unwrap();
        let socket = root.join("control.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut events = CalibrationEventLoop::new().unwrap();
        wake_from_signal();
        let ready = events.wait(listener.as_raw_fd(), None).unwrap();
        assert!(ready.signal);
        drop(events);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn synchronous_helper_exit_does_not_self_wake_the_reactor() {
        let _guard = event_loop_guard();
        let root = temp_path("synchronous-child");
        fs::create_dir(&root).unwrap();
        let socket = root.join("control.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut events = CalibrationEventLoop::new().unwrap();
        install_child_signal_handler().unwrap();

        assert!(Command::new("true").status().unwrap().success());
        let ready = events
            .wait(listener.as_raw_fd(), Some(Duration::from_millis(2)))
            .unwrap();
        assert_eq!(
            ready,
            EventReadiness {
                deadline: true,
                ..EventReadiness::default()
            }
        );
        drop(events);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn repeated_watch_refresh_is_idempotent_per_inode() {
        let _guard = event_loop_guard();
        let root = temp_path("idempotent");
        fs::create_dir(&root).unwrap();
        let mut events = CalibrationEventLoop::new().unwrap();
        events.watch_tree(&root, 0, true).unwrap();
        let first = events.watch_descriptors.len();
        events.watch_tree(&root, 0, true).unwrap();
        assert_eq!(events.watch_descriptors.len(), first);
        drop(events);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pidfd_wakes_for_an_adopted_process_without_periodic_proc_polling() {
        let _guard = event_loop_guard();
        let root = temp_path("pidfd");
        fs::create_dir(&root).unwrap();
        let socket = root.join("control.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let mut child = Command::new("sh")
            .args(["-c", "read value"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let identity = ProcessIdentity::inspect(Path::new("/proc"), child.id()).unwrap();
        let mut events = CalibrationEventLoop::new().unwrap();
        assert!(!events
            .refresh_processes(&[identity], Path::new("/proc"))
            .unwrap());
        child.kill().unwrap();
        child.wait().unwrap();
        let ready = events
            .wait(listener.as_raw_fd(), Some(Duration::from_secs(1)))
            .unwrap();
        assert!(ready.process);
        drop(events);
        fs::remove_dir_all(root).unwrap();
    }
}
