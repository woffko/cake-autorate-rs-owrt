//! Explicit, bounded MQ capability check. No traffic or existing queue changes.
//! The probe owns one down IFB identified by a durable random alias receipt.
use crate::operations::process::{run_bounded_command_output, SpawnSpec};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const RECORD_LIMIT: u64 = 4096;
const MAGIC: &[u8] = b"cake-qdisc-capability-v1\n";
const ALIAS_PREFIX: &str = "cake-autorate-mq-v1:";
type Result<T> = std::result::Result<T, String>;

fn io<T>(value: std::io::Result<T>) -> Result<T> {
    value.map_err(|_| "qdisc-capability-io".into())
}
fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|v| v.as_secs())
        .map_err(|_| "qdisc-capability-clock".into())
}
fn root() -> PathBuf {
    env::var_os("CAKE_AUTORATE_RUN_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/run/cake-autorate"))
        .join(".qdisc-capabilities")
}
fn attest_file(path: &Path, file: &File, private: bool) -> Result<fs::Metadata> {
    let named = io(fs::symlink_metadata(path))?;
    let opened = io(file.metadata())?;
    for metadata in [&named, &opened] {
        if !metadata.is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & if private { 0o077 } else { 0o022 } != 0
            || (private && metadata.nlink() != 1)
        {
            return Err("qdisc-capability-file-unsafe".into());
        }
    }
    if named.dev() != opened.dev() || named.ino() != opened.ino() {
        return Err("qdisc-capability-file-replaced".into());
    }
    Ok(opened)
}
fn owned_file(path: &Path, maximum: u64, private: bool) -> Result<(Vec<u8>, File)> {
    let file = io(OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path))?;
    let metadata = attest_file(path, &file, private)?;
    if metadata.len() > maximum {
        return Err("qdisc-capability-file-too-large".into());
    }
    let mut bytes = Vec::new();
    io((&file).take(maximum + 1).read_to_end(&mut bytes))?;
    if bytes.len() as u64 > maximum {
        return Err("qdisc-capability-file-too-large".into());
    }
    attest_file(path, &file, private)?;
    Ok((bytes, file))
}
fn small(path: &Path, maximum: usize) -> Result<String> {
    let mut bytes = Vec::new();
    io(io(File::open(path))?
        .take((maximum + 1) as u64)
        .read_to_end(&mut bytes))?;
    if bytes.len() > maximum {
        return Err("qdisc-capability-fact-too-large".into());
    }
    String::from_utf8(bytes)
        .map(|value| value.trim().to_string())
        .map_err(|_| "qdisc-capability-fact-invalid".into())
}

