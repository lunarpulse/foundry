#![allow(clippy::large_enum_variant)] // W6a clippy debt: preserve the existing public enum representation and matching behavior

use crate::{
    emit::{EmitLog, EmitOutcome, EmitSink, complete_after_w4, emit_once},
    host::{GenerationCommit, HoldState, HostError, HostResult},
    ports::{DispatchHost, IntentLog, PortError, TupleStore},
    types::{
        ArtifactLocation, ArtifactTuple, ArtifactTupleKey, Attempt, EmitReceipt, Epoch,
        ExecutionIntent, Generation, ImmutableCommit, OrderId, PromotionReceipt, RunId,
    },
};
use chrono::Utc;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRunRequest {
    pub order: OrderId,
    pub attempt: Attempt,
    pub run: RunId,
    pub base_commit: String,
    pub result_commit: String,
    pub artifact_digest: String,
    pub epoch: Epoch,
    pub activation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewVerdict {
    Passed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewEvidence {
    pub emit_receipt: EmitReceipt,
    pub tuple_key: ArtifactTupleKey,
    pub verdict: ReviewVerdict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewRunOutcome {
    Completed(ReviewEvidence),
    Held(HeldOutcome),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldOutcome {
    pub hold: HoldState,
    pub order: String,
    pub attempt: u64,
    pub run: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRecord {
    pub run: u64,
    pub base_commit: String,
    pub result_commit: String,
    pub status: String,
    /// Preserve the standalone API's original serialized field for callers.
    /// Product review code uses the typed `ReviewHoldRecord` port instead.
    pub hold_json: Option<String>,
    pub hold_instance_id: Option<String>,
}

/// Typed input to the host-owned activation-admission transaction. The store
/// validates the active epoch and predecessor and commits all admission rows
/// before returning. It never starts external execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewAdmissionRequest {
    pub intent: ExecutionIntent,
    pub base_commit: String,
    pub result_commit: String,
    pub pack_digest: String,
    /// Exact durable ProductAtomicV1 work ownership, when this activation is
    /// driven from the product backlog. Standalone hosts leave it absent.
    pub product_work: Option<ReviewWorkClaim>,
}

/// Product work claim coordinates passed to the host storage owner. This is
/// descriptive input, not authority by itself: ProductStore must match every
/// field against its current claim row inside the admission transaction.
#[derive(Clone, PartialEq, Eq)]
pub struct ReviewWorkClaim {
    pub order_id: String,
    pub lifecycle: i64,
    pub delivery_id: String,
    pub worker_id: String,
    pub epoch: i64,
    pub fence: i64,
    pub source_revision: i64,
    pub payload_digest: String,
    pub lease_until: chrono::DateTime<chrono::Utc>,
    pub task_kind: String,
    pub payload_snapshot: String,
    /// Present only for ProductAtomicV1 work claims. This is a binding copied
    /// from the claim row; ProductStore validates it on every new stage.
    pub service_binding: Option<ReviewServiceBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewServiceBinding {
    pub service_id: String,
    pub service_revision: i64,
    pub credential_revision: i64,
    pub source_instance_id: String,
}

impl std::fmt::Debug for ReviewWorkClaim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReviewWorkClaim")
            .field("order_id", &self.order_id)
            .field("lifecycle", &self.lifecycle)
            .field("delivery_id", &self.delivery_id)
            .field("worker_id", &self.worker_id)
            .field("epoch", &self.epoch)
            .field("fence", &self.fence)
            .field("source_revision", &self.source_revision)
            .field("lease_until", &self.lease_until)
            .field("task_kind", &self.task_kind)
            .field("payload_snapshot", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewAdmission {
    pub journal_run: String,
    pub inherited_counters: BTreeMap<String, u64>,
    pub inherited_result_json: Option<String>,
}

/// A stage claim is committed before an executor is called. The storage owner
/// checks the exact active intent/run and charges the run budget in the same
/// transaction as inserting the accepted stage row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewStageStart {
    pub intent: ExecutionIntent,
    pub journal_run: String,
    pub sequence: u64,
    pub stage_id: String,
    pub budget_limit: u64,
}

/// Normalized evidence returned by the existing review executor. This is a
/// bounded product of the host algorithm, not caller-authored success state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewStageEvidence {
    pub role: String,
    pub log_path: String,
    pub sha256: String,
    pub inspected_commit: Option<String>,
    pub findings: bool,
    pub verdicts: Vec<String>,
    pub input_manifest: Option<ReviewInputManifest>,
    pub deferred: bool,
    pub produced_commit: Option<String>,
    pub session: Option<ReviewSessionEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSessionEvidence {
    pub session_key: String,
    pub session_id: String,
    pub output: String,
    pub sha256: String,
}

/// Validated durable session receipt returned by a review storage owner.
/// Storage implementations serialize this value internally; host algorithms
/// never exchange JSON-encoded session fields with the persistence boundary.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewSessionReceipt {
    pub session_id: String,
    pub output: String,
    pub sha256: String,
}

/// Frozen copy of the existing harness input manifest used as review evidence.
/// Its fields intentionally match foundry_harness_cli::InputManifest.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewInputManifest {
    pub order_id: String,
    pub attempt: u32,
    pub task: String,
    pub profile: String,
    pub base: String,
    pub fix_context: bool,
    pub context: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewStageFinish {
    Complete(ReviewStageEvidence),
    Error(String),
    Blocked(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewStageFinishRequest {
    pub intent: ExecutionIntent,
    pub journal_run: String,
    pub sequence: u64,
    pub stage_id: String,
    pub finish: ReviewStageFinish,
}

/// Durable counter checkpoint produced by the existing graph engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewCounterCheckpoint {
    pub intent: ExecutionIntent,
    pub journal_run: String,
    pub counters: BTreeMap<String, u64>,
}

/// Immutable provenance captured before dispatch; stored with the terminal run.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewBinaryProvenance {
    pub executable: String,
    pub sha256: String,
    pub source_commit: String,
    pub pack_digest: String,
}

/// Engine snapshot and accumulated evidence needed to resume this exact hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewHeldSnapshot {
    pub hold: crate::host::HoldState,
    pub node_id: String,
    pub entry_counters: BTreeMap<String, u64>,
    pub requested_revert: Option<String>,
    pub engine_state: String,
    pub session_ids: Vec<String>,
    pub accepted_logs: Vec<String>,
    pub generations: Vec<crate::host::GenerationCommit>,
    pub latest_commit: String,
    pub sequence: u64,
}

/// Stable coordinates used by review-storage operations. The journal run is
/// supplied by the persistence owner and must be checked against its active
/// epoch record inside every mutation transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRunBinding {
    pub order_id: String,
    pub attempt: u64,
    pub journal_run: String,
}

/// Opaque revision token issued by a storage owner for one exact persisted
/// hold snapshot. It is a CAS precondition, never an authorization credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewHoldRevision([u8; 32]);
impl ReviewHoldRevision {
    pub fn from_storage_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewHumanAction {
    Resume {
        target: String,
    },
    CommitPass,
    CommitFail,
    Abort,
    DeadlineExpired,
    Other {
        name: String,
        target: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewHumanEvent {
    pub actor: String,
    pub occurred_at: String,
    pub action: ReviewHumanAction,
}

/// Typed continuation material decoded by a backend from its private durable
/// format. Backends retain responsibility for encoding the original evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewContinuationSnapshot {
    pub binding: ReviewRunBinding,
    pub snapshot_run_id: String,
    pub held: ReviewHeldSnapshot,
    pub intent: ExecutionIntent,
    pub resume_sequence: u64,
    pub human_events: Vec<ReviewHumanEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewHoldRecord {
    pub binding: ReviewRunBinding,
    pub status: ReviewHoldStatus,
    pub hold_id: String,
    pub deadline: i64,
    pub revision: ReviewHoldRevision,
    pub snapshot: ReviewContinuationSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewHoldStatus {
    Held,
    Resuming,
    Resolved,
    Failed,
    Aborted,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewHoldExpectation {
    pub binding: ReviewRunBinding,
    pub hold_id: String,
    pub deadline: i64,
    pub revision: ReviewHoldRevision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewHoldUpdate {
    pub expected: ReviewHoldExpectation,
    pub now: i64,
    pub snapshot: ReviewContinuationSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewHoldResolution {
    pub expected: ReviewHoldExpectation,
    pub now: i64,
    pub action: ReviewHoldAction,
    pub snapshot: ReviewContinuationSnapshot,
    pub result: HostResult,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewContinuationCompletion {
    pub binding: ReviewRunBinding,
    pub intent: ExecutionIntent,
    pub pack_digest: String,
    pub counters: BTreeMap<String, u64>,
    pub snapshot: ReviewContinuationSnapshot,
    pub result: HostResult,
    pub deadline: Option<i64>,
}

/// A hold decision that ProductAtomicV1 has already committed and revalidated
/// under the current authenticated session. The storage owner keeps its claim
/// fence private and checks it on every subsequent continuation write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewAcceptedHold {
    pub operation_id: String,
    pub actor_id: String,
    pub decision: ReviewHoldAction,
    pub binding: ReviewRunBinding,
    pub snapshot: ReviewContinuationSnapshot,
    pub reserved_run_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewContinuationBinding {
    pub pack_digest: String,
    pub next_sequence: u64,
    pub stored_result: HostResult,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRedriveArtifact {
    pub stage_id: String,
    pub verdicts: Vec<String>,
    pub log_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewTerminalStatus {
    Passed,
    Held,
    Failed,
}

/// One atomic terminal update for the run row and S4 result receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewDispatchCompletion {
    pub intent: ExecutionIntent,
    pub journal_run: String,
    pub status: ReviewTerminalStatus,
    pub counters: BTreeMap<String, u64>,
    pub result: crate::host::HostResult,
    pub held_snapshot: Option<ReviewHeldSnapshot>,
    pub provenance: ReviewBinaryProvenance,
    pub deadline: Option<i64>,
}

/// Atomically marks a running dispatch failed and records its diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewDispatchFailure {
    pub intent: ExecutionIntent,
    pub journal_run: String,
    pub counters: BTreeMap<String, u64>,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewHoldAction {
    Resume { target: String },
    CommitPass,
    CommitFail,
    Abort,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewRunError {
    MissingRunner,
    Dispatch(HostError),
    Rejected { log_path: String },
    BindingMismatch,
    EmitNotCompleted,
    Emit(EmitOutcome),
    Tuple(PortError),
}

/// High-level injection seam used by Line. Implementations must fail closed.
pub trait ReviewRunPort: Send {
    fn run(&mut self, request: ReviewRunRequest) -> Result<ReviewRunOutcome, ReviewRunError>;
    fn latest_run(
        &self,
        _order_id: &str,
        _attempt: u64,
    ) -> Result<Option<RunRecord>, ReviewRunError> {
        Ok(None)
    }
    fn resolve_hold(
        &mut self,
        _order_id: &str,
        _hold_id: &str,
        _action: ReviewHoldAction,
        _actor: &str,
    ) -> Result<(), ReviewRunError> {
        Err(ReviewRunError::MissingRunner)
    }
}

/// Private EmitLog decorator that authorizes promotion for this review tuple.
struct ReviewEmitLog<'a, L: EmitLog> {
    inner: &'a mut L,
    key: ArtifactTupleKey,
    promotion: PromotionReceipt,
}

impl<L: EmitLog> EmitLog for ReviewEmitLog<'_, L> {
    fn current_epoch(&self) -> Epoch {
        self.inner.current_epoch()
    }
    fn promotion_of(&self, key: &ArtifactTupleKey) -> Option<PromotionReceipt> {
        if key == &self.key {
            Some(self.promotion.clone())
        } else {
            self.inner.promotion_of(key)
        }
    }
    fn emit_of(&self, key: &ArtifactTupleKey) -> Option<EmitReceipt> {
        self.inner.emit_of(key)
    }
    fn record_emit(&mut self, receipt: &EmitReceipt) {
        self.inner.record_emit(receipt);
    }
    fn mark_complete(&mut self, key: &ArtifactTupleKey) {
        self.inner.mark_complete(key);
    }
    fn is_complete(&self, key: &ArtifactTupleKey) -> bool {
        self.inner.is_complete(key)
    }
}

/// Durable review orchestration. Concrete sink/log/host/intent/tuple adapters
/// are fixed when this runner is constructed; tests can supply fakes.
pub struct DurableReviewRun<S: EmitSink, L: EmitLog> {
    sink: S,
    log: L,
    host: Box<dyn DispatchHost<Result = HostResult> + Send>,
    intents: Box<dyn IntentLog + Send>,
    tuples: Arc<dyn TupleStore>,
}

impl<S: EmitSink, L: EmitLog> DurableReviewRun<S, L> {
    pub fn new(
        sink: S,
        log: L,
        host: Box<dyn DispatchHost<Result = HostResult> + Send>,
        intents: Box<dyn IntentLog + Send>,
        tuples: Arc<dyn TupleStore>,
    ) -> Self {
        Self {
            sink,
            log,
            host,
            intents,
            tuples,
        }
    }
}

impl<S, L> ReviewRunPort for DurableReviewRun<S, L>
where
    S: EmitSink + Send,
    L: EmitLog + Send,
{
    fn run(&mut self, req: ReviewRunRequest) -> Result<ReviewRunOutcome, ReviewRunError> {
        if let Some(tuple) = self
            .tuples
            .latest_promoted(&req.order, req.attempt)
            .map_err(ReviewRunError::Tuple)?
            .filter(|tuple| tuple.immutable_commit.0 == req.artifact_digest)
        {
            let key = tuple.key();
            if let Some(receipt) = self.log.emit_of(&key) {
                return Ok(ReviewRunOutcome::Completed(ReviewEvidence {
                    emit_receipt: receipt,
                    tuple_key: key,
                    verdict: ReviewVerdict::Passed,
                }));
            }
        }
        let intent = ExecutionIntent {
            order: req.order.clone(),
            attempt: req.attempt,
            run: req.run,
            activation: req.activation,
            generation: Generation(0),
            epoch: req.epoch,
            external_ref: format!(
                "review:{}:{}:{}..{}",
                req.order.0, req.attempt.0, req.base_commit, req.result_commit
            ),
            recorded_at: Utc::now(),
        };
        self.intents.append(&intent);
        let result = self
            .host
            .as_mut()
            .try_dispatch(&intent)
            .map_err(ReviewRunError::Dispatch)?;
        if let Some(hold) = result.held.clone() {
            return Ok(ReviewRunOutcome::Held(HeldOutcome {
                hold,
                order: req.order.0,
                attempt: req.attempt.0,
                run: req.run.0,
            }));
        }
        if result.exit_code != 0 {
            return Err(ReviewRunError::Rejected {
                log_path: result.log_path,
            });
        }
        if result.base_commit != req.base_commit
            || !valid_generations(&result.generations, &req, &result)
        {
            return Err(ReviewRunError::BindingMismatch);
        }
        let final_commit = result
            .generations
            .last()
            .map(|g| g.commit.clone())
            .unwrap_or_else(|| req.artifact_digest.clone());
        let tuple = ArtifactTuple {
            order: intent.order,
            attempt: intent.attempt,
            run: intent.run,
            generation: intent.generation,
            immutable_commit: ImmutableCommit(final_commit),
            artifact_location: ArtifactLocation(result.log_path),
            generations: if result.generations.is_empty() {
                vec![GenerationCommit {
                    generation: 0,
                    commit: req.result_commit.clone(),
                    fix_log_path: String::new(),
                }]
            } else {
                result.generations
            },
        };
        let key = tuple.key();
        let promotion = PromotionReceipt {
            tuple: key.clone(),
            promoted_at: Utc::now(),
        };
        let mut log = ReviewEmitLog {
            inner: &mut self.log,
            key: key.clone(),
            promotion,
        };
        let receipt = match emit_once(&mut self.sink, &mut log, &tuple, req.epoch) {
            EmitOutcome::Published(r) | EmitOutcome::Existing(r) => r,
            other => return Err(ReviewRunError::Emit(other)),
        };
        self.tuples.record(&tuple).map_err(ReviewRunError::Tuple)?;
        self.tuples
            .promote(&key, &receipt)
            .map_err(ReviewRunError::Tuple)?;
        if !complete_after_w4(&mut log, &key) && !log.is_complete(&key) {
            return Err(ReviewRunError::EmitNotCompleted);
        }
        Ok(ReviewRunOutcome::Completed(ReviewEvidence {
            emit_receipt: receipt,
            tuple_key: key,
            verdict: ReviewVerdict::Passed,
        }))
    }

    fn latest_run(&self, order: &str, attempt: u64) -> Result<Option<RunRecord>, ReviewRunError> {
        self.host
            .latest_run(order, attempt)
            .map_err(ReviewRunError::Dispatch)
    }

    fn resolve_hold(
        &mut self,
        order: &str,
        hold_id: &str,
        action: ReviewHoldAction,
        actor: &str,
    ) -> Result<(), ReviewRunError> {
        self.host
            .resolve_hold(order, hold_id, action, actor)
            .map_err(ReviewRunError::Dispatch)
    }
}

fn valid_generations(
    chain: &[GenerationCommit],
    req: &ReviewRunRequest,
    result: &HostResult,
) -> bool {
    if chain.is_empty() {
        return result.result_commit == req.result_commit;
    }
    chain.first().is_some_and(|first| {
        first.generation == 0 && first.commit == req.result_commit && first.fix_log_path.is_empty()
    }) && chain.iter().enumerate().all(|(i, entry)| {
        entry.generation == i as u64 && (i == 0 || !entry.fix_log_path.is_empty())
    }) && chain
        .last()
        .is_some_and(|last| last.commit == result.result_commit)
}
