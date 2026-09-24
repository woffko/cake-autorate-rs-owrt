//! Read-only wakeups for ordinary SQM start attestation (Full and Lite).
//! Events invalidate an observation; neither an event nor a deadline proves
//! ownership or readiness. The lifecycle must always run the exact attestor.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::fs::File;
use std::io;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub(super) trait StartEvents {
    fn drain(&mut self) -> Result<bool, String>;
    fn wait(&mut self, deadline: Instant) -> Result<bool, String>;
}

pub(super) struct SqmStartEvents {
    files: OwnedFd,
    network: OwnedFd,
    directories: Vec<PathBuf>,
    watches: BTreeMap<i32, PathBuf>,
    retired_watches: BTreeSet<i32>,
    owned_files: Option<BTreeSet<PathBuf>>,
    interfaces: BTreeSet<String>,
    indices: BTreeMap<u32, String>,
    sys_class_net: Option<PathBuf>,
}

impl SqmStartEvents {
    #[cfg(test)]
    pub(super) fn subscribe(directories: Vec<PathBuf>) -> Result<Self, String> {
        Self::subscribe_inner(directories, None, BTreeSet::new(), None)
    }

    pub(super) fn subscribe_owned(
        files: Vec<PathBuf>,
        interfaces: Vec<String>,
        sys_class_net: &Path,
    ) -> Result<Self, String> {
        if !sys_class_net.is_absolute()
            || files.len() > 256
            || interfaces.len() > 192
            || files
                .iter()
                .any(|path| !path.is_absolute() || path.file_name().is_none())
            || interfaces
                .iter()
                .any(|name| !super::runtime_health::safe_interface(name))
        {
            return Err("invalid owned SQM event scope".into());
        }
        let directories = files
            .iter()
            .filter_map(|path| path.parent().map(Path::to_path_buf))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Self::subscribe_inner(
            directories,
            Some(files.into_iter().collect()),
            interfaces.into_iter().collect(),
            Some(sys_class_net.into()),
        )
    }

    fn subscribe_inner(
        directories: Vec<PathBuf>,
        owned_files: Option<BTreeSet<PathBuf>>,
        interfaces: BTreeSet<String>,
        sys_class_net: Option<PathBuf>,
    ) -> Result<Self, String> {
        if directories.len() > 256 || directories.iter().any(|path| !path.is_absolute()) {
            return Err("invalid SQM event directories".into());
        }
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
        let mut events = Self {
            files,
            network,
            directories,
            watches: BTreeMap::new(),
            retired_watches: BTreeSet::new(),
            owned_files,
            interfaces,
            indices: BTreeMap::new(),
            sys_class_net,
        };
        events.arm_directories()?;
        events.refresh_indices()?;
        Ok(events)
    }

    fn arm_directories(&mut self) -> Result<(), String> {
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
                // SAFETY: the descriptor is owned and the NUL-terminated name
                // remains alive across this existing read-only watch boundary.
                let wd =
                    unsafe { libc::inotify_add_watch(self.files.as_raw_fd(), name.as_ptr(), mask) };
                if wd >= 0 {
                    self.retired_watches.remove(&wd);
                    self.watches.insert(wd, path.into());
                    if self.watches.len() > 512 {
                        return Err("SQM watch bound exceeded".into());
                    }
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

    fn refresh_indices(&mut self) -> Result<bool, String> {
        let Some(root) = &self.sys_class_net else {
            return Ok(false);
        };
        let mut indices = BTreeMap::new();
        for name in &self.interfaces {
            let file = match File::open(root.join(name).join("ifindex")) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => return Err("read owned SQM interface index".into()),
            };
            let mut text = String::with_capacity(32);
            file.take(32)
                .read_to_string(&mut text)
                .map_err(|_| "invalid owned SQM interface index")?;
            if text.len() == 32 {
                return Err("oversized owned SQM interface index".into());
            }
            let index = text
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|index| *index > 0 && *index <= i32::MAX as u32)
                .ok_or("invalid owned SQM interface index")?;
            if indices.insert(index, name.clone()).is_some() {
                return Err("ambiguous owned SQM interface index".into());
            }
        }
        let changed = indices != self.indices;
        self.indices = indices;
        Ok(changed)
    }

