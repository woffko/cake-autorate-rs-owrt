//! R2 integration regressions. No network, UCI, service or live qdisc mutation.
use super::*;

#[test]
fn r6_controller_owned_probe_admission_never_falls_back_to_unscoped() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller(false);
    for mode in ["auto", "main", "mwan3"] {
        controller.cfg.route_mode = mode.into();
        assert!(controller.permanent_probe_producer().unwrap().is_none());
        assert!(controller.permanent_probe_owner.is_none());
    }
    // Mutate only this fixture after construction: product explicit admission
    // remains closed in configuration/routing until the full path is ready.
    controller.cfg.route_mode = "explicit".into();
    controller.route_snapshot = None;
    assert!(
        matches!(controller.permanent_probe_producer(), Err(ref error)
        if error == "permanent-probe-route-unavailable")
    );
    assert!(controller.permanent_probe_owner.is_none());
}

#[test]
fn r6_controller_route_change_waits_for_old_producer_and_preserves_poisoned_owner() {
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::new();
    let mut controller = fixture.controller(false);
    controller.cfg.route_mode = "explicit".into();
    controller.cfg.ul_if = "eth1".into();
    let route = RouteSnapshot {
        identity: routing::RouteIdentity {
            device_ifindex: None,
            mode: "explicit".into(),
            member: String::new(),
            device: "eth1".into(),
            source_ip: "192.0.2.2".into(),
            table: "101".into(),
            fwmark: "0x100".into(),
            fwmark_mask: Some(0x3f00),
        },
        online: true,
        active: true,
        member_status: "online".into(),
        reason: String::new(),
    };
    let root = fixture.root.join("owners");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let uid = fs::metadata(&root).unwrap().uid();
    let groups: String = (0..64)
        .map(|i| format!("cake-probe-{i:02}:x:{}:\n", 43000 + i))
        .collect();
    let pool =
        probe_owner::ProbeGroupPool::from_account_text(&groups, "root:x:0:0::/:/bin/false\n")
            .unwrap();
    let mut snapshot = Vec::new();
    let owner = permanent_probe_owner::PermanentProbeOwner::acquire(
        &pool,
        &root,
        uid,
        &"e".repeat(64),
        &route,
        |_, input| {
            if let Some(input) = input {
                let batch: serde_json::Value = serde_json::from_slice(input).unwrap();
                let mut table = batch["nftables"][0]["create"]["table"].clone();
                table["handle"] = 42.into();
                snapshot =
                    serde_json::to_vec(&serde_json::json!({"nftables":[{"table":table}]})).unwrap();
                Ok((true, Vec::new()))
            } else {
                Ok((true, snapshot.clone()))
            }
        },
    )
    .unwrap();
    controller.permanent_probe_owner = Some(owner);
    controller.route_snapshot = Some(route.clone());
    let token = controller.permanent_probe_producer().unwrap().unwrap();
    let lease = root.join(format!("group-{}.lease", token.gid()));
    let receipt = fs::read(&lease).unwrap();
    // A mask-only route change is sufficient: no old worker can outlive its
    // routing owner or let the new route silently use the old credentials.
    controller
        .route_snapshot
        .as_mut()
        .unwrap()
        .identity
        .fwmark_mask = Some(0xff00);
    assert!(
        matches!(controller.permanent_probe_producer(), Err(ref error)
        if error == "permanent-probe-old-route-retiring")
    );
    assert_eq!(fs::read(&lease).unwrap(), receipt);
    assert!(controller
        .permanent_probe_owner
        .as_ref()
        .unwrap()
        .matches_route(&route.identity));
    controller.route_snapshot = Some(route.clone());
    controller.route_snapshot.as_mut().unwrap().identity.device = "eth2".into();
    assert!(
        matches!(controller.permanent_probe_producer(), Err(ref error)
        if error == "permanent-probe-route-authority-mismatch")
    );
    controller.route_snapshot = Some(route);
    drop(token); // Unverified worker stop must fence further admission.
    assert!(
        matches!(controller.permanent_probe_producer(), Err(ref error)
        if error == "permanent-probe-stop-unverified")
    );
    assert_eq!(fs::read(&lease).unwrap(), receipt);
}

#[test]
fn r7_documented_counter_cadence_uses_configured_interval_with_25ms_floor() {
    let fixture = Fixture::new();
    assert_eq!(
        Config::defaults("cadence".into()).monitor_achieved_rates_interval_ms,
        200
    );
    for (configured, effective) in [(1, 25), (25, 25), (200, 200)] {
        let monitor = RateMonitor::new(
            fixture.root.join("rx").to_str().unwrap(),
            fixture.root.join("tx").to_str().unwrap(),
            configured,
        )
        .unwrap();
        assert_eq!(monitor.min_interval, Duration::from_millis(effective));
    }
}

