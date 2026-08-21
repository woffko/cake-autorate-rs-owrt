//! Shared default reflector addresses for the runtime controller.
//!
//! These defaults belong to the normal manually configured controller.  Keep
//! them outside the optional calibration feature so the Lite build does not
//! need to compile any Auto-Tune policy or digest implementation merely to
//! seed its ordinary latency probes.

const STANDARD_REFLECTORS: &[&str] = &[
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

pub(crate) fn standard_reflectors() -> Vec<String> {
    STANDARD_REFLECTORS
        .iter()
        .map(|value| (*value).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_stable_unique_ipv4_literals() {
        let reflectors = standard_reflectors();
        assert_eq!(reflectors.len(), 30);
        let unique = reflectors.iter().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(unique.len(), reflectors.len());
        assert!(reflectors
            .iter()
            .all(|value| value.parse::<std::net::Ipv4Addr>().is_ok()));
    }
}
