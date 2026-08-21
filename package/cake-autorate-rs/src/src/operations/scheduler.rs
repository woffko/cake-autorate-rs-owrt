//! Deterministic native scheduler contracts.
//!
//! This module deliberately has no production wake loop yet.  It defines the
//! durable cursor, admission decision and conservative traffic ledger which
//! calibrationd will own at cutover.  Callers provide clock and current-state
//! observations explicitly: elapsed time can make a calendar slot due, but it
//! can never make route, runtime, recovery or accounting state trustworthy.

const CURSOR_HEADER: &str = "cake-autorate-native-schedule\t2";
const BUDGET_HEADER: &str = "cake-autorate-native-schedule-budget\t2";
const INSTANCE_STATE_HEADER_V2: &str = "cake-autorate-native-scheduler-state\t2";
const MAX_EXACT_BYTES: u64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchedulerGenerations {
    pub config_fingerprint: String,
    pub route_fingerprint: String,
    pub runtime_sequence: u64,
    pub coordinator_generation: String,
}

impl SchedulerGenerations {
    fn validate(&self) -> Result<(), String> {
        require_lower_hex("scheduler config fingerprint", &self.config_fingerprint, 64)?;
        require_lower_hex("scheduler route fingerprint", &self.route_fingerprint, 64)?;
        if self.runtime_sequence == 0 {
            return Err("scheduler runtime sequence must be non-zero".to_string());
        }
        require_lower_hex(
            "scheduler coordinator generation",
            &self.coordinator_generation,
            32,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedAttemptFence {
    pub due_unix_s: u64,
    pub interval_s: u64,
    pub generations: SchedulerGenerations,
    pub explicit_retry_sequence: u64,
}

impl FailedAttemptFence {
    fn validate(&self) -> Result<(), String> {
        if self.due_unix_s == 0 || self.interval_s == 0 {
            return Err("failed scheduler attempt is missing its due time or interval".to_string());
        }
        self.generations.validate()
    }

    fn blocks(
        &self,
        due_unix_s: u64,
        generations: &SchedulerGenerations,
        explicit_retry_sequence: u64,
    ) -> bool {
        self.due_unix_s == due_unix_s
            && self.generations == *generations
            && self.explicit_retry_sequence == explicit_retry_sequence
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleCursor {
    pub instance: String,
    pub sequence: u64,
    pub initial_due_unix_s: u64,
    pub last_success_unix_s: Option<u64>,
    pub failed_attempt: Option<FailedAttemptFence>,
}

impl ScheduleCursor {
    pub fn new(instance: String, initial_due_unix_s: u64) -> Result<Self, String> {
        let cursor = Self {
            instance,
            sequence: 1,
            initial_due_unix_s,
            last_success_unix_s: None,
            failed_attempt: None,
        };
        cursor.validate()?;
        Ok(cursor)
    }

    pub fn due_unix_s(&self, interval_s: u64) -> Result<u64, String> {
        if interval_s == 0 {
            return Err("scheduler interval must be non-zero".to_string());
        }
        let interval_due = match self.last_success_unix_s {
            Some(last) => last
                .checked_add(interval_s)
                .ok_or_else(|| "scheduler due time overflow".to_string()),
            None => Ok(self.initial_due_unix_s),
        }?;
        // `initial_due_unix_s` is also the durable lower bound for a scheduler
        // cursor. Native cursors keep it at or before their last success, while
        // a retry fence may independently move the next eligible run later.
        Ok(interval_due.max(self.initial_due_unix_s))
    }

    pub fn record_failure(&mut self, fence: FailedAttemptFence) -> Result<(), String> {
        fence.validate()?;
        if fence.due_unix_s != self.due_unix_s(fence.interval_s)? {
            return Err("scheduler failure does not belong to the current due slot".to_string());
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| "scheduler cursor sequence overflow".to_string())?;
        self.failed_attempt = Some(fence);
        self.validate()
    }

    pub fn record_success(
        &mut self,
        interval_s: u64,
        due_unix_s: u64,
        completed_unix_s: u64,
    ) -> Result<(), String> {
        if due_unix_s != self.due_unix_s(interval_s)? {
            return Err("scheduler completion does not belong to the current due slot".to_string());
        }
        if due_unix_s == 0 || completed_unix_s < due_unix_s {
            return Err("scheduler completion precedes its due calendar slot".to_string());
        }
        if self
            .last_success_unix_s
            .is_some_and(|last| completed_unix_s < last)
        {
            return Err("scheduler completion time moved backwards".to_string());
        }
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| "scheduler cursor sequence overflow".to_string())?;
        self.last_success_unix_s = Some(completed_unix_s);
        self.failed_attempt = None;
        self.validate()
    }

    pub fn validate(&self) -> Result<(), String> {
        require_identifier("scheduler instance", &self.instance)?;
        if self.sequence == 0 || self.initial_due_unix_s == 0 {
            return Err("scheduler cursor identity is incomplete".to_string());
        }
        if let Some(fence) = &self.failed_attempt {
            fence.validate()?;
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<String, String> {
        self.validate()?;
        let (
            failed_due,
            failed_interval,
            failed_config,
            failed_route,
            failed_runtime,
            failed_coordinator,
            retry,
        ) = match &self.failed_attempt {
            Some(fence) => (
                fence.due_unix_s.to_string(),
                fence.interval_s.to_string(),
                fence.generations.config_fingerprint.clone(),
                fence.generations.route_fingerprint.clone(),
                fence.generations.runtime_sequence.to_string(),
                fence.generations.coordinator_generation.clone(),
                fence.explicit_retry_sequence.to_string(),
            ),
            None => (
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
            ),
        };
        Ok(format!(
            "{CURSOR_HEADER}\ninstance\t{}\nsequence\t{}\ninitial_due_unix_s\t{}\nlast_success_unix_s\t{}\nfailed_due_unix_s\t{}\nfailed_interval_s\t{}\nfailed_config_fingerprint\t{}\nfailed_route_fingerprint\t{}\nfailed_runtime_sequence\t{}\nfailed_coordinator_generation\t{}\nfailed_explicit_retry_sequence\t{}\n",
            self.instance,
            self.sequence,
            self.initial_due_unix_s,
            optional_u64(self.last_success_unix_s),
            failed_due,
            failed_interval,
            failed_config,
            failed_route,
            failed_runtime,
            failed_coordinator,
            retry,
        ))
    }

    pub fn decode(input: &str) -> Result<Self, String> {
        let mut record = OrderedRecord::new(input, CURSOR_HEADER)?;
        let instance = record.field("instance")?;
        let sequence = parse_u64("scheduler sequence", &record.field("sequence")?)?;
        let initial_due_unix_s = parse_u64(
            "scheduler initial due time",
            &record.field("initial_due_unix_s")?,
        )?;
        let last_success_unix_s = parse_optional_u64(
            "scheduler last success",
            &record.field("last_success_unix_s")?,
        )?;
        let failed_due = record.field("failed_due_unix_s")?;
        let failed_interval = record.field("failed_interval_s")?;
        let failed_config = record.field("failed_config_fingerprint")?;
        let failed_route = record.field("failed_route_fingerprint")?;
        let failed_runtime = record.field("failed_runtime_sequence")?;
        let failed_coordinator = record.field("failed_coordinator_generation")?;
        let failed_retry = record.field("failed_explicit_retry_sequence")?;
        record.finish()?;
        let absent = failed_due == "-"
            && failed_interval == "-"
            && failed_config == "-"
            && failed_route == "-"
            && failed_runtime == "-"
            && failed_coordinator == "-"
            && failed_retry == "-";
        let present = failed_due != "-"
            && failed_interval != "-"
            && failed_config != "-"
            && failed_route != "-"
            && failed_runtime != "-"
            && failed_coordinator != "-"
            && failed_retry != "-";
        let failed_attempt = if absent {
            None
        } else if present {
            Some(FailedAttemptFence {
                due_unix_s: parse_u64("failed scheduler due time", &failed_due)?,
                interval_s: parse_u64("failed scheduler interval", &failed_interval)?,
                generations: SchedulerGenerations {
                    config_fingerprint: failed_config,
                    route_fingerprint: failed_route,
                    runtime_sequence: parse_u64(
                        "failed scheduler runtime sequence",
                        &failed_runtime,
                    )?,
                    coordinator_generation: failed_coordinator,
                },
                explicit_retry_sequence: parse_u64(
                    "failed scheduler retry sequence",
                    &failed_retry,
                )?,
            })
        } else {
            return Err("scheduler failed-attempt fence is partial".to_string());
        };
        let cursor = Self {
            instance,
            sequence,
            initial_due_unix_s,
            last_success_unix_s,
            failed_attempt,
        };
        cursor.validate()?;
        if cursor.encode()? != input {
            return Err("scheduler cursor record is not canonical".to_string());
        }
        Ok(cursor)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchedulerBlock {
    Disabled,
    ClockMovedBackwards,
    WindowClosed,
    ConfigurationPending,
    RecoveryPending,
    ManualJobPriority,
    CoordinatorBusy,
    RouteNotReady,
    RuntimeNotReady,
    QuietWindowPending,
    AccountingUnavailable,
    BudgetExhausted,
    UnchangedFailedAttempt,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchedulerDecision {
    Dormant(SchedulerBlock),
    WakeAt(u64),
    AwaitState(SchedulerBlock),
    Coalesced,
    Admit {
        due_unix_s: u64,
        traffic_budget_bytes: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SchedulerGates {
    pub configuration_committed: bool,
    pub recovery_clear: bool,
    pub manual_job_pending: bool,
    pub coordinator_idle: bool,
    pub route_ready: bool,
    pub runtime_ready: bool,
    pub quiet_window_ready: bool,
    pub accounting_healthy: bool,
    pub scheduled_job_active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchedulerObservation {
    pub now_unix_s: u64,
    pub window_open: bool,
    pub next_window_open_unix_s: Option<u64>,
    pub generations: SchedulerGenerations,
    pub explicit_retry_sequence: u64,
    pub gates: SchedulerGates,
}

pub fn evaluate_schedule(
    enabled: bool,
    interval_s: u64,
    maximum_job_traffic_bytes: u64,
    available_traffic_bytes: u64,
    cursor: &ScheduleCursor,
    observation: &SchedulerObservation,
) -> Result<SchedulerDecision, String> {
    cursor.validate()?;
    observation.generations.validate()?;
    if !enabled {
        return Ok(SchedulerDecision::Dormant(SchedulerBlock::Disabled));
    }
    if cursor
        .last_success_unix_s
        .is_some_and(|last| observation.now_unix_s < last)
    {
        return Ok(SchedulerDecision::AwaitState(
            SchedulerBlock::ClockMovedBackwards,
        ));
    }
    let due_unix_s = cursor.due_unix_s(interval_s)?;
    if observation.now_unix_s < due_unix_s {
        return Ok(SchedulerDecision::WakeAt(due_unix_s));
    }
    if !observation.window_open {
        return match observation.next_window_open_unix_s {
            Some(next) if next > observation.now_unix_s => Ok(SchedulerDecision::WakeAt(next)),
            _ => Ok(SchedulerDecision::AwaitState(SchedulerBlock::WindowClosed)),
        };
    }
    if observation.gates.scheduled_job_active {
        return Ok(SchedulerDecision::Coalesced);
    }
    for (ready, reason) in [
        (
            observation.gates.configuration_committed,
            SchedulerBlock::ConfigurationPending,
        ),
        (
            observation.gates.recovery_clear,
            SchedulerBlock::RecoveryPending,
        ),
        (
            !observation.gates.manual_job_pending,
            SchedulerBlock::ManualJobPriority,
        ),
        (
            observation.gates.coordinator_idle,
            SchedulerBlock::CoordinatorBusy,
        ),
        (observation.gates.route_ready, SchedulerBlock::RouteNotReady),
        (
            observation.gates.runtime_ready,
            SchedulerBlock::RuntimeNotReady,
        ),
        (
            observation.gates.quiet_window_ready,
            SchedulerBlock::QuietWindowPending,
        ),
        (
            observation.gates.accounting_healthy,
            SchedulerBlock::AccountingUnavailable,
        ),
    ] {
        if !ready {
            return Ok(SchedulerDecision::AwaitState(reason));
        }
    }
    if let Some(failure) = &cursor.failed_attempt {
        let same_authority = failure.generations == observation.generations
            && failure.explicit_retry_sequence == observation.explicit_retry_sequence;
        if same_authority && failure.due_unix_s != due_unix_s {
            return Err(
                "scheduler due time changed without a configuration or retry generation"
                    .to_string(),
            );
        }
        if failure.blocks(
            due_unix_s,
            &observation.generations,
            observation.explicit_retry_sequence,
        ) {
            return Ok(SchedulerDecision::AwaitState(
                SchedulerBlock::UnchangedFailedAttempt,
            ));
        }
    }
    if maximum_job_traffic_bytes > MAX_EXACT_BYTES || available_traffic_bytes > MAX_EXACT_BYTES {
        return Err("scheduler traffic budget exceeds the exact accounting range".to_string());
    }
    let traffic_budget_bytes = maximum_job_traffic_bytes.min(available_traffic_bytes);
    if traffic_budget_bytes == 0 {
        return Ok(SchedulerDecision::AwaitState(
            SchedulerBlock::BudgetExhausted,
        ));
    }
    Ok(SchedulerDecision::Admit {
        due_unix_s,
        traffic_budget_bytes,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetReservation {
    pub reservation_id: String,
    pub job_id: String,
    pub day: String,
    pub month: String,
    pub reserved_bytes: u64,
    pub authority: FailedAttemptFence,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetLedger {
    pub instance: String,
    pub sequence: u64,
    pub day: String,
    pub month: String,
    pub daily_limit_bytes: u64,
    pub monthly_limit_bytes: u64,
    pub daily_charged_bytes: u64,
    pub monthly_charged_bytes: u64,
    pub accounting_blocked: bool,
    pub reservation: Option<BudgetReservation>,
}

impl BudgetLedger {
    pub fn new(
        instance: String,
        day: String,
        month: String,
        daily_limit_bytes: u64,
        monthly_limit_bytes: u64,
    ) -> Result<Self, String> {
        let ledger = Self {
            instance,
            sequence: 1,
            day,
            month,
            daily_limit_bytes,
            monthly_limit_bytes,
            daily_charged_bytes: 0,
            monthly_charged_bytes: 0,
            accounting_blocked: false,
            reservation: None,
        };
        ledger.validate()?;
        Ok(ledger)
    }

    pub fn available_bytes(&self) -> Result<u64, String> {
        self.validate()?;
        if self.accounting_blocked || self.reservation.is_some() {
            return Ok(0);
        }
        Ok(self
            .daily_limit_bytes
            .saturating_sub(self.daily_charged_bytes)
            .min(
                self.monthly_limit_bytes
                    .saturating_sub(self.monthly_charged_bytes),
            ))
    }

    pub fn reconfigure_limits(
        &mut self,
        day: &str,
        month: &str,
        daily_limit_bytes: u64,
        monthly_limit_bytes: u64,
    ) -> Result<(), String> {
        validate_accounted_bytes("daily scheduler limit", daily_limit_bytes)?;
        validate_accounted_bytes("monthly scheduler limit", monthly_limit_bytes)?;
        if daily_limit_bytes == 0 || monthly_limit_bytes == 0 {
            return Err("scheduler traffic limits must be non-zero".to_string());
        }
        if self.reservation.is_some()
            && (self.daily_limit_bytes != daily_limit_bytes
                || self.monthly_limit_bytes != monthly_limit_bytes)
        {
            return Err(
                "scheduler traffic limits changed during an active reservation".to_string(),
            );
        }
        let mut next = self.clone();
        next.roll_period(day, month)?;
        if next.daily_limit_bytes == daily_limit_bytes
            && next.monthly_limit_bytes == monthly_limit_bytes
        {
            // Period rollover and limit reconfiguration are independent.
            // Committing the rolled clone is essential when an exhausted
            // ledger crosses a day or month while its configured limits stay
            // unchanged; otherwise every reservation path remains fenced by
            // the stale zero available balance forever.
            *self = next;
            return Ok(());
        }
        next.daily_limit_bytes = daily_limit_bytes;
        next.monthly_limit_bytes = monthly_limit_bytes;
        next.bump_sequence()?;
        next.validate()?;
        *self = next;
        Ok(())
    }

    pub fn roll_period(&mut self, day: &str, month: &str) -> Result<(), String> {
        validate_period(day, month)?;
        if day < self.day.as_str() || month < self.month.as_str() {
            return Err("scheduler budget calendar moved backwards".to_string());
        }
        if day == self.day && month == self.month {
            return Ok(());
        }
        let mut next = self.clone();
        if month != next.month {
            next.month = month.to_string();
            next.monthly_charged_bytes = 0;
        }
        if day != next.day {
            next.day = day.to_string();
            next.daily_charged_bytes = 0;
        }
        next.bump_sequence()?;
        next.validate()?;
        *self = next;
        Ok(())
    }

    pub fn reserve(
        &mut self,
        reservation_id: String,
        job_id: String,
        day: &str,
        month: &str,
        authority: FailedAttemptFence,
        reserved_bytes: u64,
    ) -> Result<(), String> {
        let mut next = self.clone();
        next.roll_period(day, month)?;
        if next.accounting_blocked || next.reservation.is_some() {
            return Err(
                "scheduler traffic accounting is not available for reservation".to_string(),
            );
        }
        let reservation = BudgetReservation {
            reservation_id,
            job_id,
            day: day.to_string(),
            month: month.to_string(),
            reserved_bytes,
            authority,
        };
        reservation.validate()?;
        if reserved_bytes > next.available_bytes()? {
            return Err("scheduler traffic reservation exceeds the remaining budget".to_string());
        }
        next.daily_charged_bytes = checked_bytes_add(
            next.daily_charged_bytes,
            reserved_bytes,
            "daily scheduler reservation",
        )?;
        next.monthly_charged_bytes = checked_bytes_add(
            next.monthly_charged_bytes,
            reserved_bytes,
            "monthly scheduler reservation",
        )?;
        next.reservation = Some(reservation);
        next.bump_sequence()?;
        next.validate()?;
        *self = next;
        Ok(())
    }

    /// Replace a conservative reservation with exact, identity-matched usage.
    ///
    /// This is also the only recovery path after `mark_accounting_unknown`:
    /// the block remains durable until the same reservation and job later
    /// obtain trusted exact accounting.
    pub fn settle(
        &mut self,
        reservation_id: &str,
        job_id: &str,
        day: &str,
        month: &str,
        consumed_bytes: u64,
    ) -> Result<(), String> {
        validate_accounted_bytes("scheduler consumed traffic", consumed_bytes)?;
        let reservation = self.exact_reservation(reservation_id, job_id)?.clone();
        let mut next = self.clone();
        next.roll_period(day, month)?;
        next.daily_charged_bytes = if reservation.day == day {
            replace_reservation_charge(
                next.daily_charged_bytes,
                reservation.reserved_bytes,
                consumed_bytes,
                "daily scheduler settlement",
            )?
        } else {
            checked_bytes_add(
                next.daily_charged_bytes,
                consumed_bytes,
                "cross-day scheduler settlement",
            )?
        };
        next.monthly_charged_bytes = if reservation.month == month {
            replace_reservation_charge(
                next.monthly_charged_bytes,
                reservation.reserved_bytes,
                consumed_bytes,
                "monthly scheduler settlement",
            )?
        } else {
            checked_bytes_add(
                next.monthly_charged_bytes,
                consumed_bytes,
                "cross-month scheduler settlement",
            )?
        };
        next.reservation = None;
        next.accounting_blocked = false;
        next.bump_sequence()?;
        next.validate()?;
        *self = next;
        Ok(())
    }

    /// Preserve the full reservation and block further scheduled traffic when
    /// the post-run interface counter is missing, reset or otherwise unknown.
    pub fn mark_accounting_unknown(
        &mut self,
        reservation_id: &str,
        job_id: &str,
    ) -> Result<(), String> {
        self.exact_reservation(reservation_id, job_id)?;
        let mut next = self.clone();
        next.accounting_blocked = true;
        next.bump_sequence()?;
        next.validate()?;
        *self = next;
        Ok(())
    }

    /// Accept the already charged full reservation after exact accounting has
    /// become permanently unavailable (for example because a reboot removed
    /// the tmpfs operation journal).  This is an explicit operator action: it
    /// never refunds bytes and cannot clear a live or otherwise healthy
    /// reservation.  The coordinator must first attest that the matching job
    /// is neither live nor backed by exact terminal traffic evidence; the
    /// ledger intentionally has no access to the tmpfs operation journal.
    pub fn acknowledge_accounting_unknown(&mut self) -> Result<u64, String> {
        if !self.accounting_blocked {
            return Err("scheduler traffic accounting is not blocked".to_string());
        }
        let charged_bytes = self
            .reservation
            .as_ref()
            .ok_or_else(|| "blocked scheduler accounting has no reservation".to_string())?
            .reserved_bytes;
        let mut next = self.clone();
        next.reservation = None;
        next.accounting_blocked = false;
        next.bump_sequence()?;
        next.validate()?;
        *self = next;
        Ok(charged_bytes)
    }

    fn exact_reservation(
        &self,
        reservation_id: &str,
        job_id: &str,
    ) -> Result<&BudgetReservation, String> {
        let reservation = self
            .reservation
            .as_ref()
            .ok_or_else(|| "scheduler budget has no active reservation".to_string())?;
        if reservation.reservation_id != reservation_id || reservation.job_id != job_id {
            return Err("scheduler budget reservation identity mismatch".to_string());
        }
        Ok(reservation)
    }

    fn bump_sequence(&mut self) -> Result<(), String> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| "scheduler budget sequence overflow".to_string())?;
        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        require_identifier("scheduler budget instance", &self.instance)?;
        if self.sequence == 0 {
            return Err("scheduler budget sequence must be non-zero".to_string());
        }
        validate_period(&self.day, &self.month)?;
        for (name, value) in [
            ("daily scheduler limit", self.daily_limit_bytes),
            ("monthly scheduler limit", self.monthly_limit_bytes),
            ("daily scheduler charge", self.daily_charged_bytes),
            ("monthly scheduler charge", self.monthly_charged_bytes),
        ] {
            validate_accounted_bytes(name, value)?;
        }
        if self.daily_limit_bytes == 0 || self.monthly_limit_bytes == 0 {
            return Err("scheduler traffic limits must be non-zero".to_string());
        }
        if let Some(reservation) = &self.reservation {
            reservation.validate()?;
            if reservation.day > self.day || reservation.month > self.month {
                return Err("scheduler reservation is dated in the future".to_string());
            }
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<String, String> {
        self.validate()?;
        let (
            reservation_id,
            job_id,
            reservation_day,
            reservation_month,
            reserved_bytes,
            reservation_due,
            reservation_interval,
            reservation_config,
            reservation_route,
            reservation_runtime,
            reservation_coordinator,
            reservation_retry,
        ) = match &self.reservation {
            Some(reservation) => (
                reservation.reservation_id.clone(),
                reservation.job_id.clone(),
                reservation.day.clone(),
                reservation.month.clone(),
                reservation.reserved_bytes.to_string(),
                reservation.authority.due_unix_s.to_string(),
                reservation.authority.interval_s.to_string(),
                reservation.authority.generations.config_fingerprint.clone(),
                reservation.authority.generations.route_fingerprint.clone(),
                reservation
                    .authority
                    .generations
                    .runtime_sequence
                    .to_string(),
                reservation
                    .authority
                    .generations
                    .coordinator_generation
                    .clone(),
                reservation.authority.explicit_retry_sequence.to_string(),
            ),
            None => (
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
            ),
        };
        Ok(format!(
            "{BUDGET_HEADER}\ninstance\t{}\nsequence\t{}\nday\t{}\nmonth\t{}\ndaily_limit_bytes\t{}\nmonthly_limit_bytes\t{}\ndaily_charged_bytes\t{}\nmonthly_charged_bytes\t{}\naccounting_blocked\t{}\nreservation_id\t{}\nreservation_job_id\t{}\nreservation_day\t{}\nreservation_month\t{}\nreservation_bytes\t{}\nreservation_due_unix_s\t{}\nreservation_interval_s\t{}\nreservation_config_fingerprint\t{}\nreservation_route_fingerprint\t{}\nreservation_runtime_sequence\t{}\nreservation_coordinator_generation\t{}\nreservation_explicit_retry_sequence\t{}\n",
            self.instance,
            self.sequence,
            self.day,
            self.month,
            self.daily_limit_bytes,
            self.monthly_limit_bytes,
            self.daily_charged_bytes,
            self.monthly_charged_bytes,
            if self.accounting_blocked { "1" } else { "0" },
            reservation_id,
            job_id,
            reservation_day,
            reservation_month,
            reserved_bytes,
            reservation_due,
            reservation_interval,
            reservation_config,
            reservation_route,
            reservation_runtime,
            reservation_coordinator,
            reservation_retry,
        ))
    }

    pub fn decode(input: &str) -> Result<Self, String> {
        let mut record = OrderedRecord::new(input, BUDGET_HEADER)?;
        let instance = record.field("instance")?;
        let sequence = parse_u64("scheduler budget sequence", &record.field("sequence")?)?;
        let day = record.field("day")?;
        let month = record.field("month")?;
        let daily_limit_bytes =
            parse_u64("daily scheduler limit", &record.field("daily_limit_bytes")?)?;
        let monthly_limit_bytes = parse_u64(
            "monthly scheduler limit",
            &record.field("monthly_limit_bytes")?,
        )?;
        let daily_charged_bytes = parse_u64(
            "daily scheduler charge",
            &record.field("daily_charged_bytes")?,
        )?;
        let monthly_charged_bytes = parse_u64(
            "monthly scheduler charge",
            &record.field("monthly_charged_bytes")?,
        )?;
        let accounting_blocked = match record.field("accounting_blocked")?.as_str() {
            "0" => false,
            "1" => true,
            _ => return Err("scheduler accounting flag is invalid".to_string()),
        };
        let reservation_id = record.field("reservation_id")?;
        let reservation_job_id = record.field("reservation_job_id")?;
        let reservation_day = record.field("reservation_day")?;
        let reservation_month = record.field("reservation_month")?;
        let reservation_bytes = record.field("reservation_bytes")?;
        let reservation_due = record.field("reservation_due_unix_s")?;
        let reservation_interval = record.field("reservation_interval_s")?;
        let reservation_config = record.field("reservation_config_fingerprint")?;
        let reservation_route = record.field("reservation_route_fingerprint")?;
        let reservation_runtime = record.field("reservation_runtime_sequence")?;
        let reservation_coordinator = record.field("reservation_coordinator_generation")?;
        let reservation_retry = record.field("reservation_explicit_retry_sequence")?;
        record.finish()?;
        let absent = reservation_id == "-"
            && reservation_job_id == "-"
            && reservation_day == "-"
            && reservation_month == "-"
            && reservation_bytes == "-"
            && reservation_due == "-"
            && reservation_interval == "-"
            && reservation_config == "-"
            && reservation_route == "-"
            && reservation_runtime == "-"
            && reservation_coordinator == "-"
            && reservation_retry == "-";
        let present = reservation_id != "-"
            && reservation_job_id != "-"
            && reservation_day != "-"
            && reservation_month != "-"
            && reservation_bytes != "-"
            && reservation_due != "-"
            && reservation_interval != "-"
            && reservation_config != "-"
            && reservation_route != "-"
            && reservation_runtime != "-"
            && reservation_coordinator != "-"
            && reservation_retry != "-";
        let reservation = if absent {
            None
        } else if present {
            Some(BudgetReservation {
                reservation_id,
                job_id: reservation_job_id,
                day: reservation_day,
                month: reservation_month,
                reserved_bytes: parse_u64("scheduler reservation", &reservation_bytes)?,
                authority: FailedAttemptFence {
                    due_unix_s: parse_u64("scheduler reservation due time", &reservation_due)?,
                    interval_s: parse_u64("scheduler reservation interval", &reservation_interval)?,
                    generations: SchedulerGenerations {
                        config_fingerprint: reservation_config,
                        route_fingerprint: reservation_route,
                        runtime_sequence: parse_u64(
                            "scheduler reservation runtime sequence",
                            &reservation_runtime,
                        )?,
                        coordinator_generation: reservation_coordinator,
                    },
                    explicit_retry_sequence: parse_u64(
                        "scheduler reservation retry sequence",
                        &reservation_retry,
                    )?,
                },
            })
        } else {
            return Err("scheduler budget reservation is partial".to_string());
        };
        let ledger = Self {
            instance,
            sequence,
            day,
            month,
            daily_limit_bytes,
            monthly_limit_bytes,
            daily_charged_bytes,
            monthly_charged_bytes,
            accounting_blocked,
            reservation,
        };
        ledger.validate()?;
        if ledger.encode()? != input {
            return Err("scheduler budget record is not canonical".to_string());
        }
        Ok(ledger)
    }
}

impl BudgetReservation {
    fn validate(&self) -> Result<(), String> {
        require_lower_hex("scheduler reservation id", &self.reservation_id, 32)?;
        require_lower_hex("scheduler reservation job id", &self.job_id, 32)?;
        validate_period(&self.day, &self.month)?;
        self.authority.validate()?;
        if self.reserved_bytes == 0 {
            return Err("scheduler reservation must be non-zero".to_string());
        }
        validate_accounted_bytes("scheduler reservation", self.reserved_bytes)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchedulerInstanceState {
    pub cursor: ScheduleCursor,
    pub budget: BudgetLedger,
    pub operator_warning: Option<String>,
}

impl SchedulerInstanceState {
    pub fn new(cursor: ScheduleCursor, budget: BudgetLedger) -> Result<Self, String> {
        let state = Self {
            cursor,
            budget,
            operator_warning: None,
        };
        state.validate()?;
        Ok(state)
    }

    pub fn validate(&self) -> Result<(), String> {
        self.cursor.validate()?;
        self.budget.validate()?;
        if self.cursor.instance != self.budget.instance {
            return Err("scheduler cursor and budget instances differ".to_string());
        }
        if let Some(warning) = &self.operator_warning {
            if warning.is_empty()
                || warning.len() > 512
                || warning.bytes().any(|byte| byte < b' ' || byte == 0x7f)
            {
                return Err(
                    "scheduler operator warning is not a bounded visible string".to_string()
                );
            }
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<String, String> {
        self.validate()?;
        let cursor = self.cursor.encode()?;
        let budget = self.budget.encode()?;
        let warning = self.operator_warning.as_deref().unwrap_or("");
        Ok(format!(
            "{INSTANCE_STATE_HEADER_V2}\ncursor_bytes\t{}\nbudget_bytes\t{}\nwarning_bytes\t{}\n{cursor}{budget}{warning}",
            cursor.len(),
            budget.len(),
            warning.len(),
        ))
    }

    pub fn decode(input: &str) -> Result<Self, String> {
        let mut lines = input.splitn(5, '\n');
        if lines.next() != Some(INSTANCE_STATE_HEADER_V2) {
            return Err("scheduler instance state header is invalid".to_string());
        }
        let cursor_bytes = parse_length_field(
            "cursor_bytes",
            lines
                .next()
                .ok_or_else(|| "scheduler instance state is missing cursor length".to_string())?,
        )?;
        let budget_bytes = parse_length_field(
            "budget_bytes",
            lines
                .next()
                .ok_or_else(|| "scheduler instance state is missing budget length".to_string())?,
        )?;
        let warning_bytes = parse_length_field(
            "warning_bytes",
            lines
                .next()
                .ok_or_else(|| "scheduler instance state is missing warning length".to_string())?,
        )?;
        let payload = lines
            .next()
            .ok_or_else(|| "scheduler instance state is missing its payload".to_string())?
            .as_bytes();
        let expected = cursor_bytes
            .checked_add(budget_bytes)
            .and_then(|value| value.checked_add(warning_bytes))
            .ok_or_else(|| "scheduler instance state length overflow".to_string())?;
        if payload.len() != expected {
            return Err("scheduler instance state payload length mismatch".to_string());
        }
        let (cursor, remainder) = payload.split_at(cursor_bytes);
        let (budget, warning) = remainder.split_at(budget_bytes);
        let cursor = std::str::from_utf8(cursor)
            .map_err(|_| "scheduler cursor state is not UTF-8".to_string())?;
        let budget = std::str::from_utf8(budget)
            .map_err(|_| "scheduler budget state is not UTF-8".to_string())?;
        let mut state = Self::new(
            ScheduleCursor::decode(cursor)?,
            BudgetLedger::decode(budget)?,
        )?;
        state.operator_warning = if warning.is_empty() {
            None
        } else {
            Some(
                std::str::from_utf8(warning)
                    .map_err(|_| "scheduler operator warning is not UTF-8".to_string())?
                    .to_string(),
            )
        };
        state.validate()?;
        if state.encode()? != input {
            return Err("scheduler instance state is not canonical".to_string());
        }
        Ok(state)
    }
}

fn parse_length_field(expected: &str, line: &str) -> Result<usize, String> {
    let (name, raw) = line
        .split_once('\t')
        .ok_or_else(|| "scheduler instance state length field is malformed".to_string())?;
    if name != expected {
        return Err(format!(
            "scheduler instance state expected {expected}, found {name}"
        ));
    }
    let value = parse_u64("scheduler instance state payload length", raw)?;
    usize::try_from(value)
        .map_err(|_| "scheduler instance state payload length is too large".to_string())
}

struct OrderedRecord<'a> {
    lines: std::str::Lines<'a>,
}

impl<'a> OrderedRecord<'a> {
    fn new(input: &'a str, header: &str) -> Result<Self, String> {
        if !input.ends_with('\n') || input.as_bytes().contains(&0) {
            return Err("scheduler record is not a complete text record".to_string());
        }
        let mut lines = input.lines();
        if lines.next() != Some(header) {
            return Err("scheduler record header is invalid".to_string());
        }
        Ok(Self { lines })
    }

    fn field(&mut self, expected: &str) -> Result<String, String> {
        let line = self
            .lines
            .next()
            .ok_or_else(|| format!("scheduler record field {expected} is missing"))?;
        let (name, value) = line
            .split_once('\t')
            .ok_or_else(|| format!("scheduler record field {expected} is malformed"))?;
        if name != expected || value.is_empty() || value.contains('\t') {
            return Err(format!("scheduler record field {expected} is invalid"));
        }
        Ok(value.to_string())
    }

    fn finish(mut self) -> Result<(), String> {
        if self.lines.next().is_some() {
            return Err("scheduler record has unexpected fields".to_string());
        }
        Ok(())
    }
}

fn optional_u64(value: Option<u64>) -> String {
    value.map_or_else(|| "-".to_string(), |value| value.to_string())
}

fn parse_optional_u64(name: &str, value: &str) -> Result<Option<u64>, String> {
    if value == "-" {
        Ok(None)
    } else {
        parse_u64(name, value).map(Some)
    }
}

fn parse_u64(name: &str, value: &str) -> Result<u64, String> {
    if value.is_empty() || (value.len() > 1 && value.starts_with('0')) {
        return Err(format!("{name} is not canonical"));
    }
    value
        .parse::<u64>()
        .map_err(|_| format!("{name} is not an unsigned integer"))
}

fn validate_period(day: &str, month: &str) -> Result<(), String> {
    if day.len() != 8
        || month.len() != 6
        || !day.bytes().all(|value| value.is_ascii_digit())
        || !month.bytes().all(|value| value.is_ascii_digit())
        || !day.starts_with(month)
    {
        return Err("scheduler budget period is invalid".to_string());
    }
    let year = day[0..4]
        .parse::<u16>()
        .map_err(|_| "scheduler budget year is invalid".to_string())?;
    let parsed_month = day[4..6]
        .parse::<u8>()
        .map_err(|_| "scheduler budget month is invalid".to_string())?;
    let parsed_day = day[6..8]
        .parse::<u8>()
        .map_err(|_| "scheduler budget day is invalid".to_string())?;
    if !(1970..=9999).contains(&year) || !(1..=12).contains(&parsed_month) {
        return Err("scheduler budget calendar date is invalid".to_string());
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let maximum_day = match parsed_month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if parsed_day == 0 || parsed_day > maximum_day {
        return Err("scheduler budget calendar date is invalid".to_string());
    }
    Ok(())
}

fn validate_accounted_bytes(name: &str, value: u64) -> Result<(), String> {
    if value > MAX_EXACT_BYTES {
        return Err(format!("{name} exceeds the exact accounting range"));
    }
    Ok(())
}

fn checked_bytes_add(lhs: u64, rhs: u64, name: &str) -> Result<u64, String> {
    let value = lhs
        .checked_add(rhs)
        .ok_or_else(|| format!("{name} overflow"))?;
    validate_accounted_bytes(name, value)?;
    Ok(value)
}

fn replace_reservation_charge(
    charged: u64,
    reserved: u64,
    consumed: u64,
    name: &str,
) -> Result<u64, String> {
    let without_reservation = charged
        .checked_sub(reserved)
        .ok_or_else(|| format!("{name} is missing its reservation charge"))?;
    checked_bytes_add(without_reservation, consumed, name)
}

pub(crate) fn validate_scheduler_instance(value: &str) -> Result<(), String> {
    require_identifier("scheduler instance", value)
}

fn require_identifier(name: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.:-".contains(&byte))
    {
        return Err(format!("{name} is invalid"));
    }
    Ok(())
}

fn require_lower_hex(name: &str, value: &str, bytes: usize) -> Result<(), String> {
    if value.len() != bytes
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!("{name} is invalid"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn generations(seed: char) -> SchedulerGenerations {
        SchedulerGenerations {
            config_fingerprint: seed.to_string().repeat(64),
            route_fingerprint: if seed == 'a' {
                "b".repeat(64)
            } else {
                "a".repeat(64)
            },
            runtime_sequence: 7,
            coordinator_generation: "c".repeat(32),
        }
    }

    fn ready_observation(now_unix_s: u64) -> SchedulerObservation {
        SchedulerObservation {
            now_unix_s,
            window_open: true,
            next_window_open_unix_s: None,
            generations: generations('a'),
            explicit_retry_sequence: 0,
            gates: SchedulerGates {
                configuration_committed: true,
                recovery_clear: true,
                manual_job_pending: false,
                coordinator_idle: true,
                route_ready: true,
                runtime_ready: true,
                quiet_window_ready: true,
                accounting_healthy: true,
                scheduled_job_active: false,
            },
        }
    }

    fn authority(due_unix_s: u64) -> FailedAttemptFence {
        FailedAttemptFence {
            due_unix_s,
            interval_s: 3_600,
            generations: generations('a'),
            explicit_retry_sequence: 0,
        }
    }

    #[test]
    fn future_and_overdue_slots_use_one_calendar_wakeup() {
        let cursor = ScheduleCursor::new("wan_sqm".to_string(), 1_000).unwrap();
        assert_eq!(
            evaluate_schedule(true, 3_600, 1_000, 1_000, &cursor, &ready_observation(900)).unwrap(),
            SchedulerDecision::WakeAt(1_000)
        );
        assert_eq!(
            evaluate_schedule(true, 3_600, 1_000, 800, &cursor, &ready_observation(2_000)).unwrap(),
            SchedulerDecision::Admit {
                due_unix_s: 1_000,
                traffic_budget_bytes: 800,
            }
        );
    }

    #[test]
    fn wall_clock_rollback_and_unrepresentable_budget_fail_closed() {
        let mut cursor = ScheduleCursor::new("wan_sqm".to_string(), 1_000).unwrap();
        cursor.record_success(3_600, 1_000, 1_100).unwrap();
        assert_eq!(
            evaluate_schedule(
                true,
                3_600,
                1_000,
                1_000,
                &cursor,
                &ready_observation(1_050),
            )
            .unwrap(),
            SchedulerDecision::AwaitState(SchedulerBlock::ClockMovedBackwards)
        );
        assert!(evaluate_schedule(
            true,
            3_600,
            MAX_EXACT_BYTES + 1,
            1_000,
            &cursor,
            &ready_observation(5_000),
        )
        .is_err());
    }

    #[test]
    fn manual_work_and_current_state_outrank_an_overdue_schedule() {
        let cursor = ScheduleCursor::new("wan_sqm".to_string(), 1_000).unwrap();
        let mut observation = ready_observation(2_000);
        observation.gates.manual_job_pending = true;
        assert_eq!(
            evaluate_schedule(true, 3_600, 1_000, 1_000, &cursor, &observation).unwrap(),
            SchedulerDecision::AwaitState(SchedulerBlock::ManualJobPriority)
        );
        observation.gates.manual_job_pending = false;
        observation.gates.route_ready = false;
        assert_eq!(
            evaluate_schedule(true, 3_600, 1_000, 1_000, &cursor, &observation).unwrap(),
            SchedulerDecision::AwaitState(SchedulerBlock::RouteNotReady)
        );
    }

    #[test]
    fn failed_slot_never_retries_merely_because_time_passed() {
        let mut cursor = ScheduleCursor::new("wan_sqm".to_string(), 1_000).unwrap();
        let observation = ready_observation(2_000);
        cursor
            .record_failure(FailedAttemptFence {
                due_unix_s: 1_000,
                interval_s: 3_600,
                generations: observation.generations.clone(),
                explicit_retry_sequence: 0,
            })
            .unwrap();
        assert_eq!(
            evaluate_schedule(
                true,
                3_600,
                1_000,
                1_000,
                &cursor,
                &ready_observation(200_000),
            )
            .unwrap(),
            SchedulerDecision::AwaitState(SchedulerBlock::UnchangedFailedAttempt)
        );
        let mut changed = ready_observation(200_000);
        changed.generations.runtime_sequence += 1;
        assert!(matches!(
            evaluate_schedule(true, 3_600, 1_000, 1_000, &cursor, &changed).unwrap(),
            SchedulerDecision::Admit { .. }
        ));
        let mut explicit = ready_observation(200_000);
        explicit.explicit_retry_sequence = 1;
        assert!(matches!(
            evaluate_schedule(true, 3_600, 1_000, 1_000, &cursor, &explicit).unwrap(),
            SchedulerDecision::Admit { .. }
        ));
    }

    #[test]
    fn cadence_cannot_change_without_a_matching_configuration_generation() {
        let mut cursor = ScheduleCursor::new("wan_sqm".to_string(), 1_000).unwrap();
        let observation = ready_observation(20_000);
        cursor.record_success(3_600, 1_000, 1_100).unwrap();
        cursor
            .record_failure(FailedAttemptFence {
                due_unix_s: 4_700,
                interval_s: 3_600,
                generations: observation.generations.clone(),
                explicit_retry_sequence: 0,
            })
            .unwrap();
        assert!(evaluate_schedule(true, 7_200, 1_000, 1_000, &cursor, &observation).is_err());
        let mut changed = observation;
        changed.generations.config_fingerprint = "d".repeat(64);
        assert!(matches!(
            evaluate_schedule(true, 7_200, 1_000, 1_000, &cursor, &changed).unwrap(),
            SchedulerDecision::Admit { .. }
        ));
    }

    #[test]
    fn a_closed_window_uses_only_the_supplied_calendar_wakeup() {
        let cursor = ScheduleCursor::new("wan_sqm".to_string(), 1_000).unwrap();
        let mut observation = ready_observation(2_000);
        observation.window_open = false;
        observation.next_window_open_unix_s = Some(5_000);
        assert_eq!(
            evaluate_schedule(true, 3_600, 1_000, 1_000, &cursor, &observation).unwrap(),
            SchedulerDecision::WakeAt(5_000)
        );
        observation.next_window_open_unix_s = Some(2_000);
        assert_eq!(
            evaluate_schedule(true, 3_600, 1_000, 1_000, &cursor, &observation).unwrap(),
            SchedulerDecision::AwaitState(SchedulerBlock::WindowClosed)
        );
    }

    #[test]
    fn adopted_retry_lower_bound_preserves_exact_last_success_and_next_due() {
        let mut cursor = ScheduleCursor::new("wan_sqm".to_string(), 9_000).unwrap();
        cursor.last_success_unix_s = Some(5_000);
        cursor.validate().unwrap();
        assert_eq!(cursor.last_success_unix_s, Some(5_000));
        assert_eq!(cursor.due_unix_s(3_600).unwrap(), 9_000);
        assert_eq!(
            ScheduleCursor::decode(&cursor.encode().unwrap()).unwrap(),
            cursor
        );

        cursor.record_success(3_600, 9_000, 9_100).unwrap();
        assert_eq!(cursor.due_unix_s(3_600).unwrap(), 12_700);
    }

    #[test]
    fn cursor_round_trip_preserves_failure_fence_and_rejects_partial_state() {
        let mut cursor = ScheduleCursor::new("wan_sqm".to_string(), 1_000).unwrap();
        cursor
            .record_failure(FailedAttemptFence {
                due_unix_s: 1_000,
                interval_s: 3_600,
                generations: generations('a'),
                explicit_retry_sequence: 3,
            })
            .unwrap();
        let encoded = cursor.encode().unwrap();
        assert_eq!(ScheduleCursor::decode(&encoded).unwrap(), cursor);
        let partial = encoded.replace(
            &format!("failed_route_fingerprint\t{}", "b".repeat(64)),
            "failed_route_fingerprint\t-",
        );
        assert!(ScheduleCursor::decode(&partial).is_err());
        assert!(ScheduleCursor::decode(encoded.trim_end()).is_err());
    }

    #[test]
    fn successful_slot_advances_from_completion_and_clears_failure() {
        let mut cursor = ScheduleCursor::new("wan_sqm".to_string(), 1_000).unwrap();
        cursor
            .record_failure(FailedAttemptFence {
                due_unix_s: 1_000,
                interval_s: 3_600,
                generations: generations('a'),
                explicit_retry_sequence: 0,
            })
            .unwrap();
        cursor.record_success(3_600, 1_000, 1_100).unwrap();
        assert_eq!(cursor.due_unix_s(3_600).unwrap(), 4_700);
        assert!(cursor.failed_attempt.is_none());
        assert!(cursor.record_success(3_600, 4_700, 1_050).is_err());
    }

    #[test]
    fn reserved_budget_survives_a_record_round_trip_fully_charged() {
        let mut ledger = BudgetLedger::new(
            "wan_sqm".to_string(),
            "20260805".to_string(),
            "202608".to_string(),
            10_000,
            50_000,
        )
        .unwrap();
        ledger
            .reserve(
                "1".repeat(32),
                "2".repeat(32),
                "20260805",
                "202608",
                authority(1_000),
                4_000,
            )
            .unwrap();
        assert_eq!(ledger.daily_charged_bytes, 4_000);
        assert_eq!(ledger.monthly_charged_bytes, 4_000);
        assert_eq!(ledger.available_bytes().unwrap(), 0);
        let recovered = BudgetLedger::decode(&ledger.encode().unwrap()).unwrap();
        assert_eq!(recovered, ledger);
        assert!(recovered.reservation.is_some());
    }

    #[test]
    fn scheduler_state_canonically_persists_operator_warning_and_rejects_v1() {
        let cursor = ScheduleCursor::new("wan_sqm".to_string(), 1_000).unwrap();
        let budget = BudgetLedger::new(
            "wan_sqm".to_string(),
            "20260805".to_string(),
            "202608".to_string(),
            10_000,
            50_000,
        )
        .unwrap();
        let mut state = SchedulerInstanceState::new(cursor, budget).unwrap();
        let current = state.encode().unwrap();
        let retired_v1 = current
            .replacen(
                INSTANCE_STATE_HEADER_V2,
                "cake-autorate-native-scheduler-state\t1",
                1,
            )
            .replace("warning_bytes\t0\n", "");
        assert!(SchedulerInstanceState::decode(&retired_v1).is_err());

        state.operator_warning = Some(
            "A pre-upgrade scheduled result requires explicit Review or a fresh run.".to_string(),
        );
        let v2 = state.encode().unwrap();
        assert!(v2.starts_with(INSTANCE_STATE_HEADER_V2));
        assert_eq!(SchedulerInstanceState::decode(&v2).unwrap(), state);
        state.operator_warning = Some("bad\nwarning".to_string());
        assert!(state.encode().is_err());
    }

    #[test]
    fn exact_settlement_replaces_reservation_with_observed_bytes() {
        let mut ledger = BudgetLedger::new(
            "wan_sqm".to_string(),
            "20260805".to_string(),
            "202608".to_string(),
            10_000,
            50_000,
        )
        .unwrap();
        ledger
            .reserve(
                "1".repeat(32),
                "2".repeat(32),
                "20260805",
                "202608",
                authority(1_000),
                4_000,
            )
            .unwrap();
        ledger
            .settle(
                &"1".repeat(32),
                &"2".repeat(32),
                "20260805",
                "202608",
                2_500,
            )
            .unwrap();
        assert_eq!(ledger.daily_charged_bytes, 2_500);
        assert_eq!(ledger.monthly_charged_bytes, 2_500);
        assert_eq!(ledger.available_bytes().unwrap(), 7_500);
        assert!(ledger.reservation.is_none());
    }

    #[test]
    fn cross_period_settlement_charges_observed_bytes_to_the_new_period() {
        let mut ledger = BudgetLedger::new(
            "wan_sqm".to_string(),
            "20260831".to_string(),
            "202608".to_string(),
            10_000,
            50_000,
        )
        .unwrap();
        ledger
            .reserve(
                "1".repeat(32),
                "2".repeat(32),
                "20260831",
                "202608",
                authority(1_000),
                4_000,
            )
            .unwrap();
        ledger
            .settle(
                &"1".repeat(32),
                &"2".repeat(32),
                "20260901",
                "202609",
                2_500,
            )
            .unwrap();
        assert_eq!(ledger.day, "20260901");
        assert_eq!(ledger.month, "202609");
        assert_eq!(ledger.daily_charged_bytes, 2_500);
        assert_eq!(ledger.monthly_charged_bytes, 2_500);
    }

    #[test]
    fn unchanged_limits_still_commit_day_and_month_rollover() {
        let mut ledger = BudgetLedger::new(
            "wan_sqm".to_string(),
            "20260805".to_string(),
            "202608".to_string(),
            10_000,
            50_000,
        )
        .unwrap();
        ledger
            .reserve(
                "1".repeat(32),
                "2".repeat(32),
                "20260805",
                "202608",
                authority(1_000),
                10_000,
            )
            .unwrap();
        ledger
            .settle(
                &"1".repeat(32),
                &"2".repeat(32),
                "20260805",
                "202608",
                10_000,
            )
            .unwrap();
        assert_eq!(ledger.available_bytes().unwrap(), 0);

        let day_sequence = ledger.sequence;
        ledger
            .reconfigure_limits("20260806", "202608", 10_000, 50_000)
            .unwrap();
        assert_eq!(ledger.sequence, day_sequence + 1);
        assert_eq!(ledger.day, "20260806");
        assert_eq!(ledger.month, "202608");
        assert_eq!(ledger.daily_charged_bytes, 0);
        assert_eq!(ledger.monthly_charged_bytes, 10_000);
        assert_eq!(ledger.available_bytes().unwrap(), 10_000);

        let month_sequence = ledger.sequence;
        ledger
            .reconfigure_limits("20260901", "202609", 10_000, 50_000)
            .unwrap();
        assert_eq!(ledger.sequence, month_sequence + 1);
        assert_eq!(ledger.day, "20260901");
        assert_eq!(ledger.month, "202609");
        assert_eq!(ledger.daily_charged_bytes, 0);
        assert_eq!(ledger.monthly_charged_bytes, 0);
        assert_eq!(ledger.available_bytes().unwrap(), 10_000);
    }

    #[test]
    fn unknown_counter_retains_full_charge_and_blocks_new_work() {
        let mut ledger = BudgetLedger::new(
            "wan_sqm".to_string(),
            "20260805".to_string(),
            "202608".to_string(),
            10_000,
            50_000,
        )
        .unwrap();
        ledger
            .reserve(
                "1".repeat(32),
                "2".repeat(32),
                "20260805",
                "202608",
                authority(1_000),
                4_000,
            )
            .unwrap();
        ledger
            .mark_accounting_unknown(&"1".repeat(32), &"2".repeat(32))
            .unwrap();
        assert!(ledger.accounting_blocked);
        assert_eq!(ledger.daily_charged_bytes, 4_000);
        assert_eq!(ledger.available_bytes().unwrap(), 0);
        assert!(ledger
            .reserve(
                "3".repeat(32),
                "4".repeat(32),
                "20260805",
                "202608",
                authority(1_000),
                1,
            )
            .is_err());
    }

    #[test]
    fn operator_acknowledgement_keeps_full_charge_and_clears_only_unknown_identity() {
        let mut ledger = BudgetLedger::new(
            "wan_sqm".to_string(),
            "20260805".to_string(),
            "202608".to_string(),
            10_000,
            50_000,
        )
        .unwrap();
        ledger
            .reserve(
                "1".repeat(32),
                "2".repeat(32),
                "20260805",
                "202608",
                authority(1_000),
                4_000,
            )
            .unwrap();
        assert!(ledger.acknowledge_accounting_unknown().is_err());
        ledger
            .mark_accounting_unknown(&"1".repeat(32), &"2".repeat(32))
            .unwrap();

        assert_eq!(ledger.acknowledge_accounting_unknown().unwrap(), 4_000);
        assert!(!ledger.accounting_blocked);
        assert!(ledger.reservation.is_none());
        assert_eq!(ledger.daily_charged_bytes, 4_000);
        assert_eq!(ledger.monthly_charged_bytes, 4_000);
        assert_eq!(ledger.available_bytes().unwrap(), 6_000);
        assert!(ledger.acknowledge_accounting_unknown().is_err());
    }

    #[test]
    fn later_exact_settlement_is_the_explicit_unknown_accounting_recovery_path() {
        let mut ledger = BudgetLedger::new(
            "wan_sqm".to_string(),
            "20260805".to_string(),
            "202608".to_string(),
            10_000,
            50_000,
        )
        .unwrap();
        ledger
            .reserve(
                "1".repeat(32),
                "2".repeat(32),
                "20260805",
                "202608",
                authority(1_000),
                4_000,
            )
            .unwrap();
        ledger
            .mark_accounting_unknown(&"1".repeat(32), &"2".repeat(32))
            .unwrap();
        ledger
            .settle(
                &"1".repeat(32),
                &"2".repeat(32),
                "20260805",
                "202608",
                2_500,
            )
            .unwrap();
        assert!(!ledger.accounting_blocked);
        assert!(ledger.reservation.is_none());
        assert_eq!(ledger.daily_charged_bytes, 2_500);
    }

    #[test]
    fn budget_identity_and_calendar_rollback_fail_closed() {
        let mut ledger = BudgetLedger::new(
            "wan_sqm".to_string(),
            "20260805".to_string(),
            "202608".to_string(),
            10_000,
            50_000,
        )
        .unwrap();
        ledger
            .reserve(
                "1".repeat(32),
                "2".repeat(32),
                "20260805",
                "202608",
                authority(1_000),
                4_000,
            )
            .unwrap();
        assert!(ledger
            .settle(&"3".repeat(32), &"2".repeat(32), "20260805", "202608", 1,)
            .is_err());
        assert!(ledger.roll_period("20260804", "202608").is_err());
        assert_eq!(ledger.day, "20260805");
    }

    #[test]
    fn budget_decoder_rejects_reordering_and_noncanonical_numbers() {
        let ledger = BudgetLedger::new(
            "wan_sqm".to_string(),
            "20260805".to_string(),
            "202608".to_string(),
            10_000,
            50_000,
        )
        .unwrap();
        let encoded = ledger.encode().unwrap();
        assert_eq!(BudgetLedger::decode(&encoded).unwrap(), ledger);
        assert!(BudgetLedger::decode(&encoded.replace("sequence\t1", "sequence\t01")).is_err());
        assert!(
            BudgetLedger::decode(&encoded.replace("day\t20260805", "month\t20260805")).is_err()
        );
        assert!(BudgetLedger::new(
            "wan_sqm".to_string(),
            "20261301".to_string(),
            "202613".to_string(),
            1,
            1,
        )
        .is_err());
        assert!(BudgetLedger::new(
            "wan_sqm".to_string(),
            "20260229".to_string(),
            "202602".to_string(),
            1,
            1,
        )
        .is_err());
        assert!(BudgetLedger::new(
            "wan_sqm".to_string(),
            "20240229".to_string(),
            "202402".to_string(),
            1,
            1,
        )
        .is_ok());
    }
}
