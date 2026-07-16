use crate::transport_quality::classify_quality;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutotuneProfile {
    Gaming,
    BestOverall,
    Fair,
}

impl AutotuneProfile {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "gaming" => Some(Self::Gaming),
            "best_overall" | "best-overall" | "balanced" => Some(Self::BestOverall),
            "fair" => Some(Self::Fair),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gaming => "gaming",
            Self::BestOverall => "best_overall",
            Self::Fair => "fair",
        }
    }

    pub fn target_grade(self) -> &'static str {
        match self {
            Self::Gaming => "A+",
            Self::BestOverall => "A",
            Self::Fair => "C",
        }
    }

    pub fn quality_target_required(self) -> bool {
        self != Self::Fair
    }

    pub fn throughput_priority(self) -> bool {
        self == Self::Fair
    }

    pub fn validation_thresholds(self) -> ValidationThresholds {
        let (capacity_retention_min_percent, latency_delta_max_ms, loss_max_percent) = match self {
            Self::Gaming => (70.0, 5.0, 1.0),
            Self::BestOverall => (80.0, 30.0, 3.0),
            Self::Fair => (90.0, 200.0, 5.0),
        };
        ValidationThresholds {
            candidate_realization_min_percent: 80.0,
            candidate_realization_max_percent: 110.0,
            capacity_retention_min_percent,
            icmp_delta_max_ms: latency_delta_max_ms,
            transport_delta_max_ms: latency_delta_max_ms,
            loss_max_percent,
            cpu_max_percent: 85.0,
        }
    }

    fn direction_factors(self, variable: bool) -> (f64, f64, f64, f64) {
        match (self, variable) {
            (Self::Gaming, true) => (0.35, 0.75, 1.20, 1.60),
            (Self::Gaming, false) => (0.60, 0.82, 0.92, 1.02),
            (Self::BestOverall, true) => (0.40, 0.85, 1.25, 1.80),
            (Self::BestOverall, false) => (0.70, 0.88, 0.95, 1.05),
            (Self::Fair, true) => (0.60, 0.92, 1.30, 1.90),
            (Self::Fair, false) => (0.80, 0.94, 0.98, 1.08),
        }
    }

    fn latency_thresholds(self, jitter_ms: f64) -> Result<(u64, u64, u64), String> {
        let (adjust_up, delay_threshold, adjust_down) = match self {
            Self::Gaming => {
                let adjust_up = jitter_ms.clamp(1.0, 3.0).ceil();
                let adjust_up = checked_latency_threshold(adjust_up)?;
                // The A+ contract is a five-millisecond loaded-delay ceiling,
                // so the runtime detector may not quietly relax beyond the
                // bound that shaped validation proved.
                (adjust_up, 5, 20)
            }
            Self::BestOverall => {
                let adjust_up = (jitter_ms * 1.5).clamp(3.0, 15.0).ceil();
                let adjust_up = checked_latency_threshold(adjust_up)?;
                (
                    adjust_up,
                    (adjust_up + 8).max(15),
                    ((adjust_up + 8).max(15) + 25).max(40),
                )
            }
            Self::Fair => {
                let adjust_up = (jitter_ms * 2.0).clamp(5.0, 20.0).ceil();
                let adjust_up = checked_latency_threshold(adjust_up)?;
                (
                    adjust_up,
                    (adjust_up + 15).max(30),
                    ((adjust_up + 15).max(30) + 30).max(60),
                )
            }
        };
        Ok((adjust_up, delay_threshold, adjust_down))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SqmRecommendation {
    pub qdisc: &'static str,
    pub script: &'static str,
    pub classification: &'static str,
    pub squash_dscp: bool,
    pub squash_ingress: bool,
    pub ingress_ecn: &'static str,
    pub egress_ecn: &'static str,
    pub iqdisc_opts: &'static str,
    pub eqdisc_opts: &'static str,
}

impl SqmRecommendation {
    fn for_profile(profile: AutotuneProfile) -> Self {
        match profile {
            AutotuneProfile::Gaming => Self {
                qdisc: "cake",
                script: "layer_cake.qos",
                classification: "diffserv4",
                squash_dscp: false,
                squash_ingress: false,
                ingress_ecn: "ECN",
                egress_ecn: "NOECN",
                iqdisc_opts: "diffserv4",
                eqdisc_opts: "diffserv4",
            },
            AutotuneProfile::BestOverall | AutotuneProfile::Fair => Self {
                qdisc: "cake",
                script: "piece_of_cake.qos",
                classification: "besteffort",
                squash_dscp: true,
                squash_ingress: true,
                ingress_ecn: "ECN",
                egress_ecn: "NOECN",
                iqdisc_opts: "",
                eqdisc_opts: "",
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LatencyBaseline {
    pub median_ms: f64,
    pub p95_ms: f64,
    pub samples: usize,
}

/// Hard input/output bound shared by the proposal and validation paths.
/// 100 Tbit/s is deliberately far above current OpenWrt targets while still
/// keeping every floating-point rate calculation and integer conversion sane.
pub const MAX_RATE_KBPS: u64 = 100_000_000;
pub const MAX_THROUGHPUT_SAMPLES: usize = 1_024;
pub const MAX_BASELINE_SAMPLES: usize = 1_000_000;
pub const MAX_LATENCY_MS: f64 = 60_000.0;

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
    pub profile: AutotuneProfile,
    pub target_grade: &'static str,
    pub quality_target_required: bool,
    pub throughput_priority: bool,
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
    pub validation_thresholds: ValidationThresholds,
    pub sqm: SqmRecommendation,
    pub confidence: u64,
    pub warnings: Vec<&'static str>,
}

impl AutotuneProposal {
    pub fn revise_base_rates(&mut self, scale: f64) -> Result<(), String> {
        self.revise_base_rates_by_direction(scale, scale)
    }

    pub fn revise_base_rates_by_direction(
        &mut self,
        download_scale: f64,
        upload_scale: f64,
    ) -> Result<(), String> {
        validate_base_scale(download_scale)?;
        validate_base_scale(upload_scale)?;
        let observed_low_ceiling = if self.profile == AutotuneProfile::Fair {
            1.0
        } else {
            0.95
        };
        revise_direction_base(&mut self.download, download_scale, observed_low_ceiling);
        revise_direction_base(&mut self.upload, upload_scale, observed_low_ceiling);
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
                "{{\"schema_version\":3,\"profile\":\"{}\",\"target_grade\":\"{}\",",
                "\"quality_target_required\":{},\"throughput_priority\":{},",
                "\"download\":{},\"upload\":{},",
                "\"active_threshold_kbps\":{},",
                "\"thresholds_ms\":{{\"adjust_up\":{},\"delay\":{},\"adjust_down\":{}}},",
                "\"adaptive_ceiling\":{{\"enabled\":{},\"hold_s\":{},\"growth_percent\":{},",
                "\"probe_s\":{},\"cooldown_s\":{},\"failed_bound_ttl_s\":{}}},",
                "\"validation\":{{\"candidate_realization_min_percent\":{:.1},",
                "\"candidate_realization_max_percent\":{:.1},",
                "\"capacity_retention_min_percent\":{:.1},",
                "\"icmp_delta_max_ms\":{:.1},\"transport_delta_max_ms\":{:.1},",
                "\"loss_max_percent\":{:.1},\"cpu_max_percent\":{:.1}}},",
                "\"sqm\":{{\"qdisc\":\"{}\",\"script\":\"{}\",\"classification\":\"{}\",",
                "\"squash_dscp\":{},\"squash_ingress\":{},",
                "\"ingress_ecn\":\"{}\",\"egress_ecn\":\"{}\",",
                "\"iqdisc_opts\":\"{}\",\"eqdisc_opts\":\"{}\"}},",
                "\"link\":{{\"kind\":\"{}\",\"layer\":\"{}\",\"overhead\":{},\"mpu\":{}}},",
                "\"confidence\":{},\"warnings\":[{}]}}"
            ),
            self.profile.as_str(),
            self.target_grade,
            self.quality_target_required,
            self.throughput_priority,
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
            self.validation_thresholds.candidate_realization_min_percent,
            self.validation_thresholds.candidate_realization_max_percent,
            self.validation_thresholds.capacity_retention_min_percent,
            self.validation_thresholds.icmp_delta_max_ms,
            self.validation_thresholds.transport_delta_max_ms,
            self.validation_thresholds.loss_max_percent,
            self.validation_thresholds.cpu_max_percent,
            self.sqm.qdisc,
            self.sqm.script,
            self.sqm.classification,
            self.sqm.squash_dscp,
            self.sqm.squash_ingress,
            self.sqm.ingress_ecn,
            self.sqm.egress_ecn,
            self.sqm.iqdisc_opts,
            self.sqm.eqdisc_opts,
            self.link_kind.as_str(),
            self.link_layer,
            self.overhead,
            self.mpu,
            self.confidence,
            warnings,
        )
    }
}

fn validate_base_scale(scale: f64) -> Result<(), String> {
    if !scale.is_finite() || !(0.5..=1.2).contains(&scale) {
        return Err("base-rate revision scale must be between 0.5 and 1.2".to_string());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DirectionValidationInput {
    pub observed_low_kbps: u64,
    pub candidate_kbps: u64,
    pub achieved_kbps: u64,
    pub minimum_kbps: u64,
    pub maximum_kbps: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ValidationThresholds {
    pub candidate_realization_min_percent: f64,
    pub candidate_realization_max_percent: f64,
    pub capacity_retention_min_percent: f64,
    pub icmp_delta_max_ms: f64,
    pub transport_delta_max_ms: f64,
    pub loss_max_percent: f64,
    pub cpu_max_percent: f64,
}

impl Default for ValidationThresholds {
    fn default() -> Self {
        Self {
            candidate_realization_min_percent: 80.0,
            candidate_realization_max_percent: 110.0,
            capacity_retention_min_percent: 80.0,
            icmp_delta_max_ms: 100.0,
            transport_delta_max_ms: 100.0,
            loss_max_percent: 5.0,
            cpu_max_percent: 95.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ValidationInput {
    pub profile: AutotuneProfile,
    pub download: DirectionValidationInput,
    pub upload: DirectionValidationInput,
    pub download_load: DirectionLoadInput,
    pub upload_load: DirectionLoadInput,
    pub thresholds: ValidationThresholds,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DirectionLoadInput {
    /// A same-quantile loaded-minus-idle ICMP delta prepared by the caller.
    pub icmp_delta_ms: f64,
    /// A same-quantile loaded-minus-idle transport delta prepared by the caller.
    pub transport_delta_ms: f64,
    pub loss_percent: f64,
    pub cpu_percent: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DirectionValidationMetrics {
    pub candidate_realization_percent: f64,
    pub capacity_retention_percent: f64,
    pub candidate_capacity_percent: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValidationScope {
    Download,
    Upload,
}

impl ValidationScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Download => "download",
            Self::Upload => "upload",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateComparison {
    Minimum,
    Maximum,
}

impl GateComparison {
    fn as_str(self) -> &'static str {
        match self {
            Self::Minimum => "minimum",
            Self::Maximum => "maximum",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ValidationGate {
    pub code: &'static str,
    pub scope: ValidationScope,
    pub required: bool,
    pub pass: bool,
    pub actual: f64,
    pub limit: f64,
    pub comparison: GateComparison,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CorrectionAction {
    None,
    Increase,
    Decrease,
    Mixed,
    RetryMeasurement,
    Infeasible,
}

impl CorrectionAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Increase => "increase",
            Self::Decrease => "decrease",
            Self::Mixed => "mixed",
            Self::RetryMeasurement => "retry-measurement",
            Self::Infeasible => "infeasible",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DirectionCorrection {
    pub action: CorrectionAction,
    pub scale: f64,
    pub proposed_kbps: u64,
    pub required_floor_kbps: u64,
    pub predicted_capacity_retention_percent: f64,
    pub reason: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ValidationCorrection {
    pub action: CorrectionAction,
    pub feasible: bool,
    pub reason: &'static str,
    pub download: DirectionCorrection,
    pub upload: DirectionCorrection,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ValidationResult {
    pub profile: AutotuneProfile,
    pub pass: bool,
    pub hard_pass: bool,
    pub quality_target_met: bool,
    pub actual_grade: &'static str,
    pub score: f64,
    pub effective_delta_ms: f64,
    pub download: DirectionValidationMetrics,
    pub upload: DirectionValidationMetrics,
    pub download_load: DirectionLoadInput,
    pub upload_load: DirectionLoadInput,
    pub gates: Vec<ValidationGate>,
    pub correction: ValidationCorrection,
}

impl ValidationResult {
    pub fn reasons(&self) -> impl Iterator<Item = &ValidationGate> {
        self.gates.iter().filter(|gate| !gate.pass)
    }

    pub fn to_json(&self) -> String {
        let gates = self
            .gates
            .iter()
            .map(gate_json)
            .collect::<Vec<_>>()
            .join(",");
        let reasons = self.reasons().map(gate_json).collect::<Vec<_>>().join(",");
        format!(
            concat!(
                "{{\"schema_version\":3,\"profile\":\"{}\",\"pass\":{},",
                "\"hard_pass\":{},\"quality_target_met\":{},\"actual_grade\":\"{}\",",
                "\"score\":{:.1},",
                "\"metrics\":{{\"download\":{},\"upload\":{},\"effective_delta_ms\":{:.3}}},",
                "\"signals\":{{\"download\":{},\"upload\":{}}},",
                "\"gates\":[{}],\"reasons\":[{}],\"correction\":{}}}"
            ),
            self.profile.as_str(),
            self.pass,
            self.hard_pass,
            self.quality_target_met,
            self.actual_grade,
            self.score,
            validation_metrics_json(self.download),
            validation_metrics_json(self.upload),
            self.effective_delta_ms,
            direction_load_json(self.download_load),
            direction_load_json(self.upload_load),
            gates,
            reasons,
            validation_correction_json(self.correction),
        )
    }
}

pub fn validate_shaped_candidate(input: ValidationInput) -> Result<ValidationResult, String> {
    validate_validation_input(&input)?;
    let download = validation_metrics(input.download);
    let upload = validation_metrics(input.upload);
    let thresholds = input.thresholds;
    let mut gates = vec![
        minimum_gate(
            "download-candidate-realization",
            ValidationScope::Download,
            download.candidate_realization_percent,
            thresholds.candidate_realization_min_percent,
        ),
        minimum_gate(
            "upload-candidate-realization",
            ValidationScope::Upload,
            upload.candidate_realization_percent,
            thresholds.candidate_realization_min_percent,
        ),
        maximum_gate(
            "download-candidate-realization-maximum",
            ValidationScope::Download,
            download.candidate_realization_percent,
            thresholds.candidate_realization_max_percent,
        ),
        maximum_gate(
            "upload-candidate-realization-maximum",
            ValidationScope::Upload,
            upload.candidate_realization_percent,
            thresholds.candidate_realization_max_percent,
        ),
        minimum_gate(
            "download-capacity-retention",
            ValidationScope::Download,
            download.capacity_retention_percent,
            thresholds.capacity_retention_min_percent,
        ),
        minimum_gate(
            "upload-capacity-retention",
            ValidationScope::Upload,
            upload.capacity_retention_percent,
            thresholds.capacity_retention_min_percent,
        ),
        maximum_gate(
            "download-icmp-latency",
            ValidationScope::Download,
            input.download_load.icmp_delta_ms,
            thresholds.icmp_delta_max_ms,
        ),
        maximum_gate(
            "download-transport-latency",
            ValidationScope::Download,
            input.download_load.transport_delta_ms,
            thresholds.transport_delta_max_ms,
        ),
        maximum_gate(
            "download-packet-loss",
            ValidationScope::Download,
            input.download_load.loss_percent,
            thresholds.loss_max_percent,
        ),
        maximum_gate(
            "download-cpu",
            ValidationScope::Download,
            input.download_load.cpu_percent,
            thresholds.cpu_max_percent,
        ),
        maximum_gate(
            "upload-icmp-latency",
            ValidationScope::Upload,
            input.upload_load.icmp_delta_ms,
            thresholds.icmp_delta_max_ms,
        ),
        maximum_gate(
            "upload-transport-latency",
            ValidationScope::Upload,
            input.upload_load.transport_delta_ms,
            thresholds.transport_delta_max_ms,
        ),
        maximum_gate(
            "upload-packet-loss",
            ValidationScope::Upload,
            input.upload_load.loss_percent,
            thresholds.loss_max_percent,
        ),
        maximum_gate(
            "upload-cpu",
            ValidationScope::Upload,
            input.upload_load.cpu_percent,
            thresholds.cpu_max_percent,
        ),
    ];
    if !input.profile.quality_target_required() {
        for gate in &mut gates {
            if matches!(
                gate.code,
                "download-icmp-latency"
                    | "download-transport-latency"
                    | "upload-icmp-latency"
                    | "upload-transport-latency"
            ) {
                gate.required = false;
            }
        }
    }
    let pass = gates.iter().all(|gate| gate.pass);
    let hard_pass = gates
        .iter()
        .filter(|gate| gate.required)
        .all(|gate| gate.pass);
    let quality_target_met = gates
        .iter()
        .filter(|gate| gate.code.contains("latency"))
        .all(|gate| gate.pass);
    let effective_delta_ms = input
        .download_load
        .icmp_delta_ms
        .max(input.download_load.transport_delta_ms)
        .max(input.upload_load.icmp_delta_ms)
        .max(input.upload_load.transport_delta_ms);
    let actual_grade = classify_quality(Some(effective_delta_ms)).as_str();
    let score = validation_score(&gates);
    let correction = validation_correction(&input, download, upload, &gates, pass);

    Ok(ValidationResult {
        profile: input.profile,
        pass,
        hard_pass,
        quality_target_met,
        actual_grade,
        score,
        effective_delta_ms,
        download,
        upload,
        download_load: input.download_load,
        upload_load: input.upload_load,
        gates,
        correction,
    })
}

fn validate_validation_input(input: &ValidationInput) -> Result<(), String> {
    validate_direction_validation_input("download", input.download)?;
    validate_direction_validation_input("upload", input.upload)?;
    validate_direction_load_input("download", input.download_load)?;
    validate_direction_load_input("upload", input.upload_load)?;

    let thresholds = input.thresholds;
    for (name, value) in [
        (
            "candidate realization minimum",
            thresholds.candidate_realization_min_percent,
        ),
        (
            "capacity retention minimum",
            thresholds.capacity_retention_min_percent,
        ),
        ("loss maximum", thresholds.loss_max_percent),
        ("CPU maximum", thresholds.cpu_max_percent),
    ] {
        if !value.is_finite() || !(0.0..=100.0).contains(&value) {
            return Err(format!("{name} must be between 0 and 100"));
        }
    }
    if !thresholds.candidate_realization_max_percent.is_finite()
        || !(100.0..=200.0).contains(&thresholds.candidate_realization_max_percent)
    {
        return Err("candidate realization maximum must be between 100 and 200".to_string());
    }
    if thresholds.candidate_realization_max_percent < thresholds.candidate_realization_min_percent {
        return Err("candidate realization maximum must not be below its minimum".to_string());
    }
    for (name, value) in [
        ("ICMP delta maximum", thresholds.icmp_delta_max_ms),
        ("transport delta maximum", thresholds.transport_delta_max_ms),
    ] {
        if !value.is_finite() || !(0.0..=MAX_LATENCY_MS).contains(&value) {
            return Err(format!(
                "{name} must be a finite number between 0 and {MAX_LATENCY_MS}"
            ));
        }
    }
    Ok(())
}

fn validate_direction_load_input(name: &str, input: DirectionLoadInput) -> Result<(), String> {
    for (metric, value) in [
        ("ICMP delta", input.icmp_delta_ms),
        ("transport delta", input.transport_delta_ms),
        ("loss percent", input.loss_percent),
        ("CPU percent", input.cpu_percent),
    ] {
        let upper = if matches!(metric, "loss percent" | "CPU percent") {
            100.0
        } else {
            MAX_LATENCY_MS
        };
        if !value.is_finite() || !(0.0..=upper).contains(&value) {
            return Err(format!(
                "{name} {metric} must be finite and between 0 and {upper}"
            ));
        }
    }
    Ok(())
}

fn validate_direction_validation_input(
    name: &str,
    input: DirectionValidationInput,
) -> Result<(), String> {
    if input.observed_low_kbps == 0 || input.candidate_kbps == 0 || input.achieved_kbps == 0 {
        return Err(format!(
            "{name} observed, candidate, and achieved rates must be positive"
        ));
    }
    for (rate_name, value) in [
        ("observed", input.observed_low_kbps),
        ("candidate", input.candidate_kbps),
        ("achieved", input.achieved_kbps),
        ("minimum", input.minimum_kbps),
        ("maximum", input.maximum_kbps),
    ] {
        if value > MAX_RATE_KBPS {
            return Err(format!(
                "{name} {rate_name} rate must not exceed {MAX_RATE_KBPS} kbit/s"
            ));
        }
    }
    if input.minimum_kbps == 0
        || input.minimum_kbps > input.candidate_kbps
        || input.candidate_kbps > input.maximum_kbps
    {
        return Err(format!(
            "{name} validation rates must be ordered minimum <= candidate <= maximum"
        ));
    }
    Ok(())
}

fn validation_metrics(input: DirectionValidationInput) -> DirectionValidationMetrics {
    DirectionValidationMetrics {
        candidate_realization_percent: input.achieved_kbps as f64 * 100.0
            / input.candidate_kbps as f64,
        capacity_retention_percent: input.achieved_kbps as f64 * 100.0
            / input.observed_low_kbps as f64,
        candidate_capacity_percent: input.candidate_kbps as f64 * 100.0
            / input.observed_low_kbps as f64,
    }
}

fn minimum_gate(
    code: &'static str,
    scope: ValidationScope,
    actual: f64,
    limit: f64,
) -> ValidationGate {
    ValidationGate {
        code,
        scope,
        required: true,
        pass: actual >= limit,
        actual,
        limit,
        comparison: GateComparison::Minimum,
    }
}

fn maximum_gate(
    code: &'static str,
    scope: ValidationScope,
    actual: f64,
    limit: f64,
) -> ValidationGate {
    ValidationGate {
        code,
        scope,
        required: true,
        pass: actual <= limit,
        actual,
        limit,
        comparison: GateComparison::Maximum,
    }
}

fn validation_score(gates: &[ValidationGate]) -> f64 {
    gates
        .iter()
        .map(|gate| match gate.comparison {
            GateComparison::Minimum => {
                if gate.limit <= 0.0 {
                    100.0
                } else {
                    gate.actual * 100.0 / gate.limit
                }
            }
            GateComparison::Maximum => {
                if gate.actual <= gate.limit || gate.actual <= 0.0 {
                    100.0
                } else if gate.limit <= 0.0 {
                    0.0
                } else {
                    gate.limit * 100.0 / gate.actual
                }
            }
        })
        .fold(100.0_f64, f64::min)
        .clamp(0.0, 100.0)
}

fn validation_correction(
    input: &ValidationInput,
    download: DirectionValidationMetrics,
    upload: DirectionValidationMetrics,
    gates: &[ValidationGate],
    pass: bool,
) -> ValidationCorrection {
    let observed_low_ceiling = if input.profile == AutotuneProfile::Fair {
        1.0
    } else {
        0.95
    };
    if pass {
        return ValidationCorrection {
            action: CorrectionAction::None,
            feasible: true,
            reason: "candidate-passed",
            download: hold_direction_correction(
                input.download,
                download,
                input.thresholds.capacity_retention_min_percent,
            ),
            upload: hold_direction_correction(
                input.upload,
                upload,
                input.thresholds.capacity_retention_min_percent,
            ),
        };
    }

    let download_correction = direction_validation_correction(
        input.download,
        download,
        input.thresholds.capacity_retention_min_percent,
        gates,
        "download-candidate-realization",
        "download-candidate-realization-maximum",
        "download-capacity-retention",
        observed_low_ceiling,
        &[
            "download-icmp-latency",
            "download-transport-latency",
            "download-packet-loss",
            "download-cpu",
        ],
    );
    let upload_correction = direction_validation_correction(
        input.upload,
        upload,
        input.thresholds.capacity_retention_min_percent,
        gates,
        "upload-candidate-realization",
        "upload-candidate-realization-maximum",
        "upload-capacity-retention",
        observed_low_ceiling,
        &[
            "upload-icmp-latency",
            "upload-transport-latency",
            "upload-packet-loss",
            "upload-cpu",
        ],
    );

    if download_correction.action == CorrectionAction::RetryMeasurement
        || upload_correction.action == CorrectionAction::RetryMeasurement
    {
        let realization_too_high = [download_correction, upload_correction]
            .iter()
            .any(|correction| correction.reason == "candidate-realization-too-high");
        return ValidationCorrection {
            action: CorrectionAction::RetryMeasurement,
            feasible: false,
            reason: if realization_too_high {
                "candidate-realization-too-high"
            } else {
                "candidate-realization-too-low"
            },
            download: download_correction,
            upload: upload_correction,
        };
    }

    if download_correction.action == CorrectionAction::Infeasible
        || upload_correction.action == CorrectionAction::Infeasible
    {
        let infeasible = [download_correction, upload_correction];
        let adverse_infeasible = infeasible.iter().any(|correction| {
            correction.action == CorrectionAction::Infeasible
                && correction.reason == "capacity-floor-leaves-no-room-to-decrease"
        });
        let maximum_infeasible = infeasible.iter().any(|correction| {
            correction.action == CorrectionAction::Infeasible
                && correction.reason == "maximum-rate-below-required-floor"
        });
        return ValidationCorrection {
            action: CorrectionAction::Infeasible,
            feasible: false,
            reason: if adverse_infeasible {
                "safety-floor-blocks-rate-reduction"
            } else if maximum_infeasible {
                "maximum-rate-cannot-reach-safety-floor"
            } else {
                "bounded-correction-cannot-reach-safety-floor"
            },
            download: download_correction,
            upload: upload_correction,
        };
    }

    let action = correction_action_for_directions(download_correction, upload_correction);

    ValidationCorrection {
        action,
        feasible: true,
        reason: match action {
            CorrectionAction::Increase => "clean-link-capacity-correction",
            CorrectionAction::Decrease => "adverse-loaded-signal",
            CorrectionAction::Mixed => "direction-specific-mixed-correction",
            CorrectionAction::None => "no-correction-required",
            _ => "direction-specific-correction",
        },
        download: download_correction,
        upload: upload_correction,
    }
}

#[allow(clippy::too_many_arguments)]
fn direction_validation_correction(
    input: DirectionValidationInput,
    metrics: DirectionValidationMetrics,
    floor_percent: f64,
    gates: &[ValidationGate],
    realization_min_gate: &str,
    realization_max_gate: &str,
    retention_gate: &str,
    observed_low_ceiling: f64,
    adverse_gates: &[&str],
) -> DirectionCorrection {
    let hold = hold_direction_correction(input, metrics, floor_percent);
    if !gate_pass(gates, realization_min_gate) {
        return retry_direction_correction(hold, "candidate-realization-too-low");
    }
    if !gate_pass(gates, realization_max_gate) {
        return retry_direction_correction(hold, "candidate-realization-too-high");
    }
    if adverse_gates.iter().any(|code| !gate_pass(gates, code)) {
        return decrease_direction_correction(input, metrics, floor_percent);
    }
    if !gate_pass(gates, retention_gate) {
        return increase_direction_correction(input, metrics, floor_percent, observed_low_ceiling);
    }
    hold
}

fn gate_pass(gates: &[ValidationGate], code: &str) -> bool {
    gates
        .iter()
        .find(|gate| gate.code == code)
        .map(|gate| gate.pass)
        .unwrap_or(false)
}

fn required_candidate_for_floor(
    input: DirectionValidationInput,
    metrics: DirectionValidationMetrics,
    floor_percent: f64,
) -> u64 {
    let realization = metrics.candidate_realization_percent / 100.0;
    rounded_rate_up(input.observed_low_kbps as f64 * (floor_percent / 100.0) / realization)
}

fn predicted_capacity_retention(
    input: DirectionValidationInput,
    metrics: DirectionValidationMetrics,
    proposed_kbps: u64,
) -> f64 {
    metrics.capacity_retention_percent * proposed_kbps as f64 / input.candidate_kbps as f64
}

fn hold_direction_correction(
    input: DirectionValidationInput,
    metrics: DirectionValidationMetrics,
    floor_percent: f64,
) -> DirectionCorrection {
    DirectionCorrection {
        action: CorrectionAction::None,
        scale: 1.0,
        proposed_kbps: input.candidate_kbps,
        required_floor_kbps: required_candidate_for_floor(input, metrics, floor_percent),
        predicted_capacity_retention_percent: metrics.capacity_retention_percent,
        reason: "hold",
    }
}

fn retry_direction_correction(
    mut correction: DirectionCorrection,
    reason: &'static str,
) -> DirectionCorrection {
    correction.action = CorrectionAction::RetryMeasurement;
    correction.reason = reason;
    correction
}

fn infeasible_direction_correction(
    input: DirectionValidationInput,
    metrics: DirectionValidationMetrics,
    required_floor_kbps: u64,
    reason: &'static str,
) -> DirectionCorrection {
    DirectionCorrection {
        action: CorrectionAction::Infeasible,
        scale: 1.0,
        proposed_kbps: input.candidate_kbps,
        required_floor_kbps,
        predicted_capacity_retention_percent: metrics.capacity_retention_percent,
        reason,
    }
}

fn decrease_direction_correction(
    input: DirectionValidationInput,
    metrics: DirectionValidationMetrics,
    floor_percent: f64,
) -> DirectionCorrection {
    let required_floor_kbps =
        required_candidate_for_floor(input, metrics, floor_percent).max(input.minimum_kbps);
    if required_floor_kbps >= input.candidate_kbps {
        return infeasible_direction_correction(
            input,
            metrics,
            required_floor_kbps,
            "capacity-floor-leaves-no-room-to-decrease",
        );
    }
    let desired_kbps = rounded_rate(input.candidate_kbps as f64 * 0.95)
        .max(input.minimum_kbps)
        .max(required_floor_kbps);
    if desired_kbps >= input.candidate_kbps {
        return infeasible_direction_correction(
            input,
            metrics,
            required_floor_kbps,
            "bounded-decrease-rounds-to-current-rate",
        );
    }
    DirectionCorrection {
        action: CorrectionAction::Decrease,
        scale: desired_kbps as f64 / input.candidate_kbps as f64,
        proposed_kbps: desired_kbps,
        required_floor_kbps,
        predicted_capacity_retention_percent: predicted_capacity_retention(
            input,
            metrics,
            desired_kbps,
        ),
        reason: "reduce-adverse-loaded-signal",
    }
}

fn increase_direction_correction(
    input: DirectionValidationInput,
    metrics: DirectionValidationMetrics,
    floor_percent: f64,
    observed_low_ceiling: f64,
) -> DirectionCorrection {
    let required_floor_kbps =
        required_candidate_for_floor(input, metrics, floor_percent).max(input.minimum_kbps);
    // A hard 95%-of-observed ceiling made a 90% retained-throughput floor
    // mathematically unreachable whenever the shaped realization was below
    // about 94.74%.  The observed-low sample itself remains the outer safety
    // bound; the candidate, configured maximum, and one-step 20% bound still
    // prevent an unbounded correction.
    let revision_upper = rounded_rate(input.observed_low_kbps as f64 * observed_low_ceiling)
        .min(input.maximum_kbps)
        .min(rounded_rate(input.candidate_kbps as f64 * 1.20))
        .max(input.minimum_kbps);
    if required_floor_kbps > revision_upper {
        let reason = if required_floor_kbps > input.maximum_kbps {
            "maximum-rate-below-required-floor"
        } else {
            "bounded-increase-below-required-floor"
        };
        return infeasible_direction_correction(input, metrics, required_floor_kbps, reason);
    }
    let desired_kbps = rounded_rate(input.candidate_kbps as f64 * 1.05)
        .max(required_floor_kbps)
        .min(revision_upper);
    if desired_kbps <= input.candidate_kbps {
        return infeasible_direction_correction(
            input,
            metrics,
            required_floor_kbps,
            "bounded-increase-cannot-reach-required-floor",
        );
    }
    DirectionCorrection {
        action: CorrectionAction::Increase,
        scale: desired_kbps as f64 / input.candidate_kbps as f64,
        proposed_kbps: desired_kbps,
        required_floor_kbps,
        predicted_capacity_retention_percent: predicted_capacity_retention(
            input,
            metrics,
            desired_kbps,
        ),
        reason: "increase-clean-link-capacity",
    }
}

fn correction_action_for_directions(
    download: DirectionCorrection,
    upload: DirectionCorrection,
) -> CorrectionAction {
    match (download.action, upload.action) {
        (CorrectionAction::None, CorrectionAction::None) => CorrectionAction::None,
        (CorrectionAction::Increase, CorrectionAction::None)
        | (CorrectionAction::None, CorrectionAction::Increase)
        | (CorrectionAction::Increase, CorrectionAction::Increase) => CorrectionAction::Increase,
        (CorrectionAction::Decrease, CorrectionAction::None)
        | (CorrectionAction::None, CorrectionAction::Decrease)
        | (CorrectionAction::Decrease, CorrectionAction::Decrease) => CorrectionAction::Decrease,
        _ => CorrectionAction::Mixed,
    }
}

fn rounded_rate_up(rate_kbps: f64) -> u64 {
    let rounded = (rate_kbps.max(100.0) / 100.0).ceil() * 100.0;
    if !rounded.is_finite() || rounded >= MAX_RATE_KBPS as f64 {
        return MAX_RATE_KBPS;
    }
    debug_assert!(rounded >= 0.0 && rounded <= u64::MAX as f64);
    rounded as u64
}

fn validation_metrics_json(metrics: DirectionValidationMetrics) -> String {
    format!(
        concat!(
            "{{\"candidate_realization_percent\":{:.3},",
            "\"capacity_retention_percent\":{:.3},",
            "\"candidate_capacity_percent\":{:.3}}}"
        ),
        metrics.candidate_realization_percent,
        metrics.capacity_retention_percent,
        metrics.candidate_capacity_percent,
    )
}

fn direction_load_json(load: DirectionLoadInput) -> String {
    format!(
        concat!(
            "{{\"icmp_delta_ms\":{:.3},\"transport_delta_ms\":{:.3},",
            "\"loss_percent\":{:.3},\"cpu_percent\":{:.3}}}"
        ),
        load.icmp_delta_ms, load.transport_delta_ms, load.loss_percent, load.cpu_percent,
    )
}

fn gate_json(gate: &ValidationGate) -> String {
    format!(
        "{{\"code\":\"{}\",\"scope\":\"{}\",\"required\":{},\"pass\":{},\"actual\":{:.3},\"limit\":{:.3},\"comparison\":\"{}\"}}",
        gate.code,
        gate.scope.as_str(),
        gate.required,
        gate.pass,
        gate.actual,
        gate.limit,
        gate.comparison.as_str(),
    )
}

fn direction_correction_json(correction: DirectionCorrection) -> String {
    format!(
        concat!(
            "{{\"action\":\"{}\",\"scale\":{:.6},\"proposed_kbps\":{},",
            "\"required_floor_kbps\":{},\"predicted_capacity_retention_percent\":{:.3},",
            "\"reason\":\"{}\"}}"
        ),
        correction.action.as_str(),
        correction.scale,
        correction.proposed_kbps,
        correction.required_floor_kbps,
        correction.predicted_capacity_retention_percent,
        correction.reason,
    )
}

fn validation_correction_json(correction: ValidationCorrection) -> String {
    format!(
        concat!(
            "{{\"action\":\"{}\",\"feasible\":{},\"reason\":\"{}\",",
            "\"download\":{},\"upload\":{}}}"
        ),
        correction.action.as_str(),
        correction.feasible,
        correction.reason,
        direction_correction_json(correction.download),
        direction_correction_json(correction.upload),
    )
}

fn checked_latency_threshold(value_ms: f64) -> Result<u64, String> {
    if !value_ms.is_finite() || !(0.0..=MAX_LATENCY_MS).contains(&value_ms) {
        return Err("calculated latency threshold is out of range".to_string());
    }
    Ok(value_ms as u64)
}

#[cfg(test)]
pub fn build_proposal(
    download_samples_kbps: &[f64],
    upload_samples_kbps: &[f64],
    baseline: LatencyBaseline,
    link_kind: LinkKind,
) -> Result<AutotuneProposal, String> {
    build_proposal_for_profile(
        download_samples_kbps,
        upload_samples_kbps,
        baseline,
        link_kind,
        AutotuneProfile::BestOverall,
    )
}

pub fn build_proposal_for_profile(
    download_samples_kbps: &[f64],
    upload_samples_kbps: &[f64],
    baseline: LatencyBaseline,
    link_kind: LinkKind,
    profile: AutotuneProfile,
) -> Result<AutotuneProposal, String> {
    validate_throughput_samples("download", download_samples_kbps)?;
    validate_throughput_samples("upload", upload_samples_kbps)?;
    validate_latency_baseline(baseline)?;
    let download = propose_direction(download_samples_kbps, profile)?;
    let upload = propose_direction(upload_samples_kbps, profile)?;
    let variable = download.variability >= 0.15 || upload.variability >= 0.15;
    let jitter_ms = (baseline.p95_ms - baseline.median_ms).max(0.0);
    let (adjust_up_threshold_ms, delay_threshold_ms, adjust_down_threshold_ms) =
        profile.latency_thresholds(jitter_ms)?;
    // Activity detection must stay well below the weakest observed direction.
    // Using a percentage of the proposed minimum is too high when one
    // direction looked stable during a short, otherwise variable calibration.
    let smallest_observed = download.observed_low_kbps.min(upload.observed_low_kbps);
    let active_threshold_kbps =
        checked_rounded_rate(smallest_observed as f64 / 10.0)?.clamp(500, 20_000);
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
    if profile == AutotuneProfile::Gaming {
        warnings.push(
            "Gaming diffserv4 prioritizes only traffic already marked with DSCP; CAKE Autorate does not guess applications or rewrite client policy.",
        );
        warnings.push(
            "Gaming preserves ingress DSCP. Use Best overall when upstream markings are not trusted.",
        );
    }
    if profile == AutotuneProfile::Fair {
        warnings.push(
            "Fair prioritizes sustained throughput with a 90% safety floor. Class C is a conditional goal; if the link cannot reach it without crossing that floor, the measured grade is reported instead of destroying throughput.",
        );
        warnings.push(
            "When a validated no-SQM control is no worse than the best shaped candidate, Review may recommend disabling SQM. That choice is never applied automatically.",
        );
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
        profile,
        target_grade: profile.target_grade(),
        quality_target_required: profile.quality_target_required(),
        throughput_priority: profile.throughput_priority(),
        download,
        upload,
        active_threshold_kbps,
        adjust_up_threshold_ms,
        delay_threshold_ms,
        adjust_down_threshold_ms,
        adaptive_ceiling_enabled: variable,
        adaptive_hold_s: match profile {
            AutotuneProfile::Gaming => 30,
            AutotuneProfile::BestOverall => {
                if variable {
                    15
                } else {
                    20
                }
            }
            AutotuneProfile::Fair => 10,
        },
        adaptive_growth_percent: match profile {
            AutotuneProfile::Gaming => 1,
            AutotuneProfile::BestOverall => 3,
            AutotuneProfile::Fair => 5,
        },
        adaptive_probe_s: match profile {
            AutotuneProfile::Fair => 10,
            AutotuneProfile::Gaming | AutotuneProfile::BestOverall => 8,
        },
        adaptive_cooldown_s: match profile {
            AutotuneProfile::Gaming => 90,
            AutotuneProfile::BestOverall => {
                if variable {
                    45
                } else {
                    60
                }
            }
            AutotuneProfile::Fair => 30,
        },
        adaptive_failed_bound_ttl_s: match profile {
            AutotuneProfile::Gaming => 1800,
            AutotuneProfile::BestOverall => {
                if variable {
                    900
                } else {
                    1800
                }
            }
            AutotuneProfile::Fair => 600,
        },
        link_kind,
        link_layer,
        overhead,
        mpu,
        validation_thresholds: profile.validation_thresholds(),
        sqm: SqmRecommendation::for_profile(profile),
        confidence: (sample_confidence + latency_confidence + link_confidence).min(100),
        warnings,
    })
}

fn propose_direction(
    samples_kbps: &[f64],
    profile: AutotuneProfile,
) -> Result<DirectionProposal, String> {
    let mut samples = samples_kbps.to_vec();
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

    let (minimum_factor, base_factor, maximum_factor, cap_factor) =
        profile.direction_factors(variable);
    let (minimum, base, maximum, cap) = (
        low * minimum_factor,
        low * base_factor,
        high * maximum_factor,
        high * cap_factor,
    );
    let minimum = checked_rounded_rate(minimum)?;
    let base = checked_rounded_rate(base)?.max(minimum);
    let maximum = checked_rounded_rate(maximum)?.max(base);
    let absolute_cap = checked_rounded_rate(cap)?.max(maximum);

    Ok(DirectionProposal {
        minimum_kbps: minimum,
        base_kbps: base,
        maximum_kbps: maximum,
        absolute_cap_kbps: absolute_cap,
        observed_low_kbps: checked_rounded_rate(low)?,
        observed_median_kbps: checked_rounded_rate(median)?,
        observed_high_kbps: checked_rounded_rate(high)?,
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

fn validate_throughput_samples(name: &str, samples: &[f64]) -> Result<(), String> {
    if samples.is_empty() {
        return Err(format!("at least one {name} throughput sample is required"));
    }
    if samples.len() > MAX_THROUGHPUT_SAMPLES {
        return Err(format!(
            "{name} throughput sample count must not exceed {MAX_THROUGHPUT_SAMPLES}"
        ));
    }
    for (index, sample) in samples.iter().copied().enumerate() {
        if !sample.is_finite() || sample <= 0.0 || sample > MAX_RATE_KBPS as f64 {
            return Err(format!(
                "{name} throughput sample {} must be finite and between 0 and {MAX_RATE_KBPS} kbit/s",
                index + 1
            ));
        }
    }
    Ok(())
}

fn validate_latency_baseline(baseline: LatencyBaseline) -> Result<(), String> {
    if !baseline.median_ms.is_finite()
        || baseline.median_ms <= 0.0
        || baseline.median_ms > MAX_LATENCY_MS
    {
        return Err(format!(
            "idle median must be finite and between 0 and {MAX_LATENCY_MS} ms"
        ));
    }
    if !baseline.p95_ms.is_finite()
        || baseline.p95_ms < baseline.median_ms
        || baseline.p95_ms > MAX_LATENCY_MS
    {
        return Err(format!(
            "idle p95 must be finite, at least the median, and at most {MAX_LATENCY_MS} ms"
        ));
    }
    if baseline.samples == 0 || baseline.samples > MAX_BASELINE_SAMPLES {
        return Err(format!(
            "idle sample count must be between 1 and {MAX_BASELINE_SAMPLES}"
        ));
    }
    Ok(())
}

fn checked_rounded_rate(rate_kbps: f64) -> Result<u64, String> {
    if !rate_kbps.is_finite() || rate_kbps <= 0.0 {
        return Err("calculated rate must be a finite positive number".to_string());
    }
    let rounded = (rate_kbps.max(100.0) / 100.0).round() * 100.0;
    if !rounded.is_finite() {
        return Err("calculated rate is not finite".to_string());
    }
    if rounded > MAX_RATE_KBPS as f64 {
        // Proposal multipliers may exceed the global bound near its edge. A
        // bounded cap is safer than Rust's saturating float-to-int cast.
        return Ok(MAX_RATE_KBPS);
    }
    if rounded < 0.0 || rounded > u64::MAX as f64 {
        return Err("calculated rate is outside the supported integer range".to_string());
    }
    Ok(rounded as u64)
}

fn rounded_rate(rate_kbps: f64) -> u64 {
    checked_rounded_rate(rate_kbps)
        .expect("rate derived from previously validated bounded integer input")
}

fn revise_direction_base(direction: &mut DirectionProposal, scale: f64, observed_low_ceiling: f64) {
    let upper = rounded_rate(direction.observed_low_kbps as f64 * observed_low_ceiling)
        .min(direction.maximum_kbps);
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
    fn profile_names_are_strict_with_a_balanced_compatibility_alias() {
        assert_eq!(
            AutotuneProfile::parse("gaming"),
            Some(AutotuneProfile::Gaming)
        );
        assert_eq!(
            AutotuneProfile::parse("best_overall"),
            Some(AutotuneProfile::BestOverall)
        );
        assert_eq!(
            AutotuneProfile::parse("balanced"),
            Some(AutotuneProfile::BestOverall)
        );
        assert_eq!(AutotuneProfile::parse("fair"), Some(AutotuneProfile::Fair));
        assert_eq!(AutotuneProfile::parse("Gaming"), None);
        assert_eq!(AutotuneProfile::parse("throughput"), None);
    }

    #[test]
    fn profiles_trade_latency_headroom_for_bounded_capacity() {
        let build = |profile| {
            build_proposal_for_profile(
                &[100_000.0, 101_000.0, 99_000.0],
                &[50_000.0, 51_000.0, 49_000.0],
                LatencyBaseline {
                    median_ms: 5.0,
                    p95_ms: 7.0,
                    samples: 20,
                },
                LinkKind::Ethernet,
                profile,
            )
            .unwrap()
        };
        let gaming = build(AutotuneProfile::Gaming);
        let best = build(AutotuneProfile::BestOverall);
        let fair = build(AutotuneProfile::Fair);

        assert!(gaming.download.base_kbps < best.download.base_kbps);
        assert!(best.download.base_kbps < fair.download.base_kbps);
        assert_eq!(
            gaming.validation_thresholds.capacity_retention_min_percent,
            70.0
        );
        assert_eq!(
            best.validation_thresholds.capacity_retention_min_percent,
            80.0
        );
        assert_eq!(
            fair.validation_thresholds.capacity_retention_min_percent,
            90.0
        );
        assert_eq!(gaming.validation_thresholds.transport_delta_max_ms, 5.0);
        assert_eq!(best.validation_thresholds.transport_delta_max_ms, 30.0);
        assert_eq!(fair.validation_thresholds.transport_delta_max_ms, 200.0);
        assert_eq!(fair.target_grade, "C");
        assert!(!fair.quality_target_required);
        assert!(fair.throughput_priority);
        assert!(gaming.adjust_up_threshold_ms <= best.adjust_up_threshold_ms);
        assert!(best.adjust_up_threshold_ms <= fair.adjust_up_threshold_ms);
        assert_eq!(gaming.delay_threshold_ms, 5);
        assert!(gaming.adjust_up_threshold_ms <= gaming.delay_threshold_ms);
    }

    #[test]
    fn gaming_profile_emits_explicit_diffserv4_without_application_guessing() {
        let proposal = build_proposal_for_profile(
            &[100_000.0, 101_000.0],
            &[50_000.0, 51_000.0],
            LatencyBaseline {
                median_ms: 5.0,
                p95_ms: 7.0,
                samples: 20,
            },
            LinkKind::Ethernet,
            AutotuneProfile::Gaming,
        )
        .unwrap();
        let json = proposal.to_json();

        assert_eq!(proposal.sqm.script, "layer_cake.qos");
        assert_eq!(proposal.sqm.classification, "diffserv4");
        assert!(!proposal.sqm.squash_dscp);
        assert!(!proposal.sqm.squash_ingress);
        assert_eq!(proposal.sqm.iqdisc_opts, "diffserv4");
        assert_eq!(proposal.sqm.eqdisc_opts, "diffserv4");
        assert!(json.contains("\"schema_version\":3"));
        assert!(json.contains("\"profile\":\"gaming\""));
        assert!(json.contains("\"target_grade\":\"A+\""));
        assert!(json.contains("\"script\":\"layer_cake.qos\""));
        assert!(proposal
            .warnings
            .iter()
            .any(|warning| warning.contains("does not guess applications")));
    }

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

        assert!(json.contains("\"schema_version\":3"));
        assert!(json.contains("\"profile\":\"best_overall\""));
        assert!(json.contains("\"minimum_kbps\""));
        assert!(json.contains("\"adaptive_ceiling\""));
        assert!(json.contains("\"classification\":\"besteffort\""));
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

    fn validation_input(
        download: DirectionValidationInput,
        upload: DirectionValidationInput,
    ) -> ValidationInput {
        let load = DirectionLoadInput {
            icmp_delta_ms: 0.5,
            transport_delta_ms: 60.0,
            loss_percent: 0.0,
            cpu_percent: 53.2,
        };
        ValidationInput {
            profile: AutotuneProfile::BestOverall,
            download,
            upload,
            download_load: load,
            upload_load: load,
            thresholds: ValidationThresholds::default(),
        }
    }

    #[test]
    fn validation_separates_candidate_realization_from_capacity_retention() {
        let result = validate_shaped_candidate(validation_input(
            DirectionValidationInput {
                observed_low_kbps: 883_500,
                candidate_kbps: 738_500,
                achieved_kbps: 683_153,
                minimum_kbps: 618_400,
                maximum_kbps: 840_100,
            },
            DirectionValidationInput {
                observed_low_kbps: 903_800,
                candidate_kbps: 755_500,
                achieved_kbps: 698_955,
                minimum_kbps: 632_600,
                maximum_kbps: 859_700,
            },
        ))
        .unwrap();

        assert!((result.download.candidate_realization_percent - 92.505).abs() < 0.01);
        assert!((result.download.capacity_retention_percent - 77.323).abs() < 0.01);
        assert!(gate_pass(&result.gates, "download-candidate-realization"));
        assert!(!gate_pass(&result.gates, "download-capacity-retention"));
        assert_eq!(result.correction.action, CorrectionAction::Increase);
        assert!(result.correction.feasible);
        assert!(
            result
                .correction
                .download
                .predicted_capacity_retention_percent
                >= 80.0
        );
    }

    #[test]
    fn clean_rc16_first_attempt_increases_only_the_failed_download_direction() {
        let mut input = validation_input(
            DirectionValidationInput {
                observed_low_kbps: 883_500,
                candidate_kbps: 777_400,
                achieved_kbps: 673_424,
                minimum_kbps: 618_400,
                maximum_kbps: 840_100,
            },
            DirectionValidationInput {
                observed_low_kbps: 903_800,
                candidate_kbps: 795_300,
                achieved_kbps: 738_447,
                minimum_kbps: 632_600,
                maximum_kbps: 859_700,
            },
        );
        input.download_load.transport_delta_ms = 0.0;
        input.upload_load.transport_delta_ms = 0.0;
        let result = validate_shaped_candidate(input).unwrap();

        assert_eq!(result.correction.action, CorrectionAction::Increase);
        assert_eq!(
            result.correction.download.action,
            CorrectionAction::Increase
        );
        assert_eq!(result.correction.upload.action, CorrectionAction::None);
        assert!(
            result
                .correction
                .download
                .predicted_capacity_retention_percent
                >= 80.0
        );
        assert_eq!(result.correction.upload.proposed_kbps, 795_300);
    }

    #[test]
    fn adverse_signal_does_not_reduce_an_already_below_floor_candidate() {
        let mut input = validation_input(
            DirectionValidationInput {
                observed_low_kbps: 883_500,
                candidate_kbps: 738_500,
                achieved_kbps: 683_153,
                minimum_kbps: 618_400,
                maximum_kbps: 840_100,
            },
            DirectionValidationInput {
                observed_low_kbps: 903_800,
                candidate_kbps: 755_500,
                achieved_kbps: 698_955,
                minimum_kbps: 632_600,
                maximum_kbps: 859_700,
            },
        );
        input.download_load.transport_delta_ms = 260.0;
        input.upload_load.transport_delta_ms = 260.0;
        input.download_load.loss_percent = 7.59;
        input.upload_load.loss_percent = 7.59;
        let result = validate_shaped_candidate(input).unwrap();

        assert_eq!(result.correction.action, CorrectionAction::Infeasible);
        assert!(!result.correction.feasible);
        assert_eq!(
            result.correction.reason,
            "safety-floor-blocks-rate-reduction"
        );
        assert_eq!(
            result.correction.download.proposed_kbps,
            input.download.candidate_kbps
        );
        assert_eq!(
            result.correction.upload.proposed_kbps,
            input.upload.candidate_kbps
        );
    }

    #[test]
    fn every_profile_fails_closed_when_its_latency_target_conflicts_with_its_floor() {
        for profile in [
            AutotuneProfile::Gaming,
            AutotuneProfile::BestOverall,
            AutotuneProfile::Fair,
        ] {
            let thresholds = profile.validation_thresholds();
            let candidate = (100_000.0 * thresholds.capacity_retention_min_percent / 100.0) as u64;
            let direction = DirectionValidationInput {
                observed_low_kbps: 100_000,
                candidate_kbps: candidate,
                achieved_kbps: candidate,
                minimum_kbps: 10_000,
                maximum_kbps: 110_000,
            };
            let load = DirectionLoadInput {
                icmp_delta_ms: thresholds.icmp_delta_max_ms + 10.0,
                transport_delta_ms: thresholds.transport_delta_max_ms + 10.0,
                loss_percent: 0.0,
                cpu_percent: 10.0,
            };
            let result = validate_shaped_candidate(ValidationInput {
                profile,
                download: direction,
                upload: direction,
                download_load: load,
                upload_load: load,
                thresholds,
            })
            .unwrap();

            assert!(
                !result.pass,
                "{profile:?} must not accept a conflicting candidate"
            );
            if profile == AutotuneProfile::Fair {
                assert!(result.hard_pass);
                assert!(!result.quality_target_met);
            } else {
                assert!(!result.hard_pass);
            }
            assert!(!result.correction.feasible);
            assert_eq!(result.correction.action, CorrectionAction::Infeasible);
            assert_eq!(
                result.correction.reason,
                "safety-floor-blocks-rate-reduction"
            );
            assert_eq!(result.correction.download.proposed_kbps, candidate);
            assert_eq!(result.correction.upload.proposed_kbps, candidate);
        }
    }

    #[test]
    fn fair_reports_actual_grade_when_class_c_conflicts_with_the_throughput_floor() {
        let thresholds = AutotuneProfile::Fair.validation_thresholds();
        let direction = DirectionValidationInput {
            observed_low_kbps: 100_000,
            candidate_kbps: 90_000,
            achieved_kbps: 90_000,
            minimum_kbps: 60_000,
            maximum_kbps: 100_000,
        };
        let load = DirectionLoadInput {
            icmp_delta_ms: 250.0,
            transport_delta_ms: 230.0,
            loss_percent: 0.0,
            cpu_percent: 20.0,
        };
        let result = validate_shaped_candidate(ValidationInput {
            profile: AutotuneProfile::Fair,
            download: direction,
            upload: direction,
            download_load: load,
            upload_load: load,
            thresholds,
        })
        .unwrap();

        assert!(!result.pass);
        assert!(result.hard_pass);
        assert!(!result.quality_target_met);
        assert_eq!(result.actual_grade, "D");
        assert_eq!(result.correction.action, CorrectionAction::Infeasible);
        assert_eq!(
            result.correction.reason,
            "safety-floor-blocks-rate-reduction"
        );
    }

    #[test]
    fn fair_can_raise_a_clean_candidate_above_the_old_ninety_five_percent_cap() {
        let thresholds = AutotuneProfile::Fair.validation_thresholds();
        let direction = DirectionValidationInput {
            observed_low_kbps: 100_000,
            candidate_kbps: 94_000,
            achieved_kbps: 88_360,
            minimum_kbps: 80_000,
            maximum_kbps: 98_000,
        };
        let load = DirectionLoadInput {
            icmp_delta_ms: 10.0,
            transport_delta_ms: 10.0,
            loss_percent: 0.0,
            cpu_percent: 20.0,
        };
        let result = validate_shaped_candidate(ValidationInput {
            profile: AutotuneProfile::Fair,
            download: direction,
            upload: direction,
            download_load: load,
            upload_load: load,
            thresholds,
        })
        .unwrap();

        assert!(!result.pass);
        assert!(!result.hard_pass);
        assert!(result.quality_target_met);
        assert_eq!(result.correction.action, CorrectionAction::Increase);
        assert!(result.correction.feasible);
        assert!(result.correction.download.proposed_kbps > 95_000);
        assert!(result.correction.download.proposed_kbps <= 98_000);
        assert!(
            result
                .correction
                .download
                .predicted_capacity_retention_percent
                >= 90.0
        );
    }

    #[test]
    fn bounded_decrease_is_clamped_to_reachable_capacity_floor() {
        let direction = DirectionValidationInput {
            observed_low_kbps: 100_000,
            candidate_kbps: 90_000,
            achieved_kbps: 85_500,
            minimum_kbps: 40_000,
            maximum_kbps: 110_000,
        };
        let mut input = validation_input(direction, direction);
        input.download_load.transport_delta_ms = 150.0;
        input.upload_load.transport_delta_ms = 150.0;
        let result = validate_shaped_candidate(input).unwrap();

        assert_eq!(result.correction.action, CorrectionAction::Decrease);
        assert!(result.correction.feasible);
        assert_eq!(result.correction.download.proposed_kbps, 85_500);
        assert!(
            result
                .correction
                .download
                .predicted_capacity_retention_percent
                >= 80.0
        );
        assert!(result.correction.download.scale > 0.949);
    }

    #[test]
    fn every_feasible_decrease_keeps_the_capacity_floor_reachable() {
        for candidate_capacity_percent in [82_u64, 85, 90, 95] {
            for realization_percent in [85_u64, 90, 95, 100] {
                let observed = 1_000_000_u64;
                let candidate = observed * candidate_capacity_percent / 100;
                let achieved = candidate * realization_percent / 100;
                let direction = DirectionValidationInput {
                    observed_low_kbps: observed,
                    candidate_kbps: candidate,
                    achieved_kbps: achieved,
                    minimum_kbps: 400_000,
                    maximum_kbps: 1_100_000,
                };
                let mut input = validation_input(direction, direction);
                input.download_load.transport_delta_ms = 150.0;
                input.upload_load.transport_delta_ms = 150.0;
                let result = validate_shaped_candidate(input).unwrap();
                if result.correction.feasible
                    && result.correction.action == CorrectionAction::Decrease
                {
                    assert!(
                        result
                            .correction
                            .download
                            .predicted_capacity_retention_percent
                            >= input.thresholds.capacity_retention_min_percent
                    );
                    assert!(
                        result
                            .correction
                            .upload
                            .predicted_capacity_retention_percent
                            >= input.thresholds.capacity_retention_min_percent
                    );
                }
            }
        }
    }

    #[test]
    fn conservative_candidate_below_floor_is_raised_instead_of_reduced() {
        let direction = DirectionValidationInput {
            observed_low_kbps: 100_000,
            candidate_kbps: 74_800,
            achieved_kbps: 74_800,
            minimum_kbps: 40_000,
            maximum_kbps: 105_000,
        };
        let mut input = validation_input(direction, direction);
        input.download_load.transport_delta_ms = 20.0;
        input.upload_load.transport_delta_ms = 20.0;
        let result = validate_shaped_candidate(input).unwrap();

        assert_eq!(result.correction.action, CorrectionAction::Increase);
        assert_eq!(result.correction.download.required_floor_kbps, 80_000);
        assert!(result.correction.download.proposed_kbps >= 80_000);
        assert!(
            result
                .correction
                .download
                .predicted_capacity_retention_percent
                >= 80.0
        );
    }

    #[test]
    fn low_candidate_realization_requests_new_measurement_without_rate_change() {
        let direction = DirectionValidationInput {
            observed_low_kbps: 100_000,
            candidate_kbps: 90_000,
            achieved_kbps: 60_000,
            minimum_kbps: 40_000,
            maximum_kbps: 105_000,
        };
        let result = validate_shaped_candidate(validation_input(direction, direction)).unwrap();

        assert_eq!(result.correction.action, CorrectionAction::RetryMeasurement);
        assert!(!result.correction.feasible);
        assert_eq!(result.correction.download.scale, 1.0);
        assert_eq!(result.correction.download.proposed_kbps, 90_000);
        assert!(result.score < 100.0);
    }

    #[test]
    fn excessive_candidate_realization_never_validates_an_unenforced_shaper() {
        let direction = DirectionValidationInput {
            observed_low_kbps: 1_000_000,
            candidate_kbps: 800_000,
            achieved_kbps: 1_000_000,
            minimum_kbps: 20_000,
            maximum_kbps: 1_000_000,
        };
        let mut input = validation_input(direction, direction);
        input.download_load.transport_delta_ms = 0.0;
        input.upload_load.transport_delta_ms = 0.0;
        let result = validate_shaped_candidate(input).unwrap();

        assert!(!result.pass);
        assert!(!gate_pass(
            &result.gates,
            "download-candidate-realization-maximum"
        ));
        assert!(!gate_pass(
            &result.gates,
            "upload-candidate-realization-maximum"
        ));
        assert_eq!(result.correction.action, CorrectionAction::RetryMeasurement);
        assert_eq!(result.correction.reason, "candidate-realization-too-high");
        assert_eq!(
            result.correction.download.reason,
            "candidate-realization-too-high"
        );
        assert_eq!(result.correction.download.proposed_kbps, 800_000);
    }

    #[test]
    fn infeasible_reason_distinguishes_maximum_from_bounded_headroom() {
        let maximum_limited = DirectionValidationInput {
            observed_low_kbps: 100_000,
            candidate_kbps: 80_000,
            achieved_kbps: 72_000,
            minimum_kbps: 40_000,
            maximum_kbps: 85_000,
        };
        let maximum_result =
            validate_shaped_candidate(validation_input(maximum_limited, maximum_limited)).unwrap();
        assert_eq!(
            maximum_result.correction.reason,
            "maximum-rate-cannot-reach-safety-floor"
        );

        let bounded = DirectionValidationInput {
            observed_low_kbps: 100_000,
            candidate_kbps: 80_000,
            achieved_kbps: 64_000,
            minimum_kbps: 40_000,
            maximum_kbps: 110_000,
        };
        let bounded_result = validate_shaped_candidate(validation_input(bounded, bounded)).unwrap();
        assert_eq!(
            bounded_result.correction.reason,
            "bounded-correction-cannot-reach-safety-floor"
        );
    }

    #[test]
    fn caller_supplied_same_quantile_deltas_are_used_without_rebaselining() {
        let direction = DirectionValidationInput {
            observed_low_kbps: 100_000,
            candidate_kbps: 90_000,
            achieved_kbps: 85_000,
            minimum_kbps: 40_000,
            maximum_kbps: 105_000,
        };
        let input = validation_input(direction, direction);
        let result = validate_shaped_candidate(input).unwrap();

        assert_eq!(result.effective_delta_ms, 60.0);
        assert!(gate_pass(&result.gates, "download-icmp-latency"));
        assert!(gate_pass(&result.gates, "upload-icmp-latency"));
        assert!(gate_pass(&result.gates, "download-transport-latency"));
        assert!(gate_pass(&result.gates, "upload-transport-latency"));
    }

    #[test]
    fn directional_base_scales_do_not_change_the_other_direction() {
        let mut proposal = build_proposal(
            &[100_000.0, 101_000.0],
            &[20_000.0, 21_000.0],
            LatencyBaseline {
                median_ms: 5.0,
                p95_ms: 6.0,
                samples: 10,
            },
            LinkKind::Pppoe,
        )
        .unwrap();
        let original_upload = proposal.upload.base_kbps;

        proposal.revise_base_rates_by_direction(1.05, 1.0).unwrap();

        assert_eq!(proposal.upload.base_kbps, original_upload);
        assert!(proposal.download.base_kbps > 88_000);
    }

    #[test]
    fn validation_json_contains_structured_gates_reasons_and_correction() {
        let direction = DirectionValidationInput {
            observed_low_kbps: 100_000,
            candidate_kbps: 90_000,
            achieved_kbps: 85_000,
            minimum_kbps: 40_000,
            maximum_kbps: 105_000,
        };
        let mut input = validation_input(direction, direction);
        input.download_load.transport_delta_ms = 150.0;
        input.upload_load.transport_delta_ms = 150.0;
        let json = validate_shaped_candidate(input).unwrap().to_json();

        assert!(json.contains("\"candidate_realization_percent\""));
        assert!(json.contains("\"capacity_retention_percent\""));
        assert!(json.contains("\"schema_version\":3"));
        assert!(json.contains("\"signals\":{\"download\":"));
        assert!(json.contains("\"code\":\"download-transport-latency\""));
        assert!(json.contains("\"code\":\"upload-transport-latency\""));
        assert!(json.contains("\"reasons\":["));
        assert!(json.contains("\"correction\":{"));
    }

    #[test]
    fn loaded_signals_correct_only_the_direction_that_failed() {
        let direction = DirectionValidationInput {
            observed_low_kbps: 100_000,
            candidate_kbps: 90_000,
            achieved_kbps: 85_000,
            minimum_kbps: 40_000,
            maximum_kbps: 105_000,
        };
        let mut input = validation_input(direction, direction);
        input.download_load.transport_delta_ms = 150.0;
        input.upload_load.transport_delta_ms = 20.0;
        let result = validate_shaped_candidate(input).unwrap();

        assert_eq!(result.correction.action, CorrectionAction::Decrease);
        assert_eq!(
            result.correction.download.action,
            CorrectionAction::Decrease
        );
        assert_eq!(result.correction.upload.action, CorrectionAction::None);
        assert!(gate_pass(&result.gates, "upload-transport-latency"));
        assert!(!gate_pass(&result.gates, "download-transport-latency"));
    }

    #[test]
    fn every_throughput_sample_must_be_valid_instead_of_being_filtered() {
        let baseline = LatencyBaseline {
            median_ms: 5.0,
            p95_ms: 8.0,
            samples: 10,
        };
        for invalid in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -1.0,
            0.0,
            MAX_RATE_KBPS as f64 + 1.0,
        ] {
            assert!(
                build_proposal(
                    &[100_000.0, invalid, 101_000.0],
                    &[20_000.0],
                    baseline,
                    LinkKind::Unknown,
                )
                .is_err(),
                "accepted invalid sample {invalid:?}"
            );
        }
        assert!(build_proposal(&[], &[1.0], baseline, LinkKind::Unknown).is_err());
        assert!(build_proposal(
            &vec![1.0; MAX_THROUGHPUT_SAMPLES + 1],
            &[1.0],
            baseline,
            LinkKind::Unknown,
        )
        .is_err());
    }

    #[test]
    fn baseline_validation_is_finite_ordered_positive_and_bounded() {
        let rates = [10_000.0];
        for baseline in [
            LatencyBaseline {
                median_ms: f64::NAN,
                p95_ms: 2.0,
                samples: 1,
            },
            LatencyBaseline {
                median_ms: 0.0,
                p95_ms: 2.0,
                samples: 1,
            },
            LatencyBaseline {
                median_ms: 2.0,
                p95_ms: 1.0,
                samples: 1,
            },
            LatencyBaseline {
                median_ms: 2.0,
                p95_ms: f64::INFINITY,
                samples: 1,
            },
            LatencyBaseline {
                median_ms: MAX_LATENCY_MS + 1.0,
                p95_ms: MAX_LATENCY_MS + 1.0,
                samples: 1,
            },
            LatencyBaseline {
                median_ms: 1.0,
                p95_ms: 2.0,
                samples: 0,
            },
            LatencyBaseline {
                median_ms: 1.0,
                p95_ms: 2.0,
                samples: MAX_BASELINE_SAMPLES + 1,
            },
        ] {
            assert!(
                build_proposal(&rates, &rates, baseline, LinkKind::Unknown).is_err(),
                "accepted baseline {baseline:?}"
            );
        }
    }

    #[test]
    fn proposal_rates_never_exceed_global_bound_at_input_boundary() {
        let proposal = build_proposal(
            &[MAX_RATE_KBPS as f64 / 2.0, MAX_RATE_KBPS as f64],
            &[MAX_RATE_KBPS as f64],
            LatencyBaseline {
                median_ms: 1.0,
                p95_ms: 1.0,
                samples: 1,
            },
            LinkKind::Ethernet,
        )
        .unwrap();
        for direction in [proposal.download, proposal.upload] {
            assert!(direction.minimum_kbps <= direction.base_kbps);
            assert!(direction.base_kbps <= direction.maximum_kbps);
            assert!(direction.maximum_kbps <= direction.absolute_cap_kbps);
            assert!(direction.absolute_cap_kbps <= MAX_RATE_KBPS);
            assert!(direction.observed_high_kbps <= MAX_RATE_KBPS);
        }
    }

    #[test]
    fn validation_rejects_rates_above_global_bound() {
        let direction = DirectionValidationInput {
            observed_low_kbps: MAX_RATE_KBPS + 1,
            candidate_kbps: 90_000,
            achieved_kbps: 85_000,
            minimum_kbps: 40_000,
            maximum_kbps: 105_000,
        };
        assert!(validate_shaped_candidate(validation_input(direction, direction)).is_err());
    }

    #[test]
    fn required_floor_conversion_is_bounded_for_tiny_realization() {
        let direction = DirectionValidationInput {
            observed_low_kbps: MAX_RATE_KBPS,
            candidate_kbps: MAX_RATE_KBPS,
            achieved_kbps: 1,
            minimum_kbps: 1,
            maximum_kbps: MAX_RATE_KBPS,
        };
        let result = validate_shaped_candidate(validation_input(direction, direction)).unwrap();
        assert_eq!(result.correction.action, CorrectionAction::RetryMeasurement);
        assert_eq!(
            result.correction.download.required_floor_kbps,
            MAX_RATE_KBPS
        );
        assert_eq!(result.correction.upload.required_floor_kbps, MAX_RATE_KBPS);
    }
}
