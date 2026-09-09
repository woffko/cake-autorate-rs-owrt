//! Long-running calibration operations.
//!
//! The production coordinator owns Full Auto-Tune, Rating, speed-test and
//! scheduled calibration through separately gated native request builders.

#[cfg(feature = "calibration")]
pub mod autotune_apply;
#[cfg(feature = "calibration")]
pub mod autotune_apply_openwrt;
#[cfg(feature = "calibration")]
pub mod autotune_apply_runtime;
#[cfg(feature = "calibration")]
pub(crate) mod autotune_bootstrap_apply;
#[cfg(feature = "calibration")]
pub(crate) mod autotune_bootstrap_apply_recovery;
#[cfg(feature = "calibration")]
pub(crate) mod autotune_bootstrap_apply_runtime;
#[cfg(feature = "calibration")]
pub mod autotune_capture;
#[cfg(feature = "calibration")]
pub mod autotune_capture_policy;
#[cfg(feature = "calibration")]
pub mod autotune_capture_session;
#[cfg(feature = "calibration")]
pub mod autotune_counter;
#[cfg(feature = "calibration")]
pub mod autotune_managed_config;
#[cfg(feature = "calibration")]
pub mod autotune_public;
#[cfg(feature = "calibration")]
pub mod autotune_request;
#[cfg(feature = "calibration")]
pub mod autotune_runtime;
#[cfg(feature = "calibration")]
pub mod autotune_runtime_driver;
#[cfg(feature = "calibration")]
pub mod autotune_runtime_store;
#[cfg(feature = "calibration")]
pub(crate) mod autotune_uci_materialization;
#[cfg(feature = "calibration")]
pub(crate) mod bootstrap_runtime_owner;
#[cfg(feature = "calibration")]
pub(crate) mod calibration_service;
#[cfg(feature = "calibration")]
pub mod coordinator;
pub(crate) mod cpu_profile;
#[cfg(feature = "calibration")]
pub mod event_loop;
#[cfg(feature = "calibration")]
pub mod full_autotune;
#[cfg(feature = "calibration")]
pub mod identity;
#[cfg(feature = "calibration")]
pub mod journal;
pub(crate) mod json_wire;
#[cfg(feature = "calibration")]
pub mod kernel_topology;
#[cfg(feature = "calibration")]
pub(crate) mod kernel_topology_netlink;
#[cfg(feature = "calibration")]
pub mod lease;
#[cfg(feature = "calibration")]
pub(crate) mod log_bundle;
#[cfg(feature = "calibration")]
pub(crate) mod luci_config;
#[cfg(feature = "calibration")]
pub(crate) mod luci_readouts;
#[cfg(feature = "calibration")]
pub(crate) mod mqtt_control;
#[cfg(feature = "calibration")]
pub(crate) mod mqtt_publisher;
#[cfg(feature = "calibration")]
pub(crate) mod native_apply_coordinator;
#[cfg(feature = "calibration")]
pub(crate) mod native_apply_lifecycle;
#[cfg(feature = "calibration")]
pub(crate) mod pinger_plan;
pub(crate) mod procd_control;
pub mod process;
#[cfg(feature = "calibration")]
pub mod protocol;
#[cfg(feature = "calibration")]
pub mod rating;
#[cfg(feature = "calibration")]
pub mod rating_request;
#[cfg(feature = "calibration")]
pub mod runtime;
pub(crate) mod runtime_health;
#[cfg(feature = "calibration")]
pub mod scheduler;
#[cfg(feature = "calibration")]
pub mod scheduler_config;
#[cfg(feature = "calibration")]
pub(crate) mod scheduler_owner;
#[cfg(feature = "calibration")]
pub mod scheduler_runtime;
#[cfg(feature = "calibration")]
pub(crate) mod scheduler_status;
#[cfg(feature = "calibration")]
pub mod scheduler_store;
pub(crate) mod service_config;
pub(crate) mod service_lifecycle;
#[cfg(feature = "calibration")]
pub mod speedtest;
#[cfg(feature = "calibration")]
pub mod speedtest_request;
#[cfg(feature = "calibration")]
pub mod sqm_identity;
pub(crate) mod sqm_projection;
pub mod sqm_recovery;
pub(crate) mod sqm_recovery_openwrt;
pub(crate) mod sqm_start_events;
#[cfg(feature = "calibration")]
pub mod state;
#[cfg(feature = "calibration")]
pub(crate) mod traffic_classifier;

