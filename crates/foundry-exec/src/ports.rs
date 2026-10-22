use crate::{
    hold::{HoldApplied, HoldCasOutcome, HoldCommand, HoldInstanceId, HoldState},
    types::{
        ArtifactTuple, ArtifactTupleKey, Attempt, EmitReceipt, Epoch, ExecutionIntent,
        ExecutionOutcome, OrderId, RunId,
    },
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
    fn apply(&self, cmd: &HoldCommand, expected: &HoldInstanceId)
    -> Result<HoldApplied, PortError>;
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
    fn latest(&self, order: &OrderId, attempt: Attempt)
    -> Result<Option<ArtifactTuple>, PortError>;
    /// Greatest tuple whose emit receipt has been durably promoted (S6).
    /// Record-only tuples are never eligible for consumption.
    fn latest_promoted(
        &self,
        order: &OrderId,
        attempt: Attempt,
    ) -> Result<Option<ArtifactTuple>, PortError>;
    /// Requires a recorded tuple and a matching receipt from the emit owner.
    /// Missing tuple or mismatched key returns MissingEmitReceipt. Exact receipt
    /// retries are idempotent; a different receipt is Permanent, without writes.
    /// This records S6 promotion authorization after emit, not S5 ref mutation.
    fn promote(&self, key: &ArtifactTupleKey, receipt: &EmitReceipt) -> Result<(), PortError>;
}

/// Owns the single durable transaction that admits a review activation and
/// copies any legally inherited result. Implementations must not expose their
/// connection or split this operation across commits.
pub trait ReviewAdmissionStore: Send {
    fn validate_product_work_claim(
        &self,
        claim: &crate::review_run::ReviewWorkClaim,
        intent: &crate::types::ExecutionIntent,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), crate::host::HostError>;
    fn admit_review_activation(
        &mut self,
        request: crate::review_run::ReviewAdmissionRequest,
    ) -> Result<crate::review_run::ReviewAdmission, crate::host::HostError>;
}

/// Typed persistence boundary used by the existing staged review algorithm.
/// Claims are durable before external execution; completion atomically stores
/// the normalized output and its session receipt under the same active run.
pub trait ReviewDispatchStore: Send {
    fn load_review_result(
        &self,
        key: &crate::host::HostKey,
    ) -> Result<Option<crate::host::HostResult>, crate::host::HostError>;
    fn checkpoint_review_counters(
        &mut self,
        request: crate::review_run::ReviewCounterCheckpoint,
    ) -> Result<(), crate::host::HostError>;
    fn start_review_stage(
        &mut self,
        request: crate::review_run::ReviewStageStart,
    ) -> Result<(), crate::host::HostError>;
    fn finish_review_stage(
        &mut self,
        request: crate::review_run::ReviewStageFinishRequest,
    ) -> Result<(), crate::host::HostError>;
    fn fail_review_dispatch(
        &mut self,
        request: crate::review_run::ReviewDispatchFailure,
    ) -> Result<(), crate::host::HostError>;
    fn complete_review_dispatch(
        &mut self,
        request: crate::review_run::ReviewDispatchCompletion,
    ) -> Result<(), crate::host::HostError>;
}

