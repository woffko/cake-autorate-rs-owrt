//! Small, typed procd service-control operations shared by ordinary service
//! lifecycle and native Apply.

use super::process::{run_bounded_command_output_with_input, SpawnSpec};
use std::collections::{BTreeMap, BTreeSet};
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

/// Query only our service; never expose the returned environment or errors.
/// None denotes a pending registration/respawn, not permission to adopt it.
pub(crate) fn controller_generations(
    ubus: &Path,
    expected: &BTreeMap<String, String>,
) -> Result<Option<BTreeMap<String, u32>>, String> {
    parse_controller_generations(&query_service(ubus)?, expected)
}

pub(crate) fn controller_batch_generations(
    ubus: &Path,
    expected: &BTreeMap<String, String>,
) -> Result<Option<BTreeMap<String, u32>>, String> {
    parse_generation_set(&query_service(ubus)?, expected, true)
}

fn query_service(ubus: &Path) -> Result<Vec<u8>, String> {
    let output = run_bounded_command_output_with_input(
        &SpawnSpec {
            program: ubus.into(),
            arguments: vec![
                "call".into(),
                "service".into(),
                "list".into(),
                r#"{"name":"cake-autorate","verbose":true}"#.into(),
            ],
            environment: vec![],
        },
        None,
        COMMAND_TIMEOUT,
        MAX_OUTPUT,
        || false,
        |_| {},
    )?;
    if !output.status.success() || !output.stderr.is_empty() {
        return Err("controller-generation-procd-query-failed".into());
    }
    Ok(output.stdout)
}

pub(crate) fn service_absent(ubus: &Path) -> Result<bool, String> {
    parse_service_absent(&query_service(ubus)?)
}

/// Complete generation-backed controller definitions, including dormant ones.
/// Keep this private runtime evidence out of logs: environment fields may be
/// sensitive. Ordinary reload compares unchanged definitions byte-for-value.
pub(crate) fn controller_definitions(
    ubus: &Path,
) -> Result<BTreeMap<String, serde_json::Value>, String> {
    parse_controller_definitions(&query_service(ubus)?)
}

fn parse_controller_definitions(
    bytes: &[u8],
) -> Result<BTreeMap<String, serde_json::Value>, String> {
    if bytes.len() > MAX_OUTPUT {
        return Err("controller-procd-response-too-large".into());
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| "controller-procd-invalid")?;
    let root = value.as_object().ok_or("controller-procd-invalid")?;
    let Some(service) = root.get("cake-autorate") else {
        return Ok(BTreeMap::new());
    };
    let service = service.as_object().ok_or("controller-procd-invalid")?;
    let Some(entries) = service.get("instances") else {
        return Ok(BTreeMap::new());
    };
    let entries = entries
        .as_object()
        .filter(|v| v.len() <= 128)
        .ok_or("controller-procd-invalid")?;
    let mut result = BTreeMap::new();
    for (name, entry) in entries {
        let controller = serde_json::json!(["/usr/sbin/cake-autorated", "--instance", name]);
        if entry.get("command") != Some(&controller) {
            if let Some(owner) = name.strip_prefix("mqtt_") {
                if super::runtime_health::safe_name(owner)
                    && owner.len() <= 64
                    && entry.get("command")
                        == Some(&serde_json::json!([
                            "/usr/sbin/cake-autorated",
                            "--mqtt-publisher",
                            owner
                        ]))
                {
                    continue;
                }
            }
            return Err("controller-procd-command-invalid".into());
        }
        if !super::runtime_health::safe_name(name) {
            return Err("controller-procd-name-invalid".into());
        }
        let id = entry
            .get("env")
            .and_then(|v| v.get(super::controller_input::GENERATION_ENV))
            .and_then(|v| v.as_str())
            .ok_or("controller-procd-generation-invalid")?;
        if id.len() != 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("controller-procd-generation-invalid".into());
        }
        let running = entry
            .get("running")
            .and_then(|v| v.as_bool())
            .ok_or("controller-procd-running-invalid")?;
        if running
            && entry
                .get("pid")
                .and_then(|v| v.as_u64())
                .filter(|pid| *pid > 1 && *pid <= u32::MAX as u64)
                .is_none()
        {
            return Err("controller-procd-pid-invalid".into());
        }
        result.insert(name.clone(), entry.clone());
    }
    if result.len() > 64 {
        return Err("controller-procd-too-many-instances".into());
    }
    Ok(result)
}

