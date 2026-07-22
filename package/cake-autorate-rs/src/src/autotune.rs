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

    pub fn target_delta_ms(self) -> f64 {
        match self {
            Self::Gaming => 5.0,
            Self::BestOverall => 30.0,
            Self::Fair => 200.0,
        }
    }

    pub fn capacity_floor_percent(self) -> f64 {
        match self {
            Self::Gaming => 70.0,
            Self::BestOverall => 80.0,
            Self::Fair => 90.0,
        }
    }

    pub fn objective(self) -> &'static str {
        match self {
            Self::Gaming => "quality-constrained-throughput",
            Self::BestOverall => "balanced-quality-throughput",
            Self::Fair => "throughput-first",
        }
    }

    pub fn validation_thresholds(self) -> ValidationThresholds {
        let (latency_delta_max_ms, loss_max_percent) = match self {
            Self::Gaming => (5.0, 1.0),
            Self::BestOverall => (30.0, 3.0),
            Self::Fair => (200.0, 5.0),
        };
        ValidationThresholds {
            candidate_realization_min_percent: 80.0,
            candidate_realization_max_percent: 110.0,
            capacity_retention_min_percent: self.capacity_floor_percent(),
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
            (Self::Fair, true) => (0.35, 0.92, 1.30, 1.90),
            // A short cellular calibration can look stable even though the
            // radio scheduler moves materially before shaped validation.  A
            // 35% search/configuration minimum gives the bounded search room
            // to establish an actually enforced CAKE rate; the 90% Fair
            // retention objective still controls unattended Auto-Apply.
            (Self::Fair, false) => (0.35, 0.94, 0.98, 1.08),
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
                script: "layer_cake.qos",
                classification: "diffserv4",
                squash_dscp: true,
                squash_ingress: true,
                ingress_ecn: "ECN",
                egress_ecn: "NOECN",
                iqdisc_opts: "besteffort",
                eqdisc_opts: "diffserv4",
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
/// A shaped candidate below half of the conservative direction-matched raw
/// capacity crosses a manual-review trust boundary.  It is not a hard safety
/// failure: cellular radio scheduling can legitimately move by more than 2x
/// between the raw and shaped samples.  Profile retention targets still block
/// Auto-Apply, while clean latency/loss/route evidence may remain reviewable.
pub const THROUGHPUT_TRUST_FLOOR_PERCENT: f64 = 50.0;

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
    /// Numeric confidence in the proposal builder's direct inputs. Full
    /// Auto-Tune publishes its structured result confidence at the job level.
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
        // The measured quality boundary, not a fixed profile multiplier, is
        // the final upper bound.  Every profile may therefore explore up to
        // the direction-matched observed-low reference during Full Auto-Tune.
        let observed_low_ceiling = 1.0;
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
        self.warnings.push(
            "Conservative calibration constraints were applied: isolated speed-test samples were preserved and confirmed maxima/caps were never raised.",
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
    if !scale.is_finite() || !(0.35..=1.5).contains(&scale) {
        return Err("base-rate revision scale must be between 0.35 and 1.5".to_string());
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
    ExclusiveMaximum,
}

impl GateComparison {
    fn as_str(self) -> &'static str {
        match self {
            Self::Minimum => "minimum",
            Self::Maximum => "maximum",
            Self::ExclusiveMaximum => "exclusive-maximum",
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
    pub safety_pass: bool,
    pub profile_objectives_met: bool,
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
        self.gates
            .iter()
            .filter(|gate| !gate.pass && (gate.required || gate.code.contains("latency")))
    }

    pub fn warnings(&self) -> impl Iterator<Item = &ValidationGate> {
        self.gates
            .iter()
            .filter(|gate| !gate.pass && !gate.required && !gate.code.contains("latency"))
    }

    pub fn to_json(&self) -> String {
        let gates = self
            .gates
            .iter()
            .map(gate_json)
            .collect::<Vec<_>>()
            .join(",");
        let reasons = self.reasons().map(gate_json).collect::<Vec<_>>().join(",");
        let warnings = self.warnings().map(gate_json).collect::<Vec<_>>().join(",");
        format!(
            concat!(
                "{{\"schema_version\":5,\"profile\":\"{}\",\"pass\":{},",
                "\"hard_pass\":{},\"safety_pass\":{},\"profile_objectives_met\":{},",
                "\"quality_target_met\":{},\"actual_grade\":\"{}\",",
                "\"score\":{:.1},",
                "\"metrics\":{{\"download\":{},\"upload\":{},\"effective_delta_ms\":{:.3}}},",
                "\"signals\":{{\"download\":{},\"upload\":{}}},",
                "\"gates\":[{}],\"reasons\":[{}],\"warnings\":[{}],\"correction\":{}}}"
            ),
            self.profile.as_str(),
            self.pass,
            self.hard_pass,
            self.safety_pass,
            self.profile_objectives_met,
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
            warnings,
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
        advisory_minimum_gate(
            "download-capacity-retention",
            ValidationScope::Download,
            download.capacity_retention_percent,
            thresholds.capacity_retention_min_percent,
        ),
        advisory_minimum_gate(
            "upload-capacity-retention",
            ValidationScope::Upload,
            upload.capacity_retention_percent,
            thresholds.capacity_retention_min_percent,
        ),
        advisory_minimum_gate(
            "download-throughput-safety-floor",
            ValidationScope::Download,
            download.capacity_retention_percent,
            THROUGHPUT_TRUST_FLOOR_PERCENT,
        ),
        advisory_minimum_gate(
            "upload-throughput-safety-floor",
            ValidationScope::Upload,
            upload.capacity_retention_percent,
            THROUGHPUT_TRUST_FLOOR_PERCENT,
        ),
        exclusive_maximum_gate(
            "download-icmp-latency",
            ValidationScope::Download,
            input.download_load.icmp_delta_ms,
            thresholds.icmp_delta_max_ms,
        ),
        exclusive_maximum_gate(
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
        advisory_maximum_gate(
            "download-cpu",
            ValidationScope::Download,
            input.download_load.cpu_percent,
            thresholds.cpu_max_percent,
        ),
        exclusive_maximum_gate(
            "upload-icmp-latency",
            ValidationScope::Upload,
            input.upload_load.icmp_delta_ms,
            thresholds.icmp_delta_max_ms,
        ),
        exclusive_maximum_gate(
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
        advisory_maximum_gate(
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
    let pass = gates
        .iter()
        .filter(|gate| gate.required || gate.code.contains("latency"))
        .all(|gate| gate.pass);
    let hard_pass = gates
        .iter()
        .filter(|gate| gate.required)
        .all(|gate| gate.pass);
    let safety_pass = gates
        .iter()
        .filter(|gate| gate.required && !gate.code.contains("latency"))
        .all(|gate| gate.pass);
    let profile_objectives_met = gates
        .iter()
        .filter(|gate| {
            matches!(
                gate.code,
                "download-candidate-realization"
                    | "upload-candidate-realization"
                    | "download-capacity-retention"
                    | "upload-capacity-retention"
            )
        })
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
        safety_pass,
        profile_objectives_met,
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

fn advisory_minimum_gate(
    code: &'static str,
    scope: ValidationScope,
    actual: f64,
    limit: f64,
) -> ValidationGate {
    let mut gate = minimum_gate(code, scope, actual, limit);
    gate.required = false;
    gate
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

fn advisory_maximum_gate(
    code: &'static str,
    scope: ValidationScope,
    actual: f64,
    limit: f64,
) -> ValidationGate {
    let mut gate = maximum_gate(code, scope, actual, limit);
    gate.required = false;
    gate
}

fn exclusive_maximum_gate(
    code: &'static str,
    scope: ValidationScope,
    actual: f64,
    limit: f64,
) -> ValidationGate {
    ValidationGate {
        code,
        scope,
        required: true,
        pass: actual < limit,
        actual,
        limit,
        comparison: GateComparison::ExclusiveMaximum,
    }
}

fn validation_score(gates: &[ValidationGate]) -> f64 {
    gates
        .iter()
        .filter(|gate| !gate.code.ends_with("-cpu"))
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
            GateComparison::ExclusiveMaximum => {
                if gate.actual < gate.limit || gate.actual <= 0.0 {
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
    let observed_low_ceiling = 1.0;
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
            "Gaming uses native profile rules for outbound DSCP classification. Review or disable the built-in presets and add explicit application/network rules when needed.",
        );
        warnings.push(
            "Gaming preserves ingress DSCP because download packets reach the SQM IFB before outbound nftables classification. Use Best overall when upstream markings are not trusted.",
        );
    } else {
        warnings.push(
            "Profile traffic rules classify outbound traffic with diffserv4. Download traffic remains best effort because WAN ingress reaches the SQM IFB before the native nftables rule hooks.",
        );
    }
    if profile == AutotuneProfile::Fair {
        warnings.push(
            "Fair prioritizes sustained throughput with a 90% retention objective and a separate 50% historical-throughput trust warning. Class C is a conditional goal; if the link cannot reach the objective, only a controlled candidate may be offered for explicit review instead of chasing bandwidth through excessive latency.",
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
    // Profile multipliers are starting hints only.  The bounded search must
    // be able to prove a faster candidate up to observed-low instead of
    // declaring the target impossible behind an artificial 0.92/0.95/0.98
    // ceiling.
    let maximum = checked_rounded_rate(maximum)?
        .max(checked_rounded_rate(low)?)
        .max(base);
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

pub const MAX_PROFILE_SEARCH_OBSERVATIONS: usize = 8;
const MAX_SAME_CANDIDATE_OBSERVATIONS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchDirection {
    Download,
    Upload,
}

impl SearchDirection {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "download" => Some(Self::Download),
            "upload" => Some(Self::Upload),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Download => "download",
            Self::Upload => "upload",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SearchObservation {
    pub candidate_kbps: u64,
    pub achieved_kbps: u64,
    pub icmp_delta_ms: f64,
    pub transport_delta_ms: f64,
    pub loss_percent: f64,
    pub cpu_percent: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SearchObservationMetrics {
    pub realization_percent: f64,
    pub retention_percent: f64,
    pub effective_delta_ms: f64,
    pub grade: &'static str,
    pub measurement_reliable: bool,
    pub resource_safe: bool,
    pub safety_pass: bool,
    pub capacity_objective_met: bool,
    pub target_met: bool,
    pub balanced_score: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProfileSearchInput {
    pub profile: AutotuneProfile,
    pub direction: SearchDirection,
    pub observed_low_kbps: u64,
    pub minimum_kbps: u64,
    pub upper_kbps: u64,
    pub thresholds: ValidationThresholds,
    pub uncertainty_percent: f64,
    pub max_attempts: usize,
    pub observations: Vec<SearchObservation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileSearchAction {
    Test,
    Complete,
    Fallback,
    Inconclusive,
}

impl ProfileSearchAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::Test => "test",
            Self::Complete => "complete",
            Self::Fallback => "fallback",
            Self::Inconclusive => "inconclusive",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProfileSearchResult {
    pub profile: AutotuneProfile,
    pub direction: SearchDirection,
    pub action: ProfileSearchAction,
    pub reason: &'static str,
    pub next_candidate_kbps: Option<u64>,
    pub selected_index: Option<usize>,
    pub lower_target_pass_kbps: Option<u64>,
    pub upper_target_fail_kbps: Option<u64>,
    pub resolution_kbps: u64,
    pub max_attempts: usize,
    pub metrics: Vec<SearchObservationMetrics>,
    pub observations: Vec<SearchObservation>,
}

impl ProfileSearchResult {
    pub fn to_json(&self) -> String {
        let selected = self.selected_index.map_or_else(
            || "null".to_string(),
            |index| {
                let observation = self.observations[index];
                let metrics = self.metrics[index];
                format!(
                    concat!(
                        "{{\"index\":{},\"candidate_kbps\":{},\"achieved_kbps\":{},",
                        "\"realization_percent\":{:.3},\"retention_percent\":{:.3},",
                        "\"effective_delta_ms\":{:.3},",
                        "\"loss_percent\":{:.3},\"cpu_percent\":{:.3},",
                        "\"grade\":\"{}\",\"safety_pass\":{},",
                        "\"capacity_objective_met\":{},\"target_met\":{}}}"
                    ),
                    index + 1,
                    observation.candidate_kbps,
                    observation.achieved_kbps,
                    metrics.realization_percent,
                    metrics.retention_percent,
                    metrics.effective_delta_ms,
                    observation.loss_percent,
                    observation.cpu_percent,
                    metrics.grade,
                    metrics.safety_pass,
                    metrics.capacity_objective_met,
                    metrics.target_met,
                )
            },
        );
        let evaluated = self
            .observations
            .iter()
            .zip(&self.metrics)
            .enumerate()
            .map(|(index, (observation, metrics))| {
                format!(
                    concat!(
                        "{{\"index\":{},\"candidate_kbps\":{},\"achieved_kbps\":{},",
                        "\"realization_percent\":{:.3},\"retention_percent\":{:.3},",
                        "\"effective_delta_ms\":{:.3},\"grade\":\"{}\",",
                        "\"loss_percent\":{:.3},\"cpu_percent\":{:.3},",
                        "\"measurement_reliable\":{},\"resource_safe\":{},",
                        "\"safety_pass\":{},\"capacity_objective_met\":{},",
                        "\"target_met\":{},\"balanced_score\":{:.3}}}"
                    ),
                    index + 1,
                    observation.candidate_kbps,
                    observation.achieved_kbps,
                    metrics.realization_percent,
                    metrics.retention_percent,
                    metrics.effective_delta_ms,
                    metrics.grade,
                    observation.loss_percent,
                    observation.cpu_percent,
                    metrics.measurement_reliable,
                    metrics.resource_safe,
                    metrics.safety_pass,
                    metrics.capacity_objective_met,
                    metrics.target_met,
                    metrics.balanced_score,
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let optional_rate = |value: Option<u64>| {
            value.map_or_else(|| "null".to_string(), |value| value.to_string())
        };
        format!(
            concat!(
                "{{\"schema_version\":1,\"profile\":\"{}\",\"direction\":\"{}\",",
                "\"objective\":\"{}\",\"target_grade\":\"{}\",",
                "\"capacity_floor_percent\":{:.1},\"capacity_objective_percent\":{:.1},",
                "\"throughput_safety_floor_percent\":{:.1},\"action\":\"{}\",",
                "\"reason\":\"{}\",\"next_candidate_kbps\":{},",
                "\"selected\":{},\"bounds\":{{\"lower_target_pass_kbps\":{},",
                "\"upper_target_fail_kbps\":{},\"resolution_kbps\":{}}},",
                "\"attempts\":{},\"max_attempts\":{},\"evaluated\":[{}]}}"
            ),
            self.profile.as_str(),
            self.direction.as_str(),
            self.profile.objective(),
            self.profile.target_grade(),
            self.profile.capacity_floor_percent(),
            self.profile.capacity_floor_percent(),
            THROUGHPUT_TRUST_FLOOR_PERCENT,
            self.action.as_str(),
            self.reason,
            optional_rate(self.next_candidate_kbps),
            selected,
            optional_rate(self.lower_target_pass_kbps),
            optional_rate(self.upper_target_fail_kbps),
            self.resolution_kbps,
            self.observations.len(),
            self.max_attempts,
            evaluated,
        )
    }
}

fn validate_profile_search_input(input: &ProfileSearchInput) -> Result<(), String> {
    if input.observed_low_kbps == 0 || input.observed_low_kbps > MAX_RATE_KBPS {
        return Err("search observed-low rate is outside the supported range".to_string());
    }
    if input.minimum_kbps == 0
        || input.minimum_kbps > input.upper_kbps
        || input.upper_kbps > input.observed_low_kbps
    {
        return Err("search bounds must satisfy 0 < minimum <= upper <= observed-low".to_string());
    }
    if input.observations.is_empty() || input.observations.len() > MAX_PROFILE_SEARCH_OBSERVATIONS {
        return Err(format!(
            "search requires between 1 and {MAX_PROFILE_SEARCH_OBSERVATIONS} observations"
        ));
    }
    if !(2..=MAX_PROFILE_SEARCH_OBSERVATIONS).contains(&input.max_attempts) {
        return Err(format!(
            "search max attempts must be between 2 and {MAX_PROFILE_SEARCH_OBSERVATIONS}"
        ));
    }
    if !input.uncertainty_percent.is_finite() || !(0.0..=10.0).contains(&input.uncertainty_percent)
    {
        return Err("search uncertainty must be between 0 and 10 percent".to_string());
    }
    let thresholds = input.thresholds;
    if !thresholds.candidate_realization_min_percent.is_finite()
        || !thresholds.candidate_realization_max_percent.is_finite()
        || thresholds.candidate_realization_min_percent < 0.0
        || thresholds.candidate_realization_max_percent
            < thresholds.candidate_realization_min_percent
        || thresholds.candidate_realization_max_percent > 200.0
        || !thresholds.capacity_retention_min_percent.is_finite()
        || !(0.0..=100.0).contains(&thresholds.capacity_retention_min_percent)
        || !thresholds.loss_max_percent.is_finite()
        || !(0.0..=100.0).contains(&thresholds.loss_max_percent)
        || !thresholds.cpu_max_percent.is_finite()
        || !(0.0..=100.0).contains(&thresholds.cpu_max_percent)
    {
        return Err("search thresholds are invalid".to_string());
    }
    for (index, observation) in input.observations.iter().enumerate() {
        if observation.candidate_kbps < input.minimum_kbps
            || observation.candidate_kbps > input.upper_kbps
            || observation.achieved_kbps == 0
            || observation.achieved_kbps > MAX_RATE_KBPS
        {
            return Err(format!(
                "search observation {} has an invalid rate",
                index + 1
            ));
        }
        for (name, value, maximum) in [
            ("ICMP delta", observation.icmp_delta_ms, MAX_LATENCY_MS),
            (
                "transport delta",
                observation.transport_delta_ms,
                MAX_LATENCY_MS,
            ),
            ("loss", observation.loss_percent, 100.0),
            ("CPU", observation.cpu_percent, 100.0),
        ] {
            if !value.is_finite() || !(0.0..=maximum).contains(&value) {
                return Err(format!(
                    "search observation {} {name} is outside the supported range",
                    index + 1
                ));
            }
        }
    }
    Ok(())
}

fn evaluate_search_observation(
    input: &ProfileSearchInput,
    observation: SearchObservation,
) -> SearchObservationMetrics {
    let realization_percent =
        observation.achieved_kbps as f64 * 100.0 / observation.candidate_kbps as f64;
    let retention_percent =
        observation.achieved_kbps as f64 * 100.0 / input.observed_low_kbps as f64;
    let effective_delta_ms = observation
        .icmp_delta_ms
        .max(observation.transport_delta_ms);
    let grade = classify_quality(Some(effective_delta_ms)).as_str();
    let measurement_reliable = realization_percent
        >= input.thresholds.candidate_realization_min_percent
        && realization_percent <= input.thresholds.candidate_realization_max_percent;
    let resource_safe = observation.loss_percent <= input.thresholds.loss_max_percent;
    // Historical retention is not a safety signal on a variable radio link,
    // but candidate realization is: CAKE cannot control a bottleneck below
    // its configured rate.  Only a sufficiently exercised candidate may be
    // selected, even for explicit manual review.
    let safety_pass = measurement_reliable && resource_safe;
    let capacity_objective_met =
        retention_percent >= input.thresholds.capacity_retention_min_percent;
    // Grade boundaries are exclusive at A+/A/B/C, matching the runtime
    // classifier exactly.  A 5.000 ms increase is A, not A+.
    let target_met = effective_delta_ms < input.profile.target_delta_ms();
    let throughput_component = (retention_percent / 100.0).clamp(0.0, 1.0);
    let quality_component = if effective_delta_ms <= 0.0 {
        1.0
    } else {
        (input.profile.target_delta_ms() / effective_delta_ms).clamp(0.0, 1.0)
    };
    let balanced_score = (throughput_component + quality_component) * 50.0;
    SearchObservationMetrics {
        realization_percent,
        retention_percent,
        effective_delta_ms,
        grade,
        measurement_reliable,
        resource_safe,
        safety_pass,
        capacity_objective_met,
        target_met,
        balanced_score,
    }
}

fn rounded_search_rate(rate_kbps: f64) -> u64 {
    rounded_rate_up(rate_kbps)
}

fn search_resolution(observed_low_kbps: u64) -> u64 {
    rounded_search_rate((observed_low_kbps as f64 * 0.005).max(100.0))
}

fn best_target_index(
    observations: &[SearchObservation],
    metrics: &[SearchObservationMetrics],
) -> Option<usize> {
    observations
        .iter()
        .enumerate()
        .filter(|(index, _)| metrics[*index].safety_pass && metrics[*index].target_met)
        .max_by(|(left_index, left), (right_index, right)| {
            left.achieved_kbps.cmp(&right.achieved_kbps).then_with(|| {
                metrics[*right_index]
                    .effective_delta_ms
                    .total_cmp(&metrics[*left_index].effective_delta_ms)
            })
        })
        .map(|(index, _)| index)
}

fn best_quality_index(
    observations: &[SearchObservation],
    metrics: &[SearchObservationMetrics],
) -> Option<usize> {
    observations
        .iter()
        .enumerate()
        .filter(|(index, _)| metrics[*index].safety_pass)
        .min_by(|(left_index, left), (right_index, right)| {
            quality_grade_rank(metrics[*left_index].grade)
                .cmp(&quality_grade_rank(metrics[*right_index].grade))
                .then_with(|| right.achieved_kbps.cmp(&left.achieved_kbps))
                .then_with(|| {
                    metrics[*left_index]
                        .effective_delta_ms
                        .total_cmp(&metrics[*right_index].effective_delta_ms)
                })
        })
        .map(|(index, _)| index)
}

fn quality_grade_rank(grade: &str) -> u8 {
    match grade {
        "A+" => 0,
        "A" => 1,
        "B" => 2,
        "C" => 3,
        "D" => 4,
        _ => 5,
    }
}

fn best_balanced_index(
    observations: &[SearchObservation],
    metrics: &[SearchObservationMetrics],
) -> Option<usize> {
    observations
        .iter()
        .enumerate()
        .filter(|(index, _)| metrics[*index].safety_pass)
        .max_by(|(left_index, left), (right_index, right)| {
            metrics[*left_index]
                .balanced_score
                .total_cmp(&metrics[*right_index].balanced_score)
                .then_with(|| left.achieved_kbps.cmp(&right.achieved_kbps))
        })
        .map(|(index, _)| index)
}

fn best_fair_index(
    input: &ProfileSearchInput,
    metrics: &[SearchObservationMetrics],
) -> Option<usize> {
    let best_achieved = input
        .observations
        .iter()
        .enumerate()
        .filter(|(index, _)| metrics[*index].safety_pass)
        .map(|(_, observation)| observation.achieved_kbps)
        .max()?;
    let lower = best_achieved as f64 * (1.0 - input.uncertainty_percent / 100.0);
    input
        .observations
        .iter()
        .enumerate()
        .filter(|(index, observation)| {
            metrics[*index].safety_pass && observation.achieved_kbps as f64 >= lower
        })
        .min_by(|(left_index, left), (right_index, right)| {
            metrics[*left_index]
                .effective_delta_ms
                .total_cmp(&metrics[*right_index].effective_delta_ms)
                .then_with(|| right.achieved_kbps.cmp(&left.achieved_kbps))
        })
        .map(|(index, _)| index)
}

fn candidate_was_tested(observations: &[SearchObservation], candidate_kbps: u64) -> bool {
    observations
        .iter()
        .any(|observation| observation.candidate_kbps == candidate_kbps)
}

fn achieved_rates_repeatable(left_kbps: u64, right_kbps: u64) -> bool {
    let high = left_kbps.max(right_kbps) as f64;
    let low = left_kbps.min(right_kbps) as f64;
    high > 0.0 && (high - low) * 100.0 / high <= 5.0
}

fn low_realization_evidence_eligible(
    profile: AutotuneProfile,
    metrics: SearchObservationMetrics,
) -> bool {
    metrics.resource_safe && (profile == AutotuneProfile::Fair || metrics.target_met)
}

fn repeatable_low_realization_peer(
    input: &ProfileSearchInput,
    metrics: &[SearchObservationMetrics],
    index: usize,
) -> Option<usize> {
    let observation = input.observations[index];
    if metrics[index].realization_percent >= input.thresholds.candidate_realization_min_percent
        || !low_realization_evidence_eligible(input.profile, metrics[index])
    {
        return None;
    }
    input
        .observations
        .iter()
        .enumerate()
        .rev()
        .find(|(peer_index, peer)| {
            *peer_index != index
                && peer.candidate_kbps == observation.candidate_kbps
                && metrics[*peer_index].realization_percent
                    < input.thresholds.candidate_realization_min_percent
                && low_realization_evidence_eligible(input.profile, metrics[*peer_index])
                && achieved_rates_repeatable(peer.achieved_kbps, observation.achieved_kbps)
        })
        .map(|(peer_index, _)| peer_index)
}

fn controlled_candidate_from_low_realization(
    input: &ProfileSearchInput,
    metrics: &[SearchObservationMetrics],
    candidate_kbps: u64,
) -> Option<u64> {
    let candidate_indices = input
        .observations
        .iter()
        .enumerate()
        .filter(|(_, observation)| observation.candidate_kbps == candidate_kbps)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();

    if candidate_indices.len() < 2
        || candidate_indices.iter().any(|index| {
            metrics[*index].realization_percent
                >= input.thresholds.candidate_realization_min_percent
                || !low_realization_evidence_eligible(input.profile, metrics[*index])
        })
    {
        return None;
    }

    let has_repeatable_pair = candidate_indices
        .iter()
        .enumerate()
        .any(|(position, left)| {
            candidate_indices.iter().skip(position + 1).any(|right| {
                achieved_rates_repeatable(
                    input.observations[*left].achieved_kbps,
                    input.observations[*right].achieved_kbps,
                )
            })
        });
    if !has_repeatable_pair && candidate_indices.len() < MAX_SAME_CANDIDATE_OBSERVATIONS {
        return None;
    }

    // Aim halfway between the configured minimum realization and 100%.  The
    // worst clean achieved sample is deliberately used so the next candidate
    // is likely to sit below the moving radio bottleneck.  The candidate is
    // still re-tested; this calculation never manufactures a passing result.
    let achieved_low = candidate_indices
        .iter()
        .map(|index| input.observations[*index].achieved_kbps)
        .min()?;
    let target_realization = (input.thresholds.candidate_realization_min_percent + 100.0) / 2.0;
    let next = rounded_search_rate(achieved_low as f64 * 100.0 / target_realization)
        .max(input.minimum_kbps)
        .min(candidate_kbps.saturating_sub(1));
    (next >= input.minimum_kbps && next < candidate_kbps).then_some(next)
}

fn midpoint_candidate(lower: u64, upper: u64) -> u64 {
    rounded_search_rate(lower as f64 + (upper - lower) as f64 / 2.0).min(upper)
}

pub fn optimize_profile_direction(
    input: ProfileSearchInput,
) -> Result<ProfileSearchResult, String> {
    validate_profile_search_input(&input)?;
    let metrics = input
        .observations
        .iter()
        .copied()
        .map(|observation| evaluate_search_observation(&input, observation))
        .collect::<Vec<_>>();
    let resolution_kbps = search_resolution(input.observed_low_kbps);
    let last_index = input.observations.len() - 1;
    let last = input.observations[last_index];
    let last_metrics = metrics[last_index];
    let duplicate_count = input
        .observations
        .iter()
        .filter(|observation| observation.candidate_kbps == last.candidate_kbps)
        .count();
    let lower_target_pass_kbps = input
        .observations
        .iter()
        .zip(&metrics)
        .filter(|(_, metric)| metric.safety_pass && metric.target_met)
        .map(|(observation, _)| observation.candidate_kbps)
        .max();
    let upper_target_fail_kbps = lower_target_pass_kbps.and_then(|lower| {
        input
            .observations
            .iter()
            .zip(&metrics)
            .filter(|(observation, metric)| {
                observation.candidate_kbps > lower
                    && metric.measurement_reliable
                    && (!metric.target_met || !metric.resource_safe)
            })
            .map(|(observation, _)| observation.candidate_kbps)
            .min()
    });
    let last_repeatable_low_peer = repeatable_low_realization_peer(&input, &metrics, last_index);
    let controlled_retry_candidate =
        controlled_candidate_from_low_realization(&input, &metrics, last.candidate_kbps);
    let profile_selected_index = match input.profile {
        AutotuneProfile::Gaming => best_target_index(&input.observations, &metrics)
            .or_else(|| best_quality_index(&input.observations, &metrics)),
        AutotuneProfile::BestOverall => best_target_index(&input.observations, &metrics)
            .or_else(|| best_balanced_index(&input.observations, &metrics)),
        AutotuneProfile::Fair => best_fair_index(&input, &metrics).or_else(|| {
            input
                .observations
                .iter()
                .enumerate()
                .filter(|(index, _)| metrics[*index].resource_safe)
                .max_by_key(|(_, observation)| observation.achieved_kbps)
                .map(|(index, _)| index)
        }),
    };
    let selected_index = profile_selected_index;

    let finish = |action: ProfileSearchAction,
                  reason: &'static str,
                  next_candidate_kbps: Option<u64>| ProfileSearchResult {
        profile: input.profile,
        direction: input.direction,
        action,
        reason,
        next_candidate_kbps,
        selected_index,
        lower_target_pass_kbps,
        upper_target_fail_kbps,
        resolution_kbps,
        max_attempts: input.max_attempts,
        metrics: metrics.clone(),
        observations: input.observations.clone(),
    };

    if !last_metrics.measurement_reliable {
        if last_metrics.realization_percent > input.thresholds.candidate_realization_max_percent {
            if duplicate_count < MAX_SAME_CANDIDATE_OBSERVATIONS
                && input.observations.len() < input.max_attempts
            {
                return Ok(finish(
                    ProfileSearchAction::Test,
                    "repeat-unreliable-realization",
                    Some(last.candidate_kbps),
                ));
            }
            return Ok(finish(
                ProfileSearchAction::Inconclusive,
                "repeated-candidate-realization-unreliable",
                None,
            ));
        }

        // Exhausting the bounded search while probing above an already
        // controlled point does not invalidate that lower measurement.  Keep
        // the proven point for manual/profile evaluation; never select the
        // final under-realized boundary probe itself.
        if input.observations.len() >= input.max_attempts {
            if let Some(index) = selected_index.filter(|index| metrics[*index].safety_pass) {
                return Ok(finish(
                    if input.profile == AutotuneProfile::Fair || metrics[index].target_met {
                        ProfileSearchAction::Complete
                    } else {
                        ProfileSearchAction::Fallback
                    },
                    "bounded-attempt-limit-controlled-candidate",
                    None,
                ));
            }
        }

        if controlled_retry_candidate.is_none() {
            if duplicate_count < MAX_SAME_CANDIDATE_OBSERVATIONS
                && input.observations.len() < input.max_attempts
            {
                return Ok(finish(
                    ProfileSearchAction::Test,
                    "repeat-low-candidate-realization",
                    Some(last.candidate_kbps),
                ));
            }
            return Ok(finish(
                ProfileSearchAction::Inconclusive,
                "low-candidate-realization-not-repeatable",
                None,
            ));
        }

        let controlled_candidate = controlled_retry_candidate.expect("checked above");
        if !candidate_was_tested(&input.observations, controlled_candidate)
            && input.observations.len() < input.max_attempts
        {
            return Ok(finish(
                ProfileSearchAction::Test,
                if last_repeatable_low_peer.is_some() {
                    "lower-candidate-to-establish-shaper-control"
                } else {
                    "lower-variable-candidate-to-establish-shaper-control"
                },
                Some(controlled_candidate),
            ));
        }
        if let Some(controlled_index) = input
            .observations
            .iter()
            .enumerate()
            .find(|(index, observation)| {
                observation.candidate_kbps == controlled_candidate && metrics[*index].safety_pass
            })
            .map(|(index, _)| index)
        {
            if last.candidate_kbps - controlled_candidate > resolution_kbps
                && input.observations.len() < input.max_attempts
            {
                let next = midpoint_candidate(controlled_candidate, last.candidate_kbps);
                if next > controlled_candidate
                    && next < last.candidate_kbps
                    && !candidate_was_tested(&input.observations, next)
                {
                    return Ok(finish(
                        ProfileSearchAction::Test,
                        "bisect-controlled-shaper-boundary",
                        Some(next),
                    ));
                }
            }
            return Ok(finish(
                if input.profile == AutotuneProfile::Fair || metrics[controlled_index].target_met {
                    ProfileSearchAction::Complete
                } else {
                    ProfileSearchAction::Fallback
                },
                "maximum-controlled-candidate-bounded",
                None,
            ));
        }
        return Ok(finish(
            ProfileSearchAction::Inconclusive,
            "unable-to-establish-controlled-shaper-candidate",
            None,
        ));
    }

    if !last_metrics.resource_safe {
        // Loss or another non-CPU resource failure must never manufacture a
        // terminal fallback without a selected safe point.  Repeat the exact
        // observation and fail as inconclusive if it remains unsafe.
        if duplicate_count < MAX_SAME_CANDIDATE_OBSERVATIONS
            && input.observations.len() < input.max_attempts
        {
            return Ok(finish(
                ProfileSearchAction::Test,
                "repeat-resource-unsafe-candidate",
                Some(last.candidate_kbps),
            ));
        }
        return Ok(finish(
            ProfileSearchAction::Inconclusive,
            "resource-safety-failure-not-resolved",
            None,
        ));
    }

    if input.observations.len() >= input.max_attempts {
        let selected_is_safe = selected_index
            .map(|index| metrics[index].safety_pass)
            .unwrap_or(false);
        let selected_meets_target = selected_index
            .map(|index| metrics[index].target_met)
            .unwrap_or(false);
        let action = match input.profile {
            AutotuneProfile::Fair if selected_is_safe => ProfileSearchAction::Complete,
            AutotuneProfile::Gaming | AutotuneProfile::BestOverall
                if selected_is_safe && selected_meets_target =>
            {
                ProfileSearchAction::Complete
            }
            _ if selected_index.is_some() => ProfileSearchAction::Fallback,
            _ => ProfileSearchAction::Inconclusive,
        };
        let reason = if action == ProfileSearchAction::Inconclusive {
            "bounded-attempt-limit-without-safe-candidate"
        } else {
            "bounded-attempt-limit"
        };
        return Ok(finish(action, reason, None));
    }

    if input.profile == AutotuneProfile::Fair {
        if !last_metrics.safety_pass && last_metrics.resource_safe {
            let realization = last_metrics.realization_percent / 100.0;
            let required = rounded_search_rate(
                input.observed_low_kbps as f64
                    * (input.thresholds.capacity_retention_min_percent / 100.0)
                    / realization,
            );
            if required > last.candidate_kbps
                && required <= input.upper_kbps
                && !candidate_was_tested(&input.observations, required)
            {
                return Ok(finish(
                    ProfileSearchAction::Test,
                    "raise-rate-toward-throughput-floor",
                    Some(required),
                ));
            }
        }
        let highest_safe = input
            .observations
            .iter()
            .zip(&metrics)
            .filter(|(_, metric)| metric.safety_pass)
            .map(|(observation, _)| observation.candidate_kbps)
            .max();
        let lowest_unsafe_above = highest_safe.and_then(|lower| {
            input
                .observations
                .iter()
                .zip(&metrics)
                .filter(|(observation, metric)| {
                    observation.candidate_kbps > lower && !metric.safety_pass
                })
                .map(|(observation, _)| observation.candidate_kbps)
                .min()
        });
        if let (Some(lower), Some(upper)) = (highest_safe, lowest_unsafe_above) {
            if upper - lower > resolution_kbps {
                let next = midpoint_candidate(lower, upper);
                if next > lower && next < upper && !candidate_was_tested(&input.observations, next)
                {
                    return Ok(finish(
                        ProfileSearchAction::Test,
                        "bisect-throughput-safety-boundary",
                        Some(next),
                    ));
                }
            }
        }
        if !candidate_was_tested(&input.observations, input.upper_kbps) {
            return Ok(finish(
                ProfileSearchAction::Test,
                "test-throughput-upper-bound",
                Some(input.upper_kbps),
            ));
        }
        let action = if selected_index
            .map(|index| metrics[index].safety_pass)
            .unwrap_or(false)
        {
            ProfileSearchAction::Complete
        } else if selected_index.is_some() {
            ProfileSearchAction::Fallback
        } else {
            ProfileSearchAction::Inconclusive
        };
        let reason = if action == ProfileSearchAction::Inconclusive {
            "throughput-search-has-no-safe-candidate"
        } else {
            "throughput-optimum-bounded"
        };
        return Ok(finish(action, reason, None));
    }

    if last_metrics.resource_safe && last_metrics.target_met && !last_metrics.safety_pass {
        let realization = last_metrics.realization_percent / 100.0;
        let required = rounded_search_rate(
            input.observed_low_kbps as f64
                * (input.thresholds.capacity_retention_min_percent / 100.0)
                / realization,
        );
        if required > last.candidate_kbps
            && required <= input.upper_kbps
            && !candidate_was_tested(&input.observations, required)
        {
            return Ok(finish(
                ProfileSearchAction::Test,
                "raise-rate-to-capacity-floor",
                Some(required),
            ));
        }
        if !candidate_was_tested(&input.observations, input.upper_kbps) {
            return Ok(finish(
                ProfileSearchAction::Test,
                "test-upper-for-capacity-floor",
                Some(input.upper_kbps),
            ));
        }
        return Ok(finish(
            ProfileSearchAction::Fallback,
            "profile-target-conflicts-with-capacity-floor",
            None,
        ));
    }

    if let Some(lower) = lower_target_pass_kbps {
        if let Some(upper) = upper_target_fail_kbps {
            if upper - lower > resolution_kbps {
                let next = midpoint_candidate(lower, upper);
                if next > lower && next < upper && !candidate_was_tested(&input.observations, next)
                {
                    return Ok(finish(
                        ProfileSearchAction::Test,
                        "bisect-quality-boundary",
                        Some(next),
                    ));
                }
            }
            return Ok(finish(
                ProfileSearchAction::Complete,
                "maximum-target-grade-bounded",
                None,
            ));
        }
        if lower < input.upper_kbps && !candidate_was_tested(&input.observations, input.upper_kbps)
        {
            return Ok(finish(
                ProfileSearchAction::Test,
                "test-quality-upper-bound",
                Some(input.upper_kbps),
            ));
        }
        return Ok(finish(
            ProfileSearchAction::Complete,
            "maximum-target-grade-confirmed",
            None,
        ));
    }

    let lowest_tested = input
        .observations
        .iter()
        .map(|observation| observation.candidate_kbps)
        .min()
        .unwrap_or(last.candidate_kbps);
    if !candidate_was_tested(&input.observations, input.minimum_kbps) {
        let next = midpoint_candidate(input.minimum_kbps, lowest_tested);
        let next = if next >= lowest_tested {
            input.minimum_kbps
        } else {
            next
        };
        return Ok(finish(
            ProfileSearchAction::Test,
            "search-lower-quality-candidate",
            Some(next),
        ));
    }
    if selected_index.is_none() {
        return Ok(finish(
            ProfileSearchAction::Inconclusive,
            "profile-search-has-no-safe-candidate",
            None,
        ));
    }
    Ok(finish(
        ProfileSearchAction::Fallback,
        if input.profile == AutotuneProfile::Gaming {
            "target-a-plus-unreachable-above-safety-floor"
        } else {
            "target-a-unreachable-use-balanced-fallback"
        },
        None,
    ))
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
            .any(|warning| warning.contains("native profile rules")));
    }

    #[test]
    fn stable_fibre_proposal_exposes_observed_low_to_search_without_adaptive_ceiling() {
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
        assert_eq!(proposal.download.maximum_kbps, 896_000);
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
        let input_confidence = proposal.confidence;

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
        assert_eq!(proposal.confidence, input_confidence);
        assert!(proposal
            .warnings
            .iter()
            .any(|warning| warning.contains("isolated speed-test samples were preserved")));
    }

    #[test]
    fn retained_direction_is_still_clamped_to_confirmed_maximum_and_cap() {
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
        let retained_download = DirectionProposal {
            minimum_kbps: 500_000,
            base_kbps: 650_000,
            maximum_kbps: 800_000,
            absolute_cap_kbps: 900_000,
            observed_low_kbps: proposal.download.observed_low_kbps,
            observed_median_kbps: proposal.download.observed_median_kbps,
            observed_high_kbps: proposal.download.observed_high_kbps,
            variability: proposal.download.variability,
        };

        proposal.apply_conservative_constraints(
            Some(retained_download),
            None,
            Some(600_000),
            Some(625_000),
            None,
            None,
        );

        assert_eq!(proposal.download.minimum_kbps, 500_000);
        assert_eq!(proposal.download.base_kbps, 600_000);
        assert_eq!(proposal.download.maximum_kbps, 600_000);
        assert_eq!(proposal.download.absolute_cap_kbps, 625_000);
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
        assert!(json.contains("\"classification\":\"diffserv4\""));
        assert!(json.contains("\"overhead\":44"));
        assert!(json.contains("\"confidence\":"));
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
        assert!(result.pass);
        assert!(result.safety_pass);
        assert!(!result.profile_objectives_met);
        assert_eq!(result.correction.action, CorrectionAction::None);
        assert!(result
            .warnings()
            .any(|gate| gate.code == "download-capacity-retention"));
    }

    #[test]
    fn clean_candidate_below_retention_objective_remains_safely_reviewable() {
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

        assert!(result.pass);
        assert!(result.safety_pass);
        assert!(!result.profile_objectives_met);
        assert_eq!(result.correction.action, CorrectionAction::None);
        assert!(!gate_pass(&result.gates, "download-capacity-retention"));
        assert!(gate_pass(&result.gates, "upload-capacity-retention"));
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
    fn fair_retention_objective_is_advisory_above_the_safety_floor() {
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

        assert!(result.pass);
        assert!(result.hard_pass);
        assert!(result.safety_pass);
        assert!(!result.profile_objectives_met);
        assert!(result.quality_target_met);
        assert_eq!(result.correction.action, CorrectionAction::None);
        assert!(result.correction.feasible);
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
    fn conservative_candidate_below_profile_objective_is_reviewable() {
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

        assert!(result.pass);
        assert!(result.safety_pass);
        assert!(!result.profile_objectives_met);
        assert_eq!(result.correction.action, CorrectionAction::None);
    }

    #[test]
    fn low_candidate_realization_blocks_an_unenforced_shaper() {
        let direction = DirectionValidationInput {
            observed_low_kbps: 100_000,
            candidate_kbps: 90_000,
            achieved_kbps: 60_000,
            minimum_kbps: 40_000,
            maximum_kbps: 105_000,
        };
        let result = validate_shaped_candidate(validation_input(direction, direction)).unwrap();

        assert!(!result.pass);
        assert!(!result.safety_pass);
        assert!(!result.profile_objectives_met);
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
    fn sub_fifty_percent_throughput_is_a_manual_trust_warning() {
        let maximum_limited = DirectionValidationInput {
            observed_low_kbps: 100_000,
            candidate_kbps: 49_000,
            achieved_kbps: 45_000,
            minimum_kbps: 40_000,
            maximum_kbps: 85_000,
        };
        let maximum_result =
            validate_shaped_candidate(validation_input(maximum_limited, maximum_limited)).unwrap();
        assert!(maximum_result.pass);
        assert!(maximum_result.hard_pass);
        assert!(maximum_result.safety_pass);
        assert!(!maximum_result.profile_objectives_met);
        assert!(!gate_pass(
            &maximum_result.gates,
            "download-throughput-safety-floor"
        ));
        assert!(maximum_result
            .warnings()
            .any(|gate| gate.code == "download-throughput-safety-floor"));
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
    fn fair_step_down_scale_can_reach_its_bounded_search_minimum() {
        let mut proposal = build_proposal_for_profile(
            &[100_000.0, 101_000.0],
            &[20_000.0, 20_200.0],
            LatencyBaseline {
                median_ms: 5.0,
                p95_ms: 6.0,
                samples: 10,
            },
            LinkKind::Cellular,
            AutotuneProfile::Fair,
        )
        .unwrap();

        proposal.revise_base_rates_by_direction(0.35, 0.35).unwrap();

        assert_eq!(proposal.download.base_kbps, proposal.download.minimum_kbps);
        assert_eq!(proposal.upload.base_kbps, proposal.upload.minimum_kbps);
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
        assert!(json.contains("\"schema_version\":5"));
        assert!(json.contains("\"profile_objectives_met\":"));
        assert!(json.contains("\"safety_pass\":"));
        assert!(json.contains("\"signals\":{\"download\":"));
        assert!(json.contains("\"code\":\"download-transport-latency\""));
        assert!(json.contains("\"code\":\"upload-transport-latency\""));
        assert!(json.contains("\"reasons\":["));
        assert!(json.contains("\"warnings\":["));
        assert!(json.contains("\"correction\":{"));
    }

    #[test]
    fn rc25_high_cpu_is_advisory_for_an_a_plus_gaming_candidate() {
        let direction = DirectionValidationInput {
            observed_low_kbps: 917_600,
            candidate_kbps: 694_700,
            achieved_kbps: 642_360,
            minimum_kbps: 100_000,
            maximum_kbps: 917_600,
        };
        let mut input = ValidationInput {
            profile: AutotuneProfile::Gaming,
            download: direction,
            upload: direction,
            download_load: DirectionLoadInput {
                icmp_delta_ms: 0.0,
                transport_delta_ms: 4.9,
                loss_percent: 0.0,
                cpu_percent: 90.1,
            },
            upload_load: DirectionLoadInput {
                icmp_delta_ms: 0.0,
                transport_delta_ms: 0.8,
                loss_percent: 0.0,
                cpu_percent: 70.2,
            },
            thresholds: AutotuneProfile::Gaming.validation_thresholds(),
        };
        input.thresholds.capacity_retention_min_percent = 70.0;

        let result = validate_shaped_candidate(input).unwrap();
        let cpu_gate = result
            .gates
            .iter()
            .find(|gate| gate.code == "download-cpu")
            .unwrap();
        assert!(result.pass);
        assert!(result.hard_pass);
        assert!(result.safety_pass);
        assert!(result.quality_target_met);
        assert_eq!(result.actual_grade, "A+");
        assert_eq!(result.score, 100.0);
        assert_eq!(result.correction.action, CorrectionAction::None);
        assert!(!cpu_gate.required);
        assert!(!cpu_gate.pass);
        assert_eq!(result.reasons().count(), 0);
        assert_eq!(
            result.warnings().map(|gate| gate.code).collect::<Vec<_>>(),
            vec!["download-cpu"]
        );
        let json = result.to_json();
        assert!(json.contains("\"warnings\":[{\"code\":\"download-cpu\""));
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
    fn tiny_realization_is_never_a_manual_apply_candidate() {
        let direction = DirectionValidationInput {
            observed_low_kbps: MAX_RATE_KBPS,
            candidate_kbps: MAX_RATE_KBPS,
            achieved_kbps: 1,
            minimum_kbps: 1,
            maximum_kbps: MAX_RATE_KBPS,
        };
        let result = validate_shaped_candidate(validation_input(direction, direction)).unwrap();
        assert!(!result.pass);
        assert!(!result.safety_pass);
        assert!(!result.profile_objectives_met);
        assert_eq!(result.correction.action, CorrectionAction::RetryMeasurement);
        assert_eq!(
            result.correction.download.required_floor_kbps,
            MAX_RATE_KBPS
        );
        assert_eq!(result.correction.upload.required_floor_kbps, MAX_RATE_KBPS);
    }

    fn search_observation(
        candidate_kbps: u64,
        achieved_kbps: u64,
        effective_delta_ms: f64,
    ) -> SearchObservation {
        search_observation_with_cpu(candidate_kbps, achieved_kbps, effective_delta_ms, 50.0)
    }

    fn search_observation_with_cpu(
        candidate_kbps: u64,
        achieved_kbps: u64,
        effective_delta_ms: f64,
        cpu_percent: f64,
    ) -> SearchObservation {
        SearchObservation {
            candidate_kbps,
            achieved_kbps,
            icmp_delta_ms: 0.0,
            transport_delta_ms: effective_delta_ms,
            loss_percent: 0.0,
            cpu_percent,
        }
    }

    fn profile_search(
        profile: AutotuneProfile,
        observed_low_kbps: u64,
        minimum_kbps: u64,
        observations: Vec<SearchObservation>,
    ) -> ProfileSearchResult {
        optimize_profile_direction(ProfileSearchInput {
            profile,
            direction: SearchDirection::Download,
            observed_low_kbps,
            minimum_kbps,
            upper_kbps: observed_low_kbps,
            thresholds: profile.validation_thresholds(),
            uncertainty_percent: 1.5,
            max_attempts: 6,
            observations,
        })
        .unwrap()
    }

    #[test]
    fn fair_retention_objective_does_not_skip_the_measured_upper_bound() {
        let result = profile_search(
            AutotuneProfile::Fair,
            902_700,
            722_100,
            vec![
                search_observation(848_500, 783_677, 3.9),
                search_observation(890_900, 804_918, 3.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Test);
        assert_eq!(result.reason, "test-throughput-upper-bound");
        assert_eq!(result.next_candidate_kbps, Some(902_700));
    }

    #[test]
    fn fair_tests_the_real_directional_upper_bound_and_uses_quality_as_tiebreaker() {
        let result = profile_search(
            AutotuneProfile::Fair,
            902_700,
            722_100,
            vec![
                search_observation(890_900, 804_918, 3.0),
                search_observation(899_300, 812_700, 3.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Test);
        assert_eq!(result.next_candidate_kbps, Some(902_700));

        let result = profile_search(
            AutotuneProfile::Fair,
            902_700,
            722_100,
            vec![
                search_observation(890_900, 804_918, 3.0),
                search_observation(899_300, 812_700, 3.0),
                search_observation(902_700, 813_000, 20.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Complete);
        let selected = result.selected_index.unwrap();
        assert_eq!(result.observations[selected].candidate_kbps, 899_300);
        assert_eq!(result.metrics[selected].grade, "A+");
    }

    #[test]
    fn gaming_maximizes_throughput_inside_the_a_plus_boundary() {
        let first = profile_search(
            AutotuneProfile::Gaming,
            100_000,
            60_000,
            vec![search_observation(82_000, 78_000, 3.0)],
        );
        assert_eq!(first.action, ProfileSearchAction::Test);
        assert_eq!(first.next_candidate_kbps, Some(100_000));

        let bracket = profile_search(
            AutotuneProfile::Gaming,
            100_000,
            60_000,
            vec![
                search_observation(82_000, 78_000, 3.0),
                search_observation(100_000, 95_000, 8.0),
            ],
        );
        assert_eq!(bracket.action, ProfileSearchAction::Test);
        assert_eq!(bracket.reason, "bisect-quality-boundary");
        assert_eq!(bracket.next_candidate_kbps, Some(91_000));
    }

    #[test]
    fn exact_five_milliseconds_is_not_mislabelled_as_a_plus() {
        let result = profile_search(
            AutotuneProfile::Gaming,
            100_000,
            60_000,
            vec![search_observation(82_000, 78_000, 5.0)],
        );
        assert!(!result.metrics[0].target_met);
        assert_eq!(result.metrics[0].grade, "A");
    }

    #[test]
    fn best_overall_uses_balanced_fallback_when_a_is_unreachable() {
        let result = profile_search(
            AutotuneProfile::BestOverall,
            100_000,
            70_000,
            vec![
                search_observation(70_000, 67_000, 35.0),
                search_observation(85_000, 82_000, 60.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Fallback);
        assert_eq!(result.next_candidate_kbps, None);
        let selected = result.selected_index.unwrap();
        // Profile retention is an optimization objective. The lower-latency
        // controlled point remains reviewable despite missing that objective.
        assert_eq!(result.observations[selected].candidate_kbps, 70_000);
    }

    #[test]
    fn repeatable_low_realization_steps_down_to_establish_shaper_control() {
        let result = profile_search(
            AutotuneProfile::Fair,
            800_000,
            640_000,
            vec![
                search_observation_with_cpu(752_000, 410_000, 8.0, 96.0),
                search_observation_with_cpu(752_000, 415_000, 8.0, 97.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Test);
        assert_eq!(result.reason, "lower-candidate-to-establish-shaper-control");
        assert_eq!(result.next_candidate_kbps, Some(640_000));
    }

    #[test]
    fn repeated_upper_low_realization_still_requires_a_controlled_retest() {
        let result = profile_search(
            AutotuneProfile::Fair,
            800_000,
            640_000,
            vec![
                search_observation_with_cpu(752_000, 410_000, 8.0, 96.0),
                search_observation_with_cpu(752_000, 415_000, 8.0, 97.0),
                search_observation_with_cpu(800_000, 420_000, 8.0, 99.0),
                search_observation_with_cpu(800_000, 425_000, 8.0, 100.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Test);
        assert_eq!(result.reason, "lower-candidate-to-establish-shaper-control");
        assert_eq!(result.next_candidate_kbps, Some(640_000));
    }

    #[test]
    fn controlled_retest_bisects_toward_maximum_safe_throughput() {
        let result = profile_search(
            AutotuneProfile::Fair,
            800_000,
            480_000,
            vec![
                search_observation(752_000, 410_000, 8.0),
                search_observation(752_000, 415_000, 8.0),
                search_observation(480_000, 455_000, 8.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Test);
        assert_eq!(result.reason, "bisect-throughput-safety-boundary");
        assert_eq!(result.next_candidate_kbps, Some(616_000));
    }

    #[test]
    fn repeated_unsafe_boundary_reuses_the_proven_controlled_lower_point() {
        let result = profile_search(
            AutotuneProfile::Fair,
            800_000,
            480_000,
            vec![
                search_observation(752_000, 410_000, 8.0),
                search_observation(752_000, 415_000, 8.0),
                search_observation(480_000, 455_000, 8.0),
                search_observation(616_000, 400_000, 8.0),
                search_observation(616_000, 405_000, 8.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Test);
        assert_eq!(result.reason, "bisect-controlled-shaper-boundary");
        assert_eq!(result.next_candidate_kbps, Some(548_000));
        assert_eq!(result.selected_index, Some(2));
    }

    #[test]
    fn attempt_limit_keeps_a_proven_controlled_point_not_the_last_bad_probe() {
        let result = profile_search(
            AutotuneProfile::Fair,
            800_000,
            480_000,
            vec![
                search_observation(480_000, 455_000, 8.0),
                search_observation(500_000, 475_000, 8.0),
                search_observation(520_000, 495_000, 8.0),
                search_observation(540_000, 510_000, 8.0),
                search_observation(560_000, 530_000, 8.0),
                search_observation(580_000, 550_000, 8.0),
                search_observation(600_000, 570_000, 8.0),
                search_observation(616_000, 400_000, 8.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Complete);
        assert_eq!(result.reason, "bounded-attempt-limit-controlled-candidate");
        let selected = result.selected_index.expect("controlled result");
        assert_ne!(selected, 7);
        assert!(result.metrics[selected].safety_pass);
        assert!(!result.metrics[7].safety_pass);
    }

    #[test]
    fn variable_5g_fair_result_steps_down_before_manual_review() {
        let download = DirectionValidationInput {
            observed_low_kbps: 140_200,
            candidate_kbps: 131_800,
            achieved_kbps: 98_101,
            minimum_kbps: 112_100,
            maximum_kbps: 140_200,
        };
        let upload = DirectionValidationInput {
            observed_low_kbps: 19_500,
            candidate_kbps: 19_500,
            achieved_kbps: 16_259,
            minimum_kbps: 15_600,
            maximum_kbps: 19_500,
        };
        let load = DirectionLoadInput {
            icmp_delta_ms: 40.6,
            transport_delta_ms: 46.9,
            loss_percent: 0.0,
            cpu_percent: 70.9,
        };
        let validation = validate_shaped_candidate(ValidationInput {
            profile: AutotuneProfile::Fair,
            download,
            upload,
            download_load: load,
            upload_load: load,
            thresholds: AutotuneProfile::Fair.validation_thresholds(),
        })
        .unwrap();

        assert!(!validation.pass);
        assert!(!validation.hard_pass);
        assert!(!validation.safety_pass);
        assert!(validation.quality_target_met);
        assert!(!validation.profile_objectives_met);
        assert_eq!(
            validation.correction.action,
            CorrectionAction::RetryMeasurement
        );
        assert!(gate_pass(
            &validation.gates,
            "download-throughput-safety-floor"
        ));
        assert!(gate_pass(
            &validation.gates,
            "upload-throughput-safety-floor"
        ));
        assert!(validation
            .reasons()
            .any(|gate| gate.code == "download-candidate-realization"));
        assert!(validation
            .warnings()
            .any(|gate| gate.code == "download-capacity-retention"));

        let download_search = profile_search(
            AutotuneProfile::Fair,
            140_200,
            112_100,
            vec![
                search_observation(131_800, 101_141, 46.9),
                search_observation(131_800, 98_101, 46.9),
            ],
        );
        assert_eq!(download_search.action, ProfileSearchAction::Test);
        assert_eq!(
            download_search.reason,
            "lower-candidate-to-establish-shaper-control"
        );
        assert_eq!(download_search.next_candidate_kbps, Some(112_100));

        let upload_search = profile_search(
            AutotuneProfile::Fair,
            19_500,
            15_600,
            vec![
                search_observation(18_300, 15_881, 42.8),
                search_observation(19_500, 16_259, 42.8),
            ],
        );
        assert_eq!(upload_search.action, ProfileSearchAction::Complete);
        let selected = upload_search.selected_index.expect("safe upload selection");
        assert!(upload_search.metrics[selected].safety_pass);
        assert!(!upload_search.metrics[selected].capacity_objective_met);
    }

    #[test]
    fn rc25_fair_search_keeps_high_cpu_advisory() {
        let result = profile_search(
            AutotuneProfile::Fair,
            904_700,
            723_800,
            vec![
                search_observation_with_cpu(849_500, 786_499, 13.6, 100.0),
                search_observation_with_cpu(904_700, 830_073, 1.9, 100.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Complete);
        let selected = result.selected_index.expect("safe high-CPU selection");
        assert_eq!(result.observations[selected].candidate_kbps, 904_700);
        assert_eq!(result.observations[selected].cpu_percent, 100.0);
        assert!(result.metrics[selected].resource_safe);
        assert!(result.metrics[selected].safety_pass);
    }

    #[test]
    fn fair_upload_objective_continues_to_the_measured_upper_bound() {
        let result = profile_search(
            AutotuneProfile::Fair,
            903_800,
            723_000,
            vec![
                search_observation_with_cpu(849_500, 786_706, 0.0, 75.0),
                search_observation_with_cpu(878_400, 811_057, 0.0, 72.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Test);
        assert_eq!(result.reason, "test-throughput-upper-bound");
        assert_eq!(result.next_candidate_kbps, Some(903_800));
    }

    #[test]
    fn repeated_non_cpu_resource_failure_is_inconclusive_not_null_fallback() {
        let observation = |achieved_kbps| SearchObservation {
            candidate_kbps: 100_000,
            achieved_kbps,
            icmp_delta_ms: 1.0,
            transport_delta_ms: 1.0,
            loss_percent: 10.0,
            cpu_percent: 50.0,
        };
        let repeat = profile_search(
            AutotuneProfile::Fair,
            100_000,
            80_000,
            vec![observation(95_000), observation(95_500)],
        );
        assert_eq!(repeat.action, ProfileSearchAction::Test);
        assert_eq!(repeat.reason, "repeat-resource-unsafe-candidate");

        let inconclusive = profile_search(
            AutotuneProfile::Fair,
            100_000,
            80_000,
            vec![
                observation(95_000),
                observation(95_500),
                observation(95_200),
            ],
        );
        assert_eq!(inconclusive.action, ProfileSearchAction::Inconclusive);
        assert_eq!(inconclusive.reason, "resource-safety-failure-not-resolved");
    }

    #[test]
    fn two_unstable_low_realizations_request_a_third_sample() {
        let result = profile_search(
            AutotuneProfile::Fair,
            800_000,
            640_000,
            vec![
                search_observation_with_cpu(752_000, 390_000, 8.0, 96.0),
                search_observation_with_cpu(752_000, 470_000, 8.0, 97.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Test);
        assert_eq!(result.reason, "repeat-low-candidate-realization");
        assert_eq!(result.next_candidate_kbps, Some(752_000));
    }

    #[test]
    fn three_clean_unstable_low_realizations_step_down_for_control() {
        let result = profile_search(
            AutotuneProfile::Fair,
            800_000,
            640_000,
            vec![
                search_observation_with_cpu(752_000, 410_000, 8.0, 96.0),
                search_observation_with_cpu(752_000, 470_000, 8.0, 97.0),
                search_observation_with_cpu(752_000, 440_000, 8.0, 98.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Test);
        assert_eq!(
            result.reason,
            "lower-variable-candidate-to-establish-shaper-control"
        );
        assert_eq!(result.next_candidate_kbps, Some(640_000));
    }

    #[test]
    fn sub_fifty_cellular_evidence_requires_a_lower_controlled_candidate() {
        let result = profile_search(
            AutotuneProfile::Fair,
            800_000,
            640_000,
            vec![
                search_observation(800_000, 300_000, 8.0),
                search_observation(800_000, 380_000, 8.0),
                search_observation(800_000, 470_000, 8.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Test);
        assert_eq!(
            result.reason,
            "lower-variable-candidate-to-establish-shaper-control"
        );
        assert_eq!(result.next_candidate_kbps, Some(640_000));
    }

    #[test]
    fn variable_advisory_requires_the_strict_profile_quality_target() {
        let result = profile_search(
            AutotuneProfile::Gaming,
            800_000,
            560_000,
            vec![
                search_observation(752_000, 410_000, 8.0),
                search_observation(752_000, 470_000, 8.0),
                search_observation(752_000, 440_000, 8.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Inconclusive);
        assert_eq!(result.reason, "low-candidate-realization-not-repeatable");
    }

    #[test]
    fn repeatable_advisory_rejects_an_unsafe_peer() {
        let unsafe_observation = |achieved_kbps| SearchObservation {
            candidate_kbps: 752_000,
            achieved_kbps,
            icmp_delta_ms: 2.0,
            transport_delta_ms: 2.0,
            loss_percent: 10.0,
            cpu_percent: 50.0,
        };
        let result = profile_search(
            AutotuneProfile::Fair,
            800_000,
            640_000,
            vec![
                unsafe_observation(390_000),
                search_observation(752_000, 410_000, 2.0),
                search_observation(752_000, 470_000, 2.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Inconclusive);
        assert_eq!(result.reason, "low-candidate-realization-not-repeatable");
    }

    #[test]
    fn real_variable_cellular_search_retests_a_lower_controlled_candidate() {
        let result = profile_search(
            AutotuneProfile::Fair,
            171_100,
            153_900,
            vec![
                search_observation(160_800, 127_728, 46.9),
                search_observation(160_800, 121_887, 46.9),
                search_observation(171_100, 108_111, 42.8),
                search_observation(171_100, 86_789, 42.8),
                search_observation(171_100, 128_038, 42.8),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Test);
        assert_eq!(
            result.reason,
            "lower-variable-candidate-to-establish-shaper-control"
        );
        assert_eq!(result.next_candidate_kbps, Some(153_900));
    }

    #[test]
    fn third_low_realization_can_establish_a_repeatable_pair() {
        let result = profile_search(
            AutotuneProfile::Fair,
            800_000,
            640_000,
            vec![
                search_observation_with_cpu(752_000, 410_000, 8.0, 96.0),
                search_observation_with_cpu(752_000, 470_000, 8.0, 97.0),
                search_observation_with_cpu(752_000, 422_000, 8.0, 98.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Test);
        assert_eq!(result.reason, "lower-candidate-to-establish-shaper-control");
        assert_eq!(result.next_candidate_kbps, Some(640_000));
    }

    #[test]
    fn stable_profile_maximum_never_blocks_the_observed_low_search_bound() {
        for profile in [
            AutotuneProfile::Gaming,
            AutotuneProfile::BestOverall,
            AutotuneProfile::Fair,
        ] {
            let proposal = build_proposal_for_profile(
                &[902_700.0, 913_100.0],
                &[902_600.0, 909_200.0],
                LatencyBaseline {
                    median_ms: 7.0,
                    p95_ms: 10.4,
                    samples: 13,
                },
                LinkKind::Pppoe,
                profile,
            )
            .unwrap();
            assert!(proposal.download.maximum_kbps >= proposal.download.observed_low_kbps);
            assert!(proposal.upload.maximum_kbps >= proposal.upload.observed_low_kbps);
        }
    }
}
