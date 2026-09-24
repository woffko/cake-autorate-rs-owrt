//! Selected publisher plan publication. The caller owns the native lifecycle
//! lock, publisher Stop/registration, and parent Apply rollback. This never
//! cleans a shared directory or derives peer intent from current UCI.

use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;

#[derive(Clone, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    length: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl FileIdentity {
    fn capture(metadata: &fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

// Deliberately no Debug: plan bytes may contain a broker password.
#[derive(Clone, PartialEq, Eq)]
struct PlanFile {
    identity: FileIdentity,
    bytes: Vec<u8>,
}

#[derive(PartialEq, Eq)]
struct Plans {
    directory: Option<(u64, u64)>,
    files: BTreeMap<String, PlanFile>,
}

/// Read-only ordering for ordinary reload. Resume the one owned linked draft
/// first; remove obsolete plans before additions can hit the directory bound.
/// Plan contents may contain credentials and never leave this module as logs.
pub(crate) fn reload_order(
    root: &Path,
    desired: &BTreeSet<String>,
    registered: &BTreeSet<String>,
    job: &str,
) -> Result<Vec<String>, String> {
    if job.len() != 32
        || !job
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || desired.len() > MAX_INSTANCES
        || registered.len() > MAX_INSTANCES
        || desired
            .iter()
            .chain(registered)
            .any(|name| !super::super::runtime_health::safe_name(name))
    {
        return Err("MQTT reload membership or operation identity invalid".into());
    }
    let mut instances: BTreeSet<_> = desired.union(registered).cloned().collect();
    let mut recovery = None;
    match fs::symlink_metadata(root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err("MQTT reload plan root unavailable".into()),
        Ok(_) => {
            let directory = open_plan_root(root)?;
            let before = directory
                .metadata()
                .map_err(|_| "MQTT reload root inspection failed")?;
            for (index, entry) in fs::read_dir(root)
                .map_err(|_| "MQTT reload enumeration failed")?
                .enumerate()
            {
                if index > MAX_INSTANCES {
                    return Err("MQTT reload directory exceeds its bound".into());
                }
                let entry = entry.map_err(|_| "MQTT reload entry unavailable")?;
                let name = entry.file_name();
                let name = name.to_str().ok_or("MQTT reload entry invalid")?;
                if let Some(instance) = name.strip_suffix(".plan") {
                    plan_path(root, instance)?;
                    read_service_plan(root, instance)?;
                    instances.insert(instance.into());
                } else if let Some(rest) = name.strip_prefix(".selected_") {
                    let (instance, owner) =
                        rest.rsplit_once('_').ok_or("MQTT reload draft invalid")?;
                    plan_path(root, instance)?;
                    if owner != job || recovery.is_some() {
                        return Err(
                            "MQTT reload draft belongs to another or multiple operations".into(),
                        );
                    }
                    read_plan_file(&entry.path(), instance)?;
                    recovery = Some(instance.to_string());
                } else {
                    return Err("MQTT reload directory contains an unknown entry".into());
                }
            }
            let after = open_plan_root(root)?
                .metadata()
                .map_err(|_| "MQTT reload root inspection failed")?;
            if before.dev() != after.dev() || before.ino() != after.ino() {
                return Err("MQTT reload root changed".into());
            }
        }
    }
    if recovery
        .as_ref()
        .is_some_and(|name| !instances.contains(name))
    {
        return Err("MQTT reload draft has no desired or previous member".into());
    }
    let mut ordered: Vec<_> = instances.into_iter().collect();
    ordered.sort_by(|a, b| {
        let priority = |name: &String| {
            if recovery.as_ref() == Some(name) {
                0
            } else if !desired.contains(name) {
                1
            } else {
                2
            }
        };
        (priority(a), a).cmp(&(priority(b), b))
    });
    Ok(ordered)
}

impl Plans {
    fn capture(root: &Path, draft: Option<(&Path, &FileIdentity)>) -> Result<Self, String> {
        match fs::symlink_metadata(root) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Self {
                    directory: None,
                    files: BTreeMap::new(),
                });
            }
            Err(error) => return Err(format!("unable to inspect MQTT plan root: {error}")),
            Ok(_) => {}
        }
        let directory = open_plan_root(root)?;
        let metadata = directory
            .metadata()
            .map_err(|_| "MQTT directory inspection failed")?;
        let identity = (metadata.dev(), metadata.ino());
        let mut files = BTreeMap::new();
        let mut draft_seen = false;
        for entry in fs::read_dir(root).map_err(|_| "MQTT plan enumeration failed")? {
            let entry = entry.map_err(|_| "MQTT plan entry inspection failed")?;
            if let Some((path, expected)) = draft {
                if entry.path() == path {
                    let metadata =
                        inspect_plan_file(path)?.ok_or("MQTT selected draft disappeared")?;
                    if &FileIdentity::capture(&metadata) != expected || metadata.dev() != identity.0
                    {
                        return Err("MQTT selected draft changed".into());
                    }
                    draft_seen = true;
                    continue;
                }
            }
            let name = entry.file_name();
            let instance = name
                .to_str()
                .and_then(|name| name.strip_suffix(".plan"))
                .ok_or("MQTT plan directory contains an unknown entry")?;
            let path = plan_path(root, instance)?;
            let before = inspect_plan_file(&path)?.ok_or("MQTT plan disappeared")?;
            let config = read_service_plan(root, instance)?;
            let after = inspect_plan_file(&path)?.ok_or("MQTT plan disappeared")?;
            if FileIdentity::capture(&before) != FileIdentity::capture(&after)
                || after.dev() != identity.0
            {
                return Err("MQTT plan changed during selected capture".into());
            }
            let bytes = config.encode_plan()?;
            if bytes.len() as u64 != after.len() {
                return Err("MQTT selected plan is not canonical".into());
            }
            files.insert(
                instance.to_string(),
                PlanFile {
                    identity: FileIdentity::capture(&after),
                    bytes,
                },
            );
            if files.len() > MAX_INSTANCES {
                return Err("too many selected MQTT peer plans".into());
            }
        }
        if draft.is_some() && !draft_seen {
            return Err("MQTT selected draft disappeared".into());
        }
        let current = open_plan_root(root)?
            .metadata()
            .map_err(|_| "MQTT directory inspection failed")?;
        if (current.dev(), current.ino()) != identity {
            return Err("MQTT plan directory changed during selected capture".into());
        }
        Ok(Self {
            directory: Some(identity),
            files,
        })
    }
}

