//! Process-only phase of ordinary reload. File/SQM/sidecar authority belongs
//! to the parent; no whole-service delete or implicit generation adoption.

use super::*;

struct ControllerSnapshot {
    definitions: BTreeMap<String, serde_json::Value>,
    processes: BTreeMap<String, ControllerIdentity>,
}

impl ControllerSnapshot {
    fn read(paths: &ServicePaths) -> Result<Self, String> {
        let definitions = super::super::procd_control::controller_definitions(&paths.ubus)?;
        let mut processes = BTreeMap::new();
        for process in discover_controllers(&paths.proc_root)? {
            if process.kind != ServiceProcessKind::Controller {
                continue;
            }
            let definition = definitions
                .get(&process.instance)
                .ok_or("reload-controller-not-registered")?;
            let generation = definition["env"][super::super::controller_input::GENERATION_ENV]
                .as_str()
                .ok_or("reload-controller-generation-missing")?;
            if definition["running"].as_bool() != Some(true)
                || definition["pid"].as_u64() != Some(u64::from(process.pid))
                || controller_generation(&paths.proc_root, &process)?.as_deref() != Some(generation)
                || !controller_identity_matches(&paths.proc_root, &process)?
            {
                return Err("reload-controller-process-mismatch".into());
            }
            if processes
                .insert(process.instance.clone(), process)
                .is_some()
            {
                return Err("reload-controller-duplicate-process".into());
            }
        }
        for (name, definition) in &definitions {
            if definition["running"].as_bool() == Some(true) && !processes.contains_key(name) {
                return Err("reload-controller-running-process-missing".into());
            }
        }
        if super::super::procd_control::controller_definitions(&paths.ubus)? != definitions {
            return Err("reload-controller-definition-changed".into());
        }
        Ok(Self {
            definitions,
            processes,
        })
    }

    fn attest(&self, paths: &ServicePaths, absent: Option<&str>) -> Result<(), String> {
        let current = Self::read(paths)?;
        let expected_definitions: BTreeMap<_, _> = self
            .definitions
            .iter()
            .filter(|(name, _)| absent != Some(name.as_str()))
            .collect();
        let actual_definitions: BTreeMap<_, _> = current.definitions.iter().collect();
        let expected_processes: BTreeMap<_, _> = self
            .processes
            .iter()
            .filter(|(name, _)| absent != Some(name.as_str()))
            .collect();
        let actual_processes: BTreeMap<_, _> = current.processes.iter().collect();
        if expected_definitions != actual_definitions || expected_processes != actual_processes {
            return Err("reload-controller-peer-or-membership-changed".into());
        }
        Ok(())
    }
}

pub(super) struct ReloadControllers {
    snapshot: ControllerSnapshot,
    obsolete: Vec<String>,
}

/// Existing live processes/definitions stay pinned while new registrations
/// may progress from absent/dormant to running. New readiness is proved by the
/// parent observer, not by an add acknowledgement or this preservation guard.
pub(super) struct RegistrationWitness {
    before: ControllerSnapshot,
    desired: BTreeMap<String, String>,
}

