//! Durable two-package publication of a native-validated committed candidate.
//! No original-name libuci calls, delta consumption or implicit rebaselining.
//! The caller holds lifecycle authority. Kernel exchange retains the displaced
//! inode; a journal is durable BEFORE the first public-name exchange.
//! Each rename is atomic, NOT simultaneous visibility of the two-package pair.
//! Runtime users must retain frozen authority until the pair is verified. An
//! accepted transaction releases rollback ownership; later edits are not ours.
use super::committed_uci::{
    file_identity, read_file, Directory, Identity, PreparedConfig, MAX_CONFIG, PACKAGES,
};
use crate::config_candidate::digest;
use serde_json::{json, Value};
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

type Result<T> = std::result::Result<T, String>;
const OWNER: &[u8] = b"cake-autorate start transaction v1\n";
const RECEIPT: &[u8] = b"cake-autorate start receipt v1\n";
const ACCEPTED: &[u8] = b"cake-autorate start transaction retired v1\n";
const MAX_RECEIPT: u64 = 16384;
const NAMES: [&str; 10] = [
    "owner",
    "intent",
    "cake-autorate.swap",
    "sqm.swap",
    "ready.tmp",
    "ready",
    "accepted",
    "accepted.tmp",
    "displaced",
    "displaced.tmp",
];

fn io<T>(result: std::io::Result<T>) -> Result<T> {
    result.map_err(|_| "uci-transaction-io".into())
}
fn sync(directory: &Directory) -> Result<()> {
    directory.attest()?;
    io(directory.file.sync_all())
}
fn exists(directory: &Directory, name: &str) -> Result<bool> {
    directory.attest()?;
    match fs::symlink_metadata(directory.path.join(name)) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err("uci-transaction-io".into()),
    }
}
fn regular(path: &Path, max: u64, private: bool) -> Result<(Vec<u8>, File, Identity)> {
    let result = read_file(path, max, private)?;
    if io(result.1.metadata())?.nlink() != 1 {
        return Err("uci-transaction-hardlink-refused".into());
    }
    Ok(result)
}
fn write_new(directory: &Directory, name: &str, bytes: &[u8]) -> Result<File> {
    directory.attest()?;
    let mut file = io(OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(directory.path.join(name)))?;
    io(file.write_all(bytes))?;
    io(file.sync_all())?;
    directory.attest()?;
    file_identity(&directory.path.join(name), &file, true)?;
    Ok(file)
}
pub(super) fn rename(
    from: &Directory,
    old: &str,
    to: &Directory,
    new: &str,
    exchange: bool,
) -> Result<()> {
    from.attest()?;
    to.attest()?;
    let old = CString::new(old).map_err(|_| "uci-transaction-path-invalid")?;
    let new = CString::new(new).map_err(|_| "uci-transaction-path-invalid")?;
    let flags = if exchange {
        libc::RENAME_EXCHANGE
    } else {
        libc::RENAME_NOREPLACE
    };
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            from.file.as_raw_fd(),
            old.as_ptr(),
            to.file.as_raw_fd(),
            new.as_ptr(),
            flags,
        )
    };
    if result != 0 {
        // No non-atomic unlink/rename fallback, including unsupported kernels.
        return Err(match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ENOSYS | libc::EOPNOTSUPP | libc::EINVAL) => {
                "uci-transaction-atomic-rename-unsupported"
            }
            Some(libc::EEXIST) => "uci-transaction-rename-target-exists",
            _ => "uci-transaction-atomic-rename-failed",
        }
        .into());
    }
    sync(from)?;
    sync(to)
}

#[derive(Clone)]
struct Version {
    identity: Identity,
    sha256: String,
}
impl Version {
    fn of(bytes: &[u8], identity: Identity) -> Self {
        Self {
            identity,
            sha256: digest(bytes),
        }
    }
    fn matches(&self, bytes: &[u8], actual: &Identity, moved: bool) -> bool {
        let mut actual = actual.clone();
        if moved {
            // rename changes ctime, but not content, inode, mtime or ownership.
            actual.ctime = self.identity.ctime;
            actual.ctime_ns = self.identity.ctime_ns;
        }
        actual == self.identity && digest(bytes) == self.sha256
    }
    fn json(&self) -> Value {
        let i = &self.identity;
        json!({"dev":i.dev,"ino":i.ino,"len":i.len,"uid":i.uid,"gid":i.gid,"mode":i.mode,
            "mtime":i.mtime,"mtime_ns":i.mtime_ns,"ctime":i.ctime,"ctime_ns":i.ctime_ns,"sha256":self.sha256})
    }
    fn parse(value: &Value) -> Result<Self> {
        let u = |key: &str| {
            value[key]
                .as_u64()
                .ok_or_else(|| "uci-transaction-receipt-invalid".to_string())
        };
        let s = |key: &str| {
            value[key]
                .as_i64()
                .ok_or_else(|| "uci-transaction-receipt-invalid".to_string())
        };
        let small = |key: &str| {
            u32::try_from(u(key)?).map_err(|_| "uci-transaction-receipt-invalid".to_string())
        };
        let identity = Identity {
            dev: u("dev")?,
            ino: u("ino")?,
            len: u("len")?,
            uid: small("uid")?,
            gid: small("gid")?,
            mode: small("mode")?,
            mtime: s("mtime")?,
            mtime_ns: s("mtime_ns")?,
            ctime: s("ctime")?,
            ctime_ns: s("ctime_ns")?,
        };
        if identity.len > MAX_CONFIG
            || identity.uid != unsafe { libc::geteuid() }
            || identity.mode & libc::S_IFMT != libc::S_IFREG
            || identity.mode & 0o022 != 0
            || !(0..1_000_000_000).contains(&identity.mtime_ns)
            || !(0..1_000_000_000).contains(&identity.ctime_ns)
        {
            return Err("uci-transaction-receipt-invalid".into());
        }
        Ok(Self {
            identity,
            sha256: hash(&value["sha256"])?,
        })
    }
}
fn hash(value: &Value) -> Result<String> {
    value
        .as_str()
        .filter(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .map(str::to_string)
        .ok_or_else(|| "uci-transaction-receipt-invalid".into())
}
fn write_receipt(directory: &Directory, name: &str, value: Value) -> Result<()> {
    let mut bytes = RECEIPT.to_vec();
    bytes.extend(serde_json::to_vec(&value).map_err(|_| "uci-transaction-receipt-encode")?);
    if bytes.len() as u64 > MAX_RECEIPT {
        return Err("uci-transaction-receipt-too-large".into());
    }
    write_new(directory, name, &bytes)?;
    sync(directory)
}
fn read_receipt(directory: &Directory, name: &str) -> Result<Value> {
    let (bytes, _, _) = regular(&directory.path.join(name), MAX_RECEIPT, true)?;
    let body = bytes
        .strip_prefix(RECEIPT)
        .ok_or("uci-transaction-receipt-invalid")?;
    let value: Value =
        serde_json::from_slice(body).map_err(|_| "uci-transaction-receipt-invalid")?;
    if value["schema"].as_u64() != Some(1) {
        return Err("uci-transaction-receipt-invalid".into());
    }
    Ok(value)
}

struct Root {
    config: Directory,
    root: Directory,
    lock: File,
}
impl Root {
    fn open(config: &Path) -> Result<Self> {
        let config = Directory::open(config, false, false)?;
        let root = Directory::open(&config.path.join(".start-uci"), true, true)?;
        let lock = io(OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(root.path.join("lock")))?;
        if file_identity(&root.path.join("lock"), &lock, true)?.len != 0 {
            return Err("uci-transaction-lock-invalid".into());
        }
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("uci-transaction-busy".into());
        }
        let result = Self { config, root, lock };
        result.attest()?;
        result.clean_retired()?;
        Ok(result)
    }
    fn attest(&self) -> Result<()> {
        self.config.attest()?;
        self.root.attest()?;
        if io(self.config.file.metadata())?.dev() != io(self.root.file.metadata())?.dev() {
            return Err("uci-transaction-cross-filesystem-refused".into());
        }
        file_identity(&self.root.path.join("lock"), &self.lock, true)?;
        for name in self.root.names()? {
            if !["lock", "pending", "garbage"].iter().any(|s| name == *s) {
                return Err("uci-transaction-foreign-workspace-entry".into());
            }
        }
        Ok(())
    }
    fn inspect(&self, directory: &Directory) -> Result<Vec<String>> {
        self.attest()?;
        directory.attest()?;
        if io(directory.file.metadata())?.dev() != io(self.root.file.metadata())?.dev() {
            return Err("uci-transaction-cross-filesystem-refused".into());
        }
        let mut names = Vec::new();
        for name in directory.names()? {
            let name = name
                .to_str()
                .ok_or("uci-transaction-foreign-workspace-entry")?;
            if !NAMES.contains(&name) {
                return Err("uci-transaction-foreign-workspace-entry".into());
            }
            let swap = name.ends_with(".swap");
            regular(
                &directory.path.join(name),
                if swap { MAX_CONFIG } else { MAX_RECEIPT },
                !swap,
            )?;
            names.push(name.to_string());
        }
        Ok(names)
    }
    fn clean_retired(&self) -> Result<()> {
        if !exists(&self.root, "garbage")? {
            return Ok(());
        }
        let directory = Directory::open(&self.root.path.join("garbage"), true, false)?;
        let names = self.inspect(&directory)?;
        if !names.is_empty() {
            let (marker, _, _) = regular(&directory.path.join("accepted"), MAX_RECEIPT, true)?;
            if marker != ACCEPTED {
                return Err("uci-transaction-retirement-unproven".into());
            }
            for name in names.iter().filter(|s| s.as_str() != "accepted") {
                directory.attest()?;
                io(fs::remove_file(directory.path.join(name)))?;
            }
            sync(&directory)?;
            io(fs::remove_file(directory.path.join("accepted")))?;
            sync(&directory)?;
        }
        directory.attest()?;
        io(fs::remove_dir(&directory.path))?;
        sync(&self.root)
    }
    fn retire(&self, directory: &Directory) -> Result<()> {
        self.inspect(directory)?;
        if !exists(directory, "accepted")? {
            if exists(directory, "accepted.tmp")? {
                let (partial, _, _) =
                    regular(&directory.path.join("accepted.tmp"), MAX_RECEIPT, true)?;
                if !ACCEPTED.starts_with(&partial) {
                    return Err("uci-transaction-retirement-unproven".into());
                }
                io(fs::remove_file(directory.path.join("accepted.tmp")))?;
            }
            write_new(directory, "accepted.tmp", ACCEPTED)?;
            rename(directory, "accepted.tmp", directory, "accepted", false)?;
        }
        let (marker, _, _) = regular(&directory.path.join("accepted"), MAX_RECEIPT, true)?;
        if marker != ACCEPTED {
            return Err("uci-transaction-retirement-unproven".into());
        }
        sync(directory)?;
        rename(&self.root, "pending", &self.root, "garbage", false)?;
        self.clean_retired()
    }
}

