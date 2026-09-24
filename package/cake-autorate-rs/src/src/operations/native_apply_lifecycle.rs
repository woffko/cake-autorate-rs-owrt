//! Exact selected-instance lifecycle used by native Apply.
//!
//! The durable Apply transaction already owns the global lifecycle lock.  This
//! module keeps every state-dependent decision in Rust and leaves rc.common
//! only the mechanical `procd add` operation which OpenWrt itself defines.

use super::autotune_apply_runtime::NativeApplyGlobalLock;
use super::autotune_uci_materialization::verify_scalar_uci_section;
use super::committed_uci::{FrozenSqmAlias, SqmAliasConfig};
use super::identity::ProcessIdentity;
use super::json_wire::json_escape;
use super::procd_control::delete_service_or_attest_absent;
use super::process::{run_bounded_command_output_with_input, SpawnSpec};
use super::protocol::OperationRequest;
use super::runtime_health::{safe_interface, safe_name, UciPackage, UciSection};
use super::service_config::{InterfaceResolver, OpenWrtEnvironment};
use super::sqm_projection::{
    plan_projection, prepare_mq_capabilities, ProjectionScope, SqmProjectionPlan,
};
use super::sqm_recovery_openwrt::{error_message as sqm_error_message, ManagedSqmAttestationSpec};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
#[cfg(test)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const CAKE_PACKAGE: &str = "cake-autorate";
const SQM_PACKAGE: &str = "sqm";
const SERVICE_NAME: &str = "cake-autorate";
const DAEMON_PATH: &str = "/usr/sbin/cake-autorated";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const CONTROLLER_STOP_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_OUTPUT: usize = 64 * 1024;
const MAX_CMDLINE: u64 = 4096;
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
#[cfg(test)]
static NEXT_SNAPSHOT: AtomicU32 = AtomicU32::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectedLifecycleAction {
    Restart,
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectedProjectionState {
    /// The cake-autorate package already contains the selected direction, but
    /// the managed SQM section may still be the exact previous projection.
    /// Either rate may therefore still describe the previous direction.  This
    /// phase validates only the structural owner and request binding; rate
    /// postconditions become authoritative after projection.
    Pending,
    /// The SQM projection has completed and both directional rates must match
    /// the selected direction exactly.
    Exact,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SelectedConfig {
    cake: UciPackage,
    sqm: UciPackage,
    instance: String,
    sqm_section: String,
    target_interface: String,
    download_interface: String,
    controller_should_run: bool,
    sqm_should_run: bool,
    download_enabled: bool,
    upload_enabled: bool,
    download_kbps: u64,
    upload_kbps: u64,
}

trait SelectedLifecycleBackend {
    fn project(&mut self, instance: &str) -> Result<SqmProjectionPlan, String>;
    fn snapshot(&mut self, projection: SelectedProjectionState) -> Result<SelectedConfig, String>;
    fn attest_unchanged(&mut self, snapshot: &SelectedConfig) -> Result<(), String>;
    fn prepare_controller(&mut self, _snapshot: &SelectedConfig) -> Result<(), String> {
        Ok(())
    }
    fn freeze_sqm(&mut self, snapshot: &SelectedConfig) -> Result<(), String>;
    fn stop_controller(&mut self, instance: &str) -> Result<(), String>;
    fn stop_sqm(&mut self, snapshot: &SelectedConfig) -> Result<(), String>;
    fn prepare_ingress(&mut self, snapshot: &SelectedConfig) -> Result<(), String>;
    fn start_sqm(&mut self, snapshot: &SelectedConfig) -> Result<(), String>;
    fn apply_classifier(&mut self, snapshot: &SelectedConfig) -> Result<(), String>;
    fn clear_classifier(&mut self, snapshot: &SelectedConfig) -> Result<(), String>;
    fn prepare_mqtt(&mut self, snapshot: &SelectedConfig, enabled: bool) -> Result<(), String>;
    fn stop_mqtt(&mut self) -> Result<(), String>;
    fn finish_mqtt(&mut self) -> Result<(), String>;
    fn register_controller(&mut self, snapshot: &SelectedConfig) -> Result<(), String>;
}

fn execute_selected_lifecycle(
    action: SelectedLifecycleAction,
    backend: &mut impl SelectedLifecycleBackend,
) -> Result<(), String> {
    let snapshot = match action {
        SelectedLifecycleAction::Restart => {
            let initial = backend.snapshot(SelectedProjectionState::Exact)?;
            let projection = backend.project(&initial.instance)?;
            if projection.conflicts().contains(&initial.instance) {
                return Err(format!(
                    "native Apply selected instance {} has a duplicate SQM target",
                    initial.instance
                ));
            }
            let projected = backend.snapshot(SelectedProjectionState::Exact)?;
            if projected.sqm_should_run {
                if !projection.is_managed()
                    || projection.interfaces().len() != 1
                    || !projection
                        .interfaces()
                        .contains(&projected.target_interface)
                {
                    return Err(
                        "native Apply SQM projection did not prove the selected active target"
                            .to_string(),
                    );
                }
                if projected.download_enabled
                    && !projection
                        .ingress_interfaces()
                        .contains(&projected.target_interface)
                {
                    return Err(
                        "native Apply SQM projection omitted the selected ingress target"
                            .to_string(),
                    );
                }
            }
            projected
        }
        SelectedLifecycleAction::Stop => backend.snapshot(SelectedProjectionState::Exact)?,
    };

    backend.attest_unchanged(&snapshot)?;
    backend.prepare_mqtt(
        &snapshot,
        action == SelectedLifecycleAction::Restart && snapshot.controller_should_run,
    )?;
    if action == SelectedLifecycleAction::Restart {
        backend.prepare_controller(&snapshot)?;
        backend.attest_unchanged(&snapshot)?;
    }
    backend.freeze_sqm(&snapshot)?;
    backend.attest_unchanged(&snapshot)?;
    backend.stop_mqtt()?;
    backend.stop_controller(&snapshot.instance)?;
    backend.attest_unchanged(&snapshot)?;
    backend.stop_sqm(&snapshot)?;
    backend.attest_unchanged(&snapshot)?;

    if action == SelectedLifecycleAction::Stop {
        backend.clear_classifier(&snapshot)?;
        backend.finish_mqtt()?;
        backend.attest_unchanged(&snapshot)?;
        return Ok(());
    }
    if snapshot.sqm_should_run {
        if snapshot.download_enabled {
            backend.prepare_ingress(&snapshot)?;
            backend.attest_unchanged(&snapshot)?;
        }
        backend.start_sqm(&snapshot)?;
        backend.attest_unchanged(&snapshot)?;
    }
    backend.apply_classifier(&snapshot)?;
    backend.attest_unchanged(&snapshot)?;
    backend.register_controller(&snapshot)?;
    backend.finish_mqtt()?;
    backend.attest_unchanged(&snapshot)?;
    Ok(())
}

struct OpenWrtSelectedLifecycle<'a> {
    request: &'a OperationRequest,
    expected_target: &'a str,
    lock: &'a NativeApplyGlobalLock,
    environment: OpenWrtEnvironment,
    paths: LifecyclePaths,
    source: Option<super::uci_transaction::PublishedConfig>,
    runner_profile: Option<super::sqm_runner::Profile>,
    classifier: Option<super::traffic_classifier::SelectedClassifier>,
    mqtt: Option<super::service_lifecycle::selected_mqtt::SelectedMqtt>,
    sqm_snapshot: Option<ContainmentSqmConfig>,
    registration: Option<super::service_lifecycle::SelectedGeneration>,
}

#[derive(Clone, Debug)]
struct LifecyclePaths {
    uci: PathBuf,
    proc_root: PathBuf,
    sys_class_net: PathBuf,
    ubus: PathBuf,
    init: PathBuf,
    sqm_runner: PathBuf,
    sqm_config: PathBuf,
    tc: PathBuf,
    snapshot_root: PathBuf,
}

impl LifecyclePaths {
    fn production() -> Self {
        Self {
            uci: env_path("CAKE_AUTORATE_UCI_BIN", "/sbin/uci"),
            proc_root: env_path("CAKE_AUTORATE_PROC_ROOT", "/proc"),
            sys_class_net: env_path("CAKE_AUTORATE_SYS_CLASS_NET", "/sys/class/net"),
            ubus: env_path("CAKE_AUTORATE_UBUS_BIN", "/bin/ubus"),
            init: env_path("CAKE_AUTORATE_INIT_BIN", "/etc/init.d/cake-autorate"),
            sqm_runner: env_path("CAKE_AUTORATE_SQM_RUNNER", "/usr/lib/sqm/run.sh"),
            sqm_config: env_path("CAKE_AUTORATE_SQM_CONFIG", "/etc/config/sqm"),
            tc: env_path("CAKE_AUTORATE_TC_BIN", "/sbin/tc"),
            snapshot_root: env_path(
                "CAKE_AUTORATE_LIFECYCLE_SNAPSHOT_ROOT",
                "/tmp/cake-autorate-native-lifecycle",
            ),
        }
    }
}

pub(crate) fn run_selected_instance_lifecycle(
    action: SelectedLifecycleAction,
    request: &OperationRequest,
    expected_target: &str,
    lock: &NativeApplyGlobalLock,
) -> Result<(), String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("native Apply selected lifecycle requires root".to_string());
    }
    validate_request_binding(request, expected_target)?;
    let mut backend = OpenWrtSelectedLifecycle {
        request,
        expected_target,
        lock,
        environment: OpenWrtEnvironment::production(),
        paths: LifecyclePaths::production(),
        source: None,
        runner_profile: None,
        classifier: None,
        mqtt: None,
        sqm_snapshot: None,
        registration: None,
    };
    execute_selected_lifecycle(action, &mut backend)
}

