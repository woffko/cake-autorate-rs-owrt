//! Selected MQTT runtime, retaining exact peer processes and procd definitions.
//! Uses the caller's global lifecycle lock and original source proof.

use super::*;
use crate::operations::mqtt_publisher::selected_plan::SelectedPlan;
use crate::operations::mqtt_publisher::start_notify::{Startup, ENV as STARTUP_ENV};

#[derive(PartialEq, Eq)]
struct Runtime {
    processes: Vec<ControllerIdentity>,
    definitions: BTreeMap<String, serde_json::Value>,
}

impl Runtime {
    fn capture(proc_root: &Path, ubus: &Path, selected: &str) -> Result<Self, String> {
        let processes = discover_controllers(proc_root)?
            .into_iter()
            .filter(|identity| identity.kind == ServiceProcessKind::MqttPublisher)
            .collect();
        let definitions = super::super::procd_control::mqtt_instances(ubus, selected)?;
        Ok(Self {
            processes,
            definitions,
        })
    }

    fn ready(&self, instance: &str) -> bool {
        let Some(definition) = self.definitions.get(instance) else {
            return false;
        };
        let Some(process) = self
            .processes
            .iter()
            .find(|identity| identity.instance == instance)
        else {
            return false;
        };
        definition.get("running").and_then(|value| value.as_bool()) == Some(true)
            && definition.get("pid").and_then(|value| value.as_u64()) == Some(process.pid.into())
    }

    fn absent(&self, instance: &str) -> bool {
        !self.definitions.contains_key(instance)
            && !self
                .processes
                .iter()
                .any(|identity| identity.instance == instance)
    }

    fn peers_equal(&self, other: &Self, instance: &str) -> bool {
        self.processes
            .iter()
            .filter(|identity| identity.instance != instance)
            .eq(other
                .processes
                .iter()
                .filter(|identity| identity.instance != instance))
            && self
                .definitions
                .iter()
                .filter(|(name, _)| name.as_str() != instance)
                .eq(other
                    .definitions
                    .iter()
                    .filter(|(name, _)| name.as_str() != instance))
    }
}

pub(crate) struct SelectedMqtt {
    instance: String,
    proc_root: PathBuf,
    ubus: PathBuf,
    plans: SelectedPlan,
    before: Runtime,
    restart: bool,
    stopped: bool,
    startup: Option<Startup>,
}

impl SelectedMqtt {
    pub(crate) fn prepare(
        proc_root: &Path,
        ubus: &Path,
        plan_root: &Path,
        instance: &str,
        cake: &UciPackage,
        controller_should_run: bool,
        job: &str,
    ) -> Result<Self, String> {
        let plans = SelectedPlan::prepare(plan_root, instance, cake, controller_should_run, job)?;
        let before = Runtime::capture(proc_root, ubus, instance)?;
        let restart = plans.changed()
            || plans.needs_recovery()
            || if plans.enabled() {
                !before.ready(instance)
            } else {
                !before.absent(instance)
            };
        plans.attest()?;
        let startup = if restart && plans.enabled() {
            Some(Startup::new()?)
        } else {
            None
        };
        Ok(Self {
            instance: instance.into(),
            proc_root: proc_root.into(),
            ubus: ubus.into(),
            plans,
            before,
            restart,
            stopped: false,
            startup,
        })
    }

    pub(crate) fn attest(&self) -> Result<(), String> {
        self.plans.attest()?;
        let current = Runtime::capture(&self.proc_root, &self.ubus, &self.instance)?;
        if !current.peers_equal(&self.before, &self.instance) {
            return Err("selected MQTT peer runtime changed".into());
        }
        if self.stopped {
            if !current.absent(&self.instance) {
                return Err("selected MQTT publisher returned after Stop".into());
            }
        } else if current != self.before {
            return Err("selected MQTT runtime changed before cutover".into());
        }
        Ok(())
    }