struct Journal {
    root: Root,
    pending: Directory,
    before: [Version; 2],
    after: [Version; 2],
    changed: [bool; 2],
}
impl Journal {
    fn prepare(config: &PreparedConfig) -> Result<Self> {
        config.attest()?;
        let root = Root::open(&config.original.directory.path)?;
        if exists(&root.root, "pending")? {
            return Err("uci-transaction-recovery-pending".into());
        }
        let pending = Directory::open(&root.root.path.join("pending"), true, true)?;
        write_new(&pending, "owner", OWNER)?;
        sync(&pending)?;
        sync(&root.root)?;
        let before = PACKAGES.map(|name| {
            let source = &config.original.sources[name];
            Version::of(&source.bytes, source.identity.clone())
        });
        let candidate = [
            config.candidate_bytes(PACKAGES[0])?,
            config.candidate_bytes(PACKAGES[1])?,
        ];
        let changed = [
            candidate[0] != config.original_bytes(PACKAGES[0])?,
            candidate[1] != config.original_bytes(PACKAGES[1])?,
        ];
        write_receipt(
            &pending,
            "intent",
            json!({"schema":1,"entries": (0..2).map(|i| json!({
            "name":PACKAGES[i],"before":before[i].json(),"changed":changed[i],
            "after_sha256":digest(candidate[i]),"after_len":candidate[i].len()})).collect::<Vec<_>>() }),
        )?;
        let mut after = before.clone();
        for i in 0..2 {
            if !changed[i] {
                continue;
            }
            config.attest()?;
            root.attest()?;
            let name = format!("{}.swap", PACKAGES[i]);
            let file = write_new(&pending, &name, candidate[i])?;
            let original = &before[i].identity;
            if unsafe { libc::fchown(file.as_raw_fd(), original.uid, original.gid) } != 0
                || unsafe { libc::fchmod(file.as_raw_fd(), original.mode & 0o7777) } != 0
            {
                return Err("uci-transaction-metadata-copy-failed".into());
            }
            io(file.sync_all())?;
            let (bytes, _, identity) = regular(&pending.path.join(name), MAX_CONFIG, false)?;
            if bytes != candidate[i] {
                return Err("uci-transaction-candidate-changed".into());
            }
            after[i] = Version::of(&bytes, identity);
        }
        config.attest()?;
        root.attest()?;
        write_receipt(
            &pending,
            "ready.tmp",
            json!({"schema":1,"after":after.iter().map(Version::json).collect::<Vec<_>>()}),
        )?;
        rename(&pending, "ready.tmp", &pending, "ready", false)?;
        Ok(Self {
            root,
            pending,
            before,
            after,
            changed,
        })
    }
    fn load(root: Root, pending: Directory) -> Result<Self> {
        root.inspect(&pending)?;
        let (owner, _, _) = regular(&pending.path.join("owner"), MAX_RECEIPT, true)?;
        if owner != OWNER {
            return Err("uci-transaction-owner-invalid".into());
        }
        let intent = read_receipt(&pending, "intent")?;
        let ready = read_receipt(&pending, "ready")?;
        let entries = intent["entries"]
            .as_array()
            .filter(|a| a.len() == 2)
            .ok_or("uci-transaction-receipt-invalid")?;
        let after = ready["after"]
            .as_array()
            .filter(|a| a.len() == 2)
            .ok_or("uci-transaction-receipt-invalid")?;
        let before = [
            Version::parse(&entries[0]["before"])?,
            Version::parse(&entries[1]["before"])?,
        ];
        let after = [Version::parse(&after[0])?, Version::parse(&after[1])?];
        let mut changed = [false; 2];
        for i in 0..2 {
            let entry = &entries[i];
            changed[i] = entry["changed"]
                .as_bool()
                .ok_or("uci-transaction-receipt-invalid")?;
            if entry["name"].as_str() != Some(PACKAGES[i])
                || hash(&entry["after_sha256"])? != after[i].sha256
                || entry["after_len"].as_u64() != Some(after[i].identity.len)
                || before[i].identity.uid != after[i].identity.uid
                || before[i].identity.gid != after[i].identity.gid
                || before[i].identity.mode != after[i].identity.mode
                || (!changed[i]
                    && (before[i].identity != after[i].identity
                        || before[i].sha256 != after[i].sha256))
            {
                return Err("uci-transaction-receipt-invalid".into());
            }
        }
        Ok(Self {
            root,
            pending,
            before,
            after,
            changed,
        })
    }
    fn lock_sources(&self) -> Result<Vec<File>> {
        self.root.attest()?;
        let mut files = Vec::new();
        for name in PACKAGES {
            let (_, file, _) = regular(&self.root.config.path.join(name), MAX_CONFIG, false)?;
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err("uci-transaction-source-busy".into());
            }
            files.push(file);
        }
        Ok(files)
    }
    fn public_matches(&self, i: usize, version: &Version, moved: bool) -> Result<bool> {
        self.root.attest()?;
        let (bytes, _, identity) =
            regular(&self.root.config.path.join(PACKAGES[i]), MAX_CONFIG, false)?;
        Ok(version.matches(&bytes, &identity, moved))
    }
    fn version_bytes(&self, i: usize, version: &Version) -> Result<Vec<u8>> {
        for path in [
            self.root.config.path.join(PACKAGES[i]),
            self.pending.path.join(format!("{}.swap", PACKAGES[i])),
        ] {
            match fs::symlink_metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err("uci-transaction-io".into()),
                Ok(_) => {}
            }
            let (bytes, _, identity) = regular(&path, MAX_CONFIG, false)?;
            if version.matches(&bytes, &identity, true) {
                return Ok(bytes);
            }
        }
        Err("uci-transaction-recovery-version-missing".into())
    }
    fn recovery_views(&self) -> Result<([Vec<u8>; 2], [Vec<u8>; 2])> {
        let mut before = [Vec::new(), Vec::new()];
        let mut after = [Vec::new(), Vec::new()];
        for i in 0..2 {
            after[i] = self.version_bytes(i, &self.after[i]).map_err(|error| {
                if error == "uci-transaction-recovery-version-missing" {
                    "uci-transaction-recovery-conflict".into()
                } else {
                    error
                }
            })?;
            before[i] = match self.version_bytes(i, &self.before[i]) {
                Ok(bytes) => bytes,
                Err(error) if error == "uci-transaction-recovery-version-missing" => {
                    // Publication may have displaced a real commit in the
                    // exchange gap. Only its durable displacement proof can
                    // substitute for the vanished historical before-version.
                    let proof = read_receipt(&self.pending, "displaced")?;
                    let displaced = Version::parse(&proof["version"])?;
                    let candidate = Version::parse(&proof["candidate"])?;
                    if proof["name"].as_str() != Some(PACKAGES[i])
                        || candidate.identity != self.after[i].identity
                        || candidate.sha256 != self.after[i].sha256
                    {
                        return Err("uci-transaction-displacement-unproven".into());
                    }
                    self.version_bytes(i, &displaced)?
                }
                Err(error) => return Err(error),
            };
        }
        Ok((before, after))
    }
    fn swap_version(&self, i: usize) -> Result<Version> {
        self.pending.attest()?;
        let (bytes, _, identity) = regular(
            &self.pending.path.join(format!("{}.swap", PACKAGES[i])),
            MAX_CONFIG,
            false,
        )?;
        Ok(Version::of(&bytes, identity))
    }
    fn attest_applied(&self) -> Result<()> {
        self.root.inspect(&self.pending)?;
        for i in 0..2 {
            if !self.public_matches(i, &self.after[i], self.changed[i])? {
                return Err("uci-transaction-published-source-changed".into());
            }
            if self.changed[i] {
                let (bytes, _, identity) = regular(
                    &self.pending.path.join(format!("{}.swap", PACKAGES[i])),
                    MAX_CONFIG,
                    false,
                )?;
                if !self.before[i].matches(&bytes, &identity, true) {
                    return Err("uci-transaction-displaced-source-changed".into());
                }
            }
        }
        Ok(())
    }
    fn publish(
        &self,
        config: &PreparedConfig,
        mut checkpoint: impl FnMut(usize) -> Result<()>,
    ) -> Result<()> {
        let _locks = self.lock_sources()?;
        config.attest()?;
        for i in 0..2 {
            checkpoint(2 * i)?;
            self.root.inspect(&self.pending)?;
            // Recheck every original/applied member, not only the next target.
            for j in 0..2 {
                let applied = j < i && self.changed[j];
                if !self.public_matches(
                    j,
                    if applied {
                        &self.after[j]
                    } else {
                        &self.before[j]
                    },
                    applied,
                )? {
                    return Err("uci-transaction-source-changed-before-exchange".into());
                }
            }
            if !self.changed[i] {
                continue;
            }
            let (bytes, _, identity) = regular(
                &self.pending.path.join(format!("{}.swap", PACKAGES[i])),
                MAX_CONFIG,
                false,
            )?;
            if !self.after[i].matches(&bytes, &identity, false) {
                return Err("uci-transaction-candidate-changed".into());
            }
            checkpoint(2 * i + 1)?;
            rename(
                &self.root.config,
                PACKAGES[i],
                &self.pending,
                &format!("{}.swap", PACKAGES[i]),
                true,
            )?;
            let (bytes, _, identity) = regular(
                &self.pending.path.join(format!("{}.swap", PACKAGES[i])),
                MAX_CONFIG,
                false,
            )?;
            if !self.before[i].matches(&bytes, &identity, true) {
                let displaced = Version::of(&bytes, identity);
                write_receipt(
                    &self.pending,
                    "displaced.tmp",
                    json!({"schema":1,
                    "name":PACKAGES[i],"version":displaced.json(),"candidate":self.after[i].json()}),
                )?;
                rename(
                    &self.pending,
                    "displaced.tmp",
                    &self.pending,
                    "displaced",
                    false,
                )?;
                return Err("uci-transaction-concurrent-commit-displaced".into());
            }
        }
        checkpoint(4)?;
        config.attest_private()?;
        self.attest_applied()
    }
    fn rollback(&self) -> Result<bool> {
        self.rollback_with(|_| Ok(()))
    }
    fn rollback_with(&self, mut checkpoint: impl FnMut(usize) -> Result<()>) -> Result<bool> {
        self.rollback_with_status(|step, _| checkpoint(step))
    }
    fn rollback_with_status(
        &self,
        mut checkpoint: impl FnMut(usize, bool) -> Result<()>,
    ) -> Result<bool> {
        let _locks = self.lock_sources()?;
        self.root.inspect(&self.pending)?;
        let mut restore: [Option<Version>; 2] = [None, None];
        let mut foreign_restored = false;
        // Preflight the pair before any restore. Never overwrite a later commit
        // at a public path, even if the other member is still ours.
        for i in 0..2 {
            if !self.changed[i] {
                if !self.public_matches(i, &self.before[i], false)? {
                    return Err("uci-transaction-recovery-conflict".into());
                }
            } else if self.public_matches(i, &self.after[i], true)? {
                let displaced = self.swap_version(i)?;
                let (bytes, _, identity) = regular(
                    &self.pending.path.join(format!("{}.swap", PACKAGES[i])),
                    MAX_CONFIG,
                    false,
                )?;
                if !self.before[i].matches(&bytes, &identity, true) {
                    // Do not mistake private backup corruption/replacement for
                    // a foreign inode observed at the publication exchange.
                    let proof = read_receipt(&self.pending, "displaced")?;
                    let expected = Version::parse(&proof["version"])?;
                    let candidate = Version::parse(&proof["candidate"])?;
                    if proof["name"].as_str() != Some(PACKAGES[i])
                        || !expected.matches(&bytes, &identity, false)
                        || candidate.identity != self.after[i].identity
                        || candidate.sha256 != self.after[i].sha256
                    {
                        return Err("uci-transaction-displacement-unproven".into());
                    }
                    foreign_restored = true;
                }
                if displaced.identity.ino == self.after[i].identity.ino
                    && displaced.identity.dev == self.after[i].identity.dev
                {
                    return Err("uci-transaction-recovery-conflict".into());
                }
                restore[i] = Some(displaced);
            } else if !self.public_matches(i, &self.before[i], true)? {
                return Err("uci-transaction-recovery-conflict".into());
            } else {
                let (bytes, _, identity) = regular(
                    &self.pending.path.join(format!("{}.swap", PACKAGES[i])),
                    MAX_CONFIG,
                    false,
                )?;
                if !self.after[i].matches(&bytes, &identity, true) {
                    return Err("uci-transaction-recovery-conflict".into());
                }
            }
        }
        for i in (0..2).rev() {
            let Some(displaced) = &restore[i] else {
                continue;
            };
            if !self.public_matches(i, &self.after[i], true)? {
                return Err("uci-transaction-recovery-conflict".into());
            }
            let current = self.swap_version(i)?;
            if current.identity != displaced.identity || current.sha256 != displaced.sha256 {
                return Err("uci-transaction-recovery-conflict".into());
            }
            checkpoint(i, foreign_restored)?;
            rename(
                &self.root.config,
                PACKAGES[i],
                &self.pending,
                &format!("{}.swap", PACKAGES[i]),
                true,
            )?;
            if !self.public_matches(i, displaced, true)? {
                return Err("uci-transaction-recovery-conflict".into());
            }
            let (bytes, _, identity) = regular(
                &self.pending.path.join(format!("{}.swap", PACKAGES[i])),
                MAX_CONFIG,
                false,
            )?;
            if !self.after[i].matches(&bytes, &identity, true) {
                // A writer won the gap during rollback itself. Return the
                // just-displaced foreign inode, not the historical baseline.
                let foreign = Version::of(&bytes, identity);
                if !self.public_matches(i, displaced, true)? {
                    return Err("uci-transaction-recovery-conflict".into());
                }
                rename(
                    &self.root.config,
                    PACKAGES[i],
                    &self.pending,
                    &format!("{}.swap", PACKAGES[i]),
                    true,
                )?;
                if !self.public_matches(i, &foreign, true)? {
                    return Err("uci-transaction-recovery-conflict".into());
                }
                return Err("uci-transaction-recovery-conflict".into());
            }
        }
        checkpoint(2, foreign_restored)?;
        self.root.retire(&self.pending)?;
        Ok(foreign_restored)
    }
}