pub(crate) fn run_selected_instance_containment(
    request: &OperationRequest,
    expected_target: &str,
    lock: &NativeApplyGlobalLock,
    cake: &UciPackage,
    sqm: &UciPackage,
    sqm_bytes: &[u8],
) -> Result<(), String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("native Apply selected containment requires root".to_string());
    }
    validate_request_binding(request, expected_target)?;
    let environment = OpenWrtEnvironment::production();
    let snapshot = selected_config(
        cake,
        sqm,
        &environment,
        request,
        expected_target,
        SelectedProjectionState::Exact,
    )?;
    let paths = LifecyclePaths::production();
    let private_sqm = ContainmentSqmConfig::prepare(&paths, &snapshot, sqm_bytes)?;
    let mut backend = OpenWrtSelectedLifecycle {
        request,
        expected_target,
        lock,
        environment,
        paths,
        source: None,
        runner_profile: None,
        classifier: None,
        mqtt: None,
        sqm_snapshot: Some(private_sqm),
        registration: None,
    };
    backend.prepare_mqtt(&snapshot, false)?;
    backend.stop_mqtt()?;
    backend.stop_controller(&snapshot.instance)?;
    backend.stop_sqm(&snapshot)?;
    backend.clear_classifier(&snapshot)?;
    backend.finish_mqtt()
}

fn capture_selected_source(
    paths: &LifecyclePaths,
) -> Result<super::uci_transaction::PublishedConfig, String> {
    let root = paths
        .sqm_config
        .parent()
        .filter(|_| paths.sqm_config.file_name() == Some(std::ffi::OsStr::new("sqm")))
        .ok_or("native selected configuration root is invalid")?;
    let committed =
        super::committed_uci::CommittedSnapshot::capture(root, &paths.snapshot_root, &paths.uci)?;
    // The parent already owns the actual candidate-file transaction. This
    // no-op wrapper grants frozen runtime authority, never another file edit.
    super::uci_transaction::publish(committed.prepare([&[], &[]], &paths.uci)?)
}

impl OpenWrtSelectedLifecycle<'_> {
    fn attest_sidecar_source(&self) -> Result<(), String> {
        validate_request_binding(self.request, self.expected_target)?;
        if let Some(source) = &self.source {
            source.attest()
        } else if let Some(frozen) = &self.sqm_snapshot {
            // Containment retains the parent's frozen recipe; a newly read
            // public configuration is not authority to remove sidecars.
            frozen.alias.sqm_runner_alias().map(|_| ())
        } else {
            Err("selected sidecar lifecycle has no frozen authority".into())
        }
    }

    fn ensure_source(&mut self) -> Result<(), String> {
        if self.source.is_none() {
            if self.sqm_snapshot.is_some() {
                return Err("containment cannot recapture public UCI as new authority".into());
            }
            self.source = Some(capture_selected_source(&self.paths)?);
        }
        self.source
            .as_ref()
            .ok_or("native selected source missing")?
            .attest()
    }
}

