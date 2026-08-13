//! Immutable measurement policy for UCI-absent native Full Auto-Tune jobs.
//!
//! A bootstrap job cannot borrow probe settings from an instance because the
//! instance intentionally does not exist until Review is accepted.  The
//! request therefore carries a versioned policy identifier and the canonical
//! digest of its fully expanded values.  An existing identifier is immutable;
//! any future policy change gets a new identifier and request schema record.

use ring::digest::{digest, SHA256};

const CAPTURE_POLICY_V1_SCHEMA_VERSION: u8 = 1;
const CAPTURE_POLICY_V2_SCHEMA_VERSION: u8 = 2;
const MAX_CAPTURE_POLICY_BYTES: usize = 16 * 1024;
const MAX_REFLECTORS: usize = 64;

const STANDARD_V1_REFLECTORS: &[&str] = &[
    "1.1.1.1",
    "1.0.0.1",
    "8.8.8.8",
    "8.8.4.4",
    "9.9.9.9",
    "9.9.9.10",
    "9.9.9.11",
    "94.140.14.15",
    "94.140.14.140",
    "94.140.14.141",
    "94.140.15.15",
    "94.140.15.16",
    "64.6.65.6",
    "156.154.70.1",
    "156.154.70.2",
    "156.154.70.3",
    "156.154.70.4",
    "156.154.70.5",
    "156.154.71.1",
    "156.154.71.2",
    "156.154.71.3",
    "156.154.71.4",
    "156.154.71.5",
    "208.67.220.2",
    "208.67.220.123",
    "208.67.220.220",
    "208.67.222.2",
    "208.67.222.123",
    "185.228.168.9",
    "185.228.168.10",
];

