//! Private immutable controller inputs. The public process argv stays unchanged;
//! procd carries only the non-secret generation ID in its environment. Creating
//! a record does not register/start a controller or accept a file transaction.
use super::committed_uci::{read_file, Directory, Identity, PreparedConfig};
use super::runtime_health::{safe_name, UciPackage};
use super::uci_transaction::{PublishedConfig, SourceReceipt};
use crate::config_candidate::digest;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub(crate) const GENERATION_ENV: &str = "CAKE_AUTORATE_SERVICE_CONFIG_ID";
const MAGIC: &[u8] = b"cake-autorate frozen controller input v1\n";
const MAX_RECORD: u64 = 2 * 1024 * 1024;
const MAX_STORE: u64 = 8 * 1024 * 1024;
const MAX_ENTRIES: usize = 256;
const BATCH_MAGIC: &[u8] = b"cake-autorate controller batch v1\n";
const BATCH_MAX: u64 = 16 * 1024;
const BATCH_PENDING: &str = "batch.pending";
const BATCH_APPLIED: &str = "batch.applied";
const BATCH_TEMP: &str = "batch.pending.tmp";
const BATCH_RESTORE: &str = "batch.restore";
const BATCH_RESTORE_TEMP: &str = "batch.restore.tmp";
type Result<T> = std::result::Result<T, String>;
fn io<T>(result: std::io::Result<T>) -> Result<T> {
    result.map_err(|_| "controller-input-io".into())
}
fn generation(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn fresh_generation() -> Result<String> {
    let mut bytes = [0u8; 16];
    io(io(File::open("/dev/urandom"))?.read_exact(&mut bytes))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
fn record_id(value: &Value) -> Result<String> {
    let mut value = value.clone();
    value
        .as_object_mut()
        .ok_or("controller-input-record-invalid")?
        .remove("generation");
    let mut bytes = MAGIC.to_vec();
    bytes.extend(serde_json::to_vec(&value).map_err(|_| "controller-input-encode-failed")?);
    Ok(digest(&bytes))
}
fn batch_id(value: &Value) -> Result<String> {
    let mut value = value.clone();
    value
        .as_object_mut()
        .ok_or("controller-batch-invalid")?
        .remove("id");
    let mut bytes = BATCH_MAGIC.to_vec();
    bytes.extend(serde_json::to_vec(&value).map_err(|_| "controller-batch-encode-failed")?);
    Ok(digest(&bytes))
}
fn record_name(instance: &str, id: &str) -> Result<String> {
    if !safe_name(instance) || !generation(id) {
        return Err("controller-input-identity-invalid".into());
    }
    Ok(format!("{instance}.{id}"))
}
fn inventory(directory: &Directory) -> Result<(usize, u64)> {
    directory.attest()?;
    let mut count = 0;
    let mut bytes = 0;
    for entry in io(fs::read_dir(&directory.path))? {
        if count == MAX_ENTRIES {
            return Err("controller-input-store-entry-limit".into());
        }
        let entry = io(entry)?;
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or("controller-input-store-foreign-entry")?;
        let base = name
            .strip_suffix(".accepted.tmp")
            .or_else(|| name.strip_suffix(".accepted"))
            .unwrap_or(name);
        if ![
            "lock",
            BATCH_PENDING,
            BATCH_APPLIED,
            BATCH_TEMP,
            BATCH_RESTORE,
            BATCH_RESTORE_TEMP,
        ]
        .contains(&name)
            && !base
                .rsplit_once('.')
                .is_some_and(|(instance, id)| safe_name(instance) && generation(id))
        {
            return Err("controller-input-store-foreign-entry".into());
        }
        let meta = io(fs::symlink_metadata(entry.path()))?;
        if !meta.is_file()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o077 != 0
            || meta.nlink() != 1
            || meta.len() > MAX_RECORD
        {
            return Err("controller-input-store-unsafe-entry".into());
        }
        bytes += meta.len();
        count += 1;
        if bytes > MAX_STORE {
            return Err("controller-input-store-byte-limit".into());
        }
    }
    directory.attest()?;
    Ok((count, bytes))
}
fn lock_store(root: &Path) -> Result<(Directory, File)> {
    let directory = Directory::open(root, true, true)?;
    let lock = io(OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.join("lock")))?;
    if super::committed_uci::file_identity(&root.join("lock"), &lock, true)?.len != 0 {
        return Err("controller-input-lock-invalid".into());
    }
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("controller-input-store-busy".into());
    }
    inventory(&directory)?;
    Ok((directory, lock))
}

/// The guard deliberately keeps no raw UCI in its Debug representation.
#[derive(Clone)]
pub(crate) struct Guard {
    root: PathBuf,
    config_root: PathBuf,
    instance: String,
    id: String,
    identity: Identity,
    sha256: String,
    sources: SourceReceipt,
}
impl std::fmt::Debug for Guard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ControllerInputGuard")
    }
}
impl Guard {
    pub(crate) fn generation(&self) -> &str {
        &self.id
    }
    pub(crate) fn instance(&self) -> &str {
        &self.instance
    }
    pub(crate) fn stop_context_directory(&self) -> &Path {
        &self.root
    }
    fn name(&self) -> Result<String> {
        record_name(&self.instance, &self.id)
    }
    fn marker(&self) -> Vec<u8> {
        format!("accepted-controller-input-v1 {} {}\n", self.id, self.sha256).into_bytes()
    }
    fn record_attest(&self) -> Result<Directory> {
        let directory = Directory::open(&self.root, true, false)?;
        let (bytes, _, identity) = read_file(&self.root.join(self.name()?), MAX_RECORD, true)?;
        if identity != self.identity || digest(&bytes) != self.sha256 {
            return Err("controller-input-record-changed".into());
        }
        directory.attest()?;
        Ok(directory)
    }
    fn accepted(&self, directory: &Directory) -> Result<bool> {
        let path = directory.path.join(format!("{}.accepted", self.name()?));
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err("controller-input-io".into()),
            Ok(_) => {
                let (bytes, _, _) = read_file(&path, 256, true)?;
                if bytes != self.marker() {
                    return Err("controller-input-acceptance-invalid".into());
                }
                Ok(true)
            }
        }
    }
    pub(crate) fn attest(&self) -> Result<()> {
        let directory = self.record_attest()?;
        if !self.accepted(&directory)? {
            self.sources.attest(&self.config_root)?;
        }
        directory.attest()
    }
    /// A live operation may borrow only an accepted generation, not a valid
    /// but merely staged input whose public source still happens to match.
    pub(crate) fn attest_applied(&self) -> Result<()> {
        let directory = self.record_attest()?;
        if !self.accepted(&directory)? {
            return Err("controller-input-not-applied".into());
        }
        directory.attest()
    }
    /// Old applied input remains Stop authority after a later user commit.
    /// This proves only the sealed record, not that its source is still current.
    pub(crate) fn attest_record(&self) -> Result<()> {
        self.record_attest().map(|_| ())
    }
    /// Caller must prove THIS generation's procd/controller readiness under
    /// lifecycle authority. Acceptance does not accept the UCI file transaction.
    pub(crate) fn mark_accepted(
        &self,
        mut attest_runtime_generation: impl FnMut(&str, &str) -> Result<()>,
    ) -> Result<()> {
        let (directory, _lock) = lock_store(&self.root)?;
        self.record_attest()?;
        attest_runtime_generation(&self.instance, &self.id)?;
        if self.accepted(&directory)? {
            return Ok(());
        }
        self.sources.attest(&self.config_root)?;
        let marker = self.marker();
        let filename = format!("{}.accepted", self.name()?);
        let temporary = format!("{filename}.tmp");
        let path = self.root.join(&filename);
        match fs::symlink_metadata(self.root.join(&temporary)) {
            Ok(_) => {
                let (partial, _, _) = read_file(&self.root.join(&temporary), 256, true)?;
                if !marker.starts_with(&partial) {
                    return Err("controller-input-acceptance-invalid".into());
                }
                io(fs::remove_file(self.root.join(&temporary)))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("controller-input-io".into()),
        }
        let (count, bytes) = inventory(&directory)?;
        if count == MAX_ENTRIES || bytes + marker.len() as u64 > MAX_STORE {
            return Err("controller-input-store-capacity".into());
        }
        let mut file = io(OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.root.join(&temporary)))?;
        io(file.write_all(&marker))?;
        io(file.sync_all())?;
        super::uci_transaction::rename(&directory, &temporary, &directory, &filename, false)?;
        let written = super::committed_uci::file_identity(&path, &file, true)?;
        let proof = self
            .sources
            .attest(&self.config_root)
            .and_then(|_| attest_runtime_generation(&self.instance, &self.id));
        if proof.is_err() {
            if super::committed_uci::file_identity(&path, &file, true)? == written {
                io(fs::remove_file(path))?;
                io(directory.file.sync_all())?;
            }
        }
        proof?;
        self.attest()
    }
    /// Caller proves the controller and its procd definition no longer refer to
    /// this generation. Other generations are not removed by this operation.
    pub(crate) fn retire(
        &self,
        mut attest_unreferenced: impl FnMut(&str, &str) -> Result<()>,
    ) -> Result<()> {
        let (directory, _lock) = lock_store(&self.root)?;
        self.record_attest()?;
        attest_unreferenced(&self.instance, &self.id)?;
        let name = self.name()?;
        let accepted = self.accepted(&directory)?;
        let temporary = self.root.join(format!("{name}.accepted.tmp"));
        let remove_temporary = match fs::symlink_metadata(&temporary) {
            Ok(_) => {
                let (partial, _, _) = read_file(&temporary, 256, true)?;
                if !self.marker().starts_with(&partial) {
                    return Err("controller-input-acceptance-invalid".into());
                }
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => return Err("controller-input-io".into()),
        };
        self.record_attest()?;
        attest_unreferenced(&self.instance, &self.id)?;
        self.record_attest()?;
        if accepted {
            io(fs::remove_file(self.root.join(format!("{name}.accepted"))))?;
        }
        if remove_temporary {
            io(fs::remove_file(temporary))?;
        }
        io(fs::remove_file(self.root.join(name)))?;
        io(directory.file.sync_all())
    }
}

pub(crate) struct Loaded {
    pub(crate) config: crate::Config,
    pub(crate) guard: Guard,
    pub(crate) cake_show: Vec<u8>,
    pub(crate) sqm_show: Vec<u8>,
}

/// Durable desired set across the prepare-start/procd/confirm-started boundary.
/// Pending means intent, never proof of runtime success. Applied remains the
/// Stop recipe registry even when every controller PID has disappeared.
#[derive(Clone)]
pub(crate) struct Batch {
    root: PathBuf,
    config_root: PathBuf,
    name: &'static str,
    bytes: Vec<u8>,
    identity: Identity,
    sources: SourceReceipt,
    generations: BTreeMap<String, String>,
    id: String,
    previous: Option<String>,
    retained: BTreeSet<String>,
    selected: Option<String>,
    supersedes: Option<String>,
    reload: bool,
}
impl std::fmt::Debug for Batch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ControllerBatch")
    }
}
impl Batch {
    pub(crate) fn attest_reinstalled_content(&self) -> Result<()> {
        self.attest_record()?;
        self.sources.attest_reinstalled_content(&self.config_root)?;
        self.attest_record()
    }

    fn supersedes_batch(&self, old: &Batch) -> bool {
        let Some(selected) = self.selected.as_deref() else {
            return false;
        };
        self.supersedes.as_ref() == Some(&old.id)
            && self.previous == old.previous
            && self.selected_change_is(selected)
            && old.selected_change_is(selected)
            && self.retained == old.retained
            && self.generations.keys().eq(old.generations.keys())
            && self
                .retained
                .iter()
                .all(|name| self.generations.get(name) == old.generations.get(name))
    }

    fn supersession_temp(&self, allow_draft: bool) -> Result<Option<(Vec<u8>, Identity)>> {
        let path = self.root.join(BATCH_TEMP);
        if !exists(&path)? {
            return Ok(None);
        }
        let (bytes, _, identity) = read_file(&path, BATCH_MAX, true)?;
        match read_batch_file(&self.root, &self.config_root, BATCH_TEMP) {
            Ok(temp)
                if self.supersedes_batch(&temp) || (allow_draft && temp.supersedes_batch(self)) =>
            {
                Ok(Some((bytes, identity)))
            }
            Err(_)
                if allow_draft
                    && (BATCH_MAGIC.starts_with(&bytes) || bytes.starts_with(BATCH_MAGIC)) =>
            {
                Ok(Some((bytes, identity)))
            }
            _ => Err("controller-batch-supersession-temp-invalid".into()),
        }
    }

    fn cleanup_supersession_temp(&self, directory: &Directory, allow_draft: bool) -> Result<()> {
        let Some((bytes, identity)) = self.supersession_temp(allow_draft)? else {
            return Ok(());
        };
        self.attest_record()?;
        let path = self.root.join(BATCH_TEMP);
        let (current, _, actual) = read_file(&path, BATCH_MAX, true)?;
        if current != bytes || actual != identity {
            return Err("controller-batch-record-changed".into());
        }
        io(fs::remove_file(path))?;
        io(directory.file.sync_all())
    }

