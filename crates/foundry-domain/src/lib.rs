//! foundry-domain — pure types + traits. No I/O. (plan §6)
pub mod order;
pub mod ports;

pub use order::{ExternalRef, Order, OrderEvent, OrderState, TransitionError};
pub use ports::{ActorId, ActorKind, ApprovalToken, Decision, DieOutput, PortError, PortResult, PublishOutcome};