pub(crate) fn standard_v1_reflectors() -> Vec<String> {
    STANDARD_V1_REFLECTORS
        .iter()
        .map(|value| (*value).to_string())
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutotuneCapturePolicyId {
    StandardV1,
    StandardV2,
}

impl AutotuneCapturePolicyId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StandardV1 => "standard_v1",
            Self::StandardV2 => "standard_v2",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "standard_v1" => Some(Self::StandardV1),
            "standard_v2" => Some(Self::StandardV2),
            _ => None,
        }
    }

    pub fn expand(self) -> Result<AutotuneCapturePolicy, String> {
        let maximum_counter_delta_span_ms = match self {
            Self::StandardV1 => 600,
            Self::StandardV2 => 1_500,
        };
        let value = AutotuneCapturePolicy {
            id: self,
            pinger_method: "fping",
            active_pingers: 6,
            reflectors: standard_v1_reflectors(),
            reflector_ping_interval_ms: 300,
            reflector_response_deadline_ms: 1_000,
            pinger_timeout_ms: 10_000,
            transport_backend: "websocket",
            transport_endpoint: "wss://ping-bufferbloat.libreqos.com/ws",
            transport_baseline_learning_interval_ms: 1_000,
            transport_loaded_interval_ms: 1_000,
            transport_timeout_ms: 5_000,
            transport_load_hold_ms: 3_000,
            transport_min_loaded_coverage_percent: 70,
            rate_sample_interval_ms: 200,
            maximum_counter_delta_span_ms,
            load_threshold_kbps: 2_000,
            reverse_ack_ratio_ppm: 80_000,
            cpu_sample_interval_ms: 2_000,
        };
        value.validate()?;
        let _ = value.canonical_bytes()?;
        Ok(value)
    }

    pub fn canonical_sha256(self) -> Result<String, String> {
        self.expand()?.canonical_sha256()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutotuneCapturePolicy {
    id: AutotuneCapturePolicyId,
    pinger_method: &'static str,
    active_pingers: u8,
    reflectors: Vec<String>,
    reflector_ping_interval_ms: u32,
    reflector_response_deadline_ms: u32,
    pinger_timeout_ms: u32,
    transport_backend: &'static str,
    transport_endpoint: &'static str,
    transport_baseline_learning_interval_ms: u32,
    transport_loaded_interval_ms: u32,
    transport_timeout_ms: u32,
    transport_load_hold_ms: u32,
    transport_min_loaded_coverage_percent: u8,
    rate_sample_interval_ms: u32,
    maximum_counter_delta_span_ms: u32,
    load_threshold_kbps: u64,
    reverse_ack_ratio_ppm: u32,
    cpu_sample_interval_ms: u32,
}

impl AutotuneCapturePolicy {
    pub fn id(&self) -> AutotuneCapturePolicyId {
        self.id
    }

    pub fn pinger_method(&self) -> &'static str {
        self.pinger_method
    }

    pub fn active_pingers(&self) -> u8 {
        self.active_pingers
    }

    pub fn reflectors(&self) -> &[String] {
        &self.reflectors
    }

    pub fn reflector_ping_interval_ms(&self) -> u32 {
        self.reflector_ping_interval_ms
    }

    pub fn reflector_response_deadline_ms(&self) -> u32 {
        self.reflector_response_deadline_ms
    }

    pub fn pinger_timeout_ms(&self) -> u32 {
        self.pinger_timeout_ms
    }

    pub fn transport_backend(&self) -> &'static str {
        self.transport_backend
    }

    pub fn transport_endpoint(&self) -> &'static str {
        self.transport_endpoint
    }

    pub fn transport_baseline_learning_interval_ms(&self) -> u32 {
        self.transport_baseline_learning_interval_ms
    }

    pub fn transport_loaded_interval_ms(&self) -> u32 {
        self.transport_loaded_interval_ms
    }

    pub fn transport_timeout_ms(&self) -> u32 {
        self.transport_timeout_ms
    }

    pub fn transport_load_hold_ms(&self) -> u32 {
        self.transport_load_hold_ms
    }

    pub fn transport_min_loaded_coverage_percent(&self) -> u8 {
        self.transport_min_loaded_coverage_percent
    }

    pub fn rate_sample_interval_ms(&self) -> u32 {
        self.rate_sample_interval_ms
    }

    /// Maximum consecutive physical counter-endpoint span which may
    /// contribute byte/phase evidence.  This is deliberately independent of
    /// the shorter one-shot endpoint-attestation and transport-dispatch
    /// authority lifetime.
    pub fn maximum_counter_delta_span_ms(&self) -> u32 {
        self.maximum_counter_delta_span_ms
    }

    pub fn load_threshold_kbps(&self) -> u64 {
        self.load_threshold_kbps
    }

    pub fn reverse_ack_ratio_ppm(&self) -> u32 {
        self.reverse_ack_ratio_ppm
    }

    pub fn cpu_sample_interval_ms(&self) -> u32 {
        self.cpu_sample_interval_ms
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let reflectors = self
            .reflectors
            .iter()
            .map(|value| format!("\"{value}\""))
            .collect::<Vec<_>>()
            .join(",");
        let output = match self.id {
            AutotuneCapturePolicyId::StandardV1 => format!(
                concat!(
                    "{{\"autotune_capture_policy_schema_version\":{},",
                    "\"policy_id\":\"{}\",",
                    "\"icmp\":{{\"method\":\"{}\",\"active_count\":{},",
                    "\"reflectors\":[{}],\"interval_ms\":{},",
                    "\"response_deadline_ms\":{},\"probe_timeout_ms\":{}}},",
                    "\"transport\":{{\"backend\":\"{}\",\"endpoint\":\"{}\",",
                    "\"baseline_learning_interval_ms\":{},\"loaded_interval_ms\":{},",
                    "\"timeout_ms\":{},\"load_hold_ms\":{},",
                    "\"minimum_loaded_coverage_percent\":{}}},",
                    "\"load\":{{\"rate_sample_interval_ms\":{},",
                    "\"active_threshold_kbps\":{},\"reverse_ack_ratio_ppm\":{}}},",
                    "\"cpu\":{{\"sample_interval_ms\":{}}}}}}}\n"
                ),
                CAPTURE_POLICY_V1_SCHEMA_VERSION,
                self.id.as_str(),
                self.pinger_method,
                self.active_pingers,
                reflectors,
                self.reflector_ping_interval_ms,
                self.reflector_response_deadline_ms,
                self.pinger_timeout_ms,
                self.transport_backend,
                self.transport_endpoint,
                self.transport_baseline_learning_interval_ms,
                self.transport_loaded_interval_ms,
                self.transport_timeout_ms,
                self.transport_load_hold_ms,
                self.transport_min_loaded_coverage_percent,
                self.rate_sample_interval_ms,
                self.load_threshold_kbps,
                self.reverse_ack_ratio_ppm,
                self.cpu_sample_interval_ms,
            ),
            AutotuneCapturePolicyId::StandardV2 => format!(
                concat!(
                    "{{\"autotune_capture_policy_schema_version\":{},",
                    "\"policy_id\":\"{}\",",
                    "\"icmp\":{{\"method\":\"{}\",\"active_count\":{},",
                    "\"reflectors\":[{}],\"interval_ms\":{},",
                    "\"response_deadline_ms\":{},\"probe_timeout_ms\":{}}},",
                    "\"transport\":{{\"backend\":\"{}\",\"endpoint\":\"{}\",",
                    "\"baseline_learning_interval_ms\":{},\"loaded_interval_ms\":{},",
                    "\"timeout_ms\":{},\"load_hold_ms\":{},",
                    "\"minimum_loaded_coverage_percent\":{}}},",
                    "\"load\":{{\"rate_sample_interval_ms\":{},",
                    "\"maximum_counter_delta_span_ms\":{},",
                    "\"active_threshold_kbps\":{},\"reverse_ack_ratio_ppm\":{}}},",
                    "\"cpu\":{{\"sample_interval_ms\":{}}}}}}}\n"
                ),
                CAPTURE_POLICY_V2_SCHEMA_VERSION,
                self.id.as_str(),
                self.pinger_method,
                self.active_pingers,
                reflectors,
                self.reflector_ping_interval_ms,
                self.reflector_response_deadline_ms,
                self.pinger_timeout_ms,
                self.transport_backend,
                self.transport_endpoint,
                self.transport_baseline_learning_interval_ms,
                self.transport_loaded_interval_ms,
                self.transport_timeout_ms,
                self.transport_load_hold_ms,
                self.transport_min_loaded_coverage_percent,
                self.rate_sample_interval_ms,
                self.maximum_counter_delta_span_ms,
                self.load_threshold_kbps,
                self.reverse_ack_ratio_ppm,
                self.cpu_sample_interval_ms,
            ),
        }
        .into_bytes();
        if output.len() > MAX_CAPTURE_POLICY_BYTES {
            return Err("native Auto-Tune capture policy exceeds its byte bound".to_string());
        }
        Ok(output)
    }

    pub fn canonical_sha256(&self) -> Result<String, String> {
        Ok(hex_digest(&self.canonical_bytes()?))
    }

    fn validate(&self) -> Result<(), String> {
        let expected_maximum_counter_delta_span_ms = match self.id {
            AutotuneCapturePolicyId::StandardV1 => 600,
            AutotuneCapturePolicyId::StandardV2 => 1_500,
        };
        if self.pinger_method != "fping"
            || self.active_pingers == 0
            || usize::from(self.active_pingers) > self.reflectors.len()
            || self.reflectors.is_empty()
            || self.reflectors.len() > MAX_REFLECTORS
        {
            return Err("native Auto-Tune capture ICMP policy is invalid".to_string());
        }
        for reflector in &self.reflectors {
            if reflector.is_empty()
                || reflector.len() > 255
                || reflector.starts_with('-')
                || !reflector
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b":.-".contains(&byte))
            {
                return Err("native Auto-Tune capture reflector is invalid".to_string());
            }
        }
        if self.reflector_ping_interval_ms == 0
            || self.reflector_response_deadline_ms < self.reflector_ping_interval_ms
            || self.pinger_timeout_ms < self.reflector_response_deadline_ms
            || self.transport_backend != "websocket"
            || !self.transport_endpoint.starts_with("wss://")
            || self.transport_endpoint.len() > 512
            || self
                .transport_endpoint
                .bytes()
                .any(|byte| byte.is_ascii_control())
            || self.transport_baseline_learning_interval_ms == 0
            || self.transport_loaded_interval_ms == 0
            || self.transport_timeout_ms == 0
            || self.transport_load_hold_ms == 0
            || !(50..=100).contains(&self.transport_min_loaded_coverage_percent)
            || !(100..=5_000).contains(&self.rate_sample_interval_ms)
            || self.maximum_counter_delta_span_ms != expected_maximum_counter_delta_span_ms
            || self.maximum_counter_delta_span_ms < self.rate_sample_interval_ms
            || self.maximum_counter_delta_span_ms > self.transport_load_hold_ms.saturating_div(2)
            || self.load_threshold_kbps == 0
            || self.reverse_ack_ratio_ppm > 250_000
            || self.cpu_sample_interval_ms == 0
        {
            return Err("native Auto-Tune capture timing policy is invalid".to_string());
        }
        Ok(())
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_v1_is_bounded_complete_and_digest_frozen() {
        let policy = AutotuneCapturePolicyId::StandardV1.expand().unwrap();
        assert_eq!(policy.id(), AutotuneCapturePolicyId::StandardV1);
        assert_eq!(policy.reflectors().len(), 30);
        assert_eq!(policy.active_pingers(), 6);
        assert_eq!(policy.pinger_method(), "fping");
        assert_eq!(policy.transport_backend(), "websocket");
        assert_eq!(policy.rate_sample_interval_ms(), 200);
        assert_eq!(policy.maximum_counter_delta_span_ms(), 600);
        assert_eq!(policy.reverse_ack_ratio_ppm(), 80_000);
        assert!(policy.canonical_bytes().unwrap().len() < MAX_CAPTURE_POLICY_BYTES);
        assert_eq!(
            policy.canonical_sha256().unwrap(),
            "9a2ff68c05b5e18f1a90041daefa72cb8ff047b1b2c895aa5b63bb4348618e9f"
        );
    }

    #[test]
    fn standard_v2_separates_physical_delta_span_from_dispatch_freshness() {
        let policy = AutotuneCapturePolicyId::StandardV2.expand().unwrap();
        assert_eq!(policy.id(), AutotuneCapturePolicyId::StandardV2);
        assert_eq!(policy.rate_sample_interval_ms(), 200);
        assert_eq!(policy.maximum_counter_delta_span_ms(), 1_500);
        assert_eq!(policy.transport_load_hold_ms(), 3_000);
        assert_eq!(policy.canonical_bytes().unwrap().len(), 1_021);
        assert_eq!(
            policy.canonical_sha256().unwrap(),
            "de4f857213a30eb6c0134702b7eb93d0ab3dff6ebb27802a99c7f0ab717f7d2e"
        );
    }

    #[test]
    fn identifiers_are_explicit_and_unknown_versions_fail_closed() {
        assert_eq!(
            AutotuneCapturePolicyId::parse("standard_v1"),
            Some(AutotuneCapturePolicyId::StandardV1)
        );
        assert_eq!(
            AutotuneCapturePolicyId::parse("standard_v2"),
            Some(AutotuneCapturePolicyId::StandardV2)
        );
        assert_eq!(AutotuneCapturePolicyId::parse("standard"), None);
        assert_eq!(AutotuneCapturePolicyId::parse("standard_v3"), None);
    }
}