pub(crate) struct PublishedConfig {
    config: PreparedConfig,
    journal: Option<Journal>,
    source: SourceReceipt,
}
pub(crate) struct CommittedConfig {
    config: PreparedConfig,
    versions: [Version; 2],
}

/// Read-only source proof for the private controller handoff. Package paths are
/// fixed by this module; no path is accepted from a serialized receipt.
#[derive(Clone)]
pub(crate) struct SourceReceipt {
    versions: [Version; 2],
}
impl std::fmt::Debug for SourceReceipt {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SourceReceipt(two committed packages)")
    }
}
impl SourceReceipt {
    pub(crate) fn from_committed(
        snapshot: &super::committed_uci::CommittedSnapshot,
    ) -> Result<Self> {
        snapshot.attest()?;
        let versions = PACKAGES.map(|name| {
            let source = &snapshot.sources[name];
            Version::of(&source.bytes, source.identity.clone())
        });
        snapshot.attest()?;
        Ok(Self { versions })
    }
    pub(crate) fn encode(&self) -> Value {
        json!({"schema":1,"versions":self.versions.iter().map(Version::json).collect::<Vec<_>>()})
    }
    pub(crate) fn fingerprint(&self) -> Result<String> {
        Ok(digest(
            &serde_json::to_vec(&self.encode())
                .map_err(|_| "uci-transaction-fingerprint-failed")?,
        ))
    }
    /// A known rollback may change rename timestamps, not bytes/inodes/modes.
    /// Validate a new strict snapshot against that proven original authority
    /// before issuing its source handoff; never adopt another committed inode.
    pub(crate) fn attest_snapshot(
        &self,
        snapshot: &super::committed_uci::CommittedSnapshot,
    ) -> Result<()> {
        snapshot.attest()?;
        for (name, version) in PACKAGES.into_iter().zip(&self.versions) {
            let source = &snapshot.sources[name];
            if !version.matches(&source.bytes, &source.identity, true) {
                return Err("uci-transaction-restored-snapshot-mismatch".into());
            }
        }
        snapshot.attest()
    }
    pub(crate) fn decode(value: &Value) -> Result<Self> {
        if value["schema"].as_u64() != Some(1) {
            return Err("uci-transaction-receipt-invalid".into());
        }
        let versions = value["versions"]
            .as_array()
            .filter(|v| v.len() == 2)
            .ok_or("uci-transaction-receipt-invalid")?;
        Ok(Self {
            versions: [Version::parse(&versions[0])?, Version::parse(&versions[1])?],
        })
    }
    pub(crate) fn attest(&self, config_root: &Path) -> Result<()> {
        let directory = Directory::open(config_root, false, false)?;
        for (name, version) in PACKAGES.into_iter().zip(&self.versions) {
            let (bytes, _, identity) = regular(&directory.path.join(name), MAX_CONFIG, false)?;
            if !version.matches(&bytes, &identity, true) {
                return Err("uci-transaction-source-receipt-changed".into());
            }
        }
        directory.attest()
    }
    /// Only the existing parent transaction's explicit recovery path may use
    /// this content proof to create a NEW identity receipt after reinstallation.
    /// It does not make the old receipt current or authorize ordinary startup.
    pub(crate) fn attest_reinstalled_content(&self, config_root: &Path) -> Result<()> {
        let directory = Directory::open(config_root, false, false)?;
        for (name, version) in PACKAGES.into_iter().zip(&self.versions) {
            let (bytes, _, identity) = regular(&directory.path.join(name), MAX_CONFIG, false)?;
            if digest(&bytes) != version.sha256
                || identity.len != version.identity.len
                || identity.uid != version.identity.uid
                || identity.gid != version.identity.gid
                || identity.mode != version.identity.mode
            {
                return Err("uci-transaction-reinstalled-content-mismatch".into());
            }
        }
        directory.attest()
    }
}
impl CommittedConfig {
    pub(crate) fn config(&self) -> &PreparedConfig {
        &self.config
    }
    pub(crate) fn attest(&self) -> Result<()> {
        self.config.attest_private()?;
        for (name, version) in PACKAGES.into_iter().zip(&self.versions) {
            let (bytes, _, identity) = regular(
                &self.config.original.directory.path.join(name),
                MAX_CONFIG,
                false,
            )?;
            if !version.matches(&bytes, &identity, true) {
                return Err("uci-transaction-committed-source-changed".into());
            }
        }
        Ok(())
    }
}
impl PublishedConfig {
    pub(crate) fn config(&self) -> &PreparedConfig {
        &self.config
    }
    pub(crate) fn source_receipt(&self) -> Result<SourceReceipt> {
        self.attest()?;
        Ok(self.source.clone())
    }
    pub(crate) fn attest(&self) -> Result<()> {
        match &self.journal {
            Some(journal) => {
                self.config.attest_private()?;
                journal.attest_applied()
            }
            None => self.config.attest(),
        }
    }
    pub(crate) fn rollback(self) -> Result<()> {
        match self.journal {
            Some(journal) => {
                if journal.rollback()? {
                    Err("uci-transaction-concurrent-commit-preserved".into())
                } else {
                    Ok(())
                }
            }
            None => self.config.attest(),
        }
    }
    pub(crate) fn accept(self) -> Result<CommittedConfig> {
        self.attest()?;
        if let Some(journal) = self.journal {
            journal.root.retire(&journal.pending)?;
        }
        let committed = CommittedConfig {
            config: self.config,
            versions: self.source.versions,
        };
        committed.attest()?;
        Ok(committed)
    }
}
pub(crate) fn publish(config: PreparedConfig) -> Result<PublishedConfig> {
    config.attest()?;
    let mut unchanged = true;
    for name in PACKAGES {
        unchanged &= config.candidate_bytes(name)? == config.original_bytes(name)?;
    }
    if unchanged {
        // An unchanged candidate must not wear flash just to journal a no-op.
        // Still refuse an outstanding transaction; recovery precedes capture.
        if exists(&config.original.directory, ".start-uci")? {
            let root = Directory::open(
                &config.original.directory.path.join(".start-uci"),
                true,
                false,
            )?;
            if exists(&root, "pending")? {
                return Err("uci-transaction-recovery-pending".into());
            }
        }
        config.attest()?;
        let source = SourceReceipt::from_committed(&config.original)?;
        return Ok(PublishedConfig {
            config,
            journal: None,
            source,
        });
    }
    let journal = Journal::prepare(&config)?;
    if let Err(error) = journal.publish(&config, |_| Ok(())) {
        return match journal.rollback() {
            Ok(_) => Err(error),
            Err(_) => Err("uci-transaction-recovery-required".into()),
        };
    }
    let source = SourceReceipt {
        versions: journal.after.clone(),
    };
    Ok(PublishedConfig {
        config,
        journal: Some(journal),
        source,
    })
}

