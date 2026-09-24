//! Private, bounded candidate transfer for rpcd's small executable-plugin stdin.
//! The RPC caller supplies opaque identities/offsets, never a filesystem path.
use crate::config_candidate;
use serde_json::{json, Value};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAGIC: &[u8] = b"cake-candidate-transfer-v1\n";
const HEADER: usize = 192;
const CHUNK_BYTES: usize = 1024;
const RPC_INPUT_BYTES: usize = 4096;
const MAX_ACTIVE: usize = 4;
const MAX_SCAN: usize = 64;
const TTL_S: u64 = 120;
type Result<T> = std::result::Result<T, &'static str>;

fn io<T>(result: std::io::Result<T>) -> Result<T> {
    result.map_err(|_| "candidate-storage-io")
}
fn identity(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn digest_identity(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn owned(metadata: &fs::Metadata) -> bool {
    metadata.is_file()
        && metadata.nlink() == 1
        && metadata.uid() == unsafe { libc::geteuid() }
        && metadata.mode() & 0o077 == 0
}
fn attest(path: &Path, file: &File) -> Result<fs::Metadata> {
    let named = io(fs::symlink_metadata(path))?;
    let opened = io(file.metadata())?;
    if !owned(&named)
        || !owned(&opened)
        || named.dev() != opened.dev()
        || named.ino() != opened.ino()
    {
        return Err("candidate-storage-identity-changed");
    }
    Ok(named)
}
fn open_file(path: &Path, create: bool) -> Result<File> {
    let file = io(OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path))?;
    attest(path, &file)?;
    Ok(file)
}

struct Record {
    request_id: String,
    token: String,
    length: usize,
    created: u64,
    digest: String,
}
impl Record {
    fn bytes(&self) -> [u8; HEADER] {
        let mut data = [0; HEADER];
        data[..MAGIC.len()].copy_from_slice(MAGIC);
        data[32..64].copy_from_slice(self.request_id.as_bytes());
        data[64..72].copy_from_slice(&(self.length as u64).to_le_bytes());
        data[72..80].copy_from_slice(&self.created.to_le_bytes());
        data[80..112].copy_from_slice(self.token.as_bytes());
        data[112..176].copy_from_slice(self.digest.as_bytes());
        data
    }
    fn read(file: &mut File, token: &str) -> Result<Self> {
        let mut data = [0; HEADER];
        io(file.seek(SeekFrom::Start(0)))?;
        io(file.read_exact(&mut data))?;
        if &data[..MAGIC.len()] != MAGIC
            || data[MAGIC.len()..32].iter().any(|b| *b != 0)
            || data[176..].iter().any(|b| *b != 0)
        {
            return Err("candidate-storage-unrecognized");
        }
        let request_id =
            std::str::from_utf8(&data[32..64]).map_err(|_| "candidate-storage-unrecognized")?;
        let stored_token =
            std::str::from_utf8(&data[80..112]).map_err(|_| "candidate-storage-unrecognized")?;
        let digest =
            std::str::from_utf8(&data[112..176]).map_err(|_| "candidate-storage-unrecognized")?;
        let length = u64::from_le_bytes(data[64..72].try_into().unwrap());
        if !identity(request_id)
            || !digest_identity(digest)
            || stored_token != token
            || length == 0
            || length > config_candidate::MAX_INPUT_BYTES as u64
        {
            return Err("candidate-storage-unrecognized");
        }
        Ok(Self {
            request_id: request_id.into(),
            token: token.into(),
            length: length as usize,
            created: u64::from_le_bytes(data[72..80].try_into().unwrap()),
            digest: digest.into(),
        })
    }
    fn live(&self, now: u64) -> bool {
        self.created <= now && now - self.created <= TTL_S
    }
}

struct Store {
    root: PathBuf,
    _lock: File,
    now: u64,
}
impl Store {
    fn open(root: &Path, now: u64) -> Result<Self> {
        let parent = root.parent().ok_or("candidate-storage-path-invalid")?;
        io(fs::create_dir_all(parent))?;
        let parent = io(fs::canonicalize(parent))?;
        let metadata = io(fs::metadata(&parent))?;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
            return Err("candidate-storage-parent-unsafe");
        }
        let root = parent.join(root.file_name().ok_or("candidate-storage-path-invalid")?);
        match fs::symlink_metadata(&root) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match fs::DirBuilder::new().mode(0o700).create(&root) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(_) => return Err("candidate-storage-io"),
                }
            }
            Err(_) => return Err("candidate-storage-io"),
        }
        let metadata = io(fs::symlink_metadata(&root))?;
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
        {
            return Err("candidate-storage-directory-unsafe");
        }
        let lock_path = root.join(".lock");
        let lock = match open_file(&lock_path, true) {
            Ok(file) => file,
            Err(_) => open_file(&lock_path, false)?,
        };
        if io(lock.metadata())?.len() != 0 {
            return Err("candidate-storage-lock-unsafe");
        }
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("candidate-storage-busy");
        }
        attest(&lock_path, &lock)?;
        Ok(Self {
            root,
            _lock: lock,
            now,
        })
    }
    fn path(&self, token: &str) -> Result<PathBuf> {
        if !identity(token) {
            return Err("candidate-token-invalid");
        }
        Ok(self.root.join(format!("{token}.candidate")))
    }
    fn remove(&self, path: &Path, file: &File) -> Result<()> {
        attest(path, file)?;
        io(fs::remove_file(path))
    }
    fn records(&self) -> Result<Vec<Record>> {
        let mut records = Vec::new();
        for (index, entry) in io(fs::read_dir(&self.root))?.enumerate() {
            if index >= MAX_SCAN {
                return Err("candidate-storage-scan-limit");
            }
            let entry = io(entry)?;
            let name = entry.file_name();
            let Some(token) = name
                .to_str()
                .and_then(|name| name.strip_suffix(".candidate"))
            else {
                continue;
            };
            if !identity(token) {
                continue;
            }
            let path = self.path(token)?;
            let mut file = open_file(&path, false)?;
            let metadata = attest(&path, &file)?;
            // Only a crash's partial reserved header may be retired as an orphan.
            if metadata.len() < HEADER as u64 {
                let mut prefix = Vec::new();
                io((&mut file)
                    .take(MAGIC.len() as u64)
                    .read_to_end(&mut prefix))?;
                if !MAGIC.starts_with(&prefix) {
                    return Err("candidate-storage-unrecognized");
                }
                if metadata.mtime() >= 0 && self.now.saturating_sub(metadata.mtime() as u64) > TTL_S
                {
                    self.remove(&path, &file)?;
                } else {
                    return Err("candidate-storage-partial-header");
                }
                continue;
            }
            let record = Record::read(&mut file, token)?;
            if metadata.len() > (HEADER + record.length) as u64 {
                return Err("candidate-storage-length-invalid");
            }
            if record.created > self.now {
                return Err("candidate-clock-regressed");
            }
            if !record.live(self.now) {
                self.remove(&path, &file)?;
            } else {
                records.push(record);
            }
        }
        Ok(records)
    }
    fn begin(&self, request_id: &str, length: usize, digest: &str) -> Result<Value> {
        if !identity(request_id)
            || !digest_identity(digest)
            || length == 0
            || length > config_candidate::MAX_INPUT_BYTES
        {
            return Err("candidate-begin-invalid");
        }
        let records = self.records()?;
        if let Some(record) = records
            .iter()
            .find(|record| record.request_id == request_id)
        {
            if record.length != length || record.digest != digest {
                return Err("candidate-request-conflict");
            }
            return Ok(json!({"token": record.token, "chunk_bytes": CHUNK_BYTES}));
        }
        if records.len() >= MAX_ACTIVE {
            return Err("candidate-storage-full");
        }
        let mut random = [0; 16];
        io(io(File::open("/dev/urandom"))?.read_exact(&mut random))?;
        let token: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = self.path(&token)?;
        let mut file = open_file(&path, true)?;
        let record = Record {
            request_id: request_id.into(),
            token: token.clone(),
            length,
            created: self.now,
            digest: digest.into(),
        };
        if file.write_all(&record.bytes()).is_err() {
            let _ = self.remove(&path, &file);
            return Err("candidate-storage-io");
        }
        Ok(json!({"token": token, "chunk_bytes": CHUNK_BYTES}))
    }
    fn load(&self, token: &str, request_id: &str) -> Result<(PathBuf, File, Record)> {
        let path = self.path(token)?;
        let mut file = open_file(&path, false)?;
        let record = Record::read(&mut file, token)?;
        if record.request_id != request_id {
            return Err("candidate-request-mismatch");
        }
        if !record.live(self.now) {
            if record.created <= self.now {
                self.remove(&path, &file)?;
            }
            return Err("candidate-expired");
        }
        if attest(&path, &file)?.len() > (HEADER + record.length) as u64 {
            return Err("candidate-storage-length-invalid");
        }
        Ok((path, file, record))
    }
    fn append(&self, token: &str, request_id: &str, offset: usize, hex: &str) -> Result<Value> {
        if hex.is_empty() || hex.len() > CHUNK_BYTES * 2 || hex.len() % 2 != 0 || !hex.is_ascii() {
            return Err("candidate-chunk-invalid");
        }
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| "candidate-chunk-invalid"))
            .collect::<Result<Vec<_>>>()?;
        let (path, mut file, record) = self.load(token, request_id)?;
        let current = attest(&path, &file)?
            .len()
            .checked_sub(HEADER as u64)
            .ok_or("candidate-storage-length-invalid")? as usize;
        let end = offset
            .checked_add(bytes.len())
            .ok_or("candidate-chunk-offset-invalid")?;
        if offset > current || end > record.length {
            return Err("candidate-chunk-offset-invalid");
        }
        // Lost ACKs and partial writes are replayed by exact byte identity,
        // never by blindly appending the same chunk twice.
        let overlap = (current - offset).min(bytes.len());
        let mut existing = vec![0; overlap];
        io(file.seek(SeekFrom::Start((HEADER + offset) as u64)))?;
        io(file.read_exact(&mut existing))?;
        if existing != bytes[..overlap] {
            return Err("candidate-chunk-conflict");
        }
        if overlap < bytes.len() {
            io(file.seek(SeekFrom::End(0)))?;
            io(file.write_all(&bytes[overlap..]))?;
        }
        attest(&path, &file)?;
        Ok(json!({"next_offset": current.max(end)}))
    }
    fn finish(&self, token: &str, request_id: &str) -> Result<Value> {
        let (path, mut file, record) = self.load(token, request_id)?;
        if attest(&path, &file)?.len() != (HEADER + record.length) as u64 {
            return Err("candidate-incomplete");
        }
        io(file.seek(SeekFrom::Start(HEADER as u64)))?;
        let mut bytes = Vec::with_capacity(record.length);
        io((&mut file)
            .take((record.length + 1) as u64)
            .read_to_end(&mut bytes))?;
        if bytes.len() != record.length || config_candidate::digest(&bytes) != record.digest {
            self.remove(&path, &file)?;
            return Err("candidate-digest-mismatch");
        }
        let mut result = config_candidate::validate_bytes(&bytes);
        self.remove(&path, &file)?;
        if result.get("request_id").is_none() && result.get("valid") == Some(&json!(false)) {
            // Structural rejection is still an identity-bound failure receipt.
            // Never attach an owner to an unbound successful validation.
            result["request_id"] = json!(request_id);
            result["candidate_sha256"] = json!(record.digest);
        } else if result.get("request_id").and_then(Value::as_str) != Some(request_id) {
            result = json!({"schema_version": 1, "validation_scope": "controller-sqm",
                "request_id": request_id, "candidate_sha256": record.digest,
                "valid": false, "code": "candidate-body-identity-invalid"});
        }
        Ok(json!({"validation": result}))
    }
    fn cancel(&self, token: &str, request_id: &str) -> Result<Value> {
        let path = self.path(token)?;
        if matches!(fs::symlink_metadata(&path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        {
            return Ok(json!({"cancelled": true}));
        }
        let (path, file, _) = self.load(token, request_id)?;
        self.remove(&path, &file)?;
        Ok(json!({"cancelled": true}))
    }

    fn cancel_request(&self, request_id: &str, digest: &str) -> Result<Value> {
        if !digest_identity(digest) {
            return Err("candidate-digest-invalid");
        }
        if let Some(record) = self
            .records()?
            .iter()
            .find(|record| record.request_id == request_id)
        {
            if record.digest != digest {
                return Err("candidate-request-conflict");
            }
            return self.cancel(&record.token, request_id);
        }
        Ok(json!({"cancelled": true}))
    }
}

fn dispatch(root: &Path, now: u64, method: &str, request: &Value) -> Result<Value> {
    let request_id = request
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|id| identity(id))
        .ok_or("candidate-request-id-invalid")?;
    let store = Store::open(root, now)?;
    if method != "begin" {
        store.records()?;
    }
    let token = || {
        request
            .get("token")
            .and_then(Value::as_str)
            .ok_or("candidate-token-invalid")
    };
    let number = |key| {
        request
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or("candidate-number-invalid")
    };
    let data = match method {
        "begin" => store.begin(
            request_id,
            number("length")?,
            request
                .get("sha256")
                .and_then(Value::as_str)
                .ok_or("candidate-digest-invalid")?,
        ),
        "append" => store.append(
            token()?,
            request_id,
            number("offset")?,
            request
                .get("data")
                .and_then(Value::as_str)
                .ok_or("candidate-chunk-invalid")?,
        ),
        "finish" => store.finish(token()?, request_id),
        "cancel" => match request.get("token").and_then(Value::as_str) {
            Some(token) if !token.is_empty() => store.cancel(token, request_id),
            _ => store.cancel_request(
                request_id,
                request
                    .get("sha256")
                    .and_then(Value::as_str)
                    .ok_or("candidate-digest-invalid")?,
            ),
        },
        _ => Err("candidate-method-invalid"),
    }?;
    Ok(json!({"schema_version": 1, "request_id": request_id, "ok": true, "result": data}))
}