#[test]
fn r7_pinger_driven_counter_reads_keep_history_averages_alive() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller(false);
    controller.cfg.graph_history_enabled = true;
    controller.rate_monitor.min_interval = Duration::ZERO;
    let active = vec!["fixture".to_string()];
    let health = ReflectorHealth::new(
        &controller.cfg,
        &active,
        PingerTiming::configured(&controller.cfg),
    );
    for seq in 0..3 {
        fs::write(fixture.root.join("rx"), format!("{}\n", seq * 125_000)).unwrap();
        assert!(
            controller
                .on_sample(
                    Sample {
                        reflector: "fixture".into(),
                        seq: seq.to_string(),
                        timestamp: epoch_secs(),
                        rtt_ms: 10.0,
                        dl_owd_us: 5000.0,
                        ul_owd_us: 5000.0,
                        timestamped_owd: false,
                    },
                    &active,
                    &health
                )
                .fresh
        );
        if seq > 0 {
            let summary = controller
                .history_traffic
                .finish()
                .expect("fresh ping-driven reads must populate each history window");
            assert!(summary.average_dl_kbps.is_finite() && summary.average_dl_kbps > 0.0);
            assert_eq!(summary.average_ul_kbps, 0.0);
        }
    }
}

#[test]
fn r7_graph_history_records_mean_peak_and_exact_full_lite_column_positions() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller(false);
    controller.cfg.graph_history_enabled = true;
    controller.history_budget.instance_budget_kib = 64;
    controller.history_budget.paused_low_memory = false;
    controller.throughput_floor_dl = 123.0;
    controller.throughput_floor_ul = 456.0;
    let now = Instant::now();
    controller
        .history_traffic
        .observe(now - Duration::from_secs(10), 0, 0);
    controller
        .history_traffic
        .observe(now - Duration::from_secs(9), 125_000, 0);
    controller.history_traffic.observe(now, 125_000, 125_000);
    controller.last_graph_history_sample = now - Duration::from_secs(60);
    controller.maybe_record_graph_history();
    let data = fs::read_to_string(controller.cfg.graph_history_path()).unwrap();
    let fields: Vec<_> = data.trim_end().split(',').collect();
    assert_eq!(fields.len(), 28);
    assert_eq!(&fields[3..5], &["100.0", "100.0"]);
    assert_eq!(&fields[5..9], &["", "", "123.0", "456.0"]);
    assert_eq!(&fields[24..28], &["avg-v1", "1000.0", "111.1", "10.000000"]);
    controller.reset_uplink_learning("fixture route change");
    controller.last_graph_history_sample = now - Duration::from_secs(60);
    controller.maybe_record_graph_history();
    let data = fs::read_to_string(controller.cfg.graph_history_path()).unwrap();
    let fields: Vec<_> = data.lines().last().unwrap().split(',').collect();
    assert_eq!(&fields[3..5], &["", ""]);
    assert_eq!(&fields[24..28], &["avg-v1", "", "", ""]);

    // Production sampling feeds history only from new counter reads.
    controller.rate_monitor.min_interval = Duration::ZERO;
    assert!(controller.sample_rates().fresh);
    fs::write(fixture.root.join("rx"), b"125000\n").unwrap();
    assert!(controller.sample_rates().fresh);
    let measured = controller.history_traffic.finish().unwrap();
    assert!(measured.average_dl_kbps.is_finite() && measured.average_dl_kbps > 0.0);
    assert_eq!(measured.average_ul_kbps, 0.0);
}

#[test]
fn r7_wire_cache_refreshes_after_recovery_and_health_observation_without_queue_mutation() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let mut controller = fixture.controller(false);
    controller.cfg.manage_sqm = true;
    controller.cfg.sqm_enabled = true;
    controller.cfg.adjust_dl_shaper_rate = false;
    controller.cfg.adjust_ul_shaper_rate = false;
    controller.runtime_override_active = false;
    for device in [&controller.cfg.dl_if, &controller.cfg.ul_if] {
        let path = fixture.root.join("sys").join(device);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("mtu"), b"1500\n").unwrap();
    }
    let tc = fixture.root.join("tc");
    let calls = fixture.root.join("calls");
    let qdisc = fixture.root.join("qdisc");
    fs::write(&qdisc, b"qdisc cake 8001: root noatm overhead 44\n").unwrap();
    fs::write(&tc, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n'qdisc show dev r2-fixture-dl'|'qdisc show dev r2-fixture-ul') cat '{}' ;;\n*) exit 91 ;;\nesac\n", calls.display(), qdisc.display())).unwrap();
    fs::set_permissions(&tc, fs::Permissions::from_mode(0o700)).unwrap();
    env::set_var("CAKE_AUTORATE_TC", &tc);
    controller.cfg.refresh_wire_packet_sizes();
    assert_eq!(controller.cfg.ul_max_wire_packet_size_bits, 12_352);
    fs::write(
        fixture
            .root
            .join("sys")
            .join(&controller.cfg.ul_if)
            .join("mtu"),
        b"1280\n",
    )
    .unwrap();
    fs::write(&qdisc, b"qdisc cake 8001: root ptm overhead -14\n").unwrap();
    controller.accept_recovered_sqm("fixture recovery").unwrap();
    assert_eq!(controller.cfg.dl_max_wire_packet_size_bits, 12_080);
    assert_eq!(controller.cfg.ul_max_wire_packet_size_bits, 10_288);

    controller.cfg.manage_sqm = false;
    controller.cfg.adjust_ul_shaper_rate = true;
    fs::write(
        fixture
            .root
            .join("sys")
            .join(&controller.cfg.ul_if)
            .join("mtu"),
        b"576\n",
    )
    .unwrap();
    fs::write(&qdisc, b"qdisc cake 8001: root atm overhead 18\n").unwrap();
    fs::write(&calls, b"").unwrap();
    assert!(controller.ensure_managed_sqm().0);
    assert_eq!(controller.cfg.ul_max_wire_packet_size_bits, 5512);
    assert_eq!(controller.cfg.dl_max_wire_packet_size_bits, 4608);
    let observed = fs::read_to_string(&calls).unwrap();
    assert!(
        !observed.contains("r2-fixture-dl"),
        "disabled external queue is not queried"
    );
    assert!(!observed.contains("change"));
    fs::write(&qdisc, b"qdisc htb 8001: root\n").unwrap();
    controller.cfg.refresh_wire_packet_sizes();
    assert_eq!(
        controller.cfg.ul_max_wire_packet_size_bits, 5512,
        "a replaced root is not a framing observation"
    );
    env::set_var("CAKE_AUTORATE_TC", "/bin/false");
    controller.cfg.refresh_wire_packet_sizes();
    assert_eq!(
        controller.cfg.ul_max_wire_packet_size_bits, 5512,
        "failed query preserves the observed cache"
    );
}