/// Reopen an already-published candidate using the durable consumer's exact
/// source receipt. No new projection, publication or adoption of current edits
/// is allowed. Runtime/recovery ownership remains the caller's responsibility.
pub(crate) fn resume_published(
    config: PreparedConfig,
    expected: &SourceReceipt,
) -> Result<PublishedConfig> {
    config.attest()?;
    expected.attest(&config.original.directory.path)?;
    for name in PACKAGES {
        if config.candidate_bytes(name)? != config.original_bytes(name)? {
            return Err("uci-transaction-resume-candidate-changed".into());
        }
    }
    let journal = if recovery_pending(&config.original.directory.path)? {
        let root = Root::open(&config.original.directory.path)?;
        let pending = Directory::open(&root.root.path.join("pending"), true, false)?;
        let journal = Journal::load(root, pending)?;
        if expected.encode()
            != (SourceReceipt {
                versions: journal.after.clone(),
            })
            .encode()
        {
            return Err("uci-transaction-recovery-receipt-mismatch".into());
        }
        journal.attest_applied()?;
        Some(journal)
    } else {
        None
    };
    // Preserve the consumer's original receipt even after journal retirement.
    // rename changes ctime; a fresh capture must not change the durable ID.
    let published = PublishedConfig {
        config,
        journal,
        source: expected.clone(),
    };
    published.attest()?;
    expected.attest(&published.config.original.directory.path)?;
    Ok(published)
}

