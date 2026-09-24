//! Permanent probe group admission. A reservation is not a runtime lease.

use std::fs::{self, DirBuilder, File, Metadata, OpenOptions};
use std::io::{Read, Seek, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub(crate) const PROBE_GROUP_SLOTS: usize = 64;
const MAX_ACCOUNT_DATABASE_BYTES: usize = 256 * 1024;

/// Linux initial namespace inode identifiers (include/linux/proc_ns.h).
/// Unknown kernel layout is unsupported, never inferred from NS_GET_PARENT
/// EPERM, which also means an inaccessible parent. No container fallback.
pub(crate) fn attest_system_probe_namespace() -> Result<(), &'static str> {
    read_proc_mount_inventory()?;
    for (name, initial) in [("user", 0xefff_fffd_u64), ("pid", 0xefff_fffc_u64)] {
        let own = File::open(format!("/proc/self/ns/{name}"));
        let init = File::open(format!("/proc/1/ns/{name}"));
        let (own, init) = match (own, init) {
            (Ok(own), Ok(init)) => (own, init),
            // Linux omits these entries when the corresponding namespace
            // feature is compiled out; no nested namespace then exists.
            (Err(own), Err(init))
                if own.kind() == std::io::ErrorKind::NotFound
                    && init.kind() == std::io::ErrorKind::NotFound =>
            {
                continue
            }
            _ => return Err("probe-owner-namespace-unavailable"),
        };
        let own = own
            .metadata()
            .map_err(|_| "probe-owner-namespace-unavailable")?;
        let init = init
            .metadata()
            .map_err(|_| "probe-owner-namespace-unavailable")?;
        if own.ino() != initial || own.dev() != init.dev() || own.ino() != init.ino() {
            return Err("probe-owner-initial-namespace-required");
        }
    }
    if fs::read_link("/proc/self").map_err(|_| "probe-owner-namespace-unavailable")?
        != PathBuf::from(std::process::id().to_string())
    {
        return Err("probe-owner-proc-pid-namespace-mismatch");
    }
    let mut status = String::new();
    File::open("/proc/self/status")
        .map_err(|_| "probe-owner-credentials-unavailable")?
        .take(65537)
        .read_to_string(&mut status)
        .map_err(|_| "probe-owner-credentials-unavailable")?;
    if status.len() > 65536 {
        return Err("probe-owner-credentials-invalid");
    }
    let mut uid = status.lines().filter_map(|line| line.strip_prefix("Uid:"));
    let values = uid.next().ok_or("probe-owner-credentials-invalid")?;
    let mut values = values.split_ascii_whitespace();
    if uid.next().is_some() || (0..4).any(|_| values.next() != Some("0")) || values.next().is_some()
    {
        return Err("probe-owner-root-credentials-required");
    }
    Ok(())
}

pub(crate) fn prepare_boot_lease_root() -> Result<PathBuf, &'static str> {
    let mut boot = String::new();
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open("/proc/sys/kernel/random/boot_id")
        .map_err(|_| "probe-owner-boot-id-unavailable")?
        .take(128)
        .read_to_string(&mut boot)
        .map_err(|_| "probe-owner-boot-id-unavailable")?;
    prepare_boot_lease_root_at(
        Path::new("/tmp/cake-permanent-probes"),
        boot.trim_end_matches('\n'),
        0,
    )
}

fn prepare_boot_lease_root_at(base: &Path, boot: &str, uid: u32) -> Result<PathBuf, &'static str> {
    if boot.len() != 36
        || !boot.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()
            }
        })
    {
        return Err("probe-owner-boot-id-invalid");
    }
    fn open_private(path: &Path, uid: u32) -> Result<File, &'static str> {
        match DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err("probe-owner-boot-directory-unavailable"),
        }
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(path)
            .map_err(|_| "probe-owner-boot-directory-unsafe")?;
        let metadata = directory
            .metadata()
            .map_err(|_| "probe-owner-boot-directory-unavailable")?;
        if metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
            return Err("probe-owner-boot-directory-unsafe");
        }
        Ok(directory)
    }
    let parent = open_private(base, uid)?;
    let anchored = PathBuf::from(format!("/proc/self/fd/{}", parent.as_raw_fd())).join(boot);
    let directory = open_private(&anchored, uid)?;
    let path = base.join(boot);
    // Do not repair, chmod or delete a foreign/replaced directory. Existing
    // receipts for this boot survive restart; older boots remain untouched.
    for (opened, named) in [(&parent, base), (&directory, path.as_path())] {
        let actual = opened
            .metadata()
            .map_err(|_| "probe-owner-boot-directory-unavailable")?;
        let published =
            fs::symlink_metadata(named).map_err(|_| "probe-owner-boot-directory-changed")?;
        if !published.is_dir()
            || published.uid() != uid
            || published.mode() & 0o077 != 0
            || published.dev() != actual.dev()
            || published.ino() != actual.ino()
        {
            return Err("probe-owner-boot-directory-changed");
        }
    }
    Ok(path)
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ProbeGroupPool {
    gids: [u32; PROBE_GROUP_SLOTS],
}