#[test]
fn r7_external_ip_failures_and_stale_results_do_not_change_controller_identity_or_rates() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller(false);
    controller.route_identity = Some("current-route".into());
    let original = (
        controller.shaper_dl,
        controller.shaper_ul,
        controller.run_state.clone(),
    );
    let (requests, _receiver) = mpsc::sync_channel(1);
    let (sender, results) = mpsc::channel();
    let mut runtime = ExternalIpRuntime {
        requests,
        results,
        in_flight: true,
        last_started: Instant::now(),
        interval: Duration::from_secs(120),
    };
    sender
        .send(ExternalIpResult {
            value: None,
            error: Some("endpoint unavailable".into()),
            route_identity: None,
        })
        .unwrap();
    runtime.drain(&mut controller);
    assert!(!runtime.in_flight);
    sender
        .send(ExternalIpResult {
            value: Some("192.0.2.1".into()),
            error: None,
            route_identity: Some("old-route".into()),
        })
        .unwrap();
    runtime.drain(&mut controller);
    assert!(controller.route_external_ip.is_empty());
    sender
        .send(ExternalIpResult {
            value: Some("192.0.2.2".into()),
            error: None,
            route_identity: Some("current-route".into()),
        })
        .unwrap();
    runtime.drain(&mut controller);
    assert_eq!(controller.route_external_ip, "192.0.2.2");
    assert_eq!(controller.route_identity.as_deref(), Some("current-route"));
    assert_eq!(
        (
            controller.shaper_dl,
            controller.shaper_ul,
            controller.run_state.clone()
        ),
        original
    );
}

#[test]
fn r6_external_controller_checks_only_enabled_queues_and_never_repairs_topology() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let mut controller = fixture.controller(false);
    controller.runtime_override_active = false;
    controller.cfg.manage_sqm = false;
    controller.cfg.dl_if = "ifb-external".into();
    controller.cfg.sqm_interface = "external-steering-source".into();
    for device in [&controller.cfg.dl_if, &controller.cfg.ul_if] {
        fs::create_dir_all(fixture.root.join("sys").join(device)).unwrap();
    }
    let tc = fixture.root.join("tc");
    let calls = fixture.root.join("tc-calls");
    let dl = fixture.root.join("dl-qdisc");
    let ul = fixture.root.join("ul-qdisc");
    fs::write(&dl, b"qdisc cake 8002: root bandwidth 100Mbit\n").unwrap();
    fs::write(&ul, b"qdisc cake 8001: root bandwidth 50Mbit\n").unwrap();
    fs::write(&tc, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n'qdisc show dev ifb-external') cat '{}' ;;\n'qdisc show dev r2-fixture-ul') cat '{}' ;;\n'qdisc change root dev r2-fixture-ul handle 8001: cake bandwidth 10000Kbit') exit 0 ;;\n*) exit 91 ;;\nesac\n", calls.display(), dl.display(), ul.display())).unwrap();
    fs::set_permissions(&tc, fs::Permissions::from_mode(0o700)).unwrap();
    env::set_var("CAKE_AUTORATE_TC", &tc);
    assert!(inspect_controller_topology(&controller.cfg).is_ok());
    assert!(!fs::read_to_string(&calls).unwrap().contains("filter"));
    // The native/SQM shape contract still refuses unknown IFB steering.
    assert!(inspect_sqm_topology(&controller.cfg).is_err());
    controller.cfg.adjust_dl_shaper_rate = false;
    fs::write(&dl, b"qdisc htb 1: root\nqdisc cake 2: parent 1:1\n").unwrap();
    fs::write(&calls, b"").unwrap();
    assert_eq!(controller.ensure_managed_sqm(), (true, false));
    assert_eq!(controller.sqm_runtime_state, "UNMANAGED");
    assert!(!fs::read_to_string(&calls).unwrap().contains("ifb-external"));
    fs::write(&ul, b"qdisc noqueue 0: root\n").unwrap();
    assert_eq!(controller.ensure_managed_sqm(), (false, false));
    assert_eq!(controller.run_state, "WAITING_EXTERNAL_SQM");
    controller.shaper_ul = 12_345.0;
    controller.apply_shaper("ul");
    assert!(!fs::read_to_string(&calls).unwrap().contains("qdisc change"));
    assert_eq!(controller.sqm_recovery_attempts, 0);
    fs::write(&ul, b"qdisc cake 8001: root bandwidth unlimited\n").unwrap();
    assert_eq!(controller.ensure_managed_sqm(), (true, true));
    assert!(!fs::read_to_string(&calls).unwrap().contains("qdisc change"));
    change_cake_rate(&controller.cfg.ul_if, 10_000, CakeQdiscKind::Cake).unwrap();
    let commands = fs::read_to_string(&calls).unwrap();
    assert!(commands.contains("handle 8001: cake bandwidth 10000Kbit"));
    assert!(!commands.contains("qdisc del"));
    assert!(!commands.contains("qdisc add"));
    let before = commands;
    fs::write(&ul, b"qdisc htb 8001: root\n").unwrap();
    assert!(change_cake_rate(&controller.cfg.ul_if, 10_000, CakeQdiscKind::Cake).is_err());
    assert_eq!(
        fs::read_to_string(&calls)
            .unwrap()
            .matches("qdisc change")
            .count(),
        before.matches("qdisc change").count()
    );
}