    pub(crate) fn stop_changed(
        &mut self,
        mut source: impl FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        source()?;
        self.attest()?;
        if !self.restart {
            return Ok(());
        }
        // Reuse the existing pidfd boundary; never send a broad process signal.
        let pidfds = capture_controller_pidfds(
            &self.proc_root,
            Some((ServiceProcessKind::MqttPublisher, &self.instance)),
        )?;
        source()?;
        self.attest()?;
        let request = serde_json::json!({"name": SERVICE_NAME, "instance": format!("mqtt_{}", self.instance)}).to_string();
        super::super::procd_control::delete_service_or_attest_absent(
            &self.ubus,
            &request,
            "delete selected MQTT publisher",
        )?;
        wait_controller_pidfds(pidfds, CONTROLLER_STOP_TIMEOUT)?;
        self.stopped = true;
        source()?;
        self.attest()
    }

    pub(crate) fn finish(
        &mut self,
        mut source: impl FnMut() -> Result<(), String>,
        register: impl FnOnce(&str, &str) -> Result<(), String>,
    ) -> Result<(), String> {
        source()?;
        self.attest()?;
        if !self.restart {
            return Ok(());
        }
        if !self.stopped {
            return Err("selected MQTT publication requires Stop".into());
        }
        let proof = || -> Result<(), String> {
            source()?;
            let current = Runtime::capture(&self.proc_root, &self.ubus, &self.instance)?;
            if !current.absent(&self.instance) || !current.peers_equal(&self.before, &self.instance)
            {
                return Err("selected MQTT Stop or peer proof changed".into());
            }
            Ok(())
        };
        self.plans.publish(proof)?;
        source()?;
        self.attest()?;
        if self.plans.enabled() {
            let startup = self
                .startup
                .as_ref()
                .ok_or("MQTT startup receiver was not prepared")?;
            let digest = self
                .plans
                .startup_digest()
                .ok_or("MQTT startup plan missing")?;
            // One registration only. A lost ACK may be resolved by the exact
            // new-process receipt and procd/source proof, never by resubmission.
            let registration = register(&self.instance, startup.name());
            let mut accepted_definition = None;
            let receipt = startup
                .wait(digest, Instant::now() + Duration::from_secs(10), || {
                    source()?;
                    self.plans.attest()?;
                    let current = Runtime::capture(&self.proc_root, &self.ubus, &self.instance)?;
                    if !current.peers_equal(&self.before, &self.instance) {
                        return Err("selected MQTT peer runtime changed during startup".into());
                    }
                    if let Some(definition) = current.definitions.get(&self.instance) {
                        if definition
                            .get("env")
                            .and_then(|env| env.get(STARTUP_ENV))
                            .and_then(|value| value.as_str())
                            != Some(startup.name())
                        {
                            return Err("selected MQTT startup registration binding changed".into());
                        }
                    }
                    if !current.ready(&self.instance) {
                        return Ok(None);
                    }
                    let process = current
                        .processes
                        .iter()
                        .find(|process| process.instance == self.instance)
                        .ok_or("MQTT startup process disappeared")?;
                    accepted_definition = current.definitions.get(&self.instance).cloned();
                    Ok(Some((process.pid, process.starttime_ticks)))
                })
                .map_err(|error| {
                    if let Err(registration) = &registration {
                        format!("{registration}; {error}")
                    } else {
                        error
                    }
                })?;
            let current = Runtime::capture(&self.proc_root, &self.ubus, &self.instance)?;
            if !current.ready(&self.instance)
                || !current.peers_equal(&self.before, &self.instance)
                || current.definitions.get(&self.instance) != accepted_definition.as_ref()
                || !current.processes.iter().any(|process| {
                    process.instance == self.instance
                        && process.pid == receipt.pid
                        && process.starttime_ticks == receipt.starttime
                })
            {
                return Err("selected MQTT registration is not ready or changed peers".into());
            }
            self.before = current;
            self.stopped = false;
        }
        source()?;
        self.attest()
    }
}

