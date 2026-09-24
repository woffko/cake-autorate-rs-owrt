//! Ordinary OpenWrt service lifecycle authority.
//!
//! rc.common remains responsible only for declaring procd instances and for
//! carrying the historical global lock descriptor across its stop callback.
//! Every decision about presets, SQM, ingress preparation and which controller
//! instances are startable is made here from one typed configuration snapshot.

use super::committed_uci::{CommittedSnapshot, PreparedConfig};

#[cfg(feature = "calibration")]
use super::coordinator::native_apply_recovery_markers_present;
use super::event_loop::CalibrationEventLoop;
use super::identity::ProcessIdentity;
#[cfg(feature = "calibration")]
use super::mqtt_publisher::{cleanup_production_service_plans, publish_production_service_plans};
use super::procd_control::delete_service_or_attest_absent;
use super::process::{run_bounded_command_output_with_input, BoundedCommandOutput, SpawnSpec};
use super::runtime_health::{json_nonnegative_f64_value, json_string_value};
use super::runtime_health::{safe_interface, safe_name, UciPackage, UciSection};
use super::service_config::{prepare_preset_edits, InterfaceResolver, OpenWrtEnvironment};
use super::sqm_projection::plan_projection;
use super::sqm_projection::{ProjectionScope, SqmProjectionPlan};
#[cfg(test)]
use super::sqm_recovery_openwrt::ManagedSqmRatePolicy;
use super::sqm_recovery_openwrt::{
    error_message as sqm_error_message, managed_sqm_rate_policy_from_options,
    stop_managed_sqm_from_committed, ManagedSqmAttestationSpec, ManagedSqmStopSpec,
    NativeSqmAttestationError, OpenWrtPaths,
};
use super::sqm_start_events::{SqmStartEvents, StartEvents};
#[cfg(feature = "calibration")]
use super::traffic_classifier::run_traffic_classifier;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

const CAKE_PACKAGE: &str = "cake-autorate";
const SQM_PACKAGE: &str = "sqm";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const SQM_START_ATTEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONTROLLER_STOP_TIMEOUT: Duration = Duration::from_secs(15);
const CONTROLLER_START_TIMEOUT: Duration = Duration::from_secs(90);
const CONTROLLER_STATUS_MAX_AGE: f64 = 15.0;
const CONTROLLER_STATUS_FUTURE_SKEW: f64 = 2.0;
const MAX_OUTPUT: usize = 64 * 1024;
const MAX_INSTANCES: usize = 64;
const MAX_CMDLINE: u64 = 4096;
const MAX_BRIDGER_CONFIG_BYTES: usize = 256 * 1024;
const SERVICE_NAME: &str = "cake-autorate";
#[cfg(test)]
#[path = "service_reload_integration.rs"]
mod reload_integration;
#[path = "service_reload_runtime.rs"]
mod reload_runtime;
#[cfg(feature = "calibration")]
#[path = "mqtt_selected_runtime.rs"]
pub(crate) mod selected_mqtt;
const DAEMON_PATH: &str = "/usr/sbin/cake-autorated";
const SERVICE_START_DEFERRED_V1: &str = "service-start-deferred-v1";
static REPLACE_SEQUENCE: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Debug, PartialEq)]
struct ServiceStartPlan {
    instances: Vec<String>,
    managed: Vec<ManagedSqmAttestationSpec>,
}

/// Configuration delta only: applying it still requires durable publication,
/// exact process ownership and readiness proof. Old SQM recipes come from the
/// accepted generations, never from a newly committed replacement recipe.
#[derive(Debug, PartialEq)]
struct ServiceReloadDelta {
    retained: BTreeMap<String, String>,
    stop_controllers: Vec<String>,
    start_controllers: Vec<String>,
    stop_sqm: Vec<ManagedSqmStopSpec>,
    start_sqm: Vec<ManagedSqmAttestationSpec>,
    publication_required: bool,
}

impl ServiceReloadDelta {
    fn prepare(
        candidate: &StartCandidate,
        batch: &super::controller_input::Batch,
        resolver: &impl InterfaceResolver,
    ) -> Result<Self, String> {
        batch.attest_settled()?;
        let delta = Self::compare(candidate, batch, resolver)?;
        batch.attest_settled()?;
        Ok(delta)
    }

    // Pure comparison also serves replay against a pending reload's immutable
    // predecessor. Fresh preparation must use the settled admission above.
    fn compare(
        candidate: &StartCandidate,
        batch: &super::controller_input::Batch,
        resolver: &impl InterfaceResolver,
    ) -> Result<Self, String> {
        candidate.attest_plan(resolver)?;
        batch.attest_record()?;
        let inputs = batch.load_inputs()?;
        for input in inputs.values() {
            input.guard.attest_applied()?;
        }
        let old = stop_plan_from_views(
            &UciPackage::default(),
            &UciPackage::default(),
            &inputs,
            resolver,
        )?;
        let mut delta = Self {
            retained: BTreeMap::new(),
            stop_controllers: Vec::new(),
            start_controllers: Vec::new(),
            stop_sqm: Vec::new(),
            start_sqm: Vec::new(),
            publication_required: false,
        };
        for name in [CAKE_PACKAGE, SQM_PACKAGE] {
            delta.publication_required |=
                candidate.config.candidate_bytes(name)? != candidate.config.original_bytes(name)?;
        }
        for (name, input) in &inputs {
            let desired = candidate.plan.instances.contains(name);
            let next_sqm = candidate
                .plan
                .managed
                .iter()
                .find(|spec| &spec.instance == name);
            let previous: Vec<_> = old
                .managed
                .iter()
                .filter(|spec| &spec.instance == name)
                .collect();
            // Controller-only edits (including history allocation) must not
            // rebuild identical queues. A changed IFB/physical mapping is a
            // topology change even when the raw queue options did not change.
            let same_sqm = match (previous.as_slice(), next_sqm) {
                ([], None) => true,
                ([before], Some(after)) => {
                    before.sqm_section == after.sqm_section
                        && before.target_interface == after.target_interface
                        && before.download_interface == after.download_interface
                        && input.sqm_show
                            == super::controller_input::prepared_sqm_show(&candidate.config, name)?
                }
                _ => false,
            };
            if desired && same_sqm && input.matches_prepared(&candidate.config)? {
                delta
                    .retained
                    .insert(name.clone(), input.guard.generation().into());
            } else {
                delta.stop_controllers.push(name.clone());
                if desired {
                    delta.start_controllers.push(name.clone());
                }
            }
            if !same_sqm {
                delta.stop_sqm.extend(previous.into_iter().cloned());
                if let Some(spec) = next_sqm {
                    delta.start_sqm.push(spec.clone());
                }
            }
        }
        for name in &candidate.plan.instances {
            if !inputs.contains_key(name) {
                delta.start_controllers.push(name.clone());
                if let Some(spec) = candidate
                    .plan
                    .managed
                    .iter()
                    .find(|spec| &spec.instance == name)
                {
                    delta.start_sqm.push(spec.clone());
                }
            }
        }
        // Deterministic execution ordering also when additions sort before
        // replacements. Every affected old queue stops before any new starts.
        delta.start_controllers.sort();
        delta.start_sqm.sort_by(|a, b| a.instance.cmp(&b.instance));
        for input in inputs.values() {
            input.guard.attest_applied()?;
        }
        batch.attest_record()?;
        candidate.attest_plan(resolver)?;
        Ok(delta)
    }
}

/// Prepared in private aliases only. The publisher/runtime cutover must retain
/// this original-source authority; capturing a newer baseline is not recovery.
struct StartCandidate {
    config: PreparedConfig,
    projection: SqmProjectionPlan,
    plan: ServiceStartPlan,
}
impl StartCandidate {
    /// Validate the complete native candidate before probing MQ capabilities.
    /// The provisional resolver is confined to this method: no candidate may
    /// escape until its projection is revalidated against actual boot facts.
    fn prepare_with_mq_refresh(
        snapshot: CommittedSnapshot,
        resolver: &impl InterfaceResolver,
        uci: &Path,
        mut ensure: impl FnMut(&str) -> Result<(), String>,
    ) -> Result<Self, String> {
        struct Requests<'a, R> {
            resolver: &'a R,
            scripts: std::cell::RefCell<BTreeSet<String>>,
        }
        impl<R: InterfaceResolver> InterfaceResolver for Requests<'_, R> {
            fn resolve(&self, name: &str) -> Result<String, String> {
                self.resolver.resolve(name)
            }
            fn supports_cake_mq(&self, script: &str) -> Result<bool, String> {
                self.scripts.borrow_mut().insert(script.into());
                Ok(true)
            }
        }
        let requests = Requests {
            resolver,
            scripts: Default::default(),
        };
        let candidate = Self::prepare(snapshot, &requests, uci)?;
        for script in requests.scripts.into_inner() {
            candidate.config.attest()?;
            ensure(&script)?;
            candidate.config.attest()?;
        }
        candidate.attest_plan(resolver)?;
        Ok(candidate)
    }

    fn attest_plan(&self, resolver: &impl InterfaceResolver) -> Result<(), String> {
        self.config.attest()?;
        Self::attest_materialized_plan(&self.config, resolver, &self.projection, &self.plan)?;
        self.config.attest()
    }

    fn attest_materialized_plan(
        config: &PreparedConfig,
        resolver: &impl InterfaceResolver,
        expected_projection: &SqmProjectionPlan,
        expected_plan: &ServiceStartPlan,
    ) -> Result<(), String> {
        config.attest_private()?;
        let cake = config.package(CAKE_PACKAGE)?;
        let mut sqm = config.package(SQM_PACKAGE)?.clone();
        let projection = plan_projection(cake, &mut sqm, resolver, &ProjectionScope::All)?;
        if !prepare_preset_edits(cake, resolver)?.1.is_empty()
            || !projection.edits().is_empty()
            || projection.is_managed() != expected_projection.is_managed()
            || projection.ingress_interfaces() != expected_projection.ingress_interfaces()
            || projection.conflicts() != expected_projection.conflicts()
            || &plan_start(cake, &sqm, resolver, &projection)? != expected_plan
        {
            return Err("service-start-candidate-plan-changed".into());
        }
        config.attest_private()
    }

    fn prepare(
        snapshot: CommittedSnapshot,
        resolver: &impl InterfaceResolver,
        uci: &Path,
    ) -> Result<Self, String> {
        snapshot.attest()?;
        let (cake, cake_edits) = prepare_preset_edits(snapshot.package(CAKE_PACKAGE)?, resolver)?;
        let mut sqm = snapshot.package(SQM_PACKAGE)?.clone();
        let projection = plan_projection(&cake, &mut sqm, resolver, &ProjectionScope::All)?;
        if !projection.conflicts().is_empty() {
            return Err("service-start-conflicting-managed-targets".into());
        }
        let expected_plan = plan_start(&cake, &sqm, resolver, &projection)?;
        let sqm_edits = projection.edits();
        let config = snapshot.prepare([&cake_edits, &sqm_edits], uci)?;
        config.attest()?;
        let cake = config.package(CAKE_PACKAGE)?;
        let mut sqm = config.package(SQM_PACKAGE)?.clone();
        if !prepare_preset_edits(cake, resolver)?.1.is_empty() {
            return Err("service-start-preset-candidate-not-idempotent".into());
        }
        let verified_projection = plan_projection(cake, &mut sqm, resolver, &ProjectionScope::All)?;
        if !verified_projection.edits().is_empty()
            || verified_projection.is_managed() != projection.is_managed()
            || verified_projection.ingress_interfaces() != projection.ingress_interfaces()
            || verified_projection.conflicts() != projection.conflicts()
        {
            return Err("service-start-sqm-candidate-not-idempotent".into());
        }
        let whole_show = config.package_show(CAKE_PACKAGE)?;
        let history = crate::parse_global_history_config(
            std::str::from_utf8(&whole_show).map_err(|_| "service-start-config-not-text")?,
        )
        .map_err(|_| "service-start-invalid-global-history-config")?;
        for (name, section) in &cake.sections {
            if section.section_type == "cake_autorate" {
                let show = config.section_show(CAKE_PACKAGE, name)?;
                let text =
                    std::str::from_utf8(&show).map_err(|_| "service-start-config-not-text")?;
                crate::Config::from_uci_text(name, text)
                    .and_then(|mut config| {
                        config.graph_history_ram_budget_kib = history.0;
                        config.graph_history_instance_count = history.1;
                        config.validate()
                    })
                    .map_err(|_| format!("service-start-invalid-controller-config:{name}"))?;
            }
        }
        let plan = plan_start(cake, &sqm, resolver, &verified_projection)?;
        if plan != expected_plan {
            return Err("service-start-candidate-plan-changed".into());
        }
        #[cfg(feature = "calibration")]
        super::mqtt_publisher::service_configs(cake)?;
        config.attest()?;
        Ok(Self {
            config,
            projection: verified_projection,
            plan,
        })
    }
}

/// A durable complete desired set precedes every runtime mutation. Dropping
/// this value deliberately preserves recovery evidence; readiness accepts it.
struct StartPublication {
    published: super::uci_transaction::PublishedConfig,
    projection: SqmProjectionPlan,
    plan: ServiceStartPlan,
    batch: super::controller_input::Batch,
}
impl StartPublication {
    fn publish(paths: &ServicePaths, candidate: StartCandidate) -> Result<Self, String> {
        let published = super::uci_transaction::publish(candidate.config)?;
        let root = paths.runtime_root.join(".controller-input");
        let batch = (|| {
            let mut inputs = BTreeMap::new();
            for instance in &candidate.plan.instances {
                let input = super::controller_input::store_for_selected_update(
                    &published,
                    instance,
                    &root,
                    &paths.config_root,
                )?;
                inputs.insert(instance.clone(), input);
            }
            super::controller_input::store_batch(
                &published,
                &candidate.plan.instances,
                &inputs,
                &root,
                &paths.config_root,
            )
        })();
        let batch = match batch {
            Ok(batch) => batch,
            Err(error) => {
                // No runtime action has been attempted. A failed rollback
                // retains its journal; never recapture/adopt a foreign commit.
                return match published.rollback() {
                    Ok(()) => Err(error),
                    Err(rollback) => Err(format!(
                        "{error}; start publication recovery required: {rollback}"
                    )),
                };
            }
        };
        Ok(Self {
            published,
            projection: candidate.projection,
            plan: candidate.plan,
            batch,
        })
    }

    fn attest(&self, resolver: &impl InterfaceResolver) -> Result<(), String> {
        self.published.attest()?;
        self.batch.attest()?;
        StartCandidate::attest_materialized_plan(
            self.published.config(),
            resolver,
            &self.projection,
            &self.plan,
        )?;
        self.published.attest()
    }
}

/// Publication/replay owns no runtime effects. Even an error after file
/// publication leaves durable recovery evidence rather than adopting a new
/// baseline or attempting an unproved global Stop.
struct ReloadPublication {
    start: StartPublication,
    previous: super::controller_input::Batch,
    delta: ServiceReloadDelta,
}

/// Hold one set across controller, SQM, sidecar and registration phases. This
/// includes unchanged queues whose controllers themselves need replacement.
struct RetainedSqmSet {
    inputs: BTreeMap<String, super::controller_input::Loaded>,
    queues: Vec<(String, super::sqm_recovery_openwrt::PinnedSqm)>,
}
impl RetainedSqmSet {
    fn capture(
        batch: &super::controller_input::Batch,
        changing: &[ManagedSqmStopSpec],
        resolver: &impl InterfaceResolver,
        lease: &ServiceGlobalLease<'_>,
    ) -> Result<Self, String> {
        batch.attest_record()?;
        let inputs = batch.load_inputs()?;
        let old = stop_plan_from_views(
            &UciPackage::default(),
            &UciPackage::default(),
            &inputs,
            resolver,
        )?;
        let mut queues = Vec::new();
        for spec in old.managed {
            if changing.iter().any(|changed| {
                changed.instance == spec.instance && changed.sqm_section == spec.sqm_section
            }) {
                continue;
            }
            let input = inputs
                .get(&spec.instance)
                .ok_or("reload-retained-input-missing")?;
            let queue = super::sqm_recovery_openwrt::PinnedSqm::capture_under_lifecycle(
                &spec, input, lease,
            )
            .map_err(|e| sqm_error_message(&e).to_string())?;
            queues.push((spec.instance, queue));
        }
        let retained = Self { inputs, queues };
        retained.attest_under_lifecycle(lease)?;
        batch.attest_record()?;
        Ok(retained)
    }
    fn attest_under_lifecycle(&self, lease: &ServiceGlobalLease<'_>) -> Result<(), String> {
        for (name, queue) in &self.queues {
            let input = self
                .inputs
                .get(name)
                .ok_or("reload-retained-input-missing")?;
            queue
                .attest_under_lifecycle(input, lease)
                .map_err(|e| sqm_error_message(&e).to_string())?;
        }
        Ok(())
    }
    fn attest(&self) -> Result<(), String> {
        for (name, queue) in &self.queues {
            let input = self
                .inputs
                .get(name)
                .ok_or("reload-retained-input-missing")?;
            queue
                .attest(input)
                .map_err(|e| sqm_error_message(&e).to_string())?;
        }
        Ok(())
    }
}

impl ReloadPublication {
    fn register_controllers(
        &self,
        paths: &ServicePaths,
        resolver: &impl InterfaceResolver,
        mut attest_preserved_runtime: impl FnMut() -> Result<(), String>,
    ) -> Result<reload_runtime::RegistrationWitness, String> {
        self.attest(resolver)?;
        let controllers = reload_runtime::ReloadControllers::capture(
            paths,
            self.previous.generations(),
            self.start.batch.generations(),
            &self.delta.retained,
        )?;
        controllers.register_missing(paths, self.start.batch.generations(), || {
            self.attest(resolver)?;
            attest_preserved_runtime()
        })
    }

    #[cfg(feature = "calibration")]
    fn reconcile_mqtt(
        &self,
        paths: &ServicePaths,
        resolver: &impl InterfaceResolver,
        plan_root: &Path,
        mut attest_preserved_runtime: impl FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        self.attest(resolver)?;
        let package = self.start.published.config().package(CAKE_PACKAGE)?;
        let desired: BTreeSet<_> = super::mqtt_publisher::service_configs(package)?
            .into_iter()
            .map(|config| config.instance)
            .collect();
        // The selected parser includes dormant registrations and rejects any
        // command/name collision before a plan or process can be removed.
        let registered = super::procd_control::mqtt_definitions(&paths.ubus)?
            .into_keys()
            .collect();
        let job = self.start.batch.reload_operation_id()?;
        let order = super::mqtt_publisher::selected_plan::reload_order(
            plan_root,
            &desired,
            &registered,
            job,
        )?;
        for instance in order {
            self.attest(resolver)?;
            attest_preserved_runtime()?;
            let mut selected = selected_mqtt::SelectedMqtt::prepare(
                &paths.proc_root,
                &paths.ubus,
                plan_root,
                &instance,
                package,
                desired.contains(&instance),
                job,
            )?;
            selected.stop_changed(|| {
                self.attest(resolver)?;
                attest_preserved_runtime()
            })?;
            selected.finish(
                || {
                    self.attest(resolver)?;
                    attest_preserved_runtime()
                },
                |name, endpoint| {
                    super::procd_control::add_instance(
                        &paths.ubus,
                        super::procd_control::Registration::Mqtt {
                            instance: name,
                            endpoint,
                        },
                    )
                },
            )?;
        }
        self.attest(resolver)?;
        let actual: BTreeSet<_> = super::mqtt_publisher::attest_service_plans(package, plan_root)?
            .into_iter()
            .collect();
        if actual != desired {
            return Err("reload MQTT desired membership is not ready".into());
        }
        attest_preserved_runtime()
    }

    #[cfg(feature = "calibration")]
    fn reconcile_classifier(
        &self,
        resolver: &impl InterfaceResolver,
        mut attest_preserved_runtime: impl FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        self.attest(resolver)?;
        let inputs = self.previous.load_inputs()?;
        let old = stop_plan_from_views(
            &UciPackage::default(),
            &UciPackage::default(),
            &inputs,
            resolver,
        )?;
        let mut targets: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for spec in old.managed {
            targets
                .entry(spec.instance)
                .or_default()
                .insert(spec.target_interface);
        }
        super::traffic_classifier::reconcile_reload(
            self.start.published.config().package(CAKE_PACKAGE)?,
            &targets,
            self.start.batch.reload_operation_id()?,
            || {
                self.attest(resolver)?;
                attest_preserved_runtime()
            },
        )
    }

    fn pin_retained_queues(
        &self,
        resolver: &impl InterfaceResolver,
        lease: &ServiceGlobalLease<'_>,
    ) -> Result<RetainedSqmSet, String> {
        self.attest(resolver)?;
        let retained =
            RetainedSqmSet::capture(&self.previous, &self.delta.stop_sqm, resolver, lease)?;
        self.attest(resolver)?;
        Ok(retained)
    }
    /// Run under the ordinary lifecycle lock with Native Apply/sidecar owners
    /// excluded. The callback proves the entire old controller/SQM/sidecar
    /// runtime unchanged; unlike ordinary Stop it must not stop those peers.
    fn recover_preparation(
        paths: &ServicePaths,
        mut attest_original_runtime: impl FnMut(&BTreeMap<String, String>) -> Result<(), String>,
    ) -> Result<Option<super::uci_transaction::SourceReceipt>, String> {
        let Some(mut recovery) = super::controller_input::UnpublishedReload::load(
            &paths.runtime_root.join(".controller-input"),
            &paths.config_root,
        )?
        else {
            return Ok(None);
        };
        let expected = recovery.expected_source.clone();
        let original = super::uci_transaction::recover_checked(
            &paths.config_root,
            expected.as_ref(),
            |event| {
                recovery.attest()?;
                attest_original_runtime(recovery.previous.generations())?;
                if let super::uci_transaction::RecoveryCheck::Restored(source) = event {
                    source.attest(&paths.config_root)?;
                    recovery.retire_draft(|old| {
                        source.attest(&paths.config_root)?;
                        attest_original_runtime(old)
                    })?;
                    source.attest(&paths.config_root)?;
                }
                Ok(())
            },
        )?;
        // A no-op publication has no UCI journal. A complete draft must still
        // match its exact source; an incomplete draft grants no source handoff.
        recovery.retire_draft(|old| {
            if let Some(source) = original.as_ref().or(expected.as_ref()) {
                source.attest(&paths.config_root)?;
            }
            attest_original_runtime(old)
        })?;
        Ok(original)
    }

    fn publish(
        paths: &ServicePaths,
        candidate: StartCandidate,
        previous: super::controller_input::Batch,
        resolver: &impl InterfaceResolver,
        mut attest_previous_runtime: impl FnMut(&BTreeMap<String, String>) -> Result<(), String>,
    ) -> Result<Self, String> {
        let delta = ServiceReloadDelta::prepare(&candidate, &previous, resolver)?;
        attest_previous_runtime(previous.generations())?;
        candidate.attest_plan(resolver)?;
        let published = super::uci_transaction::publish(candidate.config)?;
        let root = paths.runtime_root.join(".controller-input");
        let mut replacements = BTreeMap::new();
        for instance in &delta.start_controllers {
            let input = super::controller_input::store_for_selected_update(
                &published,
                instance,
                &root,
                &paths.config_root,
            )?;
            replacements.insert(instance.clone(), input);
        }
        let batch = super::controller_input::replace_reload_batch(
            &published,
            &previous,
            &candidate.plan.instances,
            &delta.retained,
            &replacements,
            &mut attest_previous_runtime,
        )?;
        let reload = Self {
            start: StartPublication {
                published,
                projection: candidate.projection,
                plan: candidate.plan,
                batch,
            },
            previous,
            delta,
        };
        reload.attest(resolver)?;
        Ok(reload)
    }

    fn resume(
        candidate: StartCandidate,
        batch: super::controller_input::Batch,
        resolver: &impl InterfaceResolver,
    ) -> Result<Self, String> {
        if !batch.is_reload_update() {
            return Err("service-reload-not-pending".into());
        }
        batch.attest()?;
        let previous = batch.applied_predecessor()?;
        let delta = ServiceReloadDelta::compare(&candidate, &previous, resolver)?;
        let source = batch.attest_reload_plan(&candidate.plan.instances, &delta.retained)?;
        let published = super::uci_transaction::resume_published(candidate.config, &source)?;
        let reload = Self {
            start: StartPublication {
                published,
                projection: candidate.projection,
                plan: candidate.plan,
                batch,
            },
            previous,
            delta,
        };
        reload.attest(resolver)?;
        Ok(reload)
    }

    fn attest(&self, resolver: &impl InterfaceResolver) -> Result<(), String> {
        self.previous.attest_record()?;
        self.start.attest(resolver)?;
        self.start
            .batch
            .attest_reload_plan(&self.start.plan.instances, &self.delta.retained)?;
        self.previous.attest_record()
    }

    fn stop_controllers(
        &self,
        paths: &ServicePaths,
        resolver: &impl InterfaceResolver,
        mut attest_topology_and_sidecars: impl FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        self.attest(resolver)?;
        attest_topology_and_sidecars()?;
        let mut controllers = reload_runtime::ReloadControllers::capture(
            paths,
            self.previous.generations(),
            self.start.batch.generations(),
            &self.delta.retained,
        )?;
        controllers.stop_obsolete(paths, || {
            self.attest(resolver)?;
            attest_topology_and_sidecars()
        })
    }