#[test]
fn r6_rate_change_requires_one_addressable_root_cake_identity() {
    assert_eq!(
        root_cake_control_identity("qdisc cake 800A: root bandwidth unlimited").unwrap(),
        (CakeQdiscKind::Cake, "800a:".into())
    );
    for invalid in [
        "qdisc cake 0: root",
        "qdisc cake 1:1 root",
        "qdisc htb 1: root\nqdisc cake 2: parent 1:1",
        "qdisc cake 1: root\nqdisc htb 2: root",
        "qdisc cake 10000: root",
        "qdisc cake missing root",
    ] {
        assert!(root_cake_control_identity(invalid).is_err());
    }
}

#[cfg(feature = "calibration")]
#[test]
fn r6_ctinfo_is_readable_but_not_exclusive_native_restore_authority() {
    let ingress = "filter parent ffff: protocol all pref 10 u32 chain 0\nfilter parent ffff: protocol all pref 10 u32 chain 0 fh 800: ht divisor 1\nfilter parent ffff: protocol all pref 10 u32 chain 0 fh 800::800 order 2048 key ht 800 bkt 0 flowid 1:1 not_in_hw\n match 00000000/00000000 at 0\n action order 1: ctinfo dscp 0xfc000000\n action order 2: mirred (Egress Redirect to device ifb4wan) stolen\n index 2 ref 1 bind 1\n";
    assert!(attest_download_redirect(ingress, true, "wan", "ifb4wan").is_ok());
    let error = attest_exclusive_sqm_ingress(ingress, "wan", "ifb4wan").unwrap_err();
    assert_eq!(error.code, "download-ingress-not-exclusive");
    assert!(error.message.contains("cannot restore ctinfo"));
}

#[test]
fn r6_unknown_runtime_owner_is_visible_without_timer_based_rate_writes() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller(false);
    controller.runtime_override_active = false;
    controller.write_initial_status(&[], None).unwrap();
    let rates = (
        controller.shaper_dl,
        controller.shaper_ul,
        controller.last_set_dl,
        controller.last_set_ul,
    );
    let status = |controller: &Controller| -> serde_json::Value {
        serde_json::from_slice(&fs::read(controller.cfg.run_dir().join("status.json")).unwrap())
            .unwrap()
    };
    for _ in 0..100 {
        controller.set_runtime_control_state(false, Some("<owner> \"unreadable\""));
    }
    let held = status(&controller);
    assert_eq!(held["runtime_control_held"], true);
    assert_eq!(held["runtime_control_degraded"], true);
    assert_eq!(held["runtime_control_error"], "<owner> \"unreadable\"");
    assert_eq!(
        (
            controller.shaper_dl,
            controller.shaper_ul,
            controller.last_set_dl,
            controller.last_set_ul
        ),
        rates
    );
    controller.set_runtime_control_state(true, None); // Known active owner, not Idle.
    assert_eq!(status(&controller)["runtime_control_held"], true);
    assert_eq!(status(&controller)["runtime_control_degraded"], false);
    controller.set_runtime_control_state(false, None); // Exact Idle proof at caller.
    assert_eq!(status(&controller)["runtime_control_held"], false);
    assert!(status(&controller)["runtime_control_error"].is_null());
}

#[test]
fn r5_no_spare_reflector_rearms_the_configured_health_window() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller(false);
    controller.cfg.reflectors = vec!["fixture".into()];
    controller.cfg.no_pingers = 1;
    let cfg = controller.cfg.clone();
    let mut active = cfg.reflectors.clone();
    let mut health = ReflectorHealth::new(&cfg, &active, PingerTiming::configured(&cfg));
    health.states.get_mut("fixture").unwrap().push_offence(true);
    assert!(!health.replace_active_reflector(
        &cfg,
        &mut active,
        0,
        "fixture deadline",
        &mut controller
    ));
    let state = health.states.get_mut("fixture").unwrap();
    assert_eq!(
        state.offences.len(),
        cfg.reflector_misbehaving_detection_window
    );
    assert_eq!(state.offence_sum, 0);
    state.push_offence(true);
    assert_eq!(state.offence_sum, 1);
}