    pub(crate) fn cleanup_superseded(
        &self,
        mut attest_transition: impl FnMut() -> Result<()>,
    ) -> Result<()> {
        if !exists(&self.root.join(BATCH_TEMP))? {
            return Ok(());
        }
        let (directory, _lock) = lock_store(&self.root)?;
        self.attest_record()?;
        attest_transition()?;
        self.attest_record()?;
        self.cleanup_supersession_temp(&directory, false)
    }
    pub(crate) fn applied_predecessor(&self) -> Result<Batch> {
        self.attest_record()?;
        if !self.pending() {
            return Ok(self.clone());
        }
        let previous = read_batch_file(&self.root, &self.config_root, BATCH_APPLIED)?;
        if self.previous.as_ref() != Some(&previous.id) {
            return Err("controller-batch-predecessor-mismatch".into());
        }
        previous.attest_record()?;
        Ok(previous)
    }
    pub(crate) fn pending_update(&self) -> Result<Option<Batch>> {
        self.attest_record()?;
        if self.pending() {
            return Ok(Some(self.clone()));
        }
        if !exists(&self.root.join(BATCH_PENDING))? {
            return Ok(None);
        }
        let pending = read_batch_file(&self.root, &self.config_root, BATCH_PENDING)?;
        if pending.previous.as_ref() == Some(&self.id) {
            pending.attest_record()?;
            Ok(Some(pending))
        } else if self.previous.as_ref() == Some(&pending.id) {
            Ok(None)
        } else {
            Err("controller-batch-predecessor-mismatch".into())
        }
    }
    pub(crate) fn generations(&self) -> &BTreeMap<String, String> {
        &self.generations
    }
    pub(crate) fn same_publication(&self, other: &Self) -> bool {
        self.root == other.root && self.config_root == other.config_root && self.id == other.id
    }
    pub(crate) fn pending(&self) -> bool {
        self.name == BATCH_PENDING
    }
    /// An applied record can coexist with unfinished marker-last restoration.
    /// Such a record is recovery authority, not a baseline for a fresh reload.
    pub(crate) fn attest_settled(&self) -> Result<()> {
        self.attest_record()?;
        if self.pending() || self.pending_update()?.is_some() || restore_pending(&self.root)? {
            return Err("controller-batch-not-settled".into());
        }
        self.attest_record()
    }
    /// Ordinary Stop may finish only its own complete publication. A selected
    /// replacement still belongs to its parent Native Apply recovery protocol.
    pub(crate) fn ordinary_stop_receipt(&self) -> Result<SourceReceipt> {
        self.attest_record()?;
        if self.pending() && self.previous.is_some() {
            return Err("controller-batch-update-recovery-required".into());
        }
        Ok(self.sources.clone())
    }
    pub(crate) fn updates_only(&self, instance: &str) -> bool {
        self.pending() && self.selected_change_is(instance)
    }
    pub(crate) fn is_reload_update(&self) -> bool {
        self.pending() && self.reload
    }
    /// Bind serialized sidecar recovery to this durable ordinary reload, not
    /// to a newly generated ID on every process restart.
    pub(crate) fn reload_operation_id(&self) -> Result<&str> {
        self.attest()?;
        if !self.is_reload_update() || restore_pending(&self.root)? {
            return Err("controller-batch-not-pending-reload".into());
        }
        self.id
            .get(..32)
            .ok_or_else(|| "controller-batch-invalid".into())
    }
    /// Bind a replayed configuration delta to this ordinary reload's exact
    /// desired/retained partition before reopening its file publication.
    pub(crate) fn attest_reload_plan(
        &self,
        expected: &[String],
        retained: &BTreeMap<String, String>,
    ) -> Result<SourceReceipt> {
        self.attest()?;
        if !self.is_reload_update()
            || restore_pending(&self.root)?
            || expected.len() != self.generations.len()
            || expected.iter().collect::<BTreeSet<_>>().len() != expected.len()
            || expected
                .iter()
                .any(|name| !self.generations.contains_key(name))
            || retained.len() != self.retained.len()
            || retained.iter().any(|(name, id)| {
                !self.retained.contains(name) || self.generations.get(name) != Some(id)
            })
        {
            return Err("controller-batch-reload-plan-mismatch".into());
        }
        self.load_inputs()?;
        self.attest()?;
        Ok(self.sources.clone())
    }
    // Content relationship also applies to a draft or displaced temporary;
    // it must not by itself grant pending/publication authority.
    fn selected_change_is(&self, instance: &str) -> bool {
        if self.reload {
            return false;
        }
        if let Some(selected) = &self.selected {
            return self.previous.is_some() && selected == instance;
        }
        self.previous.is_some()
            && self.generations.contains_key(instance)
            && !self.retained.contains(instance)
            && self
                .generations
                .keys()
                .all(|name| name == instance || self.retained.contains(name))
    }
    pub(crate) fn attest_record(&self) -> Result<()> {
        let directory = Directory::open(&self.root, true, false)?;
        let (bytes, _, identity) = read_file(&self.root.join(self.name), BATCH_MAX, true)?;
        if bytes != self.bytes || identity != self.identity {
            return Err("controller-batch-record-changed".into());
        }
        if self.pending() {
            if let Some(previous) = &self.previous {
                let applied = read_batch_file(&self.root, &self.config_root, BATCH_APPLIED)?;
                if &applied.id != previous
                    || self
                        .retained
                        .iter()
                        .any(|name| applied.generations.get(name) != self.generations.get(name))
                {
                    return Err("controller-batch-predecessor-mismatch".into());
                }
            } else if exists(&self.root.join(BATCH_APPLIED))? {
                return Err("controller-batch-predecessor-mismatch".into());
            }
        }
        directory.attest()
    }
    pub(crate) fn attest(&self) -> Result<()> {
        self.attest_record()?;
        if self.pending() {
            self.sources.attest(&self.config_root)?;
        }
        self.attest_record()
    }
    pub(crate) fn load_inputs(&self) -> Result<BTreeMap<String, Loaded>> {
        self.attest_record()?;
        let inputs = self
            .generations
            .iter()
            .map(|(instance, id)| {
                let input = load_for_recovery(instance, id, &self.root, &self.config_root)?;
                if self.retained.contains(instance) {
                    let directory = Directory::open(&self.root, true, false)?;
                    if !input.guard.accepted(&directory)? {
                        return Err("controller-batch-retained-input-unaccepted".into());
                    }
                } else if input.guard.sources.encode() != self.sources.encode() {
                    return Err("controller-batch-input-source-mismatch".into());
                }
                Ok((instance.clone(), input))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        self.attest_record()?;
        Ok(inputs)
    }
    /// Complete only after the exact desired set is ready. Each input marker
    /// and the UCI acceptance are separately rechecked; interrupted acceptance
    /// before the final rename leaves the batch pending. A lost final ACK can
    /// reload the applied record and reprove that same generation set.
    pub(crate) fn accept(
        &self,
        mut attest_runtime: impl FnMut(&BTreeMap<String, String>) -> Result<()>,
    ) -> Result<()> {
        self.attest()?;
        attest_runtime(&self.generations)?;
        self.cleanup_superseded(|| attest_runtime(&self.generations))?;
        if !self.pending() {
            let (directory, _lock) = lock_store(&self.root)?;
            self.attest_record()?;
            attest_runtime(&self.generations)?;
            if exists(&self.root.join(BATCH_RESTORE))? {
                return self.cleanup_restore(&directory);
            }
            return self.cleanup_predecessor(&directory);
        }
        if exists(&self.root.join(BATCH_RESTORE))? || exists(&self.root.join(BATCH_RESTORE_TEMP))? {
            return Err("controller-batch-restore-pending".into());
        }
        for (_, input) in self.load_inputs()? {
            input.guard.mark_accepted(|_, _| {
                self.attest()?;
                attest_runtime(&self.generations)
            })?;
        }
        super::uci_transaction::accept_receipt(&self.config_root, &self.sources, || {
            self.attest()?;
            attest_runtime(&self.generations)
        })?;
        let (directory, _lock) = lock_store(&self.root)?;
        self.attest()?;
        attest_runtime(&self.generations)?;
        self.attest()?;
        super::uci_transaction::rename(
            &directory,
            BATCH_PENDING,
            &directory,
            BATCH_APPLIED,
            self.previous.is_some(),
        )?;
        let loaded =
            load_batch(&self.root, &self.config_root)?.ok_or("controller-batch-missing")?;
        if loaded.pending() || loaded.bytes != self.bytes {
            return Err("controller-batch-acceptance-changed".into());
        }
        loaded.attest_record()?;
        attest_runtime(&self.generations)?;
        self.sources.attest(&self.config_root)?;
        loaded.cleanup_predecessor(&directory)
    }

    fn cleanup_predecessor(&self, directory: &Directory) -> Result<()> {
        if self.pending() || !exists(&self.root.join(BATCH_PENDING))? {
            return Ok(());
        }
        self.attest_record()?;
        let previous = read_batch_file(&self.root, &self.config_root, BATCH_PENDING)?;
        if self.previous.as_ref() != Some(&previous.id) {
            return Err("controller-batch-predecessor-mismatch".into());
        }
        let (bytes, _, identity) = read_file(&self.root.join(BATCH_PENDING), BATCH_MAX, true)?;
        if bytes != previous.bytes || identity != previous.identity {
            return Err("controller-batch-record-changed".into());
        }
        self.attest_record()?;
        io(fs::remove_file(self.root.join(BATCH_PENDING)))?;
        io(directory.file.sync_all())
    }

    /// Native Apply recovery has restored the old selected configuration/SQM
    /// and owns the transition, but may still need to register the old process.
    /// This does not claim Ready; it makes the old set the durable desired set.
    pub(crate) fn begin_restore(
        &self,
        mut attest_restore_authority: impl FnMut(&BTreeMap<String, String>) -> Result<()>,
    ) -> Result<Batch> {
        if !self.pending() || self.previous.is_none() {
            return Err("controller-batch-update-authority-invalid".into());
        }
        let (directory, _lock) = lock_store(&self.root)?;
        self.attest_record()?;
        let previous = read_batch_file(&self.root, &self.config_root, BATCH_APPLIED)?;
        self.require_accepted_predecessor(&previous, &directory)?;
        attest_restore_authority(&previous.generations)?;
        self.attest_record()?;
        attest_restore_authority(&previous.generations)?;
        self.attest_record()?;
        self.cleanup_supersession_temp(&directory, true)?;
        self.write_restore_marker(&previous, &directory)?;
        let current =
            load_batch(&self.root, &self.config_root)?.ok_or("controller-batch-missing")?;
        if current.pending() || current.id != previous.id {
            return Err("controller-batch-restore-invalid".into());
        }
        Ok(current)
    }

    fn require_accepted_predecessor(&self, previous: &Batch, directory: &Directory) -> Result<()> {
        if self.previous.as_ref() != Some(&previous.id) {
            return Err("controller-batch-predecessor-mismatch".into());
        }
        for input in previous.load_inputs()?.values() {
            if !input.guard.accepted(directory)? {
                return Err("controller-batch-restored-input-unaccepted".into());
            }
        }
        Ok(())
    }

    fn write_restore_marker(&self, previous: &Batch, directory: &Directory) -> Result<()> {
        let marker =
            format!("restore-controller-batch-v1 {} {}\n", self.id, previous.id).into_bytes();
        let path = self.root.join(BATCH_RESTORE);
        if exists(&path)? {
            let (bytes, _, _) = read_file(&path, 256, true)?;
            return if bytes == marker {
                Ok(())
            } else {
                Err("controller-batch-restore-invalid".into())
            };
        }
        let temp = self.root.join(BATCH_RESTORE_TEMP);
        if exists(&temp)? {
            let (bytes, _, _) = read_file(&temp, 256, true)?;
            if !marker.starts_with(&bytes) {
                return Err("controller-batch-restore-invalid".into());
            }
            io(fs::remove_file(&temp))?;
        }
        let (count, bytes) = inventory(directory)?;
        if count == MAX_ENTRIES || bytes + marker.len() as u64 > MAX_STORE {
            return Err("controller-input-store-capacity".into());
        }
        let mut file = io(OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(temp))?;
        io(file.write_all(&marker))?;
        io(file.sync_all())?;
        super::uci_transaction::rename(
            directory,
            BATCH_RESTORE_TEMP,
            directory,
            BATCH_RESTORE,
            false,
        )
    }

    fn cleanup_restore(&self, directory: &Directory) -> Result<()> {
        self.attest_record()?;
        let (pending_id, previous_id, bytes, identity) = read_restore_marker(&self.root)?;
        if self.pending() || self.id != previous_id {
            return Err("controller-batch-restore-invalid".into());
        }
        if exists(&self.root.join(BATCH_PENDING))? {
            let pending = read_batch_file(&self.root, &self.config_root, BATCH_PENDING)?;
            if pending.id != pending_id || pending.previous.as_ref() != Some(&self.id) {
                return Err("controller-batch-restore-invalid".into());
            }
            pending.attest_record()?;
            io(fs::remove_file(self.root.join(BATCH_PENDING)))?;
            io(directory.file.sync_all())?;
        }
        // Marker last: if cleanup is interrupted after pending disappears,
        // load_batch still selects the known old applied set.
        self.attest_record()?;
        let (_, _, current_bytes, current_identity) = read_restore_marker(&self.root)?;
        if bytes != current_bytes || identity != current_identity {
            return Err("controller-batch-restore-invalid".into());
        }
        io(fs::remove_file(self.root.join(BATCH_RESTORE)))?;
        io(directory.file.sync_all())
    }

    /// Selected-operation recovery restores its own UCI/runtime transaction
    /// first. Only a proof of the exact old generation set may abandon this
    /// not-yet-accepted update. This never rolls back files or starts processes.
    pub(crate) fn abandon_update(
        &self,
        mut attest_restored: impl FnMut(&BTreeMap<String, String>) -> Result<()>,
    ) -> Result<()> {
        if !self.pending() || self.previous.is_none() {
            return Err("controller-batch-update-authority-invalid".into());
        }
        let (directory, _lock) = lock_store(&self.root)?;
        self.attest_record()?;
        let previous = read_batch_file(&self.root, &self.config_root, BATCH_APPLIED)?;
        self.require_accepted_predecessor(&previous, &directory)?;
        attest_restored(&previous.generations)?;
        previous.attest_record()?;
        self.attest_record()?;
        attest_restored(&previous.generations)?;
        self.attest_record()?;
        self.cleanup_supersession_temp(&directory, true)?;
        self.write_restore_marker(&previous, &directory)?;
        previous.cleanup_restore(&directory)?;
        previous.attest_record()?;
        attest_restored(&previous.generations)
    }
    /// Only lifecycle Stop/recovery may retire this registry, after proving
    /// both procd/process absence and absence of every owned SQM runtime.
    pub(crate) fn retire(
        &self,
        mut attest_absent: impl FnMut(&BTreeMap<String, String>) -> Result<()>,
    ) -> Result<()> {
        if self.pending() && self.previous.is_some() {
            // Keep both authorities until selected-operation recovery proves
            // which generation to retain. Never tear down the predecessor
            // first and strand a pending recipe across a process crash.
            return Err("controller-batch-update-recovery-required".into());
        }
        let (directory, _lock) = lock_store(&self.root)?;
        self.attest_record()?;
        attest_absent(&self.generations)?;
        self.attest_record()?;
        attest_absent(&self.generations)?;
        self.attest_record()?;
        if exists(&self.root.join(BATCH_RESTORE))? {
            self.cleanup_restore(&directory)?;
        } else {
            self.cleanup_predecessor(&directory)?;
        }
        io(fs::remove_file(self.root.join(self.name)))?;
        io(directory.file.sync_all())
    }
}

fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err("controller-input-io".into()),
    }
}

fn read_restore_marker(root: &Path) -> Result<(String, String, Vec<u8>, Identity)> {
    let (bytes, _, identity) = read_file(&root.join(BATCH_RESTORE), 256, true)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "controller-batch-restore-invalid")?;
    let body = text
        .strip_prefix("restore-controller-batch-v1 ")
        .and_then(|s| s.strip_suffix('\n'))
        .ok_or("controller-batch-restore-invalid")?;
    let (pending, previous) = body
        .split_once(' ')
        .ok_or("controller-batch-restore-invalid")?;
    if !generation(pending) || !generation(previous) {
        return Err("controller-batch-restore-invalid".into());
    }
    Ok((pending.into(), previous.into(), bytes, identity))
}

pub(crate) fn load_batch(root: &Path, config_root: &Path) -> Result<Option<Batch>> {
    if !root.is_absolute() || !config_root.is_absolute() {
        return Err("controller-input-root-invalid".into());
    }
    if !exists(root)? {
        return Ok(None);
    }
    let directory = Directory::open(root, true, false)?;
    inventory(&directory)?;
    let pending = exists(&root.join(BATCH_PENDING))?;
    let applied = exists(&root.join(BATCH_APPLIED))?;
    if exists(&root.join(BATCH_RESTORE_TEMP))? {
        return Err("controller-batch-recovery-required".into());
    }
    if exists(&root.join(BATCH_RESTORE))? {
        if exists(&root.join(BATCH_TEMP))? {
            return Err("controller-batch-recovery-required".into());
        }
        let (pending_id, previous_id, _, _) = read_restore_marker(root)?;
        if !applied {
            return Err("controller-batch-restore-invalid".into());
        }
        let previous = read_batch_file(root, config_root, BATCH_APPLIED)?;
        if previous.id != previous_id {
            return Err("controller-batch-restore-invalid".into());
        }
        if pending {
            let pending = read_batch_file(root, config_root, BATCH_PENDING)?;
            if pending.id != pending_id || pending.previous.as_ref() != Some(&previous.id) {
                return Err("controller-batch-restore-invalid".into());
            }
        }
        previous.attest_record()?;
        directory.attest()?;
        return Ok(Some(previous));
    }
    let batch = match (pending, applied) {
        (true, true) => {
            let pending = read_batch_file(root, config_root, BATCH_PENDING)?;
            let applied = read_batch_file(root, config_root, BATCH_APPLIED)?;
            if pending.previous.as_ref() == Some(&applied.id) {
                pending
            } else if applied.previous.as_ref() == Some(&pending.id) {
                // Atomic exchange succeeded; the former applied registry is
                // merely awaiting cleanup under its recorded content identity.
                applied
            } else {
                return Err("controller-batch-predecessor-mismatch".into());
            }
        }
        (true, false) => read_batch_file(root, config_root, BATCH_PENDING)?,
        (false, true) => read_batch_file(root, config_root, BATCH_APPLIED)?,
        _ => {
            if exists(&root.join(BATCH_TEMP))? {
                return Err("controller-batch-recovery-required".into());
            }
            return Ok(None);
        }
    };
    batch.attest_record()?;
    batch.supersession_temp(false)?;
    directory.attest()?;
    Ok(Some(batch))
}

