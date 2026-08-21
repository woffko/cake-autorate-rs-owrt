//! Small, typed procd service-control operations shared by ordinary service
//! lifecycle and native Apply.

use super::process::{run_bounded_command_output_with_input, SpawnSpec};
use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_OUTPUT: usize = 64 * 1024;

// `ubus` returns the negative libubus status. UBUS_STATUS_NOT_FOUND is 4,
// therefore a POSIX shell observes (-4 & 0xff) as exit status 252. This is
// deliberately narrower than accepting an arbitrary failed delete: procd may
// otherwise retain a respawning service while the caller observes a temporary
// process-free gap.
const UBUS_NOT_FOUND_EXIT_STATUS: i32 = 252;

pub(crate) fn delete_service_or_attest_absent(
    ubus: &Path,
    request: &str,
    operation: &str,
) -> Result<(), String> {
    let output = run_bounded_command_output_with_input(
        &SpawnSpec {
            program: ubus.to_path_buf(),
            arguments: vec![
                OsString::from("call"),
                OsString::from("service"),
                OsString::from("delete"),
                OsString::from(request),
            ],
            environment: Vec::new(),
        },
        None,
        COMMAND_TIMEOUT,
        MAX_OUTPUT,
        || false,
        |_| {},
    )?;
    if output.status.success() || output.status.code() == Some(UBUS_NOT_FOUND_EXIT_STATUS) {
        return Ok(());
    }
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if detail.is_empty() {
        format!("unable to {operation}: {}", output.status)
    } else {
        format!("unable to {operation}: {detail}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);

    fn stub(exit_status: i32, stderr: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "cake-procd-control-{}-{}",
            std::process::id(),
            NEXT_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let program = root.join("ubus");
        fs::write(
            &program,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{}' >&2\nexit {exit_status}\n",
                stderr
            ),
        )
        .unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        (root, program)
    }

    #[test]
    fn delete_is_idempotent_only_for_exact_ubus_not_found_status() {
        let request = r#"{"name":"cake-autorate"}"#;

        for (status, expected_ok) in [(0, true), (UBUS_NOT_FOUND_EXIT_STATUS, true), (2, false)] {
            let (root, program) = stub(status, "synthetic ubus result");
            let result = delete_service_or_attest_absent(
                &program,
                request,
                "delete the synthetic procd service",
            );
            assert_eq!(result.is_ok(), expected_ok, "exit status {status}");
            if !expected_ok {
                assert!(result.unwrap_err().contains("synthetic ubus result"));
            }
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn two_absent_deletes_are_both_successful() {
        let (root, program) = stub(UBUS_NOT_FOUND_EXIT_STATUS, "Not found");
        let request = r#"{"name":"cake-autorate"}"#;
        delete_service_or_attest_absent(&program, request, "delete the procd service").unwrap();
        delete_service_or_attest_absent(&program, request, "delete the procd service").unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
