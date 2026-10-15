use crate::{
    hold::{HoldApplied, HoldCasOutcome, HoldCommand, HoldInstanceId, HoldState},
    types::{ArtifactTuple, ArtifactTupleKey, Attempt, EmitReceipt, Epoch, ExecutionIntent, ExecutionOutcome, OrderId, RunId},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortError {
    HoldConflict(HoldCasOutcome),
    EvidenceRequired,
    ResumeDestinationRequired,
    AlreadyResolved,
    MissingHold,
    AttemptBudgetExhausted { max_attempts: u32 },
    Store(String),
    Permanent(String),
    MissingEmitReceipt,
    StaleEpoch { active: Epoch, presented: Epoch },
}

/// Durable human hold state. Implementations serialize operations per instance.
/// `apply` must atomically compare the full instance id, bind command evidence,
/// and persist the transition. It rejects blank approve/reject evidence after
/// trimming, preserves `human_resolution` verbatim for approve/reject only,
/// and treats an identical resolved command as idempotent. A different second
/// resolution fails. Amend changes only a Waiting row's resume destination;
/// Info returns without changing state. Implementations should persist enough
/// command identity in `HoldState::resolution` to distinguish an exact retry
/// from a different command that produces the same `HoldApplied` value.
/// Amend takes its destination from `HoldCommand::resume_to`, never evidence.
pub trait HoldStore: Send + Sync {
    /// Records a waiting hold; returns false when it already exists.
    fn open(&self, instance: &HoldInstanceId, resume_to: &str) -> Result<bool, PortError>;
    /// Applies one hold operation through the shared compare-and-swap boundary.
    /// Check expected vs cmd.instance before lookup, including for missing rows.
    fn apply(&self, cmd: &HoldCommand, expected: &HoldInstanceId) -> Result<HoldApplied, PortError>;
    fn get(&self, instance: &HoldInstanceId) -> Result<Option<HoldState>, PortError>;
}

/// One-time consumption ledger for non-refundable execution attempts.
/// Count unique `(order, attempt, run)` triples, with a maximum per
/// `(order, attempt)` pair. Exact retries succeed even when that pair is full.
/// No refund or removal operation is permitted. `consumed` sums all pairs for
/// the order; adapters serialize admission and persist it before returning Ok.
pub trait AttemptLedger: Send + Sync {
    fn consume(&self, order: &OrderId, attempt: Attempt, run: RunId) -> Result<(), PortError>;
    fn consumed(&self, order: &OrderId) -> Result<u32, PortError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recorded;

/// Durable tuple storage boundary. Serialize read/modify/write across all
/// handles/processes. Ok means durable atomic persistence of complete tuples
/// (file adapters sync the temporary, rename it, then sync the directory).
/// Interrupted writes expose the old or new complete state, never a partial row.
/// Only NotFound means absence; corruption and read failures return Store.
/// Consumers receive tuples through this port, never storage paths or scans.
///
/// ```compile_fail
/// use foundry_exec::TupleStore;
/// fn scan(store: &dyn TupleStore) { store.scan_path("artifacts"); }
/// ```
/// ```compile_fail
/// use foundry_exec::{ArtifactTupleKey, TupleStore};
/// fn no_receipt(store: &dyn TupleStore, key: &ArtifactTupleKey) { store.promote(key); }
/// ```
pub trait TupleStore: Send + Sync {
    /// An identical key/value is idempotent. Any changed field under the same
    /// key returns Permanent and leaves the stored tuple unchanged.
    fn record(&self, tuple: &ArtifactTuple) -> Result<Recorded, PortError>;
    /// Greatest (run, generation) in this order/attempt, or proven absence.
    fn latest(&self, order: &OrderId, attempt: Attempt) -> Result<Option<ArtifactTuple>, PortError>;
    /// Greatest tuple whose emit receipt has been durably promoted (S6).
    /// Record-only tuples are never eligible for consumption.
    fn latest_promoted(&self, order: &OrderId, attempt: Attempt) -> Result<Option<ArtifactTuple>, PortError>;
    /// Requires a recorded tuple and a matching receipt from the emit owner.
    /// Missing tuple or mismatched key returns MissingEmitReceipt. Exact receipt
    /// retries are idempotent; a different receipt is Permanent, without writes.
    /// This records S6 promotion authorization after emit, not S5 ref mutation.
    fn promote(&self, key: &ArtifactTupleKey, receipt: &EmitReceipt) -> Result<(), PortError>;
}

/// Order-scoped fencing boundary for storage owners. The adapter serializes
/// epoch admission AND the entire write callback across handles/processes.
/// Reject presented < active with StaleEpoch without invoking the callback.
/// Otherwise durably set active = max(active, presented) BEFORE the callback;
/// never roll it back, including on write failure. Do not release the ownership
/// lock until the callback returns. A check followed by an unguarded write is
/// insufficient. Actual sibling/ref/session owners must route every write here.
/// Receipt-only retries bypass this gate because they do not repeat an effect.
pub trait EpochGate: Send + Sync {
    fn with_epoch(
        &self,
        order: &OrderId,
        presented: Epoch,
        write: &mut dyn FnMut() -> Result<(), PortError>,
    ) -> Result<(), PortError>;
}

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