/// Narrow recovery reader for the Native Apply owner. General readiness must
/// still reject a partial restore marker. The owner may resume that marker
/// only after matching the old frozen configuration and proving restoration.
pub(crate) fn load_batch_for_selected_preparation(
    root: &Path,
    config_root: &Path,
) -> Result<Option<Batch>> {
    if !root.is_absolute() || !config_root.is_absolute() {
        return Err("controller-input-root-invalid".into());
    }
    if exists(&root.join(BATCH_TEMP))? && !restore_pending(root)? {
        let directory = Directory::open(root, true, false)?;
        inventory(&directory)?;
        let pending = read_batch_file(root, config_root, BATCH_PENDING)?;
        pending.attest_record()?;
        if pending.previous.is_none() {
            return Err("controller-batch-recovery-required".into());
        }
        pending.supersession_temp(true)?;
        directory.attest()?;
        return Ok(Some(pending));
    }
    if !exists(&root.join(BATCH_RESTORE_TEMP))? {
        return load_batch(root, config_root);
    }
    let directory = Directory::open(root, true, false)?;
    inventory(&directory)?;
    if exists(&root.join(BATCH_TEMP))? {
        return Err("controller-batch-recovery-required".into());
    }
    let pending = read_batch_file(root, config_root, BATCH_PENDING)?;
    let previous = read_batch_file(root, config_root, BATCH_APPLIED)?;
    if pending.previous.as_ref() != Some(&previous.id) {
        return Err("controller-batch-predecessor-mismatch".into());
    }
    let marker = format!(
        "restore-controller-batch-v1 {} {}\n",
        pending.id, previous.id
    )
    .into_bytes();
    let (partial, _, _) = read_file(&root.join(BATCH_RESTORE_TEMP), 256, true)?;
    if !marker.starts_with(&partial) {
        return Err("controller-batch-restore-invalid".into());
    }
    pending.attest_record()?;
    directory.attest()?;
    Ok(Some(pending))
}

pub(crate) fn restore_pending(root: &Path) -> Result<bool> {
    Ok(exists(&root.join(BATCH_RESTORE))? || exists(&root.join(BATCH_RESTORE_TEMP))?)
}

/// Run under completed lifecycle authority. Keep every applied/pending root;
/// callers must separately prove that no process or procd definition refers to
/// a candidate. Partial/corrupt records remain evidence, never deletion authority.
pub(crate) fn collect_unreferenced(
    root: &Path,
    config_root: &Path,
    mut unreferenced: impl FnMut(&Loaded) -> Result<bool>,
) -> Result<usize> {
    if !root.is_absolute() || !config_root.is_absolute() {
        return Err("controller-input-root-invalid".into());
    }
    if !exists(root)? {
        return Ok(0);
    }
    let (directory, lock) = lock_store(root)?;
    // Interrupted draft/restore cleanup belongs to its transaction, not GC.
    if [BATCH_TEMP, BATCH_RESTORE, BATCH_RESTORE_TEMP]
        .iter()
        .any(|name| exists(&root.join(name)).unwrap_or(true))
    {
        return Ok(0);
    }
    load_batch(root, config_root)?;
    let mut roots = BTreeMap::new();
    let mut referenced = BTreeSet::new();
    for name in [BATCH_APPLIED, BATCH_PENDING] {
        if !exists(&root.join(name))? {
            continue;
        }
        let batch = read_batch_file(root, config_root, name)?;
        for (instance, id) in batch.generations {
            referenced.insert((instance, id));
        }
        roots.insert(name, (batch.bytes, batch.identity));
    }
    let mut names = BTreeSet::new();
    for entry in io(fs::read_dir(root))? {
        let entry = io(entry)?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err("controller-input-store-foreign-entry".into());
        };
        let Some((instance, id)) = name.rsplit_once('.') else {
            continue;
        };
        if safe_name(instance) && generation(id) {
            names.insert((instance.to_string(), id.to_string()));
        }
    }
    directory.attest()?;
    drop(lock);
    let attest_roots = || -> Result<()> {
        directory.attest()?;
        for name in [BATCH_TEMP, BATCH_RESTORE, BATCH_RESTORE_TEMP] {
            if exists(&root.join(name))? {
                return Err("controller-input-gc-roots-changed".into());
            }
        }
        for name in [BATCH_APPLIED, BATCH_PENDING] {
            match roots.get(name) {
                Some((expected, identity)) => {
                    let (bytes, _, actual) = read_file(&root.join(name), BATCH_MAX, true)?;
                    if &bytes != expected || &actual != identity {
                        return Err("controller-input-gc-roots-changed".into());
                    }
                }
                None if exists(&root.join(name))? => {
                    return Err("controller-input-gc-roots-changed".into())
                }
                None => {}
            }
        }
        Ok(())
    };
    let mut removed = 0;
    for (instance, id) in names {
        if referenced.contains(&(instance.clone(), id.clone())) {
            continue;
        }
        attest_roots()?;
        let Ok(input) = load_for_recovery(&instance, &id, root, config_root) else {
            continue;
        };
        if !unreferenced(&input)? {
            continue;
        }
        input.guard.retire(|_, _| {
            attest_roots()?;
            if !unreferenced(&input)? {
                return Err("controller-input-gc-reference-changed".into());
            }
            attest_roots()
        })?;
        removed += 1;
    }
    attest_roots()?;
    Ok(removed)
}

fn read_batch_file(root: &Path, config_root: &Path, name: &'static str) -> Result<Batch> {
    let (bytes, _, identity) = read_file(&root.join(name), BATCH_MAX, true)?;
    let value: Value = serde_json::from_slice(
        bytes
            .strip_prefix(BATCH_MAGIC)
            .ok_or("controller-batch-invalid")?,
    )
    .map_err(|_| "controller-batch-invalid")?;
    if value["schema"].as_u64() != Some(1)
        || value["id"].as_str() != Some(batch_id(&value)?.as_str())
    {
        return Err("controller-batch-invalid".into());
    }
    let sources = SourceReceipt::decode(&value["sources"])?;
    let entries = value["generations"]
        .as_object()
        .filter(|map| map.len() <= 64)
        .ok_or("controller-batch-invalid")?;
    let mut generations = BTreeMap::new();
    for (instance, id) in entries {
        let id = id.as_str().ok_or("controller-batch-invalid")?;
        record_name(instance, id)?;
        generations.insert(instance.clone(), id.into());
    }
    let previous = match value.get("previous") {
        None => None,
        Some(value) => Some(
            value
                .as_str()
                .filter(|s| generation(s))
                .ok_or("controller-batch-predecessor-invalid")?
                .to_string(),
        ),
    };
    let mut retained = BTreeSet::new();
    if let Some(value) = value.get("retained") {
        for name in value
            .as_array()
            .filter(|v| v.len() <= 64)
            .ok_or("controller-batch-retained-invalid")?
        {
            let name = name
                .as_str()
                .filter(|name| generations.contains_key(*name))
                .ok_or("controller-batch-retained-invalid")?;
            if !retained.insert(name.to_string()) {
                return Err("controller-batch-retained-invalid".into());
            }
        }
        if previous.is_none() {
            return Err("controller-batch-predecessor-invalid".into());
        }
    }
    if value.get("supersedes").is_some() && (previous.is_none() || !value["selected"].is_string()) {
        return Err("controller-batch-supersession-invalid".into());
    }
    let reload = match value.get("purpose") {
        None => false, // Existing full-start and native selected records.
        Some(Value::String(purpose))
            if purpose == "reload"
                && previous.is_some()
                && value.get("retained").is_some()
                && value.get("selected").is_none()
                && value.get("supersedes").is_none() =>
        {
            true
        }
        Some(_) => return Err("controller-batch-purpose-invalid".into()),
    };
    let batch = Batch {
        root: root.into(),
        config_root: config_root.into(),
        name,
        bytes,
        identity,
        sources,
        id: value["id"]
            .as_str()
            .ok_or("controller-batch-invalid")?
            .into(),
        selected: match value.get("selected") {
            None => None,
            Some(value) => {
                let selected = value
                    .as_str()
                    .filter(|name| safe_name(name))
                    .ok_or("controller-batch-selected-invalid")?;
                if previous.is_none()
                    || retained.contains(selected)
                    || generations
                        .keys()
                        .any(|name| name != selected && !retained.contains(name))
                {
                    return Err("controller-batch-selected-invalid".into());
                }
                Some(selected.into())
            }
        },
        generations,
        previous,
        retained,
        supersedes: match value.get("supersedes") {
            None => None,
            Some(value) => Some(
                value
                    .as_str()
                    .filter(|id| generation(id))
                    .ok_or("controller-batch-supersession-invalid")?
                    .into(),
            ),
        },
        reload,
    };
    Ok(batch)
}

pub(crate) fn store_batch(
    published: &PublishedConfig,
    expected: &[String],
    inputs: &BTreeMap<String, Guard>,
    root: &Path,
    config_root: &Path,
) -> Result<Batch> {
    if !root.is_absolute() || !config_root.is_absolute() {
        return Err("controller-input-root-invalid".into());
    }
    let sources = published.source_receipt()?;
    sources.attest(config_root)?;
    if expected.len() > 64
        || expected.len() != inputs.len()
        || expected
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            != expected.len()
        || expected.iter().any(|name| !inputs.contains_key(name))
    {
        return Err("controller-batch-membership-mismatch".into());
    }
    let mut generations = BTreeMap::new();
    for (instance, input) in inputs {
        if input.instance != *instance
            || input.root != root
            || input.config_root != config_root
            || input.sources.encode() != sources.encode()
        {
            return Err("controller-batch-input-mismatch".into());
        }
        input.attest()?;
        generations.insert(instance.clone(), input.generation().to_string());
    }
    let mut value = json!({"schema":1,"sources":sources.encode(),"generations":generations});
    value["id"] = json!(batch_id(&value)?);
    let mut bytes = BATCH_MAGIC.to_vec();
    bytes.extend(serde_json::to_vec(&value).map_err(|_| "controller-batch-encode-failed")?);
    if bytes.len() as u64 > BATCH_MAX {
        return Err("controller-batch-too-large".into());
    }
    let (directory, _lock) = lock_store(root)?;
    if [
        BATCH_PENDING,
        BATCH_APPLIED,
        BATCH_TEMP,
        BATCH_RESTORE,
        BATCH_RESTORE_TEMP,
    ]
    .iter()
    .any(|name| exists(&root.join(name)).unwrap_or(true))
    {
        return Err("controller-batch-recovery-required".into());
    }
    let (count, used) = inventory(&directory)?;
    if count == MAX_ENTRIES || used + bytes.len() as u64 > MAX_STORE {
        return Err("controller-input-store-capacity".into());
    }
    published.attest()?;
    for input in inputs.values() {
        input.attest()?;
    }
    let mut file = io(OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.join(BATCH_TEMP)))?;
    io(file.write_all(&bytes))?;
    io(file.sync_all())?;
    super::uci_transaction::rename(&directory, BATCH_TEMP, &directory, BATCH_PENDING, false)?;
    let batch = load_batch(root, config_root)?.ok_or("controller-batch-missing")?;
    batch.attest()?;
    Ok(batch)
}

/// Publish the entire ordinary-reload intent before stopping any runtime.
/// Retained inputs must match the candidate and keep their accepted sources;
/// replacements have the new source receipt. The callback proves the complete
/// old runtime set, not merely the subset that will survive the transition.
pub(crate) fn replace_reload_batch(
    published: &PublishedConfig,
    previous: &Batch,
    expected: &[String],
    retained: &BTreeMap<String, String>,
    replacements: &BTreeMap<String, Guard>,
    mut attest_previous_runtime: impl FnMut(&BTreeMap<String, String>) -> Result<()>,
) -> Result<Batch> {
    previous.attest_settled()?;
    let expected_set: BTreeSet<_> = expected.iter().collect();
    if expected.len() > 64
        || expected_set.len() != expected.len()
        || expected.len() != retained.len() + replacements.len()
        || expected.iter().any(|name| !safe_name(name))
        || retained.iter().any(|(name, id)| {
            !expected_set.contains(name)
                || previous.generations.get(name) != Some(id)
                || replacements.contains_key(name)
        })
        || replacements.keys().any(|name| !expected_set.contains(name))
    {
        return Err("controller-batch-membership-mismatch".into());
    }
    let sources = published.source_receipt()?;
    for (name, input) in replacements {
        if input.instance != *name
            || input.root != previous.root
            || input.config_root != previous.config_root
            || input.sources.encode() != sources.encode()
        {
            return Err("controller-batch-input-mismatch".into());
        }
    }
    let prove = || -> Result<()> {
        previous.attest_settled()?;
        published.attest()?;
        sources.attest(&previous.config_root)?;
        let old = previous.load_inputs()?;
        for (name, input) in &old {
            input.guard.attest_applied()?;
            if retained.contains_key(name) && !input.matches_published(published)? {
                return Err("controller-batch-retained-input-changed".into());
            }
        }
        for input in replacements.values() {
            input.attest()?;
        }
        Ok(())
    };
    prove()?;
    attest_previous_runtime(previous.generations())?;
    let mut generations = retained.clone();
    for (name, input) in replacements {
        generations.insert(name.clone(), input.id.clone());
    }
    let mut value = json!({"schema":1,"purpose":"reload","sources":sources.encode(),
        "generations":generations,"previous":previous.id,
        "retained":retained.keys().collect::<Vec<_>>()});
    value["id"] = json!(batch_id(&value)?);
    let mut bytes = BATCH_MAGIC.to_vec();
    bytes.extend(serde_json::to_vec(&value).map_err(|_| "controller-batch-encode-failed")?);
    if bytes.len() as u64 > BATCH_MAX {
        return Err("controller-batch-too-large".into());
    }
    let (directory, _lock) = lock_store(&previous.root)?;
    prove()?;
    if [BATCH_PENDING, BATCH_TEMP, BATCH_RESTORE, BATCH_RESTORE_TEMP]
        .iter()
        .any(|name| exists(&previous.root.join(name)).unwrap_or(true))
    {
        return Err("controller-batch-recovery-required".into());
    }
    let (count, used) = inventory(&directory)?;
    if count == MAX_ENTRIES || used + bytes.len() as u64 > MAX_STORE {
        return Err("controller-input-store-capacity".into());
    }
    attest_previous_runtime(previous.generations())?;
    prove()?;
    let mut file = io(OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(previous.root.join(BATCH_TEMP)))?;
    io(file.write_all(&bytes))?;
    io(file.sync_all())?;
    attest_previous_runtime(previous.generations())?;
    prove()?;
    super::uci_transaction::rename(&directory, BATCH_TEMP, &directory, BATCH_PENDING, false)?;
    let batch =
        load_batch(&previous.root, &previous.config_root)?.ok_or("controller-batch-missing")?;
    batch.attest()?;
    if batch.bytes != bytes || !batch.is_reload_update() {
        return Err("controller-batch-reload-publication-changed".into());
    }
    attest_previous_runtime(previous.generations())?;
    batch.attest()?;
    Ok(batch)
}

/// Selected lifecycle calls this only after its old selected process/procd
/// definition is absent and its new selected SQM runtime is attested. Retained
/// peers must still be the exact accepted generations, proved by the callback.
/// This stages intent only; acceptance still happens after procd registration.
pub(crate) fn replace_selected_batch(
    published: &PublishedConfig,
    previous: &Batch,
    instance: &str,
    replacement: &Guard,
    attest_transition: impl FnMut(&BTreeMap<String, String>) -> Result<()>,
) -> Result<Batch> {
    change_selected_batch(
        published,
        previous,
        instance,
        Some(replacement),
        attest_transition,
    )
}