#[cfg(test)]
mod tests {
    const JOB: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    use super::*;
    use crate::operations::mqtt_publisher::{
        publish_service_plans,
        tests::{section, temp_root},
    };
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::{Child, Command, Stdio};

    struct OwnedChildren(Vec<Child>);
    impl Drop for OwnedChildren {
        fn drop(&mut self) {
            for child in &mut self.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    #[test]
    fn r4_selected_mqtt_pidfd_stop_terminates_only_owned_child_and_recovers_lost_delete_ack() {
        let root = temp_root("real-pidfd");
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let binary = root.join("publisher");
        let compilation = Command::new("rustc")
            .args(["--edition=2021", "--crate-name", "mqtt_pidfd_fixture"])
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/mqtt-pidfd-fixture.rs"))
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(compilation.status.success(), "fixture compilation failed");

        for lost_ack in [false, true] {
            let case = root.join(if lost_ack { "lost" } else { "ack" });
            fs::create_dir(&case).unwrap();
            let proc_root = case.join("proc");
            fs::create_dir(&proc_root).unwrap();
            let mut children = OwnedChildren(Vec::new());
            for instance in ["lab", "peer"] {
                let child = Command::new(&binary)
                    .arg0(DAEMON_PATH)
                    .args(["--mqtt-publisher", instance])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap();
                let pid = child.id();
                children.0.push(child);
                // Restrict discovery to our two children while reading real
                // kernel cmdline/stat, not a handwritten process identity.
                std::os::unix::fs::symlink(format!("/proc/{pid}"), proc_root.join(pid.to_string()))
                    .unwrap();
            }
            let selected_pid = children.0[0].id();
            let peer_pid = children.0[1].id();
            let plans = case.join("plans");
            let cake = UciPackage {
                sections: BTreeMap::from([
                    ("lab".into(), section(&[])),
                    ("peer".into(), section(&[])),
                ]),
            };
            publish_service_plans(&cake, &plans).unwrap();
            let peer_file = plans.join("peer.plan");
            let peer_metadata = fs::metadata(&peer_file).unwrap();
            let peer_bytes = fs::read(&peer_file).unwrap();
            let response = case.join("response");
            let after_stop = serde_json::json!({"cake-autorate":{"instances":{"mqtt_peer":definition("peer", Some(peer_pid))}}});
            let mut initial = after_stop.clone();
            initial["cake-autorate"]["instances"]["mqtt_lab"] =
                definition("lab", Some(selected_pid));
            fs::write(&response, serde_json::to_vec(&initial).unwrap()).unwrap();
            fs::write(
                case.join("after-stop"),
                serde_json::to_vec(&after_stop).unwrap(),
            )
            .unwrap();
            let ubus = case.join("ubus");
            let request =
                serde_json::json!({"name":SERVICE_NAME,"instance":"mqtt_lab"}).to_string();
            // selected_pid cannot be reused: Child remains owned and unreaped
            // until after stop_changed returns. The stub accepts only the
            // exact selected delete and never signals a peer or PID from input.
            fs::write(&ubus, format!("#!/bin/sh\ncase \"$3\" in\nlist) cat '{}' ;;\ndelete) [ \"$4\" = '{}' ] || exit 99\n[ ! -f '{}' ] || exit 252\nkill -TERM {} || exit 98\nprintf x > '{}'\ncp '{}' '{}'\nexit {} ;;\n*) exit 97;;\nesac\n",
                response.display(), request, case.join("signalled").display(), selected_pid,
                case.join("signalled").display(), case.join("after-stop").display(), response.display(), if lost_ack { 1 } else { 0 })).unwrap();
            fs::set_permissions(&ubus, fs::Permissions::from_mode(0o700)).unwrap();
            let before = Runtime::capture(&proc_root, &ubus, "lab").unwrap();
            assert!(before.ready("lab") && before.ready("peer"));
            let mut unchanged =
                SelectedMqtt::prepare(&proc_root, &ubus, &plans, "lab", &cake, true, JOB).unwrap();
            unchanged.stop_changed(|| Ok(())).unwrap();
            unchanged
                .finish(
                    || Ok(()),
                    |_, _| panic!("unchanged live publisher was restarted"),
                )
                .unwrap();
            assert!(!case.join("signalled").exists());
            let mut selected =
                SelectedMqtt::prepare(&proc_root, &ubus, &plans, "lab", &cake, false, JOB).unwrap();
            assert_eq!(
                selected
                    .stop_changed(|| Err("source-drift".into()))
                    .unwrap_err(),
                "source-drift"
            );
            assert!(!case.join("signalled").exists());
            let stopped = selected.stop_changed(|| Ok(()));
            if lost_ack {
                assert!(stopped.is_err());
                assert!(plans.join("lab.plan").exists());
                // The next owner observes the prior terminal effect; no
                // signal retry is made by the failed operation itself.
                selected =
                    SelectedMqtt::prepare(&proc_root, &ubus, &plans, "lab", &cake, false, JOB)
                        .unwrap();
                selected.stop_changed(|| Ok(())).unwrap();
            } else {
                stopped.unwrap();
            }
            selected
                .finish(|| Ok(()), |_, _| panic!("Stop registered a publisher"))
                .unwrap();
            let exit = children.0[0].wait().unwrap();
            assert_eq!(exit.signal(), Some(libc::SIGTERM));
            assert!(children.0[1].try_wait().unwrap().is_none());
            let after = Runtime::capture(&proc_root, &ubus, "lab").unwrap();
            assert!(after.absent("lab") && after.ready("peer"));
            assert!(after.peers_equal(&before, "lab"));
            assert!(!plans.join("lab.plan").exists());
            assert_eq!(peer_bytes, fs::read(&peer_file).unwrap());
            let current = fs::metadata(&peer_file).unwrap();
            assert_eq!(
                (
                    peer_metadata.dev(),
                    peer_metadata.ino(),
                    peer_metadata.mtime_nsec()
                ),
                (current.dev(), current.ino(), current.mtime_nsec())
            );
        }
    }

    fn fake_process(root: &Path, instance: &str, pid: u32) {
        let directory = root.join(pid.to_string());
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join("cmdline"),
            format!("/usr/sbin/cake-autorated\0--mqtt-publisher\0{instance}\0"),
        )
        .unwrap();
        let mut fields = vec!["0".to_string(); 20];
        fields[0] = "S".into();
        fields[1] = "1".into();
        fields[2] = pid.to_string();
        fields[19] = "17".into();
        fs::write(
            directory.join("stat"),
            format!("{pid} (cake) {}", fields.join(" ")),
        )
        .unwrap();
    }

