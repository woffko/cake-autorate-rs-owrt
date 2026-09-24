//! Read-only committed UCI snapshots, independent of all original-name deltas.
//! No commit, revert, service/queue action, source-file lock, or raw-value error.
//! Callers must retain the snapshot and reattest it before runtime mutations.
use super::process::{run_bounded_command_output, SpawnSpec};
use super::runtime_health::UciPackage;
use super::uci_edits::{self, Edit};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(super) const PACKAGES: [&str; 2] = ["cake-autorate", "sqm"];
pub(super) const MAX_CONFIG: u64 = 1024 * 1024;
const MAX_ENTRIES: usize = 16;
const MAGIC: &[u8] = b"# cake-autorate committed snapshot v1\n";
type Result<T> = std::result::Result<T, String>;

fn io<T>(result: std::io::Result<T>) -> Result<T> {
    result.map_err(|_| "committed-uci-io".into())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Identity {
    pub(super) dev: u64,
    pub(super) ino: u64,
    pub(super) len: u64,
    pub(super) uid: u32,
    pub(super) gid: u32,
    pub(super) mode: u32,
    pub(super) mtime: i64,
    pub(super) mtime_ns: i64,
    pub(super) ctime: i64,
    pub(super) ctime_ns: i64,
}
impl Identity {
    fn of(meta: &fs::Metadata) -> Self {
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            uid: meta.uid(),
            gid: meta.gid(),
            mode: meta.mode(),
            mtime: meta.mtime(),
            mtime_ns: meta.mtime_nsec(),
            ctime: meta.ctime(),
            ctime_ns: meta.ctime_nsec(),
        }
    }
}

pub(super) fn file_identity(path: &Path, file: &File, private: bool) -> Result<Identity> {
    let named = io(fs::symlink_metadata(path))?;
    let opened = io(file.metadata())?;
    for meta in [&named, &opened] {
        if !meta.is_file()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & if private { 0o077 } else { 0o022 } != 0
            || (private && meta.nlink() != 1)
        {
            return Err("committed-uci-file-unsafe".into());
        }
    }
    if Identity::of(&named) != Identity::of(&opened) {
        return Err("committed-uci-file-replaced".into());
    }
    Ok(Identity::of(&opened))
}

pub(super) fn read_file(
    path: &Path,
    maximum: u64,
    private: bool,
) -> Result<(Vec<u8>, File, Identity)> {
    let file = io(OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path))?;
    let before = file_identity(path, &file, private)?;
    if before.len > maximum {
        return Err("committed-uci-file-too-large".into());
    }
    let mut bytes = Vec::new();
    io((&file).take(maximum + 1).read_to_end(&mut bytes))?;
    if bytes.len() as u64 > maximum || file_identity(path, &file, private)? != before {
        return Err("committed-uci-file-changed-during-read".into());
    }
    Ok((bytes, file, before))
}

pub(super) struct Directory {
    pub(super) path: PathBuf,
    pub(super) file: File,
    private: bool,
}
impl Directory {
    pub(super) fn open(path: &Path, private: bool, create: bool) -> Result<Self> {
        if create {
            match fs::DirBuilder::new().mode(0o700).create(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err("committed-uci-directory-create-failed".into()),
            }
        }
        let file = io(OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(path))?;
        let directory = Self {
            path: path.into(),
            file,
            private,
        };
        directory.attest()?;
        Ok(directory)
    }
    pub(super) fn attest(&self) -> Result<()> {
        let named = io(fs::symlink_metadata(&self.path))?;
        let opened = io(self.file.metadata())?;
        if !named.is_dir()
            || named.uid() != unsafe { libc::geteuid() }
            || named.mode() & if self.private { 0o077 } else { 0o022 } != 0
            || named.dev() != opened.dev()
            || named.ino() != opened.ino()
        {
            return Err("committed-uci-directory-unsafe-or-replaced".into());
        }
        Ok(())
    }
    pub(super) fn names(&self) -> Result<Vec<OsString>> {
        self.attest()?;
        let mut names = Vec::new();
        for entry in io(fs::read_dir(&self.path))? {
            if names.len() == MAX_ENTRIES {
                return Err("committed-uci-directory-entry-limit".into());
            }
            names.push(io(entry)?.file_name());
        }
        self.attest()?;
        Ok(names)
    }
}

