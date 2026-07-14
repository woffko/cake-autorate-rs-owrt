#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    Ethernet,
    Pppoe,
    Cellular,
    Unknown,
}

impl LinkKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ethernet" => Some(Self::Ethernet),
            "pppoe" => Some(Self::Pppoe),
            "cellular" | "wwan" => Some(Self::Cellular),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Ethernet => "ethernet",
            Self::Pppoe => "pppoe",
            Self::Cellular => "cellular",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LatencyBaseline {
    pub median_ms: f64,
    pub p95_ms: f64,
    pub samples: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DirectionProposal {
    pub minimum_kbps: u64,
    pub base_kbps: u64,
    pub maximum_kbps: u64,
    pub absolute_cap_kbps: u64,
    pub observed_low_kbps: u64,
    pub observed_median_kbps: u64,
    pub observed_high_kbps: u64,
    pub variability: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AutotuneProposal {
    pub download: DirectionProposal,
    pub upload: DirectionProposal,
    pub active_threshold_kbps: u64,
    pub adjust_up_threshold_ms: u64,
    pub delay_threshold_ms: u64,
    pub adjust_down_threshold_ms: u64,
    pub adaptive_ceiling_enabled: bool,
    pub adaptive_hold_s: u64,
    pub adaptive_growth_percent: u64,
    pub adaptive_probe_s: u64,
    pub adaptive_cooldown_s: u64,
    pub adaptive_failed_bound_ttl_s: u64,
    pub link_kind: LinkKind,
    pub link_layer: &'static str,
    pub overhead: u64,
    pub mpu: u64,
    pub confidence: u64,
    pub warnings: Vec<&'static str>,
}

impl AutotuneProposal {
    pub fn revise_base_rates(&mut self, scale: f64) -> Result<(), String> {
        if !scale.is_finite() || !(0.5..=1.2).contains(&scale) {
            return Err("base-rate revision scale must be between 0.5 and 1.2".to_string());
        }
        revise_direction_base(&mut self.download, scale);
        revise_direction_base(&mut self.upload, scale);
        Ok(())
    }

    pub fn apply_conservative_constraints(
        &mut self,
        retain_download: Option<DirectionProposal>,
        retain_upload: Option<DirectionProposal>,
        confirmed_download_max: Option<u64>,
        confirmed_download_cap: Option<u64>,
        confirmed_upload_max: Option<u64>,
        confirmed_upload_cap: Option<u64>,
    ) {
        constrain_direction(
            &mut self.download,
            retain_download,
            confirmed_download_max,
            confirmed_download_cap,
        );
        constrain_direction(
            &mut self.upload,
            retain_upload,
            confirmed_upload_max,
            confirmed_upload_cap,
        );
        self.confidence = self.confidence.min(45);
        self.warnings.push(
            "Low-confidence conservative calibration: measured background was subtracted and confirmed maxima/caps were never raised.",
        );
    }

    pub fn to_json(&self) -> String {
        let warnings = self
            .warnings
            .iter()
            .map(|warning| format!("\"{}\"", json_escape(warning)))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            concat!(
                "{{\"schema_version\":1,\"profile\":\"balanced\",",
                "\"download\":{},\"upload\":{},",
                "\"active_threshold_kbps\":{},",
                "\"thresholds_ms\":{{\"adjust_up\":{},\"delay\":{},\"adjust_down\":{}}},",
                "\"adaptive_ceiling\":{{\"enabled\":{},\"hold_s\":{},\"growth_percent\":{},",
                "\"probe_s\":{},\"cooldown_s\":{},\"failed_bound_ttl_s\":{}}},",
                "\"link\":{{\"kind\":\"{}\",\"layer\":\"{}\",\"overhead\":{},\"mpu\":{}}},",
                "\"confidence\":{},\"warnings\":[{}]}}"
            ),
            direction_json(self.download),
            direction_json(self.upload),
            self.active_threshold_kbps,
            self.adjust_up_threshold_ms,
            self.delay_threshold_ms,
            self.adjust_down_threshold_ms,
            self.adaptive_ceiling_enabled,
            self.adaptive_hold_s,
            self.adaptive_growth_percent,
            self.adaptive_probe_s,
            self.adaptive_cooldown_s,
            self.adaptive_failed_bound_ttl_s,
            self.link_kind.as_str(),
            self.link_layer,
            self.overhead,
            self.mpu,
            self.confidence,
            warnings,
        )
    }
}

pub fn build_proposal(
    download_samples_kbps: &[f64],
    upload_samples_kbps: &[f64],
    baseline: LatencyBaseline,
    link_kind: LinkKind,
) -> Result<AutotuneProposal, String> {
    let download = propose_direction(download_samples_kbps)?;
    let upload = propose_direction(upload_samples_kbps)?;
    let variable = download.variability >= 0.15 || upload.variability >= 0.15;
    let jitter_ms = (baseline.p95_ms - baseline.median_ms).max(0.0);
    let adjust_up_threshold_ms = (jitter_ms * 1.5).clamp(3.0, 15.0).ceil() as u64;
    let delay_threshold_ms = (adjust_up_threshold_ms + 8).max(15);
    let adjust_down_threshold_ms = (delay_threshold_ms + 25).max(40);
    // Activity detection must stay well below the weakest observed direction.
    // Using a percentage of the proposed minimum is too high when one
    // direction looked stable during a short, otherwise variable calibration.
    let smallest_observed = download.observed_low_kbps.min(upload.observed_low_kbps);
    let active_threshold_kbps = rounded_rate(smallest_observed as f64 / 10.0).clamp(500, 20_000);
    let (link_layer, overhead, mpu) = match link_kind {
        LinkKind::Pppoe => ("ethernet", 44, 84),
        LinkKind::Ethernet => ("ethernet", 18, 64),
        LinkKind::Cellular | LinkKind::Unknown => ("none", 0, 0),
    };
    let mut warnings = Vec::new();

    if download_samples_kbps.len() < 2 || upload_samples_kbps.len() < 2 {
        warnings.push("Only one throughput sample was available; repeat calibration before trusting the limits.");
    }
    if variable {
        warnings.push("Measured capacity is variable; conservative base rates and bounded adaptive ceiling are recommended.");
    }
    if link_kind == LinkKind::Unknown {
        warnings.push("Link-layer encapsulation could not be detected; verify overhead before applying the proposal.");
    }
    if baseline.samples == 0 {
        warnings
            .push("Idle latency baseline was unavailable; default thresholds need manual review.");
    }

    let sample_confidence = download_samples_kbps
        .len()
        .min(upload_samples_kbps.len())
        .min(3) as u64
        * 20;
    let latency_confidence = if baseline.samples >= 5
        && baseline.median_ms > 0.0
        && baseline.p95_ms >= baseline.median_ms
    {
        25
    } else {
        0
    };
    let link_confidence = if link_kind == LinkKind::Unknown {
        0
    } else {
        15
    };

    Ok(AutotuneProposal {
        download,
        upload,
        active_threshold_kbps,
        adjust_up_threshold_ms,
        delay_threshold_ms,
        adjust_down_threshold_ms,
        adaptive_ceiling_enabled: variable,
        adaptive_hold_s: if variable { 15 } else { 20 },
        adaptive_growth_percent: 3,
        adaptive_probe_s: 8,
        adaptive_cooldown_s: if variable { 45 } else { 60 },
        adaptive_failed_bound_ttl_s: if variable { 900 } else { 1800 },
        link_kind,
        link_layer,
        overhead,
        mpu,
        confidence: (sample_confidence + latency_confidence + link_confidence).min(100),
        warnings,
    })
}

fn propose_direction(samples_kbps: &[f64]) -> Result<DirectionProposal, String> {
    let mut samples = samples_kbps
        .iter()
        .copied()
        .filter(|sample| sample.is_finite() && *sample > 0.0)
        .collect::<Vec<_>>();
    if samples.is_empty() {
        return Err("at least one positive throughput sample is required".to_string());
    }
    samples.sort_by(f64::total_cmp);

    let low = if samples.len() <= 3 {
        samples[0]
    } else {
        percentile(&samples, 0.20)
    };
    let median = percentile(&samples, 0.50);
    let high = percentile(&samples, 0.90);
    let variability = ((high - low) / median.max(1.0)).max(0.0);
    let variable = variability >= 0.15;

    let (minimum, base, maximum, cap) = if variable {
        (low * 0.40, low * 0.85, high * 1.25, high * 1.80)
    } else {
        (low * 0.70, low * 0.88, high * 0.95, high * 1.05)
    };
    let minimum = rounded_rate(minimum);
    let base = rounded_rate(base).max(minimum);
    let maximum = rounded_rate(maximum).max(base);
    let absolute_cap = rounded_rate(cap).max(maximum);

    Ok(DirectionProposal {
        minimum_kbps: minimum,
        base_kbps: base,
        maximum_kbps: maximum,
        absolute_cap_kbps: absolute_cap,
        observed_low_kbps: rounded_rate(low),
        observed_median_kbps: rounded_rate(median),
        observed_high_kbps: rounded_rate(high),
        variability,
    })
}

fn percentile(sorted: &[f64], percentile: f64) -> f64 {
    if sorted.len() == 1 {
        return sorted[0];
    }
    let position = percentile.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    if lower == upper {
        sorted[lower]
    } else {
        let weight = position - lower as f64;
        sorted[lower] * (1.0 - weight) + sorted[upper] * weight
    }
}

fn rounded_rate(rate_kbps: f64) -> u64 {
    ((rate_kbps.max(100.0) / 100.0).round() * 100.0) as u64
}

fn revise_direction_base(direction: &mut DirectionProposal, scale: f64) {
    let upper = rounded_rate(direction.observed_low_kbps as f64 * 0.95).min(direction.maximum_kbps);
    direction.base_kbps = rounded_rate(direction.base_kbps as f64 * scale)
        .max(direction.minimum_kbps)
        .min(upper.max(direction.minimum_kbps));
}

fn constrain_direction(
    direction: &mut DirectionProposal,
    retained: Option<DirectionProposal>,
    confirmed_max: Option<u64>,
    confirmed_cap: Option<u64>,
) {
    if let Some(retained) = retained {
        *direction = retained;
        return;
    }

    let cap_bound = confirmed_cap.filter(|value| *value > 0).or(confirmed_max);
    if let Some(cap) = cap_bound {
        direction.absolute_cap_kbps = direction.absolute_cap_kbps.min(cap);
        direction.maximum_kbps = direction.maximum_kbps.min(direction.absolute_cap_kbps);
        direction.base_kbps = direction.base_kbps.min(direction.maximum_kbps);
        direction.minimum_kbps = direction.minimum_kbps.min(direction.base_kbps);
    }
    if let Some(maximum) = confirmed_max.filter(|value| *value > 0) {
        direction.maximum_kbps = direction.maximum_kbps.min(maximum);
        direction.base_kbps = direction.base_kbps.min(direction.maximum_kbps);
        direction.minimum_kbps = direction.minimum_kbps.min(direction.base_kbps);
        direction.absolute_cap_kbps = direction.absolute_cap_kbps.max(direction.maximum_kbps);
    }
}

fn direction_json(direction: DirectionProposal) -> String {
    format!(
        concat!(
            "{{\"minimum_kbps\":{},\"base_kbps\":{},\"maximum_kbps\":{},",
            "\"absolute_cap_kbps\":{},\"observed_low_kbps\":{},",
            "\"observed_median_kbps\":{},\"observed_high_kbps\":{},",
            "\"variability\":{:.4}}}"
        ),
        direction.minimum_kbps,
        direction.base_kbps,
        direction.maximum_kbps,
        direction.absolute_cap_kbps,
        direction.observed_low_kbps,
        direction.observed_median_kbps,
        direction.observed_high_kbps,
        direction.variability,
    )
}

fn json_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_fibre_proposal_keeps_headroom_without_enabling_adaptive_ceiling() {
        let proposal = build_proposal(
            &[896_000.0, 904_000.0, 900_000.0],
            &[764_000.0, 780_000.0, 772_000.0],
            LatencyBaseline {
                median_ms: 2.0,
                p95_ms: 3.0,
                samples: 15,
            },
            LinkKind::Pppoe,
        )
        .unwrap();

        assert!(!proposal.adaptive_ceiling_enabled);
        assert_eq!(proposal.download.base_kbps, 788_500);
        assert_eq!(proposal.download.maximum_kbps, 858_000);
        assert_eq!(proposal.overhead, 44);
        assert_eq!(proposal.mpu, 84);
        assert!(proposal.download.minimum_kbps <= proposal.download.base_kbps);
        assert!(proposal.download.base_kbps <= proposal.download.maximum_kbps);
        assert!(proposal.download.maximum_kbps <= proposal.download.absolute_cap_kbps);
    }

    #[test]
    fn variable_cellular_proposal_uses_low_sample_for_base_and_bounded_growth() {
        let proposal = build_proposal(
            &[41_800.0, 108_260.0, 114_770.0],
            &[16_200.0, 17_430.0, 18_250.0],
            LatencyBaseline {
                median_ms: 20.0,
                p95_ms: 24.0,
                samples: 15,
            },
            LinkKind::Cellular,
        )
        .unwrap();

        assert!(proposal.adaptive_ceiling_enabled);
        assert_eq!(proposal.download.minimum_kbps, 16_700);
        assert_eq!(proposal.download.base_kbps, 35_500);
        assert_eq!(proposal.download.maximum_kbps, 141_800);
        assert_eq!(proposal.download.absolute_cap_kbps, 204_200);
        assert_eq!(proposal.active_threshold_kbps, 1_600);
        assert_eq!(proposal.adjust_up_threshold_ms, 6);
        assert_eq!(proposal.delay_threshold_ms, 15);
        assert_eq!(proposal.adjust_down_threshold_ms, 40);
        assert!(proposal
            .warnings
            .iter()
            .any(|warning| warning.contains("variable")));
    }

    #[test]
    fn asymmetric_directions_are_calculated_independently() {
        let proposal = build_proposal(
            &[100_000.0, 105_000.0],
            &[10_000.0, 40_000.0],
            LatencyBaseline {
                median_ms: 10.0,
                p95_ms: 12.0,
                samples: 10,
            },
            LinkKind::Ethernet,
        )
        .unwrap();

        assert!(!proposal.download.variability.is_sign_negative());
        assert!(proposal.upload.variability > proposal.download.variability);
        assert!(proposal.upload.maximum_kbps > proposal.upload.base_kbps);
        assert!(proposal.adaptive_ceiling_enabled);
    }

    #[test]
    fn conservative_constraints_never_raise_confirmed_bounds_and_can_retain_direction() {
        let mut proposal = build_proposal(
            &[900_000.0, 880_000.0],
            &[900_000.0, 870_000.0],
            LatencyBaseline {
                median_ms: 4.0,
                p95_ms: 6.0,
                samples: 10,
            },
            LinkKind::Ethernet,
        )
        .unwrap();
        let retained_upload = DirectionProposal {
            minimum_kbps: 10_000,
            base_kbps: 20_000,
            maximum_kbps: 30_000,
            absolute_cap_kbps: 35_000,
            observed_low_kbps: proposal.upload.observed_low_kbps,
            observed_median_kbps: proposal.upload.observed_median_kbps,
            observed_high_kbps: proposal.upload.observed_high_kbps,
            variability: proposal.upload.variability,
        };

        proposal.apply_conservative_constraints(
            None,
            Some(retained_upload),
            Some(700_000),
            Some(750_000),
            Some(30_000),
            Some(35_000),
        );

        assert!(proposal.download.maximum_kbps <= 700_000);
        assert!(proposal.download.absolute_cap_kbps <= 750_000);
        assert_eq!(proposal.upload, retained_upload);
        assert!(proposal.confidence <= 45);
        assert!(proposal
            .warnings
            .iter()
            .any(|warning| warning.contains("Low-confidence")));
    }

    #[test]
    fn invalid_measurements_are_rejected() {
        assert!(build_proposal(
            &[0.0, f64::NAN],
            &[10_000.0],
            LatencyBaseline {
                median_ms: 1.0,
                p95_ms: 2.0,
                samples: 10,
            },
            LinkKind::Unknown,
        )
        .is_err());
    }

    #[test]
    fn json_contains_apply_ready_fields() {
        let json = build_proposal(
            &[100_000.0, 110_000.0],
            &[20_000.0, 22_000.0],
            LatencyBaseline {
                median_ms: 5.0,
                p95_ms: 8.0,
                samples: 10,
            },
            LinkKind::Pppoe,
        )
        .unwrap()
        .to_json();

        assert!(json.contains("\"schema_version\":1"));
        assert!(json.contains("\"minimum_kbps\""));
        assert!(json.contains("\"adaptive_ceiling\""));
        assert!(json.contains("\"overhead\":44"));
    }

    #[test]
    fn bounded_revision_changes_only_base_rates_and_preserves_order() {
        let mut proposal = build_proposal(
            &[40_000.0, 100_000.0],
            &[10_000.0, 20_000.0],
            LatencyBaseline {
                median_ms: 10.0,
                p95_ms: 15.0,
                samples: 10,
            },
            LinkKind::Cellular,
        )
        .unwrap();
        let original_dl_max = proposal.download.maximum_kbps;
        let original_dl_base = proposal.download.base_kbps;

        proposal.revise_base_rates(0.85).unwrap();
        assert!(proposal.download.base_kbps < original_dl_base);
        assert_eq!(proposal.download.maximum_kbps, original_dl_max);
        assert!(proposal.download.minimum_kbps <= proposal.download.base_kbps);

        proposal.revise_base_rates(1.2).unwrap();
        assert!(proposal.download.base_kbps <= proposal.download.observed_low_kbps * 95 / 100);
        assert!(proposal.revise_base_rates(0.1).is_err());
    }
}
