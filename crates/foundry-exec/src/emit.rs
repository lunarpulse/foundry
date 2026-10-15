use crate::types::{ArtifactTuple, ArtifactTupleKey, EmitReceipt, Epoch, PromotionReceipt};
use chrono::Utc;

/// The owner must serialize emits and epoch changes for an order, including
/// across processes. Publication and its tuple-keyed receipt must be one durable
/// transaction; reads/writes fail-stop on storage failure. `receipt_of` survives
/// adapter recreation and never consumes a receipt. Concrete fencing is b5.
pub trait EmitSink {
    fn publish(&mut self, t: &ArtifactTuple) -> EmitReceipt;
    /// Graph declarations select the event and payload reference. An adapter
    /// that only implements the legacy tuple helper must stop before any effect.
    fn publish_named(&mut self, _: &ArtifactTuple, _: &str, _: &str) -> EmitReceipt {
        panic!("emit adapter must implement declared event/payload publication")
    }
    fn receipt_of(&self, key: &ArtifactTupleKey) -> Option<EmitReceipt>;
}

/// Durable records used to authorize promotion emits and resume W3/W4. Reads
/// must survive restart; successful writes mean durable persistence. Storage
/// failures must stop execution. The owner serializes this log with the sink
/// and epoch updates for the order. Epoch changes must be monotonic; consumers
/// reject stale first-time emits while preserving exact receipt retries.
pub trait EmitLog {
    fn current_epoch(&self) -> Epoch;
    fn promotion_of(&self, key: &ArtifactTupleKey) -> Option<PromotionReceipt>;
    /// Durable orchestration copy, read before touching the external sink at W4.
    fn emit_of(&self, key: &ArtifactTupleKey) -> Option<EmitReceipt>;
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
    presented_epoch: Epoch,
) -> EmitOutcome {
    emit_with(sink, log, tuple, presented_epoch, |sink, tuple| sink.publish(tuple))
}

pub(crate) fn emit_named<S: EmitSink, L: EmitLog>(
    sink: &mut S,
    log: &mut L,
    tuple: &ArtifactTuple,
    presented_epoch: Epoch,
    event: &str,
    payload: &str,
) -> EmitOutcome {
    emit_with(sink, log, tuple, presented_epoch, |sink, tuple| {
        sink.publish_named(tuple, event, payload)
    })
}

fn emit_with<S: EmitSink, L: EmitLog>(
    sink: &mut S,
    log: &mut L,
    tuple: &ArtifactTuple,
    presented_epoch: Epoch,
    publish: impl FnOnce(&mut S, &ArtifactTuple) -> EmitReceipt,
) -> EmitOutcome {
    let key = tuple.key();
    if let Some(receipt) = log.emit_of(&key) {
        return EmitOutcome::Existing(receipt);
    }
    if let Some(receipt) = sink.receipt_of(&key) {
        // A crash can separate the atomic sink commit from this log copy.
        log.record_emit(&receipt);
        return EmitOutcome::Existing(receipt);
    }
    // Duplicate receipts take precedence over epoch checks, but first emits
    // require a current or newer fencing token.
    if presented_epoch < log.current_epoch() {
        return EmitOutcome::StaleRejected;
    }
    if log.promotion_of(&key).is_none() {
        return EmitOutcome::PromotionRequired;
    }
    let receipt = publish(sink, tuple);
    log.record_emit(&receipt);
    EmitOutcome::Published(receipt)
}

/// W4 recovery: the external emit is already committed, so persist completion only.
pub fn complete_after_w4<L: EmitLog>(log: &mut L, key: &ArtifactTupleKey) -> bool {
    if log.is_complete(key) || log.emit_of(key).is_none() {
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
