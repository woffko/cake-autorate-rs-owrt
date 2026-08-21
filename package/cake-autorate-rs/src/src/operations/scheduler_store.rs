//! Private durable state owned by the native scheduler.
//!
//! The store is intentionally independent from the coordinator journal. A
//! scheduled traffic reservation must survive coordinator death and reboot,
//! while a second scheduler owner must fail before it can inspect or mutate
//! any record. Production wiring remains disabled until this store and the
//! scheduler admission path pass an isolated OpenWrt gate.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use super::scheduler::{validate_scheduler_instance, SchedulerInstanceState};

pub(crate) const PRODUCTION_SCHEDULER_STORE_ROOT: &str = "/etc/cake-autorate-rs-scheduler";
const LOCK_FILE: &str = ".scheduler.lock";
const STATE_PREFIX: &str = "instance-";
const RECORD_SUFFIX: &str = ".state";
const TEMP_PREFIX: &str = ".scheduler-write.";
const TEMP_SUFFIX: &str = ".tmp";
const MAX_RECORD_BYTES: usize = 16 * 1024;

pub struct SchedulerStore {
    root: PathBuf,
    lock: Option<File>,
    lock_key: (u64, u64),
    write_sequence: AtomicU32,
}

impl SchedulerStore {
    pub fn open(root: &Path) -> Result<Self, String> {
        ensure_private_directory(root)?;
        let lock_path = root.join(LOCK_FILE);
        let mut local_locks = local_lock_registry()
            .lock()
            .map_err(|_| "native scheduler local lock registry is poisoned".to_string())?;
        if let Ok(metadata) = fs::symlink_metadata(&lock_path) {
            let key = (metadata.dev(), metadata.ino());
            if local_locks.contains(&key) {
                return Err("native scheduler store is owned by another process".to_string());
            }
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&lock_path)
            .map_err(|error| format!("unable to open native scheduler lock: {error}"))?;
        let metadata = lock
            .metadata()
            .map_err(|error| format!("unable to inspect native scheduler lock: {error}"))?;
        validate_private_file(&lock_path, &metadata)?;
        let lock_key = (metadata.dev(), metadata.ino());
        if local_locks.contains(&lock_key) {
            return Err("native scheduler store is owned by another process".to_string());
        }
        let mut record_lock = libc::flock {
            l_type: libc::F_WRLCK as libc::c_short,
            l_whence: libc::SEEK_SET as libc::c_short,
            l_start: 0,
            l_len: 0,
            l_pid: 0,
        };
        let status = unsafe {
            libc::fcntl(
                lock.as_raw_fd(),
                libc::F_SETLK,
                &mut record_lock as *mut libc::flock,
            )
        };
        if status != 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                return Err("native scheduler store is owned by another process".to_string());
            }
            return Err(format!("unable to lock native scheduler store: {error}"));
        }
        local_locks.insert(lock_key);
        drop(local_locks);
        if let Err(error) = clean_and_validate_entries(root) {
            drop(lock);
            if let Ok(mut locks) = local_lock_registry().lock() {
                locks.remove(&lock_key);
            }
            return Err(error);
        }
        Ok(Self {
            root: root.to_path_buf(),
            lock: Some(lock),
            lock_key,
            write_sequence: AtomicU32::new(1),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn load_state(&self, instance: &str) -> Result<Option<SchedulerInstanceState>, String> {
        let path = self.record_path(STATE_PREFIX, instance)?;
        read_optional_record(&path, SchedulerInstanceState::decode).and_then(|record| {
            if record
                .as_ref()
                .is_some_and(|state| state.cursor.instance != instance)
            {
                return Err("scheduler state filename and identity differ".to_string());
            }
            Ok(record)
        })
    }

    pub fn persist_state(&self, state: &SchedulerInstanceState) -> Result<(), String> {
        let contents = state.encode()?;
        let path = self.record_path(STATE_PREFIX, &state.cursor.instance)?;
        self.atomic_write(&path, "state", &contents)
    }

    pub fn instances(&self) -> Result<Vec<String>, String> {
        let mut instances = Vec::new();
        for entry in fs::read_dir(&self.root)
            .map_err(|error| format!("unable to enumerate native scheduler state: {error}"))?
        {
            let entry = entry
                .map_err(|error| format!("unable to inspect native scheduler state: {error}"))?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "native scheduler state filename is not UTF-8".to_string())?;
            if name == LOCK_FILE {
                continue;
            }
            let Some(instance) = record_instance(&name, STATE_PREFIX) else {
                return Err(format!(
                    "unexpected native scheduler store entry during enumeration: {name}"
                ));
            };
            validate_scheduler_instance(instance)?;
            validate_private_file(
                &entry.path(),
                &fs::symlink_metadata(entry.path()).map_err(|error| {
                    format!("unable to inspect native scheduler state: {error}")
                })?,
            )?;
            instances.push(instance.to_string());
        }
        instances.sort();
        if instances.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err("native scheduler store contains duplicate instances".to_string());
        }
        Ok(instances)
    }

    fn record_path(&self, prefix: &str, instance: &str) -> Result<PathBuf, String> {
        validate_scheduler_instance(instance)?;
        Ok(self.root.join(format!("{prefix}{instance}{RECORD_SUFFIX}")))
    }

    fn atomic_write(&self, final_path: &Path, kind: &str, contents: &str) -> Result<(), String> {
        if contents.len() > MAX_RECORD_BYTES {
            return Err("native scheduler record exceeds its size bound".to_string());
        }
        if let Ok(metadata) = fs::symlink_metadata(final_path) {
            validate_private_file(final_path, &metadata)?;
        }
        let sequence = self.write_sequence.fetch_add(1, Ordering::Relaxed);
        if sequence == u32::MAX {
            return Err("native scheduler store write sequence exhausted".to_string());
        }
        let temp_path = self.root.join(format!(
            "{TEMP_PREFIX}{kind}.{}.{}{}",
            std::process::id(),
            sequence,
            TEMP_SUFFIX
        ));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
                .open(&temp_path)
                .map_err(|error| format!("unable to create native scheduler record: {error}"))?;
            file.write_all(contents.as_bytes())
                .map_err(|error| format!("unable to write native scheduler record: {error}"))?;
            file.sync_all()
                .map_err(|error| format!("unable to sync native scheduler record: {error}"))?;
            fs::rename(&temp_path, final_path)
                .map_err(|error| format!("unable to publish native scheduler record: {error}"))?;
            File::open(&self.root)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| format!("unable to sync native scheduler directory: {error}"))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        result
    }
}

