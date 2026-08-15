use super::protocol::OperationState;

/// Validate coordinator-owned job transitions before a new snapshot is
/// published.  Recovery is deliberately reachable from every state which may
/// already own runtime resources; terminal states never become live again.
pub fn validate_transition(from: OperationState, to: OperationState) -> Result<(), String> {
    use OperationState::*;

    let allowed = match from {
        Queued => matches!(to, Starting | Cancelling | Failed | Cancelled),
        Starting => matches!(to, Running | Cancelling | Recovering | Failed | Cancelled),
        Running => matches!(
            to,
            Cancelling | Recovering | ReviewReady | Completed | Failed | Cancelled
        ),
        Cancelling => matches!(to, Recovering | Failed | Cancelled),
        Recovering => matches!(to, ReviewReady | Completed | Failed | Cancelled),
        ReviewReady | Completed | Failed | Cancelled => false,
    };

    if allowed {
        Ok(())
    } else {
        Err(format!(
            "invalid calibration job transition: {from:?} -> {to:?}"
        ))
    }
}

#[cfg(test)]
pub fn owns_runtime(state: OperationState) -> bool {
    matches!(
        state,
        OperationState::Starting
            | OperationState::Running
            | OperationState::Cancelling
            | OperationState::Recovering
    )
}

pub fn terminal(state: OperationState) -> bool {
    matches!(
        state,
        OperationState::ReviewReady
            | OperationState::Completed
            | OperationState::Failed
            | OperationState::Cancelled
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_job_can_recover_but_terminal_job_cannot_restart() {
        assert!(validate_transition(OperationState::Queued, OperationState::Starting).is_ok());
        assert!(validate_transition(OperationState::Starting, OperationState::Running).is_ok());
        assert!(validate_transition(OperationState::Running, OperationState::Recovering).is_ok());
        assert!(validate_transition(OperationState::Recovering, OperationState::Completed).is_ok());
        assert!(validate_transition(OperationState::Completed, OperationState::Running).is_err());
        assert!(validate_transition(OperationState::Failed, OperationState::Starting).is_err());
    }

    #[test]
    fn cancellation_and_runtime_ownership_are_explicit() {
        assert!(validate_transition(OperationState::Queued, OperationState::Cancelled).is_ok());
        assert!(validate_transition(OperationState::Running, OperationState::Cancelling).is_ok());
        assert!(
            validate_transition(OperationState::Cancelling, OperationState::Recovering).is_ok()
        );
        assert!(validate_transition(OperationState::Cancelling, OperationState::Cancelled).is_ok());
        assert!(owns_runtime(OperationState::Cancelling));
        assert!(!owns_runtime(OperationState::Queued));
        assert!(terminal(OperationState::ReviewReady));
    }

    #[test]
    fn self_transitions_are_rejected_instead_of_hiding_duplicate_publication() {
        for state in [
            OperationState::Queued,
            OperationState::Starting,
            OperationState::Running,
            OperationState::Cancelling,
            OperationState::Recovering,
            OperationState::ReviewReady,
            OperationState::Completed,
            OperationState::Failed,
            OperationState::Cancelled,
        ] {
            assert!(validate_transition(state, state).is_err());
        }
    }
}
