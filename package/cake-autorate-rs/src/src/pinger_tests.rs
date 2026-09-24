//! Deterministic pinger timing tests: no network or wall-clock waiting.
use super::*;

#[test]
fn r5_ping_without_backend_timestamp_uses_reader_receipt_not_parse_time() {
    let mut cfg = Config::defaults("timing".into());
    cfg.pinger_method = "ping".into();
    let sample = parse_sample_line(
        &cfg,
        "64 bytes from fixture: seq=7 ttl=64 time=12.5 ms",
        "fixture",
        100.0,
    )
    .unwrap();
    assert_eq!(sample.timestamp, 100.0);
    assert_eq!(sample.rtt_ms, 12.5);
    assert!(!sample_is_stale(&sample, 100.5));
    assert!(sample_is_stale(&sample, 100.501));
}

#[test]
fn r5_iputils_timestamp_survives_pipe_delay_and_malformed_timestamp_is_rejected() {
    let sample = parse_ping_line(
        "[100.125000] 64 bytes from fixture: icmp_seq=8 ttl=64 time=1.25 ms",
        "fixture",
        102.0,
    )
    .unwrap();
    assert_eq!(sample.timestamp, 100.125);
    assert!(sample_is_stale(&sample, 102.0));
    for prefix in ["[NaN]", "[inf]", "[-1]", "[invalid]", "[100"] {
        let line = format!("{prefix} 64 bytes from fixture: icmp_seq=8 time=1.25 ms");
        assert!(parse_ping_line(&line, "fixture", 102.0).is_none());
    }
    for rtt in ["NaN", "inf", "-1"] {
        assert!(parse_ping_line(&format!("seq=1 time={rtt} ms"), "fixture", 100.0).is_none());
    }
}

#[test]
fn r5_monotonic_queue_age_rejects_delayed_samples_despite_wall_clock_rollback() {
    let observed_at = Instant::now();
    let event = PingerLine {
        line: "seq=1 time=1 ms".into(),
        reflector: "fixture".into(),
        observed_at,
        observed_epoch_secs: 100.0,
    };
    assert!(!event.is_stale_at(observed_at + Duration::from_millis(500)));
    assert!(event.is_stale_at(observed_at + Duration::from_millis(501)));
    assert!(event.is_stale_at(observed_at - Duration::from_millis(1)));
    let sample = parse_ping_line(&event.line, &event.reflector, event.observed_epoch_secs).unwrap();
    assert!(!sample_is_stale(&sample, 99.0)); // Wall time alone misses queue delay.
    let cfg = Config::defaults("timing".into());
    let mut health =
        ReflectorHealth::new(&cfg, &["fixture".into()], PingerTiming::configured(&cfg));
    health.observe_sample(&cfg, &sample, observed_at);
    assert_eq!(health.states["fixture"].last_seen, observed_at);
    health.observe_sample(&cfg, &sample, observed_at - Duration::from_millis(10));
    assert_eq!(health.states["fixture"].last_seen, observed_at);
}

