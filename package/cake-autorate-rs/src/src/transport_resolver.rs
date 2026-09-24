//! System DNS in an owned, terminable helper rather than a detached thread.
#[cfg(any(feature = "transport-probes", test))]
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs};
#[cfg(any(feature = "transport-probes", test))]
use std::os::unix::process::CommandExt;
#[cfg(any(feature = "transport-probes", test))]
use std::path::Path;
#[cfg(any(feature = "transport-probes", test))]
use std::time::Duration;

const HEADER: &str = "cake-autorate-transport-dns\t1";
const MAX_HOST_BYTES: usize = 253;
const MAX_ADDRESSES: usize = 32;
#[cfg(any(feature = "transport-probes", test))]
const MAX_RESPONSE_BYTES: usize = 4096;
#[cfg(any(feature = "transport-probes", test))]
pub(crate) const DEADLINE_ERROR: &str = "transport probe deadline exceeded during DNS resolution";
#[cfg(any(feature = "transport-probes", test))]
pub(crate) const CANCELLED_ERROR: &str = "transport DNS resolution was cancelled";

fn validate_host(host: &str) -> Result<(), String> {
    let name = host.strip_suffix('.').unwrap_or(host);
    if host.len() > MAX_HOST_BYTES
        || name.is_empty()
        || !name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
    {
        return Err("transport-dns-host-invalid".into());
    }
    Ok(())
}

pub(crate) fn serve<R: Read, W: Write>(
    parent: u32,
    reader: R,
    mut output: W,
) -> Result<(), String> {
    if parent == 0 || parent > i32::MAX as u32 {
        return Err("transport-dns-parent-invalid".into());
    }
    // SAFETY: Linux prctl takes scalar arguments only. This arms SIGKILL for
    // this helper, not any other process. The post-arm PPID check closes the
    // fork/exec/parent-exit race before stdin or DNS can block. A syscall error
    // or mismatched parent refuses the lookup. No pointers or shared memory.
    let armed = unsafe {
        libc::prctl(
            libc::PR_SET_PDEATHSIG,
            libc::SIGKILL as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
            0 as libc::c_ulong,
        )
    };
    if armed != 0 {
        return Err("transport-dns-parent-guard-failed".into());
    }
    // SAFETY: getppid has no arguments or memory preconditions.
    let actual_parent = unsafe { libc::getppid() };
    if actual_parent <= 0 || actual_parent as u32 != parent {
        return Err("transport-dns-parent-changed".into());
    }
    // The header is also the ready witness for parent-death integration tests.
    writeln!(output, "{HEADER}")
        .and_then(|_| output.flush())
        .map_err(|_| "transport-dns-output-failed".to_string())?;
    let mut request = Vec::with_capacity(MAX_HOST_BYTES + 2);
    reader
        .take((MAX_HOST_BYTES + 2) as u64)
        .read_to_end(&mut request)
        .map_err(|_| "transport-dns-input-failed".to_string())?;
    let request = std::str::from_utf8(&request).map_err(|_| "transport-dns-host-invalid")?;
    let host = request
        .strip_suffix('\n')
        .filter(|_| request.len() <= MAX_HOST_BYTES + 1)
        .ok_or("transport-dns-host-invalid")?;
    validate_host(host)?;
    let resolved = (host, 0)
        .to_socket_addrs()
        .map_err(|_| "transport-dns-lookup-failed")?;
    let mut addresses: Vec<Ipv4Addr> = Vec::with_capacity(MAX_ADDRESSES);
    for address in resolved {
        if let SocketAddr::V4(address) = address {
            let ip = *address.ip();
            if addresses.contains(&ip) {
                continue;
            }
            if addresses.len() == MAX_ADDRESSES {
                return Err("transport-dns-answer-too-large".into());
            }
            addresses.push(ip);
        }
    }
    if addresses.is_empty() {
        return Err("transport-dns-no-ipv4".into());
    }
    addresses.sort_unstable();
    for address in addresses {
        writeln!(output, "{address}").map_err(|_| "transport-dns-output-failed".to_string())?;
    }
    Ok(())
}

