//! Pure SQM intent validation shared by candidate admission and projection.
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CakeIntent {
    pub use_mq: bool,
    pub legacy_alias: bool,
}

pub(crate) fn intent(
    qdisc: &str,
    explicit_mq: Option<&str>,
    retained_mq: Option<&str>,
) -> Result<CakeIntent, String> {
    let alias_mq = match qdisc {
        "cake" => false,
        "cake_mq" | "cake-mq" => true,
        _ => return Err("sqm_qdisc must be cake or a supported multi-queue CAKE mode".to_string()),
    };
    let use_mq = match explicit_mq {
        Some(value) => {
            crate::parse_bool(value).map_err(|_| "sqm_use_mq must be boolean".to_string())?
        }
        None if alias_mq => true,
        None => retained_mq
            .map(crate::parse_bool)
            .transpose()
            .map_err(|_| "retained SQM use_mq must be boolean".to_string())?
            .unwrap_or(false),
    };
    Ok(CakeIntent {
        use_mq,
        legacy_alias: qdisc == "cake-mq" || qdisc == "cake_mq",
    })
}

pub(crate) fn safe_script(script: &str) -> bool {
    !script.is_empty()
        && script.len() <= 128
        && script.ends_with(".qos")
        && !script.starts_with('.')
        && !script.contains("..")
        && script
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

pub(crate) fn validate(
    options: &HashMap<String, String>,
    retained_mq: Option<&str>,
    mq_supported: impl FnOnce(&str) -> Result<bool, String>,
) -> Result<CakeIntent, String> {
    let enabled = options
        .get("enabled")
        .map(|v| crate::parse_bool(v))
        .transpose()?
        .unwrap_or(false);
    let sqm_enabled = options
        .get("sqm_enabled")
        .map(|v| crate::parse_bool(v))
        .transpose()?
        .unwrap_or(enabled);
    let direction = options
        .get("sqm_direction_mode")
        .map(String::as_str)
        .unwrap_or("both");
    if !matches!(direction, "both" | "upload_only" | "download_only" | "off") {
        return Err("sqm_direction_mode must be both, upload_only, download_only or off".into());
    }
    let mode = intent(
        options
            .get("sqm_qdisc")
            .map(String::as_str)
            .unwrap_or("cake"),
        options.get("sqm_use_mq").map(String::as_str),
        retained_mq,
    )?;
    let script = options
        .get("sqm_script")
        .map(String::as_str)
        .unwrap_or("piece_of_cake.qos");
    if !safe_script(script) {
        return Err("sqm_script must be a safe installed .qos basename".to_string());
    }
    for (key, minimum, maximum) in [
        (
            "sqm_download",
            0i64,
            crate::rate_limits::MAX_RATE_KBPS as i64,
        ),
        ("sqm_upload", 0, crate::rate_limits::MAX_RATE_KBPS as i64),
        ("sqm_overhead", -1500, 1500),
        ("sqm_tcMTU", 1, 65535),
        ("sqm_tcTSIZE", 1, 65535),
        ("sqm_tcMPU", 0, 65535),
    ] {
        if let Some(value) = options.get(key).filter(|value| !value.is_empty()) {
            if value
                .parse::<i64>()
                .ok()
                .is_none_or(|value| value < minimum || value > maximum)
            {
                return Err(format!(
                    "{key} must be an integer between {minimum} and {maximum}"
                ));
            }
        }
    }
    if let Some(linklayer) = options.get("sqm_linklayer") {
        if !matches!(linklayer.as_str(), "none" | "ethernet" | "atm") {
            return Err("sqm_linklayer must be none, ethernet or atm".to_string());
        }
    }
    if enabled && sqm_enabled && direction != "off" && mode.use_mq && !mq_supported(script)? {
        return Err(
            "multi-queue CAKE requires verified kernel, tc and SQM script support".to_string(),
        );
    }
    Ok(mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn r3_qdisc_whitelist_and_legacy_alias_preserve_explicit_user_intent() {
        for value in ["fq_codel", "mq", "cake;command", ""] {
            assert!(intent(value, None, None).is_err());
        }
        assert!(!intent("cake", None, None).unwrap().use_mq);
        assert!(intent("cake-mq", None, None).unwrap().use_mq);
        assert!(intent("cake_mq", None, None).unwrap().use_mq);
        assert!(!intent("cake-mq", Some("0"), None).unwrap().use_mq);
        assert!(intent("cake", None, Some("1")).unwrap().use_mq);
        assert!(!intent("cake", Some("0"), Some("1")).unwrap().use_mq);
    }
    #[test]
    fn r3_mq_admission_requires_capability_and_safe_script() {
        let mut options = HashMap::from([
            ("sqm_use_mq".into(), "1".into()),
            ("enabled".into(), "1".into()),
        ]);
        assert!(validate(&options, None, |_| Ok(false)).is_err());
        assert!(validate(&options, None, |script| Ok(script == "piece_of_cake.qos")).is_ok());
        options.insert("sqm_script".into(), "../../foreign.qos".into());
        assert!(validate(&options, None, |_| panic!(
            "must reject before capability lookup"
        ))
        .is_err());
    }
    #[test]
    fn r3_sqm_numeric_options_reject_nonfinite_and_out_of_range() {
        for (key, value) in [
            ("sqm_download", "NaN"),
            ("sqm_upload", "-1"),
            ("sqm_overhead", "1501"),
            ("sqm_tcTSIZE", "0"),
        ] {
            assert!(
                validate(&HashMap::from([(key.into(), value.into())]), None, |_| Ok(
                    false
                ))
                .is_err()
            );
        }
        assert!(validate(
            &HashMap::from([("sqm_overhead".into(), "-44".into())]),
            None,
            |_| Ok(false)
        )
        .is_ok());
    }
}