#[test]
fn r5_cold_high_rtt_qualifies_without_false_bloat_and_still_detects_later_bloat() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller(false);
    let active = vec!["fixture".to_string()];
    let health = ReflectorHealth::new(
        &controller.cfg,
        &active,
        PingerTiming::configured(&controller.cfg),
    );
    for epoch in ["cold startup", "route learning reset"] {
        if epoch == "route learning reset" {
            controller.reset_uplink_learning("fixture actual route change");
        }
        let start = controller.baseline_epoch;
        for seq in 0..12 {
            let now = start + Duration::from_millis(100 + seq * 300);
            controller.on_sample_with_rates(
                Sample {
                    reflector: "fixture".into(),
                    seq: seq.to_string(),
                    timestamp: epoch_secs(),
                    rtt_ms: 600.0,
                    dl_owd_us: 300_000.0,
                    ul_owd_us: 300_000.0,
                    timestamped_owd: false,
                },
                &active,
                &health,
                RateSample {
                    dl_kbps: 0.0,
                    ul_kbps: 0.0,
                    fresh: false,
                    dl_observed_at: now - Duration::from_millis(50),
                    ul_observed_at: now - Duration::from_millis(50),
                },
                now,
            );
            if seq < 2 {
                assert!(controller.dl_baseline_us.is_empty());
            }
        }
        let offences = controller.dl_delays.iter().filter(|value| **value).count();
        assert_eq!(offences, 0);
        assert_eq!(controller.dl_baseline_us["fixture"], 300_000.0);
        assert_eq!(*controller.dl_delta_us.back().unwrap(), 0.0);
        for seq in 12..18 {
            let now = start + Duration::from_millis(100 + seq * 300);
            controller.on_sample_with_rates(
                Sample {
                    reflector: "fixture".into(),
                    seq: seq.to_string(),
                    timestamp: epoch_secs(),
                    rtt_ms: 800.0,
                    dl_owd_us: 400_000.0,
                    ul_owd_us: 400_000.0,
                    timestamped_owd: false,
                },
                &active,
                &health,
                RateSample {
                    dl_kbps: 0.0,
                    ul_kbps: 0.0,
                    fresh: true,
                    dl_observed_at: now,
                    ul_observed_at: now,
                },
                now,
            );
        }
        assert!(
            controller.dl_delays.iter().filter(|value| **value).count()
                >= controller.cfg.bufferbloat_detection_thr
        );
    }
}

#[test]
fn r5_cold_baseline_holds_rates_and_reports_learning_for_loaded_or_repeated_counter_frames() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller(false);
    let active = vec!["fixture".into()];
    let health = ReflectorHealth::new(
        &controller.cfg,
        &active,
        PingerTiming::configured(&controller.cfg),
    );
    let rates = (controller.shaper_dl, controller.shaper_ul);
    let start = controller.baseline_epoch;
    for seq in 0..10 {
        let now = start + Duration::from_millis(100 + seq * 100);
        let loaded = seq < 5;
        let load_epoch = if loaded {
            now
        } else {
            start + Duration::from_millis(600)
        };
        controller.on_sample_with_rates(
            Sample {
                reflector: "fixture".into(),
                seq: seq.to_string(),
                timestamp: epoch_secs(),
                rtt_ms: 600.0,
                dl_owd_us: 300_000.0,
                ul_owd_us: 300_000.0,
                timestamped_owd: false,
            },
            &active,
            &health,
            RateSample {
                dl_kbps: if loaded { 50_000.0 } else { 0.0 },
                ul_kbps: 0.0,
                fresh: loaded,
                dl_observed_at: load_epoch,
                ul_observed_at: load_epoch,
            },
            now,
        );
    }
    assert!(controller.dl_baseline_us.is_empty());
    assert_eq!(controller.run_state, "LEARNING");
    assert_eq!((controller.shaper_dl, controller.shaper_ul), rates);
    controller.refresh_status_from_last_sample().unwrap();
    let status: serde_json::Value =
        serde_json::from_slice(&fs::read(controller.cfg.run_dir().join("status.json")).unwrap())
            .unwrap();
    assert_eq!(status["latency_baseline_ready"], false);
    assert_eq!(status["latency_baseline_ready_reflectors"], 0);
    assert_eq!(status["latency_baseline_pending_reflectors"], 1);
}