#[cfg(feature = "calibration")]
pub(super) struct MqttWitness {
    definitions: BTreeMap<String, serde_json::Value>,
    processes: Vec<ControllerIdentity>,
}
#[cfg(feature = "calibration")]
impl MqttWitness {
    pub(super) fn capture(paths: &ServicePaths) -> Result<Self, String> {
        let witness = Self {
            definitions: super::super::procd_control::mqtt_definitions(&paths.ubus)?,
            processes: discover_controllers(&paths.proc_root)?
                .into_iter()
                .filter(|p| p.kind == ServiceProcessKind::MqttPublisher)
                .collect(),
        };
        for process in &witness.processes {
            if witness
                .definitions
                .get(&process.instance)
                .and_then(|v| v["pid"].as_u64())
                != Some(u64::from(process.pid))
            {
                return Err("reload MQTT process is not registered".into());
            }
        }
        witness.attest(paths)?;
        Ok(witness)
    }
    pub(super) fn attest(&self, paths: &ServicePaths) -> Result<(), String> {
        if super::super::procd_control::mqtt_definitions(&paths.ubus)? != self.definitions
            || discover_controllers(&paths.proc_root)?
                .into_iter()
                .filter(|p| p.kind == ServiceProcessKind::MqttPublisher)
                .collect::<Vec<_>>()
                != self.processes
        {
            return Err("reload preserved MQTT runtime changed".into());
        }
        Ok(())
    }
}
impl RegistrationWitness {
    pub(super) fn attest(&self, paths: &ServicePaths) -> Result<(), String> {
        let definitions = super::super::procd_control::controller_definitions(&paths.ubus)?;
        for (name, definition) in &definitions {
            if definition["env"][super::super::controller_input::GENERATION_ENV].as_str()
                != self.desired.get(name).map(String::as_str)
            {
                return Err("reload-registration-unexpected-generation".into());
            }
        }
        for name in self.before.definitions.keys() {
            if !definitions.contains_key(name) {
                return Err("reload-registration-existing-definition-lost".into());
            }
        }
        let mut actual = BTreeMap::new();
        for process in discover_controllers(&paths.proc_root)? {
            if process.kind != ServiceProcessKind::Controller {
                continue;
            }
            if controller_generation(&paths.proc_root, &process)?.as_ref()
                != self.desired.get(&process.instance)
            {
                return Err("reload-registration-unexpected-process".into());
            }
            actual.insert(process.instance.clone(), process);
        }
        for (name, process) in &self.before.processes {
            if actual.get(name) != Some(process)
                || definitions.get(name) != self.before.definitions.get(name)
                || !controller_identity_matches(&paths.proc_root, process)?
            {
                return Err("reload-registration-preserved-controller-changed".into());
            }
        }
        Ok(())
    }
}

impl ReloadControllers {
    pub(super) fn register_missing(
        self,
        paths: &ServicePaths,
        desired: &BTreeMap<String, String>,
        mut attest_source_and_runtime: impl FnMut() -> Result<(), String>,
    ) -> Result<RegistrationWitness, String> {
        self.attest_stopped(paths)?;
        let missing: Vec<_> = desired
            .keys()
            .filter(|name| !self.snapshot.definitions.contains_key(*name))
            .cloned()
            .collect();
        let witness = RegistrationWitness {
            before: self.snapshot,
            desired: desired.clone(),
        };
        witness.attest(paths)?;
        for instance in missing {
            attest_source_and_runtime()?;
            witness.attest(paths)?;
            let submitted = super::super::procd_control::add_instance(
                &paths.ubus,
                super::super::procd_control::Registration::Controller {
                    instance: &instance,
                    generation: &desired[&instance],
                },
            );
            witness.attest(paths)?;
            let definitions = super::super::procd_control::controller_definitions(&paths.ubus)?;
            if !definitions.contains_key(&instance) {
                return Err(submitted
                    .err()
                    .unwrap_or_else(|| "reload-registration-not-observed".into()));
            }
            attest_source_and_runtime()?;
        }
        witness.attest(paths)?;
        attest_source_and_runtime()?;
        Ok(witness)
    }

    pub(super) fn attest_stopped(&self, paths: &ServicePaths) -> Result<(), String> {
        if !self.obsolete.is_empty() {
            return Err("reload-obsolete-controllers-not-stopped".into());
        }
        self.snapshot.attest(paths, None)
    }

    pub(super) fn live_desired(&self, desired: &BTreeMap<String, String>) -> BTreeSet<String> {
        self.snapshot
            .processes
            .keys()
            .filter(|name| {
                self.snapshot.definitions[*name]["env"]
                    [super::super::controller_input::GENERATION_ENV]
                    .as_str()
                    == desired.get(*name).map(String::as_str)
            })
            .cloned()
            .collect()
    }
    /// Replay may see old, new, or absent affected definitions. Retained peers
    /// must remain live at exactly their accepted generation. Unknown sources
    /// and malformed/dormant retained definitions are never adopted.
    pub(super) fn capture(
        paths: &ServicePaths,
        previous: &BTreeMap<String, String>,
        desired: &BTreeMap<String, String>,
        retained: &BTreeMap<String, String>,
    ) -> Result<Self, String> {
        if retained
            .iter()
            .any(|(name, id)| previous.get(name) != Some(id) || desired.get(name) != Some(id))
        {
            return Err("reload-controller-retained-partition-invalid".into());
        }
        let snapshot = ControllerSnapshot::read(paths)?;
        let mut obsolete = Vec::new();
        for (name, definition) in &snapshot.definitions {
            let id = definition["env"][super::super::controller_input::GENERATION_ENV]
                .as_str()
                .ok_or("reload-controller-generation-missing")?;
            if previous.get(name).map(String::as_str) != Some(id)
                && desired.get(name).map(String::as_str) != Some(id)
            {
                return Err("reload-controller-unexpected-generation".into());
            }
            if desired.get(name).map(String::as_str) != Some(id) {
                obsolete.push(name.clone());
            }
        }
        for (name, id) in retained {
            if !snapshot.processes.contains_key(name)
                || snapshot
                    .definitions
                    .get(name)
                    .and_then(|v| v["env"][super::super::controller_input::GENERATION_ENV].as_str())
                    != Some(id.as_str())
            {
                return Err("reload-controller-retained-peer-not-ready".into());
            }
        }
        snapshot.attest(paths, None)?;
        Ok(Self { snapshot, obsolete })
    }