/// The file lock fences live contenders. A nonempty receipt fences reuse after
/// process death; Drop intentionally closes only the lock, never the receipt.
pub(crate) struct ProbeGroupLease {
    directory: File,
    root: PathBuf,
    file: File,
    published: Metadata,
    name: String,
    gid: u32,
    owner_uid: u32,
}

fn valid_generation(generation: &str) -> bool {
    generation.len() == 64
        && generation
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Generation of an exact, well-formed receipt for this slot. Anything else
/// is unknown ownership and keeps the slot occupied.
fn stale_receipt_generation(receipt: &str, gid: u32) -> Option<&str> {
    let rest = receipt.strip_prefix("cake-probe-owner-v1\n")?;
    let (recorded, rest) = rest.split_once('\n')?;
    let generation = rest.strip_suffix('\n')?;
    (recorded == gid.to_string() && valid_generation(generation)).then_some(generation)
}

impl ProbeGroupLease {
    /// Acquire with no recovery authority: every receipt remains a fence.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn acquire(
        pool: &ProbeGroupPool,
        root: &Path,
        owner_uid: u32,
        generation: &str,
    ) -> Result<Self, &'static str> {
        Self::acquire_recovering(pool, root, owner_uid, generation, |_, _| {
            Err("probe-owner-recovery-unavailable".into())
        })
    }

    /// A locked slot with a nonempty receipt belonged to an owner whose lock
    /// was released without retirement: that process exited, or dropped the
    /// lease after a failed retirement. The slot is reused only when the
    /// receipt is exact, no task still carries the group, and `recover`
    /// settles that generation's owned kernel state (exact owner match).
    /// Every other outcome leaves the receipt as the crash fence.
    pub(crate) fn acquire_recovering(
        pool: &ProbeGroupPool,
        root: &Path,
        owner_uid: u32,
        generation: &str,
        mut recover: impl FnMut(u32, &str) -> Result<(), String>,
    ) -> Result<Self, &'static str> {
        if !valid_generation(generation) {
            return Err("probe-owner-generation-invalid");
        }
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(root)
            .map_err(|_| "probe-owner-lease-directory-unavailable")?;
        let meta = directory
            .metadata()
            .map_err(|_| "probe-owner-lease-directory-unavailable")?;
        if !meta.is_dir() || meta.uid() != owner_uid || meta.mode() & 0o077 != 0 {
            return Err("probe-owner-lease-directory-unsafe");
        }
        for gid in pool.gids {
            let name = format!("group-{gid}.lease");
            // Anchor lookup to the opened directory, not a replaceable prefix.
            let path =
                PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join(&name);
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)
                .map_err(|_| "probe-owner-lease-file-unavailable")?;
            match file.try_lock() {
                Ok(()) => {}
                Err(fs::TryLockError::WouldBlock) => continue,
                Err(_) => return Err("probe-owner-lease-lock-failed"),
            }
            let metadata = file
                .metadata()
                .map_err(|_| "probe-owner-lease-file-unavailable")?;
            if !metadata.is_file()
                || metadata.uid() != owner_uid
                || metadata.nlink() != 1
                || metadata.mode() & 0o077 != 0
            {
                return Err("probe-owner-lease-file-unsafe");
            }
            if metadata.len() != 0 {
                if metadata.len() > 128 {
                    continue;
                }
                let mut receipt = String::new();
                if file.read_to_string(&mut receipt).is_err() {
                    continue;
                }
                let Some(stale) = stale_receipt_generation(&receipt, gid) else {
                    continue;
                };
                if stale == generation || process_group_in_use(Path::new("/proc"), gid)? {
                    continue;
                }
                if recover(gid, stale).is_err() {
                    continue;
                }
                // Recovery removed only kernel state of the recorded generation.
                // Clear the receipt last; a crash before this line repeats the
                // idempotent recovery instead of reusing unsettled state.
                file.set_len(0)
                    .and_then(|()| file.sync_all())
                    .map_err(|_| "probe-owner-lease-recovery-failed")?;
            }
            // Account reservations and an empty lease do not exclude a live
            // task retaining this group from before reservation installation.
            // Check while holding the lease; errors are not proof of absence.
            if process_group_in_use(Path::new("/proc"), gid)? {
                continue;
            }
            let receipt = format!("cake-probe-owner-v1\n{gid}\n{generation}\n");
            file.seek(std::io::SeekFrom::Start(0))
                .map_err(|_| "probe-owner-lease-publish-failed")?;
            file.write_all(receipt.as_bytes())
                .and_then(|()| file.sync_all())
                .map_err(|_| "probe-owner-lease-publish-failed")?;
            let published = file
                .metadata()
                .map_err(|_| "probe-owner-lease-file-unavailable")?;
            let lease = Self {
                directory,
                root: root.to_path_buf(),
                file,
                published,
                name,
                gid,
                owner_uid,
            };
            lease.attest()?;
            return Ok(lease);
        }
        Err("probe-owner-pool-exhausted-or-unrecovered")
    }

    pub(crate) fn gid(&self) -> u32 {
        self.gid
    }

    /// Process inventory only; callers must also own the lifetime of any
    /// sockets/threads whose credentials may have been restored after creation.
    pub(crate) fn attest_processes_absent(&self) -> Result<(), &'static str> {
        self.attest()?;
        if process_group_in_use(Path::new("/proc"), self.gid)? {
            return Err("probe-owner-processes-still-live");
        }
        Ok(())
    }

    /// Caller must stop/reap all producers and settle owned rules/late flows.
    /// Failure leaves the receipt occupied for explicit recovery, never reuse.
    pub(crate) fn retire(
        self,
        cleanup: impl FnOnce() -> Result<(), &'static str>,
    ) -> Result<(), &'static str> {
        self.attest()?;
        // Do not remove routing guards while a producer still has this group.
        if process_group_in_use(Path::new("/proc"), self.gid)? {
            return Err("probe-owner-processes-still-live");
        }
        cleanup()?;
        // A successful cleanup callback is not proof that all producers were
        // reaped. Keep the crash fence if a process/thread still carries the
        // group or the inventory cannot prove absence. Rules/late sockets
        // remain the caller's additional cleanup obligation.
        if process_group_in_use(Path::new("/proc"), self.gid)? {
            return Err("probe-owner-processes-still-live");
        }
        self.attest()?;
        self.file
            .set_len(0)
            .and_then(|()| self.file.sync_all())
            .map_err(|_| "probe-owner-lease-retire-failed")
    }

    fn attest(&self) -> Result<(), &'static str> {
        let root =
            fs::symlink_metadata(&self.root).map_err(|_| "probe-owner-lease-directory-changed")?;
        let opened = self
            .directory
            .metadata()
            .map_err(|_| "probe-owner-lease-directory-changed")?;
        if !root.is_dir()
            || root.uid() != self.owner_uid
            || root.mode() & 0o077 != 0
            || root.dev() != opened.dev()
            || root.ino() != opened.ino()
        {
            return Err("probe-owner-lease-directory-changed");
        }
        let file = self
            .file
            .metadata()
            .map_err(|_| "probe-owner-lease-file-changed")?;
        let named = fs::symlink_metadata(self.root.join(&self.name))
            .map_err(|_| "probe-owner-lease-file-changed")?;
        if !same_file(&self.published, &file)
            || file.uid() != self.owner_uid
            || file.mode() & 0o077 != 0
            || file.nlink() != 1
            || !same_file(&file, &named)
        {
            return Err("probe-owner-lease-file-changed");
        }
        Ok(())
    }
}