    fn file_events(&mut self, mut bytes: &[u8]) -> Result<bool, String> {
        let mut changed = false;
        while !bytes.is_empty() {
            if bytes.len() < 16 {
                return Err("truncated SQM file event".into());
            }
            let wd = i32::from_ne_bytes(
                bytes[0..4]
                    .try_into()
                    .map_err(|_| "invalid SQM file event")?,
            );
            let mask = u32::from_ne_bytes(
                bytes[4..8]
                    .try_into()
                    .map_err(|_| "invalid SQM file event")?,
            );
            let length = u32::from_ne_bytes(
                bytes[12..16]
                    .try_into()
                    .map_err(|_| "invalid SQM file event")?,
            ) as usize;
            let end = 16usize
                .checked_add(length)
                .filter(|end| *end <= bytes.len())
                .ok_or("truncated SQM file event")?;
            if mask & libc::IN_Q_OVERFLOW != 0 {
                changed = true;
            } else if self.retired_watches.contains(&wd) {
                // A moved-away directory is no longer the requested path.
            } else if let Some(files) = &self.owned_files {
                if let Some(directory) = self.watches.get(&wd) {
                    let path = if length == 0 {
                        directory.clone()
                    } else {
                        let name = &bytes[16..end];
                        let zero = name
                            .iter()
                            .position(|byte| *byte == 0)
                            .ok_or("unterminated SQM file event")?;
                        let name = &name[..zero];
                        if name.contains(&b'/') || name == b".." || name == b"." {
                            return Err("invalid SQM file event name".into());
                        }
                        directory.join(std::ffi::OsStr::from_bytes(name))
                    };
                    changed |= files
                        .iter()
                        .any(|file| file == &path || file.starts_with(&path));
                } else {
                    changed = true;
                } // unknown/lost watch ownership
            } else {
                changed = true;
            }
            if mask & (libc::IN_MOVE_SELF | libc::IN_DELETE_SELF) != 0 {
                self.retired_watches.insert(wd);
            }
            if mask & libc::IN_IGNORED != 0 {
                self.watches.remove(&wd);
                self.retired_watches.remove(&wd);
            }
            bytes = &bytes[end..];
        }
        Ok(changed)
    }

    fn network_events(&mut self, mut bytes: &[u8]) -> Result<bool, String> {
        if self.owned_files.is_none() {
            return Ok(!bytes.is_empty());
        }
        let mut changed = false;
        while !bytes.is_empty() {
            if bytes.len() < 16 {
                return Err("truncated SQM netlink header".into());
            }
            let length = u32::from_ne_bytes(
                bytes[0..4]
                    .try_into()
                    .map_err(|_| "invalid SQM netlink header")?,
            ) as usize;
            if length < 16 || length > bytes.len() {
                return Err("truncated SQM netlink message".into());
            }
            let kind = u16::from_ne_bytes(
                bytes[4..6]
                    .try_into()
                    .map_err(|_| "invalid SQM netlink header")?,
            );
            let payload = &bytes[16..length];
            match kind {
                1 | 3 => {}              // NLMSG_NOOP/DONE do not change topology
                2 | 4 => changed = true, // error/overrun: never assume no change
                16 | 17 => {
                    // RTM_NEWLINK/DELLINK, Linux ifinfomsg
                    if payload.len() < 16 {
                        return Err("truncated SQM link event".into());
                    }
                    let index = u32::from_ne_bytes(
                        payload[4..8]
                            .try_into()
                            .map_err(|_| "invalid SQM link event")?,
                    );
                    if index == 0 || index > i32::MAX as u32 {
                        return Err("invalid SQM link index".into());
                    }
                    let mut attrs = &payload[16..];
                    let mut name = None;
                    while !attrs.is_empty() {
                        if attrs.len() < 4 {
                            return Err("truncated SQM link attribute".into());
                        }
                        let len = u16::from_ne_bytes([attrs[0], attrs[1]]) as usize;
                        let kind = u16::from_ne_bytes([attrs[2], attrs[3]]) & 0x3fff;
                        if len < 4 || len > attrs.len() {
                            return Err("invalid SQM link attribute".into());
                        }
                        if kind == 3 {
                            // IFLA_IFNAME
                            if name.is_some() {
                                return Err("duplicate SQM link name".into());
                            }
                            let value = &attrs[4..len];
                            let zero = value
                                .iter()
                                .position(|byte| *byte == 0)
                                .ok_or("unterminated SQM link name")?;
                            name = Some(&value[..zero]);
                        }
                        let aligned = (len + 3) & !3;
                        if len == attrs.len() {
                            break;
                        }
                        attrs = attrs.get(aligned..).ok_or("invalid SQM link padding")?;
                    }
                    let known = self.indices.contains_key(&index);
                    let owned_name = name.and_then(|bytes| {
                        self.interfaces.iter().find(|name| name.as_bytes() == bytes)
                    });
                    let owned = owned_name.is_some();
                    changed |= known || owned;
                    if kind == 17 || (known && name.is_some() && !owned) {
                        self.indices.remove(&index);
                    } else if let Some(name) = owned_name {
                        self.indices
                            .retain(|old_index, old_name| *old_index == index || old_name != name);
                        self.indices.insert(index, name.clone());
                    }
                }
                36..=38 | 40..=42 | 44..=46 => {
                    // tcmsg: qdisc/class/filter
                    if payload.len() < 20 {
                        return Err("truncated SQM traffic-control event".into());
                    }
                    let index = u32::from_ne_bytes(
                        payload[4..8]
                            .try_into()
                            .map_err(|_| "invalid SQM traffic-control event")?,
                    );
                    // Shared blocks/global actions cannot be proved unrelated.
                    changed |=
                        index == 0 || index > i32::MAX as u32 || self.indices.contains_key(&index);
                }
                _ => changed = true,
            }
            let aligned = length.checked_add(3).ok_or("invalid SQM netlink length")? & !3;
            if length == bytes.len() {
                break;
            }
            bytes = bytes.get(aligned..).ok_or("invalid SQM netlink padding")?;
        }
        Ok(changed)
    }
}

