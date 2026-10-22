//! ProductAtomicV1 implementation of the shared frozen review host port.
//! All state stays in ProductStore's private connection. The artifact, session,
//! and claim roots are pinned independently by `configure_review_backend`.
use super::{
    ProductArtifactContent, ProductArtifactInfo, ProductCapability, ProductHoldAcceptance,
    ProductHoldDecision, ProductStore, resolve_session_in,
};
use foundry_exec::{
    ExecutionIntent, HostError, HostKey, HostResult, ReviewAdmission, ReviewAdmissionRequest,
    ReviewAdmissionStore, ReviewBinaryProvenance, ReviewContinuationBinding,
    ReviewContinuationCompletion, ReviewCounterCheckpoint, ReviewDispatchCompletion,
    ReviewDispatchFailure, ReviewDispatchStore, ReviewHoldRecord, ReviewHostStore,
    ReviewRunBinding, ReviewSessionReceipt, ReviewStageEvidence, ReviewStageFinish,
    ReviewStageFinishRequest, ReviewStageStart, ReviewTerminalStatus, ReviewWorkClaim,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use std::{
    fs,
    fs::File,
    path::{Path, PathBuf},
};

type LatestRunRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
);
type ActiveFenceRow = (
    String,
    i64,
    String,
    String,
    String,
    String,
    i64,
    i64,
    i64,
    String,
    String,
);
struct ActiveRunBinding {
    lifecycle: i64,
    source_revision: i64,
    admission_seq: i64,
    admission_hash: String,
    repository_id: String,
    base_commit: String,
}

fn db_error(error: impl std::fmt::Display) -> HostError {
    HostError::Persistence(error.to_string())
}
fn unsupported(what: &str) -> HostError {
    HostError::Worker(format!(
        "product review recovery operation unavailable: {what}"
    ))
}

// PackReviewHost currently pins this existing product budget at each stage
// reservation. Redrive charges use the same bound because its port predates
// the budget-limit argument; this keeps charge-at-start under the identical
// host limit instead of creating a separate/unbounded allowance.
const PRODUCT_REVIEW_BUDGET_LIMIT: i64 = 10_000;

/// Test-support-only observation of charge transaction acquisition. These
/// notifications never supply values to the transaction or change its result.
#[cfg(any(test, feature = "p2-test-support"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChargeBudgetObservation {
    BeginAttempt,
    Busy { attempts: i32 },
    StageStartBeginAttempt,
}

#[cfg(any(test, feature = "p2-test-support"))]
pub type ChargeBudgetObserver = std::sync::Arc<dyn Fn(ChargeBudgetObservation) + Send + Sync>;

#[cfg(any(test, feature = "p2-test-support"))]
std::thread_local! {
    static ACTIVE_CHARGE_BUDGET_OBSERVER: std::cell::RefCell<Option<ChargeBudgetObserver>> = const { std::cell::RefCell::new(None) };
    static CHARGE_BUDGET_WAIT_STARTED: std::cell::Cell<Option<std::time::Instant>> = const { std::cell::Cell::new(None) };
}

#[cfg(any(test, feature = "p2-test-support"))]
fn notify_charge_budget(observer: &ChargeBudgetObserver, event: ChargeBudgetObservation) {
    observer(event);
}

#[cfg(any(test, feature = "p2-test-support"))]
fn charge_budget_busy_handler(attempts: i32) -> bool {
    ACTIVE_CHARGE_BUDGET_OBSERVER.with(|slot| {
        if let Some(observer) = slot.borrow().as_ref() {
            notify_charge_budget(observer, ChargeBudgetObservation::Busy { attempts });
        }
    });
    CHARGE_BUDGET_WAIT_STARTED.with(|slot| {
        slot.get()
            .is_some_and(|started| started.elapsed() < std::time::Duration::from_secs(5))
    })
}

fn json<T: serde::Serialize>(value: &T) -> Result<String, HostError> {
    serde_json::to_string(value).map_err(db_error)
}
fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, HostError> {
    serde_json::from_str(value).map_err(db_error)
}
fn key_json(intent: &ExecutionIntent) -> Result<String, HostError> {
    json(&HostKey {
        order: intent.order.0.clone(),
        attempt: intent.attempt.0,
        run: intent.run.0,
        activation: intent.activation,
    })
}

fn append_hold_outcome_in(
    tx: &rusqlite::Transaction<'_>,
    receipt: &ProductHoldAcceptance,
    decision: &foundry_exec::ReviewHoldAction,
    at: chrono::DateTime<chrono::Utc>,
) -> Result<(), HostError> {
    let mut statement = tx.prepare(
        "SELECT seq,order_id,from_state,to_state,event,actor,intent,at,prev_hash,hash,event_json FROM ledger ORDER BY seq",
    ).map_err(db_error)?;
    let rows = statement
        .query_map([], |r| {
            Ok(crate::sqlite::LedgerRow {
                seq: r.get(0)?,
                order_id: r.get(1)?,
                from_state: r.get(2)?,
                to_state: r.get(3)?,
                event: r.get(4)?,
                actor: r.get(5)?,
                intent: r.get(6)?,
                at: r.get(7)?,
                prev_hash: r.get(8)?,
                hash: r.get(9)?,
                event_json: r.get(10)?,
            })
        })
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    drop(statement);
    let mut orders = crate::replay::replay(&rows, crate::sqlite::SqliteLedger::verify_rows)
        .map_err(|_| HostError::Persistence("S1 lifecycle verification failed".into()))?;
    let current = orders
        .iter_mut()
        .rev()
        .find(|item| item.order_id == receipt.order_id)
        .ok_or_else(|| HostError::Acceptance("hold outcome has no verified S1 lifecycle".into()))?;
    if current.lifecycle as i64 + 1 != receipt.lifecycle
        || current.order.attempt() as i64 != receipt.attempt
        || current.order.state().to_string() != "waiting"
        || current.last_seq != receipt.predecessor_seq
    {
        return Err(HostError::Acceptance(
            "hold outcome S1 predecessor changed".into(),
        ));
    }
    let event = match decision {
        foundry_exec::ReviewHoldAction::CommitPass => super::OrderEvent::InspectionPassed,
        foundry_exec::ReviewHoldAction::CommitFail => super::OrderEvent::InspectionFailed {
            reason: "product-review:commit-fail".into(),
        },
        foundry_exec::ReviewHoldAction::Abort => super::OrderEvent::InspectionFailed {
            reason: "product-review:abort".into(),
        },
        foundry_exec::ReviewHoldAction::Resume { .. } => {
            return Err(HostError::Acceptance(
                "resume is not a terminal S1 outcome".into(),
            ));
        }
    };
    let from = current.order.state().to_string();
    let to = current
        .order
        .transition(event.clone(), current.order.attempt())
        .map_err(db_error)?
        .to_string();
    let seq: i64 = tx
        .query_row("SELECT COALESCE(MAX(seq),0)+1 FROM ledger", [], |r| {
            r.get(0)
        })
        .map_err(db_error)?;
    let previous_hash: String = tx
        .query_row(
            "SELECT hash FROM ledger ORDER BY seq DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    let event_text = format!("{event:?}");
    let event_json = crate::event_json::event_to_json(&event);
    let actor = super::ActorId {
        kind: super::ActorKind::Human,
        source: "product-session".into(),
        id: receipt.actor_id.clone(),
    };
    let actor_text = format!("{:?}/{}", actor.kind, actor.id);
    let at_text = at.to_rfc3339();
    let v1 = format!(
        "{seq}|{}|{from}|{to}|{event_text}|{actor_text}|{}|{at_text}|{previous_hash}",
        receipt.order_id, receipt.operation_id
    );
    let event_hash = hex::encode(sha2::Sha256::digest(
        crate::event_json::v2_hash_payload(&v1, &event_json).as_bytes(),
    ));
    tx.execute(
        "INSERT INTO product_transition_intents_v1(order_id,lifecycle,expected_predecessor,operation) VALUES(?1,?2,?3,?4)",
        rusqlite::params![receipt.order_id,receipt.lifecycle,receipt.predecessor_seq,format!("review-hold-{}",match decision { foundry_exec::ReviewHoldAction::CommitPass=>"commit-pass",foundry_exec::ReviewHoldAction::CommitFail=>"commit-fail",foundry_exec::ReviewHoldAction::Abort=>"abort",_=>"resume" })],
    ).map_err(db_error)?;
    tx.execute(
        "INSERT INTO ledger(order_id,from_state,to_state,event,actor,intent,at,prev_hash,event_json,hash) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        rusqlite::params![receipt.order_id,from,to,event_text,actor_text,receipt.operation_id,at_text,previous_hash,event_json,event_hash],
    ).map_err(db_error)?;
    Ok(())
}

fn validate_work_claim_in(
    tx: &rusqlite::Transaction<'_>,
    claim: &ReviewWorkClaim,
    intent: &ExecutionIntent,
    lifecycle: i64,
    source_revision: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), HostError> {
    if claim.order_id != intent.order.0
        || claim.lifecycle != lifecycle
        || claim.source_revision != source_revision
        || claim.worker_id.trim().is_empty()
        || claim.epoch <= 0
        || claim.fence <= 0
        || claim.lease_until <= now
        || claim.payload_digest != super::payload_digest(&claim.task_kind, &claim.payload_snapshot)
    {
        return Err(HostError::Acceptance(
            "product work claim does not bind the current admission".into(),
        ));
    }
    let exact: bool = tx
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM product_work_claims_v1 c
                JOIN product_work_backlog_v1 b ON b.order_id=c.order_id AND b.lifecycle=c.lifecycle
                JOIN product_orders_v1 o ON o.order_id=c.order_id AND o.lifecycle=c.lifecycle
                JOIN product_sources_v1 s ON s.connector_id=o.connector_id
                JOIN product_transition_receipts_v1 r ON r.seq=o.admission_seq AND r.event_hash=o.admission_hash
                WHERE c.order_id=?1 AND c.lifecycle=?2 AND c.owner_id=?3 AND c.epoch=?4
                  AND c.fence=?5 AND c.state='claimed' AND c.lease_until>?6
                  AND c.source_revision=?7 AND s.revision=?7 AND s.enabled=1
                  AND b.state='claimed' AND b.delivery_id=?8 AND c.payload_digest=?9
                  AND o.task_kind=?10 AND o.payload_snapshot=?11
                  AND r.operation='github-admission'
            )",
            rusqlite::params![
                claim.order_id,
                claim.lifecycle,
                claim.worker_id,
                claim.epoch,
                claim.fence,
                now.to_rfc3339(),
                claim.source_revision,
                claim.delivery_id,
                claim.payload_digest,
                claim.task_kind,
                claim.payload_snapshot,
            ],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    if !exact {
        return Err(HostError::Acceptance(
            "product work claim is stale, expired, or not bound to the admitted source".into(),
        ));
    }
    super::service::validate_review_work_claim_in(tx, claim, now).map_err(|_| {
        HostError::Acceptance("review service authority is absent or expired".into())
    })?;
    Ok(())
}
fn hold_snapshot(
    snapshot: &foundry_exec::ReviewHeldSnapshot,
    intent: &ExecutionIntent,
    run: &str,
) -> Result<String, HostError> {
    let entry = serde_json::json!({"node_id":snapshot.node_id,"entry_counters":snapshot.entry_counters,"reverted_base":snapshot.requested_revert});
    let value = serde_json::json!({
        "hold":snapshot.hold,"entry":entry,"engine_state":snapshot.engine_state,"intent":intent,
        "run_id":run,"snapshot_run_id":run,"session_ids":snapshot.session_ids,
        "accepted_logs":snapshot.accepted_logs,"generations":snapshot.generations,
        "latest_commit":snapshot.latest_commit,"sequence":snapshot.sequence,
        // Initial-run holds: no resume has occurred, so the resume counter is
        // zero by definition. Resumed-run holds persist through
        // complete_continuation, which carries the live counter.
        "resume_sequence":0u64,"human_events":[]
    });
    json(&value)
}
fn encode_continuation_snapshot(
    snapshot: &foundry_exec::ReviewContinuationSnapshot,
) -> Result<String, HostError> {
    let entry = serde_json::json!({"node_id":snapshot.held.node_id,"entry_counters":snapshot.held.entry_counters,"reverted_base":snapshot.held.requested_revert});
    let events:Vec<serde_json::Value>=snapshot.human_events.iter().map(|event|{
        let (action,target)=match &event.action{foundry_exec::ReviewHumanAction::Resume{target}=>("resume",Some(target.clone())),foundry_exec::ReviewHumanAction::CommitPass=>("commit_pass",None),foundry_exec::ReviewHumanAction::CommitFail=>("commit_fail",None),foundry_exec::ReviewHumanAction::Abort=>("abort",None),foundry_exec::ReviewHumanAction::DeadlineExpired=>("human_deadline_expired",None),foundry_exec::ReviewHumanAction::Other{name,target}=>(name.as_str(),target.clone())};
        serde_json::json!({"actor":event.actor,"at":event.occurred_at,"action":action,"target":target})
    }).collect();
    json(
        &serde_json::json!({"hold":snapshot.held.hold,"entry":entry,"engine_state":snapshot.held.engine_state,"intent":snapshot.intent,"run_id":snapshot.binding.journal_run,"snapshot_run_id":snapshot.snapshot_run_id,"session_ids":snapshot.held.session_ids,"accepted_logs":snapshot.held.accepted_logs,"generations":snapshot.held.generations,"latest_commit":snapshot.held.latest_commit,"sequence":snapshot.held.sequence,"resume_sequence":snapshot.resume_sequence,"human_events":events}),
    )
}
fn decode_hold(
    binding: ReviewRunBinding,
    encoded: &str,
) -> Result<foundry_exec::ReviewContinuationSnapshot, HostError> {
    let doc: serde_json::Value = decode(encoded)?;
    let hold = serde_json::from_value(doc["hold"].clone()).map_err(db_error)?;
    let intent: ExecutionIntent =
        serde_json::from_value(doc["intent"].clone()).map_err(db_error)?;
    let held = foundry_exec::ReviewHeldSnapshot {
        hold,
        node_id: doc["entry"]["node_id"]
            .as_str()
            .ok_or_else(|| db_error("held snapshot entry node missing"))?
            .to_owned(),
        entry_counters: serde_json::from_value(doc["entry"]["entry_counters"].clone())
            .map_err(db_error)?,
        requested_revert: doc["entry"]["reverted_base"].as_str().map(str::to_owned),
        engine_state: doc["engine_state"]
            .as_str()
            .ok_or_else(|| db_error("held snapshot engine state missing"))?
            .to_owned(),
        session_ids: serde_json::from_value(doc["session_ids"].clone()).map_err(db_error)?,
        accepted_logs: serde_json::from_value(doc["accepted_logs"].clone()).map_err(db_error)?,
        generations: serde_json::from_value(doc["generations"].clone()).map_err(db_error)?,
        latest_commit: doc["latest_commit"]
            .as_str()
            .ok_or_else(|| db_error("held snapshot commit missing"))?
            .to_owned(),
        sequence: doc["sequence"]
            .as_u64()
            .ok_or_else(|| db_error("held snapshot sequence missing"))?,
    };
    if intent.order.0 != binding.order_id || intent.attempt.0 != binding.attempt {
        return Err(db_error("held snapshot binding mismatch"));
    }
    let human_events = doc["human_events"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|event| {
            let action = match event["action"].as_str().unwrap_or("") {
                "resume" => foundry_exec::ReviewHumanAction::Resume {
                    target: event["target"].as_str().unwrap_or("").to_owned(),
                },
                "commit_pass" => foundry_exec::ReviewHumanAction::CommitPass,
                "commit_fail" => foundry_exec::ReviewHumanAction::CommitFail,
                "abort" => foundry_exec::ReviewHumanAction::Abort,
                "human_deadline_expired" => foundry_exec::ReviewHumanAction::DeadlineExpired,
                other => foundry_exec::ReviewHumanAction::Other {
                    name: other.to_owned(),
                    target: event["target"].as_str().map(str::to_owned),
                },
            };
            foundry_exec::ReviewHumanEvent {
                actor: event["actor"].as_str().unwrap_or("").to_owned(),
                occurred_at: event["at"].as_str().unwrap_or("").to_owned(),
                action,
            }
        })
        .collect();
    Ok(foundry_exec::ReviewContinuationSnapshot {
        snapshot_run_id: doc["snapshot_run_id"]
            .as_str()
            .unwrap_or(&binding.journal_run)
            .to_owned(),
        resume_sequence: doc["resume_sequence"].as_u64().unwrap_or(0),
        binding,
        held,
        intent,
        human_events,
    })
}

fn validate_admitted(
    store: &ProductStore,
    order: &str,
) -> Result<(i64, i64, String, String, i64), HostError> {
    let row = store.conn.query_row(
        "SELECT o.lifecycle,o.source_revision,o.repository_id,s.enabled,s.revision,o.admission_seq,o.admission_hash FROM product_orders_v1 o JOIN product_sources_v1 s ON s.connector_id=o.connector_id WHERE o.order_id=?1",
        [order], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?,r.get::<_,i64>(5)?,r.get::<_,String>(6)?)),
    ).optional().map_err(db_error)?.ok_or_else(|| HostError::Acceptance("review order has no durable product admission".into()))?;
    if row.3 != 1 || row.1 != row.4 {
        return Err(HostError::Acceptance(
            "review source is disabled or rebound".into(),
        ));
    }
    let latest: Option<(i64, String)> = store
        .conn
        .query_row(
            "SELECT seq,to_state FROM ledger WHERE order_id=?1 ORDER BY seq DESC LIMIT 1",
            [order],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(db_error)?;
    let Some((seq, state)) = latest else {
        return Err(HostError::Acceptance(
            "review admission has no authoritative S1 event".into(),
        ));
    };
    if seq < row.5
        || !matches!(
            state.as_str(),
            "queued" | "working" | "held" | "waiting" | "inspecting"
        )
    {
        return Err(HostError::Acceptance(
            "product lifecycle does not admit review work".into(),
        ));
    }
    Ok((row.0, row.1, row.2, row.6, row.5))
}

