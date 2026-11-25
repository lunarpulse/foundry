//! Explicit review of a service-admitted, immutable local commit. This operation
//! journals the retained inspection output and leaves result approval to B1.
use super::*;
use foundry_exec::{HostResult, ProductSelectionBinding};
use sha2::{Digest, Sha256};

pub const COMMIT_REVIEW_KIND: &str = "local-commit-review-v1";

pub struct CommitReviewRequest<'a> {
    pub order: &'a str,
    pub lifecycle: i64,
    pub admission_hash: &'a str,
    pub commit: &'a str,
    pub selection_revision: i64,
    pub request_key: &'a str,
}

pub enum CommitReviewWork {
    Pending,
    Dispatch(ProductWorkClaim),
    Recorded(HostResult),
}

impl ProductStore {
    /// Authenticate the scope before opening any operator registry paths.
    pub fn authorize_pack_registry(
        &self,
        session: &str,
        source: &str,
        order: Option<(&str, i64)>,
    ) -> Result<(), ProductError> {
        let tx = self.conn.unchecked_transaction()?;
        let owner = resolve_session_in(&tx, session, self.clock.now())?;
        if owner.role != "owner" {
            return Err(w1_authorization_error());
        }
        w1_authorize_source(&tx, &owner.actor_id, source)?;
        if let Some((order, lifecycle)) = order {
            w1_authorize_order(&tx, &owner, order, lifecycle, source)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn prepare_commit_review(
        &mut self,
        owner_session: &str,
        service: &ProductServiceContext,
        request: &CommitReviewRequest<'_>,
        acquire_claim: bool,
    ) -> Result<(ProductSelectionBinding, CommitReviewWork), ProductError> {
        if self.open_mode != StoreOpenMode::Normal
            || !w1_text(request.request_key)
            || request.commit.len() != 40
            || !request
                .commit
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(ProductError::InvalidConfig);
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = self.clock.now();
        require_product_pin_version(&tx)?;
        service::require_service_context_in(&tx, service, ProductServiceAction::DriveReview, now)?;
        let owner = resolve_session_in(&tx, owner_session, now)?;
        if owner.role != "owner" {
            return Err(w1_authorization_error());
        }
        let view = require_order_capability_in(
            &tx,
            &owner,
            request.order,
            ProductCapability::ReadReview,
            true,
        )?;
        if view.lifecycle != request.lifecycle
            || view.source_instance_id != service.source_instance_id
        {
            return Err(w1_authorization_error());
        }
        let selected = w1_selection_state_in(
            &tx,
            &self.installation_id,
            &self.database_id,
            request.order,
            request.lifecycle,
            &view.source_instance_id,
        )?;
        if !selected.active
            || selected.selection_revision != request.selection_revision
            || (acquire_claim
                && !registered_pack_snapshot(self.product_registry.as_ref(), &selected.pack_digest))
        {
            return Err(w1_authorization_error());
        }
        // Match the selector's live grant and the exact acknowledged source
        // version before the backlog can become claimable, even transiently.
        let state: String = tx.query_row(
            "SELECT b.state FROM product_orders_v1 o
             JOIN product_work_backlog_v1 b ON b.order_id=o.order_id AND b.lifecycle=o.lifecycle
             JOIN product_intake_inbox_v1 i ON i.delivery_id=b.delivery_id
             JOIN product_intake_source_bindings_v1 ib ON ib.delivery_id=i.delivery_id
             JOIN product_w1_grant_sidecar_v1 sc ON sc.order_id=o.order_id AND sc.lifecycle=o.lifecycle
             JOIN product_scope_grants_v1 g ON g.subject_id=sc.selector_subject_id AND g.source_instance_id=ib.source_instance_id
             JOIN product_scope_subject_revisions_v1 sr ON sr.subject_id=g.subject_id AND sr.source_instance_id=g.source_instance_id
             WHERE o.order_id=?1 AND o.lifecycle=?2 AND o.admission_hash=?3 AND o.task_kind=?4
               AND o.payload_snapshot=?5 AND o.connector_id=?6 AND o.repository_id=?7
               AND i.state='acked' AND i.issue_id=o.issue_id AND i.repository_id=o.repository_id AND i.connector_id=o.connector_id
               AND ib.source_instance_id=?8 AND ib.source_generation=?9 AND ib.source_instance_revision=?10 AND ib.source_revision=?11
               AND sc.selection_revision=?12 AND sc.selector_subject_id=?13
               AND g.capability='read_order' AND g.enabled=1 AND g.grant_revision=sc.grant_revision AND sr.revision=sc.scope_revision",
            params![request.order,request.lifecycle,request.admission_hash,COMMIT_REVIEW_KIND,request.commit,service.connector_id,service.repository_id,service.source_instance_id,service.source_generation,service.source_instance_revision,service.connector_revision,selected.selection_revision,owner.actor_id],
            |r|r.get(0),
        ).optional()?.ok_or_else(w1_authorization_error)?;
        let binding = ProductSelectionBinding {
            order_id: selected.order,
            lifecycle: selected.lifecycle,
            source_instance_id: selected.source,
            pack_digest: selected.pack_digest,
            selection_revision: selected.selection_revision,
            referenced_approval_revision: selected.referenced_approval_revision,
        };
        let worker = authorized_worker(
            &tx,
            &binding,
            request.request_key,
            request.admission_hash,
            &service.service_id,
            service.service_revision,
            service.credential_revision,
        )?;
        let keyed: Option<(String,i64)> = tx.query_row(
            "SELECT order_id,lifecycle FROM product_w1_lifecycle_pins_v1 WHERE service_id=?1 AND request_key=?2",
            params![service.service_id,request.request_key], |r|Ok((r.get(0)?,r.get(1)?)),
        ).optional()?;
        if let Some((order, lifecycle)) = keyed {
            if order != request.order || lifecycle != request.lifecycle {
                return Err(w1_authorization_error());
            }
            // An exact committed admission can restart before its first stage.
            // Any reserved stage without a terminal outcome requires recovery.
            let bound: (Option<String>, String, i64, i64, i64, String, String) = tx.query_row(
                "SELECT h.result_json,w.delivery_id,w.claim_epoch,w.claim_fence,w.source_revision,w.payload_digest,c.lease_until
                 FROM product_w1_lifecycle_pins_v1 p
                 JOIN product_review_runs_v1 r ON r.order_id=p.order_id AND r.product_lifecycle=p.lifecycle AND r.pack_digest=p.pack_digest
                 JOIN product_review_work_bindings_v1 w ON w.order_id=r.order_id AND w.run_id=r.run_id AND w.claim_epoch=p.claim_epoch AND w.claim_fence=p.claim_fence AND w.payload_digest=p.payload_digest
                 JOIN product_review_authority_v1 a ON a.order_id=r.order_id AND a.journal_run=r.run_id AND a.service_id=p.service_id
                 JOIN product_work_claims_v1 c ON c.order_id=w.order_id AND c.lifecycle=w.lifecycle AND c.owner_id=w.worker_id
                   AND c.epoch=w.claim_epoch AND c.fence=w.claim_fence AND c.payload_digest=w.payload_digest AND c.source_revision=w.source_revision
                 JOIN product_work_backlog_v1 b ON b.order_id=c.order_id AND b.lifecycle=c.lifecycle AND b.delivery_id=w.delivery_id AND b.state=c.state
                 JOIN product_service_work_bindings_v1 sb ON sb.order_id=c.order_id AND sb.lifecycle=c.lifecycle AND sb.service_id=a.service_id
                   AND sb.service_revision=a.service_revision AND sb.credential_revision=a.credential_revision AND sb.source_instance_id=a.source_instance_id
                 JOIN product_review_host_accept_v1 accept ON accept.order_id=r.order_id AND accept.attempt=1 AND accept.run_id=1 AND accept.activation=1
                 JOIN product_review_active_v1 active ON active.order_id=r.order_id AND active.journal_run=r.run_id
                 LEFT JOIN product_review_host_results_v1 h ON h.order_id=r.order_id AND h.attempt=1 AND h.run_id=1 AND h.activation=1
                 WHERE p.service_id=?1 AND p.request_key=?2 AND p.admission_hash=?3 AND p.selection_revision=?4 AND p.referenced_approval_revision=?5
                   AND r.run_id=(p.order_id || ':1:1:1') AND r.attempt=1 AND w.task_kind=?13 AND w.payload_snapshot=?7
                   AND ((h.result_json=r.result_json AND ((r.status='held' AND c.state='claimed' AND c.lease_until>?12)
                        OR (r.status IN ('passed','failed') AND c.state='completed')))
                     OR (r.status='running' AND c.state='claimed' AND c.lease_until>?12 AND h.order_id IS NULL AND r.result_json IS NULL
                        AND NOT EXISTS(SELECT 1 FROM product_review_stages_v1 s WHERE s.order_id=r.order_id AND s.run_id=r.run_id)
                        AND NOT EXISTS(SELECT 1 FROM product_review_failures_v1 f WHERE f.order_id=r.order_id AND f.run_id=r.run_id)))
                   AND w.worker_id=?6 AND r.base_commit=?7 AND r.result_commit=?7 AND p.pack_digest=?8
                   AND a.service_revision=?9 AND a.credential_revision=?10 AND a.source_instance_id=?11",
                params![service.service_id,request.request_key,request.admission_hash,binding.selection_revision,binding.referenced_approval_revision,worker,request.commit,binding.pack_digest,service.service_revision,service.credential_revision,service.source_instance_id,now.to_rfc3339(),COMMIT_REVIEW_KIND],
                |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)),
            ).optional()?.ok_or_else(w1_authorization_error)?;
            let (recorded, delivery_id, epoch, fence, source_revision, payload_digest, lease) =
                bound;
            let work = if let Some(recorded) = recorded {
                CommitReviewWork::Recorded(
                    serde_json::from_str(&recorded).map_err(|_| ProductError::ContractMismatch)?,
                )
            } else if acquire_claim {
                CommitReviewWork::Dispatch(ProductWorkClaim {
                    order_id: request.order.into(),
                    lifecycle: request.lifecycle,
                    delivery_id,
                    worker_id: worker,
                    epoch,
                    fence,
                    source_revision,
                    payload_digest,
                    task_kind: COMMIT_REVIEW_KIND.into(),
                    payload_snapshot: request.commit.into(),
                    lease_until: lease.parse().map_err(|_| ProductError::ContractMismatch)?,
                    service_binding: Some(foundry_exec::ReviewServiceBinding {
                        service_id: service.service_id.clone(),
                        service_revision: service.service_revision,
                        credential_revision: service.credential_revision,
                        source_instance_id: service.source_instance_id.clone(),
                    }),
                })
            } else {
                CommitReviewWork::Pending
            };
            tx.commit()?;
            return Ok((binding, work));
        }
        // Before activation there is no pack pin. A newer selection can
        // supersede the pending claim. Re-fence an exact pre-pin retry too:
        // a concurrent holder either already pinned (handled above) or loses
        // the activation CAS. Never reclaim a run or orphan pin by this path.
        let pending_owner: Option<String> = if state == "claimed" {
            tx.query_row(
                "SELECT c.owner_id FROM product_work_claims_v1 c WHERE c.order_id=?1 AND c.lifecycle=?2 AND c.state='claimed'
                 AND NOT EXISTS(SELECT 1 FROM product_w1_lifecycle_pins_v1 p WHERE p.order_id=c.order_id AND p.lifecycle=c.lifecycle)
                 AND NOT EXISTS(SELECT 1 FROM product_review_runs_v1 r WHERE r.order_id=c.order_id AND r.product_lifecycle=c.lifecycle)",
                params![request.order,request.lifecycle],|row|row.get(0),
            ).optional()?
        } else {
            None
        };
        if let Some(previous) = &pending_owner {
            let revision = previous
                .strip_prefix("commit-review-")
                .and_then(|s| s.split_once('-'))
                .and_then(|(r, _)| r.parse::<i64>().ok());
            if previous != &worker
                && !revision.is_some_and(|r| r > 0 && r < binding.selection_revision)
            {
                return Err(w1_authorization_error());
            }
        } else if state != "waiting_unsupported" {
            return Err(w1_authorization_error());
        }
        if !acquire_claim {
            tx.commit()?;
            return Ok((binding, CommitReviewWork::Pending));
        }
        if let Some(previous) = pending_owner {
            tx.execute("UPDATE product_work_claims_v1 SET lease_until=?1 WHERE order_id=?2 AND lifecycle=?3 AND owner_id=?4",
                params![now.to_rfc3339(),request.order,request.lifecycle,previous])?;
        }
        tx.execute("UPDATE product_work_backlog_v1 SET state='ready' WHERE order_id=?1 AND lifecycle=?2 AND state='waiting_unsupported' AND attempts=0",params![request.order,request.lifecycle])?;
        let ProductWorkClaimResult::Claimed(claim) = Self::claim_scoped_work_in(
            &tx,
            service,
            &worker,
            Some(request.order),
            now,
            chrono::Duration::minutes(30),
        )?
        else {
            return Err(w1_authorization_error());
        };
        tx.commit()?;
        Ok((binding, CommitReviewWork::Dispatch(claim)))
    }
}

