//! foundry-domain — pure types + traits. No I/O. (plan §6)
pub mod casting;
pub mod inspector;
pub mod order;
pub mod ports;
pub mod workdir;

pub use order::{ExternalRef, Order, OrderEvent, OrderState, TransitionError};
pub use ports::{
    ActorId, ActorKind, ApprovalToken, Decision, DieOutput, PortError, PortResult, PublishOutcome,
};
pub use workdir::{WORKDIR_ROOT, repo_path, workdir_path};