    pub(super) fn stop_obsolete(
        &mut self,
        paths: &ServicePaths,
        mut attest_source_and_topology: impl FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        attest_source_and_topology()?;
        self.snapshot.attest(paths, None)?;
        while let Some(name) = self.obsolete.first().cloned() {
            attest_source_and_topology()?;
            self.snapshot.attest(paths, None)?;
            let pidfds = capture_controller_pidfds(
                &paths.proc_root,
                Some((ServiceProcessKind::Controller, &name)),
            )?;
            if pidfds.len() != usize::from(self.snapshot.processes.contains_key(&name))
                || pidfds
                    .iter()
                    .any(|p| self.snapshot.processes.get(&name) != Some(&p.identity))
            {
                return Err("reload-controller-pidfd-mismatch".into());
            }
            self.snapshot.attest(paths, None)?;
            attest_source_and_topology()?;
            let request = serde_json::json!({"name":"cake-autorate","instance":name}).to_string();
            let deleted = delete_service_or_attest_absent(
                &paths.ubus,
                &request,
                "delete obsolete reload controller",
            );
            if let Err(error) = deleted {
                // A lost delete acknowledgement is resolved by exact absence,
                // never by resubmitting the non-idempotent service operation.
                let remaining = super::super::procd_control::controller_definitions(&paths.ubus)?;
                if remaining.contains_key(&name) {
                    return Err(error);
                }
            }
            wait_controller_pidfds(pidfds, CONTROLLER_STOP_TIMEOUT)?;
            self.snapshot.attest(paths, Some(&name))?;
            attest_source_and_topology()?;
            self.snapshot.definitions.remove(&name);
            self.snapshot.processes.remove(&name);
            self.obsolete.remove(0);
        }
        self.obsolete.clear();
        self.snapshot.attest(paths, None)?;
        attest_source_and_topology()
    }
}

pub(super) trait SqmReloadBackend {
    fn attest(&mut self) -> Result<(), String>;
    fn desired_ready(&mut self, spec: &ManagedSqmAttestationSpec) -> Result<bool, String>;
    fn stop_old(&mut self, spec: &ManagedSqmStopSpec) -> Result<(), String>;
    fn start_new(&mut self, spec: &ManagedSqmAttestationSpec) -> Result<(), String>;
}

/// Stop every obsolete target before starting any new target (including target
/// swaps). A desired runtime proven ready on replay is never stopped through
/// an old recipe for that same target. Final proof covers all desired queues.
pub(super) fn reconcile_sqm(
    old: &[ManagedSqmStopSpec],
    desired: &[ManagedSqmAttestationSpec],
    backend: &mut impl SqmReloadBackend,
) -> Result<(), String> {
    backend.attest()?;
    let mut ready = BTreeSet::new();
    for spec in desired {
        if backend.desired_ready(spec)? {
            ready.insert(spec.target_interface.clone());
        }
        backend.attest()?;
    }
    for spec in old {
        if !ready.contains(&spec.target_interface) {
            backend.attest()?;
            backend.stop_old(spec)?;
            backend.attest()?;
        }
    }
    for spec in desired {
        if !ready.contains(&spec.target_interface) {
            backend.attest()?;
            backend.start_new(spec)?;
            backend.attest()?;
        }
    }
    for spec in desired {
        if !backend.desired_ready(spec)? {
            return Err("reload-sqm-desired-runtime-not-ready".into());
        }
        backend.attest()?;
    }
    backend.attest()
}

