use crate::types::{Attempt, ExecutionIntent, ExecutionOutcome, OrderId, RunId};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CounterRecord {
    /// Begins a visit before any effects. Host nodes retain the exact activation
    /// until Completed is durable, including after outcome binding succeeds.
    Started { node_id: String, activation: Option<u64> },
    /// A graph node bumped a counter (absolute value after the bump).
    Bump { node_id: String, counter: String, value: u64 },
    /// A node finished. `goto` is the next cursor, or `None` for graph completion.
    Completed { node_id: String, goto: Option<String> },
}

/// Durable write-ahead log for graph counter updates.
pub trait CounterLog {
    /// Appends an append-only fact. Ok means durable persistence. Implementations
    /// must fail-stop on ambiguous/partial writes; Err must mean nothing appended.
    fn append(&mut self, order: &str, run: u64, record: CounterRecord) -> Result<(), String>;
    /// Returns only this (order, run), in append order. Read failures must stop
    /// execution; an empty vector means no records. Serialize writers per run.
    fn load(&self, order: &str, run: u64) -> Vec<CounterRecord>;
}

/// Persistence boundary for execution intents. Implementations own all storage writes.
/// A successful return must mean durable persistence; storage failures must stop
/// execution (these infallible ports require fail-stop adapters). The owner must
/// serialize executors for a run, including across processes.
pub trait IntentLog {
    /// Records an immutable identity `(order, attempt, run, activation)`.
    fn append(&mut self, intent: &ExecutionIntent);
    /// Durably claims dispatch before the host call. This marker does not prove
    /// the host accepted work. Recovery waits unless the host proves no receipt
    /// exists for this activation.
    fn mark_started(&mut self, key: &(OrderId, Attempt, RunId), activation: u64);
    fn latest(&self, key: &(OrderId, Attempt, RunId)) -> Option<ExecutionIntent>;
    /// Reads a specific activation, retaining history for delayed retries.
    fn get(&self, key: &(OrderId, Attempt, RunId), activation: u64) -> Option<ExecutionIntent>;

    /// Reads the durable dispatch claim for this specific activation.
    fn is_started(&self, key: &(OrderId, Attempt, RunId), activation: u64) -> bool;
}

/// External execution boundary. Implementations may adapt a real host or a test double.
pub trait DispatchHost {
    type Result;

    /// The adapter must durably save the complete result under the intent's
    /// activation identity before returning it. External work and session writes
    /// remain owned by this adapter.
    fn dispatch(&mut self, intent: &ExecutionIntent) -> Self::Result;

    /// Fallible dispatch boundary used by Executor. Legacy infallible hosts keep
    /// their fail-stop contract; fallible adapters must override this method.
    fn try_dispatch(&mut self, intent: &ExecutionIntent) -> Result<Self::Result, crate::host::HostError> {
        Ok(self.dispatch(intent))
    }

    fn dispatch_count(&self) -> u64;

    /// Whether this exact activation was, or may have been, accepted. `false`
    /// must prove durable receipt absence under the exclusive execution owner.
    /// Unsupported receipts and ambiguous reads must never return false: legacy
    /// hosts conservatively wait. Read errors must fail-stop, not imply absence.
    fn accepted(&self, _intent: &ExecutionIntent) -> bool {
        true
    }

    /// Reads a durably saved result after restart, without starting external work
    /// or consuming the receipt. `None` alone does not permit redispatch; only
    /// `accepted(intent) == false` proves it safe for started work. Read failures
    /// must stop execution, not return None.
    fn recover_result(&mut self, intent: &ExecutionIntent) -> Option<Self::Result>;
}

/// Atomic persistence boundary for an outcome and its continuation data.
/// Commit must durably bind result receipts, cursor and next activation together,
/// retain outcomes by activation, and leave an existing binding unchanged on repeats.
/// Failures must stop execution; a partial write cannot be reported as success.
pub trait OutcomeStore {
    fn commit(&mut self, outcome: &ExecutionOutcome);
    fn latest(&self, key: &(OrderId, Attempt, RunId)) -> Option<ExecutionOutcome>;
    fn get(&self, key: &(OrderId, Attempt, RunId), activation: u64) -> Option<ExecutionOutcome>;
}