/// A process inventory is an additional admission check, not stale-socket or
/// nft cleanup proof. Persistent receipts still fence crashed probe owners.
fn process_group_in_use(proc_root: &Path, gid: u32) -> Result<bool, &'static str> {
    let mount_snapshot = if proc_root == Path::new("/proc") {
        Some(read_proc_mount_inventory()?)
    } else {
        None
    };
    const MAX_TASKS: usize = 65536;
    const MAX_STATUS_BYTES: u64 = 1024 * 1024;
    let processes =
        fs::read_dir(proc_root).map_err(|_| "probe-owner-process-inventory-unavailable")?;
    let mut count = 0;
    for process in processes {
        let process = process.map_err(|_| "probe-owner-process-inventory-unavailable")?;
        let name = process.file_name();
        let Some(name) = name
            .to_str()
            .filter(|name| !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()))
        else {
            continue;
        };
        let tasks = match fs::read_dir(proc_root.join(name).join("task")) {
            Ok(tasks) => tasks,
            Err(error) if process_disappeared(&error) => continue,
            Err(_) => return Err("probe-owner-process-inventory-unavailable"),
        };
        for task in tasks {
            let task = task.map_err(|_| "probe-owner-process-inventory-unavailable")?;
            count += 1;
            if count > MAX_TASKS {
                return Err("probe-owner-process-inventory-too-large");
            }
            let file = match OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(task.path().join("status"))
            {
                Ok(file) => file,
                Err(error) if process_disappeared(&error) => continue,
                Err(_) => return Err("probe-owner-process-status-unavailable"),
            };
            let mut status = String::new();
            match file.take(MAX_STATUS_BYTES + 1).read_to_string(&mut status) {
                Ok(_) => {}
                Err(error) if process_disappeared(&error) => continue,
                Err(_) => return Err("probe-owner-process-status-unavailable"),
            }
            if status.len() > MAX_STATUS_BYTES as usize {
                return Err("probe-owner-process-status-too-large");
            }
            if status_has_group(&status, gid)? {
                return Ok(true);
            }
        }
    }
    if let Some(before) = mount_snapshot {
        if before != read_proc_mount_inventory()? {
            return Err("probe-owner-proc-mount-changed");
        }
    }
    Ok(false)
}