/// Complete persistence contract used by the existing frozen-pack review
/// host. A backend owns all serialization and transactional writes; this port
/// contains no SQLite handle or SQL callback. Every operation is required so
/// an alternate backend cannot compile while silently omitting recovery or
/// continuation behavior.
///
/// `run_root` and `claim_resume` concern explicitly configured execution
/// artifacts/OS claims, not the database path. Product implementations must
/// keep those roots separate from their private database.
pub trait ReviewHostStore: ReviewAdmissionStore + ReviewDispatchStore + Send {
    fn pack_digest(&self) -> &str;
    fn run_root(&self, run_id: &str) -> Result<std::path::PathBuf, crate::host::HostError>;
    fn claim_resume(&self, run_id: &str) -> Result<Option<std::fs::File>, crate::host::HostError>;
    fn resolved_attempt(&self, order: &str, attempt: u64) -> Result<bool, crate::host::HostError>;
    fn session_receipt(
        &self,
        order: &str,
        session_id: &str,
    ) -> Result<Option<crate::review_run::ReviewSessionReceipt>, crate::host::HostError>;
    fn invalidate_terminal_runs(&mut self, order: &str) -> Result<(), crate::host::HostError>;
    fn redrive_payload(
        &self,
        order: &str,
        run: &str,
        payload: &str,
    ) -> Result<Vec<crate::review_run::ReviewRedriveArtifact>, crate::host::HostError>;
    fn charge_budget(
        &mut self,
        order: &str,
        intent: &crate::types::ExecutionIntent,
    ) -> Result<(), crate::host::HostError>;
    fn accepted(
        &self,
        intent: &crate::types::ExecutionIntent,
    ) -> Result<bool, crate::host::HostError>;
    fn latest_run(
        &self,
        order: &str,
        attempt: u64,
    ) -> Result<Option<crate::review_run::RunRecord>, crate::host::HostError>;
    fn mark_hold_expired(
        &mut self,
        request: crate::review_run::ReviewHoldUpdate,
    ) -> Result<(), crate::host::HostError>;
    fn begin_hold_resume(
        &mut self,
        request: crate::review_run::ReviewHoldUpdate,
    ) -> Result<(), crate::host::HostError>;
    fn restore_hold_after_resume_error(
        &mut self,
        binding: crate::review_run::ReviewRunBinding,
    ) -> Result<(), crate::host::HostError>;
    fn resolve_hold_result(
        &mut self,
        request: crate::review_run::ReviewHoldResolution,
    ) -> Result<(), crate::host::HostError>;
    fn save_redrive_output(
        &mut self,
        order: &str,
        intent: &crate::types::ExecutionIntent,
        run: &str,
        sequence: u64,
        stage_id: &str,
        session_key: &str,
        session_id: &str,
        output: &crate::review_run::ReviewStageEvidence,
    ) -> Result<(), crate::host::HostError>;
    fn continuation_binding(
        &self,
        order: &str,
        attempt: i64,
        run: &str,
    ) -> Result<crate::review_run::ReviewContinuationBinding, crate::host::HostError>;
    fn continuation_counters(
        &mut self,
        request: crate::review_run::ReviewCounterCheckpoint,
        pack: &str,
    ) -> Result<(), crate::host::HostError>;
    fn continuation_session(
        &self,
        order: &str,
        session_id: &str,
    ) -> Result<Option<crate::review_run::ReviewSessionReceipt>, crate::host::HostError>;
    fn continuation_budget_and_start(
        &mut self,
        request: crate::review_run::ReviewStageStart,
        pack: &str,
    ) -> Result<(), crate::host::HostError>;
    fn finish_continuation_stage(
        &mut self,
        request: crate::review_run::ReviewStageFinishRequest,
        pack: &str,
    ) -> Result<(), crate::host::HostError>;
    fn complete_continuation(
        &mut self,
        request: crate::review_run::ReviewContinuationCompletion,
    ) -> Result<(), crate::host::HostError>;
    fn latest_hold(
        &self,
        order: &str,
    ) -> Result<Option<crate::review_run::ReviewHoldRecord>, crate::host::HostError>;
    fn run_result(
        &self,
        order: &str,
        attempt: i64,
        run: &str,
    ) -> Result<crate::host::HostResult, crate::host::HostError>;
    fn binding_intent(
        &self,
        intent: &crate::types::ExecutionIntent,
    ) -> Result<Option<crate::types::ExecutionIntent>, crate::host::HostError>;
    /// Atomically validates current product authority, loads the immutable
    /// accepted decision, and claims its durable continuation progress.
    fn claim_accepted_hold(
        &mut self,
        session_digest: &str,
        operation_id: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<crate::review_run::ReviewAcceptedHold, crate::host::HostError>;
    fn finalize_accepted_hold(
        &mut self,
        claim: crate::review_run::ReviewAcceptedHold,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), crate::host::HostError>;
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
    Started {
        node_id: String,
        activation: Option<u64>,
    },
    /// A graph node bumped a counter (absolute value after the bump).
    Bump {
        node_id: String,
        counter: String,
        value: u64,
    },
    /// A node finished. `goto` is the next cursor, or `None` for graph completion.
    Completed {
        node_id: String,
        goto: Option<String>,
    },
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
    fn try_dispatch(
        &mut self,
        intent: &ExecutionIntent,
    ) -> Result<Self::Result, crate::host::HostError> {
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

    /// Reads the greatest durable review run for an order attempt.
    fn latest_run(
        &self,
        _order: &str,
        _attempt: u64,
    ) -> Result<Option<crate::review_run::RunRecord>, crate::host::HostError> {
        Ok(None)
    }

    fn resolve_hold(
        &mut self,
        _order: &str,
        _hold_id: &str,
        _action: crate::review_run::ReviewHoldAction,
        _actor: &str,
    ) -> Result<(), crate::host::HostError> {
        Err(crate::host::HostError::Worker(
            "review hold resolution is unsupported".into(),
        ))
    }
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