    fn reconcile_sqm(
        &self,
        paths: &ServicePaths,
        resolver: &impl InterfaceResolver,
        profiles: Vec<super::sqm_runner::Profile>,
        mut attest_preserved_runtime: impl FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        self.attest(resolver)?;
        if profiles.len() != self.delta.start_sqm.len() {
            return Err("service-reload-runner-membership-mismatch".into());
        }
        let controllers = reload_runtime::ReloadControllers::capture(
            paths,
            self.previous.generations(),
            self.start.batch.generations(),
            &self.delta.retained,
        )?;
        controllers.attest_stopped(paths)?;
        let live = controllers.live_desired(self.start.batch.generations());
        let profiles = self
            .delta
            .start_sqm
            .iter()
            .zip(profiles)
            .map(|(spec, profile)| (spec.target_interface.clone(), profile))
            .collect();
        struct Backend<'a, R, P> {
            reload: &'a ReloadPublication,
            paths: &'a ServicePaths,
            resolver: &'a R,
            controllers: reload_runtime::ReloadControllers,
            live: BTreeSet<String>,
            inputs: BTreeMap<String, super::controller_input::Loaded>,
            profiles: BTreeMap<String, super::sqm_runner::Profile>,
            preserved: &'a mut P,
        }
        impl<R: InterfaceResolver, P: FnMut() -> Result<(), String>>
            reload_runtime::SqmReloadBackend for Backend<'_, R, P>
        {
            fn attest(&mut self) -> Result<(), String> {
                self.reload.attest(self.resolver)?;
                self.controllers.attest_stopped(self.paths)?;
                (self.preserved)()
            }
            fn desired_ready(&mut self, spec: &ManagedSqmAttestationSpec) -> Result<bool, String> {
                let mut observed = spec.clone();
                if self.live.contains(&spec.instance) {
                    let cake = self.reload.start.published.config().package(CAKE_PACKAGE)?;
                    let section = cake
                        .sections
                        .get(&spec.instance)
                        .ok_or("reload-controller-config-missing")?;
                    let policy = managed_sqm_rate_policy_from_options(&section.options)
                        .map_err(|e| sqm_error_message(&e).to_string())?;
                    observed.minimum_download_kbps = policy.minimum_download_kbps;
                    observed.maximum_download_kbps = policy.maximum_download_kbps;
                    observed.minimum_upload_kbps = policy.minimum_upload_kbps;
                    observed.maximum_upload_kbps = policy.maximum_upload_kbps;
                }
                match super::sqm_recovery_openwrt::attest_managed_sqm_from_published(
                    &observed,
                    &self.reload.start.published,
                ) {
                    Ok(_) => Ok(true), // Includes a proven offline/deferred target.
                    Err(NativeSqmAttestationError::Failed(_)) => Ok(false),
                    Err(error) => Err(sqm_error_message(&error).to_string()),
                }
            }
            fn stop_old(&mut self, spec: &ManagedSqmStopSpec) -> Result<(), String> {
                if self.reload.start.plan.managed.iter().any(|new| {
                    self.live.contains(&new.instance)
                        && new.target_interface == spec.target_interface
                }) {
                    return Err("reload-sqm-running-desired-controller-needs-recovery".into());
                }
                let input = self
                    .inputs
                    .get(&spec.instance)
                    .ok_or("reload-old-sqm-input-missing")?;
                super::sqm_recovery_openwrt::stop_managed_sqm_from_input(spec, input)
                    .map_err(|e| sqm_error_message(&e).to_string())
            }
            fn start_new(&mut self, spec: &ManagedSqmAttestationSpec) -> Result<(), String> {
                if self.live.contains(&spec.instance) {
                    return Err("reload-sqm-running-desired-controller-needs-recovery".into());
                }
                let profile = self
                    .profiles
                    .remove(&spec.target_interface)
                    .ok_or("reload-sqm-profile-missing")?;
                super::sqm_recovery_openwrt::start_managed_sqm_from_published(
                    spec,
                    &self.reload.start.published,
                    profile,
                    &self.paths.runtime_root,
                    || false,
                )
                .map(|_| ())
                .map_err(|e| sqm_error_message(&e).to_string())
            }
        }
        let mut backend = Backend {
            reload: self,
            paths,
            resolver,
            controllers,
            live,
            inputs: self.previous.load_inputs()?,
            profiles,
            preserved: &mut attest_preserved_runtime,
        };
        reload_runtime::reconcile_sqm(&self.delta.stop_sqm, &self.delta.start_sqm, &mut backend)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ControllerStartReadiness {
    Ready,
    Waiting(String),
}

#[derive(Clone, Debug)]
struct ControllerStartAuthority {
    source: super::uci_transaction::SourceReceipt,
    instances: Vec<String>,
    generations: Option<BTreeMap<String, String>>,
}
impl ControllerStartAuthority {
    fn attest(&self, paths: &ServicePaths) -> Result<(), String> {
        self.source.attest(&paths.config_root)?;
        let batch = super::controller_input::load_batch(
            &paths.runtime_root.join(".controller-input"),
            &paths.config_root,
        )?;
        if batch.as_ref().map(|b| b.generations()) != self.generations.as_ref() {
            return Err("controller service generation set changed during readiness".into());
        }
        if let Some(batch) = batch {
            batch.attest()?;
        }
        self.source.attest(&paths.config_root)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ServiceStopPlan {
    cake: UciPackage,
    sqm: UciPackage,
    managed: Vec<ManagedSqmStopSpec>,
}

#[derive(Clone, Debug)]
struct ServicePaths {
    config_root: PathBuf,
    uci: PathBuf,
    tc: PathBuf,
    ubus: PathBuf,
    proc_root: PathBuf,
    runtime_root: PathBuf,
    runtime_lock_root: PathBuf,
    bridger_init: PathBuf,
    bridger_config: PathBuf,
    uci_workspace_root: PathBuf,
}

/// Explicit runtime context below the CLI's root gate. Test contexts use owned
/// temporary roots; production keeps the existing installed paths and checks.
struct LifecycleContext {
    paths: ServicePaths,
    environment: OpenWrtEnvironment,
    #[cfg(feature = "calibration")]
    mqtt_root: PathBuf,
    #[cfg(feature = "calibration")]
    native_recovery_root: PathBuf,
}
impl LifecycleContext {
    fn production() -> Self {
        Self {
            paths: ServicePaths::production(),
            environment: OpenWrtEnvironment::production(),
            #[cfg(feature = "calibration")]
            mqtt_root: super::mqtt_publisher::PRODUCTION_PLAN_ROOT.into(),
            #[cfg(feature = "calibration")]
            native_recovery_root: super::autotune_apply_openwrt::default_native_apply_paths()
                .recovery_root
                .into(),
        }
    }
    fn require_no_native_owner(&self) -> Result<(), String> {
        #[cfg(feature = "calibration")]
        {
            let (existing, bootstrap) =
                super::coordinator::native_apply_recovery_markers_at(&self.native_recovery_root)?;
            require_no_native_apply_recovery(existing, bootstrap)?;
        }
        Ok(())
    }
}

impl ServicePaths {
    fn production() -> Self {
        Self {
            config_root: env_path("CAKE_AUTORATE_CONFIG_DIR", "/etc/config"),
            uci: env_path("CAKE_AUTORATE_UCI_BIN", "/sbin/uci"),
            tc: env_path("CAKE_AUTORATE_TC_BIN", "/sbin/tc"),
            ubus: env_path("CAKE_AUTORATE_UBUS_BIN", "/bin/ubus"),
            proc_root: env_path("CAKE_AUTORATE_PROC_ROOT", "/proc"),
            runtime_root: env_path("CAKE_AUTORATE_RUN_ROOT", "/var/run/cake-autorate"),
            runtime_lock_root: env_path(
                "CAKE_AUTORATE_RUNTIME_LOCK_ROOT",
                "/tmp/cake-autorate-speedtest",
            ),
            bridger_init: env_path("CAKE_AUTORATE_BRIDGER_INIT", "/etc/init.d/bridger"),
            bridger_config: env_path("CAKE_AUTORATE_BRIDGER_CONFIG", "/etc/config/bridger"),
            uci_workspace_root: env_path(
                "CAKE_AUTORATE_SERVICE_UCI_WORK_ROOT",
                "/tmp/cake-autorate-service-uci",
            ),
        }
    }
}

enum ServiceGlobalLock {
    Borrowed { _guard: File },
    Owned(File),
}

pub(crate) struct ServiceGlobalLease<'a> {
    guard: &'a File,
    root: &'a Path,
}
impl ServiceGlobalLock {
    fn lease<'a>(&'a self, paths: &'a ServicePaths) -> Result<ServiceGlobalLease<'a>, String> {
        let guard = match self {
            Self::Owned(guard) => guard,
            Self::Borrowed { _guard } => _guard,
        };
        let lease = ServiceGlobalLease {
            guard,
            root: &paths.runtime_lock_root,
        };
        lease.attest_root(&paths.runtime_lock_root)?;
        Ok(lease)
    }
}
impl ServiceGlobalLease<'_> {
    pub(crate) fn attest_root(&self, root: &Path) -> Result<(), String> {
        if root != self.root {
            return Err("service-lifecycle-lease-root-mismatch".into());
        }
        attest_borrowed_exclusive_lock(&root.join("runtime.guard"), self.guard)
    }
}

#[cfg(test)]
pub(crate) fn with_test_global_lease<T>(
    root: &Path,
    action: impl FnOnce(&ServiceGlobalLease<'_>) -> T,
) -> T {
    let paths = ServicePaths {
        runtime_lock_root: root.into(),
        ..ServicePaths::production()
    };
    let lock = acquire_service_global_lock(&paths).unwrap();
    let lease = lock.lease(&paths).unwrap();
    action(&lease)
}

impl Drop for ServiceGlobalLock {
    fn drop(&mut self) {
        if let Self::Owned(file) = self {
            unsafe {
                libc::flock(file.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ServiceProcessKind {
    Controller,
    #[cfg(feature = "calibration")]
    MqttPublisher,
}

impl ServiceProcessKind {
    fn argument(self) -> &'static str {
        match self {
            Self::Controller => "--instance",
            #[cfg(feature = "calibration")]
            Self::MqttPublisher => "--mqtt-publisher",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Controller => "controller",
            #[cfg(feature = "calibration")]
            Self::MqttPublisher => "MQTT publisher",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ControllerIdentity {
    kind: ServiceProcessKind,
    instance: String,
    pid: u32,
    process_group: u32,
    starttime_ticks: u64,
}

struct ControllerPidfd {
    identity: ControllerIdentity,
    pidfd: OwnedFd,
}

trait ServiceStopBackend {
    type Controllers;

    fn snapshot(&mut self) -> Result<ServiceStopPlan, String>;
    fn attest_unchanged(&mut self, plan: &ServiceStopPlan) -> Result<(), String>;
    fn capture_controllers(&mut self) -> Result<Self::Controllers, String>;
    fn disable_service(&mut self) -> Result<(), String>;
    fn wait_controllers(&mut self, controllers: Self::Controllers) -> Result<(), String>;
    fn attest_no_controllers(&mut self) -> Result<(), String>;
    fn clear_classifier(&mut self);
    fn stop_sqm(&mut self, spec: &ManagedSqmStopSpec) -> Result<(), String>;
    fn cleanup_runtime(&mut self) -> Result<(), String>;
    fn cleanup_sidecars(&mut self) -> Result<(), String>;
    fn retire_generations(&mut self, _plan: &ServiceStopPlan) -> Result<(), String> {
        Ok(())
    }
}

struct OpenWrtServiceStop {
    environment: OpenWrtEnvironment,
    paths: ServicePaths,
    expected_source: Option<String>,
    restored_source: Option<super::uci_transaction::SourceReceipt>,
    committed: Option<CommittedSnapshot>,
    applied: BTreeMap<String, super::controller_input::Loaded>,
    batch: Option<super::controller_input::Batch>,
}

fn execute_stop(backend: &mut impl ServiceStopBackend) -> Result<(), String> {
    let plan = backend.snapshot()?;
    backend.attest_unchanged(&plan)?;
    let controllers = backend.capture_controllers()?;
    backend.attest_unchanged(&plan)?;
    backend.disable_service()?;
    backend.wait_controllers(controllers)?;
    backend.attest_no_controllers()?;
    backend.attest_unchanged(&plan)?;
    backend.clear_classifier();
    backend.attest_unchanged(&plan)?;
    for spec in &plan.managed {
        backend.stop_sqm(spec)?;
        backend.attest_unchanged(&plan)?;
    }
    backend.attest_no_controllers()?;
    backend.cleanup_runtime()?;
    backend.attest_no_controllers()?;
    backend.cleanup_sidecars()?;
    backend.attest_no_controllers()?;
    backend.attest_unchanged(&plan)?;
    backend.retire_generations(&plan)
}

#[derive(Debug, PartialEq, Eq)]
struct ExactFileSnapshot {
    bytes: Vec<u8>,
    device: u64,
    inode: u64,
    mode: u32,
    uid: u32,
    gid: u32,
}

struct BridgerWorkspace {
    root: PathBuf,
    config_dir: PathBuf,
    override_dir: PathBuf,
    savedir: PathBuf,
    alias: String,
}

impl BridgerWorkspace {
    fn create(base: &Path) -> Result<Self, String> {
        ensure_owner_directory(base)?;
        for slot in 0..16_u8 {
            let root = base.join(format!("{}.{}", std::process::id(), slot));
            match fs::create_dir(&root) {
                Ok(()) => {
                    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
                        .map_err(|error| format!("unable to secure bridger workspace: {error}"))?;
                    let config_dir = root.join("config");
                    let override_dir = root.join("override");
                    let savedir = root.join("savedir");
                    for directory in [&config_dir, &override_dir, &savedir] {
                        fs::create_dir(directory).map_err(|error| {
                            format!("unable to create bridger workspace: {error}")
                        })?;
                        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(
                            |error| format!("unable to secure bridger workspace: {error}"),
                        )?;
                    }
                    return Ok(Self {
                        root,
                        config_dir,
                        override_dir,
                        savedir,
                        alias: format!("cake_bridger_{}_{}", std::process::id(), slot),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!("unable to create bridger workspace: {error}"));
                }
            }
        }
        Err("unable to allocate an isolated bridger workspace".to_string())
    }

    fn package_path(&self) -> PathBuf {
        self.config_dir.join(&self.alias)
    }

    fn arguments(&self, arguments: impl IntoIterator<Item = OsString>) -> Vec<OsString> {
        let mut result = vec![
            OsString::from("-c"),
            self.config_dir.as_os_str().to_os_string(),
            OsString::from("-C"),
            self.override_dir.as_os_str().to_os_string(),
            OsString::from("-t"),
            self.savedir.as_os_str().to_os_string(),
            OsString::from("-q"),
        ];
        result.extend(arguments);
        result
    }
}

impl Drop for BridgerWorkspace {
    fn drop(&mut self) {
        if owner_directory_is_exact(&self.root) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

pub(crate) fn run_service_lifecycle<I>(mut arguments: I) -> Result<String, String>
where
    I: Iterator<Item = String>,
{
    let command = arguments
        .next()
        .ok_or_else(|| "service-lifecycle requires a command".to_string())?;
    if arguments.next().is_some() {
        return Err("service-lifecycle received unexpected arguments".to_string());
    }
    match command.as_str() {
        "preflight-start" => preflight_start(false),
        "preflight-reload" => preflight_start(true),
        "prepare-start" => prepare_start(),
        "confirm-started" => confirm_started_openwrt(),
        "execute-stop" => execute_stop_openwrt(),
        "reload" => execute_reload_openwrt(),
        _ => Err("service-lifecycle command is unsupported".to_string()),
    }
}

/// Fresh Native Apply admission only. The caller already owns the global
/// lifecycle lock. Do not call this from owner roll-forward/rollback: those
/// paths must be able to settle their own pending selected-generation state.
#[cfg(feature = "calibration")]
pub(crate) fn require_no_pending_service_start() -> Result<(), String> {
    let paths = ServicePaths::production();
    super::traffic_classifier::require_no_selected_recovery()?;
    super::mqtt_publisher::require_no_selected_recovery(Path::new(
        super::mqtt_publisher::PRODUCTION_PLAN_ROOT,
    ))?;
    require_no_pending_start_in(&paths.config_root, &paths.runtime_root)
}

#[cfg(feature = "calibration")]
pub(crate) fn require_bootstrap_generation_slot_absent(instance: &str) -> Result<(), String> {
    require_no_pending_service_start()?;
    let paths = ServicePaths::production();
    require_bootstrap_slot_absent_in(&paths, instance)
}

#[cfg(feature = "calibration")]
fn require_bootstrap_slot_absent_in(paths: &ServicePaths, instance: &str) -> Result<(), String> {
    if !safe_name(instance) {
        return Err("bootstrap-generation-instance-invalid".into());
    }
    if let Some(batch) = super::controller_input::load_batch(
        &paths.runtime_root.join(".controller-input"),
        &paths.config_root,
    )? {
        if batch.generations().contains_key(instance) {
            return Err("bootstrap-controller-slot-already-owned-by-applied-generation".into());
        }
    } else if !super::procd_control::generation_references(&paths.ubus)?.is_empty() {
        return Err("bootstrap-generation-registry-missing".into());
    }
    Ok(())
}

/// Keep request fingerprints as change detectors, but do not let them grant
/// runtime authority for an SQM recipe the active controller has not applied.
#[cfg(feature = "calibration")]
pub(crate) fn attest_operation_applied_sqm(
    instance: &str,
    cfg: &crate::Config,
    fingerprint: &str,
) -> Result<(), String> {
    attest_operation_applied_sqm_in(&ServicePaths::production(), instance, cfg, fingerprint)
}

#[cfg(feature = "calibration")]
fn attest_operation_applied_sqm_in(
    paths: &ServicePaths,
    instance: &str,
    cfg: &crate::Config,
    fingerprint: &str,
) -> Result<(), String> {
    let root = paths.runtime_root.join(".controller-input");
    let Some(batch) = super::controller_input::load_batch(&root, &paths.config_root)? else {
        // Pre-generation installations retain their existing runtime checks.
        // Missing metadata must not downgrade a generation-bound service.
        if !super::procd_control::generation_references(&paths.ubus)?.is_empty() {
            return Err("operation-controller-batch-missing".into());
        }
        return Ok(());
    };
    if batch.pending()
        || batch.pending_update()?.is_some()
        || super::controller_input::restore_pending(&root)?
    {
        return Err("operation-controller-start-or-recovery-pending".into());
    }
    let id = batch
        .generations()
        .get(instance)
        .ok_or("operation-controller-not-in-applied-batch")?;
    let input = super::controller_input::load(instance, id, &root, &paths.config_root)?;
    input.guard.attest_applied()?;
    let applied = &input.config;
    if cfg.enabled != applied.enabled
        || cfg.manage_sqm != applied.manage_sqm
        || cfg.sqm_enabled != applied.sqm_enabled
        || cfg.sqm_section != applied.sqm_section
        || cfg.sqm_interface != applied.sqm_interface
        || cfg.dl_if != applied.dl_if
        || cfg.ul_if != applied.ul_if
        || cfg.download_shaping_enabled() != applied.download_shaping_enabled()
        || cfg.upload_shaping_enabled() != applied.upload_shaping_enabled()
        || cfg.route_mode != applied.route_mode
        || cfg.mwan3_member != applied.mwan3_member
        || cfg.explicit_route_authority != applied.explicit_route_authority
        || cfg.explicit_dns_server != applied.explicit_dns_server
        || super::sqm_identity::managed_sqm_identity_from_input(
            &input,
            instance,
            &cfg.sqm_section,
            &cfg.sqm_interface,
        )? != fingerprint
    {
        return Err("operation-SQM-or-route-settings-not-applied; apply the selected instance before testing".into());
    }
    let expected = BTreeMap::from([(instance.to_string(), id.clone())]);
    let procd = super::procd_control::controller_generations(&paths.ubus, &expected)?
        .ok_or("operation-controller-generation-not-running")?;
    let controllers = discover_controllers(&paths.proc_root)?;
    let controller = controllers
        .iter()
        .find(|controller| {
            controller.kind == ServiceProcessKind::Controller && controller.instance == instance
        })
        .ok_or("operation-controller-process-not-running")?;
    if procd.get(instance) != Some(&controller.pid)
        || controller_generation(&paths.proc_root, controller)?.as_ref() != Some(id)
    {
        return Err("operation-controller-generation-changed".into());
    }
    input.guard.attest_applied()?;
    batch.attest_record()?;
    if !controller_identity_matches(&paths.proc_root, controller)?
        || super::procd_control::controller_generations(&paths.ubus, &expected)? != Some(procd)
    {
        return Err("operation-controller-generation-changed".into());
    }
    input.guard.attest_applied()?;
    batch.attest_record()?;
    if batch.pending_update()?.is_some() || super::controller_input::restore_pending(&root)? {
        return Err("operation-controller-start-or-recovery-pending".into());
    }
    Ok(())
}

#[cfg(feature = "calibration")]
pub(super) fn require_no_pending_start_in(
    config_root: &Path,
    runtime_root: &Path,
) -> Result<(), String> {
    if super::uci_transaction::recovery_pending(config_root)? {
        return Err("native-apply-service-publication-recovery-pending".into());
    }
    let inputs = runtime_root.join(".controller-input");
    if super::controller_input::restore_pending(&inputs)? {
        return Err("native-apply-controller-generation-recovery-pending".into());
    }
    if let Some(batch) = super::controller_input::load_batch(&inputs, config_root)? {
        if batch.pending() || batch.pending_update()?.is_some() {
            return Err("native-apply-controller-generation-recovery-pending".into());
        }
        // Applied inputs intentionally retain their old source receipt while
        // public configuration is edited. Do not rebase or require byte parity.
        batch.attest_record()?;
    }
    Ok(())
}

fn confirm_started_openwrt() -> Result<String, String> {
    confirm_controller_service_started()?;
    Ok("service-started-v1 ready\n".to_string())
}

pub(crate) fn confirm_controller_service_started() -> Result<(), String> {
    require_service_root()?;
    confirm_controller_service_started_in(&LifecycleContext::production(), |_| Ok(()), true)
}

/// A reload retains old input witnesses until confirmation returns, so its
/// caller defers GC and checks them with the active lock lease at acceptance.
fn confirm_controller_service_started_in(
    context: &LifecycleContext,
    mut preserved: impl FnMut(Option<&ServiceGlobalLease<'_>>) -> Result<(), String>,
    collect_inputs: bool,
) -> Result<(), String> {
    let paths = context.paths.clone();
    let environment = context.environment.clone();
    preserved(None)?;
    let authority = controller_start_authority(&environment, &paths)?;
    let deadline = Instant::now()
        .checked_add(CONTROLLER_START_TIMEOUT)
        .ok_or_else(|| "controller readiness deadline overflowed".to_string())?;
    let mut events = CalibrationEventLoop::new()?;

    loop {
        preserved(None)?;
        authority.attest(&paths)?;
        arm_controller_start_events(&mut events, &paths)?;
        let last_waiting =
            match observe_controller_start(&paths, &authority.instances, epoch_seconds())? {
                ControllerStartReadiness::Ready => {
                    let lock = acquire_service_global_lock(&paths)?;
                    let lease = lock.lease(&paths)?;
                    preserved(Some(&lease))?;
                    #[cfg(feature = "calibration")]
                    {
                        // A crashed parent can release its lock before its
                        // Apply journal reaches a terminal decision. Runtime
                        // readiness must not accept that parent's candidate.
                        context.require_no_native_owner()?;
                        super::traffic_classifier::require_no_selected_recovery()?;
                        super::mqtt_publisher::require_no_selected_recovery(&context.mqtt_root)?;
                    }
                    authority.attest(&paths)?;
                    match observe_controller_start(&paths, &authority.instances, epoch_seconds())? {
                        ControllerStartReadiness::Ready => {
                            if let Some(batch) = super::controller_input::load_batch(
                                &paths.runtime_root.join(".controller-input"),
                                &paths.config_root,
                            )? {
                                batch.accept(|generations| {
                                    preserved(Some(&lease))?;
                                    authority.attest(&paths)?;
                                    let expected = generations.keys().cloned().collect::<Vec<_>>();
                                    match observe_controller_start(
                                        &paths,
                                        &expected,
                                        epoch_seconds(),
                                    )? {
                                        ControllerStartReadiness::Ready => Ok(()),
                                        ControllerStartReadiness::Waiting(_) => {
                                            Err("controller-batch-readiness-changed".into())
                                        }
                                    }
                                })?;
                                preserved(Some(&lease))?;
                                if collect_inputs {
                                    if let Err(error) = collect_controller_inputs(&paths) {
                                        eprintln!(
                                            "WARNING: controller input cleanup deferred: {error}"
                                        );
                                    }
                                }
                            }
                            drop(lock);
                            return Ok(());
                        }
                        ControllerStartReadiness::Waiting(reason) => {
                            drop(lock);
                            reason
                        }
                    }
                }
                ControllerStartReadiness::Waiting(reason) => reason,
            };

        // Re-arm and re-observe before sleeping to close directory-creation
        // and atomic status-replacement races. The deadline only bounds a
        // failed startup; all ordinary progress is driven by filesystem events.
        arm_controller_start_events(&mut events, &paths)?;
        preserved(None)?;
        authority.attest(&paths)?;
        if matches!(
            observe_controller_start(&paths, &authority.instances, epoch_seconds())?,
            ControllerStartReadiness::Ready
        ) {
            continue;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "controller service did not become ready: {last_waiting}"
            ));
        }
        let readiness = events.wait(-1, Some(remaining))?;
        if readiness.signal {
            return Err("controller service readiness was interrupted".to_string());
        }
        if readiness.deadline {
            return Err(format!(
                "controller service did not become ready: {last_waiting}"
            ));
        }
    }
}

fn controller_start_authority(
    environment: &OpenWrtEnvironment,
    paths: &ServicePaths,
) -> Result<ControllerStartAuthority, String> {
    let snapshot = CommittedSnapshot::capture(&paths.config_root, &paths.runtime_root, &paths.uci)?;
    let source = super::uci_transaction::SourceReceipt::from_committed(&snapshot)?;
    let batch = super::controller_input::load_batch(
        &paths.runtime_root.join(".controller-input"),
        &paths.config_root,
    )?;
    if let Some(batch) = batch {
        batch.attest()?;
        return Ok(ControllerStartAuthority {
            source,
            instances: batch.generations().keys().cloned().collect(),
            generations: Some(batch.generations().clone()),
        });
    }
    let cake = snapshot.package(CAKE_PACKAGE)?;
    let sqm = snapshot.package(SQM_PACKAGE)?;
    let mut projected_sqm = sqm.clone();
    let projection = plan_projection(
        &cake,
        &mut projected_sqm,
        environment,
        &ProjectionScope::All,
    )?;
    if &projected_sqm != sqm {
        return Err("controller readiness found a pending SQM projection".to_string());
    }
    let plan = plan_start(&cake, &sqm, environment, &projection)?;
    Ok(ControllerStartAuthority {
        source,
        instances: plan.instances,
        generations: None,
    })
}

fn arm_controller_start_events(
    events: &mut CalibrationEventLoop,
    paths: &ServicePaths,
) -> Result<(), String> {
    let parent = paths
        .runtime_root
        .parent()
        .ok_or_else(|| "controller runtime root has no parent".to_string())?;
    let name = paths
        .runtime_root
        .file_name()
        .ok_or_else(|| "controller runtime root has no name".to_string())?;
    events.watch_named_entry(parent, name.as_bytes())?;
    events.watch_tree(&paths.runtime_root, 2, false)?;
    let processes = discover_controllers_allowing_duplicates(&paths.proc_root)?
        .into_iter()
        .map(|controller| ProcessIdentity {
            pid: controller.pid,
            process_group: controller.process_group,
            starttime_ticks: controller.starttime_ticks,
        })
        .collect::<Vec<_>>();
    events
        .refresh_processes(&processes, &paths.proc_root)
        .map(|_| ())
}

fn observe_controller_start(
    paths: &ServicePaths,
    expected: &[String],
    now_epoch: f64,
) -> Result<ControllerStartReadiness, String> {
    if crate::TERMINATE.load(Ordering::SeqCst) {
        return Err("controller service readiness was interrupted".into());
    }
    let batch = super::controller_input::load_batch(
        &paths.runtime_root.join(".controller-input"),
        &paths.config_root,
    )?;
    if let Some(batch) = &batch {
        batch.attest()?;
        if expected.len() != batch.generations().len()
            || expected
                .iter()
                .any(|instance| !batch.generations().contains_key(instance))
        {
            return Err("controller-batch-expected-membership-changed".into());
        }
    }
    let controllers = discover_controllers_allowing_duplicates(&paths.proc_root)?;
    let actual = controllers
        .iter()
        .filter(|controller| controller.kind == ServiceProcessKind::Controller)
        .collect::<Vec<_>>();
    for controller in &actual {
        if !expected.contains(&controller.instance) {
            return Err(format!(
                "unexpected controller process is running for {}",
                controller.instance
            ));
        }
    }
    if actual.len() != expected.len() {
        return Ok(ControllerStartReadiness::Waiting(format!(
            "expected {} controllers but observed {}",
            expected.len(),
            actual.len()
        )));
    }

    let boot_epoch = proc_boot_epoch(&paths.proc_root)?;
    let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks_per_second <= 0 {
        return Err("unable to determine controller clock tick rate".to_string());
    }
    for instance in expected {
        let controller = actual
            .iter()
            .find(|controller| &controller.instance == instance)
            .ok_or_else(|| format!("controller process disappeared for {instance}"))?;
        let status_path = paths.runtime_root.join(instance).join("status.json");
        let Some(status) = read_exact_file(&status_path)? else {
            return Ok(ControllerStartReadiness::Waiting(format!(
                "controller status is missing for {instance}"
            )));
        };
        if status.uid != unsafe { libc::geteuid() } || status.mode & 0o022 != 0 {
            return Err(format!(
                "controller status ownership is unsafe for {instance}"
            ));
        }
        let json = std::str::from_utf8(&status.bytes)
            .map_err(|_| format!("controller status is not UTF-8 for {instance}"))?;
        if json_string_value(json, "instance").as_deref() != Some(instance) {
            return Err(format!(
                "controller status identity is invalid for {instance}"
            ));
        }
        let state = json_string_value(json, "state")
            .ok_or_else(|| format!("controller status state is missing for {instance}"))?;
        let started_at = json_nonnegative_f64_value(json, "started_at")
            .ok_or_else(|| format!("controller start timestamp is invalid for {instance}"))?;
        let updated_at = json_nonnegative_f64_value(json, "updated_at")
            .ok_or_else(|| format!("controller update timestamp is invalid for {instance}"))?;
        let process_started_at =
            boot_epoch + controller.starttime_ticks as f64 / ticks_per_second as f64;
        if started_at + CONTROLLER_STATUS_FUTURE_SKEW < process_started_at {
            return Ok(ControllerStartReadiness::Waiting(format!(
                "controller status belongs to a prior process for {instance}"
            )));
        }
        if started_at > now_epoch + CONTROLLER_STATUS_FUTURE_SKEW
            || updated_at < started_at
            || updated_at > now_epoch + CONTROLLER_STATUS_FUTURE_SKEW
        {
            return Err(format!("controller status clock is invalid for {instance}"));
        }
        if now_epoch - updated_at > CONTROLLER_STATUS_MAX_AGE {
            return Ok(ControllerStartReadiness::Waiting(format!(
                "controller status is stale for {instance}"
            )));
        }
        match state.as_str() {
            "WAITING_OPERATION" | "RECOVERING" | "STOPPING" => {
                return Ok(ControllerStartReadiness::Waiting(format!(
                    "controller {instance} is still {state}"
                )))
            }
            "WAITING_LINK"
            | "WAITING_SQM"
            | "WAITING_EXTERNAL_SQM"
            | "RUNNING"
            | "IDLE"
            | "STALL"
            | "LEARNING"
            | "ACTIVE"
            | "STANDBY"
            | "OFFLINE" => {}
            "ERROR" => {
                return Err(format!(
                    "controller {instance} entered ERROR during startup"
                ))
            }
            _ => {
                return Err(format!(
                    "controller {instance} published unknown state {state}"
                ))
            }
        }
    }
    let mut generations = BTreeMap::new();
    for controller in &actual {
        if let Some(id) = controller_generation(&paths.proc_root, controller)? {
            let input = super::controller_input::load(
                &controller.instance,
                &id,
                &paths.runtime_root.join(".controller-input"),
                &paths.config_root,
            )?;
            input.guard.attest()?;
            generations.insert(controller.instance.clone(), id);
        }
    }
    if let Some(batch) = &batch {
        batch.attest()?;
        if &generations != batch.generations() {
            return Ok(ControllerStartReadiness::Waiting(
                "controller processes do not match the desired generation set".into(),
            ));
        }
    }
    if !generations.is_empty() || batch.is_some() {
        let observed = if batch.is_some() {
            super::procd_control::controller_batch_generations(&paths.ubus, &generations)?
        } else {
            super::procd_control::controller_generations(&paths.ubus, &generations)?
        };
        let Some(pids) = observed else {
            return Ok(ControllerStartReadiness::Waiting(
                "controller generation is not yet registered by procd".into(),
            ));
        };
        for controller in &actual {
            if generations.contains_key(&controller.instance)
                && (pids.get(&controller.instance) != Some(&controller.pid)
                    || !controller_identity_matches(&paths.proc_root, controller)?)
            {
                return Ok(ControllerStartReadiness::Waiting(
                    "controller generation process changed during readiness".into(),
                ));
            }
        }
    }
    if let Some(batch) = &batch {
        batch.attest()?;
    }
    Ok(ControllerStartReadiness::Ready)
}

fn proc_boot_epoch(proc_root: &Path) -> Result<f64, String> {
    let bytes = fs::read(proc_root.join("stat"))
        .map_err(|error| format!("unable to read controller boot time: {error}"))?;
    if bytes.len() > MAX_BRIDGER_CONFIG_BYTES || bytes.contains(&0) {
        return Err("controller boot time input exceeds its bound".to_string());
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| "controller boot time input is not UTF-8".to_string())?;
    let values = text
        .lines()
        .filter_map(|line| line.strip_prefix("btime "))
        .collect::<Vec<_>>();
    if values.len() != 1 {
        return Err("controller boot time is missing or ambiguous".to_string());
    }
    values[0]
        .parse::<u64>()
        .map(|value| value as f64)
        .map_err(|_| "controller boot time is invalid".to_string())
}

fn epoch_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

fn execute_stop_openwrt() -> Result<String, String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("service lifecycle requires root".to_string());
    }
    let paths = ServicePaths::production();
    let _lock = acquire_service_global_lock(&paths)?;
    super::sqm_runner::ensure_idle(&paths.runtime_root)?;
    #[cfg(feature = "calibration")]
    {
        let (existing, bootstrap) = native_apply_recovery_markers_present()?;
        require_no_native_apply_recovery(existing, bootstrap)?;
        super::traffic_classifier::require_no_selected_recovery()?;
        super::mqtt_publisher::require_no_selected_recovery(Path::new(
            super::mqtt_publisher::PRODUCTION_PLAN_ROOT,
        ))?;
    }
    let requested_source = expected_source_id()?;
    let initial_source = if requested_source.is_some() {
        let snapshot =
            CommittedSnapshot::capture(&paths.config_root, &paths.runtime_root, &paths.uci)?;
        Some(check_source_handoff(
            &snapshot,
            requested_source.as_deref(),
        )?)
    } else {
        None
    };
    let mut backend = OpenWrtServiceStop {
        environment: OpenWrtEnvironment::production(),
        paths,
        expected_source: requested_source.clone(),
        restored_source: None,
        committed: None,
        applied: BTreeMap::new(),
        batch: None,
    };
    let sqm_paths = OpenWrtPaths::from_environment();
    let orphan_restored = recover_unregistered_publication(&backend.paths, |specs| {
        if !discover_controllers(&backend.paths.proc_root)?.is_empty()
            || !super::procd_control::service_absent(&backend.paths.ubus)?
            || !live_input_references(&backend.paths)?.is_empty()
        {
            return Err("unregistered-start-runtime-still-referenced".into());
        }
        super::sqm_runner::ensure_idle(&backend.paths.runtime_root)?;
        for spec in specs {
            super::sqm_recovery_openwrt::attest_managed_sqm_absent(spec, &sqm_paths)
                .map_err(|error| sqm_error_message(&error).to_string())?;
        }
        Ok(())
    })?;
    if orphan_restored.is_some() || initial_source.is_some() {
        let snapshot = CommittedSnapshot::capture(
            &backend.paths.config_root,
            &backend.paths.runtime_root,
            &backend.paths.uci,
        )?;
        let source = if let Some(restored) = orphan_restored {
            restored.attest_snapshot(&snapshot)?;
            super::uci_transaction::SourceReceipt::from_committed(&snapshot)?
        } else {
            check_source_handoff(
                &snapshot,
                Some(
                    &initial_source
                        .as_ref()
                        .ok_or("service-source-handoff-missing")?
                        .fingerprint()?,
                ),
            )?
        };
        backend.expected_source = Some(source.fingerprint()?);
    }
    execute_stop(&mut backend)?;
    if requested_source.is_some() {
        let source = if let Some(restored) = backend.restored_source.take() {
            backend.committed.take(); // release the retained private alias lease
            let snapshot = CommittedSnapshot::capture(
                &backend.paths.config_root,
                &backend.paths.runtime_root,
                &backend.paths.uci,
            )?;
            restored.attest_snapshot(&snapshot)?;
            super::uci_transaction::SourceReceipt::from_committed(&snapshot)?
        } else {
            super::uci_transaction::SourceReceipt::from_committed(
                backend
                    .committed
                    .as_ref()
                    .ok_or("service Stop has no committed snapshot")?,
            )?
        };
        return Ok(format!("service-stop-v2 {}\n", source.fingerprint()?));
    }
    Ok("service-stop-v1 ok\n".to_string())
}

#[cfg(feature = "calibration")]
enum SelectedGenerationMode {
    Replace,
    Remove,
    Supersede,
    Reuse,
    Retry,
}

/// Held by the selected Native Apply backend from preflight, before healthy
/// Stop, through registration. The parent Apply journal still owns UCI rollback.
#[cfg(feature = "calibration")]
pub(crate) struct SelectedGeneration {
    paths: ServicePaths,
    published: super::uci_transaction::PublishedConfig,
    previous: super::controller_input::Batch,
    pending: Option<super::controller_input::Batch>,
    instance: String,
    input: Option<super::controller_input::Guard>,
    mode: SelectedGenerationMode,
}

#[cfg(feature = "calibration")]
pub(crate) fn prepare_selected_generation(
    instance: &str,
    expected_cake: &UciPackage,
    expected_sqm: &UciPackage,
    controller_should_run: bool,
) -> Result<Option<SelectedGeneration>, String> {
    let paths = ServicePaths::production();
    prepare_selected_generation_with_paths(
        &paths,
        instance,
        expected_cake,
        expected_sqm,
        controller_should_run,
    )
}

#[cfg(feature = "calibration")]
fn prepare_selected_generation_with_paths(
    paths: &ServicePaths,
    instance: &str,
    expected_cake: &UciPackage,
    expected_sqm: &UciPackage,
    controller_should_run: bool,
) -> Result<Option<SelectedGeneration>, String> {
    let inputs = paths.runtime_root.join(".controller-input");
    let Some(current) =
        super::controller_input::load_batch_for_selected_preparation(&inputs, &paths.config_root)?
    else {
        // Compatibility for a pre-generation service; ordinary v3 startup
        // creates a complete batch before this path becomes generation-aware.
        return Ok(None);
    };
    let previous = current.applied_predecessor()?;
    let pending = current.pending_update()?;
    if !controller_should_run && !previous.generations().contains_key(instance) && pending.is_none()
    {
        return Ok(None);
    }
    if pending
        .as_ref()
        .is_some_and(|batch| !batch.updates_only(instance))
    {
        return Err("selected-generation-other-update-pending".into());
    }
    let snapshot = CommittedSnapshot::capture(&paths.config_root, &paths.runtime_root, &paths.uci)?;
    if snapshot.package(CAKE_PACKAGE)? != expected_cake
        || snapshot.package(SQM_PACKAGE)? != expected_sqm
    {
        return Err("selected-generation-config-changed".into());
    }
    // No edits: the parent already installed and owns these candidate files.
    let prepared = snapshot.prepare([&[], &[]], &paths.uci)?;
    let published = super::uci_transaction::publish(prepared)?;
    let retained = previous
        .generations()
        .iter()
        .filter(|(name, _)| name.as_str() != instance)
        .map(|(name, id)| (name.clone(), id.clone()))
        .collect::<BTreeMap<_, _>>();
    attest_selected_peer_processes(paths, &retained, instance, true)?;
    previous.attest_record()?;
    published.attest()?;
    let mut old_inputs = previous.load_inputs()?;
    let old = old_inputs.remove(instance);
    let reuse = if !controller_should_run {
        !previous.generations().contains_key(instance)
    } else {
        match old.as_ref() {
            Some(input) => input.matches_published(&published)?,
            None => false,
        }
    };
    if !reuse && super::controller_input::restore_pending(&inputs)? {
        return Err("selected-generation-restore-source-mismatch".into());
    }
    let (input, mode) = if reuse {
        (
            if controller_should_run {
                Some(old.ok_or("selected-generation-input-missing")?.guard)
            } else {
                None
            },
            SelectedGenerationMode::Reuse,
        )
    } else if let Some(pending) = &pending {
        if pending.generations().contains_key(instance) != controller_should_run {
            return Err("selected-generation-pending-direction-changed".into());
        }
        match pending.attest() {
            Ok(()) => {
                let input = pending
                    .load_inputs()?
                    .remove(instance)
                    .map(|input| input.guard);
                if let Some(input) = &input {
                    input.attest()?;
                }
                (input, SelectedGenerationMode::Retry)
            }
            Err(error) if error == "uci-transaction-source-receipt-changed" => {
                pending.attest_reinstalled_content()?;
                let input = if controller_should_run {
                    Some(super::controller_input::store_for_selected_update(
                        &published,
                        instance,
                        &inputs,
                        &paths.config_root,
                    )?)
                } else {
                    super::controller_input::require_update_capacity(&inputs)?;
                    None
                };
                (input, SelectedGenerationMode::Supersede)
            }
            Err(error) => return Err(error),
        }
    } else if !controller_should_run {
        super::controller_input::require_update_capacity(&inputs)?;
        (None, SelectedGenerationMode::Remove)
    } else {
        (
            Some(super::controller_input::store_for_selected_update(
                &published,
                instance,
                &inputs,
                &paths.config_root,
            )?),
            SelectedGenerationMode::Replace,
        )
    };
    let prepared = SelectedGeneration {
        paths: paths.clone(),
        published,
        previous,
        pending,
        instance: instance.into(),
        input,
        mode,
    };
    prepared.attest_peers(true)?;
    Ok(Some(prepared))
}

#[cfg(feature = "calibration")]
impl SelectedGeneration {
    fn retained(&self) -> BTreeMap<String, String> {
        self.previous
            .generations()
            .iter()
            .filter(|(name, _)| name.as_str() != self.instance)
            .map(|(name, id)| (name.clone(), id.clone()))
            .collect()
    }
    fn attest_peers(&self, allow_selected: bool) -> Result<(), String> {
        self.published.attest()?;
        self.previous.attest_record()?;
        let retained = self.retained();
        attest_selected_peer_processes(&self.paths, &retained, &self.instance, allow_selected)?;
        self.previous.attest_record()?;
        self.published.attest()
    }
    /// Call after the selected runtime postcondition and before procd registration.
    pub(crate) fn prepare_registration(&self) -> Result<Option<String>, String> {
        self.attest_peers(false)?;
        match self.mode {
            SelectedGenerationMode::Replace => {
                super::controller_input::replace_selected_batch(
                    &self.published,
                    &self.previous,
                    &self.instance,
                    self.input
                        .as_ref()
                        .ok_or("selected-generation-input-missing")?,
                    |_| self.attest_peers(false),
                )?;
            }
            SelectedGenerationMode::Remove => {
                super::controller_input::remove_selected_batch(
                    &self.published,
                    &self.previous,
                    &self.instance,
                    |_| self.attest_peers(false),
                )?;
            }
            SelectedGenerationMode::Reuse => {
                if let Some(pending) = &self.pending {
                    pending.begin_restore(|_| self.attest_peers(false))?;
                }
            }
            SelectedGenerationMode::Retry => {
                let pending = self
                    .pending
                    .as_ref()
                    .ok_or("selected-generation-input-missing")?;
                pending.attest()?;
                pending.cleanup_superseded(|| self.attest_peers(false))?;
            }
            SelectedGenerationMode::Supersede => {
                super::controller_input::supersede_pending_batch(
                    &self.published,
                    self.pending
                        .as_ref()
                        .ok_or("selected-generation-input-missing")?,
                    &self.instance,
                    self.input.as_ref(),
                    |_| self.attest_peers(false),
                )?;
            }
        }
        if let Some(input) = &self.input {
            input.attest()?;
        }
        self.published.attest()?;
        Ok(self
            .input
            .as_ref()
            .map(|input| input.generation().to_string()))
    }
}

fn attest_selected_peer_processes(
    paths: &ServicePaths,
    retained: &BTreeMap<String, String>,
    selected: &str,
    allow_selected: bool,
) -> Result<(), String> {
    let actual = discover_controllers(&paths.proc_root)?;
    let actual = actual
        .iter()
        .filter(|p| p.kind == ServiceProcessKind::Controller)
        .filter(|p| !allow_selected || p.instance != selected)
        .collect::<Vec<_>>();
    if actual.len() != retained.len() || actual.iter().any(|p| !retained.contains_key(&p.instance))
    {
        return Err("selected-generation-process-membership-changed".into());
    }
    let pids = if allow_selected {
        super::procd_control::controller_generations(&paths.ubus, retained)?
    } else {
        super::procd_control::controller_batch_generations(&paths.ubus, retained)?
    }
    .ok_or("selected-generation-peer-not-registered")?;
    for process in actual {
        if controller_generation(&paths.proc_root, process)?.as_ref()
            != retained.get(&process.instance)
            || pids.get(&process.instance) != Some(&process.pid)
            || !controller_identity_matches(&paths.proc_root, process)?
        {
            return Err("selected-generation-peer-changed".into());
        }
    }
    Ok(())
}

/// Only the owning native bootstrap rollback calls this after restoring its
/// exact originally-absent baseline. The lock is an explicit ownership lease;
/// baseline proof is repeated across every private registry mutation.
#[cfg(feature = "calibration")]
pub(crate) fn settle_absent_bootstrap_generation(
    instance: &str,
    _lock: &super::autotune_apply_runtime::NativeApplyGlobalLock,
    proof: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    settle_absent_bootstrap_generation_in(&ServicePaths::production(), instance, proof)
}

#[cfg(feature = "calibration")]
fn settle_absent_bootstrap_generation_in(
    paths: &ServicePaths,
    instance: &str,
    mut proof: impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    if !safe_name(instance) {
        return Err("bootstrap-generation-instance-invalid".into());
    }
    proof()?;
    let root = paths.runtime_root.join(".controller-input");
    let Some(current) =
        super::controller_input::load_batch_for_selected_preparation(&root, &paths.config_root)?
    else {
        if !super::procd_control::generation_references(&paths.ubus)?.is_empty() {
            return Err("bootstrap-generation-registry-missing".into());
        }
        return proof();
    };
    let previous = current.applied_predecessor()?;
    if previous.generations().contains_key(instance) {
        return Err("bootstrap-generation-predecessor-is-not-absent".into());
    }
    let pending = current.pending_update()?;
    if pending.as_ref().is_some_and(|pending| {
        !pending.updates_only(instance)
            || pending
                .generations()
                .iter()
                .filter(|(name, _)| name.as_str() != instance)
                .ne(previous.generations().iter())
    }) {
        return Err("bootstrap-generation-foreign-update-pending".into());
    }
    {
        let mut check = |generations: &BTreeMap<String, String>| -> Result<(), String> {
            proof()?;
            if generations != previous.generations() {
                return Err("bootstrap-generation-peer-set-changed".into());
            }
            attest_selected_peer_processes(paths, generations, instance, false)?;
            previous.attest_record()?;
            proof()
        };
        if let Some(pending) = pending {
            pending.abandon_update(&mut check)?;
        } else if super::controller_input::restore_pending(&root)? {
            previous.accept(&mut check)?;
        } else {
            // Already settled: do not touch another operation's garbage or
            // replace an accepted peer registry merely to report idempotence.
            check(previous.generations())?;
        }
    }
    let settled = super::controller_input::load_batch(&root, &paths.config_root)?
        .ok_or("bootstrap-generation-registry-disappeared")?;
    if settled.pending()
        || settled.pending_update()?.is_some()
        || super::controller_input::restore_pending(&root)?
        || settled.generations() != previous.generations()
    {
        return Err("bootstrap-generation-restoration-incomplete".into());
    }
    attest_selected_peer_processes(paths, settled.generations(), instance, false)?;
    settled.attest_record()?;
    proof()
}

fn require_idle_reload_sidecars(context: &LifecycleContext) -> Result<(), String> {
    #[cfg(feature = "calibration")]
    {
        super::traffic_classifier::require_no_selected_recovery()?;
        super::mqtt_publisher::require_no_selected_recovery(&context.mqtt_root)?;
    }
    let _ = context;
    Ok(())
}

fn recover_reload_preparation(
    context: &LifecycleContext,
    paths: &ServicePaths,
    resolver: &impl InterfaceResolver,
    previous: &super::controller_input::Batch,
    lease: &ServiceGlobalLease<'_>,
) -> Result<Option<super::uci_transaction::SourceReceipt>, String> {
    require_idle_reload_sidecars(context)?;
    let controllers = reload_runtime::ReloadControllers::capture(
        paths,
        previous.generations(),
        previous.generations(),
        previous.generations(),
    )?;
    let queues = RetainedSqmSet::capture(previous, &[], resolver, lease)?;
    #[cfg(feature = "calibration")]
    let classifier = super::traffic_classifier::ClassifierWitness::capture()?;
    #[cfg(feature = "calibration")]
    let mqtt = reload_runtime::MqttWitness::capture(paths)?;
    ReloadPublication::recover_preparation(paths, |generations| {
        previous.attest_record()?;
        if generations != previous.generations() {
            return Err("reload-preparation-predecessor-changed".into());
        }
        controllers.attest_stopped(paths)?;
        queues.attest_under_lifecycle(lease)?;
        #[cfg(feature = "calibration")]
        {
            classifier.attest()?;
            mqtt.attest(paths)?;
        }
        Ok(())
    })
}

fn execute_reload_openwrt() -> Result<String, String> {
    require_service_root()?;
    execute_reload_in(&LifecycleContext::production())
}

fn execute_reload_in(context: &LifecycleContext) -> Result<String, String> {
    if package_upgrade_mode()? {
        return Err("service reload deferred during package replacement".into());
    }
    // This command owns release/reacquisition for readiness. A borrowed shell
    // owner cannot remain locked while newly registered controllers initialize.
    if std::env::var_os("CAKE_AUTORATE_SERVICE_LOCK_BORROW").is_some_and(|v| v == "1") {
        return Err("ordinary reload requires its own lifecycle lock".into());
    }
    let paths = context.paths.clone();
    let environment = context.environment.clone();
    let lock = acquire_service_global_lock(&paths)?;
    let lease = lock.lease(&paths)?;
    context.require_no_native_owner()?;
    super::sqm_runner::ensure_idle(&paths.runtime_root)?;
    let inputs_root = paths.runtime_root.join(".controller-input");
    let mut restored = None;
    let batch = match super::controller_input::load_batch(&inputs_root, &paths.config_root) {
        Ok(batch) => batch,
        Err(error) => {
            let recovery =
                super::controller_input::UnpublishedReload::load(&inputs_root, &paths.config_root)?
                    .ok_or(error)?;
            restored = recover_reload_preparation(
                context,
                &paths,
                &environment,
                &recovery.previous,
                &lease,
            )?;
            super::controller_input::load_batch(&inputs_root, &paths.config_root)?
        }
    };
    let Some(batch) = batch else {
        if !super::procd_control::generation_references(&paths.ubus)?.is_empty() {
            return Err("reload generation registry is missing".into());
        }
        for process in discover_controllers(&paths.proc_root)?
            .iter()
            .filter(|p| p.kind == ServiceProcessKind::Controller)
        {
            if controller_generation(&paths.proc_root, process)?.is_some() {
                return Err("reload generation registry is missing".into());
            }
        }
        require_idle_reload_sidecars(context)?;
        return Ok("service-reload-v1 legacy\n".into());
    };
    let pending = batch.pending_update()?;
    let (reload, retained_before_publication) = if let Some(pending) = pending {
        if !pending.is_reload_update() {
            return Err("reload refuses another pending generation owner".into());
        }
        pending.attest()?;
        let candidate = preflight_start_candidate(&paths, &environment)?;
        if let Some(expected) = expected_source_id()? {
            check_source_handoff(&candidate.config.original, Some(&expected))?;
        }
        (
            ReloadPublication::resume(candidate, pending, &environment)?,
            None,
        )
    } else {
        require_idle_reload_sidecars(context)?;
        batch.attest_settled()?;
        batch
            .accept(|generations| attest_selected_peer_processes(&paths, generations, "", false))?;
        if super::uci_transaction::recovery_pending(&paths.config_root)? {
            restored = recover_reload_preparation(context, &paths, &environment, &batch, &lease)?;
        }
        let candidate = preflight_start_candidate(&paths, &environment)?;
        if let Some(original) = &restored {
            original.attest_snapshot(&candidate.config.original)?;
            if let Some(expected) = expected_source_id()? {
                if original.fingerprint()? != expected {
                    return Err("reload source handoff differs from restored authority".into());
                }
            }
        } else {
            check_source_handoff(&candidate.config.original, expected_source_id()?.as_deref())?;
        }
        if reload_is_unchanged(&paths, &environment, &candidate, |cake| {
            #[cfg(feature = "calibration")]
            {
                if !super::traffic_classifier::frozen_rules_unchanged(cake)? {
                    return Ok(None);
                }
                super::mqtt_publisher::attest_service_plans(cake, &context.mqtt_root).map(Some)
            }
            #[cfg(not(feature = "calibration"))]
            {
                let _ = cake;
                Ok(Some(Vec::new()))
            }
        })? {
            return Ok("service-reload-v1 unchanged\n".into());
        }
        let controllers = reload_runtime::ReloadControllers::capture(
            &paths,
            batch.generations(),
            batch.generations(),
            batch.generations(),
        )?;
        let queues = RetainedSqmSet::capture(&batch, &[], &environment, &lease)?;
        let delta = ServiceReloadDelta::prepare(&candidate, &batch, &environment)?;
        let retained = RetainedSqmSet::capture(&batch, &delta.stop_sqm, &environment, &lease)?;
        #[cfg(feature = "calibration")]
        let classifier = super::traffic_classifier::ClassifierWitness::capture()?;
        #[cfg(feature = "calibration")]
        let mqtt = reload_runtime::MqttWitness::capture(&paths)?;
        let previous = batch.clone();
        let published =
            ReloadPublication::publish(&paths, candidate, batch, &environment, |generations| {
                previous.attest_record()?;
                if generations != previous.generations() {
                    return Err("reload predecessor changed".into());
                }
                controllers.attest_stopped(&paths)?;
                queues.attest_under_lifecycle(&lease)?;
                #[cfg(feature = "calibration")]
                {
                    classifier.attest()?;
                    mqtt.attest(&paths)?;
                }
                Ok(())
            })?;
        retained.attest_under_lifecycle(&lease)?;
        (published, Some(retained))
    };
    let queues = match retained_before_publication {
        Some(queues) => queues,
        None => reload.pin_retained_queues(&environment, &lease)?,
    };
    #[cfg(feature = "calibration")]
    {
        // Only the already-bound reload may settle its interrupted sidecar
        // before an ordinary preserved-state snapshot can be captured again.
        if super::traffic_classifier::require_no_selected_recovery().is_err() {
            reload.reconcile_classifier(&environment, || queues.attest_under_lifecycle(&lease))?;
        }
        if super::mqtt_publisher::require_no_selected_recovery(&context.mqtt_root).is_err() {
            reload.reconcile_mqtt(&paths, &environment, &context.mqtt_root, || {
                queues.attest_under_lifecycle(&lease)
            })?;
        }
    }
    #[cfg(feature = "calibration")]
    let classifier_before = super::traffic_classifier::ClassifierWitness::capture()?;
    #[cfg(feature = "calibration")]
    let mqtt_before = reload_runtime::MqttWitness::capture(&paths)?;
    let mut profiles = Vec::with_capacity(reload.delta.start_sqm.len());
    for _ in &reload.delta.start_sqm {
        profiles.push(super::sqm_runner::Profile::inspect(
            &OpenWrtPaths::from_environment().sqm_run,
        )?);
    }
    let before_sidecars = || -> Result<(), String> {
        queues.attest_under_lifecycle(&lease)?;
        #[cfg(feature = "calibration")]
        {
            classifier_before.attest()?;
            mqtt_before.attest(&paths)?;
        }
        Ok(())
    };
    reload.stop_controllers(&paths, &environment, before_sidecars)?;
    if !reload.delta.start_sqm.is_empty() {
        sync_bridger_blacklist(&paths, reload.start.projection.ingress_interfaces())?;
        for spec in &reload.delta.start_sqm {
            remove_empty_clsact(&paths, &spec.target_interface)?;
        }
    }
    reload.reconcile_sqm(&paths, &environment, profiles, before_sidecars)?;
    #[cfg(feature = "calibration")]
    let stopped = reload_runtime::ReloadControllers::capture(
        &paths,
        reload.previous.generations(),
        reload.start.batch.generations(),
        &reload.delta.retained,
    )?;
    #[cfg(feature = "calibration")]
    reload.reconcile_classifier(&environment, || {
        queues.attest_under_lifecycle(&lease)?;
        stopped.attest_stopped(&paths)?;
        mqtt_before.attest(&paths)
    })?;
    #[cfg(feature = "calibration")]
    let classifier_after = super::traffic_classifier::ClassifierWitness::capture()?;
    #[cfg(feature = "calibration")]
    reload.reconcile_mqtt(&paths, &environment, &context.mqtt_root, || {
        queues.attest_under_lifecycle(&lease)?;
        stopped.attest_stopped(&paths)?;
        classifier_after.attest()
    })?;
    #[cfg(feature = "calibration")]
    let mqtt_after = reload_runtime::MqttWitness::capture(&paths)?;
    let registration = reload.register_controllers(&paths, &environment, || {
        queues.attest_under_lifecycle(&lease)?;
        #[cfg(feature = "calibration")]
        {
            classifier_after.attest()?;
            mqtt_after.attest(&paths)?;
        }
        Ok(())
    })?;
    let expected = reload.start.batch.clone();
    let source = reload.start.published.source_receipt()?;
    let desired_sqm = reload.start.plan.managed.clone();
    let cake = reload
        .start
        .published
        .config()
        .package(CAKE_PACKAGE)?
        .clone();
    drop(reload); // Release private aliases before readiness captures its view.
    drop(lock); // New controllers may now leave WAITING_OPERATION.
    confirm_controller_service_started_in(
        context,
        |lease| {
            source.attest(&paths.config_root)?;
            let current = super::controller_input::load_batch(&inputs_root, &paths.config_root)?
                .ok_or("reload batch disappeared before acceptance")?;
            if !current.same_publication(&expected) {
                return Err("reload batch changed before acceptance".into());
            }
            registration.attest(&paths)?;
            match lease {
                Some(lease) => {
                    queues.attest_under_lifecycle(lease)?;
                    let inputs = current.load_inputs()?;
                    for spec in &desired_sqm {
                        let mut live_spec = spec.clone();
                        let section = cake
                            .sections
                            .get(&spec.instance)
                            .ok_or("reload desired controller section missing")?;
                        let policy = managed_sqm_rate_policy_from_options(&section.options)
                            .map_err(|e| sqm_error_message(&e).to_string())?;
                        live_spec.minimum_download_kbps = policy.minimum_download_kbps;
                        live_spec.maximum_download_kbps = policy.maximum_download_kbps;
                        live_spec.minimum_upload_kbps = policy.minimum_upload_kbps;
                        live_spec.maximum_upload_kbps = policy.maximum_upload_kbps;
                        let input = inputs
                            .get(&spec.instance)
                            .ok_or("reload desired controller input missing")?;
                        super::sqm_recovery_openwrt::attest_input_during_lifecycle(
                            &live_spec, input, lease,
                        )
                        .map_err(|e| sqm_error_message(&e).to_string())?;
                    }
                }
                None => queues.attest()?,
            }
            #[cfg(feature = "calibration")]
            {
                classifier_after.attest()?;
                mqtt_after.attest(&paths)?;
                super::mqtt_publisher::attest_service_plans(&cake, &context.mqtt_root)?;
            }
            Ok(())
        },
        false,
    )?;
    drop(queues);
    // GC is optional and comes after witness leases are no longer consumers.
    if let Ok(_lock) = acquire_service_global_lock(&paths) {
        if let Err(error) = collect_controller_inputs(&paths) {
            eprintln!("WARNING: controller input cleanup deferred: {error}");
        }
    }
    Ok("service-reload-v1 ready\n".into())
}

fn require_service_root() -> Result<(), String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("service lifecycle requires root".to_string());
    }
    Ok(())
}

fn preflight_start(allow_unchanged: bool) -> Result<String, String> {
    require_service_root()?;
    let paths = ServicePaths::production();
    let _lock = acquire_service_global_lock(&paths)?;
    #[cfg(feature = "calibration")]
    {
        let (existing, bootstrap) = native_apply_recovery_markers_present()?;
        require_no_native_apply_recovery(existing, bootstrap)?;
        super::traffic_classifier::require_no_selected_recovery()?;
        super::mqtt_publisher::require_no_selected_recovery(Path::new(
            super::mqtt_publisher::PRODUCTION_PLAN_ROOT,
        ))?;
    }
    let environment = OpenWrtEnvironment::production();
    let candidate = preflight_start_candidate(&paths, &environment)?;
    let source = super::uci_transaction::SourceReceipt::from_committed(&candidate.config.original)?;
    if allow_unchanged
        && reload_is_unchanged(&paths, &environment, &candidate, |cake| {
            #[cfg(feature = "calibration")]
            {
                if !super::traffic_classifier::frozen_rules_unchanged(cake)? {
                    return Ok(None);
                }
                super::mqtt_publisher::attest_service_plans(
                    cake,
                    Path::new(super::mqtt_publisher::PRODUCTION_PLAN_ROOT),
                )
                .map(Some)
            }
            #[cfg(not(feature = "calibration"))]
            {
                let _ = cake;
                Ok(Some(Vec::new()))
            }
        })?
    {
        candidate.attest_plan(&environment)?;
        return Ok(format!(
            "service-preflight-unchanged-v1 {}\n",
            source.fingerprint()?
        ));
    }
    candidate.attest_plan(&environment)?;
    Ok(format!("service-preflight-v2 {}\n", source.fingerprint()?))
}

/// Validate before healthy Stop and return the original source proof. Both
/// consumers must match its exact identity and still validate their own plans;
/// only a verified rollback may replace that source handoff.
fn preflight_start_candidate(
    paths: &ServicePaths,
    environment: &OpenWrtEnvironment,
) -> Result<StartCandidate, String> {
    let snapshot = CommittedSnapshot::capture(&paths.config_root, &paths.runtime_root, &paths.uci)?;
    let candidate = StartCandidate::prepare_with_mq_refresh(
        snapshot,
        environment,
        &paths.uci,
        crate::qdisc_capabilities::ensure,
    )?;
    if !candidate.plan.managed.is_empty() {
        super::sqm_runner::Profile::inspect(&OpenWrtPaths::from_environment().sqm_run)?;
    }
    candidate.attest_plan(environment)?;
    Ok(candidate)
}

fn expected_source_id() -> Result<Option<String>, String> {
    let Some(value) = std::env::var_os("CAKE_AUTORATE_SERVICE_SOURCE_ID") else {
        return Ok(None);
    };
    source_id_value(&value)
}
fn source_id_value(value: &std::ffi::OsStr) -> Result<Option<String>, String> {
    let value = value.to_str().ok_or("service-source-handoff-invalid")?;
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("service-source-handoff-invalid".into());
    }
    Ok(Some(value.into()))
}

fn check_source_handoff(
    snapshot: &CommittedSnapshot,
    expected: Option<&str>,
) -> Result<super::uci_transaction::SourceReceipt, String> {
    let source = super::uci_transaction::SourceReceipt::from_committed(snapshot)?;
    if let Some(expected) = expected {
        if source.fingerprint()? != expected {
            return Err("service-source-changed-since-preflight".into());
        }
    }
    Ok(source)
}

fn prepare_start() -> Result<String, String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("service lifecycle requires root".to_string());
    }
    // OpenWrt's default_postinst invokes service start before the package's
    // own guarded restart hook. A distinct typed deferral prevents both
    // topology mutation and procd registration without conflating package
    // replacement with a genuine empty controller plan; the later
    // PKG_UPGRADE=0 restart owns the complete replacement lifecycle.
    if package_upgrade_mode()? {
        return Ok(format!("{SERVICE_START_DEFERRED_V1}\n"));
    }
    // The start path mutates both UCI packages and the live SQM topology.
    // Acquire the same authority as stop before presets or projection can
    // commit anything. Full rc.common deliberately lends its already-held
    // descriptor; Lite has no calibration peer and acquires it here.
    let paths = ServicePaths::production();
    let _lock = acquire_service_global_lock(&paths)?;
    #[cfg(feature = "calibration")]
    {
        let (existing, bootstrap) = native_apply_recovery_markers_present()?;
        require_no_native_apply_recovery(existing, bootstrap)?;
        super::traffic_classifier::require_no_selected_recovery()?;
        super::mqtt_publisher::require_no_selected_recovery(Path::new(
            super::mqtt_publisher::PRODUCTION_PLAN_ROOT,
        ))?;
    }
    let environment = OpenWrtEnvironment::production();
    super::sqm_runner::ensure_idle(&paths.runtime_root)?;
    let expected_source = expected_source_id()?;
    let snapshot = CommittedSnapshot::capture(&paths.config_root, &paths.runtime_root, &paths.uci)?;
    check_source_handoff(&snapshot, expected_source.as_deref())?;
    let candidate = StartCandidate::prepare_with_mq_refresh(
        snapshot,
        &environment,
        &paths.uci,
        crate::qdisc_capabilities::ensure,
    )?;
    if let Some(batch) = super::controller_input::load_batch(
        &paths.runtime_root.join(".controller-input"),
        &paths.config_root,
    )? {
        return unchanged_start_plan(&paths, &environment, &candidate, &batch, |cake| {
            #[cfg(feature = "calibration")]
            {
                if !super::traffic_classifier::frozen_rules_unchanged(cake)? {
                    return Err("service-start-classifier-change-requires-restart".into());
                }
                super::mqtt_publisher::attest_service_plans(
                    cake,
                    Path::new(super::mqtt_publisher::PRODUCTION_PLAN_ROOT),
                )
            }
            #[cfg(not(feature = "calibration"))]
            {
                let _ = cake;
                Ok(Vec::new())
            }
        });
    }
    // A legacy/live registration cannot be adopted merely because it has no
    // generation file. An explicit attested Stop owns that transition.
    if !discover_controllers(&paths.proc_root)?.is_empty()
        || !super::procd_control::service_absent(&paths.ubus)?
    {
        return Err("service-start-existing-runtime-requires-stop".into());
    }
    let sqm_paths = OpenWrtPaths::from_environment();
    let mut profiles = Vec::with_capacity(candidate.plan.managed.len());
    for _ in &candidate.plan.managed {
        profiles.push(super::sqm_runner::Profile::inspect(&sqm_paths.sqm_run)?);
    }
    candidate.attest_plan(&environment)?;
    let start = StartPublication::publish(&paths, candidate)?;
    start.attest(&environment)?;
    sync_bridger_blacklist(&paths, start.projection.ingress_interfaces())?;
    start.attest(&environment)?;
    for interface in start.projection.ingress_interfaces() {
        remove_empty_clsact(&paths, interface)?;
        start.attest(&environment)?;
    }
    start_sqm_backend(&paths, &start, profiles, &environment)?;
    #[cfg(feature = "calibration")]
    let cake = start.published.config().package(CAKE_PACKAGE)?;
    #[cfg(feature = "calibration")]
    if let Err(error) = super::traffic_classifier::apply_frozen(cake, || start.attest(&environment))
    {
        eprintln!("WARNING: native traffic classifier is degraded: {error}");
    }

    start.attest(&environment)?;
    #[cfg(feature = "calibration")]
    let mqtt_instances = publish_production_service_plans(cake)?;
    #[cfg(not(feature = "calibration"))]
    let mqtt_instances = Vec::new();
    start.attest(&environment)?;
    for spec in &start.plan.managed {
        super::sqm_recovery_openwrt::attest_managed_sqm_from_published(spec, &start.published)
            .map_err(|error| sqm_error_message(&error).to_string())?;
    }
    start.attest(&environment)?;
    encode_generation_start_plan(start.batch.generations(), &mqtt_instances)
}

fn unchanged_start_plan(
    paths: &ServicePaths,
    resolver: &impl InterfaceResolver,
    candidate: &StartCandidate,
    batch: &super::controller_input::Batch,
    attest_mqtt: impl FnOnce(&UciPackage) -> Result<Vec<String>, String>,
) -> Result<String, String> {
    candidate.attest_plan(resolver)?;
    batch.attest()?;
    if batch.pending() || batch.pending_update()?.is_some() {
        return Err("service-start-pending-generation-recovery-required".into());
    }
    if !applied_configuration_matches(candidate, batch, resolver)? {
        return Err("service-start-changed-generation-requires-restart".into());
    }
    attest_selected_peer_processes(paths, batch.generations(), "", false)?;
    let mqtt = attest_mqtt(candidate.config.package(CAKE_PACKAGE)?)?;
    batch.attest()?;
    candidate.attest_plan(resolver)?;
    encode_generation_start_plan(batch.generations(), &mqtt)
}

fn applied_configuration_matches(
    candidate: &StartCandidate,
    batch: &super::controller_input::Batch,
    resolver: &impl InterfaceResolver,
) -> Result<bool, String> {
    batch.attest_record()?;
    if batch.pending() || batch.pending_update()?.is_some() {
        return Ok(false);
    }
    let delta = ServiceReloadDelta::prepare(candidate, batch, resolver)?;
    Ok(!delta.publication_required
        && delta.stop_controllers.is_empty()
        && delta.start_controllers.is_empty()
        && delta.stop_sqm.is_empty()
        && delta.start_sqm.is_empty())
}

fn reload_is_unchanged(
    paths: &ServicePaths,
    resolver: &impl InterfaceResolver,
    candidate: &StartCandidate,
    attest_services: impl FnOnce(&UciPackage) -> Result<Option<Vec<String>>, String>,
) -> Result<bool, String> {
    let root = paths.runtime_root.join(".controller-input");
    if super::controller_input::unregistered_store(&root)?.is_some() {
        return Ok(false);
    }
    let batch = super::controller_input::load_batch(&root, &paths.config_root)?
        .ok_or("controller-batch-missing")?;
    if !applied_configuration_matches(candidate, &batch, resolver)? {
        return Ok(false);
    }
    // A stopped/respawning registered set needs normal restart preparation,
    // not a successful no-op report about processes that are not running.
    if super::procd_control::controller_batch_generations(&paths.ubus, batch.generations())?
        .is_none()
    {
        return Ok(false);
    }
    let Some(mqtt) = attest_services(candidate.config.package(CAKE_PACKAGE)?)? else {
        return Ok(false);
    };
    unchanged_start_plan(paths, resolver, candidate, &batch, |_| Ok(mqtt))?;
    candidate.attest_plan(resolver)?;
    Ok(true)
}

fn package_upgrade_mode() -> Result<bool, String> {
    package_upgrade_mode_value(std::env::var_os("PKG_UPGRADE").as_deref())
}

fn package_upgrade_mode_value(value: Option<&std::ffi::OsStr>) -> Result<bool, String> {
    match value {
        None => Ok(false),
        Some(value) if value.is_empty() || value == "0" => Ok(false),
        Some(value) if value == "1" => Ok(true),
        Some(_) => Err("PKG_UPGRADE must be empty, 0, or 1".to_string()),
    }
}

#[cfg(feature = "calibration")]
fn require_no_native_apply_recovery(existing: bool, bootstrap: bool) -> Result<(), String> {
    if existing || bootstrap {
        return Err(
            "service lifecycle refuses a pending native Apply recovery transaction".to_string(),
        );
    }
    Ok(())
}

fn plan_start(
    cake: &UciPackage,
    sqm: &UciPackage,
    resolver: &impl InterfaceResolver,
    projection: &SqmProjectionPlan,
) -> Result<ServiceStartPlan, String> {
    let mut instances = Vec::new();
    let mut managed = Vec::new();
    for (name, section) in &cake.sections {
        if section.section_type != "cake_autorate" || !bool_option(section, "enabled", false) {
            continue;
        }
        if projection.conflicts().contains(name) {
            eprintln!("WARNING: not starting {name}: duplicate managed SQM target");
            continue;
        }
        if !bool_option(section, "manage_sqm", true) {
            push_instance(&mut instances, name)?;
            continue;
        }
        if !bool_option(section, "sqm_enabled", false) {
            eprintln!("WARNING: not starting {name}: managed SQM is disabled");
            continue;
        }
        let direction = option(section, "sqm_direction_mode")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "both".to_string());
        if direction == "off" {
            eprintln!("WARNING: not starting {name}: managed SQM direction is off");
            continue;
        }
        if !matches!(direction.as_str(), "both" | "download_only" | "upload_only") {
            return Err(format!(
                "service instance {name} has an invalid SQM direction"
            ));
        }
        let target_name = first_option(section, &["sqm_interface", "ul_if", "wan_if"])
            .ok_or_else(|| format!("service instance {name} has no SQM target"))?;
        let target = resolver.resolve(&target_name)?;
        let configured_queue = option(section, "sqm_section").filter(|value| !value.is_empty());
        let Some((queue_name, queue)) =
            find_backing_queue(sqm, configured_queue.as_deref(), &target, resolver)?
        else {
            eprintln!("WARNING: not starting {name}: no enabled SQM backing queue");
            continue;
        };
        if option(queue, "_cake_autorate_managed").as_deref() != Some(name.as_str()) {
            eprintln!("WARNING: not starting {name}: SQM owner is not exact");
            continue;
        }
        let download = parse_rate(queue, "download")?;
        let upload = parse_rate(queue, "upload")?;
        if (direction != "upload_only" && download == 0)
            || (direction == "upload_only" && download != 0)
            || (direction != "download_only" && upload == 0)
            || (direction == "download_only" && upload != 0)
        {
            return Err(format!(
                "service instance {name} SQM rates do not match its direction"
            ));
        }
        let download_interface = option(section, "dl_if")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("ifb4{target}"));
        if !safe_interface(&download_interface) {
            return Err(format!(
                "service instance {name} download interface is unsafe"
            ));
        }
        managed.push(ManagedSqmAttestationSpec {
            instance: name.clone(),
            sqm_section: queue_name,
            target_interface: target.clone(),
            upload_interface: target,
            download_interface,
            direction_mode: direction,
            minimum_download_kbps: download,
            maximum_download_kbps: download,
            minimum_upload_kbps: upload,
            maximum_upload_kbps: upload,
        });
        push_instance(&mut instances, name)?;
    }
    Ok(ServiceStartPlan { instances, managed })
}

fn plan_stop(
    cake: &UciPackage,
    sqm: &UciPackage,
    resolver: &impl InterfaceResolver,
) -> Result<ServiceStopPlan, String> {
    let mut managed = Vec::new();
    let mut targets = BTreeMap::new();
    for (section_name, queue) in &sqm.sections {
        if queue.section_type != "queue" {
            continue;
        }
        let Some(owner) = option(queue, "_cake_autorate_managed").filter(|value| !value.is_empty())
        else {
            continue;
        };
        if !safe_name(&owner) || !safe_name(section_name) {
            return Err("managed SQM stop ownership is unsafe".to_string());
        }
        let target_name = option(queue, "interface")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| format!("managed SQM section {section_name} has no target"))?;
        let target = resolver.resolve(&target_name)?;
        if !safe_interface(&target) {
            return Err(format!(
                "managed SQM section {section_name} resolved to an unsafe target"
            ));
        }
        if let Some(previous) = targets.insert(target.clone(), section_name.clone()) {
            return Err(format!(
                "managed SQM sections {previous} and {section_name} share stop target {target}"
            ));
        }
        let controller = match cake.sections.get(&owner) {
            Some(section) if section.section_type != "cake_autorate" => {
                return Err(format!(
                    "managed SQM owner {owner} is not a cake_autorate section"
                ));
            }
            value => value,
        };
        let rate_policy = controller
            .map(|section| managed_sqm_rate_policy_from_options(&section.options))
            .transpose()
            .map_err(|error| {
                format!(
                    "managed SQM owner {owner} has an invalid rate policy: {}",
                    sqm_error_message(&error)
                )
            })?;
        let download_interface = controller
            .and_then(|section| option(section, "dl_if"))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("ifb4{target}"));
        if !safe_interface(&download_interface) {
            return Err(format!(
                "managed SQM section {section_name} has an unsafe download interface"
            ));
        }
        if managed.len() >= MAX_INSTANCES {
            return Err("service lifecycle exceeds its managed-SQM stop bound".to_string());
        }
        managed.push(ManagedSqmStopSpec {
            instance: owner,
            sqm_section: section_name.clone(),
            target_interface: target,
            download_interface,
            rate_policy,
        });
    }
    Ok(ServiceStopPlan {
        cake: cake.clone(),
        sqm: sqm.clone(),
        managed,
    })
}

