//! Pure candidate/parser/window regressions, shared by Full and Lite.
use super::*;

#[test]
fn r6_explicit_dns_is_opt_in_typed_and_not_a_system_resolver_alias() {
    assert_eq!(Config::defaults("dns".into()).explicit_dns_server, None);
    assert_eq!(routing::explicit_dns_server("main", "").unwrap(), None);
    assert_eq!(
        routing::explicit_dns_server("explicit", "192.0.2.53").unwrap(),
        Some("192.0.2.53".parse().unwrap())
    );
    for mode in ["main", "mwan3", "auto"] {
        assert!(routing::explicit_dns_server(mode, "192.0.2.53").is_err());
    }
    for address in [
        "127.0.0.1",
        "0.0.0.0",
        "0.1.2.3",
        "169.254.1.1",
        "224.0.0.1",
        "240.0.0.1",
        "255.255.255.255",
        "::1",
        "dns.example",
        " 192.0.2.53",
        "192.0.2.53 192.0.2.54",
    ] {
        assert!(
            routing::explicit_dns_server("explicit", address).is_err(),
            "{address}"
        );
    }
}

#[test]
fn r7_wire_framing_handles_ptm_signed_overhead_mpu_and_only_the_root() {
    for (mode, name, expected) in [
        (CakeLinkLayer::NoAtm, "noatm", 12_352),
        (CakeLinkLayer::Atm, "atm", 13_992),
        (CakeLinkLayer::Ptm, "ptm", 12_552),
    ] {
        let output = format!("qdisc cake 8001: root bandwidth 10Mbit {name} overhead 44 mpu 84\nqdisc cake 8002: parent 1:1 atm overhead 256");
        assert_eq!(parse_tc_linklayer_overhead(&output), Some((mode, 44, 84)));
        assert_eq!(
            max_wire_packet_size_bits_from_mtu(1500, 44, mode, 84),
            expected
        );
    }
    assert_eq!(
        parse_tc_linklayer_overhead("qdisc cake_mq 1: root raw overhead -14"),
        Some((CakeLinkLayer::NoAtm, -14, 0))
    );
    assert_eq!(
        max_wire_packet_size_bits_from_mtu(1500, -14, CakeLinkLayer::NoAtm, 0),
        11_888
    );
    assert_eq!(
        max_wire_packet_size_bits_from_mtu(64, -64, CakeLinkLayer::Atm, 84),
        848
    );
    assert_eq!(
        max_wire_packet_size_bits_from_mtu(64, 0, CakeLinkLayer::Ptm, 0),
        520
    );
    assert_eq!(
        max_wire_packet_size_bits_from_mtu(65, 0, CakeLinkLayer::Ptm, 0),
        536
    );
    assert_eq!(
        max_wire_packet_size_bits_from_mtu(48, 0, CakeLinkLayer::Atm, 0),
        424
    );
    assert_eq!(
        max_wire_packet_size_bits_from_mtu(49, 0, CakeLinkLayer::Atm, 0),
        848
    );
    assert_eq!(
        max_wire_packet_size_bits_from_mtu(u64::MAX, 256, CakeLinkLayer::Ptm, 256),
        u64::MAX
    );
    for invalid in [
        "",
        "qdisc cake 1: parent 2:1 ptm overhead 44",
        "qdisc cake 1: root ptm overhead -65",
        "qdisc cake 1: root atm overhead 257",
        "qdisc cake 1: root ptm overhead 4 mpu 257",
        "qdisc cake 1: root atm overhead bad",
        "qdisc cake 1: root atm overhead 4 overhead 5",
        "qdisc cake 1: root atm ptm overhead 4",
        "qdisc cake 1: root atm overhead 4\nqdisc cake 2: root ptm overhead 4",
    ] {
        assert!(parse_tc_linklayer_overhead(invalid).is_none(), "{invalid}");
    }
}

#[test]
fn r7_external_ip_is_opt_in_with_pure_validated_candidate_fields() {
    let cfg = Config::defaults("privacy".into());
    assert!(!cfg.external_ip_check_enabled);
    assert_eq!(cfg.external_ip_check_interval_s, 3600);
    assert!(ExternalIpRuntime::spawn(&cfg, RouteSpec::new("invalid", "", "")).is_none());
    let mut values = HashMap::from([
        ("external_ip_check_enabled".into(), "1".into()),
        ("external_ip_check_interval_s".into(), "120".into()),
        (
            "external_ip_check_url".into(),
            "https://never-contact.invalid/ip".into(),
        ),
    ]);
    let cfg = Config::from_uci_values("privacy", &values, &HashMap::new()).unwrap();
    assert!(cfg.external_ip_check_enabled);
    assert_eq!(cfg.external_ip_check_interval_s, 120);
    assert_eq!(
        cfg.external_ip_check_url,
        "https://never-contact.invalid/ip"
    );
    assert!(cfg.validate().is_ok());
    for interval in ["0", "59", "604801", "18446744073709551615", "1.5"] {
        values.insert("external_ip_check_interval_s".into(), interval.into());
        assert!(
            Config::from_uci_values("privacy", &values, &HashMap::new())
                .and_then(|cfg| cfg.validate())
                .is_err(),
            "{interval}"
        );
    }
}