fn read_proc_mount_inventory() -> Result<(u64, String), &'static str> {
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open("/proc")
        .map_err(|_| "probe-owner-proc-mount-unavailable")?;
    let mut fdinfo = String::new();
    File::open(format!("/proc/self/fdinfo/{}", directory.as_raw_fd()))
        .map_err(|_| "probe-owner-proc-mount-unavailable")?
        .take(4097)
        .read_to_string(&mut fdinfo)
        .map_err(|_| "probe-owner-proc-mount-unavailable")?;
    if fdinfo.len() > 4096 {
        return Err("probe-owner-proc-mount-invalid");
    }
    let mut mount_id = None;
    for line in fdinfo.lines() {
        if let Some(value) = line.strip_prefix("mnt_id:") {
            let value = value
                .trim()
                .parse::<u64>()
                .ok()
                .filter(|id| *id != 0)
                .ok_or("probe-owner-proc-mount-invalid")?;
            if mount_id.replace(value).is_some() {
                return Err("probe-owner-proc-mount-invalid");
            }
        }
    }
    let mount_id = mount_id.ok_or("probe-owner-proc-mount-invalid")?;
    let mut mounts = String::new();
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open("/proc/self/mountinfo")
        .map_err(|_| "probe-owner-proc-mount-unavailable")?
        .take(256 * 1024 + 1)
        .read_to_string(&mut mounts)
        .map_err(|_| "probe-owner-proc-mount-unavailable")?;
    attest_proc_mount_inventory(mount_id, &mounts)?;
    Ok((mount_id, mounts))
}

// This excludes mount-level filtering, not a restricted PID/user namespace.
// Namespace admission is a separate prerequisite for the production owner.
fn attest_proc_mount_inventory(mount_id: u64, mounts: &str) -> Result<(), &'static str> {
    if mounts.len() > 256 * 1024 {
        return Err("probe-owner-proc-mount-too-large");
    }
    let mut found = false;
    for line in mounts.lines() {
        let (left, right) = line
            .split_once(" - ")
            .ok_or("probe-owner-proc-mount-invalid")?;
        let mut fields = left.split_ascii_whitespace();
        let id = fields
            .next()
            .and_then(|id| id.parse::<u64>().ok())
            .ok_or("probe-owner-proc-mount-invalid")?;
        let root = fields.nth(2).ok_or("probe-owner-proc-mount-invalid")?;
        let target = fields.next().ok_or("probe-owner-proc-mount-invalid")?;
        let options = fields.next().ok_or("probe-owner-proc-mount-invalid")?;
        if let Some(subpath) = target.strip_prefix("/proc/") {
            let first = subpath.split('/').next().unwrap_or_default();
            if matches!(first, "self" | "thread-self")
                || first.bytes().all(|byte| byte.is_ascii_digit())
                || first.contains('\\')
            {
                return Err("probe-owner-proc-process-view-overmounted");
            }
        }
        if target != "/proc" || id != mount_id {
            continue;
        }
        let mut super_fields = right.split_ascii_whitespace();
        let kind = super_fields.next();
        let _source = super_fields.next();
        let super_options = super_fields
            .next()
            .ok_or("probe-owner-proc-mount-invalid")?;
        if found || root != "/" || kind != Some("proc") {
            return Err("probe-owner-proc-mount-invalid");
        }
        for option in options.split(',').chain(super_options.split(',')) {
            if option
                .strip_prefix("hidepid=")
                .is_some_and(|mode| !matches!(mode, "0" | "off"))
            {
                return Err("probe-owner-proc-process-view-filtered");
            }
        }
        found = true;
    }
    if !found {
        return Err("probe-owner-proc-mount-missing");
    }
    Ok(())
}

fn process_disappeared(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ESRCH))
}

fn status_has_group(status: &str, gid: u32) -> Result<bool, &'static str> {
    let mut saw_gid = false;
    let mut saw_groups = false;
    let mut found = false;
    for line in status.lines() {
        let (values, primary) = if let Some(values) = line.strip_prefix("Gid:") {
            if saw_gid {
                return Err("probe-owner-process-status-invalid");
            }
            saw_gid = true;
            (values, true)
        } else if let Some(values) = line.strip_prefix("Groups:") {
            if saw_groups {
                return Err("probe-owner-process-status-invalid");
            }
            saw_groups = true;
            (values, false)
        } else {
            continue;
        };
        let mut count = 0;
        for value in values.split_whitespace() {
            let parsed = account_id(value).map_err(|_| "probe-owner-process-status-invalid")?;
            found |= parsed == gid;
            count += 1;
        }
        if primary && count != 4 {
            return Err("probe-owner-process-status-invalid");
        }
    }
    if !saw_gid || !saw_groups {
        return Err("probe-owner-process-status-invalid");
    }
    Ok(found)
}