#[test]
fn r5_ping_capability_plan_keeps_iputils_and_busybox_arguments_separate() {
    let mut cfg = Config::defaults("timing".into());
    cfg.pinger_method = "ping".into();
    let iputils =
        PingerPlan::from_version(&cfg, true, b"ping from iputils 20240117\n", b"").unwrap();
    let busybox = PingerPlan::from_version(&cfg, false, b"", b"ping: invalid option -- V\nBusyBox v1.37.0\nUsage: ping [OPTIONS] HOST\n -i SECS Interval\n -W SEC Wait\n -I IFACE Source\n").unwrap();
    let args = |plan: PingerPlan| {
        let mut command = Command::new("ping");
        plan.append_ping_arguments(&mut command);
        command
            .get_args()
            .map(|arg| arg.to_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(args(iputils), ["-n", "-i", "0.3", "-W", "10", "-D"]);
    assert_eq!(args(busybox), ["-n", "-i", "1", "-W", "10"]);
    assert_eq!(busybox.timing.interval_s, 1.0);
    assert_eq!(iputils.timing.interval_s, 0.3);
    assert_eq!(cfg.reflector_ping_interval_s, 0.3); // UCI intent stays immutable.
    for (success, stdout, stderr) in [
        (true, b"other ping".as_slice(), b"".as_slice()),
        (false, b"ping from iputils 20240117", b""),
        (false, b"", b"BusyBox v1.37.0\nUsage: ping HOST\n"),
        (true, b"\xff", b""),
    ] {
        assert!(PingerPlan::from_version(&cfg, success, stdout, stderr).is_err());
    }
}

#[test]
fn r5_clustered_one_second_replies_and_jitter_do_not_create_stall_or_health_churn() {
    let mut cfg = Config::defaults("timing".into());
    cfg.pinger_method = "ping".into();
    cfg.no_pingers = 6;
    cfg.monitor_achieved_rates_interval_ms = 100;
    cfg.min_dl_shaper_rate_kbps = 100_000.0;
    cfg.min_ul_shaper_rate_kbps = 100_000.0;
    cfg.reflector_response_deadline_s = 1.0;
    let timing = PingerTiming::configured(&cfg);
    assert_eq!(timing.stall_timeout, Duration::from_millis(1250));
    assert_eq!(timing.response_deadline, Duration::from_millis(2250));
    // Six replies may all share a phase, so the next gap is a full second,
    // not the 1/6-second mean; alternate +-50ms arrival jitter for 20 cycles.
    let mut last = 0.0_f64;
    for cycle in 1..=20 {
        let arrival = f64::from(cycle) + if cycle % 2 == 0 { 0.05 } else { -0.05 };
        assert!(arrival - last < timing.stall_timeout.as_secs_f64());
        assert!(arrival - last < timing.response_deadline.as_secs_f64());
        last = arrival;
    }
    assert!(2.1 < timing.response_deadline.as_secs_f64()); // One missing reply.
    assert!(2.3 > timing.response_deadline.as_secs_f64()); // Real sustained loss.
    cfg.reflector_ping_interval_s = 2.0;
    let slower = PingerTiming::configured(&cfg);
    assert_eq!(slower.stall_timeout, Duration::from_millis(2500));
    assert_eq!(slower.response_deadline, Duration::from_millis(4500));
}

#[test]
fn r5_capability_query_uses_configured_prefix_but_never_passes_a_destination() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!(
        "cake-ping-capability-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    let wrapper = root.join("prefix");
    fs::write(&wrapper, "#!/bin/sh\n[ \"$*\" = 'ping -V' ] || exit 91\n[ \"$LC_ALL\" = C ] || exit 92\nprintf '%s\\n' 'ping from iputils fixture'\n").unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    let mut cfg = Config::defaults("timing".into());
    cfg.pinger_method = "ping".into();
    cfg.ping_prefix_string = wrapper.to_str().unwrap().into();
    let plan = PingerPlan::detect(&cfg).unwrap();
    assert_eq!(plan.name(), "iputils-timestamped");
    assert_eq!(plan.timing.interval_s, cfg.reflector_ping_interval_s);
    let command = pinger_command(&cfg, "ping", None).unwrap();
    assert!(command
        .get_envs()
        .any(|(name, value)| name == "LC_ALL" && value == Some(std::ffi::OsStr::new("C"))));
}

#[test]
fn r6_permanent_probe_group_rejects_invalid_authority_and_legacy_prefix() {
    let mut cfg = Config::defaults("owned-probe".into());
    for binary in ["fping", "ping", "tsping", "irtt"] {
        for gid in [0, u32::MAX] {
            assert!(pinger_command(&cfg, binary, Some(gid)).is_err());
        }
        let command = pinger_command(&cfg, binary, Some(40000)).unwrap();
        assert_eq!(command.get_program(), std::ffi::OsStr::new(binary));
    }
    cfg.ping_prefix_string = "mwan3 use wan exec".into();
    assert!(pinger_command(&cfg, "fping", Some(40000)).is_err());
    assert!(pinger_command(&cfg, "fping", None).is_ok());
}

#[test]
#[ignore = "requires private user/network namespaces with UID 0 and only GID 40000 mapped"]
fn r6_permanent_probe_child_credentials_kernel_fixture() {
    let parent_namespace = std::env::var("CAKE_R6_PARENT_USERNS").unwrap();
    assert_ne!(
        fs::read_link("/proc/self/ns/user")
            .unwrap()
            .to_str()
            .unwrap(),
        parent_namespace
    );
    let status = fs::read_to_string("/proc/self/status").unwrap();
    let credentials = |text: &str| {
        text.lines()
            .filter(|line| line.starts_with("Uid:") || line.starts_with("Gid:"))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let before = credentials(&status);
    let cfg = Config::defaults("owned-probe-kernel".into());
    let output = pinger_command(&cfg, "/bin/cat", Some(40000))
        .unwrap()
        .arg("/proc/self/status")
        .output()
        .unwrap();
    assert!(output.status.success());
    let child_status = String::from_utf8(output.stdout).unwrap();
    for (name, value) in [("Uid:", "0"), ("Gid:", "40000")] {
        let fields: Vec<_> = child_status
            .lines()
            .find(|line| line.starts_with(name))
            .unwrap()
            .split_whitespace()
            .skip(1)
            .collect();
        assert_eq!(fields, vec![value; 4]);
    }
    // This group is deliberately unmapped. Failure must occur before exec,
    // never by dropping the requested owner and launching the command anyway.
    assert!(pinger_command(&cfg, "/bin/true", Some(40001))
        .unwrap()
        .status()
        .is_err());
    assert_eq!(
        credentials(&fs::read_to_string("/proc/self/status").unwrap()),
        before
    );
}

#[test]
fn r6_partial_pinger_reader_setup_reaps_all_children() {
    let mut children = Vec::new();
    for piped in [true, false] {
        children.push(
            Command::new("sh")
                .args(["-c", "read line"])
                .stdin(Stdio::piped())
                .stdout(if piped { Stdio::piped() } else { Stdio::null() })
                .spawn()
                .unwrap(),
        );
    }
    let pids: Vec<_> = children.iter().map(Child::id).collect();
    let result = PingerRuntime::from_children(
        children,
        "ping".into(),
        &["first".into(), "second".into()],
        None,
        None,
    );
    assert!(matches!(result, Err(ref error) if error == "failed to capture ping stdout"));
    for pid in pids {
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "child was not reaped"
        );
    }
}

#[test]
fn r5_one_child_eof_is_reported_while_another_pinger_remains_alive() {
    let exited = Command::new("sh")
        .args(["-c", "exit 0"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let alive = Command::new("sh")
        .args(["-c", "read line"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut runtime = PingerRuntime::from_children(
        vec![exited, alive],
        "ping".into(),
        &["first".into(), "second".into()],
        None,
        None,
    )
    .unwrap();
    let event = runtime.lines.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(matches!(event, Err(ref error) if error == "ping output closed"));
    assert!(runtime.children[1].try_wait().unwrap().is_none());
    runtime.stop();
    assert!(runtime.children.is_empty());
    assert!(runtime.readers.is_empty());
}