struct Files {
    root: PathBuf,
    directory: File,
    guard: File,
}
impl Files {
    fn open(path: &Path, create: bool) -> Result<Self> {
        let parent = path.parent().ok_or("qdisc-capability-path-invalid")?;
        if create {
            io(fs::create_dir_all(parent))?;
        }
        let parent = io(fs::canonicalize(parent))?;
        let metadata = io(fs::metadata(&parent))?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
            return Err("qdisc-capability-parent-unsafe".into());
        }
        let root = parent.join(path.file_name().ok_or("qdisc-capability-path-invalid")?);
        if create {
            match fs::DirBuilder::new().mode(0o700).create(&root) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err("qdisc-capability-io".into()),
            }
        }
        let metadata = io(fs::symlink_metadata(&root))?;
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            return Err("qdisc-capability-directory-unsafe".into());
        }
        let directory = io(OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
            .open(&root))?;
        let guard = io(OpenOptions::new()
            .read(true)
            .write(true)
            .create(create)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(root.join(".lock")))?;
        let metadata = attest_file(&root.join(".lock"), &guard, true)?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.len() != 0
            || metadata.mode() & 0o077 != 0
        {
            return Err("qdisc-capability-lock-unsafe".into());
        }
        if unsafe { libc::flock(guard.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("qdisc-capability-busy".into());
        }
        let files = Self {
            root,
            directory,
            guard,
        };
        files.attest()?;
        Ok(files)
    }
    fn attest(&self) -> Result<()> {
        let named = io(fs::symlink_metadata(&self.root))?;
        let opened = io(self.directory.metadata())?;
        if !named.is_dir()
            || named.uid() != unsafe { libc::geteuid() }
            || named.mode() & 0o077 != 0
            || named.dev() != opened.dev()
            || named.ino() != opened.ino()
        {
            return Err("qdisc-capability-directory-replaced".into());
        }
        attest_file(&self.root.join(".lock"), &self.guard, true)?;
        Ok(())
    }
    fn bytes(&self, name: &str) -> Result<Option<(Vec<u8>, File)>> {
        self.attest()?;
        let path = self.root.join(name);
        if matches!(fs::symlink_metadata(&path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        {
            return Ok(None);
        }
        owned_file(&path, RECORD_LIMIT, true).map(Some)
    }
    fn read(&self, name: &str) -> Result<Option<Value>> {
        self.bytes(name)?
            .map(|(bytes, _)| Self::parse(&bytes))
            .transpose()
    }
    fn parse(bytes: &[u8]) -> Result<Value> {
        let body = bytes
            .strip_prefix(MAGIC)
            .ok_or("qdisc-capability-record-unowned")?;
        serde_json::from_slice(body).map_err(|_| "qdisc-capability-record-invalid".into())
    }
    fn remove(&self, name: &str) -> Result<()> {
        if let Some((bytes, file)) = self.bytes(name)? {
            Self::parse(&bytes)?;
            self.unlink(name, &file)?;
        }
        Ok(())
    }
    fn unlink(&self, name: &str, file: &File) -> Result<()> {
        self.attest()?;
        let path = self.root.join(name);
        attest_file(&path, file, true)?;
        io(fs::remove_file(path))?;
        io(self.directory.sync_all())
    }
    fn discard_temporary(&self, name: &str) -> Result<()> {
        if let Some((bytes, file)) = self.bytes(name)? {
            // An interrupted write can leave an empty/partial magic or body.
            // Only this private, single-link, reserved temporary is eligible.
            if !bytes.starts_with(MAGIC) && !MAGIC.starts_with(&bytes) {
                return Err("qdisc-capability-temporary-unowned".into());
            }
            self.unlink(name, &file)?;
        }
        Ok(())
    }
    fn write(&self, name: &str, value: Value) -> Result<()> {
        self.read(name)?;
        let temporary = format!("{name}.tmp");
        self.discard_temporary(&temporary)?;
        let mut bytes = MAGIC.to_vec();
        bytes.extend(serde_json::to_vec(&value).map_err(|_| "qdisc-capability-record-invalid")?);
        if bytes.len() as u64 > RECORD_LIMIT {
            return Err("qdisc-capability-record-too-large".into());
        }
        let mut file = io(OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.root.join(&temporary)))?;
        io(file.write_all(&bytes))?;
        io(file.sync_all())?;
        self.attest()?;
        attest_file(&self.root.join(&temporary), &file, true)?;
        self.read(name)?;
        io(fs::rename(self.root.join(&temporary), self.root.join(name)))?;
        io(self.directory.sync_all())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Fingerprint {
    base: String,
    module: String,
}
impl Fingerprint {
    fn key(&self) -> String {
        crate::config_candidate::digest(format!("{}|{}", self.base, self.module).as_bytes())
    }
}

trait Backend {
    fn fingerprint(&self) -> Result<Fingerprint>;
    fn boot_namespace(&self) -> Result<(String, String)>;
    fn create(&mut self, name: &str, alias: &str) -> Result<()>;
    fn identity(&self, name: &str, alias: &str) -> Result<Option<u32>>;
    fn test_mq(&mut self, name: &str) -> Result<bool>;
    fn delete(&mut self, name: &str, alias: &str, ifindex: u32) -> Result<()>;
}

fn recover(files: &Files, backend: &mut impl Backend) -> Result<()> {
    let Some(pending) = files.read("pending.json")? else {
        return Ok(());
    };
    let name = pending["name"]
        .as_str()
        .filter(|name| {
            name.len() == 15
                && name.starts_with("cqm")
                && name[3..].bytes().all(|b| b.is_ascii_hexdigit())
        })
        .ok_or("qdisc-capability-recovery-name-invalid")?;
    let alias = pending["alias"]
        .as_str()
        .filter(|alias| {
            alias.strip_prefix(ALIAS_PREFIX).is_some_and(|nonce| {
                nonce.len() == 32 && nonce.bytes().all(|b| b.is_ascii_hexdigit())
            })
        })
        .ok_or("qdisc-capability-recovery-alias-invalid")?;
    let boot = pending["boot"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or("qdisc-capability-recovery-boot-invalid")?;
    let namespace = pending["namespace"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or("qdisc-capability-recovery-namespace-invalid")?;
    let current = backend.boot_namespace()?;
    if boot != current.0 {
        // A link cannot survive a boot. Never inspect/delete a new boot's link.
        return files.remove("pending.json");
    }
    if namespace != current.1 {
        return Err("qdisc-capability-recovery-namespace-changed".into());
    }
    if let Some(ifindex) = backend.identity(name, alias)? {
        if pending
            .get("ifindex")
            .and_then(Value::as_u64)
            .is_some_and(|expected| expected != ifindex as u64)
        {
            return Err("qdisc-capability-recovery-ifindex-changed".into());
        }
        backend.delete(name, alias, ifindex)?;
    }
    files.remove("pending.json")
}

fn probe_with(files: &Files, backend: &mut impl Backend, timestamp: u64) -> Result<Value> {
    recover(files, backend)?;
    files.remove("result.json")?;
    let before = backend.fingerprint()?;
    let mut nonce = [0; 16];
    io(io(File::open("/dev/urandom"))?.read_exact(&mut nonce))?;
    let nonce: String = nonce.iter().map(|byte| format!("{byte:02x}")).collect();
    let name = format!("cqm{}", &nonce[..12]);
    let alias = format!("{ALIAS_PREFIX}{nonce}");
    let (boot, namespace) = backend.boot_namespace()?;
    let mut pending = json!({"name": name, "alias": alias, "boot": boot, "namespace": namespace});
    files.write("pending.json", pending.clone())?;
    let result: Result<(bool, Fingerprint)> = (|| {
        backend.create(&name, &alias)?;
        let ifindex = backend
            .identity(&name, &alias)?
            .ok_or("qdisc-capability-created-link-missing")?;
        pending["ifindex"] = json!(ifindex);
        files.write("pending.json", pending)?;
        let supported = backend.test_mq(&name)?;
        let observed = backend.fingerprint()?;
        if before.base != observed.base {
            return Err("qdisc-capability-tools-changed".into());
        }
        Ok((supported, observed))
    })();
    // Cleanup is required before publishing support, including a failed test.
    recover(files, backend)?;
    let (supported, observed) = result?;
    if backend.fingerprint()? != observed {
        return Err("qdisc-capability-kernel-changed".into());
    }
    files.write(
        "result.json",
        json!({"key": observed.key(), "verified_at": timestamp, "supported": supported}),
    )?;
    Ok(json!({"supported": supported, "verified_at": timestamp}))
}

struct OpenWrt {
    ip: PathBuf,
    tc: PathBuf,
    sys: PathBuf,
}
impl OpenWrt {
    fn new() -> Self {
        fn executable(variable: &str, name: &str) -> PathBuf {
            env::var_os(variable).map(PathBuf::from).unwrap_or_else(|| {
                ["/sbin", "/usr/sbin", "/bin", "/usr/bin"]
                    .iter()
                    .map(|root| Path::new(root).join(name))
                    .find(|p| p.exists())
                    .unwrap_or_else(|| Path::new("/sbin").join(name))
            })
        }
        Self {
            ip: executable("CAKE_AUTORATE_IP", "ip"),
            tc: executable("CAKE_AUTORATE_TC", "tc"),
            sys: env::var_os("CAKE_AUTORATE_SYS_CLASS_NET")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/sys/class/net")),
        }
    }
    fn command(&self, program: &Path, arguments: &[&str]) -> Result<(bool, String)> {
        let output = run_bounded_command_output(
            &SpawnSpec {
                program: program.into(),
                arguments: arguments.iter().map(Into::into).collect(),
                environment: vec![],
            },
            Duration::from_secs(3),
            64 * 1024,
            || false,
        )
        .map_err(|_| "qdisc-capability-command-failed")?;
        Ok((
            output.status.success(),
            String::from_utf8(output.stdout).map_err(|_| "qdisc-capability-output-invalid")?,
        ))
    }
}
impl Backend for OpenWrt {
    fn boot_namespace(&self) -> Result<(String, String)> {
        let ns = io(fs::metadata("/proc/self/ns/net"))?;
        Ok((
            small(Path::new("/proc/sys/kernel/random/boot_id"), 64)?,
            format!("{}:{}", ns.dev(), ns.ino()),
        ))
    }
    fn fingerprint(&self) -> Result<Fingerprint> {
        let tc = io(fs::canonicalize(&self.tc))?;
        let mut hash = Sha256::new();
        let (boot, namespace) = self.boot_namespace()?;
        hash.update(format!("{boot}|{namespace}"));
        hash.update(small(Path::new("/proc/sys/kernel/osrelease"), 256)?);
        hash.update(owned_file(&tc, 16 * 1024 * 1024, false)?.0);
        for root in ["/usr/lib/tc", "/lib/tc"] {
            for name in ["q_cake.so", "q_cake_mq.so"] {
                let path = Path::new(root).join(name);
                hash.update(path.as_os_str().as_encoded_bytes());
                if path.exists() {
                    hash.update(
                        owned_file(&io(fs::canonicalize(path))?, 4 * 1024 * 1024, false)?.0,
                    );
                }
            }
        }
        let mut module = String::new();
        for name in ["sch_cake", "sch_cake_mq"] {
            let path = Path::new("/sys/module").join(name);
            match fs::metadata(&path) {
                Ok(meta) => module.push_str(&format!(
                    "{name}:{}:{}:{}:{}:{}",
                    meta.dev(),
                    meta.ino(),
                    meta.mtime(),
                    meta.mtime_nsec(),
                    small(&path.join("srcversion"), 128).unwrap_or_default()
                )),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    module.push_str(&format!("{name}:absent"))
                }
                Err(_) => return Err("qdisc-capability-module-unreadable".into()),
            }
        }
        Ok(Fingerprint {
            base: format!("{:x}", hash.finalize()),
            module,
        })
    }
    fn create(&mut self, name: &str, alias: &str) -> Result<()> {
        if self.sys.join(name).exists() {
            return Err("qdisc-capability-name-occupied".into());
        }
        if !self
            .command(
                &self.ip,
                &[
                    "link",
                    "add",
                    "name",
                    name,
                    "numtxqueues",
                    "2",
                    "numrxqueues",
                    "2",
                    "alias",
                    alias,
                    "type",
                    "ifb",
                ],
            )?
            .0
        {
            return Err("qdisc-capability-link-create-failed".into());
        }
        Ok(())
    }
    fn identity(&self, name: &str, alias: &str) -> Result<Option<u32>> {
        let path = self.sys.join(name);
        if !path.exists() {
            return Ok(None);
        }
        if small(&path.join("ifalias"), 256)? != alias {
            return Err("qdisc-capability-link-unowned".into());
        }
        let index = small(&path.join("ifindex"), 32)?
            .parse::<u32>()
            .map_err(|_| "qdisc-capability-ifindex-invalid")?;
        let (ok, output) = self.command(&self.ip, &["-details", "link", "show", "dev", name])?;
        if index == 0 || !ok || !output.split_whitespace().any(|word| word == "ifb") {
            return Err("qdisc-capability-link-kind-invalid".into());
        }
        Ok(Some(index))
    }
    fn test_mq(&mut self, name: &str) -> Result<bool> {
        if !self
            .command(
                &self.tc,
                &[
                    "qdisc",
                    "replace",
                    "dev",
                    name,
                    "root",
                    "cake_mq",
                    "bandwidth",
                    "1000kbit",
                    "besteffort",
                ],
            )?
            .0
        {
            return Ok(false);
        }
        let (ok, output) = self.command(&self.tc, &["qdisc", "show", "dev", name])?;
        Ok(ok
            && matches!(
                crate::root_cake_qdisc(&output),
                Ok((crate::CakeQdiscKind::CakeMq, 1000))
            ))
    }
    fn delete(&mut self, name: &str, alias: &str, ifindex: u32) -> Result<()> {
        if self.identity(name, alias)? != Some(ifindex) {
            return Err("qdisc-capability-cleanup-identity-changed".into());
        }
        if !self
            .command(&self.ip, &["link", "delete", "dev", name, "type", "ifb"])?
            .0
            || self.sys.join(name).exists()
        {
            return Err("qdisc-capability-cleanup-required".into());
        }
        Ok(())
    }
}

fn script_supports_mq(script: &str) -> Result<bool> {
    if !crate::sqm_config::safe_script(script) {
        return Err("sqm_script is unsafe".into());
    }
    let directory = env::var_os("CAKE_AUTORATE_SQM_LIB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/usr/lib/sqm"));
    let root = io(fs::canonicalize(&directory))?;
    let path = io(fs::canonicalize(root.join(script)))?;
    if !path.starts_with(&root) {
        return Err("SQM script escapes its library directory".into());
    }
    let source = String::from_utf8(owned_file(&path, 256 * 1024, false)?.0)
        .map_err(|_| "SQM script is not text")?;
    Ok(source.lines().any(|line| {
        matches!(
            line.split('#').next().unwrap_or("").trim(),
            "SUPPORT_MQ=1" | "SUPPORT_MQ='1'" | "SUPPORT_MQ=\"1\""
        )
    }))
}

pub(crate) fn supported(script: &str) -> Result<bool> {
    if !script_supports_mq(script)? {
        return Ok(false);
    }
    let path = root();
    if !path.exists() {
        return Ok(false);
    }
    let files = Files::open(&path, false)?;
    cached(&files, &OpenWrt::new(), now()?)
}

fn cached(files: &Files, backend: &impl Backend, timestamp: u64) -> Result<bool> {
    // Never authorize a config while cleanup of any previous probe is pending.
    if files.read("pending.json")?.is_some() {
        return Ok(false);
    }
    let Some(value) = files.read("result.json")? else {
        return Ok(false);
    };
    // Proof is boot/tool/module-bound, not a timer that can unexpectedly make
    // an otherwise unchanged healthy configuration impossible to restart.
    Ok(value["supported"] == true
        && value["verified_at"]
            .as_u64()
            .is_some_and(|at| at <= timestamp)
        && value["key"].as_str() == Some(backend.fingerprint()?.key().as_str()))
}

pub(crate) fn probe(script: &str) -> Result<Value> {
    if !script_supports_mq(script)? {
        return Err("selected SQM script does not declare multi-queue support".into());
    }
    let files = Files::open(&root(), true)?;
    probe_with(&files, &mut OpenWrt::new(), now()?)
}

/// For an already-authorized mutating startup/Apply preparation only. Read-only
/// validation and status must use supported(), never this refresh path.
pub(crate) fn ensure(script: &str) -> Result<()> {
    if supported(script)? {
        return Ok(());
    }
    if probe(script)?["supported"] == true {
        Ok(())
    } else {
        Err("qdisc-capability-mq-unavailable".into())
    }
}

pub(crate) fn rpc(request: &Value, active: bool) -> Value {
    let Some(script) = request
        .get("script")
        .and_then(Value::as_str)
        .filter(|script| crate::sqm_config::safe_script(script))
    else {
        return json!({"schema_version": 1, "ok": false, "code": "qdisc-capability-script-invalid"});
    };
    let result = if active {
        probe(script)
    } else {
        supported(script).map(|supported| json!({"supported": supported}))
    };
    match result {
        Ok(mut value) => {
            value["script"] = json!(script);
            json!({"schema_version": 1, "ok": true, "result": value})
        }
        Err(code) => json!({"schema_version": 1, "ok": false, "code": code}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = env::temp_dir().join(format!(
                "cake-r3-mq-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }
        fn files(&self) -> Files {
            Files::open(&self.0.join("proof"), true).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct Fake {
        link: Option<(String, String, u32)>,
        boot: String,
        namespace: String,
        base: String,
        module: String,
        supported: bool,
        fail_create: bool,
        fail_test: bool,
        fail_delete: bool,
        change_tools: bool,
        change_module: bool,
        change_index: bool,
        events: Vec<&'static str>,
    }
    impl Default for Fake {
        fn default() -> Self {
            Self {
                link: None,
                boot: "boot-a".into(),
                namespace: "ns-a".into(),
                base: "tools-a".into(),
                module: "unloaded".into(),
                supported: true,
                fail_create: false,
                fail_test: false,
                fail_delete: false,
                change_tools: false,
                change_module: false,
                change_index: false,
                events: vec![],
            }
        }
    }
    impl Backend for Fake {
        fn fingerprint(&self) -> Result<Fingerprint> {
            Ok(Fingerprint {
                base: format!("{}|{}|{}", self.boot, self.namespace, self.base),
                module: self.module.clone(),
            })
        }
        fn boot_namespace(&self) -> Result<(String, String)> {
            Ok((self.boot.clone(), self.namespace.clone()))
        }
        fn create(&mut self, name: &str, alias: &str) -> Result<()> {
            self.events.push("create");
            assert!(self.link.is_none());
            assert_eq!(name.len(), 15);
            assert_eq!(alias.strip_prefix(ALIAS_PREFIX).unwrap().len(), 32);
            self.link = Some((name.into(), alias.into(), 42));
            if self.fail_create {
                Err("injected-create-lost-ack".into())
            } else {
                Ok(())
            }
        }
        fn identity(&self, name: &str, alias: &str) -> Result<Option<u32>> {
            match &self.link {
                Some((n, a, i)) if n == name && a == alias => Ok(Some(*i)),
                Some(_) => Err("injected-foreign-link".into()),
                None => Ok(None),
            }
        }
        fn test_mq(&mut self, _: &str) -> Result<bool> {
            self.events.push("test");
            assert!(self.link.is_some());
            if self.change_tools {
                self.base = "tools-b".into();
            }
            if self.change_module {
                self.module = "loaded".into();
            }
            if self.change_index {
                self.link.as_mut().unwrap().2 = 43;
            }
            if self.fail_test {
                Err("injected-test-failed".into())
            } else {
                Ok(self.supported)
            }
        }
        fn delete(&mut self, name: &str, alias: &str, index: u32) -> Result<()> {
            assert_eq!(self.identity(name, alias)?, Some(index));
            self.events.push("delete");
            if self.fail_delete {
                return Err("injected-cleanup-failed".into());
            }
            self.link = None;
            Ok(())
        }
    }

    #[test]
    fn r3_mq_probe_publishes_only_after_owned_cleanup_and_accepts_module_autoload() {
        let fixture = Fixture::new();
        let files = fixture.files();
        let mut backend = Fake {
            change_module: true,
            ..Fake::default()
        };
        assert_eq!(
            probe_with(&files, &mut backend, 100).unwrap()["supported"],
            true
        );
        assert_eq!(backend.events, ["create", "test", "delete"]);
        assert!(backend.link.is_none());
        assert!(files.read("pending.json").unwrap().is_none());
        assert!(cached(&files, &backend, 100_000).unwrap());
        assert!(!cached(&files, &backend, 99).unwrap());
        backend.module = "reloaded".into();
        assert!(!cached(&files, &backend, 101).unwrap());
        backend.module = "loaded".into();
        backend.base = "updated-tc".into();
        assert!(!cached(&files, &backend, 101).unwrap());
        for entry in fs::read_dir(&files.root).unwrap() {
            assert_eq!(entry.unwrap().metadata().unwrap().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn r3_mq_failures_cleanup_and_never_reuse_previous_positive_proof() {
        for case in 0..4 {
            let fixture = Fixture::new();
            let files = fixture.files();
            let mut backend = Fake::default();
            probe_with(&files, &mut backend, 100).unwrap();
            match case {
                0 => backend.fail_create = true,
                1 => backend.fail_test = true,
                2 => backend.change_tools = true,
                _ => backend.supported = false,
            }
            let outcome = probe_with(&files, &mut backend, 101);
            if case == 3 {
                assert_eq!(outcome.unwrap()["supported"], false);
            } else {
                assert!(outcome.is_err());
            }
            assert!(backend.link.is_none());
            assert!(!cached(&files, &backend, 102).unwrap());
            assert!(files.read("pending.json").unwrap().is_none());
        }
    }

    #[test]
    fn r3_mq_cleanup_failure_keeps_durable_identity_for_retry_without_duplicate() {
        let fixture = Fixture::new();
        let files = fixture.files();
        let mut backend = Fake {
            fail_delete: true,
            ..Fake::default()
        };
        assert!(probe_with(&files, &mut backend, 100).is_err());
        assert!(files.read("pending.json").unwrap().is_some());
        assert!(!cached(&files, &backend, 101).unwrap());
        assert!(probe_with(&files, &mut backend, 102).is_err());
        assert_eq!(backend.events.iter().filter(|e| **e == "create").count(), 1);
        backend.fail_delete = false;
        recover(&files, &mut backend).unwrap();
        assert!(backend.link.is_none());
        assert!(files.read("pending.json").unwrap().is_none());
        assert!(files.read("result.json").unwrap().is_none());
    }

    #[test]
    fn r3_mq_recovery_preserves_replaced_links_and_other_namespaces() {
        for replacement in 0..3 {
            let fixture = Fixture::new();
            let files = fixture.files();
            let mut backend = Fake {
                fail_delete: true,
                ..Fake::default()
            };
            assert!(probe_with(&files, &mut backend, 100).is_err());
            backend.fail_delete = false;
            match replacement {
                0 => backend.link.as_mut().unwrap().1 = "foreign".into(),
                1 => backend.link.as_mut().unwrap().2 = 43,
                _ => backend.namespace = "ns-b".into(),
            }
            let link = backend.link.clone();
            let event_count = backend.events.len();
            assert!(recover(&files, &mut backend).is_err());
            assert_eq!(backend.link, link);
            assert_eq!(backend.events.len(), event_count);
            assert!(files.read("pending.json").unwrap().is_some());
            backend.boot = "boot-b".into();
            recover(&files, &mut backend).unwrap();
            assert_eq!(backend.link, link);
            assert_eq!(backend.events.len(), event_count);
        }
    }

    #[test]
    fn r3_mq_ifindex_replacement_during_probe_never_deletes_or_publishes() {
        let fixture = Fixture::new();
        let files = fixture.files();
        let mut backend = Fake {
            change_index: true,
            ..Fake::default()
        };
        assert!(probe_with(&files, &mut backend, 100).is_err());
        assert_eq!(backend.events, ["create", "test"]);
        assert!(!cached(&files, &backend, 101).unwrap());
        assert!(backend.link.is_some());
    }

    #[test]
    fn r3_mq_private_files_reject_foreign_bytes_links_and_concurrent_probe() {
        let fixture = Fixture::new();
        let files = fixture.files();
        assert!(Files::open(&files.root, true).is_err());
        let foreign = fixture.0.join("foreign");
        fs::write(&foreign, b"foreign-marker").unwrap();
        fs::set_permissions(&foreign, fs::Permissions::from_mode(0o600)).unwrap();
        let target = files.root.join("result.json");
        symlink(&foreign, &target).unwrap();
        assert!(files.write("result.json", json!({})).is_err());
        fs::remove_file(&target).unwrap();
        fs::hard_link(&foreign, &target).unwrap();
        assert!(files.remove("result.json").is_err());
        fs::remove_file(&target).unwrap();
        fs::copy(&foreign, &target).unwrap();
        assert!(files.write("result.json", json!({})).is_err());
        assert_eq!(fs::read(&foreign).unwrap(), b"foreign-marker");
        assert_eq!(fs::read(&target).unwrap(), b"foreign-marker");
    }

    #[test]
    fn r3_mq_temporary_crash_prefix_is_recoverable_but_foreign_bytes_are_preserved() {
        let fixture = Fixture::new();
        let files = fixture.files();
        let path = files.root.join("result.json.tmp");
        for bytes in [
            b"".as_slice(),
            &MAGIC[..8],
            MAGIC,
            &[MAGIC, b"{partial"].concat(),
        ] {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .unwrap();
            file.write_all(bytes).unwrap();
            files
                .write("result.json", json!({"supported": false}))
                .unwrap();
            assert!(!path.exists());
        }
        fs::write(&path, b"foreign-marker").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(files.write("result.json", json!({})).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"foreign-marker");
    }

    #[test]
    fn r3_mq_replaced_directory_invalidates_open_store() {
        let fixture = Fixture::new();
        let files = fixture.files();
        fs::rename(&files.root, fixture.0.join("old")).unwrap();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&files.root)
            .unwrap();
        assert!(files.write("pending.json", json!({})).is_err());
        assert_eq!(fs::read_dir(&files.root).unwrap().count(), 0);
    }
}