/// Exact publisher definitions, including dormant registrations. Keep payloads
/// private: procd may include environment values. Never log the returned map.
#[cfg(feature = "calibration")]
pub(crate) fn mqtt_instances(
    ubus: &Path,
    selected: &str,
) -> Result<BTreeMap<String, serde_json::Value>, String> {
    parse_mqtt_instances(&query_service(ubus)?, selected)
}

#[cfg(feature = "calibration")]
pub(crate) fn mqtt_definitions(ubus: &Path) -> Result<BTreeMap<String, serde_json::Value>, String> {
    parse_mqtt_definitions(&query_service(ubus)?, None)
}

#[cfg(feature = "calibration")]
fn parse_mqtt_instances(
    bytes: &[u8],
    selected: &str,
) -> Result<BTreeMap<String, serde_json::Value>, String> {
    if !super::runtime_health::safe_name(selected) || selected.len() > 64 {
        return Err("mqtt-procd-selected-name-invalid".into());
    }
    parse_mqtt_definitions(bytes, Some(selected))
}

#[cfg(feature = "calibration")]
fn parse_mqtt_definitions(
    bytes: &[u8],
    selected: Option<&str>,
) -> Result<BTreeMap<String, serde_json::Value>, String> {
    if bytes.len() > MAX_OUTPUT {
        return Err("mqtt-procd-response-too-large".into());
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| "mqtt-procd-invalid")?;
    let root = value.as_object().ok_or("mqtt-procd-invalid")?;
    let mut result = BTreeMap::new();
    let Some(service) = root.get("cake-autorate") else {
        return Ok(result);
    };
    let service = service.as_object().ok_or("mqtt-procd-invalid")?;
    let Some(instances) = service.get("instances") else {
        return Ok(result);
    };
    let instances = instances
        .as_object()
        .filter(|value| value.len() <= 128)
        .ok_or("mqtt-procd-invalid")?;
    for (name, entry) in instances {
        let controller = serde_json::json!(["/usr/sbin/cake-autorated", "--instance", name]);
        if entry.get("command") == Some(&controller)
            && selected.is_none_or(|selected| name != &format!("mqtt_{selected}"))
        {
            continue;
        }
        let command = entry.get("command").and_then(|value| value.as_array());
        let publisher = command
            .is_some_and(|args| args.get(1).and_then(|v| v.as_str()) == Some("--mqtt-publisher"));
        if !name.starts_with("mqtt_") && !publisher {
            continue;
        }
        let owner = name
            .strip_prefix("mqtt_")
            .filter(|name| super::runtime_health::safe_name(name) && name.len() <= 64)
            .ok_or("mqtt-procd-owner-invalid")?;
        let expected = serde_json::json!(["/usr/sbin/cake-autorated", "--mqtt-publisher", owner]);
        if entry.get("command") != Some(&expected) {
            return Err("mqtt-procd-command-or-name-collision".into());
        }
        if entry.get("running").and_then(|value| value.as_bool()) == Some(true)
            && entry
                .get("pid")
                .and_then(|value| value.as_u64())
                .filter(|pid| *pid > 1 && *pid <= u32::MAX as u64)
                .is_none()
        {
            return Err("mqtt-procd-running-pid-invalid".into());
        }
        result.insert(owner.to_string(), entry.clone());
    }
    Ok(result)
}

/// GC references include dormant/respawning definitions and any own-service
/// command carrying an input ID. Running/PID checks would lose those roots.
pub(crate) fn generation_references(ubus: &Path) -> Result<BTreeSet<String>, String> {
    parse_generation_references(&query_service(ubus)?)
}