/// Called explicitly under lifecycle authority, BEFORE capturing a new source
/// baseline. Interrupted publication is rolled back; accepted work is only
/// garbage-collected. An external edit is preserved and reported as conflict.
/// The caller must also fence/attest runtime consumers before a rollback; this
/// file transaction does not stop controllers or restore kernel state itself.
#[cfg(test)]
pub(crate) fn recover(config_dir: &Path) -> Result<()> {
    recover_fenced(config_dir, None, || Ok(())).map(|_| ())
}

/// The optional expected receipt binds ordinary Stop to its durable batch.
/// Return the original verified source authority after rollback, never a new
/// capture of arbitrary current files. The caller retains the batch until all
/// source/runtime checks and registry retirement have completed.
pub(crate) fn recover_fenced(
    config_dir: &Path,
    expected: Option<&SourceReceipt>,
    mut attest_runtime_absent: impl FnMut() -> Result<()>,
) -> Result<Option<SourceReceipt>> {
    recover_checked(config_dir, expected, |check| match check {
        RecoveryCheck::Absent => attest_runtime_absent(),
        RecoveryCheck::Versions { .. } => Ok(()),
        RecoveryCheck::Restored(_) => attest_runtime_absent(),
    })
}

pub(crate) enum RecoveryCheck<'a> {
    Absent,
    /// Both original public versions are proven, and the journal still exists.
    /// A dependent preparation record can now be retired before this receipt.
    Restored(&'a SourceReceipt),
    Versions {
        before: &'a [Vec<u8>; 2],
        after: &'a [Vec<u8>; 2],
    },
}

/// Non-mutating probe; ordinary legacy Stop must not demand process absence
/// merely because no orphan publication exists.
pub(crate) fn recovery_pending(config_dir: &Path) -> Result<bool> {
    let config = Directory::open(config_dir, false, false)?;
    if !exists(&config, ".start-uci")? {
        return Ok(false);
    }
    let root = Directory::open(&config.path.join(".start-uci"), true, false)?;
    exists(&root, "pending")
}