fn attest_stop_configuration(
    environment: &OpenWrtEnvironment,
    committed: &CommittedSnapshot,
    expected: &ServiceStopPlan,
) -> Result<(), String> {
    committed.attest()?;
    let cake = committed.package(CAKE_PACKAGE)?;
    let sqm = committed.package(SQM_PACKAGE)?;
    if *cake != expected.cake || *sqm != expected.sqm {
        return Err("service lifecycle configuration changed during stop".to_string());
    }
    if plan_stop(cake, sqm, environment)? != *expected {
        return Err("service lifecycle stop plan changed during mutation".to_string());
    }
    committed.attest()
}

fn stop_plan_with_applied(
    committed: &CommittedSnapshot,
    applied: &BTreeMap<String, super::controller_input::Loaded>,
    resolver: &impl InterfaceResolver,
) -> Result<ServiceStopPlan, String> {
    committed.attest()?;
    stop_plan_from_views(
        committed.package(CAKE_PACKAGE)?,
        committed.package(SQM_PACKAGE)?,
        applied,
        resolver,
    )
}
fn stop_plan_from_views(
    current_cake: &UciPackage,
    current_sqm: &UciPackage,
    applied: &BTreeMap<String, super::controller_input::Loaded>,
    resolver: &impl InterfaceResolver,
) -> Result<ServiceStopPlan, String> {
    let mut cake = current_cake.clone();
    let mut sqm = current_sqm.clone();
    let mut old_sqm = UciPackage::default();
    let mut physical = BTreeSet::new();
    for (instance, input) in applied {
        input.guard.attest_record()?;
        let old_cake = UciPackage::parse(
            CAKE_PACKAGE,
            std::str::from_utf8(&input.cake_show).map_err(|_| "applied-stop-input-invalid")?,
        )?;
        let old = UciPackage::parse(
            SQM_PACKAGE,
            std::str::from_utf8(&input.sqm_show).map_err(|_| "applied-stop-input-invalid")?,
        )?;
        cake.sections.insert(
            instance.clone(),
            old_cake
                .sections
                .get(instance)
                .ok_or("applied-stop-input-invalid")?
                .clone(),
        );
        for (name, section) in old.sections {
            if let Some(interface) = section.options.get("interface") {
                physical.insert(interface.clone());
            }
            if old_sqm.sections.insert(name, section).is_some() {
                return Err("applied-stop-queue-ownership-conflict".into());
            }
        }
    }
    let mut remove = Vec::new();
    for (name, section) in &sqm.sections {
        let owner = section.options.get("_cake_autorate_managed");
        let old_name = old_sqm.sections.contains_key(name);
        let old_target = section
            .options
            .get("interface")
            .is_some_and(|v| physical.contains(v));
        if owner.is_some_and(|v| applied.contains_key(v)) {
            remove.push(name.clone());
        } else if (old_name || old_target) && owner.is_some_and(|v| !v.is_empty()) {
            return Err("applied-stop-queue-ownership-conflict".into());
        }
    }
    for name in remove {
        sqm.sections.remove(&name);
    }
    for (name, section) in old_sqm.sections {
        sqm.sections.insert(name, section);
    }
    struct Pinned<'a, R> {
        resolver: &'a R,
        physical: BTreeSet<String>,
    }
    impl<R: InterfaceResolver> InterfaceResolver for Pinned<'_, R> {
        fn resolve(&self, name: &str) -> Result<String, String> {
            if self.physical.contains(name) {
                Ok(name.into())
            } else {
                self.resolver.resolve(name)
            }
        }
    }
    plan_stop(&cake, &sqm, &Pinned { resolver, physical })
}