/// Recompute the exact claim owner from current account, grant, selection and
/// service coordinates. A claim issued before an ABA cannot authorize work.
pub(super) fn authorized_worker(
    tx: &rusqlite::Transaction<'_>,
    binding: &ProductSelectionBinding,
    key: &str,
    admission_hash: &str,
    service_id: &str,
    service_revision: i64,
    credential_revision: i64,
) -> Result<String, ProductError> {
    let owner = tx.query_row(
        "SELECT a.actor_id,a.role,a.revision FROM product_w1_grant_sidecar_v1 sc
         JOIN product_accounts_v1 a ON a.actor_id=sc.selector_subject_id
         WHERE sc.order_id=?1 AND sc.lifecycle=?2 AND sc.selection_revision=?3 AND a.role='owner' AND a.enabled=1",
        params![binding.order_id,binding.lifecycle,binding.selection_revision],
        |r| Ok(ProductPrincipal{actor_id:r.get(0)?,role:r.get(1)?,authority_revision:r.get(2)?}),
    )?;
    let view = require_order_capability_in(
        tx,
        &owner,
        &binding.order_id,
        ProductCapability::ReadReview,
        true,
    )?;
    let identity = serde_json::to_vec(&(
        key,
        admission_hash,
        &binding.order_id,
        binding.lifecycle,
        &binding.source_instance_id,
        &binding.pack_digest,
        binding.selection_revision,
        binding.referenced_approval_revision,
        &owner.actor_id,
        owner.authority_revision,
        view.scope_revision,
        view.grant_revision,
        service_id,
        service_revision,
        credential_revision,
    ))
    .map_err(|_| ProductError::InvalidConfig)?;
    Ok(format!(
        "commit-review-{}-{:x}",
        binding.selection_revision,
        Sha256::digest(identity)
    ))
}