fn parse_generation_references(bytes: &[u8]) -> Result<BTreeSet<String>, String> {
    if bytes.len() > MAX_OUTPUT {
        return Err("controller-generation-procd-invalid".into());
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| "controller-generation-procd-invalid")?;
    let root = value
        .as_object()
        .ok_or("controller-generation-procd-invalid")?;
    let mut result = BTreeSet::new();
    let Some(service) = root.get("cake-autorate") else {
        return Ok(result);
    };
    let service = service
        .as_object()
        .ok_or("controller-generation-procd-invalid")?;
    let Some(instances) = service.get("instances") else {
        return Ok(result);
    };
    let instances = instances
        .as_object()
        .filter(|map| map.len() <= 128)
        .ok_or("controller-generation-procd-invalid")?;
    for instance in instances.values() {
        let instance = instance
            .as_object()
            .ok_or("controller-generation-procd-invalid")?;
        let Some(env) = instance.get("env") else {
            continue;
        };
        let env = env
            .as_object()
            .ok_or("controller-generation-procd-invalid")?;
        let Some(id) = env.get(super::controller_input::GENERATION_ENV) else {
            continue;
        };
        let id = id
            .as_str()
            .filter(|id| {
                id.len() == 64
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            .ok_or("controller-generation-procd-invalid")?;
        result.insert(id.to_string());
    }
    Ok(result)
}

fn parse_service_absent(bytes: &[u8]) -> Result<bool, String> {
    if bytes.len() > MAX_OUTPUT {
        return Err("controller-generation-procd-invalid".into());
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| "controller-generation-procd-invalid")?;
    let root = value
        .as_object()
        .ok_or("controller-generation-procd-invalid")?;
    let Some(service) = root.get("cake-autorate") else {
        return Ok(true);
    };
    let instances = service
        .as_object()
        .and_then(|service| service.get("instances"))
        .and_then(|value| value.as_object())
        .ok_or("controller-generation-procd-invalid")?;
    Ok(instances.is_empty())
}

fn parse_controller_generations(
    bytes: &[u8],
    expected: &BTreeMap<String, String>,
) -> Result<Option<BTreeMap<String, u32>>, String> {
    parse_generation_set(bytes, expected, false)
}

fn parse_generation_set(
    bytes: &[u8],
    expected: &BTreeMap<String, String>,
    exact_controllers: bool,
) -> Result<Option<BTreeMap<String, u32>>, String> {
    if bytes.len() > MAX_OUTPUT
        || expected.len() > 64
        || expected.iter().any(|(name, id)| {
            !super::runtime_health::safe_name(name)
                || id.len() != 64
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    {
        return Err("controller-generation-input-invalid".into());
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| "controller-generation-procd-invalid")?;
    let Some(root) = value.as_object() else {
        return Err("controller-generation-procd-invalid".into());
    };
    let Some(service) = root.get("cake-autorate") else {
        return Ok(if expected.is_empty() {
            Some(BTreeMap::new())
        } else {
            None
        });
    };
    let service = service
        .as_object()
        .ok_or("controller-generation-procd-invalid")?;
    let Some(instances) = service.get("instances") else {
        return Ok(if expected.is_empty() {
            Some(BTreeMap::new())
        } else {
            None
        });
    };
    let instances = instances
        .as_object()
        .filter(|v| v.len() <= 128)
        .ok_or("controller-generation-procd-invalid")?;
    if exact_controllers {
        for (name, instance) in instances {
            if expected.contains_key(name) {
                continue;
            }
            // A service batch owns all controller definitions, including
            // stopped definitions that procd could respawn later. Only the
            // existing exact MQTT sidecar command is outside this membership.
            let Some(owner) = name.strip_prefix("mqtt_") else {
                return Err("controller-generation-procd-extra-instance".into());
            };
            if !super::runtime_health::safe_name(owner)
                || instance.get("command")
                    != Some(&serde_json::json!([
                        "/usr/sbin/cake-autorated",
                        "--mqtt-publisher",
                        owner
                    ]))
            {
                return Err("controller-generation-procd-extra-instance".into());
            }
        }
    }
    let mut result = BTreeMap::new();
    for (name, id) in expected {
        let Some(instance) = instances.get(name) else {
            return Ok(None);
        };
        let instance = instance
            .as_object()
            .ok_or("controller-generation-procd-invalid")?;
        let command = instance
            .get("command")
            .and_then(|v| v.as_array())
            .ok_or("controller-generation-procd-command-invalid")?;
        if command.len() != 3
            || command[0].as_str() != Some("/usr/sbin/cake-autorated")
            || command[1].as_str() != Some("--instance")
            || command[2].as_str() != Some(name)
        {
            return Err("controller-generation-procd-command-mismatch".into());
        }
        if instance
            .get("env")
            .and_then(|v| v.get(super::controller_input::GENERATION_ENV))
            .and_then(|v| v.as_str())
            != Some(id)
        {
            return Ok(None);
        }
        if instance.get("running").and_then(|v| v.as_bool()) != Some(true) {
            return Ok(None);
        }
        let pid = instance
            .get("pid")
            .and_then(|v| v.as_u64())
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n > 1 && *n <= i32::MAX as u32)
            .ok_or("controller-generation-procd-pid-invalid")?;
        if instance
            .get("errors")
            .is_some_and(|v| !v.as_array().is_some_and(|v| v.is_empty()))
        {
            return Err("controller-generation-procd-instance-error".into());
        }
        if result.values().any(|existing| *existing == pid) {
            return Err("controller-generation-procd-pid-duplicate".into());
        }
        result.insert(name.clone(), pid);
    }
    Ok(Some(result))
}

pub(crate) enum Registration<'a> {
    Controller {
        instance: &'a str,
        generation: &'a str,
    },
    #[cfg(feature = "calibration")]
    Mqtt {
        instance: &'a str,
        endpoint: &'a str,
    },
}

fn registration_request(registration: Registration<'_>) -> Result<serde_json::Value, String> {
    let (instance, name, mut definition) = match registration {
        Registration::Controller {
            instance,
            generation,
        } => {
            if generation.len() != 64
                || !generation
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err("controller-registration-generation-invalid".into());
            }
            (
                instance,
                instance.to_string(),
                serde_json::json!({
                    "command":["/usr/sbin/cake-autorated","--instance",instance],
                    "env":{super::controller_input::GENERATION_ENV:generation}
                }),
            )
        }
        #[cfg(feature = "calibration")]
        Registration::Mqtt { instance, endpoint } => {
            let nonce = endpoint
                .strip_prefix("cmq1_")
                .ok_or("mqtt-registration-endpoint-invalid")?;
            if nonce.len() != 32
                || !nonce
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err("mqtt-registration-endpoint-invalid".into());
            }
            (
                instance,
                format!("mqtt_{instance}"),
                serde_json::json!({
                    "command":["/usr/sbin/cake-autorated","--mqtt-publisher",instance],
                    "env":{"CAKE_AUTORATE_MQTT_READY_ENDPOINT":endpoint},"term_timeout":5
                }),
            )
        }
    };
    if !super::runtime_health::safe_name(instance) || instance.len() > 64 {
        return Err("service-registration-instance-invalid".into());
    }
    // Match procd.sh's array-of-strings and boolean serialization. Omit global
    // triggers/data: add merges instances without replacing those peer fields.
    definition["respawn"] = serde_json::json!(["3600", "5", "5"]);
    definition["stdout"] = true.into();
    definition["stderr"] = true.into();
    Ok(
        serde_json::json!({"name":"cake-autorate","script":"/etc/init.d/cake-autorate","instances":{name:definition}}),
    )
}