fn find_backing_queue<'a>(
    sqm: &'a UciPackage,
    configured: Option<&str>,
    expected_target: &str,
    resolver: &impl InterfaceResolver,
) -> Result<Option<(String, &'a UciSection)>, String> {
    if let Some(name) = configured {
        let Some(queue) = sqm.sections.get(name) else {
            return Ok(None);
        };
        return Ok(
            queue_matches(queue, expected_target, resolver)?.then(|| (name.to_string(), queue))
        );
    }
    let mut match_found = None;
    for (name, queue) in &sqm.sections {
        if !queue_matches(queue, expected_target, resolver)? {
            continue;
        }
        if match_found.replace((name.clone(), queue)).is_some() {
            return Err(format!(
                "multiple enabled SQM queues match managed target {expected_target}"
            ));
        }
    }
    Ok(match_found)
}

fn queue_matches(
    queue: &UciSection,
    expected_target: &str,
    resolver: &impl InterfaceResolver,
) -> Result<bool, String> {
    if queue.section_type != "queue" || option(queue, "enabled").as_deref() != Some("1") {
        return Ok(false);
    }
    let Some(interface) = option(queue, "interface").filter(|value| !value.is_empty()) else {
        return Ok(false);
    };
    Ok(resolver.resolve(&interface)? == expected_target)
}

fn push_instance(instances: &mut Vec<String>, name: &str) -> Result<(), String> {
    if instances.len() >= MAX_INSTANCES {
        return Err("service lifecycle exceeds its instance-count bound".to_string());
    }
    if instances.iter().any(|value| value == name) {
        return Err("service lifecycle produced a duplicate instance".to_string());
    }
    instances.push(name.to_string());
    Ok(())
}

#[cfg(test)]
fn encode_start_plan(instances: &[String], mqtt_instances: &[String]) -> String {
    let controllers = if instances.is_empty() {
        "-".to_string()
    } else {
        instances.join(",")
    };
    #[cfg(feature = "calibration")]
    {
        let mqtt = if mqtt_instances.is_empty() {
            "-".to_string()
        } else {
            mqtt_instances.join(",")
        };
        format!("service-start-v2 {controllers} {mqtt}\n")
    }
    #[cfg(not(feature = "calibration"))]
    {
        let _ = mqtt_instances;
        format!("service-start-v1 {controllers}\n")
    }
}

/// Versioned mechanical bridge to init; only public non-secret generation IDs
/// cross stdout. Both variants use the same three-field v3 framing.
fn encode_generation_start_plan(
    generations: &BTreeMap<String, String>,
    mqtt_instances: &[String],
) -> Result<String, String> {
    if generations.len() > MAX_INSTANCES
        || generations.iter().any(|(instance, id)| {
            !safe_name(instance)
                || id.len() != 64
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        || mqtt_instances.len() > MAX_INSTANCES
        || mqtt_instances.iter().any(|name| !safe_name(name))
        || mqtt_instances.iter().collect::<BTreeSet<_>>().len() != mqtt_instances.len()
    {
        return Err("service-start-generation-response-invalid".into());
    }
    #[cfg(not(feature = "calibration"))]
    if !mqtt_instances.is_empty() {
        return Err("service-start-generation-response-invalid".into());
    }
    let controllers = if generations.is_empty() {
        "-".to_string()
    } else {
        generations
            .iter()
            .map(|(name, id)| format!("{name}:{id}"))
            .collect::<Vec<_>>()
            .join(",")
    };
    let mqtt = if mqtt_instances.is_empty() {
        "-".into()
    } else {
        mqtt_instances.join(",")
    };
    Ok(format!("service-start-v3 {controllers} {mqtt}\n"))
}

fn sync_bridger_blacklist(
    paths: &ServicePaths,
    interfaces: &BTreeSet<String>,
) -> Result<(), String> {
    if interfaces.is_empty() || !executable(&paths.bridger_init) {
        return Ok(());
    }
    let Some(original) = read_exact_file(&paths.bridger_config)? else {
        return Ok(());
    };
    let workspace = BridgerWorkspace::create(&paths.uci_workspace_root)?;
    write_new_file(
        &workspace.package_path(),
        &original.bytes,
        0o600,
        unsafe { libc::geteuid() },
        unsafe { libc::getegid() },
    )?;
    let defaults_target = format!("{}.@defaults[0]", workspace.alias);
    let defaults = command_owned(
        &paths.uci,
        workspace.arguments([
            OsString::from("get"),
            OsString::from(defaults_target.clone()),
        ]),
        None,
    )?;
    if !defaults.status.success() {
        return Ok(());
    }
    let blacklist_target = format!("{}.@defaults[0].blacklist", workspace.alias);
    let current_output = command_owned(
        &paths.uci,
        workspace.arguments([
            OsString::from("get"),
            OsString::from(blacklist_target.clone()),
        ]),
        None,
    )?;
    let current_text = if current_output.status.success() {
        String::from_utf8(current_output.stdout)
            .map_err(|_| "bridger blacklist output is not UTF-8".to_string())?
    } else {
        String::new()
    };
    let current = current_text
        .split_ascii_whitespace()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    if current.iter().any(|value| !safe_interface(value)) {
        return Err("bridger blacklist contains an unsafe interface".to_string());
    }
    let missing = interfaces.difference(&current).cloned().collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    let batch = bridger_batch(&workspace.alias, &missing);
    let applied = command_owned(
        &paths.uci,
        workspace.arguments([OsString::from("batch")]),
        Some(batch.as_bytes()),
    )?;
    require_success(&applied, "update the bridger blacklist")?;
    let final_output = command_owned(
        &paths.uci,
        workspace.arguments([OsString::from("get"), OsString::from(blacklist_target)]),
        None,
    )?;
    require_success(&final_output, "verify the bridger blacklist")?;
    let final_text = String::from_utf8(final_output.stdout)
        .map_err(|_| "bridger blacklist verification is not UTF-8".to_string())?;
    let final_values = final_text.split_ascii_whitespace().collect::<BTreeSet<_>>();
    if !interfaces
        .iter()
        .all(|value| final_values.contains(value.as_str()))
    {
        return Err("bridger blacklist update did not reach its exact postcondition".to_string());
    }
    let candidate = fs::read(workspace.package_path())
        .map_err(|error| format!("unable to read bridger candidate: {error}"))?;
    if candidate.len() > MAX_BRIDGER_CONFIG_BYTES {
        return Err("bridger candidate exceeds its byte bound".to_string());
    }
    replace_exact_file(&paths.bridger_config, &original, &candidate)?;
    let reload = command(paths, &paths.bridger_init, &["reload"], None)?;
    require_success(
        &reload,
        "reload bridger after its isolated configuration update",
    )
}

fn bridger_batch(alias: &str, missing: &[String]) -> String {
    let mut batch = String::new();
    for interface in missing {
        batch.push_str("add_list ");
        batch.push_str(alias);
        batch.push_str(".@defaults[0].blacklist='");
        batch.push_str(interface);
        batch.push_str("'\n");
    }
    batch.push_str("commit ");
    batch.push_str(alias);
    batch.push('\n');
    batch
}

fn remove_empty_clsact(paths: &ServicePaths, interface: &str) -> Result<(), String> {
    let qdiscs = command(paths, &paths.tc, &["qdisc", "show", "dev", interface], None)?;
    if !qdiscs.status.success() {
        return Ok(());
    }
    let qdiscs =
        String::from_utf8(qdiscs.stdout).map_err(|_| "tc qdisc output is not UTF-8".to_string())?;
    if !qdiscs.lines().any(|line| {
        let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
        fields.first() == Some(&"qdisc") && fields.get(1) == Some(&"clsact")
    }) {
        return Ok(());
    }
    for hook in ["ingress", "egress"] {
        let filters = command(
            paths,
            &paths.tc,
            &["filter", "show", "dev", interface, hook],
            None,
        )?;
        require_success(&filters, "inspect clsact filters")?;
        if !filters.stdout.iter().all(u8::is_ascii_whitespace) {
            return Err(format!(
                "cannot replace clsact on SQM interface {interface}: foreign filters remain"
            ));
        }
    }
    let removed = command(
        paths,
        &paths.tc,
        &["qdisc", "del", "dev", interface, "clsact"],
        None,
    )?;
    require_success(&removed, "remove an empty SQM clsact")
}

fn start_sqm_backend(
    paths: &ServicePaths,
    start: &StartPublication,
    profiles: Vec<super::sqm_runner::Profile>,
    resolver: &impl InterfaceResolver,
) -> Result<(), String> {
    let specs = &start.plan.managed;
    if profiles.len() != specs.len() {
        return Err("service-start-runner-membership-mismatch".into());
    }
    if specs.is_empty() {
        return start.attest(resolver);
    }
    let sqm_paths = OpenWrtPaths::from_environment();
    // Subscribe before the scoped service action sequence. Upstream iface
    // hotplug can independently stop/start SQM despite our global lock.
    let mut files = Vec::with_capacity(specs.len() + 2);
    files.push(paths.config_root.join(CAKE_PACKAGE));
    files.push(paths.config_root.join(SQM_PACKAGE));
    let mut interfaces = BTreeSet::new();
    for spec in specs {
        files.push(
            sqm_paths
                .sqm_state_root
                .join(format!("{}.state", spec.target_interface)),
        );
        interfaces.insert(spec.target_interface.clone());
        interfaces.insert(spec.upload_interface.clone());
        if spec.direction_mode != "upload_only" {
            interfaces.insert(spec.download_interface.clone());
        }
    }
    let mut events = SqmStartEvents::subscribe_owned(
        files,
        interfaces.into_iter().collect(),
        &sqm_paths.sys_class_net,
    )?;
    start.attest(resolver)?;
    for (spec, profile) in specs.iter().zip(profiles) {
        start.attest(resolver)?;
        super::sqm_recovery_openwrt::start_managed_sqm_from_published(
            spec,
            &start.published,
            profile,
            &paths.runtime_root,
            || false,
        )
        .map_err(|error| sqm_error_message(&error).to_string())?;
        start.attest(resolver)?;
    }
    let offline_instances = await_sqm_start(
        &mut events,
        Instant::now() + SQM_START_ATTEST_TIMEOUT,
        specs,
        || start.attest(resolver),
        |spec| {
            super::sqm_recovery_openwrt::attest_managed_sqm_from_published(spec, &start.published)
        },
    )?;
    for (spec, offline) in specs.iter().zip(offline_instances) {
        if offline {
            eprintln!(
                "WARNING: managed SQM start for {} is deferred until its target interface returns",
                spec.instance
            );
        }
    }
    Ok(())
}

pub(super) fn await_sqm_start(
    events: &mut impl StartEvents,
    deadline: Instant,
    specs: &[ManagedSqmAttestationSpec],
    mut attest_configuration: impl FnMut() -> Result<(), String>,
    mut attest_runtime: impl FnMut(
        &ManagedSqmAttestationSpec,
    ) -> Result<bool, NativeSqmAttestationError>,
) -> Result<Vec<bool>, String> {
    let mut last_error = "SQM topology kept changing during start attestation".to_string();
    loop {
        if Instant::now() >= deadline {
            return Err(last_error);
        }
        events.drain()?;
        attest_configuration()?;
        let mut offline_instances = Vec::with_capacity(specs.len());
        for spec in specs {
            match attest_runtime(spec) {
                Ok(offline) => offline_instances.push(offline),
                Err(error) => {
                    last_error = format!(
                        "managed SQM did not reach its exact start postcondition for {}: {}",
                        spec.instance,
                        sqm_error_message(&error)
                    );
                    // No string classification and no mutation on a failed
                    // observation. Busy/cancellation are terminal. A failed
                    // topology observation can only be replaced by a fresh
                    // complete exact attestation after a real event.
                    if !matches!(error, NativeSqmAttestationError::Failed(_)) {
                        return Err(last_error);
                    }
                    break;
                }
            }
        }
        // Also check after failures: a changed user configuration must abort,
        // never be adopted as a new baseline while waiting for hotplug.
        attest_configuration()?;
        if Instant::now() >= deadline {
            return Err(last_error);
        }
        if events.drain()? {
            continue;
        }
        if offline_instances.len() == specs.len() {
            return Ok(offline_instances);
        }
        if !events.wait(deadline)? {
            return Err(last_error);
        }
    }
}

/// Owned SQM queue interface fields are pinned physical recipe targets. Never
/// reinterpret a removed target through today's logical network mapping.
fn recovery_targets(packages: &[UciPackage; 2]) -> Result<Vec<ManagedSqmStopSpec>, String> {
    struct Physical;
    impl InterfaceResolver for Physical {
        fn resolve(&self, name: &str) -> Result<String, String> {
            if safe_interface(name) {
                Ok(name.into())
            } else {
                Err("unregistered-start-target-invalid".into())
            }
        }
    }
    Ok(plan_stop(&packages[0], &packages[1], &Physical)?.managed)
}

fn recover_unregistered_publication(
    paths: &ServicePaths,
    mut attest_runtime_absent: impl FnMut(&[ManagedSqmStopSpec]) -> Result<(), String>,
) -> Result<Option<super::uci_transaction::SourceReceipt>, String> {
    use super::uci_transaction::RecoveryCheck;
    let input_root = paths.runtime_root.join(".controller-input");
    let Some(draft) = super::controller_input::unregistered_store(&input_root)? else {
        return Ok(None);
    };
    let pending = super::uci_transaction::recovery_pending(&paths.config_root)?;
    if !draft && !pending {
        return Ok(None);
    }
    let mut targets = Vec::new();
    // A no-op publication has no file journal. Its current committed source is
    // used only to fence draft cleanup, not as authority to restore other files.
    let committed = if !pending {
        let snapshot =
            CommittedSnapshot::capture(&paths.config_root, &paths.runtime_root, &paths.uci)?;
        targets = recovery_targets(&[
            snapshot.package(CAKE_PACKAGE)?.clone(),
            snapshot.package(SQM_PACKAGE)?.clone(),
        ])?;
        Some(snapshot)
    } else {
        None
    };
    let mut check = |event: RecoveryCheck<'_>| -> Result<(), String> {
        if super::controller_input::unregistered_store(&input_root)?.is_none() {
            return Err("unregistered-start-became-registered".into());
        }
        if let RecoveryCheck::Versions { before, after } = event {
            let before =
                CommittedSnapshot::parse_versions(before, &paths.runtime_root, &paths.uci)?;
            let after = CommittedSnapshot::parse_versions(after, &paths.runtime_root, &paths.uci)?;
            targets = recovery_targets(&before)?;
            targets.extend(recovery_targets(&after)?);
            if targets.len() > 2 * MAX_INSTANCES {
                return Err("unregistered-start-target-bound".into());
            }
        }
        if let Some(committed) = &committed {
            committed.attest()?;
        }
        attest_runtime_absent(&targets)
    };
    let restored = super::uci_transaction::recover_checked(&paths.config_root, None, &mut check)?;
    super::controller_input::discard_unpublished_batch(&input_root, || {
        check(RecoveryCheck::Absent)?;
        if let Some(restored) = &restored {
            restored.attest(&paths.config_root)?;
        }
        Ok(())
    })?;
    check(RecoveryCheck::Absent)?;
    if let Some(restored) = &restored {
        restored.attest(&paths.config_root)?;
    }
    Ok(restored)
}

/// Called only after the Stop runtime postconditions. Keep the durable batch
/// until its file transaction is resolved; a returned original receipt is the
/// authority for restored files, not permission to capture a newer baseline.
fn finish_stopped_publication(
    config_root: &Path,
    uci: &Path,
    committed: &CommittedSnapshot,
    batch: &super::controller_input::Batch,
    mut attest_absent: impl FnMut(&[ManagedSqmStopSpec]) -> Result<(), String>,
) -> Result<Option<super::uci_transaction::SourceReceipt>, String> {
    committed.attest()?;
    let expected = batch.ordinary_stop_receipt()?;
    let mut targets = Vec::new();
    let restored =
        super::uci_transaction::recover_checked(config_root, Some(&expected), |check| {
            batch.attest_record()?;
            if let super::uci_transaction::RecoveryCheck::Versions { before, after } = check {
                targets = recovery_targets(&committed.parse_retained_versions(before, uci)?)?;
                targets.extend(recovery_targets(
                    &committed.parse_retained_versions(after, uci)?,
                )?);
            }
            attest_absent(&targets)
        })?;
    let attest_source = || match &restored {
        Some(original) => original.attest(config_root),
        None => committed.attest(),
    };
    attest_source()?;
    batch.retire(|_| {
        attest_source()?;
        attest_absent(&targets)?;
        attest_source()
    })?;
    Ok(restored)
}

impl ServiceStopBackend for OpenWrtServiceStop {
    type Controllers = Vec<ControllerPidfd>;

    fn snapshot(&mut self) -> Result<ServiceStopPlan, String> {
        if self.committed.is_some() {
            return Err("service Stop snapshot was already captured".into());
        }
        let committed = CommittedSnapshot::capture(
            &self.paths.config_root,
            &self.paths.runtime_root,
            &self.paths.uci,
        )?;
        check_source_handoff(&committed, self.expected_source.as_deref())?;
        self.batch = super::controller_input::load_batch(
            &self.paths.runtime_root.join(".controller-input"),
            &self.paths.config_root,
        )?;
        if let Some(batch) = &self.batch {
            self.applied = batch.load_inputs()?;
        }
        for controller in discover_controllers(&self.paths.proc_root)? {
            if controller.kind != ServiceProcessKind::Controller {
                continue;
            }
            if let Some(id) = controller_generation(&self.paths.proc_root, &controller)? {
                if self
                    .batch
                    .as_ref()
                    .is_some_and(|batch| batch.generations().get(&controller.instance) != Some(&id))
                {
                    return Err("applied-stop-process-generation-mismatch".into());
                }
                let input = super::controller_input::load_for_recovery(
                    &controller.instance,
                    &id,
                    &self.paths.runtime_root.join(".controller-input"),
                    &self.paths.config_root,
                )?;
                if !controller_identity_matches(&self.paths.proc_root, &controller)? {
                    return Err("applied-stop-process-changed".into());
                }
                self.applied.insert(controller.instance, input);
            } else if self.batch.is_some() {
                return Err("applied-stop-process-generation-missing".into());
            }
        }
        let plan = stop_plan_with_applied(&committed, &self.applied, &self.environment)?;
        committed.attest()?;
        self.committed = Some(committed);
        Ok(plan)
    }

    fn attest_unchanged(&mut self, plan: &ServiceStopPlan) -> Result<(), String> {
        if let Some(batch) = &self.batch {
            batch.attest_record()?;
        }
        let committed = self
            .committed
            .as_ref()
            .ok_or("service Stop has no committed snapshot")?;
        if self.applied.is_empty() {
            return attest_stop_configuration(&self.environment, committed, plan);
        }
        if stop_plan_with_applied(committed, &self.applied, &self.environment)? != *plan {
            return Err("applied-stop-plan-changed".into());
        }
        committed.attest()
    }

    fn capture_controllers(&mut self) -> Result<Self::Controllers, String> {
        capture_controller_pidfds(&self.paths.proc_root, None)
    }

    fn retire_generations(&mut self, plan: &ServiceStopPlan) -> Result<(), String> {
        let Some(batch) = &self.batch else {
            if let Err(error) = collect_controller_inputs(&self.paths) {
                eprintln!("WARNING: controller input cleanup deferred: {error}");
            }
            return Ok(());
        };
        let committed = self
            .committed
            .as_ref()
            .ok_or("service Stop has no committed snapshot")?;
        let sqm_paths = OpenWrtPaths::from_environment();
        self.restored_source = finish_stopped_publication(
            &self.paths.config_root,
            &self.paths.uci,
            committed,
            batch,
            |extra_targets| {
                if !discover_controllers(&self.paths.proc_root)?.is_empty() {
                    return Err("applied-stop-process-still-running".into());
                }
                if !super::procd_control::service_absent(&self.paths.ubus)? {
                    return Err("applied-stop-procd-still-registered".into());
                }
                if !live_input_references(&self.paths)?.is_empty() {
                    return Err("applied-stop-input-still-referenced".into());
                }
                super::sqm_runner::ensure_idle(&self.paths.runtime_root)?;
                for spec in plan.managed.iter().chain(extra_targets) {
                    super::sqm_recovery_openwrt::attest_managed_sqm_absent(spec, &sqm_paths)
                        .map_err(|error| sqm_error_message(&error).to_string())?;
                }
                Ok(())
            },
        )?;
        self.batch = None;
        if let Err(error) = collect_controller_inputs(&self.paths) {
            eprintln!("WARNING: controller input cleanup deferred: {error}");
        }
        Ok(())
    }

    fn disable_service(&mut self) -> Result<(), String> {
        let request = format!("{{\"name\":\"{SERVICE_NAME}\"}}");
        delete_service_or_attest_absent(&self.paths.ubus, &request, "delete the procd service")
    }

    fn wait_controllers(&mut self, controllers: Self::Controllers) -> Result<(), String> {
        wait_controller_pidfds(controllers, CONTROLLER_STOP_TIMEOUT)
    }

    fn attest_no_controllers(&mut self) -> Result<(), String> {
        let controllers = discover_controllers(&self.paths.proc_root)?;
        if controllers.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "service stop left exact managed processes: {}",
                controllers
                    .iter()
                    .map(|value| format!("{}:{}", value.kind.label(), value.instance))
                    .collect::<Vec<_>>()
                    .join(",")
            ))
        }
    }

    fn clear_classifier(&mut self) {
        #[cfg(feature = "calibration")]
        if let Err(error) = run_traffic_classifier(["clear".to_string()].into_iter()) {
            eprintln!(
                "WARNING: unable to clear the native traffic classifier during stop: {error}"
            );
        }
    }

    fn stop_sqm(&mut self, spec: &ManagedSqmStopSpec) -> Result<(), String> {
        if let Some(input) = self.applied.get(&spec.instance) {
            return super::sqm_recovery_openwrt::stop_managed_sqm_from_input(spec, input).map_err(
                |error| {
                    format!(
                        "unable to stop applied SQM section {}: {}",
                        spec.sqm_section,
                        sqm_error_message(&error)
                    )
                },
            );
        }
        stop_managed_sqm_from_committed(
            spec,
            self.committed
                .as_ref()
                .ok_or("service Stop has no committed snapshot")?,
        )
        .map_err(|error| {
            format!(
                "unable to stop managed SQM section {}: {}",
                spec.sqm_section,
                sqm_error_message(&error)
            )
        })
    }

    fn cleanup_runtime(&mut self) -> Result<(), String> {
        cleanup_runtime_files(&self.paths)
    }

    fn cleanup_sidecars(&mut self) -> Result<(), String> {
        #[cfg(feature = "calibration")]
        cleanup_production_service_plans()?;
        Ok(())
    }
}

fn acquire_service_global_lock(paths: &ServicePaths) -> Result<ServiceGlobalLock, String> {
    ensure_owner_directory(&paths.runtime_lock_root)?;
    let guard_path = paths.runtime_lock_root.join("runtime.guard");
    if std::env::var_os("CAKE_AUTORATE_SERVICE_LOCK_BORROW").is_some_and(|value| value == "1") {
        let fd = std::env::var("CAKE_AUTORATE_RUNTIME_GLOBAL_LOCK_FD")
            .ok()
            .and_then(|value| value.parse::<i32>().ok())
            .filter(|value| *value == 8)
            .ok_or_else(|| "borrowed service lifecycle lock descriptor is invalid".to_string())?;
        let raw = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        if raw < 0 {
            return Err(format!(
                "unable to duplicate borrowed service lifecycle lock: {}",
                io::Error::last_os_error()
            ));
        }
        let borrowed = unsafe { File::from_raw_fd(raw) };
        attest_borrowed_exclusive_lock(&guard_path, &borrowed)?;
        return Ok(ServiceGlobalLock::Borrowed { _guard: borrowed });
    }

    let guard = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&guard_path)
        .map_err(|error| format!("unable to open service lifecycle lock: {error}"))?;
    attest_guard_identity(&guard_path, &guard)?;
    if unsafe { libc::flock(guard.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let error = io::Error::last_os_error();
        return Err(if error.raw_os_error() == Some(libc::EWOULDBLOCK) {
            "service lifecycle is busy with another runtime mutation".to_string()
        } else {
            format!("unable to acquire service lifecycle lock: {error}")
        });
    }
    Ok(ServiceGlobalLock::Owned(guard))
}

fn attest_borrowed_exclusive_lock(path: &Path, borrowed: &File) -> Result<(), String> {
    attest_guard_identity(path, borrowed)?;
    // flock() is attached to the open file description. Re-locking a dup of
    // fd 8 would therefore always succeed and could even acquire or upgrade a
    // missing/shared lock. Probe through a separately opened description
    // instead: a real exclusive owner must reject even a shared nonblocking
    // lock with EWOULDBLOCK. This leaves the borrowed OFD untouched.
    let probe = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("unable to open service lifecycle lock probe: {error}"))?;
    attest_guard_identity(path, &probe)?;
    if unsafe { libc::flock(probe.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) } == 0 {
        unsafe {
            libc::flock(probe.as_raw_fd(), libc::LOCK_UN);
        }
        return Err("borrowed service lifecycle descriptor has no exclusive lock".to_string());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() != Some(libc::EWOULDBLOCK) {
        return Err(format!(
            "unable to attest borrowed service lifecycle lock: {error}"
        ));
    }
    Ok(())
}

fn attest_guard_identity(path: &Path, opened: &File) -> Result<(), String> {
    let current = fs::symlink_metadata(path)
        .map_err(|error| format!("unable to inspect service lifecycle lock: {error}"))?;
    let metadata = opened
        .metadata()
        .map_err(|error| format!("unable to inspect opened service lifecycle lock: {error}"))?;
    if current.file_type().is_symlink()
        || !current.file_type().is_file()
        || current.dev() != metadata.dev()
        || current.ino() != metadata.ino()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.mode() & 0o777 != 0o600
    {
        return Err("service lifecycle lock identity is unsafe".to_string());
    }
    Ok(())
}

fn capture_controller_pidfds(
    proc_root: &Path,
    selected: Option<(ServiceProcessKind, &str)>,
) -> Result<Vec<ControllerPidfd>, String> {
    let controllers = discover_controllers(proc_root)?;
    let mut result = Vec::with_capacity(controllers.len());
    for identity in controllers {
        if selected.is_some_and(|(kind, name)| identity.kind != kind || identity.instance != name) {
            continue;
        }
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, identity.pid, 0) };
        if raw < 0 {
            return Err(format!(
                "unable to open controller pidfd: {}",
                io::Error::last_os_error()
            ));
        }
        let pidfd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
        if !controller_identity_matches(proc_root, &identity)? {
            return Err(format!(
                "{} {} changed while opening pidfd",
                identity.kind.label(),
                identity.instance,
            ));
        }
        result.push(ControllerPidfd { identity, pidfd });
    }
    Ok(result)
}

fn wait_controller_pidfds(
    controllers: Vec<ControllerPidfd>,
    timeout: Duration,
) -> Result<(), String> {
    if controllers.is_empty() {
        return Ok(());
    }
    let started = Instant::now();
    let mut waiting = controllers;
    loop {
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(format!(
                "managed processes did not stop before their watchdog deadline: {}",
                waiting
                    .iter()
                    .map(|value| {
                        format!(
                            "{}:{}",
                            value.identity.kind.label(),
                            value.identity.instance
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            ));
        }
        let mut pollfds = waiting
            .iter()
            .map(|value| libc::pollfd {
                fd: value.pidfd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            })
            .collect::<Vec<_>>();
        let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
        let result = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as _, timeout_ms) };
        if result == 0 {
            return Err(
                "managed processes did not stop before their watchdog deadline".to_string(),
            );
        }
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("unable to wait for controller exits: {error}"));
        }
        let mut index = 0usize;
        waiting.retain(|_| {
            let revents = pollfds[index].revents;
            index += 1;
            revents & (libc::POLLIN | libc::POLLHUP) == 0
        });
        if waiting.is_empty() {
            return Ok(());
        }
    }
}

fn discover_controllers(proc_root: &Path) -> Result<Vec<ControllerIdentity>, String> {
    discover_controllers_with_policy(proc_root, true)
}

fn discover_controllers_allowing_duplicates(
    proc_root: &Path,
) -> Result<Vec<ControllerIdentity>, String> {
    discover_controllers_with_policy(proc_root, false)
}

fn discover_controllers_with_policy(
    proc_root: &Path,
    reject_duplicates: bool,
) -> Result<Vec<ControllerIdentity>, String> {
    let mut controllers = Vec::new();
    let mut instances = BTreeSet::new();
    for entry in fs::read_dir(proc_root)
        .map_err(|error| format!("unable to enumerate controller processes: {error}"))?
    {
        let entry = entry.map_err(|error| format!("unable to inspect process entry: {error}"))?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        if pid <= 1 {
            continue;
        }
        let Some((kind, instance)) = controller_instance(&entry.path().join("cmdline"))? else {
            continue;
        };
        let mut identity =
            match inspect_controller_identity(&entry.path().join("stat"), pid, kind, &instance) {
                Ok(identity) => identity,
                Err(error) if error == "controller process is a zombie" => continue,
                Err(error) => return Err(error),
            };
        if identity.process_group == 0 || identity.starttime_ticks == 0 {
            return Err("controller process identity contains a zero field".to_string());
        }
        if !instances.insert((kind, instance.clone())) && reject_duplicates {
            return Err(format!(
                "multiple exact {} processes are running for {instance}",
                kind.label()
            ));
        }
        identity.kind = kind;
        identity.instance = instance;
        controllers.push(identity);
    }
    controllers.sort_by(|left, right| {
        (left.kind, left.instance.as_str()).cmp(&(right.kind, right.instance.as_str()))
    });
    Ok(controllers)
}

fn controller_instance(path: &Path) -> Result<Option<(ServiceProcessKind, String)>, String> {
    let Some(bytes) = super::identity::read_process_cmdline(path, MAX_CMDLINE + 1)
        .map_err(|error| format!("unable to read controller command line: {error}"))?
    else {
        return Ok(None);
    };
    let controller = format!(
        "{DAEMON_PATH}\0{}\0",
        ServiceProcessKind::Controller.argument()
    );
    let (kind, prefix) = if bytes.starts_with(controller.as_bytes()) {
        (ServiceProcessKind::Controller, controller.len())
    } else {
        #[cfg(feature = "calibration")]
        {
            let mqtt = format!(
                "{DAEMON_PATH}\0{}\0",
                ServiceProcessKind::MqttPublisher.argument()
            );
            if bytes.starts_with(mqtt.as_bytes()) {
                (ServiceProcessKind::MqttPublisher, mqtt.len())
            } else {
                return Ok(None);
            }
        }
        #[cfg(not(feature = "calibration"))]
        {
            return Ok(None);
        }
    };
    if bytes.len() > MAX_CMDLINE as usize {
        return Err("controller command line exceeds its safety bound".to_string());
    }
    parse_service_process_instance(&bytes, kind, prefix)
}

fn parse_service_process_instance(
    bytes: &[u8],
    kind: ServiceProcessKind,
    prefix: usize,
) -> Result<Option<(ServiceProcessKind, String)>, String> {
    let Some(instance_bytes) = bytes[prefix..].strip_suffix(&[0]) else {
        return Ok(None);
    };
    if instance_bytes.is_empty() || instance_bytes.contains(&0) {
        return Ok(None);
    }
    let instance = std::str::from_utf8(instance_bytes)
        .map_err(|_| "controller instance is not UTF-8".to_string())?;
    if !safe_name(instance) {
        return Err("controller instance is unsafe".to_string());
    }
    Ok(Some((kind, instance.to_string())))
}