impl SelectedLifecycleBackend for OpenWrtSelectedLifecycle<'_> {
    fn project(&mut self, instance: &str) -> Result<SqmProjectionPlan, String> {
        self.ensure_source()?;
        let source = self
            .source
            .as_ref()
            .ok_or("native selected source missing")?;
        let cake = source.config().package(CAKE_PACKAGE)?.clone();
        let sqm = source.config().package(SQM_PACKAGE)?.clone();
        prepare_mq_capabilities(
            &cake,
            &sqm,
            &self.environment,
            &ProjectionScope::Instance(instance.into()),
            true,
            crate::qdisc_capabilities::ensure,
        )?;
        source.attest()?;
        let mut projected = sqm.clone();
        let plan = plan_projection(
            &cake,
            &mut projected,
            &self.environment,
            &ProjectionScope::Instance(instance.to_string()),
        )?;
        if projected != sqm {
            return Err(
                "native Apply selected lifecycle found a pending SQM projection".to_string(),
            );
        }
        Ok(plan)
    }

    fn snapshot(&mut self, projection: SelectedProjectionState) -> Result<SelectedConfig, String> {
        self.ensure_source()?;
        let source = self
            .source
            .as_ref()
            .ok_or("native selected source missing")?;
        let cake = source.config().package(CAKE_PACKAGE)?;
        let sqm = source.config().package(SQM_PACKAGE)?;
        let selected = selected_config(
            cake,
            sqm,
            &self.environment,
            self.request,
            self.expected_target,
            projection,
        )?;
        let expected = sqm
            .sections
            .get(&selected.sqm_section)
            .map(|section| (section.section_type.as_str(), &section.options));
        verify_scalar_uci_section(
            source.config().candidate_bytes(SQM_PACKAGE)?,
            &selected.sqm_section,
            expected,
        )?;
        source.attest()?;
        Ok(selected)
    }

    fn attest_unchanged(&mut self, snapshot: &SelectedConfig) -> Result<(), String> {
        let source = self
            .source
            .as_ref()
            .ok_or("native selected source missing")?;
        source.attest()?;
        if source.config().package(CAKE_PACKAGE)? != &snapshot.cake
            || source.config().package(SQM_PACKAGE)? != &snapshot.sqm
        {
            return Err("native Apply selected lifecycle UCI changed during mutation".to_string());
        }
        if let Some(frozen) = self.sqm_snapshot.as_ref() {
            frozen.alias.sqm_runner_alias()?;
        }
        if let Some(mqtt) = &self.mqtt {
            mqtt.attest()?;
        }
        Ok(())
    }

    fn freeze_sqm(&mut self, snapshot: &SelectedConfig) -> Result<(), String> {
        if self.sqm_snapshot.is_some() || self.runner_profile.is_some() {
            return Err("native Apply selected lifecycle SQM config was frozen twice".to_string());
        }
        let profile = super::sqm_runner::Profile::inspect(&self.paths.sqm_runner)?;
        let source = self
            .source
            .as_ref()
            .ok_or("native selected source missing")?;
        // Validate the actual alias/executable namespace and surviving-helper
        // leases before stopping the healthy controller, without running SQM.
        drop(
            profile
                .clone()
                .bind(source.config(), &self.paths.snapshot_root)?,
        );
        self.runner_profile = Some(profile);
        self.attest_unchanged(snapshot)
    }

    fn stop_controller(&mut self, instance: &str) -> Result<(), String> {
        let observed = find_controller(&self.paths.proc_root, instance)?;
        let pidfd = observed
            .as_ref()
            .map(|identity| open_stable_pidfd(&self.paths.proc_root, identity))
            .transpose()?;
        let request = format!(
            "{{\"name\":\"{}\",\"instance\":\"{}\"}}",
            SERVICE_NAME,
            json_escape(instance)
        );
        delete_service_or_attest_absent(
            &self.paths.ubus,
            &request,
            "delete the selected procd controller",
        )?;
        if let Some(pidfd) = pidfd.as_ref() {
            wait_pidfd(pidfd, CONTROLLER_STOP_TIMEOUT)?;
        }
        if find_controller(&self.paths.proc_root, instance)?.is_some() {
            return Err(format!(
                "native Apply controller {instance} remained after exact procd deletion"
            ));
        }
        Ok(())
    }

    fn stop_sqm(&mut self, snapshot: &SelectedConfig) -> Result<(), String> {
        if let Some(source) = &self.source {
            let profile = self
                .runner_profile
                .as_ref()
                .ok_or("native selected runner was not preflighted")?
                .clone();
            let runner = profile.bind(source.config(), &self.paths.snapshot_root)?;
            runner.stop(&snapshot.target_interface, || source.attest(), || false)?;
            return attest_selected_sqm_absent(&self.paths, snapshot);
        }
        let config = self.sqm_snapshot.as_ref().ok_or_else(|| {
            "native Apply selected lifecycle has no frozen SQM config".to_string()
        })?;
        let runner = config
            .profile
            .clone()
            .bind(&config.alias, &self.paths.snapshot_root)?;
        runner.stop(
            &snapshot.target_interface,
            || self.attest_sidecar_source(),
            || false,
        )?;
        // sqm-scripts status is not authoritative in either direction.  Stop
        // succeeds only when the exact selected runtime is absent.
        attest_selected_sqm_absent(&self.paths, snapshot)
    }

    fn prepare_ingress(&mut self, snapshot: &SelectedConfig) -> Result<(), String> {
        remove_empty_clsact(&self.paths, &snapshot.target_interface)
    }

    fn start_sqm(&mut self, snapshot: &SelectedConfig) -> Result<(), String> {
        let source = self
            .source
            .as_ref()
            .ok_or("native selected source missing")?;
        let profile = self
            .runner_profile
            .as_ref()
            .ok_or("native selected runner was not preflighted")?
            .clone();
        let runner = profile.bind(source.config(), &self.paths.snapshot_root)?;
        runner.start(&snapshot.target_interface, || source.attest(), || false)?;
        let spec = ManagedSqmAttestationSpec {
            instance: snapshot.instance.clone(),
            sqm_section: snapshot.sqm_section.clone(),
            target_interface: snapshot.target_interface.clone(),
            upload_interface: snapshot.target_interface.clone(),
            download_interface: snapshot.download_interface.clone(),
            direction_mode: if snapshot.download_enabled && snapshot.upload_enabled {
                "both"
            } else if snapshot.download_enabled {
                "download_only"
            } else {
                "upload_only"
            }
            .to_string(),
            minimum_download_kbps: snapshot.download_kbps,
            maximum_download_kbps: snapshot.download_kbps,
            minimum_upload_kbps: snapshot.upload_kbps,
            maximum_upload_kbps: snapshot.upload_kbps,
        };
        match super::sqm_recovery_openwrt::attest_managed_sqm_from_published(&spec, source) {
            Ok(false) => Ok(()),
            Ok(true) => Err("native selected target went offline during start".into()),
            Err(error) => Err(format!(
                "exact selected runtime postcondition failed: {}",
                sqm_error_message(&error)
            )),
        }
    }

    fn apply_classifier(&mut self, snapshot: &SelectedConfig) -> Result<(), String> {
        let classifier = self
            .classifier
            .take()
            .ok_or("selected classifier was not preflighted")?;
        classifier.apply(|| self.attest_unchanged(snapshot))
    }

    fn prepare_mqtt(&mut self, snapshot: &SelectedConfig, enabled: bool) -> Result<(), String> {
        self.attest_sidecar_source()?;
        self.mqtt = Some(
            super::service_lifecycle::selected_mqtt::SelectedMqtt::prepare(
                &self.paths.proc_root,
                &self.paths.ubus,
                Path::new(super::mqtt_publisher::PRODUCTION_PLAN_ROOT),
                &snapshot.instance,
                &snapshot.cake,
                enabled,
                &self.request.identity.job_id,
            )?,
        );
        self.attest_sidecar_source()
    }

    fn stop_mqtt(&mut self) -> Result<(), String> {
        let mut mqtt = self.mqtt.take().ok_or("selected MQTT was not prepared")?;
        let result = mqtt.stop_changed(|| self.attest_sidecar_source());
        self.mqtt = Some(mqtt);
        result
    }

    fn finish_mqtt(&mut self) -> Result<(), String> {
        let mut mqtt = self.mqtt.take().ok_or("selected MQTT was not prepared")?;
        let result = mqtt.finish(
            || self.attest_sidecar_source(),
            |instance, endpoint| {
                run_success(
                    &SpawnSpec {
                        program: self.paths.init.clone(),
                        arguments: vec![
                            "native_apply_register_mqtt".into(),
                            instance.into(),
                            endpoint.into(),
                        ],
                        environment: vec![(
                            "CAKE_AUTORATE_NATIVE_APPLY_RECOVERY".into(),
                            "1".into(),
                        )],
                    },
                    None,
                    COMMAND_TIMEOUT,
                    |command| self.lock.configure_borrowed_restart(command),
                    "register selected MQTT publisher",
                )
            },
        );
        self.mqtt = Some(mqtt);
        result
    }

    fn clear_classifier(&mut self, snapshot: &SelectedConfig) -> Result<(), String> {
        let job = self.request.identity.job_id.clone();
        let proof = || -> Result<(), String> {
            validate_request_binding(self.request, self.expected_target)?;
            if let Some(source) = &self.source {
                source.attest()?;
            } else {
                self.attest_sidecar_source()?;
            }
            if find_controller(&self.paths.proc_root, &snapshot.instance)?.is_some() {
                return Err("selected controller returned during classifier cleanup".into());
            }
            attest_selected_sqm_absent(&self.paths, snapshot)
        };
        let cleanup = super::traffic_classifier::SelectedClassifier::prepare(
            &snapshot.instance,
            &snapshot.target_interface,
            &job,
            &UciPackage::default(),
            proof,
        )?;
        cleanup.apply(proof)
    }

    fn prepare_controller(&mut self, snapshot: &SelectedConfig) -> Result<(), String> {
        self.attest_unchanged(snapshot)?;
        let job = self.request.identity.job_id.clone();
        self.classifier = Some(super::traffic_classifier::SelectedClassifier::prepare(
            &snapshot.instance,
            &snapshot.target_interface,
            &job,
            &snapshot.cake,
            || self.attest_unchanged(snapshot),
        )?);
        self.registration = super::service_lifecycle::prepare_selected_generation(
            &snapshot.instance,
            &snapshot.cake,
            &snapshot.sqm,
            snapshot.controller_should_run,
        )?;
        self.attest_unchanged(snapshot)
    }

    fn register_controller(&mut self, snapshot: &SelectedConfig) -> Result<(), String> {
        self.attest_unchanged(snapshot)?;
        let mut arguments = vec![
            OsString::from("native_apply_register_instance"),
            OsString::from(&snapshot.instance),
        ];
        if let Some(registration) = &self.registration {
            let generation = registration.prepare_registration()?;
            if generation.is_some() != snapshot.controller_should_run {
                return Err("selected-generation-registration-direction-mismatch".into());
            }
            if let Some(generation) = generation {
                arguments.push(generation.into());
            }
        }
        if !snapshot.controller_should_run {
            return Ok(());
        }
        run_success(
            &SpawnSpec {
                program: self.paths.init.clone(),
                arguments,
                environment: vec![(
                    OsString::from("CAKE_AUTORATE_NATIVE_APPLY_RECOVERY"),
                    OsString::from("1"),
                )],
            },
            None,
            COMMAND_TIMEOUT,
            |command| self.lock.configure_borrowed_restart(command),
            "register the selected procd controller",
        )
    }
}

