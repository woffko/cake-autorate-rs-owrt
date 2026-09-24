//! Bounded, side-effect-free validation of actual candidate UCI values.
//! Input is stdin only: no candidate/credential argv, savedir reads, discovery,
//! process spawning, temporary files, commit or service restart. MQ admission
//! reads local script/proof/tool identity only; it never launches a probe.
use crate::Config;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::Read;

pub(crate) const SCHEMA_VERSION: u64 = 1;
pub(crate) const MAX_INPUT_BYTES: usize = 256 * 1024;
const MAX_SECTIONS: usize = 64;
const MAX_OPTIONS: usize = 256;
const MAX_VALUE_BYTES: usize = 4096;
const MAX_LIST_ITEMS: usize = 256;

fn failure(code: &'static str) -> Value {
    json!({"schema_version": SCHEMA_VERSION, "validation_scope": "controller-sqm", "valid": false, "code": code})
}

pub(crate) fn validate_reader(input: impl Read) -> Value {
    let mut bytes = Vec::new();
    if input
        .take((MAX_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return failure("candidate-read-failed");
    }
    validate_bytes(&bytes)
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn validate_bytes(bytes: &[u8]) -> Value {
    if bytes.len() > MAX_INPUT_BYTES {
        return failure("candidate-too-large");
    }
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        // Parsing errors never echo submitted data (which may contain secrets).
        return failure("candidate-json-invalid");
    };
    let mut result = validate_value(&value);
    result["candidate_sha256"] = json!(digest(bytes));
    result
}

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn strings(value: &Value) -> Result<Vec<String>, &'static str> {
    let values = match value {
        Value::String(value) => vec![value.clone()],
        Value::Array(values) if values.len() <= MAX_LIST_ITEMS => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .ok_or("candidate-option-type-invalid")
            })
            .collect::<Result<_, _>>()?,
        _ => return Err("candidate-option-type-invalid"),
    };
    if values
        .iter()
        .any(|value| value.len() > MAX_VALUE_BYTES || value.contains('\0'))
    {
        return Err("candidate-option-value-invalid");
    }
    Ok(values)
}

fn validate_value(input: &Value) -> Value {
    validate_value_with(input, crate::qdisc_capabilities::supported)
}

