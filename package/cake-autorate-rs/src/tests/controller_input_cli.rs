//! Actual binary consumer checks; no router, service or live qdisc operation.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const MAGIC: &[u8] = b"cake-autorate frozen controller input v1\n";
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn version(path: &Path) -> Value {
    let meta = fs::metadata(path).unwrap();
    json!({"dev":meta.dev(),"ino":meta.ino(),"len":meta.len(),"uid":meta.uid(),"gid":meta.gid(),"mode":meta.mode(),
        "mtime":meta.mtime(),"mtime_ns":meta.mtime_nsec(),"ctime":meta.ctime(),"ctime_ns":meta.ctime_nsec(),"sha256":digest(&fs::read(path).unwrap())})
}
struct Fixture {
    root: PathBuf,
    config: PathBuf,
    input: PathBuf,
    id: String,
}
impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root =
            std::env::temp_dir().join(format!("cake-r4-input-cli-{}-{nonce}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let config = root.join("config");
        let inputs = root.join(".controller-input");
        let bin = root.join("bin");
        for path in [&config, &inputs, &bin] {
            fs::DirBuilder::new().mode(0o700).create(path).unwrap();
        }
        fs::write(config.join("cake-autorate"), "config cake_autorate 'lab'\n option enabled '0'\n option manage_sqm '0'\n option sqm_enabled '0'\nconfig globals 'globals'\n option graph_history_ram_budget_kib '512'\n").unwrap();
        fs::write(config.join("sqm"), "# fixture queue package\n").unwrap();
        let marker = root
            .join("legacy-uci-called")
            .to_str()
            .unwrap()
            .replace('\'', "'\\''");
        fs::write(
            bin.join("uci"),
            format!("#!/bin/sh\n: > '{marker}'\nexit 71\n"),
        )
        .unwrap();
        fs::set_permissions(bin.join("uci"), fs::Permissions::from_mode(0o700)).unwrap();
        let mut value = json!({"schema":1,"instance":"lab","nonce":format!("{nonce:032x}"),
            "show":"cake-autorate.lab=cake_autorate\ncake-autorate.lab.enabled='0'\ncake-autorate.lab.manage_sqm='0'\ncake-autorate.lab.sqm_enabled='0'\n",
            "history_budget_kib":512,"history_instances":1,
            "sources":{"schema":1,"versions":[version(&config.join("cake-autorate")),version(&config.join("sqm"))]}});
        let mut sealed = MAGIC.to_vec();
        sealed.extend(serde_json::to_vec(&value).unwrap());
        let id = digest(&sealed);
        value["generation"] = json!(id);
        let mut bytes = MAGIC.to_vec();
        bytes.extend(serde_json::to_vec(&value).unwrap());
        let input = inputs.join(format!("lab.{id}"));
        fs::write(&input, bytes).unwrap();
        fs::set_permissions(&input, fs::Permissions::from_mode(0o600)).unwrap();
        Self {
            root,
            config,
            input,
            id,
        }
    }
    fn run(&self, instance: &str, id: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_cake-autorated"))
            .args(["--instance", instance, "--dump-config"])
            .env("CAKE_AUTORATE_SERVICE_CONFIG_ID", id)
            .env("CAKE_AUTORATE_RUNTIME_ROOT", &self.root)
            .env("CAKE_AUTORATE_CONFIG_DIR", &self.config)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.root.join("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .output()
            .unwrap()
    }
    fn assert_no_legacy(&self) {
        assert!(!self.root.join("legacy-uci-called").exists());
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn r4_controller_cli_loads_sealed_input_without_any_original_name_uci_query() {
    let fixture = Fixture::new();
    let result = fixture.run("lab", &fixture.id);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output = String::from_utf8(result.stdout).unwrap();
    assert!(output.contains("instance: \"lab\""));
    assert!(output.contains("enabled: false"));
    assert!(output.contains("graph_history_ram_budget_kib: Some("));
    assert!(output.contains("512"));
    fixture.assert_no_legacy();
}

#[test]
fn r4_controller_cli_bad_missing_or_stale_input_never_falls_back_to_live_uci() {
    for case in 0..5 {
        let fixture = Fixture::new();
        let id = if case == 0 {
            "invalid-private-generation".into()
        } else if case == 1 {
            "0".repeat(64)
        } else {
            fixture.id.clone()
        };
        match case {
            2 => {
                fs::write(&fixture.input, b"private fixture corruption").unwrap();
            }
            3 => {
                fs::write(
                    fixture.config.join("sqm"),
                    b"# concurrent committed change\n",
                )
                .unwrap();
            }
            4 => {
                fs::set_permissions(&fixture.input, fs::Permissions::from_mode(0o644)).unwrap();
            }
            _ => {}
        }
        let result = fixture.run("lab", &id);
        assert!(!result.status.success(), "case {case} accepted");
        assert!(result.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&result.stderr).contains("private fixture"));
        fixture.assert_no_legacy();
    }
}

#[test]
fn r4_controller_cli_accepted_generation_respawns_without_adopting_a_later_commit() {
    let fixture = Fixture::new();
    let marker = fixture
        .root
        .join(".controller-input")
        .join(format!("lab.{}.accepted", fixture.id));
    fs::write(
        &marker,
        format!(
            "accepted-controller-input-v1 {} {}\n",
            fixture.id,
            digest(&fs::read(&fixture.input).unwrap())
        ),
    )
    .unwrap();
    fs::set_permissions(marker, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(
        fixture.config.join("cake-autorate"),
        "config cake_autorate 'lab'\n option enabled '1'\n",
    )
    .unwrap();
    let result = fixture.run("lab", &fixture.id);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8(result.stdout)
        .unwrap()
        .contains("enabled: false"));
    fixture.assert_no_legacy();
}