fn validate_admitted_in(
    tx: &rusqlite::Transaction<'_>,
    order: &str,
) -> Result<(i64, i64, String, String, i64), HostError> {
    let row: Option<(i64, i64, String, i64, i64, i64, String)> = tx.query_row(
        "SELECT o.lifecycle,o.source_revision,o.repository_id,s.enabled,s.revision,o.admission_seq,o.admission_hash FROM product_orders_v1 o JOIN product_sources_v1 s ON s.connector_id=o.connector_id WHERE o.order_id=?1",
        [order], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)),
    ).optional().map_err(db_error)?;
    let Some((
        lifecycle,
        source_revision,
        repository,
        enabled,
        source_now,
        admission_seq,
        admission_hash,
    )) = row
    else {
        return Err(HostError::Acceptance(
            "review order has no durable product admission".into(),
        ));
    };
    if enabled != 1 || source_revision != source_now {
        return Err(HostError::Acceptance(
            "review source is disabled or rebound".into(),
        ));
    }
    let latest: Option<(i64, String)> = tx
        .query_row(
            "SELECT seq,to_state FROM ledger WHERE order_id=?1 ORDER BY seq DESC LIMIT 1",
            [order],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(db_error)?;
    if !latest.is_some_and(|(seq, state)| {
        seq >= admission_seq
            && matches!(
                state.as_str(),
                "queued" | "working" | "held" | "waiting" | "inspecting"
            )
    }) {
        return Err(HostError::Acceptance(
            "product lifecycle does not admit review work".into(),
        ));
    }
    let exact_admission: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM product_transition_receipts_v1 r JOIN ledger l ON l.seq=r.seq AND l.hash=r.event_hash WHERE r.seq=?1 AND r.event_hash=?2 AND r.order_id=?3 AND r.operation='github-admission' AND l.order_id=?3)",
        params![admission_seq, admission_hash, order], |r| r.get(0),
    ).map_err(db_error)?;
    if !exact_admission {
        return Err(HostError::Acceptance(
            "product source admission receipt is invalid".into(),
        ));
    }
    Ok((
        lifecycle,
        source_revision,
        repository,
        admission_hash,
        admission_seq,
    ))
}

impl ReviewAdmissionStore for ProductStore {
    fn validate_product_work_claim(
        &self,
        claim: &ReviewWorkClaim,
        intent: &ExecutionIntent,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), HostError> {
        if claim.order_id != intent.order.0
            || claim.worker_id.trim().is_empty()
            || claim.epoch <= 0
            || claim.fence <= 0
            || claim.lease_until <= now
            || claim.payload_digest
                != super::payload_digest(&claim.task_kind, &claim.payload_snapshot)
        {
            return Err(HostError::Acceptance(
                "product work claim is invalid or expired".into(),
            ));
        }
        let tx = self.conn.unchecked_transaction().map_err(db_error)?;
        let exact: bool = tx.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM product_work_claims_v1 c
                JOIN product_work_backlog_v1 b ON b.order_id=c.order_id AND b.lifecycle=c.lifecycle
                JOIN product_orders_v1 o ON o.order_id=c.order_id AND o.lifecycle=c.lifecycle
                JOIN product_sources_v1 s ON s.connector_id=o.connector_id
                JOIN product_transition_receipts_v1 r ON r.seq=o.admission_seq AND r.event_hash=o.admission_hash
                WHERE c.order_id=?1 AND c.lifecycle=?2 AND c.owner_id=?3 AND c.epoch=?4 AND c.fence=?5
                  AND c.state='claimed' AND c.lease_until>?6 AND c.source_revision=?7
                  AND s.revision=?7 AND s.enabled=1 AND b.state='claimed' AND b.delivery_id=?8
                  AND c.payload_digest=?9 AND o.task_kind=?10 AND o.payload_snapshot=?11
                  AND r.operation='github-admission'
            )",
            params![claim.order_id,claim.lifecycle,claim.worker_id,claim.epoch,claim.fence,now.to_rfc3339(),claim.source_revision,claim.delivery_id,claim.payload_digest,claim.task_kind,claim.payload_snapshot],
            |row| row.get(0),
        ).map_err(db_error)?;
        if !exact {
            return Err(HostError::Acceptance(
                "product work claim was reclaimed or its source changed".into(),
            ));
        }
        super::service::validate_review_work_claim_in(&tx, claim, now).map_err(|_| {
            HostError::Acceptance("review service authority is absent or expired".into())
        })?;
        tx.commit().map_err(db_error)?;
        Ok(())
    }
    fn admit_review_activation(
        &mut self,
        request: ReviewAdmissionRequest,
    ) -> Result<ReviewAdmission, HostError> {
        let cfg = self
            .review_config
            .as_ref()
            .ok_or_else(|| unsupported("review roots are not configured"))?;
        if request.pack_digest != cfg.pack_digest {
            return Err(HostError::Acceptance("pinned review pack changed".into()));
        }
        let intent = &request.intent;
        if intent.generation.0 != 0 || intent.order.0.is_empty() || intent.epoch.0 > i64::MAX as u64
        {
            return Err(HostError::Acceptance(
                "unsupported product review activation".into(),
            ));
        }
        let expected_ref = format!(
            "review:{}:{}:{}..{}",
            intent.order.0, intent.attempt.0, request.base_commit, request.result_commit
        );
        if intent.external_ref != expected_ref {
            return Err(HostError::Acceptance(
                "review external reference binding mismatch".into(),
            ));
        }
        let key = key_json(intent)?;
        let clock = self.clock.clone();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let now = clock.now();
        let (lifecycle, source_revision, _, admission_hash, admission_seq) =
            validate_admitted_in(&tx, &intent.order.0)?;
        let (current_lifecycle,current_source,current_hash,current_seq):(i64,i64,String,i64)=tx.query_row("SELECT lifecycle,source_revision,admission_hash,admission_seq FROM product_orders_v1 WHERE order_id=?1",[&intent.order.0],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(db_error)?;
        if current_lifecycle != lifecycle
            || current_source != source_revision
            || current_hash != admission_hash
            || current_seq != admission_seq
        {
            return Err(HostError::Acceptance(
                "product admission changed before review".into(),
            ));
        }
        let work = request.product_work.as_ref().ok_or_else(|| {
            HostError::Acceptance("product review requires a durable work claim".into())
        })?;
        validate_work_claim_in(&tx, work, intent, lifecycle, source_revision, now)?;
        let existing: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM product_review_host_accept_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND activation=?4)",params![intent.order.0,intent.attempt.0 as i64,intent.run.0 as i64,intent.activation as i64],|r|r.get(0)).map_err(db_error)?;
        if existing {
            return Err(HostError::Persistence(
                "accepted product activation has no recoverable result; refusing redispatch".into(),
            ));
        }
        // Product result identity keeps the numeric tuple axes fixed. Do not
        // execute an activation that aliases an accepted host key or a
        // previously reserved successor run in the same tuple namespace.
        let run_alias:bool=tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM product_review_host_accept_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3)
              OR EXISTS(SELECT 1 FROM product_review_run_reservations_v1 WHERE order_id=?1 AND followup_run_id=?3)",
            params![intent.order.0,intent.attempt.0 as i64,intent.run.0 as i64],|r|r.get(0),
        ).map_err(db_error)?;
        if run_alias {
            return Err(HostError::Acceptance(
                "product review numeric run aliases an existing activation or reservation".into(),
            ));
        }
        let previous: Option<(String,i64,String,String,String)> = tx.query_row(
            "SELECT r.status,a.epoch,r.base_commit,r.result_commit,r.result_json FROM product_review_runs_v1 r JOIN product_review_active_v1 a ON a.order_id=r.order_id AND a.journal_run=r.run_id WHERE r.order_id=?1 ORDER BY r.created_at DESC LIMIT 1", [&intent.order.0],
            |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get::<_,Option<String>>(4)?.unwrap_or_default())),
        ).optional().map_err(db_error)?;
        if let Some((status, epoch, base, result, _)) = previous
            && (intent.epoch.0 < epoch as u64
                || matches!(status.as_str(), "held" | "resuming" | "running")
                || (status == "resolved"
                    && (base != request.base_commit || result != request.result_commit)))
        {
            return Err(HostError::Acceptance(
                "stale or unresolved product review run".into(),
            ));
        }
        tx.execute("INSERT INTO product_review_epochs_v1(order_id,epoch) VALUES(?1,?2) ON CONFLICT(order_id) DO UPDATE SET epoch=excluded.epoch WHERE excluded.epoch>=product_review_epochs_v1.epoch",params![intent.order.0,intent.epoch.0 as i64]).map_err(db_error)?;
        let run_id = format!(
            "{}:{}:{}:{}",
            intent.order.0, intent.attempt.0, intent.run.0, intent.activation
        );
        tx.execute("INSERT INTO product_review_runs_v1(order_id,attempt,run_id,base_commit,result_commit,pack_digest,status,counters_json,product_lifecycle,source_revision,admission_seq,admission_hash,created_at) VALUES(?1,?2,?3,?4,?5,?6,'running','{}',?7,?8,?9,?10,unixepoch())",params![intent.order.0,intent.attempt.0 as i64,run_id,request.base_commit,request.result_commit,cfg.pack_digest,lifecycle,source_revision,admission_seq,admission_hash]).map_err(db_error)?;
        tx.execute("INSERT INTO product_review_work_bindings_v1(order_id,attempt,run_id,lifecycle,delivery_id,worker_id,claim_epoch,claim_fence,source_revision,payload_digest,task_kind,payload_snapshot) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",params![intent.order.0,intent.attempt.0 as i64,run_id,lifecycle,work.delivery_id,work.worker_id,work.epoch,work.fence,work.source_revision,work.payload_digest,work.task_kind,work.payload_snapshot]).map_err(db_error)?;
        tx.execute("INSERT INTO product_review_host_accept_v1(order_id,attempt,run_id,activation) VALUES(?1,?2,?3,?4)",params![intent.order.0,intent.attempt.0 as i64,intent.run.0 as i64,intent.activation as i64]).map_err(db_error)?;
        tx.execute(
            "INSERT INTO product_review_bindings_v1(key_json,intent_json) VALUES(?1,?2)",
            params![key, json(intent)?],
        )
        .map_err(db_error)?;
        // Capture the exact original DriveReview and source/service revisions
        // while the validated activation is admitted. Historical activations
        // without this snapshot remain readable, but can never be promoted to
        // strict local results by looking up today's grant.
        if let Some(service) = work.service_binding.as_ref() {
            let authority: Option<(i64,i64,i64,i64)> = tx.query_row(
                "SELECT g.grant_revision,i.generation,i.revision,ps.revision
                 FROM product_service_grants_v1 g
                 JOIN product_scope_source_instances_v1 i ON i.source_instance_id=g.source_instance_id
                 JOIN product_services_v1 s ON s.service_id=g.service_id
                 JOIN product_sources_v1 ps ON ps.connector_id=i.connector_id
                 JOIN product_intake_source_bindings_v1 ib ON ib.delivery_id=?1
                 WHERE g.service_id=?2 AND g.source_instance_id=?3 AND g.action='drive_review'
                   AND g.enabled=1 AND i.enabled=1 AND ps.enabled=1
                   AND i.source_instance_id=ib.source_instance_id
                   AND ib.source_revision=ps.revision AND ib.source_generation=i.generation
                   AND ib.source_instance_revision=i.revision
                   AND s.revision=?4 AND ps.revision=?5",
                params![work.delivery_id,service.service_id,service.source_instance_id,service.service_revision,work.source_revision],
                |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
            ).optional().map_err(db_error)?;
            let Some((
                grant_revision,
                source_generation,
                source_instance_revision,
                connector_revision,
            )) = authority
            else {
                return Err(HostError::Acceptance(
                    "original review authority is unavailable".into(),
                ));
            };
            tx.execute("INSERT INTO product_review_authority_v1(key_json,order_id,lifecycle,attempt,run_id,activation,journal_run,host_epoch,service_id,service_revision,credential_revision,drive_review_grant_revision,source_instance_id,source_generation,source_instance_revision,connector_revision) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",params![key,intent.order.0,lifecycle,intent.attempt.0 as i64,intent.run.0 as i64,intent.activation as i64,run_id,intent.epoch.0 as i64,service.service_id,service.service_revision,service.credential_revision,grant_revision,service.source_instance_id,source_generation,source_instance_revision,connector_revision]).map_err(db_error)?;
        }
        tx.execute("INSERT INTO product_review_active_v1(order_id,key_json,epoch,journal_run) VALUES(?1,?2,?3,?4) ON CONFLICT(order_id) DO UPDATE SET key_json=excluded.key_json,epoch=excluded.epoch,journal_run=excluded.journal_run",params![intent.order.0,key,intent.epoch.0 as i64,run_id]).map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(ReviewAdmission {
            journal_run: run_id,
            inherited_counters: Default::default(),
            inherited_result_json: None,
        })
    }
}