#[cfg(all(test, not(feature = "calibration")))]
mod lite_boundary_tests {
    const SOURCE: &str = include_str!("mod.rs");

    #[test]
    fn lite_compiles_only_the_manual_runtime_operation_gate() {
        let calibration_modules = [
            "autotune_apply",
            "autotune_apply_openwrt",
            "autotune_apply_runtime",
            "autotune_bootstrap_apply",
            "autotune_bootstrap_apply_recovery",
            "autotune_bootstrap_apply_runtime",
            "autotune_capture",
            "autotune_capture_policy",
            "autotune_capture_session",
            "autotune_counter",
            "autotune_managed_config",
            "autotune_public",
            "autotune_request",
            "autotune_runtime",
            "autotune_runtime_driver",
            "autotune_runtime_store",
            "autotune_uci_materialization",
            "bootstrap_runtime_owner",
            "calibration_service",
            "coordinator",
            "event_loop",
            "full_autotune",
            "journal",
            "kernel_topology",
            "kernel_topology_netlink",
            "lease",
            "luci_config",
            "luci_readouts",
            "log_bundle",
            "mqtt_control",
            "mqtt_publisher",
            "native_apply_lifecycle",
            "pinger_plan",
            "protocol",
            "rating",
            "rating_request",
            "runtime",
            "scheduler",
            "scheduler_config",
            "scheduler_owner",
            "scheduler_runtime",
            "scheduler_status",
            "scheduler_store",
            "speedtest",
            "speedtest_request",
            "sqm_identity",
            "state",
            "traffic_classifier",
        ];
        for module in calibration_modules {
            let public = format!("#[cfg(feature = \"calibration\")]\npub mod {module};");
            let crate_private =
                format!("#[cfg(feature = \"calibration\")]\npub(crate) mod {module};");
            let crate_private_allowed = format!(
                "#[cfg(feature = \"calibration\")]\n#[allow(dead_code)]\npub(crate) mod {module};"
            );
            assert!(
                SOURCE.contains(&public)
                    || SOURCE.contains(&crate_private)
                    || SOURCE.contains(&crate_private_allowed),
                "calibration module {module} is not structurally feature-gated"
            );
        }

        assert!(SOURCE.contains("pub mod sqm_recovery;"));
        assert!(!SOURCE.contains("#[cfg(feature = \"calibration\")]\npub mod sqm_recovery;"));
        assert!(SOURCE.contains("pub(crate) mod sqm_recovery_openwrt;"));
        assert!(SOURCE.contains("pub(crate) mod json_wire;"));
        assert!(SOURCE.contains("pub(crate) mod cpu_profile;"));
        assert!(SOURCE.contains("pub(crate) mod runtime_health;"));
        assert!(SOURCE.contains("pub mod process;"));
        assert!(SOURCE.contains("pub(crate) mod procd_control;"));
        assert!(SOURCE.contains("pub(crate) mod service_config;"));
        assert!(SOURCE.contains("pub(crate) mod sqm_projection;"));
        assert!(SOURCE.contains("pub(crate) mod sqm_start_events;"));
        assert!(
            !SOURCE.contains("#[cfg(feature = \"calibration\")]\npub(crate) mod sqm_start_events;")
        );
    }
}

