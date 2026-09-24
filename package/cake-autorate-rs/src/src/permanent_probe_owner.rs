//! Permanent monitoring authority, separate from bounded Auto-Tune capture.
//! Dropping an owner quarantines its group and retains its routing fence.
//! Explicit retirement requires all producers stopped and reaped first.

use crate::owned_route_rules::{
    attest_owned_probe_counters, attest_route_pin_snapshot, cleanup_named_route_pin_with,
    install_owned_route_pin_with, nft_table_snapshot_arguments, NftSocketOwner, OwnedProbeCounters,
};
use crate::probe_owner::{ProbeGroupLease, ProbeGroupPool};
use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;

pub(crate) struct PermanentProbeOwner {
    authority: Rc<PermanentProbeAuthority>,
}

struct PermanentProbeAuthority {
    system: bool,
    route: crate::routing::RouteIdentity,
    unverified_stop: Cell<bool>,
    lease: ProbeGroupLease,
    table: String,
    owner: String,
    handle: u64,
}

/// Retained by the main-thread runtime until its children/worker are joined.
/// Workers receive only the GID. Rc keeps ownership arbitration single-threaded.
pub(crate) struct PermanentProbeProducer {
    authority: Rc<PermanentProbeAuthority>,
    stopped: bool,
}

impl PermanentProbeProducer {
    pub(crate) fn route(&self) -> &crate::routing::RouteIdentity {
        &self.authority.route
    }

    pub(crate) fn gid(&self) -> u32 {
        self.authority.lease.gid()
    }

    /// Called only after all processes and threads of this producer are joined.
    pub(crate) fn confirm_stopped(mut self) {
        self.stopped = true;
    }
}

impl Drop for PermanentProbeProducer {
    fn drop(&mut self) {
        if !self.stopped {
            // A lost token or uncertain wait is not evidence of quiescence.
            // Keep this sticky even when every token has subsequently gone.
            self.authority.unverified_stop.set(true);
        }
    }
}

impl PermanentProbeOwner {
    pub(crate) fn matches_route(&self, route: &crate::routing::RouteIdentity) -> bool {
        self.authority.route == *route
    }

    pub(crate) fn has_producers(&self) -> bool {
        Rc::strong_count(&self.authority) != 1
    }

    pub(crate) fn admit_producer(&self) -> Result<PermanentProbeProducer, String> {
        if self.authority.unverified_stop.get() {
            return Err("permanent-probe-stop-unverified".into());
        }
        Ok(self.producer())
    }

    /// Exclusive bounded helper, intended for route-bound reflector scans.
    /// Caller supplies source/device argv and reattests route before/after.
    /// An uncertain command error poisons the lease instead of releasing its fence.
    pub(crate) fn run_owned_command(
        &self,
        spec: &crate::operations::process::SpawnSpec,
        timeout: std::time::Duration,
        output_limit: usize,
        should_cancel: impl Fn() -> bool,
    ) -> Result<crate::operations::process::BoundedCommandOutput, String> {
        use std::os::unix::process::CommandExt;
        spec.validate()?;
        if !self.authority.system || self.has_producers() {
            return Err("owned probe command requires an exclusive system owner".into());
        }
        crate::probe_owner::attest_system_probe_namespace()?;
        self.authority.lease.attest_processes_absent()?;
        self.counters_with(|args| execute_system_nft(args, None))?;
        let producer = self.admit_producer()?;
        let gid = producer.gid();
        let output = crate::operations::process::run_bounded_command_output_with_input(
            spec,
            None,
            timeout,
            output_limit,
            should_cancel,
            |command| {
                command.uid(0).gid(gid);
            },
        )?;
        self.authority.lease.attest_processes_absent()?;
        producer.confirm_stopped();
        self.counters_with(|args| execute_system_nft(args, None))?;
        Ok(output)
    }