/// A read-only preflight receipt. Retain it before stopping a healthy runtime.
/// Dropping it never restores files: the parent Apply owns rollback authority.
pub(crate) struct SelectedPlan {
    root: PathBuf,
    instance: String,
    before: Plans,
    desired: Option<Vec<u8>>,
    temporary: PathBuf,
    recovered: Option<FileIdentity>,
    unnamed: Option<File>,
}

impl SelectedPlan {
    pub(crate) fn prepare(
        root: &Path,
        instance: &str,
        package: &UciPackage,
        controller_should_run: bool,
        job: &str,
    ) -> Result<Self, String> {
        plan_path(root, instance)?;
        if job.len() != 32
            || !job
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err("MQTT selected operation ID is invalid".into());
        }
        let temporary = root.join(format!(".selected_{instance}_{job}"));
        let recovered = match fs::symlink_metadata(root) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            _ => {
                open_plan_root(root)?;
                if let Some(before) = inspect_plan_file(&temporary)? {
                    // A named draft is linked only AFTER the complete file is
                    // synced. Partial/unknown payloads are not ours to remove.
                    read_plan_file(&temporary, instance)?;
                    let after =
                        inspect_plan_file(&temporary)?.ok_or("MQTT recovery draft disappeared")?;
                    if FileIdentity::capture(&before) != FileIdentity::capture(&after) {
                        return Err("MQTT recovery draft changed".into());
                    }
                    Some(FileIdentity::capture(&after))
                } else {
                    None
                }
            }
        };
        let desired = if controller_should_run {
            let section = package
                .sections
                .get(instance)
                .ok_or("MQTT selected controller is missing")?;
            MqttPublisherConfig::from_section(instance, section)?
                .map(|config| config.encode_plan())
                .transpose()?
        } else {
            // Stop/absent-bootstrap containment never re-enables a publisher
            // merely because a stale mqtt_enabled option remains in UCI.
            None
        };
        let before = Plans::capture(
            root,
            recovered
                .as_ref()
                .map(|identity| (temporary.as_path(), identity)),
        )?;
        if desired.is_some()
            && !before.files.contains_key(instance)
            && before.files.len() >= MAX_INSTANCES
        {
            return Err("MQTT selected plan capacity exhausted".into());
        }
        // Prove O_TMPFILE support and write the candidate before healthy Stop.
        // Unnamed data disappears automatically if this process exits.
        let unnamed = if let Some(bytes) = &desired {
            if before.files.get(instance).map(|plan| &plan.bytes) != Some(bytes) {
                let storage = if before.directory.is_some() {
                    root
                } else {
                    root.parent().ok_or("MQTT plan root has no parent")?
                };
                let mut file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .mode(0o600)
                    .custom_flags(libc::O_TMPFILE | libc::O_CLOEXEC | libc::O_NOFOLLOW)
                    .open(storage)
                    .map_err(|error| format!("MQTT unnamed plan preparation failed: {error}"))?;
                file.set_permissions(fs::Permissions::from_mode(0o600))
                    .map_err(|_| "MQTT unnamed plan permissions failed")?;
                file.write_all(bytes)
                    .and_then(|()| file.sync_all())
                    .map_err(|_| "MQTT unnamed plan persistence failed")?;
                Some(file)
            } else {
                None
            }
        } else {
            None
        };
        Ok(Self {
            root: root.into(),
            instance: instance.into(),
            before,
            desired,
            temporary,
            recovered,
            unnamed,
        })
    }

    pub(crate) fn needs_recovery(&self) -> bool {
        self.recovered.is_some()
    }

    pub(crate) fn enabled(&self) -> bool {
        self.desired.is_some()
    }

    pub(crate) fn startup_digest(&self) -> Option<[u8; 32]> {
        use sha2::{Digest, Sha256};
        self.desired
            .as_ref()
            .map(|bytes| Sha256::digest(bytes).into())
    }

    pub(crate) fn changed(&self) -> bool {
        self.before
            .files
            .get(&self.instance)
            .map(|plan| &plan.bytes)
            != self.desired.as_ref()
    }

    pub(crate) fn attest(&self) -> Result<(), String> {
        if Plans::capture(
            &self.root,
            self.recovered
                .as_ref()
                .map(|identity| (self.temporary.as_path(), identity)),
        )? != self.before
        {
            return Err("MQTT plan receipt changed before selected publication".into());
        }
        Ok(())
    }

    /// Caller must prove selected publisher absence and source authority on
    /// every callback. The unchanged case is read-only and needs no Stop.
    /// Errors after publication are propagated to parent lifecycle rollback.
    pub(crate) fn publish(
        &mut self,
        mut prove_stopped: impl FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        self.attest()?;
        if self.recovered.is_some() {
            prove_stopped()?;
            self.attest()?;
            let directory = open_plan_root(&self.root)?;
            fs::remove_file(&self.temporary).map_err(|_| "MQTT recovered draft removal failed")?;
            directory
                .sync_all()
                .map_err(|_| "MQTT recovered draft sync failed")?;
            self.recovered = None;
            prove_stopped()?;
            self.attest()?;
        }
        if !self.changed() {
            return Ok(());
        }
        prove_stopped()?;
        self.attest()?;
        if self.before.directory.is_none() {
            fs::create_dir(&self.root).map_err(|_| "MQTT selected root creation failed")?;
            fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))
                .map_err(|_| "MQTT selected root permissions failed")?;
        }
        let directory = open_plan_root(&self.root)?;
        let metadata = directory
            .metadata()
            .map_err(|_| "MQTT directory inspection failed")?;
        let directory_id = (metadata.dev(), metadata.ino());
        if let Some(before) = self.before.directory {
            if before != directory_id {
                return Err("MQTT selected directory replaced".into());
            }
        } else {
            self.before.directory = Some(directory_id);
        }
        self.attest()?;
        let path = plan_path(&self.root, &self.instance)?;
        if self.desired.is_some() {
            let temporary = self.temporary.clone();
            let unnamed = self
                .unnamed
                .as_ref()
                .ok_or("MQTT unnamed candidate missing")?;
            link_complete_draft(unnamed, &directory, &temporary)?;
            let staged = inspect_plan_file(&temporary)?.ok_or("MQTT staged plan disappeared")?;
            let publication: Result<(), String> = (|| {
                prove_stopped()?;
                // Exclude only the exact owned draft, never arbitrary staging
                // files left by another lifecycle operation.
                if Plans::capture(
                    &self.root,
                    Some((&temporary, &FileIdentity::capture(&staged))),
                )? != self.before
                {
                    return Err("MQTT plan receipt changed before selected publication".into());
                }
                fs::rename(&temporary, &path).map_err(|_| "MQTT selected publication failed")?;
                Ok(())
            })();
            if publication.is_err() {
                if let Ok(Some(current)) = inspect_plan_file(&temporary) {
                    if FileIdentity::capture(&current) == FileIdentity::capture(&staged) {
                        let _ = fs::remove_file(&temporary);
                    }
                }
            }
            publication?;
        } else {
            prove_stopped()?;
            self.attest()?;
            fs::remove_file(&path).map_err(|_| "MQTT selected removal failed")?;
        }
        directory
            .sync_all()
            .map_err(|_| "MQTT selected directory sync failed")?;
        let after = Plans::capture(&self.root, None)?;
        if after.directory != self.before.directory
            || after.files.get(&self.instance).map(|file| &file.bytes) != self.desired.as_ref()
            || after
                .files
                .iter()
                .filter(|(name, _)| *name != &self.instance)
                .ne(self
                    .before
                    .files
                    .iter()
                    .filter(|(name, _)| *name != &self.instance))
        {
            return Err("MQTT selected publication or peer plans changed".into());
        }
        self.before = after;
        prove_stopped()?;
        self.attest()
    }
}