#[test]
fn r5_short_stall_pauses_but_sustained_gap_and_route_loss_reset_safe_bound() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller(false);
    controller.cfg.adaptive_ceiling_enabled = true;
    controller.cfg.adjust_ul_shaper_rate = false;
    let start = Instant::now() - Duration::from_secs(60);
    controller.adaptive_dl = AdaptiveCeilingDirection::new_at(100_000.0, 150_000.0, start);
    let policy = AdaptiveCeilingPolicy {
        hold_time: Duration::from_secs(10),
        probe_step_percent: 10.0,
        probe_duration: Duration::from_secs(5),
        cooldown: Duration::from_secs(10),
        failed_bound_ttl: Duration::from_secs(300),
        eligibility_grace: Duration::from_secs(1),
        minimum_throughput_gain_percent: 1.0,
    };
    for (seconds, rate) in [
        (0, 100_000.0),
        (10, 100_000.0),
        (11, 109_000.0),
        (16, 110_000.0),
    ] {
        controller.adaptive_dl.observe(
            start + Duration::from_secs(seconds),
            AdaptiveCeilingObservation {
                eligible: true,
                bufferbloat: false,
                shaper_rate_kbps: rate,
                achieved_rate_kbps: rate,
            },
            policy,
        );
    }
    assert_eq!(controller.adaptive_dl.safe_ceiling_kbps(), 110_000.0);
    controller.set_run_state("RUNNING");
    controller.set_run_state("STALL");
    controller.set_run_state("RUNNING"); // Immediate reply; no elapsed outage.
    assert_eq!(controller.adaptive_dl.safe_ceiling_kbps(), 110_000.0);
    controller.reset_adaptive_after_sustained_gap();
    assert_eq!(controller.adaptive_dl.safe_ceiling_kbps(), 100_000.0);
    controller.adaptive_dl = AdaptiveCeilingDirection::new_at(100_000.0, 150_000.0, start);
    for (seconds, rate) in [
        (0, 100_000.0),
        (10, 100_000.0),
        (11, 109_000.0),
        (16, 110_000.0),
    ] {
        controller.adaptive_dl.observe(
            start + Duration::from_secs(seconds),
            AdaptiveCeilingObservation {
                eligible: true,
                bufferbloat: false,
                shaper_rate_kbps: rate,
                achieved_rate_kbps: rate,
            },
            policy,
        );
    }
    controller.dl_baseline_us.insert("fixture".into(), 12345.0);
    assert_eq!(controller.adaptive_dl.safe_ceiling_kbps(), 110_000.0);
    controller.reset_uplink_learning("fixture genuine route loss");
    assert_eq!(controller.adaptive_dl.safe_ceiling_kbps(), 100_000.0);
    assert!(controller.dl_baseline_us.is_empty());
}

#[test]
fn r5_pinger_failure_preserves_controller_learning_and_invalidates_active_rating() {
    let fixture = Fixture::new();
    let mut controller = fixture.controller(false);
    controller.dl_baseline_us.insert("fixture".into(), 12345.0);
    controller.ul_baseline_us.insert("fixture".into(), 23456.0);
    controller.route_identity = Some("fixture-route".into());
    let rates = (controller.shaper_dl, controller.shaper_ul);
    #[cfg(feature = "calibration")]
    {
        controller.rating_load.set_capture(
            Some("0123456789abcdef0123456789abcdef"),
            Some("speedtest"),
            None,
            0.0,
            0.0,
            Instant::now(),
        );
    }
    controller.note_pinger_failure("owned fixture EOF");
    assert_eq!(controller.run_state, "RECOVERING");
    assert_eq!(controller.dl_baseline_us["fixture"], 12345.0);
    assert_eq!(controller.ul_baseline_us["fixture"], 23456.0);
    assert_eq!(controller.route_identity.as_deref(), Some("fixture-route"));
    assert_eq!((controller.shaper_dl, controller.shaper_ul), rates);
    #[cfg(feature = "calibration")]
    assert!(controller.rating_load.capture_contaminated());
}