impl ReviewDispatchStore for ProductStore {
    fn load_review_result(&self, key: &HostKey) -> Result<Option<HostResult>, HostError> {
        self.conn.query_row("SELECT result_json FROM product_review_host_results_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND activation=?4",params![key.order,key.attempt as i64,key.run as i64,key.activation as i64],|r|r.get::<_,String>(0)).optional().map_err(db_error)?.map(|s|decode(&s)).transpose()
    }
    fn checkpoint_review_counters(
        &mut self,
        request: ReviewCounterCheckpoint,
    ) -> Result<(), HostError> {
        let cfg = self
            .review_config
            .as_ref()
            .ok_or_else(|| unsupported("review roots are not configured"))?;
        validate_admitted(self, &request.intent.order.0)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        ensure_active_fence(&tx, &request.intent, &request.journal_run, &cfg.pack_digest)?;
        let n=tx.execute("UPDATE product_review_runs_v1 SET counters_json=?1 WHERE order_id=?2 AND attempt=?3 AND run_id=?4 AND status='running'",params![json(&request.counters)?,request.intent.order.0,request.intent.attempt.0 as i64,request.journal_run]).map_err(db_error)?;
        if n != 1 {
            return Err(HostError::Worker(
                "counter checkpoint is outside active product run".into(),
            ));
        }
        tx.commit().map_err(db_error)
    }
    fn start_review_stage(&mut self, request: ReviewStageStart) -> Result<(), HostError> {
        let cfg = self
            .review_config
            .as_ref()
            .ok_or_else(|| unsupported("review roots are not configured"))?;
        validate_admitted(self, &request.intent.order.0)?;
        let clock = self.clock.clone();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let now = clock.now();
        ensure_new_work(
            &tx,
            &request.intent,
            &request.journal_run,
            &cfg.pack_digest,
            now,
        )?;
        if request.sequence > i64::MAX as u64 {
            return Err(db_error("review stage sequence overflow"));
        }
        let used: i64 = tx
            .query_row(
                "SELECT used FROM product_review_budget_v1 WHERE order_id=?1",
                [&request.intent.order.0],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?
            .unwrap_or(0);
        if used >= request.budget_limit as i64 {
            return Err(HostError::Worker("product review budget exhausted".into()));
        }
        tx.execute("INSERT INTO product_review_budget_v1(order_id,used) VALUES(?1,1) ON CONFLICT(order_id) DO UPDATE SET used=used+1",[&request.intent.order.0]).map_err(db_error)?;
        tx.execute("INSERT INTO product_review_stages_v1(order_id,run_id,sequence,stage_id,status) VALUES(?1,?2,?3,?4,'accepted')",params![request.intent.order.0,request.journal_run,request.sequence as i64,request.stage_id]).map_err(db_error)?;
        tx.commit().map_err(db_error)
    }
    fn finish_review_stage(&mut self, request: ReviewStageFinishRequest) -> Result<(), HostError> {
        let cfg = self
            .review_config
            .as_ref()
            .ok_or_else(|| unsupported("review roots are not configured"))?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        ensure_active_fence(&tx, &request.intent, &request.journal_run, &cfg.pack_digest)?;
        let (status, output, session, artifact) = match request.finish {
            ReviewStageFinish::Complete(e) => {
                let output = stage_output(&e)?;
                ("complete", output, e.session.clone(), Some(e))
            }
            ReviewStageFinish::Error(e) => ("error", e, None, None),
            ReviewStageFinish::Blocked(e) => {
                let claimed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?2 AND sequence=?3)",params![request.intent.order.0,request.journal_run,request.sequence as i64],|r|r.get(0)).map_err(db_error)?;
                if claimed {
                    return Err(HostError::Worker(
                        "product review blocked sequence already used".into(),
                    ));
                }
                tx.execute("INSERT INTO product_review_stages_v1(order_id,run_id,sequence,stage_id,status,output_json) VALUES(?1,?2,?3,?4,'blocked',?5)",params![request.intent.order.0,request.journal_run,request.sequence as i64,request.stage_id,e]).map_err(db_error)?;
                tx.commit().map_err(db_error)?;
                return Ok(());
            }
        };
        let n=tx.execute("UPDATE product_review_stages_v1 SET status=?1,output_json=?2 WHERE order_id=?3 AND run_id=?4 AND sequence=?5 AND stage_id=?6 AND status='accepted'",params![status,output,request.intent.order.0,request.journal_run,request.sequence as i64,request.stage_id]).map_err(db_error)?;
        if n != 1 {
            return Err(HostError::Worker(
                "product review stage claim missing or finalized".into(),
            ));
        }
        if let Some(evidence) = artifact.as_ref() {
            register_product_artifact_in(
                &tx,
                ProductArtifactRegistration {
                    config: cfg,
                    root_path: self
                        .artifact_root
                        .as_deref()
                        .ok_or_else(|| unsupported("artifact root is not configured"))?,
                    root_handle: self
                        .artifact_root_handle
                        .as_deref()
                        .ok_or_else(|| unsupported("artifact root handle is not configured"))?,
                    intent: &request.intent,
                    run_id: &request.journal_run,
                    stage_sequence: request.sequence,
                    stage_id: &request.stage_id,
                    evidence,
                    now: self.clock.now(),
                },
            )?;
        }
        if let Some(s) = session {
            tx.execute("INSERT OR REPLACE INTO product_review_sessions_v1(order_id,run_id,session_id,receipt_json) VALUES(?1,?2,?3,?4)",params![request.intent.order.0,request.journal_run,s.session_key,json(&ReviewSessionReceipt{session_id:s.session_id,output:s.output,sha256:s.sha256})?]).map_err(db_error)?;
        }
        tx.commit().map_err(db_error)
    }
    fn fail_review_dispatch(&mut self, request: ReviewDispatchFailure) -> Result<(), HostError> {
        let cfg = self
            .review_config
            .as_ref()
            .ok_or_else(|| unsupported("review roots are not configured"))?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        ensure_active_fence(&tx, &request.intent, &request.journal_run, &cfg.pack_digest)?;
        let n=tx.execute("UPDATE product_review_runs_v1 SET status='failed',counters_json=?1 WHERE order_id=?2 AND attempt=?3 AND run_id=?4 AND status='running'",params![json(&request.counters)?,request.intent.order.0,request.intent.attempt.0 as i64,request.journal_run]).map_err(db_error)?;
        if n != 1 {
            return Err(HostError::Worker(
                "product review failure outside active run".into(),
            ));
        }
        tx.execute(
            "INSERT INTO product_review_failures_v1(order_id,run_id,error) VALUES(?1,?2,?3)",
            params![request.intent.order.0, request.journal_run, request.error],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)
    }
    fn complete_review_dispatch(
        &mut self,
        request: ReviewDispatchCompletion,
    ) -> Result<(), HostError> {
        let cfg = self
            .review_config
            .as_ref()
            .ok_or_else(|| unsupported("review roots are not configured"))?;
        validate_terminal_payload(&request, &cfg.pack_digest)?;
        let clock = self.clock.clone();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let now = clock.now();
        let run_binding =
            ensure_active_fence(&tx, &request.intent, &request.journal_run, &cfg.pack_digest)?;
        validate_terminal_run_binding(&request, &run_binding)?;
        let admitted = (
            run_binding.lifecycle,
            run_binding.source_revision,
            run_binding.repository_id,
            run_binding.admission_hash,
            run_binding.admission_seq,
        );
        let (status, hold_json, hold_id, deadline) = match request.status {
            ReviewTerminalStatus::Passed => ("passed", None, None, None),
            ReviewTerminalStatus::Failed => ("failed", None, None, None),
            ReviewTerminalStatus::Held => {
                let s = request
                    .held_snapshot
                    .as_ref()
                    .ok_or_else(|| db_error("held result lacks frozen snapshot"))?;
                let saved = hold_snapshot(s, &request.intent, &request.journal_run)?;
                let id = s.hold.hold_instance_id.clone();
                let deadline = if s.hold.deadline_secs == 0 {
                    None
                } else {
                    Some(now.timestamp().saturating_add(s.hold.deadline_secs as i64))
                };
                ("held", Some(saved), Some(id), deadline)
            }
        };
        let result = json(&request.result)?;
        let counters = json(&request.counters)?;
        let provenance = json(&request.provenance)?;
        let n=tx.execute("UPDATE product_review_runs_v1 SET status=?1,counters_json=?2,hold_json=?3,hold_instance_id=?4,deadline=?5,result_json=?6,provenance_json=?7 WHERE order_id=?8 AND attempt=?9 AND run_id=?10 AND status='running'",params![status,counters,hold_json,hold_id,deadline,result,provenance,request.intent.order.0,request.intent.attempt.0 as i64,request.journal_run]).map_err(db_error)?;
        if n != 1 {
            return Err(HostError::Worker(
                "terminal result outside active product run".into(),
            ));
        }
        tx.execute("INSERT INTO product_review_host_results_v1(order_id,attempt,run_id,activation,result_json) VALUES(?1,?2,?3,?4,?5)",params![request.intent.order.0,request.intent.attempt.0 as i64,request.intent.run.0 as i64,request.intent.activation as i64,result]).map_err(db_error)?;
        if let (Some(snapshot), Some(saved), Some(hold_id)) =
            (request.held_snapshot.as_ref(), hold_json, hold_id)
        {
            let (lifecycle, source_revision, _, admission_hash, admission_seq) = admitted;
            let predecessor: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(seq),0) FROM ledger WHERE order_id=?1",
                    [&request.intent.order.0],
                    |r| r.get(0),
                )
                .map_err(db_error)?;
            let digest = super::product_review_snapshot_digest(&saved);
            let deadline_text = deadline
                .map(|value| {
                    chrono::DateTime::from_timestamp(value, 0)
                        .map(|time| time.to_rfc3339())
                        .ok_or_else(|| db_error("hold deadline out of range"))
                })
                .transpose()?
                .unwrap_or_default();
            tx.execute("INSERT INTO product_review_holds_v1(order_id,lifecycle,attempt,originating_run,activation,host_epoch,hold_instance_id,hold_revision,expected_predecessor,snapshot_digest,snapshot_json,allowed_targets_json,deadline,source_revision,admission_seq,admission_hash,state) VALUES(?1,?2,?3,?4,?5,?6,?7,1,?8,?9,?10,?11,?12,?13,?14,?15,'active')",params![request.intent.order.0,lifecycle,request.intent.attempt.0 as i64,request.intent.run.0 as i64,request.intent.activation as i64,request.intent.epoch.0 as i64,hold_id,predecessor,digest,saved,json(&snapshot.hold.resume_targets)?,deadline_text,source_revision,admission_seq,admission_hash]).map_err(db_error)?;
        }
        // A frozen hold is a pause in the same authorized review work, not a
        // terminal delivery. Keep its durable service/work claim live so an
        // authenticated Resume can revalidate the exact original owner,
        // epoch, fence, source and lease at each later stage reservation.
        // Terminal Passed/Failed runs alone complete the inbox backlog.
        if status != "held" {
            let completed_claim = tx.execute(
            "UPDATE product_work_claims_v1 SET state='completed',lease_until=?1
             WHERE order_id=?2 AND lifecycle=?3 AND owner_id=(SELECT worker_id FROM product_review_work_bindings_v1 WHERE order_id=?2 AND attempt=?4 AND run_id=?5)
               AND epoch=(SELECT claim_epoch FROM product_review_work_bindings_v1 WHERE order_id=?2 AND attempt=?4 AND run_id=?5)
               AND fence=(SELECT claim_fence FROM product_review_work_bindings_v1 WHERE order_id=?2 AND attempt=?4 AND run_id=?5)
               AND state='claimed'",
            params![now.to_rfc3339(),request.intent.order.0,run_binding.lifecycle,request.intent.attempt.0 as i64,request.journal_run],
        ).map_err(db_error)?;
            if completed_claim != 1 {
                return Err(HostError::Worker(
                    "product work claim changed before terminal commit".into(),
                ));
            }
            let completed_backlog = tx.execute(
            "UPDATE product_work_backlog_v1 SET state='completed'
             WHERE order_id=?1 AND lifecycle=?2 AND state='claimed'
               AND delivery_id=(SELECT delivery_id FROM product_review_work_bindings_v1 WHERE order_id=?1 AND attempt=?3 AND run_id=?4)",
            params![request.intent.order.0,run_binding.lifecycle,request.intent.attempt.0 as i64,request.journal_run],
        ).map_err(db_error)?;
            if completed_backlog != 1 {
                return Err(HostError::Worker(
                    "product work backlog changed before terminal commit".into(),
                ));
            }
        }
        tx.commit().map_err(db_error)
    }
}