pub(crate) fn remove_selected_batch(
    published: &PublishedConfig,
    previous: &Batch,
    instance: &str,
    attest_transition: impl FnMut(&BTreeMap<String, String>) -> Result<()>,
) -> Result<Batch> {
    if !previous.generations.contains_key(instance) {
        return Err("controller-batch-selected-missing".into());
    }
    change_selected_batch(published, previous, instance, None, attest_transition)
}

pub(crate) fn require_update_capacity(root: &Path) -> Result<()> {
    let (directory, _lock) = lock_store(root)?;
    let (count, bytes) = inventory(&directory)?;
    if count + 2 > MAX_ENTRIES || bytes + BATCH_MAX + 512 > MAX_STORE {
        return Err("controller-input-store-capacity".into());
    }
    Ok(())
}

/// Parent-authorized roll-forward of the SAME raw candidate under new public
/// file identities. Selected old process/definition must be absent; retained
/// peers and the applied predecessor stay unchanged. Never an ordinary rebase.
pub(crate) fn supersede_pending_batch(
    published: &PublishedConfig,
    pending: &Batch,
    instance: &str,
    replacement: Option<&Guard>,
    mut attest_transition: impl FnMut(&BTreeMap<String, String>) -> Result<()>,
) -> Result<Batch> {
    if !pending.updates_only(instance)
        || restore_pending(&pending.root)?
        || pending.generations.contains_key(instance) != replacement.is_some()
    {
        return Err("controller-batch-supersession-invalid".into());
    }
    let sources = published.source_receipt()?;
    if replacement.is_some_and(|input| {
        input.instance != instance
            || input.root != pending.root
            || input.config_root != pending.config_root
            || input.sources.encode() != sources.encode()
    }) {
        return Err("controller-batch-input-mismatch".into());
    }
    let mut generations = pending.generations.clone();
    if let Some(input) = replacement {
        generations.insert(instance.into(), input.id.clone());
    }
    let retained = pending
        .retained
        .iter()
        .map(|name| (name.clone(), pending.generations[name].clone()))
        .collect::<BTreeMap<_, _>>();
    let prove = || -> Result<()> {
        pending.attest_reinstalled_content()?;
        published.attest()?;
        sources.attest(&pending.config_root)?;
        if let Some(input) = replacement {
            input.attest()?;
        }
        pending.load_inputs()?;
        Ok(())
    };
    prove()?;
    attest_transition(&retained)?;
    let (directory, _lock) = lock_store(&pending.root)?;
    prove()?;
    pending.cleanup_supersession_temp(&directory, true)?;
    let mut value = json!({"schema":1,"sources":sources.encode(),"generations":generations,
        "previous":pending.previous,"selected":instance,"retained":pending.retained,
        "supersedes":pending.id});
    value["id"] = json!(batch_id(&value)?);
    let encoded = serde_json::to_vec(&value).map_err(|_| "controller-batch-encode-failed")?;
    let mut bytes = Vec::with_capacity(BATCH_MAGIC.len() + encoded.len());
    bytes.extend_from_slice(BATCH_MAGIC);
    bytes.extend_from_slice(&encoded);
    if bytes.len() as u64 > BATCH_MAX {
        return Err("controller-batch-too-large".into());
    }
    let (count, used) = inventory(&directory)?;
    if count == MAX_ENTRIES || used + bytes.len() as u64 > MAX_STORE {
        return Err("controller-input-store-capacity".into());
    }
    let mut file = io(OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(pending.root.join(BATCH_TEMP)))?;
    io(file.write_all(&bytes))?;
    io(file.sync_all())?;
    attest_transition(&retained)?;
    prove()?;
    super::uci_transaction::rename(&directory, BATCH_TEMP, &directory, BATCH_PENDING, true)?;
    let current = read_batch_file(&pending.root, &pending.config_root, BATCH_PENDING)?;
    let (displaced, _, _) = read_file(&pending.root.join(BATCH_TEMP), BATCH_MAX, true)?;
    if current.bytes != bytes {
        return Err("controller-batch-supersession-displaced-mismatch".into());
    }
    if displaced != pending.bytes {
        // Return the actually displaced foreign record to its original name.
        // Do not mistake it for the planned predecessor or unlink its bytes.
        current.attest_record()?;
        super::uci_transaction::rename(&directory, BATCH_PENDING, &directory, BATCH_TEMP, true)?;
        return Err("controller-batch-supersession-displaced-mismatch".into());
    }
    current.attest()?;
    attest_transition(&retained)?;
    current.attest()?;
    current.cleanup_supersession_temp(&directory, false)?;
    current.attest()?;
    Ok(current)
}

fn change_selected_batch(
    published: &PublishedConfig,
    previous: &Batch,
    instance: &str,
    replacement: Option<&Guard>,
    mut attest_transition: impl FnMut(&BTreeMap<String, String>) -> Result<()>,
) -> Result<Batch> {
    if previous.pending() || !safe_name(instance) {
        return Err("controller-batch-update-authority-invalid".into());
    }
    let sources = published.source_receipt()?;
    if replacement.is_some_and(|replacement| {
        replacement.instance != instance
            || replacement.root != previous.root
            || replacement.config_root != previous.config_root
            || replacement.sources.encode() != sources.encode()
    }) {
        return Err("controller-batch-input-mismatch".into());
    }
    let retained = previous
        .generations
        .iter()
        .filter(|(name, _)| name.as_str() != instance)
        .map(|(name, id)| (name.clone(), id.clone()))
        .collect::<BTreeMap<_, _>>();
    let mut generations = retained.clone();
    if let Some(replacement) = replacement {
        generations.insert(instance.into(), replacement.id.clone());
    }
    if generations.len() > 64 {
        return Err("controller-batch-membership-mismatch".into());
    }
    let prove = || -> Result<()> {
        previous.attest_record()?;
        sources.attest(&previous.config_root)?;
        published.attest()?;
        if let Some(replacement) = replacement {
            replacement.attest()?;
        }
        let directory = Directory::open(&previous.root, true, false)?;
        let inputs = previous.load_inputs()?;
        for name in retained.keys() {
            if !inputs[name].guard.accepted(&directory)? {
                return Err("controller-batch-retained-input-unaccepted".into());
            }
        }
        Ok(())
    };
    prove()?;
    attest_transition(&retained)?;
    let (directory, _lock) = lock_store(&previous.root)?;
    prove()?;
    if [BATCH_TEMP, BATCH_RESTORE, BATCH_RESTORE_TEMP]
        .iter()
        .any(|name| exists(&previous.root.join(name)).unwrap_or(true))
    {
        return Err("controller-batch-recovery-required".into());
    }
    previous.cleanup_predecessor(&directory)?;
    if exists(&previous.root.join(BATCH_PENDING))? {
        return Err("controller-batch-recovery-required".into());
    }
    let mut value = json!({"schema":1,"sources":sources.encode(),"generations":generations,
        "previous":previous.id,"retained":retained.keys().collect::<Vec<_>>(),"selected":instance});
    value["id"] = json!(batch_id(&value)?);
    let mut bytes = BATCH_MAGIC.to_vec();
    bytes.extend(serde_json::to_vec(&value).map_err(|_| "controller-batch-encode-failed")?);
    if bytes.len() as u64 > BATCH_MAX {
        return Err("controller-batch-too-large".into());
    }
    let (count, used) = inventory(&directory)?;
    if count == MAX_ENTRIES || used + bytes.len() as u64 > MAX_STORE {
        return Err("controller-input-store-capacity".into());
    }
    attest_transition(&retained)?;
    prove()?;
    let mut file = io(OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(previous.root.join(BATCH_TEMP)))?;
    io(file.write_all(&bytes))?;
    io(file.sync_all())?;
    super::uci_transaction::rename(&directory, BATCH_TEMP, &directory, BATCH_PENDING, false)?;
    let batch =
        load_batch(&previous.root, &previous.config_root)?.ok_or("controller-batch-missing")?;
    batch.attest()?;
    Ok(batch)
}

/// Preparation failed before any desired reload set was published. The caller
/// must hold lifecycle authority, exclude Native Apply owners and prove ALL old
/// runtime consumers unchanged. This handle never authorizes runtime actions.
pub(crate) struct UnpublishedReload {
    pub(crate) previous: Batch,
    pub(crate) expected_source: Option<SourceReceipt>,
    draft: Option<(Vec<u8>, Identity)>,
}

impl UnpublishedReload {
    pub(crate) fn load(root: &Path, config_root: &Path) -> Result<Option<Self>> {
        if !root.is_absolute() || !config_root.is_absolute() {
            return Err("controller-input-root-invalid".into());
        }
        if !exists(root)? {
            return Ok(None);
        }
        let directory = Directory::open(root, true, false)?;
        inventory(&directory)?;
        if exists(&root.join(BATCH_PENDING))? || restore_pending(root)? {
            return Err("controller-batch-reload-intent-already-published".into());
        }
        if !exists(&root.join(BATCH_APPLIED))? {
            return Ok(None);
        }
        let previous = read_batch_file(root, config_root, BATCH_APPLIED)?;
        let mut expected_source = None;
        let draft = if exists(&root.join(BATCH_TEMP))? {
            let (bytes, _, identity) = read_file(&root.join(BATCH_TEMP), BATCH_MAX, true)?;
            if !BATCH_MAGIC.starts_with(&bytes) && !bytes.starts_with(BATCH_MAGIC) {
                return Err("controller-batch-unowned-temporary".into());
            }
            if let Some(payload) = bytes.strip_prefix(BATCH_MAGIC) {
                match serde_json::from_slice::<Value>(payload) {
                    Ok(_) => {
                        let complete = read_batch_file(root, config_root, BATCH_TEMP)?;
                        if !complete.reload
                            || complete.previous.as_ref() != Some(&previous.id)
                            || complete.retained.iter().any(|name| {
                                complete.generations.get(name) != previous.generations.get(name)
                            })
                        {
                            return Err("controller-batch-reload-draft-owner-mismatch".into());
                        }
                        complete.load_inputs()?;
                        expected_source = Some(complete.sources);
                    }
                    Err(error) if error.is_eof() => {}
                    Err(_) => return Err("controller-batch-reload-draft-invalid".into()),
                }
            }
            Some((bytes, identity))
        } else {
            None
        };
        let recovery = Self {
            previous,
            expected_source,
            draft,
        };
        recovery.attest()?;
        directory.attest()?;
        Ok(Some(recovery))
    }

    pub(crate) fn attest(&self) -> Result<()> {
        self.previous.attest_settled()?;
        for input in self.previous.load_inputs()?.values() {
            input.guard.attest_applied()?;
        }
        let path = self.previous.root.join(BATCH_TEMP);
        match &self.draft {
            Some((bytes, identity)) => {
                let (current, _, actual) = read_file(&path, BATCH_MAX, true)?;
                if &current != bytes || &actual != identity {
                    return Err("controller-batch-reload-draft-changed".into());
                }
            }
            None if exists(&path)? => return Err("controller-batch-reload-draft-changed".into()),
            None => {}
        }
        self.previous.attest_record()
    }

    pub(crate) fn retire_draft(
        &mut self,
        mut attest_original_runtime: impl FnMut(&BTreeMap<String, String>) -> Result<()>,
    ) -> Result<()> {
        self.attest()?;
        attest_original_runtime(self.previous.generations())?;
        let (directory, _lock) = lock_store(&self.previous.root)?;
        self.attest()?;
        attest_original_runtime(self.previous.generations())?;
        self.attest()?;
        if self.draft.is_some() {
            io(fs::remove_file(self.previous.root.join(BATCH_TEMP)))?;
            self.draft = None;
            io(directory.file.sync_all())?;
        }
        self.attest()?;
        attest_original_runtime(self.previous.generations())
    }
}

/// Inspect without accepting a draft as a desired set. Native selected-update
/// restoration metadata can never enter ordinary orphan recovery.
/// None means a published registry; Some records whether an unpublished draft
/// exists. Complete input records alone are not registration authority.
pub(crate) fn unregistered_store(root: &Path) -> Result<Option<bool>> {
    if !root.is_absolute() {
        return Err("controller-input-root-invalid".into());
    }
    if !exists(root)? {
        return Ok(Some(false));
    }
    let directory = Directory::open(root, true, false)?;
    inventory(&directory)?;
    let unregistered = !exists(&root.join(BATCH_PENDING))? && !exists(&root.join(BATCH_APPLIED))?;
    directory.attest()?;
    if unregistered {
        if exists(&root.join(BATCH_RESTORE))? || exists(&root.join(BATCH_RESTORE_TEMP))? {
            return Err("controller-batch-update-recovery-required".into());
        }
        let draft = exists(&root.join(BATCH_TEMP))?;
        if draft {
            let (bytes, _, _) = read_file(&root.join(BATCH_TEMP), BATCH_MAX, true)?;
            if !BATCH_MAGIC.starts_with(&bytes) && !bytes.starts_with(BATCH_MAGIC) {
                return Err("controller-batch-unowned-temporary".into());
            }
        }
        Ok(Some(draft))
    } else {
        Ok(None)
    }
}

/// Only before capturing a new startup baseline, after fencing all runtime
/// consumers. No pending or applied registry is discarded through this lane.
pub(crate) fn discard_unpublished_batch(
    root: &Path,
    mut attest_absent: impl FnMut() -> Result<()>,
) -> Result<()> {
    if !root.is_absolute() {
        return Err("controller-input-root-invalid".into());
    }
    if !exists(root)? || !exists(&root.join(BATCH_TEMP))? {
        return Ok(());
    }
    let (directory, _lock) = lock_store(root)?;
    if exists(&root.join(BATCH_PENDING))? || exists(&root.join(BATCH_APPLIED))? {
        return Err("controller-batch-recovery-required".into());
    }
    let path = root.join(BATCH_TEMP);
    let (bytes, _, identity) = read_file(&path, BATCH_MAX, true)?;
    if !BATCH_MAGIC.starts_with(&bytes) && !bytes.starts_with(BATCH_MAGIC) {
        return Err("controller-batch-unowned-temporary".into());
    }
    attest_absent()?;
    directory.attest()?;
    let (current, _, actual) = read_file(&path, BATCH_MAX, true)?;
    if current != bytes || actual != identity {
        return Err("controller-batch-record-changed".into());
    }
    attest_absent()?;
    let (current, _, actual) = read_file(&path, BATCH_MAX, true)?;
    if current != bytes || actual != identity {
        return Err("controller-batch-record-changed".into());
    }
    io(fs::remove_file(path))?;
    io(directory.file.sync_all())
}
impl Loaded {
    pub(crate) fn matches_published(&self, published: &PublishedConfig) -> Result<bool> {
        let matches = self.matches_prepared(published.config())?;
        published.attest()?;
        Ok(matches)
    }
    /// Compare applied inputs without publishing the candidate or changing its
    /// public file identities. The caller retains source/plan authority.
    pub(crate) fn matches_prepared(&self, config: &PreparedConfig) -> Result<bool> {
        config.attest_private()?;
        self.guard.attest_record()?;
        let directory = Directory::open(&self.guard.root, true, false)?;
        if !self.guard.accepted(&directory)? {
            return Ok(false);
        }
        let show = config.section_show("cake-autorate", self.guard.instance())?;
        let whole = config.package_show("cake-autorate")?;
        let history = crate::parse_global_history_config(
            std::str::from_utf8(&whole).map_err(|_| "controller-input-not-text")?,
        )?;
        let matches = self.cake_show == show
            && self.sqm_show == prepared_sqm_show(config, self.guard.instance())?
            && self.config.graph_history_ram_budget_kib == history.0
            && self.config.graph_history_instance_count == history.1;
        config.attest_private()?;
        self.guard.attest_record()?;
        Ok(matches)
    }
    pub(crate) fn sqm_section(&self, section: &str) -> Result<Vec<u8>> {
        if !safe_name(section) {
            return Err("controller-input-sqm-invalid".into());
        }
        let text =
            std::str::from_utf8(&self.sqm_show).map_err(|_| "controller-input-sqm-invalid")?;
        let key = format!("sqm.{section}");
        let prefix = format!("{key}.");
        let mut output = Vec::new();
        for line in text.lines() {
            let (path, _) = line.split_once('=').ok_or("controller-input-sqm-invalid")?;
            if path == key || path.starts_with(&prefix) {
                output.extend(line.as_bytes());
                output.push(b'\n');
            }
        }
        if output.is_empty() {
            return Err("controller-input-sqm-section-missing".into());
        }
        Ok(output)
    }
}