fn validate_request_binding(
    request: &OperationRequest,
    expected_target: &str,
) -> Result<(), String> {
    if !safe_name(&request.identity.instance)
        || !safe_interface(expected_target)
        || match request.managed_sqm_section.as_deref() {
            Some(section) => !safe_name(section),
            None => true,
        }
    {
        return Err("native Apply selected lifecycle identity is unsafe".to_string());
    }
    Ok(())
}

fn selected_config(
    cake: &UciPackage,
    sqm: &UciPackage,
    resolver: &impl InterfaceResolver,
    request: &OperationRequest,
    expected_target: &str,
    projection: SelectedProjectionState,
) -> Result<SelectedConfig, String> {
    let instance = &request.identity.instance;
    let expected_sqm = request
        .managed_sqm_section
        .as_deref()
        .ok_or_else(|| "native Apply request has no managed SQM section".to_string())?;
    let section = cake
        .sections
        .get(instance)
        .filter(|section| section.section_type == "cake_autorate")
        .ok_or_else(|| format!("native Apply instance {instance} is missing"))?;
    let configured_sqm = option(section, "sqm_section")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("cake_{instance}"));
    if configured_sqm != expected_sqm {
        return Err("native Apply instance-to-SQM binding changed".to_string());
    }
    let configured_target = first_option(section, &["sqm_interface", "ul_if", "wan_if"])
        .ok_or_else(|| "native Apply selected instance has no target interface".to_string())?;
    let resolved_target = resolver.resolve(&configured_target)?;
    if configured_target != expected_target && resolved_target != expected_target {
        return Err("native Apply selected target binding changed".to_string());
    }
    let manage_sqm = bool_option(section, "manage_sqm", true);
    let enabled = bool_option(section, "enabled", false);
    let sqm_enabled = bool_option(section, "sqm_enabled", enabled);
    let direction = option(section, "sqm_direction_mode")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "both".to_string());
    if !matches!(
        direction.as_str(),
        "both" | "download_only" | "upload_only" | "off"
    ) {
        return Err("native Apply selected SQM direction is invalid".to_string());
    }
    let sqm_should_run = manage_sqm && enabled && sqm_enabled && direction != "off";
    let controller_should_run = enabled && (!manage_sqm || sqm_should_run);
    let download_enabled = sqm_should_run && direction != "upload_only";
    let upload_enabled = sqm_should_run && direction != "download_only";
    let download_interface = option(section, "dl_if")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("ifb4{expected_target}"));
    let mut download_kbps = 0;
    let mut upload_kbps = 0;

    if let Some(queue) = sqm.sections.get(expected_sqm) {
        if queue.section_type != "queue" {
            return Err("native Apply selected SQM section type changed".to_string());
        }
        if option(queue, "_cake_autorate_managed").as_deref() != Some(instance.as_str()) {
            return Err("native Apply selected SQM owner changed".to_string());
        }
        if let Some(target) = option(queue, "interface").filter(|value| !value.is_empty()) {
            let resolved = resolver.resolve(&target)?;
            if target != expected_target && resolved != expected_target {
                return Err("native Apply selected SQM target changed".to_string());
            }
        } else if sqm_should_run {
            return Err("native Apply active SQM section has no interface".to_string());
        }
        if sqm_should_run {
            if option(queue, "enabled").as_deref() != Some("1") {
                return Err("native Apply active SQM section is disabled".to_string());
            }
            download_kbps = parse_rate(queue, "download")?;
            upload_kbps = parse_rate(queue, "upload")?;
            validate_selected_rates(
                projection,
                download_enabled,
                upload_enabled,
                download_kbps,
                upload_kbps,
            )?;
        }
    } else if sqm_should_run {
        return Err("native Apply active SQM section is missing".to_string());
    }

    Ok(SelectedConfig {
        cake: cake.clone(),
        sqm: sqm.clone(),
        instance: instance.clone(),
        sqm_section: expected_sqm.to_string(),
        target_interface: expected_target.to_string(),
        download_interface,
        controller_should_run,
        sqm_should_run,
        download_enabled,
        upload_enabled,
        download_kbps,
        upload_kbps,
    })
}

fn validate_selected_rates(
    projection: SelectedProjectionState,
    download_enabled: bool,
    upload_enabled: bool,
    download_kbps: u64,
    upload_kbps: u64,
) -> Result<(), String> {
    if projection == SelectedProjectionState::Pending {
        return Ok(());
    }
    if (download_enabled && download_kbps == 0) || (upload_enabled && upload_kbps == 0) {
        return Err("native Apply selected active SQM rate is missing".to_string());
    }
    if (!download_enabled && download_kbps != 0) || (!upload_enabled && upload_kbps != 0) {
        return Err("native Apply projected SQM rates do not match its direction".to_string());
    }
    Ok(())
}

fn find_controller(proc_root: &Path, instance: &str) -> Result<Option<ProcessIdentity>, String> {
    let mut found = None;
    for entry in fs::read_dir(proc_root)
        .map_err(|error| format!("unable to enumerate controller processes: {error}"))?
    {
        let entry = entry.map_err(|error| format!("unable to inspect process entry: {error}"))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        if pid <= 1 || !controller_cmdline_matches(&entry.path().join("cmdline"), instance)? {
            continue;
        }
        let identity = ProcessIdentity::inspect(proc_root, pid)?;
        if found.replace(identity).is_some() {
            return Err(format!(
                "multiple exact controllers are running for native Apply instance {instance}"
            ));
        }
    }
    Ok(found)
}

fn controller_cmdline_matches(path: &Path, instance: &str) -> Result<bool, String> {
    let Some(bytes) = super::identity::read_process_cmdline(path, MAX_CMDLINE + 1)
        .map_err(|error| format!("unable to read controller command line: {error}"))?
    else {
        return Ok(false);
    };
    let mut expected = Vec::with_capacity(DAEMON_PATH.len() + instance.len() + 13);
    expected.extend_from_slice(DAEMON_PATH.as_bytes());
    expected.push(0);
    expected.extend_from_slice(b"--instance");
    expected.push(0);
    expected.extend_from_slice(instance.as_bytes());
    expected.push(0);
    if bytes.len() > MAX_CMDLINE as usize {
        if bytes.starts_with(&expected) {
            return Err("controller command line exceeds its safety bound".to_string());
        }
        return Ok(false);
    }
    Ok(bytes == expected)
}

fn open_stable_pidfd(proc_root: &Path, identity: &ProcessIdentity) -> Result<OwnedFd, String> {
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, identity.pid, 0) };
    if raw < 0 {
        return Err(format!(
            "unable to open pidfd for selected controller: {}",
            io::Error::last_os_error()
        ));
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    if !identity.still_matches(proc_root)? {
        return Err("selected controller identity changed while opening pidfd".to_string());
    }
    Ok(fd)
}

fn wait_pidfd(pidfd: &OwnedFd, timeout: Duration) -> Result<(), String> {
    let started = Instant::now();
    loop {
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(
                "selected controller did not stop before its watchdog deadline".to_string(),
            );
        }
        let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
        let mut descriptor = libc::pollfd {
            fd: pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if result > 0 && descriptor.revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            return Ok(());
        }
        if result == 0 {
            return Err(
                "selected controller did not stop before its watchdog deadline".to_string(),
            );
        }
        if result < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            continue;
        }
        return Err(format!(
            "unable to wait for selected controller exit: {}",
            io::Error::last_os_error()
        ));
    }
}

fn attest_selected_sqm_absent(
    paths: &LifecyclePaths,
    snapshot: &SelectedConfig,
) -> Result<(), String> {
    if netdev_exists(paths, &snapshot.target_interface) {
        let output = tc_output(paths, &["qdisc", "show", "dev", &snapshot.target_interface])?;
        if output.lines().any(|line| {
            let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
            fields.first() == Some(&"qdisc")
                && matches!(
                    fields.get(1),
                    Some(&"cake") | Some(&"cake_mq") | Some(&"ingress") | Some(&"clsact")
                )
        }) {
            return Err("selected SQM upload or ingress runtime remains after stop".to_string());
        }
    }
    if netdev_exists(paths, &snapshot.download_interface) {
        let output = tc_output(
            paths,
            &["qdisc", "show", "dev", &snapshot.download_interface],
        )?;
        if output.lines().any(|line| {
            let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
            fields.first() == Some(&"qdisc")
                && matches!(fields.get(1), Some(&"cake") | Some(&"cake_mq"))
        }) {
            return Err("selected SQM download runtime remains after stop".to_string());
        }
    }
    Ok(())
}

