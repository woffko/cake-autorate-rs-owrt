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

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ethernet => "ethernet",
            Self::Pppoe => "pppoe",
            Self::Cellular => "cellular",
            Self::Unknown => "unknown",
        }
    }
}

/// Physical/service access medium is deliberately separate from `LinkKind`.
/// PPPoE or Ethernet describes encapsulation, not whether the provider-facing
/// path is fibre, cellular, satellite, or a shared wireless hop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessMedium {
    Cellular,
    LeoSatellite,
    GeoSatellite,
    FixedWireless,
    SharedWired,
    Unknown,
}

impl AccessMedium {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "cellular" | "4g" | "5g" => Some(Self::Cellular),
            "leo_satellite" | "leo-satellite" | "leo" => Some(Self::LeoSatellite),
            "geo_satellite" | "geo-satellite" | "geo" | "high_latency_satellite" => {
                Some(Self::GeoSatellite)
            }
            "fixed_wireless" | "fixed-wireless" | "wisp" | "wifi_bridge" => {
                Some(Self::FixedWireless)
            }
            "shared_wired" | "shared-wired" | "shared" => Some(Self::SharedWired),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cellular => "cellular",
            Self::LeoSatellite => "leo_satellite",
            Self::GeoSatellite => "geo_satellite",
            Self::FixedWireless => "fixed_wireless",
            Self::SharedWired => "shared_wired",
            Self::Unknown => "unknown",
        }
    }

    fn variable_exploration_floor(self) -> f64 {
        match self {
            // Radio scheduling and LEO handovers need the deepest bounded
            // search.  The value remains an exploration boundary, never an
            // inferred runtime minimum.
            Self::Cellular | Self::LeoSatellite => 0.35,
            Self::GeoSatellite | Self::FixedWireless => 0.40,
            Self::SharedWired | Self::Unknown => 0.50,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessEvidenceSource {
    UserSelected,
    NetworkProtocol,
    DeviceType,
    InterfaceName,
    AutoInconclusive,
    LegacyDefault,
}

impl AccessEvidenceSource {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "user_selected" => Some(Self::UserSelected),
            "network_protocol" => Some(Self::NetworkProtocol),
            "device_type" => Some(Self::DeviceType),
            "interface_name" => Some(Self::InterfaceName),
            "auto_inconclusive" => Some(Self::AutoInconclusive),
            "legacy_default" => Some(Self::LegacyDefault),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserSelected => "user_selected",
            Self::NetworkProtocol => "network_protocol",
            Self::DeviceType => "device_type",
            Self::InterfaceName => "interface_name",
            Self::AutoInconclusive => "auto_inconclusive",
            Self::LegacyDefault => "legacy_default",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapacityLearningPolicy {
    VerifiedOnly,
    PassiveBounded,
    ScheduledActive,
    FixedCap,
}

impl CapacityLearningPolicy {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "verified_only" | "verified-only" => Some(Self::VerifiedOnly),
            "passive_bounded" | "passive-bounded" | "passive" => Some(Self::PassiveBounded),
            "scheduled_active" | "scheduled-active" | "periodic_active" => {
                Some(Self::ScheduledActive)
            }
            "fixed_cap" | "fixed-cap" | "fixed" => Some(Self::FixedCap),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::VerifiedOnly => "verified_only",
            Self::PassiveBounded => "passive_bounded",
            Self::ScheduledActive => "scheduled_active",
            Self::FixedCap => "fixed_cap",
        }
    }

    fn adaptive_enabled(self) -> bool {
        matches!(self, Self::PassiveBounded | Self::ScheduledActive)
    }
}

pub fn validate_capacity_learning_service_caps(
    policy: Option<CapacityLearningPolicy>,
    download_service_cap_kbps: Option<u64>,
    upload_service_cap_kbps: Option<u64>,
) -> Result<(), String> {
    if policy == Some(CapacityLearningPolicy::FixedCap)
        && (download_service_cap_kbps.is_none() || upload_service_cap_kbps.is_none())
    {
        return Err(
            "fixed-cap capacity learning requires download and upload service hard caps"
                .to_string(),
        );
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProposalContext {
    pub access_medium: Option<AccessMedium>,
    pub access_source: AccessEvidenceSource,
    pub access_confidence_percent: u64,
    pub capacity_learning_policy: Option<CapacityLearningPolicy>,
    pub download_service_cap_kbps: Option<u64>,
    pub upload_service_cap_kbps: Option<u64>,
}

impl Default for ProposalContext {
    fn default() -> Self {
        Self {
            access_medium: None,
            access_source: AccessEvidenceSource::LegacyDefault,
            access_confidence_percent: 0,
            capacity_learning_policy: None,
            download_service_cap_kbps: None,
            upload_service_cap_kbps: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutotuneProfile {
    Gaming,
    GamingExtreme,
    BestOverall,
    VariableLink,
    Fair,
}

impl AutotuneProfile {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "gaming" => Some(Self::Gaming),
            "gaming_extreme" | "gaming-extreme" | "extreme_gaming" => Some(Self::GamingExtreme),
            "best_overall" | "best-overall" | "balanced" => Some(Self::BestOverall),
            "variable_link" | "variable-link" | "variable" => Some(Self::VariableLink),
            "fair" => Some(Self::Fair),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gaming => "gaming",
            Self::GamingExtreme => "gaming_extreme",
            Self::BestOverall => "best_overall",
            Self::VariableLink => "variable_link",
            Self::Fair => "fair",
        }
    }

    pub fn target_grade(self) -> &'static str {
        match self {
            Self::Gaming | Self::GamingExtreme => "A+",
            Self::BestOverall => "A",
            Self::VariableLink => "B",
            Self::Fair => "C",
        }
    }

    pub fn quality_target_required(self) -> bool {
        !matches!(self, Self::Fair)
    }

    pub fn throughput_priority(self) -> bool {
        self == Self::Fair
    }

    pub fn target_delta_ms(self) -> f64 {
        match self {
            Self::Gaming | Self::GamingExtreme => 5.0,
            Self::BestOverall => 30.0,
            Self::VariableLink => 60.0,
            Self::Fair => 200.0,
        }
    }

    pub fn capacity_floor_percent(self) -> f64 {
        match self {
            Self::Gaming | Self::GamingExtreme => 70.0,
            Self::BestOverall => 80.0,
            Self::VariableLink => 70.0,
            Self::Fair => 90.0,
        }
    }

    pub fn objective(self) -> &'static str {
        match self {
            Self::Gaming => "quality-constrained-throughput",
            Self::GamingExtreme => "extreme-a-plus-quality-search",
            Self::BestOverall => "balanced-quality-throughput",
            Self::VariableLink => "variable-link-measured-knee",
            Self::Fair => "throughput-first",
        }
    }

    pub fn validation_thresholds(self) -> ValidationThresholds {
        let (latency_delta_max_ms, loss_max_percent) = match self {
            Self::Gaming | Self::GamingExtreme => (5.0, 1.0),
            Self::BestOverall => (30.0, 3.0),
            Self::VariableLink => (60.0, 3.0),
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

    fn exploration_minimum_factor(
        self,
        variable: bool,
        observed_low_kbps: f64,
        direction: SearchDirection,
    ) -> f64 {
        match (self, variable) {
            // Standard Gaming never explores below its 70% retention
            // objective. Deeper sacrifices require the explicit Extreme A+
            // opt-in and remain manual-only below this boundary.
            (Self::Gaming, _) => 0.70,
            (Self::GamingExtreme, _) => match direction {
                SearchDirection::Download if observed_low_kbps >= 500_000.0 => 0.25,
                SearchDirection::Download if observed_low_kbps >= 100_000.0 => 0.40,
                SearchDirection::Download if observed_low_kbps >= 25_000.0 => 0.55,
                SearchDirection::Download => 0.70,
                SearchDirection::Upload if observed_low_kbps >= 500_000.0 => 0.25,
                SearchDirection::Upload if observed_low_kbps >= 100_000.0 => 0.30,
                SearchDirection::Upload if observed_low_kbps >= 20_000.0 => 0.50,
                SearchDirection::Upload => 0.70,
            },
            (Self::BestOverall, true) => 0.40,
            (Self::BestOverall, false) => 0.70,
            // This minimum is an exploration boundary only. The runtime
            // minimum is accepted later solely from an actually measured
            // controlled CAKE point at the detected latency knee.
            (Self::VariableLink, _) => 0.35,
            (Self::Fair, true) => 0.35,
            // A short cellular calibration can look stable even though the
            // radio scheduler moves materially before shaped validation.  A
            // 35% search/configuration minimum gives the bounded search room
            // to establish an actually enforced CAKE rate; the 90% Fair
            // retention objective still controls unattended Auto-Apply.
            (Self::Fair, false) => 0.35,
        }
    }

    fn latency_thresholds(self, jitter_ms: f64) -> Result<(u64, u64, u64), String> {
        let (adjust_up, delay_threshold, adjust_down) = match self {
            Self::Gaming | Self::GamingExtreme => {
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
            Self::VariableLink => {
                let adjust_up = (jitter_ms * 1.75).clamp(4.0, 18.0).ceil();
                let adjust_up = checked_latency_threshold(adjust_up)?;
                (
                    adjust_up,
                    (adjust_up + 12).max(24),
                    ((adjust_up + 12).max(24) + 30).max(54),
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
            AutotuneProfile::Gaming | AutotuneProfile::GamingExtreme => Self {
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
            AutotuneProfile::BestOverall
            | AutotuneProfile::VariableLink
            | AutotuneProfile::Fair => Self {
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

/// Describes why a direction's configured maximum may be treated as a safe
/// runtime starting point.  A proposal assembled from raw capacity samples is
/// only a candidate: the shell supervisor promotes it to `ShapedValidation`
/// after that exact CAKE rate has passed the directional validation gates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CeilingEvidence {
    UnvalidatedCandidate,
    ShapedValidation,
    RetainedConfiguration,
}

impl CeilingEvidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnvalidatedCandidate => "unvalidated_candidate",
            Self::ShapedValidation => "shaped_validation",
            Self::RetainedConfiguration => "retained_configuration",
        }
    }
}

/// Identifies the upper-bound evidence.  Automatic calibration is deliberately
/// bounded by measured raw capacity; a future user/service-plan hard cap can
/// only tighten that boundary, never silently expand it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CeilingCapSource {
    MeasuredRaw,
    UserServiceLimit,
    RetainedConfiguration,
}

impl CeilingCapSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MeasuredRaw => "measured_raw",
            Self::UserServiceLimit => "user_service_limit",
            Self::RetainedConfiguration => "retained_configuration",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DirectionProposal {
    /// Backward-compatible effective runtime floor.  Before a measured knee is
    /// available this equals `exploration_minimum_kbps`.
    pub minimum_kbps: u64,
    pub exploration_minimum_kbps: u64,
    pub runtime_minimum_kbps: Option<u64>,
    pub base_kbps: u64,
    /// Backward-compatible configured maximum.  A final applyable proposal
    /// must bind it to `tested_safe_maximum_kbps`.
    pub maximum_kbps: u64,
    pub tested_safe_maximum_kbps: Option<u64>,
    pub exploration_cap_kbps: u64,
    pub absolute_cap_kbps: u64,
    pub service_hard_cap_kbps: Option<u64>,
    pub ceiling_evidence: CeilingEvidence,
    pub cap_source: CeilingCapSource,
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
    pub capacity_learning_policy: CapacityLearningPolicy,
    pub access_medium: AccessMedium,
    pub access_source: AccessEvidenceSource,
    pub access_confidence_percent: u64,
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
    fn apply_service_cap(
        name: &str,
        service_cap_kbps: Option<u64>,
        direction: &mut DirectionProposal,
    ) -> Result<(), String> {
        let Some(service_cap_kbps) = service_cap_kbps else {
            return Ok(());
        };
        if !(100..=MAX_RATE_KBPS).contains(&service_cap_kbps) {
            return Err(format!(
                "{name} service hard cap must be between 100 and {MAX_RATE_KBPS} kbit/s"
            ));
        }

        direction.service_hard_cap_kbps = Some(service_cap_kbps);
        if service_cap_kbps <= direction.absolute_cap_kbps {
            direction.absolute_cap_kbps = service_cap_kbps;
            direction.maximum_kbps = direction.maximum_kbps.min(service_cap_kbps);
            direction.base_kbps = direction.base_kbps.min(direction.maximum_kbps);
            direction.exploration_minimum_kbps =
                direction.exploration_minimum_kbps.min(direction.base_kbps);
            direction.minimum_kbps = direction.minimum_kbps.min(direction.base_kbps);
            direction.cap_source = CeilingCapSource::UserServiceLimit;
        }
        Ok(())
    }

    fn set_measured_runtime_minimum(
        profile: AutotuneProfile,
        name: &str,
        measured: u64,
        direction: &mut DirectionProposal,
    ) -> Result<(), String> {
        if !matches!(
            profile,
            AutotuneProfile::VariableLink | AutotuneProfile::GamingExtreme
        ) {
            return Err(
                "measured runtime minimum overrides require variable_link or gaming_extreme"
                    .to_string(),
            );
        }
        if measured < direction.exploration_minimum_kbps || measured > direction.base_kbps {
            return Err(format!(
                "measured {name} runtime minimum must stay between the exploration minimum and selected base"
            ));
        }
        direction.minimum_kbps = measured;
        direction.runtime_minimum_kbps = Some(measured);
        Ok(())
    }

    fn set_tested_safe_maximum(
        name: &str,
        measured: u64,
        direction: &mut DirectionProposal,
    ) -> Result<(), String> {
        // Full Auto-Tune's final proposal is rebuilt with the selected exact
        // candidate as its base.  Requiring equality prevents a caller from
        // blessing an unobserved rate merely because it lies inside a broad
        // numeric interval.
        if measured != direction.base_kbps {
            return Err(format!(
                "tested-safe {name} maximum must equal the exact selected base candidate"
            ));
        }
        if measured < direction.minimum_kbps || measured > direction.exploration_cap_kbps {
            return Err(format!(
                "tested-safe {name} maximum must stay inside the measured exploration bounds"
            ));
        }
        direction.maximum_kbps = measured;
        direction.tested_safe_maximum_kbps = Some(measured);
        direction.ceiling_evidence = CeilingEvidence::ShapedValidation;
        // Intentionally allow absolute_cap_kbps to grow up to exploration_cap
        // when shaped validation confirms a candidate above the previous ceiling.
        // The .max(measured) is deliberate: the caller has already proved this
        // exact rate is safe under load, so it is legitimate to raise the cap.
        // The outer .min(exploration_cap.max(measured)) prevents it from
        // escaping the measured exploration boundary.
        direction.absolute_cap_kbps = direction
            .absolute_cap_kbps
            .max(measured)
            .min(direction.exploration_cap_kbps.max(measured));
        Ok(())
    }

    /// Promote exact, already validated shaped candidates to runtime-safe
    /// maxima.  This is intentionally separate from base-rate selection.
    pub fn set_tested_safe_maximums(
        &mut self,
        download_kbps: Option<u64>,
        upload_kbps: Option<u64>,
    ) -> Result<(), String> {
        if let Some(download_kbps) = download_kbps {
            Self::set_tested_safe_maximum("download", download_kbps, &mut self.download)?;
        }
        if let Some(upload_kbps) = upload_kbps {
            Self::set_tested_safe_maximum("upload", upload_kbps, &mut self.upload)?;
        }
        Ok(())
    }

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

    /// Select an exact pair of CAKE rates for a measurement. Unlike a
    /// base-rate revision, this may move above observed-low up to the measured
    /// exploration maximum. The caller still has to validate the pair under
    /// load before it can become a tested-safe runtime configuration.
    pub fn set_measurement_base_rates(
        &mut self,
        download_kbps: u64,
        upload_kbps: u64,
    ) -> Result<(), String> {
        fn validate(
            name: &str,
            candidate_kbps: u64,
            direction: &DirectionProposal,
        ) -> Result<(), String> {
            if candidate_kbps < direction.exploration_minimum_kbps
                || candidate_kbps > direction.maximum_kbps
            {
                return Err(format!(
                    "{name} measurement base must stay within the measured exploration interval"
                ));
            }
            Ok(())
        }

        // Validate the whole pair before mutating either direction.
        validate("download", download_kbps, &self.download)?;
        validate("upload", upload_kbps, &self.upload)?;
        self.download.base_kbps = download_kbps;
        self.upload.base_kbps = upload_kbps;
        Ok(())
    }

    /// Replace the exploratory floors with runtime minima that were proved by
    /// the directional profile search. The caller must pass exact, previously
    /// tested CAKE candidates; this method only enforces ordering and prevents
    /// a result from escaping the original exploration interval.
    pub fn set_measured_runtime_minimums(
        &mut self,
        download_kbps: u64,
        upload_kbps: u64,
    ) -> Result<(), String> {
        Self::set_measured_runtime_minimum(
            self.profile,
            "download",
            download_kbps,
            &mut self.download,
        )?;
        Self::set_measured_runtime_minimum(self.profile, "upload", upload_kbps, &mut self.upload)?;
        Ok(())
    }

    /// Bind only the still-shaped direction of a one-sided topology to its
    /// exact tested runtime minimum. The bypassed peer direction deliberately
    /// retains its exploratory floor because it is not controlled at runtime.
    pub fn set_measured_download_runtime_minimum(
        &mut self,
        download_kbps: u64,
    ) -> Result<(), String> {
        Self::set_measured_runtime_minimum(
            self.profile,
            "download",
            download_kbps,
            &mut self.download,
        )
    }

    pub fn set_measured_upload_runtime_minimum(&mut self, upload_kbps: u64) -> Result<(), String> {
        Self::set_measured_runtime_minimum(self.profile, "upload", upload_kbps, &mut self.upload)
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
                "{{\"schema_version\":4,\"profile\":\"{}\",\"target_grade\":\"{}\",",
                "\"quality_target_required\":{},\"throughput_priority\":{},",
                "\"download\":{},\"upload\":{},",
                "\"active_threshold_kbps\":{},",
                "\"thresholds_ms\":{{\"adjust_up\":{},\"delay\":{},\"adjust_down\":{}}},",
                "\"adaptive_ceiling\":{{\"enabled\":{},\"hold_s\":{},\"growth_percent\":{},",
                "\"probe_s\":{},\"cooldown_s\":{},\"failed_bound_ttl_s\":{},",
                "\"policy\":\"{}\"}},",
                "\"access\":{{\"medium\":\"{}\",\"source\":\"{}\",",
                "\"confidence_percent\":{}}},",
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
            self.capacity_learning_policy.as_str(),
            self.access_medium.as_str(),
            self.access_source.as_str(),
            self.access_confidence_percent,
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
    pub realized_kbps: u64,
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
    // Falling modestly below the requested candidate is weak evidence on a
    // variable upstream bottleneck, not proof of unsafe queueing.  Keep such
    // results reviewable down to the explicit 50% trust boundary.  Near-zero
    // exercise, overshoot (possible shaper bypass), and packet loss remain
    // fail-closed safety failures.
    let safety_pass = gates
        .iter()
        .filter(|gate| {
            gate.required
                && !gate.code.contains("latency")
                && !matches!(
                    gate.code,
                    "download-candidate-realization" | "upload-candidate-realization"
                )
        })
        .all(|gate| gate.pass)
        && download.candidate_realization_percent >= THROUGHPUT_TRUST_FLOOR_PERCENT
        && upload.candidate_realization_percent >= THROUGHPUT_TRUST_FLOOR_PERCENT;
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
    if input.observed_low_kbps == 0
        || input.candidate_kbps == 0
        || input.realized_kbps == 0
        || input.achieved_kbps == 0
    {
        return Err(format!(
            "{name} observed, candidate, realized, and achieved rates must be positive"
        ));
    }
    for (rate_name, value) in [
        ("observed", input.observed_low_kbps),
        ("candidate", input.candidate_kbps),
        ("realized", input.realized_kbps),
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
        candidate_realization_percent: input.realized_kbps as f64 * 100.0
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

fn adaptive_policy_parameters(
    profile: AutotuneProfile,
    variable: bool,
    access_medium: Option<AccessMedium>,
) -> (u64, u64, u64, u64, u64) {
    if profile == AutotuneProfile::VariableLink {
        if let Some(access_medium) = access_medium {
            return match access_medium {
                AccessMedium::Cellular => (20, 3, 10, 60, 900),
                AccessMedium::LeoSatellite => (30, 2, 15, 120, 1800),
                AccessMedium::GeoSatellite => (45, 1, 20, 180, 3600),
                AccessMedium::FixedWireless => (20, 3, 10, 60, 1200),
                AccessMedium::SharedWired | AccessMedium::Unknown => (30, 2, 10, 90, 1800),
            };
        }
    }

    let hold = match profile {
        AutotuneProfile::Gaming | AutotuneProfile::GamingExtreme => 30,
        AutotuneProfile::BestOverall => {
            if variable {
                15
            } else {
                20
            }
        }
        AutotuneProfile::VariableLink => 12,
        AutotuneProfile::Fair => 10,
    };
    let growth = match profile {
        AutotuneProfile::Gaming | AutotuneProfile::GamingExtreme => 1,
        AutotuneProfile::BestOverall => 3,
        AutotuneProfile::VariableLink => 3,
        AutotuneProfile::Fair => 5,
    };
    let probe = match profile {
        AutotuneProfile::Fair => 10,
        AutotuneProfile::Gaming
        | AutotuneProfile::GamingExtreme
        | AutotuneProfile::BestOverall
        | AutotuneProfile::VariableLink => 8,
    };
    let cooldown = match profile {
        AutotuneProfile::Gaming | AutotuneProfile::GamingExtreme => 90,
        AutotuneProfile::BestOverall => {
            if variable {
                45
            } else {
                60
            }
        }
        AutotuneProfile::VariableLink => 45,
        AutotuneProfile::Fair => 30,
    };
    let ttl = match profile {
        AutotuneProfile::Gaming | AutotuneProfile::GamingExtreme => 1800,
        AutotuneProfile::BestOverall => {
            if variable {
                900
            } else {
                1800
            }
        }
        AutotuneProfile::VariableLink => 900,
        AutotuneProfile::Fair => 600,
    };
    (hold, growth, probe, cooldown, ttl)
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

#[cfg(test)]
pub fn build_proposal_for_profile(
    download_samples_kbps: &[f64],
    upload_samples_kbps: &[f64],
    baseline: LatencyBaseline,
    link_kind: LinkKind,
    profile: AutotuneProfile,
) -> Result<AutotuneProposal, String> {
    build_proposal_for_profile_with_context(
        download_samples_kbps,
        upload_samples_kbps,
        baseline,
        link_kind,
        profile,
        ProposalContext::default(),
    )
}

pub fn build_proposal_for_profile_with_context(
    download_samples_kbps: &[f64],
    upload_samples_kbps: &[f64],
    baseline: LatencyBaseline,
    link_kind: LinkKind,
    profile: AutotuneProfile,
    context: ProposalContext,
) -> Result<AutotuneProposal, String> {
    validate_throughput_samples("download", download_samples_kbps)?;
    validate_throughput_samples("upload", upload_samples_kbps)?;
    validate_latency_baseline(baseline)?;
    if context.access_confidence_percent > 100 {
        return Err("access-medium confidence must be between 0 and 100".to_string());
    }
    validate_capacity_learning_service_caps(
        context.capacity_learning_policy,
        context.download_service_cap_kbps,
        context.upload_service_cap_kbps,
    )?;
    let mut download = propose_direction(
        download_samples_kbps,
        profile,
        SearchDirection::Download,
        context.access_medium,
    )?;
    let mut upload = propose_direction(
        upload_samples_kbps,
        profile,
        SearchDirection::Upload,
        context.access_medium,
    )?;
    AutotuneProposal::apply_service_cap(
        "download",
        context.download_service_cap_kbps,
        &mut download,
    )?;
    AutotuneProposal::apply_service_cap("upload", context.upload_service_cap_kbps, &mut upload)?;
    let variable = download.variability >= 0.15 || upload.variability >= 0.15;
    let jitter_ms = (baseline.p95_ms - baseline.median_ms).max(0.0);
    let (adjust_up_threshold_ms, delay_threshold_ms, adjust_down_threshold_ms) =
        profile.latency_thresholds(jitter_ms)?;
    // Activity detection must stay well below the weakest observed direction.
    // Using a percentage of the proposed minimum is too high when one
    // direction looked stable during a short, otherwise variable calibration.
    let smallest_observed = download.observed_low_kbps.min(upload.observed_low_kbps);
    let smallest_minimum = download.minimum_kbps.min(upload.minimum_kbps);
    let active_threshold_kbps = checked_rounded_rate(smallest_observed as f64 / 10.0)?
        .clamp(1, 20_000)
        .min(smallest_minimum);
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
    if matches!(
        profile,
        AutotuneProfile::Gaming | AutotuneProfile::GamingExtreme
    ) {
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
    if profile == AutotuneProfile::VariableLink {
        warnings.push(match context.access_medium {
            Some(AccessMedium::Cellular | AccessMedium::LeoSatellite) | None =>
                "Variable link may explore down to 35% of the conservative raw reference. Its runtime minimum is not trusted until Full Auto-Tune proves an actual CAKE-controlled latency knee.",
            Some(AccessMedium::GeoSatellite | AccessMedium::FixedWireless) =>
                "Variable link may explore down to 40% of the conservative raw reference for this access medium. Its runtime minimum is accepted only from a tested CAKE-controlled latency knee.",
            Some(AccessMedium::SharedWired | AccessMedium::Unknown) =>
                "Variable link keeps a 50% exploration floor because this shared or unknown access medium cannot safely justify a deeper automatic search.",
        });
        if context.access_source == AccessEvidenceSource::AutoInconclusive {
            warnings.push(
                "Access-medium auto-detection was inconclusive. Ethernet and PPPoE do not prove the provider medium; review the Variable Link access choice before applying scheduled learning.",
            );
        }
        if context.capacity_learning_policy == Some(CapacityLearningPolicy::ScheduledActive) {
            warnings.push(
                "Scheduled active capacity learning generates substantial download and upload traffic. Configure explicit daily and monthly traffic budgets before enabling unattended runs.",
            );
        }
    }
    if profile == AutotuneProfile::GamingExtreme {
        warnings.push(
            "Extreme A+ search may test a wide link as low as 25% of its conservative raw reference. Results below 70% retention are manual-only and are not recommended for continuous household use.",
        );
        warnings.push(
            "Use Extreme A+ only for a time-limited latency-critical session after reviewing the measured throughput sacrifice; ordinary Gaming keeps the 70% exploration boundary.",
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

    let default_adaptive = variable || profile == AutotuneProfile::VariableLink;
    let capacity_learning_policy =
        context
            .capacity_learning_policy
            .unwrap_or(if default_adaptive {
                CapacityLearningPolicy::PassiveBounded
            } else {
                CapacityLearningPolicy::VerifiedOnly
            });
    let (
        adaptive_hold_s,
        adaptive_growth_percent,
        adaptive_probe_s,
        adaptive_cooldown_s,
        adaptive_failed_bound_ttl_s,
    ) = adaptive_policy_parameters(profile, variable, context.access_medium);

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
        adaptive_ceiling_enabled: capacity_learning_policy.adaptive_enabled(),
        adaptive_hold_s,
        adaptive_growth_percent,
        adaptive_probe_s,
        adaptive_cooldown_s,
        adaptive_failed_bound_ttl_s,
        capacity_learning_policy,
        access_medium: context.access_medium.unwrap_or(AccessMedium::Unknown),
        access_source: context.access_source,
        access_confidence_percent: context.access_confidence_percent,
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
    direction: SearchDirection,
    access_medium: Option<AccessMedium>,
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

    let mut minimum_factor = profile.exploration_minimum_factor(variable, low, direction);
    if profile == AutotuneProfile::VariableLink {
        if let Some(access_medium) = access_medium {
            minimum_factor = access_medium.variable_exploration_floor();
        }
    }
    let minimum = checked_rounded_rate(low * minimum_factor)?;
    // Profile policy may choose how deeply to explore, but it must never
    // manufacture an unconditional initial haircut. The conservative raw low
    // is the starting/base candidate and measured raw high is the exploration
    // cap. A later shaped search either proves the highest passing candidate
    // or records an explicit lower tested point; retention remains an
    // Auto-Apply/Review objective, not a rate multiplier.
    let base = checked_rounded_rate(low)?.max(minimum);
    let maximum = checked_rounded_rate(high)?.max(base);
    let absolute_cap = maximum;

    Ok(DirectionProposal {
        minimum_kbps: minimum,
        exploration_minimum_kbps: minimum,
        runtime_minimum_kbps: None,
        base_kbps: base,
        maximum_kbps: maximum,
        tested_safe_maximum_kbps: None,
        exploration_cap_kbps: absolute_cap,
        absolute_cap_kbps: absolute_cap,
        service_hard_cap_kbps: None,
        ceiling_evidence: CeilingEvidence::UnvalidatedCandidate,
        cap_source: CeilingCapSource::MeasuredRaw,
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
        .min(direction.exploration_cap_kbps);
    direction.base_kbps = rounded_rate(direction.base_kbps as f64 * scale)
        .max(direction.minimum_kbps)
        .min(upper.max(direction.minimum_kbps));
    if direction.ceiling_evidence == CeilingEvidence::UnvalidatedCandidate {
        direction.maximum_kbps = direction.maximum_kbps.max(direction.base_kbps);
    }
}

fn constrain_direction(
    direction: &mut DirectionProposal,
    retained: Option<DirectionProposal>,
    confirmed_max: Option<u64>,
    confirmed_cap: Option<u64>,
) {
    if let Some(retained) = retained {
        *direction = retained;
        direction.ceiling_evidence = CeilingEvidence::RetainedConfiguration;
        direction.cap_source = CeilingCapSource::RetainedConfiguration;
    }

    let cap_bound = confirmed_cap.filter(|value| *value > 0).or(confirmed_max);
    if let Some(cap) = cap_bound {
        direction.absolute_cap_kbps = direction.absolute_cap_kbps.min(cap);
        direction.exploration_cap_kbps = direction.exploration_cap_kbps.min(cap);
        direction.maximum_kbps = direction.maximum_kbps.min(direction.absolute_cap_kbps);
        direction.tested_safe_maximum_kbps = direction
            .tested_safe_maximum_kbps
            .map(|value| value.min(direction.maximum_kbps));
        direction.base_kbps = direction.base_kbps.min(direction.maximum_kbps);
        direction.minimum_kbps = direction.minimum_kbps.min(direction.base_kbps);
        direction.exploration_minimum_kbps = direction
            .exploration_minimum_kbps
            .min(direction.minimum_kbps);
        direction.runtime_minimum_kbps = direction
            .runtime_minimum_kbps
            .map(|value| value.min(direction.base_kbps));
    }
    if let Some(maximum) = confirmed_max.filter(|value| *value > 0) {
        direction.maximum_kbps = direction.maximum_kbps.min(maximum);
        direction.base_kbps = direction.base_kbps.min(direction.maximum_kbps);
        direction.minimum_kbps = direction.minimum_kbps.min(direction.base_kbps);
        direction.tested_safe_maximum_kbps = direction
            .tested_safe_maximum_kbps
            .map(|value| value.min(direction.maximum_kbps));
        direction.absolute_cap_kbps = direction.absolute_cap_kbps.max(direction.maximum_kbps);
        direction.exploration_cap_kbps = direction.exploration_cap_kbps.max(direction.maximum_kbps);
    }
}

fn direction_json(direction: DirectionProposal) -> String {
    format!(
        concat!(
            "{{\"minimum_kbps\":{},\"exploration_minimum_kbps\":{},",
            "\"runtime_minimum_kbps\":{},\"base_kbps\":{},\"maximum_kbps\":{},",
            "\"tested_safe_maximum_kbps\":{},\"exploration_cap_kbps\":{},",
            "\"absolute_cap_kbps\":{},\"service_hard_cap_kbps\":{},",
            "\"ceiling_evidence\":\"{}\",\"cap_source\":\"{}\",",
            "\"observed_low_kbps\":{},",
            "\"observed_median_kbps\":{},\"observed_high_kbps\":{},",
            "\"variability\":{:.4}}}"
        ),
        direction.minimum_kbps,
        direction.exploration_minimum_kbps,
        optional_u64_json(direction.runtime_minimum_kbps),
        direction.base_kbps,
        direction.maximum_kbps,
        optional_u64_json(direction.tested_safe_maximum_kbps),
        direction.exploration_cap_kbps,
        direction.absolute_cap_kbps,
        optional_u64_json(direction.service_hard_cap_kbps),
        direction.ceiling_evidence.as_str(),
        direction.cap_source.as_str(),
        direction.observed_low_kbps,
        direction.observed_median_kbps,
        direction.observed_high_kbps,
        direction.variability,
    )
}

fn optional_u64_json(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string())
}

fn json_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

pub const MAX_PROFILE_SEARCH_OBSERVATIONS: usize = 12;
pub const MAX_PROFILE_REVIEW_OPTIONS: usize = 3;
pub const PHYSICAL_CAPACITY_BELOW_CAKE_CANDIDATE_REVIEW_REASON: &str =
    "physical-capacity-below-cake-candidate-review";
const MAX_SAME_CANDIDATE_OBSERVATIONS: usize = 3;
const VARIABLE_LINK_EXPLORATION_STEP_PERCENT: f64 = 15.0;
const VARIABLE_LINK_PLATEAU_MINIMUM_MS: f64 = 3.0;

/// Return the next exact Variable Link candidate after a candidate produced no
/// admissible loaded observation at all.  This uses the same fixed step as the
/// measured Variable Link descent, but deliberately does not claim that the
/// failed candidate was measured.  `None` means that the authorized
/// exploration floor has already been reached.
pub fn next_variable_link_unobserved_candidate(
    observed_low_kbps: u64,
    minimum_kbps: u64,
    current_kbps: u64,
) -> Result<Option<u64>, String> {
    if observed_low_kbps == 0
        || observed_low_kbps > MAX_RATE_KBPS
        || minimum_kbps == 0
        || minimum_kbps > current_kbps
        || current_kbps > MAX_RATE_KBPS
    {
        return Err("variable-link unobserved search bounds are invalid".to_string());
    }
    let step = rounded_search_rate(
        observed_low_kbps as f64 * VARIABLE_LINK_EXPLORATION_STEP_PERCENT / 100.0,
    );
    let next = current_kbps.saturating_sub(step).max(minimum_kbps);
    Ok((next < current_kbps).then_some(next))
}

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
    pub realized_kbps: u64,
    pub achieved_kbps: u64,
    pub icmp_delta_ms: f64,
    pub transport_delta_ms: f64,
    /// The transport value is a verified lower bound from an exhausted
    /// deadline, not an exact RTT sample.
    pub transport_censored: bool,
    pub loss_percent: f64,
    pub cpu_percent: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SearchObservationMetrics {
    pub realization_percent: f64,
    pub retention_percent: f64,
    pub effective_delta_ms: f64,
    pub grade: &'static str,
    pub transport_censored: bool,
    pub measurement_reliable: bool,
    pub manual_reviewable: bool,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileSearchOptionRole {
    Recommended,
    QualityFirst,
    ThroughputFirst,
    BalancedAlternative,
}

impl ProfileSearchOptionRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recommended => "recommended",
            Self::QualityFirst => "quality_first",
            Self::ThroughputFirst => "throughput_first",
            Self::BalancedAlternative => "balanced_alternative",
        }
    }
}

/// One exact direction-level candidate retained from measured search
/// evidence.  It is deliberately not an apply contract: a download and
/// upload candidate must still be combined and re-measured as one exact pair
/// before any whole-link option can be offered to LuCI.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProfileSearchOption {
    pub role: ProfileSearchOptionRole,
    pub selected_index: usize,
    pub candidate_kbps: u64,
    pub conservative_achieved_kbps: u64,
    pub worst_delta_ms: f64,
    pub grade: &'static str,
    pub transport_censored: bool,
    pub controlled: bool,
    pub manual_reviewable: bool,
    pub target_met: bool,
    pub capacity_objective_met: bool,
    pub auto_apply_candidate: bool,
}

/// One unconfirmed whole-link coordinate assembled only from exact measured
/// direction candidates.  It becomes an operator-visible option solely after
/// the exact pair receives its own pair-confirmation measurement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfilePairCandidate {
    pub role: ProfileSearchOptionRole,
    pub download_index: usize,
    pub upload_index: usize,
    pub download_kbps: u64,
    pub upload_kbps: u64,
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
    pub observed_low_kbps: u64,
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
    pub exploration_minimum_kbps: u64,
    pub runtime_minimum_index: Option<usize>,
    pub knee_detected: bool,
    pub knee_confidence_percent: u64,
    pub plateau_improvement_ms: Option<f64>,
    pub plateau_threshold_ms: f64,
    pub repeat_count: usize,
    pub no_cake_effect: bool,
    pub noisy: bool,
}

impl ProfileSearchResult {
    pub fn review_options(&self) -> Vec<ProfileSearchOption> {
        profile_search_review_options(self)
    }

    pub fn physical_capacity_limited_review_for(&self, candidate_kbps: u64) -> bool {
        self.action == ProfileSearchAction::Fallback
            && self.reason == PHYSICAL_CAPACITY_BELOW_CAKE_CANDIDATE_REVIEW_REASON
            && self.selected_index.is_some_and(|index| {
                self.observations
                    .get(index)
                    .is_some_and(|observation| observation.candidate_kbps == candidate_kbps)
                    && self
                        .metrics
                        .get(index)
                        .is_some_and(|metrics| metrics.manual_reviewable && !metrics.safety_pass)
            })
    }

    pub fn to_json(&self) -> String {
        let selected = self.selected_index.map_or_else(
            || "null".to_string(),
            |index| {
                let observation = self.observations[index];
                let metrics = self.metrics[index];
                format!(
                    concat!(
                        "{{\"index\":{},\"candidate_kbps\":{},\"realized_kbps\":{},\"achieved_kbps\":{},",
                        "\"realization_percent\":{:.3},\"retention_percent\":{:.3},",
                        "\"effective_delta_ms\":{:.3},\"transport_censored\":{},",
                        "\"loss_percent\":{:.3},\"cpu_percent\":{:.3},",
                        "\"grade\":\"{}\",\"safety_pass\":{},\"manual_reviewable\":{},",
                        "\"capacity_objective_met\":{},\"target_met\":{}}}"
                    ),
                    index + 1,
                    observation.candidate_kbps,
                    observation.realized_kbps,
                    observation.achieved_kbps,
                    metrics.realization_percent,
                    metrics.retention_percent,
                    metrics.effective_delta_ms,
                    observation.transport_censored,
                    observation.loss_percent,
                    observation.cpu_percent,
                    metrics.grade,
                    metrics.safety_pass,
                    metrics.manual_reviewable,
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
                        "{{\"index\":{},\"candidate_kbps\":{},\"realized_kbps\":{},\"achieved_kbps\":{},",
                        "\"realization_percent\":{:.3},\"retention_percent\":{:.3},",
                        "\"effective_delta_ms\":{:.3},\"grade\":\"{}\",",
                        "\"transport_censored\":{},",
                        "\"loss_percent\":{:.3},\"cpu_percent\":{:.3},",
                        "\"measurement_reliable\":{},\"manual_reviewable\":{},\"resource_safe\":{},",
                        "\"safety_pass\":{},\"capacity_objective_met\":{},",
                        "\"target_met\":{},\"balanced_score\":{:.3}}}"
                    ),
                    index + 1,
                    observation.candidate_kbps,
                    observation.realized_kbps,
                    observation.achieved_kbps,
                    metrics.realization_percent,
                    metrics.retention_percent,
                    metrics.effective_delta_ms,
                    metrics.grade,
                    metrics.transport_censored,
                    observation.loss_percent,
                    observation.cpu_percent,
                    metrics.measurement_reliable,
                    metrics.manual_reviewable,
                    metrics.resource_safe,
                    metrics.safety_pass,
                    metrics.capacity_objective_met,
                    metrics.target_met,
                    metrics.balanced_score,
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let review_options = self
            .review_options()
            .into_iter()
            .map(|option| {
                format!(
                    concat!(
                        "{{\"role\":\"{}\",\"observation_index\":{},",
                        "\"candidate_kbps\":{},\"conservative_achieved_kbps\":{},",
                        "\"worst_delta_ms\":{:.3},\"grade\":\"{}\",",
                        "\"transport_censored\":{},",
                        "\"controlled\":{},\"manual_reviewable\":{},",
                        "\"target_met\":{},\"capacity_objective_met\":{},",
                        "\"auto_apply_candidate\":{},",
                        "\"direction_candidate_only\":true,",
                        "\"pair_confirmation_required\":true}}"
                    ),
                    option.role.as_str(),
                    option.selected_index + 1,
                    option.candidate_kbps,
                    option.conservative_achieved_kbps,
                    option.worst_delta_ms,
                    option.grade,
                    option.transport_censored,
                    option.controlled,
                    option.manual_reviewable,
                    option.target_met,
                    option.capacity_objective_met,
                    option.auto_apply_candidate,
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let optional_rate = |value: Option<u64>| {
            value.map_or_else(|| "null".to_string(), |value| value.to_string())
        };
        format!(
            concat!(
                "{{\"schema_version\":4,\"profile\":\"{}\",\"direction\":\"{}\",",
                "\"observed_low_kbps\":{},",
                "\"objective\":\"{}\",\"target_grade\":\"{}\",",
                "\"capacity_floor_percent\":{:.1},\"capacity_objective_percent\":{:.1},",
                "\"retention_objective_percent\":{:.1},",
                "\"throughput_safety_floor_percent\":{:.1},",
                "\"exploration_minimum_kbps\":{},\"runtime_minimum_kbps\":{},",
                "\"runtime_minimum_observation_index\":{},\"knee_detected\":{},",
                "\"knee_confidence_percent\":{},\"plateau_improvement_ms\":{},",
                "\"plateau_threshold_ms\":{:.3},\"repeat_count\":{},",
                "\"target_met\":{},\"review_required\":{},",
                "\"no_cake_effect\":{},\"noisy\":{},\"inconclusive\":{},",
                "\"action\":\"{}\",",
                "\"reason\":\"{}\",\"next_candidate_kbps\":{},",
                "\"selected\":{},\"bounds\":{{\"lower_target_pass_kbps\":{},",
                "\"upper_target_fail_kbps\":{},\"resolution_kbps\":{}}},",
                "\"attempts\":{},\"max_attempts\":{},",
                "\"review_options\":[{}],\"evaluated\":[{}]}}"
            ),
            self.profile.as_str(),
            self.direction.as_str(),
            self.observed_low_kbps,
            self.profile.objective(),
            self.profile.target_grade(),
            self.profile.capacity_floor_percent(),
            self.profile.capacity_floor_percent(),
            self.profile.capacity_floor_percent(),
            THROUGHPUT_TRUST_FLOOR_PERCENT,
            self.exploration_minimum_kbps,
            optional_rate(
                self.runtime_minimum_index
                    .map(|index| self.observations[index].candidate_kbps),
            ),
            self.runtime_minimum_index
                .map_or_else(|| "null".to_string(), |index| (index + 1).to_string()),
            self.knee_detected,
            self.knee_confidence_percent,
            self.plateau_improvement_ms
                .map_or_else(|| "null".to_string(), |value| format!("{value:.3}")),
            self.plateau_threshold_ms,
            self.repeat_count,
            self.selected_index
                .map(|index| self.metrics[index].target_met)
                .unwrap_or(false),
            self.action != ProfileSearchAction::Complete,
            self.no_cake_effect,
            self.noisy,
            self.action == ProfileSearchAction::Inconclusive,
            self.action.as_str(),
            self.reason,
            optional_rate(self.next_candidate_kbps),
            selected,
            optional_rate(self.lower_target_pass_kbps),
            optional_rate(self.upper_target_fail_kbps),
            self.resolution_kbps,
            self.observations.len(),
            self.max_attempts,
            review_options,
            evaluated,
        )
    }
}

fn validate_profile_search_input(input: &ProfileSearchInput) -> Result<(), String> {
    if input.observed_low_kbps == 0 || input.observed_low_kbps > MAX_RATE_KBPS {
        return Err("search observed-low rate is outside the supported range".to_string());
    }
    // `observed_low_kbps` is achieved payload, while `upper_kbps` is an
    // applied CAKE candidate.  Protocol overhead and backend behaviour mean a
    // healthy candidate can legitimately be greater than achieved payload.
    // Comparing the two as the same quantity caused a passing retained CAKE
    // ceiling to be discarded and penalised a second time by profile factors.
    if input.minimum_kbps == 0
        || input.minimum_kbps > input.upper_kbps
        || input.upper_kbps > MAX_RATE_KBPS
    {
        return Err(
            "search bounds must satisfy 0 < minimum <= upper <= global maximum".to_string(),
        );
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
            || observation.realized_kbps == 0
            || observation.realized_kbps > MAX_RATE_KBPS
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
        observation.realized_kbps as f64 * 100.0 / observation.candidate_kbps as f64;
    let retention_percent =
        observation.achieved_kbps as f64 * 100.0 / input.observed_low_kbps as f64;
    let effective_delta_ms = observation
        .icmp_delta_ms
        .max(observation.transport_delta_ms);
    let grade = classify_quality(Some(effective_delta_ms)).as_str();
    let measurement_reliable = !observation.transport_censored
        && realization_percent >= input.thresholds.candidate_realization_min_percent
        && realization_percent <= input.thresholds.candidate_realization_max_percent;
    let resource_safe = observation.loss_percent <= input.thresholds.loss_max_percent;
    // A clean 50-80% realization is not proof that CAKE controlled the
    // bottleneck, so it must never satisfy safety_pass or Auto-Apply.  It is,
    // however, bounded enough to retain as explicit manual-review evidence
    // after the search has tried a lower, exact CAKE rate.
    let manual_reviewable = realization_percent >= THROUGHPUT_TRUST_FLOOR_PERCENT
        && realization_percent <= input.thresholds.candidate_realization_max_percent
        && retention_percent >= THROUGHPUT_TRUST_FLOOR_PERCENT
        && resource_safe
        && (observation.transport_censored
            || realization_percent < input.thresholds.candidate_realization_min_percent);
    // Historical retention is not a safety signal on a variable radio link,
    // but candidate realization is: CAKE cannot control a bottleneck below
    // its configured rate.  Only a sufficiently exercised candidate may be
    // selected, even for explicit manual review.
    let safety_pass = measurement_reliable && resource_safe;
    let capacity_objective_met =
        retention_percent >= input.thresholds.capacity_retention_min_percent;
    // Grade boundaries are exclusive at A+/A/B/C, matching the runtime
    // classifier exactly.  A 5.000 ms increase is A, not A+.
    let target_met =
        !observation.transport_censored && effective_delta_ms < input.profile.target_delta_ms();
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
        transport_censored: observation.transport_censored,
        measurement_reliable,
        manual_reviewable,
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

fn best_no_cake_effect_index(
    observations: &[SearchObservation],
    metrics: &[SearchObservationMetrics],
) -> Option<usize> {
    observations
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            metrics[*index].safety_pass
                && metrics[*index].target_met
                && metrics[*index].retention_percent >= THROUGHPUT_TRUST_FLOOR_PERCENT
        })
        .max_by(|(left_index, left), (right_index, right)| {
            left.achieved_kbps.cmp(&right.achieved_kbps).then_with(|| {
                metrics[*right_index]
                    .effective_delta_ms
                    .total_cmp(&metrics[*left_index].effective_delta_ms)
            })
        })
        .map(|(index, _)| index)
}

fn best_trusted_quality_index(
    observations: &[SearchObservation],
    metrics: &[SearchObservationMetrics],
) -> Option<usize> {
    observations
        .iter()
        .enumerate()
        .filter(|(index, _)| {
            metrics[*index].safety_pass
                && metrics[*index].retention_percent >= THROUGHPUT_TRUST_FLOOR_PERCENT
        })
        .min_by(|(left_index, left), (right_index, right)| {
            quality_grade_rank(metrics[*left_index].grade)
                .cmp(&quality_grade_rank(metrics[*right_index].grade))
                .then_with(|| {
                    metrics[*left_index]
                        .effective_delta_ms
                        .total_cmp(&metrics[*right_index].effective_delta_ms)
                })
                .then_with(|| right.achieved_kbps.cmp(&left.achieved_kbps))
        })
        .map(|(index, _)| index)
}

fn best_variable_operating_ceiling_index(
    observations: &[SearchObservation],
    metrics: &[SearchObservationMetrics],
) -> Option<usize> {
    // Variable Link may descend past an already acceptable operating point to
    // learn a distinct runtime minimum. Noise during that exploration must not
    // turn extra quality above the requested grade into an implicit throughput
    // haircut: retain the highest-throughput safe target-passing observation.
    // The quality-first selector remains the explicit fallback only when no
    // tested observation met the requested class.
    best_target_index(observations, metrics)
        .or_else(|| best_trusted_quality_index(observations, metrics))
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

/// Select the best exact observation that passed the topology-independent
/// search safety contract.  A terminal probe may be noisy, uncontrolled, or
/// unsafe without invalidating an earlier controlled measurement.  Terminal
/// search paths use this selector before declaring the whole direction
/// inconclusive, so a tested safe point remains available for explicit review.
fn best_safe_fallback_index(
    input: &ProfileSearchInput,
    metrics: &[SearchObservationMetrics],
) -> Option<usize> {
    match input.profile {
        AutotuneProfile::Gaming | AutotuneProfile::GamingExtreme => {
            best_target_index(&input.observations, metrics)
                .or_else(|| best_quality_index(&input.observations, metrics))
        }
        AutotuneProfile::BestOverall | AutotuneProfile::VariableLink => {
            best_target_index(&input.observations, metrics)
                .or_else(|| best_balanced_index(&input.observations, metrics))
        }
        AutotuneProfile::Fair => best_fair_index(input, metrics),
    }
}

fn resolve_terminal_safe_fallback(
    input: &ProfileSearchInput,
    metrics: &[SearchObservationMetrics],
    action: ProfileSearchAction,
    selected_index: Option<usize>,
) -> (ProfileSearchAction, Option<usize>) {
    if action != ProfileSearchAction::Inconclusive {
        return (action, selected_index);
    }
    let selected_index = selected_index
        .filter(|index| metrics[*index].safety_pass)
        .or_else(|| best_safe_fallback_index(input, metrics));
    if selected_index.is_some() {
        (ProfileSearchAction::Fallback, selected_index)
    } else {
        (ProfileSearchAction::Inconclusive, None)
    }
}

/// Select a bounded Review-only point whose transport latency is a verified
/// lower bound rather than an exact RTT.  Each such observation already
/// represents the capture-level minimum of three attested deadline flights;
/// it must never become a target pass or an Auto-Apply candidate.
fn best_censored_review_index(
    input: &ProfileSearchInput,
    metrics: &[SearchObservationMetrics],
) -> Option<usize> {
    input
        .observations
        .iter()
        .enumerate()
        .filter(|(index, observation)| {
            observation.transport_censored && metrics[*index].manual_reviewable
        })
        .max_by(|(left_index, left), (right_index, right)| {
            left.achieved_kbps
                .cmp(&right.achieved_kbps)
                .then_with(|| left.candidate_kbps.cmp(&right.candidate_kbps))
                .then_with(|| {
                    metrics[*right_index]
                        .effective_delta_ms
                        .total_cmp(&metrics[*left_index].effective_delta_ms)
                })
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

fn rate_ratio_within(
    numerator_kbps: u64,
    denominator_kbps: u64,
    minimum_percent: f64,
    maximum_percent: f64,
) -> bool {
    denominator_kbps > 0
        && (numerator_kbps as f64 * 100.0 / denominator_kbps as f64) >= minimum_percent
        && (numerator_kbps as f64 * 100.0 / denominator_kbps as f64) <= maximum_percent
}

/// A terminal Variable-link probe may lose its lower retest when the radio
/// capacity changes underneath the exact measurement window.  In that narrow
/// case, preserve a prior *configured and measured* CAKE ceiling only when two
/// observations independently prove that CAKE wire bytes tracked backend
/// goodput even though the physical bottleneck sat below the configured rate.
///
/// This is deliberately not part of ordinary search evaluation.  It cannot
/// satisfy `safety_pass`, cannot establish a runtime minimum, and cannot be
/// selected until the lower exact retest has terminated at a measured boundary.
fn physical_capacity_limited_review_indices(
    input: &ProfileSearchInput,
    metrics: &[SearchObservationMetrics],
) -> Option<Vec<usize>> {
    if input.profile != AutotuneProfile::VariableLink {
        return None;
    }

    let mut candidates = input
        .observations
        .iter()
        .map(|observation| observation.candidate_kbps)
        .collect::<Vec<_>>();
    candidates.sort_unstable();
    candidates.dedup();

    candidates
        .into_iter()
        .filter_map(|candidate_kbps| {
            let indices = input
                .observations
                .iter()
                .enumerate()
                .filter(|(_, observation)| observation.candidate_kbps == candidate_kbps)
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            if indices.len() < 2
                || candidate_kbps <= input.minimum_kbps
                || indices.iter().any(|index| {
                    let observation = input.observations[*index];
                    let metric = metrics[*index];
                    observation.transport_censored
                        || !metric.resource_safe
                        || !metric.target_met
                        || observation.cpu_percent > input.thresholds.cpu_max_percent
                        || metric.realization_percent
                            >= input.thresholds.candidate_realization_min_percent
                        || metric.realization_percent
                            > input.thresholds.candidate_realization_max_percent
                        || observation.realized_kbps < input.minimum_kbps
                        || !rate_ratio_within(
                            observation.realized_kbps,
                            observation.achieved_kbps,
                            input.thresholds.candidate_realization_min_percent,
                            input.thresholds.candidate_realization_max_percent,
                        )
                })
            {
                return None;
            }

            let repeatable_indices = indices
                .iter()
                .copied()
                .filter(|left| {
                    indices.iter().copied().any(|right| {
                        left != &right
                            && achieved_rates_repeatable(
                                input.observations[*left].achieved_kbps,
                                input.observations[right].achieved_kbps,
                            )
                            && achieved_rates_repeatable(
                                input.observations[*left].realized_kbps,
                                input.observations[right].realized_kbps,
                            )
                    })
                })
                .collect::<Vec<_>>();
            (repeatable_indices.len() >= 2).then_some(repeatable_indices)
        })
        .max_by(|left, right| {
            let left_achieved = left
                .iter()
                .map(|index| input.observations[*index].achieved_kbps)
                .min()
                .unwrap_or(0);
            let right_achieved = right
                .iter()
                .map(|index| input.observations[*index].achieved_kbps)
                .min()
                .unwrap_or(0);
            left_achieved.cmp(&right_achieved).then_with(|| {
                input.observations[left[0]]
                    .candidate_kbps
                    .cmp(&input.observations[right[0]].candidate_kbps)
            })
        })
}

fn low_realization_evidence_eligible(
    profile: AutotuneProfile,
    metrics: SearchObservationMetrics,
) -> bool {
    metrics.resource_safe
        && (profile == AutotuneProfile::Fair
            || metrics.target_met
            || (profile == AutotuneProfile::VariableLink
                && metrics.manual_reviewable
                && metrics.effective_delta_ms <= 200.0))
}

/// Decide whether a low-realization observation may contribute only to the
/// next exact diagnostic candidate.  This is deliberately weaker than manual
/// Review eligibility for Variable Link: a clean, repeatable point below the
/// 50% trust floor may show that the physical bottleneck sits far below the
/// requested CAKE rate, but it must never itself become selectable evidence.
fn low_realization_descent_evidence_eligible(
    input: &ProfileSearchInput,
    metrics: &[SearchObservationMetrics],
    index: usize,
) -> bool {
    let metric = metrics[index];
    let observation = input.observations[index];
    if input.profile == AutotuneProfile::VariableLink
        && metric.realization_percent < THROUGHPUT_TRUST_FLOOR_PERCENT
    {
        // This observation can never become a selectable result.  It is used
        // only to choose a lower, exact diagnostic rate which is measured
        // again.  CPU saturation is therefore a reason to descend, not a
        // reason to repeat the same expensive upper candidate forever.
        // Loss and censored transport still block this achieved-rate-derived
        // step; the generic fixed-step descent below remains available after
        // a bounded repeat.
        return observation.loss_percent <= input.thresholds.loss_max_percent
            && !metric.transport_censored;
    }
    low_realization_evidence_eligible(input.profile, metric)
}

#[derive(Clone, Copy, Debug)]
struct LowRealizationReviewCandidate {
    selected_index: usize,
    candidate_kbps: u64,
    conservative_achieved_kbps: u64,
    target_met: bool,
    worst_delta_ms: f64,
}

fn low_realization_review_candidate(
    input: &ProfileSearchInput,
    metrics: &[SearchObservationMetrics],
    candidate_kbps: u64,
) -> Option<LowRealizationReviewCandidate> {
    let indices = input
        .observations
        .iter()
        .enumerate()
        .filter(|(index, observation)| {
            observation.candidate_kbps == candidate_kbps
                && metrics[*index].manual_reviewable
                && metrics[*index].effective_delta_ms <= 200.0
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    // The descent logic deliberately moves on after two achieved-rate
    // samples corroborate one another within the repeatability bound.  Review
    // must use the same evidence contract: requiring all three retry slots
    // here erased the exact two-sample candidate which authorized the lower
    // probe.  The pairwise filter below still rejects a singleton or two
    // disagreeing samples, and `manual_reviewable` keeps the 50% trust floor.
    if indices.len() < 2 {
        return None;
    }
    // A candidate group is admissible only through observations that were
    // themselves corroborated by another achieved-rate sample.  Merely
    // having one repeatable pair must not allow an unrelated latency or
    // throughput outlier from the same exact CAKE rate to become the point
    // shown to the operator.
    let repeatable_indices = indices
        .iter()
        .copied()
        .filter(|left| {
            indices.iter().copied().any(|right| {
                left != &right
                    && achieved_rates_repeatable(
                        input.observations[*left].achieved_kbps,
                        input.observations[right].achieved_kbps,
                    )
            })
        })
        .collect::<Vec<_>>();
    if repeatable_indices.len() < 2 {
        return None;
    }
    let conservative_achieved_kbps = repeatable_indices
        .iter()
        .map(|index| input.observations[*index].achieved_kbps)
        .min()?;
    // Prefer the worst clean latency sample, then the lowest achieved rate.
    // This keeps the manually reviewable fallback conservative while the
    // runtime minimum remains the exact tested CAKE candidate.
    let selected_index = repeatable_indices.into_iter().max_by(|left, right| {
        metrics[*left]
            .effective_delta_ms
            .total_cmp(&metrics[*right].effective_delta_ms)
            .then_with(|| {
                input.observations[*right]
                    .achieved_kbps
                    .cmp(&input.observations[*left].achieved_kbps)
            })
    })?;
    Some(LowRealizationReviewCandidate {
        selected_index,
        candidate_kbps,
        conservative_achieved_kbps,
        target_met: metrics[selected_index].target_met,
        worst_delta_ms: metrics[selected_index].effective_delta_ms,
    })
}

/// Retain the best bounded manual-review point across the entire descent.
/// Variable Link deliberately explores below an already useful operating
/// point in order to look for CAKE control or a latency knee. A later noisy or
/// sub-trust-floor point must not erase an earlier repeatable exact candidate.
fn best_low_realization_review_index(
    input: &ProfileSearchInput,
    metrics: &[SearchObservationMetrics],
) -> Option<usize> {
    let mut candidates = input
        .observations
        .iter()
        .map(|observation| observation.candidate_kbps)
        .collect::<Vec<_>>();
    candidates.sort_unstable();
    candidates.dedup();
    candidates
        .into_iter()
        .filter_map(|candidate_kbps| {
            low_realization_review_candidate(input, metrics, candidate_kbps)
        })
        .max_by(|left, right| {
            left.target_met
                .cmp(&right.target_met)
                .then_with(|| {
                    left.conservative_achieved_kbps
                        .cmp(&right.conservative_achieved_kbps)
                })
                .then_with(|| left.candidate_kbps.cmp(&right.candidate_kbps))
                // `max_by` is used, so reverse the latency comparison: lower
                // worst-case latency wins an otherwise equal choice.
                .then_with(|| right.worst_delta_ms.total_cmp(&left.worst_delta_ms))
        })
        .map(|candidate| candidate.selected_index)
}

#[derive(Clone, Copy, Debug)]
struct ReviewCandidate {
    selected_index: usize,
    candidate_kbps: u64,
    conservative_achieved_kbps: u64,
    worst_delta_ms: f64,
    transport_censored: bool,
    controlled: bool,
    target_met: bool,
    capacity_objective_met: bool,
}

impl ReviewCandidate {
    fn as_option(self, role: ProfileSearchOptionRole) -> ProfileSearchOption {
        let auto_apply_candidate = self.controlled
            && !self.transport_censored
            && self.target_met
            && self.capacity_objective_met;
        ProfileSearchOption {
            role,
            selected_index: self.selected_index,
            candidate_kbps: self.candidate_kbps,
            conservative_achieved_kbps: self.conservative_achieved_kbps,
            worst_delta_ms: self.worst_delta_ms,
            grade: classify_quality(Some(self.worst_delta_ms)).as_str(),
            transport_censored: self.transport_censored,
            controlled: self.controlled,
            manual_reviewable: !auto_apply_candidate,
            target_met: self.target_met,
            capacity_objective_met: self.capacity_objective_met,
            auto_apply_candidate,
        }
    }
}

fn review_candidate_for_rate(
    result: &ProfileSearchResult,
    candidate_kbps: u64,
) -> Option<ReviewCandidate> {
    let exact_indices = result
        .observations
        .iter()
        .enumerate()
        .filter(|(_, observation)| observation.candidate_kbps == candidate_kbps)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    // A repeated loss/resource failure at the same exact rate invalidates
    // that direction-level option.  CPU is advisory and is intentionally not
    // part of resource_safe.
    if exact_indices.is_empty()
        || exact_indices
            .iter()
            .any(|index| !result.metrics[*index].resource_safe)
    {
        return None;
    }
    let controlled_indices = exact_indices
        .iter()
        .copied()
        .filter(|index| result.metrics[*index].safety_pass)
        .collect::<Vec<_>>();
    let (indices, controlled) = if controlled_indices.is_empty() {
        let censored_indices = exact_indices
            .iter()
            .copied()
            .filter(|index| {
                result.observations[*index].transport_censored
                    && result.metrics[*index].manual_reviewable
            })
            .collect::<Vec<_>>();
        if !censored_indices.is_empty() {
            (censored_indices, false)
        } else {
            let input = ProfileSearchInput {
                profile: result.profile,
                direction: result.direction,
                observed_low_kbps: result.observed_low_kbps,
                minimum_kbps: result.exploration_minimum_kbps,
                upper_kbps: result
                    .observations
                    .iter()
                    .map(|observation| observation.candidate_kbps)
                    .max()?,
                thresholds: result.profile.validation_thresholds(),
                uncertainty_percent: 0.0,
                max_attempts: result.max_attempts,
                observations: result.observations.clone(),
            };
            let candidate =
                low_realization_review_candidate(&input, &result.metrics, candidate_kbps)?;
            let corroborated = exact_indices
                .iter()
                .copied()
                .filter(|left| {
                    result.metrics[*left].manual_reviewable
                        && result.metrics[*left].effective_delta_ms <= 200.0
                        && exact_indices.iter().copied().any(|right| {
                            left != &right
                                && result.metrics[right].manual_reviewable
                                && achieved_rates_repeatable(
                                    result.observations[*left].achieved_kbps,
                                    result.observations[right].achieved_kbps,
                                )
                        })
                })
                .collect::<Vec<_>>();
            debug_assert_eq!(
                candidate.conservative_achieved_kbps,
                corroborated
                    .iter()
                    .map(|index| result.observations[*index].achieved_kbps)
                    .min()
                    .unwrap_or(candidate.conservative_achieved_kbps)
            );
            (corroborated, false)
        }
    } else {
        (controlled_indices, true)
    };
    if indices.is_empty() {
        return None;
    }
    let conservative_achieved_kbps = indices
        .iter()
        .map(|index| result.observations[*index].achieved_kbps)
        .min()?;
    let selected_index = indices.into_iter().max_by(|left, right| {
        result.metrics[*left]
            .effective_delta_ms
            .total_cmp(&result.metrics[*right].effective_delta_ms)
            .then_with(|| {
                result.observations[*right]
                    .achieved_kbps
                    .cmp(&result.observations[*left].achieved_kbps)
            })
    })?;
    let worst_delta_ms = result.metrics[selected_index].effective_delta_ms;
    let retention_percent =
        conservative_achieved_kbps as f64 * 100.0 / result.observed_low_kbps as f64;
    Some(ReviewCandidate {
        selected_index,
        candidate_kbps,
        conservative_achieved_kbps,
        worst_delta_ms,
        transport_censored: result.observations[selected_index].transport_censored,
        controlled,
        target_met: result.metrics[selected_index].target_met,
        capacity_objective_met: retention_percent >= result.profile.capacity_floor_percent(),
    })
}

fn profile_search_review_options(result: &ProfileSearchResult) -> Vec<ProfileSearchOption> {
    if !matches!(
        result.action,
        ProfileSearchAction::Complete | ProfileSearchAction::Fallback
    ) {
        return Vec::new();
    }
    let mut rates = result
        .observations
        .iter()
        .map(|observation| observation.candidate_kbps)
        .collect::<Vec<_>>();
    rates.sort_unstable();
    rates.dedup();
    let candidates = rates
        .into_iter()
        .filter_map(|rate| review_candidate_for_rate(result, rate))
        .collect::<Vec<_>>();
    let frontier = candidates
        .iter()
        .copied()
        .filter(|candidate| {
            !candidates.iter().any(|other| {
                other.candidate_kbps != candidate.candidate_kbps
                    && other.controlled >= candidate.controlled
                    && other.conservative_achieved_kbps >= candidate.conservative_achieved_kbps
                    && other.worst_delta_ms <= candidate.worst_delta_ms
                    && (other.controlled != candidate.controlled
                        || other.conservative_achieved_kbps > candidate.conservative_achieved_kbps
                        || other.worst_delta_ms < candidate.worst_delta_ms)
            })
        })
        .collect::<Vec<_>>();
    if frontier.is_empty() {
        return Vec::new();
    }
    let throughput = frontier.iter().copied().max_by(|left, right| {
        left.conservative_achieved_kbps
            .cmp(&right.conservative_achieved_kbps)
            .then_with(|| right.worst_delta_ms.total_cmp(&left.worst_delta_ms))
            .then_with(|| left.candidate_kbps.cmp(&right.candidate_kbps))
    });
    let quality = frontier.iter().copied().min_by(|left, right| {
        left.worst_delta_ms
            .total_cmp(&right.worst_delta_ms)
            .then_with(|| {
                right
                    .conservative_achieved_kbps
                    .cmp(&left.conservative_achieved_kbps)
            })
            .then_with(|| right.candidate_kbps.cmp(&left.candidate_kbps))
    });
    let selected_rate = result
        .selected_index
        .and_then(|index| result.observations.get(index))
        .map(|observation| observation.candidate_kbps);
    let recommended = selected_rate
        .and_then(|rate| {
            frontier
                .iter()
                .copied()
                .find(|candidate| candidate.candidate_kbps == rate)
        })
        .or_else(|| match result.profile {
            AutotuneProfile::Fair => throughput,
            AutotuneProfile::Gaming
            | AutotuneProfile::GamingExtreme
            | AutotuneProfile::VariableLink => frontier.iter().copied().max_by(|left, right| {
                left.target_met
                    .cmp(&right.target_met)
                    .then_with(|| {
                        left.conservative_achieved_kbps
                            .cmp(&right.conservative_achieved_kbps)
                    })
                    .then_with(|| right.worst_delta_ms.total_cmp(&left.worst_delta_ms))
            }),
            AutotuneProfile::BestOverall => frontier.iter().copied().max_by(|left, right| {
                review_candidate_balanced_score(result, *left)
                    .total_cmp(&review_candidate_balanced_score(result, *right))
                    .then_with(|| {
                        left.conservative_achieved_kbps
                            .cmp(&right.conservative_achieved_kbps)
                    })
            }),
        });
    let mut options = Vec::with_capacity(MAX_PROFILE_REVIEW_OPTIONS);
    for (role, candidate) in [
        (ProfileSearchOptionRole::Recommended, recommended),
        (ProfileSearchOptionRole::QualityFirst, quality),
        (ProfileSearchOptionRole::ThroughputFirst, throughput),
    ] {
        if let Some(candidate) = candidate {
            if !options.iter().any(|option: &ProfileSearchOption| {
                option.candidate_kbps == candidate.candidate_kbps
            }) {
                options.push(candidate.as_option(role));
            }
        }
    }
    if options.len() < MAX_PROFILE_REVIEW_OPTIONS {
        let mut remaining = frontier
            .iter()
            .copied()
            .filter(|candidate| {
                !options
                    .iter()
                    .any(|option| option.candidate_kbps == candidate.candidate_kbps)
            })
            .collect::<Vec<_>>();
        remaining.sort_by(|left, right| {
            review_candidate_balanced_score(result, *right)
                .total_cmp(&review_candidate_balanced_score(result, *left))
                .then_with(|| {
                    right
                        .conservative_achieved_kbps
                        .cmp(&left.conservative_achieved_kbps)
                })
        });
        for candidate in remaining {
            options.push(candidate.as_option(ProfileSearchOptionRole::BalancedAlternative));
            if options.len() == MAX_PROFILE_REVIEW_OPTIONS {
                break;
            }
        }
    }
    options.truncate(MAX_PROFILE_REVIEW_OPTIONS);
    options
}

fn review_candidate_balanced_score(
    result: &ProfileSearchResult,
    candidate: ReviewCandidate,
) -> f64 {
    let throughput = (candidate.conservative_achieved_kbps as f64
        / result.observed_low_kbps as f64)
        .clamp(0.0, 1.0);
    let quality =
        (result.profile.target_delta_ms() / candidate.worst_delta_ms.max(0.001)).clamp(0.0, 1.0);
    throughput + quality
}

pub fn profile_pair_candidates(
    download: &ProfileSearchResult,
    upload: &ProfileSearchResult,
) -> Result<Vec<ProfilePairCandidate>, String> {
    if download.profile != upload.profile
        || download.direction != SearchDirection::Download
        || upload.direction != SearchDirection::Upload
    {
        return Err(
            "profile pair candidates require matching download and upload searches".to_string(),
        );
    }
    let download_options = download.review_options();
    let upload_options = upload.review_options();
    if download_options.is_empty() || upload_options.is_empty() {
        return Err(
            "profile pair candidates require reviewable evidence in both directions".to_string(),
        );
    }
    let pick = |options: &[ProfileSearchOption], role: ProfileSearchOptionRole| match role {
        ProfileSearchOptionRole::Recommended => options
            .iter()
            .copied()
            .find(|option| option.role == ProfileSearchOptionRole::Recommended)
            .or_else(|| options.first().copied()),
        ProfileSearchOptionRole::QualityFirst => options.iter().copied().min_by(|left, right| {
            left.worst_delta_ms
                .total_cmp(&right.worst_delta_ms)
                .then_with(|| {
                    right
                        .conservative_achieved_kbps
                        .cmp(&left.conservative_achieved_kbps)
                })
        }),
        ProfileSearchOptionRole::ThroughputFirst => {
            options.iter().copied().max_by(|left, right| {
                left.conservative_achieved_kbps
                    .cmp(&right.conservative_achieved_kbps)
                    .then_with(|| right.worst_delta_ms.total_cmp(&left.worst_delta_ms))
            })
        }
        ProfileSearchOptionRole::BalancedAlternative => options
            .iter()
            .copied()
            .find(|option| option.role == ProfileSearchOptionRole::BalancedAlternative)
            .or_else(|| options.get(1).copied())
            .or_else(|| options.first().copied()),
    };
    let mut pairs = Vec::with_capacity(MAX_PROFILE_REVIEW_OPTIONS);
    for role in [
        ProfileSearchOptionRole::Recommended,
        ProfileSearchOptionRole::QualityFirst,
        ProfileSearchOptionRole::ThroughputFirst,
        ProfileSearchOptionRole::BalancedAlternative,
    ] {
        let dl = pick(&download_options, role)
            .ok_or_else(|| "download profile option selection failed".to_string())?;
        let ul = pick(&upload_options, role)
            .ok_or_else(|| "upload profile option selection failed".to_string())?;
        if pairs.iter().any(|pair: &ProfilePairCandidate| {
            pair.download_kbps == dl.candidate_kbps && pair.upload_kbps == ul.candidate_kbps
        }) {
            continue;
        }
        pairs.push(ProfilePairCandidate {
            role,
            download_index: dl.selected_index,
            upload_index: ul.selected_index,
            download_kbps: dl.candidate_kbps,
            upload_kbps: ul.candidate_kbps,
        });
        if pairs.len() == MAX_PROFILE_REVIEW_OPTIONS {
            break;
        }
    }
    if pairs.is_empty() {
        return Err("profile pair candidate set is empty".to_string());
    }
    Ok(pairs)
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
                || !low_realization_descent_evidence_eligible(input, metrics, *index)
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

#[derive(Clone, Copy, Debug)]
struct VariableCandidateAggregate {
    candidate_kbps: u64,
    representative_index: usize,
    median_delta_ms: f64,
    repeat_count: usize,
    delta_spread_ms: f64,
}

fn variable_candidate_aggregates(
    input: &ProfileSearchInput,
    metrics: &[SearchObservationMetrics],
) -> Vec<VariableCandidateAggregate> {
    let mut candidates = input
        .observations
        .iter()
        .map(|observation| observation.candidate_kbps)
        .collect::<Vec<_>>();
    candidates.sort_unstable_by(|left, right| right.cmp(left));
    candidates.dedup();
    candidates
        .into_iter()
        .filter_map(|candidate_kbps| {
            let indices = input
                .observations
                .iter()
                .enumerate()
                .filter(|(index, observation)| {
                    observation.candidate_kbps == candidate_kbps && metrics[*index].safety_pass
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            if indices.is_empty() {
                return None;
            }
            let mut deltas = indices
                .iter()
                .map(|index| metrics[*index].effective_delta_ms)
                .collect::<Vec<_>>();
            deltas.sort_by(f64::total_cmp);
            let median_delta_ms = percentile(&deltas, 0.5);
            let representative_index = indices
                .iter()
                .copied()
                .min_by(|left, right| {
                    (metrics[*left].effective_delta_ms - median_delta_ms)
                        .abs()
                        .total_cmp(&(metrics[*right].effective_delta_ms - median_delta_ms).abs())
                })
                .expect("non-empty controlled candidate indices");
            Some(VariableCandidateAggregate {
                candidate_kbps,
                representative_index,
                median_delta_ms,
                repeat_count: indices.len(),
                delta_spread_ms: deltas.last().copied().unwrap_or(median_delta_ms)
                    - deltas.first().copied().unwrap_or(median_delta_ms),
            })
        })
        .collect()
}

fn variable_plateau_threshold_ms(
    left_delta_ms: f64,
    right_delta_ms: f64,
    uncertainty_percent: f64,
) -> f64 {
    VARIABLE_LINK_PLATEAU_MINIMUM_MS
        .max(left_delta_ms.max(right_delta_ms) * (uncertainty_percent / 100.0))
}

fn optimize_variable_link_direction(
    input: &ProfileSearchInput,
    metrics: &[SearchObservationMetrics],
) -> ProfileSearchResult {
    let last_index = input.observations.len() - 1;
    let last = input.observations[last_index];
    let last_metrics = metrics[last_index];
    let duplicate_count = input
        .observations
        .iter()
        .filter(|observation| observation.candidate_kbps == last.candidate_kbps)
        .count();
    let aggregates = variable_candidate_aggregates(input, metrics);
    let repeat_count = input.observations.len().saturating_sub(aggregates.len());
    let default_threshold = VARIABLE_LINK_PLATEAU_MINIMUM_MS;

    let finish = |action: ProfileSearchAction,
                  reason: &'static str,
                  next_candidate_kbps: Option<u64>,
                  selected_index: Option<usize>,
                  runtime_minimum_index: Option<usize>,
                  knee_detected: bool,
                  knee_confidence_percent: u64,
                  plateau_improvement_ms: Option<f64>,
                  plateau_threshold_ms: f64,
                  no_cake_effect: bool,
                  noisy: bool| {
        let (mut action, mut selected_index) =
            resolve_terminal_safe_fallback(input, metrics, action, selected_index);
        let mut reason = reason;
        // A controlled safety-pass remains stronger than manual evidence and
        // is selected first by `resolve_terminal_safe_fallback`. If no such
        // point exists, preserve the best repeatable 50-80% realization point
        // from any earlier candidate as an explicit Review-only fallback.
        if action == ProfileSearchAction::Inconclusive {
            if let Some(index) = best_low_realization_review_index(input, metrics) {
                action = ProfileSearchAction::Fallback;
                selected_index = Some(index);
                reason = "bounded-low-realization-review";
            }
        }
        if action == ProfileSearchAction::Inconclusive {
            if let Some(index) = best_censored_review_index(input, metrics) {
                action = ProfileSearchAction::Fallback;
                selected_index = Some(index);
                reason = "transport-deadline-censored-review";
            }
        }
        // A selected operating ceiling and an autorate runtime minimum are
        // independent quantities.  Only a measured latency knee proves the
        // latter.  A safe/noisy/no-effect fallback may still select a ceiling,
        // but must leave the runtime minimum unresolved instead of silently
        // turning that ceiling into a floor.
        let runtime_minimum_index = knee_detected.then_some(runtime_minimum_index).flatten();
        ProfileSearchResult {
            profile: input.profile,
            direction: input.direction,
            observed_low_kbps: input.observed_low_kbps,
            action,
            reason,
            next_candidate_kbps,
            selected_index,
            lower_target_pass_kbps: aggregates
                .iter()
                .filter(|aggregate| metrics[aggregate.representative_index].target_met)
                .map(|aggregate| aggregate.candidate_kbps)
                .max(),
            upper_target_fail_kbps: None,
            resolution_kbps: search_resolution(input.observed_low_kbps),
            max_attempts: input.max_attempts,
            metrics: metrics.to_vec(),
            observations: input.observations.clone(),
            exploration_minimum_kbps: input.minimum_kbps,
            runtime_minimum_index,
            knee_detected,
            knee_confidence_percent,
            plateau_improvement_ms,
            plateau_threshold_ms,
            repeat_count,
            no_cake_effect,
            noisy,
        }
    };

    if !last_metrics.resource_safe {
        if duplicate_count >= 2 && input.observations.len() < input.max_attempts {
            let next =
                controlled_candidate_from_low_realization(input, metrics, last.candidate_kbps)
                    .or_else(|| {
                        next_variable_link_unobserved_candidate(
                            input.observed_low_kbps,
                            input.minimum_kbps,
                            last.candidate_kbps,
                        )
                        .ok()
                        .flatten()
                    });
            if let Some(next) = next.filter(|candidate| {
                *candidate < last.candidate_kbps
                    && !candidate_was_tested(&input.observations, *candidate)
            }) {
                return finish(
                    ProfileSearchAction::Test,
                    "lower-variable-candidate-after-resource-limit",
                    Some(next),
                    None,
                    None,
                    false,
                    0,
                    None,
                    default_threshold,
                    false,
                    true,
                );
            }
        }
        if duplicate_count < MAX_SAME_CANDIDATE_OBSERVATIONS
            && input.observations.len() < input.max_attempts
        {
            return finish(
                ProfileSearchAction::Test,
                "repeat-resource-unsafe-variable-candidate",
                Some(last.candidate_kbps),
                None,
                None,
                false,
                0,
                None,
                default_threshold,
                false,
                true,
            );
        }
        return finish(
            ProfileSearchAction::Inconclusive,
            "variable-candidate-resource-safety-inconclusive",
            None,
            None,
            None,
            false,
            0,
            None,
            default_threshold,
            false,
            true,
        );
    }

    if !last_metrics.measurement_reliable {
        if let Some(next) =
            controlled_candidate_from_low_realization(input, metrics, last.candidate_kbps)
        {
            if next < last.candidate_kbps
                && !candidate_was_tested(&input.observations, next)
                && input.observations.len() < input.max_attempts
            {
                return finish(
                    ProfileSearchAction::Test,
                    "lower-variable-candidate-to-establish-shaper-control",
                    Some(next),
                    None,
                    None,
                    false,
                    0,
                    None,
                    default_threshold,
                    false,
                    false,
                );
            }
        }

        if low_realization_evidence_eligible(input.profile, last_metrics) {
            if let Some(selected_index) = best_low_realization_review_index(input, metrics) {
                return finish(
                    ProfileSearchAction::Fallback,
                    "bounded-low-realization-review",
                    None,
                    Some(selected_index),
                    Some(selected_index),
                    false,
                    0,
                    None,
                    default_threshold,
                    false,
                    false,
                );
            }
        }

        if duplicate_count < MAX_SAME_CANDIDATE_OBSERVATIONS
            && input.observations.len() < input.max_attempts
        {
            return finish(
                ProfileSearchAction::Test,
                "repeat-uncontrolled-variable-candidate",
                Some(last.candidate_kbps),
                None,
                None,
                false,
                0,
                None,
                default_threshold,
                false,
                true,
            );
        }
        return finish(
            ProfileSearchAction::Inconclusive,
            "variable-candidate-realization-inconclusive",
            None,
            None,
            None,
            false,
            0,
            None,
            default_threshold,
            false,
            true,
        );
    }

    if let Some(last_aggregate) = aggregates
        .iter()
        .find(|aggregate| aggregate.candidate_kbps == last.candidate_kbps)
    {
        let noise_limit =
            (last_aggregate.median_delta_ms * 0.10).max(VARIABLE_LINK_PLATEAU_MINIMUM_MS * 2.0);
        if last_aggregate.delta_spread_ms > noise_limit {
            if duplicate_count < MAX_SAME_CANDIDATE_OBSERVATIONS
                && input.observations.len() < input.max_attempts
            {
                return finish(
                    ProfileSearchAction::Test,
                    "repeat-noisy-variable-candidate",
                    Some(last.candidate_kbps),
                    None,
                    None,
                    false,
                    0,
                    None,
                    noise_limit,
                    false,
                    true,
                );
            }
            if let Some(selected_index) =
                best_variable_operating_ceiling_index(&input.observations, metrics)
            {
                return finish(
                    ProfileSearchAction::Fallback,
                    "noisy-link-safe-review",
                    None,
                    Some(selected_index),
                    Some(selected_index),
                    false,
                    0,
                    None,
                    noise_limit,
                    false,
                    true,
                );
            }
            return finish(
                ProfileSearchAction::Inconclusive,
                "noisy-link-candidate-did-not-converge",
                None,
                None,
                None,
                false,
                0,
                None,
                noise_limit,
                false,
                true,
            );
        }
    }

    if aggregates.len() >= 2 {
        let lower = aggregates.last().expect("at least two aggregates");
        let higher = &aggregates[aggregates.len() - 2];
        let threshold = variable_plateau_threshold_ms(
            higher.median_delta_ms,
            lower.median_delta_ms,
            input.uncertainty_percent,
        );
        if lower.median_delta_ms > higher.median_delta_ms + threshold {
            if lower.repeat_count < MAX_SAME_CANDIDATE_OBSERVATIONS
                && input.observations.len() < input.max_attempts
            {
                return finish(
                    ProfileSearchAction::Test,
                    "repeat-nonmonotonic-variable-candidate",
                    Some(lower.candidate_kbps),
                    None,
                    None,
                    false,
                    0,
                    None,
                    threshold,
                    false,
                    true,
                );
            }
            if let Some(selected_index) =
                best_variable_operating_ceiling_index(&input.observations, metrics)
            {
                return finish(
                    ProfileSearchAction::Fallback,
                    "noisy-link-safe-review",
                    None,
                    Some(selected_index),
                    Some(selected_index),
                    false,
                    0,
                    None,
                    threshold,
                    false,
                    true,
                );
            }
            return finish(
                ProfileSearchAction::Inconclusive,
                "nonmonotonic-variable-link-after-retries",
                None,
                None,
                None,
                false,
                0,
                None,
                threshold,
                false,
                true,
            );
        }
    }

    if aggregates.len() >= 3 {
        let improvements = aggregates
            .windows(2)
            .map(|pair| {
                let threshold = variable_plateau_threshold_ms(
                    pair[0].median_delta_ms,
                    pair[1].median_delta_ms,
                    input.uncertainty_percent,
                );
                (pair[0].median_delta_ms - pair[1].median_delta_ms, threshold)
            })
            .collect::<Vec<_>>();
        let last_two = &improvements[improvements.len() - 2..];
        if last_two
            .iter()
            .all(|(improvement, threshold)| *improvement <= *threshold)
        {
            let prior_useful = improvements[..improvements.len() - 2]
                .iter()
                .any(|(improvement, threshold)| *improvement > *threshold);
            let plateau_improvement_ms = last_two
                .iter()
                .map(|(improvement, _)| *improvement)
                .max_by(f64::total_cmp);
            let plateau_threshold_ms = last_two
                .iter()
                .map(|(_, threshold)| *threshold)
                .max_by(f64::total_cmp)
                .unwrap_or(default_threshold);
            if !prior_useful {
                if let Some(selected_index) =
                    best_no_cake_effect_index(&input.observations, metrics)
                {
                    // A flat latency curve is directional evidence, not a
                    // global calibration failure. Hold this direction at its
                    // highest safe, target-meeting tested point and let the
                    // peer direction continue. No latency knee was proven, so
                    // the runtime minimum remains explicitly unresolved.
                    return finish(
                        ProfileSearchAction::Fallback,
                        "queue-outside-cake-control",
                        None,
                        Some(selected_index),
                        Some(selected_index),
                        false,
                        0,
                        plateau_improvement_ms,
                        plateau_threshold_ms,
                        true,
                        false,
                    );
                }
                return finish(
                    ProfileSearchAction::Inconclusive,
                    "queue-outside-cake-control-target-unmet",
                    None,
                    None,
                    None,
                    false,
                    0,
                    plateau_improvement_ms,
                    plateau_threshold_ms,
                    true,
                    false,
                );
            }

            let knee = aggregates[aggregates.len() - 3];
            let runtime_minimum_index = knee.representative_index;
            let runtime_minimum_kbps = knee.candidate_kbps;
            let selected_index = best_target_index(&input.observations, metrics)
                .filter(|index| input.observations[*index].candidate_kbps >= runtime_minimum_kbps)
                .or_else(|| {
                    best_balanced_index(&input.observations, metrics).filter(|index| {
                        input.observations[*index].candidate_kbps >= runtime_minimum_kbps
                    })
                })
                .or(Some(runtime_minimum_index));
            let selected_metrics = selected_index.map(|index| metrics[index]);
            let action = if selected_metrics
                .map(|metric| metric.target_met && metric.capacity_objective_met)
                .unwrap_or(false)
            {
                ProfileSearchAction::Complete
            } else {
                ProfileSearchAction::Fallback
            };
            let confidence = (80 + repeat_count.min(2) as u64 * 10).min(100);
            return finish(
                action,
                "measured-variable-link-knee",
                None,
                selected_index,
                Some(runtime_minimum_index),
                true,
                confidence,
                plateau_improvement_ms,
                plateau_threshold_ms,
                false,
                false,
            );
        }
    }

    let lowest_tested = aggregates
        .last()
        .map(|aggregate| aggregate.candidate_kbps)
        .unwrap_or(last.candidate_kbps);
    if lowest_tested <= input.minimum_kbps {
        if let Some(selected_index) = best_no_cake_effect_index(&input.observations, metrics) {
            return finish(
                ProfileSearchAction::Fallback,
                "exploration-floor-reached",
                None,
                Some(selected_index),
                Some(selected_index),
                false,
                0,
                None,
                default_threshold,
                false,
                false,
            );
        }
        return finish(
            ProfileSearchAction::Inconclusive,
            "exploration-floor-reached-without-latency-knee",
            None,
            None,
            None,
            false,
            0,
            None,
            default_threshold,
            false,
            false,
        );
    }
    if input.observations.len() >= input.max_attempts {
        return finish(
            ProfileSearchAction::Inconclusive,
            "bounded-attempt-limit-before-latency-knee",
            None,
            None,
            None,
            false,
            0,
            None,
            default_threshold,
            false,
            false,
        );
    }

    let Some(next) = next_variable_link_unobserved_candidate(
        input.observed_low_kbps,
        input.minimum_kbps,
        lowest_tested,
    )
    .expect("validated Variable Link search bounds") else {
        return finish(
            ProfileSearchAction::Inconclusive,
            "variable-link-search-cannot-make-progress",
            None,
            None,
            None,
            false,
            0,
            None,
            default_threshold,
            false,
            false,
        );
    };
    if candidate_was_tested(&input.observations, next) {
        return finish(
            ProfileSearchAction::Inconclusive,
            "variable-link-search-cannot-make-progress",
            None,
            None,
            None,
            false,
            0,
            None,
            default_threshold,
            false,
            false,
        );
    }
    finish(
        ProfileSearchAction::Test,
        "descend-variable-link-fixed-step",
        Some(next),
        None,
        None,
        false,
        0,
        None,
        default_threshold,
        false,
        false,
    )
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
    if input.profile == AutotuneProfile::VariableLink {
        return Ok(optimize_variable_link_direction(&input, &metrics));
    }
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
        AutotuneProfile::Gaming | AutotuneProfile::GamingExtreme => {
            best_target_index(&input.observations, &metrics)
                .or_else(|| best_quality_index(&input.observations, &metrics))
        }
        AutotuneProfile::BestOverall => best_target_index(&input.observations, &metrics)
            .or_else(|| best_balanced_index(&input.observations, &metrics)),
        AutotuneProfile::VariableLink => unreachable!("handled before the legacy search"),
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
                  next_candidate_kbps: Option<u64>|
     -> ProfileSearchResult {
        let (mut action, mut selected_index) =
            resolve_terminal_safe_fallback(&input, &metrics, action, selected_index);
        let mut reason = reason;
        // Every profile may preserve a repeatable, bounded 50-80% realization
        // point for explicit Review when no controlled candidate survived.
        // This does not make the point an Auto-Apply success: pair
        // confirmation and explicit acceptance of every missed objective are
        // still required, and the sub-50% trust floor remains fail-closed.
        if action == ProfileSearchAction::Inconclusive {
            if let Some(index) = best_low_realization_review_index(&input, &metrics) {
                action = ProfileSearchAction::Fallback;
                selected_index = Some(index);
                reason = "bounded-low-realization-review";
            }
        }
        if action == ProfileSearchAction::Inconclusive {
            if let Some(index) = best_censored_review_index(&input, &metrics) {
                action = ProfileSearchAction::Fallback;
                selected_index = Some(index);
                reason = "transport-deadline-censored-review";
            }
        }
        let runtime_minimum_index = if input.profile == AutotuneProfile::GamingExtreme
            && matches!(
                action,
                ProfileSearchAction::Complete | ProfileSearchAction::Fallback
            ) {
            input
                .observations
                .iter()
                .enumerate()
                .filter(|(index, _)| metrics[*index].safety_pass && metrics[*index].target_met)
                .min_by_key(|(_, observation)| observation.candidate_kbps)
                .map(|(index, _)| index)
        } else {
            None
        };
        ProfileSearchResult {
            profile: input.profile,
            direction: input.direction,
            observed_low_kbps: input.observed_low_kbps,
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
            exploration_minimum_kbps: input.minimum_kbps,
            runtime_minimum_index,
            knee_detected: false,
            knee_confidence_percent: 0,
            plateau_improvement_ms: None,
            plateau_threshold_ms: 0.0,
            repeat_count: input.observations.len().saturating_sub(
                input
                    .observations
                    .iter()
                    .map(|observation| observation.candidate_kbps)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len(),
            ),
            no_cake_effect: false,
            noisy: false,
        }
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
            AutotuneProfile::Gaming
            | AutotuneProfile::GamingExtreme
            | AutotuneProfile::BestOverall
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
        if matches!(
            input.profile,
            AutotuneProfile::Gaming | AutotuneProfile::GamingExtreme
        ) {
            "target-a-plus-unreachable-above-safety-floor"
        } else {
            "target-a-unreachable-use-balanced-fallback"
        },
        None,
    ))
}

/// Stop a deterministic search at its last measured boundary without
/// manufacturing an observation for the candidate which could not be
/// measured.  This is deliberately narrower than `optimize_profile_direction`:
/// callers may use it only after the measurement apparatus remained admitted
/// but the loaded latency streams did not reach their bounded sample minimum.
///
/// The selected point is therefore drawn exclusively from prior exact
/// observations.  Controlled safe points win; when none exists, the existing
/// corroborated 50-80% realization contract, or one capture-corroborated
/// censored transport point, may retain a manual-review-only point.  Variable
/// Link has one narrower terminal-only exception: repeatable CAKE wire/goodput
/// evidence above the authorized exploration floor may identify a physical
/// bottleneck below the configured CAKE ceiling.  That exact tested ceiling is
/// Review-only, never a controlled point or an inferred runtime minimum.
pub fn terminate_profile_direction_at_measured_boundary(
    input: ProfileSearchInput,
    reason: &'static str,
) -> Result<ProfileSearchResult, String> {
    validate_profile_search_input(&input)?;
    let mut result = optimize_profile_direction(input.clone())?;
    if matches!(
        result.action,
        ProfileSearchAction::Complete | ProfileSearchAction::Fallback
    ) {
        return Ok(result);
    }

    let mut metrics = result.metrics.clone();
    let (mut action, mut selected_index) =
        resolve_terminal_safe_fallback(&input, &metrics, ProfileSearchAction::Inconclusive, None);
    if action == ProfileSearchAction::Inconclusive {
        if let Some(index) = best_low_realization_review_index(&input, &metrics) {
            action = ProfileSearchAction::Fallback;
            selected_index = Some(index);
        }
    }
    if action == ProfileSearchAction::Inconclusive {
        if let Some(index) = best_censored_review_index(&input, &metrics) {
            action = ProfileSearchAction::Fallback;
            selected_index = Some(index);
        }
    }
    if action == ProfileSearchAction::Inconclusive {
        if let Some(indices) = physical_capacity_limited_review_indices(&input, &metrics) {
            let selected = indices
                .iter()
                .copied()
                .max_by(|left, right| {
                    metrics[*left]
                        .effective_delta_ms
                        .total_cmp(&metrics[*right].effective_delta_ms)
                        .then_with(|| {
                            input.observations[*right]
                                .achieved_kbps
                                .cmp(&input.observations[*left].achieved_kbps)
                        })
                })
                .expect("capacity-limited review indices are non-empty");
            for index in indices {
                metrics[index].manual_reviewable = true;
            }
            action = ProfileSearchAction::Fallback;
            selected_index = Some(selected);
            result.reason = PHYSICAL_CAPACITY_BELOW_CAKE_CANDIDATE_REVIEW_REASON;
        }
    }

    result.action = action;
    if result.reason != PHYSICAL_CAPACITY_BELOW_CAKE_CANDIDATE_REVIEW_REASON {
        result.reason = reason;
    }
    result.next_candidate_kbps = None;
    result.selected_index = selected_index;
    result.metrics = metrics.clone();
    result.runtime_minimum_index = if input.profile == AutotuneProfile::GamingExtreme
        && action == ProfileSearchAction::Fallback
    {
        input
            .observations
            .iter()
            .enumerate()
            .filter(|(index, _)| metrics[*index].safety_pass && metrics[*index].target_met)
            .min_by_key(|(_, observation)| observation.candidate_kbps)
            .map(|(index, _)| index)
    } else {
        None
    };
    Ok(result)
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
    fn variable_access_medium_changes_only_the_exploration_policy() {
        let baseline = LatencyBaseline {
            median_ms: 25.0,
            p95_ms: 35.0,
            samples: 20,
        };
        let build = |medium| {
            build_proposal_for_profile_with_context(
                &[100_000.0, 102_000.0],
                &[20_000.0, 20_400.0],
                baseline,
                LinkKind::Unknown,
                AutotuneProfile::VariableLink,
                ProposalContext {
                    access_medium: Some(medium),
                    access_source: AccessEvidenceSource::UserSelected,
                    access_confidence_percent: 100,
                    capacity_learning_policy: Some(CapacityLearningPolicy::VerifiedOnly),
                    download_service_cap_kbps: None,
                    upload_service_cap_kbps: None,
                },
            )
            .unwrap()
        };
        let cellular = build(AccessMedium::Cellular);
        let unknown = build(AccessMedium::Unknown);

        assert_eq!(cellular.download.exploration_minimum_kbps, 35_000);
        assert_eq!(unknown.download.exploration_minimum_kbps, 50_000);
        assert_eq!(
            cellular.download.observed_low_kbps,
            unknown.download.observed_low_kbps
        );
        assert_eq!(
            cellular.download.exploration_cap_kbps,
            cellular.download.observed_high_kbps
        );
        assert!(!cellular.adaptive_ceiling_enabled);
        assert_eq!(
            cellular.capacity_learning_policy,
            CapacityLearningPolicy::VerifiedOnly
        );
    }

    #[test]
    fn service_hard_cap_tightens_but_never_expands_measured_capacity() {
        let proposal = build_proposal_for_profile_with_context(
            &[100_000.0, 110_000.0],
            &[20_000.0, 22_000.0],
            LatencyBaseline {
                median_ms: 10.0,
                p95_ms: 12.0,
                samples: 20,
            },
            LinkKind::Cellular,
            AutotuneProfile::VariableLink,
            ProposalContext {
                access_medium: Some(AccessMedium::Cellular),
                access_source: AccessEvidenceSource::NetworkProtocol,
                access_confidence_percent: 95,
                capacity_learning_policy: Some(CapacityLearningPolicy::PassiveBounded),
                download_service_cap_kbps: Some(90_000),
                upload_service_cap_kbps: Some(50_000),
            },
        )
        .unwrap();

        assert_eq!(
            proposal.download.exploration_cap_kbps,
            proposal.download.observed_high_kbps
        );
        assert_eq!(proposal.download.absolute_cap_kbps, 90_000);
        assert_eq!(proposal.download.service_hard_cap_kbps, Some(90_000));
        assert_eq!(
            proposal.download.cap_source,
            CeilingCapSource::UserServiceLimit
        );
        assert_eq!(
            proposal.upload.exploration_cap_kbps,
            proposal.upload.observed_high_kbps
        );
        assert_eq!(proposal.upload.service_hard_cap_kbps, Some(50_000));
        assert_eq!(proposal.upload.cap_source, CeilingCapSource::MeasuredRaw);
        assert!(proposal.adaptive_ceiling_enabled);
        assert!(proposal.to_json().contains("\"medium\":\"cellular\""));
        assert!(proposal
            .to_json()
            .contains("\"policy\":\"passive_bounded\""));
    }

    #[test]
    fn fixed_cap_policy_requires_both_directional_caps() {
        let result = build_proposal_for_profile_with_context(
            &[100_000.0, 101_000.0],
            &[20_000.0, 20_100.0],
            LatencyBaseline {
                median_ms: 10.0,
                p95_ms: 12.0,
                samples: 20,
            },
            LinkKind::Unknown,
            AutotuneProfile::VariableLink,
            ProposalContext {
                access_medium: Some(AccessMedium::Unknown),
                access_source: AccessEvidenceSource::AutoInconclusive,
                access_confidence_percent: 10,
                capacity_learning_policy: Some(CapacityLearningPolicy::FixedCap),
                download_service_cap_kbps: Some(100_000),
                upload_service_cap_kbps: None,
            },
        );

        assert!(result
            .unwrap_err()
            .contains("requires download and upload service hard caps"));
    }

    #[test]
    fn service_hard_cap_rejects_rates_below_supported_floor() {
        let result = build_proposal_for_profile_with_context(
            &[100_000.0, 101_000.0],
            &[20_000.0, 20_100.0],
            LatencyBaseline {
                median_ms: 10.0,
                p95_ms: 12.0,
                samples: 20,
            },
            LinkKind::Unknown,
            AutotuneProfile::VariableLink,
            ProposalContext {
                access_medium: Some(AccessMedium::Unknown),
                access_source: AccessEvidenceSource::UserSelected,
                access_confidence_percent: 100,
                capacity_learning_policy: Some(CapacityLearningPolicy::VerifiedOnly),
                download_service_cap_kbps: Some(99),
                upload_service_cap_kbps: None,
            },
        );

        assert!(result
            .unwrap_err()
            .contains("must be between 100 and 100000000 kbit/s"));
    }

    #[test]
    fn profiles_share_the_raw_start_and_trade_only_policy_objectives() {
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

        assert_eq!(gaming.download.base_kbps, gaming.download.observed_low_kbps);
        assert_eq!(best.download.base_kbps, best.download.observed_low_kbps);
        assert_eq!(fair.download.base_kbps, fair.download.observed_low_kbps);
        assert_eq!(gaming.download.base_kbps, best.download.base_kbps);
        assert_eq!(best.download.base_kbps, fair.download.base_kbps);
        assert_eq!(gaming.download.maximum_kbps, best.download.maximum_kbps);
        assert_eq!(best.download.maximum_kbps, fair.download.maximum_kbps);
        assert_eq!(
            gaming.download.maximum_kbps,
            gaming.download.absolute_cap_kbps
        );
        assert_eq!(best.download.maximum_kbps, best.download.absolute_cap_kbps);
        assert_eq!(fair.download.maximum_kbps, fair.download.absolute_cap_kbps);
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
    fn activity_threshold_stays_inside_low_rate_directional_minimums() {
        for profile in [
            AutotuneProfile::Gaming,
            AutotuneProfile::GamingExtreme,
            AutotuneProfile::BestOverall,
            AutotuneProfile::VariableLink,
            AutotuneProfile::Fair,
        ] {
            let proposal = build_proposal_for_profile(
                &[391.0, 302.0],
                &[885.0, 1_018.0],
                LatencyBaseline {
                    median_ms: 12.065,
                    p95_ms: 30.100,
                    samples: 14,
                },
                LinkKind::Ethernet,
                profile,
            )
            .unwrap();

            assert_eq!(proposal.active_threshold_kbps, 100);
            assert!(proposal.active_threshold_kbps <= proposal.download.minimum_kbps);
            assert!(proposal.active_threshold_kbps <= proposal.upload.minimum_kbps);
        }
    }

    #[test]
    fn activity_threshold_retains_the_high_rate_safety_cap() {
        let proposal = build_proposal_for_profile(
            &[500_000.0, 510_000.0],
            &[300_000.0, 310_000.0],
            LatencyBaseline {
                median_ms: 2.0,
                p95_ms: 3.0,
                samples: 20,
            },
            LinkKind::Ethernet,
            AutotuneProfile::Fair,
        )
        .unwrap();

        assert_eq!(proposal.active_threshold_kbps, 20_000);
        assert!(proposal.active_threshold_kbps <= proposal.download.minimum_kbps);
        assert!(proposal.active_threshold_kbps <= proposal.upload.minimum_kbps);
    }

    #[test]
    fn no_profile_applies_an_unmeasured_initial_rate_haircut_or_growth() {
        for samples in [
            vec![99_000.0, 100_000.0, 101_000.0],
            vec![40_000.0, 80_000.0, 120_000.0],
        ] {
            for profile in [
                AutotuneProfile::Gaming,
                AutotuneProfile::GamingExtreme,
                AutotuneProfile::BestOverall,
                AutotuneProfile::VariableLink,
                AutotuneProfile::Fair,
            ] {
                let proposal = build_proposal_for_profile(
                    &samples,
                    &samples,
                    LatencyBaseline {
                        median_ms: 5.0,
                        p95_ms: 7.0,
                        samples: 20,
                    },
                    LinkKind::Cellular,
                    profile,
                )
                .unwrap();
                for direction in [proposal.download, proposal.upload] {
                    assert_eq!(direction.base_kbps, direction.observed_low_kbps);
                    assert_eq!(direction.maximum_kbps, direction.observed_high_kbps);
                    assert_eq!(direction.absolute_cap_kbps, direction.observed_high_kbps);
                }
            }
        }
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
        assert!(json.contains("\"schema_version\":4"));
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
        assert_eq!(proposal.download.base_kbps, 896_000);
        assert_eq!(proposal.download.maximum_kbps, 903_200);
        assert_eq!(
            proposal.download.maximum_kbps,
            proposal.download.absolute_cap_kbps
        );
        assert_eq!(proposal.overhead, 44);
        assert_eq!(proposal.mpu, 84);
        assert!(proposal.download.minimum_kbps <= proposal.download.base_kbps);
        assert!(proposal.download.base_kbps <= proposal.download.maximum_kbps);
        assert!(proposal.download.maximum_kbps <= proposal.download.absolute_cap_kbps);
    }

    #[test]
    fn variable_cellular_proposal_uses_measured_raw_bounds_without_growth() {
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
        assert_eq!(proposal.download.base_kbps, 41_800);
        assert_eq!(proposal.download.maximum_kbps, 113_500);
        assert_eq!(proposal.download.absolute_cap_kbps, 113_500);
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
            exploration_minimum_kbps: 10_000,
            runtime_minimum_kbps: Some(10_000),
            base_kbps: 20_000,
            maximum_kbps: 30_000,
            tested_safe_maximum_kbps: Some(30_000),
            exploration_cap_kbps: 35_000,
            absolute_cap_kbps: 35_000,
            service_hard_cap_kbps: None,
            ceiling_evidence: CeilingEvidence::RetainedConfiguration,
            cap_source: CeilingCapSource::RetainedConfiguration,
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
            exploration_minimum_kbps: 500_000,
            runtime_minimum_kbps: Some(500_000),
            base_kbps: 650_000,
            maximum_kbps: 800_000,
            tested_safe_maximum_kbps: Some(800_000),
            exploration_cap_kbps: 900_000,
            absolute_cap_kbps: 900_000,
            service_hard_cap_kbps: None,
            ceiling_evidence: CeilingEvidence::RetainedConfiguration,
            cap_source: CeilingCapSource::RetainedConfiguration,
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

        assert!(json.contains("\"schema_version\":4"));
        assert!(json.contains("\"profile\":\"best_overall\""));
        assert!(json.contains("\"minimum_kbps\""));
        assert!(json.contains("\"exploration_minimum_kbps\""));
        assert!(json.contains("\"tested_safe_maximum_kbps\":null"));
        assert!(json.contains("\"ceiling_evidence\":\"unvalidated_candidate\""));
        assert!(json.contains("\"adaptive_ceiling\""));
        assert!(json.contains("\"classification\":\"diffserv4\""));
        assert!(json.contains("\"overhead\":44"));
        assert!(json.contains("\"confidence\":"));
    }

    #[test]
    fn variable_link_never_manufactures_capacity_above_the_raw_control() {
        let mut proposal = build_proposal_for_profile(
            &[908_900.0, 912_700.0, 915_800.0],
            &[902_800.0, 904_500.0, 905_900.0],
            LatencyBaseline {
                median_ms: 1.5,
                p95_ms: 2.0,
                samples: 20,
            },
            LinkKind::Pppoe,
            AutotuneProfile::VariableLink,
        )
        .unwrap();

        assert!(proposal.download.maximum_kbps <= proposal.download.observed_high_kbps);
        assert!(proposal.download.absolute_cap_kbps <= proposal.download.observed_high_kbps);
        assert!(proposal.upload.maximum_kbps <= proposal.upload.observed_high_kbps);
        assert!(proposal.upload.absolute_cap_kbps <= proposal.upload.observed_high_kbps);
        assert_eq!(proposal.download.tested_safe_maximum_kbps, None);
        assert_eq!(
            proposal.download.ceiling_evidence,
            CeilingEvidence::UnvalidatedCandidate
        );

        let selected_dl = proposal.download.base_kbps;
        let selected_ul = proposal.upload.base_kbps;
        proposal
            .set_tested_safe_maximums(Some(selected_dl), Some(selected_ul))
            .unwrap();
        assert_eq!(proposal.download.maximum_kbps, selected_dl);
        assert_eq!(
            proposal.download.tested_safe_maximum_kbps,
            Some(selected_dl)
        );
        assert_eq!(
            proposal.download.ceiling_evidence,
            CeilingEvidence::ShapedValidation
        );
    }

    #[test]
    fn tested_safe_maximum_must_be_the_exact_selected_candidate() {
        let mut proposal = build_proposal_for_profile(
            &[100_000.0, 101_000.0],
            &[20_000.0, 21_000.0],
            LatencyBaseline {
                median_ms: 5.0,
                p95_ms: 7.0,
                samples: 20,
            },
            LinkKind::Cellular,
            AutotuneProfile::VariableLink,
        )
        .unwrap();
        let untested = proposal.download.base_kbps + 1;
        let error = proposal
            .set_tested_safe_maximums(Some(untested), None)
            .unwrap_err();
        assert!(error.contains("exact selected base candidate"));
        assert_eq!(proposal.download.tested_safe_maximum_kbps, None);
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
        assert_eq!(
            proposal.download.base_kbps,
            proposal.download.observed_low_kbps
        );
        assert!(proposal.revise_base_rates(0.1).is_err());
    }

    #[test]
    fn exact_measurement_base_can_test_the_measured_maximum_atomically() {
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
        let original = proposal.clone();
        assert!(proposal.download.maximum_kbps > proposal.download.observed_low_kbps);
        assert!(proposal.upload.maximum_kbps > proposal.upload.observed_low_kbps);

        proposal
            .set_measurement_base_rates(
                proposal.download.maximum_kbps,
                proposal.upload.maximum_kbps,
            )
            .unwrap();
        assert_eq!(proposal.download.base_kbps, proposal.download.maximum_kbps);
        assert_eq!(proposal.upload.base_kbps, proposal.upload.maximum_kbps);
        assert_eq!(
            proposal.download.observed_low_kbps,
            original.download.observed_low_kbps
        );
        assert_eq!(
            proposal.download.maximum_kbps,
            original.download.maximum_kbps
        );

        let before_rejection = proposal.clone();
        assert!(proposal
            .set_measurement_base_rates(
                proposal.download.maximum_kbps + 100,
                proposal.upload.exploration_minimum_kbps,
            )
            .is_err());
        assert_eq!(proposal, before_rejection);
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
                realized_kbps: 683_153,
                achieved_kbps: 683_153,
                minimum_kbps: 618_400,
                maximum_kbps: 840_100,
            },
            DirectionValidationInput {
                observed_low_kbps: 903_800,
                candidate_kbps: 755_500,
                realized_kbps: 698_955,
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

        let mut lower_goodput = validation_input(
            DirectionValidationInput {
                observed_low_kbps: 883_500,
                candidate_kbps: 738_500,
                realized_kbps: 683_153,
                achieved_kbps: 500_000,
                minimum_kbps: 618_400,
                maximum_kbps: 840_100,
            },
            DirectionValidationInput {
                observed_low_kbps: 903_800,
                candidate_kbps: 755_500,
                realized_kbps: 698_955,
                achieved_kbps: 500_000,
                minimum_kbps: 632_600,
                maximum_kbps: 859_700,
            },
        );
        lower_goodput.profile = AutotuneProfile::Fair;
        let lower_goodput = validate_shaped_candidate(lower_goodput).unwrap();
        assert!((lower_goodput.download.candidate_realization_percent - 92.505).abs() < 0.01);
        assert!(lower_goodput.download.capacity_retention_percent < 57.0);
    }

    #[test]
    fn clean_candidate_below_retention_objective_remains_safely_reviewable() {
        let mut input = validation_input(
            DirectionValidationInput {
                observed_low_kbps: 883_500,
                candidate_kbps: 777_400,
                realized_kbps: 673_424,
                achieved_kbps: 673_424,
                minimum_kbps: 618_400,
                maximum_kbps: 840_100,
            },
            DirectionValidationInput {
                observed_low_kbps: 903_800,
                candidate_kbps: 795_300,
                realized_kbps: 738_447,
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
                realized_kbps: 683_153,
                achieved_kbps: 683_153,
                minimum_kbps: 618_400,
                maximum_kbps: 840_100,
            },
            DirectionValidationInput {
                observed_low_kbps: 903_800,
                candidate_kbps: 755_500,
                realized_kbps: 698_955,
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
                realized_kbps: candidate,
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
            realized_kbps: 90_000,
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
            realized_kbps: 88_360,
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
            realized_kbps: 85_500,
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
                    realized_kbps: achieved,
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
            realized_kbps: 74_800,
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
    fn low_candidate_realization_is_reviewable_but_not_a_profile_success() {
        let direction = DirectionValidationInput {
            observed_low_kbps: 100_000,
            candidate_kbps: 90_000,
            realized_kbps: 60_000,
            achieved_kbps: 60_000,
            minimum_kbps: 40_000,
            maximum_kbps: 105_000,
        };
        let result = validate_shaped_candidate(validation_input(direction, direction)).unwrap();

        assert!(!result.pass);
        assert!(result.safety_pass);
        assert!(!result.hard_pass);
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
            realized_kbps: 1_000_000,
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
        assert!(!result.safety_pass);
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
            realized_kbps: 45_000,
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
            realized_kbps: 85_000,
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
            realized_kbps: 85_000,
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
            realized_kbps: 642_360,
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
            realized_kbps: 85_000,
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
            realized_kbps: 85_000,
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
            realized_kbps: 1,
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
            realized_kbps: achieved_kbps,
            achieved_kbps,
            icmp_delta_ms: 0.0,
            transport_delta_ms: effective_delta_ms,
            transport_censored: false,
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

    fn censored_search_observation(
        candidate_kbps: u64,
        achieved_kbps: u64,
        lower_bound_ms: f64,
    ) -> SearchObservation {
        SearchObservation {
            candidate_kbps,
            realized_kbps: achieved_kbps,
            achieved_kbps,
            icmp_delta_ms: 8.0,
            transport_delta_ms: lower_bound_ms,
            transport_censored: true,
            loss_percent: 0.0,
            cpu_percent: 40.0,
        }
    }

    #[test]
    fn censored_transport_drives_descent_but_only_returns_manual_review() {
        let profile = AutotuneProfile::BestOverall;
        let upper = censored_search_observation(100_000, 95_000, 4_990.0);
        let first = optimize_profile_direction(ProfileSearchInput {
            profile,
            direction: SearchDirection::Download,
            observed_low_kbps: 100_000,
            minimum_kbps: 70_000,
            upper_kbps: 100_000,
            thresholds: profile.validation_thresholds(),
            uncertainty_percent: 1.5,
            max_attempts: 2,
            observations: vec![upper],
        })
        .unwrap();
        assert_eq!(first.action, ProfileSearchAction::Test);
        assert!(!first.metrics[0].measurement_reliable);
        assert!(!first.metrics[0].safety_pass);
        assert!(!first.metrics[0].target_met);
        assert!(first.metrics[0].manual_reviewable);
        assert!(first.next_candidate_kbps.is_some());

        let terminal = optimize_profile_direction(ProfileSearchInput {
            profile,
            direction: SearchDirection::Download,
            observed_low_kbps: 100_000,
            minimum_kbps: 70_000,
            upper_kbps: 100_000,
            thresholds: profile.validation_thresholds(),
            uncertainty_percent: 1.5,
            max_attempts: 2,
            observations: vec![upper, censored_search_observation(70_000, 67_000, 4_990.0)],
        })
        .unwrap();
        assert_eq!(terminal.action, ProfileSearchAction::Fallback);
        assert_eq!(terminal.reason, "transport-deadline-censored-review");
        let selected = terminal.selected_index.expect("censored Review point");
        assert_eq!(terminal.observations[selected].candidate_kbps, 100_000);
        assert!(terminal.observations[selected].transport_censored);
        assert!(!terminal.metrics[selected].target_met);
        let options = terminal.review_options();
        assert!(!options.is_empty());
        assert!(options[0].transport_censored);
        assert!(options[0].manual_reviewable);
        assert!(!options[0].auto_apply_candidate);
        let json = terminal.to_json();
        assert!(json.contains("\"schema_version\":4"));
        assert!(json.contains("\"transport_censored\":true"));
    }

    #[test]
    fn exact_safe_search_point_outranks_higher_censored_throughput() {
        let profile = AutotuneProfile::BestOverall;
        let result = optimize_profile_direction(ProfileSearchInput {
            profile,
            direction: SearchDirection::Download,
            observed_low_kbps: 100_000,
            minimum_kbps: 70_000,
            upper_kbps: 100_000,
            thresholds: profile.validation_thresholds(),
            uncertainty_percent: 1.5,
            max_attempts: 2,
            observations: vec![
                censored_search_observation(100_000, 95_000, 4_990.0),
                search_observation(70_000, 67_000, 20.0),
            ],
        })
        .unwrap();
        let selected = result.selected_index.expect("exact safe point");
        assert_eq!(result.observations[selected].candidate_kbps, 70_000);
        assert!(!result.observations[selected].transport_censored);
        assert!(result.metrics[selected].safety_pass);
        assert!(result.metrics[selected].target_met);
        assert!(!result.review_options()[0].transport_censored);
    }

    #[test]
    fn every_profile_retains_a_bounded_pareto_set_of_exact_direction_candidates() {
        let observations = vec![
            search_observation(100_000, 95_000, 50.0),
            search_observation(80_000, 78_000, 20.0),
            search_observation(60_000, 59_000, 4.0),
        ];
        for profile in [
            AutotuneProfile::Gaming,
            AutotuneProfile::GamingExtreme,
            AutotuneProfile::BestOverall,
            AutotuneProfile::VariableLink,
            AutotuneProfile::Fair,
        ] {
            let result = optimize_profile_direction(ProfileSearchInput {
                profile,
                direction: SearchDirection::Download,
                observed_low_kbps: 100_000,
                minimum_kbps: 60_000,
                upper_kbps: 100_000,
                thresholds: profile.validation_thresholds(),
                uncertainty_percent: 1.5,
                max_attempts: observations.len(),
                observations: observations.clone(),
            })
            .unwrap();
            assert_ne!(result.action, ProfileSearchAction::Test, "{profile:?}");
            assert_ne!(
                result.action,
                ProfileSearchAction::Inconclusive,
                "{profile:?}"
            );
            let options = result.review_options();
            assert_eq!(options.len(), MAX_PROFILE_REVIEW_OPTIONS, "{profile:?}");
            assert_eq!(options[0].role, ProfileSearchOptionRole::Recommended);
            let rates = options
                .iter()
                .map(|option| option.candidate_kbps)
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(rates.len(), options.len(), "{profile:?}");
            assert!(rates.iter().all(|rate| {
                observations
                    .iter()
                    .any(|observation| observation.candidate_kbps == *rate)
            }));
            let json = result.to_json();
            assert!(json.contains("\"schema_version\":4"));
            assert_eq!(
                json.matches("\"pair_confirmation_required\":true").count(),
                3
            );
        }
    }

    #[test]
    fn every_profile_whole_link_candidates_are_bounded_deduplicated_and_exact() {
        for profile in [
            AutotuneProfile::Gaming,
            AutotuneProfile::GamingExtreme,
            AutotuneProfile::BestOverall,
            AutotuneProfile::VariableLink,
            AutotuneProfile::Fair,
        ] {
            let search = |direction, observations: Vec<SearchObservation>| {
                optimize_profile_direction(ProfileSearchInput {
                    profile,
                    direction,
                    observed_low_kbps: 100_000,
                    minimum_kbps: 60_000,
                    upper_kbps: 100_000,
                    thresholds: profile.validation_thresholds(),
                    uncertainty_percent: 1.5,
                    max_attempts: observations.len(),
                    observations,
                })
                .unwrap()
            };
            let download = search(
                SearchDirection::Download,
                vec![
                    search_observation(100_000, 95_000, 50.0),
                    search_observation(80_000, 78_000, 20.0),
                    search_observation(60_000, 59_000, 4.0),
                ],
            );
            let upload = search(
                SearchDirection::Upload,
                vec![
                    search_observation(100_000, 94_000, 55.0),
                    search_observation(80_000, 77_000, 22.0),
                    search_observation(60_000, 58_000, 3.0),
                ],
            );

            let pairs = profile_pair_candidates(&download, &upload).unwrap();
            assert_eq!(pairs.len(), MAX_PROFILE_REVIEW_OPTIONS, "{profile:?}");
            assert_eq!(
                pairs[0].role,
                ProfileSearchOptionRole::Recommended,
                "{profile:?}"
            );
            let exact_pairs = pairs
                .iter()
                .map(|pair| (pair.download_kbps, pair.upload_kbps))
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(exact_pairs.len(), pairs.len(), "{profile:?}");
            assert!(
                pairs.iter().all(|pair| {
                    download.observations[pair.download_index].candidate_kbps == pair.download_kbps
                        && upload.observations[pair.upload_index].candidate_kbps == pair.upload_kbps
                }),
                "{profile:?}"
            );
        }
    }

    #[test]
    fn every_profile_can_preserve_repeatable_manual_only_evidence() {
        for profile in [
            AutotuneProfile::Gaming,
            AutotuneProfile::GamingExtreme,
            AutotuneProfile::BestOverall,
            AutotuneProfile::VariableLink,
            AutotuneProfile::Fair,
        ] {
            let result = optimize_profile_direction(ProfileSearchInput {
                profile,
                direction: SearchDirection::Upload,
                observed_low_kbps: 100_000,
                minimum_kbps: 100_000,
                upper_kbps: 100_000,
                thresholds: profile.validation_thresholds(),
                uncertainty_percent: 1.5,
                max_attempts: 3,
                observations: vec![
                    search_observation(100_000, 60_000, 100.0),
                    search_observation(100_000, 61_000, 95.0),
                    search_observation(100_000, 62_000, 90.0),
                ],
            })
            .unwrap();
            assert_eq!(result.action, ProfileSearchAction::Fallback, "{profile:?}");
            assert_eq!(result.reason, "bounded-low-realization-review");
            let options = result.review_options();
            assert_eq!(options.len(), 1, "{profile:?}");
            assert_eq!(options[0].candidate_kbps, 100_000);
            assert!(!options[0].controlled);
            assert!(options[0].manual_reviewable);
            assert!(!options[0].auto_apply_candidate);
            assert_eq!(result.runtime_minimum_index, None);
        }
    }

    #[test]
    fn variable_link_profile_separates_exploration_and_retention() {
        assert_eq!(
            AutotuneProfile::parse("variable_link"),
            Some(AutotuneProfile::VariableLink)
        );
        let proposal = build_proposal_for_profile(
            &[100_000.0, 100_000.0, 100_000.0],
            &[50_000.0, 50_000.0, 50_000.0],
            LatencyBaseline {
                median_ms: 10.0,
                p95_ms: 12.0,
                samples: 10,
            },
            LinkKind::Cellular,
            AutotuneProfile::VariableLink,
        )
        .unwrap();
        assert_eq!(proposal.target_grade, "B");
        assert_eq!(proposal.download.minimum_kbps, 35_000);
        assert_eq!(
            proposal
                .validation_thresholds
                .capacity_retention_min_percent,
            70.0
        );
        assert!(proposal.adaptive_ceiling_enabled);
    }

    #[test]
    fn standard_gaming_never_explores_below_seventy_percent() {
        let proposal = build_proposal_for_profile(
            &[1_000_000.0, 1_000_000.0, 1_000_000.0],
            &[200_000.0, 200_000.0, 200_000.0],
            LatencyBaseline {
                median_ms: 2.0,
                p95_ms: 3.0,
                samples: 10,
            },
            LinkKind::Ethernet,
            AutotuneProfile::Gaming,
        )
        .unwrap();
        assert_eq!(proposal.download.minimum_kbps, 700_000);
        assert_eq!(proposal.upload.minimum_kbps, 140_000);
    }

    #[test]
    fn extreme_gaming_uses_directional_capacity_aware_exploration_floors() {
        let wide = build_proposal_for_profile(
            &[1_000_000.0, 1_000_000.0, 1_000_000.0],
            &[200_000.0, 200_000.0, 200_000.0],
            LatencyBaseline {
                median_ms: 2.0,
                p95_ms: 3.0,
                samples: 10,
            },
            LinkKind::Ethernet,
            AutotuneProfile::GamingExtreme,
        )
        .unwrap();
        assert_eq!(wide.download.minimum_kbps, 250_000);
        assert_eq!(wide.upload.minimum_kbps, 60_000);

        let narrow = build_proposal_for_profile(
            &[10_000.0, 10_000.0, 10_000.0],
            &[1_000.0, 1_000.0, 1_000.0],
            LatencyBaseline {
                median_ms: 20.0,
                p95_ms: 22.0,
                samples: 10,
            },
            LinkKind::Cellular,
            AutotuneProfile::GamingExtreme,
        )
        .unwrap();
        assert_eq!(narrow.download.minimum_kbps, 7_000);
        assert_eq!(narrow.upload.minimum_kbps, 700);
    }

    #[test]
    fn extreme_gaming_accepts_only_ordered_measured_runtime_minima() {
        let mut proposal = build_proposal_for_profile(
            &[1_000_000.0, 1_000_000.0, 1_000_000.0],
            &[200_000.0, 200_000.0, 200_000.0],
            LatencyBaseline {
                median_ms: 2.0,
                p95_ms: 3.0,
                samples: 10,
            },
            LinkKind::Ethernet,
            AutotuneProfile::GamingExtreme,
        )
        .unwrap();
        proposal
            .set_measured_runtime_minimums(500_000, 100_000)
            .unwrap();
        assert_eq!(proposal.download.minimum_kbps, 500_000);
        assert_eq!(proposal.upload.minimum_kbps, 100_000);
        assert!(proposal
            .set_measured_runtime_minimums(1_000_100, 100_000)
            .is_err());

        // The supervisor first moves base to the exact selected search point,
        // then writes that same tested point as the runtime minimum. A clean
        // observed-low selection therefore preserves minimum <= base without
        // widening the setter to unselected absolute-cap values.
        proposal
            .revise_base_rates_by_direction(1_000_000.0 / 820_000.0, 200_000.0 / 164_000.0)
            .unwrap();
        proposal
            .set_measured_runtime_minimums(1_000_000, 200_000)
            .unwrap();
        assert_eq!(proposal.download.minimum_kbps, proposal.download.base_kbps);
        assert_eq!(proposal.upload.minimum_kbps, proposal.upload.base_kbps);
    }

    #[test]
    fn one_sided_topology_binds_only_its_still_shaped_runtime_minimum() {
        let mut upload_only = build_proposal_for_profile(
            &[400_000.0, 400_000.0, 400_000.0],
            &[60_000.0, 60_000.0, 60_000.0],
            LatencyBaseline {
                median_ms: 20.0,
                p95_ms: 25.0,
                samples: 10,
            },
            LinkKind::Cellular,
            AutotuneProfile::VariableLink,
        )
        .unwrap();
        let untouched_download_floor = upload_only.download.minimum_kbps;
        upload_only
            .set_measured_upload_runtime_minimum(45_000)
            .unwrap();
        assert_eq!(upload_only.download.minimum_kbps, untouched_download_floor);
        assert_eq!(upload_only.upload.minimum_kbps, 45_000);

        let mut download_only = build_proposal_for_profile(
            &[1_000_000.0, 1_000_000.0, 1_000_000.0],
            &[200_000.0, 200_000.0, 200_000.0],
            LatencyBaseline {
                median_ms: 2.0,
                p95_ms: 3.0,
                samples: 10,
            },
            LinkKind::Ethernet,
            AutotuneProfile::GamingExtreme,
        )
        .unwrap();
        let untouched_upload_floor = download_only.upload.minimum_kbps;
        download_only
            .set_measured_download_runtime_minimum(500_000)
            .unwrap();
        assert_eq!(download_only.download.minimum_kbps, 500_000);
        assert_eq!(download_only.upload.minimum_kbps, untouched_upload_floor);
    }

    #[test]
    fn extreme_gaming_runtime_minimum_is_the_lowest_tested_a_plus_candidate() {
        let result = optimize_profile_direction(ProfileSearchInput {
            profile: AutotuneProfile::GamingExtreme,
            direction: SearchDirection::Download,
            observed_low_kbps: 1_000_000,
            minimum_kbps: 250_000,
            upper_kbps: 1_000_000,
            thresholds: AutotuneProfile::GamingExtreme.validation_thresholds(),
            uncertainty_percent: 1.5,
            max_attempts: 12,
            observations: vec![
                search_observation(504_000, 484_000, 6.0),
                search_observation(400_000, 384_000, 3.0),
                search_observation(500_000, 480_000, 4.0),
            ],
        })
        .unwrap();
        assert_eq!(result.action, ProfileSearchAction::Complete);
        let selected_index = result.selected_index.unwrap();
        assert_eq!(result.observations[selected_index].candidate_kbps, 500_000);
        let runtime_index = result.runtime_minimum_index.unwrap();
        assert_eq!(result.observations[runtime_index].candidate_kbps, 400_000);
        assert!(!result.metrics[selected_index].capacity_objective_met);
    }

    #[test]
    fn variable_link_knee_uses_the_last_tested_useful_point() {
        let result = profile_search(
            AutotuneProfile::VariableLink,
            100_000,
            35_000,
            vec![
                search_observation(80_000, 76_000, 100.0),
                search_observation(65_000, 62_000, 60.0),
                search_observation(50_000, 48_000, 58.0),
                search_observation(35_000, 34_000, 57.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Fallback);
        assert_eq!(result.reason, "measured-variable-link-knee");
        assert!(result.knee_detected);
        let runtime_index = result.runtime_minimum_index.unwrap();
        assert_eq!(result.observations[runtime_index].candidate_kbps, 65_000);
        assert!(result
            .observations
            .iter()
            .any(|observation| observation.candidate_kbps == 65_000));
    }

    #[test]
    fn variable_link_retests_a_lower_exact_rate_before_manual_review() {
        let result = profile_search(
            AutotuneProfile::VariableLink,
            425_000,
            148_800,
            vec![
                search_observation(334_500, 262_970, 78.9),
                search_observation(334_500, 264_000, 104.8),
                search_observation(334_500, 264_173, 97.8),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Test);
        assert_eq!(
            result.reason,
            "lower-variable-candidate-to-establish-shaper-control"
        );
        assert_eq!(result.next_candidate_kbps, Some(292_200));
        assert!(result
            .metrics
            .iter()
            .all(|metrics| { !metrics.safety_pass && metrics.manual_reviewable }));
    }

    #[test]
    fn variable_link_bounded_low_realization_does_not_invent_a_runtime_floor() {
        let result = profile_search(
            AutotuneProfile::VariableLink,
            425_000,
            292_200,
            vec![
                search_observation(292_200, 220_000, 95.0),
                search_observation(292_200, 222_000, 100.0),
                search_observation(292_200, 221_000, 105.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Fallback);
        assert_eq!(result.reason, "bounded-low-realization-review");
        assert!(!result.knee_detected);
        assert!(!result.no_cake_effect);
        assert!(!result.noisy);
        let selected_index = result.selected_index.expect("manual review point");
        assert_eq!(result.runtime_minimum_index, None);
        assert_eq!(result.observations[selected_index].candidate_kbps, 292_200);
        assert!(!result.metrics[selected_index].safety_pass);
        assert!(result.metrics[selected_index].manual_reviewable);
        let json = result.to_json();
        assert!(json.contains("\"manual_reviewable\":true"));
        assert!(json.contains("\"runtime_minimum_kbps\":null"));
        assert!(json.contains("\"action\":\"fallback\""));
    }

    #[test]
    fn variable_link_preserves_an_earlier_manual_fallback_after_deeper_descent() {
        // Exact shape of the r101 disposable-VM failure: 52.4 Mbit/s had a
        // repeatable, target-passing, bounded manual-review result. Deeper
        // exploration then fell below the 50% trust floor. The later points
        // must not erase the earlier exact candidate.
        let result = optimize_profile_direction(ProfileSearchInput {
            profile: AutotuneProfile::VariableLink,
            direction: SearchDirection::Download,
            observed_low_kbps: 57_900,
            minimum_kbps: 28_900,
            upper_kbps: 85_000,
            thresholds: AutotuneProfile::VariableLink.validation_thresholds(),
            uncertainty_percent: 1.5,
            max_attempts: 12,
            observations: vec![
                search_observation(85_000, 49_862, 6.669),
                search_observation(85_000, 59_783, 5.272),
                search_observation(85_000, 47_129, 3.074),
                search_observation(52_400, 34_753, 9.178),
                search_observation(52_400, 32_997, 4.629),
                search_observation(52_400, 33_587, 1.610),
                search_observation(36_700, 19_551, 5.528),
                search_observation(36_700, 20_873, 3.942),
                search_observation(36_700, 21_519, 2.454),
                search_observation(28_900, 14_891, 7.980),
                search_observation(28_900, 18_231, 2.996),
                search_observation(28_900, 15_805, 6.268),
            ],
        })
        .unwrap();

        assert_eq!(result.action, ProfileSearchAction::Fallback);
        assert_eq!(result.reason, "bounded-low-realization-review");
        assert!(!result.knee_detected);
        assert_eq!(result.runtime_minimum_index, None);
        let selected_index = result.selected_index.expect("earlier manual fallback");
        assert_eq!(result.observations[selected_index].candidate_kbps, 52_400);
        assert!(result.metrics[selected_index].manual_reviewable);
        assert!(result.metrics[selected_index].target_met);
        assert!(!result.metrics[selected_index].safety_pass);
        assert!(result.observations[selected_index].achieved_kbps >= 32_997);
    }

    #[test]
    fn variable_link_preserves_a_two_sample_hardware_fallback_after_deeper_descent() {
        // Exact evidence shape from the r107 disposable-VM gate: the useful
        // 61.3 Mbit/s candidate had two corroborating 50-80% realization
        // samples.  Their agreement correctly authorized deeper exploration,
        // whose later points retained less than half of the raw reference.
        // Exhausting that descent must not erase the earlier exact rate.
        let result = optimize_profile_direction(ProfileSearchInput {
            profile: AutotuneProfile::VariableLink,
            direction: SearchDirection::Download,
            observed_low_kbps: 53_000,
            minimum_kbps: 26_500,
            upper_kbps: 85_000,
            thresholds: AutotuneProfile::VariableLink.validation_thresholds(),
            uncertainty_percent: 1.5,
            max_attempts: 8,
            observations: vec![
                search_observation(61_300, 34_962, 20.0),
                search_observation(61_300, 36_655, 22.0),
                search_observation(38_900, 22_796, 25.0),
                search_observation(38_900, 20_579, 28.0),
                search_observation(38_900, 22_398, 30.0),
                search_observation(26_500, 16_544, 35.0),
                search_observation(26_500, 14_363, 38.0),
                search_observation(26_500, 14_864, 40.0),
            ],
        })
        .unwrap();

        assert_eq!(result.action, ProfileSearchAction::Fallback);
        assert_eq!(result.reason, "bounded-low-realization-review");
        assert_eq!(result.runtime_minimum_index, None);
        let selected_index = result.selected_index.expect("two-sample fallback");
        assert_eq!(result.observations[selected_index].candidate_kbps, 61_300);
        assert!(result.metrics[selected_index].manual_reviewable);
        let options = result.review_options();
        assert!(!options.is_empty());
        assert!(options.iter().any(|option| {
            option.candidate_kbps == 61_300
                && option.manual_reviewable
                && !option.controlled
                && !option.auto_apply_candidate
        }));
    }

    #[test]
    fn two_sample_manual_fallback_still_requires_rate_corroboration() {
        let result = optimize_profile_direction(ProfileSearchInput {
            profile: AutotuneProfile::VariableLink,
            direction: SearchDirection::Download,
            observed_low_kbps: 53_000,
            minimum_kbps: 26_500,
            upper_kbps: 61_300,
            thresholds: AutotuneProfile::VariableLink.validation_thresholds(),
            uncertainty_percent: 1.5,
            max_attempts: 2,
            observations: vec![
                search_observation(61_300, 34_000, 20.0),
                search_observation(61_300, 40_000, 22.0),
            ],
        })
        .unwrap();

        assert_eq!(result.action, ProfileSearchAction::Inconclusive);
        assert!(result.selected_index.is_none());
        assert!(result.review_options().is_empty());
    }

    #[test]
    fn variable_link_never_selects_an_uncorroborated_group_outlier() {
        let result = profile_search(
            AutotuneProfile::VariableLink,
            70_000,
            70_000,
            vec![
                // The first two achieved rates corroborate one another.  The
                // third observation has the worst latency but is an achieved-
                // rate outlier, so it cannot represent this manual option.
                search_observation(70_000, 50_000, 20.0),
                search_observation(70_000, 51_000, 25.0),
                search_observation(70_000, 40_000, 55.0),
            ],
        );

        assert_eq!(result.action, ProfileSearchAction::Fallback);
        assert_eq!(result.reason, "bounded-low-realization-review");
        let selected_index = result.selected_index.expect("corroborated fallback");
        assert_eq!(result.observations[selected_index].achieved_kbps, 51_000);
        assert_eq!(result.metrics[selected_index].effective_delta_ms, 25.0);
        assert!(result.metrics[selected_index].manual_reviewable);
        assert_eq!(result.runtime_minimum_index, None);
    }

    #[test]
    fn variable_link_sub_fifty_realization_remains_hard() {
        let observations = vec![
            search_observation(292_200, 140_000, 95.0),
            search_observation(292_200, 141_000, 100.0),
            search_observation(292_200, 140_500, 105.0),
        ];
        let result = profile_search(
            AutotuneProfile::VariableLink,
            425_000,
            292_200,
            observations.clone(),
        );
        assert_eq!(result.action, ProfileSearchAction::Inconclusive);
        assert_eq!(result.reason, "variable-candidate-realization-inconclusive");
        assert!(result.selected_index.is_none());
        assert!(result
            .metrics
            .iter()
            .all(|metrics| { !metrics.safety_pass && !metrics.manual_reviewable }));
        assert!(result.review_options().is_empty());

        let terminal = terminate_profile_direction_at_measured_boundary(
            ProfileSearchInput {
                profile: AutotuneProfile::VariableLink,
                direction: SearchDirection::Download,
                observed_low_kbps: 425_000,
                minimum_kbps: 292_200,
                upper_kbps: 292_200,
                thresholds: AutotuneProfile::VariableLink.validation_thresholds(),
                uncertainty_percent: 1.5,
                max_attempts: 3,
                observations,
            },
            "candidate-observation-starved",
        )
        .unwrap();
        assert_eq!(terminal.action, ProfileSearchAction::Inconclusive);
        assert_eq!(terminal.reason, "candidate-observation-starved");
        assert!(terminal.selected_index.is_none());
        assert!(terminal.review_options().is_empty());
    }

    #[test]
    fn variable_link_repeated_subtrust_measurements_descend_to_an_exact_lower_candidate() {
        // Exact rate shape from the r213 disposable-VM failure.  All three
        // measurements are below the 50% review trust floor, so none may
        // become a proposal.  Their achieved and realized rates nevertheless
        // corroborate one another and may authorize one lower exact probe.
        let result = optimize_profile_direction(ProfileSearchInput {
            profile: AutotuneProfile::VariableLink,
            direction: SearchDirection::Download,
            observed_low_kbps: 324_500,
            minimum_kbps: 162_200,
            upper_kbps: 344_900,
            thresholds: AutotuneProfile::VariableLink.validation_thresholds(),
            uncertainty_percent: 1.5,
            max_attempts: 12,
            observations: vec![
                SearchObservation {
                    candidate_kbps: 344_900,
                    realized_kbps: 166_340,
                    achieved_kbps: 160_039,
                    icmp_delta_ms: 15.180,
                    transport_delta_ms: 67.762,
                    transport_censored: false,
                    loss_percent: 0.0,
                    cpu_percent: 70.691,
                },
                SearchObservation {
                    candidate_kbps: 344_900,
                    realized_kbps: 160_549,
                    achieved_kbps: 154_486,
                    icmp_delta_ms: 12.890,
                    transport_delta_ms: 18.466,
                    transport_censored: false,
                    loss_percent: 0.0,
                    cpu_percent: 69.517,
                },
                SearchObservation {
                    candidate_kbps: 344_900,
                    realized_kbps: 164_705,
                    achieved_kbps: 158_520,
                    icmp_delta_ms: 11.490,
                    transport_delta_ms: 44.415,
                    transport_censored: false,
                    loss_percent: 0.0,
                    cpu_percent: 69.479,
                },
            ],
        })
        .unwrap();

        assert_eq!(result.action, ProfileSearchAction::Test);
        assert_eq!(
            result.reason,
            "lower-variable-candidate-to-establish-shaper-control"
        );
        assert_eq!(result.next_candidate_kbps, Some(171_700));
        assert!(result.selected_index.is_none());
        assert!(result
            .metrics
            .iter()
            .all(|metrics| { !metrics.safety_pass && !metrics.manual_reviewable }));
        assert!(result.review_options().is_empty());
    }

    #[test]
    fn variable_link_subtrust_diagnostic_descent_uses_cpu_saturation_to_lower_the_probe() {
        let observations = [94.0, 95.0, 96.0]
            .into_iter()
            .map(|cpu_percent| SearchObservation {
                candidate_kbps: 344_900,
                realized_kbps: 162_000,
                achieved_kbps: 156_000,
                icmp_delta_ms: 15.0,
                transport_delta_ms: 20.0,
                transport_censored: false,
                loss_percent: 0.0,
                cpu_percent,
            })
            .collect();
        let result = optimize_profile_direction(ProfileSearchInput {
            profile: AutotuneProfile::VariableLink,
            direction: SearchDirection::Download,
            observed_low_kbps: 324_500,
            minimum_kbps: 162_200,
            upper_kbps: 344_900,
            thresholds: AutotuneProfile::VariableLink.validation_thresholds(),
            uncertainty_percent: 1.5,
            max_attempts: 12,
            observations,
        })
        .unwrap();

        assert_eq!(result.action, ProfileSearchAction::Test);
        assert_eq!(
            result.reason,
            "lower-variable-candidate-to-establish-shaper-control"
        );
        assert_eq!(result.next_candidate_kbps, Some(173_400));
        assert!(result.selected_index.is_none());
        assert!(result.review_options().is_empty());
        assert!(result.metrics.iter().all(|metrics| metrics.resource_safe));
    }

    #[test]
    fn variable_link_flat_target_miss_returns_exact_tested_fallback() {
        let result = profile_search(
            AutotuneProfile::VariableLink,
            100_000,
            35_000,
            vec![
                search_observation(80_000, 76_000, 100.0),
                search_observation(65_000, 62_000, 99.0),
                search_observation(50_000, 48_000, 98.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Fallback);
        assert_eq!(result.reason, "queue-outside-cake-control-target-unmet");
        assert!(result.no_cake_effect);
        let selected = result.selected_index.expect("tested safe fallback");
        assert_eq!(result.runtime_minimum_index, None);
        assert_eq!(result.observations[selected].candidate_kbps, 80_000);
        assert!(result.metrics[selected].safety_pass);
        assert!(!result.metrics[selected].target_met);
    }

    #[test]
    fn variable_link_target_met_flat_gradient_returns_tested_directional_fallback() {
        let result = profile_search(
            AutotuneProfile::VariableLink,
            100_000,
            35_000,
            vec![
                search_observation(80_000, 76_000, 50.0),
                search_observation(65_000, 62_000, 49.0),
                search_observation(50_000, 48_000, 48.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Fallback);
        assert_eq!(result.reason, "queue-outside-cake-control");
        assert!(result.no_cake_effect);
        assert!(!result.knee_detected);
        assert!(!result.noisy);
        let selected_index = result.selected_index.unwrap();
        assert_eq!(result.runtime_minimum_index, None);
        assert_eq!(result.observations[selected_index].candidate_kbps, 80_000);
        assert!(result.metrics[selected_index].safety_pass);
        assert!(result.metrics[selected_index].target_met);
        assert!(result.metrics[selected_index].retention_percent >= 50.0);
        let json = result.to_json();
        assert!(json.contains("\"action\":\"fallback\""));
        assert!(json.contains("\"runtime_minimum_kbps\":null"));
        assert!(json.contains("\"no_cake_effect\":true"));
        assert!(json.contains("\"inconclusive\":false"));
    }

    #[test]
    fn variable_link_preserves_a_higher_passing_cake_ceiling_than_payload() {
        let result = optimize_profile_direction(ProfileSearchInput {
            profile: AutotuneProfile::VariableLink,
            direction: SearchDirection::Download,
            // Payload reference from repeated shaped controls.
            observed_low_kbps: 666_400,
            minimum_kbps: 333_200,
            // Exact applied CAKE candidate; it legitimately exceeds payload.
            upper_kbps: 723_100,
            thresholds: AutotuneProfile::VariableLink.validation_thresholds(),
            uncertainty_percent: 1.5,
            max_attempts: 12,
            observations: vec![
                search_observation(723_100, 666_427, 2.0),
                search_observation(623_100, 575_000, 1.2),
                search_observation(523_100, 483_000, 1.0),
            ],
        })
        .unwrap();

        assert_eq!(result.action, ProfileSearchAction::Fallback);
        assert_eq!(result.reason, "queue-outside-cake-control");
        assert!(result.no_cake_effect);
        assert!(!result.knee_detected);
        let selected = result.selected_index.expect("passing CAKE ceiling");
        assert_eq!(result.observations[selected].candidate_kbps, 723_100);
        assert_eq!(result.observations[selected].achieved_kbps, 666_427);
        assert!(result.metrics[selected].target_met);
        assert_eq!(result.runtime_minimum_index, None);
        let json = result.to_json();
        assert!(json.contains("\"candidate_kbps\":723100"));
        assert!(json.contains("\"achieved_kbps\":666427"));
        assert!(json.contains("\"runtime_minimum_kbps\":null"));
    }

    #[test]
    fn variable_link_unobserved_descent_uses_the_same_fixed_step_to_the_exact_floor() {
        let observed_low = 357_400;
        let minimum = 178_700;
        let mut current = 377_800;
        let mut candidates = Vec::new();
        while let Some(next) =
            next_variable_link_unobserved_candidate(observed_low, minimum, current).unwrap()
        {
            candidates.push(next);
            current = next;
        }
        assert_eq!(candidates, vec![324_100, 270_400, 216_700, 178_700]);
        assert_eq!(
            next_variable_link_unobserved_candidate(observed_low, minimum, minimum).unwrap(),
            None
        );
        assert!(next_variable_link_unobserved_candidate(0, minimum, current).is_err());
        assert!(
            next_variable_link_unobserved_candidate(observed_low, current + 1, current).is_err()
        );
    }

    #[test]
    fn variable_link_flat_gradient_below_retention_floor_remains_reviewable() {
        let result = profile_search(
            AutotuneProfile::VariableLink,
            100_000,
            35_000,
            vec![
                search_observation(49_000, 48_000, 50.0),
                search_observation(42_000, 41_000, 49.0),
                search_observation(35_000, 34_000, 48.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Fallback);
        assert_eq!(result.reason, "queue-outside-cake-control-target-unmet");
        assert!(result.no_cake_effect);
        let selected = result.selected_index.expect("tested safe fallback");
        assert_eq!(result.runtime_minimum_index, None);
        assert_eq!(result.observations[selected].candidate_kbps, 49_000);
        assert!(result.metrics[selected].safety_pass);
        assert!(result.metrics[selected].retention_percent < 50.0);
    }

    #[test]
    fn variable_link_floor_without_plateau_keeps_runtime_minimum_unresolved() {
        let result = profile_search(
            AutotuneProfile::VariableLink,
            100_000,
            35_000,
            vec![
                search_observation(80_000, 76_000, 120.0),
                search_observation(65_000, 62_000, 90.0),
                search_observation(50_000, 48_000, 60.0),
                search_observation(35_000, 34_000, 30.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Fallback);
        assert_eq!(
            result.reason,
            "exploration-floor-reached-without-latency-knee"
        );
        assert!(!result.knee_detected);
        let selected = result.selected_index.expect("tested safe fallback");
        assert_eq!(result.runtime_minimum_index, None);
        assert!(result.metrics[selected].safety_pass);
        assert!(result
            .observations
            .iter()
            .any(|observation| observation.candidate_kbps
                == result.observations[selected].candidate_kbps));
    }

    #[test]
    fn variable_link_floor_without_knee_returns_exact_tested_manual_fallback() {
        let result = profile_search(
            AutotuneProfile::VariableLink,
            100_000,
            35_000,
            vec![
                search_observation(80_000, 76_000, 22.0),
                search_observation(65_000, 62_000, 21.0),
                search_observation(50_000, 48_000, 17.0),
                search_observation(35_000, 34_000, 18.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Fallback);
        assert_eq!(result.reason, "exploration-floor-reached");
        assert!(!result.knee_detected);
        assert!(!result.no_cake_effect);
        assert!(!result.noisy);
        let selected_index = result.selected_index.unwrap();
        assert_eq!(result.runtime_minimum_index, None);
        assert_eq!(result.observations[selected_index].candidate_kbps, 80_000);
        assert!(result.metrics[selected_index].target_met);
        assert!(result.metrics[selected_index].retention_percent >= 50.0);
    }

    #[test]
    fn variable_link_retries_nonmonotonic_noise_then_returns_best_trusted_review() {
        let first = profile_search(
            AutotuneProfile::VariableLink,
            100_000,
            35_000,
            vec![
                search_observation(80_000, 76_000, 80.0),
                search_observation(65_000, 62_000, 100.0),
            ],
        );
        assert_eq!(first.action, ProfileSearchAction::Test);
        assert_eq!(first.next_candidate_kbps, Some(65_000));

        let exhausted = profile_search(
            AutotuneProfile::VariableLink,
            100_000,
            35_000,
            vec![
                search_observation(80_000, 76_000, 80.0),
                search_observation(65_000, 62_000, 100.0),
                search_observation(65_000, 61_500, 100.0),
                search_observation(65_000, 61_800, 100.0),
            ],
        );
        assert_eq!(exhausted.action, ProfileSearchAction::Fallback);
        assert_eq!(exhausted.reason, "noisy-link-safe-review");
        assert!(exhausted.noisy);
        assert!(!exhausted.no_cake_effect);
        let selected_index = exhausted.selected_index.unwrap();
        assert_eq!(exhausted.runtime_minimum_index, None);
        assert_eq!(
            exhausted.observations[selected_index].candidate_kbps,
            80_000
        );
        assert_eq!(exhausted.metrics[selected_index].grade, "C");
        assert!(!exhausted.metrics[selected_index].target_met);
        assert!(exhausted.metrics[selected_index].retention_percent >= 50.0);
    }

    #[test]
    fn variable_link_noisy_fallback_preserves_a_passing_high_throughput_ceiling() {
        let result = optimize_profile_direction(ProfileSearchInput {
            profile: AutotuneProfile::VariableLink,
            direction: SearchDirection::Download,
            observed_low_kbps: 668_200,
            minimum_kbps: 334_100,
            upper_kbps: 723_100,
            thresholds: AutotuneProfile::VariableLink.validation_thresholds(),
            uncertainty_percent: 1.5,
            max_attempts: 12,
            observations: vec![
                search_observation(723_100, 668_517, 52.469),
                search_observation(622_800, 575_359, 6.358),
                search_observation(522_500, 482_501, 5.569),
                search_observation(422_200, 389_255, 26.765),
                search_observation(422_200, 389_175, 20.594),
                search_observation(422_200, 389_198, 12.954),
            ],
        })
        .unwrap();

        assert_eq!(result.action, ProfileSearchAction::Fallback);
        assert_eq!(result.reason, "noisy-link-safe-review");
        assert!(result.noisy);
        assert!(!result.knee_detected);
        assert_eq!(result.runtime_minimum_index, None);
        let selected = result.selected_index.expect("target-passing ceiling");
        assert_eq!(result.observations[selected].candidate_kbps, 723_100);
        assert_eq!(result.observations[selected].achieved_kbps, 668_517);
        assert!(result.metrics[selected].safety_pass);
        assert!(result.metrics[selected].target_met);
        assert!(result.metrics[selected].capacity_objective_met);
    }

    #[test]
    fn variable_link_nonmonotonic_fallback_preserves_a_passing_ceiling() {
        let result = optimize_profile_direction(ProfileSearchInput {
            profile: AutotuneProfile::VariableLink,
            direction: SearchDirection::Download,
            observed_low_kbps: 668_200,
            minimum_kbps: 334_100,
            upper_kbps: 723_100,
            thresholds: AutotuneProfile::VariableLink.validation_thresholds(),
            uncertainty_percent: 1.5,
            max_attempts: 12,
            observations: vec![
                search_observation(723_100, 668_500, 52.0),
                search_observation(622_800, 575_000, 6.0),
                search_observation(522_500, 482_500, 30.0),
                search_observation(522_500, 482_400, 30.0),
                search_observation(522_500, 482_600, 30.0),
            ],
        })
        .unwrap();

        assert_eq!(result.action, ProfileSearchAction::Fallback);
        assert_eq!(result.reason, "noisy-link-safe-review");
        assert!(result.noisy);
        assert!(!result.knee_detected);
        assert_eq!(result.runtime_minimum_index, None);
        let selected = result.selected_index.expect("target-passing ceiling");
        assert_eq!(result.observations[selected].candidate_kbps, 723_100);
        assert_eq!(result.observations[selected].achieved_kbps, 668_500);
        assert!(result.metrics[selected].target_met);
    }

    #[test]
    fn variable_link_noisy_points_below_trust_floor_keep_a_safe_fallback() {
        let result = profile_search(
            AutotuneProfile::VariableLink,
            100_000,
            35_000,
            vec![
                search_observation(49_000, 48_000, 80.0),
                search_observation(42_000, 41_000, 100.0),
                search_observation(42_000, 40_500, 100.0),
                search_observation(42_000, 40_800, 100.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Fallback);
        assert_eq!(result.reason, "nonmonotonic-variable-link-after-retries");
        assert!(result.noisy);
        let selected = result.selected_index.expect("tested safe fallback");
        assert_eq!(result.runtime_minimum_index, None);
        assert_eq!(result.observations[selected].candidate_kbps, 49_000);
        assert!(result.metrics[selected].safety_pass);
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
    fn observation_starvation_retains_only_a_prior_corroborated_manual_point() {
        let observation = |realized_kbps, achieved_kbps, delta_ms| SearchObservation {
            candidate_kbps: 1_000,
            realized_kbps,
            achieved_kbps,
            icmp_delta_ms: 6.0,
            transport_delta_ms: delta_ms,
            transport_censored: false,
            loss_percent: 0.0,
            cpu_percent: 8.0,
        };
        let input = ProfileSearchInput {
            profile: AutotuneProfile::Fair,
            direction: SearchDirection::Download,
            observed_low_kbps: 300,
            minimum_kbps: 100,
            upper_kbps: 1_000,
            thresholds: AutotuneProfile::Fair.validation_thresholds(),
            uncertainty_percent: 1.5,
            max_attempts: 8,
            observations: vec![observation(729, 250, 10.0), observation(771, 239, 11.0)],
        };
        let open = optimize_profile_direction(input.clone()).unwrap();
        assert_eq!(open.action, ProfileSearchAction::Test);
        assert_eq!(open.next_candidate_kbps, Some(300));

        let terminal = terminate_profile_direction_at_measured_boundary(
            input.clone(),
            "candidate-observation-starved",
        )
        .unwrap();
        assert_eq!(terminal.action, ProfileSearchAction::Fallback);
        assert_eq!(terminal.reason, "candidate-observation-starved");
        assert_eq!(terminal.next_candidate_kbps, None);
        assert_eq!(terminal.observations.len(), 2);
        let selected = terminal.selected_index.expect("manual fallback");
        assert_eq!(terminal.observations[selected].candidate_kbps, 1_000);
        assert!(terminal.metrics[selected].manual_reviewable);
        assert!(!terminal.metrics[selected].safety_pass);
        let options = terminal.review_options();
        assert_eq!(options.len(), 1);
        assert!(options[0].manual_reviewable);
        assert!(!options[0].controlled);
        assert!(!options[0].auto_apply_candidate);

        let singleton = terminate_profile_direction_at_measured_boundary(
            ProfileSearchInput {
                observations: vec![observation(729, 250, 10.0)],
                ..input
            },
            "candidate-observation-starved",
        )
        .unwrap();
        assert_eq!(singleton.action, ProfileSearchAction::Inconclusive);
        assert_eq!(singleton.selected_index, None);
        assert!(singleton.review_options().is_empty());
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
    fn variable_5g_fair_result_is_reviewable_but_not_a_profile_success() {
        let download = DirectionValidationInput {
            observed_low_kbps: 140_200,
            candidate_kbps: 131_800,
            realized_kbps: 98_101,
            achieved_kbps: 98_101,
            minimum_kbps: 112_100,
            maximum_kbps: 140_200,
        };
        let upload = DirectionValidationInput {
            observed_low_kbps: 19_500,
            candidate_kbps: 19_500,
            realized_kbps: 16_259,
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
        assert!(validation.safety_pass);
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
            realized_kbps: achieved_kbps,
            achieved_kbps,
            icmp_delta_ms: 1.0,
            transport_delta_ms: 1.0,
            transport_censored: false,
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
    fn terminal_unsafe_probe_keeps_an_earlier_exact_safe_point() {
        let unsafe_observation = |achieved_kbps| SearchObservation {
            candidate_kbps: 100_000,
            realized_kbps: achieved_kbps,
            achieved_kbps,
            icmp_delta_ms: 1.0,
            transport_delta_ms: 1.0,
            transport_censored: false,
            loss_percent: 10.0,
            cpu_percent: 50.0,
        };
        let result = profile_search(
            AutotuneProfile::BestOverall,
            100_000,
            70_000,
            vec![
                search_observation(70_000, 67_000, 20.0),
                unsafe_observation(95_000),
                unsafe_observation(95_500),
                unsafe_observation(95_200),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Fallback);
        assert_eq!(result.reason, "resource-safety-failure-not-resolved");
        assert_eq!(result.selected_index, Some(0));
        assert!(result.metrics[0].safety_pass);
        assert!(!result.metrics[3].safety_pass);
    }

    #[test]
    fn terminal_unreliable_probe_keeps_extreme_runtime_minimum_bound_to_safe_point() {
        let result = profile_search(
            AutotuneProfile::GamingExtreme,
            100_000,
            25_000,
            vec![
                search_observation(70_000, 67_000, 3.0),
                search_observation(100_000, 120_000, 2.0),
                search_observation(100_000, 121_000, 2.0),
                search_observation(100_000, 122_000, 2.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Fallback);
        assert_eq!(result.reason, "repeated-candidate-realization-unreliable");
        assert_eq!(result.selected_index, Some(0));
        assert_eq!(result.runtime_minimum_index, Some(0));
        assert!(result.metrics[0].safety_pass);
        assert!(!result.metrics[3].safety_pass);
    }

    #[test]
    fn variable_attempt_limit_keeps_the_best_exact_safe_point() {
        let result = profile_search(
            AutotuneProfile::VariableLink,
            100_000,
            35_000,
            vec![
                search_observation(95_000, 90_000, 190.0),
                search_observation(85_000, 81_000, 160.0),
                search_observation(75_000, 71_000, 130.0),
                search_observation(65_000, 62_000, 100.0),
                search_observation(55_000, 52_000, 70.0),
                search_observation(45_000, 43_000, 40.0),
            ],
        );
        assert_eq!(result.action, ProfileSearchAction::Fallback);
        assert_eq!(result.reason, "bounded-attempt-limit-before-latency-knee");
        let selected = result.selected_index.expect("tested safe fallback");
        assert_eq!(result.runtime_minimum_index, None);
        assert_eq!(result.observations[selected].candidate_kbps, 45_000);
        assert!(result.metrics[selected].safety_pass);
    }

    #[test]
    fn variable_cannot_progress_terminal_resolution_keeps_a_safe_point() {
        // The cannot-progress branch is a defensive guard for future search
        // strategies. Exercise the shared terminal resolver directly so that
        // this invariant cannot regress even though today's fixed descending
        // step normally makes that branch unreachable for a valid history.
        let input = ProfileSearchInput {
            profile: AutotuneProfile::VariableLink,
            direction: SearchDirection::Download,
            observed_low_kbps: 100_000,
            minimum_kbps: 35_000,
            upper_kbps: 100_000,
            thresholds: AutotuneProfile::VariableLink.validation_thresholds(),
            uncertainty_percent: 1.5,
            max_attempts: 6,
            observations: vec![search_observation(80_000, 76_000, 80.0)],
        };
        let metrics = input
            .observations
            .iter()
            .copied()
            .map(|observation| evaluate_search_observation(&input, observation))
            .collect::<Vec<_>>();
        let (action, selected) = resolve_terminal_safe_fallback(
            &input,
            &metrics,
            ProfileSearchAction::Inconclusive,
            None,
        );
        assert_eq!(action, ProfileSearchAction::Fallback);
        assert_eq!(selected, Some(0));
        assert!(metrics[0].safety_pass);
    }

    #[test]
    fn terminal_resolution_stays_inconclusive_when_no_safe_point_exists() {
        let input = ProfileSearchInput {
            profile: AutotuneProfile::VariableLink,
            direction: SearchDirection::Download,
            observed_low_kbps: 100_000,
            minimum_kbps: 35_000,
            upper_kbps: 100_000,
            thresholds: AutotuneProfile::VariableLink.validation_thresholds(),
            uncertainty_percent: 1.5,
            max_attempts: 6,
            observations: vec![search_observation(80_000, 30_000, 80.0)],
        };
        let metrics = input
            .observations
            .iter()
            .copied()
            .map(|observation| evaluate_search_observation(&input, observation))
            .collect::<Vec<_>>();
        let (action, selected) = resolve_terminal_safe_fallback(
            &input,
            &metrics,
            ProfileSearchAction::Inconclusive,
            None,
        );
        assert_eq!(action, ProfileSearchAction::Inconclusive);
        assert_eq!(selected, None);
        assert!(!metrics[0].safety_pass);
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
            realized_kbps: achieved_kbps,
            achieved_kbps,
            icmp_delta_ms: 2.0,
            transport_delta_ms: 2.0,
            transport_censored: false,
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