    fn definition(instance: &str, pid: Option<u32>) -> serde_json::Value {
        serde_json::json!({"command":["/usr/sbin/cake-autorated", "--mqtt-publisher", instance], "running":pid.is_some(), "pid":pid})
    }

    #[test]
    fn r4_selected_mqtt_reconciles_dormant_registration_without_opening_peer_pidfds() {
        let root = temp_root("selected-runtime");
        let proc_root = root.join("proc");
        fs::create_dir(&proc_root).unwrap();
        // Deliberately nonexistent PID: touching peer pidfds would fail.
        fake_process(&proc_root, "peer", 4_000_000_000);
        let plans = root.join("plans");
        let mut cake = UciPackage {
            sections: BTreeMap::from([("lab".into(), section(&[])), ("peer".into(), section(&[]))]),
        };
        publish_service_plans(&cake, &plans).unwrap();
        let peer_path = plans.join("peer.plan");
        let peer_before = fs::metadata(&peer_path).unwrap();
        let peer_bytes = fs::read(&peer_path).unwrap();
        let response = root.join("response");
        let stopped_response = serde_json::json!({"cake-autorate":{"instances":{"mqtt_peer":definition("peer", Some(4_000_000_000))}}});
        let mut initial = stopped_response.clone();
        initial["cake-autorate"]["instances"]["mqtt_lab"] = definition("lab", None);
        fs::write(&response, serde_json::to_vec(&initial).unwrap()).unwrap();
        fs::write(
            root.join("stopped"),
            serde_json::to_vec(&stopped_response).unwrap(),
        )
        .unwrap();
        let ubus = root.join("ubus");
        let request = serde_json::json!({"name":SERVICE_NAME,"instance":"mqtt_lab"}).to_string();
        fs::write(&ubus, format!("#!/bin/sh\ncase \"$3\" in\nlist) cat '{}';;\ndelete) [ \"$4\" = '{}' ] || exit 99\ncp '{}' '{}'\nprintf x >> '{}' ;;\n*) exit 98;;\nesac\n", response.display(), request, root.join("stopped").display(), response.display(), root.join("deletes").display())).unwrap();
        fs::set_permissions(&ubus, fs::Permissions::from_mode(0o700)).unwrap();
        cake.sections
            .get_mut("lab")
            .unwrap()
            .options
            .insert("mqtt_min_interval_s".into(), "3".into());
        cake.sections
            .get_mut("peer")
            .unwrap()
            .options
            .insert("mqtt_port".into(), "invalid-unapplied".into());
        let mut selected =
            SelectedMqtt::prepare(&proc_root, &ubus, &plans, "lab", &cake, true, JOB).unwrap();
        assert!(selected.restart);
        selected.stop_changed(|| Ok(())).unwrap();
        let mut registrations = 0;
        selected
            .finish(
                || Ok(()),
                |instance, endpoint| {
                    registrations += 1;
                    assert_eq!(instance, "lab");
                    fake_process(&proc_root, instance, 4_000_000_001);
                    let mut registered = stopped_response.clone();
                    registered["cake-autorate"]["instances"]["mqtt_lab"] =
                        definition("lab", Some(4_000_000_001));
                    registered["cake-autorate"]["instances"]["mqtt_lab"]["env"] =
                        serde_json::json!({STARTUP_ENV:endpoint});
                    fs::write(&response, serde_json::to_vec(&registered).unwrap()).unwrap();
                    let config =
                        crate::operations::mqtt_publisher::MqttPublisherConfig::from_section(
                            "lab",
                            &cake.sections["lab"],
                        )
                        .unwrap()
                        .unwrap();
                    let process = crate::operations::identity::ProcessIdentity {
                        pid: 4_000_000_001,
                        process_group: 4_000_000_001,
                        starttime_ticks: 17,
                    };
                    crate::operations::mqtt_publisher::start_notify::send(
                        endpoint, &config, &process,
                    )?;
                    Err("lost-registration-ack".into())
                },
            )
            .unwrap();
        assert_eq!(registrations, 1);
        assert_eq!(fs::read(root.join("deletes")).unwrap(), b"x");
        selected.attest().unwrap();
        let peer_after = fs::metadata(&peer_path).unwrap();
        assert_eq!(
            (
                peer_before.dev(),
                peer_before.ino(),
                peer_before.mtime_nsec()
            ),
            (peer_after.dev(), peer_after.ino(), peer_after.mtime_nsec())
        );
        assert_eq!(peer_bytes, fs::read(&peer_path).unwrap());
        let selected_before = fs::metadata(plans.join("lab.plan")).unwrap();
        let mut unchanged =
            SelectedMqtt::prepare(&proc_root, &ubus, &plans, "lab", &cake, true, JOB).unwrap();
        assert!(!unchanged.restart);
        unchanged.stop_changed(|| Ok(())).unwrap();
        unchanged
            .finish(
                || Ok(()),
                |_, _| panic!("unchanged publisher must not be registered again"),
            )
            .unwrap();
        assert_eq!(fs::read(root.join("deletes")).unwrap(), b"x");
        assert_eq!(
            selected_before.ino(),
            fs::metadata(plans.join("lab.plan")).unwrap().ino()
        );

        cake.sections
            .get_mut("lab")
            .unwrap()
            .options
            .insert("mqtt_min_interval_s".into(), "4".into());
        let mut refused =
            SelectedMqtt::prepare(&proc_root, &ubus, &plans, "lab", &cake, true, JOB).unwrap();
        assert_eq!(
            refused
                .stop_changed(|| Err("source-changed".into()))
                .unwrap_err(),
            "source-changed"
        );
        assert_eq!(fs::read(root.join("deletes")).unwrap(), b"x");
        refused.attest().unwrap();

        // The selected publisher has independently exited; its dormant procd
        // definition still must be deleted before the plan can be removed.
        fs::remove_dir_all(proc_root.join("4000000001")).unwrap();
        fs::write(&response, serde_json::to_vec(&initial).unwrap()).unwrap();
        let mut disabled =
            SelectedMqtt::prepare(&proc_root, &ubus, &plans, "lab", &cake, false, JOB).unwrap();
        disabled.stop_changed(|| Ok(())).unwrap();
        disabled
            .finish(
                || Ok(()),
                |_, _| panic!("disabled publisher must not register"),
            )
            .unwrap();
        assert!(!plans.join("lab.plan").exists());
        assert_eq!(peer_bytes, fs::read(&peer_path).unwrap());

        // A lost/failed registration cannot be returned as completed. The
        // parent may then replay its original disabled intent to remove the
        // newly published plan without touching peer plans or registrations.
        let mut failed =
            SelectedMqtt::prepare(&proc_root, &ubus, &plans, "lab", &cake, true, JOB).unwrap();
        failed.stop_changed(|| Ok(())).unwrap();
        assert!(failed
            .finish(|| Ok(()), |_, _| Err("registration-failed".into()))
            .unwrap_err()
            .starts_with("registration-failed;"));
        assert!(plans.join("lab.plan").exists());
        let mut rollback = SelectedMqtt::prepare(
            &proc_root,
            &ubus,
            &plans,
            "lab",
            &UciPackage::default(),
            false,
            JOB,
        )
        .unwrap();
        rollback.stop_changed(|| Ok(())).unwrap();
        rollback
            .finish(|| Ok(()), |_, _| panic!("containment must not register"))
            .unwrap();
        assert!(!plans.join("lab.plan").exists());
        assert_eq!(peer_before.ino(), fs::metadata(&peer_path).unwrap().ino());
        fs::remove_dir_all(root).unwrap();
    }