impl Drop for SchedulerStore {
    fn drop(&mut self) {
        // POSIX record locks are process-scoped and released when this exact
        // lock-file descriptor closes. Close before publishing the local slot
        // as free so another thread cannot race the kernel release.
        drop(self.lock.take());
        if let Ok(mut locks) = local_lock_registry().lock() {
            locks.remove(&self.lock_key);
        }
    }
}

fn local_lock_registry() -> &'static Mutex<BTreeSet<(u64, u64)>> {
    static LOCKS: OnceLock<Mutex<BTreeSet<(u64, u64)>>> = OnceLock::new();
    LOCKS.get_or_init(|| Mutex::new(BTreeSet::new()))
}

fn ensure_private_directory(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_dir()
                || metadata.file_type().is_symlink()
                || metadata.uid() != unsafe { libc::geteuid() }
            {
                return Err(
                    "native scheduler store directory is unsafe or foreign-owned".to_string(),
                );
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(path)
                .map_err(|error| format!("unable to create native scheduler store: {error}"))?;
        }
        Err(error) => {
            return Err(format!("unable to inspect native scheduler store: {error}"));
        }
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("unable to secure native scheduler store: {error}"))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("unable to re-inspect native scheduler store: {error}"))?;
    if metadata.mode() & 0o077 != 0 {
        return Err("native scheduler store directory is not private".to_string());
    }
    Ok(())
}

