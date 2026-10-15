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
    /// the host accepted work: once set, recovery waits rather than redispatches.
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
    fn dispatch_count(&self) -> u64;

    /// Whether the host durably recorded receipt of this exact activation.
    /// Legacy hosts without receipt support report false.
    fn accepted(&self, _intent: &ExecutionIntent) -> bool {
        false
    }

    /// Reads a durably saved result after restart, without starting external work
    /// or consuming the receipt. `None` means no result is available yet, never
    /// permission to redispatch. Read failures must stop execution, not return None.
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