#[cfg(feature = "calibration")]
use super::tests::HELPER_TEST_LOCK;
#[cfg(not(feature = "calibration"))]
static HELPER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Fixture {
    root: PathBuf,
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl Fixture {
    fn new() -> Self {
        let guard = HELPER_TEST_LOCK.lock().unwrap();
        let root = env::temp_dir().join(format!(
            "cake-r2-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("rx"), b"0\n").unwrap();
        fs::write(root.join("tx"), b"0\n").unwrap();
        let previous = [
            "CAKE_AUTORATE_RUN_ROOT",
            "CAKE_AUTORATE_TC",
            "CAKE_AUTORATE_SYS_CLASS_NET",
        ]
        .into_iter()
        .map(|key| (key, env::var_os(key)))
        .collect();
        env::set_var("CAKE_AUTORATE_RUN_ROOT", root.join("run"));
        // Even read-only constructor MTU/qdisc discovery must not call live tc.
        env::set_var("CAKE_AUTORATE_TC", "/bin/false");
        env::set_var("CAKE_AUTORATE_SYS_CLASS_NET", root.join("sys"));
        Self {
            root,
            previous,
            _guard: guard,
        }
    }

    fn controller(&self, transport: bool) -> Controller {
        let mut cfg = Config::defaults("r2".to_string());
        cfg.rx_bytes_path = self.root.join("rx").to_string_lossy().into_owned();
        cfg.tx_bytes_path = self.root.join("tx").to_string_lossy().into_owned();
        cfg.dl_if = "r2-fixture-dl".to_string();
        cfg.ul_if = "r2-fixture-ul".to_string();
        cfg.sqm_interface = "r2-fixture-ul".to_string();
        cfg.log_to_file = false;
        cfg.graph_history_enabled = false;
        cfg.adjust_dl_shaper_rate = true;
        cfg.adjust_ul_shaper_rate = true;
        cfg.min_dl_shaper_rate_kbps = 10_000.0;
        cfg.min_ul_shaper_rate_kbps = 5_000.0;
        cfg.base_dl_shaper_rate_kbps = 100_000.0;
        cfg.base_ul_shaper_rate_kbps = 50_000.0;
        cfg.max_dl_shaper_rate_kbps = 100_000.0;
        cfg.max_ul_shaper_rate_kbps = 50_000.0;
        #[cfg(feature = "transport-probes")]
        {
            cfg.transport_latency_enabled = transport;
            cfg.transport_controller_enabled = transport;
        }
        #[cfg(not(feature = "transport-probes"))]
        let _ = transport;
        cfg.bufferbloat_refractory_period_ms = 0;
        cfg.decay_refractory_period_ms = 0;
        let mut controller = Controller::new(cfg).unwrap();
        // All tests exercise control decisions directly; never apply to a device.
        controller.runtime_override_active = true;
        controller.uplink_state = UplinkState::Active;
        controller
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        for (key, value) in &self.previous {
            if let Some(value) = value {
                env::set_var(key, value);
            } else {
                env::remove_var(key);
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn r2_icmp_cuts_and_minimum_enforcement_reach_configured_minima() {
    let fixture = Fixture::new();
    let modes: &[bool] = if cfg!(feature = "transport-probes") {
        &[false, true]
    } else {
        &[false]
    };
    for &transport in modes {
        let mut c = fixture.controller(transport);
        for _ in 0..100 {
            for dl in [true, false] {
                c.update_direction(dl, LoadKind::High, true, 1_000_000.0, true, Instant::now());
            }
            c.clamp_rates();
        }
        assert_eq!((c.shaper_dl, c.shaper_ul), (10_000.0, 5_000.0));
        c.shaper_dl = 90_000.0;
        c.shaper_ul = 45_000.0;
        c.set_min_shaper_rates("R2 test");
        assert_eq!((c.shaper_dl, c.shaper_ul), (10_000.0, 5_000.0));
        c.cfg.adjust_ul_shaper_rate = false;
        c.shaper_ul = 123.0;
        c.clamp_rates();
        c.set_min_shaper_rates("disabled direction");
        assert_eq!(c.shaper_ul, 123.0);
    }
}

#[cfg(feature = "transport-probes")]
fn seed(tracker: &mut TransportLatencyTracker, now: Instant, delta: f64) {
    for _ in 0..20 {
        tracker.observe_success("fixture", 20.0, false, now);
    }
    for _ in 0..20 {
        tracker.observe_success("fixture", 20.0 + delta, true, now);
    }
}

#[cfg(feature = "transport-probes")]
#[test]
fn r2_directional_growth_missing_stale_good_bad_and_opt_out_matrix() {
    let fixture = Fixture::new();
    for enabled in [false, true] {
        for (label, delta, stale) in [
            ("missing", None, false),
            ("good", Some(10.0), false),
            ("bad", Some(90.0), false),
            ("stale_bad", Some(90.0), true),
        ] {
            let mut c = fixture.controller(enabled);
            let now = Instant::now();
            for dl in [true, false] {
                c.transport_latency_dl.reset();
                c.transport_latency_ul.reset();
                if let Some(delta) = delta {
                    let at = if stale {
                        now - c.transport_max_age() - Duration::from_secs(1)
                    } else {
                        now
                    };
                    seed(
                        if dl {
                            &mut c.transport_latency_dl
                        } else {
                            &mut c.transport_latency_ul
                        },
                        at,
                        delta,
                    );
                }
                let evidence = c.transport_control_evidence(dl, now);
                let expected_growth = !enabled || label != "bad";
                assert_eq!(
                    evidence.allows_icmp_growth(),
                    expected_growth,
                    "{enabled} {label} {dl}"
                );
                assert_eq!(evidence.allows_promotion(), !enabled || label == "good");
                let other = c.transport_control_evidence(!dl, now);
                assert!(
                    other.allows_icmp_growth(),
                    "one direction must not block the other"
                );
                assert_eq!(other.allows_promotion(), !enabled);
                c.shaper_dl = 40_000.0;
                c.shaper_ul = 20_000.0;
                let before = if dl { c.shaper_dl } else { c.shaper_ul };
                c.update_direction(
                    dl,
                    LoadKind::High,
                    false,
                    0.0,
                    evidence.allows_icmp_growth(),
                    now,
                );
                c.clamp_rates();
                let after = if dl { c.shaper_dl } else { c.shaper_ul };
                assert_eq!(after > before, expected_growth, "{enabled} {label} {dl}");
                // A good transport sample does not overrule ICMP bufferbloat.
                c.update_direction(
                    dl,
                    LoadKind::High,
                    true,
                    1_000_000.0,
                    evidence.allows_icmp_growth(),
                    now,
                );
                c.clamp_rates();
                assert!(if dl { c.shaper_dl } else { c.shaper_ul } < after);
            }
        }
    }
}

#[cfg(feature = "transport-probes")]
#[test]
fn r2_missing_transport_cannot_qualify_an_optional_ceiling_probe() {
    let fixture = Fixture::new();
    for enabled in [false, true] {
        for good in [false, true] {
            let mut c = fixture.controller(enabled);
            c.cfg.adaptive_ceiling_enabled = true;
            c.adaptive_dl =
                AdaptiveCeilingDirection::new_with_verified_safe(100_000.0, 150_000.0, 100_000.0);
            c.adaptive_ul =
                AdaptiveCeilingDirection::new_with_verified_safe(50_000.0, 75_000.0, 50_000.0);
            let now = Instant::now();
            let mut opened_dl = false;
            let mut opened_ul = false;
            for seconds in 0..60 {
                let at = now + Duration::from_secs(seconds);
                if good {
                    seed(&mut c.transport_latency_dl, at, 10.0);
                }
                let clean = [
                    c.transport_control_evidence(true, at).allows_promotion(),
                    c.transport_control_evidence(false, at).allows_promotion(),
                ];
                c.update_adaptive_ceilings(
                    LoadKind::High,
                    LoadKind::High,
                    99_000.0,
                    49_500.0,
                    false,
                    false,
                    0,
                    0,
                    0.0,
                    0.0,
                    clean,
                    at,
                );
                opened_dl |= c.adaptive_dl.effective_max_kbps() > 100_000.0;
                opened_ul |= c.adaptive_ul.effective_max_kbps() > 50_000.0;
                if enabled && !good {
                    assert_eq!(c.adaptive_dl.effective_max_kbps(), 100_000.0);
                }
                if enabled {
                    assert_eq!(c.adaptive_ul.effective_max_kbps(), 50_000.0);
                }
            }
            if enabled && !good {
                assert_eq!(c.adaptive_dl.phase().as_str(), "cruise");
            }
            assert_eq!(opened_dl, !enabled || good);
            assert_eq!(opened_ul, !enabled);
        }
    }
}

#[cfg(feature = "transport-probes")]
#[test]
fn r2_icmp_cut_invalidates_transport_rollback_even_above_floor() {
    let fixture = Fixture::new();
    let mut c = fixture.controller(true);
    c.cfg.shaper_rate_min_adjust_down_bufferbloat = 0.97;
    c.cfg.shaper_rate_max_adjust_down_bufferbloat = 0.97;
    let now = Instant::now();
    for dl in [true, false] {
        let policy = c.quality_policy(dl);
        let rate = if dl { c.shaper_dl } else { c.shaper_ul };
        let update = if dl {
            &mut c.quality_search_dl
        } else {
            &mut c.quality_search_ul
        }
        .observe(now, rate, 170.0, true, policy);
        if dl {
            c.shaper_dl = update.requested_rate_kbps.unwrap();
        } else {
            c.shaper_ul = update.requested_rate_kbps.unwrap();
        }
        c.update_direction(dl, LoadKind::High, true, 1_000_000.0, true, now);
        c.clamp_rates();
        let after_cut = if dl { c.shaper_dl } else { c.shaper_ul };
        assert!(after_cut > policy.floor_kbps);
        let update = if dl {
            &mut c.quality_search_dl
        } else {
            &mut c.quality_search_ul
        }
        .observe(
            now + Duration::from_secs(10),
            after_cut,
            180.0,
            true,
            policy,
        );
        assert!(update
            .requested_rate_kbps
            .is_none_or(|rate| rate <= after_cut));
    }
}

#[cfg(feature = "transport-probes")]
#[test]
fn r2_status_reports_missing_and_expired_transport_without_bad_grade() {
    let fixture = Fixture::new();
    let mut c = fixture.controller(true);
    c.write_initial_status(&[], None).unwrap();
    let status = fs::read_to_string(c.cfg.run_dir().join("status.json")).unwrap();
    assert!(status.contains("\"transport_status\":\"missing\""));
    assert!(status.contains("\"transport_sample_age_s\":null"));
    let old = Instant::now() - c.transport_max_age() - Duration::from_secs(2);
    seed(&mut c.transport_latency, old, 500.0);
    seed(&mut c.transport_latency_dl, old, 500.0);
    seed(&mut c.transport_latency_ul, old, 500.0);
    c.quality_dl_class = QualityClass::F;
    c.quality_ul_class = QualityClass::F;
    // Publish on the next scheduled status slot, not within the 250ms throttle.
    // Advance the fixture's clock state rather than sleeping or weakening the
    // production publication limit.
    c.last_status_publish = Instant::now() - STATUS_PUBLISH_INTERVAL;
    c.write_initial_status(&[], None).unwrap();
    let status = fs::read_to_string(c.cfg.run_dir().join("status.json")).unwrap();
    assert!(status.contains("\"transport_status\":\"stale\""));
    #[cfg(feature = "calibration")]
    assert!(serde_json::from_str::<serde_json::Value>(&status).is_ok());
    assert!(status.contains("\"quality_controller_reason\":\"stale\""));
    assert!(status.contains("\"transport_delta_ms\":null"));
    assert!(status.contains("\"quality_controller_class\":\"LEARNING\""));
    assert!(status.contains("\"quality_controller_dl_class\":\"LEARNING\""));
    assert!(status.contains("\"quality_controller_ul_class\":\"LEARNING\""));
    assert!(status.contains("\"transport_dl_status\":\"stale\""));
}
