//! Read-only native scheduler status projection.
//!
//! The coordinator owns the mutable scheduler/store lifecycle.  This module
//! owns only the bounded public snapshot schema and the pure projection from
//! one already-read durable instance.  Keeping that boundary explicit prevents
//! browser polling from becoming scheduler state progression.

use super::json_wire::{bool_json, json_escape};
use super::scheduler::SchedulerInstanceState;
use super::scheduler_config::ScheduledInstanceConfig;
use super::scheduler_runtime::local_calendar;
use std::time::Instant;

pub(crate) const MAX_SCHEDULER_STATUS_RESPONSE_BYTES: usize = 128 * 1024;
pub(crate) const MAX_SCHEDULER_STATUS_ENTRIES: usize = 128;
pub(crate) const SCHEDULER_STATUS_GLOBAL_ERROR: &str =
    "Native scheduler status refresh failed; showing the last complete snapshot.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeSchedulerStatusRows {
    pub(crate) observed_at: u64,
    pub(crate) observed_monotonic: Instant,
    pub(crate) next_scheduler_wake_at: Option<u64>,
    pub(crate) instances: Vec<String>,
    pub(crate) issues: Vec<String>,
}

pub(crate) fn native_scheduler_instance_wake_at(
    config: &ScheduledInstanceConfig,
    persisted: Option<&SchedulerInstanceState>,
    now_unix_s: u64,
) -> Option<u64> {
    if !config.enabled() {
        return None;
    }
    let persisted = persisted?;
    if persisted.budget.reservation.is_some() || persisted.budget.accounting_blocked {
        return None;
    }
    let calendar =
        local_calendar(now_unix_s, config.window_start_hour, config.window_end_hour).ok()?;
    let due = persisted.cursor.due_unix_s(config.interval_s).ok()?;
    if !calendar.window_open {
        calendar.next_window_open_unix_s
    } else if due > now_unix_s {
        Some(due)
    } else {
        // A due slot waits for new runtime/config evidence rather than a
        // synthetic retry clock.
        None
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn native_scheduler_status_response(
    config: &ScheduledInstanceConfig,
    persisted: Option<&SchedulerInstanceState>,
    scheduler_error: Option<&str>,
    scheduler_warning: Option<&str>,
    waiting: Option<&str>,
    now_unix_s: u64,
    day: &str,
    month: &str,
) -> Result<String, String> {
    let enabled = config.enabled();
    let Some(persisted) = persisted else {
        let (state, message, budget_authoritative, daily_remaining, monthly_remaining) = if enabled
        {
            (
                "initializing",
                "Native scheduler durable state is not initialized yet.",
                false,
                0,
                0,
            )
        } else {
            (
                "disabled",
                "Scheduled Auto-Tune is disabled for this instance.",
                true,
                config.daily_limit_bytes,
                config.monthly_limit_bytes,
            )
        };
        return Ok(format_native_scheduler_status(
            config,
            enabled,
            false,
            budget_authoritative,
            state,
            message,
            scheduler_warning,
            now_unix_s,
            0,
            0,
            0,
            config.daily_limit_bytes,
            0,
            0,
            daily_remaining,
            config.monthly_limit_bytes,
            0,
            0,
            monthly_remaining,
            false,
        ));
    };

    let mut current = persisted.clone();
    current.validate()?;
    current.budget.reconfigure_limits(
        day,
        month,
        config.daily_limit_bytes,
        config.monthly_limit_bytes,
    )?;
    let next_due_at = current.cursor.due_unix_s(config.interval_s)?;
    let last_success_at = current.cursor.last_success_unix_s.unwrap_or(0);
    let last_failure_due_at = current
        .cursor
        .failed_attempt
        .as_ref()
        .map(|failure| failure.due_unix_s)
        .unwrap_or(0);
    let daily_reserved = current
        .budget
        .reservation
        .as_ref()
        .filter(|reservation| reservation.day == current.budget.day)
        .map(|reservation| reservation.reserved_bytes)
        .unwrap_or(0);
    let monthly_reserved = current
        .budget
        .reservation
        .as_ref()
        .filter(|reservation| reservation.month == current.budget.month)
        .map(|reservation| reservation.reserved_bytes)
        .unwrap_or(0);
    let daily_used = current
        .budget
        .daily_charged_bytes
        .checked_sub(daily_reserved)
        .ok_or_else(|| "daily scheduler reservation exceeds its charged total".to_string())?;
    let monthly_used = current
        .budget
        .monthly_charged_bytes
        .checked_sub(monthly_reserved)
        .ok_or_else(|| "monthly scheduler reservation exceeds its charged total".to_string())?;
    let daily_remaining = current
        .budget
        .daily_limit_bytes
        .saturating_sub(current.budget.daily_charged_bytes);
    let monthly_remaining = current
        .budget
        .monthly_limit_bytes
        .saturating_sub(current.budget.monthly_charged_bytes);
    let accounting_error = current.budget.accounting_blocked;
    let (state, message) = if accounting_error {
        (
            "blocked",
            "Native scheduler traffic accounting is blocked pending exact recovery or operator acknowledgement.",
        )
    } else if !enabled {
        (
            "disabled",
            "Scheduled Auto-Tune is disabled for this instance.",
        )
    } else if scheduler_error.is_some() {
        (
            "error",
            "Native scheduler reported an instance-local error; inspect system logs for details.",
        )
    } else if current.budget.reservation.is_some() {
        (
            "running",
            "Scheduled calibration is active or awaiting durable settlement.",
        )
    } else if let Some(reason) = waiting {
        ("deferred", scheduler_waiting_message(reason))
    } else {
        ("idle", "Native scheduler is ready.")
    };

    Ok(format_native_scheduler_status(
        config,
        enabled,
        true,
        !accounting_error,
        state,
        message,
        scheduler_warning,
        now_unix_s,
        last_success_at,
        next_due_at,
        last_failure_due_at,
        current.budget.daily_limit_bytes,
        daily_used,
        daily_reserved,
        daily_remaining,
        current.budget.monthly_limit_bytes,
        monthly_used,
        monthly_reserved,
        monthly_remaining,
        accounting_error,
    ))
}

#[allow(clippy::too_many_arguments)]
fn format_native_scheduler_status(
    config: &ScheduledInstanceConfig,
    enabled: bool,
    initialized: bool,
    budget_authoritative: bool,
    state: &str,
    message: &str,
    warning: Option<&str>,
    observed_at: u64,
    updated_at: u64,
    next_due_at: u64,
    last_failure_due_at: u64,
    daily_limit: u64,
    daily_used: u64,
    daily_reserved: u64,
    daily_remaining: u64,
    monthly_limit: u64,
    monthly_used: u64,
    monthly_reserved: u64,
    monthly_remaining: u64,
    accounting_error: bool,
) -> String {
    let warning = warning
        .map(|warning| format!("\"{}\"", json_escape(warning)))
        .unwrap_or_else(|| "null".to_string());
    format!(
        concat!(
            "{{\"instance\":\"{}\",\"enabled\":{},\"initialized\":{},",
            "\"budget_authoritative\":{},\"state\":\"{}\",",
            "\"message\":\"{}\",\"warning\":{},\"observed_at\":{},\"updated_at\":{},",
            "\"next_due_at\":{},\"window\":{{\"start_hour\":{},\"end_hour\":{}}},",
            "\"daily\":{{\"limit_bytes\":{},\"used_bytes\":{},",
            "\"reserved_bytes\":{},\"remaining_bytes\":{}}},",
            "\"monthly\":{{\"limit_bytes\":{},\"used_bytes\":{},",
            "\"reserved_bytes\":{},\"remaining_bytes\":{}}},",
            "\"accounting_error\":{},\"last_success_at\":{},",
            "\"last_failure_due_at\":{}}}"
        ),
        json_escape(&config.instance),
        bool_json(enabled),
        bool_json(initialized),
        bool_json(budget_authoritative),
        json_escape(state),
        json_escape(message),
        warning,
        observed_at,
        updated_at,
        next_due_at,
        config.window_start_hour,
        config.window_end_hour,
        daily_limit,
        daily_used,
        daily_reserved,
        daily_remaining,
        monthly_limit,
        monthly_used,
        monthly_reserved,
        monthly_remaining,
        bool_json(accounting_error),
        updated_at,
        last_failure_due_at,
    )
}

pub(crate) fn format_native_scheduler_issue(
    instance: &str,
    message: &str,
    observed_at: u64,
) -> String {
    format!(
        concat!(
            "{{\"instance\":\"{}\",\"enabled\":false,\"initialized\":false,",
            "\"budget_authoritative\":false,\"state\":\"error\",",
            "\"message\":\"{}\",\"warning\":null,\"observed_at\":{},\"updated_at\":0,",
            "\"next_due_at\":0,\"window\":{{\"start_hour\":0,\"end_hour\":0}},",
            "\"daily\":{{\"limit_bytes\":0,\"used_bytes\":0,",
            "\"reserved_bytes\":0,\"remaining_bytes\":0}},",
            "\"monthly\":{{\"limit_bytes\":0,\"used_bytes\":0,",
            "\"reserved_bytes\":0,\"remaining_bytes\":0}},",
            "\"accounting_error\":false,\"last_success_at\":0,",
            "\"last_failure_due_at\":0}}"
        ),
        json_escape(instance),
        json_escape(message),
        observed_at,
    )
}

pub(crate) fn format_native_scheduler_batch(
    rows: &NativeSchedulerStatusRows,
    stale: bool,
    global_error: Option<&str>,
) -> Result<String, String> {
    let global_error = global_error
        .map(|message| format!("\"{}\"", json_escape(message)))
        .unwrap_or_else(|| "null".to_string());
    let response = format!(
        "{{\"schema_version\":1,\"owner\":\"native\",\"available\":true,\"observed_at\":{},\"stale\":{},\"global_error\":{},\"instances\":[{}],\"issues\":[{}]}}\n",
        rows.observed_at,
        bool_json(stale),
        global_error,
        rows.instances.join(","),
        rows.issues.join(","),
    );
    if response.len() > MAX_SCHEDULER_STATUS_RESPONSE_BYTES {
        return Err("native scheduler status exceeds its response bound".to_string());
    }
    Ok(response)
}

pub(crate) fn sanitize_scheduler_public_message(message: &str) -> String {
    const MAX_PUBLIC_MESSAGE_CHARS: usize = 256;
    let mut sanitized = String::new();
    for character in message.chars().take(MAX_PUBLIC_MESSAGE_CHARS) {
        sanitized.push(if character.is_control() {
            ' '
        } else {
            character
        });
    }
    if message.chars().count() > MAX_PUBLIC_MESSAGE_CHARS {
        sanitized.push_str("...");
    }
    if sanitized.is_empty() {
        "Native scheduler configuration is invalid for this instance.".to_string()
    } else {
        sanitized
    }
}

fn scheduler_waiting_message(reason: &str) -> &'static str {
    match reason {
        "accounting-unavailable" => "Traffic accounting is unavailable.",
        "terminal-settlement-pending" => "The previous scheduled run is awaiting settlement.",
        "not-due" => "The next scheduled calibration is not due yet.",
        "window-closed" => "Waiting for the configured calibration window.",
        "configuration-pending" => "UCI configuration has uncommitted changes.",
        "recovery-pending" => "Coordinator recovery must complete first.",
        "manual-job-priority" => "A user-requested calibration has priority.",
        "coordinator-busy" => "The calibration coordinator is busy.",
        "route-not-ready" => "The selected uplink route is not ready.",
        "runtime-not-ready" => "Instance runtime state is not ready.",
        "quiet-window-pending" => "Waiting for a sufficiently quiet traffic window.",
        "scheduled-job-active" => "A scheduled calibration is already active.",
        "budget-exhausted" => "The scheduled traffic budget is exhausted.",
        "budget-insufficient" => "The remaining traffic budget is too small for calibration.",
        _ => "Native scheduler is waiting for current admission conditions.",
    }
}