fn remove_empty_clsact(paths: &LifecyclePaths, interface: &str) -> Result<(), String> {
    if !netdev_exists(paths, interface) {
        return Ok(());
    }
    let qdiscs = tc_output(paths, &["qdisc", "show", "dev", interface])?;
    if !qdiscs.lines().any(|line| {
        let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
        fields.first() == Some(&"qdisc") && fields.get(1) == Some(&"clsact")
    }) {
        return Ok(());
    }
    for hook in ["ingress", "egress"] {
        if !tc_output(paths, &["filter", "show", "dev", interface, hook])?
            .trim()
            .is_empty()
        {
            return Err(format!(
                "cannot replace clsact on selected SQM interface {interface}: foreign filters remain"
            ));
        }
    }
    run_success(
        &SpawnSpec {
            program: paths.tc.clone(),
            arguments: ["qdisc", "del", "dev", interface, "clsact"]
                .into_iter()
                .map(OsString::from)
                .collect(),
            environment: Vec::new(),
        },
        None,
        COMMAND_TIMEOUT,
        |_| {},
        "remove an empty selected clsact",
    )
}

fn tc_output(paths: &LifecyclePaths, arguments: &[&str]) -> Result<String, String> {
    let output = run_command(
        &SpawnSpec {
            program: paths.tc.clone(),
            arguments: arguments.iter().map(OsString::from).collect(),
            environment: Vec::new(),
        },
        None,
        COMMAND_TIMEOUT,
        |_| {},
    )?;
    if !output.status.success() {
        return Err(format!("tc inspection failed: {}", stderr(&output.stderr)));
    }
    String::from_utf8(output.stdout).map_err(|_| "tc inspection output is not UTF-8".to_string())
}

fn netdev_exists(paths: &LifecyclePaths, interface: &str) -> bool {
    paths.sys_class_net.join(interface).is_dir()
}

struct ContainmentSqmConfig {
    alias: FrozenSqmAlias,
    profile: super::sqm_runner::Profile,
}

fn containment_recipe(snapshot: &SelectedConfig, bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.len() > MAX_CONFIG_BYTES as usize {
        return Err("parent SQM snapshot exceeds its bound".into());
    }
    let queue = snapshot.sqm.sections.get(&snapshot.sqm_section);
    verify_scalar_uci_section(
        bytes,
        &snapshot.sqm_section,
        queue.map(|queue| (queue.section_type.as_str(), &queue.options)),
    )?;
    if snapshot.sqm.sections.iter().any(|(name, queue)| {
        name != &snapshot.sqm_section
            && queue.section_type == "queue"
            && queue.options.get("interface") == Some(&snapshot.target_interface)
    }) {
        return Err("selected containment target has another configured SQM owner".into());
    }
    let Some(queue) = queue else {
        return Ok(Vec::new());
    };
    if queue.section_type != "queue"
        || queue.options.get("_cake_autorate_managed") != Some(&snapshot.instance)
        || queue.options.get("interface") != Some(&snapshot.target_interface)
    {
        return Err("selected containment SQM recipe owner or target mismatch".into());
    }
    let mut edits = Vec::with_capacity(queue.options.len() + 1);
    edits.push(super::uci_edits::Edit::AddSection {
        section: snapshot.sqm_section.clone(),
        kind: "queue".into(),
    });
    for (option, value) in &queue.options {
        edits.push(super::uci_edits::Edit::Set {
            section: snapshot.sqm_section.clone(),
            option: option.clone(),
            value: value.clone(),
        });
    }
    super::uci_edits::render(&[], &edits)
}

impl ContainmentSqmConfig {
    fn prepare(
        paths: &LifecyclePaths,
        snapshot: &SelectedConfig,
        bytes: &[u8],
    ) -> Result<Self, String> {
        let recipe = containment_recipe(snapshot, bytes)?;
        let profile = super::sqm_runner::Profile::inspect(&paths.sqm_runner)?;
        let alias = FrozenSqmAlias::materialize(&recipe, &paths.snapshot_root, &paths.uci)?;
        let show = alias.section_show(&snapshot.sqm_section)?;
        let native = UciPackage::parse(
            SQM_PACKAGE,
            std::str::from_utf8(&show).map_err(|_| "selected containment alias is not text")?,
        )?;
        if native.sections.get(&snapshot.sqm_section)
            != snapshot.sqm.sections.get(&snapshot.sqm_section)
        {
            return Err("selected containment alias changed the frozen SQM recipe".into());
        }
        // Prove alias/executable leases before stopping any healthy process.
        drop(profile.clone().bind(&alias, &paths.snapshot_root)?);
        Ok(Self { alias, profile })
    }
}

fn run_command<C>(
    spec: &SpawnSpec,
    input: Option<&[u8]>,
    timeout: Duration,
    configure: C,
) -> Result<super::process::BoundedCommandOutput, String>
where
    C: FnOnce(&mut std::process::Command),
{
    run_bounded_command_output_with_input(spec, input, timeout, MAX_OUTPUT, || false, configure)
}

fn run_success<C>(
    spec: &SpawnSpec,
    input: Option<&[u8]>,
    timeout: Duration,
    configure: C,
    operation: &str,
) -> Result<(), String>
where
    C: FnOnce(&mut std::process::Command),
{
    let output = run_command(spec, input, timeout, configure)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("unable to {operation}: {}", stderr(&output.stderr)))
    }
}

fn stderr(bytes: &[u8]) -> String {
    let value = String::from_utf8_lossy(bytes).trim().to_string();
    if value.is_empty() {
        "command exited unsuccessfully".to_string()
    } else {
        value
    }
}

fn option(section: &UciSection, key: &str) -> Option<String> {
    section.options.get(key).cloned()
}

fn first_option(section: &UciSection, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| option(section, key).filter(|value| !value.is_empty()))
}

fn bool_option(section: &UciSection, key: &str, default: bool) -> bool {
    option(section, key).map_or(default, |value| value == "1")
}

fn parse_rate(section: &UciSection, key: &str) -> Result<u64, String> {
    option(section, key)
        .ok_or_else(|| format!("native Apply SQM option {key} is missing"))?
        .parse::<u64>()
        .ok()
        .filter(|value| *value <= 10_000_000_000)
        .ok_or_else(|| format!("native Apply SQM option {key} is invalid"))
}