    fn process(instance: &str, pid: u32) -> ControllerIdentity {
        ControllerIdentity {
            kind: ServiceProcessKind::MqttPublisher,
            instance: instance.into(),
            pid,
            process_group: pid,
            starttime_ticks: 17,
        }
    }

    #[test]
    fn r4_selected_mqtt_runtime_requires_exact_pid_and_preserves_dormant_peer_definitions() {
        let mut runtime = Runtime {
            processes: vec![process("lab", 42)],
            definitions: BTreeMap::from([
                ("lab".into(), serde_json::json!({"running":true,"pid":42})),
                ("peer".into(), serde_json::json!({"running":false})),
            ]),
        };
        assert!(runtime.ready("lab"));
        assert!(!runtime.ready("peer"));
        assert!(!runtime.absent("peer"));
        let stopped = Runtime {
            processes: Vec::new(),
            definitions: BTreeMap::from([("peer".into(), serde_json::json!({"running":false}))]),
        };
        assert!(stopped.absent("lab"));
        assert!(stopped.peers_equal(&runtime, "lab"));
        runtime.processes[0].pid += 1;
        assert!(!runtime.ready("lab"));
        runtime.definitions.get_mut("peer").unwrap()["running"] = true.into();
        assert!(!stopped.peers_equal(&runtime, "lab"));
    }
}
