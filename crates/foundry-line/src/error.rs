//! LineError — executor failures.
use foundry_domain::ports::PortError;

#[derive(Debug, thiserror::Error)]
pub enum LineError {
    #[error("illegal transition on {0}: {1}")]
    IllegalTransition(String, foundry_domain::TransitionError),
    #[error("order {0} not found")]
    UnknownOrder(String),
    #[error("policy denied on {0}: {1}")]
    Policy(String, String),
    #[error("lease held by {0} — another writer is active")]
    LeaseHeld(String),
    #[error("port failure on {0}: {1:?}")]
    Port(String, PortError),
}
