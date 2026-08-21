//! Bounded readiness status for the native MQTT publisher.

use super::json_wire::{bool_json, json_escape};
use super::mqtt_publisher::MqttPublisherConfig;
use super::process::{run_bounded_command_output, SpawnSpec};
use super::runtime_health::UciSection;
use std::collections::BTreeMap;
use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const COMMAND_OUTPUT_LIMIT: usize = 128 * 1024;

struct Environment {
    uci: PathBuf,
}

impl Environment {
    fn live() -> Self {
        Self {
            uci: env_path("CAKE_AUTORATE_UCI_BIN", "/sbin/uci"),
        }
    }
}

fn env_path(name: &str, fallback: &str) -> PathBuf {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(fallback))
}

fn command_output(
    program: &Path,
    arguments: &[String],
) -> Result<super::process::BoundedCommandOutput, String> {
    run_bounded_command_output(
        &SpawnSpec {
            program: program.to_path_buf(),
            arguments: arguments.iter().map(OsString::from).collect(),
            environment: Vec::new(),
        },
        COMMAND_TIMEOUT,
        COMMAND_OUTPUT_LIMIT,
        || false,
    )
}

fn validate_section(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || matches!(value, "." | "..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'@' | b'.' | b'-'))
    {
        return Err("invalid MQTT instance".to_string());
    }
    Ok(())
}

fn uci_value(
    environment: &Environment,
    section: &str,
    option: &str,
) -> Result<Option<String>, String> {
    let output = command_output(
        &environment.uci,
        &[
            "-q".to_string(),
            "get".to_string(),
            format!("cake-autorate.{section}.{option}"),
        ],
    )?;
    if !output.status.success() {
        return Ok(None);
    }
    let value = std::str::from_utf8(&output.stdout)
        .map_err(|_| "MQTT UCI value is not UTF-8".to_string())?
        .trim_end_matches(['\r', '\n']);
    if value.len() > 4096 || value.chars().any(char::is_control) {
        return Err("MQTT UCI value is unsafe or oversized".to_string());
    }
    Ok(Some(value.to_string()))
}

fn bool_value(value: Option<String>, fallback: bool) -> bool {
    value.map_or(fallback, |value| value == "1")
}

fn mqtt_status(section: &str, mode: &str, environment: &Environment) -> Result<String, String> {
    let names = [
        "enabled",
        "mqtt_enabled",
        "mqtt_host",
        "mqtt_port",
        "mqtt_username",
        "mqtt_password",
        "mqtt_discovery_prefix",
        "mqtt_base_topic",
        "mqtt_device_id",
        "mqtt_device_name",
        "mqtt_min_interval_s",
        "mqtt_publish_cpu_stats",
        "log_to_file",
        "log_file_path_override",
        "output_summary_stats",
        "output_cpu_stats",
    ];
    let mut options = BTreeMap::new();
    for name in names {
        if let Some(value) = uci_value(environment, section, name)? {
            options.insert(name.to_string(), value);
        }
    }
    let uci_section = UciSection {
        section_type: "cake_autorate".to_string(),
        options,
    };
    let enabled = bool_value(uci_section.options.get("mqtt_enabled").cloned(), false);
    let configured_host = uci_section
        .options
        .get("mqtt_host")
        .is_some_and(|value| !value.is_empty());
    let log_to_file = bool_value(uci_section.options.get("log_to_file").cloned(), true);
    let summary = bool_value(
        uci_section.options.get("output_summary_stats").cloned(),
        false,
    );
    let cpu = bool_value(uci_section.options.get("output_cpu_stats").cloned(), false);
    let publish_cpu = bool_value(
        uci_section.options.get("mqtt_publish_cpu_stats").cloned(),
        false,
    );
    let (available, reason) = match MqttPublisherConfig::from_section(section, &uci_section) {
        Ok(Some(_)) => (true, "available".to_string()),
        Ok(None) => (false, "MQTT publisher is disabled".to_string()),
        Err(error) => (false, error),
    };
    Ok(format!(
        concat!(
            "{{\"section\":\"{}\",\"mode\":\"{}\",\"package\":\"{}\",",
            "\"installed\":{},\"enabled\":{},\"configured_host\":{},",
            "\"available\":{},\"summary_enabled\":{},\"cpu_enabled\":{},",
            "\"publish_cpu\":{},\"log_to_file\":{},\"reason\":\"{}\",",
            "\"install_hint\":\"\"}}\n"
        ),
        json_escape(section),
        json_escape(mode),
        "built-in",
        bool_json(true),
        bool_json(enabled),
        bool_json(configured_host),
        bool_json(available),
        bool_json(summary),
        bool_json(cpu),
        bool_json(publish_cpu),
        bool_json(log_to_file),
        json_escape(&reason),
    ))
}

pub(crate) fn run_mqtt_status<I>(mut arguments: I) -> Result<String, String>
where
    I: Iterator<Item = String>,
{
    mqtt_control(&mut arguments, &Environment::live())
}

fn mqtt_control<I>(mut arguments: I, environment: &Environment) -> Result<String, String>
where
    I: Iterator<Item = String>,
{
    let section = arguments
        .next()
        .ok_or_else(|| "mqtt-status requires an instance".to_string())?;
    validate_section(&section)?;
    let mode = arguments.next().unwrap_or_else(|| "status".to_string());
    if arguments.next().is_some() {
        return Err("MQTT package selection is not permitted".to_string());
    }
    if mode != "status" {
        return Err("mqtt-status mode must be status".to_string());
    }
    mqtt_status(&section, &mode, environment)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

    struct Fixture {
        root: PathBuf,
        environment: Environment,
    }

    impl Fixture {
        fn new() -> Self {
            let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root =
                env::temp_dir().join(format!("cake-mqtt-control-{}-{id}", std::process::id()));
            fs::create_dir(&root).unwrap();
            Self {
                environment: Environment {
                    uci: root.join("uci"),
                },
                root,
            }
        }

        fn executable(&self, path: &Path, body: &str) {
            fs::write(path, body).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }

        fn healthy_uci(&self) {
            self.executable(
                &self.environment.uci,
                "#!/bin/sh\ncase \"$*\" in\n*mqtt_enabled) printf '1\\n' ;;\n*enabled) printf '1\\n' ;;\n*mqtt_host) printf 'broker.invalid\\n' ;;\n*log_to_file) printf '1\\n' ;;\n*output_summary_stats) printf '1\\n' ;;\n*output_cpu_stats) printf '1\\n' ;;\n*mqtt_publish_cpu_stats) printf '1\\n' ;;\n*) exit 1 ;;\nesac\n",
            );
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn status_preserves_the_existing_json_contract() {
        let fixture = Fixture::new();
        fixture.healthy_uci();
        assert_eq!(
            mqtt_status("wan", "status", &fixture.environment).unwrap(),
            concat!(
                "{\"section\":\"wan\",\"mode\":\"status\",",
                "\"package\":\"built-in\",\"installed\":true,",
                "\"enabled\":true,\"configured_host\":true,\"available\":true,",
                "\"summary_enabled\":true,\"cpu_enabled\":true,\"publish_cpu\":true,",
                "\"log_to_file\":true,\"reason\":\"available\",",
                "\"install_hint\":\"\"}\n"
            )
        );
    }

    #[test]
    fn missing_client_and_configuration_report_reasons_without_mutation() {
        let fixture = Fixture::new();
        fixture.executable(&fixture.environment.uci, "#!/bin/sh\nexit 1\n");
        let output = mqtt_status("wan", "status", &fixture.environment).unwrap();
        assert!(output.contains("\"installed\":true"));
        assert!(output.contains("MQTT publisher is disabled"));
    }

    #[test]
    fn package_install_mode_is_retired_and_cannot_mutate_the_system() {
        let fixture = Fixture::new();
        fixture.healthy_uci();
        assert!(mqtt_control(
            ["wan", "install"].into_iter().map(str::to_string),
            &fixture.environment,
        )
        .is_err());
        assert!(mqtt_control(
            ["wan", "install", "arbitrary-package"]
                .into_iter()
                .map(str::to_string),
            &fixture.environment,
        )
        .is_err());
        assert!(validate_section("../wan").is_err());
    }
}
