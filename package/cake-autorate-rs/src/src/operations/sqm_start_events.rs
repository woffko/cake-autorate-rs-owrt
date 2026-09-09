//! Read-only wakeups for ordinary SQM start attestation (Full and Lite).
//! Events invalidate an observation; neither an event nor a deadline proves
//! ownership or readiness. The lifecycle must always run the exact attestor.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::Instant;

pub(super) trait StartEvents {
    fn drain(&mut self) -> Result<bool, String>;
    fn wait(&mut self, deadline: Instant) -> Result<bool, String>;
}

pub(super) struct SqmStartEvents {
    files: OwnedFd,
    network: OwnedFd,
    directories: Vec<PathBuf>,
}

impl SqmStartEvents {
    pub(super) fn subscribe(directories: Vec<PathBuf>) -> Result<Self, String> {
        let files = owned(unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) })?;
        let network = owned(unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                libc::NETLINK_ROUTE,
            )
        })?;
        // Bind only multicast groups; this socket sends no requests. Link and
        // traffic-control changes cover IFB, CAKE, ingress and mirred updates.
        let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        address.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        address.nl_groups = (libc::RTMGRP_LINK | libc::RTMGRP_TC) as u32;
        if unsafe {
            libc::bind(
                network.as_raw_fd(),
                (&address as *const libc::sockaddr_nl).cast(),
                std::mem::size_of_val(&address) as libc::socklen_t,
            )
        } != 0
        {
            return Err(event_error("subscribe to SQM kernel changes"));
        }
        let events = Self {
            files,
            network,
            directories,
        };
        events.arm_directories()?;
        Ok(events)
    }

    fn arm_directories(&self) -> Result<(), String> {
        for directory in &self.directories {
            // SQM's runtime directory may not exist at early boot. Watch its
            // nearest existing ancestor, then add the child before observing
            // again. Keep ancestor watches to notice removal/replacement.
            let mut path = directory.as_path();
            loop {
                let name = CString::new(path.as_os_str().as_bytes())
                    .map_err(|_| "invalid SQM event directory".to_string())?;
                let mask = libc::IN_CREATE
                    | libc::IN_DELETE
                    | libc::IN_MOVED_FROM
                    | libc::IN_MOVED_TO
                    | libc::IN_CLOSE_WRITE
                    | libc::IN_ATTRIB
                    | libc::IN_DELETE_SELF
                    | libc::IN_MOVE_SELF
                    | libc::IN_ONLYDIR;
                if unsafe { libc::inotify_add_watch(self.files.as_raw_fd(), name.as_ptr(), mask) }
                    >= 0
                {
                    break;
                }
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::NotFound {
                    return Err(format!("watch SQM start state: {error}"));
                }
                path = path
                    .parent()
                    .ok_or_else(|| "SQM event directory has no existing ancestor".to_string())?;
            }
        }
        Ok(())
    }
}

impl StartEvents for SqmStartEvents {
    fn drain(&mut self) -> Result<bool, String> {
        let mut changed = false;
        let mut bytes = [0u8; 64 * 1024];
        for fd in [&self.files, &self.network] {
            // A continuous event flood must fail closed, not prevent the
            // lifecycle watchdog from ever being checked.
            for batch in 0..256 {
                let count =
                    unsafe { libc::read(fd.as_raw_fd(), bytes.as_mut_ptr().cast(), bytes.len()) };
                if count > 0 {
                    changed = true;
                } else if count == 0 {
                    return Err("SQM start event channel closed".to_string());
                } else {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::WouldBlock {
                        break;
                    }
                    if error.raw_os_error() == Some(libc::ENOBUFS) {
                        // Lost netlink messages invalidate the whole snapshot.
                        changed = true;
                        continue;
                    }
                    if error.kind() != io::ErrorKind::Interrupted {
                        return Err(format!("read SQM start events: {error}"));
                    }
                }
                if batch == 255 {
                    return Err("SQM start event flood exceeded safety bound".to_string());
                }
            }
        }
        self.arm_directories()?;
        Ok(changed)
    }

    fn wait(&mut self, deadline: Instant) -> Result<bool, String> {
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            let timeout = deadline
                .duration_since(now)
                .as_millis()
                .saturating_add(1)
                .min(i32::MAX as u128) as i32;
            let mut fds =
                [self.files.as_raw_fd(), self.network.as_raw_fd()].map(|fd| libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                });
            let result =
                unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
            if result < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(event_error("wait for SQM start events"));
            }
            if result == 0 {
                return Ok(false);
            }
            if fds
                .iter()
                .any(|fd| fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0)
            {
                return Err("SQM start event channel failed".to_string());
            }
            return Ok(true);
        }
    }
}

fn owned(fd: libc::c_int) -> Result<OwnedFd, String> {
    if fd < 0 {
        Err(event_error("open SQM start event channel"))
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

fn event_error(action: &str) -> String {
    format!("{action}: {}", io::Error::last_os_error())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    #[test]
    fn watches_missing_state_directory_and_atomic_replacement_without_read_wakeups() {
        let root = std::env::temp_dir().join(format!("sqm-start-events-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let state = root.join("sqm");
        let mut events = SqmStartEvents::subscribe(vec![state.clone()]).unwrap();
        assert!(!events.drain().unwrap());
        fs::create_dir(&state).unwrap();
        assert!(events
            .wait(Instant::now() + Duration::from_secs(1))
            .unwrap());
        assert!(events.drain().unwrap());
        let file = state.join("wan.state");
        fs::write(&file, "first").unwrap();
        assert!(events.drain().unwrap());
        assert_eq!(fs::read_to_string(&file).unwrap(), "first");
        assert!(!events.drain().unwrap());
        fs::write(state.join("next"), "second").unwrap();
        fs::rename(state.join("next"), &file).unwrap();
        assert!(events.drain().unwrap());
        fs::remove_dir_all(&state).unwrap();
        assert!(events.drain().unwrap());
        fs::create_dir(&state).unwrap();
        assert!(events.drain().unwrap());
        fs::write(&file, "third").unwrap();
        assert!(events.drain().unwrap());
        assert!(!events.wait(Instant::now()).unwrap());
        fs::remove_dir_all(root).unwrap();
    }
}