struct Workspace {
    root: Directory,
    config: Directory,
    delta: Directory,
    overrides: Directory,
    lock: File,
}
impl Workspace {
    fn copy_and_query(
        &self,
        bytes: &[u8],
        query: &mut impl FnMut(Vec<OsString>) -> Result<Vec<u8>>,
    ) -> Result<(Copy, UciPackage, String)> {
        self.attest()?;
        if bytes.len() as u64 > MAX_CONFIG {
            return Err("committed-uci-file-too-large".into());
        }
        let alias = alias()?;
        let path = self.config.path.join(&alias);
        let mut file = io(OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path))?;
        io(file.write_all(MAGIC))?;
        io(file.write_all(bytes))?;
        io(file.sync_all())?;
        let (copied, _, identity) = read_file(&path, MAX_CONFIG + MAGIC.len() as u64, true)?;
        let copy = Copy {
            alias: alias.clone(),
            identity,
            digest: crate::config_candidate::digest(&copied),
        };
        let output = query(vec![
            "-c".into(),
            self.config.path.as_os_str().into(),
            "-C".into(),
            self.overrides.path.as_os_str().into(),
            "-t".into(),
            self.delta.path.as_os_str().into(),
            "-q".into(),
            "-X".into(),
            "show".into(),
            alias.clone().into(),
        ])?;
        if output.len() as u64 > MAX_CONFIG {
            return Err("committed-uci-query-too-large".into());
        }
        let text = String::from_utf8(output).map_err(|_| "committed-uci-query-not-text")?;
        let package =
            UciPackage::parse(&alias, &text).map_err(|_| "committed-uci-query-invalid")?;
        copy.attest(&self.config.path)?;
        self.attest()?;
        Ok((copy, package, text))
    }
    fn open(run_root: &Path) -> Result<Self> {
        io(fs::create_dir_all(run_root))?;
        let parent = Directory::open(run_root, false, false)?;
        let root = Directory::open(&run_root.join(".committed-uci"), true, true)?;
        let lock = io(OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(root.path.join(".lock")))?;
        if file_identity(&root.path.join(".lock"), &lock, true)?.len != 0 {
            return Err("committed-uci-lock-unsafe".into());
        }
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("committed-uci-workspace-busy".into());
        }
        let config = Directory::open(&root.path.join("config"), true, true)?;
        let delta = Directory::open(&root.path.join("delta"), true, true)?;
        let overrides = Directory::open(&root.path.join("overrides"), true, true)?;
        parent.attest()?;
        let workspace = Self {
            root,
            config,
            delta,
            overrides,
            lock,
        };
        workspace.cleanup_copies()?;
        workspace.attest()?;
        Ok(workspace)
    }
    fn attest(&self) -> Result<()> {
        self.root.attest()?;
        self.config.attest()?;
        file_identity(&self.root.path.join(".lock"), &self.lock, true)?;
        if !self.delta.names()?.is_empty() || !self.overrides.names()?.is_empty() {
            return Err("committed-uci-unexpected-delta-or-override".into());
        }
        Ok(())
    }
    fn cleanup_copies(&self) -> Result<()> {
        self.attest()?;
        for name in self.config.names()? {
            let Some(alias) = name.to_str().filter(|name| valid_alias(name)) else {
                continue;
            };
            let path = self.config.path.join(alias);
            let (bytes, file, _) = read_file(&path, MAX_CONFIG + MAGIC.len() as u64, true)?;
            // A crash may leave a partial header/body, but only these private
            // single-link reserved aliases with our comment marker are ours.
            if !bytes.starts_with(MAGIC) && !MAGIC.starts_with(&bytes) {
                return Err("committed-uci-unowned-copy".into());
            }
            self.attest()?;
            file_identity(&path, &file, true)?;
            io(fs::remove_file(path))?;
        }
        Ok(())
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = self.cleanup_copies();
    }
}