/// One registration attempt under the caller's lifecycle/source authority.
/// Readiness (including lost-ACK resolution) belongs to the exact runtime
/// observer; this function never retries or claims that a process is ready.
pub(crate) fn add_instance(ubus: &Path, registration: Registration<'_>) -> Result<(), String> {
    let request = registration_request(registration)?.to_string();
    let result = run_bounded_command_output_with_input(
        &SpawnSpec {
            program: ubus.into(),
            arguments: vec![
                "call".into(),
                "service".into(),
                "add".into(),
                request.into(),
            ],
            environment: vec![],
        },
        None,
        COMMAND_TIMEOUT,
        MAX_OUTPUT,
        || false,
        |_| {},
    )?;
    if !result.status.success() || !result.stderr.is_empty() {
        return Err("service-instance-registration-unacknowledged".into());
    }
    Ok(())
}

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

    #[cfg(feature = "calibration")]
    #[test]
    fn r4_selected_mqtt_procd_rejects_aliases_arguments_and_controller_name_collision() {
        let entry = serde_json::json!({"command":["/usr/sbin/cake-autorated", "--mqtt-publisher", "lab"],"running":true,"pid":42});
        let mut value = serde_json::json!({"cake-autorate":{"instances":{
            "mqtt_lab":entry,
            "mqtt_other_controller":{"command":["/usr/sbin/cake-autorated", "--instance", "mqtt_other_controller"]}
        }}});
        let parse = |value: &serde_json::Value| {
            parse_mqtt_instances(&serde_json::to_vec(value).unwrap(), "lab")
        };
        assert_eq!(parse(&value).unwrap().len(), 1);
        value["cake-autorate"]["instances"]["mqtt_lab"]["pid"] = 1.into();
        assert!(parse(&value).is_err());
        value["cake-autorate"]["instances"]["mqtt_lab"]["running"] = false.into();
        assert_eq!(parse(&value).unwrap().len(), 1);
        value["cake-autorate"]["instances"]["mqtt_lab"]["command"] =
            serde_json::json!(["/usr/sbin/cake-autorated", "--instance", "mqtt_lab"]);
        assert!(parse(&value).is_err());
        value["cake-autorate"]["instances"]["mqtt_lab"]["command"] = serde_json::json!([
            "/usr/sbin/cake-autorated",
            "--mqtt-publisher",
            "lab",
            "extra"
        ]);
        assert!(parse(&value).is_err());
        value["cake-autorate"]["instances"]["mqtt_lab"]["command"] =
            serde_json::json!(["/usr/sbin/cake-autorated", "--mqtt-publisher", "peer"]);
        assert!(parse(&value).is_err());
        let alias = serde_json::json!({"cake-autorate":{"instances":{"alias":entry}}});
        assert!(parse(&alias).is_err());
        assert!(parse_mqtt_instances(b"{}", "lab").unwrap().is_empty());
    }

    #[test]
    fn r4_gc_generation_references_include_dormant_and_alternate_definitions() {
        let id = "a".repeat(64);
        let value = serde_json::json!({"cake-autorate":{"instances":{
            "lab":{"running":false,"env":{super::super::controller_input::GENERATION_ENV:id}},
            "custom":{"command":["/usr/sbin/cake-autorated","--instance","lab","--once"],
                "env":{super::super::controller_input::GENERATION_ENV:"b".repeat(64)}},
            "legacy":{"running":true,"pid":123}
        }}});
        assert_eq!(
            parse_generation_references(&serde_json::to_vec(&value).unwrap()).unwrap(),
            BTreeSet::from([id, "b".repeat(64)])
        );
        assert!(parse_generation_references(b"{}").unwrap().is_empty());
        assert!(parse_generation_references(
            br#"{"cake-autorate":{"instances":{"x":{"env":null}}}}"#
        )
        .is_err());
    }

    #[test]
    fn r4_procd_batch_rejects_unexpected_dormant_controllers_but_allows_exact_mqtt_sidecars() {
        let mut value = serde_json::json!({"cake-autorate":{"instances":{}}});
        let empty = BTreeMap::new();
        assert_eq!(
            parse_generation_set(&serde_json::to_vec(&value).unwrap(), &empty, true).unwrap(),
            Some(BTreeMap::new())
        );
        value["cake-autorate"]["instances"]["extra"] = serde_json::json!({
            "command":["/usr/sbin/cake-autorated","--instance","extra"], "running":false
        });
        assert!(parse_generation_set(&serde_json::to_vec(&value).unwrap(), &empty, true).is_err());
        value["cake-autorate"]["instances"] = serde_json::json!({"mqtt_lab":{
            "command":["/usr/sbin/cake-autorated","--mqtt-publisher","lab"],"running":false
        }});
        assert!(
            parse_generation_set(&serde_json::to_vec(&value).unwrap(), &empty, true)
                .unwrap()
                .is_some()
        );
        value["cake-autorate"]["instances"]["mqtt_lab"]["command"] =
            serde_json::json!(["/usr/sbin/cake-autorated", "--instance", "mqtt_lab"]);
        assert!(parse_generation_set(&serde_json::to_vec(&value).unwrap(), &empty, true).is_err());
    }

    #[test]
    fn r4_procd_absence_requires_no_registered_instance_even_when_not_running() {
        assert!(parse_service_absent(b"{}").unwrap());
        assert!(parse_service_absent(br#"{"cake-autorate":{"instances":{}}}"#).unwrap());
        assert!(!parse_service_absent(
            br#"{"cake-autorate":{"instances":{"lab":{"running":false}}}}"#
        )
        .unwrap());
        assert!(!parse_service_absent(
            br#"{"cake-autorate":{"instances":{"mqtt_lab":{"running":false}}}}"#
        )
        .unwrap());
        for bad in [
            br#"[]"#.as_slice(),
            br#"{"cake-autorate":{}}"#,
            br#"{"cake-autorate":{"instances":null}}"#,
        ] {
            assert!(parse_service_absent(bad).is_err());
        }
    }

    #[test]
    fn r4_procd_generation_proof_requires_exact_command_generation_and_live_unique_pid() {
        let id = "a".repeat(64);
        let expected = BTreeMap::from([("lab".into(), id.clone())]);
        let valid = serde_json::json!({"cake-autorate":{"instances":{"lab":{
            "command":["/usr/sbin/cake-autorated","--instance","lab"],
            "env":{super::super::controller_input::GENERATION_ENV:id},"running":true,"pid":123
        }}}});
        assert_eq!(
            parse_controller_generations(&serde_json::to_vec(&valid).unwrap(), &expected).unwrap(),
            Some(BTreeMap::from([("lab".into(), 123)]))
        );
        for key in ["pid", "env", "running", "command"] {
            let mut value = valid.clone();
            value["cake-autorate"]["instances"]["lab"]
                .as_object_mut()
                .unwrap()
                .remove(key);
            assert!(!matches!(
                parse_controller_generations(&serde_json::to_vec(&value).unwrap(), &expected),
                Ok(Some(_))
            ));
        }
        let mut value = valid.clone();
        value["cake-autorate"]["instances"]["lab"]["command"] =
            serde_json::json!(["/usr/sbin/cake-autorated", "--instance", "lab", "--once"]);
        assert!(
            parse_controller_generations(&serde_json::to_vec(&value).unwrap(), &expected).is_err()
        );
        let mut value = valid.clone();
        value["cake-autorate"]["instances"]["lab"]["errors"] =
            serde_json::json!(["private fixture error"]);
        let error = parse_controller_generations(&serde_json::to_vec(&value).unwrap(), &expected)
            .err()
            .unwrap();
        assert!(!error.contains("private fixture"));
        assert_eq!(
            parse_controller_generations(b"{}", &expected).unwrap(),
            None
        );
        assert!(parse_controller_generations(b"[]", &expected).is_err());
    }

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
#[cfg(test)]
mod reload_definition_tests {
    use super::*;
    #[test]
    fn r4_empty_service_dump_omits_instances_but_is_not_a_malformed_service() {
        let empty = br#"{"cake-autorate":{"autostart":false}}"#;
        assert!(parse_controller_definitions(empty).unwrap().is_empty());
        assert!(parse_generation_references(empty).unwrap().is_empty());
        #[cfg(feature = "calibration")]
        assert!(parse_mqtt_instances(empty, "lab").unwrap().is_empty());
        for malformed in [
            br#"{"cake-autorate":null}"#.as_slice(),
            br#"{"cake-autorate":{"instances":42}}"#.as_slice(),
        ] {
            assert!(parse_controller_definitions(malformed).is_err());
            assert!(parse_generation_references(malformed).is_err());
            #[cfg(feature = "calibration")]
            assert!(parse_mqtt_instances(malformed, "lab").is_err());
        }
    }

    #[test]
    #[cfg(feature = "calibration")]
    fn r4_mqtt_whole_set_query_does_not_reserve_an_arbitrary_controller_name() {
        let response = br#"{"cake-autorate":{"instances":{"mqtt_reload":{"command":["/usr/sbin/cake-autorated","--instance","mqtt_reload"],"running":false}}}}"#;
        assert!(parse_mqtt_definitions(response, None).unwrap().is_empty());
        assert!(parse_mqtt_instances(response, "reload").is_err());
    }
    #[test]
    fn r4_single_instance_registration_omits_global_settings_and_matches_procd_types() {
        let id = "a".repeat(64);
        let request = registration_request(Registration::Controller {
            instance: "lab",
            generation: &id,
        })
        .unwrap();
        assert_eq!(request["name"], "cake-autorate");
        assert_eq!(request["instances"].as_object().unwrap().len(), 1);
        assert!(request.get("triggers").is_none());
        assert!(request.get("data").is_none());
        let instance = &request["instances"]["lab"];
        assert_eq!(
            instance["command"],
            serde_json::json!(["/usr/sbin/cake-autorated", "--instance", "lab"])
        );
        assert_eq!(instance["respawn"], serde_json::json!(["3600", "5", "5"]));
        assert_eq!(instance["stdout"], true);
        assert_eq!(instance["stderr"], true);
        assert_eq!(
            instance["env"][super::super::controller_input::GENERATION_ENV],
            id
        );
        for (name, generation) in [
            ("bad-name", "a".repeat(64)),
            ("lab", "A".repeat(64)),
            ("lab", "a".repeat(63)),
        ] {
            assert!(registration_request(Registration::Controller {
                instance: name,
                generation: &generation
            })
            .is_err());
        }
        #[cfg(feature = "calibration")]
        {
            let name = "x".repeat(64);
            let endpoint = format!("cmq1_{}", "b".repeat(32));
            let mut request = registration_request(Registration::Mqtt {
                instance: &name,
                endpoint: &endpoint,
            })
            .unwrap();
            let key = format!("mqtt_{name}");
            assert_eq!(request["instances"][&key]["term_timeout"], 5);
            request["instances"][&key]["running"] = false.into();
            let response = serde_json::json!({"cake-autorate":{"instances":request["instances"]}});
            assert!(
                parse_controller_definitions(&serde_json::to_vec(&response).unwrap())
                    .unwrap()
                    .is_empty()
            );
            assert!(registration_request(Registration::Mqtt {
                instance: "lab",
                endpoint: "cmq1_bad"
            })
            .is_err());
        }
    }

    #[test]
    fn r4_single_instance_add_is_submitted_once_and_unacknowledged_is_not_readiness() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "cake-procd-add-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let id = "a".repeat(64);
        for status in [0, 1] {
            let request = root.join(format!("request-{status}"));
            let ubus = root.join(format!("ubus-{status}"));
            std::fs::write(&ubus, format!("#!/bin/sh\n[ \"$1 $2 $3\" = 'call service add' ] || exit 99\n[ ! -e '{}' ] || exit 98\nprintf '%s' \"$4\" > '{}'\nexit {status}\n", request.display(), request.display())).unwrap();
            std::fs::set_permissions(&ubus, std::fs::Permissions::from_mode(0o700)).unwrap();
            let result = add_instance(
                &ubus,
                Registration::Controller {
                    instance: "lab",
                    generation: &id,
                },
            );
            assert_eq!(result.is_ok(), status == 0);
            let body: serde_json::Value =
                serde_json::from_slice(&std::fs::read(request).unwrap()).unwrap();
            assert_eq!(
                body,
                registration_request(Registration::Controller {
                    instance: "lab",
                    generation: &id
                })
                .unwrap()
            );
        }
    }

    #[test]
    fn r4_reload_procd_definitions_preserve_dormant_entries_and_reject_ambiguous_commands() {
        let id = "a".repeat(64);
        let controller = serde_json::json!({"command":["/usr/sbin/cake-autorated","--instance","lab"],
            "running":false,"env":{super::super::controller_input::GENERATION_ENV:id},"respawn":[3600,5,5]});
        let value = serde_json::json!({"cake-autorate":{"instances":{"lab":controller,
            "mqtt_peer":{"command":["/usr/sbin/cake-autorated","--mqtt-publisher","peer"],"running":false}}}});
        let parsed = parse_controller_definitions(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed["lab"], value["cake-autorate"]["instances"]["lab"]);
        for (field, replacement) in [
            (
                "command",
                serde_json::json!(["/usr/sbin/cake-autorated", "--instance", "other"]),
            ),
            (
                "command",
                serde_json::json!(["/usr/sbin/cake-autorated", "--instance", "lab", "--extra"]),
            ),
            ("env", serde_json::json!({})),
            (
                "env",
                serde_json::json!({super::super::controller_input::GENERATION_ENV:"A".repeat(64)}),
            ),
            ("running", serde_json::json!(true)), // Running without a valid PID.
            ("running", serde_json::json!("false")),
        ] {
            let mut malformed = value.clone();
            malformed["cake-autorate"]["instances"]["lab"][field] = replacement;
            assert!(
                parse_controller_definitions(&serde_json::to_vec(&malformed).unwrap()).is_err()
            );
        }
        let mut collision = value.clone();
        collision["cake-autorate"]["instances"]["mqtt_peer"]["command"] =
            serde_json::json!(["/bin/foreign"]);
        assert!(parse_controller_definitions(&serde_json::to_vec(&collision).unwrap()).is_err());
        let mut named_like_sidecar = value;
        named_like_sidecar["cake-autorate"]["instances"]["mqtt_peer"] = serde_json::json!({
            "command":["/usr/sbin/cake-autorated","--instance","mqtt_peer"], "running":false,
            "env":{super::super::controller_input::GENERATION_ENV:"b".repeat(64)}});
        assert_eq!(
            parse_controller_definitions(&serde_json::to_vec(&named_like_sidecar).unwrap())
                .unwrap()
                .len(),
            2
        );
    }
}