    /// Production entry: fixed account/root paths and fresh generation, after
    /// proving that procfs inventory covers the initial PID/user namespaces.
    pub(crate) fn acquire_system(
        route: &crate::routing::RouteSnapshot,
        execute: impl FnMut(&[&str], Option<&[u8]>) -> Result<(bool, Vec<u8>), String>,
    ) -> Result<Self, String> {
        use std::fmt::Write;
        use std::io::Read;
        crate::probe_owner::attest_system_probe_namespace()?;
        let pool = ProbeGroupPool::read_at(Path::new("/etc"), 0)?;
        let root = crate::probe_owner::prepare_boot_lease_root()?;
        let mut random = [0_u8; 32];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut random))
            .map_err(|_| "permanent-probe-generation-unavailable")?;
        let mut generation = String::with_capacity(64);
        for byte in random {
            write!(&mut generation, "{byte:02x}")
                .map_err(|_| "permanent-probe-generation-unavailable")?;
        }
        let mut owner = Self::acquire(&pool, &root, 0, &generation, route, execute)?;
        Rc::get_mut(&mut owner.authority)
            .ok_or("permanent-probe-initial-authority-shared")?
            .system = true;
        Ok(owner)
    }

    /// The caller has already attested the route and root controller identity.
    pub(crate) fn acquire(
        pool: &ProbeGroupPool,
        lease_root: &Path,
        owner_uid: u32,
        generation: &str,
        route: &crate::routing::RouteSnapshot,
        mut execute: impl FnMut(&[&str], Option<&[u8]>) -> Result<(bool, Vec<u8>), String>,
    ) -> Result<Self, String> {
        let identity = &route.identity;
        let device = identity.device.as_str();
        if !route.online || !crate::routing::is_safe_identifier(device) {
            return Err("permanent-probe-device-invalid".into());
        }
        let source = identity
            .source_ip
            .parse::<std::net::Ipv4Addr>()
            .map_err(|_| "permanent-probe-source-invalid")?;
        if source.octets()[0] == 0 || source.is_loopback() || source.octets()[0] >= 224 {
            return Err("permanent-probe-source-invalid".into());
        }
        let mark = match identity.mode.as_str() {
            "main"
                if matches!(identity.table.as_str(), "main" | "254")
                    && identity.member.is_empty()
                    && identity.fwmark_mask.is_none()
                    && (identity.fwmark.is_empty()
                        || crate::routing::parse_route_u32(&identity.fwmark) == Some(0)) =>
            {
                None
            }
            "mwan3" | "explicit"
                if crate::routing::parse_route_u32(&identity.table)
                    .is_some_and(|table| table != 0) =>
            {
                Some((
                    crate::routing::parse_route_u32(&identity.fwmark)
                        .ok_or("permanent-probe-mark-invalid")?,
                    identity.fwmark_mask.ok_or("permanent-probe-mask-missing")?,
                ))
            }
            _ => return Err("permanent-probe-route-invalid".into()),
        };
        if mark.is_some_and(|(value, mask)| mask == 0 || value == 0 || value & !mask != 0) {
            return Err("permanent-probe-mark-invalid".into());
        }
        // A crashed owner's table carries its exact generation comment. Delete
        // only that attested kernel object; a replacement or ambiguous listing
        // leaves the slot fenced for manual recovery.
        let lease = ProbeGroupLease::acquire_recovering(
            pool,
            lease_root,
            owner_uid,
            generation,
            |gid, stale| {
                cleanup_named_route_pin_with(
                    &format!("cake_pm_{gid}"),
                    &format!("cake-permanent-probe-v1:{stale}"),
                    |args| execute(args, None),
                )
            },
        )?;
        let table = format!("cake_pm_{}", lease.gid());
        let owner = format!("cake-permanent-probe-v1:{generation}");
        // No preemptive named cleanup. A collision or ambiguous command result
        // leaves the lease occupied for recovery, never adopts another table.
        install_owned_route_pin_with(
            &table,
            &owner,
            NftSocketOwner::ProbeRootGid(lease.gid()),
            mark.map(|(value, mask)| (!mask, value)),
            device,
            |args, input| execute(args, Some(input)).map(|output| output.0),
        )?;
        let (ok, snapshot) = execute(&nft_table_snapshot_arguments(&table), None)?;
        if !ok {
            return Err("permanent-probe-install-unverified".into());
        }
        let handle = attest_route_pin_snapshot(&snapshot, &table, &owner)?;
        Ok(Self {
            authority: Rc::new(PermanentProbeAuthority {
                system: false,
                route: identity.clone(),
                unverified_stop: Cell::new(false),
                lease,
                table,
                owner,
                handle,
            }),
        })
    }

    pub(crate) fn producer(&self) -> PermanentProbeProducer {
        PermanentProbeProducer {
            authority: Rc::clone(&self.authority),
            stopped: false,
        }
    }

    /// Executor must bound command duration and output, as for installation.
    /// Absence/failure is unknown accounting, never a zero-byte observation.
    pub(crate) fn counters_with(
        &self,
        mut execute: impl FnMut(&[&str]) -> Result<(bool, Vec<u8>), String>,
    ) -> Result<OwnedProbeCounters, String> {
        if self.authority.system {
            crate::probe_owner::attest_system_probe_namespace()?;
        }
        if self.authority.unverified_stop.get() {
            return Err("permanent-probe-stop-unverified".into());
        }
        let (ok, snapshot) = execute(&nft_table_snapshot_arguments(&self.authority.table))?;
        if !ok {
            return Err("permanent-probe-counter-inspection-failed".into());
        }
        attest_owned_probe_counters(
            &snapshot,
            &self.authority.table,
            &self.authority.owner,
            self.authority.handle,
        )
    }

    pub(crate) fn retire(
        self,
        mut execute: impl FnMut(&[&str]) -> Result<(bool, Vec<u8>), String>,
    ) -> Result<(), String> {
        if self.authority.system {
            crate::probe_owner::attest_system_probe_namespace()?;
        }
        // A worker restores fsGID after opening a group-owned socket, so /proc
        // credentials cannot prove socket quiescence. Tokens outlive the worker.
        let authority = Rc::try_unwrap(self.authority)
            .map_err(|_| "permanent-probe-producers-still-owned".to_string())?;
        if authority.unverified_stop.get() {
            return Err("permanent-probe-stop-unverified".into());
        }
        let mut cleanup_error = None;
        let result = authority.lease.retire(|| {
            let cleaned =
                cleanup_named_route_pin_with(&authority.table, &authority.owner, |args| {
                    let output = execute(args)?;
                    // Validate the snapshot used by shared cleanup itself, not a
                    // separate preflight which could race a same-comment replacement.
                    if args == nft_table_snapshot_arguments(&authority.table)
                        && output.0
                        && attest_route_pin_snapshot(&output.1, &authority.table, &authority.owner)?
                            != authority.handle
                    {
                        return Err("permanent-probe-table-generation-changed".into());
                    }
                    Ok(output)
                });
            cleaned.map_err(|error| {
                cleanup_error = Some(error);
                "permanent-probe-cleanup-failed"
            })
        });
        result.map_err(|error| cleanup_error.unwrap_or_else(|| error.to_string()))
    }
}