pub struct SelectedReviewHold {
    pub pack_digest: String,
    pub source_instance_id: String,
    pub commit_review: bool,
    pub active: bool,
    pub expectation: ProductHoldExpectation,
}

impl ProductStore {
    /// Resolve the originating held run under the reader's current scope. The
    /// acceptance transaction still owns the expectation and authority checks.
    pub fn selected_hold_pack(
        &self,
        session: &str,
        order: &str,
        hold: &str,
    ) -> Result<SelectedReviewHold, ProductError> {
        let tx = self.conn.unchecked_transaction()?;
        require_product_pin_version(&tx)?;
        let principal = resolve_session_in(&tx, session, self.clock.now())?;
        if !matches!(principal.role.as_str(), "owner" | "approver") {
            return Err(w1_authorization_error());
        }
        let view = require_order_capability_in(
            &tx,
            &principal,
            order,
            ProductCapability::ReadReview,
            true,
        )?;
        let hold = tx.query_row(
            "SELECT r.pack_digest,w.task_kind,h.state,h.lifecycle,h.attempt,h.originating_run,h.activation,h.host_epoch,h.hold_revision,h.source_revision,h.expected_predecessor,h.snapshot_digest FROM product_review_holds_v1 h
             JOIN product_review_runs_v1 r ON r.order_id=h.order_id AND r.product_lifecycle=h.lifecycle
               AND r.attempt=h.attempt AND r.run_id=h.order_id||':'||h.attempt||':'||h.originating_run||':'||h.activation
             JOIN product_review_work_bindings_v1 w ON w.order_id=r.order_id AND w.run_id=r.run_id
             JOIN product_w1_lifecycle_pins_v1 p ON p.order_id=r.order_id AND p.lifecycle=r.product_lifecycle
               AND p.pack_digest=r.pack_digest AND p.admission_hash=r.admission_hash AND p.admission_seq=r.admission_seq
             WHERE h.order_id=?1 AND h.hold_instance_id=?2 AND h.lifecycle=?3 AND p.source_instance_id=?4",
            params![order,hold,view.lifecycle,view.source_instance_id],|row|Ok(SelectedReviewHold {
                pack_digest:row.get(0)?, source_instance_id:view.source_instance_id.clone(),
                commit_review:row.get::<_,String>(1)?==COMMIT_REVIEW_KIND,
                active:row.get::<_,String>(2)?=="active",
                expectation:ProductHoldExpectation{lifecycle:row.get(3)?,attempt:row.get(4)?,originating_run:row.get(5)?,
                    activation:row.get(6)?,host_epoch:row.get(7)?,hold_revision:row.get(8)?,source_revision:row.get(9)?,
                    predecessor_seq:row.get(10)?,snapshot_digest:row.get(11)?},
            }),
        ).optional()?.ok_or_else(w1_authorization_error)?;
        tx.commit()?;
        Ok(hold)
    }
}

