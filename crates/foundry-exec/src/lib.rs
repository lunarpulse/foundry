//! Durable execution graph core types. Storage and execution adapters live in later batches.

pub mod budget;
pub mod attempt_budget;
pub mod emit;
pub mod graph;
pub mod host;
pub mod hold;
pub mod inject;
pub mod lifecycle;
pub mod ports;
pub mod review_run;
pub mod tuple_sqlite;
pub mod types;

pub use budget::{Budget, ReservationOutcome};
pub use emit::{EmitLog, EmitOutcome, EmitSink, complete_after_w4, emit_once};
pub use graph::{GraphDriver, GraphStep};
pub use host::{DieHostAdapter, DieWorker, HostError, HostKey, HostLedger, HostResult};
pub use attempt_budget::AttemptBudget;
pub use hold::{HoldApplied, HoldCasOutcome, HoldCommand, HoldInstanceId, HoldOperation, HoldState, HoldStatus, apply_hold, cas_hold};
pub use inject::{CrashPoint, FaultInjector};
pub use lifecycle::{DispatchResult, Executor, LifecycleError, ResumeAction, RunStep};
pub use ports::{AttemptLedger, CounterLog, CounterRecord, DispatchHost, EpochGate, HoldStore, IntentLog, OutcomeStore, PortError, Recorded, TupleStore};
pub use review_run::{DurableReviewRun, ReviewEvidence, ReviewRunError, ReviewRunPort, ReviewRunRequest, ReviewVerdict};
pub use tuple_sqlite::SqliteTupleStore;
pub use types::{
    ActivationRef, ArtifactLocation, ArtifactTuple, ArtifactTupleKey, Attempt, EmitReceipt, Epoch,
    ExecutionIntent, ExecutionOutcome, FenceVerdict, FencingToken, Generation, ImmutableCommit,
    OrderId, PromotionReceipt, Receipt, RunId, SessionReceipt, fence,
};
