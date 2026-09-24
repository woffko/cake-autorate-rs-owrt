//! Whole reload algorithm with real UCI, owned processes and modeled kernel I/O.
//! This is not a daemon, VM or device acceptance test.
use super::*;
use std::process::Command;

const HELPER: &str = "operations::service_lifecycle::reload_integration::r4_reload_command_helper";

struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn quote(value: &std::ffi::OsStr) -> String {
    format!("'{}'", value.to_str().unwrap().replace('\'', "'\\''"))
}

#[test]
#[ignore = "requires explicit inspected SDK UCI and SQM runner paths"]
fn r4_reload_command_preserves_peer_queues_and_replays_registration() {
    let root = std::env::temp_dir().join(format!(
        "cake-reload-{}-{}",
        std::process::id(),
        REPLACE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).unwrap();
    let _cleanup = Cleanup(root.clone());
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests");
    let binary = root.join("controller");
    let compiled = Command::new("rustc")
        .args(["--edition=2021", "--crate-name", "reload_owned_controller"])
        .arg(fixtures.join("mqtt-pidfd-fixture.rs"))
        .arg("-o")
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "owned controller compilation failed"
    );
    let uci = std::env::var_os("CAKE_TEST_UCI").expect("explicit UCI required");
    let uci_command = match std::env::var_os("CAKE_TEST_MUSL_LOADER") {
        Some(loader) => format!(
            "{} --library-path {} {}",
            quote(&loader),
            quote(&std::env::var_os("CAKE_TEST_LIB_DIR").expect("explicit libraries required")),
            quote(&uci)
        ),
        None => quote(&uci),
    };
    let runner = std::env::var_os("CAKE_TEST_SQM_RUNNER").expect("explicit runner required");
    for mode in ["normal", "lost-add-ack", "startup-error"] {
        let case = root.join(mode);
        for directory in [
            "config", "pending", "run", "locks", "sys", "state", "proc", "nft",
        ] {
            fs::create_dir_all(case.join(directory)).unwrap();
        }
        fs::write(case.join("fixture.owner"), b"cake-reload-fixture-v1\n").unwrap();
        fs::write(
            case.join("nft/fixture.owner"),
            b"cake-nft-selected-fixture-v1\n",
        )
        .unwrap();
        fs::copy(&binary, case.join("controller")).unwrap();
        let boot = fs::read_to_string("/proc/stat").unwrap();
        let boot = boot
            .lines()
            .find(|line| line.starts_with("btime "))
            .unwrap();
        fs::write(case.join("proc/stat"), format!("{boot}\n")).unwrap();
        executable(&case.join("uci"), &format!(
            "#!/bin/sh\nfor last do :; done\ncase \"$last\" in cu??????????????????????????????) ;; *) exit 91;; esac\nexec {uci_command} -p {} \"$@\"\n",
            quote(case.join("pending").as_os_str())
        ));
        executable(
            &case.join("ubus"),
            &format!(
                "#!/bin/sh\nexec python3 {} client {} \"$@\"\n",
                quote(fixtures.join("reload-procd-fixture.py").as_os_str()),
                quote(case.as_os_str())
            ),
        );
        executable(
            &case.join("nft-bin"),
            &format!(
                "#!/bin/sh\nexec python3 {} {} \"$@\"\n",
                quote(fixtures.join("nft-selected-fixture.py").as_os_str()),
                quote(case.join("nft").as_os_str())
            ),
        );
        let mut tc = "#!/bin/sh\ncase \"$*\" in\n".to_string();
        let mut config = String::new();
        for (index, name) in [(0, "lab"), (1, "peer")] {
            let interface = format!("fixture{index}");
            let ifb = format!("ifb4{interface}");
            for (offset, device) in [(0, &interface), (1, &ifb)] {
                fs::create_dir_all(case.join("sys").join(device).join("statistics")).unwrap();
                fs::write(
                    case.join("sys").join(device).join("statistics/tx_bytes"),
                    b"0\n",
                )
                .unwrap();
                fs::write(
                    case.join("sys").join(device).join("ifindex"),
                    format!("{}\n", 10 + index * 2 + offset),
                )
                .unwrap();
            }
            config.push_str(&format!(
                "config cake_autorate '{name}'\n option enabled '1'\n option sqm_enabled '1'\n option wan_if '{interface}'\n option no_pingers '6'\n option sqm_download '20000'\n option sqm_upload '20000'\n option sqm_use_mq '0'\n option sqm_squash_dscp '0'\n option sqm_squash_ingress '0'\n option traffic_rules_enabled '0'\n option mqtt_enabled '0'\n"
            ));
            fs::write(case.join("state").join(format!("{interface}.state")), format!(
                "IFACE=\"{interface}\"\nQDISC=\"cake\"\nSCRIPT=\"piece_of_cake.qos\"\nUPLINK=\"20000\"\nDOWNLINK=\"20000\"\nLINKLAYER=\"none\"\nLLAM=\"default\"\nOVERHEAD=\"0\"\nSTAB_MPU=\"0\"\nUSE_MQ=\"0\"\nINGRESS_CAKE_OPTS=\"besteffort triple-isolate nat wash no-ack-filter split-gso\"\nEGRESS_CAKE_OPTS=\"diffserv4 triple-isolate nat nowash no-ack-filter split-gso\"\nIQDISC_OPTS=\"\"\nEQDISC_OPTS=\"\"\nZERO_DSCP_INGRESS=\"0\"\nIGNORE_DSCP_INGRESS=\"0\"\n"
            )).unwrap();
            tc.push_str(&format!(
                "'-details qdisc show dev {interface}'|'qdisc show dev {interface}') printf '%s\\n' 'qdisc cake 8001: root bandwidth 20Mbit diffserv4 triple-isolate nat nowash no-ack-filter split-gso raw overhead 0' 'qdisc ingress ffff: parent ffff:fff1' ;;\n'-details qdisc show dev {ifb}'|'qdisc show dev {ifb}') printf '%s\\n' 'qdisc cake 8002: root bandwidth 20Mbit besteffort triple-isolate nat wash no-ack-filter split-gso raw overhead 0' ;;\n'filter show dev {interface} ingress') printf '%s\\n' 'action order 1: mirred (Egress Redirect to device {ifb})' ;;\n"
            ));
        }
        tc.push_str("*) exit 94 ;;\nesac\n"); // Reject every mutating tc command.
        executable(&case.join("tc"), &tc);
        fs::write(case.join("config").join(CAKE_PACKAGE), config).unwrap();
        fs::write(case.join("config").join(SQM_PACKAGE), b"").unwrap();
        let mut command = Command::new("python3");
        command.env_remove("PKG_UPGRADE");
        command
            .arg(fixtures.join("reload-procd-fixture.py"))
            .arg("supervise")
            .arg(&case)
            .arg(std::env::current_exe().unwrap())
            .arg(HELPER);
        // Environment changes are confined to a fresh subprocess, never the
        // parallel Rust test runner. Redirect all independent runtime readers.
        for (name, _) in std::env::vars_os() {
            if name.as_bytes().starts_with(b"CAKE_AUTORATE_") {
                command.env_remove(name);
            }
        }
        for (variable, relative) in [
            ("CONFIG_DIR", "config"),
            ("UCI_BIN", "uci"),
            ("UCI", "uci"),
            ("TC_BIN", "tc"),
            ("TC", "tc"),
            ("UBUS_BIN", "ubus"),
            ("PROC_ROOT", "proc"),
            ("RUN_ROOT", "run"),
            ("RUNTIME_ROOT", "run"),
            ("RUNTIME_LOCK_ROOT", "locks"),
            ("SYS_CLASS_NET", "sys"),
            ("SQM_CONFIG_FILE", "config/sqm"),
            ("SQM_STATE_DIR", "state"),
            ("NFT_BIN", "nft-bin"),
            ("BRIDGER_INIT", "no-bridger"),
            ("BRIDGER_CONFIG", "no-bridger-config"),
            ("SERVICE_UCI_WORK_ROOT", "uci-work"),
            ("TRAFFIC_CLASSIFIER_STATE", "run/traffic-classifier.state"),
        ] {
            command.env(format!("CAKE_AUTORATE_{variable}"), case.join(relative));
        }
        let output = command
            .env("CAKE_AUTORATE_SQM_RUN", &runner)
            .env("CAKE_RELOAD_FIXTURE_ROOT", &case)
            .env("CAKE_RELOAD_FIXTURE_MODE", mode)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn r4_reload_command_helper() {
    let Some(root) = std::env::var_os("CAKE_RELOAD_FIXTURE_ROOT").map(PathBuf::from) else {
        return;
    };
    assert!(root.is_absolute());
    assert_eq!(
        fs::read(root.join("fixture.owner")).unwrap(),
        b"cake-reload-fixture-v1\n"
    );
    let context = LifecycleContext {
        paths: ServicePaths::production(),
        environment: OpenWrtEnvironment::production(),
        #[cfg(feature = "calibration")]
        mqtt_root: root.join("mqtt"),
        #[cfg(feature = "calibration")]
        native_recovery_root: root.join("native-recovery"),
    };
    let paths = &context.paths;
    assert_eq!(paths.config_root, root.join("config"));
    assert_eq!(paths.runtime_root, root.join("run"));
    let candidate = preflight_start_candidate(paths, &context.environment).unwrap();
    let start = StartPublication::publish(paths, candidate).unwrap();
    let batch = start.batch.clone();
    drop(start);
    batch.accept(|_| Ok(())).unwrap(); // Explicit synthetic initial acceptance.
    let request = |method: &str, body: serde_json::Value| {
        let output = Command::new(&paths.ubus)
            .args(["call", "fixture", method, &body.to_string()])
            .output()
            .unwrap();
        assert!(output.status.success(), "fixture {method} failed");
    };
    request("seed", serde_json::to_value(batch.generations()).unwrap());
    let identities = || {
        discover_controllers(&paths.proc_root)
            .unwrap()
            .into_iter()
            .map(|p| (p.instance, (p.pid, p.starttime_ticks)))
            .collect::<BTreeMap<_, _>>()
    };
    let before = identities();
    let queue_files = ["fixture0.state", "fixture1.state"].map(|name| {
        let path = root.join("state").join(name);
        (fs::metadata(&path).unwrap().ino(), fs::read(path).unwrap())
    });
    let config_path = paths.config_root.join(CAKE_PACKAGE);
    let config = fs::read_to_string(&config_path).unwrap();
    let changed = config.replacen("option no_pingers '6'", "option no_pingers '3'", 1);
    assert_ne!(changed, config);
    fs::write(&config_path, changed).unwrap();
    let result = execute_reload_in(&context);
    if std::env::var("CAKE_RELOAD_FIXTURE_MODE").unwrap() == "startup-error" {
        assert!(result.unwrap_err().contains("entered ERROR"));
        let pending = super::super::controller_input::load_batch(
            &paths.runtime_root.join(".controller-input"),
            &paths.config_root,
        )
        .unwrap()
        .unwrap();
        assert!(pending.pending_update().unwrap().is_some());
        request("repair", serde_json::json!({}));
        assert_eq!(
            execute_reload_in(&context).unwrap(),
            "service-reload-v1 ready\n"
        );
    } else {
        assert_eq!(result.unwrap(), "service-reload-v1 ready\n");
    }
    let after = identities();
    assert_eq!(after["peer"], before["peer"]);
    assert_ne!(after["lab"], before["lab"]);
    assert_eq!(
        execute_reload_in(&context).unwrap(),
        "service-reload-v1 unchanged\n"
    );
    assert_eq!(identities(), after);
    let actions: Vec<String> =
        serde_json::from_slice(&fs::read(root.join("actions.json")).unwrap()).unwrap();
    assert_eq!(actions, ["delete:lab", "add:lab"]);
    for (name, expected) in ["fixture0.state", "fixture1.state"]
        .into_iter()
        .zip(queue_files)
    {
        let path = root.join("state").join(name);
        assert_eq!(
            (fs::metadata(&path).unwrap().ino(), fs::read(path).unwrap()),
            expected
        );
    }
    let applied = super::super::controller_input::load_batch(
        &paths.runtime_root.join(".controller-input"),
        &paths.config_root,
    )
    .unwrap()
    .unwrap();
    applied.attest_settled().unwrap();
    assert_eq!(applied.generations()["peer"], batch.generations()["peer"]);
    assert_ne!(applied.generations()["lab"], batch.generations()["lab"]);
    assert!(!super::super::uci_transaction::recovery_pending(&paths.config_root).unwrap());
    assert!(!root.join("nft/kernel.json").exists());
    assert!(!root.join("mqtt").exists());
}
