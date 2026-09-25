use super::autotune_capture_policy::AutotuneCapturePolicyId;
use crate::autotune::{
    AccessEvidenceSource, AccessMedium, AutotuneProfile, CapacityLearningPolicy,
};
use std::fmt;
use std::net::IpAddr;

pub const OPERATION_PROTOCOL_VERSION: u16 = 2;
pub const MAX_OPERATION_RECORD_BYTES: usize = 8 * 1024;
pub const MAX_CONTROL_MESSAGE_BYTES: usize = MAX_OPERATION_RECORD_BYTES * 2;
const MAX_TRAFFIC_BUDGET_BYTES: u64 = 1 << 40;

// Explicit traffic policy uses v7 (managed) or v8 (absent). Legacy capped
// requests retain byte-compatible v4/v5/v6 encoding. New Full Auto-Tune
// requests also require selected-source evidence in replay.
// Request schema v6 binds a versioned capture policy to an absent bootstrap
// target. Existing managed requests use v5 and the public existing-instance
// launcher uses v4. The coordinator control/status protocol remains v2. Older
// private request schemas are deliberately rejected by the post-cleanup
// release rather than translated into new authority.
const REQUEST_HEADER: &str = "cake-autorate-operation\t6\trequest";
const MANAGED_REQUEST_HEADER: &str = "cake-autorate-operation\t5\trequest";
const PUBLIC_EXISTING_REQUEST_HEADER: &str = "cake-autorate-operation\t4\trequest";
const EXPLICIT_MANAGED_REQUEST_HEADER: &str = "cake-autorate-operation\t7\trequest";
const EXPLICIT_BOOTSTRAP_REQUEST_HEADER: &str = "cake-autorate-operation\t8\trequest";
const PLANNED_MANAGED_REQUEST_HEADER: &str = "cake-autorate-operation\t9\trequest";
const PLANNED_BOOTSTRAP_REQUEST_HEADER: &str = "cake-autorate-operation\t10\trequest";
const MASKED_MANAGED_REQUEST_HEADER: &str = "cake-autorate-operation\t11\trequest";
const MASKED_BOOTSTRAP_REQUEST_HEADER: &str = "cake-autorate-operation\t12\trequest";
const MASKED_PLANNED_MANAGED_REQUEST_HEADER: &str = "cake-autorate-operation\t13\trequest";
const MASKED_PLANNED_BOOTSTRAP_REQUEST_HEADER: &str = "cake-autorate-operation\t14\trequest";
const LINKED_MANAGED_REQUEST_HEADER: &str = "cake-autorate-operation\t15\trequest";
const LINKED_BOOTSTRAP_REQUEST_HEADER: &str = "cake-autorate-operation\t16\trequest";
const LINKED_PLANNED_MANAGED_REQUEST_HEADER: &str = "cake-autorate-operation\t17\trequest";
const LINKED_PLANNED_BOOTSTRAP_REQUEST_HEADER: &str = "cake-autorate-operation\t18\trequest";
const DNS_MANAGED_REQUEST_HEADER: &str = "cake-autorate-operation\t19\trequest";
const DNS_BOOTSTRAP_REQUEST_HEADER: &str = "cake-autorate-operation\t20\trequest";
const DNS_PLANNED_MANAGED_REQUEST_HEADER: &str = "cake-autorate-operation\t21\trequest";
const DNS_PLANNED_BOOTSTRAP_REQUEST_HEADER: &str = "cake-autorate-operation\t22\trequest";
const CONTROL_HEADER: &str = "cake-autorate-operation\t2\tcontrol";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrafficPolicy {
    Capped { max_bytes: u64 },
    Unlimited,
}

impl From<u64> for TrafficPolicy {
    fn from(max_bytes: u64) -> Self {
        Self::Capped { max_bytes }
    }
}