fn env_path(name: &str, default: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeBackend {
        events: Vec<&'static str>,
        snapshots: Vec<SelectedConfig>,
        projection: SqmProjectionPlan,
        fail_attestation_at: usize,
        attestations: usize,
        fail_prepare: bool,
        fail_stop_sqm: bool,
        fail_clear_classifier: bool,
    }

    impl SelectedLifecycleBackend for FakeBackend {
        fn prepare_mqtt(&mut self, _: &SelectedConfig, _: bool) -> Result<(), String> {
            self.events.push("prepare-mqtt");
            Ok(())
        }
        fn stop_mqtt(&mut self) -> Result<(), String> {
            self.events.push("stop-mqtt");
            Ok(())
        }
        fn finish_mqtt(&mut self) -> Result<(), String> {
            self.events.push("finish-mqtt");
            Ok(())
        }
        fn project(&mut self, _: &str) -> Result<SqmProjectionPlan, String> {
            self.events.push("project");
            Ok(self.projection.clone())
        }
        fn snapshot(
            &mut self,
            projection: SelectedProjectionState,
        ) -> Result<SelectedConfig, String> {
            self.events.push(match projection {
                SelectedProjectionState::Pending => "snapshot-pending",
                SelectedProjectionState::Exact => "snapshot-exact",
            });
            let snapshot = if self.snapshots.len() > 1 {
                self.snapshots.remove(0)
            } else {
                self.snapshots[0].clone()
            };
            if snapshot.sqm_should_run {
                validate_selected_rates(
                    projection,
                    snapshot.download_enabled,
                    snapshot.upload_enabled,
                    snapshot.download_kbps,
                    snapshot.upload_kbps,
                )?;
            }
            Ok(snapshot)
        }
        fn attest_unchanged(&mut self, _: &SelectedConfig) -> Result<(), String> {
            self.events.push("attest");
            self.attestations += 1;
            if self.fail_attestation_at == self.attestations {
                Err("drift".to_string())
            } else {
                Ok(())
            }
        }
        fn freeze_sqm(&mut self, _: &SelectedConfig) -> Result<(), String> {
            self.events.push("freeze-sqm");
            Ok(())
        }
        fn prepare_controller(&mut self, _: &SelectedConfig) -> Result<(), String> {
            self.events.push("prepare-controller");
            if self.fail_prepare {
                Err("input-capacity".into())
            } else {
                Ok(())
            }
        }
        fn stop_controller(&mut self, _: &str) -> Result<(), String> {
            self.events.push("stop-controller");
            Ok(())
        }
        fn stop_sqm(&mut self, _: &SelectedConfig) -> Result<(), String> {
            self.events.push("stop-sqm");
            if self.fail_stop_sqm {
                Err("sqm-still-running".into())
            } else {
                Ok(())
            }
        }
        fn prepare_ingress(&mut self, _: &SelectedConfig) -> Result<(), String> {
            self.events.push("prepare-ingress");
            Ok(())
        }
        fn start_sqm(&mut self, _: &SelectedConfig) -> Result<(), String> {
            self.events.push("start-sqm");
            Ok(())
        }
        fn apply_classifier(&mut self, _snapshot: &SelectedConfig) -> Result<(), String> {
            self.events.push("classifier");
            Ok(())
        }
        fn clear_classifier(&mut self, _snapshot: &SelectedConfig) -> Result<(), String> {
            self.events.push("clear-classifier");
            if self.fail_clear_classifier {
                Err("classifier-cleanup-failed".into())
            } else {
                Ok(())
            }
        }
        fn register_controller(&mut self, snapshot: &SelectedConfig) -> Result<(), String> {
            self.events.push("finalize-generation");
            if snapshot.controller_should_run {
                self.events.push("register");
            }
            Ok(())
        }
    }

    fn config(active: bool, download: bool, controller: bool) -> SelectedConfig {
        SelectedConfig {
            cake: UciPackage::default(),
            sqm: UciPackage::default(),
            instance: "wan_sqm".to_string(),
            sqm_section: "cake_wan_sqm".to_string(),
            target_interface: "pppoe-wan".to_string(),
            download_interface: "ifb4pppoe-wan".to_string(),
            controller_should_run: controller,
            sqm_should_run: active,
            download_enabled: download,
            upload_enabled: active,
            download_kbps: if active && download { 80_000 } else { 0 },
            upload_kbps: if active { 20_000 } else { 0 },
        }
    }

    fn projection(active: bool, download: bool) -> SqmProjectionPlan {
        let mut interfaces = std::collections::BTreeSet::new();
        let mut ingress = std::collections::BTreeSet::new();
        if active {
            interfaces.insert("pppoe-wan".to_string());
        }
        if download {
            ingress.insert("pppoe-wan".to_string());
        }
        SqmProjectionPlan::test_summary(
            active,
            interfaces,
            ingress,
            std::collections::BTreeSet::new(),
        )
    }

    #[test]
    fn restart_order_is_state_driven_and_fenced_between_mutations() {
        let cfg = config(true, true, true);
        let mut backend = FakeBackend {
            snapshots: vec![cfg.clone(), cfg],
            projection: projection(true, true),
            ..FakeBackend::default()
        };
        execute_selected_lifecycle(SelectedLifecycleAction::Restart, &mut backend).unwrap();
        assert_eq!(
            backend.events,
            [
                "snapshot-exact",
                "project",
                "snapshot-exact",
                "attest",
                "prepare-mqtt",
                "prepare-controller",
                "attest",
                "freeze-sqm",
                "attest",
                "stop-mqtt",
                "stop-controller",
                "attest",
                "stop-sqm",
                "attest",
                "prepare-ingress",
                "attest",
                "start-sqm",
                "attest",
                "classifier",
                "attest",
                "finalize-generation",
                "register",
                "finish-mqtt",
                "attest"
            ]
        );
    }

    #[test]
    fn disabled_candidate_stops_runtime_without_restarting_sqm_or_controller() {
        let cfg = config(false, false, false);
        let mut backend = FakeBackend {
            snapshots: vec![cfg.clone(), cfg],
            projection: projection(false, false),
            ..FakeBackend::default()
        };
        execute_selected_lifecycle(SelectedLifecycleAction::Restart, &mut backend).unwrap();
        assert!(!backend.events.contains(&"start-sqm"));
        assert!(!backend.events.contains(&"register"));
        assert!(backend.events.contains(&"classifier"));
        assert!(backend.events.contains(&"finalize-generation"));
    }

    #[test]
    fn upload_only_candidate_is_verified_without_persistent_projection() {
        let exact = config(true, false, true);
        let mut backend = FakeBackend {
            snapshots: vec![exact.clone(), exact],
            projection: projection(true, false),
            ..FakeBackend::default()
        };
        execute_selected_lifecycle(SelectedLifecycleAction::Restart, &mut backend).unwrap();
        assert_eq!(
            &backend.events[..3],
            ["snapshot-exact", "project", "snapshot-exact"]
        );
        assert!(!backend.events.contains(&"prepare-ingress"));
        assert!(backend.events.contains(&"start-sqm"));
    }

    #[test]
    fn pending_accepts_every_previous_direction_until_exact_projection() {
        let directions = [
            ("both", true, true, 80_000, 20_000),
            ("download_only", true, false, 80_000, 0),
            ("upload_only", false, true, 0, 20_000),
            ("off", false, false, 0, 0),
        ];

        for (previous, _, _, download_kbps, upload_kbps) in directions {
            for (selected, download_enabled, upload_enabled, _, _) in directions {
                validate_selected_rates(
                    SelectedProjectionState::Pending,
                    download_enabled,
                    upload_enabled,
                    download_kbps,
                    upload_kbps,
                )
                .unwrap_or_else(|error| {
                    panic!("pending {previous} -> {selected} was rejected: {error}")
                });

                let exact = validate_selected_rates(
                    SelectedProjectionState::Exact,
                    download_enabled,
                    upload_enabled,
                    download_kbps,
                    upload_kbps,
                );
                assert_eq!(
                    exact.is_ok(),
                    download_enabled == (download_kbps != 0)
                        && upload_enabled == (upload_kbps != 0),
                    "unexpected exact validation for {previous} -> {selected}"
                );
            }
        }
    }

    #[test]
    fn exact_projection_rejects_missing_active_and_stale_bypassed_rates() {
        assert!(
            validate_selected_rates(SelectedProjectionState::Exact, true, true, 0, 20_000,)
                .unwrap_err()
                .contains("active SQM rate")
        );
        assert!(validate_selected_rates(
            SelectedProjectionState::Exact,
            false,
            true,
            80_000,
            20_000,
        )
        .unwrap_err()
        .contains("projected SQM rates"));
    }

    #[test]
    fn configuration_drift_stops_before_the_next_mutation() {
        let cfg = config(true, true, true);
        let mut backend = FakeBackend {
            snapshots: vec![cfg.clone(), cfg],
            projection: projection(true, true),
            fail_attestation_at: 4,
            ..FakeBackend::default()
        };
        assert!(
            execute_selected_lifecycle(SelectedLifecycleAction::Restart, &mut backend).is_err()
        );
        assert!(backend.events.contains(&"stop-controller"));
        assert!(!backend.events.contains(&"stop-sqm"));
    }

    #[test]
    fn r4_selected_input_preparation_failure_preserves_the_healthy_runtime() {
        let cfg = config(true, true, true);
        let mut backend = FakeBackend {
            snapshots: vec![cfg.clone(), cfg],
            projection: projection(true, true),
            fail_prepare: true,
            ..FakeBackend::default()
        };
        assert_eq!(
            execute_selected_lifecycle(SelectedLifecycleAction::Restart, &mut backend).unwrap_err(),
            "input-capacity"
        );
        assert!(backend.events.contains(&"prepare-controller"));
        for mutation in [
            "freeze-sqm",
            "stop-controller",
            "stop-sqm",
            "start-sqm",
            "register",
        ] {
            assert!(!backend.events.contains(&mutation));
        }
    }

    #[test]
    fn stop_action_never_projects_or_starts_any_runtime() {
        let cfg = config(true, true, true);
        let mut backend = FakeBackend {
            snapshots: vec![cfg],
            ..FakeBackend::default()
        };
        execute_selected_lifecycle(SelectedLifecycleAction::Stop, &mut backend).unwrap();
        assert_eq!(
            backend.events,
            [
                "snapshot-exact",
                "attest",
                "prepare-mqtt",
                "freeze-sqm",
                "attest",
                "stop-mqtt",
                "stop-controller",
                "attest",
                "stop-sqm",
                "attest",
                "clear-classifier",
                "finish-mqtt",
                "attest"
            ]
        );
    }

    #[test]
    fn r4_selected_stop_refuses_cleanup_without_stopped_sqm_and_propagates_cleanup_failure() {
        for (fail_stop_sqm, fail_clear_classifier, expected) in [
            (true, false, "sqm-still-running"),
            (false, true, "classifier-cleanup-failed"),
        ] {
            let mut backend = FakeBackend {
                snapshots: vec![config(true, true, true)],
                fail_stop_sqm,
                fail_clear_classifier,
                ..FakeBackend::default()
            };
            assert_eq!(
                execute_selected_lifecycle(SelectedLifecycleAction::Stop, &mut backend)
                    .unwrap_err(),
                expected
            );
            assert_eq!(backend.events.contains(&"clear-classifier"), !fail_stop_sqm);
            assert_eq!(
                backend.events.last(),
                Some(&if fail_stop_sqm {
                    "stop-sqm"
                } else {
                    "clear-classifier"
                })
            );
        }
    }

    #[test]
    fn r4_selected_stop_rechecks_source_before_and_after_classifier_cleanup() {
        for fail_attestation_at in 1..=5 {
            let mut backend = FakeBackend {
                snapshots: vec![config(true, true, true)],
                fail_attestation_at,
                ..FakeBackend::default()
            };
            assert_eq!(
                execute_selected_lifecycle(SelectedLifecycleAction::Stop, &mut backend)
                    .unwrap_err(),
                "drift"
            );
            assert_eq!(backend.events.last(), Some(&"attest"));
            assert_eq!(
                backend.events.contains(&"clear-classifier"),
                fail_attestation_at == 5
            );
        }
    }

    #[test]
    fn exact_controller_cmdline_rejects_prefixes_and_extra_arguments() {
        let root = std::env::temp_dir().join(format!(
            "cake-selected-lifecycle-cmdline-{}-{}",
            std::process::id(),
            NEXT_SNAPSHOT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        for (name, bytes, expected) in [
            (
                "exact",
                b"/usr/sbin/cake-autorated\0--instance\0wan_sqm\0".as_slice(),
                true,
            ),
            (
                "prefix",
                b"/usr/sbin/cake-autorated-helper\0--instance\0wan_sqm\0".as_slice(),
                false,
            ),
            (
                "extra",
                b"/usr/sbin/cake-autorated\0--instance\0wan_sqm\0--once\0".as_slice(),
                false,
            ),
            (
                "extra-empty",
                b"/usr/sbin/cake-autorated\0--instance\0wan_sqm\0\0".as_slice(),
                false,
            ),
        ] {
            let path = root.join(name);
            fs::write(&path, bytes).unwrap();
            assert_eq!(
                controller_cmdline_matches(&path, "wan_sqm").unwrap(),
                expected
            );
        }
        let unrelated = root.join("unrelated-long");
        let mut unrelated_cmdline = b"/usr/bin/apcontroller\0".to_vec();
        unrelated_cmdline.extend(std::iter::repeat_n(b'x', MAX_CMDLINE as usize + 32));
        unrelated_cmdline.push(0);
        fs::write(&unrelated, unrelated_cmdline).unwrap();
        assert!(!controller_cmdline_matches(&unrelated, "wan_sqm").unwrap());

        let oversized = root.join("owned-long");
        let mut oversized_cmdline = b"/usr/sbin/cake-autorated\0--instance\0wan_sqm\0".to_vec();
        oversized_cmdline.extend(std::iter::repeat_n(b'y', MAX_CMDLINE as usize + 32));
        fs::write(&oversized, oversized_cmdline).unwrap();
        assert!(controller_cmdline_matches(&oversized, "wan_sqm")
            .unwrap_err()
            .contains("exceeds its safety bound"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "requires explicit inspected SDK UCI and loader paths"]
    fn r4_selected_source_is_committed_and_never_reads_original_package_deltas() {
        let root = std::env::temp_dir().join(format!(
            "cake-selected-committed-{}-{}",
            std::process::id(),
            NEXT_SNAPSHOT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let config = root.join("config");
        let delta = root.join("delta");
        fs::create_dir(&config).unwrap();
        fs::create_dir(&delta).unwrap();
        fs::write(
            config.join(CAKE_PACKAGE),
            b"config cake_autorate 'lab'\n option ul_if 'fixture0'\n",
        )
        .unwrap();
        fs::write(config.join(SQM_PACKAGE), b"config queue 'cake_lab'\n option interface 'fixture0'\n option _cake_autorate_managed 'lab'\n").unwrap();
        fs::write(
            delta.join(SQM_PACKAGE),
            b"sqm.cake_lab.interface='wrong-target'\n",
        )
        .unwrap();
        fs::write(
            delta.join(CAKE_PACKAGE),
            b"cake-autorate.lab.ul_if='wrong-target'\n",
        )
        .unwrap();
        let quote = |value: &std::ffi::OsStr| {
            format!("'{}'", value.to_str().unwrap().replace('\'', "'\\''"))
        };
        let binary = std::env::var_os("CAKE_TEST_UCI").expect("explicit UCI required");
        let command = if let Some(loader) = std::env::var_os("CAKE_TEST_MUSL_LOADER") {
            format!(
                "{} --library-path {} {}",
                quote(&loader),
                quote(&std::env::var_os("CAKE_TEST_LIB_DIR").expect("explicit libraries required")),
                quote(&binary)
            )
        } else {
            quote(&binary)
        };
        let uci = root.join("uci");
        fs::write(&uci, format!("#!/bin/sh\nfor last do :; done\ncase \"$last\" in cu??????????????????????????????) ;; *) exit 91;; esac\nexec {command} -p {} \"$@\"\n", quote(delta.as_os_str()))).unwrap();
        fs::set_permissions(&uci, fs::Permissions::from_mode(0o700)).unwrap();
        let paths = LifecyclePaths {
            uci,
            proc_root: root.join("proc"),
            sys_class_net: root.join("sys"),
            ubus: root.join("ubus"),
            init: root.join("init"),
            sqm_runner: root.join("run.sh"),
            sqm_config: config.join(SQM_PACKAGE),
            tc: root.join("tc"),
            snapshot_root: root.join("snapshots"),
        };
        let original = [CAKE_PACKAGE, SQM_PACKAGE].map(|name| fs::read(config.join(name)).unwrap());
        let source = capture_selected_source(&paths).unwrap();
        assert_eq!(
            source.config().package(CAKE_PACKAGE).unwrap().sections["lab"].options["ul_if"],
            "fixture0"
        );
        assert_eq!(
            source.config().package(SQM_PACKAGE).unwrap().sections["cake_lab"].options["interface"],
            "fixture0"
        );
        source.attest().unwrap();
        for (name, bytes) in [CAKE_PACKAGE, SQM_PACKAGE].into_iter().zip(&original) {
            assert_eq!(fs::read(config.join(name)).unwrap(), *bytes);
        }
        assert!(!config.join(".start-uci").exists());
        fs::write(root.join("new-inode"), &original[0]).unwrap();
        fs::rename(root.join("new-inode"), config.join(CAKE_PACKAGE)).unwrap();
        assert!(source.attest().is_err());
        assert_eq!(
            fs::read(delta.join(SQM_PACKAGE)).unwrap(),
            b"sqm.cake_lab.interface='wrong-target'\n"
        );
        assert_eq!(
            fs::read(delta.join(CAKE_PACKAGE)).unwrap(),
            b"cake-autorate.lab.ul_if='wrong-target'\n"
        );
    }

    fn containment_fixture() -> (SelectedConfig, Vec<u8>) {
        let mut snapshot = config(false, false, false);
        snapshot.sqm = UciPackage::parse("sqm", "sqm.cake_wan_sqm=queue\nsqm.cake_wan_sqm.enabled='1'\nsqm.cake_wan_sqm.interface='pppoe-wan'\nsqm.cake_wan_sqm._cake_autorate_managed='wan_sqm'\nsqm.peer=queue\nsqm.peer.interface='eth9'\nsqm.peer.script='foreign-never-execute.qos'\n").unwrap();
        let bytes = b"config queue 'cake_wan_sqm'\n option enabled '1'\n option interface 'pppoe-wan'\n option _cake_autorate_managed 'wan_sqm'\nconfig queue 'peer'\n option interface 'eth9'\n option script 'foreign-never-execute.qos'\n".to_vec();
        (snapshot, bytes)
    }

    #[test]
    fn r4_containment_recipe_keeps_only_parent_owned_queue_and_refuses_aliasing() {
        let (snapshot, bytes) = containment_fixture();
        let rendered = containment_recipe(&snapshot, &bytes).unwrap();
        assert!(!String::from_utf8_lossy(&rendered).contains("foreign-never-execute"));
        assert!(!String::from_utf8_lossy(&rendered).contains("eth9"));
        verify_scalar_uci_section(
            &rendered,
            &snapshot.sqm_section,
            Some((
                "queue",
                &snapshot.sqm.sections[&snapshot.sqm_section].options,
            )),
        )
        .unwrap();
        let mut conflict = snapshot.clone();
        conflict
            .sqm
            .sections
            .get_mut("peer")
            .unwrap()
            .options
            .insert("interface".into(), snapshot.target_interface.clone());
        let conflict_bytes = String::from_utf8(bytes.clone())
            .unwrap()
            .replace("interface 'eth9'", "interface 'pppoe-wan'");
        assert!(containment_recipe(&conflict, conflict_bytes.as_bytes()).is_err());
        let mut wrong_owner = snapshot.clone();
        wrong_owner.instance = "other_owner".into();
        assert!(containment_recipe(&wrong_owner, &bytes).is_err());
        let mut wrong = snapshot.clone();
        wrong
            .sqm
            .sections
            .get_mut(&snapshot.sqm_section)
            .unwrap()
            .options
            .insert("interface".into(), "different".into());
        assert!(containment_recipe(&wrong, &bytes).is_err());
        let absent = config(false, false, false);
        let peer_only = b"config queue 'peer'\n option interface 'eth9'\n";
        assert!(containment_recipe(&absent, peer_only).unwrap().is_empty());
        assert!(containment_recipe(&absent, &bytes).is_err());
    }

    #[test]
    #[ignore = "requires explicit inspected SDK UCI and runner paths"]
    fn r4_containment_alias_ignores_public_changes_and_original_package_deltas() {
        let root = std::env::temp_dir().join(format!(
            "cake-containment-alias-{}-{}",
            std::process::id(),
            NEXT_SNAPSHOT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let delta = root.join("delta");
        fs::create_dir(&delta).unwrap();
        let poison = b"sqm.cake_wan_sqm.interface='wrong-target'\n";
        fs::write(delta.join("sqm"), poison).unwrap();
        let quote = |value: &std::ffi::OsStr| {
            format!("'{}'", value.to_str().unwrap().replace('\'', "'\\''"))
        };
        let binary = std::env::var_os("CAKE_TEST_UCI").expect("explicit UCI required");
        let command = if let Some(loader) = std::env::var_os("CAKE_TEST_MUSL_LOADER") {
            format!(
                "{} --library-path {} {}",
                quote(&loader),
                quote(&std::env::var_os("CAKE_TEST_LIB_DIR").expect("explicit libraries required")),
                quote(&binary)
            )
        } else {
            quote(&binary)
        };
        let uci = root.join("uci");
        fs::write(&uci, format!("#!/bin/sh\nfor last do :; done\ncase \"$last\" in cu??????????????????????????????) ;; *) exit 91;; esac\nexec {command} -p {} \"$@\"\n", quote(delta.as_os_str()))).unwrap();
        fs::set_permissions(&uci, fs::Permissions::from_mode(0o700)).unwrap();
        let paths = LifecyclePaths {
            uci,
            proc_root: root.join("proc"),
            sys_class_net: root.join("sys"),
            ubus: root.join("ubus"),
            init: root.join("init"),
            sqm_runner: std::env::var_os("CAKE_TEST_SQM_RUNNER")
                .expect("explicit runner required")
                .into(),
            sqm_config: root.join("public-sqm"),
            tc: root.join("tc"),
            snapshot_root: root.join("snapshots"),
        };
        let (snapshot, bytes) = containment_fixture();
        fs::write(&paths.sqm_config, b"unrelated public state\n").unwrap();
        let frozen = ContainmentSqmConfig::prepare(&paths, &snapshot, &bytes).unwrap();
        fs::write(&paths.sqm_config, b"later public state\n").unwrap();
        let show = frozen.alias.section_show(&snapshot.sqm_section).unwrap();
        let parsed = UciPackage::parse("sqm", std::str::from_utf8(&show).unwrap()).unwrap();
        assert_eq!(
            parsed.sections.get(&snapshot.sqm_section),
            snapshot.sqm.sections.get(&snapshot.sqm_section)
        );
        // Execute only a synthetic, hash-attested shell runner. It loads the
        // real SDK UCI alias but contains no SQM, tc, or device commands.
        let script = root.join("safe-runner");
        let body = format!("#!/bin/sh\nconfig_load() {{ {} -c \"$UCI_CONFIG_DIR\" -q show \"$1\"; }}\n[ -e /proc/self/fd/6 ] && [ -e /proc/self/fd/7 ] || exit 92\n    config_load sqm\nprintf '%s:%s\\n' \"$1\" \"$2\"\n", quote(paths.uci.as_os_str()));
        fs::write(&script, &body).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let profile = super::super::sqm_runner::Profile::fixture(
            &script,
            &crate::config_candidate::digest(body.as_bytes()),
        )
        .unwrap();
        let runner = profile.bind(&frozen.alias, &paths.snapshot_root).unwrap();
        let output = runner
            .stop(
                &snapshot.target_interface,
                || frozen.alias.sqm_runner_alias().map(|_| ()),
                || false,
            )
            .unwrap();
        assert!(output.status.success());
        let output = String::from_utf8(output.stdout).unwrap();
        assert!(output.contains("stop:pppoe-wan"));
        assert!(output.contains(".interface='pppoe-wan'"));
        assert!(!output.contains("foreign-never-execute"));
        assert!(!output.contains("wrong-target"));
        drop(runner);
        assert_eq!(fs::read(delta.join("sqm")).unwrap(), poison);
        assert_eq!(
            fs::read(&paths.sqm_config).unwrap(),
            b"later public state\n"
        );
        let (directory, alias) = frozen.alias.sqm_runner_alias().unwrap();
        fs::write(directory.join(alias), b"tampered alias\n").unwrap();
        assert!(frozen.alias.sqm_runner_alias().is_err());
        drop(frozen);
        let absent = config(false, false, false);
        assert!(ContainmentSqmConfig::prepare(&paths, &absent, b"").is_err());
        let empty_paths = LifecyclePaths {
            snapshot_root: root.join("empty-snapshots"),
            ..paths
        };
        let empty = ContainmentSqmConfig::prepare(&empty_paths, &absent, b"").unwrap();
        assert!(empty
            .alias
            .section_show(&absent.sqm_section)
            .unwrap()
            .is_empty());
    }
}