#[test]
fn r7_external_ip_cadence_holds_offline_inflight_and_retry_storms() {
    let now = Instant::now();
    let interval = Duration::from_secs(120);
    let (requests, receiver) = mpsc::sync_channel(1);
    let (_sender, results) = mpsc::channel();
    let mut runtime = ExternalIpRuntime {
        requests,
        results,
        in_flight: false,
        last_started: now - interval,
        interval,
    };
    runtime.maybe_start_at(false, now);
    assert!(receiver.try_recv().is_err());
    runtime.maybe_start_at(true, now);
    assert_eq!(receiver.try_recv(), Ok(()));
    runtime.maybe_start_at(true, now + interval);
    assert!(
        receiver.try_recv().is_err(),
        "only one request may be in flight"
    );
    // Both success and error completion release in_flight, not last_started.
    runtime.in_flight = false;
    runtime.maybe_start_at(true, now + interval - Duration::from_nanos(1));
    assert!(receiver.try_recv().is_err());
    runtime.maybe_start_at(true, now + interval);
    assert_eq!(receiver.try_recv(), Ok(()));
}

#[test]
fn r4_frozen_global_history_is_pure_bounded_and_ignores_foreign_section_booleans() {
    let text = "cake-autorate.globals=globals\ncake-autorate.globals.graph_history_ram_budget_kib='512'\n\
        cake-autorate.first=cake_autorate\ncake-autorate.first.enabled='1'\ncake-autorate.first.graph_history_enabled='1'\n\
        cake-autorate.second=cake_autorate\ncake-autorate.second.enabled='yes'\ncake-autorate.second.graph_history_enabled='true'\n\
        cake-autorate.foreign=unknown\ncake-autorate.foreign.enabled='private-not-a-controller-boolean'\n";
    assert_eq!(parse_global_history_config(text).unwrap(), (Some(512), 2));
    for value in ["0", "1", "18446744073709551615", "invalid-private-value"] {
        let input = text.replace("'512'", &format!("'{value}'"));
        let error = parse_global_history_config(&input).err().unwrap();
        assert!(!error.contains("private"));
    }
    for value in ["auto", ""] {
        assert_eq!(
            parse_global_history_config(&text.replace("'512'", &format!("'{value}'"))).unwrap(),
            (None, 2)
        );
    }
    assert_eq!(parse_global_history_config("").unwrap(), (None, 1));
}

#[test]
fn r3_nonfinite_scalar_is_rejected_without_replacing_the_previous_value() {
    for text in ["NaN", "inf", "-inf", "1e999"] {
        let input = HashMap::from([("alpha_delta_ewma".to_string(), text.to_string())]);
        let mut previous = 0.095;
        assert!(set_f64(&input, "alpha_delta_ewma", &mut previous).is_err());
        assert_eq!(previous, 0.095);
    }
}

#[test]
fn r3_windows_keep_the_configured_length_not_allocator_capacity() {
    for length in [0, 1, 3, 6, 60] {
        let mut booleans = filled_bool_window(length);
        let mut numbers = filled_f64_window(length);
        // Force spare allocator space: it must never become logical history.
        booleans.reserve(17);
        numbers.reserve(17);
        for index in 0..100 {
            push_window(&mut booleans, true);
            push_window(&mut numbers, index as f64);
            assert_eq!(booleans.len(), length);
            assert_eq!(numbers.len(), length);
        }
        if length > 0 {
            assert_eq!(numbers.back(), Some(&99.0));
        }
    }
}

#[test]
fn r3_config_rejects_inverted_rate_order_and_zero_bloat_window() {
    let mut cfg = Config::defaults("test".to_string());
    assert!(cfg.validate().is_ok());
    cfg.min_dl_shaper_rate_kbps = cfg.base_dl_shaper_rate_kbps + 1.0;
    assert!(cfg.validate().is_err());
    cfg.min_dl_shaper_rate_kbps = 5_000.0;
    cfg.bufferbloat_detection_window = 0;
    cfg.bufferbloat_detection_thr = 0;
    assert!(cfg.validate().is_err());
}

