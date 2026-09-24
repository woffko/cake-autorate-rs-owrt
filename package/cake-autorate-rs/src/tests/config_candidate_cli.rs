//! Actual stdin-only candidate CLI, including Lite. No router or external URL.
use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Stdio};

fn invoke(input: &[u8]) -> (Value, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_cake-autorated"))
        .arg("--validate-config-candidate")
        // A regression through live Config::from_uci/discovery must not work.
        .env("PATH", "/nonexistent-candidate-validation-path")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A limit rejection may close the pipe before an oversized body is written.
    let _ = child.stdin.take().unwrap().write_all(input);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    (
        serde_json::from_slice(&output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
fn r3_candidate_cli_validates_stdin_without_uci_or_url_discovery() {
    let mut candidate = json!({"schema_version": 1,
        "request_id": "0123456789abcdef0123456789abcdef", "sections": [{"name": "fixture",
            "options": {"min_dl_shaper_rate_kbps": "10000", "base_dl_shaper_rate_kbps": "30000",
                "reflectors_url": "https://never-contact.invalid/list", "mqtt_password": "fixture-private"}}]});
    let (good, stderr) = invoke(candidate.to_string().as_bytes());
    assert_eq!(good["valid"], true);
    assert_eq!(good["validation_scope"], "controller-sqm");
    assert!(stderr.is_empty());
    assert!(!good.to_string().contains("fixture-private"));
    candidate["sections"][0]["options"]["min_dl_shaper_rate_kbps"] = json!("40000");
    let (bad, stderr) = invoke(candidate.to_string().as_bytes());
    assert_eq!(bad["valid"], false);
    assert_eq!(bad["request_id"], candidate["request_id"]);
    assert!(stderr.is_empty());
}

#[test]
fn r3_candidate_cli_rejects_argv_payload_without_echoing_it() {
    let output = Command::new(env!("CARGO_BIN_EXE_cake-autorated"))
        .args(["--validate-config-candidate", "fixture-private-value"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("only on stdin"));
    assert!(!stderr.contains("fixture-private-value"));
}

#[test]
fn r3_candidate_cli_rejects_oversized_input_without_echoing_it() {
    let (result, stderr) = invoke(&vec![b'x'; 256 * 1024 + 1]);
    assert_eq!(result["code"], "candidate-too-large");
    assert!(stderr.is_empty());
}