fn valid_alias(name: &str) -> bool {
    name.len() == 32
        && name.starts_with("cu")
        && name[2..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn alias() -> Result<String> {
    let mut bytes = [0u8; 15];
    io(io(File::open("/dev/urandom"))?.read_exact(&mut bytes))?;
    Ok(format!(
        "cu{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}

pub(super) struct Source {
    pub(super) bytes: Vec<u8>,
    pub(super) identity: Identity,
}
struct Copy {
    alias: String,
    identity: Identity,
    digest: String,
}
impl Copy {
    fn attest(&self, directory: &Path) -> Result<()> {
        let (bytes, _, identity) = read_file(
            &directory.join(&self.alias),
            MAX_CONFIG + MAGIC.len() as u64,
            true,
        )?;
        if identity != self.identity || crate::config_candidate::digest(&bytes) != self.digest {
            return Err("committed-uci-copy-changed".into());
        }
        Ok(())
    }
}

/// Fully native-checked candidate bytes, still tied to the ORIGINAL committed
/// identity. Preparation never publishes, commits, reverts, or adopts drift.
pub(crate) struct PreparedConfig {
    pub(super) original: CommittedSnapshot,
    copies: BTreeMap<&'static str, Copy>,
    bytes: BTreeMap<&'static str, Vec<u8>>,
    packages: BTreeMap<&'static str, UciPackage>,
    shows: BTreeMap<&'static str, String>,
}
impl PreparedConfig {
    pub(crate) fn package(&self, name: &str) -> Result<&UciPackage> {
        self.packages
            .get(name)
            .ok_or_else(|| "committed-uci-package-not-captured".into())
    }
    pub(crate) fn candidate_bytes(&self, name: &str) -> Result<&[u8]> {
        self.bytes
            .get(name)
            .map(Vec::as_slice)
            .ok_or_else(|| "committed-uci-package-not-captured".into())
    }
    pub(crate) fn original_bytes(&self, name: &str) -> Result<&[u8]> {
        self.original.original_bytes(name)
    }
    pub(crate) fn package_show(&self, package: &str) -> Result<Vec<u8>> {
        self.package(package)?;
        let prefix = format!("{}.", self.copies[package].alias);
        let mut result = String::new();
        for line in self.shows[package].lines() {
            let (key, value) = line.split_once('=').ok_or("committed-uci-query-invalid")?;
            result.push_str(package);
            result.push('.');
            result.push_str(
                key.strip_prefix(&prefix)
                    .ok_or("committed-uci-query-invalid")?,
            );
            result.push('=');
            result.push_str(value);
            result.push('\n');
        }
        Ok(result.into_bytes())
    }
    pub(crate) fn section_show(&self, package: &str, section: &str) -> Result<Vec<u8>> {
        if !self.package(package)?.sections.contains_key(section) {
            return Err("committed-uci-section-not-captured".into());
        }
        section_show(
            package,
            section,
            &self.copies[package].alias,
            &self.shows[package],
        )
    }
    pub(crate) fn attest(&self) -> Result<()> {
        self.original.attest()?;
        self.attest_private()
    }
    pub(super) fn attest_private(&self) -> Result<()> {
        self.original.attest_private()?;
        for copy in self.copies.values() {
            copy.attest(&self.original.workspace.config.path)?;
        }
        Ok(())
    }
    /// Only the scoped runner adapter may use this alias for config_load; an
    /// ordinary `config_load sqm` is deliberately unsupported in this directory.
    pub(super) fn sqm_runner_alias(&self) -> Result<(&Path, &str)> {
        self.attest_private()?;
        Ok((
            &self.original.workspace.config.path,
            &self.copies["sqm"].alias,
        ))
    }
    pub(super) fn retain_alias_lease(&self) -> Result<File> {
        self.attest_private()?;
        io(self.original.workspace.lock.try_clone())
    }
}

fn section_show(package: &str, section: &str, alias: &str, show: &str) -> Result<Vec<u8>> {
    let section_key = format!("{alias}.{section}");
    let option_prefix = format!("{section_key}.");
    let mut output = String::new();
    for line in show.lines() {
        let (key, value) = line.split_once('=').ok_or("committed-uci-query-invalid")?;
        if key == section_key || key.starts_with(&option_prefix) {
            output.push_str(package);
            output.push_str(
                key.strip_prefix(alias)
                    .ok_or("committed-uci-query-invalid")?,
            );
            output.push('=');
            output.push_str(value);
            output.push('\n');
        }
    }
    Ok(output.into_bytes())
}

/// Shared cold runner boundary. Neither implementation grants authority to
/// publish public files; callers must separately attest their source/input.
pub(super) trait SqmAliasConfig {
    fn sqm_runner_alias(&self) -> Result<(&Path, &str)>;
    fn retain_alias_lease(&self) -> Result<File>;
}
impl SqmAliasConfig for PreparedConfig {
    fn sqm_runner_alias(&self) -> Result<(&Path, &str)> {
        PreparedConfig::sqm_runner_alias(self)
    }
    fn retain_alias_lease(&self) -> Result<File> {
        PreparedConfig::retain_alias_lease(self)
    }
}

/// A single frozen runtime recipe, not a new snapshot of current public UCI.
/// Uses the same private alias/override/delta isolation and inherited lease as
/// ordinary Start; native parsing verifies the rendered owned queue only.
pub(super) struct FrozenSqmAlias {
    workspace: Workspace,
    copy: Copy,
    show: String,
}
impl FrozenSqmAlias {
    pub(super) fn materialize(bytes: &[u8], runtime_root: &Path, uci: &Path) -> Result<Self> {
        let workspace = Workspace::open(runtime_root)?;
        let command = SpawnSpec {
            program: uci.into(),
            arguments: vec![],
            environment: vec![],
        };
        let (copy, _, show) = workspace
            .copy_and_query(bytes, &mut |args| CommittedSnapshot::query(&command, args))?;
        let config = Self {
            workspace,
            copy,
            show,
        };
        config.sqm_runner_alias()?;
        Ok(config)
    }
    pub(super) fn section_show(&self, section: &str) -> Result<Vec<u8>> {
        self.sqm_runner_alias()?;
        section_show("sqm", section, &self.copy.alias, &self.show)
    }
}
impl SqmAliasConfig for FrozenSqmAlias {
    fn sqm_runner_alias(&self) -> Result<(&Path, &str)> {
        self.workspace.attest()?;
        self.copy.attest(&self.workspace.config.path)?;
        Ok((&self.workspace.config.path, &self.copy.alias))
    }
    fn retain_alias_lease(&self) -> Result<File> {
        self.sqm_runner_alias()?;
        io(self.workspace.lock.try_clone())
    }
}

pub(crate) struct CommittedSnapshot {
    pub(super) directory: Directory,
    workspace: Workspace,
    pub(super) sources: BTreeMap<&'static str, Source>,
    copies: BTreeMap<&'static str, Copy>,
    packages: BTreeMap<&'static str, UciPackage>,
    shows: BTreeMap<&'static str, String>,
}
impl CommittedSnapshot {
    /// Parse verified journal versions in private aliases, not today's public
    /// files. This is read-only interpretation, never publication authority.
    pub(crate) fn parse_versions(
        bytes: &[Vec<u8>; 2],
        run_root: &Path,
        uci: &Path,
    ) -> Result<[UciPackage; 2]> {
        let workspace = Workspace::open(run_root)?;
        Self::parse_versions_in(&workspace, bytes, uci)
    }
    pub(crate) fn parse_retained_versions(
        &self,
        bytes: &[Vec<u8>; 2],
        uci: &Path,
    ) -> Result<[UciPackage; 2]> {
        self.attest()?;
        let packages = Self::parse_versions_in(&self.workspace, bytes, uci)?;
        self.attest()?;
        Ok(packages)
    }
    fn parse_versions_in(
        workspace: &Workspace,
        bytes: &[Vec<u8>; 2],
        uci: &Path,
    ) -> Result<[UciPackage; 2]> {
        let command = SpawnSpec {
            program: uci.into(),
            arguments: vec![],
            environment: vec![],
        };
        let mut packages = [UciPackage::default(), UciPackage::default()];
        for i in 0..2 {
            let (copy, package, show) =
                workspace.copy_and_query(&bytes[i], &mut |args| Self::query(&command, args))?;
            // The general scalar package view keeps the first list value.
            // Recovery must not silently lose an owner/physical target that
            // was encoded as several values. Foreign non-owned lists remain
            // permitted, as do normal controller reflector lists.
            let prefix = format!("{}.", copy.alias);
            for line in show.lines() {
                let (key, raw) = line.split_once('=').ok_or("committed-uci-query-invalid")?;
                let Some((section, option)) = key
                    .strip_prefix(&prefix)
                    .and_then(|key| key.split_once('.'))
                else {
                    continue;
                };
                let section = package
                    .sections
                    .get(section)
                    .ok_or("committed-uci-query-invalid")?;
                let scalar = if i == 1 {
                    option == "_cake_autorate_managed"
                        || section
                            .options
                            .get("_cake_autorate_managed")
                            .is_some_and(|owner| !owner.is_empty())
                } else {
                    section.section_type == "cake_autorate"
                        && matches!(
                            option,
                            "dl_if"
                                | "ul_if"
                                | "sqm_interface"
                                | "sqm_section"
                                | "sqm_direction_mode"
                        )
                };
                if scalar && crate::parse_uci_values(raw).len() != 1 {
                    return Err("committed-uci-recovery-nonscalar-ownership".into());
                }
            }
            packages[i] = package;
        }
        workspace.attest()?;
        Ok(packages)
    }
    #[cfg(test)]
    pub(crate) fn capture_fixture(
        config_dir: &Path,
        run_root: &Path,
        query: impl FnMut(Vec<OsString>) -> Result<Vec<u8>>,
    ) -> Result<Self> {
        Self::capture_with(config_dir, run_root, query)
    }
    pub(crate) fn capture(config_dir: &Path, run_root: &Path, uci: &Path) -> Result<Self> {
        let command = SpawnSpec {
            program: uci.into(),
            arguments: Vec::new(),
            environment: Vec::new(),
        };
        Self::capture_with(config_dir, run_root, |arguments| {
            Self::query(&command, arguments)
        })
    }
    fn query(command: &SpawnSpec, arguments: Vec<OsString>) -> Result<Vec<u8>> {
        let mut spec = command.clone();
        spec.arguments.extend(arguments);
        let output =
            run_bounded_command_output(&spec, Duration::from_secs(10), MAX_CONFIG as usize, || {
                false
            })
            .map_err(|_| "committed-uci-query-failed")?;
        if !output.status.success() || !output.stderr.is_empty() {
            return Err("committed-uci-query-rejected".into());
        }
        Ok(output.stdout)
    }
    fn capture_with(
        config_dir: &Path,
        run_root: &Path,
        mut query: impl FnMut(Vec<OsString>) -> Result<Vec<u8>>,
    ) -> Result<Self> {
        let directory = Directory::open(config_dir, false, false)?;
        let mut sources = BTreeMap::new();
        for name in PACKAGES {
            let (bytes, _, identity) = read_file(&config_dir.join(name), MAX_CONFIG, false)?;
            sources.insert(name, Source { bytes, identity });
        }
        let workspace = Workspace::open(run_root)?;
        let mut snapshot = Self {
            directory,
            workspace,
            sources,
            copies: BTreeMap::new(),
            packages: BTreeMap::new(),
            shows: BTreeMap::new(),
        };
        for name in PACKAGES {
            let (copy, package, text) =
                snapshot.copy_and_query(&snapshot.sources[name].bytes, &mut query)?;
            snapshot.copies.insert(name, copy);
            snapshot.packages.insert(name, package);
            snapshot.shows.insert(name, text);
            snapshot.attest()?;
        }
        Ok(snapshot)
    }
    fn copy_and_query(
        &self,
        bytes: &[u8],
        query: &mut impl FnMut(Vec<OsString>) -> Result<Vec<u8>>,
    ) -> Result<(Copy, UciPackage, String)> {
        self.attest()?;
        let copied = self.workspace.copy_and_query(bytes, query)?;
        self.attest()?;
        Ok(copied)
    }
    pub(crate) fn prepare(self, edits: [&[Edit]; 2], uci: &Path) -> Result<PreparedConfig> {
        let command = SpawnSpec {
            program: uci.into(),
            arguments: vec![],
            environment: vec![],
        };
        self.prepare_with(edits, |arguments| Self::query(&command, arguments))
    }
    #[cfg(test)]
    pub(super) fn prepare_fixture(
        self,
        edits: [&[Edit]; 2],
        query: impl FnMut(Vec<OsString>) -> Result<Vec<u8>>,
    ) -> Result<PreparedConfig> {
        self.prepare_with(edits, query)
    }
    fn prepare_with(
        self,
        edits: [&[Edit]; 2],
        mut query: impl FnMut(Vec<OsString>) -> Result<Vec<u8>>,
    ) -> Result<PreparedConfig> {
        let mut prepared = PreparedConfig {
            original: self,
            copies: BTreeMap::new(),
            bytes: BTreeMap::new(),
            packages: BTreeMap::new(),
            shows: BTreeMap::new(),
        };
        // Render/validate both edit sets before any native candidate query.
        let mut expected = BTreeMap::new();
        for (name, edits) in PACKAGES.into_iter().zip(edits) {
            let original = prepared.original.original_bytes(name)?;
            let bytes = uci_edits::render(original, edits)?;
            let view = uci_edits::show_view(
                original,
                &prepared.original.copies[name].alias,
                &prepared.original.shows[name],
            )?;
            expected.insert(name, uci_edits::expected_view(view, edits)?);
            prepared.bytes.insert(name, bytes);
        }
        for name in PACKAGES {
            prepared.attest()?;
            let (copy, package, show) = prepared
                .original
                .copy_and_query(&prepared.bytes[name], &mut query)?;
            let actual = uci_edits::show_view(&prepared.bytes[name], &copy.alias, &show)?;
            if actual != expected[name] {
                return Err("committed-uci-candidate-semantic-mismatch".into());
            }
            prepared.copies.insert(name, copy);
            prepared.packages.insert(name, package);
            prepared.shows.insert(name, show);
            prepared.attest()?;
        }
        Ok(prepared)
    }
    pub(crate) fn package(&self, name: &str) -> Result<&UciPackage> {
        self.packages
            .get(name)
            .ok_or_else(|| "committed-uci-package-not-captured".into())
    }
    pub(crate) fn original_bytes(&self, name: &str) -> Result<&[u8]> {
        self.sources
            .get(name)
            .map(|source| source.bytes.as_slice())
            .ok_or_else(|| "committed-uci-package-not-captured".into())
    }
    /// Preserve every scalar/list item for stricter downstream authority parsers.
    /// Rewrite only the known package key prefix, never content inside values.
    pub(crate) fn section_show(&self, package: &str, section: &str) -> Result<Vec<u8>> {
        if !self.package(package)?.sections.contains_key(section) {
            return Err("committed-uci-section-not-captured".into());
        }
        section_show(
            package,
            section,
            &self.copies[package].alias,
            &self.shows[package],
        )
    }
    /// The inspected SQM stop runner reads only target runtime state, not UCI.
    /// This alias-only directory is deliberately unsuitable for `config_load sqm`.
    pub(crate) fn stop_context_directory(&self) -> &Path {
        &self.workspace.config.path
    }
    pub(crate) fn attest(&self) -> Result<()> {
        self.attest_private()?;
        for (name, source) in &self.sources {
            let (bytes, _, identity) =
                read_file(&self.directory.path.join(name), MAX_CONFIG, false)?;
            if identity != source.identity || bytes != source.bytes {
                return Err("committed-uci-source-changed".into());
            }
        }
        Ok(())
    }
    fn attest_private(&self) -> Result<()> {
        self.directory.attest()?;
        self.workspace.attest()?;
        for copy in self.copies.values() {
            copy.attest(&self.workspace.config.path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    struct Fixture {
        root: PathBuf,
        config: PathBuf,
        run: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("cake-r4-committed-{}", alias().unwrap()));
            fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            let config = root.join("source");
            fs::DirBuilder::new().mode(0o700).create(&config).unwrap();
            for (package, kind) in [("cake-autorate", "cake_autorate"), ("sqm", "queue")] {
                fs::write(
                    config.join(package),
                    format!("config {kind} 'lab'\n option marker 'committed-{package}'\n"),
                )
                .unwrap();
            }
            Self {
                run: root.join("run"),
                root,
                config,
            }
        }
        fn capture(&self) -> Result<CommittedSnapshot> {
            CommittedSnapshot::capture_with(&self.config, &self.run, fake_query)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn fake_query(arguments: Vec<OsString>) -> Result<Vec<u8>> {
        assert_eq!(arguments.len(), 10);
        assert_eq!(arguments[0], "-c");
        assert_eq!(arguments[2], "-C");
        assert_eq!(arguments[4], "-t");
        assert_eq!(arguments[6..9], ["-q", "-X", "show"]);
        let alias = arguments[9].to_str().unwrap();
        assert!(valid_alias(alias));
        assert!(!PACKAGES.contains(&alias));
        let text = fs::read(Path::new(&arguments[1]).join(alias)).unwrap();
        assert!(text.starts_with(MAGIC));
        let is_cake = text
            .windows(b"cake_autorate".len())
            .any(|s| s == b"cake_autorate");
        let kind = if is_cake { "cake_autorate" } else { "queue" };
        let package = if is_cake { "cake-autorate" } else { "sqm" };
        Ok(format!("{alias}.lab={kind}\n{alias}.lab.marker='committed-{package}'\n").into_bytes())
    }

    fn sdk_command() -> SpawnSpec {
        let uci = std::env::var_os("CAKE_TEST_UCI").expect("explicit CAKE_TEST_UCI required");
        let loader = std::env::var_os("CAKE_TEST_MUSL_LOADER");
        let mut spec = SpawnSpec {
            program: loader.clone().unwrap_or_else(|| uci.clone()).into(),
            arguments: vec![],
            environment: vec![],
        };
        if loader.is_some() {
            spec.arguments.extend([
                "--library-path".into(),
                std::env::var_os("CAKE_TEST_LIB_DIR").expect("explicit library path required"),
                uci,
            ]);
        }
        spec
    }

    #[test]
    fn r4_prepared_config_preserves_sources_and_retains_both_attested_views() {
        let fixture = Fixture::new();
        let snapshot = fixture.capture().unwrap();
        let original = snapshot.original_bytes("sqm").unwrap().to_vec();
        let edits = [Edit::Set {
            section: "lab".into(),
            option: "marker".into(),
            value: "candidate".into(),
        }];
        let prepared = snapshot
            .prepare_with([&[], &edits], |args| {
                let bytes = fs::read(Path::new(&args[1]).join(&args[9])).unwrap();
                let show = String::from_utf8(fake_query(args)?).unwrap();
                Ok(
                    if bytes
                        .windows(b"'candidate'".len())
                        .any(|b| b == b"'candidate'")
                    {
                        show.replace("'committed-sqm'", "'candidate'").into_bytes()
                    } else {
                        show.into_bytes()
                    },
                )
            })
            .unwrap();
        prepared.attest().unwrap();
        assert_eq!(
            prepared.package("sqm").unwrap().sections["lab"].options["marker"],
            "candidate"
        );
        assert_eq!(prepared.original_bytes("sqm").unwrap(), original);
        assert_eq!(fs::read(fixture.config.join("sqm")).unwrap(), original);
        assert_ne!(prepared.candidate_bytes("sqm").unwrap(), original);
        assert_eq!(
            prepared.section_show("sqm", "lab").unwrap(),
            b"sqm.lab=queue\nsqm.lab.marker='candidate'\n"
        );
        assert!(prepared.candidate_bytes("network").is_err());
        assert!(prepared.section_show("sqm", "lab.marker").is_err());
        assert_eq!(prepared.original.workspace.config.names().unwrap().len(), 4);
        drop(prepared);
        assert_eq!(
            fs::read_dir(fixture.run.join(".committed-uci/config"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn r4_prepared_config_rejects_source_copy_and_delta_drift_during_native_validation() {
        for case in 0..4 {
            let fixture = Fixture::new();
            let snapshot = fixture.capture().unwrap();
            let original_copy = snapshot
                .workspace
                .config
                .path
                .join(&snapshot.copies["sqm"].alias);
            let delta = snapshot.workspace.delta.path.join("foreign");
            let result = snapshot.prepare_with([&[], &[]], |args| {
                let output = fake_query(args.clone())?;
                let path = match case {
                    0 => fixture.config.join("sqm"),
                    1 => original_copy.clone(),
                    2 => Path::new(&args[1]).join(&args[9]),
                    _ => delta.clone(),
                };
                let mut file = OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(path)
                    .unwrap();
                file.write_all(b"# concurrent fixture change\n").unwrap();
                Ok(output)
            });
            assert!(result.is_err(), "drift case {case} accepted");
            if case == 0 {
                assert!(fs::read(fixture.config.join("sqm"))
                    .unwrap()
                    .ends_with(b"# concurrent fixture change\n"));
            }
            if case == 3 {
                assert_eq!(fs::read(delta).unwrap(), b"# concurrent fixture change\n");
            }
        }
    }

    #[test]
    fn r4_prepared_config_rejects_later_list_item_drift_that_scalar_package_view_cannot_detect() {
        let fixture = Fixture::new();
        let snapshot = CommittedSnapshot::capture_with(&fixture.config, &fixture.run, |args| {
            let mut output = fake_query(args.clone())?;
            output.extend(
                format!(
                    "{}.lab.tags='first' 'retained'\n",
                    args[9].to_str().unwrap()
                )
                .as_bytes(),
            );
            Ok(output)
        })
        .unwrap();
        assert_eq!(
            snapshot.package("sqm").unwrap().sections["lab"].options["tags"],
            "first"
        );
        let error = snapshot
            .prepare_with([&[], &[]], |args| {
                let mut output = fake_query(args.clone())?;
                output.extend(
                    format!(
                        "{}.lab.tags='first' 'unexpected-private-fixture'\n",
                        args[9].to_str().unwrap()
                    )
                    .as_bytes(),
                );
                Ok(output)
            })
            .err()
            .unwrap();
        assert_eq!(error, "committed-uci-candidate-semantic-mismatch");
        assert!(!error.contains("private"));
    }

    #[test]
    fn r4_prepared_config_rejects_drift_after_success_instead_of_recapturing_baseline() {
        for source in [false, true] {
            let fixture = Fixture::new();
            let prepared = fixture
                .capture()
                .unwrap()
                .prepare_with([&[], &[]], fake_query)
                .unwrap();
            let path = if source {
                fixture.config.join("cake-autorate")
            } else {
                prepared
                    .original
                    .workspace
                    .config
                    .path
                    .join(&prepared.copies["cake-autorate"].alias)
            };
            OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(b"# changed\n")
                .unwrap();
            assert!(prepared.attest().is_err());
            if source {
                assert!(fs::read(path).unwrap().ends_with(b"# changed\n"));
            }
        }
    }

    #[test]
    #[ignore = "requires explicit inspected SDK UCI and loader paths"]
    fn r4_prepared_config_real_uci_preserves_anonymous_foreign_sections_and_all_pending_bytes() {
        let fixture = Fixture::new();
        let foreign = b"# untouched foreign anonymous queue\nconfig queue\n option interface 'foreign0'\n list tags 'one'\n list tags 'two'\nconfig queue 'foreign'\n option note 'x'\\''y'\n";
        OpenOptions::new()
            .append(true)
            .open(fixture.config.join("sqm"))
            .unwrap()
            .write_all(foreign)
            .unwrap();
        let pending = fixture.root.join("pending");
        fs::create_dir(&pending).unwrap();
        for name in PACKAGES {
            fs::write(
                pending.join(name),
                format!("{name}.lab.marker='pending-unchanged'\n"),
            )
            .unwrap();
        }
        let mut spec = sdk_command();
        spec.arguments
            .extend(["-p".into(), pending.as_os_str().into()]);
        let snapshot = CommittedSnapshot::capture_with(&fixture.config, &fixture.run, |args| {
            CommittedSnapshot::query(&spec, args)
        })
        .unwrap();
        let original_sqm = snapshot.original_bytes("sqm").unwrap().to_vec();
        let cake = [Edit::Set {
            section: "lab".into(),
            option: "marker".into(),
            value: "changed'quote".into(),
        }];
        let sqm = [
            Edit::DeleteSection {
                section: "lab".into(),
            },
            Edit::AddSection {
                section: "added".into(),
                kind: "queue".into(),
            },
            Edit::Set {
                section: "added".into(),
                option: "rate".into(),
                value: "20".into(),
            },
            Edit::Set {
                section: "added".into(),
                option: "empty".into(),
                value: "".into(),
            },
        ];
        let prepared = snapshot
            .prepare_with([&cake, &sqm], |args| CommittedSnapshot::query(&spec, args))
            .unwrap();
        assert_eq!(
            prepared.package("cake-autorate").unwrap().sections["lab"].options["marker"],
            "changed'quote"
        );
        assert_eq!(
            prepared.package("sqm").unwrap().sections["added"].options["rate"],
            "20"
        );
        assert!(!prepared.package("sqm").unwrap().sections["added"]
            .options
            .contains_key("empty"));
        assert_eq!(
            prepared.package("sqm").unwrap().sections["foreign"].options["note"],
            "x'y"
        );
        assert!(prepared
            .candidate_bytes("sqm")
            .unwrap()
            .windows(foreign.len())
            .any(|bytes| bytes == foreign));
        assert_eq!(fs::read(fixture.config.join("sqm")).unwrap(), original_sqm);
        for name in PACKAGES {
            assert_eq!(
                fs::read(fixture.config.join(name)).unwrap(),
                prepared.original_bytes(name).unwrap()
            );
            assert_eq!(
                fs::read(pending.join(name)).unwrap(),
                format!("{name}.lab.marker='pending-unchanged'\n").as_bytes()
            );
        }
        prepared.attest().unwrap();
    }

    #[test]
    fn r4_committed_snapshot_uses_private_random_aliases_without_source_writes_or_locks() {
        let fixture = Fixture::new();
        let before = fs::read(fixture.config.join("cake-autorate")).unwrap();
        let snapshot = CommittedSnapshot::capture_with(&fixture.config, &fixture.run, |args| {
            let source = File::open(fixture.config.join("cake-autorate")).unwrap();
            assert_eq!(
                unsafe { libc::flock(source.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
                0,
                "source inode locks must not be held around libuci"
            );
            fake_query(args)
        })
        .unwrap();
        assert_eq!(
            snapshot.package("cake-autorate").unwrap().sections["lab"].options["marker"],
            "committed-cake-autorate"
        );
        assert_eq!(snapshot.original_bytes("cake-autorate").unwrap(), before);
        assert!(snapshot.package("../foreign").is_err());
        assert!(snapshot.original_bytes("network").is_err());
        assert_eq!(snapshot.workspace.config.names().unwrap().len(), 2);
        snapshot.attest().unwrap();
        drop(snapshot);
        assert_eq!(
            fs::read(fixture.config.join("cake-autorate")).unwrap(),
            before
        );
        assert_eq!(
            fs::read_dir(fixture.run.join(".committed-uci/config"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn r4_committed_snapshot_rejects_busy_workspace_and_preserves_current_copy() {
        let fixture = Fixture::new();
        let snapshot = fixture.capture().unwrap();
        assert_eq!(
            fixture.capture().err().unwrap(),
            "committed-uci-workspace-busy"
        );
        snapshot.attest().unwrap();
    }

    #[test]
    fn r4_committed_snapshot_section_output_retains_lists_and_only_rewrites_keys() {
        let fixture = Fixture::new();
        let snapshot = CommittedSnapshot::capture_with(&fixture.config, &fixture.run, |args| {
            let mut output = fake_query(args.clone())?;
            let alias = args.last().unwrap().to_str().unwrap();
            output.extend(format!("{alias}.lab.items='one' 'two'\n{alias}.lab.note='{alias}.lab.inside-value'\n{alias}.lab_extra=queue\n").as_bytes());
            Ok(output)
        }).unwrap();
        let output = String::from_utf8(snapshot.section_show("sqm", "lab").unwrap()).unwrap();
        assert!(output.contains("sqm.lab.items='one' 'two'\n"));
        assert!(output.contains(&format!(
            "sqm.lab.note='{}.lab.inside-value'",
            snapshot.copies["sqm"].alias
        )));
        assert!(!output.contains("lab_extra="));
        assert!(snapshot.section_show("sqm", "lab.items").is_err());
    }

    #[test]
    fn r4_committed_snapshot_detects_same_bytes_new_inode_and_in_place_changes() {
        for replace in [false, true] {
            let fixture = Fixture::new();
            let snapshot = fixture.capture().unwrap();
            let path = fixture.config.join("cake-autorate");
            if replace {
                let replacement = fixture.config.join("replacement");
                fs::write(&replacement, fs::read(&path).unwrap()).unwrap();
                fs::rename(replacement, &path).unwrap();
            } else {
                fs::write(&path, b"config cake_autorate 'changed'\n").unwrap();
            }
            assert_eq!(
                snapshot.attest().err().unwrap(),
                "committed-uci-source-changed"
            );
        }
    }

    #[test]
    fn r4_committed_snapshot_rejects_source_and_private_copy_drift_during_query() {
        for source in [false, true] {
            let fixture = Fixture::new();
            let result = CommittedSnapshot::capture_with(&fixture.config, &fixture.run, |args| {
                let output = fake_query(args.clone())?;
                let path = if source {
                    fixture.config.join("sqm")
                } else {
                    Path::new(&args[1]).join(&args[9])
                };
                let mut file = OpenOptions::new().append(true).open(path).unwrap();
                file.write_all(b"# concurrent change\n").unwrap();
                Ok(output)
            });
            assert!(result.is_err());
        }
    }

    #[test]
    fn r4_committed_snapshot_rejects_unsafe_sources_and_output_without_raw_error_leaks() {
        for case in 0..4 {
            let fixture = Fixture::new();
            let path = fixture.config.join("cake-autorate");
            match case {
                0 => {
                    fs::remove_file(&path).unwrap();
                    symlink(fixture.config.join("sqm"), &path).unwrap();
                }
                1 => fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap(),
                2 => File::create(&path)
                    .unwrap()
                    .set_len(MAX_CONFIG + 1)
                    .unwrap(),
                _ => {}
            }
            let result = CommittedSnapshot::capture_with(&fixture.config, &fixture.run, |_| {
                if case != 3 {
                    panic!("unsafe source must fail before query");
                }
                Ok(b"private-fixture-not-uci".to_vec())
            });
            let error = result.err().unwrap();
            assert!(!error.contains("private-fixture"));
        }
    }

    #[test]
    fn r4_committed_snapshot_cleanup_preserves_foreign_files_and_rejects_delta_injection() {
        let fixture = Fixture::new();
        let snapshot = fixture.capture().unwrap();
        let foreign = snapshot.workspace.config.path.join("foreign-note");
        fs::write(&foreign, b"foreign bytes").unwrap();
        drop(snapshot);
        assert_eq!(fs::read(&foreign).unwrap(), b"foreign bytes");
        let snapshot = fixture.capture().unwrap();
        let delta = snapshot.workspace.delta.path.join("foreign-delta");
        fs::write(&delta, b"foreign pending bytes").unwrap();
        assert_eq!(
            snapshot.attest().err().unwrap(),
            "committed-uci-unexpected-delta-or-override"
        );
        drop(snapshot);
        assert_eq!(fs::read(&delta).unwrap(), b"foreign pending bytes");
        assert_eq!(
            fixture.capture().err().unwrap(),
            "committed-uci-unexpected-delta-or-override"
        );
    }

    #[test]
    fn r4_committed_snapshot_recovers_owned_crash_prefix_but_never_unlinks_foreign_alias() {
        let fixture = Fixture::new();
        drop(fixture.capture().unwrap());
        let config = fixture.run.join(".committed-uci/config");
        let path = config.join(alias().unwrap());
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.write_all(&MAGIC[..10]).unwrap();
        drop(fixture.capture().unwrap());
        assert!(!path.exists());
        fs::write(&path, b"foreign bytes").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            fixture.capture().err().unwrap(),
            "committed-uci-unowned-copy"
        );
        assert_eq!(fs::read(&path).unwrap(), b"foreign bytes");
    }

    #[test]
    fn r4_committed_snapshot_directory_replacement_and_copy_symlink_are_not_followed() {
        let fixture = Fixture::new();
        let snapshot = fixture.capture().unwrap();
        let copy = snapshot
            .workspace
            .config
            .path
            .join(&snapshot.copies["cake-autorate"].alias);
        fs::remove_file(&copy).unwrap();
        symlink(fixture.config.join("cake-autorate"), &copy).unwrap();
        assert!(snapshot.attest().is_err());
        drop(snapshot);
        assert!(fs::symlink_metadata(&copy)
            .unwrap()
            .file_type()
            .is_symlink());
        fs::remove_file(copy).unwrap();
        let snapshot = fixture.capture().unwrap();
        fs::rename(&fixture.config, fixture.root.join("old-source")).unwrap();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&fixture.config)
            .unwrap();
        assert_eq!(
            snapshot.attest().err().unwrap(),
            "committed-uci-directory-unsafe-or-replaced"
        );
    }

    #[test]
    fn r4_committed_snapshot_never_unlinks_hardlinked_private_copies() {
        let fixture = Fixture::new();
        let snapshot = fixture.capture().unwrap();
        let copy = snapshot
            .workspace
            .config
            .path
            .join(&snapshot.copies["sqm"].alias);
        let other = fixture.root.join("foreign-link");
        fs::hard_link(&copy, &other).unwrap();
        let bytes = fs::read(&other).unwrap();
        assert!(snapshot.attest().is_err());
        drop(snapshot);
        assert_eq!(fs::read(&other).unwrap(), bytes);
        assert_eq!(fs::read(&copy).unwrap(), bytes);
    }

    #[test]
    fn r4_committed_snapshot_bounds_directory_scan_and_query_output() {
        let fixture = Fixture::new();
        let result = CommittedSnapshot::capture_with(&fixture.config, &fixture.run, |_| {
            Ok(vec![b'x'; MAX_CONFIG as usize + 1])
        });
        assert_eq!(result.err().unwrap(), "committed-uci-query-too-large");
        let config = fixture.run.join(".committed-uci/config");
        assert_eq!(fs::read_dir(&config).unwrap().count(), 0);
        for index in 0..=MAX_ENTRIES {
            fs::write(config.join(format!("foreign-{index}")), b"unchanged").unwrap();
        }
        assert_eq!(
            fixture.capture().err().unwrap(),
            "committed-uci-directory-entry-limit"
        );
        assert_eq!(fs::read_dir(&config).unwrap().count(), MAX_ENTRIES + 1);
    }

    #[test]
    #[ignore = "requires explicit inspected SDK UCI and loader paths"]
    fn r4_committed_snapshot_real_uci_ignores_original_package_delta_paths() {
        let fixture = Fixture::new();
        let global = fixture.root.join("global");
        let rpcd = fixture.root.join("rpcd");
        fs::create_dir(&global).unwrap();
        fs::create_dir(&rpcd).unwrap();
        // Valid libuci delta format, previously verified by the actual CLI
        // context probe. No setup/read of the host's real package delta files.
        for dir in [&global, &rpcd] {
            fs::write(
                dir.join("cake-autorate"),
                "cake-autorate.lab.marker='foreign-cake'\n",
            )
            .unwrap();
            fs::write(dir.join("sqm"), "sqm.lab.marker='foreign-sqm'\n").unwrap();
        }
        let uci = std::env::var_os("CAKE_TEST_UCI").expect("explicit CAKE_TEST_UCI required");
        let loader = std::env::var_os("CAKE_TEST_MUSL_LOADER");
        let mut spec = SpawnSpec {
            program: loader.clone().unwrap_or_else(|| uci.clone()).into(),
            arguments: vec![],
            environment: vec![],
        };
        if loader.is_some() {
            spec.arguments.extend([
                "--library-path".into(),
                std::env::var_os("CAKE_TEST_LIB_DIR").expect("explicit library path required"),
                uci,
            ]);
        }
        spec.arguments.extend([
            "-p".into(),
            global.as_os_str().into(),
            "-p".into(),
            rpcd.as_os_str().into(),
        ]);
        for package_declaration in [false, true] {
            if package_declaration {
                let path = fixture.config.join("cake-autorate");
                let mut bytes = b"package 'cake-autorate'\n".to_vec();
                bytes.extend(fs::read(&path).unwrap());
                fs::write(path, bytes).unwrap();
            }
            let snapshot =
                CommittedSnapshot::capture_with(&fixture.config, &fixture.run, |arguments| {
                    CommittedSnapshot::query(&spec, arguments)
                })
                .unwrap();
            assert_eq!(
                snapshot.package("cake-autorate").unwrap().sections["lab"].options["marker"],
                "committed-cake-autorate"
            );
            assert_eq!(
                snapshot.package("sqm").unwrap().sections["lab"].options["marker"],
                "committed-sqm"
            );
            snapshot.attest().unwrap();
        }
        // Runtime recovery has only an accepted owned recipe, not authority to
        // capture today's public files. Exercise its distinct alias type with
        // the actual libuci CLI and both original-package savedir paths.
        let wrapper = fixture.root.join("runtime-uci");
        let quote = |value: &std::ffi::OsStr| {
            format!("'{}'", value.to_str().unwrap().replace('\'', "'\\''"))
        };
        let args = spec
            .arguments
            .iter()
            .map(|value| quote(value))
            .collect::<Vec<_>>()
            .join(" ");
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nexec {} {args} \"$@\"\n",
                quote(spec.program.as_os_str())
            ),
        )
        .unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
        let bytes = uci_edits::render(
            &[],
            &[
                Edit::AddSection {
                    section: "lab".into(),
                    kind: "queue".into(),
                },
                Edit::Set {
                    section: "lab".into(),
                    option: "marker".into(),
                    value: "applied-runtime-sqm".into(),
                },
                Edit::Set {
                    section: "lab".into(),
                    option: "note".into(),
                    value: "literal 'single quote' and spaces".into(),
                },
            ],
        )
        .unwrap();
        let frozen = FrozenSqmAlias::materialize(&bytes, &fixture.run, &wrapper).unwrap();
        let shown = frozen.section_show("lab").unwrap();
        let package = UciPackage::parse("sqm", std::str::from_utf8(&shown).unwrap()).unwrap();
        assert_eq!(
            package.sections["lab"].options["marker"],
            "applied-runtime-sqm"
        );
        assert_eq!(
            package.sections["lab"].options["note"],
            "literal 'single quote' and spaces"
        );
        let (directory, name) = frozen.sqm_runner_alias().unwrap();
        assert!(!directory.join("sqm").exists());
        fs::write(directory.join(name), b"changed alias").unwrap();
        assert!(frozen.sqm_runner_alias().is_err());
        drop(frozen);
        for dir in [&global, &rpcd] {
            assert_eq!(
                fs::read_to_string(dir.join("cake-autorate")).unwrap(),
                "cake-autorate.lab.marker='foreign-cake'\n"
            );
            assert_eq!(
                fs::read_to_string(dir.join("sqm")).unwrap(),
                "sqm.lab.marker='foreign-sqm'\n"
            );
        }
    }
}
