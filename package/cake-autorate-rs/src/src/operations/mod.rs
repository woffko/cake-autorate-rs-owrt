//! Long-running calibration operations.
//!
//! The production coordinator owns native Full Auto-Tune and is progressively
//! taking ownership of Rating, speed-test and scheduled calibration through
//! separately gated request builders. Legacy helpers remain only where their
//! production engine has not yet passed the same operation-specific gates.

pub mod autotune_apply;
pub mod autotune_apply_openwrt;
pub mod autotune_apply_runtime;
pub(crate) mod autotune_bootstrap_apply;
pub(crate) mod autotune_bootstrap_apply_recovery;
pub(crate) mod autotune_bootstrap_apply_runtime;
pub mod autotune_capture;
pub mod autotune_capture_policy;
pub mod autotune_capture_session;
pub mod autotune_counter;
pub mod autotune_managed_config;
pub mod autotune_public;
pub mod autotune_request;
pub mod autotune_runtime;
pub mod autotune_runtime_driver;
pub mod autotune_runtime_store;
pub(crate) mod autotune_uci_materialization;
#[cfg(feature = "calibration")]
pub(crate) mod bootstrap_runtime_owner;
#[cfg(feature = "calibration")]
pub mod coordinator;
pub mod event_loop;
pub mod full_autotune;
pub mod identity;
pub mod journal;
pub(crate) mod json_wire;
pub mod kernel_topology;
pub(crate) mod kernel_topology_netlink;
pub mod lease;
pub mod process;
pub mod protocol;
pub mod rating;
pub mod rating_request;
pub mod runtime;
pub mod scheduler;
pub mod scheduler_config;
pub mod scheduler_runtime;
pub(crate) mod scheduler_status;
pub mod scheduler_store;
pub mod speedtest;
pub mod speedtest_request;
pub mod sqm_identity;
pub mod sqm_recovery;
pub mod state;

#[cfg(test)]
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
            "bootstrap_runtime_owner",
            include_str!("bootstrap_runtime_owner.rs"),
        ),
        ("coordinator", include_str!("coordinator.rs")),
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
        ("process", include_str!("process.rs")),
        ("protocol", include_str!("protocol.rs")),
        ("rating", include_str!("rating.rs")),
        ("rating_request", include_str!("rating_request.rs")),
        ("runtime", include_str!("runtime.rs")),
        ("scheduler", include_str!("scheduler.rs")),
        ("scheduler_config", include_str!("scheduler_config.rs")),
        ("scheduler_runtime", include_str!("scheduler_runtime.rs")),
        ("scheduler_status", include_str!("scheduler_status.rs")),
        ("scheduler_store", include_str!("scheduler_store.rs")),
        ("speedtest", include_str!("speedtest.rs")),
        ("speedtest_request", include_str!("speedtest_request.rs")),
        ("sqm_identity", include_str!("sqm_identity.rs")),
        ("sqm_recovery", include_str!("sqm_recovery.rs")),
        ("state", include_str!("state.rs")),
    ];

    fn allowed_sleeps(module: &str) -> Vec<&'static str> {
        let mut allowed = match module {
            "autotune_apply_openwrt" => vec!["thread::sleep(VERIFY_INTERVAL);"; 2],
            "autotune_apply_runtime" => vec!["std::thread::sleep(timeout);"; 2],
            "full_autotune" => vec!["thread::sleep(RUNTIME_ACK_POLL);"; 12],
            "process" => vec![
                "thread::sleep(WAIT_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));",
                "thread::sleep(WAIT_INTERVAL.min(deadline.saturating_duration_since(now)));",
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