pub(super) fn append_execution_transition(
    tx: &rusqlite::Transaction<'_>,
    order_id: &str,
    lifecycle: i64,
    event: super::OrderEvent,
    operation: &str,
    actor: &str,
    intent: &str,
    now: DateTime<Utc>,
) -> Result<(), ProductError> {
    let mut statement = tx.prepare(
        "SELECT seq,order_id,from_state,to_state,event,actor,intent,at,prev_hash,hash,event_json FROM ledger ORDER BY seq",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok(crate::sqlite::LedgerRow {
                seq: row.get(0)?,
                order_id: row.get(1)?,
                from_state: row.get(2)?,
                to_state: row.get(3)?,
                event: row.get(4)?,
                actor: row.get(5)?,
                intent: row.get(6)?,
                at: row.get(7)?,
                prev_hash: row.get(8)?,
                hash: row.get(9)?,
                event_json: row.get(10)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut current = crate::replay::replay(&rows, crate::sqlite::SqliteLedger::verify_rows)
        .map_err(|_| ProductError::Invariant("product execution S1 verification failed"))?
        .into_iter()
        .rev()
        .find(|row| row.order_id == order_id)
        .ok_or(ProductError::Invariant(
            "product execution order unavailable",
        ))?;
    if i64::from(current.lifecycle) + 1 != lifecycle {
        return Err(ProductError::Invariant(
            "product execution lifecycle changed",
        ));
    }
    let predecessor = current.last_seq;
    let attempt = current.order.attempt().max(1);
    let from = current.order.state().to_string();
    let to = current
        .order
        .transition(event.clone(), attempt)
        .map_err(|_| ProductError::Invariant("product execution transition invalid"))?
        .to_string();
    let sequence: i64 = tx.query_row("SELECT COALESCE(MAX(seq),0)+1 FROM ledger", [], |row| {
        row.get(0)
    })?;
    let previous_hash: String = tx.query_row(
        "SELECT hash FROM ledger ORDER BY seq DESC LIMIT 1",
        [],
        |row| row.get(0),
    )?;
    let event_text = format!("{event:?}");
    let event_json = crate::event_json::event_to_json(&event);
    let at = now.to_rfc3339();
    let v1 = format!(
        "{sequence}|{order_id}|{from}|{to}|{event_text}|{actor}|{intent}|{at}|{previous_hash}"
    );
    let event_hash = hex::encode(Sha256::digest(
        crate::event_json::v2_hash_payload(&v1, &event_json).as_bytes(),
    ));
    tx.execute(
        "INSERT INTO product_transition_intents_v1(order_id,lifecycle,expected_predecessor,operation) VALUES(?1,?2,?3,?4)",
        params![order_id,lifecycle,predecessor,operation],
    )?;
    tx.execute(
        "INSERT INTO ledger(order_id,from_state,to_state,event,actor,intent,at,prev_hash,event_json,hash) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![order_id,from,to,event_text,actor,intent,at,previous_hash,event_json,event_hash],
    )?;
    let receipted: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM product_transition_receipts_v1 WHERE seq=?1 AND event_hash=?2 AND order_id=?3 AND lifecycle=?4 AND operation=?5 AND expected_predecessor=?6) AND NOT EXISTS(SELECT 1 FROM product_transition_intents_v1 WHERE order_id=?3)",
        params![sequence,event_hash,order_id,lifecycle,operation,predecessor],
        |row| row.get(0),
    )?;
    if !receipted {
        return Err(ProductError::Invariant(
            "product execution transition receipt missing",
        ));
    }
    Ok(())
}

/// Only this task uses an inspection report as its work output. Bind every
/// journal transition to its immutable input, pin and actual fenced run.
pub(super) fn record_inspection_transition(
    tx: &rusqlite::Transaction<'_>,
    order: &str,
    run: &str,
    outcome: Option<&foundry_exec::ReviewDispatchCompletion>,
    now: DateTime<Utc>,
) -> Result<(), ProductError> {
    let work:Option<(i64,String,String,String,String,i64)> = tx.query_row(
        "SELECT w.lifecycle,w.payload_snapshot,p.pack_digest,p.service_id,p.request_key,w.claim_fence
         FROM product_review_work_bindings_v1 w JOIN product_w1_lifecycle_pins_v1 p
           ON p.order_id=w.order_id AND p.lifecycle=w.lifecycle AND p.claim_epoch=w.claim_epoch AND p.claim_fence=w.claim_fence
         WHERE w.order_id=?1 AND w.run_id=?2 AND w.task_kind=?3",
        params![order,run,COMMIT_REVIEW_KIND],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))
    ).optional()?;
    let (lifecycle, commit, pack, service, key, fence) =
        work.ok_or(ProductError::ContractMismatch)?;
    let actor = format!("Service/{service}");
    let receipt = serde_json::json!({"kind":COMMIT_REVIEW_KIND,"commit":commit,"run":run,"pack_digest":pack,"request_key":key,"claim_fence":fence});
    let Some(outcome) = outcome else {
        return append_execution_transition(
            tx,
            order,
            lifecycle,
            OrderEvent::DieStarted,
            "commit-review-inspection-start",
            &actor,
            &receipt.to_string(),
            now,
        );
    };
    if outcome.result.base_commit != commit || outcome.result.result_commit != commit {
        return Err(ProductError::ContractMismatch);
    }
    let evidence:Option<(i64,String,String)>=tx.query_row(
        "SELECT s.sequence,s.stage_id,a.artifact_sha256 FROM product_review_stages_v1 s
         JOIN product_artifacts_v1 a ON a.order_id=s.order_id AND a.run_id=s.run_id AND a.stage_sequence=s.sequence AND a.stage_id=s.stage_id
         WHERE s.order_id=?1 AND s.run_id=?2 AND s.status='complete' AND json_extract(s.output_json,'$.role') IN ('lens','verify')
           AND json_extract(s.output_json,'$.inspected_commit')=?3 AND json_extract(s.output_json,'$.sha256')=a.artifact_sha256
         ORDER BY s.sequence DESC LIMIT 1",
        params![order,run,commit],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))
    ).optional()?;
    let Some((sequence, stage_id, sha256)) = evidence else {
        return Err(ProductError::Invariant(
            "commit review has no retained inspection output",
        ));
    };
    let mut evidence = receipt;
    evidence["inspection"] =
        serde_json::json!({"sequence":sequence,"stage_id":stage_id,"artifact_sha256":sha256});
    let evidence = evidence.to_string();
    append_execution_transition(
        tx,
        order,
        lifecycle,
        OrderEvent::DieSucceeded { attempt: 1 },
        "commit-review-inspection-output",
        &actor,
        &evidence,
        now,
    )?;
    use foundry_exec::ReviewTerminalStatus;
    let decision = match outcome.status {
        ReviewTerminalStatus::Passed => OrderEvent::InspectionPassed,
        ReviewTerminalStatus::Held => OrderEvent::ReviewHeld {
            reason: outcome
                .result
                .held
                .as_ref()
                .ok_or(ProductError::ContractMismatch)?
                .reason
                .clone(),
        },
        ReviewTerminalStatus::Failed => OrderEvent::InspectionFailed {
            reason: "commit-review-graph".into(),
        },
    };
    append_execution_transition(
        tx,
        order,
        lifecycle,
        decision,
        "commit-review-graph-outcome",
        &actor,
        &evidence,
        now,
    )
}

