//! Real resolver entry point, using only numeric loopback input (no DNS traffic).
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const BINARY: &str = env!("CARGO_BIN_EXE_cake-autorated");
const HEADER: &str = "cake-autorate-transport-dns\t1\n";

fn lookup(input: &[u8], parent: u32, extra: bool) -> Output {
    let mut command = Command::new(BINARY);
    command.args(["--transport-resolve-ipv4", &parent.to_string()]);
    if extra {
        command.arg("unexpected");
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(mut stdin) = child.stdin.take() {
        // Rejection may close the pipe before this small input is written.
        let _ = stdin.write_all(input);
    }
    child.wait_with_output().unwrap()
}

#[test]
fn t2_dns_cli_numeric_input_is_canonical_and_has_no_configuration_side_effects() {
    let result = lookup(b"127.0.0.1\n", std::process::id(), false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, format!("{HEADER}127.0.0.1\n").as_bytes());
    assert!(result.stderr.is_empty());
}

#[test]
fn t2_dns_cli_refuses_extra_data_wrong_parent_and_unsafe_names() {
    for input in [
        b"".as_slice(),
        b"127.0.0.1",
        b"127.0.0.1\nsecond\n",
        b"user@private-host\n",
        b"bad/name\n",
    ] {
        let result = lookup(input, std::process::id(), false);
        assert!(!result.status.success());
        assert!(!String::from_utf8_lossy(&result.stderr).contains("private-host"));
    }
    assert!(!lookup(b"127.0.0.1\n", std::process::id(), true)
        .status
        .success());
    assert!(!lookup(b"127.0.0.1\n", u32::MAX, false).status.success());
    let wrong_parent = if std::process::id() == 1 { 2 } else { 1 };
    let mismatch = lookup(b"127.0.0.1\n", wrong_parent, false);
    assert!(!mismatch.status.success());
    assert!(
        mismatch.stdout.is_empty(),
        "parent mismatch must precede readiness and lookup"
    );
}

#[test]
fn t2_dns_helper_dies_with_parent_while_stdin_remains_open_elsewhere() {
    let (reader, writer) = UnixStream::pair().unwrap();
    let script = r#"
import os, select, signal, subprocess, sys
child = subprocess.Popen([sys.argv[1], '--transport-resolve-ipv4', str(os.getpid())],
    stdin=sys.stdin.buffer, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
if not select.select([child.stdout], [], [], 2)[0]:
    raise RuntimeError('resolver did not report readiness')
if child.stdout.readline() != b'cake-autorate-transport-dns\t1\n':
    raise RuntimeError('resolver readiness is invalid')
print(child.pid, flush=True)
signal.pause()
"#;
    let mut parent = Command::new("python3")
        .args(["-c", script, BINARY])
        .stdin(Stdio::from(OwnedFd::from(reader)))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(parent.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    if line.is_empty() {
        let mut error = String::new();
        parent
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut error)
            .unwrap();
        let _ = parent.wait();
        panic!("parent fixture failed: {error}");
    }
    let child: u32 = line.trim().parse().unwrap();
    // Kill only the actual owned parent handle, not the child's process group.
    // The outer writer stays open, so EOF cannot explain helper termination.
    parent.kill().unwrap();
    parent.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    let terminated = loop {
        match std::fs::read_to_string(format!("/proc/{child}/stat")) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break true,
            Ok(stat) if stat.rsplit_once(')').unwrap().1.split_whitespace().next() == Some("Z") => {
                break true
            }
            _ if Instant::now() >= deadline => break false,
            _ => std::thread::sleep(Duration::from_millis(5)),
        }
    };
    // Also unblocks a broken helper on failure; no arbitrary PID kill is used.
    drop(writer);
    assert!(
        terminated,
        "resolver outlived its parent with input still open"
    );
}