#[cfg(all(test, feature = "calibration"))]
mod timer_ownership_tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    const SOURCES: &[(&str, &str)] = &[
        ("autotune_apply", include_str!("autotune_apply.rs")),
        (
            "autotune_apply_openwrt",
            include_str!("autotune_apply_openwrt.rs"),
        ),
        (
            "autotune_apply_runtime",
            include_str!("autotune_apply_runtime.rs"),
        ),
        (
            "autotune_bootstrap_apply",
            include_str!("autotune_bootstrap_apply.rs"),
        ),
        (
            "autotune_bootstrap_apply_recovery",
            include_str!("autotune_bootstrap_apply_recovery.rs"),
        ),
        (
            "autotune_bootstrap_apply_runtime",
            include_str!("autotune_bootstrap_apply_runtime.rs"),
        ),
        ("autotune_capture", include_str!("autotune_capture.rs")),
        (
            "autotune_capture_policy",
            include_str!("autotune_capture_policy.rs"),
        ),
        (
            "autotune_capture_session",
            include_str!("autotune_capture_session.rs"),
        ),
        ("autotune_counter", include_str!("autotune_counter.rs")),
        (
            "autotune_managed_config",
            include_str!("autotune_managed_config.rs"),
        ),
        ("autotune_public", include_str!("autotune_public.rs")),
        ("autotune_request", include_str!("autotune_request.rs")),
        ("autotune_runtime", include_str!("autotune_runtime.rs")),
        (
            "autotune_runtime_driver",
            include_str!("autotune_runtime_driver.rs"),
        ),
        (
            "autotune_runtime_store",
            include_str!("autotune_runtime_store.rs"),
        ),
        (
            "autotune_uci_materialization",
            include_str!("autotune_uci_materialization.rs"),
        ),
        (
            "native_apply_coordinator",
            include_str!("native_apply_coordinator.rs"),
        ),
        (
            "bootstrap_runtime_owner",
            include_str!("bootstrap_runtime_owner.rs"),
        ),
        (
            "calibration_service",
            include_str!("calibration_service.rs"),
        ),
        ("coordinator", include_str!("coordinator.rs")),
        ("cpu_profile", include_str!("cpu_profile.rs")),
        ("event_loop", include_str!("event_loop.rs")),
        ("full_autotune", include_str!("full_autotune.rs")),
        ("identity", include_str!("identity.rs")),
        ("journal", include_str!("journal.rs")),
        ("json_wire", include_str!("json_wire.rs")),
        ("kernel_topology", include_str!("kernel_topology.rs")),
        (
            "kernel_topology_netlink",
            include_str!("kernel_topology_netlink.rs"),
        ),
        ("lease", include_str!("lease.rs")),
        ("luci_config", include_str!("luci_config.rs")),
        ("luci_readouts", include_str!("luci_readouts.rs")),
        ("log_bundle", include_str!("log_bundle.rs")),
        ("mqtt_control", include_str!("mqtt_control.rs")),
        ("mqtt_publisher", include_str!("mqtt_publisher.rs")),
        (
            "native_apply_lifecycle",
            include_str!("native_apply_lifecycle.rs"),
        ),
        ("process", include_str!("process.rs")),
        ("procd_control", include_str!("procd_control.rs")),
        ("pinger_plan", include_str!("pinger_plan.rs")),
        ("protocol", include_str!("protocol.rs")),
        ("rating", include_str!("rating.rs")),
        ("rating_request", include_str!("rating_request.rs")),
        ("runtime", include_str!("runtime.rs")),
        ("runtime_health", include_str!("runtime_health.rs")),
        ("service_config", include_str!("service_config.rs")),
        ("service_lifecycle", include_str!("service_lifecycle.rs")),
        ("sqm_projection", include_str!("sqm_projection.rs")),
        ("scheduler", include_str!("scheduler.rs")),
        ("scheduler_config", include_str!("scheduler_config.rs")),
        ("scheduler_owner", include_str!("scheduler_owner.rs")),
        ("scheduler_runtime", include_str!("scheduler_runtime.rs")),
        ("scheduler_status", include_str!("scheduler_status.rs")),
        ("scheduler_store", include_str!("scheduler_store.rs")),
        ("speedtest", include_str!("speedtest.rs")),
        ("speedtest_request", include_str!("speedtest_request.rs")),
        ("sqm_identity", include_str!("sqm_identity.rs")),
        ("sqm_recovery", include_str!("sqm_recovery.rs")),
        ("sqm_start_events", include_str!("sqm_start_events.rs")),
        (
            "sqm_recovery_openwrt",
            include_str!("sqm_recovery_openwrt.rs"),
        ),
        ("state", include_str!("state.rs")),
        ("traffic_classifier", include_str!("traffic_classifier.rs")),
    ];

    fn allowed_sleeps(module: &str) -> Vec<&'static str> {
        let mut allowed = match module {
            "autotune_apply_openwrt" => vec!["thread::sleep(VERIFY_INTERVAL);"; 2],
            "autotune_apply_runtime" => vec!["std::thread::sleep(timeout);"; 2],
            "full_autotune" => vec!["thread::sleep(RUNTIME_ACK_POLL);"; 12],
            "process" => vec![
                "thread::sleep(WAIT_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));",
            ],
            "sqm_recovery_openwrt" => vec![
                "thread::sleep(PROCESS_WAIT_INTERVAL.min(deadline.saturating_duration_since(now)));",
            ],
            "rating" => vec![
                "thread::sleep(PERMIT_POLL_INTERVAL);",
                "thread::sleep(POLL_INTERVAL);",
                "thread::sleep(POLL_INTERVAL);",
                "thread::sleep(POLL_INTERVAL);",
                "thread::sleep(POLL_INTERVAL);",
                "thread::sleep(POLL_INTERVAL);",
            ],
            "speedtest" => vec![
                "|| thread::sleep(ROUTE_RECHECK_INTERVAL),",
                "thread::sleep(POLL_INTERVAL);",
                "thread::sleep(POLL_INTERVAL);",
            ],
            _ => Vec::new(),
        };
        allowed.sort_unstable();
        allowed
    }

    #[test]
    fn production_operation_sleeps_are_an_explicit_structural_allowlist() {
        let declared = include_str!("mod.rs")
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                line.strip_prefix("pub mod ")
                    .or_else(|| line.strip_prefix("pub(crate) mod "))
                    .and_then(|name| name.strip_suffix(';'))
            })
            .collect::<BTreeSet<_>>();
        let enrolled = SOURCES
            .iter()
            .map(|(name, _)| *name)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            declared, enrolled,
            "every operation module must join the timer audit"
        );

        for (module, source) in SOURCES {
            // Individual production-area helpers may be test-only observation
            // seams.  Stop only at the actual unit-test module; cutting at the
            // first `#[cfg(test)]` silently hid all later production waits.
            let production = source
                .split("\n#[cfg(test)]\nmod tests")
                .next()
                .unwrap_or(source);
            let mut actual = production
                .lines()
                .map(str::trim)
                .filter(|line| {
                    line.contains("thread::sleep(") || line.contains("std::thread::sleep(")
                })
                .collect::<Vec<_>>();
            actual.sort_unstable();
            assert_eq!(
                actual,
                allowed_sleeps(module),
                "production sleep ownership changed in operations/{module}.rs; classify the new wait as polling, watchdog, scheduling, measurement, or remove it"
            );
        }

        let rating = include_str!("rating.rs");
        for forbidden in ["CAPTURE_FINALIZE_TIMEOUT", "worker_started_ms"] {
            assert!(
                !rating.contains(forbidden),
                "Rating reintroduced time-owned result authority: {forbidden}"
            );
        }
    }

    #[test]
    fn retired_lab_shell_bridge_is_absent_from_rust_production_sources() {
        let coordinator = include_str!("coordinator.rs");
        let module_source = include_str!("mod.rs");
        let retired_flag = ["--lab-legacy", "-adapter"].concat();
        let retired_environment = ["CAKE_AUTORATE_ENABLE_LAB_", "LEGACY_ADAPTER"].concat();
        let retired_module = ["pub mod legacy", "_adapter;"].concat();

        assert!(!coordinator.contains(&retired_flag));
        assert!(!coordinator.contains(&retired_environment));
        assert!(!module_source.contains(&retired_module));
        assert!(!Path::new(file!())
            .with_file_name("legacy_adapter.rs")
            .exists());
    }
}