impl ReviewHostStore for ProductStore {
    fn pack_digest(&self) -> &str {
        &self
            .review_config
            .as_ref()
            .expect("product review backend must be configured")
            .pack_digest
    }
    fn run_root(&self, run_id: &str) -> Result<PathBuf, HostError> {
        let c = self
            .review_config
            .as_ref()
            .ok_or_else(|| unsupported("review roots are not configured"))?;
        Ok(c.artifact_root
            .join(format!("{:x}", Sha256::digest(run_id.as_bytes()))))
    }
    fn claim_resume(&self, run_id: &str) -> Result<Option<File>, HostError> {
        let c = self
            .review_config
            .as_ref()
            .ok_or_else(|| unsupported("review roots are not configured"))?;
        fs::create_dir_all(&c.claim_root).map_err(db_error)?;
        let p = c
            .claim_root
            .join(format!("{:x}.lock", Sha256::digest(run_id.as_bytes())));
        let f = File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(p)
            .map_err(db_error)?;
        match f.try_lock() {
            Ok(()) => Ok(Some(f)),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(db_error(e)),
        }
    }
    fn resolved_attempt(&self, order: &str, attempt: u64) -> Result<bool, HostError> {
        self.conn.query_row("SELECT EXISTS(SELECT 1 FROM product_review_runs_v1 WHERE order_id=?1 AND attempt=?2 AND status IN ('resolved','review_aborted'))",params![order,attempt as i64],|r|r.get(0)).map_err(db_error)
    }
    fn session_receipt(
        &self,
        order: &str,
        session_id: &str,
    ) -> Result<Option<ReviewSessionReceipt>, HostError> {
        self.conn.query_row("SELECT receipt_json FROM product_review_sessions_v1 WHERE order_id=?1 AND session_id=?2 ORDER BY rowid DESC LIMIT 1",params![order,session_id],|r|r.get::<_,String>(0)).optional().map_err(db_error)?.map(|s|decode(&s)).transpose()
    }
    fn invalidate_terminal_runs(&mut self, _order: &str) -> Result<(), HostError> {
        Err(unsupported(
            "terminal run invalidation requires product recovery fencing",
        ))
    }
    fn redrive_payload(
        &self,
        order: &str,
        run: &str,
        payload: &str,
    ) -> Result<Vec<foundry_exec::ReviewRedriveArtifact>, HostError> {
        let mut stmt=self.conn.prepare("SELECT stage_id,status,output_json FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?2 ORDER BY sequence DESC").map_err(db_error)?;
        let rows = stmt
            .query_map(params![order, run], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(db_error)?;
        let mut seen = std::collections::BTreeSet::new();
        let mut artifacts = Vec::new();
        for row in rows {
            let (stage, status, encoded) = row.map_err(db_error)?;
            if !seen.insert(stage.clone()) || status != "complete" {
                continue;
            }
            let value: serde_json::Value =
                decode(&encoded.ok_or_else(|| db_error("completed product stage lacks output"))?)?;
            let matches = if payload == "findings" {
                value["role"] == "verify" && value["findings"] == true
            } else {
                stage == payload
            };
            if matches {
                artifacts.push(foundry_exec::ReviewRedriveArtifact {
                    stage_id: stage,
                    verdicts: serde_json::from_value(value["verdicts"].clone()).unwrap_or_default(),
                    log_path: value["log_path"]
                        .as_str()
                        .ok_or_else(|| db_error("completed product stage lacks log path"))?
                        .to_owned(),
                });
            }
        }
        artifacts.reverse();
        Ok(artifacts)
    }
    fn charge_budget(&mut self, order: &str, intent: &ExecutionIntent) -> Result<(), HostError> {
        #[cfg(any(test, feature = "p2-test-support"))]
        let observer = self.charge_budget_observer.clone();
        #[cfg(any(test, feature = "p2-test-support"))]
        if let Some(observer) = observer.as_ref() {
            ACTIVE_CHARGE_BUDGET_OBSERVER.with(|slot| {
                *slot.borrow_mut() = Some(observer.clone());
            });
            CHARGE_BUDGET_WAIT_STARTED.with(|slot| slot.set(Some(std::time::Instant::now())));
            self.conn
                .busy_handler(Some(charge_budget_busy_handler))
                .map_err(db_error)?;
        }
        let cfg = self
            .review_config
            .as_ref()
            .ok_or_else(|| unsupported("review roots are not configured"))?;
        #[cfg(any(test, feature = "p2-test-support"))]
        let result = charge_budget_transaction(
            &mut self.conn,
            self.clock.as_ref(),
            &cfg.pack_digest,
            order,
            intent,
            observer.as_ref(),
        );
        #[cfg(not(any(test, feature = "p2-test-support")))]
        let result = charge_budget_transaction(
            &mut self.conn,
            self.clock.as_ref(),
            &cfg.pack_digest,
            order,
            intent,
            None,
        );
        #[cfg(any(test, feature = "p2-test-support"))]
        if observer.is_some() {
            // `busy_handler` replaces rusqlite's configured timeout handler.
            // Restore the exact ProductStore timeout once the transaction has
            // committed or rolled back so later operations retain normal flow.
            let restore = self.conn.busy_timeout(std::time::Duration::from_secs(5));
            ACTIVE_CHARGE_BUDGET_OBSERVER.with(|slot| *slot.borrow_mut() = None);
            CHARGE_BUDGET_WAIT_STARTED.with(|slot| slot.set(None));
            restore.map_err(db_error)?;
        }
        result
    }

    fn accepted(&self, intent: &ExecutionIntent) -> Result<bool, HostError> {
        self.conn.query_row("SELECT EXISTS(SELECT 1 FROM product_review_host_accept_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND activation=?4)",params![intent.order.0,intent.attempt.0 as i64,intent.run.0 as i64,intent.activation as i64],|r|r.get(0)).map_err(db_error)
    }
    fn latest_run(
        &self,
        order: &str,
        attempt: u64,
    ) -> Result<Option<foundry_exec::RunRecord>, HostError> {
        let row: Option<LatestRunRow> = self.conn.query_row("SELECT run_id,base_commit,result_commit,status,hold_json,hold_instance_id FROM product_review_runs_v1 WHERE order_id=?1 AND attempt=?2 ORDER BY created_at DESC,rowid DESC LIMIT 1",params![order,attempt as i64],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional().map_err(db_error)?;
        let Some((
            run_id,
            base_commit,
            result_commit,
            status,
            _private_hold_json,
            hold_instance_id,
        )) = row
        else {
            return Ok(None);
        };
        let n = run_id
            .split(':')
            .nth_back(1)
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        Ok(Some(foundry_exec::RunRecord {
            run: n,
            base_commit,
            result_commit,
            status,
            hold_json: None,
            hold_instance_id,
        }))
    }
    fn mark_hold_expired(
        &mut self,
        _request: foundry_exec::ReviewHoldUpdate,
    ) -> Result<(), HostError> {
        Err(unsupported("product hold continuation is not yet wired"))
    }
    fn begin_hold_resume(
        &mut self,
        _request: foundry_exec::ReviewHoldUpdate,
    ) -> Result<(), HostError> {
        Err(unsupported("product hold continuation is not yet wired"))
    }
    fn restore_hold_after_resume_error(
        &mut self,
        binding: ReviewRunBinding,
    ) -> Result<(), HostError> {
        let Some(claim) = self.active_review_claim.clone() else {
            return Err(unsupported("accepted hold recovery has no durable claim"));
        };
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        ensure_product_claim(&tx, &claim)?;
        ensure_product_claim_for_run(
            &tx,
            &claim,
            &binding.order_id,
            binding.attempt as i64,
            &binding.journal_run,
        )?;
        let n=tx.execute("UPDATE product_review_hold_progress_v1 SET phase='recovery_required',progress_revision=progress_revision+1,recovery_reason='continuation interrupted after accepted decision' WHERE operation_id=?1 AND owner_id=?2 AND epoch=?3 AND fence=?4",params![claim.operation_id,claim.owner_id,claim.epoch,claim.fence]).map_err(db_error)?;
        if n != 1 {
            return Err(HostError::Worker(
                "accepted hold recovery fence changed".into(),
            ));
        }
        tx.commit().map_err(db_error)?;
        self.active_review_claim = None;
        self.active_review_claim_lock = None;
        Ok(())
    }
    fn resolve_hold_result(
        &mut self,
        _request: foundry_exec::ReviewHoldResolution,
    ) -> Result<(), HostError> {
        Err(unsupported(
            "product human decisions use accept_product_review_hold",
        ))
    }
    fn save_redrive_output(
        &mut self,
        _order: &str,
        _intent: &ExecutionIntent,
        _run: &str,
        _sequence: u64,
        _stage_id: &str,
        _session_key: &str,
        _session_id: &str,
        _output: &ReviewStageEvidence,
    ) -> Result<(), HostError> {
        Err(unsupported("product redrive persistence is not configured"))
    }
    fn continuation_binding(
        &self,
        order: &str,
        attempt: i64,
        run: &str,
    ) -> Result<ReviewContinuationBinding, HostError> {
        let claim = self
            .active_review_claim
            .as_ref()
            .ok_or_else(|| unsupported("accepted continuation has no active durable claim"))?;
        let conn = &self.conn;
        let progress: Option<(String,String,i64,i64,i64)> = conn.query_row(
            "SELECT phase,owner_id,epoch,fence,reserved_run_id FROM product_review_hold_progress_v1 WHERE operation_id=?1",
            [&claim.operation_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
        ).optional().map_err(db_error)?;
        if !progress.is_some_and(|(phase, owner, epoch, fence, reserved)| {
            phase == "continuing"
                && owner == claim.owner_id
                && epoch == claim.epoch
                && fence == claim.fence
                && reserved == claim.reserved_run_id
        }) {
            return Err(HostError::Worker(
                "accepted continuation fence changed".into(),
            ));
        }
        ensure_product_claim_for_run(conn, claim, order, attempt, run)?;
        let (pack, sequence, result): (String, i64, String) = conn.query_row(
            "SELECT pack_digest,COALESCE((SELECT MAX(sequence) FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?2),0),result_json FROM product_review_runs_v1 WHERE order_id=?1 AND attempt=?3 AND run_id=?2 AND status='resuming'",
            params![order, run, attempt], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
        ).map_err(db_error)?;
        Ok(ReviewContinuationBinding {
            pack_digest: pack,
            next_sequence: sequence.max(0) as u64,
            stored_result: decode(&result)?,
        })
    }
    fn continuation_counters(
        &mut self,
        request: ReviewCounterCheckpoint,
        pack: &str,
    ) -> Result<(), HostError> {
        let claim = self
            .active_review_claim
            .clone()
            .ok_or_else(|| unsupported("accepted continuation has no active durable claim"))?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        ensure_product_claim(&tx, &claim)?;
        ensure_product_claim_for_run(
            &tx,
            &claim,
            &request.intent.order.0,
            request.intent.attempt.0 as i64,
            &request.journal_run,
        )?;
        ensure_product_work_owner_fence_in(
            &tx,
            &request.intent.order.0,
            request.intent.attempt.0 as i64,
            &request.journal_run,
        )?;
        ensure_resuming_fence(&tx, &request.intent, &request.journal_run, pack)?;
        let n=tx.execute("UPDATE product_review_runs_v1 SET counters_json=?1 WHERE order_id=?2 AND attempt=?3 AND run_id=?4 AND status='resuming'",params![json(&request.counters)?,request.intent.order.0,request.intent.attempt.0 as i64,request.journal_run]).map_err(db_error)?;
        if n != 1 {
            return Err(HostError::Worker(
                "accepted continuation run changed during checkpoint".into(),
            ));
        }
        tx.commit().map_err(db_error)
    }
    fn continuation_session(
        &self,
        order: &str,
        session_id: &str,
    ) -> Result<Option<ReviewSessionReceipt>, HostError> {
        self.session_receipt(order, session_id)
    }
    fn continuation_budget_and_start(
        &mut self,
        request: ReviewStageStart,
        pack: &str,
    ) -> Result<(), HostError> {
        let claim = self
            .active_review_claim
            .clone()
            .ok_or_else(|| unsupported("accepted continuation has no active durable claim"))?;
        if request.sequence > i64::MAX as u64 {
            return Err(db_error("continuation sequence overflow"));
        }
        #[cfg(any(test, feature = "p2-test-support"))]
        if let Some(observer) = self.charge_budget_observer.as_ref() {
            notify_charge_budget(observer, ChargeBudgetObservation::StageStartBeginAttempt);
        }
        let clock = self.clock.clone();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let now = clock.now();
        ensure_product_claim(&tx, &claim)?;
        // A Resume receipt records who accepted this hold, but that historical
        // decision is not a perpetual execution grant. Recheck the original
        // human authority in the same transaction that reserves each new
        // external stage. This deliberately runs before the independent
        // DriveReview service check below so a stale human acceptance cannot
        // be masked by service or worker credential failures.
        ensure_current_acceptance_authority_in(&tx, &claim.operation_id)?;
        ensure_product_claim_for_run(
            &tx,
            &claim,
            &request.intent.order.0,
            request.intent.attempt.0 as i64,
            &request.journal_run,
        )?;
        ensure_product_work_owner_fence_in(
            &tx,
            &request.intent.order.0,
            request.intent.attempt.0 as i64,
            &request.journal_run,
        )?;
        let binding = ensure_resuming_fence(&tx, &request.intent, &request.journal_run, pack)?;
        ensure_product_source_for_new_stage(
            &tx,
            &request.intent.order.0,
            request.intent.attempt.0 as i64,
            &request.journal_run,
            &binding,
            now,
        )?;
        let (used,max_seq):(i64,Option<i64>)=tx.query_row("SELECT COALESCE((SELECT used FROM product_review_budget_v1 WHERE order_id=?1),0), (SELECT MAX(sequence) FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?2)",params![&request.intent.order.0,&request.journal_run],|r|Ok((r.get(0)?,r.get(1)?))).map_err(db_error)?;
        if max_seq.is_some_and(|previous| request.sequence as i64 <= previous) {
            return Err(HostError::Worker(
                "continuation sequence is not after the persisted cursor".into(),
            ));
        }
        if used >= request.budget_limit as i64 {
            return Err(HostError::Worker("product review budget exhausted".into()));
        }
        tx.execute("INSERT INTO product_review_budget_v1(order_id,used) VALUES(?1,1) ON CONFLICT(order_id) DO UPDATE SET used=used+1",[&request.intent.order.0]).map_err(db_error)?;
        tx.execute("INSERT INTO product_review_stages_v1(order_id,run_id,sequence,stage_id,status) VALUES(?1,?2,?3,?4,'accepted')",params![request.intent.order.0,request.journal_run,request.sequence as i64,request.stage_id]).map_err(db_error)?;
        tx.commit().map_err(db_error)
    }
    fn finish_continuation_stage(
        &mut self,
        request: ReviewStageFinishRequest,
        pack: &str,
    ) -> Result<(), HostError> {
        let claim = self
            .active_review_claim
            .clone()
            .ok_or_else(|| unsupported("accepted continuation has no active durable claim"))?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        ensure_product_claim(&tx, &claim)?;
        ensure_product_claim_for_run(
            &tx,
            &claim,
            &request.intent.order.0,
            request.intent.attempt.0 as i64,
            &request.journal_run,
        )?;
        ensure_product_work_owner_fence_in(
            &tx,
            &request.intent.order.0,
            request.intent.attempt.0 as i64,
            &request.journal_run,
        )?;
        ensure_resuming_fence(&tx, &request.intent, &request.journal_run, pack)?;
        let (status, output, session, artifact) = match request.finish {
            ReviewStageFinish::Complete(e) => {
                let output = stage_output(&e)?;
                ("complete", output, e.session.clone(), Some(e))
            }
            ReviewStageFinish::Error(e) => ("error", e, None, None),
            ReviewStageFinish::Blocked(_) => {
                return Err(HostError::Worker(
                    "continuation stage cannot be blocked after a durable claim".into(),
                ));
            }
        };
        let n=tx.execute("UPDATE product_review_stages_v1 SET status=?1,output_json=?2 WHERE order_id=?3 AND run_id=?4 AND sequence=?5 AND stage_id=?6 AND status='accepted'",params![status,output,request.intent.order.0,request.journal_run,request.sequence as i64,request.stage_id]).map_err(db_error)?;
        if n != 1 {
            return Err(HostError::Worker(
                "continuation stage claim is missing or already finalized".into(),
            ));
        }
        if let Some(evidence) = artifact.as_ref() {
            register_product_artifact_in(
                &tx,
                ProductArtifactRegistration {
                    config: self
                        .review_config
                        .as_ref()
                        .ok_or_else(|| unsupported("artifact root is not configured"))?,
                    root_path: self
                        .artifact_root
                        .as_deref()
                        .ok_or_else(|| unsupported("artifact root is not configured"))?,
                    root_handle: self
                        .artifact_root_handle
                        .as_deref()
                        .ok_or_else(|| unsupported("artifact root handle is not configured"))?,
                    intent: &request.intent,
                    run_id: &request.journal_run,
                    stage_sequence: request.sequence,
                    stage_id: &request.stage_id,
                    evidence,
                    now: self.clock.now(),
                },
            )?;
        }
        if let Some(s) = session {
            tx.execute("INSERT INTO product_review_sessions_v1(order_id,run_id,session_id,receipt_json) VALUES(?1,?2,?3,?4) ON CONFLICT(order_id,run_id,session_id) DO UPDATE SET receipt_json=excluded.receipt_json",params![request.intent.order.0,request.journal_run,s.session_key,json(&ReviewSessionReceipt{session_id:s.session_id,output:s.output,sha256:s.sha256})?]).map_err(db_error)?;
        }
        tx.commit().map_err(db_error)
    }
    fn complete_continuation(
        &mut self,
        request: ReviewContinuationCompletion,
    ) -> Result<(), HostError> {
        let claim = self
            .active_review_claim
            .clone()
            .ok_or_else(|| unsupported("accepted continuation has no active durable claim"))?;
        if request.binding.order_id != request.intent.order.0
            || request.binding.attempt != request.intent.attempt.0
            || request.snapshot.binding != request.binding
            || request.snapshot.intent != request.intent
            || request.pack_digest != self.pack_digest()
        {
            return Err(HostError::Worker(
                "product continuation completion binding mismatch".into(),
            ));
        }
        if request.result.held.is_some()
            && request.result.held.as_ref() != Some(&request.snapshot.held.hold)
        {
            return Err(HostError::Worker(
                "continued hold snapshot does not match result".into(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        ensure_product_claim(&tx, &claim)?;
        ensure_product_claim_for_run(
            &tx,
            &claim,
            &request.intent.order.0,
            request.intent.attempt.0 as i64,
            &request.binding.journal_run,
        )?;
        ensure_product_work_owner_fence_in(
            &tx,
            &request.intent.order.0,
            request.intent.attempt.0 as i64,
            &request.binding.journal_run,
        )?;
        let run_binding = ensure_resuming_fence(
            &tx,
            &request.intent,
            &request.binding.journal_run,
            &request.pack_digest,
        )?;
        validate_continuation_result_in(&tx, &request, &run_binding)?;
        let hold_id = request
            .result
            .held
            .as_ref()
            .map(|h| h.hold_instance_id.clone());
        let hold_json = encode_continuation_snapshot(&request.snapshot)?;
        let result_json = json(&request.result)?;
        let status = if request.result.held.is_some() {
            "held"
        } else {
            "resolved"
        };
        let deadline = request.deadline;
        let n=tx.execute("UPDATE product_review_runs_v1 SET status=?1,counters_json=?2,hold_json=?3,hold_instance_id=?4,deadline=?5,result_json=?6 WHERE order_id=?7 AND attempt=?8 AND run_id=?9 AND status='resuming'",params![status,json(&request.counters)?,hold_json,hold_id,deadline,result_json,request.binding.order_id,request.binding.attempt as i64,request.binding.journal_run]).map_err(db_error)?;
        if n != 1 {
            return Err(HostError::Worker(
                "continuation run changed before completion".into(),
            ));
        }
        let n=tx.execute("UPDATE product_review_host_results_v1 SET result_json=?1 WHERE order_id=?2 AND attempt=?3 AND run_id=?4 AND activation=?5",params![result_json,request.intent.order.0,request.intent.attempt.0 as i64,request.intent.run.0 as i64,request.intent.activation as i64]).map_err(db_error)?;
        if n != 1 {
            return Err(HostError::Persistence(
                "product continuation result binding is missing".into(),
            ));
        }
        if let Some(hold) = request.result.held.as_ref() {
            let acceptance_json: String = tx.query_row(
                "SELECT a.receipt_json FROM product_review_hold_acceptances_v1 a JOIN product_review_run_reservations_v1 q ON q.operation_id=a.operation_id WHERE a.operation_id=?1 AND q.followup_run_id=?2",
                params![claim.operation_id, request.intent.run.0 as i64],
                |r| r.get(0),
            ).map_err(db_error)?;
            let accepted: ProductHoldAcceptance = decode(&acceptance_json)?;
            let (run_lifecycle,run_source_revision,run_admission_seq,run_admission_hash):(i64,i64,i64,String) = tx.query_row(
                "SELECT product_lifecycle,source_revision,admission_seq,admission_hash FROM product_review_runs_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3",
                params![accepted.order_id, accepted.attempt, request.binding.journal_run],
                |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
            ).map_err(db_error)?;
            let (origin_admission_seq,origin_admission_hash):(i64,String) = tx.query_row(
                "SELECT admission_seq,admission_hash FROM product_review_holds_v1 WHERE order_id=?1 AND lifecycle=?2 AND attempt=?3 AND originating_run=?4 AND hold_instance_id=?5",
                params![accepted.order_id,accepted.lifecycle,accepted.attempt,accepted.originating_run,accepted.hold_instance_id],
                |r| Ok((r.get(0)?,r.get(1)?)),
            ).map_err(db_error)?;
            let (current_lifecycle,current_source_revision,current_admission_seq,current_admission_hash):(i64,i64,i64,String) = tx.query_row(
                "SELECT lifecycle,source_revision,admission_seq,admission_hash FROM product_orders_v1 WHERE order_id=?1",
                [&accepted.order_id],
                |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
            ).map_err(db_error)?;
            let (origin_digest,origin_snapshot,origin_state):(String,String,String) = tx.query_row(
                "SELECT snapshot_digest,snapshot_json,state FROM product_review_holds_v1 WHERE order_id=?1 AND lifecycle=?2 AND attempt=?3 AND originating_run=?4 AND hold_instance_id=?5",
                params![accepted.order_id,accepted.lifecycle,accepted.attempt,accepted.originating_run,accepted.hold_instance_id],
                |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
            ).map_err(db_error)?;
            let origin_binding = ReviewRunBinding {
                order_id: accepted.order_id.clone(),
                attempt: accepted.attempt as u64,
                journal_run: format!(
                    "{}:{}:{}:{}",
                    accepted.order_id,
                    accepted.attempt,
                    accepted.originating_run,
                    accepted.activation
                ),
            };
            let original_snapshot = decode_hold(origin_binding, &origin_snapshot)?;
            if accepted.order_id != request.intent.order.0
                || accepted.attempt != request.intent.attempt.0 as i64
                || accepted.reserved_run_id != request.intent.run.0 as i64
                || accepted.activation != request.intent.activation as i64
                || run_lifecycle != accepted.lifecycle
                || run_source_revision != accepted.source_revision
                || run_admission_seq != origin_admission_seq
                || run_admission_hash != origin_admission_hash
                || current_lifecycle != accepted.lifecycle
                || current_source_revision != accepted.source_revision
                || current_admission_seq != origin_admission_seq
                || current_admission_hash != origin_admission_hash
                || origin_state != "accepted"
                || super::product_review_snapshot_digest(&origin_snapshot) != origin_digest
                || request.snapshot.held.hold != *hold
                || request.snapshot.snapshot_run_id != original_snapshot.snapshot_run_id
            {
                return Err(HostError::Acceptance(
                    "repeated hold is not bound to the accepted followup run".into(),
                ));
            }
            let saved = encode_continuation_snapshot(&request.snapshot)?;
            let digest = super::product_review_snapshot_digest(&saved);
            let deadline_text = request
                .deadline
                .map(|value| {
                    chrono::DateTime::from_timestamp(value, 0)
                        .map(|time| time.to_rfc3339())
                        .ok_or_else(|| db_error("followup hold deadline out of range"))
                })
                .transpose()?
                .unwrap_or_default();
            tx.execute(
                "INSERT INTO product_review_holds_v1(order_id,lifecycle,attempt,originating_run,activation,host_epoch,hold_instance_id,hold_revision,expected_predecessor,snapshot_digest,snapshot_json,allowed_targets_json,deadline,source_revision,admission_seq,admission_hash,state) VALUES(?1,?2,?3,?4,?5,?6,?7,1,?8,?9,?10,?11,?12,?13,?14,?15,'active')",
                params![accepted.order_id,accepted.lifecycle,accepted.attempt,accepted.reserved_run_id,accepted.activation,request.intent.epoch.0 as i64,hold.hold_instance_id,accepted.predecessor_seq,digest,saved,json(&hold.resume_targets)?,deadline_text,accepted.source_revision,origin_admission_seq,origin_admission_hash],
            ).map_err(db_error)?;
        }
        tx.execute("UPDATE product_review_hold_progress_v1 SET phase='completed',progress_revision=progress_revision+1 WHERE operation_id=?1 AND phase='continuing' AND owner_id=?2 AND epoch=?3 AND fence=?4",params![claim.operation_id,claim.owner_id,claim.epoch,claim.fence]).map_err(db_error)?;
        if tx.changes() != 1 {
            return Err(HostError::Worker(
                "product continuation progress fence changed".into(),
            ));
        }
        tx.commit().map_err(db_error)?;
        self.active_review_claim = None;
        self.active_review_claim_lock = None;
        Ok(())
    }
    fn claim_accepted_hold(
        &mut self,
        session_digest: &str,
        operation_id: &str,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<foundry_exec::ReviewAcceptedHold, HostError> {
        let preliminary: String = self
            .conn
            .query_row(
                "SELECT receipt_json FROM product_review_hold_acceptances_v1 WHERE operation_id=?1",
                [operation_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| HostError::Acceptance("accepted product hold is unavailable".into()))?;
        let preliminary: ProductHoldAcceptance = decode(&preliminary)?;
        let claim_run_id = format!(
            "{}:{}:{}:{}",
            preliminary.order_id,
            preliminary.attempt,
            preliminary.reserved_run_id,
            preliminary.activation
        );
        let configured_pack = self
            .review_config
            .as_ref()
            .map(|config| config.pack_digest.clone())
            .ok_or_else(|| unsupported("pinned review pack is not configured"))?;
        let lock = self.claim_resume(&claim_run_id)?.ok_or_else(|| {
            HostError::Worker("accepted review continuation is already claimed".into())
        })?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        // This is a new-work authorization transaction as well as a human
        // decision claim. Use one fresh transaction instant for both checks;
        // the caller's HTTP/CLI timestamp must not extend service authority.
        let now = self.clock.now();
        let principal = resolve_session_in(&tx, session_digest, now)
            .map_err(|_| HostError::Acceptance("current review authority is unavailable".into()))?;
        if principal.role != "owner" && principal.role != "approver" {
            return Err(HostError::Acceptance(
                "current role cannot continue accepted review".into(),
            ));
        }
        let scoped_authority = if super::scope_enforced_in(&tx).map_err(db_error)? {
            Some(
                super::require_order_capability_in(
                    &tx,
                    &principal,
                    &preliminary.order_id,
                    super::ProductCapability::ResolveReviewHold,
                    false,
                )
                .map_err(|_| HostError::Acceptance("current review scope is unavailable".into()))?,
            )
        } else {
            None
        };
        let receipt_json: String = tx
            .query_row(
                "SELECT receipt_json FROM product_review_hold_acceptances_v1 WHERE operation_id=?1",
                [operation_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?
            .ok_or_else(|| {
                HostError::Acceptance("accepted product hold receipt is unavailable".into())
            })?;
        let receipt: ProductHoldAcceptance = decode(&receipt_json)?;
        let run_id = format!(
            "{}:{}:{}:{}",
            receipt.order_id, receipt.attempt, receipt.originating_run, receipt.activation
        );
        if receipt.operation_id != operation_id
            || receipt.actor_id != principal.actor_id
            || receipt.authority_revision != principal.authority_revision
            || receipt.source_instance_id
                != scoped_authority
                    .as_ref()
                    .map(|v| v.source_instance_id.clone())
                    .unwrap_or_default()
            || receipt.scope_revision
                != scoped_authority
                    .as_ref()
                    .map(|v| v.scope_revision)
                    .unwrap_or(-1)
            || receipt.grant_revision
                != scoped_authority
                    .as_ref()
                    .map(|v| v.grant_revision)
                    .unwrap_or(-1)
        {
            return Err(HostError::Acceptance(
                "accepted product hold authority binding changed".into(),
            ));
        }
        ensure_product_work_owner_fence_in(&tx, &receipt.order_id, receipt.attempt, &run_id)?;
        let progress:(String,i64,i64,i64,i64)=tx.query_row("SELECT phase,epoch,fence,reserved_run_id,progress_revision FROM product_review_hold_progress_v1 WHERE operation_id=?1",[operation_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).map_err(db_error)?;
        if progress.3 != receipt.reserved_run_id {
            return Err(HostError::Acceptance(
                "accepted followup run reservation binding changed".into(),
            ));
        }
        let reservation_matches: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM product_review_run_reservations_v1 WHERE operation_id=?1 AND order_id=?2 AND lifecycle=?3 AND attempt=?4 AND originating_run=?5 AND hold_instance_id=?6 AND followup_run_id=?7)",
            params![operation_id,receipt.order_id,receipt.lifecycle,receipt.attempt,receipt.originating_run,receipt.hold_instance_id,receipt.reserved_run_id],
            |r| r.get(0),
        ).map_err(db_error)?;
        if !reservation_matches {
            return Err(HostError::Acceptance(
                "accepted followup run reservation is inconsistent".into(),
            ));
        }
        if progress.0 == "completed" {
            return Err(HostError::Worker(
                "accepted product hold is already completed".into(),
            ));
        }
        if progress.0 == "recovery_required" {
            return Err(HostError::Worker(
                "accepted product continuation requires explicit recovery".into(),
            ));
        }
        let (lifecycle,originating_run,activation,host_epoch,hold_revision,predecessor,snapshot_digest,frozen_json,hold_source_revision,admission_seq,admission_hash,hold_state):(i64,i64,i64,i64,i64,i64,String,String,i64,i64,String,String)=tx.query_row(
            "SELECT lifecycle,originating_run,activation,host_epoch,hold_revision,expected_predecessor,snapshot_digest,snapshot_json,source_revision,admission_seq,admission_hash,state FROM product_review_holds_v1 WHERE order_id=?1 AND lifecycle=?2 AND attempt=?3 AND originating_run=?4 AND hold_instance_id=?5",
            params![receipt.order_id,receipt.lifecycle,receipt.attempt,receipt.originating_run,receipt.hold_instance_id],
            |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?,r.get(11)?)),
        ).optional().map_err(db_error)?.ok_or_else(||HostError::Acceptance("immutable accepted hold evidence is unavailable".into()))?;
        if lifecycle != receipt.lifecycle
            || originating_run != receipt.originating_run
            || activation != receipt.activation
            || host_epoch != receipt.host_epoch
            || hold_revision != receipt.hold_revision
            || predecessor != receipt.predecessor_seq
            || hold_source_revision != receipt.source_revision
            || hold_state != "accepted"
            || super::product_review_snapshot_digest(&frozen_json) != snapshot_digest
        {
            return Err(HostError::Acceptance(
                "immutable accepted hold coordinates or digest changed".into(),
            ));
        }
        let (source_revision,source_enabled,repository_id,admission_seq_now,admission_hash_now):(i64,i64,String,i64,String)=tx.query_row(
            "SELECT s.revision,s.enabled,o.repository_id,o.admission_seq,o.admission_hash FROM product_orders_v1 o JOIN product_sources_v1 s ON s.connector_id=o.connector_id WHERE o.order_id=?1 AND o.lifecycle=?2 AND o.source_revision=?3 AND s.repository_id=o.repository_id",
            params![receipt.order_id,receipt.lifecycle,receipt.source_revision],
            |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
        ).optional().map_err(db_error)?.ok_or_else(||HostError::Acceptance("accepted source binding is unavailable".into()))?;
        if source_revision != receipt.source_revision
            || source_enabled != 1
            || admission_seq_now != admission_seq
            || admission_hash_now != admission_hash
        {
            return Err(HostError::Acceptance(
                "accepted source revision or admission binding changed".into(),
            ));
        }
        let (journal_run,hold_json,run_status,run_source_revision,run_admission_seq,run_admission_hash,run_repository,run_pack):(String,String,String,i64,i64,String,String,String)=tx.query_row(
            "SELECT run_id,hold_json,status,source_revision,admission_seq,admission_hash,(SELECT repository_id FROM product_orders_v1 WHERE order_id=product_review_runs_v1.order_id),pack_digest FROM product_review_runs_v1 WHERE order_id=?1 AND product_lifecycle=?2 AND attempt=?3 AND hold_instance_id=?4",
            params![receipt.order_id,receipt.lifecycle,receipt.attempt,receipt.hold_instance_id],
            |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?)),
        ).optional().map_err(db_error)?.ok_or_else(||HostError::Acceptance("accepted originating run is unavailable".into()))?;
        if journal_run != run_id || !matches!(run_status.as_str(), "held" | "resuming") {
            return Err(HostError::Worker(
                "accepted product hold run binding is unavailable".into(),
            ));
        }
        let mut binding = ReviewRunBinding {
            order_id: receipt.order_id.clone(),
            attempt: receipt.attempt as u64,
            journal_run: journal_run.clone(),
        };
        let frozen = decode_hold(binding.clone(), &frozen_json)?;
        let mut snapshot = decode_hold(binding.clone(), &hold_json)?;
        if frozen.binding.journal_run != journal_run
            || frozen.intent.order.0 != receipt.order_id
            || frozen.intent.attempt.0 as i64 != receipt.attempt
            || frozen.intent.run.0 as i64 != receipt.originating_run
            || frozen.intent.activation as i64 != receipt.activation
            || frozen.intent.epoch.0 as i64 != receipt.host_epoch
            || frozen.held.hold.hold_instance_id != receipt.hold_instance_id
            || snapshot.binding != frozen.binding
            || snapshot.intent != frozen.intent
            || snapshot.snapshot_run_id != frozen.snapshot_run_id
            || snapshot.held != frozen.held
            || run_source_revision != receipt.source_revision
            || run_admission_seq != admission_seq
            || run_admission_hash != admission_hash
            || run_repository != repository_id
            || run_pack != configured_pack
        {
            return Err(HostError::Acceptance(
                "accepted evolving snapshot does not match immutable origin evidence".into(),
            ));
        }
        let action = match &receipt.decision {
            ProductHoldDecision::Resume(target) => foundry_exec::ReviewHoldAction::Resume {
                target: target.clone(),
            },
            ProductHoldDecision::CommitPass => foundry_exec::ReviewHoldAction::CommitPass,
            ProductHoldDecision::CommitFail => foundry_exec::ReviewHoldAction::CommitFail,
            ProductHoldDecision::Abort => foundry_exec::ReviewHoldAction::Abort,
        };
        if matches!(action, foundry_exec::ReviewHoldAction::Resume { .. }) {
            let service_valid = super::service::review_work_service_valid_in(
                &tx,
                &receipt.order_id,
                receipt.attempt,
                &run_id,
                now,
            )
            .map_err(|_| {
                HostError::Acceptance("review service authority is absent or expired".into())
            })?;
            if !service_valid {
                return Err(HostError::Acceptance(
                    "review service authority is absent or expired".into(),
                ));
            }
        }
        if let foundry_exec::ReviewHoldAction::Resume { target } = &action {
            if !snapshot
                .held
                .hold
                .resume_targets
                .iter()
                .any(|t| t == target)
            {
                return Err(HostError::Acceptance(
                    "accepted continuation target is not in the frozen hold".into(),
                ));
            }
            if run_status == "resuming" {
                let ambiguous:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM product_review_stages_v1 WHERE order_id=?1 AND run_id=?2 AND sequence>?3)",params![receipt.order_id,journal_run,snapshot.held.sequence as i64],|r|r.get(0)).map_err(db_error)?;
                if ambiguous {
                    let changed = tx.execute(
                        "UPDATE product_review_hold_progress_v1 SET phase='recovery_required',progress_revision=progress_revision+1,recovery_reason='ambiguous continuation stage after accepted decision' WHERE operation_id=?1 AND phase=?2 AND epoch=?3 AND fence=?4 AND reserved_run_id=?5",
                        params![operation_id,progress.0,progress.1,progress.2,receipt.reserved_run_id],
                    ).map_err(db_error)?;
                    if changed != 1 {
                        return Err(HostError::Worker(
                            "ambiguous continuation recovery fence changed".into(),
                        ));
                    }
                    tx.commit().map_err(db_error)?;
                    return Err(HostError::Worker("accepted continuation has an ambiguous in-flight stage and requires recovery".into()));
                }
            }
        }
        let action_event = match &action {
            foundry_exec::ReviewHoldAction::Resume { target } => {
                foundry_exec::ReviewHumanAction::Resume {
                    target: target.clone(),
                }
            }
            foundry_exec::ReviewHoldAction::CommitPass => {
                foundry_exec::ReviewHumanAction::CommitPass
            }
            foundry_exec::ReviewHoldAction::CommitFail => {
                foundry_exec::ReviewHumanAction::CommitFail
            }
            foundry_exec::ReviewHoldAction::Abort => foundry_exec::ReviewHumanAction::Abort,
        };
        if !snapshot.human_events.iter().any(|event| {
            event.actor == receipt.actor_id
                && event.occurred_at == receipt.accepted_at
                && event.action == action_event
        }) {
            snapshot.human_events.push(foundry_exec::ReviewHumanEvent {
                actor: receipt.actor_id.clone(),
                occurred_at: receipt.accepted_at.clone(),
                action: action_event,
            });
        }
        if run_status == "held" {
            snapshot.resume_sequence = snapshot
                .resume_sequence
                .checked_add(1)
                .ok_or_else(|| db_error("resume sequence exhausted"))?;
        }
        let resume = matches!(action, foundry_exec::ReviewHoldAction::Resume { .. });
        if resume {
            let followup_run = receipt.reserved_run_id;
            let followup_journal = format!(
                "{}:{}:{}:{}",
                receipt.order_id, receipt.attempt, followup_run, receipt.activation
            );
            let existing: Option<String> = tx.query_row(
                "SELECT status FROM product_review_runs_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3",
                params![receipt.order_id, receipt.attempt, followup_journal],
                |r| r.get(0),
            ).optional().map_err(db_error)?;
            if existing.is_some() {
                let changed = tx.execute(
                    "UPDATE product_review_hold_progress_v1 SET phase='recovery_required',progress_revision=progress_revision+1,recovery_reason='reserved continuation run already materialized; execution outcome requires recovery' WHERE operation_id=?1 AND phase=?2 AND epoch=?3 AND fence=?4 AND reserved_run_id=?5",
                    params![operation_id, progress.0, progress.1, progress.2, receipt.reserved_run_id],
                ).map_err(db_error)?;
                if changed != 1 {
                    return Err(HostError::Worker(
                        "materialized continuation recovery fence changed".into(),
                    ));
                }
                tx.commit().map_err(db_error)?;
                return Err(HostError::Worker(
                    "reserved continuation run already exists and requires explicit recovery"
                        .into(),
                ));
            }
            let original_result: String = tx.query_row(
                "SELECT result_json FROM product_review_runs_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3",
                params![receipt.order_id, receipt.attempt, journal_run],
                |r| r.get(0),
            ).map_err(db_error)?;
            let original_host_result: String = tx.query_row(
                "SELECT result_json FROM product_review_host_results_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND activation=?4",
                params![receipt.order_id, receipt.attempt, receipt.originating_run, receipt.activation],
                |r| r.get(0),
            ).map_err(db_error)?;
            let original_counters: String = tx.query_row(
                "SELECT counters_json FROM product_review_runs_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3",
                params![receipt.order_id, receipt.attempt, journal_run],
                |r| r.get(0),
            ).map_err(db_error)?;
            let mut followup_intent = snapshot.intent.clone();
            followup_intent.run =
                foundry_exec::RunId(u64::try_from(followup_run).map_err(db_error)?);
            binding.journal_run = followup_journal.clone();
            snapshot.binding = binding.clone();
            snapshot.intent = followup_intent.clone();
            let encoded_snapshot = encode_continuation_snapshot(&snapshot)?;
            tx.execute(
                "INSERT INTO product_review_runs_v1(order_id,attempt,run_id,base_commit,result_commit,pack_digest,status,counters_json,hold_json,hold_instance_id,deadline,result_json,provenance_json,product_lifecycle,source_revision,admission_seq,admission_hash,created_at) SELECT order_id,attempt,?1,base_commit,result_commit,pack_digest,'resuming',?2,?3,NULL,NULL,?4,provenance_json,product_lifecycle,source_revision,admission_seq,admission_hash,unixepoch() FROM product_review_runs_v1 WHERE order_id=?5 AND attempt=?6 AND run_id=?7",
                params![followup_journal, original_counters, encoded_snapshot, original_result, receipt.order_id, receipt.attempt, journal_run],
            ).map_err(db_error)?;
            if tx.changes() != 1 {
                return Err(HostError::Persistence(
                    "originating review run disappeared during materialization".into(),
                ));
            }
            // Bind the reserved followup run to the same exact intake worker
            // claim and source-specific service receipt. The origin run's
            // binding remains immutable; a continuation stage revalidates
            // this copied binding against the live owner/epoch/fence, lease,
            // service revision, credential revision, source instance and
            // DriveReview grant in its stage-reservation transaction.
            let copied_work_binding = tx.execute(
                "INSERT INTO product_review_work_bindings_v1(order_id,attempt,run_id,lifecycle,delivery_id,worker_id,claim_epoch,claim_fence,source_revision,payload_digest,task_kind,payload_snapshot)
                 SELECT order_id,attempt,?1,lifecycle,delivery_id,worker_id,claim_epoch,claim_fence,source_revision,payload_digest,task_kind,payload_snapshot
                 FROM product_review_work_bindings_v1 WHERE order_id=?2 AND attempt=?3 AND run_id=?4",
                params![followup_journal, receipt.order_id, receipt.attempt, journal_run],
            ).map_err(db_error)?;
            if copied_work_binding != 1 {
                return Err(HostError::Acceptance(
                    "accepted continuation has no exact originating work claim".into(),
                ));
            }
            tx.execute(
                "INSERT INTO product_review_host_accept_v1(order_id,attempt,run_id,activation) VALUES(?1,?2,?3,?4)",
                params![receipt.order_id, receipt.attempt, followup_run, receipt.activation],
            ).map_err(db_error)?;
            tx.execute(
                "INSERT INTO product_review_host_results_v1(order_id,attempt,run_id,activation,result_json) VALUES(?1,?2,?3,?4,?5)",
                params![receipt.order_id, receipt.attempt, followup_run, receipt.activation, original_host_result],
            ).map_err(db_error)?;
            tx.execute(
                "INSERT INTO product_review_bindings_v1(key_json,intent_json) VALUES(?1,?2)",
                params![key_json(&followup_intent)?, json(&followup_intent)?],
            )
            .map_err(db_error)?;
            // A continuation carries the immutable authority captured at its
            // origin activation. It never refreshes the snapshot from current
            // service or grant rows.
            let copied_authority = tx.execute(
                "INSERT INTO product_review_authority_v1(key_json,order_id,lifecycle,attempt,run_id,activation,journal_run,host_epoch,service_id,service_revision,credential_revision,drive_review_grant_revision,source_instance_id,source_generation,source_instance_revision,connector_revision)
                 SELECT ?1,order_id,lifecycle,attempt,?2,activation,?3,host_epoch,service_id,service_revision,credential_revision,drive_review_grant_revision,source_instance_id,source_generation,source_instance_revision,connector_revision
                 FROM product_review_authority_v1 WHERE order_id=?4 AND attempt=?5 AND run_id=?6 AND activation=?7",
                params![key_json(&followup_intent)?,followup_run,followup_journal,receipt.order_id,receipt.attempt,receipt.originating_run,receipt.activation],
            ).map_err(db_error)?;
            if copied_authority != 1 {
                return Err(HostError::Acceptance(
                    "accepted continuation has no exact originating authority snapshot".into(),
                ));
            }
            tx.execute(
                "UPDATE product_review_active_v1 SET key_json=?1,epoch=?2,journal_run=?3 WHERE order_id=?4",
                params![key_json(&followup_intent)?, followup_intent.epoch.0 as i64, followup_journal, receipt.order_id],
            ).map_err(db_error)?;
            if tx.changes() != 1 {
                return Err(HostError::Persistence(
                    "active review run disappeared during materialization".into(),
                ));
            }
        }
        let meta_epoch: i64 = tx
            .query_row(
                "SELECT recovery_epoch FROM product_meta_v1 WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        let fence: i64 = tx
            .query_row(
                "UPDATE product_fence_counter_v1 SET next=next+1 WHERE singleton=1 RETURNING next",
                [],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        let owner = format!("{}:{meta_epoch}:{fence}", receipt.operation_id);
        let n=tx.execute("UPDATE product_review_hold_progress_v1 SET phase='continuing',progress_revision=progress_revision+1,owner_id=?1,epoch=?2,fence=?3,recovery_reason=NULL WHERE operation_id=?4 AND phase IN ('accepted','continuing','recovery_required') AND reserved_run_id=?5",params![owner,meta_epoch,fence,operation_id,receipt.reserved_run_id]).map_err(db_error)?;
        if n != 1 {
            return Err(HostError::Worker(
                "accepted product continuation progress changed".into(),
            ));
        }
        tx.commit().map_err(db_error)?;
        self.active_review_claim = Some(super::ProductReviewClaim {
            operation_id: receipt.operation_id.clone(),
            owner_id: owner,
            epoch: meta_epoch,
            fence,
            reserved_run_id: receipt.reserved_run_id,
        });
        self.active_review_claim_lock = Some(lock);
        Ok(foundry_exec::ReviewAcceptedHold {
            operation_id: receipt.operation_id,
            actor_id: receipt.actor_id,
            decision: action,
            binding,
            snapshot,
            reserved_run_id: receipt.reserved_run_id,
        })
    }
    fn finalize_accepted_hold(
        &mut self,
        claim: foundry_exec::ReviewAcceptedHold,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), HostError> {
        let active = self
            .active_review_claim
            .clone()
            .ok_or_else(|| unsupported("accepted hold finalization has no durable claim"))?;
        if active.operation_id != claim.operation_id {
            return Err(HostError::Worker(
                "accepted hold finalization claim mismatch".into(),
            ));
        }
        if matches!(
            claim.decision,
            foundry_exec::ReviewHoldAction::Resume { .. }
        ) {
            return Err(HostError::Worker(
                "resume must complete through the frozen host algorithm".into(),
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        ensure_product_claim(&tx, &active)?;
        ensure_product_claim_for_run(
            &tx,
            &active,
            &claim.binding.order_id,
            claim.binding.attempt as i64,
            &claim.binding.journal_run,
        )?;
        ensure_product_work_owner_fence_in(
            &tx,
            &claim.binding.order_id,
            claim.binding.attempt as i64,
            &claim.binding.journal_run,
        )?;
        let stored: ProductHoldAcceptance = decode(
            &tx.query_row(
                "SELECT receipt_json FROM product_review_hold_acceptances_v1 WHERE operation_id=?1",
                [&claim.operation_id],
                |r| r.get::<_, String>(0),
            )
            .map_err(db_error)?,
        )?;
        let stored_action = match &stored.decision {
            ProductHoldDecision::Resume(target) => foundry_exec::ReviewHoldAction::Resume {
                target: target.clone(),
            },
            ProductHoldDecision::CommitPass => foundry_exec::ReviewHoldAction::CommitPass,
            ProductHoldDecision::CommitFail => foundry_exec::ReviewHoldAction::CommitFail,
            ProductHoldDecision::Abort => foundry_exec::ReviewHoldAction::Abort,
        };
        if stored.actor_id != claim.actor_id
            || stored.order_id != claim.binding.order_id
            || stored.attempt != claim.binding.attempt as i64
            || stored.activation != claim.snapshot.intent.activation as i64
            || stored.host_epoch != claim.snapshot.intent.epoch.0 as i64
            || stored_action != claim.decision
            || stored.reserved_run_id != claim.reserved_run_id
            || stored.hold_instance_id != claim.snapshot.held.hold.hold_instance_id
        {
            return Err(HostError::Acceptance(
                "accepted hold proof binding changed".into(),
            ));
        }
        let mut result:HostResult=decode(&tx.query_row("SELECT result_json FROM product_review_runs_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3",params![claim.binding.order_id,claim.binding.attempt as i64,claim.binding.journal_run],|r|r.get::<_,String>(0)).map_err(db_error)?)?;
        result.held = None;
        result.exit_code = if matches!(claim.decision, foundry_exec::ReviewHoldAction::CommitPass) {
            0
        } else {
            1
        };
        let result_json = json(&result)?;
        let status = match claim.decision {
            foundry_exec::ReviewHoldAction::CommitPass
            | foundry_exec::ReviewHoldAction::CommitFail => "resolved",
            foundry_exec::ReviewHoldAction::Abort => "review_aborted",
            _ => {
                return Err(HostError::Worker(
                    "invalid accepted hold finalization action".into(),
                ));
            }
        };
        append_hold_outcome_in(&tx, &stored, &claim.decision, now)?;
        let n=tx.execute("UPDATE product_review_runs_v1 SET status=?1,hold_json=?2,result_json=?3 WHERE order_id=?4 AND attempt=?5 AND run_id=?6 AND status='held'",params![status,encode_continuation_snapshot(&claim.snapshot)?,result_json,claim.binding.order_id,claim.binding.attempt as i64,claim.binding.journal_run]).map_err(db_error)?;
        if n != 1 {
            return Err(HostError::Worker(
                "accepted hold changed before finalization".into(),
            ));
        }
        let n=tx.execute("UPDATE product_review_host_results_v1 SET result_json=?1 WHERE order_id=?2 AND attempt=?3 AND run_id=?4 AND activation=?5",params![result_json,claim.binding.order_id,claim.binding.attempt as i64,claim.snapshot.intent.run.0 as i64,claim.snapshot.intent.activation as i64]).map_err(db_error)?;
        if n != 1 {
            return Err(HostError::Persistence(
                "accepted hold result binding is missing".into(),
            ));
        }
        let n=tx.execute("UPDATE product_review_hold_progress_v1 SET phase='completed',progress_revision=progress_revision+1,recovery_reason=NULL WHERE operation_id=?1 AND phase='continuing' AND owner_id=?2 AND epoch=?3 AND fence=?4",params![active.operation_id,active.owner_id,active.epoch,active.fence]).map_err(db_error)?;
        if n != 1 {
            return Err(HostError::Worker(
                "accepted hold progress fence changed".into(),
            ));
        }
        tx.commit().map_err(db_error)?;
        self.active_review_claim = None;
        self.active_review_claim_lock = None;
        Ok(())
    }
    fn latest_hold(&self, order: &str) -> Result<Option<ReviewHoldRecord>, HostError> {
        let row:Option<(i64,String,String,i64,String)>=self.conn.query_row("SELECT attempt,run_id,status,COALESCE(deadline,0),hold_json FROM product_review_runs_v1 WHERE order_id=?1 AND hold_json IS NOT NULL ORDER BY created_at DESC,rowid DESC LIMIT 1",[order],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(db_error)?;
        let Some((attempt, run, status, deadline, encoded)) = row else {
            return Ok(None);
        };
        let binding = ReviewRunBinding {
            order_id: order.into(),
            attempt: attempt as u64,
            journal_run: run.clone(),
        };
        let snapshot = decode_hold(binding.clone(), &encoded)?;
        let digest = Sha256::digest(encoded.as_bytes()).into();
        let status = match status.as_str() {
            "held" => foundry_exec::ReviewHoldStatus::Held,
            "resuming" => foundry_exec::ReviewHoldStatus::Resuming,
            "resolved" => foundry_exec::ReviewHoldStatus::Resolved,
            "failed" => foundry_exec::ReviewHoldStatus::Failed,
            _ => foundry_exec::ReviewHoldStatus::Other,
        };
        Ok(Some(ReviewHoldRecord {
            binding,
            status,
            hold_id: snapshot.held.hold.hold_instance_id.clone(),
            deadline,
            revision: foundry_exec::ReviewHoldRevision::from_storage_digest(digest),
            snapshot,
        }))
    }
    fn run_result(&self, order: &str, attempt: i64, run: &str) -> Result<HostResult, HostError> {
        let s:String=self.conn.query_row("SELECT result_json FROM product_review_runs_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3",params![order,attempt,run],|r|r.get(0)).map_err(db_error)?;
        decode(&s)
    }
    fn binding_intent(
        &self,
        intent: &ExecutionIntent,
    ) -> Result<Option<ExecutionIntent>, HostError> {
        let s: Option<String> = self
            .conn
            .query_row(
                "SELECT intent_json FROM product_review_bindings_v1 WHERE key_json=?1",
                [key_json(intent)?],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        s.map(|v| decode(&v)).transpose()
    }
}

pub(super) fn ensure_current_acceptance_authority_in(
    tx: &rusqlite::Transaction<'_>,
    operation_id: &str,
) -> Result<(), HostError> {
    let receipt_json: String = tx
        .query_row(
            "SELECT receipt_json FROM product_review_hold_acceptances_v1 WHERE operation_id=?1",
            [operation_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)?
        .ok_or_else(|| HostError::Acceptance("accepted human authority is unavailable".into()))?;
    let receipt: ProductHoldAcceptance = decode(&receipt_json)?;
    let current: Option<(String, i64, i64)> = tx
        .query_row(
            "SELECT role,revision,enabled FROM product_accounts_v1 WHERE actor_id=?1",
            [&receipt.actor_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(db_error)?;
    let Some((role, revision, enabled)) = current else {
        return Err(HostError::Acceptance(
            "accepted human authority is unavailable".into(),
        ));
    };
    if enabled != 1
        || revision != receipt.authority_revision
        || !matches!(role.as_str(), "owner" | "approver")
    {
        return Err(HostError::Acceptance(
            "accepted human authority binding changed".into(),
        ));
    }
    if super::scope_enforced_in(tx).map_err(db_error)? {
        let current_source: Option<(i64, String)> = tx
            .query_row(
                "SELECT o.lifecycle,p.source_instance_id FROM product_orders_v1 o JOIN product_scope_order_provenance_v1 p ON p.order_id=o.order_id AND p.lifecycle=o.lifecycle AND p.verified=1 WHERE o.order_id=?1",
                [&receipt.order_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_error)?;
        let current_scope: Option<i64> = tx
            .query_row(
                "SELECT revision FROM product_scope_subject_revisions_v1 WHERE subject_id=?1 AND source_instance_id=?2",
                rusqlite::params![receipt.actor_id, receipt.source_instance_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_error)?;
        let current_grant: Option<i64> = tx
            .query_row(
                "SELECT grant_revision FROM product_scope_grants_v1 WHERE subject_id=?1 AND source_instance_id=?2 AND capability='resolve_review_hold' AND enabled=1",
                rusqlite::params![receipt.actor_id, receipt.source_instance_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_error)?;
        if current_source != Some((receipt.lifecycle, receipt.source_instance_id.clone()))
            || current_scope != Some(receipt.scope_revision)
            || current_grant != Some(receipt.grant_revision)
        {
            return Err(HostError::Acceptance(
                "accepted human scope authority is unavailable or changed".into(),
            ));
        }
    }
    Ok(())
}

fn stage_output(e: &ReviewStageEvidence) -> Result<String, HostError> {
    json(
        &serde_json::json!({"role":e.role,"log_path":e.log_path,"sha256":e.sha256,"deferred":e.deferred,"inspected_commit":e.inspected_commit,"findings":e.findings,"verdicts":e.verdicts,"input_manifest":e.input_manifest,"produced_commit":e.produced_commit}),
    )
}
fn charge_budget_transaction(
    conn: &mut rusqlite::Connection,
    clock: &dyn super::ProductClock,
    pack_digest: &str,
    order: &str,
    intent: &ExecutionIntent,
    #[cfg(any(test, feature = "p2-test-support"))] observer: Option<&ChargeBudgetObserver>,
    #[cfg(not(any(test, feature = "p2-test-support")))] _observer: Option<&()>,
) -> Result<(), HostError> {
    if order != intent.order.0 {
        return Err(HostError::Acceptance(
            "redrive budget order does not match execution intent".into(),
        ));
    }
    #[cfg(any(test, feature = "p2-test-support"))]
    if let Some(observer) = observer {
        notify_charge_budget(observer, ChargeBudgetObservation::BeginAttempt);
    }
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(db_error)?;
    // Authority and lease validity are evaluated after the serialized write
    // transaction is owned, because BEGIN IMMEDIATE may wait for another writer.
    let now = clock.now();
    let journal_run = format!(
        "{}:{}:{}:{}",
        intent.order.0, intent.attempt.0, intent.run.0, intent.activation
    );
    ensure_new_work(&tx, intent, &journal_run, pack_digest, now)?;
    let used: i64 = tx
        .query_row(
            "SELECT used FROM product_review_budget_v1 WHERE order_id=?1",
            [&intent.order.0],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)?
        .unwrap_or(0);
    if used < 0 || used >= PRODUCT_REVIEW_BUDGET_LIMIT {
        return Err(HostError::Worker(
            "product review budget exhausted or corrupt".into(),
        ));
    }
    let changed = if used == 0 {
        tx.execute(
            "INSERT INTO product_review_budget_v1(order_id,used) VALUES(?1,1)",
            [&intent.order.0],
        )
        .map_err(db_error)?
    } else {
        tx.execute(
            "UPDATE product_review_budget_v1 SET used=used+1 WHERE order_id=?1 AND used=?2 AND used<?3",
            params![intent.order.0, used, PRODUCT_REVIEW_BUDGET_LIMIT],
        )
        .map_err(db_error)?
    };
    if changed != 1 {
        return Err(HostError::Worker(
            "product review budget changed during redrive charge".into(),
        ));
    }
    tx.commit().map_err(db_error)
}

fn ensure_active_fence(
    tx: &rusqlite::Transaction<'_>,
    intent: &ExecutionIntent,
    run: &str,
    pack: &str,
) -> Result<ActiveRunBinding, HostError> {
    let row: Option<ActiveFenceRow> = tx.query_row(
        "SELECT a.journal_run,a.epoch,r.status,r.pack_digest,r.base_commit,r.result_commit,r.product_lifecycle,r.source_revision,r.admission_seq,r.admission_hash,o.repository_id FROM product_review_active_v1 a JOIN product_review_runs_v1 r ON r.order_id=a.order_id AND r.run_id=a.journal_run JOIN product_orders_v1 o ON o.order_id=r.order_id WHERE a.order_id=?1 AND a.key_json=?2 AND r.attempt=?3 AND r.admission_seq>0",
        params![intent.order.0,key_json(intent)?,intent.attempt.0 as i64],
        |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?)),
    ).optional().map_err(db_error)?;
    let Some((
        active,
        epoch,
        status,
        pinned,
        base_commit,
        result_commit,
        lifecycle,
        source_revision,
        admission_seq,
        admission_hash,
        repository_id,
    )) = row
    else {
        return Err(HostError::Worker("inactive product review run".into()));
    };
    if active != run
        || epoch != intent.epoch.0 as i64
        || status != "running"
        || pinned != pack
        || intent.external_ref
            != format!(
                "review:{}:{}:{}..{}",
                intent.order.0, intent.attempt.0, base_commit, result_commit
            )
    {
        return Err(HostError::Worker(
            "product review run binding or fence changed".into(),
        ));
    }
    let exact_admission: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM product_transition_receipts_v1 r JOIN ledger l ON l.seq=r.seq AND l.hash=r.event_hash WHERE r.seq=?1 AND r.event_hash=?2 AND r.order_id=?3 AND r.operation='github-admission')",
        params![admission_seq,admission_hash,intent.order.0], |r|r.get(0),
    ).map_err(db_error)?;
    if !exact_admission {
        return Err(HostError::Acceptance(
            "pinned product admission receipt is invalid".into(),
        ));
    }
    let exact_work_owner: bool = tx
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM product_review_work_bindings_v1 b
                JOIN product_work_claims_v1 c ON c.order_id=b.order_id AND c.lifecycle=b.lifecycle
                JOIN product_work_backlog_v1 q ON q.order_id=b.order_id AND q.lifecycle=b.lifecycle
                WHERE b.order_id=?1 AND b.attempt=?2 AND b.run_id=?3
                  AND b.lifecycle=?4 AND b.delivery_id=q.delivery_id AND b.worker_id=c.owner_id
                  AND b.claim_epoch=c.epoch AND b.claim_fence=c.fence
                  AND b.source_revision=c.source_revision AND b.payload_digest=c.payload_digest
                  AND c.state='claimed' AND q.state='claimed'
            )",
            params![intent.order.0, intent.attempt.0 as i64, run, lifecycle],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    if !exact_work_owner {
        return Err(HostError::Worker(
            "product review work claim was lost or reclaimed".into(),
        ));
    }
    Ok(ActiveRunBinding {
        lifecycle,
        source_revision,
        admission_seq,
        admission_hash,
        repository_id,
        base_commit,
    })
}

fn ensure_new_work(
    tx: &rusqlite::Transaction<'_>,
    intent: &ExecutionIntent,
    run: &str,
    pack: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), HostError> {
    let binding = ensure_active_fence(tx, intent, run, pack)?;
    let order: Option<(i64,i64,i64,String,String,i64)> = tx.query_row(
        "SELECT o.lifecycle,o.source_revision,o.admission_seq,o.admission_hash,o.repository_id,s.revision FROM product_orders_v1 o JOIN product_sources_v1 s ON s.connector_id=o.connector_id WHERE o.order_id=?1 AND s.enabled=1",
        [&intent.order.0], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
    ).optional().map_err(db_error)?;
    let Some((lifecycle, source_revision, seq, hash, repository, source_now)) = order else {
        return Err(HostError::Acceptance("product source is disabled".into()));
    };
    if lifecycle != binding.lifecycle
        || source_revision != binding.source_revision
        || source_now != binding.source_revision
        || seq != binding.admission_seq
        || hash != binding.admission_hash
        || repository != binding.repository_id
    {
        return Err(HostError::Acceptance(
            "product admission or source revision changed".into(),
        ));
    }
    let live_claim: bool = tx
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM product_review_work_bindings_v1 b
                JOIN product_work_claims_v1 c ON c.order_id=b.order_id AND c.lifecycle=b.lifecycle
                JOIN product_work_backlog_v1 q ON q.order_id=b.order_id AND q.lifecycle=b.lifecycle
                JOIN product_orders_v1 o ON o.order_id=b.order_id AND o.lifecycle=b.lifecycle
                JOIN product_sources_v1 s ON s.connector_id=o.connector_id
                WHERE b.order_id=?1 AND b.attempt=?2 AND b.run_id=?3
                  AND b.worker_id=c.owner_id AND b.claim_epoch=c.epoch AND b.claim_fence=c.fence
                  AND b.source_revision=c.source_revision AND b.payload_digest=c.payload_digest
                  AND c.state='claimed' AND c.lease_until>?4 AND q.state='claimed'
                  AND s.enabled=1 AND s.revision=b.source_revision
            )",
            params![
                intent.order.0,
                intent.attempt.0 as i64,
                run,
                now.to_rfc3339()
            ],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    if !live_claim {
        return Err(HostError::Acceptance(
            "product work claim expired or its source authorization changed".into(),
        ));
    }
    let exact_admission:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM product_transition_receipts_v1 r JOIN ledger l ON l.seq=r.seq AND l.hash=r.event_hash WHERE r.seq=?1 AND r.event_hash=?2 AND r.order_id=?3 AND r.operation='github-admission')",params![binding.admission_seq,binding.admission_hash,intent.order.0],|r|r.get(0)).map_err(db_error)?;
    if !exact_admission {
        return Err(HostError::Acceptance(
            "product source admission receipt is invalid".into(),
        ));
    }
    let state: Option<String> = tx
        .query_row(
            "SELECT to_state FROM ledger WHERE order_id=?1 ORDER BY seq DESC LIMIT 1",
            [&intent.order.0],
            |r| r.get(0),
        )
        .optional()
        .map_err(db_error)?;
    if !state
        .as_deref()
        .is_some_and(|s| matches!(s, "queued" | "working" | "held" | "waiting" | "inspecting"))
    {
        return Err(HostError::Acceptance(
            "product lifecycle no longer admits review work".into(),
        ));
    }
    let service_valid = super::service::review_work_service_valid_in(
        tx,
        &intent.order.0,
        intent.attempt.0 as i64,
        run,
        now,
    )
    .map_err(|_| HostError::Acceptance("review service authority is absent or expired".into()))?;
    if !service_valid {
        return Err(HostError::Acceptance(
            "review service authority is absent or expired".into(),
        ));
    }
    Ok(())
}

fn ensure_resuming_fence(
    tx: &rusqlite::Transaction<'_>,
    intent: &ExecutionIntent,
    run: &str,
    pack: &str,
) -> Result<ActiveRunBinding, HostError> {
    let row:Option<ActiveFenceRow>=tx.query_row(
        "SELECT a.journal_run,a.epoch,r.status,r.pack_digest,r.base_commit,r.result_commit,r.product_lifecycle,r.source_revision,r.admission_seq,r.admission_hash,o.repository_id FROM product_review_active_v1 a JOIN product_review_runs_v1 r ON r.order_id=a.order_id AND r.run_id=a.journal_run JOIN product_orders_v1 o ON o.order_id=r.order_id WHERE a.order_id=?1 AND a.key_json=?2 AND r.attempt=?3 AND r.admission_seq>0",
        params![intent.order.0,key_json(intent)?,intent.attempt.0 as i64],
        |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?)),
    ).optional().map_err(db_error)?;
    let Some((
        active,
        epoch,
        status,
        pinned,
        base,
        result,
        lifecycle,
        source_revision,
        admission_seq,
        admission_hash,
        repository_id,
    )) = row
    else {
        return Err(HostError::Worker("inactive accepted continuation".into()));
    };
    if active != run
        || epoch != intent.epoch.0 as i64
        || status != "resuming"
        || pinned != pack
        || intent.external_ref
            != format!(
                "review:{}:{}:{}..{}",
                intent.order.0, intent.attempt.0, base, result
            )
    {
        return Err(HostError::Worker(
            "accepted continuation run binding or fence changed".into(),
        ));
    }
    let exact:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM product_transition_receipts_v1 r JOIN ledger l ON l.seq=r.seq AND l.hash=r.event_hash WHERE r.seq=?1 AND r.event_hash=?2 AND r.order_id=?3 AND r.operation='github-admission')",params![admission_seq,admission_hash,intent.order.0],|r|r.get(0)).map_err(db_error)?;
    if !exact {
        return Err(HostError::Acceptance(
            "pinned product admission receipt is invalid".into(),
        ));
    }
    Ok(ActiveRunBinding {
        lifecycle,
        source_revision,
        admission_seq,
        admission_hash,
        repository_id,
        base_commit: base,
    })
}

fn ensure_product_claim(
    tx: &rusqlite::Transaction<'_>,
    claim: &super::ProductReviewClaim,
) -> Result<(), HostError> {
    let current:Option<(String,String,i64,i64,i64)>=tx.query_row("SELECT phase,owner_id,epoch,fence,reserved_run_id FROM product_review_hold_progress_v1 WHERE operation_id=?1",[&claim.operation_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(db_error)?;
    if !current.is_some_and(|(phase, owner, epoch, fence, reserved)| {
        phase == "continuing"
            && owner == claim.owner_id
            && epoch == claim.epoch
            && fence == claim.fence
            && reserved == claim.reserved_run_id
    }) {
        return Err(HostError::Worker(
            "accepted continuation owner or fence is stale".into(),
        ));
    }
    Ok(())
}

fn ensure_product_claim_for_run(
    tx: &rusqlite::Connection,
    claim: &super::ProductReviewClaim,
    order: &str,
    attempt: i64,
    run: &str,
) -> Result<(), HostError> {
    let matches:bool=tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM product_review_hold_acceptances_v1 a JOIN product_review_run_reservations_v1 q ON q.operation_id=a.operation_id JOIN product_review_runs_v1 r ON r.order_id=a.order_id AND r.attempt=a.attempt AND r.run_id=?4 WHERE a.operation_id=?1 AND a.order_id=?2 AND a.attempt=?3 AND (r.run_id=printf('%s:%d:%d:%d',a.order_id,a.attempt,a.originating_run,json_extract(a.receipt_json,'$.activation')) OR r.run_id=printf('%s:%d:%d:%d',a.order_id,a.attempt,q.followup_run_id,json_extract(a.receipt_json,'$.activation'))) AND q.followup_run_id=?5)",
        params![claim.operation_id,order,attempt,run,claim.reserved_run_id],
        |r|r.get(0),
    ).map_err(db_error)?;
    if !matches {
        return Err(HostError::Worker(
            "accepted continuation claim does not bind this product run".into(),
        ));
    }
    Ok(())
}

/// Cleanup of an already-started external stage may proceed after service or
/// source revocation, but only while the exact worker claim that reserved it
/// remains current. Lease expiry alone does not discard a returned result;
/// owner/epoch/fence reassignment makes the old result stale.
fn ensure_product_work_owner_fence_in(
    tx: &rusqlite::Transaction<'_>,
    order: &str,
    attempt: i64,
    run: &str,
) -> Result<(), HostError> {
    let exact: bool = tx
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM product_review_work_bindings_v1 b
                JOIN product_work_claims_v1 c ON c.order_id=b.order_id AND c.lifecycle=b.lifecycle
                WHERE b.order_id=?1 AND b.attempt=?2 AND b.run_id=?3
                  AND c.owner_id=b.worker_id AND c.epoch=b.claim_epoch AND c.fence=b.claim_fence
                  AND c.source_revision=b.source_revision AND c.state='claimed'
            )",
            params![order, attempt, run],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    if !exact {
        return Err(HostError::Worker(
            "accepted continuation worker owner or fence was reassigned".into(),
        ));
    }
    Ok(())
}

fn ensure_product_source_for_new_stage(
    tx: &rusqlite::Transaction<'_>,
    order: &str,
    attempt: i64,
    run: &str,
    binding: &ActiveRunBinding,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), HostError> {
    let row:Option<(i64,i64,i64,String,String,i64)>=tx.query_row("SELECT o.lifecycle,o.source_revision,o.admission_seq,o.admission_hash,o.repository_id,s.revision FROM product_orders_v1 o JOIN product_sources_v1 s ON s.connector_id=o.connector_id WHERE o.order_id=?1 AND s.enabled=1",[order],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional().map_err(db_error)?;
    let Some((lifecycle, revision, seq, hash, repository, current_revision)) = row else {
        return Err(HostError::Acceptance("product source is disabled".into()));
    };
    if lifecycle != binding.lifecycle
        || revision != binding.source_revision
        || current_revision != binding.source_revision
        || seq != binding.admission_seq
        || hash != binding.admission_hash
        || repository != binding.repository_id
    {
        return Err(HostError::Acceptance(
            "product admission or source revision changed".into(),
        ));
    }
    let (last_seq, current_lifecycle, state) =
        super::verified_order_state_in(tx, order).map_err(db_error)?;
    if current_lifecycle as i64 + 1 != binding.lifecycle
        || last_seq < binding.admission_seq
        || !matches!(
            state.as_str(),
            "queued" | "working" | "held" | "waiting" | "inspecting"
        )
    {
        return Err(HostError::Acceptance(
            "product lifecycle no longer admits continuation work".into(),
        ));
    }
    let exact:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM product_transition_receipts_v1 r JOIN ledger l ON l.seq=r.seq AND l.hash=r.event_hash WHERE r.seq=?1 AND r.event_hash=?2 AND r.order_id=?3 AND r.operation='github-admission')",params![binding.admission_seq,binding.admission_hash,order],|r|r.get(0)).map_err(db_error)?;
    if !exact {
        return Err(HostError::Acceptance(
            "product admission receipt is invalid".into(),
        ));
    }
    let service_valid = super::service::review_work_service_valid_in(tx, order, attempt, run, now)
        .map_err(|_| {
            HostError::Acceptance("review service authority is absent or expired".into())
        })?;
    if !service_valid {
        return Err(HostError::Acceptance(
            "review service authority is absent or expired".into(),
        ));
    }
    Ok(())
}

fn validate_terminal_payload(
    request: &ReviewDispatchCompletion,
    pinned_pack: &str,
) -> Result<(), HostError> {
    if request.result.held.as_ref() != request.held_snapshot.as_ref().map(|s| &s.hold) {
        return Err(HostError::Worker("terminal hold snapshot mismatch".into()));
    }
    match request.status {
        ReviewTerminalStatus::Passed if request.result.exit_code != 0 => {
            return Err(HostError::Worker(
                "passed terminal status requires a zero exit code".into(),
            ));
        }
        ReviewTerminalStatus::Failed if request.result.exit_code == 0 => {
            return Err(HostError::Worker(
                "failed terminal status requires a nonzero exit code".into(),
            ));
        }
        ReviewTerminalStatus::Held if request.result.exit_code == 0 => {
            return Err(HostError::Worker(
                "held terminal status requires a nonzero exit code".into(),
            ));
        }
        _ => {}
    }
    if request.provenance.pack_digest != pinned_pack
        || request.provenance.executable.is_empty()
        || request.provenance.source_commit.is_empty()
        || request.provenance.sha256.len() != 64
        || !request
            .provenance
            .sha256
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
    {
        return Err(HostError::Worker(
            "terminal provenance does not match the pinned product pack".into(),
        ));
    }
    for (index, generation) in request.result.generations.iter().enumerate() {
        if generation.commit.is_empty() || generation.generation != index as u64 {
            return Err(HostError::Worker(
                "terminal generation evidence is malformed".into(),
            ));
        }
    }
    Ok(())
}

fn validate_terminal_run_binding(
    request: &ReviewDispatchCompletion,
    binding: &ActiveRunBinding,
) -> Result<(), HostError> {
    let expected_final = request
        .result
        .generations
        .last()
        .map(|g| g.commit.as_str())
        .unwrap_or(&request.result.base_commit);
    if request.intent.generation.0 != 0
        || request.result.base_commit != binding.base_commit
        || request.result.result_commit != expected_final
    {
        return Err(HostError::Worker(
            "terminal result intent, base, or final generation binding is invalid".into(),
        ));
    }
    if let Some(snapshot) = &request.held_snapshot {
        if request.status != ReviewTerminalStatus::Held
            || snapshot.latest_commit != request.result.result_commit
            || snapshot.generations != request.result.generations
            || snapshot.hold.hold_instance_id.is_empty()
            || snapshot.hold.resume_targets.is_empty()
        {
            return Err(HostError::Worker(
                "held snapshot does not match terminal run evidence".into(),
            ));
        }
    } else if request.status == ReviewTerminalStatus::Held {
        return Err(HostError::Worker(
            "held terminal result lacks its frozen snapshot".into(),
        ));
    }
    Ok(())
}

fn validate_continuation_result_in(
    tx: &rusqlite::Transaction<'_>,
    request: &ReviewContinuationCompletion,
    binding: &ActiveRunBinding,
) -> Result<(), HostError> {
    let expected_final = request
        .result
        .generations
        .last()
        .map(|generation| generation.commit.as_str())
        .unwrap_or(&request.result.base_commit);
    if request.intent.generation.0 != 0
        || request.result.base_commit != binding.base_commit
        || request.result.result_commit != expected_final
        || request
            .result
            .generations
            .iter()
            .enumerate()
            .any(|(index, generation)| {
                generation.generation != index as u64 || generation.commit.is_empty()
            })
    {
        return Err(HostError::Worker(
            "continuation result intent, base, or generation chain is malformed".into(),
        ));
    }

    let last_action = request
        .snapshot
        .human_events
        .last()
        .map(|event| &event.action)
        .ok_or_else(|| HostError::Worker("continuation has no accepted human action".into()))?;
    match (
        last_action,
        request.result.held.is_some(),
        request.result.exit_code,
    ) {
        (foundry_exec::ReviewHumanAction::CommitPass, false, 0) => {}
        (foundry_exec::ReviewHumanAction::CommitFail, false, code) if code != 0 => {}
        (foundry_exec::ReviewHumanAction::Abort, false, code) if code != 0 => {}
        (foundry_exec::ReviewHumanAction::Resume { .. }, true, code) if code != 0 => {}
        (foundry_exec::ReviewHumanAction::Resume { .. }, false, _) => {}
        (foundry_exec::ReviewHumanAction::DeadlineExpired, false, _) => {}
        _ => {
            return Err(HostError::Worker(
                "continuation result status or exit code conflicts with accepted action".into(),
            ));
        }
    }
    if let Some(held) = request.result.held.as_ref() {
        if held != &request.snapshot.held.hold
            || request.snapshot.held.generations != request.result.generations
            || request.snapshot.held.latest_commit != request.result.result_commit
            || held.hold_instance_id.is_empty()
            || held.resume_targets.is_empty()
        {
            return Err(HostError::Worker(
                "continued hold does not match its generation and snapshot evidence".into(),
            ));
        }
    }

    let provenance_json: Option<String> = tx
        .query_row(
            "SELECT provenance_json FROM product_review_runs_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND pack_digest=?4",
            params![request.binding.order_id,request.binding.attempt as i64,request.binding.journal_run,request.pack_digest],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)?;
    let provenance: ReviewBinaryProvenance = decode(
        provenance_json
            .as_deref()
            .ok_or_else(|| HostError::Worker("continuation provenance is missing".into()))?,
    )?;
    if provenance.pack_digest != request.pack_digest
        || provenance.executable.trim().is_empty()
        || !valid_hex_digest(&provenance.sha256, 64)
        || !(valid_hex_digest(&provenance.source_commit, 40)
            || valid_hex_digest(&provenance.source_commit, 64))
    {
        return Err(HostError::Worker(
            "continuation provenance does not match the pinned product run".into(),
        ));
    }
    Ok(())
}

fn valid_hex_digest(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

struct ProductArtifactRegistration<'a> {
    config: &'a super::ProductReviewConfig,
    root_path: &'a Path,
    root_handle: &'a File,
    intent: &'a ExecutionIntent,
    run_id: &'a str,
    stage_sequence: u64,
    stage_id: &'a str,
    evidence: &'a ReviewStageEvidence,
    now: chrono::DateTime<chrono::Utc>,
}

fn register_product_artifact_in(
    tx: &rusqlite::Transaction<'_>,
    registration: ProductArtifactRegistration<'_>,
) -> Result<(), HostError> {
    let ProductArtifactRegistration {
        config,
        root_path,
        root_handle,
        intent,
        run_id,
        stage_sequence,
        stage_id,
        evidence,
        now,
    } = registration;
    if stage_sequence > i64::MAX as u64
        || !valid_hex_digest(&evidence.sha256, 64)
        || run_id.is_empty()
        || stage_id.is_empty()
    {
        return Err(db_error("product artifact provenance is invalid"));
    }
    let produced = Path::new(&evidence.log_path);
    let relative = produced
        .strip_prefix(root_path)
        .map_err(|_| db_error("product artifact escaped its configured root"))?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(db_error("product artifact path is invalid"));
    }
    let relative_path = relative
        .to_str()
        .ok_or_else(|| db_error("product artifact path is not portable"))?
        .to_owned();
    let verified_bytes = read_confined_artifact(root_handle, &relative_path, &evidence.sha256)
        .map_err(|_| db_error("product artifact digest changed before registration"))?;
    drop(verified_bytes);
    let artifact_sha256 = evidence.sha256.clone();
    let provenance: Option<(i64, String, i64, String, String)> = tx
        .query_row(
            "SELECT p.lifecycle,p.source_instance_id,p.admission_seq,p.admission_hash,i.repository_key
             FROM product_scope_order_provenance_v1 p
             JOIN product_scope_source_instances_v1 i USING(source_instance_id)
             JOIN product_orders_v1 o ON o.order_id=p.order_id AND o.lifecycle=p.lifecycle
             WHERE p.order_id=?1 AND p.verified=1 AND o.lifecycle=p.lifecycle",
            [&intent.order.0],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(db_error)?;
    // Old/unbound ProductAtomicV1 review history remains intact but receives
    // no W3 artifact registration without reviewed source provenance.
    let Some((lifecycle, source_instance_id, admission_seq, admission_hash, repository_key)) =
        provenance
    else {
        return Ok(());
    };
    let admission_ok: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM product_transition_receipts_v1 r JOIN ledger l ON l.seq=r.seq AND l.hash=r.event_hash WHERE r.seq=?1 AND r.event_hash=?2 AND r.order_id=?3 AND r.lifecycle=?4 AND r.operation='github-admission')",
            rusqlite::params![admission_seq, admission_hash, intent.order.0, lifecycle],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    let run_ok: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM product_review_runs_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3 AND pack_digest=?4)",
            rusqlite::params![intent.order.0, intent.attempt.0 as i64, run_id, config.pack_digest],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    if !admission_ok || !run_ok {
        return Err(db_error(
            "product artifact admission or run provenance is invalid",
        ));
    }
    let generation = i64::try_from(intent.generation.0)
        .map_err(|_| db_error("product artifact generation is invalid"))?;
    let attempt = i64::try_from(intent.attempt.0)
        .map_err(|_| db_error("product artifact attempt is invalid"))?;
    let provenance_digest = crate::product_scope::framed_hash(
        "foundry-product-stage-artifact-provenance-v1",
        &[
            &intent.order.0,
            &lifecycle.to_string(),
            &source_instance_id,
            &repository_key,
            &admission_seq.to_string(),
            &admission_hash,
            &attempt.to_string(),
            run_id,
            &generation.to_string(),
            &stage_sequence.to_string(),
            stage_id,
            &config.pack_digest,
            &artifact_sha256,
            &relative_path,
        ],
    );
    let artifact_id =
        crate::product_scope::framed_hash("foundry-product-artifact-id-v1", &[&provenance_digest]);
    tx.execute(
        "INSERT INTO product_artifacts_v1(artifact_id,order_id,lifecycle,source_instance_id,repository_key,admission_seq,admission_hash,attempt,run_id,generation,stage_sequence,stage_id,artifact_sha256,provenance_digest,relative_path,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
        rusqlite::params![artifact_id,intent.order.0,lifecycle,source_instance_id,repository_key,admission_seq,admission_hash,attempt,run_id,generation,stage_sequence as i64,stage_id,artifact_sha256,provenance_digest,relative_path,now.to_rfc3339()],
    ).map_err(db_error)?;
    Ok(())
}

type ArtifactRecord = (ProductArtifactInfo, String, String, String, String);

fn artifact_record_in(
    tx: &rusqlite::Transaction<'_>,
    principal: &super::ProductPrincipal,
    order_id: &str,
    artifact_id: Option<&str>,
) -> Result<Vec<ArtifactRecord>, super::ProductError> {
    if !ProductCapability::ReadArtifact.allowed_for_role(&principal.role) {
        return Err(super::ProductError::Invariant(
            "role does not permit artifact reads",
        ));
    }
    let mut statement = tx.prepare(
        "SELECT artifact_id,order_id,lifecycle,source_instance_id,repository_key,admission_seq,admission_hash,attempt,run_id,generation,stage_sequence,stage_id,artifact_sha256,provenance_digest,relative_path
         FROM product_artifacts_v1 WHERE order_id=?1 AND (?2 IS NULL OR artifact_id=?2) ORDER BY lifecycle,attempt,run_id,stage_sequence",
    )?;
    let rows = statement.query_map(rusqlite::params![order_id, artifact_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, i64>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, i64>(7)?,
            r.get::<_, String>(8)?,
            r.get::<_, i64>(9)?,
            r.get::<_, i64>(10)?,
            r.get::<_, String>(11)?,
            r.get::<_, String>(12)?,
            r.get::<_, String>(13)?,
            r.get::<_, String>(14)?,
        ))
    })?;
    let mut records = Vec::new();
    for row in rows {
        let (
            id,
            order,
            lifecycle,
            source,
            repository,
            admission_seq,
            admission_hash,
            attempt,
            run,
            generation,
            sequence,
            stage,
            sha256,
            provenance,
            relative,
        ) = row?;
        let scoped: Option<(String, String, i64, String, i64)> = tx.query_row(
            "SELECT p.source_instance_id,i.repository_key,p.admission_seq,p.admission_hash,p.verified
             FROM product_scope_order_provenance_v1 p
             JOIN product_scope_source_instances_v1 i USING(source_instance_id)
             WHERE p.order_id=?1 AND p.lifecycle=?2",
            rusqlite::params![order, lifecycle],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
        ).optional()?;
        let Some((scoped_source, scoped_repository, scoped_seq, scoped_hash, verified)) = scoped
        else {
            return Err(super::ProductError::Invariant(
                "artifact source provenance is unavailable",
            ));
        };
        let grant: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM product_scope_grants_v1 WHERE subject_id=?1 AND source_instance_id=?2 AND capability='read_artifact' AND enabled=1)",
            rusqlite::params![principal.actor_id,source], |r| r.get(0),
        )?;
        let scope_revision: Option<i64> = tx.query_row(
            "SELECT revision FROM product_scope_subject_revisions_v1 WHERE subject_id=?1 AND source_instance_id=?2",
            rusqlite::params![principal.actor_id,source], |r| r.get(0),
        ).optional()?;
        let admission_ok: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM product_transition_receipts_v1 r JOIN ledger l ON l.seq=r.seq AND l.hash=r.event_hash WHERE r.seq=?1 AND r.event_hash=?2 AND r.order_id=?3 AND r.lifecycle=?4 AND r.operation='github-admission')",
            rusqlite::params![admission_seq,admission_hash,order,lifecycle], |r| r.get(0),
        )?;
        if !grant || scope_revision.is_none() {
            return Err(super::ProductError::Invariant(
                "artifact read grant unavailable",
            ));
        }
        if verified != 1
            || source != scoped_source
            || repository != scoped_repository
            || admission_seq != scoped_seq
            || admission_hash != scoped_hash
            || !admission_ok
            || !valid_hex_digest(&sha256, 64)
            || !valid_hex_digest(&provenance, 64)
            || !valid_hex_digest(&id, 64)
        {
            return Err(super::ProductError::Invariant(
                "artifact immutable provenance is invalid",
            ));
        }
        let identity: (String, String, String, String) = tx.query_row(
            "SELECT r.organization,r.provider,r.api_origin,r.stable_repository_id
             FROM product_scope_repositories_v1 r WHERE r.repository_key=?1",
            [&repository],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        let canonical = crate::product_scope::RepositoryIdentity::canonicalized(
            &identity.0,
            &identity.1,
            &identity.2,
            &identity.3,
        )
        .map_err(|_| super::ProductError::Invariant("artifact repository identity is invalid"))?;
        if canonical.key() != repository {
            return Err(super::ProductError::Invariant(
                "artifact repository identity is invalid",
            ));
        }
        let expected_provenance = crate::product_scope::framed_hash(
            "foundry-product-stage-artifact-provenance-v1",
            &[
                &order,
                &lifecycle.to_string(),
                &source,
                &repository,
                &admission_seq.to_string(),
                &admission_hash,
                &attempt.to_string(),
                &run,
                &generation.to_string(),
                &sequence.to_string(),
                &stage,
                &artifact_pack_digest_in(tx, &order, &attempt, &run)?,
                &sha256,
                &relative,
            ],
        );
        let expected_id = crate::product_scope::framed_hash(
            "foundry-product-artifact-id-v1",
            &[&expected_provenance],
        );
        if expected_provenance != provenance || expected_id != id {
            return Err(super::ProductError::Invariant(
                "artifact immutable provenance is invalid",
            ));
        }
        records.push((
            ProductArtifactInfo {
                artifact_id: id,
                order_id: order,
                lifecycle,
                attempt,
                run_id: run,
                generation,
                stage_sequence: sequence,
                stage_id: stage,
                sha256,
            },
            relative,
            source,
            repository,
            admission_hash,
        ));
    }
    if records.is_empty() {
        return Err(super::ProductError::Invariant("artifact unavailable"));
    }
    Ok(records)
}