pub(crate) fn execute_system_nft(
    arguments: &[&str],
    input: Option<&[u8]>,
) -> Result<(bool, Vec<u8>), String> {
    use crate::operations::process::{run_bounded_command_output_with_input, SpawnSpec};
    use std::os::unix::fs::MetadataExt;
    let binary = Path::new("/usr/sbin/nft");
    if std::fs::metadata(binary)
        .map_err(|_| "permanent-probe-nft-unavailable")?
        .uid()
        != 0
    {
        return Err("permanent-probe-nft-owner-invalid".into());
    }
    let spec = SpawnSpec {
        program: binary.into(),
        arguments: arguments.iter().map(std::ffi::OsString::from).collect(),
        environment: Vec::new(),
    };
    let output = run_bounded_command_output_with_input(
        &spec,
        input,
        std::time::Duration::from_secs(2),
        256 * 1024,
        || false,
        |_| {},
    )?;
    Ok((output.status.success(), output.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    fn route(marked: bool) -> crate::routing::RouteSnapshot {
        crate::routing::RouteSnapshot {
            identity: crate::routing::RouteIdentity {
                device_ifindex: None,
                mode: if marked { "mwan3" } else { "main" }.into(),
                member: if marked { "wan" } else { "" }.into(),
                device: "eth1".into(),
                source_ip: "192.0.2.2".into(),
                table: if marked { "101" } else { "main" }.into(),
                fwmark: if marked { "0x100" } else { "" }.into(),
                fwmark_mask: marked.then_some(0x3f00),
            },
            online: true,
            active: true,
            member_status: "online".into(),
            reason: String::new(),
        }
    }

    /// Run only on the test VM in a fresh network namespace. No packets are
    /// generated; the real system allocator and nft executor are exercised.
    #[test]
    #[ignore = "requires initial root namespaces and isolated VM network namespace"]
    fn r6_system_owner_vm_lifecycle() {
        crate::probe_owner::attest_system_probe_namespace().unwrap();
        let own = fs::metadata("/proc/self/ns/net").unwrap();
        let init = fs::metadata("/proc/1/ns/net").unwrap();
        assert_ne!((own.dev(), own.ino()), (init.dev(), init.ino()));
        let (ok, before) = execute_system_nft(&["-j", "list", "tables"], None).unwrap();
        assert!(ok);
        let before: serde_json::Value = serde_json::from_slice(&before).unwrap();
        assert!(!before["nftables"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.get("table").is_some()));

        for marked in [false, true] {
            let mut selected = route(marked);
            selected.identity.device = "lo".into();
            if marked {
                selected.identity.mode = "explicit".into();
                selected.identity.member.clear();
            }
            let owner = PermanentProbeOwner::acquire_system(&selected, execute_system_nft).unwrap();
            assert!(owner.authority.system);
            let table = owner.authority.table.clone();
            let counters = owner
                .counters_with(|args| execute_system_nft(args, None))
                .unwrap();
            assert_eq!(counters.rx_bytes, 0);
            assert_eq!(counters.tx_bytes, 0);
            let producer = owner.admit_producer().unwrap();
            assert!(owner.has_producers());
            assert_eq!(producer.route(), &selected.identity);
            producer.confirm_stopped();
            assert!(!owner.has_producers());
            let (program, arguments) = if Path::new("/usr/bin/id").is_file() {
                ("/usr/bin/id", vec!["-g".into()])
            } else {
                ("/bin/busybox", vec!["id".into(), "-g".into()])
            };
            let output = owner
                .run_owned_command(
                    &crate::operations::process::SpawnSpec {
                        program: program.into(),
                        arguments,
                        environment: Vec::new(),
                    },
                    std::time::Duration::from_secs(2),
                    1024,
                    || false,
                )
                .unwrap();
            assert!(output.status.success());
            assert_eq!(
                String::from_utf8(output.stdout).unwrap().trim(),
                owner.authority.lease.gid().to_string()
            );
            assert!(!owner.has_producers());
            owner.retire(|args| execute_system_nft(args, None)).unwrap();
            let (ok, tables) = execute_system_nft(&["-j", "list", "tables"], None).unwrap();
            assert!(ok);
            let tables: serde_json::Value = serde_json::from_slice(&tables).unwrap();
            assert!(!tables["nftables"].as_array().unwrap().iter().any(|item| {
                item.get("table")
                    .is_some_and(|entry| entry["name"] == table)
            }));
        }
    }

    #[test]
    fn r6_runtime_tokens_survive_until_producers_are_joined() {
        use std::process::{Command, Stdio};
        let root = std::env::temp_dir().join(format!("cake-owner-runtime-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let uid = fs::metadata(&root).unwrap().uid();
        let groups: String = (0..64)
            .map(|i| format!("cake-probe-{i:02}:x:{}:\n", 41000 + i))
            .collect();
        let pool =
            ProbeGroupPool::from_account_text(&groups, "root:x:0:0::/:/bin/false\n").unwrap();
        let mut snapshot = Vec::new();
        for invalid in 0..6 {
            let mut bad = route(true);
            match invalid {
                0 => bad.online = false,
                1 => bad.identity.source_ip = "::1".into(),
                2 => bad.identity.fwmark_mask = None,
                3 => bad.identity.fwmark = "0x10000".into(),
                4 => bad.identity.table = "unknown".into(),
                _ => bad.identity.mode = "unknown".into(),
            }
            assert!(PermanentProbeOwner::acquire(
                &pool,
                &root,
                uid,
                &"d".repeat(64),
                &bad,
                |_, _| panic!("invalid route must not install nft rules")
            )
            .is_err());
            assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        }
        let owner = PermanentProbeOwner::acquire(
            &pool,
            &root,
            uid,
            &"d".repeat(64),
            &route(false),
            |_, input| {
                if let Some(input) = input {
                    let batch: serde_json::Value = serde_json::from_slice(input).unwrap();
                    let mut table = batch["nftables"][0]["create"]["table"].clone();
                    table["handle"] = 42.into();
                    snapshot =
                        serde_json::to_vec(&serde_json::json!({"nftables":[{"table":table}]}))
                            .unwrap();
                    Ok((true, Vec::new()))
                } else {
                    Ok((true, snapshot.clone()))
                }
            },
        )
        .unwrap();
        let child = Command::new("sh")
            .args(["-c", "read line"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id();
        let mut pinger = crate::PingerRuntime::from_children(
            vec![child],
            "ping".into(),
            &["fixture".into()],
            None,
            Some(owner.producer()),
        )
        .unwrap();
        assert_eq!(Rc::strong_count(&owner.authority), 2);
        assert!(pinger.children[0].try_wait().unwrap().is_none());
        pinger.stop();
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
        assert_eq!(Rc::strong_count(&owner.authority), 1);
        // A partially initialized reader must also reap before releasing its token.
        let child = Command::new("sh")
            .args(["-c", "read line"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        assert!(crate::PingerRuntime::from_children(
            vec![child],
            "ping".into(),
            &["fixture".into()],
            None,
            Some(owner.producer()),
        )
        .is_err());
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
        assert_eq!(Rc::strong_count(&owner.authority), 1);

        #[cfg(all(feature = "transport-probes", feature = "calibration"))]
        {
            use std::sync::mpsc;
            use std::time::{Duration, Instant};
            let (requests, request_rx) = mpsc::sync_channel(1);
            let (result_tx, results) = mpsc::channel();
            let (release, wait) = mpsc::sync_channel(1);
            let worker = std::thread::spawn(move || {
                let _channels = (request_rx, result_tx);
                wait.recv_timeout(Duration::from_secs(5)).unwrap();
            });
            let mut runtime = crate::TransportProbeRuntime::from_worker(
                &crate::Config::defaults("fixture".into()),
                requests,
                results,
                worker,
                Some(owner.producer()),
            );
            runtime.request_stop();
            assert!(!runtime.finish_stop());
            assert_eq!(Rc::strong_count(&owner.authority), 2);
            release.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !runtime.finish_stop() {
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            }
            assert_eq!(Rc::strong_count(&owner.authority), 1);
        }
        assert!(!owner.authority.unverified_stop.get());
        let lease_path = root.join(format!("group-{}.lease", owner.authority.lease.gid()));
        let mut failed_reader = crate::PingerRuntime::from_children(
            Vec::new(),
            "ping".into(),
            &[],
            None,
            Some(owner.producer()),
        )
        .unwrap();
        failed_reader
            .readers
            .push(std::thread::spawn(|| panic!("injected reader failure")));
        failed_reader.stop();
        assert_eq!(Rc::strong_count(&owner.authority), 1);
        assert!(owner.authority.unverified_stop.get());
        assert!(matches!(owner.admit_producer(), Err(ref error)
            if error == "permanent-probe-stop-unverified"));
        let mut wrong_route = route(false);
        wrong_route.identity.source_ip = "192.0.2.3".into();
        let cfg = crate::Config::defaults("fixture".into());
        let mut plan = crate::PingerPlan::configured(&cfg);
        let mismatched =
            crate::PingerRuntime::spawn(&cfg, &[], &mut plan, &wrong_route, Some(owner.producer()));
        assert!(matches!(mismatched, Err(ref error)
            if error == "permanent pinger route differs from its owner"));
        assert_eq!(
            owner.counters_with(|_| panic!("failed owner must not supply accounting")),
            Err("permanent-probe-stop-unverified".into())
        );
        // A later successful stop must never clear an earlier failure.
        owner.producer().confirm_stopped();
        assert_eq!(
            owner.retire(|_| panic!("unverified stop must not remove routing")),
            Err("permanent-probe-stop-unverified".into())
        );
        assert!(!fs::read(lease_path).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn r6_permanent_owner_keeps_lease_until_exact_route_cleanup() {
        let root =
            std::env::temp_dir().join(format!("cake-permanent-owner-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let uid = fs::metadata(&root).unwrap().uid();
        let groups: String = (0..64)
            .map(|i| format!("cake-probe-{i:02}:x:{}:\n", 40000 + i))
            .collect();
        let pool =
            ProbeGroupPool::from_account_text(&groups, "root:x:0:0::/:/bin/false\n").unwrap();
        for (changed_handle, retain_producer) in [(false, false), (true, false), (false, true)] {
            let mut snapshot = Vec::new();
            let mut installs = 0;
            let owner = PermanentProbeOwner::acquire(
                &pool,
                &root,
                uid,
                &"a".repeat(64),
                &route(true),
                |args, input| {
                    if let Some(input) = input {
                        installs += 1;
                        assert_eq!(args, ["-j", "-f", "-"]);
                        let batch: serde_json::Value = serde_json::from_slice(input).unwrap();
                        let mut table = batch["nftables"][0]["create"]["table"].clone();
                        table["handle"] = 42.into();
                        snapshot =
                            serde_json::to_vec(&serde_json::json!({"nftables":[{"table":table}]}))
                                .unwrap();
                        Ok((true, Vec::new()))
                    } else {
                        Ok((true, snapshot.clone()))
                    }
                },
            )
            .unwrap();
            assert_eq!(installs, 1);
            assert_eq!(
                owner.counters_with(|args| {
                    assert_eq!(args, nft_table_snapshot_arguments(&owner.authority.table));
                    Ok((false, Vec::new()))
                }),
                Err("permanent-probe-counter-inspection-failed".into())
            );
            let mut counter_snapshot: serde_json::Value =
                serde_json::from_slice(&snapshot).unwrap();
            for (name, bytes) in [("rx", 12), ("tx", 34), ("flow_fault", 0)] {
                counter_snapshot["nftables"].as_array_mut().unwrap().push(
                    serde_json::json!({"counter":{
                        "name":name, "family":"inet", "table":owner.authority.table,
                        "comment":format!("{}:flow-v1", owner.authority.owner), "bytes":bytes
                    }}),
                );
            }
            let counters = serde_json::to_vec(&counter_snapshot).unwrap();
            assert_eq!(
                owner
                    .counters_with(|_| Ok((true, counters.clone())))
                    .unwrap(),
                OwnedProbeCounters {
                    rx_bytes: 12,
                    tx_bytes: 34
                }
            );
            counter_snapshot["nftables"][0]["table"]["handle"] = 43.into();
            let replaced = serde_json::to_vec(&counter_snapshot).unwrap();
            assert_eq!(
                owner.counters_with(|_| Ok((true, replaced.clone()))),
                Err("permanent-probe-counter-generation-changed".into())
            );
            let producer = owner.producer();
            let gid = producer.gid();
            let retained = if retain_producer {
                Some(producer)
            } else {
                producer.confirm_stopped();
                None
            };
            let lease_path = root.join(format!("group-{gid}.lease"));
            assert!(!fs::read(&lease_path).unwrap().is_empty());
            if changed_handle {
                let mut replaced: serde_json::Value = serde_json::from_slice(&snapshot).unwrap();
                replaced["nftables"][0]["table"]["handle"] = 43.into();
                snapshot = serde_json::to_vec(&replaced).unwrap();
            }
            let mut deleted = false;
            let result = owner.retire(|args| {
                assert!(!retain_producer, "runtime token must prevent nft cleanup");
                assert!(!fs::read(&lease_path).unwrap().is_empty());
                if args.first() == Some(&"delete") {
                    assert!(!changed_handle);
                    assert_eq!(args, ["delete", "table", "inet", "handle", "42"]);
                    deleted = true;
                    Ok((true, Vec::new()))
                } else if args == ["-j", "list", "tables"] {
                    Ok((true, br#"{"nftables":[]}"#.to_vec()))
                } else if deleted {
                    Ok((false, Vec::new()))
                } else {
                    Ok((true, snapshot.clone()))
                }
            });
            let retired = !changed_handle && !retain_producer;
            assert_eq!(result.is_ok(), retired);
            assert_eq!(deleted, retired);
            assert_eq!(fs::read(&lease_path).unwrap().is_empty(), retired);
            if let Some(producer) = retained {
                assert_eq!(result, Err("permanent-probe-producers-still-owned".into()));
                assert_eq!(producer.gid(), gid);
                drop(producer);
                assert!(!fs::read(&lease_path).unwrap().is_empty());
            }
        }
        let mut calls = 0;
        let mut recovery_listings = Vec::new();
        let failed = PermanentProbeOwner::acquire(
            &pool,
            &root,
            uid,
            &"b".repeat(64),
            &route(false),
            |args, input| {
                if input.is_none() {
                    // Crashed generation-a slots are offered for recovery; an
                    // unavailable listing is not absence and keeps them fenced.
                    recovery_listings.push(args.join(" "));
                    return Err("recovery-listing-unavailable".into());
                }
                calls += 1;
                Err("ambiguous-install-result".into())
            },
        );
        assert!(matches!(failed, Err(ref error) if error == "ambiguous-install-result"));
        assert_eq!(
            calls, 1,
            "ambiguous installation must not trigger speculative deletion"
        );
        // Parallel tests may briefly carry one fixture GID on a thread; the
        // live-task check then skips that slot, which is the intended fence.
        let offered = [40000, 40001]
            .map(|gid| nft_table_snapshot_arguments(&format!("cake_pm_{gid}")).join(" "));
        assert!(!recovery_listings.is_empty());
        assert!(recovery_listings
            .iter()
            .all(|listing| offered.contains(listing)));
        assert!(!fs::read(root.join("group-40002.lease")).unwrap().is_empty());
        let next = ProbeGroupLease::acquire(&pool, &root, uid, &"c".repeat(64)).unwrap();
        assert_eq!(next.gid(), 40003, "all failed owners remain quarantined");
        next.retire(|| Ok(())).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