#[cfg(test)]
mod tests {
    use super::*;
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

    struct SqmFixture {
        ready: BTreeSet<String>,
        actions: Vec<String>,
        fail: Option<&'static str>,
        starts_ready: bool,
    }
    impl SqmReloadBackend for SqmFixture {
        fn attest(&mut self) -> Result<(), String> {
            if self.fail == Some("fence") {
                Err("fixture-source-changed".into())
            } else {
                Ok(())
            }
        }
        fn desired_ready(&mut self, spec: &ManagedSqmAttestationSpec) -> Result<bool, String> {
            Ok(self.ready.contains(&spec.target_interface))
        }
        fn stop_old(&mut self, spec: &ManagedSqmStopSpec) -> Result<(), String> {
            self.actions.push(format!("stop:{}", spec.target_interface));
            if self.fail == Some("stop") {
                return Err("fixture-stop-failed".into());
            }
            self.ready.remove(&spec.target_interface);
            Ok(())
        }
        fn start_new(&mut self, spec: &ManagedSqmAttestationSpec) -> Result<(), String> {
            self.actions
                .push(format!("start:{}", spec.target_interface));
            if self.fail == Some("start") {
                return Err("fixture-start-failed".into());
            }
            if self.starts_ready {
                self.ready.insert(spec.target_interface.clone());
            }
            Ok(())
        }
    }
    fn queue(name: &str, target: &str) -> (ManagedSqmStopSpec, ManagedSqmAttestationSpec) {
        (
            ManagedSqmStopSpec {
                instance: name.into(),
                sqm_section: format!("cake_{name}"),
                target_interface: target.into(),
                download_interface: format!("ifb4{target}"),
                rate_policy: None,
            },
            ManagedSqmAttestationSpec {
                instance: name.into(),
                sqm_section: format!("cake_{name}"),
                target_interface: target.into(),
                upload_interface: target.into(),
                download_interface: format!("ifb4{target}"),
                direction_mode: "both".into(),
                minimum_download_kbps: 1000,
                maximum_download_kbps: 1000,
                minimum_upload_kbps: 1000,
                maximum_upload_kbps: 1000,
            },
        )
    }

    #[test]
    fn r4_reload_sqm_stops_before_starts_and_replay_preserves_ready_targets() {
        let old = [queue("lab", "a").0, queue("peer", "b").0];
        let desired = [queue("lab", "b").1, queue("peer", "a").1];
        let mut fixture = SqmFixture {
            ready: BTreeSet::from(["foreign".into()]),
            actions: Vec::new(),
            fail: None,
            starts_ready: true,
        };
        reconcile_sqm(&old, &desired, &mut fixture).unwrap();
        assert_eq!(fixture.actions, ["stop:a", "stop:b", "start:b", "start:a"]);
        assert!(fixture.ready.contains("foreign"));
        fixture.actions.clear();
        reconcile_sqm(&old, &desired, &mut fixture).unwrap();
        assert!(fixture.actions.is_empty());
        fixture.ready.remove("a");
        reconcile_sqm(&old, &desired, &mut fixture).unwrap();
        assert_eq!(fixture.actions, ["stop:a", "start:a"]);
        assert!(fixture.ready.contains("b"));
        fixture.actions.clear();
        reconcile_sqm(&[], &[], &mut fixture).unwrap(); // Controller-only change.
        assert!(fixture.actions.is_empty());
    }