fn artifact_pack_digest_in(
    tx: &rusqlite::Transaction<'_>,
    order: &str,
    attempt: &i64,
    run: &str,
) -> Result<String, super::ProductError> {
    tx.query_row(
        "SELECT pack_digest FROM product_review_runs_v1 WHERE order_id=?1 AND attempt=?2 AND run_id=?3",
        rusqlite::params![order,attempt,run], |r| r.get(0),
    ).optional()?.ok_or(super::ProductError::Invariant("artifact review run is unavailable"))
}

impl ProductStore {
    /// List artifacts produced by the genuine ProductStore-backed review host.
    /// Authorization, source provenance, and the artifact records share one
    /// SQLite snapshot. Disabled historical sources remain readable only
    /// while the caller still has an explicit ReadArtifact grant to that
    /// original source instance.
    pub fn list_product_artifacts(
        &mut self,
        session_digest: &str,
        order_id: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<ProductArtifactInfo>, super::ProductError> {
        let tx = self.conn.unchecked_transaction()?;
        if !super::scope_enforced_in(&tx)? {
            return Err(super::ProductError::Invariant(
                "product scope setup required",
            ));
        }
        let principal = resolve_session_in(&tx, session_digest, now)?;
        if self.artifact_root.is_none() {
            return Err(super::ProductError::Invariant(
                "artifact root is unavailable",
            ));
        }
        let records = artifact_record_in(&tx, &principal, order_id, None)?;
        let current = super::verified_order_state_in(&tx, order_id)?;
        let mut visible = Vec::new();
        for (info, _, _, _, _) in records {
            if info.lifecycle <= i64::from(current.1) + 1 {
                visible.push(info);
            }
        }
        tx.commit()?;
        Ok(visible)
    }

    /// Resolve by opaque artifact ID and order binding; callers cannot supply
    /// a filesystem path. No bytes escape until the confined file has been
    /// read completely and matched against its immutable SHA-256 record.
    pub fn read_product_artifact(
        &mut self,
        session_digest: &str,
        order_id: &str,
        artifact_id: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<ProductArtifactContent, super::ProductError> {
        let tx = self.conn.unchecked_transaction()?;
        if !super::scope_enforced_in(&tx)? {
            return Err(super::ProductError::Invariant(
                "product scope setup required",
            ));
        }
        let principal = resolve_session_in(&tx, session_digest, now)?;
        let mut records = artifact_record_in(&tx, &principal, order_id, Some(artifact_id))?;
        if records.len() != 1 {
            return Err(super::ProductError::Invariant("artifact unavailable"));
        }
        let (info, relative, _, _, _) = records.pop().expect("one checked artifact");
        let current = super::verified_order_state_in(&tx, order_id)?;
        if info.lifecycle > i64::from(current.1) + 1 {
            return Err(super::ProductError::Invariant(
                "artifact lifecycle is invalid",
            ));
        }
        let root = self
            .artifact_root_handle
            .as_deref()
            .ok_or(super::ProductError::Invariant(
                "artifact root is unavailable",
            ))?;
        let bytes = read_confined_artifact(root, &relative, &info.sha256)?;
        tx.commit()?;
        Ok(ProductArtifactContent { info, bytes })
    }
}

pub(super) fn read_confined_artifact(
    pinned_root: &File,
    relative: &str,
    expected_sha256: &str,
) -> Result<Vec<u8>, super::ProductError> {
    read_confined_artifact_with_hook(pinned_root, relative, expected_sha256, |_| {})
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArtifactResolvePoint {
    RootPinned,
    DirectoryPinned(usize),
    BeforeLeafOpen,
}

fn read_confined_artifact_with_hook(
    pinned_root: &File,
    relative: &str,
    expected_sha256: &str,
    mut hook: impl FnMut(ArtifactResolvePoint),
) -> Result<Vec<u8>, super::ProductError> {
    use std::io::Read;
    let relative_path = std::path::Path::new(relative);
    if relative_path.is_absolute()
        || relative_path.as_os_str().is_empty()
        || relative_path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(super::ProductError::Invariant("artifact unavailable"));
    }
    let mut components = relative_path
        .components()
        .map(|c| match c {
            std::path::Component::Normal(part) => Ok(part.to_os_string()),
            _ => Err(super::ProductError::Invariant("artifact unavailable")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let leaf = components
        .pop()
        .ok_or(super::ProductError::Invariant("artifact unavailable"))?;
    let mut parent = pinned_root
        .try_clone()
        .map_err(|_| super::ProductError::Invariant("artifact unavailable"))?;
    hook(ArtifactResolvePoint::RootPinned);
    for (index, component) in components.iter().enumerate() {
        parent = open_pinned_child(&parent, component, true)
            .map_err(|_| super::ProductError::Invariant("artifact unavailable"))?;
        hook(ArtifactResolvePoint::DirectoryPinned(index));
    }
    hook(ArtifactResolvePoint::BeforeLeafOpen);
    let path_handle = open_pinned_child(&parent, &leaf, false)
        .map_err(|_| super::ProductError::Invariant("artifact unavailable"))?;
    let path_metadata = path_handle
        .metadata()
        .map_err(|_| super::ProductError::Invariant("artifact unavailable"))?;
    if !path_metadata.is_file() {
        return Err(super::ProductError::Invariant("artifact unavailable"));
    }
    // O_PATH pins the exact inode without opening a FIFO/device for I/O.
    // Reading through its procfs descriptor cannot be redirected by renaming
    // any pathname component after the pin.
    let proc_fd = format!("/proc/self/fd/{}", raw_fd(&path_handle));
    let mut file =
        File::open(proc_fd).map_err(|_| super::ProductError::Invariant("artifact unavailable"))?;
    let read_metadata = file
        .metadata()
        .map_err(|_| super::ProductError::Invariant("artifact unavailable"))?;
    if !read_metadata.is_file() || !same_file_identity(&path_metadata, &read_metadata) {
        return Err(super::ProductError::Invariant("artifact unavailable"));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|_| super::ProductError::Invariant("artifact unavailable"))?;
    if format!("{:x}", Sha256::digest(&bytes)) != expected_sha256 {
        bytes.fill(0);
        return Err(super::ProductError::Invariant("artifact unavailable"));
    }
    Ok(bytes)
}

/// Walk from a pinned directory descriptor one component at a time. Linux
/// O_PATH|O_NOFOLLOW|O_DIRECTORY is used through safe std APIs; unsupported
/// platforms fail closed instead of substituting pathname checks.
#[cfg(target_os = "linux")]
fn open_pinned_child(
    parent: &File,
    name: &std::ffi::OsStr,
    directory: bool,
) -> std::io::Result<File> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    const O_DIRECTORY: i32 = 0o200000;
    const O_NOFOLLOW: i32 = 0o400000;
    const O_PATH: i32 = 0o10000000;
    let anchored = PathBuf::from(format!("/proc/self/fd/{}", parent.as_raw_fd())).join(name);
    let flags = O_PATH | O_NOFOLLOW | if directory { O_DIRECTORY } else { 0 };
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(anchored)
}

#[cfg(not(target_os = "linux"))]
fn open_pinned_child(
    _parent: &File,
    _name: &std::ffi::OsStr,
    _directory: bool,
) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "race-safe artifact resolution unavailable",
    ))
}

#[cfg(target_os = "linux")]
fn raw_fd(file: &File) -> std::os::fd::RawFd {
    use std::os::fd::AsRawFd;
    file.as_raw_fd()
}

#[cfg(not(target_os = "linux"))]
fn raw_fd(_file: &File) -> i32 {
    -1
}

#[cfg(target_os = "linux")]
pub(super) fn pin_artifact_root(path: &Path) -> Result<File, super::ProductError> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    const O_DIRECTORY: i32 = 0o200000;
    const O_NOFOLLOW: i32 = 0o400000;
    const O_PATH: i32 = 0o10000000;
    let mut current = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(O_PATH | O_DIRECTORY)
        .open("/")
        .map_err(|_| super::ProductError::Invariant("artifact root unavailable"))?;
    for component in path.components() {
        match component {
            std::path::Component::RootDir | std::path::Component::CurDir => {}
            std::path::Component::Normal(name) => {
                let anchored =
                    PathBuf::from(format!("/proc/self/fd/{}", current.as_raw_fd())).join(name);
                current = std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(O_PATH | O_DIRECTORY | O_NOFOLLOW)
                    .open(anchored)
                    .map_err(|_| super::ProductError::Invariant("artifact root unavailable"))?;
            }
            _ => return Err(super::ProductError::Invariant("artifact root unavailable")),
        }
    }
    Ok(current)
}