impl TrafficPolicy {
    pub fn validate(self) -> Result<(), String> {
        if self
            .limit_bytes()
            .is_some_and(|limit| limit > MAX_TRAFFIC_BUDGET_BYTES)
        {
            return Err(format!(
                "traffic budget must not exceed {MAX_TRAFFIC_BUDGET_BYTES} bytes"
            ));
        }
        Ok(())
    }
    pub fn limit_bytes(self) -> Option<u64> {
        match self {
            Self::Capped { max_bytes } => Some(max_bytes),
            Self::Unlimited => None,
        }
    }
    pub fn is_empty(self) -> bool {
        matches!(self, Self::Capped { max_bytes: 0 })
    }
    pub fn allows(self, required_bytes: u64) -> bool {
        match self {
            Self::Capped { max_bytes } => max_bytes >= required_bytes,
            Self::Unlimited => true,
        }
    }
    pub fn exceeded(self, consumed_bytes: u64) -> bool {
        match self {
            Self::Capped { max_bytes } => consumed_bytes > max_bytes,
            Self::Unlimited => false,
        }
    }
    pub fn permits(self, narrowed: Self) -> bool {
        match narrowed.limit_bytes() {
            Some(limit) => self.allows(limit),
            None => self == Self::Unlimited,
        }
    }
    pub fn checked_sub(self, bytes: u64) -> Option<Self> {
        match self {
            Self::Capped { max_bytes } => max_bytes.checked_sub(bytes).map(Self::from),
            Self::Unlimited => Some(Self::Unlimited),
        }
    }
    pub fn json_limit(self) -> String {
        self.limit_bytes()
            .map_or_else(|| "null".into(), |value| value.to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationKind {
    FullAutotune,
    AutomaticRating,
    GuidedRating,
    Speedtest,
}

impl OperationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FullAutotune => "full_autotune",
            Self::AutomaticRating => "automatic_rating",
            Self::GuidedRating => "guided_rating",
            Self::Speedtest => "speedtest",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "full_autotune" => Some(Self::FullAutotune),
            "automatic_rating" => Some(Self::AutomaticRating),
            "guided_rating" => Some(Self::GuidedRating),
            "speedtest" => Some(Self::Speedtest),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationOrigin {
    Luci,
    Scheduler,
    Recovery,
    Internal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationTargetState {
    ExistingManaged,
    AbsentBootstrap,
}

impl OperationTargetState {
    fn as_str(self) -> &'static str {
        match self {
            Self::ExistingManaged => "existing_managed",
            Self::AbsentBootstrap => "absent_bootstrap",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "existing_managed" => Some(Self::ExistingManaged),
            "absent_bootstrap" => Some(Self::AbsentBootstrap),
            _ => None,
        }
    }
}

impl OperationOrigin {
    fn as_str(self) -> &'static str {
        match self {
            Self::Luci => "luci",
            Self::Scheduler => "scheduler",
            Self::Recovery => "recovery",
            Self::Internal => "internal",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "luci" => Some(Self::Luci),
            "scheduler" => Some(Self::Scheduler),
            "recovery" => Some(Self::Recovery),
            "internal" => Some(Self::Internal),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationRouteMode {
    Main,
    Mwan3,
    Explicit,
}

impl OperationRouteMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Mwan3 => "mwan3",
            Self::Explicit => "explicit",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "main" => Some(Self::Main),
            "mwan3" => Some(Self::Mwan3),
            "explicit" => Some(Self::Explicit),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalibrationStrategy {
    ShapedOnly,
    FullRaw,
    ReuseTrusted,
}

impl CalibrationStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ShapedOnly => "shaped_only",
            Self::FullRaw => "full_raw",
            Self::ReuseTrusted => "reuse_trusted",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "shaped_only" => Some(Self::ShapedOnly),
            "full_raw" => Some(Self::FullRaw),
            "reuse_trusted" => Some(Self::ReuseTrusted),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeedtestDirection {
    Download,
    Upload,
    Both,
}

impl SpeedtestDirection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Download => "download",
            Self::Upload => "upload",
            Self::Both => "both",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "download" => Some(Self::Download),
            "upload" => Some(Self::Upload),
            "both" => Some(Self::Both),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeedtestTopology {
    Current,
    Unshaped,
}

impl SpeedtestTopology {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Unshaped => "unshaped",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "current" => Some(Self::Current),
            "unshaped" => Some(Self::Unshaped),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationState {
    Queued,
    Starting,
    Running,
    Cancelling,
    Recovering,
    ReviewReady,
    Completed,
    Failed,
    Cancelled,
}

impl OperationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Cancelling => "cancelling",
            Self::Recovering => "recovering",
            Self::ReviewReady => "review_ready",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "starting" => Some(Self::Starting),
            "running" => Some(Self::Running),
            "cancelling" => Some(Self::Cancelling),
            "recovering" => Some(Self::Recovering),
            "review_ready" => Some(Self::ReviewReady),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlCommand {
    Ping,
    Summary,
    Start,
    Status,
    Result,
    Cancel,
}

impl ControlCommand {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ping => "ping",
            Self::Summary => "summary",
            Self::Start => "start",
            Self::Status => "status",
            Self::Result => "result",
            Self::Cancel => "cancel",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "ping" => Some(Self::Ping),
            "summary" => Some(Self::Summary),
            "start" => Some(Self::Start),
            "status" => Some(Self::Status),
            "result" => Some(Self::Result),
            "cancel" => Some(Self::Cancel),
            _ => None,
        }
    }

    fn requires_job(self) -> bool {
        matches!(
            self,
            Self::Start | Self::Status | Self::Result | Self::Cancel
        )
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ControlRequest {
    pub request_id: String,
    pub command: ControlCommand,
    pub job_id: Option<String>,
    pub job_token: Option<String>,
}

impl fmt::Debug for ControlRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControlRequest")
            .field("request_id", &self.request_id)
            .field("command", &self.command)
            .field("job_id", &self.job_id)
            .field("job_token", &self.job_token.as_ref().map(|_| "[redacted]"))
            .finish()
    }
}

impl ControlRequest {
    pub fn validate(&self) -> Result<(), String> {
        require_lower_hex("request_id", &self.request_id, 32)?;
        if self.command.requires_job() {
            require_lower_hex(
                "job_id",
                self.job_id
                    .as_deref()
                    .ok_or_else(|| "job command requires job_id".to_string())?,
                32,
            )?;
            require_lower_hex(
                "job_token",
                self.job_token
                    .as_deref()
                    .ok_or_else(|| "job command requires job_token".to_string())?,
                64,
            )?;
        } else if self.job_id.is_some() || self.job_token.is_some() {
            return Err("daemon-level command must not carry job identity".to_string());
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<String, String> {
        self.validate()?;
        encode_record(
            CONTROL_HEADER,
            &[
                ("request_id", self.request_id.clone()),
                ("command", self.command.as_str().to_string()),
                ("job_id", self.job_id.clone().unwrap_or_default()),
                ("job_token", self.job_token.clone().unwrap_or_default()),
            ],
        )
    }

    pub fn decode(input: &str) -> Result<Self, String> {
        let mut reader = RecordReader::new(input, CONTROL_HEADER)?;
        let request_id = reader.field("request_id")?;
        let command = ControlCommand::parse(&reader.field("command")?)
            .ok_or_else(|| "unsupported calibration control command".to_string())?;
        let job_id = optional_string(reader.field("job_id")?);
        let job_token = optional_string(reader.field("job_token")?);
        reader.finish()?;
        let request = Self {
            request_id,
            command,
            job_id,
            job_token,
        };
        request.validate()?;
        if request.encode()? != input {
            return Err("control request is not canonically encoded".to_string());
        }
        Ok(request)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControlMessage {
    pub control: ControlRequest,
    pub operation: Option<OperationRequest>,
}

impl ControlMessage {
    pub fn validate(&self) -> Result<(), String> {
        self.control.validate()?;
        match (self.control.command, self.operation.as_ref()) {
            (ControlCommand::Start, Some(operation)) => {
                operation.validate()?;
                if self.control.job_id.as_deref() != Some(&operation.identity.job_id)
                    || self.control.job_token.as_deref() != Some(&operation.identity.job_token)
                {
                    return Err(
                        "start control identity does not match its operation request".to_string(),
                    );
                }
            }
            (ControlCommand::Start, None) => {
                return Err("start control requires an operation request".to_string())
            }
            (_, Some(_)) => {
                return Err("only start control may carry an operation request".to_string())
            }
            (_, None) => {}
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<String, String> {
        self.validate()?;
        let mut encoded = self.control.encode()?;
        if let Some(operation) = &self.operation {
            encoded.push_str(&if operation.traffic_policy_explicit
                || operation.route.fwmark_mask.is_some()
            {
                operation.encode()?
            } else {
                match operation.target_state {
                    // Keep the established public Start wire byte-compatible for
                    // every existing-instance operation. The v5 lifecycle field
                    // is private journal authority until bootstrap is complete.
                    OperationTargetState::ExistingManaged => operation.encode_for_schema(4)?,
                    OperationTargetState::AbsentBootstrap => operation.encode()?,
                }
            });
        }
        if encoded.len() > MAX_CONTROL_MESSAGE_BYTES {
            return Err("calibration control message exceeds its bound".to_string());
        }
        Ok(encoded)
    }

    pub fn decode(input: &str) -> Result<Self, String> {
        if input.len() > MAX_CONTROL_MESSAGE_BYTES || !input.ends_with('\n') {
            return Err("calibration control message is not bounded and terminated".to_string());
        }
        // Canonical records end with one empty separator line. The control
        // record has its header plus four fields, hence six newline bytes.
        let control_end = nth_newline_end(input, 6)
            .ok_or_else(|| "calibration control message is truncated".to_string())?;
        let (control_record, operation_record) = input.split_at(control_end);
        let control = ControlRequest::decode(control_record)?;
        let operation = if operation_record.is_empty() {
            None
        } else {
            Some(OperationRequest::decode(operation_record)?)
        };
        let message = Self { control, operation };
        message.validate()?;
        if let Some(operation) = &message.operation {
            let header = operation_record.lines().next().unwrap_or_default();
            let wire_matches_target = if operation.route.fwmark_mask.is_some() {
                operation.encode()?.lines().next() == Some(header)
            } else {
                match (operation.traffic_policy_explicit, operation.target_state) {
                    (false, OperationTargetState::ExistingManaged) => {
                        header == PUBLIC_EXISTING_REQUEST_HEADER
                    }
                    (false, OperationTargetState::AbsentBootstrap) => header == REQUEST_HEADER,
                    (true, OperationTargetState::ExistingManaged) => {
                        header
                            == if operation.traffic_plan.is_some() {
                                PLANNED_MANAGED_REQUEST_HEADER
                            } else {
                                EXPLICIT_MANAGED_REQUEST_HEADER
                            }
                    }
                    (true, OperationTargetState::AbsentBootstrap) => {
                        header
                            == if operation.traffic_plan.is_some() {
                                PLANNED_BOOTSTRAP_REQUEST_HEADER
                            } else {
                                EXPLICIT_BOOTSTRAP_REQUEST_HEADER
                            }
                    }
                }
            };
            if !wire_matches_target {
                return Err(
                    "calibration control operation schema does not match its target lifecycle"
                        .to_string(),
                );
            }
        }
        Ok(message)
    }
}

fn nth_newline_end(input: &str, count: usize) -> Option<usize> {
    let mut seen = 0usize;
    for (index, byte) in input.bytes().enumerate() {
        if byte == b'\n' {
            seen += 1;
            if seen == count {
                return Some(index + 1);
            }
        }
    }
    None
}

#[derive(Clone, PartialEq, Eq)]
pub struct OperationIdentity {
    pub job_id: String,
    pub job_token: String,
    pub instance: String,
    pub operation: OperationKind,
    pub target_interface: String,
    pub route_fingerprint: String,
    pub config_fingerprint: String,
    pub sqm_fingerprint: String,
}

impl fmt::Debug for OperationIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OperationIdentity")
            .field("job_id", &self.job_id)
            .field("job_token", &"[redacted]")
            .field("instance", &self.instance)
            .field("operation", &self.operation)
            .field("target_interface", &self.target_interface)
            .field("route_fingerprint", &self.route_fingerprint)
            .field("config_fingerprint", &self.config_fingerprint)
            .field("sqm_fingerprint", &self.sqm_fingerprint)
            .finish()
    }
}

impl OperationIdentity {
    pub fn validate(&self) -> Result<(), String> {
        require_lower_hex("job_id", &self.job_id, 32)?;
        require_lower_hex("job_token", &self.job_token, 64)?;
        require_safe_identifier("instance", &self.instance, 1, 64, b"_-")?;
        require_safe_identifier("target_interface", &self.target_interface, 1, 64, b"._:@-")?;
        require_lower_hex("route_fingerprint", &self.route_fingerprint, 64)?;
        require_lower_hex("config_fingerprint", &self.config_fingerprint, 64)?;
        require_lower_hex("sqm_fingerprint", &self.sqm_fingerprint, 64)?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationRouteIdentity {
    pub dns_server: Option<std::net::Ipv4Addr>,
    pub device_ifindex: Option<u32>,
    pub mode: OperationRouteMode,
    pub mwan3_member: Option<String>,
    pub l3_device: String,
    pub source_ip: Option<IpAddr>,
    pub fwmark: Option<u32>,
    pub routing_table: Option<u32>,
    pub fwmark_mask: Option<u32>,
}

impl OperationRouteIdentity {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if let Some(server) = self.dns_server {
            crate::routing::explicit_dns_server(self.mode.as_str(), &server.to_string())?;
        }
        require_safe_identifier("l3_device", &self.l3_device, 1, 64, b"._:@-")?;
        if self
            .device_ifindex
            .is_some_and(|index| index == 0 || self.fwmark_mask.is_none())
        {
            return Err("route link witness requires a positive index and policy mask".into());
        }
        if let Some(mask) = self.fwmark_mask {
            if self.mode == OperationRouteMode::Main
                || mask == 0
                || self
                    .fwmark
                    .is_none_or(|mark| mark == 0 || mark & !mask != 0)
            {
                return Err("route mask requires a nonzero contained policy mark".into());
            }
        }
        match (self.mode, self.mwan3_member.as_deref()) {
            (OperationRouteMode::Explicit, None) => {
                let source_valid = matches!(self.source_ip, Some(IpAddr::V4(source))
                    if source.octets()[0] != 0 && !source.is_loopback() && source.octets()[0] < 224);
                if !source_valid
                    || self.device_ifindex.is_none()
                    || self.fwmark_mask.is_none()
                    || !self.routing_table.is_some_and(|table| table != 0)
                {
                    return Err(
                        "explicit route requires source, link, table and masked mark authority"
                            .into(),
                    );
                }
            }
            (OperationRouteMode::Explicit, Some(_)) => {
                return Err("explicit route must not carry an mwan3 member".into());
            }
            (OperationRouteMode::Main, None) => {}
            (OperationRouteMode::Mwan3, Some(member)) => {
                require_safe_identifier("mwan3_member", member, 1, 64, b"_-")?
            }
            (OperationRouteMode::Main, Some(_)) => {
                return Err("main route must not carry an mwan3 member".to_string())
            }
            (OperationRouteMode::Mwan3, None) => {
                return Err("mwan3 route requires a member".to_string())
            }
        }
        Ok(())
    }
}

/// User planning assumptions only; never shaping or traffic-stop authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutotuneTrafficPlan {
    pub download_kbps: u64,
    pub upload_kbps: u64,
}

impl AutotuneTrafficPlan {
    pub fn validate(self) -> Result<(), String> {
        if !(1..=100_000_000).contains(&self.download_kbps)
            || !(1..=100_000_000).contains(&self.upload_kbps)
        {
            return Err("Auto-Tune planning rates must be in 1..=100000000 kbps".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationRequest {
    pub identity: OperationIdentity,
    pub created_unix_ms: u64,
    pub deadline_unix_ms: u64,
    pub origin: OperationOrigin,
    pub backend: String,
    pub speedtest_direction: Option<SpeedtestDirection>,
    pub speedtest_server_id: Option<u64>,
    pub speedtest_topology: Option<SpeedtestTopology>,
    pub route: OperationRouteIdentity,
    pub target_state: OperationTargetState,
    pub capture_policy: Option<AutotuneCapturePolicyId>,
    pub managed_sqm_section: Option<String>,
    pub profile: Option<AutotuneProfile>,
    pub strategy: Option<CalibrationStrategy>,
    pub access_medium: Option<AccessMedium>,
    pub access_source: Option<AccessEvidenceSource>,
    pub access_confidence_percent: u8,
    pub capacity_learning_policy: Option<CapacityLearningPolicy>,
    pub service_dl_cap_kbps: Option<u64>,
    pub service_ul_cap_kbps: Option<u64>,
    pub allow_sqm_disable: bool,
    pub allow_active_traffic: bool,
    pub scheduled_auto_apply_requested: bool,
    pub traffic_budget: TrafficPolicy,
    /// Preserve legacy request bytes on decode; new launchers opt into v7/v8.
    pub traffic_policy_explicit: bool,
    pub traffic_plan: Option<AutotuneTrafficPlan>,
}

impl OperationRequest {
    pub fn validate(&self) -> Result<(), String> {
        self.identity.validate()?;
        self.route.validate()?;
        require_safe_identifier("backend", &self.backend, 1, 32, b"_-")?;
        if self.created_unix_ms == 0 {
            return Err("created_unix_ms must be non-zero".to_string());
        }
        if self.deadline_unix_ms < self.created_unix_ms {
            return Err("deadline_unix_ms must not precede creation".to_string());
        }
        self.traffic_budget.validate()?;
        if let Some(plan) = self.traffic_plan {
            plan.validate()?;
            if !self.traffic_policy_explicit
                || self.identity.operation != OperationKind::FullAutotune
                || self.origin == OperationOrigin::Scheduler
            {
                return Err(
                    "planning rates require an explicit non-scheduled Full Auto-Tune request"
                        .into(),
                );
            }
        }
        if self.traffic_budget == TrafficPolicy::Unlimited
            && (!self.traffic_policy_explicit || self.origin == OperationOrigin::Scheduler)
        {
            return Err("unlimited traffic requires explicit non-scheduled request policy".into());
        }
        if self.scheduled_auto_apply_requested
            && (self.origin != OperationOrigin::Scheduler
                || self.identity.operation != OperationKind::FullAutotune)
        {
            return Err(
                "scheduled Auto-Apply authority requires a scheduler Full Auto-Tune request"
                    .to_string(),
            );
        }
        if self.target_state == OperationTargetState::AbsentBootstrap {
            if !matches!(
                self.identity.operation,
                OperationKind::FullAutotune | OperationKind::Speedtest
            ) {
                return Err(
                    "absent bootstrap target state is valid only for Full Auto-Tune or Speedtest"
                        .to_string(),
                );
            }
            if self.origin != OperationOrigin::Luci {
                return Err(
                    "absent bootstrap target state requires a LuCI operation origin".to_string(),
                );
            }
            if self.scheduled_auto_apply_requested {
                return Err(
                    "absent bootstrap target state cannot request scheduled Auto-Apply".to_string(),
                );
            }
            let planned_sqm_section = self.managed_sqm_section.as_deref().ok_or_else(|| {
                "absent bootstrap target state requires a planned managed SQM section".to_string()
            })?;
            super::sqm_identity::validate_uci_section(planned_sqm_section)?;
            self.capture_policy
                .ok_or_else(|| {
                    "absent bootstrap target state requires an explicit capture policy".to_string()
                })?
                .expand()?;
        } else if self.capture_policy.is_some() {
            return Err(
                "only an absent bootstrap target may carry capture policy authority".to_string(),
            );
        }
        match self.identity.operation {
            OperationKind::FullAutotune => {
                if self.speedtest_direction.is_some() || self.speedtest_topology.is_some() {
                    return Err("Full Auto-Tune selects its test directions internally".to_string());
                }
                if self.speedtest_server_id == Some(0) {
                    return Err("Full Auto-Tune speed-test server ID must be positive".to_string());
                }
                if self.profile.is_none() || self.strategy.is_none() {
                    return Err("Full Auto-Tune requires a profile and strategy".to_string());
                }
                if self.traffic_budget.is_empty() {
                    return Err("Full Auto-Tune requires a non-zero traffic budget".to_string());
                }
                if self.access_medium.is_none()
                    || self.access_source.is_none()
                    || self.capacity_learning_policy.is_none()
                {
                    return Err("Full Auto-Tune requires an explicit access context".to_string());
                }
                if self.access_confidence_percent > 100 {
                    return Err("access confidence must be between 0 and 100".to_string());
                }
                require_safe_identifier(
                    "managed_sqm_section",
                    self.managed_sqm_section.as_deref().ok_or_else(|| {
                        "Full Auto-Tune requires the attested managed SQM section".to_string()
                    })?,
                    1,
                    64,
                    b"_-",
                )?;
            }
            OperationKind::AutomaticRating | OperationKind::Speedtest => {
                if self.profile.is_some() || self.strategy.is_some() {
                    return Err(
                        "only Full Auto-Tune may carry a profile or calibration strategy"
                            .to_string(),
                    );
                }
                if self.allow_sqm_disable {
                    return Err("only Full Auto-Tune may allow SQM disable".to_string());
                }
                if self.traffic_budget.is_empty() {
                    return Err(
                        "generated-traffic operation requires a non-zero budget".to_string()
                    );
                }
                self.reject_speedtest_stop_authority_or_autotune_context()?;
                if self.identity.operation == OperationKind::AutomaticRating
                    && self.managed_sqm_section.is_some()
                {
                    return Err("Rating must not carry a managed SQM section".to_string());
                }
                if self.identity.operation == OperationKind::Speedtest
                    && self.target_state == OperationTargetState::ExistingManaged
                    && self.managed_sqm_section.is_some()
                {
                    return Err(
                        "existing-instance Speedtest must not carry a planned SQM section"
                            .to_string(),
                    );
                }
                if !matches!(self.route.source_ip, Some(IpAddr::V4(_))) {
                    return Err(
                        "generated-traffic operation requires an explicit IPv4 source".to_string(),
                    );
                }
                if self.identity.operation == OperationKind::Speedtest {
                    if self.speedtest_direction.is_none() {
                        return Err("Speedtest requires an explicit direction".to_string());
                    }
                    if self.speedtest_topology.is_none() {
                        return Err("Speedtest requires an explicit topology".to_string());
                    }
                    if self.speedtest_server_id == Some(0) {
                        return Err("Speedtest server ID must be positive".to_string());
                    }
                } else if self.speedtest_direction.is_some()
                    || self.speedtest_server_id.is_some()
                    || self.speedtest_topology.is_some()
                {
                    return Err(
                        "only Speedtest may carry speed-test direction or server ID".to_string()
                    );
                }
            }
            OperationKind::GuidedRating => {
                if self.profile.is_some() || self.strategy.is_some() {
                    return Err(
                        "only Full Auto-Tune may carry a profile or calibration strategy"
                            .to_string(),
                    );
                }
                if self.allow_sqm_disable {
                    return Err("only Full Auto-Tune may allow SQM disable".to_string());
                }
                self.reject_autotune_context()?;
                if self.managed_sqm_section.is_some() {
                    return Err("Rating must not carry a managed SQM section".to_string());
                }
                if self.speedtest_direction.is_some()
                    || self.speedtest_server_id.is_some()
                    || self.speedtest_topology.is_some()
                {
                    return Err(
                        "only Speedtest may carry speed-test direction or server ID".to_string()
                    );
                }
            }
        }
        for (name, value) in [
            ("download service cap", self.service_dl_cap_kbps),
            ("upload service cap", self.service_ul_cap_kbps),
        ] {
            if value.is_some_and(|value| !(100..=crate::autotune::MAX_RATE_KBPS).contains(&value)) {
                return Err(format!(
                    "{name} must be between 100 and {} kbit/s",
                    crate::autotune::MAX_RATE_KBPS
                ));
            }
        }
        Ok(())
    }

    /// Validate policy that controls whether a structurally valid request may
    /// be admitted for new or resumed work.  This is deliberately separate
    /// from the canonical record decoder: an in-place upgrade must still be
    /// able to audit an inert, terminal request created by an older build.
    pub fn validate_admission_policy(&self) -> Result<(), String> {
        if self.route.device_ifindex.is_some() {
            // Only a complete explicit policy route is link-qualified work; the
            // worker marks, attests and accounts it through that authority.
            if !cfg!(feature = "calibration") || self.route.mode != OperationRouteMode::Explicit {
                return Err("link-qualified route execution is not available in this build".into());
            }
            super::autotune_request::explicit_operation_authority(&self.route)?;
            if self.route.dns_server.is_none() {
                return Err("explicit route operation requires an explicit IPv4 DNS server".into());
            }
        }
        self.validate()?;
        crate::autotune::validate_capacity_learning_service_caps(
            self.capacity_learning_policy,
            self.service_dl_cap_kbps,
            self.service_ul_cap_kbps,
        )?;
        if self.target_state != OperationTargetState::AbsentBootstrap {
            return Ok(());
        }
        if self.backend != "speedtest-go" {
            return Err(
                "absent bootstrap operation requires the native speedtest-go backend".to_string(),
            );
        }
        if !matches!(self.route.source_ip, Some(IpAddr::V4(_))) {
            return Err(
                "absent bootstrap operation requires an explicit IPv4 route source".to_string(),
            );
        }
        match self.identity.operation {
            OperationKind::FullAutotune => {
                if self.strategy != Some(CalibrationStrategy::FullRaw) || !self.allow_sqm_disable {
                    return Err(
                        "absent bootstrap Full Auto-Tune requires Full Raw controls and directional SQM bypass authority"
                            .to_string(),
                    );
                }
                if self.service_dl_cap_kbps.is_none() || self.service_ul_cap_kbps.is_none() {
                    return Err(
                        "absent bootstrap Full Auto-Tune requires explicit download and upload service caps as search authority"
                            .to_string(),
                    );
                }
            }
            OperationKind::Speedtest => {
                if self.speedtest_topology != Some(SpeedtestTopology::Unshaped)
                    || self.allow_sqm_disable
                    || self.allow_active_traffic
                {
                    return Err(
                        "absent bootstrap Speedtest requires a read-only unshaped policy"
                            .to_string(),
                    );
                }
            }
            _ => {
                return Err(
                    "absent bootstrap operation kind is unsupported for admission".to_string(),
                )
            }
        }
        Ok(())
    }

    /// A standalone Speedtest with an explicit capped policy carries per-direction
    /// stopping rate authority in the service ceiling fields.  They never become
    /// capacity-learning context, and every other operation still rejects them.
    fn reject_speedtest_stop_authority_or_autotune_context(&self) -> Result<(), String> {
        let stop_authority =
            self.service_dl_cap_kbps.is_some() || self.service_ul_cap_kbps.is_some();
        if !stop_authority {
            return self.reject_autotune_context();
        }
        if self.identity.operation != OperationKind::Speedtest
            || !self.traffic_policy_explicit
            || self.traffic_budget == TrafficPolicy::Unlimited
        {
            return Err(
                "only an explicit capped Speedtest may carry stopping rate authority".to_string(),
            );
        }
        if self.access_medium.is_some()
            || self.access_source.is_some()
            || self.access_confidence_percent != 0
            || self.capacity_learning_policy.is_some()
        {
            return Err("only Full Auto-Tune may carry access context".to_string());
        }
        Ok(())
    }

    fn reject_autotune_context(&self) -> Result<(), String> {
        if self.access_medium.is_some()
            || self.access_source.is_some()
            || self.access_confidence_percent != 0
            || self.capacity_learning_policy.is_some()
            || self.service_dl_cap_kbps.is_some()
            || self.service_ul_cap_kbps.is_some()
        {
            return Err("only Full Auto-Tune may carry access context".to_string());
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<String, String> {
        if self.route.fwmark_mask.is_some() {
            let schema = match (self.target_state, self.traffic_plan.is_some()) {
                (OperationTargetState::ExistingManaged, false) => 11,
                (OperationTargetState::AbsentBootstrap, false) => 12,
                (OperationTargetState::ExistingManaged, true) => 13,
                (OperationTargetState::AbsentBootstrap, true) => 14,
            };
            return self.encode_for_schema(if self.route.dns_server.is_some() {
                schema + 8
            } else if self.route.device_ifindex.is_some() {
                schema + 4
            } else {
                schema
            });
        }
        if self.traffic_plan.is_some() {
            return self.encode_for_schema(
                if self.target_state == OperationTargetState::AbsentBootstrap {
                    10
                } else {
                    9
                },
            );
        }
        if self.target_state == OperationTargetState::AbsentBootstrap {
            self.encode_for_schema(if self.traffic_policy_explicit { 8 } else { 6 })
        } else {
            self.encode_for_schema(if self.traffic_policy_explicit { 7 } else { 5 })
        }
    }

    fn encode_for_schema(&self, schema: u8) -> Result<String, String> {
        if !(4..=22).contains(&schema) {
            return Err("unsupported operation request schema".to_string());
        }
        let base_schema = if schema >= 19 {
            schema - 12
        } else if schema >= 15 {
            schema - 8
        } else if schema >= 11 {
            schema - 4
        } else {
            schema
        };
        if (schema >= 15) != self.route.device_ifindex.is_some() {
            return Err("request route link encoding downgrade or mismatch".into());
        }
        if (schema >= 19) != self.route.dns_server.is_some() {
            return Err("request route DNS encoding downgrade or mismatch".into());
        }
        if (schema >= 11) != self.route.fwmark_mask.is_some() {
            return Err("request route mask encoding downgrade or mismatch".into());
        }
        self.validate()?;
        if (base_schema >= 9) != self.traffic_plan.is_some() {
            return Err("request planning encoding downgrade or mismatch".into());
        }
        if schema < 11 && (schema >= 7) != self.traffic_policy_explicit {
            return Err("request traffic policy encoding downgrade or mismatch".into());
        }
        let bootstrap_schema = matches!(base_schema, 6 | 8 | 10);
        if !bootstrap_schema && self.target_state != OperationTargetState::ExistingManaged {
            return Err(
                "managed operation request schema cannot represent absent bootstrap authority"
                    .to_string(),
            );
        }
        if !bootstrap_schema && self.capture_policy.is_some() {
            return Err(
                "managed operation request schema cannot represent capture policy authority"
                    .to_string(),
            );
        }
        if bootstrap_schema && self.target_state != OperationTargetState::AbsentBootstrap {
            return Err(
                "operation request schema v6 is reserved for absent bootstrap authority"
                    .to_string(),
            );
        }
        let mut fields = vec![
            ("job_id", self.identity.job_id.clone()),
            ("job_token", self.identity.job_token.clone()),
            ("created_unix_ms", self.created_unix_ms.to_string()),
            ("deadline_unix_ms", self.deadline_unix_ms.to_string()),
            ("instance", self.identity.instance.clone()),
            ("operation", self.identity.operation.as_str().to_string()),
            ("origin", self.origin.as_str().to_string()),
            ("backend", self.backend.clone()),
            (
                "speedtest_direction",
                self.speedtest_direction
                    .map(|value| value.as_str().to_string())
                    .unwrap_or_default(),
            ),
            (
                "speedtest_server_id",
                self.speedtest_server_id
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
        ];
        fields.push((
            "speedtest_topology",
            self.speedtest_topology
                .map(|value| value.as_str().to_string())
                .unwrap_or_default(),
        ));
        fields.push(("target_interface", self.identity.target_interface.clone()));
        if schema >= 5 {
            fields.push(("target_state", self.target_state.as_str().to_string()));
        }
        if bootstrap_schema {
            let capture_policy = self.capture_policy.ok_or_else(|| {
                "operation request schema v6 requires capture policy authority".to_string()
            })?;
            fields.push(("capture_policy", capture_policy.as_str().to_string()));
            fields.push(("capture_policy_sha256", capture_policy.canonical_sha256()?));
        }
        fields.extend([
            (
                "managed_sqm_section",
                self.managed_sqm_section.clone().unwrap_or_default(),
            ),
            ("route_mode", self.route.mode.as_str().to_string()),
            (
                "mwan3_member",
                self.route.mwan3_member.clone().unwrap_or_default(),
            ),
            ("l3_device", self.route.l3_device.clone()),
            (
                "source_ip",
                self.route
                    .source_ip
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            (
                "fwmark",
                self.route
                    .fwmark
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            (
                "routing_table",
                self.route
                    .routing_table
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            ("route_fingerprint", self.identity.route_fingerprint.clone()),
            (
                "config_fingerprint",
                self.identity.config_fingerprint.clone(),
            ),
            ("sqm_fingerprint", self.identity.sqm_fingerprint.clone()),
            (
                "profile",
                self.profile
                    .map(|value| value.as_str().to_string())
                    .unwrap_or_default(),
            ),
            (
                "strategy",
                self.strategy
                    .map(|value| value.as_str().to_string())
                    .unwrap_or_default(),
            ),
            (
                "access_medium",
                self.access_medium
                    .map(|value| value.as_str().to_string())
                    .unwrap_or_default(),
            ),
            (
                "access_source",
                self.access_source
                    .map(|value| value.as_str().to_string())
                    .unwrap_or_default(),
            ),
            (
                "access_confidence_percent",
                self.access_confidence_percent.to_string(),
            ),
            (
                "capacity_learning_policy",
                self.capacity_learning_policy
                    .map(|value| value.as_str().to_string())
                    .unwrap_or_default(),
            ),
            (
                "service_dl_cap_kbps",
                self.service_dl_cap_kbps
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            (
                "service_ul_cap_kbps",
                self.service_ul_cap_kbps
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ),
            (
                "allow_sqm_disable",
                bool_text(self.allow_sqm_disable).to_string(),
            ),
            (
                "allow_active_traffic",
                bool_text(self.allow_active_traffic).to_string(),
            ),
        ]);
        fields.push((
            "scheduled_auto_apply_requested",
            bool_text(self.scheduled_auto_apply_requested).to_string(),
        ));
        if schema >= 7 {
            fields.push((
                "traffic_policy",
                match self.traffic_budget {
                    TrafficPolicy::Capped { .. } => "capped",
                    TrafficPolicy::Unlimited => "unlimited",
                }
                .to_string(),
            ));
        }
        fields.push((
            "traffic_budget_bytes",
            self.traffic_budget
                .limit_bytes()
                .map(|value| value.to_string())
                .unwrap_or_default(),
        ));
        if let Some(plan) = self.traffic_plan {
            fields.push(("planning_dl_kbps", plan.download_kbps.to_string()));
            fields.push(("planning_ul_kbps", plan.upload_kbps.to_string()));
        }
        if let Some(mask) = self.route.fwmark_mask {
            fields.push(("fwmark_mask", mask.to_string()));
            fields.push((
                "traffic_policy_explicit",
                bool_text(self.traffic_policy_explicit).to_string(),
            ));
        }
        if let Some(index) = self.route.device_ifindex {
            fields.push(("device_ifindex", index.to_string()));
        }
        if let Some(server) = self.route.dns_server {
            fields.push(("route_dns_ipv4", server.to_string()));
        }
        let header = match schema {
            4 => PUBLIC_EXISTING_REQUEST_HEADER,
            5 => MANAGED_REQUEST_HEADER,
            6 => REQUEST_HEADER,
            7 => EXPLICIT_MANAGED_REQUEST_HEADER,
            8 => EXPLICIT_BOOTSTRAP_REQUEST_HEADER,
            9 => PLANNED_MANAGED_REQUEST_HEADER,
            10 => PLANNED_BOOTSTRAP_REQUEST_HEADER,
            11 => MASKED_MANAGED_REQUEST_HEADER,
            12 => MASKED_BOOTSTRAP_REQUEST_HEADER,
            13 => MASKED_PLANNED_MANAGED_REQUEST_HEADER,
            14 => MASKED_PLANNED_BOOTSTRAP_REQUEST_HEADER,
            15 => LINKED_MANAGED_REQUEST_HEADER,
            16 => LINKED_BOOTSTRAP_REQUEST_HEADER,
            17 => LINKED_PLANNED_MANAGED_REQUEST_HEADER,
            18 => LINKED_PLANNED_BOOTSTRAP_REQUEST_HEADER,
            19 => DNS_MANAGED_REQUEST_HEADER,
            20 => DNS_BOOTSTRAP_REQUEST_HEADER,
            21 => DNS_PLANNED_MANAGED_REQUEST_HEADER,
            22 => DNS_PLANNED_BOOTSTRAP_REQUEST_HEADER,
            _ => unreachable!(),
        };
        encode_record(header, &fields)
    }

    #[cfg(test)]
    pub(crate) fn encode_for_test_schema(&self, schema: u8) -> Result<String, String> {
        self.encode_for_schema(schema)
    }

    pub fn decode(input: &str) -> Result<Self, String> {
        let schema = match input.lines().next() {
            Some(REQUEST_HEADER) => 6,
            Some(MANAGED_REQUEST_HEADER) => 5,
            Some(PUBLIC_EXISTING_REQUEST_HEADER) => 4,
            Some(EXPLICIT_MANAGED_REQUEST_HEADER) => 7,
            Some(EXPLICIT_BOOTSTRAP_REQUEST_HEADER) => 8,
            Some(PLANNED_MANAGED_REQUEST_HEADER) => 9,
            Some(PLANNED_BOOTSTRAP_REQUEST_HEADER) => 10,
            Some(MASKED_MANAGED_REQUEST_HEADER) => 11,
            Some(MASKED_BOOTSTRAP_REQUEST_HEADER) => 12,
            Some(MASKED_PLANNED_MANAGED_REQUEST_HEADER) => 13,
            Some(MASKED_PLANNED_BOOTSTRAP_REQUEST_HEADER) => 14,
            Some(LINKED_MANAGED_REQUEST_HEADER) => 15,
            Some(LINKED_BOOTSTRAP_REQUEST_HEADER) => 16,
            Some(LINKED_PLANNED_MANAGED_REQUEST_HEADER) => 17,
            Some(LINKED_PLANNED_BOOTSTRAP_REQUEST_HEADER) => 18,
            Some(DNS_MANAGED_REQUEST_HEADER) => 19,
            Some(DNS_BOOTSTRAP_REQUEST_HEADER) => 20,
            Some(DNS_PLANNED_MANAGED_REQUEST_HEADER) => 21,
            Some(DNS_PLANNED_BOOTSTRAP_REQUEST_HEADER) => 22,
            _ => return Err("operation record header mismatch".to_string()),
        };
        let header = match schema {
            4 => PUBLIC_EXISTING_REQUEST_HEADER,
            5 => MANAGED_REQUEST_HEADER,
            6 => REQUEST_HEADER,
            7 => EXPLICIT_MANAGED_REQUEST_HEADER,
            8 => EXPLICIT_BOOTSTRAP_REQUEST_HEADER,
            9 => PLANNED_MANAGED_REQUEST_HEADER,
            10 => PLANNED_BOOTSTRAP_REQUEST_HEADER,
            11 => MASKED_MANAGED_REQUEST_HEADER,
            12 => MASKED_BOOTSTRAP_REQUEST_HEADER,
            13 => MASKED_PLANNED_MANAGED_REQUEST_HEADER,
            14 => MASKED_PLANNED_BOOTSTRAP_REQUEST_HEADER,
            15 => LINKED_MANAGED_REQUEST_HEADER,
            16 => LINKED_BOOTSTRAP_REQUEST_HEADER,
            17 => LINKED_PLANNED_MANAGED_REQUEST_HEADER,
            18 => LINKED_PLANNED_BOOTSTRAP_REQUEST_HEADER,
            19 => DNS_MANAGED_REQUEST_HEADER,
            20 => DNS_BOOTSTRAP_REQUEST_HEADER,
            21 => DNS_PLANNED_MANAGED_REQUEST_HEADER,
            22 => DNS_PLANNED_BOOTSTRAP_REQUEST_HEADER,
            _ => unreachable!(),
        };
        let base_schema = if schema >= 19 {
            schema - 12
        } else if schema >= 15 {
            schema - 8
        } else if schema >= 11 {
            schema - 4
        } else {
            schema
        };
        let mut reader = RecordReader::new(input, header)?;
        let job_id = reader.field("job_id")?;
        let job_token = reader.field("job_token")?;
        let created_unix_ms = parse_u64("created_unix_ms", &reader.field("created_unix_ms")?)?;
        let deadline_unix_ms = parse_u64("deadline_unix_ms", &reader.field("deadline_unix_ms")?)?;
        let instance = reader.field("instance")?;
        let operation = OperationKind::parse(&reader.field("operation")?)
            .ok_or_else(|| "unsupported operation".to_string())?;
        let origin = OperationOrigin::parse(&reader.field("origin")?)
            .ok_or_else(|| "unsupported operation origin".to_string())?;
        let backend = reader.field("backend")?;
        let speedtest_direction = parse_optional_enum(
            reader.field("speedtest_direction")?,
            SpeedtestDirection::parse,
            "speed-test direction",
        )?;
        let speedtest_server_id =
            optional_parsed("speedtest_server_id", reader.field("speedtest_server_id")?)?;
        let speedtest_topology = parse_optional_enum(
            reader.field("speedtest_topology")?,
            SpeedtestTopology::parse,
            "speed-test topology",
        )?;
        let target_interface = reader.field("target_interface")?;
        let target_state = if schema >= 5 {
            OperationTargetState::parse(&reader.field("target_state")?)
                .ok_or_else(|| "unsupported operation target state".to_string())?
        } else {
            OperationTargetState::ExistingManaged
        };
        let capture_policy = if matches!(base_schema, 6 | 8 | 10) {
            let policy = AutotuneCapturePolicyId::parse(&reader.field("capture_policy")?)
                .ok_or_else(|| "unsupported Auto-Tune capture policy".to_string())?;
            let digest = reader.field("capture_policy_sha256")?;
            if digest != policy.canonical_sha256()? {
                return Err("Auto-Tune capture policy digest mismatch".to_string());
            }
            Some(policy)
        } else {
            None
        };
        let managed_sqm_section = optional_string(reader.field("managed_sqm_section")?);
        let mode = OperationRouteMode::parse(&reader.field("route_mode")?)
            .ok_or_else(|| "unsupported route mode".to_string())?;
        let mwan3_member = optional_string(reader.field("mwan3_member")?);
        let l3_device = reader.field("l3_device")?;
        let source_ip = optional_parsed("source_ip", reader.field("source_ip")?)?;
        let fwmark = optional_parsed("fwmark", reader.field("fwmark")?)?;
        let routing_table = optional_parsed("routing_table", reader.field("routing_table")?)?;
        let route_fingerprint = reader.field("route_fingerprint")?;
        let config_fingerprint = reader.field("config_fingerprint")?;
        let sqm_fingerprint = reader.field("sqm_fingerprint")?;
        let profile_text = reader.field("profile")?;
        let profile = if profile_text.is_empty() {
            None
        } else {
            Some(
                AutotuneProfile::parse(&profile_text)
                    .ok_or_else(|| "unsupported Auto-Tune profile".to_string())?,
            )
        };
        let strategy_text = reader.field("strategy")?;
        let strategy = if strategy_text.is_empty() {
            None
        } else {
            Some(
                CalibrationStrategy::parse(&strategy_text)
                    .ok_or_else(|| "unsupported calibration strategy".to_string())?,
            )
        };
        let access_medium = parse_optional_enum(
            reader.field("access_medium")?,
            AccessMedium::parse,
            "access medium",
        )?;
        let access_source = parse_optional_enum(
            reader.field("access_source")?,
            AccessEvidenceSource::parse,
            "access source",
        )?;
        let access_confidence_percent = reader
            .field("access_confidence_percent")?
            .parse::<u8>()
            .map_err(|_| "invalid access confidence".to_string())?;
        let capacity_learning_policy = parse_optional_enum(
            reader.field("capacity_learning_policy")?,
            CapacityLearningPolicy::parse,
            "capacity learning policy",
        )?;
        let service_dl_cap_kbps =
            optional_parsed("service_dl_cap_kbps", reader.field("service_dl_cap_kbps")?)?;
        let service_ul_cap_kbps =
            optional_parsed("service_ul_cap_kbps", reader.field("service_ul_cap_kbps")?)?;
        let allow_sqm_disable = parse_bool(&reader.field("allow_sqm_disable")?)?;
        let allow_active_traffic = parse_bool(&reader.field("allow_active_traffic")?)?;
        let scheduled_auto_apply_requested =
            parse_bool(&reader.field("scheduled_auto_apply_requested")?)?;
        let traffic_policy = if schema >= 7 {
            reader.field("traffic_policy")?
        } else {
            "capped".into()
        };
        let budget_value = reader.field("traffic_budget_bytes")?;
        let traffic_budget = match traffic_policy.as_str() {
            "capped" => TrafficPolicy::from(parse_u64("traffic_budget_bytes", &budget_value)?),
            "unlimited" if budget_value.is_empty() => TrafficPolicy::Unlimited,
            _ => return Err("invalid explicit traffic policy or conflicting byte limit".into()),
        };
        let traffic_plan = if base_schema >= 9 {
            Some(AutotuneTrafficPlan {
                download_kbps: parse_u64("planning_dl_kbps", &reader.field("planning_dl_kbps")?)?,
                upload_kbps: parse_u64("planning_ul_kbps", &reader.field("planning_ul_kbps")?)?,
            })
        } else {
            None
        };
        let fwmark_mask = if schema >= 11 {
            Some(
                reader
                    .field("fwmark_mask")?
                    .parse::<u32>()
                    .map_err(|_| "invalid route mark mask")?,
            )
        } else {
            None
        };
        let traffic_policy_explicit = if schema >= 11 {
            parse_bool(&reader.field("traffic_policy_explicit")?)?
        } else {
            schema >= 7
        };
        let device_ifindex = if schema >= 15 {
            Some(
                reader
                    .field("device_ifindex")?
                    .parse::<u32>()
                    .map_err(|_| "invalid route link index")?,
            )
        } else {
            None
        };
        let dns_server = if schema >= 19 {
            Some(
                reader
                    .field("route_dns_ipv4")?
                    .parse::<std::net::Ipv4Addr>()
                    .map_err(|_| "invalid route DNS server")?,
            )
        } else {
            None
        };
        reader.finish()?;
        let request = Self {
            identity: OperationIdentity {
                job_id,
                job_token,
                instance,
                operation,
                target_interface,
                route_fingerprint,
                config_fingerprint,
                sqm_fingerprint,
            },
            created_unix_ms,
            deadline_unix_ms,
            origin,
            backend,
            speedtest_direction,
            speedtest_server_id,
            speedtest_topology,
            route: OperationRouteIdentity {
                dns_server,
                device_ifindex,
                mode,
                mwan3_member,
                l3_device,
                source_ip,
                fwmark,
                routing_table,
                fwmark_mask,
            },
            target_state,
            capture_policy,
            managed_sqm_section,
            profile,
            strategy,
            access_medium,
            access_source,
            access_confidence_percent,
            capacity_learning_policy,
            service_dl_cap_kbps,
            service_ul_cap_kbps,
            allow_sqm_disable,
            allow_active_traffic,
            scheduled_auto_apply_requested,
            traffic_budget,
            traffic_policy_explicit,
            traffic_plan,
        };
        request.validate()?;
        if request.encode_for_schema(schema)? != input {
            return Err("operation request is not canonically encoded".to_string());
        }
        Ok(request)
    }
}

fn encode_record(header: &str, fields: &[(&str, String)]) -> Result<String, String> {
    let mut out = String::with_capacity(512);
    out.push_str(header);
    out.push('\n');
    for (key, value) in fields {
        out.push_str(key);
        out.push('=');
        out.push_str(&percent_encode(value));
        out.push('\n');
    }
    out.push('\n');
    if out.len() > MAX_OPERATION_RECORD_BYTES {
        return Err(format!(
            "operation record exceeds {MAX_OPERATION_RECORD_BYTES} bytes"
        ));
    }
    Ok(out)
}

struct RecordReader<'a> {
    lines: std::str::Lines<'a>,
}

impl<'a> RecordReader<'a> {
    fn new(input: &'a str, header: &str) -> Result<Self, String> {
        if input.len() > MAX_OPERATION_RECORD_BYTES {
            return Err(format!(
                "operation record exceeds {MAX_OPERATION_RECORD_BYTES} bytes"
            ));
        }
        if input.contains('\r') || !input.ends_with("\n\n") {
            return Err("operation record must use LF and end with one empty line".to_string());
        }
        let mut lines = input.lines();
        if lines.next() != Some(header) {
            return Err("unsupported operation record header or version".to_string());
        }
        Ok(Self { lines })
    }

    fn field(&mut self, expected: &str) -> Result<String, String> {
        let line = self
            .lines
            .next()
            .ok_or_else(|| format!("missing operation field {expected}"))?;
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("malformed operation field {expected}"))?;
        if key != expected {
            return Err(format!(
                "expected operation field {expected}, received {key}"
            ));
        }
        percent_decode(value)
    }

    fn finish(&mut self) -> Result<(), String> {
        if self.lines.next() != Some("") || self.lines.next().is_some() {
            return Err("operation record has trailing or unknown fields".to_string());
        }
        Ok(())
    }
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
    }
    out
}

fn percent_decode(value: &str) -> Result<String, String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                if index + 2 >= bytes.len() {
                    return Err("truncated percent escape in operation field".to_string());
                }
                let high = hex_value(bytes[index + 1])?;
                let low = hex_value(bytes[index + 2])?;
                out.push((high << 4) | low);
                index += 3;
            }
            byte if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') => {
                out.push(byte);
                index += 1;
            }
            _ => return Err("operation field contains a non-canonical raw byte".to_string()),
        }
    }
    let decoded =
        String::from_utf8(out).map_err(|_| "operation field is not valid UTF-8".to_string())?;
    if decoded.contains('\0') || decoded.contains('\r') {
        return Err("operation field contains a forbidden control byte".to_string());
    }
    Ok(decoded)
}

fn hex_value(value: u8) -> Result<u8, String> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err("operation percent escapes must use uppercase hexadecimal".to_string()),
    }
}

fn require_lower_hex(name: &str, value: &str, length: usize) -> Result<(), String> {
    if value.len() != length
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!(
            "{name} must contain exactly {length} lowercase hexadecimal characters"
        ));
    }
    Ok(())
}

fn require_safe_identifier(
    name: &str,
    value: &str,
    minimum: usize,
    maximum: usize,
    punctuation: &[u8],
) -> Result<(), String> {
    if !(minimum..=maximum).contains(&value.len())
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || punctuation.iter().any(|allowed| *allowed == byte)
        })
    {
        return Err(format!("{name} contains unsupported characters or length"));
    }
    Ok(())
}

fn bool_text(value: bool) -> &'static str {
    if value {
        "1"
    } else {
        "0"
    }
}

fn parse_bool(value: &str) -> Result<bool, String> {
    match value {
        "0" => Ok(false),
        "1" => Ok(true),
        _ => Err("operation boolean must be exactly 0 or 1".to_string()),
    }
}

fn parse_u64(name: &str, value: &str) -> Result<u64, String> {
    value.parse::<u64>().map_err(|_| format!("invalid {name}"))
}

fn optional_string(value: String) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn optional_parsed<T>(name: &str, value: String) -> Result<Option<T>, String>
where
    T: std::str::FromStr,
{
    if value.is_empty() {
        Ok(None)
    } else {
        value
            .parse::<T>()
            .map(Some)
            .map_err(|_| format!("invalid {name}"))
    }
}

fn parse_optional_enum<T>(
    value: String,
    parse: impl FnOnce(&str) -> Option<T>,
    name: &str,
) -> Result<Option<T>, String> {
    if value.is_empty() {
        Ok(None)
    } else {
        parse(&value)
            .map(Some)
            .ok_or_else(|| format!("unsupported {name}"))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn r6_dns_wire_roundtrip_preserves_old_versions_and_refuses_downgrade() {
        for absent in [false, true] {
            for planned in [false, true] {
                let mut value = request();
                value.route.mode = OperationRouteMode::Explicit;
                value.route.mwan3_member = None;
                value.route.fwmark_mask = Some(0x3f00);
                value.route.device_ifindex = Some(42);
                value.traffic_policy_explicit = true;
                if absent {
                    value.target_state = OperationTargetState::AbsentBootstrap;
                    value.capture_policy = Some(AutotuneCapturePolicyId::StandardV1);
                }
                if planned {
                    value.traffic_plan = Some(AutotuneTrafficPlan {
                        download_kbps: 100000,
                        upload_kbps: 20000,
                    });
                }
                let old = value.encode().unwrap();
                assert_eq!(OperationRequest::decode(&old).unwrap(), value);
                value.route.dns_server = Some("192.0.2.53".parse().unwrap());
                let encoded = value.encode().unwrap();
                let schema = 19 + u8::from(absent) + 2 * u8::from(planned);
                assert!(
                    encoded.starts_with(&format!("cake-autorate-operation\t{schema}\trequest\n"))
                );
                assert_eq!(OperationRequest::decode(&encoded).unwrap(), value);
                for old_schema in 4..=18 {
                    assert!(value.encode_for_test_schema(old_schema).is_err());
                }
                if !absent {
                    value.validate_admission_policy().unwrap();
                }
                let mut no_dns = value.clone();
                no_dns.route.dns_server = None;
                assert!(no_dns.validate_admission_policy().is_err());
                for field in [
                    "",
                    "route_dns_ipv4=127.0.0.1\n",
                    "route_dns_ipv4=::1\n",
                    "route_dns_ipv4=192.0.2.053\n",
                    "route_dns_ipv4=192.0.2.53\nroute_dns_ipv4=192.0.2.54\n",
                ] {
                    assert!(OperationRequest::decode(
                        &encoded.replace("route_dns_ipv4=192.0.2.53\n", field)
                    )
                    .is_err());
                }
            }
        }
    }

    #[test]
    fn r6_explicit_route_wire_requires_complete_independent_authority() {
        let mut value = request();
        value.route.mode = OperationRouteMode::Explicit;
        value.route.mwan3_member = None;
        value.route.fwmark_mask = Some(0x3f00);
        value.route.device_ifindex = Some(42);
        let encoded = value.encode().unwrap();
        assert_eq!(OperationRequest::decode(&encoded).unwrap(), value);
        assert!(value.validate_admission_policy().is_err());
        for mutation in 0..8 {
            let mut invalid = value.clone();
            match mutation {
                0 => invalid.route.mwan3_member = Some("wan".into()),
                1 => invalid.route.source_ip = None,
                2 => invalid.route.source_ip = Some("::1".parse().unwrap()),
                3 => invalid.route.device_ifindex = None,
                4 => invalid.route.routing_table = Some(0),
                5 => invalid.route.fwmark_mask = None,
                6 => invalid.route.fwmark = Some(0),
                _ => invalid.route.source_ip = Some("127.0.0.1".parse().unwrap()),
            }
            assert!(invalid.encode().is_err(), "mutation {mutation}");
        }
        assert!(value.encode_for_test_schema(11).is_err());
    }

    #[test]
    fn r6_linked_route_wire_roundtrip_never_downgrades_or_grants_execution() {
        for absent in [false, true] {
            for planned in [false, true] {
                for explicit_consent in [false, true] {
                    let mut value = request();
                    value.route.fwmark_mask = Some(0x3f00);
                    value.traffic_policy_explicit = explicit_consent;
                    if absent {
                        value.target_state = OperationTargetState::AbsentBootstrap;
                        value.capture_policy = Some(AutotuneCapturePolicyId::StandardV1);
                    }
                    if planned {
                        value.traffic_plan = Some(AutotuneTrafficPlan {
                            download_kbps: 100_000,
                            upload_kbps: 20_000,
                        });
                    }
                    if planned && !explicit_consent {
                        assert!(value.encode().is_err());
                        value.route.device_ifindex = Some(42);
                        assert!(value.encode().is_err());
                        continue;
                    }
                    let legacy = value.encode().unwrap();
                    value.route.device_ifindex = Some(42);
                    let encoded = value.encode().unwrap();
                    let schema = 15 + u8::from(absent) + 2 * u8::from(planned);
                    assert!(encoded
                        .starts_with(&format!("cake-autorate-operation\t{schema}\trequest\n")));
                    assert_eq!(OperationRequest::decode(&encoded).unwrap(), value);
                    assert!(value.validate_admission_policy().is_err());
                    for old_schema in 4..=14 {
                        assert!(value.encode_for_test_schema(old_schema).is_err());
                    }
                    for field in [
                        "",
                        "device_ifindex=0\n",
                        "device_ifindex=-1\n",
                        "device_ifindex=4294967296\n",
                        "device_ifindex=042\n",
                        "device_ifindex=42\ndevice_ifindex=43\n",
                    ] {
                        assert!(OperationRequest::decode(
                            &encoded.replace("device_ifindex=42\n", field)
                        )
                        .is_err());
                    }
                    value.route.device_ifindex = Some(43);
                    assert_ne!(value.encode().unwrap(), encoded);
                    value.route.device_ifindex = None;
                    assert_eq!(value.encode().unwrap(), legacy);
                }
            }
        }
    }

    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn fingerprint(byte: char) -> String {
        std::iter::repeat_n(byte, 64).collect()
    }

    fn request() -> OperationRequest {
        OperationRequest {
            identity: OperationIdentity {
                job_id: "0123456789abcdef0123456789abcdef".to_string(),
                job_token: fingerprint('a'),
                instance: "wan_sqm".to_string(),
                operation: OperationKind::FullAutotune,
                target_interface: "pppoe-wan".to_string(),
                route_fingerprint: fingerprint('b'),
                config_fingerprint: fingerprint('c'),
                sqm_fingerprint: fingerprint('d'),
            },
            created_unix_ms: 1_785_568_000_000,
            deadline_unix_ms: 1_785_571_600_000,
            origin: OperationOrigin::Luci,
            backend: "speedtest-go".to_string(),
            speedtest_direction: None,
            speedtest_server_id: None,
            speedtest_topology: None,
            route: OperationRouteIdentity {
                dns_server: None,
                device_ifindex: None,
                mode: OperationRouteMode::Mwan3,
                mwan3_member: Some("wan".to_string()),
                l3_device: "pppoe-wan".to_string(),
                source_ip: Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10))),
                fwmark: Some(256),
                routing_table: Some(1001),
                fwmark_mask: None,
            },
            target_state: OperationTargetState::ExistingManaged,
            capture_policy: None,
            managed_sqm_section: Some("wan_sqm".to_string()),
            profile: Some(AutotuneProfile::VariableLink),
            strategy: Some(CalibrationStrategy::FullRaw),
            access_medium: Some(AccessMedium::Cellular),
            access_source: Some(AccessEvidenceSource::UserSelected),
            access_confidence_percent: 100,
            capacity_learning_policy: Some(CapacityLearningPolicy::ScheduledActive),
            service_dl_cap_kbps: Some(1_000_000),
            service_ul_cap_kbps: Some(500_000),
            allow_sqm_disable: true,
            allow_active_traffic: false,
            scheduled_auto_apply_requested: false,
            traffic_budget: crate::operations::protocol::TrafficPolicy::Capped {
                max_bytes: 4_294_967_296,
            },
            traffic_policy_explicit: false,
            traffic_plan: None,
        }
    }

    #[test]
    fn r6_masked_route_wire_roundtrip_and_downgrade_refusal() {
        for absent in [false, true] {
            for planned in [false, true] {
                let mut value = request();
                value.traffic_policy_explicit = true;
                if absent {
                    value.target_state = OperationTargetState::AbsentBootstrap;
                    value.capture_policy = Some(AutotuneCapturePolicyId::StandardV1);
                }
                if planned {
                    value.traffic_plan = Some(AutotuneTrafficPlan {
                        download_kbps: 100_000,
                        upload_kbps: 20_000,
                    });
                }
                let legacy = value.encode().unwrap();
                let legacy_schema = match (absent, planned) {
                    (false, false) => 7,
                    (true, false) => 8,
                    (false, true) => 9,
                    (true, true) => 10,
                };
                value.route.fwmark_mask = Some(0x3f00);
                let encoded = value.encode().unwrap();
                assert!(encoded.starts_with(&format!(
                    "cake-autorate-operation\t{}\trequest\n",
                    legacy_schema + 4
                )));
                assert_eq!(OperationRequest::decode(&encoded).unwrap(), value);
                assert!(value.encode_for_test_schema(legacy_schema).is_err());
                assert!(
                    OperationRequest::decode(&encoded.replace("fwmark_mask=16128\n", "")).is_err()
                );
                assert!(OperationRequest::decode(
                    &encoded.replace("fwmark_mask=16128", "fwmark_mask=0")
                )
                .is_err());
                value.route.fwmark_mask = Some(0xff00);
                assert_ne!(value.encode().unwrap(), encoded);
                value.route.fwmark_mask = None;
                assert_eq!(value.encode().unwrap(), legacy);
                assert_eq!(
                    OperationRequest::decode(&legacy).unwrap().route.fwmark_mask,
                    None
                );
            }
        }
        let mut value = request();
        value.route.fwmark_mask = Some(0x3f00);
        let encoded = value.encode().unwrap();
        let decoded = OperationRequest::decode(&encoded).unwrap();
        assert_eq!(decoded, value);
        assert!(!decoded.traffic_policy_explicit);
        for absent in [false, true] {
            let mut masked = value.clone();
            if absent {
                masked.target_state = OperationTargetState::AbsentBootstrap;
                masked.capture_policy = Some(AutotuneCapturePolicyId::StandardV1);
            }
            let message = ControlMessage {
                control: ControlRequest {
                    request_id: "99".repeat(16),
                    command: ControlCommand::Start,
                    job_id: Some(masked.identity.job_id.clone()),
                    job_token: Some(masked.identity.job_token.clone()),
                },
                operation: Some(masked),
            };
            assert_eq!(
                ControlMessage::decode(&message.encode().unwrap()).unwrap(),
                message
            );
        }
        assert!(
            OperationRequest::decode(&encoded.replace("traffic_policy_explicit=0\n", "")).is_err()
        );
        value.traffic_budget = TrafficPolicy::Unlimited;
        assert!(
            value.encode().is_err(),
            "mask must not grant unlimited traffic"
        );
        value.traffic_budget = 1024_u64.into();
        value.traffic_policy_explicit = true;
        value.route.fwmark_mask = Some(1);
        assert!(value.encode().is_err(), "mark outside mask");
    }

    #[test]
    fn t2_planning_request_roundtrip_preserves_legacy_and_binds_pair() {
        for absent in [false, true] {
            let mut value = request();
            value.traffic_policy_explicit = true;
            if absent {
                value.target_state = OperationTargetState::AbsentBootstrap;
                value.capture_policy = Some(AutotuneCapturePolicyId::StandardV1);
            }
            let legacy = value.encode().unwrap();
            value.traffic_plan = Some(AutotuneTrafficPlan {
                download_kbps: 1_000_000,
                upload_kbps: 100_000,
            });
            let encoded = value.encode().unwrap();
            assert!(encoded.starts_with(if absent {
                PLANNED_BOOTSTRAP_REQUEST_HEADER
            } else {
                PLANNED_MANAGED_REQUEST_HEADER
            }));
            assert_eq!(OperationRequest::decode(&encoded).unwrap(), value);
            assert!(value.encode_for_schema(if absent { 8 } else { 7 }).is_err());
            assert!(
                OperationRequest::decode(&encoded.replace("planning_ul_kbps=100000\n", ""))
                    .is_err()
            );
            assert!(OperationRequest::decode(
                &encoded.replace("planning_dl_kbps=1000000", "planning_dl_kbps=0")
            )
            .is_err());
            let message = ControlMessage {
                control: ControlRequest {
                    request_id: "99".repeat(16),
                    command: ControlCommand::Start,
                    job_id: Some(value.identity.job_id.clone()),
                    job_token: Some(value.identity.job_token.clone()),
                },
                operation: Some(value.clone()),
            };
            assert_eq!(
                ControlMessage::decode(&message.encode().unwrap()).unwrap(),
                message
            );
            value.traffic_plan.as_mut().unwrap().download_kbps += 1;
            assert_ne!(value.encode().unwrap(), encoded);
            value.traffic_plan = None;
            assert_eq!(value.encode().unwrap(), legacy);
        }
    }

    #[test]
    fn t2_explicit_capped_and_unlimited_requests_roundtrip_without_legacy_downgrade() {
        for absent in [false, true] {
            let mut legacy = request();
            if absent {
                legacy.target_state = OperationTargetState::AbsentBootstrap;
                legacy.capture_policy = Some(AutotuneCapturePolicyId::StandardV1);
            }
            let old_bytes = legacy.encode().unwrap();
            assert_eq!(
                OperationRequest::decode(&old_bytes)
                    .unwrap()
                    .encode()
                    .unwrap(),
                old_bytes
            );
            for policy in [
                TrafficPolicy::Capped {
                    max_bytes: 32_000_000_000,
                },
                TrafficPolicy::Unlimited,
            ] {
                let mut explicit = legacy.clone();
                explicit.traffic_budget = policy;
                explicit.traffic_policy_explicit = true;
                let encoded = explicit.encode().unwrap();
                assert!(encoded.starts_with(if absent {
                    EXPLICIT_BOOTSTRAP_REQUEST_HEADER
                } else {
                    EXPLICIT_MANAGED_REQUEST_HEADER
                }));
                assert_eq!(OperationRequest::decode(&encoded).unwrap(), explicit);
                assert!(explicit
                    .encode_for_schema(if absent { 6 } else { 5 })
                    .is_err());
                let message = ControlMessage {
                    control: ControlRequest {
                        request_id: "99".repeat(16),
                        command: ControlCommand::Start,
                        job_id: Some(explicit.identity.job_id.clone()),
                        job_token: Some(explicit.identity.job_token.clone()),
                    },
                    operation: Some(explicit.clone()),
                };
                assert_eq!(
                    ControlMessage::decode(&message.encode().unwrap()).unwrap(),
                    message
                );
                if policy == TrafficPolicy::Unlimited {
                    assert!(encoded.contains("traffic_policy=unlimited\ntraffic_budget_bytes=\n"));
                    assert!(!encoded.contains(&u64::MAX.to_string()));
                    assert!(OperationRequest::decode(
                        &encoded.replace("traffic_budget_bytes=\n", "traffic_budget_bytes=1\n")
                    )
                    .is_err());
                    explicit.origin = OperationOrigin::Scheduler;
                    assert!(explicit.validate().is_err());
                    explicit.origin = OperationOrigin::Luci;
                    explicit.traffic_policy_explicit = false;
                    assert!(explicit.validate().is_err());
                }
            }
        }
    }

    #[test]
    fn request_has_stable_golden_encoding_and_round_trips() {
        let request = request();
        let encoded = request.encode().unwrap();
        let expected = concat!(
            "cake-autorate-operation\t5\trequest\n",
            "job_id=0123456789abcdef0123456789abcdef\n",
            "job_token=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
            "created_unix_ms=1785568000000\n",
            "deadline_unix_ms=1785571600000\n",
            "instance=wan_sqm\n",
            "operation=full_autotune\n",
            "origin=luci\n",
            "backend=speedtest-go\n",
            "speedtest_direction=\n",
            "speedtest_server_id=\n",
            "speedtest_topology=\n",
            "target_interface=pppoe-wan\n",
            "target_state=existing_managed\n",
            "managed_sqm_section=wan_sqm\n",
            "route_mode=mwan3\n",
            "mwan3_member=wan\n",
            "l3_device=pppoe-wan\n",
            "source_ip=192.0.2.10\n",
            "fwmark=256\n",
            "routing_table=1001\n",
            "route_fingerprint=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n",
            "config_fingerprint=cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc\n",
            "sqm_fingerprint=dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd\n",
            "profile=variable_link\n",
            "strategy=full_raw\n",
            "access_medium=cellular\n",
            "access_source=user_selected\n",
            "access_confidence_percent=100\n",
            "capacity_learning_policy=scheduled_active\n",
            "service_dl_cap_kbps=1000000\n",
            "service_ul_cap_kbps=500000\n",
            "allow_sqm_disable=1\n",
            "allow_active_traffic=0\n",
            "scheduled_auto_apply_requested=0\n",
            "traffic_budget_bytes=4294967296\n\n",
        );
        assert_eq!(encoded, expected);
        assert_eq!(OperationRequest::decode(&encoded).unwrap(), request);
    }

    #[test]
    fn public_existing_v4_request_has_frozen_encoding_and_decodes_exactly() {
        let expected = request();
        let encoded = expected.encode_for_schema(4).unwrap();
        let frozen = concat!(
            "cake-autorate-operation\t4\trequest\n",
            "job_id=0123456789abcdef0123456789abcdef\n",
            "job_token=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
            "created_unix_ms=1785568000000\n",
            "deadline_unix_ms=1785571600000\n",
            "instance=wan_sqm\n",
            "operation=full_autotune\n",
            "origin=luci\n",
            "backend=speedtest-go\n",
            "speedtest_direction=\n",
            "speedtest_server_id=\n",
            "speedtest_topology=\n",
            "target_interface=pppoe-wan\n",
            "managed_sqm_section=wan_sqm\n",
            "route_mode=mwan3\n",
            "mwan3_member=wan\n",
            "l3_device=pppoe-wan\n",
            "source_ip=192.0.2.10\n",
            "fwmark=256\n",
            "routing_table=1001\n",
            "route_fingerprint=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n",
            "config_fingerprint=cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc\n",
            "sqm_fingerprint=dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd\n",
            "profile=variable_link\n",
            "strategy=full_raw\n",
            "access_medium=cellular\n",
            "access_source=user_selected\n",
            "access_confidence_percent=100\n",
            "capacity_learning_policy=scheduled_active\n",
            "service_dl_cap_kbps=1000000\n",
            "service_ul_cap_kbps=500000\n",
            "allow_sqm_disable=1\n",
            "allow_active_traffic=0\n",
            "scheduled_auto_apply_requested=0\n",
            "traffic_budget_bytes=4294967296\n\n",
        );
        assert_eq!(encoded, frozen);
        let decoded = OperationRequest::decode(&encoded).unwrap();
        assert_eq!(decoded.target_state, OperationTargetState::ExistingManaged);
        assert_eq!(decoded, expected);
    }

    #[test]
    fn absent_bootstrap_v6_admission_is_exact_and_does_not_weaken_existing_requests() {
        let mut value = request();
        value.target_state = OperationTargetState::AbsentBootstrap;
        value.capture_policy = Some(AutotuneCapturePolicyId::StandardV1);
        value.validate().unwrap();

        let encoded = value.encode().unwrap();
        assert!(encoded.starts_with("cake-autorate-operation\t6\trequest\n"));
        assert!(encoded.contains("target_state=absent_bootstrap\n"));
        assert!(encoded.contains("capture_policy=standard_v1\n"));
        assert!(encoded.contains("capture_policy_sha256="));
        assert_eq!(OperationRequest::decode(&encoded).unwrap(), value);
        value.validate_admission_policy().unwrap();
        assert!(value
            .encode_for_schema(5)
            .unwrap_err()
            .contains("cannot represent absent bootstrap authority"));

        let policy_digest = AutotuneCapturePolicyId::StandardV1
            .canonical_sha256()
            .unwrap();
        let tampered_digest = encoded.replace(
            &format!("capture_policy_sha256={policy_digest}\n"),
            &format!("capture_policy_sha256={}\n", "ff".repeat(32)),
        );
        let tampered_error = OperationRequest::decode(&tampered_digest).unwrap_err();
        assert!(
            tampered_error.contains("capture policy digest mismatch"),
            "{tampered_error}"
        );

        let managed_absent = encoded
            .replacen(REQUEST_HEADER, MANAGED_REQUEST_HEADER, 1)
            .replace("capture_policy=standard_v1\n", "")
            .replace(&format!("capture_policy_sha256={policy_digest}\n"), "");
        assert!(OperationRequest::decode(&managed_absent)
            .unwrap_err()
            .contains("requires an explicit capture policy"));

        let policy_on_existing = encoded.replace(
            "target_state=absent_bootstrap\n",
            "target_state=existing_managed\n",
        );
        assert!(OperationRequest::decode(&policy_on_existing)
            .unwrap_err()
            .contains("only an absent bootstrap target"));

        let mut missing_caps = value.clone();
        missing_caps.service_dl_cap_kbps = None;
        assert!(missing_caps
            .validate_admission_policy()
            .unwrap_err()
            .contains("explicit download and upload service caps"));

        let mut unsupported_backend = value.clone();
        unsupported_backend.backend = "auto".to_string();
        assert!(unsupported_backend
            .validate_admission_policy()
            .unwrap_err()
            .contains("native speedtest-go backend"));

        let mut shaped_only = value.clone();
        shaped_only.strategy = Some(CalibrationStrategy::ShapedOnly);
        assert!(shaped_only
            .validate_admission_policy()
            .unwrap_err()
            .contains("requires Full Raw controls"));

        let mut no_bypass = value.clone();
        no_bypass.allow_sqm_disable = false;
        assert!(no_bypass
            .validate_admission_policy()
            .unwrap_err()
            .contains("directional SQM bypass authority"));

        let mut no_source = value.clone();
        no_source.route.source_ip = None;
        assert!(no_source
            .validate_admission_policy()
            .unwrap_err()
            .contains("explicit IPv4 route source"));
    }

    #[test]
    fn absent_bootstrap_structural_authority_is_narrowly_bound() {
        for operation in [OperationKind::AutomaticRating, OperationKind::GuidedRating] {
            let mut value = request();
            value.target_state = OperationTargetState::AbsentBootstrap;
            value.capture_policy = Some(AutotuneCapturePolicyId::StandardV1);
            value.identity.operation = operation;
            assert!(value
                .validate()
                .unwrap_err()
                .contains("valid only for Full Auto-Tune"));
        }

        let mut speedtest = request();
        speedtest.identity.operation = OperationKind::Speedtest;
        speedtest.target_state = OperationTargetState::AbsentBootstrap;
        speedtest.capture_policy = Some(AutotuneCapturePolicyId::StandardV2);
        speedtest.profile = None;
        speedtest.strategy = None;
        speedtest.access_medium = None;
        speedtest.access_source = None;
        speedtest.access_confidence_percent = 0;
        speedtest.capacity_learning_policy = None;
        speedtest.service_dl_cap_kbps = None;
        speedtest.service_ul_cap_kbps = None;
        speedtest.allow_sqm_disable = false;
        speedtest.allow_active_traffic = false;
        speedtest.speedtest_direction = Some(SpeedtestDirection::Both);
        speedtest.speedtest_topology = Some(SpeedtestTopology::Unshaped);
        speedtest.validate().unwrap();
        speedtest.validate_admission_policy().unwrap();

        let mut shaped_bootstrap_speedtest = speedtest.clone();
        shaped_bootstrap_speedtest.speedtest_topology = Some(SpeedtestTopology::Current);
        assert!(shaped_bootstrap_speedtest
            .validate_admission_policy()
            .unwrap_err()
            .contains("read-only unshaped policy"));

        for origin in [
            OperationOrigin::Scheduler,
            OperationOrigin::Recovery,
            OperationOrigin::Internal,
        ] {
            let mut value = request();
            value.target_state = OperationTargetState::AbsentBootstrap;
            value.capture_policy = Some(AutotuneCapturePolicyId::StandardV1);
            value.origin = origin;
            assert!(value
                .validate()
                .unwrap_err()
                .contains("requires a LuCI operation origin"));
        }

        let mut without_section = request();
        without_section.target_state = OperationTargetState::AbsentBootstrap;
        without_section.capture_policy = Some(AutotuneCapturePolicyId::StandardV1);
        without_section.managed_sqm_section = None;
        assert!(without_section
            .validate()
            .unwrap_err()
            .contains("requires a planned managed SQM section"));

        for invalid_section in ["wan-sqm", "wan.sqm"] {
            let mut value = request();
            value.target_state = OperationTargetState::AbsentBootstrap;
            value.capture_policy = Some(AutotuneCapturePolicyId::StandardV1);
            value.managed_sqm_section = Some(invalid_section.to_string());
            assert!(value
                .validate()
                .unwrap_err()
                .contains("managed SQM section name is invalid"));
        }

        // Existing managed requests retain the historical hyphen-compatible
        // syntax; only the new Absent bootstrap authority is narrowed.
        let mut existing_hyphenated_section = request();
        existing_hyphenated_section.managed_sqm_section = Some("wan-sqm".to_string());
        existing_hyphenated_section.validate().unwrap();

        let mut scheduled_apply = request();
        scheduled_apply.target_state = OperationTargetState::AbsentBootstrap;
        scheduled_apply.capture_policy = Some(AutotuneCapturePolicyId::StandardV1);
        scheduled_apply.scheduled_auto_apply_requested = true;
        assert!(scheduled_apply.validate().is_err());

        let mut missing_policy = request();
        missing_policy.target_state = OperationTargetState::AbsentBootstrap;
        assert!(missing_policy
            .validate()
            .unwrap_err()
            .contains("requires an explicit capture policy"));

        let mut policy_on_existing = request();
        policy_on_existing.capture_policy = Some(AutotuneCapturePolicyId::StandardV1);
        assert!(policy_on_existing
            .validate()
            .unwrap_err()
            .contains("only an absent bootstrap target"));
    }

    #[test]
    fn scheduled_auto_apply_authority_is_origin_and_operation_bound() {
        let mut value = request();
        value.scheduled_auto_apply_requested = true;
        assert!(value.validate().is_err());

        value.origin = OperationOrigin::Scheduler;
        value.validate().unwrap();
        assert_eq!(
            OperationRequest::decode(&value.encode().unwrap()).unwrap(),
            value
        );

        value.identity.operation = OperationKind::AutomaticRating;
        value.profile = None;
        value.strategy = None;
        value.access_medium = None;
        value.access_source = None;
        value.access_confidence_percent = 0;
        value.capacity_learning_policy = None;
        value.service_dl_cap_kbps = None;
        value.service_ul_cap_kbps = None;
        value.managed_sqm_section = None;
        value.allow_sqm_disable = false;
        assert!(value.validate().is_err());
    }

    #[test]
    fn parser_rejects_unknown_reordered_and_noncanonical_input() {
        let encoded = request().encode().unwrap();
        let unknown = encoded.replace("origin=luci\n", "unexpected=1\norigin=luci\n");
        assert!(OperationRequest::decode(&unknown).is_err());

        let reordered = encoded.replace(
            "operation=full_autotune\norigin=luci\n",
            "origin=luci\noperation=full_autotune\n",
        );
        assert!(OperationRequest::decode(&reordered).is_err());

        let noncanonical = encoded.replace("instance=wan_sqm", "instance=wan%5Fsqm");
        assert!(OperationRequest::decode(&noncanonical).is_err());
        let lowercase_escape = encoded.replace("instance=wan_sqm", "instance=wan%5fsqm");
        assert!(OperationRequest::decode(&lowercase_escape).is_err());
    }

    #[test]
    fn request_constraints_fail_closed() {
        let mut value = request();
        value.speedtest_server_id = Some(17372);
        assert!(value.validate().is_ok());
        value.speedtest_server_id = Some(0);
        assert!(value.validate().is_err());
        value.speedtest_server_id = None;
        value.speedtest_direction = Some(SpeedtestDirection::Both);
        assert!(value.validate().is_err());

        let mut value = request();
        value.route.mode = OperationRouteMode::Main;
        assert!(value.validate().is_err());

        let mut value = request();
        value.profile = None;
        assert!(value.validate().is_err());

        let mut value = request();
        value.managed_sqm_section = None;
        assert!(value.validate().is_err());
        value.managed_sqm_section = Some("invalid.section".to_string());
        assert!(value.validate().is_err());

        let mut value = request();
        value.identity.operation = OperationKind::AutomaticRating;
        assert!(value.validate().is_err());

        let mut value = request();
        value.identity.operation = OperationKind::AutomaticRating;
        value.profile = None;
        value.strategy = None;
        value.access_medium = None;
        value.access_source = None;
        value.access_confidence_percent = 0;
        value.capacity_learning_policy = None;
        value.service_dl_cap_kbps = None;
        value.service_ul_cap_kbps = None;
        value.managed_sqm_section = None;
        value.allow_sqm_disable = false;
        assert!(value.validate().is_ok());
        value.route.source_ip = None;
        assert!(value.validate().is_err());

        let mut value = request();
        value.identity.operation = OperationKind::Speedtest;
        value.profile = None;
        value.strategy = None;
        value.access_medium = None;
        value.access_source = None;
        value.access_confidence_percent = 0;
        value.capacity_learning_policy = None;
        value.service_dl_cap_kbps = None;
        value.service_ul_cap_kbps = None;
        value.managed_sqm_section = None;
        value.allow_sqm_disable = false;
        value.speedtest_direction = Some(SpeedtestDirection::Both);
        assert!(value.validate().is_err());
        value.speedtest_topology = Some(SpeedtestTopology::Current);
        assert!(value.validate().is_ok());
        value.speedtest_topology = Some(SpeedtestTopology::Unshaped);
        assert!(value.validate().is_ok());
        value.speedtest_server_id = Some(0);
        assert!(value.validate().is_err());

        let mut value = request();
        value.traffic_budget = (MAX_TRAFFIC_BUDGET_BYTES + 1).into();
        assert!(value.validate().is_err());
    }

    #[test]
    fn fixed_cap_request_and_replay_require_both_service_caps() {
        let expected = "fixed-cap capacity learning requires download and upload service hard caps";
        let mut value = request();
        value.capacity_learning_policy = Some(CapacityLearningPolicy::FixedCap);

        value.service_dl_cap_kbps = None;
        let encoded = value.encode().unwrap();
        let decoded = OperationRequest::decode(&encoded).unwrap();
        assert_eq!(decoded, value);
        assert_eq!(decoded.validate_admission_policy().unwrap_err(), expected);

        value.service_dl_cap_kbps = Some(1_000_000);
        value.service_ul_cap_kbps = None;
        assert_eq!(value.validate_admission_policy().unwrap_err(), expected);

        value.service_ul_cap_kbps = Some(500_000);
        value.validate_admission_policy().unwrap();
        let encoded = value.encode().unwrap();
        assert_eq!(OperationRequest::decode(&encoded).unwrap(), value);
    }

    #[test]
    fn control_request_is_canonical_and_job_scoped() {
        let ping = ControlRequest {
            request_id: "fedcba9876543210fedcba9876543210".to_string(),
            command: ControlCommand::Ping,
            job_id: None,
            job_token: None,
        };
        let encoded = ping.encode().unwrap();
        assert_eq!(
            encoded,
            concat!(
                "cake-autorate-operation\t2\tcontrol\n",
                "request_id=fedcba9876543210fedcba9876543210\n",
                "command=ping\n",
                "job_id=\n",
                "job_token=\n\n"
            )
        );
        assert_eq!(ControlRequest::decode(&encoded).unwrap(), ping);

        let start = ControlRequest {
            command: ControlCommand::Start,
            job_id: Some("0123456789abcdef0123456789abcdef".to_string()),
            job_token: Some(fingerprint('a')),
            ..ping
        };
        assert_eq!(
            ControlRequest::decode(&start.encode().unwrap()).unwrap(),
            start
        );

        let result = ControlRequest {
            command: ControlCommand::Result,
            ..start.clone()
        };
        assert_eq!(
            ControlRequest::decode(&result.encode().unwrap()).unwrap(),
            result
        );

        let invalid = ControlRequest {
            command: ControlCommand::Cancel,
            job_id: None,
            job_token: None,
            ..start
        };
        assert!(invalid.encode().is_err());
    }

    #[test]
    fn start_control_message_carries_one_identity_bound_operation_request() {
        let operation = request();
        let control = ControlRequest {
            request_id: "99999999999999999999999999999999".to_string(),
            command: ControlCommand::Start,
            job_id: Some(operation.identity.job_id.clone()),
            job_token: Some(operation.identity.job_token.clone()),
        };
        let message = ControlMessage {
            control,
            operation: Some(operation.clone()),
        };
        let encoded = message.encode().unwrap();
        assert!(encoded.contains("cake-autorate-operation\t4\trequest\n"));
        assert!(!encoded.contains("target_state="));
        assert_eq!(ControlMessage::decode(&encoded).unwrap(), message);

        assert!(operation.encode_for_test_schema(2).is_err());
        assert!(operation.encode_for_test_schema(3).is_err());

        let noncanonical_existing_v5 = format!(
            "{}{}",
            message.control.encode().unwrap(),
            operation.encode().unwrap()
        );
        assert!(ControlMessage::decode(&noncanonical_existing_v5)
            .unwrap_err()
            .contains("target lifecycle"));

        let mut bootstrap_operation = operation.clone();
        bootstrap_operation.target_state = OperationTargetState::AbsentBootstrap;
        bootstrap_operation.capture_policy = Some(AutotuneCapturePolicyId::StandardV1);
        let bootstrap = ControlMessage {
            control: message.control.clone(),
            operation: Some(bootstrap_operation),
        };
        let bootstrap_encoded = bootstrap.encode().unwrap();
        assert!(bootstrap_encoded.contains("cake-autorate-operation\t6\trequest\n"));
        assert!(bootstrap_encoded.contains("target_state=absent_bootstrap\n"));
        assert_eq!(
            ControlMessage::decode(&bootstrap_encoded).unwrap(),
            bootstrap
        );

        let mut mismatched = message.clone();
        mismatched.control.job_id = Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string());
        assert!(mismatched.encode().is_err());

        let status_with_payload = ControlMessage {
            control: ControlRequest {
                request_id: "99999999999999999999999999999999".to_string(),
                command: ControlCommand::Status,
                job_id: Some(operation.identity.job_id.clone()),
                job_token: Some(operation.identity.job_token.clone()),
            },
            operation: Some(operation),
        };
        assert!(status_with_payload.encode().is_err());
        assert!(ControlMessage::decode(&encoded[..encoded.len() - 1]).is_err());
    }
}
