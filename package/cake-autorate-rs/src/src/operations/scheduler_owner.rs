//! Process-wide and cross-process ownership for the native calibration scheduler.

use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

pub(crate) const PRODUCTION_SCHEDULER_OWNER_LOCK: &str =
    "/var/run/cake-autorate-scheduler-owner.lock";

pub(crate) struct SchedulerOwnerLock {
    lock: Option<File>,
    lock_key: (u64, u64),
}

impl SchedulerOwnerLock {
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        let mut locks = local_locks()
            .lock()
            .map_err(|_| "scheduler owner lock registry is poisoned".to_string())?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)
            .map_err(|error| format!("unable to open scheduler owner lock: {error}"))?;
        let metadata = lock
            .metadata()
            .map_err(|error| format!("unable to inspect scheduler owner lock: {error}"))?;
        validate_owner_lock(path, &metadata)?;
        let key = (metadata.dev(), metadata.ino());
        if locks.contains(&key) {
            return Err("scheduler ownership is already held in this process".to_string());
        }
        let status = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if status != 0 {
            return Err(format!(
                "unable to lock scheduler ownership: {}",
                io::Error::last_os_error()
            ));
        }
        locks.insert(key);
        drop(locks);
        Ok(Self {
            lock: Some(lock),
            lock_key: key,
        })
    }
}

impl Drop for SchedulerOwnerLock {
    fn drop(&mut self) {
        if let Some(lock) = self.lock.take() {
            let _ = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) };
            drop(lock);
        }
        if let Ok(mut locks) = local_locks().lock() {
            locks.remove(&self.lock_key);
        }
    }
}

pub(crate) fn attest_scheduler_owner_lock_held(path: &Path) -> Result<(), String> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("unable to open scheduler owner lock: {error}"))?;
    let metadata = lock
        .metadata()
        .map_err(|error| format!("unable to inspect scheduler owner lock: {error}"))?;
    validate_owner_lock(path, &metadata)?;
    let status = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if status == 0 {
        let _ = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) };
        return Err("native scheduler ownership lock is not held".to_string());
    }
    let error = io::Error::last_os_error();
    if error.kind() != io::ErrorKind::WouldBlock {
        return Err(format!("unable to attest scheduler owner lock: {error}"));
    }
    Ok(())
}

fn validate_owner_lock(path: &Path, metadata: &std::fs::Metadata) -> Result<(), String> {
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600
        || metadata.nlink() != 1
        || metadata.len() != 0
    {
        return Err(format!(
            "{} is not an exact native scheduler owner lock",
            path.display()
        ));
    }
    Ok(())
}

fn local_locks() -> &'static Mutex<BTreeSet<(u64, u64)>> {
    static LOCKS: OnceLock<Mutex<BTreeSet<(u64, u64)>>> = OnceLock::new();
    LOCKS.get_or_init(|| Mutex::new(BTreeSet::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQUENCE: AtomicU64 = AtomicU64::new(1);

    fn root() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "cake-autorate-native-scheduler-owner-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn owner_lock_is_exact_private_and_process_exclusive() {
        let root = root();
        let path = root.join("owner.lock");
        let owner = SchedulerOwnerLock::open(&path).unwrap();
        attest_scheduler_owner_lock_held(&path).unwrap();
        assert!(SchedulerOwnerLock::open(&path).is_err());
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        drop(owner);
        assert!(attest_scheduler_owner_lock_held(&path)
            .unwrap_err()
            .contains("not held"));
        assert!(SchedulerOwnerLock::open(&path).is_ok());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(SchedulerOwnerLock::open(&path).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn owner_lock_rejects_symlinks_and_nonempty_files() {
        let root = root();
        let target = root.join("target");
        fs::write(&target, b"x").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(SchedulerOwnerLock::open(&target).is_err());
        let link = root.join("link");
        symlink(&target, &link).unwrap();
        assert!(SchedulerOwnerLock::open(&link).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
