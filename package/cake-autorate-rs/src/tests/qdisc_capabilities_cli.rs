//! Real native dispatch/process/file boundaries. The only executables used as
//! ip/tc are fixture scripts which operate on a private fake sysfs directory.
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "cake-mq-cli-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(root.join("sys")).unwrap();
        fs::create_dir(root.join("sqm")).unwrap();
        fs::write(
            root.join("sqm/piece_of_cake.qos"),
            "SUPPORT_MQ=1\nQDISC=cake\n",
        )
        .unwrap();
        fs::write(
            root.join("sqm/unsupported.qos"),
            "SUPPORT_MQ=0\nQDISC=cake\n",
        )
        .unwrap();
        fs::write(root.join("ip-fixture"), r#"#!/bin/sh
set -eu
sys="${0%/*}/sys"
case "$1:$2" in
link:add)
  [ "$#" = 12 ] && [ "$3" = name ] && [ "$5:$6:$7:$8:$9" = numtxqueues:2:numrxqueues:2:alias ] && [ "${11}:${12}" = type:ifb ]
  case "$4" in cqm????????????) ;; *) exit 41;; esac
  case "${10}" in cake-autorate-mq-v1:????????????????????????????????) ;; *) exit 42;; esac
  mkdir "$sys/$4"
  printf '%s\n' "${10}" > "$sys/$4/ifalias"
  printf '42\n' > "$sys/$4/ifindex"
  printf 'create\n' >> "$sys/events"
  ;;
-details:link)
  [ "$3:$4" = show:dev ] && [ -d "$sys/$5" ]
  printf '42: fixture: ifb\n'
  ;;
link:delete)
  [ "$#" = 6 ] && [ "$3:$5:$6" = dev:type:ifb ]
  [ ! -f "$sys/fail-delete" ] || exit 43
  case "$4" in cqm????????????) ;; *) exit 44;; esac
  rm "$sys/$4/ifalias" "$sys/$4/ifindex"
  rmdir "$sys/$4"
  printf 'delete\n' >> "$sys/events"
  ;;
*) exit 45;;
esac
"#).unwrap();
        fs::write(
            root.join("tc-fixture"),
            r#"#!/bin/sh
set -eu
sys="${0%/*}/sys"
[ -d "$sys/$4" ]
case "$1:$2:$3" in
qdisc:replace:dev)
  [ "$#" = 9 ] && [ "$5:$6:$7:$8:$9" = root:cake_mq:bandwidth:1000kbit:besteffort ]
  [ ! -f "$sys/unsupported" ] || { printf 'private fixture failure\n' >&2; exit 51; }
  printf 'test\n' >> "$sys/events"
  ;;
qdisc:show:dev) printf 'qdisc cake_mq 1: root bandwidth 1Mbit\n';;
*) exit 52;;
esac
"#,
        )
        .unwrap();
        for name in ["ip-fixture", "tc-fixture"] {
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o700)).unwrap();
        }
        symlink(
            env!("CARGO_BIN_EXE_cake-autorated"),
            root.join("cake-autorate-config"),
        )
        .unwrap();
        Self { root }
    }
    fn call(&self, method: &str, script: &str) -> Value {
        let mut child = Command::new(self.root.join("cake-autorate-config"))
            .args(["call", method])
            .env("CAKE_AUTORATE_RUN_ROOT", self.root.join("run"))
            .env("CAKE_AUTORATE_SQM_LIB_DIR", self.root.join("sqm"))
            .env("CAKE_AUTORATE_SYS_CLASS_NET", self.root.join("sys"))
            .env("CAKE_AUTORATE_IP", self.root.join("ip-fixture"))
            .env("CAKE_AUTORATE_TC", self.root.join("tc-fixture"))
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(json!({"script": script}).to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(!result.to_string().contains("private fixture"));
        result
    }
    fn links(&self) -> usize {
        fs::read_dir(self.root.join("sys"))
            .unwrap()
            .filter(|entry| entry.as_ref().unwrap().file_type().unwrap().is_dir())
            .count()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn r3_mq_cli_status_is_read_only_and_explicit_probe_attests_fake_kernel_tc_script() {
    let fixture = Fixture::new();
    let result = fixture.call("mq_status", "piece_of_cake.qos");
    assert_eq!(result["ok"], true);
    assert_eq!(result["result"]["supported"], false);
    assert!(!fixture.root.join("run").exists());
    assert!(!fixture.root.join("sys/events").exists());
    assert_eq!(fixture.call("mq_probe", "unsupported.qos")["ok"], false);
    assert!(!fixture.root.join("run").exists());
    let result = fixture.call("mq_probe", "piece_of_cake.qos");
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["result"]["supported"], true);
    assert_eq!(fixture.links(), 0);
    assert_eq!(
        fs::read_to_string(fixture.root.join("sys/events")).unwrap(),
        "create\ntest\ndelete\n"
    );
    assert_eq!(
        fixture.call("mq_status", "piece_of_cake.qos")["result"]["supported"],
        true
    );
    assert_eq!(
        fs::read_to_string(fixture.root.join("sys/events")).unwrap(),
        "create\ntest\ndelete\n",
        "status must not invoke ip/tc"
    );
    assert_eq!(
        fixture.call("mq_status", "unsupported.qos")["result"]["supported"],
        false
    );
    let tc = fixture.root.join("tc-fixture");
    let mut source = fs::read_to_string(&tc).unwrap();
    source.push_str("\n# updated binary fixture\n");
    fs::write(tc, source).unwrap();
    assert_eq!(
        fixture.call("mq_status", "piece_of_cake.qos")["result"]["supported"],
        false
    );
    assert_eq!(fixture.call("mq_probe", "../escape.qos")["ok"], false);
}

#[test]
fn r3_mq_cli_cleanup_failure_survives_process_exit_and_next_probe_recovers_once() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("sys/fail-delete"), b"1").unwrap();
    let result = fixture.call("mq_probe", "piece_of_cake.qos");
    assert_eq!(result["ok"], false);
    assert_eq!(fixture.links(), 1);
    assert_eq!(
        fixture.call("mq_status", "piece_of_cake.qos")["result"]["supported"],
        false
    );
    assert_eq!(fixture.call("mq_probe", "piece_of_cake.qos")["ok"], false);
    assert_eq!(
        fixture.links(),
        1,
        "no duplicate probe while cleanup is blocked"
    );
    fs::remove_file(fixture.root.join("sys/fail-delete")).unwrap();
    assert_eq!(
        fixture.call("mq_probe", "piece_of_cake.qos")["result"]["supported"],
        true
    );
    assert_eq!(fixture.links(), 0);
    assert_eq!(
        fs::read_to_string(fixture.root.join("sys/events")).unwrap(),
        "create\ntest\ndelete\ncreate\ntest\ndelete\n"
    );
    assert!(!fixture
        .root
        .join("run/.qdisc-capabilities/pending.json")
        .exists());
}

#[test]
fn r3_mq_cli_unsupported_kernel_does_not_echo_child_stderr_and_cleans_up() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("sys/unsupported"), b"1").unwrap();
    let result = fixture.call("mq_probe", "piece_of_cake.qos");
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["result"]["supported"], false);
    assert_eq!(fixture.links(), 0);
    assert_eq!(
        fixture.call("mq_status", "piece_of_cake.qos")["result"]["supported"],
        false
    );
}
