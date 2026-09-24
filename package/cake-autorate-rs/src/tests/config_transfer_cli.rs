//! Actual rpcd-style process boundaries with sub-PIPE_BUF JSON requests.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const ID: &str = "0123456789abcdef0123456789abcdef";
struct Fixture {
    root: PathBuf,
    plugin: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "cake-transfer-cli-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let plugin = root.join("cake-autorate-config");
        symlink(env!("CARGO_BIN_EXE_cake-autorated"), &plugin).unwrap();
        Self { root, plugin }
    }
    fn call(&self, method: &str, input: Value) -> Value {
        let bytes = input.to_string().into_bytes();
        assert!(
            bytes.len() < 3000,
            "every RPC request must fit a small pipe including rpcd framing"
        );
        let mut child = Command::new(&self.plugin)
            .args(["call", method])
            .env("CAKE_AUTORATE_RUN_ROOT", self.root.join("run"))
            .env("PATH", "/nonexistent-candidate-validation-path")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&bytes).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        serde_json::from_slice(&output.stdout).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn r3_transfer_cli_large_candidate_is_complete_bound_private_and_removed() {
    let f = Fixture::new();
    let pending = f.root.join("foreign-pending");
    fs::write(&pending, b"unchanged foreign UCI candidate\n").unwrap();
    let mut options = serde_json::Map::new();
    for index in 0..20 {
        options.insert(
            format!("retained_{index}"),
            json!("private-fixture-ж".repeat(50)),
        );
    }
    options.insert("base_dl_shaper_rate_kbps".to_string(), json!("30000"));
    options.insert("min_dl_shaper_rate_kbps".to_string(), json!("40000"));
    let body = json!({"schema_version": 1, "request_id": ID,
        "sections": [{"name": "fixture", "options": options}]})
    .to_string()
    .into_bytes();
    assert!(body.len() > 8192);
    let digest = format!("{:x}", Sha256::digest(&body));
    let begin = json!({"request_id": ID, "length": body.len(), "sha256": digest});
    let started = f.call("begin", begin.clone());
    assert_eq!(started["ok"], true);
    assert_eq!(
        f.call("begin", begin),
        started,
        "a lost begin ACK must not allocate another transaction"
    );
    let token = started["result"]["token"].as_str().unwrap();
    let store = f.root.join("run/.candidate-checks");
    assert_eq!(
        fs::metadata(&store).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let staged = store.join(format!("{token}.candidate"));
    assert_eq!(
        fs::metadata(&staged).unwrap().permissions().mode() & 0o777,
        0o600
    );
    for (index, bytes) in body.chunks(1024).enumerate() {
        let data: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let request =
            json!({"request_id": ID, "token": token, "offset": index * 1024, "data": data});
        let reply = f.call("append", request.clone());
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["result"]["next_offset"], index * 1024 + bytes.len());
        if index == 0 {
            assert_eq!(f.call("append", request), reply);
        }
    }
    let complete = f.call("finish", json!({"request_id": ID, "token": token}));
    assert_eq!(complete["ok"], true);
    assert_eq!(complete["result"]["validation"]["valid"], false);
    assert_eq!(complete["result"]["validation"]["candidate_sha256"], digest);
    assert_eq!(complete["result"]["validation"]["request_id"], ID);
    assert!(!complete.to_string().contains("private-fixture"));
    assert!(!staged.exists());
    assert_eq!(
        fs::read(&pending).unwrap(),
        b"unchanged foreign UCI candidate\n"
    );
}

#[test]
fn r3_transfer_cli_plugin_signature_and_schema_have_no_storage_side_effects() {
    let f = Fixture::new();
    let output = Command::new(&f.plugin).arg("list").output().unwrap();
    assert!(output.status.success());
    let signature: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(signature["begin"]["sha256"], "");
    assert_eq!(signature["append"]["offset"], 0);
    let schema = f.call("schema", json!({}));
    assert_eq!(schema["result"]["protocol"], "chunked-candidate-v1");
    assert_eq!(schema["result"]["validation_scope"], "controller-sqm");
    assert!(!f.root.join("run").exists());
    assert_eq!(
        f.call("--service-lifecycle", json!({}))["code"],
        "candidate-method-invalid"
    );
    assert!(!f.root.join("run").exists());
}