pub(crate) fn store(
    published: &PublishedConfig,
    instance: &str,
    root: &Path,
    config_root: &Path,
) -> Result<Guard> {
    store_with_reserve(published, instance, root, config_root, 0, 0)
}

/// Reserve room for pending/acceptance/restoration metadata before a selected
/// lifecycle stops anything. The parent's exclusive operation lock serializes
/// cooperating writers until registration consumes this preflight capacity.
pub(crate) fn store_for_selected_update(
    published: &PublishedConfig,
    instance: &str,
    root: &Path,
    config_root: &Path,
) -> Result<Guard> {
    store_with_reserve(published, instance, root, config_root, 3, BATCH_MAX + 512)
}

fn published_sqm_show(published: &PublishedConfig, instance: &str) -> Result<Vec<u8>> {
    published.attest()?;
    prepared_sqm_show(published.config(), instance)
}

pub(crate) fn prepared_sqm_show(config: &PreparedConfig, instance: &str) -> Result<Vec<u8>> {
    let mut sqm_show = Vec::new();
    for (name, section) in &config.package("sqm")?.sections {
        if section
            .options
            .get("_cake_autorate_managed")
            .map(String::as_str)
            == Some(instance)
        {
            sqm_show.extend(config.section_show("sqm", name)?);
        }
    }
    Ok(sqm_show)
}

fn store_with_reserve(
    published: &PublishedConfig,
    instance: &str,
    root: &Path,
    config_root: &Path,
    reserve_entries: usize,
    reserve_bytes: u64,
) -> Result<Guard> {
    if !root.is_absolute() || !config_root.is_absolute() {
        return Err("controller-input-root-invalid".into());
    }
    published.attest()?;
    if !safe_name(instance) {
        return Err("controller-input-identity-invalid".into());
    }
    let data = published.config().section_show("cake-autorate", instance)?;
    let whole = published.config().package_show("cake-autorate")?;
    let history = crate::parse_global_history_config(
        std::str::from_utf8(&whole).map_err(|_| "controller-input-not-text")?,
    )?;
    let sources = published.source_receipt()?;
    sources.attest(config_root)?;
    let sqm_show = published_sqm_show(published, instance)?;
    // A private random nonce blinds the public content-bound generation ID.
    let nonce = fresh_generation()?;
    let mut value = json!({"schema":1,"instance":instance,"nonce":nonce,"sources":sources.encode(),
        "sqm_show":std::str::from_utf8(&sqm_show).map_err(|_| "controller-input-not-text")?,
        "show":std::str::from_utf8(&data).map_err(|_| "controller-input-not-text")?,
        "history_budget_kib":history.0,"history_instances":history.1});
    let id = record_id(&value)?;
    value["generation"] = json!(id);
    let mut bytes = MAGIC.to_vec();
    bytes.extend(serde_json::to_vec(&value).map_err(|_| "controller-input-encode-failed")?);
    if bytes.len() as u64 > MAX_RECORD {
        return Err("controller-input-record-too-large".into());
    }
    parse_config(&value, instance, &id)?;
    let (directory, _lock) = lock_store(root)?;
    let (count, used) = inventory(&directory)?;
    if count + 1 + reserve_entries > MAX_ENTRIES
        || used + bytes.len() as u64 + reserve_bytes > MAX_STORE
    {
        return Err("controller-input-store-capacity".into());
    }
    published.attest()?;
    let name = record_name(instance, &id)?;
    let mut file = io(OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.join(&name)))?;
    io(file.write_all(&bytes))?;
    io(file.sync_all())?;
    io(directory.file.sync_all())?;
    let (_, _, identity) = read_file(&root.join(&name), MAX_RECORD, true)?;
    let guard = Guard {
        root: root.into(),
        config_root: config_root.into(),
        instance: instance.into(),
        id,
        identity,
        sha256: digest(&bytes),
        sources,
    };
    let proof = published.attest().and_then(|_| guard.attest());
    if proof.is_err() && guard.record_attest().is_ok() {
        io(fs::remove_file(root.join(guard.name()?)))?;
        io(directory.file.sync_all())?;
    }
    proof?;
    Ok(guard)
}

fn parse_config(value: &Value, instance: &str, id: &str) -> Result<crate::Config> {
    if value["schema"].as_u64() != Some(1)
        || value["instance"].as_str() != Some(instance)
        || value["generation"].as_str() != Some(id)
        || record_id(value)? != id
    {
        return Err("controller-input-identity-mismatch".into());
    }
    let show = value["show"]
        .as_str()
        .ok_or("controller-input-show-invalid")?;
    let package =
        UciPackage::parse("cake-autorate", show).map_err(|_| "controller-input-show-invalid")?;
    if package.sections.len() != 1
        || package
            .sections
            .get(instance)
            .is_none_or(|s| s.section_type != "cake_autorate")
    {
        return Err("controller-input-section-mismatch".into());
    }
    let budget = if value["history_budget_kib"].is_null() {
        None
    } else {
        Some(
            value["history_budget_kib"]
                .as_u64()
                .filter(|n| {
                    (crate::GRAPH_HISTORY_MIN_BUDGET_KIB..=crate::GRAPH_HISTORY_HARD_MAX_KIB)
                        .contains(n)
                })
                .ok_or("controller-input-history-invalid")?,
        )
    };
    let count = value["history_instances"]
        .as_u64()
        .filter(|n| (1..=64).contains(n))
        .ok_or("controller-input-history-invalid")? as usize;
    let mut config = crate::Config::from_uci_text(instance, show)
        .map_err(|_| "controller-input-config-invalid")?;
    config.graph_history_ram_budget_kib = budget;
    config.graph_history_instance_count = count;
    config
        .validate()
        .map_err(|_| "controller-input-config-invalid")?;
    let sqm_text = match value.get("sqm_show") {
        None => "",
        Some(value) => value.as_str().ok_or("controller-input-sqm-invalid")?,
    };
    let recipes = UciPackage::parse("sqm", sqm_text).map_err(|_| "controller-input-sqm-invalid")?;
    if recipes.sections.values().any(|section| {
        section.section_type != "queue"
            || section
                .options
                .get("_cake_autorate_managed")
                .map(String::as_str)
                != Some(instance)
    }) {
        return Err("controller-input-sqm-owner-mismatch".into());
    }
    if config.enabled && config.manage_sqm && config.sqm_enabled && recipes.sections.is_empty() {
        return Err("controller-input-sqm-recipe-missing".into());
    }
    Ok(config)
}

pub(crate) fn load(instance: &str, id: &str, root: &Path, config_root: &Path) -> Result<Loaded> {
    let loaded = load_for_recovery(instance, id, root, config_root)?;
    loaded.guard.attest()?;
    Ok(loaded)
}
/// Read the old sealed generation for an owner-attested Stop/recovery. It must
/// not be used as a fresh controller admission in place of load().
pub(crate) fn load_for_recovery(
    instance: &str,
    id: &str,
    root: &Path,
    config_root: &Path,
) -> Result<Loaded> {
    if !root.is_absolute() || !config_root.is_absolute() {
        return Err("controller-input-root-invalid".into());
    }
    let name = record_name(instance, id)?;
    let directory = Directory::open(root, true, false)?;
    let (bytes, _, identity) = read_file(&root.join(name), MAX_RECORD, true)?;
    let body = bytes
        .strip_prefix(MAGIC)
        .ok_or("controller-input-record-invalid")?;
    let value: Value =
        serde_json::from_slice(body).map_err(|_| "controller-input-record-invalid")?;
    let config = parse_config(&value, instance, id)?;
    let sources = SourceReceipt::decode(&value["sources"])?;
    let guard = Guard {
        root: root.into(),
        config_root: config_root.into(),
        instance: instance.into(),
        id: id.into(),
        identity,
        sha256: digest(&bytes),
        sources,
    };
    guard.attest_record()?;
    directory.attest()?;
    let cake_show = value["show"]
        .as_str()
        .ok_or("controller-input-show-invalid")?
        .as_bytes()
        .to_vec();
    let sqm_show = value
        .get("sqm_show")
        .and_then(Value::as_str)
        .unwrap_or("")
        .as_bytes()
        .to_vec();
    Ok(Loaded {
        config,
        guard,
        cake_show,
        sqm_show,
    })
}

pub(crate) fn load_production(instance: &str, id: &str) -> Result<Loaded> {
    let runtime = std::env::var_os("CAKE_AUTORATE_RUNTIME_ROOT")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| "/var/run/cake-autorate".into());
    let config = std::env::var_os("CAKE_AUTORATE_CONFIG_DIR")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| "/etc/config".into());
    load(instance, id, &runtime.join(".controller-input"), &config)
}

#[cfg(test)]
mod tests {
    use super::super::committed_uci::CommittedSnapshot;
    use super::super::uci_transaction::publish;
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::fs::{symlink, DirBuilderExt, PermissionsExt};