pub(crate) fn run(mut args: impl Iterator<Item = String>, input: impl Read) -> Value {
    let command = args.next();
    let method = args.next();
    if command.as_deref() == Some("list") && method.is_none() {
        return json!({"schema": {}, "mq_status": {"script": ""}, "mq_probe": {"script": ""}, "begin": {"request_id": "", "length": 0, "sha256": ""},
            "append": {"request_id": "", "token": "", "offset": 0, "data": ""},
            "finish": {"request_id": "", "token": ""}, "cancel": {"request_id": "", "token": "", "sha256": ""}});
    }
    let result = (|| {
        if command.as_deref() != Some("call") || args.next().is_some() {
            return Err("candidate-rpc-arguments-invalid");
        }
        let method = method
            .as_deref()
            .filter(|m| {
                matches!(
                    *m,
                    "schema" | "begin" | "append" | "finish" | "cancel" | "mq_status" | "mq_probe"
                )
            })
            .ok_or("candidate-method-invalid")?;
        let mut bytes = Vec::new();
        io(input
            .take((RPC_INPUT_BYTES + 1) as u64)
            .read_to_end(&mut bytes))?;
        if bytes.len() > RPC_INPUT_BYTES {
            return Err("candidate-rpc-too-large");
        }
        let request: Value =
            serde_json::from_slice(&bytes).map_err(|_| "candidate-rpc-json-invalid")?;
        if matches!(method, "mq_status" | "mq_probe") {
            return Ok(crate::qdisc_capabilities::rpc(
                &request,
                method == "mq_probe",
            ));
        }
        if method == "schema" {
            return Ok(json!({"schema_version": 1, "ok": true, "result": {
                "protocol": "chunked-candidate-v1", "validation_scope": "controller-sqm",
                "variant": if cfg!(feature = "calibration") { "full" } else { "lite" },
                "max_input_bytes": config_candidate::MAX_INPUT_BYTES, "chunk_bytes": CHUNK_BYTES,
                "max_active": MAX_ACTIVE, "ttl_s": TTL_S,
                "max_window_samples": crate::config_validation::MAX_WINDOW_SAMPLES,
                "max_pingers": crate::config_validation::MAX_PINGERS,
                "max_timer_s": crate::config_validation::MAX_TIMER_S,
                "min_timer_s": crate::config_validation::MIN_TIMER_S,
                "max_rate_kbps": crate::rate_limits::MAX_RATE_KBPS
                ,"rate_rules": crate::config_validation::rate_schema()
                ,"fields": crate::config_fields::OPTIONS,
                "global_fields": ["graph_history_ram_budget_kib"],
                "min_history_budget_kib": crate::GRAPH_HISTORY_MIN_BUDGET_KIB,
                "max_history_budget_kib": crate::GRAPH_HISTORY_HARD_MAX_KIB
            }}));
        }
        let root = env::var_os("CAKE_AUTORATE_RUN_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/var/run/cake-autorate"))
            .join(".candidate-checks");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "candidate-clock-invalid")?
            .as_secs();
        dispatch(&root, now, method, &request)
    })();
    result.unwrap_or_else(|code| json!({"schema_version": 1, "ok": false, "code": code}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::sync::atomic::{AtomicU64, Ordering};
    const ID: &str = "0123456789abcdef0123456789abcdef";
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = env::temp_dir().join(format!(
                "cake-transfer-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            Self(root)
        }
        fn root(&self) -> PathBuf {
            self.0.join(".candidate-checks")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }
    fn body() -> Vec<u8> {
        json!({"schema_version": 1, "request_id": ID,
            "sections": [{"name": "test", "options": {"base_dl_shaper_rate_kbps": "30000"}}]})
        .to_string()
        .into_bytes()
    }
    #[test]
    fn r3_transfer_survives_process_boundaries_and_replayed_chunks() {
        let fixture = Fixture::new();
        let data = body();
        let token = {
            let store = Store::open(&fixture.root(), 100).unwrap();
            let result = store
                .begin(ID, data.len(), &config_candidate::digest(&data))
                .unwrap();
            assert_eq!(
                store
                    .begin(ID, data.len(), &config_candidate::digest(&data))
                    .unwrap(),
                result
            );
            result["token"].as_str().unwrap().to_string()
        };
        let store = Store::open(&fixture.root(), 101).unwrap();
        let first = store.append(&token, ID, 0, &hex(&data)).unwrap();
        assert_eq!(store.append(&token, ID, 0, &hex(&data)).unwrap(), first);
        assert!(store.append(&token, ID, 0, &hex(b"wrong")).is_err());
        assert_eq!(
            store.finish(&token, ID).unwrap()["validation"]["valid"],
            true
        );
        assert!(!store.path(&token).unwrap().exists());
        assert_eq!(store.cancel(&token, ID).unwrap()["cancelled"], true);
    }
    #[test]
    fn r3_transfer_resumes_partial_write_without_duplicating_bytes() {
        let fixture = Fixture::new();
        let store = Store::open(&fixture.root(), 100).unwrap();
        let data = body();
        let token = store
            .begin(ID, data.len(), &config_candidate::digest(&data))
            .unwrap()["token"]
            .as_str()
            .unwrap()
            .to_string();
        let mut file = open_file(&store.path(&token).unwrap(), false).unwrap();
        file.seek(SeekFrom::End(0)).unwrap();
        file.write_all(&data[..7]).unwrap();
        assert!(store.finish(&token, ID).is_err());
        assert_eq!(
            store.append(&token, ID, 0, &hex(&data)).unwrap()["next_offset"],
            data.len()
        );
        assert_eq!(
            store.finish(&token, ID).unwrap()["validation"]["valid"],
            true
        );
    }
    #[test]
    fn r3_transfer_quota_expiry_and_invalid_validation_keep_resources_bounded() {
        let fixture = Fixture::new();
        {
            let store = Store::open(&fixture.root(), 100).unwrap();
            for n in 0..MAX_ACTIVE {
                store
                    .begin(
                        &format!("{n:032x}"),
                        config_candidate::MAX_INPUT_BYTES,
                        &config_candidate::digest(b"x"),
                    )
                    .unwrap();
            }
            assert_eq!(
                store
                    .begin(ID, 100, &config_candidate::digest(b"x"))
                    .unwrap_err(),
                "candidate-storage-full"
            );
            assert!(Store::open(&fixture.root(), 100).is_err());
        }
        let store = Store::open(&fixture.root(), 100 + TTL_S + 1).unwrap();
        let token = store.begin(ID, 1, &config_candidate::digest(b"x")).unwrap()["token"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(store.records().unwrap().len(), 1);
        store.append(&token, ID, 0, "78").unwrap();
        assert_eq!(
            store.finish(&token, ID).unwrap()["validation"]["valid"],
            false
        );
        assert_eq!(fs::read_dir(fixture.root()).unwrap().count(), 1); // empty lock only
    }
    #[test]
    fn r3_transfer_rejects_paths_wrong_identity_offsets_and_oversized_chunks() {
        let fixture = Fixture::new();
        let store = Store::open(&fixture.root(), 100).unwrap();
        let token = store
            .begin(ID, 100, &config_candidate::digest(b"x"))
            .unwrap()["token"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(store.path("../../foreign").is_err());
        assert!(store
            .append(&token, "ffffffffffffffffffffffffffffffff", 0, "61")
            .is_err());
        assert!(store.append(&token, ID, 1, "61").is_err());
        assert!(store
            .append(&token, ID, 0, &"61".repeat(CHUNK_BYTES + 1))
            .is_err());
        assert!(store.append(&token, ID, 0, "éé").is_err());
        assert!(store
            .begin(ID, 101, &config_candidate::digest(b"x"))
            .is_err());
        assert!(store
            .begin(ID, 100, &config_candidate::digest(b"y"))
            .is_err());
        assert_eq!(
            fs::metadata(store.path(&token).unwrap()).unwrap().len(),
            HEADER as u64
        );
    }
    #[test]
    fn r3_transfer_preserves_foreign_files_and_unsafe_paths() {
        let fixture = Fixture::new();
        let foreign = fixture.0.join("foreign");
        fs::write(&foreign, "operator-private-value").unwrap();
        symlink(&fixture.0, fixture.root()).unwrap();
        assert!(Store::open(&fixture.root(), 100).is_err());
        fs::remove_file(fixture.root()).unwrap();
        let store = Store::open(&fixture.root(), 100).unwrap();
        let named = store.path(ID).unwrap();
        symlink(&foreign, &named).unwrap();
        assert!(store
            .begin(ID, 100, &config_candidate::digest(b"x"))
            .is_err());
        assert_eq!(
            fs::read_to_string(&foreign).unwrap(),
            "operator-private-value"
        );
        fs::remove_file(&named).unwrap();
        fs::write(&named, "foreign record").unwrap();
        fs::set_permissions(&named, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(store
            .begin(ID, 100, &config_candidate::digest(b"x"))
            .is_err());
        assert_eq!(fs::read_to_string(&named).unwrap(), "foreign record");
    }
    #[test]
    fn r3_transfer_terminal_receipt_cannot_claim_another_body_identity() {
        let fixture = Fixture::new();
        let store = Store::open(&fixture.root(), 100).unwrap();
        let data = body();
        let owner = "ffffffffffffffffffffffffffffffff";
        let token = store
            .begin(owner, data.len(), &config_candidate::digest(&data))
            .unwrap()["token"]
            .as_str()
            .unwrap()
            .to_string();
        store.append(&token, owner, 0, &hex(&data)).unwrap();
        let result = store.finish(&token, owner).unwrap();
        assert_eq!(
            result["validation"]["code"],
            "candidate-body-identity-invalid"
        );
        assert!(!store.path(&token).unwrap().exists());
    }

    #[test]
    fn r3_transfer_digest_rejects_a_different_complete_body() {
        let fixture = Fixture::new();
        let store = Store::open(&fixture.root(), 100).unwrap();
        let token = store.begin(ID, 1, &config_candidate::digest(b"x")).unwrap()["token"]
            .as_str()
            .unwrap()
            .to_string();
        store.append(&token, ID, 0, "79").unwrap();
        assert_eq!(
            store.finish(&token, ID).unwrap_err(),
            "candidate-digest-mismatch"
        );
        assert!(!store.path(&token).unwrap().exists());
    }

    #[test]
    fn r3_transfer_cancel_lost_begin_ack_uses_request_and_digest_without_restarting() {
        let fixture = Fixture::new();
        let store = Store::open(&fixture.root(), 100).unwrap();
        let digest = config_candidate::digest(b"x");
        store.begin(ID, 1, &digest).unwrap();
        assert!(store
            .cancel_request(ID, &config_candidate::digest(b"wrong"))
            .is_err());
        assert_eq!(store.records().unwrap().len(), 1);
        assert_eq!(
            store.cancel_request(ID, &digest).unwrap()["cancelled"],
            true
        );
        assert!(store.records().unwrap().is_empty());
    }

    #[test]
    fn r3_transfer_crash_header_is_retired_but_hardlinked_records_are_preserved() {
        let fixture = Fixture::new();
        let store = Store::open(&fixture.root(), 1000).unwrap();
        let path = store.path(ID).unwrap();
        let mut partial = open_file(&path, true).unwrap();
        partial.write_all(&MAGIC[..8]).unwrap();
        partial
            .set_times(
                fs::FileTimes::new().set_modified(UNIX_EPOCH + std::time::Duration::from_secs(1)),
            )
            .unwrap();
        assert!(store.records().unwrap().is_empty());
        assert!(!path.exists());
        let token = store.begin(ID, 1, &config_candidate::digest(b"x")).unwrap()["token"]
            .as_str()
            .unwrap()
            .to_string();
        let path = store.path(&token).unwrap();
        let foreign = fixture.0.join("operator-link");
        fs::hard_link(&path, &foreign).unwrap();
        assert!(store.cancel(&token, ID).is_err());
        assert!(path.exists() && foreign.exists());
    }
}
