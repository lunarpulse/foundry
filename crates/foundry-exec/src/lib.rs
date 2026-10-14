//! Durable execution graph core types. Storage and execution adapters live in later batches.

pub mod budget;
pub mod hold;
pub mod inject;
pub mod lifecycle;
pub mod ports;
pub mod types;

pub use budget::{Budget, ReservationOutcome};
pub use hold::{HoldCasOutcome, HoldCommand, HoldInstanceId, HoldOperation, cas_hold};
pub use inject::{CrashPoint, FaultInjector};
pub use lifecycle::{DispatchResult, Executor, LifecycleError, ResumeAction, RunStep};
pub use ports::{DispatchHost, IntentLog, OutcomeStore};
pub use types::{
    ActivationRef, ArtifactLocation, ArtifactTuple, ArtifactTupleKey, Attempt, EmitReceipt, Epoch,
    ExecutionIntent, ExecutionOutcome, FenceVerdict, FencingToken, Generation, ImmutableCommit,
    OrderId, PromotionReceipt, Receipt, RunId, SessionReceipt, fence,
};