    struct Fixture {
        root: PathBuf,
        config: PathBuf,
        inputs: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("cake-r4-input-{}", fresh_generation().unwrap()));
            fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            let config = root.join("config");
            fs::create_dir(&config).unwrap();
            fs::write(config.join("cake-autorate"), "config cake_autorate 'lab'\n option enabled '0'\n option manage_sqm '0'\n option sqm_enabled '0'\nconfig globals 'globals'\n option graph_history_ram_budget_kib '512'\n").unwrap();
            fs::write(config.join("sqm"), b"# no owned queue\n").unwrap();
            Self {
                inputs: root.join(".controller-input"),
                root,
                config,
            }
        }
        fn published(&self) -> PublishedConfig {
            let snapshot = CommittedSnapshot::capture_fixture(
                &self.config,
                &self.root.join("snapshot"),
                query,
            )
            .unwrap();
            publish(snapshot.prepare_fixture([&[], &[]], query).unwrap()).unwrap()
        }
        fn input(&self, published: &PublishedConfig) -> Guard {
            store(published, "lab", &self.inputs, &self.config).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    fn query(args: Vec<OsString>) -> Result<Vec<u8>> {
        let name = args[9].to_str().unwrap();
        let bytes = fs::read(Path::new(&args[1]).join(name)).unwrap();
        if bytes
            .windows(b"cake_autorate".len())
            .any(|b| b == b"cake_autorate")
        {
            let mut output = format!("{name}.lab=cake_autorate\n{name}.lab.enabled='0'\n{name}.lab.manage_sqm='0'\n{name}.lab.sqm_enabled='0'\n");
            output.push_str(&format!(
                "{name}.globals=globals\n{name}.globals.graph_history_ram_budget_kib='512'\n"
            ));
            if bytes.windows(b"'peer'".len()).any(|v| v == b"'peer'") {
                output.push_str(&format!("{name}.peer=cake_autorate\n{name}.peer.enabled='0'\n{name}.peer.manage_sqm='0'\n{name}.peer.sqm_enabled='0'\n"));
            }
            Ok(output.into_bytes())
        } else {
            Ok(vec![])
        }
    }

    #[test]
    #[cfg(feature = "calibration")]
    fn r4_applied_input_attestation_requires_acceptance_without_rebasing_public_source() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let input = fixture.input(&published);
        input.attest().unwrap();
        assert_eq!(
            input.attest_applied().unwrap_err(),
            "controller-input-not-applied"
        );
        input.mark_accepted(|_, _| Ok(())).unwrap();
        input.attest_applied().unwrap();
        fs::write(
            fixture.config.join("cake-autorate"),
            b"later unapplied configuration\n",
        )
        .unwrap();
        input.attest_applied().unwrap();
        let marker = fixture
            .inputs
            .join(format!("{}.accepted", input.name().unwrap()));
        fs::write(marker, b"damaged acceptance").unwrap();
        assert!(input.attest_applied().is_err());
    }

    #[test]
    fn r4_input_gc_preserves_a_record_changed_during_final_reference_proof() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let input = fixture.input(&published);
        let path = fixture.inputs.join(input.name().unwrap());
        let mut calls = 0;
        assert!(collect_unreferenced(&fixture.inputs, &fixture.config, |_| {
            calls += 1;
            if calls == 3 {
                fs::write(&path, b"changed during proof").unwrap();
            }
            Ok(true)
        })
        .is_err());
        assert_eq!(calls, 3);
        assert_eq!(fs::read(&path).unwrap(), b"changed during proof");
    }

    #[test]
    fn r4_input_gc_keeps_repeated_accepted_updates_bounded() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let input = fixture.input(&published);
        let initial = store_batch(
            &published,
            &["lab".into()],
            &BTreeMap::from([("lab".into(), input)]),
            &fixture.inputs,
            &fixture.config,
        )
        .unwrap();
        drop(published);
        initial.accept(|_| Ok(())).unwrap();
        for revision in 0..32 {
            let previous = load_batch(&fixture.inputs, &fixture.config)
                .unwrap()
                .unwrap();
            fs::write(
                fixture.config.join("sqm"),
                format!("# revision {revision}\n"),
            )
            .unwrap();
            let published = fixture.published();
            let input = fixture.input(&published);
            let update =
                replace_selected_batch(&published, &previous, "lab", &input, |_| Ok(())).unwrap();
            drop(published);
            update.accept(|_| Ok(())).unwrap();
            assert_eq!(
                collect_unreferenced(&fixture.inputs, &fixture.config, |_| Ok(true)).unwrap(),
                1
            );
            assert_eq!(fs::read_dir(&fixture.inputs).unwrap().count(), 4);
            input.attest().unwrap();
        }
    }

    #[test]
    fn r4_input_gc_rejects_a_new_batch_root_before_deletion() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let input = fixture.input(&published);
        let mut created = false;
        assert!(collect_unreferenced(&fixture.inputs, &fixture.config, |_| {
            if !created {
                store_batch(
                    &published,
                    &["lab".into()],
                    &BTreeMap::from([("lab".into(), input.clone())]),
                    &fixture.inputs,
                    &fixture.config,
                )?;
                created = true;
            }
            Ok(true)
        })
        .is_err());
        input.attest_record().unwrap();
        assert!(load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap()
            .generations()
            .contains_key("lab"));
    }

    #[test]
    fn r4_input_gc_preserves_batch_and_runtime_roots_and_removes_only_orphans() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let active = fixture.input(&published);
        let orphan = fixture.input(&published);
        let runtime_only = fixture.input(&published);
        let batch = store_batch(
            &published,
            &["lab".into()],
            &BTreeMap::from([("lab".into(), active.clone())]),
            &fixture.inputs,
            &fixture.config,
        )
        .unwrap();
        drop(published);
        batch.accept(|_| Ok(())).unwrap();
        orphan.mark_accepted(|_, _| Ok(())).unwrap();
        let partial = fixture.inputs.join(format!("partial.{}", "c".repeat(64)));
        fs::write(&partial, MAGIC).unwrap();
        fs::set_permissions(&partial, fs::Permissions::from_mode(0o600)).unwrap();
        let removed = collect_unreferenced(&fixture.inputs, &fixture.config, |input| {
            assert_ne!(input.guard.id, active.id);
            Ok(input.guard.id != runtime_only.id)
        })
        .unwrap();
        assert_eq!(removed, 1);
        assert!(!fixture.inputs.join(orphan.name().unwrap()).exists());
        assert!(!fixture
            .inputs
            .join(format!("{}.accepted", orphan.name().unwrap()))
            .exists());
        active.attest_record().unwrap();
        runtime_only.attest_record().unwrap();
        assert_eq!(fs::read(&partial).unwrap(), MAGIC);
        assert_eq!(
            collect_unreferenced(&fixture.inputs, &fixture.config, |_| Ok(true)).unwrap(),
            1
        );
        active.attest_record().unwrap();
    }

    #[test]
    fn r4_input_gc_rechecks_new_runtime_references_before_deletion() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let orphan = fixture.input(&published);
        let mut checks = 0;
        assert!(collect_unreferenced(&fixture.inputs, &fixture.config, |_| {
            checks += 1;
            Ok(checks == 1)
        })
        .is_err());
        orphan.attest_record().unwrap();
        assert_eq!(checks, 2);
    }

    #[test]
    fn r4_controller_batch_supersession_requires_same_content_and_survives_exchange_gap() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let old_input = fixture.input(&published);
        let initial = store_batch(
            &published,
            &["lab".into()],
            &BTreeMap::from([("lab".into(), old_input)]),
            &fixture.inputs,
            &fixture.config,
        )
        .unwrap();
        drop(published);
        initial.accept(|_| Ok(())).unwrap();
        let previous = load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap();
        fs::write(fixture.config.join("sqm"), b"# candidate bytes\n").unwrap();
        let published = fixture.published();
        let first_input = fixture.input(&published);
        let pending =
            replace_selected_batch(&published, &previous, "lab", &first_input, |_| Ok(())).unwrap();
        drop(published);
        let source = fixture.config.join("sqm");
        let bytes = fs::read(&source).unwrap();
        let mode = fs::metadata(&source).unwrap().permissions();
        let reinstall = fixture.root.join("reinstalled");
        fs::write(&reinstall, &bytes).unwrap();
        fs::set_permissions(&reinstall, mode).unwrap();
        fs::rename(reinstall, &source).unwrap();
        assert!(pending.attest().is_err());
        pending.attest_reinstalled_content().unwrap();
        let published = fixture.published();
        let second_input = fixture.input(&published);
        assert!(
            supersede_pending_batch(&published, &pending, "lab", Some(&second_input), |_| {
                if exists(&fixture.inputs.join(BATCH_TEMP))? {
                    Err("interrupted-before-exchange".into())
                } else {
                    Ok(())
                }
            })
            .is_err()
        );
        assert!(
            load_batch(&fixture.inputs, &fixture.config).is_err(),
            "draft is not admission"
        );
        assert_eq!(
            load_batch_for_selected_preparation(&fixture.inputs, &fixture.config)
                .unwrap()
                .unwrap()
                .id,
            pending.id
        );
        let error =
            supersede_pending_batch(&published, &pending, "lab", Some(&second_input), |_| {
                if read_batch_file(&fixture.inputs, &fixture.config, BATCH_PENDING)?.id
                    != pending.id
                {
                    Err("lost-ack-after-exchange".into())
                } else {
                    Ok(())
                }
            })
            .err()
            .unwrap();
        assert_eq!(error, "lost-ack-after-exchange");
        let current = load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap();
        assert_eq!(current.generations().get("lab"), Some(&second_input.id));
        assert!(fixture.inputs.join(BATCH_TEMP).exists());
        assert!(current
            .cleanup_superseded(|| Err("presence-unproven".into()))
            .is_err());
        current.cleanup_superseded(|| Ok(())).unwrap();
        assert!(!fixture.inputs.join(BATCH_TEMP).exists());
        previous.attest_record().unwrap();
        drop(published);
        current.accept(|_| Ok(())).unwrap();
        assert!(load(
            "lab",
            first_input.generation(),
            &fixture.inputs,
            &fixture.config
        )
        .is_err());
        load(
            "lab",
            second_input.generation(),
            &fixture.inputs,
            &fixture.config,
        )
        .unwrap();
        fs::write(&source, b"# different candidate\n").unwrap();
        assert!(current.attest_reinstalled_content().is_err());
        assert_eq!(fs::read(source).unwrap(), b"# different candidate\n");
    }

    #[test]
    fn r4_controller_batch_removal_is_explicit_and_can_restore_or_accept_absence() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let input = fixture.input(&published);
        let batch = store_batch(
            &published,
            &["lab".into()],
            &BTreeMap::from([("lab".into(), input.clone())]),
            &fixture.inputs,
            &fixture.config,
        )
        .unwrap();
        drop(published);
        batch.accept(|_| Ok(())).unwrap();
        let previous = load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap();
        let published = fixture.published();
        assert!(remove_selected_batch(&published, &previous, "other", |_| Ok(())).is_err());
        assert!(remove_selected_batch(&published, &previous, "lab", |_| Err(
            "selected-still-running".into()
        ))
        .is_err());
        assert!(!fixture.inputs.join(BATCH_PENDING).exists());
        let removal = remove_selected_batch(&published, &previous, "lab", |peers| {
            assert!(peers.is_empty());
            Ok(())
        })
        .unwrap();
        assert!(removal.generations().is_empty());
        assert!(removal.updates_only("lab"));
        assert!(!removal.updates_only("other"));
        let old = removal
            .begin_restore(|expected| {
                assert_eq!(expected.get("lab"), Some(&input.id));
                Ok(())
            })
            .unwrap();
        old.accept(|_| Ok(())).unwrap();
        assert_eq!(
            load_batch(&fixture.inputs, &fixture.config)
                .unwrap()
                .unwrap()
                .generations()
                .get("lab"),
            Some(&input.id)
        );
        let removal = remove_selected_batch(&published, &previous, "lab", |_| Ok(())).unwrap();
        drop(published);
        assert!(removal
            .accept(|_| Err("residual-controller".into()))
            .is_err());
        removal
            .accept(|map| {
                assert!(map.is_empty());
                Ok(())
            })
            .unwrap();
        let applied = load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap();
        assert!(applied.generations().is_empty());
        assert!(!applied.pending());
        applied.retire(|_| Ok(())).unwrap();
    }

    #[test]
    fn r4_selected_input_reserves_registry_and_recovery_entries_before_writing() {
        for (padding, allowed) in [(MAX_ENTRIES - 5, true), (MAX_ENTRIES - 4, false)] {
            let fixture = Fixture::new();
            let published = fixture.published();
            let (_, lock) = lock_store(&fixture.inputs).unwrap();
            for i in 0..padding {
                let path = fixture
                    .inputs
                    .join(format!("padding_{i}.{}", "a".repeat(64)));
                fs::write(&path, []).unwrap();
                fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
            }
            drop(lock);
            let count = fs::read_dir(&fixture.inputs).unwrap().count();
            let result =
                store_for_selected_update(&published, "lab", &fixture.inputs, &fixture.config);
            assert_eq!(result.is_ok(), allowed);
            assert_eq!(
                fs::read_dir(&fixture.inputs).unwrap().count(),
                count + usize::from(allowed)
            );
            if let Ok(guard) = result {
                guard.attest().unwrap();
            }
        }
    }

    #[test]
    fn r4_controller_batch_restore_intent_survives_cleanup_gap_and_blocks_new_acceptance() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let guard = fixture.input(&published);
        let first = store_batch(
            &published,
            &["lab".into()],
            &BTreeMap::from([("lab".into(), guard.clone())]),
            &fixture.inputs,
            &fixture.config,
        )
        .unwrap();
        drop(published);
        first.accept(|_| Ok(())).unwrap();
        let old = load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap();
        fs::write(fixture.config.join("sqm"), b"# selected candidate\n").unwrap();
        let published = fixture.published();
        let replacement = fixture.input(&published);
        let pending =
            replace_selected_batch(&published, &old, "lab", &replacement, |_| Ok(())).unwrap();
        drop(published);
        assert!(pending
            .begin_restore(|_| Err("restoration-unproven".into()))
            .is_err());
        assert!(!fixture.inputs.join(BATCH_RESTORE).exists());
        let restored = pending
            .begin_restore(|expected| {
                assert_eq!(
                    expected,
                    &BTreeMap::from([("lab".into(), guard.id.clone())])
                );
                Ok(())
            })
            .unwrap();
        assert_eq!(restored.id, old.id);
        assert_eq!(
            load_batch(&fixture.inputs, &fixture.config)
                .unwrap()
                .unwrap()
                .id,
            old.id
        );
        assert!(pending.accept(|_| Ok(())).is_err());
        assert!(restored
            .accept(|_| Err("old-controller-not-ready".into()))
            .is_err());
        assert!(fixture.inputs.join(BATCH_PENDING).exists());
        // Model the persistent prefix after pending removal but before the
        // marker-last cleanup. The restore intent still selects the old set.
        fs::remove_file(fixture.inputs.join(BATCH_PENDING)).unwrap();
        let interrupted = load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap();
        assert_eq!(interrupted.id, old.id);
        assert!(interrupted
            .accept(|_| Err("readiness-still-required".into()))
            .is_err());
        assert!(fixture.inputs.join(BATCH_RESTORE).exists());
        interrupted.accept(|_| Ok(())).unwrap();
        assert!(!fixture.inputs.join(BATCH_RESTORE).exists());
        assert!(!fixture.inputs.join(BATCH_PENDING).exists());
        old.attest_record().unwrap();
    }

    #[test]
    fn r4_controller_batch_selected_update_refuses_source_drift_before_publishing_intent() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let guard = fixture.input(&published);
        let batch = store_batch(
            &published,
            &["lab".into()],
            &BTreeMap::from([("lab".into(), guard)]),
            &fixture.inputs,
            &fixture.config,
        )
        .unwrap();
        drop(published);
        batch.accept(|_| Ok(())).unwrap();
        let previous = load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap();
        fs::write(fixture.config.join("sqm"), b"# selected candidate\n").unwrap();
        let published = fixture.published();
        let replacement = fixture.input(&published);
        assert!(
            replace_selected_batch(&published, &previous, "lab", &replacement, |_| {
                fs::write(fixture.config.join("sqm"), b"# later user commit\n").unwrap();
                Ok(())
            })
            .is_err()
        );
        assert!(!fixture.inputs.join(BATCH_PENDING).exists());
        assert!(!fixture.inputs.join(BATCH_TEMP).exists());
        previous.attest_record().unwrap();
        assert_eq!(
            fs::read(fixture.config.join("sqm")).unwrap(),
            b"# later user commit\n"
        );
    }

    #[test]
    fn r4_controller_batch_unrelated_pending_cannot_be_adopted_or_cleaned() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let batch = store_batch(
            &published,
            &[],
            &BTreeMap::new(),
            &fixture.inputs,
            &fixture.config,
        )
        .unwrap();
        drop(published);
        batch.accept(|_| Ok(())).unwrap();
        let applied = load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap();
        let path = fixture.inputs.join(BATCH_PENDING);
        fs::write(&path, &applied.bytes).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(load_batch(&fixture.inputs, &fixture.config).is_err());
        assert!(applied.accept(|_| Ok(())).is_err());
        assert!(applied.retire(|_| Ok(())).is_err());
        assert_eq!(fs::read(&path).unwrap(), applied.bytes);
        applied.attest_record().unwrap();
    }

    #[test]
    fn r4_controller_batch_selected_update_preserves_peer_sources_and_recovers_lost_ack() {
        let fixture = Fixture::new();
        let mut cake = fs::read(fixture.config.join("cake-autorate")).unwrap();
        cake.extend(b"config cake_autorate 'peer'\n option enabled '0'\n option manage_sqm '0'\n option sqm_enabled '0'\n");
        fs::write(fixture.config.join("cake-autorate"), cake).unwrap();
        let published = fixture.published();
        let lab = fixture.input(&published);
        let peer = store(&published, "peer", &fixture.inputs, &fixture.config).unwrap();
        let batch = store_batch(
            &published,
            &["lab".into(), "peer".into()],
            &BTreeMap::from([("lab".into(), lab), ("peer".into(), peer.clone())]),
            &fixture.inputs,
            &fixture.config,
        )
        .unwrap();
        drop(published);
        batch.accept(|_| Ok(())).unwrap();
        let previous = load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap();
        fs::write(
            fixture.config.join("sqm"),
            b"# new selected operation source\n",
        )
        .unwrap();
        let published = fixture.published();
        let replacement = fixture.input(&published);
        let marker_path = fixture
            .inputs
            .join(format!("{}.accepted", peer.name().unwrap()));
        let marker = fs::read(&marker_path).unwrap();
        fs::remove_file(&marker_path).unwrap();
        assert!(
            replace_selected_batch(&published, &previous, "lab", &replacement, |_| panic!(
                "unaccepted peer must fail before runtime transition proof"
            ))
            .is_err()
        );
        assert!(!fixture.inputs.join(BATCH_PENDING).exists());
        fs::write(&marker_path, marker).unwrap();
        fs::set_permissions(&marker_path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(
            replace_selected_batch(&published, &previous, "lab", &replacement, |_| Err(
                "retained-peer-changed".into()
            ))
            .is_err()
        );
        assert!(!fixture.inputs.join(BATCH_PENDING).exists());
        let expected_retained = BTreeMap::from([("peer".into(), peer.id.clone())]);
        let pending =
            replace_selected_batch(&published, &previous, "lab", &replacement, |retained| {
                assert_eq!(retained, &expected_retained);
                Ok(())
            })
            .unwrap();
        assert!(pending.pending());
        previous.attest_record().unwrap();
        let inputs = pending.load_inputs().unwrap();
        assert_eq!(inputs["peer"].guard.sources.encode(), peer.sources.encode());
        assert_ne!(
            inputs["peer"].guard.sources.encode(),
            pending.sources.encode()
        );
        assert_eq!(
            inputs["lab"].guard.sources.encode(),
            pending.sources.encode()
        );
        assert!(pending.retire(|_| Ok(())).is_err());
        assert!(fixture.inputs.join(BATCH_PENDING).exists());
        assert!(fixture.inputs.join(BATCH_APPLIED).exists());
        assert!(pending
            .abandon_update(|_| Err("old-generation-not-restored".into()))
            .is_err());
        pending.attest_record().unwrap();
        pending
            .abandon_update(|restored| {
                assert_eq!(restored, previous.generations());
                Ok(())
            })
            .unwrap();
        assert_eq!(
            load_batch(&fixture.inputs, &fixture.config)
                .unwrap()
                .unwrap()
                .id,
            previous.id
        );
        assert!(!fixture.inputs.join(BATCH_PENDING).exists());
        let pending =
            replace_selected_batch(&published, &previous, "lab", &replacement, |retained| {
                assert_eq!(retained, &expected_retained);
                Ok(())
            })
            .unwrap();
        drop(published);
        // Inject a lost ACK immediately after the atomic exchange. Both files
        // remain, but their linked identities unambiguously select the new set.
        assert!(pending
            .accept(|map| {
                assert_eq!(map.get("peer"), Some(&peer.id));
                assert_eq!(map.get("lab"), Some(&replacement.id));
                if !load_batch(&fixture.inputs, &fixture.config)?
                    .unwrap()
                    .pending()
                {
                    return Err("lost-final-ack".into());
                }
                Ok(())
            })
            .is_err());
        let applied = load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap();
        assert!(!applied.pending());
        assert_eq!(applied.id, pending.id);
        assert!(fixture.inputs.join(BATCH_PENDING).exists());
        applied.accept(|_| Ok(())).unwrap();
        assert!(!fixture.inputs.join(BATCH_PENDING).exists());
        let inputs = applied.load_inputs().unwrap();
        assert_eq!(inputs["peer"].guard.sources.encode(), peer.sources.encode());
        assert_eq!(inputs["lab"].guard.id, replacement.id);
        // A subsequent selected update can retain an already mixed batch.
        fs::write(
            fixture.config.join("sqm"),
            b"# next selected operation source\n",
        )
        .unwrap();
        let published = fixture.published();
        let next_peer = store(&published, "peer", &fixture.inputs, &fixture.config).unwrap();
        let next = replace_selected_batch(&published, &applied, "peer", &next_peer, |retained| {
            assert_eq!(
                retained,
                &BTreeMap::from([("lab".into(), replacement.id.clone())])
            );
            Ok(())
        })
        .unwrap();
        drop(published);
        next.accept(|_| Ok(())).unwrap();
        let final_batch = load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap();
        assert_eq!(
            final_batch.load_inputs().unwrap()["lab"]
                .guard
                .sources
                .encode(),
            replacement.sources.encode()
        );
        final_batch.retire(|_| Ok(())).unwrap();
        assert!(load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .is_none());
    }

    #[test]
    fn r4_controller_batch_reload_retains_peers_and_cannot_be_adopted_as_native_apply() {
        let fixture = Fixture::new();
        let mut cake = fs::read(fixture.config.join("cake-autorate")).unwrap();
        cake.extend(b"config cake_autorate 'peer'\n option enabled '0'\n option manage_sqm '0'\n option sqm_enabled '0'\n");
        fs::write(fixture.config.join("cake-autorate"), cake).unwrap();
        let published = fixture.published();
        let lab = fixture.input(&published);
        let peer = store(&published, "peer", &fixture.inputs, &fixture.config).unwrap();
        let first = store_batch(
            &published,
            &["lab".into(), "peer".into()],
            &BTreeMap::from([("lab".into(), lab), ("peer".into(), peer.clone())]),
            &fixture.inputs,
            &fixture.config,
        )
        .unwrap();
        drop(published);
        first.accept(|_| Ok(())).unwrap();
        let previous = load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap();
        let previous_bytes = fs::read(fixture.inputs.join(BATCH_APPLIED)).unwrap();
        fs::write(fixture.config.join("sqm"), b"# ordinary reload source\n").unwrap();
        let published = fixture.published();
        let replacement = fixture.input(&published);
        let retained = BTreeMap::from([("peer".into(), peer.id.clone())]);
        let replacements = BTreeMap::from([("lab".into(), replacement.clone())]);
        let expected = vec!["lab".into(), "peer".into()];
        assert!(replace_reload_batch(
            &published,
            &previous,
            &expected,
            &retained,
            &replacements,
            |_| Err("old-runtime-changed".into())
        )
        .is_err());
        assert!(!fixture.inputs.join(BATCH_PENDING).exists());
        assert!(!fixture.inputs.join(BATCH_TEMP).exists());
        let pending = replace_reload_batch(
            &published,
            &previous,
            &expected,
            &retained,
            &replacements,
            |old| {
                assert_eq!(old, previous.generations());
                Ok(())
            },
        )
        .unwrap();
        assert!(pending.is_reload_update());
        assert!(!pending.updates_only("lab"));
        assert!(!pending.updates_only("peer"));
        assert_eq!(
            pending
                .attest_reload_plan(&expected, &retained)
                .unwrap()
                .encode(),
            published.source_receipt().unwrap().encode()
        );
        for (names, keep) in [
            (vec!["lab".into(), "lab".into()], retained.clone()),
            (expected.clone(), BTreeMap::new()),
            (
                expected.clone(),
                BTreeMap::from([("peer".into(), "0".repeat(64))]),
            ),
        ] {
            assert_eq!(
                pending.attest_reload_plan(&names, &keep).unwrap_err(),
                "controller-batch-reload-plan-mismatch"
            );
        }
        assert_eq!(
            fs::read(fixture.inputs.join(BATCH_APPLIED)).unwrap(),
            previous_bytes
        );
        let pending_bytes = fs::read(fixture.inputs.join(BATCH_PENDING)).unwrap();
        assert!(supersede_pending_batch(
            &published,
            &pending,
            "lab",
            Some(&replacement),
            |_| panic!("reload is not native selected authority")
        )
        .is_err());
        assert!(pending.ordinary_stop_receipt().is_err());
        assert!(replace_reload_batch(
            &published,
            &previous,
            &expected,
            &retained,
            &replacements,
            |_| panic!("duplicate pending intent must fail first")
        )
        .is_err());
        assert_eq!(
            fs::read(fixture.inputs.join(BATCH_PENDING)).unwrap(),
            pending_bytes
        );
        let loaded = pending.load_inputs().unwrap();
        assert_eq!(loaded["peer"].guard.sources.encode(), peer.sources.encode());
        assert_eq!(
            loaded["lab"].guard.sources.encode(),
            published.source_receipt().unwrap().encode()
        );
        drop(published);
        drop(pending);
        let recovered = load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap();
        assert!(recovered.is_reload_update());
        assert!(recovered
            .accept(|_| Err("new-runtime-not-ready".into()))
            .is_err());
        assert_eq!(
            fs::read(fixture.inputs.join(BATCH_PENDING)).unwrap(),
            pending_bytes
        );
        recovered
            .accept(|set| {
                assert_eq!(
                    set,
                    &BTreeMap::from([
                        ("lab".into(), replacement.id.clone()),
                        ("peer".into(), peer.id.clone())
                    ])
                );
                Ok(())
            })
            .unwrap();
        let applied = load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap();
        applied.attest_settled().unwrap();
        assert!(!applied.is_reload_update());
        assert!(applied.same_publication(&recovered));
        assert!(!applied.same_publication(&previous));
        assert_eq!(applied.generations()["peer"], peer.id);
        applied.accept(|_| Ok(())).unwrap(); // Lost final acknowledgement.
        assert!(!fixture.inputs.join(BATCH_PENDING).exists());
    }

    #[test]
    fn r4_controller_batch_reload_validates_membership_before_writing_intent() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let lab = fixture.input(&published);
        let first = store_batch(
            &published,
            &["lab".into()],
            &BTreeMap::from([("lab".into(), lab)]),
            &fixture.inputs,
            &fixture.config,
        )
        .unwrap();
        drop(published);
        first.accept(|_| Ok(())).unwrap();
        let previous = load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap();
        let published = fixture.published();
        let replacement = fixture.input(&published);
        let replacements = BTreeMap::from([("lab".into(), replacement)]);
        for (expected, retained) in [
            (vec!["lab".into(), "lab".into()], BTreeMap::new()),
            (vec!["lab".into()], previous.generations().clone()),
            (vec!["other".into()], BTreeMap::new()),
            (vec![], BTreeMap::new()),
            (
                vec!["lab".into(), "peer".into()],
                BTreeMap::from([("peer".into(), "0".repeat(64))]),
            ),
        ] {
            assert!(replace_reload_batch(
                &published,
                &previous,
                &expected,
                &retained,
                &replacements,
                |_| panic!("invalid partition must fail before runtime proof")
            )
            .is_err());
            assert!(!fixture.inputs.join(BATCH_PENDING).exists());
            assert!(!fixture.inputs.join(BATCH_TEMP).exists());
        }
        let pending = replace_reload_batch(
            &published,
            &previous,
            &[],
            &BTreeMap::new(),
            &BTreeMap::new(),
            |old| {
                assert_eq!(old.len(), 1);
                Ok(())
            },
        )
        .unwrap();
        assert!(pending.is_reload_update());
        assert!(pending.generations().is_empty());
        drop(published);
        pending
            .accept(|desired| {
                assert!(desired.is_empty());
                Ok(())
            })
            .unwrap();
        load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap()
            .attest_settled()
            .unwrap();
    }

    #[test]
    fn r4_controller_batch_reload_rejects_invalid_purpose_even_with_recomputed_id() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let first = store_batch(
            &published,
            &["lab".into()],
            &BTreeMap::from([("lab".into(), fixture.input(&published))]),
            &fixture.inputs,
            &fixture.config,
        )
        .unwrap();
        let bytes = fs::read(fixture.inputs.join(BATCH_PENDING)).unwrap();
        for purpose in [json!("unknown"), json!(false), json!("reload")] {
            let mut value: Value = serde_json::from_slice(&bytes[BATCH_MAGIC.len()..]).unwrap();
            value["purpose"] = purpose;
            value["id"] = json!(batch_id(&value).unwrap());
            let mut changed = BATCH_MAGIC.to_vec();
            changed.extend(serde_json::to_vec(&value).unwrap());
            fs::write(fixture.inputs.join(BATCH_PENDING), &changed).unwrap();
            assert_eq!(
                load_batch(&fixture.inputs, &fixture.config).unwrap_err(),
                "controller-batch-purpose-invalid"
            );
            assert_eq!(
                fs::read(fixture.inputs.join(BATCH_PENDING)).unwrap(),
                changed
            );
        }
        drop(first);
    }

    #[test]
    fn r4_controller_batch_reload_adds_members_with_retained_or_replaced_predecessor() {
        for retain_lab in [true, false] {
            let fixture = Fixture::new();
            let published = fixture.published();
            let lab = fixture.input(&published);
            let first = store_batch(
                &published,
                &["lab".into()],
                &BTreeMap::from([("lab".into(), lab.clone())]),
                &fixture.inputs,
                &fixture.config,
            )
            .unwrap();
            drop(published);
            first.accept(|_| Ok(())).unwrap();
            let previous = load_batch(&fixture.inputs, &fixture.config)
                .unwrap()
                .unwrap();
            let mut cake = fs::read(fixture.config.join("cake-autorate")).unwrap();
            cake.extend(b"config cake_autorate 'peer'\n option enabled '0'\n option manage_sqm '0'\n option sqm_enabled '0'\n");
            fs::write(fixture.config.join("cake-autorate"), cake).unwrap();
            let published = fixture.published();
            let mut replacements = BTreeMap::from([(
                "peer".into(),
                store(&published, "peer", &fixture.inputs, &fixture.config).unwrap(),
            )]);
            let retained = if retain_lab {
                previous.generations().clone()
            } else {
                replacements.insert("lab".into(), fixture.input(&published));
                BTreeMap::new()
            };
            let pending = replace_reload_batch(
                &published,
                &previous,
                &["lab".into(), "peer".into()],
                &retained,
                &replacements,
                |old| {
                    assert_eq!(old, previous.generations());
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(pending.generations().len(), 2);
            assert_eq!(pending.generations()["lab"] == lab.id, retain_lab);
            assert_eq!(pending.retained.contains("lab"), retain_lab);
            assert!(pending.is_reload_update());
            drop(published);
            pending
                .accept(|set| {
                    assert_eq!(set.len(), 2);
                    Ok(())
                })
                .unwrap();
            load_batch(&fixture.inputs, &fixture.config)
                .unwrap()
                .unwrap()
                .attest_settled()
                .unwrap();
        }
    }

    #[test]
    fn r4_controller_batch_reload_interrupted_draft_is_not_deleted_or_native_authority() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let first = store_batch(
            &published,
            &["lab".into()],
            &BTreeMap::from([("lab".into(), fixture.input(&published))]),
            &fixture.inputs,
            &fixture.config,
        )
        .unwrap();
        drop(published);
        first.accept(|_| Ok(())).unwrap();
        let previous = load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap();
        let published = fixture.published();
        let replacements = BTreeMap::from([("lab".into(), fixture.input(&published))]);
        let mut calls = 0;
        assert_eq!(
            replace_reload_batch(
                &published,
                &previous,
                &["lab".into()],
                &BTreeMap::new(),
                &replacements,
                |_| {
                    calls += 1;
                    if calls == 3 {
                        Err("stop-before-intent-rename".into())
                    } else {
                        Ok(())
                    }
                }
            )
            .unwrap_err(),
            "stop-before-intent-rename"
        );
        assert!(!fixture.inputs.join(BATCH_PENDING).exists());
        let bytes = fs::read(fixture.inputs.join(BATCH_TEMP)).unwrap();
        let draft = read_batch_file(&fixture.inputs, &fixture.config, BATCH_TEMP).unwrap();
        assert!(draft.reload);
        assert!(!draft.updates_only("lab"));
        assert!(!draft.is_reload_update());
        assert!(replace_reload_batch(
            &published,
            &previous,
            &["lab".into()],
            &BTreeMap::new(),
            &replacements,
            |_| Ok(())
        )
        .is_err());
        assert!(discard_unpublished_batch(&fixture.inputs, || panic!(
            "applied registry cannot be orphan-cleaned"
        ))
        .is_err());
        assert_eq!(fs::read(fixture.inputs.join(BATCH_TEMP)).unwrap(), bytes);
        previous.attest_record().unwrap();
        let mut recovery = UnpublishedReload::load(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap();
        assert!(recovery.expected_source.is_some());
        assert!(recovery
            .retire_draft(|_| Err("old-runtime-changed".into()))
            .is_err());
        assert_eq!(fs::read(fixture.inputs.join(BATCH_TEMP)).unwrap(), bytes);
        recovery
            .retire_draft(|old| {
                assert_eq!(old, previous.generations());
                Ok(())
            })
            .unwrap();
        assert!(!fixture.inputs.join(BATCH_TEMP).exists());
        recovery.attest().unwrap();
    }

    #[test]
    fn r4_unpublished_reload_partial_draft_cleanup_rejects_foreign_or_changed_records() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let first = store_batch(
            &published,
            &["lab".into()],
            &BTreeMap::from([("lab".into(), fixture.input(&published))]),
            &fixture.inputs,
            &fixture.config,
        )
        .unwrap();
        drop(published);
        first.accept(|_| Ok(())).unwrap();
        let before = fs::read(fixture.inputs.join(BATCH_APPLIED)).unwrap();
        for bytes in [
            BATCH_MAGIC[..8].to_vec(),
            [BATCH_MAGIC, b"{\"schema\":1,"].concat(),
        ] {
            fs::write(fixture.inputs.join(BATCH_TEMP), &bytes).unwrap();
            fs::set_permissions(
                fixture.inputs.join(BATCH_TEMP),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            let mut recovery = UnpublishedReload::load(&fixture.inputs, &fixture.config)
                .unwrap()
                .unwrap();
            assert!(recovery.expected_source.is_none());
            assert!(recovery
                .retire_draft(|_| Err("runtime-not-proven".into()))
                .is_err());
            assert_eq!(fs::read(fixture.inputs.join(BATCH_TEMP)).unwrap(), bytes);
            recovery
                .retire_draft(|old| {
                    assert_eq!(old.len(), 1);
                    Ok(())
                })
                .unwrap();
            assert!(!fixture.inputs.join(BATCH_TEMP).exists());
            assert_eq!(
                fs::read(fixture.inputs.join(BATCH_APPLIED)).unwrap(),
                before
            );
        }
        fs::write(fixture.inputs.join(BATCH_TEMP), b"foreign bytes").unwrap();
        fs::set_permissions(
            fixture.inputs.join(BATCH_TEMP),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert!(UnpublishedReload::load(&fixture.inputs, &fixture.config).is_err());
        assert_eq!(
            fs::read(fixture.inputs.join(BATCH_TEMP)).unwrap(),
            b"foreign bytes"
        );
        fs::write(fixture.inputs.join(BATCH_TEMP), BATCH_MAGIC).unwrap();
        let mut recovery = UnpublishedReload::load(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap();
        fs::write(fixture.inputs.join(BATCH_TEMP), b"changed while inspected").unwrap();
        assert!(recovery
            .retire_draft(|_| panic!("changed draft fails before runtime proof"))
            .is_err());
        assert_eq!(
            fs::read(fixture.inputs.join(BATCH_TEMP)).unwrap(),
            b"changed while inspected"
        );
    }

    #[test]
    fn r4_unpublished_reload_never_adopts_a_native_selected_draft() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let first = store_batch(
            &published,
            &["lab".into()],
            &BTreeMap::from([("lab".into(), fixture.input(&published))]),
            &fixture.inputs,
            &fixture.config,
        )
        .unwrap();
        drop(published);
        first.accept(|_| Ok(())).unwrap();
        let previous = load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap();
        let published = fixture.published();
        let replacement = fixture.input(&published);
        let native =
            replace_selected_batch(&published, &previous, "lab", &replacement, |_| Ok(())).unwrap();
        assert!(UnpublishedReload::load(&fixture.inputs, &fixture.config).is_err());
        fs::rename(
            fixture.inputs.join(BATCH_PENDING),
            fixture.inputs.join(BATCH_TEMP),
        )
        .unwrap();
        let bytes = fs::read(fixture.inputs.join(BATCH_TEMP)).unwrap();
        assert!(UnpublishedReload::load(&fixture.inputs, &fixture.config).is_err());
        assert_eq!(fs::read(fixture.inputs.join(BATCH_TEMP)).unwrap(), bytes);
        drop(native);
    }

    #[test]
    fn r4_controller_batch_interrupted_acceptance_retries_without_adopting_new_input() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let input = fixture.input(&published);
        let batch = store_batch(
            &published,
            &["lab".into()],
            &BTreeMap::from([("lab".into(), input.clone())]),
            &fixture.inputs,
            &fixture.config,
        )
        .unwrap();
        drop(published);
        let mut step = 0;
        assert!(batch
            .accept(|_| {
                step += 1;
                if step == 4 {
                    Err("interrupted-before-file-acceptance".into())
                } else {
                    Ok(())
                }
            })
            .is_err());
        assert!(input
            .accepted(&Directory::open(&fixture.inputs, true, false).unwrap())
            .unwrap());
        assert!(load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap()
            .pending());
        batch
            .accept(|actual| {
                assert_eq!(actual.get("lab").unwrap(), input.generation());
                Ok(())
            })
            .unwrap();
        assert!(!load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap()
            .pending());
    }

    #[test]
    fn r4_controller_batch_partial_write_recovery_requires_absence_and_owned_prefix() {
        for partial in [b"".as_slice(), &BATCH_MAGIC[..8], BATCH_MAGIC] {
            let fixture = Fixture::new();
            let (directory, lock) = lock_store(&fixture.inputs).unwrap();
            let path = fixture.inputs.join(BATCH_TEMP);
            fs::write(&path, partial).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            drop(lock);
            assert!(load_batch(&fixture.inputs, &fixture.config).is_err());
            assert!(
                discard_unpublished_batch(&fixture.inputs, || Err("live-helper".into())).is_err()
            );
            assert_eq!(fs::read(&path).unwrap(), partial);
            let mut checks = 0;
            discard_unpublished_batch(&fixture.inputs, || {
                checks += 1;
                Ok(())
            })
            .unwrap();
            assert_eq!(checks, 2);
            assert!(!path.exists());
            directory.attest().unwrap();
        }
        let fixture = Fixture::new();
        let (_, lock) = lock_store(&fixture.inputs).unwrap();
        let path = fixture.inputs.join(BATCH_TEMP);
        fs::write(&path, b"foreign").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        drop(lock);
        assert!(discard_unpublished_batch(&fixture.inputs, || Ok(())).is_err());
        assert_eq!(fs::read(path).unwrap(), b"foreign");
    }

    #[test]
    fn r4_controller_batch_requires_exact_membership_and_survives_preparer_exit() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let input = fixture.input(&published);
        let inputs = BTreeMap::from([("lab".into(), input.clone())]);
        for names in [
            vec![],
            vec!["other".into()],
            vec!["lab".into(), "lab".into()],
        ] {
            assert!(store_batch(
                &published,
                &names,
                &inputs,
                &fixture.inputs,
                &fixture.config
            )
            .is_err());
        }
        let batch = store_batch(
            &published,
            &["lab".into()],
            &inputs,
            &fixture.inputs,
            &fixture.config,
        )
        .unwrap();
        let expected = BTreeMap::from([("lab".into(), input.generation().into())]);
        assert_eq!(batch.generations(), &expected);
        assert!(batch.pending());
        assert!(store_batch(
            &published,
            &["lab".into()],
            &inputs,
            &fixture.inputs,
            &fixture.config
        )
        .is_err());
        drop(batch);
        drop(published);
        let batch = load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap();
        assert!(batch.accept(|_| Err("not-ready".into())).is_err());
        assert!(batch.pending());
        assert!(!input
            .accepted(&Directory::open(&fixture.inputs, true, false).unwrap())
            .unwrap());
        let mut checks = 0;
        batch
            .accept(|actual| {
                assert_eq!(actual, &expected);
                checks += 1;
                Ok(())
            })
            .unwrap();
        assert!(checks >= 4);
        let applied = load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap();
        assert!(!applied.pending());
        assert!(input
            .accepted(&Directory::open(&fixture.inputs, true, false).unwrap())
            .unwrap());
        fs::write(
            fixture.config.join("cake-autorate"),
            b"# later user commit\n",
        )
        .unwrap();
        applied.attest().unwrap();
        assert_eq!(
            applied.load_inputs().unwrap()["lab"].guard.generation(),
            input.generation()
        );
        assert!(applied
            .retire(|_| Err("runtime-still-present".into()))
            .is_err());
        applied.attest_record().unwrap();
        applied
            .retire(|actual| {
                assert_eq!(actual, &expected);
                Ok(())
            })
            .unwrap();
        assert!(load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .is_none());
        assert_eq!(
            fs::read(fixture.config.join("cake-autorate")).unwrap(),
            b"# later user commit\n"
        );
    }

    #[test]
    fn r4_controller_batch_refuses_source_drift_and_corruption_without_deleting_evidence() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let input = fixture.input(&published);
        let inputs = BTreeMap::from([("lab".into(), input)]);
        let batch = store_batch(
            &published,
            &["lab".into()],
            &inputs,
            &fixture.inputs,
            &fixture.config,
        )
        .unwrap();
        let old = batch.bytes.clone();
        assert!(batch
            .accept(|_| {
                fs::write(fixture.config.join("sqm"), b"# concurrent commit\n").unwrap();
                Ok(())
            })
            .is_err());
        assert!(fixture.inputs.join(BATCH_PENDING).exists());
        assert!(!fixture.inputs.join(BATCH_APPLIED).exists());
        assert_eq!(
            fs::read(fixture.config.join("sqm")).unwrap(),
            b"# concurrent commit\n"
        );
        let mut value: Value =
            serde_json::from_slice(old.strip_prefix(BATCH_MAGIC).unwrap()).unwrap();
        value["generations"]["lab"] = json!("0".repeat(64));
        let mut bad = BATCH_MAGIC.to_vec();
        bad.extend(serde_json::to_vec(&value).unwrap());
        fs::write(fixture.inputs.join(BATCH_PENDING), &bad).unwrap();
        assert!(load_batch(&fixture.inputs, &fixture.config).is_err());
        assert!(batch.retire(|_| Ok(())).is_err());
        assert_eq!(fs::read(fixture.inputs.join(BATCH_PENDING)).unwrap(), bad);
    }

    #[test]
    fn r4_controller_batch_acceptance_retry_and_empty_membership_keep_runtime_proof() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let batch = store_batch(
            &published,
            &[],
            &BTreeMap::new(),
            &fixture.inputs,
            &fixture.config,
        )
        .unwrap();
        drop(published);
        assert!(batch.accept(|_| Err("residual-process".into())).is_err());
        batch
            .accept(|map| {
                assert!(map.is_empty());
                Ok(())
            })
            .unwrap();
        let batch = load_batch(&fixture.inputs, &fixture.config)
            .unwrap()
            .unwrap();
        assert!(batch.accept(|_| Err("late-process".into())).is_err());
        batch
            .accept(|map| {
                assert!(map.is_empty());
                Ok(())
            })
            .unwrap();
        batch.retire(|_| Ok(())).unwrap();
    }

    #[test]
    fn r4_controller_input_is_bound_to_instance_generation_and_native_history() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let guard = fixture.input(&published);
        assert_eq!(guard.instance(), "lab");
        assert!(generation(guard.generation()));
        let loaded = load("lab", guard.generation(), &fixture.inputs, &fixture.config).unwrap();
        assert_eq!(loaded.config.instance, "lab");
        assert!(!loaded.config.enabled);
        assert_eq!(loaded.config.graph_history_ram_budget_kib, Some(512));
        assert_eq!(loaded.config.graph_history_instance_count, 1);
        loaded.guard.attest().unwrap();
        assert!(load(
            "other",
            guard.generation(),
            &fixture.inputs,
            &fixture.config
        )
        .is_err());
        assert!(load(
            "../lab",
            guard.generation(),
            &fixture.inputs,
            &fixture.config
        )
        .is_err());
        assert!(load(
            "lab",
            "invalid-private-value",
            &fixture.inputs,
            &fixture.config
        )
        .err()
        .unwrap()
        .contains("identity-invalid"));
        assert_eq!(fs::metadata(&fixture.inputs).unwrap().mode() & 0o777, 0o700);
        assert_eq!(
            fs::metadata(fixture.inputs.join(guard.name().unwrap()))
                .unwrap()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn r4_controller_input_pending_source_drift_fails_but_accepted_generation_remains_respawnable()
    {
        for accepted in [false, true] {
            let fixture = Fixture::new();
            let published = fixture.published();
            let guard = fixture.input(&published);
            if accepted {
                guard
                    .mark_accepted(|instance, id| {
                        assert_eq!(instance, "lab");
                        assert_eq!(id, guard.generation());
                        Ok(())
                    })
                    .unwrap();
            }
            OpenOptions::new()
                .append(true)
                .open(fixture.config.join("cake-autorate"))
                .unwrap()
                .write_all(b"# unrelated later commit\n")
                .unwrap();
            assert_eq!(guard.attest().is_ok(), accepted);
            assert_eq!(
                load("lab", guard.generation(), &fixture.inputs, &fixture.config).is_ok(),
                accepted
            );
        }
    }

    #[test]
    fn r4_controller_input_content_change_cannot_reuse_a_valid_generation_name() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let guard = fixture.input(&published);
        let path = fixture.inputs.join(guard.name().unwrap());
        let bytes = fs::read(&path).unwrap();
        let mut value: Value = serde_json::from_slice(bytes.strip_prefix(MAGIC).unwrap()).unwrap();
        value["history_budget_kib"] = json!(1024);
        let mut changed = MAGIC.to_vec();
        changed.extend(serde_json::to_vec(&value).unwrap());
        fs::write(path, changed).unwrap();
        assert!(guard.attest().is_err());
        assert_eq!(
            load("lab", guard.generation(), &fixture.inputs, &fixture.config)
                .err()
                .unwrap(),
            "controller-input-identity-mismatch"
        );
    }

    #[test]
    fn r4_controller_input_recipe_cannot_include_another_owner_even_with_a_valid_content_id() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let guard = fixture.input(&published);
        let bytes = fs::read(fixture.inputs.join(guard.name().unwrap())).unwrap();
        let original: Value = serde_json::from_slice(bytes.strip_prefix(MAGIC).unwrap()).unwrap();
        for recipe in [
            json!("sqm.alien=queue\nsqm.alien._cake_autorate_managed='other'\n"),
            json!(["not a canonical show"]),
        ] {
            let mut value = original.clone();
            value["sqm_show"] = recipe;
            let id = record_id(&value).unwrap();
            value["generation"] = json!(id);
            assert!(parse_config(&value, "lab", &id).is_err());
        }
    }

    #[test]
    fn r4_controller_input_rejects_links_and_unsafe_permissions_without_exposing_bytes() {
        for case in 0..4 {
            let fixture = Fixture::new();
            let published = fixture.published();
            let guard = fixture.input(&published);
            let path = fixture.inputs.join(guard.name().unwrap());
            let foreign = fixture.root.join("foreign");
            fs::write(&foreign, b"private fixture bytes").unwrap();
            match case {
                0 => {
                    fs::remove_file(&path).unwrap();
                    symlink(&foreign, &path).unwrap();
                }
                1 => {
                    fs::hard_link(&path, fixture.root.join("foreign-link")).unwrap();
                }
                2 => {
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
                }
                _ => {
                    fs::write(&path, b"private fixture bytes").unwrap();
                }
            }
            let error = load("lab", guard.generation(), &fixture.inputs, &fixture.config)
                .err()
                .unwrap();
            assert!(!error.contains("private fixture"));
            assert_eq!(fs::read(foreign).unwrap(), b"private fixture bytes");
        }
    }

    #[test]
    fn r4_controller_input_acceptance_requires_same_generation_runtime_proof_and_rechecks_source() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let guard = fixture.input(&published);
        assert!(guard.mark_accepted(|_, _| Err("not-ready".into())).is_err());
        assert!(!fixture
            .inputs
            .join(format!("{}.accepted", guard.name().unwrap()))
            .exists());
        let mut checks = 0;
        assert!(guard
            .mark_accepted(|_, _| {
                checks += 1;
                if checks == 2 {
                    Err("changed-during-acceptance".into())
                } else {
                    Ok(())
                }
            })
            .is_err());
        assert!(!fixture
            .inputs
            .join(format!("{}.accepted", guard.name().unwrap()))
            .exists());
        guard.mark_accepted(|_, _| Ok(())).unwrap();
        guard.mark_accepted(|_, _| Ok(())).unwrap();
        guard.attest().unwrap();
    }

    #[test]
    fn r4_controller_input_retries_owned_partial_acceptance_and_retires_only_unreferenced_generation(
    ) {
        let fixture = Fixture::new();
        let published = fixture.published();
        let guard = fixture.input(&published);
        let other = fixture.input(&published);
        let path = fixture
            .inputs
            .join(format!("{}.accepted.tmp", guard.name().unwrap()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap();
        file.write_all(&guard.marker()[..5]).unwrap();
        guard.mark_accepted(|_, _| Ok(())).unwrap();
        assert!(guard.retire(|_, _| Err("still-referenced".into())).is_err());
        guard.attest().unwrap();
        guard
            .retire(|instance, id| {
                assert_eq!(instance, "lab");
                assert_eq!(id, guard.generation());
                Ok(())
            })
            .unwrap();
        assert!(!fixture.inputs.join(guard.name().unwrap()).exists());
        other.attest().unwrap();
    }

    #[test]
    fn r4_controller_input_bounds_private_store_and_preserves_foreign_entries() {
        let fixture = Fixture::new();
        let published = fixture.published();
        let guard = fixture.input(&published);
        let note = fixture.inputs.join("foreign-note");
        fs::write(&note, b"not our file").unwrap();
        assert!(store(&published, "lab", &fixture.inputs, &fixture.config).is_err());
        assert_eq!(fs::read(note).unwrap(), b"not our file");
        guard.attest().unwrap();
    }

    #[test]
    fn r4_controller_input_store_enforces_both_entry_and_aggregate_byte_bounds() {
        for byte_limit in [false, true] {
            let fixture = Fixture::new();
            let published = fixture.published();
            let _guard = fixture.input(&published);
            let count = if byte_limit { 5 } else { MAX_ENTRIES };
            for index in 0..count {
                let path = fixture.inputs.join(format!("fixture.{index:064x}"));
                let file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(path)
                    .unwrap();
                if byte_limit {
                    file.set_len(MAX_RECORD).unwrap();
                }
            }
            let error = store(&published, "lab", &fixture.inputs, &fixture.config)
                .err()
                .unwrap();
            assert_eq!(
                error,
                if byte_limit {
                    "controller-input-store-byte-limit"
                } else {
                    "controller-input-store-entry-limit"
                }
            );
        }
    }
}