#[cfg(not(target_os = "linux"))]
pub(super) fn pin_artifact_root(_path: &Path) -> Result<File, super::ProductError> {
    Err(super::ProductError::Invariant(
        "race-safe artifact resolution unavailable",
    ))
}

pub(super) fn artifact_root_identity(file: &File) -> Result<(i64, i64), super::ProductError> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file
            .metadata()
            .map_err(|_| super::ProductError::Invariant("artifact root unavailable"))?;
        if !metadata.is_dir() {
            return Err(super::ProductError::Invariant("artifact root unavailable"));
        }
        let device = i64::try_from(metadata.dev())
            .map_err(|_| super::ProductError::Invariant("artifact root unavailable"))?;
        let inode = i64::try_from(metadata.ino())
            .map_err(|_| super::ProductError::Invariant("artifact root unavailable"))?;
        Ok((device, inode))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = file;
        Err(super::ProductError::Invariant(
            "race-safe artifact resolution unavailable",
        ))
    }
}

#[cfg(unix)]
fn same_file_identity(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    a.dev() == b.dev() && a.ino() == b.ino()
}

#[cfg(not(unix))]
fn same_file_identity(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.len() == b.len() && a.modified().ok() == b.modified().ok()
}

#[cfg(all(test, target_os = "linux"))]
mod artifact_resolution_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn scratch() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "foundry-artifact-race-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn digest(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn open_root(path: &Path) -> File {
        pin_artifact_root(path).unwrap()
    }

    #[test]
    fn leaf_symlink_replacement_after_parent_pin_is_rejected() {
        use std::os::unix::fs::symlink;
        let base = scratch();
        let root = base.join("root");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub/file"), b"authorized").unwrap();
        let outside = base.join("outside-secret");
        fs::write(&outside, b"outside secret").unwrap();
        let pinned = open_root(&root);
        let mut replaced = false;
        let result = read_confined_artifact_with_hook(
            &pinned,
            "sub/file",
            &digest(b"authorized"),
            |point| {
                if point == ArtifactResolvePoint::BeforeLeafOpen && !replaced {
                    fs::remove_file(root.join("sub/file")).unwrap();
                    symlink(&outside, root.join("sub/file")).unwrap();
                    replaced = true;
                }
            },
        );
        assert!(replaced, "deterministic final-component window was reached");
        assert!(result.is_err(), "replacement symlink is never followed");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn ancestor_replacement_after_directory_pin_reads_only_pinned_original() {
        use std::os::unix::fs::symlink;
        let base = scratch();
        let root = base.join("root");
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub/file"), b"authorized").unwrap();
        let outside = base.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("file"), b"outside secret").unwrap();
        let pinned = open_root(&root);
        let mut replaced = false;
        let bytes = read_confined_artifact_with_hook(
            &pinned,
            "sub/file",
            &digest(b"authorized"),
            |point| {
                if point == ArtifactResolvePoint::DirectoryPinned(0) && !replaced {
                    fs::rename(root.join("sub"), root.join("sub-original")).unwrap();
                    symlink(&outside, root.join("sub")).unwrap();
                    replaced = true;
                }
            },
        )
        .unwrap();
        assert!(
            replaced,
            "deterministic ancestor replacement window was reached"
        );
        assert_eq!(bytes, b"authorized");
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn configured_root_replacement_after_pin_keeps_original_root_identity() {
        let base = scratch();
        let root = base.join("root");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("file"), b"authorized").unwrap();
        let pinned = open_root(&root);
        let original_identity = artifact_root_identity(&pinned).unwrap();
        let replacement = base.join("replacement");
        fs::create_dir_all(&replacement).unwrap();
        fs::write(replacement.join("file"), b"outside secret").unwrap();
        let mut replaced = false;
        let bytes =
            read_confined_artifact_with_hook(&pinned, "file", &digest(b"authorized"), |point| {
                if point == ArtifactResolvePoint::RootPinned && !replaced {
                    fs::rename(&root, base.join("root-original")).unwrap();
                    fs::rename(&replacement, &root).unwrap();
                    replaced = true;
                }
            })
            .unwrap();
        assert!(
            replaced,
            "deterministic root replacement window was reached"
        );
        assert_eq!(bytes, b"authorized");
        assert_ne!(
            artifact_root_identity(&open_root(&root)).unwrap(),
            original_identity
        );
        fs::remove_dir_all(base).unwrap();
    }
}