pub(crate) fn recover_checked(
    config_dir: &Path,
    expected: Option<&SourceReceipt>,
    mut check: impl FnMut(RecoveryCheck<'_>) -> Result<()>,
) -> Result<Option<SourceReceipt>> {
    check(RecoveryCheck::Absent)?;
    let config = Directory::open(config_dir, false, false)?;
    if !exists(&config, ".start-uci")? {
        return Ok(None);
    }
    let root = Root::open(config_dir)?;
    if !exists(&root.root, "pending")? {
        return Ok(None);
    }
    let pending = Directory::open(&root.root.path.join("pending"), true, false)?;
    let names = root.inspect(&pending)?;
    if names.is_empty() {
        check(RecoveryCheck::Absent)?;
        io(fs::remove_dir(&pending.path))?;
        sync(&root.root)?;
        return Ok(None);
    }
    let (owner, _, _) = regular(&pending.path.join("owner"), MAX_RECEIPT, true)?;
    if !OWNER.starts_with(&owner) && owner != OWNER {
        return Err("uci-transaction-owner-invalid".into());
    }
    if exists(&pending, "accepted")? {
        check(RecoveryCheck::Absent)?;
        root.retire(&pending)?;
        return Ok(None);
    }
    if !exists(&pending, "ready")? {
        // READY is renamed into place and directory-fsynced before any exchange.
        // Do not treat loss/corruption of a once-valid READY as an empty setup:
        // once swap files exist, both original versions must still be public.
        if exists(&pending, "displaced")? || exists(&pending, "displaced.tmp")? {
            return Err("uci-transaction-readiness-unproven".into());
        }
        if PACKAGES
            .iter()
            .any(|name| names.contains(&format!("{name}.swap")))
        {
            let intent = read_receipt(&pending, "intent")?;
            let entries = intent["entries"]
                .as_array()
                .filter(|a| a.len() == 2)
                .ok_or("uci-transaction-receipt-invalid")?;
            for i in 0..2 {
                let before = Version::parse(&entries[i]["before"])?;
                let (bytes, _, identity) =
                    regular(&root.config.path.join(PACKAGES[i]), MAX_CONFIG, false)?;
                if entries[i]["name"].as_str() != Some(PACKAGES[i])
                    || !before.matches(&bytes, &identity, false)
                {
                    return Err("uci-transaction-readiness-unproven".into());
                }
            }
        }
        check(RecoveryCheck::Absent)?;
        root.retire(&pending)?;
        return Ok(None);
    }
    let journal = Journal::load(root, pending)?;
    if expected.is_some_and(|expected| {
        expected.encode()
            != (SourceReceipt {
                versions: journal.after.clone(),
            })
            .encode()
    }) {
        return Err("uci-transaction-recovery-receipt-mismatch".into());
    }
    let original = SourceReceipt {
        versions: journal.before.clone(),
    };
    let (before, after) = journal.recovery_views()?;
    check(RecoveryCheck::Versions {
        before: &before,
        after: &after,
    })?;
    if journal.rollback_with_status(|step, foreign_restored| {
        check(RecoveryCheck::Absent)?;
        if step == 2 && !foreign_restored {
            original.attest(config_dir)?;
            check(RecoveryCheck::Restored(&original))?;
            original.attest(config_dir)?;
        }
        Ok(())
    })? {
        // The foreign commit is back at its original public name, but this
        // Start must abort rather than silently adopting that newer baseline.
        Err("uci-transaction-concurrent-commit-preserved".into())
    } else {
        original.attest(config_dir)?;
        check(RecoveryCheck::Absent)?;
        original.attest(config_dir)?;
        Ok(Some(original))
    }
}

/// Finish a publication after its preparing process exited. The lifecycle
/// caller supplies the durable desired-source receipt and proves the matching
/// runtime generation; current public bytes alone are never a new authority.
pub(crate) fn accept_receipt(
    config_dir: &Path,
    expected: &SourceReceipt,
    mut attest_runtime: impl FnMut() -> Result<()>,
) -> Result<()> {
    expected.attest(config_dir)?;
    attest_runtime()?;
    let config = Directory::open(config_dir, false, false)?;
    if !exists(&config, ".start-uci")? {
        // A no-op publication deliberately has no persistent journal.
        expected.attest(config_dir)?;
        attest_runtime()?;
        return expected.attest(config_dir);
    }
    let root = Root::open(config_dir)?;
    if !exists(&root.root, "pending")? {
        expected.attest(config_dir)?;
        attest_runtime()?;
        return expected.attest(config_dir);
    }
    let pending = Directory::open(&root.root.path.join("pending"), true, false)?;
    // load also validates ready/intent and before/after package identities.
    let journal = Journal::load(root, pending)?;
    let actual = SourceReceipt {
        versions: journal.after.clone(),
    };
    if actual.encode() != expected.encode() {
        return Err("uci-transaction-acceptance-receipt-mismatch".into());
    }
    journal.attest_applied()?;
    expected.attest(config_dir)?;
    attest_runtime()?;
    journal.attest_applied()?;
    journal.root.retire(&journal.pending)?;
    expected.attest(config_dir)?;
    attest_runtime()?;
    expected.attest(config_dir)
}

#[cfg(test)]
mod tests {
    use super::super::committed_uci::CommittedSnapshot;
    use super::super::uci_edits::Edit;
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::fs::{symlink, DirBuilderExt, PermissionsExt};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Fixture {
        root: PathBuf,
        config: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root =
                std::env::temp_dir().join(format!("cake-r4-txn-{}-{nonce}", std::process::id()));
            fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            let config = root.join("config");
            fs::create_dir(&config).unwrap();
            for name in PACKAGES {
                fs::write(
                    config.join(name),
                    b"# retained comment\nconfig queue 'lab'\n option marker 'before'\n",
                )
                .unwrap();
                fs::set_permissions(config.join(name), fs::Permissions::from_mode(0o640)).unwrap();
            }
            fs::write(
                root.join("fixture-owner"),
                b"cake transaction controlled test\n",
            )
            .unwrap();
            Self { root, config }
        }
        fn prepare(&self, changed: bool) -> PreparedConfig {
            let snapshot = CommittedSnapshot::capture_fixture(
                &self.config,
                &self.root.join("run"),
                fixture_query,
            )
            .unwrap();
            let edits = if changed {
                vec![Edit::Set {
                    section: "lab".into(),
                    option: "marker".into(),
                    value: "after".into(),
                }]
            } else {
                vec![]
            };
            snapshot
                .prepare_fixture([&edits, &edits], fixture_query)
                .unwrap()
        }
        fn versions(&self) -> [Version; 2] {
            PACKAGES.map(|name| {
                let (bytes, _, identity) =
                    regular(&self.config.join(name), MAX_CONFIG, false).unwrap();
                Version::of(&bytes, identity)
            })
        }
        fn assert_versions(&self, expected: &[Version; 2], moved: bool) {
            for (name, version) in PACKAGES.into_iter().zip(expected) {
                let (bytes, _, identity) =
                    regular(&self.config.join(name), MAX_CONFIG, false).unwrap();
                assert!(
                    version.matches(&bytes, &identity, moved),
                    "version changed for {name}"
                );
            }
        }
        fn foreign_commit(&self, name: &str) -> Vec<u8> {
            let value = b"# concurrent foreign committed bytes\nconfig queue 'foreign'\n".to_vec();
            let path = self.root.join("foreign-replacement");
            fs::write(&path, &value).unwrap();
            fs::rename(path, self.config.join(name)).unwrap();
            value
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    fn fixture_query(args: Vec<OsString>) -> Result<Vec<u8>> {
        assert_eq!(args.len(), 10);
        assert_eq!(args[8], "show");
        let alias = args[9].to_str().unwrap();
        assert!(alias.starts_with("cu") && alias.len() == 32);
        let bytes = fs::read(Path::new(&args[1]).join(alias)).unwrap();
        if bytes
            .windows(b"config queue 'foreign'".len())
            .any(|v| v == b"config queue 'foreign'")
        {
            return Ok(format!("{alias}.foreign=queue\n").into_bytes());
        }
        let value = if bytes.windows(7).any(|b| b == b"'after'") {
            "after"
        } else {
            "before"
        };
        Ok(format!("{alias}.lab=queue\n{alias}.lab.marker='{value}'\n").into_bytes())
    }

    #[test]
    fn r4_transaction_acceptance_rechecks_sources_after_the_runtime_proof_callback() {
        for changed in [false, true] {
            let fixture = Fixture::new();
            let published = publish(fixture.prepare(changed)).unwrap();
            let receipt = published.source_receipt().unwrap();
            drop(published);
            let mut calls = 0;
            let mut foreign = Vec::new();
            assert!(accept_receipt(&fixture.config, &receipt, || {
                calls += 1;
                if calls == 2 {
                    foreign = fixture.foreign_commit("sqm");
                }
                Ok(())
            })
            .is_err());
            assert_eq!(calls, 2);
            assert_eq!(fs::read(fixture.config.join("sqm")).unwrap(), foreign);
            assert_eq!(fixture.config.join(".start-uci/pending").exists(), changed);
        }
    }

    #[test]
    fn r4_transaction_resumed_acceptance_requires_original_receipt_and_runtime_proof() {
        for changed in [false, true] {
            let fixture = Fixture::new();
            let published = publish(fixture.prepare(changed)).unwrap();
            let receipt = published.source_receipt().unwrap();
            let after = fixture.versions();
            drop(published);
            assert!(accept_receipt(&fixture.config, &receipt, || Err("not-ready".into())).is_err());
            fixture.assert_versions(&after, false);
            assert_eq!(fixture.config.join(".start-uci/pending").exists(), changed);
            let mut proofs = 0;
            accept_receipt(&fixture.config, &receipt, || {
                proofs += 1;
                Ok(())
            })
            .unwrap();
            assert!(proofs >= 2);
            assert!(!fixture.config.join(".start-uci/pending").exists());
            fixture.assert_versions(&after, false);
            // Lost final ACK retries the same receipt without republishing or
            // retiring a different transaction, and still requires proof.
            accept_receipt(&fixture.config, &receipt, || Ok(())).unwrap();
            assert!(
                accept_receipt(&fixture.config, &receipt, || Err("wrong-generation".into()))
                    .is_err()
            );
            fixture.assert_versions(&after, false);
            if !changed {
                assert!(!fixture.config.join(".start-uci").exists());
            }
        }
    }

    #[test]
    fn r4_transaction_resumed_acceptance_preserves_later_commit_and_corrupt_journal() {
        let fixture = Fixture::new();
        let published = publish(fixture.prepare(true)).unwrap();
        let receipt = published.source_receipt().unwrap();
        drop(published);
        let foreign = fixture.foreign_commit("sqm");
        assert!(accept_receipt(&fixture.config, &receipt, || Ok(())).is_err());
        assert_eq!(fs::read(fixture.config.join("sqm")).unwrap(), foreign);
        assert!(fixture.config.join(".start-uci/pending").exists());

        let fixture = Fixture::new();
        let published = publish(fixture.prepare(true)).unwrap();
        let receipt = published.source_receipt().unwrap();
        drop(published);
        let after = fixture.versions();
        fs::write(fixture.config.join(".start-uci/pending/ready"), b"damaged").unwrap();
        assert!(accept_receipt(&fixture.config, &receipt, || Ok(())).is_err());
        fixture.assert_versions(&after, false);
        assert!(fixture.config.join(".start-uci/pending").exists());
    }

    #[test]
    fn r4_transaction_publish_accept_preserves_metadata_pending_and_native_candidate() {
        let fixture = Fixture::new();
        let before = fixture.versions();
        let pending = fixture.root.join("foreign-delta");
        fs::write(&pending, b"foreign pending bytes").unwrap();
        let published = publish(fixture.prepare(true)).unwrap();
        published.attest().unwrap();
        assert_eq!(
            published.config().package("sqm").unwrap().sections["lab"].options["marker"],
            "after"
        );
        for i in 0..2 {
            let (bytes, _, identity) =
                regular(&fixture.config.join(PACKAGES[i]), MAX_CONFIG, false).unwrap();
            assert_eq!(
                bytes,
                published.config().candidate_bytes(PACKAGES[i]).unwrap()
            );
            assert_eq!(identity.mode, before[i].identity.mode);
            assert_eq!(identity.uid, before[i].identity.uid);
            assert_eq!(identity.gid, before[i].identity.gid);
        }
        let committed = published.accept().unwrap();
        committed.attest().unwrap();
        assert_eq!(
            committed.config().package("sqm").unwrap().sections["lab"].options["marker"],
            "after"
        );
        assert!(!fixture.config.join(".start-uci/pending").exists());
        assert!(!fixture.config.join(".start-uci/garbage").exists());
        assert_eq!(fs::read(pending).unwrap(), b"foreign pending bytes");
        let after = fixture.versions();
        recover(&fixture.config).unwrap();
        fixture.assert_versions(&after, false);
        fixture.foreign_commit("sqm");
        assert!(committed.attest().is_err());
    }

    #[test]
    fn r4_transaction_resume_reopens_exact_source_without_republishing() {
        for accepted in [false, true] {
            let fixture = Fixture::new();
            let published = publish(fixture.prepare(true)).unwrap();
            let source = published.source_receipt().unwrap();
            let versions = fixture.versions();
            if accepted {
                drop(published.accept().unwrap());
            } else {
                drop(published);
            }
            let resumed = resume_published(fixture.prepare(false), &source).unwrap();
            resumed.attest().unwrap();
            assert_eq!(resumed.source_receipt().unwrap().encode(), source.encode());
            fixture.assert_versions(&versions, false);
            assert_eq!(recovery_pending(&fixture.config).unwrap(), !accepted);
            drop(resumed);
            let resumed = resume_published(fixture.prepare(false), &source).unwrap();
            drop(resumed.accept().unwrap());
            fixture.assert_versions(&versions, false);
            assert!(!recovery_pending(&fixture.config).unwrap());
        }
    }

    #[test]
    fn r4_transaction_restored_hook_retains_journal_until_dependent_cleanup_succeeds() {
        let fixture = Fixture::new();
        let before = fixture.versions();
        let published = publish(fixture.prepare(true)).unwrap();
        let source = published.source_receipt().unwrap();
        drop(published);
        let mut restored_calls = 0;
        assert_eq!(
            recover_checked(&fixture.config, Some(&source), |event| {
                if let RecoveryCheck::Restored(original) = event {
                    original.attest(&fixture.config)?;
                    assert!(recovery_pending(&fixture.config).unwrap());
                    fixture.assert_versions(&before, true);
                    restored_calls += 1;
                    return Err("dependent-draft-not-retired".into());
                }
                Ok(())
            })
            .unwrap_err(),
            "dependent-draft-not-retired"
        );
        assert_eq!(restored_calls, 1);
        assert!(recovery_pending(&fixture.config).unwrap());
        let original = recover_checked(&fixture.config, Some(&source), |event| {
            if let RecoveryCheck::Restored(original) = event {
                original.attest(&fixture.config)?;
                assert!(recovery_pending(&fixture.config).unwrap());
                restored_calls += 1;
            }
            Ok(())
        })
        .unwrap()
        .unwrap();
        original.attest(&fixture.config).unwrap();
        assert_eq!(restored_calls, 2);
        fixture.assert_versions(&before, true);
        assert!(!recovery_pending(&fixture.config).unwrap());
    }

    #[test]
    fn r4_transaction_resume_refuses_new_edits_and_foreign_source() {
        let fixture = Fixture::new();
        let unchanged = publish(fixture.prepare(false)).unwrap();
        let source = unchanged.source_receipt().unwrap();
        drop(unchanged);
        let versions = fixture.versions();
        assert_eq!(
            resume_published(fixture.prepare(true), &source)
                .err()
                .unwrap(),
            "uci-transaction-resume-candidate-changed"
        );
        fixture.assert_versions(&versions, false);
        assert!(!recovery_pending(&fixture.config).unwrap());
        let published = publish(fixture.prepare(true)).unwrap();
        let source = published.source_receipt().unwrap();
        drop(published);
        let foreign = fixture.foreign_commit("sqm");
        assert!(resume_published(fixture.prepare(false), &source).is_err());
        assert_eq!(fs::read(fixture.config.join("sqm")).unwrap(), foreign);
        assert!(recovery_pending(&fixture.config).unwrap());
    }

    #[test]
    fn r4_transaction_noop_keeps_source_inodes_and_timestamps() {
        let fixture = Fixture::new();
        let before = fixture.versions();
        recover(&fixture.config).unwrap();
        assert!(!fixture.config.join(".start-uci").exists());
        let published = publish(fixture.prepare(false)).unwrap();
        assert!(published.journal.is_none());
        assert!(!fixture.config.join(".start-uci").exists());
        published.accept().unwrap();
        fixture.assert_versions(&before, false);
    }

    #[test]
    fn r4_transaction_noop_does_not_bypass_a_pending_recovery_receipt() {
        let fixture = Fixture::new();
        let config = fixture.prepare(false);
        let root = Root::open(&fixture.config).unwrap();
        let pending = Directory::open(&root.root.path.join("pending"), true, true).unwrap();
        write_new(&pending, "owner", OWNER).unwrap();
        assert_eq!(
            publish(config).err().unwrap(),
            "uci-transaction-recovery-pending"
        );
        assert_eq!(fs::read(pending.path.join("owner")).unwrap(), OWNER);
    }

    #[test]
    fn r4_transaction_fenced_recovery_keeps_receipt_through_each_refused_boundary() {
        for fail_at in 0..=3 {
            let fixture = Fixture::new();
            let before = fixture.versions();
            let published = publish(fixture.prepare(true)).unwrap();
            let expected = published.source_receipt().unwrap();
            drop(published);
            let mut calls = 0;
            let error = recover_fenced(&fixture.config, Some(&expected), || {
                let at = calls;
                calls += 1;
                if at == fail_at {
                    Err("runtime-not-absent".into())
                } else {
                    Ok(())
                }
            })
            .err()
            .unwrap();
            assert_eq!(error, "runtime-not-absent");
            assert!(fixture.config.join(".start-uci/pending/ready").is_file());
            let restored = recover_fenced(&fixture.config, Some(&expected), || Ok(()))
                .unwrap()
                .unwrap();
            restored.attest(&fixture.config).unwrap();
            fixture.assert_versions(&before, true);
            assert!(!fixture.config.join(".start-uci/pending").exists());
        }
    }

    #[test]
    fn r4_transaction_fenced_recovery_refuses_another_batch_receipt() {
        let fixture = Fixture::new();
        let config = fixture.prepare(true);
        let wrong = SourceReceipt::from_committed(&config.original).unwrap();
        let published = publish(config).unwrap();
        let after = fixture.versions();
        drop(published);
        assert_eq!(
            recover_fenced(&fixture.config, Some(&wrong), || Ok(()))
                .err()
                .unwrap(),
            "uci-transaction-recovery-receipt-mismatch"
        );
        fixture.assert_versions(&after, false);
        assert!(fixture.config.join(".start-uci/pending/ready").is_file());
    }

    #[test]
    fn r4_transaction_explicit_rollback_restores_original_inodes_bytes_and_modes() {
        let fixture = Fixture::new();
        let before = fixture.versions();
        publish(fixture.prepare(true)).unwrap().rollback().unwrap();
        fixture.assert_versions(&before, true);
        recover(&fixture.config).unwrap();
        assert!(!fixture.config.join(".start-uci/pending").exists());
    }

    #[test]
    fn r4_transaction_recovers_each_interrupted_publication_boundary() {
        for step in 0..=4 {
            let fixture = Fixture::new();
            let before = fixture.versions();
            let config = fixture.prepare(true);
            let journal = Journal::prepare(&config).unwrap();
            assert!(journal
                .publish(&config, |at| if at == step {
                    Err("injected-stop".into())
                } else {
                    Ok(())
                })
                .is_err());
            drop(journal);
            drop(config);
            recover(&fixture.config).unwrap();
            fixture.assert_versions(&before, true);
            recover(&fixture.config).unwrap();
        }
    }

    #[test]
    fn r4_transaction_actual_process_exit_leaves_recoverable_durable_receipt() {
        for step in [0, 2, 4] {
            let fixture = Fixture::new();
            let before = fixture.versions();
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "operations::uci_transaction::tests::transaction_process_exit_fixture",
                    "--ignored",
                    "--test-threads=1",
                ])
                .env("CAKE_TRANSACTION_TEST_ROOT", &fixture.root)
                .env("CAKE_TRANSACTION_TEST_STEP", step.to_string())
                .output()
                .unwrap();
            assert_eq!(
                child.status.code(),
                Some(78),
                "child did not exit at the requested boundary: {}",
                String::from_utf8_lossy(&child.stderr)
            );
            recover(&fixture.config).unwrap();
            fixture.assert_versions(&before, true);
        }
    }

    #[test]
    #[ignore = "subprocess fixture, invoked explicitly by the bounded parent test"]
    fn transaction_process_exit_fixture() {
        let root = PathBuf::from(
            std::env::var_os("CAKE_TRANSACTION_TEST_ROOT")
                .expect("explicit controlled fixture required"),
        );
        assert!(root.starts_with(std::env::temp_dir()));
        assert!(root
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("cake-r4-txn-"));
        assert_eq!(
            fs::read(root.join("fixture-owner")).unwrap(),
            b"cake transaction controlled test\n"
        );
        let step = std::env::var("CAKE_TRANSACTION_TEST_STEP")
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let fixture = Fixture {
            config: root.join("config"),
            root,
        };
        let config = fixture.prepare(true);
        let journal = Journal::prepare(&config).unwrap();
        journal
            .publish(&config, |at| {
                if at == step {
                    unsafe { libc::_exit(78) }
                }
                Ok(())
            })
            .unwrap();
        panic!("requested exit point was not reached");
    }

    #[test]
    fn r4_transaction_concurrent_commit_in_exchange_gap_is_returned_not_replaced_by_old_baseline() {
        let fixture = Fixture::new();
        let config = fixture.prepare(true);
        let journal = Journal::prepare(&config).unwrap();
        let mut foreign = Vec::new();
        let error = journal
            .publish(&config, |at| {
                if at == 1 {
                    foreign = fixture.foreign_commit("cake-autorate");
                }
                Ok(())
            })
            .err()
            .unwrap();
        assert_eq!(error, "uci-transaction-concurrent-commit-displaced");
        journal.rollback().unwrap();
        assert_eq!(
            fs::read(fixture.config.join("cake-autorate")).unwrap(),
            foreign
        );
        assert_eq!(
            fs::read(fixture.config.join("sqm")).unwrap(),
            config.original_bytes("sqm").unwrap()
        );
    }

    #[test]
    fn r4_transaction_later_commit_refuses_pair_rollback_without_touching_either_member() {
        let fixture = Fixture::new();
        let published = publish(fixture.prepare(true)).unwrap();
        fixture.foreign_commit("sqm");
        let before_recovery = fixture.versions();
        assert!(published.attest().is_err());
        drop(published);
        assert_eq!(
            recover(&fixture.config).err().unwrap(),
            "uci-transaction-recovery-conflict"
        );
        fixture.assert_versions(&before_recovery, false);
        assert!(fixture.config.join(".start-uci/pending/ready").exists());
    }

    #[test]
    fn r4_transaction_recovery_reports_restored_foreign_commit_without_silent_rebase() {
        let fixture = Fixture::new();
        let config = fixture.prepare(true);
        let journal = Journal::prepare(&config).unwrap();
        let mut foreign = Vec::new();
        assert!(journal
            .publish(&config, |at| {
                if at == 1 {
                    foreign = fixture.foreign_commit("cake-autorate");
                }
                Ok(())
            })
            .is_err());
        drop(journal);
        drop(config);
        assert_eq!(
            recover(&fixture.config).err().unwrap(),
            "uci-transaction-concurrent-commit-preserved"
        );
        assert_eq!(
            fs::read(fixture.config.join("cake-autorate")).unwrap(),
            foreign
        );
        assert!(!fixture.config.join(".start-uci/pending").exists());
    }

    #[test]
    fn r4_transaction_commit_in_rollback_gap_is_returned_and_recovery_stays_explicit() {
        let fixture = Fixture::new();
        let published = publish(fixture.prepare(true)).unwrap();
        let mut foreign = Vec::new();
        assert!(published
            .journal
            .as_ref()
            .unwrap()
            .rollback_with(|index| {
                if index == 1 {
                    foreign = fixture.foreign_commit("sqm");
                }
                Ok(())
            })
            .is_err());
        assert_eq!(fs::read(fixture.config.join("sqm")).unwrap(), foreign);
        assert!(fixture.config.join(".start-uci/pending/ready").exists());
    }

    #[test]
    fn r4_transaction_partial_preparation_is_cleaned_without_touching_public_sources() {
        for stage in 0..5 {
            let fixture = Fixture::new();
            let before = fixture.versions();
            let root = Root::open(&fixture.config).unwrap();
            let pending = Directory::open(&root.root.path.join("pending"), true, true).unwrap();
            if stage > 0 {
                write_new(
                    &pending,
                    "owner",
                    if stage == 1 { &OWNER[..5] } else { OWNER },
                )
                .unwrap();
            }
            if stage == 3 {
                write_new(&pending, "intent", &RECEIPT[..5]).unwrap();
            }
            if stage > 3 {
                write_receipt(
                    &pending,
                    "intent",
                    json!({"schema":1,"entries": (0..2).map(|i|
                    json!({"name":PACKAGES[i],"before":before[i].json()})).collect::<Vec<_>>() }),
                )
                .unwrap();
                write_new(&pending, "cake-autorate.swap", b"partial owned candidate").unwrap();
                write_new(&pending, "ready.tmp", &RECEIPT[..3]).unwrap();
                write_new(&pending, "accepted.tmp", &ACCEPTED[..3]).unwrap();
            }
            drop(pending);
            drop(root);
            recover(&fixture.config).unwrap();
            fixture.assert_versions(&before, false);
            assert!(!fixture.config.join(".start-uci/pending").exists());
        }
    }

    #[test]
    fn r4_transaction_accepted_cleanup_interruption_never_rolls_back_committed_files() {
        for garbage in [false, true] {
            let fixture = Fixture::new();
            let published = publish(fixture.prepare(true)).unwrap();
            let after = fixture.versions();
            let journal = published.journal.as_ref().unwrap();
            write_new(&journal.pending, "accepted", ACCEPTED).unwrap();
            sync(&journal.pending).unwrap();
            if garbage {
                rename(
                    &journal.root.root,
                    "pending",
                    &journal.root.root,
                    "garbage",
                    false,
                )
                .unwrap();
                fs::remove_file(fixture.config.join(".start-uci/garbage/intent")).unwrap();
                fs::remove_file(fixture.config.join(".start-uci/garbage/ready")).unwrap();
            }
            drop(published);
            recover(&fixture.config).unwrap();
            fixture.assert_versions(&after, false);
        }
    }

    #[test]
    fn r4_transaction_missing_ready_after_publication_never_discards_original_files() {
        let fixture = Fixture::new();
        let published = publish(fixture.prepare(true)).unwrap();
        fs::remove_file(fixture.config.join(".start-uci/pending/ready")).unwrap();
        let after = fixture.versions();
        drop(published);
        assert_eq!(
            recover(&fixture.config).err().unwrap(),
            "uci-transaction-readiness-unproven"
        );
        fixture.assert_versions(&after, false);
        assert!(fixture
            .config
            .join(".start-uci/pending/cake-autorate.swap")
            .exists());
        assert!(fixture.config.join(".start-uci/pending/sqm.swap").exists());
    }

    #[test]
    fn r4_transaction_corrupt_or_replaced_backup_is_not_restored_as_a_foreign_commit() {
        for replaced in [false, true] {
            let fixture = Fixture::new();
            let published = publish(fixture.prepare(true)).unwrap();
            let swap = fixture.config.join(".start-uci/pending/sqm.swap");
            if replaced {
                fs::remove_file(&swap).unwrap();
            }
            fs::write(&swap, b"unexpected private backup bytes").unwrap();
            let after = fixture.versions();
            drop(published);
            assert!(recover(&fixture.config).is_err());
            fixture.assert_versions(&after, false);
            assert_eq!(fs::read(swap).unwrap(), b"unexpected private backup bytes");
        }
    }

    #[test]
    fn r4_transaction_rejects_foreign_entries_links_and_corrupt_receipts_without_cleanup() {
        for case in 0..5 {
            let fixture = Fixture::new();
            let config = fixture.prepare(true);
            let journal = Journal::prepare(&config).unwrap();
            let ready = journal.pending.path.join("ready");
            let foreign = fixture.root.join("foreign-file");
            fs::write(&foreign, b"foreign retained bytes").unwrap();
            match case {
                0 => {
                    fs::write(journal.pending.path.join("foreign-note"), b"untouched").unwrap();
                }
                1 => {
                    fs::remove_file(&ready).unwrap();
                    symlink(&foreign, &ready).unwrap();
                }
                2 => {
                    fs::hard_link(&ready, fixture.root.join("foreign-hardlink")).unwrap();
                }
                3 => {
                    fs::write(&ready, b"broken-private-fixture-receipt").unwrap();
                }
                _ => {
                    fs::set_permissions(&ready, fs::Permissions::from_mode(0o666)).unwrap();
                }
            }
            let original = fixture.versions();
            drop(journal);
            drop(config);
            let error = recover(&fixture.config).err().unwrap();
            assert!(!error.contains("private-fixture"));
            assert_eq!(fs::read(foreign).unwrap(), b"foreign retained bytes");
            fixture.assert_versions(&original, false);
            assert!(ready.symlink_metadata().is_ok());
        }
    }

    #[test]
    fn r4_transaction_refuses_busy_authority_and_same_bytes_new_source_inode() {
        let fixture = Fixture::new();
        let config = fixture.prepare(true);
        let journal = Journal::prepare(&config).unwrap();
        assert_eq!(
            Root::open(&fixture.config).err().unwrap(),
            "uci-transaction-busy"
        );
        let path = fixture.root.join("same-bytes");
        fs::write(&path, config.original_bytes("sqm").unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        fs::rename(path, fixture.config.join("sqm")).unwrap();
        assert!(journal.publish(&config, |_| Ok(())).is_err());
        let retained = fixture.versions();
        drop(journal);
        drop(config);
        assert!(recover(&fixture.config).is_err());
        fixture.assert_versions(&retained, false);
    }
}