#[cfg(any(feature = "transport-probes", test))]
fn parse_response(bytes: &[u8], port: u16) -> Result<Vec<SocketAddr>, String> {
    if bytes.len() > MAX_RESPONSE_BYTES || !bytes.ends_with(b"\n") || bytes.contains(&b'\r') {
        return Err("transport-dns-response-invalid".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "transport-dns-response-invalid")?;
    let mut lines = text.lines();
    if lines.next() != Some(HEADER) {
        return Err("transport-dns-response-version-invalid".into());
    }
    let mut addresses = Vec::with_capacity(MAX_ADDRESSES);
    let mut previous = None;
    for line in lines {
        let ip: Ipv4Addr = line.parse().map_err(|_| "transport-dns-response-invalid")?;
        if ip.to_string() != line
            || previous.is_some_and(|last| ip <= last)
            || addresses.len() == MAX_ADDRESSES
        {
            return Err("transport-dns-response-invalid".into());
        }
        previous = Some(ip);
        addresses.push(SocketAddr::from((ip, port)));
    }
    if addresses.is_empty() {
        return Err("transport-dns-no-ipv4".into());
    }
    Ok(addresses)
}

#[cfg(any(feature = "transport-probes", test))]
pub(crate) fn resolve_with_program<F: Fn() -> bool>(
    program: &Path,
    host: &str,
    port: u16,
    timeout: Duration,
    traffic_gid: Option<u32>,
    should_cancel: F,
) -> Result<Vec<SocketAddr>, String> {
    validate_host(host)?;
    if traffic_gid.is_some_and(|gid| matches!(gid, 0 | u32::MAX)) {
        return Err("transport-dns-group-invalid".into());
    }
    let spec = crate::operations::process::SpawnSpec {
        program: program.to_path_buf(),
        arguments: vec![
            "--transport-resolve-ipv4".into(),
            std::process::id().to_string().into(),
        ],
        environment: Vec::new(),
    };
    // Keep the hostname off argv and stderr. Only the owned helper sees stdin.
    let mut input = String::with_capacity(host.len() + 1);
    let _ = writeln!(input, "{host}");
    let output = crate::operations::process::run_bounded_command_output_with_input(
        &spec,
        Some(input.as_bytes()),
        timeout,
        MAX_RESPONSE_BYTES,
        should_cancel,
        |command| {
            if let Some(gid) = traffic_gid {
                command.gid(gid);
            }
        },
    )
    .map_err(|error| match error.as_str() {
        "bounded-command-timeout" => DEADLINE_ERROR.to_string(),
        "bounded-command-cancelled" => CANCELLED_ERROR.to_string(),
        _ => "transport-dns-helper-failed".to_string(),
    })?;
    if !output.status.success() {
        return Err("transport-dns-helper-failed".into());
    }
    parse_response(&output.stdout, port)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    #[test]
    fn t2_dns_response_is_bounded_canonical_and_port_independent() {
        let valid = format!("{HEADER}\n127.0.0.1\n192.0.2.1\n");
        let parsed = parse_response(valid.as_bytes(), 443).unwrap();
        assert_eq!(
            parsed,
            vec![
                "127.0.0.1:443".parse().unwrap(),
                "192.0.2.1:443".parse().unwrap()
            ]
        );
        for invalid in [
            format!("{HEADER}\n"),
            format!("{HEADER}\n::1\n"),
            format!("{HEADER}\n127.0.0.1"),
            format!("{HEADER}\n127.0.0.1\n127.0.0.1\n"),
            format!("{HEADER}\n192.0.2.1\n127.0.0.1\n"),
            format!("{HEADER}\n127.0.0.1\n\n"),
            format!("{HEADER}\r\n127.0.0.1\r\n"),
        ] {
            assert!(
                parse_response(invalid.as_bytes(), 443).is_err(),
                "{invalid:?}"
            );
        }
        let many = format!(
            "{HEADER}\n{}",
            (1..=33)
                .map(|n| format!("192.0.2.{n}\n"))
                .collect::<String>()
        );
        assert!(parse_response(many.as_bytes(), 443).is_err());
        for host in [
            "",
            "a..b",
            "a/b",
            "name:443",
            "name\nother",
            "-host",
            "host-",
            "user@host",
        ] {
            assert!(validate_host(host).is_err());
        }
        assert!(validate_host("resolver.example.").is_ok());
    }

    #[test]
    fn t2_dns_helper_rejects_invalid_group_before_launch() {
        for gid in [0, u32::MAX] {
            assert_eq!(
                resolve_with_program(
                    Path::new("/does/not/exist"),
                    "fixture.invalid",
                    443,
                    Duration::from_secs(1),
                    Some(gid),
                    || false
                )
                .unwrap_err(),
                "transport-dns-group-invalid"
            );
        }
    }

    #[test]
    fn t2_dns_helper_receives_group_without_changing_parent_or_resolving_network_names() {
        let current_gid = || {
            fs::read_to_string("/proc/thread-self/status")
                .unwrap()
                .lines()
                .find(|line| line.starts_with("Gid:"))
                .unwrap()
                .split_whitespace()
                .nth(2)
                .unwrap()
                .parse::<u32>()
                .unwrap()
        };
        let gid = current_gid();
        if gid == 0 {
            return;
        } // Root GID is deliberately not a traffic tag.
        let dir = std::env::temp_dir().join(format!(
            "cake-dns-group-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let helper = dir.join("helper");
        fs::write(&helper, format!("#!/bin/sh\ncat >/dev/null\n[ \"$(id -g)\" = \"{gid}\" ] || exit 7\nprintf 'cake-autorate-transport-dns\\t1\\n192.0.2.1\\n'\n")).unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        for ownership in [None, Some(gid)] {
            let addresses = resolve_with_program(
                &helper,
                "fixture.invalid",
                443,
                Duration::from_secs(2),
                ownership,
                || false,
            )
            .unwrap();
            assert_eq!(
                addresses,
                vec!["192.0.2.1:443".parse::<SocketAddr>().unwrap()]
            );
            assert_eq!(current_gid(), gid);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn t2_dns_helper_deadline_and_cancel_use_owned_process_cleanup() {
        let dir = std::env::temp_dir().join(format!(
            "cake-dns-helper-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        let helper = dir.join("helper");
        fs::write(&helper, "#!/bin/sh\ncat >/dev/null\nexec sleep 30\n").unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(
            resolve_with_program(
                &helper,
                "fixture.invalid",
                443,
                Duration::from_millis(80),
                None,
                || false
            )
            .unwrap_err(),
            DEADLINE_ERROR
        );
        let start = Instant::now();
        assert_eq!(
            resolve_with_program(
                &helper,
                "fixture.invalid",
                443,
                Duration::from_secs(2),
                None,
                || start.elapsed() >= Duration::from_millis(80)
            )
            .unwrap_err(),
            CANCELLED_ERROR
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