fn inspect_controller_identity(
    path: &Path,
    pid: u32,
    kind: ServiceProcessKind,
    instance: &str,
) -> Result<ControllerIdentity, String> {
    let stat = match fs::read_to_string(path) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err("controller disappeared during process snapshot".to_string())
        }
        Err(error) => {
            return Err(format!(
                "unable to read controller process identity: {error}"
            ))
        }
    };
    let close = stat
        .rfind(')')
        .ok_or_else(|| "controller process identity is malformed".to_string())?;
    if !stat[..=close].starts_with(&format!("{pid} (")) {
        return Err("controller process PID identity changed".to_string());
    }
    let fields = stat[close + 1..]
        .split_ascii_whitespace()
        .collect::<Vec<_>>();
    if fields.len() <= 19 || fields[0].as_bytes().len() != 1 {
        return Err("controller process identity has too few fields".to_string());
    }
    if fields[0] == "Z" {
        return Err("controller process is a zombie".to_string());
    }
    let process_group = fields[2]
        .parse::<u32>()
        .map_err(|_| "controller process group is invalid".to_string())?;
    let starttime_ticks = fields[19]
        .parse::<u64>()
        .map_err(|_| "controller process start time is invalid".to_string())?;
    Ok(ControllerIdentity {
        kind,
        instance: instance.to_string(),
        pid,
        process_group,
        starttime_ticks,
    })
}

fn controller_identity_matches(
    proc_root: &Path,
    expected: &ControllerIdentity,
) -> Result<bool, String> {
    let root = proc_root.join(expected.pid.to_string());
    if controller_instance(&root.join("cmdline"))?
        != Some((expected.kind, expected.instance.clone()))
    {
        return Ok(false);
    }
    match inspect_controller_identity(
        &root.join("stat"),
        expected.pid,
        expected.kind,
        &expected.instance,
    ) {
        Ok(actual) => Ok(actual == *expected),
        Err(error) if error.contains("disappeared") || error.contains("zombie") => Ok(false),
        Err(error) => Err(error),
    }
}

fn controller_generation(
    proc_root: &Path,
    controller: &ControllerIdentity,
) -> Result<Option<String>, String> {
    read_process_generation(proc_root, controller.pid, || {
        controller_identity_matches(proc_root, controller)
    })
}

fn read_process_generation(
    proc_root: &Path,
    pid: u32,
    mut identity_matches: impl FnMut() -> Result<bool, String>,
) -> Result<Option<String>, String> {
    if !identity_matches()? {
        return Err("controller-generation-process-changed".into());
    }
    let path = proc_root.join(pid.to_string()).join("environ");
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if !identity_matches()? {
                return Err("controller-generation-process-changed".into());
            }
            return Ok(None);
        }
        Err(_) => return Err("controller-generation-environment-unavailable".into()),
    };
    let metadata = file
        .metadata()
        .map_err(|_| "controller-generation-environment-unavailable")?;
    if !metadata.is_file()
        // SAFETY: geteuid has no pointer arguments and cannot fail; it supplies
        // the OS credential used to reject another user's proc environment.
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o022 != 0
    {
        return Err("controller-generation-environment-invalid".into());
    }
    let mut bytes = Vec::with_capacity(64 * 1024 + 1);
    Read::by_ref(&mut file)
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "controller-generation-environment-read-failed")?;
    let result = parse_controller_generation(&bytes)?;
    if !identity_matches()? {
        return Err("controller-generation-process-changed".into());
    }
    Ok(result)
}

fn live_input_references(paths: &ServicePaths) -> Result<BTreeSet<String>, String> {
    let mut references = super::procd_control::generation_references(&paths.ubus)?;
    for entry in
        fs::read_dir(&paths.proc_root).map_err(|_| "input-gc-process-enumeration-failed")?
    {
        let entry = entry.map_err(|_| "input-gc-process-enumeration-failed")?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
            .filter(|pid| *pid > 1)
        else {
            continue;
        };
        let command_path = entry.path().join("cmdline");
        let Some(command) = read_exact_file(&command_path)? else {
            continue;
        };
        if command.bytes.split(|b| *b == 0).next() != Some(DAEMON_PATH.as_bytes()) {
            continue;
        }
        // Any mode of our daemon can carry the internal input environment;
        // don't restrict GC roots to the ordinary three-argument invocation.
        let Some(identity) = ProcessIdentity::inspect_live(&paths.proc_root, pid)? else {
            continue;
        };
        if let Some(id) = read_process_generation(&paths.proc_root, pid, || {
            Ok(
                ProcessIdentity::inspect_live(&paths.proc_root, pid)?.as_ref() == Some(&identity)
                    && read_exact_file(&command_path)?
                        .is_some_and(|current| current.bytes == command.bytes),
            )
        })? {
            references.insert(id);
        }
    }
    references.extend(super::procd_control::generation_references(&paths.ubus)?);
    Ok(references)
}

/// Called only under the lifecycle lock after successful acceptance or Stop.
/// GC failure retains records and is maintenance degradation, not failed Start.
fn collect_controller_inputs(paths: &ServicePaths) -> Result<usize, String> {
    let root = paths.runtime_root.join(".controller-input");
    super::controller_input::collect_unreferenced(&root, &paths.config_root, |input| {
        Ok(!live_input_references(paths)?.contains(input.guard.generation()))
    })
}

fn parse_controller_generation(bytes: &[u8]) -> Result<Option<String>, String> {
    if bytes.len() > 64 * 1024 || (!bytes.is_empty() && bytes.last() != Some(&0)) {
        return Err("controller-generation-environment-invalid".into());
    }
    let prefix = format!("{}=", super::controller_input::GENERATION_ENV);
    let mut generation = None;
    for entry in bytes.split(|b| *b == 0) {
        let Some(id) = entry.strip_prefix(prefix.as_bytes()) else {
            continue;
        };
        if generation.is_some()
            || id.len() != 64
            || !id
                .iter()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
        {
            return Err("controller-generation-environment-invalid".into());
        }
        generation = Some(
            String::from_utf8(id.to_vec())
                .map_err(|_| "controller-generation-environment-invalid")?,
        );
    }
    Ok(generation)
}

fn cleanup_runtime_files(paths: &ServicePaths) -> Result<(), String> {
    let entries = match fs::read_dir(&paths.runtime_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("unable to enumerate runtime directories: {error}")),
    };
    for entry in entries {
        let entry = entry.map_err(|error| format!("unable to inspect runtime entry: {error}"))?;
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| format!("unable to inspect runtime directory: {error}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            continue;
        }
        let Some(instance) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if !safe_name(&instance) {
            continue;
        }
        if discover_controllers(&paths.proc_root)?
            .iter()
            .any(|controller| {
                controller.kind == ServiceProcessKind::Controller && controller.instance == instance
            })
        {
            eprintln!("WARNING: preserving runtime files for active replacement {instance}");
            continue;
        }
        for name in ["status.json", "history.csv", "history.csv.tmp"] {
            let path = entry.path().join(name);
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("unable to inspect runtime file: {error}")),
            };
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.nlink() != 1
            {
                return Err(format!("runtime file {} is unsafe", path.display()));
            }
            if discover_controllers(&paths.proc_root)?
                .iter()
                .any(|controller| {
                    controller.kind == ServiceProcessKind::Controller
                        && controller.instance == instance
                })
            {
                eprintln!("WARNING: preserving runtime files for replacement {instance}");
                break;
            }
            fs::remove_file(&path).map_err(|error| {
                format!("unable to remove runtime file {}: {error}", path.display())
            })?;
        }
    }
    Ok(())
}

fn command(
    _paths: &ServicePaths,
    program: &Path,
    arguments: &[&str],
    input: Option<&[u8]>,
) -> Result<BoundedCommandOutput, String> {
    command_owned(
        program,
        arguments.iter().map(OsString::from).collect(),
        input,
    )
}

fn command_owned(
    program: &Path,
    arguments: Vec<OsString>,
    input: Option<&[u8]>,
) -> Result<BoundedCommandOutput, String> {
    run_bounded_command_output_with_input(
        &SpawnSpec {
            program: program.to_path_buf(),
            arguments,
            environment: Vec::new(),
        },
        input,
        COMMAND_TIMEOUT,
        MAX_OUTPUT,
        || false,
        |_| {},
    )
}

fn ensure_owner_directory(path: &Path) -> Result<(), String> {
    match fs::create_dir(path) {
        Ok(()) => fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("unable to secure service owner directory: {error}"))?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!("unable to create service owner directory: {error}"));
        }
    }
    if owner_directory_is_exact(path) {
        Ok(())
    } else {
        Err("service owner directory is not private".to_string())
    }
}

fn owner_directory_is_exact(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.file_type().is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o7777 == 0o700
    })
}

fn read_exact_file(path: &Path) -> Result<Option<ExactFileSnapshot>, String> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("unable to open {}: {error}", path.display())),
    };
    let before = file
        .metadata()
        .map_err(|error| format!("unable to inspect {}: {error}", path.display()))?;
    if !before.file_type().is_file() || before.nlink() != 1 {
        return Err(format!(
            "{} is not an exclusive regular configuration file",
            path.display()
        ));
    }
    if before.len() > MAX_BRIDGER_CONFIG_BYTES as u64 {
        return Err(format!("{} exceeds its byte bound", path.display()));
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_BRIDGER_CONFIG_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("unable to read {}: {error}", path.display()))?;
    if bytes.len() > MAX_BRIDGER_CONFIG_BYTES {
        return Err(format!("{} exceeds its byte bound", path.display()));
    }
    let after = file
        .metadata()
        .map_err(|error| format!("unable to reinspect {}: {error}", path.display()))?;
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
    {
        return Err(format!("{} changed while it was read", path.display()));
    }
    Ok(Some(ExactFileSnapshot {
        bytes,
        device: after.dev(),
        inode: after.ino(),
        mode: after.mode(),
        uid: after.uid(),
        gid: after.gid(),
    }))
}

fn write_new_file(path: &Path, bytes: &[u8], mode: u32, uid: u32, gid: u32) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| format!("unable to create {}: {error}", path.display()))?;
    let result = (|| {
        if unsafe { libc::fchown(file.as_raw_fd(), uid, gid) } != 0 {
            return Err(format!(
                "unable to set ownership on {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }
        file.set_permissions(fs::Permissions::from_mode(mode & 0o7777))
            .map_err(|error| format!("unable to set mode on {}: {error}", path.display()))?;
        file.write_all(bytes)
            .map_err(|error| format!("unable to write {}: {error}", path.display()))?;
        file.sync_all()
            .map_err(|error| format!("unable to sync {}: {error}", path.display()))
    })();
    if result.is_err() {
        drop(file);
        let _ = fs::remove_file(path);
    }
    result
}

fn same_exact_file(left: &ExactFileSnapshot, right: &ExactFileSnapshot) -> bool {
    left.bytes == right.bytes
        && left.device == right.device
        && left.inode == right.inode
        && left.mode == right.mode
        && left.uid == right.uid
        && left.gid == right.gid
}

fn replace_exact_file(
    path: &Path,
    original: &ExactFileSnapshot,
    candidate: &[u8],
) -> Result<(), String> {
    let current = read_exact_file(path)?
        .ok_or_else(|| format!("{} disappeared before replacement", path.display()))?;
    if !same_exact_file(original, &current) {
        return Err(format!("{} changed before replacement", path.display()));
    }
    let parent = path
        .parent()
        .ok_or_else(|| "bridger configuration has no parent directory".to_string())?;
    let temporary = parent.join(format!(
        ".bridger.cake-autorate.{}.{}",
        std::process::id(),
        REPLACE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    write_new_file(
        &temporary,
        candidate,
        original.mode,
        original.uid,
        original.gid,
    )?;
    let current = read_exact_file(path)?
        .ok_or_else(|| format!("{} disappeared before commit", path.display()))?;
    if !same_exact_file(original, &current) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("{} changed before commit", path.display()));
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("unable to commit bridger configuration: {error}"));
    }
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("unable to sync bridger configuration directory: {error}"))?;
    let committed = read_exact_file(path)?
        .ok_or_else(|| "bridger configuration disappeared after commit".to_string())?;
    if committed.bytes != candidate
        || committed.mode != original.mode
        || committed.uid != original.uid
        || committed.gid != original.gid
    {
        return Err("bridger configuration failed its exact commit postcondition".to_string());
    }
    Ok(())
}

fn require_success(output: &BoundedCommandOutput, operation: &str) -> Result<(), String> {
    if output.status.success() {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if detail.is_empty() {
        format!("unable to {operation}: {}", output.status)
    } else {
        format!("unable to {operation}: {detail}")
    })
}

fn executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
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
        .ok_or_else(|| format!("SQM option {key} is missing"))?
        .parse::<u64>()
        .ok()
        .filter(|value| *value <= 10_000_000_000)
        .ok_or_else(|| format!("SQM option {key} is invalid"))
}