    #[test]
    fn r4_reload_sqm_failures_never_start_after_failed_stop_or_accept_unready_runtime() {
        let (old, desired) = queue("lab", "a");
        let mut fixture = SqmFixture {
            ready: BTreeSet::new(),
            actions: Vec::new(),
            fail: Some("fence"),
            starts_ready: true,
        };
        assert!(reconcile_sqm(
            std::slice::from_ref(&old),
            std::slice::from_ref(&desired),
            &mut fixture
        )
        .is_err());
        assert!(fixture.actions.is_empty());
        fixture.fail = Some("stop");
        assert!(reconcile_sqm(
            std::slice::from_ref(&old),
            std::slice::from_ref(&desired),
            &mut fixture
        )
        .is_err());
        assert_eq!(fixture.actions, ["stop:a"]);
        fixture.actions.clear();
        fixture.fail = None;
        fixture.starts_ready = false;
        assert_eq!(
            reconcile_sqm(
                std::slice::from_ref(&old),
                std::slice::from_ref(&desired),
                &mut fixture
            )
            .unwrap_err(),
            "reload-sqm-desired-runtime-not-ready"
        );
        assert_eq!(fixture.actions, ["stop:a", "start:a"]);
        fixture.actions.clear();
        fixture.ready.insert("a".into()); // A delayed postcondition became ready.
        reconcile_sqm(&[old], &[desired], &mut fixture).unwrap();
        assert!(fixture.actions.is_empty());
    }