fn link_complete_draft(file: &File, directory: &File, target: &Path) -> Result<(), String> {
    let metadata = file
        .metadata()
        .map_err(|_| "MQTT unnamed plan inspection failed")?;
    let parent = directory
        .metadata()
        .map_err(|_| "MQTT plan directory inspection failed")?;
    if metadata.nlink() != 0 || metadata.dev() != parent.dev() || metadata.mode() & 0o7777 != 0o600
    {
        return Err("MQTT unnamed plan ownership changed".into());
    }
    let source = CString::new(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .map_err(|_| "MQTT unnamed source path invalid")?;
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("MQTT draft name invalid")?;
    let name = CString::new(name).map_err(|_| "MQTT draft name invalid")?;
    // SAFETY: Both C strings live across linkat and contain no interior NUL.
    // File/Directory own valid live descriptors; the source is our own procfs
    // descriptor, never user-supplied. Destination is a single checked name
    // relative to the retained directory. linkat refuses an existing target.
    // This publishes a fully synced inode only, with no new unsafe ownership.
    let result = unsafe {
        libc::linkat(
            libc::AT_FDCWD,
            source.as_ptr(),
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::AT_SYMLINK_FOLLOW,
        )
    };
    if result != 0 {
        return Err(format!(
            "MQTT complete draft link failed: {}",
            io::Error::last_os_error()
        ));
    }
    directory
        .sync_all()
        .map_err(|_| "MQTT complete draft directory sync failed".to_string())
}

#[cfg(test)]
mod tests {
    const JOB: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    use super::super::tests::{section, temp_root};
    use super::*;

    fn package() -> UciPackage {
        UciPackage {
            sections: BTreeMap::from([("lab".into(), section(&[])), ("peer".into(), section(&[]))]),
        }
    }

    #[test]
    fn r4_mqtt_reload_order_resumes_owned_draft_then_frees_capacity_before_additions() {
        let root = temp_root("reload-order");
        publish_service_plans(&package(), &root).unwrap();
        let desired = BTreeSet::from(["alpha".into(), "peer".into()]);
        let registered = BTreeSet::from(["lab".into(), "peer".into()]);
        let before = Plans::capture(&root, None).unwrap();
        assert_eq!(
            reload_order(&root, &desired, &registered, JOB).unwrap(),
            ["lab", "alpha", "peer"]
        );
        assert!(Plans::capture(&root, None).unwrap() == before);
        let draft = root.join(format!(".selected_peer_{JOB}"));
        fs::copy(root.join("peer.plan"), &draft).unwrap();
        fs::set_permissions(&draft, fs::Permissions::from_mode(0o600)).unwrap();
        let bytes = fs::read(&draft).unwrap();
        assert_eq!(
            reload_order(&root, &desired, &registered, JOB).unwrap(),
            ["peer", "lab", "alpha"]
        );
        assert_eq!(fs::read(&draft).unwrap(), bytes);
        assert!(reload_order(&root, &desired, &registered, &"b".repeat(32)).is_err());
        assert_eq!(fs::read(&draft).unwrap(), bytes);
        let second = root.join(format!(".selected_lab_{JOB}"));
        fs::copy(root.join("lab.plan"), &second).unwrap();
        fs::set_permissions(&second, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(reload_order(&root, &desired, &registered, JOB).is_err());
        assert!(draft.exists() && second.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn r4_mqtt_reload_order_handles_absence_and_refuses_unknown_or_invalid_entries() {
        let root = temp_root("reload-order-invalid");
        let absent = root.join("absent");
        let desired = BTreeSet::from(["new".into()]);
        assert_eq!(
            reload_order(&absent, &desired, &BTreeSet::new(), JOB).unwrap(),
            ["new"]
        );
        assert!(!absent.exists());
        assert!(reload_order(&absent, &desired, &BTreeSet::new(), "bad").is_err());
        publish_service_plans(&package(), &root).unwrap();
        fs::write(root.join("unknown"), b"foreign").unwrap();
        assert!(reload_order(&root, &desired, &BTreeSet::new(), JOB).is_err());
        assert_eq!(fs::read(root.join("unknown")).unwrap(), b"foreign");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn r4_mqtt_reload_at_full_plan_capacity_removes_old_before_creating_new() {
        let root = temp_root("reload-full-capacity");
        let old = UciPackage {
            sections: (0..MAX_INSTANCES)
                .map(|i| (format!("z{i:03}"), section(&[])))
                .collect(),
        };
        let desired = UciPackage {
            sections: (0..MAX_INSTANCES)
                .map(|i| (format!("a{i:03}"), section(&[])))
                .collect(),
        };
        publish_service_plans(&old, &root).unwrap();
        let registered = old.sections.keys().cloned().collect();
        let desired_names = desired.sections.keys().cloned().collect();
        let order = reload_order(&root, &desired_names, &registered, JOB).unwrap();
        assert_eq!(order.len(), 2 * MAX_INSTANCES);
        assert!(order[..MAX_INSTANCES]
            .iter()
            .all(|name| name.starts_with('z')));
        assert!(order[MAX_INSTANCES..]
            .iter()
            .all(|name| name.starts_with('a')));
        assert!(SelectedPlan::prepare(&root, "a000", &desired, true, JOB).is_err());
        let mut removal =
            SelectedPlan::prepare(&root, &order[0], &UciPackage::default(), false, JOB).unwrap();
        removal.publish(|| Ok(())).unwrap(); // Synthetic publisher absence.
        let mut addition = SelectedPlan::prepare(&root, "a000", &desired, true, JOB).unwrap();
        addition.publish(|| Ok(())).unwrap();
        assert_eq!(
            Plans::capture(&root, None).unwrap().files.len(),
            MAX_INSTANCES
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn r4_selected_mqtt_draft_crash_helper() {
        let Some(root) = std::env::var_os("CAKE_TEST_MQTT_CRASH_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        assert!(root.is_absolute());
        // Only the parent-created marked fixture can opt into abrupt exit.
        assert_eq!(
            fs::read(root.join("../crash-fixture")).unwrap(),
            b"mqtt-draft-crash-test-v1"
        );
        let mut desired = package();
        desired
            .sections
            .get_mut("lab")
            .unwrap()
            .options
            .insert("mqtt_min_interval_s".into(), "3".into());
        let selected = SelectedPlan::prepare(&root, "lab", &desired, true, JOB).unwrap();
        let phase = std::env::var("CAKE_TEST_MQTT_CRASH_PHASE").unwrap();
        if phase == "unnamed" {
            std::process::exit(77);
        }
        let directory = open_plan_root(&root).unwrap();
        link_complete_draft(
            selected.unnamed.as_ref().unwrap(),
            &directory,
            &selected.temporary,
        )
        .unwrap();
        if phase == "linked" {
            std::process::exit(77);
        }
        assert_eq!(phase, "renamed");
        fs::rename(&selected.temporary, root.join("lab.plan")).unwrap();
        directory.sync_all().unwrap();
        std::process::exit(77);
    }

    fn crash_child(root: &Path, phase: &str) {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "operations::mqtt_publisher::selected_plan::tests::r4_selected_mqtt_draft_crash_helper", "--test-threads=1"])
            .env("CAKE_TEST_MQTT_CRASH_ROOT", root)
            .env("CAKE_TEST_MQTT_CRASH_PHASE", phase)
            .output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(77),
            "crash fixture did not reach its cutpoint"
        );
    }

    #[test]
    fn r4_selected_mqtt_draft_recovers_process_exit_before_and_after_publication() {
        for phase in ["unnamed", "linked", "renamed"] {
            let parent = temp_root("crash-plan");
            fs::write(parent.join("crash-fixture"), b"mqtt-draft-crash-test-v1").unwrap();
            let root = parent.join("plans");
            let original = package();
            publish_service_plans(&original, &root).unwrap();
            let before = Plans::capture(&root, None).unwrap();
            crash_child(&root, phase);
            let temporary = root.join(format!(".selected_lab_{JOB}"));
            assert_eq!(temporary.exists(), phase == "linked");
            if phase == "linked" {
                assert!(publish_service_plans(&original, &root).is_err());
                assert!(cleanup_plan_root(&root).is_err());
                assert!(temporary.exists());
                for (name, file) in &before.files {
                    let path = plan_path(&root, name).unwrap();
                    assert!(FileIdentity::capture(&fs::metadata(&path).unwrap()) == file.identity);
                    assert_eq!(fs::read(&path).unwrap(), file.bytes);
                }
            }
            let mut recovered = SelectedPlan::prepare(&root, "lab", &original, true, JOB).unwrap();
            assert_eq!(recovered.needs_recovery(), phase == "linked");
            recovered.publish(|| Ok(())).unwrap();
            let after = Plans::capture(&root, None).unwrap();
            assert!(after.files["peer"] == before.files["peer"]);
            assert_eq!(after.files["lab"].bytes, before.files["lab"].bytes);
            if phase != "renamed" {
                assert!(after.files["lab"].identity == before.files["lab"].identity);
            }
            assert!(!temporary.exists());
            assert!(!recovered.needs_recovery());
            fs::remove_dir_all(parent).unwrap();
        }
    }

    #[test]
    fn r4_selected_mqtt_draft_refuses_other_jobs_tamper_and_nonprivate_files() {
        let parent = temp_root("crash-refusal");
        fs::write(parent.join("crash-fixture"), b"mqtt-draft-crash-test-v1").unwrap();
        let root = parent.join("plans");
        let original = package();
        publish_service_plans(&original, &root).unwrap();
        crash_child(&root, "linked");
        let path = root.join(format!(".selected_lab_{JOB}"));
        let bytes = fs::read(&path).unwrap();
        assert!(SelectedPlan::prepare(
            &root,
            "lab",
            &original,
            true,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        )
        .is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(SelectedPlan::prepare(&root, "lab", &original, true, JOB).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
        assert!(SelectedPlan::prepare(&root, "lab", &original, true, JOB).is_err());
        assert!(path.exists());
        fs::write(&path, fs::read(root.join("peer.plan")).unwrap()).unwrap();
        assert!(SelectedPlan::prepare(&root, "lab", &original, true, JOB).is_err());
        assert!(path.exists());
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn r4_selected_mqtt_unnamed_preflight_rejects_unsupported_storage_and_invalid_job() {
        let root = Path::new("/proc/cake-mqtt-unnamed-unsupported-fixture");
        assert!(!root.exists());
        assert!(SelectedPlan::prepare(root, "lab", &package(), true, JOB).is_err());
        assert!(!root.exists());
        for job in [
            "",
            "a",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "gggggggggggggggggggggggggggggggg",
        ] {
            assert!(SelectedPlan::prepare(root, "lab", &package(), true, job)
                .err()
                .unwrap()
                .contains("operation ID"));
        }
    }

    #[test]
    fn r4_selected_mqtt_plan_preserves_peers_and_unchanged_file_identity() {
        let root = temp_root("selected-peer");
        let mut desired = package();
        publish_service_plans(&desired, &root).unwrap();
        let before = Plans::capture(&root, None).unwrap();
        let mut same = SelectedPlan::prepare(&root, "lab", &desired, true, JOB).unwrap();
        assert!(same.enabled());
        assert!(!same.changed());
        same.publish(|| panic!("unchanged plan must not need publisher Stop"))
            .unwrap();
        assert!(before == Plans::capture(&root, None).unwrap());
        desired
            .sections
            .get_mut("lab")
            .unwrap()
            .options
            .insert("mqtt_min_interval_s".into(), "3".into());
        // Invalid unapplied peer UCI must not be consulted or published.
        desired
            .sections
            .get_mut("peer")
            .unwrap()
            .options
            .insert("mqtt_port".into(), "invalid".into());
        let mut selected = SelectedPlan::prepare(&root, "lab", &desired, true, JOB).unwrap();
        assert!(selected.changed());
        let mut proofs = 0;
        selected
            .publish(|| {
                proofs += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(proofs, 3);
        let after = Plans::capture(&root, None).unwrap();
        assert!(before.files["peer"] == after.files["peer"]);
        assert_ne!(
            before.files["lab"].identity.inode,
            after.files["lab"].identity.inode
        );
        assert_eq!(
            read_service_plan(&root, "lab").unwrap().min_interval,
            Duration::from_secs(3)
        );
        assert!(!selected.changed());
        selected.attest().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn r4_selected_mqtt_plan_stops_disabled_and_absent_without_touching_peers() {
        let root = temp_root("selected-disable");
        let mut desired = package();
        publish_service_plans(&desired, &root).unwrap();
        let before = Plans::capture(&root, None).unwrap();
        desired
            .sections
            .get_mut("lab")
            .unwrap()
            .options
            .insert("enabled".into(), "0".into());
        let mut stop = SelectedPlan::prepare(&root, "lab", &desired, false, JOB).unwrap();
        assert!(!stop.enabled());
        stop.publish(|| Ok(())).unwrap();
        assert!(!root.join("lab.plan").exists());
        assert!(before.files["peer"] == Plans::capture(&root, None).unwrap().files["peer"]);
        let mut absent =
            SelectedPlan::prepare(&root, "lab", &UciPackage::default(), false, JOB).unwrap();
        absent
            .publish(|| panic!("already absent needs no plan mutation"))
            .unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn r4_selected_mqtt_plan_refuses_drift_and_cleans_only_its_failed_draft() {
        let root = temp_root("selected-drift");
        let mut desired = package();
        publish_service_plans(&desired, &root).unwrap();
        desired
            .sections
            .get_mut("lab")
            .unwrap()
            .options
            .insert("mqtt_min_interval_s".into(), "3".into());
        let before = Plans::capture(&root, None).unwrap();
        for fail_at in [1, 2] {
            let mut selected = SelectedPlan::prepare(&root, "lab", &desired, true, JOB).unwrap();
            let mut proofs = 0;
            assert_eq!(
                selected
                    .publish(|| {
                        proofs += 1;
                        if proofs == fail_at {
                            Err("source-drift".into())
                        } else {
                            Ok(())
                        }
                    })
                    .unwrap_err(),
                "source-drift"
            );
            assert!(before == Plans::capture(&root, None).unwrap());
        }
        let mut selected = SelectedPlan::prepare(&root, "lab", &desired, true, JOB).unwrap();
        let peer = root.join("peer.plan");
        let peer_bytes = fs::read(&peer).unwrap();
        let mut proofs = 0;
        assert!(selected
            .publish(|| {
                proofs += 1;
                if proofs == 2 {
                    let replacement = root.join("replacement");
                    write_plan_file(&replacement, &peer_bytes)?;
                    fs::rename(replacement, &peer).unwrap();
                }
                Ok(())
            })
            .is_err());
        let after = Plans::capture(&root, None).unwrap();
        assert!(before.files["lab"] == after.files["lab"]);
        assert_ne!(
            before.files["peer"].identity.inode,
            after.files["peer"].identity.inode
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn r4_selected_mqtt_plan_initial_publication_and_post_write_error_are_explicit() {
        let parent = temp_root("selected-initial");
        let root = parent.join("plans");
        let desired = package();
        let mut selected = SelectedPlan::prepare(&root, "lab", &desired, true, JOB).unwrap();
        assert!(!root.exists(), "preflight must not create the root");
        let mut proofs = 0;
        assert_eq!(
            selected
                .publish(|| {
                    proofs += 1;
                    if proofs == 3 {
                        Err("source-drift-after-publication".into())
                    } else {
                        Ok(())
                    }
                })
                .unwrap_err(),
            "source-drift-after-publication"
        );
        assert!(root.join("lab.plan").exists());
        assert!(!root.join("peer.plan").exists());
        selected.attest().unwrap();
        let mut rollback =
            SelectedPlan::prepare(&root, "lab", &UciPackage::default(), false, JOB).unwrap();
        rollback.publish(|| Ok(())).unwrap();
        assert!(Plans::capture(&root, None).unwrap().files.is_empty());
        fs::write(root.join("foreign"), b"preserve").unwrap();
        assert!(SelectedPlan::prepare(&root, "lab", &desired, true, JOB).is_err());
        assert_eq!(fs::read(root.join("foreign")).unwrap(), b"preserve");
        fs::remove_dir_all(parent).unwrap();
    }
}
