use crate::types::{ArtifactTuple, ArtifactTupleKey, EmitReceipt, Epoch, PromotionReceipt};
use chrono::Utc;

pub trait EmitSink {
    fn publish(&mut self, t: &ArtifactTuple) -> EmitReceipt;
    fn receipt_of(&self, key: &ArtifactTupleKey) -> Option<EmitReceipt>;
}

/// Durable records used to authorize promotion emits and resume W3/W4.
pub trait EmitLog {
    fn current_epoch(&self) -> Epoch;
    fn promotion_of(&self, key: &ArtifactTupleKey) -> Option<PromotionReceipt>;
    fn record_emit(&mut self, receipt: &EmitReceipt);
    fn mark_complete(&mut self, key: &ArtifactTupleKey);
    fn is_complete(&self, key: &ArtifactTupleKey) -> bool;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmitOutcome {
    Published(EmitReceipt),
    Existing(EmitReceipt),
    StaleRejected,
    PromotionRequired,
}

/// Enforces I3/I4: only promoted tuples may be emitted, and an existing receipt
/// always wins over a retry without causing another external publication.
pub fn emit_once<S: EmitSink, L: EmitLog>(
    sink: &mut S,
    log: &mut L,
    tuple: &ArtifactTuple,
    fence_epoch: Epoch,
) -> EmitOutcome {
    let key = tuple.key();
    if let Some(receipt) = sink.receipt_of(&key) {
        return EmitOutcome::Existing(receipt);
    }
    if log.promotion_of(&key).is_none() {
        return EmitOutcome::PromotionRequired;
    }
    if fence_epoch < log.current_epoch() {
        return EmitOutcome::StaleRejected;
    }
    let receipt = sink.publish(tuple);
    log.record_emit(&receipt);
    EmitOutcome::Published(receipt)
}

/// W4 recovery: the external emit is already committed, so persist completion only.
pub fn complete_after_w4<L: EmitLog>(log: &mut L, key: &ArtifactTupleKey) -> bool {
    if log.is_complete(key) {
        return false;
    }
    log.mark_complete(key);
    true
}

/// A small helper for in-memory sinks and adapters creating receipts.
pub fn new_receipt(tuple: &ArtifactTuple, event: impl Into<String>) -> EmitReceipt {
    EmitReceipt {
        key: tuple.key(),
        event: event.into(),
        at: Utc::now(),
    }
}
