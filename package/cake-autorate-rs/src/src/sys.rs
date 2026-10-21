//! The daemon's direct libc calls, behind safe signatures.
//!
//! A call that fits a plain signature lives here with one `unsafe` block and
//! one `// SAFETY:` comment, and the rest of the daemon calls the safe function.
//! Anything that needs a caller-owned C structure with several fields, a
//! `pre_exec` closure or a signal handler keeps its `unsafe` block next to the
//! code that owns it, with a comment of its own. `clippy::undocumented_unsafe_blocks`
//! is denied in the source gate, so no block can be added without one.
//!
//! The wrappers change nothing about the calls they make. They do not retry:
//! `EINTR` and `EAGAIN` reach the caller as `ErrorKind::Interrupted` and
//! `ErrorKind::WouldBlock`, and the caller decides, as it did when it called
//! libc itself. The OS error is read straight after the call, before anything
//! else can overwrite `errno`. Functions that return an `OwnedFd` hand over a
//! descriptor that nothing else owns.

// Some wrappers are used by the Full build only.
#![cfg_attr(not(feature = "calibration"), allow(dead_code))]

use std::ffi::CStr;
use std::io;
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::Command;

/// `Ok(())` for a system call that returns zero, the OS error otherwise.
fn check(result: libc::c_int) -> io::Result<()> {
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Take ownership of the descriptor that a creating system call returned.
///
/// Private to this module and called only with the value that `eventfd`,
/// `timerfd_create`, `inotify_init1`, `signalfd`, `socket`, `pidfd_open` or
/// `fcntl(F_DUPFD_CLOEXEC)` has just returned.
fn adopt(raw: libc::c_int) -> io::Result<OwnedFd> {
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw` is a non-negative value that a descriptor-creating system
    // call returned a moment ago, so it is open and nothing else owns it. That
    // is the contract of `OwnedFd::from_raw_fd`.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

// ---- identity ---------------------------------------------------------------

/// The effective user id of this process.
pub(crate) fn effective_uid() -> u32 {
    // SAFETY: geteuid(2) takes no argument, reads and writes no memory and
    // cannot fail.
    unsafe { libc::geteuid() }
}

/// The effective group id of this process.
pub(crate) fn effective_gid() -> u32 {
    // SAFETY: getegid(2) takes no argument, reads and writes no memory and
    // cannot fail.
    unsafe { libc::getegid() }
}

/// The process id of the caller.
pub(crate) fn process_id() -> libc::pid_t {
    // SAFETY: getpid(2) takes no argument, reads and writes no memory and
    // cannot fail.
    unsafe { libc::getpid() }
}

/// The process id of the caller's parent.
pub(crate) fn parent_process_id() -> libc::pid_t {
    // SAFETY: getppid(2) takes no argument, reads and writes no memory and
    // cannot fail.
    unsafe { libc::getppid() }
}

/// The kernel's clock tick rate for the times in `/proc`; zero or negative when
/// the system cannot say.
pub(crate) fn clock_ticks_per_second() -> libc::c_long {
    // SAFETY: sysconf(3) takes an integer name and reads and writes no memory.
    unsafe { libc::sysconf(libc::_SC_CLK_TCK) }
}

// ---- locks and descriptors --------------------------------------------------

/// `flock(2)` on an open file. With `LOCK_NB` a lock that is held elsewhere is
/// `ErrorKind::WouldBlock`.
pub(crate) fn flock(file: impl AsFd, operation: libc::c_int) -> io::Result<()> {
    // SAFETY: flock(2) takes the descriptor, which `file` keeps open for the
    // call, and an operation word. It reads and writes no memory.
    check(unsafe { libc::flock(file.as_fd().as_raw_fd(), operation) })
}

/// A file whose `flock(2)` lock this value holds. Drop unlocks the open file
/// description before the file is closed: a child process that got a copy of
/// the descriptor between fork and exec would otherwise keep the lock until it
/// execs or exits, and the next owner would find it busy.
#[derive(Debug)]
pub(crate) struct LockedFile(std::fs::File);

impl LockedFile {
    /// Takes `operation` (`LOCK_EX` or `LOCK_SH`, with or without `LOCK_NB`).
    pub(crate) fn lock(file: std::fs::File, operation: libc::c_int) -> io::Result<Self> {
        flock(&file, operation)?;
        Ok(Self(file))
    }

    /// Takes over the lock the caller has just taken on `file`.
    pub(crate) fn held(file: std::fs::File) -> Self {
        Self(file)
    }
}

impl std::ops::Deref for LockedFile {
    type Target = std::fs::File;

    fn deref(&self) -> &std::fs::File {
        &self.0
    }
}

impl Drop for LockedFile {
    fn drop(&mut self) {
        let _ = flock(&self.0, libc::LOCK_UN);
    }
}

/// Milliseconds on the kernel boot clock (`CLOCK_BOOTTIME`): the clock behind
/// `/proc/uptime`, which keeps counting through a suspend and which a step of
/// the wall clock never moves. One system call, no file to open or parse.
pub(crate) fn boottime_ms() -> io::Result<u64> {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime(2) writes one timespec through the pointer, which
    // is a valid, exclusive reference to a local value for the whole call.
    check(unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut now) })?;
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "the boot clock is out of range");
    let seconds = u64::try_from(now.tv_sec).map_err(|_| invalid())?;
    let nanoseconds = u64::try_from(now.tv_nsec).map_err(|_| invalid())?;
    seconds
        .checked_mul(1_000)
        .and_then(|value| value.checked_add(nanoseconds / 1_000_000))
        .ok_or_else(invalid)
}

/// A pidfd for process `pid`: readable once the process has exited. A number
/// that is not a process id at all is `EINVAL`, as the kernel answers it.
pub(crate) fn pidfd_open(pid: u32) -> io::Result<OwnedFd> {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    };
    // SAFETY: pidfd_open(2), made through syscall(2), takes a process id and a
    // flags word (none). It reads and writes no memory.
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    // A descriptor number always fits in an int.
    adopt(raw as libc::c_int)
}

/// A new eventfd counter that starts at `initial`.
pub(crate) fn eventfd(initial: u32, flags: libc::c_int) -> io::Result<OwnedFd> {
    // SAFETY: eventfd(2) takes two integers and reads and writes no memory.
    adopt(unsafe { libc::eventfd(initial, flags) })
}

/// A new timer descriptor on `clock`.
pub(crate) fn timerfd_create(clock: libc::clockid_t, flags: libc::c_int) -> io::Result<OwnedFd> {
    // SAFETY: timerfd_create(2) takes two integers and reads and writes no
    // memory.
    adopt(unsafe { libc::timerfd_create(clock, flags) })
}

/// Arm or disarm `timer`. The previous setting is not wanted.
pub(crate) fn timerfd_settime(
    timer: impl AsFd,
    flags: libc::c_int,
    setting: &libc::itimerspec,
) -> io::Result<()> {
    // SAFETY: timerfd_settime(2) reads one itimerspec through `setting`, which
    // is a live reference for the whole call. The old-value pointer is null,
    // which the call allows.
    check(unsafe {
        libc::timerfd_settime(
            timer.as_fd().as_raw_fd(),
            flags,
            setting,
            std::ptr::null_mut(),
        )
    })
}

/// A new inotify instance.
pub(crate) fn inotify_init1(flags: libc::c_int) -> io::Result<OwnedFd> {
    // SAFETY: inotify_init1(2) takes a flags word and reads and writes no
    // memory.
    adopt(unsafe { libc::inotify_init1(flags) })
}

/// Watch `path` for the events in `mask`; the result is the watch descriptor.
pub(crate) fn inotify_add_watch(
    inotify: impl AsFd,
    path: &CStr,
    mask: u32,
) -> io::Result<libc::c_int> {
    // SAFETY: inotify_add_watch(2) reads the NUL-terminated path, which `path`
    // keeps alive for the call, and takes the descriptor `inotify` keeps open.
    let watch =
        unsafe { libc::inotify_add_watch(inotify.as_fd().as_raw_fd(), path.as_ptr(), mask) };
    if watch < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(watch)
    }
}

/// Remove a watch that [`inotify_add_watch`] returned.
pub(crate) fn inotify_rm_watch(inotify: impl AsFd, watch: libc::c_int) -> io::Result<()> {
    // SAFETY: inotify_rm_watch(2) takes the descriptor `inotify` keeps open and
    // a watch number. It reads and writes no memory.
    check(unsafe { libc::inotify_rm_watch(inotify.as_fd().as_raw_fd(), watch) })
}

/// A new socket.
pub(crate) fn socket(
    domain: libc::c_int,
    kind: libc::c_int,
    protocol: libc::c_int,
) -> io::Result<OwnedFd> {
    // SAFETY: socket(2) takes three integers and reads and writes no memory.
    adopt(unsafe { libc::socket(domain, kind, protocol) })
}

/// `fcntl(F_DUPFD_CLOEXEC)`: a close-on-exec duplicate of `descriptor` numbered
/// `minimum` or higher. The duplicate is a new descriptor; `descriptor` stays
/// as it is, and an invalid number is `EBADF`.
pub(crate) fn duplicate_from(descriptor: RawFd, minimum: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: fcntl(2) with F_DUPFD_CLOEXEC takes the descriptor number and an
    // integer. It reads and writes no memory and creates a new descriptor.
    adopt(unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, minimum) })
}

/// Switch `descriptor` to non-blocking mode.
pub(crate) fn set_nonblocking(descriptor: impl AsFd) -> io::Result<()> {
    let raw = descriptor.as_fd().as_raw_fd();
    // SAFETY: fcntl(2) with F_GETFL takes the descriptor `descriptor` keeps
    // open. It reads and writes no memory.
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fcntl(2) with F_SETFL takes the same descriptor and an integer
    // flag word. It reads and writes no memory.
    check(unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) })
}

// ---- reading, writing and waiting -------------------------------------------

/// `poll(2)` over `descriptors`, with a timeout in milliseconds (negative waits
/// without limit). The result is the number of descriptors with events.
pub(crate) fn poll(descriptors: &mut [libc::pollfd], timeout_ms: libc::c_int) -> io::Result<usize> {
    // SAFETY: the pointer and the length describe the caller's slice, which is
    // borrowed exclusively for the call. poll(2) reads `fd` and `events` and
    // writes `revents` of each element.
    let ready = unsafe {
        libc::poll(
            descriptors.as_mut_ptr(),
            descriptors.len() as libc::nfds_t,
            timeout_ms,
        )
    };
    if ready < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ready as usize)
    }
}

/// `read(2)` into `buffer`; the result is the number of bytes read.
pub(crate) fn read(descriptor: impl AsFd, buffer: &mut [u8]) -> io::Result<usize> {
    // SAFETY: the pointer and the length describe `buffer`, which is borrowed
    // exclusively for the call, and read(2) writes at most that many bytes. The
    // descriptor stays open while `descriptor` is borrowed.
    let count = unsafe {
        libc::read(
            descriptor.as_fd().as_raw_fd(),
            buffer.as_mut_ptr().cast::<libc::c_void>(),
            buffer.len(),
        )
    };
    if count < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(count as usize)
    }
}

/// `write(2)` of `bytes`; the result is the number of bytes written. This is
/// async-signal-safe: it allocates nothing, and the error is only an OS code.
fn write(descriptor: impl AsFd, bytes: &[u8]) -> io::Result<usize> {
    // SAFETY: the pointer and the length describe `bytes`, which write(2) only
    // reads. The descriptor stays open while `descriptor` is borrowed.
    let count = unsafe {
        libc::write(
            descriptor.as_fd().as_raw_fd(),
            bytes.as_ptr().cast::<libc::c_void>(),
            bytes.len(),
        )
    };
    if count < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(count as usize)
    }
}

/// Read one 64-bit counter record from an eventfd or a timerfd. `Ok(None)` is a
/// short read, which these descriptors never produce; `WouldBlock` means no
/// record is waiting.
pub(crate) fn read_u64(descriptor: impl AsFd) -> io::Result<Option<u64>> {
    let mut bytes = [0_u8; std::mem::size_of::<u64>()];
    let count = read(descriptor, &mut bytes)?;
    Ok((count == bytes.len()).then(|| u64::from_ne_bytes(bytes)))
}

/// Read every record that is waiting on a non-blocking eventfd or timerfd and
/// throw them away; an empty descriptor is `Ok`. A short read, which these
/// descriptors never produce, is `InvalidData`.
pub(crate) fn drain_u64(descriptor: impl AsFd) -> io::Result<()> {
    loop {
        match read_u64(&descriptor) {
            Ok(Some(_)) => {}
            Ok(None) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "short read from a counter descriptor",
                ))
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

/// Add `value` to an eventfd. `Ok(false)` is a short write, which an eventfd
/// never produces. Async-signal-safe.
pub(crate) fn write_u64(descriptor: impl AsFd, value: u64) -> io::Result<bool> {
    let bytes = value.to_ne_bytes();
    Ok(write(descriptor, &bytes)? == bytes.len())
}

// ---- processes and signals --------------------------------------------------

/// Send `signal` to the single process `pid`. A pid of zero or below would mean
/// the caller's own group or every process, so it is refused with `EINVAL`.
pub(crate) fn kill(pid: libc::pid_t, signal: libc::c_int) -> io::Result<()> {
    if pid <= 0 {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    // SAFETY: kill(2) takes a process id and a signal number and reads and
    // writes no memory. The pid is positive, so it names exactly one process.
    check(unsafe { libc::kill(pid, signal) })
}

/// Send `signal` to every process of the group `group`. Group 1 is init's, and
/// zero or below would not be a group at all, so those are refused with
/// `EINVAL`. The caller proves that the group is still its own: a pidfd and the
/// process start time guard against a reused number (see `process.rs`).
pub(crate) fn kill_process_group(group: libc::pid_t, signal: libc::c_int) -> io::Result<()> {
    if group <= 1 {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    // SAFETY: kill(2) takes a process id and a signal number and reads and
    // writes no memory. The negated value is below -1, which names one process
    // group and never "every process".
    check(unsafe { libc::kill(-group, signal) })
}

/// Make the child a session and process-group leader before it runs.
pub(crate) fn lead_new_session(command: &mut Command) {
    // SAFETY: the closure runs in the forked child before exec, where only
    // async-signal-safe calls are allowed. It makes one setsid(2) call, which
    // allocates nothing and takes no lock, and it returns the OS error through
    // `io::Error::last_os_error()`, which does not allocate either. It captures
    // nothing, so no state is shared with the parent.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

// ---- signals as descriptors -------------------------------------------------

/// A signal set holding `signals`.
pub(crate) fn signal_set(signals: &[libc::c_int]) -> io::Result<libc::sigset_t> {
    let mut set = zeroed::<libc::sigset_t>();
    // SAFETY: sigemptyset(3) initialises the sigset_t behind the exclusive
    // reference `set`.
    check(unsafe { libc::sigemptyset(&mut set) })?;
    for &signal in signals {
        // SAFETY: sigaddset(3) updates the same sigset_t through the exclusive
        // reference; it fails only for a signal number that does not exist.
        check(unsafe { libc::sigaddset(&mut set, signal) })?;
    }
    Ok(set)
}

/// Block `set` in the calling thread, so the signals are only seen through a
/// [`signalfd`].
pub(crate) fn block_signals(set: &libc::sigset_t) -> io::Result<()> {
    // SAFETY: pthread_sigmask(3) reads the set through the reference and is
    // told not to return the previous mask (null), which it allows.
    let error = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, set, std::ptr::null_mut()) };
    // pthread_sigmask returns the error number instead of setting errno.
    if error == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(error))
    }
}

/// A new descriptor that becomes readable when a signal of `set` is pending.
pub(crate) fn signalfd(set: &libc::sigset_t, flags: libc::c_int) -> io::Result<OwnedFd> {
    // SAFETY: signalfd(2) with -1 creates a new descriptor. It reads the set
    // through the reference, which is live for the call.
    adopt(unsafe { libc::signalfd(-1, set, flags) })
}

// ---- file system ------------------------------------------------------------

/// `renameat2(2)`, made through `syscall(2)` because musl has no wrapper. `flags`
/// is `RENAME_EXCHANGE` or `RENAME_NOREPLACE`.
pub(crate) fn renameat2(
    old_directory: impl AsFd,
    old: &CStr,
    new_directory: impl AsFd,
    new: &CStr,
    flags: libc::c_uint,
) -> io::Result<()> {
    // SAFETY: renameat2(2) reads two NUL-terminated paths, which `old` and
    // `new` keep alive for the call, and takes two directory descriptors that
    // the arguments keep open. It writes no memory of this process.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            old_directory.as_fd().as_raw_fd(),
            old.as_ptr(),
            new_directory.as_fd().as_raw_fd(),
            new.as_ptr(),
            flags,
        )
    };
    if result != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

// ---- plain C structures -----------------------------------------------------

/// A C structure that is made only of integers, arrays of integers, raw
/// pointers and optional function pointers, so that all-zero bytes are a valid
/// value of it. Implement it only for such types.
pub(crate) trait Plain: Sized {}

impl Plain for libc::msghdr {}
impl Plain for libc::sigaction {}
impl Plain for libc::sigset_t {}
impl Plain for libc::siginfo_t {}
impl Plain for libc::sockaddr_nl {}
impl Plain for libc::tm {}

/// An all-zero value of a plain C structure, to be filled in field by field.
pub(crate) fn zeroed<T: Plain>() -> T {
    // SAFETY: `Plain` is implemented only for C structures of integers, integer
    // arrays, raw pointers and optional function pointers, and all-zero bytes
    // are a valid, initialised value of each of those.
    unsafe { std::mem::zeroed() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File, OpenOptions};
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::process::ExitStatusExt;
    use std::path::PathBuf;
    use std::process::Stdio;

    fn scratch(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("cake-sys-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn readable(descriptor: &impl AsFd, timeout_ms: libc::c_int) -> bool {
        let mut set = [libc::pollfd {
            fd: descriptor.as_fd().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        poll(&mut set, timeout_ms).unwrap() == 1 && set[0].revents & libc::POLLIN != 0
    }

    #[test]
    fn identity_matches_what_the_standard_library_reports() {
        let directory = scratch("identity");
        let metadata = fs::metadata(&directory).unwrap();
        assert_eq!(effective_uid(), metadata.uid());
        assert_eq!(process_id() as u32, std::process::id());
        #[cfg(feature = "calibration")]
        assert_eq!(
            parent_process_id() as u32,
            std::os::unix::process::parent_id()
        );
        assert!(clock_ticks_per_second() > 0);
        // The group of a new file is the effective group unless the directory
        // is setgid; it is only checked for a plain temporary directory.
        if metadata.mode() & 0o2000 == 0 {
            let file = directory.join("file");
            File::create(&file).unwrap();
            assert_eq!(effective_gid(), fs::metadata(&file).unwrap().gid());
        }
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn flock_is_per_open_file_description_and_reports_a_busy_lock() {
        let directory = scratch("flock");
        let path = directory.join("lock");
        let first = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .unwrap();
        let second = OpenOptions::new().write(true).open(&path).unwrap();
        flock(&first, libc::LOCK_EX | libc::LOCK_NB).unwrap();
        let busy = flock(&second, libc::LOCK_EX | libc::LOCK_NB).unwrap_err();
        assert_eq!(busy.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(busy.raw_os_error(), Some(libc::EWOULDBLOCK));
        assert!(flock(&second, libc::LOCK_SH | libc::LOCK_NB).is_err());
        flock(&first, libc::LOCK_UN).unwrap();
        flock(&second, libc::LOCK_SH | libc::LOCK_NB).unwrap();
        flock(&first, libc::LOCK_SH | libc::LOCK_NB).unwrap();
        assert!(flock(&first, libc::LOCK_EX | libc::LOCK_NB).is_err());
        fs::remove_dir_all(&directory).unwrap();
    }

    /// A copy of the descriptor (what a child holds between fork and exec)
    /// keeps a lock held after a plain close, but not after LockedFile drops.
    #[test]
    fn a_locked_file_releases_its_lock_while_a_copy_of_the_descriptor_lives() {
        let directory = scratch("locked-file");
        let path = directory.join("lock");
        let open = || {
            OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&path)
                .unwrap()
        };
        let plain = open();
        flock(&plain, libc::LOCK_EX | libc::LOCK_NB).unwrap();
        let copy = plain.try_clone().unwrap();
        drop(plain);
        assert!(flock(open(), libc::LOCK_EX | libc::LOCK_NB).is_err());
        drop(copy);

        let locked = LockedFile::lock(open(), libc::LOCK_EX | libc::LOCK_NB).unwrap();
        let copy = locked.try_clone().unwrap();
        assert!(LockedFile::lock(open(), libc::LOCK_EX | libc::LOCK_NB).is_err());
        drop(locked);
        drop(LockedFile::lock(open(), libc::LOCK_EX | libc::LOCK_NB).unwrap());
        drop(copy);
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn a_pidfd_becomes_readable_when_the_process_exits_and_a_gone_process_is_esrch() {
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id() as libc::pid_t;
        let pidfd = pidfd_open(pid as u32).unwrap();
        assert!(!readable(&pidfd, 0));
        kill(pid, libc::SIGKILL).unwrap();
        assert!(readable(&pidfd, 5_000));
        let status = child.wait().unwrap();
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        let gone = pidfd_open(pid as u32).unwrap_err();
        assert_eq!(gone.raw_os_error(), Some(libc::ESRCH));
        let invalid = pidfd_open(u32::MAX).unwrap_err();
        assert_eq!(invalid.raw_os_error(), Some(libc::EINVAL));
    }

    #[test]
    fn draining_an_eventfd_takes_every_record_and_an_empty_one_is_fine() {
        let counter = eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC).unwrap();
        drain_u64(&counter).unwrap();
        assert!(write_u64(&counter, 5).unwrap());
        assert!(write_u64(&counter, 9).unwrap());
        drain_u64(&counter).unwrap();
        let empty = read_u64(&counter).unwrap_err();
        assert_eq!(empty.kind(), io::ErrorKind::WouldBlock);
        // A descriptor that cannot be read is an error, not an empty counter.
        let directory = File::open("/").unwrap();
        assert!(drain_u64(&directory).is_err());
    }

    #[test]
    fn the_boot_clock_agrees_with_proc_uptime_and_never_goes_back() {
        let first = boottime_ms().unwrap();
        let uptime = fs::read_to_string("/proc/uptime").unwrap();
        let from_proc = (uptime
            .split_whitespace()
            .next()
            .unwrap()
            .parse::<f64>()
            .unwrap()
            * 1000.0) as u64;
        let second = boottime_ms().unwrap();
        assert!(first > 0 && second >= first);
        // /proc/uptime is read between the two calls and has 10 ms resolution.
        assert!(
            from_proc + 20 >= first && from_proc <= second + 20,
            "{first} {from_proc} {second}"
        );
    }

    #[test]
    fn an_eventfd_counts_and_reports_an_empty_counter_as_would_block() {
        let counter = eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC).unwrap();
        let empty = read_u64(&counter).unwrap_err();
        assert_eq!(empty.kind(), io::ErrorKind::WouldBlock);
        assert!(!readable(&counter, 0));
        assert!(write_u64(&counter, 2).unwrap());
        assert!(write_u64(&counter, 3).unwrap());
        assert!(readable(&counter, 1_000));
        assert_eq!(read_u64(&counter).unwrap(), Some(5));
        assert_eq!(
            read_u64(&counter).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let started = eventfd(7, libc::EFD_NONBLOCK).unwrap();
        assert_eq!(read_u64(&started).unwrap(), Some(7));
    }

    #[test]
    fn a_timerfd_fires_once_and_counts_the_expirations() {
        let timer = timerfd_create(libc::CLOCK_MONOTONIC, libc::TFD_CLOEXEC).unwrap();
        let setting = libc::itimerspec {
            it_interval: libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
            it_value: libc::timespec {
                tv_sec: 0,
                tv_nsec: 20_000_000,
            },
        };
        timerfd_settime(&timer, 0, &setting).unwrap();
        assert!(readable(&timer, 5_000));
        assert!(read_u64(&timer).unwrap().is_some_and(|count| count >= 1));
        let invalid = libc::itimerspec {
            it_interval: libc::timespec {
                tv_sec: 0,
                tv_nsec: 1_000_000_000,
            },
            it_value: libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            },
        };
        let error = timerfd_settime(&timer, 0, &invalid).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EINVAL));
    }

    #[test]
    fn inotify_reports_a_new_file_and_a_watch_can_be_removed() {
        let directory = scratch("inotify");
        let inotify = inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC).unwrap();
        let path = std::ffi::CString::new(directory.to_str().unwrap()).unwrap();
        let watch = inotify_add_watch(&inotify, &path, libc::IN_CREATE).unwrap();
        assert!(watch > 0);
        assert_eq!(
            read(&inotify, &mut [0_u8; 256]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        File::create(directory.join("new")).unwrap();
        assert!(readable(&inotify, 5_000));
        let mut buffer = [0_u8; 1024];
        let count = read(&inotify, &mut buffer).unwrap();
        assert!(count >= std::mem::size_of::<libc::inotify_event>());
        inotify_rm_watch(&inotify, watch).unwrap();
        assert!(inotify_rm_watch(&inotify, watch).is_err());
        let missing = std::ffi::CString::new("/nonexistent-cake-sys").unwrap();
        let error = inotify_add_watch(&inotify, &missing, libc::IN_CREATE).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ENOENT));
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn poll_times_out_and_reports_a_hung_up_pipe() {
        let mut ends = [0 as libc::c_int; 2];
        // The standard library has no plain pipe.
        // SAFETY: pipe2(2) writes two descriptors into the array, which has room
        // for them; both are adopted at once.
        let created = unsafe { libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC) };
        assert_eq!(created, 0);
        let reader = adopt(ends[0]).unwrap();
        let writer = adopt(ends[1]).unwrap();
        let mut set = [libc::pollfd {
            fd: reader.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        assert_eq!(poll(&mut set, 10).unwrap(), 0);
        assert_eq!(poll(&mut [], 1).unwrap(), 0);
        drop(writer);
        assert_eq!(poll(&mut set, 1_000).unwrap(), 1);
        assert_ne!(set[0].revents & libc::POLLHUP, 0);
    }

    #[test]
    fn kill_refuses_numbers_that_would_signal_more_than_one_process_or_group() {
        for pid in [0, -1, -2, libc::pid_t::MIN] {
            assert_eq!(
                kill(pid, 0).unwrap_err().raw_os_error(),
                Some(libc::EINVAL),
                "pid {pid}"
            );
        }
        for group in [1, 0, -1, libc::pid_t::MIN] {
            assert_eq!(
                kill_process_group(group, 0).unwrap_err().raw_os_error(),
                Some(libc::EINVAL),
                "group {group}"
            );
        }
        // Signal 0 only checks that the process exists.
        kill(process_id(), 0).unwrap();
        assert_eq!(
            kill(i32::MAX, 0).unwrap_err().raw_os_error(),
            Some(libc::ESRCH)
        );
    }

    #[test]
    fn a_new_session_leader_owns_its_process_group_and_the_group_can_be_killed() {
        let mut command = Command::new("sleep");
        command.arg("30").stdin(Stdio::null());
        lead_new_session(&mut command);
        let mut child = command.spawn().unwrap();
        let pid = child.id() as libc::pid_t;
        // SAFETY: getpgid(2) takes a process id and writes no memory.
        assert_eq!(unsafe { libc::getpgid(pid) }, pid);
        kill_process_group(pid, libc::SIGKILL).unwrap();
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGKILL));
    }

    #[test]
    fn a_pre_exec_os_error_reaches_the_parent_as_the_same_error_number() {
        // The speed-test backend reports "the worker is gone" from its pre_exec
        // closure as ESRCH, without allocating after fork, and the parent maps
        // that number to a reason. This is the contract that relies on.
        let mut command = Command::new("true");
        // SAFETY: the closure returns at once without making any call or
        // allocating, so it is async-signal-safe.
        unsafe {
            command.pre_exec(|| Err(io::Error::from_raw_os_error(libc::ESRCH)));
        }
        let error = command.spawn().unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ESRCH));
    }

    #[test]
    fn set_nonblocking_makes_an_empty_pipe_report_would_block() {
        let mut child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        set_nonblocking(&stdout).unwrap();
        assert_eq!(
            read(&stdout, &mut [0_u8; 16]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        kill(child.id() as libc::pid_t, libc::SIGKILL).unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn duplicate_from_numbers_the_copy_above_the_minimum_and_closes_it_on_exec() {
        let file = File::open("/dev/null").unwrap();
        let copy = duplicate_from(file.as_raw_fd(), 100).unwrap();
        assert!(copy.as_raw_fd() >= 100);
        // SAFETY: fcntl(2) with F_GETFD takes a descriptor number and writes no
        // memory.
        let flags = unsafe { libc::fcntl(copy.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
        assert_eq!(
            duplicate_from(-1, 3).unwrap_err().raw_os_error(),
            Some(libc::EBADF)
        );
    }

    #[test]
    fn a_socket_is_created_and_an_invalid_domain_is_refused() {
        let socket = socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0).unwrap();
        assert!(socket.as_raw_fd() > 2);
        assert_eq!(
            super::socket(-1, libc::SOCK_STREAM, 0)
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EAFNOSUPPORT)
        );
    }

    #[test]
    fn a_signal_is_delivered_through_the_descriptor_and_not_to_the_thread() {
        // The mask change is per thread, so it dies with this one.
        std::thread::spawn(|| {
            let set = signal_set(&[libc::SIGUSR2]).unwrap();
            block_signals(&set).unwrap();
            let descriptor = signalfd(&set, libc::SFD_NONBLOCK | libc::SFD_CLOEXEC).unwrap();
            assert!(!readable(&descriptor, 0));
            // SAFETY: raise(3) takes a signal number and writes no memory. The
            // signal is blocked in this thread, so it stays pending.
            assert_eq!(unsafe { libc::raise(libc::SIGUSR2) }, 0);
            assert!(readable(&descriptor, 5_000));
            let mut record = [0_u8; std::mem::size_of::<libc::signalfd_siginfo>()];
            assert_eq!(read(&descriptor, &mut record).unwrap(), record.len());
            assert!(!readable(&descriptor, 0));
        })
        .join()
        .unwrap();
        assert_eq!(
            signal_set(&[0]).unwrap_err().raw_os_error(),
            Some(libc::EINVAL)
        );
    }

    #[test]
    fn renameat2_exchanges_and_refuses_to_replace() {
        let directory = scratch("renameat2");
        let handle = File::open(&directory).unwrap();
        fs::write(directory.join("a"), b"a").unwrap();
        fs::write(directory.join("b"), b"b").unwrap();
        let a = std::ffi::CString::new("a").unwrap();
        let b = std::ffi::CString::new("b").unwrap();
        let c = std::ffi::CString::new("c").unwrap();
        let exists = renameat2(&handle, &a, &handle, &b, libc::RENAME_NOREPLACE).unwrap_err();
        assert_eq!(exists.raw_os_error(), Some(libc::EEXIST));
        renameat2(&handle, &a, &handle, &b, libc::RENAME_EXCHANGE).unwrap();
        assert_eq!(fs::read(directory.join("a")).unwrap(), b"b");
        assert_eq!(fs::read(directory.join("b")).unwrap(), b"a");
        renameat2(&handle, &a, &handle, &c, libc::RENAME_NOREPLACE).unwrap();
        assert!(!directory.join("a").exists());
        assert_eq!(fs::read(directory.join("c")).unwrap(), b"b");
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn zeroed_structures_are_all_zero() {
        let time = zeroed::<libc::tm>();
        assert_eq!((time.tm_year, time.tm_mon, time.tm_mday), (0, 0, 0));
        let action = zeroed::<libc::sigaction>();
        assert_eq!(action.sa_flags, 0);
        let address = zeroed::<libc::sockaddr_nl>();
        assert_eq!((address.nl_pid, address.nl_groups), (0, 0));
        let message = zeroed::<libc::msghdr>();
        assert!(message.msg_name.is_null() && message.msg_iov.is_null());
        let info = zeroed::<libc::siginfo_t>();
        assert_eq!(info.si_signo, 0);
    }
}