fn clean_and_validate_entries(root: &Path) -> Result<(), String> {
    for entry in fs::read_dir(root)
        .map_err(|error| format!("unable to scan native scheduler store: {error}"))?
    {
        let entry = entry.map_err(|error| format!("unable to read scheduler entry: {error}"))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "native scheduler entry name is not UTF-8".to_string())?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("unable to inspect native scheduler entry: {error}"))?;
        if name == LOCK_FILE {
            validate_private_file(&path, &metadata)?;
            continue;
        }
        if let Some(instance) = record_instance(&name, STATE_PREFIX) {
            validate_scheduler_instance(instance)?;
            validate_private_file(&path, &metadata)?;
            if metadata.len() > MAX_RECORD_BYTES as u64 {
                return Err("native scheduler record exceeds its size bound".to_string());
            }
            continue;
        }
        if is_exact_temp_name(&name) {
            validate_private_file(&path, &metadata)?;
            fs::remove_file(&path)
                .map_err(|error| format!("unable to remove stale scheduler record: {error}"))?;
            continue;
        }
        return Err(format!("unexpected native scheduler store entry: {name}"));
    }
    Ok(())
}

fn record_instance<'a>(name: &'a str, prefix: &str) -> Option<&'a str> {
    name.strip_prefix(prefix)?.strip_suffix(RECORD_SUFFIX)
}

fn is_exact_temp_name(name: &str) -> bool {
    let Some(body) = name
        .strip_prefix(TEMP_PREFIX)
        .and_then(|value| value.strip_suffix(TEMP_SUFFIX))
    else {
        return false;
    };
    let mut fields = body.split('.');
    matches!(fields.next(), Some("state"))
        && fields.next().is_some_and(|value| {
            !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
        })
        && fields.next().is_some_and(|value| {
            !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
        })
        && fields.next().is_none()
}

fn validate_private_file(path: &Path, metadata: &fs::Metadata) -> Result<(), String> {
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
    {
        return Err(format!(
            "{} is not a private scheduler-owned regular file",
            path.display()
        ));
    }
    Ok(())
}