fn env_path(name: &str, default: &str) -> PathBuf {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(default))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "calibration")]
    #[test]
    fn r4_native_apply_admission_is_read_only_and_rejects_pending_or_damaged_start_state() {
        let root = std::env::temp_dir().join(format!(
            "cake-native-admission-{}-{}",
            std::process::id(),
            REPLACE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
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
        let runtime = root.join("run");
        fs::create_dir(&config).unwrap();
        fs::write(config.join(CAKE_PACKAGE), b"original cake\n").unwrap();
        fs::write(config.join(SQM_PACKAGE), b"original sqm\n").unwrap();
        require_no_pending_start_in(&config, &runtime).unwrap();
        assert!(!runtime.exists());
        let journal = config.join(".start-uci");
        fs::create_dir(&journal).unwrap();
        fs::set_permissions(&journal, fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir(journal.join("pending")).unwrap();
        assert_eq!(
            require_no_pending_start_in(&config, &runtime).unwrap_err(),
            "native-apply-service-publication-recovery-pending"
        );
        assert!(
            !journal.join("lock").exists(),
            "admission must not enter transaction recovery"
        );
        fs::remove_dir_all(&journal).unwrap();
        let inputs = runtime.join(".controller-input");
        fs::create_dir_all(&inputs).unwrap();
        fs::set_permissions(&inputs, fs::Permissions::from_mode(0o700)).unwrap();
        for name in [
            "batch.pending",
            "batch.pending.tmp",
            "batch.restore",
            "batch.restore.tmp",
        ] {
            let path = inputs.join(name);
            fs::write(&path, b"partial retained evidence").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            let before = fs::metadata(&path).unwrap();
            assert!(require_no_pending_start_in(&config, &runtime).is_err());
            assert_eq!(fs::read(&path).unwrap(), b"partial retained evidence");
            assert_eq!(fs::metadata(&path).unwrap().ino(), before.ino());
            fs::remove_file(path).unwrap();
        }
        require_no_pending_start_in(&config, &runtime).unwrap();
        assert_eq!(
            fs::read(config.join(CAKE_PACKAGE)).unwrap(),
            b"original cake\n"
        );
        assert_eq!(
            fs::read(config.join(SQM_PACKAGE)).unwrap(),
            b"original sqm\n"
        );
        assert_eq!(fs::read_dir(&inputs).unwrap().count(), 0);
    }

    #[test]
    fn r4_controller_environment_generation_is_exact_bounded_and_never_echoes_other_values() {
        let id = "b".repeat(64);
        let bytes = format!(
            "UNRELATED=private fixture\0{}={id}\0",
            super::super::controller_input::GENERATION_ENV
        );
        assert_eq!(
            parse_controller_generation(bytes.as_bytes()).unwrap(),
            Some(id)
        );
        assert_eq!(
            parse_controller_generation(b"UNRELATED=private fixture\0").unwrap(),
            None
        );
        for bytes in [
            bytes.repeat(2).into_bytes(),
            b"missing-null-terminator".to_vec(),
            vec![b'x'; 65537],
        ] {
            let error = parse_controller_generation(&bytes).err().unwrap();
            assert!(!error.contains("private fixture"));
        }
    }

    #[test]
    #[ignore = "requires explicit inspected SDK UCI and loader paths"]
    fn r4_start_candidate_real_uci_presets_projection_and_transaction_rollback() {
        let root = test_root("start-candidate-real");
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let config = root.join("config");
        let pending = root.join("pending");
        for path in [&config, &pending] {
            fs::create_dir(path).unwrap();
        }
        let source = "config cake_autorate 'lab'\n option enabled '1'\n option sqm_enabled '1'\n option wan_if 'logical0'\n option sqm_download '20000'\n option sqm_upload '10000'\n";
        let foreign = "# retained foreign\nconfig queue\n option enabled '0'\n option interface 'foreign0'\n list note 'one'\n list note 'two'\n";
        fs::write(config.join(CAKE_PACKAGE), source).unwrap();
        fs::write(config.join(SQM_PACKAGE), foreign).unwrap();
        fs::write(
            pending.join(CAKE_PACKAGE),
            "cake-autorate.lab.wan_if='not-the-committed-target'\n",
        )
        .unwrap();
        fn quote(value: &std::ffi::OsStr) -> String {
            format!("'{}'", value.to_str().unwrap().replace('\'', "'\\''"))
        }
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
        let wrapper = root.join("uci-wrapper");
        write_executable(&wrapper, &format!("#!/bin/sh\nfor last do :; done\ncase \"$last\" in cu??????????????????????????????) ;; *) exit 91;; esac\nexec {command} -p {} \"$@\"\n", quote(pending.as_os_str())));
        let resolver = Resolver(BTreeMap::from([("logical0".into(), "fixture0".into())]));
        let snapshot = CommittedSnapshot::capture(&config, &root.join("run"), &wrapper).unwrap();
        let candidate = StartCandidate::prepare(snapshot, &resolver, &wrapper).unwrap();
        assert_eq!(candidate.plan.instances, ["lab"]);
        assert_eq!(candidate.plan.managed.len(), 1);
        assert_eq!(candidate.plan.managed[0].target_interface, "fixture0");
        assert_eq!(candidate.plan.managed[0].minimum_download_kbps, 20000);
        assert!(candidate.projection.is_managed());
        assert_eq!(
            candidate.config.package(CAKE_PACKAGE).unwrap().sections["lab"].options["dl_if"],
            "ifb4fixture0"
        );
        assert_eq!(
            candidate.config.package(SQM_PACKAGE).unwrap().sections["cake_lab"].options["upload"],
            "10000"
        );
        assert!(candidate
            .config
            .candidate_bytes(SQM_PACKAGE)
            .unwrap()
            .starts_with(foreign.as_bytes()));
        candidate.config.attest().unwrap();
        assert_eq!(
            fs::read(config.join(CAKE_PACKAGE)).unwrap(),
            source.as_bytes()
        );
        assert_eq!(
            fs::read(config.join(SQM_PACKAGE)).unwrap(),
            foreign.as_bytes()
        );
        assert_eq!(
            fs::read(pending.join(CAKE_PACKAGE)).unwrap(),
            b"cake-autorate.lab.wan_if='not-the-committed-target'\n"
        );
        // Exercise the same native-prepared bytes through the real filesystem
        // transaction, then restore before the next negative candidate case.
        let published = crate::operations::uci_transaction::publish(candidate.config).unwrap();
        published.attest().unwrap();
        let inputs = root.join(".controller-input");
        let input =
            super::super::controller_input::store(&published, "lab", &inputs, &config).unwrap();
        let loaded =
            super::super::controller_input::load("lab", input.generation(), &inputs, &config)
                .unwrap();
        assert_eq!(loaded.config.ul_if, "fixture0");
        assert_eq!(loaded.config.base_dl_shaper_rate_kbps, 20000.0);
        loaded.guard.attest().unwrap();
        {
            let proc_root = root.join("generation-proc");
            fs::create_dir_all(proc_root.join("100")).unwrap();
            fs::write(proc_root.join("stat"), b"btime 100\n").unwrap();
            fs::write(
                proc_root.join("100/cmdline"),
                b"/usr/sbin/cake-autorated\0--instance\0lab\0",
            )
            .unwrap();
            let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
            fs::write(
                proc_root.join("100/stat"),
                format!(
                    "100 (cake) S 1 100 100 0 0 0 0 0 0 0 0 0 0 0 0 0 1 0 {}\n",
                    ticks * 10
                ),
            )
            .unwrap();
            fs::write(
                proc_root.join("100/environ"),
                format!(
                    "{}={}\0",
                    super::super::controller_input::GENERATION_ENV,
                    input.generation()
                ),
            )
            .unwrap();
            fs::create_dir(root.join("lab")).unwrap();
            fs::write(root.join("lab/status.json"), b"{\"instance\":\"lab\",\"state\":\"RUNNING\",\"started_at\":111,\"updated_at\":121}").unwrap();
            let ubus = root.join("generation-ubus");
            let mut response = serde_json::json!({"cake-autorate":{"instances":{"lab":{
                "command":["/usr/sbin/cake-autorated","--instance","lab"],"pid":100,"running":true,
                "env":{super::super::controller_input::GENERATION_ENV:input.generation()}}}}});
            let response_path = root.join("generation-procd.json");
            fs::write(&response_path, serde_json::to_vec(&response).unwrap()).unwrap();
            write_executable(
                &ubus,
                &format!("#!/bin/sh\ncat '{}'\n", response_path.display()),
            );
            let paths = ServicePaths {
                config_root: config.clone(),
                uci: wrapper.clone(),
                tc: root.join("unused-tc"),
                ubus,
                proc_root,
                runtime_root: root.clone(),
                runtime_lock_root: root.join("locks"),
                bridger_init: root.join("unused-bridger"),
                bridger_config: root.join("unused-bridger-config"),
                uci_workspace_root: root.join("uci-work"),
            };
            let batch = super::super::controller_input::store_batch(
                &published,
                &["lab".into()],
                &BTreeMap::from([("lab".into(), input.clone())]),
                &inputs,
                &config,
            )
            .unwrap();
            assert_eq!(
                observe_controller_start(&paths, &["lab".into()], 123.0).unwrap(),
                ControllerStartReadiness::Ready
            );
            assert!(observe_controller_start(&paths, &[], 123.0).is_err());
            let environ = fs::read(paths.proc_root.join("100/environ")).unwrap();
            fs::write(paths.proc_root.join("100/environ"), b"UNRELATED=value\0").unwrap();
            assert!(matches!(
                observe_controller_start(&paths, &["lab".into()], 123.0).unwrap(),
                ControllerStartReadiness::Waiting(_)
            ));
            fs::write(paths.proc_root.join("100/environ"), environ).unwrap();
            response["cake-autorate"]["instances"]["lab"]["env"]
                [super::super::controller_input::GENERATION_ENV] =
                serde_json::json!("0".repeat(64));
            fs::write(&response_path, serde_json::to_vec(&response).unwrap()).unwrap();
            assert!(matches!(
                observe_controller_start(&paths, &["lab".into()], 123.0).unwrap(),
                ControllerStartReadiness::Waiting(_)
            ));
            // A retained batch remains a Stop recipe even during a procd
            // respawn gap with no current controller /proc entry.
            fs::remove_dir_all(paths.proc_root.join("100")).unwrap();
            assert!(discover_controllers(&paths.proc_root).unwrap().is_empty());
            let orphan_inputs = batch.load_inputs().unwrap();
            assert_eq!(orphan_inputs["lab"].guard.generation(), input.generation());
            assert_eq!(
                stop_plan_from_views(
                    &UciPackage::default(),
                    &UciPackage::default(),
                    &orphan_inputs,
                    &resolver
                )
                .unwrap()
                .managed[0]
                    .target_interface,
                "fixture0"
            );
            batch
                .retire(|_| {
                    assert!(discover_controllers(&paths.proc_root).unwrap().is_empty());
                    Ok(())
                })
                .unwrap();
        }
        let mut next_cake = published.config().package(CAKE_PACKAGE).unwrap().clone();
        next_cake
            .sections
            .get_mut("lab")
            .unwrap()
            .options
            .insert("min_dl_shaper_rate_kbps".into(), "invalid-new-value".into());
        next_cake
            .sections
            .get_mut("lab")
            .unwrap()
            .options
            .insert("dl_if".into(), "new_ifb".into());
        let mut next_sqm = published.config().package(SQM_PACKAGE).unwrap().clone();
        next_sqm
            .sections
            .get_mut("cake_lab")
            .unwrap()
            .options
            .insert("interface".into(), "new_target".into());
        let applied = BTreeMap::from([("lab".into(), loaded)]);
        let remapped = Resolver(BTreeMap::from([(
            "fixture0".into(),
            "must_not_remap".into(),
        )]));
        let stopped = stop_plan_from_views(&next_cake, &next_sqm, &applied, &remapped).unwrap();
        assert_eq!(stopped.managed.len(), 1);
        assert_eq!(stopped.managed[0].target_interface, "fixture0");
        assert_eq!(stopped.managed[0].download_interface, "ifb4fixture0");
        let mut conflict = next_sqm.clone();
        conflict.sections.insert(
            "alien".into(),
            UciSection {
                section_type: "queue".into(),
                options: BTreeMap::from([
                    ("_cake_autorate_managed".into(), "other".into()),
                    ("interface".into(), "fixture0".into()),
                ]),
            },
        );
        assert!(stop_plan_from_views(&next_cake, &conflict, &applied, &remapped).is_err());
        input
            .retire(|instance, id| {
                assert_eq!(instance, "lab");
                assert_eq!(id, input.generation());
                Ok(())
            })
            .unwrap();
        published.rollback().unwrap();
        assert_eq!(
            fs::read(config.join(CAKE_PACKAGE)).unwrap(),
            source.as_bytes()
        );
        assert_eq!(
            fs::read(config.join(SQM_PACKAGE)).unwrap(),
            foreign.as_bytes()
        );
        // Invalid controller parameters cannot produce a publishable candidate.
        let invalid = format!("{source} option no_pingers '1000000'\n");
        fs::write(config.join(CAKE_PACKAGE), &invalid).unwrap();
        let snapshot = CommittedSnapshot::capture(&config, &root.join("run"), &wrapper).unwrap();
        assert_eq!(
            StartCandidate::prepare(snapshot, &resolver, &wrapper)
                .err()
                .unwrap(),
            "service-start-invalid-controller-config:lab"
        );
        assert_eq!(
            fs::read(config.join(CAKE_PACKAGE)).unwrap(),
            invalid.as_bytes()
        );
        assert_eq!(
            fs::read(config.join(SQM_PACKAGE)).unwrap(),
            foreign.as_bytes()
        );
        let invalid_globals = format!(
            "{source}\nconfig globals 'globals'\n option graph_history_ram_budget_kib '1'\n"
        );
        fs::write(config.join(CAKE_PACKAGE), &invalid_globals).unwrap();
        let snapshot = CommittedSnapshot::capture(&config, &root.join("run"), &wrapper).unwrap();
        assert_eq!(
            StartCandidate::prepare(snapshot, &resolver, &wrapper)
                .err()
                .unwrap(),
            "service-start-invalid-global-history-config"
        );
        assert_eq!(
            fs::read(config.join(CAKE_PACKAGE)).unwrap(),
            invalid_globals.as_bytes()
        );
        assert_eq!(
            fs::read(config.join(SQM_PACKAGE)).unwrap(),
            foreign.as_bytes()
        );

        // A cold MQ cache must not reject a valid restart before its normal
        // capability refresh. Conversely, malformed controller settings must
        // fail before even a scratch-interface probe is requested.
        struct MqResolver<'a> {
            inner: &'a Resolver,
            proof: std::cell::Cell<bool>,
        }
        impl InterfaceResolver for MqResolver<'_> {
            fn resolve(&self, name: &str) -> Result<String, String> {
                self.inner.resolve(name)
            }
            fn supports_cake_mq(&self, script: &str) -> Result<bool, String> {
                assert_eq!(script, "piece_of_cake.qos");
                Ok(self.proof.get())
            }
        }
        let mq_resolver = MqResolver {
            inner: &resolver,
            proof: std::cell::Cell::new(false),
        };
        let mq_source = format!("{source} option sqm_qdisc 'cake_mq'\n");
        let conflict = format!("{mq_source}\nconfig cake_autorate 'second'\n option enabled '1'\n option sqm_enabled '1'\n option wan_if 'logical0'\n");
        fs::write(config.join(CAKE_PACKAGE), &conflict).unwrap();
        let snapshot = CommittedSnapshot::capture(&config, &root.join("run"), &wrapper).unwrap();
        assert_eq!(
            StartCandidate::prepare_with_mq_refresh(snapshot, &mq_resolver, &wrapper, |_| {
                panic!("conflicting targets must fail before capability probes")
            })
            .err()
            .unwrap(),
            "service-start-conflicting-managed-targets"
        );
        assert_eq!(
            fs::read(config.join(CAKE_PACKAGE)).unwrap(),
            conflict.as_bytes()
        );
        assert_eq!(
            fs::read(config.join(SQM_PACKAGE)).unwrap(),
            foreign.as_bytes()
        );
        let invalid = format!("{mq_source} option no_pingers '1000000'\n");
        fs::write(config.join(CAKE_PACKAGE), &invalid).unwrap();
        let snapshot = CommittedSnapshot::capture(&config, &root.join("run"), &wrapper).unwrap();
        assert_eq!(
            StartCandidate::prepare_with_mq_refresh(snapshot, &mq_resolver, &wrapper, |_| {
                panic!("invalid candidate must never request a capability probe")
            })
            .err()
            .unwrap(),
            "service-start-invalid-controller-config:lab"
        );
        fs::write(config.join(CAKE_PACKAGE), &mq_source).unwrap();
        let snapshot = CommittedSnapshot::capture(&config, &root.join("run"), &wrapper).unwrap();
        let mut probes = 0;
        let candidate =
            StartCandidate::prepare_with_mq_refresh(snapshot, &mq_resolver, &wrapper, |script| {
                assert_eq!(script, "piece_of_cake.qos");
                probes += 1;
                mq_resolver.proof.set(true);
                Ok(())
            })
            .unwrap();
        assert_eq!(probes, 1);
        candidate.attest_plan(&mq_resolver).unwrap();
        assert_eq!(
            fs::read(config.join(CAKE_PACKAGE)).unwrap(),
            mq_source.as_bytes()
        );
        assert_eq!(
            fs::read(config.join(SQM_PACKAGE)).unwrap(),
            foreign.as_bytes()
        );
        drop(candidate);

        mq_resolver.proof.set(false);
        let snapshot = CommittedSnapshot::capture(&config, &root.join("run"), &wrapper).unwrap();
        assert!(
            StartCandidate::prepare_with_mq_refresh(snapshot, &mq_resolver, &wrapper, |_| Ok(()))
                .is_err(),
            "probe success without actual boot-bound proof is not admission"
        );
        let snapshot = CommittedSnapshot::capture(&config, &root.join("run"), &wrapper).unwrap();
        assert_eq!(
            StartCandidate::prepare_with_mq_refresh(snapshot, &mq_resolver, &wrapper, |_| {
                Err("fixture-probe-failed".into())
            })
            .err()
            .unwrap(),
            "fixture-probe-failed"
        );
        let snapshot = CommittedSnapshot::capture(&config, &root.join("run"), &wrapper).unwrap();
        assert!(
            StartCandidate::prepare_with_mq_refresh(snapshot, &mq_resolver, &wrapper, |_| {
                mq_resolver.proof.set(true);
                fs::write(config.join(CAKE_PACKAGE), &source).unwrap();
                Ok(())
            })
            .is_err(),
            "source drift during capability refresh must not be rebased"
        );
        assert_eq!(
            fs::read(config.join(CAKE_PACKAGE)).unwrap(),
            source.as_bytes()
        );
        assert_eq!(
            fs::read(config.join(SQM_PACKAGE)).unwrap(),
            foreign.as_bytes()
        );
        #[cfg(feature = "calibration")]
        {
            let bad_mqtt =
                format!("{source} option mqtt_enabled '1'\n option mqtt_port 'not-a-port'\n");
            fs::write(config.join(CAKE_PACKAGE), &bad_mqtt).unwrap();
            let snapshot =
                CommittedSnapshot::capture(&config, &root.join("run"), &wrapper).unwrap();
            assert!(StartCandidate::prepare_with_mq_refresh(
                snapshot,
                &resolver,
                &wrapper,
                |_| panic!("invalid MQTT must fail before effects")
            )
            .is_err());
            assert_eq!(
                fs::read(config.join(CAKE_PACKAGE)).unwrap(),
                bad_mqtt.as_bytes()
            );
            fs::write(config.join(CAKE_PACKAGE), source).unwrap();
        }
        ordinary_start_publication_fixture(&root, &config, &wrapper, &resolver);
        ordinary_reload_delta_fixture(&root, &wrapper);
        #[cfg(feature = "calibration")]
        {
            selected_generation_registration_fixture(&root, &config, &wrapper, &resolver);
            bootstrap_generation_rollback_fixture(&root, &wrapper, false);
            bootstrap_generation_rollback_fixture(&root, &wrapper, true);
        }
    }

    fn ordinary_reload_delta_fixture(parent: &Path, uci: &Path) {
        let root = parent.join("reload-delta");
        let config = root.join("config");
        let runtime = root.join("run");
        fs::create_dir_all(&config).unwrap();
        let paths = ServicePaths {
            config_root: config.clone(),
            uci: uci.into(),
            tc: root.join("unused-tc"),
            ubus: root.join("unused-ubus"),
            proc_root: root.join("unused-proc"),
            runtime_root: runtime.clone(),
            runtime_lock_root: root.join("locks"),
            bridger_init: root.join("unused-bridger"),
            bridger_config: root.join("unused-bridger-config"),
            uci_workspace_root: root.join("uci-work"),
        };
        let resolver = Resolver(BTreeMap::new());
        let section = |name: &str, interface: &str, download: u64| {
            format!("config cake_autorate '{name}'\n option enabled '1'\n option sqm_enabled '1'\n option wan_if '{interface}'\n option no_pingers '6'\n option sqm_download '{download}'\n option sqm_upload '10000'\n")
        };
        fs::write(
            config.join(CAKE_PACKAGE),
            format!(
                "{}\n{}",
                section("lab", "fixture0", 20000),
                section("peer", "fixture1", 40000)
            ),
        )
        .unwrap();
        fs::write(config.join(SQM_PACKAGE), b"").unwrap();
        let capture = || {
            StartCandidate::prepare(
                CommittedSnapshot::capture(&config, &runtime, uci).unwrap(),
                &resolver,
                uci,
            )
            .unwrap()
        };
        let start = StartPublication::publish(&paths, capture()).unwrap();
        let pending = start.batch.clone();
        drop(start);
        assert_eq!(
            ServiceReloadDelta::prepare(&capture(), &pending, &resolver).unwrap_err(),
            "controller-batch-not-settled"
        );
        pending.accept(|_| Ok(())).unwrap(); // Synthetic readiness, no processes.
        let batch =
            super::super::controller_input::load_batch(&runtime.join(".controller-input"), &config)
                .unwrap()
                .unwrap();
        let cake = fs::read_to_string(config.join(CAKE_PACKAGE)).unwrap();
        let sqm = fs::read_to_string(config.join(SQM_PACKAGE)).unwrap();
        let evaluate = |next_cake: &str, next_sqm: &str| {
            fs::write(config.join(CAKE_PACKAGE), next_cake).unwrap();
            fs::write(config.join(SQM_PACKAGE), next_sqm).unwrap();
            let versions = [CAKE_PACKAGE, SQM_PACKAGE].map(|name| {
                (
                    fs::metadata(config.join(name)).unwrap().ino(),
                    fs::read(config.join(name)).unwrap(),
                )
            });
            let delta = ServiceReloadDelta::prepare(&capture(), &batch, &resolver).unwrap();
            for (name, (inode, bytes)) in [CAKE_PACKAGE, SQM_PACKAGE].into_iter().zip(versions) {
                assert_eq!(fs::metadata(config.join(name)).unwrap().ino(), inode);
                assert_eq!(fs::read(config.join(name)).unwrap(), bytes);
            }
            batch.attest_record().unwrap();
            assert!(batch.pending_update().unwrap().is_none());
            delta
        };
        let unchanged = evaluate(&cake, &sqm);
        assert_eq!(unchanged.retained, *batch.generations());
        assert!(unchanged.stop_controllers.is_empty());
        assert!(unchanged.start_controllers.is_empty());
        assert!(unchanged.stop_sqm.is_empty());
        assert!(unchanged.start_sqm.is_empty());
        assert!(!unchanged.publication_required);
        let controller = cake.replacen("option no_pingers '6'", "option no_pingers '3'", 1);
        assert_ne!(controller, cake);
        let delta = evaluate(&controller, &sqm);
        assert_eq!(
            delta.retained,
            BTreeMap::from([("peer".into(), batch.generations()["peer"].clone())])
        );
        assert_eq!(delta.stop_controllers, ["lab"]);
        assert_eq!(delta.start_controllers, ["lab"]);
        assert!(delta.stop_sqm.is_empty());
        assert!(delta.start_sqm.is_empty());

        let rate = cake.replace("option sqm_download '20000'", "option sqm_download '24000'");
        assert_ne!(rate, cake);
        let delta = evaluate(&rate, &sqm);
        assert_eq!(delta.retained.len(), 1);
        assert!(delta.retained.contains_key("peer"));
        assert_eq!(delta.stop_controllers, ["lab"]);
        assert_eq!(delta.start_controllers, ["lab"]);
        assert_eq!(delta.stop_sqm.len(), 1);
        assert_eq!(delta.stop_sqm[0].instance, "lab");
        assert_eq!(delta.stop_sqm[0].target_interface, "fixture0");
        assert_eq!(delta.start_sqm.len(), 1);
        assert_eq!(delta.start_sqm[0].minimum_download_kbps, 24000);
        assert!(delta.publication_required);

        let moved = cake.replace("'fixture0'", "'fixture2'");
        assert_ne!(moved, cake);
        let delta = evaluate(&moved, &sqm);
        assert_eq!(delta.stop_sqm[0].target_interface, "fixture0");
        assert_eq!(delta.start_sqm[0].target_interface, "fixture2");
        assert!(delta.retained.contains_key("peer"));

        // Disabling and deleting the controller still stop its old frozen
        // queue, even if the new committed SQM file no longer describes it.
        let disabled = cake.replacen("option enabled '1'", "option enabled '0'", 1);
        let delta = evaluate(&disabled, &sqm);
        assert_eq!(delta.stop_controllers, ["lab"]);
        assert!(delta.start_controllers.is_empty());
        assert_eq!(delta.stop_sqm[0].instance, "lab");
        assert!(delta.start_sqm.is_empty());
        assert!(delta.retained.contains_key("peer"));
        let peer_offset = cake.find("config cake_autorate 'peer'").unwrap();
        let delta = evaluate(&cake[peer_offset..], "");
        assert_eq!(delta.stop_controllers, ["lab"]);
        assert_eq!(delta.stop_sqm[0].target_interface, "fixture0");
        assert!(delta.retained.contains_key("peer"));

        let added = format!("{cake}\n{}", section("added", "fixture3", 30000));
        let delta = evaluate(&added, &sqm);
        assert_eq!(delta.retained, *batch.generations());
        assert!(delta.stop_controllers.is_empty());
        assert_eq!(delta.start_controllers, ["added"]);
        assert!(delta.stop_sqm.is_empty());
        assert_eq!(delta.start_sqm[0].instance, "added");

        let history = format!(
            "{cake}\nconfig globals 'globals'\n option graph_history_ram_budget_kib '8192'\n"
        );
        let delta = evaluate(&history, &sqm);
        assert!(delta.retained.is_empty());
        assert_eq!(delta.stop_controllers, ["lab", "peer"]);
        assert_eq!(delta.start_controllers, ["lab", "peer"]);
        assert!(delta.stop_sqm.is_empty());
        assert!(delta.start_sqm.is_empty());
        let foreign = format!(
            "{sqm}\nconfig queue 'foreign'\n option enabled '0'\n option interface 'foreign0'\n"
        );
        assert_eq!(evaluate(&cake, &foreign), unchanged);

        for name in ["batch.restore", "batch.restore.tmp"] {
            let marker = runtime.join(".controller-input").join(name);
            fs::write(&marker, b"incomplete fixture recovery marker").unwrap();
            assert_eq!(
                ServiceReloadDelta::prepare(&capture(), &batch, &resolver).unwrap_err(),
                "controller-batch-not-settled"
            );
            assert_eq!(
                fs::read(&marker).unwrap(),
                b"incomplete fixture recovery marker"
            );
            fs::remove_file(marker).unwrap();
        }
        let captured = capture();
        fs::write(config.join(CAKE_PACKAGE), &controller).unwrap();
        assert!(ServiceReloadDelta::prepare(&captured, &batch, &resolver).is_err());
        assert_eq!(
            fs::read_to_string(config.join(CAKE_PACKAGE)).unwrap(),
            controller
        );
        drop(captured);

        // Bind the real-UCI delta to a durable mixed-source batch. No runtime
        // actions happen here: readiness callbacks below are synthetic.
        fs::write(config.join(CAKE_PACKAGE), &rate).unwrap();
        fs::write(config.join(SQM_PACKAGE), &sqm).unwrap();
        let candidate = capture();
        let delta = ServiceReloadDelta::prepare(&candidate, &batch, &resolver).unwrap();
        let expected = candidate.plan.instances.clone();
        let published = super::super::uci_transaction::publish(candidate.config).unwrap();
        let inputs_root = runtime.join(".controller-input");
        let mut replacements = BTreeMap::new();
        for name in &delta.start_controllers {
            replacements.insert(
                name.clone(),
                super::super::controller_input::store_for_selected_update(
                    &published,
                    name,
                    &inputs_root,
                    &config,
                )
                .unwrap(),
            );
        }
        assert_eq!(
            super::super::controller_input::replace_reload_batch(
                &published,
                &batch,
                &expected,
                batch.generations(),
                &BTreeMap::new(),
                |_| panic!("changed input cannot be retained even with a valid old generation"),
            )
            .unwrap_err(),
            "controller-batch-retained-input-changed"
        );
        let pending = super::super::controller_input::replace_reload_batch(
            &published,
            &batch,
            &expected,
            &delta.retained,
            &replacements,
            |old| {
                assert_eq!(old, batch.generations());
                Ok(())
            },
        )
        .unwrap();
        assert!(pending.is_reload_update());
        assert!(!pending.updates_only("lab"));
        assert_eq!(pending.generations()["peer"], batch.generations()["peer"]);
        assert_ne!(pending.generations()["lab"], batch.generations()["lab"]);
        drop(published);
        pending.accept(|_| Ok(())).unwrap();
        let applied = super::super::controller_input::load_batch(&inputs_root, &config)
            .unwrap()
            .unwrap();
        let settled = ServiceReloadDelta::prepare(&capture(), &applied, &resolver).unwrap();
        assert_eq!(settled.retained, *applied.generations());
        assert!(settled.stop_sqm.is_empty());
        assert!(settled.start_sqm.is_empty());

        let next = fs::read_to_string(config.join(CAKE_PACKAGE))
            .unwrap()
            .replace("option sqm_download '24000'", "option sqm_download '28000'");
        fs::write(config.join(CAKE_PACKAGE), &next).unwrap();
        let staged =
            ReloadPublication::publish(&paths, capture(), applied.clone(), &resolver, |old| {
                assert_eq!(old, applied.generations());
                Ok(())
            })
            .unwrap();
        assert_eq!(staged.delta.start_controllers, ["lab"]);
        assert_eq!(staged.delta.start_sqm[0].minimum_download_kbps, 28000);
        let desired = staged.start.batch.generations().clone();
        let stopped = staged.delta.stop_sqm.clone();
        let started = staged.delta.start_sqm.clone();
        drop(staged); // Preparer exited after durable intent, before runtime effects.
        let versions = [CAKE_PACKAGE, SQM_PACKAGE].map(|name| {
            (
                fs::metadata(config.join(name)).unwrap().ino(),
                fs::read(config.join(name)).unwrap(),
            )
        });
        let pending = super::super::controller_input::load_batch(&inputs_root, &config)
            .unwrap()
            .unwrap();
        let resumed = ReloadPublication::resume(capture(), pending, &resolver).unwrap();
        resumed.attest(&resolver).unwrap();
        assert_eq!(resumed.start.batch.generations(), &desired);
        assert_eq!(resumed.delta.stop_sqm, stopped);
        assert_eq!(resumed.delta.start_sqm, started);
        assert_eq!(
            resumed.delta.retained,
            BTreeMap::from([("peer".into(), applied.generations()["peer"].clone())])
        );
        for (name, (inode, bytes)) in [CAKE_PACKAGE, SQM_PACKAGE].into_iter().zip(versions) {
            assert_eq!(fs::metadata(config.join(name)).unwrap().ino(), inode);
            assert_eq!(fs::read(config.join(name)).unwrap(), bytes);
        }
        let pending = resumed.start.batch.clone();
        drop(resumed);
        pending.accept(|_| Ok(())).unwrap();
        let applied = super::super::controller_input::load_batch(&inputs_root, &config)
            .unwrap()
            .unwrap();
        assert!(ReloadPublication::resume(capture(), applied.clone(), &resolver).is_err());
        applied.attest_settled().unwrap();
        let base = fs::read_to_string(config.join(CAKE_PACKAGE)).unwrap();
        for mode in ["before-draft", "partial-draft", "complete-draft"] {
            let requested =
                base.replace("option sqm_download '28000'", "option sqm_download '32000'");
            assert_ne!(requested, base);
            fs::write(config.join(CAKE_PACKAGE), &requested).unwrap();
            let before = [CAKE_PACKAGE, SQM_PACKAGE].map(|name| {
                (
                    fs::metadata(config.join(name)).unwrap().ino(),
                    fs::read(config.join(name)).unwrap(),
                )
            });
            let mut calls = 0;
            let failure_at = if mode == "complete-draft" { 4 } else { 2 };
            assert!(ReloadPublication::publish(
                &paths,
                capture(),
                applied.clone(),
                &resolver,
                |old| {
                    assert_eq!(old, applied.generations());
                    calls += 1;
                    if calls == failure_at {
                        Err("injected-before-reload-intent".into())
                    } else {
                        Ok(())
                    }
                }
            )
            .is_err());
            assert_eq!(calls, failure_at);
            let draft = inputs_root.join("batch.pending.tmp");
            if mode == "partial-draft" {
                fs::write(&draft, b"cake-autorate controller batch v1\n{\"schema\":1,").unwrap();
                fs::set_permissions(&draft, fs::Permissions::from_mode(0o600)).unwrap();
            }
            assert!(!inputs_root.join("batch.pending").exists());
            assert_eq!(draft.exists(), mode != "before-draft");
            assert!(super::super::uci_transaction::recovery_pending(&config).unwrap());
            let candidate_bytes =
                [CAKE_PACKAGE, SQM_PACKAGE].map(|name| fs::read(config.join(name)).unwrap());
            assert!(ReloadPublication::recover_preparation(&paths, |_| Err(
                "old-runtime-unproven".into()
            ))
            .is_err());
            for (name, bytes) in [CAKE_PACKAGE, SQM_PACKAGE].into_iter().zip(candidate_bytes) {
                assert_eq!(fs::read(config.join(name)).unwrap(), bytes);
            }
            assert!(super::super::uci_transaction::recovery_pending(&config).unwrap());
            let original = ReloadPublication::recover_preparation(&paths, |old| {
                assert_eq!(old, applied.generations());
                Ok(())
            })
            .unwrap()
            .unwrap();
            original.attest(&config).unwrap();
            assert!(!draft.exists());
            assert!(!super::super::uci_transaction::recovery_pending(&config).unwrap());
            for (name, (inode, bytes)) in [CAKE_PACKAGE, SQM_PACKAGE].into_iter().zip(before) {
                assert_eq!(fs::metadata(config.join(name)).unwrap().ino(), inode);
                assert_eq!(fs::read(config.join(name)).unwrap(), bytes);
            }
            applied.attest_settled().unwrap();
            assert!(ReloadPublication::recover_preparation(&paths, |old| {
                assert_eq!(old, applied.generations());
                Ok(())
            })
            .unwrap()
            .is_none());
        }
        assert!(!paths.proc_root.exists());
        assert!(!paths.ubus.exists());
    }

    fn ordinary_start_publication_fixture(
        root: &Path,
        config: &Path,
        uci: &Path,
        resolver: &impl InterfaceResolver,
    ) {
        let original = [CAKE_PACKAGE, SQM_PACKAGE].map(|name| fs::read(config.join(name)).unwrap());
        let runtime = root.join("ordinary-generation");
        fs::create_dir(&runtime).unwrap();
        let paths = ServicePaths {
            config_root: config.into(),
            uci: uci.into(),
            tc: runtime.join("unused-tc"),
            ubus: runtime.join("ubus"),
            proc_root: runtime.join("proc"),
            runtime_root: runtime.clone(),
            runtime_lock_root: runtime.join("locks"),
            bridger_init: runtime.join("unused-bridger"),
            bridger_config: runtime.join("unused-bridger-config"),
            uci_workspace_root: runtime.join("uci-work"),
        };
        let capture = || {
            StartCandidate::prepare(
                CommittedSnapshot::capture(config, &runtime, uci).unwrap(),
                resolver,
                uci,
            )
            .unwrap()
        };
        let inputs = runtime.join(".controller-input");
        fs::write(&inputs, b"foreign store entry").unwrap();
        assert!(StartPublication::publish(&paths, capture()).is_err());
        for (name, bytes) in [CAKE_PACKAGE, SQM_PACKAGE].into_iter().zip(&original) {
            assert_eq!(fs::read(config.join(name)).unwrap(), *bytes);
        }
        assert_eq!(fs::read(&inputs).unwrap(), b"foreign store entry");
        fs::remove_file(&inputs).unwrap();
        let start = StartPublication::publish(&paths, capture()).unwrap();
        start.attest(resolver).unwrap();
        assert!(start.batch.pending());
        let batch = super::super::controller_input::load_batch(&inputs, config)
            .unwrap()
            .unwrap();
        let id = batch.generations()["lab"].clone();
        assert_eq!(batch.load_inputs().unwrap()["lab"].config.ul_if, "fixture0");
        assert_eq!(
            encode_generation_start_plan(batch.generations(), &[]).unwrap(),
            format!("service-start-v3 lab:{id} -\n")
        );
        // Release the publisher's private alias lease before a second command
        // captures its own candidate. The durable pending batch survives it.
        drop(start);
        assert_eq!(
            unchanged_start_plan(&paths, resolver, &capture(), &batch, |_| panic!(
                "pending must not inspect MQTT"
            ))
            .unwrap_err(),
            "service-start-pending-generation-recovery-required"
        );
        assert!(
            !reload_is_unchanged(&paths, resolver, &capture(), |_| panic!(
                "pending is not a no-op"
            ))
            .unwrap()
        );
        // Publisher exit before registration leaves both the complete Stop
        // recipe and the transaction pending; no runtime was started here.
        batch.attest().unwrap();
        assert!(batch.load_inputs().unwrap()["lab"].guard.attest().is_ok());
        batch.accept(|_| Ok(())).unwrap(); // fixture's synthetic runtime proof
        let applied = super::super::controller_input::load_batch(&inputs, config)
            .unwrap()
            .unwrap();
        fs::create_dir_all(paths.proc_root.join("100")).unwrap();
        fs::write(paths.proc_root.join("stat"), b"btime 100\n").unwrap();
        fs::write(
            paths.proc_root.join("100/cmdline"),
            b"/usr/sbin/cake-autorated\0--instance\0lab\0",
        )
        .unwrap();
        fs::write(
            paths.proc_root.join("100/stat"),
            "100 (cake) S 1 100 100 0 0 0 0 0 0 0 0 0 0 0 0 0 1 0 1000\n",
        )
        .unwrap();
        fs::write(
            paths.proc_root.join("100/environ"),
            format!("{}={id}\0", super::super::controller_input::GENERATION_ENV),
        )
        .unwrap();
        let response = serde_json::json!({"cake-autorate":{"instances":{"lab":{
            "command":[DAEMON_PATH,"--instance","lab"],"pid":100,"running":true,
            "env":{super::super::controller_input::GENERATION_ENV:&id}
        }}}});
        write_executable(
            &paths.ubus,
            &format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", response),
        );
        let versions = [CAKE_PACKAGE, SQM_PACKAGE].map(|name| {
            let path = config.join(name);
            (fs::metadata(&path).unwrap().ino(), fs::read(path).unwrap())
        });
        let candidate = capture();
        assert!(
            reload_is_unchanged(&paths, resolver, &candidate, |_| Ok(Some(Vec::new()))).unwrap()
        );
        assert_eq!(
            unchanged_start_plan(&paths, resolver, &candidate, &applied, |_| Ok(Vec::new()))
                .unwrap(),
            format!("service-start-v3 lab:{id} -\n")
        );
        for (name, (inode, bytes)) in [CAKE_PACKAGE, SQM_PACKAGE].into_iter().zip(&versions) {
            assert_eq!(fs::metadata(config.join(name)).unwrap().ino(), *inode);
            assert_eq!(fs::read(config.join(name)).unwrap(), *bytes);
        }
        assert!(paths.proc_root.join("100").exists());
        drop(candidate);
        // An unrelated committed queue is not a reason to replace our applied
        // controller generation or rewrite either public configuration file.
        let foreign_change = format!(
            "{}\nconfig queue 'unrelated_new'\n option interface 'foreign1'\n option enabled '0'\n",
            String::from_utf8(versions[1].1.clone()).unwrap()
        );
        fs::write(config.join(SQM_PACKAGE), &foreign_change).unwrap();
        let foreign_inode = fs::metadata(config.join(SQM_PACKAGE)).unwrap().ino();
        let candidate = capture();
        assert!(
            reload_is_unchanged(&paths, resolver, &candidate, |_| Ok(Some(Vec::new()))).unwrap()
        );
        assert!(!reload_is_unchanged(&paths, resolver, &candidate, |_| Ok(None)).unwrap());
        assert_eq!(
            unchanged_start_plan(&paths, resolver, &candidate, &applied, |_| Ok(Vec::new()))
                .unwrap(),
            format!("service-start-v3 lab:{id} -\n")
        );
        assert_eq!(
            fs::read(config.join(SQM_PACKAGE)).unwrap(),
            foreign_change.as_bytes()
        );
        assert_eq!(
            fs::metadata(config.join(SQM_PACKAGE)).unwrap().ino(),
            foreign_inode
        );
        drop(candidate);
        let wrong_projection =
            foreign_change.replace("option download '20000'", "option download '1'");
        assert_ne!(wrong_projection, foreign_change);
        fs::write(config.join(SQM_PACKAGE), &wrong_projection).unwrap();
        assert!(
            !reload_is_unchanged(&paths, resolver, &capture(), |_| panic!(
                "required projection publication cannot be skipped"
            ))
            .unwrap()
        );
        assert_eq!(
            fs::read(config.join(SQM_PACKAGE)).unwrap(),
            wrong_projection.as_bytes()
        );
        fs::write(config.join(SQM_PACKAGE), &foreign_change).unwrap();
        let candidate = capture();
        // Wrong process generation refuses an unchanged response without
        // rewriting files or adopting another process as the controller.
        fs::write(
            paths.proc_root.join("100/environ"),
            format!(
                "{}={}\0",
                super::super::controller_input::GENERATION_ENV,
                "0".repeat(64)
            ),
        )
        .unwrap();
        assert!(
            unchanged_start_plan(&paths, resolver, &candidate, &applied, |_| Ok(Vec::new()))
                .is_err()
        );
        drop(candidate);
        // A real committed controller change is not silently published by a
        // second Start. Check before process/MQTT observations.
        let bytes = String::from_utf8(versions[0].1.clone()).unwrap();
        fs::write(
            config.join(CAKE_PACKAGE),
            format!(
                "{bytes}\nconfig globals 'globals'\n option graph_history_ram_budget_kib '8192'\n"
            ),
        )
        .unwrap();
        assert!(
            !reload_is_unchanged(&paths, resolver, &capture(), |_| panic!(
                "changed configuration requires restart"
            ))
            .unwrap()
        );
        assert_eq!(
            unchanged_start_plan(&paths, resolver, &capture(), &applied, |_| panic!(
                "changed input must fail first"
            ))
            .unwrap_err(),
            "service-start-changed-generation-requires-restart"
        );
        for (name, bytes) in [CAKE_PACKAGE, SQM_PACKAGE].into_iter().zip(original) {
            fs::write(config.join(name), bytes).unwrap();
        }
        ordinary_stop_recovery_fixture(&paths, resolver);
        unregistered_start_recovery_fixture(&paths, resolver);
        source_handoff_fixture(&paths);
    }

    #[test]
    fn r4_source_handoff_environment_is_bounded_and_non_secret() {
        use std::ffi::OsStr;
        assert!(source_id_value(OsStr::new("")).unwrap().is_none());
        assert_eq!(
            source_id_value(OsStr::new(&"a".repeat(64))).unwrap(),
            Some("a".repeat(64))
        );
        for value in [
            "a".repeat(63),
            "a".repeat(65),
            "A".repeat(64),
            format!("{}\n", "a".repeat(64)),
            "not-a-source".into(),
        ] {
            assert_eq!(
                source_id_value(OsStr::new(&value)).unwrap_err(),
                "service-source-handoff-invalid"
            );
        }
    }

    fn source_handoff_fixture(base: &ServicePaths) {
        let root = base.runtime_root.join("source-handoff");
        let config = root.join("config");
        let run = root.join("run");
        fs::create_dir_all(&config).unwrap();
        for name in [CAKE_PACKAGE, SQM_PACKAGE] {
            fs::copy(base.config_root.join(name), config.join(name)).unwrap();
        }
        let capture = || CommittedSnapshot::capture(&config, &run, &base.uci).unwrap();
        let first = capture();
        let source = check_source_handoff(&first, None).unwrap();
        let fingerprint = source.fingerprint().unwrap();
        drop(first);
        check_source_handoff(&capture(), Some(&fingerprint)).unwrap();
        // A same-byte UCI commit still replaces the source identity; neither
        // preflight nor the restored-receipt lane may silently adopt it.
        fs::copy(config.join(CAKE_PACKAGE), root.join("user-commit")).unwrap();
        fs::rename(root.join("user-commit"), config.join(CAKE_PACKAGE)).unwrap();
        let replaced = capture();
        assert_eq!(
            check_source_handoff(&replaced, Some(&fingerprint)).unwrap_err(),
            "service-source-changed-since-preflight"
        );
        assert!(source.attest_snapshot(&replaced).is_err());
    }

    fn unregistered_start_recovery_fixture(base: &ServicePaths, resolver: &impl InterfaceResolver) {
        for case in 0..9 {
            let root = base.runtime_root.join(format!("orphan-recovery-{case}"));
            let config_root = root.join("config");
            fs::create_dir_all(&config_root).unwrap();
            let cake = format!(
                "{} option sqm_section 'replacement'\n",
                fs::read_to_string(base.config_root.join(CAKE_PACKAGE)).unwrap()
            );
            let sqm = format!("{}\nconfig queue 'cake_lab'\n option _cake_autorate_managed 'lab'\n option interface 'old0'\n option enabled '1'\n option download '20000'\n option upload '10000'\n option qdisc 'cake'\n option script 'piece_of_cake.qos'\n", fs::read_to_string(base.config_root.join(SQM_PACKAGE)).unwrap());
            fs::write(config_root.join(CAKE_PACKAGE), cake).unwrap();
            let sqm = if case == 8 {
                sqm.replace(
                    "option interface 'old0'",
                    "list interface 'old0'\n list interface 'old1'",
                )
            } else {
                sqm
            };
            fs::write(config_root.join(SQM_PACKAGE), sqm).unwrap();
            let paths = ServicePaths {
                config_root,
                runtime_root: root.join("run"),
                ..base.clone()
            };
            if case == 0 {
                recover_unregistered_publication(&paths, |_| {
                    panic!("ordinary legacy Stop must not require absence without an orphan")
                })
                .unwrap();
                continue;
            }
            let candidate = StartCandidate::prepare(
                CommittedSnapshot::capture(&paths.config_root, &paths.runtime_root, &paths.uci)
                    .unwrap(),
                resolver,
                &paths.uci,
            )
            .unwrap();
            assert!(!candidate
                .config
                .package(SQM_PACKAGE)
                .unwrap()
                .sections
                .contains_key("cake_lab"));
            if case == 6 {
                let start = StartPublication::publish(&paths, candidate).unwrap();
                recover_unregistered_publication(&paths, |_| {
                    panic!("registered batches belong to ordinary Stop")
                })
                .unwrap();
                start.batch.attest().unwrap();
                let batch = start.batch.clone();
                drop(start);
                let snapshot =
                    CommittedSnapshot::capture(&paths.config_root, &paths.runtime_root, &paths.uci)
                        .unwrap();
                let cookie = check_source_handoff(&snapshot, None)
                    .unwrap()
                    .fingerprint()
                    .unwrap();
                assert_eq!(
                    finish_stopped_publication(
                        &paths.config_root,
                        &paths.uci,
                        &snapshot,
                        &batch,
                        |targets| {
                            if targets.iter().any(|spec| spec.target_interface == "old0") {
                                Err("registered-old-target-live".into())
                            } else {
                                Ok(())
                            }
                        }
                    )
                    .unwrap_err(),
                    "registered-old-target-live"
                );
                batch.attest().unwrap();
                let restored = finish_stopped_publication(
                    &paths.config_root,
                    &paths.uci,
                    &snapshot,
                    &batch,
                    |_| Ok(()),
                )
                .unwrap()
                .unwrap();
                drop(snapshot);
                let snapshot =
                    CommittedSnapshot::capture(&paths.config_root, &paths.runtime_root, &paths.uci)
                        .unwrap();
                assert!(check_source_handoff(&snapshot, Some(&cookie)).is_err());
                restored.attest_snapshot(&snapshot).unwrap();
                check_source_handoff(&snapshot, None).unwrap();
                continue;
            }
            let mut original = [CAKE_PACKAGE, SQM_PACKAGE]
                .map(|name| fs::read(paths.config_root.join(name)).unwrap());
            // Simulate exit after the real file transaction but before any
            // complete desired-set publication or runtime action.
            let published = super::super::uci_transaction::publish(candidate.config).unwrap();
            if case == 3 {
                published.accept().unwrap();
                original = [CAKE_PACKAGE, SQM_PACKAGE]
                    .map(|name| fs::read(paths.config_root.join(name)).unwrap());
            } else {
                drop(published);
            }
            let input_root = paths.runtime_root.join(".controller-input");
            if matches!(case, 2 | 3 | 4) {
                fs::create_dir(&input_root).unwrap();
                fs::set_permissions(&input_root, fs::Permissions::from_mode(0o700)).unwrap();
                let draft = if case == 4 {
                    b"foreign metadata".as_slice()
                } else {
                    b"cake-autorate controller batch v1\n{\"schema\":".as_slice()
                };
                fs::write(input_root.join("batch.pending.tmp"), draft).unwrap();
                fs::set_permissions(
                    input_root.join("batch.pending.tmp"),
                    fs::Permissions::from_mode(0o600),
                )
                .unwrap();
            }
            if case == 5 {
                fs::write(
                    root.join("foreign"),
                    b"# later user commit\nconfig globals 'globals'\n",
                )
                .unwrap();
                fs::rename(root.join("foreign"), paths.config_root.join(CAKE_PACKAGE)).unwrap();
            }
            let current = [CAKE_PACKAGE, SQM_PACKAGE]
                .map(|name| fs::read(paths.config_root.join(name)).unwrap());
            if case == 7 {
                // A registration appearing after the first fence revokes
                // orphan recovery, even before its contents can be interpreted.
                assert!(recover_unregistered_publication(&paths, |_| {
                    fs::create_dir(&input_root).unwrap();
                    fs::set_permissions(&input_root, fs::Permissions::from_mode(0o700)).unwrap();
                    fs::write(
                        input_root.join("batch.pending"),
                        b"unexpected published root",
                    )
                    .unwrap();
                    fs::set_permissions(
                        input_root.join("batch.pending"),
                        fs::Permissions::from_mode(0o600),
                    )
                    .unwrap();
                    Ok(())
                })
                .is_err());
                for (name, bytes) in [CAKE_PACKAGE, SQM_PACKAGE].into_iter().zip(current) {
                    assert_eq!(fs::read(paths.config_root.join(name)).unwrap(), bytes);
                }
                assert_eq!(
                    fs::read(input_root.join("batch.pending")).unwrap(),
                    b"unexpected published root"
                );
                continue;
            }
            if matches!(case, 4 | 5 | 8) {
                assert!(recover_unregistered_publication(&paths, |_| Ok(())).is_err());
                for (name, bytes) in [CAKE_PACKAGE, SQM_PACKAGE].into_iter().zip(current) {
                    assert_eq!(fs::read(paths.config_root.join(name)).unwrap(), bytes);
                }
                if case == 4 {
                    assert_eq!(
                        fs::read(input_root.join("batch.pending.tmp")).unwrap(),
                        b"foreign metadata"
                    );
                }
                continue;
            }
            if case != 3 {
                let mut saw_removed = false;
                assert_eq!(
                    recover_unregistered_publication(&paths, |specs| {
                        if specs.iter().any(|spec| spec.target_interface == "old0") {
                            saw_removed = true;
                            return Err("removed-target-still-live".into());
                        }
                        Ok(())
                    })
                    .unwrap_err(),
                    "removed-target-still-live"
                );
                assert!(saw_removed);
                for (name, bytes) in [CAKE_PACKAGE, SQM_PACKAGE].into_iter().zip(current) {
                    assert_eq!(fs::read(paths.config_root.join(name)).unwrap(), bytes);
                }
            }
            let mut observed = BTreeSet::new();
            recover_unregistered_publication(&paths, |specs| {
                observed.extend(specs.iter().map(|spec| spec.target_interface.clone()));
                Ok(())
            })
            .unwrap();
            assert!(observed.contains("fixture0"));
            if case != 3 {
                assert!(observed.contains("old0"));
            }
            for (name, bytes) in [CAKE_PACKAGE, SQM_PACKAGE].into_iter().zip(original) {
                assert_eq!(fs::read(paths.config_root.join(name)).unwrap(), bytes);
            }
            assert!(!input_root.join("batch.pending.tmp").exists());
            assert!(!super::super::uci_transaction::recovery_pending(&paths.config_root).unwrap());
        }
    }

    fn ordinary_stop_recovery_fixture(base: &ServicePaths, resolver: &impl InterfaceResolver) {
        for case in 0..4 {
            let root = base.runtime_root.join(format!("stop-recovery-{case}"));
            let config_root = root.join("config");
            fs::create_dir_all(&config_root).unwrap();
            for name in [CAKE_PACKAGE, SQM_PACKAGE] {
                fs::copy(base.config_root.join(name), config_root.join(name)).unwrap();
            }
            let paths = ServicePaths {
                config_root,
                runtime_root: root.join("run"),
                ..base.clone()
            };
            let capture = || {
                CommittedSnapshot::capture(&paths.config_root, &paths.runtime_root, &paths.uci)
                    .unwrap()
            };
            let original = [CAKE_PACKAGE, SQM_PACKAGE].map(|name| {
                let path = paths.config_root.join(name);
                (fs::metadata(&path).unwrap().ino(), fs::read(path).unwrap())
            });
            let start = StartPublication::publish(
                &paths,
                StartCandidate::prepare(capture(), resolver, &paths.uci).unwrap(),
            )
            .unwrap();
            let batch = start.batch.clone();
            let published = [CAKE_PACKAGE, SQM_PACKAGE]
                .map(|name| fs::read(paths.config_root.join(name)).unwrap());
            drop(start);
            if case == 3 {
                let foreign = paths.config_root.join("foreign");
                fs::write(
                    &foreign,
                    b"# later committed change\nconfig globals 'globals'\n",
                )
                .unwrap();
                fs::rename(foreign, paths.config_root.join(CAKE_PACKAGE)).unwrap();
                let current = [CAKE_PACKAGE, SQM_PACKAGE]
                    .map(|name| fs::read(paths.config_root.join(name)).unwrap());
                assert!(finish_stopped_publication(
                    &paths.config_root,
                    &paths.uci,
                    &capture(),
                    &batch,
                    |_| Ok(())
                )
                .is_err());
                for (name, bytes) in [CAKE_PACKAGE, SQM_PACKAGE].into_iter().zip(current) {
                    assert_eq!(fs::read(paths.config_root.join(name)).unwrap(), bytes);
                }
                batch.attest_record().unwrap();
                assert!(paths.config_root.join(".start-uci/pending").exists());
                continue;
            }
            let committed = capture();
            if case == 1 {
                assert_eq!(
                    finish_stopped_publication(
                        &paths.config_root,
                        &paths.uci,
                        &committed,
                        &batch,
                        |_| Err("runtime-still-present".into())
                    )
                    .unwrap_err(),
                    "runtime-still-present"
                );
                for (name, bytes) in [CAKE_PACKAGE, SQM_PACKAGE].into_iter().zip(&published) {
                    assert_eq!(fs::read(paths.config_root.join(name)).unwrap(), *bytes);
                }
                batch.attest_record().unwrap();
            }
            if case == 2 {
                // Lose the final runtime proof after file rollback, before
                // retiring the batch. A subsequent explicit Stop uses restored
                // public sources and the still-sealed pending Stop recipes.
                let mut failed_after_retirement = false;
                assert!(finish_stopped_publication(
                    &paths.config_root,
                    &paths.uci,
                    &committed,
                    &batch,
                    |_| {
                        if !paths.config_root.join(".start-uci/pending").exists() {
                            failed_after_retirement = true;
                            Err("runtime-returned".into())
                        } else {
                            Ok(())
                        }
                    }
                )
                .is_err());
                assert!(failed_after_retirement);
                batch.attest_record().unwrap();
                assert!(!paths.config_root.join(".start-uci/pending").exists());
                drop(committed);
                finish_stopped_publication(
                    &paths.config_root,
                    &paths.uci,
                    &capture(),
                    &batch,
                    |_| Ok(()),
                )
                .unwrap();
            } else {
                finish_stopped_publication(
                    &paths.config_root,
                    &paths.uci,
                    &committed,
                    &batch,
                    |_| Ok(()),
                )
                .unwrap();
                drop(committed);
            }
            for (name, (inode, bytes)) in [CAKE_PACKAGE, SQM_PACKAGE].into_iter().zip(original) {
                assert_eq!(
                    fs::metadata(paths.config_root.join(name)).unwrap().ino(),
                    inode
                );
                assert_eq!(fs::read(paths.config_root.join(name)).unwrap(), bytes);
            }
            assert!(super::super::controller_input::load_batch(
                &paths.runtime_root.join(".controller-input"),
                &paths.config_root
            )
            .unwrap()
            .is_none());
            let next = StartPublication::publish(
                &paths,
                StartCandidate::prepare(capture(), resolver, &paths.uci).unwrap(),
            )
            .unwrap();
            next.attest(resolver).unwrap();
        }
    }

    #[cfg(feature = "calibration")]
    fn bootstrap_generation_rollback_fixture(parent: &Path, uci: &Path, marker_only: bool) {
        use super::super::autotune_apply_runtime::{
            NativeApplyGlobalLock, NativeApplyTransactionPaths,
        };
        use super::super::autotune_bootstrap_apply::{
            NativeBootstrapApplyMode, NativeBootstrapApplyPlan,
        };
        use super::super::autotune_bootstrap_apply_recovery::{
            NativeBootstrapApplyRecoveryRecord, NativeBootstrapApplyRecoveryState,
            NativeBootstrapApplyRecoveryStore,
        };
        use super::super::autotune_bootstrap_apply_runtime::{
            execute_native_bootstrap_apply_commit, recover_native_bootstrap_apply,
            NativeBootstrapApplyBackend,
        };
        use super::super::autotune_runtime::AbsentRuntimeBaseline;
        use super::super::protocol::OperationRequest;

        let root = parent.join(if marker_only {
            "bootstrap-generation-marker-recovery"
        } else {
            "bootstrap-generation-rollback"
        });
        fs::create_dir(&root).unwrap();
        let config = root.join("config");
        fs::create_dir(&config).unwrap();
        let cake_original = b"config cake_autorate 'peer'\n option enabled '1'\n option manage_sqm '0'\n option sqm_enabled '0'\n option ul_if 'eth8'\n option dl_if 'ifb8'\n".to_vec();
        let sqm_original = b"# no bootstrap queue\n".to_vec();
        fs::write(config.join(CAKE_PACKAGE), &cake_original).unwrap();
        fs::write(config.join(SQM_PACKAGE), &sqm_original).unwrap();
        let runtime = root.join("run");
        let snapshot = CommittedSnapshot::capture(&config, &runtime, uci).unwrap();
        let published =
            super::super::uci_transaction::publish(snapshot.prepare([&[], &[]], uci).unwrap())
                .unwrap();
        let inputs = runtime.join(".controller-input");
        let peer =
            super::super::controller_input::store(&published, "peer", &inputs, &config).unwrap();
        let batch = super::super::controller_input::store_batch(
            &published,
            &["peer".into()],
            &BTreeMap::from([("peer".into(), peer.clone())]),
            &inputs,
            &config,
        )
        .unwrap();
        drop(published);
        batch.accept(|_| Ok(())).unwrap();
        let peer_generations = batch.generations().clone();
        let proc_root = root.join("proc");
        fs::create_dir_all(proc_root.join("100")).unwrap();
        fs::write(
            proc_root.join("100/cmdline"),
            b"/usr/sbin/cake-autorated\0--instance\0peer\0",
        )
        .unwrap();
        fs::write(
            proc_root.join("100/stat"),
            b"100 (cake) S 1 100 100 0 0 0 0 0 0 0 0 0 0 0 0 0 1 0 1000\n",
        )
        .unwrap();
        fs::write(
            proc_root.join("100/environ"),
            format!(
                "{}={}\0",
                super::super::controller_input::GENERATION_ENV,
                peer.generation()
            ),
        )
        .unwrap();
        let procd = serde_json::to_vec(&serde_json::json!({"cake-autorate":{"instances":{"peer":{
            "command":[DAEMON_PATH,"--instance","peer"],"pid":100,"running":true,
            "env":{super::super::controller_input::GENERATION_ENV:peer.generation()}
        }}}}))
        .unwrap();
        let response = root.join("procd.json");
        fs::write(&response, &procd).unwrap();
        let ubus = root.join("ubus");
        write_executable(&ubus, &format!("#!/bin/sh\ncat '{}'\n", response.display()));
        let paths = ServicePaths {
            config_root: config.clone(),
            runtime_root: runtime.clone(),
            proc_root,
            uci: uci.into(),
            ubus,
            tc: root.join("unused-tc"),
            runtime_lock_root: root.join("locks"),
            bridger_init: root.join("unused-init"),
            bridger_config: root.join("unused-bridger"),
            uci_workspace_root: root.join("uci-work"),
        };
        struct Backend<'a> {
            paths: &'a ServicePaths,
            originals: [&'a [u8]; 2],
            fail_settle: bool,
            restarts: usize,
            quiesces: usize,
        }
        impl Backend<'_> {
            fn original(&self) -> Result<(), String> {
                for (name, expected) in [CAKE_PACKAGE, SQM_PACKAGE].into_iter().zip(self.originals)
                {
                    if fs::read(self.paths.config_root.join(name))
                        .map_err(|_| "fixture read failed")?
                        != expected
                    {
                        return Err("fixture original baseline not restored".into());
                    }
                }
                Ok(())
            }
        }
        impl NativeBootstrapApplyBackend for Backend<'_> {
            fn candidate_already_applied(
                &mut self,
                _: &NativeBootstrapApplyPlan,
            ) -> Result<bool, String> {
                Ok(false)
            }
            fn attest_absent_before_mutation(
                &mut self,
                plan: &NativeBootstrapApplyPlan,
            ) -> Result<(), String> {
                self.original()?;
                require_bootstrap_slot_absent_in(self.paths, &plan.request().identity.instance)
            }
            fn attest_candidate_before_restart(
                &mut self,
                _: &NativeBootstrapApplyPlan,
            ) -> Result<(), String> {
                Ok(())
            }
            fn restart_service(
                &mut self,
                request: &OperationRequest,
                _: &NativeApplyGlobalLock,
            ) -> Result<(), String> {
                self.restarts += 1;
                let snapshot = CommittedSnapshot::capture(
                    &self.paths.config_root,
                    &self.paths.runtime_root,
                    &self.paths.uci,
                )?;
                let cake = snapshot.package(CAKE_PACKAGE)?.clone();
                let sqm = snapshot.package(SQM_PACKAGE)?.clone();
                drop(snapshot);
                let selected = prepare_selected_generation_with_paths(
                    self.paths,
                    &request.identity.instance,
                    &cake,
                    &sqm,
                    true,
                )?
                .ok_or("fixture generation missing")?;
                selected
                    .prepare_registration()?
                    .ok_or("fixture generation not registered")?;
                // Inject failure after actual pending-batch publication but
                // before any controller or qdisc is started in this fixture.
                Ok(())
            }
            fn verify_applied(&mut self, _: &NativeBootstrapApplyPlan) -> Result<(), String> {
                Err("injected candidate failure".into())
            }
            fn discard_pending_uci_changes(&mut self) -> Result<(), String> {
                Ok(())
            }
            fn quiesce_candidate_before_rollback(
                &mut self,
                request: &OperationRequest,
                _: &NativeBootstrapApplyRecoveryRecord,
                _: &NativeApplyGlobalLock,
            ) -> Result<(), String> {
                self.quiesces += 1;
                let snapshot = CommittedSnapshot::capture(
                    &self.paths.config_root,
                    &self.paths.runtime_root,
                    &self.paths.uci,
                )?;
                if !snapshot
                    .package(CAKE_PACKAGE)?
                    .sections
                    .contains_key(&request.identity.instance)
                {
                    return Err("candidate context missing at quiesce".into());
                }
                Ok(())
            }
            fn attest_rollforward_before_restart(
                &mut self,
                _: &OperationRequest,
                _: &NativeBootstrapApplyRecoveryRecord,
            ) -> Result<(), String> {
                Err("unexpected roll-forward".into())
            }
            fn verify_absent_restored(
                &mut self,
                _: &OperationRequest,
                _: &AbsentRuntimeBaseline,
            ) -> Result<(), String> {
                self.original()
            }
            fn settle_absent_controller_generation(
                &mut self,
                request: &OperationRequest,
                _: &AbsentRuntimeBaseline,
                _: &NativeApplyGlobalLock,
            ) -> Result<(), String> {
                self.original()?;
                if self.fail_settle {
                    return Err("injected generation settlement failure".into());
                }
                settle_absent_bootstrap_generation_in(
                    self.paths,
                    &request.identity.instance,
                    || self.original(),
                )
            }
            fn verify_recovered_candidate(
                &mut self,
                _: &OperationRequest,
                _: &NativeBootstrapApplyRecoveryRecord,
            ) -> Result<(), String> {
                Err("unexpected accepted recovery".into())
            }
            fn emergency_contain(
                &mut self,
                _: &OperationRequest,
                _: NativeBootstrapApplyMode,
                _: &NativeApplyGlobalLock,
            ) -> Result<(), String> {
                Ok(())
            }
        }
        let plan = super::super::autotune_bootstrap_apply::tests::fixture_plan();
        require_bootstrap_slot_absent_in(&paths, &plan.request().identity.instance).unwrap();
        assert!(require_bootstrap_slot_absent_in(&paths, "peer").is_err());
        let recovery = root.join("native-recovery");
        let lock = root.join("locks/runtime.guard");
        let cake_path = config.join(CAKE_PACKAGE);
        let sqm_path = config.join(SQM_PACKAGE);
        let transaction_paths = || NativeApplyTransactionPaths {
            recovery_root: &recovery,
            global_lock: &lock,
            cake_config: &cake_path,
            sqm_config: &sqm_path,
        };
        let mut backend = Backend {
            paths: &paths,
            originals: [&cake_original, &sqm_original],
            fail_settle: true,
            restarts: 0,
            quiesces: 0,
        };
        let error = execute_native_bootstrap_apply_commit(
            &plan,
            &plan.canonical_manifest_bytes().unwrap(),
            transaction_paths(),
            &mut backend,
        )
        .unwrap_err();
        assert!(error.contains("generation settlement failure"), "{error}");
        let store = NativeBootstrapApplyRecoveryStore::new(&recovery);
        assert_eq!(
            store.read_record().unwrap().unwrap().state,
            NativeBootstrapApplyRecoveryState::Restored
        );
        assert!(super::super::controller_input::load_batch(&inputs, &config)
            .unwrap()
            .unwrap()
            .pending());
        backend.original().unwrap();
        let pending_path = inputs.join("batch.pending");
        let pending_bytes = fs::read(&pending_path).unwrap();
        for wrong in ["other_instance", "peer"] {
            assert!(
                settle_absent_bootstrap_generation_in(&paths, wrong, || backend.original())
                    .is_err()
            );
        }
        assert_eq!(
            settle_absent_bootstrap_generation_in(
                &paths,
                &plan.request().identity.instance,
                || Err("source-drift".into())
            )
            .unwrap_err(),
            "source-drift"
        );
        let mut wrong_peer: serde_json::Value = serde_json::from_slice(&procd).unwrap();
        wrong_peer["cake-autorate"]["instances"]["peer"]["running"] = false.into();
        fs::write(&response, serde_json::to_vec(&wrong_peer).unwrap()).unwrap();
        assert!(settle_absent_bootstrap_generation_in(
            &paths,
            &plan.request().identity.instance,
            || backend.original()
        )
        .is_err());
        fs::write(&response, &procd).unwrap();
        assert_eq!(fs::read(&pending_path).unwrap(), pending_bytes);
        if marker_only {
            let _owner = NativeApplyGlobalLock::acquire(&lock).unwrap();
            let pending = super::super::controller_input::load_batch(&inputs, &config)
                .unwrap()
                .unwrap();
            pending
                .begin_restore(|generations| {
                    backend.original()?;
                    attest_selected_peer_processes(
                        &paths,
                        generations,
                        &plan.request().identity.instance,
                        false,
                    )
                })
                .unwrap();
            // Simulate interruption at cleanup_restore's marker-last window.
            fs::remove_file(&pending_path).unwrap();
            assert!(super::super::controller_input::restore_pending(&inputs).unwrap());
        }
        backend.fail_settle = false;
        let result = recover_native_bootstrap_apply(transaction_paths(), &mut backend)
            .unwrap()
            .unwrap();
        assert!(result.recovery_cleared && !result.rolled_forward);
        assert_eq!(
            backend.restarts, 1,
            "metadata recovery must not restart the restored candidate"
        );
        assert_eq!(
            backend.quiesces, 1,
            "metadata recovery must not quiesce again"
        );
        let restored = super::super::controller_input::load_batch(&inputs, &config)
            .unwrap()
            .unwrap();
        assert!(!restored.pending());
        assert!(restored.pending_update().unwrap().is_none());
        assert!(!super::super::controller_input::restore_pending(&inputs).unwrap());
        assert_eq!(restored.generations(), &peer_generations);
        assert_eq!(fs::read(&response).unwrap(), procd);
        peer.attest_applied().unwrap();
        backend.original().unwrap();
        require_no_pending_start_in(&config, &runtime).unwrap();
        settle_absent_bootstrap_generation_in(&paths, &plan.request().identity.instance, || {
            backend.original()
        })
        .unwrap();
    }

    #[cfg(feature = "calibration")]
    fn selected_generation_registration_fixture(
        root: &Path,
        config: &Path,
        uci: &Path,
        resolver: &impl InterfaceResolver,
    ) {
        let runtime = root.join("selected-generation");
        fs::create_dir(&runtime).unwrap();
        let snapshot = CommittedSnapshot::capture(config, &runtime, uci).unwrap();
        let candidate = StartCandidate::prepare(snapshot, resolver, uci).unwrap();
        let published = super::super::uci_transaction::publish(candidate.config).unwrap();
        let old_cake = published.config().package(CAKE_PACKAGE).unwrap().clone();
        let old_sqm = published.config().package(SQM_PACKAGE).unwrap().clone();
        let old_bytes = fs::read(config.join(CAKE_PACKAGE)).unwrap();
        let inputs = runtime.join(".controller-input");
        let old_input =
            super::super::controller_input::store(&published, "lab", &inputs, config).unwrap();
        let batch = super::super::controller_input::store_batch(
            &published,
            &["lab".into()],
            &BTreeMap::from([("lab".into(), old_input.clone())]),
            &inputs,
            config,
        )
        .unwrap();
        assert!(require_no_pending_start_in(config, &runtime).is_err());
        // Fixture-only publication acceptance isolates the still-pending
        // controller batch from the separately pending UCI file journal.
        let receipt = published.source_receipt().unwrap();
        drop(published);
        super::super::uci_transaction::accept_receipt(config, &receipt, || Ok(())).unwrap();
        assert_eq!(
            require_no_pending_start_in(config, &runtime).unwrap_err(),
            "native-apply-controller-generation-recovery-pending"
        );
        batch.accept(|_| Ok(())).unwrap();
        require_no_pending_start_in(config, &runtime).unwrap();
        let proc_root = runtime.join("proc");
        fs::create_dir(&proc_root).unwrap();
        fs::write(proc_root.join("stat"), b"btime 100\n").unwrap();
        let set_process = |id: &str| {
            fs::create_dir_all(proc_root.join("100")).unwrap();
            fs::write(
                proc_root.join("100/cmdline"),
                b"/usr/sbin/cake-autorated\0--instance\0lab\0",
            )
            .unwrap();
            let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
            fs::write(
                proc_root.join("100/stat"),
                format!(
                    "100 (cake) S 1 100 100 0 0 0 0 0 0 0 0 0 0 0 0 0 1 0 {}\n",
                    ticks * 10
                ),
            )
            .unwrap();
            fs::write(
                proc_root.join("100/environ"),
                format!("{}={id}\0", super::super::controller_input::GENERATION_ENV),
            )
            .unwrap();
        };
        set_process(old_input.generation());
        let response = runtime.join("procd.json");
        let old_response = serde_json::json!({"cake-autorate":{"instances":{"lab":{
            "command":[DAEMON_PATH,"--instance","lab"],"pid":100,"running":true,
            "env":{super::super::controller_input::GENERATION_ENV:old_input.generation()}
        }}}});
        fs::write(&response, serde_json::to_vec(&old_response).unwrap()).unwrap();
        let ubus = runtime.join("ubus");
        write_executable(&ubus, &format!("#!/bin/sh\ncat '{}'\n", response.display()));
        let paths = ServicePaths {
            config_root: config.into(),
            uci: uci.into(),
            tc: runtime.join("unused-tc"),
            ubus,
            proc_root: proc_root.clone(),
            runtime_root: runtime.clone(),
            runtime_lock_root: runtime.join("locks"),
            bridger_init: runtime.join("unused-bridger"),
            bridger_config: runtime.join("unused-bridger-config"),
            uci_workspace_root: runtime.join("uci-work"),
        };
        let operation_input =
            super::super::controller_input::load("lab", old_input.generation(), &inputs, config)
                .unwrap();
        let operation_cfg = operation_input.config.clone();
        let operation_sqm = super::super::sqm_identity::managed_sqm_identity_from_input(
            &operation_input,
            "lab",
            &operation_cfg.sqm_section,
            &operation_cfg.sqm_interface,
        )
        .unwrap();
        attest_operation_applied_sqm_in(&paths, "lab", &operation_cfg, &operation_sqm).unwrap();
        assert!(
            attest_operation_applied_sqm_in(&paths, "lab", &operation_cfg, &"0".repeat(64))
                .is_err()
        );
        let mut unapplied_route = operation_cfg.clone();
        unapplied_route.route_mode = if operation_cfg.route_mode == "primary_wan" {
            "mwan3_member"
        } else {
            "primary_wan"
        }
        .into();
        assert!(
            attest_operation_applied_sqm_in(&paths, "lab", &unapplied_route, &operation_sqm)
                .is_err()
        );
        let mut test_policy = operation_cfg.clone();
        for change_dns in [false, true] {
            let mut unapplied_authority = operation_cfg.clone();
            if change_dns {
                unapplied_authority.explicit_dns_server = Some("192.0.2.53".parse().unwrap());
            } else {
                unapplied_authority.explicit_route_authority =
                    crate::routing::ExplicitRouteAuthority::from_fields(
                        "explicit",
                        ["192.0.2.2", "101", "0x100", "0x3f00"],
                    )
                    .unwrap();
            }
            assert!(attest_operation_applied_sqm_in(
                &paths,
                "lab",
                &unapplied_authority,
                &operation_sqm
            )
            .is_err());
        }
        test_policy.speedtest_backend = if operation_cfg.speedtest_backend == "auto" {
            "ookla"
        } else {
            "auto"
        }
        .into();
        attest_operation_applied_sqm_in(&paths, "lab", &test_policy, &operation_sqm).unwrap();
        let mut wrong_generation = old_response.clone();
        wrong_generation["cake-autorate"]["instances"]["lab"]["env"] =
            serde_json::json!({super::super::controller_input::GENERATION_ENV:"0".repeat(64)});
        fs::write(&response, serde_json::to_vec(&wrong_generation).unwrap()).unwrap();
        assert!(
            attest_operation_applied_sqm_in(&paths, "lab", &operation_cfg, &operation_sqm).is_err()
        );
        fs::write(&response, serde_json::to_vec(&old_response).unwrap()).unwrap();
        set_process(&"0".repeat(64));
        assert!(
            attest_operation_applied_sqm_in(&paths, "lab", &operation_cfg, &operation_sqm).is_err()
        );
        set_process(old_input.generation());
        let saved = runtime.join("saved-inputs");
        fs::rename(&inputs, &saved).unwrap();
        assert_eq!(
            attest_operation_applied_sqm_in(&paths, "lab", &operation_cfg, &operation_sqm)
                .unwrap_err(),
            "operation-controller-batch-missing"
        );
        fs::rename(&saved, &inputs).unwrap();
        attest_operation_applied_sqm_in(&paths, "lab", &operation_cfg, &operation_sqm).unwrap();
        let reuse =
            prepare_selected_generation_with_paths(&paths, "lab", &old_cake, &old_sqm, true)
                .unwrap()
                .unwrap();
        assert!(matches!(reuse.mode, SelectedGenerationMode::Reuse));
        assert!(
            reuse.prepare_registration().is_err(),
            "selected controller must be stopped before registration"
        );
        assert!(proc_root.join("100").exists());
        drop(reuse);
        let snapshot = CommittedSnapshot::capture(config, &runtime, uci).unwrap();
        let edits = [super::super::uci_edits::Edit::Set {
            section: "lab".into(),
            option: "log_file_max_size_KB".into(),
            value: "1000".into(),
        }];
        let changed =
            super::super::uci_transaction::publish(snapshot.prepare([&edits, &[]], uci).unwrap())
                .unwrap()
                .accept()
                .unwrap();
        let new_cake = changed.config().package(CAKE_PACKAGE).unwrap().clone();
        let new_sqm = changed.config().package(SQM_PACKAGE).unwrap().clone();
        require_no_pending_start_in(config, &runtime).unwrap();
        drop(changed);
        let before_inputs = fs::read_dir(&inputs).unwrap().count();
        fs::create_dir(proc_root.join("101")).unwrap();
        fs::write(
            proc_root.join("101/cmdline"),
            b"/usr/sbin/cake-autorated\0--instance\0unexpected\0",
        )
        .unwrap();
        let unexpected_stat = fs::read_to_string(proc_root.join("100/stat"))
            .unwrap()
            .replacen("100 (cake) S 1 100 100", "101 (cake) S 1 101 101", 1);
        fs::write(proc_root.join("101/stat"), unexpected_stat).unwrap();
        assert!(
            prepare_selected_generation_with_paths(&paths, "lab", &new_cake, &new_sqm, true)
                .is_err()
        );
        assert_eq!(fs::read_dir(&inputs).unwrap().count(), before_inputs);
        assert!(proc_root.join("100").exists());
        fs::remove_dir_all(proc_root.join("101")).unwrap();
        let stage =
            prepare_selected_generation_with_paths(&paths, "lab", &new_cake, &new_sqm, true)
                .unwrap()
                .unwrap();
        assert!(matches!(stage.mode, SelectedGenerationMode::Replace));
        assert!(proc_root.join("100").exists());
        assert!(!inputs.join("batch.pending").exists());
        fs::remove_dir_all(proc_root.join("100")).unwrap();
        fs::write(&response, br#"{"cake-autorate":{"instances":{}}}"#).unwrap();
        let generation = stage.prepare_registration().unwrap().unwrap();
        assert_ne!(generation, old_input.generation());
        drop(stage);
        let retry =
            prepare_selected_generation_with_paths(&paths, "lab", &new_cake, &new_sqm, true)
                .unwrap()
                .unwrap();
        assert!(matches!(retry.mode, SelectedGenerationMode::Retry));
        assert_eq!(
            retry.prepare_registration().unwrap(),
            Some(generation.clone())
        );
        drop(retry);
        let candidate_bytes = fs::read(config.join(CAKE_PACKAGE)).unwrap();
        let permissions = fs::metadata(config.join(CAKE_PACKAGE))
            .unwrap()
            .permissions();
        let reinstalled = runtime.join("reinstalled-cake");
        fs::write(&reinstalled, &candidate_bytes).unwrap();
        fs::set_permissions(&reinstalled, permissions).unwrap();
        fs::rename(&reinstalled, config.join(CAKE_PACKAGE)).unwrap();
        let before_inputs = fs::read_dir(&inputs).unwrap().count();
        let mut different = candidate_bytes.clone();
        different.extend_from_slice(b"\n# concurrent user commit\n");
        fs::write(config.join(CAKE_PACKAGE), &different).unwrap();
        assert!(
            prepare_selected_generation_with_paths(&paths, "lab", &new_cake, &new_sqm, true)
                .is_err()
        );
        assert_eq!(fs::read_dir(&inputs).unwrap().count(), before_inputs);
        assert_eq!(fs::read(config.join(CAKE_PACKAGE)).unwrap(), different);
        // Restore the controlled fixture for the independent identical-content case.
        fs::write(config.join(CAKE_PACKAGE), candidate_bytes).unwrap();
        let supersede =
            prepare_selected_generation_with_paths(&paths, "lab", &new_cake, &new_sqm, true)
                .unwrap()
                .unwrap();
        assert!(matches!(supersede.mode, SelectedGenerationMode::Supersede));
        assert_ne!(supersede.prepare_registration().unwrap(), Some(generation));
        drop(supersede);
        // Parent-level rollback restores its own raw original file; the
        // accepted old input is reused despite the changed public inode/time.
        fs::write(config.join(CAKE_PACKAGE), old_bytes).unwrap();
        let partial_restore = inputs.join("batch.restore.tmp");
        fs::write(&partial_restore, b"restore-controller-").unwrap();
        fs::set_permissions(&partial_restore, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(super::super::controller_input::load_batch(&inputs, config).is_err());
        let restore =
            prepare_selected_generation_with_paths(&paths, "lab", &old_cake, &old_sqm, true)
                .unwrap()
                .unwrap();
        assert!(matches!(restore.mode, SelectedGenerationMode::Reuse));
        assert_eq!(
            restore.prepare_registration().unwrap(),
            Some(old_input.generation().to_string())
        );
        assert!(!partial_restore.exists());
        drop(restore);
        set_process(old_input.generation());
        fs::write(&response, serde_json::to_vec(&old_response).unwrap()).unwrap();
        fs::create_dir(runtime.join("lab")).unwrap();
        fs::write(
            runtime.join("lab/status.json"),
            br#"{"instance":"lab","state":"RUNNING","started_at":111,"updated_at":121}"#,
        )
        .unwrap();
        let authority =
            controller_start_authority(&OpenWrtEnvironment::production(), &paths).unwrap();
        authority.attest(&paths).unwrap();
        let current = super::super::controller_input::load_batch(&inputs, config)
            .unwrap()
            .unwrap();
        current
            .accept(|_| {
                authority.attest(&paths)?;
                assert_eq!(
                    observe_controller_start(&paths, &authority.instances, 123.0)?,
                    ControllerStartReadiness::Ready
                );
                Ok(())
            })
            .unwrap();
        assert!(!inputs.join("batch.pending").exists());
        assert!(!inputs.join("batch.restore").exists());
        let snapshot = CommittedSnapshot::capture(config, &runtime, uci).unwrap();
        let cake_edits = [
            super::super::uci_edits::Edit::Set {
                section: "lab".into(),
                option: "enabled".into(),
                value: "0".into(),
            },
            super::super::uci_edits::Edit::Set {
                section: "lab".into(),
                option: "sqm_enabled".into(),
                value: "0".into(),
            },
        ];
        let sqm_edits = [super::super::uci_edits::Edit::Set {
            section: "cake_lab".into(),
            option: "enabled".into(),
            value: "0".into(),
        }];
        let disabled = super::super::uci_transaction::publish(
            snapshot.prepare([&cake_edits, &sqm_edits], uci).unwrap(),
        )
        .unwrap()
        .accept()
        .unwrap();
        let disabled_cake = disabled.config().package(CAKE_PACKAGE).unwrap().clone();
        let disabled_sqm = disabled.config().package(SQM_PACKAGE).unwrap().clone();
        drop(disabled);
        let before = fs::read_dir(&inputs).unwrap().count();
        let removal = prepare_selected_generation_with_paths(
            &paths,
            "lab",
            &disabled_cake,
            &disabled_sqm,
            false,
        )
        .unwrap()
        .unwrap();
        assert!(matches!(removal.mode, SelectedGenerationMode::Remove));
        assert_eq!(
            fs::read_dir(&inputs).unwrap().count(),
            before,
            "disable preparation creates no new controller input"
        );
        assert!(removal.prepare_registration().is_err());
        assert!(proc_root.join("100").exists());
        fs::remove_dir_all(proc_root.join("100")).unwrap();
        fs::write(&response, br#"{"cake-autorate":{"instances":{}}}"#).unwrap();
        assert_eq!(removal.prepare_registration().unwrap(), None);
        drop(removal);
        let retry = prepare_selected_generation_with_paths(
            &paths,
            "lab",
            &disabled_cake,
            &disabled_sqm,
            false,
        )
        .unwrap()
        .unwrap();
        assert!(matches!(retry.mode, SelectedGenerationMode::Retry));
        assert_eq!(retry.prepare_registration().unwrap(), None);
        drop(retry);
        let authority =
            controller_start_authority(&OpenWrtEnvironment::production(), &paths).unwrap();
        assert!(authority.instances.is_empty());
        let current = super::super::controller_input::load_batch(&inputs, config)
            .unwrap()
            .unwrap();
        current
            .accept(|generations| {
                assert!(generations.is_empty());
                authority.attest(&paths)?;
                assert_eq!(
                    observe_controller_start(&paths, &authority.instances, 123.0)?,
                    ControllerStartReadiness::Ready
                );
                Ok(())
            })
            .unwrap();
        assert!(super::super::controller_input::load_batch(&inputs, config)
            .unwrap()
            .unwrap()
            .generations()
            .is_empty());
        set_process(old_input.generation());
        fs::write(
            proc_root.join("100/cmdline"),
            b"/usr/sbin/cake-autorated\0--instance\0lab\0--once\0",
        )
        .unwrap();
        // A different process's environment must never be read for GC.
        fs::create_dir(proc_root.join("101")).unwrap();
        fs::write(proc_root.join("101/cmdline"), b"/usr/bin/unrelated\0").unwrap();
        std::os::unix::fs::symlink(runtime.join("must-not-read"), proc_root.join("101/environ"))
            .unwrap();
        collect_controller_inputs(&paths).unwrap();
        old_input.attest_record().unwrap();
        assert_eq!(collect_controller_inputs(&paths).unwrap(), 0);
        fs::remove_dir_all(proc_root.join("100")).unwrap();
        let dormant = serde_json::json!({"cake-autorate":{"instances":{"lab":{
            "running":false,"env":{super::super::controller_input::GENERATION_ENV:old_input.generation()}
        }}}});
        fs::write(&response, serde_json::to_vec(&dormant).unwrap()).unwrap();
        assert_eq!(collect_controller_inputs(&paths).unwrap(), 0);
        old_input.attest_record().unwrap();
        fs::write(&response, br#"{"cake-autorate":{"instances":{}}}"#).unwrap();
        assert_eq!(collect_controller_inputs(&paths).unwrap(), 1);
        assert_eq!(collect_controller_inputs(&paths).unwrap(), 0);
    }

    #[test]
    #[ignore = "requires explicit inspected SDK UCI and loader paths"]
    fn r4_stop_real_uci_snapshot_preserves_both_savedirs_and_committed_target() {
        let root = test_root("stop-real-uci");
        let config = root.join("config");
        let sys = root.join("sys");
        let global = root.join("global");
        let rpcd = root.join("rpcd");
        let overrides = root.join("overrides");
        for path in [&config, &sys, &global, &rpcd, &overrides] {
            fs::create_dir(path).unwrap();
        }
        fs::create_dir(sys.join("fixture0")).unwrap();
        fs::write(config.join("cake-autorate"), "config cake_autorate 'lab'\n option enabled '1'\n option sqm_enabled '1'\n option sqm_interface 'fixture0'\n").unwrap();
        fs::write(config.join("sqm"), "config queue 'owned'\n option _cake_autorate_managed 'lab'\n option interface 'fixture0'\n").unwrap();
        for (path, value) in [(&global, "foreign-global"), (&rpcd, "foreign-rpcd")] {
            fs::write(
                path.join("cake-autorate"),
                format!("cake-autorate.lab.sqm_interface='{value}'\n"),
            )
            .unwrap();
            fs::write(path.join("sqm"), format!("sqm.owned.interface='{value}'\n")).unwrap();
        }
        let files = [&config, &global, &rpcd]
            .into_iter()
            .flat_map(|dir| [dir.join("cake-autorate"), dir.join("sqm")])
            .collect::<Vec<_>>();
        let before = files
            .iter()
            .map(|path| fs::read(path).unwrap())
            .collect::<Vec<_>>();
        fn quote(value: &std::ffi::OsStr) -> String {
            format!(
                "'{}'",
                value
                    .to_str()
                    .expect("test path must be UTF-8")
                    .replace('\'', "'\\''")
            )
        }
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
        let wrapper = root.join("uci-wrapper");
        write_executable(
            &wrapper,
            &format!(
                "#!/bin/sh\nexec {command} -c {} -C {} -p {} -p {} \"$@\"\n",
                quote(config.as_os_str()),
                quote(overrides.as_os_str()),
                quote(global.as_os_str()),
                quote(rpcd.as_os_str())
            ),
        );
        let paths = ServicePaths {
            config_root: config.clone(),
            uci: wrapper.clone(),
            tc: root.join("no-tc"),
            ubus: root.join("no-ubus"),
            proc_root: root.join("no-proc"),
            runtime_root: root.join("run"),
            runtime_lock_root: root.join("locks"),
            bridger_init: root.join("no-bridger"),
            bridger_config: root.join("no-bridger-config"),
            uci_workspace_root: root.join("uci-work"),
        };
        fs::create_dir(&paths.proc_root).unwrap();
        let environment = OpenWrtEnvironment::test_paths(wrapper, root.join("no-ubus"), sys);
        let mut backend = OpenWrtServiceStop {
            environment,
            paths,
            expected_source: None,
            restored_source: None,
            committed: None,
            applied: BTreeMap::new(),
            batch: None,
        };
        let plan = backend.snapshot().unwrap();
        assert_eq!(plan.managed.len(), 1);
        assert_eq!(plan.managed[0].target_interface, "fixture0");
        backend.attest_unchanged(&plan).unwrap();
        assert_eq!(
            files
                .iter()
                .map(|path| fs::read(path).unwrap())
                .collect::<Vec<_>>(),
            before
        );
        let mut replacement = fs::read(config.join("sqm")).unwrap();
        replacement.extend(b"# concurrent committed change\n");
        fs::write(config.join("sqm"), replacement).unwrap();
        assert!(backend.attest_unchanged(&plan).is_err());
        for (index, path) in files.iter().enumerate().skip(2) {
            assert_eq!(fs::read(path).unwrap(), before[index]);
        }
        drop(backend);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn r4_stop_backend_uses_one_committed_snapshot_not_original_name_uci() {
        let root = test_root("committed-stop");
        let config = root.join("config");
        let sys = root.join("sys");
        fs::create_dir(&config).unwrap();
        fs::create_dir(&sys).unwrap();
        fs::create_dir(sys.join("fixture0")).unwrap();
        fs::write(
            config.join("cake-autorate"),
            "config cake_autorate 'lab'\n option marker 'committed'\n",
        )
        .unwrap();
        fs::write(config.join("sqm"), "config queue 'owned'\n option _cake_autorate_managed 'lab'\n option interface 'fixture0'\n").unwrap();
        let pending = root.join("foreign-pending");
        fs::write(&pending, b"foreign staged bytes").unwrap();
        let uci = root.join("uci-fixture");
        write_executable(
            &uci,
            r#"#!/bin/sh
set -eu
[ "$#" = 10 ] && [ "$1:$3:$5:$7:$8:$9" = -c:-C:-t:-q:-X:show ] || exit 91
alias="${10}"
[ "${#alias}" = 32 ] || exit 92
printf '%s\n' "$alias" >> "${0%/*}/uci-calls"
if grep -q 'config cake_autorate' "$2/$alias"; then
  printf "%s.lab=cake_autorate\n%s.lab.marker='committed'\n" "$alias" "$alias"
else
  printf "%s.owned=queue\n%s.owned._cake_autorate_managed='lab'\n%s.owned.interface='fixture0'\n" "$alias" "$alias" "$alias"
fi
"#,
        );
        let paths = ServicePaths {
            config_root: config.clone(),
            uci: uci.clone(),
            tc: root.join("no-tc"),
            ubus: root.join("no-ubus"),
            proc_root: root.join("no-proc"),
            runtime_root: root.join("run"),
            runtime_lock_root: root.join("locks"),
            bridger_init: root.join("no-bridger"),
            bridger_config: root.join("no-bridger-config"),
            uci_workspace_root: root.join("uci-work"),
        };
        fs::create_dir(&paths.proc_root).unwrap();
        let environment = OpenWrtEnvironment::test_paths(uci, root.join("no-ubus"), sys);
        let mut backend = OpenWrtServiceStop {
            environment,
            paths,
            expected_source: None,
            restored_source: None,
            committed: None,
            applied: BTreeMap::new(),
            batch: None,
        };
        let plan = backend.snapshot().unwrap();
        assert_eq!(plan.managed.len(), 1);
        assert_eq!(plan.managed[0].target_interface, "fixture0");
        backend.attest_unchanged(&plan).unwrap();
        fs::write(&pending, b"new foreign staged bytes").unwrap();
        backend.attest_unchanged(&plan).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("uci-calls"))
                .unwrap()
                .lines()
                .count(),
            2,
            "attestation must not re-open original-name UCI views"
        );
        let replacement = config.join("replacement");
        fs::write(&replacement, fs::read(config.join("sqm")).unwrap()).unwrap();
        fs::rename(replacement, config.join("sqm")).unwrap();
        assert!(backend.attest_unchanged(&plan).is_err());
        assert_eq!(fs::read(pending).unwrap(), b"new foreign staged bytes");
        drop(backend);
        fs::remove_dir_all(root).unwrap();
    }
    use std::collections::BTreeMap;

    static TEST_SEQUENCE: AtomicU32 = AtomicU32::new(0);

    #[derive(Default)]
    struct Resolver(BTreeMap<String, String>);

    impl InterfaceResolver for Resolver {
        fn resolve(&self, name: &str) -> Result<String, String> {
            Ok(self
                .0
                .get(name)
                .cloned()
                .unwrap_or_else(|| name.to_string()))
        }
    }

    fn parse(package: &str, body: &str) -> UciPackage {
        UciPackage::parse(package, body).unwrap()
    }

    fn projection(conflicts: &[&str]) -> SqmProjectionPlan {
        SqmProjectionPlan::test_summary(
            true,
            BTreeSet::from(["wwan0".to_string()]),
            BTreeSet::from(["wwan0".to_string()]),
            conflicts.iter().map(|value| value.to_string()).collect(),
        )
    }

    fn test_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "cake-service-lifecycle-{label}-{}-{}",
            std::process::id(),
            TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        root
    }

    fn write_executable(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    struct ScriptedStartEvents {
        drains: std::collections::VecDeque<bool>,
        waits: std::collections::VecDeque<bool>,
    }

    impl StartEvents for ScriptedStartEvents {
        fn drain(&mut self) -> Result<bool, String> {
            Ok(self.drains.pop_front().expect("unexpected event drain"))
        }
        fn wait(&mut self, _: Instant) -> Result<bool, String> {
            Ok(self.waits.pop_front().expect("unexpected readiness wait"))
        }
    }

    fn start_spec(instance: &str) -> ManagedSqmAttestationSpec {
        ManagedSqmAttestationSpec {
            instance: instance.to_string(),
            sqm_section: format!("cake_{instance}"),
            target_interface: "eth1".to_string(),
            upload_interface: "eth1".to_string(),
            download_interface: "ifb4eth1".to_string(),
            direction_mode: "both".to_string(),
            minimum_download_kbps: 20_000,
            maximum_download_kbps: 20_000,
            minimum_upload_kbps: 21_202,
            maximum_upload_kbps: 21_202,
        }
    }

    #[test]
    fn sqm_start_hotplug_gap_requires_event_then_rechecks_every_instance() {
        let mut events = ScriptedStartEvents {
            drains: [false, false, true, false].into(),
            waits: [true].into(),
        };
        let specs = [start_spec("first"), start_spec("second")];
        let mut calls = Vec::new();
        let mut checks = 0;
        let result = await_sqm_start(
            &mut events,
            Instant::now() + Duration::from_secs(10),
            &specs,
            || {
                checks += 1;
                Ok(())
            },
            |spec| {
                calls.push(spec.instance.clone());
                if calls.len() == 2 {
                    Err(NativeSqmAttestationError::Failed(
                        "IFB counter missing in hotplug stop/start gap".into(),
                    ))
                } else {
                    Ok(false)
                }
            },
        )
        .unwrap();
        assert_eq!(result, [false, false]);
        assert_eq!(calls, ["first", "second", "first", "second"]);
        assert_eq!(checks, 4);
        assert!(events.waits.is_empty());
    }

    #[test]
    fn sqm_start_event_during_success_invalidates_the_whole_observation() {
        let mut events = ScriptedStartEvents {
            drains: [false, true, false, false].into(),
            waits: [].into(),
        };
        let mut calls = 0;
        let result = await_sqm_start(
            &mut events,
            Instant::now() + Duration::from_secs(10),
            &[start_spec("wan")],
            || Ok(()),
            |_| {
                calls += 1;
                Ok(calls == 2)
            },
        )
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(result, [true]); // Only the existing strict offline proof can return true.
    }

    #[test]
    fn sqm_start_timeout_never_accepts_foreign_or_missing_runtime() {
        let mut events = ScriptedStartEvents {
            drains: [false, false].into(),
            waits: [false].into(),
        };
        let mut calls = 0;
        let error = await_sqm_start(
            &mut events,
            Instant::now() + Duration::from_secs(10),
            &[start_spec("wan")],
            || Ok(()),
            |_| {
                calls += 1;
                Err(NativeSqmAttestationError::Failed("foreign runtime".into()))
            },
        )
        .unwrap_err();
        assert!(error.contains("foreign runtime"));
        assert_eq!(calls, 1); // No timer polling and no repeated service actions.
    }

    #[test]
    fn sqm_start_configuration_change_after_failed_observation_aborts_before_wait() {
        let mut events = ScriptedStartEvents {
            drains: [false].into(),
            waits: [].into(),
        };
        let mut checks = 0;
        let error = await_sqm_start(
            &mut events,
            Instant::now() + Duration::from_secs(10),
            &[start_spec("wan")],
            || {
                checks += 1;
                if checks == 2 {
                    Err("configuration changed".into())
                } else {
                    Ok(())
                }
            },
            |_| {
                Err(NativeSqmAttestationError::Failed(
                    "state is being replaced".into(),
                ))
            },
        )
        .unwrap_err();
        assert_eq!(error, "configuration changed");
        assert_eq!(checks, 2);
    }

    #[test]
    fn sqm_start_cancellation_and_busy_are_terminal_without_wait() {
        for error in [
            NativeSqmAttestationError::Terminated,
            NativeSqmAttestationError::Busy("owner busy".into()),
        ] {
            let mut events = ScriptedStartEvents {
                drains: [false].into(),
                waits: [].into(),
            };
            let mut calls = 0;
            assert!(await_sqm_start(
                &mut events,
                Instant::now() + Duration::from_secs(10),
                &[start_spec("wan")],
                || Ok(()),
                |_| {
                    calls += 1;
                    Err(error.clone())
                }
            )
            .is_err());
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn start_plan_preserves_manual_instances_and_requires_exact_managed_backing() {
        let cake = parse(
            CAKE_PACKAGE,
            "cake-autorate.manual=cake_autorate\ncake-autorate.manual.enabled='1'\ncake-autorate.manual.manage_sqm='0'\ncake-autorate.wan=cake_autorate\ncake-autorate.wan.enabled='1'\ncake-autorate.wan.manage_sqm='1'\ncake-autorate.wan.sqm_enabled='1'\ncake-autorate.wan.sqm_direction_mode='both'\ncake-autorate.wan.sqm_interface='wan'\ncake-autorate.wan.dl_if='ifb4wwan0'\ncake-autorate.wan.sqm_section='cake_wan'\n",
        );
        let sqm = parse(
            SQM_PACKAGE,
            "sqm.cake_wan=queue\nsqm.cake_wan.enabled='1'\nsqm.cake_wan.interface='wwan0'\nsqm.cake_wan._cake_autorate_managed='wan'\nsqm.cake_wan.download='500000'\nsqm.cake_wan.upload='100000'\n",
        );
        let resolver = Resolver(BTreeMap::from([("wan".to_string(), "wwan0".to_string())]));
        let plan = plan_start(&cake, &sqm, &resolver, &projection(&[])).unwrap();
        assert_eq!(plan.instances, ["manual", "wan"]);
        assert_eq!(plan.managed.len(), 1);
        assert_eq!(plan.managed[0].target_interface, "wwan0");
        assert_eq!(plan.managed[0].minimum_download_kbps, 500_000);
        #[cfg(feature = "calibration")]
        assert_eq!(
            encode_start_plan(&plan.instances, &[]),
            "service-start-v2 manual,wan -\n"
        );
        #[cfg(not(feature = "calibration"))]
        assert_eq!(
            encode_start_plan(&plan.instances, &[]),
            "service-start-v1 manual,wan\n"
        );
    }

    #[test]
    fn disabled_missing_foreign_and_conflicting_managed_queues_never_start() {
        let cake = parse(
            CAKE_PACKAGE,
            "cake-autorate.disabled=cake_autorate\ncake-autorate.disabled.enabled='1'\ncake-autorate.disabled.sqm_enabled='0'\ncake-autorate.missing=cake_autorate\ncake-autorate.missing.enabled='1'\ncake-autorate.missing.sqm_enabled='1'\ncake-autorate.missing.sqm_interface='wwan0'\ncake-autorate.missing.sqm_section='none'\ncake-autorate.conflict=cake_autorate\ncake-autorate.conflict.enabled='1'\ncake-autorate.conflict.manage_sqm='0'\n",
        );
        let plan = plan_start(
            &cake,
            &UciPackage::default(),
            &Resolver::default(),
            &projection(&["conflict"]),
        )
        .unwrap();
        assert!(plan.instances.is_empty());
        assert!(plan.managed.is_empty());
    }

    #[test]
    fn invalid_direction_and_rate_shape_fail_closed() {
        let cake = parse(
            CAKE_PACKAGE,
            "cake-autorate.wan=cake_autorate\ncake-autorate.wan.enabled='1'\ncake-autorate.wan.sqm_enabled='1'\ncake-autorate.wan.sqm_interface='wwan0'\ncake-autorate.wan.sqm_section='cake_wan'\ncake-autorate.wan.sqm_direction_mode='upload_only'\n",
        );
        let sqm = parse(
            SQM_PACKAGE,
            "sqm.cake_wan=queue\nsqm.cake_wan.enabled='1'\nsqm.cake_wan.interface='wwan0'\nsqm.cake_wan._cake_autorate_managed='wan'\nsqm.cake_wan.download='1'\nsqm.cake_wan.upload='100000'\n",
        );
        assert!(plan_start(&cake, &sqm, &Resolver::default(), &projection(&[]),).is_err());
    }

    #[test]
    fn r4_generation_start_response_is_versioned_bounded_and_never_embeds_configuration() {
        let id = "0123456789abcdef".repeat(4);
        let generations =
            BTreeMap::from([("wan".into(), id.clone()), ("backup".into(), id.clone())]);
        assert_eq!(
            encode_generation_start_plan(&generations, &[]).unwrap(),
            format!("service-start-v3 backup:{id},wan:{id} -\n")
        );
        assert_eq!(
            encode_generation_start_plan(&BTreeMap::new(), &[]).unwrap(),
            "service-start-v3 - -\n"
        );
        #[cfg(feature = "calibration")]
        assert_eq!(
            encode_generation_start_plan(&generations, &["wan".into()]).unwrap(),
            format!("service-start-v3 backup:{id},wan:{id} wan\n")
        );
        #[cfg(not(feature = "calibration"))]
        assert!(encode_generation_start_plan(&generations, &["wan".into()]).is_err());
        for invalid in ["", "short", "invalid-private-value\n", &"F".repeat(64)] {
            let error = encode_generation_start_plan(
                &BTreeMap::from([("lab".into(), invalid.into())]),
                &[],
            )
            .unwrap_err();
            assert_eq!(error, "service-start-generation-response-invalid");
        }
        assert!(encode_generation_start_plan(
            &BTreeMap::from([("bad-name".into(), id.clone())]),
            &[]
        )
        .is_err());
        assert!(encode_generation_start_plan(&generations, &["wan".into(), "wan".into()]).is_err());
        let maximum = (0..64).map(|i| (format!("lab_{i}"), id.clone())).collect();
        assert!(encode_generation_start_plan(&maximum, &[]).is_ok());
        let excessive = (0..65).map(|i| (format!("lab_{i}"), id.clone())).collect();
        assert!(encode_generation_start_plan(&excessive, &[]).is_err());
    }

    #[test]
    fn output_protocol_is_bounded_and_unambiguous() {
        #[cfg(feature = "calibration")]
        assert_eq!(encode_start_plan(&[], &[]), "service-start-v2 - -\n");
        #[cfg(not(feature = "calibration"))]
        assert_eq!(encode_start_plan(&[], &[]), "service-start-v1 -\n");
        let instances = (0..MAX_INSTANCES)
            .map(|index| format!("wan_{index}"))
            .collect::<Vec<_>>();
        #[cfg(feature = "calibration")]
        assert!(encode_start_plan(&instances, &["wan_0".to_string()])
            .starts_with("service-start-v2 wan_0,"));
        #[cfg(not(feature = "calibration"))]
        assert!(encode_start_plan(&instances, &[]).starts_with("service-start-v1 wan_0,"));
        let mut full = instances;
        assert!(push_instance(&mut full, "overflow").is_err());
        assert_eq!(
            bridger_batch("cake_bridger_7_0", &["ifb4wwan0".to_string()]),
            "add_list cake_bridger_7_0.@defaults[0].blacklist='ifb4wwan0'\ncommit cake_bridger_7_0\n"
        );
    }

    #[test]
    fn package_upgrade_has_a_distinct_typed_deferral() {
        assert!(!package_upgrade_mode_value(None).unwrap());
        assert!(!package_upgrade_mode_value(Some(std::ffi::OsStr::new(""))).unwrap());
        assert!(!package_upgrade_mode_value(Some(std::ffi::OsStr::new("0"))).unwrap());
        assert!(package_upgrade_mode_value(Some(std::ffi::OsStr::new("1"))).unwrap());
        for invalid in ["true", "01", "2", "-1"] {
            assert!(package_upgrade_mode_value(Some(std::ffi::OsStr::new(invalid))).is_err());
        }
        assert_eq!(
            format!("{SERVICE_START_DEFERRED_V1}\n"),
            "service-start-deferred-v1\n"
        );
    }

    #[cfg(feature = "calibration")]
    #[test]
    fn pending_native_apply_recovery_cannot_enter_ordinary_start() {
        assert!(require_no_native_apply_recovery(false, false).is_ok());
        assert!(require_no_native_apply_recovery(true, false).is_err());
        assert!(require_no_native_apply_recovery(false, true).is_err());
        assert!(require_no_native_apply_recovery(true, true).is_err());
    }

    #[test]
    fn borrowed_lock_requires_a_preexisting_exclusive_owner_without_upgrading_it() {
        let root = test_root("borrowed-lock");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.join("runtime.guard");
        let owner = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();

        assert!(attest_borrowed_exclusive_lock(&path, &owner)
            .unwrap_err()
            .contains("has no exclusive lock"));

        assert_eq!(
            unsafe { libc::flock(owner.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) },
            0
        );
        assert!(attest_borrowed_exclusive_lock(&path, &owner)
            .unwrap_err()
            .contains("has no exclusive lock"));
        let shared_probe = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert_eq!(
            unsafe { libc::flock(shared_probe.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB,) },
            0,
            "borrow attestation silently upgraded the shared lock"
        );
        unsafe {
            libc::flock(shared_probe.as_raw_fd(), libc::LOCK_UN);
            libc::flock(owner.as_raw_fd(), libc::LOCK_UN);
        }

        assert_eq!(
            unsafe { libc::flock(owner.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        attest_borrowed_exclusive_lock(&path, &owner).unwrap();
        let exclusive_probe = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        assert_ne!(
            unsafe { libc::flock(exclusive_probe.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB,) },
            0,
            "borrow attestation released the parent's exclusive lock"
        );
        unsafe {
            libc::flock(owner.as_raw_fd(), libc::LOCK_UN);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bridger_update_is_materialized_under_an_alias_and_committed_atomically() {
        let root = test_root("bridger");
        let config = root.join("bridger");
        let uci = root.join("uci");
        let bridger_init = root.join("bridger-init");
        let log = root.join("calls");
        fs::write(
            &config,
            b"config defaults\n\toption bridge_local_tx '1'\n\tlist blacklist 'eth0'\n",
        )
        .unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o640)).unwrap();
        write_executable(
            &uci,
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
config_dir=
while [ "$#" -gt 0 ]; do
    case "$1" in
        -c) config_dir="$2"; shift 2 ;;
        -C|-t) shift 2 ;;
        -q) shift ;;
        *) break ;;
    esac
done
command="$1"
shift
case "$command" in
    get)
        target="$1"
        alias="${{target%%.*}}"
        case "$target" in
            *.@defaults\[0\].blacklist)
                sed -n "s/^[[:space:]]*list[[:space:]]*blacklist[[:space:]]*'\\([^']*\\)'.*/\\1/p" "$config_dir/$alias"
                ;;
            *.@defaults\[0\]) printf '%s\n' defaults ;;
            *) exit 1 ;;
        esac
        ;;
    batch)
        while IFS=' ' read -r operation target; do
            case "$operation" in
                add_list)
                    alias="${{target%%.*}}"
                    value="${{target#*=}}"
                    value="${{value#\'}}"
                    value="${{value%\'}}"
                    printf "\tlist blacklist '%s'\n" "$value" >> "$config_dir/$alias"
                    ;;
                commit) : ;;
                '') : ;;
                *) exit 1 ;;
            esac
        done
        ;;
    *) exit 1 ;;
