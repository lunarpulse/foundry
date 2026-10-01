//! foundry-line — the line executor (plan Task 7, v3 §10).
//! Owns the Order loop: transition → atomic (state+ledger) commit → Die run.
//! watch is the single writer (critique #4); attempt numbers fence stale
//! Dies (#3); audit appends share the SQLite transaction with the state
//! write (#5); operation_id idempotency guards publishing (#2).
pub mod error;
pub mod line;

pub use error::LineError;
pub use line::{backoff_delay, Line, LineConfig};