    #[test]
    fn r4_reload_controller_pidfd_stop_preserves_peers_and_already_updated_processes() {
        let root = std::env::temp_dir().join(format!(
            "cake-reload-pidfd-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let binary = root.join("controller");
        let compiled = Command::new("rustc")
            .args(["--edition=2021", "--crate-name", "reload_pidfd_fixture"])
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/mqtt-pidfd-fixture.rs"))
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            compiled.status.success(),
            "owned fixture compilation failed"
        );
        let generation_env = super::super::super::controller_input::GENERATION_ENV;
        let previous = BTreeMap::from([
            ("lab".into(), "a".repeat(64)),
            ("peer".into(), "b".repeat(64)),
        ]);
        for mode in [
            "normal",
            "lost-ack",
            "unchanged",
            "already-new",
            "source-failure",
            "peer-definition-change",
        ] {
            let case = root.join(mode);
            let proc_root = case.join("proc");
            fs::create_dir_all(&proc_root).unwrap();
            let mut children = OwnedChildren(Vec::new());
            let lab_id = if mode == "already-new" {
                "c".repeat(64)
            } else {
                previous["lab"].clone()
            };
            for (name, id) in [("lab", &lab_id), ("peer", &previous["peer"])] {
                let child = Command::new(&binary)
                    .arg0(DAEMON_PATH)
                    .args(["--instance", name])
                    .env_clear()
                    .env(generation_env, id)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap();
                let pid = child.id();
                children.0.push(child);
                std::os::unix::fs::symlink(format!("/proc/{pid}"), proc_root.join(pid.to_string()))
                    .unwrap();
            }
            let definition = |name: &str, pid: u32, id: &str| {
                serde_json::json!({
                    "command":[DAEMON_PATH,"--instance",name], "running":true,"pid":pid,
                    "env":{generation_env:id}, "respawn":[3600,5,5]
                })
            };
            let peer_definition = definition("peer", children.0[1].id(), &previous["peer"]);
            let after = serde_json::json!({"cake-autorate":{"instances":{"peer":peer_definition}}});
            let mut before = after.clone();
            before["cake-autorate"]["instances"]["lab"] =
                definition("lab", children.0[0].id(), &lab_id);
            let response = case.join("response");
            fs::write(&response, serde_json::to_vec(&before).unwrap()).unwrap();
            fs::write(case.join("after"), serde_json::to_vec(&after).unwrap()).unwrap();
            let ubus = case.join("ubus");
            let deleted = case.join("deleted");
            // Only our owned, unreaped selected child can receive this signal.
            // A second or broad delete is rejected; the peer PID is not an input.
            fs::write(&ubus, format!("#!/bin/sh\ncase \"$3\" in\nlist) cat '{}' ;;\ndelete) [ \"$4\" = '{{\"instance\":\"lab\",\"name\":\"cake-autorate\"}}' ] || exit 99\n[ ! -f '{}' ] || exit 98\nkill -TERM {} || exit 97\nprintf x > '{}'\ncp '{}' '{}'\nexit {} ;;\n*) exit 96;;\nesac\n",
                response.display(), deleted.display(), children.0[0].id(), deleted.display(),
                case.join("after").display(), response.display(), if mode == "lost-ack" {1} else {0})).unwrap();
            fs::set_permissions(&ubus, fs::Permissions::from_mode(0o700)).unwrap();
            let paths = ServicePaths {
                config_root: case.join("unused-config"),
                uci: case.join("unused-uci"),
                tc: case.join("unused-tc"),
                ubus,
                proc_root,
                runtime_root: case.join("unused-run"),
                runtime_lock_root: case.join("unused-locks"),
                bridger_init: case.join("unused-bridger"),
                bridger_config: case.join("unused-bridger-config"),
                uci_workspace_root: case.join("unused-work"),
            };
            let mut desired = previous.clone();
            if mode != "unchanged" {
                desired.insert("lab".into(), "c".repeat(64));
            }
            let retained = if mode == "unchanged" {
                previous.clone()
            } else {
                BTreeMap::from([("peer".into(), previous["peer"].clone())])
            };
            let mut controllers =
                ReloadControllers::capture(&paths, &previous, &desired, &retained).unwrap();
            let peer = controllers.snapshot.processes["peer"].clone();
            if mode == "source-failure" {
                assert!(controllers
                    .stop_obsolete(&paths, || Err("source-changed".into()))
                    .is_err());
            } else if mode == "peer-definition-change" {
                let mut drift = before.clone();
                drift["cake-autorate"]["instances"]["peer"]["respawn"] =
                    serde_json::json!([3600, 9, 5]);
                fs::write(&response, serde_json::to_vec(&drift).unwrap()).unwrap();
                assert!(controllers.stop_obsolete(&paths, || Ok(())).is_err());
                assert!(!deleted.exists());
                fs::write(&response, serde_json::to_vec(&before).unwrap()).unwrap();
            } else {
                controllers.stop_obsolete(&paths, || Ok(())).unwrap();
                controllers.stop_obsolete(&paths, || Ok(())).unwrap();
            }
            let should_stop = mode == "normal" || mode == "lost-ack";
            assert_eq!(deleted.exists(), should_stop);
            if should_stop {
                let mut pending = after.clone();
                pending["cake-autorate"]["instances"]["lab"] = serde_json::json!({
                    "command":[DAEMON_PATH,"--instance","lab"],"running":false,
                    "env":{generation_env:&desired["lab"]}
                });
                fs::write(
                    case.join("after-add"),
                    serde_json::to_vec(&pending).unwrap(),
                )
                .unwrap();
                let added = case.join("added");
                fs::write(&paths.ubus, format!("#!/bin/sh\ncase \"$3\" in\nlist) cat '{}' ;;\nadd) [ ! -f '{}' ] || exit 98\nprintf x > '{}'\ncp '{}' '{}'\nexit {} ;;\n*) exit 97;;\nesac\n", response.display(), added.display(), added.display(), case.join("after-add").display(), response.display(), if mode == "lost-ack" {1} else {0})).unwrap();
                let current =
                    ReloadControllers::capture(&paths, &previous, &desired, &retained).unwrap();
                assert!(current
                    .register_missing(&paths, &desired, || Err("source-before-add".into()))
                    .is_err());
                assert!(!added.exists());
                let current =
                    ReloadControllers::capture(&paths, &previous, &desired, &retained).unwrap();
                let witness = current
                    .register_missing(&paths, &desired, || Ok(()))
                    .unwrap();
                witness.attest(&paths).unwrap();
                assert!(added.exists());
                let resumed =
                    ReloadControllers::capture(&paths, &previous, &desired, &retained).unwrap();
                resumed
                    .register_missing(&paths, &desired, || Ok(()))
                    .unwrap()
                    .attest(&paths)
                    .unwrap();
                assert!(matches!(
                    observe_controller_start(
                        &paths,
                        &desired.keys().cloned().collect::<Vec<_>>(),
                        epoch_seconds()
                    )
                    .unwrap(),
                    ControllerStartReadiness::Waiting(_)
                ));
            }
            if should_stop {
                assert_eq!(children.0[0].wait().unwrap().signal(), Some(libc::SIGTERM));
            } else {
                assert!(children.0[0].try_wait().unwrap().is_none());
            }
            assert!(children.0[1].try_wait().unwrap().is_none());
            assert!(controller_identity_matches(&paths.proc_root, &peer).unwrap());
            let actual =
                super::super::super::procd_control::controller_definitions(&paths.ubus).unwrap();
            assert_eq!(actual["peer"], after["cake-autorate"]["instances"]["peer"]);
            assert!(!paths.config_root.exists());
            assert!(!paths.tc.exists());
        }
    }
}