fn read_optional_record<T>(
    path: &Path,
    decode: impl FnOnce(&str) -> Result<T, String>,
) -> Result<Option<T>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("unable to inspect scheduler record: {error}")),
    };
    validate_private_file(path, &metadata)?;
    if metadata.len() > MAX_RECORD_BYTES as u64 {
        return Err("native scheduler record exceeds its size bound".to_string());
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| {
            file.take((MAX_RECORD_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
        })
        .map_err(|error| format!("unable to read native scheduler record: {error}"))?;
    if bytes.len() > MAX_RECORD_BYTES {
        return Err("native scheduler record exceeds its size bound".to_string());
    }
    let contents =
        String::from_utf8(bytes).map_err(|_| "native scheduler record is not UTF-8".to_string())?;
    decode(&contents).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operations::scheduler::{
        BudgetLedger, FailedAttemptFence, ScheduleCursor, SchedulerGenerations,
        SchedulerInstanceState,
    };
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

    fn test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "cake-autorate-scheduler-store-{name}-{}-{}",
            std::process::id(),
            TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn budget(instance: &str) -> BudgetLedger {
        BudgetLedger::new(
            instance.to_string(),
            "20260805".to_string(),
            "202608".to_string(),
            10_000,
            50_000,
        )
        .unwrap()
    }

    fn state(instance: &str) -> SchedulerInstanceState {
        SchedulerInstanceState::new(
            ScheduleCursor::new(instance.to_string(), 1_000).unwrap(),
            budget(instance),
        )
        .unwrap()
    }

    #[test]
    fn cursor_and_budget_round_trip_through_one_private_state_record() {
        let root = test_root("roundtrip");
        let store = SchedulerStore::open(&root).unwrap();
        let state = state("wan_sqm");
        store.persist_state(&state).unwrap();
        assert_eq!(store.instances().unwrap(), vec!["wan_sqm".to_string()]);
        assert_eq!(store.load_state("wan_sqm").unwrap(), Some(state));
        assert_eq!(
            fs::symlink_metadata(root.join("instance-wan_sqm.state"))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn active_temp_name_collision_fails_closed_without_publishing_a_record() {
        let root = test_root("active-temp-collision");
        let store = SchedulerStore::open(&root).unwrap();
        let temp = root.join(format!(
            "{TEMP_PREFIX}state.{}.1{TEMP_SUFFIX}",
            std::process::id()
        ));
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
            .unwrap();
        assert!(store.persist_state(&state("wan_sqm")).is_err());
        assert!(!root.join("instance-wan_sqm.state").exists());
        assert!(!temp.exists());
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn portable_write_sequence_exhaustion_fails_before_creating_a_temp_file() {
        let root = test_root("sequence-exhaustion");
        let store = SchedulerStore::open(&root).unwrap();
        store.write_sequence.store(u32::MAX, Ordering::Relaxed);
        let error = store.persist_state(&state("wan_sqm")).unwrap_err();
        assert!(error.contains("write sequence exhausted"));
        assert!(!root.join("instance-wan_sqm.state").exists());
        assert_eq!(
            fs::read_dir(&root)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX))
                .count(),
            0
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_second_scheduler_owner_is_rejected() {
        let root = test_root("lock");
        let owner = SchedulerStore::open(&root).unwrap();
        let error = SchedulerStore::open(&root).err().unwrap();
        assert!(error.contains("owned by another process"));
        drop(owner);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_reserved_budget_survives_owner_death_and_reopen() {
        let root = test_root("reservation");
        let store = SchedulerStore::open(&root).unwrap();
        let mut ledger = budget("wan_sqm");
        ledger
            .reserve(
                "a".repeat(32),
                "b".repeat(32),
                "20260805",
                "202608",
                FailedAttemptFence {
                    due_unix_s: 1_000,
                    interval_s: 3_600,
                    generations: SchedulerGenerations {
                        config_fingerprint: "c".repeat(64),
                        route_fingerprint: "d".repeat(64),
                        runtime_sequence: 7,
                        coordinator_generation: "e".repeat(32),
                    },
                    explicit_retry_sequence: 0,
                },
                4_000,
            )
            .unwrap();
        let state = SchedulerInstanceState::new(
            ScheduleCursor::new("wan_sqm".to_string(), 1_000).unwrap(),
            ledger.clone(),
        )
        .unwrap();
        store.persist_state(&state).unwrap();
        drop(store);

        let reopened = SchedulerStore::open(&root).unwrap();
        let restored = reopened.load_state("wan_sqm").unwrap().unwrap();
        assert_eq!(restored.budget.reservation, ledger.reservation);
        assert_eq!(restored.budget.available_bytes().unwrap(), 0);
        drop(reopened);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_stale_temporary_files_are_removed_but_unknown_entries_fail_closed() {
        let root = test_root("stale");
        drop(SchedulerStore::open(&root).unwrap());
        let stale = root.join(".scheduler-write.state.123.7.tmp");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&stale)
            .unwrap();
        let reopened = SchedulerStore::open(&root).unwrap();
        assert!(!stale.exists());
        drop(reopened);

        let unknown = root.join("foreign");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&unknown)
            .unwrap();
        let error = SchedulerStore::open(&root).err().unwrap();
        assert!(error.contains("unexpected native scheduler store entry"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_and_permissive_records_fail_closed() {
        let root = test_root("corrupt");
        drop(SchedulerStore::open(&root).unwrap());
        let path = root.join("instance-wan.state");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.write_all(b"truncated\n").unwrap();
        drop(file);
        let store = SchedulerStore::open(&root).unwrap();
        assert!(store.load_state("wan").is_err());
        drop(store);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(SchedulerStore::open(&root).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn symlink_store_root_is_rejected() {
        let target = test_root("symlink-target");
        let link = test_root("symlink-link");
        fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(SchedulerStore::open(&link).is_err());
        fs::remove_file(link).unwrap();
        fs::remove_dir(target).unwrap();
    }
}