impl StartEvents for SqmStartEvents {
    fn drain(&mut self) -> Result<bool, String> {
        let mut changed = false;
        let mut bytes = [0u8; 64 * 1024];
        for (fd, network) in [
            (self.files.as_raw_fd(), false),
            (self.network.as_raw_fd(), true),
        ] {
            // A continuous event flood must fail closed, not prevent the
            // lifecycle watchdog from ever being checked.
            for batch in 0..256 {
                let count = unsafe { libc::read(fd, bytes.as_mut_ptr().cast(), bytes.len()) };
                if count > 0 {
                    let data = bytes
                        .get(..count as usize)
                        .ok_or("oversized SQM event read")?;
                    changed |= if network {
                        self.network_events(data)?
                    } else {
                        self.file_events(data)?
                    };
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
        changed |= self.refresh_indices()?;
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
                .any(|fd| fd.revents & (libc::POLLHUP | libc::POLLNVAL) != 0)
                || fds[0].revents & libc::POLLERR != 0
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

    fn net_message(kind: u16, payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0u8; 16];
        bytes[0..4].copy_from_slice(&((16 + payload.len()) as u32).to_ne_bytes());
        bytes[4..6].copy_from_slice(&kind.to_ne_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }
    fn link_message(kind: u16, index: u32, name: &[u8]) -> Vec<u8> {
        let mut payload = vec![0u8; 16];
        payload[4..8].copy_from_slice(&index.to_ne_bytes());
        let len = 4 + name.len() + 1;
        payload.extend_from_slice(&(len as u16).to_ne_bytes());
        payload.extend_from_slice(&3u16.to_ne_bytes());
        payload.extend_from_slice(name);
        payload.push(0);
        payload.resize(16 + ((len + 3) & !3), 0);
        net_message(kind, &payload)
    }
    fn tc_message(index: u32) -> Vec<u8> {
        let mut payload = vec![0u8; 20];
        payload[4..8].copy_from_slice(&index.to_ne_bytes());
        net_message(36, &payload)
    }

    #[test]
    fn r4_owned_netlink_events_track_ifb_recreation_and_invalidate_loss() {
        // Pure parser fixture: no network socket or host network mutation.
        let null = File::open("/dev/null").unwrap();
        let mut events = SqmStartEvents {
            files: null.try_clone().unwrap().into(),
            network: null.into(),
            directories: vec![],
            watches: BTreeMap::new(),
            retired_watches: BTreeSet::new(),
            owned_files: Some(BTreeSet::new()),
            interfaces: BTreeSet::from(["wan0".into(), "ifb4wan0".into()]),
            indices: BTreeMap::from([(11, "wan0".into())]),
            sys_class_net: None,
        };
        assert!(!events
            .network_events(&link_message(16, 99, b"unrelated"))
            .unwrap());
        assert!(!events
            .network_events(&link_message(16, 99, &[255]))
            .unwrap());
        assert!(!events.network_events(&tc_message(99)).unwrap());
        assert!(events.network_events(&tc_message(11)).unwrap());
        assert!(events
            .network_events(&link_message(16, 22, b"ifb4wan0"))
            .unwrap());
        assert!(events.network_events(&tc_message(22)).unwrap());
        assert!(events
            .network_events(&link_message(16, 23, b"ifb4wan0"))
            .unwrap());
        assert!(!events.network_events(&tc_message(22)).unwrap());
        assert!(events.network_events(&tc_message(23)).unwrap());
        assert!(events
            .network_events(&link_message(17, 23, b"ifb4wan0"))
            .unwrap());
        assert!(!events
            .network_events(&link_message(16, 23, b"other"))
            .unwrap());
        assert!(!events.network_events(&tc_message(23)).unwrap());
        assert!(events
            .network_events(&link_message(16, 11, b"renamed-away"))
            .unwrap());
        assert!(!events.network_events(&tc_message(11)).unwrap());
        assert!(events.network_events(&net_message(4, &[])).unwrap());
        assert!(events.network_events(&net_message(48, &[0; 4])).unwrap());
        assert!(events.network_events(&tc_message(u32::MAX)).unwrap());
        assert!(events.network_events(&[0; 15]).is_err());
        let mut truncated = tc_message(99);
        truncated.pop();
        assert!(events.network_events(&truncated).is_err());
        let mut invalid_attr = link_message(16, 99, b"other");
        invalid_attr[32..34].copy_from_slice(&u16::MAX.to_ne_bytes());
        assert!(events.network_events(&invalid_attr).is_err());
        let mut both = link_message(16, 11, b"wan0");
        both.extend(tc_message(99));
        assert!(events.network_events(&both).unwrap());
    }

    #[test]
    fn r4_owned_file_events_ignore_foreign_writes_and_follow_directory_replacement() {
        let root = std::env::temp_dir().join(format!("sqm-owned-files-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let state = root.join("state");
        let config = root.join("config");
        fs::create_dir(&config).unwrap();
        let file = state.join("wan0.state");
        let mut events = SqmStartEvents::subscribe_owned(
            vec![file.clone(), config.join("sqm")],
            vec![],
            &root.join("sys"),
        )
        .unwrap();
        assert!(!events.drain().unwrap());
        fs::write(root.join("unrelated"), b"foreign").unwrap();
        assert!(!events.drain().unwrap());
        fs::create_dir(&state).unwrap();
        assert!(events.drain().unwrap());
        fs::write(state.join("other.state"), b"foreign").unwrap();
        fs::write(config.join("dhcp"), b"foreign").unwrap();
        assert!(!events.drain().unwrap());
        fs::write(&file, b"owned").unwrap();
        assert!(events.drain().unwrap());
        assert!(!events.drain().unwrap());
        fs::write(config.join("sqm"), b"source change").unwrap();
        assert!(events.drain().unwrap());
        fs::write(state.join("temporary"), b"replacement").unwrap();
        assert!(!events.drain().unwrap());
        fs::rename(state.join("temporary"), &file).unwrap();
        assert!(events.drain().unwrap());
        fs::rename(&state, root.join("old-state")).unwrap();
        assert!(events.drain().unwrap());
        fs::create_dir(&state).unwrap();
        assert!(events.drain().unwrap());
        fs::write(root.join("old-state/wan0.state"), b"no longer owned path").unwrap();
        assert!(!events.drain().unwrap());
        fs::write(&file, b"new owned path").unwrap();
        assert!(events.drain().unwrap());
        let mut overflow = vec![0u8; 16];
        overflow[0..4].copy_from_slice(&(-1i32).to_ne_bytes());
        overflow[4..8].copy_from_slice(&libc::IN_Q_OVERFLOW.to_ne_bytes());
        assert!(events.file_events(&overflow).unwrap());
        assert!(events.file_events(&[0; 3]).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn r4_owned_indices_refresh_after_a_lost_recreation_event() {
        let root = std::env::temp_dir().join(format!("sqm-owned-indices-{}", std::process::id()));
        fs::create_dir_all(root.join("ifb4wan0")).unwrap();
        let null = File::open("/dev/null").unwrap();
        let mut events = SqmStartEvents {
            files: null.try_clone().unwrap().into(),
            network: null.into(),
            directories: vec![],
            watches: BTreeMap::new(),
            retired_watches: BTreeSet::new(),
            owned_files: Some(BTreeSet::new()),
            interfaces: BTreeSet::from(["ifb4wan0".into()]),
            indices: BTreeMap::new(),
            sys_class_net: Some(root.clone()),
        };
        assert!(!events.refresh_indices().unwrap());
        fs::write(root.join("ifb4wan0/ifindex"), "22\n").unwrap();
        assert!(events.refresh_indices().unwrap());
        assert!(!events.refresh_indices().unwrap());
        fs::write(root.join("ifb4wan0/ifindex"), "23\n").unwrap();
        assert!(events.refresh_indices().unwrap());
        assert!(!events.indices.contains_key(&22));
        fs::remove_file(root.join("ifb4wan0/ifindex")).unwrap();
        assert!(events.refresh_indices().unwrap());
        assert!(events.indices.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

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