#[test]
fn r3_pure_candidate_parsing_keeps_url_and_interface_discovery_deferred() {
    let values = HashMap::from([
        ("enabled".to_string(), "1".to_string()),
        ("wan_if".to_string(), "candidate-wan".to_string()),
        (
            "reflectors_url".to_string(),
            "https://never-contact.invalid/list".to_string(),
        ),
        ("min_dl_shaper_rate_kbps".to_string(), "10000".to_string()),
        ("base_dl_shaper_rate_kbps".to_string(), "50000".to_string()),
        ("max_dl_shaper_rate_kbps".to_string(), "90000".to_string()),
    ]);
    let cfg = Config::from_uci_values("candidate", &values, &HashMap::new()).unwrap();
    assert_eq!(cfg.ul_if, "candidate-wan");
    assert_eq!(cfg.dl_if, "ifb4candidate-wan");
    assert_eq!(cfg.base_dl_shaper_rate_kbps, 50_000.0);
    assert_eq!(cfg.reflectors, default_reflectors());
    assert_eq!(cfg.dl_max_wire_packet_size_bits, 0);
    assert_eq!(cfg.ul_max_wire_packet_size_bits, 0);
    assert!(cfg.validate().is_ok());
    let mut invalid = values;
    invalid.insert("min_dl_shaper_rate_kbps".to_string(), "95000".to_string());
    assert!(Config::from_uci_values("candidate", &invalid, &HashMap::new()).is_err());
}

#[test]
fn r3_text_parser_selects_only_the_requested_package_and_section() {
    let cfg = Config::from_uci_text(
        "candidate",
        concat!(
            "cake-autorate.candidate.base_dl_shaper_rate_kbps='30000'\n",
            "cake-autorate.other.base_dl_shaper_rate_kbps='70000'\n",
            "unrelated.candidate.base_dl_shaper_rate_kbps='75000'\n",
        ),
    )
    .unwrap();
    assert_eq!(cfg.base_dl_shaper_rate_kbps, 30_000.0);
}

#[test]
fn r3_disabled_direction_allows_zero_tuple_but_not_mixed_invalid_rates() {
    let mut cfg = Config::defaults("test".to_string());
    cfg.adjust_ul_shaper_rate = false;
    cfg.min_ul_shaper_rate_kbps = 0.0;
    cfg.base_ul_shaper_rate_kbps = 0.0;
    cfg.max_ul_shaper_rate_kbps = 0.0;
    assert!(cfg.validate().is_ok());
    cfg.base_ul_shaper_rate_kbps = 10_000.0;
    assert!(cfg.validate().is_err());
    cfg.base_ul_shaper_rate_kbps = 0.0;
    cfg.adjust_ul_shaper_rate = true;
    assert!(cfg.validate().is_err());
}

#[test]
fn r3_numeric_invariants_reject_invalid_windows_timers_ratios_and_factors() {
    let cases: &[fn(&mut Config)] = &[
        |c| c.bufferbloat_detection_thr = 0,
        |c| c.bufferbloat_detection_window = usize::MAX,
        |c| c.reflector_misbehaving_detection_window = usize::MAX,
        |c| c.high_load_thr = 1.1,
        |c| c.alpha_delta_ewma = f64::NAN,
        |c| c.alpha_baseline_increase = -0.1,
        |c| c.alpha_baseline_decrease = 1.1,
        |c| c.global_ping_response_timeout_s = f64::INFINITY,
        |c| c.adaptive_ceiling_hold_time_s = 1e-30,
        |c| c.startup_wait_s = -1.0,
        |c| c.reflector_ping_interval_s = 0.0,
        |c| c.monitor_achieved_rates_interval_ms = 0,
        |c| c.shaper_rate_max_adjust_down_bufferbloat = 1.1,
        |c| c.shaper_rate_max_adjust_down_bufferbloat = 0.999,
        |c| c.shaper_rate_adjust_up_load_low = 0.9,
        |c| c.shaper_rate_max_adjust_up_load_high = 0.9,
        |c| c.dl_avg_owd_delta_max_adjust_up_thr_ms = 40.0,
        |c| c.ul_avg_owd_delta_max_adjust_down_thr_ms = 20.0,
    ];
    for (index, mutate) in cases.iter().enumerate() {
        let mut cfg = Config::defaults("test".to_string());
        mutate(&mut cfg);
        assert!(cfg.validate().is_err(), "invalid numeric case {index}");
    }
    let mut cfg = Config::defaults("test".to_string());
    cfg.bufferbloat_detection_window = config_validation::MAX_WINDOW_SAMPLES;
    cfg.bufferbloat_detection_thr = config_validation::MAX_WINDOW_SAMPLES;
    cfg.alpha_baseline_increase = 0.0;
    cfg.alpha_baseline_decrease = 1.0;
    assert!(cfg.validate().is_ok());
}

#[test]
fn r3_reflector_offence_count_uses_logical_history_length() {
    for length in [0, 1, 3, 60] {
        let mut state = ReflectorState::new(Instant::now(), length);
        state.offences.reserve(100);
        for _ in 0..100 {
            state.push_offence(true);
        }
        assert_eq!(state.offences.len(), length);
        assert_eq!(state.offence_sum, length);
        for _ in 0..100 {
            state.push_offence(false);
        }
        assert_eq!(state.offence_sum, 0);
        assert_eq!(state.offences.len(), length);
    }
}