esac
"#,
                log.display()
            ),
        );
        write_executable(
            &bridger_init,
            &format!(
                "#!/bin/sh\nprintf 'bridger:%s\\n' \"$*\" >> '{}'\n",
                log.display()
            ),
        );
        let paths = ServicePaths {
            config_root: root.join("config"),
            uci,
            tc: root.join("tc"),
            ubus: root.join("ubus"),
            proc_root: root.join("proc"),
            runtime_root: root.join("runtime"),
            runtime_lock_root: root.join("locks"),
            bridger_init,
            bridger_config: config.clone(),
            uci_workspace_root: root.join("workspace"),
        };
        sync_bridger_blacklist(&paths, &BTreeSet::from(["ifb4wwan0".to_string()])).unwrap();

        let committed = fs::read_to_string(&config).unwrap();
        assert!(committed.contains("option bridge_local_tx '1'"));
        assert!(committed.contains("list blacklist 'eth0'"));
        assert!(committed.contains("list blacklist 'ifb4wwan0'"));
        assert_eq!(fs::metadata(&config).unwrap().mode() & 0o7777, 0o640);
        let calls = fs::read_to_string(&log).unwrap();
        assert!(calls
            .lines()
            .filter(|line| line.starts_with("-c "))
            .all(|line| line.contains(" -C ") && line.contains(" -t ")));
        assert!(calls
            .lines()
            .filter(|line| line.contains(" get "))
            .all(|line| line.contains("cake_bridger_") && !line.contains(" bridger.@")));
        assert!(calls.lines().any(|line| line == "bridger:reload"));
        assert!(fs::read_dir(root.join("workspace"))
            .unwrap()
            .next()
            .is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_file_replacement_rejects_concurrent_drift() {
        let root = test_root("replace-drift");
        let config = root.join("bridger");
        fs::write(&config, b"original\n").unwrap();
        let snapshot = read_exact_file(&config).unwrap().unwrap();
        fs::write(&config, b"foreign\n").unwrap();
        assert!(replace_exact_file(&config, &snapshot, b"candidate\n").is_err());
        assert_eq!(fs::read(&config).unwrap(), b"foreign\n");
        fs::remove_dir_all(root).unwrap();
    }

    #[derive(Default)]
    struct FakeStopBackend {
        events: Vec<String>,
        plan: ServiceStopPlan,
        residual_controller: bool,
    }

    impl ServiceStopBackend for FakeStopBackend {
        type Controllers = ();

        fn snapshot(&mut self) -> Result<ServiceStopPlan, String> {
            self.events.push("snapshot".to_string());
            Ok(self.plan.clone())
        }

        fn attest_unchanged(&mut self, _plan: &ServiceStopPlan) -> Result<(), String> {
            self.events.push("attest".to_string());
            Ok(())
        }

        fn capture_controllers(&mut self) -> Result<Self::Controllers, String> {
            self.events.push("pidfd-open".to_string());
            Ok(())
        }

        fn disable_service(&mut self) -> Result<(), String> {
            self.events.push("service-delete".to_string());
            Ok(())
        }

        fn wait_controllers(&mut self, _controllers: Self::Controllers) -> Result<(), String> {
            self.events.push("pidfd-wait".to_string());
            Ok(())
        }

        fn attest_no_controllers(&mut self) -> Result<(), String> {
            self.events.push("process-rescan".to_string());
            if self.residual_controller {
                Err("residual controller".to_string())
            } else {
                Ok(())
            }
        }

        fn clear_classifier(&mut self) {
            self.events.push("classifier-clear".to_string());
        }

        fn stop_sqm(&mut self, spec: &ManagedSqmStopSpec) -> Result<(), String> {
            self.events.push(format!("sqm-stop:{}", spec.sqm_section));
            Ok(())
        }

        fn cleanup_runtime(&mut self) -> Result<(), String> {
            self.events.push("runtime-cleanup".to_string());
            Ok(())
        }

        fn cleanup_sidecars(&mut self) -> Result<(), String> {
            self.events.push("sidecar-cleanup".to_string());
            Ok(())
        }
    }

    fn stop_spec(section: &str, target: &str) -> ManagedSqmStopSpec {
        ManagedSqmStopSpec {
            instance: section.trim_start_matches("cake_").to_string(),
            sqm_section: section.to_string(),
            target_interface: target.to_string(),
            download_interface: format!("ifb4{target}"),
            rate_policy: None,
        }
    }

    #[test]
    fn stop_state_machine_disables_respawn_before_sqm_and_cleans_last() {
        let mut backend = FakeStopBackend {
            plan: ServiceStopPlan {
                managed: vec![stop_spec("cake_wan", "wwan0")],
                ..ServiceStopPlan::default()
            },
            ..FakeStopBackend::default()
        };
        execute_stop(&mut backend).unwrap();
        let joined = backend.events.join("|");
        assert!(joined.find("pidfd-open").unwrap() < joined.find("service-delete").unwrap());
        assert!(joined.find("pidfd-wait").unwrap() < joined.find("sqm-stop:cake_wan").unwrap());
        assert!(
            joined.find("sqm-stop:cake_wan").unwrap() < joined.find("runtime-cleanup").unwrap()
        );
        assert_eq!(
            backend
                .events
                .iter()
                .filter(|event| *event == "process-rescan")
                .count(),
            4
        );
        assert!(joined.find("runtime-cleanup").unwrap() < joined.find("sidecar-cleanup").unwrap());
    }

    #[test]
    fn residual_controller_blocks_every_runtime_mutation() {
        let mut backend = FakeStopBackend {
            residual_controller: true,
            ..FakeStopBackend::default()
        };
        assert!(execute_stop(&mut backend).is_err());
        assert_eq!(
            backend.events,
            [
                "snapshot",
                "attest",
                "pidfd-open",
                "attest",
                "service-delete",
                "pidfd-wait",
                "process-rescan"
            ]
        );
    }

    #[test]
    fn stop_plan_includes_disabled_and_stale_owned_queues_but_rejects_shared_target() {
        let cake = parse(
            CAKE_PACKAGE,
            "cake-autorate.wan=cake_autorate\ncake-autorate.wan.dl_if='ifb4wwan0'\n",
        );
        let sqm = parse(
            SQM_PACKAGE,
            "sqm.cake_wan=queue\nsqm.cake_wan.enabled='0'\nsqm.cake_wan.interface='wwan0'\nsqm.cake_wan._cake_autorate_managed='wan'\nsqm.stale=queue\nsqm.stale.enabled='0'\nsqm.stale.interface='wwan1'\nsqm.stale._cake_autorate_managed='removed'\n",
        );
        let plan = plan_stop(&cake, &sqm, &Resolver::default()).unwrap();
        assert_eq!(
            plan.managed
                .iter()
                .map(|spec| spec.sqm_section.as_str())
                .collect::<Vec<_>>(),
            ["cake_wan", "stale"]
        );
        assert_eq!(plan.managed[1].download_interface, "ifb4wwan1");
        assert_eq!(
            plan.managed[0].rate_policy,
            Some(ManagedSqmRatePolicy {
                minimum_download_kbps: crate::rate_limits::DEFAULT_MIN_DL_SHAPER_RATE_KBPS,
                maximum_download_kbps: crate::rate_limits::DEFAULT_MAX_DL_SHAPER_RATE_KBPS,
                minimum_upload_kbps: crate::rate_limits::DEFAULT_MIN_UL_SHAPER_RATE_KBPS,
                maximum_upload_kbps: crate::rate_limits::DEFAULT_MAX_UL_SHAPER_RATE_KBPS,
            })
        );
        assert_eq!(plan.managed[1].rate_policy, None);

        let wrong_owner_type = parse(
            CAKE_PACKAGE,
            "cake-autorate.wan=system\ncake-autorate.wan.dl_if='ifb4wwan0'\n",
        );
        assert!(plan_stop(&wrong_owner_type, &sqm, &Resolver::default()).is_err());

        let duplicate = parse(
            SQM_PACKAGE,
            "sqm.one=queue\nsqm.one.interface='wwan0'\nsqm.one._cake_autorate_managed='wan'\nsqm.two=queue\nsqm.two.interface='wwan0'\nsqm.two._cake_autorate_managed='wanb'\n",
        );
        assert!(plan_stop(&cake, &duplicate, &Resolver::default()).is_err());
    }

    #[test]
    fn controller_match_is_exact_and_rejects_unsafe_identity() {
        let root = test_root("controller-cmdline");
        let path = root.join("cmdline");
        fs::write(&path, b"/usr/sbin/cake-autorated\0--instance\0wan\0").unwrap();
        assert_eq!(
            controller_instance(&path).unwrap(),
            Some((ServiceProcessKind::Controller, "wan".to_string()))
        );
        #[cfg(feature = "calibration")]
        {
            fs::write(&path, b"/usr/sbin/cake-autorated\0--mqtt-publisher\0wan\0").unwrap();
            assert_eq!(
                controller_instance(&path).unwrap(),
                Some((ServiceProcessKind::MqttPublisher, "wan".to_string()))
            );
        }
        fs::write(
            &path,
            b"/usr/sbin/cake-autorated\0--instance\0wan\0--extra\0",
        )
        .unwrap();
        assert_eq!(controller_instance(&path).unwrap(), None);
        fs::write(&path, b"/usr/sbin/cake-autorated\0--instance\0bad-name\0").unwrap();
        assert!(controller_instance(&path).is_err());
        let mut unrelated = b"/usr/bin/apcontroller\0".to_vec();
        unrelated.extend(std::iter::repeat_n(b'x', MAX_CMDLINE as usize + 32));
        unrelated.push(0);
        fs::write(&path, unrelated).unwrap();
        assert_eq!(controller_instance(&path).unwrap(), None);
        let mut oversized_owned = b"/usr/sbin/cake-autorated\0--instance\0wan\0".to_vec();
        oversized_owned.extend(std::iter::repeat_n(b'y', MAX_CMDLINE as usize + 32));
        fs::write(&path, oversized_owned).unwrap();
        assert!(controller_instance(&path)
            .unwrap_err()
            .contains("exceeds its safety bound"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn controller_readiness_binds_process_status_state_and_freshness() {
        let root = test_root("controller-readiness");
        let proc_root = root.join("proc");
        let runtime_root = root.join("run");
        fs::create_dir(&proc_root).unwrap();
        fs::create_dir(&runtime_root).unwrap();
        fs::write(proc_root.join("stat"), b"cpu 1 2 3 4\nbtime 100\n").unwrap();
        let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
        assert!(ticks_per_second > 0);
        let process = proc_root.join("100");
        fs::create_dir(&process).unwrap();
        fs::write(
            process.join("cmdline"),
            b"/usr/sbin/cake-autorated\0--instance\0wan\0",
        )
        .unwrap();
        fs::write(
            process.join("stat"),
            format!(
                "100 (cake) S 1 100 100 0 0 0 0 0 0 0 0 0 0 0 0 0 1 0 {}\n",
                ticks_per_second * 10
            ),
        )
        .unwrap();
        let status_dir = runtime_root.join("wan");
        fs::create_dir(&status_dir).unwrap();
        let status = status_dir.join("status.json");
        let write_status = |state: &str, started_at: f64, updated_at: f64| {
            fs::write(
                &status,
                format!(
                    "{{\"instance\":\"wan\",\"state\":\"{state}\",\"started_at\":{started_at},\"updated_at\":{updated_at}}}"
                ),
            )
            .unwrap();
            fs::set_permissions(&status, fs::Permissions::from_mode(0o644)).unwrap();
        };
        let paths = ServicePaths {
            config_root: root.join("config"),
            uci: root.join("uci"),
            tc: root.join("tc"),
            ubus: root.join("ubus"),
            proc_root: proc_root.clone(),
            runtime_root: runtime_root.clone(),
            runtime_lock_root: root.join("locks"),
            bridger_init: root.join("bridger"),
            bridger_config: root.join("bridger-config"),
            uci_workspace_root: root.join("uci-work"),
        };
        let expected = vec!["wan".to_string()];

        write_status("WAITING_OPERATION", 111.0, 121.0);
        assert!(matches!(
            observe_controller_start(&paths, &expected, 121.0).unwrap(),
            ControllerStartReadiness::Waiting(reason) if reason.contains("WAITING_OPERATION")
        ));

        write_status("RUNNING", 111.0, 121.0);
        assert_eq!(
            observe_controller_start(&paths, &expected, 121.0).unwrap(),
            ControllerStartReadiness::Ready
        );

        let replacement = proc_root.join("101");
        fs::create_dir(&replacement).unwrap();
        fs::write(
            replacement.join("cmdline"),
            b"/usr/sbin/cake-autorated\0--instance\0wan\0",
        )
        .unwrap();
        fs::write(
            replacement.join("stat"),
            format!(
                "101 (cake) S 1 101 101 0 0 0 0 0 0 0 0 0 0 0 0 0 1 0 {}\n",
                ticks_per_second * 11
            ),
        )
        .unwrap();
        assert!(matches!(
            observe_controller_start(&paths, &expected, 121.0).unwrap(),
            ControllerStartReadiness::Waiting(reason) if reason.contains("observed 2")
        ));
        fs::remove_dir_all(replacement).unwrap();

        write_status("RUNNING", 100.0, 121.0);
        assert!(matches!(
            observe_controller_start(&paths, &expected, 121.0).unwrap(),
            ControllerStartReadiness::Waiting(reason) if reason.contains("prior process")
        ));

        write_status("RUNNING", 111.0, 121.0);
        assert!(matches!(
            observe_controller_start(&paths, &expected, 200.0).unwrap(),
            ControllerStartReadiness::Waiting(reason) if reason.contains("stale")
        ));

        write_status("ERROR", 111.0, 121.0);
        assert!(observe_controller_start(&paths, &expected, 121.0).is_err());
        fs::set_permissions(&status, fs::Permissions::from_mode(0o666)).unwrap();
        write_status("RUNNING", 111.0, 121.0);
        fs::set_permissions(&status, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(observe_controller_start(&paths, &expected, 121.0).is_err());

        fs::remove_dir_all(&process).unwrap();
        assert_eq!(
            observe_controller_start(&paths, &[], 121.0).unwrap(),
            ControllerStartReadiness::Ready
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(feature = "calibration")]
    fn process_snapshot_includes_controller_and_mqtt_with_the_same_instance() {
        let root = test_root("managed-processes");
        for (pid, argument) in [(100u32, "--instance"), (101u32, "--mqtt-publisher")] {
            let process = root.join(pid.to_string());
            fs::create_dir(&process).unwrap();
            fs::write(
                process.join("cmdline"),
                format!("/usr/sbin/cake-autorated\0{argument}\0wan\0"),
            )
            .unwrap();
            fs::write(
                process.join("stat"),
                format!(
                    "{pid} (cake) S 1 {pid} {pid} 0 0 0 0 0 0 0 0 0 0 0 0 0 1 0 {}\n",
                    pid + 1000
                ),
            )
            .unwrap();
        }
        let processes = discover_controllers(&root).unwrap();
        assert_eq!(processes.len(), 2);
        assert_eq!(processes[0].kind, ServiceProcessKind::Controller);
        assert_eq!(processes[1].kind, ServiceProcessKind::MqttPublisher);
        assert!(processes.iter().all(|value| value.instance == "wan"));
        fs::remove_dir_all(root).unwrap();
    }
}