impl ProbeGroupPool {
    /// Runtime callers use /etc and uid 0. Alternate roots/owners are supported
    /// for offline installation tests, never taken from user configuration.
    pub(crate) fn read_at(directory: &Path, owner_uid: u32) -> Result<Self, &'static str> {
        let directory_before = fs::symlink_metadata(directory)
            .map_err(|_| "probe-owner-account-directory-unavailable")?;
        if !directory_before.is_dir()
            || directory_before.uid() != owner_uid
            || directory_before.mode() & 0o022 != 0
        {
            return Err("probe-owner-account-directory-unsafe");
        }
        let mut groups = AccountSnapshot::open(&directory.join("group"), owner_uid)?;
        let mut users = AccountSnapshot::open(&directory.join("passwd"), owner_uid)?;
        let group_text = groups.read()?;
        let user_text = users.read()?;
        let pool = Self::from_account_text(&group_text, &user_text)?;
        groups.attest(&directory.join("group"))?;
        users.attest(&directory.join("passwd"))?;
        let directory_after = fs::symlink_metadata(directory)
            .map_err(|_| "probe-owner-account-directory-unavailable")?;
        if !same_file(&directory_before, &directory_after) {
            return Err("probe-owner-account-directory-changed");
        }
        Ok(pool)
    }

    /// Validate the entire reserved pool before assigning any socket owner.
    /// File metadata, concurrent leases and surviving producers are separately
    /// attested by the runtime; this pure parser cannot prove exclusivity.
    pub(crate) fn from_account_text(groups: &str, users: &str) -> Result<Self, &'static str> {
        if groups.len() > MAX_ACCOUNT_DATABASE_BYTES || users.len() > MAX_ACCOUNT_DATABASE_BYTES {
            return Err("probe-owner-account-database-too-large");
        }
        let mut gids = [0; PROBE_GROUP_SLOTS];
        for line in groups.lines().filter(|line| !line.is_empty()) {
            let fields = group_fields(line)?;
            let Some(suffix) = fields[0].strip_prefix("cake-probe-") else {
                continue;
            };
            if suffix.len() != 2 || !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err("probe-owner-reservation-name-invalid");
            }
            let slot = suffix
                .parse::<usize>()
                .map_err(|_| "probe-owner-reservation-name-invalid")?;
            if slot >= PROBE_GROUP_SLOTS || gids[slot] != 0 || !fields[3].is_empty() {
                return Err("probe-owner-reservation-conflict");
            }
            let gid = account_id(fields[2])?;
            if gid == 0 || gid == u32::MAX || gids.contains(&gid) {
                return Err("probe-owner-reservation-conflict");
            }
            gids[slot] = gid;
        }
        if gids.contains(&0) {
            return Err("probe-owner-reservation-missing");
        }
        // A second name for a reserved GID can admit unrelated socket owners.
        for line in groups.lines().filter(|line| !line.is_empty()) {
            let fields = group_fields(line)?;
            let gid = account_id(fields[2])?;
            if gids.contains(&gid) && !fields[0].starts_with("cake-probe-") {
                return Err("probe-owner-reservation-aliased");
            }
        }
        for line in users.lines().filter(|line| !line.is_empty()) {
            let fields: Vec<_> = line.split(':').collect();
            if fields.len() != 7 || fields[0].is_empty() {
                return Err("probe-owner-account-database-invalid");
            }
            account_id(fields[2])?;
            if gids.contains(&account_id(fields[3])?) {
                return Err("probe-owner-reservation-primary-group");
            }
        }
        Ok(Self { gids })
    }

    pub(crate) fn gid(&self, slot: usize) -> Option<u32> {
        self.gids.get(slot).copied()
    }
}

struct AccountSnapshot {
    file: File,
    metadata: Metadata,
}

impl AccountSnapshot {
    fn open(path: &Path, owner_uid: u32) -> Result<Self, &'static str> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| "probe-owner-account-file-unavailable")?;
        let metadata = file
            .metadata()
            .map_err(|_| "probe-owner-account-file-unavailable")?;
        if !metadata.is_file()
            || metadata.uid() != owner_uid
            || metadata.nlink() != 1
            || metadata.mode() & 0o022 != 0
            || metadata.len() > MAX_ACCOUNT_DATABASE_BYTES as u64
        {
            return Err("probe-owner-account-file-unsafe");
        }
        Ok(Self { file, metadata })
    }

    fn read(&mut self) -> Result<String, &'static str> {
        let mut text = String::new();
        (&mut self.file)
            .take(MAX_ACCOUNT_DATABASE_BYTES as u64 + 1)
            .read_to_string(&mut text)
            .map_err(|_| "probe-owner-account-file-read-failed")?;
        if text.len() > MAX_ACCOUNT_DATABASE_BYTES {
            return Err("probe-owner-account-database-too-large");
        }
        Ok(text)
    }

    fn attest(&self, path: &Path) -> Result<(), &'static str> {
        let opened = self
            .file
            .metadata()
            .map_err(|_| "probe-owner-account-file-unavailable")?;
        let named =
            fs::symlink_metadata(path).map_err(|_| "probe-owner-account-file-unavailable")?;
        if !same_file(&self.metadata, &opened) || !same_file(&opened, &named) {
            return Err("probe-owner-account-file-changed");
        }
        Ok(())
    }
}

fn same_file(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
        && a.nlink() == b.nlink()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}

