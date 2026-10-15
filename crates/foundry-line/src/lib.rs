//! foundry-line — the line executor (plan Task 7, v3 §10).
//! Owns the Order loop: transition → atomic (state+ledger) commit → Die run.
//! watch is the single writer (critique #4); attempt numbers fence stale
//! Dies (#3); audit appends share the SQLite transaction with the state
//! write (#5); operation_id idempotency guards publishing (#2).
pub mod error;
pub mod line;
pub mod rehydrate;
pub mod review_host;

pub use error::LineError;
pub use line::{Line, LineConfig, backoff_delay};
pub use review_host::{InspectingReviewRun, BinaryProvenance, PackReviewHost, StageExecutor, StageOutput, StageTask, SqliteReviewLog, SqliteReviewSink, SqliteIntentLog, UnavailableStageExecutor, durable_review_run};

/// (Notifier v2.7) publish 결과 영수증 — 상태 + PR URL(Unknown이면 None).
#[derive(Debug, Clone)]
pub struct PublishReceipt {
    pub state: foundry_domain::OrderState,
    pub pr_url: Option<String>,
}
