use crate::{
    emit::{complete_after_w4, emit_once, EmitLog, EmitOutcome, EmitSink},
    host::{HostError, HostResult},
    ports::{DispatchHost, IntentLog, PortError, TupleStore},
    types::{ArtifactLocation, ArtifactTuple, ArtifactTupleKey, Attempt, EmitReceipt, Epoch, ExecutionIntent, Generation, ImmutableCommit, OrderId, PromotionReceipt, RunId},
};
use chrono::Utc;
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
pub enum ReviewVerdict { Passed }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewEvidence {
    pub emit_receipt: EmitReceipt,
    pub tuple_key: ArtifactTupleKey,
    pub verdict: ReviewVerdict,
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
    fn run(&mut self, request: ReviewRunRequest) -> Result<ReviewEvidence, ReviewRunError>;
}

/// Private EmitLog decorator that authorizes promotion for this review tuple.
struct ReviewEmitLog<'a, L: EmitLog> {
    inner: &'a mut L,
    key: ArtifactTupleKey,
    promotion: PromotionReceipt,
}

impl<L: EmitLog> EmitLog for ReviewEmitLog<'_, L> {
    fn current_epoch(&self) -> Epoch { self.inner.current_epoch() }
    fn promotion_of(&self, key: &ArtifactTupleKey) -> Option<PromotionReceipt> {
        if key == &self.key { Some(self.promotion.clone()) } else { self.inner.promotion_of(key) }
    }
    fn emit_of(&self, key: &ArtifactTupleKey) -> Option<EmitReceipt> { self.inner.emit_of(key) }
    fn record_emit(&mut self, receipt: &EmitReceipt) { self.inner.record_emit(receipt); }
    fn mark_complete(&mut self, key: &ArtifactTupleKey) { self.inner.mark_complete(key); }
    fn is_complete(&self, key: &ArtifactTupleKey) -> bool { self.inner.is_complete(key) }
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
        Self { sink, log, host, intents, tuples }
    }

}

impl<S, L> ReviewRunPort for DurableReviewRun<S, L>
where
    S: EmitSink + Send,
    L: EmitLog + Send,
{
    fn run(&mut self, req: ReviewRunRequest) -> Result<ReviewEvidence, ReviewRunError> {
        if let Some(tuple) = self.tuples.latest_promoted(&req.order, req.attempt)
            .map_err(ReviewRunError::Tuple)?
            .filter(|tuple| tuple.immutable_commit.0 == req.artifact_digest)
        {
            let key = tuple.key();
            if let Some(receipt) = self.log.emit_of(&key) {
                return Ok(ReviewEvidence { emit_receipt: receipt, tuple_key: key, verdict: ReviewVerdict::Passed });
            }
        }
        let intent = ExecutionIntent {
            order: req.order.clone(), attempt: req.attempt, run: req.run,
            activation: req.activation, generation: Generation(0), epoch: req.epoch,
            external_ref: format!("review:{}:{}:{}..{}", req.order.0, req.attempt.0, req.base_commit, req.result_commit),
            recorded_at: Utc::now(),
        };
        self.intents.append(&intent);
        let result = self.host.as_mut().try_dispatch(&intent).map_err(ReviewRunError::Dispatch)?;
        if result.exit_code != 0 {
            return Err(ReviewRunError::Rejected { log_path: result.log_path });
        }
        if result.base_commit != req.base_commit || result.result_commit != req.result_commit {
            return Err(ReviewRunError::BindingMismatch);
        }
        let tuple = ArtifactTuple {
            order: intent.order, attempt: intent.attempt, run: intent.run,
            generation: intent.generation, immutable_commit: ImmutableCommit(req.artifact_digest),
            artifact_location: ArtifactLocation(result.log_path),
        };
        let key = tuple.key();
        let promotion = PromotionReceipt { tuple: key.clone(), promoted_at: Utc::now() };
        let mut log = ReviewEmitLog { inner: &mut self.log, key: key.clone(), promotion };
        let receipt = match emit_once(&mut self.sink, &mut log, &tuple, req.epoch) {
            EmitOutcome::Published(r) | EmitOutcome::Existing(r) => r,
            other => return Err(ReviewRunError::Emit(other)),
        };
        self.tuples.record(&tuple).map_err(ReviewRunError::Tuple)?;
        self.tuples.promote(&key, &receipt).map_err(ReviewRunError::Tuple)?;
        if !complete_after_w4(&mut log, &key) && !log.is_complete(&key) {
            return Err(ReviewRunError::EmitNotCompleted);
        }
        Ok(ReviewEvidence { emit_receipt: receipt, tuple_key: key, verdict: ReviewVerdict::Passed })
    }
}