pub(super) fn record_failed_inspection(
    tx: &rusqlite::Transaction<'_>,
    order: &str,
    run: &str,
    now: DateTime<Utc>,
) -> Result<(), ProductError> {
    // An admission may fail before the first stage. S1 has no started attempt
    // in that case. The host still retains the failure in the same transaction.
    let started: Option<(i64,String)> = tx.query_row(
        "SELECT w.lifecycle,a.service_id FROM product_review_work_bindings_v1 w
         JOIN product_review_authority_v1 a ON a.order_id=w.order_id AND a.journal_run=w.run_id
         WHERE w.order_id=?1 AND w.run_id=?2 AND EXISTS(
           SELECT 1 FROM product_transition_receipts_v1 t JOIN ledger l ON l.seq=t.seq AND l.hash=t.event_hash
           WHERE t.order_id=w.order_id AND t.lifecycle=w.lifecycle AND t.operation='commit-review-inspection-start'
             AND json_extract(l.intent,'$.run')=w.run_id)",
        params![order,run], |row|Ok((row.get(0)?,row.get(1)?)),
    ).optional()?;
    if let Some((lifecycle, service)) = started {
        append_execution_transition(
            tx,
            order,
            lifecycle,
            OrderEvent::DieFailed {
                attempt: 1,
                reason: "commit inspection failed".into(),
            },
            "commit-review-inspection-failed",
            &format!("Service/{service}"),
            &serde_json::json!({"kind":COMMIT_REVIEW_KIND,"run":run}).to_string(),
            now,
        )?;
    }
    Ok(())
}