fn validate_value_with(
    input: &Value,
    mut mq_supported: impl FnMut(&str) -> Result<bool, String>,
) -> Value {
    if input.get("schema_version").and_then(Value::as_u64) != Some(SCHEMA_VERSION) {
        return failure("candidate-schema-mismatch");
    }
    let Some(request_id) = input
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
    else {
        return failure("candidate-request-id-invalid");
    };
    let Some(sections) = input
        .get("sections")
        .and_then(Value::as_array)
        .filter(|sections| sections.len() <= MAX_SECTIONS)
    else {
        return failure("candidate-sections-invalid");
    };
    let mut seen = HashSet::new();
    let mut errors = Vec::new();
    let mut sqm = HashMap::new();
    if let Some(queues) = input.get("sqm") {
        let Some(queues) = queues
            .as_array()
            .filter(|queues| queues.len() <= MAX_SECTIONS)
        else {
            return failure("candidate-sqm-sections-invalid");
        };
        for queue in queues {
            let Some(name) = queue
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| safe_name(name))
            else {
                return failure("candidate-sqm-name-invalid");
            };
            let Some(options) = queue
                .get("options")
                .and_then(Value::as_object)
                .filter(|options| options.len() <= MAX_OPTIONS)
            else {
                return failure("candidate-sqm-options-invalid");
            };
            let mut values = HashMap::new();
            for (key, value) in options {
                if key.len() > 64
                    || key.is_empty()
                    || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    return failure("candidate-sqm-option-name-invalid");
                }
                let value = match strings(value) {
                    Ok(value) => value,
                    Err(code) => return failure(code),
                };
                if let Some(value) = value.first() {
                    values.insert(key.as_str(), value.clone());
                }
            }
            if sqm.insert(name, values).is_some() {
                return failure("candidate-sqm-section-duplicate");
            }
        }
    }
    if let Some(globals) = input.get("globals") {
        let Some(globals) = globals.as_object() else {
            return failure("candidate-globals-invalid");
        };
        if let Some(budget) = globals.get("graph_history_ram_budget_kib") {
            let valid = budget.as_str().is_some_and(|value| {
                value.is_empty()
                    || value == "auto"
                    || value.parse::<u64>().ok().is_some_and(|value| {
                        (crate::GRAPH_HISTORY_MIN_BUDGET_KIB..=crate::GRAPH_HISTORY_HARD_MAX_KIB)
                            .contains(&value)
                    })
            });
            if !valid {
                errors.push(json!({"instance": "globals", "code": "invalid-controller-config",
                    "message": format!("graph_history_ram_budget_kib must be auto or between {} and {}", crate::GRAPH_HISTORY_MIN_BUDGET_KIB, crate::GRAPH_HISTORY_HARD_MAX_KIB)}));
            }
        }
    }
    for section in sections {
        let Some(name) = section
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| safe_name(name))
        else {
            return failure("candidate-section-name-invalid");
        };
        if !seen.insert(name) {
            return failure("candidate-section-duplicate");
        }
        let Some(options) = section
            .get("options")
            .and_then(Value::as_object)
            .filter(|options| options.len() <= MAX_OPTIONS)
        else {
            return failure("candidate-options-invalid");
        };
        let mut single = HashMap::new();
        let mut lists = HashMap::new();
        for (key, value) in options {
            if key.is_empty()
                || key.len() > 64
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            {
                return failure("candidate-option-name-invalid");
            }
            let values = match strings(value) {
                Ok(values) => values,
                Err(code) => return failure(code),
            };
            if let Some(value) = values.first() {
                single.insert(key.clone(), value.clone());
            }
            lists.insert(key.clone(), values);
        }
        // Config's legacy unsupported-pinger diagnostic interpolates its value.
        // Reject it here without echoing user input into a public response.
        let pinger_supported = single.get("pinger_method").is_none_or(|method| {
            matches!(method.as_str(), "fping" | "fping-ts" | "tsping" | "ping")
                || (cfg!(feature = "calibration") && method == "irtt")
        });
        let result = if pinger_supported {
            Config::from_uci_values(name, &single, &lists).and_then(|cfg| cfg.validate())
        } else {
            Err("pinger_method is unsupported for this package variant".to_string())
        };
        let result = result.and_then(|()| {
            let managed = single
                .get("manage_sqm")
                .map(|value| crate::parse_bool(value))
                .transpose()?
                .unwrap_or(true);
            if !managed {
                return Ok(());
            }
            let target = single
                .get("sqm_section")
                .filter(|name| !name.is_empty())
                .cloned()
                .unwrap_or_else(|| format!("cake_{name}"));
            if !safe_name(&target) {
                return Err("sqm_section must be a safe UCI section name".into());
            }
            if sqm
                .get(target.as_str())
                .and_then(|queue| queue.get("_cake_autorate_managed"))
                .is_some_and(|owner| !owner.is_empty() && owner != name)
            {
                return Err(
                    "requested SQM section belongs to another CAKE Autorate instance".into(),
                );
            }
            let retained_mq = sqm
                .get(target.as_str())
                .and_then(|queue| queue.get("use_mq"))
                .map(String::as_str);
            crate::sqm_config::validate(&single, retained_mq, &mut mq_supported).map(|_| ())
        });
        if let Err(message) = result {
            errors.push(
                json!({"instance": name, "code": "invalid-controller-config", "message": message}),
            );
        }
    }
    json!({"schema_version": SCHEMA_VERSION, "validation_scope": "controller-sqm", "request_id": request_id,
        "valid": errors.is_empty(), "code": if errors.is_empty() { "candidate-valid" } else { "candidate-invalid" },
        "validated_instances": seen.len(), "errors": errors})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(options: Value) -> Value {
        json!({"schema_version": 1, "request_id": "0123456789abcdef0123456789abcdef",
            "sections": [{"name": "candidate", "options": options}]})
    }

    #[test]
    fn r3_candidate_sqm_whitelist_and_capability_apply_to_the_submitted_intent() {
        let mut input = request(json!({"enabled": "1", "sqm_qdisc": "fq_codel"}));
        assert_eq!(
            validate_value_with(&input, |_| panic!(
                "invalid qdisc must not query capabilities"
            ))["valid"],
            false
        );
        input["sections"][0]["options"]["sqm_qdisc"] = json!("cake-mq");
        assert_eq!(validate_value_with(&input, |_| Ok(false))["valid"], false);
        assert_eq!(validate_value_with(&input, |_| Ok(true))["valid"], true);
        input["sections"][0]["options"]["sqm_use_mq"] = json!("0");
        assert_eq!(
            validate_value_with(&input, |_| panic!("explicit off must not probe"))["valid"],
            true
        );
    }

    #[test]
    fn r3_candidate_retains_sqm_mq_and_ignores_unrelated_queue_qdisc() {
        let mut input = request(json!({"enabled": "1"}));
        input["sqm"] = json!([
            {"name": "foreign", "options": {"qdisc": "fq_codel", "use_mq": "0"}},
            {"name": "cake_candidate", "options": {"qdisc": "cake", "use_mq": "1"}}
        ]);
        assert_eq!(validate_value_with(&input, |_| Ok(false))["valid"], false);
        assert_eq!(validate_value_with(&input, |_| Ok(true))["valid"], true);
        input["sections"][0]["options"]["sqm_use_mq"] = json!("0");
        assert_eq!(
            validate_value_with(&input, |_| panic!("explicit off"))["valid"],
            true
        );
        input["sqm"][1]["name"] = json!("foreign");
        assert_eq!(
            validate_value(&input)["code"],
            "candidate-sqm-section-duplicate"
        );
    }

    #[test]
    fn r3_candidate_sqm_can_be_disabled_without_requiring_a_live_mq_proof() {
        let mut input = request(json!({"enabled": "0", "sqm_qdisc": "cake_mq"}));
        assert_eq!(
            validate_value_with(&input, |_| panic!("disabled SQM needs no proof"))["valid"],
            true
        );
        input["sections"][0]["options"]["sqm_tcTSIZE"] = json!("0");
        assert_eq!(
            validate_value_with(&input, |_| panic!("invalid bounds precede capabilities"))["valid"],
            false
        );
        input["sections"][0]["options"]["manage_sqm"] = json!("0");
        assert_eq!(
            validate_value_with(&input, |_| panic!("unmanaged options unused"))["valid"],
            true
        );
    }

    #[test]
    fn r3_candidate_foreign_sqm_owner_is_rejected_before_capability_lookup() {
        let mut input = request(json!({"enabled": "1", "sqm_use_mq": "1"}));
        input["sqm"] = json!([{"name": "cake_candidate", "options": {"_cake_autorate_managed": "private-foreign-owner", "use_mq": "1"}}]);
        let result = validate_value_with(&input, |_| panic!("foreign owner must fail first"));
        assert_eq!(result["valid"], false);
        assert!(!result.to_string().contains("private-foreign-owner"));
        input["sqm"][0]["options"]["_cake_autorate_managed"] = json!("candidate");
        assert_eq!(validate_value_with(&input, |_| Ok(true))["valid"], true);
    }

    #[test]
    fn r3_candidate_uses_submitted_values_not_old_committed_defaults() {
        let mut input = request(
            json!({"base_dl_shaper_rate_kbps": "30000", "min_dl_shaper_rate_kbps": "25000"}),
        );
        let result = validate_reader(input.to_string().as_bytes());
        assert_eq!(result["valid"], true);
        assert_eq!(result["request_id"], input["request_id"]);
        input["sections"][0]["options"]["min_dl_shaper_rate_kbps"] = json!("50000");
        let result = validate_reader(input.to_string().as_bytes());
        assert_eq!(result["valid"], false);
        assert!(result["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("min <= base <= max"));
    }

    #[test]
    fn r3_candidate_rejects_wire_ambiguity_and_nonfinite_numbers() {
        for options in [
            json!({"alpha_delta_ewma": "NaN"}),
            json!({"enabled": true}),
            json!({"no_pingers": 6}),
            json!({"reflector": ["example.invalid", 3]}),
        ] {
            assert_eq!(
                validate_reader(request(options).to_string().as_bytes())["valid"],
                false
            );
        }
        let mut input = request(json!({}));
        let duplicate = input["sections"][0].clone();
        input["sections"].as_array_mut().unwrap().push(duplicate);
        assert_eq!(
            validate_reader(input.to_string().as_bytes())["code"],
            "candidate-section-duplicate"
        );
    }

    #[test]
    fn r3_candidate_errors_never_echo_private_options_or_bad_backend_values() {
        let input = request(json!({"mqtt_password": "fixture-private-value",
            "pinger_method": "fixture-private-backend", "reflectors_url": "https://never-contact.invalid/list"}));
        let result = validate_reader(input.to_string().as_bytes());
        assert_eq!(result["valid"], false);
        assert!(!result.to_string().contains("fixture-private"));
        assert!(!result.to_string().contains("never-contact"));
    }

    #[test]
    fn r3_candidate_payload_and_schema_are_bounded() {
        assert_eq!(
            validate_reader(vec![b' '; MAX_INPUT_BYTES + 1].as_slice())["code"],
            "candidate-too-large"
        );
        assert_eq!(
            validate_reader(b"{private malformed".as_slice())["code"],
            "candidate-json-invalid"
        );
        let mut input = request(json!({}));
        input["schema_version"] = json!(2);
        assert_eq!(
            validate_reader(input.to_string().as_bytes())["code"],
            "candidate-schema-mismatch"
        );
        input["schema_version"] = json!(1);
        input["request_id"] = json!("bad");
        assert_eq!(
            validate_reader(input.to_string().as_bytes())["code"],
            "candidate-request-id-invalid"
        );
    }

    #[test]
    fn r3_candidate_checks_global_budget_and_does_not_echo_bad_booleans() {
        let mut input = request(json!({}));
        for budget in ["auto", "256", "102400"] {
            input["globals"] = json!({"graph_history_ram_budget_kib": budget});
            assert_eq!(validate_reader(input.to_string().as_bytes())["valid"], true);
        }
        input["globals"] = json!({"graph_history_ram_budget_kib": "0"});
        assert_eq!(
            validate_reader(input.to_string().as_bytes())["valid"],
            false
        );
        input["globals"] = json!({});
        input["sections"][0]["options"]["enabled"] = json!("fixture-private-bool");
        let result = validate_reader(input.to_string().as_bytes());
        assert_eq!(result["valid"], false);
        assert!(!result.to_string().contains("fixture-private-bool"));
    }
}