fn group_fields(line: &str) -> Result<[&str; 4], &'static str> {
    let mut fields = line.split(':');
    let row = [fields.next(), fields.next(), fields.next(), fields.next()];
    match row {
        [Some(name), Some(password), Some(gid), Some(members)]
            if !name.is_empty() && fields.next().is_none() =>
        {
            Ok([name, password, gid, members])
        }
        _ => Err("probe-owner-account-database-invalid"),
    }
}

fn account_id(value: &str) -> Result<u32, &'static str> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("probe-owner-account-id-invalid");
    }
    value.parse().map_err(|_| "probe-owner-account-id-invalid")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "read-only positive admission on a root initial-namespace Linux test VM"]
    fn r6_system_namespace_host_read_only() {
        super::attest_system_probe_namespace().unwrap();
    }

    #[test]
    #[ignore = "requires private user/mount/PID namespaces and private mounted procfs"]
    fn r6_proc_mount_visibility_kernel_fixture() {
        use std::process::Command;
        for (name, variable) in [
            ("user", "CAKE_R6_PARENT_USERNS"),
            ("mnt", "CAKE_R6_PARENT_MNTNS"),
            ("pid", "CAKE_R6_PARENT_PIDNS"),
        ] {
            assert_ne!(
                std::fs::read_link(format!("/proc/self/ns/{name}"))
                    .unwrap()
                    .to_str()
                    .unwrap(),
                std::env::var(variable).unwrap()
            );
        }
        assert_eq!(std::process::id(), 1, "fixture must own its PID namespace");
        assert_eq!(
            super::attest_system_probe_namespace(),
            Err("probe-owner-initial-namespace-required")
        );
        let mount = |arguments: &[&str]| {
            assert!(Command::new("mount")
                .args(arguments)
                .status()
                .unwrap()
                .success());
        };
        assert!(!super::process_group_in_use(std::path::Path::new("/proc"), 424242).unwrap());
        mount(&["-o", "remount,hidepid=2", "/proc"]);
        assert_eq!(
            super::process_group_in_use(std::path::Path::new("/proc"), 424242),
            Err("probe-owner-proc-process-view-filtered")
        );
        mount(&["-o", "remount,hidepid=0", "/proc"]);
        assert!(!super::process_group_in_use(std::path::Path::new("/proc"), 424242).unwrap());
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("cake-proc-cover-{unique}"));
        std::fs::create_dir(&directory).unwrap();
        let target = format!("/proc/{}", child.id());
        mount(&["--bind", directory.to_str().unwrap(), &target]);
        assert_eq!(
            super::process_group_in_use(std::path::Path::new("/proc"), 424242),
            Err("probe-owner-proc-process-view-overmounted")
        );
        assert!(Command::new("umount")
            .arg(&target)
            .status()
            .unwrap()
            .success());
        child.kill().unwrap();
        child.wait().unwrap();
        std::fs::remove_dir(directory).unwrap();
        assert!(!super::process_group_in_use(std::path::Path::new("/proc"), 424242).unwrap());
    }

    #[test]
    fn r6_proc_inventory_rejects_filtered_or_overmounted_process_views() {
        let valid = "23 1 0:4 / /proc rw,nosuid,nodev,noexec - proc proc rw\n";
        assert!(super::attest_proc_mount_inventory(23, valid).is_ok());
        assert!(super::attest_proc_mount_inventory(
            23,
            &valid.replace("proc rw", "proc rw,hidepid=0")
        )
        .is_ok());
        // Static sysctl overlays do not conceal process directories.
        assert!(super::attest_proc_mount_inventory(
            23,
            &format!("{valid}24 23 0:4 /sys /proc/sys ro - proc proc rw\n")
        )
        .is_ok());
        for mode in [
            "1",
            "2",
            "4",
            "invisible",
            "noaccess",
            "ptraceable",
            "unknown",
        ] {
            assert!(super::attest_proc_mount_inventory(
                23,
                &valid.replace("proc rw", &format!("proc rw,hidepid={mode}"))
            )
            .is_err());
            assert!(super::attest_proc_mount_inventory(
                23,
                &valid.replace("rw,nosuid", &format!("rw,hidepid={mode},nosuid"))
            )
            .is_err());
        }
        for path in [
            "/proc/123",
            "/proc/123/task",
            "/proc/self",
            "/proc/thread-self/status",
            "/proc/\\06123",
        ] {
            assert!(super::attest_proc_mount_inventory(
                23,
                &format!("{valid}24 23 0:5 / {path} ro - tmpfs tmpfs rw\n")
            )
            .is_err());
        }
        for bad in [
            String::new(),
            "malformed".into(),
            format!("{valid}{valid}"),
            valid.replace(" / /proc ", " /partial /proc "),
            valid.replace(" - proc ", " - tmpfs "),
        ] {
            assert!(super::attest_proc_mount_inventory(23, &bad).is_err());
        }
        assert!(super::attest_proc_mount_inventory(23, &" ".repeat(256 * 1024 + 1)).is_err());
        let stacked = format!("{valid}24 23 0:5 / /proc rw - proc proc rw,hidepid=2\n");
        assert!(super::attest_proc_mount_inventory(23, &stacked).is_ok());
        assert!(super::attest_proc_mount_inventory(24, &stacked).is_err());
        assert!(super::attest_proc_mount_inventory(25, &stacked).is_err());
    }

    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn r6_boot_lease_directory_preserves_restart_receipts_and_rejects_foreign_paths() {
        use std::os::unix::fs::symlink;
        let root =
            std::env::temp_dir().join(format!("cake-probe-boot-root-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let uid = fs::metadata(&root).unwrap().uid();
        let base = root.join("owners");
        for invalid in [
            "",
            "../escape",
            "AAAAAAAA-0000-0000-0000-000000000000",
            "00000000-0000-0000-0000-000000000000/child",
        ] {
            assert!(prepare_boot_lease_root_at(&base, invalid, uid).is_err());
            assert!(!base.exists());
        }
        let boot = "00000000-0000-0000-0000-000000000001";
        let current = prepare_boot_lease_root_at(&base, boot, uid).unwrap();
        fs::write(current.join("occupied.lease"), b"retain crashed owner").unwrap();
        assert_eq!(
            prepare_boot_lease_root_at(&base, boot, uid).unwrap(),
            current
        );
        assert_eq!(
            fs::read(current.join("occupied.lease")).unwrap(),
            b"retain crashed owner"
        );
        let next =
            prepare_boot_lease_root_at(&base, "00000000-0000-0000-0000-000000000002", uid).unwrap();
        assert_eq!(fs::read_dir(&next).unwrap().count(), 0);
        assert!(current.join("occupied.lease").exists());
        assert!(prepare_boot_lease_root_at(&base, boot, uid.wrapping_add(1)).is_err());
        fs::set_permissions(&base, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(prepare_boot_lease_root_at(&base, boot, uid).is_err());
        assert_eq!(fs::metadata(&base).unwrap().mode() & 0o777, 0o755);
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();
        fs::rename(&current, root.join("saved")).unwrap();
        symlink(root.join("saved"), &current).unwrap();
        assert!(prepare_boot_lease_root_at(&base, boot, uid).is_err());
        assert!(fs::symlink_metadata(&current)
            .unwrap()
            .file_type()
            .is_symlink());
        let alias = root.join("alias");
        symlink(&base, &alias).unwrap();
        assert!(prepare_boot_lease_root_at(&alias, boot, uid).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn r6_crashed_receipt_is_reused_only_after_exact_generation_recovery() {
        let root = std::env::temp_dir().join(format!("cake-probe-recover-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let uid = fs::metadata(&root).unwrap().uid();
        // Group IDs far outside ordinary allocation; /proc proves them unused.
        let mut gids = [0_u32; PROBE_GROUP_SLOTS];
        for (slot, gid) in gids.iter_mut().enumerate() {
            *gid = 3_900_000_000 + slot as u32;
            assert!(!process_group_in_use(Path::new("/proc"), *gid).unwrap());
        }
        let pool = ProbeGroupPool { gids };
        let write = |gid: u32, text: &str| {
            let path = root.join(format!("group-{gid}.lease"));
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)
                .unwrap();
            file.write_all(text.as_bytes()).unwrap();
            path
        };
        let receipt =
            |gid: u32, generation: &str| format!("cake-probe-owner-v1\n{gid}\n{generation}\n");
        let crashed = write(gids[0], &receipt(gids[0], &"b".repeat(64)));
        let malformed = write(gids[1], "cake-probe-owner-v1\nforeign\n");
        let wrong_slot = write(gids[2], &receipt(gids[9], &"d".repeat(64)));
        let held = write(gids[3], &receipt(gids[3], &"e".repeat(64)));
        let holder = OpenOptions::new().read(true).open(&held).unwrap();
        holder.try_lock().unwrap();

        // A failed recovery keeps the crash fence and moves to a free slot.
        let mut calls = Vec::new();
        let lease = ProbeGroupLease::acquire_recovering(
            &pool,
            &root,
            uid,
            &"a".repeat(64),
            |gid, stale| {
                calls.push((gid, stale.to_string()));
                Err("fixture-table-still-foreign".into())
            },
        )
        .unwrap();
        assert_eq!(calls, vec![(gids[0], "b".repeat(64))]);
        assert_eq!(lease.gid(), gids[4]);
        assert_eq!(
            fs::read_to_string(&crashed).unwrap(),
            receipt(gids[0], &"b".repeat(64))
        );
        lease.retire(|| Ok(())).unwrap();

        // Successful exact recovery reuses the slot with a new receipt.
        calls.clear();
        let lease = ProbeGroupLease::acquire_recovering(
            &pool,
            &root,
            uid,
            &"c".repeat(64),
            |gid, stale| {
                calls.push((gid, stale.to_string()));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(calls, vec![(gids[0], "b".repeat(64))]);
        assert_eq!(lease.gid(), gids[0]);
        assert_eq!(
            fs::read_to_string(&crashed).unwrap(),
            receipt(gids[0], &"c".repeat(64))
        );
        // Unknown, misplaced and live-locked receipts were never offered.
        assert_eq!(
            fs::read_to_string(&malformed).unwrap(),
            "cake-probe-owner-v1\nforeign\n"
        );
        assert_eq!(
            fs::read_to_string(&wrong_slot).unwrap(),
            receipt(gids[9], &"d".repeat(64))
        );
        assert_eq!(
            fs::read_to_string(&held).unwrap(),
            receipt(gids[3], &"e".repeat(64))
        );
        drop(holder);
        drop(lease);

        // The dropped (unretired) lease is itself a crash fence for its own
        // generation until another owner settles it.
        calls.clear();
        let next = ProbeGroupLease::acquire_recovering(
            &pool,
            &root,
            uid,
            &"f".repeat(64),
            |gid, stale| {
                calls.push((gid, stale.to_string()));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(calls, vec![(gids[0], "c".repeat(64))]);
        assert_eq!(next.gid(), gids[0]);
        drop(next);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn r6_retirement_keeps_receipt_when_cleanup_leaves_a_live_group() {
        let root =
            std::env::temp_dir().join(format!("cake-probe-retire-live-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        // Model an already-acquired lease whose producer is still alive.
        // The test's own filesystem GID supplies real /proc evidence without
        // changing credentials or spawning an unbounded child.
        let metadata = fs::metadata(&root).unwrap();
        let gid = metadata.gid();
        assert!(process_group_in_use(Path::new("/proc"), gid).unwrap());
        let name = format!("group-{gid}.lease");
        let path = root.join(&name);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.try_lock().unwrap();
        let receipt = format!("cake-probe-owner-v1\n{gid}\n{}\n", "a".repeat(64));
        file.write_all(receipt.as_bytes()).unwrap();
        file.sync_all().unwrap();
        let lease = ProbeGroupLease {
            directory: File::open(&root).unwrap(),
            root: root.clone(),
            published: file.metadata().unwrap(),
            file,
            name,
            gid,
            owner_uid: metadata.uid(),
        };
        let mut cleaned = false;
        let result = lease.retire(|| {
            cleaned = true;
            Ok(())
        });
        assert!(!cleaned, "live producers must retain their routing guards");
        assert_eq!(result, Err("probe-owner-processes-still-live"));
        assert_eq!(fs::read(&path).unwrap(), receipt.as_bytes());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn r6_process_inventory_checks_thread_fs_and_supplementary_groups() {
        for status in [
            "Gid:\t1 2 3 40000\nGroups:\n",
            "Gid: 1 2 3 4\nGroups: 5 40000\n",
        ] {
            assert_eq!(status_has_group(status, 40000), Ok(true));
            assert_eq!(status_has_group(status, 40001), Ok(false));
        }
        for invalid in [
            "",
            "Gid: 1 2 3\nGroups:\n",
            "Gid: 1 2 3 4\n",
            "Gid: 1 2 3 4\nGroups: -1\n",
            "Gid: 1 2 3 4\nGroups:\nGroups:\n",
        ] {
            assert!(status_has_group(invalid, 40000).is_err());
        }
        let root = std::env::temp_dir().join(format!("cake-probe-proc-{}", std::process::id()));
        fs::create_dir_all(root.join("123/task/123")).unwrap();
        fs::create_dir_all(root.join("123/task/124")).unwrap();
        fs::write(root.join("123/task/123/status"), "Gid: 1 1 1 1\nGroups:\n").unwrap();
        fs::write(
            root.join("123/task/124/status"),
            "Gid: 1 1 1 40000\nGroups:\n",
        )
        .unwrap();
        assert_eq!(process_group_in_use(&root, 40000), Ok(true));
        fs::remove_file(root.join("123/task/124/status")).unwrap();
        assert_eq!(process_group_in_use(&root, 40000), Ok(false));
        fs::write(root.join("123/task/123/status"), "malformed").unwrap();
        assert!(process_group_in_use(&root, 40000).is_err());
        fs::remove_dir_all(&root).unwrap();
        assert!(process_group_in_use(&root, 40000).is_err());
        assert!(process_disappeared(&std::io::Error::from_raw_os_error(
            libc::ESRCH
        )));
        assert!(!process_disappeared(&std::io::Error::from_raw_os_error(
            libc::EACCES
        )));
    }

    #[test]
    fn r6_account_snapshot_rejects_replacement_and_in_place_change() {
        let root = std::env::temp_dir().join(format!("cake-probe-snapshot-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let path = root.join("group");
        fs::write(&path, "root:x:0:\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let uid = fs::metadata(&path).unwrap().uid();
        let mut snapshot = AccountSnapshot::open(&path, uid).unwrap();
        assert_eq!(snapshot.read().unwrap(), "root:x:0:\n");
        snapshot.attest(&path).unwrap();
        fs::rename(&path, root.join("old")).unwrap();
        fs::write(&path, "root:x:0:\n").unwrap();
        assert!(snapshot.attest(&path).is_err());
        let snapshot = AccountSnapshot::open(&path, uid).unwrap();
        fs::write(&path, "root:x:0:\nchanged:x:12:\n").unwrap();
        assert!(snapshot.attest(&path).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
